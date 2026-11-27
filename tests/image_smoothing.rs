//! An image drawn larger with smoothing on stores each channel at the exact
//! bilinear interpolation of its source, rounded to the nearest level, on a
//! CPU canvas; and within the precision a texture unit weights a sample at,
//! on a GPU canvas.
//!
//! Chrome 159's software canvas stores the same interpolation truncated: a
//! canvas drawn at four times its size reads back the floor of the exact value
//! on every channel, a level under it wherever the fraction is half or more.
//! The entry in `docs/browser-differences.md` on scaling an image up says
//! why this library keeps the nearest.

use meo_skia_canvas::prelude::*;

/// The source's side, in pixels.
const SOURCE: u32 = 16;

/// The canvas each scaled draw lands on.
const PAGE: u32 = 64;

/// The destination rectangles, `[x, y, width, height]`: four times on the
/// pixel grid, 3.7 times off it, and two unequal factors, 2.5 across and 3.5
/// down.
const DESTINATIONS: [[f32; 4]; 3] = [
    [0.0, 0.0, 64.0, 64.0],
    [0.3, 0.6, 59.2, 59.2],
    [1.25, 2.0, 40.0, 56.0],
];

/// How far past half a level the CPU's stored channel may stand from the
/// exact interpolation: the raster pipeline works the sample's position and
/// weights in `f32`, which put 2 of 9,747 channels 0.501 of a level from it
/// at 3.7 times, against the `f64` reference here.
const CPU_SLACK_LEVELS: f64 = 1.0 / 128.0;

/// The bits of a texel's width a GPU's texture unit resolves a sample's
/// position to before weighting the texels around it: eight, which
/// Direct3D 11 requires and which the Metal figure in [`GPU_LEVELS`] fits.
/// Vulkan allows as few as four (`subTexelPrecisionBits`), and a device
/// reporting fewer than eight can stand further off than this bound.
const SUBTEXEL_BITS: i32 = 8;

/// How far a GPU canvas's stored channel may stand from the exact
/// interpolation: half a level of rounding, and what a weight resolved to
/// [`SUBTEXEL_BITS`] can move a sample on each of its two axes, half a step
/// of that resolution times a channel's whole range. Measured on Metal with
/// [`source`], 1.018 levels at 3.7 times. Not measured on llvmpipe.
const GPU_LEVELS: f64 =
    0.5 + 2.0 * 255.0 / (1u32 << (SUBTEXEL_BITS + 1)) as f64;

/// A source of opaque pixels from a fixed linear congruential sequence, so
/// that every channel interpolates between unrelated neighbours and an
/// unpremultiplied read moves nothing.
fn source() -> Vec<u8> {
    let mut state: u32 = 0x2545_f491;
    let mut next = || {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (state >> 24) as u8
    };
    (0..SOURCE * SOURCE)
        .flat_map(|_| [next(), next(), next(), u8::MAX])
        .collect()
}

/// `pixels` scaled into `rect` on a fresh canvas on `gpu`'s engine, read
/// back premultiplied; `None` where that engine is not available.
fn scaled(pixels: &[u8], rect: [f32; 4], gpu: bool) -> Option<Vec<u8>> {
    let mut canvas = Canvas::with_options(
        PAGE as f32,
        PAGE as f32,
        CanvasOptions {
            gpu,
            ..CanvasOptions::default()
        },
    )
    .expect("a canvas");
    if canvas.engine_kind()
        != if gpu {
            EngineKind::Gpu
        } else {
            EngineKind::Cpu
        }
    {
        return None;
    }
    let data = ImageData::from_pixels(
        SOURCE,
        SOURCE,
        PixelExportOptions {
            premultiplied: true,
            ..PixelExportOptions::default()
        },
        pixels.to_vec(),
    )
    .expect("a premultiplied buffer");
    let image = Image::try_from(&data).expect("the source");
    let ctx = canvas.context();
    let [x, y, w, h] = rect;
    ctx.draw_image_sized(&image, x, y, w, h);
    Some(
        canvas
            .to_image_data(
                None,
                &ImageDataExport {
                    pixels: PixelExportOptions {
                        premultiplied: true,
                        ..PixelExportOptions::default()
                    },
                    ..ImageDataExport::default()
                },
            )
            .expect("the page reads back")
            .pixels()
            .to_vec(),
    )
}

/// The exact bilinear interpolation of `pixels` at each destination pixel
/// whose centre lies at least a pixel inside `rect`, as `(index, value)`
/// for every colour channel: the centre mapped into the source, half a
/// pixel taken off to find the four texels around it, the source's edge
/// clamped, the weights its fractions.
fn exact(pixels: &[u8], rect: [f32; 4]) -> Vec<(usize, f64)> {
    let [dx, dy, dw, dh] = rect.map(f64::from);
    let side = f64::from(SOURCE);
    let texel = |x: i64, y: i64, channel: usize| {
        let last = i64::from(SOURCE) - 1;
        let (x, y) = (x.clamp(0, last), y.clamp(0, last));
        f64::from(pixels[(y * i64::from(SOURCE) + x) as usize * 4 + channel])
    };
    let mut out = vec![];
    for row in 0..PAGE {
        for column in 0..PAGE {
            let (cx, cy) = (f64::from(column) + 0.5, f64::from(row) + 0.5);
            if cx < dx + 1.0
                || cx >= dx + dw - 1.0
                || cy < dy + 1.0
                || cy >= dy + dh - 1.0
            {
                continue;
            }
            let u = (cx - dx) * side / dw - 0.5;
            let v = (cy - dy) * side / dh - 0.5;
            let (x0, y0) = (u.floor(), v.floor());
            let (fx, fy) = (u - x0, v - y0);
            let (x0, y0) = (x0 as i64, y0 as i64);
            for channel in 0..3 {
                let value = (1.0 - fx) * (1.0 - fy) * texel(x0, y0, channel)
                    + fx * (1.0 - fy) * texel(x0 + 1, y0, channel)
                    + (1.0 - fx) * fy * texel(x0, y0 + 1, channel)
                    + fx * fy * texel(x0 + 1, y0 + 1, channel);
                out.push(((row * PAGE + column) as usize * 4 + channel, value));
            }
        }
    }
    out
}

/// The channels of `stored` that stand further than `bound` from `exact`,
/// as `(index, stored, exact)`.
fn beyond(
    stored: &[u8],
    exact: &[(usize, f64)],
    bound: f64,
) -> Vec<(usize, u8, f64)> {
    exact
        .iter()
        .filter(|&&(at, value)| (f64::from(stored[at]) - value).abs() > bound)
        .map(|&(at, value)| (at, stored[at], value))
        .collect()
}

/// The CPU stores the exact interpolation rounded to the nearest level.
/// The check refuses Chrome's truncation of the same values, so that it
/// would see the CPU take it.
#[test]
fn a_scaled_up_image_stores_the_nearest_level_on_the_cpu() {
    let pixels = source();
    let bound = 0.5 + CPU_SLACK_LEVELS;
    for rect in DESTINATIONS {
        let exact = exact(&pixels, rect);
        assert!(exact.len() > 4000, "{rect:?}: {} channels", exact.len());
        let truncated: Vec<u8> = {
            let mut page = vec![0u8; (PAGE * PAGE * 4) as usize];
            for &(at, value) in &exact {
                page[at] = value.floor() as u8;
            }
            page
        };
        assert!(
            !beyond(&truncated, &exact, bound).is_empty(),
            "{rect:?}: the check passes the truncated values"
        );
        let stored = scaled(&pixels, rect, false).expect("a CPU canvas");
        let off = beyond(&stored, &exact, bound);
        assert!(
            off.is_empty(),
            "{rect:?}: {} of {} channels off the nearest level, first {:?}",
            off.len(),
            exact.len(),
            off.first()
        );
    }
}

/// A GPU canvas stores the interpolation to within [`GPU_LEVELS`].
#[test]
fn a_scaled_up_image_stores_within_the_samplers_precision_on_the_gpu() {
    let pixels = source();
    for rect in DESTINATIONS {
        let Some(stored) = scaled(&pixels, rect, true) else {
            eprintln!("skipped: no GPU");
            return;
        };
        let exact = exact(&pixels, rect);
        let off = beyond(&stored, &exact, GPU_LEVELS);
        assert!(
            off.is_empty(),
            "{rect:?}: {} of {} channels further off than {GPU_LEVELS}, first {:?}",
            off.len(),
            exact.len(),
            off.first()
        );
    }
}
