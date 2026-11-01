//! On an eight-bit sRGB canvas a shadow stores the exact product of its
//! colour and its alpha, rounded once, on whichever engine draws the
//! canvas: `shadowColor`, a CSS `drop-shadow()`, a CanvasKit drop shadow,
//! and a CanvasKit blend filter over an opaque fill, each cast by or drawn
//! over an opaque shape -- the blend on the GPU; on the raster backend it is
//! a fill, and stores what a fill does until #440. The product is the byte
//! 7.0 stores for a translucent flat colour on such a canvas; the raster
//! backend's fill rounds the colour and its alpha to bytes first and
//! premultiplies the bytes, and the two part on about one case in eight
//! here. The CSS shadow, run by this crate on the raster backend, stored
//! that rounded fill's 69 for `color(srgb-linear 0.25 0.25 0.25 / 0.5)`
//! where the product is 68.
//!
//! The cases lie off the bytes -- see [`OFF_BYTE`] -- since on whole bytes
//! the two rules store the same, and a sweep of bytes, kept here as the
//! control, cannot tell them apart.
//!
//! The recording a canvas keeps is the same on both engines and is drawn by
//! whichever the canvas uses when it is read, so the same holds when
//! `canvas.gpu` changes between drawing and reading.

use meo_skia_canvas::prelude::*;

/// The engines a canvas is drawn by. Where no GPU is reachable the second
/// is the raster backend again, which the test prints rather than fails.
const ENGINES: [bool; 2] = [false, true];

/// How far a shadow is cast from the shape casting it, and where a
/// filtered fill is drawn: past the row of shapes.
const ROW: f32 = 256.0;

/// Where inside its byte a case's grey and alpha lie, in levels: off the
/// byte, so that rounding them to bytes before the product -- which the
/// raster backend's fill does -- parts from rounding the product once. Of
/// two bytes the two rules agree, `SkMulDiv255Round` being the product of
/// the bytes rounded, and a sweep of bytes cannot tell them apart. Byte 255
/// is the top of the range and stays whole.
const OFF_BYTE: (f64, f64) = (0.37, 0.61);

/// How near a half of a level a product may lie and still be held to one
/// byte: the colour reaches Skia as `f32`, through a conversion for the
/// CanvasKit routes, which moves a product by about 1e-4 of a level.
const NEAR_HALF: f64 = 1e-3;

/// Whole bytes: the control, which the two rules store alike, so that a
/// miss there is the readback's or the parser's and not the blend's.
const ON_BYTE: (f64, f64) = (0.0, 0.0);

/// Bucket `i` of 256 at its offset `off` in levels, 255 whole.
fn level(i: u32, off: f64) -> f64 {
    match i {
        255 => 255.0,
        _ => f64::from(i) + off,
    }
}

/// A grey of bucket `c` at the alpha of bucket `a`, each at its offset in
/// `at`, as CSS names it.
fn grey(c: u32, a: u32, at: (f64, f64)) -> String {
    let v = level(c, at.0);
    format!("rgb({v} {v} {v} / {})", level(a, at.1) / 255.0)
}

/// The premultiplied bytes the grey of bucket `c` at the alpha of bucket
/// `a`, at the offsets `at`, is stored as: the product of the two, rounded
/// once, half up. `None` where the product lies within [`NEAR_HALF`] of a
/// half.
fn product(c: u32, a: u32, at: (f64, f64)) -> Option<[u8; 4]> {
    let (v, alpha) = (level(c, at.0), level(a, at.1));
    let exact = v * alpha / 255.0;
    if (exact.fract() - 0.5).abs() < NEAR_HALF {
        return None;
    }
    let channel = (exact + 0.5).floor() as u8;
    Some([channel, channel, channel, (alpha + 0.5).floor() as u8])
}

/// The ways a colour reaches a shadow, or a filter's colour a fill.
#[derive(Clone, Copy, Debug)]
enum Route {
    ShadowColor,
    CssDropShadow,
    CanvasKitDropShadow,
    CanvasKitBlend,
}

impl Route {
    const ALL: [Self; 4] = [
        Self::ShadowColor,
        Self::CssDropShadow,
        Self::CanvasKitDropShadow,
        Self::CanvasKitBlend,
    ];

    /// This route's shadow of an opaque one-pixel shape at `x`, or its
    /// filtered opaque fill, in `css`, landing at `x + ROW` in row 0.
    fn draw(self, ctx: &mut Context2D, x: f32, css: &str) {
        ctx.set_fill_style_css("white").expect("white");
        match self {
            Self::ShadowColor => {
                ctx.set_shadow_color_css(css).expect("a colour");
                ctx.set_shadow_offset(ROW, 0.0);
                ctx.fill_rect(x, 0.0, 1.0, 1.0);
                ctx.set_shadow_color_css("transparent").expect("a colour");
            }
            Self::CssDropShadow => {
                ctx.set_filter_css(&format!(
                    "drop-shadow({ROW}px 0px 0px {css})"
                ))
                .expect("a filter");
                ctx.fill_rect(x, 0.0, 1.0, 1.0);
                ctx.set_filter_css("none").expect("no filter");
            }
            Self::CanvasKitDropShadow => {
                let colour = RgbaLinear::from_css(css).expect("a colour");
                let shadow = ImageFilter::drop_shadow_only(
                    ROW, 0.0, 0.0, 0.0, colour, None, None,
                )
                .expect("a shadow");
                ctx.set_image_filter(Some(&shadow));
                ctx.fill_rect(x, 0.0, 1.0, 1.0);
                ctx.set_image_filter(None);
            }
            Self::CanvasKitBlend => {
                let colour = RgbaLinear::from_css(css).expect("a colour");
                let blend = ColorFilter::blend(colour, BlendMode::SourceIn)
                    .expect("a blend");
                ctx.set_color_filter(Some(&blend));
                ctx.fill_rect(ROW + x, 0.0, 1.0, 1.0);
                ctx.set_color_filter(None);
            }
        }
    }
}

/// The premultiplied bytes of row 0 of `canvas`, read by `gpu`.
fn row(canvas: &mut Canvas, gpu: bool) -> Vec<u8> {
    canvas.set_gpu(gpu);
    let options = PixelExportOptions {
        premultiplied: true,
        ..PixelExportOptions::default()
    };
    canvas
        .context()
        .get_image_data_as(0.0, 0.0, 2.0 * ROW, 1.0, options)
        .expect("the row")
        .pixels()
        .to_vec()
}

/// Whether `route` read on `engine` is held out of the exact sweep: a
/// CanvasKit blend filter over a fill on the raster backend, which Skia
/// folds into the paint's colour and blits as it blits any fill, rounding
/// the colour and its alpha to bytes before the product -- 8,186 of the
/// 65,536 off-byte cases store a level off it. That is the raster
/// backend's fill rule, which #440 changes; until then
/// [`a_blend_filter_over_a_fill_on_the_cpu_misses_until_440`] holds the
/// count. Asked of the engine the read runs on, not the one it asked for:
/// a machine with no GPU reads a canvas set to it on the raster backend.
fn known_miss(route: Route, engine: EngineKind) -> bool {
    matches!(route, Route::CanvasKitBlend) && engine == EngineKind::Cpu
}

/// What `routes` store for every grey at every alpha, at the offsets `at`,
/// against the exact product: each drawn with every engine in `drawn` and
/// read with every engine in `read`, a row of canvas per alpha, so that no
/// canvas holds more than 256 filtered draws -- Ganesh's playback of a
/// page's filtered draws grows faster than their number. `held_out` names
/// the (route, engine read on) pairs left out, the engine the one the
/// canvas reports. The misses, described.
fn misses(
    routes: &[Route],
    at: (f64, f64),
    drawn: &[bool],
    read: &[bool],
    held_out: impl Fn(Route, EngineKind) -> bool,
) -> Vec<String> {
    let mut missed = vec![];
    for &route in routes {
        for a in 0..=255 {
            for &drawn in drawn {
                let mut canvas = Canvas::with_options(
                    2.0 * ROW,
                    1.0,
                    CanvasOptions::default(),
                )
                .expect("a canvas");
                canvas.set_gpu(drawn);
                for c in 0..=255 {
                    route.draw(canvas.context(), c as f32, &grey(c, a, at));
                }
                for &read in read {
                    canvas.set_gpu(read);
                    if held_out(route, canvas.engine_kind()) {
                        continue;
                    }
                    let pixels = row(&mut canvas, read);
                    let (pixels, _) = pixels.as_chunks::<4>();
                    for c in 0..=255 {
                        let Some(want) = product(c, a, at) else {
                            continue;
                        };
                        let stored = pixels[256 + c as usize];
                        if stored != want {
                            missed.push(format!(
                                "{route:?} drawn gpu {drawn} read gpu {read}, \
                                 {}: {stored:?} for {want:?}",
                                grey(c, a, at)
                            ));
                        }
                    }
                }
            }
        }
    }
    missed
}

/// The engines a canvas reports for each of [`ENGINES`], so that a run
/// with no GPU says it tested the raster backend twice.
fn engines() -> Vec<EngineKind> {
    ENGINES
        .iter()
        .map(|&gpu| {
            let mut canvas =
                Canvas::with_options(1.0, 1.0, CanvasOptions::default())
                    .expect("a canvas");
            canvas.set_gpu(gpu);
            canvas.engine_kind()
        })
        .collect()
}

/// Every grey at every alpha, off the bytes, cast or filtered by every
/// route, stores the exact product, on both engines, drawn with either and
/// read with either -- but for the one route [`known_miss`] holds out.
#[test]
fn every_shadow_stores_the_exact_product() {
    println!("engines {:?}", engines());
    let missed = misses(&Route::ALL, OFF_BYTE, &ENGINES, &ENGINES, known_miss);
    assert!(
        missed.is_empty(),
        "{} missed, first {:#?}",
        missed.len(),
        &missed[..missed.len().min(4)]
    );
}

/// The control: every grey at every alpha on whole bytes, which the raster
/// backend's rule and the exact product store alike, stores the product by
/// every route on each engine, drawn and read by it -- a miss here is the
/// readback's or the parser's, not the blend's.
#[test]
fn a_shadow_of_whole_bytes_stores_their_product() {
    let missed: Vec<String> = ENGINES
        .iter()
        .flat_map(|&gpu| {
            misses(&Route::ALL, ON_BYTE, &[gpu], &[gpu], |_, _| false)
        })
        .collect();
    assert!(
        missed.is_empty(),
        "{} missed, first {:#?}",
        missed.len(),
        &missed[..missed.len().min(4)]
    );
}

/// The route [`known_miss`] holds out, held to the count it misses by now:
/// a change to it in either direction fails here, and #440 replaces this
/// with the route in the exact sweep.
#[test]
fn a_blend_filter_over_a_fill_on_the_cpu_misses_until_440() {
    let missed = misses(
        &[Route::CanvasKitBlend],
        OFF_BYTE,
        &[false],
        &[false],
        |_, _| false,
    );
    assert_eq!(
        missed.len(),
        8186,
        "the raster backend's blend over a fill, until #440; first {:#?}",
        &missed[..missed.len().min(4)]
    );
}

/// The `globalAlpha`s a shadow is cast under.
const GLOBAL_ALPHAS: [f64; 2] = [0.5, 0.3];

/// A grey shadow cast by an opaque shape under a `globalAlpha` stores the
/// exact product of the grey and its alpha times the global one, rounded
/// once, as a fill of the grey under it does: the global alpha is taken into
/// the shadow's colour before it is placed, not rounded again on its own.
/// Products within [`NEAR_HALF`] of a half are left out.
#[test]
fn a_shadow_under_a_global_alpha_stores_the_exact_product() {
    let mut missed = vec![];
    for global in GLOBAL_ALPHAS {
        for a in (0..=255).step_by(5) {
            for gpu in ENGINES {
                let mut canvas = Canvas::with_options(
                    2.0 * ROW,
                    1.0,
                    CanvasOptions::default(),
                )
                .expect("a canvas");
                canvas.set_gpu(gpu);
                let ctx = canvas.context();
                ctx.set_global_alpha(global as f32);
                for c in 0..=255 {
                    Route::ShadowColor.draw(
                        ctx,
                        c as f32,
                        &grey(c, a, OFF_BYTE),
                    );
                }
                let pixels = row(&mut canvas, gpu);
                let (pixels, _) = pixels.as_chunks::<4>();
                let alpha =
                    level(a, OFF_BYTE.1) / 255.0 * f64::from(global as f32);
                let stored_alpha = (alpha * 255.0 + 0.5).floor();
                for c in 0..=255 {
                    let exact = level(c, OFF_BYTE.0) * alpha;
                    if (exact.fract() - 0.5).abs() < NEAR_HALF {
                        continue;
                    }
                    let channel = (exact + 0.5).floor() as u8;
                    let want = [channel, channel, channel, stored_alpha as u8];
                    let stored = pixels[256 + c as usize];
                    if stored != want {
                        missed.push(format!(
                            "gpu {gpu}, {} under {global}: {stored:?} for {want:?}",
                            grey(c, a, OFF_BYTE)
                        ));
                    }
                }
            }
        }
    }
    assert!(
        missed.is_empty(),
        "{} missed, first {:#?}",
        missed.len(),
        &missed[..missed.len().min(4)]
    );
}
