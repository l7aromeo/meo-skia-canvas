use skia_safe::{
    Color as SkColor, Color4f, ColorSpace as SkColorSpace, named_primaries,
    named_transfer_fn,
};

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

/// The sRGB electro-optical transfer function: gamma-encoded to linear.
///
/// The exact inverse of [`linear_to_srgb_byte`]'s encoding step, so a value
/// built by [`RgbaLinear::from_srgb8`] reads back as the byte it came from.
fn srgb_to_linear(v: f32) -> f32 {
    match v <= 0.040_45 {
        true => v / 12.92,
        false => ((v + 0.055) / 1.055).powf(2.4),
    }
}

fn linear_to_srgb_byte(v: f32) -> u8 {
    let v = v.clamp(0.0, 1.0);
    let s = if v <= 0.003_130_8 {
        12.92 * v
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    };
    (s * 255.0).round() as u8
}

/// Working color space a surface composites in, with a linear transfer
/// function.
///
/// Blending happens in linear light, so this picks the primaries only; the
/// transfer function is linear for every variant. Choose the export encoding
/// separately with [`OutputColorSpace`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LinearColorSpace {
    /// sRGB primaries. The default, and what CSS colors are defined in.
    Srgb,
    /// Display P3 primaries -- wider gamut, standard on Apple displays.
    DisplayP3,
    /// Rec. 2020 primaries -- the widest of the three, used by HDR video.
    Rec2020,
}

/// Color space an exported image is encoded in.
///
/// Unlike [`LinearColorSpace`] these carry a display transfer function, since
/// the encoded file is destined for a viewer rather than for further
/// compositing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OutputColorSpace {
    /// sRGB primaries with the sRGB transfer function. Safe everywhere.
    Srgb,
    /// Display P3 primaries with the sRGB transfer function.
    DisplayP3,
    /// Rec. 2020 primaries with the Rec. 709 transfer function.
    Rec2020,
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

    /// Scales the color by `opacity`, clamped to `0.0..=1.0`.
    ///
    /// Every component is scaled, alpha included, which keeps the result
    /// premultiplied.
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

impl LinearColorSpace {}

impl OutputColorSpace {
    pub(crate) fn to_skia_color_space(self) -> Result<SkColorSpace, Error> {
        match self {
            Self::Srgb => Ok(SkColorSpace::new_srgb()),
            Self::DisplayP3 => SkColorSpace::new_cicp(
                named_primaries::CicpId::SMPTE_EG_432_1,
                named_transfer_fn::CicpId::IEC61966_2_1,
            )
            .ok_or(Error::UnsupportedOutputColorSpace { color_space: self }),
            Self::Rec2020 => SkColorSpace::new_cicp(
                named_primaries::CicpId::Rec2020,
                named_transfer_fn::CicpId::Rec709,
            )
            .ok_or(Error::UnsupportedOutputColorSpace { color_space: self }),
        }
    }
}
