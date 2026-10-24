//! A read unpremultiplies the same way on either engine, and every time.
//!
//! A canvas holds premultiplied pixels and `getImageData` hands back
//! unpremultiplied ones, so a read divides by alpha and rounds. The raster
//! engine rounds to nearest, as Chrome 153 does. Skia's GPU readback rounded
//! down, so on that engine the first read of a state came back a level low
//! wherever the quotient fell on a half -- 127 for 128 -- and the second,
//! served from a raster copy, did not. Each row was measured in Chrome 153
//! on a Display P3 canvas read back in P3.

use meo_skia_canvas::prelude::*;

const ROWS: &[(&str, [u8; 4])] = &[
    ("color(display-p3 0.5 0.2 0.1 / 0.1)", [128, 49, 29, 26]),
    ("color(display-p3 0.3 0.6 0.9 / 0.1)", [78, 147, 226, 26]),
    ("color(display-p3 0.5 0.2 0.1 / 0.2)", [125, 50, 25, 51]),
    ("color(display-p3 0.3 0.6 0.9 / 0.2)", [75, 155, 230, 51]),
    ("color(display-p3 0.5 0.2 0.1 / 0.25)", [128, 52, 24, 64]),
    ("color(display-p3 0.3 0.6 0.9 / 0.25)", [76, 151, 227, 64]),
    ("color(display-p3 0.5 0.2 0.1 / 0.3)", [126, 50, 26, 77]),
    ("color(display-p3 0.3 0.6 0.9 / 0.3)", [76, 152, 229, 77]),
    ("color(display-p3 0.5 0.2 0.1 / 0.4)", [128, 50, 25, 102]),
    ("color(display-p3 0.3 0.6 0.9 / 0.4)", [77, 152, 230, 102]),
    ("color(display-p3 0.5 0.2 0.1 / 0.5)", [128, 52, 26, 128]),
    ("color(display-p3 0.3 0.6 0.9 / 0.5)", [76, 153, 229, 128]),
    ("color(display-p3 0.5 0.2 0.1 / 0.6)", [127, 52, 25, 153]),
    ("color(display-p3 0.3 0.6 0.9 / 0.6)", [77, 153, 230, 153]),
    ("color(display-p3 0.5 0.2 0.1 / 0.7)", [127, 51, 26, 179]),
    ("color(display-p3 0.3 0.6 0.9 / 0.7)", [77, 152, 229, 179]),
    ("color(display-p3 0.5 0.2 0.1 / 0.75)", [128, 51, 25, 191]),
    ("color(display-p3 0.3 0.6 0.9 / 0.75)", [76, 154, 230, 191]),
    ("color(display-p3 0.5 0.2 0.1 / 0.8)", [128, 51, 25, 204]),
    ("color(display-p3 0.3 0.6 0.9 / 0.8)", [76, 152, 230, 204]),
    ("color(display-p3 0.5 0.2 0.1 / 0.9)", [128, 51, 26, 230]),
    ("color(display-p3 0.3 0.6 0.9 / 0.9)", [76, 153, 230, 230]),
];

fn read_twice(css: &str, gpu: bool, side: f32) -> Result<[[u8; 4]; 2], Error> {
    let mut canvas = Canvas::with_options(
        side,
        side,
        CanvasOptions {
            color_space: PixelColorSpace::DisplayP3,
            gpu,
            ..CanvasOptions::default()
        },
    )?;
    let ctx = canvas.context();
    ctx.set_fill_style_css(css)?;
    ctx.fill_rect(0.0, 0.0, side, side);
    let options = PixelExportOptions {
        color_space: PixelColorSpace::DisplayP3,
        ..PixelExportOptions::default()
    };
    let mut read = || -> Result<[u8; 4], Error> {
        let data = ctx.get_image_data_as(0.0, 0.0, 1.0, 1.0, options)?;
        let p = data.pixels();
        Ok([p[0], p[1], p[2], p[3]])
    };
    Ok([read()?, read()?])
}

#[test]
fn either_engine_unpremultiplies_as_chrome_does() -> Result<(), Error> {
    // Four pixels square is served by a tile; 600 by the page.
    for gpu in [false, true] {
        for side in [4.0, 600.0] {
            for &(css, chrome) in ROWS {
                assert_eq!(
                    read_twice(css, gpu, side)?,
                    [chrome; 2],
                    "{css}, gpu {gpu}, {side} pixels",
                );
            }
        }
    }
    Ok(())
}
