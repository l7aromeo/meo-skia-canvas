//! The exact Gaussian blur of pixel content, on the CPU.
//!
//! "Exact" has one definition here. A rasterised pixel is already the
//! average of its shape over the pixel's box, so the blur of pixel content
//! is the Gaussian *sampled* at whole-pixel offsets -- the box applied once,
//! by the rasteriser, rather than twice. The kernel is that sampled Gaussian
//! normalised over a reach of [`REACH_SIGMAS`] sigmas, and the result is
//! rounded once, at the end. Two implementations of it give the same bytes
//! except on exact ties -- a value within float noise of a half-level, which
//! either may round either way -- and the tests hold them to that against an
//! f64 reference.
//!
//! The blur is separable: a 1-D pass along every row, a transpose, the same
//! pass along every row of the transpose (the columns), and a transpose
//! back. Each axis takes its own sigma, since a transform can scale x and y
//! apart. A pass is direct -- the taps summed -- below [`FFT_FROM_SIGMA`],
//! and an FFT convolution from there, which costs the same whatever the
//! sigma.
//!
//! A pass works each line in place, from a copy of it its worker keeps, so
//! the image and its transpose are the only buffers of its size alive at
//! once: 32 bytes a pixel in floats.
//!
//! Float pixels are worked in f64 a line at a time, and bytes in f32 -- see
//! [`Lane`]. Unrounded, a float result has to be the f32 nearest the exact
//! sum, which f32 arithmetic misses by a few ulps; a byte is rounded to a
//! level, which f32 holds to about 1e-4.

use rayon::prelude::*;
use rustfft::{Fft, FftNum, FftPlanner, num_complex::Complex};
use std::{cell::RefCell, collections::HashMap, f64::consts::TAU, sync::Arc};

/// How many sigmas the kernel reaches either side of its centre.
///
/// Measured against a reference reaching 8 sigmas, on noise, hard edges and
/// a single pixel: at 4 the renormalised tail moved values by up to 0.014 of
/// a level, which changed bytes; at 5 by 2.6e-5, which changed none; at 6 by
/// nothing measurable. Six it is, so that truncation is never what decides a
/// byte.
pub(crate) const REACH_SIGMAS: f64 = 6.0;

/// The sigma from which an axis is convolved through the FFT rather than
/// tap by tap.
///
/// Direct costs grow with the sigma and the FFT's do not. On an 800x600
/// premultiplied RGBA blur on 14 threads, paired per round over 7 rounds,
/// the FFT took 1.21 times direct's time at a sigma of 4 and 0.44 times at
/// 12; direct's time is close to linear in the sigma, which puts the two
/// level near 5. The crossover moves with the image's size and the thread
/// count, so this is the measured figure at one size rather than a law.
pub(crate) const FFT_FROM_SIGMA: f32 = 5.0;

/// The half-kernel `w_0..=w_r`: the Gaussian sampled at whole-pixel offsets,
/// normalised over its whole reach, `ceil(REACH_SIGMAS * sigma)` either
/// side, but holding no more than `limit + 1` weights: a line of `limit + 1`
/// pixels never reads a tap further out than that, and a blur as wide as a
/// CSS length can make one -- `blur(calc(1px * infinity))` is 3.4e38 -- must
/// not allocate its whole reach. In f64, so the weights are right to the
/// last bit of the f32 they become.
pub(crate) fn half_kernel(sigma: f32, limit: usize) -> Vec<f64> {
    let sigma = f64::from(sigma);
    let r = (REACH_SIGMAS * sigma).ceil().min(limit as f64) as usize;
    let total = gaussian_sum(sigma);
    (0..=r)
        .map(|d| (-((d * d) as f64) / (2.0 * sigma * sigma)).exp() / total)
        .collect()
}

/// The widest reach, in taps either side, whose weights [`gaussian_sum`]
/// adds up one by one. Past it the closed form is used, which is within
/// 5.5e-13 of the sum at this reach and nearer beyond.
const SUMMED_REACH: f64 = 65_536.0;

/// `erf(REACH_SIGMAS / sqrt(2))`: the share of a Gaussian within
/// [`REACH_SIGMAS`] sigmas of its centre, which the closed form in
/// [`gaussian_sum`] scales by.
const SHARE_WITHIN_REACH: f64 = 0.999_999_998_026_824_7;

/// The sum of the Gaussian of `sigma` sampled at every whole-pixel offset
/// within its reach of `ceil(REACH_SIGMAS * sigma)`: what the weights are
/// normalised by. Summed tap by tap up to [`SUMMED_REACH`]; past it, as
/// `sigma * sqrt(2 pi)` times [`SHARE_WITHIN_REACH`], the integral the sum
/// is a midpoint rule for.
pub(crate) fn gaussian_sum(sigma: f64) -> f64 {
    let reach = (REACH_SIGMAS * sigma).ceil();
    if reach > SUMMED_REACH {
        return sigma * TAU.sqrt() * SHARE_WITHIN_REACH;
    }
    let reach = reach as i64;
    (-reach..=reach)
        .map(|d| (-((d * d) as f64) / (2.0 * sigma * sigma)).exp())
        .sum()
}

/// Blurs `pixels`, premultiplied RGBA floats `width` by `height`, by
/// `sigma_x` along rows and `sigma_y` along columns, reading outside the
/// image as transparent. Returns the blurred floats, unrounded, in the
/// buffer `pixels` came in.
///
/// Each line is worked in f64 and narrowed to f32 once, so that a value is
/// the f32 nearest the exact sum: inside its reach a constant comes back
/// to the bit, as a flat colour on a float page has to. Summed in f32, the
/// rounding of every tap reached 23 ulps at a sigma of 20. Summed in f32
/// with each product and sum compensated, the direct path was exact too
/// but slower than f64 at every sigma timed, and the FFT has no such form.
///
/// A sigma that is zero, negative or not a number leaves that axis alone,
/// and an infinite one spreads it to nothing.
pub(crate) fn blur(
    pixels: Vec<f32>,
    width: usize,
    height: usize,
    sigma_x: f32,
    sigma_y: f32,
) -> Vec<f32> {
    blur_in::<f64>(pixels, width, height, sigma_x, sigma_y)
}

/// Blurs `pixels`, premultiplied RGBA bytes, as [`blur`] does but in f32,
/// scales the result by `gain` and rounds once at the end: a blurred draw
/// composited at an alpha takes its alpha here, so that it is not rounded
/// twice. f32 holds a level to within about 1e-4, so it decides no byte
/// that f64 would decide otherwise but on exact ties, at about twice the
/// speed. `pixels` is let go as soon as it is in floats.
pub(crate) fn blur_bytes(
    pixels: Vec<u8>,
    width: usize,
    height: usize,
    sigma_x: f32,
    sigma_y: f32,
    gain: f32,
) -> Vec<u8> {
    let floats: Vec<f32> = pixels.par_iter().map(|&b| f32::from(b)).collect();
    drop(pixels);
    blur_in::<f32>(floats, width, height, sigma_x, sigma_y)
        .into_par_iter()
        // Each value is a weighted mean of bytes times a gain in [0, 1], so
        // it lies in [0, 255] up to float noise; the cast saturates what
        // noise puts outside.
        .map(|v| (v * gain + 0.5) as u8)
        .collect()
}

/// [`blur`], with each line worked in `L`.
fn blur_in<L: Lane>(
    mut pixels: Vec<f32>,
    width: usize,
    height: usize,
    sigma_x: f32,
    sigma_y: f32,
) -> Vec<f32> {
    pass::<L>(&mut pixels, width, height, sigma_x);
    let mut columns = vec![0.0; pixels.len()];
    transpose(&pixels, &mut columns, width, height);
    pass::<L>(&mut columns, height, width, sigma_y);
    transpose(&columns, &mut pixels, height, width);
    pixels
}

/// The arithmetic a line is worked in: f32 for bytes, f64 for floats.
trait Lane: FftNum {
    /// `v` in this arithmetic.
    fn of(v: f32) -> Self;
    /// A weight, worked out in f64, in this arithmetic.
    fn weight(w: f64) -> Self;
    /// This value as the f32 a pixel holds.
    fn narrow(self) -> f32;
    /// Runs `sum` over the line's accumulator, which ends in `out`: `out`
    /// itself in f32, and in f64 `scratch`, narrowed into `out` once
    /// `sum` is done.
    fn accumulate(
        out: &mut [f32],
        scratch: &mut Vec<Self>,
        sum: impl FnOnce(&mut [Self]),
    );
    /// The forward and inverse plans of `len` in this arithmetic, kept per
    /// worker.
    fn plans(len: usize) -> Plans<Self>;
}

impl Lane for f32 {
    fn of(v: f32) -> Self {
        v
    }

    fn weight(w: f64) -> Self {
        w as f32
    }

    fn narrow(self) -> f32 {
        self
    }

    fn accumulate(
        out: &mut [f32],
        _: &mut Vec<Self>,
        sum: impl FnOnce(&mut [Self]),
    ) {
        sum(out);
    }

    fn plans(len: usize) -> Plans<Self> {
        PLANS_F32.with(|plans| plan(&mut plans.borrow_mut(), len))
    }
}

impl Lane for f64 {
    fn of(v: f32) -> Self {
        f64::from(v)
    }

    fn weight(w: f64) -> Self {
        w
    }

    fn narrow(self) -> f32 {
        self as f32
    }

    fn accumulate(
        out: &mut [f32],
        scratch: &mut Vec<Self>,
        sum: impl FnOnce(&mut [Self]),
    ) {
        scratch.resize(out.len(), 0.0);
        sum(scratch);
        out.iter_mut()
            .zip(scratch.iter())
            .for_each(|(o, &v)| *o = v.narrow());
    }

    fn plans(len: usize) -> Plans<Self> {
        PLANS_F64.with(|plans| plan(&mut plans.borrow_mut(), len))
    }
}

/// One axis: convolves each of `lines` lines of `n` RGBA pixels, laid end
/// to end in `pixels`, with the kernel for `sigma`, in place, in `L`.
fn pass<L: Lane>(pixels: &mut [f32], n: usize, lines: usize, sigma: f32) {
    if sigma == f32::INFINITY {
        // Spread over an infinite reach, every value comes to nothing.
        pixels.fill(0.0);
        return;
    }
    if !(sigma.is_finite() && sigma > 0.0) || n == 0 || lines == 0 {
        return;
    }
    let k = half_kernel(sigma, n - 1);
    match method(sigma) {
        Method::Direct => direct::<L>(pixels, n, &k),
        Method::Fourier => fourier::<L>(pixels, n, &k),
    }
}

/// How an axis is convolved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Method {
    Direct,
    Fourier,
}

/// The method for `sigma`: both give the same bytes but for ties, so this is
/// a choice of speed alone, made at [`FFT_FROM_SIGMA`].
fn method(sigma: f32) -> Method {
    if sigma < FFT_FROM_SIGMA {
        Method::Direct
    } else {
        Method::Fourier
    }
}

/// Each line tap by tap: `out[x] = k0 in[x] + sum_d k_d (in[x-d] + in[x+d])`,
/// over a copy of the line padded with zeros so no tap leaves it, summed
/// into the line's accumulator -- see [`Lane::accumulate`]. Each tap is one
/// pass over the whole line, which the compiler vectorises.
fn direct<L: Lane>(pixels: &mut [f32], n: usize, k: &[f64]) {
    let k: Vec<L> = k.iter().map(|&w| L::weight(w)).collect();
    let r = k.len() - 1;
    let (stride, pad) = (n * 4, r * 4);
    let zero = L::of(0.0);
    pixels.par_chunks_mut(stride).for_each_init(
        || (vec![zero; stride + 2 * pad], Vec::new()),
        |(padded, scratch), out| {
            padded[pad..pad + stride]
                .iter_mut()
                .zip(out.iter())
                .for_each(|(p, &v)| *p = L::of(v));
            let padded = &*padded;
            let line = &padded[pad..pad + stride];
            L::accumulate(out, scratch, |acc| {
                acc.iter_mut().zip(line).for_each(|(o, &v)| *o = k[0] * v);
                for (d, &w) in k.iter().enumerate().skip(1) {
                    let left = &padded[pad - d * 4..][..stride];
                    let right = &padded[pad + d * 4..][..stride];
                    acc.iter_mut()
                        .zip(left.iter().zip(right))
                        .for_each(|(o, (&a, &b))| *o = *o + w * (a + b));
                }
            });
        },
    );
}

/// A length rustfft transforms quickly: the next 2^a 3^b 5^c 7^d at or above
/// `n`.
fn fast_len(n: usize) -> usize {
    (n..)
        .find(|&m| {
            let mut m = m;
            for p in [2, 3, 5, 7] {
                while m % p == 0 {
                    m /= p;
                }
            }
            m == 1
        })
        // SAFETY: powers of two are 2-smooth, so the search ends by the next
        // one above `n`.
        .expect("a smooth length exists")
}

/// A forward and an inverse plan of one length.
type Plans<L> = (Arc<dyn Fft<L>>, Arc<dyn Fft<L>>);

thread_local! {
    /// Plans by length, kept per worker: planning a length costs more than
    /// using it, and a page blurs at few lengths. One map per arithmetic.
    static PLANS_F32: RefCell<HashMap<usize, Plans<f32>>> = RefCell::new(HashMap::new());
    static PLANS_F64: RefCell<HashMap<usize, Plans<f64>>> = RefCell::new(HashMap::new());
}

/// The plans of `len` in `plans`, made there if they are not yet.
fn plan<L: FftNum>(
    plans: &mut HashMap<usize, Plans<L>>,
    len: usize,
) -> Plans<L> {
    plans
        .entry(len)
        .or_insert_with(|| {
            let mut planner = FftPlanner::new();
            (planner.plan_fft_forward(len), planner.plan_fft_inverse(len))
        })
        .clone()
}

/// Each line through the FFT, in `L`. The line is zero-padded to at least
/// `n + r`: the kernel is zero past `r`, so a tap that wraps round the
/// circular convolution lands outside `[0, n)` and the result is the linear
/// convolution, with transparent outside. The kernel is real and symmetric,
/// so its spectrum is real, and two real channels share one complex
/// transform: red with green, blue with alpha. The whole line is read into
/// those transforms before any of it is written back, so it is worked in
/// place.
fn fourier<L: Lane>(pixels: &mut [f32], n: usize, k: &[f64]) {
    let r = k.len() - 1;
    let len = fast_len(n + r);
    let spectrum: Vec<L> =
        spectrum(k, len).into_iter().map(L::weight).collect();
    let zero = Complex::new(L::of(0.0), L::of(0.0));
    pixels.par_chunks_mut(n * 4).for_each_init(
        || {
            let (forward, inverse) = L::plans(len);
            let scratch = forward
                .get_inplace_scratch_len()
                .max(inverse.get_inplace_scratch_len());
            (
                forward,
                inverse,
                vec![zero; len],
                vec![zero; len],
                vec![zero; scratch],
            )
        },
        |(forward, inverse, a, b, scratch), line| {
            for (i, [r, g, bl, al]) in
                line.as_chunks::<4>().0.iter().enumerate()
            {
                a[i] = Complex::new(L::of(*r), L::of(*g));
                b[i] = Complex::new(L::of(*bl), L::of(*al));
            }
            a[n..].fill(zero);
            b[n..].fill(zero);
            for buffer in [&mut *a, &mut *b] {
                forward.process_with_scratch(buffer, scratch);
                buffer
                    .iter_mut()
                    .zip(&spectrum)
                    .for_each(|(c, &s)| *c = *c * s);
                inverse.process_with_scratch(buffer, scratch);
            }
            for (i, px) in line.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                *px = [a[i].re, a[i].im, b[i].re, b[i].im].map(L::narrow);
            }
        },
    );
}

/// The kernel's spectrum at length `len`, real since the kernel is
/// symmetric, with the inverse transform's `1 / len` folded in, in f64.
fn spectrum(k: &[f64], len: usize) -> Vec<f64> {
    let forward = FftPlanner::<f64>::new().plan_fft_forward(len);
    let mut buffer = vec![Complex::new(0.0, 0.0); len];
    buffer[0].re = k[0];
    for (d, &w) in k.iter().enumerate().skip(1) {
        buffer[d].re = w;
        buffer[len - d].re = w;
    }
    forward.process(&mut buffer);
    buffer.iter().map(|c| c.re / len as f64).collect()
}

/// Pixels a transpose block spans along each side: 16 RGBA pixels of f32 is
/// 256 bytes per row read, four 64-byte cache lines on x86-64 and most Arm
/// cores, and two of the 128-byte lines of Apple silicon.
const BLOCK: usize = 16;

/// Writes the transpose of `src`, `height` rows of `width` RGBA pixels,
/// into `dst`. Each worker owns a block of destination rows, so it writes
/// one contiguous run, and reads the source a block at a time.
fn transpose(src: &[f32], dst: &mut [f32], width: usize, height: usize) {
    if width == 0 || height == 0 {
        return;
    }
    dst.par_chunks_mut(BLOCK * height * 4).enumerate().for_each(
        |(bx, rows)| {
            let x0 = bx * BLOCK;
            let xs = rows.len() / (height * 4);
            for y0 in (0..height).step_by(BLOCK) {
                for x in 0..xs {
                    for y in y0..(y0 + BLOCK).min(height) {
                        let s = (y * width + x0 + x) * 4;
                        let d = (x * height + y) * 4;
                        rows[d..d + 4].copy_from_slice(&src[s..s + 4]);
                    }
                }
            }
        },
    );
}

#[cfg(test)]
/// The blur in f64, tap by tap, reaching 8 sigmas so that its own
/// truncation is far below a level.
pub(crate) fn reference(
    src: &[u8],
    w: usize,
    h: usize,
    sx: f32,
    sy: f32,
) -> Vec<f64> {
    let wide = |sigma: f32| {
        let sigma = f64::from(sigma);
        let r = (8.0 * sigma).ceil() as i64;
        let g: Vec<f64> = (0..=r)
            .map(|d| (-((d * d) as f64) / (2.0 * sigma * sigma)).exp())
            .collect();
        let total = g[0] + 2.0 * g[1..].iter().sum::<f64>();
        (r, g.into_iter().map(|v| v / total).collect::<Vec<_>>())
    };
    let ((rx, kx), (ry, ky)) = (wide(sx), wide(sy));
    let at = |x: i64, y: i64, c: usize| ((y * w as i64 + x) * 4) as usize + c;
    let mut mid = vec![0f64; w * h * 4];
    for y in 0..h as i64 {
        for x in 0..w as i64 {
            for c in 0..4 {
                mid[at(x, y, c)] = (-rx..=rx)
                    .filter(|d| (0..w as i64).contains(&(x + d)))
                    .map(|d| {
                        kx[d.unsigned_abs() as usize]
                            * f64::from(src[at(x + d, y, c)])
                    })
                    .sum();
            }
        }
    }
    let mut out = vec![0f64; w * h * 4];
    for y in 0..h as i64 {
        for x in 0..w as i64 {
            for c in 0..4 {
                out[at(x, y, c)] = (-ry..=ry)
                    .filter(|d| (0..h as i64).contains(&(y + d)))
                    .map(|d| {
                        ky[d.unsigned_abs() as usize] * mid[at(x, y + d, c)]
                    })
                    .sum();
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lcg(seed: &mut u32) -> u32 {
        *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        *seed >> 8
    }

    /// Premultiplied scenes: noise with random alpha, an opaque block, one
    /// lit pixel, and stripes, which put many values exactly on a tie.
    fn scenes(w: usize, h: usize) -> Vec<Vec<u8>> {
        let mut s = 11;
        let noise = (0..w * h)
            .flat_map(|_| {
                let a = (lcg(&mut s) & 255) as u8;
                let c: [u8; 3] = std::array::from_fn(|_| {
                    ((lcg(&mut s) % 256) * u32::from(a) / 255) as u8
                });
                [c[0], c[1], c[2], a]
            })
            .collect();
        let block = (0..w * h)
            .flat_map(|i| {
                let (x, y) = (i % w, i / w);
                if x >= w / 4 && x <= 3 * w / 4 && y >= h / 3 && y <= 2 * h / 3
                {
                    [255, 128, 0, 255]
                } else {
                    [0; 4]
                }
            })
            .collect();
        let mut dot = vec![0u8; w * h * 4];
        let c = (h / 2 * w + w / 2) * 4;
        dot[c..c + 4].copy_from_slice(&[255; 4]);
        let stripes = (0..w * h)
            .flat_map(|i| {
                if (i % w).is_multiple_of(2) {
                    [255; 4]
                } else {
                    [0; 4]
                }
            })
            .collect();
        vec![noise, block, dot, stripes]
    }

    /// The bytes that differ from the rounded reference, and how far from a
    /// half-level the farthest of them lies in the reference.
    fn off_ties(sx: f32, sy: f32) -> (usize, f64) {
        let (mut off, mut farthest) = (0, 0f64);
        for (w, h) in [
            (1usize, 1usize),
            (5, 3),
            (37, 23),
            (64, 48),
            (200, 7),
            (7, 130),
        ] {
            for src in scenes(w, h) {
                let got = blur_bytes(src.clone(), w, h, sx, sy, 1.0);
                let want = reference(&src, w, h, sx, sy);
                for (&g, &e) in got.iter().zip(&want) {
                    if f64::from(g) != (e + 0.5).floor() {
                        off += 1;
                        farthest = farthest.max((e - e.floor() - 0.5).abs());
                    }
                }
            }
        }
        (off, farthest)
    }

    /// Float noise moves a value by well under this; a byte that differs from
    /// the reference further from a half-level than this is a wrong byte.
    const TIE: f64 = 1e-3;

    /// FNV-1a over `bytes`: a fingerprint of a blur's output that does not
    /// move with the standard library's hasher.
    fn fingerprint(bytes: &[u8]) -> u64 {
        bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| {
            (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
        })
    }

    #[test]
    fn eight_bit_pixels_blur_as_they_always_have() {
        // An eight-bit blur is rounded to a level, where f32 is exact enough,
        // and runs in f32 for its speed; only float pixels take the wider
        // arithmetic. Every scene at direct sigmas, fingerprinted as
        // `blur_bytes` gave it in f32 alone. Direct sums are the same IEEE
        // operations in the same order on every target; the FFT's are not,
        // rustfft picking NEON, AVX or scalar kernels, so its side is held by
        // which arithmetic ran rather than by its bytes.
        let got = [0.5f32, 1.0, 2.0, 3.3, 4.9]
            .into_iter()
            .flat_map(|sigma| {
                scenes(64, 48).into_iter().map(move |src| {
                    fingerprint(&blur_bytes(src, 64, 48, sigma, sigma, 1.0))
                })
            })
            .fold(0u64, |acc, h| acc.rotate_left(7) ^ h);
        assert_eq!(got, 0x132e_d974_e015_fad3, "{got:#x}");
    }

    #[test]
    fn a_constant_blurred_in_floats_is_itself_inside_its_reach() {
        // Every weight within the reach lands on the constant, and the
        // weights sum to one, so the exact blur there is the constant: in
        // floats, unrounded, it has to come back to the bit. Sigmas either
        // side of the FFT's crossover, on a line wide enough to have an
        // inside at each.
        let n = 800;
        let mut wrong = Vec::new();
        for sigma in [1.0f32, 2.0, 4.0, 8.0, 20.0, 60.0] {
            let reach = (REACH_SIGMAS * f64::from(sigma)).ceil() as usize;
            let mut worst = (0u32, 0f32);
            for i in 0..=40 {
                let v = i as f32 / 40.0 * 1.2 - 0.1;
                let pixel = [v, 0.25, 0.75, 1.0];
                let line = pixel.repeat(n);
                let got = blur(line, n, 1, sigma, 0.0);
                for px in got.as_chunks::<4>().0[reach..n - reach].iter() {
                    for (g, e) in px.iter().zip(pixel) {
                        let ulps = g.to_bits().abs_diff(e.to_bits());
                        if ulps > worst.0 {
                            worst = (ulps, v);
                        }
                    }
                }
            }
            if worst.0 > 0 {
                wrong.push(format!(
                    "sigma {sigma}: {} ulps off, at {}",
                    worst.0, worst.1
                ));
            }
        }
        assert!(wrong.is_empty(), "{}", wrong.join("; "));
    }

    #[test]
    fn a_direct_blur_is_the_exact_gaussian_except_on_ties() {
        for sigma in [0.5, 1.0, 2.0, 3.3, 4.9] {
            let (off, farthest) = off_ties(sigma, sigma);
            assert!(
                farthest < TIE,
                "sigma {sigma}: {off} bytes off, one {farthest} from a tie"
            );
        }
    }

    #[test]
    fn an_fft_blur_is_the_exact_gaussian_except_on_ties() {
        for sigma in [FFT_FROM_SIGMA, 6.0, 12.0, 25.0, 40.0] {
            let (off, farthest) = off_ties(sigma, sigma);
            assert!(
                farthest < TIE,
                "sigma {sigma}: {off} bytes off, one {farthest} from a tie"
            );
        }
    }

    #[test]
    fn each_axis_takes_its_own_sigma_and_method() {
        // One axis direct and the other through the FFT, both ways round.
        for (sx, sy) in [(2.0, 12.0), (12.0, 2.0), (0.7, 30.0)] {
            let (off, farthest) = off_ties(sx, sy);
            assert!(
                farthest < TIE,
                "({sx}, {sy}): {off} bytes off, one {farthest} from a tie"
            );
        }
    }

    #[test]
    fn a_sigma_of_nothing_leaves_its_axis_alone() {
        let src: Vec<u8> = (0..20 * 4).map(|i| (i * 7 % 256) as u8).collect();
        assert_eq!(blur_bytes(src.clone(), 20, 1, 0.0, 0.0, 1.0), src);
        assert_eq!(blur_bytes(src.clone(), 20, 1, f32::NAN, -1.0, 1.0), src);
    }

    #[test]
    fn the_fft_takes_over_at_its_crossover() {
        assert_eq!(method(FFT_FROM_SIGMA - 0.01), Method::Direct);
        assert_eq!(method(FFT_FROM_SIGMA), Method::Fourier);
        assert_eq!(method(0.5), Method::Direct);
        assert_eq!(method(100.0), Method::Fourier);
    }

    #[test]
    fn the_kernel_sums_to_one_and_reaches_six_sigmas() {
        for sigma in [0.5f32, 3.0, 40.0] {
            let k = half_kernel(sigma, usize::MAX);
            assert_eq!(k.len() - 1, (6.0 * f64::from(sigma)).ceil() as usize);
            let total = k[0] + 2.0 * k[1..].iter().sum::<f64>();
            assert!((total - 1.0).abs() < 1e-12, "{total}");
        }
    }

    #[test]
    fn a_kernel_held_to_its_line_keeps_the_weights_it_holds() {
        let whole = half_kernel(100.0, usize::MAX);
        let held = half_kernel(100.0, 10);
        assert_eq!(held.len(), 11);
        assert_eq!(held, whole[..11]);
    }

    #[test]
    fn the_closed_form_sum_agrees_with_the_sum_either_side_of_its_reach() {
        // The last sigma summed tap by tap, and the first taken in closed
        // form, each against the other way of reaching the same total.
        let summed = SUMMED_REACH / REACH_SIGMAS;
        for sigma in [summed, summed + 1.0] {
            let reach = (REACH_SIGMAS * sigma).ceil() as i64;
            let by_tap: f64 = (-reach..=reach)
                .map(|d| (-((d * d) as f64) / (2.0 * sigma * sigma)).exp())
                .sum();
            let closed = sigma * TAU.sqrt() * SHARE_WITHIN_REACH;
            assert!((gaussian_sum(sigma) - by_tap).abs() / by_tap < 1e-12);
            assert!((closed - by_tap).abs() / by_tap < 1e-12, "{sigma}");
        }
        // The constant against the standard's definition of the share.
        let reach = REACH_SIGMAS / std::f64::consts::SQRT_2;
        assert!((SHARE_WITHIN_REACH - (1.0 - erfc_far(reach))).abs() < 1e-15);
    }

    /// `erfc(x)` for `x` past 4, by its continued fraction.
    fn erfc_far(x: f64) -> f64 {
        let tail = (1..=60)
            .rev()
            .fold(0.0, |tail, k| (f64::from(k) / 2.0) / (x + tail));
        (-x * x).exp() / std::f64::consts::PI.sqrt() / (x + tail)
    }

    #[test]
    fn a_blur_wider_than_any_length_spreads_the_image_to_nothing() {
        let (w, h) = (64, 48);
        let pixels = vec![255u8; w * h * 4];
        // `blur(calc(1px * infinity))`, and an infinite sigma.
        for sigma in [f32::MAX, f32::INFINITY] {
            let out = blur_bytes(pixels.clone(), w, h, sigma, sigma, 1.0);
            assert!(out.iter().all(|&v| v == 0), "{sigma}");
        }
    }
}
