#![allow(dead_code)]
use core::ops::Range;
use neon::prelude::*;
use skia_safe::{Color, Color4f, ColorSpace, Data, Matrix, Path, Point, RGB};
use std::cmp;

// One definition of the sRGB transfer function for the whole crate. This
// file carried its own copy of each direction plus a third inlined into
// `linear_color4f_to_srgb_color`, all six constants spelled out and
// uncited at every one -- and the decode here was extended through zero
// while the one in `color` was not, so the same negative component came
// back as two different numbers.
use crate::{
    color::{
        RgbaLinear, SRGB_SLOPE, linear_to_srgb, linear_to_srgb_byte,
        srgb_to_linear,
    },
    css::{
        DEGREES_PER_TURN, Dimension, Expect, FontUnits, Percent, evaluate,
        is_math, literal,
    },
};

//
// meta-helpers
//

/// `n` with its English ordinal suffix -- `1st`, `2nd`, `13th`, `21st`.
///
/// Not incremented: argument 0 is always the receiver, so the index a caller
/// counts from is already the one to print.
///
/// The rule has one exception and it is the whole reason this is not a
/// lookup on the last digit: eleven, twelve and thirteen take `th` where
/// one, two and three take `st`, `nd` and `rd`. This was written as
/// `((n + 90) % 100 - 10) % 10 - 1`, which is correct -- the `+ 90` walks
/// the teens off the end so they land negative and miss the table -- and
/// gives a reader no way to see that without working three modulo
/// operations by hand.
pub(crate) fn arg_num(o: usize) -> String {
    let suffix = match (o % 100, o % 10) {
        // The teens are irregular in English and take `th` regardless of
        // what their last digit would otherwise ask for.
        (11..=13, _) => "th",
        (_, 1) => "st",
        (_, 2) => "nd",
        (_, 3) => "rd",
        _ => "th",
    };
    format!("{o}{suffix}")
}

/// How far apart two coordinates may be and still count as the same one.
///
/// A hundred-thousandth of a pixel, which is far below anything a canvas can
/// draw and far above the rounding an `f32` accumulates through a matrix.
/// Named rather than spelled out at each comparison below: a tolerance
/// written in two places only means anything while the two agree.
const COORDINATE_EPSILON: f32 = 0.00001;

/// Whether `a` and `b` are the same coordinate to within
/// [`COORDINATE_EPSILON`].
pub fn almost_equal(a: f32, b: f32) -> bool {
    (a - b).abs() < COORDINATE_EPSILON
}

/// Whether `a` is zero to within [`COORDINATE_EPSILON`].
pub fn almost_zero(a: f32) -> bool {
    almost_equal(a, 0.0)
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
            return cx.throw_type_error(format!(
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
            return cx.throw_type_error(format!(
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
            return cx.throw_type_error(format!(
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

/// Refuses a call carrying fewer arguments than `method` reads.
///
/// `names` are the arguments as the caller writes them, so the message can
/// say which ones are missing rather than only that some are. The three
/// sibling helpers here already do that; this one said `not enough
/// arguments` and nothing else, which reached `isPointInPath` and
/// `isPointInStroke` -- the only two methods that use it.
///
/// The receiver arrives as argument 0 and is not one of `names`, hence the
/// saturating subtraction: a call can reach here carrying none at all,
/// because nothing outside this crate is obliged to pass a receiver and the
/// addon's exports are reachable directly.
pub fn check_argc(
    cx: &mut FunctionContext,
    method: &str,
    names: &[&str],
) -> NeonResult<()> {
    let given = cx.len().saturating_sub(1);
    if given >= names.len() {
        return Ok(());
    }
    cx.throw_type_error(format!(
        "not enough arguments: {method} expected {}, got {given} (missing: {})",
        names.len(),
        names[given..].join(", ")
    ))
}

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
            cx.throw_type_error(format!("Expected an object for \"{}\"", attr))
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
            cx.throw_type_error(format!("Expected an object for \"{}\"", attr))
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
            cx.throw_type_error(format!("Expected a string for \"{}\"", attr))
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
            "Expected a boolean value for \"{}\"",
            attr
        )),
    }
}

//
// floats
//

fn _as_double_raw(
    cx: &mut FunctionContext,
    val: &Handle<JsValue>,
) -> Option<f64> {
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
}

/// As [`_as_double_raw`], refusing a value that converts to a non-finite
/// number.
///
/// Nearly every reader wants this: a coordinate of `NaN` is no more usable
/// than a `Symbol`, and both come back `None`. The two callers that need to
/// tell them apart use the raw form -- see [`converts_to_non_finite`].
fn _as_double(cx: &mut FunctionContext, val: &Handle<JsValue>) -> Option<f64> {
    _as_double_raw(cx, val).filter(|num| num.is_finite())
}

/// Whether every argument in `range` converts to a number and at least one of
/// them is not finite.
///
/// The Canvas API separates two failures that [`_as_double`] folds together.
/// A coordinate that converts to `Infinity` or `NaN` makes the call a no-op;
/// a value with no numeric conversion at all -- a `Symbol`, a `BigInt` -- is
/// a `TypeError`. Both arrive as `None` from [`_as_double`], so a reader
/// built on it cannot give them different answers.
///
/// False when anything in `range` fails to convert, so the caller falls
/// through to its usual reader and that value is refused by name. Argument
/// order is not consulted: a call carrying both is refused rather than
/// ignored, which is what a browser does when the unconvertible one is read
/// first and is the safer way round when it is not.
pub fn converts_to_non_finite(
    cx: &mut FunctionContext,
    range: Range<usize>,
) -> bool {
    let mut saw_non_finite = false;
    for idx in range {
        match cx
            .argument_opt(idx)
            .and_then(|val| _as_double_raw(cx, &val))
        {
            Some(num) if !num.is_finite() => saw_non_finite = true,
            Some(_) => (),
            // No numeric conversion, or no such argument. Either way this is
            // not the case being detected.
            None => return false,
        }
    }
    saw_non_finite
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
    // Arguments start after the `this` reference, and a call can arrive
    // carrying fewer than that: nothing outside this crate is obliged to pass
    // a receiver, and the addon's exports are reachable directly. Saturating,
    // so an empty call reports the count that is wrong. A bare subtraction
    // wraps to `usize::MAX` here, which skips the check below and reports the
    // first *value* as missing instead -- or, where overflow checks are on,
    // panics across the Neon boundary.
    let argc = cx.len().saturating_sub(start);
    if argc < names.len() {
        return cx.throw_type_error(format!(
            "not enough arguments (missing: {})",
            names[argc..].join(", ")
        ));
    }

    let mut args: Vec<f32> = Vec::new();
    for (i, name) in names.iter().enumerate() {
        match classify_double(cx, i + start) {
            Ok(v) => args.push(v as f32),
            Err(non_finite) => {
                // `or_bail` used to decide the marker for every refusal this
                // reader makes, so a call site chose whether a `Symbol` was
                // ignored or refused. The failure decides now: a value that
                // is not a number at all is always raised, and one that is
                // merely non-finite is swallowed into the no-op the Canvas
                // API asks for.
                return cx.throw_type_error(format!(
                    "{}Expected a number for `{}` as {} arg",
                    refusal_marker(non_finite && or_bail),
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

/// The array at `attr`, or `None` when the key is absent or holds
/// something else.
pub fn opt_array_for_key<'a>(
    cx: &mut FunctionContext<'a>,
    obj: &Handle<'a, JsObject>,
    attr: &str,
) -> Option<Handle<'a, JsArray>> {
    obj.get(cx, attr)
        .ok()
        .and_then(|val: Handle<JsValue>| val.downcast::<JsArray, _>(cx).ok())
}

pub fn float_for_key(
    cx: &mut FunctionContext,
    obj: &Handle<JsObject>,
    attr: &str,
) -> NeonResult<f32> {
    match opt_float_for_key(cx, obj, attr) {
        Some(num) => Ok(num),
        None => cx.throw_type_error(format!(
            "Expected a numerical value for \"{}\"",
            attr
        )),
    }
}

/// Every number in an array argument, or `None` if it is not an array of them.
///
/// All or nothing: a list holding something that is not a number is not a list
/// of numbers, and the verb reading it decides what to do about that.
pub fn opt_float_vec_arg(
    cx: &mut FunctionContext,
    idx: usize,
) -> Option<Vec<f32>> {
    let list = cx.argument_opt(idx)?.downcast::<JsArray, _>(cx).ok()?;
    let raw = list.to_vec(cx).ok()?;
    let numbers = floats_in(cx, &raw);
    match numbers.len() == raw.len() {
        true => Some(numbers),
        false => None,
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

/// Argument `idx` as a number, and when it is not one, which of the two ways
/// it failed.
///
/// The Canvas API separates the two and this reports them apart. A value
/// that converts to a non-finite number makes the call a no-op --
/// `Err(true)` here. A value with no numeric conversion at all is a
/// `TypeError` the caller sees: WebIDL makes `ToNumber` on a `Symbol` or a
/// `BigInt` throw, and a browser raises one -- `Err(false)`.
///
/// Both still arrive as `None` from [`_as_double`], so a reader built on
/// that one cannot tell them apart, and which answer it gave was a property
/// of the call site: the path verbs ignored both, `roundRect` refused both.
fn classify_double(cx: &mut FunctionContext, idx: usize) -> Result<f64, bool> {
    match cx
        .argument_opt(idx)
        .and_then(|val| _as_double_raw(cx, &val))
    {
        Some(num) if num.is_finite() => Ok(num),
        Some(_) => Err(true),
        None => Err(false),
    }
}

/// The marker a refusal carries.
///
/// Present for a non-finite number, so `lib/classes/neon.js` swallows the
/// message unless `SKIA_CANVAS_STRICT` is set and the call is the no-op the
/// Canvas API asks for.
///
/// Absent only for a value JavaScript cannot convert to a number at all -- a
/// `Symbol` or a `BigInt`, which throw on conversion -- and that error is
/// raised in both modes. A string, a plain object and `undefined` are none of
/// them numbers and all take the marked branch, because the conversion above
/// turns them into `NaN` before this ever sees them. The line is therefore
/// *coerces to a number* against *throws on coercion*, not *number* against
/// *not a number*.
fn refusal_marker(non_finite: bool) -> &'static str {
    match non_finite {
        true => "\u{26a0}\u{fe0f}",
        false => "",
    }
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
    match classify_double(cx, idx) {
        Ok(num) => Ok(num),
        Err(non_finite) => cx.throw_type_error(format!(
            "{}Expected a number for `{attr}` as {} arg",
            refusal_marker(non_finite),
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

/// The same as [`float_args_or_bail_n`], without narrowing.
///
/// A JavaScript number is an `f64`, and so is the buffer a batch of verbs
/// arrives in, so this is what the arguments actually are. Most verbs hand
/// them to Skia, which is `f32` throughout, and narrow immediately -- but not
/// all: `globalAlpha` is kept wide here on purpose, so that a fill at 0.5 lands
/// on an alpha byte of 128 rather than 127.
pub fn double_args_or_bail_n<const N: usize>(
    cx: &mut FunctionContext,
    names: &[&str; N],
) -> NeonResult<[f64; N]> {
    // Saturating for the reason [`_float_args_at`] gives.
    let argc = cx.len().saturating_sub(1);
    if argc < N {
        return cx.throw_type_error(format!(
            "not enough arguments (missing: {})",
            names[argc..].join(", ")
        ));
    }

    let mut args = [0.0; N];
    for (i, name) in names.iter().enumerate() {
        match classify_double(cx, i + 1) {
            Ok(v) => args[i] = v,
            Err(non_finite) => {
                // Marked only when the value was a number that is not
                // finite; a value with no numeric conversion is refused
                // whatever strict mode says.
                return cx.throw_type_error(format!(
                    "{}Expected a number for `{}` as {} arg",
                    refusal_marker(non_finite),
                    name,
                    arg_num(i + 1)
                ));
            }
        }
    }

    Ok(args)
}

/// The same as [`float_args_or_bail`], into a fixed-size array.
///
/// The allocating form returns a `Vec` for two floats, which measured at 39
/// nanoseconds of the 82 a `lineTo` costs -- more than twice the 17 the
/// crossing itself takes. Nothing about the argument list needs the heap: the
/// widest call here takes nine numbers, and the count is known where it is
/// written.
pub fn float_args_or_bail_n<const N: usize>(
    cx: &mut FunctionContext,
    names: &[&str; N],
) -> NeonResult<[f32; N]> {
    // Saturating for the reason [`_float_args_at`] gives.
    let argc = cx.len().saturating_sub(1);
    if argc < N {
        return cx.throw_type_error(format!(
            "not enough arguments (missing: {})",
            names[argc..].join(", ")
        ));
    }

    let mut args = [0.0; N];
    for (i, name) in names.iter().enumerate() {
        match classify_double(cx, i + 1) {
            Ok(v) => args[i] = v as f32,
            Err(non_finite) => {
                // Marked only when the value was a number that is not
                // finite; a value with no numeric conversion is refused
                // whatever strict mode says.
                return cx.throw_type_error(format!(
                    "{}Expected a number for `{}` as {} arg",
                    refusal_marker(non_finite),
                    name,
                    arg_num(i + 1)
                ));
            }
        }
    }

    Ok(args)
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

/// Rec. 2020's transfer function, extended through zero the way
/// [`srgb_to_linear`] is.
///
/// ITU-R BT.2020 writes the encode as
///
/// ```text
/// V = 4.5 L                        for L < beta
/// V = alpha L^0.45 - (alpha - 1)   otherwise
/// ```
///
/// and this is its inverse. The constants are the standard's own, to the
/// precision it gives for ten-bit systems.
fn rec2020_decode(encoded: f32) -> f32 {
    /// Scale of the power segment, and the offset that follows from it.
    const ALPHA: f32 = 1.099_296_8;
    /// Breakpoint, on the linear-light side.
    const BETA: f32 = 0.018_053_97;
    /// Slope of the linear segment near black. Steeper than sRGB's 12.92
    /// because the breakpoint sits higher.
    const SLOPE: f32 = 4.5;
    /// Exponent of the power segment. Unlike sRGB, the standard states this
    /// one on the encode side, so the decode raises to its reciprocal.
    const EXPONENT: f32 = 0.45;

    let magnitude = encoded.abs();
    let linear = match magnitude < BETA * SLOPE {
        true => magnitude / SLOPE,
        false => ((magnitude + ALPHA - 1.0) / ALPHA).powf(1.0 / EXPONENT),
    };
    linear.copysign(encoded)
}

/// Parses CSS Color 4's `color(<space> r g b[ / a])`.
///
/// Handled here for the four spaces `csscolorparser` refuses, `display-p3`,
/// `rec2020`, `a98-rgb` and `prophoto-rgb`. It parses `srgb`, `srgb-linear`
/// and the three `xyz` forms itself as of 0.9, and this runs first, so every
/// space a canvas can hold is read by one function. The components of a
/// space a canvas can hold are carried with its name rather than converted
/// here, which is what lets a P3 colour reach a P3 canvas without a trip
/// through sRGB; `a98-rgb` and `prophoto-rgb` come back in sRGB. The function
/// name and the space are ASCII case-insensitive, as CSS names are.
fn parse_color_function(css: &str) -> Option<(Color4f, &'static str)> {
    let css = css.trim();
    let inner = css
        .get(..6)
        .filter(|name| name.eq_ignore_ascii_case("color("))
        .and_then(|_| css[6..].strip_suffix(')'))?;
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
    let name = match space.to_ascii_lowercase().as_str() {
        "srgb" => "srgb",
        "srgb-linear" => "srgb-linear",
        "display-p3" => "display-p3",
        "rec2020" => "rec2020",
        // No canvas holds these two, so there is no space to carry them in
        // and they convert to sRGB here.
        name @ ("a98-rgb" | "prophoto-rgb") => {
            let color = Color4f::new(r, g, b, alpha);
            return Some((color_function_to_srgb(color, name), "srgb"));
        }
        _ => return None,
    };
    Some((Color4f::new(r, g, b, alpha), name))
}

/// Linear-light Display P3 into linear-light sRGB.
///
/// The `f32` nearest the product of two matrices CSS Color 4's sample code
/// publishes as exact fractions, `lin_P3_to_XYZ` then `XYZ_to_lin_sRGB`; the
/// same product derived from each space's primaries and the D65 white point
/// agrees to nine places. `colour_from_css` holds a primary to `1e-5`, which
/// a matrix written to four places misses.
const LINEAR_P3_TO_LINEAR_SRGB: [[f32; 3]; 3] = [
    [1.224_940_2, -0.224_940_18, 0.0],
    [-0.042_056_955, 1.042_056_9, 0.0],
    [-0.019_637_555, -0.078_636_04, 1.098_273_6],
];

/// Linear-light Rec. 2020 into linear-light sRGB, by `lin_2020_to_XYZ` then
/// `XYZ_to_lin_sRGB`, on the terms given for [`LINEAR_P3_TO_LINEAR_SRGB`].
const LINEAR_REC2020_TO_LINEAR_SRGB: [[f32; 3]; 3] = [
    [1.660_491, -0.587_641_1, -0.072_849_86],
    [-0.124_550_48, 1.132_899_9, -0.008_349_422],
    [-0.018_150_764, -0.100_578_9, 1.118_729_7],
];

/// Linear-light A98 RGB into linear-light sRGB.
///
/// The `f32` nearest the product of CSS Color 4's `lin_a98rgb_to_XYZ` and
/// `XYZ_to_lin_sRGB`, both published as exact fractions; the same product
/// derived from Adobe RGB (1998)'s primaries and the D65 white point agrees
/// to fifteen places. The two spaces share their red and blue primaries and
/// their white, so A98's red and blue land on sRGB's own, scaled, and only
/// its green mixes.
const LINEAR_A98_TO_LINEAR_SRGB: [[f32; 3]; 3] = [
    [1.398_355_7, -0.398_355_75, 0.0],
    [0.0, 1.0, 0.0],
    [0.0, -0.042_928_99, 1.042_928_9],
];

/// Linear-light ProPhoto RGB into linear-light sRGB: CSS Color 4's
/// `lin_ProPhoto_to_XYZ`, then `D50_to_D65`, since ProPhoto is referred to
/// D50, then `XYZ_to_lin_sRGB`. The product derived from ProPhoto's
/// primaries and the D50 white point agrees with it to seventeen places.
const LINEAR_PROPHOTO_TO_LINEAR_SRGB: [[f32; 3]; 3] = [
    [2.034_381, -0.727_635_8, -0.306_745_05],
    [-0.228_825_73, 1.231_742_5, -0.002_916_809_6],
    [-0.008_558_828, -0.153_266_7, 1.161_825_5],
];

/// A98 RGB's transfer function, CSS Color 4's `lin_a98rgb`: a pure power,
/// mirrored through zero so a negative component keeps its sign.
fn a98_decode(encoded: f32) -> f32 {
    /// The exponent Adobe RGB (1998) specifies as 2 51/256.
    const EXPONENT: f32 = 563.0 / 256.0;
    encoded.abs().powf(EXPONENT).copysign(encoded)
}

/// ProPhoto RGB's transfer function, CSS Color 4's `lin_ProPhoto`: a 1.8
/// power above a short linear toe, mirrored through zero.
fn prophoto_decode(encoded: f32) -> f32 {
    /// Where the toe ends, on the encoded side.
    const TOE: f32 = 16.0 / 512.0;
    /// The toe's slope, as the encoded value over the linear one.
    const TOE_SLOPE: f32 = 16.0;
    /// The power above the toe.
    const EXPONENT: f32 = 1.8;
    let magnitude = encoded.abs();
    let linear = match magnitude <= TOE {
        true => magnitude / TOE_SLOPE,
        false => magnitude.powf(EXPONENT),
    };
    linear.copysign(encoded)
}

/// The inverse of a 3x3 matrix, by its adjugate, in `f64` so the inverse
/// carries no more error than the `f32` matrix it came from.
///
/// For the conversions out of linear sRGB, derived from the ones into it
/// rather than written out a second time, so the pair cannot disagree.
fn inverse(m: [[f32; 3]; 3]) -> [[f32; 3]; 3] {
    let m = m.map(|row| row.map(f64::from));
    let cofactor = |r: usize, c: usize| {
        let (r0, r1) = ((r + 1) % 3, (r + 2) % 3);
        let (c0, c1) = ((c + 1) % 3, (c + 2) % 3);
        m[r0][c0] * m[r1][c1] - m[r0][c1] * m[r1][c0]
    };
    let determinant = (0..3).map(|c| m[0][c] * cofactor(0, c)).sum::<f64>();
    // The adjugate is the transpose of the cofactor matrix.
    std::array::from_fn(|r| {
        std::array::from_fn(|c| (cofactor(c, r) / determinant) as f32)
    })
}

/// The primaries `space` is built on, by name: `"srgb"`, `"display-p3"` or
/// `"rec2020"`, whatever its transfer function. `None` for any other.
fn primaries(space: &ColorSpace) -> Option<&'static str> {
    let linear = space.with_linear_gamma();
    ["srgb", "display-p3", "rec2020"].into_iter().find(|name| {
        opt_color_space(name).map(|named| named.with_linear_gamma())
            == Some(linear.clone())
    })
}

/// A colour as a paint holds it -- `color` in `space`, encoded with that
/// space's transfer function -- as an [`RgbaLinear`]: premultiplied linear
/// light in the primaries of `working`, the canvas's working space, which is
/// what `RgbaLinear` means.
///
/// Through extended sRGB, so a colour named in any space this crate can name
/// lands in any working space it can build, and a colour outside a gamut
/// keeps its negative or above-one components rather than being clipped. An
/// untagged colour is sRGB, as Skia reads one.
pub(crate) fn color4f_to_working_linear(
    color: Color4f,
    space: Option<&ColorSpace>,
    working: &ColorSpace,
) -> RgbaLinear {
    let srgb = match space.and_then(color_space_name) {
        Some(name) => color_function_to_srgb(color, name),
        None => color,
    };
    let linear = RgbaLinear::from_srgb(srgb.r, srgb.g, srgb.b, srgb.a);
    let to_working = match primaries(working) {
        Some("display-p3") => inverse(LINEAR_P3_TO_LINEAR_SRGB),
        Some("rec2020") => inverse(LINEAR_REC2020_TO_LINEAR_SRGB),
        // Linear sRGB is already the working primaries.
        _ => return linear,
    };
    // A linear map, so it applies to premultiplied components as it would
    // to straight ones.
    let [r, g, b] = to_working
        .map(|row| row[0] * linear.r + row[1] * linear.g + row[2] * linear.b);
    RgbaLinear {
        r,
        g,
        b,
        a: linear.a,
    }
}

/// Converts `color()` components into extended sRGB.
///
/// For the callers that need a colour in sRGB rather than tagged with its
/// space: a gradient stop, an 8-bit `SkColor`, a getter's report and
/// `RgbaLinear::from_css`. A paint keeps the colour in the space it was named
/// in, so a P3 colour on a P3 canvas needs no conversion at all, and a round
/// trip through sRGB would cost it about a level of the eight the readback
/// quantises to.
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
            linear_to_srgb(color.r),
            linear_to_srgb(color.g),
            linear_to_srgb(color.b),
        ],
        "display-p3" => apply(LINEAR_P3_TO_LINEAR_SRGB, linear(srgb_to_linear))
            .map(linear_to_srgb),
        "rec2020" => {
            apply(LINEAR_REC2020_TO_LINEAR_SRGB, linear(rec2020_decode))
                .map(linear_to_srgb)
        }
        "a98-rgb" => apply(LINEAR_A98_TO_LINEAR_SRGB, linear(a98_decode))
            .map(linear_to_srgb),
        "prophoto-rgb" => {
            apply(LINEAR_PROPHOTO_TO_LINEAR_SRGB, linear(prophoto_decode))
                .map(linear_to_srgb)
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

/// CIE Lab's reference white, which CSS Color 4 fixes at D50.
///
/// [Section 10.1] states it directly: "Note that the `lab()` and `lch()`
/// functions use a D50 whitepoint". The chromaticity is the one the
/// specification's own sample code uses, expressed as an XYZ triple at
/// `Y = 1`.
///
/// [Section 10.1]: https://www.w3.org/TR/css-color-4/#cie-lab
const LAB_WHITE_D50: [f32; 3] =
    [0.3457 / 0.3585, 1.0, (1.0 - 0.3457 - 0.3585) / 0.3585];

/// Bradford chromatic adaptation from D50 to D65.
///
/// CSS Color 4 [section 12.1] requires this step between Lab's D50 reference
/// and sRGB's D65 one: "chromatic adaptation ... using the Bradford method".
/// Omitting it shifts blues and yellows while leaving greys alone;
/// [`parse_lab_like`] says why this crate reads Lab itself.
///
/// [section 12.1]: https://www.w3.org/TR/css-color-4/#color-conversion-code
const BRADFORD_D50_TO_D65: [[f32; 3]; 3] = [
    [0.955_473_4, -0.023_098_5, 0.063_259_3],
    [-0.028_369_7, 1.009_995_5, 0.021_041_4],
    [0.012_314, -0.020_507_7, 1.330_365_9],
];

/// XYZ with a D65 reference white into linear-light sRGB.
///
/// The inverse of the sRGB primaries matrix, as CSS Color 4 [section 12.1]
/// gives it.
///
/// [section 12.1]: https://www.w3.org/TR/css-color-4/#color-conversion-code
const XYZ_D65_TO_LINEAR_SRGB: [[f32; 3]; 3] = [
    [3.240_97, -1.537_383_2, -0.498_610_8],
    [-0.969_243_6, 1.875_967_5, 0.041_555_1],
    [0.055_630_1, -0.203_977, 1.056_971_5],
];

/// CIE's `kappa`, the linear segment's slope in the Lab transfer function.
///
/// The rational form 24389/27, which CSS Color 4 [section 12.1] uses in
/// preference to the rounded 903.3 of older texts.
///
/// [section 12.1]: https://www.w3.org/TR/css-color-4/#color-conversion-code
const LAB_KAPPA: f32 = 24389.0 / 27.0;

/// CIE's `epsilon`, where that transfer function leaves its linear segment.
///
/// The rational form 216/24389, for the reason given on [`LAB_KAPPA`].
const LAB_EPSILON: f32 = 216.0 / 24389.0;

/// A `lab()` percentage on the `a` and `b` axes is this many units.
///
/// CSS Color 4 [section 10.1]: "the percentage 100% ... means 125".
///
/// [section 10.1]: https://www.w3.org/TR/css-color-4/#specifying-lab-lch
const LAB_AB_FULL_SCALE: f32 = 125.0;

/// An `lch()` percentage on the chroma axis is this many units.
///
/// CSS Color 4 [section 10.2]: "the percentage 100% ... means 150".
///
/// [section 10.2]: https://www.w3.org/TR/css-color-4/#specifying-lab-lch
const LCH_CHROMA_FULL_SCALE: f32 = 150.0;

/// Reads one `lab()`/`lch()` component, resolving a percentage against
/// `full_scale` and the CSS-wide `none` keyword to zero.
///
/// `none` is a missing component rather than a value, and CSS Color 4
/// [section 4.4] resolves a missing component to zero everywhere it is not
/// being interpolated. Nothing here interpolates.
///
/// [section 4.4]: https://www.w3.org/TR/css-color-4/#missing
fn lab_component(token: &str, full_scale: f32) -> Option<f32> {
    let token = token.trim();
    if token.eq_ignore_ascii_case("none") {
        return Some(0.0);
    }
    match token.strip_suffix('%') {
        Some(pct) => pct.parse::<f32>().ok().map(|v| v / 100.0 * full_scale),
        None => token.parse::<f32>().ok(),
    }
}

/// Reads an `lch()` hue, which CSS Color 4 types as an `<angle>`.
///
/// A bare number is degrees, and all four of CSS's angle units are accepted
/// because the grammar admits them. Refusing one would not reject the colour
/// -- it would fall through to `csscolorparser` and be converted without the
/// adaptation, which is the outcome this function exists to prevent.
fn lab_hue_degrees(token: &str) -> Option<f32> {
    let token = token.trim().to_ascii_lowercase();
    if token == "none" {
        return Some(0.0);
    }
    // `grad` before `rad`, which is a suffix of it: matching `rad` first would
    // read `100grad` as 100 radians and silently place the hue elsewhere.
    for (unit, per_turn) in [
        ("deg", 360.0),
        ("grad", 400.0),
        ("rad", core::f32::consts::TAU),
        ("turn", 1.0),
    ] {
        if let Some(value) = token.strip_suffix(unit) {
            return value.parse::<f32>().ok().map(|v| v / per_turn * 360.0);
        }
    }
    token.parse::<f32>().ok()
}

/// Parses CSS Color 4's `lab()` and `lch()`, adapting D50 to D65, in either
/// case: CSS function names are ASCII case-insensitive, so `LAB(` is the
/// same function and takes the same conversion.
///
/// This is CSS Color 4's conversion -- Lab's D50 white, the Bradford step
/// [`BRADFORD_D50_TO_D65`] performs, and the XYZ-to-sRGB matrix -- and it
/// runs before `csscolorparser` does, which reads Lab too, because the two
/// are not the same reading. Over a grid of every form the grammar takes,
/// 17,712 inputs, this lands within 8.5e-6 of the specification's own
/// conversion done in `f64`, and `csscolorparser` 0.9 alone within 2.2e-3:
/// close enough to agree on most rows, and a whole 8-bit level apart on 121,
/// one of which `the_rust_surface_takes_css_colors` pins against Chrome.
/// Chrome sits nearer this side than that one in the tail of the grid. So
/// this is not
/// redundant with the dependency, and
/// `lab_and_lch_follow_css_color_4_across_their_grammar` fails if Lab is
/// left to it.
///
/// The step it adds exists because `csscolorparser` once skipped the
/// adaptation outright, treating Lab's components as D65-referred: zero
/// error on the neutral axis, where `lab(50% 0 0)` is the same grey either
/// way, growing along `b`, so it read as a slight shift in blues and
/// yellows -- red 31 rather than 63 for `lab(30% 30 -60)`. 0.9 adapts; the
/// difference left is precision.
///
/// `oklab()` and `oklch()` are deliberately left to the crate. Oklab is
/// defined D65-referred and has no adaptation step to get wrong. Its encode
/// of a colour outside sRGB is wrong in another way, which
/// `css_to_color4f_in_space` repairs for every notation the crate converts
/// in linear light.
fn parse_lab_like(css: &str) -> Option<Color4f> {
    let css = css.trim();
    let name = css.get(..4)?;
    let polar = match name {
        _ if name.eq_ignore_ascii_case("lab(") => false,
        _ if name.eq_ignore_ascii_case("lch(") => true,
        _ => return None,
    };
    let inner = &css[4..];
    let inner = inner.strip_suffix(')')?;

    let (components, alpha) = match inner.split_once('/') {
        Some((components, alpha)) => (components, lab_component(alpha, 1.0)?),
        None => (inner, 1.0),
    };
    let parts: Vec<&str> = components.split_whitespace().collect();
    let [lightness, second, third] = parts[..] else {
        return None;
    };

    // `L` is a percentage of 100 in both functions, and a bare number is
    // already on that scale.
    let l = lab_component(lightness, 100.0)?;
    let (a, b) = if polar {
        let chroma = lab_component(second, LCH_CHROMA_FULL_SCALE)?;
        let hue = lab_hue_degrees(third)?.to_radians();
        (chroma * hue.cos(), chroma * hue.sin())
    } else {
        (
            lab_component(second, LAB_AB_FULL_SCALE)?,
            lab_component(third, LAB_AB_FULL_SCALE)?,
        )
    };

    // Lab to XYZ against Lab's own D50 white, CSS Color 4 section 12.1.
    let fy = (l + 16.0) / 116.0;
    let fx = a / 500.0 + fy;
    let fz = fy - b / 200.0;
    let inverse = |f: f32| {
        let cubed = f * f * f;
        if cubed > LAB_EPSILON {
            cubed
        } else {
            (116.0 * f - 16.0) / LAB_KAPPA
        }
    };
    let y = if l > LAB_KAPPA * LAB_EPSILON {
        fy * fy * fy
    } else {
        l / LAB_KAPPA
    };
    let xyz_d50 = [
        inverse(fx) * LAB_WHITE_D50[0],
        y * LAB_WHITE_D50[1],
        inverse(fz) * LAB_WHITE_D50[2],
    ];

    let apply = |m: [[f32; 3]; 3], v: [f32; 3]| {
        [
            m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
            m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
            m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
        ]
    };
    let [r, g, blue] =
        apply(XYZ_D65_TO_LINEAR_SRGB, apply(BRADFORD_D50_TO_D65, xyz_d50))
            .map(linear_to_srgb);

    // Unclamped, for the reason `css_to_color4f` gives: Lab names colours
    // outside the sRGB gamut and clamping here would lose them before the
    // surface that can hold them.
    Some(Color4f::new(r, g, blue, alpha))
}

/// Whether `csscolorparser` builds `css` through `Color::from_linear_rgba`.
///
/// It does for the notations whose conversion runs in linear light --
/// `oklab()`, `oklch()`, `lab()`, `lch()` and `color()` in `srgb-linear` or
/// an `xyz` space -- and for nothing else: hex, names, `rgb()`, `hsl()`,
/// `hwb()` and `color(srgb ...)` are stored as written. The distinction
/// matters because only the first group can carry the encode defect
/// `css_to_color4f_in_space` repairs, and a negative in the second, such as
/// `color(srgb -0.1 0 0)`, already means what it says. Some of the first
/// group are normally read by this crate's own parsers before this is asked;
/// what reaches `csscolorparser` is what they refused, a relative
/// `lab(from ...)` among it.
fn built_from_linear_light(css: &str) -> bool {
    let Some((name, rest)) = css.trim_start().split_once('(') else {
        return false;
    };
    match name.to_ascii_lowercase().as_str() {
        "oklab" | "oklch" | "lab" | "lch" => true,
        "color" => color_function_space(rest).is_some_and(|space| {
            matches!(
                space.to_ascii_lowercase().as_str(),
                "srgb-linear" | "xyz" | "xyz-d50" | "xyz-d65"
            )
        }),
        _ => false,
    }
}

/// The arguments of a call, given what follows its `(`, as spans of `args`:
/// split at whitespace outside parentheses, and ending at the `)` that closes
/// the call. An argument that is itself a call -- a relative colour's origin
/// -- is one span, spaces and all.
fn top_level_spans(args: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut depth = 0usize;
    let mut start = None;
    let mut end = args.len();
    for (at, c) in args.char_indices() {
        match c {
            ')' if depth == 0 => {
                end = at;
                break;
            }
            ')' => depth -= 1,
            '(' => {
                depth += 1;
                start.get_or_insert(at);
            }
            c if c.is_whitespace() && depth == 0 => {
                spans.extend(start.take().map(|from| (from, at)));
            }
            _ => {
                start.get_or_insert(at);
            }
        }
    }
    spans.extend(start.map(|from| (from, end)));
    spans
}

/// The space a `color()` call names, given what follows its `(`.
///
/// The first argument, or for a relative colour -- `color(from <colour>
/// <space> ...)` -- the one after the origin.
fn color_function_space(args: &str) -> Option<&str> {
    let spans = top_level_spans(args);
    let token = |at: usize| spans.get(at).map(|&(from, to)| &args[from..to]);
    match token(0) {
        Some(first) if first.eq_ignore_ascii_case("from") => token(2),
        first => first,
    }
}

/// A relative colour -- `rgb(from <origin> r g b)` -- rewritten so that
/// `csscolorparser` reads its origin as CSS means it, or `None` for anything
/// else.
///
/// `csscolorparser` keeps one sRGB value per colour, encoded with the
/// transfer function `css_to_color4f_in_space` describes, and reads a
/// relative colour's channels out of it: as stored for `rgb()` and
/// `color(srgb ...)`, and decoded by the matching inverse for the functions
/// that work in linear light. The pair cancels, so an origin
/// it built in linear light reaches a linear-light function intact -- and
/// reaches an sRGB one with its negative channel on the wrong curve, while
/// an origin written in sRGB with a negative channel reaches a linear-light
/// function on the wrong curve. An origin it cannot read at all, such as
/// `color(display-p3 ...)`, refuses the whole colour.
///
/// So the origin is resolved by this crate's own parser, which is right in
/// every space, and handed over as a literal in the space the function
/// reads: `color(srgb ...)` with the CSS encoding for the sRGB functions,
/// `color(srgb-linear ...)` for the others, whose stored encoding the
/// matching decode then undoes exactly. The origin may itself be relative,
/// and is resolved the same way. A `color()` in a space `csscolorparser`
/// does not offer is left as it is, and refused. `hsl()` and `hwb()` never
/// reach here: [`relative_hue_color`] reads them whole.
fn with_resolved_origin(css: &str) -> Option<String> {
    // UPSTREAM: csscolorparser 0.9.0 -- unfiled -- worked around
    // Re-check: return `None` here, then cargo test
    // the_rust_surface_takes_css_colors; its `rgb(from oklch(0.8 0.18 165) r g
    // b)` row on a Display P3 canvas reads 4 rather than 91 if the defect is
    // still there. The defect is the encode `css_to_color4f_in_space` repairs,
    // seen from inside a relative colour, where a repair afterwards is too
    // late.
    let (name, args) = css.trim().split_once('(')?;
    let spans = top_level_spans(args);
    let [(from, from_end), (origin, origin_end), ..] = spans[..] else {
        return None;
    };
    if !args[from..from_end].eq_ignore_ascii_case("from") {
        return None;
    }
    let linear = match name.to_ascii_lowercase().as_str() {
        "rgb" | "rgba" => false,
        "lab" | "lch" | "oklab" | "oklch" => true,
        "color" => {
            match color_function_space(args)?.to_ascii_lowercase().as_str() {
                "srgb" => false,
                "srgb-linear" | "xyz" | "xyz-d50" | "xyz-d65" => true,
                _ => return None,
            }
        }
        _ => return None,
    };
    let c = css_to_color4f(&args[origin..origin_end])?;
    let literal = match linear {
        true => format!(
            "color(srgb-linear {} {} {} / {})",
            srgb_to_linear(c.r),
            srgb_to_linear(c.g),
            srgb_to_linear(c.b),
            c.a,
        ),
        false => format!("color(srgb {} {} {} / {})", c.r, c.g, c.b, c.a),
    };
    Some(format!(
        "{name}({}{literal}{}",
        &args[..origin],
        &args[origin_end..]
    ))
}

/// What a relative `hsl()` or `hwb()` channel keyword reads saturation,
/// lightness, whiteness and blackness out of.
///
/// CSS Color 5 resolves each to a `<number>` on the scale of its percentage,
/// so `s` in `hsl(from red h s l)` is 100 rather than 1, and a percentage
/// written in its place is the same number: `50%` and `50` are one value.
const HUE_CHANNEL_SCALE: f64 = 100.0;

/// Which channel of a relative `hsl()` or `hwb()` a token is, which decides
/// what it may be written as besides a number.
#[derive(Clone, Copy)]
enum HueChannel {
    /// The hue, in degrees or as an angle.
    Hue,
    /// Saturation, lightness, whiteness or blackness, out of
    /// [`HUE_CHANNEL_SCALE`] or as a percentage.
    Scaled,
    /// The alpha, out of one or as a percentage.
    Alpha,
}

/// The hue of an sRGB colour in degrees, `[0, 360)`, as CSS Color 4's
/// `rgbToHue` finds it ([section 8.2]); zero when it has no chroma.
///
/// Unclipped, so a colour outside sRGB keeps the hue it has. The hue of a
/// colour without chroma is powerless, and a relative colour reads a
/// powerless channel as zero. Where the sample code calls a colour
/// achromatic within an epsilon, Chrome 153 does not: `hwb(from color(srgb
/// 0.5 0.5 0.500001) h 0 0)` is blue there, and is blue here.
///
/// [section 8.2]: https://www.w3.org/TR/css-color-4/#rgb-to-hwb
fn srgb_hue([r, g, b]: [f64; 3]) -> f64 {
    let max = r.max(g).max(b);
    let chroma = max - r.min(g).min(b);
    if chroma == 0.0 {
        return 0.0;
    }
    let sector = match max {
        _ if max == r => (g - b) / chroma,
        _ if max == g => (b - r) / chroma + 2.0,
        _ => (r - g) / chroma + 4.0,
    };
    (sector * 60.0).rem_euclid(DEGREES_PER_TURN)
}

/// An sRGB colour as `hsl()` channels -- hue in degrees, saturation and
/// lightness as fractions -- by CSS Color 4's `rgbToHsl` ([section 7.2]).
///
/// Unclipped. A colour far enough outside sRGB gives a negative saturation,
/// which the sample code turns into a positive one by rotating the hue half
/// a turn, and Chrome 153 does the same.
///
/// [section 7.2]: https://www.w3.org/TR/css-color-4/#rgb-to-hsl
fn srgb_to_hsl(rgb @ [r, g, b]: [f64; 3]) -> [f64; 3] {
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let lightness = (max + min) / 2.0;
    let saturation = match max == min || lightness == 0.0 || lightness == 1.0 {
        true => 0.0,
        false => (max - lightness) / lightness.min(1.0 - lightness),
    };
    let hue = srgb_hue(rgb);
    match saturation < 0.0 {
        true => [
            (hue + DEGREES_PER_TURN / 2.0).rem_euclid(DEGREES_PER_TURN),
            -saturation,
            lightness,
        ],
        false => [hue, saturation, lightness],
    }
}

/// `hsl()` channels into sRGB by CSS Color 4's `hslToRgb` ([section 7.1]),
/// with saturation and lightness as fractions and neither clipped.
///
/// [section 7.1]: https://www.w3.org/TR/css-color-4/#hsl-to-rgb
fn hsl_to_srgb(hue: f64, saturation: f64, lightness: f64) -> [f64; 3] {
    let hue = hue.rem_euclid(DEGREES_PER_TURN);
    let amount = saturation * lightness.min(1.0 - lightness);
    [0.0, 8.0, 4.0].map(|n: f64| {
        let k = (n + hue / 30.0) % 12.0;
        lightness - amount * (k - 3.0).min(9.0 - k).clamp(-1.0, 1.0)
    })
}

/// `hwb()` channels into sRGB by CSS Color 4's `hwbToRgb` ([section 8.1]),
/// with whiteness and blackness as fractions and neither clipped.
///
/// [section 8.1]: https://www.w3.org/TR/css-color-4/#hwb-to-rgb
fn hwb_to_srgb(hue: f64, white: f64, black: f64) -> [f64; 3] {
    if white + black >= 1.0 {
        return [white / (white + black); 3];
    }
    hsl_to_srgb(hue, 1.0, 0.5).map(|c| c * (1.0 - white - black) + white)
}

/// A relative `hsl()` or `hwb()` -- `hsl(from <origin> h s l)` -- read by
/// this crate rather than by `csscolorparser`, or `None` for any other
/// colour.
///
/// `Some(None)` when it is one and does not parse, which is a refusal rather
/// than a cue to read it some other way. `hwba(from ...)` is refused here:
/// Chrome 153 has no such function, and `csscolorparser` reads it as `hwb()`.
///
/// The origin is resolved by this crate's own parser, its channels found
/// from it as it is, and each channel expression evaluated by the CSS math
/// evaluator with the channel keywords bound -- so nothing is clipped to
/// sRGB anywhere between the origin and the result, which is what Chrome
/// does and what CSS Color 5 describes.
fn relative_hue_color(css: &str) -> Option<Option<Color4f>> {
    // UPSTREAM: csscolorparser 0.9.0 -- unfiled -- worked around
    // Re-check: return `None` here, then cargo test
    // the_rust_surface_takes_css_colors; on a Display P3 canvas its
    // `hsl(from oklch(0.8 0.18 165) h s l)` row reads 102 rather than 91 for
    // red, and `hsl(from color(srgb 1.2 0.2 0.1) h s l)` 235, 72, 47 rather
    // than 255, 81, 54, if `Color::to_hsla` still clamps. An sRGB canvas
    // cannot tell: both clip to the same byte there.
    //
    // `to_hsla` and `to_hwba` clamp a colour to 0..1 before converting it, so
    // `csscolorparser` reads `hsl()` and `hwb()` over an origin outside sRGB
    // as a different colour, however well the origin is resolved. Its channel
    // grammar is narrower than the standard's too: it refuses an angle unit,
    // a percentage, `calc(e)` and an infinity, and reads a negative
    // saturation as zero.
    let (name, args) = css.trim().split_once('(')?;
    let hwb = match name.to_ascii_lowercase().as_str() {
        "hsl" | "hsla" => false,
        "hwb" | "hwba" => true,
        _ => return None,
    };
    let &(from, from_end) = top_level_spans(args).first()?;
    if !args[from..from_end].eq_ignore_ascii_case("from") {
        return None;
    }
    if name.eq_ignore_ascii_case("hwba") {
        return Some(None);
    }
    Some(relative_hue_channels(args, hwb))
}

/// The colour `relative_hue_color` reads, given what follows the `(`.
fn relative_hue_channels(args: &str, hwb: bool) -> Option<Color4f> {
    let body = args.trim_end().strip_suffix(')')?.trim_end();
    let spans = top_level_spans(body);
    // A `)` before the end stops the spans short of it: `hsl(from red h s
    // l))` is refused for the second.
    if spans.last()?.1 != body.len() {
        return None;
    }
    let [_, (origin, origin_end), ..] = spans[..] else {
        return None;
    };
    let origin = css_to_color4f(&body[origin..origin_end])?;
    let rest = &body[origin_end..];

    let mut depth = 0usize;
    let slash = rest.char_indices().find_map(|(at, c)| {
        match c {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            '/' if depth == 0 => return Some(at),
            _ => {}
        }
        None
    });
    let (channels, alpha) = match slash {
        Some(at) => (&rest[..at], Some(&rest[at + 1..])),
        None => (rest, None),
    };
    fn token(text: &str, (from, to): (usize, usize)) -> &str {
        &text[from..to]
    }
    let [first, second, third] = top_level_spans(channels)[..] else {
        return None;
    };
    let alpha = match alpha.map(|text| (text, top_level_spans(text))) {
        Some((text, spans)) => match spans[..] {
            [span] => Some(token(text, span)),
            _ => return None,
        },
        None => None,
    };

    let rgb = [origin.r, origin.g, origin.b].map(f64::from);
    let [hue, x, y] = match hwb {
        true => {
            let [r, g, b] = rgb;
            [srgb_hue(rgb), r.min(g).min(b), 1.0 - r.max(g).max(b)]
        }
        false => srgb_to_hsl(rgb),
    };
    let names = match hwb {
        true => ["h", "w", "b"],
        false => ["h", "s", "l"],
    };
    let bound = [
        (names[0], hue),
        (names[1], x * HUE_CHANNEL_SCALE),
        (names[2], y * HUE_CHANNEL_SCALE),
        ("alpha", f64::from(origin.a)),
    ];
    let units = FontUnits::assumed(f32::NAN);
    let read = |text: &str, channel: HueChannel| -> Option<f64> {
        let keyword = text.to_ascii_lowercase();
        if keyword == "none" {
            return Some(0.0);
        }
        if let Some(&(_, value)) =
            bound.iter().find(|(name, _)| *name == keyword)
        {
            return Some(value);
        }
        // A percentage may appear inside a hue's math function so long as
        // the result is not one: Chrome 153 reads `calc(50% / 1%)` as a hue
        // of 50 and refuses `50%`. The dimension is what decides, below.
        let expect = Expect {
            percent: Percent::Own,
            units: &units,
            channels: &bound,
        };
        let quantity = match is_math(text) {
            true => evaluate(text, &expect)?,
            false => literal(text, &expect)?,
        };
        let value = match (channel, quantity.dimension) {
            (_, Dimension::NUMBER) | (HueChannel::Hue, Dimension::ANGLE) => {
                quantity.finished()
            }
            (HueChannel::Scaled, Dimension::PERCENT) => {
                quantity.finished() * HUE_CHANNEL_SCALE
            }
            (HueChannel::Alpha, Dimension::PERCENT) => quantity.finished(),
            _ => return None,
        };
        // CSS Values 4 section 10.9 clamps an infinity to the largest value
        // the property can hold, which for these is the `f32` the colour
        // carries. That is a whole number of turns, so an infinite hue is
        // red, as it is in Chrome 153.
        let largest = f64::from(f32::MAX);
        Some(value.clamp(-largest, largest))
    };

    let hue = read(token(channels, first), HueChannel::Hue)?;
    let x = read(token(channels, second), HueChannel::Scaled)?;
    let y = read(token(channels, third), HueChannel::Scaled)?;
    let alpha = match alpha {
        Some(text) => read(text, HueChannel::Alpha)?,
        None => f64::from(origin.a),
    };
    let (x, y) = (x / HUE_CHANNEL_SCALE, y / HUE_CHANNEL_SCALE);
    let [r, g, b] = match hwb {
        true => hwb_to_srgb(hue, x, y),
        false => hsl_to_srgb(hue, x, y),
    }
    .map(|c| c as f32);
    Some(Color4f::new(r, g, b, alpha.clamp(0.0, 1.0) as f32))
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
    if let Some(color) = parse_lab_like(css) {
        return Some((color, ColorSpace::new_srgb()));
    }
    // UPSTREAM: csscolorparser 0.9.0 -- unfiled -- worked around
    // Re-check: return `c.r`, `c.g` and `c.b` unchanged, then cargo test
    // the_rust_surface_takes_css_colors; its `oklch(0.8 0.18 165)` row on a
    // Display P3 canvas reads 4 rather than 91 if the defect is still there.
    //
    // `Color::from_linear_rgba` encodes every value below the sRGB breakpoint
    // as `12.92 * x`, negatives included, where CSS Color 4 mirrors the curve
    // through zero. A colour outside sRGB arrives with its negative channel
    // on the linear segment's slope rather than the curve's -- invisible on
    // an sRGB canvas, where both clip to zero, and a different colour on a
    // wider one. Below the breakpoint that encode is linear, so the light it
    // meant is exactly the value over the slope, and re-encoding it through
    // the mirrored curve gives the CSS answer. Only a negative channel of a
    // colour built that way is touched; see `built_from_linear_light`. A
    // relative colour reads its origin before any of this runs, so its origin
    // is resolved first; see `with_resolved_origin`.
    if let Some(color) = relative_hue_color(css) {
        return color.map(|color| (color, ColorSpace::new_srgb()));
    }
    let resolved = with_resolved_origin(css);
    let css = resolved.as_deref().unwrap_or(css);
    let from_linear = built_from_linear_light(css);
    let channel = |v: f32| match from_linear && v < 0.0 {
        true => linear_to_srgb(v / SRGB_SLOPE),
        false => v,
    };
    csscolorparser::parse(css).ok().map(|c| {
        (
            Color4f::new(channel(c.r), channel(c.g), channel(c.b), c.a),
            ColorSpace::new_srgb(),
        )
    })
}

/// Converts a colour into sRGB when it was named in some other space.
///
/// The drawing paths tag a paint with its color space and let Skia convert at
/// the draw. A gradient stop has no paint to tag -- Skia interpolates the
/// stops it is given -- so a stop has to be converted before it is stored, or
/// the components are read as sRGB and the space is silently lost.
///
/// `None`, and any space this crate cannot name, pass through: the float-array
/// form of a stop is already sRGB.
pub fn color4f_to_srgb(color: Color4f, space: Option<&ColorSpace>) -> Color4f {
    match space.and_then(color_space_name) {
        Some(name) => color_function_to_srgb(color, name),
        None => color,
    }
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
    Color::from_argb(
        (c.a.clamp(0.0, 1.0) * 255.0).round() as u8,
        linear_to_srgb_byte(c.r),
        linear_to_srgb_byte(c.g),
        linear_to_srgb_byte(c.b),
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
            .as_chunks::<2>()
            .0
            .iter()
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
    let alpha_type = opt_alpha_type_arg(cx, idx)?;

    Ok(ImageData::new(
        buffer,
        width,
        height,
        color_type,
        color_space,
        alpha_type,
    ))
}

/// The `alphaType` key of the object at `idx`, unpremultiplied when the key
/// or the object is absent -- the standard's `ImageData`, which has no such
/// key, reads as what it is.
pub fn opt_alpha_type_arg(
    cx: &mut FunctionContext,
    idx: usize,
) -> NeonResult<AlphaType> {
    match opt_object_arg(cx, idx)
        .and_then(|obj| opt_string_for_key(cx, &obj, "alphaType"))
    {
        Some(name) => alpha_type_or_throw(cx, &name),
        None => Ok(AlphaType::Unpremul),
    }
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
                color_type_or_throw(cx, &color_type)?,
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
    Option<AlphaType>,
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
            let color_type = match opt_string_for_key(cx, &obj, "colorType") {
                Some(mode) => Some(color_type_or_throw(cx, &mode)?),
                None => None,
            };
            let color_space = match opt_string_for_key(cx, &obj, "colorSpace") {
                Some(space) => Some(color_space_or_throw(cx, &space)?),
                None => None,
            };
            let matte = opt_color_for_key(cx, &obj, "matte");
            let density = opt_float_for_key(cx, &obj, "density").unwrap_or(1.0);
            let msaa = opt_float_for_key(cx, &obj, "msaa").map(|n| n as usize);
            // `None` rather than a substituted `Unpremul`, for the reason the
            // comment above gives about colorType: the caller merges, and the
            // default belongs in one place. Here that place is
            // `ExportOptions::default`, so an absent key and an explicit
            // `"unpremultiplied"` reach Skia through the same value rather
            // than two paths that have to be kept agreeing.
            let alpha_type = match opt_string_for_key(cx, &obj, "alphaType") {
                Some(mode) => Some(alpha_type_or_throw(cx, &mode)?),
                None => None,
            };
            Ok((color_type, color_space, matte, density, msaa, alpha_type))
        }
        None => Ok((None, None, None, 1.0, None, None)),
    }
}

/// Parses an `alphaType` name, or throws naming the two it accepts.
///
/// A value outside an enumeration is a `TypeError`, which is WebIDL's rule
/// for an enum whether or not the standard defines one -- the same rule
/// `colorType` and `colorSpace` follow one function above.
pub fn alpha_type_or_throw(
    cx: &mut FunctionContext,
    mode: &str,
) -> NeonResult<AlphaType> {
    match mode {
        "unpremultiplied" => Ok(AlphaType::Unpremul),
        "premultiplied" => Ok(AlphaType::Premul),
        _ => cx.throw_type_error(format!(
            "Expected \"unpremultiplied\" or \"premultiplied\" for \"alphaType\" \
             (got \"{mode}\")"
        )),
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

/// Every name `colorType` accepts, and the Skia type each one names.
///
/// One table because three places used to answer this question and all
/// three disagreed. The addon took `"N32"`, the `ColorType` union in
/// `index.d.ts` did not list it, and `pixelSize` in `imagery.js` threw on
/// it; the union listed `"RGBA8888"` twice and so did `pixelSize`'s
/// four-byte list, the two having been written from each other. The
/// binding now reads the names from here through `colorTypes()`, and a test
/// holds the union to the same list.
///
/// In the union's order -- narrowest first -- so the two read side by side.
pub const COLOR_TYPES: &[(&str, ColorType)] = &[
    ("Alpha8", ColorType::Alpha8),
    ("Gray8", ColorType::Gray8),
    ("R8UNorm", ColorType::R8UNorm),
    ("A16Float", ColorType::A16Float),
    ("A16UNorm", ColorType::A16UNorm),
    ("ARGB4444", ColorType::ARGB4444),
    ("R8G8UNorm", ColorType::R8G8UNorm),
    ("RGB565", ColorType::RGB565),
    ("rgb", ColorType::RGB888x),
    ("RGB888x", ColorType::RGB888x),
    ("rgba", ColorType::RGBA8888),
    ("RGBA8888", ColorType::RGBA8888),
    ("bgra", ColorType::BGRA8888),
    ("BGRA8888", ColorType::BGRA8888),
    ("BGR101010x", ColorType::BGR101010x),
    ("BGRA1010102", ColorType::BGRA1010102),
    ("R16G16Float", ColorType::R16G16Float),
    ("R16G16UNorm", ColorType::R16G16UNorm),
    ("RGB101010x", ColorType::RGB101010x),
    ("RGBA1010102", ColorType::RGBA1010102),
    ("SRGBA8888", ColorType::SRGBA8888),
    // Whichever 32-bit order this platform composites in -- BGRA on Apple
    // and Windows, RGBA elsewhere. It is what a canvas's surface is built
    // as, so naming it is how a caller asks for a readback that needs no
    // swizzle at all. Reading the type back reports the concrete layout,
    // because that is what the pixels turned out to be.
    ("N32", ColorType::N32),
    ("R16G16B16A16UNorm", ColorType::R16G16B16A16UNorm),
    ("RGBAF16", ColorType::RGBAF16),
    ("RGBAF16Norm", ColorType::RGBAF16Norm),
    ("RGBAF32", ColorType::RGBAF32),
];

/// The Skia type `name` names, or `None` for a name no table entry claims.
pub fn opt_color_type(name: &str) -> Option<ColorType> {
    COLOR_TYPES
        .iter()
        .find(|(named, _)| *named == name)
        .map(|(_, color_type)| *color_type)
}

/// The chroma sampling a `chromaSampling` string names.
///
/// Spelled in the `4:4:4` notation the format and every other tool use,
/// rather than in the Rust variant names -- a caller reaching for this has
/// read it in those terms, and the identifier rules that keep the enum from
/// starting with a digit do not apply to a string.
pub fn chroma_or_throw<'a, C: Context<'a>>(
    cx: &mut C,
    name: &str,
) -> NeonResult<ChromaSampling> {
    match name {
        "4:4:4" => Ok(ChromaSampling::Full),
        "4:2:2" => Ok(ChromaSampling::Half),
        "4:2:0" => Ok(ChromaSampling::Quarter),
        // A `TypeError`, as WebIDL specifies for a value outside an
        // enumeration and as the four other unrecognised-name sites in this
        // file already raise. It was a `RangeError`, which reads as a claim
        // that these three are ordered and that `"4:1:1"` sits outside them.
        _ => cx.throw_type_error(format!(
            "Expected one of \"4:4:4\", \"4:2:2\" or \"4:2:0\" for \
             `chromaSampling` (got \"{name}\")"
        )),
    }
}

/// As [`opt_color_type`], throwing rather than substituting.
///
/// The substitution is why this exists. Every unrecognised name used to
/// become `RGBA8888`, so `new Canvas(w, h, {colorType: "rgba8888"})` --
/// the right type, the wrong case -- silently built the default and
/// reported it back as `"rgba"`, and a typo could not be told from a
/// choice. The export path already threw, from `pixelSize` on the
/// JavaScript side, so the same bad value was a `TypeError` in one place
/// and a shrug in the other.
///
/// The name is matched case-sensitively, unlike [`enum_arg_or`], and
/// deliberately: the names here are wire-format identifiers -- `RGBA8888`,
/// `RGBAF16`, `N32` -- where the capitalisation is part of how the format is
/// written down, and folding it would accept `rgbaf16` as a spelling nothing
/// else uses. `enum_arg_or` takes CSS keywords, which CSS itself matches
/// without regard to case, so the two conventions differ because the
/// vocabularies do.
pub fn color_type_or_throw<'a, C: Context<'a>>(
    cx: &mut C,
    name: &str,
) -> NeonResult<ColorType> {
    match opt_color_type(name) {
        Some(color_type) => Ok(color_type),
        None => cx.throw_type_error(format!("Unknown colorType: {name}")),
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
        // `COLOR_TYPES` accepts both.
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

use crate::{
    context::page::ExportOptions,
    export::{ChromaSampling, ImageFormat},
};

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
    // Refused here rather than deep in the encoder, which used to report an
    // unknown format only once a surface had been rasterized for it.
    let named = string_for_key(cx, &opts, "format")?;
    let format = match ImageFormat::from_name(&named) {
        Some(format) => format,
        None => {
            return cx.throw_type_error(format!(
                "Expected one of {} for `format` (got \"{}\")",
                ImageFormat::names().join(", "),
                named
            ));
        }
    };
    let quality = float_for_key(cx, &opts, "quality")?;
    let density = float_for_key(cx, &opts, "density")?;
    let jpeg_downsample = bool_for_key(cx, &opts, "downsample")?;
    let matte = opt_color_for_key(cx, &opts, "matte");
    let msaa =
        opt_float_for_key(cx, &opts, "msaa").map(|num| num.floor() as usize);
    let color_type = match opt_string_for_key(cx, &opts, "colorType") {
        Some(mode) => color_type_or_throw(cx, &mode)?,
        None => defaults.color_type,
    };
    // Refused here for the same reason `format` is: the alternative is
    // rasterizing a page and only then discovering that the depth asked for
    // is not one the chosen format codes.
    let bit_depth = match opt_float_for_key(cx, &opts, "bitDepth") {
        None => None,
        Some(bits) => {
            let taken = format.bit_depths();
            if taken.is_empty() {
                return cx.throw_type_error(format!(
                    "\"{}\" takes its depth from the canvas -- pass \
                     `colorType` instead of `bitDepth`",
                    format.as_str()
                ));
            }
            if !taken.contains(&(bits as u8)) || bits.fract() != 0.0 {
                return cx.throw_range_error(format!(
                    "Expected one of {} for `bitDepth` of \"{}\" (got {bits})",
                    taken
                        .iter()
                        .map(u8::to_string)
                        .collect::<Vec<_>>()
                        .join(", "),
                    format.as_str()
                ));
            }
            Some(bits as u8)
        }
    };
    // Refused here for the same reason `bitDepth` is, and against the same
    // question: AVIF is the only format that offers the choice, and JPEG's
    // own subsampling switch is the separate `jpegDownsample` boolean.
    let chroma = match opt_string_for_key(cx, &opts, "chromaSampling") {
        None => None,
        Some(name) => {
            if format != ImageFormat::Avif {
                return cx.throw_type_error(format!(
                    "\"{}\" does not choose its chroma sampling -- only \
                     \"avif\" takes `chromaSampling`",
                    format.as_str()
                ));
            }
            Some(chroma_or_throw(cx, &name)?)
        }
    };
    // Refused for the same reason `chromaSampling` is, and additionally
    // against subsampling: the two cannot both be honoured.
    let lossless = match opt_bool_for_key(cx, &opts, "lossless") {
        None | Some(false) => false,
        Some(true) => {
            if format != ImageFormat::Avif {
                return cx.throw_type_error(format!(
                    "\"{}\" is either lossless already or has no lossless \
                     form -- only \"avif\" takes `lossless`",
                    format.as_str()
                ));
            }
            if matches!(
                chroma,
                Some(ChromaSampling::Half | ChromaSampling::Quarter)
            ) {
                return cx.throw_type_error(
                    "Subsampled chroma discards colour before the encoder \
                     sees it, so `lossless` cannot be combined with a \
                     `chromaSampling` other than \"4:4:4\"",
                );
            }
            true
        }
    };
    let text_contrast = float_for_key(cx, &opts, "textContrast")?;
    let text_gamma = float_for_key(cx, &opts, "textGamma")?;
    let outline = bool_for_key(cx, &opts, "outline")?;
    // Only `toBuffer("raw")` reads this: an encoder is handed an image and
    // codes whatever alpha mode it requires, so asking for premultiplied PNG
    // bytes would name something the format does not have. Refusing it here
    // would be a second rule about which options belong to which format,
    // where the raw path already ignores `quality` and the encoders already
    // ignore this. Absent means the default, which is `Unpremul`.
    let alpha_type = match opt_string_for_key(cx, &opts, "alphaType") {
        Some(mode) => alpha_type_or_throw(cx, &mode)?,
        None => defaults.alpha_type,
    };

    // Animation timing. `fps` sets a uniform rate; `frameDelays` names each
    // frame's own duration and wins when it has one entry per page, which is
    // what lets an animation read out of an `Image` be written back with the
    // timing it arrived with.
    // Left as `None` when the caller did not name one, so a rate given to
    // a format with no clock can be refused rather than ignored.
    let fps = opt_float_for_key(cx, &opts, "fps");
    let frame_delays = match opt_array_for_key(cx, &opts, "frameDelays") {
        Some(values) => {
            let values = values.to_vec(cx)?;
            let mut delays = Vec::with_capacity(values.len());
            for (index, value) in values.iter().enumerate() {
                // Refused rather than defaulted. This read anything that was
                // not a number as a zero-length frame, which is what a hole
                // in a sparse array arrives as -- so `frameDelays` built by
                // assigning into a `new Array(n)` retimed the animation to
                // nothing and reported success.
                let Ok(number) = value.downcast::<JsNumber, _>(cx) else {
                    return cx.throw_type_error(format!(
                        "Expected a number for `frameDelays[{index}]`"
                    ));
                };
                let delay = number.value(cx);
                if !delay.is_finite() || delay < 0.0 {
                    return cx.throw_range_error(format!(
                        "Expected a non-negative number for \
                         `frameDelays[{index}]` (got {delay})"
                    ));
                }
                delays.push(delay as u32);
            }
            delays
        }
        None => Vec::new(),
    };
    // Zero is how both formats spell "forever", and the argument is named
    // after the GIF and APNG fields rather than after this crate's `Option`.
    let loops = opt_float_for_key(cx, &opts, "loop")
        .map(|count| count.max(0.0) as u32)
        .filter(|count| *count > 0);

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
        bit_depth,
        chroma,
        lossless,
        color_space,
        alpha_type,
        surface_color_space: defaults.surface_color_space.clone(),
        // As with the space: `colorType` above is what this call reads back
        // in, while the surface keeps the format the canvas was built with.
        surface_color_type: defaults.surface_color_type,
        jpeg_downsample,
        text_contrast,
        text_gamma,
        fps,
        frame_delays,
        loops,
        ..ExportOptions::default()
    })
}

//
// Images
//

use crate::node::image::Source;

/// Reads an argument that is something to paint, for a verb declared to take
/// one. `None` where the argument is not an image, which the verb answers for.
pub fn opt_image_source_arg(
    cx: &mut FunctionContext,
    idx: usize,
) -> Option<Source> {
    let arg = cx.argument_opt(idx)?;
    Source::of(cx, arg)
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

use crate::node::filter::SamplingQuality;

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
        // Rule 1: the standard names the exception -- "If repetition is not
        // identical to one of "repeat", "repeat-x", "repeat-y", or
        // "no-repeat", then throw a "SyntaxError" DOMException." The name
        // crosses as text and `lib/classes/neon.js` builds the exception.
        None => cx.throw_error(
            "SyntaxError: Expected `repetition` to be \"repeat\", \"repeat-x\", \"repeat-y\", or \"no-repeat\"",
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

use crate::paint::StrokeCap;
use skia_safe::PaintCap;

/// The crate's [`StrokeCap`] a `lineCap` name asks for.
///
/// Returns the crate's type rather than Skia's, so the binding and a Rust
/// caller name the same three caps -- and a name the crate has no word for
/// cannot be reached from JavaScript. The three arms are the whole enum.
pub fn to_stroke_cap(mode_name: &str) -> Option<StrokeCap> {
    let mode = match mode_name.to_lowercase().as_str() {
        "butt" => StrokeCap::Butt,
        "round" => StrokeCap::Round,
        "square" => StrokeCap::Square,
        _ => return None,
    };
    Some(mode)
}

pub fn from_stroke_cap(mode: PaintCap) -> String {
    match StrokeCap::from_skia(mode) {
        StrokeCap::Butt => "butt",
        StrokeCap::Round => "round",
        StrokeCap::Square => "square",
    }
    .to_string()
}

use crate::paint::StrokeJoin;
use skia_safe::PaintJoin;

/// The crate's [`StrokeJoin`] a `lineJoin` name asks for.
///
/// As [`to_stroke_cap`]: the crate's type, and the three arms are the whole
/// enum.
pub fn to_stroke_join(mode_name: &str) -> Option<StrokeJoin> {
    let mode = match mode_name.to_lowercase().as_str() {
        "miter" => StrokeJoin::Miter,
        "round" => StrokeJoin::Round,
        "bevel" => StrokeJoin::Bevel,
        _ => return None,
    };
    Some(mode)
}

pub fn from_stroke_join(mode: PaintJoin) -> String {
    match StrokeJoin::from_skia(mode) {
        StrokeJoin::Miter => "miter",
        StrokeJoin::Round => "round",
        StrokeJoin::Bevel => "bevel",
    }
    .to_string()
}

use crate::paint::BlendMode as CrateBlend;
use skia_safe::BlendMode as SkBlend;
pub fn to_blend_mode(mode_name: &str) -> Option<CrateBlend> {
    let mode = match mode_name.to_lowercase().as_str() {
        "source-over" => CrateBlend::SourceOver,
        "destination-over" => CrateBlend::DestinationOver,
        "copy" => CrateBlend::Copy,
        "destination" => CrateBlend::Destination,
        "clear" => CrateBlend::Clear,
        "modulate" => CrateBlend::Modulate,
        "source-in" => CrateBlend::SourceIn,
        "destination-in" => CrateBlend::DestinationIn,
        "source-out" => CrateBlend::SourceOut,
        "destination-out" => CrateBlend::DestinationOut,
        "source-atop" => CrateBlend::SourceAtop,
        "destination-atop" => CrateBlend::DestinationAtop,
        "xor" => CrateBlend::Xor,
        "lighter" => CrateBlend::Lighter,
        "multiply" => CrateBlend::Multiply,
        "screen" => CrateBlend::Screen,
        "overlay" => CrateBlend::Overlay,
        "darken" => CrateBlend::Darken,
        "lighten" => CrateBlend::Lighten,
        "color-dodge" => CrateBlend::ColorDodge,
        "color-burn" => CrateBlend::ColorBurn,
        "hard-light" => CrateBlend::HardLight,
        "soft-light" => CrateBlend::SoftLight,
        "difference" => CrateBlend::Difference,
        "exclusion" => CrateBlend::Exclusion,
        "hue" => CrateBlend::Hue,
        "saturation" => CrateBlend::Saturation,
        "color" => CrateBlend::Color,
        "luminosity" => CrateBlend::Luminosity,
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
pub fn to_filter_blend_mode(mode_name: &str) -> Option<CrateBlend> {
    if let Some(mode) = to_blend_mode(mode_name) {
        return Some(mode);
    }

    Some(match mode_name.to_lowercase().as_str() {
        "src" | "source" => CrateBlend::Copy,
        "dst" => CrateBlend::Destination,
        "srcover" | "src-over" => CrateBlend::SourceOver,
        "dstover" | "dst-over" => CrateBlend::DestinationOver,
        "srcin" | "src-in" => CrateBlend::SourceIn,
        "dstin" | "dst-in" => CrateBlend::DestinationIn,
        "srcout" | "src-out" => CrateBlend::SourceOut,
        "dstout" | "dst-out" => CrateBlend::DestinationOut,
        "srcatop" | "src-atop" => CrateBlend::SourceAtop,
        "dstatop" | "dst-atop" => CrateBlend::DestinationAtop,
        "plus" | "plus-lighter" => CrateBlend::Lighter,
        "colordodge" => CrateBlend::ColorDodge,
        "colorburn" => CrateBlend::ColorBurn,
        "hardlight" => CrateBlend::HardLight,
        "softlight" => CrateBlend::SoftLight,
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
) -> NeonResult<CrateBlend> {
    let name = string_arg(cx, idx, attr)?;
    match to_filter_blend_mode(&name) {
        Some(mode) => Ok(mode),
        // The same sentence as `enum_arg`'s, deliberately without the list of
        // permitted values that one carries. `enum_arg` is handed a table and
        // can name its entries; this reads a hand-written match of about
        // thirty spellings, half of them aliases, so a list here would be
        // written out by hand and would drift the first time one was added.
        // The `BlendMode` type in `lib/index.d.ts` is the list.
        None => cx.throw_type_error(format!("Unknown {attr} \"{name}\"")),
    }
}

/// The `globalCompositeOperation` name for a blend mode.
///
/// Takes Skia's value, because that is what the paint state holds, and
/// names it through the crate's [`BlendMode`] so the spelling has one
/// definition rather than two.
///
/// [`BlendMode`]: crate::paint::BlendMode
pub fn from_blend_mode(mode: SkBlend) -> String {
    match CrateBlend::from_skia(mode) {
        CrateBlend::SourceOver => "source-over",
        CrateBlend::DestinationOver => "destination-over",
        CrateBlend::Copy => "copy",
        CrateBlend::Destination => "destination",
        CrateBlend::Clear => "clear",
        CrateBlend::Modulate => "modulate",
        CrateBlend::SourceIn => "source-in",
        CrateBlend::DestinationIn => "destination-in",
        CrateBlend::SourceOut => "source-out",
        CrateBlend::DestinationOut => "destination-out",
        CrateBlend::SourceAtop => "source-atop",
        CrateBlend::DestinationAtop => "destination-atop",
        CrateBlend::Xor => "xor",
        CrateBlend::Lighter => "lighter",
        CrateBlend::Multiply => "multiply",
        CrateBlend::Screen => "screen",
        CrateBlend::Overlay => "overlay",
        CrateBlend::Darken => "darken",
        CrateBlend::Lighten => "lighten",
        CrateBlend::ColorDodge => "color-dodge",
        CrateBlend::ColorBurn => "color-burn",
        CrateBlend::HardLight => "hard-light",
        CrateBlend::SoftLight => "soft-light",
        CrateBlend::Difference => "difference",
        CrateBlend::Exclusion => "exclusion",
        CrateBlend::Hue => "hue",
        CrateBlend::Saturation => "saturation",
        CrateBlend::Color => "color",
        CrateBlend::Luminosity => "luminosity",
    }
    .to_string()
}

use crate::path::PathOp;

/// The crate's [`PathOp`] a JavaScript operator name asks for.
///
/// Returns the crate's type rather than Skia's, so the binding names an
/// operation the same way a Rust caller does and cannot reach one the crate
/// has no word for.
pub fn to_path_op(op_name: &str) -> Option<PathOp> {
    let op = match op_name.to_lowercase().as_str() {
        "difference" => PathOp::Difference,
        "intersect" => PathOp::Intersect,
        "union" => PathOp::Union,
        "xor" => PathOp::Xor,
        "reversedifference" | "complement" => PathOp::Complement,
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

use crate::path::FillRule;

/// The crate's [`FillRule`] a `fillRule` argument asks for.
///
/// Returns the crate's type rather than Skia's `PathFillType`, which has two
/// further variants -- the inverse fills -- that no Canvas name reaches. The
/// two arms here are the whole of [`FillRule`].
pub fn fill_rule_arg_or(
    cx: &mut FunctionContext,
    idx: usize,
    default: &str,
) -> NeonResult<FillRule> {
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
        "nonzero" => Ok(FillRule::NonZero),
        "evenodd" => Ok(FillRule::EvenOdd),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_argument_index_gets_the_ordinal_english_gives_it() {
        for (n, want) in [
            (0usize, "0th"),
            (1, "1st"),
            (2, "2nd"),
            (3, "3rd"),
            (4, "4th"),
            (11, "11th"),
            (12, "12th"),
            (13, "13th"),
            (21, "21st"),
            (22, "22nd"),
            (23, "23rd"),
            (101, "101st"),
            (111, "111th"),
            (113, "113th"),
            (121, "121st"),
        ] {
            assert_eq!(arg_num(n), want);
        }
    }

    #[test]
    fn the_new_ordinal_agrees_with_the_arithmetic_it_replaced() {
        // The old expression was correct and unreadable. This checks the
        // rewrite is a rewrite rather than a fix, over every index a
        // message could plausibly carry.
        let old = |n: usize| {
            let ords = ["st", "nd", "rd"];
            let slot = ((n + 90) % 100) as i64 - 10;
            let slot = (slot % 10) - 1;
            let suffix = match (0..=2).contains(&slot) {
                true => ords[slot as usize],
                false => "th",
            };
            format!("{n}{suffix}")
        };
        for n in 0..1000usize {
            assert_eq!(arg_num(n), old(n), "index {n}");
        }
    }

    /// Every arm of the two blend parsers is reachable.
    ///
    /// Both match on `mode_name.to_lowercase()`, so an arm whose literal
    /// carries a capital can never match. That is not hypothetical:
    /// [`enum_arg_or`] carries a `debug_assert!` against exactly this for
    /// table-driven paths, and its comment records that it is "how the camel
    /// case blend names went unreachable for as long as they were
    /// advertised". These two are `match` arms rather than tables, so that
    /// assertion cannot reach them -- and a dead arm raises no warning,
    /// fails no test, and reads as a supported spelling to anyone reading
    /// the source.
    ///
    /// The arms are read out of this file rather than listed here, because a
    /// list would be a second copy to maintain and would go stale in the
    /// direction that hides the defect.
    #[test]
    fn every_blend_arm_is_reachable() {
        /// The arm literals of one `fn`, read from the source between its
        /// signature and the next item. Doc comments are skipped: the one
        /// above `to_filter_blend_mode` names `srcOver` in prose, which is
        /// not an arm.
        fn arm_literals(source: &str, signature: &str) -> Vec<String> {
            let body = source
                .split_once(signature)
                .expect("the function is in this file")
                .1;
            let body =
                body.split_once("\npub fn ").map_or(body, |(head, _)| head);
            body.lines()
                .map(str::trim)
                .filter(|line| !line.starts_with("//") && line.contains("=>"))
                .flat_map(|line| {
                    let head =
                        line.split_once("=>").map_or(line, |(head, _)| head);
                    head.split('"')
                        .skip(1)
                        .step_by(2)
                        .map(str::to_string)
                        .collect::<Vec<_>>()
                })
                .collect()
        }

        let source = include_str!("utils.rs");
        for (signature, parse) in [
            (
                "pub fn to_blend_mode(mode_name: &str)",
                &to_blend_mode as &dyn Fn(&str) -> Option<CrateBlend>,
            ),
            (
                "pub fn to_filter_blend_mode(mode_name: &str)",
                &to_filter_blend_mode as &dyn Fn(&str) -> Option<CrateBlend>,
            ),
        ] {
            let arms = arm_literals(source, signature);
            // The floor is the guard on the reader itself: a scan that stops
            // finding arms would otherwise pass by having nothing to check.
            assert!(
                arms.len() >= 20,
                "{signature} should have found the arm literals, got {arms:?}"
            );
            for arm in arms {
                let Some(mode) = parse(&arm) else {
                    panic!(
                        "`{arm}` is an arm of {signature} and cannot be \
                         matched -- the parser folds to lowercase first"
                    );
                };
                // And the crate enum these now produce survives the trip
                // through Skia's, which is what the call sites do with it.
                // Every arm is checked rather than a list of variants,
                // because the arms reach all of them and a list would be a
                // second copy to maintain.
                assert_eq!(
                    CrateBlend::from_skia(mode.to_skia()),
                    mode,
                    "`{arm}` does not survive to_skia then from_skia"
                );
            }
        }
    }
}
