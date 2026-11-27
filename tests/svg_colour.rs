//! An SVG document's paint is the colour it names, in every CSS Color 4 form
//! a canvas `fillStyle` reads, as Chrome draws it (#815).
//!
//! Skia's SVG module parses paints itself and knows hex, named colours,
//! `rgb()` and `rgba()` alone; any other form fell back to the initial
//! black. Each case here is drawn twice on an 8-bit sRGB canvas: as an SVG
//! document whose paint is the form, and as a `fillRect` whose `fillStyle`
//! is the same text. The two must agree, the colour clipped to sRGB as the
//! canvas clips it.

use meo_skia_canvas::prelude::*;

/// The forms read, each a colour the canvas draws: modern and legacy
/// syntax, every colour space `color()` names, and a wide colour outside
/// sRGB.
const FORMS: [&str; 25] = [
    "#0c0",
    "Lime",
    "lightgray",
    "grey",
    "#00cc00",
    "#00cc0080",
    "#0c08",
    "rebeccapurple",
    "transparent",
    "rgb(0, 204, 0)",
    "rgb(0 204 0)",
    "rgb(0 204 0 / 50%)",
    "rgba(0, 204, 0, 0.5)",
    "hsl(120 100% 40%)",
    "hsl(120deg 100% 40% / 0.25)",
    "hwb(120 0% 20%)",
    "lab(70 -60 50)",
    "lch(70 70 140)",
    "oklab(0.7 -0.15 0.12)",
    "oklch(0.7 0.18 140)",
    "color(srgb 0 0.8 0)",
    "color(srgb-linear 0 0.5 0)",
    "color(display-p3 0 1 0)",
    "color(rec2020 0.2 0.7 0.1)",
    "color(display-p3 1 0.5 0 / 0.5)",
];

/// The pixel at (2, 2) of a 4x4 canvas filled with `css`.
fn filled(css: &str) -> [u8; 4] {
    let mut canvas = Canvas::new(4.0, 4.0);
    canvas.set_gpu(false);
    let ctx = canvas.context();
    ctx.set_fill_style_css(css)
        .expect("a colour the canvas reads");
    ctx.fill_rect(0.0, 0.0, 4.0, 4.0);
    pixel(ctx)
}

/// The pixel at (2, 2) of a 4x4 canvas onto which `svg` is drawn.
fn drawn(svg: &str) -> [u8; 4] {
    let image = Svg::parse(svg)
        .expect("parses")
        .rasterize(4, 4)
        .expect("rasterizes");
    let mut canvas = Canvas::new(4.0, 4.0);
    canvas.set_gpu(false);
    let ctx = canvas.context();
    ctx.draw_image(&image, 0.0, 0.0);
    pixel(ctx)
}

fn pixel(ctx: &mut Context2D) -> [u8; 4] {
    let data = ctx.get_image_data(2.0, 2.0, 1.0, 1.0).expect("read back");
    let px = data.pixels();
    [px[0], px[1], px[2], px[3]]
}

/// A 4x4 document whose square is painted by `paint`, wrapped in whatever
/// the property needs.
fn document(property: &str, value: &str) -> String {
    let body = match property {
        "fill" => format!(r#"<rect width="4" height="4" fill="{value}"/>"#),
        "style fill" => {
            format!(r#"<rect width="4" height="4" style="fill: {value}"/>"#)
        }
        // `fill="none"`, because the Skia of a build without
        // containers/skia-svg-line-fill.patch -- the stock crate-consumer leg
        // runs this file against crates.io's -- fills a `<line>` with the
        // initial black, which SVG says a line never is, and a translucent
        // stroke over it reads that black through (#825).
        "stroke" => format!(
            r#"<line x1="0" y1="2" x2="4" y2="2" fill="none" stroke-width="4" stroke="{value}"/>"#
        ),
        "currentcolor" => format!(
            r#"<g color="{value}"><rect width="4" height="4" fill="currentcolor"/></g>"#
        ),
        "stop-color" => format!(
            r#"<defs><linearGradient id="g"><stop offset="0" stop-color="{value}"/><stop offset="1" stop-color="{value}"/></linearGradient></defs><rect width="4" height="4" fill="url(#g)"/>"#
        ),
        _ => unreachable!("{property}"),
    };
    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="4">{body}</svg>"#
    )
}

/// How far two pixels may stand apart: none where the colour is opaque, a
/// level where it is translucent, which the document's premultiplied 8-bit
/// raster rounds once more than the fill does.
fn allowed(expected: [u8; 4]) -> u8 {
    u8::from(expected[3] != 255)
}

#[test]
fn an_svg_paint_is_the_colour_it_names() {
    let off: Vec<String> =
        ["fill", "style fill", "stroke", "currentcolor", "stop-color"]
            .iter()
            .flat_map(|property| {
                FORMS.iter().filter_map(move |form| {
                    let expected = filled(form);
                    let got = drawn(&document(property, form));
                    let far = expected
                        .iter()
                        .zip(got)
                        .map(|(a, b)| a.abs_diff(b))
                        .max()
                        .unwrap_or(0);
                    (far > allowed(expected)).then(|| {
                format!("{property} {form}: {got:?}, the canvas {expected:?}")
            })
                })
            })
            .collect();
    assert!(off.is_empty(), "drawn off the canvas's colour: {off:#?}");
}

/// The control: a paint Skia already reads is drawn as the canvas draws
/// it, so the comparison above passes where nothing is wrong.
#[test]
fn a_paint_skia_reads_matches_the_canvas() {
    for form in ["#00cc00", "rgb(0, 204, 0)", "rgba(0, 204, 0, 0.5)"] {
        let expected = filled(form);
        let got = drawn(&document("fill", form));
        assert!(
            expected
                .iter()
                .zip(got)
                .all(|(a, b)| a.abs_diff(b) <= allowed(expected)),
            "{form}: {got:?}, the canvas {expected:?}"
        );
    }
}
