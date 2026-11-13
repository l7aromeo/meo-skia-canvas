//! `Srgba::from_css` against values computed outside this crate.
//!
//! A colour written in sRGB is expected exactly as written: a byte as itself
//! over 255, a `color(srgb)` channel and a decimal alpha as the `f64` its
//! text reads as. A colour written in another space is expected as CSS Color
//! 4's sample code computes it in `f64` from the rational matrices the
//! specification publishes, evaluated outside this crate, so agreement is
//! evidence rather than a table matching itself.

use meo_skia_canvas::prelude::*;

/// How far a channel converted from another space may sit from the sample
/// code's value: the two evaluate the same matrices in a different order, so
/// they agree to a few units in the last place of an `f64`, far inside this,
/// and a wrong matrix entry or a missing transfer function misses by more
/// than a thousandth.
const CONVERTED_TOLERANCE: f64 = 1e-12;

#[track_caller]
fn parse(css: &str) -> Srgba {
    Srgba::from_css(css)
        .unwrap_or_else(|error| panic!("{css} was refused: {error}"))
}

#[track_caller]
fn expect_exact(css: &str, want: [f64; 4]) {
    let got = parse(css);
    assert_eq!([got.r, got.g, got.b, got.a], want, "{css}");
}

#[track_caller]
fn expect_close(css: &str, want: [f64; 4]) {
    let got = parse(css);
    let channels = [got.r, got.g, got.b, got.a];
    assert!(
        channels
            .iter()
            .zip(want)
            .all(|(&g, w)| (g - w).abs() <= CONVERTED_TOLERANCE),
        "{css}: got {channels:?}, want {want:?}"
    );
}

/// `parse-color.tsv` from meo-canvas, whose `parseColor` this is meant to
/// serve: each colour as sRGB bytes and its alpha as written, the way
/// `getComputedStyle` reports it.
const PARSE_COLOR_ROWS: [(&str, [u8; 3], f64); 18] = [
    ("#808080", [128, 128, 128], 1.0),
    ("#f2aa4c", [242, 170, 76], 1.0),
    ("rebeccapurple", [102, 51, 153], 1.0),
    ("rgb(255, 0, 0)", [255, 0, 0], 1.0),
    ("rgba(0, 0, 255, 0.5)", [0, 0, 255], 0.5),
    ("rgba(0, 0, 0, 0.1)", [0, 0, 0], 0.1),
    ("rgba(0, 0, 0, 0.25)", [0, 0, 0], 0.25),
    ("rgba(0, 0, 0, 0.75)", [0, 0, 0], 0.75),
    ("rgba(0, 0, 0, 0.33)", [0, 0, 0], 0.33),
    ("rgba(0, 0, 0, 0.9)", [0, 0, 0], 0.9),
    ("rgba(0, 0, 0, 0)", [0, 0, 0], 0.0),
    ("#fff", [255, 255, 255], 1.0),
    ("#0000007f", [0, 0, 0], 0.4980392156862745),
    ("#0008", [0, 0, 0], 0.5333333333333333),
    ("#000000cc", [0, 0, 0], 0.8),
    ("rgba(0, 0, 0, 50%)", [0, 0, 0], 0.5),
    ("rgba(0, 0, 0, 33%)", [0, 0, 0], 0.33),
    ("rgba(0, 0, 0, 12.5%)", [0, 0, 0], 0.125),
];

#[test]
fn bytes_and_alphas_read_back_as_written() {
    for (css, bytes, alpha) in PARSE_COLOR_ROWS {
        let got = parse(css);
        let back = [got.r, got.g, got.b].map(|c| c * 255.0);
        assert_eq!(back, bytes.map(f64::from), "{css}: channels");
        assert_eq!(got.a, alpha, "{css}: alpha");
    }
}

#[test]
fn a_byte_is_itself_over_255() {
    // The literal is the nearest `f64` to 127/255, what JavaScript computes;
    // `f32` holds 0.49803921580314636, the value this exists to rule out.
    assert_eq!(127.0 / 255.0, 0.4980392156862745);
    assert_eq!(parse("#0000007f").a, 127.0 / 255.0);
    expect_exact(
        "#808080",
        [128.0 / 255.0, 128.0 / 255.0, 128.0 / 255.0, 1.0],
    );
    expect_exact("#f2aa4c", [242.0 / 255.0, 170.0 / 255.0, 76.0 / 255.0, 1.0]);
    expect_exact(
        "rebeccapurple",
        [102.0 / 255.0, 51.0 / 255.0, 153.0 / 255.0, 1.0],
    );
}

#[test]
fn colour_srgb_channels_come_back_unrounded() {
    // -0.1 has no `f32` that reads back as -25.5 when scaled to bytes.
    let got = parse("color(srgb 1.25 -0.1 0.5 / 0.1)");
    assert_eq!([got.r, got.g, got.b, got.a], [1.25, -0.1, 0.5, 0.1]);
    assert_eq!(
        [got.r, got.g, got.b].map(|c| c * 255.0),
        [318.75, -25.5, 127.5]
    );
}

#[test]
fn decimal_alphas_come_back_as_written() {
    for (css, alpha) in [
        ("rgba(0, 0, 0, 0.1)", 0.1),
        ("rgba(0, 0, 0, 0.33)", 0.33),
        ("rgba(0, 0, 0, 0.005)", 0.005),
        ("rgb(0 0 0 / 12.5%)", 0.125),
        ("color(srgb 0 0 0 / 0.33)", 0.33),
    ] {
        assert_eq!(parse(css).a, alpha, "{css}");
    }
}

#[test]
fn a_transparent_colour_keeps_its_hue() {
    for css in [
        "rgba(255, 0, 0, 0)",
        "rgb(255 0 0 / 0)",
        "color(srgb 1 0 0 / 0)",
        "color(from red srgb r g b / 0)",
        "color(srgb 1 0 0 / -1)",
    ] {
        expect_exact(css, [1.0, 0.0, 0.0, 0.0]);
    }
    expect_exact("hsl(120 100% 50% / 0)", [0.0, 1.0, 0.0, 0.0]);
    // Four digits are `#rgba`: red and green full, blue and alpha zero.
    expect_exact("#ff00", [1.0, 1.0, 0.0, 0.0]);
    // The sample code's `gam_sRGB(1)` is `1.055 - 0.055`, one unit in the
    // last place below one in `f64`.
    expect_close("color(srgb-linear 1 0 0/0)", [1.0, 0.0, 0.0, 0.0]);
    expect_close(
        "COLOR(xyz 1 0 0 / 0)",
        [
            1.6670022357335075,
            -0.9863567239475025,
            0.261566407098026,
            0.0,
        ],
    );
}

#[test]
fn a_colour_outside_srgb_is_not_clamped() {
    let p3_red = [
        1.0930663624351615,
        -0.22674197356975417,
        -0.15013458093711954,
        1.0,
    ];
    expect_close("color(display-p3 1 0 0)", p3_red);
    let bytes = [p3_red[0], p3_red[1], p3_red[2]].map(|c| c * 255.0);
    assert!(
        (bytes[0] - 278.7).abs() < 0.05
            && (bytes[1] + 57.8).abs() < 0.05
            && (bytes[2] + 38.3).abs() < 0.05,
        "{bytes:?}"
    );
}

/// `color(display-p3-linear ...)` inside sRGB's gamut: the sRGB CSS Color
/// 4's sample code computes for it -- `lin_P3_to_XYZ`, `XYZ_to_lin_sRGB`
/// and `gam_sRGB`, with no transfer function on the way in, since the space
/// is linear light -- evaluated in `f64` outside this crate, and the pixel
/// Chrome 158 paints for it on an 8-bit sRGB canvas.
///
/// Inside the gamut, so that nothing clips: read with Display P3's curve,
/// these paint 0, 130, 71 and 82, 36, 159 and 162, 99, 38, and read as
/// `srgb-linear` 124, 188, 149 and 149, 108, 203 and 203, 170, 124. A red
/// of one, the obvious case, clips to 255, 0, 0 whichever it is read as.
const DISPLAY_P3_LINEAR: [(&str, [f64; 3], [u8; 3]); 3] = [
    (
        "color(display-p3-linear 0.2 0.5 0.3)",
        [0.3994996827410734, 0.7436066251815049, 0.5714521599124812],
        [102, 190, 146],
    ),
    (
        "color(display-p3-linear 0.3 0.15 0.6)",
        [0.6128410730408542, 0.41509109160468405, 0.821707974595879],
        [156, 106, 210],
    ),
    (
        "color(display-p3-linear 0.6 0.4 0.2)",
        [0.823818081719513, 0.6588357821126819, 0.45704928262859973],
        [210, 168, 117],
    ),
];

#[test]
fn display_p3_linear_is_p3_light_with_no_curve() {
    for (css, srgb, chrome) in DISPLAY_P3_LINEAR {
        expect_close(css, [srgb[0], srgb[1], srgb[2], 1.0]);
        let got = parse(css);
        let bytes = [got.r, got.g, got.b].map(|c| (c * 255.0).round() as u8);
        assert_eq!(bytes, chrome, "{css}: as Chrome 158 paints it");
    }
    // A grey stays grey: P3 and sRGB share D65's white, so linear 0.5 is
    // sRGB's encoding of 0.5 in every channel, 187.52 as a byte. Chrome 158
    // paints 187, 188, 188; docs/browser-differences.md.
    expect_close(
        "color(display-p3-linear 0.5 0.5 0.5)",
        [
            0.7353569830524493,
            0.7353569830524495,
            0.7353569830524495,
            1.0,
        ],
    );
    // The red of the report: linear one is encoded one, so this is
    // `display-p3`'s red, outside sRGB and kept there.
    expect_close(
        "color(display-p3-linear 1 0 0)",
        [
            1.0930663624351615,
            -0.22674197356975417,
            -0.15013458093711954,
            1.0,
        ],
    );
    expect_close(
        "COLOR(Display-P3-Linear 0.2 0.5 0.3 / 50%)",
        [
            0.3994996827410734,
            0.7436066251815049,
            0.5714521599124812,
            0.5,
        ],
    );
}

#[test]
fn display_p3_linear_as_linear_light_is_the_matrices_alone() {
    // `XYZ_to_lin_sRGB` after `lin_P3_to_XYZ`, nothing else: the same
    // sample code, stopped before `gam_sRGB`.
    let linear = [0.13251794711583198, 0.5126170864129066, 0.286236546348907];
    let got = RgbaLinear::from_css("color(display-p3-linear 0.2 0.5 0.3)")
        .expect("a colour");
    let channels = [got.r, got.g, got.b, got.a].map(f64::from);
    assert!(
        channels
            .iter()
            .zip([linear[0], linear[1], linear[2], 1.0])
            .all(|(g, w)| (g - w).abs() <= f64::from(f32::EPSILON)),
        "{channels:?}"
    );
}

/// Colours written in sRGB inside its gamut: their channels are the written
/// numbers, so they lie in `0.0..=1.0` exactly.
const WRITTEN_IN_SRGB: [&str; 13] = [
    "#fff",
    "#000",
    "white",
    "rgb(255 255 255)",
    "rgb(300 -20 0)",
    "hsl(0 100% 50%)",
    "hsl(120 100% 50%)",
    "hsl(240 100% 50%)",
    "hsl(0 0% 100%)",
    "hwb(0 100% 0%)",
    "hwb(0 0% 100%)",
    "color(srgb 1 1 1)",
    "color(srgb 0 0 0)",
];

/// White written in another space, and the sRGB CSS Color 4's sample code
/// computes for it: `conversions.js` with its rational matrices and the
/// current `lin_2020`, a 2.4 power, evaluated in `f64` outside this crate.
/// The published Oklab matrices carry white to a red 2 units in the last
/// place above one: `OKLab_to_XYZ` maps (1, 1, 1) to (0.9504559270516719,
/// 0.9999999999999999, 1.0890577507598782) against D65's (0.9504559270516716,
/// 1, 1.0890577507598784).
const CONVERTED_WHITES: [(&str, [f64; 3]); 8] = [
    ("color(srgb-linear 1 1 1)", [0.9999999999999999; 3]),
    (
        "lab(100 0 0)",
        [0.9999999999999997, 0.9999999999999999, 0.9999999999999997],
    ),
    (
        "lch(100 0 0)",
        [0.9999999999999997, 0.9999999999999999, 0.9999999999999997],
    ),
    (
        "oklab(1 0 0)",
        [1.0000000000000004, 0.9999999999999997, 0.9999999999999997],
    ),
    (
        "oklch(1 0 0)",
        [1.0000000000000004, 0.9999999999999997, 0.9999999999999997],
    ),
    (
        "color(display-p3 1 1 1)",
        [0.9999999999999997, 0.9999999999999999, 0.9999999999999999],
    ),
    (
        "color(display-p3-linear 1 1 1)",
        [0.9999999999999997, 0.9999999999999999, 0.9999999999999999],
    ),
    (
        "color(rec2020 1 1 1)",
        [0.9999999999999997, 0.9999999999999999, 0.9999999999999999],
    ),
];

/// How far outside `0.0..=1.0` a colour converted from another space may
/// sit at the gamut's edge: a few units in the last place of an `f64` near
/// one, where a unit is 2.2e-16, and well short of the `f32` round trip's
/// 8e-8 this type exists to avoid.
const EDGE_TOLERANCE: f64 = 1e-15;

#[test]
fn a_colour_written_in_srgb_stays_inside_exactly() {
    for css in WRITTEN_IN_SRGB {
        let got = parse(css);
        for channel in [got.r, got.g, got.b] {
            assert!((0.0..=1.0).contains(&channel), "{css}: {got:?}");
        }
    }
}

#[test]
fn a_converted_white_is_the_specifications() {
    let inside = -EDGE_TOLERANCE..=1.0 + EDGE_TOLERANCE;
    for (css, want) in CONVERTED_WHITES {
        let got = parse(css);
        let channels = [got.r, got.g, got.b];
        assert_eq!(channels, want, "{css}");
        assert!(channels.iter().all(|c| inside.contains(c)), "{css}");
    }
    for css in ["lab(0 0 0)", "oklab(0 0 0)", "oklch(0 0 0)"] {
        let got = parse(css);
        assert!(
            [got.r, got.g, got.b].iter().all(|c| inside.contains(c)),
            "{css}: {got:?}"
        );
    }
}

#[test]
fn refusals_are_the_fill_styles() {
    let mut canvas = Canvas::new(4.0, 4.0);
    let ctx = canvas.context();
    let strings = [
        "",
        "   ",
        "not a colour",
        "hwba(0 0% 0%)",
        "hsv(0 100% 100%)",
        "hsva(0 100% 100% / 1)",
        "ff0000",
        "#ggg",
        "#12345",
        "rgb (255 0 0)",
        "rgb(255, 0%, 0)",
        "lab(50, 40, 30)",
        "red blue",
        "/* only a comment */",
        // A canvas's `colorSpace` alias, which `color()` does not name.
        "color(p3-linear 0.2 0.5 0.3)",
        // Accepted by both: the refusal table needs rows that must not refuse.
        "red",
        "rgb(255 0 0",
        "/* x */ red",
        "currentcolor",
        "transparent",
    ];
    let mut refused = 0;
    for css in strings {
        let fill = ctx.set_fill_style_css(css);
        let parsed = Srgba::from_css(css);
        match (fill, parsed) {
            (Ok(()), Ok(_)) => {}
            (Err(fill), Err(parsed)) => {
                assert!(
                    matches!(parsed, Error::InvalidColor { .. }),
                    "{css}: {parsed:?}"
                );
                assert_eq!(parsed.to_string(), fill.to_string(), "{css}");
                refused += 1;
            }
            (fill, parsed) => {
                panic!("{css}: the fill style gave {fill:?}, this {parsed:?}")
            }
        }
    }
    assert_eq!(refused, 15, "rows the fill style refuses");
}

#[test]
fn as_linear_light_it_is_the_fill_styles_colour() {
    // Exactly, for colours written in sRGB: the channels round to `f32` at
    // the same point on both paths.
    for css in [
        "#808080",
        "rgba(255, 0, 0, 0.5)",
        "rebeccapurple",
        "color(srgb 1.25 -0.1 0.5 / 0.1)",
        "hsl(120 100% 25% / 0.5)",
    ] {
        assert_eq!(
            Some(RgbaLinear::from(parse(css))),
            RgbaLinear::from_css(css),
            "{css}"
        );
    }
    // Premultiplied, a transparent colour is four zeros, as it is from
    // `RgbaLinear::from_css`; the hue is what `Srgba` is for.
    assert_eq!(
        RgbaLinear::from(parse("rgba(255, 0, 0, 0)")),
        RgbaLinear::new_premultiplied(0.0, 0.0, 0.0, 0.0)
    );
}
