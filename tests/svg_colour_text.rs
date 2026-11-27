//! Text in an SVG document set in a face with a `COLR` table is drawn as
//! `fillText` draws it, through this library's own painter for colour glyphs
//! rather than the platform's scaler, which draws a COLRv0 glyph on macOS
//! 27.2 as nothing, a COLRv1 one on macOS 15 and 26 as nothing, and both
//! elsewhere in other colours than Chrome's (#663).
//!
//! Each case is drawn twice on a CPU canvas: as an SVG document rasterized
//! and drawn as an image, and as the `fillText` calls that document's text
//! comes to, under `geometricPrecision`, since the SVG module draws text
//! unhinted. Every glyph here is a colour glyph: [`Svg::rasterize`] draws a
//! plain glyph's mask with Skia's default text contrast and gamma rather than
//! a canvas's, so plain text and the control that it agrees with `fillText`
//! are in the JavaScript suite, whose documents a canvas plays back.

use meo_skia_canvas::prelude::*;

const COLR_V1: &str =
    "tests/assets/fonts/color-fonts/test_glyphs-glyf_colr_1_variable.ttf";
const COLR_V0: &str = "tests/assets/fonts/colr-v0/COLR-palettes-test-font.ttf";

/// A COLRv1 test glyph whose paint graph the platform scalers draw apart
/// from Chrome's: up to 85 levels on Linux's FreeType at 48px, and in other
/// colours on macOS's CoreText.
const V1_GLYPH: &str = "\u{f0101}";

/// How far a premultiplied channel may stand between the document and the
/// canvas. The two draw the same glyphs through the same painter: the canvas
/// onto its eight-bit sRGB bytes, the document into `Svg::rasterize`'s half
/// floats, rounded to bytes once when the image is drawn, so a pixel, on a
/// glyph's edge or inside it, can land a level apart. Inside a glyph drawn at
/// an alpha of 0.4, both sides land that alpha as 102, 0.4 of 255 exactly;
/// the level is in the glyph's own colours scaled by it, which the half
/// floats carry unrounded and the canvas lands on its bytes by its own
/// rounding. It is precision, not a fault.
///
/// Compared premultiplied, as the canvas stores them: read back
/// unpremultiplied, a level is up to 255 levels of colour at an alpha of 1
/// and up to 3 at an alpha of 102.
const TOLERANCE: u8 = 1;

/// How far a text composited as a group by its `opacity` may stand from the
/// same glyph drawn under `globalAlpha`: the group is drawn into a layer and
/// the layer composited, which rounds once more than compositing the glyph
/// does.
const GROUP_TOLERANCE: u8 = 2;

const WIDTH: u32 = 200;
const HEIGHT: u32 = 80;

fn register() {
    let library = FontLibrary::new();
    for (family, path) in [("SvgColrV1", COLR_V1), ("SvgColrV0", COLR_V0)] {
        library
            .use_paths(Some(family), [path])
            .expect("the face registers");
    }
}

/// A canvas the size of every document here, on the CPU.
fn page() -> Canvas {
    let mut canvas = Canvas::new(WIDTH as f32, HEIGHT as f32);
    canvas.set_gpu(false);
    canvas
}

/// `canvas`'s pixels, premultiplied -- see [`TOLERANCE`].
fn pixels(canvas: &mut Canvas) -> Vec<u8> {
    let premultiplied = ImageDataExport {
        pixels: PixelExportOptions {
            premultiplied: true,
            ..PixelExportOptions::default()
        },
        ..ImageDataExport::default()
    };
    canvas
        .to_image_data(None, &premultiplied)
        .expect("read back")
        .pixels()
        .to_vec()
}

/// The pixels of `body` drawn as an SVG document of this size, whose root
/// carries `root` besides its namespace and size.
fn drawn(root: &str, body: &str) -> Vec<u8> {
    let xml = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{WIDTH}" height="{HEIGHT}" {root}>{body}</svg>"#
    );
    let image = Svg::parse(&xml)
        .expect("parses")
        .rasterize(WIDTH, HEIGHT)
        .expect("rasterizes");
    let mut canvas = page();
    canvas.context().draw_image(&image, 0.0, 0.0);
    pixels(&mut canvas)
}

/// The pixels `draw` leaves on a canvas whose text is set unhinted.
fn painted(draw: impl FnOnce(&mut Context2D)) -> Vec<u8> {
    let mut canvas = page();
    let ctx = canvas.context();
    ctx.set_text_rendering(TextRendering::GeometricPrecision);
    draw(ctx);
    pixels(&mut canvas)
}

/// Sets `css` as the font and `fill` as the fill style.
fn set(ctx: &mut Context2D, css: &str, fill: &str) {
    assert!(ctx.set_font_css(css), "{css}");
    ctx.set_fill_style_css(fill).expect(fill);
}

/// How many pixels stand more than `tolerance` apart, and the farthest a
/// channel stands, for an assertion message.
fn apart(got: &[u8], expected: &[u8], tolerance: u8) -> (usize, u8) {
    let far = got
        .chunks(4)
        .zip(expected.chunks(4))
        .map(|(a, b)| {
            a.iter()
                .zip(b)
                .map(|(a, b)| a.abs_diff(*b))
                .max()
                .unwrap_or(0)
        })
        .collect::<Vec<_>>();
    (
        far.iter().filter(|&&d| d > tolerance).count(),
        far.iter().copied().max().unwrap_or(0),
    )
}

fn inked(pixels: &[u8]) -> usize {
    pixels.chunks(4).filter(|p| p[3] != 0).count()
}

fn assert_agree(case: &str, got: &[u8], expected: &[u8]) {
    assert_within(case, got, expected, TOLERANCE);
}

fn assert_within(case: &str, got: &[u8], expected: &[u8], tolerance: u8) {
    assert!(inked(expected) > 0, "{case}: the canvas drew nothing");
    let (pixels, far) = apart(got, expected, tolerance);
    assert_eq!(
        pixels,
        0,
        "{case}: {pixels} pixels apart, by up to {far} levels; the document \
         inked {} pixels and the canvas {}",
        inked(got),
        inked(expected)
    );
}

#[test]
fn colour_text_in_an_svg_is_drawn_as_fill_text_draws_it() {
    register();
    for (family, text) in [("SvgColrV0", "AB"), ("SvgColrV1", V1_GLYPH)] {
        let got = drawn(
            "",
            &format!(
                r#"<text x="10" y="60" font-family="{family}" font-size="48" fill="rgb(40,90,160)">{text}</text>"#
            ),
        );
        let expected = painted(|ctx| {
            set(ctx, &format!("48px {family}"), "rgb(40,90,160)");
            ctx.fill_text(text, 10.0, 60.0, None);
        });
        assert_agree(family, &got, &expected);
    }
}

/// A `text-anchor` places each chunk by its own advance: the middle of it,
/// or its end, at the position the chunk starts from.
#[test]
fn colour_text_is_anchored_as_it_asks() {
    register();
    for (anchor, align) in
        [("middle", TextAlign::Center), ("end", TextAlign::Right)]
    {
        let got = drawn(
            "",
            &format!(
                r##"<text x="100" y="60" text-anchor="{anchor}" font-family="SvgColrV0" font-size="30" fill="#000">AB</text>"##
            ),
        );
        let expected = painted(|ctx| {
            set(ctx, "30px SvgColrV0", "#000");
            ctx.set_text_align(align);
            ctx.fill_text("AB", 100.0, 60.0, None);
        });
        assert_agree(anchor, &got, &expected);
    }
}

/// Each character a position list reaches is placed where the list says,
/// and a `tspan` moves its own characters by its `dx` and `dy`.
#[test]
fn colour_text_is_placed_where_its_positions_say() {
    register();
    let got = drawn(
        "",
        r##"<text x="10 90" y="40" font-family="SvgColrV0" font-size="30" fill="#000">AB<tspan dx="20" dy="15">C</tspan></text>"##,
    );
    let expected = painted(|ctx| {
        set(ctx, "30px SvgColrV0", "#000");
        let b = 90.0;
        ctx.fill_text("A", 10.0, 40.0, None);
        ctx.fill_text("B", b, 40.0, None);
        let advance = ctx.measure_text("B", None).width;
        ctx.fill_text("C", b + advance + 20.0, 55.0, None);
    });
    assert_agree("positions", &got, &expected);
}

/// The text is drawn under every transform above it and the root's
/// `viewBox`, and between what the document draws before it and after it,
/// each drawn once: the rect after it is translucent, so a second drawing of
/// it, or of the one before, would show.
#[test]
fn colour_text_keeps_its_transform_and_its_place_in_the_paint_order() {
    register();
    let got = drawn(
        r#"viewBox="0 0 100 40""#,
        r##"<rect width="100" height="40" fill="#fec"/><g transform="translate(5 2) rotate(10)"><text x="10" y="25" font-family="SvgColrV0" font-size="18" fill="#000">AB</text></g><rect x="20" width="10" height="40" fill="#0a0" fill-opacity="0.5"/>"##,
    );
    let expected = painted(|ctx| {
        ctx.set_transform(Affine::scale(2.0, 2.0));
        ctx.set_fill_style_css("#fec").expect("colour");
        ctx.fill_rect(0.0, 0.0, 100.0, 40.0);
        ctx.translate(5.0, 2.0);
        ctx.rotate(10f32.to_radians());
        set(ctx, "18px SvgColrV0", "#000");
        ctx.fill_text("AB", 10.0, 25.0, None);
        ctx.set_transform(Affine::scale(2.0, 2.0));
        ctx.set_fill_style_css("rgb(0 170 0 / 0.5)")
            .expect("colour");
        ctx.fill_rect(20.0, 0.0, 10.0, 40.0);
    });
    assert_agree("transform", &got, &expected);
}

/// A colour text's own `opacity` composites it as one group, and
/// `fill-opacity` and `currentcolor` reach the foreground its glyphs take.
#[test]
fn colour_text_takes_its_paint_and_opacity() {
    register();
    let got = drawn(
        r##"color="#c03""##,
        r#"<text x="10" y="60" opacity="0.5" fill="currentcolor" fill-opacity="0.8" font-family="SvgColrV1" font-size="48">&#xf0101;</text>"#,
    );
    let expected = painted(|ctx| {
        set(ctx, "48px SvgColrV1", "rgb(204 0 51 / 0.8)");
        ctx.set_global_alpha(0.5);
        ctx.fill_text(V1_GLYPH, 10.0, 60.0, None);
    });
    assert_within("paint", &got, &expected, GROUP_TOLERANCE);
}
