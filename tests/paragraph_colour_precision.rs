//! A paragraph's decoration and text shadow are painted in the colour their
//! style names, at its precision, as its text is.
//!
//! SkParagraph holds both colours as an eight-bit `SkColor`, and they were
//! drawn in that: on an F16 or F32 canvas a line or a shadow in alpha 0.3
//! came out at 77/255 beside text at 0.3, and on an eight-bit canvas a
//! colour outside sRGB was gamut-mapped where a fill of it is clipped.
//! SkParagraph draws neither -- `node::oblique::Passes` does -- so they are
//! drawn in the colours the style named.

use meo_skia_canvas::prelude::*;

const OSWALD: &str = "tests/assets/fonts/Oswald/Oswald-VariableFont_wght.ttf";
const FAMILY: &str = "ParagraphColourOswald";

/// The side of the square canvas every case draws on.
const SIDE: f32 = 64.0;

/// How far a pixel may be from the alpha it should have, as a share of it:
/// F16 holds a value to about a two-thousandth.
const TOLERANCE: f32 = 0.002;

/// How far a shadow is cast, and its paragraph drawn back, so that only the
/// shadow lands on the canvas.
const OFF_CANVAS: f32 = 200.0;

/// A transparent colour, so that only the line or the shadow inks.
fn clear() -> RgbaLinear {
    RgbaLinear::new_premultiplied(0.0, 0.0, 0.0, 0.0)
}

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

/// `HH` in 40px Oswald, transparent, with `decorate` applied to its style.
fn paragraph(decorate: impl Fn(&mut TextStyle)) -> Paragraph {
    let library = FontLibrary::new();
    library.use_paths(Some(FAMILY), [OSWALD]).expect("Oswald");
    let mut style = TextStyle {
        font_families: vec![FAMILY.into()],
        font_size: 40.0,
        color: clear(),
        ..TextStyle::default()
    };
    decorate(&mut style);
    TextEngine::new(&library).layout_text("HH", &style, SIDE * 4.0)
}

fn underlined(color: RgbaLinear) -> Paragraph {
    paragraph(|style| {
        style.decoration = TextDecoration {
            underline: true,
            ..TextDecoration::default()
        };
        style.decoration_color = Some(color);
        style.decoration_thickness = 4.0;
    })
}

fn shadowed(color: RgbaLinear) -> Paragraph {
    paragraph(|style| {
        style.shadows = vec![TextShadow {
            color,
            offset_x: OFF_CANVAS,
            offset_y: 0.0,
            blur_sigma: 0.0,
        }];
    })
}

/// The strongest premultiplied alpha on `paragraph` drawn at `x`, read back
/// in F32.
fn strongest(depth: PixelDepth, paragraph: &Paragraph, x: f32) -> f32 {
    let mut canvas = canvas(depth);
    canvas.context().draw_paragraph(paragraph, x, 0.0);
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
        .chunks(16)
        .map(|p| f32::from_le_bytes([p[12], p[13], p[14], p[15]]))
        .fold(0.0, f32::max)
}

fn keeps(paragraph: &Paragraph, x: f32, alpha: f32) {
    for depth in [PixelDepth::F16, PixelDepth::F32] {
        let got = strongest(depth, paragraph, x);
        assert!(
            (got - alpha).abs() <= alpha * TOLERANCE,
            "on an {depth:?} canvas the paragraph should leave an alpha of \
             {alpha}, got {got}",
        );
    }
}

#[test]
fn a_paragraph_s_decoration_keeps_its_alpha() {
    keeps(
        &underlined(RgbaLinear::new_premultiplied(0.0, 0.0, 0.0, 0.3)),
        0.0,
        0.3,
    );
}

#[test]
fn a_paragraph_s_text_shadow_keeps_its_alpha() {
    keeps(
        &shadowed(RgbaLinear::new_premultiplied(0.0, 0.0, 0.0, 0.3)),
        -OFF_CANVAS,
        0.3,
    );
}

/// The most opaque pixel `paragraph` leaves on an eight-bit canvas when
/// drawn at `x`, unpremultiplied.
fn eight_bit_ink(paragraph: &Paragraph, x: f32) -> Vec<u8> {
    let mut canvas = canvas(PixelDepth::Uint8);
    canvas.context().draw_paragraph(paragraph, x, 0.0);
    canvas
        .context()
        .get_image_data(0.0, 0.0, SIDE, SIDE)
        .expect("pixels")
        .pixels()
        .chunks(4)
        .max_by_key(|p| p[3])
        .expect("a pixel")
        .to_vec()
}

/// The pixel a fill of `color` leaves on an eight-bit canvas.
fn eight_bit_fill(color: RgbaLinear) -> Vec<u8> {
    let mut canvas = canvas(PixelDepth::Uint8);
    canvas.context().set_fill_style(color);
    canvas.context().fill_rect(0.0, 0.0, SIDE, SIDE);
    canvas
        .context()
        .get_image_data(0.0, 0.0, 1.0, 1.0)
        .expect("a pixel")
        .pixels()
        .to_vec()
}

/// On an eight-bit canvas a line or a shadow in a colour outside sRGB draws
/// the bytes a fill of it does, where it was gamut-mapped; one in whole
/// bytes draws them, as it did.
#[test]
fn an_eight_bit_paragraph_line_and_shadow_draw_the_fill_s_bytes() {
    let wide = RgbaLinear::from_css("color(display-p3 0 1 0)").expect("P3");
    let bytes = RgbaLinear::from_css("#336699").expect("hex");
    for color in [wide, bytes] {
        assert_eq!(
            eight_bit_ink(&underlined(color), 0.0),
            eight_bit_fill(color),
            "a line in {color:?}",
        );
        assert_eq!(
            eight_bit_ink(&shadowed(color), -OFF_CANVAS),
            eight_bit_fill(color),
            "a shadow in {color:?}",
        );
    }
}
