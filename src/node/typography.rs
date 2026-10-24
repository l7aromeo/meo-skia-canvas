#![allow(dead_code)]
#![allow(non_snake_case)]
use crate::{
    context::State,
    css::{FontUnits, is_font_relative, parse_length},
    font::{slant_for_matching, variation_axes},
    font_library::{FontEpoch, FontLibrary, base_face},
    text::{
        TextAlign as CrateTextAlign, TextBaseline as CrateTextBaseline,
        TextMetricsLine, TextMetricsRun,
    },
    utils::*,
};
use neon::prelude::*;
use skia_safe::{
    Color, Font, FontMetrics, FourByteTag, GlyphId, Matrix, Paint,
    Path as SkPath, PathBuilder as SkPathBuilder, Point, Rect, Typeface,
    font_style::{FontStyle, Slant, Weight, Width},
    textlayout::{
        Decoration, FontCollection, Paragraph, ParagraphBuilder,
        ParagraphStyle, RectHeightStyle, RectWidthStyle, TextAlign,
        TextDirection, TextStyle, TypefaceFontProvider,
    },
    typeface::TypefaceId,
};
use std::{
    borrow::Cow,
    cell::{Cell, RefCell},
    collections::{HashMap, VecDeque},
    fmt,
    iter::zip,
    ops::Range,
    rc::Rc,
};

thread_local! {
    /// [`face_reports_half_kerns`]'s verdicts, by the face's [`base_face`].
    static KERN_VERDICTS: RefCell<HashMap<TypefaceId, Option<bool>>> =
        RefCell::new(HashMap::new());
    /// [`family_of`]'s answers, by the face's [`base_face`].
    static FAMILIES: RefCell<HashMap<TypefaceId, String>> =
        RefCell::new(HashMap::new());
}

/// How many glyphs' outline bounds [`glyph_ink`] remembers.
///
/// Each entry is a key and a rectangle, about sixty bytes, so the bound is a
/// quarter of a megabyte -- several faces' worth of the glyphs a page uses at
/// a handful of sizes each.
const GLYPH_INK_CACHE: usize = 4096;

/// What a glyph's outline depends on: the face, the size, the horizontal
/// scale and skew, whether it is emboldened, the hinting, and the glyph.
type GlyphKey = (TypefaceId, u32, u32, u32, bool, i32, GlyphId);

/// [`glyph_ink`]'s answers, each with the insertion it was made at, and the
/// count of insertions.
type GlyphInkCache = (HashMap<GlyphKey, (Option<Rect>, u64)>, u64);

thread_local! {
    static GLYPH_INK: RefCell<GlyphInkCache> =
        RefCell::new((HashMap::with_capacity(GLYPH_INK_CACHE), 0));
}

/// The tight bounds of `glyph`'s outline in `font`, or `None` for a glyph
/// with no outline -- a space, a bitmap or a colour emoji -- or an empty
/// one. Remembered.
///
/// The outline is what an ink box is joined from, and making it was two
/// thirds of working out a run's ink with every cache in Skia already warm:
/// `SkFont::getPath` builds a strike description and copies the path at the
/// font's size for every glyph, and `ComputeTightBounds` then walks it. A
/// glyph's outline depends on nothing but what the key holds, so the
/// second time a glyph is measured at a size it is a lookup.
///
/// When the map is full the older half goes, by insertion, so that finding
/// what to drop is paid once per two thousand glyphs rather than on each.
fn glyph_ink(font: &Font, glyph: GlyphId) -> Option<Rect> {
    let key = (
        font.typeface().unique_id(),
        font.size().to_bits(),
        font.scale_x().to_bits(),
        font.skew_x().to_bits(),
        font.is_embolden(),
        font.hinting() as i32,
        glyph,
    );
    if let Some(found) = GLYPH_INK
        .with(|cache| cache.borrow().0.get(&key).map(|(bounds, _)| *bounds))
    {
        return found;
    }
    let bounds = font
        .get_path(glyph)
        .map(|outline| outline.compute_tight_bounds())
        .filter(|outline| !outline.is_empty());
    GLYPH_INK.with(|cache| {
        let (entries, made) = &mut *cache.borrow_mut();
        if entries.len() >= GLYPH_INK_CACHE {
            let cutoff = *made - (GLYPH_INK_CACHE / 2) as u64;
            entries.retain(|_, (_, at)| *at > cutoff);
        }
        *made += 1;
        entries.insert(key, (bounds, *made));
    });
    bounds
}

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
    let typeface = font.typeface();
    // The face's, not the instance's: which kerning path the shaper takes
    // is a property of the font's tables, which every instance shares.
    let id = base_face(typeface.unique_id());
    if let Some(known) = KERN_VERDICTS.with(|c| c.borrow().get(&id).copied()) {
        return known;
    }
    let verdict = classify_face(font, typeface);
    KERN_VERDICTS.with(|c| {
        c.borrow_mut().insert(id, verdict);
    });
    verdict
}

/// The family `typeface` reports, remembered per face.
///
/// Per face rather than per instance, as [`face_reports_half_kerns`] is:
/// every instance of a variable font reports its family, and CoreText builds
/// the font's name table again for each new instance it is asked about.
fn family_of(typeface: &Typeface) -> String {
    let id = base_face(typeface.unique_id());
    FAMILIES.with(|families| {
        families
            .borrow_mut()
            .entry(id)
            .or_insert_with(|| typeface.family_name())
            .clone()
    })
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
    /// The style of the face the style matched first, where one was.
    ///
    /// Decides the style the paragraph is first built with, by
    /// `synthesis_style`, which is the whole of it when every run is in
    /// that face. A run another face draws is restyled for its own face in
    /// `layout`.
    matched_style: Option<FontStyle>,
    /// Borrowed from a [`TypesetInputs`] laying a run out again, which keeps
    /// its own; owned otherwise.
    char_style: Cow<'a, TextStyle>,
    graf_style: Cow<'a, ParagraphStyle>,
    text_decoration: Cow<'a, DecorationStyle>,
    /// The decoration's thickness in pixels, where one was set -- see
    /// [`decoration_thickness`].
    decoration_thickness: Option<f32>,
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

/// How far below the weight a style asks for SkParagraph needs a face to be
/// before it fakes a bold: `OneLineShaper` fakes one where the weight is
/// `kSemiBold_Weight` or more and `wantedWeight - fontStyle().weight() >=
/// 200`, with the 200 written as a literal.
const SKIA_FAKE_BOLD_GAP: i32 = 200;

/// The style to lay out a run in once its face is known, so that
/// SkParagraph synthesises a bold or an italic exactly where Chrome does.
///
/// SkParagraph fakes a bold when the style asks for 600 or more and the
/// face is [`SKIA_FAKE_BOLD_GAP`] or more lighter, and an italic -- a skew
/// of a quarter, which is Chrome's to the pixel: the stem of an `I` slopes
/// -0.2504 in Chrome 152 at 40, 100 and 200px -- when the style asks for
/// italic and the face is not. The style handed to it is chosen so those
/// two rules reach Chrome's answer for this face:
///
/// - **A bold is faked where 600 or more is asked for and the face is under
///   600**, which is Chrome's rule, measured on single faces of Montserrat: its
///   200 and 400 faces are emboldened from 600, by the same amount at 900, and
///   not at 500, and its 700 face never is. A face under 600 is handed the
///   weight asked for, or 200 above its own where that is more; any other is
///   handed its own weight, which fakes nothing. Neither moves the match: a
///   face under 600 matched for a weight of 600 or more is the heaviest the
///   family has below it, so a heavier request finds it again. A variable face
///   is instanced at the weight asked for, so it is under 600 only where its
///   axis ends below 600 -- weight 900 on Oswald, whose `wght` stops at 700,
///   paints exactly as 700.
/// - **The slant is italic where the request is slanted and the face is
///   upright**, so the run is skewed; a face that is itself italic or oblique
///   keeps its own slant and is not skewed a second time.
fn synthesis_style(requested: FontStyle, face: FontStyle) -> FontStyle {
    let (asked, has) = (*requested.weight(), *face.weight());
    let bold = *Weight::SEMI_BOLD;
    let weight = match asked >= bold && has < bold {
        true => asked.max(has + SKIA_FAKE_BOLD_GAP),
        false => has,
    };
    let slant = match (requested.slant(), face.slant()) {
        (Slant::Upright, face) => face,
        (_, Slant::Upright) => Slant::Italic,
        (_, face) => face,
    };
    FontStyle::new(Weight::from(weight), face.width(), slant)
}

/// The ranges of `paragraph` whose face wants a different style from
/// `base`, each with the style `synthesis_style` gives it, or none where
/// every run is in a face `base` already suits.
fn runs_to_restyle(
    paragraph: &mut Paragraph,
    requested: FontStyle,
    base: FontStyle,
) -> Vec<(Range<usize>, FontStyle)> {
    let mut restyled = Vec::new();
    paragraph.extended_visit(|_line, visit| {
        let Some(info) = visit else { return };
        let face = info.font().typeface();
        let style = synthesis_style(requested, face.font_style());
        let starts = info.utf8_starts();
        if style != base
            && let (Some(first), Some(last)) = (starts.first(), starts.last())
        {
            restyled.push((*first as usize..*last as usize, style));
        }
    });
    restyled
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

/// The typography a run is laid out with, as [`State::typography`] gives it.
type Typography = (TextStyle, ParagraphStyle, DecorationStyle, Baseline, bool);

/// The decoration's thickness in pixels, where one was set, resolved
/// against `state`: a thickness in `ex` or `ch` needs the face the state
/// resolves to, which a run laid out again later no longer has to hand.
fn decoration_thickness(state: &State) -> Option<f32> {
    state
        .text_decoration
        .size
        .as_ref()
        .map(|size| size.in_px(&state.font_units()))
}

/// [`Typography`], owned or borrowed from where it is kept.
type TypographyRef<'a> = (
    Cow<'a, TextStyle>,
    Cow<'a, ParagraphStyle>,
    Cow<'a, DecorationStyle>,
    Baseline,
    bool,
);

impl<'a> Typesetter<'a> {
    pub fn new(state: &State, text: &'a str, width: Option<f32>) -> Self {
        Self::from_parts(
            state.typography(),
            &state.variations,
            decoration_thickness(state),
            text,
            width,
        )
    }

    /// A typesetter for `text` in `typography` and `variations`, rather than
    /// in a context's state as it stands, with its faces from the fonts
    /// registered now.
    fn from_parts(
        typography: Typography,
        variations: &[(FourByteTag, f32)],
        decoration_thickness: Option<f32>,
        text: &'a str,
        width: Option<f32>,
    ) -> Self {
        // The matched style comes back with the collection, from the one
        // search that found it. `layout` used to search again for it, on
        // every call, and it is the same search.
        let faces = FontLibrary::with_shared(|lib| {
            lib.set_hinting(typography.1.hinting_is_on())
                .fonts_for_style(&typography.0, variations)
        });
        let (char_style, graf_style, text_decoration, baseline, text_wrap) =
            typography;
        Self::from_resolved(
            (
                Cow::Owned(char_style),
                Cow::Owned(graf_style),
                Cow::Owned(text_decoration),
                baseline,
                text_wrap,
            ),
            decoration_thickness,
            faces,
            text,
            width,
        )
    }

    /// A typesetter laying `text` out in `faces`, the collection a search
    /// for `typography`'s style found and the style it matched first.
    fn from_resolved(
        typography: TypographyRef<'a>,
        decoration_thickness: Option<f32>,
        (typefaces, matched_style): (FontCollection, Option<FontStyle>),
        text: &'a str,
        width: Option<f32>,
    ) -> Self {
        let (char_style, graf_style, text_decoration, baseline, text_wrap) =
            typography;
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
            decoration_thickness,
            text_wrap,
        }
    }

    /// Lays the run out, and works out how far it has to be squeezed.
    pub fn layout(&self, paint: &Paint) -> TextLayout {
        let mut char_style = TextStyle::clone(&self.char_style);
        char_style.set_foreground_paint(paint);
        char_style.set_decoration(&self.text_decoration.for_layout(
            &char_style,
            paint.color(),
            self.decoration_thickness,
        ));

        // Let SkParagraph synthesise what Chrome synthesises and nothing
        // else -- see `synthesis_style`. The match was found once, when the
        // collection was chosen, rather than by searching it again here: the
        // search needed a `Vec` of the family names, a clone of the
        // collection and a lookup, 173 nanoseconds of every layout on this
        // machine, to arrive at what `fonts_for_style` had already had in
        // hand.
        let requested = self.char_style.font_style();
        if let Some(matched) = self.matched_style {
            char_style.set_font_style(synthesis_style(requested, matched));
        }

        // That style is the first face's. A run another face draws -- a
        // glyph the first family lacks, falling back to the next -- is laid
        // out again in the style for its own face, since SkParagraph applies
        // one style's weight to whatever face each run finds: at
        // `900 48px Montserrat, Raleway`, Chrome draws the Latin as plain
        // Montserrat 700 and the Cyrillic, which Montserrat lacks, as
        // Raleway's 900 instance.
        let mut first = self.paragraph_with(&char_style, &[]);
        let restyled =
            runs_to_restyle(&mut first, requested, char_style.font_style());
        let paragraph = match restyled.is_empty() {
            true => first,
            false => self.paragraph_with(&char_style, &restyled),
        };

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

    /// The run laid out in `char_style`, with each range in `restyled` in
    /// the font style beside it, at the layout width.
    fn paragraph_with(
        &self,
        char_style: &TextStyle,
        restyled: &[(Range<usize>, FontStyle)],
    ) -> Paragraph {
        let mut paragraph_builder =
            ParagraphBuilder::new(&self.graf_style, &self.typefaces);
        let mut unkerned = char_style.clone();
        unkerned.add_font_feature("kern", 0);

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
        //
        // A restyled range splits a piece further, at a boundary where the
        // face already changes, so it breaks no kern pair that could form.
        let text: &str = &self.text;
        let mut at = 0;
        while at < text.len() {
            let separating = text[at..].starts_with(is_word_separator);
            let mut end = text[at..]
                .find(|ch| is_word_separator(ch) != separating)
                .map_or(text.len(), |offset| at + offset);
            let style = restyled
                .iter()
                .find(|(range, _)| range.contains(&at))
                .map(|(range, style)| {
                    end = end.min(range.end);
                    *style
                });
            if let Some(next) = restyled
                .iter()
                .map(|(range, _)| range.start)
                .filter(|start| *start > at)
                .min()
            {
                end = end.min(next);
            }
            let mut piece_style = match separating {
                true => unkerned.clone(),
                false => char_style.clone(),
            };
            if let Some(style) = style {
                piece_style.set_font_style(style);
            }
            paragraph_builder
                .push_style(&piece_style)
                .add_text(&text[at..end])
                .pop();
            at = end;
        }

        let mut paragraph = paragraph_builder.build();
        paragraph.layout(self.width);
        paragraph
    }

    /// Measurements of the run, ink and all: [`measure`](Self::measure)
    /// and then [`Measured::ink`]. What the Rust API's `measure_text`
    /// reports.
    pub fn extents(&self) -> TextExtents {
        let (mut extents, mut measured) = self.measure();
        let (ink, line_details) = measured.ink();
        extents.ink = ink;
        extents.line_details = line_details;
        extents
    }

    /// Measurements of the run except its ink, and what the ink is worked
    /// out from.
    ///
    /// Apart because the ink is most of the cost of a measurement and most
    /// callers never read it: it outlines every glyph, and on a typeface
    /// not drawn before -- each size of a face with an `opsz` axis is one --
    /// those outlines are made from nothing. JavaScript's `TextMetrics`
    /// works it out when an `actualBoundingBox` value or `lines` is first
    /// read. The returned extents carry an empty ink box and no lines.
    pub fn measure(&self) -> (TextExtents, Measured) {
        let TextLayout {
            paragraph,
            offset: origin,
            condensation,
        } = self.layout(&Paint::default());

        // Baseline offsets, relative to whatever `textBaseline` selected.
        let shift = self.char_style.baseline_shift();
        let hang = Baseline::Hanging.get_offset(&self.char_style) - shift;
        let norm = Baseline::Alphabetic.get_offset(&self.char_style) - shift;
        let ideo = Baseline::Ideographic.get_offset(&self.char_style) - shift;
        let half_leading = self.graf_style.strut_style().leading().max(0.0)
            * self.char_style.font_size()
            / 2.0;

        // The laid-out box of each line horizontally, which is what the
        // Canvas API's `width` measures, so trailing whitespace counts.
        //
        // The letter-space Skia puts after the last glyph is kept. CSS adds
        // `letter-spacing` after every character including the final one, so
        // an `n`-character run is `n` spaces wide, and Chrome measures it
        // that way. This used to subtract a whole space from `right`, which
        // gave `n - 1`: at 40px Helvetica with 10px spacing, `"a"` measured
        // 22.25 -- identical to no spacing at all -- where Chrome gives
        // 32.25, and `"abcd"` measured 116.74 against 126.74.
        //
        // A union of the horizontal extents alone, which keeps the width
        // independent of the ink. Joining whole rects whose height came from
        // each line's ink would let Skia's `join`, which skips an empty rect
        // unless it comes first, decide what counts -- and make the width
        // wait on the ink. Measured on wrapped text with lines of nothing but
        // spaces, at the start, the middle and the end, the two agree.
        let utf16 = Utf16Index::new(&self.text);
        let mut left = f32::INFINITY;
        let mut right = f32::NEG_INFINITY;
        let mut char_ranges = Vec::with_capacity(paragraph.line_number());
        for line in 0..paragraph.line_number() {
            let text_range =
                paragraph.get_actual_text_range(line, !self.text_wrap);
            let char_range = utf16.range(&text_range);
            for rect in paragraph.get_rects_for_range(
                char_range.clone(),
                RectHeightStyle::Tight,
                RectWidthStyle::Tight,
            ) {
                let rect = rect.rect.with_offset(origin);
                left = left.min(rect.left);
                right = right.max(rect.right);
            }
            char_ranges.push(char_range);
        }
        // Condensation is a horizontal scale about the anchor, and these
        // coordinates are already relative to it -- `origin` carries the
        // alignment offset -- so every horizontal quantity is multiplied by
        // it and no vertical one is.
        let width = match left < right {
            true => (right - left) * condensation,
            false => 0.0,
        };

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

        let extents = TextExtents {
            width,
            ink: Rect::new_empty(),
            line_details: vec![],
            font_ascent,
            font_descent,
            alphabetic: norm,
            hanging: hang,
            ideographic: ideo,
            height: paragraph.height(),
            lines: paragraph.line_number(),
        };
        let measured = Measured {
            paragraph,
            origin,
            condensation,
            half_leading,
            hang,
            norm,
            ideo,
            char_ranges,
        };
        (extents, measured)
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
        // was taken from.
        //
        // UPSTREAM: skia-safe 0.153.3 -- #131 -- worked around
        // Re-check: cargo test skia_still_halves_a_legacy_kern, which draws
        // a kerned run and compares what Skia reports against where it
        // paints. It goes red when the defect is fixed rather than needing a
        // workaround removed first: `painted_positions` refuses positions
        // that are already painted positions, so a corrected Skia makes the
        // test's own `expect` fail. Nothing is filed with rust-skia or Skia:
        // #131 records the attribution to Skia as not yet established, so
        // this may turn out to be ours.
        //
        // `painted_positions` documents the recovery and the guard on it.
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
    /// The angle of an `oblique` other than zero and the default 14deg, for
    /// the drawing to decide on, or `None`.
    pub oblique_degrees: Option<f32>,
}

impl FontSpec {
    pub fn with_width(&self, width: Width) -> Self {
        Self {
            width,
            ..self.clone()
        }
    }

    pub fn style(&self) -> FontStyle {
        FontStyle::new(self.weight, self.width, slant_for_matching(self.slant))
    }
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
    if let Some(params) = variation_axes(font) {
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

pub fn to_text_align(mode_name: &str) -> Option<CrateTextAlign> {
    let mode = match mode_name.to_lowercase().as_str() {
        "left" => CrateTextAlign::Left,
        "right" => CrateTextAlign::Right,
        "center" => CrateTextAlign::Center,
        "justify" => CrateTextAlign::Justify,
        "start" => CrateTextAlign::Start,
        "end" => CrateTextAlign::End,
        _ => return None,
    };
    Some(mode)
}

pub fn from_text_align(mode: CrateTextAlign) -> String {
    match mode {
        CrateTextAlign::Left => "left",
        CrateTextAlign::Right => "right",
        CrateTextAlign::Center => "center",
        CrateTextAlign::Justify => "justify",
        CrateTextAlign::Start => "start",
        CrateTextAlign::End => "end",
    }
    .to_string()
}

impl From<CrateTextBaseline> for Baseline {
    /// The crate's baseline as the internal one that carries `get_offset`.
    ///
    /// One mapping rather than two: `Context2D::set_text_baseline` used to
    /// carry its own copy of this match, and a parser that produced `Baseline`
    /// directly meant the crate's enum and the JavaScript vocabulary never met.
    fn from(baseline: CrateTextBaseline) -> Self {
        match baseline {
            CrateTextBaseline::Top => Self::Top,
            CrateTextBaseline::Hanging => Self::Hanging,
            CrateTextBaseline::Middle => Self::Middle,
            CrateTextBaseline::Alphabetic => Self::Alphabetic,
            CrateTextBaseline::Ideographic => Self::Ideographic,
            CrateTextBaseline::Bottom => Self::Bottom,
        }
    }
}

impl From<Baseline> for CrateTextBaseline {
    /// The reverse, for reading the state back out to JavaScript.
    fn from(baseline: Baseline) -> Self {
        match baseline {
            Baseline::Top => Self::Top,
            Baseline::Hanging => Self::Hanging,
            Baseline::Middle => Self::Middle,
            Baseline::Alphabetic => Self::Alphabetic,
            Baseline::Ideographic => Self::Ideographic,
            Baseline::Bottom => Self::Bottom,
        }
    }
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

pub fn to_text_baseline(mode_name: &str) -> Option<CrateTextBaseline> {
    let mode = match mode_name.to_lowercase().as_str() {
        "top" => CrateTextBaseline::Top,
        "hanging" => CrateTextBaseline::Hanging,
        "middle" => CrateTextBaseline::Middle,
        "alphabetic" => CrateTextBaseline::Alphabetic,
        "ideographic" => CrateTextBaseline::Ideographic,
        "bottom" => CrateTextBaseline::Bottom,
        _ => return None,
    };
    Some(mode)
}

pub fn from_text_baseline(mode: CrateTextBaseline) -> String {
    match mode {
        CrateTextBaseline::Top => "top",
        CrateTextBaseline::Hanging => "hanging",
        CrateTextBaseline::Middle => "middle",
        CrateTextBaseline::Alphabetic => "alphabetic",
        CrateTextBaseline::Ideographic => "ideographic",
        CrateTextBaseline::Bottom => "bottom",
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
    /// The decoration as Skia takes it, for text in `style`, with
    /// `thickness` the resolved [`size`](Self::size) in pixels.
    pub fn for_layout(
        &self,
        style: &TextStyle,
        text_color: Color,
        thickness: Option<f32>,
    ) -> Decoration {
        // Skia takes a multiple of the font's own underline thickness.
        let font_thickness =
            style.font_metrics().underline_thickness().unwrap_or(1.0);
        let thickness_multiplier =
            thickness.map_or(1.0, |px| px / font_thickness);
        let color = self.color.unwrap_or(text_color);
        Decoration {
            thickness_multiplier,
            color,
            ..self.decoration
        }
    }
}

//
// Font-relative lengths (for text spacing & decoration thickness)
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
    /// A spacing from a CSS length, as `letterSpacing` and `wordSpacing`
    /// take one, or `None` for one Chrome ignores.
    ///
    /// Carries the unit through rather than resolving it, so a font-relative
    /// length is still relative after it is set: [`in_px`](Self::in_px)
    /// resolves it at the moment the text is laid out.
    pub fn from_css(css: &str) -> Option<Self> {
        // Whitespace around the length is refused too: Chrome ignores
        // `ctx.letterSpacing = " 5px "`.
        if css.trim() != css {
            return None;
        }
        let length = parse_length(css)?;
        // So is a bare zero, which CSS lets a length be and `blur(0)` takes:
        // Chrome ignores `ctx.letterSpacing = "0"`.
        if length.unit.is_empty() {
            return None;
        }
        Self::parse(length.value, length.unit, length.pixels)
    }

    pub fn parse(raw_size: f32, unit: String, px_size: f32) -> Option<Self> {
        let main_size = match is_font_relative(&unit) {
            true => raw_size,
            false => px_size,
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

    /// The spacing in pixels for the font `units` describes. A
    /// font-relative unit is resolved here, at layout, so it follows the
    /// font rather than staying at the one it was set with.
    pub fn in_px(&self, units: &FontUnits) -> f32 {
        let relative = is_font_relative(&self.unit)
            .then(|| units.px_per(&self.unit))
            .flatten();
        relative.map_or(self.px_size, |per| self.raw_size * per)
    }
}

impl fmt::Display for Spacing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", self.raw_size, self.unit)
    }
}

/// The spacing a `letterSpacing` or `wordSpacing` assignment names, or
/// `None` for a value Chrome ignores -- which includes anything that is not
/// a string, since `ctx.letterSpacing = 5` reads `"5"` and is refused.
pub fn opt_spacing_arg(
    cx: &mut FunctionContext,
    idx: usize,
) -> NeonResult<Option<Spacing>> {
    let value = cx.argument::<JsValue>(idx)?;
    Ok(value
        .downcast::<JsString, _>(cx)
        .ok()
        .and_then(|css| Spacing::from_css(&css.value(cx))))
}

/// A run laid out and measured, with what its ink is worked out from.
///
/// Kept among the recent layouts for a JavaScript `TextMetrics` whose ink is
/// not yet read -- see [`PendingInk`] -- and spent by the read:
/// [`Measured::ink`] walks the paragraph once.
pub struct Measured {
    paragraph: Paragraph,
    /// Where the paragraph is painted, relative to the anchor.
    origin: Point,
    /// The horizontal factor the run is squeezed by. See [`TextLayout`].
    condensation: f32,
    /// Half the strut's leading, which the line baselines are moved by.
    half_leading: f32,
    /// The hanging, alphabetic and ideographic baselines' offsets from the
    /// selected one.
    hang: f32,
    norm: f32,
    ideo: f32,
    /// Each line's text, in UTF-16 code units.
    char_ranges: Vec<Range<usize>>,
}

impl Measured {
    /// The ink box of the run, and each line with the single-font runs in
    /// it, as [`TextExtents::ink`] and [`TextExtents::line_details`] hold
    /// them.
    pub fn ink(&mut self) -> (Rect, Vec<TextMetricsLine>) {
        let Self {
            paragraph,
            origin,
            condensation,
            half_leading,
            hang,
            norm,
            ideo,
            char_ranges,
        } = self;
        let (origin, condensation, half_leading) =
            (*origin, *condensation, *half_leading);
        let (hang, norm, ideo) = (*hang, *norm, *ideo);

        // Each run's glyph bounds, with the family and font metrics
        // alongside, which is what makes the per-run detail below reportable
        // rather than a second walk.
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
                        let ink =
                            glyph_ink(info.font(), *glyph).unwrap_or(*rect);
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
                    family: family_of(&info.font().typeface()),
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

        // The glyphs' own box, joined line by line. See `TextExtents::ink`.
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
        let mut taken = 0;

        for (line, char_range) in char_ranges.iter().enumerate() {
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
            let char_range = char_range.clone();

            if let Some(line_metrics) = paragraph.get_line_metrics_at(line) {
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

            ink_bounds = Some(match ink_bounds {
                Some(so_far) => Rect::join2(so_far, text_bounds),
                None => text_bounds,
            });
        }
        let ink_bounds = ink_bounds.unwrap_or(Rect::new_empty());

        // The ink box takes the condensation as the width does: a
        // horizontal scale about the anchor.
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
        (ink_bounds, line_details)
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
    Field::plain("fontBoundingBoxAscent", |m| m.font_ascent as f64),
    Field::plain("fontBoundingBoxDescent", |m| m.font_descent as f64),
    Field::plain("emHeightAscent", |m| m.font_ascent as f64),
    Field::plain("emHeightDescent", |m| m.font_descent as f64),
    Field::plain("hangingBaseline", |m| m.hanging as f64),
    Field::plain("alphabeticBaseline", |m| m.alphabetic as f64),
    Field::plain("ideographicBaseline", |m| m.ideographic as f64),
    Field::plain("height", |m| m.height as f64),
];

/// The `TextMetrics` values worked out from the ink, which `measureText`
/// leaves until one of them is read: the ink box, as
/// [`Measured::ink`] returns it.
///
/// Negated as [`METRIC_FIELDS`] explains.
const INK_FIELDS: &[Field<Rect>] = &[
    Field::plain("actualBoundingBoxLeft", |ink| (0.0 - ink.left) as f64),
    Field::plain("actualBoundingBoxRight", |ink| ink.right as f64),
    Field::plain("actualBoundingBoxAscent", |ink| (0.0 - ink.top) as f64),
    Field::plain("actualBoundingBoxDescent", |ink| ink.bottom as f64),
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

/// The measurement `measureText` reports before anything reads its ink, as
/// numbers, in [`METRIC_FIELDS`]'s order.
///
/// A buffer rather than the object itself. Building that object here meant
/// a property write per value, and each one is a call across the binding:
/// 4.6 microseconds of a 9.4-microsecond `measureText`, against 3.5 for the
/// typesetting it reports. One buffer is one call and a copy, and the
/// object is assembled in JavaScript, where a write is a few nanoseconds.
pub fn js_text_metrics<'a, C: Context<'a>>(
    cx: &mut C,
    extents: &TextExtents,
) -> JsResult<'a, JsFloat64Array> {
    let mut out = Vec::with_capacity(packed_len(METRIC_FIELDS));
    let mut names = vec![];
    pack(METRIC_FIELDS, extents, &mut out, &mut names);
    JsFloat64Array::from_slice(cx, &out)
}

/// The values worked out from the ink, as numbers and names: [`INK_FIELDS`]
/// for the ink box, then the line count and each line in [`LINE_FIELDS`]'
/// order, each followed by its run count and its runs in [`RUN_FIELDS`]'.
/// The runs' families travel in the array beside the numbers.
pub fn js_text_ink<'a, C: Context<'a>>(
    cx: &mut C,
    ink: &Rect,
    line_details: &[TextMetricsLine],
) -> JsResult<'a, JsArray> {
    let runs: usize = line_details.iter().map(|line| line.runs.len()).sum();
    let mut out = Vec::with_capacity(
        packed_len(INK_FIELDS)
            + 1
            + line_details.len() * (packed_len(LINE_FIELDS) + 1)
            + runs * packed_len(RUN_FIELDS),
    );
    let mut names = Vec::with_capacity(runs);

    pack(INK_FIELDS, ink, &mut out, &mut names);
    out.push(line_details.len() as f64);
    for line in line_details {
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

/// How many measurements keep the paragraph their ink is worked out from.
///
/// The rest keep only what they were laid out from, and lay it out again if
/// their ink is read. A paragraph cannot be left to the `TextMetrics`
/// holding it: Node frees what a JavaScript object holds only when the
/// event loop turns, so a loop that measures and never reads the ink kept
/// every paragraph it made, and each one the typeface it was laid out in.
/// Eleven thousand measurements of Amstelvar at sizes not seen before grew
/// resident memory to 701 MB that way, against 159 when the ink was worked
/// out with the rest.
///
/// With the bound the same loop holds 178 MB: what each measurement keeps
/// until Node frees it is what it was laid out from, about 1.7 KB.
///
/// Sixty-four covers a caller that reads the ink straight after measuring,
/// which finds its paragraph every time, and one that measures a batch and
/// reads back a few dozen. The paragraphs it keeps measured up to about
/// 47 KB each, with the typeface instance they hold.
const PENDING_LAYOUTS: usize = 64;

thread_local! {
    /// The most recent measurements' paragraphs, oldest first, by the id
    /// their [`PendingInk`] carries.
    static RECENT_LAYOUTS: RefCell<VecDeque<(u64, Measured)>> =
        RefCell::new(VecDeque::with_capacity(PENDING_LAYOUTS));
}

/// What a run is laid out from: the typography a context had when it
/// measured, the text and the width. Owned, so it outlasts the context's
/// next change.
pub struct TypesetInputs {
    typography: Typography,
    variations: Vec<(FourByteTag, f32)>,
    /// Resolved when the run was measured -- see [`decoration_thickness`].
    decoration_thickness: Option<f32>,
    text: String,
    width: Option<f32>,
    /// The fonts registered when the run was measured, so that laying it
    /// out again finds the faces it was measured in.
    fonts: Rc<FontEpoch>,
}

impl TypesetInputs {
    pub fn new(state: &State, text: &str, width: Option<f32>) -> Self {
        Self {
            typography: state.typography(),
            variations: state.variations.clone(),
            decoration_thickness: decoration_thickness(state),
            text: text.to_string(),
            width,
            fonts: FontLibrary::with_shared(|lib| lib.epoch()),
        }
    }

    /// A typesetter for what these inputs describe, in the faces the fonts
    /// registered at the time resolve to.
    pub fn typesetter(&self) -> Typesetter<'_> {
        let faces = FontLibrary::fonts_for_style_in(
            &self.fonts,
            self.typography.1.hinting_is_on(),
            &self.typography.0,
            &self.variations,
        );
        let (char_style, graf_style, text_decoration, baseline, text_wrap) =
            &self.typography;
        Typesetter::from_resolved(
            (
                Cow::Borrowed(char_style),
                Cow::Borrowed(graf_style),
                Cow::Borrowed(text_decoration),
                *baseline,
                *text_wrap,
            ),
            self.decoration_thickness,
            faces,
            &self.text,
            self.width,
        )
    }
}

/// A measurement's ink, until JavaScript reads it.
///
/// Its paragraph is kept apart, among the [`PENDING_LAYOUTS`] most recent,
/// and what it was laid out from is kept here. Spent by the first read.
pub struct PendingInk {
    id: u64,
    inputs: RefCell<Option<TypesetInputs>>,
}

impl Finalize for PendingInk {}

impl PendingInk {
    /// Holds `measured` among the recent layouts, and `inputs` for when it
    /// has left them.
    pub fn new(inputs: TypesetInputs, measured: Measured) -> Self {
        thread_local! {
            static NEXT: Cell<u64> = const { Cell::new(0) };
        }
        let id = NEXT.with(|next| {
            next.set(next.get() + 1);
            next.get()
        });
        RECENT_LAYOUTS.with(|recent| {
            let mut recent = recent.borrow_mut();
            if recent.len() == PENDING_LAYOUTS {
                recent.pop_front();
            }
            recent.push_back((id, measured));
        });
        Self {
            id,
            inputs: RefCell::new(Some(inputs)),
        }
    }

    /// The ink box and the lines, from the paragraph if it is still held
    /// and from laying the run out again if not, in the faces it was
    /// measured in. `None` once spent.
    pub fn ink(&self) -> Option<(Rect, Vec<TextMetricsLine>)> {
        let inputs = self.inputs.borrow_mut().take()?;
        let held = RECENT_LAYOUTS.with(|recent| {
            let mut recent = recent.borrow_mut();
            let at = recent.iter().position(|(id, _)| *id == self.id)?;
            recent.remove(at).map(|(_, measured)| measured)
        });
        let mut measured =
            held.unwrap_or_else(|| inputs.typesetter().measure().1);
        Some(measured.ink())
    }
}

/// What the buffer holds, for JavaScript to build its reader from.
///
/// `{ metrics: [...], ink: [...], line: [...], run: [...] }`, each a list of
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
    let ink = listed(&mut cx, INK_FIELDS)?;
    table.set(&mut cx, "ink", ink)?;
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
    use super::{
        FAMILIES, KERN_VERDICTS, face_reports_half_kerns, painted_positions,
        reconstruct_run,
    };
    use crate::{canvas::Canvas, context2d::Font, font::FontLibrary as Fonts};
    use skia_safe::{Data, FontMgr, Point};

    #[test]
    fn a_measurement_whose_paragraph_was_let_go_lays_its_ink_out_again() {
        // Only the most recent measurements keep their paragraphs; an older
        // one keeps what it was laid out from, and its ink comes out the
        // same either way.
        use super::{PENDING_LAYOUTS, RECENT_LAYOUTS};

        const FAMILY: &str = "AmstelvarPendingInk";
        let fonts = Fonts::new();
        fonts
            .register_font_from_path(
                FAMILY,
                concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/tests/assets/fonts/AmstelvarAlpha-VF.ttf"
                ),
            )
            .expect("bundled Amstelvar registers");
        let mut canvas = Canvas::new(10.0, 10.0);
        let ctx = canvas.context();
        ctx.set_font(&Font::new(FAMILY, 24.0));
        let measure =
            || ctx.inner.measure_text_lazily("Hamburgefonstiv", None).1;

        let oldest = measure();
        let newest: Vec<_> = (0..PENDING_LAYOUTS).map(|_| measure()).collect();
        let held = |pending: &super::PendingInk| {
            RECENT_LAYOUTS.with(|recent| {
                recent.borrow().iter().any(|(id, _)| *id == pending.id)
            })
        };
        assert_eq!(
            RECENT_LAYOUTS.with(|recent| recent.borrow().len()),
            PENDING_LAYOUTS,
            "no more paragraphs are kept than the bound"
        );
        assert!(!held(&oldest), "the oldest measurement's paragraph went");
        assert!(held(&newest[PENDING_LAYOUTS - 1]));

        let (kept_ink, kept_lines) =
            newest[PENDING_LAYOUTS - 1].ink().expect("not yet read");
        let (again_ink, again_lines) = oldest.ink().expect("not yet read");
        assert_eq!(again_ink, kept_ink);
        assert_eq!(again_lines, kept_lines);
        assert!(oldest.ink().is_none(), "a measurement's ink is read once");
        assert!(
            !held(&newest[PENDING_LAYOUTS - 1]),
            "and its paragraph goes when it is"
        );
    }

    #[test]
    fn an_evicted_measurement_lays_out_again_in_the_faces_it_was_measured_in() {
        // A font registered between `measureText` and the first ink read
        // must not reach a measurement that has to be laid out again: its
        // ink would come from one font and its width from another.
        use super::{PENDING_LAYOUTS, RECENT_LAYOUTS};

        const FAMILY: &str = "PendingInkSwapped";
        const TEXT: &str = "Hamburgefonstiv";
        let path = |file: &str| {
            format!("{}/tests/assets/fonts/{file}", env!("CARGO_MANIFEST_DIR"))
        };
        let fonts = Fonts::new();
        fonts
            .register_font_from_path(FAMILY, path("AmstelvarAlpha-VF.ttf"))
            .expect("bundled Amstelvar registers");
        let mut canvas = Canvas::new(10.0, 10.0);
        let ctx = canvas.context();
        ctx.set_font(&Font::new(FAMILY, 24.0));

        let measured_then = ctx.inner.measure_text_extents(TEXT, None);
        let pending = ctx.inner.measure_text_lazily(TEXT, None).1;

        fonts
            .register_font_from_path(
                FAMILY,
                path("Oswald/Oswald-VariableFont_wght.ttf"),
            )
            .expect("bundled Oswald registers");
        let measured_now = ctx.inner.measure_text_extents(TEXT, None);
        assert_ne!(
            measured_now.ink, measured_then.ink,
            "the family has to draw differently for this to mean anything"
        );

        for _ in 0..PENDING_LAYOUTS {
            ctx.inner.measure_text_lazily(TEXT, None);
        }
        assert!(
            !RECENT_LAYOUTS.with(|recent| {
                recent.borrow().iter().any(|(id, _)| *id == pending.id)
            }),
            "the measurement's paragraph went, so its ink is laid out again"
        );
        let (ink, lines) = pending.ink().expect("not yet read");
        assert_eq!(ink, measured_then.ink);
        assert_eq!(lines, measured_then.line_details);
    }

    #[test]
    fn a_glyphs_remembered_ink_is_its_outlines_at_that_size() {
        // The bounds come back from the map the second time, and have to be
        // what the outline gives -- at the size asked for, not at the size
        // the glyph was first measured at.
        use super::{GLYPH_INK, GLYPH_INK_CACHE, glyph_ink};

        let font = raleway();
        let ids: Vec<u16> = font.str_to_glyphs_vec("Hamburgefonstiv");
        let outline = |font: &skia_safe::Font, glyph: u16| {
            font.get_path(glyph)
                .map(|path| path.compute_tight_bounds())
                .filter(|bounds| !bounds.is_empty())
        };
        let mut half = font.clone();
        half.set_size(font.size() / 2.0);
        for _ in 0..2 {
            for &glyph in &ids {
                assert_eq!(glyph_ink(&font, glyph), outline(&font, glyph));
                assert_eq!(glyph_ink(&half, glyph), outline(&half, glyph));
            }
        }
        let a = ids[0];
        assert_ne!(
            glyph_ink(&font, a),
            glyph_ink(&half, a),
            "two sizes of a glyph are two entries"
        );

        let spaces = font.str_to_glyphs_vec(" ");
        assert_eq!(glyph_ink(&font, spaces[0]), None, "a space has no ink");

        // Every glyph id of the face, at a run of sizes, is more than the
        // map holds.
        let mut sized = font.clone();
        for step in 0..(GLYPH_INK_CACHE / 64 + 4) {
            sized.set_size(10.0 + step as f32);
            for glyph in 0..64 {
                glyph_ink(&sized, glyph);
            }
        }
        assert!(
            GLYPH_INK.with(|cache| cache.borrow().0.len()) <= GLYPH_INK_CACHE,
            "the map stays at its bound"
        );
    }

    #[test]
    fn every_size_of_a_variable_face_is_probed_and_named_once() {
        // Each size of a face with an `opsz` axis is an instance of its own,
        // with an id of its own. The kerning verdict and the family belong
        // to the font, so a measurement at a size not seen before reuses
        // them rather than laying out the probe pairs again.
        const FAMILY: &str = "AmstelvarProbedOnce";
        let fonts = Fonts::new();
        fonts
            .register_font_from_path(
                FAMILY,
                concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/tests/assets/fonts/AmstelvarAlpha-VF.ttf"
                ),
            )
            .expect("bundled Amstelvar registers");
        let mut canvas = Canvas::new(10.0, 10.0);
        let ctx = canvas.context();
        for step in 0..20 {
            ctx.set_font(&Font::new(FAMILY, 12.0 + step as f32));
            ctx.measure_text("Hamburgefonstiv", None);
        }
        assert_eq!(
            KERN_VERDICTS.with(|verdicts| verdicts.borrow().len()),
            1,
            "twenty sizes of one face decide its kerning once"
        );
        assert_eq!(
            FAMILIES.with(|families| families.borrow().len()),
            1,
            "and look its family up once"
        );
    }

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
    /// the fault needs kerning from the legacy `kern` table, and no font in
    /// `tests/assets` carries one. The arithmetic is what is under
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

#[cfg(test)]
mod synthesis {
    use super::*;

    fn style(weight: i32, slant: Slant) -> FontStyle {
        FontStyle::new(Weight::from(weight), Width::NORMAL, slant)
    }

    #[test]
    fn a_slanted_request_skews_only_an_upright_face() {
        let asked = style(400, Slant::Italic);
        for (face, slant) in [
            (Slant::Upright, Slant::Italic),
            // A face with a slant of its own keeps it, so SkParagraph's
            // fake italic -- which applies to any face not reporting
            // `Italic` -- does not lean an oblique face a second time.
            (Slant::Oblique, Slant::Oblique),
            (Slant::Italic, Slant::Italic),
        ] {
            let got = synthesis_style(asked, style(400, face));
            assert_eq!(got.slant(), slant, "{face:?}");
        }
        // An upright request takes the face as it is.
        let upright = style(400, Slant::Upright);
        let got = synthesis_style(upright, style(400, Slant::Oblique));
        assert_eq!(got.slant(), Slant::Oblique);
    }

    /// Skia's rule applied to the weight handed to it gives Chrome's answer
    /// for every weight asked for and every face weight: a bold faked
    /// exactly where 600 or more is asked for and the face is under 600.
    #[test]
    fn skia_fakes_a_bold_exactly_where_chrome_does() {
        let skia_fakes = |weight: i32, face: i32| {
            weight >= *Weight::SEMI_BOLD && weight - face >= SKIA_FAKE_BOLD_GAP
        };
        let chrome_fakes = |asked: i32, face: i32| {
            asked >= *Weight::SEMI_BOLD && face < *Weight::SEMI_BOLD
        };
        for asked in (100..=1000).step_by(50) {
            for face in (100..=1000).step_by(50) {
                let handed = *synthesis_style(
                    style(asked, Slant::Upright),
                    style(face, Slant::Upright),
                )
                .weight();
                assert_eq!(
                    skia_fakes(handed, face),
                    chrome_fakes(asked, face),
                    "asked {asked}, face {face}, handed {handed}"
                );
            }
        }
    }

    #[test]
    fn the_weight_handed_on_is_the_request_or_the_face() {
        for (asked, face, handed) in [
            // Faked, at the weight asked for where that is 200 above.
            (900, 400, 900),
            (600, 400, 600),
            // Faked, but the request is too close for Skia: 200 above the
            // face instead, which a heavier request finds the face again at.
            (600, 500, 700),
            // Not faked: the face's own weight, whatever was asked.
            (900, 700, 700),
            (500, 200, 200),
            (400, 700, 700),
        ] {
            let got = synthesis_style(
                style(asked, Slant::Upright),
                style(face, Slant::Upright),
            );
            assert_eq!(*got.weight(), handed, "asked {asked}, face {face}");
        }
    }
}
