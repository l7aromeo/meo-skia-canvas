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
