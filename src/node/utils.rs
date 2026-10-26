#![allow(dead_code)]
use core::ops::Range;
use neon::prelude::*;
use skia_safe::{Color, Color4f, ColorSpace, Data, Matrix, Path, Point, RGB};
use std::{borrow::Cow, cmp};

// One definition of the sRGB transfer function for the whole crate, in
// `color`. A copy here would have to agree with it on every constant and on
// how a negative component extends through zero, and nothing would check
// that it did.
use crate::{
    color::{RgbaLinear, linear_to_srgb, srgb_to_linear},
    css::{
        DEGREES_PER_TURN, Dimension, Expect, FontUnits, Percent, Quantity,
        evaluate, is_math, literal, without_comments,
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

/// The exponent of `rec2020`'s transfer function.
///
/// CSS Color 4 section 10.8 gives the function as "gamma 2.40, from
/// [REC_BT.1886]": BT.1886's reference EOTF with black lift `b = 0` and gain
/// `a = 1`, which is a pure power. Its sample code applies it to the
/// magnitude and keeps the sign, so a component below zero reaches a colour
/// outside the gamut rather than being clipped. Fixed by the standard, not
/// chosen; drafts before the Candidate Recommendation Draft of 24 April 2025
/// used BT.2020's piecewise inverse OETF instead.
const REC2020_GAMMA: f32 = 2.4;

/// `rec2020`'s transfer function, CSS Color 4's `lin_2020`.
fn rec2020_decode(encoded: f32) -> f32 {
    encoded.abs().powf(REC2020_GAMMA).copysign(encoded)
}

/// `rec2020`'s encode, CSS Color 4's `gam_2020`: the inverse of
/// [`rec2020_decode`].
fn rec2020_encode(linear: f32) -> f32 {
    linear.abs().powf(REC2020_GAMMA.recip()).copysign(linear)
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

/// A98 RGB's exponent, which Adobe RGB (1998) specifies as 2 51/256.
const A98_EXPONENT: f32 = 563.0 / 256.0;

/// A98 RGB's transfer function, CSS Color 4's `lin_a98rgb`: a pure power,
/// mirrored through zero so a negative component keeps its sign.
fn a98_decode(encoded: f32) -> f32 {
    encoded.abs().powf(A98_EXPONENT).copysign(encoded)
}

/// A98 RGB's encode, CSS Color 4's `gam_a98rgb`.
fn a98_encode(linear: f32) -> f32 {
    linear.abs().powf(1.0 / A98_EXPONENT).copysign(linear)
}

/// Where ProPhoto RGB's linear toe ends, on the encoded side: CSS Color 4's
/// `Et2`.
const PROPHOTO_TOE: f32 = 16.0 / 512.0;
/// The toe's slope, as the encoded value over the linear one.
const PROPHOTO_TOE_SLOPE: f32 = 16.0;
/// ProPhoto RGB's power above the toe.
const PROPHOTO_EXPONENT: f32 = 1.8;

/// ProPhoto RGB's transfer function, CSS Color 4's `lin_ProPhoto`: a 1.8
/// power above a short linear toe, mirrored through zero.
fn prophoto_decode(encoded: f32) -> f32 {
    let magnitude = encoded.abs();
    let linear = match magnitude <= PROPHOTO_TOE {
        true => magnitude / PROPHOTO_TOE_SLOPE,
        false => magnitude.powf(PROPHOTO_EXPONENT),
    };
    linear.copysign(encoded)
}

/// ProPhoto RGB's encode, CSS Color 4's `gam_ProPhoto`, mirrored through
/// zero as the decode is. The toe ends at `Et`, 1/512, on the linear side,
/// which is where it ends on the encoded side over its slope.
fn prophoto_encode(linear: f32) -> f32 {
    let magnitude = linear.abs();
    let encoded = match magnitude >= PROPHOTO_TOE / PROPHOTO_TOE_SLOPE {
        true => magnitude.powf(1.0 / PROPHOTO_EXPONENT),
        false => magnitude * PROPHOTO_TOE_SLOPE,
    };
    encoded.copysign(linear)
}

/// `m` times `v`.
fn apply_matrix(m: [[f32; 3]; 3], v: [f32; 3]) -> [f32; 3] {
    m.map(|row| row[0] * v[0] + row[1] * v[1] + row[2] * v[2])
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
/// light in sRGB's primaries, unbounded, which is what `RgbaLinear` means on
/// every canvas.
///
/// Through extended sRGB, so a colour named in any space this crate can name
/// keeps its negative or above-one components rather than being clipped. An
/// untagged colour is sRGB, as Skia reads one.
pub(crate) fn color4f_to_rgba_linear(
    color: Color4f,
    space: Option<&ColorSpace>,
) -> RgbaLinear {
    let srgb = match space.and_then(color_space_name) {
        Some(name) => color_function_to_srgb(color, name),
        None => color,
    };
    RgbaLinear::from_srgb(srgb.r, srgb.g, srgb.b, srgb.a)
}

/// Converts `color()` components into extended sRGB.
///
/// For the callers that need a colour in sRGB rather than tagged with its
/// space: a gradient stop, an 8-bit `SkColor`, a getter's report,
/// `RgbaLinear::from_css`, and a colour in a space no canvas holds, `xyz`
/// among them. A paint keeps the colour in the space it was named
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
        "xyz" | "xyz-d65" => {
            apply(XYZ_D65_TO_LINEAR_SRGB, [color.r, color.g, color.b])
                .map(linear_to_srgb)
        }
        "xyz-d50" => apply(
            XYZ_D65_TO_LINEAR_SRGB,
            apply(BRADFORD_D50_TO_D65, [color.r, color.g, color.b]),
        )
        .map(linear_to_srgb),
        _ => return color,
    };
    Color4f::new(r, g, b, color.a)
}

/// Extended sRGB into the components `color()` names in `space`, the
/// inverse of [`color_function_to_srgb`]: into linear light, through the
/// inverse of the matrix that space converts by, and out through its own
/// transfer function. `None` for a space `color()` does not name.
fn srgb_to_color_function(rgb: [f32; 3], space: &str) -> Option<[f32; 3]> {
    let linear = rgb.map(srgb_to_linear);
    let through = |m: [[f32; 3]; 3], encode: fn(f32) -> f32| {
        apply_matrix(inverse(m), linear).map(encode)
    };
    Some(match space {
        "srgb" => rgb,
        "srgb-linear" => linear,
        "display-p3" => through(LINEAR_P3_TO_LINEAR_SRGB, linear_to_srgb),
        "rec2020" => through(LINEAR_REC2020_TO_LINEAR_SRGB, rec2020_encode),
        "a98-rgb" => through(LINEAR_A98_TO_LINEAR_SRGB, a98_encode),
        "prophoto-rgb" => {
            through(LINEAR_PROPHOTO_TO_LINEAR_SRGB, prophoto_encode)
        }
        "xyz" | "xyz-d65" => {
            apply_matrix(inverse(XYZ_D65_TO_LINEAR_SRGB), linear)
        }
        "xyz-d50" => apply_matrix(
            inverse(BRADFORD_D50_TO_D65),
            apply_matrix(inverse(XYZ_D65_TO_LINEAR_SRGB), linear),
        ),
        _ => return None,
    })
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
/// [`lab_to_srgb`] says why this crate converts Lab itself.
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

/// CIE Lab into extended sRGB: to XYZ against Lab's own D50 white, CSS
/// Color 4 section 12.1, then [`BRADFORD_D50_TO_D65`] and the sRGB matrix,
/// clipping nothing, a lightness outside `0..=100` included.
///
/// Over a grid of every form the `lab()` and `lch()` grammar takes, 17,712
/// inputs, this lands within 8.5e-6 of the specification's own conversion
/// done in `f64`, which `lab_and_lch_follow_css_color_4_across_their_grammar`
/// holds it to; a conversion within 2.2e-3 puts 121 of them a whole 8-bit
/// level off, one of which `the_rust_surface_takes_css_colors` pins against
/// Chrome.
fn lab_to_srgb([l, a, b]: [f32; 3]) -> [f32; 3] {
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
    apply_matrix(
        XYZ_D65_TO_LINEAR_SRGB,
        apply_matrix(BRADFORD_D50_TO_D65, xyz_d50),
    )
    .map(linear_to_srgb)
}

/// Extended sRGB into CIE Lab, CSS Color 4's `XYZ_to_Lab` after the
/// inverses of the matrices [`lab_to_srgb`] applies, so the two are a pair.
fn srgb_to_lab(rgb: [f32; 3]) -> [f32; 3] {
    let xyz_d50 = apply_matrix(
        inverse(BRADFORD_D50_TO_D65),
        apply_matrix(inverse(XYZ_D65_TO_LINEAR_SRGB), rgb.map(srgb_to_linear)),
    );
    let [fx, fy, fz] = std::array::from_fn(|i| {
        let scaled = xyz_d50[i] / LAB_WHITE_D50[i];
        match scaled > LAB_EPSILON {
            true => scaled.cbrt(),
            false => (LAB_KAPPA * scaled + 16.0) / 116.0,
        }
    });
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

/// XYZ with a D65 white into Oklab's cone responses: the first matrix of
/// CSS Color 4's `XYZ_to_OKLab` ([section 19]), recalculated there for a
/// consistent white.
///
/// [section 19]: https://www.w3.org/TR/css-color-4/#color-conversion-code
const XYZ_D65_TO_OKLAB_LMS: [[f32; 3]; 3] = [
    [0.819_022_4, 0.361_906_26, -0.128_873_78],
    [0.032_983_654, 0.929_286_86, 0.036_144_666],
    [0.048_177_19, 0.264_239_53, 0.633_547_8],
];

/// Oklab's cube-rooted cone responses into `L`, `a` and `b`, the second
/// matrix of CSS Color 4's `XYZ_to_OKLab`.
const OKLAB_LMS_TO_OKLAB: [[f32; 3]; 3] = [
    [0.210_454_27, 0.793_617_8, -0.004_072_043],
    [1.977_998_5, -2.428_592_2, 0.450_593_7],
    [0.025_904_042, 0.782_771_7, -0.808_675_77],
];

/// Extended sRGB into Oklab, by CSS Color 4's `XYZ_to_OKLab`. The cube root
/// keeps its sign, as the sample code warns a port must.
fn srgb_to_oklab(rgb: [f32; 3]) -> [f32; 3] {
    let xyz =
        apply_matrix(inverse(XYZ_D65_TO_LINEAR_SRGB), rgb.map(srgb_to_linear));
    apply_matrix(
        OKLAB_LMS_TO_OKLAB,
        apply_matrix(XYZ_D65_TO_OKLAB_LMS, xyz).map(f32::cbrt),
    )
}

/// CSS Color 4's just noticeable difference, in deltaEOK: section 14.2.1,
/// "For the OkLCh color space, one JND is is an OkLCh difference of 0.02". A
/// clipped colour within it of the colour it stands for is taken as it is.
const GAMUT_MAP_JND: f64 = 0.02;

/// The chroma resolution section 14.2.2's binary search stops at: "let
/// epsilon be 0.0001".
const GAMUT_MAP_EPSILON: f64 = 0.0001;

/// Linear-light sRGB into Oklab, in `f64`, by the matrices
/// [`srgb_to_oklab`] applies.
fn linear_srgb_to_oklab_f64(rgb: [f64; 3]) -> [f64; 3] {
    let widen = |m: [[f32; 3]; 3]| m.map(|row| row.map(f64::from));
    let apply = |m: [[f64; 3]; 3], v: [f64; 3]| {
        m.map(|row| row[0] * v[0] + row[1] * v[1] + row[2] * v[2])
    };
    let xyz = apply(widen(inverse(XYZ_D65_TO_LINEAR_SRGB)), rgb);
    let lms = apply(widen(XYZ_D65_TO_OKLAB_LMS), xyz).map(f64::cbrt);
    apply(widen(OKLAB_LMS_TO_OKLAB), lms)
}

/// Oklab into linear-light sRGB, in `f64`, the inverse of
/// [`linear_srgb_to_oklab_f64`].
fn oklab_to_linear_srgb_f64(lab: [f64; 3]) -> [f64; 3] {
    let widen = |m: [[f32; 3]; 3]| m.map(|row| row.map(f64::from));
    let apply = |m: [[f64; 3]; 3], v: [f64; 3]| {
        m.map(|row| row[0] * v[0] + row[1] * v[1] + row[2] * v[2])
    };
    let lms = apply(widen(inverse(OKLAB_LMS_TO_OKLAB)), lab).map(|c| c * c * c);
    let xyz = apply(widen(inverse(XYZ_D65_TO_OKLAB_LMS)), lms);
    apply(widen(XYZ_D65_TO_LINEAR_SRGB), xyz)
}

/// Linear-light sRGB components, inside sRGB's gamut or not, mapped into it
/// by CSS Color 4 section 14.2.2's binary search with local MINDE: chroma is
/// reduced in OkLCh, holding lightness and hue, until clipping what is left
/// moves it by less than [`GAMUT_MAP_JND`]. For the Skia calls that take an
/// eight-bit sRGB colour and nothing wider, where clipping each channel on
/// its own would shift a wide colour's hue -- a P3 orange clips towards
/// yellow.
///
/// Returns the components unchanged where they are already inside the
/// gamut, so the search runs only for a colour outside it, and only where
/// a colour is set, not as it is drawn: about twenty halvings of the chroma
/// at most, each two Oklab conversions.
pub(crate) fn gamut_map_srgb(linear: [f32; 3]) -> [f32; 3] {
    let in_gamut =
        |c: [f64; 3]| c.iter().all(|channel| (0.0..=1.0).contains(channel));
    let origin = linear.map(f64::from);
    if in_gamut(origin) {
        return linear;
    }
    let [lightness, a, b] = linear_srgb_to_oklab_f64(origin);
    // Steps 3 and 4: at or past either end of lightness, white or black.
    if lightness >= 1.0 {
        return [1.0; 3];
    }
    if lightness <= 0.0 {
        return [0.0; 3];
    }
    let chroma = a.hypot(b);
    let at = |reduced: f64| {
        let scale = reduced / chroma;
        oklab_to_linear_srgb_f64([lightness, a * scale, b * scale])
    };
    let clip = |c: [f64; 3]| c.map(|channel| channel.clamp(0.0, 1.0));
    let delta = |one: [f64; 3], two: [f64; 3]| {
        let [l1, a1, b1] = linear_srgb_to_oklab_f64(one);
        let [l2, a2, b2] = linear_srgb_to_oklab_f64(two);
        ((l1 - l2).powi(2) + (a1 - a2).powi(2) + (b1 - b2).powi(2)).sqrt()
    };
    let mut clipped = clip(origin);
    if delta(clipped, origin) < GAMUT_MAP_JND {
        return clipped.map(|channel| channel as f32);
    }
    let (mut min, mut max, mut min_in_gamut) = (0.0, chroma, true);
    while max - min > GAMUT_MAP_EPSILON {
        let reduced = (min + max) / 2.0;
        let current = at(reduced);
        if min_in_gamut && in_gamut(current) {
            min = reduced;
            continue;
        }
        clipped = clip(current);
        let error = delta(clipped, current);
        if error < GAMUT_MAP_JND {
            if GAMUT_MAP_JND - error < GAMUT_MAP_EPSILON {
                break;
            }
            min_in_gamut = false;
            min = reduced;
        } else {
            max = reduced;
        }
    }
    clipped.map(|channel| channel as f32)
}

/// Oklab into extended sRGB, by the inverses of the matrices
/// [`srgb_to_oklab`] applies, so the two are a pair -- CSS Color 4 publishes
/// its own inverses, which agree with these to their precision.
fn oklab_to_srgb(oklab: [f32; 3]) -> [f32; 3] {
    let lms =
        apply_matrix(inverse(OKLAB_LMS_TO_OKLAB), oklab).map(|c| c * c * c);
    apply_matrix(
        XYZ_D65_TO_LINEAR_SRGB,
        apply_matrix(inverse(XYZ_D65_TO_OKLAB_LMS), lms),
    )
    .map(linear_to_srgb)
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
            c if c.is_ascii_whitespace() && depth == 0 => {
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

/// The scale a relative colour's channel keyword is on where CSS Color 4
/// writes a percentage of 100: `hsl()` saturation and lightness, `hwb()`
/// whiteness and blackness, and Lab and LCH lightness. CSS Color 5 resolves
/// each keyword to that `<number>`, so `s` in `hsl(from red h s l)` is 100
/// rather than 1, and `50%` written in its place is 50.
const PERCENT_CHANNEL_SCALE: f64 = 100.0;

/// What an `rgb()` percentage is out of: CSS Color 4 section 5.1, "100%
/// corresponds to 255". The `r`, `g` and `b` keywords are on the same scale.
const RGB_CHANNEL_SCALE: f64 = 255.0;

/// What an `oklab()` percentage on the `a` and `b` axes is, and an
/// `oklch()` chroma's: CSS Color 4 section 9.4, "100% = 0.4" for both.
const OKLAB_CHROMA_FULL_SCALE: f64 = 0.4;

/// Where an LCH hue becomes powerless: CSS Color 4 section 9.3's table,
/// "C <= 0.0015", the `epsilon` of its `Lab_to_LCH`.
const LCH_POWERLESS_CHROMA: f32 = 0.0015;

/// Where an OkLCh hue becomes powerless: CSS Color 4 section 9.4's table,
/// "C <= 0.000004", the `epsilon` of its `OKLab_to_OKLCH`.
const OKLCH_POWERLESS_CHROMA: f32 = 0.000_004;

/// What one channel of a relative colour may be written as besides a
/// number.
#[derive(Clone, Copy)]
enum Channel {
    /// A hue, in degrees or as an angle.
    Hue,
    /// A number on this scale, or a percentage of it.
    Scaled(f64),
}

/// The function a relative colour is written in, which decides its channel
/// keywords, the scale of each, and how the result becomes a colour.
#[derive(Clone, Copy, PartialEq)]
enum Relative {
    Rgb,
    Hsl,
    Hwb,
    Lab,
    Lch,
    Oklab,
    Oklch,
    /// `color(from <origin> <space> ...)`, in a space `color()` names.
    Color(&'static str),
}

impl Relative {
    /// The function's channel keywords, in order, then `alpha`.
    fn keywords(self) -> [&'static str; 4] {
        match self {
            Self::Rgb => ["r", "g", "b", "alpha"],
            Self::Hsl => ["h", "s", "l", "alpha"],
            Self::Hwb => ["h", "w", "b", "alpha"],
            Self::Lab | Self::Oklab => ["l", "a", "b", "alpha"],
            Self::Lch | Self::Oklch => ["l", "c", "h", "alpha"],
            Self::Color(space) if space.starts_with("xyz") => {
                ["x", "y", "z", "alpha"]
            }
            Self::Color(_) => ["r", "g", "b", "alpha"],
        }
    }

    /// What each of the three channels may be written as.
    fn channels(self) -> [Channel; 3] {
        let lab_ab = Channel::Scaled(f64::from(LAB_AB_FULL_SCALE));
        let oklab_ab = Channel::Scaled(OKLAB_CHROMA_FULL_SCALE);
        let percent = Channel::Scaled(PERCENT_CHANNEL_SCALE);
        match self {
            Self::Rgb => [Channel::Scaled(RGB_CHANNEL_SCALE); 3],
            Self::Hsl | Self::Hwb => [Channel::Hue, percent, percent],
            Self::Lab => [percent, lab_ab, lab_ab],
            Self::Lch => [
                percent,
                Channel::Scaled(f64::from(LCH_CHROMA_FULL_SCALE)),
                Channel::Hue,
            ],
            Self::Oklab => [Channel::Scaled(1.0), oklab_ab, oklab_ab],
            Self::Oklch => [Channel::Scaled(1.0), oklab_ab, Channel::Hue],
            Self::Color(_) => [Channel::Scaled(1.0); 3],
        }
    }

    /// The origin's three channels as the keywords read them, or `None` for
    /// a `color()` space it does not name.
    ///
    /// Unclamped, a lightness outside its range included: Chrome 153 reads
    /// `l` as 104.4 in `lab(from color(srgb 1.05 1.05 1.05) calc(l - 30) a
    /// b)`, which paints `lab(74.4 0 0)`. Only the result is clamped, by
    /// [`color`](Self::color).
    fn origin(self, rgb: [f32; 3]) -> Option<[f64; 3]> {
        let wide = rgb.map(f64::from);
        let lab = || srgb_to_lab(rgb);
        let oklab = || srgb_to_oklab(rgb);
        let scaled = |[x, y, z]: [f32; 3]| [x, y, z].map(f64::from);
        Some(match self {
            Self::Rgb => wide.map(|c| c * RGB_CHANNEL_SCALE),
            Self::Hsl => {
                let [h, s, l] = srgb_to_hsl(wide);
                [h, s * PERCENT_CHANNEL_SCALE, l * PERCENT_CHANNEL_SCALE]
            }
            Self::Hwb => {
                let [r, g, b] = wide;
                [
                    srgb_hue(wide),
                    r.min(g).min(b) * PERCENT_CHANNEL_SCALE,
                    (1.0 - r.max(g).max(b)) * PERCENT_CHANNEL_SCALE,
                ]
            }
            Self::Lab => scaled(lab()),
            Self::Lch => scaled(to_polar(lab(), LCH_POWERLESS_CHROMA)),
            Self::Oklab => scaled(oklab()),
            Self::Oklch => scaled(to_polar(oklab(), OKLCH_POWERLESS_CHROMA)),
            Self::Color(space) => scaled(srgb_to_color_function(rgb, space)?),
        })
    }

    /// An absolute colour's channels with the clamps CSS Color 4 applies
    /// when it parses the function, as Chrome 153 applies them: `rgb()` into
    /// `0..=255`, `hsl()` saturation and lightness and `hwb()` whiteness and
    /// blackness at zero. Lightness and chroma are clamped for Lab and Oklab
    /// by [`color`](Self::color), for a relative colour too; these are not,
    /// since a relative `rgb(from red calc(r - 300) g b)` keeps its negative
    /// red there.
    fn parsed(self, [x, y, z]: [f64; 3]) -> [f64; 3] {
        match self {
            Self::Rgb => [x, y, z].map(|c| c.clamp(0.0, RGB_CHANNEL_SCALE)),
            Self::Hsl | Self::Hwb => [x, y.max(0.0), z.max(0.0)],
            _ => [x, y, z],
        }
    }

    /// The colour the evaluated channels name, and the space it is in: the
    /// space `color()` named where a canvas can hold it, sRGB otherwise.
    ///
    /// Nothing is clipped but what CSS Color 4 clamps when it parses the
    /// function: Lab and Oklab lightness into its range, and a polar chroma
    /// at zero. Chrome 153 does both -- `lab(from red calc(l - 200) a b)` is
    /// `lab(0 a b)` there and `lch(from red l calc(c - 200) h)` grey -- and
    /// clips nothing else.
    fn color(self, [x, y, z]: [f64; 3]) -> ([f32; 3], &'static str) {
        let narrow = |v: [f64; 3]| v.map(|c| c as f32);
        let lab_l = x.clamp(0.0, PERCENT_CHANNEL_SCALE);
        let oklab_l = x.clamp(0.0, 1.0);
        let from_polar = |l: f64, c: f64, h: f64| {
            let (sin, cos) = h.to_radians().sin_cos();
            narrow([l, c.max(0.0) * cos, c.max(0.0) * sin])
        };
        let srgb = match self {
            Self::Rgb => narrow([x, y, z].map(|c| c / RGB_CHANNEL_SCALE)),
            Self::Hsl => narrow(hsl_to_srgb(
                x,
                y / PERCENT_CHANNEL_SCALE,
                z / PERCENT_CHANNEL_SCALE,
            )),
            Self::Hwb => narrow(hwb_to_srgb(
                x,
                y / PERCENT_CHANNEL_SCALE,
                z / PERCENT_CHANNEL_SCALE,
            )),
            Self::Lab => lab_to_srgb(narrow([lab_l, y, z])),
            Self::Lch => lab_to_srgb(from_polar(lab_l, y, z)),
            Self::Oklab => oklab_to_srgb(narrow([oklab_l, y, z])),
            Self::Oklch => oklab_to_srgb(from_polar(oklab_l, y, z)),
            Self::Color(
                space @ ("srgb" | "srgb-linear" | "display-p3" | "rec2020"),
            ) => {
                return (narrow([x, y, z]), space);
            }
            Self::Color(space) => {
                let [r, g, b] = narrow([x, y, z]);
                let c =
                    color_function_to_srgb(Color4f::new(r, g, b, 1.0), space);
                [c.r, c.g, c.b]
            }
        };
        (srgb, "srgb")
    }
}

/// Lab or Oklab as lightness, chroma and hue in degrees, the hue zero where
/// it is powerless: at a chroma of `powerless` or less, which CSS Color 4
/// sets per space.
///
/// Chrome 153 reads a hue off a neutral origin instead, whatever its own
/// arithmetic leaves there -- about 356.6 degrees for white in LCH and 23.7
/// in OkLCh -- so `lch(from white l 50 h)` is magenta there, where here its
/// hue is zero. That hue is rounding noise, and this follows the standard;
/// `docs/browser-differences.md` has the measurements.
fn to_polar([l, a, b]: [f32; 3], powerless: f32) -> [f32; 3] {
    let chroma = a.hypot(b);
    let hue = match chroma <= powerless {
        true => 0.0,
        false => b.atan2(a).to_degrees().rem_euclid(DEGREES_PER_TURN as f32),
    };
    [l, chroma, hue]
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

/// A colour function -- `rgb(255 0 0)`, `lab(from red l a b)` and the rest --
/// with the space the result is in, or `None` for what is no colour function
/// this crate knows.
///
/// `Some(None)` when it is a colour function and does not parse, which is a
/// refusal rather than a cue to read it some other way.
///
/// Every channel is evaluated by the CSS math evaluator, so a math function
/// may stand in any of them. A relative colour's origin is resolved by this
/// crate's own parser, its channels found in the function's own space from
/// it as it is, and its channel keywords bound -- so nothing is clipped to
/// sRGB between the origin and the result, which is what Chrome does and
/// what CSS Color 5 describes.
///
/// `hwba()`, `hsv()` and `hsva()`, absolute or relative, are `Some(None)`:
/// no CSS function has those names and no browser reads them -- Chrome 153
/// ignores `hwba(0 0% 0%)` and `hsv(0 100% 100%)`.
fn read_color_function(css: &str) -> Option<Option<(Color4f, &'static str)>> {
    let (name, args) = css.trim_ascii().split_once('(')?;
    let function = match name.to_ascii_lowercase().as_str() {
        "rgb" | "rgba" => Relative::Rgb,
        "hsl" | "hsla" => Relative::Hsl,
        "hwb" => Relative::Hwb,
        "hwba" | "hsv" | "hsva" => return Some(None),
        "lab" => Relative::Lab,
        "lch" => Relative::Lch,
        "oklab" => Relative::Oklab,
        "oklch" => Relative::Oklch,
        // The space is read with the channels, below.
        "color" => Relative::Color(""),
        _ => return None,
    };
    let relative = top_level_spans(args)
        .first()
        .is_some_and(|&(from, to)| args[from..to].eq_ignore_ascii_case("from"));
    match relative {
        true => Some(relative_channels(args, function)),
        false => Some(absolute_channels(args, function)),
    }
}

/// The `color()` space `token` names, in lower case, or `None` for none.
fn color_space(token: &str) -> Option<&'static str> {
    Some(match token.to_ascii_lowercase().as_str() {
        "srgb" => "srgb",
        "srgb-linear" => "srgb-linear",
        "display-p3" => "display-p3",
        "rec2020" => "rec2020",
        "a98-rgb" => "a98-rgb",
        "prophoto-rgb" => "prophoto-rgb",
        "xyz" => "xyz",
        "xyz-d50" => "xyz-d50",
        "xyz-d65" => "xyz-d65",
        _ => return None,
    })
}

/// `text`, which follows a function's `(` or its space, split into its
/// three channels and the alpha after a `/`, if there is one: exactly three
/// and at most one, or `None`.
fn channels_and_alpha(text: &str) -> Option<([&str; 3], Option<&str>)> {
    fn token(text: &str, (from, to): (usize, usize)) -> &str {
        &text[from..to]
    }
    let mut depth = 0usize;
    let slash = text.char_indices().find_map(|(at, c)| {
        match c {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            '/' if depth == 0 => return Some(at),
            _ => {}
        }
        None
    });
    let (channels, alpha) = match slash {
        Some(at) => (&text[..at], Some(&text[at + 1..])),
        None => (text, None),
    };
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
    Some((
        [
            token(channels, first),
            token(channels, second),
            token(channels, third),
        ],
        alpha,
    ))
}

/// One channel's value as written, by the CSS math evaluator: a math
/// function, or a number, percentage or angle on its own. `bound` are the
/// channel keywords a relative colour has, which only a math function may
/// name here; `none` and a bare keyword are the caller's.
fn channel_quantity(text: &str, bound: &[(&str, f64)]) -> Option<Quantity> {
    let units = FontUnits::assumed(f32::NAN);
    // A percentage may appear inside a hue's math function so long as the
    // result is not one: Chrome 153 reads `calc(50% / 1%)` as a hue of 50 and
    // refuses `50%`. The dimension is what decides, in `scaled`.
    let expect = Expect {
        percent: Percent::Own,
        units: &units,
        channels: bound,
    };
    match is_math(text) {
        true => evaluate(text, &expect),
        false => literal(text, &expect),
    }
}

/// `quantity` as the number `channel` takes, or `None` for a dimension it
/// does not take.
///
/// CSS Values 4 section 10.9 clamps an infinity to the largest value the
/// property can hold, which for these is the `f32` the colour carries. That
/// is a whole number of turns, so an infinite hue is red, as it is in
/// Chrome 153.
fn scaled(quantity: Quantity, channel: Channel) -> Option<f64> {
    let value = match (channel, quantity.dimension) {
        (_, Dimension::NUMBER) | (Channel::Hue, Dimension::ANGLE) => {
            quantity.finished()
        }
        (Channel::Scaled(scale), Dimension::PERCENT) => {
            quantity.finished() * scale
        }
        _ => return None,
    };
    let largest = f64::from(f32::MAX);
    Some(value.clamp(-largest, largest))
}

/// One channel read: `none` as zero, a channel keyword as its value in
/// `bound`, and anything else through [`channel_quantity`] and [`scaled`].
fn read_channel(
    text: &str,
    channel: Channel,
    bound: &[(&str, f64)],
) -> Option<f64> {
    let keyword = text.to_ascii_lowercase();
    if keyword == "none" {
        return Some(0.0);
    }
    if let Some(&(_, value)) = bound.iter().find(|(name, _)| *name == keyword) {
        return Some(value);
    }
    scaled(channel_quantity(text, bound)?, channel)
}

/// The colour a relative colour function names, given what follows the `(`.
fn relative_channels(
    args: &str,
    function: Relative,
) -> Option<(Color4f, &'static str)> {
    fn token(text: &str, (from, to): (usize, usize)) -> &str {
        &text[from..to]
    }
    let body = args.trim_ascii_end().strip_suffix(')')?.trim_ascii_end();
    let spans = top_level_spans(body);
    // A `)` before the end stops the spans short of it: `hsl(from red h s
    // l))` is refused for the second.
    if spans.last()?.1 != body.len() {
        return None;
    }
    let [_, origin @ (_, origin_end), ..] = spans[..] else {
        return None;
    };
    let origin = css_to_color4f(token(body, origin))?;
    let (function, rest) = match function {
        Relative::Color(_) => {
            let &space @ (_, space_end) = spans.get(2)?;
            let space = color_space(token(body, space))?;
            (Relative::Color(space), &body[space_end..])
        }
        other => (other, &body[origin_end..]),
    };
    let ([t0, t1, t2], alpha) = channels_and_alpha(rest)?;

    let [k0, k1, k2, k_alpha] = function.keywords();
    let [o0, o1, o2] = function.origin([origin.r, origin.g, origin.b])?;
    let bound = [(k0, o0), (k1, o1), (k2, o2), (k_alpha, f64::from(origin.a))];
    let [c0, c1, c2] = function.channels();
    let values = [
        read_channel(t0, c0, &bound)?,
        read_channel(t1, c1, &bound)?,
        read_channel(t2, c2, &bound)?,
    ];
    let alpha = match alpha {
        Some(text) => read_channel(text, Channel::Scaled(1.0), &bound)?,
        None => f64::from(origin.a),
    };
    let ([r, g, b], space) = function.color(values);
    Some((Color4f::new(r, g, b, alpha.clamp(0.0, 1.0) as f32), space))
}

/// The colour an absolute colour function names, given what follows the
/// `(`, and the space it is in, or `None` where it does not parse.
fn absolute_channels(
    args: &str,
    function: Relative,
) -> Option<(Color4f, &'static str)> {
    let body = args.trim_ascii_end().strip_suffix(')')?.trim_ascii();
    match has_top_level_comma(body) {
        true => legacy_channels(body, function),
        false => modern_channels(body, function),
    }
}

/// Whether `text` has a comma outside every bracket in it.
fn has_top_level_comma(text: &str) -> bool {
    let mut depth = 0usize;
    text.chars().any(|c| {
        match c {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => return true,
            _ => {}
        }
        false
    })
}

/// An absolute colour in the space-separated syntax CSS Color 4 gives every
/// function, given what follows its `(` without the `)`.
fn modern_channels(
    body: &str,
    function: Relative,
) -> Option<(Color4f, &'static str)> {
    let spans = top_level_spans(body);
    // A `)` before the end stops the spans short of it.
    if spans.last()?.1 != body.len() {
        return None;
    }
    let (function, rest) = match function {
        Relative::Color(_) => {
            let &(from, to) = spans.first()?;
            (Relative::Color(color_space(&body[from..to])?), &body[to..])
        }
        other => (other, body),
    };
    let ([t0, t1, t2], alpha) = channels_and_alpha(rest)?;
    let [c0, c1, c2] = function.channels();
    let values = [
        read_channel(t0, c0, &[])?,
        read_channel(t1, c1, &[])?,
        read_channel(t2, c2, &[])?,
    ];
    let alpha = match alpha {
        Some(text) => read_channel(text, Channel::Scaled(1.0), &[])?,
        None => 1.0,
    };
    let ([r, g, b], space) = function.color(function.parsed(values));
    Some((Color4f::new(r, g, b, alpha.clamp(0.0, 1.0) as f32), space))
}

/// An absolute `rgb()` or `hsl()` in the comma-separated legacy syntax, given
/// what follows its `(` without the `)`, or `None` for any other comma form.
///
/// CSS Color 4 section 5.1 and 7.1 give that syntax three channels and an
/// optional alpha, no `none`, and for `rgb()` channels all numbers or all
/// percentages, for `hsl()` percentages after the hue. What follows that is
/// read here, a math function in any channel included -- Chrome 153 reads
/// `rgba(calc(255), 0, 0)` -- with `hsl()` saturation and lightness held
/// to `0%..=100%`, which keeps a legacy colour inside sRGB. What does not is
/// refused, as Chrome 153 refuses each: `rgb(255, 0%, 0)`, `rgb(255,0 0)`,
/// `hsl(120, 100, 50)`, `rgb(none, 0, 0)`, `rgb(255, 0, 0, none)`, and commas
/// in a function with no legacy syntax, `lab(50, 40, 30)`.
fn legacy_channels(
    body: &str,
    function: Relative,
) -> Option<(Color4f, &'static str)> {
    if !matches!(function, Relative::Rgb | Relative::Hsl) {
        return None;
    }
    let mut depth = 0usize;
    let mut parts = Vec::new();
    let mut start = 0;
    for (at, c) in body.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                parts.push(body[start..at].trim_ascii());
                start = at + 1;
            }
            _ => {}
        }
    }
    parts.push(body[start..].trim_ascii());
    let (channels, alpha) = match parts[..] {
        [a, b, c] => ([a, b, c], None),
        [a, b, c, alpha] => ([a, b, c], Some(alpha)),
        _ => return None,
    };
    if parts.iter().any(|part| {
        part.eq_ignore_ascii_case("none") || top_level_spans(part).len() != 1
    }) {
        return None;
    }
    let quantities = [
        channel_quantity(channels[0], &[])?,
        channel_quantity(channels[1], &[])?,
        channel_quantity(channels[2], &[])?,
    ];
    let kinds = quantities.map(|q| q.dimension);
    let lawful = match function {
        Relative::Rgb => {
            kinds.iter().all(|&kind| kind == Dimension::NUMBER)
                || kinds.iter().all(|&kind| kind == Dimension::PERCENT)
        }
        _ => {
            matches!(kinds[0], Dimension::NUMBER | Dimension::ANGLE)
                && kinds[1] == Dimension::PERCENT
                && kinds[2] == Dimension::PERCENT
        }
    };
    if !lawful {
        return None;
    }
    let [c0, c1, c2] = function.channels();
    let values = [
        scaled(quantities[0], c0)?,
        scaled(quantities[1], c1)?,
        scaled(quantities[2], c2)?,
    ];
    let alpha = match alpha {
        Some(text) => read_channel(text, Channel::Scaled(1.0), &[])?,
        None => 1.0,
    };
    // Legacy `hsl()` holds saturation and lightness to `0%..=100%`, so a
    // legacy colour stays inside sRGB: Chrome 153 reads `hsl(370, 120%,
    // 42%)` as `#d62400`, which is `hsl(10, 100%, 42%)`, and computes
    // `hsl(0, 150%, 50%)` as `color(srgb 1 0 0)` where the space-separated
    // `hsl(0 150% 50%)` keeps its 1.25.
    let [x, y, z] = function.parsed(values);
    let values = match function {
        Relative::Hsl => [
            x,
            y.min(PERCENT_CHANNEL_SCALE),
            z.min(PERCENT_CHANNEL_SCALE),
        ],
        _ => [x, y, z],
    };
    let ([r, g, b], space) = function.color(values);
    Some((Color4f::new(r, g, b, alpha.clamp(0.0, 1.0) as f32), space))
}

/// CSS Color 4's named colours ([section 6.1]), in the order its table lists
/// them, which is alphabetical: a binary search finds one.
///
/// Generated from the table's own rows, each checked there against the hex
/// form beside it. `transparent` is not among them: [section 6.3] defines it
/// apart, and [`hex_or_named`] reads it.
///
/// [section 6.1]: https://www.w3.org/TR/css-color-4/#named-colors
/// [section 6.3]: https://www.w3.org/TR/css-color-4/#transparent-color
const NAMED_COLORS: [(&str, [u8; 3]); 148] = [
    ("aliceblue", [240, 248, 255]),
    ("antiquewhite", [250, 235, 215]),
    ("aqua", [0, 255, 255]),
    ("aquamarine", [127, 255, 212]),
    ("azure", [240, 255, 255]),
    ("beige", [245, 245, 220]),
    ("bisque", [255, 228, 196]),
    ("black", [0, 0, 0]),
    ("blanchedalmond", [255, 235, 205]),
    ("blue", [0, 0, 255]),
    ("blueviolet", [138, 43, 226]),
    ("brown", [165, 42, 42]),
    ("burlywood", [222, 184, 135]),
    ("cadetblue", [95, 158, 160]),
    ("chartreuse", [127, 255, 0]),
    ("chocolate", [210, 105, 30]),
    ("coral", [255, 127, 80]),
    ("cornflowerblue", [100, 149, 237]),
    ("cornsilk", [255, 248, 220]),
    ("crimson", [220, 20, 60]),
    ("cyan", [0, 255, 255]),
    ("darkblue", [0, 0, 139]),
    ("darkcyan", [0, 139, 139]),
    ("darkgoldenrod", [184, 134, 11]),
    ("darkgray", [169, 169, 169]),
    ("darkgreen", [0, 100, 0]),
    ("darkgrey", [169, 169, 169]),
    ("darkkhaki", [189, 183, 107]),
    ("darkmagenta", [139, 0, 139]),
    ("darkolivegreen", [85, 107, 47]),
    ("darkorange", [255, 140, 0]),
    ("darkorchid", [153, 50, 204]),
    ("darkred", [139, 0, 0]),
    ("darksalmon", [233, 150, 122]),
    ("darkseagreen", [143, 188, 143]),
    ("darkslateblue", [72, 61, 139]),
    ("darkslategray", [47, 79, 79]),
    ("darkslategrey", [47, 79, 79]),
    ("darkturquoise", [0, 206, 209]),
    ("darkviolet", [148, 0, 211]),
    ("deeppink", [255, 20, 147]),
    ("deepskyblue", [0, 191, 255]),
    ("dimgray", [105, 105, 105]),
    ("dimgrey", [105, 105, 105]),
    ("dodgerblue", [30, 144, 255]),
    ("firebrick", [178, 34, 34]),
    ("floralwhite", [255, 250, 240]),
    ("forestgreen", [34, 139, 34]),
    ("fuchsia", [255, 0, 255]),
    ("gainsboro", [220, 220, 220]),
    ("ghostwhite", [248, 248, 255]),
    ("gold", [255, 215, 0]),
    ("goldenrod", [218, 165, 32]),
    ("gray", [128, 128, 128]),
    ("green", [0, 128, 0]),
    ("greenyellow", [173, 255, 47]),
    ("grey", [128, 128, 128]),
    ("honeydew", [240, 255, 240]),
    ("hotpink", [255, 105, 180]),
    ("indianred", [205, 92, 92]),
    ("indigo", [75, 0, 130]),
    ("ivory", [255, 255, 240]),
    ("khaki", [240, 230, 140]),
    ("lavender", [230, 230, 250]),
    ("lavenderblush", [255, 240, 245]),
    ("lawngreen", [124, 252, 0]),
    ("lemonchiffon", [255, 250, 205]),
    ("lightblue", [173, 216, 230]),
    ("lightcoral", [240, 128, 128]),
    ("lightcyan", [224, 255, 255]),
    ("lightgoldenrodyellow", [250, 250, 210]),
    ("lightgray", [211, 211, 211]),
    ("lightgreen", [144, 238, 144]),
    ("lightgrey", [211, 211, 211]),
    ("lightpink", [255, 182, 193]),
    ("lightsalmon", [255, 160, 122]),
    ("lightseagreen", [32, 178, 170]),
    ("lightskyblue", [135, 206, 250]),
    ("lightslategray", [119, 136, 153]),
    ("lightslategrey", [119, 136, 153]),
    ("lightsteelblue", [176, 196, 222]),
    ("lightyellow", [255, 255, 224]),
    ("lime", [0, 255, 0]),
    ("limegreen", [50, 205, 50]),
    ("linen", [250, 240, 230]),
    ("magenta", [255, 0, 255]),
    ("maroon", [128, 0, 0]),
    ("mediumaquamarine", [102, 205, 170]),
    ("mediumblue", [0, 0, 205]),
    ("mediumorchid", [186, 85, 211]),
    ("mediumpurple", [147, 112, 219]),
    ("mediumseagreen", [60, 179, 113]),
    ("mediumslateblue", [123, 104, 238]),
    ("mediumspringgreen", [0, 250, 154]),
    ("mediumturquoise", [72, 209, 204]),
    ("mediumvioletred", [199, 21, 133]),
    ("midnightblue", [25, 25, 112]),
    ("mintcream", [245, 255, 250]),
    ("mistyrose", [255, 228, 225]),
    ("moccasin", [255, 228, 181]),
    ("navajowhite", [255, 222, 173]),
    ("navy", [0, 0, 128]),
    ("oldlace", [253, 245, 230]),
    ("olive", [128, 128, 0]),
    ("olivedrab", [107, 142, 35]),
    ("orange", [255, 165, 0]),
    ("orangered", [255, 69, 0]),
    ("orchid", [218, 112, 214]),
    ("palegoldenrod", [238, 232, 170]),
    ("palegreen", [152, 251, 152]),
    ("paleturquoise", [175, 238, 238]),
    ("palevioletred", [219, 112, 147]),
    ("papayawhip", [255, 239, 213]),
    ("peachpuff", [255, 218, 185]),
    ("peru", [205, 133, 63]),
    ("pink", [255, 192, 203]),
    ("plum", [221, 160, 221]),
    ("powderblue", [176, 224, 230]),
    ("purple", [128, 0, 128]),
    ("rebeccapurple", [102, 51, 153]),
    ("red", [255, 0, 0]),
    ("rosybrown", [188, 143, 143]),
    ("royalblue", [65, 105, 225]),
    ("saddlebrown", [139, 69, 19]),
    ("salmon", [250, 128, 114]),
    ("sandybrown", [244, 164, 96]),
    ("seagreen", [46, 139, 87]),
    ("seashell", [255, 245, 238]),
    ("sienna", [160, 82, 45]),
    ("silver", [192, 192, 192]),
    ("skyblue", [135, 206, 235]),
    ("slateblue", [106, 90, 205]),
    ("slategray", [112, 128, 144]),
    ("slategrey", [112, 128, 144]),
    ("snow", [255, 250, 250]),
    ("springgreen", [0, 255, 127]),
    ("steelblue", [70, 130, 180]),
    ("tan", [210, 180, 140]),
    ("teal", [0, 128, 128]),
    ("thistle", [216, 191, 216]),
    ("tomato", [255, 99, 71]),
    ("turquoise", [64, 224, 208]),
    ("violet", [238, 130, 238]),
    ("wheat", [245, 222, 179]),
    ("white", [255, 255, 255]),
    ("whitesmoke", [245, 245, 245]),
    ("yellow", [255, 255, 0]),
    ("yellowgreen", [154, 205, 50]),
];

/// A hex colour -- `#rgb`, `#rgba`, `#rrggbb` or `#rrggbbaa` -- or a named one,
/// in either case, or `None` for anything else.
///
/// Each component is its byte over 255, and
/// `hex_and_named_colours_read_as_they_did` holds every named colour and
/// every hex form to the `f32`s earlier releases gave. A `#` followed by
/// anything else is `Some(None)`, a refusal: only hex follows one.
fn hex_or_named(css: &str) -> Option<Option<Color4f>> {
    let css = css.trim_ascii();
    let byte = |b: u8| f32::from(b) / 255.0;
    if let Some(digits) = css.strip_prefix('#') {
        let nibbles: Option<Vec<u8>> = digits
            .chars()
            .map(|c| c.to_digit(16).map(|d| d as u8))
            .collect();
        let Some(nibbles) = nibbles else {
            return Some(None);
        };
        let channels: Vec<u8> = match nibbles.len() {
            // `#abc` is `#aabbcc`: each digit stands for itself twice.
            3 | 4 => nibbles.iter().map(|&n| n << 4 | n).collect(),
            6 | 8 => nibbles
                .chunks(2)
                .map(|pair| pair[0] << 4 | pair[1])
                .collect(),
            _ => return Some(None),
        };
        let alpha = channels.get(3).copied().unwrap_or(u8::MAX);
        return Some(Some(Color4f::new(
            byte(channels[0]),
            byte(channels[1]),
            byte(channels[2]),
            byte(alpha),
        )));
    }
    let name = css.to_ascii_lowercase();
    if name == "transparent" {
        return Some(Some(Color4f::new(0.0, 0.0, 0.0, 0.0)));
    }
    // `currentcolor` is the `color` property of the element a colour is on.
    // A canvas here is on none, and CSS Color 4's "parse a CSS <color>
    // value", which HTML's `fillStyle` and `strokeStyle` setters run with no
    // element when the canvas has none, resolves it with "the initial values
    // of the properties": `color`'s, `CanvasText`, black. Chrome 152 paints
    // it opaque black on a canvas outside a document.
    if name == "currentcolor" {
        return Some(Some(Color4f::new(0.0, 0.0, 0.0, 1.0)));
    }
    let at = NAMED_COLORS
        .binary_search_by(|&(named, _)| named.cmp(name.as_str()))
        .ok()?;
    let [r, g, b] = NAMED_COLORS[at].1;
    Some(Some(Color4f::new(byte(r), byte(g), byte(b), 1.0)))
}

/// `css` with every bracket still open at its end closed, as CSS Syntax 3
/// closes every open block when the input ends: `rgb(255 0 0` is
/// `rgb(255 0 0)`, as Chrome 153's canvas reads it.
///
/// A function closed this way reads everything after its `(`, so
/// `rgb(calc(255 0 0` closes as `calc(255 0 0)` and is refused, as it is in
/// Chrome. A `)` that closes nothing is not this function's to repair; the
/// parsers refuse it, as Chrome does `)rgb(255 0 0`.
fn closed_at_end(css: &str) -> Cow<'_, str> {
    let depth = css.chars().fold(0usize, |depth, c| match c {
        '(' => depth + 1,
        ')' => depth.saturating_sub(1),
        _ => depth,
    });
    match depth {
        0 => Cow::Borrowed(css),
        open => Cow::Owned(format!("{css}{}", ")".repeat(open))),
    }
}

/// Parses a CSS color, keeping the space it was named in.
///
/// `color(display-p3 1 0 0)` on a Display P3 canvas is then an identity --
/// Skia converts at the paint, and converting via sRGB first would cost a
/// level of the eight the readback quantises to.
pub fn css_to_color4f_in_space(css: &str) -> Option<(Color4f, ColorSpace)> {
    let uncommented = without_comments(css)?;
    let css = closed_at_end(uncommented.as_ref());
    let css = css.as_ref();
    if let Some(color) = read_color_function(css) {
        // SAFETY: `read_color_function` only returns names this builds.
        return color
            .and_then(|(color, space)| Some((color, opt_color_space(space)?)));
    }
    if let Some(color) = hex_or_named(css) {
        return color.map(|color| (color, ColorSpace::new_srgb()));
    }
    // Neither a colour function nor a hex or named colour, which is all CSS
    // has: hex digits with no `#`, a function name with a space before its
    // `(` -- `rgb (255 0 0)`, which CSS Syntax reads as a word and a block --
    // and the rest. Chrome 153 ignores each.
    None
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

/// `color` in `space` as Skia is handed it.
///
/// Every reader that tags a colour with the space it was named in goes
/// through this, so a colour decodes by one curve whichever reader paints
/// it. Each space this crate names decodes correctly from its Skia tag, so
/// the colour passes through as it is; this is the one place to divert a
/// space whose tag Skia would decode differently from the standard.
/// `rec2020` is not one: CSS Color 4 section 10.8 defines it as "gamma
/// 2.40, from [REC_BT.1886]", and the space [`opt_color_space`] builds for
/// it is the CICP pair of BT.2020 primaries and transfer characteristic 1,
/// whose `named_transfer_fn::REC709` is a pure 2.4 gamma, which agrees.
/// Skia's own `kRec2020` transfer function is BT.2020's piecewise curve.
pub fn skia_paint_color(
    color: Color4f,
    space: &ColorSpace,
) -> (Color4f, ColorSpace) {
    (color, space.clone())
}

/// Parses a CSS color into 8-bit sRGB, gamut-mapped.
///
/// For the callers that can only hand Skia an `SkColor` -- a window
/// background, a light's colour, an SVG's `currentColor`. Anything drawn
/// through a paint should take [`css_to_color4f`] instead and keep the
/// precision. A colour outside sRGB is mapped as [`srgb_to_skia_color`] says.
pub fn css_to_color(css: &str) -> Option<Color> {
    css_to_color4f(css).map(srgb_to_skia_color)
}

/// Parses a CSS color into 8-bit sRGB, clamping each channel on its own.
///
/// For a colour that is per-channel coefficients rather than a colour --
/// `ColorFilter.MakeLighting`'s multiply and add -- where mapping in a
/// perceptual space would change what each channel does.
pub fn css_to_clamped_color(css: &str) -> Option<Color> {
    css_to_color4f(css).map(|c| {
        let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        Color::from_argb(byte(c.a), byte(c.r), byte(c.g), byte(c.b))
    })
}

/// An extended-sRGB colour, straight alpha, as an 8-bit `SkColor`.
///
/// A colour inside sRGB is quantised as it is, so its bytes are the ones the
/// CSS named. One outside is gamut-mapped per CSS Color 4 -- chroma reduced
/// in Oklch at constant lightness and hue -- by [`gamut_map_srgb`], which
/// keeps the hue a per-channel clip would shift.
pub fn srgb_to_skia_color(c: Color4f) -> Color {
    let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    let [r, g, b] = if [c.r, c.g, c.b].iter().all(|v| (0.0..=1.0).contains(v)) {
        [c.r, c.g, c.b]
    } else {
        gamut_map_srgb([c.r, c.g, c.b].map(srgb_to_linear)).map(linear_to_srgb)
    };
    Color::from_argb(byte(c.a), byte(r), byte(g), byte(b))
}

/// A colour as a paint holds it -- `color` in `space` -- as an 8-bit
/// `SkColor`, for the Skia APIs that take nothing wider. Converted to sRGB
/// and then as [`srgb_to_skia_color`] says.
pub fn color4f_to_skia_color(color: Color4f, space: &ColorSpace) -> Color {
    srgb_to_skia_color(color4f_to_srgb(color, Some(space)))
}

pub fn color_in<'a>(
    cx: &mut FunctionContext<'a>,
    val: Handle<'a, JsValue>,
) -> Option<Color> {
    let (color, space) = color4f_string_in(cx, val)?;
    Some(color4f_to_skia_color(color, &space))
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
/// array, as an unpremultiplied `Color4f` and the space it is in:
/// - Float array: gamma-encoded sRGB components with straight alpha -- CSS's
///   `color(srgb r g b / a)`, and CanvasKit's convention -- tagged sRGB, so
///   Skia converts it to whatever canvas it is drawn on.
/// - CSS string: encoded in the space the string named, tagged with it.
pub fn color4f_in<'a>(
    cx: &mut FunctionContext<'a>,
    val: Handle<'a, JsValue>,
) -> Option<(Color4f, ColorSpace)> {
    // An array of four numbers is the float-color form. Anything else that
    // happens to be an array falls through to the string path rather than
    // being rejected: an Array then reaches the JsObject arm and is parsed
    // as CSS via its `toString()`, so `['red']` sets red. That holds for the
    // callers here -- the context's colour styles and `addColorStop` -- and
    // is what their setters have always done, so do not start rejecting
    // malformed arrays here. A Paragraph colour option is not read here: it
    // is converted as WebIDL converts a union, where an array is a sequence
    // of four numbers and `['red']` a `TypeError` -- see `colour_option` in
    // src/node/paragraph.rs.
    // Tagged rather than left for Skia to read in the canvas's space, so the
    // colour is the same on every canvas.
    if let Some(color) = opt_color4f_from_array(cx, val) {
        return Some((color, ColorSpace::new_srgb()));
    }
    // Unquantised: `Color4f::from` an 8-bit `Color` clipped every colour the
    // CSS Color 4 functions can name outside sRGB, before the surface it was
    // bound for ever saw it.
    color4f_string_in(cx, val)
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

/// The CSS colour at `attr` as a matte, kept in floating point in the space
/// it was named in, so a wide matte reaches a wide export.
pub fn opt_matte_for_key(
    cx: &mut FunctionContext,
    obj: &Handle<JsObject>,
    attr: &str,
) -> Option<Matte> {
    let value = obj.get_value(cx, attr).ok()?;
    let (color, space) = color4f_string_in(cx, value)?;
    Some(Matte::new(color, space))
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
    Option<Matte>,
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
            let matte = opt_matte_for_key(cx, &obj, "matte");
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
    context::page::{ExportOptions, Matte},
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
    let matte = opt_matte_for_key(cx, &opts, "matte");
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

/// The quality an `imageSmoothingQuality` name asks for, matched exactly as
/// WebIDL matches an enumeration's values.
pub fn to_filter_quality(mode_name: &str) -> Option<SamplingQuality> {
    let mode = match mode_name {
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
/// The tiling a `createPattern` repetition asks for, matched exactly: the
/// standard refuses one not "identical to" its four names.
pub fn to_repeat_mode(repeat: &str) -> Option<(TileMode, TileMode)> {
    let mode = match repeat {
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
/// cannot be reached from JavaScript. The three arms are the whole enum,
/// matched exactly as WebIDL matches an enumeration's values.
pub fn to_stroke_cap(mode_name: &str) -> Option<StrokeCap> {
    let mode = match mode_name {
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
/// As [`to_stroke_cap`]: the crate's type, the three arms are the whole
/// enum, and they are matched exactly.
pub fn to_stroke_join(mode_name: &str) -> Option<StrokeJoin> {
    let mode = match mode_name {
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
/// The blend a `globalCompositeOperation` name asks for, matched exactly:
/// the standard ignores a value not "identical to" one it lists.
pub fn to_blend_mode(mode_name: &str) -> Option<CrateBlend> {
    let mode = match mode_name {
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
/// own `src`/`dst` shorthands and CanvasKit's camel case. The name is
/// lowercased before either is tried, so `srcOver` and `srcover` are one
/// arm, and `Multiply` is `multiply` here where `globalCompositeOperation`
/// ignores it.
///
/// Kept separate rather than folded into [`to_blend_mode`] on purpose. These
/// spellings belong to the CanvasKit-mirroring surface; letting
/// `globalCompositeOperation` accept `"srcOver"` would put a name in the core
/// API that no browser has.
pub fn to_filter_blend_mode(mode_name: &str) -> Option<CrateBlend> {
    let mode_name = mode_name.to_lowercase();
    if let Some(mode) = to_blend_mode(&mode_name) {
        return Some(mode);
    }

    Some(match mode_name.as_str() {
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
    parsed_color_arg(cx, idx, attr, css_to_color)
}

/// [`color_arg`] for per-channel coefficients, clamped rather than
/// gamut-mapped -- see [`css_to_clamped_color`].
///
/// # Errors
///
/// As [`color_arg`].
pub fn clamped_color_arg(
    cx: &mut FunctionContext,
    idx: usize,
    attr: &str,
) -> NeonResult<Color> {
    parsed_color_arg(cx, idx, attr, css_to_clamped_color)
}

/// [`color_arg`] kept in floating point, with the space the colour was named
/// in, for a Skia API that takes a tagged `Color4f`.
///
/// # Errors
///
/// As [`color_arg`].
pub fn color4f_arg(
    cx: &mut FunctionContext,
    idx: usize,
    attr: &str,
) -> NeonResult<(Color4f, ColorSpace)> {
    parsed_color_arg(cx, idx, attr, css_to_color4f_in_space)
}

fn parsed_color_arg<T>(
    cx: &mut FunctionContext,
    idx: usize,
    attr: &str,
    parse: fn(&str) -> Option<T>,
) -> NeonResult<T> {
    let name = string_arg(cx, idx, attr)?;
    match parse(&name) {
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
/// The style a `lineDashFit` name asks for, matched exactly as the standard
/// attributes beside it are.
pub fn to_1d_style(mode_name: &str) -> Option<path_1d_path_effect::Style> {
    let mode = match mode_name {
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

    /// FNV-1a over every input [`hex_and_named_colours_read_as_they_did`]
    /// reads, a byte saying whether it was read, and the `f32` bits it reads
    /// as: `csscolorparser` 0.9's answers, which these matched bit for bit,
    /// input by input, when the inputs were first recorded.
    const HEX_AND_NAMED_DIGEST: u64 = 0x50bc_dc39_c4f3_3e1b;

    /// FNV-1a's 64-bit offset basis and prime, as the FNV specification,
    /// draft-eastlake-fnv, gives them.
    const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x100_0000_01b3;

    /// Set to print each input and what it reads as, one per line, to
    /// standard error.
    const DUMP_VARIABLE: &str = "HEX_AND_NAMED_DUMP";

    /// Display P3 colours as linear-light sRGB, through the tree's own
    /// matrix, so the inputs are the colours CSS names.
    fn p3(rgb: [f32; 3]) -> [f32; 3] {
        apply_matrix(LINEAR_P3_TO_LINEAR_SRGB, rgb.map(srgb_to_linear))
    }

    #[test]
    fn a_colour_inside_srgb_is_not_gamut_mapped() {
        for rgb in [
            [0.0, 0.0, 0.0],
            [1.0, 1.0, 1.0],
            [0.2, 0.5, 0.9],
            [1.0, 0.0, 0.0],
        ] {
            assert_eq!(gamut_map_srgb(rgb), rgb, "{rgb:?}");
        }
    }

    #[test]
    fn gamut_mapping_matches_css_color_4s_worked_example() {
        // CSS Color 4 section 14.1.3 reduces Display P3 yellow's chroma in
        // OkLCh to the edge of sRGB, `color(srgb 0.99116 0.99733 0.00001)`.
        // Section 14.2.2's search stops as soon as clipping costs less than
        // a JND, so it lands within one JND of that edge rather than on it.
        let mapped = gamut_map_srgb(p3([1.0, 1.0, 0.0]));
        let edge = [0.99116f32, 0.99733, 0.00001].map(srgb_to_linear);
        let [l1, a1, b1] = linear_srgb_to_oklab_f64(mapped.map(f64::from));
        let [l2, a2, b2] = linear_srgb_to_oklab_f64(edge.map(f64::from));
        let distance =
            ((l1 - l2).powi(2) + (a1 - a2).powi(2) + (b1 - b2).powi(2)).sqrt();
        assert!(distance < GAMUT_MAP_JND, "{mapped:?}, {distance} off");
    }

    #[test]
    fn an_eight_bit_colour_maps_only_what_srgb_cannot_hold() {
        // Inside sRGB the bytes are the ones the CSS named, half steps
        // rounded as Chrome rounds them; nothing passes through the mapping.
        assert_eq!(
            css_to_color("rgb(10.5 20.4 30.6)"),
            Some(Color::from_argb(255, 11, 20, 31))
        );
        assert_eq!(
            css_to_color("#80808080"),
            Some(Color::from_argb(128, 128, 128, 128))
        );
        // Outside it, the mapped colour rather than the clipped one: P3 red
        // clips to sRGB red, and maps to a red that keeps its hue.
        let mapped = css_to_color("color(display-p3 1 0 0)").expect("a colour");
        let clipped =
            css_to_clamped_color("color(display-p3 1 0 0)").expect("a colour");
        assert_eq!(clipped, Color::from_argb(255, 255, 0, 0));
        let expected = gamut_map_srgb(p3([1.0, 0.0, 0.0])).map(linear_to_srgb);
        let byte = |v: f32| (v * 255.0).round() as u8;
        assert_eq!(
            mapped,
            Color::from_argb(
                255,
                byte(expected[0]),
                byte(expected[1]),
                byte(expected[2])
            )
        );
        assert_ne!(mapped, clipped);
    }

    #[test]
    fn past_either_end_of_lightness_is_white_or_black() {
        // Steps 3 and 4 of section 14.2.2.
        assert_eq!(gamut_map_srgb([2.0, 2.0, 2.0]), [1.0; 3]);
        assert_eq!(gamut_map_srgb([-0.1, -0.1, -0.1]), [0.0; 3]);
    }

    /// The JND stop, against an `f64` reference: what comes back is inside
    /// sRGB, and within one JND of a colour of the origin's own lightness
    /// and hue -- the point the search reduced chroma to.
    #[test]
    fn gamut_mapping_stops_within_a_jnd_of_the_origins_hue() {
        let oklch = |rgb: [f32; 3]| {
            let [l, a, b] = linear_srgb_to_oklab_f64(rgb.map(f64::from));
            (l, a.hypot(b), b.atan2(a))
        };
        for origin in [
            p3([1.0, 0.0, 0.0]),
            p3([0.0, 1.0, 0.0]),
            p3([1.0, 0.5, 0.0]),
            p3([0.0, 0.3, 1.0]),
        ] {
            let mapped = gamut_map_srgb(origin);
            assert!(
                mapped.iter().all(|c| (0.0..=1.0).contains(c)),
                "{mapped:?} is inside sRGB"
            );
            let (l0, _, h0) = oklch(origin);
            let (l1, c1, h1) = oklch(mapped);
            // The colour the search held lightness and hue for, at the
            // chroma the mapped one has.
            let held = [l0, c1 * h0.cos(), c1 * h0.sin()];
            let [lm, am, bm] = [l1, c1 * h1.cos(), c1 * h1.sin()];
            let distance = ((lm - held[0]).powi(2)
                + (am - held[1]).powi(2)
                + (bm - held[2]).powi(2))
            .sqrt();
            assert!(
                distance < GAMUT_MAP_JND,
                "{origin:?} mapped to {mapped:?}, {distance} off its hue"
            );
        }
    }

    #[test]
    fn hex_and_named_colours_read_as_they_did() {
        // Every named colour in either case, every short hex form, and the
        // long forms channel by channel with a spread of the rest.
        //
        // A digest names no input when it fails. To find the ones that moved,
        // run this with `HEX_AND_NAMED_DUMP=1` and `-- --nocapture`, here and
        // at the last commit it passed, and diff the two outputs.
        //
        // A refusal folds a 0 byte and an answer a 1 before its bits. Without
        // that byte a refusal and transparent black fold the same bytes, so a
        // reader that refused `transparent` or `#0000` would pass.
        let dump = std::env::var_os(DUMP_VARIABLE).is_some();
        let mut digest = FNV_OFFSET_BASIS;
        let mut fold = |bytes: &[u8]| {
            for &byte in bytes {
                digest = (digest ^ u64::from(byte)).wrapping_mul(FNV_PRIME);
            }
        };
        let mut read = |css: &str| {
            let ours = hex_or_named(css)
                .flatten()
                .map(|c| c.as_array().map(f32::to_bits));
            if dump {
                eprintln!("{css} {ours:08x?}");
            }
            fold(css.as_bytes());
            fold(&[u8::from(ours.is_some())]);
            for bits in ours.unwrap_or_default() {
                fold(&bits.to_le_bytes());
            }
        };
        for (name, _) in NAMED_COLORS {
            read(name);
            read(&name.to_ascii_uppercase());
        }
        for name in ["transparent", "Transparent", "TRANSPARENT"] {
            read(name);
        }
        for value in 0..16u32.pow(4) {
            read(&format!("#{value:04x}"));
            read(&format!("#{value:04X}"));
            if value < 16u32.pow(3) {
                read(&format!("#{value:03x}"));
            }
        }
        for byte in 0..=255u32 {
            for shift in [0, 8, 16, 24] {
                read(&format!(
                    "#{:08x}",
                    byte << shift | 0x5a5a5a5a & !(0xff << shift)
                ));
            }
            read(&format!("#{:06x}", byte * 0x010101));
        }
        let mut state = 0x2545_f491_u32;
        for _ in 0..50_000 {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            read(&format!("#{state:08x}"));
            read(&format!("#{:06x}", state >> 8));
        }
        assert_eq!(digest, HEX_AND_NAMED_DIGEST);
    }

    #[test]
    fn a_hash_that_is_not_hex_is_refused_here() {
        for css in ["#", "#ab", "#abcde", "#abcdefabc", "#ggg", "#12345g"] {
            assert_eq!(hex_or_named(css), Some(None), "{css}");
        }
        assert_eq!(hex_or_named("notacolour"), None);
    }

    #[test]
    fn a_hue_is_powerless_at_the_chroma_css_color_4_names_and_no_further() {
        // CSS Color 4's tables: LCH at C <= 0.0015, OkLCh at C <= 0.000004.
        // At the threshold the hue reads zero; one `f32` step past it, the
        // quarter turn `b` alone gives.
        for powerless in [LCH_POWERLESS_CHROMA, OKLCH_POWERLESS_CHROMA] {
            let [_, chroma, hue] = to_polar([50.0, 0.0, powerless], powerless);
            assert_eq!((chroma, hue), (powerless, 0.0), "at {powerless}");
            let past = powerless.next_up();
            let [_, _, hue] = to_polar([50.0, 0.0, past], powerless);
            assert_eq!(hue, 90.0, "past {powerless}");
        }
    }

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
    /// `to_filter_blend_mode` lowercases a name before either `match` sees
    /// it, so an arm whose literal carries a capital can never match. That is
    /// not hypothetical: [`enum_arg_or`] carries a `debug_assert!` against
    /// exactly this for table-driven paths, and its comment records that it
    /// is "how the camel case blend names went unreachable for as long as
    /// they were advertised". These two are `match` arms rather than
    /// tables, so that assertion cannot reach them -- and a dead arm raises
    /// no warning, fails no test, and reads as a supported spelling to
    /// anyone reading the source.
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
