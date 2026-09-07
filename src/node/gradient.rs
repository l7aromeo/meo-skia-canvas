#![allow(non_snake_case)]
use crate::shader::{AlphaInterpolation, GradientColorSpace, HueMethod};
use neon::prelude::*;
use skia_safe::{
    Color, Color4f, Matrix, Point, Shader, TileMode,
    gradient::{
        Colors as GradientColors, Gradient as SkGradient, Interpolation,
        shaders as gradient_shaders,
    },
    shaders,
};
use std::{cell::RefCell, rc::Rc};

use crate::{export::VectorFeatures, utils::*};

/// Degrees in a full turn, which is how far `createConicGradient` sweeps.
const FULL_TURN_DEGREES: f32 = 360.0;

enum Gradient {
    Linear {
        start: Point,
        end: Point,
        stops: Vec<f32>,
        colors: Vec<Color4f>,
    },
    Radial {
        start_point: Point,
        start_radius: f32,
        end_point: Point,
        end_radius: f32,
        stops: Vec<f32>,
        colors: Vec<Color4f>,
    },
    Conic {
        center: Point,
        angle: f32,
        /// How far round the sweep runs, in degrees.
        ///
        /// A full turn for `createConicGradient`, which is the only thing
        /// the Canvas API can ask for. Skia sweeps any arc, and the Rust
        /// API has always taken a start and an end -- so this is that
        /// capability reaching the binding, as an optional fourth
        /// argument, rather than a second way to spell 360.
        sweep: f32,
        stops: Vec<f32>,
        colors: Vec<Color4f>,
    },
}

impl Gradient {
    fn get_stops(&self) -> &Vec<f32> {
        match self {
            Gradient::Linear { stops, .. } => stops,
            Gradient::Radial { stops, .. } => stops,
            Gradient::Conic { stops, .. } => stops,
        }
    }

    /// Whether the Canvas standard says this gradient paints nothing.
    ///
    /// Three clauses, and they are the reason this is one predicate rather
    /// than a test at each site that cares. "If there are no stops, the
    /// gradient is transparent black" holds whatever the geometry, so it
    /// covers the conic case, for which the standard describes no
    /// coincident-endpoint condition at all. "If x0 = x1 and y0 = y1, then
    /// the linear gradient must paint nothing" -- exact equality, as the
    /// clause is written: two endpoints a hair apart describe a real, very
    /// steep ramp. "If x0 = x1 and y0 = y1 and r0 = r1, then the radial
    /// gradient must paint nothing" -- all three, so a circle that grows
    /// from a point still paints.
    ///
    /// [`CanvasGradient::is_opaque`] has to agree with this or a full-page
    /// fill takes the erase path in `Context2D::draw_path` and clears the
    /// page that the shader then declines to paint over.
    fn paints_nothing(&self) -> bool {
        if self.get_stops().is_empty() {
            return true;
        }
        match self {
            Gradient::Linear { start, end, .. } => start == end,
            Gradient::Radial {
                start_point,
                end_point,
                start_radius,
                end_radius,
                ..
            } => start_point == end_point && start_radius == end_radius,
            Gradient::Conic { .. } => false,
        }
    }

    fn get_colors(&self) -> &Vec<Color4f> {
        match self {
            Gradient::Linear { colors, .. } => colors,
            Gradient::Radial { colors, .. } => colors,
            Gradient::Conic { colors, .. } => colors,
        }
    }

    fn add_stop(&mut self, offset: f32, color: Color4f) {
        let stops = self.get_stops();

        // insert the new entries at the right index to keep the vectors sorted
        let idx = stops
            .binary_search_by(|n| {
                (n - f32::EPSILON)
                    .partial_cmp(&offset)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .unwrap_or_else(|x| x);
        match self {
            Gradient::Linear { colors, stops, .. } => {
                colors.insert(idx, color);
                stops.insert(idx, offset);
            }
            Gradient::Radial { colors, stops, .. } => {
                colors.insert(idx, color);
                stops.insert(idx, offset);
            }
            Gradient::Conic { colors, stops, .. } => {
                colors.insert(idx, color);
                stops.insert(idx, offset);
            }
        };
    }
}

pub type BoxedCanvasGradient = JsBox<RefCell<CanvasGradient>>;
impl Finalize for CanvasGradient {}

#[derive(Clone)]
pub struct CanvasGradient {
    gradient: Rc<RefCell<Gradient>>,
    color_space: GradientColorSpace,
    hue_method: HueMethod,
    alpha: AlphaInterpolation,
}

impl CanvasGradient {
    /// What a vector backend has to reckon with to name this gradient.
    ///
    /// SVG writes `linearGradient` and `radialGradient` and has nothing to
    /// say for a sweep, so a conic gradient's draws are rasterized into the
    /// document rather than emitted with no fill at all.
    pub fn vector_features(&self) -> VectorFeatures {
        match &*self.gradient.borrow() {
            Gradient::Conic { .. } => VectorFeatures::EXOTIC_SHADER,
            Gradient::Linear { .. } | Gradient::Radial { .. } => {
                VectorFeatures::PLAIN
            }
        }
    }

    /// What a gradient the standard says paints nothing paints.
    ///
    /// A transparent shader rather than `None`. `None` reads as "paint
    /// nothing" and is not: `Paint::set_shader(None)` clears the shader and
    /// leaves the paint's own colour, which is opaque black -- which is
    /// precisely what a gradient with no stops used to cover the fill area
    /// with.
    fn transparent() -> Option<Shader> {
        Some(shaders::color(Color::TRANSPARENT))
    }

    pub fn shader(&self) -> Option<Shader> {
        let interp = Interpolation {
            // Was hard-coded to `No` here, which is what made the crate's
            // `AlphaInterpolation` unreachable from JavaScript: the option
            // existed, threaded through `Shader`'s factories, and this line
            // discarded it on the one path a JavaScript caller can take.
            in_premul: self.alpha.to_skia(),
            color_space: self.color_space.to_skia(),
            hue_method: self.hue_method.to_skia(),
        };

        match &*self.gradient.borrow() {
            // The clauses are on [`Gradient::paints_nothing`], which
            // `is_opaque` reads too.
            ramp if ramp.paints_nothing() => Self::transparent(),

            Gradient::Linear {
                start,
                end,
                stops,
                colors,
            } => {
                let stop_colors = GradientColors::new(
                    colors.as_slice(),
                    Some(stops.as_slice()),
                    TileMode::Clamp,
                    None,
                );
                let gradient = SkGradient::new(stop_colors, interp);
                gradient_shaders::linear_gradient(
                    (*start, *end),
                    &gradient,
                    None,
                )
            }
            Gradient::Radial {
                start_point,
                start_radius,
                end_point,
                end_radius,
                stops,
                colors,
            } => {
                let stop_colors = GradientColors::new(
                    colors.as_slice(),
                    Some(stops.as_slice()),
                    TileMode::Clamp,
                    None,
                );
                let gradient = SkGradient::new(stop_colors, interp);
                gradient_shaders::two_point_conical_gradient(
                    (*start_point, *start_radius),
                    (*end_point, *end_radius),
                    &gradient,
                    None,
                )
            }
            Gradient::Conic {
                center,
                angle,
                sweep,
                stops,
                colors,
            } => {
                let Point { x, y } = *center;
                let mut rotated = Matrix::new_identity();
                rotated
                    .pre_translate((x, y))
                    .pre_rotate(*angle, None)
                    .pre_translate((-x, -y));

                let stop_colors = GradientColors::new(
                    colors.as_slice(),
                    Some(stops.as_slice()),
                    TileMode::Clamp,
                    None,
                );
                let gradient = SkGradient::new(stop_colors, interp);
                // The old `sweep_with_interpolation` defaulted the
                // angle range to `(0, 360)` when passed `None`; the
                // new `sweep_gradient` requires it explicitly.
                gradient_shaders::sweep_gradient(
                    *center,
                    (0.0, *sweep),
                    &gradient,
                    Some(&rotated),
                )
            }
        }
    }

    pub fn add_color_stop(&mut self, offset: f32, color: Color4f) {
        self.gradient.borrow_mut().add_stop(offset, color);
    }

    pub fn is_opaque(&self) -> bool {
        let gradient = self.gradient.borrow();
        // A gradient that paints nothing is not opaque, and saying otherwise
        // is not a cosmetic disagreement: `is_opaque` is one of the guards
        // on the full-page erase in `Context2D::draw_path`, so a no-stop
        // gradient answering `true` here erased the page and then painted
        // nothing over it. An empty stop list makes the `any` below
        // vacuously false, which is where that `true` came from.
        !gradient.paints_nothing()
            && !gradient.get_colors().iter().any(|c| c.a < 1.0)
    }
}

//
// -- Javascript Methods
// --------------------------------------------------------------------------
//

pub fn linear(mut cx: FunctionContext) -> JsResult<BoxedCanvasGradient> {
    let nums = &float_args(&mut cx, &["x1", "y1", "x2", "y2"])?[..4];
    let [x1, y1, x2, y2] = nums else { panic!() };

    let start = Point::new(*x1, *y1);
    let end = Point::new(*x2, *y2);
    let ramp = Gradient::Linear {
        start,
        end,
        stops: vec![],
        colors: vec![],
    };
    let canvas_gradient = CanvasGradient {
        gradient: Rc::new(RefCell::new(ramp)),
        color_space: GradientColorSpace::Destination,
        hue_method: HueMethod::Shorter,
        alpha: AlphaInterpolation::Unpremultiplied,
    };
    let this = RefCell::new(canvas_gradient);
    Ok(cx.boxed(this))
}

pub fn radial(mut cx: FunctionContext) -> JsResult<BoxedCanvasGradient> {
    let nums =
        &float_args(&mut cx, &["x1", "y1", "r1", "x2", "y2", "r2"])?[..6];
    let [x1, y1, r1, x2, y2, r2] = nums else {
        panic!()
    };

    let start_point = Point::new(*x1, *y1);
    let end_point = Point::new(*x2, *y2);
    let bloom = Gradient::Radial {
        start_point,
        start_radius: *r1,
        end_point,
        end_radius: *r2,
        stops: vec![],
        colors: vec![],
    };
    let canvas_gradient = CanvasGradient {
        gradient: Rc::new(RefCell::new(bloom)),
        color_space: GradientColorSpace::Destination,
        hue_method: HueMethod::Shorter,
        alpha: AlphaInterpolation::Unpremultiplied,
    };
    let this = RefCell::new(canvas_gradient);
    Ok(cx.boxed(this))
}

pub fn conic(mut cx: FunctionContext) -> JsResult<BoxedCanvasGradient> {
    let nums = &float_args(&mut cx, &["theta", "x", "y"])?[..3];
    let [theta, x, y] = nums else { panic!() };

    // A fourth argument, past what `createConicGradient` takes, naming how
    // far round the sweep runs. Absent, it is the full turn the Canvas API
    // always draws.
    let sweep_radians = opt_float_arg(&mut cx, 4);
    if let Some(radians) = sweep_radians
        && (!radians.is_finite() || radians <= 0.0)
    {
        return cx.throw_range_error(format!(
            "Expected a positive number for `endAngle` (got {radians})"
        ));
    }

    let center = Point::new(*x, *y);
    let angle = theta.to_degrees();
    let sweep = Gradient::Conic {
        center,
        angle,
        sweep: sweep_radians.map_or(FULL_TURN_DEGREES, f32::to_degrees),
        stops: vec![],
        colors: vec![],
    };
    let canvas_gradient = CanvasGradient {
        gradient: Rc::new(RefCell::new(sweep)),
        color_space: GradientColorSpace::Destination,
        hue_method: HueMethod::Shorter,
        alpha: AlphaInterpolation::Unpremultiplied,
    };
    let this = RefCell::new(canvas_gradient);
    Ok(cx.boxed(this))
}

pub fn addColorStop(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let this = cx.argument::<BoxedCanvasGradient>(0)?;
    let mut this = this.borrow_mut();

    let offset = float_arg(&mut cx, 1, "offset")?;
    if !(0.0..=1.0).contains(&offset) {
        // "If offset is less than 0 or greater than 1, then throw an
        // IndexSizeError" -- the Canvas standard, and what Chrome raises. The
        // name in front is read by `lib/classes/neon.js`, which builds the
        // `DOMException`: Neon can construct an `Error`, a `TypeError` and a
        // `RangeError` and nothing else, so it has to cross as text.
        //
        // The offset is in the message because a caller who passed the wrong
        // one needs to see it. This was the only refusal in the range family
        // that named the permitted bounds and not the value that missed them.
        return cx.throw_error(format!(
            "IndexSizeError: The provided value ({offset}) is outside the \
             range [0.0, 1.0]"
        ));
    }

    // Accept either a CSS string or a `[r, g, b, a]` premultiplied
    // linear-light float array (the `Color4fInput` shape mirroring
    // `TextColorInput`). A string naming a `color()` space is converted to
    // sRGB here rather than tagged: Skia interpolates the stop values it is
    // given, so an unconverted stop is read as sRGB and the space is lost --
    // `color(srgb-linear 0.2 0.4 0.6)` painted 51,102,153 as a stop where the
    // same string fills 124,170,203. The stops then flow into Skia's
    // interpolation as-is; callers that need a non-default interpolation
    // color space set it via `gradient.interpolation`.
    let color_arg = cx.argument::<JsValue>(2)?;
    if let Some((color4f, cs)) = color4f_in(&mut cx, color_arg) {
        this.add_color_stop(offset, color4f_to_srgb(color4f, cs.as_ref()));
    } else {
        // "If color cannot be parsed as a CSS <color> value, then throw a
        // SyntaxError" -- the Canvas standard, and again what Chrome raises.
        // Reached only from here: `fillStyle` and its neighbours ignore a
        // colour they cannot parse, as the standard separately requires, and
        // do not come through this function.
        let shown = color_arg.to_string(&mut cx)?.value(&mut cx);
        return cx.throw_error(format!(
            "SyntaxError: The value provided (\"{shown}\") could not be \
             parsed as a color"
        ));
    }

    Ok(cx.undefined())
}

pub fn repr(mut cx: FunctionContext) -> JsResult<JsString> {
    let this = cx.argument::<BoxedCanvasGradient>(0)?;
    let this = this.borrow();
    let gradient = Rc::clone(&this.gradient);

    let style = match &*gradient.borrow() {
        Gradient::Linear { .. } => "Linear",
        Gradient::Radial { .. } => "Radial",
        Gradient::Conic { .. } => "Conic",
    };

    Ok(cx.string(style))
}

//
// -- Interpolation color space
// --------------------------------------------------------------------------
//

fn color_space_to_str(cs: GradientColorSpace) -> &'static str {
    match cs {
        GradientColorSpace::Destination => "destination",
        GradientColorSpace::Srgb => "srgb",
        GradientColorSpace::SrgbLinear => "srgb-linear",
        GradientColorSpace::Lab => "lab",
        GradientColorSpace::Oklab => "oklab",
        GradientColorSpace::Oklch => "oklch",
        GradientColorSpace::Lch => "lch",
        GradientColorSpace::Hsl => "hsl",
        GradientColorSpace::Hwb => "hwb",
        // Added because closing `GradientColorSpace` made this match total.
        // The four CSS Color 4 predefined spaces and the three XYZ names are
        // the specification's own identifiers, so they are not a choice. The
        GradientColorSpace::DisplayP3 => "display-p3",
        GradientColorSpace::Rec2020 => "rec2020",
        GradientColorSpace::ProphotoRgb => "prophoto-rgb",
        GradientColorSpace::A98Rgb => "a98-rgb",
        GradientColorSpace::Xyz => "xyz",
        GradientColorSpace::XyzD65 => "xyz-d65",
        GradientColorSpace::XyzD50 => "xyz-d50",
    }
}

fn str_to_color_space(s: &str) -> Option<GradientColorSpace> {
    let space = match s {
        "srgb" => GradientColorSpace::Srgb,
        "destination" => GradientColorSpace::Destination,
        "srgb-linear" => GradientColorSpace::SrgbLinear,
        "lab" => GradientColorSpace::Lab,
        "oklab" => GradientColorSpace::Oklab,
        "oklch" => GradientColorSpace::Oklch,
        "lch" => GradientColorSpace::Lch,
        "hsl" => GradientColorSpace::Hsl,
        "hwb" => GradientColorSpace::Hwb,
        // The write direction has to accept everything the read direction
        // can emit, or the property cannot round-trip through itself:
        // `g.interpolation = g.interpolation` would raise for any space the
        // getter names and the setter refuses. Closing `GradientColorSpace`
        // made `color_space_to_str` total and left this half at the eight it
        // had, which is how the two came apart.
        "display-p3" => GradientColorSpace::DisplayP3,
        "rec2020" => GradientColorSpace::Rec2020,
        "prophoto-rgb" => GradientColorSpace::ProphotoRgb,
        "a98-rgb" => GradientColorSpace::A98Rgb,
        "xyz" => GradientColorSpace::Xyz,
        "xyz-d65" => GradientColorSpace::XyzD65,
        "xyz-d50" => GradientColorSpace::XyzD50,
        _ => return None,
    };
    Some(space)
}

fn hue_method_to_str(hm: HueMethod) -> &'static str {
    match hm {
        HueMethod::Shorter => "shorter",
        HueMethod::Longer => "longer",
        HueMethod::Increasing => "increasing",
        HueMethod::Decreasing => "decreasing",
    }
}

fn str_to_hue_method(s: &str) -> Option<HueMethod> {
    let method = match s {
        "shorter" => HueMethod::Shorter,
        "longer" => HueMethod::Longer,
        "increasing" => HueMethod::Increasing,
        "decreasing" => HueMethod::Decreasing,
        _ => return None,
    };
    Some(method)
}

fn alpha_to_str(alpha: AlphaInterpolation) -> &'static str {
    match alpha {
        AlphaInterpolation::Unpremultiplied => "unpremultiplied",
        AlphaInterpolation::Premultiplied => "premultiplied",
    }
}

fn str_to_alpha(s: &str) -> Option<AlphaInterpolation> {
    let alpha = match s {
        "unpremultiplied" => AlphaInterpolation::Unpremultiplied,
        "premultiplied" => AlphaInterpolation::Premultiplied,
        _ => return None,
    };
    Some(alpha)
}

pub fn get_interpolation(mut cx: FunctionContext) -> JsResult<JsString> {
    let this = cx.argument::<BoxedCanvasGradient>(0)?;
    let this = this.borrow();
    Ok(cx.string(color_space_to_str(this.color_space)))
}

pub fn set_interpolation(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let this = cx.argument::<BoxedCanvasGradient>(0)?;
    let mut this = this.borrow_mut();
    let value = string_arg(&mut cx, 1, "interpolation")?;

    if let Some(cs) = str_to_color_space(&value) {
        this.color_space = cs;
    }

    Ok(cx.undefined())
}

pub fn get_hueInterpolation(mut cx: FunctionContext) -> JsResult<JsString> {
    let this = cx.argument::<BoxedCanvasGradient>(0)?;
    let this = this.borrow();
    Ok(cx.string(hue_method_to_str(this.hue_method)))
}

pub fn set_hueInterpolation(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let this = cx.argument::<BoxedCanvasGradient>(0)?;
    let mut this = this.borrow_mut();
    let value = string_arg(&mut cx, 1, "hueInterpolation")?;

    if let Some(hm) = str_to_hue_method(&value) {
        this.hue_method = hm;
    }

    Ok(cx.undefined())
}

pub fn get_alphaInterpolationMethod(
    mut cx: FunctionContext,
) -> JsResult<JsString> {
    let this = cx.argument::<BoxedCanvasGradient>(0)?;
    let this = this.borrow();
    Ok(cx.string(alpha_to_str(this.alpha)))
}

pub fn set_alphaInterpolationMethod(
    mut cx: FunctionContext,
) -> JsResult<JsUndefined> {
    let this = cx.argument::<BoxedCanvasGradient>(0)?;
    let mut this = this.borrow_mut();
    let value = string_arg(&mut cx, 1, "alphaInterpolationMethod")?;

    // Silent here and refused in `lib/classes/canvas.js`, as both siblings
    // are: the JavaScript setter holds the accepted list and raises a
    // `TypeError` before anything crosses, so a value reaching this line has
    // already been checked. Leaving it permissive rather than duplicating the
    // list keeps one validation site, which is the same reason the
    // deprecated spellings delegate rather than repeat.
    if let Some(alpha) = str_to_alpha(&value) {
        this.alpha = alpha;
    }

    Ok(cx.undefined())
}

#[cfg(test)]
mod tests {
    use super::*;
    // The list lives beside the enum it enumerates, so the two tests that
    // depend on it cannot drift from each other.
    use crate::shader::interpolation_space_tests::every_color_space;

    /// The name the getter hands out is a name the setter takes back.
    ///
    /// This is the invariant, and it is a property of the pair rather than of
    /// either function: `gradient.interpolation = gradient.interpolation`
    /// must not be able to throw for anything the getter can produce. The two
    /// tables came apart once already -- closing the enum made
    /// `color_space_to_str` total because the compiler demanded it and said
    /// nothing about `str_to_color_space`, leaving seven spaces emitted and
    /// refused. Asserting that the two lists match would test the symptom;
    /// this tests the thing a caller can observe.
    ///
    /// The two Skia gamut-mapped spaces are absent from the enum rather than
    /// present and unexposed, so there is nothing here for them to fail on --
    /// which is the right side of the line for them to sit on while they
    /// paint grey.
    #[test]
    fn a_color_space_round_trips_through_its_own_name() {
        for space in every_color_space() {
            let name = color_space_to_str(space);
            assert_eq!(
                str_to_color_space(name),
                Some(space),
                "the setter refuses {name:?}, which the getter emits"
            );
        }
    }

    /// And no two spaces share a name, which the round trip alone would not
    /// catch: two variants mapping to one string round-trip through whichever
    /// the setter names, and the other becomes unreachable while every
    /// assertion above still passes. `Srgb` and `Destination` were one
    /// variant until this release and briefly shared `"srgb"`.
    #[test]
    fn no_two_color_spaces_share_a_name() {
        let mut seen: Vec<&'static str> = vec![];
        for space in every_color_space() {
            let name = color_space_to_str(space);
            assert!(!seen.contains(&name), "{name:?} names two spaces");
            seen.push(name);
        }
    }

    fn opaque(offset: f32) -> (f32, Color4f) {
        (offset, Color4f::new(1.0, 0.0, 0.0, 1.0))
    }

    fn linear(start: Point, end: Point, stops: &[(f32, Color4f)]) -> Gradient {
        let mut ramp = Gradient::Linear {
            start,
            end,
            stops: vec![],
            colors: vec![],
        };
        for (offset, color) in stops {
            ramp.add_stop(*offset, *color);
        }
        ramp
    }

    fn radial(radii: (f32, f32), stops: &[(f32, Color4f)]) -> Gradient {
        let mut ramp = Gradient::Radial {
            start_point: Point::new(1.0, 1.0),
            start_radius: radii.0,
            end_point: Point::new(1.0, 1.0),
            end_radius: radii.1,
            stops: vec![],
            colors: vec![],
        };
        for (offset, color) in stops {
            ramp.add_stop(*offset, *color);
        }
        ramp
    }

    /// The invariant the erase path in `Context2D::draw_path` rests on.
    ///
    /// `is_opaque` is one of its guards, so a gradient that paints nothing
    /// while calling itself opaque erases the page and puts nothing back.
    /// Asserted in both directions: the ramps that do paint have to stay
    /// opaque, or the same fill stops taking a path it is entitled to.
    #[test]
    fn a_gradient_that_paints_nothing_is_never_opaque() {
        let point = Point::new(1.0, 1.0);
        let elsewhere = Point::new(4.0, 4.0);
        let both = [opaque(0.0), opaque(1.0)];

        let nothing = [
            ("no stops", linear(point, elsewhere, &[])),
            ("coincident endpoints", linear(point, point, &both)),
            ("same centre and radius", radial((3.0, 3.0), &both)),
        ];
        for (what, ramp) in nothing {
            assert!(ramp.paints_nothing(), "{what} should paint nothing");
            assert!(
                !wrap(ramp).is_opaque(),
                "{what} must not call itself opaque"
            );
        }

        let paints = [
            ("a real ramp", linear(point, elsewhere, &both)),
            ("a circle from a point", radial((0.0, 4.0), &both)),
        ];
        for (what, ramp) in paints {
            assert!(!ramp.paints_nothing(), "{what} should paint");
            assert!(wrap(ramp).is_opaque(), "{what} should stay opaque");
        }
    }

    /// The real `CanvasGradient`, so the assertions above run against
    /// `is_opaque` itself rather than against a second copy of its rule.
    fn wrap(gradient: Gradient) -> CanvasGradient {
        CanvasGradient {
            gradient: Rc::new(RefCell::new(gradient)),
            color_space: GradientColorSpace::default(),
            hue_method: HueMethod::default(),
            alpha: AlphaInterpolation::default(),
        }
    }
}
