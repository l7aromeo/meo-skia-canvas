//! The exact Gaussian blur, written apart from any code that draws one,
//! for tests to hold a drawn blur to.

/// The error function: by the series `2x/sqrt(pi) e^(-x^2) sum (2x^2)^n /
/// (1 3 5 ... (2n+1))` below 3, where every term is positive, and past it
/// by the complement's continued fraction.
pub fn erf(x: f64) -> f64 {
    let a = x.abs();
    let value = match a < 3.0 {
        true => {
            let (mut term, mut sum, mut n) = (1.0, 1.0, 0.0);
            while term > sum * f64::EPSILON {
                n += 1.0;
                term *= 2.0 * a * a / (2.0 * n + 1.0);
                sum += term;
            }
            2.0 * a / std::f64::consts::PI.sqrt() * (-a * a).exp() * sum
        }
        false => {
            let mut tail = 0.0;
            for k in (1..=80).rev() {
                tail = (f64::from(k) / 2.0) / (a + tail);
            }
            1.0 - (-a * a).exp() / std::f64::consts::PI.sqrt() / (a + tail)
        }
    };
    value.copysign(x)
}

/// `image`, `width` pixels wide, blurred by `sigma` with the Gaussian's
/// weight over each pixel, one axis and then the other.
pub fn gaussian(image: &[f64], width: usize, sigma: f64) -> Vec<f64> {
    let height = image.len() / width;
    let reach = (6.0 * sigma).ceil() as i64 + 1;
    let k = sigma * std::f64::consts::SQRT_2;
    let weights: Vec<f64> = (-reach..=reach)
        .map(|d| (erf((d as f64 + 0.5) / k) - erf((d as f64 - 0.5) / k)) / 2.0)
        .collect();
    let pass = |source: &[f64], across: bool| -> Vec<f64> {
        (0..image.len())
            .map(|i| {
                let (x, y) = ((i % width) as i64, (i / width) as i64);
                weights
                    .iter()
                    .zip(-reach..)
                    .map(|(weight, d)| {
                        let (sx, sy) = match across {
                            true => (x + d, y),
                            false => (x, y + d),
                        };
                        match (0..width as i64).contains(&sx)
                            && (0..height as i64).contains(&sy)
                        {
                            true => {
                                weight
                                    * source[(sy * width as i64 + sx) as usize]
                            }
                            false => 0.0,
                        }
                    })
                    .sum()
            })
            .collect()
    };
    pass(&pass(image, true), false)
}

/// Past this `sigma * sqrt(2)`, [`profile`] integrates each pixel itself
/// rather than differencing an antiderivative. The antiderivative's values
/// at a pixel's two sides differ by about `1 / k` of themselves, so their
/// difference times `k` carries an error of about `k * 1e-16`, which is
/// all of a faint blur's value once `k` is large; `erf` at points inside
/// the pixel has no such cancellation.
const WIDE: f64 = 1000.0;

/// Three-point Gauss-Legendre on `[-1, 1]`: nodes `0` and `+-sqrt(3 / 5)`,
/// with their weights. Exact for
/// polynomials to degree five; on a blur wider than [`WIDE`] the integrand
/// changes over one pixel by a part in a thousand, so the rule's error is
/// below any float's precision.
const GAUSS_LEGENDRE: [(f64, f64); 3] = [
    (-0.774_596_669_241_483_4, 5.0 / 9.0),
    (0.0, 8.0 / 9.0),
    (0.774_596_669_241_483_4, 5.0 / 9.0),
];

/// The interval `[a, b]` blurred by `sigma`, averaged over each of `n`
/// pixels from zero.
pub fn profile(n: usize, a: f64, b: f64, sigma: f64) -> Vec<f64> {
    let k = sigma * std::f64::consts::SQRT_2;
    if k > WIDE {
        let blurred = |x: f64| (erf((x - a) / k) - erf((x - b) / k)) / 2.0;
        return (0..n)
            .map(|i| {
                GAUSS_LEGENDRE
                    .iter()
                    .map(|(t, w)| w * blurred(i as f64 + (1.0 + t) / 2.0))
                    .sum::<f64>()
                    / 2.0
            })
            .collect();
    }
    // An antiderivative of `erf`.
    let g = |u: f64| u * erf(u) + (-u * u).exp() / std::f64::consts::PI.sqrt();
    let integral = |x: f64| k / 2.0 * (g((x - a) / k) - g((x - b) / k));
    (0..n)
        .map(|i| integral(i as f64 + 1.0) - integral(i as f64))
        .collect()
}

/// How far a value may sit from a half-level and still round either way:
/// the f32 accumulation a blur is made in carries about 1e-4 of a level.
pub const TIE: f64 = 1e-3;

/// `image`, `width` pixels wide, blurred by `sigma` with the Gaussian
/// sampled at whole-pixel offsets and normalised over its reach, one axis
/// and then the other, reading outside the image as nothing.
pub fn sampled(image: &[f64], width: usize, sigma: f64) -> Vec<f64> {
    let height = image.len() / width;
    let reach = (8.0 * sigma).ceil() as i64;
    let raw: Vec<f64> = (-reach..=reach)
        .map(|d| (-((d * d) as f64) / (2.0 * sigma * sigma)).exp())
        .collect();
    let sum: f64 = raw.iter().sum();
    let weights: Vec<f64> = raw.iter().map(|w| w / sum).collect();
    let pass = |source: &[f64], across: bool| -> Vec<f64> {
        (0..source.len())
            .map(|i| {
                let (x, y) = ((i % width) as i64, (i / width) as i64);
                weights
                    .iter()
                    .zip(-reach..)
                    .filter_map(|(weight, d)| {
                        let (sx, sy) = match across {
                            true => (x + d, y),
                            false => (x, y + d),
                        };
                        ((0..width as i64).contains(&sx)
                            && (0..height as i64).contains(&sy))
                        .then(|| {
                            weight * source[(sy * width as i64 + sx) as usize]
                        })
                    })
                    .sum()
            })
            .collect()
    };
    pass(&pass(image, true), false)
}

/// The pixels of `alpha` that are not the reference's rounded level, but
/// for those within [`TIE`] of a half-level that land on either neighbour.
pub fn misses(alpha: &[u8], reference: &[f64]) -> Vec<(usize, u8, f64)> {
    alpha
        .iter()
        .zip(reference)
        .enumerate()
        .filter(|(_, (a, r))| {
            let level = **r * 255.0;
            let tie = (level.fract() - 0.5).abs() < TIE;
            let near = (f64::from(**a) - level).abs() < 0.5 + TIE;
            f64::from(**a) != level.round() && !(tie && near)
        })
        .map(|(i, (a, r))| (i, *a, r * 255.0))
        .collect()
}
