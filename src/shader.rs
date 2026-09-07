use skia_safe::{
    Color4f, Point as SkPoint, Shader as SkShader, TileMode,
    gradient::{
        Colors as GradientColors, Gradient as SkGradient, Interpolation,
        interpolation, shaders as gradient_shaders,
    },
    shaders as noise_shaders,
};

use crate::{
    color::{RgbaLinear, linear_to_srgb},
    error::Error,
    export::VectorFeatures,
    geometry::Point,
};

/// Color space a gradient's stops are interpolated in.
///
/// The variants carry the same names and meanings as the `interpolation`
/// property on the JavaScript side and as
/// [CSS Color 4](https://www.w3.org/TR/css-color-4/#interpolation-space), so
/// a gradient described in one can be reproduced in the other. Each maps
/// straight onto Skia's pipeline; none falls back silently.
///
/// The choice shows only between the stops, and it shows a lot. Interpolating
/// black to white, the midpoint reads:
///
/// | space | midpoint |
/// |-------|----------|
/// | [`Srgb`](Self::Srgb) | 128 |
/// | [`SrgbLinear`](Self::SrgbLinear) | 188 |
/// | [`Lab`](Self::Lab), [`Lch`](Self::Lch) | 119 |
/// | [`Oklab`](Self::Oklab), [`Oklch`](Self::Oklch) | 99 |
/// | [`Hsl`](Self::Hsl), [`Hwb`](Self::Hwb) | 128 |
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum GradientColorSpace {
    /// Interpolates in whatever color space the canvas is drawing in. The
    /// default.
    ///
    /// This is what the HTML Standard asks for when nobody has said
    /// otherwise -- stops are interpolated in "the context's color space" --
    /// so it is the right default precisely because it follows the surface.
    /// On an sRGB canvas it is [`Srgb`](Self::Srgb); on a `display-p3` one
    /// it is P3, and the two part company there.
    ///
    /// **Not the compositing operator of the same name.** `"destination"` is
    /// already a `globalCompositeOperation` value, where it means keep the
    /// destination and ignore the source. Nothing is shared but the word:
    /// this one names a colour space to mix stops in and has no bearing on
    /// how the result is composited. A reader who knows the operator will
    /// meet the word in that sense first, which is why this says so.
    #[default]
    Destination,
    /// Interpolates in gamma-encoded sRGB, whatever the canvas is drawing
    /// in.
    ///
    /// The two agree on an sRGB canvas and part on a wide-gamut one. Red to
    /// blue at the midpoint of a `display-p3` canvas reads `[115, 20, 125]`
    /// here against `[115, 25, 142]` through
    /// [`Destination`](Self::Destination); on an sRGB canvas both read
    /// `[126, 0, 129]`.
    ///
    /// **This variant changed meaning.** It followed the surface until this
    /// release, which is what [`Destination`](Self::Destination) is now
    /// for. Code naming it still compiles and draws differently on a canvas
    /// that is not sRGB.
    ///
    /// It previously mapped to [`SrgbLinear`](Self::SrgbLinear) as well, so
    /// a gradient built with the default came out washed out against a
    /// browser: 188 at the midpoint of black to white instead of 128.
    Srgb,
    /// Interpolates in linear-light sRGB. CSS calls this `"srgb-linear"`.
    ///
    /// Physically the honest way to mix light, and the reason it looks wrong
    /// beside a browser: black to white passes through 188, not 128.
    SrgbLinear,
    /// Interpolates in CIE Lab.
    Lab,
    /// Interpolates in Oklab: perceptually uniform, and free of the muddy
    /// grey midpoint plain RGB gives between complementary hues.
    Oklab,
    /// Interpolates in CIE LCH, the cylindrical form of [`Lab`](Self::Lab).
    /// Hue follows the shorter arc.
    Lch,
    /// Interpolates in Oklch, the cylindrical form of [`Oklab`](Self::Oklab).
    /// Hue follows the shorter arc.
    Oklch,
    /// Interpolates in HSL. Hue follows the shorter arc.
    Hsl,
    /// Interpolates in HWB. Hue follows the shorter arc.
    Hwb,
    /// Interpolates in Display P3's primaries.
    DisplayP3,
    /// Interpolates in Rec. 2020's primaries.
    Rec2020,
    /// Interpolates in ProPhoto RGB's primaries.
    ProphotoRgb,
    /// Interpolates in Adobe RGB (1998)'s primaries.
    A98Rgb,
    // Skia's `OKLabGamutMap` and `OKLCHGamutMap` are deliberately not here.
    // Both strip a gradient of all chroma in this configuration: green to
    // yellow, comfortably inside sRGB, has a midpoint of [158, 221, 0]
    // through `Oklab` and [197, 197, 197] through the mapped variant, while
    // grey to grey is unchanged -- so it is the colour that is being
    // destroyed, not the whole pipeline. Gamut mapping needs a destination
    // gamut to map into, and the gradient is built with no color space
    // tagged; tagging one is what the note in `linear_gradient` says crashes
    // this Skia build on the OKLCH variant. Exposing them would ship a
    // choice that silently greys a caller's gradient.
    /// Interpolates in CIE XYZ with a D65 white point. CSS spells this
    /// `xyz-d65`, and `xyz` is its alias.
    ///
    /// Identical in effect to [`SrgbLinear`](Self::SrgbLinear), and
    /// deliberately implemented as it -- see the note on
    /// [`XyzD50`](Self::XyzD50), which carries the argument for all three.
    XyzD65,
    /// CSS's `xyz`, an alias for [`XyzD65`](Self::XyzD65).
    Xyz,
    /// Interpolates in CIE XYZ with a D50 white point. CSS spells this
    /// `xyz-d50`.
    ///
    /// **Also identical in effect to [`SrgbLinear`](Self::SrgbLinear)**, and
    /// this is a proof rather than an approximation. CSS Color 4 section 12
    /// defines interpolation in a space as: convert both endpoints into it,
    /// interpolate each component, convert back. Every step between linear
    /// sRGB and either XYZ white point is an invertible *linear* map -- the
    /// primaries matrix, and Bradford for the white point -- and a linear map
    /// commutes with a componentwise interpolation, so
    /// `M⁻¹·lerp(M·a, M·b, t)` is `lerp(a, b, t)`. Checked numerically
    /// against the specification's own matrices at three positions across
    /// three colour pairs: the largest deviation was 4.4e-16.
    ///
    /// Skia offers no XYZ interpolation space, so the alternative was to
    /// carry two matrices and an adaptation of our own. They would compute an
    /// answer this already gives exactly, and every coefficient would be one
    /// more unreviewable literal.
    XyzD50,
}

impl GradientColorSpace {
    pub(crate) fn to_skia(self) -> interpolation::ColorSpace {
        match self {
            // Two spaces where there was one. `Destination` tracks the
            // surface, which is what the HTML Standard asks for by default;
            // `Srgb` is the literal space, which is what a caller naming
            // sRGB is asking for. They differ only on a canvas that is not
            // sRGB, which is why one name served for so long.
            Self::Destination => interpolation::ColorSpace::Destination,
            Self::Srgb => interpolation::ColorSpace::SRGB,
            Self::SrgbLinear => interpolation::ColorSpace::SRGBLinear,
            Self::Lab => interpolation::ColorSpace::Lab,
            Self::Oklab => interpolation::ColorSpace::OKLab,
            Self::Lch => interpolation::ColorSpace::LCH,
            Self::Oklch => interpolation::ColorSpace::OKLCH,
            Self::Hsl => interpolation::ColorSpace::HSL,
            Self::Hwb => interpolation::ColorSpace::HWB,
            Self::DisplayP3 => interpolation::ColorSpace::DisplayP3,
            Self::Rec2020 => interpolation::ColorSpace::Rec2020,
            Self::ProphotoRgb => interpolation::ColorSpace::ProphotoRGB,
            Self::A98Rgb => interpolation::ColorSpace::A98RGB,
            // The three XYZ spaces are linear re-coordinatisations of linear
            // sRGB, so interpolating in them is interpolating in it. The
            // argument is on `XyzD50`.
            Self::Xyz | Self::XyzD65 | Self::XyzD50 => {
                interpolation::ColorSpace::SRGBLinear
            }
        }
    }

    /// Pairs this space with a hue-interpolation method.
    ///
    /// Only the four cylindrical spaces have a hue axis to walk, so this is
    /// inert for the others -- kept accepted rather than rejected because
    /// the CSS grammar accepts it there too.
    pub fn hue(self, hue: HueMethod) -> GradientInterpolation {
        GradientInterpolation::new(self).with_hue(hue)
    }

    /// Pairs this space with an alpha-interpolation mode.
    pub fn alpha(self, alpha: AlphaInterpolation) -> GradientInterpolation {
        GradientInterpolation::new(self).with_alpha(alpha)
    }
}

/// Which way around the colour wheel a gradient's hue travels.
///
/// Meaningful in the cylindrical spaces -- [`Lch`](GradientColorSpace::Lch),
/// [`Oklch`](GradientColorSpace::Oklch), [`Hsl`](GradientColorSpace::Hsl) and
/// [`Hwb`](GradientColorSpace::Hwb) -- where two hues can be joined going
/// either way. The names and meanings are CSS Color 4's, which is also where
/// the JavaScript side's hue-interpolation property takes them from.
///
/// That property is deliberately not named here. It is being renamed as the
/// proposal's spelling becomes canonical, and a doc comment naming the old
/// one would point a Rust reader at a deprecated alias with nothing to catch
/// it -- `cargo doc` cannot check a string, and the JavaScript half is not in
/// this file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum HueMethod {
    /// Takes the shorter arc between the two hues. The default, and what
    /// every gradient did before this was selectable.
    #[default]
    Shorter,
    /// Takes the longer arc, so red to blue sweeps through green rather than
    /// magenta.
    Longer,
    /// Walks hue upwards, wrapping past 360 if it must.
    Increasing,
    /// Walks hue downwards, wrapping past 0 if it must.
    Decreasing,
}

impl HueMethod {
    pub(crate) fn to_skia(self) -> interpolation::HueMethod {
        match self {
            Self::Shorter => interpolation::HueMethod::Shorter,
            Self::Longer => interpolation::HueMethod::Longer,
            Self::Increasing => interpolation::HueMethod::Increasing,
            Self::Decreasing => interpolation::HueMethod::Decreasing,
        }
    }
}

/// Whether a gradient mixes its stops with the alpha multiplied in.
///
/// The difference shows only through a stop that is not opaque, and there it
/// shows plainly. Fading red to `transparent`:
/// [`Unpremultiplied`](Self::Unpremultiplied) carries the colour down with
/// the alpha and reads `[191, 0, 0, 191]`, `[127, 0, 0, 128]`,
/// `[64, 0, 0, 64]`, which is what a browser draws;
/// [`Premultiplied`](Self::Premultiplied) holds the hue at full strength and
/// reads `[255, 0, 0, a]` the whole way.
///
/// Two values and no more, because alpha is either multiplied in or it is
/// not -- which is why this is not `#[non_exhaustive]` where the space and
/// hue enums are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum AlphaInterpolation {
    /// Mixes the colour channels independently of alpha. The default, and
    /// what a browser does.
    #[default]
    Unpremultiplied,
    /// Mixes the colour channels with alpha already multiplied in, as CSS
    /// Color 4 section 12.3 specifies for CSS gradients.
    ///
    /// Canvas gradients are not CSS gradients and that rule does not govern
    /// them, which is why it is offered rather than imposed.
    Premultiplied,
}

impl AlphaInterpolation {
    pub(crate) fn to_skia(self) -> interpolation::InPremul {
        match self {
            Self::Unpremultiplied => interpolation::InPremul::No,
            Self::Premultiplied => interpolation::InPremul::Yes,
        }
    }
}

/// How a gradient interpolates between its stops: a colour space, and the
/// direction hue travels within it.
///
/// Every gradient factory takes `impl Into<Self>`, so a
/// [`GradientColorSpace`] can be passed on its own where the default hue
/// method will do -- which is every non-cylindrical space, and the common
/// case in the rest.
///
/// ```
/// use meo_skia_canvas::prelude::*;
///
/// let plain = GradientColorSpace::Oklch;
/// let the_long_way = GradientColorSpace::Oklch.hue(HueMethod::Longer);
/// assert_eq!(GradientInterpolation::from(plain).hue, HueMethod::Shorter);
/// assert_eq!(the_long_way.hue, HueMethod::Longer);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub struct GradientInterpolation {
    /// The space the stops are mixed in.
    pub space: GradientColorSpace,
    /// Which way hue travels, for the spaces that have one.
    pub hue: HueMethod,
    /// Whether alpha is multiplied in before mixing.
    pub alpha: AlphaInterpolation,
}

impl GradientInterpolation {
    /// Interpolation in `space`, with the default hue direction and alpha
    /// handling.
    ///
    /// This is the way to build one: the struct is `#[non_exhaustive]`, so a
    /// literal will not compile outside this crate, and a field added later
    /// must not break a caller who never mentioned it. The fields stay
    /// readable.
    pub fn new(space: GradientColorSpace) -> Self {
        Self {
            space,
            hue: HueMethod::default(),
            alpha: AlphaInterpolation::default(),
        }
    }

    /// The same interpolation with a different hue direction.
    pub fn with_hue(mut self, hue: HueMethod) -> Self {
        self.hue = hue;
        self
    }

    /// The same interpolation with a different alpha handling.
    pub fn with_alpha(mut self, alpha: AlphaInterpolation) -> Self {
        self.alpha = alpha;
        self
    }
}

impl From<GradientColorSpace> for GradientInterpolation {
    fn from(space: GradientColorSpace) -> Self {
        Self::new(space)
    }
}

/// One color stop in a gradient.
///
/// `position` is in `0.0..=1.0` along the gradient axis; `color` is
/// `RgbaLinear` premultiplied in the active surface's working color space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GradientStop {
    /// Position along the gradient axis, `0.0` at the start and `1.0` at
    /// the end.
    pub position: f32,
    /// Premultiplied linear-light color at this stop.
    pub color: RgbaLinear,
}

/// Public shader handle used by `Paint::set_shader`.
///
/// Exposes the gradient factories (linear / radial / sweep / two-point conical)
/// plus procedural Perlin noise (fractal noise / turbulence). Mirrors the
/// CanvasKit `ShaderFactory` surface.
#[derive(Clone)]
#[doc(alias = "CanvasGradient")]
pub struct Shader {
    pub(crate) inner: SkShader,
    /// What a vector backend would have to reckon with to write this shader
    /// out. Linear, radial and two-point conical gradients are paint servers
    /// SVG names; a sweep gradient and either noise shader are not.
    pub(crate) features: VectorFeatures,
}

impl std::fmt::Debug for Shader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shader").finish_non_exhaustive()
    }
}

impl Shader {
    /// Validates `stops` and produces the unpremultiplied `Color4f` list,
    /// position list, and interpolation config shared by every gradient
    /// factory.
    ///
    /// Stops must be >= 2, sorted ascending, with the first and last positions
    /// in `0.0..=1.0`.
    fn prepare_stops(
        stops: &[GradientStop],
        interpolation: GradientInterpolation,
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
                // Two conversions, both required, for different reasons.
                //
                // Unpremultiply, because Skia's gradient pipeline takes
                // unpremultiplied `Color4f` and `InPremul::No` below leaves
                // it that way through the interpolation.
                //
                // Then gamma-encode, because the colours are handed over
                // untagged -- `GradientColors::new(.., None)` -- which Skia
                // reads as "already in the destination's working colour
                // space", and that space is gamma-encoded sRGB while
                // `RgbaLinear` is linear light. Skipping this step is what
                // made every gradient far too dark: `#0f1b2d` filled as
                // `[15, 27, 45]` through `set_fill_style` and drew
                // `[1, 3, 7]` as a gradient stop of the same colour. Every
                // gradient test on this crate ramped black to white, and
                // those are the transfer function's two fixed points, so
                // none of them could see it.
                //
                // Encoding here rather than tagging the colours with a
                // linear space and letting Skia convert: tagging engages the
                // primaries-conversion path, which crashes on the OKLCH
                // interpolation variant in this Skia build.
                //
                // Alpha is not gamma-encoded and passes through untouched.
                //
                // A fully transparent stop keeps whatever hue it was built
                // with -- see the zero-alpha arm below.
                let (r, g, b) = match stop.color.a > 0.0 {
                    true => (
                        stop.color.r / stop.color.a,
                        stop.color.g / stop.color.a,
                        stop.color.b / stop.color.a,
                    ),
                    // Nothing was multiplied away at zero alpha, so whatever
                    // is stored is already the straight hue. Forcing black
                    // here instead is what made a colour fading out fade
                    // toward black: `transparent` and a transparent cream
                    // became the same stop, and the animated eye came back
                    // ringed in grey where the binding draws cream.
                    false => (stop.color.r, stop.color.g, stop.color.b),
                };
                Color4f {
                    r: linear_to_srgb(r),
                    g: linear_to_srgb(g),
                    b: linear_to_srgb(b),
                    a: stop.color.a,
                }
            })
            .collect();
        let positions: Vec<f32> = stops.iter().map(|s| s.position).collect();

        let interp = Interpolation {
            // Whatever the caller asked for, defaulting to unpremultiplied
            // -- what a browser does, what the JavaScript binding has always
            // done, and what `a_gradient_fading_to_transparent_carries_its_
            // colour_down` pins. The reasoning for that default, and the
            // values each mode produces, are on `AlphaInterpolation`.
            in_premul: interpolation.alpha.to_skia(),
            color_space: interpolation.space.to_skia(),
            hue_method: interpolation.hue.to_skia(),
        };
        Ok((colors, positions, interp))
    }

    /// Builds a linear gradient between `start` and `end` from a sorted list of
    /// stops.
    ///
    /// Colors are interpreted in the destination surface's working color space
    /// (no extra primaries conversion).
    ///
    /// Colors are interpreted in the destination surface's working color
    /// space. Outside the stop range the endpoint colors extend
    /// indefinitely: the tile mode is fixed at clamp and is not selectable.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidGradient`] unless `stops` holds at least two
    /// entries, sorted by ascending `position`, with the first and last
    /// positions inside `0.0..=1.0` -- or if Skia declines the shader.
    pub fn linear_gradient(
        start: Point,
        end: Point,
        stops: &[GradientStop],
        interpolation: impl Into<GradientInterpolation>,
    ) -> Result<Self, Error> {
        let (colors, positions, interp) =
            Self::prepare_stops(stops, interpolation.into())?;
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
        Ok(Self {
            inner: shader,
            features: VectorFeatures::PLAIN,
        })
    }

    /// Radial gradient centered at `center` with the given `radius`.
    ///
    /// Colors are interpreted in the destination surface's working color
    /// space. Outside the stop range the endpoint colors extend
    /// indefinitely: the tile mode is fixed at clamp and is not selectable.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidGradient`] unless `stops` holds at least two
    /// entries, sorted by ascending `position`, with the first and last
    /// positions inside `0.0..=1.0` -- or if Skia declines the shader.
    pub fn radial_gradient(
        center: Point,
        radius: f32,
        stops: &[GradientStop],
        interpolation: impl Into<GradientInterpolation>,
    ) -> Result<Self, Error> {
        let (colors, positions, interp) =
            Self::prepare_stops(stops, interpolation.into())?;
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
        Ok(Self {
            inner: shader,
            features: VectorFeatures::PLAIN,
        })
    }

    /// Sweep (angular / conic) gradient around `center`, sweeping from
    /// `start_angle` to `end_angle` in degrees (clockwise from +x).
    ///
    /// Colors are interpreted in the destination surface's working color
    /// space. Outside the stop range the endpoint colors extend
    /// indefinitely: the tile mode is fixed at clamp and is not selectable.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidGradient`] unless `stops` holds at least two
    /// entries, sorted by ascending `position`, with the first and last
    /// positions inside `0.0..=1.0` -- or if Skia declines the shader.
    pub fn sweep_gradient(
        center: Point,
        start_angle: f32,
        end_angle: f32,
        stops: &[GradientStop],
        interpolation: impl Into<GradientInterpolation>,
    ) -> Result<Self, Error> {
        let (colors, positions, interp) =
            Self::prepare_stops(stops, interpolation.into())?;
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
        Ok(Self {
            inner: shader,
            features: VectorFeatures::EXOTIC_SHADER,
        })
    }

    /// Two-point conical (two-circle) gradient between a start circle `(start,
    /// start_radius)` and an end circle `(end, end_radius)`.
    ///
    /// The two-circle form CanvasKit exposes that the Canvas2D radial gradient
    /// does not.
    ///
    /// Colors are interpreted in the destination surface's working color
    /// space. Outside the stop range the endpoint colors extend
    /// indefinitely: the tile mode is fixed at clamp and is not selectable.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidGradient`] unless `stops` holds at least two
    /// entries, sorted by ascending `position`, with the first and last
    /// positions inside `0.0..=1.0` -- or if Skia declines the shader.
    pub fn two_point_conical_gradient(
        start: Point,
        start_radius: f32,
        end: Point,
        end_radius: f32,
        stops: &[GradientStop],
        interpolation: impl Into<GradientInterpolation>,
    ) -> Result<Self, Error> {
        let (colors, positions, interp) =
            Self::prepare_stops(stops, interpolation.into())?;
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
        Ok(Self {
            inner: shader,
            features: VectorFeatures::PLAIN,
        })
    }

    /// Procedural fractal (Perlin) noise -- film grain, clouds, organic
    /// texture.
    ///
    /// `base_frequency` is the noise frequency per axis (small values = larger
    /// features); `octaves` adds detail; `seed` varies the pattern. Mirrors
    /// CanvasKit's `Shader.MakeFractalNoise`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidGradient`] when Skia declines to build the
    /// shader. The variant is shared with the gradient factories.
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
        Ok(Self {
            inner: shader,
            features: VectorFeatures::EXOTIC_SHADER,
        })
    }

    /// Procedural turbulence (absolute-value Perlin noise) -- sharper, more
    /// chaotic than fractal noise.
    ///
    /// Mirrors CanvasKit's `Shader.MakeTurbulence`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidGradient`] when Skia declines to build the
    /// shader. The variant is shared with the gradient factories.
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
        Ok(Self {
            inner: shader,
            features: VectorFeatures::EXOTIC_SHADER,
        })
    }
}

#[cfg(test)]
mod interpolation_space_tests {
    use super::*;
    use crate::{
        canvas::{Canvas, CanvasOptions},
        pixels::PixelColorSpace,
    };

    /// The midpoint of a red-to-blue gradient interpolated in `space` and
    /// painted on a canvas in `surface`.
    ///
    /// 64 wide so the midpoint lands on a whole pixel at x = 32, which keeps
    /// the reading off the interpolation rather than off a rounding.
    fn midpoint(
        space: GradientColorSpace,
        surface: PixelColorSpace,
    ) -> [u8; 3] {
        let mut canvas = Canvas::with_options(
            64.0,
            8.0,
            CanvasOptions {
                color_space: surface,
                ..CanvasOptions::default()
            },
        )
        .expect("a canvas in that color space");
        canvas.set_gpu(false);
        {
            let ctx = canvas.context();
            let shader = Shader::linear_gradient(
                Point { x: 0.0, y: 0.0 },
                Point { x: 64.0, y: 0.0 },
                &[
                    GradientStop {
                        position: 0.0,
                        color: RgbaLinear::from_srgb8(255, 0, 0, 1.0),
                    },
                    GradientStop {
                        position: 1.0,
                        color: RgbaLinear::from_srgb8(0, 0, 255, 1.0),
                    },
                ],
                GradientInterpolation::new(space),
            )
            .expect("two stops describe a gradient");
            ctx.set_fill_shader(&shader);
            ctx.fill_rect(0.0, 0.0, 64.0, 8.0);
        }
        let data = canvas
            .context()
            .get_image_data(0.0, 0.0, 64.0, 8.0)
            .expect("read the page back");
        let at = 32 * 4;
        let px = data.pixels();
        [px[at], px[at + 1], px[at + 2]]
    }

    /// `Srgb` and `Destination` are the same answer on an sRGB canvas and
    /// different answers on a wide-gamut one.
    ///
    /// This is what makes the rename a **silent** break: code naming `Srgb`
    /// compiles exactly as it did and draws differently, and only on a canvas
    /// most callers never make. The numbers are asserted rather than
    /// described because they are quoted as evidence in the changelog, and a
    /// figure nothing re-checks is a figure that drifts.
    ///
    /// Both readings come back in their own surface's encoding, which is why
    /// the sRGB row cannot be compared against either P3 row: on a P3 canvas
    /// even an unchanged interpolation reads different bytes.
    #[test]
    fn srgb_and_the_destination_part_company_on_a_wide_gamut_canvas() {
        assert_eq!(
            midpoint(GradientColorSpace::Destination, PixelColorSpace::Srgb),
            midpoint(GradientColorSpace::Srgb, PixelColorSpace::Srgb),
            "on an sRGB canvas the surface is sRGB, so the two agree"
        );
        assert_eq!(
            midpoint(GradientColorSpace::Srgb, PixelColorSpace::Srgb),
            [126, 0, 129],
            "the sRGB canvas reading both names share"
        );

        let following = midpoint(
            GradientColorSpace::Destination,
            PixelColorSpace::DisplayP3,
        );
        let literal =
            midpoint(GradientColorSpace::Srgb, PixelColorSpace::DisplayP3);
        assert_ne!(
            following, literal,
            "on a display-p3 canvas the two must part company"
        );
        assert_eq!(following, [115, 25, 142], "following the surface");
        assert_eq!(literal, [115, 20, 125], "literal sRGB");
    }

    /// The three XYZ spaces are linear sRGB, exactly.
    ///
    /// Asserted as an equality between renders rather than against recorded
    /// bytes, so it keeps meaning if the ramp is ever rebuilt: the claim is
    /// that these four names produce one answer, not that the answer is any
    /// particular colour.
    #[test]
    fn the_xyz_spaces_are_linear_srgb() {
        let reference =
            midpoint(GradientColorSpace::SrgbLinear, PixelColorSpace::Srgb);
        for space in [
            GradientColorSpace::Xyz,
            GradientColorSpace::XyzD65,
            GradientColorSpace::XyzD50,
        ] {
            assert_eq!(
                midpoint(space, PixelColorSpace::Srgb),
                reference,
                "{space:?} interpolates as linear sRGB does"
            );
        }
        assert_ne!(
            reference,
            midpoint(GradientColorSpace::Srgb, PixelColorSpace::Srgb),
            "and linear sRGB is not plain sRGB, or the test above proves \
             nothing"
        );
    }
}
