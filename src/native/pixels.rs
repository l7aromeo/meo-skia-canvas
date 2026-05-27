use skia_safe::{
    AlphaType, ColorSpace as SkColorSpace, ColorType, FilterMode, MipmapMode,
    SamplingOptions,
};

use crate::native::{
    backend::RenderEngine,
    color::{LinearColorSpace, OutputColorSpace},
    error::NativeError,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PixelFormat {
    Rgba8UnormPremul,
    Rgba8UnormUnpremul,
    Rgba16fPremul,
    Rgba32fPremul,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AlphaMode {
    Premultiplied,
    Unpremultiplied,
}

/// Image sampling strategy for `draw_image_src` and similar resampled
/// draws. `Nearest` preserves hard pixel edges (used for ID buffers and
/// preprocessed Citra output); `Linear` uses bilinear filtering;
/// `Mipmapped` enables trilinear sampling for downscales.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SamplingMode {
    Nearest,
    #[default]
    Linear,
    Mipmapped,
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
        }
    }
}

/// Strict export color space for surface read/write. Each variant is its
/// own combination of primaries and transfer function. Linear variants
/// are linear-light; non-linear variants are gamma-coded for the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PixelColorSpace {
    Srgb,
    SrgbLinear,
    DisplayP3,
    DisplayP3Linear,
    Rec2020,
    Rec2020Linear,
}

/// Bit depth of exported pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PixelDepth {
    Uint8,
    F16,
    F32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PixelExportOptions {
    pub color_space: PixelColorSpace,
    pub depth: PixelDepth,
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

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn stride(&self) -> usize {
        self.stride
    }

    pub fn color_space(&self) -> PixelColorSpace {
        self.color_space
    }

    pub fn depth(&self) -> PixelDepth {
        self.depth
    }

    pub fn premultiplied(&self) -> bool {
        self.premultiplied
    }

    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    pub fn into_pixels(self) -> Vec<u8> {
        self.pixels
    }
}

impl PixelColorSpace {
    pub(crate) fn to_skia_color_space(
        self,
    ) -> Result<SkColorSpace, NativeError> {
        use skia_safe::{named_primaries, named_transfer_fn};
        match self {
            Self::Srgb => Ok(SkColorSpace::new_srgb()),
            Self::SrgbLinear => Ok(SkColorSpace::new_srgb_linear()),
            Self::DisplayP3 => SkColorSpace::new_cicp(
                named_primaries::CicpId::SMPTE_EG_432_1,
                named_transfer_fn::CicpId::IEC61966_2_1,
            )
            .ok_or(NativeError::UnsupportedPixelColorSpace {
                color_space: self,
            }),
            Self::DisplayP3Linear => SkColorSpace::new_cicp(
                named_primaries::CicpId::SMPTE_EG_432_1,
                named_transfer_fn::CicpId::Linear,
            )
            .ok_or(NativeError::UnsupportedPixelColorSpace {
                color_space: self,
            }),
            Self::Rec2020 => SkColorSpace::new_cicp(
                named_primaries::CicpId::Rec2020,
                named_transfer_fn::CicpId::Rec709,
            )
            .ok_or(NativeError::UnsupportedPixelColorSpace {
                color_space: self,
            }),
            Self::Rec2020Linear => SkColorSpace::new_cicp(
                named_primaries::CicpId::Rec2020,
                named_transfer_fn::CicpId::Linear,
            )
            .ok_or(NativeError::UnsupportedPixelColorSpace {
                color_space: self,
            }),
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

    pub fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Uint8 => 4,
            Self::F16 => 8,
            Self::F32 => 16,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SurfaceOptions {
    pub color_space: LinearColorSpace,
    pub density: f32,
    pub msaa: Option<usize>,
    /// Which rasterizer to use. Default `RenderEngine::Auto` picks the
    /// GPU when one is compiled in and runtime-available, falling back
    /// to CPU.
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

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RawFrameOptions {
    pub pixel_format: PixelFormat,
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

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn stride(&self) -> usize {
        self.stride
    }

    pub fn pixel_format(&self) -> PixelFormat {
        self.pixel_format
    }

    pub fn color_space(&self) -> OutputColorSpace {
        self.color_space
    }

    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    pub fn into_pixels(self) -> Vec<u8> {
        self.pixels
    }
}

impl PixelFormat {
    pub(crate) fn to_skia_color_type(self) -> Result<ColorType, NativeError> {
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

    pub fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Rgba8UnormPremul | Self::Rgba8UnormUnpremul => 4,
            Self::Rgba16fPremul => 8,
            Self::Rgba32fPremul => 16,
        }
    }
}
