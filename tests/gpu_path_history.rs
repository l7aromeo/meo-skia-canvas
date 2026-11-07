//! A GPU canvas draws a curve the same whatever the context drew before.
//!
//! Ganesh's `TriangulatingPathRenderer` keeps a path's triangles in the
//! context's `GrThreadSafeCache`, keyed by the path's shape and not by the
//! transform it is drawn under, and reuses an entry made at any tolerance
//! under three times the one a draw asks for, finer or coarser -- `cache_match`
//! in Skia M156's `src/gpu/ganesh/ops/TriangulatingPathRenderer.cpp`, under
//! the skia-bindings crate. So a curve exported at density 1 is drawn from its
//! own triangles on a fresh context, and from the finer ones a density-3
//! export left, and under 4x MSAA the two move edge pixels by up to two
//! samples' worth of coverage: 128 levels. `containers/
//! skia-triangulation-tolerance.patch` reuses an entry only at the draw's own
//! tolerance. A crate build's Skia does not carry it, and
//! `cfg(meo_triangulation_tolerance)`, from `build.rs`, says which this build
//! compiled.

use meo_skia_canvas::prelude::*;

const SIZE: (f32, f32) = (300.0, 220.0);

/// The turn the text is drawn under in issue #511: about 17 degrees.
const TURN: Affine = Affine {
    a: 0.955,
    b: 0.296,
    c: -0.296,
    d: 0.955,
    tx: 0.0,
    ty: 0.0,
};

/// The whole page of `canvas` on the GPU at `density`, premultiplied.
fn exported(canvas: &mut Canvas, density: u32) -> Vec<u8> {
    canvas.set_gpu(true);
    let pixels = canvas
        .to_image_data(
            None,
            &ImageDataExport {
                density,
                pixels: PixelExportOptions {
                    premultiplied: true,
                    ..PixelExportOptions::default()
                },
                ..ImageDataExport::default()
            },
        )
        .expect("pixels")
        .pixels()
        .to_vec();
    canvas.set_gpu(false);
    pixels
}

/// `canvas` exported on the GPU at density 1, then at density 3, then at
/// density 1 again: the first and last should be the same pixels. `None`
/// where this machine has no GPU engine.
fn before_and_after(mut canvas: Canvas) -> Option<(Vec<u8>, Vec<u8>)> {
    let mut probe = Canvas::new(1.0, 1.0);
    probe.set_gpu(true);
    if probe.engine_kind() != EngineKind::Gpu {
        eprintln!("skipped: no GPU engine");
        return None;
    }
    let first = exported(&mut canvas, 1);
    exported(&mut canvas, 3);
    let again = exported(&mut canvas, 1);
    Some((first, again))
}

/// The largest difference between two exports, and how many bytes differ.
fn apart(a: &[u8], b: &[u8]) -> (u8, usize) {
    let worst = a.iter().zip(b).map(|(a, b)| a.abs_diff(*b)).max();
    let count = a.iter().zip(b).filter(|(a, b)| a != b).count();
    (worst.unwrap_or(0), count)
}

/// Holds a scene's first and last density-1 exports to what the Skia
/// compiled does: the same bytes where it carries the patch, and some bytes
/// moved where it does not, since crates.io's Skia then draws the last from
/// the triangles the density-3 export cut. How many move, and how far,
/// depends on the device's multisampling, so only that some do is held; the
/// day none do there, the cache rule this note describes has changed.
fn assert_drawn_as_the_skia_compiled_draws(first: &[u8], again: &[u8]) {
    let (worst, count) = apart(first, again);
    if cfg!(meo_triangulation_tolerance) {
        assert_eq!((worst, count), (0, 0));
    } else {
        assert!(
            count > 0,
            "crates.io's Skia drew the curve again from its own triangles; \
             it reuses another tolerance's no longer"
        );
    }
}

#[test]
fn a_curve_after_a_larger_draw_of_it_draws_alike_where_skia_is_patched() {
    let mut canvas = Canvas::new(SIZE.0, SIZE.1);
    canvas.set_gpu(false);
    let ctx = canvas.context();
    ctx.set_fill_style(RgbaLinear::from_srgb8(200, 120, 40, 1.0));
    ctx.translate(60.3, 40.6);
    ctx.transform(TURN);
    ctx.begin_path();
    ctx.move_to(0.0, 0.0);
    ctx.bezier_curve_to(140.0, -60.0, 40.0, 160.0, 190.0, 120.0);
    ctx.bezier_curve_to(90.0, 40.0, 160.0, 210.0, 10.0, 130.0);
    ctx.bezier_curve_to(80.0, 70.0, -40.0, 60.0, 0.0, 0.0);
    ctx.fill(FillRule::NonZero);
    let Some((first, again)) = before_and_after(canvas) else {
        return;
    };
    assert_drawn_as_the_skia_compiled_draws(&first, &again);
}

/// The scene of issue #511: Zapfino, turned, at 96px. Its large glyphs are
/// drawn as paths, through the same cache. Skipped, saying so, where the
/// system has no Zapfino, as on Linux: the text then falls back to another
/// face, which drew alike on either Skia when measured on Linux, so the
/// scene would hold nothing there.
#[test]
fn turned_text_after_a_larger_draw_of_it_draws_alike_where_skia_is_patched() {
    if FontLibrary::new().family_details("Zapfino").is_none() {
        eprintln!(
            "skipped: no Zapfino, which the scene of issue #511 is set in"
        );
        return;
    }
    let mut canvas = Canvas::new(SIZE.0, SIZE.1);
    canvas.set_gpu(false);
    let ctx = canvas.context();
    ctx.set_font_css("96px \"Zapfino\"");
    ctx.set_fill_style(RgbaLinear::from_srgb8(200, 120, 40, 1.0));
    ctx.translate(60.3, 110.6);
    ctx.transform(TURN);
    ctx.fill_text("fj Wq", 0.0, 0.0, None);
    let Some((first, again)) = before_and_after(canvas) else {
        return;
    };
    assert_drawn_as_the_skia_compiled_draws(&first, &again);
}

/// This repository's builds compile the tree `scripts/skia-source.mjs`
/// prepared, which carries `containers/skia-triangulation-tolerance.patch` on
/// every platform, and name it in `SKIA_SOURCE_DIR`: such a build from an
/// unpatched tree fails here by name rather than only by the pixels above. A
/// build that names no tree compiles crates.io's Skia, which does not carry
/// it, and `build.rs` must say so, or the tests above would hold it to pixels
/// it cannot draw.
#[test]
fn a_build_carries_the_triangulation_patch_where_it_names_a_patched_tree() {
    assert_eq!(
        cfg!(meo_triangulation_tolerance),
        option_env!("SKIA_SOURCE_DIR").is_some(),
        "cfg(meo_triangulation_tolerance) against SKIA_SOURCE_DIR {:?}",
        option_env!("SKIA_SOURCE_DIR"),
    );
}
