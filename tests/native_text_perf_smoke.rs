use std::time::Instant;

use anyhow::Result;
use skia_canvas::native::{
    LinearColorSpace, NativeRecorder, OutputColorSpace, PixelFormat,
    RawFrameOptions, Rect, RgbaLinear, SurfaceOptions, TextAlign,
    TextBoxOptions, VerticalAlign,
};

const WIDTH: u32 = 1920;
const HEIGHT: u32 = 1080;
const TEXT: &str = "Welcome to Phyron Studio";
const FONT_SIZE: f32 = 72.0;

#[test]
#[ignore = "manual performance smoke; run with --ignored --nocapture"]
fn renders_default_text_to_rgba32f_smoke() -> Result<()> {
    let started = Instant::now();
    let mut recorder = NativeRecorder::new(Rect::from_xywh(
        0.0,
        0.0,
        WIDTH as f32,
        HEIGHT as f32,
    ))?;

    let record_started = Instant::now();
    recorder.record(|canvas| {
        canvas.clear(RgbaLinear::opaque(0.0, 0.0, 0.0));
        canvas.draw_text_box(
            TEXT,
            Rect::from_xywh(560.0, 440.0, 800.0, 200.0),
            &TextBoxOptions {
                color: RgbaLinear::opaque(1.0, 1.0, 1.0),
                font_family: Some("Inter".to_string()),
                font_size: FONT_SIZE,
                font_weight: 700,
                horizontal_align: TextAlign::Center,
                vertical_align: VerticalAlign::Top,
                opacity: 1.0,
            },
        );
    });
    let record_ms = record_started.elapsed().as_secs_f64() * 1000.0;

    let render_started = Instant::now();
    let frame = recorder.render_raw(
        SurfaceOptions {
            color_space: LinearColorSpace::Srgb,
            ..SurfaceOptions::default()
        },
        RawFrameOptions {
            pixel_format: PixelFormat::Rgba32fPremul,
            color_space: OutputColorSpace::Srgb,
        },
    )?;
    let render_ms = render_started.elapsed().as_secs_f64() * 1000.0;
    let total_ms = started.elapsed().as_secs_f64() * 1000.0;

    assert_eq!(frame.width(), WIDTH);
    assert_eq!(frame.height(), HEIGHT);
    assert_eq!(frame.pixel_format(), PixelFormat::Rgba32fPremul);
    assert_eq!(
        frame.stride(),
        WIDTH as usize * PixelFormat::Rgba32fPremul.bytes_per_pixel()
    );
    assert_eq!(frame.pixels().len(), HEIGHT as usize * frame.stride());
    assert!(
        frame
            .pixels()
            .chunks_exact(PixelFormat::Rgba32fPremul.bytes_per_pixel())
            .any(|pixel| pixel[..12].iter().any(|channel| *channel != 0)),
        "expected text draw to write non-black RGB channels"
    );

    eprintln!(
        "native_text_rgba32f_smoke width={} height={} stride={} bytes={} record_ms={:.1} render_raw_ms={:.1} total_ms={:.1}",
        frame.width(),
        frame.height(),
        frame.stride(),
        frame.pixels().len(),
        record_ms,
        render_ms,
        total_ms
    );

    Ok(())
}
