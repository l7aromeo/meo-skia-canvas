#![allow(non_snake_case)]
use neon::prelude::*;
use skia_safe::{
    BlendMode, Color, ColorFilter as SkColorFilter, color_filters,
};
use std::cell::RefCell;

use crate::utils::*;

pub type BoxedColorFilter = JsBox<RefCell<ColorFilter>>;
impl Finalize for ColorFilter {}

#[derive(Clone)]
pub struct ColorFilter {
    pub inner: SkColorFilter,
    deleted: bool,
}

impl ColorFilter {
    pub fn is_deleted(&self) -> bool {
        self.deleted
    }
}

/// Box a ColorFilter directly (for functions that always succeed)
macro_rules! box_color_filter {
    ($cx:expr, $filter:expr) => {{
        let cf = ColorFilter {
            inner: $filter,
            deleted: false,
        };
        Ok($cx.boxed(RefCell::new(cf)).upcast())
    }};
}

/// Wrap `Option<ColorFilter>`: `Some` -> boxed, `None` -> null.
macro_rules! wrap_color_filter {
    ($cx:expr, $result:expr) => {
        match $result {
            Some(filter) => box_color_filter!($cx, filter),
            None => Ok($cx.null().upcast()),
        }
    };
}

/// Parse BlendMode from string
fn parse_blend_mode(mode: &str) -> BlendMode {
    match mode.to_lowercase().as_str() {
        "clear" => BlendMode::Clear,
        "src" => BlendMode::Src,
        "dst" => BlendMode::Dst,
        "src-over" | "srcover" | "source-over" => BlendMode::SrcOver,
        "dst-over" | "dstover" | "destination-over" => BlendMode::DstOver,
        "src-in" | "srcin" | "source-in" => BlendMode::SrcIn,
        "dst-in" | "dstin" | "destination-in" => BlendMode::DstIn,
        "src-out" | "srcout" | "source-out" => BlendMode::SrcOut,
        "dst-out" | "dstout" | "destination-out" => BlendMode::DstOut,
        "src-atop" | "srcatop" | "source-atop" => BlendMode::SrcATop,
        "dst-atop" | "dstatop" | "destination-atop" => BlendMode::DstATop,
        "xor" => BlendMode::Xor,
        "plus" | "plus-lighter" => BlendMode::Plus,
        "modulate" => BlendMode::Modulate,
        "screen" => BlendMode::Screen,
        "overlay" => BlendMode::Overlay,
        "darken" => BlendMode::Darken,
        "lighten" => BlendMode::Lighten,
        "color-dodge" | "colordodge" => BlendMode::ColorDodge,
        "color-burn" | "colorburn" => BlendMode::ColorBurn,
        "hard-light" | "hardlight" => BlendMode::HardLight,
        "soft-light" | "softlight" => BlendMode::SoftLight,
        "difference" => BlendMode::Difference,
        "exclusion" => BlendMode::Exclusion,
        "multiply" => BlendMode::Multiply,
        "hue" => BlendMode::Hue,
        "saturation" => BlendMode::Saturation,
        "color" => BlendMode::Color,
        "luminosity" => BlendMode::Luminosity,
        _ => BlendMode::SrcOver,
    }
}

/// `ColorFilter.MakeMatrix(matrix: ArrayLike<number>)` -- 20 elements.
pub fn makeMatrix(mut cx: FunctionContext) -> JsResult<JsValue> {
    let matrix_vec = float_array_arg(&mut cx, 1, 20)?;

    // Validate no NaN/Infinity
    for (i, &val) in matrix_vec.iter().enumerate() {
        if !val.is_finite() {
            return cx.throw_type_error(format!(
                "Matrix element {} is not a finite number",
                i
            ));
        }
    }

    // Convert Vec to fixed-size array
    let matrix: [f32; 20] = matrix_vec
        .try_into()
        // SAFETY: `float_array_arg` validated the length is exactly 20.
        .expect("float_array_arg should have validated length");

    let filter = color_filters::matrix_row_major(&matrix, None);
    let cf = ColorFilter {
        inner: filter,
        deleted: false,
    };
    Ok(cx.boxed(RefCell::new(cf)).upcast())
}

/// ColorFilter.MakeSRGBToLinearGamma()
pub fn makeSRGBToLinearGamma(mut cx: FunctionContext) -> JsResult<JsValue> {
    box_color_filter!(cx, color_filters::srgb_to_linear_gamma())
}

/// ColorFilter.MakeLinearToSRGBGamma()
pub fn makeLinearToSRGBGamma(mut cx: FunctionContext) -> JsResult<JsValue> {
    box_color_filter!(cx, color_filters::linear_to_srgb_gamma())
}

/// ColorFilter.MakeBlend(color, mode) - blend with a solid color
pub fn makeBlend(mut cx: FunctionContext) -> JsResult<JsValue> {
    let color_str = cx.argument::<JsString>(1)?.value(&mut cx);
    let color: Color = css_to_color(&color_str).unwrap_or(Color::BLACK);

    let mode_str = cx.argument::<JsString>(2)?.value(&mut cx);
    let mode = parse_blend_mode(&mode_str);

    wrap_color_filter!(cx, color_filters::blend(color, mode))
}

/// ColorFilter.MakeCompose(outer, inner) - compose two ColorFilters
pub fn makeCompose(mut cx: FunctionContext) -> JsResult<JsValue> {
    let outer = cx.argument::<BoxedColorFilter>(1)?;
    let inner = cx.argument::<BoxedColorFilter>(2)?;
    checkDeleted(&mut cx, &outer)?;
    checkDeleted(&mut cx, &inner)?;

    wrap_color_filter!(
        cx,
        color_filters::compose(
            outer.borrow().inner.clone(),
            inner.borrow().inner.clone()
        )
    )
}

/// ColorFilter.MakeLerp(t, dst, src) - interpolate between two filters
pub fn makeLerp(mut cx: FunctionContext) -> JsResult<JsValue> {
    let t = cx.argument::<JsNumber>(1)?.value(&mut cx) as f32;
    let dst = cx.argument::<BoxedColorFilter>(2)?;
    let src = cx.argument::<BoxedColorFilter>(3)?;
    checkDeleted(&mut cx, &dst)?;
    checkDeleted(&mut cx, &src)?;

    wrap_color_filter!(
        cx,
        color_filters::lerp(
            t,
            dst.borrow().inner.clone(),
            src.borrow().inner.clone()
        )
    )
}

/// ColorFilter.MakeHSLAMatrix(matrix) - HSLA color matrix (20 elements)
pub fn makeHSLAMatrix(mut cx: FunctionContext) -> JsResult<JsValue> {
    let matrix_vec = float_array_arg(&mut cx, 1, 20)?;
    let matrix: [f32; 20] = matrix_vec
        .try_into()
        // SAFETY: `float_array_arg` validated the length is exactly 20.
        .expect("float_array_arg should have validated length");

    box_color_filter!(cx, color_filters::hsla_matrix(&matrix))
}

/// ColorFilter.MakeLighting(multiply, add) - lighting effect
pub fn makeLighting(mut cx: FunctionContext) -> JsResult<JsValue> {
    let mul_str = cx.argument::<JsString>(1)?.value(&mut cx);
    let add_str = cx.argument::<JsString>(2)?.value(&mut cx);
    let mul: Color = css_to_color(&mul_str).unwrap_or(Color::WHITE);
    let add: Color = css_to_color(&add_str).unwrap_or(Color::BLACK);

    wrap_color_filter!(cx, color_filters::lighting(mul, add))
}

/// ColorFilter.MakeLumaColorFilter() - extract luminance
pub fn makeLumaColorFilter(mut cx: FunctionContext) -> JsResult<JsValue> {
    // Luma filter is a specific matrix that extracts luminance
    // Y = 0.2126*R + 0.7152*G + 0.0722*B
    let luma_matrix: [f32; 20] = [
        0.0, 0.0, 0.0, 0.0, 0.0, // R output (black)
        0.0, 0.0, 0.0, 0.0, 0.0, // G output (black)
        0.0, 0.0, 0.0, 0.0, 0.0, // B output (black)
        0.2126, 0.7152, 0.0722, 0.0, 0.0, // A = luma
    ];
    box_color_filter!(cx, color_filters::matrix_row_major(&luma_matrix, None))
}

/// ColorFilter.MakeTable(table) - 256-element lookup table for all channels
pub fn makeTable(mut cx: FunctionContext) -> JsResult<JsValue> {
    let table_vec = u8_array_arg(&mut cx, 1, 256)?;
    let table: [u8; 256] = table_vec
        .try_into()
        // SAFETY: `u8_array_arg` validated the length is exactly 256.
        .expect("u8_array_arg should have validated length");

    wrap_color_filter!(cx, color_filters::table(&table))
}

/// ColorFilter.MakeTableARGB(a, r, g, b) - separate lookup tables per channel
pub fn makeTableARGB(mut cx: FunctionContext) -> JsResult<JsValue> {
    let a_vec = opt_u8_array_arg(&mut cx, 1, 256);
    let r_vec = opt_u8_array_arg(&mut cx, 2, 256);
    let g_vec = opt_u8_array_arg(&mut cx, 3, 256);
    let b_vec = opt_u8_array_arg(&mut cx, 4, 256);

    let a: Option<&[u8; 256]> =
        a_vec.as_ref().and_then(|v| v.as_slice().try_into().ok());
    let r: Option<&[u8; 256]> =
        r_vec.as_ref().and_then(|v| v.as_slice().try_into().ok());
    let g: Option<&[u8; 256]> =
        g_vec.as_ref().and_then(|v| v.as_slice().try_into().ok());
    let b: Option<&[u8; 256]> =
        b_vec.as_ref().and_then(|v| v.as_slice().try_into().ok());

    wrap_color_filter!(cx, color_filters::table_argb(a, r, g, b))
}

pub fn repr(mut cx: FunctionContext) -> JsResult<JsString> {
    let this = cx.argument::<BoxedColorFilter>(0)?;
    let label = if this.borrow().deleted {
        "deleted"
    } else {
        "Matrix"
    };
    Ok(cx.string(label))
}

pub fn delete(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let this = cx.argument::<BoxedColorFilter>(0)?;
    this.borrow_mut().deleted = true;
    Ok(cx.undefined())
}

pub fn checkDeleted(
    cx: &mut FunctionContext,
    filter: &BoxedColorFilter,
) -> NeonResult<()> {
    if filter.borrow().deleted {
        cx.throw_error("ColorFilter has been deleted")
    } else {
        Ok(())
    }
}
