use skia_safe::{
    Color4f, Point as SkPoint, Shader as SkShader, TileMode,
    gradient_shader::{self, Interpolation, interpolation},
};

use crate::native::color::RgbaLinear;
use crate::native::error::NativeError;
use crate::native::geometry::Point;

/// Color-interpolation space for gradient stops. Mirrors Skia's
/// `gradient::Interpolation::ColorSpace`.
///
/// - `Srgb` interpolates in linear-light sRGB primaries (the default
///   Canvas behavior).
/// - `Oklch` interpolates in CIE OKLCH, which is perceptually uniform
///   and avoids the muddy-grey midpoint that plain RGB interpolation
///   produces between complementary hues. Hue interpolation uses the
///   shorter arc.
///
/// No silent fallback: both variants flow through Skia's interpolation
/// pipeline directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum GradientInterpolation {
    #[default]
    Srgb,
    Oklch,
}

impl GradientInterpolation {
    pub(crate) fn to_skia(self) -> interpolation::ColorSpace {
        match self {
            Self::Srgb => interpolation::ColorSpace::SRGBLinear,
            Self::Oklch => interpolation::ColorSpace::OKLCH,
        }
    }
}

/// One color stop in a gradient. `position` is in `0.0..=1.0` along the
/// gradient axis; `color` is `RgbaLinear` premultiplied in the active
/// surface's working color space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GradientStop {
    pub position: f32,
    pub color: RgbaLinear,
}

/// Public shader handle used by `NativePaint::set_shader`. Currently
/// only linear gradients are exposed; radial/sweep/conic land later.
#[derive(Clone)]
pub struct NativeShader {
    pub(crate) inner: SkShader,
}

impl std::fmt::Debug for NativeShader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeShader").finish_non_exhaustive()
    }
}

impl NativeShader {
    /// Build a linear gradient between `start` and `end` from a sorted
    /// list of stops. Stops must be in ascending position order with
    /// positions in `0.0..=1.0`; violations return
    /// `NativeError::InvalidGradient`. Colors are interpreted in the
    /// destination surface's working color space (no extra primaries
    /// conversion).
    pub fn linear_gradient(
        start: Point,
        end: Point,
        stops: &[GradientStop],
        interpolation_space: GradientInterpolation,
    ) -> Result<Self, NativeError> {
        if stops.len() < 2 {
            return Err(NativeError::InvalidGradient {
                reason: format!("need at least 2 stops, got {}", stops.len()),
            });
        }
        for window in stops.windows(2) {
            if window[1].position < window[0].position {
                return Err(NativeError::InvalidGradient {
                    reason: format!(
                        "stops must be sorted by position; saw {} after {}",
                        window[1].position, window[0].position
                    ),
                });
            }
        }
        let first_pos = stops[0].position;
        let last_pos = stops[stops.len() - 1].position;
        if !(0.0..=1.0).contains(&first_pos) || !(0.0..=1.0).contains(&last_pos) {
            return Err(NativeError::InvalidGradient {
                reason: format!("stop positions must be in 0..=1, got [{first_pos}..{last_pos}]"),
            });
        }

        let colors: Vec<Color4f> = stops
            .iter()
            .map(|stop| {
                // Skia's gradient pipeline takes unpremultiplied Color4f;
                // unpremultiply our `RgbaLinear` for input. `InPremul::Yes`
                // below tells Skia to interpolate in premultiplied space,
                // matching Studio's renderer convention.
                if stop.color.a > 0.0 {
                    Color4f {
                        r: stop.color.r / stop.color.a,
                        g: stop.color.g / stop.color.a,
                        b: stop.color.b / stop.color.a,
                        a: stop.color.a,
                    }
                } else {
                    Color4f::new(0.0, 0.0, 0.0, 0.0)
                }
            })
            .collect();
        let positions: Vec<f32> = stops.iter().map(|s| s.position).collect();

        let interp = Interpolation {
            in_premul: interpolation::InPremul::Yes,
            color_space: interpolation_space.to_skia(),
            hue_method: interpolation::HueMethod::Shorter,
        };

        // The gradient pipeline interprets `None` as "treat the
        // `Color4f` stops as already in the destination's working
        // color space", which matches our `RgbaLinear` convention --
        // unlike `Paint::set_color4f`, which treats `None` as
        // sRGB-encoded. Keep `None` here. Tagging with
        // `Some(ColorSpace::new_srgb_linear())` engages Skia's
        // primaries-conversion path, which crashes on the
        // `interpolation::ColorSpace::OKLCH` variant in this Skia
        // build.
        let shader = gradient_shader::linear_with_interpolation(
            (SkPoint::new(start.x, start.y), SkPoint::new(end.x, end.y)),
            (&colors, None),
            positions.as_slice(),
            TileMode::Clamp,
            interp,
            None,
        )
        .ok_or_else(|| NativeError::InvalidGradient {
            reason: "skia could not build linear gradient".to_string(),
        })?;
        Ok(Self { inner: shader })
    }
}
