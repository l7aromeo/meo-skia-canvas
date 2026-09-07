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
//! tell a platform apart from a defect. Black to white is the same tie in
//! sRGB, and in `srgb-linear` it lands on 187.516.
//!
//! **Two hazards share one measure, and only one of them is rounding.** A
//! tie-breaking rule -- half-up, half-even, half-down -- chooses only when the
//! fraction is exactly `.5`, so it moves 127.5 and nothing else: at 187.516
//! every rule gives 188. What threatens a near-tie is arithmetic, not
//! direction. Anything perturbing the unrounded value by more than its
//! clearance crosses the boundary and changes the byte -- a different engine,
//! an 8-bit intermediate, a changed conversion path. Float error alone does
//! not: `f32` carries about 6e-8 relative precision, three orders of
//! magnitude short of even the tightest clearance here. So a clearance is the
//! right thing to measure for both, and the cause named for a miss is
//! different: at exactly `.5`, the platform; anywhere else, the arithmetic.
//!
//! The pairs below were searched for instead: the worst clearance from a
//! `.5` across all sixteen spaces is 0.0875, at `Lch`. Combined with
//! `set_gpu(false)`, that makes these values exact -- so the assertions are
//! equality and a one-level disagreement is a real finding rather than noise.
//!
//! **Which figures here are checked, and which are not.** Every expected
//! channel value below is asserted, so it cannot go stale in silence. The
//! *clearance* figures cannot: `0.0875` here, `0.046` and `0.060` on the
//! engine split, `0.495` and `1.495` on the alpha column, `0.36` on the
//! near-neutral pair, and the `63.75`-against-63 hue quantisation. Those are
//! measurements taken when the endpoints were chosen, and nothing in this file
//! recomputes them -- deliberately, since a clearance needs the unrounded value
//! and a float readback reports whole numbers on the GPU path, so a check built
//! on one would be silently inert there.
//!
//! So they are measurements, not bounds. **If the endpoints change, re-measure
//! rather than trusting them**; they will still read as maintained, because a
//! figure quoted next to an assertion that does not check it looks exactly
//! like one that is kept.
//!
//! **Why the raster path, in the form that does not decay.** Exact bytes have
//! to come from one named rasteriser, or the table means nothing on hardware
//! that is not this hardware. That holds whether or not the two engines
//! currently agree. They do currently differ -- see
//! `the_gpu_path_keeps_the_spaces_apart` -- but "pin the CPU because the
//! engines differ" is a reason that expires the moment they stop, and would
//! then argue for deleting the pinning and quietly making the table
//! hardware-dependent again.
use meo_skia_canvas::{canvas::EngineKind, prelude::*};

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

/// The stop pair, shared with the binding suite so a value that disagrees
/// between the two surfaces means something.
///
/// Both channel sums are even, so the sRGB midpoint is the exact integer
/// `125, 1, 127`. `255, 0, 0` to `0, 0, 255` puts *two* channels on exactly
/// 127.5, where macOS rounds up and Linux rounds down and no expected value
/// is right on both -- the tie a `<= 1` tolerance here was absorbing while
/// the binding suite went red on it.
///
/// **A pair has to clear two hazards, and they take different remedies.**
/// The tie is one. The other is that the raster and GPU engines compute
/// different floats: the binding lane found `display-p3` and `hsl` differing
/// by a level between them at clearances of 0.046 and 0.060. Neither is a
/// tie, so a check looking for exactly `.5` passes both -- and they flip
/// anyway, because the engines diverge by more than that clearance. How much
/// more is not recorded here and is not needed: what matters is that it
/// exceeds a gap that small. Both figures also sit below the 0.0875 the
/// chosen table clears, so they cannot be clearances of these endpoints --
/// they belong to a pair that was rejected. `red` to `silver` fails both
/// hazards at once, which is the clearest evidence the two are distinct.
/// The endpoints answer the first; naming a single rasteriser answers the
/// second.
///
/// Worst clearance across all sixteen spaces is 0.0875, at `Lch`. An earlier
/// note here said 0.23; that was the worst of the eight spaces the enum had
/// when this file was written, and does not cover the seven added since.
fn from_stop() -> RgbaLinear {
    RgbaLinear::from_srgb8(250, 2, 0, 1.0)
}
fn to_stop() -> RgbaLinear {
    RgbaLinear::from_srgb8(0, 0, 254, 1.0)
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
/// are 81 levels apart in green and 34 in blue, which the two rows below
/// carry -- `125, 1, 127` against `138, 82, 161`. A pair near the neutral
/// axis would agree everywhere and pass against any implementation, correct
/// or not -- see `every_space_agrees_on_a_pair_that_cannot_discriminate`.
///
/// Those two gaps are derivable from the rows rather than measured
/// separately, so unlike the clearances named in the module header they
/// cannot go stale without a row going stale with them.
#[test]
fn each_interpolation_space_mixes_the_pair_its_own_way() {
    // space, expected midpoint, and what the row rules out.
    let table: &[(GradientColorSpace, [u8; 3], &str)] = &[
        (
            GradientColorSpace::Destination,
            [125, 1, 127],
            "follows the surface, which is sRGB here",
        ),
        (
            GradientColorSpace::Srgb,
            [125, 1, 127],
            "gamma-encoded sRGB, the Canvas default",
        ),
        (
            GradientColorSpace::SrgbLinear,
            [184, 1, 187],
            "linear light, 59 levels above the default",
        ),
        (
            GradientColorSpace::DisplayP3,
            [125, 10, 144],
            "a wider primary set, still gamma-encoded",
        ),
        (
            GradientColorSpace::A98Rgb,
            [125, 0, 129],
            "distinct from Srgb, but by only two levels of blue",
        ),
        (
            GradientColorSpace::ProphotoRgb,
            [183, 4, 156],
            "the widest RGB gamut in the list",
        ),
        (
            GradientColorSpace::Rec2020,
            [159, 19, 147],
            "between A98Rgb and ProphotoRgb, as its gamut is",
        ),
        (
            GradientColorSpace::XyzD65,
            [184, 1, 187],
            "linear light again, so it equals SrgbLinear",
        ),
        (
            GradientColorSpace::Xyz,
            [184, 1, 187],
            "the same space as XyzD65 under a shorter name",
        ),
        (
            GradientColorSpace::XyzD50,
            [184, 1, 187],
            "a different white point, same result once resolved",
        ),
        (
            GradientColorSpace::Lab,
            [190, 0, 135],
            "CIE Lab through the D50 adaptation",
        ),
        (
            GradientColorSpace::Oklab,
            [138, 82, 161],
            "the only row that raises green to 82",
        ),
        (
            GradientColorSpace::Lch,
            [242, 0, 132],
            "polar Lab: chroma stays high through the arc",
        ),
        (
            GradientColorSpace::Oklch,
            [184, 0, 191],
            "polar Oklab; Metal answers 192 here, the raster path 191",
        ),
        (
            GradientColorSpace::Hsl,
            [252, 0, 251],
            "hue arc at full saturation",
        ),
        (
            GradientColorSpace::Hwb,
            [252, 0, 251],
            "identical to Hsl on this pair -- see the separate test",
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
            [250, 2, 0],
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
            [184, 1, 187],
            "Srgb against SrgbLinear",
        ),
        (
            GradientColorSpace::SrgbLinear,
            [125, 1, 127],
            "SrgbLinear against Srgb",
        ),
        (
            GradientColorSpace::DisplayP3,
            [125, 1, 127],
            "DisplayP3 against Srgb",
        ),
        (
            GradientColorSpace::A98Rgb,
            [184, 1, 187],
            "A98Rgb against SrgbLinear",
        ),
        (
            GradientColorSpace::ProphotoRgb,
            [159, 19, 147],
            "ProphotoRgb against Rec2020",
        ),
        (
            GradientColorSpace::Rec2020,
            [183, 4, 156],
            "Rec2020 against ProphotoRgb",
        ),
        (GradientColorSpace::Lab, [138, 82, 161], "Lab against Oklab"),
        (
            GradientColorSpace::Oklab,
            [190, 0, 135],
            "Oklab against Lab",
        ),
        (GradientColorSpace::Lch, [184, 0, 191], "Lch against Oklch"),
        (
            GradientColorSpace::Oklch,
            [242, 0, 132],
            "Oklch against Lch",
        ),
        (GradientColorSpace::Hsl, [125, 1, 127], "Hsl against Srgb"),
        (
            GradientColorSpace::XyzD65,
            [125, 1, 127],
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
/// column 0.495 of a level from a boundary -- as clean as the ramp gets. The
/// pair used elsewhere would have landed the green channel on 1.495. That is
/// not a tie and no rounding rule moves it, but a two-hundredth of a level is
/// crossed by any arithmetic difference at all, which is the hazard this file
/// exists to avoid.
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

/// The other half of the pair above, so the default is a choice rather than
/// the only thing the code can do.
///
/// Same stops, same sample, same space -- only `AlphaInterpolation` differs,
/// which is what makes the gap attributable to it. Premultiplied holds red at
/// full strength as the alpha falls, because the stored value it walks is
/// already multiplied by alpha and dividing it back out returns 255.
///
/// Without this the sibling's failure message was a prediction: nothing ran
/// the premultiplied path from Rust, and a `Premultiplied` that silently did
/// nothing would have left every Rust assertion green.
#[test]
fn alpha_interpolates_premultiplied_when_asked() {
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
            GradientInterpolation::from(GradientColorSpace::Srgb)
                .with_alpha(AlphaInterpolation::Premultiplied),
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
        [255, 0, 0, 178],
        "premultiplied holds the colour as alpha falls; \
         unpremultiplied reads [178, 0, 0, 178]",
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

/// `Hsl` and `Hwb` are different spaces, on a pair that can show it.
///
/// The shared pair cannot: both its ends are fully saturated, so whiteness
/// and blackness stay at zero and the two spaces have nothing to disagree
/// about -- they land on the same `252, 0, 251`. Two rows of the main table
/// would therefore pass for an implementation that resolved `Hwb` to `Hsl`,
/// which is the gap this closes rather than a defect in the pair.
///
/// Ending on a grey is what separates them: `190, 190, 190` carries
/// whiteness and blackness that `Hwb` interpolates and `Hsl` does not
/// represent. Tie-free and stable across the raster and GPU engines, which
/// a pair has to be for both of the reasons the main table's comment gives.
#[test]
fn hsl_and_hwb_are_different_spaces() {
    let from = RgbaLinear::from_srgb8(250, 2, 0, 1.0);
    let to = RgbaLinear::from_srgb8(190, 190, 190, 1.0);
    let hsl = midpoint(from, to, GradientColorSpace::Hsl.into());
    let hwb = midpoint(from, to, GradientColorSpace::Hwb.into());
    exact(hsl, [206, 110, 109], "Hsl toward a grey");
    exact(hwb, [220, 96, 95], "Hwb toward the same grey");
    assert_ne!(
        hsl, hwb,
        "the pair has to separate them or it adds nothing to the main table",
    );
}

/// The GPU path keeps the spaces apart, even where its bytes differ.
///
/// Every other test here calls `set_gpu(false)`, which is right for a reason
/// that outlives the engine difference: exact bytes have to come from one
/// named rasteriser or they describe only the machine that produced them.
/// But **pinning a dimension to make a test deterministic also makes that
/// dimension's failures invisible to it**, so the engine this file switches
/// off has no coverage in it at all. The binding lane found `display-p3` and
/// `hsl` differing by a level between the engines; nothing on this side would
/// have noticed.
///
/// What is asserted is the property rather than the bytes. Pinning GPU output
/// would pin one vendor's arithmetic -- Metal here, Vulkan elsewhere -- and
/// asserting that the engines *differ* would pin the defect itself, so a
/// later fix would fail the test. What must hold on any backend is that the
/// spaces remain distinguishable: if a GPU shader collapsed two of them, the
/// gradient would be wrong there in the way this whole file exists to catch.
///
/// Skips where there is no GPU, which is every CI runner, and says so through
/// `engine_kind` rather than assuming -- `set_gpu(true)` falls back silently
/// to the raster path, so a test that merely asked for a GPU would run on the
/// CPU and report a pass it had not earned.
#[test]
fn the_gpu_path_keeps_the_spaces_apart() {
    let mut probe = Canvas::new(WIDTH, 4.0);
    probe.set_gpu(true);
    if probe.engine_kind() != EngineKind::Gpu {
        return;
    }

    let on_gpu = |space: GradientColorSpace| {
        let mut canvas = Canvas::new(WIDTH, 4.0);
        canvas.set_gpu(true);
        {
            let ctx = canvas.context();
            let shader = Shader::linear_gradient(
                Point { x: 0.0, y: 0.0 },
                Point { x: WIDTH, y: 0.0 },
                &[
                    GradientStop {
                        position: 0.0,
                        color: from_stop(),
                    },
                    GradientStop {
                        position: 1.0,
                        color: to_stop(),
                    },
                ],
                space,
            )
            .expect("gradient");
            ctx.set_fill_shader(&shader);
            ctx.fill_rect(0.0, 0.0, WIDTH, 4.0);
        }
        let buffer = pixels(&mut canvas);
        let i = ((2 * WIDTH as u32 + 50) * 4) as usize;
        [buffer[i], buffer[i + 1], buffer[i + 2]]
    };

    // The pairs the CPU table separates, which the GPU must separate too.
    // `Hsl`/`Hwb` and the three XYZ names are left out: they agree on this
    // pair by construction, so requiring a difference would be asserting
    // something false rather than something unverified.
    for (a, b) in [
        (GradientColorSpace::Srgb, GradientColorSpace::SrgbLinear),
        (GradientColorSpace::Srgb, GradientColorSpace::DisplayP3),
        (GradientColorSpace::Srgb, GradientColorSpace::A98Rgb),
        (GradientColorSpace::Lab, GradientColorSpace::Oklab),
        (GradientColorSpace::Lch, GradientColorSpace::Oklch),
        (GradientColorSpace::ProphotoRgb, GradientColorSpace::Rec2020),
    ] {
        assert_ne!(
            on_gpu(a),
            on_gpu(b),
            "{a:?} and {b:?} must stay distinguishable on the GPU",
        );
    }
}

/// The midpoint of the same ramp, drawn on a canvas that is not sRGB.
///
/// Every other test in this file draws on `Canvas::new`, which is sRGB -- and
/// on an sRGB canvas `Destination` and `Srgb` name the same space, so nothing
/// above can tell them apart. This one exists because they part company only
/// here.
fn midpoint_on_display_p3(interp: GradientInterpolation) -> [u8; 4] {
    let mut canvas = Canvas::with_options(
        WIDTH,
        4.0,
        CanvasOptions {
            color_space: PixelColorSpace::DisplayP3,
            color_type: PixelDepth::Uint8,
            gpu: false,
            ..Default::default()
        },
    )
    .expect("a display-p3 canvas");
    {
        let ctx = canvas.context();
        let shader = Shader::linear_gradient(
            Point { x: 0.0, y: 0.0 },
            Point { x: WIDTH, y: 0.0 },
            &[
                GradientStop {
                    position: 0.0,
                    color: from_stop(),
                },
                GradientStop {
                    position: 1.0,
                    color: to_stop(),
                },
            ],
            interp,
        )
        .expect("gradient");
        ctx.set_fill_shader(&shader);
        ctx.fill_rect(0.0, 0.0, WIDTH, 4.0);
    }
    // The same pixel the sRGB helper reads, so the two are comparable.
    let buffer = pixels(&mut canvas);
    let i = ((2 * WIDTH as u32 + 50) * 4) as usize;
    [buffer[i], buffer[i + 1], buffer[i + 2], buffer[i + 3]]
}

/// `Destination` and `Srgb` are different spaces, and this is the only test
/// that can say so.
///
/// `Srgb` changed meaning in this release: it followed the surface, and now
/// names the literal space, which is what `Destination` is for. **The change
/// is silent on every canvas the rest of this suite builds** -- they are all
/// sRGB, where the two resolve to one space and any test of the split passes
/// whichever way the code goes. The comment at `tests/native_context2d.rs`
/// says as much beside the assertion that could not cover it.
///
/// Both expected values are derived from the CSS Color 4 formulae in a script
/// that reads nothing from this library: each stop converted sRGB -> linear ->
/// XYZ -> Display P3, and the two orders of operation compared.
///
/// - `Destination` converts both stops into P3 and interpolates there.
/// - `Srgb` interpolates in gamma-encoded sRGB and converts the result.
///
/// They differ because the transfer function is not linear, so converting and
/// then mixing is not mixing and then converting. The larger space keeps more
/// of the blue: 139 against 123.
///
/// **Why both an identity and two absolutes.** The two assertions do
/// different jobs and neither covers the other. The identity -- equal on an
/// sRGB canvas, unequal on Display P3 -- is what fails if the split is undone,
/// and no rounding boundary can break it, because it compares two measurements
/// against each other rather than against a constant. The absolutes are what
/// fail if both values move together to a *different* pair of spaces: a wrong
/// implementation that still differs on P3 and still agrees on sRGB satisfies
/// the identity completely. So the identity pins the relationship and the rows
/// pin which two spaces it holds between.
///
/// **Boundary clearance.** The worst channel here sits 0.123 from a `.5`
/// boundary -- `Srgb`'s blue at 122.623. No tie, so nothing here turns on a
/// rounding rule; the clearance is the room against an arithmetic difference,
/// and it is wider than the 0.0875 the sixteen-space table clears and an
/// order of magnitude wider than the difference being asserted. As with every
/// clearance in this file it is a measurement taken when the endpoints were
/// chosen, not something the test recomputes: if the stops change, re-measure.
#[test]
fn destination_and_srgb_part_company_off_an_srgb_canvas() {
    let destination = midpoint_on_display_p3(GradientInterpolation::new(
        GradientColorSpace::Destination,
    ));
    let literal_srgb = midpoint_on_display_p3(GradientInterpolation::new(
        GradientColorSpace::Srgb,
    ));

    exact(
        destination,
        [115, 25, 139],
        "Destination follows the canvas into P3",
    );
    exact(literal_srgb, [114, 20, 123], "Srgb names the literal space");

    // The point of the test, stated as its own assertion rather than left to
    // be inferred from two rows that happen to differ: if these ever agree,
    // the split has been undone and both rows above would still need to
    // change before anyone noticed.
    assert_ne!(
        [destination[0], destination[1], destination[2]],
        [literal_srgb[0], literal_srgb[1], literal_srgb[2]],
        "the two spaces must differ on a canvas that is not sRGB",
    );

    // And the control that explains why this gap existed: on an sRGB canvas
    // the same two values are the same pixel, so no test drawing there can
    // discriminate them however carefully it is written.
    let on_srgb_destination = midpoint(
        from_stop(),
        to_stop(),
        GradientInterpolation::new(GradientColorSpace::Destination),
    );
    let on_srgb_literal = midpoint(
        from_stop(),
        to_stop(),
        GradientInterpolation::new(GradientColorSpace::Srgb),
    );
    assert_eq!(
        on_srgb_destination, on_srgb_literal,
        "on an sRGB canvas the two name one space, which is why this was missed",
    );
}
