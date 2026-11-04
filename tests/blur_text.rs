//! Text under a `blur()` filter is the blur of the drawn text, as the
//! Gaussian defines it: it keeps the text's ink, and it is the sharp text
//! blurred exactly, rounded once, as a blurred path is. Chrome's is three box
//! blurs, up to 12.7 levels off -- see `docs/browser-differences.md`.
//!
//! Skia's coverage blur of text is not that. It blurs each glyph's mask on
//! its own and draws the glyphs over one another, so where their blurs
//! overlap they combine as `1 - (1 - a)(1 - b)` rather than adding -- on
//! the CPU engine the cases below lost up to 35% of their ink that way.

// Each test file uses part of it.
#[allow(dead_code)]
mod exact;

use exact::{gaussian, misses, sampled};
use meo_skia_canvas::prelude::*;

const OSWALD: &str = "tests/assets/fonts/Oswald/Oswald-VariableFont_wght.ttf";
const FAMILY: &str = "BlurTextOswald";
const TEXT: &str = "Hamburgefonstiv";

/// Text sizes and sigmas, in pixels, held on both engines. Blurring each
/// glyph alone, on the CPU at 16px by 20, 48px by 10 and 48px by 24, kept
/// 65%, 97% and 91% of the ink and came 9.0, 10.3 and 6.7 levels from
/// exact; on the GPU at 16px by 2, 48px by 5 and 16px by 20 it came 29.8,
/// 20.0 and 8.8 levels from exact.
const CASES: [(f32, f32); 5] = [
    (16.0, 2.0),
    (16.0, 20.0),
    (48.0, 5.0),
    (48.0, 10.0),
    (48.0, 24.0),
];

/// How much of the sharp text's ink its blur keeps at the least. A
/// Gaussian keeps all of it, and the canvas is wide enough that the blur's
/// tails stay on it; the blurred result kept 99.15% of it at 16px by 20,
/// where the tails are widest against the text, and 99.8% or more in every
/// other case here.
const INK_KEPT: f64 = 0.99;

/// How far, in levels of an 8-bit alpha, the blur may be from the sharp
/// text blurred by the Gaussian's weight over each pixel. The blurred
/// result is the Gaussian sampled at whole pixels and rounded once -- see
/// `blurred_text_is_the_exact_gaussian_of_the_sharp_text` -- and was 0.50
/// to 1.34 levels from this one in these cases on both engines, the most
/// at a sigma of 2, where sampling and the weight over a pixel part most.
const EXACT_LEVELS: f64 = 2.0;

/// How many sigmas of the blur fit between the text and each edge of the
/// canvas: past four, a Gaussian's tail holds under a hundred-thousandth
/// of its weight.
const MARGIN_SIGMAS: f32 = 4.0;

fn alpha(canvas: &mut Canvas) -> Vec<f64> {
    canvas
        .to_image_data(None, &ImageDataExport::default())
        .expect("pixels")
        .pixels()
        .chunks(4)
        .map(|p| f64::from(p[3]))
        .collect()
}

#[test]
fn blurred_text_keeps_its_ink_and_blurs_as_a_whole() {
    FontLibrary::new()
        .use_paths(Some(FAMILY), [OSWALD])
        .expect("registers");
    for gpu in [false, true] {
        for (em, sigma) in CASES {
            let margin = (MARGIN_SIGMAS * sigma).ceil();
            let mut probe = Canvas::new(1.0, 1.0);
            let ctx = probe.context();
            assert!(ctx.set_font_css(&format!("{em}px {FAMILY}")));
            let advance = ctx.measure_text(TEXT, None).width;
            let (width, height) = (
                (advance + 2.0 * margin).ceil(),
                (2.0 * em + 2.0 * margin).ceil(),
            );
            let drawn = |filter: &str| -> Option<Vec<f64>> {
                let mut canvas = Canvas::new(width, height);
                canvas.set_gpu(gpu);
                if gpu && canvas.engine_kind() != EngineKind::Gpu {
                    return None;
                }
                let ctx = canvas.context();
                assert!(ctx.set_font_css(&format!("{em}px {FAMILY}")));
                ctx.set_filter_css(filter).expect("a filter");
                ctx.fill_text(TEXT, margin, margin + 1.5 * em, None);
                Some(alpha(&mut canvas))
            };
            let Some(sharp) = drawn("none") else {
                continue;
            };
            let blurred = drawn(&format!("blur({sigma}px)")).expect("pixels");
            let ink = |a: &[f64]| a.iter().sum::<f64>();
            let kept = ink(&blurred) / ink(&sharp);
            assert!(
                kept >= INK_KEPT,
                "gpu {gpu} {em}px blurred by {sigma}: kept {kept:.3} of its ink"
            );
            let off = gaussian(&sharp, width as usize, f64::from(sigma))
                .iter()
                .zip(&blurred)
                .map(|(e, b)| (e - b).abs())
                .fold(0.0, f64::max);
            assert!(
                off <= EXACT_LEVELS,
                "gpu {gpu} {em}px blurred by {sigma}: {off:.1} levels from exact"
            );
        }
    }
}

/// Text sizes and sigmas the exact blur is held to on both engines: a
/// small sigma under the FFT's crossover, the 40px row of the bench, and a
/// wide one.
const EXACT_CASES: [(f32, f32); 3] = [(16.0, 2.0), (40.0, 3.0), (48.0, 10.0)];

/// `TEXT` at `em` pixels in black on a canvas `margin` wider than it on
/// every side, through `filter`, on `gpu`'s engine, as its alpha bytes and
/// width; `None` where that engine is not available.
fn text_alpha(
    gpu: bool,
    em: f32,
    margin: f32,
    filter: &str,
) -> Option<(Vec<u8>, usize)> {
    let mut probe = Canvas::new(1.0, 1.0);
    assert!(probe.context().set_font_css(&format!("{em}px {FAMILY}")));
    let advance = probe.context().measure_text(TEXT, None).width;
    let (width, height) = (
        (advance + 2.0 * margin).ceil(),
        (2.0 * em + 2.0 * margin).ceil(),
    );
    let mut canvas = Canvas::new(width, height);
    canvas.set_gpu(gpu);
    if gpu && canvas.engine_kind() != EngineKind::Gpu {
        return None;
    }
    let ctx = canvas.context();
    assert!(ctx.set_font_css(&format!("{em}px {FAMILY}")));
    ctx.set_filter_css(filter).expect("a filter");
    // Off the pixel grid, so the glyphs' own antialiasing is in what is
    // blurred.
    ctx.fill_text(TEXT, margin + 0.3, margin + 1.5 * em + 0.6, None);
    let pixels = canvas
        .to_image_data(None, &ImageDataExport::default())
        .expect("pixels");
    Some((
        pixels.pixels().chunks(4).map(|p| p[3]).collect(),
        width as usize,
    ))
}

/// Text under `blur()` is the exact Gaussian of the text drawn sharp: its
/// own glyphs, rasterised as they are without the filter, convolved with
/// the Gaussian sampled at whole pixels and rounded once, as a blurred path
/// is. Skia's image filter, which Chrome draws text under `blur()` through,
/// approximates the Gaussian with three box blurs and came up to 12.7
/// levels from it on 40px text blurred by 3.
#[test]
fn blurred_text_is_the_exact_gaussian_of_the_sharp_text() {
    FontLibrary::new()
        .use_paths(Some(FAMILY), [OSWALD])
        .expect("registers");
    for gpu in [false, true] {
        for (em, sigma) in EXACT_CASES {
            let margin = (MARGIN_SIGMAS * sigma).ceil();
            let Some((sharp, width)) = text_alpha(gpu, em, margin, "none")
            else {
                continue;
            };
            let (blurred, _) =
                text_alpha(gpu, em, margin, &format!("blur({sigma}px)"))
                    .expect("pixels");
            let sharp: Vec<f64> =
                sharp.iter().map(|a| f64::from(*a) / 255.0).collect();
            let misses =
                misses(&blurred, &sampled(&sharp, width, f64::from(sigma)));
            assert!(
                misses.is_empty(),
                "gpu {gpu} {em}px blurred by {sigma}: {} pixels off the exact blur, first {:?}",
                misses.len(),
                &misses[..misses.len().min(4)],
            );
        }
    }
}

/// Text sizes and sigmas the coverage route takes, for
/// `blurred_text_is_its_colour_over_the_blurred_coverage`: sigmas under the
/// FFT's crossover (`exact_blur::FFT_FROM_SIGMA`, 5), which the route leaves
/// to the chain because the FFT pairs a plane's rows.
const COVERAGE_CASES: [(f32, f32); 3] = [(16.0, 2.0), (40.0, 3.0), (48.0, 4.5)];

/// Colours a run is blurred in: as CSS, the same colour opaque, and the
/// premultiplied bytes a fill of it stores on an eight-bit sRGB page --
/// each channel times the alpha, and the alpha, times 255, none of them on
/// a tie. Not black: its colour channels are zero whatever the route, so it
/// cannot tell a colour blurred with its coverage from one written after
/// the blur. Skia covers a glyph differently in each (see
/// `a_runs_coverage_depends_on_the_colour_it_is_drawn_in`), so each is
/// compared with its own sharp text, drawn opaque: Skia pre-blends a mask by
/// the colour and not its alpha, so the opaque run covers what the
/// translucent one does.
const COLOURS: [(&str, &str, [u8; 4]); 5] = [
    ("rgb(200, 80, 30)", "rgb(200, 80, 30)", [200, 80, 30, 255]),
    (
        "rgb(30, 160, 220)",
        "rgb(30, 160, 220)",
        [30, 160, 220, 255],
    ),
    (
        "rgb(255, 255, 255)",
        "rgb(255, 255, 255)",
        [255, 255, 255, 255],
    ),
    (
        "rgba(200, 80, 30, 0.6)",
        "rgb(200, 80, 30)",
        [120, 48, 18, 153],
    ),
    (
        "rgba(30, 160, 220, 0.4)",
        "rgb(30, 160, 220)",
        [12, 64, 88, 102],
    ),
];

/// Coloured text under `blur()` on an eight-bit CPU page is the exact blur
/// of its own coverage times the bytes a fill of its colour stores, each
/// premultiplied channel and the alpha rounded once: the colour and the
/// alpha are applied to the blurred coverage, not blurred out of bytes the
/// sharp text already rounded.
#[test]
fn blurred_text_is_its_colour_over_the_blurred_coverage() {
    FontLibrary::new()
        .use_paths(Some(FAMILY), [OSWALD])
        .expect("registers");
    let mut failures = Vec::new();
    for (em, sigma) in COVERAGE_CASES {
        let margin = (MARGIN_SIGMAS * sigma).ceil();
        for (css, opaque, fill) in COLOURS {
            let (sharp, width) = text_premultiplied(em, margin, "none", opaque);
            let (blurred, _) = text_premultiplied(
                em,
                margin,
                &format!("blur({sigma}px)"),
                css,
            );
            let coverage: Vec<f64> =
                sharp.chunks(4).map(|p| f64::from(p[3]) / 255.0).collect();
            let spread = sampled(&coverage, width, f64::from(sigma));
            for (k, byte) in fill.iter().enumerate() {
                let reference: Vec<f64> = spread
                    .iter()
                    .map(|a| a * f64::from(*byte) / 255.0)
                    .collect();
                let got: Vec<u8> = blurred.chunks(4).map(|p| p[k]).collect();
                let off = misses(&got, &reference);
                if !off.is_empty() {
                    failures.push(format!(
                        "{css} {em}px blurred by {sigma}: channel {k} {} pixels off the blurred coverage times {byte}, first {:?}",
                        off.len(),
                        &off[..off.len().min(4)],
                    ));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// `TEXT` at `em` pixels in the colour `css` on an eight-bit sRGB CPU canvas
/// `margin` wider than it on every side, through `filter`, as premultiplied
/// RGBA bytes and its width.
fn text_premultiplied(
    em: f32,
    margin: f32,
    filter: &str,
    css: &str,
) -> (Vec<u8>, usize) {
    let mut probe = Canvas::new(1.0, 1.0);
    assert!(probe.context().set_font_css(&format!("{em}px {FAMILY}")));
    let advance = probe.context().measure_text(TEXT, None).width;
    let (width, height) = (
        (advance + 2.0 * margin).ceil(),
        (2.0 * em + 2.0 * margin).ceil(),
    );
    let mut canvas = Canvas::new(width, height);
    canvas.set_gpu(false);
    let ctx = canvas.context();
    assert!(ctx.set_font_css(&format!("{em}px {FAMILY}")));
    ctx.set_fill_style_css(css).expect("a colour");
    ctx.set_filter_css(filter).expect("a filter");
    ctx.fill_text(TEXT, margin + 0.3, margin + 1.5 * em + 0.6, None);
    let options = ImageDataExport {
        pixels: PixelExportOptions {
            premultiplied: true,
            ..PixelExportOptions::default()
        },
        ..ImageDataExport::default()
    };
    let pixels = canvas.to_image_data(None, &options).expect("pixels");
    (pixels.pixels().to_vec(), width as usize)
}
