use skia_safe::{
    AlphaType, ColorSpace as SkColorSpace, ColorType, IPoint, ISize, ImageInfo, Pixmap,
    Surface as SkSurface, surfaces,
};

use crate::native::color::LinearColorSpace;
use crate::native::error::NativeError;
use crate::native::image::NativeImage;
use crate::native::pixels::{
    ExportedPixels, PixelColorSpace, PixelDepth, PixelExportOptions, SurfaceOptions,
};
use crate::native::recorder::NativeCanvas;

pub struct NativeSurface {
    inner: SkSurface,
    color_space: LinearColorSpace,
    /// Cached Skia color-space handle for the working space. Built once
    /// at construction so `with_canvas` can hand it to `NativeCanvas`
    /// without re-resolving on every borrow.
    working_color_space: SkColorSpace,
    width: u32,
    height: u32,
}

impl NativeSurface {
    pub(crate) fn new(
        width: u32,
        height: u32,
        options: SurfaceOptions,
    ) -> Result<Self, NativeError> {
        if width == 0 || height == 0 {
            return Err(NativeError::InvalidDimensions {
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
        let surface =
            surfaces::raster(&info, None, None).ok_or_else(|| NativeError::SurfaceCreate {
                reason: format!("could not allocate {width}x{height} surface"),
            })?;
        Ok(Self {
            inner: surface,
            color_space: options.color_space,
            working_color_space: cs,
            width,
            height,
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn color_space(&self) -> LinearColorSpace {
        self.color_space
    }

    /// CPU surfaces have no GPU command queue to flush. Provided for API
    /// consistency once GPU surfaces land.
    pub fn flush(&mut self) {}

    pub fn snapshot(&mut self) -> NativeImage {
        NativeImage {
            inner: self.inner.image_snapshot(),
        }
    }

    pub fn create_offscreen(
        &mut self,
        width: u32,
        height: u32,
    ) -> Result<NativeSurface, NativeError> {
        if width == 0 || height == 0 {
            return Err(NativeError::InvalidDimensions {
                width: width as f32,
                height: height as f32,
            });
        }
        let off = self
            .inner
            .new_surface_with_dimensions(ISize::new(width as i32, height as i32))
            .ok_or_else(|| NativeError::SurfaceCreate {
                reason: format!("could not allocate {width}x{height} offscreen surface"),
            })?;
        Ok(NativeSurface {
            inner: off,
            color_space: self.color_space,
            working_color_space: self.working_color_space.clone(),
            width,
            height,
        })
    }

    pub fn with_canvas<R>(&mut self, f: impl FnOnce(&mut NativeCanvas<'_>) -> R) -> R {
        // Forward the surface's working color space so canvas methods
        // can tag every `RgbaLinear` value with the right primaries.
        let working_cs = self.working_color_space.clone();
        let canvas = self.inner.canvas();
        let mut nc = NativeCanvas::new(canvas, working_cs);
        f(&mut nc)
    }

    /// Default readback: tight, sRGB gamma, Uint8, unpremultiplied. Matches
    /// the wire format expected by `HTMLCanvasElement.putImageData`.
    pub fn read_pixels(&mut self) -> Result<ExportedPixels, NativeError> {
        self.read_pixels_as(PixelExportOptions::default())
    }

    /// Read the surface in its working color space at native precision
    /// (F16, premultiplied). Used when callers need exact internal values.
    pub fn read_pixels_raw(&mut self) -> Result<ExportedPixels, NativeError> {
        self.read_pixels_as(PixelExportOptions {
            color_space: self.linear_pixel_color_space(),
            depth: PixelDepth::F16,
            premultiplied: true,
        })
    }

    /// Read F32 linear pixels in the surface's working color space.
    pub fn read_pixels_linear(&mut self) -> Result<ExportedPixels, NativeError> {
        self.read_pixels_as(PixelExportOptions {
            color_space: self.linear_pixel_color_space(),
            depth: PixelDepth::F32,
            premultiplied: true,
        })
    }

    pub fn read_pixels_as(
        &mut self,
        options: PixelExportOptions,
    ) -> Result<ExportedPixels, NativeError> {
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
        if !self
            .inner
            .read_pixels(&info, &mut buffer, stride, IPoint::new(0, 0))
        {
            return Err(NativeError::PixelReadback {
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

    pub fn write_pixels(
        &mut self,
        bytes: &[u8],
        options: PixelExportOptions,
    ) -> Result<(), NativeError> {
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
            return Err(NativeError::InvalidByteLength {
                expected,
                actual: bytes.len(),
            });
        }
        // Pixmap requires `&mut [u8]`; Skia does not modify the source on
        // write, so we copy the caller's slice once. Acceptable cost given
        // write_pixels is not on the per-frame hot path.
        let mut copy = bytes.to_vec();
        let pixmap =
            Pixmap::new(&info, &mut copy, stride).ok_or_else(|| NativeError::PixelWrite {
                reason: "pixmap construct failed".to_string(),
            })?;
        self.inner
            .write_pixels_from_pixmap(&pixmap, IPoint::new(0, 0));
        Ok(())
    }

    pub fn write_pixels_linear(&mut self, bytes: &[u8]) -> Result<(), NativeError> {
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
