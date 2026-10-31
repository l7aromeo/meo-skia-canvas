//! A flat colour lands exactly on a float canvas in a space other than
//! sRGB, a document of that canvas keeps it a plain colour, and text on it
//! is covered as it is on an sRGB one.
//!
//! Skia stores a paint's colour in sRGB and converts it into the canvas's
//! space at the draw through skcms's approximate transfer function, whose
//! output lies on a grid 64 ulps wide, so no plain colour lands exactly
//! there. Such a canvas records each draw twice -- see
//! `page::Recorders` -- once for pixels, painting a flat colour through a
//! shader that holds it in the canvas's own space, or text through a colour
//! filter that does, and once for documents, painting it plain.

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

/// Every pixel of `canvas` its draws covered whole, premultiplied F32 in
/// the canvas's own space: those with an alpha of one.
///
/// Every pixel, rather than one known spot: where a glyph's ink falls
/// depends on the face a family resolves to, and a 96px bold "M" in the
/// face a Linux runner gives `sans-serif` leaves the canvas's middle bare.
fn covered(canvas: &mut Canvas, space: PixelColorSpace) -> Vec<[f32; 4]> {
    let data = canvas
        .context()
        .get_image_data_as(
            0.0,
            0.0,
            SIDE,
            SIDE,
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
        .map(|px| {
            [0, 1, 2, 3].map(|c| {
                f32::from_le_bytes([
                    px[c * 4],
                    px[c * 4 + 1],
                    px[c * 4 + 2],
                    px[c * 4 + 3],
                ])
            })
        })
        .filter(|px| px[3] == 1.0)
        .collect()
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

/// The ways a flat colour is drawn, each covering some of the canvas whole.
type Draw = fn(&mut Context2D);

const DRAWS: [(&str, Draw); 6] = [
    ("a fill", |ctx| ctx.fill_rect(0.0, 0.0, SIDE, SIDE)),
    ("text", |ctx| {
        ctx.set_font_css("bold 96px sans-serif");
        ctx.fill_text("M", -16.0, 64.0, None);
    }),
    ("stroked text", |ctx| {
        ctx.set_font_css("bold 96px sans-serif");
        ctx.set_line_width(SIDE);
        ctx.stroke_text("M", -16.0, 64.0, None);
    }),
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
/// back as the colour named in every pixel it covers whole, at every one of
/// 401 values of its first component from -0.1 to 1.1.
#[test]
fn a_flat_colour_lands_exactly_on_a_float_page() {
    let mut wrong = Vec::new();
    for gpu in ENGINES {
        for (space, function) in SPACES {
            for depth in [PixelDepth::F32, PixelDepth::F16] {
                for (name, draw) in DRAWS {
                    // Text and blurs are swept more thinly: each costs more
                    // to lay down than a rectangle.
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
                            let got = covered(&mut canvas, space);
                            match got.iter().find(|px| {
                                !holds(**px, [v, 0.25, 0.75, 1.0], depth)
                            }) {
                                _ if got.is_empty() => {
                                    Some(format!("{v}: nothing covered"))
                                }
                                Some(px) => Some(format!("{v}: {px:?}")),
                                None => None,
                            }
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

/// A drop-shadow in a colour named in the page's space reads back as that
/// colour, in every pixel it covers whole, on every space, depth and engine:
/// the shadow is tinted by a colour matrix whose offsets are the colour
/// converted on this side. Tinted through Skia's blend filter instead, which
/// converts the colour through skcms's approximate transfer function, it
/// landed up to 2.6e-4 from the colour on a Display P3 F32 page and a whole
/// half-float step off on F16.
#[test]
fn a_drop_shadow_lands_exactly_on_a_float_page() {
    let spaces = [(PixelColorSpace::Srgb, "srgb")].into_iter().chain(SPACES);
    let mut wrong = Vec::new();
    for gpu in ENGINES {
        for (space, function) in spaces.clone() {
            for depth in [PixelDepth::F32, PixelDepth::F16] {
                let missed: Vec<_> = (0..=400)
                    .step_by(20)
                    .filter_map(|i| {
                        let v = i as f32 / 400.0 * 1.2 - 0.1;
                        let mut canvas = canvas(space, depth, gpu);
                        let ctx = canvas.context();
                        // Cast onto the page from a square drawn off it.
                        ctx.set_filter_css(&format!(
                            "drop-shadow({SIDE}px 0 0 color({function} {v} 0.25 0.75))"
                        ))
                        .expect("a filter");
                        ctx.fill_rect(-SIDE, 0.0, SIDE, SIDE);
                        let got = covered(&mut canvas, space);
                        match got.iter().find(|px| {
                            !holds(**px, [v, 0.25, 0.75, 1.0], depth)
                        }) {
                            _ if got.is_empty() => {
                                Some(format!("{v}: nothing covered"))
                            }
                            Some(px) => Some(format!("{v}: {px:?}")),
                            None => None,
                        }
                    })
                    .collect();
                if let Some(first) = missed.first() {
                    wrong.push(format!(
                        "gpu {gpu}, {space:?} {depth:?}: {} missed, first {first}",
                        missed.len()
                    ));
                }
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// A drop-shadow in a colour named in another space than the page's is
/// the colour a fill of it paints, in every pixel it covers whole: both
/// convert it on this side, by the same arithmetic.
#[test]
fn a_drop_shadow_in_another_space_is_its_fill_on_a_float_page() {
    let mut wrong = Vec::new();
    for gpu in ENGINES {
        for (space, _) in SPACES {
            for depth in [PixelDepth::F32, PixelDepth::F16] {
                for i in (0..=400).step_by(40) {
                    let v = i as f32 / 400.0 * 1.2 - 0.1;
                    let colour = format!("color(srgb {v} 0.25 0.75)");
                    let mut shadow = canvas(space, depth, gpu);
                    let ctx = shadow.context();
                    ctx.set_filter_css(&format!(
                        "drop-shadow({SIDE}px 0 0 {colour})"
                    ))
                    .expect("a filter");
                    ctx.fill_rect(-SIDE, 0.0, SIDE, SIDE);
                    let mut fill = canvas(space, depth, gpu);
                    fill.context()
                        .set_fill_style_css(&colour)
                        .expect("a colour");
                    fill.context().fill_rect(0.0, 0.0, SIDE, SIDE);
                    let (got, want) = (
                        covered(&mut shadow, space),
                        covered(&mut fill, space),
                    );
                    if got.is_empty() || got != want {
                        wrong.push(format!(
                            "gpu {gpu}, {space:?} {depth:?}, {colour}: {:?} \
                             against {:?}",
                            got.first(),
                            want.first()
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

/// The colours the text pin draws in: one named in whole bytes, and two at
/// the edge of a contrast bucket, where Skia's rounding of the colour to
/// bytes crosses into the next. Skia sets a glyph mask's contrast from the
/// colour the paint's colour filter makes, read as sRGB, and each of these
/// held in the canvas's space and converted back to sRGB by Skia lands past
/// its edge, so text through a filter that hands it back so is covered
/// unlike the plain paint's. The first sits 2e-5 short of 63.5 / 255 in
/// green on either page. The second is one of three such colours on a
/// Rec. 2020 page that `ExactColour::dye` finds only if it reads Skia's
/// colour as Skia computes it: read through a colour shader instead, the
/// filter's stages round it into plain's bucket. Neither sits at 31.5 / 255:
/// CoreText's scaler halves the bucketed colour and buckets it again, which
/// merges that edge's two buckets into one.
const TEXT_COLOURS: [&str; 3] = [
    "#3366b3",
    "color(srgb 0.85 0.249 0.85)",
    "color(srgb 0.45 0.24897961 0.45)",
];

/// A run of text drawn in `css`.
fn text_on(
    space: PixelColorSpace,
    depth: PixelDepth,
    css: &str,
    gpu: bool,
) -> Canvas {
    let mut canvas = Canvas::with_options(
        TEXT_SIZE.0,
        TEXT_SIZE.1,
        CanvasOptions {
            color_space: space,
            color_type: depth,
            gpu,
            ..CanvasOptions::default()
        },
    )
    .expect("a canvas");
    let ctx = canvas.context();
    ctx.set_fill_style_css(css).expect("a colour");
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
    let mut wrong = Vec::new();
    for gpu in ENGINES {
        for space in [PixelColorSpace::DisplayP3, PixelColorSpace::Rec2020] {
            for depth in [PixelDepth::F32, PixelDepth::F16] {
                for css in TEXT_COLOURS {
                    let on = |space| text_on(space, depth, css, gpu);
                    // The first draw of the run on this thread lays it out;
                    // the second draws it from the run kept.
                    let first = alphas(&mut on(space), space);
                    let srgb = alphas(
                        &mut on(PixelColorSpace::Srgb),
                        PixelColorSpace::Srgb,
                    );
                    let kept = alphas(&mut on(space), space);
                    for (draw, alphas) in [("first", first), ("kept", kept)] {
                        let parted = alphas
                            .iter()
                            .zip(&srgb)
                            .filter(|(a, b)| a != b)
                            .count();
                        if parted > 0 {
                            wrong.push(format!(
                                "gpu {gpu}, {space:?} {depth:?}, {css}, {draw} draw: {parted} pixels covered unlike sRGB"
                            ));
                        }
                    }
                }
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// Text under a colour filter on such a canvas lays down the coverage it
/// does on an sRGB one. The filter here doubles the colour, which moves the
/// contrast-bucket edge at 63.5 / 255 down to a green of about 0.1245: one
/// the colour the dye makes would cross after the filter, though it sits in
/// plain's bucket before it, so a dye chosen by its own luminance alone
/// covered 352 pixels of this run unlike the plain paint.
#[test]
fn a_colour_filter_keeps_text_covered_as_srgb_does() {
    #[rustfmt::skip]
    let double = ColorFilter::matrix([
        2.0, 0.0, 0.0, 0.0, 0.0,
        0.0, 2.0, 0.0, 0.0, 0.0,
        0.0, 0.0, 2.0, 0.0, 0.0,
        0.0, 0.0, 0.0, 1.0, 0.0,
    ])
    .expect("a matrix");
    let css = "color(srgb 0.85 0.12445 0.85)";
    let on = |space, depth| {
        let mut canvas = Canvas::with_options(
            TEXT_SIZE.0,
            TEXT_SIZE.1,
            CanvasOptions {
                color_space: space,
                color_type: depth,
                ..CanvasOptions::default()
            },
        )
        .expect("a canvas");
        let ctx = canvas.context();
        ctx.set_color_filter(Some(&double));
        ctx.set_fill_style_css(css).expect("a colour");
        ctx.set_font_css("bold 40px sans-serif");
        ctx.fill_text("Wg", 4.0, 48.0, None);
        alphas(&mut canvas, space)
    };
    for depth in [PixelDepth::F32, PixelDepth::F16] {
        let srgb = on(PixelColorSpace::Srgb, depth);
        for space in [PixelColorSpace::DisplayP3, PixelColorSpace::Rec2020] {
            let parted = on(space, depth)
                .iter()
                .zip(&srgb)
                .filter(|(a, b)| a != b)
                .count();
            assert_eq!(
                parted, 0,
                "{space:?} {depth:?}: {parted} pixels covered unlike sRGB"
            );
        }
    }
}

/// A colour filter on the context applies to text's exact colour as to a
/// fill's: the filter's own output, not the colour it was handed.
#[test]
fn a_colour_filter_applies_to_text_on_a_float_page() {
    // Red and blue exchanged.
    #[rustfmt::skip]
    let swap = ColorFilter::matrix([
        0.0, 0.0, 1.0, 0.0, 0.0,
        0.0, 1.0, 0.0, 0.0, 0.0,
        1.0, 0.0, 0.0, 0.0, 0.0,
        0.0, 0.0, 0.0, 1.0, 0.0,
    ])
    .expect("a matrix");
    for (space, function) in SPACES {
        for (name, draw) in &DRAWS[..2] {
            let mut canvas = canvas(space, PixelDepth::F32, false);
            let ctx = canvas.context();
            ctx.set_color_filter(Some(&swap));
            ctx.set_fill_style_css(&format!("color({function} 0.2 0.25 0.75)"))
                .expect("a colour");
            draw(ctx);
            let got = covered(&mut canvas, space);
            assert!(!got.is_empty(), "{space:?}, {name}: nothing covered");
            for px in got {
                assert_eq!(px, [0.75, 0.25, 0.2, 1.0], "{space:?}, {name}");
            }
        }
    }
}

/// Every canvas space a page can be drawn into.
const ALL_SPACES: [PixelColorSpace; 8] = [
    PixelColorSpace::Srgb,
    PixelColorSpace::SrgbLinear,
    PixelColorSpace::DisplayP3,
    PixelColorSpace::DisplayP3Linear,
    PixelColorSpace::Rec2020,
    PixelColorSpace::Rec2020Linear,
    PixelColorSpace::Rec2020Pq,
    PixelColorSpace::Rec2020Hlg,
];

/// Such a canvas drawn into another canvas, of any space, paints its text
/// in the colour its fill takes there, to within the rounding of the two
/// conversions Skia makes into that space.
///
/// Onto sRGB, and only there, text paints the colour as the plain paint
/// holds it: see `ExactColour::dye`. A filter that took any other space
/// for sRGB would paint those sRGB numbers there as they stand, as much as
/// 0.61 off a Rec. 2020 canvas's own colour drawn into another.
#[test]
fn a_float_page_drawn_into_another_paints_its_text_as_its_fill() {
    let mut wrong = Vec::new();
    for (space, function) in SPACES {
        for into in ALL_SPACES {
            let [fill, text] = [&DRAWS[0], &DRAWS[1]].map(|(_, draw)| {
                let mut source = canvas(space, PixelDepth::F32, false);
                let ctx = source.context();
                ctx.set_fill_style_css(&format!(
                    "color({function} 0.2 0.6 0.35)"
                ))
                .expect("a colour");
                draw(ctx);
                let mut target = canvas(into, PixelDepth::F32, false);
                target.context().draw_canvas(&mut source, 0.0, 0.0);
                covered(&mut target, into)
            });
            let (Some(fill), false) = (fill.first(), text.is_empty()) else {
                wrong.push(format!("{space:?} into {into:?}: nothing covered"));
                continue;
            };
            let off = text
                .iter()
                .flat_map(|px| px.iter().zip(fill).map(|(t, f)| (t - f).abs()))
                .fold(0.0, f32::max);
            if off > 1e-3 {
                wrong.push(format!(
                    "{space:?} into {into:?}: text {off} off, first {:?}, fill {fill:?}",
                    text[0]
                ));
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}
