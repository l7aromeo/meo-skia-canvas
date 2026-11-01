//! A colour named for a text decoration or a CSS `drop-shadow()` is
//! painted at the precision it names, as a fill's is, on every canvas.
//!
//! Both were rounded to an eight-bit sRGB colour when they were set: the
//! decoration's held as an `SkColor`, the shadow's quantised to bytes before
//! it became a linear colour. So on an F16 or F32 canvas
//! `underline rgb(0 0 0 / 0.3)` drew its line at 77/255 and
//! `rgb(0 0 0 / 0.001)` not at all, beside text and fills that kept the
//! colour they were given; and on an eight-bit canvas a colour between
//! bytes, or outside sRGB, drew a level or more from a fill of it.
//!
//! A drop-shadow on a float canvas is not held to its alpha here: Skia's
//! raster backend resolves it on an eight-bit layer, since
//! `SkBitmapDevice::createDevice` makes any layer under an image filter N32.
//! `css::tests::a_drop_shadow_s_colour_is_the_colour_css_names` holds the
//! colour it is handed instead.

use std::ops::Range;

use meo_skia_canvas::prelude::*;

const OSWALD: &str = "tests/assets/fonts/Oswald/Oswald-VariableFont_wght.ttf";
const FAMILY: &str = "FloatDecorationOswald";

/// The side of the square canvas every case draws on.
const SIDE: f32 = 32.0;

/// The rows a 6px underline fills under text set at [`BASELINE`] in 28px
/// Oswald, clear of the glyph above them.
const LINE_ROWS: Range<usize> = 14..21;

/// The baseline every case sets its text at: high enough on the canvas
/// for the line to fit under it.
const BASELINE: f32 = 12.0;

/// How far the line may be from the alpha it should have, as a share of
/// it: F16 holds a value to about a two-thousandth.
const TOLERANCE: f32 = 0.002;

fn canvas(depth: PixelDepth) -> Canvas {
    Canvas::with_options(
        SIDE,
        SIDE,
        CanvasOptions {
            color_type: depth,
            gpu: false,
            ..CanvasOptions::default()
        },
    )
    .expect("a canvas")
}

fn underlined(depth: PixelDepth, set: impl Fn(&mut Context2D)) -> Canvas {
    FontLibrary::new()
        .use_paths(Some(FAMILY), [OSWALD])
        .expect("Oswald");
    let mut canvas = canvas(depth);
    let ctx = canvas.context();
    assert!(ctx.set_font_css(&format!("28px {FAMILY}")), "a font");
    set(ctx);
    ctx.fill_text("H", 0.0, BASELINE, None);
    canvas
}

/// The strongest premultiplied alpha in the line's rows, read back in F32.
fn line_alpha(canvas: &mut Canvas) -> f32 {
    canvas
        .context()
        .get_image_data_as(
            0.0,
            0.0,
            SIDE,
            SIDE,
            PixelExportOptions {
                depth: PixelDepth::F32,
                premultiplied: true,
                ..PixelExportOptions::default()
            },
        )
        .expect("pixels")
        .pixels()
        .chunks(16 * SIDE as usize)
        .skip(LINE_ROWS.start)
        .take(LINE_ROWS.len())
        .flat_map(|row| row.chunks(16))
        .map(|p| f32::from_le_bytes([p[12], p[13], p[14], p[15]]))
        .fold(0.0, f32::max)
}

/// Draws the underline on an F16 and an F32 canvas and holds its alpha to
/// `alpha`.
fn keeps(alpha: f32, set: impl Fn(&mut Context2D)) {
    for depth in [PixelDepth::F16, PixelDepth::F32] {
        let got = line_alpha(&mut underlined(depth, &set));
        assert!(
            (got - alpha).abs() <= alpha * TOLERANCE,
            "on an {depth:?} canvas the line should have an alpha of \
             {alpha}, got {got}",
        );
    }
}

#[test]
fn a_named_line_colour_keeps_its_alpha() {
    keeps(0.3, |ctx| {
        ctx.set_text_decoration_css("underline rgb(0 0 0 / 0.3) 6px")
            .expect("a decoration");
    });
}

#[test]
fn a_faint_named_line_colour_draws() {
    keeps(0.001, |ctx| {
        ctx.set_text_decoration_css("underline rgb(0 0 0 / 0.001) 6px")
            .expect("a decoration");
    });
}

#[test]
fn a_line_colour_set_as_a_value_keeps_its_alpha() {
    keeps(0.3, |ctx| {
        ctx.set_text_decoration(
            TextDecoration {
                underline: true,
                ..TextDecoration::default()
            },
            TextDecorationStyle::Solid,
            Some(RgbaLinear::new_premultiplied(0.0, 0.0, 0.0, 0.3)),
            Some(6.0),
        );
    });
}

/// The pixel at the middle of the line's left end, which the line covers
/// whole.
const LINE_PIXEL: (f32, f32) = (8.0, 17.0);

/// The pixel at [`LINE_PIXEL`] of an eight-bit canvas, drawn by `draw`.
fn eight_bit_pixel(draw: impl Fn(&mut Context2D)) -> Vec<u8> {
    let mut canvas = underlined(PixelDepth::Uint8, draw);
    canvas
        .context()
        .get_image_data(LINE_PIXEL.0, LINE_PIXEL.1, 1.0, 1.0)
        .expect("a pixel")
        .pixels()
        .to_vec()
}

/// An eight-bit canvas draws a line in the bytes a fill of the same colour
/// draws: a half step, an alpha that is not a whole byte and a colour
/// outside sRGB all come out as the fill's, where the line was rounded to
/// bytes as Chrome rounds it and a wide colour gamut-mapped.
#[test]
fn an_eight_bit_line_draws_the_fill_s_bytes() {
    for css in [
        "rgb(10.5 20.4 30.6)",
        "rgb(10.5 20.4 30.6 / 0.3)",
        "color(display-p3 0 1 0)",
        "oklch(0.7 0.35 30)",
    ] {
        let line = eight_bit_pixel(|ctx| {
            ctx.set_text_decoration_css(&format!("underline {css} 6px"))
                .expect("a decoration");
        });
        let fill = eight_bit_pixel(|ctx| {
            ctx.set_fill_style_css(css).expect("a colour");
            ctx.fill_rect(
                0.0,
                LINE_ROWS.start as f32,
                SIDE,
                LINE_ROWS.len() as f32,
            );
            ctx.set_fill_style(RgbaLinear::new_premultiplied(
                0.0, 0.0, 0.0, 0.0,
            ));
        });
        assert_eq!(line, fill, "{css}");
    }
}

/// A line in a colour of whole bytes draws those bytes, as it did.
#[test]
fn an_eight_bit_line_in_bytes_is_unchanged() {
    let line = eight_bit_pixel(|ctx| {
        ctx.set_text_decoration_css("underline #336699 6px")
            .expect("a decoration");
    });
    assert_eq!(line, [0x33, 0x66, 0x99, 0xff]);
}

/// An eight-bit canvas leaves a faint line colour at the zero it rounds to.
#[test]
fn an_eight_bit_canvas_drops_a_faint_line_colour_as_before() {
    let line = eight_bit_pixel(|ctx| {
        ctx.set_text_decoration_css("underline rgb(0 0 0 / 0.001) 6px")
            .expect("a decoration");
    });
    assert_eq!(line, [0, 0, 0, 0]);
}

/// A canvas whose only ink is a drop-shadow, from a rectangle drawn off
/// the canvas and cast back onto all of it.
fn shadowed(depth: PixelDepth, color: &str) -> Canvas {
    let mut canvas = canvas(depth);
    let ctx = canvas.context();
    ctx.set_filter_css(&format!("drop-shadow({OFF_CANVAS}px 0 0 {color})"))
        .expect("a filter");
    ctx.fill_rect(-OFF_CANVAS, 0.0, SIDE, SIDE);
    canvas
}

/// How far the rectangle is drawn off the canvas, and its shadow cast back.
const OFF_CANVAS: f32 = 100.0;

/// The middle of the canvas, which the shadow covers whole.
const MIDDLE: f32 = SIDE / 2.0;

/// The colour a CPU fill still rounds before it multiplies by its alpha,
/// where a shadow stores the exact product: green 0.5 at 0.3 is 38.25
/// levels and stores 38, where the fill rounds 0.5 to 128 and stores 39 of
/// 77. #440 makes the fill store the product, and this is then empty.
const FILL_ROUNDS_TWICE: &str = "color(srgb 0.25 0.5 0.75 / 0.3)";

/// An sRGB colour's channels in levels, and its alpha.
type Levels = ([f64; 3], f64);

/// An eight-bit canvas draws a drop-shadow in the bytes its colour's exact
/// product rounds to, and in a fill's bytes wherever a fill stores that
/// product.
#[test]
fn an_eight_bit_drop_shadow_stores_its_colour_s_exact_product() {
    // Each colour with its channels in levels and its alpha, where it names
    // an sRGB colour; display-p3 green is opaque, where a fill's bytes are
    // the product.
    let cases: [(&str, Option<Levels>); 6] = [
        ("rgb(10.5 20.4 30.6)", Some(([10.5, 20.4, 30.6], 1.0))),
        ("rgb(10.5 20.4 30.6 / 0.3)", Some(([10.5, 20.4, 30.6], 0.3))),
        ("color(display-p3 0 1 0)", None),
        ("#336699", Some(([51.0, 102.0, 153.0], 1.0))),
        (FILL_ROUNDS_TWICE, Some(([63.75, 127.5, 191.25], 0.3))),
        ("rgb(255 128 1 / 0.5)", Some(([255.0, 128.0, 1.0], 0.5))),
    ];
    let pixel = |canvas: &mut Canvas, premultiplied: bool| {
        canvas
            .context()
            .get_image_data_as(
                MIDDLE,
                MIDDLE,
                1.0,
                1.0,
                PixelExportOptions {
                    premultiplied,
                    ..PixelExportOptions::default()
                },
            )
            .expect("a pixel")
            .pixels()
            .to_vec()
    };
    for (css, named) in cases {
        let mut shadow = shadowed(PixelDepth::Uint8, css);
        let mut fill = canvas(PixelDepth::Uint8);
        fill.context().set_fill_style_css(css).expect("a colour");
        fill.context().fill_rect(0.0, 0.0, SIDE, SIDE);
        if let Some((levels, alpha)) = named {
            let product = levels
                .iter()
                .map(|level| (level * alpha).round() as u8)
                .chain([(255.0 * alpha).round() as u8])
                .collect::<Vec<_>>();
            assert_eq!(pixel(&mut shadow, true), product, "{css}");
        }
        assert_eq!(
            pixel(&mut shadow, false) != pixel(&mut fill, false),
            css == FILL_ROUNDS_TWICE,
            "{css}: the shadow and a fill should differ only where the fill \
             rounds twice",
        );
    }
}

/// A float canvas draws a drop-shadow in the alpha its colour names, a
/// faint one included: the shadow is tinted on a layer of the canvas's own
/// floats, where Skia's layer for it was eight bits and drew 77/255 for 0.3
/// and nothing for 0.001.
#[test]
fn a_float_drop_shadow_keeps_its_colour_s_alpha() {
    for depth in [PixelDepth::F16, PixelDepth::F32] {
        for alpha in [0.3f32, 0.001] {
            let css = format!("rgb(10 20 30 / {alpha})");
            let got = shadowed(depth, &css)
                .context()
                .get_image_data_as(
                    MIDDLE,
                    MIDDLE,
                    1.0,
                    1.0,
                    PixelExportOptions {
                        depth: PixelDepth::F32,
                        premultiplied: true,
                        ..PixelExportOptions::default()
                    },
                )
                .expect("a pixel")
                .pixels()[12..16]
                .try_into()
                .map(f32::from_le_bytes)
                .expect("an alpha");
            assert!(
                (got - alpha).abs() <= alpha * TOLERANCE,
                "on an {depth:?} canvas {css} should cast a shadow of alpha \
                 {alpha}, got {got}",
            );
        }
    }
}

/// The side of the canvas a run's shadow is cast onto, wide enough for a
/// run of eight stems and its underline.
const RUN_CANVAS: (f32, f32) = (200.0, 120.0);

/// How far left a run is drawn, off the canvas, for only its shadow to land.
const RUN_OFF: f32 = 1000.0;

/// The rows above a run's underline, where only its glyphs cast: the run is
/// set at a baseline of 60, and the line's shadow starts at row 64.
const GLYPH_ROWS: usize = 62;

/// The shadow alone of `IIIIIIII` in 40px Oswald filled in `fill` under
/// `global_alpha`, cast in `shadow`, underlined in `rgb(0 90 200 / 0.6)`
/// where `lined`, read in F32 premultiplied.
fn run_shadow(
    depth: PixelDepth,
    fill: &str,
    lined: bool,
    global_alpha: f32,
    shadow: &str,
) -> Vec<f32> {
    FontLibrary::new()
        .use_paths(Some(FAMILY), [OSWALD])
        .expect("Oswald");
    let (width, height) = RUN_CANVAS;
    let mut canvas = Canvas::with_options(
        width,
        height,
        CanvasOptions {
            color_type: depth,
            gpu: false,
            ..CanvasOptions::default()
        },
    )
    .expect("a canvas");
    let ctx = canvas.context();
    ctx.set_fill_style_css(fill).expect("a colour");
    ctx.set_shadow_offset(-RUN_OFF, 0.0);
    ctx.set_shadow_color_css(shadow).expect("a colour");
    ctx.set_global_alpha(global_alpha);
    assert!(ctx.set_font_css(&format!("40px {FAMILY}")), "a font");
    if lined {
        ctx.set_text_decoration_css("underline rgb(0 90 200 / 0.6)")
            .expect("a decoration");
    }
    ctx.fill_text("IIIIIIII", RUN_OFF + 10.0, 60.0, None);
    canvas
        .context()
        .get_image_data_as(
            0.0,
            0.0,
            width,
            height,
            PixelExportOptions {
                depth: PixelDepth::F32,
                premultiplied: true,
                ..PixelExportOptions::default()
            },
        )
        .expect("pixels")
        .pixels()
        .chunks(16)
        .map(|pixel| {
            pixel[12..16]
                .try_into()
                .map(f32::from_le_bytes)
                .expect("an alpha")
        })
        .collect()
}

/// A run naming a line colour casts its shadow under a `globalAlpha` as the
/// one product of the alphas, its line's included, on every canvas: half of
/// `rgb(200 0 0 / 0.8)` lands where a whole `rgb(200 0 0 / 0.4)` does. The
/// line colour holds the `globalAlpha` apart from the glyphs' paint, so a
/// shadow that took the paint's alpha into its colour drew the line's at
/// half, 30 levels under, on eight bits and on F16 alike; and a run filled
/// transparent, its line still drawn, cast no line at all.
#[test]
fn a_named_line_colour_casts_its_shadow_under_a_global_alpha_once() {
    for depth in [PixelDepth::Uint8, PixelDepth::F16, PixelDepth::F32] {
        for fill in ["rgb(0 0 0)", "rgb(0 0 0 / 0)"] {
            let half = run_shadow(depth, fill, true, 0.5, "rgb(200 0 0 / 0.8)");
            let whole =
                run_shadow(depth, fill, true, 1.0, "rgb(200 0 0 / 0.4)");
            let line = &whole[GLYPH_ROWS * RUN_CANVAS.0 as usize..];
            assert!(
                line.iter().any(|&alpha| alpha > 0.2),
                "{depth:?} {fill}: the line casts no shadow to compare"
            );
            let apart = half.iter().zip(&whole).filter(|(a, b)| a != b);
            assert_eq!(apart.count(), 0, "{depth:?} {fill}");
        }
    }
}

/// The rounding #441 leaves, held so that it cannot pass for a fix: a run
/// filled translucent and naming a line colour moves only the `globalAlpha`
/// into its shadow's colour, which its line holds too, so on eight bits its
/// glyphs are rounded to a byte at the fill's alpha before the shadow
/// multiplies them, and their shadow lands up to a level from the one the
/// same run with no line casts. Flipped by a shadow that rounds the glyphs
/// once.
#[test]
fn a_translucent_run_naming_a_line_colour_rounds_its_glyphs_shadow_twice() {
    let fill = "rgb(0 0 0 / 0.5)";
    let shadow = "rgb(200 0 0 / 0.8)";
    let lined = run_shadow(PixelDepth::Uint8, fill, true, 0.7, shadow);
    let plain = run_shadow(PixelDepth::Uint8, fill, false, 0.7, shadow);
    let glyphs = GLYPH_ROWS * RUN_CANVAS.0 as usize;
    let levels = lined[..glyphs]
        .iter()
        .zip(&plain[..glyphs])
        .map(|(a, b)| ((a - b).abs() * 255.0).round() as u8)
        .filter(|&levels| levels > 0)
        .collect::<Vec<_>>();
    assert!(
        !levels.is_empty(),
        "#441's known miss is gone: update this pin"
    );
    assert_eq!(levels.iter().max(), Some(&1), "{} px apart", levels.len());
}
