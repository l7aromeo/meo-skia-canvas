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
