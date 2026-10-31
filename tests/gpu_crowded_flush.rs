//! A path drawn on the GPU covers each pixel as it does in a flush of its
//! own, however many other paths the flush holds.
//!
//! Two thousand small filled stars on one page: about 26 by 26 pixels of atlas
//! each, enough to fill a 512-by-512 atlas five times over, and a 2048 one a
//! third full. Each pixel's alpha is checked against the stars that can reach
//! it, drawn alone, in the page's order, on a canvas of their own: a flush of
//! a few stars, whose atlas stays small. Without the atlas cap the crowded
//! page read 51 levels of alpha above or below that at 28 pixels, 19 above and
//! 9 below, single stars among them -- so the error goes both ways, and is
//! not a matter of compositing.

use std::{
    env,
    f32::consts::{FRAC_PI_2, PI},
    ops::Range,
};

use meo_skia_canvas::prelude::*;

/// The page, in pixels.
const WIDTH: usize = 1280;
const HEIGHT: usize = 960;

/// How many stars the page holds.
const STARS: usize = 2000;

/// The side of the box a star is drawn in: its outer radius is 12 about the
/// box's centre.
const SIDE: f32 = 24.0;

/// How far past its box a star's anti-aliasing can reach, in pixels.
const FRINGE: f32 = 2.0;

/// The same text tests/gpu_path_coverage.rs reads, for the same reason: CI's
/// Linux runner sets it so that a green run has drawn on lavapipe.
const REQUIRE_GPU: &str = "MEO_SKIA_CANVAS_REQUIRE_GPU";

/// A five-pointed star in a `SIDE` box, its points 12 and 5 from the centre.
fn star() -> Path2D {
    let mut builder = PathBuilder::new();
    for k in 0..10 {
        let radius = if k % 2 == 1 { 5.0 } else { 12.0 };
        let angle = k as f32 * PI / 5.0 - FRAC_PI_2;
        let (x, y) = (12.0 + radius * angle.cos(), 12.0 + radius * angle.sin());
        match k {
            0 => builder.move_to(x, y),
            _ => builder.line_to(x, y),
        };
    }
    builder.close_path();
    builder.build(FillRule::NonZero)
}

/// Where star `i` is drawn: spread over the page, at fractions of a pixel
/// that vary from star to star.
fn origin(i: usize) -> (f32, f32) {
    (
        ((i * 53) % 1256) as f32 + (i % 7) as f32 * 0.3,
        ((i * 97) % 936) as f32 + (i % 5) as f32 * 0.2,
    )
}

/// The pixels star `i` can reach, as column and row ranges on the page.
fn reach(i: usize) -> (Range<usize>, Range<usize>) {
    let (x, y) = origin(i);
    let span = |from: f32, limit: usize| {
        let start = (from - FRINGE).floor().max(0.0) as usize;
        let end = ((from + SIDE + FRINGE).ceil() as usize).min(limit);
        start..end
    };
    (span(x, WIDTH), span(y, HEIGHT))
}

fn overlaps(a: &Range<usize>, b: &Range<usize>) -> bool {
    a.start < b.end && b.start < a.end
}

/// Draws `stars`, in order, moved `(dx, dy)`, onto a GPU canvas of
/// `width` by `height`, and returns its pixels.
fn drawn(
    stars: &[usize],
    (dx, dy): (f32, f32),
    width: usize,
    height: usize,
) -> Result<Vec<u8>, Error> {
    let path = star();
    let mut canvas = Canvas::new(width as f32, height as f32);
    canvas.set_gpu(true);
    let ctx = canvas.context();
    for &i in stars {
        let (x, y) = origin(i);
        ctx.set_fill_style_css(&format!(
            "hsl({} 70% 50% / 0.8)",
            (i * 37) % 360
        ))?;
        ctx.set_transform(Affine::translation(x - dx, y - dy));
        ctx.fill_path(&path, FillRule::NonZero);
    }
    canvas.to_buffer(ImageFormat::Raw, &EncodeOptions::default())
}

/// Skips where there is no GPU, unless [`REQUIRE_GPU`] asks for one.
#[test]
fn a_path_covers_a_crowded_flush_as_it_covers_a_small_one() -> Result<(), Error>
{
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

    let all: Vec<usize> = (0..STARS).collect();
    let page = drawn(&all, (0.0, 0.0), WIDTH, HEIGHT)?;

    // Every star that reaches a pixel of star `i`'s box reaches the box, so
    // drawing those alone, moved by a whole number of pixels so that each
    // keeps its fraction, gives the box as a small flush draws it.
    let mut moved: Vec<(usize, usize, u8, u8)> = all
        .iter()
        .flat_map(|&i| {
            let (columns, rows) = reach(i);
            let near: Vec<usize> = all
                .iter()
                .copied()
                .filter(|&j| {
                    let (c, r) = reach(j);
                    overlaps(&c, &columns) && overlaps(&r, &rows)
                })
                .collect();
            let (left, top) = (columns.start, rows.start);
            let (width, height) = (columns.len(), rows.len());
            let alone = drawn(&near, (left as f32, top as f32), width, height)
                .expect("a small flush draws");
            rows.clone()
                .flat_map(|y| columns.clone().map(move |x| (x, y)))
                .filter_map(|(x, y)| {
                    let crowded = page[(y * WIDTH + x) * 4 + 3];
                    let small = alone[((y - top) * width + x - left) * 4 + 3];
                    (crowded != small).then_some((x, y, crowded, small))
                })
                .collect::<Vec<_>>()
        })
        .collect();
    // Boxes overlap, so a pixel can be read more than once.
    moved.sort_unstable();
    moved.dedup();

    assert!(
        moved.is_empty(),
        "{} pixels of the crowded page differ from a small flush of the stars \
         that reach them, {} higher and {} lower, first (x, y, crowded, small) \
         {:?}",
        moved.len(),
        moved.iter().filter(|m| m.2 > m.3).count(),
        moved.iter().filter(|m| m.2 < m.3).count(),
        moved.first(),
    );
    Ok(())
}
