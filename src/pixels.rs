use skia_safe::{
    AlphaType, ColorSpace as SkColorSpace, ColorSpacePrimaries,
    ColorSpaceTransferFn, ColorType, named_primaries, named_transfer_fn,
};

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

impl PixelColorSpace {
    /// The name a caller writes for this space.
    ///
    /// The `ColorSpace` union in `lib/index.d.ts` is the authority; where it
    /// lists aliases -- `p3` beside `display-p3`, `bt2020` beside `rec2020`
    /// -- this returns the canonical spelling. Derived `Debug` is not usable
    /// here: it renders `DisplayP3Linear` where the caller wrote
    /// `display-p3-linear`, which is a Rust identifier surfacing in a message
    /// that reaches JavaScript and Python callers.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Srgb => "srgb",
            Self::SrgbLinear => "srgb-linear",
            Self::DisplayP3 => "display-p3",
            Self::DisplayP3Linear => "display-p3-linear",
            Self::Rec2020 => "rec2020",
            Self::Rec2020Linear => "rec2020-linear",
            Self::Rec2020Pq => "rec2020-pq",
            Self::Rec2020Hlg => "rec2020-hlg",
        }
    }
}

/// Pixel layout a surface is read back in, or written from.
///
/// Named for the depth because that is what the first three were about,
/// and kept that way: `Uint8`, `F16` and `F32` are the three a canvas is
/// usually built with, and they lead the list.
///
/// The rest are the layouts the JavaScript binding's `colorType` has always
/// taken and this side could not name -- greyscale, alpha-only, the packed
/// ten-bit formats, the sixteen-bit unorm ones. A Rust caller wanting a
/// single-channel readback had to take four bytes a pixel and throw three
/// of them away.
///
/// Each maps to exactly one Skia colour type, so the two surfaces name the
/// same set. The variants are spelled as the binding spells them, since
/// those are Skia's own names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PixelDepth {
    /// 8-bit unsigned normalized RGBA, 4 bytes per pixel. The usual one.
    Uint8,
    /// 16-bit float RGBA, 8 bytes per pixel.
    F16,
    /// 32-bit float RGBA, 16 bytes per pixel.
    F32,
    /// 8-bit alpha only. Colour reads as zero.
    Alpha8,
    /// 8-bit greyscale. Alpha reads as opaque.
    Gray8,
    /// 8-bit single channel, unsigned normalized.
    R8UNorm,
    /// 8-bit red and green, unsigned normalized.
    R8G8UNorm,
    /// 16-bit float alpha only.
    A16Float,
    /// 16-bit unsigned normalized alpha only.
    A16UNorm,
    /// 4 bits a channel, packed into 16 bits.
    Argb4444,
    /// 5 bits red, 6 green, 5 blue, packed into 16 bits. No alpha.
    Rgb565,
    /// 8-bit RGB with a padding byte. Alpha reads as opaque.
    Rgb888x,
    /// 8-bit BGRA -- the byte order Apple and Windows composite in.
    Bgra8888,
    /// 8-bit RGBA whose colour channels are sRGB-encoded rather than the
    /// space being carried alongside them.
    Srgba8888,
    /// Whichever 8-bit order this platform composites in natively.
    ///
    /// [`Bgra8888`](Self::Bgra8888) on Apple and Windows,
    /// [`Uint8`](Self::Uint8) elsewhere. Naming it asks for the readback
    /// that needs no channel swizzle at all.
    N32,
    /// 10 bits a colour channel and 2 of alpha, packed into 32 bits.
    Rgba1010102,
    /// As [`Rgba1010102`](Self::Rgba1010102), in BGRA order.
    Bgra1010102,
    /// 10 bits a channel with 2 padding bits. No alpha.
    Rgb101010x,
    /// As [`Rgb101010x`](Self::Rgb101010x), in BGR order.
    Bgr101010x,
    /// 16-bit float red and green.
    R16G16Float,
    /// 16-bit unsigned normalized red and green.
    R16G16UNorm,
    /// 16 bits a channel, unsigned normalized RGBA.
    R16G16B16A16UNorm,
    /// 16-bit float RGBA, clamped to `0..=1`.
    F16Norm,
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
pub struct ImageData {
    width: u32,
    height: u32,
    stride: usize,
    color_space: PixelColorSpace,
    depth: PixelDepth,
    premultiplied: bool,
    pixels: Vec<u8>,
}

impl ImageData {
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
    /// let dot = ImageData::from_pixels(
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

/// What is fixed about a color space, in one place.
///
/// Every space this crate offers is a pair of ITU-T H.273 code points -- one
/// naming the primaries, one the transfer function -- which is how Skia is
/// asked for it and, not by coincidence, how a container declares it. AVIF's
/// `colr` box and PNG's `cICP` chunk take the same two numbers, so a format
/// that can be told which space it holds is told with the values in this
/// table rather than with a second list that could disagree.
///
/// The code points are the enum discriminants Skia already carries -- `Rec709`
/// is 1 and `SMPTE_EG_432_1` is 12 because H.273 says so -- so nothing here
/// restates a number that lives somewhere else.
// Not `Copy`: `ColorSpacePrimaries` is eight bare floats but skia-safe
// derives only `Clone` on it.
#[derive(Debug, Clone)]
pub(crate) struct ColorSpaceTraits {
    /// H.273 `colour_primaries`.
    pub primaries: named_primaries::CicpId,
    /// The chromaticities those primaries name, for the containers that
    /// declare a space as xy coordinates rather than as a code point.
    ///
    /// TIFF and BMP both do. They predate H.273 by decades and describe
    /// colour the way the CIE diagram does, which is the same information
    /// spelled differently.
    pub chromaticities: ColorSpacePrimaries,
    /// H.273 `transfer_characteristics`.
    pub transfer: named_transfer_fn::CicpId,
    /// The curve that code point names, in the seven coefficients ICC and
    /// skcms both use.
    ///
    /// For the containers that write a curve rather than name one. PQ and
    /// HLG arrive here with a negative exponent, which is skcms' way of
    /// saying the curve is not parametric and the other coefficients mean
    /// something else -- see `encode::color::ColorProfile`.
    pub transfer_fn: ColorSpaceTransferFn,
}

impl PixelColorSpace {
    /// The space listed after this one, or `None` at the end of the list.
    ///
    /// A chain rather than a `const` array for the reason
    /// `ImageFormat::following` is one: this match is exhaustive, so a new
    /// space will not compile until it has been given a place in the order,
    /// where an array would have accepted one nobody added to it and
    /// [`matching`](Self::matching) would then have failed to recognize a
    /// canvas built in it.
    fn following(self) -> Option<Self> {
        match self {
            Self::Srgb => Some(Self::SrgbLinear),
            Self::SrgbLinear => Some(Self::DisplayP3),
            Self::DisplayP3 => Some(Self::DisplayP3Linear),
            Self::DisplayP3Linear => Some(Self::Rec2020),
            Self::Rec2020 => Some(Self::Rec2020Linear),
            Self::Rec2020Linear => Some(Self::Rec2020Pq),
            Self::Rec2020Pq => Some(Self::Rec2020Hlg),
            Self::Rec2020Hlg => None,
        }
    }

    /// Every space, in declaration order.
    pub(crate) fn all() -> impl Iterator<Item = Self> {
        std::iter::successors(Some(Self::Srgb), |space| space.following())
    }

    /// The space in this list that `skia` is, if it is one of them.
    ///
    /// Compared by Skia's own two hashes rather than by pointer: a space
    /// built from code points and one built by `new_srgb` are the same space
    /// and not the same object, and the hashes are what Skia itself uses to
    /// decide whether a conversion is needed.
    ///
    /// `None` for a canvas built from an ICC profile this crate has no name
    /// for. A caller can reach that through the Rust API, and the honest
    /// answer is that the space cannot be written down in a code point --
    /// which is what the callers of this do with a `None`.
    pub(crate) fn matching(skia: &SkColorSpace) -> Option<Self> {
        Self::all().find(|space| {
            space.to_skia_color_space().is_ok_and(|mine| {
                mine.to_xyzd50_hash() == skia.to_xyzd50_hash()
                    && mine.transfer_fn_hash() == skia.transfer_fn_hash()
            })
        })
    }

    /// Everything fixed about this space.
    pub(crate) fn traits(self) -> ColorSpaceTraits {
        // As with `ImageFormat::traits`, every derived answer reads this
        // match and nothing else, so a new space is one row rather than a
        // hunt for the sites that would otherwise disagree about it.
        match self {
            Self::Srgb => ColorSpaceTraits {
                primaries: named_primaries::CicpId::Rec709,
                chromaticities: named_primaries::REC709,
                transfer: named_transfer_fn::CicpId::IEC61966_2_1,
                transfer_fn: named_transfer_fn::IEC61966_2_1,
            },
            Self::SrgbLinear => ColorSpaceTraits {
                primaries: named_primaries::CicpId::Rec709,
                chromaticities: named_primaries::REC709,
                transfer: named_transfer_fn::CicpId::Linear,
                transfer_fn: named_transfer_fn::LINEAR,
            },
            Self::DisplayP3 => ColorSpaceTraits {
                primaries: named_primaries::CicpId::SMPTE_EG_432_1,
                chromaticities: named_primaries::SMPTE_EG_432_1,
                transfer: named_transfer_fn::CicpId::IEC61966_2_1,
                transfer_fn: named_transfer_fn::IEC61966_2_1,
            },
            Self::DisplayP3Linear => ColorSpaceTraits {
                primaries: named_primaries::CicpId::SMPTE_EG_432_1,
                chromaticities: named_primaries::SMPTE_EG_432_1,
                transfer: named_transfer_fn::CicpId::Linear,
                transfer_fn: named_transfer_fn::LINEAR,
            },
            Self::Rec2020 => ColorSpaceTraits {
                primaries: named_primaries::CicpId::Rec2020,
                chromaticities: named_primaries::REC2020,
                transfer: named_transfer_fn::CicpId::Rec709,
                transfer_fn: named_transfer_fn::REC709,
            },
            Self::Rec2020Linear => ColorSpaceTraits {
                primaries: named_primaries::CicpId::Rec2020,
                chromaticities: named_primaries::REC2020,
                transfer: named_transfer_fn::CicpId::Linear,
                transfer_fn: named_transfer_fn::LINEAR,
            },
            // The same pair the Node binding builds `hdr10` and `hlg` from,
            // so a canvas made either way is the same canvas.
            Self::Rec2020Pq => ColorSpaceTraits {
                primaries: named_primaries::CicpId::Rec2020,
                chromaticities: named_primaries::REC2020,
                transfer: named_transfer_fn::CicpId::PQ,
                transfer_fn: named_transfer_fn::PQ,
            },
            Self::Rec2020Hlg => ColorSpaceTraits {
                primaries: named_primaries::CicpId::Rec2020,
                chromaticities: named_primaries::REC2020,
                transfer: named_transfer_fn::CicpId::HLG,
                transfer_fn: named_transfer_fn::HLG,
            },
        }
    }

    pub(crate) fn to_skia_color_space(self) -> Result<SkColorSpace, Error> {
        let traits = self.traits();
        // sRGB and linear sRGB were built through `new_srgb` and
        // `new_srgb_linear` until the table existed. Measured against the
        // code-point form before the change: identical `to_xyzd50_hash`,
        // identical `transfer_fn_hash`, and `is_srgb()` still true for the
        // one and false for the other. Pinned by a test below, because that
        // equivalence is Skia's to keep rather than ours.
        SkColorSpace::new_cicp(traits.primaries, traits.transfer)
            .ok_or(Error::UnsupportedPixelColorSpace { color_space: self })
    }
}

impl PixelDepth {
    pub(crate) fn to_skia_color_type(self) -> ColorType {
        match self {
            Self::Uint8 => ColorType::RGBA8888,
            Self::F16 => ColorType::RGBAF16,
            Self::F32 => ColorType::RGBAF32,
            Self::Alpha8 => ColorType::Alpha8,
            Self::Gray8 => ColorType::Gray8,
            Self::R8UNorm => ColorType::R8UNorm,
            Self::R8G8UNorm => ColorType::R8G8UNorm,
            Self::A16Float => ColorType::A16Float,
            Self::A16UNorm => ColorType::A16UNorm,
            Self::Argb4444 => ColorType::ARGB4444,
            Self::Rgb565 => ColorType::RGB565,
            Self::Rgb888x => ColorType::RGB888x,
            Self::Bgra8888 => ColorType::BGRA8888,
            Self::Srgba8888 => ColorType::SRGBA8888,
            Self::N32 => ColorType::N32,
            Self::Rgba1010102 => ColorType::RGBA1010102,
            Self::Bgra1010102 => ColorType::BGRA1010102,
            Self::Rgb101010x => ColorType::RGB101010x,
            Self::Bgr101010x => ColorType::BGR101010x,
            Self::R16G16Float => ColorType::R16G16Float,
            Self::R16G16UNorm => ColorType::R16G16UNorm,
            Self::R16G16B16A16UNorm => ColorType::R16G16B16A16UNorm,
            Self::F16Norm => ColorType::RGBAF16Norm,
        }
    }

    /// Returns the size of one pixel in bytes.
    ///
    /// Asked of Skia rather than tabulated here, so a layout this crate has
    /// never thought about the width of still reports the right one.
    pub fn bytes_per_pixel(self) -> usize {
        self.to_skia_color_type().bytes_per_pixel()
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Every space this crate offers.
    fn all() -> [PixelColorSpace; 8] {
        [
            PixelColorSpace::Srgb,
            PixelColorSpace::SrgbLinear,
            PixelColorSpace::DisplayP3,
            PixelColorSpace::DisplayP3Linear,
            PixelColorSpace::Rec2020,
            PixelColorSpace::Rec2020Linear,
            PixelColorSpace::Rec2020Pq,
            PixelColorSpace::Rec2020Hlg,
        ]
    }

    #[test]
    fn the_code_points_build_the_same_srgb_the_named_constructors_did() {
        // `to_skia_color_space` reads the table for every space now,
        // including the two Skia has dedicated constructors for. Those two
        // are the ones that could have moved, so they are checked against
        // the constructors they replaced rather than assumed equivalent.
        let built = |space: PixelColorSpace| {
            space
                .to_skia_color_space()
                .expect("skia knows every code point in the table")
        };

        for (space, expected) in [
            (PixelColorSpace::Srgb, SkColorSpace::new_srgb()),
            (PixelColorSpace::SrgbLinear, SkColorSpace::new_srgb_linear()),
        ] {
            let got = built(space);
            assert_eq!(
                got.to_xyzd50_hash(),
                expected.to_xyzd50_hash(),
                "{space:?} primaries"
            );
            assert_eq!(
                got.transfer_fn_hash(),
                expected.transfer_fn_hash(),
                "{space:?} transfer function"
            );
            // The one that is not a hash: some of Skia's own fast paths ask
            // this question, so a space that stopped answering it would be
            // slower rather than visibly wrong.
            assert_eq!(got.is_srgb(), expected.is_srgb(), "{space:?} is_srgb");
        }
    }

    #[test]
    fn every_space_is_a_pair_of_code_points_skia_accepts() {
        for space in all() {
            let traits = space.traits();
            assert!(
                SkColorSpace::new_cicp(traits.primaries, traits.transfer)
                    .is_some(),
                "{space:?} names a pair skia refuses"
            );
        }
    }

    #[test]
    fn the_spaces_that_share_primaries_share_chromaticities() {
        // The two fields are the same fact twice -- a code point and the xy
        // coordinates it stands for -- so a row that pairs them wrongly is
        // the failure this catches. Grouping by code point and checking the
        // chromaticities agree does it without restating the numbers here,
        // which would only move the chance of a typo.
        for outer in all() {
            for inner in all() {
                if outer.traits().primaries as u8
                    == inner.traits().primaries as u8
                {
                    let (a, b) = (
                        outer.traits().chromaticities,
                        inner.traits().chromaticities,
                    );
                    assert_eq!(
                        (a.rx, a.ry, a.gx, a.gy, a.bx, a.by, a.wx, a.wy),
                        (b.rx, b.ry, b.gx, b.gy, b.bx, b.by, b.wx, b.wy),
                        "{outer:?} and {inner:?} claim the same primaries \
                         code point and different chromaticities"
                    );
                }
            }
        }
    }

    #[test]
    fn the_wide_spaces_are_wider_than_srgb() {
        // A row that named the wrong chromaticities but a self-consistent
        // pair would pass the test above. This one asks the question that
        // makes the values worth carrying: a wide-gamut space has a red
        // primary further from white than sRGB's.
        let srgb = PixelColorSpace::Srgb.traits().chromaticities;
        for space in [PixelColorSpace::DisplayP3, PixelColorSpace::Rec2020] {
            let wide = space.traits().chromaticities;
            assert!(
                wide.rx > srgb.rx && wide.gy > srgb.gy,
                "{space:?} should reach past sRGB, got r.x {} and g.y {}",
                wide.rx,
                wide.gy
            );
        }
    }
}
