//! Paragraphs whose `oblique` ranges are drawn as CSS Fonts 4 matches them.
//!
//! A range asking for oblique is laid out in the family's face that leans as
//! asked where it has one, and otherwise upright and sheared by
//! `tan(14deg)`, the default angle -- the rule a canvas `oblique` follows,
//! `run_shear` in `node::typography`. It carries `slnt` among its font
//! arguments either way, so that SkParagraph shapes it as the font of its
//! own it is and no ligature forms across its edge -- see `add_slnt`.
//!
//! SkParagraph cannot shear a range of its own, and a paragraph is painted
//! by `Paragraph::paint`, which carries each range's colours, shadows and
//! decorations. It paints a line in passes -- backgrounds, shadows, glyphs,
//! decorations -- and the oblique ranges' shadows and glyphs, drawn a run
//! at a time with the paints SkParagraph would have used, have to go in
//! the same order. So a paragraph with an oblique range is built again from
//! the steps that made it, as passes:
//!
//! - where an oblique range casts a shadow, one with every glyph transparent
//!   and no decorations, which paints the backgrounds and the upright shadows,
//!   before the sheared shadows;
//! - one with the oblique ranges' glyphs transparent, which paints the upright
//!   glyphs -- and the backgrounds and upright shadows too where the first pass
//!   is not needed -- before the sheared glyphs;
//! - and, where any range is decorated, one with every glyph transparent and
//!   only the decorations, which paints every line after every glyph.
//!
//! A paragraph with no oblique range is built once, as it is filled in, and
//! painted by `Paragraph::paint` alone. The passes are the paragraph's, not
//! each line's: a pass for the whole paragraph comes before the next, where
//! SkParagraph finishes one line before it starts the following one. Lines
//! that overlap -- a descender reaching a background on the line below --
//! can therefore stack differently; ranges on one line paint as they would.

use crate::{
    context2d::DEFAULT_OBLIQUE_DEGREES,
    font::leans_as_asked,
    node::typography::{SLNT, Shear, run_shear, sheared_blob},
};
use skia_safe::{
    BlurStyle, Canvas, Color, Font, FontArguments, FontStyle, GlyphId,
    MaskFilter, Paint, Point,
    font_arguments::{VariationPosition, variation_position::Coordinate},
    font_style::Slant,
    textlayout::{
        FontCollection, Paragraph, ParagraphBuilder, ParagraphStyle,
        PlaceholderStyle, TextDecoration, TextShadow, TextStyle,
    },
};
use std::{borrow::Cow, iter::once, ops::Range};
use unicode_segmentation::UnicodeSegmentation;

/// The character SkParagraph adds to a paragraph's text for each
/// placeholder, U+FFFC OBJECT REPLACEMENT CHARACTER, whose UTF-8 length
/// moves every later text offset.
const PLACEHOLDER_CHARACTER: char = '\u{FFFC}';

/// The slant a style asking for `font_style` is laid out in, or `None`
/// where it does not ask for oblique.
///
/// `Oblique` where the family's face for an oblique request leans as asked
/// -- a face flagged oblique, or one whose `slnt` axis is already set --
/// and `Upright` otherwise, so that the face sheared is the upright one
/// rather than the italic Skia's matcher would hand back. CSS Fonts 4,
/// section 5.2, puts "a fallback match ... produced by geometric shearing to
/// the specified oblique value" before any italic face.
pub(crate) fn oblique_layout_slant(
    collection: &mut FontCollection,
    families: &[&str],
    font_style: FontStyle,
) -> Option<Slant> {
    if font_style.slant() != Slant::Oblique {
        return None;
    }
    let leans = collection
        .find_typefaces(families, font_style)
        .first()
        .is_some_and(|face| leans_as_asked(face, DEFAULT_OBLIQUE_DEGREES));
    Some(match leans {
        true => Slant::Oblique,
        false => Slant::Upright,
    })
}

/// Sets `style` to the slant [`oblique_layout_slant`] lays it out in, and
/// says whether it asks for oblique.
pub(crate) fn match_oblique(
    style: &mut TextStyle,
    collection: &mut FontCollection,
) -> bool {
    let font_style = style.font_style();
    if font_style.slant() != Slant::Oblique {
        return false;
    }
    let families = style.font_families();
    let families: Vec<&str> = families.iter().collect();
    let Some(slant) = oblique_layout_slant(collection, &families, font_style)
    else {
        return false;
    };
    style.set_font_style(FontStyle::new(
        font_style.weight(),
        font_style.width(),
        slant,
    ));
    true
}

/// Puts `slnt` among `coordinates`, at the default oblique angle, unless
/// they set it already: the font arguments of a style that asks for
/// oblique.
///
/// An oblique is a font of its own, and this is how SkParagraph is told so.
/// It shapes adjacent styles as one run wherever their font style,
/// families, size and font arguments agree, whatever else differs, so an
/// oblique range whose face is the upright one formed a ligature with the
/// text beside it -- "fi" across the edge drawn half sheared, half not. With
/// the coordinate, shaping breaks at the edge, as it does at any change of
/// font. A face without the axis ignores it.
pub(crate) fn add_slnt(coordinates: &mut Vec<Coordinate>) {
    if !coordinates.iter().any(|coordinate| coordinate.axis == SLNT) {
        coordinates.push(Coordinate {
            axis: SLNT,
            value: -DEFAULT_OBLIQUE_DEGREES,
        });
    }
}

/// Sets `style`'s font arguments to `coordinates`, or leaves it without any
/// where there are none.
pub(crate) fn set_coordinates(
    style: &mut TextStyle,
    coordinates: &[Coordinate],
) {
    if !coordinates.is_empty() {
        style.set_font_arguments(
            &FontArguments::new().set_variation_design_position(
                VariationPosition { coordinates },
            ),
        );
    }
}

/// One step of a paragraph's construction, as [`ParagraphRecipe`] keeps it.
enum Step {
    /// The style at this index of [`ParagraphRecipe::styles`] pushed.
    Push(usize),
    Pop,
    /// Text added, as UTF-8 offsets into the builder's text.
    Text(Range<usize>),
    Placeholder(PlaceholderStyle),
}

/// A paragraph under construction, built as it is filled in and kept so
/// that one with oblique ranges can be built again from the same steps --
/// see the module documentation.
///
/// The steps hold no text of their own, only where it lies in the
/// builder's, so a paragraph with no oblique range costs what building it
/// did before.
pub(crate) struct ParagraphRecipe {
    builder: ParagraphBuilder,
    style: ParagraphStyle,
    collection: FontCollection,
    steps: Vec<Step>,
    /// Each style pushed, and whether it asks for oblique.
    styles: Vec<(TextStyle, bool)>,
    /// The length of the builder's text, in UTF-8 bytes.
    length: usize,
    /// Whether the paragraph style's own text style asks for oblique.
    base_oblique: bool,
    /// Whether any style does, the paragraph's own included.
    oblique: bool,
}

impl ParagraphRecipe {
    /// A paragraph in `style`, whose text style says by `base_oblique`
    /// whether it asks for oblique -- [`match_oblique`]'s answer for it.
    pub(crate) fn new(
        style: ParagraphStyle,
        base_oblique: bool,
        collection: FontCollection,
    ) -> Self {
        Self {
            builder: ParagraphBuilder::new(&style, &collection),
            style,
            collection,
            steps: Vec::with_capacity(8),
            styles: Vec::with_capacity(4),
            length: 0,
            base_oblique,
            oblique: base_oblique,
        }
    }

    /// Pushes `style`, which `oblique` says asks for oblique.
    pub(crate) fn push_style(&mut self, style: TextStyle, oblique: bool) {
        self.builder.push_style(&style);
        self.oblique |= oblique;
        self.steps.push(Step::Push(self.styles.len()));
        self.styles.push((style, oblique));
    }

    pub(crate) fn pop(&mut self) {
        self.builder.pop();
        self.steps.push(Step::Pop);
    }

    pub(crate) fn add_text(&mut self, text: &str) {
        self.builder.add_text(text);
        let start = self.length;
        self.length += text.len();
        self.steps.push(Step::Text(start..self.length));
    }

    pub(crate) fn add_placeholder(&mut self, placeholder: &PlaceholderStyle) {
        self.builder.add_placeholder(placeholder);
        self.length += PLACEHOLDER_CHARACTER.len_utf8();
        self.steps.push(Step::Placeholder(placeholder.clone()));
    }

    /// The paragraph, not yet laid out, and what draws its oblique ranges
    /// where it has any.
    pub(crate) fn build(mut self) -> (Paragraph, Option<Sheared>) {
        if !self.oblique {
            return (self.builder.build(), None);
        }
        let text = self.builder.get_text().to_owned();
        self.keep_graphemes_whole(&text);
        let ranges = self.oblique_ranges();
        if ranges.is_empty() {
            return (self.builder.build(), None);
        }
        // SkParagraph paints a line in passes -- backgrounds, shadows,
        // glyphs, decorations -- so the sheared shadows go between the
        // upright ones and the upright glyphs, and every decoration after
        // every glyph. Each pass is a paragraph of its own only where
        // something needs to come between it and the next.
        let shadowed = ranges.iter().any(|range| !range.shadows.is_empty());
        let decorated = self.decorated();
        let under = shadowed.then(|| {
            self.replay(&text, |style, oblique| {
                let mut style = style.clone();
                style
                    .set_foreground_paint(&transparent())
                    .set_decoration_type(TextDecoration::NO_DECORATION);
                if oblique {
                    style.reset_shadows();
                }
                Some(style)
            })
        });
        let paragraph = self.replay(&text, |style, oblique| {
            if !(oblique || shadowed || decorated) {
                return None;
            }
            let mut style = style.clone();
            if oblique {
                style.set_foreground_paint(&transparent()).reset_shadows();
            }
            if shadowed {
                style.clear_background_color().reset_shadows();
            }
            if decorated {
                style.set_decoration_type(TextDecoration::NO_DECORATION);
            }
            Some(style)
        });
        let decorations = decorated.then(|| {
            self.replay(&text, |style, _| {
                let mut style = style.clone();
                style
                    .set_foreground_paint(&transparent())
                    .clear_background_color()
                    .reset_shadows();
                Some(style)
            })
        });
        let sheared = Sheared {
            ranges,
            under,
            decorations,
            runs: Vec::new(),
        };
        (paragraph, Some(sheared))
    }

    /// Moves each edge between an oblique and an upright piece of text that
    /// falls inside a grapheme cluster to the cluster's end -- see
    /// [`whole_graphemes`].
    fn keep_graphemes_whole(&mut self, text: &str) {
        let mut stack = vec![self.base_oblique];
        let mut pieces = Vec::new();
        for (at, step) in self.steps.iter().enumerate() {
            match step {
                Step::Push(style) => stack.push(self.styles[*style].1),
                Step::Pop if stack.len() > 1 => {
                    stack.pop();
                }
                Step::Pop | Step::Placeholder(_) => {}
                Step::Text(bytes) => {
                    pieces.push(Piece {
                        step: at,
                        bytes: bytes.clone(),
                        // SAFETY: `stack` starts with the base style's flag
                        // and a pop never takes it, so it is never empty.
                        oblique: *stack.last().unwrap(),
                    });
                }
            }
        }
        whole_graphemes(text, &mut pieces);
        for piece in pieces {
            self.steps[piece.step] = Step::Text(piece.bytes);
        }
        debug_assert!(
            self.covers(text),
            "the pieces of text cover the builder's, byte for byte"
        );
    }

    /// Whether the text steps and placeholders, in order, are exactly
    /// `text`: each piece starting where the one before it ended.
    fn covers(&self, text: &str) -> bool {
        let mut at = 0;
        for step in &self.steps {
            match step {
                Step::Text(bytes) if bytes.start == at => at = bytes.end,
                Step::Text(_) => return false,
                Step::Placeholder(_) => at += PLACEHOLDER_CHARACTER.len_utf8(),
                Step::Push(_) | Step::Pop => {}
            }
        }
        at == text.len()
    }

    /// Whether any style decorates its text, the paragraph's own included.
    fn decorated(&self) -> bool {
        once(self.style.text_style())
            .chain(self.styles.iter().map(|(style, _)| style))
            .any(|style| {
                style.decoration_type() != TextDecoration::NO_DECORATION
            })
    }

    /// The text of every oblique range, in UTF-8 offsets into the
    /// paragraph's text, in order, with what painting it needs.
    ///
    /// A pop with nothing pushed is ignored, as SkParagraph's builder
    /// ignores it.
    fn oblique_ranges(&self) -> Vec<ObliqueRange> {
        let mut stack = vec![(self.style.text_style(), self.base_oblique)];
        let mut ranges = Vec::new();
        for step in &self.steps {
            match step {
                Step::Push(at) => {
                    let (style, oblique) = &self.styles[*at];
                    stack.push((style, *oblique));
                }
                Step::Pop if stack.len() > 1 => {
                    stack.pop();
                }
                Step::Pop | Step::Placeholder(_) => {}
                Step::Text(bytes) => {
                    // SAFETY: `stack` starts with the base style and a pop
                    // never takes it, so it is never empty.
                    let (style, oblique) = stack.last().unwrap();
                    if *oblique && !bytes.is_empty() {
                        ranges.push(ObliqueRange::new(bytes.clone(), style));
                    }
                }
            }
        }
        ranges
    }

    /// A paragraph from these steps and `text`, the builder's, with each
    /// style -- the paragraph style's own included -- replaced by what
    /// `restyle` makes of it and of whether it asks for oblique, where that
    /// is `Some`.
    fn replay(
        &self,
        text: &str,
        restyle: impl Fn(&TextStyle, bool) -> Option<TextStyle>,
    ) -> Paragraph {
        let style = match restyle(self.style.text_style(), self.base_oblique) {
            Some(base) => {
                let mut style = self.style.clone();
                style.set_text_style(&base);
                Cow::Owned(style)
            }
            None => Cow::Borrowed(&self.style),
        };
        let mut builder = ParagraphBuilder::new(&style, &self.collection);
        for step in &self.steps {
            match step {
                Step::Push(at) => {
                    let (style, oblique) = &self.styles[*at];
                    match restyle(style, *oblique) {
                        Some(style) => builder.push_style(&style),
                        None => builder.push_style(style),
                    };
                }
                Step::Pop => {
                    builder.pop();
                }
                Step::Text(bytes) => {
                    builder.add_text(&text[bytes.clone()]);
                }
                Step::Placeholder(placeholder) => {
                    builder.add_placeholder(placeholder);
                }
            }
        }
        builder.build()
    }
}

/// One piece of a paragraph's text: the step that adds it, its bytes, and
/// whether its style asks for oblique.
struct Piece {
    step: usize,
    bytes: Range<usize>,
    oblique: bool,
}

/// Moves each edge between two adjacent pieces of `text`, one oblique and
/// one not, that falls inside a grapheme cluster to the cluster's end, so
/// that the style of the cluster's base takes the whole of it.
///
/// Grapheme clusters rather than SkParagraph's glyph clusters, which cannot
/// tell the two cases at an oblique edge apart: a ligature across the edge
/// is one glyph cluster and should break, since an oblique is a font of its
/// own, and a base and its combining marks are one too and should not,
/// since a mark shaped apart from its base loses the glyph it attaches to;
/// the base decides which glyph that is, so its style is the cluster's.
///
/// Only where one side is oblique: elsewhere SkParagraph shapes the two
/// pieces as one and paints each its own way, as it always has. And only
/// between two pieces that meet: a placeholder stands in the text as
/// U+FFFC, which no piece holds, so the pieces either side of one do not
/// meet and its edges never move. A mark right after one is a grapheme
/// with it and has no base to go with, so it stays in its own piece and is
/// shaped alone, as SkParagraph shapes it. Edges only move later and never
/// past the next, so the pieces still hold the same bytes.
fn whole_graphemes(text: &str, pieces: &mut [Piece]) {
    let starts: Vec<usize> =
        text.grapheme_indices(true).map(|(at, _)| at).collect();
    let cluster_end = |at: usize| match starts.binary_search(&at) {
        Ok(_) => at,
        Err(next) => starts.get(next).copied().unwrap_or(text.len()),
    };
    // The piece holding the base of the cluster at the edge being looked
    // at: the last one with any text, since a piece the edges before have
    // emptied holds none of it.
    let mut owner = 0;
    for at in 1..pieces.len() {
        let right = &pieces[at];
        let joined = pieces[owner].bytes.end == right.bytes.start;
        if joined && pieces[owner].oblique != right.oblique {
            let edge = cluster_end(right.bytes.start).min(right.bytes.end);
            pieces[owner].bytes.end = edge;
            for emptied in &mut pieces[owner + 1..at] {
                emptied.bytes = edge..edge;
            }
            pieces[at].bytes.start = edge;
        }
        if !joined || !pieces[at].bytes.is_empty() {
            owner = at;
        }
    }
}

/// A paint that draws nothing, for a glyph or a decoration painted
/// elsewhere.
fn transparent() -> Paint {
    let mut paint = Paint::default();
    paint.set_color(Color::TRANSPARENT);
    paint
}

/// The text of one oblique range and what painting it takes.
struct ObliqueRange {
    /// UTF-8 offsets into the paragraph's text.
    bytes: Range<usize>,
    /// The paint SkParagraph draws the range's glyphs with: its foreground
    /// where it has one, and otherwise a default paint in its colour.
    foreground: Paint,
    shadows: Vec<TextShadow>,
}

impl ObliqueRange {
    fn new(bytes: Range<usize>, style: &TextStyle) -> Self {
        let foreground = match style.has_foreground() {
            true => style.foreground(),
            false => {
                let mut paint = Paint::default();
                paint.set_color(style.color());
                paint
            }
        };
        Self {
            bytes,
            foreground,
            shadows: style.shadows().to_vec(),
        }
    }
}

/// What draws a paragraph's oblique ranges: their glyph runs, found again at
/// each layout, and the paragraph their decorations come from.
pub(crate) struct Sheared {
    ranges: Vec<ObliqueRange>,
    /// The backgrounds and the upright shadows, where an oblique range
    /// casts a shadow to go between them and the upright glyphs.
    under: Option<Paragraph>,
    /// Every decoration, where any style has one, to go over every glyph.
    decorations: Option<Paragraph>,
    runs: Vec<ShearedRun>,
}

/// The glyphs of one oblique range on one line in one font.
struct ShearedRun {
    font: Font,
    glyphs: Vec<GlyphId>,
    positions: Vec<Point>,
    origin: Point,
    /// The index of the range in [`Sheared::ranges`].
    range: usize,
    shear: Shear,
}

impl Sheared {
    /// Follows `paragraph`, just laid out at `width`: lays the decorations
    /// out at the same width, and finds the oblique ranges' glyphs again.
    ///
    /// The glyphs are read with `Paragraph::visit`, which reports the text
    /// blobs `Paragraph::paint` draws: each glyph where it is painted and
    /// the text it comes from, on every line, and nothing for a
    /// placeholder. `extended_visit` reports less than it paints, so it is
    /// not used: a glyph's position without the offset the shaper gave it,
    /// which is where a mark stacks or sits over its base, and the text
    /// offsets of a run continued onto a later line restarting at those
    /// of its first -- see `cluster_under`.
    ///
    /// A run's first glyph says whether it is oblique: an oblique style is a
    /// font of its own, so no run holds oblique text and upright text both.
    /// Each glyph then takes the range its text is in, or the one before
    /// it where its text is in none: the glyphs of an ellipsis or a hyphen
    /// report offsets counted from the text they stand after, which can run
    /// past it.
    pub(crate) fn lay_out(&mut self, paragraph: &mut Paragraph, width: f32) {
        for pass in [&mut self.under, &mut self.decorations]
            .into_iter()
            .flatten()
        {
            pass.layout(width);
        }
        let mut visited = Vec::new();
        paragraph.visit(|_, visit| {
            if let Some(info) = visit {
                let count = info.glyphs().len();
                visited.push((
                    info.font().clone(),
                    info.glyphs().to_vec(),
                    info.positions().to_vec(),
                    info.origin(),
                    info.utf8_starts()[..count].to_vec(),
                ));
            }
        });
        self.runs = visited
            .into_iter()
            .flat_map(|(font, glyphs, positions, origin, starts)| {
                let range = |start: &u32| self.range_of(*start as usize);
                let Some(first) = starts.first().and_then(range) else {
                    return Vec::new();
                };
                let ranges: Vec<usize> = starts
                    .iter()
                    .scan(first, |last, start| {
                        *last = range(start).unwrap_or(*last);
                        Some(*last)
                    })
                    .collect();
                let shear =
                    run_shear(&font.typeface(), Some(DEFAULT_OBLIQUE_DEGREES));
                ranges
                    .chunk_by(|a, b| a == b)
                    .scan(0, |start, chunk| {
                        let at = *start..*start + chunk.len();
                        *start = at.end;
                        Some((at, chunk[0]))
                    })
                    .map(|(at, range)| ShearedRun {
                        font: font.clone(),
                        glyphs: glyphs[at.clone()].to_vec(),
                        positions: positions[at].to_vec(),
                        origin,
                        range,
                        shear,
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
    }

    /// The index of the oblique range holding the text at `byte`.
    fn range_of(&self, byte: usize) -> Option<usize> {
        let after =
            self.ranges.partition_point(|range| range.bytes.end <= byte);
        self.ranges
            .get(after)
            .filter(|range| range.bytes.contains(&byte))
            .map(|_| after)
    }

    /// Paints `paragraph`, which this was laid out with, at `origin`: the
    /// paragraph, then the oblique ranges' shadows, their glyphs and their
    /// decorations.
    pub(crate) fn paint(
        &self,
        paragraph: &Paragraph,
        canvas: &Canvas,
        origin: Point,
    ) {
        if let Some(under) = &self.under {
            under.paint(canvas, origin);
        }
        for run in &self.runs {
            for shadow in &self.ranges[run.range].shadows {
                if !shadow.has_shadow() {
                    continue;
                }
                // `CanvasParagraphPainter::drawTextShadow`'s paint.
                let mut paint = Paint::default();
                paint.set_color(shadow.color);
                if shadow.blur_sigma != 0.0 {
                    paint.set_mask_filter(MaskFilter::blur(
                        BlurStyle::Normal,
                        shadow.blur_sigma as f32,
                        false,
                    ));
                }
                run.draw(canvas, origin + shadow.offset, &paint);
            }
        }
        paragraph.paint(canvas, origin);
        for run in &self.runs {
            run.draw(canvas, origin, &self.ranges[run.range].foreground);
        }
        if let Some(decorations) = &self.decorations {
            decorations.paint(canvas, origin);
        }
    }
}

impl ShearedRun {
    /// Draws the run with the paragraph's origin at `origin`, sheared about
    /// its baseline -- see `sheared_blob`.
    fn draw(&self, canvas: &Canvas, origin: Point, paint: &Paint) {
        if let Some(blob) =
            sheared_blob(&self.font, &self.glyphs, &self.positions, self.shear)
        {
            canvas.draw_text_blob(blob, origin + self.origin, paint);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Piece, whole_graphemes};
    use skia_safe::{
        Data, FontMgr,
        textlayout::{
            FontCollection, Paragraph, ParagraphBuilder, ParagraphStyle,
            TextStyle, TypefaceFontProvider,
        },
    };
    use std::ops::Range;

    /// The pieces of `text` at `ranges`, each oblique or not, and the text
    /// each holds once its edges have moved. Between two ranges that do not
    /// meet lies text no piece holds, as a placeholder's U+FFFC is.
    fn moved(text: &str, ranges: &[(Range<usize>, bool)]) -> Vec<String> {
        let mut pieces: Vec<Piece> = ranges
            .iter()
            .enumerate()
            .map(|(at, (bytes, oblique))| Piece {
                step: at,
                bytes: bytes.clone(),
                oblique: *oblique,
            })
            .collect();
        let held = |pieces: &[Piece]| -> Vec<usize> {
            pieces.iter().flat_map(|p| p.bytes.clone()).collect()
        };
        let before = held(&pieces);
        whole_graphemes(text, &mut pieces);
        assert_eq!(held(&pieces), before, "the same bytes, each held once");
        pieces
            .iter()
            .map(|p| text[p.bytes.clone()].to_owned())
            .collect()
    }

    const ACUTE: &str = "\u{301}";
    const DIAERESIS: &str = "\u{308}";

    #[test]
    fn a_mark_given_to_the_other_side_of_an_oblique_edge_goes_with_its_base() {
        let text = format!("cafe{ACUTE} x");
        let n = text.len();
        assert_eq!(
            moved(&text, &[(0..4, false), (4..n, true)]),
            [format!("cafe{ACUTE}"), " x".to_owned()]
        );
        assert_eq!(
            moved(&text, &[(0..4, true), (4..n, false)]),
            [format!("cafe{ACUTE}"), " x".to_owned()],
            "the other way round"
        );
    }

    #[test]
    fn marks_split_over_several_pieces_go_with_their_base_where_needed() {
        // An upright "e", an oblique acute and an upright diaeresis: the
        // acute goes to the "e", emptying the oblique piece, and the
        // diaeresis is then upright beside an upright base, an edge that
        // does not move -- SkParagraph shapes the two as one.
        let text = format!("e{ACUTE}{DIAERESIS}x");
        let (acute, n) = (1 + ACUTE.len(), text.len());
        assert_eq!(
            moved(&text, &[(0..1, false), (1..acute, true), (acute..n, false)]),
            [format!("e{ACUTE}"), String::new(), format!("{DIAERESIS}x")]
        );
        // An oblique diaeresis instead: it goes to the "e" too, past the
        // emptied piece.
        assert_eq!(
            moved(&text, &[(0..1, false), (1..acute, true), (acute..n, true)]),
            [
                format!("e{ACUTE}{DIAERESIS}"),
                String::new(),
                "x".to_owned()
            ]
        );
    }

    #[test]
    fn only_an_edge_with_an_oblique_side_moves() {
        let text = format!("cafe{ACUTE} x");
        let n = text.len();
        assert_eq!(
            moved(&text, &[(0..4, false), (4..n, false)]),
            ["cafe".to_owned(), format!("{ACUTE} x")]
        );
    }

    #[test]
    fn nothing_moves_across_a_placeholder() {
        // "a", a placeholder's U+FFFC, which no piece holds, and an oblique
        // acute and "x": the acute joins the U+FFFC in one grapheme and
        // has no base to go with.
        let text = format!("a\u{FFFC}{ACUTE}x");
        let (mark, n) = (1 + '\u{FFFC}'.len_utf8(), text.len());
        assert_eq!(
            moved(&text, &[(0..1, false), (mark..n, true)]),
            ["a".to_owned(), format!("{ACUTE}x")]
        );
    }

    #[test]
    fn a_ligature_edge_stays_where_it_is() {
        assert_eq!(moved("effi", &[(0..2, false), (2..4, true)]), ["ef", "fi"]);
    }

    /// A paragraph of bundled Oswald at 200px, filled in by `fill`.
    fn oswald_paragraph(
        fill: impl FnOnce(&mut ParagraphBuilder, &TextStyle),
    ) -> Paragraph {
        const FAMILY: &str = "OswaldWrapped";
        let bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/assets/fonts/Oswald/Oswald-VariableFont_wght.ttf"
        ))
        .expect("bundled Oswald reads");
        let face = FontMgr::new()
            .new_from_data(Data::new_copy(&bytes), None)
            .expect("bundled Oswald loads");
        let mut provider = TypefaceFontProvider::new();
        provider.register_typeface(face, Some(FAMILY));
        let mut collection = FontCollection::new();
        collection.set_asset_font_manager(Some(provider.into()));

        let mut text_style = TextStyle::new();
        text_style.set_font_families(&[FAMILY]).set_font_size(200.0);
        let mut style = ParagraphStyle::new();
        style.set_text_style(&text_style);
        let mut builder = ParagraphBuilder::new(&style, collection);
        fill(&mut builder, &text_style);
        builder.build()
    }

    /// The second defect `cluster_under` works around, measured directly:
    /// for a run that does not start the text, the offset after its last
    /// glyph -- the end of the run -- is not the end of the run.
    ///
    /// Skia copies the run's `count` offsets into storage of that size and
    /// hands out `count + 1`, so the last is read from past what it wrote;
    /// its value is whatever that storage held. Red once Skia writes it,
    /// which is when the cluster lookup can go.
    #[test]
    fn skia_still_misreports_utf8_starts() {
        let mut paragraph = oswald_paragraph(|builder, style| {
            // A size of its own makes the second piece a run of its own,
            // starting at the third byte.
            builder.add_text("IH");
            let mut larger = style.clone();
            larger.set_font_size(120.0);
            builder.push_style(&larger);
            builder.add_text("IH");
        });
        paragraph.layout(1000.0);
        let mut second = None;
        paragraph.extended_visit(|_, visit| {
            if let Some(info) = visit
                && info.utf8_starts()[0] == 2
            {
                second = Some(info.utf8_starts().to_vec());
            }
        });
        let starts = second.expect("the second piece is a run of its own");
        assert_eq!(starts[..2], [2, 3], "the glyphs' own offsets are right");
        assert_ne!(starts[2], 4, "and the run's end is not");
    }

    /// The defect `cluster_under` works around, measured directly: a run
    /// continued on a second line reports text offsets from its first
    /// glyph, not from the first glyph of that line's part of it.
    ///
    /// Red once Skia offsets them, which is when the cluster lookup can go.
    #[test]
    fn skia_still_restarts_utf8_starts_on_a_wrapped_line() {
        let mut paragraph = oswald_paragraph(|builder, _| {
            // One run in one font, wrapped after the space.
            builder.add_text("II II");
        });
        paragraph.layout(130.0);
        assert_eq!(paragraph.line_number(), 2, "the run wraps once");

        let mut second = None;
        paragraph.extended_visit(|line, visit| {
            if let (1, Some(info)) = (line, visit) {
                let first = info.origin() + info.positions()[0];
                second = Some((info.utf8_starts()[0], first));
            }
        });
        let (reported, first) = second.expect("the second line has a run");
        let cluster = paragraph
            .get_closest_glyph_cluster_at((first.x + 1.0, first.y))
            .expect("a glyph is under the second line's start");
        assert_eq!(
            cluster.text_range.start, 3,
            "the second line starts at the third byte"
        );
        assert_eq!(
            reported, 0,
            "and SkParagraph reports the first byte of the whole run"
        );
    }
}
