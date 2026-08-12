use skia_safe::{AlphaType, ColorSpace as SkColorSpace, ColorType};

use crate::error::Error;

/// Channel layout and alpha mode of a raw frame.
///
/// Every variant is RGBA in that byte order; they differ in per-channel
/// width and whether color is premultiplied by alpha.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PixelFormat {
    /// 8 bits per channel, premultiplied. 4 bytes per pixel.
    Rgba8UnormPremul,
    /// 8 bits per channel, unpremultiplied. 4 bytes per pixel, and what
    /// `putImageData` expects.
    Rgba8UnormUnpremul,
    /// 16-bit float per channel, premultiplied. 8 bytes per pixel.
    Rgba16fPremul,
    /// 32-bit float per channel, premultiplied. 16 bytes per pixel.
    Rgba32fPremul,
}

/// Image sampling strategy for `draw_image_src` and similar resampled draws.
///
/// `Nearest` preserves hard pixel edges, which is what ID buffers and already-
/// scaled sources want; `Linear` uses bilinear filtering; `Mipmapped` enables
/// trilinear sampling for downscales.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SamplingMode {
    /// Nearest-neighbour. Keeps hard pixel edges intact.
    Nearest,
    /// Bilinear filtering. The default.
    #[default]
    Linear,
    /// Trilinear filtering off a mipmap chain. Better under minification.
    Mipmapped,
    /// Mitchell-Netravali bicubic resampling -- the highest-quality option for
    /// down/upscaled and moving imagery, where bilinear and even trilinear
    /// (`Mipmapped`) alias or shimmer.
    ///
    /// Mirrors CanvasKit's `drawImageCubic` / `CubicResampler`.
    Cubic,
}

impl SamplingMode {}

/// Strict export color space for surface read/write.
///
/// Each variant is its own combination of primaries and transfer function.
/// Linear variants are linear-light; non-linear variants are gamma-coded for
/// the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PixelColorSpace {
    /// sRGB primaries, sRGB transfer function.
    Srgb,
    /// sRGB primaries, linear transfer function.
    SrgbLinear,
    /// Display P3 primaries, sRGB transfer function.
    DisplayP3,
    /// Display P3 primaries, linear transfer function.
    DisplayP3Linear,
    /// Rec. 2020 primaries, Rec. 709 transfer function.
    Rec2020,
    /// Rec. 2020 primaries, linear transfer function.
    Rec2020Linear,
    /// Rec. 2020 primaries, PQ transfer function -- HDR10.
    ///
    /// The JavaScript names for it are `rec2020-pq` and `hdr10`.
    ///
    /// This builds a canvas that composites through the PQ curve and tags its
    /// exports with it. It does not make the pixels carry HDR: a colour is
    /// still clamped at 1.0 on the way in, and the formats this crate encodes
    /// -- PNG, JPEG, WebP -- are none of them HDR containers. What it is good
    /// for is producing correctly tagged Rec. 2020 output for a pipeline that
    /// takes the raw buffer somewhere else.
    Rec2020Pq,
    /// Rec. 2020 primaries, HLG transfer function -- broadcast HDR.
    ///
    /// The JavaScript names for it are `rec2020-hlg` and `hlg`. Subject to
    /// the same limits as [`Rec2020Pq`](PixelColorSpace::Rec2020Pq).
    Rec2020Hlg,
}

/// Bit depth of exported pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PixelDepth {
    /// 8-bit unsigned normalized, 4 bytes per pixel.
    Uint8,
    /// 16-bit float, 8 bytes per pixel.
    F16,
    /// 32-bit float, 16 bytes per pixel.
    F32,
}

/// Layout to read a surface back in, or write one from.
///
/// [`Default`] is the `putImageData` wire format: sRGB, `Uint8`,
/// unpremultiplied.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PixelExportOptions {
    /// Color space to convert to on the way out, or from on the way in.
    pub color_space: PixelColorSpace,
    /// Bits per channel.
    pub depth: PixelDepth,
    /// Whether color channels are scaled by alpha.
    pub premultiplied: bool,
}

impl Default for PixelExportOptions {
    fn default() -> Self {
        Self {
            color_space: PixelColorSpace::Srgb,
            depth: PixelDepth::Uint8,
            premultiplied: false,
        }
    }
}

impl PixelExportOptions {
    pub(crate) fn to_alpha_type(self) -> AlphaType {
        match self.premultiplied {
            true => AlphaType::Premul,
            false => AlphaType::Unpremul,
        }
    }
}

/// An owned pixel buffer read back from a canvas, together with the layout
/// needed to interpret it.
///
/// Also the crate's `ImageData`: it is what
/// [`Context2D::get_image_data`](crate::context2d::Context2D::get_image_data)
/// returns and what
/// [`Context2D::put_image_data`](crate::context2d::Context2D::put_image_data)
/// takes. One type rather than two, since the fields a canvas needs -- size,
/// row length, color space, alpha mode -- are the fields a readback already
/// carries.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportedPixels {
    width: u32,
    height: u32,
    stride: usize,
    color_space: PixelColorSpace,
    depth: PixelDepth,
    premultiplied: bool,
    pixels: Vec<u8>,
}

impl ExportedPixels {
    pub(crate) fn new(
        width: u32,
        height: u32,
        stride: usize,
        color_space: PixelColorSpace,
        depth: PixelDepth,
        premultiplied: bool,
        pixels: Vec<u8>,
    ) -> Self {
        Self {
            width,
            height,
            stride,
            color_space,
            depth,
            premultiplied,
            pixels,
        }
    }

    /// Allocates a transparent buffer of `width` by `height` in `options`'
    /// layout.
    ///
    /// Every byte is zero, which is transparent black at each supported
    /// depth. This is what
    /// [`Context2D::create_image_data`](crate::context2d::Context2D::create_image_data)
    /// hands back.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidDimensions`] when either dimension is zero.
    /// A buffer with no pixels cannot be drawn and cannot be written into,
    /// so it is rejected here rather than by the draw that would ignore it.
    ///
    /// Returns the same error when the buffer would exceed the signed 32-bit
    /// byte count Skia addresses a pixel buffer with -- 23170 square at the
    /// 4-byte depths. Such a request cannot be used, and allocating it would
    /// either abort the process or, worse, quietly succeed against pages
    /// that are only mapped lazily.
    pub fn blank(
        width: u32,
        height: u32,
        options: PixelExportOptions,
    ) -> Result<Self, Error> {
        let stride = Self::row_bytes(width, height, options)?;
        let len = Self::byte_len(stride, height)?;
        Ok(Self::new(
            width,
            height,
            stride,
            options.color_space,
            options.depth,
            options.premultiplied,
            vec![0; len],
        ))
    }

    /// Wraps a caller-supplied buffer in `options`' layout.
    ///
    /// Rows must be tight: `pixels` is exactly
    /// `width * height * options.depth.bytes_per_pixel()` bytes.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidDimensions`] when either dimension is zero or
    /// the layout would exceed the signed 32-bit byte count Skia addresses a
    /// pixel buffer with, and [`Error::InvalidByteLength`] when `pixels` is
    /// not the length the layout requires.
    ///
    /// # Examples
    ///
    /// ```
    /// use meo_skia_canvas::prelude::*;
    ///
    /// // One opaque red pixel.
    /// let dot = ExportedPixels::from_pixels(
    ///     1,
    ///     1,
    ///     PixelExportOptions::default(),
    ///     vec![255, 0, 0, 255],
    /// )?;
    /// assert_eq!(dot.stride(), 4);
    /// # Ok::<(), meo_skia_canvas::error::Error>(())
    /// ```
    pub fn from_pixels(
        width: u32,
        height: u32,
        options: PixelExportOptions,
        pixels: Vec<u8>,
    ) -> Result<Self, Error> {
        let stride = Self::row_bytes(width, height, options)?;
        let expected = Self::byte_len(stride, height)?;
        if pixels.len() != expected {
            return Err(Error::InvalidByteLength {
                expected,
                actual: pixels.len(),
            });
        }
        Ok(Self::new(
            width,
            height,
            stride,
            options.color_space,
            options.depth,
            options.premultiplied,
            pixels,
        ))
    }

    /// Row length for a tightly packed buffer, rejecting an empty extent.
    fn row_bytes(
        width: u32,
        height: u32,
        options: PixelExportOptions,
    ) -> Result<usize, Error> {
        if width == 0 || height == 0 {
            return Err(Error::InvalidDimensions {
                width: width as f32,
                height: height as f32,
            });
        }
        Ok(width as usize * options.depth.bytes_per_pixel())
    }

    /// Total buffer size, rejecting a product that does not fit and one that
    /// fits but could never be used.
    ///
    /// Checked, not plain: `stride * height` overflows `usize` for dimensions
    /// Skia would refuse anyway, and a release build wraps rather than
    /// panicking. It wrapped to exactly zero for a 2^30-square F32 buffer, so
    /// `blank` returned `Ok` with an empty `Vec` that still reported its full
    /// width, height and stride -- the one invariant this type promises.
    ///
    /// Overflow is not the only way to ask for too much, though, and it is
    /// not the worst. `checked_mul` passes every size below `usize::MAX`,
    /// which left two failure modes above what Skia can address:
    ///
    /// * A merely enormous request aborted the process. `1e9` square is 4×10^18
    ///   bytes, which fits a `usize`, so the check passed and the allocation
    ///   failed -- `rc=134`, not an [`Error`], and nothing a caller could
    ///   catch.
    /// * A request between the two *succeeded*, which is worse. `vec![0; n]`
    ///   allocates zeroed, so the pages are mapped lazily and never touched:
    ///   `100000` square returned `Ok` holding 40 GB of untouched address
    ///   space, ready to kill the process on the first write to it.
    ///
    /// So the ceiling is what Skia can actually address -- a pixel buffer is
    /// measured in signed 32-bit bytes -- which matches the guard on the
    /// readback path in `context::page`, and puts the limit at 23170 square
    /// for the 4-byte depths.
    fn byte_len(stride: usize, height: u32) -> Result<usize, Error> {
        let too_big = || Error::InvalidDimensions {
            width: (stride / 4) as f32,
            height: height as f32,
        };
        let len = stride.checked_mul(height as usize).ok_or_else(too_big)?;
        if len > i32::MAX as usize {
            return Err(too_big());
        }
        Ok(len)
    }

    /// Returns the layout as the options struct that would reproduce it.
    pub fn options(&self) -> PixelExportOptions {
        PixelExportOptions {
            color_space: self.color_space,
            depth: self.depth,
            premultiplied: self.premultiplied,
        }
    }

    /// Returns the width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Returns the height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Returns the row length in bytes. Rows are tight, so this is
    /// `width * bytes_per_pixel`.
    pub fn stride(&self) -> usize {
        self.stride
    }

    /// Returns the color space the pixels are in.
    pub fn color_space(&self) -> PixelColorSpace {
        self.color_space
    }

    /// Returns the bits per channel.
    pub fn depth(&self) -> PixelDepth {
        self.depth
    }

    /// Returns `true` when color channels are scaled by alpha.
    pub fn premultiplied(&self) -> bool {
        self.premultiplied
    }

    /// Borrows the raw bytes.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// Borrows the raw bytes for writing.
    ///
    /// The slice covers the whole buffer, so the layout cannot be
    /// invalidated through it: length, stride and color space stay as
    /// constructed.
    pub fn pixels_mut(&mut self) -> &mut [u8] {
        &mut self.pixels
    }

    /// Takes ownership of the raw bytes, consuming the buffer.
    pub fn into_pixels(self) -> Vec<u8> {
        self.pixels
    }
}

impl PixelColorSpace {
    pub(crate) fn to_skia_color_space(self) -> Result<SkColorSpace, Error> {
        use skia_safe::{named_primaries, named_transfer_fn};
        match self {
            Self::Srgb => Ok(SkColorSpace::new_srgb()),
            Self::SrgbLinear => Ok(SkColorSpace::new_srgb_linear()),
            Self::DisplayP3 => SkColorSpace::new_cicp(
                named_primaries::CicpId::SMPTE_EG_432_1,
                named_transfer_fn::CicpId::IEC61966_2_1,
            )
            .ok_or(Error::UnsupportedPixelColorSpace { color_space: self }),
            Self::DisplayP3Linear => SkColorSpace::new_cicp(
                named_primaries::CicpId::SMPTE_EG_432_1,
                named_transfer_fn::CicpId::Linear,
            )
            .ok_or(Error::UnsupportedPixelColorSpace { color_space: self }),
            Self::Rec2020 => SkColorSpace::new_cicp(
                named_primaries::CicpId::Rec2020,
                named_transfer_fn::CicpId::Rec709,
            )
            .ok_or(Error::UnsupportedPixelColorSpace { color_space: self }),
            Self::Rec2020Linear => SkColorSpace::new_cicp(
                named_primaries::CicpId::Rec2020,
                named_transfer_fn::CicpId::Linear,
            )
            .ok_or(Error::UnsupportedPixelColorSpace { color_space: self }),
            // The same pair the Node binding builds `hdr10` and `hlg` from,
            // so a canvas made either way is the same canvas.
            Self::Rec2020Pq => SkColorSpace::new_cicp(
                named_primaries::CicpId::Rec2020,
                named_transfer_fn::CicpId::PQ,
            )
            .ok_or(Error::UnsupportedPixelColorSpace { color_space: self }),
            Self::Rec2020Hlg => SkColorSpace::new_cicp(
                named_primaries::CicpId::Rec2020,
                named_transfer_fn::CicpId::HLG,
            )
            .ok_or(Error::UnsupportedPixelColorSpace { color_space: self }),
        }
    }
}

impl PixelDepth {
    pub(crate) fn to_skia_color_type(self) -> ColorType {
        match self {
            Self::Uint8 => ColorType::RGBA8888,
            Self::F16 => ColorType::RGBAF16,
            Self::F32 => ColorType::RGBAF32,
        }
    }

    /// Returns the size of one pixel in bytes.
    pub fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Uint8 => 4,
            Self::F16 => 8,
            Self::F32 => 16,
        }
    }
}

impl PixelFormat {
    pub(crate) fn to_skia_color_type(self) -> Result<ColorType, Error> {
        match self {
            Self::Rgba8UnormPremul | Self::Rgba8UnormUnpremul => {
                Ok(ColorType::RGBA8888)
            }
            Self::Rgba16fPremul => Ok(ColorType::RGBAF16),
            Self::Rgba32fPremul => Ok(ColorType::RGBAF32),
        }
    }

    pub(crate) fn to_skia_alpha_type(self) -> AlphaType {
        match self {
            Self::Rgba8UnormUnpremul => AlphaType::Unpremul,
            Self::Rgba8UnormPremul
            | Self::Rgba16fPremul
            | Self::Rgba32fPremul => AlphaType::Premul,
        }
    }

    /// Returns the size of one pixel in bytes.
    pub fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Rgba8UnormPremul | Self::Rgba8UnormUnpremul => 4,
            Self::Rgba16fPremul => 8,
            Self::Rgba32fPremul => 16,
        }
    }
}
