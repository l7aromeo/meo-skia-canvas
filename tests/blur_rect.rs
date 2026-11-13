//! A rectangle under a `blur()` filter is its exact Gaussian blur: the
//! product of the blurred interval across and the blurred interval down,
//! each averaged over a pixel, to the precision the pixels hold. On both
//! engines, at every sigma -- there is no point at which the blur changes
//! kind -- at an export's density, and under a transform that keeps the
//! rectangle one.

// Each test file uses part of it.
#[allow(dead_code)]
mod exact;

use exact::profile;
use meo_skia_canvas::prelude::*;

/// How far an 8-bit alpha may lie from the exact blur on the CPU engine:
/// half a level for rounding to a level, and a hundredth for the arithmetic
/// before it. Measured at 0.500 in every case here.
const ROUNDED: f64 = 0.51;

/// The alpha of the translucent colour a case of
/// [`a_rectangle_drawn_after_another_is_its_own_blur`] draws in,
/// `rgba(0, 0, 0, 0.5)`.
const TRANSLUCENT: f64 = 0.5;

/// `alpha`'s byte, rounded half up as a fill's is: 128 of 0.5.
fn alpha_byte(alpha: f64) -> f64 {
    (alpha * f64::from(u8::MAX) + 0.5).floor()
}

/// How far an 8-bit alpha may lie from the exact blur on an engine: the
/// same on both, since the GPU draws the CPU's profiles rounded as the CPU
/// rounds them -- see `blur_mask::RECT_BLUR_SKSL`. Working the profile
/// itself in the GPU's arithmetic, it lay up to 0.859 off.
fn rounded(_gpu: bool) -> f64 {
    ROUNDED
}

/// How far an F16 alpha may lie from the exact blur, as a fraction of one:
/// half a unit in the last place of a half-precision float just below 1,
/// 2^-12, and a little over.
const HALF_FLOAT: f64 = 3.0e-4;

/// How far an F32 alpha may lie from the exact blur: the profiles are
/// worked in doubles and kept as floats, whose unit in the last place below
/// 1 is 6e-8; a product of two and a premultiply stay well within this.
const FULL_FLOAT: f64 = 1.0e-6;

/// The sigmas every case is drawn at, either side of 40 -- where a
/// rectangle's blur used to change from Skia's coverage blur to its image
/// blur -- and either side of its cap of 128 at density 2.
const SIGMAS: [f32; 7] = [0.5, 2.0, 12.0, 40.0, 41.0, 70.0, 100.0];

/// A rectangle with edges off the pixel grid, so that the profiles' pixel
/// averaging is tested where it matters.
const RECT: [f32; 4] = [30.25, 20.5, 60.0, 40.0];

/// The exact blur of `rect` by `sigma` on a `size` canvas, as alpha from 0 to
/// 1.
fn expected(size: (usize, usize), rect: [f64; 4], sigma: f64) -> Vec<f64> {
    let [x, y, w, h] = rect;
    let across = profile(size.0, x, x + w, sigma);
    let down = profile(size.1, y, y + h, sigma);
    down.iter()
        .flat_map(|d| across.iter().map(move |a| a * d))
        .collect()
}

/// How far the blur of `sigma` reaches, and so how far `RECT` is moved in
/// from the corner and how much room is left past it: beyond four sigmas
/// the Gaussian's tail is under a hundredth of a level.
fn reach(sigma: f32) -> f32 {
    (4.0 * sigma).ceil() + 2.0
}

/// `RECT` moved in by the blur's reach, keeping its fractional edges.
fn placed(sigma: f32) -> [f32; 4] {
    let [x, y, w, h] = RECT;
    [x + reach(sigma), y + reach(sigma), w, h]
}

/// The canvas a case draws on: `RECT` placed, and the blur's reach past it.
fn extent(sigma: f32) -> (f32, f32) {
    let [x, y, w, h] = placed(sigma);
    (x + w + reach(sigma), y + h + reach(sigma))
}

fn alpha(data: &ImageData) -> Vec<f64> {
    data.pixels().chunks(4).map(|p| f64::from(p[3])).collect()
}

/// The worst distance of `got`, in the units `scale` gives one, from the
/// exact blur.
fn worst(got: &[f64], want: &[f64], scale: f64) -> f64 {
    got.iter()
        .zip(want)
        .map(|(g, w)| (g - w * scale).abs())
        .fold(0.0, f64::max)
}

/// A canvas for `size` on `gpu`, or `None` where that engine is absent.
fn canvas(size: (f32, f32), gpu: bool) -> Option<Canvas> {
    let mut canvas = Canvas::new(size.0, size.1);
    canvas.set_gpu(gpu);
    (!gpu || canvas.engine_kind() == EngineKind::Gpu).then_some(canvas)
}

#[test]
fn a_blurred_rectangle_is_its_exact_blur_at_every_sigma() {
    for gpu in [false, true] {
        for sigma in SIGMAS {
            for density in [1u32, 2, 3] {
                let size = extent(sigma);
                let Some(mut canvas) = canvas(size, gpu) else {
                    continue;
                };
                let ctx = canvas.context();
                ctx.set_fill_style_css("black").expect("a colour");
                ctx.set_filter_css(&format!("blur({sigma}px)"))
                    .expect("a filter");
                let [x, y, w, h] = placed(sigma);
                ctx.fill_rect(x, y, w, h);
                let data = canvas
                    .to_image_data(
                        None,
                        &ImageDataExport {
                            density,
                            ..ImageDataExport::default()
                        },
                    )
                    .expect("pixels");
                let d = f64::from(density);
                let scaled = (
                    (f64::from(size.0) * d) as usize,
                    (f64::from(size.1) * d) as usize,
                );
                let want = expected(
                    scaled,
                    [x, y, w, h].map(|v| f64::from(v) * d),
                    f64::from(sigma) * d,
                );
                let off = worst(&alpha(&data), &want, 255.0);
                assert!(
                    off <= rounded(gpu),
                    "gpu {gpu} sigma {sigma} density {density}: {off:.3} levels from the exact blur"
                );
            }
        }
    }
}

#[test]
fn a_blurred_rectangle_under_a_transform_that_keeps_it_one_is_exact() {
    // A scale that differs across and down, and a quarter turn. The blur's
    // sigma is in canvas pixels whatever the transform, as a filter's
    // length is; the rectangle is where the transform puts it.
    let sigma = 6.0f32;
    let [x, y, w, h] = [10.25f32, 12.5, 40.0, 20.0];
    for gpu in [false, true] {
        for turn in [false, true] {
            let size = (220.0f32, 200.0f32);
            let Some(mut canvas) = canvas(size, gpu) else {
                continue;
            };
            let ctx = canvas.context();
            ctx.set_fill_style_css("black").expect("a colour");
            ctx.set_filter_css(&format!("blur({sigma}px)"))
                .expect("a filter");
            let device = match turn {
                // A quarter turn about (150, 30): (x, y) goes to (150 - (y -
                // 30), 30 + (x - 0)).
                true => {
                    ctx.translate(150.0, 30.0);
                    ctx.rotate(std::f32::consts::FRAC_PI_2);
                    [150.0 - (y + h), 30.0 + x, h, w]
                }
                false => {
                    ctx.translate(40.0, 50.0);
                    ctx.scale(2.0, 0.5);
                    [40.0 + 2.0 * x, 50.0 + 0.5 * y, 2.0 * w, 0.5 * h]
                }
            };
            ctx.fill_rect(x, y, w, h);
            let data = canvas
                .to_image_data(None, &ImageDataExport::default())
                .expect("pixels");
            let want = expected(
                (size.0 as usize, size.1 as usize),
                device.map(f64::from),
                f64::from(sigma),
            );
            let off = worst(&alpha(&data), &want, 255.0);
            assert!(
                off <= rounded(gpu),
                "gpu {gpu} turned {turn}: {off:.3} levels from the exact blur"
            );
        }
    }
}

#[test]
fn a_blurred_rectangle_on_a_float_canvas_is_exact_to_its_precision() {
    for (depth, tolerance) in
        [(PixelDepth::F16, HALF_FLOAT), (PixelDepth::F32, FULL_FLOAT)]
    {
        for gpu in [false, true] {
            for sigma in [2.0f32, 41.0] {
                let size = extent(sigma);
                let mut canvas = Canvas::with_options(
                    size.0,
                    size.1,
                    CanvasOptions {
                        color_type: depth,
                        ..CanvasOptions::default()
                    },
                )
                .expect("a canvas");
                canvas.set_gpu(gpu);
                if gpu && canvas.engine_kind() != EngineKind::Gpu {
                    continue;
                }
                let ctx = canvas.context();
                ctx.set_fill_style_css("black").expect("a colour");
                ctx.set_filter_css(&format!("blur({sigma}px)"))
                    .expect("a filter");
                let [x, y, w, h] = placed(sigma);
                ctx.fill_rect(x, y, w, h);
                let data = canvas
                    .to_image_data(
                        None,
                        &ImageDataExport {
                            pixels: PixelExportOptions {
                                depth: PixelDepth::F32,
                                ..PixelExportOptions::default()
                            },
                            ..ImageDataExport::default()
                        },
                    )
                    .expect("pixels");
                let got: Vec<f64> = data
                    .pixels()
                    .chunks(16)
                    .map(|p| {
                        f64::from(f32::from_ne_bytes([
                            p[12], p[13], p[14], p[15],
                        ]))
                    })
                    .collect();
                let want = expected(
                    (size.0 as usize, size.1 as usize),
                    [x, y, w, h].map(f64::from),
                    f64::from(sigma),
                );
                let off = worst(&got, &want, 1.0);
                assert!(
                    off <= tolerance,
                    "{depth:?} gpu {gpu} sigma {sigma}: {off:e} from the exact blur"
                );
            }
        }
    }
}

/// How far an F16 alpha may lie from a faint exact blur, as a fraction of
/// it: half a unit in the last place of a half-precision float, 2^-11 of
/// the value, and a millionth for the arithmetic before it.
const HALF_FLOAT_SHARE: f64 = 1.0 / 2048.0 + 1.0e-6;

/// Half of the smallest half-precision float above zero, 2^-24: what an
/// F16 alpha below it rounds to zero by.
const HALF_FLOAT_FLOOR: f64 = 1.0 / (1u32 << 25) as f64;

/// How far an F32 alpha may lie from a faint exact blur, as a fraction of
/// it: the profiles are kept as 32-bit floats, a part in ten million each,
/// and a product of two and a premultiply stay within this.
const FULL_FLOAT_SHARE: f64 = 1.0e-6;

#[test]
fn a_faint_blur_keeps_its_coverage_in_floats() {
    // A blur far wider than its rectangle is faint everywhere: at a sigma of
    // a thousand this one peaks at 2e-4, and at 1e12 at 2e-22. A float
    // pixel holds either, so the blur is held to a share of itself rather
    // than to a distance from it.
    let (size, rect) = ((80.0f32, 60.0f32), [10.25f32, 10.5, 40.0, 30.0]);
    for (depth, share, floor) in [
        (PixelDepth::F16, HALF_FLOAT_SHARE, HALF_FLOAT_FLOOR),
        (PixelDepth::F32, FULL_FLOAT_SHARE, 0.0),
    ] {
        for gpu in [false, true] {
            for sigma in [1e3f32, 1e4, 1e8, 1e12] {
                let mut canvas = Canvas::with_options(
                    size.0,
                    size.1,
                    CanvasOptions {
                        color_type: depth,
                        ..CanvasOptions::default()
                    },
                )
                .expect("a canvas");
                canvas.set_gpu(gpu);
                if gpu && canvas.engine_kind() != EngineKind::Gpu {
                    continue;
                }
                let ctx = canvas.context();
                ctx.set_fill_style_css("black").expect("a colour");
                ctx.set_filter_css(&format!("blur({sigma}px)"))
                    .expect("a filter");
                let [x, y, w, h] = rect;
                ctx.fill_rect(x, y, w, h);
                let data = canvas
                    .to_image_data(
                        None,
                        &ImageDataExport {
                            pixels: PixelExportOptions {
                                depth: PixelDepth::F32,
                                ..PixelExportOptions::default()
                            },
                            ..ImageDataExport::default()
                        },
                    )
                    .expect("pixels");
                let got = data.pixels().chunks(16).map(|p| {
                    f64::from(f32::from_ne_bytes([p[12], p[13], p[14], p[15]]))
                });
                let want = expected(
                    (size.0 as usize, size.1 as usize),
                    rect.map(f64::from),
                    f64::from(sigma),
                );
                let off = got
                    .zip(&want)
                    .map(|(g, w)| (g - w).abs() / (share * w + floor))
                    .fold(0.0, f64::max);
                assert!(
                    off <= 1.0,
                    "{depth:?} gpu {gpu} sigma {sigma}: {off} of its tolerance from the exact blur"
                );
            }
        }
    }
}

#[test]
fn a_rectangle_drawn_after_another_is_its_own_blur() {
    // Blurred rectangles are kept between draws; each of these differs from
    // the one drawn before it in one thing a kept one is filed by, and has
    // to be drawn as itself rather than as the one kept.
    let sigma = 5.0f32;
    let size = (160.0f32, 120.0f32);
    let cases: [(&str, [f32; 4], f32, &str); 5] = [
        ("first", [40.25, 30.5, 50.0, 30.0], sigma, "black"),
        ("width", [40.25, 30.5, 51.0, 30.0], sigma, "black"),
        ("fraction", [40.75, 30.5, 51.0, 30.0], sigma, "black"),
        ("sigma", [40.75, 30.5, 51.0, 30.0], sigma + 1.0, "black"),
        (
            "colour",
            [40.75, 30.5, 51.0, 30.0],
            sigma + 1.0,
            "rgba(0, 0, 0, 0.5)",
        ),
    ];
    for gpu in [false, true] {
        let Some(mut canvas) = canvas(size, gpu) else {
            continue;
        };
        for (what, [x, y, w, h], sigma, colour) in cases {
            let ctx = canvas.context();
            ctx.clear_rect(0.0, 0.0, size.0, size.1);
            ctx.set_fill_style_css(colour).expect("a colour");
            ctx.set_filter_css(&format!("blur({sigma}px)"))
                .expect("a filter");
            ctx.fill_rect(x, y, w, h);
            let data = canvas
                .to_image_data(None, &ImageDataExport::default())
                .expect("pixels");
            // A translucent colour is held to its alpha's byte, 128 of 0.5,
            // as a fill of it stores it.
            let opacity = match colour {
                "black" => f64::from(u8::MAX),
                _ => alpha_byte(TRANSLUCENT),
            };
            let want = expected(
                (size.0 as usize, size.1 as usize),
                [x, y, w, h].map(f64::from),
                f64::from(sigma),
            );
            let off = worst(&alpha(&data), &want, opacity);
            assert!(
                off <= rounded(gpu),
                "gpu {gpu}, {what}: {off:.3} levels from the exact blur"
            );
        }
    }
}

#[test]
fn a_rectangle_on_a_canvas_of_another_kind_is_its_own_blur() {
    // The same rectangle blurred alike, drawn first on an 8-bit sRGB canvas
    // and then on canvases a blur kept from it does not fit: an F16 one,
    // whose pixels hold more than an 8-bit bake does, and a Display P3 one,
    // whose colours are other numbers for the same red.
    let sigma = 3.0f32;
    let size = extent(sigma);
    let [x, y, w, h] = placed(sigma);
    let draw =
        |options: CanvasOptions, colour: &str, read: PixelExportOptions| {
            let mut canvas = Canvas::with_options(size.0, size.1, options)
                .expect("a canvas");
            canvas.set_gpu(false);
            let ctx = canvas.context();
            ctx.set_fill_style_css(colour).expect("a colour");
            ctx.set_filter_css(&format!("blur({sigma}px)"))
                .expect("a filter");
            ctx.fill_rect(x, y, w, h);
            canvas
                .to_image_data(
                    None,
                    &ImageDataExport {
                        pixels: read,
                        ..ImageDataExport::default()
                    },
                )
                .expect("pixels")
                .pixels()
                .to_vec()
        };
    let floats = PixelExportOptions {
        depth: PixelDepth::F32,
        ..PixelExportOptions::default()
    };
    let want = expected(
        (size.0 as usize, size.1 as usize),
        [x, y, w, h].map(f64::from),
        f64::from(sigma),
    );
    draw(
        CanvasOptions::default(),
        "black",
        PixelExportOptions::default(),
    );
    let half = draw(
        CanvasOptions {
            color_type: PixelDepth::F16,
            ..CanvasOptions::default()
        },
        "black",
        floats,
    );
    let got: Vec<f64> = half
        .chunks(16)
        .map(|p| f64::from(f32::from_ne_bytes([p[12], p[13], p[14], p[15]])))
        .collect();
    let off = worst(&got, &want, 1.0);
    assert!(
        off <= HALF_FLOAT,
        "F16 after 8-bit: {off:e} from the exact blur"
    );

    // Red, read back as unpremultiplied floats in the canvas's own space:
    // where the blur has ink, its colour is the fill's whatever the coverage.
    let p3 = CanvasOptions {
        color_space: PixelColorSpace::DisplayP3,
        ..CanvasOptions::default()
    };
    let red_in_p3 = PixelExportOptions {
        depth: PixelDepth::F32,
        color_space: PixelColorSpace::DisplayP3,
        ..PixelExportOptions::default()
    };
    draw(
        CanvasOptions::default(),
        "red",
        PixelExportOptions::default(),
    );
    let blurred = draw(p3.clone(), "red", red_in_p3);
    let solid = {
        let mut canvas = Canvas::with_options(1.0, 1.0, p3).expect("a canvas");
        canvas.set_gpu(false);
        let ctx = canvas.context();
        ctx.set_fill_style_css("red").expect("a colour");
        ctx.fill_rect(0.0, 0.0, 1.0, 1.0);
        canvas
            .to_image_data(
                None,
                &ImageDataExport {
                    pixels: red_in_p3,
                    ..ImageDataExport::default()
                },
            )
            .expect("pixels")
            .pixels()
            .to_vec()
    };
    let channel = |p: &[u8], i: usize| {
        f32::from_ne_bytes([p[i * 4], p[i * 4 + 1], p[i * 4 + 2], p[i * 4 + 3]])
    };
    let middle = {
        let (cx, cy) = ((x + w / 2.0) as usize, (y + h / 2.0) as usize);
        &blurred[(cy * size.0 as usize + cx) * 16..][..16]
    };
    for i in 0..3 {
        assert!(
            (channel(middle, i) - channel(&solid, i)).abs() < 1e-3,
            "P3 after sRGB, channel {i}: {} against the fill's {}",
            channel(middle, i),
            channel(&solid, i)
        );
    }
}

#[test]
fn a_wide_blurred_rectangle_has_no_seam_where_its_interior_meets_its_edges() {
    // Wide against its blur, so most of it is flat: drawn on the CPU as baked
    // edges around a filled interior. Along a row and a column through the
    // middle, every pixel whose exact value lies within a thousandth of the
    // middle's is the same byte -- which takes in the baked edge pixels next
    // to the interior as well as the interior, so a step between the two
    // shows as a byte that differs -- and all of it lies within rounding of
    // the exact blur. The alpha is 0.6, 153 levels, so that the flat value
    // and every value within a thousandth of it round to the same level
    // rather than straddling a half.
    let sigma = 3.0f32;
    let [x, y, w, h] = [20.25f32, 20.5, 300.0, 200.0];
    let size = (340.0f32, 240.0f32);
    for gpu in [false, true] {
        let Some(mut canvas) = canvas(size, gpu) else {
            continue;
        };
        let ctx = canvas.context();
        ctx.set_fill_style_css("rgba(20, 90, 200, 0.6)")
            .expect("a colour");
        ctx.set_filter_css(&format!("blur({sigma}px)"))
            .expect("a filter");
        ctx.fill_rect(x, y, w, h);
        let data = canvas
            .to_image_data(None, &ImageDataExport::default())
            .expect("pixels");
        let (cols, rows) = (size.0 as usize, size.1 as usize);
        let across =
            profile(cols, f64::from(x), f64::from(x + w), f64::from(sigma));
        let down =
            profile(rows, f64::from(y), f64::from(y + h), f64::from(sigma));
        let pixel =
            |c: usize, r: usize| &data.pixels()[(r * cols + c) * 4..][..4];
        let (mid_c, mid_r) = (cols / 2, rows / 2);
        let level =
            |p: &[f64], i: usize, mid: usize| (p[i] - p[mid]).abs() <= 1e-3;
        for (line, name) in [
            (
                (0..cols)
                    .filter(|&c| level(&across, c, mid_c))
                    .map(|c| (c, mid_r))
                    .collect::<Vec<_>>(),
                "row",
            ),
            (
                (0..rows)
                    .filter(|&r| level(&down, r, mid_r))
                    .map(|r| (mid_c, r))
                    .collect::<Vec<_>>(),
                "column",
            ),
        ] {
            assert!(line.len() > 100, "gpu {gpu}: a {name} with a flat run");
            let first = pixel(line[0].0, line[0].1).to_vec();
            for (c, r) in line {
                assert_eq!(
                    pixel(c, r),
                    first.as_slice(),
                    "gpu {gpu}: {name} at ({c}, {r})"
                );
            }
        }
        let want = expected(
            (cols, rows),
            [x, y, w, h].map(f64::from),
            f64::from(sigma),
        );
        let off = worst(&alpha(&data), &want, 0.6 * 255.0);
        assert!(
            off <= rounded(gpu),
            "gpu {gpu}: {off:.3} levels from the exact blur"
        );
    }
}

#[test]
fn a_clipped_blurred_rectangle_is_its_exact_blur_inside_the_clip() {
    // Only what lands inside the clip is made. Inside it the blur is exact;
    // outside it nothing is drawn. The clip cuts the blur's reach on one
    // side, so the part made is not centred on the rectangle.
    let sigma = 6.0f32;
    let [x, y, w, h] = [40.25f32, 30.5, 80.0, 50.0];
    let size = (200.0f32, 140.0f32);
    let [cx, cy, cw, ch] = [60.0f32, 0.0, 140.0, 140.0];
    for gpu in [false, true] {
        let Some(mut canvas) = canvas(size, gpu) else {
            continue;
        };
        let ctx = canvas.context();
        ctx.begin_path();
        ctx.rect(cx, cy, cw, ch);
        ctx.clip(FillRule::NonZero);
        ctx.set_fill_style_css("black").expect("a colour");
        ctx.set_filter_css(&format!("blur({sigma}px)"))
            .expect("a filter");
        ctx.fill_rect(x, y, w, h);
        let data = canvas
            .to_image_data(None, &ImageDataExport::default())
            .expect("pixels");
        let (cols, rows) = (size.0 as usize, size.1 as usize);
        let full = expected(
            (cols, rows),
            [x, y, w, h].map(f64::from),
            f64::from(sigma),
        );
        let want: Vec<f64> = full
            .iter()
            .enumerate()
            .map(|(i, v)| match (i % cols) as f32 >= cx {
                true => *v,
                false => 0.0,
            })
            .collect();
        let off = worst(&alpha(&data), &want, 255.0);
        assert!(
            off <= rounded(gpu),
            "gpu {gpu}: {off:.3} levels from the clipped exact blur"
        );
    }
}

#[test]
fn a_rectangle_blurred_past_any_length_draws_nothing() {
    // A sigma no pixel can resolve spreads the rectangle to nothing: its
    // exact blur rounds to zero everywhere. Its reach is far past what an
    // `i32` holds, which is made only where the canvas is.
    for gpu in [false, true] {
        let Some(mut canvas) = canvas((80.0, 60.0), gpu) else {
            continue;
        };
        let ctx = canvas.context();
        ctx.set_fill_style_css("black").expect("a colour");
        ctx.set_filter_css("blur(1e30px)").expect("a filter");
        ctx.fill_rect(10.0, 10.0, 40.0, 30.0);
        let data = canvas
            .to_image_data(None, &ImageDataExport::default())
            .expect("pixels");
        assert!(alpha(&data).iter().all(|a| *a == 0.0), "gpu {gpu}");
    }
}

/// The eight-bit spaces a blurred rectangle's colour is held on the bytes
/// a fill stores in, each with its CSS name.
const EIGHT_BIT_SPACES: [(PixelColorSpace, &str); 4] = [
    (PixelColorSpace::Srgb, "srgb"),
    (PixelColorSpace::SrgbLinear, "srgb-linear"),
    (PixelColorSpace::DisplayP3, "display-p3"),
    (PixelColorSpace::Rec2020, "rec2020"),
];

/// The eight-bit spaces whose pages hold no exact colour, each with the
/// space its colours are named in: CSS has no `color()` in either, so they
/// are named in sRGB.
const UNPLACED_SPACES: [(PixelColorSpace, &str); 2] = [
    (PixelColorSpace::Rec2020Pq, "srgb"),
    (PixelColorSpace::Rec2020Hlg, "srgb"),
];

/// How far off the byte grid a channel and the alpha are named, in levels:
/// far enough from a byte that rounding first moves the product, and apart
/// from each other so that the two offsets do not cancel.
const OFF_GRID: (f64, f64) = (0.37, 0.61);

/// The levels of an eight-bit channel.
const LEVELS: f64 = 255.0;

/// The alphas the sweeps draw at: every byte, where rounding first and the
/// exact product agree; every byte plus [`OFF_GRID`]'s alpha offset, where
/// they do not; and 0.3, 0.5 and 0.7, whose products are ties.
fn sweep_alphas() -> Vec<f64> {
    let bytes = (1..=u8::MAX).map(|a| f64::from(a) / LEVELS);
    let off = (0..u8::MAX).map(|a| (f64::from(a) + OFF_GRID.1) / LEVELS);
    bytes.chain(off).chain([0.3, 0.5, 0.7]).collect()
}

/// A colour off the byte grid in every channel, which moves with `i` so that
/// the sweep is not one colour at many alphas.
fn off_grid_colour(i: usize) -> [f64; 3] {
    [7, 101, 193].map(|step| ((i * step) % 255) as f64 + OFF_GRID.0)
}

/// `rgb`, in levels, at `alpha`, named in `space`.
fn css_in(space: &str, rgb: [f64; 3], alpha: f64) -> String {
    let [r, g, b] = rgb.map(|c| c / LEVELS);
    format!("color({space} {r} {g} {b} / {alpha})")
}

/// An eight-bit canvas of `size` in `space` on `gpu`, or `None` where that
/// engine is absent.
fn page(size: (f32, f32), space: PixelColorSpace, gpu: bool) -> Option<Canvas> {
    let mut canvas = Canvas::with_options(
        size.0,
        size.1,
        CanvasOptions {
            color_space: space,
            ..CanvasOptions::default()
        },
    )
    .expect("a canvas");
    canvas.set_gpu(gpu);
    (!gpu || canvas.engine_kind() == EngineKind::Gpu).then_some(canvas)
}

/// The premultiplied bytes of `canvas` in its own `space`, row by row.
fn stored(canvas: &mut Canvas, space: PixelColorSpace) -> Vec<u8> {
    canvas
        .to_image_data(
            None,
            &ImageDataExport {
                pixels: PixelExportOptions {
                    color_space: space,
                    premultiplied: true,
                    ..PixelExportOptions::default()
                },
                ..ImageDataExport::default()
            },
        )
        .expect("pixels")
        .pixels()
        .to_vec()
}

#[test]
fn a_blurred_rectangles_interior_stores_what_a_fill_of_its_colour_does() {
    // A rectangle wide against its blur, whose middle is drawn at full
    // coverage, beside a fill of the same colour: the two store the same
    // bytes, on every eight-bit space, at every alpha byte and off the byte
    // grid, where the alpha as given lands a level from the fill's bytes --
    // and on pages with no exact colour, where the GPU's blur took the
    // colour by another route than a fill and rounded near-ties apart.
    let size = (40.0f32, 20.0f32);
    let width = size.0 as usize;
    let (blurred, filled) = ((10, 10), (30, 10));
    for gpu in [false, true] {
        for (space, name) in EIGHT_BIT_SPACES.into_iter().chain(UNPLACED_SPACES)
        {
            let Some(mut canvas) = page(size, space, gpu) else {
                continue;
            };
            let mut misses = Vec::new();
            for (i, alpha) in sweep_alphas().into_iter().enumerate() {
                let css = css_in(name, off_grid_colour(i), alpha);
                let ctx = canvas.context();
                ctx.clear_rect(0.0, 0.0, size.0, size.1);
                ctx.set_fill_style_css(&css).expect("a colour");
                ctx.set_filter_css("blur(0.5px)").expect("a filter");
                ctx.fill_rect(2.0, 2.0, 16.0, 16.0);
                ctx.set_filter_css("none").expect("no filter");
                ctx.fill_rect(22.0, 2.0, 16.0, 16.0);
                let pixels = stored(&mut canvas, space);
                let at = |(x, y): (usize, usize)| {
                    let i = (y * width + x) * 4;
                    [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
                };
                if at(blurred) != at(filled) {
                    misses.push(format!(
                        "{css}: {:?} against {:?}",
                        at(blurred),
                        at(filled)
                    ));
                }
            }
            assert!(
                misses.is_empty(),
                "gpu {gpu}, {space:?}: {} of {} missed, first {:?}",
                misses.len(),
                sweep_alphas().len(),
                &misses[..misses.len().min(4)]
            );
        }
    }
}

#[test]
fn a_blurred_rectangle_is_a_fills_bytes_times_its_blur_inside_and_out() {
    // Narrow against its blur, so the interior's coverage is well under one:
    // the frame baked around it and the interior filled in it scale the
    // same bytes, a fill's, and each pixel lies within rounding of those
    // times the exact blur. A frame and an interior scaling different
    // colours meet at a step.
    let (size, sigma) = ((48.0f32, 56.0f32), 3.0f32);
    let [x, y, w, h] = [20.25f32, 10.5, 6.0, 34.0];
    let want = expected(
        (size.0 as usize, size.1 as usize),
        [x, y, w, h].map(f64::from),
        f64::from(sigma),
    );
    for gpu in [false, true] {
        for (space, name) in EIGHT_BIT_SPACES {
            let Some(mut canvas) = page(size, space, gpu) else {
                continue;
            };
            let mut worst_off = 0.0f64;
            let mut at = String::new();
            for (i, alpha) in [0.3, 0.5, 0.7, 129.49 / LEVELS, 200.49 / LEVELS]
                .into_iter()
                .enumerate()
            {
                let rgb = off_grid_colour(i + 1);
                let css = css_in(name, rgb, alpha);
                let ctx = canvas.context();
                ctx.clear_rect(0.0, 0.0, size.0, size.1);
                ctx.set_fill_style_css(&css).expect("a colour");
                ctx.set_filter_css(&format!("blur({sigma}px)"))
                    .expect("a filter");
                ctx.fill_rect(x, y, w, h);
                let blurred = stored(&mut canvas, space);
                // A fill's bytes, read from a fill of the colour.
                let ctx = canvas.context();
                ctx.clear_rect(0.0, 0.0, size.0, size.1);
                ctx.set_filter_css("none").expect("no filter");
                ctx.fill_rect(0.0, 0.0, size.0, size.1);
                let fill = stored(&mut canvas, space);
                let bytes = [fill[0], fill[1], fill[2], fill[3]].map(f64::from);
                for (p, coverage) in blurred.chunks(4).zip(&want) {
                    for (got, byte) in p.iter().zip(bytes) {
                        let off = (f64::from(*got) - byte * coverage).abs();
                        if off > worst_off {
                            worst_off = off;
                            at = css.clone();
                        }
                    }
                }
            }
            assert!(
                worst_off <= ROUNDED,
                "gpu {gpu}, {name}: {worst_off:.3} levels from a fill's bytes \
                 times the exact blur, at {at}"
            );
        }
    }
}

#[test]
fn a_blurred_rectangle_on_float_pixels_keeps_its_alpha_as_given() {
    // Float pixels hold an alpha of 0.5 as itself: the blur scales it, not
    // the 128 255ths an eight-bit page stores, which lies 0.002 from it. In
    // sRGB, which keeps no exact colour on floats, and in Display P3, which
    // does.
    let sigma = 3.0f32;
    let size = extent(sigma);
    let [x, y, w, h] = placed(sigma);
    let want = expected(
        (size.0 as usize, size.1 as usize),
        [x, y, w, h].map(f64::from),
        f64::from(sigma),
    );
    for gpu in [false, true] {
        for (space, depth) in
            [PixelColorSpace::Srgb, PixelColorSpace::DisplayP3]
                .into_iter()
                .flat_map(|s| [(s, PixelDepth::F16), (s, PixelDepth::F32)])
        {
            let mut canvas = Canvas::with_options(
                size.0,
                size.1,
                CanvasOptions {
                    color_type: depth,
                    color_space: space,
                    ..CanvasOptions::default()
                },
            )
            .expect("a canvas");
            canvas.set_gpu(gpu);
            if gpu && canvas.engine_kind() != EngineKind::Gpu {
                continue;
            }
            let ctx = canvas.context();
            ctx.set_fill_style_css("rgba(0, 0, 0, 0.5)")
                .expect("a colour");
            ctx.set_filter_css(&format!("blur({sigma}px)"))
                .expect("a filter");
            ctx.fill_rect(x, y, w, h);
            let data = canvas
                .to_image_data(
                    None,
                    &ImageDataExport {
                        pixels: PixelExportOptions {
                            depth: PixelDepth::F32,
                            ..PixelExportOptions::default()
                        },
                        ..ImageDataExport::default()
                    },
                )
                .expect("pixels");
            let got: Vec<f64> = data
                .pixels()
                .chunks(16)
                .map(|p| {
                    f64::from(f32::from_ne_bytes([p[12], p[13], p[14], p[15]]))
                })
                .collect();
            let off = worst(&got, &want, TRANSLUCENT);
            assert!(
                off <= HALF_FLOAT,
                "gpu {gpu}, {space:?} {depth:?}: {off:e} from the exact blur \
                 at 0.5"
            );
        }
    }
}

/// The sigmas [`the_gpu_draws_a_blurred_rectangle_to_the_cpus_bytes`]
/// sweeps: under a pixel, where the GPU's own profile lay 0.859 off, about
/// the one-time quadrature switch at 4, and well past it.
const EVERY_SIGMA: [f32; 11] =
    [0.25, 0.5, 1.0, 1.5, 2.0, 3.0, 4.0, 5.0, 8.0, 12.0, 40.0];

/// How near a half-level an exact value has to lie for two engines to round
/// it apart: the GPU's product and the CPU's are the same `f32` operations,
/// and differ only where a fused multiply-add or the profile's 24-bit
/// texel moves a value lying on a tie.
const TIE: f64 = 0.01;

/// Where a transform puts a rectangle `[x, y, w, h]` on the device.
type OnDevice = fn([f32; 4]) -> [f64; 4];

/// A rectangle's axes are kept by where its edges lie in their window, not
/// by where the window lies, so a rectangle the size of one drawn before,
/// lying alike within a pixel, draws from the kept axes -- and lands where
/// it is drawn: the GPU stores the CPU's bytes, ties aside.
#[test]
fn a_rectangle_drawn_again_elsewhere_lands_where_it_is_drawn() {
    let size = (220.0f32, 160.0f32);
    let draw = |gpu: bool| -> Option<Vec<u8>> {
        let mut canvas = page(size, PixelColorSpace::Srgb, gpu)?;
        let ctx = canvas.context();
        ctx.set_filter_css("blur(3px)").expect("a filter");
        for (x, y) in [(20.25, 15.5), (120.25, 95.5)] {
            ctx.fill_rect(x, y, 41.3, 27.6);
        }
        Some(stored(&mut canvas, PixelColorSpace::Srgb))
    };
    let (Some(cpu), Some(gpu)) = (draw(false), draw(true)) else {
        return;
    };
    let apart = cpu
        .iter()
        .zip(&gpu)
        .filter(|(c, g)| c.abs_diff(**g) > 1)
        .count();
    assert_eq!(apart, 0, "bytes apart by more than a tie");
}

#[test]
fn the_gpu_draws_a_blurred_rectangle_to_the_cpus_bytes() {
    // Both engines draw the CPU's profiles, so the GPU lands the CPU's
    // bytes, each within rounding of the exact blur: at every sigma, in an
    // opaque colour, a translucent one and under a `globalAlpha`, for a
    // rectangle on and off the pixel grid, at a scale and turned a quarter.
    // A turn that leaves the rectangle unaligned is a path's blur, the
    // chain's -- see `tests/blur_result.rs`.
    let size = (220.0f32, 160.0f32);
    let n = (size.0 as usize, size.1 as usize);
    let transforms: [(&str, [f32; 6], OnDevice); 3] = [
        ("identity", [1.0, 0.0, 0.0, 1.0, 0.0, 0.0], |r| {
            r.map(f64::from)
        }),
        ("scale 2", [2.0, 0.0, 0.0, 2.0, 0.0, 0.0], |[x, y, w, h]| {
            [x, y, w, h].map(|v| f64::from(v) * 2.0)
        }),
        // A quarter turn about the origin, moved back onto the page: x' =
        // 200 - y, y' = x.
        (
            "quarter turn",
            [0.0, 1.0, -1.0, 0.0, 200.0, 0.0],
            |[x, y, w, h]| {
                let [x, y, w, h] = [x, y, w, h].map(f64::from);
                [200.0 - y - h, x, h, w]
            },
        ),
    ];
    let rects: [[f32; 4]; 2] =
        [[30.25, 20.5, 45.0, 30.0], [41.3, 33.7, 37.45, 21.85]];
    let paints = [
        ("black", 1.0f32),
        ("rgba(200, 80, 20, 0.6)", 1.0),
        ("rgb(30 160 90)", 0.45),
        ("color(srgb 0.137 0.981 0.786 / 0.786)", 1.0),
    ];
    let draw = |gpu: bool,
                transform: [f32; 6],
                rect: [f32; 4],
                paint: &str,
                global_alpha: f32,
                filter: &str|
     -> Option<Vec<u8>> {
        let mut canvas = page(size, PixelColorSpace::Srgb, gpu)?;
        let ctx = canvas.context();
        ctx.set_global_alpha(global_alpha);
        ctx.set_fill_style_css(paint).expect("a colour");
        ctx.set_filter_css(filter).expect("a filter");
        let [a, b, c, d, tx, ty] = transform;
        ctx.set_transform(Affine { a, b, c, d, tx, ty });
        let [x, y, w, h] = rect;
        ctx.fill_rect(x, y, w, h);
        Some(stored(&mut canvas, PixelColorSpace::Srgb))
    };
    let mut wrong = vec![];
    for (name, transform, device) in transforms {
        for rect in rects {
            for (paint, global_alpha) in paints {
                // The bytes a fill of the paint stores: what the blur scales.
                let Some(fill) = draw(
                    false,
                    [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
                    [0.0, 0.0, size.0, size.1],
                    paint,
                    global_alpha,
                    "none",
                ) else {
                    continue;
                };
                let fill = [fill[0], fill[1], fill[2], fill[3]].map(f64::from);
                for sigma in EVERY_SIGMA {
                    let filter = format!("blur({sigma}px)");
                    let (Some(cpu), Some(gpu)) = (
                        draw(
                            false,
                            transform,
                            rect,
                            paint,
                            global_alpha,
                            &filter,
                        ),
                        draw(
                            true,
                            transform,
                            rect,
                            paint,
                            global_alpha,
                            &filter,
                        ),
                    ) else {
                        continue;
                    };
                    let want = expected(n, device(rect), f64::from(sigma));
                    let (mut apart, mut off, mut worst) = (0, 0, 0.0f64);
                    let mut by_engine = [0.0f64; 2];
                    let mut at = (0usize, 0.0f64, 0u8, 0u8);
                    for (i, (c, g)) in cpu.iter().zip(&gpu).enumerate() {
                        let exact = fill[i % 4] * want[i / 4];
                        for (e, got) in [c, g].into_iter().enumerate() {
                            let d = (f64::from(*got) - exact).abs();
                            worst = worst.max(d);
                            if d > by_engine[e] {
                                by_engine[e] = d;
                                if e == 1 {
                                    at = (i, want[i / 4], *c, *g);
                                }
                            }
                            if d > ROUNDED {
                                off += 1;
                            }
                        }
                        let tie = (exact.fract() - 0.5).abs() < TIE;
                        if c != g && !tie {
                            apart += 1;
                        }
                    }
                    if apart > 0 || off > 0 {
                        wrong.push(format!(
                            "{name} {rect:?} {paint} at {global_alpha} sigma \
                             {sigma}: {apart} bytes apart off a tie, {off} \
                             off the exact blur, worst {worst:.3}, cpu \
                             {:.3} gpu {:.3} at byte {} coverage {:.4} cpu \
                             {} gpu {}",
                            by_engine[0], by_engine[1], at.0, at.1, at.2, at.3
                        ));
                    }
                }
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}
