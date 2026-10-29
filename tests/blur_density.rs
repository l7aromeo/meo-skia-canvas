//! A blur keeps its width, in canvas pixels, whatever the drawing is
//! replayed at: an export at a density, or a canvas drawn into another at a
//! size. The reference is the exact Gaussian blur of the rectangle -- its
//! indicator convolved with a Gaussian of standard deviation `sigma`,
//! averaged over each pixel -- at the scaled geometry, written in these tests
//! apart from any code that draws a blur.

mod exact;

use exact::{erf, gaussian};
use meo_skia_canvas::prelude::*;

/// The interval `[a, b]` blurred by `sigma`, averaged over each of `n`
/// pixels from zero.
fn profile(n: usize, a: f64, b: f64, sigma: f64) -> Vec<f64> {
    let k = sigma * std::f64::consts::SQRT_2;
    // An antiderivative of `erf`.
    let g = |u: f64| u * erf(u) + (-u * u).exp() / std::f64::consts::PI.sqrt();
    let integral = |x: f64| k / 2.0 * (g((x - a) / k) - g((x - b) / k));
    (0..n)
        .map(|i| integral(i as f64 + 1.0) - integral(i as f64))
        .collect()
}

/// The largest difference, in levels of an 8-bit alpha, between `alpha`
/// and the exact blur of `rect` at `sigma` on a `size` canvas.
fn worst(
    alpha: &[u8],
    size: (usize, usize),
    rect: [f64; 4],
    sigma: f64,
) -> f64 {
    let [x, y, w, h] = rect;
    let across = profile(size.0, x, x + w, sigma);
    let down = profile(size.1, y, y + h, sigma);
    alpha
        .iter()
        .enumerate()
        .map(|(i, &a)| {
            (f64::from(a) - across[i % size.0] * down[i / size.0] * 255.0).abs()
        })
        .fold(0.0, f64::max)
}

fn alpha(data: &ImageData) -> Vec<u8> {
    data.pixels().chunks(4).map(|p| p[3]).collect()
}

/// How much further from the exact blur a replay at another scale may come
/// than the same drawing does at its own: the coverage blur's error moves
/// with where its edges fall within a pixel, by a level and a fraction
/// between density 1 and 2 at sigma 1 and 2. A sigma left in device
/// pixels misses by 60 to 100. Also how far a rounded rectangle exported at
/// density 2 may be from the same drawing made twice as large, which on
/// macOS measured no distance at all on either engine.
const REPLAY_MARGIN: f64 = 2.0;

/// How far blurred text exported at density 2 may be from the same text set
/// twice as large. The glyphs themselves differ: FreeType rasterizes a
/// 20px glyph under a doubling transform and a 40px glyph to outlines up to
/// 255 levels apart, hinted or not, and `blur(4px)` leaves 12 of that. Core
/// Text gives identical glyphs and 0. A blur left at its density-1 width is
/// 87 apart on either.
const TEXT_REPLAY_MARGIN: f64 = 16.0;

const SIZE: (f32, f32) = (60.0, 44.0);
const RECT: [f32; 4] = [12.0, 12.25, 36.0, 20.0];

/// A canvas with `RECT` filled in flat black through `blur(sigma)`, which
/// blurs the rectangle's coverage.
fn blurred(gpu: bool, sigma: f32) -> Option<Canvas> {
    let mut canvas = Canvas::new(SIZE.0, SIZE.1);
    canvas.set_gpu(gpu);
    if gpu && canvas.engine_kind() != EngineKind::Gpu {
        return None;
    }
    let ctx = canvas.context();
    ctx.set_fill_style_css("black").expect("a colour");
    ctx.set_filter_css(&format!("blur({sigma}px)"))
        .expect("a filter");
    let [x, y, w, h] = RECT;
    ctx.fill_rect(x, y, w, h);
    Some(canvas)
}

fn scaled(factor: f32) -> ((usize, usize), [f64; 4]) {
    (
        ((SIZE.0 * factor) as usize, (SIZE.1 * factor) as usize),
        RECT.map(|v| f64::from(v * factor)),
    )
}

#[test]
fn a_blur_scales_with_the_density_it_is_exported_at() {
    for gpu in [false, true] {
        for sigma in [0.5f32, 1.0, 2.0, 4.0] {
            let Some(mut canvas) = blurred(gpu, sigma) else {
                continue;
            };
            let at = |canvas: &mut Canvas, density: u32| {
                let data = canvas
                    .to_image_data(
                        None,
                        &ImageDataExport {
                            density,
                            ..ImageDataExport::default()
                        },
                    )
                    .expect("pixels");
                let (size, rect) = scaled(density as f32);
                worst(
                    &alpha(&data),
                    size,
                    rect,
                    f64::from(sigma) * f64::from(density),
                )
            };
            let own = at(&mut canvas, 1);
            for density in [2, 3] {
                let error = at(&mut canvas, density);
                assert!(
                    error <= own + REPLAY_MARGIN,
                    "gpu {gpu} sigma {sigma} density {density}: {error:.1} levels \
                     against {own:.1} at density 1"
                );
            }
        }
    }
}

#[test]
fn a_blur_scales_with_the_canvas_it_is_drawn_into() {
    for gpu in [false, true] {
        for sigma in [0.5f32, 1.0, 2.0, 4.0] {
            let Some(mut source) = blurred(gpu, sigma) else {
                continue;
            };
            let own = {
                let data =
                    source.context().get_image_data(0.0, 0.0, SIZE.0, SIZE.1);
                let (size, rect) = scaled(1.0);
                worst(
                    &alpha(&data.expect("pixels")),
                    size,
                    rect,
                    f64::from(sigma),
                )
            };

            // At twice the size.
            let mut target = Canvas::new(SIZE.0 * 2.0, SIZE.1 * 2.0);
            target.set_gpu(gpu);
            let ctx = target.context();
            ctx.draw_canvas_sized(
                &mut source,
                0.0,
                0.0,
                SIZE.0 * 2.0,
                SIZE.1 * 2.0,
            );
            let data = ctx
                .get_image_data(0.0, 0.0, SIZE.0 * 2.0, SIZE.1 * 2.0)
                .expect("pixels");
            let (size, rect) = scaled(2.0);
            let error =
                worst(&alpha(&data), size, rect, f64::from(sigma) * 2.0);
            assert!(
                error <= own + REPLAY_MARGIN,
                "gpu {gpu} sigma {sigma} drawn at twice the size: {error:.1} levels \
                 against {own:.1} as drawn"
            );

            // Turned a quarter: (x, y) lands at (height - y, x).
            let mut target = Canvas::new(SIZE.1, SIZE.0);
            target.set_gpu(gpu);
            let ctx = target.context();
            ctx.translate(SIZE.1, 0.0);
            ctx.rotate(std::f32::consts::FRAC_PI_2);
            ctx.draw_canvas(&mut source, 0.0, 0.0);
            let data = ctx
                .get_image_data(0.0, 0.0, SIZE.1, SIZE.0)
                .expect("pixels");
            let [x, y, w, h] = RECT.map(f64::from);
            let turned = [f64::from(SIZE.1) - y - h, x, h, w];
            let size = (SIZE.1 as usize, SIZE.0 as usize);
            let error = worst(&alpha(&data), size, turned, f64::from(sigma));
            assert!(
                error <= own + REPLAY_MARGIN,
                "gpu {gpu} sigma {sigma} turned a quarter: {error:.1} levels \
                 against {own:.1} as drawn"
            );
        }
    }
}

#[test]
fn a_rounded_rect_or_text_blurs_at_density_two_as_drawn_twice_as_large() {
    // The same coverage blur reaches rounded rectangles and text, for which
    // there is no closed form to compare with: exported at density 2, the
    // drawing is the one drawn at twice the size, blur and all -- within
    // what the glyphs themselves differ by, for text. With the blur fixed
    // in device pixels the two differed by 50 to 87 levels.
    let draw = |ctx: &mut Context2D, scale: f32, text: bool| {
        ctx.set_fill_style_css("black").expect("a colour");
        ctx.set_filter_css(&format!("blur({}px)", 2.0 * scale))
            .expect("a filter");
        match text {
            true => {
                assert!(
                    ctx.set_font_css(&format!("{}px sans-serif", 20.0 * scale))
                );
                ctx.fill_text("Hamburg", 10.0 * scale, 40.0 * scale, None);
            }
            false => {
                ctx.begin_path();
                ctx.round_rect(
                    10.0 * scale,
                    10.25 * scale,
                    60.0 * scale,
                    30.0 * scale,
                    [8.0 * scale; 4],
                )
                .expect("a rounded rectangle");
                ctx.fill(FillRule::NonZero);
            }
        }
    };
    for gpu in [false, true] {
        for text in [false, true] {
            let mut exported = Canvas::new(100.0, 60.0);
            exported.set_gpu(gpu);
            if gpu && exported.engine_kind() != EngineKind::Gpu {
                continue;
            }
            draw(exported.context(), 1.0, text);
            let exported = exported
                .to_image_data(
                    None,
                    &ImageDataExport {
                        density: 2,
                        ..ImageDataExport::default()
                    },
                )
                .expect("pixels");
            let mut large = Canvas::new(200.0, 120.0);
            large.set_gpu(gpu);
            draw(large.context(), 2.0, text);
            let large = large
                .context()
                .get_image_data(0.0, 0.0, 200.0, 120.0)
                .expect("pixels");
            let worst = alpha(&exported)
                .iter()
                .zip(alpha(&large))
                .map(|(a, b)| a.abs_diff(b))
                .max()
                .expect("pixels");
            let margin = match text {
                true => TEXT_REPLAY_MARGIN,
                false => REPLAY_MARGIN,
            };
            assert!(
                f64::from(worst) <= margin,
                "gpu {gpu} text {text}: {worst} levels apart"
            );
        }
    }
}

/// How far from the exact blur a blur the coverage blur cannot reach may be
/// drawn. It is blurred on its result, by the exact Gaussian, and measured
/// half a level off on either engine at 160 device pixels: the rounding
/// alone. Skia's image blur was 7.4 levels off on the CPU and 5.1 on the
/// GPU, and drawn at Skia's coverage-blur cap of 128 it was 48.7 and 40.9.
const PAST_THE_CAP: f64 = 1.0;

#[test]
fn a_blur_past_the_coverage_cap_at_a_density_is_drawn_at_its_width() {
    // `blur(40px)` on a flat rectangle takes the coverage blur at density 1,
    // and at density 4 comes to 160 device pixels, past the 128 that blur
    // can draw. The choice is made again where the page is played back --
    // inside a `saveLayer` as well, whose contents are played back with it.
    let (width, height) = (400.0f32, 300.0f32);
    let rect = [150.0f32, 100.25, 100.0, 100.0];
    let sigma = 40.0f32;
    for (gpu, layered) in
        [(false, false), (true, false), (false, true), (true, true)]
    {
        let mut canvas = Canvas::new(width, height);
        canvas.set_gpu(gpu);
        if gpu && canvas.engine_kind() != EngineKind::Gpu {
            continue;
        }
        let ctx = canvas.context();
        ctx.set_fill_style_css("black").expect("a colour");
        ctx.set_filter_css(&format!("blur({sigma}px)"))
            .expect("a filter");
        if layered {
            ctx.save_layer();
        }
        let [x, y, w, h] = rect;
        ctx.fill_rect(x, y, w, h);
        if layered {
            ctx.restore();
        }
        let density = 4;
        let data = canvas
            .to_image_data(
                None,
                &ImageDataExport {
                    density,
                    ..ImageDataExport::default()
                },
            )
            .expect("pixels");
        let scale = density as f32;
        let size = ((width * scale) as usize, (height * scale) as usize);
        let error = worst(
            &alpha(&data),
            size,
            rect.map(|v| f64::from(v * scale)),
            f64::from(sigma * scale),
        );
        assert!(
            error <= PAST_THE_CAP,
            "gpu {gpu} layered {layered}: {error:.1} levels from the exact \
             blur at density {density}"
        );
    }
}

/// How much closer a paragraph's blurred shadow exported at density 2 must
/// be to its unblurred shadow blurred by twice the sigma than by the sigma
/// itself. The first is 20 to 24 levels away -- the coverage blur's own
/// approximation of a Gaussian -- and the second 92 to 105; with the sigma
/// fixed in device pixels the two were the other way round, 94 to 100 and
/// 23 to 41.
const PARAGRAPH_SCALED_RATIO: f64 = 3.0;

#[test]
fn a_paragraph_shadow_blurs_at_density_two_by_twice_its_sigma() {
    // Wide enough that no font family the text falls back to reaches
    // the edge, where the blur would spread ink the canvas does not keep.
    let width = 240.0;
    let shadowed = |blur: f32, oblique: bool| {
        let library = FontLibrary::new();
        let engine = TextEngine::new(&library);
        let style = TextStyle {
            font_families: vec!["sans-serif".into()],
            font_size: 24.0,
            // The text itself transparent, so only its shadow is drawn.
            color: RgbaLinear::from_srgb8(0, 0, 0, 0.0),
            slant: match oblique {
                true => TextSlant::OBLIQUE,
                false => TextSlant::Upright,
            },
            shadows: vec![TextShadow {
                color: RgbaLinear::from_srgb8(0, 0, 0, 1.0),
                offset_x: 1.0,
                offset_y: 0.0,
                blur_sigma: blur,
            }],
            ..TextStyle::default()
        };
        let mut builder = engine.paragraph_builder(&style);
        builder.add_text("Hamburg");
        builder.build(1000.0)
    };
    for gpu in [false, true] {
        for oblique in [false, true] {
            let exported = |blur: f32| -> Option<Vec<f64>> {
                let mut canvas = Canvas::new(width, 50.0);
                canvas.set_gpu(gpu);
                if gpu && canvas.engine_kind() != EngineKind::Gpu {
                    return None;
                }
                canvas.context().draw_paragraph(
                    &shadowed(blur, oblique),
                    10.0,
                    10.0,
                );
                let data = canvas
                    .to_image_data(
                        None,
                        &ImageDataExport {
                            density: 2,
                            ..ImageDataExport::default()
                        },
                    )
                    .expect("pixels");
                Some(alpha(&data).into_iter().map(f64::from).collect())
            };
            let Some(sharp) = exported(0.0) else {
                continue;
            };
            for sigma in [1.0f32, 2.0] {
                let got = exported(sigma).expect("pixels");
                let off = |by: f64| {
                    gaussian(&sharp, (width * 2.0) as usize, by)
                        .iter()
                        .zip(&got)
                        .map(|(e, g)| (e - g).abs())
                        .fold(0.0, f64::max)
                };
                let (scaled, unscaled) =
                    (off(f64::from(sigma) * 2.0), off(f64::from(sigma)));
                assert!(
                    scaled * PARAGRAPH_SCALED_RATIO <= unscaled,
                    "gpu {gpu} oblique {oblique} sigma {sigma}: {scaled:.1} levels \
                     from twice the sigma, {unscaled:.1} from the sigma itself"
                );
            }
        }
    }
}
