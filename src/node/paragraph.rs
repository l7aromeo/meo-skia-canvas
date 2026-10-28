//
// ParagraphBuilder & Paragraph wrappers for CanvasKit parity
//
#![allow(non_snake_case)]
use neon::{prelude::*, types::JsBigInt};
use std::{cell::RefCell, sync::OnceLock};

use skia_safe::{
    Color, Color4f, ColorSpace, FourByteTag, Paint, PaintStyle, Point,
    font_style::{FontStyle, Slant, Weight, Width},
    textlayout::{
        FontCollection, Paragraph as SkParagraph, ParagraphStyle,
        PlaceholderStyle, StrutStyle, TextDecoration, TextDecorationMode,
        TextDirection, TextShadow, TextStyle,
    },
};

use crate::{
    font::{FontFamily, ObliqueAngle},
    font_library::{FontLibrary, instance_coordinates},
    node::{
        oblique::{
            ParagraphRecipe, Passes, add_slnt, match_oblique, set_coordinates,
            without_paragraph_cache,
        },
        typography::with_oblique_slnt,
    },
    text::{
        PlaceholderAlignment as CratePlaceholderAlignment, PlaceholderBaseline,
        RectHeightStyle as CrateRectHeightStyle,
        RectWidthStyle as CrateRectWidthStyle,
        TextDecoration as CrateTextDecoration,
        TextDecorationStyle as CrateTextDecorationStyle,
        TextHeightBehavior as CrateTextHeightBehavior,
    },
    typography::to_text_align,
    utils::*,
};

//
// Boxed wrapper types
//

pub struct ParagraphBuilderWrap {
    /// The steps the paragraph is built from, until `build()` takes them.
    recipe: Option<ParagraphRecipe>,
    /// The collection the builder resolves against, kept alive for it, and
    /// searched by a pushed style for the axes its face has.
    collection: FontCollection,
    /// The paragraph's `fontVariations`, which a pushed style with none of
    /// its own is instanced at.
    variations: Vec<(FourByteTag, f32)>,
}

impl Finalize for ParagraphBuilderWrap {}
pub type BoxedParagraphBuilder = JsBox<RefCell<ParagraphBuilderWrap>>;

pub struct ParagraphWrap {
    pub paragraph: SkParagraph,
    /// What draws the paragraph's oblique ranges and its decorations,
    /// where it has any.
    pub passes: Option<Passes>,
    /// The collection whose paragraph cache a layout goes without -- see
    /// `oblique::without_paragraph_cache`.
    pub uncached: Option<FontCollection>,
}

impl Finalize for ParagraphWrap {}
pub type BoxedParagraph = JsBox<RefCell<ParagraphWrap>>;

//
// Style parsing helpers
//

// Each option is converted as WebIDL converts a value to the type the
// option's declaration names, as the Canvas API's own attributes are: a
// number by `ToNumber`, so `fontSize: "12"` is 12 as `ctx.lineWidth = "5"`
// is 5; a string by `ToString`, `null` being the option left out; a
// boolean by `ToBoolean`; an object of options as a dictionary; a list as a
// sequence. Where a conversion throws,
// the `TypeError` names the option and what it takes -- AGENTS.md's refusal
// rule 5, the error WebIDL raises -- and an absent or `undefined` option
// takes its default, as an omitted dictionary member does. A converted
// value outside what an option allows is another rule's: a number out of
// range, a colour that does not parse.
//
// `prefix` and `key` are where the option is -- `textStyle.` and
// `fontSize`, or no prefix and `shadows[0].offset` -- so that the refusal
// names the option the way the caller wrote it.

/// What a JavaScript value is, as a refusal names it.
fn kind_of(cx: &mut FunctionContext, value: Handle<JsValue>) -> &'static str {
    if value.is_a::<JsNull, _>(cx) {
        "null"
    } else if value.is_a::<JsUndefined, _>(cx) {
        "undefined"
    } else if value.is_a::<JsString, _>(cx) {
        "a string"
    } else if value.is_a::<JsNumber, _>(cx) {
        "a number"
    } else if value.is_a::<JsBoolean, _>(cx) {
        "a boolean"
    } else if value.is_a::<JsArray, _>(cx) {
        "an array"
    } else if value.is_a::<JsFunction, _>(cx) {
        "a function"
    } else if value.is_a::<JsObject, _>(cx) {
        "an object"
    } else if value.is_a::<JsBigInt, _>(cx) {
        "a bigint"
    } else {
        "a symbol"
    }
}

/// Whether `value` is a Symbol, the one primitive `kind_of` names by
/// elimination: `ToString` and `ToNumber` both throw for one.
fn is_symbol(cx: &mut FunctionContext, value: Handle<JsValue>) -> bool {
    kind_of(cx, value) == "a symbol"
}

/// Refuses `value`, given for the option at `prefix` and `key`, which
/// takes `takes`.
fn wrong_kind<T>(
    cx: &mut FunctionContext,
    prefix: &str,
    key: &str,
    takes: &str,
    value: Handle<JsValue>,
) -> NeonResult<T> {
    let got = kind_of(cx, value);
    cx.throw_type_error(format!("`{prefix}{key}` takes {takes}, not {got}"))
}

/// The value of `key` in `obj`, or `None` where it is absent or `undefined`.
fn option_value<'a>(
    cx: &mut FunctionContext<'a>,
    obj: &Handle<'a, JsObject>,
    key: &str,
) -> NeonResult<Option<Handle<'a, JsValue>>> {
    let value: Handle<JsValue> = obj.get_value(cx, key)?;
    Ok((!value.is_a::<JsUndefined, _>(cx)).then_some(value))
}

/// `value` as an IDL `double`, WebIDL section 3.2.7: "Let x be ?
/// ToNumber(V). If x is NaN, +∞, or −∞, then throw a TypeError."
///
/// `ToNumber` is the global `Number`, which is it for every value but a
/// BigInt, for which `Number` converts and `ToNumber` throws. Every number
/// option here is a `double` rather than an `unrestricted double`: none of
/// them has a use for NaN or an infinity, and lib/index.d.ts declares none
/// as taking one.
fn to_double(
    cx: &mut FunctionContext,
    value: Handle<JsValue>,
    prefix: &str,
    key: &str,
) -> NeonResult<f64> {
    const TAKES: &str = "a finite number";
    let number = match value.downcast::<JsNumber, _>(cx) {
        Ok(number) => number.value(cx),
        Err(_) if value.is_a::<JsBigInt, _>(cx) || is_symbol(cx, value) => {
            return wrong_kind(cx, prefix, key, TAKES, value);
        }
        Err(_) => {
            let to_number: Handle<JsFunction> = cx.global("Number")?;
            to_number
                .call_with(cx)
                .arg(value)
                .apply::<JsNumber, _>(cx)?
                .value(cx)
        }
    };
    if number.is_finite() {
        return Ok(number);
    }
    let spelled = spelled(cx, number)?;
    let got = match value.is_a::<JsNumber, _>(cx) {
        true => spelled,
        false => format!("{} converting to {spelled}", kind_of(cx, value)),
    };
    cx.throw_type_error(format!("`{prefix}{key}` takes {TAKES}, not {got}"))
}

/// A number option, as an IDL `double` -- see [`to_double`].
fn number_option<'a>(
    cx: &mut FunctionContext<'a>,
    obj: &Handle<'a, JsObject>,
    prefix: &str,
    key: &str,
) -> NeonResult<Option<f64>> {
    match option_value(cx, obj, key)? {
        Some(value) => to_double(cx, value, prefix, key).map(Some),
        None => Ok(None),
    }
}

/// A number option as an `f32`.
fn float_option<'a>(
    cx: &mut FunctionContext<'a>,
    obj: &Handle<'a, JsObject>,
    prefix: &str,
    key: &str,
) -> NeonResult<Option<f32>> {
    Ok(number_option(cx, obj, prefix, key)?.map(|number| number as f32))
}

/// The code `number` names in `codes`, which lists them in order from 0, or
/// a `RangeError` naming `number` -- AGENTS.md's rule 4, a number outside
/// the set it is read from, where a misspelt name would be rule 2's
/// `TypeError`. Matched on the number itself rather than on an integer cast
/// from it, which truncates `1.5` to a code and saturates `1e10` to
/// `i32::MAX`, naming a number the caller never passed.
fn code<T: Copy>(
    cx: &mut FunctionContext,
    number: f64,
    codes: &[T],
    name: &str,
) -> NeonResult<T> {
    // The cast truncates and saturates, so only a number it gives back
    // unchanged is a whole one in range.
    let at = number as usize;
    if let Some(&code) = codes.get(at)
        && at as f64 == number
    {
        return Ok(code);
    }
    let spelled = spelled(cx, number)?;
    let expected = match codes.len() {
        2 => "0 or 1".to_string(),
        len => format!("0 to {}", len - 1),
    };
    cx.throw_range_error(format!(
        "Unknown {name} {spelled} (expected {expected})"
    ))
}

/// `value` as an IDL `DOMString`, WebIDL section 3.2.10: "Let x be ?
/// ToString(V)." Only a Symbol throws, and is refused here so the error
/// names the option.
fn to_dom_string(
    cx: &mut FunctionContext,
    value: Handle<JsValue>,
    prefix: &str,
    key: &str,
) -> NeonResult<String> {
    if is_symbol(cx, value) {
        return wrong_kind(cx, prefix, key, "a string", value);
    }
    Ok(value.to_string(cx)?.value(cx))
}

/// A string option, as an IDL `DOMString?`, WebIDL section 3.2.20: "if V is
/// null or undefined, then return the IDL nullable type T? value null",
/// and otherwise the `DOMString` of [`to_dom_string`]. Every string option
/// is nullable, null meaning the option is absent: `ellipsis: null` is no
/// ellipsis rather than the four letters, and `locale: null` no locale
/// rather than one named "null".
fn string_option<'a>(
    cx: &mut FunctionContext<'a>,
    obj: &Handle<'a, JsObject>,
    prefix: &str,
    key: &str,
) -> NeonResult<Option<String>> {
    match option_value(cx, obj, key)? {
        Some(value) if value.is_a::<JsNull, _>(cx) => Ok(None),
        Some(value) => to_dom_string(cx, value, prefix, key).map(Some),
        None => Ok(None),
    }
}

/// A boolean option, as an IDL `boolean`, WebIDL section 3.2.3: "Let x be
/// the result of computing ToBoolean(V)." Nothing is refused: every value
/// is truthy or not.
fn bool_option<'a>(
    cx: &mut FunctionContext<'a>,
    obj: &Handle<'a, JsObject>,
    key: &str,
) -> NeonResult<Option<bool>> {
    let Some(value) = option_value(cx, obj, key)? else {
        return Ok(None);
    };
    let to_boolean: Handle<JsFunction> = cx.global("Boolean")?;
    let truth = to_boolean
        .call_with(cx)
        .arg(value)
        .apply::<JsBoolean, _>(cx)?
        .value(cx);
    Ok(Some(truth))
}

/// `value` as an IDL dictionary, WebIDL section 3.2.17: "If jsDict is not an
/// Object and jsDict is neither undefined nor null, then throw a TypeError."
/// `None` for `null`, whose members are all left out, as `undefined`'s are;
/// an array is an Object, and its members are read from it.
///
/// So is a function, which Neon's `JsObject` is not: its members are read
/// from a copy, `Object.assign({}, value)`, which takes the own enumerable
/// ones where WebIDL's `Get` would reach its prototype too -- the same
/// answer for any function anyone passes as options.
fn to_dictionary<'a>(
    cx: &mut FunctionContext<'a>,
    value: Handle<'a, JsValue>,
    prefix: &str,
    key: &str,
) -> NeonResult<Option<Handle<'a, JsObject>>> {
    if value.is_a::<JsNull, _>(cx) || value.is_a::<JsUndefined, _>(cx) {
        return Ok(None);
    }
    if let Ok(object) = value.downcast::<JsObject, _>(cx) {
        return Ok(Some(object));
    }
    if value.is_a::<JsFunction, _>(cx) {
        let object: Handle<JsFunction> = cx.global("Object")?;
        let assign: Handle<JsFunction> = object.get(cx, "assign")?;
        let copy = cx.empty_object();
        let members = assign
            .call_with(cx)
            .arg(copy)
            .arg(value)
            .apply::<JsObject, _>(cx)?;
        return Ok(Some(members));
    }
    wrong_kind(cx, prefix, key, "an object", value)
}

/// An option holding an object of options, as an IDL dictionary -- see
/// [`to_dictionary`].
fn object_option<'a>(
    cx: &mut FunctionContext<'a>,
    obj: &Handle<'a, JsObject>,
    prefix: &str,
    key: &str,
) -> NeonResult<Option<Handle<'a, JsObject>>> {
    match option_value(cx, obj, key)? {
        Some(value) => to_dictionary(cx, value, prefix, key),
        None => Ok(None),
    }
}

/// `value`'s elements where it is iterable, or `None` where it is not: the
/// test both WebIDL's sequence conversion and its union conversion make,
/// "Let method be ? GetMethod(V, %Symbol.iterator%)". An array is read as it
/// is; any other iterable through `Array.from`, which runs its iterator.
fn iterated<'a>(
    cx: &mut FunctionContext<'a>,
    value: Handle<'a, JsValue>,
) -> NeonResult<Option<Vec<Handle<'a, JsValue>>>> {
    if let Ok(array) = value.downcast::<JsArray, _>(cx) {
        return array.to_vec(cx).map(Some);
    }
    let Ok(object) = value.downcast::<JsObject, _>(cx) else {
        return Ok(None);
    };
    // A function is an Object to WebIDL and not to Neon's `JsObject`; one
    // holding an iterator is not a list anyone writes, so it is not read
    // as one.
    let symbol: Handle<JsFunction> = cx.global("Symbol")?;
    let iterator: Handle<JsValue> = symbol.get(cx, "iterator")?;
    let method: Handle<JsValue> = object.get_value(cx, iterator)?;
    if !method.is_a::<JsFunction, _>(cx) {
        return Ok(None);
    }
    let array: Handle<JsFunction> = cx.global("Array")?;
    let from: Handle<JsFunction> = array.get(cx, "from")?;
    let elements = from.call_with(cx).arg(value).apply::<JsArray, _>(cx)?;
    elements.to_vec(cx).map(Some)
}

/// `value` as an IDL `sequence`, WebIDL section 3.2.21: "If V is not an
/// Object, throw a TypeError. Let method be ? GetMethod(V,
/// %Symbol.iterator%). If method is undefined, throw a TypeError." The
/// elements, each still to be converted to the sequence's type.
fn to_sequence<'a>(
    cx: &mut FunctionContext<'a>,
    value: Handle<'a, JsValue>,
    prefix: &str,
    key: &str,
    takes: &str,
) -> NeonResult<Vec<Handle<'a, JsValue>>> {
    match iterated(cx, value)? {
        Some(elements) => Ok(elements),
        None => wrong_kind(cx, prefix, key, takes, value),
    }
}

/// A list option, as an IDL `sequence` -- see [`to_sequence`].
fn array_option<'a>(
    cx: &mut FunctionContext<'a>,
    obj: &Handle<'a, JsObject>,
    prefix: &str,
    key: &str,
    takes: &str,
) -> NeonResult<Option<Vec<Handle<'a, JsValue>>>> {
    match option_value(cx, obj, key)? {
        Some(value) => to_sequence(cx, value, prefix, key, takes).map(Some),
        None => Ok(None),
    }
}

/// `value` as a sequence of exactly `N` doubles, or the refusal naming the
/// option: a sequence of another length is a `TypeError` too, by rule 3, as
/// `new DOMMatrix([1, 2, 3])` is in Chrome.
fn to_doubles<'a, const N: usize>(
    cx: &mut FunctionContext<'a>,
    elements: &[Handle<'a, JsValue>],
    prefix: &str,
    key: &str,
    takes: &str,
) -> NeonResult<[f64; N]> {
    if elements.len() != N {
        let got = format!("a list of {}", elements.len());
        return cx.throw_type_error(format!(
            "`{prefix}{key}` takes {takes}, not {got}"
        ));
    }
    let mut out = [0.0; N];
    for (at, element) in elements.iter().enumerate() {
        out[at] = to_double(cx, *element, prefix, &format!("{key}[{at}]"))?;
    }
    Ok(out)
}

/// A colour option -- `(DOMString or sequence<double>)`, converted as
/// WebIDL section 3.2.25 converts a union: an iterable is the sequence,
/// `[r, g, b, a]` sRGB components with straight alpha, `color(srgb r g b /
/// a)`, and anything else is `ToString`'d and read as CSS. Either way the
/// colour comes back with the space it is in: sRGB for the sequence, the
/// space a CSS string named. `None` where it is absent or does not parse,
/// which leaves the colour as it would have been.
fn colour_option<'a>(
    cx: &mut FunctionContext<'a>,
    obj: &Handle<'a, JsObject>,
    prefix: &str,
    key: &str,
) -> NeonResult<Option<(Color4f, ColorSpace)>> {
    const TAKES: &str = "a CSS colour string or an [r, g, b, a] array";
    let Some(value) = option_value(cx, obj, key)? else {
        return Ok(None);
    };
    if let Some(elements) = iterated(cx, value)? {
        let [r, g, b, a] = to_doubles::<4>(cx, &elements, prefix, key, TAKES)?;
        let color = Color4f::new(r as f32, g as f32, b as f32, a as f32);
        return Ok(Some((color, ColorSpace::new_srgb())));
    }
    if is_symbol(cx, value) {
        return wrong_kind(cx, prefix, key, TAKES, value);
    }
    let css = value.to_string(cx)?.value(cx);
    Ok(css_to_color4f_in_space(&css))
}

/// Parses `fontVariations` from a `TextStyleInput` JS object: `None` to
/// take the axes in force -- the key absent or `null`, or an empty list -- and
/// otherwise the style's own, which `"normal"` makes none, resetting to the
/// face's defaults as CSS `font-variation-settings: normal` does.
///
/// A list holds `{axis, value}` entries. Axis tags must be exactly 4 ASCII
/// characters (Skia convention, e.g. "wght", "wdth", "ital"). An entry
/// missing either, or with a tag of another length, is skipped -- the field
/// is optional and missing variations fall back to the typeface's default
/// axis positions. The option is `(DOMString or sequence<dictionary>)`,
/// converted as WebIDL converts a union: an iterable is the list, and
/// anything else is `ToString`'d. A string other than `"normal"`, in any
/// case, is a `TypeError`: it names a keyword the property does not have, so
/// what the caller asked for would not happen.
fn parse_font_variations<'a>(
    cx: &mut FunctionContext<'a>,
    obj: &Handle<'a, JsObject>,
    prefix: &str,
) -> NeonResult<Option<Vec<(FourByteTag, f32)>>> {
    let mut out: Vec<(FourByteTag, f32)> = Vec::new();
    // Nullable, as the string options are: `null` is the option left out.
    let Some(vars_val) = option_value(cx, obj, "fontVariations")? else {
        return Ok(None);
    };
    if vars_val.is_a::<JsNull, _>(cx) {
        return Ok(None);
    }
    let Some(entries) = iterated(cx, vars_val)? else {
        // A value outside an enumeration is a `TypeError` (AGENTS.md, rule
        // 2).
        let keyword = to_dom_string(cx, vars_val, prefix, "fontVariations")?;
        return match keyword.eq_ignore_ascii_case("normal") {
            true => Ok(Some(Vec::new())),
            false => cx.throw_type_error(format!(
                "Expected \"normal\" or a list of {{axis, value}} for \
                 `{prefix}fontVariations` (got {keyword:?})"
            )),
        };
    };
    for (at, v) in entries.into_iter().enumerate() {
        let key = format!("fontVariations[{at}]");
        // `null` is an entry with no axis and no value, and is skipped.
        let Some(v_obj) = to_dictionary(cx, v, prefix, &key)? else {
            continue;
        };
        let inner = format!("{prefix}{key}.");
        let axis = string_option(cx, &v_obj, &inner, "axis")?;
        let value = float_option(cx, &v_obj, &inner, "value")?;
        let (Some(axis), Some(value)) = (axis, value) else {
            continue;
        };
        let bytes = axis.as_bytes();
        if bytes.len() != 4 {
            continue;
        }
        let tag = FourByteTag::from_chars(
            bytes[0] as char,
            bytes[1] as char,
            bytes[2] as char,
            bytes[3] as char,
        );
        out.push((tag, value));
    }
    Ok((!out.is_empty()).then_some(out))
}

/// The keys [`parse_text_style`] reads. Anything else in the object is a
/// misspelling or a property this build does not have, and either way the
/// value a caller passed will not reach the layout.
const TEXT_STYLE_KEYS: &[&str] = &[
    "backgroundColor",
    "baselineShift",
    "color",
    "decoration",
    "decorationColor",
    "decorationStyle",
    "decorationThickness",
    "fontFamilies",
    "fontFeatures",
    "fontSize",
    "fontStyle",
    "fontVariations",
    "foregroundColor",
    "halfLeading",
    "heightMultiplier",
    "letterSpacing",
    "locale",
    "shadows",
    "strokeWidth",
    "wordSpacing",
];

/// The keys [`parse_paragraph_style`] reads.
const PARAGRAPH_STYLE_KEYS: &[&str] = &[
    "ellipsis",
    "maxLines",
    "strutStyle",
    "textAlign",
    "textDirection",
    "textHeightBehavior",
    "textStyle",
];

/// Whether the caller asked to hear about what the Canvas API would ignore.
///
/// Read once, as `lib/classes/neon.js` reads it, so the two halves agree
/// within a process. Consulted here rather than deferring to the `\u{26a0}`
/// marker that `rustError` filters on, because that marker only suits a
/// setter: it lets the Rust side abort and the JavaScript side swallow the
/// message, which for a *constructor* means the caller is handed nothing
/// back. Deciding here keeps the tolerant path from failing at all.
fn strict_mode() -> bool {
    static STRICT: OnceLock<bool> = OnceLock::new();
    *STRICT.get_or_init(|| {
        !matches!(
            std::env::var("SKIA_CANVAS_STRICT")
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase()
                .as_str(),
            "" | "0" | "false" | "off"
        )
    })
}

/// Refuses a key the parser does not read, under `SKIA_CANVAS_STRICT`.
///
/// Silent otherwise, which is how the Canvas API treats a value it does not
/// recognise.
///
/// Worth saying at all because the failure it catches is invisible. A style
/// object is read key by key, so an unrecognised one is not an error, it is
/// an absence -- `{ locale: "ja" }` built a paragraph and laid it out and
/// changed nothing for as long as `locale` went unread, and a misspelled
/// `fontsize` leaves the size at its default with nothing to see.
fn refuse_unknown_keys(
    cx: &mut FunctionContext,
    obj: &Handle<JsObject>,
    known: &[&str],
    what: &str,
) -> NeonResult<()> {
    if !strict_mode() {
        return Ok(());
    }
    let keys = obj.get_own_property_names(cx)?.to_vec(cx)?;
    for name in strings_in(cx, &keys) {
        if !known.contains(&name.as_str()) {
            cx.throw_type_error::<_, ()>(format!(
                "Unknown {what} key `{name}`"
            ))?;
        }
    }
    Ok(())
}

/// The strings of a `fontFamilies` list, each converted as a `DOMString`, or
/// `None` where the list is absent or empty: no family names the default
/// list, as the option left out does.
fn font_families<'a>(
    cx: &mut FunctionContext<'a>,
    obj: &Handle<'a, JsObject>,
    prefix: &str,
) -> NeonResult<Option<Vec<String>>> {
    let Some(names) =
        array_option(cx, obj, prefix, "fontFamilies", "a list of strings")?
    else {
        return Ok(None);
    };
    // UPSTREAM: skia-safe 0.153.3 -- unfiled -- worked around
    // Re-check: cargo test an_empty_family_list_is_a_null_slice_in_skia_safe
    // `NativeSliceAccess::native` makes a slice from the first element's
    // pointer, null for an empty list, and `set_font_families` hands it one:
    // a debug build's precondition check aborts the process there, and a
    // release build passes a null slice on. The crate's own styles never set
    // an empty list, so only this binding reached it.
    let families = names
        .into_iter()
        .enumerate()
        .map(|(at, name)| {
            to_dom_string(cx, name, prefix, &format!("fontFamilies[{at}]"))
        })
        .collect::<NeonResult<Vec<String>>>()?;
    Ok((!families.is_empty()).then_some(families))
}

/// The names the shared font collection is asked for `families` by: each
/// read as CSS reads an unquoted name, as `FontFamily`'s `From<&str>` does,
/// and lowered by `FontLibrary::collection_names`. CanvasKit's
/// `fontFamilies` has no quoting, so a generic keyword is the generic.
fn collection_names(families: &[String]) -> Vec<String> {
    let families: Vec<FontFamily> =
        families.iter().map(FontFamily::from).collect();
    FontLibrary::with_shared(|lib| lib.collection_names(&families))
}

/// A text style from `obj`, which sits at `prefix` among the options --
/// `textStyle.` for a paragraph's, nothing for a pushed style -- and the
/// angle of the oblique it asks for, if any: Skia's font style has no field
/// for one.
fn parse_text_style<'a>(
    cx: &mut FunctionContext<'a>,
    obj: &Handle<'a, JsObject>,
    prefix: &str,
) -> NeonResult<(TextStyle, Option<f32>)> {
    // Black where no colour is named, as CSS's initial `color` is and as
    // the declarations promise; SkParagraph's own default is white.
    let mut style = TextStyle::new();
    style.set_color(Color::BLACK);
    let mut oblique = None;

    refuse_unknown_keys(cx, obj, TEXT_STYLE_KEYS, "text style")?;

    // fontSize
    if let Some(size) = float_option(cx, obj, prefix, "fontSize")? {
        style.set_font_size(size);
    }

    // fontFamilies
    if let Some(families) = font_families(cx, obj, prefix)? {
        let families = collection_names(&families);
        style.set_font_families(&families);
    }

    // color / foregroundColor / backgroundColor accept a CSS string or a
    // `[r, g, b, a]` float array, which `colour_option` tags -- the string
    // with the space it named, the array as sRGB -- so Skia converts either
    // to the canvas's space at paint time.
    // Remembered so an unspecified decorationColor can fall back to it. The
    // text color goes in as a foreground *paint*, which leaves
    // TextStyle::color at its default, so the decoration cannot read it
    // back from the style.
    let mut text_color = None;
    if let Some((color4f, cs)) = colour_option(cx, obj, prefix, "color")? {
        let mut paint = Paint::default();
        let (paint_color, paint_space) = skia_paint_color(color4f, &cs);
        paint.set_color4f(paint_color, Some(&paint_space));
        style.set_foreground_paint(&paint);
        text_color = Some(color4f_to_skia_color(color4f, &cs));
    }
    if let Some((color4f, cs)) =
        colour_option(cx, obj, prefix, "foregroundColor")?
    {
        let mut paint = Paint::default();
        let (paint_color, paint_space) = skia_paint_color(color4f, &cs);
        paint.set_color4f(paint_color, Some(&paint_space));
        style.set_foreground_paint(&paint);
        text_color = Some(color4f_to_skia_color(color4f, &cs));
    }

    // backgroundColor
    if let Some((color4f, cs)) =
        colour_option(cx, obj, prefix, "backgroundColor")?
    {
        let mut paint = Paint::default();
        let (paint_color, paint_space) = skia_paint_color(color4f, &cs);
        paint.set_color4f(paint_color, Some(&paint_space));
        style.set_background_paint(&paint);
    }

    // fontStyle: { weight, width, slant }
    if let Some(fs_obj) = object_option(cx, obj, prefix, "fontStyle")? {
        let inner = format!("{prefix}fontStyle.");
        let weight = float_option(cx, &fs_obj, &inner, "weight")?
            .map(|w| Weight::from(w as i32))
            .unwrap_or(Weight::NORMAL);
        let width = float_option(cx, &fs_obj, &inner, "width")?
            .map(|w| Width::from(w as i32))
            .unwrap_or(Width::NORMAL);
        // Refused rather than defaulted, as the other numeric style codes in
        // this parser are: a caller reading a constant off the wrong object
        // gets a reason rather than an upright run.
        const SLANTS: [Slant; 3] =
            [Slant::Upright, Slant::Italic, Slant::Oblique];
        let slant = match number_option(cx, &fs_obj, &inner, "slant")? {
            None => Slant::Upright,
            Some(number) => code(cx, number, &SLANTS, "slant")?,
        };
        let angle = oblique_angle_for_key(cx, &fs_obj, &inner, slant)?;
        oblique = (slant == Slant::Oblique)
            .then(|| angle.unwrap_or(ObliqueAngle::DEFAULT).degrees());
        // An oblique of 0deg leans no way, so it asks for the upright face
        // that draws it, as `TextSlant::to_skia` does.
        let slant = match oblique {
            Some(0.0) => Slant::Upright,
            _ => slant,
        };
        style.set_font_style(FontStyle::new(weight, width, slant));
    }

    // letterSpacing
    if let Some(ls) = float_option(cx, obj, prefix, "letterSpacing")? {
        style.set_letter_spacing(ls);
    }

    // wordSpacing
    if let Some(ws) = float_option(cx, obj, prefix, "wordSpacing")? {
        style.set_word_spacing(ws);
    }

    // baselineShift -- moves the run off the baseline, which is what a
    // superscript needs. Negative lifts, positive drops, and zero is the
    // default, so the value is taken as given rather than filtered the way
    // `strokeWidth` is. The paragraph grows to contain the moved run: the
    // line box is not preserved, whatever the sign.
    if let Some(shift) = float_option(cx, obj, prefix, "baselineShift")? {
        style.set_baseline_shift(shift);
    }

    // heightMultiplier
    if let Some(hm) = float_option(cx, obj, prefix, "heightMultiplier")? {
        style.set_height(hm);
        style.set_height_override(true);
    }

    // decoration (bitmask)
    //
    // The JavaScript side numbers these the way CanvasKit does, which is the
    // way Skia does -- underline 1, overline 2, line-through 4. So the mask
    // arrives already in `TextDecoration`'s own representation, and a whole
    // number holding only those bits is taken apart into the three flags.
    // Anything else is refused, by rule 4 as a code outside its set is: a
    // fraction or a bit Skia does not define draws no line the caller asked
    // for, and a cast to `u32` truncated the one and dropped the other.
    if let Some(deco) = number_option(cx, obj, prefix, "decoration")? {
        let defined = (TextDecoration::UNDERLINE
            | TextDecoration::OVERLINE
            | TextDecoration::LINE_THROUGH)
            .bits();
        // A fraction, a negative and a number past `u32::MAX` do not come
        // back from the cast unchanged.
        let bits = deco as u32;
        if f64::from(bits) != deco || bits & !defined != 0 {
            let spelled = spelled(cx, deco)?;
            return cx.throw_range_error(format!(
                "Unknown decoration {spelled} (expected a mask of 0 to {defined})"
            ));
        }
        let decoration = CrateTextDecoration {
            underline: bits & TextDecoration::UNDERLINE.bits() != 0,
            overline: bits & TextDecoration::OVERLINE.bits() != 0,
            line_through: bits & TextDecoration::LINE_THROUGH.bits() != 0,
        };
        style.set_decoration_type(decoration.to_skia_flags());
    }

    // decorationStyle
    //
    // Refused rather than defaulted outside the set, so a caller reading a
    // constant off the wrong object gets a reason rather than a solid line.
    if let Some(ds) = number_option(cx, obj, prefix, "decorationStyle")? {
        const STYLES: [CrateTextDecorationStyle; 5] = [
            CrateTextDecorationStyle::Solid,
            CrateTextDecorationStyle::Double,
            CrateTextDecorationStyle::Dotted,
            CrateTextDecorationStyle::Dashed,
            CrateTextDecorationStyle::Wavy,
        ];
        let style_code = code(cx, ds, &STYLES, "decorationStyle")?;
        style.set_decoration_style(style_code.to_skia());
    }

    // UPSTREAM: skia-safe 0.153.3 -- unfiled -- worked around
    // Re-check: grep -n "fn set_decoration_color\|fn new(color"
    // ~/.cargo/registry/src/*/skia-safe-*/src/modules/paragraph/text_style.rs
    // ~/.cargo/registry/src/*/skia-safe-*/src/modules/paragraph/text_shadow.rs
    // SkParagraph takes a decoration's colour, and a text shadow's, as an
    // eight-bit sRGB `SkColor` and nothing wider, so `color4f_to_skia_color`
    // converts from the space `colour_option` tagged and gamut-maps a wide
    // colour into sRGB on the way in.
    let mut decoration_color = None;
    if let Some((color4f, cs)) =
        colour_option(cx, obj, prefix, "decorationColor")?
    {
        decoration_color = Some(color4f_to_skia_color(color4f, &cs));
    }
    // Fall back to the text color, as CSS's `currentcolor` does and as the
    // native TextStyle documents, and to the style's own colour where none
    // was named at all, which is the black the text is drawn in. Skia
    // defaults the decoration's colour to transparent, so a line left at
    // that default is invisible.
    style.set_decoration_color(
        decoration_color.or(text_color).unwrap_or(style.color()),
    );

    // decorationThickness
    if let Some(dt) = float_option(cx, obj, prefix, "decorationThickness")? {
        style.set_decoration_thickness_multiplier(dt);
    }

    // decoration mode
    style.set_decoration_mode(TextDecorationMode::Through);

    // `fontVariations` is read by the callers rather than here: the
    // paragraph's by `paragraph::new`, which instances the builder's
    // collection at it, and a pushed style's by `pushStyle`, which sets
    // the position as the style's font arguments.

    // shadows: [{ color, offset: [dx, dy], blurRadius }]
    if let Some(shadows) =
        array_option(cx, obj, prefix, "shadows", "a list of shadows")?
    {
        for (at, shadow_val) in shadows.into_iter().enumerate() {
            let key = format!("shadows[{at}]");
            // `null` is a shadow with every member left out, as `{}` is:
            // black, unmoved and unblurred.
            let shadow_obj = to_dictionary(cx, shadow_val, prefix, &key)?;
            let inner = format!("{prefix}{key}.");
            let mut color = Color::BLACK;
            let mut offset = Point::new(0.0, 0.0);
            let mut blur = 0.0;
            if let Some(shadow_obj) = shadow_obj {
                // Eight-bit, as decorationColor is and for the same reason.
                if let Some((c, cs)) =
                    colour_option(cx, &shadow_obj, &inner, "color")?
                {
                    color = color4f_to_skia_color(c, &cs);
                }

                if let Some(offset_val) =
                    option_value(cx, &shadow_obj, "offset")?
                {
                    const TAKES: &str = "an [x, y] array of two numbers";
                    let pair =
                        to_sequence(cx, offset_val, &inner, "offset", TAKES)?;
                    let [dx, dy] =
                        to_doubles::<2>(cx, &pair, &inner, "offset", TAKES)?;
                    offset = Point::new(dx as f32, dy as f32);
                }

                // Halved for the same reason `shadowBlur` is, and against
                // the same sentence: sigma is exactly half the blur radius.
                // https://www.w3.org/TR/css-backgrounds-3/#shadow-blur
                //
                // `TextShadow`'s third argument is Skia's sigma -- which is
                // what the Rust side calls it, `TextStyle::blur_sigma` --
                // and this key is a radius, so handing it over unscaled made
                // one library answer a single number two ways:
                // `shadowBlur = 8` on a context blurred half as far as
                // `blurRadius: 8` on a paragraph.
                blur = float_option(cx, &shadow_obj, &inner, "blurRadius")?
                    .unwrap_or(0.0)
                    * 0.5;
            }

            style.add_shadow(TextShadow::new(color, offset, blur as f64));
        }
    }

    // fontFeatures: [{ name, value }] -- OpenType features (smcp, liga,
    // onum, ss01, ...). Mirrors CanvasKit's `TextStyle.fontFeatures`. The
    // Canvas2D path reaches features via `fontVariant`; the paragraph
    // path needs them here so multi-style server text can drive small
    // caps and friends. `value` defaults to `1` (enable) when omitted.
    if let Some(features) =
        array_option(cx, obj, prefix, "fontFeatures", "a list of features")?
    {
        for (at, feat_val) in features.into_iter().enumerate() {
            let key = format!("fontFeatures[{at}]");
            // `null` is a feature with no name, and is skipped.
            let Some(feat_obj) = to_dictionary(cx, feat_val, prefix, &key)?
            else {
                continue;
            };
            let inner = format!("{prefix}{key}.");
            let Some(name) = string_option(cx, &feat_obj, &inner, "name")?
            else {
                continue;
            };
            if name.is_empty() {
                continue;
            }
            // A feature's value is a whole number, as CSS's
            // `font-feature-settings` takes, up to the `i32` Skia carries.
            // Rule 4 refuses another, which a cast truncated or saturated.
            let value = match number_option(cx, &feat_obj, &inner, "value")? {
                None => 1,
                Some(number) => {
                    let whole = number as i32;
                    if whole < 0 || f64::from(whole) != number {
                        let spelled = spelled(cx, number)?;
                        return cx.throw_range_error(format!(
                            "`{inner}value` takes a whole number from 0 to {}, not \
                             {spelled}",
                            i32::MAX
                        ));
                    }
                    whole
                }
            };
            style.add_font_feature(name, value);
        }
    }

    // strokeWidth -- outline the glyphs rather than filling them, as CSS
    // `-webkit-text-stroke` does. Not positive is ignored, matching
    // `lineWidth`; Skia would take zero as a hairline instead.
    if let Some(width) = float_option(cx, obj, prefix, "strokeWidth")?
        && width > 0.0
    {
        let mut paint = Paint::default();
        paint.set_anti_alias(true);
        paint.set_color(style.foreground().color());
        paint.set_style(PaintStyle::Stroke);
        paint.set_stroke_width(width);
        style.set_foreground_paint(&paint);
    }

    // locale -- the BCP 47 tag the run is written in, which decides which
    // language's letterform a unified codepoint is drawn with. Han
    // characters share codepoints across Japanese and Chinese and differ in
    // shape, and nothing in the text says which the reader should see.
    if let Some(locale) = string_option(cx, obj, prefix, "locale")? {
        style.set_locale(&locale);
    }

    // halfLeading -- distribute leading half above / half below the text.
    if let Some(half) = bool_option(cx, obj, "halfLeading")? {
        style.set_half_leading(half);
    }

    Ok((style, oblique))
}

/// `fontStyle.obliqueAngle` in degrees, `None` where it is not given.
///
/// Converted as the other number options are, as an IDL `double`, so `"10"`
/// is 10 and NaN or an infinity is a `TypeError`. Only an oblique has an
/// angle, so one given with another `slant` is a `TypeError` naming the
/// fix: the caller asked for an angle that would not be drawn, which is
/// substitutive and refused in every mode. Outside -90deg to 90deg, the
/// range CSS Fonts 4 allows, it is a `RangeError` -- rule 4, a number whose
/// value is not one the range holds.
fn oblique_angle_for_key<'a>(
    cx: &mut FunctionContext<'a>,
    font_style: &Handle<'a, JsObject>,
    prefix: &str,
    slant: Slant,
) -> NeonResult<Option<ObliqueAngle>> {
    let Some(degrees) = number_option(cx, font_style, prefix, "obliqueAngle")?
    else {
        return Ok(None);
    };
    if slant != Slant::Oblique {
        return cx.throw_type_error(format!(
            "`{prefix}obliqueAngle` needs `{prefix}slant: 2` (oblique)"
        ));
    }
    // Checked at full width: a number just past 90 rounds to 90 as an `f32`.
    if degrees.abs() > f64::from(ObliqueAngle::MAX_DEGREES) {
        let spelled = cx.number(degrees).to_string(cx)?.value(cx);
        return cx.throw_range_error(format!(
            "`{prefix}obliqueAngle` {spelled} is outside -90 to 90"
        ));
    }
    match ObliqueAngle::new(degrees as f32) {
        Ok(angle) => Ok(Some(angle)),
        Err(error) => cx.throw_range_error(error.to_string()),
    }
}

/// A paragraph style, and the angle of the oblique its text style asks for,
/// if any.
fn parse_paragraph_style<'a>(
    cx: &mut FunctionContext<'a>,
    obj: &Handle<'a, JsObject>,
) -> NeonResult<(ParagraphStyle, Option<f32>)> {
    let mut style = ParagraphStyle::new();
    let mut oblique = None;

    refuse_unknown_keys(cx, obj, PARAGRAPH_STYLE_KEYS, "paragraph style")?;

    // textAlign
    //
    // Through the same parser `ctx.textAlign` uses rather than a second copy
    // of the six names: this file spelled them out again, so the two could
    // drift and nothing would have said so. Another name is a `TypeError` by
    // rule 2, as for `textDirection`, where the context's attribute ignores
    // one as HTML's setter does.
    if let Some(name) = string_option(cx, obj, "", "textAlign")? {
        let Some(align) = to_text_align(&name) else {
            return cx.throw_type_error(format!(
                "`textAlign` takes \"left\", \"right\", \"center\", \"justify\", \
                 \"start\" or \"end\", not {name:?}"
            ));
        };
        style.set_text_align(align.to_skia());
    }

    // textDirection, matched exactly as WebIDL matches an enumeration's
    // values. Another name is a `TypeError` by rule 2, as WebIDL raises for
    // a dictionary member of an enumeration type.
    if let Some(dir_str) = string_option(cx, obj, "", "textDirection")? {
        let direction = match dir_str.as_str() {
            "rtl" => TextDirection::RTL,
            "ltr" => TextDirection::LTR,
            _ => {
                return cx.throw_type_error(format!(
                    "`textDirection` takes \"ltr\" or \"rtl\", not {dir_str:?}"
                ));
            }
        };
        style.set_text_direction(direction);
    }

    // maxLines -- a whole number of lines from 1, and rule 4 refuses another:
    // Skia lays out one line for 0, so neither 0 nor a fraction can be what
    // the caller asked for. One past `usize::MAX` saturates to it, which is
    // as unlimited as the number asked.
    if let Some(max) = number_option(cx, obj, "", "maxLines")? {
        if max.fract() != 0.0 || max < 1.0 {
            let spelled = spelled(cx, max)?;
            return cx.throw_range_error(format!(
                "`maxLines` takes a whole number of lines from 1, not {spelled}"
            ));
        }
        style.set_max_lines(Some(max as usize));
    }

    // ellipsis
    if let Some(ell) = string_option(cx, obj, "", "ellipsis")?
        && !ell.is_empty()
    {
        style.set_ellipsis(ell);
    }

    // textStyle (default text style for the paragraph)
    if let Some(ts_obj) = object_option(cx, obj, "", "textStyle")? {
        let (text_style, asked) = parse_text_style(cx, &ts_obj, "textStyle.")?;
        style.set_text_style(&text_style);
        oblique = asked;
    }

    // textHeightBehavior: 0 = All, 1 = DisableFirstAscent,
    // 2 = DisableLastDescent, 3 = DisableAll, and nothing else.
    if let Some(thb) = number_option(cx, obj, "", "textHeightBehavior")? {
        const BEHAVIORS: [CrateTextHeightBehavior; 4] = [
            CrateTextHeightBehavior::All,
            CrateTextHeightBehavior::DisableFirstAscent,
            CrateTextHeightBehavior::DisableLastDescent,
            CrateTextHeightBehavior::DisableAll,
        ];
        let behavior = code(cx, thb, &BEHAVIORS, "textHeightBehavior")?;
        style.set_text_height_behavior(behavior.to_skia());
    }

    // strutStyle: a fixed line box for deterministic leading. Presence of
    // the object enables the strut unless `enabled: false` is set.
    if let Some(strut_obj) = object_option(cx, obj, "", "strutStyle")? {
        const STRUT: &str = "strutStyle.";
        let mut strut = StrutStyle::new();
        strut.set_strut_enabled(
            bool_option(cx, &strut_obj, "enabled")?.unwrap_or(true),
        );
        if let Some(families) = font_families(cx, &strut_obj, STRUT)? {
            let families = collection_names(&families);
            strut.set_font_families(&families);
        }
        if let Some(size) = float_option(cx, &strut_obj, STRUT, "fontSize")? {
            strut.set_font_size(size);
        }
        if let Some(h) =
            float_option(cx, &strut_obj, STRUT, "heightMultiplier")?
        {
            strut.set_height(h);
            strut.set_height_override(true);
        }
        if let Some(l) = float_option(cx, &strut_obj, STRUT, "leading")? {
            strut.set_leading(l);
        }
        strut.set_force_strut_height(
            bool_option(cx, &strut_obj, "forceStrutHeight")?.unwrap_or(false),
        );
        strut.set_half_leading(
            bool_option(cx, &strut_obj, "halfLeading")?.unwrap_or(false),
        );
        style.set_strut_style(strut);
    }

    Ok((style, oblique))
}

/// Parses the `textStyle.fontVariations` array from a
/// `ParagraphStyleInput` JS object, mirroring `parse_font_variations` in
/// shape. Returned to
/// `paragraph::new` so the font collection used for typeface matching
/// can be the variable-instantiated one (otherwise the paragraph
/// engine renders at the typeface master regardless of explicit weight,
/// breaking parity with CanvasKit's behaviour on variable fonts).
fn parse_paragraph_font_variations<'a>(
    cx: &mut FunctionContext<'a>,
    obj: &Handle<'a, JsObject>,
) -> NeonResult<Vec<(FourByteTag, f32)>> {
    let Some(ts_obj) = object_option(cx, obj, "", "textStyle")? else {
        return Ok(Vec::new());
    };
    // A paragraph has nothing to inherit, so taking the axes in force and
    // `"normal"` are both no axes of its own.
    Ok(parse_font_variations(cx, &ts_obj, "textStyle.")?.unwrap_or_default())
}

//
// ParagraphBuilder FFI functions
//

pub fn new(mut cx: FunctionContext) -> JsResult<BoxedParagraphBuilder> {
    let style_arg = cx.argument::<JsObject>(1)?;
    let (para_style, asked) = parse_paragraph_style(&mut cx, &style_arg)?;
    let variations = parse_paragraph_font_variations(&mut cx, &style_arg)?;

    // Route through `fonts_for_style` so any variable typefaces in the
    // collection get instantiated at the requested axis positions (or
    // auto-`wght` from the text style's nominal weight when no
    // explicit variations are passed). Without this the paragraph
    // engine matches the typeface's master instance regardless of
    // weight and the rendered glyphs drift off the requested wght
    // axis -- a visible parity gap on variable fonts like Dosis vs
    // CanvasKit's render.
    let mut text_style = para_style.text_style().clone();
    // An oblique paragraph is instanced at `slnt` where its faces have the
    // axis, as a pushed oblique style is.
    let instanced = with_oblique_slnt(&variations, asked);
    // The matched face is for a caller deciding what Skia may synthesise;
    // a paragraph built here carries its own text style and does not.
    // Hinted, as every paragraph is drawn -- see `ParagraphRecipe::new`.
    let ((mut collection, _), epoch) = FontLibrary::with_shared(|lib| {
        (
            lib.set_hinting(true)
                .fonts_for_style(&text_style, &instanced),
            lib.epoch(),
        )
    });
    let base_oblique = asked.and_then(|degrees| {
        match_oblique(&mut text_style, &mut collection, degrees)
    });
    let mut para_style = para_style;
    // Only an oblique changes the style, and copying it back costs a copy
    // of every field it has.
    if let Some(degrees) = asked {
        let mut coordinates = Vec::new();
        add_slnt(&mut coordinates, degrees);
        set_coordinates(&mut text_style, &coordinates);
        para_style.set_text_style(&text_style);
    }

    Ok(cx.boxed(RefCell::new(ParagraphBuilderWrap {
        recipe: Some(ParagraphRecipe::new(
            para_style,
            base_oblique,
            collection.clone(),
            Some(epoch),
        )),
        collection,
        variations,
    })))
}

pub fn pushStyle(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let this = cx.argument::<BoxedParagraphBuilder>(0)?;
    let style_obj = cx.argument::<JsObject>(1)?;
    let (mut text_style, asked) = parse_text_style(&mut cx, &style_obj, "")?;
    let own = parse_font_variations(&mut cx, &style_obj, "")?;

    let mut this = this.borrow_mut();
    let ParagraphBuilderWrap {
        recipe,
        collection,
        variations: paragraph_variations,
    } = &mut *this;
    // A style's own `fontVariations` replace the paragraph's, as a nested
    // `font-variation-settings` replaces its parent's in CSS, and `"normal"`
    // replaces them with none; with none of its own it takes the
    // paragraph's. Its weight, stretch and font size supply
    // `wght`, `wdth` and `opsz` unless the list in force names them. The
    // collection holds the paragraph's faces instanced at its own style, and
    // the position set here names every axis, so nothing of the paragraph's
    // carries over.
    let variations = own.as_deref().unwrap_or(paragraph_variations);
    // An oblique is set at `slnt` where the face has the axis, ahead of the
    // style's own variations so that a `slnt` they name wins, and is laid
    // out upright where no face leans as asked -- see `match_oblique`.
    let variations = with_oblique_slnt(variations, asked);
    let oblique = asked.and_then(|degrees| {
        match_oblique(&mut text_style, collection, degrees)
    });
    let families = text_style.font_families();
    let families: Vec<&str> = families.iter().collect();
    let faces = collection.find_typefaces(&families, text_style.font_style());
    let axes = FontLibrary::with_shared(|lib| {
        faces.iter().find_map(|face| lib.axes_of(face))
    });
    let mut coordinates = match axes {
        Some(axes) => {
            let coordinates =
                instance_coordinates(&axes, &text_style, &variations);
            if FontLibrary::with_shared(|lib| {
                lib.admit_position(&families, &coordinates)
            }) {
                collection.clear_caches();
            }
            coordinates
        }
        None => Vec::new(),
    };
    if let Some(degrees) = asked {
        add_slnt(&mut coordinates, degrees);
    }
    set_coordinates(&mut text_style, &coordinates);
    if let Some(recipe) = recipe.as_mut() {
        recipe.push_style(text_style, oblique);
    }
    Ok(cx.undefined())
}

pub fn pop(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let this = cx.argument::<BoxedParagraphBuilder>(0)?;
    let mut this = this.borrow_mut();
    if let Some(recipe) = this.recipe.as_mut() {
        recipe.pop();
    }
    Ok(cx.undefined())
}

pub fn addText(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let this = cx.argument::<BoxedParagraphBuilder>(0)?;
    let text = string_arg(&mut cx, 1, "text")?;

    let mut this = this.borrow_mut();
    if let Some(recipe) = this.recipe.as_mut() {
        recipe.add_text(&text);
    }
    Ok(cx.undefined())
}

pub fn addPlaceholder(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let this = cx.argument::<BoxedParagraphBuilder>(0)?;
    let width = float_arg_or_bail(&mut cx, 1, "width")?;
    let height = float_arg_or_bail(&mut cx, 2, "height")?;
    // The numbering is CanvasKit's.
    const ALIGNMENTS: [CratePlaceholderAlignment; 6] = [
        CratePlaceholderAlignment::Baseline,
        CratePlaceholderAlignment::AboveBaseline,
        CratePlaceholderAlignment::BelowBaseline,
        CratePlaceholderAlignment::Top,
        CratePlaceholderAlignment::Bottom,
        CratePlaceholderAlignment::Middle,
    ];
    const BASELINES: [PlaceholderBaseline; 2] = [
        PlaceholderBaseline::Alphabetic,
        PlaceholderBaseline::Ideographic,
    ];
    let align = opt_float_arg(&mut cx, 3).unwrap_or(0.0);
    let alignment =
        code(&mut cx, f64::from(align), &ALIGNMENTS, "placeholder align")?;
    let baseline = opt_float_arg(&mut cx, 4).unwrap_or(0.0);
    let baseline = code(
        &mut cx,
        f64::from(baseline),
        &BASELINES,
        "placeholder baseline",
    )?;

    let offset = opt_float_arg(&mut cx, 5).unwrap_or(0.0);

    let placeholder = PlaceholderStyle {
        width,
        height,
        alignment: alignment.to_skia(),
        baseline: baseline.to_skia(),
        baseline_offset: offset,
    };

    let mut this = this.borrow_mut();
    if let Some(recipe) = this.recipe.as_mut() {
        recipe.add_placeholder(&placeholder);
    }
    Ok(cx.undefined())
}

pub fn build(mut cx: FunctionContext) -> JsResult<BoxedParagraph> {
    let this = cx.argument::<BoxedParagraphBuilder>(0)?;
    let mut this = this.borrow_mut();

    match this.recipe.take() {
        Some(recipe) => {
            let uncached = recipe.uncached();
            let (paragraph, passes) = recipe.build();
            Ok(cx.boxed(RefCell::new(ParagraphWrap {
                paragraph,
                passes,
                uncached,
            })))
        }
        None => cx.throw_error(
            "ParagraphBuilder has already been consumed by build()",
        ),
    }
}

//
// Paragraph FFI functions
//

pub fn layout(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let this = cx.argument::<BoxedParagraph>(0)?;
    let width = float_arg_or_bail(&mut cx, 1, "width")?;

    let mut this = this.borrow_mut();
    let ParagraphWrap {
        paragraph,
        passes,
        uncached,
    } = &mut *this;
    without_paragraph_cache(uncached.as_ref(), || {
        paragraph.layout(width);
        if let Some(passes) = passes {
            passes.lay_out(paragraph, width);
        }
    });
    Ok(cx.undefined())
}

pub fn paint(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    // This is handled via drawParagraph on the context side
    cx.throw_error("Use ctx.drawParagraph() instead of Paragraph.paint()")
}

pub fn getHeight(mut cx: FunctionContext) -> JsResult<JsNumber> {
    let this = cx.argument::<BoxedParagraph>(0)?;
    let this = this.borrow();
    Ok(cx.number(this.paragraph.height()))
}

pub fn getLongestLine(mut cx: FunctionContext) -> JsResult<JsNumber> {
    let this = cx.argument::<BoxedParagraph>(0)?;
    let this = this.borrow();
    Ok(cx.number(this.paragraph.longest_line()))
}

pub fn getMaxWidth(mut cx: FunctionContext) -> JsResult<JsNumber> {
    let this = cx.argument::<BoxedParagraph>(0)?;
    let this = this.borrow();
    Ok(cx.number(this.paragraph.max_width()))
}

pub fn getMaxIntrinsicWidth(mut cx: FunctionContext) -> JsResult<JsNumber> {
    let this = cx.argument::<BoxedParagraph>(0)?;
    let this = this.borrow();
    Ok(cx.number(this.paragraph.max_intrinsic_width()))
}

pub fn getMinIntrinsicWidth(mut cx: FunctionContext) -> JsResult<JsNumber> {
    let this = cx.argument::<BoxedParagraph>(0)?;
    let this = this.borrow();
    Ok(cx.number(this.paragraph.min_intrinsic_width()))
}

pub fn getAlphabeticBaseline(mut cx: FunctionContext) -> JsResult<JsNumber> {
    let this = cx.argument::<BoxedParagraph>(0)?;
    let this = this.borrow();
    Ok(cx.number(this.paragraph.alphabetic_baseline()))
}

pub fn getIdeographicBaseline(mut cx: FunctionContext) -> JsResult<JsNumber> {
    let this = cx.argument::<BoxedParagraph>(0)?;
    let this = this.borrow();
    Ok(cx.number(this.paragraph.ideographic_baseline()))
}

pub fn getLineMetrics(mut cx: FunctionContext) -> JsResult<JsArray> {
    let this = cx.argument::<BoxedParagraph>(0)?;
    let this = this.borrow();

    let metrics = this.paragraph.get_line_metrics();
    let result = JsArray::new(&mut cx, metrics.len());

    for (i, m) in metrics.iter().enumerate() {
        let obj = JsObject::new(&mut cx);

        let v = cx.number(m.start_index as f64);
        obj.set(&mut cx, "startIndex", v)?;
        let v = cx.number(m.end_index as f64);
        obj.set(&mut cx, "endIndex", v)?;
        let v = cx.number(m.end_excluding_whitespaces as f64);
        obj.set(&mut cx, "endExcludingWhitespaces", v)?;
        let v = cx.number(m.end_including_newline as f64);
        obj.set(&mut cx, "endIncludingNewline", v)?;
        let v = cx.boolean(m.hard_break);
        obj.set(&mut cx, "isHardBreak", v)?;
        let v = cx.number(m.ascent);
        obj.set(&mut cx, "ascent", v)?;
        let v = cx.number(m.descent);
        obj.set(&mut cx, "descent", v)?;
        let v = cx.number(m.height);
        obj.set(&mut cx, "height", v)?;
        let v = cx.number(m.width);
        obj.set(&mut cx, "width", v)?;
        let v = cx.number(m.left);
        obj.set(&mut cx, "left", v)?;
        let v = cx.number(m.baseline);
        obj.set(&mut cx, "baseline", v)?;
        let v = cx.number(m.line_number as f64);
        obj.set(&mut cx, "lineNumber", v)?;

        result.set(&mut cx, i as u32, obj)?;
    }

    Ok(result)
}

pub fn getGlyphPositionAtCoordinate(
    mut cx: FunctionContext,
) -> JsResult<JsObject> {
    let this = cx.argument::<BoxedParagraph>(0)?;
    let x = float_arg_or_bail(&mut cx, 1, "x")?;
    let y = float_arg_or_bail(&mut cx, 2, "y")?;

    let this = this.borrow();
    let pos = this.paragraph.get_glyph_position_at_coordinate((x, y));

    let result = JsObject::new(&mut cx);
    let pos_val = cx.number(pos.position as f64);
    result.set(&mut cx, "pos", pos_val)?;
    let affinity_val = cx.number(pos.affinity as i32 as f64);
    result.set(&mut cx, "affinity", affinity_val)?;

    Ok(result)
}

pub fn getRectsForRange(mut cx: FunctionContext) -> JsResult<JsArray> {
    let this = cx.argument::<BoxedParagraph>(0)?;
    let start = float_arg_or_bail(&mut cx, 1, "start")? as usize;
    let end = float_arg_or_bail(&mut cx, 2, "end")? as usize;
    const HEIGHT_STYLES: [CrateRectHeightStyle; 6] = [
        CrateRectHeightStyle::Tight,
        CrateRectHeightStyle::Max,
        CrateRectHeightStyle::IncludeLineSpacingMiddle,
        CrateRectHeightStyle::IncludeLineSpacingTop,
        CrateRectHeightStyle::IncludeLineSpacingBottom,
        CrateRectHeightStyle::Strut,
    ];
    const WIDTH_STYLES: [CrateRectWidthStyle; 2] =
        [CrateRectWidthStyle::Tight, CrateRectWidthStyle::Max];
    let h_style = opt_float_arg(&mut cx, 3).unwrap_or(0.0);
    let w_style = opt_float_arg(&mut cx, 4).unwrap_or(0.0);
    let rect_height_style = code(
        &mut cx,
        f64::from(h_style),
        &HEIGHT_STYLES,
        "rect height style",
    )?;
    let rect_width_style = code(
        &mut cx,
        f64::from(w_style),
        &WIDTH_STYLES,
        "rect width style",
    )?;

    let this = this.borrow();
    let boxes = this.paragraph.get_rects_for_range(
        start..end,
        rect_height_style.to_skia(),
        rect_width_style.to_skia(),
    );

    let result = JsArray::new(&mut cx, boxes.len());
    for (i, tb) in boxes.iter().enumerate() {
        let obj = JsObject::new(&mut cx);

        let rect = JsArray::new(&mut cx, 4);
        let v = cx.number(tb.rect.left);
        rect.set(&mut cx, 0u32, v)?;
        let v = cx.number(tb.rect.top);
        rect.set(&mut cx, 1u32, v)?;
        let v = cx.number(tb.rect.right);
        rect.set(&mut cx, 2u32, v)?;
        let v = cx.number(tb.rect.bottom);
        rect.set(&mut cx, 3u32, v)?;
        obj.set(&mut cx, "rect", rect)?;

        let dir = cx.number(tb.direct as i32 as f64);
        obj.set(&mut cx, "direction", dir)?;

        result.set(&mut cx, i as u32, obj)?;
    }

    Ok(result)
}

pub fn didExceedMaxLines(mut cx: FunctionContext) -> JsResult<JsBoolean> {
    let this = cx.argument::<BoxedParagraph>(0)?;
    let this = this.borrow();
    Ok(cx.boolean(this.paragraph.did_exceed_max_lines()))
}

pub fn getNumberOfLines(mut cx: FunctionContext) -> JsResult<JsNumber> {
    let this = cx.argument::<BoxedParagraph>(0)?;
    let this = this.borrow();
    Ok(cx.number(this.paragraph.line_number() as f64))
}

pub fn getRectsForPlaceholders(mut cx: FunctionContext) -> JsResult<JsArray> {
    let this = cx.argument::<BoxedParagraph>(0)?;
    let this = this.borrow();
    let boxes = this.paragraph.get_rects_for_placeholders();

    let result = JsArray::new(&mut cx, boxes.len());
    for (i, tb) in boxes.iter().enumerate() {
        let obj = JsObject::new(&mut cx);

        let rect = JsArray::new(&mut cx, 4);
        let v = cx.number(tb.rect.left);
        rect.set(&mut cx, 0u32, v)?;
        let v = cx.number(tb.rect.top);
        rect.set(&mut cx, 1u32, v)?;
        let v = cx.number(tb.rect.right);
        rect.set(&mut cx, 2u32, v)?;
        let v = cx.number(tb.rect.bottom);
        rect.set(&mut cx, 3u32, v)?;
        obj.set(&mut cx, "rect", rect)?;

        let dir = cx.number(tb.direct as i32 as f64);
        obj.set(&mut cx, "direction", dir)?;

        result.set(&mut cx, i as u32, obj)?;
    }

    Ok(result)
}

pub fn getUnresolvedCodepoints(mut cx: FunctionContext) -> JsResult<JsArray> {
    let this = cx.argument::<BoxedParagraph>(0)?;
    // `unresolved_codepoints` mutates the paragraph (computed lazily).
    let mut this = this.borrow_mut();
    let codepoints = this.paragraph.unresolved_codepoints();

    let result = JsArray::new(&mut cx, codepoints.len());
    for (i, cp) in codepoints.iter().enumerate() {
        let v = cx.number(*cp as f64);
        result.set(&mut cx, i as u32, v)?;
    }

    Ok(result)
}

//
// Context integration: drawParagraph
//

pub fn drawParagraph(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let ctx = cx.argument::<crate::context::BoxedContext2D>(0)?;
    let para = cx.argument::<BoxedParagraph>(1)?;
    let x = float_arg_or_bail(&mut cx, 2, "x")?;
    let y = float_arg_or_bail(&mut cx, 3, "y")?;

    let para = para.borrow_mut();
    let ctx = ctx.borrow();

    // Composited, not bare: `Paragraph::paint` uses the text styles' own
    // paints, so the context's globalAlpha and globalCompositeOperation have
    // to be applied around it or they are silently dropped.
    ctx.with_composited_canvas(|canvas| {
        let at = Point::new(x, y);
        match &para.passes {
            Some(passes) => passes.paint(&para.paragraph, canvas, at),
            None => para.paragraph.paint(canvas, at),
        }
    });

    Ok(cx.undefined())
}
