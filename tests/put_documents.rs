//! A PDF or SVG of a page holding a `put_image_data` shows what the canvas
//! does: the put replaces what lay beneath it, transparent bytes included.
//!
//! A put is recorded as its bytes drawn under `BlendMode::Clear` and then
//! drawn again. Skia's SVG device writes no blend, so a vector SVG left the
//! page beneath a transparent put showing; the PDF device writes the Clear,
//! so a canvas drawn in holding a put erased the page beneath the canvas.

mod common;

use meo_skia_canvas::{
    export::{EncodeOptions, ImageFormat},
    prelude::*,
};

/// The side of every page drawn here, in pixels.
const SIDE: f32 = 24.0;

/// How far a document's rendering may land from the canvas's own pixels:
/// within a level of rounding either way, as `pdf_blend` measures poppler.
const RENDERED: u8 = 2;

/// A CPU canvas of `width` by `height`.
fn canvas(width: f32, height: f32) -> Canvas {
    let mut canvas = Canvas::new(width, height);
    canvas.set_gpu(false);
    canvas
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

/// 6x6 bytes of red at half alpha, unpremultiplied.
fn half_red(ctx: &mut Context2D) -> ImageData {
    let mut data = ctx.create_image_data(6, 6).expect("image data");
    for pixel in data.pixels_mut().chunks_mut(4) {
        pixel.copy_from_slice(&[255, 0, 0, 128]);
    }
    data
}

/// A page of [`SIDE`] filled with opaque blue.
fn blue_page() -> Canvas {
    let mut page = canvas(SIDE, SIDE);
    let ctx = page.context();
    ctx.set_fill_style_css("rgb(0,0,255)").expect("a colour");
    ctx.fill_rect(0.0, 0.0, SIDE, SIDE);
    page
}

/// A [`blue_page`] with transparent bytes put at (4, 4) and half-alpha red
/// put at (12, 12), each 6x6, and the pixels it holds, premultiplied: what
/// its documents should show.
fn put_on_the_page() -> (Canvas, Vec<u8>) {
    let mut page = blue_page();
    let ctx = page.context();
    let blank = ctx.create_image_data(6, 6).expect("image data");
    ctx.put_image_data(&blank, 4.0, 4.0).expect("a put");
    let red = half_red(ctx);
    ctx.put_image_data(&red, 12.0, 12.0).expect("a put");
    let expected = read(&mut page);
    (page, expected)
}

/// A [`blue_page`] with a 12x12 canvas drawn in at (6, 6) holding green,
/// transparent bytes put over its top-left 6x6 and half-alpha red over its
/// bottom-right 6x6; and, as its documents should show it, the source's
/// own pixels drawn there as an image. The reference is not the page's own
/// pixels, so that a page that erases beneath the put cannot agree with a
/// document that does too.
fn put_in_a_canvas_drawn_in() -> (Canvas, Vec<u8>) {
    let mut source = canvas(12.0, 12.0);
    let ctx = source.context();
    ctx.set_fill_style_css("rgb(0,160,0)").expect("a colour");
    ctx.fill_rect(0.0, 0.0, 12.0, 12.0);
    let blank = ctx.create_image_data(6, 6).expect("image data");
    ctx.put_image_data(&blank, 0.0, 0.0).expect("a put");
    let red = half_red(ctx);
    ctx.put_image_data(&red, 6.0, 6.0).expect("a put");
    let premultiplied = PixelExportOptions {
        premultiplied: true,
        ..PixelExportOptions::default()
    };
    let image =
        Image::from_pixels(&read(&mut source), 12, 12, 12 * 4, premultiplied)
            .expect("an image of the source");
    let mut reference = blue_page();
    reference.context().draw_image(&image, 6.0, 6.0);
    let mut page = blue_page();
    page.context().draw_canvas(&mut source, 6.0, 6.0);
    (page, read(&mut reference))
}

/// A page holding a put, and the pixels its documents should show,
/// premultiplied.
type Scene = fn() -> (Canvas, Vec<u8>);

/// The scenes, by name.
const SCENES: [(&str, Scene); 2] = [
    ("a put on the page", put_on_the_page),
    ("a put in a canvas drawn in", put_in_a_canvas_drawn_in),
];

/// `canvas` exported as `format`.
fn export(canvas: &mut Canvas, format: ImageFormat) -> Vec<u8> {
    canvas
        .to_buffer(format, &EncodeOptions::default())
        .expect("a document")
}

/// Premultiplied `pixels` drawn over white, as RGB.
fn over_white(pixels: &[u8]) -> Vec<[u8; 3]> {
    pixels
        .chunks(4)
        .map(|p| {
            let rest = u16::from(u8::MAX - p[3]);
            [0, 1, 2].map(|c| (u16::from(p[c]) + rest).min(255) as u8)
        })
        .collect()
}

/// How many device pixels poppler renders each page pixel as, across.
///
/// Poppler covers an image one device pixel past its right and bottom
/// edges, at 72 dots per inch and at 288 alike, so a render at the page's
/// own resolution stains the page pixel beside every put. Rendered at four
/// times, each page pixel is read at its centre, which that pixel never
/// reaches.
const SCALE: u32 = 4;

/// `pdf` as poppler renders it, over white, read at the centre of each page
/// pixel -- see [`SCALE`] -- or `None` where `pdftoppm` is not installed.
fn poppler(pdf: &[u8], name: &str) -> Option<Vec<[u8; 3]>> {
    let dir = std::env::temp_dir()
        .join(format!("put-documents-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a directory");
    let path = dir.join("page.pdf");
    std::fs::write(&path, pdf).expect("the PDF written");
    let out = dir.join("page");
    let ran = std::process::Command::new("pdftoppm")
        .args(["-r", &(72 * SCALE).to_string(), "-png", "-singlefile"])
        .arg(&path)
        .arg(&out)
        .status();
    let rendered = match ran {
        Ok(status) => {
            assert!(status.success(), "pdftoppm failed on {name}");
            let png =
                std::fs::read(out.with_extension("png")).expect("the render");
            let image = Image::from_encoded(&png).expect("a decodable render");
            let side = SIDE * SCALE as f32;
            let mut back = canvas(side, side);
            back.context().draw_image(&image, 0.0, 0.0);
            let rendered = over_white(&read(&mut back));
            let (side, scale) = (SIDE as usize, SCALE as usize);
            let centre = |page: usize| page * scale + scale / 2;
            Some(
                (0..side * side)
                    .map(|i| {
                        let (x, y) = (centre(i % side), centre(i / side));
                        rendered[y * side * scale + x]
                    })
                    .collect(),
            )
        }
        Err(_) => None,
    };
    std::fs::remove_dir_all(&dir).expect("cleaned up");
    rendered
}

/// The largest difference between `rendered` and `expected`, channel by
/// channel.
fn worst<const N: usize>(rendered: &[[u8; N]], expected: &[[u8; N]]) -> u8 {
    assert_eq!(rendered.len(), expected.len(), "one render a pixel");
    rendered
        .iter()
        .zip(expected)
        .flat_map(|(a, b)| a.iter().zip(b).map(|(a, b)| a.abs_diff(*b)))
        .max()
        .unwrap_or(0)
}

/// Runs wherever `pdftoppm` is installed, and fails rather than skips on
/// CI, which installs it for every job that runs this suite.
#[test]
fn a_pdf_of_a_put_renders_as_the_canvas_in_poppler() {
    let mut wrong = Vec::new();
    for (name, scene) in SCENES {
        let (mut page, expected) = scene();
        let expected = over_white(&expected);
        let pdf = export(&mut page, ImageFormat::Pdf);
        let Some(rendered) = poppler(&pdf, &name.replace(' ', "-")) else {
            assert!(
                std::env::var_os("CI").is_none(),
                "pdftoppm is missing on CI: the workflow installs poppler-utils"
            );
            eprintln!(
                "skipped: pdftoppm not found -- `brew install poppler` or `apt install poppler-utils`"
            );
            return;
        };
        let apart = worst(&rendered, &expected);
        if apart > RENDERED {
            wrong.push(format!("{name}: {apart} levels from the canvas"));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("; "));
}

/// A put is an entry of its own: what is drawn after it is not rasterized
/// with it, and stays a vector in an SVG.
#[test]
fn an_svg_keeps_what_is_drawn_after_a_put_as_vectors() {
    let (mut page, _) = put_on_the_page();
    let ctx = page.context();
    ctx.set_fill_style_css("rgb(0,160,0)").expect("a colour");
    ctx.fill_rect(0.0, 20.0, 4.0, 4.0);
    let svg = export(&mut page, ImageFormat::Svg);
    let svg = String::from_utf8(svg).expect("utf-8");
    assert!(svg.contains("fill=\"#00A000\""), "no vector fill in {svg}");
}

#[test]
fn an_svg_of_a_put_renders_as_the_canvas() {
    let mut wrong = Vec::new();
    for (name, scene) in SCENES {
        let (mut page, expected) = scene();
        let svg = export(&mut page, ImageFormat::Svg);
        let svg = String::from_utf8(svg).expect("utf-8");
        let rendered = common::rendered_svg(&svg, SIDE as u32, SIDE as u32);
        let pixels = |bytes: &[u8]| -> Vec<[u8; 4]> {
            bytes.chunks(4).map(|p| [p[0], p[1], p[2], p[3]]).collect()
        };
        let apart = worst(&pixels(&rendered), &pixels(&expected));
        if apart > RENDERED {
            wrong.push(format!("{name}: {apart} levels from the canvas"));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("; "));
}
