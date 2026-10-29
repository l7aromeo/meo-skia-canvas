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

#[test]
fn a_drop_shadow_past_a_canvas_drawn_in_survives_the_layer_around_it() {
    // A read inside an open layer replays the group the drawn canvas makes,
    // and a plain layer around it is confined to what its contents reach:
    // the shadow lies outside the destination, so a group reporting the
    // destination as its reach would have the parent layer cut it off.
    for gpu in [false, true] {
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
            continue;
        }
        let ctx = canvas.context();
        ctx.save_layer_with(0.5, None, None);
        ctx.set_filter_css("drop-shadow(30px 0px 0px blue)")
            .expect("a filter");
        ctx.draw_canvas(&mut source, 60.0, 30.0);
        for open in [true, false] {
            if !open {
                ctx.restore();
            }
            let data =
                ctx.get_image_data(0.0, 50.0, SIZE.0, 1.0).expect("pixels");
            let pixel = |x: usize| &data.pixels()[x * 4..x * 4 + 4];
            let (shadow, square) = (pixel(115), pixel(80));
            assert!(
                shadow[2] == 255
                    && shadow[0] == 0
                    && (127..=128).contains(&shadow[3]),
                "gpu {gpu}, layer open {open}: the shadow reads {shadow:?}"
            );
            assert!(
                square[0] == 255
                    && square[2] == 0
                    && (127..=128).contains(&square[3]),
                "gpu {gpu}, layer open {open}: the square reads {square:?}"
            );
        }
    }
}

#[test]
fn a_flood_through_a_canvas_drawn_in_covers_the_page_as_chrome_does() {
    // Chrome 156 floods the whole canvas when an SVG `feFlood` filter draws
    // a canvas in: on a 300 by 200 page, blue 50 pixels right of the
    // destination and 80 below it, whatever the filter region. An image filter
    // that paints transparent pixels is this library's way to the same
    // draw, and the destination bounds what is drawn, not what the filter
    // paints -- inside an open layer as well as on the page.
    let flood = ImageFilter::from_color_filter(
        ColorFilter::blend(
            RgbaLinear::from_srgb8(0, 0, 255, 1.0),
            BlendMode::Copy,
        )
        .expect("a blend"),
        None,
        None,
    )
    .expect("a flood");
    for gpu in [false, true] {
        for in_layer in [false, true] {
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
                continue;
            }
            let ctx = canvas.context();
            if in_layer {
                ctx.save_layer();
            }
            ctx.set_image_filter(Some(&flood));
            ctx.draw_canvas(&mut source, 60.0, 30.0);
            if in_layer {
                ctx.restore();
            }
            for (x, y) in
                [(80.0, 50.0), (150.0, 50.0), (80.0, 90.0), (2.0, 2.0)]
            {
                let data = ctx.get_image_data(x, y, 1.0, 1.0).expect("a pixel");
                assert_eq!(
                    data.pixels(),
                    [0, 0, 255, 255],
                    "gpu {gpu}, in a layer {in_layer}: ({x}, {y})"
                );
            }
        }
    }
}

/// A 40px image whose alpha varies across it, so a blur has structure to
/// spread and not only an edge.
fn varied_image() -> Image {
    let pixels: Vec<u8> = (0..40 * 40)
        .flat_map(|i| {
            let (x, y) = (i % 40, i / 40);
            let alpha = ((x * 7 + y * 13) % 256) as u8;
            [alpha, 0, alpha / 2, alpha]
        })
        .collect();
    Image::from_pixels(
        &pixels,
        40,
        40,
        40 * 4,
        PixelExportOptions {
            premultiplied: true,
            ..PixelExportOptions::default()
        },
    )
    .expect("an image")
}

/// A canvas on `gpu`'s engine with [`varied_image`] drawn at (60.25, 30)
/// through `filter` at `global_alpha`, or `None` where that engine is not
/// available.
fn drawn_image(gpu: bool, filter: &str, global_alpha: f32) -> Option<Canvas> {
    let mut canvas = Canvas::new(SIZE.0, SIZE.1);
    canvas.set_gpu(gpu);
    if gpu && canvas.engine_kind() != EngineKind::Gpu {
        return None;
    }
    let ctx = canvas.context();
    ctx.set_filter_css(filter).expect("a filter");
    ctx.set_global_alpha(global_alpha);
    ctx.draw_image(&varied_image(), 60.25, 30.0);
    Some(canvas)
}

#[test]
fn a_blurred_image_is_the_exact_gaussian_of_what_it_draws() {
    // The filter applies to what is drawn, and the global alpha after it,
    // as the Canvas drawing model orders them: the reference is the image
    // drawn opaque, blurred, then scaled by the alpha.
    for gpu in [false, true] {
        for sigma in [2.0f32, 8.0] {
            for global in [1.0f32, 0.5] {
                let (Some(mut drawn), Some(mut blurred)) = (
                    drawn_image(gpu, "none", 1.0),
                    drawn_image(gpu, &format!("blur({sigma}px)"), global),
                ) else {
                    continue;
                };
                let drawn: Vec<f64> = alpha(&mut drawn)
                    .into_iter()
                    .map(|a| f64::from(a) / 255.0)
                    .collect();
                let reference: Vec<f64> =
                    sampled(&drawn, SIZE.0 as usize, f64::from(sigma))
                        .into_iter()
                        .map(|v| v * f64::from(global))
                        .collect();
                let misses = misses(&alpha(&mut blurred), &reference);
                assert!(
                    misses.is_empty(),
                    "gpu {gpu} sigma {sigma} alpha {global}: {} pixels off \
                     the exact blur, first {:?}",
                    misses.len(),
                    &misses[..misses.len().min(4)],
                );
            }
        }
    }
}

/// A canvas on `gpu`'s engine with a rounded rectangle filled in flat black
/// at `x` through `filter`, or `None` where that engine is not available.
fn rounded(gpu: bool, filter: &str, x: f32) -> Option<Canvas> {
    let mut canvas = Canvas::new(SIZE.0, SIZE.1);
    canvas.set_gpu(gpu);
    if gpu && canvas.engine_kind() != EngineKind::Gpu {
        return None;
    }
    let ctx = canvas.context();
    ctx.set_fill_style_css("black").expect("a colour");
    ctx.set_filter_css(filter).expect("a filter");
    ctx.begin_path();
    ctx.round_rect(x, 30.25, 80.0, 40.0, [12.0; 4])
        .expect("a rounded rectangle");
    ctx.fill(FillRule::NonZero);
    Some(canvas)
}

/// Where [`rounded`] draws its rectangle across the rows of a test: the
/// middle, and left edges that put the layer's origin where Skia drew the
/// corners otherwise than the page did, before the layer was placed as the
/// page is -- some on the CPU, every odd one on the GPU.
const ROUNDED_AT: [f32; 9] =
    [40.0, 33.0, 41.0, 44.0, 47.0, 49.0, 52.0, 56.0, 57.0];

#[test]
fn a_blurred_rounded_rectangle_is_the_exact_gaussian_of_what_it_draws() {
    // Skia's coverage blur of a rounded rectangle approximates the
    // Gaussian, so a rounded rectangle is blurred on its result at every
    // sigma, as a triangle is.
    for gpu in [false, true] {
        for (sigma, at) in [2.0f32, 8.0]
            .into_iter()
            .flat_map(|sigma| ROUNDED_AT.map(|x| (sigma, x)))
            .chain([(30.0, 40.0)])
        {
            let filter = format!("blur({sigma}px)");
            let (Some(mut drawn), Some(mut blurred)) =
                (rounded(gpu, "none", at), rounded(gpu, &filter, at))
            else {
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
                "gpu {gpu} sigma {sigma} at {at}: {} pixels off the exact \
                 blur, first {:?}",
                misses.len(),
                &misses[..misses.len().min(4)],
            );
        }
    }
}

/// The alpha of `window`, (left, top, width, height), of a canvas `size`
/// large on `gpu`'s engine with [`rounded`]'s rectangle at `at` filled
/// through `filter`, read as a whole: a read of part of a canvas plays it
/// from that part's origin. `None` where the engine is not available.
fn far(
    gpu: bool,
    size: (f32, f32),
    at: (f32, f32),
    filter: &str,
    window: (usize, usize, usize, usize),
) -> Option<Vec<u8>> {
    let mut canvas = Canvas::new(size.0, size.1);
    canvas.set_gpu(gpu);
    if gpu && canvas.engine_kind() != EngineKind::Gpu {
        return None;
    }
    let ctx = canvas.context();
    ctx.set_fill_style_css("black").expect("a colour");
    ctx.set_filter_css(filter).expect("a filter");
    ctx.begin_path();
    ctx.round_rect(at.0, at.1, 80.0, 40.0, [12.0; 4])
        .expect("a rounded rectangle");
    ctx.fill(FillRule::NonZero);
    let data = ctx
        .get_image_data(0.0, 0.0, size.0, size.1)
        .expect("image data");
    let (left, top, width, height) = window;
    let pixels = data.pixels();
    Some(
        (top..top + height)
            .flat_map(|row| {
                let start = (row * size.0 as usize + left) * 4;
                pixels[start..start + width * 4].chunks(4).map(|p| p[3])
            })
            .collect(),
    )
}

/// Whether `window` of [`far`]'s canvas, blurred by `sigma`, is the exact
/// blur of what the canvas draws there unfiltered; `None` where the engine
/// is not available.
fn far_misses(
    gpu: bool,
    size: (f32, f32),
    at: (f32, f32),
    window: (usize, usize, usize, usize),
    sigma: f32,
) -> Option<Vec<(usize, u8, f64)>> {
    far_misses_past(gpu, size, at, window, sigma, (0, 0))
}

/// [`far_misses`], with the unfiltered draw the reference is blurred from
/// made on a canvas larger by `pad.0` pixels before the page on both axes,
/// the draw moved in by as much, and `pad.1` after it, so that what it
/// draws past the page is there to blur -- the blur of a draw reaching off
/// the page includes that part, and the page itself cannot hold it.
/// Compared inside `window`, on the page.
fn far_misses_past(
    gpu: bool,
    size: (f32, f32),
    at: (f32, f32),
    window: (usize, usize, usize, usize),
    sigma: f32,
    pad: (usize, usize),
) -> Option<Vec<(usize, u8, f64)>> {
    let (left, top, width, height) = window;
    let (before, after) = pad;
    let wide = (left, top, width + before + after, height + before + after);
    let (shift, grow) = (before as f32, (before + after) as f32);
    let drawn: Vec<f64> = far(
        gpu,
        (size.0 + grow, size.1 + grow),
        (at.0 + shift, at.1 + shift),
        "none",
        wide,
    )?
    .into_iter()
    .map(|a| f64::from(a) / 255.0)
    .collect();
    let blurred_wide = sampled(&drawn, wide.2, f64::from(sigma));
    let reference: Vec<f64> = (before..before + height)
        .flat_map(|row| {
            let start = row * wide.2 + before;
            blurred_wide[start..start + width].iter().copied()
        })
        .collect();
    let blurred = far(gpu, size, at, &format!("blur({sigma}px)"), window)?;
    Some(misses(&blurred, &reference))
}

/// The reference a page's right or bottom edge is measured against: the
/// unfiltered draw on a canvas 40 pixels larger after the page, so the part
/// past the edge is drawn and nothing moves.
const PAST_THE_PAGE: (usize, usize) = (0, 40);

/// The reference a page's left or top edge is measured against: the draw
/// moved 64 pixels in, onto a canvas that much larger before the page. A
/// draw starting off the page has no reference that does not move it, and
/// the CPU's anti-aliasing depends on where a draw lies, so on the CPU the
/// two meet only to within a level: a draw wholly inside the page, exact
/// against itself unmoved, is 32 pixels off this reference and 17 off one
/// moved 40.
const BEFORE_THE_PAGE: (usize, usize) = (64, 0);

#[test]
fn a_blurred_rounded_rectangle_past_the_page_is_the_exact_gaussian_of_the_whole_draw()
 {
    // The blur of a draw reaching off the page includes the part off it, as
    // a draw cut at the page would not: against a reference made on the
    // page, these read 92 levels off. The CPU's past the right and bottom
    // edges, and the GPU's past every edge, are exact.
    let exact = [
        (
            true,
            (300.0, 200.0),
            (-3.0, 30.25),
            (0, 0, 140, 100),
            BEFORE_THE_PAGE,
        ),
        (
            true,
            (300.0, 200.0),
            (30.25, -3.0),
            (0, 0, 140, 100),
            BEFORE_THE_PAGE,
        ),
        (
            true,
            (300.0, 200.0),
            (223.0, 30.25),
            (160, 0, 140, 100),
            PAST_THE_PAGE,
        ),
        (
            true,
            (300.0, 200.0),
            (30.25, 163.0),
            (0, 100, 140, 100),
            PAST_THE_PAGE,
        ),
        (
            false,
            (300.0, 200.0),
            (223.0, 30.25),
            (160, 0, 140, 100),
            PAST_THE_PAGE,
        ),
        (
            false,
            (300.0, 200.0),
            (30.25, 163.0),
            (0, 100, 140, 100),
            PAST_THE_PAGE,
        ),
        (
            false,
            (300.0, 200.0),
            (222.75, 165.5),
            (160, 100, 140, 100),
            PAST_THE_PAGE,
        ),
        (
            false,
            (16384.0, 120.0),
            (16290.0, 30.25),
            (16244, 0, 140, 100),
            PAST_THE_PAGE,
        ),
        (
            false,
            (120.0, 16384.0),
            (20.25, 16330.5),
            (0, 16284, 120, 100),
            PAST_THE_PAGE,
        ),
    ];
    for (gpu, size, at, window, pad) in exact {
        for sigma in [2.0f32, 8.0] {
            let Some(misses) =
                far_misses_past(gpu, size, at, window, sigma, pad)
            else {
                continue;
            };
            assert!(
                misses.is_empty(),
                "gpu {gpu}, sigma {sigma}, at {at:?} on {size:?}: {} pixels \
                 off the exact blur, first {:?}",
                misses.len(),
                &misses[..misses.len().min(4)],
            );
        }
    }
    // The CPU's past the left and top edges, within a level of the draw
    // moved in -- see [`BEFORE_THE_PAGE`].
    let near = [
        ((300.0, 200.0), (-3.0, 30.25), (0, 0, 140, 100)),
        ((300.0, 200.0), (30.25, -3.0), (0, 0, 140, 100)),
        ((300.0, 200.0), (-3.0, -3.0), (0, 0, 140, 100)),
        ((300.0, 200.0), (-5.5, 30.25), (0, 0, 140, 100)),
        ((16384.0, 120.0), (-3.0, 30.25), (0, 0, 140, 100)),
        ((120.0, 16384.0), (20.25, -3.0), (0, 0, 120, 100)),
    ];
    for (size, at, window) in near {
        for sigma in [2.0f32, 8.0] {
            let misses = far_misses_past(
                false,
                size,
                at,
                window,
                sigma,
                BEFORE_THE_PAGE,
            )
            .expect("the CPU is always there");
            let worst = misses
                .iter()
                .map(|(_, got, exact)| (f64::from(*got) - exact).abs())
                .fold(0.0, f64::max);
            assert!(
                worst < 1.0,
                "sigma {sigma}, at {at:?} on {size:?}: {worst:.2} levels off"
            );
        }
    }
}

#[test]
fn a_blurred_rounded_rectangle_far_into_the_page_is_the_exact_gaussian_of_what_it_draws()
 {
    // Skia splits a curve in floats at the device's own coordinates, so a
    // curve thousands of pixels in is drawn otherwise once translated
    // nearer the origin: a layer starting at the region's top left this
    // one 7000.7 pixels down off the exact blur by 7 pixels. Each window
    // holds the whole rectangle, and beyond it the canvas's edge or nothing.
    //
    // The CPU's; the GPU's are below, as what moves its edges is not this.
    let rows = [
        ((160.0, 7100.0), (41.0, 7000.7), (0, 6960, 160, 120)),
        ((8000.0, 120.0), (7900.0, 30.25), (7860, 0, 140, 120)),
    ];
    for (size, at, window) in rows {
        for sigma in [2.0f32, 8.0] {
            let misses = far_misses(false, size, at, window, sigma)
                .expect("the CPU is always there");
            assert!(
                misses.is_empty(),
                "sigma {sigma} at {at:?}: {} pixels off the exact blur, \
                 first {:?}",
                misses.len(),
                &misses[..misses.len().min(4)],
            );
        }
    }
}

#[test]
fn a_blurred_rounded_rectangle_on_the_gpu_is_the_exact_gaussian_wherever_it_lies()
 {
    // On the GPU a draw rounds by the size of the target as well as by where
    // it lies, and a layer the size of the region drew each of these but the
    // first otherwise than the page does -- 12, 12, 10, 8, 2, 2, 2, 5 and 2
    // pixels off the exact blur. The last two lie on pages 16383 and 16384
    // pixels long. Each window holds the whole rectangle.
    let rows = [
        ((300.0, 200.0), (41.0, 30.25), (11, 0, 140, 100)),
        ((1536.0, 120.0), (1125.75, 30.25), (1095, 0, 140, 100)),
        ((4000.0, 120.0), (41.75, 30.25), (11, 0, 140, 100)),
        ((160.0, 3000.0), (41.0, 41.0), (0, 11, 160, 100)),
        ((160.0, 3000.0), (41.0, 750.5), (0, 720, 160, 100)),
        ((2000.0, 120.0), (1500.0, 30.25), (1470, 0, 140, 100)),
        ((2000.0, 120.0), (1860.0, 30.25), (1830, 0, 140, 100)),
        ((4000.0, 120.0), (3860.0, 30.25), (3830, 0, 140, 100)),
        ((16383.0, 120.0), (16289.0, 30.25), (16243, 0, 140, 100)),
        ((120.0, 16384.0), (20.25, 2.0), (0, 0, 120, 100)),
    ];
    for (size, at, window) in rows {
        let Some(misses) = far_misses(true, size, at, window, 2.0) else {
            return;
        };
        assert!(
            misses.is_empty(),
            "at {at:?} on {size:?}: {} pixels off the exact blur, first {:?}",
            misses.len(),
            &misses[..misses.len().min(4)],
        );
    }
}

/// A canvas on `gpu`'s engine with a rectangle stroked 6px wide in flat
/// black through `filter`, or `None` where that engine is not available.
fn stroked(gpu: bool, filter: &str) -> Option<Canvas> {
    let mut canvas = Canvas::new(SIZE.0, SIZE.1);
    canvas.set_gpu(gpu);
    if gpu && canvas.engine_kind() != EngineKind::Gpu {
        return None;
    }
    let ctx = canvas.context();
    ctx.set_stroke_style_css("black").expect("a colour");
    ctx.set_line_width(6.0);
    ctx.set_filter_css(filter).expect("a filter");
    ctx.stroke_rect(40.0, 30.25, 80.0, 40.0);
    Some(canvas)
}

#[test]
fn a_blurred_stroked_rectangle_is_the_exact_gaussian_of_what_it_draws() {
    // A rectangle the closed-form blur does not take, a stroked one, is
    // blurred on its result: Skia's coverage blur, which took it up to a
    // sigma of 40, approximates the Gaussian.
    for gpu in [false, true] {
        for sigma in [2.0f32, 8.0] {
            let filter = format!("blur({sigma}px)");
            let (Some(mut drawn), Some(mut blurred)) =
                (stroked(gpu, "none"), stroked(gpu, &filter))
            else {
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
