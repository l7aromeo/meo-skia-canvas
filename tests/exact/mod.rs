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
