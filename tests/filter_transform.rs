//! A CSS filter's lengths are in the canvas's pixels and on its axes,
//! whatever the transform -- the figures here are Chrome's, measured by the
//! same centroids -- while a Skia image filter's are in local units, as
//! CanvasKit draws them.

use std::f32::consts::FRAC_PI_2;

use meo_skia_canvas::prelude::*;

const SIDE: usize = 200;

/// The centre every transform here turns or stretches about.
const CENTRE: f32 = 100.0;

/// How far a centroid may sit from where Chrome puts it. The shadow is the
/// same shape as what casts it, so the two centroids differ by the offset
/// exactly where both are drawn alike; under a skew the shadow is made in
/// the layer Skia draws a filtered draw through and resampled from it, and
/// lands up to 0.03 of a pixel from the square's own -- against a whole
/// pixel or more for an offset taken along the wrong axes.
const CENTROID: f64 = 0.05;

type Transform = fn(&mut Context2D);

/// Transforms about [`CENTRE`]: the axis-aligned ones the old construction
/// got right, and the ones that turn the canvas's axes.
const TRANSFORMS: [(&str, Transform); 6] = [
    ("none", |_| {}),
    ("scale(2)", |c| about(c, |c| c.scale(2.0, 2.0))),
    ("scale(2, 1)", |c| about(c, |c| c.scale(2.0, 1.0))),
    ("rotate(30deg)", |c| about(c, |c| c.rotate(FRAC_PI_2 / 3.0))),
    ("rotate(90deg)", |c| about(c, |c| c.rotate(FRAC_PI_2))),
    ("skewX", |c| {
        about(c, |c| {
            c.transform(Affine {
                a: 1.0,
                b: 0.0,
                c: 0.5,
                d: 1.0,
                tx: 0.0,
                ty: 0.0,
            })
        })
    }),
];

fn about(ctx: &mut Context2D, transform: impl FnOnce(&mut Context2D)) {
    ctx.translate(CENTRE, CENTRE);
    transform(ctx);
    ctx.translate(-CENTRE, -CENTRE);
}

/// A canvas of `depth` on `gpu`'s engine, `None` where that engine is not
/// available.
fn canvas(depth: PixelDepth, gpu: bool) -> Option<Canvas> {
    let canvas = Canvas::with_options(
        SIDE as f32,
        SIDE as f32,
        CanvasOptions {
            color_type: depth,
            gpu,
            ..CanvasOptions::default()
        },
    )
    .expect("a canvas");
    (!gpu || canvas.engine_kind() == EngineKind::Gpu).then_some(canvas)
}

/// The canvas's premultiplied sRGB pixels as floats.
fn pixels(canvas: &mut Canvas) -> Vec<[f32; 4]> {
    let data = canvas
        .context()
        .get_image_data_as(
            0.0,
            0.0,
            SIDE as f32,
            SIDE as f32,
            PixelExportOptions {
                depth: PixelDepth::F32,
                color_space: PixelColorSpace::Srgb,
                premultiplied: true,
                ..PixelExportOptions::default()
            },
        )
        .expect("pixels");
    let floats: Vec<f32> = data
        .pixels()
        .as_chunks::<4>()
        .0
        .iter()
        .map(|bytes| f32::from_le_bytes(*bytes))
        .collect();
    floats.as_chunks::<4>().0.to_vec()
}

/// Where the red ink's centroid lies from the blue ink's.
fn red_from_blue(pixels: &[[f32; 4]]) -> (f64, f64) {
    let centroid = |channel: usize| {
        let (x, y, weight) = pixels.iter().enumerate().fold(
            (0.0, 0.0, 0.0),
            |(x, y, weight), (i, pixel)| {
                let ink = pixel[channel] as f64;
                let (px, py) =
                    ((i % SIDE) as f64 + 0.5, (i / SIDE) as f64 + 0.5);
                (x + px * ink, y + py * ink, weight + ink)
            },
        );
        (x / weight, y / weight)
    };
    let (red, blue) = (centroid(0), centroid(2));
    (red.0 - blue.0, red.1 - blue.1)
}

/// A six-pixel blue square at the centre under `transform`, through
/// `filter`.
fn square(
    depth: PixelDepth,
    gpu: bool,
    transform: Transform,
    filter: impl FnOnce(&mut Context2D),
) -> Option<Vec<[f32; 4]>> {
    let mut canvas = canvas(depth, gpu)?;
    let ctx = canvas.context();
    transform(ctx);
    filter(ctx);
    ctx.set_fill_style_css("blue").expect("a colour");
    ctx.fill_rect(CENTRE - 3.0, CENTRE - 3.0, 6.0, 6.0);
    Some(pixels(&mut canvas))
}

#[test]
fn a_drop_shadow_lands_on_the_canvas_axes_under_any_transform() {
    // Chrome: 12, 4 under every one of these, as `shadowOffsetX` and
    // `shadowOffsetY` are. Divided by the transform's diagonal instead, the
    // offset landed at 9.69, 10.93 under the 30-degree turn, and under the
    // quarter turn the diagonal is 0 and the shadow was not drawn at all.
    for depth in [PixelDepth::Uint8, PixelDepth::F32] {
        for gpu in [false, true] {
            for (name, transform) in TRANSFORMS {
                let Some(pixels) = square(depth, gpu, transform, |c| {
                    c.set_filter_css("drop-shadow(12px 4px 0 red)")
                        .expect("a filter")
                }) else {
                    continue;
                };
                let (x, y) = red_from_blue(&pixels);
                assert!(
                    (x - 12.0).abs() < CENTROID && (y - 4.0).abs() < CENTROID,
                    "{depth:?}, gpu {gpu}, {name}: the shadow lies {x:.2}, \
                     {y:.2} from the square"
                );
            }
        }
    }
}

/// Twice the distance between where `line` first reaches 15.87% and 84.13%
/// of its peak, which for a Gaussian edge is one standard deviation.
fn edge_sigma(line: &[f32]) -> f32 {
    let peak = line.iter().copied().fold(0.0, f32::max);
    let at = |fraction: f32| {
        let level = fraction * peak;
        (1..line.len())
            .find(|&i| line[i] >= level)
            .map(|i| {
                (i - 1) as f32 + (level - line[i - 1]) / (line[i] - line[i - 1])
            })
            .expect("the line reaches the level")
    };
    (at(0.8413) - at(0.1587)) / 2.0
}

/// The red shadow's edge, across the row through its centre, of a blue disc
/// at the centre under `transform` casting `drop-shadow(-60px 0 4px red)`.
/// A disc is the same under a turn, so the edge is too; the shadow lies
/// well clear of the disc.
fn shadow_edge(
    depth: PixelDepth,
    gpu: bool,
    transform: Transform,
) -> Option<f32> {
    let mut canvas = canvas(depth, gpu)?;
    let ctx = canvas.context();
    transform(ctx);
    ctx.set_filter_css("drop-shadow(-60px 0 4px red)")
        .expect("a filter");
    ctx.set_fill_style_css("blue").expect("a colour");
    ctx.begin_path();
    ctx.arc(CENTRE, CENTRE, 15.0, 0.0, 4.0 * FRAC_PI_2, false)
        .expect("a radius");
    ctx.fill(FillRule::NonZero);
    let pixels = pixels(&mut canvas);
    let row = CENTRE as usize * SIDE;
    let red: Vec<f32> = (0..CENTRE as usize - 60)
        .map(|x| pixels[row + x][0])
        .collect();
    Some(edge_sigma(&red))
}

#[test]
fn a_drop_shadows_blur_keeps_its_length_under_a_turn() {
    // Chrome blurs a `blur(4px)` edge to the same standard deviation under
    // every transform. Divided by the diagonal, a 30-degree turn blurred
    // the shadow by 4 / cos 30deg, and a quarter turn by an infinite one.
    // Compared with the same canvas untransformed rather than with a
    // figure, since the edge of a disc is not a straight Gaussian edge.
    for depth in [PixelDepth::Uint8, PixelDepth::F32] {
        for gpu in [false, true] {
            let Some(upright) = shadow_edge(depth, gpu, TRANSFORMS[0].1) else {
                continue;
            };
            for (name, transform) in [TRANSFORMS[3], TRANSFORMS[4]] {
                let turned = shadow_edge(depth, gpu, transform)
                    .expect("the engine drew the upright disc");
                assert!(
                    (turned - upright).abs() < 0.1,
                    "{depth:?}, gpu {gpu}, {name}: the shadow's edge is \
                     {turned:.2} against {upright:.2} upright"
                );
            }
        }
    }
}

#[test]
fn a_skia_image_filters_lengths_are_local_units() {
    // CanvasKit's semantics, and Skia's: the filter is evaluated in the
    // space the draw is made in, so its offset turns and stretches with the
    // transform.
    let expected: [(f64, f64); 6] = [
        (12.0, 4.0),
        (24.0, 8.0),
        (24.0, 4.0),
        (8.392, 9.464),
        (-4.0, 12.0),
        (14.0, 4.0),
    ];
    for depth in [PixelDepth::Uint8, PixelDepth::F32] {
        for gpu in [false, true] {
            for ((name, transform), (ex, ey)) in
                TRANSFORMS.into_iter().zip(expected)
            {
                let Some(pixels) = square(depth, gpu, transform, |c| {
                    let shadow = ImageFilter::drop_shadow(
                        12.0,
                        4.0,
                        0.0,
                        0.0,
                        RgbaLinear::new_premultiplied(1.0, 0.0, 0.0, 1.0),
                        None,
                        None,
                    )
                    .expect("a filter");
                    c.set_image_filter(Some(&shadow));
                }) else {
                    continue;
                };
                let (x, y) = red_from_blue(&pixels);
                assert!(
                    (x - ex).abs() < CENTROID && (y - ey).abs() < CENTROID,
                    "{depth:?}, gpu {gpu}, {name}: the shadow lies {x:.3}, \
                     {y:.3} from the square, not {ex}, {ey}"
                );
            }
        }
    }
}

#[test]
fn a_drop_shadow_is_built_again_for_a_turn_the_other_way() {
    // Turning by 30 degrees either way leaves the transform's diagonal the
    // same and its skews opposite, so a filter kept by the diagonal alone
    // was the first turn's for the second.
    for gpu in [false, true] {
        let Some(mut canvas) = canvas(PixelDepth::Uint8, gpu) else {
            continue;
        };
        let ctx = canvas.context();
        ctx.set_filter_css("drop-shadow(12px 4px 0 red)")
            .expect("a filter");
        ctx.set_fill_style_css("blue").expect("a colour");
        for turn in [1.0, -1.0] {
            let ctx = canvas.context();
            ctx.reset_transform();
            ctx.clear_rect(0.0, 0.0, SIDE as f32, SIDE as f32);
            // Not inside `save` and `restore`, which would take the kept
            // filter away with the state.
            ctx.reset_transform();
            about(ctx, |c| c.rotate(turn * FRAC_PI_2 / 3.0));
            ctx.fill_rect(CENTRE - 3.0, CENTRE - 3.0, 6.0, 6.0);
            let (x, y) = red_from_blue(&pixels(&mut canvas));
            assert!(
                (x - 12.0).abs() < CENTROID && (y - 4.0).abs() < CENTROID,
                "gpu {gpu}, turned {turn:+}: the shadow lies {x:.2}, {y:.2} \
                 from the square"
            );
        }
    }
}

/// Alpha across the row through the centre of a 40px blue square at the
/// centre under `transform`, blurred by `blur(4px)` beside a Skia image
/// filter that leaves it alone, so that Skia draws the whole filter.
fn blurred_beside_skia(gpu: bool, transform: Transform) -> Option<Vec<u8>> {
    let mut canvas = canvas(PixelDepth::Uint8, gpu)?;
    let ctx = canvas.context();
    transform(ctx);
    ctx.set_filter_css("blur(4px)").expect("a filter");
    let unmoved = ImageFilter::offset(0.0, 0.0, None, None).expect("a filter");
    ctx.set_image_filter(Some(&unmoved));
    ctx.set_fill_style_css("blue").expect("a colour");
    ctx.fill_rect(CENTRE - 20.0, CENTRE - 20.0, 40.0, 40.0);
    let data = ctx
        .get_image_data(0.0, CENTRE, SIDE as f32, 1.0)
        .expect("pixels");
    Some(data.pixels().chunks(4).map(|pixel| pixel[3]).collect())
}

#[test]
fn a_blur_skia_draws_keeps_its_length_under_a_quarter_turn() {
    // The square is the same turned a quarter, so its blur is too. Divided
    // by the transform's diagonal, which a quarter turn makes 0, the blur
    // was infinite and Skia drew the square unblurred.
    for gpu in [false, true] {
        let Some(upright) = blurred_beside_skia(gpu, TRANSFORMS[0].1) else {
            continue;
        };
        let turned = blurred_beside_skia(gpu, TRANSFORMS[4].1)
            .expect("the engine drew the upright square");
        let worst = upright
            .iter()
            .zip(&turned)
            .map(|(a, b)| a.abs_diff(*b))
            .max();
        assert!(
            worst <= Some(1),
            "gpu {gpu}: the turned blur is {worst:?} levels from the \
             upright one: {:?} against {:?}",
            &turned[70..86],
            &upright[70..86]
        );
    }
}
