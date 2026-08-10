use skia_safe::{
    AlphaType, ColorSpace as SkColorSpace, ColorType, IPoint, ISize, ImageInfo,
    Pixmap, Surface as SkSurface,
};

use crate::{
    backend::{EngineKind, engine_kind_from, resolve_engine},
    color::LinearColorSpace,
    context::page::ExportOptions,
    error::Error,
    image::Image,
    pixels::{
        ExportedPixels, PixelColorSpace, PixelDepth, PixelExportOptions,
        SurfaceOptions,
    },
    recorder::Canvas,
};

/// A drawable raster target.
///
/// Surfaces always composite at RGBA F16 precision in their working color
/// space; the format you read back with is chosen separately via
/// [`PixelExportOptions`]. Build one with
/// [`Backend::create_surface`](crate::backend::Backend::create_surface).
pub struct Surface {
    inner: SkSurface,
    color_space: LinearColorSpace,
    /// Cached Skia color-space handle for the working space. Built once
    /// at construction so `with_canvas` can hand it to `Canvas`
    /// without re-resolving on every borrow.
    working_color_space: SkColorSpace,
    /// Which rasterizer the surface ended up using. `Auto` resolves at
    /// construction time, so this is always concrete (`Cpu` or `Gpu`).
    engine: EngineKind,
    width: u32,
    height: u32,
}

impl Surface {
    pub(crate) fn new(
        width: u32,
        height: u32,
        options: SurfaceOptions,
    ) -> Result<Self, Error> {
        if width == 0 || height == 0 {
            return Err(Error::InvalidDimensions {
                width: width as f32,
                height: height as f32,
            });
        }
        let cs = options.color_space.to_skia_color_space()?;
        // Surfaces always composite at RGBAF16 precision in their working
        // color space; readback options control the exported format.
        let info = ImageInfo::new(
            (width as i32, height as i32),
            ColorType::RGBAF16,
            AlphaType::Premul,
            cs.clone(),
        );
        let internal = resolve_engine(options.engine)?;
        let export_options = ExportOptions {
            msaa: options.msaa,
            color_type: ColorType::RGBAF16,
            color_space: cs.clone(),
            ..ExportOptions::default()
        };
        let surface = internal.make_surface(&info, &export_options).map_err(
            |reason| Error::SurfaceCreate {
                reason: format!(
                    "could not allocate {width}x{height} surface: {reason}"
                ),
            },
        )?;
        Ok(Self {
            inner: surface,
            color_space: options.color_space,
            working_color_space: cs,
            engine: engine_kind_from(internal),
            width,
            height,
        })
    }

    /// Returns the width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Returns the height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Returns the working color space this surface composites in.
    pub fn color_space(&self) -> LinearColorSpace {
        self.color_space
    }

    /// Returns the rasterizer that ended up backing this surface.
    ///
    /// With [`RenderEngine::Auto`](crate::backend::RenderEngine::Auto) this
    /// is how a caller learns whether the GPU path was selected at
    /// construction time.
    pub fn engine(&self) -> EngineKind {
        self.engine
    }

    /// Flushes pending Skia work. No-op for CPU surfaces; for GPU
    /// surfaces, submits queued draw commands so that the next
    /// `read_pixels*` reflects them.
    pub fn flush(&mut self) {
        #[cfg(any(feature = "vulkan", feature = "metal"))]
        if self.engine == EngineKind::Gpu {
            crate::gpu::RenderingEngine::GPU.with_direct_context(|ctx| {
                if let Some(ctx) = ctx {
                    ctx.flush_and_submit();
                }
            });
        }
    }

    /// Captures the current contents as an immutable [`Image`].
    ///
    /// Copy-on-write: the snapshot shares pixels with the surface until the
    /// surface is drawn to again. The [`Image`] itself is immutable.
    pub fn snapshot(&mut self) -> Image {
        Image {
            inner: self.inner.image_snapshot(),
        }
    }

    /// Creates a second surface sharing this one's working color space and
    /// rasterizer, for rendering something separately before compositing it
    /// back.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidDimensions`] if either dimension is zero, or
    /// [`Error::SurfaceCreate`] if Skia declines the allocation.
    pub fn create_offscreen(
        &mut self,
        width: u32,
        height: u32,
    ) -> Result<Surface, Error> {
        if width == 0 || height == 0 {
            return Err(Error::InvalidDimensions {
                width: width as f32,
                height: height as f32,
            });
        }
        let off = self
            .inner
            .new_surface_with_dimensions(ISize::new(
                width as i32,
                height as i32,
            ))
            .ok_or_else(|| Error::SurfaceCreate {
                reason: format!(
                    "could not allocate {width}x{height} offscreen surface"
                ),
            })?;
        Ok(Surface {
            inner: off,
            color_space: self.color_space,
            working_color_space: self.working_color_space.clone(),
            engine: self.engine,
            width,
            height,
        })
    }

    /// Runs `f` with a [`Canvas`] that draws into this surface, returning
    /// whatever `f` returns.
    ///
    /// The canvas borrows the surface, so it cannot outlive the call.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use meo_skia_canvas::prelude::*;
    /// # let backend = Backend::new();
    /// # let mut surface =
    /// #     backend.create_surface(64, 64, SurfaceOptions::default())?;
    /// surface.with_canvas(|canvas| {
    ///     canvas.clear(RgbaLinear::opaque(1.0, 1.0, 1.0));
    /// });
    /// # Ok::<(), meo_skia_canvas::error::Error>(())
    /// ```
    pub fn with_canvas<R>(
        &mut self,
        f: impl FnOnce(&mut Canvas<'_>) -> R,
    ) -> R {
        // Forward the surface's working color space so canvas methods
        // can tag every `RgbaLinear` value with the right primaries.
        let working_cs = self.working_color_space.clone();
        let canvas = self.inner.canvas();
        let mut nc = Canvas::new(canvas, working_cs);
        f(&mut nc)
    }

    /// Reads the surface back with the default layout: tight, sRGB gamma,
    /// `Uint8`, unpremultiplied. Matches
    /// the wire format expected by `HTMLCanvasElement.putImageData`.
    ///
    /// # Errors
    ///
    /// As [`Surface::read_pixels_as`].
    pub fn read_pixels(&mut self) -> Result<ExportedPixels, Error> {
        self.read_pixels_as(PixelExportOptions::default())
    }

    /// Reads the surface in its working color space at native precision
    /// (F16, premultiplied). Used when callers need exact internal values.
    ///
    /// # Errors
    ///
    /// As [`Surface::read_pixels_as`].
    pub fn read_pixels_raw(&mut self) -> Result<ExportedPixels, Error> {
        self.read_pixels_as(PixelExportOptions {
            color_space: self.linear_pixel_color_space(),
            depth: PixelDepth::F16,
            premultiplied: true,
        })
    }

    /// Reads F32 linear pixels in the surface's working color space.
    ///
    /// # Errors
    ///
    /// As [`Surface::read_pixels_as`].
    pub fn read_pixels_linear(&mut self) -> Result<ExportedPixels, Error> {
        self.read_pixels_as(PixelExportOptions {
            color_space: self.linear_pixel_color_space(),
            depth: PixelDepth::F32,
            premultiplied: true,
        })
    }

    /// Reads the surface back in a caller-chosen color space, depth and
    /// alpha mode, converting as needed.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnsupportedPixelColorSpace`] for a color space this
    /// build cannot construct, or [`Error::PixelReadback`] if the surface
    /// refuses the conversion.
    pub fn read_pixels_as(
        &mut self,
        options: PixelExportOptions,
    ) -> Result<ExportedPixels, Error> {
        let dst_cs = options.color_space.to_skia_color_space()?;
        let dst_ct = options.depth.to_skia_color_type();
        let dst_at = if options.premultiplied {
            AlphaType::Premul
        } else {
            AlphaType::Unpremul
        };
        let info = ImageInfo::new(
            (self.width as i32, self.height as i32),
            dst_ct,
            dst_at,
            dst_cs,
        );
        let bpp = options.depth.bytes_per_pixel();
        let stride = (self.width as usize) * bpp;
        let mut buffer: Vec<u8> = vec![0; stride * self.height as usize];
        if !self.inner.read_pixels(
            &info,
            &mut buffer,
            stride,
            IPoint::new(0, 0),
        ) {
            return Err(Error::PixelReadback {
                reason: format!(
                    "read failed for {:?} {:?} premul={}",
                    options.color_space, options.depth, options.premultiplied
                ),
            });
        }
        Ok(ExportedPixels::new(
            self.width,
            self.height,
            stride,
            options.color_space,
            options.depth,
            options.premultiplied,
            buffer,
        ))
    }

    /// Overwrites the whole surface from a caller-supplied pixel buffer.
    ///
    /// `bytes` must describe exactly `width * height` pixels in the layout
    /// `options` names, with no row padding.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnsupportedPixelColorSpace`] for a color space this
    /// build cannot construct, [`Error::InvalidByteLength`] when `bytes` is
    /// not the exact size the layout implies, and [`Error::PixelWrite`] if
    /// the pixmap cannot be constructed.
    ///
    /// Skia's own write result is not checked, so a write it declines still
    /// returns `Ok(())`.
    pub fn write_pixels(
        &mut self,
        bytes: &[u8],
        options: PixelExportOptions,
    ) -> Result<(), Error> {
        let dst_cs = options.color_space.to_skia_color_space()?;
        let dst_ct = options.depth.to_skia_color_type();
        let dst_at = if options.premultiplied {
            AlphaType::Premul
        } else {
            AlphaType::Unpremul
        };
        let info = ImageInfo::new(
            (self.width as i32, self.height as i32),
            dst_ct,
            dst_at,
            dst_cs,
        );
        let bpp = options.depth.bytes_per_pixel();
        let stride = (self.width as usize) * bpp;
        let expected = stride * self.height as usize;
        if bytes.len() != expected {
            return Err(Error::InvalidByteLength {
                expected,
                actual: bytes.len(),
            });
        }
        // Pixmap requires `&mut [u8]`; Skia does not modify the source on
        // write, so we copy the caller's slice once. Acceptable cost given
        // write_pixels is not on the per-frame hot path.
        let mut copy = bytes.to_vec();
        let pixmap =
            Pixmap::new(&info, &mut copy, stride).ok_or_else(|| {
                Error::PixelWrite {
                    reason: "pixmap construct failed".to_string(),
                }
            })?;
        self.inner
            .write_pixels_from_pixmap(&pixmap, IPoint::new(0, 0));
        Ok(())
    }

    /// Writes F32 linear premultiplied pixels in the surface's working
    /// color space -- the inverse of [`Surface::read_pixels_linear`].
    ///
    /// # Errors
    ///
    /// As [`Surface::write_pixels`].
    pub fn write_pixels_linear(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.write_pixels(
            bytes,
            PixelExportOptions {
                color_space: self.linear_pixel_color_space(),
                depth: PixelDepth::F32,
                premultiplied: true,
            },
        )
    }

    fn linear_pixel_color_space(&self) -> PixelColorSpace {
        match self.color_space {
            LinearColorSpace::Srgb => PixelColorSpace::SrgbLinear,
            LinearColorSpace::DisplayP3 => PixelColorSpace::DisplayP3Linear,
            LinearColorSpace::Rec2020 => PixelColorSpace::Rec2020Linear,
        }
    }
}
