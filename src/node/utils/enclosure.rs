//! Where a Lab, LCH, Oklab or OkLCh colour converts to, bounded without
//! converting it, for [`declared_to_css`](super::declared_to_css)'s search.
//!
//! The search tries hundreds of spellings and keeps the shortest that
//! repaints the colour held -- converts to the same `f32` sRGB. Most miss by
//! far more than an `f32` step, which a first-order prediction shows for
//! nine multiplications where a conversion costs hundreds of operations.
//! This is that prediction, made sound.
//!
//! [`Enclosure::of`] runs the conversion once over a box `B` around the
//! colour held, in interval arithmetic: each value an interval holding the
//! exact result for every input in `B`, each partial derivative an interval
//! holding it over `B`, and each result carrying a bound on how far `f64`
//! rounding can take the code's result from the exact one. By the
//! mean-value theorem, a candidate `c` in `B` converts exactly to a point of
//! `f(x) + [J](c - x)`, and the code's result lies within twice the rounding
//! bound of that. [`Enclosure::refuses`] says a candidate cannot repaint
//! only when that interval misses, for some channel, the `f32` cell the
//! colour held rounds into.
//!
//! It only refuses. A candidate it does not refuse is converted in full, so
//! a bound too wide costs time and nothing else. A bound too narrow would
//! refuse a spelling that repaints, and the search would return a longer
//! one -- a wrong answer -- so every allowance below errs outward: each
//! interval operation rounds outward by an ulp, each rounding bound takes
//! `f64::EPSILON` where an operation is within half of it, and a library
//! function is allowed [`LIBRARY_ULPS`]. Where the box straddles a piecewise
//! definition's knee, or anything is not finite, there is no enclosure and
//! nothing is refused.

use crate::color::{
    SRGB_EXPONENT, SRGB_LINEAR_THRESHOLD, SRGB_OFFSET, SRGB_SCALE, SRGB_SLOPE,
};

use super::{
    D50_TO_D65, Declared, Form, LAB_EPSILON, LAB_KAPPA, LAB_WHITE_D50,
    MATRIX_ROUNDING, Matrix3, OKLAB_LMS_TO_XYZ, OKLAB_TO_OKLAB_LMS,
    XYZ_TO_LINEAR_SRGB, declared_srgb,
};

/// How many ulps of its result `powi`, `powf`, `sin` and `cos` are allowed
/// to be off by.
///
/// Rust documents their precision as unspecified. The platform libraries
/// this builds against are within one ulp for these functions; four is that
/// with a margin, and erring wide only refuses less.
const LIBRARY_ULPS: f64 = 4.0;

/// A bound on the error of one `f64` operation, relative to its result.
///
/// A correctly rounded operation is within half of this; the whole of it
/// errs outward.
const ROUNDING: f64 = f64::EPSILON;

/// How far a box reaches from each channel held, relative to the channel,
/// and absolute so that a channel at zero has a box at all.
///
/// [`Enclosure::NEAR`], `2^-10` and `2^-20` along every channel, is far
/// wider than the spellings the mixed search tries around its own length
/// -- a decimal at six figures is `1e-5` of its value -- and narrow enough
/// that the derivatives hardly vary across it. A spelling outside the box
/// is not refused.
#[derive(Clone, Copy, Debug)]
pub(super) struct Radius {
    relative: [f64; 3],
    absolute: [f64; 3],
}

/// A closed interval of `f64`.
#[derive(Clone, Copy, Debug)]
struct Interval {
    lo: f64,
    hi: f64,
}

impl Interval {
    fn point(x: f64) -> Self {
        Self { lo: x, hi: x }
    }

    /// `lo..=hi`, each end rounded outward by one ulp.
    fn outward(lo: f64, hi: f64) -> Self {
        Self {
            lo: lo.next_down(),
            hi: hi.next_up(),
        }
    }

    fn add(self, other: Self) -> Self {
        Self::outward(self.lo + other.lo, self.hi + other.hi)
    }

    fn neg(self) -> Self {
        Self {
            lo: -self.hi,
            hi: -self.lo,
        }
    }

    fn mul(self, other: Self) -> Self {
        let products = [
            self.lo * other.lo,
            self.lo * other.hi,
            self.hi * other.lo,
            self.hi * other.hi,
        ];
        Self::outward(
            products.into_iter().fold(f64::INFINITY, f64::min),
            products.into_iter().fold(f64::NEG_INFINITY, f64::max),
        )
    }

    fn scale(self, factor: f64) -> Self {
        self.mul(Self::point(factor))
    }

    /// The interval with each end moved outward by `by`, and an ulp more.
    fn widened(self, by: f64) -> Self {
        Self::outward(self.lo - by, self.hi + by)
    }

    fn hull(self, other: Self) -> Self {
        Self {
            lo: self.lo.min(other.lo),
            hi: self.hi.max(other.hi),
        }
    }

    /// The largest magnitude in the interval.
    fn magnitude(self) -> f64 {
        self.lo.abs().max(self.hi.abs())
    }

    fn is_finite(self) -> bool {
        self.lo.is_finite() && self.hi.is_finite()
    }
}

/// One quantity of the conversion over the box: an interval holding its
/// exact value at every point of the box, intervals holding its partial
/// derivatives by the three channels over the box, and a bound on how far
/// the code's `f64` result is from the exact one anywhere in the box.
#[derive(Clone, Copy, Debug)]
struct Bound {
    value: Interval,
    slope: [Interval; 3],
    error: f64,
}

impl Bound {
    /// Channel `index`, over `centre ± radius`: exact, since a channel is an
    /// `f32` read as `f64`.
    fn channel(index: usize, centre: f64, radius: f64) -> Self {
        let mut slope = [Interval::point(0.0); 3];
        slope[index] = Interval::point(1.0);
        Self {
            value: Interval::outward(centre - radius, centre + radius),
            slope,
            error: 0.0,
        }
    }

    /// The largest the code's result can be in magnitude.
    fn reach(self) -> f64 {
        self.value.magnitude() + self.error
    }

    fn add(self, other: Self) -> Self {
        let value = self.value.add(other.value);
        let error = self.error + other.error;
        Self {
            value,
            slope: [0, 1, 2].map(|i| self.slope[i].add(other.slope[i])),
            error: error + ROUNDING * (value.magnitude() + error),
        }
    }

    fn neg(self) -> Self {
        Self {
            value: self.value.neg(),
            slope: self.slope.map(Interval::neg),
            error: self.error,
        }
    }

    fn mul(self, other: Self) -> Self {
        Self {
            value: self.value.mul(other.value),
            slope: [0, 1, 2].map(|i| {
                self.slope[i]
                    .mul(other.value)
                    .add(self.value.mul(other.slope[i]))
            }),
            error: other.value.magnitude() * self.error
                + self.value.magnitude() * other.error
                + self.error * other.error
                + ROUNDING * self.reach() * other.reach(),
        }
    }

    /// `self * factor`, `factor` a constant the code multiplies by.
    fn scale(self, factor: f64) -> Self {
        Self {
            value: self.value.scale(factor),
            slope: self.slope.map(|s| s.scale(factor)),
            error: factor.abs() * self.error
                + ROUNDING * factor.abs() * self.reach(),
        }
    }

    /// `self / divisor`, `divisor` a constant the code divides by.
    fn divide(self, divisor: f64) -> Self {
        let over = |i: Interval| {
            let (a, b) = (i.lo / divisor, i.hi / divisor);
            Interval::outward(a.min(b), a.max(b))
        };
        Self {
            value: over(self.value),
            slope: self.slope.map(over),
            error: self.error / divisor.abs()
                + ROUNDING * self.reach() / divisor.abs(),
        }
    }

    /// `self + constant`.
    fn offset(self, constant: f64) -> Self {
        let value = self.value.add(Interval::point(constant));
        Self {
            value,
            slope: self.slope,
            error: self.error + ROUNDING * (value.magnitude() + self.error),
        }
    }

    /// The code's result can be on the far side of `threshold` from the
    /// exact one by at most its error, so the value with its error must be
    /// on one side for the branch taken to be known.
    fn above(self, threshold: f64) -> Option<bool> {
        let reach = self.value.widened(self.error);
        match (reach.lo > threshold, reach.hi <= threshold) {
            (true, _) => Some(true),
            (_, true) => Some(false),
            _ => None,
        }
    }

    /// `self.powi(3)`, a library call: the exact cube by interval products,
    /// its error the input's carried through `3t^2` and [`LIBRARY_ULPS`] of
    /// the result.
    fn cube(self) -> Self {
        let square = self.value.mul(self.value);
        let reach = self.reach();
        Self {
            value: square.mul(self.value),
            slope: self.slope.map(|s| s.mul(square.scale(3.0))),
            error: 3.0 * reach * reach * self.error
                + LIBRARY_ULPS * ROUNDING * reach.powi(3),
        }
    }

    /// `self * self * self`, as the code writes Oklab's cube.
    fn cube_by_products(self) -> Self {
        self.mul(self).mul(self)
    }

    /// A row of [`super::apply_matrix`]: three products summed in order,
    /// and the snap to zero of a result within [`MATRIX_ROUNDING`] of its
    /// terms' magnitude, which moves the code's result by at most that.
    fn row(row: [f64; 3], v: [Bound; 3]) -> Self {
        let terms = [0, 1, 2].map(|i| v[i].scale(row[i]));
        let sum = terms[0].add(terms[1]).add(terms[2]);
        let magnitude: f64 = terms.iter().map(|t| t.reach()).sum();
        Self {
            error: sum.error + MATRIX_ROUNDING * magnitude,
            ..sum
        }
    }

    fn matrix(m: Matrix3, v: [Bound; 3]) -> [Bound; 3] {
        m.map(|row| Self::row(row, v))
    }

    /// [`crate::color::linear_to_srgb_f64`]: linear within the threshold
    /// either side of zero, a power beyond it, and `None` where the box
    /// reaches across the threshold.
    fn srgb_encode(self) -> Option<Self> {
        let reach = self.value.widened(self.error);
        if reach.lo >= -SRGB_LINEAR_THRESHOLD
            && reach.hi <= SRGB_LINEAR_THRESHOLD
        {
            return Some(self.scale(SRGB_SLOPE));
        }
        let (positive, magnitude) = match (
            reach.lo > SRGB_LINEAR_THRESHOLD,
            reach.hi < -SRGB_LINEAR_THRESHOLD,
        ) {
            (true, _) => (true, self),
            (_, true) => (false, self.neg()),
            _ => return None,
        };
        let power = magnitude.power(SRGB_EXPONENT.recip())?;
        let curve = power.scale(SRGB_SCALE).offset(-SRGB_OFFSET);
        Some(match positive {
            true => curve,
            false => curve.neg(),
        })
    }

    /// `self.powf(exponent)` for `0 < exponent < 1` and a positive base:
    /// increasing, with a decreasing derivative, so both are bounded by the
    /// ends of the interval.
    fn power(self, exponent: f64) -> Option<Self> {
        let reach = self.value.widened(self.error);
        if reach.lo <= 0.0 {
            return None;
        }
        let library = |x: f64| LIBRARY_ULPS * ROUNDING * x.abs();
        let at = |x: f64| x.powf(exponent);
        let value = Interval {
            lo: at(self.value.lo) - library(at(self.value.lo)),
            hi: at(self.value.hi) + library(at(self.value.hi)),
        }
        .widened(0.0);
        let slope_at = |x: f64| exponent * x.powf(exponent - 1.0);
        let derivative = Interval {
            lo: slope_at(self.value.hi) - library(slope_at(self.value.hi)),
            hi: slope_at(self.value.lo) + library(slope_at(self.value.lo)),
        }
        .widened(0.0);
        // The code's input is within the error of the exact one, where the
        // derivative is at most its value at the lower end of the reach.
        let steepest = slope_at(reach.lo) * (1.0 + LIBRARY_ULPS * ROUNDING);
        Some(Self {
            value,
            slope: self.slope.map(|s| s.mul(derivative)),
            error: steepest * self.error
                + library(value.magnitude() + steepest * self.error),
        })
    }

    /// `sin` and `cos` of `self` in radians, library calls; `None` for an
    /// interval more than ten turns wide, which no hue box is.
    fn sin_cos(self) -> Option<(Self, Self)> {
        let reach = self.value.widened(self.error);
        let width = reach.hi - reach.lo;
        if width.is_nan() || width >= 64.0 {
            return None;
        }
        let quarter = std::f64::consts::FRAC_PI_2;
        // `sin` peaks at quarter turns `4k + 1` and dips at `4k + 3`; `cos`
        // at `4k` and `4k + 2`. Any quarter-turn mark within the interval,
        // with a margin for the mark's own rounding, is taken as reached.
        let first = (reach.lo / quarter).floor() as i64 - 1;
        let last = (reach.hi / quarter).ceil() as i64 + 1;
        let marks: Vec<i64> = (first..=last)
            .filter(|&n| {
                let at = n as f64 * quarter;
                at >= reach.lo - 1e-9 && at <= reach.hi + 1e-9
            })
            .collect();
        let range = |f: fn(f64) -> f64, peaks: [i64; 2]| {
            let mut i = Interval {
                lo: f(reach.lo).min(f(reach.hi)),
                hi: f(reach.lo).max(f(reach.hi)),
            };
            for &n in &marks {
                if n.rem_euclid(4) == peaks[0] {
                    i.hi = 1.0;
                }
                if n.rem_euclid(4) == peaks[1] {
                    i.lo = -1.0;
                }
            }
            i.widened(LIBRARY_ULPS * ROUNDING)
        };
        let sin = range(f64::sin, [1, 3]);
        let cos = range(f64::cos, [0, 2]);
        // Each moves by at most the input's error, its slope being at most
        // one, and is off by [`LIBRARY_ULPS`] of a result at most one.
        let error = self.error + LIBRARY_ULPS * ROUNDING;
        Some((
            Self {
                value: sin,
                slope: self.slope.map(|s| s.mul(cos)),
                error,
            },
            Self {
                value: cos,
                slope: self.slope.map(|s| s.mul(sin.neg())),
                error,
            },
        ))
    }

    fn is_finite(self) -> bool {
        self.value.is_finite()
            && self.slope.iter().all(|s| s.is_finite())
            && self.error.is_finite()
    }
}

/// The super module's `lab_to_srgb`, step for step.
fn lab_to_srgb([l, a, b]: [Bound; 3]) -> Option<[Bound; 3]> {
    let lightness = l.divide(116.0);
    let (a500, b200) = (a.divide(500.0), b.divide(200.0));
    let fy = lightness.offset(16.0 / 116.0);
    let (fx, fz) = (fy.add(a500), fy.add(b200.neg()));
    let cube = |f: Bound, linear: Bound| -> Option<Bound> {
        let cubed = f.cube();
        Some(match cubed.above(LAB_EPSILON)? {
            true => cubed,
            false => linear.scale(116.0 / LAB_KAPPA),
        })
    };
    // The branch is on the channel itself, which carries no error.
    let y = match l.above(LAB_KAPPA * LAB_EPSILON)? {
        true => l.offset(16.0).divide(116.0).cube(),
        false => l.divide(LAB_KAPPA),
    };
    let xyz_d50 = [
        cube(fx, lightness.add(a500))?.scale(LAB_WHITE_D50[0]),
        y.scale(LAB_WHITE_D50[1]),
        cube(fz, lightness.add(b200.neg()))?.scale(LAB_WHITE_D50[2]),
    ];
    let linear =
        Bound::matrix(XYZ_TO_LINEAR_SRGB, Bound::matrix(D50_TO_D65, xyz_d50));
    let [r, g, b] = linear.map(Bound::srgb_encode);
    Some([r?, g?, b?])
}

/// The super module's `oklab_to_srgb`, step for step.
fn oklab_to_srgb(lab: [Bound; 3]) -> Option<[Bound; 3]> {
    let lms =
        Bound::matrix(OKLAB_TO_OKLAB_LMS, lab).map(Bound::cube_by_products);
    let linear =
        Bound::matrix(XYZ_TO_LINEAR_SRGB, Bound::matrix(OKLAB_LMS_TO_XYZ, lms));
    let [r, g, b] = linear.map(Bound::srgb_encode);
    Some([r?, g?, b?])
}

/// The super module's `from_polar`: `f64::to_radians` multiplies by the
/// constant `PI / 180`.
fn from_polar([l, chroma, hue]: [Bound; 3]) -> Option<[Bound; 3]> {
    let (sin, cos) = hue.scale(std::f64::consts::PI / 180.0).sin_cos()?;
    Some([l, chroma.mul(cos), chroma.mul(sin)])
}

/// One channel's share of a candidate's predicted sRGB: the term it adds
/// to each sRGB channel, and the slack that term carries.
#[derive(Clone, Copy, Debug)]
pub(super) struct Part {
    pub(super) terms: [f64; 3],
    pub(super) slack: [f64; 3],
}

/// What a candidate spelling can convert to, from the colour held.
#[derive(Debug)]
pub(super) struct Enclosure {
    /// The channels held, as the `f32`s the conversion reads.
    centre: [f64; 3],
    /// How far from the centre a candidate may be and still be bounded.
    radius: [f64; 3],
    /// The code's sRGB for the channels held, before its rounding to `f32`.
    held: [f64; 3],
    /// The `f32` cell each channel of `held` rounds into, both ends
    /// included: a value on an end may round either way.
    cells: [Interval; 3],
    /// Every derivative of each sRGB channel by each channel held, over the
    /// box, as a midpoint and a radius that together hold the interval.
    slope_mid: [[f64; 3]; 3],
    slope_radius: [[f64; 3]; 3],
    /// How far the code's sRGB can be from the exact one in the box.
    error: [f64; 3],
}

impl Enclosure {
    /// The box the mixed search's spellings fall in.
    pub(super) const NEAR: Radius = Radius {
        relative: [1.0 / 1024.0; 3],
        absolute: [1.0 / 1_048_576.0; 3],
    };

    /// The enclosure around `declared`, a colour in a form that mixes its
    /// channels; `None` where it has none.
    pub(super) fn of(declared: &Declared, reach: Radius) -> Option<Self> {
        let centre: [f64; 3] = declared
            .channels
            .map(|c| f64::from(c.unwrap_or(0.0) as f32));
        let radius = [0, 1, 2]
            .map(|i| centre[i].abs() * reach.relative[i] + reach.absolute[i]);
        let channels =
            [0, 1, 2].map(|i| Bound::channel(i, centre[i], radius[i]));
        let srgb = match declared.form {
            Form::Lab => lab_to_srgb(channels)?,
            Form::Lch => lab_to_srgb(from_polar(channels)?)?,
            Form::Oklab => oklab_to_srgb(channels)?,
            Form::Oklch => oklab_to_srgb(from_polar(channels)?)?,
            _ => return None,
        };
        if !srgb.iter().all(|b| b.is_finite()) {
            return None;
        }
        let at_f32 = Declared {
            channels: centre.map(Some),
            ..*declared
        };
        let held = declared_srgb(&at_f32);
        if !held.iter().all(|c| c.is_finite()) {
            return None;
        }
        let cells = held.map(|c| {
            let v = c as f32;
            let below = f64::from(v.next_down());
            let above = f64::from(v.next_up());
            let v = f64::from(v);
            Interval {
                lo: (below + v) / 2.0,
                hi: (v + above) / 2.0,
            }
        });
        let slope_mid = srgb.map(|b| b.slope.map(|s| (s.lo + s.hi) / 2.0));
        let slope_radius = [0, 1, 2].map(|k| {
            [0, 1, 2].map(|i| {
                let s = srgb[k].slope[i];
                let mid = slope_mid[k][i];
                (s.hi - mid).max(mid - s.lo).next_up()
            })
        });
        Some(Self {
            centre,
            radius,
            held,
            cells,
            slope_mid,
            slope_radius,
            error: srgb.map(|b| b.error),
        })
    }

    /// One channel's share of a candidate's predicted sRGB, for a channel
    /// that reads as the `f32` `value`: computed once per decimal and summed
    /// per candidate by [`Enclosure::refuses_parts`]. `None` outside half
    /// the box: half, so that a difference rounded down cannot let one in
    /// from outside. That takes in a hue that wrapped through zero to sit
    /// near the hue held -- read back as `0.001` against a hue held at
    /// `359.999` -- since its difference is near a whole turn; the
    /// derivatives were bounded over the box and not across the wrap, and
    /// refusing nothing there costs one conversion.
    pub(super) fn part(&self, index: usize, value: f64) -> Option<Part> {
        let delta = value - self.centre[index];
        if delta.abs() > self.radius[index] / 2.0 {
            return None;
        }
        // The true difference is within half an ulp of `delta`, and each
        // derivative within its radius of its midpoint.
        Some(Part {
            terms: [0, 1, 2].map(|k| self.slope_mid[k][index] * delta),
            slack: [0, 1, 2].map(|k| {
                (self.slope_radius[k][index]
                    + self.slope_mid[k][index].abs() * ROUNDING)
                    * delta.abs()
                    * (1.0 + ROUNDING)
            }),
        })
    }

    /// Whether a candidate made of `parts`, one per channel, certainly
    /// converts outside the cell of some channel of the colour held, so
    /// cannot repaint it.
    ///
    /// In plain `f64`, every rounding taken into the slack: the parts' own
    /// above, and the sum of seven rounded terms within `7 * f64::EPSILON /
    /// 2` of their magnitudes -- taken as eight whole epsilons, and the
    /// slack's own arithmetic as four more.
    pub(super) fn refuses_parts(&self, parts: [&Part; 3]) -> bool {
        (0..3).any(|k| {
            let mut sum = self.held[k];
            let mut magnitude = sum.abs();
            let mut slack = 0.0;
            for part in parts {
                sum += part.terms[k];
                magnitude += part.terms[k].abs();
                slack += part.slack[k];
            }
            self.outside(k, sum, magnitude, slack)
        })
    }

    /// The colour held's sRGB channel `k`, before rounding.
    pub(super) fn held(&self, k: usize) -> f64 {
        self.held[k]
    }

    /// Whether a prediction for sRGB channel `k` -- `sum`, the held value
    /// and up to six terms added in any order, `magnitude` the sum of their
    /// magnitudes and `slack` of their slacks -- certainly lies outside the
    /// channel's cell. The rounding of up to seven additions in any order is
    /// within `7 * f64::EPSILON / 2` of `magnitude`, taken as eight whole
    /// epsilons, the slack's own arithmetic as four more, and twice the
    /// conversion's rounding bound is added here.
    pub(super) fn outside(
        &self,
        k: usize,
        sum: f64,
        magnitude: f64,
        slack: f64,
    ) -> bool {
        let slack = (slack + 2.0 * self.error[k]) * (1.0 + 4.0 * ROUNDING)
            + 8.0 * ROUNDING * magnitude;
        sum + slack < self.cells[k].lo || sum - slack > self.cells[k].hi
    }

    /// Whether a candidate whose channels read as the `f32`s `channels`
    /// certainly cannot repaint the colour held: [`Enclosure::part`] of each
    /// and [`Enclosure::refuses_parts`].
    pub(super) fn refuses(&self, channels: [f64; 3]) -> bool {
        let [a, b, c] = [0, 1, 2].map(|i| self.part(i, channels[i]));
        match (a, b, c) {
            (Some(a), Some(b), Some(c)) => self.refuses_parts([&a, &b, &c]),
            _ => false,
        }
    }
}
