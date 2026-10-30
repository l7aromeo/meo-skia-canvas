//! A Skia image filter on a page that composites in floats keeps the
//! page's floats. Skia draws a filtered draw through a layer of its own,
//! and on the CPU that layer is eight bits a channel whatever the page --
//! `SkBitmapDevice::createDevice` makes any layer whose paint carries an
//! image filter `kN32` -- so an alpha of 0.003 came back as a whole level
//! or none.

use meo_skia_canvas::prelude::*;

const SIDE: f32 = 200.0;

/// An alpha no eight-bit layer holds: 0.765 of a level.
const FAINT: f32 = 0.003;

/// How far an F16 page's alpha may be from [`FAINT`]: half of F16's step
/// there, 2^-19.
const F16_STEP: f32 = 1.0 / 524_288.0;

fn canvas(depth: PixelDepth, gpu: bool) -> Option<Canvas> {
    let canvas = Canvas::with_options(
        SIDE,
        SIDE,
        CanvasOptions {
            color_type: depth,
            gpu,
            ..CanvasOptions::default()
        },
    )
    .expect("a canvas");
    (!gpu || canvas.engine_kind() == EngineKind::Gpu).then_some(canvas)
}

/// The strongest premultiplied alpha on the page, read back as F32.
fn strongest_alpha(canvas: &mut Canvas) -> f32 {
    let data = canvas
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
        .expect("pixels");
    data.pixels()
        .chunks(16)
        .map(|p| f32::from_le_bytes([p[12], p[13], p[14], p[15]]))
        .fold(0.0, f32::max)
}

/// The premultiplied alpha at `(x, y)`, read back as F32.
fn alpha_at(canvas: &mut Canvas, x: f32, y: f32) -> f32 {
    let data = canvas
        .context()
        .get_image_data_as(
            x,
            y,
            1.0,
            1.0,
            PixelExportOptions {
                depth: PixelDepth::F32,
                premultiplied: true,
                ..PixelExportOptions::default()
            },
        )
        .expect("a pixel");
    let alpha = &data.pixels()[12..16];
    f32::from_le_bytes([alpha[0], alpha[1], alpha[2], alpha[3]])
}

fn unmoved() -> ImageFilter {
    ImageFilter::offset(0.0, 0.0, None, None).expect("a filter")
}

type Draw = fn(&mut Context2D);

/// Draws in [`FAINT`] across the middle of the page, through whatever
/// filter is set: a path, text, an image and a canvas drawn in at that
/// alpha, and a path blurred by a Skia blur, whose middle is still the
/// colour's alpha. Each one's strongest pixel is [`FAINT`].
const DRAWS: [(&str, Draw); 5] = [
    ("a path", |ctx| {
        ctx.set_fill_style_css(&format!("rgb(0 0 255 / {FAINT})"))
            .expect("a colour");
        ctx.fill_rect(50.0, 50.0, 100.0, 100.0);
    }),
    ("text", |ctx| {
        ctx.set_font_css("bold 120px sans-serif");
        ctx.set_fill_style_css(&format!("rgb(0 0 255 / {FAINT})"))
            .expect("a colour");
        ctx.fill_text("I", 70.0, 150.0, None);
    }),
    ("an image", |ctx| {
        let pixels = [0u8, 0, 255, 255].repeat(100 * 100);
        let image = Image::from_pixels(
            &pixels,
            100,
            100,
            100 * 4,
            PixelExportOptions::default(),
        )
        .expect("an image");
        ctx.set_global_alpha(FAINT);
        ctx.draw_image(&image, 50.0, 50.0);
    }),
    ("a canvas drawn in", |ctx| {
        let mut source = Canvas::new(100.0, 100.0);
        source
            .context()
            .set_fill_style_css("blue")
            .expect("a colour");
        source.context().fill_rect(0.0, 0.0, 100.0, 100.0);
        ctx.set_global_alpha(FAINT);
        ctx.draw_canvas(&mut source, 50.0, 50.0);
    }),
    ("a path under a Skia blur", |ctx| {
        let blur =
            ImageFilter::blur(2.0, 2.0, None, None, None).expect("a filter");
        ctx.set_image_filter(Some(&blur));
        ctx.set_fill_style_css(&format!("rgb(0 0 255 / {FAINT})"))
            .expect("a colour");
        ctx.fill_rect(50.0, 50.0, 100.0, 100.0);
    }),
];

#[test]
fn a_skia_image_filter_on_a_float_page_keeps_its_floats() {
    for (depth, step) in [(PixelDepth::F16, F16_STEP), (PixelDepth::F32, 1e-6)]
    {
        for gpu in [false, true] {
            for (name, draw) in DRAWS {
                let Some(mut canvas) = canvas(depth, gpu) else {
                    continue;
                };
                let ctx = canvas.context();
                ctx.set_image_filter(Some(&unmoved()));
                draw(ctx);
                let alpha = strongest_alpha(&mut canvas);
                assert!(
                    (alpha - FAINT).abs() <= step,
                    "{depth:?}, gpu {gpu}, {name}: alpha {alpha}, not \
                     {FAINT}"
                );
            }
        }
    }
}

#[test]
fn a_skia_image_filter_on_a_float_page_moves_a_draw_into_a_clip() {
    // The square lies wholly outside the clip and the filter moves it
    // wholly inside: what it is made from has to be read from past the
    // clip by as far as the filter carries it.
    for gpu in [false, true] {
        let Some(mut canvas) = canvas(PixelDepth::F32, gpu) else {
            continue;
        };
        let ctx = canvas.context();
        ctx.begin_path();
        ctx.rect(SIDE / 2.0, 0.0, SIDE / 2.0, SIDE);
        ctx.clip(FillRule::NonZero);
        let moved =
            ImageFilter::offset(100.0, 0.0, None, None).expect("a filter");
        ctx.set_image_filter(Some(&moved));
        ctx.set_fill_style_css("blue").expect("a colour");
        ctx.fill_rect(20.0, 90.0, 20.0, 20.0);
        let (moved, left) = (
            alpha_at(&mut canvas, 130.0, 100.0),
            alpha_at(&mut canvas, 30.0, 100.0),
        );
        assert!(
            moved == 1.0 && left == 0.0,
            "gpu {gpu}: the moved square reads {moved}, where it was {left}"
        );
    }
}

#[test]
fn a_skia_image_filter_moves_a_draw_off_the_page_onto_it_as_it_is_scaled() {
    // Under a scale of 2 the filter's 60 units are 120 pixels, and the
    // square drawn 100 pixels off the page lands on it: the draw is kept
    // as far off the page as the filter carries it on the page, not in its
    // own units.
    for gpu in [false, true] {
        let Some(mut canvas) = canvas(PixelDepth::F32, gpu) else {
            continue;
        };
        let ctx = canvas.context();
        ctx.scale(2.0, 2.0);
        let moved =
            ImageFilter::offset(60.0, 0.0, None, None).expect("a filter");
        ctx.set_image_filter(Some(&moved));
        ctx.set_fill_style_css("blue").expect("a colour");
        ctx.fill_rect(-50.0, 40.0, 10.0, 10.0);
        let moved = alpha_at(&mut canvas, 30.0, 90.0);
        assert!(moved == 1.0, "gpu {gpu}: the moved square reads {moved}");
    }
}
