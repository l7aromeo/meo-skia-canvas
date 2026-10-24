//! An `ImageData` drawn as an image, as `drawImage` and `createPattern` take
//! one in JavaScript.
//!
//! The expected pixels were measured on the Node binding, which accepts an
//! `ImageData` in both: the same 2x2 buffer, the same calls, read back
//! unpremultiplied at eight bits.

use meo_skia_canvas::prelude::*;

const SIDE: f32 = 6.0;

/// Red at half alpha, opaque blue, green at quarter alpha, opaque white.
fn quad(premultiplied: bool) -> ImageData {
    let pixels = match premultiplied {
        false => vec![
            255, 0, 0, 128, 0, 0, 255, 255, 0, 255, 0, 64, 255, 255, 255, 255,
        ],
        // The same four colours with each channel scaled by its alpha.
        true => vec![
            128, 0, 0, 128, 0, 0, 255, 255, 0, 64, 0, 64, 255, 255, 255, 255,
        ],
    };
    let options = PixelExportOptions {
        premultiplied,
        ..PixelExportOptions::default()
    };
    ImageData::from_pixels(2, 2, options, pixels).expect("a 2x2 buffer")
}

/// The page after `draw`, as `x,y -> "r,g,b,a"`.
fn drawn(draw: impl FnOnce(&mut Context2D)) -> impl Fn(u32, u32) -> String {
    let mut canvas = Canvas::new(SIDE, SIDE);
    let ctx = canvas.context();
    draw(ctx);
    let data = ctx
        .get_image_data(0.0, 0.0, SIDE, SIDE)
        .expect("a readback");
    let pixels = data.pixels().to_vec();
    move |x, y| {
        let at = ((y * SIDE as u32 + x) * 4) as usize;
        format!(
            "{},{},{},{}",
            pixels[at],
            pixels[at + 1],
            pixels[at + 2],
            pixels[at + 3]
        )
    }
}

fn image(data: &ImageData) -> Image {
    Image::try_from(data).expect("an ImageData converts")
}

#[test]
fn an_image_data_draws_at_its_natural_size() {
    let px = drawn(|ctx| ctx.draw_image(&image(&quad(false)), 1.0, 1.0));
    assert_eq!(
        [px(1, 1), px(2, 1), px(1, 2), px(2, 2), px(0, 0)],
        [
            "255,0,0,128",
            "0,0,255,255",
            "0,255,0,64",
            "255,255,255,255",
            "0,0,0,0"
        ]
    );
}

#[test]
fn a_drawn_image_data_takes_the_global_alpha() {
    // Unlike `put_image_data`, which writes the bytes as they are.
    let px = drawn(|ctx| {
        ctx.set_global_alpha(0.5);
        ctx.draw_image(&image(&quad(false)), 1.0, 1.0);
    });
    assert_eq!(
        [px(1, 1), px(2, 1), px(1, 2)],
        ["255,0,0,64", "0,0,255,128", "0,255,0,32"]
    );
}

#[test]
fn a_drawn_image_data_takes_the_transform() {
    let px = drawn(|ctx| {
        ctx.set_image_smoothing_enabled(false);
        ctx.scale(2.0, 2.0);
        ctx.draw_image(&image(&quad(false)), 1.0, 1.0);
    });
    assert_eq!(
        [px(2, 2), px(3, 3), px(4, 2), px(2, 4), px(1, 1)],
        [
            "255,0,0,128",
            "255,0,0,128",
            "0,0,255,255",
            "0,255,0,64",
            "0,0,0,0"
        ]
    );
}

#[test]
fn an_image_data_draws_scaled_into_a_rectangle() {
    let px = drawn(|ctx| {
        ctx.set_image_smoothing_enabled(false);
        ctx.draw_image_sized(&image(&quad(false)), 0.0, 0.0, 4.0, 4.0);
    });
    assert_eq!(
        [px(0, 0), px(1, 1), px(2, 0), px(3, 3)],
        [
            "255,0,0,128",
            "255,0,0,128",
            "0,0,255,255",
            "255,255,255,255"
        ]
    );
}

#[test]
fn an_image_data_tiles_as_a_pattern() {
    let px = drawn(|ctx| {
        let pattern =
            ctx.create_pattern(&image(&quad(false)), PatternRepeat::Repeat);
        ctx.set_fill_pattern(&pattern);
        ctx.fill_rect(0.0, 0.0, SIDE, SIDE);
    });
    assert_eq!(
        [px(0, 0), px(1, 0), px(2, 0), px(4, 5), px(5, 5)],
        [
            "255,0,0,128",
            "0,0,255,255",
            "255,0,0,128",
            "0,255,0,64",
            "255,255,255,255"
        ]
    );
}

#[test]
fn a_premultiplied_image_data_draws_the_colours_it_holds() {
    // The same four colours stored premultiplied, which an `ImageData` read
    // back with `premultiplied: true` carries. Treated as unpremultiplied,
    // the red would draw at 128 rather than 255.
    let px = drawn(|ctx| ctx.draw_image(&image(&quad(true)), 1.0, 1.0));
    assert_eq!(
        [px(1, 1), px(2, 1), px(1, 2), px(2, 2)],
        [
            "255,0,0,128",
            "0,0,255,255",
            "0,255,0,64",
            "255,255,255,255"
        ]
    );
}

#[test]
fn a_float_image_data_converts_too() {
    let floats: Vec<u8> = [1.0f32, 0.0, 0.0, 0.5]
        .iter()
        .flat_map(|channel| channel.to_ne_bytes())
        .collect();
    let options = PixelExportOptions {
        depth: PixelDepth::F32,
        ..PixelExportOptions::default()
    };
    let data =
        ImageData::from_pixels(1, 1, options, floats).expect("one pixel");
    let px = drawn(|ctx| ctx.draw_image(&image(&data), 0.0, 0.0));
    assert_eq!(px(0, 0), "255,0,0,128");
}

#[test]
fn an_image_data_is_drawn_from_its_own_colour_space() {
    // `234,51,35` in Display P3 is sRGB's pure red; read as sRGB it is
    // itself. The Node binding gives these two answers for the same calls.
    let pixel = |color_space| {
        let options = PixelExportOptions {
            color_space,
            ..PixelExportOptions::default()
        };
        ImageData::from_pixels(1, 1, options, vec![234, 51, 35, 255])
            .expect("one pixel")
    };
    let p3 = drawn(|ctx| {
        ctx.draw_image(&image(&pixel(PixelColorSpace::DisplayP3)), 0.0, 0.0)
    });
    let srgb = drawn(|ctx| {
        ctx.draw_image(&image(&pixel(PixelColorSpace::Srgb)), 0.0, 0.0)
    });
    assert_eq!([p3(0, 0), srgb(0, 0)], ["255,0,0,255", "234,51,35,255"]);
}
