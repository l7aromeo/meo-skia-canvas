//! An SVG export is a well-formed document holding everything the page
//! draws: every output here is parsed as XML before anything is asked of it.

use meo_skia_canvas::prelude::*;
use quick_xml::{Reader, events::Event};

/// The page every case draws on.
const WIDTH: f32 = 80.0;
const HEIGHT: f32 = 40.0;

/// The elements of a parsed document, each with its attributes, in order,
/// and whether it is drawn: not one inside a `<clipPath>`, which only
/// shapes a clip. Skia's writer puts an embedded image in `<defs>` and draws
/// it through a `<use>`, so one there counts.
struct Document {
    elements: Vec<(String, Vec<(String, String)>)>,
    drawn: Vec<bool>,
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
    let mut drawn = Vec::new();
    // The open elements, outermost first.
    let mut open: Vec<String> = Vec::new();
    loop {
        let event = reader
            .read_event()
            .unwrap_or_else(|e| panic!("not well-formed XML ({e}): {text}"));
        match event {
            Event::Start(ref tag) | Event::Empty(ref tag) => {
                let name = tag.name().into_inner().to_owned();
                drawn.push(!open.iter().any(|e| e == "clipPath"));
                if matches!(event, Event::Start(_)) {
                    open.push(name.clone());
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
                elements.push((name, attributes));
            }
            Event::End(_) => {
                open.pop();
            }
            Event::Eof => break,
            _ => {}
        }
    }
    assert!(open.is_empty(), "an element left open: {text}");
    Document { elements, drawn }
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

/// The number of elements named `name` drawn, outside a clip's shape.
fn count(document: &Document, name: &str) -> usize {
    document
        .elements
        .iter()
        .zip(&document.drawn)
        .filter(|((element, _), drawn)| element == name && **drawn)
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

/// `canvas`'s own pixels, RGBA.
fn pixels(canvas: &mut Canvas) -> Vec<[u8; 4]> {
    let data = canvas
        .context()
        .get_image_data(0.0, 0.0, WIDTH, HEIGHT)
        .expect("pixels");
    data.pixels()
        .chunks(4)
        .map(|p| [p[0], p[1], p[2], p[3]])
        .collect()
}

/// `svg` rendered at the page's size by Skia's SVG renderer, as `pixels`
/// reads a canvas.
fn rendered(svg: &[u8]) -> Vec<[u8; 4]> {
    let image = Svg::parse(&String::from_utf8_lossy(svg))
        .expect("parses")
        .rasterize(WIDTH as u32, HEIGHT as u32)
        .expect("rasterizes");
    let mut back = Canvas::new(WIDTH, HEIGHT);
    back.set_gpu(false);
    back.context().draw_image(&image, 0.0, 0.0);
    pixels(&mut back)
}

/// How many pixels of `a` and `b` part by more than `levels` in a channel,
/// and the most any pixel parts by.
fn parted(a: &[[u8; 4]], b: &[[u8; 4]], levels: u8) -> (usize, u8) {
    a.iter().zip(b).fold((0, 0), |(count, most), (a, b)| {
        let d = (0..4).map(|i| a[i].abs_diff(b[i])).max().unwrap_or(0);
        (count + usize::from(d > levels), most.max(d))
    })
}

fn shadowed_square(ctx: &mut Context2D, x: f32) {
    ctx.set_shadow_color(RgbaLinear::from_srgb8(255, 0, 0, 1.0));
    ctx.set_shadow_offset(3.0, 2.0);
    ctx.set_shadow_blur(2.0);
    ctx.fill_rect(x, 5.0, 12.0, 10.0);
    ctx.set_shadow_color(RgbaLinear::from_srgb8(0, 0, 0, 0.0));
}

/// How far a pixel of an export, rendered by Skia's SVG renderer, may part
/// from the canvas's own: the vector draws and the embedded pixels both
/// land within it, under a turn as well.
const RENDERED: u8 = 2;

/// `draw` on a page, exported to SVG: the document, and how many pixels of
/// it rendered part from the canvas's own by more than [`RENDERED`].
fn exported_svg(draw: impl Fn(&mut Context2D)) -> (Document, usize) {
    let mut canvas = Canvas::new(WIDTH, HEIGHT);
    canvas.set_gpu(false);
    draw(canvas.context());
    let svg = svg_of(&mut canvas);
    let document = parse(&svg);
    let (count, _) = parted(&pixels(&mut canvas), &rendered(&svg), RENDERED);
    (document, count)
}

/// A layer that composites what it holds as it stands is taken entry by
/// entry: its plain draw stays a vector, and only the shadowed draw an SVG
/// cannot write is embedded as pixels.
#[test]
fn an_unpainted_layer_keeps_what_it_can_write_vector() {
    let (svg, parted) = exported_svg(|ctx| {
        ctx.save_layer();
        ctx.fill_rect(5.0, 20.0, 20.0, 12.0);
        shadowed_square(ctx, 40.0);
        ctx.restore();
    });
    assert_eq!(count(&svg, "path"), 1, "the plain draw");
    assert_eq!(count(&svg, "image"), 1, "the shadowed draw");
    assert_eq!(parted, 0);
}

/// A layer holding a blend is one image: what it holds blends with the
/// layer, not with the page beneath it.
#[test]
fn a_layer_holding_a_blend_is_one_image() {
    let (svg, parted) = exported_svg(|ctx| {
        ctx.save_layer();
        ctx.fill_rect(5.0, 20.0, 20.0, 12.0);
        ctx.set_global_composite_operation(BlendMode::Multiply);
        ctx.fill_rect(10.0, 10.0, 30.0, 15.0);
        ctx.restore();
    });
    assert_eq!(count(&svg, "path"), 0);
    assert_eq!(count(&svg, "image"), 1);
    assert_eq!(parted, 0);
}

/// Nested layers under a turn are taken entry by entry too, and what is
/// embedded of them lands where the canvas draws it.
#[test]
fn nested_layers_under_a_turn_embed_in_place() {
    let (svg, parted) = exported_svg(|ctx| {
        ctx.translate(40.0, 20.0);
        ctx.rotate(0.4);
        ctx.save_layer();
        ctx.save_layer();
        ctx.fill_rect(-30.0, -10.0, 15.0, 10.0);
        ctx.set_filter_css("blur(1.5px)").expect("blur");
        ctx.fill_rect(0.0, -8.0, 18.0, 12.0);
        ctx.restore();
        ctx.restore();
    });
    assert_eq!(count(&svg, "path"), 1, "the plain draw");
    assert_eq!(count(&svg, "image"), 1, "the blurred draw");
    assert_eq!(parted, 0);
}

/// A layer's bounds confine what it holds in an SVG as on the canvas.
#[test]
fn an_unpainted_layer_keeps_to_its_bounds() {
    let (svg, parted) = exported_svg(|ctx| {
        let bounds = Rect {
            left: 10.0,
            top: 10.0,
            right: 40.0,
            bottom: 30.0,
        };
        ctx.save_layer_with(1.0, Some(bounds), None);
        ctx.fill_rect(0.0, 0.0, 60.0, 40.0);
        shadowed_square(ctx, 50.0);
        ctx.restore();
    });
    assert_eq!(count(&svg, "path"), 1);
    assert_eq!(parted, 0);
}

/// The clip a layer is opened under confines what it holds in an SVG once,
/// as it confines the same draws without the layer: an SVG renderer takes a
/// clip's edge its own way, so the reference is the page drawn without the
/// layer and rendered the same way. Clipped again by the layer's clip, the
/// edges' coverage was taken twice, 64 levels off at the arc.
#[test]
fn an_unpainted_layer_keeps_to_its_clip_once() {
    let svg = |layer: bool| {
        let mut canvas = Canvas::new(WIDTH, HEIGHT);
        canvas.set_gpu(false);
        let ctx = canvas.context();
        ctx.begin_path();
        ctx.arc(30.0, 20.0, 15.0, 0.0, std::f32::consts::TAU, false)
            .expect("arc");
        ctx.clip(FillRule::NonZero);
        if layer {
            ctx.save_layer();
        }
        ctx.fill_rect(0.0, 0.0, 30.0, 40.0);
        shadowed_square(ctx, 30.0);
        if layer {
            ctx.restore();
        }
        svg_of(&mut canvas)
    };
    let (with, without) = (svg(true), svg(false));
    let document = parse(&with);
    assert_eq!(count(&document, "path"), 1, "the plain draw");
    assert_eq!(count(&document, "image"), 1, "the shadowed draw");
    assert_eq!(parted(&rendered(&with), &rendered(&without), RENDERED).0, 0);
}

/// A layer that composites as it stands keeps its draws vector even right
/// after a draw an SVG embeds: the run of what is embedded ends at it.
#[test]
fn an_unpainted_layer_after_an_embedded_draw_stays_vector() {
    let (svg, parted) = exported_svg(|ctx| {
        shadowed_square(ctx, 40.0);
        ctx.save_layer();
        ctx.fill_rect(5.0, 20.0, 20.0, 12.0);
        ctx.restore();
    });
    assert_eq!(count(&svg, "path"), 1, "the layer's draw");
    assert_eq!(count(&svg, "image"), 1, "the shadowed draw");
    assert_eq!(parted, 0);
}

/// A layer filtering what lies beneath it is no layer that composites what
/// it holds as it stands, and is embedded whole.
#[test]
fn a_layer_over_a_backdrop_filter_is_one_image() {
    let (svg, _) = exported_svg(|ctx| {
        ctx.fill_rect(0.0, 0.0, 40.0, 40.0);
        let offset =
            ImageFilter::offset(2.0, 0.0, None, None).expect("a filter");
        ctx.save_layer_with(1.0, None, Some(&offset));
        ctx.fill_rect(45.0, 5.0, 20.0, 12.0);
        ctx.restore();
    });
    assert_eq!(count(&svg, "path"), 1, "only the page's own square");
    assert_eq!(count(&svg, "image"), 1, "the layer");
}
