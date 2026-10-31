//! A pattern made from a canvas draws what that canvas draws, at whatever
//! density the page it fills is read at: the pattern of a canvas holding a
//! blur shows the blur as the canvas shows it there, not a blur made once at
//! one density and resampled to the others.
//!
//! A draw such a pattern dyes paints the canvas where it covers, through
//! its own alpha, blend mode and colour filter, as a draw dyed by a pattern
//! of the canvas's own pixels at that density does: to the byte, the
//! density the pattern is made at and the colour type it is made in being
//! the page's own.

use meo_skia_canvas::prelude::*;

/// The size of every canvas here.
const WIDTH: f32 = 200.0;
const HEIGHT: f32 = 120.0;

/// The densities each case is read at.
const DENSITIES: [u32; 3] = [1, 2, 3];

/// The pixels of `canvas` read at `density`.
fn pixels(canvas: &mut Canvas, density: u32) -> Vec<u8> {
    canvas
        .to_image_data(
            None,
            &ImageDataExport {
                density,
                ..Default::default()
            },
        )
        .expect("pixels")
        .pixels()
        .to_vec()
}

/// A triangle with its corner at (`x`, `y`), filled in the current style.
fn triangle(ctx: &mut Context2D, x: f32, y: f32) {
    ctx.begin_path();
    ctx.move_to(x, y);
    ctx.line_to(x + 25.5, y + 9.25);
    ctx.line_to(x + 7.0, y + 17.5);
    ctx.close_path();
    ctx.fill(FillRule::NonZero);
}

/// What each source draws: the blur in each of the forms a page keeps it.
#[derive(Clone, Copy, Debug)]
enum Source {
    /// A rectangle under `blur(2px)` inside a circle's clip, with a triangle
    /// drawn before the clip or not, and the clip inside `save` and
    /// `restore` or not.
    ClippedRect { before: bool, saved: bool },
    /// A triangle under `blur(3px)`, which the page blurs by its coverage.
    Path3,
    /// A triangle under `blur(70px)`, which the page blurs as a result.
    Path70,
    /// A triangle casting a `drop-shadow()`.
    Shadow,
    /// A triangle drawn in a `saveLayer` under `blur(4px)`.
    Layer,
}

const SOURCES: [Source; 8] = [
    Source::ClippedRect {
        before: false,
        saved: false,
    },
    Source::ClippedRect {
        before: true,
        saved: false,
    },
    Source::ClippedRect {
        before: false,
        saved: true,
    },
    Source::ClippedRect {
        before: true,
        saved: true,
    },
    Source::Path3,
    Source::Path70,
    Source::Shadow,
    Source::Layer,
];

fn source(kind: Source) -> Canvas {
    source_on(kind, false)
}

/// `kind` drawn on the GPU engine where `gpu`, which a build or a machine
/// without one draws on the CPU.
fn source_on(kind: Source, gpu: bool) -> Canvas {
    let mut canvas = Canvas::new(WIDTH, HEIGHT);
    canvas.set_gpu(gpu);
    let ctx = canvas.context();
    ctx.set_fill_style_css("rgb(30 60 200 / 0.7)")
        .expect("a colour");
    match kind {
        Source::ClippedRect { before, saved } => {
            if before {
                triangle(ctx, 70.0, 5.0);
            }
            if saved {
                ctx.save();
            }
            ctx.begin_path();
            ctx.arc(90.3, 60.2, 45.3, 0.0, std::f32::consts::TAU, false)
                .expect("an arc");
            ctx.clip(FillRule::NonZero);
            ctx.set_filter_css("blur(2px)").expect("a filter");
            ctx.fill_rect(80.0, 50.0, 20.0, 12.0);
            ctx.set_filter_css("none").expect("no filter");
            if saved {
                ctx.restore();
            }
        }
        Source::Path3 | Source::Path70 => {
            let radius = match kind {
                Source::Path3 => 3,
                _ => 70,
            };
            ctx.set_filter_css(&format!("blur({radius}px)"))
                .expect("a filter");
            triangle(ctx, 75.0, 40.0);
        }
        Source::Shadow => {
            ctx.set_filter_css("drop-shadow(4px 3px 2.5px rgb(200 40 90))")
                .expect("a filter");
            triangle(ctx, 75.0, 40.0);
        }
        Source::Layer => {
            ctx.set_filter_css("blur(4px)").expect("a filter");
            ctx.save_layer();
            ctx.set_filter_css("none").expect("no filter");
            triangle(ctx, 75.0, 40.0);
            ctx.restore();
        }
    }
    canvas
}

/// `source` as a pattern filling a canvas of its size, on the engine it
/// draws on.
fn as_pattern(source: &mut Canvas, repeat: PatternRepeat) -> Canvas {
    let mut filled = Canvas::new(WIDTH, HEIGHT);
    filled.set_gpu(source.gpu());
    let pattern = filled.context().create_pattern_from_canvas(source, repeat);
    filled.context().set_fill_pattern(&pattern);
    filled.context().fill_rect(0.0, 0.0, WIDTH, HEIGHT);
    filled
}

/// The pixels of `canvas` read at `density`, premultiplied, as a pattern of
/// them takes them back without rounding.
fn premultiplied(canvas: &mut Canvas, density: u32) -> Vec<u8> {
    canvas
        .to_image_data(
            None,
            &ImageDataExport {
                density,
                pixels: PixelExportOptions {
                    premultiplied: true,
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .expect("pixels")
        .pixels()
        .to_vec()
}

/// A pattern of `source`'s own pixels at `density`, scaled back to its
/// size: the pattern a draw at that density should paint.
fn pixels_pattern(source: &mut Canvas, density: u32) -> Pattern {
    let image = Image::from_pixels(
        &premultiplied(source, density),
        WIDTH as u32 * density,
        HEIGHT as u32 * density,
        WIDTH as usize * density as usize * 4,
        PixelExportOptions {
            premultiplied: true,
            ..Default::default()
        },
    )
    .expect("an image of the pixels");
    let mut holder = Canvas::new(WIDTH, HEIGHT);
    let mut pattern = holder
        .context()
        .create_pattern(&image, PatternRepeat::Repeat);
    let scale = 1.0 / density as f32;
    pattern.set_transform(Affine {
        a: scale,
        d: scale,
        ..Affine::IDENTITY
    });
    pattern
}

/// What each draw dyed by a pattern does, and the most its pixels may part
/// from the same draw dyed by the pixels' pattern: none where the draw
/// covers every pixel it touches whole at full alpha, or rasterizes the
/// same coverage both ways -- text, a draw through a filter, `copy` -- one
/// level where it covers part of a pixel or composites at an alpha, and
/// two where it lies over its own shadow at an alpha, the shadow and the
/// draw each composited through a layer: measured, every pixel two levels
/// off lay inside the draw.
#[derive(Clone, Copy, Debug)]
enum Draw {
    WholeRect,
    PartRect,
    Path,
    Stroke,
    Text,
    Alpha,
    Shadow,
    ShadowAlpha,
    Tinted,
    Filter,
    Copy,
    Multiply,
}

impl Draw {
    const ALL: [Draw; 12] = [
        Draw::WholeRect,
        Draw::PartRect,
        Draw::Path,
        Draw::Stroke,
        Draw::Text,
        Draw::Alpha,
        Draw::Shadow,
        Draw::ShadowAlpha,
        Draw::Tinted,
        Draw::Filter,
        Draw::Copy,
        Draw::Multiply,
    ];

    fn on(self, ctx: &mut Context2D) {
        let path = |ctx: &mut Context2D| {
            ctx.begin_path();
            ctx.move_to(40.3, 20.7);
            ctx.line_to(95.8, 39.95);
            ctx.line_to(57.3, 68.2);
            ctx.close_path();
        };
        match self {
            Draw::WholeRect => ctx.fill_rect(10.0, 10.0, 170.0, 90.0),
            Draw::PartRect => ctx.fill_rect(10.5, 10.25, 170.0, 90.0),
            Draw::Path => {
                path(ctx);
                ctx.fill(FillRule::NonZero);
            }
            Draw::Stroke => {
                ctx.set_line_width(3.0);
                path(ctx);
                ctx.stroke();
            }
            Draw::Text => {
                ctx.set_font_css("bold 60px sans-serif");
                ctx.fill_text("Wg", 50.0, 90.0, None);
            }
            Draw::Alpha => {
                ctx.set_global_alpha(0.5);
                ctx.fill_rect(10.0, 10.0, 170.0, 90.0);
            }
            Draw::Shadow => {
                ctx.set_shadow_blur(4.0);
                ctx.set_shadow_offset(5.0, 3.0);
                ctx.set_shadow_color_css("rgb(200 0 0 / 0.8)")
                    .expect("a colour");
                path(ctx);
                ctx.fill(FillRule::NonZero);
            }
            Draw::ShadowAlpha => {
                ctx.set_global_alpha(0.5);
                Draw::Shadow.on(ctx);
            }
            // A colour matrix lifting red by an offset, which is not the
            // same applied before an alpha as after it.
            Draw::Tinted => {
                let lift = ColorFilter::matrix([
                    1.0, 0.0, 0.0, 0.0, 0.3, //
                    0.0, 1.0, 0.0, 0.0, 0.0, //
                    0.0, 0.0, 1.0, 0.0, 0.0, //
                    0.0, 0.0, 0.0, 1.0, 0.0,
                ])
                .expect("a colour matrix");
                ctx.set_color_filter(Some(&lift));
                Draw::Alpha.on(ctx);
            }
            Draw::Filter => {
                ctx.set_filter_css("blur(1px)").expect("a filter");
                path(ctx);
                ctx.fill(FillRule::NonZero);
            }
            Draw::Copy => {
                ctx.set_global_composite_operation(BlendMode::Copy);
                path(ctx);
                ctx.fill(FillRule::NonZero);
            }
            Draw::Multiply => {
                ctx.set_global_composite_operation(BlendMode::Multiply);
                path(ctx);
                ctx.fill(FillRule::NonZero);
            }
        }
    }
}

/// `draw` dyed by `pattern`, over a band of colour, read at `density`.
fn dyed(pattern: &Pattern, draw: Draw, density: u32) -> Vec<u8> {
    let mut canvas = Canvas::new(WIDTH, HEIGHT);
    canvas.set_gpu(false);
    let ctx = canvas.context();
    ctx.set_fill_style_css("rgb(250 200 0)").expect("a colour");
    ctx.fill_rect(0.0, 0.0, WIDTH, 40.0);
    ctx.set_fill_pattern(pattern);
    ctx.set_stroke_pattern(pattern);
    draw.on(ctx);
    premultiplied(&mut canvas, density)
}

/// Where two reads part: how many pixels, and by how much at most.
fn parting(got: &[u8], want: &[u8]) -> (usize, u8) {
    got.chunks(4)
        .zip(want.chunks(4))
        .filter(|(a, b)| a != b)
        .fold((0, 0), |(n, most), (a, b)| {
            let by = a.iter().zip(b).map(|(x, y)| x.abs_diff(*y)).max();
            (n + 1, most.max(by.unwrap_or(0)))
        })
}

/// The pattern lies over the page exactly, so each density reads the
/// canvas's own pixels at that density, to the byte -- one page read at
/// each in turn, as an export at one density and a read at another would.
/// On either engine: a GPU page draws its blurs other than the CPU does,
/// and a pattern of it draws them as it does.
#[test]
fn a_pattern_of_a_canvas_holding_a_blur_draws_it_at_every_density() {
    let mut failures = vec![];
    for (kind, gpu) in SOURCES
        .into_iter()
        .flat_map(|kind| [(kind, false), (kind, true)])
    {
        for repeat in [PatternRepeat::Repeat, PatternRepeat::NoRepeat] {
            let mut source = source_on(kind, gpu);
            let mut filled = as_pattern(&mut source, repeat);
            for density in DENSITIES {
                let own = pixels(&mut source, density);
                let drawn = pixels(&mut filled, density);
                if drawn != own {
                    failures.push(format!(
                        "{kind:?} gpu {gpu} {repeat:?} {density}x: {:?}",
                        parting(&drawn, &own)
                    ));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// Every kind of draw a pattern of a blurred canvas dyes paints the
/// canvas's pixels at the density it is read at, as the same draw dyed by a
/// pattern of those pixels does: to the byte, or within the one level a
/// second rounding costs -- see the module's note.
#[test]
fn a_draw_dyed_by_it_paints_the_canvas_at_every_density() {
    let kinds = [
        Source::ClippedRect {
            before: true,
            saved: true,
        },
        Source::Path70,
        Source::Shadow,
    ];
    let mut failures = vec![];
    for kind in kinds {
        for density in DENSITIES {
            let mut source = source(kind);
            let pixels = pixels_pattern(&mut source, density);
            let mut holder = Canvas::new(WIDTH, HEIGHT);
            let made = holder
                .context()
                .create_pattern_from_canvas(&mut source, PatternRepeat::Repeat);
            for draw in Draw::ALL {
                let (n, most) = parting(
                    &dyed(&made, draw, density),
                    &dyed(&pixels, draw, density),
                );
                if most > 0 {
                    failures.push(format!(
                        "{kind:?} {draw:?} {density}x: {n} px, {most}"
                    ));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// An SVG export embeds a draw dyed by a canvas's pattern as pixels: SVG
/// names no paint server for the picture the pattern is to a document, and
/// Skia's SVG backend wrote the draw as a bare path, black.
#[test]
fn an_svg_rasterizes_a_draw_dyed_by_it() {
    let mut source = source(Source::ClippedRect {
        before: false,
        saved: false,
    });
    let mut canvas = Canvas::new(WIDTH, HEIGHT);
    let pattern = canvas
        .context()
        .create_pattern_from_canvas(&mut source, PatternRepeat::Repeat);
    let ctx = canvas.context();
    ctx.set_fill_pattern(&pattern);
    ctx.begin_path();
    ctx.move_to(40.3, 20.7);
    ctx.line_to(95.8, 39.95);
    ctx.line_to(57.3, 68.2);
    ctx.close_path();
    ctx.fill(FillRule::NonZero);
    let svg = canvas
        .to_buffer(ImageFormat::Svg, &EncodeOptions::default())
        .expect("an svg");
    let svg = String::from_utf8_lossy(&svg);
    assert!(svg.contains("<image"), "the draw is in the SVG: {svg}");
    assert!(!svg.contains("<path"), "the draw is a bare path: {svg}");
}

/// `text` in `font` at (`x`, `y`), dyed by `pattern` where `pattern` is
/// `Some`, or not drawn at all, over the band [`dyed`] draws, read at
/// `density`.
fn dyed_text(
    pattern: Option<&Pattern>,
    font: &str,
    text: &str,
    density: u32,
) -> Vec<u8> {
    let mut canvas = Canvas::new(WIDTH, HEIGHT);
    canvas.set_gpu(false);
    let ctx = canvas.context();
    ctx.set_fill_style_css("rgb(250 200 0)").expect("a colour");
    ctx.fill_rect(0.0, 0.0, WIDTH, 40.0);
    if let Some(pattern) = pattern {
        ctx.set_font_css(font);
        ctx.set_fill_pattern(pattern);
        ctx.fill_text(text, TEXT_AT.0, TEXT_AT.1, None);
    }
    premultiplied(&mut canvas, density)
}

/// Where [`dyed_text`] draws: off the pixel grid, so that the edges of the
/// ink fall part way across pixels.
const TEXT_AT: (f32, f32) = (20.3, 70.6);

/// Text whose ink at density 2 lies past the box it measures -- the rows a
/// text's reach is kept to the clip for, since hinting at the density a page
/// is read at puts the ink where the bounds recorded at 1x do not -- is
/// dyed out to those rows. A pattern clipped to less than the text covers
/// at that density leaves them as they were, and parts from the draw dyed by
/// the pixels' pattern there.
#[test]
fn text_inked_past_its_measured_box_is_dyed_to_its_edge() {
    let density = 2;
    let scale = density as f32;
    // A blur wide enough to leave no pixel of the tile transparent, so that
    // every pixel the text covers is one the pattern changes.
    let mut source = source(Source::Path70);
    let pixels = pixels_pattern(&mut source, density);
    let mut holder = Canvas::new(WIDTH, HEIGHT);
    let made = holder
        .context()
        .create_pattern_from_canvas(&mut source, PatternRepeat::Repeat);
    let bare = dyed_text(None, "", "", density);
    let width = (WIDTH * scale) as usize;
    let (mut past, mut failures) = (0, vec![]);
    for size in (12..=80).map(|half| half as f32 / 2.0) {
        for text in ["Q|", "Éjy"] {
            let font = format!("{size}px serif");
            let mut probe = Canvas::new(WIDTH, HEIGHT);
            probe.context().set_font_css(&font);
            let m = probe.context().measure_text(text, None);
            let (x, y) = TEXT_AT;
            let left = ((x - m.actual_bounding_box_left) * scale).floor();
            let right = ((x + m.actual_bounding_box_right) * scale).ceil();
            let top = ((y - m.actual_bounding_box_ascent) * scale).floor();
            let bottom = ((y + m.actual_bounding_box_descent) * scale).ceil();
            let want = dyed_text(Some(&pixels), &font, text, density);
            // The pixels outside the measured box the text paints.
            let outside = want
                .chunks(4)
                .zip(bare.chunks(4))
                .enumerate()
                .filter(|(at, (dyed, bare))| {
                    let (col, row) = ((at % width) as f32, (at / width) as f32);
                    dyed != bare
                        && (!(left..right).contains(&col)
                            || !(top..bottom).contains(&row))
                })
                .count();
            if outside == 0 {
                continue;
            }
            past += 1;
            let got = dyed_text(Some(&made), &font, text, density);
            if got != want {
                failures.push(format!(
                    "{font} {text:?}: {outside} px past the box, {:?}",
                    parting(&got, &want)
                ));
            }
        }
    }
    assert!(past > 0, "no text inked past its measured box");
    assert!(failures.is_empty(), "{failures:#?}");
}

/// One GPU page drawing two patterns, and one of them at two densities,
/// paints each draw as a page drawing it alone does: each pattern and each
/// density rasterizes a tile of its own, however many draws on the page
/// share one.
#[test]
fn a_gpu_page_draws_each_pattern_at_each_density_from_its_own_tile() {
    let mut holder = Canvas::new(WIDTH, HEIGHT);
    // The first with sharp edges, which a tile at the wrong density shows.
    let mut first = source_on(Source::Shadow, true);
    let mut second = source_on(Source::Path70, true);
    let first = holder
        .context()
        .create_pattern_from_canvas(&mut first, PatternRepeat::Repeat);
    let second = holder
        .context()
        .create_pattern_from_canvas(&mut second, PatternRepeat::Repeat);
    // Each a pattern, the scale it is drawn under, and the left edge and
    // width of the band it fills. None overlaps another, and each lies on
    // whole device pixels; under a scale of 2 the first pattern is
    // rasterized at twice the density.
    let draws = [
        (&first, 1.0, 0.0, 60.0),
        (&second, 1.0, 60.0, 60.0),
        (&first, 2.0, 60.0, 20.0),
    ];
    let page = |draws: &[(&Pattern, f32, f32, f32)]| {
        let mut canvas = Canvas::new(WIDTH, HEIGHT);
        canvas.set_gpu(true);
        let ctx = canvas.context();
        for &(pattern, scale, x, width) in draws {
            ctx.save();
            ctx.scale(scale, scale);
            ctx.set_fill_pattern(pattern);
            ctx.fill_rect(x, 0.0, width, HEIGHT / scale);
            ctx.restore();
        }
        pixels(&mut canvas, 1)
    };
    let together = page(&draws);
    let alone: Vec<Vec<u8>> = draws
        .iter()
        .map(|draw| page(std::slice::from_ref(draw)))
        .collect();
    // Each pixel as the one page that drew there has it, and transparent
    // where none did.
    let merged: Vec<u8> = (0..together.len())
        .step_by(4)
        .flat_map(|at| {
            alone
                .iter()
                .map(|pixels| &pixels[at..at + 4])
                .find(|pixel| pixel[3] != 0)
                .unwrap_or(&[0; 4])
                .to_vec()
        })
        .collect();
    assert_eq!(parting(&together, &merged), (0, 0));
}

/// The face the text cases below are drawn in, registered once.
const OSWALD: &str = "tests/assets/fonts/Oswald/Oswald-VariableFont_wght.ttf";
const OSWALD_FAMILY: &str = "PatternDensityOswald";

/// What a run of text is drawn with, beyond its font: every way the run
/// painter draws a run other than as plain glyphs.
#[derive(Clone, Copy, Debug)]
enum Run {
    /// Leaning past the shear FreeType draws -- about 79.65 degrees -- so
    /// that on Linux its glyphs are drawn from their outlines.
    PastFreeType,
    /// Underlined and struck through, the lines drawn beneath and over the
    /// glyphs.
    Decorated,
    /// Given a maximum width under its natural one, which squeezes the run
    /// by a scale about its anchor.
    Squeezed,
    /// Stroked rather than filled.
    Stroked,
}

impl Run {
    const ALL: [Run; 4] = [
        Run::PastFreeType,
        Run::Decorated,
        Run::Squeezed,
        Run::Stroked,
    ];

    fn on(self, ctx: &mut Context2D) {
        let font = match self {
            Run::PastFreeType => format!("oblique 85deg 40px {OSWALD_FAMILY}"),
            _ => format!("40px {OSWALD_FAMILY}"),
        };
        assert!(ctx.set_font_css(&font));
        if let Run::Decorated = self {
            ctx.set_text_decoration_css("underline line-through")
                .expect("a decoration");
        }
        match self {
            Run::Stroked => {
                ctx.set_line_width(2.0);
                ctx.stroke_text("IHg", 30.3, 70.6, None);
            }
            Run::Squeezed => ctx.fill_text("IHg", 30.3, 70.6, Some(30.0)),
            _ => ctx.fill_text("IHg", 30.3, 70.6, None),
        }
    }
}

/// `run` dyed by `pattern` on the engine `gpu` asks for, over the band
/// [`dyed`] draws, read at `density`.
fn dyed_run(pattern: &Pattern, run: Run, gpu: bool, density: u32) -> Vec<u8> {
    let mut canvas = Canvas::new(WIDTH, HEIGHT);
    canvas.set_gpu(gpu);
    let ctx = canvas.context();
    ctx.set_fill_style_css("rgb(250 200 0)").expect("a colour");
    ctx.fill_rect(0.0, 0.0, WIDTH, 40.0);
    ctx.set_fill_pattern(pattern);
    ctx.set_stroke_pattern(pattern);
    run.on(ctx);
    premultiplied(&mut canvas, density)
}

/// Text dyed by the pattern is drawn as the run painter draws it -- leaning
/// past what FreeType draws, decorated, condensed, stroked -- and paints the
/// canvas's pixels there as text dyed by a pattern of those pixels does, to
/// the byte, on either engine and at every density.
#[test]
fn every_way_a_run_is_drawn_is_dyed_as_the_pixels_pattern_dyes_it() {
    FontLibrary::new()
        .use_paths(Some(OSWALD_FAMILY), [OSWALD])
        .expect("bundled Oswald registers");
    let mut failures = vec![];
    for gpu in [false, true] {
        for density in DENSITIES {
            let mut source = source_on(Source::Path70, gpu);
            let pixels = pixels_pattern(&mut source, density);
            let mut holder = Canvas::new(WIDTH, HEIGHT);
            let made = holder
                .context()
                .create_pattern_from_canvas(&mut source, PatternRepeat::Repeat);
            for run in Run::ALL {
                let want = dyed_run(&pixels, run, gpu, density);
                let got = dyed_run(&made, run, gpu, density);
                if got != want {
                    failures.push(format!(
                        "{run:?} gpu {gpu} {density}x: {:?}",
                        parting(&got, &want)
                    ));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// A run dyed by a canvas's pattern, drawn again on the GPU in another
/// pattern and alpha after any other and with the canvas read back in
/// between, draws what a fresh canvas draws: a replayed run builds its
/// blobs for each draw rather than drawing again blobs built for another
/// paint, which Skia's GPU backend draws unlike fresh ones -- see
/// `LaidOut`'s blobs.
#[test]
fn a_run_replayed_on_the_gpu_draws_as_a_fresh_one() {
    FontLibrary::new()
        .use_paths(Some(OSWALD_FAMILY), [OSWALD])
        .expect("bundled Oswald registers");
    let colours = [
        ("#000000", 1.0),
        ("#203040", 1.0),
        ("green", 1.0),
        ("green", 0.5),
        ("#c03010", 1.0),
        ("#808080", 1.0),
        ("#ffffff", 1.0),
    ];
    let mut holder = Canvas::new(WIDTH, HEIGHT);
    let patterns: Vec<(Pattern, f32)> = colours
        .iter()
        .map(|&(colour, alpha)| {
            let mut source = Canvas::new(20.0, 20.0);
            let tile = source.context();
            tile.set_fill_style_css(colour).expect("a colour");
            tile.fill_rect(0.0, 0.0, 20.0, 20.0);
            let pattern = holder
                .context()
                .create_pattern_from_canvas(&mut source, PatternRepeat::Repeat);
            (pattern, alpha)
        })
        .collect();
    let draw = |canvas: &mut Canvas, (pattern, alpha): &(Pattern, f32)| {
        let ctx = canvas.context();
        ctx.clear_rect(0.0, 0.0, WIDTH, HEIGHT);
        ctx.set_global_alpha(*alpha);
        ctx.set_fill_pattern(pattern);
        ctx.fill_text("Hamburgefonstiv fi AV", 10.0, 70.0, None);
        pixels(canvas, 1)
    };
    let canvas = || {
        let mut canvas = Canvas::new(WIDTH, HEIGHT);
        canvas.set_gpu(true);
        assert!(
            canvas
                .context()
                .set_font_css(&format!("28px {OSWALD_FAMILY}"))
        );
        canvas
    };
    let mut differing = vec![];
    for (second, colour) in patterns.iter().zip(colours) {
        let wanted = draw(&mut canvas(), second);
        for (first, before) in patterns.iter().zip(colours) {
            // Twice in the first, for the run to be kept.
            let mut again = canvas();
            draw(&mut again, first);
            draw(&mut again, first);
            if draw(&mut again, second) != wanted {
                differing.push(format!("{before:?} then {colour:?}"));
            }
        }
    }
    assert!(differing.is_empty(), "{differing:#?}");
}
