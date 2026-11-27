//! A blurred fill on the GPU covers its pixels alike whether or not another
//! blurred mask is computed in the same flush.
//!
//! Two blurred ellipses of `examples/node/animated-eye.js`'s first frame: a
//! large shading blob at the left, and a small "caustic" in the iris at the
//! right, far outside the blob's reach. The caustic's pixels ought not to
//! depend on whether the blob is drawn too. Drawn by a Skia without
//! containers/skia-atlas-placement.patch, on Metal they did: with the blob's
//! mask computed in the same flush, the atlas path renderer packed the
//! caustic elsewhere in its atlas and moved its alpha by one level at 25
//! pixels, where a mask already cached from an earlier draw did not -- which
//! is how a frame came to read differently with what the GPU had drawn
//! before it.
//!
//! Its own file because the other GPU tests draw the same blob, and a mask
//! they left cached would hide the difference.

use meo_skia_canvas::prelude::*;

mod common;

/// The page the ellipses were recorded on, in pixels.
const WIDTH: f32 = 640.0;
const HEIGHT: f32 = 500.0;

/// An ellipse as `Context2D::ellipse` takes it, with the blur, alpha and
/// colour it is filled with.
struct Blurred {
    ellipse: [f32; 7],
    sigma: f32,
    alpha: f32,
    style: &'static str,
}

/// The shading blob: draw 286 of the example's first frame.
const BLOB: Blurred = Blurred {
    ellipse: [94.0, 280.0, 36.0, 82.0, 0.1, 0.0, 7.0],
    sigma: 26.0,
    alpha: 0.3,
    style: "#dda595",
};

/// The caustic: draw 1512 of the same frame.
///
/// Its centre's x is the `f32` the example's `350.37464871341285` rounds to,
/// `350.374_63` naming the same one, and that precision is load-bearing: one
/// `f32` step to the right, `350.374_664`, and an unpatched Skia's caustic
/// does not move at all, so where the path falls decides whether this test
/// can see the defect.
const CAUSTIC: Blurred = Blurred {
    ellipse: [350.374_63, 302.72, 44.0, 26.4, 0.5, 0.0, 7.0],
    sigma: 9.0,
    alpha: 1.0,
    style: "rgba(190, 238, 252, 0.4)",
};

/// The box around the caustic that the comparison reads, `(x, y, width,
/// height)`: wide of the caustic's blur, and well clear of the blob's.
const CAUSTIC_BOX: (usize, usize, usize, usize) = (250, 200, 199, 140);

fn fill(ctx: &mut Context2D, shape: &Blurred, dx: f32) -> Result<(), Error> {
    let [x, y, rx, ry, rotation, start, end] = shape.ellipse;
    ctx.save();
    ctx.set_global_alpha(shape.alpha);
    ctx.set_fill_style_css(shape.style)?;
    let blur = MaskFilter::blur(BlurStyle::Normal, shape.sigma, true)?;
    ctx.set_mask_filter(Some(&blur));
    ctx.begin_path();
    ctx.ellipse(x + dx, y, rx, ry, rotation, start, end, false)?;
    ctx.fill(FillRule::NonZero);
    ctx.restore();
    Ok(())
}

/// The alpha of a GPU page holding `shapes`, each moved `dx` pixels right.
///
/// A whole pixel's move changes no pixel's coverage, but it does change the
/// path, and so the key Skia caches a blurred mask under: each call that
/// moves the caustic computes its mask afresh rather than reading the one an
/// earlier call left.
fn alpha(shapes: &[&Blurred], dx: f32) -> Result<Vec<u8>, Error> {
    let mut canvas = Canvas::new(WIDTH, HEIGHT);
    canvas.set_gpu(true);
    let ctx = canvas.context();
    for shape in shapes {
        fill(ctx, shape, dx)?;
    }
    let pixels =
        canvas.to_buffer(ImageFormat::Raw, &EncodeOptions::default())?;
    Ok(pixels.chunks(4).map(|px| px[3]).collect())
}

/// How far each pixel of the caustic's box in `a` lies from the same pixel of
/// `b` moved `shift` pixels right, where the two differ.
fn parted(a: &[u8], b: &[u8], shift: usize) -> Vec<u8> {
    let (left, top, width, height) = CAUSTIC_BOX;
    let row = WIDTH as usize;
    (top..top + height)
        .flat_map(|y| (left..left + width).map(move |x| (x, y)))
        .map(|(x, y)| a[y * row + x].abs_diff(b[y * row + x + shift]))
        .filter(|&level| level != 0)
        .collect()
}

/// Skips where there is no GPU, and fails where one is required or Metal has
/// a device but the engine did not start on it -- see `common::has_gpu`.
///
/// Ignored, saying why, where the Skia compiled lacks the atlas placement --
/// `cfg(meo_atlas_placement)`, from `build.rs`.
#[test]
#[cfg_attr(
    not(meo_atlas_placement),
    ignore = "crates.io's Skia, without containers/skia-atlas-placement.patch, \
              moves the caustic's coverage with where the flush packs it in \
              the atlas"
)]
fn a_blurred_mask_in_the_same_flush_leaves_another_alone() -> Result<(), Error>
{
    if !common::has_gpu() {
        return Ok(());
    }

    let both = alpha(&[&BLOB, &CAUSTIC], 0.0)?;
    let alone = alpha(&[&CAUSTIC], 1.0)?;
    // The instrument: the caustic alone, moved a pixel further, lies over
    // itself exactly, so a difference below is the blob's and not the move's.
    let control = alpha(&[&CAUSTIC], 2.0)?;
    assert!(
        parted(&alone, &control, 1).is_empty(),
        "the caustic alone does not lie over itself a pixel further on"
    );

    let moved = parted(&both, &alone, 1);
    assert!(
        moved.is_empty(),
        "the caustic beside the blob reads {} pixels apart from the caustic \
         alone, by up to {} levels",
        moved.len(),
        moved.iter().max().copied().unwrap_or(0),
    );
    Ok(())
}
