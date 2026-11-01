//! A float canvas keeps a draw whose alpha is under half of one 255th.
//!
//! Skia drops a source-over draw whose paint alpha rounds to zero in eight
//! bits, whatever the pixels it draws into, so on an F16 or F32 canvas a
//! `globalAlpha` of 0.001, or a fill of `rgba(0, 0, 0, 0.001)`, drew
//! nothing at all. Each test is one way a paint reaches the page, drawn on
//! both float depths; the last two hold an eight-bit canvas to what it drew
//! before.

use std::ops::Range;

use meo_skia_canvas::prelude::*;

const OSWALD: &str = "tests/assets/fonts/Oswald/Oswald-VariableFont_wght.ttf";
const FAMILY: &str = "FaintAlphaOswald";

/// An alpha under half of one 255th, which Skia's eight-bit rounding of a
/// paint's alpha takes to zero: `0.5 / 255` is about 0.00196.
const FAINT: f32 = 0.001;

/// The side of the square canvas every case draws on.
const SIDE: f32 = 32.0;

/// How far the strongest pixel may be from the alpha it should have, as a
/// share of it. F16 holds 0.001 to within about a two-thousandth.
const TOLERANCE: f32 = 0.01;

/// Where a shape is put so that only its shadow lands on the canvas: a
/// shadow offset by this much brings it back to the origin.
const OFF_CANVAS: f32 = 100.0;

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

fn read(canvas: &mut Canvas, depth: PixelDepth) -> Vec<u8> {
    canvas
        .context()
        .get_image_data_as(
            0.0,
            0.0,
            SIDE,
            SIDE,
            PixelExportOptions {
                depth,
                premultiplied: true,
                ..PixelExportOptions::default()
            },
        )
        .expect("pixels")
        .pixels()
        .to_vec()
}

/// The strongest premultiplied alpha in `rows` of the page, read back in
/// F32.
fn strongest(canvas: &mut Canvas, rows: Range<usize>) -> f32 {
    read(canvas, PixelDepth::F32)
        .chunks(16 * SIDE as usize)
        .skip(rows.start)
        .take(rows.len())
        .flat_map(|row| row.chunks(16))
        .map(|p| f32::from_le_bytes([p[12], p[13], p[14], p[15]]))
        .fold(0.0, f32::max)
}

/// Draws with `draw` on an F16 and an F32 canvas and holds the strongest
/// pixel of each to `alpha`, within [`TOLERANCE`] of it.
fn keeps(alpha: f32, draw: impl Fn(&mut Context2D)) {
    keeps_in(0..SIDE as usize, alpha, draw);
}

/// [`keeps`], looking only at `rows`.
fn keeps_in(rows: Range<usize>, alpha: f32, draw: impl Fn(&mut Context2D)) {
    keeps_within(rows, alpha, alpha * TOLERANCE, draw);
}

/// [`keeps_in`], within `slack` of `alpha`.
fn keeps_within(
    rows: Range<usize>,
    alpha: f32,
    slack: f32,
    draw: impl Fn(&mut Context2D),
) {
    FontLibrary::new()
        .use_paths(Some(FAMILY), [OSWALD])
        .expect("Oswald");
    for depth in [PixelDepth::F16, PixelDepth::F32] {
        let mut canvas = canvas(depth);
        draw(canvas.context());
        let got = strongest(&mut canvas, rows.clone());
        assert!(
            (got - alpha).abs() <= slack,
            "on an {depth:?} canvas the draw should leave an alpha of \
             {alpha}, got {got}",
        );
    }
}

fn opaque_image() -> Image {
    Image::from_pixels(
        &[255u8; 8 * 8 * 4],
        8,
        8,
        8 * 4,
        PixelExportOptions::default(),
    )
    .expect("an image")
}

fn faint_black() -> RgbaLinear {
    RgbaLinear::new_premultiplied(0.0, 0.0, 0.0, FAINT)
}

fn font(ctx: &mut Context2D) {
    assert!(ctx.set_font_css(&format!("28px {FAMILY}")), "a font");
}

#[test]
fn a_faint_colour_fills() {
    keeps(FAINT, |ctx| {
        ctx.set_fill_style(faint_black());
        ctx.fill_rect(0.0, 0.0, SIDE, SIDE);
    });
}

#[test]
fn a_faint_global_alpha_fills() {
    keeps(FAINT, |ctx| {
        ctx.set_global_alpha(FAINT);
        ctx.fill_rect(0.0, 0.0, SIDE, SIDE);
    });
}

#[test]
fn a_faint_path_fills() {
    keeps(FAINT, |ctx| {
        ctx.set_fill_style(faint_black());
        ctx.begin_path();
        ctx.arc(16.0, 16.0, 12.0, 0.0, std::f32::consts::TAU, false)
            .expect("an arc");
        ctx.fill(FillRule::NonZero);
    });
}

#[test]
fn a_faint_stroke_strokes() {
    keeps(FAINT, |ctx| {
        ctx.set_stroke_style(faint_black());
        ctx.set_line_width(6.0);
        ctx.stroke_rect(8.0, 8.0, 16.0, 16.0);
    });
}

#[test]
fn a_faint_dashed_stroke_strokes() {
    keeps(FAINT, |ctx| {
        ctx.set_global_alpha(FAINT);
        ctx.set_line_width(6.0);
        ctx.set_line_dash(&[8.0, 4.0]);
        ctx.stroke_rect(8.0, 8.0, 16.0, 16.0);
    });
}

#[test]
fn faint_text_fills() {
    keeps(FAINT, |ctx| {
        font(ctx);
        ctx.set_fill_style(faint_black());
        ctx.fill_text("H", 4.0, 28.0, None);
    });
}

#[test]
fn text_under_a_faint_global_alpha_fills() {
    keeps(FAINT, |ctx| {
        font(ctx);
        ctx.set_global_alpha(FAINT);
        ctx.fill_text("H", 4.0, 28.0, None);
    });
}

/// The second draw of a string draws the run kept from the first rather
/// than laying it out again.
#[test]
fn faint_text_drawn_again_fills() {
    keeps(FAINT, |ctx| {
        font(ctx);
        ctx.set_fill_style(faint_black());
        ctx.fill_text("H", 4.0, 28.0, None);
        ctx.clear_rect(0.0, 0.0, SIDE, SIDE);
        ctx.fill_text("H", 4.0, 28.0, None);
    });
}

#[test]
fn faint_text_strokes() {
    keeps(FAINT, |ctx| {
        font(ctx);
        ctx.set_stroke_style(faint_black());
        ctx.set_line_width(4.0);
        ctx.stroke_text("H", 4.0, 28.0, None);
    });
}

/// The rows an underline 6px thick fills under text set at a baseline of
/// [`UNDERLINED_BASELINE`] in [`font`], clear of the glyph above them.
const UNDERLINE_ROWS: Range<usize> = 14..21;

/// The baseline of the underlined cases: high enough on the canvas for
/// the line to fit under it.
const UNDERLINED_BASELINE: f32 = 12.0;

#[test]
fn a_faint_underline_draws() {
    keeps_in(UNDERLINE_ROWS, FAINT, |ctx| {
        font(ctx);
        ctx.set_text_decoration_css("underline 6px")
            .expect("underline");
        ctx.set_fill_style(faint_black());
        ctx.fill_text("H", 0.0, UNDERLINED_BASELINE, None);
    });
}

/// In a colour of its own, the line takes its own alpha times the global
/// alpha. A line colour is held in eight bits, so a half is 128/255.
#[test]
fn a_named_underline_colour_under_a_faint_global_alpha_draws() {
    let half = f32::from(128u8) / 255.0;
    keeps_in(UNDERLINE_ROWS, half * FAINT, |ctx| {
        font(ctx);
        ctx.set_text_decoration_css("underline rgb(0 0 0 / 0.5) 6px")
            .expect("underline");
        ctx.set_global_alpha(FAINT);
        ctx.fill_text("H", 0.0, UNDERLINED_BASELINE, None);
    });
}

/// A dotted line too fine for its dots to be told apart is drawn whole at
/// the share of the line its dots cover, which is fainter still. Its pixels
/// are part-covered, so only their being there is held.
#[test]
fn a_fine_dotted_underline_under_a_faint_global_alpha_draws() {
    FontLibrary::new()
        .use_paths(Some(FAMILY), [OSWALD])
        .expect("Oswald");
    for depth in [PixelDepth::F16, PixelDepth::F32] {
        let mut canvas = canvas(depth);
        let ctx = canvas.context();
        font(ctx);
        ctx.set_text_decoration_css("underline dotted 0.25px")
            .expect("underline");
        ctx.set_global_alpha(FAINT);
        ctx.fill_text("H", 0.0, UNDERLINED_BASELINE, None);
        let got = strongest(&mut canvas, UNDERLINE_ROWS);
        assert!(
            got > 0.0 && got < FAINT,
            "on an {depth:?} canvas the line should leave an alpha under \
             {FAINT}, got {got}",
        );
    }
}

/// A paragraph paints with its own paints, so the context's global alpha
/// reaches it through a layer.
#[test]
fn a_paragraph_under_a_faint_global_alpha_draws() {
    let library = FontLibrary::new();
    library.use_paths(Some(FAMILY), [OSWALD]).expect("Oswald");
    let engine = TextEngine::new(&library);
    let style = TextStyle {
        font_families: vec![FAMILY.into()],
        font_size: 28.0,
        color: RgbaLinear::opaque(0.0, 0.0, 0.0),
        ..TextStyle::default()
    };
    let paragraph = engine.layout_text("H", &style, SIDE);
    keeps(FAINT, |ctx| {
        ctx.set_global_alpha(FAINT);
        ctx.draw_paragraph(&paragraph, 0.0, 0.0);
    });
}

#[test]
fn an_image_under_a_faint_global_alpha_draws() {
    keeps(FAINT, |ctx| {
        ctx.set_global_alpha(FAINT);
        ctx.draw_image_sized(&opaque_image(), 0.0, 0.0, SIDE, SIDE);
    });
}

#[test]
fn a_canvas_under_a_faint_global_alpha_draws() {
    keeps(FAINT, |ctx| {
        let mut source = Canvas::new(SIDE, SIDE);
        source.context().fill_rect(0.0, 0.0, SIDE, SIDE);
        ctx.set_global_alpha(FAINT);
        ctx.draw_canvas(&mut source, 0.0, 0.0);
    });
}

#[test]
fn a_pattern_under_a_faint_global_alpha_fills() {
    keeps(FAINT, |ctx| {
        let pattern =
            ctx.create_pattern(&opaque_image(), PatternRepeat::Repeat);
        ctx.set_fill_pattern(&pattern);
        ctx.set_global_alpha(FAINT);
        ctx.fill_rect(0.0, 0.0, SIDE, SIDE);
    });
}

#[test]
fn a_gradient_under_a_faint_global_alpha_fills() {
    keeps(FAINT, |ctx| {
        let gradient = Shader::linear_gradient(
            Point { x: 0.0, y: 0.0 },
            Point { x: SIDE, y: 0.0 },
            &[
                GradientStop {
                    position: 0.0,
                    color: RgbaLinear::opaque(1.0, 0.0, 0.0),
                },
                GradientStop {
                    position: 1.0,
                    color: RgbaLinear::opaque(0.0, 0.0, 1.0),
                },
            ],
            GradientColorSpace::Srgb,
        )
        .expect("a gradient");
        ctx.set_fill_shader(&gradient);
        ctx.set_global_alpha(FAINT);
        ctx.fill_rect(0.0, 0.0, SIDE, SIDE);
    });
}

#[test]
fn a_texture_under_a_faint_global_alpha_fills() {
    keeps(FAINT, |ctx| {
        let texture = Texture::new(&TextureOptions {
            line: 4.0,
            spacing: (4.0, 4.0),
            ..TextureOptions::default()
        });
        ctx.set_fill_texture(&texture);
        ctx.set_global_alpha(FAINT);
        ctx.fill_rect(0.0, 0.0, SIDE, SIDE);
    });
}

/// A texture drawn as outlines fills the marks' outlines, cut to the path,
/// rather than clipping its grid to the path.
#[test]
fn an_outlined_texture_under_a_faint_global_alpha_fills() {
    keeps(FAINT, |ctx| {
        let texture = Texture::new(&TextureOptions {
            line: 4.0,
            spacing: (4.0, 4.0),
            outline: true,
            ..TextureOptions::default()
        });
        ctx.set_fill_texture(&texture);
        ctx.set_global_alpha(FAINT);
        ctx.fill_rect(0.0, 0.0, SIDE, SIDE);
    });
}

#[test]
fn a_shadow_under_a_faint_global_alpha_is_cast() {
    keeps(FAINT, |ctx| {
        ctx.set_shadow_color(RgbaLinear::opaque(0.0, 0.0, 0.0));
        ctx.set_shadow_offset(OFF_CANVAS, 0.0);
        ctx.set_global_alpha(FAINT);
        ctx.fill_rect(-OFF_CANVAS, 0.0, SIDE, SIDE);
    });
}

/// Not faint, but the same path: the shape is drawn into the shadow
/// filter's layer, which Skia's raster backend gives back in steps of one
/// 255th, so half came back as 128/255. Both depths hold a half exactly,
/// so the slack is far inside the 0.002 between the two.
#[test]
fn a_shadow_under_half_a_global_alpha_is_half() {
    keeps_within(0..SIDE as usize, 0.5, 1e-4, |ctx| {
        ctx.set_shadow_color(RgbaLinear::opaque(0.0, 0.0, 0.0));
        ctx.set_shadow_offset(OFF_CANVAS, 0.0);
        ctx.set_global_alpha(0.5);
        ctx.fill_rect(-OFF_CANVAS, 0.0, SIDE, SIDE);
    });
}

#[test]
fn a_faint_blur_fills_its_interior() {
    keeps(FAINT, |ctx| {
        ctx.set_filter_css("blur(1px)").expect("a blur");
        ctx.set_fill_style(faint_black());
        ctx.fill_rect(-8.0, -8.0, SIDE + 16.0, SIDE + 16.0);
    });
}

#[test]
fn a_faint_lighter_fill_adds() {
    keeps(FAINT, |ctx| {
        ctx.set_global_composite_operation(BlendMode::Lighter);
        ctx.set_fill_style(faint_black());
        ctx.fill_rect(0.0, 0.0, SIDE, SIDE);
    });
}

#[test]
fn a_faint_destination_over_fill_goes_beneath() {
    keeps(FAINT, |ctx| {
        ctx.set_global_composite_operation(BlendMode::DestinationOver);
        ctx.set_fill_style(faint_black());
        ctx.fill_rect(0.0, 0.0, SIDE, SIDE);
    });
}

#[test]
fn a_faint_layer_composites() {
    keeps(FAINT, |ctx| {
        ctx.save_layer_with(FAINT, None, None);
        ctx.fill_rect(0.0, 0.0, SIDE, SIDE);
        ctx.restore();
    });
}

/// The page every eight-bit case draws over: opaque, so that a faint draw
/// has something to move.
fn grey_page() -> Canvas {
    let mut canvas = canvas(PixelDepth::Uint8);
    let ctx = canvas.context();
    ctx.set_fill_style(RgbaLinear::opaque(0.2, 0.2, 0.2));
    ctx.fill_rect(0.0, 0.0, SIDE, SIDE);
    canvas
}

/// An eight-bit canvas leaves a faint draw to Skia's skip. Drawn as a float
/// canvas draws it, a faint fill in a colour far outside sRGB moves a level
/// of the page; skipped, it moves none.
#[test]
fn an_eight_bit_canvas_draws_a_faint_fill_as_before() {
    let mut canvas = grey_page();
    let before = read(&mut canvas, PixelDepth::Uint8);
    let ctx = canvas.context();
    ctx.set_fill_style(RgbaLinear::new_premultiplied(
        10.0 * FAINT,
        0.0,
        0.0,
        FAINT,
    ));
    ctx.fill_rect(0.0, 0.0, SIDE, SIDE);
    assert_eq!(read(&mut canvas, PixelDepth::Uint8), before);
}

/// An eight-bit canvas's shadow takes the global alpha into its colour, so
/// the two are rounded once: 0.3 times a half is 38.25/255 and stores 38.
/// Taken through the shape, 0.3 is 77/255 in the filter's layer and half of
/// that rounds to 39.
#[test]
fn an_eight_bit_canvas_casts_a_shadow_under_global_alpha_as_one_product() {
    let mut canvas = canvas(PixelDepth::Uint8);
    let ctx = canvas.context();
    ctx.set_shadow_color(RgbaLinear::new_premultiplied(0.0, 0.0, 0.0, 0.5));
    ctx.set_shadow_offset(OFF_CANVAS, 0.0);
    ctx.set_global_alpha(0.3);
    ctx.fill_rect(-OFF_CANVAS, 0.0, SIDE, SIDE);
    assert_eq!(read(&mut canvas, PixelDepth::Uint8)[3], 38);
}
