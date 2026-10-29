//! A half-float canvas stores each value as the nearest half, subnormals
//! included.
//!
//! Skia's raster pipeline narrows a float to a half in its `store_f16`
//! stage. NEON and the AVX2 and AVX-512 stages use the hardware conversion,
//! which rounds to nearest; the portable stage an x86 build is compiled with
//! truncates and flushes subnormal halves to zero. Which one an x86 machine
//! runs is chosen by `SkGraphics::Init`, so these values tell the two apart:
//! a tie, a value three quarters of the way to the next half, and a
//! subnormal.

use meo_skia_canvas::prelude::*;

/// Three quarters of a unit in the last place above one half. The nearest
/// half is the next one up, `0.5 + 2^-11`; truncation keeps `0.5`.
const ABOVE_HALF: f32 = 0.5 + 0.75 / 2048.0;

/// The half nearest [`ABOVE_HALF`]: halves in `[0.5, 1)` are `2^-11` apart.
const ABOVE_HALF_AS_HALF: f32 = 0.5 + 1.0 / 2048.0;

/// `2^-20`, a subnormal half exactly -- sixteen of the smallest, `2^-24` --
/// which a store that flushes subnormals turns into zero.
const SUBNORMAL: f32 = 1.0 / 1_048_576.0;

/// Exactly between the halves `0.5 + 2^-11`, whose last bit is odd, and
/// `0.5 + 2 * 2^-11`, whose last bit is even. Round-to-nearest-even takes the
/// even one, above; truncation takes the odd one, below.
const TIE: f32 = 0.5 + 1.5 / 2048.0;

/// The half [`TIE`] rounds to under round-to-nearest-even.
const TIE_AS_HALF: f32 = 0.5 + 2.0 / 2048.0;

/// The pixel a 1x1 canvas of `depth` holds after [`TIE`], [`SUBNORMAL`] and
/// [`ABOVE_HALF`] are written to it as float pixels, read back as floats.
///
/// Written rather than filled: `putImageData` lands the pixels as given, so
/// the only narrowing on the way in is the canvas's own store, where a fill's
/// colour would first be converted into the canvas's space.
fn stored(depth: PixelDepth) -> Result<[f32; 4], Error> {
    let mut canvas = Canvas::with_options(
        1.0,
        1.0,
        CanvasOptions {
            color_space: PixelColorSpace::SrgbLinear,
            color_type: depth,
            ..CanvasOptions::default()
        },
    )?;
    let floats = PixelExportOptions {
        color_space: PixelColorSpace::SrgbLinear,
        depth: PixelDepth::F32,
        premultiplied: true,
        ..PixelExportOptions::default()
    };
    let written: Vec<u8> = [TIE, SUBNORMAL, ABOVE_HALF, 1.0]
        .iter()
        .flat_map(|v| v.to_ne_bytes())
        .collect();
    let data = ImageData::from_pixels(1, 1, floats, written)?;
    canvas.context().put_image_data(&data, 0.0, 0.0)?;
    let export = ImageDataExport {
        pixels: floats,
        ..ImageDataExport::default()
    };
    let read = canvas.to_image_data(None, &export)?;
    let p = read.pixels();
    let channel = |i: usize| {
        f32::from_ne_bytes([p[i * 4], p[i * 4 + 1], p[i * 4 + 2], p[i * 4 + 3]])
    };
    Ok([channel(0), channel(1), channel(2), channel(3)])
}

#[test]
fn a_float_canvas_holds_the_values_it_was_given() -> Result<(), Error> {
    assert_eq!(stored(PixelDepth::F32)?, [TIE, SUBNORMAL, ABOVE_HALF, 1.0]);
    Ok(())
}

#[test]
fn a_half_float_canvas_stores_the_nearest_half() -> Result<(), Error> {
    assert_eq!(
        stored(PixelDepth::F16)?,
        [TIE_AS_HALF, SUBNORMAL, ABOVE_HALF_AS_HALF, 1.0],
        "a half-float store truncated or flushed a subnormal: Skia's \
         CPU-specific raster stages were not selected"
    );
    Ok(())
}
