//! Paragraphs whose `oblique` ranges are drawn as CSS Fonts 4 matches them,
//! and whose decorations, shadows and backgrounds are drawn as CSS paints
//! them.
//!
//! A range asking for oblique is laid out in the family's face that leans as
//! asked where it has one, and otherwise upright and sheared by
//! `tan(14deg)`, the default angle -- the rule a canvas `oblique` follows,
//! `run_shear` in `node::typography`. It carries `slnt` among its font
//! arguments either way, so that SkParagraph shapes it as the font of its
//! own it is and no ligature forms across its edge -- see `add_slnt`.
//!
//! Decorations, text shadows and backgrounds are taken off every style as
//! it is pushed, so SkParagraph lays the paragraph out and draws none of
//! them. A paragraph that had any, or an oblique range, is drawn by
//! [`Passes`], from the glyph runs `Paragraph::visit` reports, a line at a
//! time in CSS 2.1 Appendix E's order; the lines are placed by
//! `node::decorate` from each box's first available font -- see the
//! `UPSTREAM:` note there for why not SkParagraph's -- and a background
//! covers its font's content area.
//!
//! An edge between two styles that falls inside a grapheme cluster moves to
//! the cluster's end where a side is oblique or the two are shaped as one
//! font -- see [`whole_graphemes`] -- and the paragraph is built again from
//! its steps. A colour change left inside a ligature has [`Passes`] draw
//! the paragraph too, the ligature whole. Any other paragraph is built once,
//! as it is filled in, and painted by `Paragraph::paint` alone.

use crate::{
    context::faint::Faint,
    font::leans_as_asked,
    font_library::text_hinting,
    node::{
        decorate::{
            self, Decoration, FaceLines, Layer, Span, Spans, Thickness,
        },
        font_library::FontEpoch,
        typography::{
            GlyphRun, GlyphRunRef, SLNT, Shear, glyph_runs, run_shear,
            sheared_blob, visit_glyph_runs,
        },
    },
};
use skia_safe::{
    BlurStyle, Canvas, Color4f, FontArguments, FontStyle, MaskFilter, Paint,
    Point, Rect, TextBlob,
    font_arguments::{VariationPosition, variation_position::Coordinate},
    font_style::Slant,
    textlayout::{
        FontCollection, Paragraph, ParagraphBuilder, ParagraphStyle,
        PlaceholderStyle, StyleType, TextDecoration, TextShadow, TextStyle,
    },
};
use std::{borrow::Cow, cell::RefCell, iter::once, mem, ops::Range, rc::Rc};
use unicode_segmentation::GraphemeCursor;

/// What a paragraph's lines are drawn to do with a paint too faint for eight
/// bits: left to Skia's skip, because a paragraph's glyph paints are fixed
/// when it is built, before the canvas it is drawn on is known, and a line
/// redrawn where its glyphs are not would draw half the paragraph.
const PARAGRAPH_FAINT: Faint = Faint::Dropped;

/// The character SkParagraph adds to a paragraph's text for each
/// placeholder, U+FFFC OBJECT REPLACEMENT CHARACTER, whose UTF-8 length
/// moves every later text offset.
const PLACEHOLDER_CHARACTER: char = '\u{FFFC}';

/// The most entries a list a [`ParagraphRecipe`] hands back may hold room
/// for and still be kept for the next recipe on the thread.
///
/// A kept list keeps its capacity for as long as the thread lives, so one
/// paragraph of thousands of styles would otherwise pin room for thousands
/// on every thread that ever built it. A list past this is dropped and the
/// next recipe allocates its own. Every paragraph of up to this many
/// styles, steps or both reuses what the last left, which is the paragraph
/// a caller builds again and again.
const KEPT_LIST_CAPACITY: usize = 64;

thread_local! {
    /// The lists the last [`ParagraphRecipe`] on this thread handed back,
    /// emptied, for the next one to fill -- see [`KEPT_LIST_CAPACITY`].
    static SPARE_LISTS: RefCell<(Vec<Step>, Vec<Pushed>)> =
        const { RefCell::new((Vec::new(), Vec::new())) };
}

/// The slant a style asking for `font_style`, an oblique of `degrees`, is
/// laid out in, or `None` where it does not ask for oblique.
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
    degrees: f32,
) -> Option<Slant> {
    if font_style.slant() != Slant::Oblique {
        return None;
    }
    let leans = collection
        .find_typefaces(families, font_style)
        .first()
        .is_some_and(|face| leans_as_asked(face, degrees));
    Some(match leans {
        true => Slant::Oblique,
        false => Slant::Upright,
    })
}

/// Sets `style`, which asks for an oblique of `degrees` if any, to the slant
/// [`oblique_layout_slant`] lays it out in, and gives the angle back where
/// it does ask for oblique.
pub(crate) fn match_oblique(
    style: &mut TextStyle,
    collection: &mut FontCollection,
    degrees: f32,
) -> Option<f32> {
    let font_style = style.font_style();
    if font_style.slant() != Slant::Oblique {
        return None;
    }
    let families = style.font_families();
    let families: Vec<&str> = families.iter().collect();
    let slant =
        oblique_layout_slant(collection, &families, font_style, degrees)?;
    style.set_font_style(FontStyle::new(
        font_style.weight(),
        font_style.width(),
        slant,
    ));
    Some(degrees)
}

/// Puts `slnt` among `coordinates`, at an oblique of `degrees`, unless they
/// set it already: the font arguments of a style that asks for oblique.
///
/// An oblique is a font of its own, and this is how SkParagraph is told so.
/// It shapes adjacent styles as one run wherever their font style,
/// families, size and font arguments agree, whatever else differs, so an
/// oblique range whose face is the upright one formed a ligature with the
/// text beside it -- "fi" across the edge drawn half sheared, half not. With
/// the coordinate, shaping breaks at the edge, as it does at any change of
/// font, and between two obliques of different angles. A face without the
/// axis ignores it. OpenType counts `slnt` counter-clockwise, so the
/// coordinate is the angle negated.
pub(crate) fn add_slnt(coordinates: &mut Vec<Coordinate>, degrees: f32) {
    if !coordinates.iter().any(|coordinate| coordinate.axis == SLNT) {
        coordinates.push(Coordinate {
            axis: SLNT,
            value: -degrees,
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
/// that one whose edges move can be built again from the same steps -- see
/// the module documentation.
///
/// The steps hold no text of their own, only where it lies in the
/// builder's. A paragraph in one style is built as it was filled in; one in
/// several reads its text back, once, only where the bytes either side of
/// an edge, noted as they are added, leave a grapheme cluster across it
/// possible.
pub(crate) struct ParagraphRecipe {
    builder: ParagraphBuilder,
    style: ParagraphStyle,
    collection: FontCollection,
    /// The fonts `collection` was built over, where it was built over the
    /// shared library's, so that a decoration's or background's face is
    /// found once a epoch -- see [`FaceLines::first_available`].
    epoch: Option<Rc<FontEpoch>>,
    /// How the paragraph was filled in, in order.
    steps: Vec<Step>,
    /// What was taken off the paragraph style's own text style, its slot 0.
    base_taken: Taken,
    /// Each style pushed, in slot 1 and on, at its index plus 1: one list
    /// rather than one for each thing kept of it. Taken, with `steps`, from
    /// what the last recipe on the thread handed back, since every
    /// paragraph is built through here -- see [`SPARE_LISTS`].
    pushed: Vec<Pushed>,
    /// The length of the builder's text, in UTF-8 bytes.
    length: usize,
    /// The angle of the oblique the paragraph style's own text style asks
    /// for, if any.
    base_oblique: Option<f32>,
    /// Whether any style does, the paragraph's own included.
    oblique: bool,
    /// Whether any style splits its leading half above and half below,
    /// the paragraph's own included -- see [`without_paragraph_cache`].
    half_leading: bool,
    /// The slot of the style text is added in now: 0 for the paragraph's
    /// own, and a pushed style's index plus 1.
    top: usize,
    /// Whether the caller vouches that every style paints its glyphs as
    /// the paragraph's own style does -- see [`Self::new`].
    one_paint: bool,
    /// Whether [`ascii_boundary`] holds at every text step's start, judged
    /// as the bytes either side of it are added rather than by reading the
    /// builder's text back -- see [`Self::meet`].
    ascii_edges: bool,
    /// The last byte added, a placeholder's included: the one before the
    /// next step's start.
    last_byte: Option<u8>,
    /// Whether a text step added empty starts where the builder's text
    /// ends, so that the byte after its start is the next one added.
    edge_waiting: bool,
}

impl ParagraphRecipe {
    /// A paragraph in `style`, whose text style asks for the oblique
    /// `base_oblique` gives the angle of, if any -- [`match_oblique`]'s
    /// answer for it -- laid out in `collection`, built over the fonts of
    /// `epoch` where it came from the shared library.
    ///
    /// A paragraph's glyphs are drawn hinted, every style's, as canvas text
    /// is by default -- see `font_library::text_hinting`. Both callers hand
    /// in a paragraph style whose own text style is hinted already, as
    /// Skia's `TextStyle` is from the start, and one with no decoration,
    /// shadow or background either is built from as it stands. Any other is
    /// copied, hinted and has those taken off it here, so a caller that
    /// hands in an unhinted style still draws hinted. Each pushed style is
    /// hinted in [`Self::push_style`].
    ///
    /// `one_paint` vouches that every style the paragraph is filled in
    /// paints its glyphs as its own style does, until [`Self::paints_differ`]
    /// says otherwise: for a caller that knows what each glyph paint is
    /// built from, as the crate's `ParagraphBuilder` does. A paragraph in one
    /// paint whose edges all lie between grapheme clusters has no edge for a
    /// ligature to straddle in two paints, and none that moves, so
    /// [`Self::build`] walks none of them.
    pub(crate) fn new(
        mut style: ParagraphStyle,
        base_oblique: Option<f32>,
        mut collection: FontCollection,
        epoch: Option<Rc<FontEpoch>>,
        colours: &StyleColours,
        one_paint: bool,
    ) -> Self {
        let half_leading = style.text_style().half_leading();
        let (decoration, shadows, background) =
            match as_given(style.text_style()) {
                true => (None, Vec::new(), None),
                false => {
                    #[cfg(any(
                        test,
                        all(feature = "node-addon", debug_assertions)
                    ))]
                    STYLE_COPIES.with(|copies| copies.set(copies.get() + 1));
                    let mut base = style.text_style().clone();
                    base.set_font_hinting(text_hinting(true));
                    let decoration = StyleDecoration::take(
                        &mut base,
                        &mut collection,
                        epoch.as_ref(),
                        colours,
                    )
                    .map(Box::new);
                    let shadows = take_shadows(&mut base, colours);
                    let background = StyleBackground::take(
                        &mut base,
                        &mut collection,
                        epoch.as_ref(),
                    )
                    .map(Box::new);
                    style.set_text_style(&base);
                    (decoration, shadows, background)
                }
            };
        // The lists the last recipe on this thread left, or new ones where
        // another recipe holds them or none has been made.
        let (mut steps, pushed) = SPARE_LISTS
            .try_with(|spare| {
                spare
                    .try_borrow_mut()
                    .map(|mut spare| mem::take(&mut *spare))
                    .unwrap_or_default()
            })
            .unwrap_or_default();
        if steps.capacity() == 0 {
            steps.reserve(8);
        }
        Self {
            builder: ParagraphBuilder::new(&style, &collection),
            style,
            collection,
            epoch,
            steps,
            base_taken: Taken {
                decoration,
                shadows,
                background,
            },
            pushed,
            length: 0,
            base_oblique,
            oblique: base_oblique.is_some(),
            half_leading,
            top: 0,
            one_paint,
            ascii_edges: true,
            last_byte: None,
            edge_waiting: false,
        }
    }

    /// Pushes `style`, which asks for an oblique of the angle `oblique`
    /// gives, if any, hinted -- see [`Self::new`] -- and whose decoration
    /// and shadows are in `colours`.
    pub(crate) fn push_style(
        &mut self,
        mut style: TextStyle,
        oblique: Option<f32>,
        colours: &StyleColours,
    ) {
        style.set_font_hinting(text_hinting(true));
        #[cfg(test)]
        TAKES.set(TAKES.get() + 1);
        let decoration = StyleDecoration::take(
            &mut style,
            &mut self.collection,
            self.epoch.as_ref(),
            colours,
        )
        .map(Box::new);
        let shadows = take_shadows(&mut style, colours);
        let background = StyleBackground::take(
            &mut style,
            &mut self.collection,
            self.epoch.as_ref(),
        )
        .map(Box::new);
        let taken = Taken {
            decoration,
            shadows,
            background,
        };
        self.push(style, oblique, taken);
    }

    /// Pushes `style`, as [`Self::push_style`] does, where the caller knows
    /// it has no decoration, shadow or background to take off: built from
    /// a description that names none, as a crate `TextStyle` is.
    pub(crate) fn push_style_as_given(
        &mut self,
        mut style: TextStyle,
        oblique: Option<f32>,
    ) {
        style.set_font_hinting(text_hinting(true));
        debug_assert!(
            as_given(&style),
            "a style pushed as given has nothing to take off"
        );
        let taken = Taken {
            decoration: None,
            shadows: Vec::new(),
            background: None,
        };
        self.push(style, oblique, taken);
    }

    /// Pushes `style`, with `taken` taken off it.
    fn push(&mut self, style: TextStyle, oblique: Option<f32>, taken: Taken) {
        self.builder.push_style(&style);
        self.oblique |= oblique.is_some();
        self.half_leading |= style.half_leading();
        self.steps.push(Step::Push(self.pushed.len()));
        self.pushed.push(Pushed {
            parent: self.top,
            style,
            oblique,
            taken,
        });
        self.top = self.pushed.len();
    }

    pub(crate) fn pop(&mut self) {
        self.builder.pop();
        self.steps.push(Step::Pop);
        // As the builder's stack does, a pop with nothing pushed keeps the
        // paragraph's own style.
        if self.top != 0 {
            self.top = self.pushed[self.top - 1].parent;
        }
    }

    pub(crate) fn add_text(&mut self, text: &str) {
        self.builder.add_text(text);
        match (text.as_bytes().first(), text.as_bytes().last()) {
            (Some(&first), Some(&last)) => self.meet(first, last, true),
            _ => self.edge_waiting = true,
        }
        let start = self.length;
        self.length += text.len();
        self.steps.push(Step::Text(start..self.length));
    }

    pub(crate) fn add_placeholder(&mut self, placeholder: &PlaceholderStyle) {
        self.builder.add_placeholder(placeholder);
        let mut bytes = [0; 4];
        let bytes = PLACEHOLDER_CHARACTER.encode_utf8(&mut bytes).as_bytes();
        self.meet(bytes[0], bytes[bytes.len() - 1], false);
        self.length += bytes.len();
        self.steps.push(Step::Placeholder(placeholder.clone()));
    }

    /// Notes bytes from `first` to `last` added where the builder's text
    /// ends, by a text step where `text` and by a placeholder otherwise:
    /// whether [`ascii_boundary`] holds where they start, for a text step
    /// starting there -- this one, or an empty one waiting -- from the byte
    /// before and `first`, as it would read them from the builder's text.
    fn meet(&mut self, first: u8, last: u8, text: bool) {
        if text || self.edge_waiting {
            self.ascii_edges &= self.last_byte.is_none_or(|before| {
                before.is_ascii()
                    && first.is_ascii()
                    && !(before == b'\r' && first == b'\n')
            });
        }
        self.edge_waiting = false;
        self.last_byte = Some(last);
    }

    /// Withdraws the `one_paint` [`Self::new`] was given: a style has been
    /// pushed whose glyph paint may differ from the paragraph's own.
    pub(crate) fn paints_differ(&mut self) {
        self.one_paint = false;
    }

    /// The collection whose paragraph cache this paragraph's layouts have to
    /// go without, where a style splits its leading -- see
    /// [`without_paragraph_cache`].
    pub(crate) fn uncached(&self) -> Option<FontCollection> {
        self.half_leading.then(|| self.collection.clone())
    }

    /// The paragraph, not yet laid out, and what paints it where it has an
    /// oblique range, a decoration, a shadow or a background, or a colour
    /// change that a layout may find inside a ligature -- see [`Passes`]. A
    /// paragraph with none of these is painted by `Paragraph::paint` alone.
    pub(crate) fn build(mut self) -> (Paragraph, Option<Passes>) {
        // Asked of what was taken off rather than of the boxes, which are
        // collected only for a paragraph that has passes to hand them to.
        let drawn = self.oblique
            || self.taken().any(|taken| {
                taken.decoration.is_some()
                    || !taken.shadows.is_empty()
                    || taken.background.is_some()
            });
        // With one style there is no edge to fall inside a cluster.
        if !drawn && self.pushed.is_empty() {
            return (self.builder.build(), None);
        }
        #[cfg(test)]
        let full = FULL_GRAPHEMES.get();
        #[cfg(not(test))]
        let full = false;
        #[cfg(test)]
        let one_paint = self.one_paint && !IGNORE_ONE_PAINT.get();
        #[cfg(not(test))]
        let one_paint = self.one_paint;
        // In one paint, with every edge between two clusters, nothing moves
        // and no edge is a paint edge -- see `Self::new`.
        if !drawn && one_paint && self.ascii_edges && !full {
            return (self.builder.build(), None);
        }
        // Read back and copied only where an edge may fall inside a
        // grapheme cluster, the one thing the text is read for here -- see
        // `ascii_boundary`, judged as the text was added.
        let text = (full || !self.ascii_edges).then(|| {
            #[cfg(test)]
            TEXT_READS.set(TEXT_READS.get() + 1);
            self.builder.get_text().to_owned()
        });
        #[cfg(test)]
        let asked_text = self.builder.get_text().to_owned();
        // The builder holds the text where it was added, so once an edge
        // has moved, only the steps say where it went.
        let (moved, edges) = self.keep_graphemes_whole(text.as_deref());
        let paragraph = match (moved, &text) {
            (true, Some(text)) => self.replay(text, |_, _, _| None),
            _ => self.builder.build(),
        };
        if !drawn && edges.is_empty() {
            return (paragraph, None);
        }
        let (boxes, box_slots) = self.boxes();
        let slots = self.slot_ranges();
        let style_edges = slots
            .windows(2)
            .filter(|pair| pair[0].0.end == pair[1].0.start)
            .map(|pair| pair[1].0.start)
            .collect();
        let (paints, sheared): (Vec<Paint>, Vec<Option<f32>>) = self
            .text_styles()
            .zip(
                once(self.base_oblique)
                    .chain(self.pushed.iter().map(|pushed| pushed.oblique)),
            )
            .map(|(style, oblique)| (foreground(style), oblique))
            .unzip();
        let mut background_slots = Vec::with_capacity(self.pushed.len() + 1);
        let mut backgrounds = Vec::new();
        for taken in self.taken() {
            background_slots.push(taken.background.as_ref().map(
                |background| {
                    backgrounds.push(StyleBackground::clone(background));
                    backgrounds.len() - 1
                },
            ));
        }
        let (boxes, box_shadows) = boxes
            .into_iter()
            .map(|decorated| (decorated.decoration, decorated.shadows))
            .unzip();
        let passes = Passes {
            #[cfg(test)]
            as_asked: self.as_asked(&asked_text),
            box_ranges: remap(&slots, &box_slots),
            background_ranges: remap(&slots, &background_slots),
            slots,
            paints,
            sheared,
            shadows: once(mem::take(&mut self.base_taken.shadows))
                .chain(
                    self.pushed
                        .iter_mut()
                        .map(|pushed| mem::take(&mut pushed.taken.shadows)),
                )
                .collect(),
            boxes,
            box_shadows,
            backgrounds,
            runs: Vec::new(),
            spans: Vec::new(),
            background_spans: Vec::new(),
            lines: 0,
            ligature_edges: match drawn {
                true => None,
                false => Some(edges),
            },
            drawn,
            style_edges,
            skia_glyphs: false,
            followed: false,
            #[cfg(any(test, debug_assertions))]
            follows: 0,
            #[cfg(test)]
            per_line_only: false,
        };
        (paragraph, Some(passes))
    }

    /// The style in `slot`: the paragraph's own at 0, and each pushed one at
    /// its index in `styles` plus 1.
    fn text_style(&self, slot: usize) -> &TextStyle {
        match slot {
            0 => self.style.text_style(),
            _ => &self.pushed[slot - 1].style,
        }
    }

    /// What was taken off each style, by slot, the paragraph's own first.
    fn taken(&self) -> impl Iterator<Item = &Taken> {
        once(&self.base_taken)
            .chain(self.pushed.iter().map(|pushed| &pushed.taken))
    }

    /// What was taken off the style in `slot` -- see [`Self::text_style`].
    #[cfg(test)]
    fn taken_at(&self, slot: usize) -> &Taken {
        match slot {
            0 => &self.base_taken,
            _ => &self.pushed[slot - 1].taken,
        }
    }

    /// Every style, the paragraph's own first.
    fn text_styles(&self) -> impl Iterator<Item = &TextStyle> {
        once(self.style.text_style())
            .chain(self.pushed.iter().map(|pushed| &pushed.style))
    }

    /// Each decorated style's decoration, by slot, and which box each slot
    /// is: `None` for one with no decoration.
    fn boxes(&self) -> (Vec<StyleDecoration>, Vec<Option<usize>>) {
        #[cfg(test)]
        BOXES_MADE.set(BOXES_MADE.get() + 1);
        let mut boxes = Vec::new();
        let slots = self
            .taken()
            .map(|taken| {
                taken.decoration.as_ref().map(|decoration| {
                    boxes.push(StyleDecoration::clone(decoration));
                    boxes.len() - 1
                })
            })
            .collect();
        (boxes, slots)
    }

    /// The text each slot's style covers, in UTF-8 offsets into the
    /// paragraph's text, in order, with the slot.
    fn slot_ranges(&self) -> Vec<(Range<usize>, usize)> {
        let mut stack = Vec::with_capacity(self.pushed.len() + 1);
        stack.push(0);
        let mut ranges: Vec<(Range<usize>, usize)> =
            Vec::with_capacity(self.steps.len());
        for step in &self.steps {
            match step {
                Step::Push(at) => stack.push(at + 1),
                Step::Pop if stack.len() > 1 => {
                    stack.pop();
                }
                Step::Pop | Step::Placeholder(_) => {}
                Step::Text(bytes) => {
                    // SAFETY: `stack` starts with the base style's slot and a
                    // pop never takes it, so it is never empty.
                    let index = *stack.last().unwrap();
                    push_range(&mut ranges, bytes.clone(), index);
                }
            }
        }
        ranges
    }

    /// The paragraph as its caller asked for it, its shadows and
    /// backgrounds SkParagraph's to draw: the control for what [`Passes`]
    /// draws in their place.
    #[cfg(test)]
    fn as_asked(&self, text: &str) -> Option<Paragraph> {
        Some(self.replay(text, |style, _, slot| {
            let taken = self.taken_at(slot);
            let mut style = restore_shadows(style, &taken.shadows)
                .unwrap_or_else(|| style.clone());
            if let Some(background) = &taken.background {
                style.set_background_paint(&background.paint);
            }
            Some(style)
        }))
    }

    /// Moves each edge that falls inside a grapheme cluster, beside an
    /// oblique piece of text or between two that SkParagraph shapes as one,
    /// to the cluster's end -- see [`whole_graphemes`]. Says whether any
    /// did, and where the edges that remain between two pieces shaped as
    /// one with different glyph paints lie: those can still fall inside a
    /// glyph cluster, a ligature's, which only the layout tells -- see
    /// [`Passes::follow`]. `text` is the builder's, or `None` where every
    /// edge is known to lie between two clusters, and nothing moves -- see
    /// [`Self::edges_between_clusters`].
    fn keep_graphemes_whole(
        &mut self,
        text: Option<&str>,
    ) -> (bool, Vec<usize>) {
        let Some(text) = text else {
            return (false, self.edges_between_clusters());
        };
        let mut stack = Vec::with_capacity(self.pushed.len() + 1);
        stack.push((self.base_oblique.is_some(), 0));
        let mut pieces = Vec::with_capacity(self.steps.len());
        for (at, step) in self.steps.iter().enumerate() {
            match step {
                Step::Push(style) => {
                    stack.push((
                        self.pushed[*style].oblique.is_some(),
                        style + 1,
                    ));
                }
                Step::Pop if stack.len() > 1 => {
                    stack.pop();
                }
                Step::Pop | Step::Placeholder(_) => {}
                Step::Text(bytes) => {
                    // SAFETY: `stack` starts with the base style's entry and
                    // a pop never takes it, so it is never empty.
                    let (oblique, slot) = *stack.last().unwrap();
                    pieces.push(Piece {
                        step: at,
                        bytes: bytes.clone(),
                        oblique,
                        slot,
                    });
                }
            }
        }
        whole_graphemes(text, &mut pieces, |left, right| {
            self.shaped_as_one(left, right)
        });
        let mut edges = Vec::new();
        let mut owner: Option<&Piece> = None;
        for piece in pieces.iter().filter(|piece| !piece.bytes.is_empty()) {
            if let Some(left) = owner
                && left.bytes.end == piece.bytes.start
                && self.paint_edge(left.slot, piece.slot)
            {
                edges.push(piece.bytes.start);
            }
            owner = Some(piece);
        }
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
        (moved, edges)
    }

    /// The edges [`Self::keep_graphemes_whole`] finds where every edge
    /// lies between two grapheme clusters, so that no piece of text moves:
    /// found in one walk of the steps, the style each piece is in followed
    /// through [`Pushed::parent`], with nothing collected but the edges.
    fn edges_between_clusters(&self) -> Vec<usize> {
        let mut edges = Vec::new();
        let mut slot = 0;
        // The end and slot of the last text that was not empty.
        let mut owner: Option<(usize, usize)> = None;
        for step in &self.steps {
            match step {
                Step::Push(style) => slot = style + 1,
                Step::Pop if slot != 0 => slot = self.pushed[slot - 1].parent,
                Step::Pop | Step::Placeholder(_) => {}
                Step::Text(bytes) if bytes.is_empty() => {}
                Step::Text(bytes) => {
                    if let Some((end, left)) = owner
                        && end == bytes.start
                        && self.paint_edge(left, slot)
                    {
                        edges.push(bytes.start);
                    }
                    owner = Some((bytes.end, slot));
                }
            }
        }
        edges
    }

    /// Whether SkParagraph shapes the styles in slots `left` and `right`
    /// as one font, so that a ligature can form across an edge between
    /// them.
    fn shaped_as_one(&self, left: usize, right: usize) -> bool {
        self.text_style(left)
            .match_one_attribute(StyleType::Font, self.text_style(right))
    }

    /// Whether an edge between text in slots `left` and `right` is one a
    /// ligature can straddle in two paints: the two shaped as one, with
    /// glyph paints that differ.
    fn paint_edge(&self, left: usize, right: usize) -> bool {
        #[cfg(test)]
        PAINT_COMPARISONS.set(PAINT_COMPARISONS.get() + 1);
        self.shaped_as_one(left, right)
            && foreground(self.text_style(left))
                != foreground(self.text_style(right))
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

    /// A paragraph from these steps and `text`, the builder's, with each
    /// style -- the paragraph style's own included -- replaced by what
    /// `restyle` makes of it, of whether it asks for oblique and of its
    /// slot, where that is `Some`.
    fn replay(
        &self,
        text: &str,
        restyle: impl Fn(&TextStyle, Option<f32>, usize) -> Option<TextStyle>,
    ) -> Paragraph {
        self.replay_builder(text, restyle).build()
    }

    /// The builder [`Self::replay`] builds its paragraph with.
    fn replay_builder(
        &self,
        text: &str,
        restyle: impl Fn(&TextStyle, Option<f32>, usize) -> Option<TextStyle>,
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
                    let Pushed { style, oblique, .. } = &self.pushed[*at];
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

impl Drop for ParagraphRecipe {
    /// Hands the recipe's lists back, emptied, for the next recipe on the
    /// thread, where they are no longer than [`KEPT_LIST_CAPACITY`] and
    /// longer than what is kept already.
    fn drop(&mut self) {
        let mut steps = mem::take(&mut self.steps);
        let mut pushed = mem::take(&mut self.pushed);
        steps.clear();
        pushed.clear();
        // Nothing to hand back to once the thread's storage is gone, nor
        // while something else on the thread holds it.
        let _ = SPARE_LISTS.try_with(|spare| {
            if let Ok(mut spare) = spare.try_borrow_mut() {
                if steps.capacity() <= KEPT_LIST_CAPACITY
                    && steps.capacity() > spare.0.capacity()
                {
                    spare.0 = steps;
                }
                if pushed.capacity() <= KEPT_LIST_CAPACITY
                    && pushed.capacity() > spare.1.capacity()
                {
                    spare.1 = pushed;
                }
            }
        });
    }
}

/// A style pushed onto a [`ParagraphRecipe`]: the slot it was pushed over,
/// the style, without what was taken off it, and the angle of the oblique
/// it asks for, if any.
struct Pushed {
    /// The slot of the style it was pushed over, which a pop returns to.
    parent: usize,
    style: TextStyle,
    oblique: Option<f32>,
    taken: Taken,
}

/// What was taken off a style to be drawn here rather than by SkParagraph
/// -- see [`Passes`]: its decoration, its shadows and its background.
///
/// The decoration and the background each behind a pointer, made only for
/// a style that has one: a recipe moves one of these for its own style and
/// keeps one for each style pushed, and most styles have neither.
struct Taken {
    decoration: Option<Box<StyleDecoration>>,
    shadows: Vec<Shadow>,
    background: Option<Box<StyleBackground>>,
}

/// One piece of a paragraph's text: the step that adds it, its bytes,
/// whether its style asks for oblique, and its style's slot.
struct Piece {
    step: usize,
    bytes: Range<usize>,
    oblique: bool,
    slot: usize,
}

#[cfg(test)]
thread_local! {
    /// Whether [`ParagraphRecipe::build`] reads the text for every edge, as
    /// the reference its byte-pair shortcut is held to -- see
    /// [`ascii_boundary`].
    pub(crate) static FULL_GRAPHEMES: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
    /// How many times [`ParagraphRecipe::build`] has read the builder's
    /// text back on this thread, for the test that holds a paragraph whose
    /// edges are all ASCII to reading none.
    pub(crate) static TEXT_READS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
    /// How many pushed styles [`ParagraphRecipe::push_style`] has taken
    /// decorations, shadows and backgrounds off on this thread, for the
    /// tests that hold a plain style to none.
    pub(crate) static TAKES: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
    /// How many times [`ParagraphRecipe::boxes`] has collected a recipe's
    /// decorations on this thread, for the test that holds a paragraph
    /// [`Passes`] has nothing to draw for to collecting none.
    pub(crate) static BOXES_MADE: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
    /// How many times the edge walk has compared two styles' glyph paints
    /// on this thread, for the test that holds a crate paragraph in one
    /// paint to none.
    pub(crate) static PAINT_COMPARISONS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
    /// Whether [`ParagraphRecipe::build`] walks the edges of a paragraph
    /// vouched to be in one paint as it walks any other's: the reference
    /// the vouching is held to.
    pub(crate) static IGNORE_ONE_PAINT: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

/// Whether a grapheme cluster boundary lies at byte `at` of `text`, judged
/// from the two bytes either side of it alone: where both are ASCII and are
/// not CR then LF, it does, and `false` here means only that the two bytes
/// cannot say.
///
/// UAX #29, section 3.1.1, breaks everywhere except where a rule says not
/// to, and these are the rules that join two characters across a point:
/// GB3, CR × LF, which is excluded here; GB9b, Prepend × anything, which
/// needs a Prepend character before the point, and no ASCII character is
/// one; and GB6 to GB8, GB9, GB9a, GB9c, GB11 and GB12 to GB13, which each
/// need the character after it to be Hangul, Extend, ZWJ, a SpacingMark, an
/// Indic consonant, Extended_Pictographic or a Regional_Indicator, which no
/// ASCII character is either. GB4 and GB5 only break, and the ends of the
/// text are boundaries by GB1 and GB2. The properties are Unicode 17.0's,
/// the version `unicode-segmentation` carries.
fn ascii_boundary(text: &str, at: usize) -> bool {
    let bytes = text.as_bytes();
    if at == 0 || at >= bytes.len() {
        return true;
    }
    let (before, after) = (bytes[at - 1], bytes[at]);
    before.is_ascii()
        && after.is_ascii()
        && !(before == b'\r' && after == b'\n')
}

/// Moves each edge between two adjacent pieces of `text` that falls inside
/// a grapheme cluster to the cluster's end, so that the style of the
/// cluster's base takes the whole of it, where either piece is oblique or
/// `shaped_as_one` says SkParagraph shapes the two pieces' styles, by slot,
/// as one font.
///
/// Grapheme clusters rather than SkParagraph's glyph clusters, which cannot
/// tell the two cases at an oblique edge apart: a ligature across the edge
/// is one glyph cluster and should break, since an oblique is a font of its
/// own, and a base and its combining marks are one too and should not,
/// since a mark shaped apart from its base loses the glyph it attaches to;
/// the base decides which glyph that is, so its style is the cluster's.
///
/// Between two pieces shaped as one, the glyphs are shared and only their
/// paint can differ. Chrome 156 paints a grapheme split that way whole, in
/// its base's style, a mark that is a glyph of its own included, where
/// SkParagraph cuts a glyph by the share of its UTF-8 bytes each side
/// holds, and loses a mark beside a base it has cut. CSS Text 3, section
/// 1.4, leaves the case undefined and allows either side. Where the fonts
/// differ, the two are shaped apart and each side keeps its own style, as in
/// Chrome, unless one is oblique.
///
/// Only between two pieces with text that meet: an empty piece holds no
/// base, and a placeholder stands in the text as U+FFFC, which no piece
/// holds, so the pieces either side of one never meet, whatever empty
/// pieces lie between, and its edges never move. A mark right after one is
/// a grapheme with it and has no base to go with, so it stays in its own
/// piece and is shaped alone, as SkParagraph shapes it. Edges only move
/// later and never past the next, so the pieces still hold the same bytes.
fn whole_graphemes(
    text: &str,
    pieces: &mut [Piece],
    shaped_as_one: impl Fn(usize, usize) -> bool,
) {
    // The end of the grapheme cluster `at` falls in, or `at` where it falls
    // between two.
    let cluster_end = |at: usize| {
        let mut cursor = GraphemeCursor::new(at, text.len(), true);
        match cursor.is_boundary(text, 0) {
            Ok(true) => at,
            _ => cursor
                .next_boundary(text, 0)
                .ok()
                .flatten()
                .unwrap_or(text.len()),
        }
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
            && (pieces[left].oblique
                || pieces[at].oblique
                || shaped_as_one(pieces[left].slot, pieces[at].slot))
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

/// Whether a paragraph style's own text style can be built from as it
/// stands: hinted, and with no decoration, shadow or background for
/// [`Passes`] to draw in SkParagraph's place -- see [`ParagraphRecipe::new`].
fn as_given(style: &TextStyle) -> bool {
    #[cfg(test)]
    if COPY_ALWAYS.get() {
        return false;
    }
    style.font_hinting() == text_hinting(true)
        && style.decoration_type() == TextDecoration::NO_DECORATION
        && style.shadows().is_empty()
        && !style.has_background()
}

#[cfg(any(test, all(feature = "node-addon", debug_assertions)))]
thread_local! {
    /// How many paragraph styles [`ParagraphRecipe::new`] has copied on this
    /// thread -- see [`style_copies`].
    static STYLE_COPIES: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

#[cfg(test)]
thread_local! {
    /// Whether [`ParagraphRecipe::new`] copies every paragraph style, as the
    /// reference a style built from as it stands is held to.
    pub(crate) static COPY_ALWAYS: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

/// How many paragraph styles [`ParagraphRecipe::new`] has copied on this
/// thread, for the tests to hold each caller to handing in one that needs
/// no copy: none for a style hinted and with nothing to take off it.
#[cfg(any(test, all(feature = "node-addon", debug_assertions)))]
pub(crate) fn style_copies() -> usize {
    STYLE_COPIES.get()
}

/// Takes `style`'s text shadows off it, for [`Passes`] to draw.
fn take_shadows(style: &mut TextStyle, colours: &StyleColours) -> Vec<Shadow> {
    let shadows = Shadow::of(style, colours);
    if !shadows.is_empty() {
        style.reset_shadows();
    }
    shadows
}

/// A text shadow as SkParagraph holds it, and its colour as the style named
/// it: SkParagraph's is an eight-bit `SkColor`, and [`Passes`] draws every
/// shadow itself, so it draws them in this one.
#[derive(Clone)]
pub(crate) struct Shadow {
    shadow: TextShadow,
    /// In extended sRGB, unpremultiplied.
    color: Color4f,
}

impl Shadow {
    /// `style`'s shadows, each in its colour from `colours` where that
    /// names one, and in SkParagraph's otherwise.
    fn of(style: &TextStyle, colours: &StyleColours) -> Vec<Self> {
        style
            .shadows()
            .iter()
            .enumerate()
            .map(|(at, shadow)| Self {
                shadow: *shadow,
                color: colours
                    .shadows
                    .get(at)
                    .copied()
                    .unwrap_or_else(|| Color4f::from(shadow.color)),
            })
            .collect()
    }
}

/// The colours of a style that SkParagraph holds in eight bits -- its
/// decoration's and its text shadows' -- as the style named them, in
/// extended sRGB, unpremultiplied. [`Passes`] draws both itself, in these.
/// Empty where the caller has none to give, which leaves SkParagraph's.
#[derive(Clone, Debug, Default)]
pub(crate) struct StyleColours {
    /// The decoration's colour.
    pub(crate) decoration: Option<Color4f>,
    /// One for each of the style's text shadows, in its order.
    pub(crate) shadows: Vec<Color4f>,
}

/// `style` with `shadows` given back, where it has any.
fn restore_shadows(style: &TextStyle, shadows: &[Shadow]) -> Option<TextStyle> {
    if shadows.is_empty() {
        return None;
    }
    let mut style = style.clone();
    for shadow in shadows {
        style.add_shadow(shadow.shadow);
    }
    Some(style)
}

/// The paint SkParagraph draws `style`'s glyphs with: its foreground where it
/// has one, and otherwise a default paint in its colour --
/// `TextLine::buildTextBlob`'s.
fn foreground(style: &TextStyle) -> Paint {
    match style.has_foreground() {
        true => style.foreground(),
        false => {
            let mut paint = Paint::default();
            paint.set_color(style.color());
            paint
        }
    }
}

/// A style's background, taken off it so that SkParagraph draws none, and
/// the face whose content area it covers.
#[derive(Clone)]
struct StyleBackground {
    paint: Paint,
    face: FaceLines,
}

impl StyleBackground {
    /// Takes `style`'s background off it. CSS paints an inline box's
    /// background over its content area -- CSS 2.1 section 10.6.1, CSS
    /// Inline 3 -- which is the first available font's ascent and descent
    /// and nothing of the line height, so the face is that font, at the
    /// style's size and axes -- see [`FaceLines::first_available`].
    fn take(
        style: &mut TextStyle,
        collection: &mut FontCollection,
        epoch: Option<&Rc<FontEpoch>>,
    ) -> Option<Self> {
        if !style.has_background() {
            return None;
        }
        let paint = style.background();
        style.clear_background_color();
        let families = style.font_families();
        let families: Vec<&str> = families.iter().collect();
        let face = FaceLines::first_available(
            collection,
            epoch,
            &families,
            style.font_style(),
            style.font_size(),
            style.font_arguments(),
        )?;
        Some(Self { paint, face })
    }
}

/// A style's decoration, taken off it so that SkParagraph draws none, and
/// the style's text shadows, which shadow the decoration as they do the
/// glyphs.
#[derive(Clone)]
struct StyleDecoration {
    decoration: Decoration,
    shadows: Vec<Shadow>,
}

impl StyleDecoration {
    /// Takes `style`'s decoration off it, placed by the style's first
    /// available font in `collection` -- see [`FaceLines::first_available`].
    ///
    /// The colour is `colours`' decoration colour where it names one, and
    /// the style's as it stands otherwise: both callers set that to the
    /// text's colour where none was asked for, since the text's goes in as
    /// a foreground paint that a style's own colour does not report.
    fn take(
        style: &mut TextStyle,
        collection: &mut FontCollection,
        epoch: Option<&Rc<FontEpoch>>,
        colours: &StyleColours,
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
            epoch,
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
                color: Some(colours.decoration.unwrap_or_else(|| {
                    Color4f::from(style.decoration_color())
                })),
                // `drawParagraph` applies a canvas's global alpha to the
                // whole paragraph through a layer.
                opacity: 1.0,
                face,
            },
            shadows: Shadow::of(style, colours),
        })
    }
}

/// Runs `lay_out` with `collection`'s paragraph cache off where one is
/// given -- see [`ParagraphRecipe::uncached`] -- and on again after.
///
/// SkParagraph's cache matches two paragraphs whose styles differ only in
/// `halfLeading` and hands the second the first's runs, whose ascent and
/// descent the first's leading rule already set, so whichever was laid out
/// first decides both. With the cache off for its layouts, a paragraph that
/// splits its leading neither takes another's runs nor leaves its own for
/// another to take, and every other paragraph keeps the cache. Nothing in
/// this crate turns a collection's cache off otherwise, so on is what it
/// goes back to.
///
/// The flag is the collection's, so no other layout may run on it between
/// the two switches, and none can: skia-safe's `FontCollection` is neither
/// `Send` nor `Sync`, and nothing here wraps one in skia-safe's `Sendable`,
/// so a collection is used only on the thread that made it -- each thread
/// has its own `FontLibrary`, and `TextEngine` cannot leave its thread.
/// `Paragraph::layout` runs to the end without calling back into this
/// crate, so nothing on that thread lays out between the switches either.
// UPSTREAM: skia-safe 0.153.3 -- https://github.com/google/skia/commit/e0d79efbf171fdd78063f7673d517491154c9a7a -- worked around
// Re-check: cargo test a_half_leading_paragraph_is_not_laid_out_as_another_was
pub(crate) fn without_paragraph_cache<T>(
    collection: Option<&FontCollection>,
    lay_out: impl FnOnce() -> T,
) -> T {
    let Some(collection) = collection else {
        return lay_out();
    };
    let mut collection = collection.clone();
    collection.paragraph_cache_mut().turn_on(false);
    let laid_out = lay_out();
    collection.paragraph_cache_mut().turn_on(true);
    laid_out
}

/// What paints a paragraph that has an oblique range, a decoration, a shadow
/// or a background, or a ligature in two colours -- see
/// [`Self::ligature_edges`]: SkParagraph lays it out, and this draws every
/// line of it from the glyph runs `Paragraph::visit` reports, in three
/// passes: every line's backgrounds, then every line's text shadows, then
/// each line's underlines and overlines, glyphs and lines-through before the
/// next line's. The last pass is CSS 2.1 Appendix E's order for a line box,
/// so a wrapped line's overline lies over the descenders of the line above,
/// as in Chrome. The shadows come out of it because CSS Text Decoration 3
/// section 4 keeps a shadow beneath text -- its own, and its neighbours' --
/// where Chrome paints a later span's or line's shadow over earlier glyphs.
///
/// Where that order changes nothing, the glyphs are `Paragraph::paint`'s,
/// between the lines beneath them and the lines over them -- see
/// [`Self::skia_glyphs`].
pub(crate) struct Passes {
    /// The text each style covers, in UTF-8 offsets into the paragraph's
    /// text, in order, with its slot -- see [`ParagraphRecipe::shadows`].
    slots: Vec<(Range<usize>, usize)>,
    /// Each slot's glyph paint, whether it asks for oblique, and its text
    /// shadows.
    paints: Vec<Paint>,
    sheared: Vec<Option<f32>>,
    shadows: Vec<Vec<Shadow>>,
    /// Each decorated style's decoration, by box, its text shadows, and the
    /// text each box decorates -- see `decorate::box_at`.
    boxes: Vec<Decoration>,
    box_shadows: Vec<Vec<Shadow>>,
    box_ranges: Vec<(Range<usize>, usize)>,
    /// Each style's background, by box, and the text each covers.
    backgrounds: Vec<StyleBackground>,
    background_ranges: Vec<(Range<usize>, usize)>,
    /// What this layout draws, in line order, found at its first paint --
    /// see [`Self::follow`].
    runs: Vec<PaintedRun>,
    spans: Vec<Span>,
    background_spans: Vec<Span>,
    lines: usize,
    /// For a paragraph with nothing else to draw here, the edges between
    /// two styles shaped as one with different glyph paints -- see
    /// [`ParagraphRecipe::keep_graphemes_whole`]. The first paint after
    /// each layout asks whether one falls inside a glyph cluster, and only
    /// then draws the paragraph here.
    ligature_edges: Option<Vec<usize>>,
    /// Whether this layout is drawn here rather than by `Paragraph::paint`.
    drawn: bool,
    /// The edges between two styles, in UTF-8 offsets: where `Paragraph::
    /// paint` cuts a glyph cluster that one falls inside, which is drawn
    /// here whole -- see [`Self::painted`].
    style_edges: Vec<usize>,
    /// Whether this layout's glyphs are left to `Paragraph::paint`, drawn
    /// between the lines beneath them and the lines over them, rather than
    /// rebuilt here run by run -- see [`Self::skia_glyphs`].
    skia_glyphs: bool,
    /// Whether the fields above that describe a layout describe the current
    /// one: false from each layout until the first paint after it follows
    /// the paragraph.
    followed: bool,
    /// How many times a layout has been followed, for the tests to hold
    /// that one laid out and never painted is followed never, and one
    /// painted again is not followed again.
    #[cfg(any(test, debug_assertions))]
    pub(crate) follows: usize,
    /// Whether every layout is drawn run by run here, for the tests to hold
    /// the two ways of drawing against each other.
    #[cfg(test)]
    per_line_only: bool,
    /// The paragraph with its shadows and backgrounds SkParagraph's to draw
    /// -- see [`ParagraphRecipe::as_asked`].
    #[cfg(test)]
    pub(crate) as_asked: Option<Paragraph>,
}

/// The glyphs of one style in one font on one line, sheared where the style
/// asks for oblique.
struct PaintedRun {
    blob: TextBlob,
    origin: Point,
    line: usize,
    /// The style the glyphs' text is in.
    slot: usize,
    /// Whether the glyphs are in an oblique style.
    sheared: bool,
    /// The glyphs and their positions before any shear, for the tests to
    /// hold against SkParagraph's runs.
    #[cfg(test)]
    glyphs: Vec<skia_safe::GlyphId>,
    #[cfg(test)]
    positions: Vec<Point>,
}

/// How near two reports of a glyph's place are the same place, in pixels: a
/// cluster SkParagraph draws for two styles comes in two blobs whose
/// origins are worked out from different clips, so their sums can part in
/// the last bits.
const SAME_PLACE: f32 = 1.0 / 64.0;

/// `slots`, the text each slot covers, with each slot mapped through
/// `map` and a slot it maps to `None` left out, neighbours mapped alike
/// joined.
fn remap(
    slots: &[(Range<usize>, usize)],
    map: &[Option<usize>],
) -> Vec<(Range<usize>, usize)> {
    let mut ranges = Vec::with_capacity(slots.len());
    for (bytes, slot) in slots {
        if let Some(index) = map[*slot] {
            push_range(&mut ranges, bytes.clone(), index);
        }
    }
    ranges
}

/// Adds `bytes` in `index` to `ranges`, joined to the last where it
/// carries on from it in the same index, and nothing where it is empty.
fn push_range(
    ranges: &mut Vec<(Range<usize>, usize)>,
    bytes: Range<usize>,
    index: usize,
) {
    match ranges.last_mut() {
        Some((last, at)) if *at == index && last.end == bytes.start => {
            last.end = bytes.end;
        }
        _ if bytes.is_empty() => {}
        _ => ranges.push((bytes, index)),
    }
}

/// The glyph runs `Paragraph::visit` reports, each glyph once.
///
/// SkParagraph makes a text blob for each run of one foreground in a
/// style, and where a style's edge falls inside a cluster it makes one for
/// each side, both holding the whole cluster and clipped to their share:
/// `visit` then reports the cluster's glyphs twice, at the end of one run
/// and the start of the next. They are kept the first time here, and
/// [`Passes::painted`] draws them whole.
fn records(paragraph: &mut Paragraph) -> Vec<GlyphRun> {
    let mut runs: Vec<GlyphRun> = Vec::new();
    for mut run in glyph_runs(paragraph) {
        if let Some(last) = runs.last()
            && last.line == run.line
        {
            let seen = |at: usize| {
                let place = run.origin + run.positions[at];
                last.glyphs.iter().enumerate().any(|(index, &glyph)| {
                    glyph == run.glyphs[at]
                        && last.starts[index] == run.starts[at]
                        && (last.origin + last.positions[index] - place)
                            .length()
                            <= SAME_PLACE
                })
            };
            let repeated =
                (0..run.glyphs.len()).take_while(|&at| seen(at)).count();
            if repeated == run.glyphs.len() {
                continue;
            }
            run.glyphs.drain(..repeated);
            run.positions.drain(..repeated);
            run.starts.drain(..repeated);
        }
        runs.push(run);
    }
    runs
}

impl Passes {
    /// The edges between styles shaped as one with different paints, for a
    /// paragraph here only for them -- see [`Self::ligature_edges`].
    #[cfg(test)]
    pub(crate) fn ligature_edges_for_test(&self) -> Option<&[usize]> {
        self.ligature_edges.as_deref()
    }

    /// Notes that the paragraph has been laid out again, at `_width`, so that
    /// the next paint follows it afresh -- see [`Self::follow`] -- and lets
    /// go of what the last layout drew.
    ///
    /// Nothing is read off the paragraph here: following it costs more than
    /// laying a short paragraph out, and one that is laid out to be measured
    /// is never drawn.
    pub(crate) fn lay_out(&mut self, _width: f32) {
        #[cfg(test)]
        if let Some(as_asked) = &mut self.as_asked {
            as_asked.layout(_width);
        }
        self.followed = false;
        self.runs = Vec::new();
        self.spans = Vec::new();
        self.background_spans = Vec::new();
        self.lines = 0;
    }

    /// Follows `paragraph`, as last laid out, unless that layout has been
    /// followed already: finds again the glyph runs, the decorations' spans
    /// and the backgrounds' spans.
    ///
    /// The glyphs are read with `Paragraph::visit`, through [`records`],
    /// which reports the text blobs `Paragraph::paint` draws: each glyph
    /// where it is painted and the text it comes from, on every line, and
    /// nothing for a placeholder.
    ///
    /// A glyph takes the style its text is in, or the one before it on the
    /// line where its text is in none: the glyphs of an ellipsis or a hyphen
    /// report offsets counted from the text they stand after, which can run
    /// past it. A glyph standing for a cluster of several characters takes
    /// its first's -- see [`Self::painted`].
    ///
    /// A paragraph here only for [`Self::ligature_edges`] is followed only
    /// where one of them falls inside a glyph cluster: a lookup for each
    /// edge, and otherwise `Paragraph::paint` draws it.
    pub(crate) fn follow(&mut self, paragraph: &mut Paragraph) {
        if self.followed {
            return;
        }
        self.followed = true;
        #[cfg(any(test, debug_assertions))]
        {
            self.follows += 1;
        }
        if let Some(edges) = &self.ligature_edges {
            self.drawn = edges.iter().any(|&edge| {
                paragraph
                    .get_glyph_cluster_at(edge)
                    .is_some_and(|cluster| cluster.text_range.start < edge)
            });
            if !self.drawn {
                self.runs.clear();
                self.lines = 0;
                return;
            }
        }
        self.skia_glyphs = self.skia_glyphs(paragraph);
        if self.skia_glyphs {
            let mut spans = Spans::default();
            let mut background_spans = Spans::default();
            let mut lines = 0;
            visit_glyph_runs(paragraph, |run| {
                lines = lines.max(run.line + 1);
                if !self.boxes.is_empty() {
                    spans
                        .add(&run, |at| decorate::box_at(&self.box_ranges, at));
                }
                if !self.backgrounds.is_empty() {
                    background_spans.add(&run, |at| {
                        decorate::box_at(&self.background_ranges, at)
                    });
                }
            });
            self.spans = spans.finish();
            self.background_spans = background_spans.finish();
            self.runs.clear();
            self.lines = lines;
            return;
        }
        let mut spans = Spans::default();
        let mut background_spans = Spans::default();
        let mut runs = Vec::new();
        let mut lines = 0;
        // The style the last glyph took, and the line it is on.
        let mut last = (usize::MAX, 0);
        for run in &records(paragraph) {
            let run = run.borrowed();
            lines = lines.max(run.line + 1);
            if !self.boxes.is_empty() {
                spans.add(&run, |at| decorate::box_at(&self.box_ranges, at));
            }
            if !self.backgrounds.is_empty() {
                background_spans.add(&run, |at| {
                    decorate::box_at(&self.background_ranges, at)
                });
            }
            if last.0 != run.line {
                last = (run.line, 0);
            }
            self.painted(&run, &mut last.1, &mut runs);
        }
        self.spans = spans.finish();
        self.background_spans = background_spans.finish();
        self.runs = runs;
        self.lines = lines;
    }

    /// Whether `paragraph`, just laid out, can have its glyphs drawn by
    /// `Paragraph::paint` with every line's underlines and overlines before
    /// them and every line's lines-through after, and look as it does drawn
    /// a line at a time.
    ///
    /// It can where nothing of it is drawn here for a reason of its own:
    /// no style asks for oblique, which SkParagraph would draw upright; no
    /// text shadow, which goes beneath every glyph; and no edge between two
    /// styles inside a glyph cluster, which SkParagraph cuts where this draws
    /// the cluster whole in its first style. And where the order of the
    /// lines changes nothing: with one line, or with no decoration at all --
    /// backgrounds are beneath every line's glyphs either way -- since on a
    /// second line an overline lies over the descenders above, and a line
    /// at a tight line height can reach the glyphs of the next.
    ///
    /// The spans are found as they are otherwise, from `Paragraph::visit`,
    /// which also fills the blob cache `Paragraph::paint` then draws from;
    /// what is saved is copying the runs and building their blobs here.
    fn skia_glyphs(&self, paragraph: &mut Paragraph) -> bool {
        #[cfg(test)]
        if self.per_line_only {
            return false;
        }
        self.sheared.iter().all(Option::is_none)
            // A decorated style's shadows are its slot's too, so this
            // covers a decoration's shadow as well as the glyphs'.
            && self.shadows.iter().all(Vec::is_empty)
            && (self.boxes.is_empty() || paragraph.line_number() <= 1)
            && self.style_edges.iter().all(|&edge| {
                paragraph
                    .get_glyph_cluster_at(edge)
                    .is_none_or(|cluster| cluster.text_range.start >= edge)
            })
    }

    /// Adds to `out` the pieces of `run`, a stretch of glyphs in one style
    /// each, `slot` carrying the style of the glyph before.
    ///
    /// A glyph is drawn whole in the style of the first character of its
    /// cluster. A cluster whose text crosses into a style with another glyph
    /// paint is a ligature across the edge, since an edge inside a grapheme
    /// cluster has moved to its end -- see [`whole_graphemes`] -- and Chrome
    /// 156 paints `f` and `i` in two colours as one `fi` in the first's.
    fn painted(
        &self,
        run: &GlyphRunRef<'_>,
        slot: &mut usize,
        out: &mut Vec<PaintedRun>,
    ) {
        let mut piece: Option<(Range<usize>, usize)> = None;
        for (at, &start) in run.starts.iter().enumerate() {
            let style =
                decorate::box_at(&self.slots, start as usize).unwrap_or(*slot);
            *slot = style;
            match &mut piece {
                Some((glyphs, last)) if *last == style => glyphs.end = at + 1,
                _ => {
                    if let Some((glyphs, last)) = piece.take() {
                        self.push_piece(run, glyphs, last, out);
                    }
                    piece = Some((at..at + 1, style));
                }
            }
        }
        if let Some((glyphs, last)) = piece {
            self.push_piece(run, glyphs, last, out);
        }
    }

    /// Adds `run`'s glyphs `at`, in style `slot`, to `out`.
    fn push_piece(
        &self,
        run: &GlyphRunRef<'_>,
        at: Range<usize>,
        slot: usize,
        out: &mut Vec<PaintedRun>,
    ) {
        let degrees = self.sheared[slot];
        let sheared = degrees.is_some();
        let shear = match degrees {
            Some(degrees) => run_shear(&run.font.typeface(), Some(degrees)),
            None => Shear::None,
        };
        let Some(blob) = sheared_blob(
            run.font,
            &run.glyphs[at.clone()],
            &run.positions[at.clone()],
            shear,
        ) else {
            return;
        };
        out.push(PaintedRun {
            blob,
            origin: run.origin,
            line: run.line,
            slot,
            sheared,
            #[cfg(test)]
            glyphs: run.glyphs[at.clone()].to_vec(),
            #[cfg(test)]
            positions: run.positions[at].to_vec(),
        });
    }

    /// Paints `paragraph`, which this was laid out with, at `origin`,
    /// following its layout first where no paint has since it was laid out
    /// -- see [`Self::follow`].
    pub(crate) fn paint(
        &mut self,
        paragraph: &mut Paragraph,
        canvas: &Canvas,
        origin: Point,
    ) {
        self.follow(paragraph);
        self.paint_followed(paragraph, canvas, origin);
    }

    /// Paints `paragraph`, whose layout this has followed, at `origin`, a
    /// line at a time -- see [`Passes`] -- each glyph drawn here with the
    /// paint SkParagraph would have used. Where the layout was found to be
    /// one whose order of lines changes nothing, the glyphs are
    /// `Paragraph::paint`'s instead, between every line's lines beneath and
    /// over them -- see [`Self::skia_glyphs`].
    fn paint_followed(
        &self,
        paragraph: &Paragraph,
        canvas: &Canvas,
        origin: Point,
    ) {
        if !self.drawn {
            paragraph.paint(canvas, origin);
            return;
        }
        let base = Paint::default();
        if self.skia_glyphs {
            self.paint_backgrounds(canvas, origin);
            for line in 0..self.lines {
                let spans = decorate::on_line(&self.spans, line);
                decorate::paint(
                    canvas,
                    spans,
                    &self.boxes,
                    origin,
                    Layer::Beneath,
                    &base,
                    PARAGRAPH_FAINT,
                );
            }
            paragraph.paint(canvas, origin);
            for line in 0..self.lines {
                let spans = decorate::on_line(&self.spans, line);
                decorate::paint(
                    canvas,
                    spans,
                    &self.boxes,
                    origin,
                    Layer::Over,
                    &base,
                    PARAGRAPH_FAINT,
                );
            }
            return;
        }
        let runs_on = |line: usize| {
            let start = self.runs.partition_point(|run| run.line < line);
            let end = self.runs.partition_point(|run| run.line <= line);
            &self.runs[start..end]
        };
        self.paint_backgrounds(canvas, origin);
        // Every line's shadows before any line's glyphs: CSS Text Decoration
        // 3 section 4 says shadows "never overlay the text itself" and that
        // UAs "should avoid painting text shadows over text in adjacent
        // elements", so a shadow stays beneath the glyphs of its own span,
        // of the spans before it on its line, and of the lines above.
        // Chrome paints each span's shadow with its text instead, which puts
        // a later shadow over earlier glyphs -- docs/browser-differences.md.
        for line in 0..self.lines {
            let spans = decorate::on_line(&self.spans, line);
            for run in runs_on(line) {
                for shadow in &self.shadows[run.slot] {
                    if let Some(paint) = shadow_paint(shadow, canvas) {
                        run.draw(canvas, origin + shadow.shadow.offset, &paint);
                    }
                }
            }
            for (index, shadows) in self.box_shadows.iter().enumerate() {
                for shadow in shadows {
                    if let Some(paint) = shadow_paint(shadow, canvas) {
                        decorate::paint_shadow(
                            canvas,
                            spans,
                            &self.boxes,
                            index,
                            origin + shadow.shadow.offset,
                            &paint,
                            PARAGRAPH_FAINT,
                        );
                    }
                }
            }
        }
        for line in 0..self.lines {
            let spans = decorate::on_line(&self.spans, line);
            decorate::paint(
                canvas,
                spans,
                &self.boxes,
                origin,
                Layer::Beneath,
                &base,
                PARAGRAPH_FAINT,
            );
            for run in runs_on(line) {
                run.draw(canvas, origin, &self.paints[run.slot]);
            }
            decorate::paint(
                canvas,
                spans,
                &self.boxes,
                origin,
                Layer::Over,
                &base,
                PARAGRAPH_FAINT,
            );
        }
    }
}

impl Passes {
    /// Every line's backgrounds, over the content area of the face each
    /// one's style first has -- see [`StyleBackground`].
    fn paint_backgrounds(&self, canvas: &Canvas, origin: Point) {
        for line in 0..self.lines {
            for span in decorate::on_line(&self.background_spans, line) {
                let background = &self.backgrounds[span.box_index];
                let baseline = origin.y + span.y;
                canvas.draw_rect(
                    Rect::new(
                        origin.x + span.left,
                        baseline + background.face.ascent,
                        origin.x + span.right,
                        baseline + background.face.descent,
                    ),
                    &background.paint,
                );
            }
        }
    }
}

impl PaintedRun {
    /// Draws the run with the paragraph's origin at `origin`.
    fn draw(&self, canvas: &Canvas, origin: Point, paint: &Paint) {
        canvas.draw_text_blob(&self.blob, origin + self.origin, paint);
    }
}

/// The paint a text shadow is drawn with on `canvas`: the shadow's colour
/// as its style named it,
/// blurred by its sigma where it has one. `None` for a shadow that casts
/// nothing.
///
/// The sigma is in pixels of the canvas whatever its transform, as
/// `CanvasParagraphPainter::drawTextShadow` takes it, and as a canvas
/// shadow's blur is. It goes to the mask filter in local units with
/// `respect_ctm` set, so that a replay at another scale -- an export
/// density, a canvas drawn into another at a size -- scales it with the
/// text. SkParagraph's own fixes it in device pixels, which kept a shadow as
/// wide in pixels at density 2 as at 1 while its text doubled. A transform
/// that maps no length to a positive one keeps the device sigma.
fn shadow_paint(shadow: &Shadow, canvas: &Canvas) -> Option<Paint> {
    let Shadow { shadow, color } = shadow;
    if !shadow.has_shadow() {
        return None;
    }
    let mut paint = Paint::default();
    paint.set_color4f(*color, None);
    if shadow.blur_sigma != 0.0 {
        let sigma = shadow.blur_sigma as f32;
        let scale = canvas
            .local_to_device_as_3x3()
            .map_radius(1.0)
            .filter(|s| s.is_normal() && *s > 0.0);
        paint.set_mask_filter(match scale {
            Some(scale) => {
                MaskFilter::blur(BlurStyle::Normal, sigma / scale, true)
            }
            None => MaskFilter::blur(BlurStyle::Normal, sigma, false),
        });
    }
    Some(paint)
}

#[cfg(test)]
mod tests {
    use super::{
        BOXES_MADE, FULL_GRAPHEMES, ParagraphRecipe, Piece, Step, StyleColours,
        TEXT_READS, add_slnt, ascii_boundary, match_oblique, set_coordinates,
        whole_graphemes,
    };
    use crate::{
        color::RgbaLinear,
        font::ObliqueAngle,
        font_library::text_hinting,
        node::typography::glyph_runs,
        started,
        text::{TextShadow as SpanShadow, TextSlant, TextStyle as SpanStyle},
    };
    use skia_safe::{
        Color, Data, FontHinting, FontMgr, FontStyle, GlyphId, ImageInfo,
        Point,
        font_style::{Slant, Weight, Width},
        textlayout::{
            FontCollection, ParagraphBuilder, ParagraphStyle,
            PlaceholderAlignment, PlaceholderStyle, TextBaseline, TextStyle,
            TypefaceFontProvider,
        },
    };
    use std::ops::Range;
    use unicode_segmentation::UnicodeSegmentation;

    /// The pieces of `text` at `ranges`, each oblique or not and each in a
    /// style of its own, and the text each holds once its edges have moved.
    /// Between two ranges that do not meet lies text no piece holds, as a
    /// placeholder's U+FFFC is.
    fn moved(text: &str, ranges: &[(Range<usize>, bool)]) -> Vec<String> {
        moved_where(text, ranges, |_, _| false)
    }

    /// [`moved`], with `shaped_as_one` saying which styles, by piece, are
    /// shaped as one font.
    fn moved_where(
        text: &str,
        ranges: &[(Range<usize>, bool)],
        shaped_as_one: impl Fn(usize, usize) -> bool,
    ) -> Vec<String> {
        let mut pieces: Vec<Piece> = ranges
            .iter()
            .enumerate()
            .map(|(at, (bytes, oblique))| Piece {
                step: at,
                bytes: bytes.clone(),
                oblique: *oblique,
                slot: at,
            })
            .collect();
        let held = |pieces: &[Piece]| -> Vec<usize> {
            pieces.iter().flat_map(|p| p.bytes.clone()).collect()
        };
        let before = held(&pieces);
        whole_graphemes(text, &mut pieces, shaped_as_one);
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
        // diaeresis is then upright beside an upright base in a font of
        // its own, an edge that does not move.
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
    fn an_upright_edge_moves_only_between_styles_shaped_as_one() {
        let text = format!("cafe{ACUTE} x");
        let n = text.len();
        assert_eq!(
            moved(&text, &[(0..4, false), (4..n, false)]),
            ["cafe".to_owned(), format!("{ACUTE} x")],
            "two fonts: each side keeps its own style"
        );
        assert_eq!(
            moved_where(&text, &[(0..4, false), (4..n, false)], |_, _| true),
            [format!("cafe{ACUTE}"), " x".to_owned()],
            "one font, two paints: the grapheme is its base's"
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
        assert_eq!(
            moved_where("effi", &[(0..3, false), (3..4, false)], |_, _| true),
            ["eff", "i"],
            "a grapheme edge, whatever glyph the font makes across it"
        );
    }

    /// A paragraph of `pieces`, each in Raleway in its own colour, laid out:
    /// whether it has [`super::Passes`], and whether they draw it.
    fn colours_drawn(pieces: &[(&str, Color)]) -> Option<bool> {
        let collection = raleway();
        let style = |color| {
            let mut style = TextStyle::new();
            style
                .set_font_families(&[RALEWAY])
                .set_font_size(SIZE)
                .set_color(color);
            style
        };
        let mut paragraph_style = ParagraphStyle::new();
        paragraph_style.set_text_style(&style(Color::BLACK));
        let none = StyleColours::default();
        let mut recipe = ParagraphRecipe::new(
            paragraph_style,
            None,
            collection,
            None,
            &none,
            false,
        );
        for (text, color) in pieces {
            recipe.push_style(style(*color), None, &none);
            recipe.add_text(text);
            recipe.pop();
        }
        let (mut paragraph, passes) = recipe.build();
        paragraph.layout(WIDTH);
        passes.map(|mut passes| {
            passes.lay_out(WIDTH);
            passes.follow(&mut paragraph);
            passes.drawn
        })
    }

    #[test]
    fn only_a_colour_change_inside_a_ligature_is_drawn_here() {
        assert_eq!(colours_drawn(&[("fi", Color::BLACK)]), None, "one colour");
        assert_eq!(
            colours_drawn(&[("ab", Color::BLACK), ("cd", Color::RED)]),
            Some(false),
            "two colours, no cluster between them: Paragraph::paint draws it"
        );
        assert_eq!(
            colours_drawn(&[("f", Color::BLACK), ("i", Color::RED)]),
            Some(true),
            "an fi ligature in two colours"
        );
        assert_eq!(
            colours_drawn(&[("e", Color::BLACK), ("\u{301}", Color::RED)]),
            None,
            "a mark goes to its base's style as the paragraph is built, \
             leaving no edge to look up"
        );
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
    /// an oblique one, and which in two colours the recipe draws whole where
    /// SkParagraph cuts it -- see [`super::Passes::painted`].
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
    ) -> (TextStyle, Option<f32>) {
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
        let degrees = ObliqueAngle::DEFAULT.degrees();
        let oblique = match_oblique(&mut style, collection, degrees);
        if oblique.is_some() {
            let mut coordinates = Vec::new();
            add_slnt(&mut coordinates, degrees);
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
        /// for what the sheared passes paint. It moves an edge inside a
        /// grapheme as the recipe does, so a mark in another colour from its
        /// base is held to Chrome's picture rather than SkParagraph's, which
        /// cuts the glyph -- see [`whole_graphemes`].
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
            None,
            &StyleColours::default(),
            false,
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
                    recipe.push_style(
                        style.clone(),
                        flag,
                        &StyleColours::default(),
                    );
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
                recipe.keep_graphemes_whole(Some(&text));
                (recipe.replay(&text, |_, _, _| None), None)
            }
            Painter::Plain => (builder.build(), None),
        };
        paragraph.layout(WIDTH);
        if let Some(sheared) = sheared.as_mut() {
            sheared.lay_out(WIDTH);
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
            started::raster_n32_premul(size).expect("a raster surface");
        let origin = Point::new(MARGIN, MARGIN);
        match &mut sheared {
            Some(sheared) => {
                sheared.paint(&mut paragraph, surface.canvas(), origin);
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
                .filter(|run| run.sheared)
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
                        slot: pieces.len(),
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
    /// moved back, across an edge `moves` says moves, inside its grapheme,
    /// into the piece that now holds the byte before that edge; and no such
    /// edge between two pieces with text that meet falls inside a grapheme.
    fn misplaced(
        text: &str,
        before: &[Piece],
        after: &[Piece],
        moves: impl Fn(&Piece, &Piece) -> bool,
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
                    if !moves(&after[is], &before[was])
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
                && moves(pair[0], pair[1])
                && starts.binary_search(&edge).is_err())
            .then(|| format!("an edge at byte {edge} splits a grapheme"))
        })
    }

    /// What is wrong with where a generated paragraph's bytes end up, or
    /// `None`: each byte stays in its piece or goes to its grapheme's base
    /// across an edge that moves -- see [`misplaced`] -- and the text the
    /// recipe replays is the builder's, byte for byte.
    ///
    /// Twice: with only an oblique side moving an edge, and with every
    /// edge moving, which is what a generated paragraph does, since its
    /// upright styles are all one font.
    fn misplaced_bytes(
        collection: &mut FontCollection,
        generated: &(Vec<Op>, bool),
    ) -> Option<String> {
        let (text, before) = pieces(generated);
        for one_font in [false, true] {
            let (_, mut after) = pieces(generated);
            whole_graphemes(&text, &mut after, |_, _| one_font);
            let moves =
                |a: &Piece, b: &Piece| one_font || a.oblique || b.oblique;
            if let Some(why) = misplaced(&text, &before, &after, moves) {
                return Some(why);
            }
        }

        let mut recipe = ParagraphRecipe::new(
            ParagraphStyle::new(),
            generated.1.then_some(ObliqueAngle::DEFAULT.degrees()),
            collection.clone(),
            None,
            &StyleColours::default(),
            false,
        );
        for op in &generated.0 {
            match *op {
                Op::Push { oblique } => {
                    let (style, oblique) =
                        generated_style(collection, oblique, true);
                    recipe.push_style(style, oblique, &StyleColours::default());
                }
                Op::Pop => recipe.pop(),
                Op::Text(text) => recipe.add_text(text),
                Op::Placeholder => {
                    recipe.add_placeholder(&PlaceholderStyle::default())
                }
            }
        }
        let text = recipe.builder.get_text().to_owned();
        recipe.keep_graphemes_whole(Some(&text));
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

    /// A paragraph of `pieces`, each in its own style, drawn through its
    /// passes -- run by run where `per_line_only`, and otherwise however the
    /// layout chooses -- with whether its glyphs were `Paragraph::paint`'s.
    /// `None` for one with nothing for passes to draw.
    fn drawn_with(
        pieces: &[(&str, TextStyle)],
        width: f32,
        per_line_only: bool,
    ) -> Option<(Vec<u8>, bool)> {
        let collection = raleway();
        let mut base = TextStyle::new();
        base.set_font_families(&[RALEWAY]).set_font_size(SIZE);
        let mut paragraph_style = ParagraphStyle::new();
        paragraph_style.set_text_style(&base);
        let mut recipe = ParagraphRecipe::new(
            paragraph_style,
            None,
            collection,
            None,
            &StyleColours::default(),
            false,
        );
        for (text, style) in pieces {
            recipe.push_style(style.clone(), None, &StyleColours::default());
            recipe.add_text(text);
            recipe.pop();
        }
        let (mut paragraph, passes) = recipe.build();
        let mut passes = passes?;
        passes.per_line_only = per_line_only;
        paragraph.layout(width);
        passes.lay_out(width);
        let (w, h) = (
            (width + 2.0 * MARGIN) as i32,
            (paragraph.height() + 2.0 * MARGIN) as i32,
        );
        let mut surface =
            started::raster_n32_premul((w, h)).expect("a surface");
        surface.canvas().clear(Color::WHITE);
        passes.paint(
            &mut paragraph,
            surface.canvas(),
            Point::new(MARGIN, MARGIN),
        );
        let info = ImageInfo::new_n32_premul((w, h), None);
        let mut pixels = vec![0u8; (w * h * 4) as usize];
        assert!(surface.read_pixels(
            &info,
            &mut pixels,
            (w * 4) as usize,
            (0, 0)
        ));
        Some((pixels, passes.skia_glyphs))
    }

    /// A style in Raleway at the generated size, in `color`, decorated with
    /// `lines` in `style`, and over `background` where there is one.
    fn decorated_style(
        color: Color,
        lines: skia_safe::textlayout::TextDecoration,
        style: skia_safe::textlayout::TextDecorationStyle,
        background: Option<Color>,
    ) -> TextStyle {
        let mut text_style = TextStyle::new();
        text_style
            .set_font_families(&[RALEWAY])
            .set_font_size(SIZE)
            .set_color(color);
        text_style.set_decoration_type(lines);
        text_style.set_decoration_style(style);
        text_style.set_decoration_color(color);
        if let Some(background) = background {
            let mut paint = skia_safe::Paint::default();
            paint.set_color(background);
            text_style.set_background_paint(&paint);
        }
        text_style
    }

    /// Where a paragraph's glyphs are left to `Paragraph::paint`, it draws
    /// byte for byte what it draws run by run: over generated paragraphs of
    /// coloured, decorated and backgrounded pieces, on one line and on
    /// several. Some of them are left to it and some are not, so neither
    /// side of the comparison is empty.
    #[test]
    fn glyphs_left_to_skia_draw_what_the_runs_draw() {
        use skia_safe::textlayout::{TextDecoration, TextDecorationStyle};
        const COLORS: [Color; 3] = [Color::BLACK, Color::RED, Color::BLUE];
        const LINES: [TextDecoration; 5] = [
            TextDecoration::NO_DECORATION,
            TextDecoration::UNDERLINE,
            TextDecoration::LINE_THROUGH,
            TextDecoration::OVERLINE,
            TextDecoration::UNDERLINE.union(TextDecoration::LINE_THROUGH),
        ];
        const STYLES: [TextDecorationStyle; 5] = [
            TextDecorationStyle::Solid,
            TextDecorationStyle::Double,
            TextDecorationStyle::Dotted,
            TextDecorationStyle::Dashed,
            TextDecorationStyle::Wavy,
        ];
        let mut random = SplitMix(SEED);
        let (mut left, mut kept) = (0, 0);
        for case in 0..CASES / 4 {
            let pieces: Vec<(&str, TextStyle)> = (0..1 + random.below(5))
                .map(|_| {
                    let text = PIECES[random.below(PIECES.len())];
                    let background = (random.below(3) == 0)
                        .then(|| Color::from_argb(255, 255, 230, 120));
                    let style = decorated_style(
                        COLORS[random.below(COLORS.len())],
                        LINES[random.below(LINES.len())],
                        STYLES[random.below(STYLES.len())],
                        background,
                    );
                    (text, style)
                })
                .collect();
            // Narrow enough to wrap the longer ones.
            let width = [WIDTH, 60.0][random.below(2)];
            let Some((skia, by_skia)) = drawn_with(&pieces, width, false)
            else {
                continue;
            };
            let (runs, _) =
                drawn_with(&pieces, width, true).expect("the same passes");
            match by_skia {
                true => left += 1,
                false => kept += 1,
            }
            let texts: Vec<&str> =
                pieces.iter().map(|(text, _)| *text).collect();
            assert!(skia == runs, "case {case}: {texts:?} at {width}");
        }
        assert!(left > 0 && kept > 0, "{left} left to Skia, {kept} not");
    }

    /// A second line's overline lies over the descenders of the line above
    /// it, which only drawing a line at a time puts there: such a paragraph
    /// keeps its glyphs here, as one with only backgrounds over several
    /// lines, or a decoration on one, does not.
    #[test]
    fn an_overline_on_a_second_line_keeps_its_glyphs_here() {
        use skia_safe::textlayout::{TextDecoration, TextDecorationStyle};
        let lines = |decoration, background| {
            decorated_style(
                Color::BLACK,
                decoration,
                TextDecorationStyle::Solid,
                background,
            )
        };
        let wrapped = "gyp jqy pqg";
        let overlined = [(wrapped, lines(TextDecoration::OVERLINE, None))];
        let by_skia = |pieces: &[(&str, TextStyle)], width| {
            drawn_with(pieces, width, false).expect("passes").1
        };
        assert!(!by_skia(&overlined, 60.0), "an overline over two lines");
        assert!(by_skia(&overlined, WIDTH * 4.0), "an overline on one line");
        let backed = [(
            wrapped,
            lines(TextDecoration::NO_DECORATION, Some(Color::YELLOW)),
        )];
        assert!(by_skia(&backed, 60.0), "a background over two lines");

        // And those it keeps here for a reason of their own: an fi ligature
        // in two colours, which SkParagraph cuts and this draws whole, and a
        // text shadow, which goes beneath every glyph. Each draws as it
        // does run by run.
        let underlined = |color| {
            decorated_style(
                color,
                TextDecoration::UNDERLINE,
                TextDecorationStyle::Solid,
                None,
            )
        };
        let mut shadowed = decorated_style(
            Color::BLACK,
            TextDecoration::NO_DECORATION,
            TextDecorationStyle::Solid,
            None,
        );
        shadowed.add_shadow(skia_safe::textlayout::TextShadow::new(
            Color::RED,
            (3.0, 3.0),
            0.0,
        ));
        for (what, pieces) in [
            (
                "an fi ligature in two colours",
                vec![
                    ("f", underlined(Color::RED)),
                    ("i", underlined(Color::BLUE)),
                ],
            ),
            ("a text shadow", vec![("ab", shadowed)]),
        ] {
            let (skia, by_skia) =
                drawn_with(&pieces, WIDTH, false).expect("passes");
            let (runs, _) = drawn_with(&pieces, WIDTH, true).expect("passes");
            assert!(!by_skia, "{what}");
            assert!(skia == runs, "{what}");
        }
    }

    /// A change made to a paragraph's own style -- see [`GIVEN`].
    type Change = fn(&mut SpanStyle);

    /// The crate engine's paragraph whose own style is `base` with each
    /// change in `changes` made to it, a span pushed after it: whether
    /// [`ParagraphRecipe::new`] copied the paragraph style, and the pixels
    /// it draws on a canvas, with every style copied where `copied`.
    fn built_from(changes: Change, copied: bool) -> (bool, Vec<u8>) {
        use crate::{canvas::Canvas, font::FontLibrary, text::TextEngine};
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            FontLibrary::new()
                .register_font_from_path(
                    "GivenOswald",
                    "tests/assets/fonts/Oswald/Oswald-VariableFont_wght.ttf",
                )
                .expect("Oswald registers");
        });
        let mut base = SpanStyle {
            font_families: vec!["GivenOswald".into()],
            font_size: 28.0,
            color: RgbaLinear::from_srgb8(0x20, 0x30, 0x40, 1.0),
            ..SpanStyle::default()
        };
        changes(&mut base);
        let engine = TextEngine::new(&FontLibrary::new());
        super::COPY_ALWAYS.set(copied);
        let before = super::style_copies();
        let mut builder = engine.paragraph_builder(&base);
        let copies = super::style_copies() - before;
        super::COPY_ALWAYS.set(false);
        builder.add_text("Hamburgefonstiv ");
        builder.push_style(&SpanStyle {
            color: RgbaLinear::from_srgb8(200, 0, 0, 1.0),
            ..base.clone()
        });
        builder.add_text("quick brown fox jumps");
        builder.pop();
        let paragraph = builder.build(260.0);
        let mut canvas = Canvas::new(300.0, 160.0);
        let ctx = canvas.context();
        ctx.draw_paragraph(&paragraph, 7.6, 10.3);
        let pixels = ctx
            .get_image_data(0.0, 0.0, 300.0, 160.0)
            .expect("the canvas reads back")
            .pixels()
            .to_vec();
        assert!(copies <= 1, "one paragraph style, copied {copies} times");
        (copies == 1, pixels)
    }

    /// A paragraph style's own text style asking for each thing the recipe
    /// may have to do to it, and whether it has anything to take off.
    const GIVEN: [(&str, Change, bool); 6] = [
        ("plain", |_| {}, false),
        (
            "oblique",
            |style| style.slant = TextSlant::Oblique(ObliqueAngle::DEFAULT),
            false,
        ),
        ("half leading", |style| style.half_leading = true, false),
        (
            "underlined",
            |style| style.decoration.underline = true,
            true,
        ),
        (
            "shadowed",
            |style| {
                style.shadows = vec![SpanShadow {
                    color: RgbaLinear::from_srgb8(0, 0, 200, 0.5),
                    offset_x: 2.0,
                    offset_y: 3.0,
                    blur_sigma: 1.5,
                }]
            },
            true,
        ),
        (
            "backgrounded",
            |style| {
                style.background_color =
                    Some(RgbaLinear::from_srgb8(250, 230, 0, 1.0))
            },
            true,
        ),
    ];

    /// The engine hands the recipe a hinted paragraph style, so one with
    /// nothing to take off is built from as it stands and only one with a
    /// decoration, a shadow or a background is copied: the copy took about
    /// 4.5 per cent of a two-span build's samples.
    #[test]
    fn only_a_style_with_something_to_take_off_is_copied() {
        for (what, changes, taken) in GIVEN {
            let (copied, _) = built_from(changes, false);
            assert_eq!(copied, taken, "{what}");
        }
    }

    /// A paragraph style built from as it stands draws byte for byte what
    /// its copy draws.
    #[test]
    fn a_style_built_from_as_it_stands_draws_as_its_copy() {
        for (what, changes, _) in GIVEN {
            let (_, given) = built_from(changes, false);
            let (copied, copy) = built_from(changes, true);
            assert!(copied, "{what}: the reference is a copy");
            assert!(given == copy, "{what}");
        }
    }

    /// Skia's own text style is hinted as a paragraph is drawn, so a style
    /// either caller builds from a new one needs no copy for its hinting --
    /// see [`ParagraphRecipe::new`].
    #[test]
    fn a_new_text_style_is_hinted_as_a_paragraph_is_drawn() {
        assert_eq!(TextStyle::new().font_hinting(), text_hinting(true));
    }

    /// A paragraph style handed in unhinted, as neither caller hands one,
    /// is copied and drawn hinted, its own text's runs with the rest.
    #[test]
    fn a_paragraph_style_handed_in_unhinted_is_drawn_hinted() {
        let mut base = TextStyle::new();
        base.set_font_families(&[RALEWAY])
            .set_font_size(24.0)
            .set_font_hinting(FontHinting::None);
        let mut paragraph_style = ParagraphStyle::new();
        paragraph_style.set_text_style(&base);
        let before = super::style_copies();
        let mut recipe = ParagraphRecipe::new(
            paragraph_style,
            None,
            raleway(),
            None,
            &StyleColours::default(),
            false,
        );
        assert_eq!(super::style_copies() - before, 1, "copied");
        recipe.add_text("Hamburgefonstiv");
        let (mut paragraph, _) = recipe.build();
        paragraph.layout(1000.0);
        let hinting: Vec<_> = glyph_runs(&mut paragraph)
            .iter()
            .map(|run| run.font.hinting())
            .collect();
        assert!(!hinting.is_empty(), "no runs");
        assert!(
            hinting.iter().all(|h| *h == text_hinting(true)),
            "{hinting:?}"
        );
    }

    /// A style in Raleway at the generated size, in `color`.
    fn colored(color: Color) -> TextStyle {
        let mut style = TextStyle::new();
        style
            .set_font_families(&[RALEWAY])
            .set_font_size(SIZE)
            .set_color(color);
        style
    }

    /// Whether `GraphemeCursor` puts a boundary between `a` and `b`.
    fn cursor_boundary(a: char, b: char) -> bool {
        let text = format!("{a}{b}");
        let at = a.len_utf8();
        unicode_segmentation::GraphemeCursor::new(at, text.len(), true)
            .is_boundary(&text, 0)
            == Ok(true)
    }

    /// Where the two bytes either side of a point say it is a boundary, it
    /// is one: every pair of ASCII characters, the only pairs they vouch
    /// for, against `GraphemeCursor`. And every character in Unicode that
    /// joins an ASCII letter after it into one cluster -- the Prepend
    /// characters, found by scanning rather than listed -- is one they do
    /// not vouch for, as CR is before LF.
    #[test]
    fn two_ascii_bytes_say_where_a_cluster_ends() {
        for a in (0u8..0x80).map(char::from) {
            for b in (0u8..0x80).map(char::from) {
                if ascii_boundary(&format!("{a}{b}"), 1) {
                    assert!(cursor_boundary(a, b), "{a:?} {b:?}");
                }
            }
        }
        assert!(!cursor_boundary('\r', '\n'), "CR LF is one cluster");
        assert!(!ascii_boundary("\r\n", 1), "and not vouched for");
        let joining: Vec<char> = (0..=0x10FFFF)
            .filter_map(char::from_u32)
            .filter(|&c| !cursor_boundary(c, 'a'))
            .collect();
        assert!(joining.contains(&'\u{600}'), "Prepend joins: {joining:?}");
        for c in joining {
            let text = format!("{c}a");
            assert!(!ascii_boundary(&text, c.len_utf8()), "{c:?}");
        }
    }

    /// The edge between two pieces in different paints of one font, as
    /// each piece's first bytes and slots end up: a Prepend character or a
    /// CR before an ASCII piece is one cluster with its first character,
    /// which goes with it, whatever the byte pair would say.
    #[test]
    fn a_cluster_across_an_ascii_edge_is_kept_whole() {
        for (left, right, slots) in [
            ("\u{600}", "12", vec![(0..3, 1), (3..4, 2)]),
            ("\u{110BD}", "ab", vec![(0..5, 1), (5..6, 2)]),
            ("a\r", "\nb", vec![(0..3, 1), (3..4, 2)]),
            ("e", "\u{301}x", vec![(0..3, 1), (3..4, 2)]),
            ("ab", "cd", vec![(0..2, 1), (2..4, 2)]),
        ] {
            let mut base = TextStyle::new();
            base.set_font_families(&[RALEWAY]).set_font_size(SIZE);
            let mut paragraph_style = ParagraphStyle::new();
            paragraph_style.set_text_style(&base);
            let mut recipe = ParagraphRecipe::new(
                paragraph_style,
                None,
                raleway(),
                None,
                &StyleColours::default(),
                false,
            );
            for (text, color) in [(left, Color::RED), (right, Color::BLUE)] {
                recipe.push_style(
                    colored(color),
                    None,
                    &StyleColours::default(),
                );
                recipe.add_text(text);
                recipe.pop();
            }
            let (_, passes) = recipe.build();
            let passes = passes.expect("a colour edge between one font's");
            assert_eq!(passes.slots, slots, "{left:?} {right:?}");
        }
    }

    /// What the edge tests' paragraphs are made of: text ASCII at both
    /// ends, at one end only, CR and LF apart, a ligature, and empty text.
    const EDGE_PIECES: &[&str] = &[
        "ab", "cd", " ", "fi", "mn op", "x\r", "\n", "\r", "\u{e9}",
        "e\u{301}", "\u{301}x", "",
    ];

    /// A step of an edge test's paragraph: a style pushed, by its index in
    /// [`edge_styles`], a pop, text, or a placeholder.
    #[derive(Debug, Clone, Copy)]
    enum EdgeOp {
        Push(usize),
        Pop,
        Text(&'static str),
        Placeholder,
    }

    /// The styles an edge test pushes: the paragraph's own and two other
    /// colours of it, each shaped as one with it, a larger size, which is
    /// shaped apart, and a decoration, which is drawn by the passes.
    fn edge_styles() -> Vec<TextStyle> {
        use skia_safe::textlayout::{TextDecoration, TextDecorationStyle};
        let sized = |color: Color, size: f32| {
            let mut style = TextStyle::new();
            style
                .set_font_families(&[RALEWAY])
                .set_font_size(size)
                .set_color(color);
            style
        };
        vec![
            sized(Color::BLACK, SIZE),
            sized(Color::RED, SIZE),
            sized(Color::BLUE, SIZE),
            sized(Color::BLACK, SIZE * 1.5),
            decorated_style(
                Color::RED,
                TextDecoration::UNDERLINE,
                TextDecorationStyle::Solid,
                None,
            ),
        ]
    }

    fn edge_ops(random: &mut SplitMix) -> Vec<EdgeOp> {
        (0..1 + random.below(10))
            .map(|_| match random.below(12) {
                0..=5 => {
                    EdgeOp::Text(EDGE_PIECES[random.below(EDGE_PIECES.len())])
                }
                6..=8 => EdgeOp::Push(random.below(edge_styles().len())),
                9..=10 => EdgeOp::Pop,
                _ => EdgeOp::Placeholder,
            })
            .collect()
    }

    /// The recipe `ops` make, over the paragraph style of
    /// [`edge_styles`]'s first.
    fn edge_recipe(ops: &[EdgeOp]) -> ParagraphRecipe {
        let styles = edge_styles();
        let mut paragraph_style = ParagraphStyle::new();
        paragraph_style.set_text_style(&styles[0]);
        let mut recipe = ParagraphRecipe::new(
            paragraph_style,
            None,
            raleway(),
            None,
            &StyleColours::default(),
            false,
        );
        let placeholder = PlaceholderStyle::new(
            SIZE * 0.75,
            SIZE * 0.75,
            PlaceholderAlignment::Baseline,
            TextBaseline::Alphabetic,
            0.0,
        );
        for op in ops {
            match *op {
                EdgeOp::Push(style) => recipe.push_style(
                    styles[style].clone(),
                    None,
                    &StyleColours::default(),
                ),
                EdgeOp::Pop => recipe.pop(),
                EdgeOp::Text(text) => recipe.add_text(text),
                EdgeOp::Placeholder => recipe.add_placeholder(&placeholder),
            }
        }
        recipe
    }

    /// A recipe's text steps, as the bytes each covers.
    fn text_steps(recipe: &ParagraphRecipe) -> Vec<Range<usize>> {
        recipe
            .steps
            .iter()
            .filter_map(|step| match step {
                Step::Text(bytes) => Some(bytes.clone()),
                _ => None,
            })
            .collect()
    }

    /// Whether every text step starts at an ASCII boundary is judged as the
    /// text is added exactly as `ascii_boundary` judges it from the text
    /// read back: over generated paragraphs whose edges are ASCII on both
    /// sides, on one side only, CR then LF, beside placeholders and beside
    /// empty text. Both answers come up, so neither side is untested.
    #[test]
    fn edges_judged_as_added_are_edges_read_back() {
        let mut random = SplitMix(SEED);
        let mut answers = [0; 2];
        for _ in 0..CASES {
            let ops = edge_ops(&mut random);
            let mut recipe = edge_recipe(&ops);
            let text = recipe.builder.get_text().to_owned();
            let read = text_steps(&recipe)
                .iter()
                .all(|bytes| ascii_boundary(&text, bytes.start));
            assert_eq!(recipe.ascii_edges, read, "{ops:?}");
            answers[usize::from(read)] += 1;
        }
        assert!(answers.iter().all(|&n| n > CASES / 10), "{answers:?}");
    }

    /// Where every edge is ASCII, the walk of the steps finds the edges the
    /// full pass finds, the full pass moves nothing, and the paragraph
    /// draws byte for byte as with the text read for every edge: over
    /// generated paragraphs in colours shaped as one, a size shaped apart,
    /// a decoration and placeholders. Some have edges and some do not.
    #[test]
    fn the_walk_finds_the_edges_the_full_pass_finds() {
        let mut random = SplitMix(SEED);
        let (mut walked, mut with_edges) = (0, 0);
        for _ in 0..CASES {
            let ops = edge_ops(&mut random);
            let mut recipe = edge_recipe(&ops);
            if !recipe.ascii_edges {
                continue;
            }
            let text = recipe.builder.get_text().to_owned();
            let walk = recipe.edges_between_clusters();
            let steps = text_steps(&recipe);
            let (moved, full) = recipe.keep_graphemes_whole(Some(&text));
            assert!(!moved, "{ops:?}");
            assert_eq!(walk, full, "{ops:?}");
            assert_eq!(text_steps(&recipe), steps, "{ops:?}");
            // Drawn where there are edges, and in a quarter of the rest,
            // since a debug build draws slowly.
            if !walk.is_empty() || walked % 4 == 0 {
                FULL_GRAPHEMES.set(true);
                let read = edge_painted(&ops);
                FULL_GRAPHEMES.set(false);
                assert!(read == edge_painted(&ops), "{ops:?}");
            }
            walked += 1;
            with_edges += usize::from(!walk.is_empty());
        }
        assert!(walked > CASES / 4, "{walked} walked");
        assert!(with_edges > CASES / 40, "{with_edges} with edges");
    }

    /// The paragraph `ops` make, laid out and painted, its passes' where it
    /// has them.
    fn edge_painted(ops: &[EdgeOp]) -> Vec<u8> {
        let (mut paragraph, passes) = edge_recipe(ops).build();
        paragraph.layout(WIDTH);
        let (w, h) = (
            (WIDTH + 2.0 * MARGIN) as i32,
            (paragraph.height() + 2.0 * MARGIN) as i32,
        );
        let mut surface =
            started::raster_n32_premul((w, h)).expect("a surface");
        surface.canvas().clear(Color::WHITE);
        let origin = Point::new(MARGIN, MARGIN);
        match passes {
            Some(mut passes) => {
                passes.lay_out(WIDTH);
                passes.paint(&mut paragraph, surface.canvas(), origin);
            }
            None => paragraph.paint(surface.canvas(), origin),
        }
        let info = ImageInfo::new_n32_premul((w, h), None);
        let mut pixels = vec![0u8; (w * h * 4) as usize];
        assert!(surface.read_pixels(
            &info,
            &mut pixels,
            (w * 4) as usize,
            (0, 0)
        ));
        pixels
    }

    /// A paragraph of `styles` pushed styles, each over a little text.
    fn styled(styles: usize) -> Vec<EdgeOp> {
        (0..styles)
            .flat_map(|_| [EdgeOp::Push(1), EdgeOp::Text("ab")])
            .collect()
    }

    /// A recipe starts with the lists the one before it on the thread
    /// handed back, whether that one was built or dropped unbuilt, so a
    /// caller building paragraph after paragraph allocates neither.
    #[test]
    fn a_recipe_takes_the_lists_the_last_one_left() {
        let ops = styled(20);
        drop(edge_recipe(&ops).build());
        let next = edge_recipe(&[]);
        assert!(next.steps.capacity() >= 40, "{}", next.steps.capacity());
        assert!(next.pushed.capacity() >= 20, "{}", next.pushed.capacity());
        drop(next);
        drop(edge_recipe(&styled(30)));
        let next = edge_recipe(&[]);
        assert!(
            next.pushed.capacity() >= 30,
            "kept from one dropped unbuilt"
        );
        assert!(next.steps.is_empty() && next.pushed.is_empty());
    }

    /// A list longer than the cap is not handed back, so one paragraph of
    /// many styles leaves its capacity behind on no thread.
    #[test]
    fn a_recipe_keeps_no_list_past_the_cap() {
        drop(edge_recipe(&styled(super::KEPT_LIST_CAPACITY + 1)).build());
        let next = edge_recipe(&[]);
        assert!(next.steps.capacity() <= super::KEPT_LIST_CAPACITY);
        assert!(next.pushed.capacity() <= super::KEPT_LIST_CAPACITY);
    }

    /// What is taken off a style is a few words, its decoration and its
    /// background each behind a pointer: every recipe moves one for its own
    /// style and keeps one for each style pushed, and most styles have
    /// neither, so their size was copied with every move of a builder for
    /// nothing.
    #[test]
    fn what_is_taken_off_a_style_is_a_few_words() {
        let words =
            std::mem::size_of::<super::Taken>() / std::mem::size_of::<usize>();
        assert!(words <= 5, "{words} words");
    }

    /// A paragraph [`super::Passes`] has nothing to draw for is built
    /// without its decorations collected: one style, a second that is
    /// never shaped with it, a second in the same paint, and text in the
    /// paragraph's own style either side of a push. A decorated style and a
    /// colour change between two styles shaped as one are collected, so the
    /// count can move.
    #[test]
    fn a_paragraph_with_nothing_to_draw_collects_no_decorations() {
        let made = |ops: &[EdgeOp]| {
            let before = BOXES_MADE.get();
            let (_, passes) = edge_recipe(ops).build();
            (BOXES_MADE.get() - before, passes.is_some())
        };
        for ops in [
            &[EdgeOp::Text("Hamburgefonstiv")][..],
            &[
                EdgeOp::Text("Hamburgefonstiv "),
                EdgeOp::Push(3),
                EdgeOp::Text("quick"),
                EdgeOp::Pop,
            ],
            &[
                EdgeOp::Text("Hamburgefonstiv "),
                EdgeOp::Push(0),
                EdgeOp::Text("quick"),
                EdgeOp::Pop,
                EdgeOp::Text(" brown"),
            ],
        ] {
            assert_eq!(made(ops), (0, false), "{} ops", ops.len());
        }
        for ops in [
            &[
                EdgeOp::Text("Hamburgefonstiv "),
                EdgeOp::Push(4),
                EdgeOp::Text("quick"),
            ][..],
            &[
                EdgeOp::Text("Hamburgefonstiv "),
                EdgeOp::Push(1),
                EdgeOp::Text("quick"),
            ],
        ] {
            assert_eq!(made(ops), (1, true), "{} ops", ops.len());
        }
    }

    /// A paragraph in two styles whose edges are all ASCII is built without
    /// its text read back.
    #[test]
    fn an_ascii_paragraph_is_built_without_its_text_read() {
        let before = TEXT_READS.get();
        edge_recipe(&[
            EdgeOp::Text("Hamburgefonstiv "),
            EdgeOp::Push(1),
            EdgeOp::Text("quick brown"),
            EdgeOp::Pop,
        ])
        .build();
        assert_eq!(TEXT_READS.get() - before, 0);
    }

    /// The byte-pair shortcut changes nothing a paragraph draws: generated
    /// paragraphs, Prepend characters, CR LF and marks among their pieces,
    /// draw byte for byte as with every edge read from the text.
    #[test]
    fn reading_fewer_edges_draws_the_same() {
        let extra = ["\u{600}", "1", "\r", "\n", "\u{605}x", "x\r"];
        let all: Vec<&str> = PIECES.iter().copied().chain(extra).collect();
        let colors = [Color::BLACK, Color::RED, Color::BLUE];
        let mut random = SplitMix(SEED);
        let mut drawn = 0;
        for _ in 0..CASES / 4 {
            let pieces: Vec<(&str, TextStyle)> = (0..2 + random.below(4))
                .map(|_| {
                    let text = all[random.below(all.len())];
                    (text, colored(colors[random.below(colors.len())]))
                })
                .collect();
            let width = [WIDTH, 60.0][random.below(2)];
            FULL_GRAPHEMES.set(true);
            let full = drawn_with(&pieces, width, true);
            FULL_GRAPHEMES.set(false);
            let short = drawn_with(&pieces, width, true);
            let texts: Vec<&str> = pieces.iter().map(|piece| piece.0).collect();
            assert!(full == short, "{texts:?}");
            drawn += usize::from(full.is_some());
        }
        assert!(drawn > CASES / 16, "{drawn} cases drawn by passes");
    }

    /// The crate engine's paragraph in Oswald, its middle span in its own
    /// style changed by `change`, laid out at `width`.
    fn lazily_drawn(change: Change, width: f32) -> crate::text::Paragraph {
        use crate::{font::FontLibrary, text::TextEngine};
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            FontLibrary::new()
                .register_font_from_path(
                    "LazyOswald",
                    "tests/assets/fonts/Oswald/Oswald-VariableFont_wght.ttf",
                )
                .expect("Oswald registers");
        });
        let base = SpanStyle {
            font_families: vec!["LazyOswald".into()],
            font_size: 24.0,
            color: RgbaLinear::from_srgb8(0x20, 0x30, 0x40, 1.0),
            ..SpanStyle::default()
        };
        let mut middle = base.clone();
        change(&mut middle);
        let engine = TextEngine::new(&FontLibrary::new());
        let mut builder = engine.paragraph_builder(&base);
        builder.add_text("Hamburgefonstiv quick ");
        builder.push_style(&middle);
        builder.add_text("brown fox jumps over the lazy dog while office ");
        builder.pop();
        builder.add_text("staff fluff fifty ruffled waffles.");
        builder.build(width)
    }

    /// The pixels `paragraph` draws on a canvas of its own.
    fn lazy_pixels(paragraph: &crate::text::Paragraph) -> Vec<u8> {
        use crate::canvas::Canvas;
        let mut canvas = Canvas::new(420.0, 200.0);
        let ctx = canvas.context();
        ctx.draw_paragraph(paragraph, 7.6, 10.3);
        ctx.get_image_data(0.0, 0.0, 420.0, 200.0)
            .expect("the canvas reads back")
            .pixels()
            .to_vec()
    }

    /// A paragraph's layout is followed for drawing at the first paint after
    /// it, and only then: one laid out and measured, however often, is
    /// followed never, and one painted again is not followed again.
    #[test]
    fn a_layout_is_followed_at_its_first_paint_only() {
        let mut drawn = 0;
        for (what, change, _) in GIVEN {
            let mut paragraph = lazily_drawn(change, 400.0);
            if paragraph.follows().is_none() {
                continue;
            }
            drawn += 1;
            assert_eq!(paragraph.follows(), Some(0), "{what}: built");
            paragraph.layout(300.0);
            let _ = (paragraph.height(), paragraph.width());
            let _ = (paragraph.line_metrics(), paragraph.line_count());
            let _ = paragraph.glyph_position_at_coordinate(40.0, 10.0);
            assert_eq!(paragraph.follows(), Some(0), "{what}: laid out");
            lazy_pixels(&paragraph);
            assert_eq!(paragraph.follows(), Some(1), "{what}: painted");
            lazy_pixels(&paragraph);
            assert_eq!(paragraph.follows(), Some(1), "{what}: painted again");
            paragraph.layout(160.0);
            assert_eq!(paragraph.follows(), Some(1), "{what}: laid out again");
            lazy_pixels(&paragraph);
            assert_eq!(paragraph.follows(), Some(2), "{what}: then painted");
        }
        assert!(drawn >= 4, "{drawn} kinds drawn by passes");
    }

    /// A paragraph painted, laid out again at another width and painted
    /// again draws byte for byte what one built at that width draws: what
    /// the first layout drew is not drawn for the second.
    #[test]
    fn a_paragraph_laid_out_again_draws_its_new_layout() {
        let mut drawn = 0;
        for (what, change, _) in GIVEN {
            let mut paragraph = lazily_drawn(change, 400.0);
            drawn += usize::from(paragraph.follows().is_some());
            let wide = lazy_pixels(&paragraph);
            paragraph.layout(160.0);
            let rewrapped = lazy_pixels(&paragraph);
            let fresh = lazy_pixels(&lazily_drawn(change, 160.0));
            assert!(wide != fresh, "{what}: the widths draw alike");
            assert!(rewrapped == fresh, "{what}");
        }
        assert!(drawn >= 4, "{drawn} kinds drawn by passes");
    }

    /// Every paragraph is built through a recipe, so one with no style
    /// pushed holds no list for pushed styles, and one style pushed is one
    /// entry in one list -- not one in each of a list per thing kept of it.
    #[test]
    fn a_recipe_holds_no_list_for_styles_until_one_is_pushed() {
        let mut style = TextStyle::new();
        style.set_font_families(&[RALEWAY]).set_font_size(SIZE);
        let mut paragraph_style = ParagraphStyle::new();
        paragraph_style.set_text_style(&style);
        let mut recipe = ParagraphRecipe::new(
            paragraph_style,
            None,
            raleway(),
            None,
            &StyleColours::default(),
            false,
        );
        recipe.add_text("Hamburgefonstiv ");
        assert_eq!(recipe.pushed.capacity(), 0, "nothing pushed");
        recipe.push_style(style, None, &StyleColours::default());
        recipe.add_text("quick");
        recipe.pop();
        assert_eq!(recipe.pushed.len(), 1, "one style pushed");
    }
}
