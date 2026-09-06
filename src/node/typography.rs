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
    Color, Font, FontMetrics, GlyphId, Matrix, Paint, Path as SkPath,
    PathBuilder as SkPathBuilder, Point, Rect, Typeface,
    font_style::{FontStyle, Slant, Weight, Width},
    textlayout::{
        Decoration, FontCollection, Paragraph, ParagraphBuilder,
        ParagraphStyle, RectHeightStyle, RectWidthStyle, TextAlign,
        TextDecoration, TextDecorationMode, TextDecorationStyle, TextDirection,
        TextStyle, TypefaceFontProvider,
    },
    typeface::TypefaceId,
};
use std::{
    borrow::Cow, cell::RefCell, collections::HashMap, fmt, iter::zip,
    ops::Range,
};

/// The glyph positions `Paragraph::paint` uses, recovered from the ones the
/// read-back APIs report, or `None` where they cannot be.
///
/// Skia reports a glyph half of its own preceding kern to the right of where
/// it paints that glyph. `Paragraph::paint` is unaffected, so a path built
/// from `Paragraph::get_path_at` does not fill as the same paragraph draws,
/// and an ink box joined from `extended_visit` is wrong by the same half on
/// any string carrying a kern pair. Measured at 480px Helvetica, `"To"`
/// paints its `o` at 240.0 and reports it at 266.6 against a kern of -53.2.
///
/// With `r` reported, `t` painted and `k` the kern before glyph `i`, the two
/// statements
///
/// ```text
/// r(i) = t(i) + k(i) / 2
/// t(i) = t(i - 1) + advance(i - 1) + k(i)
/// ```
///
/// have one solution, and it needs no kern table because the error is exactly
/// half: `t(i) = 2 * r(i) - t(i - 1) - advance(i - 1)`, from `t(0) = r(0)`.
/// The coefficient on `t(i - 1)` is -1, so rounding propagates along the run
/// without amplifying.
///
/// # It depends on the face, not just the string
///
/// The fault needs kerning that comes from the legacy `kern` table. Measured
/// on `"To"` at 480px, with the tables each face carries read off its own
/// header:
///
/// ```text
/// Helvetica.ttc   kern, no GPOS   reports 266.602, paints 240.000   halved
/// Times.ttc       kern, no GPOS   reports 276.445, paints 259.688   halved
/// Arial.ttf       kern and GPOS   reports 240.000, paints 240.000   correct
/// Raleway VF      GPOS only       reports 233.280, paints 233.280   correct
/// ```
///
/// So it is not reachable through every font, and on a platform whose
/// `Helvetica` resolves to a GPOS-kerned substitute it does not arise at
/// all. The guard below is what keeps that case untouched rather than a
/// check on the font's tables: a face with nothing to correct fails the sum
/// and keeps the positions it reported.
///
/// # What the check is, and what it is not
///
/// The recurrence assumes every discrepancy between a reported position and
/// the one advances imply is a halved kern. That is false for scripts whose
/// shaping moves glyphs for other reasons: on Arabic, where Skia reports the
/// painted positions correctly, applying it moved five glyphs and produced a
/// 22-column error against the painter. So the result is checked before it is
/// used -- the last glyph's position plus its own advance must be the run's
/// advance -- and the reported positions are kept when it disagrees. That
/// same check is what makes a GPOS-kerned face a no-op, and what makes this
/// disable itself if Skia is fixed: on positions that are already painted
/// positions the recurrence overshoots by a whole kern and is refused.
///
/// That check is **necessary and not proven sufficient**. It rejects the one
/// failure found (Arabic, off by 21.9 where every correct run was off by
/// zero), and it does not prove that no wrong reconstruction can still sum to
/// the right total. Treat it as a guard that can be strengthened, not as a
/// proof of correctness.
/// Whether this face reports half-kerned positions at all, decided once and
/// remembered.
///
/// The recurrence below cannot tell the two cases apart on its own: for a run
/// that needs reconstructing the kern it recovers *is* the font's kern, and
/// for a run that does not it is exactly twice that kern -- and the font's
/// kern is the thing we do not have. A factor of two between two unknowns is
/// not decidable per run, which is why two run-level checks have both been
/// defeated by it.
///
/// It is decidable per *face*, directly. Lay out a pair that kerns, and ask
/// where the second glyph is reported: at `advance(first) + kern` the face is
/// telling the truth, at `advance(first) + kern / 2` it is not.
///
/// Measured across four sizes each, the verdict is the face's and does not
/// vary with size: Helvetica and Times report half, Arial and Raleway and
/// Oswald report whole, and Amstelvar has no kerning to ask about. Within a
/// face it does not vary by pair either, which is what makes deciding once
/// sound -- thirteen kerning pairs of Arial, which carries a `kern` table
/// *and* GPOS and so is the case that would expose a per-pair split, all
/// answer the same way.
///
/// `None` means no probe pair kerned, so the face has no kerning for the
/// reconstruction to recover and it does not matter which answer is given.
///
/// # Why this lays out text instead of reading the font's tables
///
/// Reading the header would be cheaper and it would be wrong. The obvious
/// rule -- a `kern` table and no GPOS misreports, GPOS reports whole -- holds
/// for Helvetica, Times, Arial and Raleway and fails on Verdana, which
/// carries **both** and still misreports, because its GPOS has no kern
/// feature and HarfBuzz falls back to the legacy table. What decides this is
/// which path the shaper took, and the only way to know that is to ask it.
///
/// # The limit, in the direction that hides
///
/// A face that kerns through `kern` but happens not to kern any of these
/// pairs would be classified `None` and left half-kerned -- not a wrong
/// picture, but silently not fixed. A `kern` table exists to carry pairs like
/// `To` and `AV` so I believe that is rare, and I have not found such a face;
/// I have also not searched hard for one. The pairs are all Latin: a CJK or
/// Arabic face answers `None`, which changes nothing today because the
/// run-level checks already refuse those runs, but that is an assumption
/// rather than something measured.
fn face_reports_half_kerns(font: &Font) -> Option<bool> {
    thread_local! {
        static DECIDED: RefCell<HashMap<TypefaceId, Option<bool>>> =
            RefCell::new(HashMap::new());
    }

    let typeface = font.typeface();
    let id = typeface.unique_id();
    if let Some(known) = DECIDED.with(|c| c.borrow().get(&id).copied()) {
        return known;
    }
    let verdict = classify_face(font, typeface);
    DECIDED.with(|c| {
        c.borrow_mut().insert(id, verdict);
    });
    verdict
}

/// The probe behind [`face_reports_half_kerns`], run once per face.
///
/// Three small paragraphs for the first pair that kerns -- the pair and its
/// two glyphs alone, to get the kern the shaper applied. More if the early
/// pairs do not kern in this face, which is why the list is ordered with the
/// classic kerning pairs first.
fn classify_face(font: &Font, typeface: Typeface) -> Option<bool> {
    /// Pairs that kern in most Latin faces. Oswald does not kern `To` and
    /// does kern `AV`, which is why this is a list rather than one pair.
    const PROBES: [&str; 8] = ["To", "AV", "Ta", "LT", "P.", "Yo", "F,", "we"];

    let em = font.size();
    if em <= 0.0 {
        return None;
    }

    let mut provider = TypefaceFontProvider::new();
    provider.register_typeface(typeface, Some("probe"));
    let mut fonts = FontCollection::new();
    fonts.set_asset_font_manager(Some(provider.into()));

    let width_of = |text: &str| {
        let mut style = TextStyle::new();
        style.set_font_families(&["probe"]);
        style.set_font_size(em);
        let mut builder = ParagraphBuilder::new(&ParagraphStyle::new(), &fonts);
        builder.push_style(&style);
        builder.add_text(text);
        let mut paragraph = builder.build();
        paragraph.layout(f32::INFINITY);
        paragraph.max_intrinsic_width()
    };

    for probe in PROBES {
        let (first, second) = probe.split_at(1);
        let kern = width_of(probe) - width_of(first) - width_of(second);
        // A pair that does not kern cannot tell the two answers apart, and
        // one that barely kerns cannot tell them apart reliably: the two
        // candidate positions are `kern / 2` apart.
        if kern.abs() < em * 0.01 {
            continue;
        }

        let mut style = TextStyle::new();
        style.set_font_families(&["probe"]);
        style.set_font_size(em);
        let mut builder = ParagraphBuilder::new(&ParagraphStyle::new(), &fonts);
        builder.push_style(&style);
        builder.add_text(probe);
        let mut paragraph = builder.build();
        paragraph.layout(f32::INFINITY);

        let mut reported = None;
        paragraph.extended_visit(|_line, visit| {
            if let Some(info) = visit
                && info.glyphs().len() == 2
            {
                reported = Some(info.positions()[1].x);
            }
        });
        let Some(reported) = reported else { continue };

        let whole = width_of(first) + kern;
        let halved = width_of(first) + kern / 2.0;
        return Some((reported - halved).abs() < (reported - whole).abs());
    }
    None
}

fn painted_positions(
    font: &Font,
    glyphs: &[GlyphId],
    reported: &[Point],
    run_advance: f32,
) -> Option<Vec<Point>> {
    // Two questions, and they are not the same one. This asks whether the
    // *face* misreports at all; the checks below ask whether this particular
    // *run* can be reconstructed. A face that half-kerns can still contain a
    // run the recurrence cannot model -- a cursive or mark-positioned span, a
    // ligature run -- so neither makes the other redundant, and removing
    // either leaves a hole the other never covered.
    if face_reports_half_kerns(font) != Some(true) {
        return None;
    }
    reconstruct_run(font, glyphs, reported, run_advance)
}

/// The run-level half: can *this* run be reconstructed, given that its face
/// misreports?
fn reconstruct_run(
    font: &Font,
    glyphs: &[GlyphId],
    reported: &[Point],
    run_advance: f32,
) -> Option<Vec<Point>> {
    if glyphs.len() != reported.len() || glyphs.len() < 2 {
        // One glyph has no preceding kern, so there is nothing to recover.
        return None;
    }

    let mut advances = vec![0.0; glyphs.len()];
    font.get_widths(glyphs, &mut advances);

    let mut painted = Vec::with_capacity(reported.len());
    painted.push(reported[0]);
    let mut widest_positive_kern = 0.0f32;
    for i in 1..reported.len() {
        let x = 2.0 * reported[i].x - painted[i - 1].x - advances[i - 1];
        // The kern the recurrence had to assume to place this glyph. On a run
        // that really is half-kerned it is the font's own kern; on one that is
        // already correct it is whatever the arithmetic needs, and that is
        // where the two separate.
        widest_positive_kern =
            widest_positive_kern.max(x - painted[i - 1].x - advances[i - 1]);
        painted.push(Point::new(x, reported[i].y));
    }

    // A kern that spreads a pair apart by a fifth of the em is not a kern, it
    // is the recurrence being applied where it does not belong.
    //
    // The sum check below is necessary and not sufficient: the error it
    // measures is `e(i) = k(i) - e(i-1)`, an *alternating* sum, and an
    // alternating series can cancel. `"To "` repeated gives the kern sequence
    // `k, 0, 0, k, 0, 0, ...` whose alternating sum vanishes, and a correct
    // GPOS run of it came back 3.6e-4 of its advance -- inside the threshold --
    // and had its glyphs moved 68px at 480px. This is the per-glyph half of
    // the guard, and it does not depend on anything cancelling.
    //
    // Measured over 18 runs -- Helvetica and Times, three strings, 24px, 48px
    // and 480px -- a genuinely half-kerned run recovers **no** positive kern
    // at all, exactly 0.00000 em in every case. The two runs that must be
    // refused recover +0.228 em and +0.328 em, at every size. The bound sits
    // between them with room for a face that does kern a pair apart, which
    // these two never do.
    //
    // What would falsify it: a face that genuinely spreads a pair by more
    // than this. Such a run would be refused where it should be
    // reconstructed, which is a silent no-op rather than a wrong picture --
    // the reported positions stand and stay half-kerned. That is the same
    // failure the first sum threshold had, so if one is ever found the answer
    // is to raise the bound against the measurement rather than to widen it
    // on suspicion.
    const WIDEST_PLAUSIBLE_KERN_EM: f32 = 0.05;
    if widest_positive_kern > WIDEST_PLAUSIBLE_KERN_EM * font.size() {
        return None;
    }

    // A thousandth of the run's own width, which is where the two answers
    // this has to tell apart actually sit.
    //
    // The first version of this budgeted for `f32` rounding -- one unit in
    // the last place per glyph -- and that is the wrong model, not merely a
    // mis-scaled one. The noise here is quantisation between the advances
    // `get_widths` reports and the ones Skia laid out with, and it holds at
    // roughly `4e-6` of the run advance however long the run is: measured
    // `3.9e-6` at two glyphs, `1.1e-6` at sixteen and `5.5e-6` at 1199. An
    // ulp-per-glyph budget is far under that, and it rejected seven runs of
    // `"Wave To the Yak 世界 "` that were half-kerned and needed correcting.
    //
    // What has to be separated is that noise from a run whose positions are
    // already the painted ones, where the sum misses by the run's whole
    // kerning -- `4.7e-2` of the advance for the Arabic case, four orders
    // above the noise. So a thousandth sits about 250 times above what a
    // correct reconstruction costs and 47 times below the smallest wrong one
    // measured. `max(1.0)` keeps a zero-width run from being asked for
    // exactness it already has.
    const RELATIVE_TOLERANCE: f32 = 1e-3;
    let tolerance = run_advance.abs().max(1.0) * RELATIVE_TOLERANCE;
    // SAFETY: `painted` and `advances` are both `glyphs.len()` long and that
    // is at least two, so both `last` calls are `Some`.
    let implied = painted.last().expect("non-empty").x
        + advances.last().expect("non-empty");
    match (implied - run_advance).abs() <= tolerance {
        true => Some(painted),
        false => None,
    }
}

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

pub struct Typesetter<'a> {
    /// The text as it will be laid out, borrowed from the caller's `&str`
    /// where normalising changed nothing.
    ///
    /// A `String` here meant every unwrapped draw copied the string to own a
    /// byte-identical version of what it had been handed, on the path
    /// `fillText`, `strokeText`, `measureText` and `outlineText` all take.
    text: Cow<'a, str>,
    width: f32,
    /// The `maxWidth` the run is condensed to fit, where one was given and
    /// wrapping is off.
    ///
    /// Held apart from `width`, which is the layout budget. The two were one
    /// field, and that is what made a `maxWidth` a wrap width: the paragraph
    /// broke at it and `max_lines(1)` discarded everything past the first
    /// line, so `fillText("Hello maxWidth world", 4, 60, 193)` painted byte
    /// for byte what `fillText("Hello", 4, 60)` paints.
    condense_to: Option<f32>,
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

/// Whether `ch` ends a word, for the purpose of kerning.
///
/// A space, and the hard breaks that survive it: [`normalize_to_one_line`]
/// turns those into spaces on the non-wrapping path, but wrapping mode lays
/// them out as themselves, so both have to be named here or kerning would be
/// suppressed across a space and not across a newline in the same string.
fn is_word_separator(ch: char) -> bool {
    ch == ' ' || HARD_BREAKS.contains(&ch)
}

/// Replaces every hard break in `text` with a space.
///
/// Borrows where there is nothing to replace, which is the overwhelmingly
/// common case for a single-line draw. The `contains` guard is not redundant
/// with `replace`: `str::replace` allocates whether or not it finds anything,
/// so without the guard every draw would pay for a copy to hand back the
/// string it was given.
fn normalize_to_one_line(text: &str) -> Cow<'_, str> {
    match text.contains(HARD_BREAKS) {
        true => Cow::Owned(text.replace(HARD_BREAKS, " ")),
        false => Cow::Borrowed(text),
    }
}

/// A run laid out and ready to paint.
///
/// The condensation travels with the paragraph because it cannot be derived
/// from it afterwards -- it needs the `maxWidth` the typesetter holds -- and
/// a caller that painted the paragraph without it would silently draw the
/// uncondensed run.
pub struct TextLayout {
    /// The laid-out paragraph.
    pub paragraph: Paragraph,
    /// Where to paint it, relative to the anchor the caller supplies.
    pub offset: Point,
    /// The horizontal factor the run is squeezed by, about the anchor.
    ///
    /// `1.0` where no `maxWidth` was given or the run already fits. Chrome
    /// applies this as a plain anisotropic transform rather than a narrower
    /// face: at 200px with `lineWidth` 12, a half-width `strokeText("H")`
    /// draws stems 6 pixels wide and a crossbar still 12, and the inked
    /// height does not move at any factor.
    pub condensation: f32,
}

/// Scales a measured box horizontally about the anchor.
///
/// The vertical extent does not move: condensation is a horizontal transform
/// and Chrome's inked height is identical at every factor, down to a tenth.
fn squeeze(rect: Rect, factor: f32) -> Rect {
    match factor {
        1.0 => rect,
        s => Rect::new(rect.left * s, rect.top, rect.right * s, rect.bottom),
    }
}

impl<'a> Typesetter<'a> {
    pub fn new(state: &State, text: &'a str, width: Option<f32>) -> Self {
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
        // With wrapping on, the width given to a draw is the wrap width and
        // is what the paragraph is laid out to -- this fork's extension, and
        // the only reading under which a paragraph may break. With it off,
        // the Canvas standard says the run is laid out unconstrained and
        // then condensed to fit, so the layout budget is the galley and the
        // width is carried separately.
        let (width, condense_to) = match text_wrap {
            true => (width.unwrap_or(GALLEY), None),
            false => (GALLEY, width),
        };
        let text = match text_wrap {
            // Wrapping mode lays the breaks out rather than replacing them,
            // so the caller's string is used as it is.
            true => Cow::Borrowed(text),
            false => normalize_to_one_line(text),
        };
        // "If maxWidth was provided but is less than or equal to zero or
        // equal to NaN, then return an empty array" -- the text preparation
        // algorithm, which every text operation runs. An empty run draws
        // nothing, outlines to an empty path and measures as zero, which is
        // what Chrome does: `fillText` with a `maxWidth` of 0 or -5 inks no
        // pixel at all.
        let text = match condense_to {
            Some(max) if max <= 0.0 || max.is_nan() => Cow::Borrowed(""),
            _ => text,
        };
        // Dropped once the run is emptied, so nothing downstream divides by
        // the natural width of nothing: `0.0 / -5.0` is a negative infinity
        // that multiplies an empty box into `NaN`, and `measureText(t, -5)`
        // reported that as its width.
        let condense_to = condense_to.filter(|max| *max > 0.0);

        Typesetter {
            text,
            width,
            condense_to,
            baseline,
            typefaces,
            matched_style,
            char_style,
            graf_style,
            text_decoration,
            text_wrap,
        }
    }

    /// Lays the run out, and works out how far it has to be squeezed.
    pub fn layout(&self, paint: &Paint) -> TextLayout {
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

        // Kerning stops at a word boundary. Skia does not do that on its own
        // and a browser does it without exception -- Chrome's `"A V"` is
        // exactly `w("A") + w(" ") + w("V")` across every pair measured,
        // including ones it kerns tight, where this was 1.33 narrower at 24px
        // because the `AV` pair reached across the space.
        //
        // **The style boundary is what suppresses it, not the feature.** Skia
        // segments a shaping run where a shaping-relevant style changes, and a
        // kern pair cannot form across two runs; pushing a different font size
        // on the separator does the same thing, and pushing a different colour
        // or an identical letter spacing does not. `kern = 0` is chosen
        // because it is a boundary that is also honest about what it wants.
        //
        // Splitting the text across several `add_text` calls does *not* work
        // -- Skia concatenates and shapes as one run regardless of how many
        // calls made it, so the obvious approach measures identically to no
        // change at all.
        //
        // If Skia ever stops treating this feature as shaping-relevant the
        // runs merge again and the suppression silently disappears, so the
        // guard is a width assertion rather than a check that this code ran.
        if self.text.contains(is_word_separator) {
            let mut unkerned = char_style.clone();
            unkerned.add_font_feature("kern", 0);

            let mut rest: &str = &self.text;
            while !rest.is_empty() {
                let separating = rest.starts_with(is_word_separator);
                let end = rest
                    .find(|ch| is_word_separator(ch) != separating)
                    .unwrap_or(rest.len());
                let (piece, tail) = rest.split_at(end);
                paragraph_builder
                    .push_style(match separating {
                        true => &unkerned,
                        false => &char_style,
                    })
                    .add_text(piece)
                    .pop();
                rest = tail;
            }
        } else {
            // One run, as before: nothing to separate.
            paragraph_builder.push_style(&char_style);
            paragraph_builder.add_text(&self.text);
        }

        let mut paragraph = paragraph_builder.build();
        paragraph.layout(self.width);

        let offset = Point::new(
            self.alignment_offset(),
            -paragraph.alphabetic_baseline(),
        );

        // The natural width is the one `measureText` reports, so that a draw
        // constrained to a run's own measured width is a no-op rather than a
        // hair's-breadth squeeze. `max_intrinsic_width` is that width for a
        // run laid out to the galley: nothing wrapped, so what the paragraph
        // would take unwrapped is what it took.
        let condensation = match self.condense_to {
            Some(max) => {
                let natural = paragraph.max_intrinsic_width();
                match natural > max {
                    true => max / natural,
                    false => 1.0,
                }
            }
            None => 1.0,
        };

        TextLayout {
            paragraph,
            offset,
            condensation,
        }
    }

    /// Measurements of the run, as a struct rather than as JSON.
    ///
    /// `metrics` below serializes for the Node binding and is what the JS
    /// `measureText` returns; this is a sibling for the Rust API rather than
    /// a refactor of it, so that output stays byte-for-byte identical. The
    /// two share the baseline math deliberately: they are measuring the same
    /// thing and must not drift.
    pub fn extents(&self) -> TextExtents {
        let TextLayout {
            mut paragraph,
            offset: origin,
            condensation,
        } = self.layout(&Paint::default());

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
                    // `painted_positions` rather than `info.positions()`:
                    // the reported ones put a glyph half its own preceding
                    // kern too far right, so an ink box joined from them is
                    // wider than the text that gets drawn.
                    bounds: zip(
                        painted_positions(
                            info.font(),
                            info.glyphs(),
                            info.positions(),
                            info.advance().width,
                        )
                        .unwrap_or_else(|| info.positions().to_vec()),
                        zip(info.glyphs(), info.bounds()),
                    )
                    .filter(|(_, (_, rect))| !rect.is_empty())
                    .map(|(pt, (glyph, rect))| {
                        // The glyph's outline, not the box `info.bounds()`
                        // reports. That one is the rasterisation box: the
                        // outline rounded outwards to the pixel grid and
                        // padded for the mask. At 48px it is exactly
                        // `floor - 1` and `ceil + 1` on every glyph measured,
                        // across Helvetica, Times, Arial and Courier New; at
                        // 480px the margin is wider and scales with the size.
                        // Either way it is lossy -- 3.773 cannot be recovered
                        // from 2 -- so the subpixel box the Canvas API asks
                        // for has to come from the outline.
                        //
                        // A glyph with no outline, a bitmap or colour emoji,
                        // keeps the reported box: it is the only one there is
                        // for a glyph that is not a path.
                        let ink = info
                            .font()
                            .get_path(*glyph)
                            .map(|outline| outline.compute_tight_bounds())
                            .filter(|outline| !outline.is_empty())
                            .unwrap_or(*rect);
                        // `info.origin()` as reported, rounding and all.
                        // Its `y` is the run's baseline snapped to a whole
                        // pixel -- 37 against 36.960938 at 48px -- and
                        // `Paragraph::paint` draws with the same snapped
                        // value, so a box measured without it would describe
                        // ink this library does not put on the canvas. The
                        // Canvas API asks for the box of the text as drawn,
                        // and that is this one. Chrome does not round here
                        // and so reports 0.039 more ascent at 48px; the
                        // difference is in the drawing, not the measurement.
                        ink.with_offset(
                            pt + info.origin() + origin - Point::new(0.0, norm),
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
        let mut ink_bounds: Option<Rect> = None;
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
            // The glyphs' own box, kept apart from the layout one because
            // the two answer different questions and the Canvas API asks
            // both. See `TextExtents::ink`.
            ink_bounds = Some(match ink_bounds {
                Some(so_far) => Rect::join2(so_far, text_bounds),
                None => text_bounds,
            });
        }
        let full_bounds = full_bounds.unwrap_or(Rect::new_empty());
        let ink_bounds = ink_bounds.unwrap_or(full_bounds);

        // Condensation is a horizontal scale about the anchor, and these
        // coordinates are already relative to it -- `origin` carries the
        // alignment offset -- so every horizontal quantity is multiplied by
        // it and no vertical one is. Applied here rather than at each
        // source: the factor is uniform across the paragraph, and threading
        // it through the joins would have every one of them restate that.
        let full_bounds = squeeze(full_bounds, condensation);
        // The ink box is a second accumulator and takes the same factor. It
        // did not exist when the condensation went in, so a merge of the two
        // changes leaves it unscaled and a condensed run reports the ink box
        // of the uncondensed one -- which is what
        // `measuring and outlining condense with the drawing` catches, once
        // the horizontal pair reads this rather than `full_bounds`.
        let ink_bounds = squeeze(ink_bounds, condensation);
        if condensation != 1.0 {
            for line in &mut line_details {
                line.x *= condensation;
                line.width *= condensation;
                for run in &mut line.runs {
                    run.x *= condensation;
                    run.width *= condensation;
                }
            }
        }

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
            ink: ink_bounds,
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
        let TextLayout {
            mut paragraph,
            offset: mut origin,
            condensation,
        } = self.layout(&Paint::default());
        let headroom = self.char_style.font_metrics().ascent
            + paragraph.alphabetic_baseline();
        let offset = self.baseline.get_offset(&self.char_style);
        let anchor: Point = point.into();
        origin += anchor;
        origin.y -= headroom - offset;

        // Built a run at a time from `painted_positions` rather than taken
        // from `Paragraph::get_path_at`, which places each glyph half its own
        // preceding kern to the right of where `Paragraph::paint` draws it --
        // so the path this returns used to fill differently from the text it
        // was taken from. Filed upstream; `painted_positions` documents the
        // recovery and the guard on it.
        let mut runs: Vec<(Font, Vec<GlyphId>, Vec<Point>, Point)> = vec![];
        paragraph.extended_visit(|_line, visit| {
            if let Some(info) = visit {
                let positions = painted_positions(
                    info.font(),
                    info.glyphs(),
                    info.positions(),
                    info.advance().width,
                )
                .unwrap_or_else(|| info.positions().to_vec());
                runs.push((
                    info.font().clone(),
                    info.glyphs().to_vec(),
                    positions,
                    info.origin(),
                ));
            }
        });

        // One glyph outline at a time rather than through
        // `Paragraph::GetPath`, which applies a translation of its own: for
        // `"To"` it returned a path starting at x = 46.4 from a blob whose
        // own bounds start at -45.6. `Font::get_path` is the glyph in its own
        // em box, so the only offset applied here is the one this function
        // computed. A glyph with no outline -- a bitmap or colour emoji --
        // yields `None` and contributes nothing, which is what
        // `get_path_at`'s skipped-glyph count reported.
        let mut builder = SkPathBuilder::new();
        for (font, glyphs, positions, run_origin) in runs {
            for (glyph, at) in zip(glyphs, positions) {
                if let Some(outline) = font.get_path(glyph) {
                    builder.add_path(
                        &outline.with_offset(at + run_origin + origin),
                        None,
                    );
                }
            }
        }
        let path = builder.detach();

        // Squeezed about the anchor, which is where a draw squeezes it: the
        // outline of a condensed run has to be the shape that run paints.
        match condensation {
            1.0 => path,
            s => path.with_transform(
                &(Matrix::translate(anchor)
                    * Matrix::scale((s, 1.0))
                    * Matrix::translate(-anchor)),
            ),
        }
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

        // `alignment_factor` shifts the whole line to left/right/centre align
        // it. `spacing_step` is the letter-spacing correction, and it is only
        // needed on the left: Skia puts half a space before the line's first
        // character, so a left-aligned run starts half a space right of the
        // anchor until that is taken back.
        //
        // Centre and right take none. Skia's line box is symmetric about the
        // ink -- half a space at each end -- so aligning it already puts the
        // glyphs where CSS wants them, which counts the trailing space as
        // part of the inline box. The `0.5` and `1.0` that stood here shifted
        // the run *back* by that amount and pinned it in place: at 40px with
        // 10px spacing, centred text kept its midpoint on the anchor at every
        // spacing where Chrome moves it 5 pixels left, and right-aligned text
        // kept its right edge there where Chrome moves it 10. Eighteen rows
        // across two strings and three spacings agree with Chrome 148 now,
        // the three that do not being a half-pixel in one ink edge that is
        // there at zero spacing too.
        let (alignment_factor, spacing_step) = match gravity {
            TextAlign::Left | TextAlign::Justify => (0.0, -0.5),
            TextAlign::Center => (-0.5, 0.0),
            TextAlign::Right => (-1.0, 0.0),
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
    /// The string that names this specification uniquely, and the key the
    /// resolved-font cache uses. Carries every component, including the ones
    /// at their initial values and the line height.
    pub canonical: String,
    /// The string the `font` getter reports: the same specification with
    /// initial values and the line height left out, as the Canvas API
    /// requires. Never a cache key -- two line heights serialize alike.
    pub serialized: String,
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
    let serialized = string_for_key(cx, &font_desc, "serialized")?;
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
            serialized,
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

#[cfg(test)]
mod tests {
    use super::{Cow, normalize_to_one_line};

    /// The borrow is the point, so it is asserted rather than assumed.
    ///
    /// The doc comment claimed it before the code did: this returned a
    /// `String` and copied the text it was handed, on the path every
    /// unwrapped `fillText`, `strokeText`, `measureText` and `outlineText`
    /// takes. A test on the returned text alone would have passed throughout.
    #[test]
    fn text_with_no_hard_break_is_not_copied() {
        assert!(matches!(
            normalize_to_one_line("a plain single-line label"),
            Cow::Borrowed(_)
        ));
    }

    /// And the replacing case still replaces, every character of it.
    ///
    /// `U+000B`, `U+2028` and `U+2029` are here because they are not ASCII
    /// whitespace: the Canvas standard's text preparation does not reach
    /// them, and they are replaced anyway because the alternative is
    /// discarding the rest of the string.
    #[test]
    fn every_hard_break_becomes_a_space() {
        let text = "A\tB\nC\u{b}D\u{c}E\rF\u{2028}G\u{2029}H";
        let flat = normalize_to_one_line(text);
        assert!(matches!(flat, Cow::Owned(_)), "a replacement allocates");
        assert_eq!(flat, "A B C D E F G H");
    }
}

#[cfg(test)]
mod half_kern {
    use super::{face_reports_half_kerns, painted_positions, reconstruct_run};
    use skia_safe::{Data, FontMgr, Point};

    fn raleway() -> skia_safe::Font {
        let bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/assets/fonts/Raleway/Raleway-VariableFont_wght.ttf"
        ))
        .expect("bundled Raleway is readable");
        let typeface = FontMgr::new()
            .new_from_data(Data::new_copy(&bytes), None)
            .expect("Raleway parses");
        skia_safe::Font::from_typeface(typeface, 480.0)
    }

    /// The two glyph ids for `"To"`, and their advances.
    fn to_glyphs(font: &skia_safe::Font) -> ([u16; 2], [f32; 2]) {
        let mut ids = [0u16; 2];
        font.str_to_glyphs("To", &mut ids);
        let mut widths = [0.0; 2];
        font.get_widths(&ids, &mut widths);
        (ids, widths)
    }

    /// A synthetic run carrying the defect: the second glyph placed half its
    /// kern to the right of where it belongs.
    ///
    /// Built rather than measured because no bundled face reproduces it --
    /// the fault needs kerning from the legacy `kern` table, and every font
    /// in `tests/assets` kerns through GPOS. The arithmetic is what is under
    /// test here; `outline_text` agreeing with a draw is covered from
    /// JavaScript, where a real face can be named.
    #[test]
    fn the_recurrence_recovers_a_halved_kern() {
        let font = raleway();
        let (ids, widths) = to_glyphs(&font);
        let kern = -54.72;

        let reported = [
            Point::new(0.0, 0.0),
            Point::new(widths[0] + kern / 2.0, 0.0),
        ];
        let advance = widths[0] + widths[1] + kern;

        let painted = reconstruct_run(&font, &ids, &reported, advance)
            .expect("a halved kern is recoverable");
        assert!(
            (painted[1].x - (widths[0] + kern)).abs() < 0.01,
            "the second glyph goes back to advance + kern: {} against {}",
            painted[1].x,
            widths[0] + kern
        );
    }

    /// And a run that is already right is left alone.
    ///
    /// This is the case every GPOS-kerned face presents, which is most of
    /// them, and it is also what a fixed Skia would present for all of them
    /// -- so the fix disables itself rather than doubling the correction.
    #[test]
    fn a_run_that_is_already_right_is_refused() {
        let font = raleway();
        let (ids, widths) = to_glyphs(&font);
        let kern = -54.72;

        let reported =
            [Point::new(0.0, 0.0), Point::new(widths[0] + kern, 0.0)];
        let advance = widths[0] + widths[1] + kern;

        assert!(
            reconstruct_run(&font, &ids, &reported, advance).is_none(),
            "the guard refuses a run it would move away from the truth"
        );
    }

    /// A correct run whose errors cancel is still refused.
    ///
    /// The sum check alone cannot do this. The error the recurrence puts on a
    /// run that needs no reconstruction is `e(i) = k(i) - e(i-1)`, so the
    /// total is an *alternating* sum of the kerns, and `"To "` repeated gives
    /// the sequence `k, 0, 0, k, 0, 0, ...` whose alternating sum vanishes.
    /// Raleway kerns through GPOS, so its reported positions are already the
    /// painted ones; before the per-glyph bound this run came back 3.6e-4 of
    /// its advance -- inside the threshold -- was accepted, and had its glyphs
    /// moved 68px at 480px.
    ///
    /// Nothing reaches this through `Typesetter`, because #117 pushes a
    /// separate style at word boundaries and a spaced string never becomes one
    /// long run. That makes the guard's correctness depend on a change made
    /// for a different reason, which is why this is asserted here against a
    /// bare paragraph rather than through the public API.
    #[test]
    fn a_run_whose_errors_cancel_is_still_refused() {
        use skia_safe::textlayout::{
            FontCollection, ParagraphBuilder, ParagraphStyle, TextStyle,
            TypefaceFontProvider,
        };

        let bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/assets/fonts/Raleway/Raleway-VariableFont_wght.ttf"
        ))
        .expect("bundled Raleway is readable");
        let typeface = FontMgr::new()
            .new_from_data(Data::new_copy(&bytes), None)
            .expect("Raleway parses");
        let mut provider = TypefaceFontProvider::new();
        provider.register_typeface(typeface, Some("Raleway"));
        let mut fonts = FontCollection::new();
        fonts.set_asset_font_manager(Some(provider.into()));
        let mut style = TextStyle::new();
        style.set_font_families(&["Raleway"]);
        style.set_font_size(480.0);
        let mut builder = ParagraphBuilder::new(&ParagraphStyle::new(), &fonts);
        builder.push_style(&style);
        builder.add_text("To ".repeat(60));
        let mut paragraph = builder.build();
        paragraph.layout(f32::INFINITY);

        // Collected, not asserted: a panic here crosses Skia's visitor and
        // aborts the binary. See `skia_still_halves_a_legacy_kern`.
        let mut long_runs = 0;
        let mut accepted = 0;
        let mut misses = vec![];
        paragraph.extended_visit(|_line, visit| {
            if let Some(info) = visit {
                if info.glyphs().len() < 20 {
                    return;
                }
                long_runs += 1;
                let advance = info.advance().width;
                let mut widths = vec![0.0; info.glyphs().len()];
                info.font().get_widths(info.glyphs(), &mut widths);
                let mut rebuilt = vec![info.positions()[0].x];
                for i in 1..info.glyphs().len() {
                    rebuilt.push(
                        2.0 * info.positions()[i].x
                            - rebuilt[i - 1]
                            - widths[i - 1],
                    );
                }
                let last = rebuilt.len() - 1;
                misses.push(
                    (rebuilt[last] + widths[last] - advance).abs()
                        / advance.abs().max(1.0),
                );
                // `reconstruct_run`, not `painted_positions`: the face gate
                // refuses Raleway outright, so going through the entry point
                // would make this pass without ever reaching the per-glyph
                // bound it was written to test. The gate is asserted
                // separately, in `a_gpos_face_is_refused_before_any_run_is`.
                if reconstruct_run(
                    info.font(),
                    info.glyphs(),
                    info.positions(),
                    advance,
                )
                .is_some()
                {
                    accepted += 1;
                }
            }
        });

        assert_eq!(long_runs, 1, "fixture: one long run to judge");
        // The fixture is only interesting while the sum check is fooled by it.
        // If Skia's shaping changes and this run stops cancelling, the sum
        // check would refuse it on its own and this test would pass for the
        // wrong reason -- so the cancellation is asserted, not assumed.
        assert!(
            misses[0] < 1e-3,
            "fixture: the sum check alone accepts this run, miss {}",
            misses[0]
        );
        assert_eq!(accepted, 0, "the per-glyph bound refuses it anyway");
    }

    /// Nothing lands near the threshold, which is why it can sit where it
    /// does.
    ///
    /// A run this can reconstruct misses the sum by quantisation noise, a few
    /// millionths of the run's width. A run it must not touch misses by the
    /// run's whole kerning, a few hundredths. The threshold is a thousandth,
    /// between the two, and this asserts the band around it stays empty --
    /// if a real run ever lands in it, the number is arbitrary and the guard
    /// is guessing.
    ///
    /// Written because the first threshold was derived from `f32` rounding
    /// rather than from measurement, landed at the noise instead of between
    /// the two populations, and silently refused seven half-kerned runs of
    /// the mixed string below. Nothing in the suite noticed: the tests were
    /// all single-run Latin, where the reconstruction is exact and the
    /// distance to the threshold never mattered.
    ///
    /// macOS only, and that limit is the finding rather than a convenience.
    /// The lower population is not `f32` rounding -- it is the difference
    /// between the advances `get_widths` reports and the ones Skia laid out
    /// with, which is a property of the rasteriser. It was measured at about
    /// four millionths of the run advance on Core Text. A hinted FreeType
    /// rasteriser quantises advances to the pixel grid, so that difference
    /// can be orders larger without either population having moved, which
    /// would put a run in the band and fail this for a reason that says
    /// nothing about the threshold. Asserting it where it was measured is
    /// the honest scope; #139 covers establishing the floor elsewhere.
    #[test]
    #[cfg(target_os = "macos")]
    fn no_run_lands_near_the_threshold() {
        use skia_safe::textlayout::{
            FontCollection, ParagraphBuilder, ParagraphStyle, TextStyle,
        };

        for text in [
            "Wave To the Yak \u{4e16}\u{754c} ".repeat(20),
            "To \u{1f600} Va ".repeat(10),
            "To \u{645}\u{631}\u{62d}\u{628}\u{627} Va".to_string(),
            "AVATo Wave Yak Type ".repeat(40),
        ] {
            let mut fonts = FontCollection::new();
            fonts.set_default_font_manager(FontMgr::new(), None);
            fonts.enable_font_fallback();
            let mut style = TextStyle::new();
            style.set_font_families(&["Helvetica"]);
            style.set_font_size(480.0);
            let mut builder =
                ParagraphBuilder::new(&ParagraphStyle::new(), &fonts);
            builder.push_style(&style);
            builder.add_text(&text);
            let mut paragraph = builder.build();
            paragraph.layout(f32::INFINITY);

            let mut runs = 0;
            let mut measured = Vec::new();
            paragraph.extended_visit(|_line, visit| {
                if let Some(info) = visit {
                    let glyphs = info.glyphs();
                    if glyphs.len() < 2 {
                        return;
                    }
                    runs += 1;
                    let advance = info.advance().width;
                    let mut widths = vec![0.0; glyphs.len()];
                    info.font().get_widths(glyphs, &mut widths);
                    let mut rebuilt = vec![info.positions()[0].x];
                    for i in 1..glyphs.len() {
                        rebuilt.push(
                            2.0 * info.positions()[i].x
                                - rebuilt[i - 1]
                                - widths[i - 1],
                        );
                    }
                    let miss = (rebuilt[glyphs.len() - 1]
                        + widths[glyphs.len() - 1]
                        - advance)
                        .abs()
                        / advance.abs().max(1.0);
                    // Recorded rather than asserted here. A panic inside
                    // this closure crosses Skia's C++ visitor trampoline,
                    // where it cannot unwind -- the process aborts with
                    // SIGABRT and the whole test binary dies, so a single
                    // wrong run takes down every other test in the file and
                    // reports none of them. The assertions are below, where
                    // a failure is a failure.
                    let taken = reconstruct_run(
                        info.font(),
                        glyphs,
                        info.positions(),
                        advance,
                    )
                    .is_some();
                    measured.push((
                        miss,
                        taken,
                        info.font().typeface().family_name(),
                        glyphs.len(),
                    ));
                }
            });
            assert!(runs > 0, "fixture: {text:?} laid out runs to check");

            for (miss, taken, family, glyphs) in &measured {
                assert!(
                    *miss < 1e-4 || *miss > 1e-2,
                    "a run misses the sum by {miss} of its width, which is \
                     neither noise nor a whole kerning -- the threshold at \
                     1e-3 is then a guess. Family {family}, {glyphs} glyphs."
                );

                // And the guard has to act on that: a run whose sum comes
                // back at the noise is reconstructable and must be taken.
                // This is the assertion the first threshold would have
                // failed -- it refused seven such runs and every test then in
                // the suite stayed green.
                assert_eq!(
                    *taken,
                    *miss < 1e-4,
                    "a run missing by {miss} of its width should {} be \
                     reconstructed. Family {family}, {glyphs} glyphs.",
                    if *miss < 1e-4 { "" } else { "not" }
                );
            }
        }
    }

    /// A face that does not misreport is refused before any run is judged.
    ///
    /// This is #153. A long run with its reconstruction error concentrated in
    /// the last term and never positive walks through both run-level checks
    /// -- the sum reads `|e(last)|` and the per-glyph bound reads
    /// `max e(i)` on the positive side, and `"n" * 200 + "To"` in a
    /// GPOS-kerned face is small in both while its interior glyphs move 54px
    /// at 480px. Asking about the face instead settles it before the run is
    /// looked at.
    #[test]
    fn a_gpos_face_is_refused_before_any_run_is() {
        use skia_safe::textlayout::{
            FontCollection, ParagraphBuilder, ParagraphStyle, TextStyle,
            TypefaceFontProvider,
        };

        let bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/assets/fonts/Raleway/Raleway-VariableFont_wght.ttf"
        ))
        .expect("bundled Raleway is readable");
        let typeface = FontMgr::new()
            .new_from_data(Data::new_copy(&bytes), None)
            .expect("Raleway parses");
        let mut provider = TypefaceFontProvider::new();
        provider.register_typeface(typeface, Some("Raleway"));
        let mut fonts = FontCollection::new();
        fonts.set_asset_font_manager(Some(provider.into()));
        let mut style = TextStyle::new();
        style.set_font_families(&["Raleway"]);
        style.set_font_size(480.0);
        let mut builder = ParagraphBuilder::new(&ParagraphStyle::new(), &fonts);
        builder.push_style(&style);
        builder.add_text(format!("{}To", "n".repeat(200)));
        let mut paragraph = builder.build();
        paragraph.layout(f32::INFINITY);

        let mut long_runs = 0;
        let mut run_level_would_accept = 0;
        let mut entry_point_accepts = 0;
        paragraph.extended_visit(|_line, visit| {
            if let Some(info) = visit {
                if info.glyphs().len() < 20 {
                    return;
                }
                long_runs += 1;
                let advance = info.advance().width;
                if reconstruct_run(
                    info.font(),
                    info.glyphs(),
                    info.positions(),
                    advance,
                )
                .is_some()
                {
                    run_level_would_accept += 1;
                }
                if painted_positions(
                    info.font(),
                    info.glyphs(),
                    info.positions(),
                    advance,
                )
                .is_some()
                {
                    entry_point_accepts += 1;
                }
            }
        });

        assert_eq!(long_runs, 1, "fixture: one long run to judge");
        // The fixture is only interesting while the run-level checks are
        // fooled by it. If shaping changes and they start refusing it on
        // their own, this would pass for the wrong reason.
        assert_eq!(
            run_level_would_accept, 1,
            "fixture: the run-level checks alone still accept this run"
        );
        assert_eq!(
            entry_point_accepts, 0,
            "the face gate refuses it before the run is judged"
        );
    }

    /// The classifier's own detector, and like the one below it asserts what
    /// is *wrong* today.
    ///
    /// A failure here is good news: Skia has stopped reporting half-kerned
    /// positions for a face that used to, and the reconstruction should come
    /// out. Without it the fix would quietly become a no-op -- every face
    /// would classify as truthful, nothing would be reconstructed, and every
    /// other test here would still pass.
    ///
    /// Raleway and Amstelvar are bundled. Helvetica is macOS-only and is the
    /// only face here that answers `true`, which is why this test cannot
    /// assert a `true` anywhere else.
    #[test]
    fn the_classifier_still_finds_a_face_that_misreports() {
        let raleway_says = face_reports_half_kerns(&raleway());
        assert_eq!(
            raleway_says,
            Some(false),
            "Raleway kerns through GPOS and reports whole kerns"
        );

        let amstelvar = {
            let bytes = std::fs::read(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/assets/fonts/AmstelvarAlpha-VF.ttf"
            ))
            .expect("bundled Amstelvar is readable");
            let typeface = FontMgr::new()
                .new_from_data(Data::new_copy(&bytes), None)
                .expect("Amstelvar parses");
            skia_safe::Font::from_typeface(typeface, 480.0)
        };
        assert_eq!(
            face_reports_half_kerns(&amstelvar),
            None,
            "a face with no kerning has nothing to decide"
        );

        #[cfg(target_os = "macos")]
        {
            let helvetica = FontMgr::new()
                .match_family_style("Helvetica", skia_safe::FontStyle::normal())
                .map(|tf| skia_safe::Font::from_typeface(tf, 480.0))
                .expect("Helvetica resolves on macOS");
            assert_eq!(
                face_reports_half_kerns(&helvetica),
                Some(true),
                "Helvetica kerns through the legacy table and misreports"
            );
        }
    }

    /// The detector, on a real face, and it asserts the *defect*.
    ///
    /// A failure here is good news: Skia has changed, and the reconstruction
    /// should come out. The thing to check first is whether `outline_text`
    /// agrees with a draw without any help.
    ///
    /// macOS only, because the fault needs a face that kerns through the
    /// legacy `kern` table and no bundled font does. Helvetica and Times both
    /// carry `kern` with no GPOS and both show it; Arial carries both and
    /// does not. Elsewhere this cannot be asserted, and the two tests above
    /// cover the arithmetic on every platform.
    #[test]
    #[cfg(target_os = "macos")]
    fn skia_still_halves_a_legacy_kern() {
        // Scoped here rather than at the module: this is the only test that
        // lays out a paragraph, and it does not compile on other platforms.
        use skia_safe::textlayout::{
            FontCollection, ParagraphBuilder, ParagraphStyle, TextStyle,
        };

        let mut fonts = FontCollection::new();
        fonts.set_default_font_manager(FontMgr::new(), None);
        let mut style = TextStyle::new();
        style.set_font_families(&["Helvetica"]);
        style.set_font_size(480.0);
        let mut builder = ParagraphBuilder::new(&ParagraphStyle::new(), &fonts);
        builder.push_style(&style);
        builder.add_text("To");
        let mut paragraph = builder.build();
        paragraph.layout(f32::INFINITY);

        // Recorded rather than asserted here, and the `expect` below is out
        // of the closure for the same reason. A panic inside `extended_visit`
        // crosses Skia's C++ visitor trampoline, which cannot unwind: it
        // aborts the whole test binary with `SIGABRT` and reports nothing
        // about any other test. That is the failure this test exists to
        // produce, so producing it usefully is the point.
        let mut seen: Option<(f32, Option<f32>)> = None;
        paragraph.extended_visit(|_line, visit| {
            if let Some(info) = visit {
                let glyphs = info.glyphs();
                if glyphs.len() != 2 {
                    return;
                }
                let mut widths = vec![0.0; 2];
                info.font().get_widths(glyphs, &mut widths);
                let kern = info.advance().width - widths.iter().sum::<f32>();
                let shift = painted_positions(
                    info.font(),
                    glyphs,
                    info.positions(),
                    info.advance().width,
                )
                .map(|painted| info.positions()[1].x - painted[1].x);
                seen = Some((kern, shift));
            }
        });

        let (kern, shift) =
            seen.expect("the paragraph produced a two-glyph run");
        // Taken from the run rather than pinned: a face that does not kern
        // this pair makes the assertion below vacuous, so it fails here as a
        // broken fixture instead of as news about Skia.
        assert!(kern < -1.0, "fixture: this face kerns T/o, got {kern}");
        let shift = shift.expect("the guard accepts a legacy-kerned Latin run");
        assert!(
            (shift - -kern / 2.0).abs() < 0.5,
            "reported sits half a kern right of painted: {shift} against {}",
            -kern / 2.0
        );
    }
}
