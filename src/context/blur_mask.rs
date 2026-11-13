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
//! drawn again costs the sprite alone.
//!
//! On the GPU the profiles are the CPU's, worked here in `f64` and kept as
//! `f32`, and each axis is handed over as a one-pixel-high texture: the
//! fragment shader [`RECT_BLUR_SKSL`] multiplies the two at its pixel by the
//! colour and rounds the product as [`bake`] does. Worked per fragment on
//! the GPU instead, from `erfc` and `exp`, the profile lay up to 0.86 of a
//! level off the exact blur at a sigma of 0.25 and over half a level at every
//! sigma, where the same arithmetic in `f32` on the CPU is within half: the
//! GPU's own transcendentals and division are less precise than `f32`, and
//! Ganesh then stores the colour in half precision, a sixteenth of a level
//! near 1. The interior is drawn as the CPU draws it -- see
//! [`draw_interior`].

use std::sync::{Arc, LazyLock};

use parking_lot::Mutex;
use skia_safe::{
    AlphaType, Bitmap, BlendMode, Canvas as SkCanvas, Color4f, ColorSpace,
    ColorType, IPoint, IRect, Image, ImageInfo, Paint, Rect, shaders,
};
#[cfg(any(feature = "metal", feature = "vulkan"))]
use skia_safe::{Data, RuntimeEffect, SamplingOptions, TileMode};

use crate::{
    cache_store::{Budget, Store, StoreKey, Stored},
    context::layer::ExactColour,
    started,
};

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

/// What the mask cache keeps its bakes to.
const MASK_BUDGET: Budget = Budget {
    bytes: MASK_CACHE_BYTES,
    most: MASK_MAX_BYTES,
    low_water: MASK_CACHE_LOW_WATER,
};

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
/// `erf`: the edge's share of a pixel spanning those distances. `above`
/// is [`above_abs`] of a distance over `k`, which the caller works out once
/// for each pixel boundary and two pixels share.
///
/// Close to the edge against `k`, from the series
/// `G(u) = (1 + u^2 - u^4 / 6 + u^6 / 30 - u^8 / 168 + ...) / sqrt(pi)`,
/// whose n-th coefficient is `(-1)^(n+1) / (n! (2n - 1))`, taken to its
/// `u^6` term; past it, as `|u|` and what `G` lies above it.
fn edge_share(
    (d0, d1): (f64, f64),
    k: f64,
    above: impl Fn(usize) -> f64,
    (i0, i1): (usize, usize),
) -> f64 {
    if d0.max(d1) < SERIES_BELOW * k {
        let k2 = k * k;
        let (q0, q1) = (d0 * d0 / k2, d1 * d1 / k2);
        let term = |n: i32, c: f64| c * (q1.powi(n) - q0.powi(n));
        k * FRAC_1_SQRT_PI
            * (term(1, 1.0) + term(2, -1.0 / 6.0) + term(3, 1.0 / 30.0))
    } else {
        (d1 - d0) + k * (above(i1) - above(i0))
    }
}

/// The interval `[a, b]` blurred by `sigma`, averaged over each of `n` pixels
/// from zero: pixel `i` covers `[i, i + 1)`. Each boundary's [`above_abs`]
/// is worked out once, for both pixels it bounds: an `erfc` and an `exp` an
/// edge and a boundary, half what a pixel at a time takes, and the same
/// value either way.
fn profile(n: usize, a: f64, b: f64, sigma: f64) -> Vec<f32> {
    let k = sigma * std::f64::consts::SQRT_2;
    let above = |e: f64| -> Vec<f64> {
        (0..=n)
            .map(|j| above_abs((j as f64 - e).abs() / k))
            .collect()
    };
    let (above_a, above_b) = (above(a), above(b));
    let edge = |i: usize, e: f64, kept: &[f64]| {
        let (x0, x1) = (i as f64, i as f64 + 1.0);
        edge_share(((x0 - e).abs(), (x1 - e).abs()), k, |j| kept[j], (i, i + 1))
    };
    (0..n)
        .map(|i| {
            ((edge(i, a, &above_a) - edge(i, b, &above_b)) / 2.0)
                .clamp(0.0, 1.0) as f32
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
        let interior = interior_of(across, down, color, bytes);
        let coverage = interior.map(|(_, coverage)| coverage);
        let interior = interior.map(|(r, _)| r);
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
        Some(Baked {
            bands,
            interior: interior.zip(coverage),
            bytes,
        })
    }

    /// Draws the rectangle with its extent's origin at `origin` in device
    /// pixels, the interior in `color`, or in `exact` placed at its alpha as
    /// [`draw_rect`] bakes it.
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
            draw_interior(
                canvas,
                rect.with_offset(origin),
                coverage,
                color,
                exact,
            );
        }
    }
}

/// The interior of the rectangle of `across` and `down` in `color`,
/// premultiplied, from the extent's origin, and the coverage it is filled
/// at: the run where both profiles are flat, on 8-bit pixels widened while
/// it bakes to the same bytes -- see [`widened`]. `None` where either
/// profile has no flat run.
///
/// An 8-bit interior whose bytes are full coverage's is filled at full
/// coverage, to the same bytes: Skia fills an opaque colour as a memset and
/// a nearly opaque one by blending each pixel, about six times as long at
/// 600 by 400.
fn interior_of(
    across: &Axis,
    down: &Axis,
    color: [f32; 4],
    eight_bit: bool,
) -> Option<(IRect, f32)> {
    if across.flat.is_empty() || down.flat.is_empty() {
        return None;
    }
    let flat = IRect::new(
        across.flat.start as i32,
        down.flat.start as i32,
        across.flat.end as i32,
        down.flat.end as i32,
    );
    let rect = match eight_bit {
        true => widened(flat, across, down, color),
        false => flat,
    };
    let coverage = across.middle() * down.middle();
    let byte = |c: f32| color.map(|v| (v * c * 255.0 + 0.5) as u8);
    let coverage = match eight_bit && byte(coverage) == byte(1.0) {
        true => 1.0,
        false => coverage,
    };
    Some((rect, coverage))
}

/// Fills `rect`, in device pixels, at `coverage` in `color`, or in `exact`
/// placed at its alpha: the rectangle's interior, as both engines draw it.
fn draw_interior(
    canvas: &SkCanvas,
    rect: IRect,
    coverage: f32,
    color: Color4f,
    exact: Option<&ExactColour>,
) {
    {
        {
            let fill =
                Color4f::new(color.r, color.g, color.b, color.a * coverage);
            // UPSTREAM: skia-safe 0.156.0 -- unfiled -- worked around
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
            //
            // The exact colour is placed at the draw's alpha, as the bands
            // were baked in it, and scaled by the coverage after: placed at
            // the coverage's alpha instead, it rounds apart from the bands
            // beside it wherever the interior is not at full coverage.
            let mut paint = Paint::default();
            match exact
                .map(|exact| exact.at(color.a))
                .and_then(|(exact, alpha)| Some((exact.shader()?, alpha)))
            {
                // On a page that keeps a recording for pixels, where on
                // floats a paint's alpha is not dropped.
                Some((shader, alpha)) => {
                    paint.set_shader(shader).set_alpha_f(alpha * coverage);
                }
                None => {
                    paint.set_shader(shaders::color_in_space(
                        fill,
                        ColorSpace::new_srgb(),
                    ));
                }
            }
            paint.set_anti_alias(false);
            canvas.draw_irect(rect, &paint);
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
/// making it; a filter chain's result is written this way too.
pub(crate) fn baked_image(
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

impl StoreKey for RectKey {
    /// Never: a key holds the rectangle by value, and nothing it names can
    /// go.
    fn is_orphaned(&self) -> bool {
        false
    }
}

impl Stored for Arc<Baked> {
    fn bytes(&self) -> usize {
        self.bytes
    }
}

/// The baked rectangles kept, by what changes their pixels.
type MaskCache = Store<RectKey, Arc<Baked>>;

impl Default for MaskCache {
    fn default() -> Self {
        Store::new(MASK_BUDGET)
    }
}

/// Every baked rectangle kept, for every thread: pages are played back on
/// rayon workers.
static MASKS: LazyLock<Mutex<MaskCache>> = LazyLock::new(Mutex::default);

/// Drops every baked rectangle. Called with the page cache's release, when
/// rendering has been idle.
pub(crate) fn release_cached_masks() {
    MASKS.lock().release();
}

/// The blurred rectangle at a fragment, in device pixels: the colour,
/// premultiplied in the page's space, times the profile across at the
/// fragment's column and the profile down at its row, read from the rows
/// of a texture the CPU made -- see [`encoded`] -- whose first texels lie
/// at `origin`. Where `levels` is not zero, as on 8-bit pixels, the product is
/// rounded to them as [`bake`] rounds it, before Ganesh stores it at half
/// precision: a value on the grid of levels survives that within 0.06 of a
/// level, and the store's own rounding then lands on it.
///
/// It works no profile itself. The GPU's `exp`, `erfc` and division are less
/// precise than an `f32`, and the profile worked from them per fragment lay
/// up to 0.86 of a level off the exact blur at a sigma of 0.25, where the
/// same arithmetic in `f32` on the CPU is within half a level.
#[cfg(any(feature = "metal", feature = "vulkan"))]
const RECT_BLUR_SKSL: &str = "
uniform shader profiles;
uniform float2 origin;
uniform float4 colour;
uniform float levels;

// A profile value from its texel: 24 bits of fixed point, most significant
// byte in red. Ganesh hands the texel over in half precision, which holds a
// byte's level to well within half a level, so rounding recovers it.
float profile(half4 t) {
    float3 k = floor(float3(t.rgb) * 255.0 + 0.5);
    return (k.r * 65536.0 + k.g * 256.0 + k.b) / 16777215.0;
}

half4 main(float2 p) {
    float a = profile(profiles.eval(float2(p.x - origin.x, 0.5)));
    float d = profile(profiles.eval(float2(p.y - origin.y, 1.5)));
    float4 v = (colour * a) * d;
    if (levels > 0.0) {
        v = floor(v * levels + 0.5) / levels;
    }
    return half4(v);
}
";

/// The largest value [`RECT_BLUR_SKSL`]'s 24-bit fixed point holds: a
/// profile value `v` is kept as `v * PROFILE_SCALE` rounded, within 3e-8 of
/// it, under the `f32` profile's own precision near 1.
#[cfg(any(feature = "metal", feature = "vulkan"))]
const PROFILE_SCALE: f32 = ((1u32 << 24) - 1) as f32;

/// How many axes, and how many pairs' textures, a thread keeps -- see
/// [`kept_axis`] and [`profiles_of`].
#[cfg(any(feature = "metal", feature = "vulkan"))]
const AXES_KEPT: usize = 512;

/// A rectangle's two profiles as [`RECT_BLUR_SKSL`] reads them: a texture
/// two pixels high, `across` in the first row and `down` in the second,
/// the shorter padded with zeros nothing reads, each texel a profile value
/// in 24-bit fixed point, opaque and in no colour space so that nothing
/// converts it. One texture rather than one an axis: a new rectangle
/// uploads once, and Skia builds one child for the shader.
#[cfg(any(feature = "metal", feature = "vulkan"))]
fn encoded(across: &[f32], down: &[f32]) -> Option<Image> {
    let width = across.len().max(down.len());
    let row = |profile: &[f32]| {
        profile
            .iter()
            .copied()
            .chain(std::iter::repeat(0.0))
            .take(width)
            .flat_map(|v| {
                let n = (v.clamp(0.0, 1.0) * PROFILE_SCALE).round() as u32;
                [(n >> 16) as u8, (n >> 8) as u8, n as u8, u8::MAX]
            })
            .collect::<Vec<u8>>()
    };
    let texels: Vec<u8> = [row(across), row(down)].concat();
    let info = ImageInfo::new(
        (width as i32, 2),
        ColorType::RGBA8888,
        AlphaType::Opaque,
        None,
    );
    started::raster_from_data(&info, Data::new_copy(&texels), width * 4)
}

/// What an axis is kept by: its edges from the window's origin, as the
/// profile takes them, the window's length, the sigma and the tolerance
/// its flat run is found to, as bits. Not where the window lies: the
/// profile is the same wherever the edges keep their place in it.
#[cfg(any(feature = "metal", feature = "vulkan"))]
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct AxisKey {
    edges: [u64; 2],
    len: usize,
    sigma: u64,
    flat: u32,
}

#[cfg(any(feature = "metal", feature = "vulkan"))]
thread_local! {
    /// The axes this thread drew last, so that a rectangle with an axis
    /// drawn before works that profile out no more. Cleared whole past
    /// [`AXES_KEPT`].
    static AXES: std::cell::RefCell<std::collections::HashMap<AxisKey, std::rc::Rc<Axis>>> =
        std::cell::RefCell::default();

    /// The textures of the pairs of axes this thread drew last, so that a
    /// rectangle drawn again uploads nothing: Skia keeps a texture for an
    /// image as long as the image lives. Cleared whole past [`AXES_KEPT`].
    static PROFILES: std::cell::RefCell<std::collections::HashMap<(AxisKey, AxisKey), Image>> =
        std::cell::RefCell::default();
}

/// The axis of `[lo, hi]` over `span`, blurred by `sigma` and drawn to
/// `precision`, and what it is kept by: worked out once and kept -- see
/// [`AXES`]. Worked out on every draw, the profiles' `erfc` and `exp` led
/// the profile of a page of a hundred small rectangles of eight sizes drawn
/// over and over.
#[cfg(any(feature = "metal", feature = "vulkan"))]
fn kept_axis(
    lo: f32,
    hi: f32,
    sigma: f64,
    precision: Precision,
    span: (i32, i32),
) -> (std::rc::Rc<Axis>, AxisKey) {
    let origin = f64::from(span.0);
    let key = AxisKey {
        edges: [f64::from(lo) - origin, f64::from(hi) - origin]
            .map(f64::to_bits),
        len: (span.1 - span.0) as usize,
        sigma: sigma.to_bits(),
        flat: precision.flat_tolerance.to_bits(),
    };
    let axis = AXES.with(|kept| {
        let mut kept = kept.borrow_mut();
        if let Some(axis) = kept.get(&key) {
            return axis.clone();
        }
        let axis =
            std::rc::Rc::new(Axis::within(lo, hi, sigma, precision, span));
        if kept.len() >= AXES_KEPT {
            kept.clear();
        }
        kept.insert(key, axis.clone());
        axis
    });
    (axis, key)
}

/// The texture of the axes kept as `across` and `down`, made once for the
/// pair and kept -- see [`PROFILES`].
#[cfg(any(feature = "metal", feature = "vulkan"))]
fn profiles_of(
    (across, across_key): (&Axis, AxisKey),
    (down, down_key): (&Axis, AxisKey),
) -> Option<Image> {
    PROFILES.with(|kept| {
        let mut kept = kept.borrow_mut();
        let key = (across_key, down_key);
        if let Some(image) = kept.get(&key) {
            return Some(image.clone());
        }
        let image = encoded(&across.profile, &down.profile)?;
        if kept.len() >= AXES_KEPT {
            kept.clear();
        }
        kept.insert(key, image.clone());
        Some(image)
    })
}

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
pub(crate) fn on_gpu(canvas: &SkCanvas) -> bool {
    canvas.recording_context().is_some()
}

#[cfg(not(any(feature = "metal", feature = "vulkan")))]
pub(crate) fn on_gpu(_canvas: &SkCanvas) -> bool {
    false
}

/// Draws `rect`, already in device pixels, blurred by `sigma` device pixels
/// in `color` -- unpremultiplied sRGB -- onto `canvas`, whose matrix is
/// expected to be the identity and on which [`draws_on`] is true. Returns
/// `false`, having drawn nothing, where a surface or a shader could not be
/// made.
///
/// With `exact`, the colour is that, held in the page's own space and placed
/// at `color`'s alpha by [`ExactColour::at`] -- on an eight-bit page on the
/// bytes a fill of it stores, as the GPU places it too: what a page that
/// keeps a recording for pixels paints a flat colour in -- see
/// [`ExactColour`]. Converted from sRGB instead, it
/// lands on the 64-ulp grid Skia's conversion leaves it on. On the CPU the
/// exact colour is baked as the surface's own components, so it is taken
/// only on a surface in its space: a page played onto another, as a canvas
/// drawn into one in another space is, is painted `color` converted into
/// that one.
///
/// Both engines draw the same pixels: the colour times the two profiles,
/// rounded once as [`bake`] rounds it, around an interior drawn as a fill of
/// the colour -- see [`draw_interior`]. The CPU bakes them into sprites; the
/// GPU works the same product per fragment from the same profiles -- see
/// [`RECT_BLUR_SKSL`].
pub(crate) fn draw_rect(
    canvas: &SkCanvas,
    rect: Rect,
    sigma: f32,
    color: Color4f,
    exact: Option<&ExactColour>,
) -> bool {
    let sigma = f64::from(sigma);
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
    // An exact colour placed at its alpha, as a fill of it is: on an
    // eight-bit page, on the bytes the fill stores, so that the blur scales
    // those, as a blurred path's does -- see `layer::BlurOp::result_chain` --
    // and the interior stores what the fill does. Scaled at the alpha as
    // given, the interior of `rgba(0, 0, 0, 0.5)` lay just under 127.5 and
    // stored 127, where a fill stores 128.
    let premul = || match exact {
        Some(exact) => {
            let (placed, alpha) = exact.at(color.a);
            let [r, g, b] = placed.rgb.map(|c| c * alpha);
            Some([r, g, b, alpha])
        }
        None => premultiplied(color, &target),
    };
    #[cfg(any(feature = "metal", feature = "vulkan"))]
    if on_gpu(canvas) {
        let (across, across_key) =
            kept_axis(rect.left, rect.right, sigma, precision, cols);
        let (down, down_key) =
            kept_axis(rect.top, rect.bottom, sigma, precision, rows);
        let (Some(profiles), Some(premul)) = (
            profiles_of((&across, across_key), (&down, down_key)),
            premul(),
        ) else {
            return false;
        };
        return draw_on_gpu(
            canvas,
            origin,
            (&across, &down, profiles),
            premul,
            &target,
        );
    }
    // The profiles only on a miss: a kept bake is drawn without them.
    let kept = MASKS.lock().get(&key);
    let baked = match kept {
        Some(baked) => baked,
        None => {
            let across =
                Axis::within(rect.left, rect.right, sigma, precision, cols);
            let down =
                Axis::within(rect.top, rect.bottom, sigma, precision, rows);
            let Some(premul) = premul() else {
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

/// [`RECT_BLUR_SKSL`] over `profiles`, the texture of both axes, whose
/// first texels lie at `origin` in device pixels, in `premul`, rounded to
/// `levels` where they are not zero.
#[cfg(any(feature = "metal", feature = "vulkan"))]
fn rect_blur_shader(
    profiles: Image,
    origin: (f32, f32),
    premul: [f32; 4],
    levels: f32,
) -> Option<skia_safe::Shader> {
    let nearest = SamplingOptions::default();
    // Raw: the texels are numbers, which a colour-managed shader would
    // convert from sRGB into the page's space -- 58 levels off on a
    // linear page.
    let profiles = profiles.to_raw_shader(
        (TileMode::Decal, TileMode::Decal),
        nearest,
        None,
    )?;
    let uniforms: Vec<u8> = [origin.0, origin.1]
        .into_iter()
        .chain(premul)
        .chain([levels])
        .flat_map(f32::to_ne_bytes)
        .collect();
    RECT_BLUR.with(|effect| {
        effect.as_ref().and_then(|e| {
            e.make_shader(Data::new_copy(&uniforms), &[profiles.into()], None)
        })
    })
}

/// [`draw_rect`] on a GPU surface, in one draw: every pixel of the extent
/// the profiles `across` and `down` span from `at` worked per fragment by
/// [`RECT_BLUR_SKSL`], in `premul`, the colour premultiplied in `target`'s
/// space, and rounded to its levels where it holds bytes. A kept axis is
/// the same wherever its window lies, so the window's place is the draw's.
///
/// The interior too: at full coverage both profiles read exactly one, and
/// the product rounded to a level is the byte a fill of the colour stores.
/// Filled apart, as the CPU fills it, a rectangle was two draws, and drawn
/// as the four bands about the interior as well, Skia built the shader's
/// program four times a rectangle; a page of a hundred small ones took 1.6
/// times what one draw does.
#[cfg(any(feature = "metal", feature = "vulkan"))]
fn draw_on_gpu(
    canvas: &SkCanvas,
    at: IPoint,
    (across, down, profiles): (&Axis, &Axis, Image),
    premul: [f32; 4],
    target: &ImageInfo,
) -> bool {
    let levels = match target.color_type() {
        ColorType::RGBA8888 | ColorType::BGRA8888 => f32::from(u8::MAX),
        _ => 0.0,
    };
    let extent = IRect::new(
        at.x,
        at.y,
        at.x + across.profile.len() as i32,
        at.y + down.profile.len() as i32,
    );
    let origin = (at.x as f32, at.y as f32);
    let Some(shader) = rect_blur_shader(profiles, origin, premul, levels)
    else {
        return false;
    };
    let mut paint = Paint::default();
    paint.set_shader(shader).set_anti_alias(false);
    canvas.draw_irect(extent, &paint);
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

    /// [`RECT_BLUR_SKSL`] drawn over `across` and `down` from their first
    /// pixels, in `premul`, onto a raster surface of 8-bit pixels with no
    /// colour space. Skia runs a runtime effect on the CPU too, in `f32`, so
    /// this checks the shader's reading of its textures and its rounding
    /// without a GPU, on the profiles the GPU is handed.
    #[cfg(any(feature = "metal", feature = "vulkan"))]
    fn shaded(across: &Axis, down: &Axis, premul: [f32; 4]) -> Vec<u8> {
        let (w, h) = (across.profile.len(), down.profile.len());
        let profiles =
            encoded(&across.profile, &down.profile).expect("a texture");
        let shader =
            rect_blur_shader(profiles, (0.0, 0.0), premul, f32::from(u8::MAX))
                .expect("a shader");
        let info = ImageInfo::new(
            (w as i32, h as i32),
            ColorType::RGBA8888,
            AlphaType::Premul,
            None,
        );
        let mut surface =
            started::raster(&info, None, None).expect("a surface");
        let mut paint = Paint::default();
        paint.set_shader(shader).set_blend_mode(BlendMode::Src);
        surface.canvas().draw_paint(&paint);
        let mut bytes = vec![0u8; w * h * 4];
        assert!(surface.read_pixels(&info, &mut bytes, w * 4, (0, 0)));
        bytes
    }

    /// How near a half-level the bake's value has to lie for the shader to
    /// round it apart: the two multiply in another order, and the texture
    /// holds a profile value to 24 bits of fixed point where the bake holds
    /// it as an `f32`.
    #[cfg(any(feature = "metal", feature = "vulkan"))]
    const SHADED_TIE: f32 = 1e-3;

    /// The shader draws the bake's bytes: the colour times the two profiles,
    /// rounded once, at every pixel of the extent, for a rectangle off the
    /// pixel grid at sigmas from under a pixel to past the bake's own
    /// window, in an opaque colour and a translucent one. Worked from
    /// `erfc` and `exp` on the GPU instead, the profile lay up to 0.86 of a
    /// level off the exact blur.
    #[cfg(any(feature = "metal", feature = "vulkan"))]
    #[test]
    fn the_shader_draws_what_the_bake_does() {
        let colours = [[0.0, 0.0, 0.0, 1.0], [0.42, 0.11, 0.33, 0.6]];
        for sigma in [0.25, 1.0, 4.0, 12.0, 40.0] {
            let across = Axis::of(10.3, 70.85, sigma, BYTE_PRECISION);
            let down = Axis::of(5.6, 33.2, sigma, BYTE_PRECISION);
            let (w, h) = (across.profile.len(), down.profile.len());
            for premul in colours {
                let mut baked = vec![0u8; w * h * 4];
                bake(
                    &across,
                    &down,
                    IRect::new(0, 0, w as i32, h as i32),
                    premul,
                    ColorType::RGBA8888,
                    &mut baked,
                    w * 4,
                );
                let shaded = shaded(&across, &down, premul);
                let apart: Vec<(usize, u8, u8)> = baked
                    .iter()
                    .zip(&shaded)
                    .enumerate()
                    .filter(|(i, (b, s))| {
                        let (x, y) = ((i / 4) % w, (i / 4) / w);
                        let exact = premul[i % 4]
                            * 255.0
                            * across.profile[x]
                            * down.profile[y];
                        b != s && (exact.fract() - 0.5).abs() > SHADED_TIE
                    })
                    .map(|(i, (b, s))| (i, *b, *s))
                    .collect();
                assert!(
                    apart.is_empty(),
                    "sigma {sigma}, {premul:?}: {} bytes apart, first {:?}",
                    apart.len(),
                    &apart[..apart.len().min(4)]
                );
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
                cache.bytes() <= MASK_CACHE_BYTES,
                "{} bytes after {n}",
                cache.bytes()
            );
            assert_eq!(cache.bytes(), cache.len() * one.bytes);
            assert!(cache.get(&key(n)).is_some(), "the latest, {n}, is kept");
        }
        // Cut back to the low-water mark rather than by one: the oldest went.
        assert!(cache.get(&key(0)).is_none());
    }

    #[test]
    fn the_mask_cache_lets_go_of_the_least_recently_used_to_its_low_water() {
        // As many bakes as fit the budget, the first of them used again, and
        // one more: the eviction cuts back to `kept` bakes, oldest use first,
        // so the one used again stays where filing order would let it go.
        let one = baked(120.0, 30.0);
        let fit = (MASK_CACHE_BYTES / one.bytes) as u32;
        let kept = (MASK_CACHE_LOW_WATER / one.bytes) as u32;
        assert!(kept >= 3 && kept < fit, "{kept} of {fit}");
        let mut cache = MaskCache::default();
        for n in 0..fit {
            cache.put(key(n), one.clone());
        }
        assert!(cache.get(&key(0)).is_some());
        cache.put(key(fit), one.clone());
        // The bake just filed, the one used again, and the latest filed
        // before them.
        let mut expected = vec![0, fit];
        expected.extend(fit + 2 - kept..fit);
        expected.sort_unstable();
        let survivors: Vec<u32> = (0..=fit)
            .filter(|&n| cache.get(&key(n)).is_some())
            .collect();
        assert_eq!(survivors, expected);
        assert_eq!(cache.bytes(), kept as usize * one.bytes);
    }

    #[test]
    fn the_mask_cache_does_not_keep_one_bake_past_its_cap() {
        let mut cache = MaskCache::default();
        let huge = baked(400.0, 200.0);
        assert!(huge.bytes > MASK_MAX_BYTES, "{} bytes", huge.bytes);
        cache.put(key(1), huge);
        assert!(cache.get(&key(1)).is_none());
        assert_eq!(cache.bytes(), 0);
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
