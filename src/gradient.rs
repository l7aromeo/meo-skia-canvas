#![allow(non_snake_case)]
// `skia_safe::gradient_shader` is deprecated as of skia-safe 0.93;
// the new `skia_safe::gradient::shaders` module bundles colors /
// positions / interpolation into a `Gradient` struct. Migration is
// tracked separately; suppressing the deprecation here for now so
// the call sites compile clean.
#![allow(deprecated)]
use neon::prelude::*;
use skia_safe::{
    Color4f, Matrix, Point, Shader, TileMode,
    gradient_shader::{self, Interpolation, interpolation},
};
use std::{cell::RefCell, rc::Rc};

use crate::utils::*;

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
    color_space: interpolation::ColorSpace,
    hue_method: interpolation::HueMethod,
}

impl CanvasGradient {
    pub fn shader(&self) -> Option<Shader> {
        let interp = Interpolation {
            in_premul: interpolation::InPremul::No,
            color_space: self.color_space,
            hue_method: self.hue_method,
        };

        match &*self.gradient.borrow() {
            Gradient::Linear {
                start,
                end,
                stops,
                colors,
            } => gradient_shader::linear_with_interpolation(
                (*start, *end),
                (colors.as_slice(), None),
                Some(stops.as_slice()),
                TileMode::Clamp,
                interp,
                None,
            ),
            Gradient::Radial {
                start_point,
                start_radius,
                end_point,
                end_radius,
                stops,
                colors,
            } => gradient_shader::two_point_conical_with_interpolation(
                (*start_point, *start_radius),
                (*end_point, *end_radius),
                (colors.as_slice(), None),
                Some(stops.as_slice()),
                TileMode::Clamp,
                interp,
                None,
            ),
            Gradient::Conic {
                center,
                angle,
                stops,
                colors,
            } => {
                let Point { x, y } = *center;
                let mut rotated = Matrix::new_identity();
                rotated
                    .pre_translate((x, y))
                    .pre_rotate(*angle, None)
                    .pre_translate((-x, -y));

                gradient_shader::sweep_with_interpolation(
                    *center,
                    (colors.as_slice(), None),
                    Some(stops.as_slice()),
                    TileMode::Clamp,
                    None,
                    interp,
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
        !gradient.get_colors().iter().any(|c| c.a < 1.0)
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
        color_space: interpolation::ColorSpace::Destination,
        hue_method: interpolation::HueMethod::Shorter,
    };
    let this = RefCell::new(canvas_gradient);
    Ok(cx.boxed(this))
}

pub fn radial(mut cx: FunctionContext) -> JsResult<BoxedCanvasGradient> {
    let nums = &float_args(&mut cx, &["x1", "y1", "r1", "x2", "y2", "r2"])?[..6];
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
        color_space: interpolation::ColorSpace::Destination,
        hue_method: interpolation::HueMethod::Shorter,
    };
    let this = RefCell::new(canvas_gradient);
    Ok(cx.boxed(this))
}

pub fn conic(mut cx: FunctionContext) -> JsResult<BoxedCanvasGradient> {
    let nums = &float_args(&mut cx, &["theta", "x", "y"])?[..3];
    let [theta, x, y] = nums else { panic!() };

    let center = Point::new(*x, *y);
    let angle = to_degrees(*theta);
    let sweep = Gradient::Conic {
        center,
        angle,
        stops: vec![],
        colors: vec![],
    };
    let canvas_gradient = CanvasGradient {
        gradient: Rc::new(RefCell::new(sweep)),
        color_space: interpolation::ColorSpace::Destination,
        hue_method: interpolation::HueMethod::Shorter,
    };
    let this = RefCell::new(canvas_gradient);
    Ok(cx.boxed(this))
}

pub fn addColorStop(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let this = cx.argument::<BoxedCanvasGradient>(0)?;
    let mut this = this.borrow_mut();

    let offset = float_arg(&mut cx, 1, "offset")?;
    if !(0.0..=1.0).contains(&offset) {
        return cx.throw_range_error("Color stop offsets must be between 0.0 and 1.0");
    }

    // Accept either a CSS string (parsed as sRGB-gamma) or a
    // `[r, g, b, a]` premultiplied linear-light float array (the
    // `Color4fInput` shape mirroring `TextColorInput`). The stop
    // values flow into Skia's gradient interpolation as-is; callers
    // that need a non-default interpolation color space set it via
    // `gradient.interpolation`.
    let color_arg = cx.argument::<JsValue>(2)?;
    if let Some((color4f, _cs)) = color4f_in(&mut cx, color_arg) {
        this.add_color_stop(offset, color4f);
    } else {
        return cx.throw_type_error("Could not be parsed as a color");
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

fn color_space_to_str(cs: interpolation::ColorSpace) -> &'static str {
    use interpolation::ColorSpace;
    match cs {
        ColorSpace::SRGBLinear => "srgb-linear",
        ColorSpace::Lab => "lab",
        ColorSpace::OKLab => "oklab",
        ColorSpace::OKLCH => "oklch",
        ColorSpace::LCH => "lch",
        ColorSpace::HSL => "hsl",
        ColorSpace::HWB => "hwb",
        _ => "srgb",
    }
}

fn str_to_color_space(s: &str) -> Option<interpolation::ColorSpace> {
    use interpolation::ColorSpace;
    match s {
        "srgb" => Some(ColorSpace::Destination),
        "srgb-linear" => Some(ColorSpace::SRGBLinear),
        "lab" => Some(ColorSpace::Lab),
        "oklab" => Some(ColorSpace::OKLab),
        "oklch" => Some(ColorSpace::OKLCH),
        "lch" => Some(ColorSpace::LCH),
        "hsl" => Some(ColorSpace::HSL),
        "hwb" => Some(ColorSpace::HWB),
        _ => None,
    }
}

fn hue_method_to_str(hm: interpolation::HueMethod) -> &'static str {
    use interpolation::HueMethod;
    match hm {
        HueMethod::Shorter => "shorter",
        HueMethod::Longer => "longer",
        HueMethod::Increasing => "increasing",
        HueMethod::Decreasing => "decreasing",
    }
}

fn str_to_hue_method(s: &str) -> Option<interpolation::HueMethod> {
    use interpolation::HueMethod;
    match s {
        "shorter" => Some(HueMethod::Shorter),
        "longer" => Some(HueMethod::Longer),
        "increasing" => Some(HueMethod::Increasing),
        "decreasing" => Some(HueMethod::Decreasing),
        _ => None,
    }
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
