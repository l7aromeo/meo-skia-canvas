use skia_safe::{
    Color4f, Point as SkPoint, Shader as SkShader, TileMode,
    gradient::{
        Colors as GradientColors, Gradient as SkGradient, Interpolation,
        interpolation, shaders as gradient_shaders,
    },
    shaders as noise_shaders,
};

use crate::{color::RgbaLinear, error::Error, geometry::Point};

/// Color-interpolation space for gradient stops. Mirrors Skia's
/// `gradient::Interpolation::ColorSpace`.
///
/// - `Srgb` interpolates in linear-light sRGB primaries (the default Canvas
///   behavior).
/// - `Oklch` interpolates in CIE OKLCH, which is perceptually uniform and
///   avoids the muddy-grey midpoint that plain RGB interpolation produces
///   between complementary hues. Hue interpolation uses the shorter arc.
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

/// Public shader handle used by `Paint::set_shader`. Exposes the
/// gradient factories (linear / radial / sweep / two-point conical) plus
/// procedural Perlin noise (fractal noise / turbulence). Mirrors the
/// CanvasKit `ShaderFactory` surface.
#[derive(Clone)]
pub struct Shader {
    pub(crate) inner: SkShader,
}

impl std::fmt::Debug for Shader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shader").finish_non_exhaustive()
    }
}

impl Shader {
    /// Validate `stops` and produce the unpremultiplied `Color4f` list,
    /// position list, and interpolation config shared by every gradient
    /// factory. Stops must be >= 2, sorted ascending, with the first and
    /// last positions in `0.0..=1.0`.
    fn prepare_stops(
        stops: &[GradientStop],
        interpolation_space: GradientInterpolation,
    ) -> Result<(Vec<Color4f>, Vec<f32>, Interpolation), Error> {
        if stops.len() < 2 {
            return Err(Error::InvalidGradient {
                reason: format!("need at least 2 stops, got {}", stops.len()),
            });
        }
        for window in stops.windows(2) {
            if window[1].position < window[0].position {
                return Err(Error::InvalidGradient {
                    reason: format!(
                        "stops must be sorted by position; saw {} after {}",
                        window[1].position, window[0].position
                    ),
                });
            }
        }
        let first_pos = stops[0].position;
        let last_pos = stops[stops.len() - 1].position;
        if !(0.0..=1.0).contains(&first_pos) || !(0.0..=1.0).contains(&last_pos)
        {
            return Err(Error::InvalidGradient {
                reason: format!(
                    "stop positions must be in 0..=1, got [{first_pos}..{last_pos}]"
                ),
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
        Ok((colors, positions, interp))
    }

    /// Build a linear gradient between `start` and `end` from a sorted
    /// list of stops. Colors are interpreted in the destination
    /// surface's working color space (no extra primaries conversion).
    pub fn linear_gradient(
        start: Point,
        end: Point,
        stops: &[GradientStop],
        interpolation_space: GradientInterpolation,
    ) -> Result<Self, Error> {
        let (colors, positions, interp) =
            Self::prepare_stops(stops, interpolation_space)?;
        // `Colors::new` carries the stops + positions + tile mode +
        // (optional) color space; `None` keeps the pipeline's "treat
        // `Color4f` as already in the destination's working color space"
        // semantic that matches our `RgbaLinear` convention. Tagging a
        // color space would engage Skia's primaries-conversion path,
        // which crashes on the OKLCH variant in this Skia build.
        let stop_colors = GradientColors::new(
            &colors,
            Some(positions.as_slice()),
            TileMode::Clamp,
            None,
        );
        let gradient = SkGradient::new(stop_colors, interp);
        let shader = gradient_shaders::linear_gradient(
            (SkPoint::new(start.x, start.y), SkPoint::new(end.x, end.y)),
            &gradient,
            None,
        )
        .ok_or_else(|| Error::InvalidGradient {
            reason: "skia could not build linear gradient".to_string(),
        })?;
        Ok(Self { inner: shader })
    }

    /// Radial gradient centered at `center` with the given `radius`.
    pub fn radial_gradient(
        center: Point,
        radius: f32,
        stops: &[GradientStop],
        interpolation_space: GradientInterpolation,
    ) -> Result<Self, Error> {
        let (colors, positions, interp) =
            Self::prepare_stops(stops, interpolation_space)?;
        let stop_colors = GradientColors::new(
            &colors,
            Some(positions.as_slice()),
            TileMode::Clamp,
            None,
        );
        let gradient = SkGradient::new(stop_colors, interp);
        let shader = gradient_shaders::radial_gradient(
            (SkPoint::new(center.x, center.y), radius),
            &gradient,
            None,
        )
        .ok_or_else(|| Error::InvalidGradient {
            reason: "skia could not build radial gradient".to_string(),
        })?;
        Ok(Self { inner: shader })
    }

    /// Sweep (angular / conic) gradient around `center`, sweeping from
    /// `start_angle` to `end_angle` in degrees (clockwise from +x).
    pub fn sweep_gradient(
        center: Point,
        start_angle: f32,
        end_angle: f32,
        stops: &[GradientStop],
        interpolation_space: GradientInterpolation,
    ) -> Result<Self, Error> {
        let (colors, positions, interp) =
            Self::prepare_stops(stops, interpolation_space)?;
        let stop_colors = GradientColors::new(
            &colors,
            Some(positions.as_slice()),
            TileMode::Clamp,
            None,
        );
        let gradient = SkGradient::new(stop_colors, interp);
        let shader = gradient_shaders::sweep_gradient(
            SkPoint::new(center.x, center.y),
            (start_angle, end_angle),
            &gradient,
            None,
        )
        .ok_or_else(|| Error::InvalidGradient {
            reason: "skia could not build sweep gradient".to_string(),
        })?;
        Ok(Self { inner: shader })
    }

    /// Two-point conical (two-circle) gradient between a start circle
    /// `(start, start_radius)` and an end circle `(end, end_radius)`.
    /// The two-circle form CanvasKit exposes that the Canvas2D radial
    /// gradient does not.
    pub fn two_point_conical_gradient(
        start: Point,
        start_radius: f32,
        end: Point,
        end_radius: f32,
        stops: &[GradientStop],
        interpolation_space: GradientInterpolation,
    ) -> Result<Self, Error> {
        let (colors, positions, interp) =
            Self::prepare_stops(stops, interpolation_space)?;
        let stop_colors = GradientColors::new(
            &colors,
            Some(positions.as_slice()),
            TileMode::Clamp,
            None,
        );
        let gradient = SkGradient::new(stop_colors, interp);
        let shader = gradient_shaders::two_point_conical_gradient(
            (SkPoint::new(start.x, start.y), start_radius),
            (SkPoint::new(end.x, end.y), end_radius),
            &gradient,
            None,
        )
        .ok_or_else(|| Error::InvalidGradient {
            reason: "skia could not build two-point conical gradient"
                .to_string(),
        })?;
        Ok(Self { inner: shader })
    }

    /// Procedural fractal (Perlin) noise -- film grain, clouds, organic
    /// texture. `base_frequency` is the noise frequency per axis (small
    /// values = larger features); `octaves` adds detail; `seed` varies
    /// the pattern. Mirrors CanvasKit's `Shader.MakeFractalNoise`.
    pub fn fractal_noise(
        base_frequency_x: f32,
        base_frequency_y: f32,
        octaves: usize,
        seed: f32,
    ) -> Result<Self, Error> {
        let shader = noise_shaders::fractal_noise(
            (base_frequency_x, base_frequency_y),
            octaves,
            seed,
            None,
        )
        .ok_or_else(|| Error::InvalidGradient {
            reason: "skia could not build fractal noise shader".to_string(),
        })?;
        Ok(Self { inner: shader })
    }

    /// Procedural turbulence (absolute-value Perlin noise) -- sharper,
    /// more chaotic than fractal noise. Mirrors CanvasKit's
    /// `Shader.MakeTurbulence`.
    pub fn turbulence(
        base_frequency_x: f32,
        base_frequency_y: f32,
        octaves: usize,
        seed: f32,
    ) -> Result<Self, Error> {
        let shader = noise_shaders::turbulence(
            (base_frequency_x, base_frequency_y),
            octaves,
            seed,
            None,
        )
        .ok_or_else(|| Error::InvalidGradient {
            reason: "skia could not build turbulence shader".to_string(),
        })?;
        Ok(Self { inner: shader })
    }
}
