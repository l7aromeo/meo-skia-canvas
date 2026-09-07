//! Gradient interpolation against an externally derived reference table.
//!
//! Every expected value below has two independent sources: the CSS Color 4
//! and Oklab conversion formulae, computed in a script that reads nothing
//! from this library, and Chrome 148 measured through `color-mix()` read back
//! with `getComputedStyle` and through its own canvas gradients. Neither
//! source is this implementation, which is the point -- a table read back
//! from the code under test asserts that the code does what it does.
//!
//! **Anchored at the midpoint, on endpoints chosen to avoid a rounding tie.**
//! The obvious pair, `rgb(255 0 0)` to `rgb(0 0 255)`, has an sRGB midpoint of
//! exactly 127.5 -- macOS rounds it up and Linux rounds it down, so no exact
//! value is right on both and a tolerance wide enough to accept either cannot
//! tell a platform apart from a defect. Black to white is the same tie, and
//! `srgb-linear` lands on 187.516, a sixtieth of a level from flipping.
//!
//! The pairs below were searched for instead: every expected channel, in every
//! space, sits at least 0.23 of a level from the nearest `.5`. Combined with
//! `set_gpu(false)`, which takes the deterministic raster path rather than a
//! backend that interpolates the ramp at reduced precision, that makes these
//! values exact on every platform -- so the assertions are equality and a
//! one-level disagreement is a real finding rather than noise.
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
    // The raster path, so the ramp is the same arithmetic on every runner.
    canvas.set_gpu(false);
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

/// The stop pair, agreed with the binding suite so a value that disagrees
/// between the two means something.
///
/// Every channel sum is even, so the sRGB midpoint is the exact integer
/// `122, 70, 130`: no channel sits on a `.5`, which is the property that
/// matters. `255, 0, 0` to `0, 0, 255` puts *two* channels on exactly 127.5,
/// where macOS rounds up and Linux rounds down and no expected value is right
/// on both -- the tie a `<= 1` tolerance here was absorbing while the
/// JavaScript suite went red on it.
///
/// It separates more of the list than red to blue did. Both ends of that pair
/// sit on primaries the spaces share, which collapsed `A98Rgb` onto `Srgb`;
/// here they are 131 and 122. The collapses left are the necessary ones --
/// `Destination` is `Srgb` on an sRGB canvas, and the three XYZ names are one
/// space under three spellings.
fn from_stop() -> RgbaLinear {
    RgbaLinear::from_srgb8(200, 56, 70, 1.0)
}
fn to_stop() -> RgbaLinear {
    RgbaLinear::from_srgb8(44, 84, 190, 1.0)
}

/// Exact equality on the three colour channels.
///
/// A tolerance stood here and hid a real platform difference for a day: `<= 1`
/// on a midpoint of exactly 127.5 accepts both 127 and 128, which is every
/// answer there is, so it could not fail while the binding suite's exact
/// assertion on the same value went red on Linux. The stops are off the tie
/// now and `set_gpu(false)` fixes the backend, so equality is the honest
/// assertion and any difference at all is a finding.
fn exact(got: [u8; 4], want: [u8; 3], why: &str) {
    assert_eq!(
        [got[0], got[1], got[2]],
        want,
        "{why}: got {got:?}, reference says {want:?}",
    );
}

/// The stop pair, midpoint, in each of the sixteen spaces.
///
/// The pair is chosen because the spaces must disagree on it: sRGB and Oklab
/// are 60 levels apart in red and 83 in green. A pair near the neutral axis
/// would agree everywhere and pass against any implementation, correct or
/// not -- see `every_space_agrees_on_a_pair_that_cannot_discriminate`.
#[test]
fn each_interpolation_space_mixes_the_pair_its_own_way() {
    // space, expected midpoint, and what the row rules out.
    let table: &[(GradientColorSpace, [u8; 3], &str)] = &[
        (
            GradientColorSpace::Destination,
            [122, 70, 130],
            "follows the surface, which is sRGB here",
        ),
        (
            GradientColorSpace::Srgb,
            [122, 70, 130],
            "gamma-encoded sRGB, the Canvas default",
        ),
        (
            GradientColorSpace::SrgbLinear,
            [149, 72, 146],
            "linear light, 27 levels above the default",
        ),
        (
            GradientColorSpace::DisplayP3,
            [126, 73, 132],
            "a wider primary set, still gamma-encoded",
        ),
        (
            GradientColorSpace::A98Rgb,
            [131, 70, 131],
            "distinct from Srgb; red to blue collapsed these",
        ),
        (
            GradientColorSpace::ProphotoRgb,
            [146, 73, 135],
            "the widest RGB gamut in the list",
        ),
        (
            GradientColorSpace::Rec2020,
            [136, 74, 133],
            "between A98Rgb and ProphotoRgb, as its gamut is",
        ),
        (
            GradientColorSpace::XyzD65,
            [149, 72, 146],
            "linear light again, so it equals SrgbLinear",
        ),
        (
            GradientColorSpace::Xyz,
            [149, 72, 146],
            "the same space as XyzD65 under a shorter name",
        ),
        (
            GradientColorSpace::XyzD50,
            [149, 72, 146],
            "a different white point, same result once resolved",
        ),
        (
            GradientColorSpace::Lab,
            [148, 74, 129],
            "CIE Lab through the D50 adaptation",
        ),
        (
            GradientColorSpace::Oklab,
            [132, 85, 136],
            "the only row that raises green to 85",
        ),
        (
            GradientColorSpace::Lch,
            [171, 47, 146],
            "polar Lab: chroma stays high through the arc",
        ),
        (
            GradientColorSpace::Oklch,
            [150, 60, 163],
            "polar Oklab, distinct from Oklab and from Lch",
        ),
        (
            GradientColorSpace::Hsl,
            [168, 50, 195],
            "hue arc; blue goes furthest of any row",
        ),
        (
            GradientColorSpace::Hwb,
            [168, 50, 195],
            "the same bytes as Hsl on this pair, deliberately",
        ),
    ];
    for (space, want, why) in table {
        exact(
            midpoint(from_stop(), to_stop(), (*space).into()),
            *want,
            &format!("{space:?} -- {why}"),
        );
    }
}

/// The four hue methods, on two pairs that between them separate all four.
///
/// **Asserted as structure rather than as absolute channel values.** A hue
/// midpoint depends on where the 8-bit endpoints land once round-tripped
/// through HSL: `hsl(20)` and `hsl(250)` stored as bytes are not exactly
/// 20 and 250 degrees, so the midpoint of the long arc computes to 63.75
/// from float endpoints and renders 63, while the same nominal 64 on the
/// `0 -> 90` pair renders 64. Pinning either number would assert a rounding
/// model rather than the behaviour, and the model is the part I cannot
/// derive exactly. The absolute values are pinned in
/// `each_interpolation_space_mixes_the_pair_its_own_way`, where the
/// reference is exact.
///
/// What a hue method decides is which way round the circle the arc travels,
/// and that is what is asserted: the four partition into two pairs, and the
/// partition is *different* on the two stop pairs. On `20 -> 250` the arc
/// exceeds 180, so `Shorter` turns back and joins `Decreasing` while
/// `Longer` joins `Increasing`; on `340 -> 20` the wrap puts `Shorter` with
/// `Increasing` and `Longer` with `Decreasing`. Each method sits in a
/// different pair each time, which is what identifies it -- one stop pair
/// identifies none of them.
#[test]
fn two_hue_pairs_separate_all_four_hue_methods() {
    let hsl = |h: f32| {
        let (r, g, b) = hsl_to_rgb8(h);
        RgbaLinear::from_srgb8(r, g, b, 1.0)
    };
    let by = |from: f32, to: f32, m: HueMethod| {
        midpoint(hsl(from), hsl(to), GradientColorSpace::Hsl.hue(m))
    };
    for (from, to, with_shorter, with_longer) in [
        (
            20.0f32,
            250.0f32,
            HueMethod::Decreasing,
            HueMethod::Increasing,
        ),
        (340.0, 20.0, HueMethod::Increasing, HueMethod::Decreasing),
    ] {
        let short = by(from, to, HueMethod::Shorter);
        let long = by(from, to, HueMethod::Longer);
        assert_ne!(
            short, long,
            "hsl {from} -> {to}: the two arcs must differ or the pair \
             discriminates nothing",
        );
        assert_eq!(
            by(from, to, with_shorter),
            short,
            "hsl {from} -> {to}: {with_shorter:?} takes the same arc as Shorter",
        );
        assert_eq!(
            by(from, to, with_longer),
            long,
            "hsl {from} -> {to}: {with_longer:?} takes the same arc as Longer",
        );
    }

    // The partition itself flips between the two stop pairs, which is what
    // makes two pairs identify four methods where one identifies none.
    assert_eq!(
        by(20.0, 250.0, HueMethod::Decreasing),
        by(20.0, 250.0, HueMethod::Shorter),
    );
    assert_eq!(
        by(340.0, 20.0, HueMethod::Increasing),
        by(340.0, 20.0, HueMethod::Shorter),
    );
    assert_ne!(
        by(20.0, 250.0, HueMethod::Increasing),
        by(20.0, 250.0, HueMethod::Shorter),
        "Increasing joins Shorter on one pair and not the other",
    );
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
    exact(by(HueMethod::Shorter), [255, 191, 0], "the short arc");
    exact(by(HueMethod::Longer), [0, 64, 255], "the long arc");
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
        GradientColorSpace::Destination,
        GradientColorSpace::Srgb,
        GradientColorSpace::SrgbLinear,
        GradientColorSpace::DisplayP3,
        GradientColorSpace::A98Rgb,
        GradientColorSpace::ProphotoRgb,
        GradientColorSpace::Rec2020,
        GradientColorSpace::XyzD65,
        GradientColorSpace::Xyz,
        GradientColorSpace::XyzD50,
        GradientColorSpace::Lab,
        GradientColorSpace::Oklab,
        GradientColorSpace::Lch,
        GradientColorSpace::Oklch,
        GradientColorSpace::Hsl,
        GradientColorSpace::Hwb,
    ] {
        exact(
            midpoint(from_stop(), from_stop(), space.into()),
            [200, 56, 70],
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
            [149, 72, 146],
            "Srgb against SrgbLinear",
        ),
        (
            GradientColorSpace::SrgbLinear,
            [122, 70, 130],
            "SrgbLinear against Srgb",
        ),
        (
            GradientColorSpace::DisplayP3,
            [131, 70, 131],
            "DisplayP3 against A98Rgb",
        ),
        (
            GradientColorSpace::A98Rgb,
            [122, 70, 130],
            "A98Rgb against Srgb, which red to blue could not separate",
        ),
        (
            GradientColorSpace::ProphotoRgb,
            [136, 74, 133],
            "ProphotoRgb against Rec2020",
        ),
        (
            GradientColorSpace::Rec2020,
            [146, 73, 135],
            "Rec2020 against ProphotoRgb",
        ),
        (GradientColorSpace::Lab, [132, 85, 136], "Lab against Oklab"),
        (
            GradientColorSpace::Oklab,
            [148, 74, 129],
            "Oklab against Lab",
        ),
        (GradientColorSpace::Lch, [150, 60, 163], "Lch against Oklch"),
        (
            GradientColorSpace::Oklch,
            [171, 47, 146],
            "Oklch against Lch",
        ),
        (GradientColorSpace::Hsl, [122, 70, 130], "Hsl against Srgb"),
        (
            GradientColorSpace::XyzD65,
            [122, 70, 130],
            "XyzD65 against Srgb",
        ),
    ];
    let mut indistinguishable = Vec::new();
    for (space, wrong, label) in table {
        let got = midpoint(from_stop(), to_stop(), (*space).into());
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
/// Pure red here rather than the pair above: this row is about alpha, not
/// about the space, and `255, 0, 0` fading to transparent puts the sampled
/// column 0.495 of a level from a tie -- as clean as the ramp gets. The pair
/// used elsewhere would have landed the green channel on 1.495, which is the
/// tie this file exists to avoid.
///
/// Sampled at x=30, where alpha is still 178 of 255. Further down the ramp
/// unpremultiplying divides by a small alpha and magnifies the half-level of
/// quantization in premultiplied storage, which is a property of how the
/// colour is stored and not of the interpolation.
///
/// Premultiplied would hold red at 255 the whole way and read `[255, 0, 0,
/// 178]` here -- a 77-level gap, and what CSS `color-mix()` gives for the
/// same two stops. Chrome's canvas agrees with the unpremultiplied column.
#[test]
fn alpha_interpolates_unpremultiplied_by_default() {
    let mut canvas = Canvas::new(WIDTH, 4.0);
    canvas.set_gpu(false);
    {
        let ctx = canvas.context();
        let shader = Shader::linear_gradient(
            Point { x: 0.0, y: 0.0 },
            Point { x: WIDTH, y: 0.0 },
            &[
                GradientStop {
                    position: 0.0,
                    color: RgbaLinear::from_srgb8(255, 0, 0, 1.0),
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
    let i = ((2 * WIDTH as u32 + 30) * 4) as usize;
    let got = [buffer[i], buffer[i + 1], buffer[i + 2], buffer[i + 3]];
    assert_eq!(
        got,
        [178, 0, 0, 178],
        "red falls with alpha; premultiplied would read [255, 0, 0, 178]",
    );
}

/// A near-neutral pair: the default, and the lightness curves.
///
/// Red to blue separates the spaces by hue and leaves the gamma handling
/// almost untested. This is its complement: chroma is near zero, so the
/// midpoint is a grey that depends only on how each space treats lightness.
///
/// The 129 is the one that matters. `GradientColorSpace::Srgb` once mapped to
/// Skia's `SRGBLinear`, so a default gradient came out washed out -- 187 here
/// where 129 belongs. The doc comment records it; nothing outside this tree
/// pinned it, because that table is our own output written down and cannot
/// catch the case where our output is what is wrong.
///
/// `4` and `254` rather than black and white: those give an sRGB midpoint of
/// exactly 127.5, the same tie as the pure red-to-blue pair, and every space
/// here sits at least 0.36 of a level clear of one.
///
/// **What this pair cannot do** is separate `Lab` from `Lch`, `Oklab` from
/// `Oklch`, or `Srgb` from `Hsl` and `Hwb`: with no chroma the polar spaces
/// have no hue to travel, which Chrome reports directly as a hue of `none`.
/// Those four are asserted as equalities and separated by red to blue
/// instead. Either pair alone leaves rows that look like independent
/// coverage and are not.
#[test]
fn a_near_neutral_pair_pins_the_default_and_the_lightness_curves() {
    let dark = RgbaLinear::from_srgb8(4, 4, 4, 1.0);
    let light = RgbaLinear::from_srgb8(254, 254, 254, 1.0);
    let table: &[(GradientColorSpace, u8, &str)] = &[
        (
            GradientColorSpace::Srgb,
            129,
            "the default; 187 would be the SRGBLinear regression",
        ),
        (
            GradientColorSpace::SrgbLinear,
            187,
            "linear light, 58 levels above the default",
        ),
        (
            GradientColorSpace::Lab,
            120,
            "CIE lightness is not sRGB's midpoint",
        ),
        (
            GradientColorSpace::Oklab,
            114,
            "Oklab lightness, 6 levels below Lab's",
        ),
        (
            GradientColorSpace::Lch,
            120,
            "achromatic, so it must equal Lab",
        ),
        (
            GradientColorSpace::Oklch,
            114,
            "achromatic, so it must equal Oklab",
        ),
        (
            GradientColorSpace::Hsl,
            129,
            "achromatic, so it must equal Srgb",
        ),
        (
            GradientColorSpace::Hwb,
            129,
            "achromatic, so it must equal Srgb",
        ),
    ];
    for (space, want, why) in table {
        exact(
            midpoint(dark, light, (*space).into()),
            [*want, *want, *want],
            &format!("{space:?} near-neutral -- {why}"),
        );
    }
}
