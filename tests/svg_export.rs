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
    // What an export marks a group's place with, and the paint it draws to
    // place the mark, are replaced before it returns.
    assert!(
        !text.contains("meo-skia-canvas:group:"),
        "a group's mark left: {text}"
    );
    assert!(!text.contains("#0F1E2D"), "a mark's paint left: {text}");
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

/// `p`, read unpremultiplied, premultiplied: what a pixel contributes. A
/// pixel of alpha 1 is 255 levels of red or of nothing read unpremultiplied,
/// and one level apart premultiplied.
fn premultiplied(p: &[u8; 4]) -> [f32; 4] {
    let a = p[3] as f32 / 255.0;
    [
        p[0] as f32 * a,
        p[1] as f32 * a,
        p[2] as f32 * a,
        p[3] as f32,
    ]
}

/// How many pixels of `a` and `b` part by more than `levels` in a
/// premultiplied channel, and the most any pixel parts by.
fn parted(a: &[[u8; 4]], b: &[[u8; 4]], levels: u8) -> (usize, u8) {
    a.iter().zip(b).fold((0, 0), |(count, most), (a, b)| {
        let (a, b) = (premultiplied(a), premultiplied(b));
        let d = (0..4)
            .map(|i| (a[i] - b[i]).abs())
            .fold(0.0, f32::max)
            .round() as u8;
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
fn exported_svg(draw: impl FnOnce(&mut Context2D)) -> (Document, usize) {
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

/// A linear gradient across `width`, red to blue.
fn ramp(width: f32) -> Shader {
    Shader::linear_gradient(
        Point { x: 0.0, y: 0.0 },
        Point { x: width, y: 0.0 },
        &[
            GradientStop {
                position: 0.0,
                color: RgbaLinear::from_srgb8(255, 0, 0, 1.0),
            },
            GradientStop {
                position: 1.0,
                color: RgbaLinear::from_srgb8(0, 0, 255, 1.0),
            },
        ],
        GradientColorSpace::Srgb,
    )
    .expect("a gradient")
}

/// The elements written as groups at an opacity, each with its opacity.
fn opacities(document: &Document) -> Vec<String> {
    document
        .elements
        .iter()
        .filter(|(element, _)| element == "g")
        .filter_map(|(_, attributes)| {
            attributes
                .iter()
                .find(|(key, _)| key == "opacity")
                .map(|(_, value)| value.clone())
        })
        .collect()
}

/// `draw` on a page exported to SVG, rendered by Skia's SVG renderer.
fn rendered_export(draw: impl FnOnce(&mut Context2D)) -> Vec<[u8; 4]> {
    let mut canvas = Canvas::new(WIDTH, HEIGHT);
    canvas.set_gpu(false);
    draw(canvas.context());
    rendered(&svg_of(&mut canvas))
}

/// How many pixels of `got` part by more than [`RENDERED`] levels, in a
/// premultiplied channel, from `reference` scaled by the opacity `alpha`
/// gives the pixel's column: what a group at that opacity renders as, its
/// content rendered at full opacity and composited at it. A renderer draws
/// text and gradients its own way, which a canvas's pixels cannot stand in
/// for, so the reference is the same renderer's.
fn scaled_parted(
    got: &[[u8; 4]],
    reference: &[[u8; 4]],
    alpha: impl Fn(usize) -> f32,
) -> usize {
    got.iter()
        .zip(reference)
        .enumerate()
        .filter(|(at, (got, reference))| {
            let a = alpha(at % WIDTH as usize);
            premultiplied(got).iter().zip(premultiplied(reference)).any(
                |(got, reference)| {
                    (got - reference * a).abs() > RENDERED as f32
                },
            )
        })
        .count()
}

/// A paragraph under a global alpha is its text inside a group at that
/// opacity, as it is on the canvas: composited as one.
#[test]
fn a_paragraph_under_alpha_is_text_in_a_group() {
    let draw = |alpha: f32| {
        move |ctx: &mut Context2D| {
            ctx.set_global_alpha(alpha);
            ctx.draw_paragraph(&paragraph(vec![]), 5.0, 5.0);
        }
    };
    let (svg, _) = exported_svg(draw(0.25));
    assert_eq!(opacities(&svg), ["0.25"]);
    assert_eq!(count(&svg, "text"), 1);
    assert_eq!(count(&svg, "image"), 0);
    let parted = scaled_parted(
        &rendered_export(draw(0.25)),
        &rendered_export(draw(1.0)),
        |_| 0.25,
    );
    assert_eq!(parted, 0);
}

/// A paragraph casting a blurred shadow under a global alpha keeps its
/// text: the group holds the shadow as pixels and the text as a vector.
#[test]
fn a_shadowed_paragraph_under_alpha_keeps_its_text() {
    let draw = |alpha: f32| {
        move |ctx: &mut Context2D| {
            ctx.set_global_alpha(alpha);
            ctx.draw_paragraph(&paragraph(vec![shadow(2.0)]), 5.0, 5.0);
        }
    };
    let (svg, _) = exported_svg(draw(0.6));
    assert_eq!(opacities(&svg), ["0.6"]);
    assert_eq!(count(&svg, "text"), 1);
    assert_eq!(count(&svg, "image"), 1);
    let parted = scaled_parted(
        &rendered_export(draw(0.6)),
        &rendered_export(draw(1.0)),
        |_| 0.6,
    );
    assert_eq!(parted, 0);
}

/// Translucent layers inside one another nest as groups: where their
/// squares overlap, the inner one is composited at both opacities.
#[test]
fn nested_translucent_layers_nest_their_groups() {
    let (svg, parted) = exported_svg(|ctx| {
        ctx.save_layer_with(0.5, None, None);
        ctx.fill_rect(5.0, 5.0, 30.0, 20.0);
        ctx.save_layer_with(0.5, None, None);
        ctx.fill_rect(20.0, 15.0, 30.0, 20.0);
        ctx.restore();
        ctx.restore();
    });
    assert_eq!(opacities(&svg), ["0.5", "0.5"]);
    assert_eq!(count(&svg, "image"), 0);
    assert_eq!(parted, 0);
}

/// Every id a document declares is declared once, and everything that
/// refers to one names one it declares: nested groups at an opacity, each
/// clipped and filled with a gradient, beside the page's own clip and
/// gradient, are each written into a document of their own whose ids
/// would otherwise be the page's.
#[test]
fn nested_groups_keep_their_ids_apart() {
    let draw = |outer: f32, inner: f32| {
        move |ctx: &mut Context2D| {
            let clipped = |ctx: &mut Context2D, x: f32| {
                ctx.save();
                ctx.begin_path();
                ctx.rect(x, 4.0, 22.0, 30.0);
                ctx.clip(FillRule::NonZero);
                ctx.set_fill_shader(&ramp(WIDTH));
                ctx.fill_rect(0.0, 0.0, WIDTH, HEIGHT);
                ctx.restore();
            };
            clipped(ctx, 2.0);
            ctx.save_layer_with(outer, None, None);
            clipped(ctx, 28.0);
            ctx.save_layer_with(inner, None, None);
            clipped(ctx, 54.0);
            ctx.restore();
            ctx.restore();
        }
    };
    let (svg, _) = exported_svg(draw(0.8, 0.5));
    let ids: Vec<&str> = svg
        .elements
        .iter()
        .flat_map(|(_, attributes)| attributes)
        .filter(|(key, _)| key == "id")
        .map(|(_, value)| value.as_str())
        .collect();
    let mut unique = ids.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), ids.len(), "an id declared twice: {ids:?}");
    let references: Vec<&str> = svg
        .elements
        .iter()
        .flat_map(|(_, attributes)| attributes)
        .filter_map(|(key, value)| match key.ends_with("href") {
            true => value.strip_prefix('#'),
            false => value
                .strip_prefix("url(#")
                .and_then(|value| value.strip_suffix(')')),
        })
        .collect();
    assert!(
        references.len() >= 6,
        "three clips and three gradients: {references:?}"
    );
    for reference in references {
        assert!(
            ids.contains(&reference),
            "{reference} names no id in {ids:?}"
        );
    }
    assert_eq!(opacities(&svg), ["0.8", "0.5"]);
    // Each strip at the opacities it lies under: none, the outer layer's,
    // and both layers'.
    let parted = scaled_parted(
        &rendered_export(draw(0.8, 0.5)),
        &rendered_export(draw(1.0, 1.0)),
        |x| match x {
            0..26 => 1.0,
            26..52 => 0.8,
            _ => 0.4,
        },
    );
    assert_eq!(parted, 0);
}

/// A canvas drawn into another at another size under a global alpha is a
/// group at that opacity, and what it holds that an SVG embeds lands where
/// the canvas draws it, at that size.
#[test]
fn a_canvas_drawn_in_under_alpha_embeds_in_place() {
    let mut source = Canvas::new(40.0, 20.0);
    {
        let ctx = source.context();
        ctx.fill_rect(2.0, 2.0, 10.0, 10.0);
        shadowed_square(ctx, 20.0);
    }
    let (svg, parted) = exported_svg(|ctx| {
        ctx.set_global_alpha(0.5);
        ctx.draw_canvas_sized(&mut source, 6.0, 4.0, 60.0, 30.0);
    });
    assert_eq!(opacities(&svg), ["0.5"]);
    assert_eq!(count(&svg, "image"), 1, "the shadowed square");
    assert_eq!(count(&svg, "path"), 1, "the plain one");
    // A square and a blurred one, which a renderer draws as the canvas
    // does: the canvas's own pixels are the reference.
    assert_eq!(parted, 0);
}

/// A canvas holding a draw its clip cuts, drawn into another, is written
/// as what it holds: the cut triangle as a path under its clip, through no
/// layer, and, where the source also holds a shadowed square and is drawn
/// through a layer for it, that alone as pixels.
#[test]
fn a_canvas_holding_a_cut_draw_is_drawn_in_as_its_entries() {
    for shadowed in [false, true] {
        let mut source = Canvas::new(WIDTH, HEIGHT);
        source.set_gpu(false);
        {
            let ctx = source.context();
            ctx.save();
            ctx.begin_path();
            ctx.rect(0.0, 0.0, 40.5, HEIGHT);
            ctx.clip(FillRule::NonZero);
            ctx.begin_path();
            ctx.move_to(10.0, 35.0);
            ctx.line_to(45.0, 5.0);
            ctx.line_to(70.0, 35.0);
            ctx.close_path();
            ctx.fill(FillRule::NonZero);
            ctx.restore();
            if shadowed {
                shadowed_square(ctx, 50.0);
            }
        }
        let (svg, _) =
            exported_svg(|ctx| ctx.draw_canvas(&mut source, 0.0, 0.0));
        assert_eq!(count(&svg, "path"), 1, "shadowed {shadowed}: the triangle");
        assert_eq!(count(&svg, "image"), usize::from(shadowed));
        assert!(count(&svg, "clipPath") > 0, "the triangle under its clip");
    }
}

/// Inside a layer, a draw an SVG embeds and then, under a clip, a plain one:
/// the plain draw stays a path, filed apart from the embedded one though
/// the frame's run it was recorded in closes into the layer's recording.
#[test]
fn a_plain_draw_after_an_embedded_one_in_a_layer_stays_vector() {
    let (svg, parted) = exported_svg(|ctx| {
        ctx.save_layer();
        shadowed_square(ctx, 40.0);
        ctx.save();
        ctx.begin_path();
        ctx.rect(2.0, 2.0, 30.0, 30.0);
        ctx.clip(FillRule::NonZero);
        ctx.fill_rect(5.0, 5.0, 20.0, 20.0);
        ctx.restore();
        ctx.restore();
    });
    assert_eq!(count(&svg, "path"), 1, "the plain draw");
    assert_eq!(count(&svg, "image"), 1, "the shadowed one");
    assert_eq!(parted, 0);
}

/// A paragraph under a global alpha inside a clip is clipped once, as the
/// same paragraph without the alpha is.
#[test]
fn a_paragraph_under_alpha_is_clipped_once() {
    let clips = |alpha: f32| {
        let mut canvas = Canvas::new(WIDTH, HEIGHT);
        let ctx = canvas.context();
        ctx.begin_path();
        ctx.arc(30.0, 20.0, 18.0, 0.0, std::f32::consts::TAU, false)
            .expect("arc");
        ctx.clip(FillRule::NonZero);
        ctx.set_global_alpha(alpha);
        ctx.draw_paragraph(&paragraph(vec![]), 5.0, 5.0);
        count(&parse(&svg_of(&mut canvas)), "clipPath")
    };
    assert_eq!(clips(0.5), clips(1.0));
    assert!(clips(1.0) > 0);
}

/// Half a level above one 255th. Nothing these pages draw is as faint, and
/// the mark an export holds a group's place with, at no opacity, and the
/// paint it draws before the mark, at one 255th, are both fainter.
const FAINT: f32 = 1.5 / 255.0;

/// How many elements `document` draws at an opacity under [`FAINT`].
fn faint(document: &Document) -> usize {
    document
        .elements
        .iter()
        .zip(&document.drawn)
        .filter(|((_, attributes), drawn)| {
            **drawn
                && attributes.iter().any(|(key, value)| {
                    matches!(
                        key.as_str(),
                        "opacity" | "fill-opacity" | "stroke-opacity"
                    ) && value.parse::<f32>().is_ok_and(|v| v < FAINT)
                })
        })
        .count()
}

/// How many alphas above 0 and under [`FAINT`] `pdf`'s graphics states set:
/// Skia's PDF backend writes a translucent paint's alpha as a graphics
/// state's `/ca` and `/CA` (ISO 32000-1, table 58), outside any compressed
/// stream. An alpha of 0 is not counted: the backend sets one for a
/// paragraph's blurred shadow at any global alpha, in a page with no group.
fn faint_in_pdf(pdf: &[u8]) -> usize {
    let text = String::from_utf8_lossy(pdf);
    ["/ca ", "/CA "]
        .iter()
        .flat_map(|key| text.match_indices(key))
        .filter(|(at, key)| {
            text[at + key.len()..]
                .split(|c: char| c != '.' && !c.is_ascii_digit())
                .next()
                .and_then(|value| value.parse::<f32>().ok())
                .is_some_and(|value| value > 0.0 && value < FAINT)
        })
        .count()
}

/// A page a test draws: what it holds, and how it is drawn.
type NamedDraw<'a> = (&'a str, &'a dyn Fn(&mut Context2D));

/// Nothing of a group's mark, or of the paint an export draws before it,
/// is left in any export of a page holding groups: no element as faint as
/// either in the SVG, and no alpha that faint in the PDF. A fill at one
/// 255th, exported the same way, is found by both checks.
#[test]
fn no_trace_of_a_group_s_mark_is_left_in_an_export() {
    let export = |draw: &dyn Fn(&mut Context2D)| {
        let mut canvas = Canvas::new(WIDTH, HEIGHT);
        canvas.set_gpu(false);
        draw(canvas.context());
        let svg = parse(&svg_of(&mut canvas));
        let pdf = canvas
            .to_buffer(ImageFormat::Pdf, &EncodeOptions::default())
            .expect("a PDF");
        (svg, pdf)
    };
    let (svg, pdf) = export(&|ctx| {
        ctx.set_fill_style(RgbaLinear::from_srgb8(200, 40, 120, 1.0 / 255.0));
        ctx.fill_rect(5.0, 5.0, 20.0, 20.0);
    });
    assert_eq!(faint(&svg), 1, "the SVG check finds a faint fill");
    assert!(faint_in_pdf(&pdf) > 0, "the PDF check finds a faint fill");

    let pages: [NamedDraw; 5] = [
        ("a paragraph under alpha in a clip", &|ctx| {
            ctx.begin_path();
            ctx.rect(4.0, 4.0, 40.0, 30.0);
            ctx.clip(FillRule::NonZero);
            ctx.set_global_alpha(0.25);
            ctx.draw_paragraph(&paragraph(vec![]), 5.0, 5.0);
        }),
        ("a shadowed paragraph under alpha", &|ctx| {
            ctx.set_global_alpha(0.6);
            ctx.draw_paragraph(&paragraph(vec![shadow(2.0)]), 5.0, 5.0);
        }),
        ("nested translucent layers", &|ctx| {
            ctx.save_layer_with(0.8, None, None);
            ctx.set_fill_shader(&ramp(WIDTH));
            ctx.fill_rect(5.0, 5.0, 30.0, 20.0);
            ctx.save_layer_with(0.5, None, None);
            ctx.fill_rect(20.0, 15.0, 30.0, 20.0);
            ctx.restore();
            ctx.restore();
        }),
        ("groups in clips side by side", &|ctx| {
            for x in [2.0, 28.0, 54.0] {
                ctx.save();
                ctx.begin_path();
                ctx.rect(x, 4.0, 22.0, 30.0);
                ctx.clip(FillRule::NonZero);
                ctx.save_layer_with(0.5, None, None);
                ctx.fill_rect(0.0, 0.0, WIDTH, HEIGHT);
                ctx.restore();
                ctx.restore();
            }
        }),
        ("a canvas drawn in under alpha", &|ctx| {
            let mut source = Canvas::new(40.0, 20.0);
            shadowed_square(source.context(), 4.0);
            ctx.set_global_alpha(0.5);
            ctx.draw_canvas(&mut source, 10.0, 10.0);
        }),
    ];
    for (name, draw) in pages {
        let (svg, pdf) = export(draw);
        assert!(!opacities(&svg).is_empty(), "{name}: a group is spliced");
        assert_eq!(faint(&svg), 0, "{name}: a faint element in the SVG");
        assert_eq!(faint_in_pdf(&pdf), 0, "{name}: a faint alpha in the PDF");
    }
}

/// The same canvas exported to SVG twice, with another export between,
/// comes out byte for byte the same: its clips, and those of a group in
/// it, are numbered by the document rather than by what the process
/// drew before.
#[test]
fn an_svg_export_does_not_depend_on_what_was_exported_before() {
    let page = |x: f32| {
        let mut canvas = Canvas::new(WIDTH, HEIGHT);
        canvas.set_gpu(false);
        let ctx = canvas.context();
        ctx.save();
        ctx.begin_path();
        ctx.rect(x, 4.0, 30.0, 30.0);
        ctx.clip(FillRule::NonZero);
        ctx.fill_rect(0.0, 0.0, WIDTH, HEIGHT);
        ctx.restore();
        ctx.save_layer_with(0.5, None, None);
        ctx.begin_path();
        ctx.rect(x + 10.0, 10.0, 20.0, 20.0);
        ctx.clip(FillRule::NonZero);
        ctx.fill_rect(0.0, 0.0, WIDTH, HEIGHT);
        ctx.restore();
        canvas
    };
    let mut first = page(2.0);
    let before = svg_of(&mut first);
    let document = parse(&before);
    assert_eq!(
        count(&document, "clipPath"),
        2,
        "the page's and the group's"
    );
    assert_eq!(opacities(&document), ["0.5"]);
    parse(&svg_of(&mut page(30.0)));
    assert_eq!(
        String::from_utf8_lossy(&svg_of(&mut first)),
        String::from_utf8_lossy(&before)
    );
}

/// One draw of the sweep, on a page with nothing else on it.
type SweepDraw = fn(&mut Context2D);

/// The draws the sweep makes: each kind of draw a page holds, and each
/// that reaches a vector backend some other way -- through a shadow, a
/// filter, a shader a backend may not write, a paragraph's own paints, a
/// canvas's pattern.
fn sweep_draws() -> Vec<(&'static str, SweepDraw)> {
    vec![
        ("a rectangle", |ctx| ctx.fill_rect(10.0, 8.0, 30.0, 20.0)),
        ("a path", |ctx| {
            ctx.begin_path();
            ctx.move_to(10.0, 30.0);
            ctx.line_to(30.0, 5.0);
            ctx.line_to(55.0, 34.0);
            ctx.close_path();
            ctx.fill(FillRule::NonZero);
        }),
        ("a stroke", |ctx| {
            ctx.set_line_width(4.0);
            ctx.stroke_rect(10.0, 8.0, 30.0, 20.0);
        }),
        ("canvas text", |ctx| {
            assert!(ctx.set_font_css("20px sans-serif"));
            ctx.fill_text("Hi", 10.0, 28.0, None);
        }),
        ("an image", |ctx| {
            let mut source = Canvas::new(20.0, 20.0);
            source.context().fill_rect(0.0, 0.0, 20.0, 20.0);
            let png = source
                .to_buffer(ImageFormat::Png, &EncodeOptions::default())
                .expect("a PNG");
            ctx.draw_image(
                &Image::from_encoded(&png).expect("an image"),
                10.0,
                8.0,
            );
        }),
        ("a canvas drawn in", |ctx| {
            let mut source = Canvas::new(20.0, 20.0);
            source.context().fill_rect(0.0, 0.0, 20.0, 20.0);
            ctx.draw_canvas(&mut source, 10.0, 8.0);
        }),
        ("a paragraph", |ctx| {
            ctx.draw_paragraph(&paragraph(vec![]), 5.0, 5.0)
        }),
        ("a paragraph with a blurred shadow", |ctx| {
            ctx.draw_paragraph(&paragraph(vec![shadow(2.0)]), 5.0, 5.0);
        }),
        ("a shadowed rectangle", |ctx| shadowed_square(ctx, 20.0)),
        ("a blurred rectangle", |ctx| {
            ctx.set_filter_css("blur(2px)").expect("blur");
            ctx.fill_rect(10.0, 8.0, 30.0, 20.0);
        }),
        ("a drop-shadowed rectangle", |ctx| {
            ctx.set_filter_css("drop-shadow(3px 3px 1px blue)")
                .expect("shadow");
            ctx.fill_rect(10.0, 8.0, 30.0, 20.0);
        }),
        ("a canvas pattern", |ctx| {
            let mut source = Canvas::new(20.0, 20.0);
            {
                let tile = source.context();
                tile.set_fill_style_css("rgb(200 40 90)").expect("a colour");
                tile.fill_rect(0.0, 0.0, 20.0, 20.0);
                tile.set_filter_css("blur(2px)").expect("blur");
                tile.set_fill_style_css("rgb(30 60 200)").expect("a colour");
                tile.fill_rect(5.0, 5.0, 10.0, 10.0);
            }
            let pattern = ctx
                .create_pattern_from_canvas(&mut source, PatternRepeat::Repeat);
            ctx.set_fill_pattern(&pattern);
            ctx.fill_rect(10.0, 8.0, 30.0, 20.0);
        }),
        ("a linear gradient", |ctx| {
            ctx.set_fill_shader(&ramp(WIDTH));
            ctx.fill_rect(10.0, 8.0, 50.0, 20.0);
        }),
        ("a conic gradient", |ctx| {
            let conic = Shader::sweep_gradient(
                Point { x: 35.0, y: 18.0 },
                0.0,
                360.0,
                &[
                    GradientStop {
                        position: 0.0,
                        color: RgbaLinear::from_srgb8(255, 0, 0, 1.0),
                    },
                    GradientStop {
                        position: 1.0,
                        color: RgbaLinear::from_srgb8(0, 0, 255, 1.0),
                    },
                ],
                GradientColorSpace::Srgb,
            )
            .expect("a conic gradient");
            ctx.set_fill_shader(&conic);
            ctx.fill_rect(10.0, 8.0, 50.0, 20.0);
        }),
    ]
}

/// The states the sweep draws each under, set before the draw, and what
/// ends them after it.
fn sweep_states() -> Vec<(&'static str, SweepDraw, SweepDraw)> {
    vec![
        ("plain", |_| {}, |_| {}),
        ("a global alpha", |ctx| ctx.set_global_alpha(0.5), |_| {}),
        (
            "multiply",
            |ctx| ctx.set_global_composite_operation(BlendMode::Multiply),
            |_| {},
        ),
        ("a layer", |ctx| ctx.save_layer(), |ctx| ctx.restore()),
        (
            "a translucent layer",
            |ctx| ctx.save_layer_with(0.5, None, None),
            |ctx| ctx.restore(),
        ),
        (
            "nested translucent layers under a turn",
            |ctx| {
                ctx.translate(40.0, 20.0);
                ctx.rotate(0.2);
                ctx.translate(-40.0, -20.0);
                ctx.save_layer_with(0.8, None, None);
                ctx.save_layer_with(0.6, None, None);
            },
            |ctx| {
                ctx.restore();
                ctx.restore();
            },
        ),
        (
            "a clip",
            |ctx| {
                ctx.begin_path();
                ctx.arc(30.0, 20.0, 16.0, 0.0, std::f32::consts::TAU, false)
                    .expect("arc");
                ctx.clip(FillRule::NonZero);
            },
            |_| {},
        ),
    ]
}

/// Every draw of the sweep under every state exports an SVG that is
/// well-formed XML, with nothing left of a group's mark, and holds the
/// draw -- rendered by Skia's renderer, it inks at least half the pixels
/// the canvas inks -- and a PDF that is a whole file. Neither holds
/// anything as faint as a mark's paint. A drop here is a draw an export
/// silently loses.
#[test]
fn every_draw_under_every_state_reaches_the_svg_and_the_pdf() {
    let mut lost = Vec::new();
    for (draw_name, draw) in sweep_draws() {
        for (state_name, enter, leave) in sweep_states() {
            let mut canvas = Canvas::new(WIDTH, HEIGHT);
            canvas.set_gpu(false);
            {
                let ctx = canvas.context();
                enter(ctx);
                draw(ctx);
                leave(ctx);
            }
            let svg = svg_of(&mut canvas);
            let Ok(document) = std::panic::catch_unwind(|| parse(&svg)) else {
                lost.push(format!(
                    "{draw_name} under {state_name}: malformed, or a mark left"
                ));
                continue;
            };
            if faint(&document) > 0 {
                lost.push(format!(
                    "{draw_name} under {state_name}: a faint element in the SVG"
                ));
            }
            let inked =
                |pixels: &[[u8; 4]]| pixels.iter().filter(|p| p[3] > 0).count();
            let (own, written) =
                (inked(&pixels(&mut canvas)), inked(&rendered(&svg)));
            if own > 0 && written * 2 < own {
                lost.push(format!("{draw_name} under {state_name}: {written} of {own} pixels in the SVG"));
            }
            let pdf = canvas
                .to_buffer(ImageFormat::Pdf, &EncodeOptions::default())
                .expect("a PDF");
            let ends =
                String::from_utf8_lossy(&pdf[pdf.len().saturating_sub(16)..])
                    .into_owned();
            if !pdf.starts_with(b"%PDF-") || !ends.contains("%%EOF") {
                lost.push(format!(
                    "{draw_name} under {state_name}: a PDF cut short"
                ));
            }
            if faint_in_pdf(&pdf) > 0 {
                lost.push(format!(
                    "{draw_name} under {state_name}: a faint alpha in the PDF"
                ));
            }
        }
    }
    assert!(lost.is_empty(), "{}", lost.join("\n"));
}
