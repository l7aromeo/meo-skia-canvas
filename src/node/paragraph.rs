//
// ParagraphBuilder & Paragraph wrappers for CanvasKit parity
//
#![allow(non_snake_case)]
use neon::prelude::*;
use std::cell::RefCell;

use skia_safe::{
    Color, ColorSpace, FourByteTag, Paint, Point,
    font_style::{FontStyle, Slant, Weight, Width},
    textlayout::{
        FontCollection, Paragraph as SkParagraph,
        ParagraphBuilder as SkParagraphBuilder, ParagraphStyle,
        PlaceholderAlignment, PlaceholderStyle, RectHeightStyle,
        RectWidthStyle, StrutStyle, TextAlign, TextBaseline, TextDecoration,
        TextDecorationMode, TextDecorationStyle, TextDirection,
        TextHeightBehavior, TextShadow, TextStyle,
    },
};

use crate::{font_library::FontLibrary, utils::*};

//
// Boxed wrapper types
//

pub struct ParagraphBuilderWrap {
    builder: Option<SkParagraphBuilder>,
    _collection: FontCollection,
}

impl Finalize for ParagraphBuilderWrap {}
pub type BoxedParagraphBuilder = JsBox<RefCell<ParagraphBuilderWrap>>;

pub struct ParagraphWrap {
    pub paragraph: SkParagraph,
}

impl Finalize for ParagraphWrap {}
pub type BoxedParagraph = JsBox<RefCell<ParagraphWrap>>;

//
// Style parsing helpers
//

/// Parses `fontVariations: [{axis, value}]` from a `TextStyleInput` JS
/// object. Axis tags must be exactly 4 ASCII characters (Skia
/// convention, e.g. "wght", "wdth", "ital"). Malformed entries are
/// silently skipped -- the field is optional and missing variations
/// fall back to the typeface's default axis positions.
fn parse_font_variations(
    cx: &mut FunctionContext,
    obj: &Handle<JsObject>,
) -> NeonResult<Vec<(FourByteTag, f32)>> {
    let mut out: Vec<(FourByteTag, f32)> = Vec::new();
    let Ok(vars_val) = obj.get::<JsValue, _, _>(cx, "fontVariations") else {
        return Ok(out);
    };
    let Ok(vars_arr) = vars_val.downcast::<JsArray, _>(cx) else {
        return Ok(out);
    };
    for v in vars_arr.to_vec(cx)? {
        let Ok(v_obj) = v.downcast::<JsObject, _>(cx) else {
            continue;
        };
        let Some(axis) = opt_string_for_key(cx, &v_obj, "axis") else {
            continue;
        };
        let Some(value) = opt_float_for_key(cx, &v_obj, "value") else {
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
    Ok(out)
}

fn parse_text_style(
    cx: &mut FunctionContext,
    obj: &Handle<JsObject>,
) -> NeonResult<TextStyle> {
    let mut style = TextStyle::new();

    // fontSize
    if let Some(size) = opt_float_for_key(cx, obj, "fontSize") {
        style.set_font_size(size);
    }

    // fontFamilies
    if let Ok(fam_val) = obj.get::<JsValue, _, _>(cx, "fontFamilies")
        && let Ok(fam_arr) = fam_val.downcast::<JsArray, _>(cx)
    {
        let fam_vec = fam_arr.to_vec(cx)?;
        let families = strings_in(cx, &fam_vec);
        style.set_font_families(&families);
    }

    // color / foregroundColor / backgroundColor accept either:
    //   * a CSS string -- tagged as sRGB by `color4f_in`,
    //   * a `[r, g, b, a]` float array -- tagged here as `srgb_linear` so Skia
    //     converts to the destination working color space at paint time instead
    //     of treating the linear values as sRGB-encoded.
    // Remembered so an unspecified decorationColor can fall back to it. The
    // text color goes in as a foreground *paint*, which leaves
    // TextStyle::color at its default, so the decoration cannot read it
    // back from the style.
    let mut text_color = None;
    if let Ok(color_val) = obj.get::<JsValue, _, _>(cx, "color")
        && let Some((color4f, cs)) = color4f_in(cx, color_val)
    {
        let mut paint = Paint::default();
        let cs = cs.unwrap_or_else(ColorSpace::new_srgb_linear);
        paint.set_color4f(color4f, Some(&cs));
        style.set_foreground_paint(&paint);
        text_color = Some(color4f.to_color());
    }
    if let Ok(color_val) = obj.get::<JsValue, _, _>(cx, "foregroundColor")
        && let Some((color4f, cs)) = color4f_in(cx, color_val)
    {
        let mut paint = Paint::default();
        let cs = cs.unwrap_or_else(ColorSpace::new_srgb_linear);
        paint.set_color4f(color4f, Some(&cs));
        style.set_foreground_paint(&paint);
        text_color = Some(color4f.to_color());
    }

    // backgroundColor
    if let Ok(color_val) = obj.get::<JsValue, _, _>(cx, "backgroundColor")
        && let Some((color4f, cs)) = color4f_in(cx, color_val)
    {
        let mut paint = Paint::default();
        let cs = cs.unwrap_or_else(ColorSpace::new_srgb_linear);
        paint.set_color4f(color4f, Some(&cs));
        style.set_background_paint(&paint);
    }

    // fontStyle: { weight, width, slant }
    if let Ok(fs_val) = obj.get::<JsValue, _, _>(cx, "fontStyle")
        && let Ok(fs_obj) = fs_val.downcast::<JsObject, _>(cx)
    {
        let weight = opt_float_for_key(cx, &fs_obj, "weight")
            .map(|w| Weight::from(w as i32))
            .unwrap_or(Weight::NORMAL);
        let width = opt_float_for_key(cx, &fs_obj, "width")
            .map(|w| Width::from(w as i32))
            .unwrap_or(Width::NORMAL);
        let slant = opt_float_for_key(cx, &fs_obj, "slant")
            .map(|s| match s as i32 {
                1 => Slant::Italic,
                2 => Slant::Oblique,
                _ => Slant::Upright,
            })
            .unwrap_or(Slant::Upright);
        style.set_font_style(FontStyle::new(weight, width, slant));
    }

    // letterSpacing
    if let Some(ls) = opt_float_for_key(cx, obj, "letterSpacing") {
        style.set_letter_spacing(ls);
    }

    // wordSpacing
    if let Some(ws) = opt_float_for_key(cx, obj, "wordSpacing") {
        style.set_word_spacing(ws);
    }

    // heightMultiplier
    if let Some(hm) = opt_float_for_key(cx, obj, "heightMultiplier") {
        style.set_height(hm);
        style.set_height_override(true);
    }

    // decoration (bitmask)
    if let Some(deco) = opt_float_for_key(cx, obj, "decoration") {
        let deco_val = deco as u32;
        let mut td = TextDecoration::NO_DECORATION;
        if deco_val & 0x1 != 0 {
            td |= TextDecoration::UNDERLINE;
        }
        if deco_val & 0x2 != 0 {
            td |= TextDecoration::OVERLINE;
        }
        if deco_val & 0x4 != 0 {
            td |= TextDecoration::LINE_THROUGH;
        }
        style.set_decoration_type(td);
    }

    // decorationStyle
    if let Some(ds) = opt_float_for_key(cx, obj, "decorationStyle") {
        style.set_decoration_style(match ds as i32 {
            1 => TextDecorationStyle::Double,
            2 => TextDecorationStyle::Dotted,
            3 => TextDecorationStyle::Dashed,
            4 => TextDecorationStyle::Wavy,
            _ => TextDecorationStyle::Solid,
        });
    }

    // decorationColor accepts a CSS string or a `[r, g, b, a]` linear float
    // array. Skia's `set_decoration_color` takes a u32 ARGB tag that it
    // treats as sRGB-encoded, so the linear-array path must gamma-encode
    // before quantizing -- otherwise Skia's implicit decode darkens the
    // decoration and drops the alpha precision the caller asked for.
    let mut decoration_color = None;
    if let Ok(color_val) = obj.get::<JsValue, _, _>(cx, "decorationColor")
        && let Some((color4f, cs)) = color4f_in(cx, color_val)
    {
        decoration_color = Some(if cs.is_some() {
            color4f.to_color()
        } else {
            linear_color4f_to_srgb_color(&color4f)
        });
    }
    // Fall back to the text color, as CSS does and as the native TextStyle
    // documents. Skia defaults it to transparent, so leaving it unset drew an
    // invisible decoration unless the caller also passed a decorationColor.
    if let Some(color) = decoration_color.or(text_color) {
        style.set_decoration_color(color);
    }

    // decorationThickness
    if let Some(dt) = opt_float_for_key(cx, obj, "decorationThickness") {
        style.set_decoration_thickness_multiplier(dt);
    }

    // decoration mode
    style.set_decoration_mode(TextDecorationMode::Through);

    // NOTE: `fontVariations` is parsed in `paragraph::new` instead of
    // here. `SkParagraphBuilder` reads its font collection at
    // construction time, not per-`pushStyle`, so the instantiated
    // variable typeface has to be seeded on the builder's collection.
    // A `style.set_typeface(face)` call here would be ignored by the
    // layout path (the collection-resident match takes precedence on
    // every glyph run), so we deliberately don't attempt one.

    // shadows: [{ color, offset: [dx, dy], blurRadius }]
    if let Ok(shadows_val) = obj.get::<JsValue, _, _>(cx, "shadows")
        && let Ok(shadows_arr) = shadows_val.downcast::<JsArray, _>(cx)
    {
        for shadow_val in shadows_arr.to_vec(cx)? {
            if let Ok(shadow_obj) = shadow_val.downcast::<JsObject, _>(cx) {
                // Shadow color follows the same rule as decorationColor:
                // CSS strings round-trip through u32 ARGB cleanly, linear
                // float arrays must be gamma-encoded first.
                let color = shadow_obj
                    .get::<JsValue, _, _>(cx, "color")
                    .ok()
                    .and_then(|v| color4f_in(cx, v))
                    .map(|(c, cs)| {
                        if cs.is_some() {
                            c.to_color()
                        } else {
                            linear_color4f_to_srgb_color(&c)
                        }
                    })
                    .unwrap_or(Color::BLACK);

                let mut offset = Point::new(0.0, 0.0);
                if let Ok(offset_val) =
                    shadow_obj.get::<JsValue, _, _>(cx, "offset")
                    && let Ok(offset_arr) =
                        offset_val.downcast::<JsArray, _>(cx)
                {
                    let vals = offset_arr.to_vec(cx)?;
                    if vals.len() >= 2
                        && let (Ok(dx), Ok(dy)) = (
                            vals[0].downcast::<JsNumber, _>(cx),
                            vals[1].downcast::<JsNumber, _>(cx),
                        )
                    {
                        offset = Point::new(
                            dx.value(cx) as f32,
                            dy.value(cx) as f32,
                        );
                    }
                }

                let blur = opt_float_for_key(cx, &shadow_obj, "blurRadius")
                    .unwrap_or(0.0);

                style.add_shadow(TextShadow::new(color, offset, blur as f64));
            }
        }
    }

    // fontFeatures: [{ name, value }] -- OpenType features (smcp, liga,
    // onum, ss01, ...). Mirrors CanvasKit's `TextStyle.fontFeatures`. The
    // Canvas2D path reaches features via `fontVariant`; the paragraph
    // path needs them here so multi-style server text can drive small
    // caps and friends. `value` defaults to `1` (enable) when omitted.
    if let Ok(feats_val) = obj.get::<JsValue, _, _>(cx, "fontFeatures")
        && let Ok(feats_arr) = feats_val.downcast::<JsArray, _>(cx)
    {
        for feat_val in feats_arr.to_vec(cx)? {
            if let Ok(feat_obj) = feat_val.downcast::<JsObject, _>(cx) {
                let Some(name) = opt_string_for_key(cx, &feat_obj, "name")
                else {
                    continue;
                };
                if name.is_empty() {
                    continue;
                }
                let value = opt_float_for_key(cx, &feat_obj, "value")
                    .unwrap_or(1.0) as i32;
                style.add_font_feature(name, value);
            }
        }
    }

    // halfLeading -- distribute leading half above / half below the text.
    if let Some(half) = opt_bool_for_key(cx, obj, "halfLeading") {
        style.set_half_leading(half);
    }

    Ok(style)
}

fn parse_paragraph_style(
    cx: &mut FunctionContext,
    obj: &Handle<JsObject>,
) -> NeonResult<ParagraphStyle> {
    let mut style = ParagraphStyle::new();

    // textAlign
    if let Some(align_str) = opt_string_for_key(cx, obj, "textAlign") {
        match align_str.to_lowercase().as_str() {
            "left" => {
                style.set_text_align(TextAlign::Left);
            }
            "right" => {
                style.set_text_align(TextAlign::Right);
            }
            "center" => {
                style.set_text_align(TextAlign::Center);
            }
            "justify" => {
                style.set_text_align(TextAlign::Justify);
            }
            "start" => {
                style.set_text_align(TextAlign::Start);
            }
            "end" => {
                style.set_text_align(TextAlign::End);
            }
            _ => {}
        }
    }

    // textDirection
    if let Some(dir_str) = opt_string_for_key(cx, obj, "textDirection") {
        match dir_str.to_lowercase().as_str() {
            "rtl" => {
                style.set_text_direction(TextDirection::RTL);
            }
            "ltr" => {
                style.set_text_direction(TextDirection::LTR);
            }
            _ => {}
        }
    }

    // maxLines
    if let Some(max) = opt_float_for_key(cx, obj, "maxLines") {
        style.set_max_lines(Some(max as usize));
    }

    // ellipsis
    if let Some(ell) = opt_string_for_key(cx, obj, "ellipsis")
        && !ell.is_empty()
    {
        style.set_ellipsis(ell);
    }

    // textStyle (default text style for the paragraph)
    if let Ok(ts_val) = obj.get::<JsValue, _, _>(cx, "textStyle")
        && let Ok(ts_obj) = ts_val.downcast::<JsObject, _>(cx)
    {
        let text_style = parse_text_style(cx, &ts_obj)?;
        style.set_text_style(&text_style);
    }

    // textHeightBehavior: 0 = All, 1 = DisableFirstAscent,
    // 2 = DisableLastDescent, 3 = DisableAll.
    if let Some(thb) = opt_float_for_key(cx, obj, "textHeightBehavior") {
        style.set_text_height_behavior(match thb as i32 {
            1 => TextHeightBehavior::DisableFirstAscent,
            2 => TextHeightBehavior::DisableLastDescent,
            3 => TextHeightBehavior::DisableAll,
            _ => TextHeightBehavior::All,
        });
    }

    // strutStyle: a fixed line box for deterministic leading. Presence of
    // the object enables the strut unless `enabled: false` is set.
    if let Ok(strut_val) = obj.get::<JsValue, _, _>(cx, "strutStyle")
        && let Ok(strut_obj) = strut_val.downcast::<JsObject, _>(cx)
    {
        let mut strut = StrutStyle::new();
        strut.set_strut_enabled(
            opt_bool_for_key(cx, &strut_obj, "enabled").unwrap_or(true),
        );
        if let Ok(fam_val) = strut_obj.get::<JsValue, _, _>(cx, "fontFamilies")
            && let Ok(fam_arr) = fam_val.downcast::<JsArray, _>(cx)
        {
            let fam_vec = fam_arr.to_vec(cx)?;
            let families = strings_in(cx, &fam_vec);
            strut.set_font_families(&families);
        }
        if let Some(size) = opt_float_for_key(cx, &strut_obj, "fontSize") {
            strut.set_font_size(size);
        }
        if let Some(h) = opt_float_for_key(cx, &strut_obj, "heightMultiplier") {
            strut.set_height(h);
            strut.set_height_override(true);
        }
        if let Some(l) = opt_float_for_key(cx, &strut_obj, "leading") {
            strut.set_leading(l);
        }
        strut.set_force_strut_height(
            opt_bool_for_key(cx, &strut_obj, "forceStrutHeight")
                .unwrap_or(false),
        );
        strut.set_half_leading(
            opt_bool_for_key(cx, &strut_obj, "halfLeading").unwrap_or(false),
        );
        style.set_strut_style(strut);
    }

    Ok(style)
}

/// Parses the `textStyle.fontVariations` array from a
/// `ParagraphStyleInput` JS object, mirroring `parse_font_variations` in
/// shape. Returned to
/// `paragraph::new` so the font collection used for typeface matching
/// can be the variable-instantiated one (otherwise the paragraph
/// engine renders at the typeface master regardless of explicit weight,
/// breaking parity with CanvasKit's behaviour on variable fonts).
fn parse_paragraph_font_variations(
    cx: &mut FunctionContext,
    obj: &Handle<JsObject>,
) -> NeonResult<Vec<(FourByteTag, f32)>> {
    let Ok(ts_val) = obj.get::<JsValue, _, _>(cx, "textStyle") else {
        return Ok(Vec::new());
    };
    let Ok(ts_obj) = ts_val.downcast::<JsObject, _>(cx) else {
        return Ok(Vec::new());
    };
    parse_font_variations(cx, &ts_obj)
}

//
// ParagraphBuilder FFI functions
//

pub fn new(mut cx: FunctionContext) -> JsResult<BoxedParagraphBuilder> {
    let style_arg = cx.argument::<JsObject>(1)?;
    let para_style = parse_paragraph_style(&mut cx, &style_arg)?;
    let variations = parse_paragraph_font_variations(&mut cx, &style_arg)?;

    // Route through `fonts_for_style` so any variable typefaces in the
    // collection get instantiated at the requested axis positions (or
    // auto-`wght` from the text style's nominal weight when no
    // explicit variations are passed). Without this the paragraph
    // engine matches the typeface's master instance regardless of
    // weight and the rendered glyphs drift off the requested wght
    // axis -- a visible parity gap on variable fonts like Dosis vs
    // CanvasKit's render.
    let text_style = para_style.text_style().clone();
    let collection = FontLibrary::with_shared(|lib| {
        lib.fonts_for_style(&text_style, &variations)
    });

    let builder = SkParagraphBuilder::new(&para_style, &collection);

    Ok(cx.boxed(RefCell::new(ParagraphBuilderWrap {
        builder: Some(builder),
        _collection: collection,
    })))
}

pub fn pushStyle(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let this = cx.argument::<BoxedParagraphBuilder>(0)?;
    let style_obj = cx.argument::<JsObject>(1)?;
    let text_style = parse_text_style(&mut cx, &style_obj)?;

    let mut this = this.borrow_mut();
    if let Some(builder) = this.builder.as_mut() {
        builder.push_style(&text_style);
    }
    Ok(cx.undefined())
}

pub fn pop(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let this = cx.argument::<BoxedParagraphBuilder>(0)?;
    let mut this = this.borrow_mut();
    if let Some(builder) = this.builder.as_mut() {
        builder.pop();
    }
    Ok(cx.undefined())
}

pub fn addText(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let this = cx.argument::<BoxedParagraphBuilder>(0)?;
    let text = string_arg(&mut cx, 1, "text")?;

    let mut this = this.borrow_mut();
    if let Some(builder) = this.builder.as_mut() {
        builder.add_text(&text);
    }
    Ok(cx.undefined())
}

pub fn addPlaceholder(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let this = cx.argument::<BoxedParagraphBuilder>(0)?;
    let width = float_arg_or_bail(&mut cx, 1, "width")?;
    let height = float_arg_or_bail(&mut cx, 2, "height")?;
    // Both were read and discarded, so every placeholder laid out on the
    // baseline whatever the caller asked for. The numbering is CanvasKit's.
    let alignment = match opt_float_arg(&mut cx, 3).unwrap_or(0.0) as i32 {
        0 => PlaceholderAlignment::Baseline,
        1 => PlaceholderAlignment::AboveBaseline,
        2 => PlaceholderAlignment::BelowBaseline,
        3 => PlaceholderAlignment::Top,
        4 => PlaceholderAlignment::Bottom,
        5 => PlaceholderAlignment::Middle,
        other => {
            return cx.throw_type_error(format!(
                "Unknown placeholder align {other} (expected 0 to 5)"
            ));
        }
    };

    let baseline = match opt_float_arg(&mut cx, 4).unwrap_or(0.0) as i32 {
        0 => TextBaseline::Alphabetic,
        1 => TextBaseline::Ideographic,
        other => {
            return cx.throw_type_error(format!(
                "Unknown placeholder baseline {other} (expected 0 or 1)"
            ));
        }
    };

    let offset = opt_float_arg(&mut cx, 5).unwrap_or(0.0);

    let placeholder = PlaceholderStyle {
        width,
        height,
        alignment,
        baseline,
        baseline_offset: offset,
    };

    let mut this = this.borrow_mut();
    if let Some(builder) = this.builder.as_mut() {
        builder.add_placeholder(&placeholder);
    }
    Ok(cx.undefined())
}

pub fn build(mut cx: FunctionContext) -> JsResult<BoxedParagraph> {
    let this = cx.argument::<BoxedParagraphBuilder>(0)?;
    let mut this = this.borrow_mut();

    match this.builder.take() {
        Some(mut builder) => {
            let paragraph = builder.build();
            Ok(cx.boxed(RefCell::new(ParagraphWrap { paragraph })))
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
    this.paragraph.layout(width);
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
    let h_style = opt_float_arg(&mut cx, 3).unwrap_or(0.0) as i32;
    let w_style = opt_float_arg(&mut cx, 4).unwrap_or(0.0) as i32;

    let rect_height_style = match h_style {
        1 => RectHeightStyle::Max,
        2 => RectHeightStyle::IncludeLineSpacingMiddle,
        3 => RectHeightStyle::IncludeLineSpacingTop,
        4 => RectHeightStyle::IncludeLineSpacingBottom,
        5 => RectHeightStyle::Strut,
        _ => RectHeightStyle::Tight,
    };
    let rect_width_style = match w_style {
        1 => RectWidthStyle::Max,
        _ => RectWidthStyle::Tight,
    };

    let this = this.borrow();
    let boxes = this.paragraph.get_rects_for_range(
        start..end,
        rect_height_style,
        rect_width_style,
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
        para.paragraph.paint(canvas, Point::new(x, y));
    });

    Ok(cx.undefined())
}
