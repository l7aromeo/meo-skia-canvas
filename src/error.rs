use std::fmt;

use crate::{
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
    /// Canvas or image dimensions were not finite and positive.
    InvalidDimensions {
        /// The rejected width, in pixels.
        width: f32,
        /// The rejected height, in pixels.
        height: f32,
    },
    /// A rectangle could not be used: an edge is non-finite, or the rect
    /// reaches past the signed 32-bit coordinate range Skia rounds into.
    ///
    /// Returned by
    /// [`Context2D::get_image_data`](crate::context2d::Context2D::get_image_data)
    /// and its `_as` variant. The rectangle it carries is the one that was
    /// rejected.
    ///
    /// It used to be returned for a bad *radius* as well, carrying a
    /// rectangle built out of the radius rather than one anything refused --
    /// so `round_rect(5, 5, 30, 30, [NaN, 0, 0, 0])` reported "invalid rect"
    /// about a rectangle that is perfectly valid, and `arc` with a negative
    /// radius reported one with its edges crossed. That is
    /// [`Error::InvalidRadius`] now.
    InvalidRect {
        /// The rejected rectangle.
        rect: Rect,
    },
    /// A radius was negative or not finite.
    ///
    /// Returned by every arc and rounded-rectangle builder that takes one --
    /// [`arc`](crate::context2d::Context2D::arc),
    /// [`ellipse`](crate::context2d::Context2D::ellipse),
    /// [`arc_to`](crate::context2d::Context2D::arc_to),
    /// [`round_rect`](crate::context2d::Context2D::round_rect),
    /// [`round_rect_elliptical`](crate::context2d::Context2D::round_rect_elliptical)
    /// and their [`PathBuilder`](crate::path::PathBuilder) counterparts.
    ///
    /// Where several radii are given, this carries the first one that broke
    /// the rule, in the order the caller wrote them.
    InvalidRadius {
        /// The rejected radius.
        radius: f32,
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
    /// A frame index named a frame the image does not have.
    FrameOutOfRange {
        /// The index that was asked for.
        index: usize,
        /// How many frames the image has. A still image has one.
        count: usize,
    },
    /// A CSS length could not be parsed.
    ///
    /// A unit CSS does not define, a bare number other than zero -- CSS
    /// requires a unit on every length but that one -- or a percentage,
    /// which reads like a length and is not one.
    InvalidCssLength {
        /// The input that was rejected.
        reason: String,
    },
    /// A CSS `filter` string could not be parsed.
    ///
    /// The whole string is refused rather than the recognised steps kept: a
    /// chain missing one step is a different picture, and dropping the step
    /// nobody could read is how a typo becomes a rendering bug.
    InvalidFilter {
        /// Which piece failed, and the string it came from.
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
    /// An export option held a value the crate will not act on.
    ///
    /// Distinct from [`Error::Encode`]: nothing was drawn or encoded, the
    /// call was refused on the way in. These were once quietly substituted
    /// -- a negative `fps` became 30, a `quality` outside `0..=1` was
    /// clamped, a mismatched `frame_delays` was ignored -- which the
    /// JavaScript binding had always refused, so the same call behaved
    /// differently depending on which surface made it.
    InvalidExportOption {
        /// The field, named as it appears on
        /// [`EncodeOptions`](crate::export::EncodeOptions).
        option: &'static str,
        /// What was wrong with it.
        reason: String,
    },
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
    ///
    /// Reported when Skia declines the buffer's layout. No
    /// [`ImageData`](crate::pixels::ImageData) the crate hands out
    /// can reach it: every one is tightly packed and length-checked at
    /// construction, which is what Skia checks. It guards the case where
    /// that stops being true -- a padded stride, or a pixel layout a future
    /// Skia refuses.
    PixelWrite {
        /// What the surface reported.
        reason: String,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidDimensions { width, height } => {
                write!(f, "invalid dimensions: {width}x{height}")
            }
            // A rectangle reads as a rectangle. The derived Debug renders
            // `Rect { left: 0.0, top: 0.0, right: 40.0, bottom: 20.0 }` --
            // a Rust struct dump in a message that reaches JavaScript and
            // Python callers, where the line above it already prints
            // `40x20`. Width and height come first because that is what a
            // caller passed; the origin follows because it is context.
            // The value, not the shape it would have described. A caller
            // who passed a negative radius needs to see the radius.
            Self::InvalidRadius { radius } => {
                write!(f, "invalid radius: {radius}")
            }
            Self::InvalidRect { rect } => write!(
                f,
                "invalid rect: {}x{} at {},{}",
                rect.width(),
                rect.height(),
                rect.left,
                rect.top
            ),
            // `as_str` rather than Debug: a caller writes `display-p3-linear`
            // and Debug would answer `DisplayP3Linear`.
            Self::UnsupportedPixelColorSpace { color_space } => {
                write!(
                    f,
                    "unsupported pixel color space: {}",
                    color_space.as_str()
                )
            }
            // `PixelFormat` and `PixelDepth` keep Debug deliberately. Their
            // variants are internal spellings -- `Rgba8UnormPremul`, `Uint8`
            // -- rather than anything a caller writes, so there is no
            // caller-facing name to prefer, and both variants are documented
            // as currently unreachable. Inventing a vocabulary for a message
            // nothing produces would be speculation, not a fix.
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
            Self::FrameOutOfRange { index, count } => {
                write!(
                    f,
                    "frame {index} is out of range; the image has {count}"
                )
            }
            Self::InvalidCssLength { reason } => {
                write!(f, "invalid CSS length: {reason}")
            }
            Self::InvalidFilter { reason } => {
                write!(f, "invalid filter: {reason}")
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
            Self::InvalidExportOption { option, reason } => {
                write!(f, "invalid `{option}`: {reason}")
            }
            Self::Encode { reason } => write!(f, "encode failed: {reason}"),
            Self::PixelReadback { reason } => {
                write!(f, "pixel readback failed: {reason}")
            }
            Self::PixelWrite { reason } => {
                write!(f, "pixel write failed: {reason}")
            }
        }
    }
}

impl std::error::Error for Error {}
