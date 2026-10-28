//! `RgbaLinear::from_css` against values computed from CSS Color 4.
//!
//! Every expected value here was computed outside this crate: the matrices
//! from the rational forms CSS Color 4's sample code publishes, checked to
//! nine digits against the same matrices derived from each space's primaries
//! and the D65 white point; the transfer function from IEC 61966-2-1; Oklab
//! from Björn Ottosson's published matrices. None of them was read off the
//! parser, so agreement is evidence rather than a table matching itself.
//!
//! Colours named in another space are cases at `0` and `1` on purpose: the
//! transfer function is the identity there, so those cases test the primaries
//! and nothing else. `display_p3_off_the_corners_decodes_before_converting`
//! is the one that exercises a transfer function on the way.

use meo_skia_canvas::prelude::*;

/// How far a component may sit from its computed value.
///
/// Above the error `f32` arithmetic accumulates through a decode, one matrix
/// and a premultiply, which is a few units in the sixth place; below a
/// matrix written to four or five digits, which misses by `2e-5` to `5e-5`
/// and is the defect this exists to see.
const TOLERANCE: f32 = 1e-5;

fn close(got: RgbaLinear, want: [f64; 4]) -> bool {
    [got.r, got.g, got.b, got.a]
        .iter()
        .zip(want)
        .all(|(&g, w)| (f64::from(g) - w).abs() <= f64::from(TOLERANCE))
}

#[track_caller]
fn expect(css: &str, want: [f64; 4]) {
    let got = RgbaLinear::from_css(css).expect("a CSS colour");
    assert!(close(got, want), "{css}: got {got:?}, want {want:?}");
}

#[test]
fn an_srgb_colour_is_the_constructor_a_port_already_uses() {
    // Exactly, not within tolerance: `from_srgb8` is what the crate tells a
    // port from JavaScript to write, and the two spellings of one colour
    // must not differ in the last bit.
    let cases = [
        ("#808080", RgbaLinear::from_srgb8(0x80, 0x80, 0x80, 1.0)),
        ("#f00", RgbaLinear::from_hex("#f00").expect("hex")),
        ("rebeccapurple", RgbaLinear::from_srgb8(102, 51, 153, 1.0)),
        (
            "rgba(255, 0, 0, 0.5)",
            RgbaLinear::from_srgb8(255, 0, 0, 0.5),
        ),
        ("rgb(255 0 0 / 50%)", RgbaLinear::from_srgb8(255, 0, 0, 0.5)),
        (
            "transparent",
            RgbaLinear::new_premultiplied(0.0, 0.0, 0.0, 0.0),
        ),
    ];
    for (css, want) in cases {
        assert_eq!(RgbaLinear::from_css(css), Some(want), "{css}");
    }
    assert_ne!(
        RgbaLinear::from_css("#808080"),
        Some(RgbaLinear::opaque(0.5, 0.5, 0.5)),
        "a CSS grey is not linear-light 0.5",
    );
}

#[test]
fn hsl_resolves_through_srgb() {
    expect("hsl(120 100% 50%)", [0.0, 1.0, 0.0, 1.0]);
    expect(
        "hsl(270deg 50% 40%)",
        [0.132_868, 0.033_105, 0.318_547, 1.0],
    );
}

#[test]
fn a_display_p3_primary_lands_outside_srgb_rather_than_on_its_edge() {
    // The first column of CSS Color 4's linear P3 -> linear sRGB matrix.
    // Clamping it to `(1, 0, 0)` would hand a drop shadow sRGB red, which is
    // a different and duller colour.
    expect(
        "color(display-p3 1 0 0)",
        [1.224_940_18, -0.042_056_95, -0.019_637_55, 1.0],
    );
}

#[test]
fn display_p3_off_the_corners_decodes_before_converting() {
    // P3 carries the sRGB transfer function, so this is decode, matrix, and
    // the result left linear.
    expect(
        "color(display-p3 0.5 0.25 0.75)",
        [0.250_743_52, 0.044_013_86, 0.565_667_69, 1.0],
    );
}

#[test]
fn a_rec2020_primary_converts_through_the_full_matrix() {
    // The second column of CSS Color 4's linear Rec. 2020 -> linear sRGB
    // matrix, the one whose entries differ most from a short decimal.
    expect(
        "color(rec2020 0 1 0)",
        [-0.587_641_14, 1.132_899_90, -0.100_578_90, 1.0],
    );
}

#[test]
fn a98_rgb_decodes_with_its_own_gamma_before_converting() {
    // `lin_a98rgb` then `lin_a98rgb_to_XYZ` and `XYZ_to_lin_sRGB`, CSS Color
    // 4's sample code, with the power `563/256` mirrored through zero for the
    // negative red. The second lands outside sRGB on both sides.
    expect(
        "color(a98-rgb 0.5 0.2 0.1)",
        [0.292_936_358, 0.029_027_662, 0.005_346_157, 1.0],
    );
    expect(
        "color(a98-rgb -0.2 0.5 1.1)",
        [-0.127_335_164, 0.217_755_528, 1.276_786_24, 1.0],
    );
}

#[test]
fn prophoto_rgb_adapts_from_d50_and_keeps_its_linear_toe() {
    // `lin_ProPhoto`, `lin_ProPhoto_to_XYZ`, `D50_to_D65` and
    // `XYZ_to_lin_sRGB`. `0.01` sits below `16/512`, on the linear segment
    // that divides by 16; the others are on the 1.8 power, mirrored for the
    // negative red.
    expect(
        "color(prophoto-rgb 0.5 0.2 0.1)",
        [0.539_203_275, 0.002_219_705, 0.007_497_151, 1.0],
    );
    expect(
        "color(prophoto-rgb 0.01 0.5 0.9)",
        [-0.461_441_72, 0.351_169_212, 0.917_099_946, 1.0],
    );
    expect(
        "color(prophoto-rgb -0.2 0.5 1.1)",
        [-0.685_387_767, 0.362_891_163, 1.335_723_158, 1.0],
    );
}

#[test]
fn srgb_linear_passes_through_unchanged() {
    expect("color(srgb-linear 0.5 0.25 0)", [0.5, 0.25, 0.0, 1.0]);
}

#[test]
fn a_lab_grey_is_its_luminance() {
    // A neutral in Lab is the white point, which the Bradford step carries
    // from D50 to D65 unchanged in hue, so every channel is
    // `((L + 16) / 116)^3`.
    let y = ((50.0f64 + 16.0) / 116.0).powi(3);
    expect("lab(50% 0 0)", [y, y, y, 1.0]);
}

#[test]
fn an_oklch_colour_outside_srgb_keeps_its_sign() {
    // Ottosson's matrices give green below zero and red above one. A tighter
    // tolerance than this would be testing the difference between his
    // published matrices and CSS Color 4's, which is not what this asks.
    let got = RgbaLinear::from_css("oklch(0.7 0.35 30)").expect("oklch");
    let want = [1.657_377, -0.091_825, -0.049_600];
    for (g, w) in [got.r, got.g, got.b].into_iter().zip(want) {
        assert!(
            (f64::from(g) - w).abs() < 1e-4,
            "got {got:?}, want {want:?}"
        );
    }
}

#[test]
fn alpha_premultiplies_a_wide_colour_too() {
    expect(
        "color(display-p3 1 0 0 / 0.5)",
        [0.612_470_09, -0.021_028_48, -0.009_818_78, 0.5],
    );
}

#[test]
fn what_is_not_a_colour_is_none() {
    for css in [
        "",
        "not-a-colour",
        "#ggg",
        "rgb(1, 2)",
        "color(prophoto 1 0 0)",
    ] {
        assert_eq!(RgbaLinear::from_css(css), None, "{css:?}");
    }
}

#[test]
fn a_fill_from_css_and_a_fill_from_the_colour_agree_on_an_srgb_canvas() {
    // The two routes into a fill share one parser. On an sRGB canvas, where
    // `RgbaLinear` is read as linear sRGB, they paint the same pixel.
    let pixel = |paint: &dyn Fn(&mut Context2D)| {
        let mut canvas = Canvas::new(1.0, 1.0);
        canvas.set_gpu(false);
        let ctx = canvas.context();
        paint(ctx);
        ctx.fill_rect(0.0, 0.0, 1.0, 1.0);
        ctx.get_image_data(0.0, 0.0, 1.0, 1.0)
            .expect("readback")
            .pixels()
            .to_vec()
    };
    for css in ["#742", "hsl(200 60% 40%)", "lab(60% 20 -30)"] {
        let colour = RgbaLinear::from_css(css).expect("a CSS colour");
        let by_css = pixel(&|ctx| ctx.set_fill_style_css(css).expect("css"));
        let by_value = pixel(&|ctx| ctx.set_fill_style(colour));
        assert_eq!(by_css, by_value, "{css}");
    }
}

#[test]
fn a_filter_shadow_named_in_css_takes_the_colour_a_fill_takes() {
    // A drop-shadow's colour goes through the same parser as a fill's, and
    // lands on the pixel Chrome draws. Off the 0 and 1 corners, so a transfer
    // function applied twice -- or not at all -- moves every channel.
    let shadow = |css: &str| {
        let mut canvas = Canvas::new(20.0, 20.0);
        canvas.set_gpu(false);
        let ctx = canvas.context();
        ctx.set_filter_css(&format!("drop-shadow(10px 10px 0px {css})"))
            .expect("a filter");
        ctx.fill_rect(0.0, 0.0, 5.0, 5.0);
        ctx.get_image_data(12.0, 12.0, 1.0, 1.0)
            .expect("readback")
            .pixels()
            .to_vec()
    };
    let fill = |css: &str| {
        let mut canvas = Canvas::new(1.0, 1.0);
        canvas.set_gpu(false);
        let ctx = canvas.context();
        ctx.set_fill_style_css(css).expect("a fill");
        ctx.fill_rect(0.0, 0.0, 1.0, 1.0);
        ctx.get_image_data(0.0, 0.0, 1.0, 1.0)
            .expect("readback")
            .pixels()
            .to_vec()
    };
    // Each measured in Chrome 152 as the same drop-shadow, read back from
    // the shadow's pixel. `rgb(10.5 ...)` is the half step Chrome rounds up
    // at parse.
    for (css, chrome) in [
        ("rebeccapurple", [102, 51, 153, 255]),
        ("#742", [119, 68, 34, 255]),
        ("hsl(120 50% 50%)", [64, 191, 64, 255]),
        ("lab(50 40 30)", [187, 88, 70, 255]),
        ("oklch(0.6 0.2 30)", [222, 62, 45, 255]),
        ("rgb(10.5 20.4 30.6)", [11, 20, 31, 255]),
    ] {
        assert_eq!(shadow(css), chrome, "{css} against Chrome");
        assert_eq!(shadow(css), fill(css), "{css} against the fill");
    }
}

#[test]
fn a_fill_set_from_css_reads_back_as_the_colour_it_paints() {
    // `fill_style` and `stroke_style` report an `RgbaLinear`, which is linear
    // light in the canvas's working space, so handing the report back to
    // `set_fill_style` has to paint what the CSS did. On a Display P3 canvas
    // the working primaries are P3's, and a colour named in sRGB or in
    // Rec. 2020 has to arrive in them.
    let paint = |space: PixelColorSpace, set: &dyn Fn(&mut Context2D)| {
        let mut canvas = Canvas::with_options(
            1.0,
            1.0,
            CanvasOptions {
                color_space: space,
                ..CanvasOptions::default()
            },
        )
        .expect("a canvas");
        canvas.set_gpu(false);
        let ctx = canvas.context();
        set(ctx);
        ctx.fill_rect(0.0, 0.0, 1.0, 1.0);
        ctx.get_image_data(0.0, 0.0, 1.0, 1.0)
            .expect("readback")
            .pixels()
            .to_vec()
    };
    let reported = |space: PixelColorSpace, css: &str| {
        let mut canvas = Canvas::with_options(
            1.0,
            1.0,
            CanvasOptions {
                color_space: space,
                ..CanvasOptions::default()
            },
        )
        .expect("a canvas");
        let ctx = canvas.context();
        ctx.set_fill_style_css(css).expect("a fill");
        ctx.set_stroke_style_css(css).expect("a stroke");
        assert_eq!(ctx.fill_style(), ctx.stroke_style(), "{css}");
        match ctx.fill_style() {
            PaintSource::Color(colour) => colour,
            other => panic!("{css}: {other:?}"),
        }
    };
    let mut wrong = Vec::new();
    for space in [PixelColorSpace::Srgb, PixelColorSpace::DisplayP3] {
        for css in [
            "rebeccapurple",
            "#742",
            "hsl(200 60% 40%)",
            "rgb(255 0 0 / 50%)",
            "color(display-p3 0.2 0.6 0.4)",
            "color(rec2020 0.3 0.5 0.2)",
            "color(srgb-linear 0.2 0.3 0.4)",
        ] {
            let colour = reported(space, css);
            let by_css = paint(space, &|ctx| {
                ctx.set_fill_style_css(css).expect("a fill");
            });
            let by_report = paint(space, &|ctx| ctx.set_fill_style(colour));
            if by_css != by_report {
                wrong.push(format!(
                    "{space:?} {css}: CSS paints {by_css:?}, the report \
                     {by_report:?}"
                ));
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    // And on an sRGB canvas the report is `RgbaLinear::from_css`, the value
    // the documentation of both types promises.
    assert_eq!(
        reported(PixelColorSpace::Srgb, "rebeccapurple"),
        RgbaLinear::from_css("rebeccapurple").expect("a colour"),
    );
}

#[test]
fn a_fill_set_from_a_colour_reads_back_as_that_colour() {
    // The typed setter's value is already linear in the working space, and
    // comes back unchanged on any canvas -- a P3 one included, where it is
    // not sRGB.
    for space in [PixelColorSpace::Srgb, PixelColorSpace::DisplayP3] {
        let mut canvas = Canvas::with_options(
            1.0,
            1.0,
            CanvasOptions {
                color_space: space,
                ..CanvasOptions::default()
            },
        )
        .expect("a canvas");
        let ctx = canvas.context();
        // Alphas on a power of two, where the premultiply round trip is
        // exact.
        for colour in [
            RgbaLinear::opaque(0.25, 0.5, 0.125),
            RgbaLinear {
                r: 0.125,
                g: 0.0625,
                b: 0.25,
                a: 0.5,
            },
            RgbaLinear::opaque(1.25, -0.0625, 0.5),
        ] {
            ctx.set_fill_style(colour);
            assert_eq!(
                ctx.fill_style(),
                PaintSource::Color(colour),
                "{space:?}"
            );
        }
    }
}

#[test]
fn lab_and_lch_are_read_the_same_in_either_case() {
    // CSS function names are ASCII case-insensitive, so the upper-case
    // spellings are the same colours. Both rows sit where a conversion
    // that is close but not exact lands on a different value, so equality
    // here means both cases took the same path.
    for (lower, upper) in [
        ("lab(60% 20 -30)", "LAB(60% 20 -30)"),
        ("lch(50% 70 0)", "LCH(50% 70 0)"),
        ("lab(79.18% -50% 30)", "Lab(79.18% -50% 30)"),
    ] {
        let a = RgbaLinear::from_css(lower).expect("lower case");
        let b = RgbaLinear::from_css(upper).expect("upper case");
        assert_eq!(a, b, "{upper} against {lower}");
    }
}

/// CSS Color 4's conversion from Lab to sRGB, in `f64`, from its sample code
/// rather than from this crate: Lab's D50 white, the specification's Bradford
/// matrix to D65, and the rational XYZ-to-linear-sRGB matrix. Returns linear
/// sRGB, unclamped, premultiplied by `alpha`, as `RgbaLinear::from_css` does.
fn css_color_4_lab(l: f64, a: f64, b: f64, alpha: f64) -> [f64; 4] {
    const KAPPA: f64 = 24389.0 / 27.0;
    const EPSILON: f64 = 216.0 / 24389.0;
    let white = [0.3457 / 0.3585, 1.0, (1.0 - 0.3457 - 0.3585) / 0.3585];
    let bradford = [
        [0.955473421488075, -0.02309845494876471, 0.06325924320057072],
        [
            -0.0283697093338637,
            1.0099953980813041,
            0.021041441191917323,
        ],
        [
            0.012314014864481998,
            -0.020507649298898964,
            1.330365926242124,
        ],
    ];
    let to_srgb = [
        [12831.0 / 3959.0, -329.0 / 214.0, -1974.0 / 3959.0],
        [
            -851781.0 / 878810.0,
            1648619.0 / 878810.0,
            36519.0 / 878810.0,
        ],
        [705.0 / 12673.0, -2585.0 / 12673.0, 705.0 / 667.0],
    ];
    let apply = |m: [[f64; 3]; 3], v: [f64; 3]| {
        [0, 1, 2].map(|i| m[i][0] * v[0] + m[i][1] * v[1] + m[i][2] * v[2])
    };
    let f1 = (l + 16.0) / 116.0;
    let (f0, f2) = (a / 500.0 + f1, f1 - b / 200.0);
    let cube = |f: f64| match f * f * f > EPSILON {
        true => f * f * f,
        false => (116.0 * f - 16.0) / KAPPA,
    };
    let y = match l > KAPPA * EPSILON {
        true => f1 * f1 * f1,
        false => l / KAPPA,
    };
    let xyz = [cube(f0) * white[0], y * white[1], cube(f2) * white[2]];
    let [r, g, bl] = apply(to_srgb, apply(bradford, xyz));
    [r * alpha, g * alpha, bl * alpha, alpha]
}

/// The largest error `lab()` and `lch()` may carry against CSS Color 4.
///
/// Above what holding the channels and the result at `f32`, and decoding the
/// result to linear light in `f32`, leave on this grid -- at most 2.9e-6,
/// at `lab(100 17.56 -160)`, where the conversion itself runs in `f64` --
/// and far below the 1.07e-3 the dependency underneath reaches on its own,
/// which is what this exists to refuse.
const LAB_TOLERANCE: f64 = 5e-6;

#[test]
fn lab_and_lch_follow_css_color_4_across_their_grammar() {
    // Every lightness, both axes and the chroma and hue forms the grammar
    // takes -- numbers, percentages, `none`, every angle unit, and values
    // outside sRGB -- against the reference above. A claim that the
    // dependency underneath reads Lab well enough to drop this crate's own
    // reading is a claim about every one of these, and this is where it is
    // tested: the dependency alone misses by 1.07e-3 in linear light, which
    // is 2.2e-3 once encoded and a whole level in 8-bit on 121 inputs of a
    // grid this one samples.
    let number = |t: &str, full: f64| match t {
        "none" => 0.0,
        t => match t.strip_suffix('%') {
            Some(p) => p.parse::<f64>().expect("number") / 100.0 * full,
            None => t.parse::<f64>().expect("number"),
        },
    };
    let angle = |t: &str| -> f64 {
        let turn = std::f64::consts::TAU;
        [
            ("deg", 360.0),
            ("grad", 400.0),
            ("rad", turn),
            ("turn", 1.0),
        ]
        .iter()
        .find_map(|(unit, per)| {
            t.strip_suffix(unit)
                .map(|v| v.parse::<f64>().expect("angle") / per * 360.0)
        })
        .unwrap_or_else(|| number(t, 1.0))
    };
    let lightness = ["0", "12.5", "25%", "50", "79.18%", "100", "none"];
    let axis = [
        "-160", "-62.27", "-30", "0", "17.56", "60", "125", "-50%", "100%",
        "none",
    ];
    let chroma = ["0", "40", "70", "150", "230", "50%", "none"];
    let hue = [
        "0", "30", "165", "400", "-30", "30deg", "100grad", "1rad", "0.5turn",
        "none",
    ];
    let alpha = [("", 1.0), (" / 0.5", 0.5), (" / 50%", 0.5)];
    let mut cases = Vec::new();
    for l in lightness {
        for (al, alpha) in alpha {
            for a in axis {
                for b in axis {
                    let want = css_color_4_lab(
                        number(l, 100.0),
                        number(a, 125.0),
                        number(b, 125.0),
                        alpha,
                    );
                    cases.push((format!("lab({l} {a} {b}{al})"), want));
                }
            }
            for c in chroma {
                for h in hue {
                    let (c_, h_) = (number(c, 150.0), angle(h).to_radians());
                    let want = css_color_4_lab(
                        number(l, 100.0),
                        c_ * h_.cos(),
                        c_ * h_.sin(),
                        alpha,
                    );
                    cases.push((format!("lch({l} {c} {h}{al})"), want));
                }
            }
        }
    }
    let mut worst = (0.0, String::new());
    for (css, want) in &cases {
        let got =
            RgbaLinear::from_css(css).unwrap_or_else(|| panic!("{css} parses"));
        let error = [got.r, got.g, got.b, got.a]
            .iter()
            .zip(want)
            .map(|(g, w)| (f64::from(*g) - w).abs())
            .fold(0.0, f64::max);
        if error > worst.0 {
            worst = (error, css.clone());
        }
    }
    assert!(
        worst.0 <= LAB_TOLERANCE,
        "{} cases checked; {} is {:e} from CSS Color 4",
        cases.len(),
        worst.1,
        worst.0,
    );
    assert!(cases.len() > 2000, "the grid is the point: {}", cases.len());
}
