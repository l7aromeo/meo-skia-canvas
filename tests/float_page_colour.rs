//! A flat colour lands exactly on a float canvas in a space other than
//! sRGB, a document of that canvas keeps it a plain colour, and text on it
//! is covered as it is on an sRGB one.
//!
//! Skia stores a paint's colour in sRGB and converts it into the canvas's
//! space at the draw through skcms's approximate transfer function, whose
//! output lies on a grid 64 ulps wide, so no plain colour lands exactly
//! there. Such a canvas records each draw twice -- see
//! `page::Recorders` -- once for pixels, painting a flat colour through a
//! shader that holds it in the canvas's own space, and once for documents,
//! painting it plain. Text paints plain in both, and so is not exact: see
//! `a_float_page_covers_text_as_srgb_does`.

use meo_skia_canvas::prelude::*;

/// Both engines. A float canvas asking for the GPU is drawn on the raster
/// backend where the GPU cannot composite in float -- see
/// `Canvas::engine_kind` -- which on Metal and Vulkan is every float canvas
/// today, so there the two draw alike; the GPU case holds for a backend
/// that can.
const ENGINES: [bool; 2] = [false, true];

/// The canvas spaces, each with the CSS function naming a colour in it, so
/// that the exact value read back is the colour's own components.
const SPACES: [(PixelColorSpace, &str); 3] = [
    (PixelColorSpace::SrgbLinear, "srgb-linear"),
    (PixelColorSpace::DisplayP3, "display-p3"),
    (PixelColorSpace::Rec2020, "rec2020"),
];

/// The side of the square canvas every case draws on.
const SIDE: f32 = 32.0;

fn canvas(space: PixelColorSpace, depth: PixelDepth, gpu: bool) -> Canvas {
    Canvas::with_options(
        SIDE,
        SIDE,
        CanvasOptions {
            color_space: space,
            color_type: depth,
            gpu,
            ..CanvasOptions::default()
        },
    )
    .expect("a canvas")
}

/// The pixel at (`x`, `y`), premultiplied F32 in the canvas's own space.
fn pixel(
    canvas: &mut Canvas,
    space: PixelColorSpace,
    x: f32,
    y: f32,
) -> [f32; 4] {
    let data = canvas
        .context()
        .get_image_data_as(
            x,
            y,
            1.0,
            1.0,
            PixelExportOptions {
                depth: PixelDepth::F32,
                color_space: space,
                premultiplied: true,
                ..PixelExportOptions::default()
            },
        )
        .expect("a pixel");
    let bytes = data.pixels();
    [0, 1, 2, 3].map(|c| {
        f32::from_le_bytes([
            bytes[c * 4],
            bytes[c * 4 + 1],
            bytes[c * 4 + 2],
            bytes[c * 4 + 3],
        ])
    })
}

/// Whether `got` is `exact` as `depth` holds it: the same value in F32,
/// within the half of a step an F16 rounds by.
fn holds(got: [f32; 4], exact: [f32; 4], depth: PixelDepth) -> bool {
    got.iter().zip(exact).all(|(got, exact)| match depth {
        PixelDepth::F16 => {
            (got - exact).abs() <= exact.abs() * 2f32.powi(-11) + 2f32.powi(-25)
        }
        _ => *got == exact,
    })
}

/// The ways a flat colour is drawn, each leaving the middle of the canvas
/// covered whole.
type Draw = fn(&mut Context2D);

const DRAWS: [(&str, Draw); 4] = [
    ("a fill", |ctx| ctx.fill_rect(0.0, 0.0, SIDE, SIDE)),
    ("a stroke", |ctx| {
        ctx.set_line_width(SIDE);
        ctx.begin_path();
        ctx.move_to(0.0, SIDE / 2.0);
        ctx.line_to(SIDE, SIDE / 2.0);
        ctx.stroke();
    }),
    ("a blurred fill", |ctx| {
        ctx.set_filter_css("blur(1px)").expect("a blur");
        ctx.fill_rect(-SIDE, -SIDE, SIDE * 3.0, SIDE * 3.0);
    }),
    // A path, which the blur takes on its result, through `context::chain`.
    ("a blurred stroke", |ctx| {
        ctx.set_filter_css("blur(1px)").expect("a blur");
        ctx.set_line_width(SIDE * 3.0);
        ctx.begin_path();
        ctx.move_to(-SIDE, SIDE / 2.0);
        ctx.line_to(SIDE * 2.0, SIDE / 2.0);
        ctx.stroke();
    }),
];

/// Every way a flat colour is drawn, on every space, depth and engine, reads
/// back as the colour named, at every one of 401 values of its first
/// component from -0.1 to 1.1.
#[test]
fn a_flat_colour_lands_exactly_on_a_float_page() {
    let mut wrong = Vec::new();
    for gpu in ENGINES {
        for (space, function) in SPACES {
            for depth in [PixelDepth::F32, PixelDepth::F16] {
                for (name, draw) in DRAWS {
                    // Blurs are swept more thinly: a blur costs more to lay
                    // down than a rectangle.
                    let step = match name {
                        "a fill" | "a stroke" => 1,
                        _ => 20,
                    };
                    let missed: Vec<_> = (0..=400)
                        .step_by(step)
                        .filter_map(|i| {
                            let v = i as f32 / 400.0 * 1.2 - 0.1;
                            let mut canvas = canvas(space, depth, gpu);
                            let ctx = canvas.context();
                            ctx.set_fill_style_css(&format!(
                                "color({function} {v} 0.25 0.75)"
                            ))
                            .expect("a colour");
                            ctx.set_stroke_style_css(&format!(
                                "color({function} {v} 0.25 0.75)"
                            ))
                            .expect("a colour");
                            draw(ctx);
                            let got = pixel(
                                &mut canvas,
                                space,
                                SIDE / 2.0,
                                SIDE / 2.0,
                            );
                            (!holds(got, [v, 0.25, 0.75, 1.0], depth))
                                .then(|| format!("{v}: {got:?}"))
                        })
                        .collect();
                    if let Some(first) = missed.first() {
                        wrong.push(format!(
                            "gpu {gpu}, {space:?} {depth:?}, {name}: {} missed, first {first}",
                            missed.len()
                        ));
                    }
                }
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

fn has(bytes: &[u8], needle: &str) -> bool {
    bytes.windows(needle.len()).any(|w| w == needle.as_bytes())
}

/// A PDF and an SVG of such a canvas keep a flat colour plain: no pattern,
/// no image -- what the pixels' shader would have written.
#[test]
fn a_document_of_a_float_page_keeps_its_colours_plain() {
    for (space, function) in SPACES {
        let mut canvas = canvas(space, PixelDepth::F32, false);
        let ctx = canvas.context();
        ctx.set_fill_style_css(&format!("color({function} 0.5 0.25 0.75)"))
            .expect("a colour");
        ctx.fill_rect(4.0, 4.0, 16.0, 16.0);
        let pdf = canvas
            .to_buffer(ImageFormat::Pdf, &EncodeOptions::default())
            .expect("a pdf");
        assert!(!has(&pdf, "/Pattern"), "{space:?}: the PDF holds a pattern");
        assert!(
            !has(&pdf, "/Subtype /Image"),
            "{space:?}: the PDF holds an image"
        );
        let svg = canvas
            .to_buffer(ImageFormat::Svg, &EncodeOptions::default())
            .expect("an svg");
        assert!(!has(&svg, "<pattern"), "{space:?}: the SVG holds a pattern");
        assert!(!has(&svg, "<image"), "{space:?}: the SVG holds an image");
    }
}

/// The alpha of every pixel of `canvas`, premultiplied F32.
fn alphas(canvas: &mut Canvas, space: PixelColorSpace) -> Vec<f32> {
    let data = canvas
        .context()
        .get_image_data_as(
            0.0,
            0.0,
            TEXT_SIZE.0,
            TEXT_SIZE.1,
            PixelExportOptions {
                depth: PixelDepth::F32,
                color_space: space,
                premultiplied: true,
                ..PixelExportOptions::default()
            },
        )
        .expect("pixels");
    data.pixels()
        .as_chunks::<16>()
        .0
        .iter()
        .map(|px| f32::from_le_bytes([px[12], px[13], px[14], px[15]]))
        .collect()
}

/// The canvas the text pin draws on, wide enough for its run.
const TEXT_SIZE: (f32, f32) = (96.0, 64.0);

/// A run of text drawn in a flat colour.
fn text_on(space: PixelColorSpace, gpu: bool) -> Canvas {
    let mut canvas = Canvas::with_options(
        TEXT_SIZE.0,
        TEXT_SIZE.1,
        CanvasOptions {
            color_space: space,
            color_type: PixelDepth::F32,
            gpu,
            ..CanvasOptions::default()
        },
    )
    .expect("a canvas");
    let ctx = canvas.context();
    ctx.set_fill_style_css("#3366b3").expect("a colour");
    ctx.set_font_css("bold 40px sans-serif");
    ctx.fill_text("Wg", 4.0, 48.0, None);
    canvas
}

/// Text on such a canvas lays down the coverage it does on an sRGB one:
/// every pixel's alpha the same, on the run's first draw and on a draw of
/// the run kept from it.
///
/// Skia sets a glyph mask's contrast from the luminance of the paint's
/// colour, and reads a paint with an image shader as mid-grey, so text
/// painted through the exact colour's shader came out with heavier edges.
/// Only in a space with a transfer function, as sRGB has: in a linear one
/// Skia skips the gamma step the luminance feeds, and covers text unlike
/// sRGB by design.
#[test]
fn a_float_page_covers_text_as_srgb_does() {
    for gpu in ENGINES {
        for space in [PixelColorSpace::DisplayP3, PixelColorSpace::Rec2020] {
            // The first draw of the run on this thread lays it out; the
            // second draws it from the run kept.
            let first = alphas(&mut text_on(space, gpu), space);
            let srgb = alphas(
                &mut text_on(PixelColorSpace::Srgb, gpu),
                PixelColorSpace::Srgb,
            );
            let kept = alphas(&mut text_on(space, gpu), space);
            for (draw, alphas) in [("first", first), ("kept", kept)] {
                let parted =
                    alphas.iter().zip(&srgb).filter(|(a, b)| a != b).count();
                assert_eq!(
                    parted, 0,
                    "gpu {gpu}, {space:?}, {draw} draw: {parted} pixels covered unlike sRGB"
                );
            }
        }
    }
}

/// The images an SVG of a page embeds, as the base64 text it writes them
/// in: here, a fill and a blurred fill inside a translucent layer, which
/// the SVG backend draws as an image.
fn embedded(
    space: PixelColorSpace,
    depth: PixelDepth,
    css: &str,
) -> Vec<String> {
    let mut canvas = Canvas::with_options(
        SIDE,
        SIDE,
        CanvasOptions {
            color_space: space,
            color_type: depth,
            ..CanvasOptions::default()
        },
    )
    .expect("a canvas");
    let ctx = canvas.context();
    ctx.save_layer_with(0.5, None, None);
    ctx.set_fill_style_css(css).expect("a colour");
    ctx.fill_rect(2.0, 2.0, 12.0, 12.0);
    ctx.set_filter_css("blur(1px)").expect("a blur");
    ctx.fill_rect(18.0, 18.0, 12.0, 12.0);
    ctx.restore();
    let svg = canvas
        .to_buffer(ImageFormat::Svg, &EncodeOptions::default())
        .expect("an svg");
    String::from_utf8_lossy(&svg)
        .split("base64,")
        .skip(1)
        .map(|rest| rest.split('"').next().unwrap_or_default().to_string())
        .collect()
}

/// The image an SVG of such a canvas embeds is the one an eight-bit canvas
/// in that space embeds, byte for byte: the raster is eight-bit sRGB, and a
/// colour the canvas holds exactly is drawn there as the eight-bit canvas
/// draws its plain one.
#[test]
fn a_raster_in_a_document_of_a_float_page_is_an_eight_bit_pages() {
    let mut wrong = Vec::new();
    for (space, function) in SPACES {
        for depth in [PixelDepth::F32, PixelDepth::F16] {
            let parted = (0..=400)
                .step_by(10)
                .filter(|i| {
                    let v = *i as f32 / 400.0 * 1.2 - 0.1;
                    let css = format!("color({function} {v} 0.25 0.75)");
                    let images = embedded(space, depth, &css);
                    assert!(
                        !images.is_empty(),
                        "{space:?}: no raster embedded"
                    );
                    images != embedded(space, PixelDepth::Uint8, &css)
                })
                .count();
            if parted > 0 {
                wrong.push(format!(
                    "{space:?} {depth:?}: {parted} of 41 parted"
                ));
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}
