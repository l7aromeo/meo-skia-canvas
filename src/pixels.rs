use skia_safe::{
    AlphaType, ColorSpace as SkColorSpace, ColorType, CubicResampler,
    FilterMode, MipmapMode, SamplingOptions,
};

use crate::{
    backend::RenderEngine,
    color::{LinearColorSpace, OutputColorSpace},
    error::Error,
};

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

/// Whether color channels are scaled by alpha.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AlphaMode {
    /// Color channels are already multiplied by alpha.
    Premultiplied,
    /// Color channels are independent of alpha.
    Unpremultiplied,
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

impl SamplingMode {
    pub(crate) fn to_skia(self) -> SamplingOptions {
        match self {
            Self::Nearest => {
                SamplingOptions::new(FilterMode::Nearest, MipmapMode::None)
            }
            Self::Linear => {
                SamplingOptions::new(FilterMode::Linear, MipmapMode::None)
            }
            Self::Mipmapped => {
                SamplingOptions::new(FilterMode::Linear, MipmapMode::Linear)
            }
            Self::Cubic => SamplingOptions::from(CubicResampler::mitchell()),
        }
    }
}

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

/// An owned pixel buffer read back from a [`Surface`](crate::surface::Surface),
/// together with the layout needed to interpret it.
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

/// How a surface should be built.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SurfaceOptions {
    /// Working color space the surface composites in.
    pub color_space: LinearColorSpace,
    /// Device pixel ratio, honoured only by
    /// [`Recorder::render_raw`](crate::recorder::Recorder::render_raw), which
    /// scales the output frame by it.
    ///
    /// Surfaces built through
    /// [`Backend::create_surface`](crate::backend::Backend::create_surface)
    /// ignore this and use the width and height passed there.
    pub density: f32,
    /// Multisample count, or `None` to disable multisampling.
    pub msaa: Option<usize>,
    /// Which rasterizer to use.
    ///
    /// Default `RenderEngine::Auto` picks the GPU when one is compiled in and
    /// runtime-available, falling back to CPU.
    pub engine: RenderEngine,
}

impl Default for SurfaceOptions {
    fn default() -> Self {
        Self {
            color_space: LinearColorSpace::Srgb,
            density: 1.0,
            msaa: None,
            engine: RenderEngine::Auto,
        }
    }
}

/// How [`Recorder::render_raw`](crate::recorder::Recorder::render_raw)
/// should hand back its pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RawFrameOptions {
    /// Channel layout and alpha mode of the returned buffer.
    pub pixel_format: PixelFormat,
    /// Color space the returned buffer is encoded in.
    pub color_space: OutputColorSpace,
}

impl Default for RawFrameOptions {
    fn default() -> Self {
        Self {
            pixel_format: PixelFormat::Rgba8UnormUnpremul,
            color_space: OutputColorSpace::Srgb,
        }
    }
}

/// A rendered frame as raw bytes, with the layout needed to read it.
#[derive(Debug, Clone, PartialEq)]
pub struct RawFrame {
    width: u32,
    height: u32,
    stride: usize,
    pixel_format: PixelFormat,
    color_space: OutputColorSpace,
    pixels: Vec<u8>,
}

impl RawFrame {
    pub(crate) fn new(
        width: u32,
        height: u32,
        stride: usize,
        pixel_format: PixelFormat,
        color_space: OutputColorSpace,
        pixels: Vec<u8>,
    ) -> Self {
        Self {
            width,
            height,
            stride,
            pixel_format,
            color_space,
            pixels,
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

    /// Returns the row length in bytes.
    pub fn stride(&self) -> usize {
        self.stride
    }

    /// Returns the channel layout and alpha mode.
    pub fn pixel_format(&self) -> PixelFormat {
        self.pixel_format
    }

    /// Returns the color space the pixels are encoded in.
    pub fn color_space(&self) -> OutputColorSpace {
        self.color_space
    }

    /// Borrows the raw bytes.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// Takes ownership of the raw bytes, consuming the frame.
    pub fn into_pixels(self) -> Vec<u8> {
        self.pixels
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
