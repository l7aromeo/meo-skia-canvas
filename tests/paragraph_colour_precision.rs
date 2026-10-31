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

/// The float pages a paragraph lands on, each with a canvas space a fill
/// paints its colour exactly in -- see `tests/float_page_colour.rs`.
const FLOAT_PAGES: [(PixelColorSpace, PixelDepth); 8] = [
    (PixelColorSpace::Srgb, PixelDepth::F32),
    (PixelColorSpace::Srgb, PixelDepth::F16),
    (PixelColorSpace::SrgbLinear, PixelDepth::F32),
    (PixelColorSpace::SrgbLinear, PixelDepth::F16),
    (PixelColorSpace::DisplayP3, PixelDepth::F32),
    (PixelColorSpace::DisplayP3, PixelDepth::F16),
    (PixelColorSpace::Rec2020, PixelDepth::F32),
    (PixelColorSpace::Rec2020, PixelDepth::F16),
];

/// The side of the page a whole letter's stem covers.
const PAGE: f32 = 120.0;

fn page(space: PixelColorSpace, depth: PixelDepth) -> Canvas {
    Canvas::with_options(
        PAGE,
        PAGE,
        CanvasOptions {
            color_type: depth,
            color_space: space,
            ..CanvasOptions::default()
        },
    )
    .expect("a canvas")
}

/// Every pixel of `canvas` covered whole, premultiplied F32 in its space.
fn covered(canvas: &mut Canvas, space: PixelColorSpace) -> Vec<[f32; 4]> {
    let data = canvas
        .context()
        .get_image_data_as(
            0.0,
            0.0,
            PAGE,
            PAGE,
            PixelExportOptions {
                depth: PixelDepth::F32,
                color_space: space,
                premultiplied: true,
                ..PixelExportOptions::default()
            },
        )
        .expect("pixels");
    let floats: Vec<f32> = data
        .pixels()
        .as_chunks::<4>()
        .0
        .iter()
        .map(|bytes| f32::from_le_bytes(*bytes))
        .collect();
    floats
        .as_chunks::<4>()
        .0
        .iter()
        .copied()
        .filter(|pixel| pixel[3] == 1.0)
        .collect()
}

/// A heavy "M" in `style`, laid out on one line: in a span pushed over the
/// paragraph's own style where `pushed`, and in that style itself where not.
fn letter(style: TextStyle, pushed: bool) -> Paragraph {
    let fonts = FontLibrary::new();
    fonts
        .register_font_from_path(FAMILY, OSWALD)
        .expect("Oswald registers");
    let engine = TextEngine::new(&fonts);
    let style = TextStyle {
        font_families: vec![FAMILY.into()],
        font_size: 140.0,
        font_weight: 700,
        ..style
    };
    let mut builder = engine.paragraph_builder(&style);
    match pushed {
        true => {
            builder.push_style(&style);
            builder.add_text("M");
            builder.pop();
        }
        false => {
            builder.add_text("M");
        }
    }
    builder.build(1000.0)
}

type Site = fn(RgbaLinear) -> (TextStyle, f32);

/// Each colour a paragraph's style names, in a style drawn so that the
/// colour alone covers some of the page whole, with how far down the
/// paragraph is drawn.
const SITES: [(&str, Site); 7] = [
    ("color", |c| {
        (
            TextStyle {
                color: c,
                ..TextStyle::default()
            },
            -20.0,
        )
    }),
    // A stroke thick enough to cover some of the page whole, which keeps its
    // style through the paint the page makes of it.
    ("stroke", |c| {
        (
            TextStyle {
                color: c,
                stroke_width: Some(40.0),
                ..TextStyle::default()
            },
            -20.0,
        )
    }),
    // The colour of a paragraph drawn by its passes: an underline, in no
    // colour at all, is what gives it them.
    ("color, drawn by passes", |c| {
        (
            TextStyle {
                color: c,
                decoration: TextDecoration::underline(),
                decoration_color: Some(clear()),
                ..TextStyle::default()
            },
            -20.0,
        )
    }),
    ("foreground_color", |c| {
        let red = RgbaLinear::from_srgb8(255, 0, 0, 1.0);
        (
            TextStyle {
                color: red,
                foreground_color: Some(c),
                ..TextStyle::default()
            },
            -20.0,
        )
    }),
    ("background_color", |c| {
        (
            TextStyle {
                color: clear(),
                background_color: Some(c),
                ..TextStyle::default()
            },
            -20.0,
        )
    }),
    // The letter drawn off the page and its shadow cast onto it.
    ("shadow", |c| {
        let shadow = TextShadow {
            color: c,
            offset_x: 0.0,
            offset_y: 400.0,
            blur_sigma: 0.0,
        };
        (
            TextStyle {
                color: clear(),
                shadows: vec![shadow],
                ..TextStyle::default()
            },
            -420.0,
        )
    }),
    // A thick underline of the letter, which covers the page across.
    ("decoration_color", |c| {
        (
            TextStyle {
                color: clear(),
                decoration: TextDecoration::underline(),
                decoration_color: Some(c),
                decoration_thickness: 30.0,
                ..TextStyle::default()
            },
            -80.0,
        )
    }),
];

/// On every float page, every colour a paragraph's style names lands as a
/// fill of that colour lands, to the bit: the paragraph's paints are made
/// for the page as it is drawn, in a span or in the paragraph's own style.
/// Built with the paragraph instead, before its page was known, they missed
/// by up to 2.2e-4 on F32 and by a whole half-float step on F16.
#[test]
fn a_paragraph_lands_in_its_colours_as_a_fill_does_on_a_float_page() {
    let mut wrong = Vec::new();
    for (space, depth) in FLOAT_PAGES {
        for ((name, site), pushed) in
            SITES.iter().flat_map(|site| [(site, true), (site, false)])
        {
            let name = format!(
                "{name}{}",
                if pushed {
                    ""
                } else {
                    " (the paragraph's own style)"
                }
            );
            for i in 1..=10 {
                let v = i as f32 / 11.0;
                let colour =
                    RgbaLinear::new_premultiplied(v, v * 0.5, 1.0 - v, 1.0);
                let mut fill = page(space, depth);
                fill.context().set_fill_style(colour);
                fill.context().fill_rect(0.0, 0.0, PAGE, PAGE);
                let want = covered(&mut fill, space)[0];
                let (style, y) = site(colour);
                let paragraph = letter(style, pushed);
                let mut drawn = page(space, depth);
                drawn.context().draw_paragraph(&paragraph, -5.0, y);
                let got = covered(&mut drawn, space);
                if got.is_empty() {
                    wrong.push(format!(
                        "{space:?} {depth:?} {name}: nothing covered"
                    ));
                } else if let Some(px) = got.iter().find(|px| **px != want) {
                    wrong.push(format!("{space:?} {depth:?} {name} {v}: {px:?} against {want:?}"));
                }
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "{} missed:\n{}",
        wrong.len(),
        wrong.join("\n")
    );
}
