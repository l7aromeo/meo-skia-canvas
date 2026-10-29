//! A blurred rectangle drawn exactly.
//!
//! The Gaussian blur of a rectangle is separable and has a closed form: the
//! interval `[a, b]` blurred by `sigma` is `(erf((x - a) / k) - erf((x - b) /
//! k)) / 2` with `k = sigma * sqrt(2)`, and the rectangle is the product of
//! one such profile across and one down. Averaged over each pixel, which is
//! what a pixel of a blurred picture is, it is the difference of an
//! antiderivative -- see [`profile`]. So a rectangle that stays one on the
//! device needs no convolution at all, and its blur is exact to the
//! precision the pixels hold.
//!
//! Skia has two blurs and neither is that. Its coverage blur of a rectangle
//! is an approximation within about 5 levels of the Gaussian, held to a sigma
//! of 128; its image blur is three box blurs, and on the CPU it blurs the
//! whole layer the draw lands in -- a 200 pixel square under `blur(100px)` on
//! an 800 by 600 canvas cost about 11 milliseconds a draw that way, where
//! Skia's own rectangle mask had cost 1.3.
//!
//! On a raster surface the blur is baked into premultiplied pixels of the
//! surface's own colour type and space, in the draw's colour, and drawn as a
//! sprite: Skia draws an alpha-only image in a colour through its shader
//! pipeline at about 1.5 ns a pixel, and a sprite of the same pixels at
//! about 0.3. Baked pixels are kept by [`RectKey`], so the same rectangle
//! drawn again costs the sprite alone. On the GPU the blur is worked per
//! fragment by [`RECT_BLUR_SKSL`], which costs less there than uploading a
//! texture.

use std::sync::{
    Arc, LazyLock,
    atomic::{AtomicU64, Ordering},
};

use parking_lot::Mutex;
use skia_safe::{
    AlphaType, Bitmap, BlendMode, Canvas as SkCanvas, Color4f, ColorSpace,
    ColorType, IPoint, IRect, Image, ImageInfo, Paint, Rect, shaders,
};
#[cfg(any(feature = "metal", feature = "vulkan"))]
use skia_safe::{Data, RuntimeEffect, color_filters};

use crate::{cache_map::CacheMap, context::layer::ExactColour, started};

/// How closely the blur is drawn on a surface: how far past each edge, and
/// how near the middle's value a column has to be to count as the flat
/// interior. What an 8-bit pixel cannot hold is not worth drawing, and a
/// float one holds a great deal more.
#[derive(Clone, Copy)]
struct Precision {
    /// How many sigmas past each edge the blur is drawn.
    tail_sigmas: f64,
    /// How far a column's profile may lie from the middle's, as a fraction
    /// of the middle's, and still be drawn as part of the flat interior. A
    /// fraction rather than a distance: a blur far wider than its rectangle
    /// is faint everywhere, and a distance would call all of it flat.
    flat_tolerance: f32,
}

/// For 8-bit pixels. Beyond four sigmas the Gaussian's tail holds under
/// 3.2e-5 of the edge's weight, 0.008 of a level, and the interior is level
/// with its middle to 0.0026 of one at most, so it and the baked pixels
/// beside it round alike.
const BYTE_PRECISION: Precision = Precision {
    tail_sigmas: 4.0,
    flat_tolerance: 1e-5,
};

/// For float pixels. Beyond six sigmas the tail holds under 1e-9, and the
/// interior is level to 1e-7 of itself, both under a 32-bit float's unit in
/// the last place.
const FLOAT_PRECISION: Precision = Precision {
    tail_sigmas: 6.0,
    flat_tolerance: 1e-7,
};

impl Precision {
    /// The precision for pixels of `kind`.
    fn of(kind: ColorType) -> Self {
        match kind {
            ColorType::RGBA8888 | ColorType::BGRA8888 => BYTE_PRECISION,
            _ => FLOAT_PRECISION,
        }
    }
}

/// How many bytes of baked rectangles are kept, across every rectangle and
/// every thread. A 200 pixel square blurred by 100 bakes about 4 MB, a
/// card-sized one blurred by 4 a few KB; the page cache, which holds whole
/// pages, keeps 64 MB.
const MASK_CACHE_BYTES: usize = 16 * 1024 * 1024;

/// The largest single bake that is kept. A larger one is made, drawn and
/// dropped, so that one huge rectangle cannot empty the cache of the many
/// small ones a page repeats.
const MASK_MAX_BYTES: usize = 4 * 1024 * 1024;

/// What a fill of the cache is cut back to, as a share of
/// [`MASK_CACHE_BYTES`]: one eviction pass serves several bakes rather than
/// one, as the page cache's low-water mark does.
const MASK_CACHE_LOW_WATER: usize = MASK_CACHE_BYTES * 3 / 4;

/// `1 / sqrt(pi)`.
const FRAC_1_SQRT_PI: f64 = 0.564_189_583_547_756_3;

/// How far `G(v) = v erf(v) + e^(-v^2) / sqrt(pi)`, the antiderivative of
/// `erf`, lies above `|v|`, for `v >= 0`. Written with `erfc` so that it does
/// not subtract two numbers near `v` from each other, which is where `G`
/// itself loses its digits.
fn above_abs(v: f64) -> f64 {
    (-v * v).exp() * FRAC_1_SQRT_PI - v * libm::erfc(v)
}

/// Below this distance from an edge, in units of `sigma * sqrt(2)`, a pixel's
/// share of the edge is worked from the series of `G` rather than from `G`
/// itself. Far enough out `G(d1 / k)` and `G(d0 / k)` agree in every digit a
/// float keeps, so their difference times `k` is the rounding error of the
/// two, and a blur of 1e30 pixels would draw the unblurred rectangle. The
/// first term the series drops, `u^8 / 168`, moves a share by under 3e-8 of
/// itself at this bound, below what the `f32` profile keeps.
const SERIES_BELOW: f64 = 0.1;

/// `k (G(d1 / k) - G(d0 / k))` for distances `d0` and `d1` from an edge,
/// where `G(u) = u erf(u) + e^(-u^2) / sqrt(pi)` is the antiderivative of
/// `erf`: the edge's share of a pixel spanning those distances.
///
/// Close to the edge against `k`, from the series
/// `G(u) = (1 + u^2 - u^4 / 6 + u^6 / 30 - u^8 / 168 + ...) / sqrt(pi)`,
/// whose n-th coefficient is `(-1)^(n+1) / (n! (2n - 1))`, taken to its
/// `u^6` term; past it, as `|u|` and what `G` lies above it -- see
/// [`above_abs`].
fn edge_share(d0: f64, d1: f64, k: f64) -> f64 {
    if d0.max(d1) < SERIES_BELOW * k {
        let k2 = k * k;
        let (q0, q1) = (d0 * d0 / k2, d1 * d1 / k2);
        let term = |n: i32, c: f64| c * (q1.powi(n) - q0.powi(n));
        k * FRAC_1_SQRT_PI
            * (term(1, 1.0) + term(2, -1.0 / 6.0) + term(3, 1.0 / 30.0))
    } else {
        (d1 - d0) + k * (above_abs(d1 / k) - above_abs(d0 / k))
    }
}

/// The interval `[a, b]` blurred by `sigma`, averaged over each of `n` pixels
/// from zero: pixel `i` covers `[i, i + 1)`.
fn profile(n: usize, a: f64, b: f64, sigma: f64) -> Vec<f32> {
    let k = sigma * std::f64::consts::SQRT_2;
    let edge = |x0: f64, x1: f64, e: f64| {
        edge_share((x0 - e).abs(), (x1 - e).abs(), k)
    };
    (0..n)
        .map(|i| {
            let (x0, x1) = (i as f64, i as f64 + 1.0);
            ((edge(x0, x1, a) - edge(x0, x1, b)) / 2.0).clamp(0.0, 1.0) as f32
        })
        .collect()
}

/// One axis of a blurred rectangle on the device: where its extent starts in
/// whole pixels, the profile across it, where the profile peaks, and the run
/// about the peak that is flat.
struct Axis {
    origin: i32,
    profile: Vec<f32>,
    peak: usize,
    flat: std::ops::Range<usize>,
}

impl Axis {
    /// The axis of an edge pair `[lo, hi]` in device pixels, blurred by
    /// `sigma`, drawn to `precision`, over the whole of its reach.
    #[cfg(test)]
    fn of(lo: f32, hi: f32, sigma: f64, precision: Precision) -> Self {
        let span = Self::span(lo, hi, sigma, precision, (i32::MIN, i32::MAX));
        // SAFETY: an unbounded window holds every non-empty reach.
        Self::within(lo, hi, sigma, precision, span.expect("a reach"))
    }

    /// The axis over `span`, a window of the device as `Self::span` gives
    /// it.
    fn within(
        lo: f32,
        hi: f32,
        sigma: f64,
        precision: Precision,
        span: (i32, i32),
    ) -> Self {
        let (origin, n) = (span.0, (span.1 - span.0) as usize);
        let (a, b) = (
            f64::from(lo) - f64::from(origin),
            f64::from(hi) - f64::from(origin),
        );
        let profile = profile(n, a, b, sigma);
        // The peak, not the middle of the window: a window the clip cuts on
        // one side is not centred on the interval.
        let peak = profile
            .iter()
            .enumerate()
            .fold(
                (0, f32::MIN),
                |best, (i, v)| if *v > best.1 { (i, *v) } else { best },
            )
            .0;
        let top = profile.get(peak).copied().unwrap_or(0.0);
        let flat = |i: &usize| {
            (profile[*i] - top).abs() <= precision.flat_tolerance * top
        };
        let start = (0..peak)
            .rev()
            .take_while(|i| flat(i))
            .last()
            .unwrap_or(peak);
        let end = (peak..n)
            .take_while(|i| flat(i))
            .last()
            .map_or(peak, |i| i + 1);
        Axis {
            origin,
            profile,
            peak,
            flat: start..end,
        }
    }

    /// The whole device pixels the blur of `[lo, hi]` covers within
    /// `visible`, first and one past the last: the interval and
    /// `precision.tail_sigmas` sigmas and a pixel to each side, cut to the
    /// window. `None` where none of it is visible. Worked in `f64`, since a
    /// sigma is any finite length and its reach can pass an `i32`; a pixel
    /// outside `visible` is never drawn, so nothing past it is made.
    fn span(
        lo: f32,
        hi: f32,
        sigma: f64,
        precision: Precision,
        visible: (i32, i32),
    ) -> Option<(i32, i32)> {
        let margin = (precision.tail_sigmas * sigma).ceil() + 1.0;
        let start = (f64::from(lo).floor() - margin).max(f64::from(visible.0));
        let end = (f64::from(hi).ceil() + margin).min(f64::from(visible.1));
        (start < end).then_some((start as i32, end as i32))
    }

    /// The profile's largest value.
    fn middle(&self) -> f32 {
        self.profile.get(self.peak).copied().unwrap_or(0.0)
    }
}

/// A blurred rectangle baked for one surface's colour type and space: the
/// frame of pixels around its flat interior, as up to four sprites with
/// their offsets from the extent's origin, and the interior's coverage.
pub(crate) struct Baked {
    bands: Vec<(Image, IPoint)>,
    interior: Option<(IRect, f32)>,
    bytes: usize,
}

impl Baked {
    /// Bakes the rectangle of `across` and `down` in `color`, premultiplied
    /// in `target`'s colour space, as pixels of `target`'s colour type where
    /// that is 8-bit RGBA and as 32-bit floats otherwise.
    fn make(
        across: &Axis,
        down: &Axis,
        color: [f32; 4],
        target: &ImageInfo,
    ) -> Option<Self> {
        let (w, h) = (across.profile.len(), down.profile.len());
        let space = target.color_space();
        let kind = match target.color_type() {
            kind @ (ColorType::RGBA8888 | ColorType::BGRA8888) => kind,
            _ => ColorType::RGBAF32,
        };
        let bytes = matches!(kind, ColorType::RGBA8888 | ColorType::BGRA8888);
        let interior =
            (!across.flat.is_empty() && !down.flat.is_empty()).then(|| {
                let flat = IRect::new(
                    across.flat.start as i32,
                    down.flat.start as i32,
                    across.flat.end as i32,
                    down.flat.end as i32,
                );
                match bytes {
                    true => widened(flat, across, down, color),
                    false => flat,
                }
            });
        // On 8-bit pixels, the columns and rows every pixel of which bakes to
        // a zero byte are left out: they draw nothing, and on a wide
        // rectangle at a small sigma they were a sixth of what was blitted.
        //
        // A pixel bakes to `(c * p * q * 255 + 0.5) as u8` for each channel
        // `c` -- see `bake` -- which is zero exactly when `c * p * q * 255`
        // is under one half. Every premultiplied channel is at most the
        // alpha, and `q` at most the other profile's largest value, so a
        // column whose `p` keeps `alpha * p * q_max * 255` under one half is
        // zero in every row and every channel, and is not drawn. That is the
        // bake's own rounding, so the trimmed bake draws what the whole one
        // would, to the byte. Float pixels hold what would round to zero
        // here, and keep the whole reach.
        let reach = match kind {
            ColorType::RGBA8888 | ColorType::BGRA8888 => {
                let lit = |profile: &[f32], other: f32| {
                    let floor = 0.5 / (255.0 * color[3] * other);
                    let first = profile.iter().position(|v| *v >= floor);
                    let last = profile.iter().rposition(|v| *v >= floor);
                    first.zip(last).map(|(a, b)| (a as i32, b as i32 + 1))
                };
                match (
                    lit(&across.profile, down.middle()),
                    lit(&down.profile, across.middle()),
                ) {
                    (Some((l, r)), Some((t, b))) => IRect::new(l, t, r, b),
                    _ => IRect::new_empty(),
                }
            }
            _ => IRect::new(0, 0, w as i32, h as i32),
        };
        // The frame: everything lit but the interior, as the bands above and
        // below it and the two beside it.
        let regions: Vec<IRect> = match interior {
            None => vec![reach],
            Some(r) => vec![
                IRect::new(reach.left, reach.top, reach.right, r.top),
                IRect::new(reach.left, r.bottom, reach.right, reach.bottom),
                IRect::new(reach.left, r.top, r.left, r.bottom),
                IRect::new(r.right, r.top, reach.right, r.bottom),
            ],
        };
        let bands = regions
            .into_iter()
            .filter(|r| !r.is_empty())
            .map(|r| {
                let info = ImageInfo::new(
                    r.size(),
                    kind,
                    AlphaType::Premul,
                    space.clone(),
                );
                baked_image(&info, |pixels, stride| {
                    bake(across, down, r, color, kind, pixels, stride);
                })
                .map(|image| (image, IPoint::new(r.left, r.top)))
            })
            .collect::<Option<Vec<_>>>()?;
        let bytes = bands
            .iter()
            .map(|(image, _)| image.image_info().compute_min_byte_size())
            .sum();
        // An 8-bit interior whose bytes are full coverage's is filled at
        // full coverage, to the same bytes: Skia fills an opaque colour as a
        // memset and a nearly opaque one by blending each pixel, about six
        // times as long at 600 by 400.
        let coverage = across.middle() * down.middle();
        let byte = |c: f32| color.map(|v| (v * c * 255.0 + 0.5) as u8);
        let eight_bit =
            matches!(kind, ColorType::RGBA8888 | ColorType::BGRA8888);
        let coverage = match eight_bit && byte(coverage) == byte(1.0) {
            true => 1.0,
            false => coverage,
        };
        Some(Baked {
            bands,
            interior: interior.map(|r| (r, coverage)),
            bytes,
        })
    }

    /// Draws the rectangle with its extent's origin at `origin` in device
    /// pixels, the interior in `color`.
    fn draw(
        &self,
        canvas: &SkCanvas,
        origin: IPoint,
        color: Color4f,
        exact: Option<&ExactColour>,
    ) {
        self.bands.iter().for_each(|(image, at)| {
            canvas.draw_image(image, origin + *at, None);
        });
        if let Some((rect, coverage)) = self.interior {
            let fill =
                Color4f::new(color.r, color.g, color.b, color.a * coverage);
            // UPSTREAM: skia-safe 0.153.3 -- unfiled -- worked around
            // Re-check: cargo test a_trimmed_bake_is_the_whole_product
            // with the fill given as the paint's colour.
            //
            // The fill goes through a colour shader so that it rounds once,
            // as the bake beside it does. Given as the paint's colour, Skia
            // blits it onto N32 pixels by rounding the unpremultiplied
            // colour to bytes and premultiplying those, a level off the
            // bake. The shader's paint keeps an alpha of one, so a faint
            // interior on float pixels is not dropped either -- see the
            // UPSTREAM note on `context::faint::Faint::drawable`.
            let mut paint = Paint::default();
            match exact.and_then(ExactColour::shader) {
                // On a float page, where a paint's alpha is not dropped.
                Some(shader) => {
                    paint.set_shader(shader).set_alpha_f(fill.a);
                }
                None => {
                    paint.set_shader(shaders::color_in_space(
                        fill,
                        ColorSpace::new_srgb(),
                    ));
                }
            }
            paint.set_anti_alias(false);
            canvas.draw_irect(rect.with_offset(origin), &paint);
        }
    }
}

/// The interior `flat` grown outward while every pixel it takes in bakes to
/// the same bytes as the interior's middle in `color`, on 8-bit pixels. Each
/// such pixel is drawn by the fill at no further error than rounding gives,
/// since it rounds where the exact value does; past the tolerance-flat run
/// they were baked and blitted all the same, a band a sigma and a half deep
/// inside each edge.
///
/// A row grows against the profile down's middle and a column against the
/// profile across's, and then the corners are checked against both
/// together. That the corners speak for the whole interior rests on each
/// profile being unimodal: an interval's indicator convolved with a
/// Gaussian rises to the interval's centre and falls past it, and averaging
/// over a pixel keeps that. So within the interior each profile is least at
/// the interior's edges, the product least at its corners, and an interior
/// whose corners bake to the middle's bytes bakes to them throughout -- the
/// middle is the largest value and every pixel lies between it and a
/// corner's. A corner that does not shrinks it a pixel all round until it
/// does.
fn widened(flat: IRect, across: &Axis, down: &Axis, color: [f32; 4]) -> IRect {
    let byte = |c: f32| color.map(|v| (v * c * 255.0 + 0.5) as u8);
    let (pc, qc) = (across.middle(), down.middle());
    let middle = byte(pc * qc);
    let grow = |profile: &[f32], other: f32, start: i32, end: i32| {
        let same = |i: i32| byte(profile[i as usize] * other) == middle;
        let start = (0..start)
            .rev()
            .take_while(|i| same(*i))
            .last()
            .unwrap_or(start);
        let end = (end..profile.len() as i32)
            .take_while(|i| same(*i))
            .last()
            .map_or(end, |i| i + 1);
        (start, end)
    };
    let (mut l, mut r) = grow(&across.profile, qc, flat.left, flat.right);
    let (mut t, mut b) = grow(&down.profile, pc, flat.top, flat.bottom);
    let corners_match = |l: i32, t: i32, r: i32, b: i32| {
        [(l, t), (r - 1, t), (l, b - 1), (r - 1, b - 1)]
            .into_iter()
            .all(|(x, y)| {
                byte(across.profile[x as usize] * down.profile[y as usize])
                    == middle
            })
    };
    while (l < flat.left || r > flat.right || t < flat.top || b > flat.bottom)
        && !corners_match(l, t, r, b)
    {
        l = (l + 1).min(flat.left);
        r = (r - 1).max(flat.right);
        t = (t + 1).min(flat.top);
        b = (b - 1).max(flat.bottom);
    }
    IRect::new(l, t, r, b)
}

/// An image of `info` whose pixels `fill` writes, given them and their row
/// stride in bytes. Written in place and handed to Skia without a copy: a
/// bake is up to several megabytes, and copying it again cost as much as
/// making it.
fn baked_image(
    info: &ImageInfo,
    fill: impl FnOnce(&mut [u8], usize),
) -> Option<Image> {
    let mut bitmap = Bitmap::new();
    bitmap.try_alloc_pixels_info(info, None).then_some(())?;
    let mut pixmap = bitmap.peek_pixels()?;
    let stride = pixmap.row_bytes();
    fill(pixmap.bytes_mut()?, stride);
    bitmap.set_immutable();
    started::raster_from_bitmap(&bitmap)
}

/// Writes the pixels of `region` of the rectangle, `color` times the two
/// profiles, as `kind`, into `pixels` of `stride` bytes a row. Each row is a
/// precomputed row of `color` times the profile across, times the profile
/// down at that row: one multiply a channel.
fn bake(
    across: &Axis,
    down: &Axis,
    region: IRect,
    color: [f32; 4],
    kind: ColorType,
    pixels: &mut [u8],
    stride: usize,
) {
    let cols = &across.profile[region.left as usize..region.right as usize];
    let rows = &down.profile[region.top as usize..region.bottom as usize];
    match kind {
        ColorType::RGBA8888 | ColorType::BGRA8888 => {
            let [r, g, b, a] = color.map(|c| c * 255.0);
            let order = match kind {
                ColorType::BGRA8888 => [b, g, r, a],
                _ => [r, g, b, a],
            };
            let line: Vec<f32> =
                cols.iter().flat_map(|p| order.map(|c| c * p)).collect();
            pixels.chunks_mut(stride).zip(rows).for_each(|(out, q)| {
                out.iter_mut()
                    .zip(&line)
                    .for_each(|(o, v)| *o = (v * q + 0.5) as u8);
            });
        }
        _ => {
            let line: Vec<f32> =
                cols.iter().flat_map(|p| color.map(|c| c * p)).collect();
            pixels.chunks_mut(stride).zip(rows).for_each(|(out, q)| {
                out.as_chunks_mut::<4>().0.iter_mut().zip(&line).for_each(
                    |(o, v)| {
                        o.copy_from_slice(&(v * q).to_ne_bytes());
                    },
                );
            });
        }
    }
}

/// What a baked rectangle is kept by: everything that changes its pixels.
/// The whole-pixel part of its position is not here, since it only moves the
/// sprite.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct RectKey {
    /// The device rectangle's width and height, as bits.
    size: [u32; 2],
    /// The window of it that is made, its first column and one past its
    /// last, then its first row and one past its last, each from the
    /// rectangle's whole-pixel left or top edge. The blur's whole reach
    /// unless the clip cuts it, and then only what lies inside the clip.
    window: [i64; 4],
    /// The fractional part of its left and top edges, as bits.
    fraction: [u32; 2],
    /// The blur's standard deviation in device pixels, as bits.
    sigma: u32,
    /// The draw's colour, unpremultiplied sRGB with the global alpha in it,
    /// as bits.
    color: [u32; 4],
    /// The colour held exactly in the surface's space, where the draw has
    /// one, as bits -- see [`draw_rect`].
    exact: Option<[u32; 3]>,
    /// The surface's colour type: a bake for 8-bit pixels holds less than a
    /// float surface shows.
    kind: i32,
    /// The surface's colour space, by Skia's two hashes of it. A bake is
    /// tagged with the space it was made in, so one drawn on a surface of
    /// another space is still the right colour -- Skia converts it while
    /// drawing -- but no longer a sprite. Filing by space keeps each
    /// surface's bakes in its own, and a collision of both hashes costs
    /// that speed and not a pixel.
    space: Option<(u32, u32)>,
}

struct Entry {
    baked: Arc<Baked>,
    used: u64,
}

#[derive(Default)]
struct MaskCache {
    entries: CacheMap<RectKey, Entry>,
    bytes: usize,
}

/// Every baked rectangle kept, for every thread: pages are played back on
/// rayon workers.
static MASKS: LazyLock<Mutex<MaskCache>> = LazyLock::new(Mutex::default);

/// Ticks once per use, to order entries for eviction.
static MASK_USES: AtomicU64 = AtomicU64::new(0);

/// Drops every baked rectangle. Called with the page cache's release, when
/// rendering has been idle.
pub(crate) fn release_cached_masks() {
    let mut cache = MASKS.lock();
    cache.entries.clear();
    cache.bytes = 0;
}

impl MaskCache {
    fn get(&mut self, key: &RectKey) -> Option<Arc<Baked>> {
        let entry = self.entries.get_mut(key)?;
        entry.used = MASK_USES.fetch_add(1, Ordering::Relaxed);
        Some(entry.baked.clone())
    }

    fn put(&mut self, key: RectKey, baked: Arc<Baked>) {
        if baked.bytes > MASK_MAX_BYTES {
            return;
        }
        let used = MASK_USES.fetch_add(1, Ordering::Relaxed);
        self.bytes += baked.bytes;
        if let Some(old) = self.entries.insert(key, Entry { baked, used }) {
            self.bytes -= old.baked.bytes;
        }
        if self.bytes > MASK_CACHE_BYTES {
            let mut order: Vec<(u64, RectKey, usize)> = self
                .entries
                .iter()
                .map(|(k, e)| (e.used, *k, e.baked.bytes))
                .collect();
            order.sort_unstable_by_key(|(used, _, _)| *used);
            for (_, key, bytes) in order {
                if self.bytes <= MASK_CACHE_LOW_WATER {
                    break;
                }
                self.entries.remove(&key);
                self.bytes -= bytes;
            }
        }
    }
}

/// The blurred rectangle, per fragment, in device pixels: `rect` is the
/// device rectangle's left, top, right and bottom, and `k` is
/// `sigma * sqrt(2)`. Its coverage goes out as a premultiplied white, which
/// the paint's colour filter turns into the draw's colour.
///
/// `erfc` is Numerical Recipes' Chebyshev fit, whose fractional error is
/// below 1.2e-7 everywhere -- the precision of the `float` it runs in. Each
/// axis's pixel average is worked one of three ways, by what a `float` keeps
/// of it:
///
/// - Close to both edges against `k`, from the series, as [`edge_share`] works
///   it and for the same reason, which a `float` meets far sooner than a
///   `double` does. Each power's difference is taken as `q1 - q0` times a sum,
///   with `q1 - q0` itself a product: the squares of neighbouring distances
///   agree in most of a `float`'s digits, and their difference was otherwise
///   off by 3e-5 of itself.
/// - For a blur under four pixels, from the antiderivative, as [`edge_share`]
///   works it past the series.
/// - Otherwise by three-point Gauss-Legendre over `erf` across the pixel, each
///   difference of two `erf`s taken as one of two `erfc`s on the side where
///   both are small. The antiderivative's difference across a pixel cancels by
///   about `k`, and in a `float` that was 1.2e-4 off at a sigma of 700 and
///   1.6e-3 at 1e4; the quadrature stays within 2e-7 from a sigma of 4 up, and
///   the antiderivative within 5e-7 below it, where the quadrature's own error
///   grows -- 4e-6 at a sigma of 1.
#[cfg(any(feature = "metal", feature = "vulkan"))]
const RECT_BLUR_SKSL: &str = "
uniform float4 rect;
uniform float k;

const float FRAC_1_SQRT_PI = 0.5641895835;

float erfc_nr(float z) {
    float t = 1.0 / (1.0 + 0.5 * z);
    float p = -0.82215223 + t * 0.17087277;
    p = 1.48851587 + t * p;
    p = -1.13520398 + t * p;
    p = 0.27886807 + t * p;
    p = -0.18628806 + t * p;
    p = 0.09678418 + t * p;
    p = 0.37409196 + t * p;
    p = 1.00002368 + t * p;
    p = -1.26551223 + t * p;
    return t * exp(-z * z + p);
}

float above_abs(float v) {
    return exp(-v * v) * FRAC_1_SQRT_PI - v * erfc_nr(v);
}

float edge(float x0, float x1, float e) {
    float d0 = abs(x0 - e);
    float d1 = abs(x1 - e);
    if (max(d0, d1) < 0.1 * k) {
        float q0 = (d0 / k) * (d0 / k);
        float q1 = (d1 / k) * (d1 / k);
        float dq = ((d1 - d0) / k) * ((d1 + d0) / k);
        float s = dq * (1.0
            - (q1 + q0) / 6.0
            + (q1 * q1 + q1 * q0 + q0 * q0) / 30.0);
        return k * FRAC_1_SQRT_PI * s;
    }
    return (d1 - d0) + k * (above_abs(d1 / k) - above_abs(d0 / k));
}

// erf(z1) - erf(z2) for z1 > z2, never subtracting two values near 1.
float erf_diff(float z1, float z2) {
    if (z2 >= 0.0) {
        return erfc_nr(z2) - erfc_nr(z1);
    }
    if (z1 <= 0.0) {
        return erfc_nr(-z1) - erfc_nr(-z2);
    }
    return 2.0 - erfc_nr(z1) - erfc_nr(-z2);
}

// The Gauss-Legendre nodes either side of a pixel's centre, sqrt(3/5) / 2.
const float NODE = 0.3872983346;

// From this k, a sigma of 4, the quadrature is closer than the
// antiderivative.
const float QUADRATURE_FROM = 5.656854249;

float profile(float x, float a, float b) {
    float x0 = x - 0.5;
    float x1 = x + 0.5;
    float far = max(max(abs(x0 - a), abs(x1 - a)), max(abs(x0 - b), abs(x1 - b)));
    if (far < 0.1 * k || k < QUADRATURE_FROM) {
        return clamp((edge(x0, x1, a) - edge(x0, x1, b)) * 0.5, 0.0, 1.0);
    }
    float s = (5.0 / 18.0) * erf_diff((x - NODE - a) / k, (x - NODE - b) / k)
        + (8.0 / 18.0) * erf_diff((x - a) / k, (x - b) / k)
        + (5.0 / 18.0) * erf_diff((x + NODE - a) / k, (x + NODE - b) / k);
    return clamp(s * 0.5, 0.0, 1.0);
}

half4 main(float2 p) {
    return half4(profile(p.x, rect.x, rect.z) * profile(p.y, rect.y, rect.w));
}
";

#[cfg(any(feature = "metal", feature = "vulkan"))]
thread_local! {
    /// [`RECT_BLUR_SKSL`], compiled once per thread: a `RuntimeEffect` is
    /// neither `Send` nor `Sync`.
    static RECT_BLUR: Option<RuntimeEffect> =
        RuntimeEffect::make_for_shader(RECT_BLUR_SKSL, None).ok();
}

/// Whether [`draw_rect`] draws on `canvas`: a raster surface or a GPU one.
/// A recording or a vector document has no pixels for the blur to be made
/// in, and takes the blur its caller would otherwise make.
pub(crate) fn draws_on(canvas: &SkCanvas) -> bool {
    on_gpu(canvas) || draws_on_cpu(canvas)
}

/// Whether `canvas` is a raster surface on the CPU: one with pixels that no
/// GPU draws, which a recording and a vector document are not.
pub(crate) fn draws_on_cpu(canvas: &SkCanvas) -> bool {
    !on_gpu(canvas) && canvas.image_info().color_type() != ColorType::Unknown
}

/// Whether `canvas` draws on the GPU. A build without a GPU backend has no
/// such canvas.
#[cfg(any(feature = "metal", feature = "vulkan"))]
fn on_gpu(canvas: &SkCanvas) -> bool {
    canvas.recording_context().is_some()
}

#[cfg(not(any(feature = "metal", feature = "vulkan")))]
fn on_gpu(_canvas: &SkCanvas) -> bool {
    false
}

/// Draws `rect`, already in device pixels, blurred by `sigma` device pixels
/// in `color` -- unpremultiplied sRGB -- onto `canvas`, whose matrix is
/// expected to be the identity and on which [`draws_on`] is true. Returns
/// `false`, having drawn nothing, where a surface or a shader could not be
/// made.
///
/// With `exact`, the colour is that, held in the page's own space, at
/// `color`'s alpha: what a page that keeps a recording for pixels paints a
/// flat colour in -- see [`ExactColour`]. Converted from sRGB instead, it
/// lands on the 64-ulp grid Skia's conversion leaves it on. On the CPU the
/// exact colour is baked as the surface's own components, so it is taken
/// only on a surface in its space: a page played onto another, as onto the
/// eight-bit sRGB raster an SVG embeds, is painted `color` converted into
/// that one.
pub(crate) fn draw_rect(
    canvas: &SkCanvas,
    rect: Rect,
    sigma: f32,
    color: Color4f,
    exact: Option<&ExactColour>,
) -> bool {
    let sigma = f64::from(sigma);
    #[cfg(any(feature = "metal", feature = "vulkan"))]
    if on_gpu(canvas) {
        return draw_on_gpu(canvas, rect, sigma, color, exact);
    }
    let target = canvas.image_info();
    let space = target.color_space();
    let exact = exact.filter(|exact| space.as_ref() == Some(&exact.space));
    let key = RectKey {
        window: [0; 4],
        size: [rect.width().to_bits(), rect.height().to_bits()],
        fraction: [rect.left.fract().to_bits(), rect.top.fract().to_bits()],
        sigma: (sigma as f32).to_bits(),
        color: [color.r, color.g, color.b, color.a].map(f32::to_bits),
        exact: exact.map(|exact| exact.rgb.map(f32::to_bits)),
        kind: target.color_type() as i32,
        space: space
            .as_ref()
            .map(|s| (s.to_xyzd50_hash().0, s.transfer_fn_hash())),
    };
    let precision = Precision::of(target.color_type());
    // Only what lands inside the clip is made: past it nothing is drawn,
    // and a blur's reach is any length.
    let clip = canvas.device_clip_bounds().unwrap_or_default();
    let (Some(cols), Some(rows)) = (
        Axis::span(
            rect.left,
            rect.right,
            sigma,
            precision,
            (clip.left, clip.right),
        ),
        Axis::span(
            rect.top,
            rect.bottom,
            sigma,
            precision,
            (clip.top, clip.bottom),
        ),
    ) else {
        return true;
    };
    let key = RectKey {
        window: [
            i64::from(cols.0) - rect.left.floor() as i64,
            i64::from(cols.1) - rect.left.floor() as i64,
            i64::from(rows.0) - rect.top.floor() as i64,
            i64::from(rows.1) - rect.top.floor() as i64,
        ],
        ..key
    };
    let origin = IPoint::new(cols.0, rows.0);
    // The profiles only on a miss: a kept bake is drawn without them.
    let kept = MASKS.lock().get(&key);
    let baked = match kept {
        Some(baked) => baked,
        None => {
            let across =
                Axis::within(rect.left, rect.right, sigma, precision, cols);
            let down =
                Axis::within(rect.top, rect.bottom, sigma, precision, rows);
            let premul = match exact {
                Some(exact) => {
                    let [r, g, b] = exact.rgb.map(|c| c * color.a);
                    Some([r, g, b, color.a])
                }
                None => premultiplied(color, &target),
            };
            let Some(premul) = premul else {
                return false;
            };
            let Some(baked) = Baked::make(&across, &down, premul, &target)
            else {
                return false;
            };
            let baked = Arc::new(baked);
            MASKS.lock().put(key, baked.clone());
            baked
        }
    };
    baked.draw(canvas, origin, color, exact);
    true
}

/// `color` premultiplied in `target`'s colour space, as Skia converts it: one
/// pixel of that space drawn in it and read back as floats.
fn premultiplied(color: Color4f, target: &ImageInfo) -> Option<[f32; 4]> {
    let info = ImageInfo::new(
        (1, 1),
        ColorType::RGBAF32,
        AlphaType::Premul,
        target.color_space(),
    );
    let mut pixel = started::raster(&info, None, None)?;
    pixel.canvas().draw_color(color, BlendMode::Src);
    let mut bytes = [0u8; 16];
    pixel
        .read_pixels(&info, &mut bytes, 16, (0, 0))
        .then_some(())?;
    let channel = |i: usize| {
        f32::from_ne_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]])
    };
    Some([channel(0), channel(4), channel(8), channel(12)])
}

/// [`draw_rect`] on a GPU surface: the blur worked per fragment over the
/// rectangle's extent.
#[cfg(any(feature = "metal", feature = "vulkan"))]
fn draw_on_gpu(
    canvas: &SkCanvas,
    rect: Rect,
    sigma: f64,
    color: Color4f,
    exact: Option<&ExactColour>,
) -> bool {
    let k = (sigma * std::f64::consts::SQRT_2) as f32;
    let uniforms: Vec<u8> = [rect.left, rect.top, rect.right, rect.bottom, k]
        .into_iter()
        .flat_map(f32::to_ne_bytes)
        .collect();
    let shader = RECT_BLUR.with(|effect| {
        effect
            .as_ref()
            .and_then(|e| e.make_shader(Data::new_copy(&uniforms), &[], None))
    });
    let Some(shader) = shader else {
        return false;
    };
    let reach = (FLOAT_PRECISION.tail_sigmas * sigma).ceil() as f32 + 1.0;
    let mut paint = Paint::default();
    match exact.and_then(ExactColour::shader) {
        // The exact colour where the coverage is: `SrcIn` keeps the
        // colour's shader at the coverage's alpha.
        Some(colour) => {
            paint
                .set_shader(shaders::blend(BlendMode::SrcIn, shader, colour))
                .set_alpha_f(color.a);
        }
        None => {
            paint.set_shader(shader);
            paint.set_color_filter(color_filters::blend_with_color_space(
                color,
                None,
                BlendMode::SrcIn,
            ));
        }
    }
    canvas.draw_rect(rect.with_outset((reach, reach)), &paint);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The profile against the blurred interval integrated over each pixel
    /// numerically: the midpoint rule over `erf`, with no antiderivative.
    #[test]
    fn a_profile_is_the_blurred_interval_averaged_over_each_pixel() {
        let steps = 4096;
        for (a, b, sigma) in
            [(10.25, 30.5, 2.0), (40.0, 60.0, 9.5), (130.0, 134.0, 30.0)]
        {
            let k = sigma * std::f64::consts::SQRT_2;
            let n = (b + 5.0 * sigma) as usize;
            let got = profile(n, a, b, sigma);
            for (i, value) in got.iter().enumerate() {
                let mean = (0..steps)
                    .map(|s| {
                        let x = i as f64 + (s as f64 + 0.5) / steps as f64;
                        (libm::erf((x - a) / k) - libm::erf((x - b) / k)) / 2.0
                    })
                    .sum::<f64>()
                    / steps as f64;
                assert!(
                    (f64::from(*value) - mean).abs() < 1e-6,
                    "[{a}, {b}] by {sigma}, pixel {i}: {value} against {mean}"
                );
            }
        }
    }

    /// Past the reach of the closed form, where its two terms cancel, the
    /// profile is still the averaged blur to a part in a million of itself.
    #[test]
    fn a_profile_blurred_past_any_length_is_its_faint_average() {
        let steps = 4096;
        let (a, b) = (10.0, 30.0);
        for sigma in [400.0, 1e3, 1e6, 1e12] {
            let k = sigma * std::f64::consts::SQRT_2;
            let got = profile(64, a, b, sigma);
            for (i, value) in got.iter().enumerate() {
                let mean = (0..steps)
                    .map(|s| {
                        let x = i as f64 + (s as f64 + 0.5) / steps as f64;
                        (libm::erf((x - a) / k) - libm::erf((x - b) / k)) / 2.0
                    })
                    .sum::<f64>()
                    / steps as f64;
                assert!(
                    (f64::from(*value) - mean).abs() < 1e-6 * mean,
                    "[{a}, {b}] by {sigma}, pixel {i}: {value} against {mean}"
                );
            }
        }
    }

    /// The interval `[a, b]` blurred with `k = sigma * sqrt(2)`, averaged
    /// over pixel `i` by the midpoint rule over `erf` -- no antiderivative --
    /// with each difference of two `erf`s taken from `erfc` where both lie
    /// near 1, so that a faint pixel keeps its digits in `f64`. The rule's
    /// error is at most `max |f''| / (24 steps^2)`, and `|f''|` at most
    /// `0.97 / k^2`: under 9e-7 at a sigma of 0.3, and smaller for every
    /// larger one.
    #[cfg(any(feature = "metal", feature = "vulkan"))]
    fn pixel_mean(i: usize, a: f64, b: f64, k: f64) -> f64 {
        let steps = 512;
        let difference = |z1: f64, z2: f64| match (z1, z2) {
            (_, z2) if z2 >= 0.0 => libm::erfc(z2) - libm::erfc(z1),
            (z1, _) if z1 <= 0.0 => libm::erfc(-z1) - libm::erfc(-z2),
            _ => libm::erf(z1) - libm::erf(z2),
        };
        (0..steps)
            .map(|s| {
                let x = i as f64 + (s as f64 + 0.5) / steps as f64;
                difference((x - a) / k, (x - b) / k) / 2.0
            })
            .sum::<f64>()
            / steps as f64
    }

    /// [`RECT_BLUR_SKSL`]'s first `n` pixels along a row, for the interval
    /// `[a, b]` across and `[top, bottom]` down, blurred with `k`. Skia runs
    /// a runtime effect on a raster surface too, in 32-bit floats, so this
    /// reaches every branch of it without a GPU: Ganesh rounds a draw's
    /// source to eight bits, which no float a shader gives survives, and a
    /// float canvas does not composite there -- see
    /// `RenderingEngine::can_composite`.
    #[cfg(any(feature = "metal", feature = "vulkan"))]
    fn shader_row(
        n: usize,
        across: [f32; 2],
        down: [f32; 2],
        k: f64,
    ) -> Vec<f32> {
        let effect = RuntimeEffect::make_for_shader(RECT_BLUR_SKSL, None)
            .expect("the shader compiles");
        let uniforms: Vec<u8> =
            [across[0], down[0], across[1], down[1], k as f32]
                .into_iter()
                .flat_map(f32::to_ne_bytes)
                .collect();
        let mut paint = Paint::default();
        paint.set_shader(
            effect
                .make_shader(Data::new_copy(&uniforms), &[], None)
                .expect("a shader"),
        );
        paint.set_blend_mode(BlendMode::Src);
        let info = ImageInfo::new(
            (n as i32, 1),
            ColorType::RGBAF32,
            AlphaType::Premul,
            None,
        );
        let mut surface =
            started::raster(&info, None, None).expect("a surface");
        surface.canvas().draw_paint(&paint);
        let mut bytes = vec![0u8; n * 16];
        assert!(surface.read_pixels(&info, &mut bytes, n * 16, (0, 0)));
        bytes
            .as_chunks::<16>()
            .0
            .iter()
            .map(|p| f32::from_ne_bytes([p[12], p[13], p[14], p[15]]))
            .collect()
    }

    /// Whether every distance from pixel `i` to `a` and to `b` is under the
    /// shader's series bound, `0.1 k`.
    #[cfg(any(feature = "metal", feature = "vulkan"))]
    fn near_both(i: usize, a: f32, b: f32, k: f64) -> bool {
        [a, b].into_iter().all(|e| {
            let e = f64::from(e);
            let (d0, d1) = ((i as f64 - e).abs(), (i as f64 + 1.0 - e).abs());
            d0.max(d1) < 0.1 * k
        })
    }

    /// How far the shader's series may lie from the exact share, as a
    /// fraction of it: four units in the last place of a `float`, its own
    /// arithmetic. The series' smallest term moves a share by 1e-5 at the
    /// bound, and the unfactored difference of squares lay 3e-5 off.
    #[cfg(any(feature = "metal", feature = "vulkan"))]
    const SHADER_SERIES_TOLERANCE: f64 = 2e-6;

    /// The shader's series against [`pixel_mean`] on the pixels close
    /// enough to both edges against `k` that each takes the series. At a
    /// sigma of 640 the distances reach the series' bound, where its `u^4`
    /// and `u^6` terms move a share by up to 1e-4 and 1e-5 of itself; past
    /// 1e4 the other forms cancel to nothing and the series alone draws the
    /// faint blur.
    #[cfg(any(feature = "metal", feature = "vulkan"))]
    #[test]
    fn the_shader_takes_a_share_near_its_edge_from_the_series() {
        let n = 200;
        // The pixels' row, `[0, 1)`, sits inside the interval down.
        let (a, b, top, bottom) = (10.25f32, 90.5f32, -30.0f32, 30.0f32);
        for sigma in [640.0, 700.0, 1e4, 1e6, 1e12] {
            let k = sigma * std::f64::consts::SQRT_2;
            let row = shader_row(n, [a, b], [top, bottom], k);
            let down = pixel_mean(0, f64::from(top), f64::from(bottom), k);
            let pixels: Vec<usize> =
                (0..n).filter(|i| near_both(*i, a, b, k)).collect();
            assert!(pixels.len() >= n / 2, "{sigma}: {} pixels", pixels.len());
            for i in pixels {
                let want = pixel_mean(i, f64::from(a), f64::from(b), k) * down;
                assert!(
                    (f64::from(row[i]) - want).abs()
                        < SHADER_SERIES_TOLERANCE * want,
                    "sigma {sigma}, pixel {i}: {} against {want}",
                    row[i]
                );
            }
        }
    }

    /// How far the shader may lie from the exact share past the series, on
    /// Skia's CPU. Its raster pipeline takes `exp` from `approx_pow2`, a
    /// rational fit good to about 1e-5 of itself, and that sets what the CPU
    /// gives: 4.4e-5 at most across the sigmas below, where the same
    /// arithmetic with an exact `exp` is within 5e-7. The antiderivative the
    /// quadrature replaced lies 7.5e-4 off at a sigma of 100 and 6e-2 at 1e4
    /// here, and the quadrature on its own 9e-4 off at a sigma of 0.3.
    #[cfg(any(feature = "metal", feature = "vulkan"))]
    const SHADER_FAR_TOLERANCE: f64 = 1e-4;

    /// The shader against [`pixel_mean`] on every pixel of the blur's reach
    /// that does not take the series: from the antiderivative under a sigma
    /// of 4 and by quadrature from there, across a rectangle narrower than
    /// a pixel's reach, one a pixel wide and one far wider than the blur.
    #[cfg(any(feature = "metal", feature = "vulkan"))]
    #[test]
    fn the_shader_works_a_share_away_from_its_edge_by_quadrature() {
        let n = 4096;
        for sigma in [0.3, 1.0, 2.0, 4.0, 8.0, 40.0, 100.0, 700.0, 3000.0, 1e4]
        {
            let k = sigma * std::f64::consts::SQRT_2;
            let reach = 6.0 * sigma + 2.0;
            for (a, b) in
                [(1000.25f32, 1080.5f32), (1100.5, 1101.5), (1000.0, 3000.0)]
            {
                let row = shader_row(n, [a, b], [-1e6, 1e6], k);
                let pixels: Vec<usize> = (0..n)
                    .filter(|i| {
                        let x = *i as f64;
                        x + 1.0 >= f64::from(a) - reach
                            && x <= f64::from(b) + reach
                            && !near_both(*i, a, b, k)
                    })
                    .collect();
                assert!(!pixels.is_empty(), "sigma {sigma}, [{a}, {b}]");
                for i in pixels {
                    let want = pixel_mean(i, f64::from(a), f64::from(b), k);
                    assert!(
                        (f64::from(row[i]) - want).abs() < SHADER_FAR_TOLERANCE,
                        "sigma {sigma}, [{a}, {b}], pixel {i}: {} against {want}",
                        row[i]
                    );
                }
            }
        }
    }

    /// A bake of a `size` square, blurred by `sigma`, for float pixels: its
    /// whole extent at 16 bytes a pixel, so its size is known ahead.
    fn baked(size: f32, sigma: f64) -> Arc<Baked> {
        let target =
            ImageInfo::new((1, 1), ColorType::RGBAF32, AlphaType::Premul, None);
        let across = Axis::of(10.0, 10.0 + size, sigma, FLOAT_PRECISION);
        let down = Axis::of(10.0, 10.0 + size, sigma, FLOAT_PRECISION);
        Arc::new(
            Baked::make(&across, &down, [0.0, 0.0, 0.0, 1.0], &target)
                .expect("a bake"),
        )
    }

    fn key(n: u32) -> RectKey {
        RectKey {
            window: [0; 4],
            size: [n, n],
            fraction: [0, 0],
            sigma: 0,
            color: [0; 4],
            exact: None,
            kind: 0,
            space: None,
        }
    }

    #[test]
    fn the_mask_cache_keeps_to_its_bytes_and_keeps_the_latest() {
        let mut cache = MaskCache::default();
        // Each about 2 MB: a 120 pixel square blurred by 30, at 16 bytes a
        // pixel.
        let one = baked(120.0, 30.0);
        assert!(
            one.bytes > MASK_CACHE_BYTES / 16 && one.bytes <= MASK_MAX_BYTES
        );
        for n in 0..40 {
            cache.put(key(n), one.clone());
            assert!(
                cache.bytes <= MASK_CACHE_BYTES,
                "{} bytes after {n}",
                cache.bytes
            );
            assert_eq!(cache.bytes, cache.entries.len() * one.bytes);
            assert!(cache.get(&key(n)).is_some(), "the latest, {n}, is kept");
        }
        // Cut back to the low-water mark rather than by one: the oldest went.
        assert!(cache.get(&key(0)).is_none());
    }

    #[test]
    fn the_mask_cache_does_not_keep_one_bake_past_its_cap() {
        let mut cache = MaskCache::default();
        let huge = baked(400.0, 200.0);
        assert!(huge.bytes > MASK_MAX_BYTES, "{} bytes", huge.bytes);
        cache.put(key(1), huge);
        assert!(cache.get(&key(1)).is_none());
        assert_eq!(cache.bytes, 0);
    }

    #[test]
    fn the_idle_release_empties_the_mask_cache() {
        // The shared cache, which other tests fill as they draw; only that a
        // release empties it is asked here.
        MASKS.lock().put(key(u32::MAX), baked(20.0, 2.0));
        release_cached_masks();
        assert!(MASKS.lock().get(&key(u32::MAX)).is_none());
    }

    /// A rectangle blurred by `sigma`, `w` by `h` with its left and top
    /// edges at `x` and `y`, baked for `target` in `color`.
    fn axes(
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        sigma: f64,
        kind: ColorType,
    ) -> (Axis, Axis) {
        let precision = Precision::of(kind);
        (
            Axis::of(x, x + w, sigma, precision),
            Axis::of(y, y + h, sigma, precision),
        )
    }

    #[test]
    fn a_trimmed_bake_is_the_whole_product() {
        // The frame leaves out the columns and rows that round to zero and
        // fills the interior where it rounds as the middle does. Drawn, it
        // is every byte of the outer product baked whole, worked out here a
        // pixel at a time by the bake's own arithmetic. On N32 pixels, which
        // Skia blits a flat colour onto by its own 8-bit arithmetic, and
        // which a canvas's pixels are.
        let target =
            ImageInfo::new((1, 1), ColorType::N32, AlphaType::Premul, None);
        // Two colours, neither opaque: the second's alpha, a global alpha
        // as the draw carries it, puts the middle's bytes off any round
        // number.
        for color in [
            Color4f::new(0.2, 0.4, 0.8, 0.6),
            Color4f::new(0.9, 0.1, 0.3, 0.37),
        ] {
            let premul = premultiplied(color, &target).expect("a colour");
            for (x, y, w, h, sigma) in [
                (100.25f32, 100.5f32, 600.0f32, 400.0f32, 10.0f64),
                (100.25, 100.5, 200.0, 200.0, 10.0),
                (30.75, 30.25, 200.0, 200.0, 2.0),
                (50.5, 50.5, 120.0, 40.0, 0.5),
                (300.0, 300.0, 200.0, 200.0, 100.0),
            ] {
                let (across, down) = axes(x, y, w, h, sigma, ColorType::N32);
                let (cols, rows) = (across.profile.len(), down.profile.len());
                let baked = Baked::make(&across, &down, premul, &target)
                    .expect("a bake");
                let info = ImageInfo::new(
                    (cols as i32, rows as i32),
                    ColorType::N32,
                    AlphaType::Premul,
                    None,
                );
                let mut surface =
                    started::raster(&info, None, None).expect("a surface");
                baked.draw(surface.canvas(), IPoint::new(0, 0), color, None);
                let mut got = vec![0u8; cols * rows * 4];
                assert!(surface.read_pixels(&info, &mut got, cols * 4, (0, 0)));
                let [r, g, b, a] = premul.map(|c| c * 255.0);
                let order = match ColorType::N32 {
                    ColorType::BGRA8888 => [b, g, r, a],
                    _ => [r, g, b, a],
                };
                for (j, q) in down.profile.iter().enumerate() {
                    for (i, p) in across.profile.iter().enumerate() {
                        let want = order.map(|c| ((c * p) * q + 0.5) as u8);
                        let at = (j * cols + i) * 4;
                        assert_eq!(
                            &got[at..at + 4],
                            &want,
                            "{w}x{h} by {sigma} in {color:?}: pixel ({i}, {j})"
                        );
                    }
                }
            }
        }
    }

    /// How far an F16 pixel may lie from the value it holds: half a unit in
    /// the last place of a half-precision float just below 1, 2^-12.
    const HALF_ULP: f32 = 1.0 / 4096.0;

    #[test]
    fn a_bake_for_float_pixels_draws_the_whole_outer_product() {
        // On F16 pixels nothing is trimmed and the interior is the run that
        // is level to 1e-7; drawn, the bake is the outer product to the
        // precision of a half float.
        let target =
            ImageInfo::new((1, 1), ColorType::RGBAF16, AlphaType::Premul, None);
        let color = Color4f::new(0.9, 0.1, 0.3, 0.37);
        let premul = premultiplied(color, &target).expect("a colour");
        for (x, y, w, h, sigma) in [
            (100.25f32, 100.5f32, 600.0f32, 400.0f32, 10.0f64),
            (100.25, 100.5, 200.0, 200.0, 10.0),
            (30.75, 30.25, 200.0, 200.0, 2.0),
        ] {
            let (across, down) = axes(x, y, w, h, sigma, ColorType::RGBAF16);
            let (cols, rows) = (across.profile.len(), down.profile.len());
            let baked =
                Baked::make(&across, &down, premul, &target).expect("a bake");
            let info = ImageInfo::new(
                (cols as i32, rows as i32),
                ColorType::RGBAF16,
                AlphaType::Premul,
                None,
            );
            let mut surface =
                started::raster(&info, None, None).expect("a surface");
            baked.draw(surface.canvas(), IPoint::new(0, 0), color, None);
            let read = ImageInfo::new(
                (cols as i32, rows as i32),
                ColorType::RGBAF32,
                AlphaType::Premul,
                None,
            );
            let mut got = vec![0u8; cols * rows * 16];
            assert!(surface.read_pixels(&read, &mut got, cols * 16, (0, 0)));
            let channel = |at: usize| {
                f32::from_ne_bytes([
                    got[at],
                    got[at + 1],
                    got[at + 2],
                    got[at + 3],
                ])
            };
            for (j, q) in down.profile.iter().enumerate() {
                for (i, p) in across.profile.iter().enumerate() {
                    for (k, c) in premul.iter().enumerate() {
                        let off = (channel((j * cols + i) * 16 + k * 4)
                            - c * p * q)
                            .abs();
                        assert!(
                            off <= HALF_ULP,
                            "{w}x{h} by {sigma}: pixel ({i}, {j}) channel {k} {off:e} off"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn a_bake_for_float_pixels_keeps_its_whole_reach() {
        // Float pixels hold what an 8-bit one rounds away, so nothing is
        // trimmed: the bands and the interior cover the whole extent.
        let target =
            ImageInfo::new((1, 1), ColorType::RGBAF16, AlphaType::Premul, None);
        let (across, down) =
            axes(100.25, 100.5, 600.0, 400.0, 10.0, ColorType::RGBAF16);
        let baked = Baked::make(&across, &down, [0.0, 0.0, 0.0, 1.0], &target)
            .expect("a bake");
        let banded: usize = baked
            .bands
            .iter()
            .map(|(image, _)| (image.width() * image.height()) as usize)
            .sum();
        let interior = baked
            .interior
            .map_or(0, |(r, _)| (r.width() * r.height()) as usize);
        assert_eq!(
            banded + interior,
            across.profile.len() * down.profile.len()
        );
    }

    #[test]
    fn an_axis_flat_run_is_where_the_profile_is_level() {
        // Wide against its blur: a flat middle, and the profile within the
        // tolerance of its middle all along it.
        let axis = Axis::of(100.25, 900.75, 10.0, BYTE_PRECISION);
        assert!(!axis.flat.is_empty());
        let middle = axis.middle();
        assert!(axis.flat.clone().all(|i| (axis.profile[i] - middle).abs()
            <= BYTE_PRECISION.flat_tolerance));
        // Just past each end of the run, it is not.
        assert!(
            (axis.profile[axis.flat.start - 1] - middle).abs()
                > BYTE_PRECISION.flat_tolerance
        );
        assert!(
            (axis.profile[axis.flat.end] - middle).abs()
                > BYTE_PRECISION.flat_tolerance
        );
        // Narrow against its blur: only the few columns about the peak, where
        // the profile's slope is nil, lie within the tolerance of it.
        let narrow = Axis::of(300.0, 500.0, 100.0, BYTE_PRECISION);
        let peak = narrow.middle();
        assert!(
            narrow.flat.len() < narrow.profile.len() / 50,
            "{:?} of {}",
            narrow.flat,
            narrow.profile.len()
        );
        assert!(narrow.flat.clone().all(|i| (narrow.profile[i] - peak).abs()
            <= BYTE_PRECISION.flat_tolerance));
    }
}
