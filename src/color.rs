use skia_safe::{Color as SkColor, Color4f, ColorSpace as SkColorSpace};

use crate::error::Error;

/// Linear-light sRGB color space tag for `Color4f` handoffs to Skia.
///
/// Skia's `Paint::set_color4f`, `image_filters::drop_shadow`, and the
/// gradient pipeline all interpret a `Color4f` as **sRGB-encoded** when
/// no color space is supplied. Our `RgbaLinear` carries linear-light
/// values, so we must always pair the `Color4f` with this tag to
/// suppress Skia's implicit gamma decode. (Wider-gamut working spaces
/// would tag with the surface's working space; for the SDR / linear
/// sRGB primaries used internally, this matches.)
pub(crate) fn linear_srgb_color_space() -> SkColorSpace {
    SkColorSpace::new_srgb_linear()
}

/// Converts an `RgbaLinear` to a Skia `Color` (`u32` ARGB, sRGB-encoded by Skia
/// convention).
///
/// Used for sites where Skia accepts only an untagged `Color` (e.g.
/// `TextStyle::set_decoration_color`, `TextShadow::new`): we unpremultiply,
/// gamma-encode linear → sRGB, and quantize to `u8` so Skia's implicit "decode
/// as sRGB" round-trips back to the original linear value.
pub(crate) fn rgba_linear_to_skia_color(color: RgbaLinear) -> SkColor {
    let (r, g, b, a) = if color.a > 0.0 {
        (
            color.r / color.a,
            color.g / color.a,
            color.b / color.a,
            color.a,
        )
    } else {
        (0.0, 0.0, 0.0, 0.0)
    };
    let alpha_byte = (a.clamp(0.0, 1.0) * 255.0).round() as u8;
    SkColor::from_argb(
        alpha_byte,
        linear_to_srgb_byte(r),
        linear_to_srgb_byte(g),
        linear_to_srgb_byte(b),
    )
}

/// A color as the CSS `rgba()` a getter reports.
///
/// sRGB bytes, not the struct's own fields: [`RgbaLinear`] is premultiplied
/// linear-light on 0..1, while CSS `rgb()` takes straight sRGB on 0..255.
/// Emitting the raw floats produces `rgba(0.29,0.02,0.11,0.5)`, which parses
/// back as very nearly black.
///
/// Comma syntax, and the color's own alpha rather than the byte it rounds to:
/// that is the form the JavaScript getters report for the same color, and
/// `0.5` reads better than the `0.5019608` a round trip through 8 bits gives.
pub(crate) fn rgba_css(color: RgbaLinear) -> String {
    let srgb = rgba_linear_to_skia_color(color);
    format!("rgba({},{},{},{})", srgb.r(), srgb.g(), srgb.b(), color.a)
}

/// Unpremultiplies an `RgbaLinear` and emits a `Color4f` carrying the caller-
/// side linear-light values.
///
/// Pair with `linear_srgb_color_space()` when handing the `Color4f` to Skia
/// APIs that take an explicit color space (`set_color4f`, `drop_shadow`,
/// `gradient_shader::linear_with_interpolation`).
pub(crate) fn rgba_linear_to_unpremul_color4f(color: RgbaLinear) -> Color4f {
    if color.a > 0.0 {
        Color4f {
            r: color.r / color.a,
            g: color.g / color.a,
            b: color.b / color.a,
            a: color.a,
        }
    } else {
        Color4f::new(0.0, 0.0, 0.0, 0.0)
    }
}

/// Converts a Skia `Color` back to the crate's premultiplied linear form.
///
/// The inverse of [`rgba_linear_to_skia_color`] as far as an `SkColor` can
/// carry it: eight bits a channel and eight for alpha. A colour that came
/// from 8-bit sRGB with an alpha on a whole 255th round-trips exactly --
/// every one of the 256 bytes and every one of the 256 alphas was checked --
/// but any other alpha is quantised on the way out and returns shifted.
/// `0.5` comes back as `0.5019608`, and the premultiplied components move
/// with it, since they are re-derived from the stored alpha.
///
/// [`Context2D::shadow_color`](crate::context2d::Context2D::shadow_color)
/// reads back through here and inherits that.
/// Premultiplies an unpremultiplied [`Color4f`] back into an [`RgbaLinear`].
///
/// The inverse of [`rgba_linear_to_unpremul_color4f`]. Exact for an alpha of
/// zero or one and for any alpha that is a power of two; elsewhere the pair
/// of divisions and multiplications rounds, so a colour stored and read back
/// can differ in the last bit or so of each component.
pub(crate) fn unpremul_color4f_to_rgba_linear(color: Color4f) -> RgbaLinear {
    RgbaLinear {
        r: color.r * color.a,
        g: color.g * color.a,
        b: color.b * color.a,
        a: color.a,
    }
}

pub(crate) fn skia_color_to_rgba_linear(color: SkColor) -> RgbaLinear {
    let alpha = f32::from(color.a()) / 255.0;
    RgbaLinear::from_srgb(
        f32::from(color.r()) / 255.0,
        f32::from(color.g()) / 255.0,
        f32::from(color.b()) / 255.0,
        alpha,
    )
}

// The sRGB transfer function, from IEC 61966-2-1. The standard writes the
// encode as
//
//     V = 12.92 L                     for L <= 0.0031308
//     V = 1.055 L^(1/2.4) - 0.055     otherwise
//
// and the decode as its inverse, breaking at V <= 0.04045. The two thresholds
// are the same point on the curve, named from either side of it.
//
// The exponent here is 2.4 and not 2.2. Both numbers describe sRGB and they
// are not interchangeable: 2.4 belongs to this piecewise curve, while 2.2 is
// the pure power function that approximates the whole of it -- which is what
// `encode/apng.rs` writes into a PNG `gAMA` chunk, and why that file says so.

/// Breakpoint on the linear-light side.
const SRGB_LINEAR_THRESHOLD: f32 = 0.003_130_8;
/// Breakpoint on the gamma-encoded side. The same point as
/// [`SRGB_LINEAR_THRESHOLD`], through the curve.
const SRGB_ENCODED_THRESHOLD: f32 = 0.040_45;
/// Slope of the linear segment near black, which exists so the curve has a
/// finite derivative at zero.
const SRGB_SLOPE: f32 = 12.92;
/// Scale and offset of the power segment, chosen so the two segments meet
/// with matching value and slope at the breakpoint.
const SRGB_SCALE: f32 = 1.055;
const SRGB_OFFSET: f32 = 0.055;
/// Exponent of the power segment.
const SRGB_EXPONENT: f32 = 2.4;

/// Applies `curve` to the magnitude and puts the sign back.
///
/// Neither direction is clamped, and both are odd-symmetric about zero, so a
/// component outside `0..1` -- which a wider-gamut space reaches -- keeps its
/// sign and its magnitude instead of folding to black or to `NaN` in `powf`.
/// That is the extended sRGB convention CSS Color 4 defines, and it is what
/// makes the two directions exact inverses across the whole line rather than
/// only on `0..1`.
fn odd_symmetric(v: f32, curve: impl Fn(f32) -> f32) -> f32 {
    curve(v.abs()).copysign(v)
}

/// The sRGB electro-optical transfer function: gamma-encoded to linear.
///
/// The exact inverse of [`linear_to_srgb`], so a value built by
/// [`RgbaLinear::from_srgb8`] reads back as the byte it came from.
pub(crate) fn srgb_to_linear(v: f32) -> f32 {
    odd_symmetric(v, |v| match v <= SRGB_ENCODED_THRESHOLD {
        true => v / SRGB_SLOPE,
        false => ((v + SRGB_OFFSET) / SRGB_SCALE).powf(SRGB_EXPONENT),
    })
}

/// The sRGB transfer function: linear light in, gamma-encoded out.
pub(crate) fn linear_to_srgb(v: f32) -> f32 {
    odd_symmetric(v, |v| match v <= SRGB_LINEAR_THRESHOLD {
        true => SRGB_SLOPE * v,
        false => SRGB_SCALE * v.powf(1.0 / SRGB_EXPONENT) - SRGB_OFFSET,
    })
}

/// [`linear_to_srgb`] clamped to the displayable range and quantized to the
/// byte a Skia `Color` holds.
pub(crate) fn linear_to_srgb_byte(v: f32) -> u8 {
    (linear_to_srgb(v.clamp(0.0, 1.0)) * 255.0).round() as u8
}

/// A premultiplied color in linear light.
///
/// Components are premultiplied by alpha and are **not** gamma-encoded, so
/// these are not the 0-255 sRGB bytes a CSS color parses to. Values normally
/// lie in `0.0..=1.0`; wider-gamut spaces may exceed that range.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RgbaLinear {
    /// Red, premultiplied by `a`.
    pub r: f32,
    /// Green, premultiplied by `a`.
    pub g: f32,
    /// Blue, premultiplied by `a`.
    pub b: f32,
    /// Alpha, `0.0` transparent to `1.0` opaque.
    pub a: f32,
}

impl RgbaLinear {
    /// Builds a color from components that are **already** premultiplied by
    /// `a`. No multiplication is performed.
    pub fn new_premultiplied(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self { r, g, b, a }
    }

    /// Builds a color from sRGB bytes, the form a CSS color literal takes.
    ///
    /// This is the constructor to reach for when porting JavaScript:
    /// `fillStyle = "#808080"` is `RgbaLinear::from_srgb8(0x80, 0x80, 0x80,
    /// 1.0)`, **not** `RgbaLinear::opaque(0.5, 0.5, 0.5)`. The latter is
    /// linear-light `0.5`, which encodes back to sRGB byte 188 -- a visibly
    /// lighter grey, 60 levels off.
    ///
    /// `alpha` is `0.0` to `1.0` and is applied by premultiplication, so
    /// `rgba(255, 0, 0, 0.5)` is `from_srgb8(255, 0, 0, 0.5)`.
    ///
    /// # Examples
    ///
    /// ```
    /// use meo_skia_canvas::prelude::*;
    ///
    /// let css_grey = RgbaLinear::from_srgb8(0x80, 0x80, 0x80, 1.0);
    /// let linear_half = RgbaLinear::opaque(0.5, 0.5, 0.5);
    /// assert_ne!(css_grey, linear_half);
    /// ```
    pub fn from_srgb8(r: u8, g: u8, b: u8, alpha: f32) -> Self {
        Self::from_srgb(
            f32::from(r) / 255.0,
            f32::from(g) / 255.0,
            f32::from(b) / 255.0,
            alpha,
        )
    }

    /// Builds a color from sRGB components on `0.0..=1.0`.
    ///
    /// As [`RgbaLinear::from_srgb8`], for callers whose channels are already
    /// normalized. Values outside the range are kept rather than clamped, so
    /// a wide-gamut source survives; `alpha` is clamped, since it scales the
    /// premultiplication.
    pub fn from_srgb(r: f32, g: f32, b: f32, alpha: f32) -> Self {
        let alpha = alpha.clamp(0.0, 1.0);
        Self {
            r: srgb_to_linear(r) * alpha,
            g: srgb_to_linear(g) * alpha,
            b: srgb_to_linear(b) * alpha,
            a: alpha,
        }
    }

    /// Parses a CSS hex color: `#rgb`, `#rgba`, `#rrggbb` or `#rrggbbaa`.
    ///
    /// The leading `#` is optional. Shorthand digits are doubled the way CSS
    /// defines, so `#f00` is `#ff0000`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidColor`] when the string is not one of those
    /// four lengths or contains a non-hex digit.
    ///
    /// # Examples
    ///
    /// ```
    /// use meo_skia_canvas::prelude::*;
    ///
    /// assert_eq!(
    ///     RgbaLinear::from_hex("#f00")?,
    ///     RgbaLinear::from_hex("#ff0000")?
    /// );
    /// assert!(RgbaLinear::from_hex("#not").is_err());
    /// # Ok::<(), meo_skia_canvas::error::Error>(())
    /// ```
    pub fn from_hex(hex: &str) -> Result<Self, Error> {
        // One `#`, not any number of them: `trim_start_matches` accepted
        // `###f00`, which is not a colour in any stylesheet.
        let trimmed = hex.trim();
        let digits = trimmed.strip_prefix('#').unwrap_or(trimmed);
        let reject = || Error::InvalidColor {
            reason: format!("not a hex color: {hex:?}"),
        };
        if !digits.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(reject());
        }

        // CSS doubles each digit of the shorthand forms: #f00 is #ff0000.
        let byte = |at: usize| -> Result<u8, Error> {
            let text = match digits.len() {
                3 | 4 => {
                    let d = &digits[at..at + 1];
                    format!("{d}{d}")
                }
                _ => digits[at * 2..at * 2 + 2].to_string(),
            };
            u8::from_str_radix(&text, 16).map_err(|_| reject())
        };

        match digits.len() {
            3 | 6 => Ok(Self::from_srgb8(byte(0)?, byte(1)?, byte(2)?, 1.0)),
            4 | 8 => Ok(Self::from_srgb8(
                byte(0)?,
                byte(1)?,
                byte(2)?,
                f32::from(byte(3)?) / 255.0,
            )),
            _ => Err(reject()),
        }
    }

    /// Builds a fully opaque color.
    ///
    /// With `a` at `1.0`, premultiplied and straight components coincide, so
    /// the values pass through unchanged.
    pub fn opaque(r: f32, g: f32, b: f32) -> Self {
        Self { r, g, b, a: 1.0 }
    }

    /// The same colour at zero alpha, keeping its hue.
    ///
    /// [`with_opacity`](Self::with_opacity) of `0.0` multiplies the channels
    /// away, which is what premultiplication means and is right everywhere a
    /// colour is painted: at zero alpha nothing is drawn, so the hue cannot
    /// matter. It matters in exactly one place, a
    /// [`GradientStop`](crate::shader::GradientStop), because there the
    /// colour is not painted but interpolated *toward*.
    ///
    /// Multiplied away, a transparent cream is the same four zeros as CSS's
    /// `transparent` -- which is a transparent *black* -- so a gradient
    /// fading cream out fades it toward black. This keeps the channels, so a
    /// stop can say which colour is disappearing. The result is deliberately
    /// not a canonical premultiplied colour: its channels exceed its alpha,
    /// and the gradient path reads them as the straight hue.
    ///
    /// ```
    /// use meo_skia_canvas::prelude::*;
    ///
    /// let cream = RgbaLinear::from_srgb8(246, 242, 238, 1.0);
    /// assert_eq!(
    ///     cream.with_opacity(0.0),
    ///     RgbaLinear::from_srgb8(0, 0, 0, 0.0)
    /// );
    /// assert_ne!(cream.fading_out(), RgbaLinear::from_srgb8(0, 0, 0, 0.0));
    /// ```
    pub fn fading_out(self) -> Self {
        // Undo whatever premultiplication is already in place, so the hue is
        // the straight one whether this started opaque or half-faded.
        let straighten = match self.a > 0.0 {
            true => 1.0 / self.a,
            false => 1.0,
        };
        Self {
            r: self.r * straighten,
            g: self.g * straighten,
            b: self.b * straighten,
            a: 0.0,
        }
    }

    /// Scales the color by `opacity`, clamped to `0.0..=1.0`.
    ///
    /// Every component is scaled, alpha included, which keeps the result
    /// premultiplied. At an `opacity` of zero that leaves nothing of the
    /// hue; [`fading_out`](Self::fading_out) is the one to reach for where
    /// the colour is interpolated toward rather than painted.
    pub fn with_opacity(self, opacity: f32) -> Self {
        let clamped = opacity.clamp(0.0, 1.0);
        Self {
            r: self.r * clamped,
            g: self.g * clamped,
            b: self.b * clamped,
            a: self.a * clamped,
        }
    }
}

#[cfg(test)]
mod transfer_function_tests {
    use super::*;

    /// Every byte survives the trip out to linear light and back.
    ///
    /// This is the property [`RgbaLinear::from_srgb8`] rests on, and the one
    /// the crate's three separate copies of this curve all happened to agree
    /// about -- which is why having three went unnoticed.
    #[test]
    fn every_byte_round_trips() {
        for byte in 0..=u8::MAX {
            let encoded = f32::from(byte) / 255.0;
            let back = linear_to_srgb_byte(srgb_to_linear(encoded));
            assert_eq!(back, byte, "sRGB byte {byte} came back as {back}");
        }
    }

    /// The two directions are inverses *outside* `0..1` as well.
    ///
    /// They were not. `srgb_to_linear` broke on the raw value rather than on
    /// its magnitude, so a negative component took the linear segment however
    /// large it was: -0.5 decoded to -0.0387 and encoded back to -0.217. The
    /// copy in the Node binding was extended and did not have the fault, so
    /// the same component read as two different numbers depending on which
    /// path reached it.
    ///
    /// Negative components are not hypothetical here -- they are how a color
    /// outside the sRGB gamut is carried in sRGB primaries, which is what a
    /// Display P3 canvas hands back.
    #[test]
    fn the_curve_is_odd_symmetric_about_zero() {
        for step in -30..=30 {
            let v = step as f32 / 20.0;
            let round_trip = linear_to_srgb(srgb_to_linear(v));
            assert!(
                (round_trip - v).abs() < 1e-5,
                "{v} round-tripped to {round_trip}"
            );
            // And each direction mirrors, rather than clipping or NaN-ing.
            assert_eq!(srgb_to_linear(-v), -srgb_to_linear(v));
            assert_eq!(linear_to_srgb(-v), -linear_to_srgb(v));
        }
    }

    /// The breakpoints are the same point on the curve, seen from either
    /// side: encoding the linear one lands on the encoded one.
    #[test]
    fn the_two_thresholds_are_one_point() {
        let encoded = linear_to_srgb(SRGB_LINEAR_THRESHOLD);
        assert!(
            (encoded - SRGB_ENCODED_THRESHOLD).abs() < 1e-6,
            "linear {SRGB_LINEAR_THRESHOLD} encodes to {encoded}, \
             but the decode breaks at {SRGB_ENCODED_THRESHOLD}"
        );
    }
}
