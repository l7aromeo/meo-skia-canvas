//! Pins a defect: on Metal, a blurred fill moves by a level where another
//! blurred mask is computed in the same flush.
//!
//! Two blurred ellipses of `examples/node/animated-eye.js`'s first frame: a
//! large shading blob at the left, and a small "caustic" in the iris at the
//! right, far outside the blob's reach. The caustic's pixels ought not to
//! depend on whether the blob is drawn too. On Metal they do: with the blob's
//! mask computed in the same flush, Skia's atlas path renderer moves the
//! caustic's alpha by one level at a few dozen pixels, and a mask already
//! cached from an earlier draw does not -- which is how a frame came to read
//! differently with what the GPU had drawn before it. The cause is not known
//! and nothing here works around it.
//!
//! This test asserts the defect as it stands, so that it fails, as the sign
//! to look again, the day the caustic stops moving. Its own file because the
//! other GPU tests draw the same blob, and a mask they left cached would hide
//! the defect.

use meo_skia_canvas::prelude::*;

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
/// `f32` step to the right, `350.374_664`, and the caustic does not move at
/// all. So the defect is a matter of where the path falls, and this test can
/// flip on a Skia change that only shifts it -- which is still the sign to
/// look again.
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

/// Whether Metal has a device to offer, asked of Metal itself rather than of
/// the engine under test.
#[cfg(feature = "metal")]
fn has_metal_device() -> bool {
    objc2_metal::MTLCreateSystemDefaultDevice().is_some()
}

#[cfg(not(feature = "metal"))]
fn has_metal_device() -> bool {
    false
}

/// Skips where there is no GPU and the defect has nowhere to show, and fails
/// where Metal has a device but the engine did not start on it.
#[test]
fn a_blurred_mask_in_the_same_flush_moves_another_by_a_level()
-> Result<(), Error> {
    let mut probe = Canvas::new(1.0, 1.0);
    probe.set_gpu(true);
    if probe.engine_kind() != EngineKind::Gpu {
        assert!(
            !has_metal_device(),
            "Metal has a device, but the GPU engine did not start on it"
        );
        return Ok(());
    }
    // Only on Metal: the defect has not been seen on Vulkan.
    if !has_metal_device() {
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

    // Measured on M156: 25 pixels, each one level apart.
    let moved = parted(&both, &alone, 1);
    assert!(
        !moved.is_empty() && moved.iter().all(|&level| level == 1),
        "the caustic beside the blob now reads {} pixels apart from the \
         caustic alone, by up to {} levels: the defect this pins has changed, \
         so look again at the UPSTREAM note in scripts/skia-source.mjs",
        moved.len(),
        moved.iter().max().copied().unwrap_or(0),
    );
    Ok(())
}
