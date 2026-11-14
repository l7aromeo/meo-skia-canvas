//! A path drawn on the GPU covers the same pixels wherever the path atlas
//! places it.
//!
//! Ganesh draws a small path that is not known to be convex into an atlas
//! first, at a location that depends on what else the flush put there, and
//! samples the page's pixels from it. The rounded rectangle here is drawn
//! after `k` copies of itself, each elsewhere on the page and each the same
//! size in the atlas, so that the flush packs it at a different atlas column
//! for every `k`. Its own pixels are compared with a flush that holds it
//! alone.
//!
//! Drawn by a Skia without containers/skia-atlas-placement.patch, a copy
//! placed with its atlas columns at 256 or more read one pixel's alpha a
//! sample lower: 128 rather than 171 at (284, 192), where its device rows
//! are under 256. A path whose atlas coordinates are of a higher power of
//! two than its device coordinates is drawn at the coarser float precision
//! of that band. The patch keeps each path's atlas location within the
//! power-of-two band of its device location in each axis.

use std::{env, ops::Range};

use meo_skia_canvas::prelude::*;

/// The page, in pixels.
const WIDTH: usize = 720;
const HEIGHT: usize = 480;

/// The page's turn about its centre, in radians: enough that no edge of
/// the rectangle lies on a whole pixel.
const TURN: f32 = 0.15;

/// The rectangle that is compared: its top-left corner before the turn, its
/// size, and its corners' radius.
const ORIGIN: (f32, f32) = (204.5, 144.25);
const SIZE: (f32, f32) = (90.0, 60.0);
const RADIUS: f32 = 10.0;

/// The copies drawn ahead of it sit in a row along the page's top, this far
/// apart, clear of every pixel the rectangle reaches.
const ROW: (f32, f32) = (12.5, 12.25);
const STEP: f32 = 96.0;

/// Copies enough to pack the rectangle past atlas column 256 twice over:
/// each takes 75 columns of the atlas.
const COPIES: usize = 7;

/// How far past its turned bounds a path's anti-aliasing can reach, in
/// pixels.
const FRINGE: f32 = 2.0;

/// The same text tests/gpu_path_coverage.rs reads, for the same reason: CI's
/// Linux runner sets it so that a green run has drawn on lavapipe.
const REQUIRE_GPU: &str = "MEO_SKIA_CANVAS_REQUIRE_GPU";

/// The page's centre, which it turns about.
const CENTRE: (f32, f32) = (WIDTH as f32 / 2.0, HEIGHT as f32 / 2.0);

/// Where the page's turn takes the point `(x, y)`.
fn turned((x, y): (f32, f32)) -> (f32, f32) {
    let (sin, cos) = TURN.sin_cos();
    let (dx, dy) = (x - CENTRE.0, y - CENTRE.1);
    (
        CENTRE.0 + dx * cos - dy * sin,
        CENTRE.1 + dx * sin + dy * cos,
    )
}

/// The pixels the rectangle at `origin` can reach, as column and row ranges.
fn reach(origin: (f32, f32)) -> (Range<usize>, Range<usize>) {
    let corners = [(0.0, 0.0), (SIZE.0, 0.0), (0.0, SIZE.1), (SIZE.0, SIZE.1)]
        .map(|(dx, dy)| turned((origin.0 + dx, origin.1 + dy)));
    let span = |values: [f32; 4], limit: usize| {
        let low = values.into_iter().fold(f32::INFINITY, f32::min);
        let high = values.into_iter().fold(f32::NEG_INFINITY, f32::max);
        let start = (low - FRINGE).floor().max(0.0) as usize;
        let end = ((high + FRINGE).ceil() as usize).min(limit);
        start..end
    };
    (
        span(corners.map(|c| c.0), WIDTH),
        span(corners.map(|c| c.1), HEIGHT),
    )
}

/// The top-left corner of copy `k`, before the turn.
fn copy(k: usize) -> (f32, f32) {
    (ROW.0 + k as f32 * STEP, ROW.1)
}

/// A GPU page holding `copies` copies of the rectangle along its top, then
/// the rectangle, all in one flush, read back.
fn page(copies: usize) -> Result<Vec<u8>, Error> {
    let mut canvas = Canvas::new(WIDTH as f32, HEIGHT as f32);
    canvas.set_gpu(true);
    let ctx = canvas.context();
    ctx.translate(CENTRE.0, CENTRE.1);
    ctx.rotate(TURN);
    ctx.translate(-CENTRE.0, -CENTRE.1);
    ctx.set_fill_style_css("rgb(30 90 160)")?;
    for (x, y) in (0..copies).map(copy).chain([ORIGIN]) {
        ctx.begin_path();
        ctx.round_rect(x, y, SIZE.0, SIZE.1, [RADIUS; 4])?;
        ctx.fill(FillRule::NonZero);
    }
    canvas.to_buffer(ImageFormat::Raw, &EncodeOptions::default())
}

/// Skips where there is no GPU, unless [`REQUIRE_GPU`] asks for one.
///
/// Ignored, saying why, where the Skia compiled lacks the placement --
/// `cfg(meo_atlas_placement)`, from `build.rs`.
#[test]
#[cfg_attr(
    not(meo_atlas_placement),
    ignore = "crates.io's Skia, without containers/skia-atlas-placement.patch, \
              moves a path's coverage with the atlas column the flush packs \
              it at"
)]
fn a_path_covers_its_pixels_wherever_the_atlas_places_it() -> Result<(), Error>
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

    let (columns, rows) = reach(ORIGIN);
    // The copies are clear of every pixel the rectangle reaches, so the
    // pixels compared are the rectangle's alone.
    for k in 0..COPIES {
        let (c, r) = reach(copy(k));
        assert!(
            c.end <= columns.start
                || columns.end <= c.start
                || r.end <= rows.start,
            "copy {k} reaches {c:?} x {r:?}, inside the rectangle's {columns:?} x {rows:?}"
        );
    }
    let alone = page(0)?;
    let moved: Vec<(usize, usize, usize, u8, u8)> = (1..=COPIES)
        .map(|copies| page(copies).map(|after| (copies, after)))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .flat_map(|(copies, after)| {
            let (alone, columns) = (&alone, columns.clone());
            rows.clone()
                .flat_map(move |y| columns.clone().map(move |x| (x, y)))
                .filter_map(move |(x, y)| {
                    let at = (y * WIDTH + x) * 4 + 3;
                    (after[at] != alone[at])
                        .then_some((copies, x, y, after[at], alone[at]))
                })
                .collect::<Vec<_>>()
        })
        .collect();

    assert!(
        moved.is_empty(),
        "{} alphas of the rectangle differ from a flush holding it alone, \
         after {:?} copies; first (copies, x, y, after copies, alone) {:?}",
        moved.len(),
        {
            let mut copies: Vec<usize> = moved.iter().map(|m| m.0).collect();
            copies.dedup();
            copies
        },
        moved.first(),
    );
    Ok(())
}
