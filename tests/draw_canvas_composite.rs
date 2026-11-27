//! A canvas drawn into another at `source-over` and full alpha composites
//! as its own pixels do: the source's draws composite with one another on
//! transparent black, and the result composites onto the destination once.
//!
//! Replayed straight onto the destination instead, a source's
//! `put_image_data` -- recorded as its bytes drawn under `BlendMode::Clear`
//! and then drawn again -- erased the destination beneath it, and its
//! overlapping translucent draws composited one at a time onto the
//! destination's pixels, each rounded to a byte, rather than as the
//! source's bitmap.
//!
//! The reference is the source's own premultiplied bytes, read back and
//! drawn as an image at the same place, which composites them once.

use meo_skia_canvas::prelude::*;

mod common;

/// The destination every case draws on: opaque, with no channel at 0 or 255,
/// where Skia's approximate N32 sprite row happens to land the exact
/// composite and so cannot be told from it.
const PAGE: &str = "rgb(40,120,200)";

/// A canvas on `gpu`'s engine, or `None` where the GPU's is not there --
/// see [`common::on_gpu`].
fn canvas(width: f32, height: f32, gpu: bool) -> Option<Canvas> {
    let mut canvas = Canvas::new(width, height);
    canvas.set_gpu(gpu);
    (!gpu || common::on_gpu(&canvas)).then_some(canvas)
}

/// `canvas`'s pixels, premultiplied.
fn read(canvas: &mut Canvas) -> Vec<u8> {
    canvas
        .to_image_data(
            None,
            &ImageDataExport {
                pixels: PixelExportOptions {
                    premultiplied: true,
                    ..PixelExportOptions::default()
                },
                ..ImageDataExport::default()
            },
        )
        .expect("pixels")
        .pixels()
        .to_vec()
}

/// A destination of `width` by `height` filled with [`PAGE`].
fn page(width: f32, height: f32, gpu: bool) -> Canvas {
    let mut page = canvas(width, height, gpu).expect("the engine");
    let ctx = page.context();
    ctx.set_fill_style_css(PAGE).expect("a colour");
    ctx.fill_rect(0.0, 0.0, width, height);
    page
}

/// How many bytes `source` drawn onto a [`page`] by `draw_canvas` stands
/// apart from its own bytes drawn there as an image, and the largest
/// difference among them.
fn apart(source: &mut Canvas, gpu: bool) -> (usize, u8) {
    let (width, height) = (source.width(), source.height());
    let bytes = read(source);
    let image = Image::from_pixels(
        &bytes,
        width as u32,
        height as u32,
        width as usize * 4,
        PixelExportOptions {
            premultiplied: true,
            ..PixelExportOptions::default()
        },
    )
    .expect("an image of the source");
    let mut drawn = page(width, height, gpu);
    drawn.context().draw_canvas(source, 0.0, 0.0);
    let mut reference = page(width, height, gpu);
    reference.context().draw_image(&image, 0.0, 0.0);
    let (drawn, reference) = (read(&mut drawn), read(&mut reference));
    let differences: Vec<u8> = drawn
        .iter()
        .zip(&reference)
        .filter(|(a, b)| a != b)
        .map(|(a, b)| a.abs_diff(*b))
        .collect();
    (
        differences.len(),
        differences.into_iter().max().unwrap_or(0),
    )
}

/// A source canvas made on the GPU engine where asked, or `None` where that
/// engine is not there.
type Scene = fn(bool) -> Option<Canvas>;

/// A 4x4 canvas holding only transparent bytes, put there.
fn transparent_put(gpu: bool) -> Option<Canvas> {
    let mut held = canvas(4.0, 4.0, gpu)?;
    let ctx = held.context();
    let data = ctx.create_image_data(4, 4).expect("image data");
    ctx.put_image_data(&data, 0.0, 0.0).expect("a put");
    Some(held)
}

/// A 4x4 canvas holding opaque red put in its top half and transparent
/// bytes put in its bottom half.
fn half_red_put(gpu: bool) -> Option<Canvas> {
    let mut red = canvas(4.0, 2.0, gpu)?;
    red.context()
        .set_fill_style_css("rgb(255,0,0)")
        .expect("a colour");
    red.context().fill_rect(0.0, 0.0, 4.0, 2.0);
    let red = red
        .context()
        .get_image_data(0.0, 0.0, 4.0, 2.0)
        .expect("the red");
    let mut held = canvas(4.0, 4.0, gpu)?;
    let ctx = held.context();
    let blank = ctx.create_image_data(4, 2).expect("image data");
    ctx.put_image_data(&red, 0.0, 0.0).expect("a put");
    ctx.put_image_data(&blank, 0.0, 2.0).expect("a put");
    Some(held)
}

/// A 64x64 canvas holding two translucent rectangles that overlap.
fn overlapping_translucent(gpu: bool) -> Option<Canvas> {
    let mut held = canvas(64.0, 64.0, gpu)?;
    let ctx = held.context();
    ctx.set_fill_style_css("rgba(200,80,30,0.6)")
        .expect("a colour");
    ctx.fill_rect(4.0, 4.0, 40.0, 40.0);
    ctx.set_fill_style_css("rgba(20,160,90,0.45)")
        .expect("a colour");
    ctx.fill_rect(20.0, 20.0, 40.0, 40.0);
    Some(held)
}

#[test]
fn a_canvas_drawn_at_source_over_composites_as_its_own_pixels() {
    let scenes: [(&str, Scene); 3] = [
        ("transparent bytes put", transparent_put),
        ("red put over transparent", half_red_put),
        (
            "overlapping translucent rectangles",
            overlapping_translucent,
        ),
    ];
    let mut wrong = Vec::new();
    let mut checked = 0;
    for gpu in common::engines() {
        for (name, scene) in scenes {
            let Some(mut source) = scene(gpu) else {
                continue;
            };
            checked += 1;
            let (bytes, most) = apart(&mut source, gpu);
            if bytes > 0 {
                wrong.push((name, gpu, bytes, most));
            }
        }
    }
    assert!(checked >= scenes.len(), "no CPU canvas");
    assert!(
        wrong.is_empty(),
        "draw_canvas apart from the source's bytes (scene, gpu, bytes, \
         most): {wrong:?}"
    );
}

/// The one case the reference cannot tell apart from a wrong draw on its
/// own: a source whose put leaves the destination showing, checked to
/// leave it showing rather than to match bytes that might both be wrong.
#[test]
fn transparent_bytes_put_leave_the_destination_as_it_was() {
    let mut source = transparent_put(false).expect("a CPU canvas");
    let mut drawn = page(4.0, 4.0, false);
    drawn.context().draw_canvas(&mut source, 0.0, 0.0);
    let pixels = read(&mut drawn);
    let destination = read(&mut page(1.0, 1.0, false));
    assert!(
        pixels.chunks(4).all(|pixel| pixel == destination),
        "the destination read {:?}",
        &pixels[..4]
    );
}

/// A 80x60 canvas holding an ellipse turned off the axes and two
/// translucent rectangles that overlap it: curves, whose edges Skia covers by
/// where they lie on the raster it fills, and draws that composite with one
/// another.
fn curves(gpu: bool) -> Option<Canvas> {
    let mut held = canvas(80.0, 60.0, gpu)?;
    let ctx = held.context();
    ctx.set_fill_style_css("rgba(0,0,0,0.8)").expect("a colour");
    ctx.begin_path();
    ctx.ellipse(
        40.0,
        30.0,
        35.0,
        22.5,
        0.4,
        0.0,
        std::f32::consts::TAU,
        false,
    )
    .expect("an ellipse");
    ctx.fill(FillRule::NonZero);
    ctx.set_fill_style_css("rgba(200,80,30,0.6)")
        .expect("a colour");
    ctx.fill_rect(4.0, 4.0, 30.0, 30.0);
    ctx.set_fill_style_css("rgba(20,160,90,0.45)")
        .expect("a colour");
    ctx.fill_rect(20.0, 20.0, 40.0, 30.0);
    Some(held)
}

/// A clip, or none, set on the destination before the source is drawn in.
type Clip = fn(&mut Context2D);

/// The circle a clip cuts the source by: its centre and its radius, off
/// whole pixels, so that its edge covers many pixels in part.
const CIRCLE: (f32, f32, f32) = (80.0, 50.0, 25.3);

#[test]
fn a_canvas_drawn_in_at_a_whole_pixel_lands_its_own_pixels() {
    // Wherever the clip or the page's edge cuts it, as `drawImage` lands a
    // canvas's bitmap. Drawn straight into a layer the clip and the page's
    // edge bound, the CPU covered the source's curves otherwise.
    let none: Clip = |_| {};
    let rect: Clip = |ctx| {
        ctx.begin_path();
        ctx.rect(70.0, 40.0, 100.0, 100.0);
        ctx.clip(FillRule::NonZero);
    };
    let fractional: Clip = |ctx| {
        ctx.begin_path();
        ctx.rect(70.4, 40.6, 100.0, 100.0);
        ctx.clip(FillRule::NonZero);
    };
    let circle: Clip = |ctx| {
        ctx.begin_path();
        ctx.arc(
            CIRCLE.0,
            CIRCLE.1,
            CIRCLE.2,
            0.0,
            std::f32::consts::TAU,
            false,
        )
        .expect("an arc");
        ctx.clip(FillRule::NonZero);
    };
    // Each case: its name, where the source's corner lands, the part of the
    // source drawn where it is cropped, and the clip.
    type Case = (&'static str, (f32, f32), Option<Rect>, Clip);
    let crop = Rect::from_xywh(10.0, 8.0, 50.0, 40.0);
    let cases: [Case; 8] = [
        ("in the page", (40.0, 20.0), None, none),
        ("cut by a rectangle", (40.0, 20.0), None, rect),
        (
            "cut by a fractional rectangle",
            (40.0, 20.0),
            None,
            fractional,
        ),
        ("cut by a circle", (40.0, 20.0), None, circle),
        ("cut by the top left edge", (-13.0, -7.0), None, none),
        ("cut by the bottom right edge", (150.0, 100.0), None, none),
        ("cropped", (40.0, 20.0), Some(crop), none),
        (
            "cropped and cut by a circle",
            (40.0, 20.0),
            Some(crop),
            circle,
        ),
    ];
    let (mut wrong, mut on_the_edge) = (Vec::new(), 0);
    let mut checked = 0;
    for gpu in common::engines() {
        for (name, (x, y), crop, clip) in cases {
            let Some(mut source) = curves(gpu) else {
                continue;
            };
            checked += 1;
            let bytes = read(&mut source);
            let image = Image::from_pixels(
                &bytes,
                80,
                60,
                80 * 4,
                PixelExportOptions {
                    premultiplied: true,
                    ..PixelExportOptions::default()
                },
            )
            .expect("an image of the source");
            let mut drawn = page(200.0, 140.0, gpu);
            clip(drawn.context());
            let mut reference = page(200.0, 140.0, gpu);
            clip(reference.context());
            match crop {
                None => {
                    drawn.context().draw_canvas(&mut source, x, y);
                    reference.context().draw_image(&image, x, y);
                }
                Some(part) => {
                    let (left, top, width, height) =
                        (part.left, part.top, part.width(), part.height());
                    let (to_x, to_y) = (x + left, y + top);
                    drawn.context().draw_canvas_region(
                        &mut source,
                        left,
                        top,
                        width,
                        height,
                        to_x,
                        to_y,
                        width,
                        height,
                    );
                    reference.context().draw_image_region(
                        &image, left, top, width, height, to_x, to_y, width,
                        height,
                    );
                }
            }
            let (drawn, reference) = (read(&mut drawn), read(&mut reference));
            // A GPU canvas composites the canvas drawn in through Ganesh's
            // own blend and an image of its bytes in the CPU's arithmetic
            // (#837); the two agree wherever the clip covers a pixel whole,
            // and may part by a level only where the circle's anti-aliased
            // edge covers one in part.
            let edge = |i: usize| {
                let (x, y) = ((i / 4) % 200, (i / 4) / 200);
                let (dx, dy) =
                    (x as f32 + 0.5 - CIRCLE.0, y as f32 + 0.5 - CIRCLE.1);
                ((dx * dx + dy * dy).sqrt() - CIRCLE.2).abs() < 1.0
            };
            let allowed = |i: usize, a: u8, b: u8| {
                gpu && name.ends_with("by a circle")
                    && a.abs_diff(b) <= 1
                    && edge(i)
            };
            let apart = drawn
                .iter()
                .zip(&reference)
                .enumerate()
                .filter(|(_, (a, b))| a != b);
            on_the_edge += apart
                .clone()
                .filter(|(i, (a, b))| allowed(*i, **a, **b))
                .count();
            let off = apart.filter(|(i, (a, b))| !allowed(*i, **a, **b));
            let most = off.clone().map(|(_, (a, b))| a.abs_diff(*b)).max();
            if let Some(most) = most {
                wrong.push((name, gpu, off.count(), most));
            }
        }
    }
    assert!(checked >= cases.len(), "no CPU canvas");
    eprintln!("bytes a level apart along the circle's edge: {on_the_edge}");
    assert!(
        wrong.is_empty(),
        "draw_canvas apart from the source's bytes drawn as an image (case, \
         gpu, bytes, most): {wrong:?}"
    );
}

/// An SVG has no layers, so the group a canvas drawn in at `source-over`
/// composites through is written as its entries: each of the source's
/// fills, at its own opacity.
#[test]
fn an_svg_writes_what_a_canvas_drawn_in_holds() {
    use meo_skia_canvas::export::{EncodeOptions, ImageFormat};
    let mut source = overlapping_translucent(false).expect("a CPU canvas");
    let mut drawn = page(128.0, 128.0, false);
    drawn.context().draw_canvas(&mut source, 2.5, 2.5);
    let svg = drawn
        .prepare_export(ImageFormat::Svg, &EncodeOptions::default())
        .and_then(|prepared| prepared.encode())
        .expect("an SVG");
    let svg = String::from_utf8_lossy(&svg);
    for fill in ["fill=\"#C8501E\"", "fill=\"#14A05A\""] {
        assert!(svg.contains(fill), "no {fill} in {svg}");
    }
}
