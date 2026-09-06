#![allow(dead_code)]
#![allow(non_snake_case)]
use crate::{
    context::State,
    font_library::FontLibrary,
    text::{TextMetricsLine, TextMetricsRun},
    utils::*,
};
use neon::prelude::*;
use skia_safe::{
    Color, FontMetrics, Paint, Path as SkPath, PathBuilder as SkPathBuilder,
    Point, Rect, Typeface,
    font_style::{FontStyle, Slant, Weight, Width},
    textlayout::{
        Decoration, FontCollection, Paragraph, ParagraphBuilder,
        ParagraphStyle, RectHeightStyle, RectWidthStyle, TextAlign,
        TextDecoration, TextDecorationMode, TextDecorationStyle, TextDirection,
        TextStyle,
    },
};
use std::{fmt, iter::zip, ops::Range};

/// The next multiple of a hundred strictly above `weight`.
///
/// The step the `wght` axis is enumerated in. A hundred because that is
/// what CSS names a weight in -- 400 regular, 700 bold -- so the values a
/// caller is likely to ask for are the round ones plus whatever the axis
/// itself starts at.
fn next_multiple_of_100(weight: i32) -> i32 {
    const STEP: i32 = 100;
    weight + STEP - weight.rem_euclid(STEP)
}

//
// Text layout and metrics
//

const GALLEY: f32 = 100_000.0;

pub struct Typesetter {
    text: String,
    width: f32,
    baseline: Baseline,
    typefaces: FontCollection,
    /// The style the matched face reports, where a face was matched.
    ///
    /// Pinned onto the character style before a paragraph is built, so Skia
    /// does not synthesise a weight or a slant the face already has -- or
    /// does not have, and would then fake.
    matched_style: Option<FontStyle>,
    char_style: TextStyle,
    graf_style: ParagraphStyle,
    text_decoration: DecorationStyle,
    text_wrap: bool,
}

/// Characters Skia's paragraph treats as a hard line break.
///
/// With wrapping off the paragraph is built with `set_max_lines(Some(1))`, so
/// anything after the first break is discarded rather than drawn -- silently,
/// and from `measureText` as well as from the canvas. `"A\u{c}B C D"` painted
/// 236 pixels reaching x=24, byte for byte what `"A"` alone paints, against
/// 1051 reaching x=116 for the same string spaced.
///
/// The Canvas text preparation algorithm says to "replace all ASCII
/// whitespace in text with U+0020 SPACE characters", which covers TAB, LF, FF
/// and CR. TAB and CR already measure as a space here and are replaced anyway,
/// so this states the specification's rule rather than the subset that
/// happened to be broken.
///
/// `U+000B`, `U+2028` and `U+2029` are **not** ASCII whitespace and the
/// specification does not reach them. They are replaced because the choice is
/// not between a space and something else -- it is between a space and
/// discarding the rest of the string, which nothing licenses. Chrome renders
/// the vertical tab as a space; that is corroboration rather than the reason.
///
/// Wrapping mode is untouched and was never affected: every one of these
/// breaks a line there and nothing is lost.
const HARD_BREAKS: [char; 7] = [
    '\u{9}',    // TAB
    '\u{a}',    // LF
    '\u{b}',    // VT
    '\u{c}',    // FF
    '\u{d}',    // CR
    '\u{2028}', // LINE SEPARATOR
    '\u{2029}', // PARAGRAPH SEPARATOR
];

/// Replaces every hard break in `text` with a space.
///
/// One pass, and it borrows rather than allocating where there is nothing to
/// replace -- the overwhelmingly common case for a single-line draw.
fn normalize_to_one_line(text: &str) -> String {
    if text.contains(HARD_BREAKS) {
        text.replace(HARD_BREAKS, " ")
    } else {
        text.to_string()
    }
}

impl Typesetter {
    pub fn new(state: &State, text: &str, width: Option<f32>) -> Self {
        let (char_style, graf_style, text_decoration, baseline, text_wrap) =
            state.typography();
        let variations = &state.variations;
        // The matched style comes back with the collection, from the one
        // search that found it. `layout` used to search again for it, on
        // every call, and it is the same search.
        let (typefaces, matched_style) = FontLibrary::with_shared(|lib| {
            lib.set_hinting(graf_style.hinting_is_on())
                .fonts_for_style(&char_style, variations)
        });
        let width = width.unwrap_or(GALLEY);
        let text = match text_wrap {
            true => text.to_string(),
            false => normalize_to_one_line(text),
        };

        Typesetter {
            text,
            width,
            baseline,
            typefaces,
            matched_style,
            char_style,
            graf_style,
            text_decoration,
            text_wrap,
        }
    }

    pub fn layout(&self, paint: &Paint) -> (Paragraph, Point) {
        let mut char_style = self.char_style.clone();
        char_style.set_foreground_paint(paint);
        char_style.set_decoration(
            &self.text_decoration.for_layout(&char_style, paint.color()),
        );

        // Prevent SkParagraph from faking the font style where the match is
        // not the requested weight or slant. Found once, when the collection
        // was chosen, rather than by searching it again here: the search
        // needed a `Vec` of the family names, a clone of the collection and a
        // lookup, 173 nanoseconds of every layout on this machine, to arrive
        // at what `fonts_for_style` had already had in hand.
        if let Some(matched) = self.matched_style {
            char_style.set_font_style(matched);
        }

        let mut paragraph_builder =
            ParagraphBuilder::new(&self.graf_style, &self.typefaces);
        paragraph_builder.push_style(&char_style);
        paragraph_builder.add_text(&self.text);

        let mut paragraph = paragraph_builder.build();
        paragraph.layout(self.width);

        let offset = Point::new(
            self.alignment_offset(),
            -paragraph.alphabetic_baseline(),
        );

        (paragraph, offset)
    }

    /// Measurements of the run, as a struct rather than as JSON.
    ///
    /// `metrics` below serializes for the Node binding and is what the JS
    /// `measureText` returns; this is a sibling for the Rust API rather than
    /// a refactor of it, so that output stays byte-for-byte identical. The
    /// two share the baseline math deliberately: they are measuring the same
    /// thing and must not drift.
    pub fn extents(&self) -> TextExtents {
        let (mut paragraph, origin) = self.layout(&Paint::default());

        // Baseline offsets, relative to whatever `textBaseline` selected.
        let shift = self.char_style.baseline_shift();
        let hang = Baseline::Hanging.get_offset(&self.char_style) - shift;
        let norm = Baseline::Alphabetic.get_offset(&self.char_style) - shift;
        let ideo = Baseline::Ideographic.get_offset(&self.char_style) - shift;

        // Per-line glyph bounds, as `metrics` gathers them -- with the family
        // and font metrics alongside, which is what makes the per-run detail
        // below reportable rather than a second walk.
        //
        // Sized for one run a line, which is what a measurement of one font
        // has, so the common case allocates once and never grows.
        let mut run_bounds: Vec<RunBound> =
            Vec::with_capacity(paragraph.line_number());
        paragraph.extended_visit(|line, visit| {
            if let Some(info) = visit {
                run_bounds.push(RunBound {
                    line,
                    bounds: zip(info.positions(), info.bounds())
                        .filter(|(_, rect)| !rect.is_empty())
                        .map(|(pt, rect)| {
                            rect.with_offset(
                                *pt + info.origin() + origin
                                    - Point::new(0.0, norm),
                            )
                        })
                        .reduce(Rect::join2)
                        .unwrap_or(Rect::new_empty()),
                    family: info.font().typeface().family_name(),
                    metrics: info.font().metrics().1,
                });
            }
        });

        // Each line's runs are the stretch of the list carrying its number,
        // which holds because `extended_visit` reports them in line order.
        // Checked rather than assumed -- the check is a scan and the sort it
        // guards is never expected to run, but a list that arrived out of
        // order would otherwise put a run on the wrong line and report it
        // there. The sort is stable, because runs within a line are in
        // visual order and that is what `runs` reports.
        if !run_bounds.is_sorted_by_key(|run| run.line) {
            run_bounds.sort_by_key(|run| run.line);
        }

        // The laid-out box of each line, which is what the Canvas API
        // measures: glyph ink for the vertical extent, but the layout rect
        // horizontally, so trailing whitespace counts.
        //
        // The letter-space Skia puts after the last glyph is kept. CSS adds
        // `letter-spacing` after every character including the final one, so
        // an `n`-character run is `n` spaces wide, and Chrome measures it
        // that way. This used to subtract a whole space from `right`, which
        // gave `n - 1`: at 40px Helvetica with 10px spacing, `"a"` measured
        // 22.25 -- identical to no spacing at all -- where Chrome gives
        // 32.25, and `"abcd"` measured 116.74 against 126.74.
        //
        // Nothing drawn moves. The subtraction only ever reached the
        // reported box: measured across left, centre and right alignment at
        // 0px and 10px, every inked column is where it was, and the
        // `letterSpacing` test's three ink assertions -- no indent, a gap
        // between the glyphs, no outdent -- pass unchanged. What moves is
        // the advance the measurement reports, by exactly one space.
        //
        // Joined as the lines are walked rather than collected and joined
        // afterwards: the collection existed only to be reduced.
        let mut full_bounds: Option<Rect> = None;
        let mut line_details: Vec<TextMetricsLine> =
            Vec::with_capacity(paragraph.line_number());

        // Grouped once rather than filtered per line. The closure this
        // replaces scanned every run in the paragraph and was called twice
        // inside the loop -- once for the line's bounds and once for its runs
        // -- so the work was `2 x lines x runs` and a wrapped paragraph pays
        // it in both factors. Measured before this: doubling the lines
        // multiplied the time by about 3.9 each step, 930 microseconds at 30
        // lines and 211 milliseconds at 480.
        let utf16 = Utf16Index::new(&self.text);
        let mut taken = 0;

        for line in 0..paragraph.line_number() {
            // The run list is in line order, so this line's runs are the
            // stretch starting where the last line's ended. A slice rather
            // than a list of indices: the indices were a `Vec` per line, and
            // a wrapped paragraph allocated one for every one of them.
            let start = taken;
            while run_bounds.get(taken).is_some_and(|run| run.line == line) {
                taken += 1;
            }
            let on_line = &mut run_bounds[start..taken];

            let text_bounds = on_line
                .iter()
                .map(|run| run.bounds)
                .reduce(Rect::join2)
                .unwrap_or(Rect::new_empty());

            let text_range =
                paragraph.get_actual_text_range(line, !self.text_wrap);
            let char_range = utf16.range(&text_range);

            // The same arithmetic `metrics` does for the JSON it hands the
            // binding, so the two surfaces report one measurement rather
            // than two derivations of it.
            if let Some(line_metrics) = paragraph.get_line_metrics_at(line) {
                let half_leading =
                    self.graf_style.strut_style().leading().max(0.0)
                        * self.char_style.font_size()
                        / 2.0;
                let baseline =
                    line_metrics.baseline as f32 + origin.y - half_leading;
                line_details.push(TextMetricsLine {
                    x: text_bounds.left,
                    y: text_bounds.top,
                    width: text_bounds.width(),
                    height: text_bounds.height(),
                    baseline,
                    hanging_baseline: baseline - hang,
                    alphabetic_baseline: baseline - norm,
                    ideographic_baseline: baseline - ideo,
                    ascent: baseline - line_metrics.ascent as f32,
                    descent: baseline + line_metrics.descent as f32,
                    start_index: char_range.start,
                    end_index: char_range.end,
                    // The family name is moved out rather than copied: it
                    // was built by the walk above and this is the only place
                    // that reads it, so the clone was a second allocation
                    // per run for the same string.
                    runs: on_line
                        .iter_mut()
                        .map(|run| TextMetricsRun {
                            x: run.bounds.left,
                            y: run.bounds.top,
                            width: run.bounds.width(),
                            height: run.bounds.height(),
                            family: std::mem::take(&mut run.family),
                            ascent: baseline - norm + run.metrics.ascent,
                            descent: baseline - norm + run.metrics.descent,
                            cap_height: baseline
                                - norm
                                - run.metrics.cap_height,
                            x_height: baseline - norm - run.metrics.x_height,
                            underline: run
                                .metrics
                                .underline_position()
                                .map(|at| baseline - norm + at),
                            strikethrough: run
                                .metrics
                                .strikeout_position()
                                .map(|at| baseline - norm + at),
                        })
                        .collect(),
                });
            }

            let line_rect = paragraph
                .get_rects_for_range(
                    char_range,
                    RectHeightStyle::Tight,
                    RectWidthStyle::Tight,
                )
                .iter()
                .map(|tb| {
                    let Rect { top, bottom, .. } = text_bounds;
                    let Rect { left, right, .. } = tb.rect.with_offset(origin);
                    Rect::new(left, top, right, bottom)
                })
                .reduce(Rect::join2)
                .unwrap_or(text_bounds);
            full_bounds = Some(match full_bounds {
                Some(so_far) => Rect::join2(so_far, line_rect),
                None => line_rect,
            });
        }
        let full_bounds = full_bounds.unwrap_or(Rect::new_empty());

        // Font extents describe what the face can reach for any string, so
        // they come from the first line's metrics rather than these glyphs --
        // and they are relative to the selected baseline, which is why `norm`
        // appears. An empty run has no line metrics and falls back to the
        // style's own. Skia's ascent is negative above the baseline.
        let (font_ascent, font_descent) = paragraph
            .get_line_metrics_at(0)
            .map(|line| (norm + line.ascent as f32, line.descent as f32 - norm))
            .unwrap_or_else(|| {
                let FontMetrics {
                    ascent, descent, ..
                } = self.char_style.font_metrics();
                (norm - ascent, descent - norm)
            });

        TextExtents {
            // The laid-out width, not `max_intrinsic_width`: that is what the
            // run would take unwrapped, so it contradicts both the ink bounds
            // beside it and the pixels actually drawn.
            width: full_bounds.width(),
            ink: full_bounds,
            line_details,
            font_ascent,
            font_descent,
            alphabetic: norm,
            hanging: hang,
            ideographic: ideo,
            height: paragraph.height(),
            lines: paragraph.line_number(),
        }
    }

    pub fn path(&mut self, point: impl Into<Point>) -> SkPath {
        let (mut paragraph, mut origin) = self.layout(&Paint::default());
        let headroom = self.char_style.font_metrics().ascent
            + paragraph.alphabetic_baseline();
        let offset = self.baseline.get_offset(&self.char_style);
        origin += point.into();
        origin.y -= headroom - offset;

        let mut builder = SkPathBuilder::new();
        for idx in 0..paragraph.line_number() {
            let (_skipped, line) = paragraph.get_path_at(idx);
            let translated = line.with_offset(origin);
            builder.add_path(&translated, None);
        }
        builder.detach()
    }

    fn alignment_offset(&self) -> f32 {
        // convert start/end to left/right depending on writing system
        let gravity = match (
            self.graf_style.text_direction(),
            self.graf_style.text_align(),
        ) {
            (TextDirection::LTR, TextAlign::Start)
            | (TextDirection::RTL, TextAlign::End) => TextAlign::Left,
            (TextDirection::LTR, TextAlign::End)
            | (TextDirection::RTL, TextAlign::Start) => TextAlign::Right,
            (_, alignment) => alignment,
        };

        // `alignment_factor` shifts the entire line to left/right/center align
        // it `spacing_step` compensates for the letterspacing Paragraph
        // adds before the line's first character
        let (alignment_factor, spacing_step) = match gravity {
            TextAlign::Left | TextAlign::Justify => (0.0, -0.5),
            TextAlign::Center => (-0.5, 0.5),
            TextAlign::Right => (-1.0, 1.0),
            _ => (0.0, 0.0), // start & end have already been remapped
        };

        alignment_factor * self.width
            + spacing_step * self.char_style.letter_spacing()
    }
}

//
// Convert utf-8 byte indices -> utf-16 codepoint indices
//
/// One single-font stretch of a laid-out line, as the glyph walk found it.
///
/// A named struct rather than the four-field tuple this was, because three
/// of the four are read in one place and the fourth -- the line it belongs to
/// -- decides the grouping, which is easier to be sure of when it has a name.
struct RunBound {
    /// Which line of the paragraph it sits on.
    line: usize,
    /// The union of its glyphs' inked bounds.
    bounds: Rect,
    /// The family the typeface reports, moved out when the run is reported.
    family: String,
    /// The metrics of the font it was drawn in.
    metrics: FontMetrics,
}

/// A byte offset to UTF-16 offset map for one string.
///
/// JavaScript counts string positions in UTF-16 code units and Skia reports
/// them in bytes, so every line's start and end has to be converted. Doing
/// that from the string each time is what made measuring a wrapped paragraph
/// quadratic: the conversion walked the whole text to build its table, and
/// then summed the code units from the beginning to reach the line -- both
/// per line, both O(N), with N growing alongside the line count.
///
/// Built once per measurement instead. `cumulative[i]` is the number of
/// UTF-16 units before char `i`, so a range is two lookups and a subtraction,
/// and `offsets` is ascending so an endpoint is a binary search.
enum Utf16Index {
    /// Text whose byte offsets are already its UTF-16 offsets.
    ///
    /// Every ASCII character is one byte and one UTF-16 unit, so the two
    /// tables below would be `0, 1, 2, ...` and the lookup an identity. The
    /// check is a vectorised scan of the string; building the tables is two
    /// allocations and a pass that appends to both.
    Ascii { len: usize },
    /// Anything else, indexed.
    Mapped {
        /// Byte offset of each char, ascending.
        offsets: Vec<usize>,
        /// UTF-16 units before each char; one longer than `offsets`.
        cumulative: Vec<usize>,
    },
}

impl Utf16Index {
    fn new(text: &str) -> Self {
        if text.is_ascii() {
            return Utf16Index::Ascii { len: text.len() };
        }

        let mut offsets = Vec::with_capacity(text.len());
        let mut cumulative = Vec::with_capacity(text.len() + 1);
        let mut units = 0;
        for (at, ch) in text.char_indices() {
            offsets.push(at);
            cumulative.push(units);
            units += ch.len_utf16();
        }
        cumulative.push(units);
        Utf16Index::Mapped {
            offsets,
            cumulative,
        }
    }

    /// The UTF-16 range covering `byte_range`.
    ///
    /// The two fallbacks are the previous implementation's and are kept
    /// deliberately: a range starting past the last character counted from
    /// zero, and one ending before the first reported its own start twice.
    /// Neither is reachable from a laid-out line, and changing them would be
    /// a behaviour change smuggled in beside a performance one.
    fn range(&self, byte_range: &Range<usize>) -> Range<usize> {
        // The two arms are one calculation over two representations of the
        // same tables. For ASCII the tables are `0, 1, 2, ...`, so a
        // `partition_point` over them is `min`, and reading one back is the
        // index itself -- written out rather than built, which is the whole
        // of the fast path.
        let (count, first_at_or_after, at_or_after_end) = match self {
            Utf16Index::Ascii { len } => {
                (*len, byte_range.start.min(*len), byte_range.end.min(*len))
            }
            Utf16Index::Mapped { offsets, .. } => (
                offsets.len(),
                offsets.partition_point(|at| *at < byte_range.start),
                offsets.partition_point(|at| *at < byte_range.end),
            ),
        };

        let start = match first_at_or_after < count {
            true => first_at_or_after,
            false => 0,
        };
        let end = match at_or_after_end {
            0 => start,
            past => past,
        };

        let units = |at: usize| match self {
            Utf16Index::Ascii { .. } => at,
            Utf16Index::Mapped { cumulative, .. } => cumulative[at],
        };
        let head = units(start);
        let tail = match end > start {
            true => units(end) - units(start),
            false => head,
        };
        head..head + tail
    }
}

//
// Font argument packing & unpacking
//
#[derive(Debug, Clone)]
pub struct FontSpec {
    pub families: Vec<String>,
    pub size: f32,
    pub line_height: Option<f32>,
    pub weight: Weight,
    pub width: Width,
    pub slant: Slant,
    pub features: Vec<(String, i32)>,
    pub variant: String,
    pub canonical: String,
}

impl FontSpec {
    pub fn with_width(&self, width: Width) -> Self {
        Self {
            width,
            ..self.clone()
        }
    }

    pub fn style(&self) -> FontStyle {
        FontStyle::new(self.weight, self.width, self.slant)
    }
}

pub fn font_arg(
    cx: &mut FunctionContext,
    idx: usize,
) -> NeonResult<Option<FontSpec>> {
    let arg = cx.argument::<JsValue>(idx)?;
    if arg.is_a::<JsNull, _>(cx) {
        return Ok(None);
    }

    let font_desc = cx.argument::<JsObject>(idx)?;
    let families = strings_at_key(cx, &font_desc, "family")?;
    let canonical = string_for_key(cx, &font_desc, "canonical")?;
    let variant = string_for_key(cx, &font_desc, "variant")?;
    let size = float_for_key(cx, &font_desc, "size")?;
    let weight = Weight::from(float_for_key(cx, &font_desc, "weight")? as i32);
    let slant = to_slant(string_for_key(cx, &font_desc, "style")?.as_str());
    let width = to_width(string_for_key(cx, &font_desc, "stretch")?.as_str());
    let line_height = opt_float_for_key(cx, &font_desc, "lineHeight")
        .map(|pt_size| pt_size / size);

    let feat_obj: Handle<JsObject> = font_desc.get(cx, "features")?;
    let features = font_features(cx, &feat_obj)?;

    Ok(match families[0].is_empty() {
        true => None, /* silently fail if a family name was omitted (e.g., */
        // "bold 50px")
        false => Some(FontSpec {
            families,
            size,
            line_height,
            weight,
            slant,
            width,
            features,
            variant,
            canonical,
        }),
    })
}

pub fn font_features(
    cx: &mut FunctionContext,
    obj: &Handle<JsObject>,
) -> NeonResult<Vec<(String, i32)>> {
    let keys = obj.get_own_property_names(cx)?.to_vec(cx)?;
    let mut features: Vec<(String, i32)> = vec![];
    for key in strings_in(cx, &keys).iter() {
        match key.as_str() {
            "on" | "off" => {
                strings_at_key(cx, obj, key)?.iter().for_each(|feat| {
                    features.push((
                        feat.to_string(),
                        if key == "on" { 1 } else { 0 },
                    ));
                })
            }
            _ => features
                .push((key.to_string(), float_for_key(cx, obj, key)? as i32)),
        }
    }
    Ok(features)
}

pub fn typeface_details<'a>(
    cx: &mut FunctionContext<'a>,
    filename: &str,
    font: &Typeface,
    alias: Option<String>,
) -> JsResult<'a, JsObject> {
    let style = font.font_style();

    let filename = cx.string(filename);
    let family = cx.string(match alias {
        Some(name) => name,
        None => font.family_name(),
    });
    let weight = cx.number(*style.weight() as f64);
    let slant = cx.string(from_slant(style.slant()));
    let width = cx.string(from_width(style.width()));

    let dict = JsObject::new(cx);
    let attr = cx.string("family");
    dict.set(cx, attr, family)?;
    let attr = cx.string("weight");
    dict.set(cx, attr, weight)?;
    let attr = cx.string("style");
    dict.set(cx, attr, slant)?;
    let attr = cx.string("width");
    dict.set(cx, attr, width)?;
    let attr = cx.string("file");
    dict.set(cx, attr, filename)?;
    Ok(dict)
}

pub fn typeface_wght_range(font: &Typeface) -> Vec<i32> {
    let mut wghts = vec![];
    if let Some(params) = font.variation_design_parameters() {
        for param in params {
            let chars = vec![
                param.tag.a(),
                param.tag.b(),
                param.tag.c(),
                param.tag.d(),
            ];
            let tag = String::from_utf8_lossy(&chars).into_owned();
            let (min, max) = (param.min as i32, param.max as i32);
            if tag == "wght" {
                // The weights a caller is likely to name: the axis minimum,
                // then every round hundred up to its maximum. `val + 100 -
                // val % 100` is "the next multiple of a hundred", which
                // steps by a full hundred from a round value and by less
                // from the axis minimum, so a font whose range starts at 350
                // reports 350, 400, 500 rather than 350, 450, 550.
                let mut val = min;
                while val <= max {
                    wghts.push(val);
                    val = next_multiple_of_100(val);
                }
                if !wghts.contains(&max) {
                    wghts.push(max);
                }
            }
        }
    }
    wghts
}

pub fn to_slant(slant_name: &str) -> Slant {
    match slant_name.to_lowercase().as_str() {
        "italic" => Slant::Italic,
        "oblique" => Slant::Oblique,
        _ => Slant::Upright,
    }
}

pub fn from_slant(slant: Slant) -> String {
    match slant {
        Slant::Upright => "normal",
        Slant::Italic => "italic",
        Slant::Oblique => "oblique",
    }
    .to_string()
}

pub fn to_width(width_name: &str) -> Width {
    match width_name.to_lowercase().as_str() {
        "ultra-condensed" => Width::ULTRA_CONDENSED,
        "extra-condensed" => Width::EXTRA_CONDENSED,
        "condensed" => Width::CONDENSED,
        "semi-condensed" => Width::SEMI_CONDENSED,
        "semi-expanded" => Width::SEMI_EXPANDED,
        "expanded" => Width::EXPANDED,
        "extra-expanded" => Width::EXTRA_EXPANDED,
        "ultra-expanded" => Width::ULTRA_EXPANDED,
        _ => Width::NORMAL,
    }
}

pub fn from_width(width: Width) -> String {
    match width {
        w if w == Width::ULTRA_CONDENSED => "ultra-condensed",
        w if w == Width::EXTRA_CONDENSED => "extra-condensed",
        w if w == Width::CONDENSED => "condensed",
        w if w == Width::SEMI_CONDENSED => "semi-condensed",
        w if w == Width::SEMI_EXPANDED => "semi-expanded",
        w if w == Width::EXPANDED => "expanded",
        w if w == Width::EXTRA_EXPANDED => "extra-expanded",
        w if w == Width::ULTRA_EXPANDED => "ultra-expanded",
        _ => "normal",
    }
    .to_string()
}

pub fn to_text_align(mode_name: &str) -> Option<TextAlign> {
    let mode = match mode_name.to_lowercase().as_str() {
        "left" => TextAlign::Left,
        "right" => TextAlign::Right,
        "center" => TextAlign::Center,
        "justify" => TextAlign::Justify,
        "start" => TextAlign::Start,
        "end" => TextAlign::End,
        _ => return None,
    };
    Some(mode)
}

pub fn from_text_align(mode: TextAlign) -> String {
    match mode {
        TextAlign::Left => "left",
        TextAlign::Right => "right",
        TextAlign::Center => "center",
        TextAlign::Justify => "justify",
        TextAlign::Start => "start",
        TextAlign::End => "end",
    }
    .to_string()
}

#[derive(Copy, Clone, Debug)]
pub enum Baseline {
    Top,
    Hanging,
    Middle,
    Alphabetic,
    Ideographic,
    Bottom,
}

pub fn to_text_baseline(mode_name: &str) -> Option<Baseline> {
    let mode = match mode_name.to_lowercase().as_str() {
        "top" => Baseline::Top,
        "hanging" => Baseline::Hanging,
        "middle" => Baseline::Middle,
        "alphabetic" => Baseline::Alphabetic,
        "ideographic" => Baseline::Ideographic,
        "bottom" => Baseline::Bottom,
        _ => return None,
    };
    Some(mode)
}

pub fn from_text_baseline(mode: Baseline) -> String {
    match mode {
        Baseline::Top => "top",
        Baseline::Hanging => "hanging",
        Baseline::Middle => "middle",
        Baseline::Alphabetic => "alphabetic",
        Baseline::Ideographic => "ideographic",
        Baseline::Bottom => "bottom",
    }
    .to_string()
}

impl Baseline {
    pub fn get_offset(&self, style: &TextStyle) -> f32 {
        let FontMetrics {
            mut ascent,
            mut descent,
            ..
        } = style.font_metrics();
        ascent -= style.baseline_shift(); // offsets are defined relative to the alphabetic baseline, so
        descent -= style.baseline_shift(); // compensate for any other textBaseline setting

        // see TextMetrics::GetFontBaseline from Chromium for reference:
        // https://github.com/chromium/chromium/blob/main/third_party/blink/renderer/core/html/canvas/text_metrics.cc#L34
        match self {
            Baseline::Top => -ascent,
            Baseline::Hanging => -ascent * 0.8,
            Baseline::Middle => -(ascent + descent) / 2.0,
            Baseline::Alphabetic => 0.0,
            Baseline::Bottom | Baseline::Ideographic => -descent,
        }
    }
}

#[derive(Clone, Debug)]
pub struct DecorationStyle {
    pub css: String,
    pub decoration: Decoration,
    pub size: Option<Spacing>,
    pub color: Option<Color>,
}

impl Default for DecorationStyle {
    fn default() -> Self {
        Self {
            decoration: Decoration::default(),
            size: None,
            color: None,
            css: "none".to_string(),
        }
    }
}

impl DecorationStyle {
    pub fn for_layout(
        &self,
        style: &TextStyle,
        text_color: Color,
    ) -> Decoration {
        // convert `size` into a multiple of the current font's default
        // thickness
        let em_size = style.font_size();
        let thickness =
            style.font_metrics().underline_thickness().unwrap_or(1.0);
        let thickness_multiplier = self
            .size
            .clone()
            .map(|size| size.in_px(em_size) / thickness)
            .unwrap_or(1.0);
        let color = self.color.unwrap_or(text_color);
        Decoration {
            thickness_multiplier,
            color,
            ..self.decoration
        }
    }
}

pub fn decoration_arg(
    cx: &mut FunctionContext,
    idx: usize,
) -> NeonResult<Option<DecorationStyle>> {
    if let Some(deco) = opt_object_arg(cx, idx) {
        let css = string_for_key(cx, &deco, "str")?;

        let line = string_for_key(cx, &deco, "line")?;
        let ty = match line.as_str() {
            "underline" => TextDecoration::UNDERLINE,
            "overline" => TextDecoration::OVERLINE,
            "line-through" => TextDecoration::LINE_THROUGH,
            _ => return Ok(Some(DecorationStyle::default())),
        };

        let line_style = string_for_key(cx, &deco, "style")?;
        let style = match line_style.as_str() {
            "wavy" => TextDecorationStyle::Wavy,
            "dotted" => TextDecorationStyle::Dotted,
            "dashed" => TextDecorationStyle::Dashed,
            "double" => TextDecorationStyle::Double,
            _ => TextDecorationStyle::Solid,
        };

        // `currentColor` and an unparseable color are different things, and
        // conflating them broke the feature for every form that did not name
        // an explicit color: `underline`, `underline wavy` and
        // `line-through` were all discarded in silence.
        //
        // `None` here is the valid "inherit the fill color" case, which
        // `DecorationStyle::for_layout` implements by substituting the text
        // color at layout time. Only a color string Skia cannot parse
        // invalidates the declaration.
        let color = match string_for_key(cx, &deco, "color")?.as_str() {
            "currentColor" => None,
            color_str => match css_to_color(color_str) {
                Some(color) => Some(color),
                None => return Ok(None),
            },
        };

        let inherit = string_for_key(cx, &deco, "inherit")?;
        let size = match inherit.as_str() {
            "from-font" => None,
            _ => match opt_object_for_key(cx, &deco, "thickness") {
                Some(thickness) => Spacing::from_obj(cx, &thickness)?,
                _ => None,
            },
        };

        // an empty declaration is still ignored
        if css.is_empty() {
            return Ok(None);
        }

        // As of skia_safe 0.78.2, `Gaps` mode is too buggy, with random breaks
        // in places that don't have descenders. It would be nice to
        // enable this in a future release once it stabilizes…
        let mode = TextDecorationMode::Through;

        let decoration = Decoration {
            ty,
            style,
            mode,
            ..Decoration::default()
        };
        Ok(Some(DecorationStyle {
            decoration,
            size,
            color,
            css,
        }))
    } else {
        Ok(None)
    }
}

//
// Em-relative lengths (for text spacing & decoration thickness)
//
#[derive(Clone, Debug)]
pub struct Spacing {
    raw_size: f32,
    unit: String,
    px_size: f32,
}

impl Default for Spacing {
    fn default() -> Self {
        Self {
            raw_size: 0.0,
            unit: "px".to_string(),
            px_size: 0.0,
        }
    }
}

impl Spacing {
    pub fn from_obj(
        cx: &mut FunctionContext,
        spacing: &Handle<JsObject>,
    ) -> NeonResult<Option<Self>> {
        let raw_size = float_for_key(cx, spacing, "size")?;
        let unit = string_for_key(cx, spacing, "unit")?;
        let px_size = float_for_key(cx, spacing, "px")?;
        Ok(Self::parse(raw_size, unit, px_size))
    }

    pub fn parse(raw_size: f32, unit: String, px_size: f32) -> Option<Self> {
        let main_size = match unit.as_str() {
            "em" | "rem" => raw_size,
            _ => px_size,
        };

        match main_size.is_nan() {
            false => Some(Self {
                raw_size,
                unit,
                px_size,
            }),
            true => None,
        }
    }

    pub fn in_px(&self, em_size: f32) -> f32 {
        match self.unit.as_str() {
            "em" | "rem" => self.raw_size * em_size,
            _ => self.px_size,
        }
    }
}

impl fmt::Display for Spacing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", self.raw_size, self.unit)
    }
}

pub fn opt_spacing_arg<'a>(
    cx: &mut FunctionContext<'a>,
    idx: usize,
) -> NeonResult<Option<Spacing>> {
    match cx.argument::<JsValue>(idx)?.is_a::<JsNull, _>(cx) {
        true => Ok(None),
        false => {
            let spacing = cx.argument::<JsObject>(idx)?;
            Spacing::from_obj(cx, &spacing)
        }
    }
}

/// Structured measurements of a laid-out run.
///
/// Feeds the Rust API's `TextMetrics`. Distances are pixels relative to the
/// baseline the current `textBaseline` selected.
pub struct TextExtents {
    /// Advance width of the run.
    pub width: f32,
    /// Union of the per-glyph inked bounds, baseline-relative.
    pub ink: Rect,
    /// Distance the face can reach above the baseline, positive.
    pub font_ascent: f32,
    /// Distance the face can reach below the baseline, positive.
    pub font_descent: f32,
    /// Offset of the alphabetic baseline from the selected one.
    pub alphabetic: f32,
    /// Offset of the hanging baseline from the selected one.
    pub hanging: f32,
    /// Offset of the ideographic baseline from the selected one.
    pub ideographic: f32,
    /// Height of the laid-out run.
    pub height: f32,
    /// Number of lines the run occupied.
    pub lines: usize,
    /// Each line, with the single-font runs inside it.
    pub line_details: Vec<TextMetricsLine>,
}

/// Where one reported number comes from.
///
/// `Family` is the odd one: a run reports the typeface it resolved to, which
/// is a string and cannot travel in a buffer of numbers, so it goes into an
/// array beside it and keeps its place in the published order.
enum Source<T> {
    /// A number the measurement always has.
    Number(fn(&T) -> f64),
    /// A number the font may not report. `NaN` in the buffer, `null` in
    /// JavaScript, which is what these have always been.
    Optional(fn(&T) -> f64),
    /// A string, taken in order from the array travelling beside the numbers.
    Family(fn(&T) -> &str),
}

/// One field of what `measureText` reports.
///
/// The tables below are read twice: to fill the buffer that crosses, and to
/// say what the buffer holds. The reader on the JavaScript side is built from
/// the published names in the published order rather than repeating them, so
/// the two cannot disagree about which slot is `width` -- and the failure
/// that would produce is one measurement reported under another's name, which
/// nothing would raise.
struct Field<T> {
    /// The name JavaScript knows it by.
    name: &'static str,
    /// Where its value comes from.
    from: Source<T>,
}

impl<T> Field<T> {
    /// A number that is always reported.
    const fn plain(name: &'static str, read: fn(&T) -> f64) -> Self {
        Self {
            name,
            from: Source::Number(read),
        }
    }

    /// A number the font may leave unsaid.
    const fn optional(name: &'static str, read: fn(&T) -> f64) -> Self {
        Self {
            name,
            from: Source::Optional(read),
        }
    }

    /// The resolved family name.
    const fn family(name: &'static str, read: fn(&T) -> &str) -> Self {
        Self {
            name,
            from: Source::Family(read),
        }
    }
}

/// The `TextMetrics` of the Canvas specification.
///
/// `0.0 - x` rather than `-x`: negating a zero produces a negative zero,
/// which a browser never reports here and which JavaScript can see through
/// `Object.is`. Subtracting from zero gives `+0.0` for a zero input and is
/// otherwise the same negation.
const METRIC_FIELDS: &[Field<TextExtents>] = &[
    Field::plain("width", |m| m.width as f64),
    Field::plain("actualBoundingBoxLeft", |m| (0.0 - m.ink.left) as f64),
    Field::plain("actualBoundingBoxRight", |m| m.ink.right as f64),
    Field::plain("actualBoundingBoxAscent", |m| (0.0 - m.ink.top) as f64),
    Field::plain("actualBoundingBoxDescent", |m| m.ink.bottom as f64),
    Field::plain("fontBoundingBoxAscent", |m| m.font_ascent as f64),
    Field::plain("fontBoundingBoxDescent", |m| m.font_descent as f64),
    Field::plain("emHeightAscent", |m| m.font_ascent as f64),
    Field::plain("emHeightDescent", |m| m.font_descent as f64),
    Field::plain("hangingBaseline", |m| m.hanging as f64),
    Field::plain("alphabeticBaseline", |m| m.alphabetic as f64),
    Field::plain("ideographicBaseline", |m| m.ideographic as f64),
];

/// One line of a laid-out run, as `lines` reports it.
const LINE_FIELDS: &[Field<TextMetricsLine>] = &[
    Field::plain("x", |l| l.x as f64),
    Field::plain("y", |l| l.y as f64),
    Field::plain("width", |l| l.width as f64),
    Field::plain("height", |l| l.height as f64),
    Field::plain("baseline", |l| l.baseline as f64),
    Field::plain("hangingBaseline", |l| l.hanging_baseline as f64),
    Field::plain("alphabeticBaseline", |l| l.alphabetic_baseline as f64),
    Field::plain("ideographicBaseline", |l| l.ideographic_baseline as f64),
    Field::plain("ascent", |l| l.ascent as f64),
    Field::plain("descent", |l| l.descent as f64),
    Field::plain("startIndex", |l| l.start_index as f64),
    Field::plain("endIndex", |l| l.end_index as f64),
];

/// One single-font stretch of a line, as `runs` reports it.
const RUN_FIELDS: &[Field<TextMetricsRun>] = &[
    Field::plain("x", |r| r.x as f64),
    Field::plain("y", |r| r.y as f64),
    Field::plain("width", |r| r.width as f64),
    Field::plain("height", |r| r.height as f64),
    Field::family("family", |r| r.family.as_str()),
    Field::plain("ascent", |r| r.ascent as f64),
    Field::plain("descent", |r| r.descent as f64),
    Field::plain("capHeight", |r| r.cap_height as f64),
    Field::plain("xHeight", |r| r.x_height as f64),
    Field::optional("underline", |r| unsaid(r.underline)),
    Field::optional("strikethrough", |r| unsaid(r.strikethrough)),
];

/// A measurement the font did not report, as the buffer carries it.
fn unsaid(value: Option<f32>) -> f64 {
    value.map(|found| found as f64).unwrap_or(f64::NAN)
}

/// Appends one value's fields, numbers to `out` and strings to `names`.
fn pack<'a, T>(
    fields: &[Field<T>],
    value: &'a T,
    out: &mut Vec<f64>,
    names: &mut Vec<&'a str>,
) {
    for field in fields {
        match field.from {
            Source::Number(read) | Source::Optional(read) => {
                out.push(read(value))
            }
            Source::Family(read) => names.push(read(value)),
        }
    }
}

/// How many numbers a value of `fields` occupies.
const fn packed_len<T>(fields: &[Field<T>]) -> usize {
    let mut len = 0;
    let mut at = 0;
    while at < fields.len() {
        if !matches!(fields[at].from, Source::Family(_)) {
            len += 1;
        }
        at += 1;
    }
    len
}

/// The measurement `measureText` reports, as numbers and names.
///
/// A buffer and an array of family names, rather than the object itself.
/// Building that object here meant about forty property writes -- twelve for
/// the metrics, twelve more for each line and eleven for each run inside it
/// -- and each one is a call across the binding: 4.6 microseconds of a
/// 9.4-microsecond `measureText`, against 3.5 for the typesetting it reports.
/// One buffer is one call and a copy, and the object is assembled in
/// JavaScript, where a property write is a few nanoseconds.
pub fn js_text_metrics<'a, C: Context<'a>>(
    cx: &mut C,
    extents: &TextExtents,
) -> JsResult<'a, JsArray> {
    let runs: usize = extents
        .line_details
        .iter()
        .map(|line| line.runs.len())
        .sum();
    let mut out = Vec::with_capacity(
        packed_len(METRIC_FIELDS)
            + 1
            + extents.line_details.len() * (packed_len(LINE_FIELDS) + 1)
            + runs * packed_len(RUN_FIELDS),
    );
    let mut names = Vec::with_capacity(runs);

    pack(METRIC_FIELDS, extents, &mut out, &mut names);
    out.push(extents.line_details.len() as f64);
    for line in &extents.line_details {
        pack(LINE_FIELDS, line, &mut out, &mut names);
        out.push(line.runs.len() as f64);

        for run in &line.runs {
            pack(RUN_FIELDS, run, &mut out, &mut names);
        }
    }

    let packed = JsFloat64Array::from_slice(cx, &out)?;
    let families = JsArray::new(cx, names.len());
    for (at, name) in names.iter().enumerate() {
        let name = cx.string(name);
        families.set(cx, at as u32, name)?;
    }

    let pair = JsArray::new(cx, 2);
    pair.set(cx, 0u32, packed)?;
    pair.set(cx, 1u32, families)?;
    Ok(pair)
}

/// What the buffer holds, for JavaScript to build its reader from.
///
/// `{ metrics: [...], line: [...], run: [...] }`, each a list of
/// `{ name, kind }` in the order the numbers were written. Published rather
/// than written out on the JavaScript side, so a field added to one of the
/// tables above reaches the object without anything else being touched.
#[allow(non_snake_case)]
pub fn textMetricsFields(mut cx: FunctionContext) -> JsResult<JsObject> {
    fn listed<'a, T, C: Context<'a>>(
        cx: &mut C,
        fields: &[Field<T>],
    ) -> JsResult<'a, JsArray> {
        let list = JsArray::new(cx, fields.len());
        for (at, field) in fields.iter().enumerate() {
            let entry = cx.empty_object();
            let name = cx.string(field.name);
            entry.set(cx, "name", name)?;
            let kind = cx.string(match field.from {
                Source::Number(_) => "number",
                Source::Optional(_) => "optional",
                Source::Family(_) => "family",
            });
            entry.set(cx, "kind", kind)?;
            list.set(cx, at as u32, entry)?;
        }
        Ok(list)
    }

    let table = cx.empty_object();
    let metrics = listed(&mut cx, METRIC_FIELDS)?;
    table.set(&mut cx, "metrics", metrics)?;
    let line = listed(&mut cx, LINE_FIELDS)?;
    table.set(&mut cx, "line", line)?;
    let run = listed(&mut cx, RUN_FIELDS)?;
    table.set(&mut cx, "run", run)?;
    Ok(table)
}

#[cfg(test)]
mod utf16_tests {
    use super::*;

    /// The implementation this replaced, kept as the oracle.
    ///
    /// Character by character from the start of the string, which is what
    /// made it quadratic when called per line. Equivalence is asserted rather
    /// than assumed because the replacement is index arithmetic over a prefix
    /// table and a binary search -- the kind of change that is either exactly
    /// right or off by one everywhere.
    fn reference(text: &str, byte_range: &Range<usize>) -> Range<usize> {
        let chars: Vec<(usize, usize)> = text
            .char_indices()
            .map(|(idx, c)| (idx, c.len_utf16()))
            .collect();
        let start = chars
            .iter()
            .position(|(i, _)| *i >= byte_range.start)
            .unwrap_or(0);
        let end = chars
            .iter()
            .rposition(|(i, _)| *i < byte_range.end)
            .map(|i| i + 1)
            .unwrap_or(start);
        let sum = |a, b| a + b;
        let len = |&(_, len): &(usize, usize)| len;
        let head = chars.iter().take(start).map(len).reduce(sum).unwrap_or(0);
        let tail = chars
            .iter()
            .skip(start)
            .take(end.saturating_sub(start))
            .map(len)
            .reduce(sum)
            .unwrap_or(head);
        head..head + tail
    }

    #[test]
    fn ascii_text_skips_the_index_it_does_not_need() {
        // The equivalence below holds whichever arm answers, so it cannot
        // notice the fast path being lost -- only that it is still right.
        // This is the half that says it is still taken.
        assert!(matches!(
            Utf16Index::new("hello world"),
            Utf16Index::Ascii { .. }
        ));
        assert!(matches!(Utf16Index::new(""), Utf16Index::Ascii { .. }));
        assert!(matches!(
            Utf16Index::new("naïve"),
            Utf16Index::Mapped { .. }
        ));
        assert!(matches!(Utf16Index::new("🎉"), Utf16Index::Mapped { .. }));
    }

    #[test]
    fn the_prefix_index_agrees_with_the_walk_it_replaced() {
        // Astral characters are the case the whole conversion exists for:
        // an emoji is one `char` and two UTF-16 units, so a byte offset and
        // a JavaScript string index part company at the first one.
        let texts = [
            "",
            "a",
            "hello world",
            "wrap this text across lines",
            "naïve café résumé",
            "日本語のテキスト",
            "emoji 😀 then more 🎉 text",
            "🎉🎉🎉",
            "mixed ascii 日本 😀 end",
        ];
        for text in texts {
            let index = Utf16Index::new(text);
            let limit = text.len() + 2;
            for start in 0..limit {
                for end in 0..limit {
                    let range = start..end;
                    assert_eq!(
                        index.range(&range),
                        reference(text, &range),
                        "text {text:?} range {range:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_range_over_astral_characters_counts_utf16_units() {
        // Not just self-consistent with the old walk -- right. Three party
        // poppers are three chars and six UTF-16 units, which is what
        // JavaScript's `String.length` reports.
        let text = "🎉🎉🎉";
        assert_eq!(text.chars().count(), 3);
        let index = Utf16Index::new(text);
        assert_eq!(index.range(&(0..text.len())), 0..6);
        // The middle character alone: two units in, two units long.
        assert_eq!(index.range(&(4..8)), 2..4);
    }
}
