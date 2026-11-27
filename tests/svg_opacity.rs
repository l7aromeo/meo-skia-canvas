//! A document's opacities land through `Svg::rasterize` as Chrome lands
//! them, each paint's alpha on a byte.
//!
//! Chrome paints an SVG with an eight-bit alpha: the product of a paint's
//! colour alpha, its `fill-opacity` or `stroke-opacity`, and any `opacity`
//! folded into that one draw -- a leaf's own, and a group's over one shape --
//! rounded once. `rasterize` draws into half floats, which keep the product
//! unrounded, so a tie such as black at 0.5 over red, exactly 127.5, landed
//! 128 where Chrome lands 127. `containers/skia-svg-byte-paint-alpha.patch`
//! rounds the product for rasterize's playback (`BytePaintAlpha`).
//!
//! The reference is Chrome 159's headless shell: each document drawn as an
//! `<img>` onto a 2D canvas, black at an alpha over an opaque red ground,
//! read at one pixel. Two rows differ from it, and say why:
//!
//! - A group over two overlapping shapes is a real layer. Chrome composites it
//!   with the backdrop's share as `d * (255 - A) / 256`, the legacy blitter's
//!   divide, so red and blue sum to 254 where they sum to 255; this lands the
//!   exact composite instead.
//! - `stop-opacity` stays in float in Chrome as here. At 0.3, 178.5 is an exact
//!   tie: Chrome's eight-bit surface rounds it half to even, 178, and the half
//!   float stores 0.7 as 0.70020 and reads 179. The difference is the
//!   representation, not a rounding rule.
//!
//! Without the patch, as on crates.io's Skia, every row is held to what the
//! half floats land instead, so the day Skia rounds, this says so. Built
//! without `containers/skia-svg-wide-colour.patch` too, Skia holds a colour,
//! and so an `rgba()` fill's and a flood's alpha, as an eight-bit `SkColor`,
//! so those rows land on a byte, as Chrome's, whatever the surface keeps.

use meo_skia_canvas::prelude::*;

/// Whether Skia rounds a paint's alpha for rasterize's playback here.
const ROUNDS: bool = cfg!(meo_svg_byte_paint_alpha);

/// Whether Skia keeps an SVG colour's alpha in float, rather than as the
/// byte of an `SkColor`.
const FLOAT_COLOUR: bool = cfg!(meo_svg_wide_colour);

/// Each side of every document and canvas, in pixels.
const SIDE: u32 = 4;

/// The pixel read: inside every shape, away from every edge.
const AT: usize = 1;

/// How black is laid at an alpha over the red ground.
#[derive(Clone, Copy, Debug)]
enum Way {
    /// `opacity` on the shape itself.
    Opacity,
    /// `fill-opacity`.
    FillOpacity,
    /// `stroke-opacity`, on a stroke covering the canvas.
    StrokeOpacity,
    /// `stop-opacity` on both stops of a gradient.
    StopOpacity,
    /// `flood-opacity` on a filter's `<feFlood>`.
    FloodOpacity,
    /// The alpha of an `rgba()` fill.
    ColourAlpha,
    /// `fill-opacity` on a gradient fill.
    GradientFillOpacity,
    /// `opacity` on a group over one opaque shape.
    GroupOverOneShape,
    /// `opacity` on a group over a black shape covered by a blue one.
    GroupOverTwoShapes,
    /// `opacity` on a group over a shape at `fill-opacity` 0.5.
    GroupOverHalfFill,
}

/// The document laying black over red `way` at `alpha`.
fn document(way: Way, alpha: f32) -> String {
    let rect = |attrs: &str| format!(r#"<rect width="4" height="4" {attrs}/>"#);
    let (defs, body) = match way {
        Way::Opacity => (String::new(), rect(&format!(r#"fill="black" opacity="{alpha}""#))),
        Way::FillOpacity => (String::new(), rect(&format!(r#"fill="black" fill-opacity="{alpha}""#))),
        Way::StrokeOpacity => (
            String::new(),
            format!(
                r#"<rect x="-2" y="-2" width="8" height="8" fill="none" stroke="black" stroke-width="12" stroke-opacity="{alpha}"/>"#
            ),
        ),
        Way::StopOpacity => (
            format!(
                r#"<linearGradient id="g"><stop offset="0" stop-color="black" stop-opacity="{alpha}"/><stop offset="1" stop-color="black" stop-opacity="{alpha}"/></linearGradient>"#
            ),
            rect(r#"fill="url(#g)""#),
        ),
        Way::FloodOpacity => (
            format!(
                r#"<filter id="f" x="0" y="0" width="1" height="1"><feFlood flood-color="black" flood-opacity="{alpha}"/></filter>"#
            ),
            rect(r#"fill="black" filter="url(#f)""#),
        ),
        Way::ColourAlpha => (String::new(), rect(&format!(r#"fill="rgba(0,0,0,{alpha})""#))),
        Way::GradientFillOpacity => (
            r#"<linearGradient id="h"><stop offset="0" stop-color="black"/><stop offset="1" stop-color="black"/></linearGradient>"#.to_owned(),
            rect(&format!(r#"fill="url(#h)" fill-opacity="{alpha}""#)),
        ),
        Way::GroupOverOneShape => (
            String::new(),
            format!(r#"<g opacity="{alpha}">{}</g>"#, rect(r#"fill="black""#)),
        ),
        Way::GroupOverTwoShapes => (
            String::new(),
            format!(
                r#"<g opacity="{alpha}">{}{}</g>"#,
                rect(r#"fill="black""#),
                rect(r#"fill="rgb(0,0,255)""#)
            ),
        ),
        Way::GroupOverHalfFill => (
            String::new(),
            format!(
                r#"<g opacity="{alpha}">{}</g>"#,
                rect(r#"fill="black" fill-opacity="0.5""#)
            ),
        ),
    };
    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="4"><defs>{defs}</defs>{}{body}</svg>"#,
        rect(r#"fill="rgb(255,0,0)""#)
    )
}

/// The red, green and blue `rasterize` lands at [`AT`] for `xml`, drawn onto
/// an eight-bit sRGB CPU canvas.
fn landed(xml: &str) -> Rgb {
    let image = Svg::parse(xml)
        .expect("the document parses")
        .rasterize(SIDE, SIDE)
        .expect("rasterizes");
    let mut canvas = Canvas::new(SIDE as f32, SIDE as f32);
    canvas.set_gpu(false);
    canvas.context().draw_image(&image, 0.0, 0.0);
    let data = canvas
        .to_image_data(None, &ImageDataExport::default())
        .expect("pixels");
    let at = (AT * SIDE as usize + AT) * 4;
    let px = &data.pixels()[at..at + 3];
    [px[0], px[1], px[2]]
}

/// Why a row lands apart from Chrome with the patch: see the module's
/// documentation.
const OVERLAP: &str = "a real layer: Chrome divides the backdrop by 256, this lands the exact composite";
const STOP_TIE: &str =
    "178.5 is an exact tie, and a half float stores 0.7 as 0.70020";

/// A pixel's red, green and blue.
type Rgb = [u8; 3];

/// Where the patch lands a row apart from Chrome: what it lands, and why.
type Apart = Option<(Rgb, &'static str)>;

/// Each row: how, at what alpha, what Chrome 159 lands, what the half floats
/// land without the patch, and where the patch lands apart from Chrome.
#[rustfmt::skip]
const ROWS: [(Way, f32, Rgb, Rgb, Apart); 40] = {
    use Way::*;
    [
        (Opacity, 0.5, [127, 0, 0], [128, 0, 0], None),
        (FillOpacity, 0.5, [127, 0, 0], [128, 0, 0], None),
        (StrokeOpacity, 0.5, [127, 0, 0], [128, 0, 0], None),
        (StopOpacity, 0.5, [128, 0, 0], [128, 0, 0], None),
        (FloodOpacity, 0.5, [127, 0, 0], [128, 0, 0], None),
        (ColourAlpha, 0.5, [127, 0, 0], [128, 0, 0], None),
        (GradientFillOpacity, 0.5, [127, 0, 0], [128, 0, 0], None),
        (GroupOverOneShape, 0.5, [127, 0, 0], [128, 0, 0], None),
        (GroupOverTwoShapes, 0.5, [126, 0, 128], [128, 0, 128], Some(([128, 0, 128], OVERLAP))),
        (GroupOverHalfFill, 0.5, [191, 0, 0], [191, 0, 0], None),
        (Opacity, 0.3, [178, 0, 0], [179, 0, 0], None),
        (FillOpacity, 0.3, [178, 0, 0], [179, 0, 0], None),
        (StrokeOpacity, 0.3, [178, 0, 0], [179, 0, 0], None),
        (StopOpacity, 0.3, [178, 0, 0], [179, 0, 0], Some(([179, 0, 0], STOP_TIE))),
        (FloodOpacity, 0.3, [178, 0, 0], [179, 0, 0], None),
        (ColourAlpha, 0.3, [178, 0, 0], [179, 0, 0], None),
        (GradientFillOpacity, 0.3, [178, 0, 0], [179, 0, 0], None),
        (GroupOverOneShape, 0.3, [178, 0, 0], [179, 0, 0], None),
        (GroupOverTwoShapes, 0.3, [177, 0, 77], [179, 0, 77], Some(([179, 0, 77], OVERLAP))),
        (GroupOverHalfFill, 0.3, [217, 0, 0], [217, 0, 0], None),
        (Opacity, 0.6, [102, 0, 0], [102, 0, 0], None),
        (FillOpacity, 0.6, [102, 0, 0], [102, 0, 0], None),
        (StrokeOpacity, 0.6, [102, 0, 0], [102, 0, 0], None),
        (StopOpacity, 0.6, [102, 0, 0], [102, 0, 0], None),
        (FloodOpacity, 0.6, [102, 0, 0], [102, 0, 0], None),
        (ColourAlpha, 0.6, [102, 0, 0], [102, 0, 0], None),
        (GradientFillOpacity, 0.6, [102, 0, 0], [102, 0, 0], None),
        (GroupOverOneShape, 0.6, [102, 0, 0], [102, 0, 0], None),
        (GroupOverTwoShapes, 0.6, [101, 0, 153], [102, 0, 153], Some(([102, 0, 153], OVERLAP))),
        (GroupOverHalfFill, 0.6, [178, 0, 0], [179, 0, 0], None),
        (Opacity, 0.2, [204, 0, 0], [204, 0, 0], None),
        (FillOpacity, 0.2, [204, 0, 0], [204, 0, 0], None),
        (StrokeOpacity, 0.2, [204, 0, 0], [204, 0, 0], None),
        (StopOpacity, 0.2, [204, 0, 0], [204, 0, 0], None),
        (FloodOpacity, 0.2, [204, 0, 0], [204, 0, 0], None),
        (ColourAlpha, 0.2, [204, 0, 0], [204, 0, 0], None),
        (GradientFillOpacity, 0.2, [204, 0, 0], [204, 0, 0], None),
        (GroupOverOneShape, 0.2, [204, 0, 0], [204, 0, 0], None),
        (GroupOverTwoShapes, 0.2, [203, 0, 51], [204, 0, 51], Some(([204, 0, 51], OVERLAP))),
        (GroupOverHalfFill, 0.2, [229, 0, 0], [229, 0, 0], None),
    ]
};

#[test]
fn a_document_rounds_its_opacities_as_chrome_does() {
    let wrong: Vec<String> = ROWS
        .iter()
        .filter_map(|&(way, alpha, chrome, unrounded, apart)| {
            let colour_byte = matches!(way, Way::ColourAlpha | Way::FloodOpacity);
            let want = match (ROUNDS, apart) {
                (false, _) if colour_byte && !FLOAT_COLOUR => chrome,
                (false, _) => unrounded,
                (true, Some((ours, _))) => ours,
                (true, None) => chrome,
            };
            let got = landed(&document(way, alpha));
            (got != want).then(|| {
                format!("{way:?} at {alpha}: {got:?}, expected {want:?} (Chrome {chrome:?})")
            })
        })
        .collect();
    assert!(
        wrong.is_empty(),
        "{} rows apart:\n{}",
        wrong.len(),
        wrong.join("\n")
    );
}
