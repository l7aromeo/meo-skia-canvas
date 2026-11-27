//! `Svg::rasterize` composites a document as a browser does, and as the
//! canvas composites the same draws: in sRGB, its colours' own space, rather
//! than in linear light.
//!
//! The reference for each case is the canvas drawing what the document
//! describes, so it shares nothing with the SVG renderer but the colours.

mod common;

use meo_skia_canvas::{
    export::{EncodeOptions, ImageFormat},
    prelude::*,
};

/// The side of every document and canvas here, in pixels.
const SIDE: u32 = 8;

/// Bytes in a pixel of an eight-bit RGBA raster.
const BYTES_PER_PIXEL: usize = 4;

/// Half alpha as a byte, for the image case.
const HALF: u8 = 128;

/// A CPU canvas of [`SIDE`] by [`SIDE`], filled opaque blue.
fn blue() -> Canvas {
    let mut canvas = Canvas::new(SIDE as f32, SIDE as f32);
    canvas.set_gpu(false);
    let ctx = canvas.context();
    ctx.set_fill_style_css("rgb(0,0,255)").expect("a colour");
    ctx.fill_rect(0.0, 0.0, SIDE as f32, SIDE as f32);
    canvas
}

/// `canvas`'s pixels, premultiplied RGBA in sRGB.
fn read(canvas: &mut Canvas) -> Vec<u8> {
    read_in(canvas, PixelColorSpace::Srgb)
}

/// `canvas`'s pixels, premultiplied RGBA in `space`.
fn read_in(canvas: &mut Canvas, space: PixelColorSpace) -> Vec<u8> {
    canvas
        .to_image_data(
            None,
            &ImageDataExport {
                pixels: PixelExportOptions {
                    premultiplied: true,
                    color_space: space,
                    ..PixelExportOptions::default()
                },
                ..ImageDataExport::default()
            },
        )
        .expect("pixels")
        .pixels()
        .to_vec()
}

/// `svg` rasterized at [`SIDE`] by [`SIDE`], as premultiplied RGBA.
fn rasterized(svg: &str) -> Vec<u8> {
    common::rendered_svg(svg, SIDE, SIDE)
}

/// A document of [`SIDE`] by [`SIDE`] holding a blue rectangle under `over`.
fn document(over: &str) -> String {
    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="{SIDE}" height="{SIDE}"><rect width="{SIDE}" height="{SIDE}" fill="blue"/>{over}</svg>"#
    )
}

/// Asserts that `got` and `want` hold the same pixels, naming the first
/// one apart.
fn same(got: &[u8], want: &[u8], what: &str) {
    let apart = got
        .chunks(BYTES_PER_PIXEL)
        .zip(want.chunks(BYTES_PER_PIXEL))
        .position(|(g, w)| g != w);
    if let Some(i) = apart {
        let at = i * BYTES_PER_PIXEL..(i + 1) * BYTES_PER_PIXEL;
        panic!(
            "{what}: pixel {i} rasterized {:?}, the canvas {:?}",
            &got[at.clone()],
            &want[at]
        );
    }
}

/// A shape at an opacity over another composites in sRGB. 0.6 is a whole
/// byte, 153, so the canvas's eight-bit alpha and the document's float one
/// are the same alpha, and the blue left, 102, falls on no rounding tie.
#[test]
fn a_translucent_fill_composites_in_srgb() {
    let mut canvas = blue();
    let ctx = canvas.context();
    ctx.set_fill_style_css("rgba(255,0,0,0.6)")
        .expect("a colour");
    ctx.fill_rect(0.0, 0.0, SIDE as f32, SIDE as f32);
    let svg = document(&format!(
        r#"<rect width="{SIDE}" height="{SIDE}" fill="red" fill-opacity="0.6"/>"#
    ));
    same(&rasterized(&svg), &read(&mut canvas), "fill-opacity 0.6");
}

/// An image whose pixels are translucent composites over what lies beneath
/// it in sRGB.
#[test]
fn a_translucent_image_composites_in_srgb() {
    let mut source = Canvas::new(SIDE as f32, SIDE as f32);
    source.set_gpu(false);
    let ctx = source.context();
    let mut data = ctx.create_image_data(SIDE, SIDE).expect("image data");
    for pixel in data.pixels_mut().chunks_mut(BYTES_PER_PIXEL) {
        pixel.copy_from_slice(&[255, 0, 0, HALF]);
    }
    ctx.put_image_data(&data, 0.0, 0.0).expect("a put");
    let url = source
        .to_data_url(ImageFormat::Png, &EncodeOptions::default())
        .expect("a data URL");
    let png = source
        .to_buffer(ImageFormat::Png, &EncodeOptions::default())
        .expect("a PNG");

    let mut canvas = blue();
    let image = Image::from_encoded(&png).expect("the PNG decodes");
    canvas.context().draw_image(&image, 0.0, 0.0);
    let svg = document(&format!(
        r#"<image width="{SIDE}" height="{SIDE}" xlink:href="{url}"/>"#
    ));
    same(
        &rasterized(&svg),
        &read(&mut canvas),
        "an image at half alpha",
    );
}

/// A Display P3 canvas of [`SIDE`] by [`SIDE`] on the CPU.
fn p3() -> Canvas {
    let mut canvas = Canvas::with_options(
        SIDE as f32,
        SIDE as f32,
        CanvasOptions {
            color_space: PixelColorSpace::DisplayP3,
            gpu: false,
            ..CanvasOptions::default()
        },
    )
    .expect("a Display P3 canvas");
    canvas.set_gpu(false);
    canvas
}

/// An image the document embeds keeps colours outside sRGB: Display P3's
/// red, embedded as a PNG of a P3 canvas, reaches a P3 canvas as the PNG
/// drawn there directly does, rather than clamped to sRGB's red.
#[test]
fn an_embedded_image_keeps_colours_outside_srgb() {
    let mut source = p3();
    let ctx = source.context();
    ctx.set_fill_style_css("color(display-p3 1 0 0)")
        .expect("a colour");
    ctx.fill_rect(0.0, 0.0, SIDE as f32, SIDE as f32);
    let url = source
        .to_data_url(ImageFormat::Png, &EncodeOptions::default())
        .expect("a data URL");
    let png = source
        .to_buffer(ImageFormat::Png, &EncodeOptions::default())
        .expect("a PNG");

    let mut direct = p3();
    let image = Image::from_encoded(&png).expect("the PNG decodes");
    direct.context().draw_image(&image, 0.0, 0.0);
    // Read in P3: read in sRGB, P3's red and sRGB's both clamp to 255, 0, 0.
    let want = read_in(&mut direct, PixelColorSpace::DisplayP3);
    assert_eq!(&want[..3], &[255, 0, 0], "P3's red, drawn directly");

    let svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="{SIDE}" height="{SIDE}"><image width="{SIDE}" height="{SIDE}" xlink:href="{url}"/></svg>"#
    );
    let image = Svg::parse(&svg)
        .expect("the SVG parses")
        .rasterize(SIDE, SIDE)
        .expect("rasterizes");
    let mut through = p3();
    through.context().draw_image(&image, 0.0, 0.0);
    same(
        &read_in(&mut through, PixelColorSpace::DisplayP3),
        &want,
        "P3's red through the document",
    );
}

/// A paint the document names outside sRGB keeps it: Display P3's green as
/// a `fill` reaches a P3 canvas as P3's green, where sRGB's green, the most
/// an eight-bit sRGB surface could hold, reads 117, 251, 76 in P3.
///
/// The red is 1 rather than 0: P3's green is sRGB's red below zero, which
/// the half float stores to within a level, and drawing it back into P3
/// lands a level above zero. Held to those bytes so a change to either the
/// surface or the conversion is seen. Without the SVG wide-colour patch the
/// paint is clipped to sRGB's green before anything is drawn.
#[test]
fn a_wide_paint_reaches_a_wide_canvas() {
    /// P3's green read back in P3 through the document.
    const KEPT: [u8; 3] = [1, 255, 0];
    /// sRGB's green read back in P3.
    const CLIPPED: [u8; 3] = [117, 251, 76];
    let want = if cfg!(meo_svg_wide_colour) {
        KEPT
    } else {
        CLIPPED
    };
    let svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{SIDE}" height="{SIDE}"><rect width="{SIDE}" height="{SIDE}" fill="color(display-p3 0 1 0)"/></svg>"#
    );
    let image = Svg::parse(&svg)
        .expect("the SVG parses")
        .rasterize(SIDE, SIDE)
        .expect("rasterizes");
    let mut canvas = p3();
    canvas.context().draw_image(&image, 0.0, 0.0);
    let read = read_in(&mut canvas, PixelColorSpace::DisplayP3);
    assert_eq!(&read[..3], &want, "P3's green through the document");
}
