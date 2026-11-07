//! The CPU raster's blend modes that divide -- `color-dodge`, `color-burn`,
//! `hue`, `saturation`, `color` and `luminosity` -- draw the bytes their
//! formulas give in IEEE single precision with exact division, on every
//! machine.
//!
//! Skia's raster pipeline divides in these stages through `rcp_fast`, an
//! estimate whose precision is that of the instruction a SIMD level has:
//! `_mm256_rcp_ps` on AVX2, `_mm512_rcp14_ps` on AVX-512, NEON's estimate and
//! one refinement step on arm64. `SkGraphics::Init` moves an x86 machine onto
//! its widest level, so the same page drew differently on an AMD EPYC with
//! AVX-512 and one without (#642). `containers/skia-exact-blend-division.patch`
//! divides with `/`, which IEEE 754 rounds correctly at every level, so each
//! stage is the same sequence of correctly rounded operations everywhere.
//!
//! The reference here is that sequence, written out: the pipeline's own load,
//! formula and store, operation by operation in `f32`, a `mad` fused where the
//! stage fuses it. It is held to the formula in `f64` too, so a reference that
//! drifted from the formula would show.
//!
//! A crate build's Skia does not carry the patch, and
//! `cfg(meo_exact_blend_division)`, from `build.rs`, says whether this build
//! compiled it. Without it the test holds the build to bytes off the exact
//! quotient instead, so the day crates.io's Skia divides exactly, the test
//! says so.

use meo_skia_canvas::prelude::*;
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
use std::arch::is_x86_feature_detected;

/// Whether this x86 machine has what Skia's x86-64-v3 stages need
/// (`SkX64::ML3` in `src/core/SkCpu.h`): AVX2, BMI1, BMI2, F16C and FMA.
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
fn has_ml3() -> bool {
    is_x86_feature_detected!("avx2")
        && is_x86_feature_detected!("bmi1")
        && is_x86_feature_detected!("bmi2")
        && is_x86_feature_detected!("f16c")
        && is_x86_feature_detected!("fma")
}

/// Whether this x86 machine has what Skia's x86-64-v4 stages need
/// (`SkX64::ML4`): AVX-512 F, DQ, CD, BW and VL. Skia takes them only on a
/// machine that has the v3 stages' features too.
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
fn has_ml4() -> bool {
    has_ml3()
        && is_x86_feature_detected!("avx512f")
        && is_x86_feature_detected!("avx512dq")
        && is_x86_feature_detected!("avx512cd")
        && is_x86_feature_detected!("avx512bw")
        && is_x86_feature_detected!("avx512vl")
}

/// Whether the stages this machine runs fuse an explicit `mad`: on x86 the
/// v3 and v4 stages' `fmadd` does. The SSE stages Skia keeps on an x86
/// machine without the v3 features compute `a + f * m` in two roundings.
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
fn fuses_mad() -> bool {
    has_ml3()
}

/// Whether the stages this machine runs fuse an explicit `mad`: on arm64
/// NEON's `vfmaq_f32` does.
#[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
fn fuses_mad() -> bool {
    true
}

/// The SIMD level `SkGraphics::Init` gives this x86 machine's raster
/// pipeline (`SkOpts::init`): the v4 (AVX-512) or v3 (AVX2) stages, or the
/// SSE ones built in.
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
fn simd_level() -> &'static str {
    match (has_ml4(), has_ml3()) {
        (true, _) => "x86-64-v4, AVX-512",
        (false, true) => "x86-64-v3, AVX2",
        (false, false) => "x86 SSE",
    }
}

/// The SIMD level this arm64 machine's raster pipeline runs: NEON.
#[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
fn simd_level() -> &'static str {
    "NEON"
}

/// `a + f * m` as the stage computes `mad(f, m, a)`.
fn mad(f: f32, m: f32, a: f32) -> f32 {
    match fuses_mad() {
        true => f.mul_add(m, a),
        false => a + f * m,
    }
}

/// `a - f * m` as the stage computes `nmad(f, m, a)`.
fn nmad(f: f32, m: f32, a: f32) -> f32 {
    match fuses_mad() {
        true => (-f).mul_add(m, a),
        false => a - f * m,
    }
}

/// A byte as the pipeline loads it: `from_8888`, the byte times the single
/// nearest `1/255`.
fn load(byte: u8) -> f32 {
    f32::from(byte) * (1.0f32 / 255.0)
}

/// A value as `store_8888` writes it: `to_unorm`, `mad(v, 255, 0)` clamped
/// to `[0, 255]` and rounded to nearest, ties to even.
fn store(v: f32) -> u8 {
    mad(v, 255.0, 0.0).clamp(0.0, 255.0).round_ties_even() as u8
}

fn inv(x: f32) -> f32 {
    1.0 - x
}

/// A separable mode's channel: `s` over `d`, alphas `sa` and `da`.
type Channel = fn(f32, f32, f32, f32) -> f32;

/// `BLEND_MODE(colordodge)`, dividing exactly.
fn dodge(s: f32, d: f32, sa: f32, da: f32) -> f32 {
    if d == 0.0 {
        s * inv(da)
    } else if s == sa {
        s + d * inv(sa)
    } else {
        sa * da.min((d * sa) / (sa - s)) + s * inv(da) + d * inv(sa)
    }
}

/// `BLEND_MODE(colorburn)`, dividing exactly.
fn burn(s: f32, d: f32, sa: f32, da: f32) -> f32 {
    if d == da {
        d + s * inv(da)
    } else if s == 0.0 {
        d * inv(sa)
    } else {
        sa * (da - da.min((da - d) * sa / s)) + s * inv(da) + d * inv(sa)
    }
}

/// The same channels in `f64`, the formula's value without single-precision
/// rounding.
fn dodge_f64(s: f64, d: f64, sa: f64, da: f64) -> f64 {
    if d == 0.0 {
        s * (1.0 - da)
    } else if s == sa {
        s + d * (1.0 - sa)
    } else {
        sa * da.min(d * sa / (sa - s)) + s * (1.0 - da) + d * (1.0 - sa)
    }
}

fn burn_f64(s: f64, d: f64, sa: f64, da: f64) -> f64 {
    if d == da {
        d + s * (1.0 - da)
    } else if s == 0.0 {
        d * (1.0 - sa)
    } else {
        sa * (da - da.min((da - d) * sa / s)) + s * (1.0 - da) + d * (1.0 - sa)
    }
}

/// `lum`, the stages' luminance.
fn lum([r, g, b]: [f32; 3]) -> f32 {
    mad(r, 0.30, mad(g, 0.59, b * 0.11))
}

/// `sat`, the spread of the channels.
fn sat([r, g, b]: [f32; 3]) -> f32 {
    r.max(g.max(b)) - r.min(g.min(b))
}

/// `set_sat`, dividing exactly.
fn set_sat(c: [f32; 3], s: f32) -> [f32; 3] {
    let mn = c[0].min(c[1].min(c[2]));
    let mx = c[0].max(c[1].max(c[2]));
    let spread = mx - mn;
    let s = if spread == 0.0 { 0.0 } else { s / spread };
    c.map(|v| (v - mn) * s)
}

/// `set_lum`.
fn set_lum(c: [f32; 3], l: f32) -> [f32; 3] {
    let diff = l - lum(c);
    c.map(|v| v + diff)
}

/// `clip_color`, dividing exactly.
fn clip_color(c: [f32; 3], a: f32) -> [f32; 3] {
    let mn = c[0].min(c[1].min(c[2]));
    let mx = c[0].max(c[1].max(c[2]));
    let l = lum(c);
    let low = mn < 0.0 && l != mn;
    let high = mx > a && l != mx;
    let mn_scale = l / (l - mn);
    let mx_scale = (a - l) / (mx - l);
    c.map(|v| {
        let v = if low { mad(mn_scale, v - l, l) } else { v };
        let v = if high { mad(mx_scale, v - l, l) } else { v };
        v.max(0.0)
    })
}

/// A non-separable stage's colour: `s` over `d` with alphas `a` and `da`,
/// from the blended `R`, `G`, `B`.
fn non_separable(
    s: [f32; 3],
    d: [f32; 3],
    a: f32,
    da: f32,
    blended: [f32; 3],
) -> [f32; 4] {
    let c = |i: usize| mad(s[i], inv(da), mad(d[i], inv(a), blended[i]));
    [c(0), c(1), c(2), a + nmad(a, da, da)]
}

/// The stage of each mode the test draws, from source and backdrop colours
/// and alphas, all premultiplied, to the premultiplied result.
fn stage(mode: BlendMode, s: [f32; 4], d: [f32; 4]) -> [f32; 4] {
    let (sc, dc) = ([s[0], s[1], s[2]], [d[0], d[1], d[2]]);
    let (a, da) = (s[3], d[3]);
    let separable = |channel: Channel| {
        let c = |i: usize| channel(sc[i], dc[i], a, da);
        [c(0), c(1), c(2), mad(da, inv(a), a)]
    };
    match mode {
        BlendMode::ColorDodge => separable(dodge),
        BlendMode::ColorBurn => separable(burn),
        BlendMode::Hue => {
            let rgb = sc.map(|v| v * a);
            let rgb = set_sat(rgb, sat(dc) * a);
            let rgb = set_lum(rgb, lum(dc) * a);
            non_separable(sc, dc, a, da, clip_color(rgb, a * da))
        }
        BlendMode::Saturation => {
            let rgb = dc.map(|v| v * a);
            let rgb = set_sat(rgb, sat(sc) * da);
            let rgb = set_lum(rgb, lum(dc) * a);
            non_separable(sc, dc, a, da, clip_color(rgb, a * da))
        }
        BlendMode::Color => {
            let rgb = sc.map(|v| v * da);
            let rgb = set_lum(rgb, lum(dc) * a);
            non_separable(sc, dc, a, da, clip_color(rgb, a * da))
        }
        BlendMode::Luminosity => {
            let rgb = dc.map(|v| v * a);
            let rgb = set_lum(rgb, lum(sc) * da);
            non_separable(sc, dc, a, da, clip_color(rgb, a * da))
        }
        _ => unreachable!("only the dividing modes are drawn"),
    }
}

/// The modes whose stages divide.
const DIVIDING: [BlendMode; 6] = [
    BlendMode::ColorDodge,
    BlendMode::ColorBurn,
    BlendMode::Hue,
    BlendMode::Saturation,
    BlendMode::Color,
    BlendMode::Luminosity,
];

/// A premultiplied pixel: three channels at most `alpha`, and `alpha`.
type Pixel = [u8; 4];

/// The pixels of a `w` by `h` page, row by row.
struct Page {
    w: u32,
    h: u32,
    pixels: Vec<Pixel>,
}

/// Two pages the same size, a source drawn over a backdrop.
struct Pair {
    name: String,
    source: Page,
    backdrop: Page,
}

/// Every channel value against every other at alphas `sa` over `da`: a
/// source pixel's channels are its row, a backdrop pixel's its column, each
/// the same in red, green and blue.
fn ramps(sa: u8, da: u8) -> Pair {
    let (w, h) = (u32::from(da) + 1, u32::from(sa) + 1);
    let page = |value: &dyn Fn(u32, u32) -> u8, alpha: u8| Page {
        w,
        h,
        pixels: (0..h)
            .flat_map(|y| (0..w).map(move |x| (x, y)))
            .map(|(x, y)| {
                let v = value(x, y);
                [v, v, v, alpha]
            })
            .collect(),
    };
    Pair {
        name: format!("ramps at alphas {sa} over {da}"),
        source: page(&|_, y| y as u8, sa),
        backdrop: page(&|x, _| x as u8, da),
    }
}

/// 128 by 128 pixels of unrelated channels and alphas, from a fixed
/// sequence, for the modes that mix channels.
fn mixed(seed: u32) -> Pair {
    let mut state = seed;
    let mut next = move || {
        // Numerical Recipes' 32-bit linear congruential generator.
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (state >> 24) as u8
    };
    let mut page = || Page {
        w: 128,
        h: 128,
        pixels: (0..128 * 128)
            .map(|_| {
                let alpha = next();
                let mut channel =
                    || (u16::from(next()) * u16::from(alpha) / 255) as u8;
                [channel(), channel(), channel(), alpha]
            })
            .collect(),
    };
    Pair {
        name: format!("mixed pixels from seed {seed}"),
        source: page(),
        backdrop: page(),
    }
}

/// `page` as a premultiplied `ImageData`.
fn image_data(page: &Page) -> ImageData {
    ImageData::from_pixels(
        page.w,
        page.h,
        PixelExportOptions {
            premultiplied: true,
            ..PixelExportOptions::default()
        },
        page.pixels.concat(),
    )
    .expect("a premultiplied buffer")
}

/// The source drawn over the backdrop under `mode` on an eight-bit sRGB CPU
/// canvas, read back premultiplied.
fn drawn(pair: &Pair, mode: BlendMode) -> Vec<Pixel> {
    let (w, h) = (pair.source.w as f32, pair.source.h as f32);
    let mut canvas = Canvas::with_options(
        w,
        h,
        CanvasOptions {
            gpu: false,
            ..CanvasOptions::default()
        },
    )
    .expect("a canvas");
    let ctx = canvas.context();
    ctx.put_image_data(&image_data(&pair.backdrop), 0.0, 0.0)
        .expect("the backdrop");
    ctx.set_global_composite_operation(mode);
    let source =
        Image::try_from(&image_data(&pair.source)).expect("the source");
    ctx.draw_image(&source, 0.0, 0.0);
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
        .as_chunks::<4>()
        .0
        .to_vec()
}

/// The pairs drawn: the ramps at four pairs of alphas, and mixed pixels.
fn pairs() -> Vec<Pair> {
    [(255, 255), (255, 128), (128, 255), (200, 90)]
        .into_iter()
        .map(|(sa, da)| ramps(sa, da))
        .chain([mixed(642), mixed(7)])
        .collect()
}

#[test]
fn the_dividing_blend_modes_draw_their_formulas_with_exact_division() {
    let mut wrong = Vec::new();
    let mut compared = 0usize;
    for pair in pairs() {
        for mode in DIVIDING {
            let page = drawn(&pair, mode);
            let expected =
                pair.source.pixels.iter().zip(&pair.backdrop.pixels).map(
                    |(s, d)| stage(mode, s.map(load), d.map(load)).map(store),
                );
            // The pixels off, and by how many levels the furthest byte is.
            let (off, levels) = page.iter().zip(expected).fold(
                (0usize, 0u8),
                |(off, levels), (got, want)| {
                    let gap = (0..4)
                        .map(|c| got[c].abs_diff(want[c]))
                        .max()
                        .unwrap_or(0);
                    (off + usize::from(gap > 0), levels.max(gap))
                },
            );
            compared += page.len();
            if off > 0 {
                wrong.push(format!(
                    "{:?} on {}: {off} of {} pixels, by up to {levels}",
                    mode,
                    pair.name,
                    page.len()
                ));
            }
        }
    }
    // Which level drew, for a log to say: a run proves its own level, and
    // the levels agree because each matches the same reference.
    println!("SIMD level: {}", simd_level());
    println!("off the exact division: {wrong:#?}");
    if cfg!(meo_exact_blend_division) {
        assert!(
            wrong.is_empty(),
            "pixels off the exact division, of {compared}: {wrong:#?}"
        );
    } else {
        assert!(
            !wrong.is_empty(),
            "crates.io's Skia drew all {compared} pixels as exact division \
             does; its blend stages no longer divide through an estimate"
        );
    }
}

#[test]
fn the_reference_is_the_formula_within_a_level() {
    // The single-precision reference against the same formula in f64,
    // stored the same way: a reference that drifted from the formula --
    // a wrong branch, a swapped operand -- would be off by more than the
    // rounding of single precision.
    let mut worst = 0u8;
    let mut moved = 0usize;
    for (sa, da) in [(255u8, 255u8), (255, 128), (128, 255), (200, 90)] {
        let (fa, fda) = (load(sa), load(da));
        for s in 0..=sa {
            for d in 0..=da {
                let (fs, fd) = (load(s), load(d));
                for (single, double) in [
                    (
                        dodge(fs, fd, fa, fda),
                        dodge_f64 as fn(_, _, _, _) -> f64,
                    ),
                    (burn(fs, fd, fa, fda), burn_f64),
                ] {
                    let exact = double(
                        f64::from(s) / 255.0,
                        f64::from(d) / 255.0,
                        f64::from(sa) / 255.0,
                        f64::from(da) / 255.0,
                    );
                    let exact =
                        (exact * 255.0).clamp(0.0, 255.0).round_ties_even()
                            as u8;
                    let gap = store(single).abs_diff(exact);
                    worst = worst.max(gap);
                    moved += usize::from(gap > 0);
                }
            }
        }
    }
    assert!(worst <= 1, "a byte {worst} levels from the f64 formula");
    println!("{moved} bytes a level from the f64 formula");
}
