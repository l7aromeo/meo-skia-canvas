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
//! The blur is separable: a 1-D pass along every row, then one down every
//! column. Each axis takes its own sigma, since a transform can scale x and
//! y apart. A pass is direct -- the taps summed -- below [`FFT_FROM_SIGMA`],
//! and an FFT convolution from there, which costs the same whatever the
//! sigma.
//!
//! A direct column pass sums each row of the result from whole rows of the
//! image, a tile of values across them at a time -- see [`down`] -- so it
//! reads every row where it lies and nothing is transposed. An FFT column
//! pass transposes, works the columns as rows and transposes back. Do not
//! gather a direct column's values one by one from the rows instead: that
//! sums the same values in the same order but reads them a row apart, and
//! in release builds a drop shadow's 59 by 56 layer drew 2 to 4% slower
//! than through the transposes, and a blurred 15 by 12 ellipse 11% slower.
//!
//! A row pass works each line in place, from a copy of it its worker keeps,
//! and the column pass writes a new image, so two buffers the image's size
//! are alive at once: 32 bytes a pixel in floats.
//!
//! Float pixels are worked in f64 a line at a time, and bytes in f32 -- see
//! [`Lane`]. Unrounded, a float result has to be the f32 nearest the exact
//! sum, which f32 arithmetic misses by a few ulps; a byte is rounded to a
//! level, which f32 holds to about 1e-4. An alpha alone, for a layer of
//! bytes, is worked in f32 four rows or columns to a line, as the four
//! channels of one -- see [`blur_alpha`].

use rayon::prelude::*;
use rustfft::{Fft, FftNum, FftPlanner, num_complex::Complex};
use std::{
    array, borrow::Cow, cell::RefCell, collections::HashMap, f64::consts::TAU,
    sync::Arc,
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

/// Whether [`direct`] leaves a line of +0.0 as it is, and [`down`] leaves
/// out rows of +0.0 in a lane that may.
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
///
/// Where both axes are summed tap by tap, a channel whose plane holds the
/// same bits as another's is blurred once and copied, and a plane of +0.0
/// is left as it is -- see [`Planes`]. A drop shadow in black or grey
/// tints its colour channels to nothing or to one plane, and is blurred a
/// plane or two at a time instead of four.
pub(crate) fn blur(
    pixels: Vec<f32>,
    width: usize,
    height: usize,
    sigma_x: f32,
    sigma_y: f32,
) -> Vec<f32> {
    match Planes::of(&pixels, sigma_x, sigma_y) {
        Some(planes) => planes.blurred(pixels, width, height, sigma_x, sigma_y),
        None => blur_in::<f64>(pixels, width, height, sigma_x, sigma_y),
    }
}

/// The most distinct planes, other than planes of +0.0, that [`blur`]
/// blurs one at a time: each costs a quarter of the four-channel sums and
/// its own re-layout. Chosen, not derived: a black or grey shadow has one
/// or two, and three or four were not measured to pay.
const MOST_DISTINCT_PLANES: usize = 2;

/// Which of an image's four channels [`blur`] can work as planes of their
/// own: each channel either a plane of +0.0, or one with the same bits as
/// the first channel that holds them.
///
/// Tap by tap, a channel's sums read that channel alone -- every weight
/// times that channel's values, added in one order whichever lane holds it
/// -- so two channels whose planes hold the same bits come out the same
/// bits, and a plane of +0.0 comes out +0.0. So the key is the bits of the
/// plane, not the colour that made it: two colours equal to the eye can
/// tint to planes an ulp apart, and those are blurred apart. The FFT does
/// not hold to this, since it works two channels in one transform and each
/// takes the other's round-off, so an axis through it keeps all four.
struct Planes {
    /// Per channel, `None` for a plane of +0.0, or the channel holding the
    /// same bits first.
    source: [Option<usize>; 4],
}

impl Planes {
    /// The planes of `pixels` where both axes are summed tap by tap and no
    /// more than [`MOST_DISTINCT_PLANES`] are distinct, or `None`.
    fn of(pixels: &[f32], sigma_x: f32, sigma_y: f32) -> Option<Planes> {
        let by_taps = |sigma: f32| {
            !(sigma.is_finite() && sigma > 0.0)
                || method(sigma) == Method::Direct
        };
        if !(by_taps(sigma_x) && by_taps(sigma_y)) {
            return None;
        }
        // In one pass, which channels are all +0.0 and which pairs hold
        // the same bits, stopping as soon as no channel is either: a
        // coloured shadow's first opaque pixel ends it.
        let (mut zero, mut same) = ([true; 4], [[true; 4]; 4]);
        for p in pixels.as_chunks::<4>().0 {
            let bits = p.map(f32::to_bits);
            let mut any = false;
            for c in 0..4 {
                zero[c] &= bits[c] == 0;
                any |= zero[c];
                for d in 0..c {
                    same[c][d] &= bits[c] == bits[d];
                    any |= same[c][d];
                }
            }
            if !any {
                return None;
            }
        }
        let mut source = [None; 4];
        for c in 0..4 {
            source[c] = match zero[c] {
                true => None,
                false => Some(
                    (0..c)
                        .find(|&d| source[d] == Some(d) && same[c][d])
                        .unwrap_or(c),
                ),
            };
        }
        let distinct = (0..4).filter(|&c| source[c] == Some(c)).count();
        (distinct <= MOST_DISTINCT_PLANES).then_some(Planes { source })
    }

    /// `pixels` blurred plane by plane, each distinct plane once.
    fn blurred(
        &self,
        mut pixels: Vec<f32>,
        width: usize,
        height: usize,
        sigma_x: f32,
        sigma_y: f32,
    ) -> Vec<f32> {
        let blurred: Vec<(usize, Vec<f32>)> = (0..4)
            .filter(|&c| self.source[c] == Some(c))
            .map(|c| {
                let plane = pixels.iter().skip(c).step_by(4).copied();
                let plane = blur_plane::<f64>(
                    plane.collect(),
                    width,
                    height,
                    sigma_x,
                    sigma_y,
                );
                (c, plane)
            })
            .collect();
        let plane_of = |c: usize| {
            blurred
                .iter()
                .find(|(d, _)| *d == c)
                .map(|(_, plane)| plane)
        };
        // A plane of +0.0 blurs to itself, and is left where it is.
        for c in 0..4 {
            if let Some(plane) = self.source[c].and_then(plane_of) {
                pixels
                    .iter_mut()
                    .skip(c)
                    .step_by(4)
                    .zip(plane)
                    .for_each(|(v, &b)| *v = b);
            }
        }
        pixels
    }
}

/// Blurs `alpha`, one float a pixel `width` by `height`, as [`blur_bytes`]
/// blurs a channel, in f32, for a layer whose result is rounded to bytes,
/// and returns it -- see [`blur_plane`]. Tap by
/// tap that is the alpha [`blur_in`] gives in f32, bit for bit. Through the
/// FFT a channel shares its transform with another -- see [`fourier`] --
/// here a neighbouring row's or column's alpha where there it is blue, and
/// the two differ by float rounding.
pub(crate) fn blur_alpha(
    alpha: Vec<f32>,
    width: usize,
    height: usize,
    sigma_x: f32,
    sigma_y: f32,
) -> Vec<f32> {
    blur_plane::<f32>(alpha, width, height, sigma_x, sigma_y)
}

/// Blurs `plane`, one float a pixel `width` by `height`, in `L`, and
/// returns it.
///
/// Where each axis is summed tap by tap or left alone, the rows are summed
/// where they lie, as lines of one value a pixel -- see [`direct`] -- and
/// the columns down the rows -- see [`down`] -- each value by the same taps
/// in the same order as an RGBA channel's.
///
/// Otherwise, for an axis through the FFT, the plane is laid out again in
/// the buffer it came in: four rows of it go through the row pass as the
/// four channels of one line, and four columns through the column pass, so
/// that each is summed by the code that sums a channel, at a quarter of the
/// work of blurring four. Keep that re-layout indexed: the same moves
/// written as chunked copies -- rows interleaved band by band, the rows put
/// back before the columns are gathered four pixels at a time -- drew an
/// 8-bit drop shadow 2.5 to 3.0% slower in a release build.
fn blur_plane<L: Lane>(
    mut plane: Vec<f32>,
    width: usize,
    height: usize,
    sigma_x: f32,
    sigma_y: f32,
) -> Vec<f32> {
    if width == 0 || height == 0 {
        return plane;
    }
    let in_place = |sigma: f32| tapped(sigma) || sigma == 0.0;
    if in_place(sigma_x) && in_place(sigma_y) {
        if tapped(sigma_x) {
            let k = half_kernel(sigma_x, width - 1);
            direct::<L, 1>(&mut plane, width, &k);
        }
        return match tapped(sigma_y) {
            true => down::<L, 1>(&plane, width, height, sigma_y),
            false => plane,
        };
    }
    // Rows `4 * band` to `4 * band + 3`, each in a channel of band `band`.
    let bands = height.div_ceil(4);
    let mut rows = vec![0.0; width * bands * 4];
    for (y, row) in plane.chunks(width).enumerate() {
        for (x, &v) in row.iter().enumerate() {
            rows[(y / 4 * width + x) * 4 + y % 4] = v;
        }
    }
    pass::<L>(&mut rows, width, bands, sigma_x);
    // Columns `4 * band` to `4 * band + 3`, each in a channel of band `band`.
    let bands = width.div_ceil(4);
    let mut columns = vec![0.0; height * bands * 4];
    for y in 0..height {
        for x in 0..width {
            columns[(x / 4 * height + y) * 4 + x % 4] =
                rows[(y / 4 * width + x) * 4 + y % 4];
        }
    }
    pass::<L>(&mut columns, height, bands, sigma_y);
    for (y, row) in plane.chunks_mut(width).enumerate() {
        for (x, v) in row.iter_mut().enumerate() {
            *v = columns[(x / 4 * height + y) * 4 + x % 4];
        }
    }
    plane
}

/// Blurs `pixels`, premultiplied RGBA bytes, as [`blur`] does but in f32,
/// scales the result by `gain` and rounds once at the end, into the first
/// `width * 4` bytes of each of `out`'s rows, `stride` bytes apart: a
/// blurred draw composited at an alpha takes its alpha here, so that it is
/// not rounded twice. f32 holds a level to within about 1e-4, so it decides
/// no byte that f64 would decide otherwise but on exact ties, at about
/// twice the speed. `pixels` is let go as soon as it is in floats.
///
/// A column pass taken tap by tap rounds each row as it sums it, straight
/// into `out` -- see [`down_into`] -- so the result is never held in floats.
pub(crate) fn blur_bytes_into(
    pixels: Vec<u8>,
    width: usize,
    height: usize,
    (sigma_x, sigma_y): (f32, f32),
    gain: f32,
    out: &mut [u8],
    stride: usize,
) {
    if width == 0 || height == 0 {
        return;
    }
    let split = parallel(pixels.len(), COPY_PARALLEL_FROM);
    let mut floats: Vec<f32> = match split {
        true => pixels.par_iter().map(|&b| f32::from(b)).collect(),
        false => pixels.iter().map(|&b| f32::from(b)).collect(),
    };
    drop(pixels);
    // Each value is a weighted mean of bytes times a gain in [0, 1], so it
    // lies in [0, 255] up to float noise; the cast saturates what noise puts
    // outside.
    let byte = |v: f32| (v * gain + 0.5) as u8;
    if tapped(sigma_y) {
        pass::<f32>(&mut floats, width, height, sigma_x);
        return down_into::<f32, u8, 4>(
            &floats, width, height, sigma_y, out, stride, byte,
        );
    }
    let line = width * 4;
    let blurred = blur_in::<f32>(floats, width, height, sigma_x, sigma_y);
    let rows = |(row, values): (&mut [u8], &[f32])| {
        row[..line]
            .iter_mut()
            .zip(values)
            .for_each(|(o, &v)| *o = byte(v));
    };
    match split {
        true => out
            .par_chunks_mut(stride)
            .zip(blurred.par_chunks(line))
            .for_each(rows),
        false => out
            .chunks_mut(stride)
            .zip(blurred.chunks(line))
            .for_each(rows),
    }
}

/// [`blur_bytes_into`], into a buffer of its own with no room between rows.
#[cfg(test)]
pub(crate) fn blur_bytes(
    pixels: Vec<u8>,
    width: usize,
    height: usize,
    sigma_x: f32,
    sigma_y: f32,
    gain: f32,
) -> Vec<u8> {
    let mut out = vec![0; pixels.len()];
    blur_bytes_into(
        pixels,
        width,
        height,
        (sigma_x, sigma_y),
        gain,
        &mut out,
        width * 4,
    );
    out
}

/// [`blur`], with each line worked in `L`. Columns summed tap by tap are
/// summed down the rows where they lie -- see [`down`] -- and only a column
/// pass through the FFT is transposed.
fn blur_in<L: Lane>(
    mut pixels: Vec<f32>,
    width: usize,
    height: usize,
    sigma_x: f32,
    sigma_y: f32,
) -> Vec<f32> {
    pass::<L>(&mut pixels, width, height, sigma_x);
    if tapped(sigma_y) && width > 0 && height > 0 {
        return down::<L, 4>(&pixels, width, height, sigma_y);
    }
    let mut columns = vec![0.0; pixels.len()];
    transpose(&pixels, &mut columns, width, height);
    pass::<L>(&mut columns, height, width, sigma_y);
    transpose(&columns, &mut pixels, height, width);
    pixels
}

/// The arithmetic a line is worked in: f32 for bytes, f64 for floats.
trait Lane: FftNum {
    /// Whether the FFT holds this lane's values to their line's range --
    /// see [`within_reach`]. A float result is stored as it is, so its
    /// round-off past the range would be too; an f32 lane's values are
    /// rounded to bytes, and the cast to a byte saturates below 0 and
    /// above 255, so the range would only repeat what the cast does.
    const HELD_TO_REACH: bool;
    /// Whether this lane's results lose a zero's sign, so that [`down`] may
    /// leave out a pair of rows holding nothing but +0.0: adding +0.0
    /// changes a sum only by turning a -0.0 to +0.0. An f32 lane's values
    /// are rounded to bytes, which have no sign; a float result keeps it,
    /// so its every pair is added.
    const SIGNLESS_ZEROS: bool;
    /// `v` in this arithmetic.
    fn of(v: f32) -> Self;
    /// A weight, worked out in f64, in this arithmetic.
    fn weight(w: f64) -> Self;
    /// This value as the f32 a pixel holds.
    fn narrow(self) -> f32;
    /// The forward and inverse plans of `len` in this arithmetic, kept per
    /// worker.
    fn plans(len: usize) -> Plans<Self>;
    /// `values` in this arithmetic, converted once for every tap that reads
    /// them: themselves in f32, and in f64 a copy.
    fn lifted(values: &[f32]) -> Cow<'_, [Self]>;
}

impl Lane for f32 {
    const HELD_TO_REACH: bool = false;
    const SIGNLESS_ZEROS: bool = true;

    fn of(v: f32) -> Self {
        v
    }

    fn weight(w: f64) -> Self {
        w as f32
    }

    fn narrow(self) -> f32 {
        self
    }

    fn plans(len: usize) -> Plans<Self> {
        PLANS_F32.with(|plans| plan(&mut plans.borrow_mut(), len))
    }

    fn lifted(values: &[f32]) -> Cow<'_, [Self]> {
        Cow::Borrowed(values)
    }
}

impl Lane for f64 {
    const HELD_TO_REACH: bool = true;
    const SIGNLESS_ZEROS: bool = false;

    fn of(v: f32) -> Self {
        f64::from(v)
    }

    fn weight(w: f64) -> Self {
        w
    }

    fn narrow(self) -> f32 {
        self as f32
    }

    fn plans(len: usize) -> Plans<Self> {
        PLANS_F64.with(|plans| plan(&mut plans.borrow_mut(), len))
    }

    fn lifted(values: &[f32]) -> Cow<'_, [Self]> {
        Cow::Owned(values.iter().map(|&v| f64::from(v)).collect())
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
        Method::Direct => direct::<L, 4>(pixels, n, &k),
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

/// Whether an axis blurred by `sigma` is summed tap by tap: a sigma that
/// blurs at all, below [`FFT_FROM_SIGMA`].
fn tapped(sigma: f32) -> bool {
    sigma.is_finite() && sigma > 0.0 && method(sigma) == Method::Direct
}

/// The floats of a line whose sums [`tile`] works together, each in a
/// register across every tap -- four RGBA pixels. Sixteen f32 take four
/// 128-bit registers and sixteen f64 eight, of Arm64's 32 or x86-64's 16,
/// which leaves room for the taps' values. Chosen to fit, not measured
/// against other widths.
const TILE: usize = 16;

/// The most pairs of taps a line summed tap by tap takes: its reach,
/// `ceil(REACH_SIGMAS * sigma)` -- see [`half_kernel`] -- for a sigma below
/// [`FFT_FROM_SIGMA`].
const MOST_PAIRS: usize = (REACH_SIGMAS * FFT_FROM_SIGMA as f64) as usize;

/// One pair of taps: its weight and the two lines it weighs, each read
/// from where the line being summed is, already in the lane -- converting
/// a value at every tap that reads it bound an f64 tap loop by the
/// conversions, and drew a float layer's blurred ellipse 5.7% slower.
type Pair<'a, L> = (L, &'a [L], &'a [L]);

/// Sums the `N` values of `out` from `at`: `k0` times `centre`'s, then
/// each pair's weight times the sum of its two lines' values, pair by pair,
/// in `L`, each written by `put` once summed -- narrowed to a float, or
/// rounded to a byte. The accumulators are an array the
/// size of the tile, which the compiler keeps in registers, so a tap costs
/// two reads a value rather than four memory moves.
#[inline(always)]
fn tile<L: Lane, T, const N: usize>(
    out: &mut [T],
    at: usize,
    centre: &[L],
    k0: L,
    pairs: &[Pair<L>],
    put: &impl Fn(L) -> T,
) {
    let centre = &centre[at..at + N];
    let mut acc: [L; N] = array::from_fn(|i| k0 * centre[i]);
    for &(w, a, b) in pairs {
        let (a, b) = (&a[at..at + N], &b[at..at + N]);
        for i in 0..N {
            acc[i] = acc[i] + w * (a[i] + b[i]);
        }
    }
    out[at..at + N]
        .iter_mut()
        .zip(acc)
        .for_each(|(o, v)| *o = put(v));
}

/// Sums every value of `out`, a line of pixels of `C` values each, as
/// [`tile`] does: [`TILE`] at a time, then what is left eight and four at a
/// time, and the rest a pixel at a time. Every value takes the same sums in
/// the same order whatever tile it is in; the narrower tiles keep the tail
/// of a line of one value a pixel in registers, where a value at a time
/// sums it in scalars.
fn sum_line<L: Lane, T, const C: usize>(
    out: &mut [T],
    centre: &[L],
    k0: L,
    pairs: &[Pair<L>],
    put: &impl Fn(L) -> T,
) {
    let mut at = 0;
    while out.len() - at >= TILE {
        tile::<L, T, TILE>(out, at, centre, k0, pairs, put);
        at += TILE;
    }
    if out.len() - at >= 8 {
        tile::<L, T, 8>(out, at, centre, k0, pairs, put);
        at += 8;
    }
    if out.len() - at >= 4 {
        tile::<L, T, 4>(out, at, centre, k0, pairs, put);
        at += 4;
    }
    for at in (at..out.len()).step_by(C) {
        tile::<L, T, C>(out, at, centre, k0, pairs, put);
    }
}

/// Each line of `n` pixels of `C` values tap by tap: `out[x] = k0 in[x] +
/// sum_d k_d (in[x-d] + in[x+d])`, a value of the pixel `d` either side, over
/// a copy of the line padded with zeros so no tap leaves it, a tile of sums at
/// a time -- see [`tile`]. RGBA lines take `C` of 4, and a plane of one float
/// a pixel -- see [`blur_plane`] -- 1.
///
/// A line holding nothing but +0.0 is left as it is: every tap sums to +0.0
/// there, bit for bit, and a draw's layer is mostly such lines. A -0.0 is
/// worked like any other value, since a sum of signed zeros need not keep
/// its sign.
fn direct<L: Lane, const C: usize>(pixels: &mut [f32], n: usize, k: &[f64]) {
    let k: Vec<L> = k.iter().map(|&w| L::weight(w)).collect();
    let r = k.len() - 1;
    let (stride, pad) = (n * C, r * C);
    let work = pixels.len() * k.len();
    let skip = skips_empty_lines();
    each_chunk(
        pixels,
        stride,
        work,
        || vec![L::of(0.0); stride + 2 * pad],
        |padded, out| {
            if skip && out.iter().all(|v| v.to_bits() == 0) {
                return;
            }
            padded[pad..pad + stride]
                .iter_mut()
                .zip(out.iter())
                .for_each(|(p, &v)| *p = L::of(v));
            let padded = &*padded;
            // Held on the stack, since they borrow the worker's own copy of
            // the line; past MOST_PAIRS the slice below refuses rather than
            // drop a tap.
            let mut pairs: [Pair<L>; MOST_PAIRS] = [(k[0], &[], &[]); _];
            for (pair, (d, &w)) in
                pairs.iter_mut().zip(k.iter().enumerate().skip(1))
            {
                *pair = (w, &padded[pad - d * C..], &padded[pad + d * C..]);
            }
            sum_line::<L, f32, C>(
                out,
                &padded[pad..],
                k[0],
                &pairs[..r],
                &L::narrow,
            );
        },
    );
}

/// The column pass of `pixels`, `height` rows of `width` pixels of `C`
/// values each -- RGBA, or a plane's one float -- tap by tap, into a new
/// image: each row of the result summed from whole rows
/// of `pixels` as [`direct`] sums a line from its neighbours, with a row of
/// +0.0 past either end. Every value takes the same sums in the same order
/// as through a transpose, and the rows are read where they lie, a tile at
/// a time across them.
///
/// In a lane whose zeros carry no sign -- see [`Lane::SIGNLESS_ZEROS`] --
/// a row's sum takes only the pairs that reach the span from the first row
/// holding anything but +0.0 to the last, and a row outside that span that
/// no pair reaches is left at +0.0: a layer's rows past what it draws are
/// such rows, and so are the pairs most of its sums would add. A pair of
/// rows of +0.0 inside the span is still added, which adds +0.0. The pairs
/// come as at most two runs of distances, so choosing them costs no test a
/// tap: one a tap -- two flags read and branched on, for each of 18 pairs
/// of each of 51 rows -- took 7.8% of a blurred 15 by 12 ellipse's draw.
/// Floats add every pair, and so turn a -0.0 to +0.0 as the sum defines.
fn down<L: Lane, const C: usize>(
    pixels: &[f32],
    width: usize,
    height: usize,
    sigma: f32,
) -> Vec<f32> {
    let mut out = vec![0.0; pixels.len()];
    down_into::<L, f32, C>(
        pixels,
        width,
        height,
        sigma,
        &mut out,
        width * C,
        L::narrow,
    );
    out
}

/// [`down`], each value written by `put` -- see [`tile`] -- into the first
/// `width * C` values of each of `out`'s rows, `stride` apart.
fn down_into<L: Lane, T: Copy + Send, const C: usize>(
    pixels: &[f32],
    width: usize,
    height: usize,
    sigma: f32,
    out: &mut [T],
    stride: usize,
    put: impl Fn(L) -> T + Sync,
) {
    let line_len = width * C;
    let k: Vec<L> = half_kernel(sigma, height - 1)
        .iter()
        .map(|&w| L::weight(w))
        .collect();
    let r = k.len() - 1;
    let lifted = L::lifted(pixels);
    let rows: Vec<&[L]> = lifted.chunks(line_len).collect();
    let nothing = vec![L::of(0.0); line_len];
    let row = |y: isize| match usize::try_from(y) {
        Ok(y) if y < height => rows[y],
        _ => &nothing,
    };
    // The first and last rows holding anything but +0.0, in a lane that may
    // leave the rest out.
    let held = |row: &[f32]| row.iter().any(|v| v.to_bits() != 0);
    let span = match L::SIGNLESS_ZEROS && skips_empty_lines() {
        true => match pixels.chunks(line_len).position(held) {
            Some(lo) => {
                let below = pixels.chunks(line_len).rev().position(held);
                Some((lo, height - 1 - below.unwrap_or(0)))
            }
            None => {
                let nothing = put(L::of(0.0));
                out.chunks_mut(stride)
                    .for_each(|row| row[..line_len].fill(nothing));
                return;
            }
        },
        false => None,
    };
    let line = |pairs: &mut Vec<_>, (y, out): (usize, &mut [T])| {
        let out = &mut out[..line_len];
        pairs.clear();
        let taps = |d: usize| {
            let (y, d) = (y as isize, d as isize);
            (k[d as usize], row(y - d), row(y + d))
        };
        match span {
            None => pairs.extend((1..=r).map(taps)),
            Some((lo, hi)) => {
                let (y, lo, hi, r) =
                    (y as isize, lo as isize, hi as isize, r as isize);
                // The distances reaching the span from above and from below,
                // in order of distance and each once.
                let above = (y - hi).max(1)..=(y - lo).min(r);
                let below = (lo - y).max(1)..=(hi - y).min(r);
                let (first, second) = match above.start() <= below.start() {
                    true => (above, below),
                    false => (below, above),
                };
                let seam = (*first.end() + 1).max(*second.start());
                let ds = first.chain(seam..=*second.end());
                pairs.extend(ds.map(|d| taps(d as usize)));
                if pairs.is_empty() && !(lo..=hi).contains(&y) {
                    out.fill(put(L::of(0.0)));
                    return;
                }
            }
        }
        sum_line::<L, T, C>(out, rows[y], k[0], pairs, &put);
    };
    match parallel(pixels.len() * k.len(), PARALLEL_FROM) {
        true => out
            .par_chunks_mut(stride)
            .enumerate()
            .for_each_init(Vec::new, line),
        false => {
            let mut pairs = Vec::with_capacity(k.len());
            out.chunks_mut(stride)
                .enumerate()
                .for_each(|row| line(&mut pairs, row));
        }
    }
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
/// place, and in a lane that holds them -- see [`Lane::HELD_TO_REACH`] --
/// each value written back is held to the range its line's channel allows,
/// see [`within_reach`]. Every value written back is a zero of positive
/// sign where it is zero at all -- see [`positive_zero`].
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
            let pixels = line.as_chunks::<4>().0;
            for (i, [r, g, bl, al]) in pixels.iter().enumerate() {
                a[i] = Complex::new(L::of(*r), L::of(*g));
                b[i] = Complex::new(L::of(*bl), L::of(*al));
            }
            // Each channel's least and greatest value, and zero, found by
            // comparison: a NaN compares false, so it reaches neither.
            let reach = L::HELD_TO_REACH.then(|| {
                pixels.iter().fold(([0f32; 4], [0f32; 4]), |(lo, hi), p| {
                    (
                        array::from_fn(
                            |c| if p[c] < lo[c] { p[c] } else { lo[c] },
                        ),
                        array::from_fn(
                            |c| if p[c] > hi[c] { p[c] } else { hi[c] },
                        ),
                    )
                })
            });
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
                *px = match reach {
                    Some((lo, hi)) => {
                        array::from_fn(|c| within_reach(sums[c], lo[c], hi[c]))
                    }
                    None => sums,
                }
                .map(positive_zero);
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
///
/// `f32::clamp` is those two comparisons, and refuses a NaN bound or
/// `lo > hi` by panicking; neither can happen, since both ends start at
/// zero and move only by comparison, which a NaN never wins.
fn within_reach(v: f32, lo: f32, hi: f32) -> f32 {
    v.clamp(lo, hi)
}

/// `v`, or +0.0 where `v` is -0.0; every other value, a NaN among them,
/// keeps its bits.
///
/// The FFT's round-off leaves a -0.0 now and then where the exact blur is
/// zero: an F32 layer of one opaque rect, blurred at a sigma of 6, kept two
/// alphas of -0.0. The sign is the round-off's, not the blur's, and it is
/// not harmless: through Skia's colour matrix, as a chain's colour step and
/// a float layer's drop shadow tint, a pixel of alpha -0.0 came out NaN in
/// every channel, and the shadow's blur then spread that across 338 pixels.
/// A sum tap by tap makes no -0.0 from values that hold none -- each is a
/// weight times a value, added -- so only the FFT's values need it.
fn positive_zero(v: f32) -> f32 {
    if v == 0.0 { 0.0 } else { v }
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

    #[test]
    fn a_positive_zero_changes_nothing_but_a_negative_zero() {
        // Each sign and exponent, with mantissas at both ends and between,
        // and the NaNs, quiet and signalling, of either sign: every value
        // keeps its bits but -0.0, which becomes +0.0.
        let mantissas =
            [0u32, 1, 2, 0x40_0000, 0x55_5555, 0x7F_FFFE, 0x7F_FFFF];
        let mut seen = 0;
        for sign in [0u32, 1 << 31] {
            for exponent in 0..=255u32 {
                for m in mantissas {
                    let bits = sign | exponent << 23 | m;
                    let want = match bits == 1 << 31 {
                        true => 0,
                        false => bits,
                    };
                    let got = positive_zero(f32::from_bits(bits)).to_bits();
                    assert_eq!(got, want, "{bits:#010x}");
                    seen += 1;
                }
            }
        }
        assert_eq!(seen, 2 * 256 * mantissas.len());
    }

    #[test]
    fn the_fft_writes_no_negative_zero() {
        // F32 layers of one opaque grey rect, blurred through the FFT, where
        // the round-off left -0.0 before `positive_zero` was applied to what
        // the FFT writes. Where it lands depends on the machine, since
        // rustfft picks its arithmetic for the processor: the first layer
        // left two alphas of -0.0 on arm64 and none on an x86-64 with AVX2
        // and FMA, and the second left one colour value of -0.0 there and
        // none on arm64. Each was found by a search of 828 such layers on
        // both machines.
        for (w, h, (x0, y0, x1, y1), sigma) in [
            (144, 108, (72, 10, 120, 46), 6.0),
            (192, 144, (48, 36, 112, 84), 8.0),
        ] {
            let pixels: Vec<f32> = (0..w * h)
                .flat_map(|i| {
                    let (x, y) = (i % w, i / w);
                    match (x0..x1).contains(&x) && (y0..y1).contains(&y) {
                        true => [0.4, 0.4, 0.4, 1.0],
                        false => [0.0; 4],
                    }
                })
                .collect();
            assert_eq!(method(sigma), Method::Fourier);
            let negative: Vec<usize> = blur(pixels, w, h, sigma, sigma)
                .iter()
                .enumerate()
                .filter(|(_, v)| **v == 0.0 && v.is_sign_negative())
                .map(|(i, _)| i)
                .collect();
            assert!(
                negative.is_empty(),
                "{w} by {h} at a sigma of {sigma}: -0.0 at values {negative:?}"
            );
        }
    }

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

    /// One axis of the blur as its taps define it, a value at a time:
    /// `k_0 v + sum_d k_d (v[-d] + v[+d])` in `L`, adding `d` from 1 out,
    /// with +0.0 past either end of the line, narrowed once a value is
    /// summed. Along rows where `rows`, down columns otherwise. Through the
    /// FFT, or with no sigma to speak of, an axis is left to [`pass`].
    fn by_the_taps<L: Lane>(
        src: &[f32],
        width: usize,
        height: usize,
        sigma: f32,
        rows: bool,
    ) -> Vec<f32> {
        if !tapped(sigma) {
            let mut out = src.to_vec();
            match rows {
                true => pass::<L>(&mut out, width, height, sigma),
                false => {
                    let mut columns = vec![0.0; out.len()];
                    transpose(&out, &mut columns, width, height);
                    pass::<L>(&mut columns, height, width, sigma);
                    transpose(&columns, &mut out, height, width);
                }
            }
            return out;
        }
        let n = match rows {
            true => width,
            false => height,
        };
        let k: Vec<L> = half_kernel(sigma, n - 1)
            .into_iter()
            .map(L::weight)
            .collect();
        let at = |x: usize, y: usize, c: usize, i: isize| -> L {
            if !(0..n as isize).contains(&i) {
                return L::of(0.0);
            }
            let (x, y) = match rows {
                true => (i as usize, y),
                false => (x, i as usize),
            };
            L::of(src[(y * width + x) * 4 + c])
        };
        (0..src.len())
            .map(|v| {
                let (c, x, y) = (v % 4, v / 4 % width, v / 4 / width);
                let i = match rows {
                    true => x,
                    false => y,
                } as isize;
                let mut acc = k[0] * at(x, y, c, i);
                for (d, &w) in k.iter().enumerate().skip(1) {
                    let d = d as isize;
                    acc = acc + w * (at(x, y, c, i - d) + at(x, y, c, i + d));
                }
                acc.narrow()
            })
            .collect()
    }

    /// Rows of +0.0 and -0.0 taking turns, so that a column's sum keeps a
    /// -0.0 only where it adds every tap, the rows of +0.0 among them.
    fn turns_of_zeros(w: usize, h: usize) -> Vec<f32> {
        (0..w * h * 4)
            .map(|i| match (i / (w * 4)).is_multiple_of(2) {
                true => -0.0,
                false => 0.0,
            })
            .collect()
    }

    /// Opaque rows a few apart with empty rows between, so that a column's
    /// rows of +0.0 lie inside what it holds as well as around it.
    fn gapped(w: usize, h: usize) -> Vec<u8> {
        (0..w * h * 4)
            .map(|i| match (i / (w * 4)) % 7 == 3 {
                true => [200, 120, 40, 255][i % 4],
                false => 0,
            })
            .collect()
    }

    #[test]
    fn a_blur_sums_each_value_as_its_taps_define() {
        // Every value the blur gives, against the taps summed one value at a
        // time in the order that defines them, bit for bit: in f32, as bytes
        // are blurred, and in f64, as floats are, with the zeros' signs. Both
        // axes tap by tap, one or the other through the FFT, an axis left
        // alone, on lines shorter than the reach, empty rows above, below and
        // between what is drawn, widths that leave part of a tile, and
        // large enough to be split over rayon's pool.
        fn check<L: Lane>(scene: &[f32], w: usize, h: usize, s: (f32, f32)) {
            let rows = by_the_taps::<L>(scene, w, h, s.0, true);
            let want = by_the_taps::<L>(&rows, w, h, s.1, false);
            for plain in [false, true] {
                let blurred = || blur_in::<L>(scene.to_vec(), w, h, s.0, s.1);
                let got = match plain {
                    true => plainly(blurred),
                    false => blurred(),
                };
                let differs = got
                    .iter()
                    .zip(&want)
                    .position(|(g, e)| g.to_bits() != e.to_bits());
                assert_eq!(
                    differs,
                    None,
                    "{} lane, {w}x{h}, sigmas {s:?}, plain {plain}",
                    size_of::<L>() * 8
                );
            }
        }
        let sigmas = [
            (0.5f32, 0.5f32),
            (1.0, 3.0),
            (3.0, 3.0),
            (4.9, 4.9),
            (3.0, 0.0),
            (0.0, 2.0),
            (8.0, 3.0),
            (3.0, 8.0),
        ];
        let mut compared = 0;
        for (w, h) in [
            (1usize, 1usize),
            (1, 40),
            (40, 1),
            (5, 3),
            (7, 130),
            (54, 51),
            (59, 56),
            (200, 7),
        ] {
            for s in sigmas {
                for scene in scenes(w, h).into_iter().chain([gapped(w, h)]) {
                    let floats: Vec<f32> =
                        scene.into_iter().map(f32::from).collect();
                    check::<f32>(&floats, w, h, s);
                    compared += 1;
                }
                for scene in
                    float_scenes(w, h).into_iter().chain([turns_of_zeros(w, h)])
                {
                    check::<f64>(&scene, w, h, s);
                    compared += 1;
                }
            }
        }
        for s in [(3.0, 3.0), (1.0, 4.9)] {
            let floats: Vec<f32> =
                scenes(400, 300)[1].iter().copied().map(f32::from).collect();
            check::<f32>(&floats, 400, 300, s);
            check::<f64>(&float_scenes(400, 300)[0], 400, 300, s);
            compared += 2;
        }
        assert_eq!(compared, 8 * 8 * (5 + 5) + 2 * 2);
    }

    #[test]
    fn a_byte_blur_rounds_the_taps_once_into_its_rows() {
        // Each byte against the taps summed in f32 as they define it, times
        // the gain, rounded once; written into rows with room after each,
        // which is left as it was. Both axes tap by tap, either through the
        // FFT, an axis left alone, empty rows around and between, a layer
        // of nothing, and large enough to be split over rayon's pool.
        const ROOM: usize = 12;
        const UNTOUCHED: u8 = 0xab;
        let check = |src: &[u8],
                     w: usize,
                     h: usize,
                     s: (f32, f32),
                     gain: f32| {
            let floats: Vec<f32> = src.iter().copied().map(f32::from).collect();
            let rows = by_the_taps::<f32>(&floats, w, h, s.0, true);
            let want: Vec<u8> = by_the_taps::<f32>(&rows, w, h, s.1, false)
                .into_iter()
                .map(|v| (v * gain + 0.5) as u8)
                .collect();
            let stride = w * 4 + ROOM;
            for plain in [false, true] {
                let mut out = vec![UNTOUCHED; stride * h];
                let mut blurred = || {
                    blur_bytes_into(
                        src.to_vec(),
                        w,
                        h,
                        s,
                        gain,
                        &mut out,
                        stride,
                    )
                };
                match plain {
                    true => plainly(blurred),
                    false => blurred(),
                }
                for (y, row) in out.chunks(stride).enumerate() {
                    assert!(
                        row[..w * 4] == want[y * w * 4..][..w * 4],
                        "{w}x{h}, sigmas {s:?}, gain {gain}, row {y}, plain {plain}"
                    );
                    assert!(row[w * 4..].iter().all(|&b| b == UNTOUCHED));
                }
            }
        };
        let sigmas = [
            (0.5, 0.5),
            (3.0, 3.0),
            (4.9, 1.0),
            (3.0, 0.0),
            (0.0, 2.0),
            (8.0, 3.0),
            (3.0, 8.0),
        ];
        let mut compared = 0;
        for (w, h) in [(1, 1), (5, 3), (7, 130), (54, 51), (200, 7)] {
            let empty = vec![0u8; w * h * 4];
            for s in sigmas {
                for scene in scenes(w, h)
                    .into_iter()
                    .chain([gapped(w, h), empty.clone()])
                {
                    for gain in [1.0, 0.6] {
                        check(&scene, w, h, s, gain);
                        compared += 1;
                    }
                }
            }
        }
        check(&scenes(400, 300)[1], 400, 300, (3.0, 3.0), 1.0);
        assert_eq!(compared, 5 * 7 * 6 * 2);
    }

    #[test]
    fn a_float_zero_keeps_the_sign_its_taps_give_it() {
        // A column of -0.0 sums to -0.0 wherever its taps stay inside it, and
        // +0.0 where one reaches past an end, whose +0.0 takes the sign away.
        // Rows of +0.0 and -0.0 taking turns give +0.0 everywhere: each sum
        // adds a pair of rows of +0.0, which a pass that left those out would
        // not, and would give -0.0. Floats keep a zero's sign; bytes do not.
        let (w, h, sigma) = (3, 40, 1.0);
        let reach = (REACH_SIGMAS * f64::from(sigma)).ceil() as usize;
        let negative = vec![-0.0f32; w * h * 4];
        let got = blur_in::<f64>(negative, w, h, 0.0, sigma);
        for (y, row) in got.chunks(w * 4).enumerate() {
            let inside = (reach..h - reach).contains(&y);
            for v in row {
                assert_eq!(
                    v.is_sign_negative(),
                    inside,
                    "row {y} of a column of -0.0 gave {v:?}"
                );
            }
        }
        let got = blur_in::<f64>(turns_of_zeros(w, h), w, h, 0.0, sigma);
        let negative = got.iter().position(|v| v.to_bits() != 0);
        assert_eq!(negative, None, "rows of zeros taking turns");
        // A lone row of -0.0 among rows of +0.0: its every pair is two rows
        // of +0.0, which a pass leaving such pairs out of a float's sum would
        // drop, and keep the -0.0.
        let lone: Vec<f32> = (0..w * 7 * 4)
            .map(|i| match i / (w * 4) == 3 {
                true => -0.0,
                false => 0.0,
            })
            .collect();
        let got = blur_in::<f64>(lone, w, 7, 0.0, sigma);
        let negative = got.iter().position(|v| v.to_bits() != 0);
        assert_eq!(negative, None, "a lone row of -0.0");
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

    /// The most [`blur_alpha`] may lie from the alpha [`blur_in`] gives in
    /// f32 through the FFT, where each shares its transform with a
    /// different channel: a few f32 ulps of the largest value, which is at
    /// most 1.
    const FFT_PARTNER_NOISE: f32 = 1e-6;

    #[test]
    fn an_alpha_blurs_as_the_alpha_of_four_channels_does() {
        // Summed where it lies wherever each axis is tapped or left alone,
        // and laid out as four channels through the FFT or past every line:
        // both against the alpha of a four-channel blur. Lines shorter than
        // the reach, and widths and heights that leave part of a tile.
        let (mut compared, mut farthest) = (0, 0f32);
        for (w, h) in [
            (1, 1),
            (7, 3),
            (5, 9),
            (1, 40),
            (40, 1),
            (200, 7),
            (59, 56),
            (400, 300),
        ] {
            for sigma in [
                (0.5f32, 0.5f32),
                (3.0, 1.5),
                (4.9, 4.75),
                (4.9, 5.0),
                (12.0, 40.0),
                (0.0, 2.0),
                (3.0, 0.0),
                (3.0, f32::INFINITY),
            ] {
                let fft = [sigma.0, sigma.1].into_iter().any(|s| {
                    s.is_finite() && s > 0.0 && method(s) == Method::Fourier
                });
                for (i, scene) in float_scenes(w, h).into_iter().enumerate() {
                    let alpha = scene.iter().skip(3).step_by(4).copied();
                    let ours =
                        blur_alpha(alpha.collect(), w, h, sigma.0, sigma.1);
                    let four = blur_in::<f32>(scene, w, h, sigma.0, sigma.1);
                    let four = four.iter().skip(3).step_by(4);
                    assert_eq!(ours.len(), w * h);
                    match fft {
                        true => ours.iter().zip(four).for_each(|(a, b)| {
                            farthest = farthest.max((a - b).abs());
                        }),
                        false => assert_eq!(
                            ours.iter()
                                .zip(four)
                                .position(|(a, b)| a.to_bits() != b.to_bits()),
                            None,
                            "{w}x{h}, sigma {sigma:?}, scene {i}"
                        ),
                    }
                    compared += 1;
                }
            }
        }
        assert_eq!(compared, 8 * 8 * 4);
        assert!(farthest > 0.0, "the FFT's partners never moved a value");
        assert!(farthest < FFT_PARTNER_NOISE, "{farthest} apart");
        println!("through the FFT, farthest apart {farthest}");
    }

    /// A shadow's layer `w` by `h`: an alpha of noise over a block, each
    /// pixel's colour `tint` times its alpha, as a tint colours a shadow.
    fn tinted(w: usize, h: usize, tint: [f32; 3]) -> Vec<f32> {
        let mut seed = 41;
        (0..w * h)
            .flat_map(|i| {
                let (x, y) = (i % w, i / w);
                let inside = (w / 4..3 * w / 4).contains(&x)
                    && (h / 4..3 * h / 4).contains(&y);
                let a = match inside {
                    true => 0.9,
                    false => (lcg(&mut seed) % 1000) as f32 / 4000.0,
                };
                [tint[0] * a, tint[1] * a, tint[2] * a, a]
            })
            .collect()
    }

    #[test]
    fn a_blur_by_planes_keeps_every_bit_of_four_channels() {
        // Black, grey and coloured shadows, opaque and translucent -- the
        // translucency is the tint's own alpha, folded into the plane --
        // either side of the FFT, on the calling thread and split.
        let tints = [
            [0.0, 0.0, 0.0],
            [0.5, 0.5, 0.5],
            [0.25, 0.25, 0.25],
            [0.78, 0.16, 0.35],
            [0.0, 0.6, 0.0],
            // A plane of -0.0, which is not a plane of nothing: its blur
            // is +0.0 wherever a tap reaches past the edge.
            [0.0, 0.0, -0.0],
        ];
        let gains = [1.0, 0.45];
        let sigmas = [
            (0.5, 0.5),
            (3.0, 3.0),
            (4.9, 4.75),
            (4.9, 5.0),
            (5.0, 5.0),
            (8.0, 8.0),
            (1.5, 0.0),
            (0.0, 3.0),
        ];
        let (mut compared, mut by_planes) = (0, 0);
        for (w, h) in [(1, 1), (7, 3), (59, 56), (400, 300)] {
            for tint in tints {
                for gain in gains {
                    let mut layer = tinted(w, h, tint);
                    layer.iter_mut().for_each(|v| *v *= gain);
                    for sigma in sigmas {
                        let ours = blur(layer.clone(), w, h, sigma.0, sigma.1);
                        let four = blur_in::<f64>(
                            layer.clone(),
                            w,
                            h,
                            sigma.0,
                            sigma.1,
                        );
                        let differs = ours
                            .iter()
                            .zip(&four)
                            .position(|(a, b)| a.to_bits() != b.to_bits());
                        assert_eq!(
                            differs, None,
                            "{w}x{h}, tint {tint:?} at {gain}, sigma {sigma:?}"
                        );
                        if Planes::of(&layer, sigma.0, sigma.1).is_some() {
                            by_planes += 1;
                        }
                        compared += 1;
                    }
                }
            }
        }
        assert_eq!(compared, 4 * 6 * 2 * 8);
        // Black, grey, the green with no red or blue and the black with a
        // blue of -0.0 -- two planes each -- at the five pairs summed tap
        // by tap on both axes.
        assert_eq!(by_planes, 4 * 5 * 2 * 5);
    }

    #[test]
    fn the_planes_are_told_apart_by_their_bits() {
        let black = tinted(9, 7, [0.0, 0.0, 0.0]);
        let grey = tinted(9, 7, [0.5, 0.5, 0.5]);
        let planes =
            |layer: &[f32]| Planes::of(layer, 2.0, 2.0).map(|p| p.source);
        assert_eq!(planes(&black), Some([None, None, None, Some(3)]));
        assert_eq!(planes(&grey), Some([Some(0), Some(0), Some(0), Some(3)]));
        let signed = tinted(9, 7, [0.0, 0.0, -0.0]);
        assert_eq!(planes(&signed), Some([None, None, Some(2), Some(3)]));
        // Through the FFT on either axis, all four are kept.
        assert!(Planes::of(&black, 5.0, 2.0).is_none());
        assert!(Planes::of(&black, 2.0, 8.0).is_none());
        // Three distinct planes are blurred as four channels.
        assert!(Planes::of(&tinted(9, 7, [0.1, 0.2, 0.3]), 2.0, 2.0).is_none());
    }

    #[test]
    fn a_plane_an_ulp_from_another_is_blurred_apart() {
        // Red an ulp above green: two colours a reader would call the same,
        // and two planes of different bits, so red, green and alpha are
        // three distinct planes and the image is blurred as four channels.
        let (w, h) = (31, 23);
        let mut layer = tinted(w, h, [0.5, 0.5, 0.0]);
        for px in layer.as_chunks_mut::<4>().0 {
            px[0] = f32::from_bits(px[1].to_bits() + 1);
        }
        assert!(Planes::of(&layer, 2.0, 3.0).is_none());
        let ours = blur(layer.clone(), w, h, 2.0, 3.0);
        let four = blur_in::<f64>(layer, w, h, 2.0, 3.0);
        assert!(
            ours.iter()
                .zip(&four)
                .all(|(a, b)| a.to_bits() == b.to_bits())
        );
        // And red does not blur to green's bits, so taking one for the
        // other would show.
        let apart = ours
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| p[0].to_bits() != p[1].to_bits())
            .count();
        assert!(apart > w * h / 2, "only {apart} pixels tell red from green");
    }
}
