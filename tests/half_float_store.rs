//! A half-float canvas stores each value as the nearest half, subnormals
//! included.
//!
//! Skia's raster pipeline narrows a float to a half in its `store_f16`
//! stage. NEON and the AVX2 and AVX-512 stages use the hardware conversion,
//! which rounds to nearest; the portable stage an x86 build is compiled with
//! truncates and flushes subnormal halves to zero. Which one an x86 machine
//! runs is chosen by `SkGraphics::Init`, so these values tell the two apart:
//! a tie, a value three quarters of the way to the next half, and a
//! subnormal. An x86 CPU without AVX2 and F16C keeps the portable stage even
//! then, and the half-float test skips there, saying why.

use meo_skia_canvas::prelude::*;
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
use std::arch::is_x86_feature_detected;

/// Whether Skia has a stage on this CPU that rounds a half to nearest: on
/// x86 the AVX2 one, which needs AVX2 and F16C. NEON always does.
///
/// Detected, not assumed: an x86 machine without both keeps the portable
/// stage even once Skia is started, and the tests skip there rather than
/// pass, saying why. libtest has no skip, so it is a message and an early
/// return.
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
fn has_rounding_stage() -> bool {
    is_x86_feature_detected!("avx2") && is_x86_feature_detected!("f16c")
}

/// Whether Skia has a stage on this CPU that rounds a half to nearest; on
/// ARM, NEON's, always.
#[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
fn has_rounding_stage() -> bool {
    true
}

/// Why a test skipped on a CPU without [`has_rounding_stage`].
const NO_ROUNDING_STAGE: &str = "skipped: this x86 CPU lacks AVX2 or F16C, so Skia keeps \
     its portable half-float stage, which truncates (see the UPSTREAM note on \
     `start_skia`)";

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
    if !has_rounding_stage() {
        eprintln!("{NO_ROUNDING_STAGE}");
        return Ok(());
    }
    assert_eq!(
        stored(PixelDepth::F16)?,
        [TIE_AS_HALF, SUBNORMAL, ABOVE_HALF_AS_HALF, 1.0],
        "a half-float store truncated or flushed a subnormal: Skia's \
         CPU-specific raster stages were not selected"
    );
    Ok(())
}
