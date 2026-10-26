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
//! UPSTREAM: skia-safe 0.153.3 -- unfiled -- worked around
//! Re-check: cargo test a_decoration_sits_where_its_font_says
//! with SkParagraph's own decorations painted in place of these.
//! SkParagraph's `Decorations::paint` centres each line on the position
//! `SkFontMetrics` documents as the "distance from baseline to top of
//! stroke", so every line sat half its thickness high, and it takes each
//! run's own font rather than the box's.

use crate::node::typography::{GlyphRun, GlyphRunRef};
use skia_safe::{
    Canvas, Color, Color4f, Font, FontStyle, Paint, Path, PathBuilder,
    PathVerb, Point, Rect,
    paint::{Cap, Style},
    path_utils::fill_path_with_paint,
    textlayout::{
        FontArguments, FontCollection, TextDecoration, TextDecorationStyle,
    },
};
use std::ops::Range;

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
    pub(crate) fn first_available(
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
    pub(crate) color: Option<Color>,
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
/// named.
pub(crate) fn paint(
    canvas: &Canvas,
    spans: &[Span],
    boxes: &[Decoration],
    origin: Point,
    layer: Layer,
    base: &Paint,
) {
    for span in spans {
        let decoration = &boxes[span.box_index];
        for kind in layer.kinds() {
            if decoration.lines.contains(*kind) {
                draw_line(
                    canvas,
                    decoration,
                    *kind,
                    span,
                    origin,
                    base,
                    decoration.color.map(|color| {
                        let mut color = Color4f::from(color);
                        color.a *= decoration.opacity;
                        color
                    }),
                );
            }
        }
    }
}

/// Paints every line of box `box_index`'s spans in `shadow`, a text
/// shadow's paint, as is -- its colour, not the line's. CSS Text Decoration
/// 4, text-shadow: "Each layer shadows the element's text and all its text
/// decorations (composited together)".
pub(crate) fn paint_shadow(
    canvas: &Canvas,
    spans: &[Span],
    boxes: &[Decoration],
    box_index: usize,
    origin: Point,
    shadow: &Paint,
) {
    let decoration = &boxes[box_index];
    for span in spans.iter().filter(|span| span.box_index == box_index) {
        for kind in [
            TextDecoration::UNDERLINE,
            TextDecoration::OVERLINE,
            TextDecoration::LINE_THROUGH,
        ] {
            if decoration.lines.contains(kind) {
                draw_line(canvas, decoration, kind, span, origin, shadow, None);
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

fn draw_line(
    canvas: &Canvas,
    decoration: &Decoration,
    kind: TextDecoration,
    span: &Span,
    origin: Point,
    base: &Paint,
    color: Option<Color4f>,
) {
    let (top, t, out) = band(decoration, kind);
    if t.is_nan() || t <= 0.0 || span.right <= span.left {
        return;
    }
    let mut paint = base.clone();
    paint
        .set_shader(None)
        .set_anti_alias(true)
        .set_style(Style::Fill);
    if let Some(color) = color {
        paint.set_color4f(color, None);
    }
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
                let mut share = paint.clone();
                share.set_alpha_f(paint.alpha_f() * on / period);
                canvas.draw_rect(rect(left, right, top), &share);
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
/// half-waves to one per device pixel along it.
///
/// The stroke's outline, cut to its band: square at `left` and `right`,
/// where the butt caps meet the centreline at a slant and would otherwise
/// poke past the box, and along the top and bottom, which the stroke only
/// touches -- the crests swing `WAVE_SWING / 2` thicknesses off the middle
/// and the stroke adds half a thickness more, `WAVE_BAND / 2` -- but which
/// the stroker's curves overshoot by a fraction of a pixel. The cut is made
/// on the outline rather than by a clip, so that a filter or a shadow on the
/// paint spreads past the line as it spreads past a glyph.
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
    let band = WAVE_BAND * t;
    let bounds =
        Rect::new(left, middle - band / 2.0, right, middle + band / 2.0);
    Some(cut_to(&wave_stroke(left, right, middle, t), bounds))
}

/// The outline of a wave's stroke from `left` to `right` about `middle`,
/// `t` thick and butt-capped, before [`wave`] cuts it to its band.
fn wave_stroke(left: f32, right: f32, middle: f32, t: f32) -> Path {
    // A quadratic peaks at half its control point's offset, so a control
    // 2 * (swing / 2) off the centreline swings it by `swing` from crest to
    // trough.
    let control = WAVE_SWING * t;
    let half = WAVE_PERIOD * t / 2.0;
    let mut path = PathBuilder::new();
    path.move_to((left, middle));
    let mut x = left;
    let mut side = 1.0;
    while x < right {
        let crest = (x + half / 2.0, middle + side * control);
        let end = (x + half, middle);
        // The control point sits halfway along, so the curve's x is linear
        // in its parameter and the last half-wave stops at `right` where it
        // is split at `(right - x) / half`.
        match (right - x) / half {
            whole if whole >= 1.0 => {
                path.quad_to(crest, end);
            }
            part => {
                let lerp = |(ax, ay): (f32, f32), (bx, by): (f32, f32)| {
                    (ax + (bx - ax) * part, ay + (by - ay) * part)
                };
                let near = lerp((x, middle), crest);
                path.quad_to(near, lerp(near, lerp(crest, end)));
            }
        }
        x += half;
        side = -side;
    }
    let mut stroke = Paint::default();
    stroke
        .set_style(Style::Stroke)
        .set_stroke_width(t)
        .set_stroke_cap(Cap::Butt);
    let mut outline = PathBuilder::new();
    fill_path_with_paint(&path.detach(), &stroke, &mut outline, None, None);
    outline.detach()
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

/// `outline` cut to `bounds`, each cut a straight edge along the side it
/// is made on.
///
/// Each closed contour is clipped against one side at a time, as
/// Sutherland and Hodgman clip a polygon by a half-plane: a piece inside is
/// kept, a piece crossing is split where it crosses, and the stretch
/// outside is replaced by the straight edge along the side from where the
/// contour left to where it came back. Every point inside keeps the winding
/// number it had, so the outline fills as it did wherever it is kept, even
/// where a stroke's inner side folds over itself. Linear in the outline's
/// size, where a boolean operation rebuilds all of it.
///
/// The outline is the stroke of a quadratic centreline with butt caps and
/// the default join, which Skia's stroker draws in lines and quadratics
/// only; `a_wave_is_lines_and_quadratics` pins that.
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
/// `along` is not negative, the cut a straight edge -- one side of
/// [`cut_to`].
fn cut(
    pieces: &[Piece],
    along: impl Fn(Point) -> f32 + Copy,
    inside: impl Fn(f32) -> f32,
) -> Vec<Piece> {
    let mut kept: Vec<Piece> = Vec::with_capacity(pieces.len() + 2);
    // Where the contour last left the kept side, to join to where it
    // comes back.
    let mut left_at: Option<Point> = None;
    let rejoin = |kept: &mut Vec<Piece>, from: Option<Point>, to: Point| {
        if let Some(from) = from.filter(|from| *from != to) {
            kept.push(Piece::Line(from, to));
        }
    };
    for piece in pieces.iter().flat_map(|piece| piece.monotone(along)) {
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
    use skia_safe::{
        Color, Paint, PictureRecorder, Point, Rect,
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
            color: Some(Color::BLACK),
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

    /// A wave's uncut stroke and its band: the reference the cut one is
    /// held to, drawn under a clip to the band.
    fn stroke_and_band(
        left: f32,
        right: f32,
        middle: f32,
        t: f32,
    ) -> (skia_safe::Path, Rect) {
        let band = super::WAVE_BAND * t;
        (
            super::wave_stroke(left, right, middle, t),
            Rect::new(left, middle - band / 2.0, right, middle + band / 2.0),
        )
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
            skia_safe::surfaces::raster_n32_premul((w, h)).expect("a surface");
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
    /// box, where the cut goes through a butt cap, and along the band's
    /// edges, which the stroker's curves overshoot -- at lengths down to
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

    /// The stroker draws a wave's outline in lines and quadratics only,
    /// which is all `cut_to` takes: at every thickness, at lengths
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
        let mut surface =
            skia_safe::surfaces::raster_n32_premul((sw as i32, sh as i32))
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
}
