//! Paragraphs whose `oblique` ranges are drawn as CSS Fonts 4 matches them.
//!
//! A range asking for oblique is laid out in the family's face that leans as
//! asked where it has one, and otherwise upright and sheared by
//! `tan(14deg)`, the default angle -- the rule a canvas `oblique` follows,
//! `run_shear` in `node::typography`. SkParagraph cannot shear a range of
//! its own, and a paragraph is painted by `Paragraph::paint`, which carries
//! each range's colours, shadows and decorations, so a paragraph with an
//! oblique range is built twice from the steps that made it:
//!
//! - once with the oblique ranges' glyphs and shadows transparent and their
//!   decorations off, which paints everything else, backgrounds included, as it
//!   always did;
//! - and, where an oblique range is decorated, once more with every glyph
//!   transparent and only the oblique ranges' decorations on, which paints
//!   those lines after the glyphs they cross.
//!
//! Between the two the oblique ranges' shadows and glyphs are drawn a run
//! at a time, each under its own shear, with the paints SkParagraph would
//! have used. A paragraph with no oblique range is built once, as it is
//! filled in, and painted by `Paragraph::paint` alone.
//!
//! SkParagraph paints a line in passes -- backgrounds, shadows, glyphs,
//! decorations -- and the oblique ranges' shadows and glyphs come after the
//! rest of the paragraph's glyphs and decorations rather than with them. Two
//! ranges that overlap one another can therefore stack in a different
//! order; ranges side by side paint as they would.

use crate::{
    context2d::DEFAULT_OBLIQUE_DEGREES,
    font::leans_as_asked,
    node::typography::{
        Shear, clusters_under, painted_positions, run_shear, sheared_blob,
    },
};
use skia_safe::{
    BlurStyle, Canvas, Color, Font, FontStyle, GlyphId, MaskFilter, Paint,
    Point,
    font_style::Slant,
    textlayout::{
        FontCollection, Paragraph, ParagraphBuilder, ParagraphStyle,
        PlaceholderStyle, TextDecoration, TextShadow, TextStyle,
    },
};
use std::{borrow::Cow, ops::Range};

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
        let ranges = match self.oblique {
            true => self.oblique_ranges(),
            false => Vec::new(),
        };
        if ranges.is_empty() {
            return (self.builder.build(), None);
        }
        let text = self.builder.get_text().to_owned();
        let paragraph = self.replay(&text, |style, oblique| {
            oblique.then(|| {
                let mut style = style.clone();
                style
                    .set_foreground_paint(&transparent())
                    .reset_shadows()
                    .set_decoration_type(TextDecoration::NO_DECORATION);
                style
            })
        });
        let decorations =
            ranges.iter().any(|range| range.decorated).then(|| {
                self.replay(&text, |style, oblique| {
                    let mut style = style.clone();
                    style
                        .set_foreground_paint(&transparent())
                        .clear_background_color()
                        .reset_shadows();
                    if !oblique {
                        style
                            .set_decoration_type(TextDecoration::NO_DECORATION);
                    }
                    Some(style)
                })
            });
        let sheared = Sheared {
            ranges,
            decorations,
            runs: Vec::new(),
        };
        (paragraph, Some(sheared))
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
    decorated: bool,
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
            decorated: style.decoration_type() != TextDecoration::NO_DECORATION,
        }
    }
}

/// What draws a paragraph's oblique ranges: their glyph runs, found again at
/// each layout, and the paragraph their decorations come from.
pub(crate) struct Sheared {
    ranges: Vec<ObliqueRange>,
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
    /// A glyph's text is found by asking the paragraph for the cluster
    /// under it -- see `clusters_under`.
    pub(crate) fn lay_out(&mut self, paragraph: &mut Paragraph, width: f32) {
        if let Some(decorations) = &mut self.decorations {
            decorations.layout(width);
        }
        let mut visited = Vec::new();
        paragraph.extended_visit(|_, visit| {
            if let Some(info) = visit {
                let positions = painted_positions(
                    info.font(),
                    info.glyphs(),
                    info.positions(),
                    info.advance().width,
                )
                .unwrap_or_else(|| info.positions().to_vec());
                visited.push((
                    info.font().clone(),
                    info.glyphs().to_vec(),
                    positions,
                    info.origin(),
                    info.advance().width,
                ));
            }
        });
        self.runs = visited
            .into_iter()
            .flat_map(|(font, glyphs, positions, origin, advance)| {
                let ranges: Vec<Option<usize>> =
                    clusters_under(paragraph, origin, &positions, advance)
                        .into_iter()
                        .map(|cluster| self.range_of(cluster?.start))
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
                    .filter_map(|(at, range)| {
                        Some(ShearedRun {
                            font: font.clone(),
                            glyphs: glyphs[at.clone()].to_vec(),
                            positions: positions[at].to_vec(),
                            origin,
                            range: range?,
                            shear,
                        })
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
        paragraph.paint(canvas, origin);
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
    use skia_safe::{
        Data, FontMgr,
        textlayout::{
            FontCollection, Paragraph, ParagraphBuilder, ParagraphStyle,
            TextStyle, TypefaceFontProvider,
        },
    };

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

    /// The second defect `clusters_under` works around, measured directly:
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

    /// The defect `Sheared::lay_out` works around, measured directly: a run
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
