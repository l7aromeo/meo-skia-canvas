//! Consecutive blurred rectangles drawn alike are recorded as one run and
//! played back together. A run has to end wherever the drawing changes, and
//! must never grow after a page has been read: each is checked here against
//! the same draws made so that no two can share a run.

use meo_skia_canvas::prelude::*;

fn pixels(canvas: &mut Canvas) -> Vec<u8> {
    canvas
        .to_image_data(None, &ImageDataExport::default())
        .expect("pixels")
        .pixels()
        .to_vec()
}

/// What changes between the fourth and fifth of eight blurred rectangles.
#[derive(Clone, Copy, Debug)]
enum Break {
    Nothing,
    Colour,
    Alpha,
    Sigma,
    Transform,
    Clip,
    APath,
    AStroke,
    /// An unblurred rectangle under where the fifth blurred one lands,
    /// which has to stay beneath it.
    APlainDraw,
}

const BREAKS: [Break; 9] = [
    Break::Nothing,
    Break::Colour,
    Break::Alpha,
    Break::Sigma,
    Break::Transform,
    Break::Clip,
    Break::APath,
    Break::AStroke,
    Break::APlainDraw,
];

/// Eight blurred rectangles with `change` made before the fifth. With
/// `apart`, an unblurred draw off the canvas comes between each two, which
/// keeps every draw out of any run by a rule apart from the ones under test
/// -- something was recorded in between -- and leaves the pixels alone.
fn draw(gpu: bool, change: Break, apart: bool) -> Option<Canvas> {
    let mut canvas = Canvas::new(200.0, 150.0);
    canvas.set_gpu(gpu);
    if gpu && canvas.engine_kind() != EngineKind::Gpu {
        return None;
    }
    let ctx = canvas.context();
    ctx.set_fill_style_css("rgba(30, 60, 200, 0.6)")
        .expect("a colour");
    ctx.set_filter_css("blur(3px)").expect("a filter");
    for i in 0..8 {
        let x = 10.0 + i as f32 * 22.0;
        if i == 4 {
            match change {
                Break::Nothing => {}
                Break::Colour => ctx
                    .set_fill_style_css("rgba(200, 30, 30, 0.6)")
                    .expect("a colour"),
                Break::Alpha => ctx.set_global_alpha(0.3),
                Break::Sigma => {
                    ctx.set_filter_css("blur(6px)").expect("a filter")
                }
                Break::Transform => ctx.translate(3.0, 7.0),
                Break::Clip => {
                    ctx.begin_path();
                    ctx.rect(0.0, 0.0, 120.0, 150.0);
                    ctx.clip(FillRule::NonZero);
                }
                Break::APath => {
                    ctx.begin_path();
                    ctx.arc(
                        100.0,
                        75.0,
                        20.0,
                        0.0,
                        std::f32::consts::TAU,
                        false,
                    )
                    .expect("an arc");
                    ctx.fill(FillRule::NonZero);
                }
                Break::AStroke => {
                    ctx.set_line_width(3.0);
                    ctx.stroke_rect(x, 40.0, 18.0, 18.0);
                }
                // Set and put back by hand: a `save` would change the frame,
                // which ends a run by another rule.
                Break::APlainDraw => {
                    ctx.set_filter_css("none").expect("no filter");
                    ctx.set_fill_style_css("orange").expect("a colour");
                    ctx.fill_rect(x - 5.0, 40.0, 40.0, 40.0);
                    ctx.set_fill_style_css("rgba(30, 60, 200, 0.6)")
                        .expect("a colour");
                    ctx.set_filter_css("blur(3px)").expect("a filter");
                }
            }
        }
        ctx.fill_rect(x, 20.0 + (i % 3) as f32 * 30.0, 30.0, 40.0);
        if apart {
            ctx.save();
            ctx.set_filter_css("none").expect("no filter");
            ctx.fill_rect(-50.0, -50.0, 10.0, 10.0);
            ctx.restore();
        }
    }
    Some(canvas)
}

#[test]
fn a_run_of_blurred_rectangles_ends_where_the_drawing_changes() {
    for gpu in [false, true] {
        for change in BREAKS {
            let (Some(mut run), Some(mut apart)) =
                (draw(gpu, change, false), draw(gpu, change, true))
            else {
                continue;
            };
            assert!(
                pixels(&mut run) == pixels(&mut apart),
                "gpu {gpu}, {change:?} before the fifth rectangle"
            );
        }
    }
}

#[test]
fn a_run_read_part_way_does_not_grow_behind_the_read() {
    // A page read while a run is open is cached; rectangles drawn after it
    // have to reach the next read rather than join a layer already drawn.
    for gpu in [false, true] {
        let more = |canvas: &mut Canvas| {
            let ctx = canvas.context();
            for i in 0..4 {
                ctx.fill_rect(15.0 + i as f32 * 40.0, 100.0, 25.0, 25.0);
            }
        };
        let (Some(mut read), Some(mut whole)) = (
            draw(gpu, Break::Nothing, false),
            draw(gpu, Break::Nothing, false),
        ) else {
            continue;
        };
        pixels(&mut read);
        more(&mut read);
        more(&mut whole);
        assert!(pixels(&mut read) == pixels(&mut whole), "gpu {gpu}");
    }
}
