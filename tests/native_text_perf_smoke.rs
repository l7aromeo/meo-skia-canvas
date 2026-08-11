use std::time::Instant;

use anyhow::Result;
use meo_skia_canvas::prelude::*;

const WIDTH: f32 = 1920.0;
const HEIGHT: f32 = 1080.0;
const TEXT: &str = "Welcome to the canvas";
const FONT_SIZE: f32 = 72.0;

#[test]
#[ignore = "manual performance smoke; run with --ignored --nocapture"]
fn renders_a_page_of_text_to_float_pixels_smoke() -> Result<()> {
    let started = Instant::now();
    let mut canvas = Canvas::with_options(
        WIDTH,
        HEIGHT,
        CanvasOptions {
            color_type: PixelDepth::F32,
            gpu: false,
            ..CanvasOptions::default()
        },
    )?;

    let draw_started = Instant::now();
    {
        let ctx = canvas.context();
        ctx.set_fill_style(RgbaLinear::opaque(0.0, 0.0, 0.0));
        ctx.fill_rect(0.0, 0.0, WIDTH, HEIGHT);

        ctx.set_fill_style(RgbaLinear::opaque(1.0, 1.0, 1.0));
        ctx.set_font(&Font::new("Inter", FONT_SIZE).weight(700));
        ctx.set_text_align(TextAlign::Center);
        ctx.fill_text(TEXT, WIDTH / 2.0, HEIGHT / 2.0, None);
    }
    let draw_ms = draw_started.elapsed().as_secs_f64() * 1000.0;

    let render_started = Instant::now();
    let frame =
        canvas.to_buffer(ImageFormat::Raw, &EncodeOptions::default())?;
    let render_ms = render_started.elapsed().as_secs_f64() * 1000.0;
    let total_ms = started.elapsed().as_secs_f64() * 1000.0;

    // Sixteen bytes a pixel: the canvas was built for F32 and the readback
    // follows it.
    assert_eq!(frame.len(), (WIDTH * HEIGHT) as usize * 16);
    assert!(
        frame.iter().any(|channel| *channel != 0),
        "the page should carry ink",
    );

    println!(
        "text perf smoke: draw {draw_ms:.1} ms, render {render_ms:.1} ms, \
         total {total_ms:.1} ms"
    );
    Ok(())
}
