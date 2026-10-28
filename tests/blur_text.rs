//! Text under a `blur()` filter is the blur of the drawn text, as it is in
//! Chrome and as the Gaussian defines it: it keeps the text's ink, and it
//! lies within a few levels of the sharp text blurred exactly.
//!
//! Skia's coverage blur of text is not that. It blurs each glyph's mask on
//! its own and draws the glyphs over one another, so where their blurs
//! overlap they combine as `1 - (1 - a)(1 - b)` rather than adding -- on
//! the CPU engine the cases below lost up to 35% of their ink that way.

mod exact;

use exact::gaussian;
use meo_skia_canvas::prelude::*;

const OSWALD: &str = "tests/assets/fonts/Oswald/Oswald-VariableFont_wght.ttf";
const FAMILY: &str = "BlurTextOswald";
const TEXT: &str = "Hamburgefonstiv";

/// Text sizes and sigmas, in pixels, on the CPU engine, where blurring
/// each glyph alone kept 65%, 97% and 91% of the ink and came 9.0, 10.3
/// and 6.7 levels from exact.
const CPU_CASES: [(f32, f32); 3] = [(16.0, 20.0), (48.0, 10.0), (48.0, 24.0)];

/// Text sizes and sigmas on the GPU engine, where blurring each glyph alone
/// came 29.8, 20.0 and 8.8 levels from exact. Not the CPU's: from a sigma
/// of 10 at 48px the GPU's image blur, which the blurred result takes, is
/// itself up to 10 levels off and 4.7% heavy, and no nearer than the glyphs
/// blurred one by one.
const GPU_CASES: [(f32, f32); 3] = [(16.0, 2.0), (48.0, 5.0), (16.0, 20.0)];

/// How much of the sharp text's ink its blur keeps at the least. A
/// Gaussian keeps all of it, and the canvas is wide enough that the blur's
/// tails stay on it; the blurred result kept 99.5% or more of it in every
/// case here.
const INK_KEPT: f64 = 0.99;

/// How far, in levels of an 8-bit alpha, the blur may be from the sharp
/// text blurred exactly. The blurred result was 1.3 to 3.8 levels away in
/// these cases.
const EXACT_LEVELS: f64 = 5.0;

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
    for (gpu, cases) in [(false, CPU_CASES), (true, GPU_CASES)] {
        for (em, sigma) in cases {
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
