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
use std::{
    array, cell::RefCell, collections::HashMap, f64::consts::TAU, sync::Arc,
};

#[cfg(test)]
use std::cell::Cell;

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

/// The multiply-adds from which a pass of the blur is split over rayon's
/// pool rather than run on the calling thread.
///
/// A pass handed to the pool from outside it waits for a worker to take it
/// up, and one split into a task per line wakes a worker for each: a 59 by
/// 56 drop shadow at a sigma of 3 blurred in 215 microseconds on 14 threads
/// and 100 on one. Drop shadows at a sigma of 3 on the CPU, swept over the
/// threshold: a 40 pixel square, 0.44 million multiply-adds a pass, drew in
/// 177 microseconds on one thread and 278 split; a 100 pixel square,
/// 1.4 million, in 463 split and 533 on one. Each line is worked by the same
/// code either way, so the choice moves no bit of the result.
const PARALLEL_FROM: usize = 1 << 20;

/// The floats from which a copy the blur makes -- a transpose, the bytes
/// read into floats and rounded back -- is split over rayon's pool. A copy
/// costs less a float than a pass, so it pays to split sooner: swept with
/// the passes split, a 380 pixel square's drop shadow, 0.69 million floats
/// a transpose, drew in 2,145 microseconds with them split and 2,479 with
/// them on one thread; a 200 pixel square's, 0.22 million, in 878 split and 928
/// on one; a 100 pixel square's, 74 thousand, in 493 split and 469 on one. The
/// threshold lies inside that bracket and was not itself timed; the
/// release's timing covers it.
const COPY_PARALLEL_FROM: usize = 1 << 17;

#[cfg(test)]
thread_local! {
    /// Whether the blur runs every step on rayon's pool and skips no line:
    /// the plain schedule, which the defaults are held to bit for bit. Read
    /// on the thread that calls the blur.
    static PLAIN_SCHEDULE: Cell<bool> = const { Cell::new(false) };
}

/// Whether a step costing `work` is split over rayon's pool, given the work
/// `from` which a step of its kind pays to split -- [`PARALLEL_FROM`] or
/// [`COPY_PARALLEL_FROM`].
fn parallel(work: usize, from: usize) -> bool {
    #[cfg(test)]
    if PLAIN_SCHEDULE.with(Cell::get) {
        return true;
    }
    work >= from
}

/// Whether [`direct`] leaves a line of +0.0 as it is.
fn skips_empty_lines() -> bool {
    #[cfg(test)]
    if PLAIN_SCHEDULE.with(Cell::get) {
        return false;
    }
    true
}

/// Runs `op` on each `len`-long chunk of `data`, with a state `init` makes
/// for each worker: across rayon's pool where `work` -- what the whole call
/// costs -- is worth splitting, and on the calling thread otherwise -- see
/// [`parallel`].
fn each_chunk<S>(
    data: &mut [f32],
    len: usize,
    work: usize,
    init: impl Fn() -> S + Send + Sync,
    op: impl Fn(&mut S, &mut [f32]) + Send + Sync,
) {
    match parallel(work, PARALLEL_FROM) {
        true => data.par_chunks_mut(len).for_each_init(init, op),
        false => {
            let mut state = init();
            data.chunks_mut(len).for_each(|chunk| op(&mut state, chunk));
        }
    }
}

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
    let split = parallel(pixels.len(), COPY_PARALLEL_FROM);
    let floats: Vec<f32> = match split {
        true => pixels.par_iter().map(|&b| f32::from(b)).collect(),
        false => pixels.iter().map(|&b| f32::from(b)).collect(),
    };
    drop(pixels);
    let blurred = blur_in::<f32>(floats, width, height, sigma_x, sigma_y);
    // Each value is a weighted mean of bytes times a gain in [0, 1], so it
    // lies in [0, 255] up to float noise; the cast saturates what noise puts
    // outside.
    let byte = |v: f32| (v * gain + 0.5) as u8;
    match split {
        true => blurred.into_par_iter().map(byte).collect(),
        false => blurred.into_iter().map(byte).collect(),
    }
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
///
/// A line holding nothing but +0.0 is left as it is: every tap sums to +0.0
/// there, bit for bit, and a draw's layer is mostly such lines. A -0.0 is
/// worked like any other value, since a sum of signed zeros need not keep
/// its sign.
fn direct<L: Lane>(pixels: &mut [f32], n: usize, k: &[f64]) {
    let k: Vec<L> = k.iter().map(|&w| L::weight(w)).collect();
    let r = k.len() - 1;
    let (stride, pad) = (n * 4, r * 4);
    let zero = L::of(0.0);
    let work = pixels.len() * k.len();
    let skip = skips_empty_lines();
    each_chunk(
        pixels,
        stride,
        work,
        || (vec![zero; stride + 2 * pad], Vec::new()),
        |(padded, scratch), out| {
            if skip && out.iter().all(|v| v.to_bits() == 0) {
                return;
            }
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
/// place, and each value written back is held to the range its line's
/// channel allows -- see [`within_reach`].
fn fourier<L: Lane>(pixels: &mut [f32], n: usize, k: &[f64]) {
    let r = k.len() - 1;
    let len = fast_len(n + r);
    let spectrum: Vec<L> =
        spectrum(k, len).into_iter().map(L::weight).collect();
    let zero = Complex::new(L::of(0.0), L::of(0.0));
    // Two transforms each way per line, each about `len log2 len` steps.
    let work = pixels.len() / 4 * len.ilog2() as usize * 4;
    each_chunk(
        pixels,
        n * 4,
        work,
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
            // Each channel's least and greatest value, and zero.
            let (mut lo, mut hi) = ([0f32; 4], [0f32; 4]);
            for (i, px @ [r, g, bl, al]) in
                line.as_chunks::<4>().0.iter().enumerate()
            {
                a[i] = Complex::new(L::of(*r), L::of(*g));
                b[i] = Complex::new(L::of(*bl), L::of(*al));
                for (c, &v) in px.iter().enumerate() {
                    if v < lo[c] {
                        lo[c] = v;
                    }
                    if v > hi[c] {
                        hi[c] = v;
                    }
                }
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
                let sums = [a[i].re, a[i].im, b[i].re, b[i].im].map(L::narrow);
                *px = array::from_fn(|c| within_reach(sums[c], lo[c], hi[c]));
            }
        },
    );
}

/// `v`, a value of a line's channel through the FFT, held to `[lo, hi]`:
/// the least and greatest of that channel's values along the line, each
/// taken with zero. Every weight of the kernel is positive and they sum to
/// at most 1, and the line is padded with zeros, so each exact value of the
/// blur is a weighted mean of the channel's values and zero and lies in
/// that range. The FFT's round-off does not: a channel shares its transform
/// with another, and a black shadow's blue took its alpha's, coming out
/// within 1.7e-16 of 0 either side where the exact blur is 0, which a float
/// layer stores as it is. So a value past an end is that end, and every
/// value the exact blur can take is left as it is -- the comparisons let
/// -0.0 and a NaN through untouched. The range is the line's and not zero
/// to one: a colour outside the layer's gamut is carried as a channel below
/// zero, and its blur is too.
fn within_reach(v: f32, lo: f32, hi: f32) -> f32 {
    if v < lo {
        lo
    } else if v > hi {
        hi
    } else {
        v
    }
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
    let block = |(bx, rows): (usize, &mut [f32])| {
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
    };
    let rows = BLOCK * height * 4;
    match parallel(dst.len(), COPY_PARALLEL_FROM) {
        true => dst.par_chunks_mut(rows).enumerate().for_each(block),
        false => dst.chunks_mut(rows).enumerate().for_each(block),
    }
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

    /// Float scenes a schedule could tell apart: a block in a mostly empty
    /// layer, as a draw's shadow is; dense noise; rows of zeros of both
    /// signs, mixed, which sum to +0.0 where a line of +0.0 alone is left
    /// as it is; and rows whose first pixel is empty and whose later ones
    /// are not.
    fn float_scenes(w: usize, h: usize) -> Vec<Vec<f32>> {
        let mut s = 29;
        let mut unit = move || (lcg(&mut s) % 10_000) as f32 / 10_000.0;
        let block = (0..h)
            .flat_map(|y| {
                (0..w).flat_map(move |x| {
                    let inside = (w / 3..w / 3 + w / 4 + 1).contains(&x)
                        && (h / 3..h / 3 + h / 4 + 1).contains(&y);
                    match inside {
                        true => [0.25, 0.5, 0.125, 0.75],
                        false => [0.0; 4],
                    }
                })
            })
            .collect();
        let noise = (0..w * h * 4).map(|_| unit()).collect();
        let signed = (0..w * h * 4)
            .map(|i| match (i / (w * 4)) % 3 {
                0 => 0.0,
                1 => [0.0, -0.0][i % 2],
                _ => -0.0,
            })
            .collect();
        let late = (0..h)
            .flat_map(|_| {
                (0..w * 4).map(|i| match i >= w * 2 {
                    true => 0.5,
                    false => 0.0,
                })
            })
            .collect();
        vec![block, noise, signed, late]
    }

    /// The blur with [`PLAIN_SCHEDULE`] set: every step on rayon's pool and
    /// no line skipped.
    fn plainly<T>(f: impl FnOnce() -> T) -> T {
        PLAIN_SCHEDULE.with(|plain| plain.set(true));
        let out = f();
        PLAIN_SCHEDULE.with(|plain| plain.set(false));
        out
    }

    #[test]
    fn scheduling_by_work_moves_no_bit_of_the_blur() {
        // Small enough to run on the calling thread and large enough to be
        // split, at sigmas taken tap by tap and through the FFT.
        let mut compared = 0;
        for (w, h) in [(1, 1), (7, 3), (59, 56), (400, 300)] {
            for sigma in [0.5f32, 1.5, 3.0, 4.9, 5.0, 12.0, 40.0] {
                for (i, scene) in float_scenes(w, h).into_iter().enumerate() {
                    let ours = blur(scene.clone(), w, h, sigma, sigma * 0.7);
                    let plain =
                        plainly(|| blur(scene, w, h, sigma, sigma * 0.7));
                    let differs = ours
                        .iter()
                        .zip(&plain)
                        .position(|(a, b)| a.to_bits() != b.to_bits());
                    assert_eq!(
                        differs, None,
                        "{w}x{h}, sigma {sigma}, scene {i}"
                    );
                    compared += 1;
                }
                for (i, scene) in scenes(w, h).into_iter().enumerate() {
                    let ours =
                        blur_bytes(scene.clone(), w, h, sigma, sigma, 0.6);
                    let plain =
                        plainly(|| blur_bytes(scene, w, h, sigma, sigma, 0.6));
                    assert!(
                        ours == plain,
                        "bytes, {w}x{h}, sigma {sigma}, scene {i}"
                    );
                    compared += 1;
                }
            }
        }
        assert_eq!(compared, 4 * 7 * (4 + 4));
    }

    /// A layer of `w` by `h` floats, transparent but for a block in the
    /// middle third of `colour`, premultiplied.
    fn block_of(w: usize, h: usize, colour: [f32; 4]) -> Vec<f32> {
        (0..h)
            .flat_map(|y| {
                (0..w).flat_map(move |x| {
                    let inside = (w / 3..2 * w / 3).contains(&x)
                        && (h / 3..2 * h / 3).contains(&y);
                    match inside {
                        true => colour,
                        false => [0.0; 4],
                    }
                })
            })
            .collect()
    }

    /// The sigmas, both axes and one, at which a pass goes through the FFT.
    const FFT_SIGMAS: [(f32, f32); 4] =
        [(5.0, 5.0), (8.0, 8.0), (8.0, 0.0), (0.0, 5.0)];

    #[test]
    fn a_channel_of_nothing_blurs_to_nothing_through_the_fft() {
        // A black shadow: its blue shares a transform with its alpha, and
        // takes the alpha's round-off with it unless it is held to its
        // line's range, which is zero alone.
        for sigma in FFT_SIGMAS {
            let blurred = blur(
                block_of(60, 52, [0.0, 0.0, 0.0, 0.6]),
                60,
                52,
                sigma.0,
                sigma.1,
            );
            let colour =
                blurred.as_chunks::<4>().0.iter().flat_map(|p| &p[..3]);
            let (low, high) =
                colour.fold((0f32, 0f32), |(l, h), &v| (l.min(v), h.max(v)));
            assert_eq!((low, high), (0.0, 0.0), "sigma {sigma:?}");
            assert!(blurred.iter().all(|&v| v >= 0.0), "sigma {sigma:?}");
        }
    }

    #[test]
    fn a_channel_below_zero_keeps_its_blur_through_the_fft() {
        // A colour outside the layer's gamut carries its red below zero,
        // and the blur of that red is below zero too: the exact blur's,
        // to within float noise, and not zero.
        let (w, h) = (60, 52);
        for sigma in FFT_SIGMAS {
            let layer = block_of(w, h, [-0.15, 0.4, 0.2, 0.6]);
            let blurred = blur(layer.clone(), w, h, sigma.0, sigma.1);
            let exact = reference_floats(&layer, w, h, sigma.0, sigma.1);
            let lowest = blurred.iter().step_by(4).fold(0f32, |l, &v| l.min(v));
            assert!(
                lowest < -0.05,
                "sigma {sigma:?}: red reached only {lowest}"
            );
            let farthest = blurred
                .iter()
                .zip(&exact)
                .map(|(&v, &e)| (f64::from(v) - e).abs())
                .fold(0f64, f64::max);
            assert!(
                farthest < 1e-6,
                "sigma {sigma:?}: {farthest} from the exact blur"
            );
        }
    }

    /// `pixels`, RGBA floats, blurred tap by tap in f64 by the kernel the
    /// blur takes, with nothing narrowed between the passes.
    fn reference_floats(
        pixels: &[f32],
        w: usize,
        h: usize,
        sx: f32,
        sy: f32,
    ) -> Vec<f64> {
        let axis = |v: &[f64],
                    n: usize,
                    lines: usize,
                    step: usize,
                    along: usize,
                    sigma: f32| {
            if !(sigma.is_finite() && sigma > 0.0) {
                return v.to_vec();
            }
            let k = half_kernel(sigma, n - 1);
            let mut out = v.to_vec();
            for line in 0..lines {
                for i in 0..n {
                    for c in 0..4 {
                        let at =
                            |j: usize| v[(line * step + j * along) * 4 + c];
                        let mut sum = k[0] * at(i);
                        for (d, &weight) in k.iter().enumerate().skip(1) {
                            if i >= d {
                                sum += weight * at(i - d);
                            }
                            if i + d < n {
                                sum += weight * at(i + d);
                            }
                        }
                        out[(line * step + i * along) * 4 + c] = sum;
                    }
                }
            }
            out
        };
        let wide: Vec<f64> = pixels.iter().map(|&v| f64::from(v)).collect();
        let rows = axis(&wide, w, h, w, 1, sx);
        axis(&rows, h, w, 1, w, sy)
    }
}
