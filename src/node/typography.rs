#![allow(dead_code)]
#![allow(non_snake_case)]
#[cfg(all(feature = "node-addon", debug_assertions))]
use crate::context::typesetting::BUILDS;
use crate::{
    cache_map::{CacheMap, CacheSet},
    context::{State, faint::Faint, typesetting::Typography},
    css::{FontUnits, is_font_relative, parse_length, without_comments},
    font::{FontFamily, ObliqueAngle, leans_as_asked, variation_axes},
    font_library::{FontEpoch, FontLibrary, base_face},
    node::{
        decorate::{self, FaceLines, Layer, Thickness},
        lru::{Lru, LruId},
    },
    text::{
        TextAlign as CrateTextAlign, TextBaseline as CrateTextBaseline,
        TextMetricsLine, TextMetricsRun,
    },
    utils::*,
};
use neon::prelude::*;
use skia_safe::{
    Canvas, Color4f, Font, FontMetrics, FourByteTag, GlyphId, Matrix, Paint,
    Path as SkPath, PathBuilder as SkPathBuilder, Point, Rect, TextBlob,
    TextBlobBuilder, Typeface,
    font_style::{FontStyle, Slant, Weight, Width},
    textlayout::{
        Decoration, FontCollection, Paragraph, ParagraphBuilder,
        ParagraphStyle, RectHeightStyle, RectWidthStyle, TextAlign,
        TextDecoration, TextDirection, TextStyle,
    },
    typeface::TypefaceId,
};
use std::{
    borrow::Cow,
    cell::{Cell, RefCell, RefMut},
    collections::VecDeque,
    fmt,
    hash::{BuildHasher, RandomState},
    iter::{successors, zip},
    ops::Range,
    rc::Rc,
};

thread_local! {
    /// [`family_of`]'s answers, by the face's [`base_face`].
    static FAMILIES: RefCell<CacheMap<TypefaceId, String>> =
        RefCell::default();
}

/// How many faces [`glyph_inks`] remembers outline bounds for, the most
/// recently used kept: a face here is one instance at one size, so a page
/// of text at a handful of sizes in two or three families fits.
const INK_FACES: usize = 16;

/// How many glyphs one face's table holds. A glyph lives in the slot the
/// low bits of its id name, so finding it is an index rather than a hash,
/// and a glyph that shares its slot replaces it. Latin text in most faces
/// keeps its glyph ids under this, each in a slot of its own; at about
/// twenty-four bytes a slot, sixteen faces hold about 100 KB.
const INK_SLOTS: usize = 256;

/// What a glyph's outline depends on besides the glyph: the face, the
/// size, the horizontal scale and skew, whether it is emboldened, and the
/// hinting.
type FaceKey = (TypefaceId, u32, u32, u32, bool, i32);

/// A slot of a face's table: the glyph held there and its outline's
/// bounds, or `None` before any glyph has been.
type InkSlot = Option<(GlyphId, Option<Rect>)>;

/// One face's remembered outline bounds, by slot.
struct FaceInk {
    key: FaceKey,
    slots: Box<[InkSlot]>,
}

thread_local! {
    /// The faces [`glyph_inks`] remembers, the most recently used last.
    static FACE_INK: RefCell<Vec<FaceInk>> = const { RefCell::new(Vec::new()) };
}

/// The tight bounds of each of `glyphs`' outlines in `font`, each `None` for
/// a glyph with no outline -- a space, a bitmap or a colour emoji -- or an
/// empty one. Remembered.
///
/// The outline is what an ink box is joined from, and making it was two
/// thirds of working out a run's ink with every cache in Skia already warm:
/// `SkFont::getPath` builds a strike description and copies the path at the
/// font's size for every glyph, and `ComputeTightBounds` then walks it. A
/// glyph's outline depends on nothing but its face key and its id, so the
/// second time a glyph is measured in a face it is an index.
///
/// The face is found once for the run, not once a glyph: building its key
/// asks Skia six questions, and hashing it with the glyph cost more than
/// the rest of a 270-glyph measurement's ink.
fn glyph_inks(font: &Font, glyphs: &[GlyphId]) -> Vec<Option<Rect>> {
    let key = (
        font.typeface().unique_id(),
        font.size().to_bits(),
        font.scale_x().to_bits(),
        font.skew_x().to_bits(),
        font.is_embolden(),
        font.hinting() as i32,
    );
    let outline = |glyph| {
        font.get_path(glyph)
            .map(|outline| outline.compute_tight_bounds())
            .filter(|outline| !outline.is_empty())
    };
    FACE_INK.with(|faces| {
        let mut faces = faces.borrow_mut();
        match faces.iter().position(|face| face.key == key) {
            Some(at) => {
                let face = faces.remove(at);
                faces.push(face);
            }
            None => {
                if faces.len() == INK_FACES {
                    faces.remove(0);
                }
                faces.push(FaceInk {
                    key,
                    slots: vec![None; INK_SLOTS].into_boxed_slice(),
                });
            }
        }
        // SAFETY: a face was pushed on the line before, whichever arm ran.
        let face = faces.last_mut().unwrap();
        glyphs
            .iter()
            .map(|&glyph| {
                let slot = &mut face.slots[usize::from(glyph) % INK_SLOTS];
                match *slot {
                    Some((held, ink)) if held == glyph => ink,
                    _ => {
                        let ink = outline(glyph);
                        *slot = Some((glyph, ink));
                        ink
                    }
                }
            })
            .collect()
    })
}

/// The ink of `glyph` in `font`, drawn under `shear`, relative to its
/// position, or `None` for a glyph that inks nothing: one whose `reported`
/// box from `Font::get_bounds` is empty, as a space's is, or any glyph of a
/// flat run. `upright` is its outline's bounds from [`glyph_inks`], which
/// an upright run reads.
///
/// The glyph's outline, not the box `Font::get_bounds` reports. That one is
/// the rasterisation box: the outline rounded outwards to the pixel grid and
/// padded for the mask. At 48px it is exactly `floor - 1` and `ceil + 1` on
/// every glyph measured, across Helvetica, Times, Arial and Courier New; at
/// 480px the margin is wider and scales with the size. Either way it is
/// lossy -- 3.773 cannot be recovered from 2 -- so the subpixel box the
/// Canvas API asks for has to come from the outline. A glyph with no
/// outline, a bitmap or colour emoji, keeps the reported box: it is the only
/// one there is for a glyph that is not a path.
///
/// A sheared glyph's ink is its sheared outline's box, which is not the box
/// of the sheared box: a glyph's corners are rarely ink.
fn glyph_box(
    font: &Font,
    glyph: GlyphId,
    reported: Rect,
    upright: Option<Rect>,
    shear: &Shear,
) -> Option<Rect> {
    if reported.is_empty() {
        return None;
    }
    match shear {
        Shear::Flat => None,
        Shear::None => Some(upright.unwrap_or(reported)),
        Shear::By(shear) => Some(match font.get_path(glyph) {
            Some(outline) => {
                outline.with_transform(shear).compute_tight_bounds()
            }
            None => shear.map_rect(reported).0,
        }),
    }
}

/// The family `typeface` reports, remembered per face.
///
/// Per face rather than per instance: every instance of a variable font
/// reports its family, and CoreText builds the font's name table again for
/// each new instance it is asked about.
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

/// Forgets every family [`family_of`] remembered, called by
/// `FontLibrary::invalidate` whenever the registered fonts change.
///
/// Skia never gives two typefaces the same id, so an entry for a face the
/// library has let go of could only ever be asked for again by a
/// measurement still holding that face, and the next one asks for it once
/// more. Without this every registration of a font left one entry behind
/// for good -- two hundred registrations kept two hundred.
/// The maximum intrinsic width SkParagraph's `TextWrapper` starts from and
/// reports for a paragraph whose every line is no wider than nothing:
/// `std::numeric_limits<SkScalar>::min()`, the smallest positive normal
/// float -- see `Typesetter::signed_advance`. It comes back as itself, the
/// rounding hack being off; with the hack on it would round to zero.
const INTRINSIC_WIDTH_FLOOR: f32 = f32::MIN_POSITIVE;

/// A paragraph style as every paragraph here is laid out in: SkParagraph's
/// rounding hack off.
///
/// With it on, SkParagraph rounds intrinsic widths, line metrics, rects and
/// the glyph ends hit-testing reports to hundredths (`littleRound`, which its
/// source says is there to match Flutter's tests), compares widths rounded
/// when it breaks lines (`compareRound`), and breaks at its width floored to
/// a whole pixel (`ParagraphImpl::layout`). Chrome does none of these: its
/// `measureText` width is the advances' sum as a float, and its text wraps
/// at the width it is given -- Chrome 156 fits "Hamburgefonstiv quick" in
/// Oswald 24px, 200.53 wide, at 200.7px, where the floored width does not.
pub(crate) fn paragraph_style() -> ParagraphStyle {
    // UPSTREAM: skia-safe 0.156.0 -- unfiled -- not worked around
    // Re-check: "a line breaks within a hundredth of the edge" in
    // tests/suite/paragraph.test.js, its last assertion red once fixed.
    // With the hack off, `LineBreakerWithLittleRounding::breakLine` in
    // `TextWrapper.cpp` breaks, within a quarter pixel of `maxWidth`, where
    // `floor(width * 100) / 100 > maxWidth`: where the width reaches the
    // first hundredth above `maxWidth`, so a line up to a hundredth wider
    // than the paragraph still fits. Not worked around, as nothing exact
    // reaches it: no `maxWidth` makes that test `width > maxWidth` unless it
    // sits just under a hundredth, and `lookAhead` also measures each word
    // and cluster and the line aligns against the same `maxWidth`; laying an
    // overhanging line out again at a narrower width breaks every other
    // line there too; and breaking it by hand changes the text a
    // `Paragraph`'s ranges index.
    let mut style = ParagraphStyle::new();
    style.set_apply_rounding_hack(false);
    style
}

pub(crate) fn forget_families() {
    FAMILIES.with(|families| families.borrow_mut().clear());
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
    /// The fonts `typefaces` was built over, so that the run's decoration
    /// finds its face once an epoch -- see [`FaceLines::first_available`].
    fonts: Rc<FontEpoch>,
    /// The style of the face the style matched first, where one was.
    ///
    /// Decides the style the paragraph is first built with, by
    /// `synthesis_style`, which is the whole of it when every run is in
    /// that face. A run another face draws is restyled for its own face in
    /// `layout`.
    matched_style: Option<FontStyle>,
    /// The angle an `oblique` font is drawn at, or `None` -- see
    /// [`run_shear`].
    oblique: Option<f32>,
    /// Whether the face the style matched first already leans as the
    /// `oblique` asks, where there is an `oblique` to ask about.
    matched_leans: Option<bool>,
    /// Shared with the context that set it, or the [`TypesetInputs`] laying
    /// a run out again -- see [`State::typography`]. For an `oblique` that
    /// lays out in the upright style, a copy of the context's of its own.
    typography: Rc<Typography>,
    /// The context's typography as it keeps it, which `typography` is a
    /// copy of where an `oblique` needed one: what the runs kept for drawing
    /// are filed under, so that a copy made again for each draw finds the
    /// run the last one kept -- see [`LaidOutKey`]. The copy is worked out
    /// from it and the fonts' epoch alone, which it stands for.
    kept: Rc<Typography>,
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

/// `style` with kerning turned off, for a run of word separators -- see
/// [`Typesetter::paragraph_with`].
fn without_kerning(style: &TextStyle) -> TextStyle {
    let mut unkerned = style.clone();
    unkerned.add_font_feature("kern", 0);
    unkerned
}

/// The characters word spacing is added after: CSS Text 3 section 8.2's
/// word-separator characters, fixed by the specification. U+3000, the
/// ideographic space, is not one, though SkParagraph spaces it.
const WORD_SEPARATORS: [char; 7] = [
    '\u{20}',    // SPACE
    '\u{a0}',    // NO-BREAK SPACE
    '\u{1361}',  // ETHIOPIC WORDSPACE
    '\u{10100}', // AEGEAN WORD SEPARATOR LINE
    '\u{10101}', // AEGEAN WORD SEPARATOR DOT
    '\u{1039f}', // UGARITIC WORD DIVIDER
    '\u{1091f}', // PHOENICIAN WORD SEPARATOR
];

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
    /// The ranges laid out in a style of their own.
    restyled: Vec<(Range<usize>, FontStyle)>,
    /// The angle an `oblique` font is drawn at, which decides which runs
    /// are sheared -- see [`run_shear`].
    oblique: Option<f32>,
}

/// Which painter a test has a run drawn with: from the kept runs, as every
/// draw is, or by `Paragraph::paint`, the oracle the kept runs are held to
/// -- see `the_run_painter_draws_what_paragraph_paint_draws`.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Painter {
    /// `Paragraph::paint`, the whole paragraph at once.
    Paragraph,
    /// [`LaidOut::paint`], a kept glyph run at a time.
    Runs,
}

impl TextLayout {
    /// `run`, a glyph run of this layout, as a text blob in its own font,
    /// to draw at the run's origin, or `None` where it draws nothing.
    ///
    /// A run at a time rather than through `Paragraph::paint`, so that a run
    /// can be drawn sheared or not on its own -- see [`sheared_blob`] -- and
    /// in any paint once laid out. The pixels of a run drawn as it stands
    /// are `Paragraph::paint`'s: each run's `Font` already carries what
    /// SkParagraph synthesised for it -- an embolden, a skew -- and the run's
    /// origin is the one `Paragraph::paint` draws from, its baseline snapped
    /// to a whole pixel (#142). A plain run is about 1.5% slower to draw
    /// this way than by `Paragraph::paint`, which a kept run cannot use: it
    /// was laid out with no paint.
    pub(crate) fn run_blob(&self, run: &GlyphRun) -> Option<TextBlob> {
        let shear = run_shear(&run.font.typeface(), self.oblique);
        sheared_blob(&run.font, &run.glyphs, &run.positions, shear)
    }
}

/// A run laid out once, and painted from then on with whatever paint a draw
/// brings: its glyph runs, read off the paragraph when it was laid out, and
/// the spans and box its decoration lines are drawn from.
///
/// Laid out with no paint, because nothing about where a glyph or a line
/// goes depends on the paint: a draw in another colour, a gradient or
/// another alpha paints the same runs and the same lines, which is what lets
/// a run be kept across them. The glyphs are drawn as the blobs
/// [`TextLayout::run_blob`] builds, whose pixels are `Paragraph::paint`'s,
/// and the lines by [`decorate::paint`] in the colour the draw's paint
/// gives them.
///
/// Keeping the layout is the point: building the paragraph and laying it out
/// was 55% of the samples under `fillText` in a release build, and reading
/// its runs back another 633 of the run painter's 1172.
pub struct LaidOut {
    layout: TextLayout,
    /// The glyph runs, read off the paragraph once.
    runs: Vec<GlyphRun>,
    /// The runs' blobs, kept by the paint they were built for, at most
    /// [`BLOB_PAINTS`] paints' worth, the most recently built last.
    ///
    /// A blob is drawn again only in the paint it was built for, compared
    /// whole -- see [`PaintedBlobs::matches`]. On Skia's GPU backend a blob
    /// drawn again in another colour after the canvas had been read back
    /// came out unlike a blob of its own drawn in that colour: 1330 pixels
    /// of a run drawn in `#c03010` after the same blob in `green`, and as
    /// many the other way, while every blob drawn again in the paint it was
    /// built for drew the same as a fresh one. Which pairs of colours part
    /// is not settled: of seven opaque colours only those two did, and a
    /// translucent pink parted from `green` as `#c03010` did.
    ///
    /// UPSTREAM: skia-safe 0.156.0 -- unfiled -- worked around
    /// Re-check: cargo test a_run_drawn_again_on_the_gpu_draws_as_a_fresh_one
    /// with `PaintedBlobs::matches` answering `true` for every paint
    blobs: RefCell<Vec<PaintedBlobs>>,
    /// The run's one decorating box, or `None` where it has no lines. Its
    /// opacity is set by each draw -- see [`LaidOut::paint`].
    decoration: Option<decorate::Decoration>,
    /// What each line of the box covers, by [`decorate::spans`].
    spans: Vec<decorate::Span>,
    /// How many lines the runs are on.
    lines: usize,
    /// What this costs to keep, by [`laid_out_bytes`]'s estimate.
    bytes: usize,
}

impl LaidOut {
    fn new(
        mut layout: TextLayout,
        decoration: Option<decorate::Decoration>,
    ) -> Self {
        let runs = glyph_runs(&mut layout.paragraph);
        // The run is one decorating box: every run of it is the box's,
        // whatever face draws it.
        let spans = match decoration {
            Some(_) => decorate::spans(&runs, |_| Some(0)),
            None => Vec::new(),
        };
        let lines = runs.last().map_or(0, |run| run.line + 1);
        let glyphs = runs.iter().map(|run| run.glyphs.len()).sum();
        Self {
            layout,
            runs,
            blobs: RefCell::new(Vec::new()),
            decoration,
            spans,
            lines,
            bytes: laid_out_bytes(glyphs),
        }
    }

    /// The runs' blobs for `paint`: the ones kept for it, or built now and
    /// kept, letting the paint built for least recently go past
    /// [`BLOB_PAINTS`]. `None` where the blobs are already borrowed, for the
    /// caller to build its own.
    fn kept_blobs(
        &self,
        paint: &Paint,
    ) -> Option<RefMut<'_, [Option<TextBlob>]>> {
        let mut kept = self.blobs.try_borrow_mut().ok()?;
        let at = match kept.iter().position(|painted| painted.matches(paint)) {
            Some(at) => at,
            None => {
                if kept.len() == BLOB_PAINTS {
                    kept.remove(0);
                }
                kept.push(PaintedBlobs {
                    paint: paint.clone(),
                    blobs: self.built_blobs(),
                });
                kept.len() - 1
            }
        };
        Some(RefMut::map(kept, |kept| kept[at].blobs.as_mut_slice()))
    }

    fn built_blobs(&self) -> Vec<Option<TextBlob>> {
        self.runs
            .iter()
            .map(|run| self.layout.run_blob(run))
            .collect()
    }

    /// Draws the runs in `range` from `blobs`, the blobs of every run, onto
    /// pixels that do with a faint paint what `faint` says.
    fn draw_runs(
        &self,
        canvas: &Canvas,
        origin: Point,
        paint: &Paint,
        range: Range<usize>,
        blobs: &[Option<TextBlob>],
        faint: Faint,
    ) {
        let paint = faint.drawable(paint);
        let runs = zip(&self.runs[range.clone()], &blobs[range]);
        for (run, blob) in runs {
            if let Some(blob) = blob {
                canvas.draw_text_blob(blob, origin + run.origin, &paint);
            }
        }
    }

    /// Where to paint the run, relative to the anchor -- see
    /// [`TextLayout::offset`].
    pub fn offset(&self) -> Point {
        self.layout.offset
    }

    /// The horizontal factor the run is squeezed by -- see
    /// [`TextLayout::condensation`].
    pub fn condensation(&self) -> f32 {
        self.layout.condensation
    }

    /// Paints the run with its anchor at `origin` in `paint`, a named line
    /// colour's alpha multiplied by `global_alpha`, as
    /// [`Typesetter::decoration`] takes it, onto pixels that do with a faint
    /// paint what `faint` says.
    ///
    /// A decorated run is drawn a line at a time, in the order CSS 2.1
    /// Appendix E paints a line box: underlines and overlines beneath the
    /// glyphs, line-through over them -- CSS Text Decoration 4, section 5.1
    /// -- so that a wrapped line's underline lies over the line above's
    /// descenders, as in Chrome.
    pub fn paint(
        &self,
        canvas: &Canvas,
        origin: Point,
        paint: &Paint,
        global_alpha: f32,
        faint: Faint,
    ) {
        let built;
        let kept = self.kept_blobs(paint);
        let blobs: &[Option<TextBlob>] = match &kept {
            Some(kept) => kept,
            None => {
                built = self.built_blobs();
                &built
            }
        };
        let Some(decoration) = &self.decoration else {
            let all = 0..self.runs.len();
            self.draw_runs(canvas, origin, paint, all, blobs, faint);
            return;
        };
        let boxes = [decorate::Decoration {
            opacity: global_alpha,
            ..decoration.clone()
        }];
        for line in 0..self.lines {
            let spans = decorate::on_line(&self.spans, line);
            let start = self.runs.partition_point(|run| run.line < line);
            let end = self.runs.partition_point(|run| run.line <= line);
            decorate::paint(
                canvas,
                spans,
                &boxes,
                origin,
                Layer::Beneath,
                paint,
                faint,
            );
            self.draw_runs(canvas, origin, paint, start..end, blobs, faint);
            decorate::paint(
                canvas,
                spans,
                &boxes,
                origin,
                Layer::Over,
                paint,
                faint,
            );
        }
    }
}

/// A run as one draw paints it: kept, or laid out for that draw alone --
/// see [`Typesetter::laid_out`].
pub enum DrawnRun {
    /// Kept across draws -- see [`LaidOut`].
    Kept(Rc<LaidOut>),
    /// Laid out for the draw's paint and drawn by `Paragraph::paint`.
    Plain(TextLayout),
    /// Laid out for the draw's paint and drawn from its runs.
    Runs(LaidOut),
}

impl DrawnRun {
    /// Where to paint the run, relative to the anchor -- see
    /// [`TextLayout::offset`].
    pub fn offset(&self) -> Point {
        match self {
            Self::Kept(laid_out) => laid_out.offset(),
            Self::Plain(layout) => layout.offset,
            Self::Runs(laid_out) => laid_out.offset(),
        }
    }

    /// The horizontal factor the run is squeezed by -- see
    /// [`TextLayout::condensation`].
    pub fn condensation(&self) -> f32 {
        match self {
            Self::Kept(laid_out) => laid_out.condensation(),
            Self::Plain(layout) => layout.condensation,
            Self::Runs(laid_out) => laid_out.condensation(),
        }
    }

    /// Paints the run with its anchor at `origin` in `paint` -- see
    /// [`LaidOut::paint`]. A run laid out for one draw paints in the paint
    /// it was laid out for, so a caller drawing onto pixels that keep a
    /// faint paint lays it out for `faint.drawable(paint)`.
    pub fn paint(
        &self,
        canvas: &Canvas,
        origin: Point,
        paint: &Paint,
        global_alpha: f32,
        faint: Faint,
    ) {
        match self {
            Self::Kept(laid_out) => {
                laid_out.paint(canvas, origin, paint, global_alpha, faint)
            }
            Self::Plain(layout) => layout.paragraph.paint(canvas, origin),
            Self::Runs(laid_out) => {
                laid_out.paint(canvas, origin, paint, global_alpha, faint)
            }
        }
    }
}

/// The blobs of a kept run's glyph runs, built for one paint: one for each
/// run, `None` for a run that draws nothing.
struct PaintedBlobs {
    /// The paint the blobs were built for, kept whole: holding it keeps its
    /// shader and other effects alive, so that an address it is compared by
    /// cannot be freed and given to another.
    paint: Paint,
    blobs: Vec<Option<TextBlob>>,
}

impl PaintedBlobs {
    /// Whether the blobs may be drawn again in `paint`: only in the paint
    /// they were built for, every field of it the same -- colour and alpha
    /// by value, the shader and other effects by identity.
    fn matches(&self, paint: &Paint) -> bool {
        self.paint == *paint
    }
}

/// How many paints a kept run keeps blobs for: a shadowed run is drawn in
/// two, the shadow's and the text's, every time.
const BLOB_PAINTS: usize = 2;

/// What a kept run holds beyond its glyphs: the laid-out paragraph's own
/// structure, the entry and the blobs it keeps.
///
/// Measured as the malloc zone's bytes in use that letting runs of 24px
/// Oswald go handed back, in a debug build on macOS, each drawn in two
/// colours so that it kept blobs for two paints: 17.6 KB a run of 12
/// glyphs, 28.3 KB of 48 and 70.5 KB of 192, which this and
/// [`LAID_OUT_GLYPH_BYTES`] fit to within 4%. The blobs are inside that:
/// the same runs drawn in one colour measured the same to within the
/// meter's noise.
const LAID_OUT_BASE_BYTES: usize = 13 * 1024;

/// What each glyph of a kept run adds, in the paragraph and in the glyph run
/// read off it -- see [`LAID_OUT_BASE_BYTES`].
const LAID_OUT_GLYPH_BYTES: usize = 324;

/// The estimated cost of keeping a run of `glyphs` glyphs.
fn laid_out_bytes(glyphs: usize) -> usize {
    LAID_OUT_BASE_BYTES + glyphs * LAID_OUT_GLYPH_BYTES
}

/// The most the kept runs are estimated to hold together, past which the
/// least recently drawn go.
const LAID_OUT_BYTES: usize = 8 * 1024 * 1024;

/// The most runs kept, whatever they are estimated to hold, so that a flood
/// of tiny ones cannot grow the table without end.
const LAID_OUT_COUNT: usize = 512;

/// What a kept run was laid out from, compared against a typesetter: the
/// context's typography as the context keeps it, not the copy an `oblique`
/// lays out in -- see `Typesetter::kept` -- shared rather than copied and
/// held so that its address cannot be reused; the text as laid out; and the
/// widths.
///
/// The typography stands for every setting a run is laid out from, the
/// `oblique` angle among them, and the fonts registered when it was built
/// -- see `context::typesetting` -- so a run kept from another font epoch,
/// or before a setting changed, is one no typesetter can match.
struct LaidOutKey {
    typography: Rc<Typography>,
    text: Box<str>,
    width: u32,
    condense_to: Option<u32>,
}

impl LaidOutKey {
    fn of(typesetter: &Typesetter<'_>) -> Self {
        Self {
            typography: typesetter.kept.clone(),
            text: typesetter.text.as_ref().into(),
            width: typesetter.width.to_bits(),
            condense_to: typesetter.condense_to.map(f32::to_bits),
        }
    }

    fn matches(&self, typesetter: &Typesetter<'_>) -> bool {
        Rc::ptr_eq(&self.typography, &typesetter.kept)
            && self.width == typesetter.width.to_bits()
            && self.condense_to == typesetter.condense_to.map(f32::to_bits)
            && *self.text == *typesetter.text
    }

    /// The bucket a typesetter's run is filed under, in the cache whose
    /// hash keys are `keys`.
    fn hash(typesetter: &Typesetter<'_>, keys: &RandomState) -> u64 {
        #[cfg(test)]
        if COLLIDING.get() {
            return 0;
        }
        Self::digest(
            keys,
            Rc::as_ptr(&typesetter.kept),
            &typesetter.text,
            typesetter.width,
        )
    }

    /// The hash of a run's typography, text and width under `keys`.
    fn digest(
        keys: &RandomState,
        typography: *const Typography,
        text: &str,
        width: f32,
    ) -> u64 {
        keys.hash_one((typography, text, width.to_bits()))
    }
}

/// A kept run, filed under its key's hash ahead of the runs filed there
/// before it.
struct LaidOutEntry {
    key: LaidOutKey,
    hash: u64,
    laid_out: Rc<LaidOut>,
    /// The next run filed under the same hash. Two runs share one only where
    /// their hashes collide, so a chain is almost always one run long.
    next: Option<LruId>,
}

/// The kept runs, the least recently drawn going first past
/// [`LAID_OUT_BYTES`] or [`LAID_OUT_COUNT`].
///
/// The runs are in an [`Lru`], so that drawing one, keeping one and letting
/// the oldest go are each a constant amount of work and the bytes held are a
/// running total. A run is held in an `Rc` because a draw holds it beyond the
/// cache, across the shadow pass and the fill, while drawing another run may
/// let it go.
#[derive(Default)]
struct LaidOutCache {
    runs: Lru<LaidOutEntry>,
    /// The keys every hash this cache files a run under is taken with,
    /// chosen at random for this cache, so that which texts share a hash
    /// cannot be worked out ahead of a draw.
    keys: RandomState,
    /// The first run filed under each hash; the rest follow it through
    /// [`LaidOutEntry::next`].
    ///
    /// A `CacheMap`, though the text the hash is taken from can come from
    /// anyone: its keys are digests taken with [`LaidOutCache::keys`], and
    /// it holds at most [`LAID_OUT_COUNT`], as [`LaidOutCache::seen`] holds
    /// at most [`SEEN_ONCE`].
    by_hash: CacheMap<u64, LruId>,
    /// The hashes of runs drawn once and not kept, so that the second draw
    /// of one keeps it -- see [`Typesetter::laid_out`].
    seen: CacheSet<u64>,
    /// [`LaidOutCache::seen`] in the order the hashes came, for letting the
    /// oldest go past [`SEEN_ONCE`].
    seen_order: VecDeque<u64>,
}

/// How many runs drawn once a thread remembers, so that the next draw of
/// one keeps it: twice the most runs kept, so that a frame of new labels as
/// long as the cache can hold is kept from its second frame, with as many
/// one-off strings drawn between.
const SEEN_ONCE: usize = 2 * LAID_OUT_COUNT;

impl LaidOutCache {
    fn find(&self, typesetter: &Typesetter<'_>) -> Option<LruId> {
        let first = self
            .by_hash
            .get(&LaidOutKey::hash(typesetter, &self.keys))
            .copied();
        successors(first, |id| self.runs.get(*id)?.next).find(|id| {
            self.runs
                .get(*id)
                .is_some_and(|entry| entry.key.matches(typesetter))
        })
    }

    fn get(&mut self, typesetter: &Typesetter<'_>) -> Option<Rc<LaidOut>> {
        let id = self.find(typesetter)?;
        Some(self.runs.touch(id)?.laid_out.clone())
    }

    fn insert(&mut self, typesetter: &Typesetter<'_>, laid_out: Rc<LaidOut>) {
        let hash = LaidOutKey::hash(typesetter, &self.keys);
        let bytes = laid_out.bytes;
        let entry = LaidOutEntry {
            key: LaidOutKey::of(typesetter),
            hash,
            laid_out,
            next: self.by_hash.get(&hash).copied(),
        };
        let id = self.runs.insert(entry, bytes);
        self.by_hash.insert(hash, id);
        self.trim();
    }

    fn trim(&mut self) {
        while self.runs.len() > LAID_OUT_COUNT
            || self.runs.bytes() > LAID_OUT_BYTES
        {
            let Some(oldest) = self.runs.oldest() else {
                break;
            };
            self.evict(oldest);
        }
    }

    /// Lets the run `id` was kept as go, and takes it out of its hash's
    /// chain.
    fn evict(&mut self, id: LruId) {
        let Some(gone) = self.runs.remove(id) else {
            return;
        };
        let mut at = match self.by_hash.get(&gone.hash) {
            Some(first) if *first == id => {
                match gone.next {
                    Some(next) => self.by_hash.insert(gone.hash, next),
                    None => self.by_hash.remove(&gone.hash),
                };
                return;
            }
            first => first.copied(),
        };
        while let Some(entry) = at.and_then(|before| self.runs.get_mut(before))
        {
            if entry.next == Some(id) {
                entry.next = gone.next;
                return;
            }
            at = entry.next;
        }
    }

    /// Whether the run `typesetter` lays out has been drawn since the
    /// thread's fonts last changed, and so is worth keeping; where not,
    /// records that it has been now.
    ///
    /// By hash alone: two runs whose hashes collide only mean the second is
    /// kept a draw early, since a kept run is found by its whole key.
    fn admits(&mut self, typesetter: &Typesetter<'_>) -> bool {
        let hash = LaidOutKey::hash(typesetter, &self.keys);
        if self.seen.contains(&hash) {
            return true;
        }
        self.seen.insert(hash);
        self.seen_order.push_back(hash);
        if self.seen_order.len() > SEEN_ONCE
            && let Some(oldest) = self.seen_order.pop_front()
        {
            self.seen.remove(&oldest);
        }
        false
    }

    fn clear(&mut self) {
        self.runs.clear();
        self.by_hash.clear();
        self.seen.clear();
        self.seen_order.clear();
    }
}

thread_local! {
    /// The runs this thread has laid out for drawing -- see [`LaidOut`].
    static LAID_OUT: RefCell<LaidOutCache> = RefCell::default();
}

#[cfg(test)]
thread_local! {
    /// Test-only: files every run under one hash, so that the chains a
    /// collision makes are walked and cut as a real one's would be.
    static COLLIDING: Cell<bool> = const { Cell::new(false) };
}

/// Lets every kept run go: called when the registered fonts change, since a
/// kept run holds the faces it was laid out in, and by the idle release.
pub(crate) fn forget_laid_out() {
    let _ = LAID_OUT.try_with(|cache| {
        if let Ok(mut cache) = cache.try_borrow_mut() {
            cache.clear();
        }
    });
}

/// Test-only: `[count, bytes]`, how many runs this thread keeps for drawing
/// and what they are estimated to hold -- see [`LaidOut`].
///
/// Exported by debug builds only, as `__discardGpuDraws` is.
#[cfg(all(feature = "node-addon", debug_assertions))]
pub fn laidOut(mut cx: FunctionContext) -> JsResult<JsArray> {
    let (count, bytes) = laid_out_held();
    let held = cx.empty_array();
    let count = cx.number(count as f64);
    held.set(&mut cx, 0, count)?;
    let bytes = cx.number(bytes as f64);
    held.set(&mut cx, 1, bytes)?;
    Ok(held)
}

/// Test-only: how many typographies this thread has built -- see
/// `context::typesetting::BUILDS`.
///
/// Exported by debug builds only, as `__discardGpuDraws` is.
#[cfg(all(feature = "node-addon", debug_assertions))]
pub fn typographyBuilds(mut cx: FunctionContext) -> JsResult<JsNumber> {
    Ok(cx.number(BUILDS.get() as f64))
}

/// Test-only: lets every run this thread keeps for drawing go, so that a
/// test can measure what they held.
///
/// Exported by debug builds only, as `__discardGpuDraws` is.
#[cfg(all(feature = "node-addon", debug_assertions))]
pub fn forgetLaidOut(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    forget_laid_out();
    Ok(cx.undefined())
}

/// How many runs are kept and what they are estimated to hold, for the tests
/// here and the debug addon's `__laidOut`.
#[cfg(any(test, all(debug_assertions, feature = "node-addon")))]
pub(crate) fn laid_out_held() -> (usize, usize) {
    LAID_OUT.with(|cache| {
        let cache = cache.borrow();
        (cache.runs.len(), cache.runs.bytes())
    })
}

/// One glyph run of a laid-out paragraph: the glyphs one font draws, where
/// each goes relative to the run's origin, and that origin relative to the
/// paragraph's.
pub(crate) struct GlyphRun {
    /// The line the run is on, from 0.
    pub(crate) line: usize,
    pub(crate) font: Font,
    pub(crate) glyphs: Vec<GlyphId>,
    pub(crate) positions: Vec<Point>,
    /// The UTF-8 offset of the text each glyph comes from.
    pub(crate) starts: Vec<u32>,
    /// The UTF-8 offset after the last glyph's -- see [`GlyphRunRef::end`].
    pub(crate) end: u32,
    pub(crate) origin: Point,
    /// The right edge of the run's clip, in the paragraph's coordinates.
    pub(crate) advance_x: f32,
}

/// The glyph runs `paragraph` paints, in the positions `Paragraph::paint`
/// draws them at.
///
/// Read with `Paragraph::visit`, which reports the text blobs
/// `Paragraph::paint` draws, glyph positions and all. `extended_visit`
/// reports each glyph where its advances put it, without the offset the
/// shaper gave it: the offset that stacks a mark or sets it over its base,
/// and the half of a kern from a font's legacy `kern` table that HarfBuzz
/// carries as an offset. The run painter's differential test separates the
/// two, on stacked marks and on 48px Helvetica `"AV To Ta"`. Its text
/// offsets are worse: on a line a run continues onto they repeat those of
/// the run's first line, and for a run that does not start the text the
/// one after its last glyph is read from past the storage holding them.
/// `visit`'s are each glyph's own, on every line.
pub(crate) fn glyph_runs(paragraph: &mut Paragraph) -> Vec<GlyphRun> {
    let mut runs = vec![];
    visit_glyph_runs(paragraph, |run| {
        runs.push(GlyphRun {
            line: run.line,
            font: run.font.clone(),
            glyphs: run.glyphs.to_vec(),
            positions: run.positions.to_vec(),
            starts: run.starts.to_vec(),
            end: run.end,
            origin: run.origin,
            advance_x: run.advance_x,
        });
    });
    runs
}

/// A [`GlyphRun`] borrowed from `Paragraph::visit` for the length of the
/// visit, for a caller that needs nothing of it afterwards.
pub(crate) struct GlyphRunRef<'a> {
    pub(crate) line: usize,
    pub(crate) font: &'a Font,
    pub(crate) glyphs: &'a [GlyphId],
    pub(crate) positions: &'a [Point],
    pub(crate) starts: &'a [u32],
    /// The UTF-8 offset `Paragraph::visit` reports after the last glyph's:
    /// the start of the cluster that follows the run in its text.
    pub(crate) end: u32,
    pub(crate) origin: Point,
    pub(crate) advance_x: f32,
}

impl GlyphRun {
    pub(crate) fn borrowed(&self) -> GlyphRunRef<'_> {
        GlyphRunRef {
            line: self.line,
            font: &self.font,
            glyphs: &self.glyphs,
            positions: &self.positions,
            starts: &self.starts,
            end: self.end,
            origin: self.origin,
            advance_x: self.advance_x,
        }
    }
}

/// Calls `each` with every glyph run [`glyph_runs`] would return, borrowed
/// rather than copied.
pub(crate) fn visit_glyph_runs(
    paragraph: &mut Paragraph,
    mut each: impl FnMut(GlyphRunRef<'_>),
) {
    // UPSTREAM: skia-safe 0.153.3 -- unfiled -- not worked around
    // Re-check: a profile of a canvas `oblique` fillText in a variable face
    // shows SkTextBlobBuilder::TightRunBounds under TextLine::buildTextBlob,
    // called from ParagraphImpl::visit.
    //
    // `visit` reports the runs from each line's text blobs, building them
    // first, and SkParagraph allocates every run in them without bounds, so a
    // variable face's are worked out glyph by glyph for blobs nothing draws:
    // 1.8 to 2.5 per cent of the samples of an oblique 24px Oswald
    // `fillText`. The blobs cannot be skipped, since they are where the
    // glyphs' shaper offsets are -- see `glyph_runs` -- and the bounds are
    // SkParagraph's to give, in `TextLine::buildTextBlob`.
    paragraph.visit(|line, visit| {
        if let Some(info) = visit {
            let count = info.glyphs().len();
            each(GlyphRunRef {
                line,
                font: info.font(),
                glyphs: info.glyphs(),
                positions: info.positions(),
                starts: &info.utf8_starts()[..count],
                end: info.utf8_starts()[count],
                origin: info.origin(),
                advance_x: info.advance_x(),
            });
        }
    });
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
/// - **An `oblique` request is never synthesised here.** `leans` says whether
///   the face already leans as asked -- see [`leans_as_asked`]: such a face
///   keeps its slant, and any other is laid out upright, an italic face
///   included, and sheared by the painter to the angle asked for -- see
///   [`run_shear`].
fn synthesis_style(
    requested: FontStyle,
    face: FontStyle,
    leans: Option<bool>,
) -> FontStyle {
    let (asked, has) = (*requested.weight(), *face.weight());
    let bold = *Weight::SEMI_BOLD;
    let weight = match asked >= bold && has < bold {
        true => asked.max(has + SKIA_FAKE_BOLD_GAP),
        false => has,
    };
    let slant = match (requested.slant(), face.slant(), leans) {
        (Slant::Oblique, face, Some(true)) => face,
        (Slant::Oblique, _, Some(false)) => Slant::Upright,
        (Slant::Upright, face, _) => face,
        (_, Slant::Upright, _) => Slant::Italic,
        (_, face, _) => face,
    };
    FontStyle::new(Weight::from(weight), face.width(), slant)
}

/// `point` under `matrix`, an affine one, as `Matrix::map_point` maps it --
/// in Rust, so that mapping a run's glyphs one at a time crosses into Skia
/// for none of them.
pub(crate) fn map_affine(matrix: &Matrix, point: Point) -> Point {
    Point::new(
        matrix.scale_x() * point.x
            + matrix.skew_x() * point.y
            + matrix.translate_x(),
        matrix.skew_y() * point.x
            + matrix.scale_y() * point.y
            + matrix.translate_y(),
    )
}

/// The smallest rectangle holding `a` and `b`, two that are not empty, in
/// Rust: a run's glyph boxes are joined one at a time, and `Rect::join`
/// crosses into Skia for each.
fn union(a: Rect, b: Rect) -> Rect {
    Rect::new(
        a.left.min(b.left),
        a.top.min(b.top),
        a.right.max(b.right),
        a.bottom.max(b.bottom),
    )
}

/// How a run is drawn for an `oblique` request: as laid out, sheared, or not
/// at all.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Shear {
    /// Drawn as laid out.
    None,
    /// Drawn under this shear, about the run's baseline.
    By(Matrix),
    /// Sheared to a right angle, which lays every glyph along its own
    /// baseline: drawn as nothing, with no ink and no outline, and advancing
    /// as it would upright.
    Flat,
}

/// A blob of `glyphs` at `positions`, relative to a run's origin, sheared
/// by `shear` about the run's baseline -- or `None` where there is nothing
/// to draw.
///
/// The glyphs are sheared through the font's own x skew, and their
/// positions with them, rather than by drawing under a sheared canvas: the
/// canvas's transform also carries the paint, so a gradient, a pattern, a
/// shadow's blur and a stroke's pen would all be sheared with the glyphs.
/// Shears along x add, so a run SkParagraph skewed for a fake italic keeps
/// that skew and takes this one on top, as the outline the path and the
/// ink box shear does.
///
/// The run is allocated without bounds, so Skia works them out: from the
/// face's bounds where it has them, and glyph by glyph where it has none,
/// which is every variable face -- Oswald's on CoreText reports
/// `FontMetrics::has_bounds` false and empty typeface bounds, since its
/// head table's box need not hold an instance's ink. Handing a rectangle
/// from those metrics instead would be cheaper and is not done: blob bounds
/// cull the draw and size a filter's layer, and nothing guarantees such a
/// rectangle holds the glyphs. The bounds Skia works out here cost about 2
/// per cent of an oblique `fillText` -- 1.9 per cent of the samples of 24px
/// Oswald drawn and read back -- which is less than a rectangle that has to
/// be padded for the ink outside each outline, in device pixels, is worth.
pub(crate) fn sheared_blob(
    font: &Font,
    glyphs: &[GlyphId],
    positions: &[Point],
    shear: Shear,
) -> Option<TextBlob> {
    let (font, positions) = match shear {
        Shear::None => (Cow::Borrowed(font), Cow::Borrowed(positions)),
        Shear::By(shear) => {
            let mut sheared = font.clone();
            sheared.set_skew_x(font.skew_x() + shear.skew_x());
            let moved = positions.iter().map(|&at| map_affine(&shear, at));
            (Cow::Owned(sheared), Cow::Owned(moved.collect()))
        }
        Shear::Flat => return None,
    };
    let mut builder = TextBlobBuilder::new();
    let (into_glyphs, into_positions) =
        builder.alloc_run_pos(&font, glyphs.len(), None);
    into_glyphs.copy_from_slice(glyphs);
    into_positions.copy_from_slice(&positions);
    builder.make()
}

/// The angle at which a shear lays a glyph along its baseline, where its
/// tangent has no finite value.
const FLAT_DEGREES: f32 = 90.0;

/// The shear a run in `face` is drawn under for an `oblique` request at
/// `angle` degrees -- [`shear_for`] of the face's reported slant and of
/// whether it leans by design.
pub(crate) fn run_shear(face: &Typeface, angle: Option<f32>) -> Shear {
    shear_for(
        face.font_style().slant(),
        |angle| leans_as_asked(face, angle),
        angle,
    )
}

/// The shear a run is drawn under for an `oblique` request at `angle`
/// degrees, in a face Skia reports as `slant` and that `leans` at an angle
/// or not.
///
/// A face upright by design that does not lean as asked is sheared, by
/// CSS Fonts 4's "geometric shearing to the specified oblique value": a
/// positive angle leans clockwise, the top of each glyph to the right, and a
/// negative one backwards. One that leans as asked, or is slanted by design
/// either way, is not sheared on top of its own slant.
///
/// CoreText and FreeType report every face that leans -- one flagged
/// oblique, and a variable instance at a nonzero `slnt` -- as slanted, so on
/// both the slant test alone decides. `leans` is asked as well for a backend
/// that reports an instance with its default instance's style, which nothing
/// here has measured DirectWrite not to do.
///
/// At 90deg either way the shear is [`Shear::Flat`]. That is the limit of
/// the shear rather than a case of its own: as the angle nears a right angle
/// the glyph thins towards its baseline and covers less of any region of
/// finite height, so `oblique 89.9deg` draws a faint sliver and 90deg the
/// nothing it tends to.
fn shear_for(
    slant: Slant,
    leans: impl FnOnce(f32) -> bool,
    angle: Option<f32>,
) -> Shear {
    let Some(angle) = angle else {
        return Shear::None;
    };
    if slant != Slant::Upright || leans(angle) {
        return Shear::None;
    }
    match angle.abs() >= FLAT_DEGREES {
        true => Shear::Flat,
        false => Shear::By(Matrix::skew((-angle.to_radians().tan(), 0.0))),
    }
}

/// The faces `style` is laid out with for an `oblique` at `angle`, found by
/// `resolve`, with `style` set upright where the request is drawn by
/// shearing.
///
/// CSS Fonts 4, section 5.2, puts "a fallback match ... produced by
/// geometric shearing to the specified oblique value" before any italic
/// face: "If no match is found, italic values ... are checked" comes after
/// it, and with synthesis allowed a shear always matches. So where the face
/// an oblique request finds first does not lean as asked -- Skia's matcher
/// answers `Oblique` with a family's italic -- the run is laid out in the
/// upright face, and sheared. The faces are resolved again for that,
/// because a variable family is instanced only for the faces its request
/// matched.
fn resolve_for_oblique(
    style: &mut TextStyle,
    angle: Option<f32>,
    resolve: impl Fn(&TextStyle) -> (FontCollection, Option<FontStyle>),
) -> (FontCollection, Option<FontStyle>) {
    let faces = resolve(style);
    let Some(angle) = angle else { return faces };
    let families = style.font_families();
    let families: Vec<&str> = families.iter().collect();
    let leans = faces
        .0
        .clone()
        .find_typefaces(&families, style.font_style())
        .first()
        .is_some_and(|face| leans_as_asked(face, angle));
    if leans {
        return faces;
    }
    let asked = style.font_style();
    style.set_font_style(FontStyle::new(
        asked.weight(),
        asked.width(),
        Slant::Upright,
    ));
    resolve(style)
}

/// The OpenType `slnt` axis tag.
pub(crate) const SLNT: FourByteTag =
    FourByteTag::from_chars('s', 'l', 'n', 't');

/// The OpenType `wght` axis tag.
pub(crate) const WGHT: FourByteTag =
    FourByteTag::from_chars('w', 'g', 'h', 't');

/// `variations` with an `oblique` angle's `slnt` and a fractional weight's
/// `wght` ahead of them, so that a face with those axes is instanced at
/// them and an explicit `slnt` or `wght` in `variations` still wins.
///
/// CSS Fonts 4, section 5.2: "For variable fonts with a slnt axis, a match is
/// created by setting the slnt value with the specified oblique value."
/// OpenType counts `slnt` counter-clockwise, so it is the angle negated, and
/// the axis clamps it: `oblique 30deg` on an axis that stops at -15 sets it
/// to -15, the nearest oblique value the face has, as the search through
/// oblique values below the one asked for would find. What the face draws
/// there is its own design: the Recursive fixture's `avar` gives one slope,
/// -0.2499, from -14 to -15. An angle of the sign
/// the axis does not reach clamps to upright, and that run is sheared
/// instead -- see [`run_shear`].
///
/// `font-weight` is a <number>, and OpenType defines `wght` on its scale, so
/// `512.5` is instanced at 512.5. A whole weight needs nothing here: the
/// instance takes `wght` from the matched style's weight, which a fraction
/// does not survive -- Skia's `FontStyle` holds an integer -- and the
/// fraction alone is carried to this point beside it.
pub(crate) fn with_style_axes(
    variations: &[(FourByteTag, f32)],
    angle: Option<f32>,
    weight: Option<f32>,
) -> Cow<'_, [(FourByteTag, f32)]> {
    match (angle, weight) {
        (None, None) => Cow::Borrowed(variations),
        _ => Cow::Owned(
            angle
                .map(|angle| (SLNT, -angle))
                .into_iter()
                .chain(weight.map(|weight| (WGHT, weight)))
                .chain(variations.iter().copied())
                .collect(),
        ),
    }
}

/// The ranges of `paragraph` whose face wants a different style from
/// `base`, each with the style `synthesis_style` gives it, or none where
/// every run is in a face `base` already suits.
///
/// Read from `Paragraph::get_fonts`, which lists each shaped run's font and
/// its text range: no glyph is looked at, and a run of marks alone, with no
/// advance, is a run like any other. Finding a run's text from its glyphs
/// cost more and missed that one -- the cluster under the middle of its end
/// glyphs, where a run with no width has none.
fn runs_to_restyle(
    paragraph: &Paragraph,
    requested: FontStyle,
    base: FontStyle,
    oblique: Option<f32>,
) -> Vec<(Range<usize>, FontStyle)> {
    paragraph
        .get_fonts()
        .into_iter()
        .filter_map(|run| {
            let face = run.font.typeface();
            let leans = oblique.map(|angle| leans_as_asked(&face, angle));
            let style = synthesis_style(requested, face.font_style(), leans);
            (style != base).then_some((run.text_range, style))
        })
        .collect()
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

/// The decoration's thickness in pixels, where one was set, resolved
/// against `state`: a thickness in `ex` or `ch` needs the face the state
/// resolves to, which a run laid out again later no longer has to hand.
fn decoration_thickness(state: &State) -> Option<f32> {
    state
        .text
        .text_decoration
        .size
        .as_ref()
        .map(|size| size.in_px(&state.font_units()))
}

impl<'a> Typesetter<'a> {
    pub fn new(state: &State, text: &'a str, width: Option<f32>) -> Self {
        Self::from_parts(
            state.typography(),
            &state.text.variations,
            decoration_thickness(state),
            state.text.oblique_angle,
            state.text.weight,
            text,
            width,
        )
    }

    /// A typesetter for `text` in `typography` and `variations`, rather than
    /// in a context's state as it stands, with its faces from the fonts
    /// registered now.
    fn from_parts(
        typography: Rc<Typography>,
        variations: &[(FourByteTag, f32)],
        decoration_thickness: Option<f32>,
        oblique: Option<f32>,
        weight: Option<f32>,
        text: &'a str,
        width: Option<f32>,
    ) -> Self {
        // The matched style comes back with the collection, from the one
        // search that found it. `layout` used to search again for it, on
        // every call, and it is the same search.
        let kept = typography.clone();
        let mut typography = typography;
        let hinting = typography.1.hinting_is_on();
        let variations = with_style_axes(variations, oblique, weight);
        let resolve = |style: &TextStyle| {
            FontLibrary::with_shared(|lib| {
                lib.set_hinting(hinting).fonts_for_style(style, &variations)
            })
        };
        let faces = match oblique {
            // An oblique may lay out in the upright style, which changes the
            // style for this run alone: a copy of its own, and the kept one
            // untouched.
            Some(_) => resolve_for_oblique(
                &mut Rc::make_mut(&mut typography).0,
                oblique,
                resolve,
            ),
            None => resolve(&typography.0),
        };
        let fonts = FontLibrary::with_shared(|lib| lib.epoch());
        Self::from_resolved(
            (typography, kept),
            decoration_thickness,
            oblique,
            faces,
            fonts,
            text,
            width,
        )
    }

    /// A typesetter laying `text` out in `faces`, the collection a search
    /// for `typography`'s style found and the style it matched first, built
    /// over the fonts of `fonts`.
    fn from_resolved(
        (typography, kept): (Rc<Typography>, Rc<Typography>),
        decoration_thickness: Option<f32>,
        oblique: Option<f32>,
        (typefaces, matched_style): (FontCollection, Option<FontStyle>),
        fonts: Rc<FontEpoch>,
        text: &'a str,
        width: Option<f32>,
    ) -> Self {
        let (_, _, _, baseline, text_wrap) = *typography;
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

        // Asked of the face itself, which only an `oblique` needs: the style
        // alone cannot say whether a face leans by design, since CoreText
        // reports an oblique face as italic -- see `leans_as_asked`.
        let matched_leans = oblique.map(|angle| {
            let char_style = &typography.0;
            let families = char_style.font_families();
            let families: Vec<&str> = families.iter().collect();
            typefaces
                .clone()
                .find_typefaces(&families, char_style.font_style())
                .first()
                .is_some_and(|face| leans_as_asked(face, angle))
        });

        Typesetter {
            text,
            width,
            condense_to,
            baseline,
            typefaces,
            fonts,
            matched_style,
            oblique,
            matched_leans,
            typography,
            kept,
            decoration_thickness,
            text_wrap,
        }
    }

    /// The run's text style.
    fn char_style(&self) -> &TextStyle {
        &self.typography.0
    }

    /// The paragraph style the run is laid out in.
    fn graf_style(&self) -> &ParagraphStyle {
        &self.typography.1
    }

    /// The decoration the run is drawn with.
    fn text_decoration(&self) -> &DecorationStyle {
        &self.typography.2
    }

    /// Lays the run out, and works out how far it has to be squeezed.
    pub fn layout(&self, paint: &Paint) -> TextLayout {
        let (paragraph, restyled) = self.restyled(&self.layout_style(paint));

        let offset = Point::new(
            self.alignment_offset() + self.trailing_space_shift(&paragraph),
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
            restyled,
            oblique: self.oblique,
        }
    }

    /// The run's decoration, or `None` where it has no lines: in its own
    /// colour where it names one, its alpha times `global_alpha` as the
    /// text's paint has it, and otherwise in the text's paint's colour.
    ///
    /// The whole run is one decorating box, so its lines are placed by its
    /// first available font -- see [`FaceLines::first_available`] -- at the
    /// run's size and axes, and a run another face draws for want of a
    /// glyph is underlined at the same height.
    pub fn decoration(
        &self,
        global_alpha: f32,
    ) -> Option<decorate::Decoration> {
        let decoration = &self.text_decoration().decoration;
        if decoration.ty == TextDecoration::NO_DECORATION {
            return None;
        }
        let families = self.char_style().font_families();
        let families: Vec<&str> = families.iter().collect();
        let face = FaceLines::first_available(
            &mut self.typefaces.clone(),
            Some(&self.fonts),
            &families,
            self.char_style().font_style(),
            self.char_style().font_size(),
            self.char_style().font_arguments(),
        )?;
        Some(decorate::Decoration {
            lines: decoration.ty,
            style: decoration.style,
            thickness: self
                .decoration_thickness
                .map_or(Thickness::Font, Thickness::Pixels),
            color: self.text_decoration().color,
            opacity: global_alpha,
            face,
        })
    }

    /// The run kept for drawing what this typesetter lays out: the one kept
    /// already, or one laid out now and kept where the run has been drawn
    /// before -- see [`LaidOut`] and [`LAID_OUT`]. `None` the first time,
    /// for the draw to lay the run out for its own paint with
    /// [`Typesetter::drawn_once`].
    ///
    /// Not on the first draw, because keeping a run costs more than drawing
    /// it once: laid out with no paint, its runs read back and its blobs
    /// built. A page of strings each drawn once took a fifth longer kept
    /// than drawn plainly, and holds nothing anyone draws again.
    pub fn laid_out(&self) -> Option<Rc<LaidOut>> {
        let found = LAID_OUT.with(|cache| {
            let mut cache = cache.borrow_mut();
            let kept = cache.get(self);
            kept.ok_or_else(|| cache.admits(self))
        });
        match found {
            Ok(kept) => Some(kept),
            Err(false) => None,
            Err(true) => {
                // The opacity is the draw's, put in by `LaidOut::paint`.
                let laid_out = Rc::new(LaidOut::new(
                    self.layout(&Paint::default()),
                    self.decoration(1.0),
                ));
                LAID_OUT.with(|cache| {
                    cache.borrow_mut().insert(self, laid_out.clone())
                });
                Some(laid_out)
            }
        }
    }

    /// The run laid out for one draw in `paint` and not kept, as a run is
    /// drawn the first time: by `Paragraph::paint` where nothing needs its
    /// runs, and from its runs, lines between them, where it is decorated
    /// or `oblique`.
    pub fn drawn_once(&self, paint: &Paint) -> DrawnRun {
        let layout = self.layout(paint);
        match (self.decoration(1.0), self.oblique) {
            (None, None) => DrawnRun::Plain(layout),
            (decoration, _) => DrawnRun::Runs(LaidOut::new(layout, decoration)),
        }
    }

    /// The run's advance -- every glyph and space, with the letter spacing
    /// after each and the word spacing after each word separator -- signed,
    /// as Chrome reports it for `measureText().width`. The rule is Blink's,
    /// from `text_metrics.cc`, which adds up the runs' advances into the
    /// width and does not clamp the sum; what follows is how it is reached
    /// through SkParagraph, which does.
    ///
    /// `max_intrinsic_width` is that advance wherever it is positive,
    /// trailing spaces included, which the line's own width leaves out. One
    /// that is not it reports as [`INTRINSIC_WIDTH_FLOOR`], where
    /// SkParagraph's `TextWrapper` starts its maximum before keeping the
    /// larger of that and each line. The
    /// advance is linear in the letter spacing, which every cluster carries
    /// once, so two layouts at spacings large enough to be positive give the
    /// slope -- the number of clusters -- and the line through them gives
    /// the advance at the spacing asked for. Each layout is restyled face by
    /// face as the drawing is, so the advance is of the weights drawn.
    fn signed_advance(&self, paragraph: &Paragraph) -> f32 {
        let natural = paragraph.max_intrinsic_width();
        if natural > INTRINSIC_WIDTH_FLOOR || self.text.is_empty() {
            return natural;
        }
        let letter = self.char_style().letter_spacing();
        // Enough to make either layout positive: a cluster loses at most
        // its letter and word spacing, and the font size on top keeps the
        // advance clear of zero for every cluster there is.
        let step = letter.abs()
            + self.char_style().word_spacing().abs()
            + self.char_style().font_size();
        let style = self.layout_style(&Paint::default());
        let at = |spacing: f32| {
            let mut style = style.clone();
            style.set_letter_spacing(spacing);
            self.restyled_paragraph(&style).max_intrinsic_width()
        };
        let (near, far) = (at(letter + step), at(letter + 2.0 * step));
        // A whole number of clusters, rounded off the float error of the
        // two widths' difference.
        let clusters = ((far - near) / step).round();
        near - clusters * step
    }

    /// The style the run is laid out in, painted with `paint`.
    fn layout_style(&self, paint: &Paint) -> TextStyle {
        let mut char_style = TextStyle::clone(self.char_style());
        char_style.set_foreground_paint(paint);

        // Let SkParagraph synthesise what Chrome synthesises and nothing
        // else -- see `synthesis_style`. The match was found once, when the
        // collection was chosen, rather than by searching it again here: the
        // search needed a `Vec` of the family names, a clone of the
        // collection and a lookup, 173 nanoseconds of every layout on this
        // machine, to arrive at what `fonts_for_style` had already had in
        // hand.
        if let Some(matched) = self.matched_style {
            char_style.set_font_style(synthesis_style(
                self.char_style().font_style(),
                matched,
                self.matched_leans,
            ));
        }
        char_style
    }

    /// The run laid out in `char_style`, which is the first face's style,
    /// and again with each run another face draws in the style for its own
    /// face.
    ///
    /// SkParagraph applies one style's weight to whatever face each run
    /// finds: at `900 48px Montserrat, Raleway`, Chrome draws the Latin as
    /// plain Montserrat 700 and the Cyrillic, which Montserrat lacks, as
    /// Raleway's 900 instance.
    fn restyled_paragraph(&self, char_style: &TextStyle) -> Paragraph {
        self.restyled(char_style).0
    }

    /// [`Typesetter::restyled_paragraph`], with the ranges it restyled.
    fn restyled(
        &self,
        char_style: &TextStyle,
    ) -> (Paragraph, Vec<(Range<usize>, FontStyle)>) {
        let first = self.paragraph_with(char_style, &[]);
        let restyled = runs_to_restyle(
            &first,
            self.char_style().font_style(),
            char_style.font_style(),
            self.oblique,
        );
        match restyled.is_empty() {
            true => (first, restyled),
            false => {
                let paragraph = self.paragraph_with(char_style, &restyled);
                (paragraph, restyled)
            }
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
            ParagraphBuilder::new(self.graf_style(), &self.typefaces);

        // Word spacing is added after every word-separator character, as CSS
        // Text 3 section 8.2 says, leading, doubled and trailing ones
        // included. SkParagraph's own adds it once per run of spaces and
        // only between words, so it is given none, and each separator is laid
        // out in a piece of its own with the word spacing added to its letter
        // spacing. Nothing else moves: spaced text measures its unspaced width
        // plus the spacing, which "width is the signed advance" in
        // `tests/suite/text.test.js` holds, a no-break space inside a word
        // included.
        //
        // With no word spacing there is nothing to add or to take off, and no
        // style is made for it.
        let word = char_style.word_spacing();
        let unspaced_style;
        let char_style = match word == 0.0 {
            true => char_style,
            false => {
                let mut style = char_style.clone();
                style.set_word_spacing(0.0);
                unspaced_style = style;
                &unspaced_style
            }
        };
        let unkerned = without_kerning(char_style);
        // The separators' styles, in words and between them.
        let spaced = (word != 0.0).then(|| {
            let mut style = char_style.clone();
            style.set_letter_spacing(char_style.letter_spacing() + word);
            let unkerned = without_kerning(&style);
            (style, unkerned)
        });
        let takes_word_spacing =
            |ch: char| spaced.is_some() && WORD_SEPARATORS.contains(&ch);

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
            let spacing = text[at..].starts_with(takes_word_spacing);
            let mut end = text[at..]
                .find(|ch| {
                    is_word_separator(ch) != separating
                        || takes_word_spacing(ch) != spacing
                })
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
            let mut piece_style = match spaced.as_ref().filter(|_| spacing) {
                Some((_, unkerned)) if separating => unkerned.clone(),
                Some((spaced, _)) => spaced.clone(),
                None if separating => unkerned.clone(),
                None => char_style.clone(),
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
            ..
        } = self.layout(&Paint::default());

        // Baseline offsets, relative to whatever `textBaseline` selected.
        let shift = self.char_style().baseline_shift();
        let hang = Baseline::Hanging.get_offset(self.char_style()) - shift;
        let norm = Baseline::Alphabetic.get_offset(self.char_style()) - shift;
        let ideo = Baseline::Ideographic.get_offset(self.char_style()) - shift;

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
                } = self.char_style().font_metrics();
                (norm - ascent, descent - norm)
            });

        // UPSTREAM: FreeType 2.14.3 -- unfiled -- not worked around
        // Re-check: bun run test:suite --test-name-pattern fraction
        //
        // The FreeType skia-bindings 0.156.0 embeds rounds each `HVAR`
        // advance delta to a whole font unit: `tt_var_get_item_delta` in
        // src/truetype/ttgxvar.c returns `ft_round_and_shift16`, `(value +
        // 0x8000) >> 16`, which `tt_hvadvance_adjust` adds to an integer
        // advance. So on Linux a variable face's advances move in whole
        // units between its masters -- Oswald's "0" at weight 450 is 526 at
        // 1000px, where its interpolation and CoreText give 525.52. OpenType
        // 1.9.1 rounds only the normalised coordinate and recommends "that
        // any rounding be done at the last point before a value is used".
        // No load flag avoids it, and neither Skia's scaler nor its HarfBuzz
        // font functions take an advance from anywhere else.
        //
        // An unwrapped run measures its advance, every glyph and space with
        // its spacing, signed -- what Chrome reports. Not the laid-out boxes:
        // those are sorted rectangles, so never negative, and they leave out
        // a trailing space's negative spacing, so `"a "` at -10px measured
        // 7.04 where the advance is 6.2. A wrapped run measures its laid-out
        // lines, since
        // the advance is what it would take unwrapped and would contradict
        // both the ink bounds beside it and the pixels drawn.
        let width = match self.text_wrap {
            false => self.signed_advance(&paragraph) * condensation,
            true => width,
        };

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
            hang,
            norm,
            ideo,
            char_ranges,
            oblique: self.oblique,
        };
        (extents, measured)
    }

    pub fn path(&mut self, point: impl Into<Point>) -> SkPath {
        let TextLayout {
            mut paragraph,
            offset: mut origin,
            condensation,
            ..
        } = self.layout(&Paint::default());
        let headroom = self.char_style().font_metrics().ascent
            + paragraph.alphabetic_baseline();
        let offset = self.baseline.get_offset(self.char_style());
        let anchor: Point = point.into();
        origin += anchor;
        origin.y -= headroom - offset;

        // Built a run at a time from `glyph_runs` rather than taken from
        // `Paragraph::get_path_at`, which places each glyph half its own
        // preceding kern to the right of where `Paragraph::paint` draws it --
        // so the path this returns used to fill differently from the text it
        // was taken from. `glyph_runs` holds the positions and their marker.
        //
        // One glyph outline at a time rather than through
        // `Paragraph::GetPath`, which applies a translation of its own: for
        // `"To"` it returned a path starting at x = 46.4 from a blob whose
        // own bounds start at -45.6. `Font::get_path` is the glyph in its own
        // em box, so the only offset applied here is the one this function
        // computed. A glyph with no outline -- a bitmap or colour emoji --
        // yields `None` and contributes nothing, which is what
        // `get_path_at`'s skipped-glyph count reported.
        let mut builder = SkPathBuilder::new();
        for run in glyph_runs(&mut paragraph) {
            // The shear the painter draws the run with, applied to each
            // outline in its own em box, where the baseline is y = 0, and to
            // its position, as `sheared_blob` applies it.
            let shear = run_shear(&run.font.typeface(), self.oblique);
            if shear == Shear::Flat {
                continue;
            }
            for (glyph, at) in zip(run.glyphs, run.positions) {
                if let Some(outline) = run.font.get_path(glyph) {
                    // Added where it goes, under its shear, in one call each:
                    // a sheared copy and then a moved one were two paths
                    // made and dropped for every glyph.
                    match &shear {
                        Shear::By(shear) => {
                            let to =
                                map_affine(shear, at) + run.origin + origin;
                            let placed = Matrix::from_affine(&[
                                shear.scale_x(),
                                shear.skew_y(),
                                shear.skew_x(),
                                shear.scale_y(),
                                to.x,
                                to.y,
                            ]);
                            builder.add_path_with_transform(
                                &outline, &placed, None,
                            );
                        }
                        Shear::None | Shear::Flat => {
                            builder.add_path_with_offset(
                                &outline,
                                at + run.origin + origin,
                                None,
                            );
                        }
                    }
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

    /// The alignment with `start` and `end` resolved to a side by the
    /// direction.
    fn gravity(&self) -> TextAlign {
        match (
            self.graf_style().text_direction(),
            self.graf_style().text_align(),
        ) {
            (TextDirection::LTR, TextAlign::Start)
            | (TextDirection::RTL, TextAlign::End) => TextAlign::Left,
            (TextDirection::LTR, TextAlign::End)
            | (TextDirection::RTL, TextAlign::Start) => TextAlign::Right,
            (_, alignment) => alignment,
        }
    }

    fn alignment_offset(&self) -> f32 {
        let gravity = self.gravity();

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

        alignment_factor * self.width + spacing_step * self.leading_spacing()
    }

    /// How far a centred or right-aligned run moves for the whitespace it
    /// ends in, which SkParagraph leaves out of the line it aligns.
    ///
    /// The canvas aligns the whole run, its advance with every trailing
    /// space and that space's spacing, as Chrome does: a centred `"a "`
    /// stands half a space left of `"a"`. SkParagraph aligns a line by its
    /// width without the whitespace it ends in, which hangs, so the run is
    /// moved by the share of the difference its alignment takes. A wrapped
    /// run's lines hang their trailing spaces as CSS lines do, and keep
    /// SkParagraph's alignment. Left to right only: a right-to-left run's
    /// trailing space is at its visual start, and no browser measurement
    /// says where Chrome puts it.
    fn trailing_space_shift(&self, paragraph: &Paragraph) -> f32 {
        let share = match self.gravity() {
            TextAlign::Center => 0.5,
            TextAlign::Right => 1.0,
            _ => return 0.0,
        };
        let applies = !self.text_wrap
            && self.graf_style().text_direction() == TextDirection::LTR
            && self.text.ends_with(char::is_whitespace);
        match applies {
            true => {
                -share
                    * (self.signed_advance(paragraph)
                        - paragraph.longest_line())
            }
            false => 0.0,
        }
    }

    /// The letter spacing of the line's first character on the left, of
    /// which Skia puts half before it: the run's own, and the word spacing
    /// on top where that character is a word separator -- see
    /// [`Typesetter::paragraph_with`].
    fn leading_spacing(&self) -> f32 {
        let letter = self.char_style().letter_spacing();
        let word = self.char_style().word_spacing();
        let leftmost = match self.graf_style().text_direction() {
            TextDirection::LTR => self.text.chars().next(),
            TextDirection::RTL => self.text.chars().next_back(),
        };
        match leftmost.is_some_and(|ch| WORD_SEPARATORS.contains(&ch)) {
            true => letter + word,
            false => letter,
        }
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
    pub families: Vec<FontFamily>,
    /// The names a font collection is asked for `families` by, which only
    /// the library resolving the specification can say -- see
    /// `FontLibrary::collection_names`. Empty until it has.
    pub collection_names: Vec<String>,
    pub size: f32,
    /// The shorthand's line height in pixels, a number or percentage
    /// already multiplied by `size`, or `None` for `normal`.
    pub line_height: Option<f32>,
    pub weight: Weight,
    pub width: Width,
    pub slant: Slant,
    /// The angle of an `oblique` other than the default 14deg, or `None`:
    /// [`FontSpec::slant`] holds no angle.
    pub oblique_degrees: Option<f32>,
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
    /// The weight as written where it is not a whole number, which
    /// [`FontSpec::weight`] cannot hold, or `None`. The face is matched by
    /// the whole weight and instanced at this -- see [`with_style_axes`].
    pub fractional_weight: Option<f32>,
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

    /// The angle an `oblique` specification is drawn at, or `None` where it
    /// is not oblique.
    pub fn oblique_angle(&self) -> Option<f32> {
        (self.slant == Slant::Oblique).then(|| {
            self.oblique_degrees
                .unwrap_or(ObliqueAngle::DEFAULT.degrees())
        })
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

pub fn from_slant(slant: Slant) -> String {
    match slant {
        Slant::Upright => "normal",
        Slant::Italic => "italic",
        Slant::Oblique => "oblique",
    }
    .to_string()
}

/// The width a `fontStretch` name asks for, or `None` for a name the
/// enumeration does not have. Matched exactly, as WebIDL matches an
/// enumeration's values: Chrome ignores `"CONDENSED"`.
pub fn to_width(width_name: &str) -> Option<Width> {
    Some(match width_name {
        "ultra-condensed" => Width::ULTRA_CONDENSED,
        "extra-condensed" => Width::EXTRA_CONDENSED,
        "condensed" => Width::CONDENSED,
        "semi-condensed" => Width::SEMI_CONDENSED,
        "semi-expanded" => Width::SEMI_EXPANDED,
        "expanded" => Width::EXPANDED,
        "extra-expanded" => Width::EXTRA_EXPANDED,
        "ultra-expanded" => Width::ULTRA_EXPANDED,
        "normal" => Width::NORMAL,
        _ => return None,
    })
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

/// Matched exactly, as WebIDL matches an enumeration's values: Chrome
/// ignores `"CENTER"` as it ignores any other name it does not have.
pub fn to_text_align(mode_name: &str) -> Option<CrateTextAlign> {
    let mode = match mode_name {
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

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Baseline {
    Top,
    Hanging,
    Middle,
    Alphabetic,
    Ideographic,
    Bottom,
}

/// Matched exactly, as [`to_text_align`] is.
pub fn to_text_baseline(mode_name: &str) -> Option<CrateTextBaseline> {
    let mode = match mode_name {
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

#[derive(Clone, Debug, PartialEq)]
pub struct DecorationStyle {
    pub css: String,
    pub decoration: Decoration,
    pub size: Option<Spacing>,
    /// The lines' own colour as named, in extended sRGB, unpremultiplied,
    /// or `None` for the text's.
    pub color: Option<Color4f>,
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

//
// Font-relative lengths (for text spacing & decoration thickness)
//
#[derive(Clone, Debug, PartialEq)]
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
    /// take one, or `None` for a string that is not one.
    ///
    /// Carries the unit through rather than resolving it, so a font-relative
    /// length is still relative after it is set: [`in_px`](Self::in_px)
    /// resolves it at the moment the text is laid out.
    pub fn from_css(css: &str) -> Option<Self> {
        // HTML's `letterSpacing` and `wordSpacing` setters parse "the given
        // value as a CSS <length>", and CSS Syntax 3's "parse a component
        // value" discards the whitespace on either side -- "Discard
        // whitespace from input", before the value and after it -- and its
        // tokenizer the comments, so `" 5px "` and `/**/5px` are 5px. Chrome
        // 152 ignores both; docs/browser-differences.md records it. Inside
        // the length nothing may come between the number and its unit, so
        // `5 px` and `5/**/px` stay refused.
        let uncommented = without_comments(css)?;
        let length = parse_length(uncommented.trim_ascii())?;
        // A number with no unit is a length only at zero. CSS Values 4
        // section 6: "For zero lengths the unit identifier is optional (i.e.
        // can be syntactically represented as the <number> 0)." Chrome
        // ignores `ctx.letterSpacing = "0"`; docs/browser-differences.md
        // records it.
        if length.unit.is_empty() {
            // `parse_length` takes a bare number only where it is written
            // as zero.
            return Some(Self::default());
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
    /// The hanging, alphabetic and ideographic baselines' offsets from the
    /// selected one.
    hang: f32,
    norm: f32,
    ideo: f32,
    /// Each line's text, in UTF-16 code units.
    char_ranges: Vec<Range<usize>>,
    /// The angle an `oblique` font is drawn at, which shears the ink of the
    /// runs it shears -- see [`run_shear`].
    oblique: Option<f32>,
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
            hang,
            norm,
            ideo,
            char_ranges,
            oblique,
        } = self;
        let (origin, condensation) = (*origin, *condensation);
        let (hang, norm, ideo) = (*hang, *norm, *ideo);

        // Each run's glyph bounds, with the family and font metrics
        // alongside, which is what makes the per-run detail below reportable
        // rather than a second walk.
        //
        // Sized for one run a line, which is what a measurement of one font
        // has, so the common case allocates once and never grows.
        let mut run_bounds: Vec<RunBound> =
            Vec::with_capacity(paragraph.line_number());
        let oblique = *oblique;
        // `visit` rather than `extended_visit`, which reports each glyph
        // without the offset the shaper gave it -- see `glyph_runs`.
        paragraph.visit(|line, visit| {
            if let Some(info) = visit {
                let shear = run_shear(&info.font().typeface(), oblique);
                let font = info.font();
                // `info.origin()` as reported, rounding and all. Its `y` is
                // the run's baseline snapped to a whole pixel -- 37 against
                // 36.960938 at 48px -- and `Paragraph::paint` draws with the
                // same snapped value, so a box measured without it would
                // describe ink this library does not put on the canvas. The
                // Canvas API asks for the box of the text as drawn, and that
                // is this one. Chrome does not round here and so reports
                // 0.039 more ascent at 48px; the difference is in the
                // drawing, not the measurement.
                let at = info.origin() + origin - Point::new(0.0, norm);
                // One call for the run: each call looks the glyphs' cache up
                // again, so asking glyph by glyph costs more than every box.
                let mut reported = vec![Rect::new_empty(); info.glyphs().len()];
                font.get_bounds(info.glyphs(), &mut reported, None);
                let upright = match shear {
                    Shear::None => glyph_inks(font, info.glyphs()),
                    Shear::By(_) | Shear::Flat => vec![None; reported.len()],
                };
                run_bounds.push(RunBound {
                    line,
                    bounds: zip(info.positions(), zip(info.glyphs(), reported))
                        .zip(upright)
                        .filter_map(|((&pt, (&glyph, reported)), upright)| {
                            let ink = glyph_box(
                                font, glyph, reported, upright, &shear,
                            )?;
                            // A sheared glyph's position is sheared with it.
                            let pt = match &shear {
                                Shear::By(shear) => map_affine(shear, pt),
                                Shear::None | Shear::Flat => pt,
                            };
                            Some(ink.with_offset(pt + at))
                        })
                        .reduce(union)
                        .unwrap_or(Rect::new_empty()),
                    family: family_of(&font.typeface()),
                    metrics: font.metrics().1,
                });
            }
        });

        // Each line's runs are the stretch of the list carrying its number,
        // which holds because `visit` reports them in line order.
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
                let baseline = line_metrics.baseline as f32 + origin.y;
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
    /// Union of the per-glyph inked bounds, baseline-relative -- reaching
    /// back on the left to the end of a negative advance, as Chrome's box
    /// does.
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
    typography: Rc<Typography>,
    variations: Vec<(FourByteTag, f32)>,
    /// Resolved when the run was measured -- see [`decoration_thickness`].
    decoration_thickness: Option<f32>,
    /// The angle an `oblique` font is drawn at -- see [`run_shear`].
    oblique: Option<f32>,
    /// A fractional weight the font is instanced at -- see
    /// [`with_style_axes`].
    weight: Option<f32>,
    text: String,
    width: Option<f32>,
    /// The fonts registered when the run was measured, so that laying it
    /// out again finds the faces it was measured in.
    fonts: Rc<FontEpoch>,
}

impl TypesetInputs {
    pub fn new(state: &State, text: &str, width: Option<f32>) -> Self {
        // The upright choice an `oblique` makes is made here, once, with the
        // fonts registered now, so that laying the run out again lays out
        // what was measured -- see `resolve_for_oblique`.
        // Only for an oblique: the faces it resolves are not kept, so for
        // any other font the search would be work for nothing.
        let mut typography = state.typography();
        if let Some(angle) = state.text.oblique_angle {
            let hinting = typography.1.hinting_is_on();
            let variations = with_style_axes(
                &state.text.variations,
                Some(angle),
                state.text.weight,
            );
            let style = &mut Rc::make_mut(&mut typography).0;
            resolve_for_oblique(style, Some(angle), |style| {
                FontLibrary::with_shared(|lib| {
                    lib.set_hinting(hinting).fonts_for_style(style, &variations)
                })
            });
        }
        Self {
            typography,
            variations: state.text.variations.clone(),
            decoration_thickness: decoration_thickness(state),
            oblique: state.text.oblique_angle,
            weight: state.text.weight,
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
            &with_style_axes(&self.variations, self.oblique, self.weight),
        );
        Typesetter::from_resolved(
            (self.typography.clone(), self.typography.clone()),
            self.decoration_thickness,
            self.oblique,
            faces,
            self.fonts.clone(),
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

/// Lets the paragraph kept for this measurement go with it.
///
/// A `TextMetrics` collected before its ink was read would otherwise leave
/// its paragraph, and the typeface instance that holds, among the recent
/// layouts until sixty-four newer measurements pushed it out -- which an
/// idle process never makes. Neon drops a boxed value on the JavaScript
/// thread when it finalizes it, the thread whose layouts these are; a
/// thread already tearing its storage down has nothing left to remove.
impl Drop for PendingInk {
    fn drop(&mut self) {
        let _ = RECENT_LAYOUTS.try_with(|recent| {
            let mut recent = recent.borrow_mut();
            if let Some(at) = recent.iter().position(|(id, _)| *id == self.id) {
                recent.remove(at);
            }
        });
    }
}

/// Lets every kept paragraph go, called by `FontLibrary::invalidate` when
/// the registered fonts change.
///
/// A kept paragraph pins the faces it was laid out in, which the change has
/// just replaced. Its measurement still has what it was laid out from, and
/// lays out again in those same faces if its ink is read -- see
/// [`PendingInk::ink`].
pub(crate) fn forget_kept_paragraphs() {
    RECENT_LAYOUTS.with(|recent| recent.borrow_mut().clear());
}

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
mod shear {
    use super::{Matrix, Shear, Slant, shear_for};

    const UPRIGHT_BY_DESIGN: fn(f32) -> bool = |_| false;
    const LEANS_BY_DESIGN: fn(f32) -> bool = |_| true;

    #[test]
    fn an_upright_face_is_sheared_by_the_tangent_of_the_angle() {
        let tan = |degrees: f32| degrees.to_radians().tan();
        for angle in [0.5f32, 10.0, 14.0, -14.0, 89.9] {
            assert_eq!(
                shear_for(Slant::Upright, UPRIGHT_BY_DESIGN, Some(angle)),
                Shear::By(Matrix::skew((-tan(angle), 0.0))),
                "{angle}deg"
            );
        }
        assert_eq!(
            shear_for(Slant::Upright, UPRIGHT_BY_DESIGN, None),
            Shear::None,
            "no oblique asked for"
        );
    }

    /// A face reported upright that leans by design is not sheared. Neither
    /// CoreText nor FreeType reports such a face -- both call it slanted --
    /// so this is the only place the second test of the two is observed.
    #[test]
    fn a_face_reported_upright_that_leans_already_is_not_sheared() {
        assert_eq!(
            shear_for(Slant::Upright, LEANS_BY_DESIGN, Some(10.0)),
            Shear::None
        );
        for slant in [Slant::Italic, Slant::Oblique] {
            assert_eq!(
                shear_for(slant, UPRIGHT_BY_DESIGN, Some(10.0)),
                Shear::None,
                "{slant:?}"
            );
        }
    }

    #[test]
    fn a_right_angle_lays_the_glyphs_flat() {
        for angle in [90.0, -90.0] {
            assert_eq!(
                shear_for(Slant::Upright, UPRIGHT_BY_DESIGN, Some(angle)),
                Shear::Flat,
                "{angle}deg"
            );
            assert_eq!(
                shear_for(Slant::Upright, LEANS_BY_DESIGN, Some(angle)),
                Shear::None,
                "a face that leans by design is drawn as it is at {angle}deg"
            );
        }
    }
}

#[cfg(test)]
mod measurement {
    use super::FAMILIES;
    use crate::{
        canvas::Canvas, context2d::Font, font::FontLibrary as Fonts,
        font_library::FontLibrary,
    };
    use skia_safe::{Data, FontMgr};

    #[test]
    fn a_change_to_the_fonts_forgets_the_families_of_the_faces_it_replaced() {
        // Each registration makes a typeface with an id of its own, and Skia
        // never hands an id out twice, so a family remembered for a face the
        // library has let go of can never be asked for again.
        const FAMILY: &str = "OswaldRegisteredAgain";
        let bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/assets/fonts/Oswald/Oswald-VariableFont_wght.ttf"
        ))
        .expect("bundled Oswald is readable");
        let mut canvas = Canvas::new(10.0, 10.0);
        for _ in 0..20 {
            let face = FontMgr::new()
                .new_from_data(Data::new_copy(&bytes), None)
                .expect("Oswald parses");
            FontLibrary::with_shared(|lib| {
                lib.register_typeface(face, Some(FAMILY.to_string()))
            });
            let ctx = canvas.context();
            ctx.set_font(&Font::new(FAMILY, 20.0));
            ctx.measure_text("Hamburgefonstiv", None);
        }
        assert_eq!(
            FAMILIES.with(|families| families.borrow().len()),
            1,
            "only the face registered now is remembered"
        );
    }

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
    fn a_measurement_let_go_of_takes_its_paragraph_with_it() {
        // A `TextMetrics` collected before its ink is read has no further
        // use for the paragraph kept for it.
        use super::RECENT_LAYOUTS;

        let mut canvas = Canvas::new(10.0, 10.0);
        let ctx = canvas.context();
        ctx.set_font(&Font::new("sans-serif", 24.0));
        let kept = ctx.inner.measure_text_lazily("Hamburgefonstiv", None).1;
        let dropped = ctx.inner.measure_text_lazily("Handgloves", None).1;
        let held = |id: u64| {
            RECENT_LAYOUTS.with(|recent| {
                recent.borrow().iter().any(|(held, _)| *held == id)
            })
        };
        let dropped_id = dropped.id;
        assert!(held(dropped_id));
        drop(dropped);
        assert!(!held(dropped_id), "the paragraph went with its measurement");
        assert!(held(kept.id), "and no other one did");
    }

    #[test]
    fn a_change_to_the_fonts_lets_every_kept_paragraph_go() {
        // A kept paragraph pins the faces it was laid out in, which a change
        // to the fonts has replaced; the measurement lays out again from
        // what it was measured with instead, in the same faces.
        use super::RECENT_LAYOUTS;

        const FAMILY: &str = "KeptParagraphsForgotten";
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
        let measured = ctx.inner.measure_text_extents(TEXT, None);
        let pending: Vec<_> = (0..8)
            .map(|_| ctx.inner.measure_text_lazily(TEXT, None).1)
            .collect();
        assert_eq!(RECENT_LAYOUTS.with(|recent| recent.borrow().len()), 8);

        fonts
            .register_font_from_path(
                FAMILY,
                path("Oswald/Oswald-VariableFont_wght.ttf"),
            )
            .expect("bundled Oswald registers");
        assert_eq!(
            RECENT_LAYOUTS.with(|recent| recent.borrow().len()),
            0,
            "no paragraph of the replaced fonts is kept"
        );
        for each in &pending {
            let (ink, lines) = each.ink().expect("not yet read");
            assert_eq!(ink, measured.ink);
            assert_eq!(lines, measured.line_details);
        }
    }

    #[test]
    fn an_idle_release_lets_every_kept_paragraph_go() {
        // What the idle watcher posts to a thread empties the kept paragraphs
        // with the collections' caches, and each measurement lays out again
        // for its ink, in the faces it was measured in.
        use super::RECENT_LAYOUTS;

        const FAMILY: &str = "KeptParagraphsReleased";
        const TEXT: &str = "Hamburgefonstiv";
        let fonts = Fonts::new();
        fonts
            .register_font_from_path(
                FAMILY,
                format!(
                    "{}/tests/assets/fonts/AmstelvarAlpha-VF.ttf",
                    env!("CARGO_MANIFEST_DIR")
                ),
            )
            .expect("bundled Amstelvar registers");
        let mut canvas = Canvas::new(10.0, 10.0);
        let ctx = canvas.context();
        ctx.set_font(&Font::new(FAMILY, 24.0));
        let measured = ctx.inner.measure_text_extents(TEXT, None);
        let pending: Vec<_> = (0..8)
            .map(|_| ctx.inner.measure_text_lazily(TEXT, None).1)
            .collect();
        assert_eq!(RECENT_LAYOUTS.with(|recent| recent.borrow().len()), 8);

        FontLibrary::with_shared(FontLibrary::release_caches);
        assert_eq!(
            RECENT_LAYOUTS.with(|recent| recent.borrow().len()),
            0,
            "no paragraph is kept past a release"
        );
        for each in &pending {
            let (ink, lines) = each.ink().expect("not yet read");
            assert_eq!(ink, measured.ink);
            assert_eq!(lines, measured.line_details);
        }
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

        // Kept, so that their paragraphs push the first one out: one let go
        // of takes its paragraph with it.
        let _newer: Vec<_> = (0..PENDING_LAYOUTS)
            .map(|_| ctx.inner.measure_text_lazily(TEXT, None).1)
            .collect();
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

    /// Bundled Raleway at 480px.
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

    #[test]
    fn a_glyphs_remembered_ink_is_its_outlines_at_that_size() {
        // The bounds come back from the table the second time, and have to
        // be what the outline gives -- at the size asked for, not at the
        // size the glyph was first measured at, and for the glyph asked
        // for, not another sharing its slot.
        use super::{FACE_INK, INK_FACES, INK_SLOTS, glyph_inks};

        let font = raleway();
        let ids: Vec<u16> = font.str_to_glyphs_vec("Hamburgefonstiv");
        let outline = |font: &skia_safe::Font, glyph: u16| {
            font.get_path(glyph)
                .map(|path| path.compute_tight_bounds())
                .filter(|bounds| !bounds.is_empty())
        };
        let outlines = |font: &skia_safe::Font, glyphs: &[u16]| {
            glyphs
                .iter()
                .map(|&glyph| outline(font, glyph))
                .collect::<Vec<_>>()
        };
        let mut half = font.clone();
        half.set_size(font.size() / 2.0);
        for _ in 0..2 {
            assert_eq!(glyph_inks(&font, &ids), outlines(&font, &ids));
            assert_eq!(glyph_inks(&half, &ids), outlines(&half, &ids));
        }
        assert_ne!(
            glyph_inks(&font, &ids[..1]),
            glyph_inks(&half, &ids[..1]),
            "two sizes of a glyph are two faces"
        );

        // Two glyphs in one slot, each asked for after the other.
        let (a, b) = (ids[0], ids[0] + INK_SLOTS as u16);
        for _ in 0..2 {
            assert_eq!(glyph_inks(&font, &[a, b]), outlines(&font, &[a, b]));
        }

        let spaces = font.str_to_glyphs_vec(" ");
        assert_eq!(glyph_inks(&font, &spaces), [None], "a space has no ink");

        // More sizes of the face than the faces held.
        let mut sized = font.clone();
        for step in 0..(INK_FACES + 4) {
            sized.set_size(10.0 + step as f32);
            glyph_inks(&sized, &ids);
        }
        assert_eq!(
            FACE_INK.with(|faces| faces.borrow().len()),
            INK_FACES,
            "the faces stay at their bound"
        );
    }

    #[test]
    fn every_size_of_a_variable_face_is_named_once() {
        // Each size of a face with an `opsz` axis is an instance of its own,
        // with an id of its own. The family belongs to the font, so a
        // measurement at a size not seen before reuses it rather than asking
        // the instance's name table again.
        const FAMILY: &str = "AmstelvarNamedOnce";
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
            FAMILIES.with(|families| families.borrow().len()),
            1,
            "twenty sizes of one face look its family up once"
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
            let got = synthesis_style(asked, style(400, face), None);
            assert_eq!(got.slant(), slant, "{face:?}");
        }
        // An upright request takes the face as it is.
        let upright = style(400, Slant::Upright);
        let got = synthesis_style(upright, style(400, Slant::Oblique), None);
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
                    None,
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
                None,
            );
            assert_eq!(*got.weight(), handed, "asked {asked}, face {face}");
        }
    }
}

#[cfg(test)]
mod hinting {
    use super::{FontLibrary, TextStyle, Typesetter, glyph_runs};
    use crate::{
        Canvas, Font as CanvasFont, FontLibrary as Library, RichTextSpan,
        TextEngine, TextRendering, TextStyle as SpanStyle,
        node::oblique::{ParagraphRecipe, StyleColours},
    };
    use skia_safe::{FontHinting, Paint, textlayout::ParagraphStyle};

    /// The bundled Oswald, under a name of these tests' own.
    const FAMILY: &str = "HintingOswald";
    const SIZE: f32 = 13.0;
    const TEXT: &str = "Hamburgefonstiv";

    fn register() -> Library {
        let library = Library::new();
        library
            .register_font_from_path(
                FAMILY,
                concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/tests/assets/fonts/Oswald/Oswald-VariableFont_wght.ttf"
                ),
            )
            .expect("bundled Oswald registers");
        library
    }

    /// The hinting of every glyph run canvas text is drawn with, its
    /// `fontHinting` set where `hinted` is given.
    fn canvas_hinting(hinted: Option<bool>) -> Vec<FontHinting> {
        let mut canvas = Canvas::new(200.0, 50.0);
        let ctx = canvas.context();
        ctx.set_font(&CanvasFont::new(FAMILY, SIZE));
        if let Some(hinted) = hinted {
            ctx.set_font_hinting(hinted);
        }
        let mut layout = Typesetter::new(&ctx.inner.state, TEXT, None)
            .layout(&Paint::default());
        glyph_runs(&mut layout.paragraph)
            .iter()
            .map(|run| run.font.hinting())
            .collect()
    }

    /// The hinting of every glyph run a paragraph is drawn with.
    fn paragraph_hinting(engine: &TextEngine) -> Vec<FontHinting> {
        let style = SpanStyle {
            font_families: vec![FAMILY.into()],
            font_size: SIZE,
            ..SpanStyle::default()
        };
        let mut paragraph = engine.layout_rich_text(
            &[RichTextSpan {
                text: TEXT.to_string(),
                style: style.clone(),
            }],
            &style,
            1000.0,
        );
        glyph_runs(paragraph.paragraph.get_mut())
            .iter()
            .map(|run| run.font.hinting())
            .collect()
    }

    fn all(hinting: &[FontHinting], expected: FontHinting, what: &str) {
        assert!(!hinting.is_empty(), "{what}: no runs");
        assert!(
            hinting.iter().all(|h| *h == expected),
            "{what}: {hinting:?}, expected {expected:?}"
        );
    }

    /// `geometricPrecision` lays every run out unhinted, whatever
    /// `fontHinting` says: one hinting level for the whole layout. Asking the
    /// collection for unhinted faces while the run style still carries Slight
    /// would draw unhinted metrics with hinted glyphs.
    #[test]
    fn geometric_precision_lays_every_run_out_unhinted() {
        register();
        let mut canvas = Canvas::new(200.0, 50.0);
        let ctx = canvas.context();
        ctx.set_font(&CanvasFont::new(FAMILY, SIZE));
        ctx.set_font_hinting(true);
        ctx.set_text_rendering(TextRendering::GeometricPrecision);
        let mut layout = Typesetter::new(&ctx.inner.state, TEXT, None)
            .layout(&Paint::default());
        let hinting: Vec<FontHinting> = glyph_runs(&mut layout.paragraph)
            .iter()
            .map(|run| run.font.hinting())
            .collect();
        all(&hinting, FontHinting::None, "geometricPrecision");
    }

    #[test]
    fn canvas_text_is_hinted_unless_turned_off() {
        register();
        all(&canvas_hinting(None), FontHinting::Slight, "by default");
        all(
            &canvas_hinting(Some(false)),
            FontHinting::None,
            "turned off",
        );
        all(
            &canvas_hinting(Some(true)),
            FontHinting::Slight,
            "turned on",
        );
    }

    /// The hinting of every glyph run a paragraph from the Node binding's
    /// path is drawn with: its collection from the shared library, as
    /// `node::paragraph::new` takes it.
    fn shared_paragraph_hinting() -> Vec<FontHinting> {
        let mut style = TextStyle::new();
        style.set_font_families(&[FAMILY]).set_font_size(SIZE);
        let (collection, _) = FontLibrary::with_shared(|lib| {
            lib.set_hinting(true).fonts_for_style(&style, &[])
        });
        let mut paragraph_style = ParagraphStyle::new();
        paragraph_style.set_text_style(&style);
        let none = StyleColours::default();
        let mut recipe = ParagraphRecipe::new(
            paragraph_style,
            None,
            collection,
            None,
            &none,
            false,
        );
        recipe.push_style(style, None, &none);
        recipe.add_text(TEXT);
        recipe.pop();
        let (mut paragraph, _) = recipe.build();
        paragraph.layout(1000.0);
        glyph_runs(&mut paragraph)
            .iter()
            .map(|run| run.font.hinting())
            .collect()
    }

    /// Canvas text drawn unhinted and a paragraph of the same text, hinted,
    /// share a library: each keeps its own hinting in either order. The
    /// paragraph cache matches the two without comparing hinting, so laid
    /// out against one collection the second takes the first's runs.
    #[test]
    fn hinted_and_unhinted_text_keep_their_own_hinting() {
        register();
        for canvas_first in [true, false] {
            let mut seen = Vec::new();
            for step in 0..4 {
                let canvas = (step % 2 == 0) == canvas_first;
                let hinting = match canvas {
                    true => canvas_hinting(Some(false)),
                    false => shared_paragraph_hinting(),
                };
                let expected = match canvas {
                    true => FontHinting::None,
                    false => FontHinting::Slight,
                };
                seen.push(hinting.clone());
                all(
                    &hinting,
                    expected,
                    &format!(
                        "step {step}, canvas first {canvas_first}: {seen:?}"
                    ),
                );
            }
        }
    }

    #[test]
    fn a_paragraph_is_hinted() {
        let engine = TextEngine::new(&register());
        all(
            &paragraph_hinting(&engine),
            FontHinting::Slight,
            "a paragraph",
        );
    }
}

#[cfg(test)]
mod laid_out {
    use super::{
        COLLIDING, LAID_OUT, LAID_OUT_BYTES, LAID_OUT_COUNT, LaidOutCache,
        LaidOutKey, SEEN_ONCE, forget_laid_out, laid_out_held,
    };
    use crate::{
        Canvas, Context2D, FontLibrary as Library, GradientInterpolation,
        GradientStop, RgbaLinear, Shader, geometry::Point,
        node::font_library::FontLibrary,
    };
    use std::ptr;

    const OSWALD: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/assets/fonts/Oswald/Oswald-VariableFont_wght.ttf"
    );
    const AMSTELVAR: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/assets/fonts/AmstelvarAlpha-VF.ttf"
    );
    const TEXT: &str = "Hamburgefonstiv AV";

    /// A change made to a context before it draws.
    type Setup = Box<dyn Fn(&mut Context2D)>;

    /// A family of these tests' own, in `file`.
    fn register(family: &str, file: &str) {
        Library::new()
            .register_font_from_path(family, file)
            .expect("a bundled face registers");
    }

    /// The canvas's pixels after `draw` on `ctx`, once.
    fn drawn_once(
        canvas: &mut Canvas,
        draw: impl Fn(&mut Context2D),
    ) -> Vec<u8> {
        let ctx = canvas.context();
        ctx.clear_rect(0.0, 0.0, 400.0, 120.0);
        draw(ctx);
        ctx.get_image_data(0.0, 0.0, 400.0, 120.0)
            .expect("the canvas reads back")
            .pixels()
            .to_vec()
    }

    /// The canvas's pixels after `draw` on `ctx` twice, so that what it
    /// draws is kept -- see `Typesetter::laid_out`.
    fn drawn(canvas: &mut Canvas, draw: impl Fn(&mut Context2D)) -> Vec<u8> {
        drawn_once(canvas, &draw);
        drawn_once(canvas, draw)
    }

    /// What a context given `setup` from the start draws for `draw`, the
    /// first time, which keeps nothing: the oracle a kept run is held to.
    fn fresh(
        setup: impl Fn(&mut Context2D),
        draw: impl Fn(&mut Context2D),
    ) -> Vec<u8> {
        let mut canvas = Canvas::new(400.0, 120.0);
        setup(canvas.context());
        drawn_once(&mut canvas, draw)
    }

    fn fill(
        text: &'static str,
        width: Option<f32>,
    ) -> impl Fn(&mut Context2D) + Copy {
        move |ctx: &mut Context2D| ctx.fill_text(text, 10.0, 70.0, width)
    }

    /// Draws `first`, changes `change`, draws `second`: the second has to be
    /// a run of its own, drawn as a fresh context draws it.
    fn changed(
        family: &str,
        setup: impl Fn(&mut Context2D) + Copy,
        first: impl Fn(&mut Context2D),
        change: impl Fn(&mut Context2D) + Copy,
        second: impl Fn(&mut Context2D) + Copy,
    ) {
        forget_laid_out();
        let font = format!("40px {family}");
        let mut canvas = Canvas::new(400.0, 120.0);
        assert!(canvas.context().set_font_css(&font));
        setup(canvas.context());
        let before = drawn(&mut canvas, first);
        change(canvas.context());
        let after = drawn(&mut canvas, second);
        let wanted = fresh(
            |ctx| {
                assert!(ctx.set_font_css(&font));
                setup(ctx);
                change(ctx);
            },
            second,
        );
        assert_ne!(after, before, "the change shows");
        assert!(after == wanted, "drawn as a fresh context draws it");
    }

    #[test]
    fn a_run_drawn_again_in_another_paint_is_laid_out_once() {
        // Colour, gradient and alpha change between draws of one run, and
        // each draw is the one a fresh context makes in that paint.
        const FAMILY: &str = "LaidOutPaints";
        register(FAMILY, OSWALD);
        let gradient = Shader::linear_gradient(
            Point::new(0.0, 0.0),
            Point::new(400.0, 0.0),
            &[
                GradientStop {
                    position: 0.0,
                    color: RgbaLinear::opaque(1.0, 0.0, 0.0),
                },
                GradientStop {
                    position: 1.0,
                    color: RgbaLinear::opaque(0.0, 0.2, 1.0),
                },
            ],
            GradientInterpolation::default(),
        )
        .expect("a gradient");
        let paints: [Setup; 4] = [
            Box::new(|ctx| ctx.set_fill_style_css("#203040").expect("c")),
            Box::new(move |ctx| ctx.set_fill_shader(&gradient)),
            Box::new(|ctx| {
                ctx.set_fill_style_css("green").expect("c");
                ctx.set_global_alpha(0.5);
            }),
            Box::new(|ctx| {
                ctx.set_fill_style_css("rgba(200, 30, 90, 0.6)").expect("c")
            }),
        ];
        let font = format!("40px {FAMILY}");
        let wanted: Vec<_> = paints
            .iter()
            .map(|paint| {
                fresh(
                    |ctx| {
                        assert!(ctx.set_font_css(&font));
                        paint(ctx);
                    },
                    fill(TEXT, None),
                )
            })
            .collect();
        forget_laid_out();
        let mut canvas = Canvas::new(400.0, 120.0);
        assert!(canvas.context().set_font_css(&font));
        for (at, paint) in paints.iter().enumerate() {
            paint(canvas.context());
            let kept = drawn(&mut canvas, fill(TEXT, None));
            canvas.context().set_global_alpha(1.0);
            assert!(kept == wanted[at], "paint {at}: as a fresh context draws");
        }
        assert_eq!(laid_out_held().0, 1, "one run, laid out once");
    }

    #[test]
    fn another_text_is_a_run_of_its_own() {
        const FAMILY: &str = "LaidOutText";
        register(FAMILY, OSWALD);
        changed(
            FAMILY,
            |_| {},
            fill("Hamburg", None),
            |_| {},
            fill("Handgloves", None),
        );
    }

    #[test]
    fn another_wrap_width_is_a_run_of_its_own() {
        const FAMILY: &str = "LaidOutWrap";
        register(FAMILY, OSWALD);
        changed(
            FAMILY,
            |ctx| ctx.set_text_wrap(true),
            fill(TEXT, Some(160.0)),
            |_| {},
            fill(TEXT, Some(380.0)),
        );
    }

    #[test]
    fn another_condensing_width_is_a_run_of_its_own() {
        const FAMILY: &str = "LaidOutCondense";
        register(FAMILY, OSWALD);
        changed(
            FAMILY,
            |_| {},
            fill(TEXT, Some(160.0)),
            |_| {},
            fill(TEXT, Some(240.0)),
        );
    }

    #[test]
    fn a_changed_setting_is_a_run_of_its_own() {
        const FAMILY: &str = "LaidOutSetting";
        register(FAMILY, OSWALD);
        changed(
            FAMILY,
            |_| {},
            fill(TEXT, None),
            |ctx| ctx.set_letter_spacing_css("4px").expect("a spacing"),
            fill(TEXT, None),
        );
    }

    #[test]
    fn an_oblique_run_drawn_again_is_found_kept() {
        // The font set once and the run drawn three times: kept on the
        // second draw and found on the third, as an upright run is.
        const FAMILY: &str = "LaidOutObliqueAgain";
        register(FAMILY, OSWALD);
        forget_laid_out();
        let mut canvas = Canvas::new(400.0, 120.0);
        assert!(
            canvas
                .context()
                .set_font_css(&format!("oblique 10deg 40px {FAMILY}"))
        );
        for _ in 0..3 {
            drawn(&mut canvas, fill(TEXT, None));
        }
        assert_eq!(laid_out_held().0, 1, "one run, kept once and found");
    }

    #[test]
    fn another_oblique_angle_is_a_run_of_its_own() {
        const FAMILY: &str = "LaidOutOblique";
        register(FAMILY, OSWALD);
        changed(
            FAMILY,
            |ctx| {
                assert!(ctx.set_font_css("oblique 10deg 40px LaidOutOblique"))
            },
            fill(TEXT, None),
            |ctx| {
                assert!(ctx.set_font_css("oblique 30deg 40px LaidOutOblique"))
            },
            fill(TEXT, None),
        );
    }

    #[test]
    fn a_run_laid_out_before_a_font_change_is_not_drawn_after_it() {
        // The family registered again: the context's settings are the same,
        // and the run has to come out in the face that replaced the first.
        const FAMILY: &str = "LaidOutEpoch";
        register(FAMILY, OSWALD);
        changed(
            FAMILY,
            |_| {},
            fill(TEXT, None),
            |_| register(FAMILY, AMSTELVAR),
            fill(TEXT, None),
        );
    }

    #[test]
    fn a_decoration_takes_its_colour_from_the_paint_where_it_has_none() {
        // Underlined in the text colour: red, then blue, each its own lines.
        const FAMILY: &str = "LaidOutPaintLines";
        register(FAMILY, OSWALD);
        let font = format!("40px {FAMILY}");
        let colours = ["red", "blue"];
        let wanted = colours.map(|colour| {
            fresh(
                |ctx| {
                    assert!(ctx.set_font_css(&font));
                    ctx.set_text_decoration_css("underline").expect("d");
                    ctx.set_fill_style_css(colour).expect("c");
                },
                fill(TEXT, None),
            )
        });
        forget_laid_out();
        let mut canvas = Canvas::new(400.0, 120.0);
        let ctx = canvas.context();
        assert!(ctx.set_font_css(&font));
        ctx.set_text_decoration_css("underline").expect("d");
        let mut held = vec![];
        for (at, colour) in colours.iter().enumerate() {
            canvas.context().set_fill_style_css(colour).expect("c");
            let kept = drawn(&mut canvas, fill(TEXT, None));
            assert!(kept == wanted[at], "{colour}: as a fresh context draws");
            held.push(laid_out_held());
        }
        assert_eq!(held[1].0, 1, "one run");
        assert_eq!(held[0].1, held[1].1, "its lines drawn from the one box");
    }

    #[test]
    fn a_decoration_with_a_colour_of_its_own_is_laid_out_once() {
        // Green lines under red text and then blue: one decoration paragraph.
        const FAMILY: &str = "LaidOutOwnLines";
        register(FAMILY, OSWALD);
        let font = format!("40px {FAMILY}");
        let colours = ["red", "blue"];
        let wanted = colours.map(|colour| {
            fresh(
                |ctx| {
                    assert!(ctx.set_font_css(&font));
                    ctx.set_text_decoration_css("underline green").expect("d");
                    ctx.set_fill_style_css(colour).expect("c");
                },
                fill(TEXT, None),
            )
        });
        forget_laid_out();
        let mut canvas = Canvas::new(400.0, 120.0);
        let ctx = canvas.context();
        assert!(ctx.set_font_css(&font));
        ctx.set_text_decoration_css("underline green").expect("d");
        let mut held = vec![];
        for (at, colour) in colours.iter().enumerate() {
            canvas.context().set_fill_style_css(colour).expect("c");
            let kept = drawn(&mut canvas, fill(TEXT, None));
            assert!(kept == wanted[at], "{colour}: as a fresh context draws");
            held.push(laid_out_held());
        }
        assert_eq!(held[1].0, 1, "one run");
        assert_eq!(held[0].1, held[1].1, "the green lines are laid out once");
    }

    #[test]
    fn a_named_line_colour_takes_each_draw_s_global_alpha() {
        // Red lines, drawn opaque and then at half alpha from one run, in a
        // gap between glyphs that only the line covers: its alpha is the
        // draw's, which a fresh context drawing the same would get wrong in
        // the same way, so it is read off the pixels as well.
        const FAMILY: &str = "LaidOutLineAlpha";
        register(FAMILY, OSWALD);
        let font = format!("96px {FAMILY}");
        let gap =
            |ctx: &mut Context2D| ctx.fill_text("n      n", 10.0, 100.0, None);
        let alphas = [1.0, 0.5];
        let wanted = alphas.map(|alpha| {
            fresh(
                |ctx| {
                    assert!(ctx.set_font_css(&font));
                    ctx.set_text_decoration_css("underline 8px red")
                        .expect("d");
                    ctx.set_global_alpha(alpha);
                },
                gap,
            )
        });
        forget_laid_out();
        let mut canvas = Canvas::new(400.0, 120.0);
        let ctx = canvas.context();
        assert!(ctx.set_font_css(&font));
        ctx.set_text_decoration_css("underline 8px red").expect("d");
        for (at, alpha) in alphas.into_iter().enumerate() {
            canvas.context().set_global_alpha(alpha);
            let kept = drawn(&mut canvas, gap);
            assert!(kept == wanted[at], "{alpha}: as a fresh context draws");
            // The line's own pixels: red, where the glyphs are black.
            let line = kept
                .as_chunks::<4>()
                .0
                .iter()
                .filter(|px| px[0] > 200 && px[1] < 40 && px[2] < 40)
                .map(|px| px[3])
                .max()
                .expect("the line is drawn");
            let expected = (alpha * 255.0).round() as u8;
            assert!(
                line.abs_diff(expected) <= 1,
                "{alpha}: the line's alpha is {line}, not {expected}"
            );
        }
        assert_eq!(laid_out_held().0, 1, "one run");
    }

    #[test]
    fn another_decoration_thickness_is_a_run_of_its_own() {
        // The thickness reaches the typesetter apart from the typography,
        // resolved against the font: a run kept at one must not draw the
        // other.
        const FAMILY: &str = "LaidOutThickness";
        register(FAMILY, OSWALD);
        changed(
            FAMILY,
            |ctx| ctx.set_text_decoration_css("underline 0.05em").expect("d"),
            fill(TEXT, None),
            |ctx| ctx.set_text_decoration_css("underline 0.2em").expect("d"),
            fill(TEXT, None),
        );
    }

    /// Whether a run of `text` is kept.
    fn kept(text: &str) -> bool {
        LAID_OUT.with(|cache| {
            cache
                .borrow()
                .runs
                .values()
                .any(|entry| &*entry.key.text == text)
        })
    }

    #[test]
    fn past_the_count_the_least_recently_drawn_runs_go() {
        least_recently_drawn_go_past_the_count("LaidOutCount");
    }

    #[test]
    fn a_run_s_hash_cannot_be_worked_out_ahead_of_its_cache() {
        let digest = |cache: &LaidOutCache| {
            LaidOutKey::digest(&cache.keys, ptr::null(), TEXT, 400.0)
        };
        let (one, other) = (LaidOutCache::default(), LaidOutCache::default());
        assert_eq!(digest(&one), digest(&one), "one cache files a run alike");
        assert_ne!(
            digest(&one),
            digest(&other),
            "another cache, as another process, files the same run elsewhere"
        );
    }

    #[test]
    fn runs_whose_hashes_collide_are_found_and_let_go_as_any_other() {
        // Every run in one chain: each found among the others, and the
        // oldest cut out of the chain wherever it sits in it.
        COLLIDING.set(true);
        register("LaidOutCollidingText", OSWALD);
        changed(
            "LaidOutCollidingText",
            |_| {},
            fill("Hamburg", None),
            |_| {},
            fill("Handgloves", None),
        );
        least_recently_drawn_go_past_the_count("LaidOutCollidingCount");
        COLLIDING.set(false);
    }

    /// Draws one run every time and a new one each time, past the count.
    fn least_recently_drawn_go_past_the_count(family: &str) {
        register(family, OSWALD);
        forget_laid_out();
        let mut canvas = Canvas::new(400.0, 120.0);
        let ctx = canvas.context();
        assert!(ctx.set_font_css(&format!("12px {family}")));
        for at in 0..LAID_OUT_COUNT + 64 {
            ctx.fill_text("kept throughout", 0.0, 20.0, None);
            // Twice, for it to be kept.
            for _ in 0..2 {
                ctx.fill_text(&format!("run {at}"), 0.0, 40.0, None);
            }
        }
        assert_eq!(laid_out_held().0, LAID_OUT_COUNT);
        assert!(kept("kept throughout"), "drawn every time, it stays");
        LAID_OUT.with(|cache| {
            let cache = cache.borrow();
            let copies = cache
                .runs
                .values()
                .filter(|entry| &*entry.key.text == "kept throughout")
                .count();
            assert_eq!(copies, 1, "found each time rather than laid out again");
            assert!(
                cache.by_hash.len() <= cache.runs.len(),
                "no hash is left filed for a run that has gone"
            );
        });
        assert!(!kept("run 0"), "the first of the others has gone");
        assert!(kept(&format!("run {}", LAID_OUT_COUNT + 63)));
    }

    #[test]
    fn past_the_bytes_the_least_recently_drawn_runs_go() {
        const FAMILY: &str = "LaidOutBytes";
        register(FAMILY, OSWALD);
        forget_laid_out();
        let mut canvas = Canvas::new(400.0, 120.0);
        let ctx = canvas.context();
        assert!(ctx.set_font_css(&format!("12px {FAMILY}")));
        ctx.set_text_wrap(true);
        let long = "the quick brown fox jumps over the lazy dog ".repeat(8);
        for at in 0..200 {
            for _ in 0..2 {
                ctx.fill_text(&format!("{at} {long}"), 0.0, 20.0, Some(380.0));
            }
        }
        let (count, bytes) = laid_out_held();
        assert!(bytes <= LAID_OUT_BYTES, "{bytes} bytes held");
        assert!(count < 200, "{count} runs kept of 200");
        assert!(kept(&format!("199 {long}")) && !kept(&format!("0 {long}")));
    }

    #[test]
    fn a_font_change_and_the_idle_release_let_every_run_go() {
        const FAMILY: &str = "LaidOutForgotten";
        register(FAMILY, OSWALD);
        let mut canvas = Canvas::new(400.0, 120.0);
        let ctx = canvas.context();
        assert!(ctx.set_font_css(&format!("40px {FAMILY}")));
        drawn(&mut canvas, fill(TEXT, None));
        assert_eq!(laid_out_held().0, 1);
        register(FAMILY, AMSTELVAR);
        assert_eq!(laid_out_held().0, 0, "a registration");

        drawn(&mut canvas, fill(TEXT, None));
        assert_eq!(laid_out_held().0, 1);
        FontLibrary::with_shared(FontLibrary::release_caches);
        assert_eq!(laid_out_held().0, 0, "the idle release");
    }

    #[test]
    fn a_run_is_kept_from_its_second_draw() {
        const FAMILY: &str = "LaidOutSecond";
        register(FAMILY, OSWALD);
        forget_laid_out();
        let mut canvas = Canvas::new(400.0, 120.0);
        assert!(canvas.context().set_font_css(&format!("40px {FAMILY}")));
        let wanted = drawn_once(&mut canvas, fill(TEXT, None));
        assert_eq!(laid_out_held().0, 0, "a run drawn once is not kept");
        let again = drawn_once(&mut canvas, fill(TEXT, None));
        assert_eq!(laid_out_held().0, 1, "and is kept when drawn again");
        assert!(again == wanted, "drawn the same either way");
    }

    #[test]
    fn a_font_change_and_the_idle_release_forget_the_runs_drawn_once() {
        // Drawn once before each: the draw after is a first draw again.
        const FAMILY: &str = "LaidOutSeenForgotten";
        register(FAMILY, OSWALD);
        let mut canvas = Canvas::new(400.0, 120.0);
        assert!(canvas.context().set_font_css(&format!("40px {FAMILY}")));
        let forgets: [Setup; 2] = [
            Box::new(|_| register(FAMILY, OSWALD)),
            Box::new(|_| FontLibrary::with_shared(FontLibrary::release_caches)),
        ];
        for (at, forget) in forgets.iter().enumerate() {
            drawn_once(&mut canvas, fill(TEXT, None));
            forget(canvas.context());
            drawn_once(&mut canvas, fill(TEXT, None));
            assert_eq!(laid_out_held().0, 0, "forget {at}: not kept");
        }
    }

    #[test]
    fn past_the_runs_remembered_a_run_drawn_once_is_forgotten() {
        const FAMILY: &str = "LaidOutSeenBound";
        register(FAMILY, OSWALD);
        forget_laid_out();
        let mut canvas = Canvas::new(400.0, 120.0);
        let ctx = canvas.context();
        assert!(ctx.set_font_css(&format!("12px {FAMILY}")));
        ctx.fill_text("the first", 0.0, 20.0, None);
        for at in 0..SEEN_ONCE {
            ctx.fill_text(&format!("once {at}"), 0.0, 40.0, None);
        }
        ctx.fill_text("the first", 0.0, 20.0, None);
        assert!(!kept("the first"), "forgotten, so a first draw again");
        assert_eq!(laid_out_held().0, 0);
        ctx.fill_text(&format!("once {}", SEEN_ONCE - 1), 0.0, 40.0, None);
        assert!(
            kept(&format!("once {}", SEEN_ONCE - 1)),
            "where the last is not"
        );
    }
}
