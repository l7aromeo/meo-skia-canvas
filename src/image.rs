use skia_safe::{
    AlphaType, Color4f, ColorSpace, ColorType, Data, FontMgr, Image as SkImage,
    ImageInfo, Size, images, surfaces,
};

use crate::{
    error::Error,
    pixels::{PixelColorSpace, PixelFormat},
};

/// An immutable decoded raster image.
///
/// Cloning is cheap: Skia images are reference-counted and the pixels are
/// shared, not copied.
#[derive(Debug, Clone)]
pub struct Image {
    pub(crate) inner: SkImage,
}

impl Image {
    /// Decodes an encoded image (PNG, JPEG, WebP, etc.) into a `Image`.
    /// For raw decoded video frames (rsmpeg) or generated pixel buffers
    /// (Citra), prefer `from_pixels` -- it skips the encode/decode round
    /// trip.
    pub fn from_encoded(bytes: &[u8]) -> Result<Self, Error> {
        let data = Data::new_copy(bytes);
        let image =
            SkImage::from_encoded(data).ok_or_else(|| Error::DecodeImage {
                reason: "skia could not decode the encoded image bytes"
                    .to_string(),
            })?;
        Ok(Self { inner: image })
    }

    /// Builds a `Image` directly from a raw pixel buffer. The intended
    /// bridge for rsmpeg-decoded video frames and Citra-generated images:
    /// no PNG/JPEG/WebP encode round trip is required.
    ///
    /// The caller specifies pixel layout and color metadata explicitly.
    /// `pixel_format` covers the pixel layout and alpha mode (premul vs
    /// unpremul); `color_space` is a `PixelColorSpace` (the same enum used
    /// for surface readback), so callers must explicitly state whether
    /// pixels are gamma-coded sRGB / Display P3 / Rec.2020 or their linear
    /// counterparts. There is no implicit fallback to sRGB.
    ///
    /// Validation:
    ///
    /// - `width` and `height` must be non-zero.
    /// - `stride` must be at least `width * pixel_format.bytes_per_pixel()`.
    /// - `bytes.len()` must equal `stride * height` exactly.
    ///
    /// Pixel data is copied; the returned image owns its storage. F16 / F32
    /// formats preserve HDR values without clamping.
    pub fn from_pixels(
        bytes: &[u8],
        width: u32,
        height: u32,
        stride: usize,
        pixel_format: PixelFormat,
        color_space: PixelColorSpace,
    ) -> Result<Self, Error> {
        if width == 0 || height == 0 {
            return Err(Error::InvalidDimensions {
                width: width as f32,
                height: height as f32,
            });
        }
        let bpp = pixel_format.bytes_per_pixel();
        let min_stride = (width as usize) * bpp;
        if stride < min_stride {
            return Err(Error::InvalidStride {
                expected: min_stride,
                actual: stride,
            });
        }
        let expected_len = stride * (height as usize);
        if bytes.len() != expected_len {
            return Err(Error::InvalidByteLength {
                expected: expected_len,
                actual: bytes.len(),
            });
        }

        let color_type = pixel_format.to_skia_color_type()?;
        let alpha_type = pixel_format.to_skia_alpha_type();
        let sk_color_space = color_space.to_skia_color_space()?;
        let info = ImageInfo::new(
            (width as i32, height as i32),
            color_type,
            alpha_type,
            sk_color_space,
        );

        let data = Data::new_copy(bytes);
        let image = images::raster_from_data(&info, data, stride).ok_or_else(|| {
            Error::DecodeImage {
                reason: format!(
                    "skia could not build image from raw pixels ({pixel_format:?} {color_space:?})"
                ),
            }
        })?;
        Ok(Self { inner: image })
    }

    /// Rasterize an SVG XML document into a `Image` of the given
    /// dimensions. `from_encoded` does not decode SVG XML (it handles
    /// raster codecs only); this method is the explicit SVG bridge.
    ///
    /// SVG content is rendered into a transparent linear-light sRGB
    /// surface at the requested width and height, then snapshotted. The
    /// result is suitable for passing to `draw_image_rect` /
    /// `draw_image_src`.
    ///
    /// `width` and `height` set the SVG container size: the SVG's own
    /// `viewBox` and intrinsic dimensions are mapped into this box.
    pub fn from_svg_xml(
        svg: &str,
        width: u32,
        height: u32,
    ) -> Result<Self, Error> {
        if width == 0 || height == 0 {
            return Err(Error::InvalidDimensions {
                width: width as f32,
                height: height as f32,
            });
        }
        let font_mgr = FontMgr::new();
        let mut dom = skia_safe::svg::Dom::from_bytes(svg.as_bytes(), font_mgr)
            .map_err(|_| Error::DecodeImage {
                reason: "could not parse SVG XML".to_string(),
            })?;
        dom.set_container_size(Size::new(width as f32, height as f32));

        let info = ImageInfo::new(
            (width as i32, height as i32),
            ColorType::RGBAF16,
            AlphaType::Premul,
            ColorSpace::new_srgb_linear(),
        );
        let mut surface =
            surfaces::raster(&info, None, None).ok_or_else(|| {
                Error::DecodeImage {
                    reason: format!(
                        "could not allocate {width}x{height} SVG render surface"
                    ),
                }
            })?;
        {
            let canvas = surface.canvas();
            canvas.clear(Color4f::new(0.0, 0.0, 0.0, 0.0));
            dom.render(canvas);
        }
        Ok(Self {
            inner: surface.image_snapshot(),
        })
    }

    /// Returns the width in pixels.
    pub fn width(&self) -> u32 {
        self.inner.width().max(0) as u32
    }

    /// Returns the height in pixels.
    pub fn height(&self) -> u32 {
        self.inner.height().max(0) as u32
    }

    /// Internal alpha mode: `AlphaType::Premul`/`Unpremul`/`Opaque`.
    /// Skia surfaces composite at premultiplied alpha; raw inputs may be
    /// either premul or unpremul depending on the originating producer.
    pub fn is_premultiplied(&self) -> bool {
        matches!(
            self.inner.alpha_type(),
            AlphaType::Premul | AlphaType::Opaque
        )
    }
}
