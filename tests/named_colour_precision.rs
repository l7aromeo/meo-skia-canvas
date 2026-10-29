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

/// An eight-bit canvas draws a drop-shadow in the bytes a fill of its
/// colour draws.
#[test]
fn an_eight_bit_drop_shadow_draws_the_fill_s_bytes() {
    for css in [
        "rgb(10.5 20.4 30.6)",
        "rgb(10.5 20.4 30.6 / 0.3)",
        "color(display-p3 0 1 0)",
        "#336699",
    ] {
        let mut shadow = shadowed(PixelDepth::Uint8, css);
        let mut fill = canvas(PixelDepth::Uint8);
        fill.context().set_fill_style_css(css).expect("a colour");
        fill.context().fill_rect(0.0, 0.0, SIDE, SIDE);
        let at = |canvas: &mut Canvas| {
            canvas
                .context()
                .get_image_data(MIDDLE, MIDDLE, 1.0, 1.0)
                .expect("a pixel")
                .pixels()
                .to_vec()
        };
        assert_eq!(at(&mut shadow), at(&mut fill), "{css}");
    }
}
