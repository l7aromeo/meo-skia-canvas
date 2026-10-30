//! Text decoration lines, drawn here rather than by SkParagraph, for canvas
//! text and the Paragraph API alike.
//!
//! A line is placed where its decorating box's first available font says,
//! and grows the way CSS Text Decoration 4 says:
//!
//! - an underline's top is the `post` table's `underlinePosition`, "the top of
//!   the underline", and it grows down -- section 2.8.1, "extending its
//!   thickness in the positive direction only";
//! - an overline stands on the ascent and grows up;
//! - a line-through is the `OS/2` strikeout stroke, `yStrikeoutPosition` being
//!   "the top of the strikeout stroke", and a thickness other than the font's
//!   grows it equally about the stroke's centre, so that it still crosses the
//!   glyphs at the height the font meant -- growing from the top would move the
//!   crossing as it thickened.
//!
//! One position and thickness per box and line, from the box's first
//! available font, as section 2.9 asks: "the UA must use a single thickness
//! and position on each line for the decorations deriving from a single
//! decorating box". Underlines and overlines are painted beneath the glyphs
//! and line-through over them, section 5.1's order after CSS 2 Appendix E.
//!
//! UPSTREAM: skia-safe 0.156.0 -- unfiled -- worked around
//! Re-check: cargo test a_decoration_sits_where_its_font_says
//! with SkParagraph's own decorations painted in place of these.
//! SkParagraph's `Decorations::paint` centres each line on the position
//! `SkFontMetrics` documents as the "distance from baseline to top of
//! stroke", so every line sat half its thickness high, and it takes each
//! run's own font rather than the box's.

use crate::{
    cache_map::CacheMap,
    context::faint::Faint,
    node::{
        font_library::FontEpoch,
        typography::{GlyphRun, GlyphRunRef},
    },
};
use skia_safe::{
    Canvas, Color4f, Font, FontStyle, Paint, Path, PathBuilder, PathFillType,
    PathVerb, Point, Rect,
    paint::{Cap, Style},
    path_utils::fill_path_with_paint,
    textlayout::{
        FontArguments, FontCollection, TextDecoration, TextDecorationStyle,
    },
};
use std::{
    cell::RefCell,
    hash::{Hash, Hasher},
    ops::Range,
    ptr,
    rc::{Rc, Weak},
};

/// The thickness Skia gives a line where the font states none: a
/// fourteenth of the font size, SkParagraph's `calculateThickness` ("This is
/// how flutter calculates the thickness"). Kept so a face without the
/// tables decorates as it always did.
const FALLBACK_THICKNESS_PER_SIZE: f32 = 1.0 / 14.0;

/// A double line's second line is this many thicknesses beyond the first's
/// top: two lines of `t` with a gap of `t`. Chrome 153 leaves a fixed 1px
/// gap at every thickness, which at 8px reads as one line and does not
/// scale with the text; docs/browser-differences.md has the measurements.
const DOUBLE_OFFSET: f32 = 2.0;

/// Chrome 153's dotted pattern, measured at 4 and 8px: a dot `t` long, then
/// a gap of `t`, starting with a dot at the box's left edge.
const DOT_ON: f32 = 1.0;
const DOT_PERIOD: f32 = 2.0;

/// Chrome 153's dashed pattern, measured at 4 and 8px: a dash `2t` long,
/// then a gap of `t`, starting with a dash at the box's left edge.
const DASH_ON: f32 = 2.0;
const DASH_PERIOD: f32 = 3.0;

/// The wave, in thicknesses: Chrome 153's measured at 2, 4, 8 and 16px and
/// fitted -- period 4t + 0.75px, centreline swing 1.75t, band 2.75t +
/// 0.25px -- with the fixed pixel terms dropped, so that the figure is the
/// same at every size and under every transform. Stroked `t` wide, its
/// band, the swing plus the stroke, starts where a solid line would.
const WAVE_PERIOD: f32 = 4.0;
/// The swing decides what a wave costs per pixel it inks: filled the same
/// way, a 24px wave swinging 1.75t costs 1.15 times as much per inked pixel
/// as one swinging `t`, SkParagraph's swing, and 1.2 times as much a draw.
const WAVE_SWING: f32 = 1.75;
const WAVE_BAND: f32 = WAVE_SWING + 1.0;

/// The shortest repeat, in device pixels, a dotted, dashed or wavy line is
/// drawn period by period at. Shorter, and each pixel holds more than one
/// period: its coverage is then the share of the period that is inked,
/// which a solid line at that share draws exactly, in one rectangle rather
/// than a loop whose length grows without bound as the line thins.
const MIN_DEVICE_PERIOD: f32 = 1.0;

/// Where a face puts its lines, in pixels at its size, each measured down
/// from the baseline as Skia's y is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct FaceLines {
    underline_top: f32,
    underline_thickness: f32,
    strikeout_top: f32,
    strikeout_thickness: f32,
    /// The face's ascent and descent as Skia reports them: on FreeType the
    /// `OS/2` typographic ones where the face sets `USE_TYPO_METRICS` and
    /// otherwise `hhea`'s, on CoreText `CTFontGetAscent` and its descent,
    /// and on DirectWrite `DWRITE_FONT_METRICS`.
    pub(crate) ascent: f32,
    pub(crate) descent: f32,
}

impl FaceLines {
    /// The lines `font` states, with SkParagraph's fallbacks where it
    /// states none: a fourteenth of the size for a thickness, an underline
    /// one thickness below the baseline, and a line-through centred half
    /// the x-height above it.
    pub(crate) fn of(font: &Font) -> Self {
        let (_, metrics) = font.metrics();
        let fallback = font.size() * FALLBACK_THICKNESS_PER_SIZE;
        let underline_thickness = metrics
            .underline_thickness()
            .filter(|t| *t > 0.0)
            .unwrap_or(fallback);
        let underline_top = metrics
            .underline_position()
            .filter(|top| *top > 0.0)
            .unwrap_or(underline_thickness);
        let strikeout_thickness = metrics
            .strikeout_thickness()
            .filter(|t| *t > 0.0)
            .unwrap_or(underline_thickness);
        let strikeout_top = metrics
            .strikeout_position()
            .unwrap_or(-metrics.x_height / 2.0 - strikeout_thickness / 2.0);
        Self {
            underline_top,
            underline_thickness,
            strikeout_top,
            strikeout_thickness,
            ascent: metrics.ascent,
            descent: metrics.descent,
        }
    }

    /// The lines of the first available font of a style in `families` at
    /// `size`: CSS Fonts 4, section 5.2, "the first font for which the
    /// character U+0020 (space) is not excluded by a unicode-range, given
    /// the font families in the font-family list". Nothing here has a
    /// `unicode-range`, so it is the first family there is, whatever its
    /// character map holds -- Chrome 153 draws an underline from a first
    /// family with no space in it -- and a family that is not there is no
    /// font, so the next one is asked. `None` where the collection has no
    /// face at all.
    ///
    /// One query for the whole list, as SkParagraph makes one: it answers
    /// the families that match, in order, and the collection's default face
    /// only where none does. Asked a family at a time, a missing one would
    /// answer with that default and stop the search there.
    ///
    /// With the `epoch` of fonts `collection` was built over, the answer is
    /// kept for the next ask in that epoch -- see [`FACE_LINES`]. Every
    /// collection built over one epoch resolves a family list to the same
    /// face, whichever hinting or instances it holds, so the epoch is what
    /// the answer depends on. Without one, as for a collection the crate's
    /// `TextEngine` builds over a library of its own, nothing is kept.
    pub(crate) fn first_available(
        collection: &mut FontCollection,
        epoch: Option<&Rc<FontEpoch>>,
        families: &[&str],
        style: FontStyle,
        size: f32,
        arguments: Option<&FontArguments>,
    ) -> Option<Self> {
        let Some(epoch) = epoch else {
            return Self::look_up(collection, families, style, size, arguments);
        };
        FACE_LINES.with_borrow_mut(|cache| {
            if !ptr::eq(cache.epoch.as_ptr(), Rc::as_ptr(epoch)) {
                cache.lines.clear();
                cache.epoch = Rc::downgrade(epoch);
            }
            let key = FaceKey {
                families: families
                    .iter()
                    .map(|name| name.to_string())
                    .collect(),
                weight: *style.weight(),
                width: *style.width(),
                slant: style.slant() as i32,
                size: size.to_bits(),
                arguments: arguments.cloned().map(Arguments),
            };
            if let Some(lines) = cache.lines.get(&key) {
                return *lines;
            }
            let lines =
                Self::look_up(collection, families, style, size, arguments);
            if cache.lines.len() >= FACE_LINES_KEPT {
                cache.lines.clear();
            }
            cache.lines.insert(key, lines);
            lines
        })
    }

    /// Drops every kept answer, so that nothing is kept for an epoch that
    /// has passed -- through [`forget`].
    fn forget() {
        FACE_LINES.with_borrow_mut(|cache| *cache = FaceLinesCache::default());
    }

    /// How many answers this thread keeps.
    #[cfg(test)]
    fn kept() -> usize {
        FACE_LINES.with_borrow(|cache| cache.lines.len())
    }

    /// [`first_available`](Self::first_available), asked of the collection.
    fn look_up(
        collection: &mut FontCollection,
        families: &[&str],
        style: FontStyle,
        size: f32,
        arguments: Option<&FontArguments>,
    ) -> Option<Self> {
        // Through the collection's own cache, which keeps the instance it
        // makes for `arguments`: a typeface instanced afresh is a new font
        // to the glyph cache, and reading its metrics then builds a
        // platform font each time -- on macOS a CoreText one.
        let typeface = collection
            .find_typefaces_with_font_arguments(families, style, arguments)
            .into_iter()
            .next()?;
        Some(Self::of(&Font::from_typeface(typeface, size)))
    }
}

/// Drops everything this module keeps on this thread: the first-available
/// answers, which belong to the fonts of an epoch, and the wave chunks,
/// which belong to no font and go too so that one call releases all of it.
/// For the font library to call when the fonts it holds change, and for
/// anything releasing memory while the library is idle.
pub(crate) fn forget() {
    FaceLines::forget();
    WaveChunk::forget();
}

/// How many first-available answers a thread keeps for one epoch of fonts.
///
/// A bound rather than a working set: an answer is a few numbers, but its
/// key holds a size, which can be any number, so text whose size animates
/// would add one a frame. Past it, every kept answer is dropped and asked
/// again.
const FACE_LINES_KEPT: usize = 256;

thread_local! {
    /// The answers [`FaceLines::first_available`] has given for the fonts
    /// of one epoch, dropped when a different epoch is asked about, when
    /// the library's fonts change -- [`FaceLines::forget`] -- and at
    /// [`FACE_LINES_KEPT`].
    static FACE_LINES: RefCell<FaceLinesCache> = RefCell::default();
}

#[derive(Default)]
struct FaceLinesCache {
    /// The epoch the answers are for, held weakly: the cache keeps no
    /// passed epoch's fonts alive, and while the weak reference is held the
    /// allocation it points at is not reused, so a later epoch cannot take
    /// its address.
    epoch: Weak<FontEpoch>,
    lines: CacheMap<FaceKey, Option<FaceLines>>,
}

/// What a first-available answer depends on within one epoch.
#[derive(PartialEq, Eq, Hash)]
struct FaceKey {
    families: Vec<String>,
    weight: i32,
    width: i32,
    slant: i32,
    /// The size's bits, so that the key is hashable; sizes that are equal
    /// as numbers but differ in bits, 0 and -0, are kept apart, which costs
    /// a second lookup and nothing else.
    size: u32,
    arguments: Option<Arguments>,
}

/// A text style's font arguments as part of a key, compared and hashed as
/// Skia compares and hashes them, coordinate by coordinate. A coordinate
/// that is NaN never equals itself, so such a key is never found again and
/// is asked afresh each time, within [`FACE_LINES_KEPT`].
struct Arguments(FontArguments);

impl PartialEq for Arguments {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl Eq for Arguments {}

impl Hash for Arguments {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.hash(state);
    }
}

/// A thickness asked for in place of the font's.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Thickness {
    /// The font's own.
    Font,
    /// So many pixels, as a canvas `textDecoration` length gives.
    Pixels(f32),
    /// So many times the font's own, as a Paragraph style's
    /// `decoration_thickness` gives.
    Times(f32),
}

impl Thickness {
    fn of(self, font: f32) -> f32 {
        match self {
            Self::Font => font,
            Self::Pixels(px) => px,
            Self::Times(times) => times * font,
        }
    }
}

/// What one decorating box draws.
#[derive(Clone, Debug)]
pub(crate) struct Decoration {
    pub(crate) lines: TextDecoration,
    pub(crate) style: TextDecorationStyle,
    pub(crate) thickness: Thickness,
    /// The lines' colour where one was named, or `None` for the colour of
    /// the paint the text is drawn with -- CSS's `currentcolor`, kept as the
    /// paint holds it, wide-gamut and with any global alpha already in it.
    /// In extended sRGB, unpremultiplied.
    pub(crate) color: Option<Color4f>,
    /// What a named colour's alpha is multiplied by: a canvas's global
    /// alpha, which the text's own paint carries and a named colour does
    /// not. 1 where the paint is composited through a layer that applies it.
    pub(crate) opacity: f32,
    pub(crate) face: FaceLines,
}

/// Which of a box's lines a pass paints: the ones beneath the glyphs, or
/// the line-through over them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Layer {
    Beneath,
    Over,
}

impl Layer {
    fn kinds(self) -> &'static [TextDecoration] {
        match self {
            Self::Beneath => {
                &[TextDecoration::UNDERLINE, TextDecoration::OVERLINE]
            }
            Self::Over => &[TextDecoration::LINE_THROUGH],
        }
    }
}

/// A box's part of one line: from `left` to `right` on the baseline at `y`,
/// in the paragraph's coordinates, on line `line`, counted from 0.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Span {
    pub(crate) left: f32,
    pub(crate) right: f32,
    pub(crate) y: f32,
    pub(crate) line: usize,
    pub(crate) box_index: usize,
}

/// The spans of `spans`, in line order, that are on `line`.
pub(crate) fn on_line(spans: &[Span], line: usize) -> &[Span] {
    let start = spans.partition_point(|span| span.line < line);
    let end = spans.partition_point(|span| span.line <= line);
    &spans[start..end]
}

/// The span each box covers on each line of a paragraph, gathered a glyph
/// run at a time from the runs `Paragraph::paint` draws -- see
/// `glyph_runs`, which leave out a line's trailing spaces as SkParagraph's
/// own decorations did.
///
/// A glyph at a time rather than a run: SkParagraph shapes adjacent styles
/// in one font as one run, so a run can hold text of two boxes, or of a box
/// and none. HarfBuzz gives a run's glyphs in visual order, so a box's
/// glyphs in a run lie side by side, and its span reaches the next glyph's
/// position or, for a run's last, the run's `advance_x`. A run's origin is
/// the line's, its baseline already rounded to the pixel `Paragraph::paint`
/// draws on.
///
/// A box's pieces join into one span where one follows the other directly,
/// so a dotted line keeps its phase across a change of font: on one line,
/// with nothing between them -- no text of another box or of none, which
/// [`Spans::add`] sees, and no placeholder, which the visit does not report
/// and which leaves a gap.
#[derive(Default)]
pub(crate) struct Spans {
    spans: Vec<Span>,
    /// Whether the last piece seen was the last span's, so that the next
    /// may join it.
    joinable: bool,
}

/// How far apart, in pixels, the end of one piece and the start of the next
/// may be and still be one span: the two are the same edge, one the run's
/// clip and the other the next run's position, so only rounding parts them.
const JOIN_TOLERANCE: f32 = 1.0 / 64.0;

impl Spans {
    /// Adds `run`'s spans. `box_of` names the box the text at a UTF-8 offset
    /// belongs to, or `None` for text with no decoration.
    pub(crate) fn add(
        &mut self,
        run: &GlyphRunRef<'_>,
        box_of: impl Fn(usize) -> Option<usize>,
    ) {
        let mut start = 0;
        while start < run.starts.len() {
            let current = box_of(run.starts[start] as usize);
            let end = (start + 1..run.starts.len())
                .find(|&at| box_of(run.starts[at] as usize) != current)
                .unwrap_or(run.starts.len());
            match current {
                Some(box_index) => self.push(run, start..end, box_index),
                None => self.joinable = false,
            }
            start = end;
        }
    }

    fn push(
        &mut self,
        run: &GlyphRunRef<'_>,
        glyphs: Range<usize>,
        box_index: usize,
    ) {
        let left = run.positions[glyphs.clone()]
            .iter()
            .map(|position| position.x)
            .fold(f32::INFINITY, f32::min);
        let right = run
            .positions
            .get(glyphs.end)
            .map_or(run.advance_x, |next| run.origin.x + next.x);
        let span = Span {
            left: run.origin.x + left,
            right,
            y: run.origin.y,
            line: run.line,
            box_index,
        };
        match self.spans.last_mut() {
            Some(last)
                if self.joinable
                    && last.line == run.line
                    && last.box_index == box_index
                    && last.y == span.y
                    && (span.left - last.right).abs() <= JOIN_TOLERANCE =>
            {
                last.right = span.right;
            }
            _ => self.spans.push(span),
        }
        self.joinable = true;
    }

    /// The spans, in the order their lines and glyph runs came.
    pub(crate) fn finish(self) -> Vec<Span> {
        self.spans
    }
}

/// [`Spans`] over runs already collected.
pub(crate) fn spans(
    runs: &[GlyphRun],
    box_of: impl Fn(usize) -> Option<usize>,
) -> Vec<Span> {
    let mut spans = Spans::default();
    for run in runs {
        spans.add(&run.borrowed(), &box_of);
    }
    spans.finish()
}

/// Paints `layer`'s lines of every span, offset by `origin`, on `base` --
/// the glyphs' paint, so that a canvas's compositing, filter and shadow
/// reach the lines as they reach the text -- in the box's named colour, its
/// alpha times the box's `opacity`, or in `base`'s own colour where none was
/// named -- onto pixels that do with a faint paint what `faint` says.
pub(crate) fn paint(
    canvas: &Canvas,
    spans: &[Span],
    boxes: &[Decoration],
    origin: Point,
    layer: Layer,
    base: &Paint,
    faint: Faint,
) {
    for span in spans {
        let decoration = &boxes[span.box_index];
        for kind in layer.kinds() {
            if decoration.lines.contains(*kind) {
                let color = decoration.color.map(|mut color| {
                    color.a *= decoration.opacity;
                    color
                });
                draw_line(
                    canvas,
                    decoration,
                    *kind,
                    span,
                    origin,
                    &line_paint(base, color),
                    faint,
                );
            }
        }
    }
}

/// Paints every line of box `box_index`'s spans in `shadow`, a text
/// shadow's paint, as is -- its colour, not the line's. CSS Text Decoration
/// 4, text-shadow: "Each layer shadows the element's text and all its text
/// decorations (composited together)". Onto pixels that do with a faint
/// paint what `faint` says.
pub(crate) fn paint_shadow(
    canvas: &Canvas,
    spans: &[Span],
    boxes: &[Decoration],
    box_index: usize,
    origin: Point,
    shadow: &Paint,
    faint: Faint,
) {
    let decoration = &boxes[box_index];
    for span in spans.iter().filter(|span| span.box_index == box_index) {
        for kind in [
            TextDecoration::UNDERLINE,
            TextDecoration::OVERLINE,
            TextDecoration::LINE_THROUGH,
        ] {
            if decoration.lines.contains(kind) {
                draw_line(
                    canvas,
                    decoration,
                    kind,
                    span,
                    origin,
                    &line_paint(shadow, None),
                    faint,
                );
            }
        }
    }
}

/// The band one line occupies before its style is applied, as its top and
/// thickness, and which way a second line goes: +1 down, -1 up.
fn band(decoration: &Decoration, kind: TextDecoration) -> (f32, f32, f32) {
    let face = &decoration.face;
    if kind == TextDecoration::OVERLINE {
        let t = decoration.thickness.of(face.underline_thickness);
        (face.ascent - t, t, -1.0)
    } else if kind == TextDecoration::LINE_THROUGH {
        // About the font's stroke centre, whatever the thickness.
        let t = decoration.thickness.of(face.strikeout_thickness);
        let centre = face.strikeout_top + face.strikeout_thickness / 2.0;
        (centre - t / 2.0, t, 1.0)
    } else {
        let t = decoration.thickness.of(face.underline_thickness);
        (face.underline_top, t, 1.0)
    }
}

/// The paint a line is drawn in: `base`, the glyphs' paint or a text
/// shadow's, as an antialiased fill with no shader, in `color` where the box
/// names one.
fn line_paint(base: &Paint, color: Option<Color4f>) -> Paint {
    let mut paint = base.clone();
    paint
        .set_shader(None)
        .set_anti_alias(true)
        .set_style(Style::Fill);
    if let Some(color) = color {
        paint.set_color4f(color, None);
    }
    paint
}

/// Draws one line of `span` in `line`, its paint from [`line_paint`], onto
/// pixels that do with a faint paint what `faint` says.
fn draw_line(
    canvas: &Canvas,
    decoration: &Decoration,
    kind: TextDecoration,
    span: &Span,
    origin: Point,
    line: &Paint,
    faint: Faint,
) {
    let (top, t, out) = band(decoration, kind);
    if t.is_nan() || t <= 0.0 || span.right <= span.left {
        return;
    }
    let paint = faint.drawable(line);
    let left = origin.x + span.left;
    let right = origin.x + span.right;
    let top = origin.y + span.y + top;
    let rect = |l: f32, r: f32, y: f32| Rect::new(l, y, r.min(right), y + t);
    // How long one unit along the line is on the device.
    let device = canvas
        .local_to_device_as_3x3()
        .map_vector((1.0, 0.0))
        .length();
    match decoration.style {
        TextDecorationStyle::Double => {
            canvas.draw_rect(rect(left, right, top), &paint);
            canvas.draw_rect(
                rect(left, right, top + out * DOUBLE_OFFSET * t),
                &paint,
            );
        }
        TextDecorationStyle::Dotted | TextDecorationStyle::Dashed => {
            let (on, period) = match decoration.style {
                TextDecorationStyle::Dotted => (DOT_ON, DOT_PERIOD),
                _ => (DASH_ON, DASH_PERIOD),
            };
            if period * t * device < MIN_DEVICE_PERIOD {
                let mut share = line.clone();
                share.set_alpha_f(line.alpha_f() * on / period);
                canvas
                    .draw_rect(rect(left, right, top), &faint.drawable(&share));
                return;
            }
            let mut x = left;
            while x < right {
                canvas.draw_rect(rect(x, x + on * t, top), &paint);
                x += period * t;
            }
        }
        TextDecorationStyle::Wavy => {
            // The band grows the way a second line would: down from an
            // underline's top, up from an overline's foot, and about a
            // line-through's centre.
            let band = WAVE_BAND * t;
            let band_top = match kind {
                k if k == TextDecoration::OVERLINE => top + t - band,
                k if k == TextDecoration::LINE_THROUGH => {
                    top + t / 2.0 - band / 2.0
                }
                _ => top,
            };
            let middle = band_top + band / 2.0;
            match wave(left, right, middle, t, device) {
                Some(wave) => canvas.draw_path(&wave, &paint),
                // Shorter than a device pixel, a wave swings less than its
                // period, so its ink averages to the stroke along its middle.
                None => canvas
                    .draw_rect(rect(left, right, middle - t / 2.0), &paint),
            };
        }
        _ => {
            canvas.draw_rect(rect(left, right, top), &paint);
        }
    }
}

/// A wavy line's ink from `left` to `right`, `t` thick, swinging about
/// `middle` -- or `None` where its period is shorter than
/// [`MIN_DEVICE_PERIOD`] at `device` pixels a unit, which bounds the
/// periods to one per device pixel along it.
///
/// A continuous wave, run on half a wave past each end of the box, its
/// stroke's outline cut to its band: square at `left` and `right` across
/// the whole stroke, as Chrome 152 draws a wave, where a stroke ending at
/// the box would end in butt caps at a slant to it, poking out on one side
/// and leaving a notch on the other; and along the top and bottom, which
/// the stroke only touches -- the crests swing `WAVE_SWING / 2` thicknesses
/// off the middle and the stroke adds half a thickness more, `WAVE_BAND / 2`
/// -- but which the stroker's curves overshoot by a fraction of a pixel. The
/// cut is made on the outline rather than by a clip, so that a filter or a
/// shadow on the paint spreads past the line as it spreads past a glyph.
///
/// Its phase is its box's: the centreline crosses the middle on its way
/// down at `left`, wherever the box starts. It is laid out as copies of a
/// run of [`WAVE_CHUNK`] periods, [`WaveChunk::of`] `t`, already cut along
/// the band, end to end from half a wave before `left`; only a copy
/// reaching past `left` or `right` is cut again. Each copy ends where the
/// next begins, across the same normal to the same centreline, and the
/// copies between the two cut at the ends are joined into one contour --
/// see [`Joined`] -- so the wave fills as the one stroke did:
/// `a_tiled_wave_is_the_whole_wave` holds it to that. Neither the stroker
/// nor the cut runs per period on a draw: the stroker was most of a wavy
/// line's cost when it ran over the whole centreline each time.
fn wave(
    left: f32,
    right: f32,
    middle: f32,
    t: f32,
    device: f32,
) -> Option<Path> {
    if WAVE_PERIOD * t * device < MIN_DEVICE_PERIOD {
        return None;
    }
    #[cfg(test)]
    if WHOLE_WAVE.get() {
        return Some(whole_wave(left, right, middle, t));
    }
    Some(lay_out(&WaveChunk::of(t), left, right, middle, t))
}

/// Copies of `chunk`, a wave `t` thick, laid end to end about `middle` from
/// half a wave before `left` to at least half a wave past `right`, cut
/// square at both -- [`wave`]'s body, apart from the chunk it is given.
fn lay_out(
    chunk: &WaveChunk,
    left: f32,
    right: f32,
    middle: f32,
    t: f32,
) -> Path {
    let length = WAVE_PERIOD * t;
    let step = length * chunk.periods as f32;
    let start = left - length / 2.0;
    let x = |v: Point| v.x;
    let mut path = PathBuilder::new();
    path.set_fill_type(chunk.fill_type);
    let mut add = |pieces: &mut dyn Iterator<Item = Piece>| {
        let mut pieces = pieces.peekable();
        let Some(first) = pieces.peek() else {
            return;
        };
        path.move_to(first.start());
        pieces.for_each(|piece| piece.add_to(&mut path));
        path.close();
    };
    // Each copy's offset from the start rather than a running sum, so that
    // the last copy is placed as exactly as the first.
    let offset = |n: usize| start + n as f32 * step;
    let laid = |at: f32| at < right + length / 2.0;
    // The copies no end of the box cuts, joined into one contour as they
    // come, while the chunk's caps are known.
    let mut joined = Joined::default();
    for n in (0..).take_while(|n| laid(offset(*n))) {
        let at = offset(n);
        let by = Point::new(at, middle);
        let (cut_left, cut_right) =
            (at + chunk.reach.0 < left, at + chunk.reach.1 > right);
        // Where another copy follows, this one ends on exactly the points
        // the next starts on, placed as the next places them.
        let next = Point::new(offset(n + 1), middle);
        let meets = chunk.caps.filter(|_| laid(next.x));
        for (index, contour) in chunk.contours.iter().enumerate() {
            let moved = contour.iter().enumerate().map(|(at, piece)| {
                let piece = piece.offset(by);
                match meets.filter(|caps| caps.contour == index) {
                    Some(caps) => caps.meet(contour, at, piece, next),
                    None => piece,
                }
            });
            let caps = chunk.caps.filter(|caps| caps.contour == index);
            if !cut_left && !cut_right {
                match caps {
                    Some(caps) => joined.push(caps, moved.collect()),
                    None => add(&mut moved.into_iter()),
                }
                continue;
            }
            if caps.is_some() {
                joined.finish(&mut add);
            }
            let mut pieces: Vec<Piece> = moved.collect();
            if cut_left {
                pieces = cut(&pieces, x, |v| v - left);
            }
            if cut_right {
                pieces = cut(&pieces, x, |v| right - v);
            }
            add(&mut pieces.into_iter());
        }
    }
    joined.finish(&mut add);
    path.detach()
}

/// Copies of a chunk laid end to end and joined into one contour: each
/// copy's outer side after the last, then the last copy's end cap, the inner
/// sides back from the last copy to the first, and the first copy's start
/// cap. Where two copies meet, the one's end cap and the other's start cap
/// are the same line run both ways, which the fill only ever cancels; joined,
/// neither is there to fill, and the wave is the one contour the stroke of
/// its whole centreline would be.
#[derive(Default)]
struct Joined {
    outer: Vec<Piece>,
    /// Each copy's inner side, first copy first.
    inner: Vec<Vec<Piece>>,
    start_cap: Option<Piece>,
    end_cap: Option<Piece>,
}

impl Joined {
    /// Adds the next copy, placed: its contour as `pieces`, its caps where
    /// `caps` says they are.
    fn push(&mut self, caps: Caps, pieces: Vec<Piece>) {
        let count = pieces.len();
        // The pieces after `from` and before `to`, round the contour.
        let between = |from: usize, to: usize| {
            (1..count)
                .map(move |k| (from + k) % count)
                .take_while(move |at| *at != to)
        };
        self.outer
            .extend(between(caps.start, caps.end).map(|at| pieces[at]));
        self.inner
            .push(between(caps.end, caps.start).map(|at| pieces[at]).collect());
        self.start_cap.get_or_insert(pieces[caps.start]);
        self.end_cap = Some(pieces[caps.end]);
    }

    /// Adds the copies pushed so far as one contour, and starts again.
    fn finish(&mut self, add: &mut dyn FnMut(&mut dyn Iterator<Item = Piece>)) {
        let joined = std::mem::take(self);
        let (Some(start_cap), Some(end_cap)) =
            (joined.start_cap, joined.end_cap)
        else {
            return;
        };
        let mut pieces = joined
            .outer
            .into_iter()
            .chain(std::iter::once(end_cap))
            .chain(joined.inner.into_iter().rev().flatten())
            .chain(std::iter::once(start_cap));
        add(&mut pieces);
    }
}

#[cfg(test)]
thread_local! {
    /// Whether [`wave`] draws [`whole_wave`] instead, as the reference a
    /// tiled wave is held to through everything that draws one.
    pub(crate) static WHOLE_WAVE: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
    /// Whether [`Caps::find`] finds none, as it would after a change to
    /// Skia's stroker: the copies are then laid out unjoined.
    pub(crate) static NO_CAPS: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
    /// How many pieces [`cut`] has been handed on this thread.
    static CUT_PIECES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The wave [`wave`] lays out as copies of a period, stroked whole instead:
/// one centreline from half a wave before `left` to at least half a wave
/// past `right`, its outline cut to the band.
#[cfg(test)]
fn whole_wave(left: f32, right: f32, middle: f32, t: f32) -> Path {
    let control = WAVE_SWING * t;
    let half = WAVE_PERIOD * t / 2.0;
    let mut centre = PathBuilder::new();
    centre.move_to((left - half, middle));
    let mut x = left - half;
    let mut side = -1.0;
    while x < right + half {
        centre.quad_to(
            (x + half / 2.0, middle + side * control),
            (x + half, middle),
        );
        x += half;
        side = -side;
    }
    let mut stroke = Paint::default();
    stroke
        .set_style(Style::Stroke)
        .set_stroke_width(t)
        .set_stroke_cap(Cap::Butt);
    let mut outline = PathBuilder::new();
    fill_path_with_paint(&centre.detach(), &stroke, &mut outline, None, None);
    let band = WAVE_BAND * t;
    cut_to(
        &outline.detach(),
        Rect::new(left, middle - band / 2.0, right, middle + band / 2.0),
    )
}

/// A run of periods of a wave `t` thick, about y = 0 from x = 0, where the
/// centreline crosses the middle on its way down: its butt-capped stroke's
/// outline, cut to the band's top and bottom -- what [`wave`] lays end to
/// end. Kept per thickness, since everything in it scales with `t` but the
/// stroker's tolerance, which does not.
#[derive(Clone)]
struct WaveChunk {
    /// The outline's closed contours.
    contours: Rc<[Vec<Piece>]>,
    /// The least and greatest x of any point of them, control points
    /// included, so that a copy wholly inside a box is known to need no
    /// cut there.
    reach: (f32, f32),
    fill_type: PathFillType,
    /// How many periods the run is.
    periods: usize,
    /// Where its caps are, so that copies laid end to end meet exactly.
    caps: Option<Caps>,
    /// What it holds, in bytes, against [`WAVE_CHUNKS_BYTES`].
    bytes: usize,
}

/// Where a chunk's two butt caps lie in its outline -- the line across the
/// stroke at its start and the one at its end -- so that where copies are
/// laid end to end, each copy's end cap can be given exactly the points the
/// next copy's start cap is placed at.
///
/// The two are the same line in exact arithmetic, and in floating point a
/// millionth of a pixel apart, more the longer the chunk. Two edges that
/// cancel exactly leave the fill as one stroke would; two that nearly do
/// leave the antialiasing a sliver, which reached 16 of 255 at a chunk's
/// end in a wave scaled by 2 and turned.
#[derive(Clone, Copy)]
struct Caps {
    /// The contour both lie in.
    contour: usize,
    /// The index of the start cap's line in it.
    start: usize,
    /// The index of the end cap's line in it.
    end: usize,
}

impl Caps {
    /// The caps of `contours`, a chunk `length` long: the one line across
    /// the middle at each end, in one contour. `None` where the outline is
    /// not shaped so, which leaves copies meeting as they are placed.
    fn find(contours: &[Vec<Piece>], length: f32, t: f32) -> Option<Self> {
        #[cfg(test)]
        if NO_CAPS.get() {
            return None;
        }
        let across = |x: f32| {
            contours
                .iter()
                .enumerate()
                .flat_map(move |(contour, pieces)| {
                    pieces.iter().enumerate().filter_map(move |(at, piece)| {
                        let Piece::Line(a, b) = piece else {
                            return None;
                        };
                        let mid = (*a + *b) * 0.5;
                        ((mid.x - x).abs() < t / 4.0 && mid.y.abs() < t / 4.0)
                            .then_some((contour, at))
                    })
                })
        };
        let mut starts = across(0.0);
        let mut ends = across(length);
        let (Some(start), None) = (starts.next(), starts.next()) else {
            return None;
        };
        let (Some(end), None) = (ends.next(), ends.next()) else {
            return None;
        };
        if start.0 != end.0 {
            return None;
        }
        // The end cap runs from the outer side to the inner and the start
        // cap back, so the end's first point is the start's last moved on
        // by the chunk's length.
        let pieces = &contours[start.0];
        let (Piece::Line(_, outer), Piece::Line(from, _)) =
            (pieces[start.1], pieces[end.1])
        else {
            return None;
        };
        let along = from - Point::new(length, 0.0);
        ((along - outer).length() < t / 4.0).then_some(Self {
            contour: start.0,
            start: start.1,
            end: end.1,
        })
    }

    /// `piece`, at index `at` of `contour` and already placed, made to meet
    /// the copy placed at `next`: the end cap becomes the next copy's start
    /// cap run the other way, and the pieces either side of it end and
    /// start where it now does.
    fn meet(
        self,
        contour: &[Piece],
        at: usize,
        piece: Piece,
        next: Point,
    ) -> Piece {
        let Piece::Line(inner, outer) = contour[self.start] else {
            return piece;
        };
        let (inner, outer) = (inner + next, outer + next);
        let count = contour.len();
        if at == self.end {
            Piece::Line(outer, inner)
        } else if at == (self.end + count - 1) % count {
            piece.with_end(outer)
        } else if at == (self.end + 1) % count {
            piece.with_start(inner)
        } else {
            piece
        }
    }
}

/// How many periods [`wave`] strokes as one and lays end to end.
///
/// Copies are joined into one contour wherever the chunk's caps are known,
/// so the chunk's length no longer decides how many contours the fill
/// walks: from one period to sixteen, a 200px wave is at most three contours
/// and fills in the same time within 5%. What it decides is the rest of a
/// wave's cost. Copies of two lay out a 200px wave fastest -- 5 to 10% under
/// copies of one, and level with or under four; a wave three periods long,
/// which is cut through a whole copy, costs about 1.4 times as much at four
/// and 2.3 at eight; and stroking a chunk the first time a thickness is drawn
/// costs 1.5 to 1.8 times one period's at two, 2.7 to 3.1 at four and 9 to 11
/// at sixteen. Measured at 1.2px and 2px on a quiet machine, timed in a debug
/// build, so as ratios only. A chunk of two is about 550 bytes.
const WAVE_CHUNK: usize = 2;

/// How many bytes of stroked chunks a thread keeps, all thicknesses
/// together: about 450 chunks of [`WAVE_CHUNK`] periods.
///
/// A bound rather than a working set, as [`FACE_LINES_KEPT`] is: a
/// thickness can be any number, so a line whose thickness animates would
/// add a chunk a frame. Past it, every kept chunk is dropped and stroked
/// again.
const WAVE_CHUNKS_BYTES: usize = 256 * 1024;

thread_local! {
    /// The chunks [`WaveChunk::of`] has stroked, by the bits of their
    /// thickness, with the bytes they hold; dropped at
    /// [`WAVE_CHUNKS_BYTES`] and by [`forget`].
    static WAVE_CHUNKS: RefCell<(CacheMap<u32, WaveChunk>, usize)> =
        RefCell::default();
}

impl WaveChunk {
    /// The chunk of a wave `t` thick, stroked on this thread's first ask
    /// for `t` and kept.
    fn of(t: f32) -> Self {
        WAVE_CHUNKS.with_borrow_mut(|(kept, bytes)| {
            if let Some(chunk) = kept.get(&t.to_bits()) {
                return chunk.clone();
            }
            let chunk = Self::stroked(t, WAVE_CHUNK);
            if *bytes + chunk.bytes > WAVE_CHUNKS_BYTES {
                kept.clear();
                *bytes = 0;
            }
            *bytes += chunk.bytes;
            kept.insert(t.to_bits(), chunk.clone());
            chunk
        })
    }

    /// `periods` periods of a wave `t` thick, stroked as one.
    fn stroked(t: f32, periods: usize) -> Self {
        // A quadratic peaks at half its control point's offset, so a control
        // 2 * (swing / 2) off the centreline swings it by `swing` from crest
        // to trough. Down first: y grows downward, so up is negative.
        let control = WAVE_SWING * t;
        let half = WAVE_PERIOD * t / 2.0;
        let mut centre = PathBuilder::new();
        centre.move_to((0.0, 0.0));
        for n in 0..periods {
            let at = 2.0 * half * n as f32;
            centre.quad_to((at + half / 2.0, -control), (at + half, 0.0));
            centre.quad_to((at + half * 1.5, control), (at + 2.0 * half, 0.0));
        }
        let mut stroke = Paint::default();
        stroke
            .set_style(Style::Stroke)
            .set_stroke_width(t)
            .set_stroke_cap(Cap::Butt);
        let mut outline = PathBuilder::new();
        fill_path_with_paint(
            &centre.detach(),
            &stroke,
            &mut outline,
            None,
            None,
        );
        let outline = outline.detach();
        let edge = WAVE_BAND * t / 2.0;
        let y = |p: Point| p.y;
        let contours: Rc<[Vec<Piece>]> = contours(&outline)
            .into_iter()
            .map(|pieces| cut(&pieces, y, |v| v + edge))
            .map(|pieces| cut(&pieces, y, |v| edge - v))
            .filter(|pieces| !pieces.is_empty())
            .collect();
        let reach = contours
            .iter()
            .flatten()
            .flat_map(|piece| piece.points())
            .fold((f32::INFINITY, f32::NEG_INFINITY), |(low, high), p| {
                (low.min(p.x), high.max(p.x))
            });
        let caps = Caps::find(&contours, WAVE_PERIOD * t * periods as f32, t);
        let bytes = contours
            .iter()
            .map(|pieces| {
                size_of::<Vec<Piece>>() + pieces.len() * size_of::<Piece>()
            })
            .sum();
        Self {
            contours,
            reach,
            fill_type: outline.fill_type(),
            periods,
            caps,
            bytes,
        }
    }

    /// Drops every kept chunk -- through [`forget`].
    fn forget() {
        WAVE_CHUNKS.with_borrow_mut(|kept| *kept = Default::default());
    }

    /// How many chunks this thread keeps, and the bytes they hold.
    #[cfg(test)]
    fn kept() -> (usize, usize) {
        WAVE_CHUNKS.with_borrow(|(kept, bytes)| (kept.len(), *bytes))
    }
}

/// One piece of an outline: a line or a quadratic, each from its first
/// point to its last.
#[derive(Clone, Copy, Debug)]
enum Piece {
    Line(Point, Point),
    Quad(Point, Point, Point),
}

impl Piece {
    fn start(self) -> Point {
        match self {
            Piece::Line(a, _) | Piece::Quad(a, _, _) => a,
        }
    }

    fn end(self) -> Point {
        match self {
            Piece::Line(_, b) | Piece::Quad(_, _, b) => b,
        }
    }

    fn at(self, u: f32) -> Point {
        match self {
            Piece::Line(a, b) => a + (b - a) * u,
            Piece::Quad(a, c, b) => {
                let v = 1.0 - u;
                a * (v * v) + c * (2.0 * u * v) + b * (u * u)
            }
        }
    }

    /// The piece split at `u`, by de Casteljau, into the part before and
    /// the part after.
    fn split(self, u: f32) -> (Piece, Piece) {
        let lerp = |a: Point, b: Point| a + (b - a) * u;
        match self {
            Piece::Line(a, b) => {
                let m = lerp(a, b);
                (Piece::Line(a, m), Piece::Line(m, b))
            }
            Piece::Quad(a, c, b) => {
                let (ac, cb) = (lerp(a, c), lerp(c, b));
                let m = lerp(ac, cb);
                (Piece::Quad(a, ac, m), Piece::Quad(m, cb, b))
            }
        }
    }

    /// Where along a quadratic `along` -- its x or its y -- turns back,
    /// if it does within it.
    fn turn(self, along: impl Fn(Point) -> f32) -> Option<f32> {
        match self {
            Piece::Line(..) => None,
            Piece::Quad(a, c, b) => {
                let (a, c, b) = (along(a), along(c), along(b));
                let denominator = a - 2.0 * c + b;
                let u = (a - c) / denominator;
                (denominator != 0.0 && u > 0.0 && u < 1.0).then_some(u)
            }
        }
    }

    /// The piece in parts that each move one way along `along`, so that
    /// each crosses a line across that axis at most once.
    fn monotone(
        self,
        along: impl Fn(Point) -> f32,
    ) -> impl Iterator<Item = Piece> {
        let (first, second) = match self.turn(along) {
            Some(u) => {
                let (before, after) = self.split(u);
                (before, Some(after))
            }
            None => (self, None),
        };
        std::iter::once(first).chain(second)
    }

    /// Its points, the control point included.
    fn points(self) -> impl Iterator<Item = Point> {
        let (a, c, b) = match self {
            Piece::Line(a, b) => (a, None, b),
            Piece::Quad(a, c, b) => (a, Some(c), b),
        };
        std::iter::once(a).chain(c).chain(std::iter::once(b))
    }

    /// The piece with its first point moved to `point`.
    fn with_start(self, point: Point) -> Piece {
        match self {
            Piece::Line(_, b) => Piece::Line(point, b),
            Piece::Quad(_, c, b) => Piece::Quad(point, c, b),
        }
    }

    /// The piece with its last point moved to `point`.
    fn with_end(self, point: Point) -> Piece {
        match self {
            Piece::Line(a, _) => Piece::Line(a, point),
            Piece::Quad(a, c, _) => Piece::Quad(a, c, point),
        }
    }

    /// The piece moved `by`.
    fn offset(self, by: Point) -> Piece {
        match self {
            Piece::Line(a, b) => Piece::Line(a + by, b + by),
            Piece::Quad(a, c, b) => Piece::Quad(a + by, c + by, b + by),
        }
    }

    fn add_to(self, path: &mut PathBuilder) {
        match self {
            Piece::Line(_, b) => path.line_to(b),
            Piece::Quad(_, c, b) => path.quad_to(c, b),
        };
    }
}

/// Bisection steps for where a piece crosses a cut: past the precision of
/// an `f32` parameter, so the crossing is as exact as the type holds.
const CROSSING_STEPS: usize = 32;

/// `outline` cut to `bounds` by [`cut`], one side at a time: the whole
/// wave a tiled one is held to, and what `a_cut_leaves_what_a_clip_leaves`
/// holds `cut` to.
#[cfg(test)]
fn cut_to(outline: &Path, bounds: Rect) -> Path {
    let x = |p: Point| p.x;
    let y = |p: Point| p.y;
    let contours = contours(outline)
        .into_iter()
        .map(|pieces| cut(&pieces, x, |v| v - bounds.left))
        .map(|pieces| cut(&pieces, x, |v| bounds.right - v))
        .map(|pieces| cut(&pieces, y, |v| v - bounds.top))
        .map(|pieces| cut(&pieces, y, |v| bounds.bottom - v));
    let mut path = PathBuilder::new();
    path.set_fill_type(outline.fill_type());
    for pieces in contours {
        let Some(first) = pieces.first() else {
            continue;
        };
        path.move_to(first.start());
        pieces.iter().for_each(|piece| piece.add_to(&mut path));
        path.close();
    }
    path.detach()
}

/// The closed contours of `outline` as pieces, each closed explicitly.
fn contours(outline: &Path) -> Vec<Vec<Piece>> {
    let points = outline.points();
    let mut contours: Vec<Vec<Piece>> = Vec::new();
    let mut at = 0;
    let mut pen = Point::default();
    let mut open = Point::default();
    let close = |pieces: &mut Vec<Piece>, pen: Point, open: Point| {
        if pen != open {
            pieces.push(Piece::Line(pen, open));
        }
    };
    for verb in outline.verbs() {
        match verb {
            PathVerb::Move => {
                if let Some(pieces) = contours.last_mut() {
                    close(pieces, pen, open);
                }
                contours.push(Vec::new());
                (pen, open) = (points[at], points[at]);
                at += 1;
            }
            PathVerb::Line => {
                if let Some(pieces) = contours.last_mut() {
                    pieces.push(Piece::Line(pen, points[at]));
                }
                pen = points[at];
                at += 1;
            }
            PathVerb::Quad => {
                if let Some(pieces) = contours.last_mut() {
                    pieces.push(Piece::Quad(pen, points[at], points[at + 1]));
                }
                pen = points[at + 1];
                at += 2;
            }
            PathVerb::Close => {
                if let Some(pieces) = contours.last_mut() {
                    close(pieces, pen, open);
                }
                pen = open;
            }
            // Broken invariant, not input: the stroker draws a quadratic
            // centreline's butt-capped stroke in lines and quadratics, and
            // `a_wave_is_lines_and_quadratics` fails before this can be
            // reached by a change to it.
            PathVerb::Conic | PathVerb::Cubic => {
                unreachable!("a wave's outline has a {verb:?}")
            }
        }
    }
    if let Some(pieces) = contours.last_mut() {
        close(pieces, pen, open);
    }
    contours
}

/// The closed contour `pieces` cut to where `inside` of its coordinate
/// `along` is not negative, the cut a straight edge along that side.
///
/// As Sutherland and Hodgman clip a polygon by a half-plane: a piece inside
/// is kept, a piece crossing is split where it crosses, and the stretch
/// outside is replaced by the straight edge along the side from where the
/// contour left to where it came back. Every point inside keeps the winding
/// number it had, so the outline fills as it did wherever it is kept, even
/// where a stroke's inner side folds over itself. Linear in the contour's
/// size, where a boolean operation rebuilds the whole outline.
///
/// The contours cut here are a wave's: the stroke of a quadratic
/// centreline with butt caps and the default join, which Skia's stroker
/// draws in lines and quadratics only; `a_wave_is_lines_and_quadratics`
/// pins that.
fn cut(
    pieces: &[Piece],
    along: impl Fn(Point) -> f32 + Copy,
    inside: impl Fn(f32) -> f32,
) -> Vec<Piece> {
    #[cfg(test)]
    CUT_PIECES.set(CUT_PIECES.get() + pieces.len());
    let mut kept: Vec<Piece> = Vec::with_capacity(pieces.len() + 2);
    // Where the contour last left the kept side, to join to where it
    // comes back.
    let mut left_at: Option<Point> = None;
    let rejoin = |kept: &mut Vec<Piece>, from: Option<Point>, to: Point| {
        if let Some(from) = from.filter(|from| *from != to) {
            kept.push(Piece::Line(from, to));
        }
    };
    for piece in pieces.iter().flat_map(|piece| {
        // A piece lies within the hull of its points, so one whose points
        // are all on one side is wholly there, and is kept or dropped whole
        // rather than split where it turns: splitting adds a piece that the
        // outline then carries, for nothing.
        let sides = piece.points().map(|point| inside(along(point)) >= 0.0);
        let (mut any_in, mut any_out) = (false, false);
        sides.for_each(|side| match side {
            true => any_in = true,
            false => any_out = true,
        });
        let whole = !(any_in && any_out);
        let (first, second) = match whole {
            true => (Some(*piece), None),
            false => (None, Some(piece.monotone(along))),
        };
        first.into_iter().chain(second.into_iter().flatten())
    }) {
        let (a, b) = (
            inside(along(piece.start())) >= 0.0,
            inside(along(piece.end())) >= 0.0,
        );
        match (a, b) {
            (true, true) => kept.push(piece),
            (false, false) => {}
            (_, _) => {
                // Monotone along the axis, so the side changes once.
                let (mut low, mut high) = (0.0f32, 1.0f32);
                for _ in 0..CROSSING_STEPS {
                    let mid = (low + high) / 2.0;
                    if (inside(along(piece.at(mid))) >= 0.0) == a {
                        low = mid;
                    } else {
                        high = mid;
                    }
                }
                let (before, after) = piece.split((low + high) / 2.0);
                if a {
                    kept.push(before);
                    left_at = Some(before.end());
                } else {
                    rejoin(&mut kept, left_at.take(), after.start());
                    kept.push(after);
                }
            }
        }
    }
    // A contour that begins outside is joined round from where it last
    // left to where it first came back.
    if let (Some(from), Some(first)) = (left_at, kept.first().copied()) {
        rejoin(&mut kept, Some(from), first.start());
    }
    kept
}

/// The box a UTF-8 offset falls in, among boxes given as the text each
/// covers, in order and not overlapping -- for [`spans`]' `box_of`.
pub(crate) fn box_at(
    ranges: &[(Range<usize>, usize)],
    at: usize,
) -> Option<usize> {
    let after = ranges.partition_point(|(range, _)| range.end <= at);
    ranges
        .get(after)
        .filter(|(range, _)| range.contains(&at))
        .map(|(_, index)| *index)
}

#[cfg(test)]
mod tests {
    use super::{Decoration, FaceLines, Layer, Span, Thickness, paint, wave};
    use crate::{context::faint::Faint, started};
    use skia_safe::{
        Color, Color4f, Paint, PictureRecorder, Point, Rect,
        textlayout::{TextDecoration, TextDecorationStyle},
    };

    /// How many drawing operations one box's underline in `style`, `t`
    /// thick and 500 pixels long, records at a canvas scale of `scale`.
    fn ops(style: TextDecorationStyle, t: f32, scale: f32) -> usize {
        let face = FaceLines {
            underline_top: 2.0,
            underline_thickness: 1.0,
            strikeout_top: -8.0,
            strikeout_thickness: 1.0,
            ascent: -20.0,
            descent: 5.0,
        };
        let boxes = [Decoration {
            lines: TextDecoration::UNDERLINE,
            style,
            thickness: Thickness::Pixels(t),
            color: Some(Color4f::from(Color::BLACK)),
            opacity: 1.0,
            face,
        }];
        let spans = [Span {
            left: 0.0,
            right: 500.0,
            y: 50.0,
            line: 0,
            box_index: 0,
        }];
        let mut recorder = PictureRecorder::new();
        let canvas =
            recorder.begin_recording(Rect::new(0.0, 0.0, 600.0, 100.0), false);
        canvas.scale((scale, scale));
        paint(
            canvas,
            &spans,
            &boxes,
            Point::new(0.0, 0.0),
            Layer::Beneath,
            &Paint::default(),
            Faint::Dropped,
        );
        recorder
            .finish_recording_as_picture(None)
            .map_or(0, |picture| picture.approximate_op_count())
    }

    /// A pattern too fine for a device pixel to hold one period is drawn as
    /// the share of a solid line it inks, in one operation, however thin;
    /// one a pixel can hold is drawn period by period, at most a step per
    /// device pixel along it.
    #[test]
    fn a_pattern_finer_than_a_pixel_is_drawn_once() {
        for style in [TextDecorationStyle::Dotted, TextDecorationStyle::Dashed]
        {
            for t in [0.0001, 0.1] {
                assert!(ops(style, t, 1.0) <= 4, "{style:?} at {t}px");
            }
            // Scaled up, the same thin line is coarse enough to draw.
            assert!(ops(style, 0.1, 40.0) > 4, "{style:?} scaled up");
            assert!(ops(style, 2.0, 1.0) <= 500 + 4, "{style:?} at 2px");
        }
        // A wave is one path, so its bound is the path's: no wave below a
        // device pixel's period, and above it a size linear in the length.
        for t in [0.0001, 0.1] {
            assert!(wave(0.0, 500.0, 0.0, t, 1.0).is_none(), "{t}px");
        }
        let coarse = wave(0.0, 500.0, 0.0, 0.5, 1.0).expect("a wave");
        // Its outline takes about eight points a half-wave, and there are no
        // more half-waves than device pixels: 500 here.
        assert!(
            coarse.count_points() <= 16 * 500,
            "{}",
            coarse.count_points()
        );
    }

    /// A wave's uncut stroke, from half a wave before `left` to half a wave
    /// past `right`, and its band: the reference the cut one is held to,
    /// drawn under a clip to the band.
    fn stroke_and_band(
        left: f32,
        right: f32,
        middle: f32,
        t: f32,
    ) -> (skia_safe::Path, Rect) {
        let half = super::WAVE_PERIOD * t / 2.0;
        let (stroke, band) =
            continuous_wave(left - 2.0 * half, right + half, middle, t);
        (stroke, Rect::new(left, band.top, right, band.bottom))
    }

    /// The wave this module draws, from `left` to `right` about `middle`.
    fn drawn_wave(
        left: f32,
        right: f32,
        middle: f32,
        t: f32,
    ) -> skia_safe::Path {
        wave(left, right, middle, t, 1.0).expect("a wave")
    }

    /// Each pixel's alpha with `path` filled black, at `scale`.
    fn coverage(path: &skia_safe::Path, scale: f32) -> Vec<u8> {
        let (w, h) = (200, 80);
        let mut surface =
            started::raster_n32_premul((w, h)).expect("a surface");
        let canvas = surface.canvas();
        canvas.scale((scale, scale));
        let mut paint = Paint::default();
        paint.set_anti_alias(true).set_color(Color::BLACK);
        canvas.draw_path(path, &paint);
        let info = skia_safe::ImageInfo::new(
            (w, h),
            skia_safe::ColorType::Alpha8,
            skia_safe::AlphaType::Premul,
            None,
        );
        let mut alpha = vec![0u8; (w * h) as usize];
        assert!(surface.read_pixels(&info, &mut alpha, w as usize, (0, 0)));
        alpha
    }

    /// A wave is its stroke cut to its band -- square at the ends of its
    /// box, and along the band's edges, which the stroker's curves
    /// overshoot -- at lengths down to
    /// less than one half-wave and at fractional ends: what a clip to the
    /// band leaves of the stroke, sampled as `a_cut_leaves_what_a_clip_leaves`
    /// samples it. They part by up to 8 of 255, allowed 10, because the
    /// rasterizer flattens a curve split in two differently from the whole
    /// one -- splitting every curve of these strokes exactly, and cutting
    /// nothing, moves them by up to 8. Leaving the ends uncut parts them by
    /// 255, and leaving the bottom uncut by 60: the stroker overshoots the
    /// band below its troughs. Above its crests it does not on these
    /// strokes, so leaving the top uncut passes here, and the cut is kept
    /// for the stroke that does.
    #[test]
    fn a_wave_is_its_stroke_cut_to_its_band() {
        for t in [0.5, 1.0, 2.0, 4.0, 8.0, 16.0] {
            for (left, right) in [
                (10.25, 150.6),
                (10.0, 10.0 + t * 0.3),
                (10.4, 10.4 + t * 1.3),
                (10.0, 10.0 + t * 2.0),
                (10.7, 10.7 + t * 5.1),
            ] {
                let mine = averaged(&drawn_wave(left, right, 30.0, t), None);
                let (stroke, band) = stroke_and_band(left, right, 30.0, t);
                let clipped = averaged(&stroke, Some(band));
                let worst = mine
                    .iter()
                    .zip(&clipped)
                    .map(|(a, b)| a.abs_diff(*b))
                    .max()
                    .unwrap_or(0);
                assert!(
                    worst <= 10,
                    "{t}px from {left} to {right}: {worst} off"
                );
            }
        }
    }

    /// A wave's ends are square across the whole stroke, as Chrome 152
    /// draws one: a continuous wave cut at the box, not a stroke that
    /// starts and stops there. A butt cap meets the centreline at a slant,
    /// so a stroke ending at the box leaves a notch on the cap's inner side;
    /// the same wave run on for whole periods past each end and clipped to
    /// the band has none.
    #[test]
    fn a_wave_is_cut_square_from_a_longer_wave() {
        for t in [1.0, 4.0, 8.0, 16.0] {
            for (left, right) in [(10.4, 150.6), (10.7, 10.7 + t * 5.1)] {
                let mine = averaged(&drawn_wave(left, right, 30.0, t), None);
                let period = super::WAVE_PERIOD * t;
                let (longer, band) =
                    continuous_wave(left - period, right + period, 30.0, t);
                let band = Rect::new(left, band.top, right, band.bottom);
                let clipped = averaged(&longer, Some(band));
                let worst = mine
                    .iter()
                    .zip(&clipped)
                    .map(|(a, b)| a.abs_diff(*b))
                    .max()
                    .unwrap_or(0);
                assert!(
                    worst <= 10,
                    "{t}px from {left} to {right}: {worst} off"
                );
            }
        }
    }

    /// The stroke of a wave of whole half-waves from `from`, starting down
    /// from the middle, to past `to`, and its band -- built here, apart from
    /// [`super::WaveChunk`], as the reference a cut wave is held to.
    fn continuous_wave(
        from: f32,
        to: f32,
        middle: f32,
        t: f32,
    ) -> (skia_safe::Path, Rect) {
        use skia_safe::{
            paint::{Cap, Style},
            path_utils::fill_path_with_paint,
        };
        let half = super::WAVE_PERIOD * t / 2.0;
        let mut centre = skia_safe::PathBuilder::new();
        centre.move_to((from, middle));
        let (mut x, mut side) = (from, 1.0);
        while x < to {
            centre.quad_to(
                (x + half / 2.0, middle + side * super::WAVE_SWING * t),
                (x + half, middle),
            );
            x += half;
            side = -side;
        }
        let mut stroke = Paint::default();
        stroke
            .set_style(Style::Stroke)
            .set_stroke_width(t)
            .set_stroke_cap(Cap::Butt);
        let mut outline = skia_safe::PathBuilder::new();
        fill_path_with_paint(
            &centre.detach(),
            &stroke,
            &mut outline,
            None,
            None,
        );
        let band = super::WAVE_BAND * t;
        (
            outline.detach(),
            Rect::new(from, middle - band / 2.0, to, middle + band / 2.0),
        )
    }

    /// The stroker draws a wave's outline in lines and quadratics only,
    /// which is all `cut` takes: at every thickness, at lengths
    /// that end inside a half-wave, and scaled.
    #[test]
    fn a_wave_is_lines_and_quadratics() {
        use skia_safe::PathVerb;
        for t in [0.3, 0.5, 1.0, 2.0, 4.0, 8.0, 16.0, 40.0] {
            for length in [t * 0.3, t * 1.7, 97.3, 500.0] {
                for device in [1.0, 4.0] {
                    let Some(path) = wave(3.1, 3.1 + length, 20.0, t, device)
                    else {
                        continue;
                    };
                    assert!(
                        path.verbs().iter().all(|verb| matches!(
                            verb,
                            PathVerb::Move
                                | PathVerb::Line
                                | PathVerb::Quad
                                | PathVerb::Close
                        )),
                        "{t}px, {length} long"
                    );
                }
            }
        }
    }

    /// `cut_to` cuts a contour that begins outside its bounds, on each
    /// side in turn, crossing more than one side and folding over itself,
    /// to what an antialiased clip to the bounds leaves of it: the stretch
    /// outside is replaced by the edge along the side, the contour closed
    /// round from where it last left, and every point inside keeps its
    /// winding, twice round where it goes round twice. Compared at eight
    /// samples a pixel each way and averaged: at one, an antialiased clip
    /// multiplies its coverage into the shape's and is itself off by up to
    /// 14 of 255 where both edges cross a pixel, at two by 4, at eight by 1,
    /// which is rounding. A boolean intersection parts from the clip by 38
    /// on the figure eight, whose lobes it unwinds.
    #[test]
    fn a_cut_leaves_what_a_clip_leaves() {
        let bounds = Rect::new(40.0, 20.0, 160.0, 60.0);
        let shapes: [&[(f32, f32)]; 6] = [
            &[(20.0, 40.0), (100.0, 0.0), (180.0, 45.0), (100.0, 75.0)],
            &[(100.0, 5.0), (175.0, 40.0), (100.0, 70.0), (30.0, 35.0)],
            &[(170.0, 30.0), (60.0, 75.0), (25.0, 10.0), (130.0, 15.0)],
            &[(90.0, 72.0), (30.0, 25.0), (150.0, 2.0), (175.0, 66.0)],
            // A figure eight, crossing itself inside, so its lobes wind
            // opposite ways.
            &[(20.0, 10.0), (180.0, 70.0), (180.0, 10.0), (20.0, 70.0)],
            // Round twice the same way, so its middle winds twice.
            &[
                (100.0, 2.0),
                (178.0, 40.0),
                (100.0, 78.0),
                (22.0, 40.0),
                (100.0, 8.0),
                (170.0, 40.0),
                (100.0, 72.0),
                (30.0, 40.0),
            ],
        ];
        for points in shapes {
            let mut shape = skia_safe::PathBuilder::new();
            shape.move_to(points[0]);
            points[1..].iter().for_each(|p| {
                shape.quad_to((p.0 - 10.0, p.1 + 7.0), *p);
            });
            shape.close();
            let shape = shape.detach();
            let mine = averaged(&super::cut_to(&shape, bounds), None);
            let clipped = averaged(&shape, Some(bounds));
            let worst = mine
                .iter()
                .zip(&clipped)
                .map(|(a, b)| a.abs_diff(*b))
                .max()
                .unwrap_or(0);
            assert!(worst <= 1, "{points:?}: {worst} off");
        }
    }

    /// Each pixel's alpha with `path` filled black, inside an antialiased
    /// clip to `clip` where there is one, drawn at eight samples a pixel
    /// each way and averaged down.
    fn averaged(path: &skia_safe::Path, clip: Option<Rect>) -> Vec<u8> {
        const SAMPLES: usize = 8;
        let (w, h) = (200, 80);
        let (sw, sh) = (w * SAMPLES, h * SAMPLES);
        let mut surface = started::raster_n32_premul((sw as i32, sh as i32))
            .expect("a surface");
        let canvas = surface.canvas();
        canvas.scale((SAMPLES as f32, SAMPLES as f32));
        if let Some(clip) = clip {
            canvas.clip_rect(clip, None, true);
        }
        let mut paint = Paint::default();
        paint.set_anti_alias(true).set_color(Color::BLACK);
        canvas.draw_path(path, &paint);
        let info = skia_safe::ImageInfo::new(
            (sw as i32, sh as i32),
            skia_safe::ColorType::Alpha8,
            skia_safe::AlphaType::Premul,
            None,
        );
        let mut alpha = vec![0u8; sw * sh];
        assert!(surface.read_pixels(&info, &mut alpha, sw, (0, 0)));
        (0..w * h)
            .map(|i| {
                let (x, y) = (i % w * SAMPLES, i / w * SAMPLES);
                let sum: usize = (0..SAMPLES * SAMPLES)
                    .map(|k| {
                        alpha[(y + k / SAMPLES) * sw + x + k % SAMPLES] as usize
                    })
                    .sum();
                (sum / (SAMPLES * SAMPLES)) as u8
            })
            .collect()
    }

    /// Each pixel's alpha with `path` filled black on the engine `gpu`
    /// names, the context scaled by `scale` and then turned by `turn`
    /// radians, on a canvas big enough for any wave here at twice its size.
    fn filled(
        path: &skia_safe::Path,
        gpu: bool,
        (scale, turn): (f32, f32),
    ) -> Vec<u8> {
        use crate::{
            canvas::{Canvas, EngineKind},
            path::{FillRule, Path2D},
        };
        let mut canvas = Canvas::new(400.0, 200.0);
        canvas.set_gpu(gpu);
        assert_eq!(
            canvas.engine_kind() == EngineKind::Gpu,
            gpu,
            "the engine named"
        );
        let ctx = canvas.context();
        ctx.scale(scale, scale);
        ctx.rotate(turn);
        ctx.fill_path(&Path2D::from_inner(path.clone()), FillRule::NonZero);
        ctx.get_image_data(0.0, 0.0, 400.0, 200.0)
            .expect("the canvas reads back")
            .pixels()
            .as_chunks::<4>()
            .0
            .iter()
            .map(|px| px[3])
            .collect()
    }

    /// The largest difference between two images, byte for byte.
    fn worst(a: &[u8], b: &[u8]) -> u8 {
        assert_eq!(a.len(), b.len());
        a.iter()
            .zip(b)
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap_or(0)
    }

    /// Whether this machine has a GPU engine to run a GPU leg on.
    fn gpu_reachable() -> bool {
        use crate::canvas::{Canvas, EngineKind};
        let mut probe = Canvas::new(4.0, 4.0);
        probe.set_gpu(true);
        probe.engine_kind() == EngineKind::Gpu
    }

    /// The engines to draw on: the CPU, and the GPU where there is one.
    fn engines() -> &'static [bool] {
        if gpu_reachable() {
            &[false, true]
        } else {
            &[false]
        }
    }

    /// Scaled up, down, and turned -- where a seam along a copy's end would
    /// no longer fall on a pixel column.
    const VIEWS: [(f32, f32); 4] =
        [(1.0, 0.0), (2.0, 0.0), (2.0, 0.05), (0.75, 0.0)];

    /// A wave laid out as copies of one period fills as the same wave
    /// stroked whole: no seam where one copy meets the next, and the same
    /// phase, at every thickness, at fractional starts, and at lengths inside
    /// one half-wave and across many -- rasterized directly, not averaged,
    /// which would hide a seam, on the CPU and the GPU, as drawn, scaled and
    /// turned.
    #[test]
    fn a_tiled_wave_is_the_whole_wave() {
        for &gpu in engines() {
            for t in [0.5, 1.0, 1.5, 2.0, 3.0, 4.0, 8.0, 16.0] {
                for left in [10.0, 10.25, 10.5, 10.73] {
                    for length in [t * 0.3, t * 1.7, t * 5.1, 140.6] {
                        let right = left + length;
                        let tiled = drawn_wave(left, right, 30.0, t);
                        let whole = super::whole_wave(left, right, 30.0, t);
                        for view in VIEWS {
                            let off = worst(
                                &filled(&tiled, gpu, view),
                                &filled(&whole, gpu, view),
                            );
                            assert!(
                                off <= tiled_off(gpu),
                                "gpu {gpu}, {t}px from {left} to {right}, \
                                 scaled and turned {view:?}: {off} off"
                            );
                        }
                    }
                }
            }
        }
    }

    /// How far a tiled wave's pixels may part from the whole one's on the
    /// engine `gpu` names: as far as the whole wave parts from itself when
    /// it is built 37 and 11 pixels away and drawn back in place, which
    /// moves no curve but rounds every coordinate differently. On the CPU
    /// that is up to 10 of 255, the stroker's subdivision of a quadratic
    /// depending on where it lies; on the GPU it is one sample of a pixel's
    /// [`GPU_SAMPLES`] turning over, in up to 15 pixels a wave. A seam
    /// where two copies met would mark a pixel at every copy's end; leaving
    /// a 0.05 pixel gap between them parts the CPU's pixels by more than 10.
    fn tiled_off(gpu: bool) -> u8 {
        match gpu {
            false => 10,
            true => (256 / GPU_SAMPLES) as u8,
        }
    }

    /// The samples a GPU canvas takes per pixel unless asked otherwise --
    /// `ExportOptions::msaa_from`'s default.
    const GPU_SAMPLES: usize = 4;

    /// Drawing a longer wave cuts no more of it: only the copies reaching
    /// past the box's ends are cut, and the band's top and bottom were cut
    /// once, when the chunk was stroked.
    #[test]
    fn a_long_wave_cuts_only_its_end_chunks() {
        let (left, t) = (10.3, 2.0);
        let period = super::WAVE_PERIOD * t;
        let cut = |right: f32| {
            wave(left, right, 30.0, t, 1.0).expect("a wave");
            super::CUT_PIECES.set(0);
            wave(left, right, 30.0, t, 1.0).expect("a wave");
            super::CUT_PIECES.get()
        };
        let chunk = super::WAVE_CHUNK as f32;
        let short = cut(left + 3.0 * chunk * period + 3.1);
        let long = cut(left + 30.0 * chunk * period + 3.1);
        assert!(short > 0, "the ends are cut");
        assert_eq!(long, short, "and nothing between them");
    }

    /// A cut keeps a piece whose points all lie on its kept side as it is,
    /// and drops one whose points all lie on the other, splitting only a
    /// piece that may cross: a quadratic lies within the triangle of its
    /// points. Splitting every piece where it turns gave the outline a
    /// piece more for each turn, and the fill an edge more to walk.
    #[test]
    fn a_cut_splits_only_what_it_crosses() {
        use super::{Piece, cut};
        let p = |x: f32, y: f32| Point::new(x, y);
        // A closed contour of quadratics that each turn in x.
        let contour = vec![
            Piece::Quad(p(0.0, 0.0), p(20.0, 5.0), p(0.0, 10.0)),
            Piece::Quad(p(0.0, 10.0), p(-20.0, 15.0), p(0.0, 20.0)),
            Piece::Line(p(0.0, 20.0), p(0.0, 0.0)),
        ];
        let x = |point: Point| point.x;
        let kept = cut(&contour, x, |v| v + 100.0);
        assert_eq!(kept.len(), contour.len(), "nothing crosses: kept whole");
        let dropped = cut(&contour, x, |v| v - 100.0);
        assert!(dropped.is_empty(), "all outside: nothing kept");
        let crossed = cut(&contour, x, |v| v - 5.0);
        assert!(
            crossed
                .iter()
                .all(|piece| piece.points().all(|q| q.x >= 5.0 - 1e-3)),
            "what crosses is cut: {crossed:?}"
        );
        // Ends on the kept side, bulging past it: crosses twice, and is cut
        // at both, though neither end says so.
        let bulging = cut(&contour, x, |v| 5.0 - v);
        assert!(
            bulging
                .iter()
                .all(|piece| piece.points().all(|q| q.x <= 5.0 + 1e-3)),
            "what bulges across is cut: {bulging:?}"
        );
    }

    /// Every chunk knows its caps, so that copies of it meet exactly: at
    /// every thickness a wave is drawn at, from a fraction of a pixel to
    /// many, the outline has one line across the middle at each end. Where
    /// it did not, copies would meet a rounding apart and the fill would
    /// leave a sliver -- `a_tiled_wave_is_the_whole_wave` finds one at a
    /// chunk's end, 1px thick, scaled by 2 and turned.
    #[test]
    fn a_chunk_knows_its_caps() {
        for t in [0.25, 0.5, 1.0, 1.2, 2.0, 3.0, 4.0, 8.0, 16.0, 40.0] {
            let chunk = super::WaveChunk::stroked(t, super::WAVE_CHUNK);
            assert!(chunk.caps.is_some(), "{t}px");
        }
    }

    /// A chunk is kept per thickness, no more bytes of them than the
    /// bound, and none after [`super::forget`].
    #[test]
    fn kept_chunks_are_bounded_and_forgotten() {
        use super::{WAVE_CHUNKS_BYTES, WaveChunk};
        super::forget();
        assert_eq!(WaveChunk::kept(), (0, 0));
        wave(0.0, 40.0, 10.0, 1.0, 1.0).expect("a wave");
        wave(0.0, 80.0, 10.0, 1.0, 1.0).expect("a wave");
        let (chunks, bytes) = WaveChunk::kept();
        assert_eq!(chunks, 1, "one thickness, one chunk");
        // Enough thicknesses to pass the bound twice over.
        let passes = 2 * WAVE_CHUNKS_BYTES / bytes + 1;
        for n in 0..passes {
            let t = 1.0 + n as f32 / 64.0;
            wave(0.0, 20.0, 10.0, t, 1.0).expect("a wave");
            let (_, held) = WaveChunk::kept();
            assert!(held <= WAVE_CHUNKS_BYTES, "{held} bytes held");
        }
        assert!(WaveChunk::kept().0 >= 1);
        super::forget();
        assert_eq!(WaveChunk::kept(), (0, 0));
    }

    /// A wave is at most three contours, whatever its length: the copy cut
    /// at its left end, the copies between joined into one, and the copy cut
    /// at its right -- each closed by two lines, its caps or its cuts. Laid
    /// out unjoined, as it is where the caps are not found, it is a contour a
    /// chunk and one more.
    #[test]
    fn a_wave_is_at_most_three_contours() {
        use skia_safe::PathVerb;
        let count = |path: &skia_safe::Path, verb: PathVerb| {
            path.verbs().iter().filter(|v| **v == verb).count()
        };
        for t in [0.5, 1.2, 2.0, 8.0] {
            let period = super::WAVE_PERIOD * t;
            for periods in [0.3, 3.0, 17.0, 40.0, 100.0] {
                let right = 10.0 + periods * period;
                let path = wave(10.0, right, 30.0, t, 1.0).expect("a wave");
                let contours = count(&path, PathVerb::Move);
                assert!(contours <= 3, "{t}px, {periods} periods: {contours}");
                let lines = count(&path, PathVerb::Line);
                assert!(
                    lines <= 2 * contours,
                    "{t}px, {periods} periods: {lines} lines"
                );
                super::forget();
                super::NO_CAPS.set(true);
                let unjoined = wave(10.0, right, 30.0, t, 1.0).expect("a wave");
                super::NO_CAPS.set(false);
                super::forget();
                let chunks =
                    ((periods + 1.0) / super::WAVE_CHUNK as f32).ceil();
                assert!(
                    count(&unjoined, PathVerb::Move) as f32 <= chunks + 1.0,
                    "{t}px, {periods} periods, unjoined"
                );
            }
        }
    }

    /// Where a copy cut at an end of the box meets the copies joined between,
    /// the one's cap and the other's are the same line exactly, run both
    /// ways: every line across the middle of one contour has its reverse,
    /// bit for bit, in another. A rounding apart, the antialiasing leaves a
    /// sliver along them, which reached 16 of 255 for chunks of four scaled
    /// and turned.
    #[test]
    fn copies_meet_on_exactly_the_same_points() {
        for t in [0.5, 1.2, 2.0, 8.0] {
            let period = super::WAVE_PERIOD * t;
            for (left, periods) in [(10.0, 40.0), (10.73, 17.3), (3.31, 100.0)]
            {
                let path = wave(left, left + periods * period, 30.0, t, 1.0)
                    .expect("a wave");
                let contours = super::contours(&path);
                // The caps: lines crossing the middle at a slant, the cuts at
                // the ends being upright -- to a bisection's rounding -- and
                // the band's edges level.
                let caps: Vec<(usize, Point, Point)> = contours
                    .iter()
                    .enumerate()
                    .flat_map(|(at, pieces)| {
                        pieces.iter().filter_map(move |piece| match piece {
                            super::Piece::Line(a, b)
                                if (a.y - 30.0) * (b.y - 30.0) < 0.0
                                    && (a.x - b.x).abs() > t / 100.0 =>
                            {
                                Some((at, *a, *b))
                            }
                            _ => None,
                        })
                    })
                    .collect();
                assert!(!caps.is_empty(), "{t}px, {periods} periods: caps");
                for (at, a, b) in &caps {
                    assert!(
                        caps.iter().any(|(other, c, d)| other != at
                            && c == b
                            && d == a),
                        "{t}px from {left}, {periods} periods: {a:?} {b:?}"
                    );
                }
            }
        }
    }

    /// Where the caps are not found -- as after a change to Skia's stroker
    /// moves them -- the copies are laid out unjoined, and draw as the joined
    /// wave does within the seam floor, on the CPU and the GPU, as drawn,
    /// scaled and turned. Unjoined, two copies meet a rounding apart rather
    /// than exactly, which the CPU's antialiasing shows as 1 of 255 as drawn
    /// and up to 8 scaled and turned; the GPU draws them the same.
    /// `a_chunk_knows_its_caps` is what says the caps have stopped being
    /// found.
    #[test]
    fn a_wave_whose_caps_are_not_found_draws_as_the_joined_one() {
        for &gpu in engines() {
            for t in [0.5, 1.0, 2.0, 8.0] {
                for (left, length) in [(10.0, 140.6), (10.73, t * 21.0)] {
                    let right = left + length;
                    super::forget();
                    super::NO_CAPS.set(true);
                    let unjoined = drawn_wave(left, right, 30.0, t);
                    super::NO_CAPS.set(false);
                    super::forget();
                    let joined = drawn_wave(left, right, 30.0, t);
                    for view in VIEWS {
                        let off = worst(
                            &filled(&unjoined, gpu, view),
                            &filled(&joined, gpu, view),
                        );
                        assert!(
                            off <= tiled_off(gpu),
                            "gpu {gpu}, {t}px from {left} to {right}, \
                             {view:?}: {off} off"
                        );
                    }
                }
            }
        }
    }

    /// Wavy text drawn with `WHOLE_WAVE` at `whole` on the engine `gpu`
    /// names, `view` scaling then turning the context: canvas text from a
    /// fractional x, and a paragraph whose wave changes thickness and colour
    /// mid-line, so that each span takes its own phase, and wraps to a
    /// second line.
    fn wavy_text(gpu: bool, whole: bool, (scale, turn): (f32, f32)) -> Vec<u8> {
        use crate::{
            canvas::{Canvas, EngineKind},
            color::RgbaLinear,
            font::FontLibrary,
            text::{
                TextDecoration as Lines, TextDecorationStyle as LineStyle,
                TextEngine, TextStyle,
            },
        };
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            FontLibrary::new()
                .register_font_from_path(
                    "WaveOswald",
                    "tests/assets/fonts/Oswald/Oswald-VariableFont_wght.ttf",
                )
                .expect("Oswald registers");
        });
        let base = TextStyle {
            font_families: vec!["WaveOswald".into()],
            font_size: 28.0,
            color: RgbaLinear::from_srgb8(0x20, 0x30, 0x40, 1.0),
            decoration: Lines {
                underline: true,
                overline: false,
                line_through: true,
            },
            decoration_style: LineStyle::Wavy,
            ..TextStyle::default()
        };
        let engine = TextEngine::new(&FontLibrary::new());
        let mut builder = engine.paragraph_builder(&base);
        builder.add_text("Hamburgefonstiv ");
        builder.push_style(&TextStyle {
            decoration_thickness: 2.5,
            decoration_color: Some(RgbaLinear::from_srgb8(200, 0, 0, 1.0)),
            ..base.clone()
        });
        builder.add_text("quick brown fox jumps");
        builder.pop();
        let paragraph = builder.build(260.0);

        super::WHOLE_WAVE.set(whole);
        let mut canvas = Canvas::new(360.0, 240.0);
        canvas.set_gpu(gpu);
        // What the canvas draws with, not what it was asked for: asking
        // for the GPU falls back to the raster engine silently.
        assert_eq!(
            canvas.engine_kind() == EngineKind::Gpu,
            gpu,
            "the leg runs on the engine it is named for"
        );
        let ctx = canvas.context();
        ctx.scale(scale, scale);
        ctx.rotate(turn);
        assert!(ctx.set_font_css("24px WaveOswald"));
        ctx.set_text_decoration_css("underline line-through wavy")
            .expect("a wavy decoration");
        ctx.fill_text("Hamburgefonstiv", 10.37, 30.6, None);
        ctx.draw_paragraph(&paragraph, 7.6, 40.3);
        let pixels = ctx
            .get_image_data(0.0, 0.0, 360.0, 240.0)
            .expect("the canvas reads back")
            .pixels()
            .to_vec();
        super::WHOLE_WAVE.set(false);
        // Premultiplied, as the canvas holds them: unpremultiplied, a pixel
        // a wave's edge barely touches reads as the full colour or none.
        pixels
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|px| {
                let alpha = u16::from(px[3]);
                let premultiplied =
                    |c: u8| ((u16::from(c) * alpha + 127) / 255) as u8;
                [
                    premultiplied(px[0]),
                    premultiplied(px[1]),
                    premultiplied(px[2]),
                    px[3],
                ]
            })
            .collect()
    }

    /// Wavy text draws the same with its waves tiled as stroked whole,
    /// through everything that draws one -- canvas text and the Paragraph
    /// API, a span edge mid-line and a second line -- on the CPU and, where
    /// this machine has one, the GPU, as drawn, scaled, and scaled and
    /// turned.
    #[test]
    fn tiled_waves_draw_as_whole_ones_on_every_engine() {
        for &gpu in engines() {
            for view in [(1.0, 0.0), (2.0, 0.0), (1.5, 0.05)] {
                let tiled = wavy_text(gpu, false, view);
                let whole = wavy_text(gpu, true, view);
                assert!(
                    tiled.iter().any(|byte| *byte != 0),
                    "something is drawn"
                );
                let off = worst(&tiled, &whole);
                assert!(
                    off <= tiled_off(gpu),
                    "gpu {gpu}, scaled and turned {view:?}: {off} off"
                );
            }
        }
    }

    /// The collection and epoch the shared library hands out for
    /// `families` at 24px.
    fn shared_fonts(
        families: &[&str],
    ) -> (
        skia_safe::textlayout::FontCollection,
        std::rc::Rc<crate::node::font_library::FontEpoch>,
    ) {
        use crate::node::font_library::FontLibrary;
        let mut style = skia_safe::textlayout::TextStyle::new();
        style.set_font_families(families).set_font_size(24.0);
        FontLibrary::with_shared(|lib| {
            (lib.fonts_for_style(&style, &[]).0, lib.epoch())
        })
    }

    /// A first-available answer is kept for the fonts of one epoch and no
    /// longer: once a family is registered under a name that found nothing
    /// before, the name finds it, and the library's change leaves nothing
    /// kept for the epoch that passed.
    #[test]
    fn a_kept_face_goes_when_the_fonts_change() {
        use super::FaceLines;
        use crate::node::font_library::FontLibrary;
        use skia_safe::{Data, FontMgr, FontStyle};
        let family = ["DecorKeptProbe"];
        let ask = |collection: &mut skia_safe::textlayout::FontCollection,
                   epoch| {
            FaceLines::first_available(
                collection,
                Some(epoch),
                &family,
                FontStyle::normal(),
                24.0,
                None,
            )
        };
        let (mut before_fonts, before_epoch) = shared_fonts(&family);
        let before = ask(&mut before_fonts, &before_epoch);
        assert!(FaceLines::kept() > 0, "the answer is kept");
        wave(0.0, 40.0, 10.0, 1.0, 1.0).expect("a wave");
        assert!(super::WaveChunk::kept().0 > 0, "and a wave's chunk");

        let file = "tests/assets/fonts/Oswald/Oswald-VariableFont_wght.ttf";
        let data = Data::new_copy(&std::fs::read(file).expect("Oswald"));
        let face = FontMgr::new().new_from_data(data, None).expect("a face");
        FontLibrary::with_shared(|lib| {
            lib.register_typeface(face, Some(family[0].to_string()))
        });
        assert_eq!(FaceLines::kept(), 0, "the change drops what was kept");
        assert_eq!(super::WaveChunk::kept().0, 0, "chunks too");

        let (mut after_fonts, after_epoch) = shared_fonts(&family);
        let after = ask(&mut after_fonts, &after_epoch);
        let fresh = FaceLines::first_available(
            &mut after_fonts,
            None,
            &family,
            FontStyle::normal(),
            24.0,
            None,
        );
        assert_eq!(after, fresh, "the registered family's lines");
        assert_ne!(after, before, "and not the ones kept before");
        // Asked again about the epoch that passed, it answers for that
        // epoch's fonts, not the new one's.
        assert_eq!(ask(&mut before_fonts, &before_epoch), before);
    }

    /// The answers a thread keeps are bounded, however many sizes are
    /// asked for, and each is the one the collection gives uncached.
    /// What the idle watcher posts to a thread lets the kept faces' lines
    /// and the kept wave chunks go with the font caches.
    #[test]
    fn an_idle_release_lets_kept_lines_and_waves_go() {
        use super::{FaceLines, WaveChunk};
        use crate::node::font_library::FontLibrary;
        use skia_safe::FontStyle;
        let family = ["DecorIdleProbe"];
        let (mut fonts, epoch) = shared_fonts(&family);
        FaceLines::first_available(
            &mut fonts,
            Some(&epoch),
            &family,
            FontStyle::normal(),
            24.0,
            None,
        );
        for t in [1.0, 2.5] {
            wave(0.0, 40.0, 10.0, t, 1.0).expect("a wave");
        }
        assert!(FaceLines::kept() > 0, "a face's lines are kept");
        assert_eq!(WaveChunk::kept().0, 2, "and a chunk per thickness");

        FontLibrary::with_shared(FontLibrary::release_caches);
        assert_eq!(FaceLines::kept(), 0, "no face's lines past a release");
        assert_eq!(WaveChunk::kept().0, 0, "and no wave chunk");
    }

    #[test]
    fn kept_faces_are_bounded_and_answer_as_the_collection_does() {
        use super::{FACE_LINES_KEPT, FaceLines};
        use skia_safe::FontStyle;
        let family = ["serif"];
        let (mut fonts, epoch) = shared_fonts(&family);
        for step in 0..=FACE_LINES_KEPT {
            let size = 8.0 + step as f32 / 4.0;
            let kept = FaceLines::first_available(
                &mut fonts,
                Some(&epoch),
                &family,
                FontStyle::bold(),
                size,
                None,
            );
            let again = FaceLines::first_available(
                &mut fonts,
                Some(&epoch),
                &family,
                FontStyle::bold(),
                size,
                None,
            );
            let fresh = FaceLines::first_available(
                &mut fonts,
                None,
                &family,
                FontStyle::bold(),
                size,
                None,
            );
            assert_eq!(kept, fresh, "at {size}px");
            assert_eq!(again, fresh, "kept at {size}px");
            assert!(FaceLines::kept() <= FACE_LINES_KEPT, "at {size}px");
        }
    }

    /// A kept answer is the one for its style: three static faces of
    /// different families under one name, told apart by weight and by slant,
    /// each asked at one size, answer as the collection does uncached.
    #[test]
    fn a_kept_face_is_the_one_for_its_style() {
        use super::FaceLines;
        use crate::node::font_library::FontLibrary;
        use skia_safe::{
            Data, FontMgr, FontStyle,
            font_style::{Slant, Weight, Width},
        };
        let family = ["DecorStyleProbe"];
        for file in [
            "tests/assets/fonts/Monoton-Regular.woff2",
            "tests/assets/fonts/montserrat-latin/montserrat-v30-latin-700.woff2",
            "tests/assets/fonts/montserrat-latin/montserrat-v30-latin-italic.woff2",
        ] {
            let data = Data::new_copy(&std::fs::read(file).expect("a font"));
            let face =
                FontMgr::new().new_from_data(data, None).expect("a face");
            FontLibrary::with_shared(|lib| {
                lib.register_typeface(face, Some(family[0].to_string()))
            });
        }
        let (mut fonts, epoch) = shared_fonts(&family);
        let styles = [
            FontStyle::new(Weight::NORMAL, Width::NORMAL, Slant::Upright),
            FontStyle::new(Weight::BOLD, Width::NORMAL, Slant::Upright),
            FontStyle::new(Weight::NORMAL, Width::NORMAL, Slant::Italic),
        ];
        let answers: Vec<_> = styles
            .iter()
            .map(|style| {
                let kept = FaceLines::first_available(
                    &mut fonts,
                    Some(&epoch),
                    &family,
                    *style,
                    24.0,
                    None,
                );
                let fresh = FaceLines::first_available(
                    &mut fonts, None, &family, *style, 24.0, None,
                );
                assert_eq!(kept, fresh, "{style:?}");
                kept
            })
            .collect();
        // Not vacuous: the three faces' lines differ.
        assert_ne!(answers[0], answers[1], "by weight");
        assert_ne!(answers[0], answers[2], "by slant");
    }
}
