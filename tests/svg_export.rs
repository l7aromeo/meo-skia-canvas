//! An SVG export is a well-formed document holding everything the page
//! draws: every output here is parsed as XML before anything is asked of it.

use meo_skia_canvas::prelude::*;
use quick_xml::{Reader, events::Event};

/// The page every case draws on.
const WIDTH: f32 = 80.0;
const HEIGHT: f32 = 40.0;

/// The elements of a parsed document, each with its attributes, in order.
struct Document {
    elements: Vec<(String, Vec<(String, String)>)>,
}

impl Document {
    /// The first element named `name`.
    fn first(&self, name: &str) -> Option<&[(String, String)]> {
        self.elements
            .iter()
            .find(|(element, _)| element == name)
            .map(|(_, attributes)| attributes.as_slice())
    }
}

/// `svg` parsed as XML: every start, end and empty tag with its attributes
/// checked, and every element closed in order. Panics, naming the fault,
/// where it is not well-formed.
fn parse(svg: &[u8]) -> Document {
    let text = String::from_utf8_lossy(svg);
    let mut reader = Reader::from_str(&text);
    reader.config_mut().check_end_names = true;
    let mut elements = Vec::new();
    let mut depth = 0usize;
    loop {
        let event = reader
            .read_event()
            .unwrap_or_else(|e| panic!("not well-formed XML ({e}): {text}"));
        match event {
            Event::Start(ref tag) | Event::Empty(ref tag) => {
                if matches!(event, Event::Start(_)) {
                    depth += 1;
                }
                let attributes = tag
                    .attributes()
                    .with_checks(true)
                    .map(|attribute| {
                        let attribute = attribute.unwrap_or_else(|e| {
                            panic!("a malformed attribute ({e}): {text}")
                        });
                        (
                            attribute.key.into_inner().to_owned(),
                            attribute.value.to_string(),
                        )
                    })
                    .collect();
                elements.push((tag.name().into_inner().to_owned(), attributes));
            }
            Event::End(_) => depth -= 1,
            Event::Eof => break,
            _ => {}
        }
    }
    assert_eq!(depth, 0, "an element left open: {text}");
    Document { elements }
}

fn svg_of(canvas: &mut Canvas) -> Vec<u8> {
    canvas
        .to_buffer(ImageFormat::Svg, &EncodeOptions::default())
        .expect("an SVG")
}

/// A page with nothing on it exports a well-formed document whose root
/// carries the page's `viewBox`. Skia writes an empty document's root
/// self-closed, and the `viewBox` went in after its `/`, which no XML
/// parser reads.
#[test]
fn an_empty_page_exports_a_well_formed_svg() {
    let mut canvas = Canvas::new(WIDTH, HEIGHT);
    let document = parse(&svg_of(&mut canvas));
    let root = document.first("svg").expect("an svg root");
    let view_box = root
        .iter()
        .find(|(key, _)| key == "viewBox")
        .map(|(_, value)| value.as_str());
    assert_eq!(view_box, Some("0 0 80 40"));
}

/// A page with something on it keeps its `viewBox` too, on a root that is
/// opened and closed.
#[test]
fn a_drawn_page_exports_its_view_box() {
    let mut canvas = Canvas::new(WIDTH, HEIGHT);
    canvas.context().fill_rect(10.0, 10.0, 20.0, 10.0);
    let document = parse(&svg_of(&mut canvas));
    let root = document.first("svg").expect("an svg root");
    assert!(
        root.iter()
            .any(|(key, value)| key == "viewBox" && value == "0 0 80 40")
    );
    assert!(document.first("path").is_some(), "the rectangle is drawn");
}

/// The number of elements named `name`.
fn count(document: &Document, name: &str) -> usize {
    document
        .elements
        .iter()
        .filter(|(element, _)| element == name)
        .count()
}

/// Whether `pdf` embeds an image, which is how it holds what it rasterizes.
fn embeds_an_image(pdf: &[u8]) -> bool {
    pdf.windows(b"/Subtype /Image".len())
        .any(|w| w == b"/Subtype /Image")
}

/// A paragraph of one style, with `shadows`.
fn paragraph(shadows: Vec<TextShadow>) -> Paragraph {
    let library = FontLibrary::new();
    let engine = TextEngine::new(&library);
    let style = TextStyle {
        font_families: vec!["sans-serif".into()],
        font_size: 20.0,
        shadows,
        ..TextStyle::default()
    };
    let mut builder = engine.paragraph_builder(&style);
    builder.add_text("Para");
    builder.build(200.0)
}

fn shadow(blur_sigma: f32) -> TextShadow {
    TextShadow {
        color: RgbaLinear::from_srgb8(255, 0, 0, 1.0),
        offset_x: 3.0,
        offset_y: 3.0,
        blur_sigma,
    }
}

/// `paragraph` drawn under `alpha` and `blend`, exported to SVG and PDF.
fn exported(
    paragraph: &Paragraph,
    alpha: f32,
    blend: BlendMode,
) -> (Document, Vec<u8>) {
    let mut canvas = Canvas::new(WIDTH, HEIGHT);
    let ctx = canvas.context();
    ctx.set_global_alpha(alpha);
    ctx.set_global_composite_operation(blend);
    ctx.draw_paragraph(paragraph, 5.0, 5.0);
    let svg = parse(&svg_of(&mut canvas));
    let pdf = canvas
        .to_buffer(ImageFormat::Pdf, &EncodeOptions::default())
        .expect("a PDF");
    (svg, pdf)
}

/// A paragraph casting a sharp shadow is text and its shadow in an SVG, as
/// vectors: there is no group to composite at an alpha of 1, and the SVG
/// writes nothing of one.
#[test]
fn a_paragraph_s_sharp_shadow_stays_vector() {
    let (svg, pdf) =
        exported(&paragraph(vec![shadow(0.0)]), 1.0, BlendMode::SourceOver);
    assert_eq!(count(&svg, "text"), 2, "the shadow and the text");
    assert_eq!(count(&svg, "image"), 0);
    assert!(!embeds_an_image(&pdf), "the PDF stays vector");
}

/// A blurred shadow is pixels in an SVG, which has no blur to write, and
/// the text over it stays a vector.
#[test]
fn a_paragraph_s_blurred_shadow_is_embedded_beneath_its_text() {
    let (svg, pdf) =
        exported(&paragraph(vec![shadow(2.0)]), 1.0, BlendMode::SourceOver);
    assert_eq!(count(&svg, "image"), 1, "the blurred shadow");
    assert_eq!(count(&svg, "text"), 1, "the text over it");
    let image = svg.elements.iter().position(|(e, _)| e == "image");
    let text = svg.elements.iter().position(|(e, _)| e == "text");
    assert!(image < text, "the shadow lies beneath the text");
    // Skia's PDF backend draws a mask filter's blur as an image mask, as it
    // did inside the group.
    assert!(embeds_an_image(&pdf));
}

/// A paragraph under a global alpha, with or without a shadow, or under a
/// blend, reaches the SVG. Its group composites through a layer, which the
/// SVG writer drops whole.
#[test]
fn a_composited_paragraph_reaches_the_svg() {
    for (name, shadows, alpha, blend) in [
        ("alpha", vec![], 0.5, BlendMode::SourceOver),
        (
            "a shadow under alpha",
            vec![shadow(0.0)],
            0.5,
            BlendMode::SourceOver,
        ),
        ("multiply", vec![], 1.0, BlendMode::Multiply),
    ] {
        let (svg, _) = exported(&paragraph(shadows), alpha, blend);
        assert!(
            count(&svg, "image") + count(&svg, "text") > 0,
            "{name}: the paragraph is in the SVG"
        );
    }
}

/// Under a blend a PDF composites wrongly, so a blended paragraph is
/// embedded there as it is in an SVG.
#[test]
fn a_blended_paragraph_is_embedded_in_a_pdf() {
    let (_, pdf) = exported(&paragraph(vec![]), 1.0, BlendMode::Multiply);
    assert!(embeds_an_image(&pdf));
    let (_, plain) = exported(&paragraph(vec![]), 0.5, BlendMode::SourceOver);
    assert!(
        !embeds_an_image(&plain),
        "an alpha alone stays vector in a PDF"
    );
}

/// A canvas drawn into another under a global alpha reaches the SVG.
#[test]
fn a_canvas_drawn_in_under_alpha_reaches_the_svg() {
    let mut source = Canvas::new(20.0, 20.0);
    source.context().fill_rect(0.0, 0.0, 20.0, 20.0);
    let mut canvas = Canvas::new(WIDTH, HEIGHT);
    let ctx = canvas.context();
    ctx.set_global_alpha(0.5);
    ctx.draw_canvas(&mut source, 10.0, 10.0);
    let svg = parse(&svg_of(&mut canvas));
    assert!(count(&svg, "image") + count(&svg, "path") > 0);
}
