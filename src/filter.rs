use skia_safe::{
    BlurStyle as SkBlurStyle, ColorFilter as SkColorFilter,
    ImageFilter as SkImageFilter, MaskFilter as SkMaskFilter, Point as SkPoint,
    color_filters, image_filters, luma_color_filter,
};

use crate::{
    color::{
        RgbaLinear, linear_srgb_color_space, rgba_linear_to_skia_color,
        rgba_linear_to_unpremul_color4f,
    },
    error::Error,
    node::filter::FilterSpec,
};

/// Image-domain filter (blur, drop shadow, color matrix wrapped as image
/// filter, compose).
///
/// Composed by `Paint` and applied to draws.
#[derive(Clone)]
pub struct ImageFilter {
    pub(crate) inner: SkImageFilter,
}

impl std::fmt::Debug for ImageFilter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImageFilter").finish_non_exhaustive()
    }
}

/// Color-domain filter (luma, gamma transfers, color matrix, compose).
///
/// Composed by `Paint` or wrapped as an image filter via
/// `ImageFilter::from_color_filter`.
#[derive(Clone)]
pub struct ColorFilter {
    pub(crate) inner: SkColorFilter,
}

impl std::fmt::Debug for ColorFilter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ColorFilter").finish_non_exhaustive()
    }
}

/// Coverage-mask blur style. Mirrors CanvasKit's `BlurStyle`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum BlurStyle {
    /// Blur both inside and outside the geometry (the usual soft blur).
    #[default]
    Normal,
    /// Solid interior with a blurred exterior (glow that keeps the shape).
    Solid,
    /// Blur only outside the geometry (outline / halo).
    Outer,
    /// Blur only inside the geometry (inner shadow / feathered fill).
    Inner,
}

impl BlurStyle {
    fn to_skia(self) -> SkBlurStyle {
        match self {
            Self::Normal => SkBlurStyle::Normal,
            Self::Solid => SkBlurStyle::Solid,
            Self::Outer => SkBlurStyle::Outer,
            Self::Inner => SkBlurStyle::Inner,
        }
    }
}

/// Coverage-mask filter applied before rasterization.
///
/// Unlike a plain image-filter blur, the [`BlurStyle`] variants give glows,
/// feathered edges, and outline blurs. Composed by
/// [`Paint`](crate::paint::Paint). Mirrors CanvasKit's `MaskFilter.MakeBlur`.
#[derive(Clone)]
pub struct MaskFilter {
    pub(crate) inner: SkMaskFilter,
}

impl std::fmt::Debug for MaskFilter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MaskFilter").finish_non_exhaustive()
    }
}

impl MaskFilter {
    /// Builds a Gaussian coverage blur.
    ///
    /// `sigma` is the blur standard deviation in pixels. `respect_ctm` scales
    /// the blur with the canvas transform (zoom / keyframed scale); pass
    /// `false` to keep it screen-fixed.
    ///
    /// # Errors
    ///
    /// Returns [`Error::FilterCreate`] when Skia declines to build the
    /// filter.
    pub fn blur(
        style: BlurStyle,
        sigma: f32,
        respect_ctm: bool,
    ) -> Result<Self, Error> {
        SkMaskFilter::blur(style.to_skia(), sigma, respect_ctm)
            .map(|inner| Self { inner })
            .ok_or_else(|| Error::FilterCreate {
                reason: format!("mask blur (style={style:?}, sigma={sigma})"),
            })
    }
}

impl ImageFilter {
    /// Builds a Gaussian blur with separable sigmas.
    ///
    /// `input` is the upstream filter to blur, or `None` to blur the source
    /// draw.
    ///
    /// # Errors
    ///
    /// Returns [`Error::FilterCreate`] when Skia declines to build the
    /// filter.
    pub fn blur(
        sigma_x: f32,
        sigma_y: f32,
        input: Option<ImageFilter>,
    ) -> Result<Self, Error> {
        let inner = input.map(|f| f.inner);
        image_filters::blur((sigma_x, sigma_y), None, inner, None)
            .map(|f| ImageFilter { inner: f })
            .ok_or_else(|| Error::FilterCreate {
                reason: format!("blur({sigma_x}, {sigma_y}) failed"),
            })
    }

    /// Builds a drop shadow at `(dx, dy)` with separable blur sigmas.
    ///
    /// `color` is premultiplied linear and is tagged as linear-light sRGB,
    /// not as the destination's working color space. On a wider-gamut
    /// surface the shadow will therefore not match an equivalent
    /// [`Paint`](crate::paint::Paint) fill of the same `RgbaLinear`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::FilterCreate`] when Skia declines to build the
    /// filter.
    pub fn drop_shadow(
        dx: f32,
        dy: f32,
        sigma_x: f32,
        sigma_y: f32,
        color: RgbaLinear,
        input: Option<ImageFilter>,
    ) -> Result<Self, Error> {
        let unpremul = rgba_linear_to_unpremul_color4f(color);
        let inner = input.map(|f| f.inner);
        // Tag the shadow color as linear-light sRGB. Without an
        // explicit color space, Skia treats the value as
        // sRGB-encoded and gamma-decodes it -- darkening the shadow.
        let cs = linear_srgb_color_space();
        image_filters::drop_shadow(
            skia_safe::Vector::new(dx, dy),
            (sigma_x, sigma_y),
            unpremul,
            Some(cs),
            inner,
            None,
        )
        .map(|f| ImageFilter { inner: f })
        .ok_or_else(|| Error::FilterCreate {
            reason: format!("drop_shadow({dx}, {dy}) failed"),
        })
    }

    /// 4x5 color matrix in row-major order:
    ///
    /// ```text
    /// | r_r  r_g  r_b  r_a  r_offset |
    /// | g_r  g_g  g_b  g_a  g_offset |
    /// | b_r  b_g  b_b  b_a  b_offset |
    /// | a_r  a_g  a_b  a_a  a_offset |
    /// ```
    ///
    /// Output channel `c` = `c_r * r_in + c_g * g_in + c_b * b_in + c_a *
    /// a_in + c_offset`. Offsets are in the `0..1` range for `u8` channels.
    ///
    /// # Errors
    ///
    /// Returns [`Error::FilterCreate`] when Skia declines to build the
    /// filter.
    pub fn color_matrix(
        matrix: [f32; 20],
        input: Option<ImageFilter>,
    ) -> Result<Self, Error> {
        let cf = color_filters::matrix_row_major(&matrix, None);
        let inner = input.map(|f| f.inner);
        image_filters::color_filter(cf, inner, None)
            .map(|f| ImageFilter { inner: f })
            .ok_or_else(|| Error::FilterCreate {
                reason: "color_matrix failed".to_string(),
            })
    }

    /// Wraps a `ColorFilter` as an image filter, optionally chained
    /// onto `input`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::FilterCreate`] when Skia declines to build the
    /// filter.
    pub fn from_color_filter(
        color_filter: ColorFilter,
        input: Option<ImageFilter>,
    ) -> Result<Self, Error> {
        let inner = input.map(|f| f.inner);
        image_filters::color_filter(color_filter.inner, inner, None)
            .map(|f| ImageFilter { inner: f })
            .ok_or_else(|| Error::FilterCreate {
                reason: "from_color_filter failed".to_string(),
            })
    }

    /// Composes two image filters: `outer(inner(source))`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::FilterCreate`] when Skia declines to build the
    /// filter.
    pub fn compose(
        outer: ImageFilter,
        inner: ImageFilter,
    ) -> Result<Self, Error> {
        image_filters::compose(outer.inner, inner.inner)
            .map(|f| ImageFilter { inner: f })
            .ok_or_else(|| Error::FilterCreate {
                reason: "image filter compose failed".to_string(),
            })
    }
}

impl ColorFilter {
    /// Builds Skia's luma color filter.
    ///
    /// Output alpha is the perceived luminance of the input RGB, and output
    /// RGB is zero. Useful as the `inner` filter in a `destination-in` mask
    /// path, where luminance becomes the alpha mask.
    pub fn luma() -> Self {
        Self {
            inner: luma_color_filter::new(),
        }
    }

    /// Applies the linear-to-sRGB gamma transfer to the input color before
    /// downstream draws see it.
    ///
    /// Used to bridge linear-light pipelines to gamma-coded readers.
    pub fn linear_to_srgb_gamma() -> Self {
        Self {
            inner: color_filters::linear_to_srgb_gamma(),
        }
    }

    /// Inverse of `linear_to_srgb_gamma`.
    pub fn srgb_to_linear_gamma() -> Self {
        Self {
            inner: color_filters::srgb_to_linear_gamma(),
        }
    }

    /// Composes two color filters: `outer(inner(input))`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::FilterCreate`] when Skia declines to build the
    /// filter.
    pub fn compose(
        outer: ColorFilter,
        inner: ColorFilter,
    ) -> Result<Self, Error> {
        color_filters::compose(outer.inner, inner.inner)
            .map(|f| ColorFilter { inner: f })
            .ok_or_else(|| Error::FilterCreate {
                reason: "color filter compose failed".to_string(),
            })
    }
}

/// One step of a CSS-style filter chain.
///
/// The Canvas API's `filter` property takes a string such as
/// `"blur(4px) saturate(150%)"`. Here it is a slice of these, passed to
/// [`Context2D::set_filter`](crate::context2d::Context2D::set_filter). No
/// parser: a value that a stylesheet would have to spell correctly is a
/// typed argument instead. One failure mode survives that -- a non-finite
/// amount, which
/// [`set_filter`](crate::context2d::Context2D::set_filter) rejects rather
/// than passing on to fail at the next draw.
///
/// Amounts are fractions, not percentages, so CSS's `150%` is `1.5`.
///
/// What a value means differs by group, which is easy to get backwards:
///
/// - **Scaling filters** -- [`Brightness`](FilterOp::Brightness),
///   [`Contrast`](FilterOp::Contrast), [`Opacity`](FilterOp::Opacity),
///   [`Saturate`](FilterOp::Saturate) -- multiply what is already there, so
///   `1.0` leaves the drawing unchanged and `0.0` erases the property.
/// - **Fraction filters** -- [`Grayscale`](FilterOp::Grayscale),
///   [`Invert`](FilterOp::Invert), [`Sepia`](FilterOp::Sepia) -- apply an
///   effect by amount, so `0.0` leaves the drawing unchanged and `1.0` applies
///   it fully.
/// - **Measured filters** -- [`Blur`](FilterOp::Blur) and
///   [`DropShadow`](FilterOp::DropShadow) in pixels,
///   [`HueRotate`](FilterOp::HueRotate) in degrees -- carry a quantity rather
///   than a fraction. `0.0` still leaves the drawing unchanged, but there is no
///   full: `HueRotate(1.0)` turns the hue by one degree and is all but
///   invisible, where `HueRotate(180.0)` is the opposite colour, and
///   `Blur(1.0)` is a one-pixel radius.
///
/// # Examples
///
/// ```
/// use meo_skia_canvas::prelude::*;
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let mut canvas = Canvas::new(64.0, 64.0);
/// let ctx = canvas.context();
///
/// // "blur(4px) saturate(150%)"
/// ctx.set_filter(&[FilterOp::Blur(4.0), FilterOp::Saturate(1.5)])?;
///
/// // "none"
/// ctx.set_filter(&[])?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FilterOp {
    /// Gaussian blur of the given radius in pixels. `0.0` is no blur.
    Blur(f32),
    /// Linear brightness scale. `1.0` unchanged, `0.0` black.
    Brightness(f32),
    /// Contrast scale about mid-gray. `1.0` unchanged, `0.0` flat gray.
    Contrast(f32),
    /// Desaturation fraction. `0.0` unchanged, `1.0` fully gray.
    Grayscale(f32),
    /// Hue rotation in degrees. `0.0` unchanged.
    HueRotate(f32),
    /// Inversion fraction. `0.0` unchanged, `1.0` fully inverted.
    Invert(f32),
    /// Opacity scale. `1.0` unchanged, `0.0` fully transparent.
    Opacity(f32),
    /// Saturation scale. `1.0` unchanged, `0.0` gray, above `1.0` boosted.
    Saturate(f32),
    /// Sepia fraction. `0.0` unchanged, `1.0` fully sepia.
    Sepia(f32),
    /// A shadow cast by the drawing's alpha, behind it.
    ///
    /// Unlike the [`shadow`](crate::context2d::Context2D::set_shadow_blur)
    /// state, a filter shadow applies to the drawing as a whole after it is
    /// composed, and several can be chained.
    DropShadow {
        /// Horizontal offset in pixels. Positive moves right.
        offset_x: f32,
        /// Vertical offset in pixels. Positive moves down.
        offset_y: f32,
        /// Blur radius in pixels. `0.0` gives a hard-edged shadow.
        blur: f32,
        /// Shadow color.
        color: RgbaLinear,
    },
}

impl FilterOp {
    /// Rejects a value Skia cannot build a filter from.
    ///
    /// The JavaScript side never needs this: its CSS parser discards a
    /// non-finite amount before a `FilterSpec` is ever built. A typed API
    /// hands the float straight through, so the check has to live here --
    /// without it Skia returns a null color filter and skia-safe unwraps
    /// it, aborting on the *next draw* rather than at the offending call.
    pub(crate) fn validate(self) -> Result<(), Error> {
        let reject = |what: &str, value: f32| {
            Err(Error::FilterCreate {
                reason: format!("{what} must be finite, got {value}"),
            })
        };
        let finite = |what: &str, value: f32| match value.is_finite() {
            true => Ok(()),
            false => reject(what, value),
        };

        match self {
            Self::Blur(radius) => finite("blur radius", radius),
            Self::Brightness(amount) => finite("brightness", amount),
            Self::Contrast(amount) => finite("contrast", amount),
            Self::Grayscale(amount) => finite("grayscale", amount),
            Self::HueRotate(degrees) => finite("hue-rotate", degrees),
            Self::Invert(amount) => finite("invert", amount),
            Self::Opacity(amount) => finite("opacity", amount),
            Self::Saturate(amount) => finite("saturate", amount),
            Self::Sepia(amount) => finite("sepia", amount),
            Self::DropShadow {
                offset_x,
                offset_y,
                blur,
                color,
            } => {
                finite("drop-shadow offset x", offset_x)?;
                finite("drop-shadow offset y", offset_y)?;
                finite("drop-shadow blur", blur)?;
                for (channel, value) in [
                    ("r", color.r),
                    ("g", color.g),
                    ("b", color.b),
                    ("a", color.a),
                ] {
                    finite(&format!("drop-shadow color {channel}"), value)?;
                }
                Ok(())
            }
        }
    }

    /// The CSS the operation corresponds to.
    ///
    /// Joined with spaces this is what
    /// [`Context2D::filter`](crate::context2d::Context2D::filter) reports,
    /// so the Rust side round-trips through the same string the JavaScript
    /// side would have set.
    pub(crate) fn to_css(self) -> String {
        match self {
            Self::Blur(radius) => format!("blur({radius}px)"),
            Self::Brightness(amount) => format!("brightness({amount})"),
            Self::Contrast(amount) => format!("contrast({amount})"),
            Self::Grayscale(amount) => format!("grayscale({amount})"),
            Self::HueRotate(degrees) => format!("hue-rotate({degrees}deg)"),
            Self::Invert(amount) => format!("invert({amount})"),
            Self::Opacity(amount) => format!("opacity({amount})"),
            Self::Saturate(amount) => format!("saturate({amount})"),
            Self::Sepia(amount) => format!("sepia({amount})"),
            Self::DropShadow {
                offset_x,
                offset_y,
                blur,
                color,
            } => {
                // sRGB bytes, not the struct's own fields: `RgbaLinear` is
                // premultiplied linear-light on 0..1, while CSS `rgb()`
                // takes straight sRGB on 0..255. Emitting the raw floats
                // produced `rgba(0.29 0.02 0.11 / 0.5)`, which parses back
                // as very nearly black.
                let skia = rgba_linear_to_skia_color(color);
                let alpha = f32::from(skia.a()) / 255.0;
                format!(
                    "drop-shadow({offset_x}px {offset_y}px {blur}px \
                     rgba({} {} {} / {alpha}))",
                    skia.r(),
                    skia.g(),
                    skia.b()
                )
            }
        }
    }

    /// Lowers the operation onto the internal filter spec.
    pub(crate) fn to_spec(self) -> FilterSpec {
        let plain = |name: &str, value: f32| FilterSpec::Plain {
            name: name.to_string(),
            value,
        };
        match self {
            Self::Blur(radius) => plain("blur", radius),
            Self::Brightness(amount) => plain("brightness", amount),
            Self::Contrast(amount) => plain("contrast", amount),
            Self::Grayscale(amount) => plain("grayscale", amount),
            Self::HueRotate(degrees) => plain("hue-rotate", degrees),
            Self::Invert(amount) => plain("invert", amount),
            Self::Opacity(amount) => plain("opacity", amount),
            Self::Saturate(amount) => plain("saturate", amount),
            Self::Sepia(amount) => plain("sepia", amount),
            Self::DropShadow {
                offset_x,
                offset_y,
                blur,
                color,
            } => FilterSpec::Shadow {
                offset: SkPoint::new(offset_x, offset_y),
                blur,
                color: rgba_linear_to_skia_color(color),
            },
        }
    }
}
