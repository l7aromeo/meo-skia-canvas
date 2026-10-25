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

use crate::node::typography::GlyphRun;
use skia_safe::{
    Canvas, ClipOp, Color, Font, FontStyle, Paint, PathBuilder, Point, Rect,
    paint::{Cap, Style},
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

/// Where a face puts its lines, in pixels at its size, each measured down
/// from the baseline as Skia's y is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct FaceLines {
    underline_top: f32,
    underline_thickness: f32,
    strikeout_top: f32,
    strikeout_thickness: f32,
    ascent: f32,
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
        }
    }

    /// The lines of the first available font of a style in `families` at
    /// `size`: CSS Fonts 4, section 5.2, "the first font for which the
    /// character U+0020 (space) is not excluded by a unicode-range, given
    /// the font families in the font-family list". Nothing here has a
    /// `unicode-range`, so it is the first family there is, whatever its
    /// character map holds -- Chrome 153 draws an underline from a first
    /// family with no space in it. Where no family is there, the
    /// collection's default fallback, which draws the text; `None` where
    /// there is none either.
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
        // platform font each time -- on macOS a CoreText one, which made a
        // decorated paragraph cost two thirds as much again to draw.
        let typeface = families
            .iter()
            .find_map(|family| {
                collection
                    .find_typefaces_with_font_arguments(
                        &[*family],
                        style,
                        arguments,
                    )
                    .into_iter()
                    .next()
            })
            .or_else(|| {
                let fallback = collection.default_fallback()?;
                Some(
                    arguments
                        .and_then(|a| a.clone_typeface(fallback.clone()))
                        .unwrap_or(fallback),
                )
            })?;
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
    pub(crate) color: Color,
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
/// in the paragraph's coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Span {
    pub(crate) left: f32,
    pub(crate) right: f32,
    pub(crate) y: f32,
    pub(crate) box_index: usize,
}

/// The span each box covers on each line, from `runs`, the glyph runs
/// `Paragraph::paint` draws -- see `glyph_runs`, which leave out a line's
/// trailing spaces as SkParagraph's own decorations did. `box_of` names the
/// box the text at a UTF-8 offset belongs to, or `None` for text with no
/// decoration.
///
/// A glyph at a time rather than a run: SkParagraph shapes adjacent styles
/// in one font as one run, so a run can hold text of two boxes, or of a box
/// and none. HarfBuzz gives a run's glyphs in visual order, so a box's
/// glyphs in a run lie side by side, and its span reaches the next glyph's
/// position or, for a run's last, the run's `advance_x`. A run's origin is
/// the line's, its baseline already rounded to the pixel `Paragraph::paint`
/// draws on.
pub(crate) fn spans(
    runs: &[GlyphRun],
    box_of: impl Fn(usize) -> Option<usize>,
) -> Vec<Span> {
    let mut spans: Vec<(usize, Span)> = Vec::new();
    for run in runs {
        let boxes: Vec<Option<usize>> = run
            .starts
            .iter()
            .map(|start| box_of(*start as usize))
            .collect();
        let mut at = 0;
        for chunk in boxes.chunk_by(|a, b| a == b) {
            let glyphs = at..at + chunk.len();
            at = glyphs.end;
            let Some(box_index) = chunk[0] else { continue };
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
                box_index,
            };
            // Runs of one box on one line join into one span, so a dotted
            // line keeps its phase across a change of font.
            match spans.last_mut() {
                Some((line, last))
                    if *line == run.line
                        && last.box_index == box_index
                        && last.y == span.y =>
                {
                    last.left = last.left.min(span.left);
                    last.right = last.right.max(span.right);
                }
                _ => spans.push((run.line, span)),
            }
        }
    }
    spans.into_iter().map(|(_, span)| span).collect()
}

/// Paints `layer`'s lines of every span, offset by `origin`, each in its
/// box's colour on `base` -- the glyphs' paint, so that a canvas's alpha,
/// compositing, filter and shadow reach the lines as they reach the text.
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
                    decoration.color,
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
                draw_line(
                    canvas,
                    decoration,
                    kind,
                    span,
                    origin,
                    shadow,
                    shadow.color(),
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

fn draw_line(
    canvas: &Canvas,
    decoration: &Decoration,
    kind: TextDecoration,
    span: &Span,
    origin: Point,
    base: &Paint,
    color: Color,
) {
    let (top, t, out) = band(decoration, kind);
    if t.is_nan() || t <= 0.0 || span.right <= span.left {
        return;
    }
    let mut paint = base.clone();
    paint
        .set_shader(None)
        .set_color(color)
        .set_anti_alias(true)
        .set_style(Style::Fill);
    let left = origin.x + span.left;
    let right = origin.x + span.right;
    let top = origin.y + span.y + top;
    let rect = |l: f32, r: f32, y: f32| Rect::new(l, y, r.min(right), y + t);
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
            // A quadratic peaks at half its control point's offset, so a
            // control 2 * (swing / 2) off the centreline swings it by
            // `swing` from crest to trough.
            let control = WAVE_SWING * t;
            let half = WAVE_PERIOD * t / 2.0;
            let mut path = PathBuilder::new();
            path.move_to((left, middle));
            let mut x = left;
            let mut side = 1.0;
            while x < right {
                path.quad_to(
                    (x + half / 2.0, middle + side * control),
                    (x + half, middle),
                );
                x += half;
                side = -side;
            }
            paint
                .set_style(Style::Stroke)
                .set_stroke_width(t)
                .set_stroke_cap(Cap::Butt);
            canvas.save();
            canvas.clip_rect(
                Rect::new(left, band_top, right, band_top + band),
                ClipOp::Intersect,
                true,
            );
            canvas.draw_path(&path.detach(), &paint);
            canvas.restore();
        }
        _ => {
            canvas.draw_rect(rect(left, right, top), &paint);
        }
    }
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
