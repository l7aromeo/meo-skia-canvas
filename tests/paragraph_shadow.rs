//! A paragraph's text shadows are drawn as CSS Text Decoration 3 draws
//! `text-shadow`: each style's run of text on a line casts its shadow as one
//! element -- the exact Gaussian blur of the whole run, not of each glyph --
//! and the shadows composite in document order, beneath all the text, with a
//! style's first-listed shadow on top of the rest.
//!
//! The reference for a blurred shadow is the same shadow drawn sharp, with
//! no blur, convolved with the Gaussian sampled at whole-pixel offsets and
//! rounded once, written here apart from any code that draws a blur.

use meo_skia_canvas::prelude::*;

const SIZE: (f32, f32) = (240.0, 80.0);

/// Where the paragraph is drawn.
const AT: (f32, f32) = (10.0, 10.0);

/// How far a value may sit from a half-level and still round either way.
const TIE: f64 = 1e-3;

/// A shadow in an sRGB colour, at an offset, blurred by `sigma`.
fn shadow(rgb: (u8, u8, u8), dx: f32, dy: f32, sigma: f32) -> TextShadow {
    TextShadow {
        color: RgbaLinear::from_srgb8(rgb.0, rgb.1, rgb.2, 1.0),
        offset_x: dx,
        offset_y: dy,
        blur_sigma: sigma,
    }
}

/// A style of 24px sans-serif, its text transparent so that only its
/// shadows are drawn.
fn style(size: f32, shadows: Vec<TextShadow>) -> TextStyle {
    TextStyle {
        font_families: vec!["sans-serif".into()],
        font_size: size,
        color: RgbaLinear::from_srgb8(0, 0, 0, 0.0),
        shadows,
        ..TextStyle::default()
    }
}

/// A paragraph of `spans`, each its text in its own style.
fn paragraph(spans: &[(&str, TextStyle)]) -> Paragraph {
    let library = FontLibrary::new();
    let engine = TextEngine::new(&library);
    let mut builder = engine.paragraph_builder(&spans[0].1);
    for (text, style) in spans {
        builder.push_style(style);
        builder.add_text(text);
        builder.pop();
    }
    builder.build(1000.0)
}

/// `paragraph` drawn at [`AT`] on `gpu`'s engine, its pixels read back, or
/// `None` where that engine is not available.
fn drawn(gpu: bool, paragraph: &Paragraph) -> Option<Vec<[u8; 4]>> {
    let mut canvas = Canvas::new(SIZE.0, SIZE.1);
    canvas.set_gpu(gpu);
    if gpu && canvas.engine_kind() != EngineKind::Gpu {
        return None;
    }
    canvas.context().draw_paragraph(paragraph, AT.0, AT.1);
    let data = canvas
        .context()
        .get_image_data(0.0, 0.0, SIZE.0, SIZE.1)
        .expect("pixels");
    Some(
        data.pixels()
            .chunks(4)
            .map(|p| [p[0], p[1], p[2], p[3]])
            .collect(),
    )
}

/// `image`, `width` pixels wide, blurred by `sigma` with the Gaussian
/// sampled at whole-pixel offsets and normalised over its reach, one axis
/// and then the other, reading outside the image as nothing.
fn sampled(image: &[f64], width: usize, sigma: f64) -> Vec<f64> {
    let height = image.len() / width;
    let reach = (8.0 * sigma).ceil() as i64;
    let raw: Vec<f64> = (-reach..=reach)
        .map(|d| (-((d * d) as f64) / (2.0 * sigma * sigma)).exp())
        .collect();
    let sum: f64 = raw.iter().sum();
    let weights: Vec<f64> = raw.iter().map(|w| w / sum).collect();
    let pass = |source: &[f64], across: bool| -> Vec<f64> {
        (0..source.len())
            .map(|i| {
                let (x, y) = ((i % width) as i64, (i / width) as i64);
                weights
                    .iter()
                    .zip(-reach..)
                    .filter_map(|(weight, d)| {
                        let (sx, sy) = match across {
                            true => (x + d, y),
                            false => (x, y + d),
                        };
                        ((0..width as i64).contains(&sx)
                            && (0..height as i64).contains(&sy))
                        .then(|| {
                            weight * source[(sy * width as i64 + sx) as usize]
                        })
                    })
                    .sum()
            })
            .collect()
    };
    pass(&pass(image, true), false)
}

/// The alpha of each pixel, as a share of one.
fn alpha(pixels: &[[u8; 4]]) -> Vec<f64> {
    pixels.iter().map(|p| f64::from(p[3]) / 255.0).collect()
}

/// The pixels whose alpha is not the reference's rounded level, but for
/// those within [`TIE`] of a half-level that land on either neighbour.
fn misses(got: &[[u8; 4]], reference: &[f64]) -> Vec<(usize, u8, f64)> {
    got.iter()
        .zip(reference)
        .enumerate()
        .filter(|(_, (p, r))| {
            let level = **r * 255.0;
            let tie = (level.fract() - 0.5).abs() < TIE;
            let near = (f64::from(p[3]) - level).abs() < 0.5 + TIE;
            f64::from(p[3]) != level.round() && !(tie && near)
        })
        .map(|(i, (p, r))| (i, p[3], r * 255.0))
        .collect()
}

#[test]
fn a_paragraph_shadow_is_the_exact_gaussian_of_its_sharp_shadow() {
    // One style in one face on one line: one element, whose shadow is the
    // blur of the whole run. Blurring each glyph alone loses ink where two
    // glyphs' blurs meet, and a mask filter draws a glyph heavier than the
    // glyph itself on macOS.
    let shadowed = |sigma: f32| {
        paragraph(&[(
            "Hamburg",
            style(24.0, vec![shadow((0, 0, 0), 1.0, 0.0, sigma)]),
        )])
    };
    for gpu in [false, true] {
        let Some(sharp) = drawn(gpu, &shadowed(0.0)) else {
            continue;
        };
        for sigma in [2.0f32, 5.0, 8.0] {
            let reference =
                sampled(&alpha(&sharp), SIZE.0 as usize, f64::from(sigma));
            let got = drawn(gpu, &shadowed(sigma)).expect("pixels");
            let misses = misses(&got, &reference);
            assert!(
                misses.is_empty(),
                "gpu {gpu} sigma {sigma}: {} pixels off the exact blur, \
                 first {:?}",
                misses.len(),
                &misses[..misses.len().min(4)],
            );
        }
    }
}

/// The pixels where both `a` and `b` are opaque.
fn both_opaque(a: &[[u8; 4]], b: &[[u8; 4]]) -> Vec<usize> {
    (0..a.len())
        .filter(|&i| a[i][3] == 255 && b[i][3] == 255)
        .collect()
}

/// How many of `at` in `pixels` are not opaque `rgb`.
fn not(pixels: &[[u8; 4]], at: &[usize], rgb: (u8, u8, u8)) -> usize {
    at.iter()
        .filter(|&&i| pixels[i] != [rgb.0, rgb.1, rgb.2, 255])
        .count()
}

const RED: (u8, u8, u8) = (255, 0, 0);
const BLUE: (u8, u8, u8) = (0, 0, 255);

#[test]
fn a_styles_first_listed_shadow_lies_on_top() {
    // CSS Backgrounds 3 and Text Decoration 3 paint a list of shadows with
    // the first on top, as `box-shadow` and `text-shadow` do in Chrome.
    let red = shadow(RED, 2.0, 1.0, 0.0);
    let blue = shadow(BLUE, 3.0, 1.0, 0.0);
    for gpu in [false, true] {
        let one = |shadows| paragraph(&[("Hm", style(48.0, shadows))]);
        let (Some(both), Some(reds), Some(blues)) = (
            drawn(gpu, &one(vec![red, blue])),
            drawn(gpu, &one(vec![red])),
            drawn(gpu, &one(vec![blue])),
        ) else {
            continue;
        };
        let overlap = both_opaque(&reds, &blues);
        assert!(overlap.len() > 50, "{} pixels overlap", overlap.len());
        assert_eq!(not(&both, &overlap, RED), 0, "gpu {gpu}: red is on top");
    }
}

#[test]
fn a_later_spans_shadow_lies_over_an_earlier_spans() {
    // Document order: the second span's shadow over the first's, where the
    // first's is cast across it.
    let spans = |first: Vec<TextShadow>, second: Vec<TextShadow>| {
        paragraph(&[("Hm", style(48.0, first)), ("Hm", style(48.0, second))])
    };
    let red = shadow(RED, 30.0, 0.0, 0.0);
    let blue = shadow(BLUE, 1.0, 0.0, 0.0);
    for gpu in [false, true] {
        let (Some(both), Some(reds), Some(blues)) = (
            drawn(gpu, &spans(vec![red], vec![blue])),
            drawn(gpu, &spans(vec![red], vec![])),
            drawn(gpu, &spans(vec![], vec![blue])),
        ) else {
            continue;
        };
        let overlap = both_opaque(&reds, &blues);
        assert!(overlap.len() > 50, "{} pixels overlap", overlap.len());
        assert_eq!(not(&both, &overlap, BLUE), 0, "gpu {gpu}: blue is on top");
    }
}

#[test]
fn a_later_lines_shadow_lies_over_an_earlier_lines() {
    let spans = |first: Vec<TextShadow>, second: Vec<TextShadow>| {
        paragraph(&[("Hm\n", style(24.0, first)), ("Hm", style(24.0, second))])
    };
    let red = shadow(RED, 0.0, 24.0, 0.0);
    let blue = shadow(BLUE, 0.0, 1.0, 0.0);
    for gpu in [false, true] {
        let (Some(both), Some(reds), Some(blues)) = (
            drawn(gpu, &spans(vec![red], vec![blue])),
            drawn(gpu, &spans(vec![red], vec![])),
            drawn(gpu, &spans(vec![], vec![blue])),
        ) else {
            continue;
        };
        let overlap = both_opaque(&reds, &blues);
        assert!(overlap.len() > 20, "{} pixels overlap", overlap.len());
        assert_eq!(not(&both, &overlap, BLUE), 0, "gpu {gpu}: blue is on top");
    }
}

/// How far a float page's alpha may sit from the exact composite: the f32
/// a blur is narrowed to and a layer composites in, well under a level.
const FLOAT_NOISE: f64 = 1e-4;

/// `paragraph` drawn at [`AT`] on an F32 page on `gpu`'s engine, its
/// premultiplied alpha read back, or `None` where that engine is not
/// available.
fn drawn_float(gpu: bool, paragraph: &Paragraph) -> Option<Vec<f64>> {
    let mut canvas = Canvas::with_options(
        SIZE.0,
        SIZE.1,
        CanvasOptions {
            color_type: PixelDepth::F32,
            gpu,
            ..CanvasOptions::default()
        },
    )
    .expect("a canvas");
    if gpu && canvas.engine_kind() != EngineKind::Gpu {
        return None;
    }
    canvas.context().draw_paragraph(paragraph, AT.0, AT.1);
    let data = canvas
        .context()
        .get_image_data_as(
            0.0,
            0.0,
            SIZE.0,
            SIZE.1,
            PixelExportOptions {
                depth: PixelDepth::F32,
                premultiplied: true,
                ..PixelExportOptions::default()
            },
        )
        .expect("pixels");
    Some(
        data.pixels()
            .chunks(16)
            .map(|p| {
                f64::from(f32::from_le_bytes([p[12], p[13], p[14], p[15]]))
            })
            .collect(),
    )
}

#[test]
fn neighbouring_spans_blur_apart_and_composite_in_order() {
    // Each span's shadow is the blur of its own run, and the second lies
    // over the first's tail where the two meet: alpha `b + a (1 - b)`. On
    // a float page, where nothing rounds them to a level, so that the
    // composite is held to the exact one and not to whatever Skia's eight
    // bit source-over makes of two rounded blurs.
    let sigma = 4.0f32;
    let spans = |first: Vec<TextShadow>, second: Vec<TextShadow>| {
        paragraph(&[("Hm", style(24.0, first)), ("Hm", style(24.0, second))])
    };
    for gpu in [false, true] {
        // Each sharp in the colour its span casts in: under FreeType a
        // glyph's coverage follows its colour's luminance (Skia's mask
        // preblend), so a red mask is not the blue one the blur is of.
        let sharp = |first: bool| match first {
            true => drawn_float(
                gpu,
                &spans(vec![shadow(RED, 1.0, 0.0, 0.0)], vec![]),
            ),
            false => drawn_float(
                gpu,
                &spans(vec![], vec![shadow(BLUE, 1.0, 0.0, 0.0)]),
            ),
        };
        let (Some(first), Some(second)) = (sharp(true), sharp(false)) else {
            continue;
        };
        let blur =
            |alpha: &[f64]| sampled(alpha, SIZE.0 as usize, f64::from(sigma));
        let (a, b) = (blur(&first), blur(&second));
        let got = drawn_float(
            gpu,
            &spans(
                vec![shadow(RED, 1.0, 0.0, sigma)],
                vec![shadow(BLUE, 1.0, 0.0, sigma)],
            ),
        )
        .expect("pixels");
        let off: Vec<_> = got
            .iter()
            .zip(a.iter().zip(&b))
            .enumerate()
            .filter(|(_, (g, (a, b)))| {
                (**g - (**b + **a * (1.0 - **b))).abs() > FLOAT_NOISE
            })
            .map(|(i, (g, (a, b)))| (i, *g, *a, *b))
            .collect();
        assert!(
            off.is_empty(),
            "gpu {gpu}: {} pixels off the exact composite, first {:?}",
            off.len(),
            &off[..off.len().min(4)],
        );
    }
}

/// The most one rounding to the nearest level moves a value, in levels.
const ROUNDING: f64 = 0.5;

#[test]
fn neighbouring_spans_composite_within_three_eight_bit_roundings() {
    // On an eight-bit page each span's blurred shadow is stored at a level
    // before the next is composited over it, and Skia's lowp source-over is
    // `s + div255_accurate(d * (255 - s))` (SkRasterPipeline_opts.h; the
    // inaccurate `div255` is used only under SK_USE_INACCURATE_DIV255_IN_BLEND,
    // which no build of ours defines), itself rounded to the nearest level.
    // With `s` and `d` the second and first span's stored levels, off their
    // exact blurs `255 B` and `255 A` by `e_s` and `e_d`, and `r` the blend's
    // rounding, the drawn level misses the exact `255 (B + A (1 - B))` by
    //
    //     e_s (1 - A) + e_d (1 - s / 255) + r,
    //
    // so by at most half a level for each rounding, weighted as above: under
    // 1.5 levels, and less wherever either shadow is strong. The float page
    // has none of these roundings and is held to the exact composite in the
    // test above.
    let sigma = 4.0f32;
    let spans = |first: Vec<TextShadow>, second: Vec<TextShadow>| {
        paragraph(&[("Hm", style(24.0, first)), ("Hm", style(24.0, second))])
    };
    for gpu in [false, true] {
        // Each sharp in the colour its span casts in: under FreeType a
        // glyph's coverage follows its colour's luminance (Skia's mask
        // preblend), so a red mask is not the blue one the blur is of.
        let sharp = |first: bool| match first {
            true => {
                drawn(gpu, &spans(vec![shadow(RED, 1.0, 0.0, 0.0)], vec![]))
            }
            false => {
                drawn(gpu, &spans(vec![], vec![shadow(BLUE, 1.0, 0.0, 0.0)]))
            }
        };
        let (Some(first), Some(second)) = (sharp(true), sharp(false)) else {
            continue;
        };
        let blur =
            |alpha: &[f64]| sampled(alpha, SIZE.0 as usize, f64::from(sigma));
        let (a, b) = (blur(&alpha(&first)), blur(&alpha(&second)));
        let second_alone =
            drawn(gpu, &spans(vec![], vec![shadow(BLUE, 1.0, 0.0, sigma)]))
                .expect("pixels");
        let got = drawn(
            gpu,
            &spans(
                vec![shadow(RED, 1.0, 0.0, sigma)],
                vec![shadow(BLUE, 1.0, 0.0, sigma)],
            ),
        )
        .expect("pixels");
        let off: Vec<_> = got
            .iter()
            .zip(&second_alone)
            .zip(a.iter().zip(&b))
            .enumerate()
            .filter_map(|(i, ((g, s), (a, b)))| {
                let exact = (b + a * (1.0 - b)) * 255.0;
                let stored = f64::from(s[3]) / 255.0;
                let bound =
                    ROUNDING * (1.0 - a) + ROUNDING * (1.0 - stored) + ROUNDING;
                let miss = (f64::from(g[3]) - exact).abs();
                (miss > bound + TIE).then_some((i, g[3], exact, bound))
            })
            .collect();
        assert!(
            off.is_empty(),
            "gpu {gpu}: {} pixels beyond three roundings of the exact \
             composite, first {:?}",
            off.len(),
            &off[..off.len().min(4)],
        );
    }
}

#[test]
fn a_style_set_in_two_faces_casts_the_blur_of_the_whole_run() {
    // Oswald has no omega and Raleway, next in the families, has, so
    // "AbΩΩCd" is three glyph runs in one style -- `context2d`'s
    // `a_style_set_in_two_faces_casts_one_shadow` holds that -- and its
    // shadow is the blur of all three, not three blurs laid over one
    // another, which lose ink where their tails meet. Both faces are
    // bundled, so the split is the same wherever the test runs.
    let library = FontLibrary::new();
    for (family, file) in [
        ("ShadowSplitOswald", "Oswald/Oswald-VariableFont_wght.ttf"),
        (
            "ShadowSplitRaleway",
            "Raleway/Raleway-VariableFont_wght.ttf",
        ),
    ] {
        library
            .register_font_from_path(
                family,
                format!("tests/assets/fonts/{file}"),
            )
            .expect("a bundled face registers");
    }
    let shadowed = |sigma: f32| {
        let mut style = style(36.0, vec![shadow((0, 0, 0), 1.0, 0.0, sigma)]);
        style.font_families =
            vec!["ShadowSplitOswald".into(), "ShadowSplitRaleway".into()];
        paragraph(&[("AbΩΩCd", style)])
    };
    for gpu in [false, true] {
        let Some(sharp) = drawn(gpu, &shadowed(0.0)) else {
            continue;
        };
        for sigma in [3.0f32, 8.0] {
            let reference =
                sampled(&alpha(&sharp), SIZE.0 as usize, f64::from(sigma));
            let got = drawn(gpu, &shadowed(sigma)).expect("pixels");
            let misses = misses(&got, &reference);
            assert!(
                misses.is_empty(),
                "gpu {gpu} sigma {sigma}: {} pixels off the exact blur, \
                 first {:?}",
                misses.len(),
                &misses[..misses.len().min(4)],
            );
        }
    }
}
