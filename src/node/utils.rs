#![allow(dead_code)]
use core::ops::Range;
use neon::prelude::*;
use skia_safe::{Color, Color4f, ColorSpace, Data, Matrix, Path, Point, RGB};
use std::{cmp, f32::consts::PI};

//
// meta-helpers
//

fn arg_num(o: usize) -> String {
    // let n = (o + 1) as i32; // we're working with zero-bounded idxs
    let n = o; // arg 0 is always self, so no need to increment the idx
    let ords = ["st", "nd", "rd"];
    let slot = ((n + 90) % 100 - 10) % 10 - 1;
    let suffix = if (0..=2).contains(&slot) {
        ords[slot]
    } else {
        "th"
    };
    format!("{}{}", n, suffix)
}

// pub fn argv<'a>() -> Vec<Handle<'a, JsValue>>{
//   let list:Vec<Handle<JsValue>> = Vec::new();
//   list
// }

// pub fn clamp(val: f32, min:f64, max:f64) -> f32{
//   let min = min as f32;
//   let max = max as f32;
//   if val < min { min } else if val > max { max } else { val }
// }

pub fn almost_equal(a: f32, b: f32) -> bool {
    (a - b).abs() < 0.00001
}

pub fn almost_zero(a: f32) -> bool {
    a.abs() < 0.00001
}

pub fn to_degrees(radians: f32) -> f32 {
    radians / PI * 180.0
}

pub fn to_radians(degrees: f32) -> f32 {
    degrees / 180.0 * PI
}

/// Parses fixed-length float array from JS (copies input).
/// Supports `Float32Array`, regular arrays, and `ArrayLike` objects.
pub fn float_array_arg(
    cx: &mut FunctionContext,
    idx: usize,
    len: usize,
) -> NeonResult<Vec<f32>> {
    let arg = cx.argument::<JsValue>(idx)?;

    // Try as array first (most common case for JS)
    if let Ok(array) = arg.downcast::<JsArray, _>(cx) {
        let array_len = array.len(cx) as usize;
        if array_len != len {
            return cx.throw_range_error(format!(
                "Expected {} elements, got {}",
                len, array_len
            ));
        }
        let mut result = Vec::with_capacity(len);
        for i in 0..len {
            let val: Handle<JsValue> = array.get(cx, i as u32)?;
            if let Ok(num) = val.downcast::<JsNumber, _>(cx) {
                result.push(num.value(cx) as f32);
            } else {
                return cx.throw_type_error(format!(
                    "Element {} is not a number",
                    i
                ));
            }
        }
        return Ok(result);
    }

    // Try ArrayLike (has .length property)
    if let Ok(obj) = arg.downcast::<JsObject, _>(cx)
        && let Ok(length_val) = obj.get::<JsValue, _, _>(cx, "length")
        && let Ok(length) = length_val.downcast::<JsNumber, _>(cx)
    {
        let len_num = length.value(cx) as usize;
        if len_num != len {
            return cx.throw_range_error(format!(
                "Expected {} elements, got {}",
                len, len_num
            ));
        }
        let mut result = Vec::with_capacity(len);
        for i in 0..len {
            let val: Handle<JsValue> = obj.get(cx, i as u32)?;
            if let Ok(num) = val.downcast::<JsNumber, _>(cx) {
                result.push(num.value(cx) as f32);
            } else {
                return cx.throw_type_error(format!(
                    "Element {} is not a number",
                    i
                ));
            }
        }
        return Ok(result);
    }

    cx.throw_type_error(format!(
        "Expected array or ArrayLike with {} numbers",
        len
    ))
}

pub fn u8_array_arg(
    cx: &mut FunctionContext,
    idx: usize,
    len: usize,
) -> NeonResult<Vec<u8>> {
    let arg = cx.argument::<JsValue>(idx)?;

    if let Ok(array) = arg.downcast::<JsArray, _>(cx) {
        let array_len = array.len(cx) as usize;
        if array_len != len {
            return cx.throw_range_error(format!(
                "Expected {} elements, got {}",
                len, array_len
            ));
        }
        let mut result = Vec::with_capacity(len);
        for i in 0..len {
            let val: Handle<JsValue> = array.get(cx, i as u32)?;
            if let Ok(num) = val.downcast::<JsNumber, _>(cx) {
                result.push(num.value(cx) as u8);
            } else {
                return cx.throw_type_error(format!(
                    "Element {} is not a number",
                    i
                ));
            }
        }
        return Ok(result);
    }

    cx.throw_type_error(format!("Expected array with {} elements", len))
}

pub fn opt_u8_array_arg(
    cx: &mut FunctionContext,
    idx: usize,
    len: usize,
) -> Option<Vec<u8>> {
    let arg = cx.argument_opt(idx)?;

    if arg.is_a::<JsNull, _>(cx) || arg.is_a::<JsUndefined, _>(cx) {
        return None;
    }

    if let Ok(array) = arg.downcast::<JsArray, _>(cx) {
        let array_len = array.len(cx) as usize;
        if array_len != len {
            return None;
        }
        let mut result = Vec::with_capacity(len);
        for i in 0..len {
            if let Ok(val) = array.get::<JsValue, _, _>(cx, i as u32) {
                if let Ok(num) = val.downcast::<JsNumber, _>(cx) {
                    result.push(num.value(cx) as u8);
                } else {
                    return None;
                }
            } else {
                return None;
            }
        }
        return Some(result);
    }

    None
}

pub fn check_argc(cx: &mut FunctionContext, argc: usize) -> NeonResult<()> {
    match cx.len() >= argc {
        true => Ok(()),
        false => cx.throw_type_error("not enough arguments"),
    }
}

// pub fn symbol<'a>(cx: &mut FunctionContext<'a>, symbol_name: &str) ->
// JsResult<'a, JsValue> {   let global = cx.global();
//   let symbol_ctor = global
//       .get(cx, "Symbol")?
//       .downcast::<JsObject, _>(cx)
//       .or_throw(cx)?
//       .get(cx, "for")?
//       .downcast::<JsFunction, _>(cx)
//       .or_throw(cx)?;

//   let symbol_label = cx.string(symbol_name);
//   let sym = symbol_ctor.call(cx, global, vec![symbol_label])?;
//   Ok(sym)
// }

//
// plain objects
//

pub fn opt_object_arg<'a>(
    cx: &mut FunctionContext<'a>,
    idx: usize,
) -> Option<Handle<'a, JsObject>> {
    match cx.argument_opt(idx) {
        Some(arg) => arg.downcast::<JsObject, _>(cx).ok(),
        None => None,
    }
}

pub fn object_arg<'a>(
    cx: &mut FunctionContext<'a>,
    idx: usize,
    attr: &str,
) -> NeonResult<Handle<'a, JsObject>> {
    match opt_object_arg(cx, idx) {
        Some(val) => Ok(val),
        None => {
            cx.throw_type_error(format!("Exptected an object for \"{}\"", attr))
        }
    }
}

pub fn opt_object_for_key<'a>(
    cx: &mut FunctionContext<'a>,
    obj: &Handle<'a, JsObject>,
    attr: &str,
) -> Option<Handle<'a, JsObject>> {
    if let Ok(val) = obj.get::<JsValue, _, _>(cx, attr) {
        return val.downcast::<JsObject, _>(cx).ok();
    }
    None
}

pub fn object_for_key<'a>(
    cx: &mut FunctionContext<'a>,
    obj: &Handle<'a, JsObject>,
    attr: &str,
) -> NeonResult<Handle<'a, JsObject>> {
    match opt_object_for_key(cx, obj, attr) {
        Some(val) => Ok(val),
        None => {
            cx.throw_type_error(format!("Exptected an object for \"{}\"", attr))
        }
    }
}

//
// strings
//

pub fn strings_in(
    cx: &mut FunctionContext,
    vals: &[Handle<JsValue>],
) -> Vec<String> {
    let mut strs: Vec<String> = Vec::new();
    for val in vals.iter() {
        if let Ok(txt) = val.downcast::<JsString, _>(cx) {
            let val = txt.value(cx);
            strs.push(val);
        }
    }
    strs
}

pub fn strings_at_key(
    cx: &mut FunctionContext,
    obj: &Handle<JsObject>,
    attr: &str,
) -> NeonResult<Vec<String>> {
    let array: Handle<JsArray> = obj.get(cx, attr)?;
    let list = array.to_vec(cx)?;
    Ok(strings_in(cx, &list))
}

pub fn opt_string_for_key(
    cx: &mut FunctionContext,
    obj: &Handle<JsObject>,
    attr: &str,
) -> Option<String> {
    obj.get(cx, attr)
        .ok()
        .and_then(|val: Handle<JsValue>| val.downcast::<JsString, _>(cx).ok())
        .map(|v| v.value(cx))
}

pub fn opt_bool_for_key(
    cx: &mut FunctionContext,
    obj: &Handle<JsObject>,
    attr: &str,
) -> Option<bool> {
    obj.get(cx, attr)
        .ok()
        .and_then(|val: Handle<JsValue>| val.downcast::<JsBoolean, _>(cx).ok())
        .map(|v| v.value(cx))
}

pub fn string_for_key(
    cx: &mut FunctionContext,
    obj: &Handle<JsObject>,
    attr: &str,
) -> NeonResult<String> {
    let key = cx.string(attr);
    let val: Handle<JsValue> = obj.get(cx, key)?;
    match val.downcast::<JsString, _>(cx) {
        Ok(s) => Ok(s.value(cx)),
        Err(_e) => {
            cx.throw_type_error(format!("Exptected a string for \"{}\"", attr))
        }
    }
}

pub fn opt_string_arg(cx: &mut FunctionContext, idx: usize) -> Option<String> {
    match cx.argument_opt(idx) {
        Some(arg) => match arg.downcast::<JsString, _>(cx) {
            Ok(v) => Some(v.value(cx)),
            Err(_e) => None,
        },
        None => None,
    }
}

pub fn string_arg_or(
    cx: &mut FunctionContext,
    idx: usize,
    default: &str,
) -> String {
    match opt_string_arg(cx, idx) {
        Some(v) => v,
        None => String::from(default),
    }
}

/// Renders a table's accepted names for an error message.
fn known_names<T>(table: &[(&str, T)]) -> String {
    table
        .iter()
        .map(|(name, _)| format!("\"{name}\""))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Parses a string argument against a table of accepted names, falling back to
/// `default` only when the argument is genuinely absent.
///
/// An omitted optional argument takes the default; a supplied one that is not
/// in the table throws, which is WebIDL's rule for an invalid enum in a method
/// argument and the same choice [`color_space_or_throw`] makes. Substituting a
/// default for a misspelling made the declared string unions in
/// `lib/index.d.ts` a lie -- `MaskFilter.MakeBlur("bogus", 4)` returned a
/// normal blur, and `ColorFilter.MakeBlend("colorDodge", ...)` composited
/// source-over.
///
/// The argument is folded to lowercase before lookup, so every entry in
/// `table` has to be lowercase or it can never be reached.
///
/// # Errors
///
/// Throws a `TypeError` if the argument is present but is not a string, or is
/// a string the table does not list.
pub fn enum_arg_or<T: Copy>(
    cx: &mut FunctionContext,
    idx: usize,
    attr: &str,
    table: &[(&str, T)],
    default: T,
) -> NeonResult<T> {
    // Names are compared after folding, so an entry carrying a capital could
    // never match -- which is exactly how the camel case blend names went
    // unreachable for as long as they were advertised.
    debug_assert!(
        table.iter().all(|(name, _)| *name == name.to_lowercase()),
        "`{attr}` has a table entry that is not lowercase, so it can never match"
    );

    let Some(arg) = cx.argument_opt(idx) else {
        return Ok(default);
    };
    if arg.is_a::<JsNull, _>(cx) || arg.is_a::<JsUndefined, _>(cx) {
        return Ok(default);
    }

    let Ok(string) = arg.downcast::<JsString, _>(cx) else {
        return cx.throw_type_error(format!(
            "Expected a string for `{attr}` (expected one of: {})",
            known_names(table)
        ));
    };

    // Reported with the caller's own casing: echoing the lowercased form back
    // makes the message look like it is describing a different value.
    let name = string.value(cx);
    let folded = name.to_lowercase();
    match table.iter().find(|(known, _)| *known == folded) {
        Some((_, value)) => Ok(*value),
        None => cx.throw_type_error(format!(
            "Unknown {attr} \"{name}\" (expected one of: {})",
            known_names(table)
        )),
    }
}

pub fn string_arg(
    cx: &mut FunctionContext,
    idx: usize,
    attr: &str,
) -> NeonResult<String> {
    let exists = cx.len() > idx;
    match opt_string_arg(cx, idx) {
        Some(v) => Ok(v),
        None => cx.throw_type_error(if exists {
            format!("Expected a string for `{}`", attr)
        } else {
            format!(
                "not enough arguments: expected a string for `{}` as {} arg",
                attr,
                arg_num(idx)
            )
        }),
    }
}

pub fn strings_to_array<'a>(
    cx: &mut FunctionContext<'a>,
    strings: &[String],
) -> JsResult<'a, JsArray> {
    let array = JsArray::new(cx, strings.len());
    for (i, val) in strings.iter().enumerate() {
        let num = cx.string(val.as_str());
        array.set(cx, i as u32, num)?;
    }
    Ok(array)
}

//
// bools
//

pub fn opt_bool_arg(cx: &mut FunctionContext, idx: usize) -> Option<bool> {
    match cx.argument_opt(idx) {
        Some(arg) => match arg.downcast::<JsBoolean, _>(cx) {
            Ok(v) => Some(v.value(cx)),
            Err(_e) => None,
        },
        None => None,
    }
}

pub fn bool_arg_or(
    cx: &mut FunctionContext,
    idx: usize,
    default: bool,
) -> bool {
    match opt_bool_arg(cx, idx) {
        Some(v) => v,
        None => default,
    }
}

pub fn bool_arg(
    cx: &mut FunctionContext,
    idx: usize,
    attr: &str,
) -> NeonResult<bool> {
    let exists = cx.len() > idx;
    match opt_bool_arg(cx, idx) {
        Some(v) => Ok(v),
        None => cx.throw_type_error(if exists {
            format!("{} must be a boolean", attr)
        } else {
            format!(
                "not enough arguments: expected a boolean for `{}` as {} arg",
                attr,
                arg_num(idx)
            )
        }),
    }
}

pub fn bool_for_key(
    cx: &mut FunctionContext,
    obj: &Handle<JsObject>,
    attr: &str,
) -> NeonResult<bool> {
    let key = cx.string(attr);
    let val: Handle<JsValue> = obj.get(cx, key)?;
    match val.downcast::<JsBoolean, _>(cx) {
        Ok(v) => Ok(v.value(cx)),
        Err(_e) => cx.throw_type_error(format!(
            "Exptected a boolean value for \"{}\"",
            attr
        )),
    }
}

//
// floats
//

fn _as_double(cx: &mut FunctionContext, val: &Handle<JsValue>) -> Option<f64> {
    // emulate (some of) javascript's wildly permissive type coercion <https://www.w3schools.com/js/js_type_conversion.asp>
    val.downcast::<JsNumber, _>(cx)
        .ok()
        .map(|num| num.value(cx))
        .or_else(|| {
            // strings
            val.downcast::<JsString, _>(cx).ok().and_then(|txt| {
                let s = txt.value(cx);
                if let Some(s) = s.strip_prefix("0x") {
                    u64::from_str_radix(s, 16).map(|i| i as f64).ok()
                } else if let Some(s) = s.strip_prefix("0o") {
                    u64::from_str_radix(s, 8).map(|i| i as f64).ok()
                } else if let Some(s) = s.strip_prefix("0b") {
                    u64::from_str_radix(s, 2).map(|i| i as f64).ok()
                } else if s.is_empty() {
                    Some(0.0)
                } else {
                    s.parse::<f64>().ok()
                }
            })
        })
        .or_else(|| {
            // booleans
            val.downcast::<JsBoolean, _>(cx)
                .ok()
                .map(|b| match b.value(cx) {
                    true => 1.0,
                    false => 0.0,
                })
        })
        .or_else(|| {
            // null
            val.downcast::<JsNull, _>(cx).ok().map(|_| 0.0)
        })
        .or_else(|| {
            // arrays
            val.downcast::<JsArray, _>(cx).ok().and_then(|array| {
                match array.len(cx) {
                    0 => Some(0.0),
                    1 => array
                        .to_vec(cx)
                        .ok()
                        .and_then(|nums| _as_double(cx, &nums[0])),
                    _ => None,
                }
            })
        })
        .and_then(|num| match num.is_finite() {
            true => Some(num),
            false => None,
        })
}

fn _as_float(cx: &mut FunctionContext, val: &Handle<JsValue>) -> Option<f32> {
    _as_double(cx, val).map(|num| num as f32)
}

pub fn _float_args_at(
    cx: &mut FunctionContext,
    start: usize,
    names: &[&str],
    or_bail: bool,
) -> NeonResult<Vec<f32>> {
    let argc = cx.len() - start; // args start after the `this` reference
    if argc < names.len() {
        return cx.throw_type_error(format!(
            "not enough arguments (missing: {})",
            names[argc..].join(", ")
        ));
    }

    // emoji indicates that it will only throw in strict mode
    let prefix = if or_bail { "⚠️" } else { "" };

    let mut args: Vec<f32> = Vec::new();
    for (i, name) in names.iter().enumerate() {
        match opt_float_arg(cx, i + start) {
            Some(v) => args.push(v),
            None => {
                return cx.throw_type_error(format!(
                    "{}Expected a number for `{}` as {} arg",
                    prefix,
                    name,
                    arg_num(i + start)
                ));
            }
        }
    }

    Ok(args)
}

pub fn opt_double_for_key(
    cx: &mut FunctionContext,
    obj: &Handle<JsObject>,
    attr: &str,
) -> Option<f64> {
    obj.get(cx, attr).ok().and_then(|val| _as_double(cx, &val))
}

pub fn opt_float_for_key(
    cx: &mut FunctionContext,
    obj: &Handle<JsObject>,
    attr: &str,
) -> Option<f32> {
    obj.get(cx, attr).ok().and_then(|val| _as_float(cx, &val))
}

pub fn float_for_key(
    cx: &mut FunctionContext,
    obj: &Handle<JsObject>,
    attr: &str,
) -> NeonResult<f32> {
    match opt_float_for_key(cx, obj, attr) {
        Some(num) => Ok(num),
        None => cx.throw_type_error(format!(
            "Exptected a numerical value for \"{}\"",
            attr
        )),
    }
}

pub fn floats_in(
    cx: &mut FunctionContext,
    vals: &[Handle<JsValue>],
) -> Vec<f32> {
    vals.iter()
        .filter_map(|val| _as_float(cx, val))
        .collect::<Vec<f32>>()
}

pub fn opt_float_arg(cx: &mut FunctionContext, idx: usize) -> Option<f32> {
    cx.argument_opt(idx).and_then(|val| _as_float(cx, &val))
}

pub fn float_arg_or(cx: &mut FunctionContext, idx: usize, default: f32) -> f32 {
    match opt_float_arg(cx, idx) {
        Some(v) => v,
        None => default,
    }
}

pub fn float_arg(
    cx: &mut FunctionContext,
    idx: usize,
    attr: &str,
) -> NeonResult<f32> {
    // SAFETY: `_float_args_at` with a single-element slice always returns a
    // single-element vec.
    _float_args_at(cx, idx, &[attr], false)
        .map(|vec| vec.into_iter().next().unwrap())
}

/// As [`float_arg_or_bail`], keeping the full `double` JavaScript gave.
///
/// Canvas attributes are `double` in the IDL. Coercing to `f32` at the
/// boundary is right where the value ends up in a Skia paint, and wrong where
/// it is stored and read back: `0.37` came back out as `0.3700000047683716`.
pub fn double_arg_or_bail(
    cx: &mut FunctionContext,
    idx: usize,
    attr: &str,
) -> NeonResult<f64> {
    match cx.argument_opt(idx).and_then(|val| _as_double(cx, &val)) {
        Some(num) => Ok(num),
        None => cx.throw_type_error(format!(
            "⚠️Expected a number for `{attr}` as {} arg",
            arg_num(idx)
        )),
    }
}

pub fn float_arg_or_bail(
    cx: &mut FunctionContext,
    idx: usize,
    attr: &str,
) -> NeonResult<f32> {
    // SAFETY: `_float_args_at` with a single-element slice always returns a
    // single-element vec.
    _float_args_at(cx, idx, &[attr], true)
        .map(|vec| vec.into_iter().next().unwrap())
}

pub fn floats_to_array<'a>(
    cx: &mut FunctionContext<'a>,
    nums: &[f32],
) -> JsResult<'a, JsValue> {
    let array = JsArray::new(cx, nums.len());
    for (i, val) in nums.iter().enumerate() {
        let num = cx.number(*val);
        array.set(cx, i as u32, num)?;
    }
    Ok(array.upcast())
}

//
// float spreads
//

pub fn opt_float_args(cx: &mut FunctionContext, rng: Range<usize>) -> Vec<f32> {
    let end = cmp::min(rng.end, cx.len());
    let rng = rng.start..end;

    let mut args: Vec<f32> = Vec::new();
    for i in rng.start..end {
        if let Some(val) = opt_float_arg(cx, i) {
            args.push(val);
        }
    }
    args
}

pub fn float_args(
    cx: &mut FunctionContext,
    names: &[&str],
) -> NeonResult<Vec<f32>> {
    _float_args_at(cx, 1, names, false)
}

pub fn float_args_at(
    cx: &mut FunctionContext,
    start: usize,
    names: &[&str],
) -> NeonResult<Vec<f32>> {
    _float_args_at(cx, start, names, false)
}

pub fn float_args_or_bail(
    cx: &mut FunctionContext,
    names: &[&str],
) -> NeonResult<Vec<f32>> {
    _float_args_at(cx, 1, names, true)
}

pub fn float_args_or_bail_at(
    cx: &mut FunctionContext,
    start: usize,
    names: &[&str],
) -> NeonResult<Vec<f32>> {
    _float_args_at(cx, start, names, true)
}

//
// Colors
//

/// The sRGB transfer function, extended through zero.
///
/// CSS Color 4 defines these for negative inputs by mirroring, which is what
/// keeps a colour outside the sRGB gamut representable as sRGB components.
fn srgb_encode(linear: f32) -> f32 {
    let magnitude = linear.abs();
    let encoded = match magnitude <= 0.003_130_8 {
        true => magnitude * 12.92,
        false => 1.055 * magnitude.powf(1.0 / 2.4) - 0.055,
    };
    encoded.copysign(linear)
}

fn srgb_decode(encoded: f32) -> f32 {
    let magnitude = encoded.abs();
    let linear = match magnitude <= 0.040_45 {
        true => magnitude / 12.92,
        false => ((magnitude + 0.055) / 1.055).powf(2.4),
    };
    linear.copysign(encoded)
}

/// Rec. 2020's transfer function, extended the same way.
fn rec2020_decode(encoded: f32) -> f32 {
    const ALPHA: f32 = 1.099_296_8;
    const BETA: f32 = 0.018_053_97;
    let magnitude = encoded.abs();
    let linear = match magnitude < BETA * 4.5 {
        true => magnitude / 4.5,
        false => ((magnitude + ALPHA - 1.0) / ALPHA).powf(1.0 / 0.45),
    };
    linear.copysign(encoded)
}

/// Parses CSS Color 4's `color(<space> r g b[ / a])`.
///
/// Handled here rather than by `csscolorparser`, which does not implement the
/// function. Components are converted into extended sRGB -- the one space
/// every later stage already speaks -- rather than carried with a tag, so a
/// colour keeps a single spelling from the parser to the readback.
fn parse_color_function(css: &str) -> Option<(Color4f, &'static str)> {
    let inner = css.trim().strip_prefix("color(")?.strip_suffix(')')?;
    let (space, components) = inner.trim().split_once(char::is_whitespace)?;

    let (components, alpha) = match components.split_once('/') {
        Some((rgb, a)) => (rgb, a.trim().parse::<f32>().ok()?),
        None => (components, 1.0),
    };
    let parts: Vec<f32> = components
        .split_whitespace()
        .map(|n| match n.strip_suffix('%') {
            Some(pct) => pct.parse::<f32>().map(|v| v / 100.0),
            None => n.parse::<f32>(),
        })
        .collect::<Result<_, _>>()
        .ok()?;
    let [r, g, b] = parts[..] else { return None };

    // Into linear light in the named space, through the matrix that takes it
    // to linear sRGB, then back out through the sRGB transfer function. The
    // matrices are the CSS Color 4 conversions, applied in linear light.
    let linear = |f: fn(f32) -> f32| [f(r), f(g), f(b)];
    let apply = |m: [[f32; 3]; 3], v: [f32; 3]| {
        [
            m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
            m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
            m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
        ]
    };

    let _ = (&linear, &apply);
    // Borrowed from the input otherwise, which outlives nothing.
    let name = match space {
        "srgb" => "srgb",
        "srgb-linear" => "srgb-linear",
        "display-p3" => "display-p3",
        "rec2020" => "rec2020",
        _ => return None,
    };
    Some((Color4f::new(r, g, b, alpha), name))
}

/// Converts `color()` components into extended sRGB.
///
/// Only for reporting: the paint keeps the colour in the space it was named
/// in, so a P3 colour on a P3 canvas needs no conversion at all. Serialising
/// it does, and a round trip through sRGB costs about a level of the eight
/// the readback quantises to.
fn color_function_to_srgb(color: Color4f, space: &str) -> Color4f {
    let apply = |m: [[f32; 3]; 3], v: [f32; 3]| {
        [
            m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
            m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
            m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
        ]
    };
    let linear = |f: fn(f32) -> f32| [f(color.r), f(color.g), f(color.b)];

    // The CSS Color 4 conversions, applied in linear light.
    let [r, g, b] = match space {
        "srgb" => return color,
        "srgb-linear" => [
            srgb_encode(color.r),
            srgb_encode(color.g),
            srgb_encode(color.b),
        ],
        "display-p3" => {
            const TO_SRGB: [[f32; 3]; 3] = [
                [1.224_940_1, -0.224_940_4, 0.0],
                [-0.042_056_9, 1.042_057_1, 0.0],
                [-0.019_637_6, -0.078_636_1, 1.098_273_5],
            ];
            apply(TO_SRGB, linear(srgb_decode)).map(srgb_encode)
        }
        "rec2020" => {
            const TO_SRGB: [[f32; 3]; 3] = [
                [1.660_5, -0.587_6, -0.072_8],
                [-0.124_6, 1.132_9, -0.008_3],
                [-0.018_2, -0.100_6, 1.118_7],
            ];
            apply(TO_SRGB, linear(rec2020_decode)).map(srgb_encode)
        }
        _ => return color,
    };
    Color4f::new(r, g, b, color.a)
}

/// The name of a color space this crate can build, by comparing against each.
///
/// Skia keeps no record of which CICP pair made a `ColorSpace`, so the only
/// way back to a name is to ask whether it equals one built from that name.
pub fn color_space_name(space: &ColorSpace) -> Option<&'static str> {
    ["srgb", "srgb-linear", "display-p3", "rec2020"]
        .into_iter()
        .find(|name| opt_color_space(name).as_ref() == Some(space))
}

/// Parses a CSS color into extended-sRGB components, unclamped.
///
/// Extended, because CSS Color 4 can name colors outside the sRGB gamut --
/// `oklch(0.7 0.35 30)` comes back with a red above one and a green below
/// zero. Clamping here would throw that away before the surface it is
/// destined for ever sees it, and a wide surface is exactly where it means
/// something.
pub fn css_to_color4f(css: &str) -> Option<Color4f> {
    let (color, space) = css_to_color4f_in_space(css)?;
    match color_space_name(&space) {
        Some(name) => Some(color_function_to_srgb(color, name)),
        None => Some(color),
    }
}

/// Parses a CSS color, keeping the space it was named in.
///
/// `color(display-p3 1 0 0)` on a Display P3 canvas is then an identity --
/// Skia converts at the paint, and converting via sRGB first would cost a
/// level of the eight the readback quantises to.
pub fn css_to_color4f_in_space(css: &str) -> Option<(Color4f, ColorSpace)> {
    if let Some((color, space)) = parse_color_function(css) {
        // SAFETY: `parse_color_function` only returns names this builds.
        return Some((color, opt_color_space(space)?));
    }
    csscolorparser::parse(css)
        .ok()
        .map(|c| (Color4f::new(c.r, c.g, c.b, c.a), ColorSpace::new_srgb()))
}

/// Parses a CSS color and quantises it to 8-bit sRGB.
///
/// For the callers that hand Skia an `SkColor` -- window backgrounds, mattes,
/// texture marks. Anything drawn through a paint should take
/// [`css_to_color4f`] instead and keep the precision.
pub fn css_to_color(css: &str) -> Option<Color> {
    css_to_color4f(css).map(|c| {
        let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        Color::from_argb(byte(c.a), byte(c.r), byte(c.g), byte(c.b))
    })
}

pub fn color_in<'a>(
    cx: &mut FunctionContext<'a>,
    val: Handle<'a, JsValue>,
) -> Option<Color> {
    let (color, space) = color4f_string_in(cx, val)?;
    let color = match color_space_name(&space) {
        Some(name) => color_function_to_srgb(color, name),
        None => color,
    };
    let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    Some(Color::from_argb(
        byte(color.a),
        byte(color.r),
        byte(color.g),
        byte(color.b),
    ))
}

/// The CSS-string half of a color argument, kept in floating point.
///
/// A value outside the sRGB gamut -- which `oklch()` and `lab()` can name --
/// only survives if it is never quantised on the way in. The 8-bit form is
/// [`color_in`], for the callers that need an `SkColor`.
pub fn color4f_string_in<'a>(
    cx: &mut FunctionContext<'a>,
    val: Handle<'a, JsValue>,
) -> Option<(Color4f, ColorSpace)> {
    if val.is_a::<JsString, _>(cx) {
        // SAFETY: `is_a::<JsString>` check passed above.
        let css = val.downcast::<JsString, _>(cx).unwrap().value(cx);
        css_to_color4f_in_space(&css)
    } else {
        // for other objects, try calling their .toString() method (if it
        // exists)
        let obj = val.downcast::<JsObject, _>(cx).ok()?;
        let attr = obj.get::<JsValue, _, _>(cx, "toString").ok()?;
        let to_string = attr.downcast::<JsFunction, _>(cx).ok()?;
        let result = to_string.call(cx, obj, vec![]).ok()?;
        let css = result.downcast::<JsString, _>(cx).ok()?.value(cx);
        css_to_color4f_in_space(&css)
    }
}

/// Parses a color value that may be a CSS string or a `[r, g, b, a]` float
/// array. Returns `(Color4f, Option<ColorSpace>)` where:
/// - Float array: linear-light unpremultiplied float values, returned with
///   `None` for the color space tag. Callers decide how to tag them -- the
///   paragraph text path tags as `srgb_linear` (Skia converts to the
///   destination working space at paint time), the canvas
///   `fillStyle`/`strokeStyle` path tags with the canvas's working color space
///   directly.
/// - CSS string: values are in sRGB gamma, returned with `Some(sRGB)`.
pub fn color4f_in<'a>(
    cx: &mut FunctionContext<'a>,
    val: Handle<'a, JsValue>,
) -> Option<(Color4f, Option<ColorSpace>)> {
    // An array of four numbers is the float-color form. Anything else that
    // happens to be an array falls through to the string path rather than
    // being rejected: an Array then reaches the JsObject arm and is parsed
    // as CSS via its `toString()`, so `['red']` sets red. Callers rely on
    // that, so it is a compatibility guarantee rather than a quirk to be
    // tidied away -- do not start rejecting malformed arrays here.
    if let Some(rgba) = opt_color4f_from_array(cx, val) {
        return Some((rgba, None));
    }
    // Unquantised: `Color4f::from` an 8-bit `Color` clipped every colour the
    // CSS Color 4 functions can name outside sRGB, before the surface it was
    // bound for ever saw it.
    let (color, space) = color4f_string_in(cx, val)?;
    Some((color, Some(space)))
}

fn opt_color4f_from_array<'a>(
    cx: &mut FunctionContext<'a>,
    val: Handle<'a, JsValue>,
) -> Option<Color4f> {
    let arr = val.downcast::<JsArray, _>(cx).ok()?;
    if arr.len(cx) < 4 {
        return None;
    }
    // Use get_value + downcast to avoid throwing on non-number elements.
    let rv = arr.get_value(cx, 0).ok()?;
    let gv = arr.get_value(cx, 1).ok()?;
    let bv = arr.get_value(cx, 2).ok()?;
    let av = arr.get_value(cx, 3).ok()?;
    let r = rv.downcast::<JsNumber, _>(cx).ok()?.value(cx) as f32;
    let g = gv.downcast::<JsNumber, _>(cx).ok()?.value(cx) as f32;
    let b = bv.downcast::<JsNumber, _>(cx).ok()?.value(cx) as f32;
    let a = av.downcast::<JsNumber, _>(cx).ok()?.value(cx) as f32;
    Some(Color4f::new(r, g, b, a))
}

/// Quantize an `unpremultiplied`, linear-light `Color4f` (e.g. from a
/// `[r, g, b, a]` float array) to a Skia `Color` (u32 ARGB, sRGB-encoded by
/// Skia convention).
///
/// Skia APIs that take a `Color` -- `TextStyle::set_decoration_color`,
/// `TextShadow::new`, etc. -- treat their u32 input as sRGB-encoded and
/// gamma-decode it back to linear during compositing. To survive that
/// round-trip our linear value must be gamma-encoded first, *then*
/// quantized to u8. Naively calling `Color4f::to_color()` clamps and
/// quantizes as if the value were already sRGB-encoded, which produces a
/// darker color and quietly drops the linear-light precision the caller
/// asked for.
pub fn linear_color4f_to_srgb_color(c: &Color4f) -> Color {
    let encode = |v: f32| -> u8 {
        let v = v.clamp(0.0, 1.0);
        let s = if v <= 0.003_130_8 {
            12.92 * v
        } else {
            1.055 * v.powf(1.0 / 2.4) - 0.055
        };
        (s * 255.0).round() as u8
    };
    Color::from_argb(
        (c.a.clamp(0.0, 1.0) * 255.0).round() as u8,
        encode(c.r),
        encode(c.g),
        encode(c.b),
    )
}

pub fn opt_color_arg(cx: &mut FunctionContext, idx: usize) -> Option<Color> {
    match cx.argument_opt(idx) {
        Some(arg) => color_in(cx, arg),
        _ => None,
    }
}

pub fn opt_color_for_key(
    cx: &mut FunctionContext,
    obj: &Handle<JsObject>,
    attr: &str,
) -> Option<Color> {
    obj.get(cx, attr).ok().and_then(|val| color_in(cx, val))
}

pub fn color_to_css<'a>(
    cx: &mut FunctionContext<'a>,
    color: &Color,
) -> JsResult<'a, JsValue> {
    let RGB { r, g, b } = color.to_rgb();
    let css = match color.a() {
        255 => format!("#{:02x}{:02x}{:02x}", r, g, b),
        _ => {
            let alpha = format!("{:.3}", color.a() as f32 / 255.0);
            let alpha = alpha.trim_end_matches('0');
            format!(
                "rgba({}, {}, {}, {})",
                r,
                g,
                b,
                if alpha == "0." { "0" } else { alpha }
            )
        }
    };
    Ok(cx.string(css).upcast())
}

/// Serialize a Color4f back to a CSS color string (sRGB).
pub fn color4f_to_css<'a>(
    cx: &mut FunctionContext<'a>,
    color: &Color4f,
    space: Option<&ColorSpace>,
) -> JsResult<'a, JsValue> {
    // Reported in the space it was named in when that is one we can name, so
    // `color(display-p3 …)` reads back as itself. The value is converted only
    // when the space has no name to give.
    let named = space.and_then(color_space_name);
    if let Some(name) = named
        && name != "srgb"
    {
        let css = match color.a >= 1.0 {
            true => {
                format!("color({name} {} {} {})", color.r, color.g, color.b)
            }
            false => format!(
                "color({name} {} {} {} / {})",
                color.r, color.g, color.b, color.a
            ),
        };
        return Ok(cx.string(css).upcast());
    }

    // A colour outside the sRGB gamut has no hex or `rgb()` spelling, and
    // quantising it to one reports a colour the context is not holding --
    // `oklch(0.7 0.35 30)` read back as `#ff0000`. CSS Color 4's `color()`
    // notation carries the components as they are. A browser echoes the
    // notation the colour was authored in, which needs the source text; this
    // keeps the value, which is the half that matters.
    let inside = |v: f32| (-f32::EPSILON..=1.0 + f32::EPSILON).contains(&v);
    if inside(color.r) && inside(color.g) && inside(color.b) {
        // Rounded, not truncated: `Color4f::to_color` floors, which reports
        // `hwb(90 10% 20%)` as `#72cc19` where every browser says `#73cc1a`.
        let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        let quantised = Color::from_argb(
            byte(color.a),
            byte(color.r),
            byte(color.g),
            byte(color.b),
        );
        return color_to_css(cx, &quantised);
    }

    let css = match color.a >= 1.0 {
        true => format!("color(srgb {} {} {})", color.r, color.g, color.b),
        false => format!(
            "color(srgb {} {} {} / {})",
            color.r, color.g, color.b, color.a
        ),
    };
    Ok(cx.string(css).upcast())
}

//
// Matrices
//

// pub fn matrix_in(cx: &mut FunctionContext, vals:&[Handle<JsValue>]) ->
// NeonResult<Matrix>{   // for converting single js-array args
//   let terms = floats_in(vals);
//   match to_matrix(&terms){
//     Some(matrix) => Ok(matrix),
//     None => cx.throw_error(format!("expected 6 or 9 matrix values (got {})",
// terms.len()))   }
// }

pub fn to_matrix(t: &[f32]) -> Option<Matrix> {
    match t.len() {
        6 => Some(Matrix::new_all(
            t[0], t[1], t[2], t[3], t[4], t[5], 0.0, 0.0, 1.0,
        )),
        9 => Some(Matrix::new_all(
            t[0], t[1], t[2], t[3], t[4], t[5], t[6], t[7], t[8],
        )),
        _ => None,
    }
}

// pub fn matrix_args(cx: &mut FunctionContext, rng: Range<usize>) ->
// NeonResult<Matrix>{   // for converting inline args (e.g., in
// Path.transform())   let terms = opt_float_args(cx, rng);
//   match to_matrix(&terms){
//     Some(matrix) => Ok(matrix),
//     None => cx.throw_error(format!("expected 6 or 9 matrix values (got {})",
// terms.len()))   }
// }

pub fn opt_matrix_arg(cx: &mut FunctionContext, idx: usize) -> Option<Matrix> {
    if let Some(arg) = cx.argument_opt(idx)
        && let Ok(array) = arg.downcast::<JsArray, _>(cx)
        && let Ok(vals) = array.to_vec(cx)
    {
        let terms = floats_in(cx, &vals);
        return to_matrix(&terms);
    }
    None
}

pub fn matrix_arg(cx: &mut FunctionContext, idx: usize) -> NeonResult<Matrix> {
    match opt_matrix_arg(cx, idx) {
        Some(v) => Ok(v),
        None => cx.throw_type_error("Expected a DOMMatrix"),
    }
}

//
// Points
//

pub fn points_arg(
    cx: &mut FunctionContext,
    idx: usize,
) -> NeonResult<Vec<Point>> {
    let mut nums: Vec<f32> = vec![];
    if let Some(arg) = cx.argument_opt(idx)
        && let Ok(array) = arg.downcast::<JsArray, _>(cx)
        && let Ok(vals) = array.to_vec(cx)
    {
        nums = floats_in(cx, &vals);
    }

    if nums.len() % 2 == 1 {
        let which = if idx == 1 {
            "first"
        } else if idx == 2 {
            "second"
        } else {
            "an"
        };
        cx.throw_type_error(format!(
            "Lists of x/y points must have an even number of values (got {} in {} argument)",
            nums.len(),
            which
        ))
    } else {
        let points = nums
            .as_slice()
            .chunks_exact(2)
            .map(|pair| Point::new(pair[0], pair[1]))
            .collect();
        Ok(points)
    }
}

//
// Image & ImageData
//

use crate::node::image::ImageData;
use neon::types::buffer::TypedArray;
use skia_safe::{AlphaType, ColorType, ImageInfo};

pub fn opt_image_info_arg(
    cx: &mut FunctionContext,
    idx: usize,
) -> NeonResult<Option<ImageInfo>> {
    if let Some(raw_info) = opt_object_arg(cx, idx) {
        Ok(Some(ImageInfo::new(
            (
                float_for_key(cx, &raw_info, "width")? as _,
                float_for_key(cx, &raw_info, "height")? as _,
            ),
            ColorType::RGBA8888,
            match bool_for_key(cx, &raw_info, "premultiplied")? {
                false => AlphaType::Unpremul,
                true => AlphaType::Premul,
            },
            ColorSpace::new_srgb(),
        )))
    } else {
        Ok(None)
    }
}

pub fn image_data_arg(
    cx: &mut FunctionContext,
    idx: usize,
) -> NeonResult<ImageData> {
    let obj = object_arg(cx, idx, "imageData")?;
    let width = float_for_key(cx, &obj, "width")?;
    let height = float_for_key(cx, &obj, "height")?;
    let color_type = string_for_key(cx, &obj, "colorType")?;
    let color_space = string_for_key(cx, &obj, "colorSpace")?;
    let js_buffer: Handle<JsBuffer> = obj.get(cx, "data")?;
    let buffer = Data::new_copy(js_buffer.as_slice(cx));

    Ok(ImageData::new(
        buffer,
        width,
        height,
        color_type,
        color_space,
    ))
}

pub fn image_data_settings_arg(
    cx: &mut FunctionContext,
    idx: usize,
) -> NeonResult<(ColorType, ColorSpace)> {
    match opt_object_arg(cx, idx) {
        Some(obj) => {
            let color_type = opt_string_for_key(cx, &obj, "colorType")
                .unwrap_or("rgba".to_string());
            let color_space = opt_string_for_key(cx, &obj, "colorSpace")
                .unwrap_or("srgb".to_string());
            Ok((
                to_color_type(&color_type),
                color_space_or_throw(cx, &color_space)?,
            ))
        }
        None => Ok((ColorType::RGBA8888, ColorSpace::new_srgb())),
    }
}

/// Export settings read off an optional argument: color type, color space,
/// matte, density, MSAA. Each is `None` when the key is absent, so the caller
/// can fall back to the canvas's own setting.
pub type ImageDataExportArgs = (
    Option<ColorType>,
    Option<ColorSpace>,
    Option<Color>,
    f32,
    Option<usize>,
);

pub fn image_data_export_arg(
    cx: &mut FunctionContext,
    idx: usize,
) -> NeonResult<ImageDataExportArgs> {
    match opt_object_arg(cx, idx) {
        Some(obj) => {
            // None, not a substituted "rgba"/"srgb": an absent key means
            // "inherit the canvas's own setting", and the caller
            // merges. Defaulting here shadowed the canvas defaults
            // it was supposed to fall back to, which made the colorType
            // and colorSpace passed to `new Canvas()` dead options.
            let color_type = opt_string_for_key(cx, &obj, "colorType")
                .map(|mode| to_color_type(&mode));
            let color_space = match opt_string_for_key(cx, &obj, "colorSpace") {
                Some(space) => Some(color_space_or_throw(cx, &space)?),
                None => None,
            };
            let matte = opt_color_for_key(cx, &obj, "matte");
            let density = opt_float_for_key(cx, &obj, "density").unwrap_or(1.0);
            let msaa = opt_float_for_key(cx, &obj, "msaa").map(|n| n as usize);
            Ok((color_type, color_space, matte, density, msaa))
        }
        None => Ok((None, None, None, 1.0, None)),
    }
}

/// Parses a `ColorSpace` name, or `None` if it names nothing.
///
/// Separate from `color_space_or_throw` so the caller decides what an
/// unrecognised name means. Nothing should choose sRGB silently: asking for
/// `hdr10` and getting sRGB back with no word said is the failure this
/// replaced.
pub fn opt_color_space(mode_name: &str) -> Option<ColorSpace> {
    use skia_safe::{named_primaries, named_transfer_fn};

    // CICP primaries
    let p_p3 = named_primaries::CicpId::SMPTE_EG_432_1; // Display P3
    let p_2020 = named_primaries::CicpId::Rec2020; // BT.2020

    // CICP transfer functions
    let t_srgb = named_transfer_fn::CicpId::IEC61966_2_1; // sRGB
    let t_linear = named_transfer_fn::CicpId::Linear; // Linear
    let t_709 = named_transfer_fn::CicpId::Rec709; // BT.709
    let t_pq = named_transfer_fn::CicpId::PQ; // PQ (HDR10)
    let t_hlg = named_transfer_fn::CicpId::HLG; // HLG

    // A CICP space Skia declines to build still falls back, because that is a
    // build limitation rather than caller error -- but the name was valid, so
    // it is not reported as one.
    match mode_name {
        "srgb" => Some(ColorSpace::new_srgb()),
        "srgb-linear" | "linear" => Some(ColorSpace::new_srgb_linear()),

        // Display P3 (wide gamut, used by Apple devices)
        "display-p3" | "p3" => Some(
            ColorSpace::new_cicp(p_p3, t_srgb)
                .unwrap_or_else(ColorSpace::new_srgb),
        ),
        "display-p3-linear" | "p3-linear" => Some(
            ColorSpace::new_cicp(p_p3, t_linear)
                .unwrap_or_else(ColorSpace::new_srgb_linear),
        ),

        // Rec. 2020 (wide gamut for UHD/HDR)
        "rec2020" | "bt2020" => Some(
            ColorSpace::new_cicp(p_2020, t_709)
                .unwrap_or_else(ColorSpace::new_srgb),
        ),
        "rec2020-linear" | "bt2020-linear" => Some(
            ColorSpace::new_cicp(p_2020, t_linear)
                .unwrap_or_else(ColorSpace::new_srgb_linear),
        ),

        // HDR transfer functions with Rec.2020 gamut
        "rec2020-pq" | "hdr10" => Some(
            ColorSpace::new_cicp(p_2020, t_pq)
                .unwrap_or_else(ColorSpace::new_srgb),
        ),
        "rec2020-hlg" | "hlg" => Some(
            ColorSpace::new_cicp(p_2020, t_hlg)
                .unwrap_or_else(ColorSpace::new_srgb),
        ),

        _ => None,
    }
}

/// Parses a `ColorSpace` name or throws, as Chrome does for an invalid
/// `colorSpace`. Silently substituting sRGB meant a caller could ask for a
/// wide gamut, get none, and have nothing to go on.
pub fn color_space_or_throw<'a, C: Context<'a>>(
    cx: &mut C,
    name: &str,
) -> NeonResult<ColorSpace> {
    match opt_color_space(name) {
        Some(space) => Ok(space),
        None => cx.throw_type_error(format!("Unknown colorSpace: {name}")),
    }
}

/// Canonical name for a `ColorSpace` alias, e.g. `p3` -> `display-p3`.
///
/// Reported back rather than derived from the `ColorSpace` object: Skia keeps
/// no record of which CICP pair built it, so the only honest answer is the one
/// the caller asked for. The previous reverse mapping could answer nothing but
/// `srgb` or `srgb-linear`, which would have made a Display P3 canvas claim to
/// be sRGB.
pub fn canonical_color_space(mode_name: &str) -> Option<&'static str> {
    Some(match mode_name {
        "srgb" => "srgb",
        "srgb-linear" | "linear" => "srgb-linear",
        "display-p3" | "p3" => "display-p3",
        "display-p3-linear" | "p3-linear" => "display-p3-linear",
        "rec2020" | "bt2020" => "rec2020",
        "rec2020-linear" | "bt2020-linear" => "rec2020-linear",
        "rec2020-pq" | "hdr10" => "rec2020-pq",
        "rec2020-hlg" | "hlg" => "rec2020-hlg",
        _ => return None,
    })
}

pub fn to_color_type(type_name: &str) -> ColorType {
    match type_name {
        "Alpha8" => ColorType::Alpha8,
        "RGB565" => ColorType::RGB565,
        "ARGB4444" => ColorType::ARGB4444,
        "RGBA1010102" => ColorType::RGBA1010102,
        "BGRA1010102" => ColorType::BGRA1010102,
        "RGB101010x" => ColorType::RGB101010x,
        "BGR101010x" => ColorType::BGR101010x,
        "Gray8" => ColorType::Gray8,
        "RGBAF16Norm" => ColorType::RGBAF16Norm,
        "RGBAF16" => ColorType::RGBAF16,
        "RGBAF32" => ColorType::RGBAF32,
        "R8G8UNorm" => ColorType::R8G8UNorm,
        "A16Float" => ColorType::A16Float,
        "R16G16Float" => ColorType::R16G16Float,
        "A16UNorm" => ColorType::A16UNorm,
        "R16G16UNorm" => ColorType::R16G16UNorm,
        "R16G16B16A16UNorm" => ColorType::R16G16B16A16UNorm,
        "SRGBA8888" => ColorType::SRGBA8888,
        "R8UNorm" => ColorType::R8UNorm,
        "N32" => ColorType::N32,
        "RGB888x" | "rgb" => ColorType::RGB888x,
        "BGRA8888" | "bgra" => ColorType::BGRA8888,
        _ => ColorType::RGBA8888,
    }
}

pub fn from_color_type(color_type: ColorType) -> String {
    match color_type {
        ColorType::Alpha8 => "Alpha8",
        ColorType::RGB565 => "RGB565",
        ColorType::ARGB4444 => "ARGB4444",
        // The three with a short alias report the alias, because that is the
        // spelling the JS API uses everywhere else --
        // ImageData.colorType has always read "rgba", not "RGBA8888".
        // to_color_type accepts both.
        ColorType::RGBA8888 => "rgba",
        ColorType::RGB888x => "rgb",
        ColorType::BGRA8888 => "bgra",
        ColorType::RGBA1010102 => "RGBA1010102",
        ColorType::BGRA1010102 => "BGRA1010102",
        ColorType::RGB101010x => "RGB101010x",
        ColorType::BGR101010x => "BGR101010x",
        ColorType::Gray8 => "Gray8",
        ColorType::RGBAF16Norm => "RGBAF16Norm",
        ColorType::RGBAF16 => "RGBAF16",
        ColorType::RGBAF32 => "RGBAF32",
        ColorType::R8G8UNorm => "R8G8UNorm",
        ColorType::A16Float => "A16Float",
        ColorType::R16G16Float => "R16G16Float",
        ColorType::A16UNorm => "A16UNorm",
        ColorType::R16G16UNorm => "R16G16UNorm",
        ColorType::R16G16B16A16UNorm => "R16G16B16A16UNorm",
        ColorType::SRGBA8888 => "SRGBA8888",
        ColorType::R8UNorm => "R8UNorm",
        _ => "unknown",
    }
    .to_string()
}

//
// ExportOptions
//

use crate::context::page::ExportOptions;

/// `defaults` supplies the canvas's own settings for keys the call omits, so
/// `new Canvas(w, h, {colorType})` is inherited by every export from it while
/// an explicit per-call `colorType` still wins.
pub fn export_options_arg(
    cx: &mut FunctionContext,
    idx: usize,
    defaults: &ExportOptions,
) -> NeonResult<ExportOptions> {
    let opts = match opt_object_arg(cx, idx) {
        Some(obj) => obj,
        None => return cx.throw_type_error("Expected an options object"),
    };
    let format = string_for_key(cx, &opts, "format")?;
    let quality = float_for_key(cx, &opts, "quality")?;
    let density = float_for_key(cx, &opts, "density")?;
    let jpeg_downsample = bool_for_key(cx, &opts, "downsample")?;
    let matte = opt_color_for_key(cx, &opts, "matte");
    let msaa =
        opt_float_for_key(cx, &opts, "msaa").map(|num| num.floor() as usize);
    let color_type = opt_string_for_key(cx, &opts, "colorType")
        .map(|mode| to_color_type(&mode))
        .unwrap_or(defaults.color_type);
    let text_contrast = float_for_key(cx, &opts, "textContrast")?;
    let text_gamma = float_for_key(cx, &opts, "textGamma")?;
    let outline = bool_for_key(cx, &opts, "outline")?;

    // The output space follows the call, falling back to the canvas's own.
    // The surface space never does: drawing happens in the space the canvas
    // was built with, and an export converts out of it.
    let color_space = match opt_string_for_key(cx, &opts, "colorSpace") {
        Some(s) => color_space_or_throw(cx, &s)?,
        None => defaults.surface_color_space.clone(),
    };

    Ok(ExportOptions {
        format,
        quality,
        density,
        outline,
        matte,
        msaa,
        color_type,
        color_space,
        surface_color_space: defaults.surface_color_space.clone(),
        // As with the space: `colorType` above is what this call reads back
        // in, while the surface keeps the format the canvas was built with.
        surface_color_type: defaults.surface_color_type,
        jpeg_downsample,
        text_contrast,
        text_gamma,
    })
}

//
// Path2D
//

use crate::node::path::BoxedPath2D;

pub fn opt_skpath_arg(cx: &mut FunctionContext, idx: usize) -> Option<Path> {
    if let Some(arg) = cx.argument_opt(idx)
        && let Ok(arg) = arg.downcast::<BoxedPath2D, _>(cx)
    {
        let arg = arg.borrow();
        return Some(arg.path());
    }
    None
}

pub fn path2d_arg<'a>(
    cx: &mut FunctionContext<'a>,
    idx: usize,
) -> NeonResult<Handle<'a, BoxedPath2D>> {
    if cx.len() <= idx {
        return cx.throw_type_error(format!(
            "not enough arguments (missing: Path2D as {} arg)",
            arg_num(idx)
        ));
    }

    match cx.argument::<JsValue>(idx)?.downcast::<BoxedPath2D, _>(cx) {
        Ok(path_obj) => Ok(path_obj),
        Err(_) => cx.throw_type_error(format!(
            "Expected a Path2D for {} arg",
            arg_num(idx)
        )),
    }
}

//
// Filters
//

use crate::node::filter::{FilterSpec, SamplingQuality};

pub fn filter_arg(
    cx: &mut FunctionContext,
    idx: usize,
) -> NeonResult<(String, Vec<FilterSpec>)> {
    let arg = cx.argument::<JsObject>(idx)?;
    let canonical = string_for_key(cx, &arg, "canonical")?;

    let obj: Handle<JsObject> = arg.get(cx, "filters")?;
    let keys = obj.get_own_property_names(cx)?.to_vec(cx)?;
    let mut filters = vec![];
    for (name, key) in strings_in(cx, &keys).iter().zip(keys) {
        match name.as_str() {
            "drop-shadow" => {
                let values = obj.get::<JsArray, _, _>(cx, key)?;
                let nums = values.to_vec(cx)?;
                let dims = floats_in(cx, &nums);
                let color_str = values.get::<JsString, _, _>(cx, 3)?.value(cx);
                if let Some(color) = css_to_color(&color_str) {
                    filters.push(FilterSpec::Shadow {
                        offset: Point::new(dims[0], dims[1]),
                        blur: dims[2],
                        color,
                    });
                }
            }
            _ => {
                let value =
                    obj.get::<JsNumber, _, _>(cx, key)?.value(cx) as f32;
                filters.push(FilterSpec::Plain {
                    name: name.to_string(),
                    value,
                })
            }
        }
    }
    Ok((canonical, filters))
}

pub fn to_filter_quality(mode_name: &str) -> Option<SamplingQuality> {
    let mode = match mode_name.to_lowercase().as_str() {
        "low" => SamplingQuality::Low,
        "medium" => SamplingQuality::Medium,
        "high" => SamplingQuality::High,
        _ => return None,
    };
    Some(mode)
}

pub fn from_filter_quality(mode: SamplingQuality) -> String {
    match mode {
        SamplingQuality::Low => "low",
        SamplingQuality::Medium => "medium",
        SamplingQuality::High => "high",
        _ => "low",
    }
    .to_string()
}

//
// CanvasPattern
//

pub fn repetition_arg<'a>(
    cx: &mut FunctionContext<'a>,
    idx: usize,
) -> NeonResult<(TileMode, TileMode)> {
    let repetition = if cx.len() > idx
        && cx.argument::<JsValue>(idx)?.is_a::<JsNull, _>(cx)
    {
        "".to_string() // null is a valid synonym for "repeat" (as is "")
    } else {
        string_arg(cx, idx, "repetition")?
    };

    match to_repeat_mode(&repetition) {
        Some(mode) => Ok(mode),
        None => cx.throw_type_error(
            "Expected `repetition` to be \"repeat\", \"repeat-x\", \"repeat-y\", or \"no-repeat\"",
        ),
    }
}

//
// Skia Enums
//

use skia_safe::{
    TileMode,
    TileMode::{Decal, Repeat},
};
pub fn to_repeat_mode(repeat: &str) -> Option<(TileMode, TileMode)> {
    let mode = match repeat.to_lowercase().as_str() {
        "repeat" | "" => (Repeat, Repeat),
        "repeat-x" => (Repeat, Decal),
        "repeat-y" => (Decal, Repeat),
        "no-repeat" => (Decal, Decal),
        _ => return None,
    };
    Some(mode)
}

use skia_safe::PaintCap;
pub fn to_stroke_cap(mode_name: &str) -> Option<PaintCap> {
    let mode = match mode_name.to_lowercase().as_str() {
        "butt" => PaintCap::Butt,
        "round" => PaintCap::Round,
        "square" => PaintCap::Square,
        _ => return None,
    };
    Some(mode)
}

pub fn from_stroke_cap(mode: PaintCap) -> String {
    match mode {
        PaintCap::Butt => "butt",
        PaintCap::Round => "round",
        PaintCap::Square => "square",
    }
    .to_string()
}

use skia_safe::PaintJoin;
pub fn to_stroke_join(mode_name: &str) -> Option<PaintJoin> {
    let mode = match mode_name.to_lowercase().as_str() {
        "miter" => PaintJoin::Miter,
        "round" => PaintJoin::Round,
        "bevel" => PaintJoin::Bevel,
        _ => return None,
    };
    Some(mode)
}

pub fn from_stroke_join(mode: PaintJoin) -> String {
    match mode {
        PaintJoin::Miter => "miter",
        PaintJoin::Round => "round",
        PaintJoin::Bevel => "bevel",
    }
    .to_string()
}

use skia_safe::BlendMode;
pub fn to_blend_mode(mode_name: &str) -> Option<BlendMode> {
    let mode = match mode_name.to_lowercase().as_str() {
        "source-over" => BlendMode::SrcOver,
        "destination-over" => BlendMode::DstOver,
        "copy" => BlendMode::Src,
        "destination" => BlendMode::Dst,
        "clear" => BlendMode::Clear,
        "modulate" => BlendMode::Modulate,
        "source-in" => BlendMode::SrcIn,
        "destination-in" => BlendMode::DstIn,
        "source-out" => BlendMode::SrcOut,
        "destination-out" => BlendMode::DstOut,
        "source-atop" => BlendMode::SrcATop,
        "destination-atop" => BlendMode::DstATop,
        "xor" => BlendMode::Xor,
        "lighter" => BlendMode::Plus,
        "multiply" => BlendMode::Multiply,
        "screen" => BlendMode::Screen,
        "overlay" => BlendMode::Overlay,
        "darken" => BlendMode::Darken,
        "lighten" => BlendMode::Lighten,
        "color-dodge" => BlendMode::ColorDodge,
        "color-burn" => BlendMode::ColorBurn,
        "hard-light" => BlendMode::HardLight,
        "soft-light" => BlendMode::SoftLight,
        "difference" => BlendMode::Difference,
        "exclusion" => BlendMode::Exclusion,
        "hue" => BlendMode::Hue,
        "saturation" => BlendMode::Saturation,
        "color" => BlendMode::Color,
        "luminosity" => BlendMode::Luminosity,
        _ => return None,
    };
    Some(mode)
}

/// Parses a blend mode for the filter factories, which accept more spellings
/// than `globalCompositeOperation` does.
///
/// The standard names are tried first, so [`to_blend_mode`] stays the single
/// definition of what the Canvas API accepts and this only adds to it: Skia's
/// own `src`/`dst` shorthands and CanvasKit's camel case, matched after
/// lowercasing so `srcOver` and `srcover` are one arm.
///
/// Kept separate rather than folded into [`to_blend_mode`] on purpose. These
/// spellings belong to the CanvasKit-mirroring surface; letting
/// `globalCompositeOperation` accept `"srcOver"` would put a name in the core
/// API that no browser has.
pub fn to_filter_blend_mode(mode_name: &str) -> Option<BlendMode> {
    if let Some(mode) = to_blend_mode(mode_name) {
        return Some(mode);
    }

    Some(match mode_name.to_lowercase().as_str() {
        "src" | "source" => BlendMode::Src,
        "dst" => BlendMode::Dst,
        "srcover" | "src-over" => BlendMode::SrcOver,
        "dstover" | "dst-over" => BlendMode::DstOver,
        "srcin" | "src-in" => BlendMode::SrcIn,
        "dstin" | "dst-in" => BlendMode::DstIn,
        "srcout" | "src-out" => BlendMode::SrcOut,
        "dstout" | "dst-out" => BlendMode::DstOut,
        "srcatop" | "src-atop" => BlendMode::SrcATop,
        "dstatop" | "dst-atop" => BlendMode::DstATop,
        "plus" | "plus-lighter" => BlendMode::Plus,
        "colordodge" => BlendMode::ColorDodge,
        "colorburn" => BlendMode::ColorBurn,
        "hardlight" => BlendMode::HardLight,
        "softlight" => BlendMode::SoftLight,
        _ => return None,
    })
}

/// Parses a CSS color argument, or throws.
///
/// Substituting a default for an unparseable color hid the mistake and then
/// painted with a color the caller never asked for. Chrome throws for an
/// invalid color in a method argument -- `addColorStop` is the closest
/// parallel -- rather than picking one.
///
/// # Errors
///
/// Throws a `TypeError` if the argument is missing, is not a string, or names
/// no color.
pub fn color_arg(
    cx: &mut FunctionContext,
    idx: usize,
    attr: &str,
) -> NeonResult<Color> {
    let name = string_arg(cx, idx, attr)?;
    match css_to_color(&name) {
        Some(color) => Ok(color),
        None => cx.throw_type_error(format!(
            "Could not parse {attr} color \"{name}\""
        )),
    }
}

/// Parses a blend mode argument for the filter factories, or throws.
///
/// Accepts everything [`to_filter_blend_mode`] does. Unlike
/// `globalCompositeOperation`, which the Canvas standard requires to ignore a
/// value it does not recognise, this is a method argument -- so an unknown one
/// is an error rather than a silent source-over.
///
/// # Errors
///
/// Throws a `TypeError` if the argument is missing, is not a string, or names
/// no blend mode.
pub fn filter_blend_mode_arg(
    cx: &mut FunctionContext,
    idx: usize,
    attr: &str,
) -> NeonResult<BlendMode> {
    let name = string_arg(cx, idx, attr)?;
    match to_filter_blend_mode(&name) {
        Some(mode) => Ok(mode),
        None => cx.throw_type_error(format!("Unknown {attr} \"{name}\"")),
    }
}

pub fn from_blend_mode(mode: BlendMode) -> String {
    match mode {
        BlendMode::SrcOver => "source-over",
        BlendMode::DstOver => "destination-over",
        BlendMode::Src => "copy",
        BlendMode::Dst => "destination",
        BlendMode::Clear => "clear",
        BlendMode::Modulate => "modulate",
        BlendMode::SrcIn => "source-in",
        BlendMode::DstIn => "destination-in",
        BlendMode::SrcOut => "source-out",
        BlendMode::DstOut => "destination-out",
        BlendMode::SrcATop => "source-atop",
        BlendMode::DstATop => "destination-atop",
        BlendMode::Xor => "xor",
        BlendMode::Plus => "lighter",
        BlendMode::Multiply => "multiply",
        BlendMode::Screen => "screen",
        BlendMode::Overlay => "overlay",
        BlendMode::Darken => "darken",
        BlendMode::Lighten => "lighten",
        BlendMode::ColorDodge => "color-dodge",
        BlendMode::ColorBurn => "color-burn",
        BlendMode::HardLight => "hard-light",
        BlendMode::SoftLight => "soft-light",
        BlendMode::Difference => "difference",
        BlendMode::Exclusion => "exclusion",
        BlendMode::Hue => "hue",
        BlendMode::Saturation => "saturation",
        BlendMode::Color => "color",
        BlendMode::Luminosity => "luminosity",
    }
    .to_string()
}

use skia_safe::PathOp;
pub fn to_path_op(op_name: &str) -> Option<PathOp> {
    let op = match op_name.to_lowercase().as_str() {
        "difference" => PathOp::Difference,
        "intersect" => PathOp::Intersect,
        "union" => PathOp::Union,
        "xor" => PathOp::XOR,
        "reversedifference" | "complement" => PathOp::ReverseDifference,
        _ => return None,
    };
    Some(op)
}

use skia_safe::path_1d_path_effect;
pub fn to_1d_style(mode_name: &str) -> Option<path_1d_path_effect::Style> {
    let mode = match mode_name.to_lowercase().as_str() {
        "move" => path_1d_path_effect::Style::Translate,
        "turn" => path_1d_path_effect::Style::Rotate,
        "follow" => path_1d_path_effect::Style::Morph,
        _ => return None,
    };
    Some(mode)
}

pub fn from_1d_style(mode: path_1d_path_effect::Style) -> String {
    match mode {
        path_1d_path_effect::Style::Translate => "move",
        path_1d_path_effect::Style::Rotate => "turn",
        path_1d_path_effect::Style::Morph => "follow",
    }
    .to_string()
}

use skia_safe::PathFillType;

pub fn fill_rule_arg_or(
    cx: &mut FunctionContext,
    idx: usize,
    default: &str,
) -> NeonResult<PathFillType> {
    let err_msg = format!(
        "Expected `fillRule` to be \"nonzero\" or \"evenodd\" for {} arg",
        arg_num(idx)
    );

    // if arg is provided, verify that it's a string (if absent use default val)
    let mode = match cx.argument_opt(idx) {
        Some(arg) => match arg.downcast::<JsString, _>(cx) {
            Ok(v) => Ok(v.value(cx)),
            Err(_e) => cx.throw_type_error(&err_msg),
        },
        None => Ok(default.to_string()),
    }?;

    match mode.as_str() {
        "nonzero" => Ok(PathFillType::Winding),
        "evenodd" => Ok(PathFillType::EvenOdd),
        _ => cx.throw_type_error(&err_msg),
    }
}

use crate::gpu::RenderingEngine;
pub fn to_engine(engine_name: &str) -> Option<RenderingEngine> {
    let mode = match engine_name.to_lowercase().as_str() {
        "gpu" => RenderingEngine::GPU,
        "cpu" => RenderingEngine::CPU,
        _ => return None,
    };
    Some(mode)
}

pub fn from_engine(engine: RenderingEngine) -> String {
    match engine {
        RenderingEngine::GPU => "gpu",
        RenderingEngine::CPU => "cpu",
    }
    .to_string()
}

// pub fn blend_mode_arg(cx: &mut FunctionContext, idx: usize, attr: &str) ->
// NeonResult<BlendMode>{   let mode_name = string_arg(cx, idx, attr)?;
//   match to_blend_mode(&mode_name){
//     Some(blend_mode) => Ok(blend_mode),
//     None => cx.throw_error("blendMode must be SrcOver, DstOver, Src, Dst,
// Clear, SrcIn, DstIn, \                             SrcOut, DstOut, SrcATop,
// DstATop, Xor, Plus, Multiply, Screen, Overlay, \
// Darken, Lighten, ColorDodge, ColorBurn, HardLight, SoftLight, Difference, \
//                             Exclusion, Hue, Saturation, Color, Luminosity, or
// Modulate")   }
// }
