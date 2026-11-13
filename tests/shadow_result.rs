//! A `shadowColor` shadow is the exact Gaussian blur of what casts it: the
//! draw's own coverage convolved with the Gaussian of a standard deviation
//! half the `shadowBlur`, sampled at whole-pixel offsets, in the shadow's
//! colour, rounded once -- as the HTML Standard defines it and as a
//! `drop-shadow()` is drawn here. The reference, `exact::sampled`, is
//! written apart from any code that draws a blur.
//!
//! Each case draws its shadow alone: the draw is moved off the page by
//! [`AWAY`] device pixels and the shadow's offset brings the shadow back,
//! so every pixel read is the shadow's. What it is held to is the same
//! engine's coverage of the same draw, unshadowed and in place, blurred.

// Each test file uses part of it.
#[allow(dead_code)]
mod exact;

use exact::{misses, profile, sampled};
use meo_skia_canvas::prelude::*;

const SIZE: (f32, f32) = (160.0, 100.0);

/// How far off the page a draw is moved, in device pixels, and how far its
/// shadow's offset moves the shadow back: past the reach of any blur here.
const AWAY: f32 = 1000.0;

/// The `shadowBlur`s swept: under Skia's raster engine's switch to three
/// box blurs at a standard deviation of 2, at it, and past it, to past the
/// crate's FFT crossover.
const BLURS: [f32; 6] = [2.0, 3.9, 4.0, 8.0, 12.0, 24.0];

/// A triangle across most of the page.
const WIDE: [(f32, f32); 3] = [(20.0, 80.0), (80.0, 15.25), (140.5, 85.0)];

/// A draw: what it does to the context once its state is set.
type Draw = fn(&mut Context2D);

fn triangle(ctx: &mut Context2D) {
    let [(x0, y0), (x1, y1), (x2, y2)] = WIDE;
    ctx.begin_path();
    ctx.move_to(x0, y0);
    ctx.line_to(x1, y1);
    ctx.line_to(x2, y2);
    ctx.close_path();
}

fn filled_triangle(ctx: &mut Context2D) {
    triangle(ctx);
    ctx.fill(FillRule::NonZero);
}

fn stroked_triangle(ctx: &mut Context2D) {
    triangle(ctx);
    ctx.set_line_width(3.0);
    ctx.stroke();
}

/// The rectangle [`rectangle`] fills: x, y, width and height.
const RECT: (f64, f64, f64, f64) = (60.3, 25.6, 60.0, 40.0);

fn rectangle(ctx: &mut Context2D) {
    let (x, y, w, h) = RECT;
    ctx.fill_rect(x as f32, y as f32, w as f32, h as f32);
}

/// How far an 8-bit alpha of a rectangle's shadow may lie from its closed
/// form, as `tests/blur_rect.rs` holds a blurred rectangle: half a level
/// for rounding and a hundredth for the arithmetic, on either engine.
const RECT_TOLERANCE: f64 = 0.51;

/// The shadow of [`RECT`] blurred by `sigma`: the interval across blurred
/// times the interval down, each averaged over a pixel -- the closed form a
/// rectangle's shadow is drawn in, from its geometry rather than from the
/// coverage Skia rasterises it to, which steps an edge's vertical coverage
/// in quarters: the top edge at 25.6 is covered 0.5 where it covers 0.4.
fn rect_shadow(sigma: f64) -> Vec<f64> {
    let (x, y, w, h) = RECT;
    let across = profile(SIZE.0 as usize, x, x + w, sigma);
    let down = profile(SIZE.1 as usize, y, y + h, sigma);
    down.iter()
        .flat_map(|d| across.iter().map(move |a| a * d))
        .collect()
}

fn text(ctx: &mut Context2D) {
    assert!(ctx.set_font_css("48px sans-serif"), "a font");
    ctx.fill_text("Wq", 40.0, 70.0, None);
}

fn image(ctx: &mut Context2D) {
    ctx.draw_image(&varied_image(), 60.25, 30.0);
}

fn scaled_triangle(ctx: &mut Context2D) {
    ctx.scale(0.75, 0.75);
    filled_triangle(ctx);
}

fn turned_rectangle(ctx: &mut Context2D) {
    ctx.translate(80.0, 50.0);
    ctx.rotate(0.3);
    ctx.fill_rect(-30.0, -20.0, 60.0, 40.0);
}

/// A 12px canvas of translucent stripes, its alpha varying across it.
fn striped() -> Canvas {
    let mut tile = Canvas::new(12.0, 12.0);
    let ctx = tile.context();
    for (x, alpha) in [(0.0, 1.0), (4.0, 0.6), (8.0, 0.25)] {
        ctx.set_fill_style_css(&format!("rgba(0, 0, 0, {alpha})"))
            .expect("a colour");
        ctx.fill_rect(x, 0.0, 4.0, 12.0);
    }
    tile
}

fn pattern_triangle(ctx: &mut Context2D) {
    let pattern =
        ctx.create_pattern_from_canvas(&mut striped(), PatternRepeat::Repeat);
    ctx.set_fill_pattern(&pattern);
    filled_triangle(ctx);
}

/// Text filled in a canvas's pattern: a draw the pattern's own route takes,
/// shadow and all, where a path files its shadow before it.
fn pattern_text(ctx: &mut Context2D) {
    let pattern =
        ctx.create_pattern_from_canvas(&mut striped(), PatternRepeat::Repeat);
    ctx.set_fill_pattern(&pattern);
    text(ctx);
}

fn drawn_canvas(ctx: &mut Context2D) {
    let mut source = Canvas::new(80.0, 60.0);
    let inner = source.context();
    inner
        .set_fill_style_css("rgba(0, 0, 0, 0.8)")
        .expect("a colour");
    inner.begin_path();
    inner
        .ellipse(
            40.0,
            30.0,
            35.0,
            22.5,
            0.4,
            0.0,
            std::f32::consts::TAU,
            false,
        )
        .expect("an ellipse");
    inner.fill(FillRule::NonZero);
    ctx.draw_canvas(&mut source, 40.25, 20.0);
}

/// Every source a shadow is cast from, by name.
const SOURCES: [(&str, Draw); 9] = [
    ("filled triangle", filled_triangle),
    ("stroked triangle", stroked_triangle),
    ("rectangle", rectangle),
    ("text", text),
    ("image", image),
    ("scaled triangle", scaled_triangle),
    ("turned rectangle", turned_rectangle),
    ("canvas pattern", pattern_triangle),
    ("drawn canvas", drawn_canvas),
];

/// Each draw under a filter, or a clip, of its own: what a draw that casts a
/// shadow takes a route of its own for -- a `BlurOp`, a cut, a chain --
/// beside the route its shadow takes.
const FILTERED: [(&str, Draw); 10] = [
    ("triangle under blur()", |ctx| {
        under("blur(3px)", ctx, filled_triangle)
    }),
    ("rectangle under blur()", |ctx| {
        under("blur(3px)", ctx, rectangle)
    }),
    ("triangle under drop-shadow()", |ctx| {
        under(DROP_SHADOW, ctx, filled_triangle)
    }),
    ("triangle a clip cuts", |ctx| {
        ctx.begin_path();
        ctx.rect(0.0, 0.0, 90.5, SIZE.1);
        ctx.clip(FillRule::NonZero);
        filled_triangle(ctx);
    }),
    ("canvas pattern under drop-shadow()", |ctx| {
        under(DROP_SHADOW, ctx, pattern_triangle)
    }),
    ("text in a canvas pattern under drop-shadow()", |ctx| {
        under(DROP_SHADOW, ctx, pattern_text)
    }),
    ("drawn canvas under blur()", |ctx| {
        under("blur(3px)", ctx, drawn_canvas)
    }),
    ("text under blur()", |ctx| under("blur(2px)", ctx, text)),
    ("turned rectangle under drop-shadow()", |ctx| {
        under(DROP_SHADOW, ctx, turned_rectangle)
    }),
    ("image under blur()", |ctx| under("blur(3px)", ctx, image)),
];

/// A `drop-shadow()` that moves what it casts by whole pixels, in a colour
/// no shadow here is cast in.
const DROP_SHADOW: &str = "drop-shadow(30px 0 0 blue)";

/// `draw` under the CSS filter `filter`.
fn under(filter: &str, ctx: &mut Context2D, draw: Draw) {
    ctx.set_filter_css(filter).expect("a filter");
    draw(ctx);
}

/// A 40px image whose alpha varies across it, so a shadow has structure to
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

/// A transparent page on `gpu`'s engine, or `None` where that engine is not
/// available.
fn page(gpu: bool) -> Option<Canvas> {
    let mut canvas = Canvas::new(SIZE.0, SIZE.1);
    canvas.set_gpu(gpu);
    (!gpu || canvas.engine_kind() == EngineKind::Gpu).then_some(canvas)
}

fn alpha(canvas: &mut Canvas) -> Vec<u8> {
    let data = canvas
        .context()
        .get_image_data(0.0, 0.0, SIZE.0, SIZE.1)
        .expect("pixels");
    data.pixels().chunks(4).map(|p| p[3]).collect()
}

/// `draw`'s coverage on `gpu`'s engine, unshadowed and in place, from zero
/// to one.
fn coverage(gpu: bool, draw: Draw) -> Option<Vec<f64>> {
    let mut canvas = page(gpu)?;
    let ctx = canvas.context();
    ctx.set_fill_style_css("black").expect("a colour");
    ctx.set_stroke_style_css("black").expect("a colour");
    draw(ctx);
    Some(
        alpha(&mut canvas)
            .into_iter()
            .map(|a| f64::from(a) / 255.0)
            .collect(),
    )
}

/// How a shadow is cast, beyond its blur.
#[derive(Clone, Copy)]
struct Cast {
    /// The shadow's colour, as CSS.
    colour: &'static str,
    /// Its alpha, which scales the shadow's coverage.
    colour_alpha: f64,
    global_alpha: f32,
    /// The left part of the page the shadow is composited within, in whole
    /// pixels, where there is a clip.
    clip: Option<f32>,
}

const PLAIN: Cast = Cast {
    colour: "black",
    colour_alpha: 1.0,
    global_alpha: 1.0,
    clip: None,
};

/// The alpha of `draw`'s shadow alone, cast through `cast` with a
/// `shadowBlur` of `blur` on `gpu`'s engine.
fn shadow_alone(
    gpu: bool,
    draw: Draw,
    blur: f32,
    cast: Cast,
) -> Option<Vec<u8>> {
    let mut canvas = page(gpu)?;
    let ctx = canvas.context();
    if let Some(width) = cast.clip {
        ctx.begin_path();
        ctx.rect(0.0, 0.0, width, SIZE.1);
        ctx.clip(FillRule::NonZero);
    }
    ctx.set_fill_style_css("black").expect("a colour");
    ctx.set_stroke_style_css("black").expect("a colour");
    ctx.set_shadow_color_css(cast.colour).expect("a colour");
    ctx.set_shadow_blur(blur);
    ctx.set_shadow_offset(AWAY, 0.0);
    ctx.set_global_alpha(cast.global_alpha);
    // Moved in device pixels, under whatever transform `draw` sets after it.
    ctx.translate(-AWAY, 0.0);
    draw(ctx);
    Some(alpha(&mut canvas))
}

/// Whether `name`'s shadow is a rectangle's closed form -- see
/// `Context2D::files_rect_shadow` -- rather than the exact Gaussian of the
/// coverage Skia rasterises it to: an untransformed rectangle's, blurred.
fn closed_form(name: &str, sigma: f64) -> bool {
    name == "rectangle" && sigma > 0.0
}

/// What `draw`'s shadow alone should be: its coverage `drawn`, blurred by
/// the exact Gaussian of half `blur`, scaled by the shadow's alpha and the
/// global alpha, inside the clip; a rectangle's, its closed form -- see
/// [`closed_form`].
fn reference(name: &str, drawn: &[f64], blur: f32, cast: Cast) -> Vec<f64> {
    let sigma = f64::from(blur) / 2.0;
    let closed = closed_form(name, sigma);
    let blurred = match closed {
        true => rect_shadow(sigma),
        false => sampled(drawn, SIZE.0 as usize, sigma),
    };
    let gain = cast.colour_alpha * f64::from(cast.global_alpha);
    blurred
        .iter()
        .enumerate()
        .map(|(i, b)| match cast.clip {
            Some(width) if (i % SIZE.0 as usize) as f32 >= width => 0.0,
            _ => b * gain,
        })
        .collect()
}

/// Each source, blur and engine whose shadow cast through `cast` is off
/// the exact one, with how many pixels and the worst of them.
fn off_the_gaussian(cast: Cast, blurs: &[f32]) -> Vec<String> {
    let mut wrong = vec![];
    for gpu in [false, true] {
        for (name, draw) in SOURCES {
            let Some(drawn) = coverage(gpu, draw) else {
                continue;
            };
            for &blur in blurs {
                let Some(shadow) = shadow_alone(gpu, draw, blur, cast) else {
                    continue;
                };
                let reference = reference(name, &drawn, blur, cast);
                let closed = closed_form(name, f64::from(blur) / 2.0);
                let misses = match closed {
                    true => shadow
                        .iter()
                        .zip(&reference)
                        .enumerate()
                        .filter(|(_, (a, r))| {
                            (f64::from(**a) - **r * 255.0).abs()
                                > RECT_TOLERANCE
                        })
                        .map(|(i, (a, r))| (i, *a, r * 255.0))
                        .collect(),
                    false => misses(&shadow, &reference),
                };
                if let Some(worst) = misses.iter().max_by(|a, b| {
                    let off =
                        |m: &(usize, u8, f64)| (f64::from(m.1) - m.2).abs();
                    off(a).total_cmp(&off(b))
                }) {
                    wrong.push(format!(
                        "gpu {gpu}, {name}, shadowBlur {blur}: {} pixels off, \
                         worst {} where the Gaussian is {:.2}",
                        misses.len(),
                        worst.1,
                        worst.2,
                    ));
                }
            }
        }
    }
    wrong
}

#[test]
fn a_shadow_is_the_exact_gaussian_of_what_casts_it() {
    let wrong = off_the_gaussian(PLAIN, &BLURS);
    assert!(wrong.is_empty(), "off the exact shadow: {wrong:#?}");
}

#[test]
fn a_translucent_shadow_is_the_exact_gaussian_rounded_once() {
    // The shadow's alpha and the global alpha scale the blurred coverage,
    // and the product is rounded once.
    let cast = Cast {
        colour: "rgba(200, 0, 0, 0.6)",
        colour_alpha: 0.6,
        global_alpha: 0.5,
        clip: None,
    };
    let wrong = off_the_gaussian(cast, &[4.0, 12.0]);
    assert!(wrong.is_empty(), "off the exact shadow: {wrong:#?}");
}

/// The clip [`a_clip_confines_the_shadow_and_not_what_casts_it`] casts
/// within: every draw reaches past it, by less than the smallest blur there
/// reaches, 36 pixels at a `shadowBlur` of 12.
///
/// A shadow's layer, as a filter's, is cut at the clip widened by the
/// blur's reach, and Skia rasterises a path its clip cuts by another route
/// than one it contains -- see `chain::region`. A draw the layer cut would
/// test that instead of the shadow, so each stays within it whole.
const CLIP: f32 = 110.0;

#[test]
fn a_clip_confines_the_shadow_and_not_what_casts_it() {
    // The shadow is cast from the whole draw and composited inside the
    // clip, so its blur reaches into the clip from what lies outside it.
    let cast = Cast {
        clip: Some(CLIP),
        ..PLAIN
    };
    let wrong = off_the_gaussian(cast, &[12.0, 24.0]);
    assert!(wrong.is_empty(), "off the exact shadow: {wrong:#?}");
}

/// The bytes of `draw` on a page filled in a colour first, through `paint`
/// at `global_alpha`, casting a shadow where `shadow` is set: one cast off
/// the page by [`AWAY`] pixels, so that no pixel read holds any of it.
fn drawn_over(
    gpu: bool,
    draw: Draw,
    paint: &str,
    global_alpha: f32,
    shadow: Option<f32>,
) -> Option<Vec<u8>> {
    let mut canvas = page(gpu)?;
    let ctx = canvas.context();
    ctx.set_fill_style_css("rgb(40 120 200)").expect("a colour");
    ctx.fill_rect(0.0, 0.0, SIZE.0, SIZE.1);
    ctx.set_fill_style_css(paint).expect("a colour");
    ctx.set_stroke_style_css(paint).expect("a colour");
    ctx.set_global_alpha(global_alpha);
    if let Some(blur) = shadow {
        ctx.set_shadow_color_css("rgba(200, 0, 0, 0.6)")
            .expect("a colour");
        ctx.set_shadow_blur(blur);
        ctx.set_shadow_offset(AWAY, 0.0);
    }
    draw(ctx);
    let data = ctx
        .get_image_data(0.0, 0.0, SIZE.0, SIZE.1)
        .expect("pixels");
    Some(data.pixels().to_vec())
}

#[test]
fn a_draw_casting_a_shadow_lands_the_bytes_it_does_casting_none() {
    // The shadow is a group of its own, cast before the draw, and the draw
    // is drawn as it is: through the shadow's layer it would be rounded
    // twice where it is translucent, and an edge's coverage with it. Under a
    // filter or a clip of its own the draw takes the route it takes casting
    // none: a `BlurOp`, a cut, a chain, each of which refuses a shadow.
    let mut wrong = vec![];
    for gpu in [false, true] {
        for (name, draw) in SOURCES.iter().chain(&FILTERED) {
            for (paint, global_alpha) in [
                ("rgb(250 200 20)", 1.0),
                ("rgba(250 200 20 / 0.55)", 1.0),
                ("rgba(250 200 20 / 0.55)", 0.45),
            ] {
                let Some(plain) =
                    drawn_over(gpu, *draw, paint, global_alpha, None)
                else {
                    continue;
                };
                for blur in [0.0, 4.0, 12.0] {
                    let shadowed =
                        drawn_over(gpu, *draw, paint, global_alpha, Some(blur))
                            .expect("the same engine");
                    let apart = plain
                        .iter()
                        .zip(&shadowed)
                        .filter(|(a, b)| a != b)
                        .count();
                    if apart > 0 {
                        wrong.push(format!(
                            "gpu {gpu}, {name}, {paint} at {global_alpha}, \
                             shadowBlur {blur}: {apart} bytes apart"
                        ));
                    }
                }
            }
        }
    }
    assert!(wrong.is_empty(), "the draw moved: {wrong:#?}");
}

#[test]
fn a_filtered_draw_casts_the_shadow_of_what_its_filter_makes() {
    // The standard filters the draw and casts the shadow from what the
    // filter makes: the draw's coverage blurred by the filter, held as
    // pixels -- rounded to a level -- and blurred again by the shadow, or
    // not at all where the shadow is not blurred. Each source takes a route
    // of its own: a path, a canvas's pattern under a path and under text, a
    // drawn canvas.
    let filter_sigma = 2.0f64;
    let sources: [(&str, Draw); 4] = [
        ("filled triangle", filled_triangle),
        ("canvas pattern", pattern_triangle),
        ("text in a canvas pattern", pattern_text),
        ("drawn canvas", drawn_canvas),
    ];
    let mut wrong = vec![];
    for gpu in [false, true] {
        for (name, draw) in sources {
            let Some(drawn) = coverage(gpu, draw) else {
                continue;
            };
            let filtered: Vec<f64> =
                sampled(&drawn, SIZE.0 as usize, filter_sigma)
                    .into_iter()
                    .map(|v| (v * 255.0).round() / 255.0)
                    .collect();
            for blur in [0.0f32, 4.0, 12.0] {
                let mut canvas = page(gpu).expect("the same engine");
                let ctx = canvas.context();
                ctx.set_fill_style_css("black").expect("a colour");
                ctx.set_filter_css(&format!("blur({filter_sigma}px)"))
                    .expect("a filter");
                ctx.set_shadow_color_css("black").expect("a colour");
                ctx.set_shadow_blur(blur);
                ctx.set_shadow_offset(AWAY, 0.0);
                ctx.translate(-AWAY, 0.0);
                draw(ctx);
                let shadow = alpha(&mut canvas);
                let reference = match blur {
                    0.0 => filtered.clone(),
                    _ => sampled(
                        &filtered,
                        SIZE.0 as usize,
                        f64::from(blur) / 2.0,
                    ),
                };
                let misses = misses(&shadow, &reference);
                if !misses.is_empty() {
                    wrong.push(format!(
                        "gpu {gpu}, {name}, shadowBlur {blur}: {} pixels off, \
                         first {:?}",
                        misses.len(),
                        &misses[..misses.len().min(3)],
                    ));
                }
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "off the shadow of the filtered draw: {wrong:#?}"
    );
}

/// The pixels of a black triangle's shadow alone, in red, under the CSS
/// filter `filter` and the blend `blend`, on `gpu`'s engine.
fn filtered_shadow(
    gpu: bool,
    filter: &str,
    blend: BlendMode,
) -> Option<Vec<u8>> {
    let mut canvas = page(gpu)?;
    let ctx = canvas.context();
    ctx.set_global_composite_operation(blend);
    ctx.set_fill_style_css("black").expect("a colour");
    ctx.set_filter_css(filter).expect("a filter");
    ctx.set_shadow_color_css("red").expect("a colour");
    ctx.set_shadow_offset(AWAY, 0.0);
    ctx.translate(-AWAY, 0.0);
    filled_triangle(ctx);
    let data = ctx
        .get_image_data(0.0, 0.0, SIZE.0, SIZE.1)
        .expect("pixels");
    Some(data.pixels().to_vec())
}

#[test]
fn a_shadow_is_cast_after_the_filter_and_is_not_filtered() {
    // The HTML Standard filters the draw and casts the shadow from the
    // result: a filter that does not commute with a shadow shows the order.
    // The shadow of a draw through `invert(1)` is in the shadow's colour,
    // where the filter run after it would turn red to cyan; the shadow of
    // one through a `drop-shadow()` holds both the draw and its copy, all
    // in the shadow's colour, where the filter run after it would add a
    // blue copy of the shadow. Under a blend that composites the whole
    // canvas the shadow is Skia's, and the order is the same.
    let mut wrong = vec![];
    for gpu in [false, true] {
        let Some(copy) =
            coverage(gpu, |ctx| under(DROP_SHADOW, ctx, filled_triangle))
        else {
            continue;
        };
        for blend in [BlendMode::SourceOver, BlendMode::Copy] {
            let shadow = filtered_shadow(gpu, "invert(1)", blend)
                .expect("the same engine");
            let opaque = shadow.chunks(4).filter(|p| p[3] == 255);
            let not_red = opaque.clone().filter(|p| p[..3] != [255, 0, 0]);
            if opaque.count() == 0 || not_red.count() > 0 {
                wrong.push(format!(
                    "gpu {gpu}, {blend:?}: the shadow of a draw through \
                     invert(1) is not red"
                ));
            }
            let shadow = filtered_shadow(gpu, DROP_SHADOW, blend)
                .expect("the same engine");
            let tinted = shadow
                .chunks(4)
                .filter(|p| p[3] > 0 && p[..3] != [255, 0, 0])
                .count();
            let apart = shadow
                .chunks(4)
                .zip(&copy)
                .filter(|(p, c)| f64::from(p[3]) != (**c * 255.0).round())
                .count();
            if tinted > 0 || apart > 0 {
                wrong.push(format!(
                    "gpu {gpu}, {blend:?}: the shadow of a draw through \
                     {DROP_SHADOW} has {tinted} pixels not red and {apart} \
                     whose alpha is not the filtered draw's"
                ));
            }
        }
    }
    assert!(wrong.is_empty(), "the filter and the shadow: {wrong:#?}");
}
