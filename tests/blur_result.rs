//! A draw blurred on its result is the exact Gaussian blur of what it draws:
//! the draw's own pixels convolved with the Gaussian sampled at whole-pixel
//! offsets, rounded once. A rasterised pixel is already its shape averaged
//! over the pixel's box, so sampling the Gaussian -- rather than averaging
//! it over each pixel too -- applies the box once. The reference here is
//! written apart from any code that draws a blur, in f64, reaching eight
//! sigmas.

use meo_skia_canvas::prelude::*;

const SIZE: (f32, f32) = (160.0, 100.0);

/// How far a value may sit from a half-level and still round either way:
/// the f32 accumulation a blur is made in carries about 1e-4 of a level.
const TIE: f64 = 1e-3;

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

/// A triangle across most of the canvas.
const WIDE: [(f32, f32); 3] = [(20.0, 80.0), (80.0, 15.25), (140.5, 85.0)];

/// A canvas on `gpu`'s engine with the triangle `at` filled in flat black
/// through `filter`, clipped to the left `clip` pixels where there is one,
/// or `None` where that engine is not available.
fn triangle(
    gpu: bool,
    at: [(f32, f32); 3],
    filter: &str,
    clip: Option<f32>,
) -> Option<Canvas> {
    triangle_in(PixelDepth::Uint8, gpu, at, filter, clip, false)
}

/// [`triangle`] on a canvas whose pixels are `depth`, drawn inside a
/// `saveLayer` where `layered` says so.
fn triangle_in(
    depth: PixelDepth,
    gpu: bool,
    at: [(f32, f32); 3],
    filter: &str,
    clip: Option<f32>,
    layered: bool,
) -> Option<Canvas> {
    let mut canvas = Canvas::with_options(
        SIZE.0,
        SIZE.1,
        CanvasOptions {
            color_type: depth,
            gpu,
            ..CanvasOptions::default()
        },
    )
    .expect("a canvas");
    if gpu && canvas.engine_kind() != EngineKind::Gpu {
        return None;
    }
    let ctx = canvas.context();
    ctx.set_fill_style_css("black").expect("a colour");
    ctx.set_filter_css(filter).expect("a filter");
    if let Some(width) = clip {
        ctx.begin_path();
        ctx.rect(0.0, 0.0, width, SIZE.1);
        ctx.clip(FillRule::NonZero);
    }
    if layered {
        ctx.save_layer();
    }
    ctx.begin_path();
    let [(x0, y0), (x1, y1), (x2, y2)] = at;
    ctx.move_to(x0, y0);
    ctx.line_to(x1, y1);
    ctx.line_to(x2, y2);
    ctx.close_path();
    ctx.fill(FillRule::NonZero);
    if layered {
        ctx.restore();
    }
    Some(canvas)
}

fn alpha(canvas: &mut Canvas) -> Vec<u8> {
    let data = canvas
        .context()
        .get_image_data(0.0, 0.0, SIZE.0, SIZE.1)
        .expect("pixels");
    data.pixels().chunks(4).map(|p| p[3]).collect()
}

/// The premultiplied alpha of every pixel, read back as F32.
fn float_alpha(canvas: &mut Canvas) -> Vec<f64> {
    canvas
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
        .expect("pixels")
        .pixels()
        .chunks(16)
        .map(|p| f64::from(f32::from_le_bytes([p[12], p[13], p[14], p[15]])))
        .collect()
}

/// The pixels of `alpha` that are not the reference's rounded level, but
/// for those within [`TIE`] of a half-level that land on either neighbour.
fn misses(alpha: &[u8], reference: &[f64]) -> Vec<(usize, u8, f64)> {
    alpha
        .iter()
        .zip(reference)
        .enumerate()
        .filter(|(_, (a, r))| {
            let level = **r * 255.0;
            let tie = (level.fract() - 0.5).abs() < TIE;
            let near = (f64::from(**a) - level).abs() < 0.5 + TIE;
            f64::from(**a) != level.round() && !(tie && near)
        })
        .map(|(i, (a, r))| (i, *a, r * 255.0))
        .collect()
}

#[test]
fn a_blurred_path_is_the_exact_gaussian_of_what_it_draws() {
    for gpu in [false, true] {
        // Under the FFT's crossover, over it, and far over it.
        for sigma in [2.0f32, 8.0, 30.0] {
            let (Some(mut drawn), Some(mut blurred)) = (
                triangle(gpu, WIDE, "none", None),
                triangle(gpu, WIDE, &format!("blur({sigma}px)"), None),
            ) else {
                continue;
            };
            let drawn: Vec<f64> = alpha(&mut drawn)
                .into_iter()
                .map(|a| f64::from(a) / 255.0)
                .collect();
            let reference = sampled(&drawn, SIZE.0 as usize, f64::from(sigma));
            let misses = misses(&alpha(&mut blurred), &reference);
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

/// Where the left part of the canvas keeps what the blur leaves there.
const CLIP: f32 = 70.0;

/// A triangle reaching past [`CLIP`] by less than the smallest blur below
/// reaches, 12 pixels at a sigma of 2.
///
/// Skia rasterises a path its clip does not contain by another route than
/// one it does, and the edges differ by up to 15 levels however far they
/// are from the cut. A shape the blur's layer cut would test that instead
/// of the blur, so this one stays within the layer whole.
const NARROW: [(f32, f32); 3] = [(10.0, 85.0), (45.0, 10.25), (80.5, 90.0)];

#[test]
fn a_clip_confines_the_blur_and_not_what_it_blurs() {
    // What lies outside the clip is blurred into it, as a filter is applied
    // to the draw and the clip to what the filter makes.
    for gpu in [false, true] {
        for sigma in [2.0f32, 8.0, 30.0] {
            let (Some(mut drawn), Some(mut blurred)) = (
                triangle(gpu, NARROW, "none", None),
                triangle(gpu, NARROW, &format!("blur({sigma}px)"), Some(CLIP)),
            ) else {
                continue;
            };
            let drawn: Vec<f64> = alpha(&mut drawn)
                .into_iter()
                .map(|a| f64::from(a) / 255.0)
                .collect();
            let width = SIZE.0 as usize;
            let reference: Vec<f64> = sampled(&drawn, width, f64::from(sigma))
                .into_iter()
                .enumerate()
                .map(|(i, v)| match (i % width) < CLIP as usize {
                    true => v,
                    false => 0.0,
                })
                .collect();
            let misses = misses(&alpha(&mut blurred), &reference);
            assert!(
                misses.is_empty(),
                "gpu {gpu} sigma {sigma}: {} pixels off the clipped exact \
                 blur, first {:?}",
                misses.len(),
                &misses[..misses.len().min(4)],
            );
        }
    }
}

/// How far a float page's pixel may be from the reference, as a fraction of
/// it: what the page's own rounding allows. F16 keeps ten bits of fraction,
/// so it rounds to within half of 2^-10 of a value; F32 adds nothing that
/// the f32 sums below do not.
const F16_ROUNDING: f64 = 1.0 / 2048.0;
const F32_ROUNDING: f64 = 0.0;

/// How far any float pixel may be from the reference whatever its value:
/// the f32 the blur is summed in. The FFT's error is absolute, about 1e-7
/// of the largest value, and an F16 subnormal is held to 2^-25. This is a
/// four-thousandth of an 8-bit level, and Skia's own blur misses by
/// hundreds of times it.
const F32_NOISE: f64 = 1e-6;

#[test]
fn a_blurred_path_on_float_pixels_is_the_exact_gaussian_unrounded() {
    for (depth, rounding) in [
        (PixelDepth::F16, F16_ROUNDING),
        (PixelDepth::F32, F32_ROUNDING),
    ] {
        for gpu in [false, true] {
            for sigma in [2.0f32, 8.0, 30.0] {
                let filter = format!("blur({sigma}px)");
                let (Some(mut drawn), Some(mut blurred)) = (
                    triangle_in(depth, gpu, WIDE, "none", None, false),
                    triangle_in(depth, gpu, WIDE, &filter, None, false),
                ) else {
                    continue;
                };
                let reference = sampled(
                    &float_alpha(&mut drawn),
                    SIZE.0 as usize,
                    f64::from(sigma),
                );
                let got = float_alpha(&mut blurred);
                // The worst miss as a multiple of what that pixel allows.
                let worst = got
                    .iter()
                    .zip(&reference)
                    .map(|(g, r)| (g - r).abs() / (r * rounding + F32_NOISE))
                    .fold(0.0, f64::max);
                assert!(
                    worst <= 1.0,
                    "{depth:?} gpu {gpu} sigma {sigma}: {worst:.2} times as \
                     far off the exact blur as allowed"
                );
            }
        }
    }
}

#[test]
fn a_blurred_path_exported_at_a_density_is_the_exact_gaussian_there() {
    // The blur's length is in canvas pixels, so at density 2 it is twice as
    // many device pixels, and is the exact blur of what is drawn there.
    let density = 2;
    let at = |canvas: &mut Canvas| -> Vec<u8> {
        canvas
            .to_image_data(
                None,
                &ImageDataExport {
                    density,
                    ..ImageDataExport::default()
                },
            )
            .expect("pixels")
            .pixels()
            .chunks(4)
            .map(|p| p[3])
            .collect()
    };
    // Inside a `saveLayer` too: the layer's contents are played at the
    // export's scale, and the blur is made there rather than where the
    // draw was recorded.
    for (gpu, layered) in
        [(false, false), (true, false), (false, true), (true, true)]
    {
        for sigma in [2.0f32, 8.0] {
            let filter = format!("blur({sigma}px)");
            let (Some(mut drawn), Some(mut blurred)) = (
                triangle(gpu, WIDE, "none", None),
                triangle_in(
                    PixelDepth::Uint8,
                    gpu,
                    WIDE,
                    &filter,
                    None,
                    layered,
                ),
            ) else {
                continue;
            };
            let drawn: Vec<f64> = at(&mut drawn)
                .into_iter()
                .map(|a| f64::from(a) / 255.0)
                .collect();
            let width = SIZE.0 as usize * density as usize;
            let reference =
                sampled(&drawn, width, f64::from(sigma) * f64::from(density));
            let misses = misses(&at(&mut blurred), &reference);
            assert!(
                misses.is_empty(),
                "gpu {gpu} layered {layered} sigma {sigma} density \
                 {density}: {} pixels off the exact blur, first {:?}",
                misses.len(),
                &misses[..misses.len().min(4)],
            );
        }
    }
}

/// A canvas on `gpu`'s engine with a 40px red square canvas drawn into it
/// at (60, 30) through `filter`, at its own size or `sized` to it, or `None`
/// where that engine is not available.
fn drawn_canvas(gpu: bool, filter: &str, sized: bool) -> Option<Canvas> {
    let mut source = Canvas::new(40.0, 40.0);
    source.set_gpu(gpu);
    source
        .context()
        .set_fill_style_css("red")
        .expect("a colour");
    source.context().fill_rect(0.0, 0.0, 40.0, 40.0);
    let mut canvas = Canvas::new(SIZE.0, SIZE.1);
    canvas.set_gpu(gpu);
    if gpu && canvas.engine_kind() != EngineKind::Gpu {
        return None;
    }
    let ctx = canvas.context();
    ctx.set_filter_css(filter).expect("a filter");
    match sized {
        true => ctx.draw_canvas_sized(&mut source, 60.0, 30.0, 40.0, 40.0),
        false => ctx.draw_canvas(&mut source, 60.0, 30.0),
    }
    Some(canvas)
}

#[test]
fn a_blurred_canvas_drawn_in_spreads_past_where_it_is_drawn() {
    // The destination rectangle bounds what is drawn, not its blur: Chrome
    // 156 reads alpha 72 five pixels outside a red square blurred by 8px and
    // 28 ten pixels out, where this read 0 at both.
    for gpu in [false, true] {
        for sized in [false, true] {
            for sigma in [2.0f32, 8.0] {
                let (Some(mut drawn), Some(mut blurred)) = (
                    drawn_canvas(gpu, "none", sized),
                    drawn_canvas(gpu, &format!("blur({sigma}px)"), sized),
                ) else {
                    continue;
                };
                let drawn: Vec<f64> = alpha(&mut drawn)
                    .into_iter()
                    .map(|a| f64::from(a) / 255.0)
                    .collect();
                let reference =
                    sampled(&drawn, SIZE.0 as usize, f64::from(sigma));
                let got = alpha(&mut blurred);
                let misses = misses(&got, &reference);
                assert!(
                    misses.is_empty(),
                    "gpu {gpu} sized {sized} sigma {sigma}: {} pixels off \
                     the exact blur, first {:?}",
                    misses.len(),
                    &misses[..misses.len().min(4)],
                );
                // And so it reaches past the square, as Chrome's does.
                let row = 50 * SIZE.0 as usize;
                assert!(got[row + 55] > 0, "gpu {gpu} sigma {sigma}");
            }
        }
    }
}

#[test]
fn a_canvas_drawn_in_casts_its_drop_shadow_past_where_it_is_drawn() {
    // A `drop-shadow()` is still Skia's filter, through a layer; the
    // destination rectangle confines what is drawn into that layer, and the
    // shadow, thirty pixels to the right of the square, lands outside it.
    for gpu in [false, true] {
        let Some(mut canvas) =
            drawn_canvas(gpu, "drop-shadow(30px 0px 0px blue)", false)
        else {
            continue;
        };
        let data = canvas
            .context()
            .get_image_data(0.0, 50.0, SIZE.0, 1.0)
            .expect("pixels");
        let pixel = |x: usize| &data.pixels()[x * 4..x * 4 + 4];
        assert_eq!(pixel(115), [0, 0, 255, 255], "gpu {gpu}: the shadow");
        assert_eq!(pixel(80), [255, 0, 0, 255], "gpu {gpu}: the square");
    }
}
