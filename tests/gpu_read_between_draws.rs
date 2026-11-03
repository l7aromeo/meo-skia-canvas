//! A read between draws on a GPU canvas changes nothing drawn after it.
//!
//! A read ends the render pass the page's draws are in, and the next draw
//! opens another on the same multisampled target. The page's samples have
//! to come back as they were left for an edge drawn over another edge to
//! cover it as it would in one pass: a fill of alpha 0.8 whose right edge is
//! at x = 300.5, drawn twice, covers the same samples of column 300 twice,
//! and reads back an alpha of 123 -- about half of 0.96 -- with or without a
//! read between. Where the samples come back from the resolved pixel instead,
//! each takes the pixel's mean, and the second fill reads back 163.
//!
//! The scenes are drawn twice, once read after every step and once read only
//! at the end, and the two final reads have to be the same bytes.

use std::env;

use meo_skia_canvas::prelude::*;

/// The page, in pixels.
const WIDTH: f32 = 600.0;
const HEIGHT: f32 = 400.0;

/// How many times each scene is drawn over itself.
const STEPS: usize = 4;

/// The same text tests/gpu_path_coverage.rs reads, for the same reason: CI's
/// Linux runner sets it so that a green run has drawn on lavapipe.
const REQUIRE_GPU: &str = "MEO_SKIA_CANVAS_REQUIRE_GPU";

/// One step of a scene: draws onto `ctx`, the `step`-th time.
type Scene = fn(&mut Context2D, usize) -> Result<(), Error>;

/// A translucent rectangle whose right edge halves column 300, so that its
/// samples there are covered and uncovered alike, drawn once a step.
fn halved_column(ctx: &mut Context2D, _: usize) -> Result<(), Error> {
    ctx.set_fill_style_css("rgb(10 90 200 / 0.8)")?;
    ctx.fill_rect(0.0, 0.0, WIDTH / 2.0 + 0.5, HEIGHT);
    Ok(())
}

/// A translucent circle moved a fraction of a pixel each step, and a stroke
/// across it, so that edges fall over the last step's edges.
fn crossing_edges(ctx: &mut Context2D, step: usize) -> Result<(), Error> {
    ctx.set_fill_style_css("color(display-p3 0.1 0.5 0.9 / 0.7)")?;
    ctx.begin_path();
    ctx.arc(
        WIDTH / 2.0 + step as f32 * 3.3,
        HEIGHT / 2.0,
        110.3,
        0.0,
        7.0,
        false,
    )?;
    ctx.fill(FillRule::NonZero);
    ctx.set_stroke_style_css("rgb(200 20 60)")?;
    ctx.set_line_width(2.7);
    ctx.begin_path();
    (0..30).for_each(|i| {
        let y = 40.0 + ((i * 37 + step * 11) % 200) as f32 + 0.4;
        ctx.line_to(20.0 + i as f32 * 17.3, y);
    });
    ctx.stroke();
    Ok(())
}

/// Draws `scene` [`STEPS`] times on a GPU canvas of `options`, reading the
/// whole page after every step where `read_between` says to, and returns the
/// last read. `None` where the canvas is not on the GPU.
fn drawn(
    scene: Scene,
    options: &CanvasOptions,
    read_between: bool,
) -> Result<Option<ImageData>, Error> {
    let mut canvas = Canvas::with_options(WIDTH, HEIGHT, options.clone())?;
    canvas.set_gpu(true);
    if canvas.engine_kind() != EngineKind::Gpu {
        return Ok(None);
    }
    let ctx = canvas.context();
    for step in 0..STEPS {
        ctx.save();
        scene(ctx, step)?;
        ctx.restore();
        if read_between && step + 1 < STEPS {
            ctx.get_image_data(0.0, 0.0, WIDTH, HEIGHT)?;
        }
    }
    ctx.get_image_data(0.0, 0.0, WIDTH, HEIGHT).map(Some)
}

/// Skips where there is no GPU, unless [`REQUIRE_GPU`] asks for one.
#[test]
fn a_read_between_draws_changes_no_later_pixel() -> Result<(), Error> {
    let mut probe = Canvas::new(1.0, 1.0);
    probe.set_gpu(true);
    if let Ok(required) = env::var(REQUIRE_GPU) {
        let backend = BackendInfo::query();
        assert!(
            probe.engine_kind() == EngineKind::Gpu
                && backend
                    .device
                    .as_deref()
                    .is_some_and(|device| device.contains(&required)),
            "{REQUIRE_GPU} asks for a GPU named {required:?}; the engine is \
             {:?} on {:?}, error {:?}",
            probe.engine_kind(),
            backend.device,
            backend.error,
        );
    }
    if probe.engine_kind() != EngineKind::Gpu {
        return Ok(());
    }

    let pages = [
        ("sRGB, 8-bit", CanvasOptions::default()),
        (
            "Display P3, 8-bit",
            CanvasOptions {
                color_space: PixelColorSpace::DisplayP3,
                ..CanvasOptions::default()
            },
        ),
    ];
    let scenes: [(&str, Scene); 2] = [
        ("halved column", halved_column),
        ("crossing edges", crossing_edges),
    ];
    let mut compared = 0;
    for (page, options) in &pages {
        for (name, scene) in scenes {
            let (Some(read_between), Some(read_once)) =
                (drawn(scene, options, true)?, drawn(scene, options, false)?)
            else {
                continue;
            };
            assert!(
                read_between == read_once,
                "{name} on a {page} page: reading between the draws changed \
                 the page the draws after it left"
            );
            compared += 1;
        }
    }
    assert_eq!(
        compared,
        pages.len() * scenes.len(),
        "a page was not on the GPU"
    );
    Ok(())
}
