//! A decoded image drawn onto a CPU canvas composites with each step rounded
//! as the Canvas compositing model rounds it, wherever it lands: each
//! premultiplied channel scaled by the global alpha and rounded to a byte,
//! then composited `source-over` onto the page and rounded again.
//!
//! Skia decodes an encoded image into its N32 colour type, the page's own
//! byte order, and draws an N32 image onto an N32 page at a whole-pixel
//! offset, unscaled, through its sprite rows (`SkBlendARGB32` and
//! `blit_row_s32a_opaque`) rather than its raster pipeline. Those rows divide
//! by 256 where the composite divides by 255: at full alpha on x86, and below
//! it on every CPU, a level apart from the exact bytes for a large share of
//! pixels (#828).
//!
//! The reference is that arithmetic written out over the image's own
//! premultiplied bytes, read back from the image drawn onto a transparent
//! canvas at full alpha, which every route copies exactly.
//!
//! Below full alpha the image is scaled by the paint's alpha in Skia's
//! eight-bit raster pipeline, whose `div255` is exact on arm64 and is
//! `(v + 255) >> 8` elsewhere, so the alphas below full are held to the
//! exact composite only on arm64: see [`EXACT_BELOW_FULL`].

use meo_skia_canvas::prelude::*;
use skia_safe::{
    AlphaType, ColorSpace, ColorType, Data, EncodedImageFormat, ImageInfo,
    images,
};

/// Whether Skia's CPU scales a paint's colour by its alpha exactly here --
/// see the module's documentation -- so that every alpha, and not full
/// alpha alone, lands the exact composite.
const EXACT_BELOW_FULL: bool = cfg!(target_arch = "aarch64");

/// Each side of the image and the page.
const SIDE: u32 = 256;

/// Bytes in a pixel of an eight-bit RGBA raster.
const BYTES_PER_PIXEL: usize = 4;

/// `n / 255` rounded to the nearest integer. 255 is odd, so no `n` falls
/// halfway and the direction of a tie never matters.
fn over_255(n: u32) -> u32 {
    (2 * n + 255) / 510
}

/// A PNG of [`SIDE`] by [`SIDE`] unpremultiplied pixels: alpha by column,
/// colour by row, so that every alpha meets many colours.
fn png() -> Vec<u8> {
    let pixels: Vec<u8> = (0..SIDE * SIDE)
        .flat_map(|i| {
            let (row, column) = ((i / SIDE) as u8, (i % SIDE) as u8);
            [row, u8::MAX - row, row.wrapping_mul(7), column]
        })
        .collect();
    let info = ImageInfo::new(
        (SIDE as i32, SIDE as i32),
        ColorType::RGBA8888,
        AlphaType::Unpremul,
        ColorSpace::new_srgb(),
    );
    images::raster_from_data(
        &info,
        Data::new_copy(&pixels),
        SIDE as usize * BYTES_PER_PIXEL,
    )
    .expect("a raster image")
    .encode(None, EncodedImageFormat::PNG, None)
    .expect("a PNG")
    .as_bytes()
    .to_vec()
}

/// The opaque backdrop, RGBA, its channels varying across both axes so that
/// no channel sits at 0 or 255 for long: where they do, the sprite rows land
/// the exact composite and cannot be told from it.
fn backdrop() -> Vec<u8> {
    (0..SIDE * SIDE)
        .flat_map(|i| {
            let d = ((i / SIDE) * 5 + (i % SIDE) * 3) as u8;
            [d, u8::MAX - d, d / 2 + 40, u8::MAX]
        })
        .collect()
}

/// `canvas`'s pixels, premultiplied RGBA.
fn read(canvas: &mut Canvas) -> Vec<u8> {
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
        .expect("pixels")
        .pixels()
        .to_vec()
}

/// A canvas of [`SIDE`] by [`SIDE`] on the CPU engine, holding `bytes` where
/// given.
fn page(bytes: Option<&[u8]>) -> Canvas {
    let mut canvas = Canvas::new(SIDE as f32, SIDE as f32);
    canvas.set_gpu(false);
    if let Some(bytes) = bytes {
        let data = ImageData::from_pixels(
            SIDE,
            SIDE,
            PixelExportOptions::default(),
            bytes.to_vec(),
        )
        .expect("image data");
        canvas
            .context()
            .put_image_data(&data, 0.0, 0.0)
            .expect("a put");
    }
    canvas
}

/// What the page holds after `source`, premultiplied, is drawn over
/// [`backdrop`] at the alpha byte `alpha`: each channel scaled and rounded,
/// then composited and rounded.
fn expected(source: &[u8], alpha: u8) -> Vec<u8> {
    let alpha = u32::from(alpha);
    source
        .chunks(BYTES_PER_PIXEL)
        .zip(backdrop().chunks(BYTES_PER_PIXEL))
        .flat_map(|(s, d)| {
            let scaled =
                [0, 1, 2, 3].map(|c| over_255(u32::from(s[c]) * alpha));
            let inverse = u32::from(u8::MAX) - scaled[3];
            [0, 1, 2, 3].map(|c| {
                (scaled[c] + over_255(u32::from(d[c]) * inverse)) as u8
            })
        })
        .collect()
}

#[test]
fn a_decoded_image_composites_with_each_step_rounded() {
    let image = Image::from_encoded(&png()).expect("the PNG decodes");
    let mut alone = page(None);
    alone.context().draw_image(&image, 0.0, 0.0);
    let source = read(&mut alone);
    assert!(
        source
            .chunks(BYTES_PER_PIXEL)
            .any(|p| p[3] > 0 && p[3] < u8::MAX),
        "the image holds translucent pixels"
    );

    let back = backdrop();
    let mut wrong = Vec::new();
    let alphas = match EXACT_BELOW_FULL {
        true => 1..=u8::MAX,
        false => u8::MAX..=u8::MAX,
    };
    let tested = alphas.clone().count();
    for alpha in alphas {
        let mut drawn = page(Some(&back));
        let ctx = drawn.context();
        ctx.set_global_alpha(f32::from(alpha) / f32::from(u8::MAX));
        ctx.draw_image(&image, 0.0, 0.0);
        let drawn = read(&mut drawn);
        let apart: Vec<u8> = drawn
            .iter()
            .zip(expected(&source, alpha))
            .filter(|(a, b)| *a != b)
            .map(|(a, b)| a.abs_diff(b))
            .collect();
        if !apart.is_empty() {
            wrong.push((alpha, apart.len(), apart.into_iter().max()));
        }
    }
    assert!(
        wrong.is_empty(),
        "{} of {tested} alphas apart from the exact composite (alpha, bytes, \
         most), first: {:?}, last: {:?}",
        wrong.len(),
        &wrong[..wrong.len().min(8)],
        wrong.last()
    );
}
