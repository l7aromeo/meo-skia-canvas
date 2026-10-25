//! Paragraphs whose `oblique` ranges are drawn as CSS Fonts 4 matches them,
//! and whose decorations are drawn where their fonts put them.
//!
//! A range asking for oblique is laid out in the family's face that leans as
//! asked where it has one, and otherwise upright and sheared by
//! `tan(14deg)`, the default angle -- the rule a canvas `oblique` follows,
//! `run_shear` in `node::typography`. It carries `slnt` among its font
//! arguments either way, so that SkParagraph shapes it as the font of its
//! own it is and no ligature forms across its edge -- see `add_slnt`.
//!
//! Decorations and text shadows are taken off every style as it is pushed.
//! The lines are drawn by `node::decorate`, which places each by its style's
//! first available font -- see the `UPSTREAM:` note there for why not
//! SkParagraph's -- and the shadows from the glyph runs `Paragraph::paint`
//! draws, with the paint SkParagraph uses, so that the lines go beneath them
//! and a shadow shadows the lines too.
//!
//! SkParagraph cannot shear a range of its own, and a paragraph is painted
//! by `Paragraph::paint`, which carries each range's colours and
//! backgrounds. It paints a line's backgrounds before its glyphs, and the
//! shadows, the lines beneath the glyphs, the sheared glyphs and the
//! lines-through have to go in CSS's order around them -- see
//! [`Passes::paint`]. So a paragraph with an oblique range, a decoration or
//! a shadow is painted in passes, built again from the steps that made it
//! where a pass needs styles of its own:
//!
//! - where a paragraph with a background has a shadow or a decoration to go
//!   over it, one with every glyph transparent, which paints the backgrounds;
//! - one with the oblique ranges' glyphs transparent, which paints the upright
//!   glyphs, and the backgrounds where the first pass is not needed.
//!
//! A paragraph with none of the three is built once, as it is filled in, and
//! painted by `Paragraph::paint` alone; so is one whose passes need no styles
//! of their own. The passes are the paragraph's, not each line's: a pass for
//! the whole paragraph comes before the next, where SkParagraph finishes one
//! line before it starts the following one. Lines that overlap -- a
//! descender reaching a background on the line below -- can therefore stack
//! differently; ranges on one line paint as they would.

use crate::{
    context2d::DEFAULT_OBLIQUE_DEGREES,
    font::leans_as_asked,
    node::{
        decorate::{self, Decoration, FaceLines, Layer, Span, Thickness},
        typography::{
            GlyphRun, SLNT, Shear, glyph_runs, run_shear, sheared_blob,
        },
    },
};
use skia_safe::{
    BlurStyle, Canvas, Color, FontArguments, FontStyle, MaskFilter, Paint,
    Point, TextBlob,
    font_arguments::{VariationPosition, variation_position::Coordinate},
    font_style::Slant,
    textlayout::{
        FontCollection, Paragraph, ParagraphBuilder, ParagraphStyle,
        PlaceholderStyle, TextDecoration, TextShadow, TextStyle,
    },
};
use std::{
    borrow::Cow,
    iter::{once, zip},
    ops::Range,
};
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
    /// Each style pushed, without its decoration or its shadows, and
    /// whether it asks for oblique.
    styles: Vec<(TextStyle, bool)>,
    /// Each style's decoration and shadows, by slot: the paragraph style's
    /// own text style's at 0 and each pushed style's after it, at its index
    /// in `styles` plus 1. Both are drawn here rather than by SkParagraph --
    /// see [`Passes`].
    decorations: Vec<Option<StyleDecoration>>,
    shadows: Vec<Vec<TextShadow>>,
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
        mut style: ParagraphStyle,
        base_oblique: bool,
        mut collection: FontCollection,
    ) -> Self {
        let mut base = style.text_style().clone();
        let decoration = StyleDecoration::take(&mut base, &mut collection);
        let shadows = take_shadows(&mut base);
        if decoration.is_some() || !shadows.is_empty() {
            style.set_text_style(&base);
        }
        let mut decorations = Vec::with_capacity(4);
        decorations.push(decoration);
        let mut shadow_slots = Vec::with_capacity(4);
        shadow_slots.push(shadows);
        Self {
            builder: ParagraphBuilder::new(&style, &collection),
            style,
            collection,
            steps: Vec::with_capacity(8),
            styles: Vec::with_capacity(4),
            decorations,
            shadows: shadow_slots,
            length: 0,
            base_oblique,
            oblique: base_oblique,
        }
    }

    /// Pushes `style`, which `oblique` says asks for oblique.
    pub(crate) fn push_style(&mut self, mut style: TextStyle, oblique: bool) {
        let decoration =
            StyleDecoration::take(&mut style, &mut self.collection);
        let shadows = take_shadows(&mut style);
        self.builder.push_style(&style);
        self.oblique |= oblique;
        self.steps.push(Step::Push(self.styles.len()));
        self.styles.push((style, oblique));
        self.decorations.push(decoration);
        self.shadows.push(shadows);
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

    /// The paragraph, not yet laid out, and what draws its oblique ranges,
    /// its shadows and its decorations where it has any.
    pub(crate) fn build(mut self) -> (Paragraph, Option<Passes>) {
        let (boxes, box_slots) = self.boxes();
        let shadowed = self.shadows.iter().any(|shadows| !shadows.is_empty());
        if !self.oblique && boxes.is_empty() && !shadowed {
            return (self.builder.build(), None);
        }
        let text = self.builder.get_text().to_owned();
        // The builder holds the text where it was added, so once an edge
        // has moved, only the steps say where it went.
        let moved = self.oblique && self.keep_graphemes_whole(&text);
        let ranges = match self.oblique {
            true => self.oblique_ranges(),
            false => Vec::new(),
        };
        if ranges.is_empty() && boxes.is_empty() && !shadowed {
            let paragraph = match moved {
                true => self.replay(&text, |_, _, _| None),
                false => self.builder.build(),
            };
            return (paragraph, None);
        }
        let backed = self.text_styles().any(TextStyle::has_background);
        if backed && ranges.is_empty() && boxes.is_empty() {
            // Shadows alone, which go between the backgrounds and the
            // glyphs, where SkParagraph draws them itself: given back, so
            // that the paragraph is laid out once.
            let paragraph = self.replay(&text, |style, _, slot| {
                restore_shadows(style, &self.shadows[slot])
            });
            return (paragraph, None);
        }
        // SkParagraph paints a line's backgrounds before its glyphs, and
        // the shadows and the lines beneath the glyphs go between the two.
        // Where there are backgrounds to go under them, they come from a
        // paragraph of their own, laid out a second time: a decorated
        // paragraph with a background cost 12.7 microseconds a draw before
        // decorations were drawn here and 17.3 with it, in a release build
        // at 24px.
        //
        // Not drawn here instead, because they cannot be drawn exactly as
        // `TextLine::paintBackground` draws them: its rectangle spans a run's
        // `runTop` and `calculateHeight`, from the line's rounded sizes and
        // each run's ascent corrected for its height multiplier, half
        // leading and baseline shift, across the run's advances before the
        // shaper's offsets, with cluster edges apportioned. `Paragraph::visit`
        // reports glyph positions with the offsets already in them and none
        // of the line's sizes.
        let under = (backed && (shadowed || !boxes.is_empty())).then(|| {
            self.replay(&text, |style, _, _| {
                let mut style = style.clone();
                style.set_foreground_paint(&transparent());
                Some(style)
            })
        });
        let split = under.is_some();
        let paragraph = match moved || !ranges.is_empty() || split {
            true => self.replay(&text, |style, oblique, _| {
                if !(oblique || split) {
                    return None;
                }
                let mut style = style.clone();
                if oblique {
                    style.set_foreground_paint(&transparent());
                }
                if split {
                    style.clear_background_color();
                }
                Some(style)
            }),
            false => self.builder.build(),
        };
        let shadow_slots: Vec<Option<usize>> = self
            .shadows
            .iter()
            .enumerate()
            .map(|(slot, shadows)| (!shadows.is_empty()).then_some(slot))
            .collect();
        let (boxes, box_shadows) = boxes
            .into_iter()
            .map(|decorated| (decorated.decoration, decorated.shadows))
            .unzip();
        let passes = Passes {
            #[cfg(test)]
            as_asked: self.as_asked(&text),
            box_ranges: self.slot_ranges(&box_slots),
            shadow_ranges: self.slot_ranges(&shadow_slots),
            shadows: self.shadows,
            ranges,
            under,
            boxes,
            box_shadows,
            runs: Vec::new(),
            spans: Vec::new(),
        };
        (paragraph, Some(passes))
    }

    /// Every style, the paragraph's own first.
    fn text_styles(&self) -> impl Iterator<Item = &TextStyle> {
        once(self.style.text_style())
            .chain(self.styles.iter().map(|(style, _)| style))
    }

    /// Each decorated style's decoration, by slot, and which box each slot
    /// is: `None` for one with no decoration.
    fn boxes(&self) -> (Vec<StyleDecoration>, Vec<Option<usize>>) {
        let mut boxes = Vec::new();
        let slots = self
            .decorations
            .iter()
            .map(|decoration| {
                decoration.as_ref().map(|decoration| {
                    boxes.push(decoration.clone());
                    boxes.len() - 1
                })
            })
            .collect();
        (boxes, slots)
    }

    /// The text each slot's style covers, in UTF-8 offsets into the
    /// paragraph's text, in order, with what `slots` maps the slot to,
    /// leaving out a slot it maps to `None`.
    fn slot_ranges(
        &self,
        slots: &[Option<usize>],
    ) -> Vec<(Range<usize>, usize)> {
        let mut stack = vec![slots[0]];
        let mut ranges: Vec<(Range<usize>, usize)> = Vec::new();
        for step in &self.steps {
            match step {
                Step::Push(at) => stack.push(slots[at + 1]),
                Step::Pop if stack.len() > 1 => {
                    stack.pop();
                }
                Step::Pop | Step::Placeholder(_) => {}
                Step::Text(bytes) => {
                    // SAFETY: `stack` starts with the base style's slot and a
                    // pop never takes it, so it is never empty.
                    let Some(index) = *stack.last().unwrap() else {
                        continue;
                    };
                    match ranges.last_mut() {
                        Some((last, at))
                            if *at == index && last.end == bytes.start =>
                        {
                            last.end = bytes.end;
                        }
                        _ if bytes.is_empty() => {}
                        _ => ranges.push((bytes.clone(), index)),
                    }
                }
            }
        }
        ranges
    }

    /// The paragraph as its caller asked for it, its shadows SkParagraph's
    /// to draw: the control for what [`Passes`] draws in their place.
    #[cfg(test)]
    fn as_asked(&self, text: &str) -> Option<Paragraph> {
        Some(self.replay(text, |style, _, slot| {
            restore_shadows(style, &self.shadows[slot])
        }))
    }

    /// Moves each edge beside an oblique piece of text that falls inside a
    /// grapheme cluster to the cluster's end -- see [`whole_graphemes`] --
    /// and says whether any did.
    fn keep_graphemes_whole(&mut self, text: &str) -> bool {
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
        let mut moved = false;
        for piece in pieces {
            let step = Step::Text(piece.bytes);
            moved |= !matches!(
                (&self.steps[piece.step], &step),
                (Step::Text(was), Step::Text(is)) if was == is
            );
            self.steps[piece.step] = step;
        }
        debug_assert!(
            self.covers(text),
            "the pieces of text cover the builder's, byte for byte"
        );
        moved
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
    /// `restyle` makes of it, of whether it asks for oblique and of its
    /// slot, where that is `Some`.
    fn replay(
        &self,
        text: &str,
        restyle: impl Fn(&TextStyle, bool, usize) -> Option<TextStyle>,
    ) -> Paragraph {
        self.replay_builder(text, restyle).build()
    }

    /// The builder [`Self::replay`] builds its paragraph with.
    fn replay_builder(
        &self,
        text: &str,
        restyle: impl Fn(&TextStyle, bool, usize) -> Option<TextStyle>,
    ) -> ParagraphBuilder {
        let style = match restyle(self.style.text_style(), self.base_oblique, 0)
        {
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
                    match restyle(style, *oblique, at + 1) {
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
        builder
    }
}

/// One piece of a paragraph's text: the step that adds it, its bytes, and
/// whether its style asks for oblique.
struct Piece {
    step: usize,
    bytes: Range<usize>,
    oblique: bool,
}

/// Moves each edge between two adjacent pieces of `text`, either of them
/// oblique, that falls inside a grapheme cluster to the cluster's end, so
/// that the style of the cluster's base takes the whole of it.
///
/// Grapheme clusters rather than SkParagraph's glyph clusters, which cannot
/// tell the two cases at an oblique edge apart: a ligature across the edge
/// is one glyph cluster and should break, since an oblique is a font of its
/// own, and a base and its combining marks are one too and should not,
/// since a mark shaped apart from its base loses the glyph it attaches to;
/// the base decides which glyph that is, so its style is the cluster's.
///
/// Only where a side is oblique: elsewhere SkParagraph shapes the two pieces
/// as one and paints each its own way, as it always has. Between two
/// oblique pieces it shapes them as one too, but the sheared pass draws a
/// glyph whole, in one range's paint, so the cluster goes to its base's
/// piece there as well. And only between two pieces with text that meet: an
/// empty piece holds no base, and a placeholder stands in the text as
/// U+FFFC, which no piece holds, so the pieces either side of one never
/// meet, whatever empty pieces lie between, and its edges never move. A
/// mark right after one is a grapheme with it and has no base to go with,
/// so it stays in its own piece and is shaped alone, as SkParagraph shapes
/// it. Edges only move later and never past the next, so the pieces still
/// hold the same bytes.
fn whole_graphemes(text: &str, pieces: &mut [Piece]) {
    let starts: Vec<usize> =
        text.grapheme_indices(true).map(|(at, _)| at).collect();
    let cluster_end = |at: usize| match starts.binary_search(&at) {
        Ok(_) => at,
        Err(next) => starts.get(next).copied().unwrap_or(text.len()),
    };
    // The last piece with any text: the one holding the base of the
    // cluster at the next edge.
    let mut owner: Option<usize> = None;
    for at in 0..pieces.len() {
        if pieces[at].bytes.is_empty() {
            continue;
        }
        if let Some(left) = owner
            && pieces[left].bytes.end == pieces[at].bytes.start
            && (pieces[left].oblique || pieces[at].oblique)
        {
            let edge =
                cluster_end(pieces[at].bytes.start).min(pieces[at].bytes.end);
            pieces[left].bytes.end = edge;
            // Empty, and where the two met: no placeholder lies between.
            for between in &mut pieces[left + 1..at] {
                between.bytes = edge..edge;
            }
            pieces[at].bytes.start = edge;
        }
        if !pieces[at].bytes.is_empty() {
            owner = Some(at);
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

/// Takes `style`'s text shadows off it, for [`Passes`] to draw.
fn take_shadows(style: &mut TextStyle) -> Vec<TextShadow> {
    let shadows = style.shadows().to_vec();
    if !shadows.is_empty() {
        style.reset_shadows();
    }
    shadows
}

/// `style` with `shadows` given back, where it has any.
fn restore_shadows(
    style: &TextStyle,
    shadows: &[TextShadow],
) -> Option<TextStyle> {
    if shadows.is_empty() {
        return None;
    }
    let mut style = style.clone();
    for shadow in shadows {
        style.add_shadow(*shadow);
    }
    Some(style)
}

/// The text of one oblique range and what painting it takes.
struct ObliqueRange {
    /// UTF-8 offsets into the paragraph's text.
    bytes: Range<usize>,
    /// The paint SkParagraph draws the range's glyphs with: its foreground
    /// where it has one, and otherwise a default paint in its colour.
    foreground: Paint,
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
        Self { bytes, foreground }
    }
}

/// A style's decoration, taken off it so that SkParagraph draws none, and
/// the style's text shadows, which shadow the decoration as they do the
/// glyphs.
#[derive(Clone)]
struct StyleDecoration {
    decoration: Decoration,
    shadows: Vec<TextShadow>,
}

impl StyleDecoration {
    /// Takes `style`'s decoration off it, placed by the style's first
    /// available font in `collection` -- see [`FaceLines::first_available`].
    ///
    /// The colour is the style's decoration colour as it stands: both
    /// callers set it to the text's colour where none was asked for, since
    /// the text's goes in as a foreground paint that a style's own colour
    /// does not report.
    fn take(
        style: &mut TextStyle,
        collection: &mut FontCollection,
    ) -> Option<Self> {
        let lines = style.decoration_type();
        if lines == TextDecoration::NO_DECORATION {
            return None;
        }
        style.set_decoration_type(TextDecoration::NO_DECORATION);
        let families = style.font_families();
        let families: Vec<&str> = families.iter().collect();
        let face = FaceLines::first_available(
            collection,
            &families,
            style.font_style(),
            style.font_size(),
            style.font_arguments(),
        )?;
        Some(Self {
            decoration: Decoration {
                lines,
                style: style.decoration_style(),
                thickness: Thickness::Times(
                    style.decoration_thickness_multiplier(),
                ),
                color: style.decoration_color(),
                face,
            },
            shadows: style.shadows().to_vec(),
        })
    }
}

/// What a paragraph is painted with beyond `Paragraph::paint`: its glyph
/// runs that are sheared or cast a shadow and its decorations' spans, each
/// found again at every layout, and the paragraph its backgrounds come from
/// where something has to go between them and the glyphs.
pub(crate) struct Passes {
    ranges: Vec<ObliqueRange>,
    /// The backgrounds, where a shadow or a line beneath the glyphs goes
    /// between them and the glyphs.
    under: Option<Paragraph>,
    /// Each decorated style's decoration, by box, and its text shadows.
    boxes: Vec<Decoration>,
    box_shadows: Vec<Vec<TextShadow>>,
    /// The text each box decorates, in order -- see `decorate::box_at`.
    box_ranges: Vec<(Range<usize>, usize)>,
    /// Each style's text shadows, by slot -- see
    /// [`ParagraphRecipe::shadows`] -- and the text each slot with any
    /// covers.
    shadows: Vec<Vec<TextShadow>>,
    shadow_ranges: Vec<(Range<usize>, usize)>,
    runs: Vec<PaintedRun>,
    spans: Vec<Span>,
    /// The paragraph with its shadows SkParagraph's to draw -- see
    /// [`ParagraphRecipe::as_asked`].
    #[cfg(test)]
    pub(crate) as_asked: Option<Paragraph>,
}

/// The glyphs of one style in one font on one line that this draws: sheared
/// where they are oblique, with their style's shadows where it casts any.
struct PaintedRun {
    blob: TextBlob,
    origin: Point,
    /// The index in [`Passes::ranges`] of the oblique range the glyphs are
    /// in, whose foreground draws them here.
    range: Option<usize>,
    /// The slot whose shadows they cast.
    shadows: Option<usize>,
    /// The glyphs and their positions before any shear, for the tests to
    /// hold against SkParagraph's runs.
    #[cfg(test)]
    glyphs: Vec<skia_safe::GlyphId>,
    #[cfg(test)]
    positions: Vec<Point>,
}

impl Passes {
    /// Follows `paragraph`, just laid out at `width`: lays the pass beneath
    /// it out at the same width, and finds the decorations' spans and the
    /// glyphs drawn here again.
    ///
    /// The glyphs are read with `Paragraph::visit`, through `glyph_runs`,
    /// which reports the text blobs `Paragraph::paint` draws: each glyph
    /// where it is painted and the text it comes from, on every line, and
    /// nothing for a placeholder.
    ///
    /// A run's first glyph says whether it is oblique: an oblique style is a
    /// font of its own, so no run holds oblique text and upright text both.
    /// Each glyph then takes the range its text is in, or the one before
    /// it where its text is in none: the glyphs of an ellipsis or a hyphen
    /// report offsets counted from the text they stand after, which can run
    /// past it. A glyph casts the shadows of the style its text is in, which
    /// for a cluster of several characters is its first's.
    pub(crate) fn lay_out(&mut self, paragraph: &mut Paragraph, width: f32) {
        if let Some(under) = &mut self.under {
            under.layout(width);
        }
        #[cfg(test)]
        if let Some(as_asked) = &mut self.as_asked {
            as_asked.layout(width);
        }
        let runs = glyph_runs(paragraph);
        self.spans = match self.boxes.is_empty() {
            true => Vec::new(),
            false => decorate::spans(&runs, |at| {
                decorate::box_at(&self.box_ranges, at)
            }),
        };
        self.runs = match self.ranges.is_empty()
            && self.shadow_ranges.is_empty()
        {
            true => Vec::new(),
            false => runs.iter().flat_map(|run| self.painted(run)).collect(),
        };
    }

    /// The pieces of `run` drawn here.
    fn painted(&self, run: &GlyphRun) -> Vec<PaintedRun> {
        let oblique = |start: &u32| self.range_of(*start as usize);
        let ranges: Vec<Option<usize>> =
            match run.starts.first().and_then(oblique) {
                Some(first) => run
                    .starts
                    .iter()
                    .scan(first, |last, start| {
                        *last = oblique(start).unwrap_or(*last);
                        Some(Some(*last))
                    })
                    .collect(),
                None => vec![None; run.starts.len()],
            };
        let keys: Vec<(Option<usize>, Option<usize>)> = zip(
            ranges,
            run.starts.iter().map(|start| {
                decorate::box_at(&self.shadow_ranges, *start as usize)
            }),
        )
        .collect();
        let shear = |range: Option<usize>| match range {
            Some(_) => {
                run_shear(&run.font.typeface(), Some(DEFAULT_OBLIQUE_DEGREES))
            }
            None => Shear::None,
        };
        keys.chunk_by(|a, b| a == b)
            .scan(0, |start, chunk| {
                let at = *start..*start + chunk.len();
                *start = at.end;
                Some((at, chunk[0]))
            })
            .filter(|(_, (range, shadows))| {
                range.is_some() || shadows.is_some()
            })
            .filter_map(|(at, (range, shadows))| {
                let blob = sheared_blob(
                    &run.font,
                    &run.glyphs[at.clone()],
                    &run.positions[at.clone()],
                    shear(range),
                )?;
                Some(PaintedRun {
                    blob,
                    origin: run.origin,
                    range,
                    shadows,
                    #[cfg(test)]
                    glyphs: run.glyphs[at.clone()].to_vec(),
                    #[cfg(test)]
                    positions: run.positions[at].to_vec(),
                })
            })
            .collect()
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

    /// Paints `paragraph`, which this was laid out with, at `origin`, in
    /// CSS's order -- section 5.1 of CSS Text Decoration 4, after CSS 2
    /// Appendix E: the backgrounds, the shadows, the glyphs' and then the
    /// decorations', the underlines and overlines, the glyphs, upright and
    /// then sheared, and the lines-through over them all.
    ///
    /// The shadows are drawn here rather than by `Paragraph::paint` so that
    /// they go beneath the lines, with the paint
    /// `CanvasParagraphPainter::drawTextShadow` uses and from the same
    /// glyphs. Every line's shadows go before any line's glyphs, where
    /// SkParagraph draws one line's shadows and glyphs before the next
    /// line's: a shadow reaching the line above lies beneath its glyphs here.
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
            let Some(slot) = run.shadows else { continue };
            for shadow in &self.shadows[slot] {
                if let Some(paint) = shadow_paint(shadow) {
                    canvas.draw_text_blob(
                        &run.blob,
                        origin + run.origin + shadow.offset,
                        &paint,
                    );
                }
            }
        }
        for (index, shadows) in self.box_shadows.iter().enumerate() {
            for shadow in shadows {
                if let Some(paint) = shadow_paint(shadow) {
                    decorate::paint_shadow(
                        canvas,
                        &self.spans,
                        &self.boxes,
                        index,
                        origin + shadow.offset,
                        &paint,
                    );
                }
            }
        }
        let base = Paint::default();
        decorate::paint(
            canvas,
            &self.spans,
            &self.boxes,
            origin,
            Layer::Beneath,
            &base,
        );
        paragraph.paint(canvas, origin);
        for run in &self.runs {
            if let Some(range) = run.range {
                canvas.draw_text_blob(
                    &run.blob,
                    origin + run.origin,
                    &self.ranges[range].foreground,
                );
            }
        }
        decorate::paint(
            canvas,
            &self.spans,
            &self.boxes,
            origin,
            Layer::Over,
            &base,
        );
    }
}

/// The paint SkParagraph draws a text shadow with, `CanvasParagraphPainter::
/// drawTextShadow`'s: the shadow's colour, blurred by its sigma where it has
/// one. `None` for a shadow that casts nothing.
fn shadow_paint(shadow: &TextShadow) -> Option<Paint> {
    if !shadow.has_shadow() {
        return None;
    }
    let mut paint = Paint::default();
    paint.set_color(shadow.color);
    if shadow.blur_sigma != 0.0 {
        paint.set_mask_filter(MaskFilter::blur(
            BlurStyle::Normal,
            shadow.blur_sigma as f32,
            false,
        ));
    }
    Some(paint)
}

#[cfg(test)]
mod tests {
    use super::{
        ParagraphRecipe, Piece, add_slnt, match_oblique, set_coordinates,
        whole_graphemes,
    };
    use skia_safe::{
        Color, Data, FontMgr, FontStyle, GlyphId, ImageInfo, Point,
        font_style::{Slant, Weight, Width},
        surfaces,
        textlayout::{
            FontCollection, ParagraphBuilder, ParagraphStyle,
            PlaceholderAlignment, PlaceholderStyle, TextBaseline, TextStyle,
            TypefaceFontProvider,
        },
    };
    use std::ops::Range;
    use unicode_segmentation::UnicodeSegmentation;

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
        // Two oblique pieces, which the sheared pass draws a glyph of whole.
        assert_eq!(
            moved(&text, &[(0..4, true), (4..n, true)]),
            [format!("cafe{ACUTE}"), " x".to_owned()]
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

    /// SplitMix64, Steele, Lea and Flood's generator ("Fast splittable
    /// pseudorandom number generators", OOPSLA 2014), with the constants
    /// that paper and Java's `SplittableRandom` publish. Small and exact, so
    /// a generated case reads the same on every platform.
    struct SplitMix(u64);

    impl SplitMix {
        /// The increment, the odd integer nearest 2^64 over the golden ratio.
        const GAMMA: u64 = 0x9E37_79B9_7F4A_7C15;
        /// The two multipliers of the output mix.
        const MIX: [u64; 2] = [0xBF58_476D_1CE4_E5B9, 0x94D0_49BB_1331_11EB];

        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(Self::GAMMA);
            let z = (self.0 ^ (self.0 >> 30)).wrapping_mul(Self::MIX[0]);
            let z = (z ^ (z >> 27)).wrapping_mul(Self::MIX[1]);
            z ^ (z >> 31)
        }

        /// A number in `0..n`.
        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
    }

    /// One step of a generated paragraph. The styles are numbered in the
    /// order they are pushed, from 1; the paragraph's own is 0.
    #[derive(Clone, Copy, Debug)]
    enum Op {
        Push { oblique: bool },
        Pop,
        Text(&'static str),
        Placeholder,
    }

    /// What a generated paragraph's text is made of: bases and marks in
    /// every arrangement an edge can split, empty text, a space to wrap
    /// at, and a literal U+FFFC that is text rather than a placeholder.
    /// No "f", whose ligatures form across an edge upright and not across
    /// an oblique one.
    const PIECES: &[&str] = &[
        "ab",
        "cd",
        "e",
        "e\u{301}",
        "\u{301}",
        "\u{308}",
        "\u{301}x",
        "\u{301}\u{308}",
        "",
        "",
        " ",
        "mn op",
        "H",
        "\u{FFFC}",
    ];

    /// How many paragraphs
    /// [`generated_oblique_paragraphs_keep_their_invariants`] builds, and
    /// the seed they come from: fixed, so a failure names a case that can
    /// be run again.
    const CASES: usize = 400;
    const SEED: u64 = 288;

    /// The generated paragraphs' size, line length and margin. The margin
    /// holds what a 14deg shear leans past a line, and marks above it.
    const SIZE: f32 = 40.0;
    const WIDTH: f32 = 260.0;
    const MARGIN: f32 = 40.0;

    /// A paragraph's steps, and whether its own style asks for oblique.
    fn generate(random: &mut SplitMix) -> (Vec<Op>, bool) {
        let base_oblique = random.below(5) == 0;
        let ops = (0..1 + random.below(9))
            .map(|_| match random.below(20) {
                0..=8 => Op::Text(PIECES[random.below(PIECES.len())]),
                9..=12 => Op::Push {
                    oblique: random.below(2) == 0,
                },
                13..=15 => Op::Pop,
                _ => Op::Placeholder,
            })
            .collect();
        (ops, base_oblique)
    }

    /// The bundled Raleway, upright and italic, as one family. Its italic
    /// does not lean as an oblique asks, so an oblique is its upright face
    /// sheared.
    const RALEWAY: &str = "RalewayGenerated";

    fn raleway() -> FontCollection {
        let mut provider = TypefaceFontProvider::new();
        for file in [
            "Raleway-VariableFont_wght.ttf",
            "Raleway-Italic-VariableFont_wght.ttf",
        ] {
            let bytes = std::fs::read(format!(
                "{}/tests/assets/fonts/Raleway/{file}",
                env!("CARGO_MANIFEST_DIR")
            ))
            .expect("bundled Raleway reads");
            let face = FontMgr::new()
                .new_from_data(Data::new_copy(&bytes), None)
                .expect("bundled Raleway loads");
            provider.register_typeface(face, Some(RALEWAY));
        }
        let mut collection = FontCollection::new();
        collection.set_asset_font_manager(Some(provider.into()));
        collection
    }

    /// Style `index`, black where it is `visible` and transparent where
    /// not, asking for oblique where `oblique` says, as the binding builds
    /// one: matched by [`match_oblique`], with `slnt` among its arguments.
    fn generated_style(
        collection: &mut FontCollection,
        oblique: bool,
        visible: bool,
    ) -> (TextStyle, bool) {
        let mut style = TextStyle::new();
        style
            .set_font_families(&[RALEWAY])
            .set_font_size(SIZE)
            .set_color(match visible {
                true => Color::BLACK,
                false => Color::TRANSPARENT,
            });
        if oblique {
            style.set_font_style(FontStyle::new(
                Weight::NORMAL,
                Width::NORMAL,
                Slant::Oblique,
            ));
        }
        let oblique = match_oblique(&mut style, collection);
        if oblique {
            let mut coordinates = Vec::new();
            add_slnt(&mut coordinates);
            set_coordinates(&mut style, &coordinates);
        }
        (style, oblique)
    }

    /// Where a generated paragraph is drawn, and what with.
    #[derive(Clone, Copy, PartialEq)]
    enum Painter {
        /// Built by [`ParagraphRecipe`] and drawn with its sheared runs.
        Recipe,
        /// Every style upright, its text where the recipe puts it once its
        /// edges have moved, drawn by `Paragraph::paint` alone: the control
        /// for what the sheared passes paint.
        Assigned,
        /// Every style upright, its text where it was added, built by a
        /// plain `ParagraphBuilder`.
        Plain,
    }

    /// A placeholder's run, or a sheared one: where it was drawn, and
    /// what.
    type Run = (Point, Vec<Point>, Vec<GlyphId>);

    /// A generated paragraph as drawn.
    struct Drawn {
        pixels: Vec<u8>,
        /// The runs SkParagraph reports for the placeholders, and the runs
        /// the sheared pass draws.
        placeholders: Vec<Run>,
        sheared: Vec<Run>,
    }

    /// Draws a generated paragraph through `painter`, with only style
    /// `visible` inked, or every style where it is `None`.
    fn draw(
        collection: &mut FontCollection,
        (ops, base_oblique): &(Vec<Op>, bool),
        painter: Painter,
        visible: Option<usize>,
    ) -> Drawn {
        let shown = |index: usize| visible.is_none_or(|v| v == index);
        // The recipe's style, and whether it asks for oblique, which
        // decides where its edges move -- of the oblique style for the
        // recipe and of the upright one for the control.
        let style = |collection: &mut FontCollection, oblique, index| {
            let (asked, flag) =
                generated_style(collection, oblique, shown(index));
            match painter {
                Painter::Recipe => (asked, flag),
                Painter::Assigned | Painter::Plain => {
                    (generated_style(collection, false, shown(index)).0, flag)
                }
            }
        };
        let (base, base_flag) = style(collection, *base_oblique, 0);
        let mut paragraph_style = ParagraphStyle::new();
        paragraph_style.set_text_style(&base);
        let mut recipe = ParagraphRecipe::new(
            paragraph_style.clone(),
            base_flag,
            collection.clone(),
        );
        let mut builder =
            ParagraphBuilder::new(&paragraph_style, collection.clone());
        let placeholder = PlaceholderStyle::new(
            SIZE * 0.75,
            SIZE * 0.75,
            PlaceholderAlignment::Baseline,
            TextBaseline::Alphabetic,
            0.0,
        );
        let mut placeholders = Vec::new();
        let mut pushed = 0;
        for op in ops {
            match *op {
                Op::Push { oblique } => {
                    pushed += 1;
                    let (style, flag) = style(collection, oblique, pushed);
                    recipe.push_style(style.clone(), flag);
                    builder.push_style(&style);
                }
                Op::Pop => {
                    recipe.pop();
                    builder.pop();
                }
                Op::Text(text) => {
                    recipe.add_text(text);
                    builder.add_text(text);
                }
                Op::Placeholder => {
                    placeholders.push(builder.get_text().len());
                    recipe.add_placeholder(&placeholder);
                    builder.add_placeholder(&placeholder);
                }
            }
        }
        let (mut paragraph, mut sheared) = match painter {
            Painter::Recipe => recipe.build(),
            Painter::Assigned => {
                let text = recipe.builder.get_text().to_owned();
                recipe.keep_graphemes_whole(&text);
                (recipe.replay(&text, |_, _, _| None), None)
            }
            Painter::Plain => (builder.build(), None),
        };
        paragraph.layout(WIDTH);
        if let Some(sheared) = sheared.as_mut() {
            sheared.lay_out(&mut paragraph, WIDTH);
        }

        let mut found = Vec::new();
        paragraph.extended_visit(|_, visit| {
            if let Some(info) = visit
                && info.utf8_starts().first().is_some_and(|start| {
                    placeholders.contains(&(*start as usize))
                })
            {
                found.push((
                    info.origin(),
                    info.positions().to_vec(),
                    info.glyphs().to_vec(),
                ));
            }
        });

        let size = (
            (WIDTH + 2.0 * MARGIN) as i32,
            (paragraph.height() + 2.0 * MARGIN).ceil() as i32,
        );
        let mut surface =
            surfaces::raster_n32_premul(size).expect("a raster surface");
        let origin = Point::new(MARGIN, MARGIN);
        match &sheared {
            Some(sheared) => {
                sheared.paint(&paragraph, surface.canvas(), origin);
            }
            None => paragraph.paint(surface.canvas(), origin),
        }
        let info = ImageInfo::new_n32_premul(size, None);
        let mut pixels = vec![0; info.compute_min_byte_size()];
        assert!(surface.read_pixels(
            &info,
            &mut pixels,
            info.min_row_bytes(),
            (0, 0)
        ));
        Drawn {
            pixels,
            placeholders: found,
            sheared: sheared
                .iter()
                .flat_map(|sheared| &sheared.runs)
                .filter(|run| run.range.is_some())
                .map(|run| {
                    (run.origin, run.positions.clone(), run.glyphs.clone())
                })
                .collect(),
        }
    }

    /// The sum of every pixel's alpha.
    fn ink(pixels: &[u8]) -> u64 {
        pixels
            .as_chunks::<4>()
            .0
            .iter()
            .map(|pixel| u64::from(pixel[3]))
            .sum()
    }

    /// A generated paragraph's text, a placeholder standing as U+FFFC, and
    /// its pieces, each oblique where its style asks -- which for Raleway is
    /// wherever it asks for oblique, since `match_oblique` says yes to every
    /// oblique request.
    fn pieces((ops, base_oblique): &(Vec<Op>, bool)) -> (String, Vec<Piece>) {
        let (mut text, mut pieces, mut stack) =
            (String::new(), Vec::new(), vec![*base_oblique]);
        for (step, op) in ops.iter().enumerate() {
            match *op {
                Op::Push { oblique } => stack.push(oblique),
                Op::Pop if stack.len() > 1 => {
                    stack.pop();
                }
                Op::Pop => {}
                Op::Text(added) => {
                    pieces.push(Piece {
                        step,
                        bytes: text.len()..text.len() + added.len(),
                        oblique: stack[stack.len() - 1],
                    });
                    text.push_str(added);
                }
                Op::Placeholder => text.push('\u{FFFC}'),
            }
        }
        (text, pieces)
    }

    /// What is wrong with where [`whole_graphemes`] put `text`'s bytes,
    /// `before` to `after`, or `None`: every byte held once; one that moved
    /// moved back, across an edge with an oblique side inside its grapheme,
    /// into the piece that now holds the byte before that edge; and no such
    /// edge between two pieces with text that meet falls inside a grapheme.
    fn misplaced(
        text: &str,
        before: &[Piece],
        after: &[Piece],
    ) -> Option<String> {
        let owner = |pieces: &[Piece], byte: usize| {
            let mut owners = pieces
                .iter()
                .enumerate()
                .filter(|(_, piece)| piece.bytes.contains(&byte));
            let found = owners.next().map(|(at, _)| at);
            match owners.next() {
                Some(_) => Err(format!("byte {byte} is held twice")),
                None => Ok(found),
            }
        };
        let starts: Vec<usize> =
            text.grapheme_indices(true).map(|(at, _)| at).collect();
        let grapheme = |byte: usize| match starts.binary_search(&byte) {
            Ok(at) => starts[at],
            Err(at) => starts[at - 1],
        };
        for byte in 0..text.len() {
            let (was, is) = match (owner(before, byte), owner(after, byte)) {
                (Ok(was), Ok(is)) => (was, is),
                (Err(why), _) | (_, Err(why)) => return Some(why),
            };
            match (was, is) {
                (was, is) if was == is => {}
                (Some(was), Some(is)) => {
                    let edge = before[was].bytes.start;
                    let left = edge
                        .checked_sub(1)
                        .and_then(|left| owner(after, left).ok().flatten());
                    if !(before[was].oblique || after[is].oblique)
                        || grapheme(byte) >= edge
                        || left != Some(is)
                    {
                        return Some(format!(
                            "byte {byte} moved from piece {was} to {is}, \
                             across an edge at {edge} held on its left by \
                             {left:?}"
                        ));
                    }
                }
                (was, is) => {
                    return Some(format!(
                        "byte {byte} held by {was:?}, then {is:?}"
                    ));
                }
            }
        }
        let with_text: Vec<&Piece> = after
            .iter()
            .filter(|piece| !piece.bytes.is_empty())
            .collect();
        with_text.windows(2).find_map(|pair| {
            let edge = pair[1].bytes.start;
            (pair[0].bytes.end == edge
                && (pair[0].oblique || pair[1].oblique)
                && starts.binary_search(&edge).is_err())
            .then(|| {
                format!("an oblique edge at byte {edge} splits a grapheme")
            })
        })
    }

    /// What is wrong with where a generated paragraph's bytes end up, or
    /// `None`: each byte stays in its piece or goes to its grapheme's base
    /// across an oblique edge -- see [`misplaced`] -- and the text the
    /// recipe replays is the builder's, byte for byte.
    fn misplaced_bytes(
        collection: &mut FontCollection,
        generated: &(Vec<Op>, bool),
    ) -> Option<String> {
        let (text, before) = pieces(generated);
        let (_, mut after) = pieces(generated);
        whole_graphemes(&text, &mut after);
        if let Some(why) = misplaced(&text, &before, &after) {
            return Some(why);
        }

        let mut recipe = ParagraphRecipe::new(
            ParagraphStyle::new(),
            generated.1,
            collection.clone(),
        );
        for op in &generated.0 {
            match *op {
                Op::Push { oblique } => {
                    let (style, oblique) =
                        generated_style(collection, oblique, true);
                    recipe.push_style(style, oblique);
                }
                Op::Pop => recipe.pop(),
                Op::Text(text) => recipe.add_text(text),
                Op::Placeholder => {
                    recipe.add_placeholder(&PlaceholderStyle::default())
                }
            }
        }
        let text = recipe.builder.get_text().to_owned();
        recipe.keep_graphemes_whole(&text);
        let replayed = recipe
            .replay_builder(&text, |_, _, _| None)
            .get_text()
            .to_owned();
        (replayed != text).then(|| format!("replays {replayed:?} for {text:?}"))
    }

    /// The steps [`every_arrangement_of_three_steps_keeps_its_bytes`] puts
    /// side by side: a placeholder, or a style pushed around a piece of
    /// text -- empty, a base, marks with no base, marks before a base --
    /// upright or oblique, and popped.
    fn arrangement_steps() -> Vec<Vec<Op>> {
        const TEXTS: [&str; 5] = ["", "e", "\u{301}", "\u{301}x", "\u{308}"];
        std::iter::once(vec![Op::Placeholder])
            .chain(TEXTS.iter().flat_map(|&text| {
                [false, true].map(|oblique| {
                    vec![Op::Push { oblique }, Op::Text(text), Op::Pop]
                })
            }))
            .collect()
    }

    /// Every edge three steps can make, in a paragraph upright or oblique.
    /// Some are rare at random -- an empty piece after a placeholder, then
    /// marks in the other slant, comes up in under one case in five hundred
    /// -- and every one is tried here. Only the exact invariants, which
    /// need no drawing.
    #[test]
    fn every_arrangement_of_three_steps_keeps_its_bytes() {
        let mut collection = raleway();
        let steps = arrangement_steps();
        let mut failures = Vec::new();
        for a in &steps {
            for b in &steps {
                for c in &steps {
                    for base_oblique in [false, true] {
                        let generated = (
                            [a.clone(), b.clone(), c.clone()].concat(),
                            base_oblique,
                        );
                        if let Some(why) =
                            misplaced_bytes(&mut collection, &generated)
                        {
                            failures.push(format!("{generated:?}: {why}"));
                        }
                    }
                }
            }
        }
        assert!(
            failures.is_empty(),
            "{} of {} arrangements failed:\n{}",
            failures.len(),
            2 * steps.len().pow(3),
            failures.join("\n")
        );
    }

    #[test]
    fn generated_oblique_paragraphs_keep_their_invariants() {
        let mut collection = raleway();
        let mut random = SplitMix(SEED);
        let mut failures = Vec::new();
        for case in 0..CASES {
            let generated = generate(&mut random);
            let (ops, _) = &generated;
            let mut fail = |what: String| {
                failures.push(format!("case {case} {generated:?}: {what}"))
            };

            if let Some(why) = misplaced_bytes(&mut collection, &generated) {
                fail(why);
            }

            // No placeholder's run is drawn by the sheared pass.
            let drawn =
                draw(&mut collection, &generated, Painter::Recipe, None);
            if let Some(run) = drawn
                .sheared
                .iter()
                .find(|run| drawn.placeholders.contains(run))
            {
                fail(format!("shears a placeholder's run, {run:?}"));
            }

            // With every style upright, the recipe draws what SkParagraph
            // draws.
            let upright: (Vec<Op>, bool) = (
                ops.iter()
                    .map(|op| match op {
                        Op::Push { .. } => Op::Push { oblique: false },
                        op => *op,
                    })
                    .collect(),
                false,
            );
            if draw(&mut collection, &upright, Painter::Recipe, None).pixels
                != draw(&mut collection, &upright, Painter::Plain, None).pixels
            {
                fail("draws an all-upright paragraph differently".into());
            }

            // Each style inks what it inks upright, its text where the
            // recipe put it: a shear keeps a glyph's area, so a glyph drawn
            // once inks the same, give or take its antialiased edge. A style
            // with nothing to draw draws nothing, exactly.
            let styles = 1 + ops
                .iter()
                .filter(|op| matches!(op, Op::Push { .. }))
                .count();
            for style in 0..styles {
                let [sheared, upright] = [Painter::Recipe, Painter::Assigned]
                    .map(|painter| {
                        ink(&draw(
                            &mut collection,
                            &generated,
                            painter,
                            Some(style),
                        )
                        .pixels)
                    });
                let gap = sheared.abs_diff(upright);
                if gap > (upright as f64 * INK_SPREAD) as u64 {
                    fail(format!(
                        "style {style} inks {sheared}, upright {upright}"
                    ));
                }
            }
        }
        assert!(
            failures.is_empty(),
            "{} of {CASES} cases failed:\n{}",
            failures.len(),
            failures.join("\n")
        );
    }

    /// How far a style's ink may stray from its upright control, as a
    /// share of it.
    ///
    /// A sheared glyph rasterises with 5 to 7 per cent more ink than the
    /// same glyph upright, on every one of these 400 cases that is right:
    /// the shear keeps a glyph's area, and the difference is the
    /// rasteriser's, systematic rather than an edge effect, since 80px
    /// shows the same 5 to 6. Every defect this has found was 27 per cent
    /// or more. A control of 0 allows nothing, so a style that should draw
    /// nothing is held to exactly that.
    const INK_SPREAD: f64 = 0.10;
}
