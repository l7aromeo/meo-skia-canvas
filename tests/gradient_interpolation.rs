//! Gradient interpolation against an externally derived reference table.
//!
//! Every expected value below has two independent sources: the CSS Color 4
//! and Oklab conversion formulae, computed in a script that reads nothing
//! from this library, and Chrome 148 measured through `color-mix()` read back
//! with `getComputedStyle` and through its own canvas gradients. Neither
//! source is this implementation, which is the point -- a table read back
//! from the code under test asserts that the code does what it does.
//!
//! **Anchored at the midpoint.** The two sources agree exactly there in every
//! space. Off the midpoint they differ by up to one level: a canvas ramp
//! samples at pixel centres, so `t` is `(x + 0.5) / width` and the sRGB red
//! channel at x=25 of 101 is 190.619, which this project has already recorded
//! reading 191 on one backend and 190 on another. The midpoint of a
//! 101-pixel ramp is the one column where `t` is exactly 0.5.
use meo_skia_canvas::prelude::*;

/// Renders and returns unencoded RGBA.
fn pixels(canvas: &mut Canvas) -> Vec<u8> {
    canvas
        .to_buffer(ImageFormat::Raw, &EncodeOptions::default())
        .expect("raw export")
}

const WIDTH: f32 = 101.0;

/// The midpoint pixel of a two-stop ramp drawn across `WIDTH`.
fn midpoint(
    from: RgbaLinear,
    to: RgbaLinear,
    interp: GradientInterpolation,
) -> [u8; 4] {
    let mut canvas = Canvas::new(WIDTH, 4.0);
    {
        let ctx = canvas.context();
        let shader = Shader::linear_gradient(
            Point { x: 0.0, y: 0.0 },
            Point { x: WIDTH, y: 0.0 },
            &[
                GradientStop {
                    position: 0.0,
                    color: from,
                },
                GradientStop {
                    position: 1.0,
                    color: to,
                },
            ],
            interp,
        )
        .expect("gradient");
        ctx.set_fill_shader(&shader);
        ctx.fill_rect(0.0, 0.0, WIDTH, 4.0);
    }
    let buffer = pixels(&mut canvas);
    let i = ((2 * WIDTH as u32 + 50) * 4) as usize;
    [buffer[i], buffer[i + 1], buffer[i + 2], buffer[i + 3]]
}

fn red() -> RgbaLinear {
    RgbaLinear::from_srgb8(255, 0, 0, 1.0)
}
fn blue() -> RgbaLinear {
    RgbaLinear::from_srgb8(0, 0, 255, 1.0)
}

/// Within one level, which is the documented spread between this project's
/// raster and GPU backends on a single ramp column.
fn near(got: [u8; 4], want: [u8; 3], why: &str) {
    for (channel, (g, w)) in
        ["r", "g", "b"].iter().zip(got.iter().zip(want.iter()))
    {
        assert!(
            (*g as i32 - *w as i32).abs() <= 1,
            "{why}: {channel} was {g}, reference says {w} (got {got:?}, want {want:?})",
        );
    }
}

/// Red to blue, midpoint, in each interpolation space.
///
/// The pair is chosen because the spaces must disagree on it: sRGB and Oklab
/// are 60 levels apart in red and 83 in green. A pair near the neutral axis
/// would agree everywhere and pass against any implementation, correct or
/// not -- see `every_space_agrees_on_a_pair_that_cannot_discriminate`.
#[test]
fn each_interpolation_space_mixes_red_and_blue_its_own_way() {
    // space, expected midpoint, and what the row rules out.
    let table: &[(GradientColorSpace, [u8; 3], &str)] = &[
        (
            GradientColorSpace::Srgb,
            [128, 0, 128],
            "gamma-encoded sRGB, the Canvas default",
        ),
        (
            GradientColorSpace::SrgbLinear,
            [188, 0, 188],
            "linear light: separates from Srgb by 60 levels",
        ),
        (
            GradientColorSpace::Lab,
            [193, 0, 136],
            "CIE Lab through the D50 adaptation",
        ),
        (
            GradientColorSpace::Oklab,
            [140, 83, 162],
            "Oklab: the only space here with green in the mix",
        ),
        (
            GradientColorSpace::Lch,
            [245, 0, 134],
            "polar Lab: chroma stays high through the arc",
        ),
        (
            GradientColorSpace::Oklch,
            [186, 0, 194],
            "polar Oklab, distinct from both Oklab and Lch",
        ),
        (
            GradientColorSpace::Hsl,
            [255, 0, 255],
            "hue arc at full saturation, so the midpoint saturates",
        ),
        (
            GradientColorSpace::Hwb,
            [255, 0, 255],
            "same arc as Hsl; the two agree on a fully saturated pair",
        ),
    ];
    for (space, want, why) in table {
        near(
            midpoint(red(), blue(), (*space).into()),
            *want,
            &format!("{space:?} -- {why}"),
        );
    }
}

/// The four hue methods, on two pairs that between them separate all four.
///
/// One pair is not enough. On `20deg -> 250deg` the arc exceeds 180, so
/// `shorter` turns back and lands with `decreasing` while `longer` lands with
/// `increasing`; on `340deg -> 20deg` the wrap puts `shorter` with
/// `increasing` and `longer` with `decreasing`. Only a method appearing in a
/// different pair each time is identified, and each of the four does.
#[test]
fn two_hue_pairs_separate_all_four_hue_methods() {
    let hsl = |h: f32| {
        let (r, g, b) = hsl_to_rgb8(h);
        RgbaLinear::from_srgb8(r, g, b, 1.0)
    };
    let cases: &[(f32, f32, [[u8; 3]; 4])] = &[
        // shorter, longer, increasing, decreasing
        (
            20.0,
            250.0,
            [[255, 0, 191], [0, 255, 64], [0, 255, 64], [255, 0, 191]],
        ),
        (
            340.0,
            20.0,
            [[255, 0, 0], [0, 255, 255], [255, 0, 0], [0, 255, 255]],
        ),
    ];
    let methods = [
        HueMethod::Shorter,
        HueMethod::Longer,
        HueMethod::Increasing,
        HueMethod::Decreasing,
    ];
    for (from, to, wants) in cases {
        for (method, want) in methods.iter().zip(wants.iter()) {
            let interp = GradientColorSpace::Hsl.hue(*method);
            near(
                midpoint(hsl(*from), hsl(*to), interp),
                *want,
                &format!("hsl {from} -> {to} with {method:?}"),
            );
        }
    }
}

/// A pair whose hue arc is 90 degrees, which **cannot** tell `Shorter` from
/// `Increasing` nor `Longer` from `Decreasing`.
///
/// Kept, and labelled, because it is the shape of a test that looks like
/// coverage and is not: it passes whether or not the four methods are
/// distinguished at all. The pair above is what actually separates them.
#[test]
fn a_ninety_degree_hue_arc_cannot_separate_shorter_from_increasing() {
    let hsl = |h: f32| {
        let (r, g, b) = hsl_to_rgb8(h);
        RgbaLinear::from_srgb8(r, g, b, 1.0)
    };
    let by = |m: HueMethod| {
        midpoint(hsl(0.0), hsl(90.0), GradientColorSpace::Hsl.hue(m))
    };
    assert_eq!(
        by(HueMethod::Shorter),
        by(HueMethod::Increasing),
        "a 90 degree arc travels the same way under both",
    );
    assert_eq!(
        by(HueMethod::Longer),
        by(HueMethod::Decreasing),
        "and the long way round is the same under both",
    );
    near(by(HueMethod::Shorter), [255, 191, 0], "the short arc");
    near(by(HueMethod::Longer), [0, 64, 255], "the long arc");
}

/// Identical stops: every space and every hue method must return the stop.
///
/// This row discriminates **nothing**, and that is why it is here. It is the
/// control for the harness rather than for the implementation: if it fails,
/// the sampling, the ramp width or the pixel index is wrong, and every other
/// number in this file is measuring the wrong column.
#[test]
fn every_space_agrees_on_a_pair_that_cannot_discriminate() {
    for space in [
        GradientColorSpace::Srgb,
        GradientColorSpace::SrgbLinear,
        GradientColorSpace::Lab,
        GradientColorSpace::Oklab,
        GradientColorSpace::Lch,
        GradientColorSpace::Oklch,
        GradientColorSpace::Hsl,
        GradientColorSpace::Hwb,
    ] {
        near(
            midpoint(red(), red(), space.into()),
            [255, 0, 0],
            &format!("{space:?} on identical stops"),
        );
    }
}

/// `hsl(h 100% 50%)` as 8-bit sRGB, so the tests above can name a hue.
fn hsl_to_rgb8(h: f32) -> (u8, u8, u8) {
    let h = h.rem_euclid(360.0);
    let x = 1.0 - ((h / 60.0) % 2.0 - 1.0).abs();
    let (r, g, b) = match (h / 60.0) as u32 {
        0 => (1.0, x, 0.0),
        1 => (x, 1.0, 0.0),
        2 => (0.0, 1.0, x),
        3 => (0.0, x, 1.0),
        4 => (x, 0.0, 1.0),
        _ => (1.0, 0.0, x),
    };
    let to8 = |v: f32| (v * 255.0).round() as u8;
    (to8(r), to8(g), to8(b))
}
/// Every row of the table above differs from its neighbours' value.
///
/// Without this the eight rows could all be measuring the same column, or the
/// same wrong one, and would still read as eight passes. Each space is
/// asserted against a *different* space's reference value and must fail; a
/// row that passed here would be one that cannot discriminate, and the table
/// would be decoration.
#[test]
fn each_row_can_tell_its_space_from_another() {
    let table: &[(GradientColorSpace, [u8; 3], &str)] = &[
        (
            GradientColorSpace::Srgb,
            [188, 0, 188],
            "Srgb against SrgbLinear",
        ),
        (
            GradientColorSpace::SrgbLinear,
            [128, 0, 128],
            "SrgbLinear against Srgb",
        ),
        (GradientColorSpace::Lab, [140, 83, 162], "Lab against Oklab"),
        (
            GradientColorSpace::Oklab,
            [193, 0, 136],
            "Oklab against Lab",
        ),
        (GradientColorSpace::Lch, [186, 0, 194], "Lch against Oklch"),
        (
            GradientColorSpace::Oklch,
            [245, 0, 134],
            "Oklch against Lch",
        ),
        (GradientColorSpace::Hsl, [128, 0, 128], "Hsl against Srgb"),
        (GradientColorSpace::Hwb, [140, 83, 162], "Hwb against Oklab"),
    ];
    let mut indistinguishable = Vec::new();
    for (space, wrong, label) in table {
        let got = midpoint(red(), blue(), (*space).into());
        if (0..3).all(|i| (got[i] as i32 - wrong[i] as i32).abs() <= 1) {
            indistinguishable.push(*label);
        }
    }
    assert!(
        indistinguishable.is_empty(),
        "these rows cannot discriminate and prove nothing: {indistinguishable:?}",
    );
}

/// Alpha interpolates unpremultiplied, which is the Canvas rule.
///
/// The colour travels toward the other stop as alpha falls, rather than the
/// hue being held. Sampled at x=25 rather than the midpoint: unpremultiplied
/// and premultiplied differ by 64 levels of red there, and alpha is still
/// high enough that unpremultiplying does not magnify the half-level of
/// quantization in premultiplied storage -- at x=75 the same ramp reads 68
/// against a computed 64 for that reason, which is a property of the storage
/// and not of the interpolation.
///
/// Premultiplied would read `[255, 0, 0, 191]` here, which is what CSS
/// `color-mix()` gives for the same two stops and what any future
/// `alpha: "premultiplied"` option must produce. Chrome's canvas agrees with
/// the unpremultiplied column exactly.
#[test]
fn alpha_interpolates_unpremultiplied_by_default() {
    let mut canvas = Canvas::new(WIDTH, 4.0);
    {
        let ctx = canvas.context();
        let shader = Shader::linear_gradient(
            Point { x: 0.0, y: 0.0 },
            Point { x: WIDTH, y: 0.0 },
            &[
                GradientStop {
                    position: 0.0,
                    color: red(),
                },
                GradientStop {
                    position: 1.0,
                    color: RgbaLinear::new_premultiplied(0.0, 0.0, 0.0, 0.0),
                },
            ],
            GradientColorSpace::Srgb,
        )
        .expect("gradient");
        ctx.set_fill_shader(&shader);
        ctx.fill_rect(0.0, 0.0, WIDTH, 4.0);
    }
    let buffer = pixels(&mut canvas);
    let i = ((2 * WIDTH as u32 + 25) * 4) as usize;
    let got = [buffer[i], buffer[i + 1], buffer[i + 2], buffer[i + 3]];
    assert!(
        (got[0] as i32 - 191).abs() <= 1 && (got[3] as i32 - 191).abs() <= 1,
        "red falls with alpha: got {got:?}, reference says [191, 0, 64, 191]",
    );
    assert!(
        got[0] < 240,
        "premultiplied would hold red at 255 here; got {got:?}",
    );
}

/// Black to white: the pair that pins the default, and the regression the
/// doc comment on `GradientColorSpace::Srgb` records.
///
/// `Srgb` once mapped to Skia's `SRGBLinear`, so a default gradient came out
/// washed out -- 188 at this midpoint where 128 belongs. That is fixed, and
/// nothing outside this tree pinned it: the table in the doc comment is our
/// own output written down, which cannot catch the case where our output is
/// what is wrong. Both numbers below come from the formulae and from Chrome.
///
/// **This pair is the complement of red-to-blue, not a substitute.** On the
/// neutral axis chroma is zero, so `Lab` and `Lch` give the same grey, so do
/// `Oklab` and `Oklch`, and `Srgb`, `Hsl` and `Hwb` all give 128 -- Chrome
/// reports the hue as `none` for the polar spaces here, which is the same
/// observation. What it separates, red-to-blue cannot: 128, 188, 119 and 99
/// are four distinct greys where the gamma handling and the lightness curve
/// are the whole difference.
#[test]
fn black_to_white_pins_the_default_and_the_lightness_curves() {
    let black = RgbaLinear::from_srgb8(0, 0, 0, 1.0);
    let white = RgbaLinear::from_srgb8(255, 255, 255, 1.0);
    let table: &[(GradientColorSpace, u8, &str)] = &[
        (
            GradientColorSpace::Srgb,
            128,
            "the default; 188 here is the SRGBLinear regression",
        ),
        (
            GradientColorSpace::SrgbLinear,
            188,
            "linear light, 60 levels above the default",
        ),
        (
            GradientColorSpace::Lab,
            119,
            "CIE lightness: L=50 is not sRGB 128",
        ),
        (
            GradientColorSpace::Oklab,
            99,
            "Oklab lightness, 20 levels below Lab's",
        ),
        (
            GradientColorSpace::Lch,
            119,
            "achromatic, so it must equal Lab",
        ),
        (
            GradientColorSpace::Oklch,
            99,
            "achromatic, so it must equal Oklab",
        ),
        (
            GradientColorSpace::Hsl,
            128,
            "achromatic, so it must equal Srgb",
        ),
        (
            GradientColorSpace::Hwb,
            128,
            "achromatic, so it must equal Srgb",
        ),
    ];
    for (space, want, why) in table {
        near(
            midpoint(black, white, (*space).into()),
            [*want, *want, *want],
            &format!("{space:?} black to white -- {why}"),
        );
    }
}
