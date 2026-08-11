use std::fmt;

use crate::{
    backend::RenderEngine,
    color::{LinearColorSpace, OutputColorSpace},
    geometry::Rect,
    pixels::{PixelColorSpace, PixelDepth, PixelFormat},
};

/// Everything this crate can fail with.
///
/// Variants carrying a `reason` describe a failure whose cause is not
/// enumerable. Skia mostly signals failure with a bare `None`, so the string
/// is usually written here rather than reported by Skia: expect an echo of
/// the arguments, useful for logs and not worth matching on. Variants
/// carrying typed values instead describe input the crate rejected, and hand
/// back what was passed so a caller need not re-derive it.
#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    /// Surface or image dimensions were not finite and positive.
    InvalidDimensions {
        /// The rejected width, in pixels.
        width: f32,
        /// The rejected height, in pixels.
        height: f32,
    },
    /// A rectangle could not be used: an edge is non-finite, the rect reaches
    /// past the signed 32-bit coordinate range Skia rounds into, or a radius
    /// that describes one is negative or non-finite.
    ///
    /// Returned by
    /// [`Context2D::get_image_data`](crate::context2d::Context2D::get_image_data)
    /// and its `_as` variant, and by every arc and rounded-rectangle builder
    /// that takes a radius --
    /// [`arc`](crate::context2d::Context2D::arc),
    /// [`ellipse`](crate::context2d::Context2D::ellipse),
    /// [`arc_to`](crate::context2d::Context2D::arc_to),
    /// [`round_rect`](crate::context2d::Context2D::round_rect),
    /// [`round_rect_elliptical`](crate::context2d::Context2D::round_rect_elliptical)
    /// and their [`PathBuilder`](crate::path::PathBuilder) counterparts. All
    /// of them carry the rectangle that was rejected, or the one the radius
    /// described, so the caller can see what was at fault.
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
    ///
    /// Every [`PixelFormat`] maps to a Skia color type today, so nothing
    /// returns this; it exists for formats added later.
    UnsupportedPixelFormat {
        /// The pixel format that was asked for.
        pixel_format: PixelFormat,
    },
    /// The requested bits-per-channel is not supported.
    ///
    /// Every [`PixelDepth`] maps to a Skia color type today, so nothing
    /// returns this; it exists for depths added later.
    UnsupportedPixelDepth {
        /// The pixel depth that was asked for.
        depth: PixelDepth,
    },
    /// A caller-supplied row stride is shorter than one row of pixels.
    ///
    /// Padded rows are accepted; only a stride below the minimum is an
    /// error.
    InvalidStride {
        /// Minimum bytes per row the layout allows.
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
    /// An image could not be constructed.
    ///
    /// Usually a decode: data truncated, corrupt, or in a format this build
    /// of Skia was not compiled with. Also covers wrapping a raw pixel
    /// buffer that Skia declines, and failing to allocate the surface an
    /// SVG rasterizes into.
    DecodeImage {
        /// What went wrong.
        reason: String,
    },
    /// A color string could not be parsed.
    InvalidColor {
        /// The input that was rejected.
        reason: String,
    },
    /// An SVG path string could not be parsed.
    ///
    /// The parser reports no offset, so the whole input is echoed rather
    /// than the offending span.
    InvalidSvgPath {
        /// The path data that was rejected.
        reason: String,
    },
    /// A shader could not be built.
    ///
    /// For gradients: fewer than two stops, stops out of ascending order,
    /// or a first or last position outside `0.0..=1.0`. Radii are not
    /// validated and are passed to Skia as given. Also covers the
    /// procedural noise shaders, which report through this variant when
    /// Skia declines to construct them.
    InvalidGradient {
        /// What was wrong with the gradient.
        reason: String,
    },
    /// A font could not be registered with the font collection.
    ///
    /// The bytes are not a font this build can parse, or the file could not
    /// be read.
    FontRegister {
        /// What went wrong -- an OS error for a file that cannot be read,
        /// or a note that the bytes did not parse.
        reason: String,
    },
    /// Skia declined to build an image, color, or mask filter.
    ///
    /// Skia gives no reason for these, so the string echoes the arguments.
    FilterCreate {
        /// The filter and arguments that were rejected.
        reason: String,
    },
    /// Replaying recorded drawing commands onto a surface failed.
    Render {
        /// What the renderer reported.
        reason: String,
    },
    /// Encoding a drawing to an image or document format failed.
    ///
    /// Distinct from [`Error::Render`]: the drawing itself succeeded and the
    /// encoder rejected it, most often because the page had a zero dimension
    /// or the format could not represent it.
    Encode {
        /// What the encoder reported.
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
            Self::InvalidColor { reason } => {
                write!(f, "invalid color: {reason}")
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
            Self::Encode { reason } => write!(f, "encode failed: {reason}"),
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
