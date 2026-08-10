use std::fmt;

use crate::{
    backend::RenderEngine,
    color::{LinearColorSpace, OutputColorSpace},
    geometry::Rect,
    pixels::{PixelColorSpace, PixelDepth, PixelFormat},
};

/// Everything this crate can fail with.
///
/// Variants carrying a `reason` wrap a failure reported by Skia or by the
/// platform; the string is Skia's own message where there is one, and is
/// meant for logs rather than for matching on. The rest describe input the
/// crate rejected before doing any work, and carry the offending values so a
/// caller can report them without re-deriving what it passed.
#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    /// Surface or image dimensions were not finite and positive.
    InvalidDimensions {
        /// The rejected width, in pixels.
        width: f32,
        /// The rejected height, in pixels.
        height: f32,
    },
    /// A rectangle was empty, inverted, or had a non-finite edge.
    InvalidRect {
        /// The rejected rectangle.
        rect: Rect,
    },
    /// The working color space is not available in this build.
    UnsupportedColorSpace {
        /// The color space that was asked for.
        color_space: LinearColorSpace,
    },
    /// The requested export color space cannot be encoded to.
    UnsupportedOutputColorSpace {
        /// The color space that was asked for.
        color_space: OutputColorSpace,
    },
    /// The requested pixel-buffer color space is not supported.
    UnsupportedPixelColorSpace {
        /// The color space that was asked for.
        color_space: PixelColorSpace,
    },
    /// The requested channel order or packing is not supported.
    UnsupportedPixelFormat {
        /// The pixel format that was asked for.
        pixel_format: PixelFormat,
    },
    /// The requested bits-per-channel is not supported.
    UnsupportedPixelDepth {
        /// The pixel depth that was asked for.
        depth: PixelDepth,
    },
    /// A caller-supplied row stride does not match the pixel layout.
    InvalidStride {
        /// Bytes per row the layout requires.
        expected: usize,
        /// Bytes per row the caller supplied.
        actual: usize,
    },
    /// A caller-supplied buffer is the wrong size for the pixel layout.
    InvalidByteLength {
        /// Buffer length the layout requires, in bytes.
        expected: usize,
        /// Buffer length the caller supplied, in bytes.
        actual: usize,
    },
    /// Skia declined to allocate the backing surface.
    ///
    /// Usually memory pressure, or a GPU surface larger than the device's
    /// maximum texture size.
    SurfaceCreate {
        /// What the surface backend reported.
        reason: String,
    },
    /// Image bytes could not be decoded.
    ///
    /// The data is truncated, corrupt, or in a format this build of Skia
    /// was not compiled with.
    DecodeImage {
        /// What the decoder reported.
        reason: String,
    },
    /// An SVG path string could not be parsed.
    InvalidSvgPath {
        /// Which part of the path data was rejected.
        reason: String,
    },
    /// Gradient stops or geometry were rejected.
    ///
    /// Stop offsets outside `0.0..=1.0`, fewer than two stops, or a radius
    /// that is negative or non-finite.
    InvalidGradient {
        /// What was wrong with the gradient.
        reason: String,
    },
    /// A font could not be registered with the font collection.
    ///
    /// The bytes are not a font this build can parse, or the file could not
    /// be read.
    FontRegister {
        /// What the font manager reported.
        reason: String,
    },
    /// Skia declined to build an image, color, or mask filter.
    FilterCreate {
        /// What the filter constructor reported.
        reason: String,
    },
    /// Replaying recorded drawing commands onto a surface failed.
    Render {
        /// What the renderer reported.
        reason: String,
    },
    /// Pixels could not be read back from a surface.
    ///
    /// On a GPU surface this includes the readback the driver refused, not
    /// just an unsupported destination layout.
    PixelReadback {
        /// What the surface reported.
        reason: String,
    },
    /// Pixels could not be written into a surface.
    PixelWrite {
        /// What the surface reported.
        reason: String,
    },
    /// Caller pinned [`RenderEngine::Gpu`] but no GPU backend is
    /// compiled in or the runtime cannot reach a device.
    EngineUnavailable {
        /// The engine the caller pinned.
        engine: RenderEngine,
        /// Why it could not be used -- feature not compiled in, no adapter
        /// found, or device creation refused.
        reason: String,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidDimensions { width, height } => {
                write!(f, "invalid dimensions: {width}x{height}")
            }
            Self::InvalidRect { rect } => write!(f, "invalid rect: {rect:?}"),
            Self::UnsupportedColorSpace { color_space } => {
                write!(f, "unsupported linear color space: {color_space:?}")
            }
            Self::UnsupportedOutputColorSpace { color_space } => {
                write!(f, "unsupported output color space: {color_space:?}")
            }
            Self::UnsupportedPixelColorSpace { color_space } => {
                write!(f, "unsupported pixel color space: {color_space:?}")
            }
            Self::UnsupportedPixelFormat { pixel_format } => {
                write!(f, "unsupported pixel format: {pixel_format:?}")
            }
            Self::UnsupportedPixelDepth { depth } => {
                write!(f, "unsupported pixel depth: {depth:?}")
            }
            Self::InvalidStride { expected, actual } => {
                write!(f, "invalid stride: expected {expected}, got {actual}")
            }
            Self::InvalidByteLength { expected, actual } => {
                write!(
                    f,
                    "invalid byte length: expected {expected}, got {actual}"
                )
            }
            Self::SurfaceCreate { reason } => {
                write!(f, "surface create failed: {reason}")
            }
            Self::DecodeImage { reason } => {
                write!(f, "decode image failed: {reason}")
            }
            Self::InvalidSvgPath { reason } => {
                write!(f, "invalid SVG path: {reason}")
            }
            Self::InvalidGradient { reason } => {
                write!(f, "invalid gradient: {reason}")
            }
            Self::FontRegister { reason } => {
                write!(f, "font register failed: {reason}")
            }
            Self::FilterCreate { reason } => {
                write!(f, "filter create failed: {reason}")
            }
            Self::Render { reason } => write!(f, "render failed: {reason}"),
            Self::PixelReadback { reason } => {
                write!(f, "pixel readback failed: {reason}")
            }
            Self::PixelWrite { reason } => {
                write!(f, "pixel write failed: {reason}")
            }
            Self::EngineUnavailable { engine, reason } => {
                write!(f, "render engine {engine:?} unavailable: {reason}")
            }
        }
    }
}

impl std::error::Error for Error {}
