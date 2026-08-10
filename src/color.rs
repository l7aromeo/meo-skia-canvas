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
/// sRGB primaries used by Studio internally, this matches.)
pub(crate) fn linear_srgb_color_space() -> SkColorSpace {
    SkColorSpace::new_srgb_linear()
}

/// Converts an `RgbaLinear` to a Skia `Color` (u32 ARGB, sRGB-encoded
/// by Skia convention). Used for sites where Skia accepts only an
/// untagged `Color` (e.g. `TextStyle::set_decoration_color`,
/// `TextShadow::new`): we unpremultiply, gamma-encode linear → sRGB,
/// and quantize to u8 so Skia's implicit "decode as sRGB" round-trips
/// back to the original linear value.
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

/// Unpremultiply an `RgbaLinear` and emit a `Color4f` carrying the
/// caller-side linear-light values. Pair with
/// `linear_srgb_color_space()` when handing the `Color4f` to Skia
/// APIs that take an explicit color space (`set_color4f`,
/// `drop_shadow`, `gradient_shader::linear_with_interpolation`).
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

    /// Builds a fully opaque color. With `a` at `1.0`, premultiplied and
    /// straight components coincide, so the values pass through unchanged.
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

impl LinearColorSpace {
    pub(crate) fn to_skia_color_space(self) -> Result<SkColorSpace, Error> {
        match self {
            Self::Srgb => Ok(SkColorSpace::new_srgb_linear()),
            Self::DisplayP3 => SkColorSpace::new_cicp(
                named_primaries::CicpId::SMPTE_EG_432_1,
                named_transfer_fn::CicpId::Linear,
            )
            .ok_or(Error::UnsupportedColorSpace { color_space: self }),
            Self::Rec2020 => SkColorSpace::new_cicp(
                named_primaries::CicpId::Rec2020,
                named_transfer_fn::CicpId::Linear,
            )
            .ok_or(Error::UnsupportedColorSpace { color_space: self }),
        }
    }
}

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
