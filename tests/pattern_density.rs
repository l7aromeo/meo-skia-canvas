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
