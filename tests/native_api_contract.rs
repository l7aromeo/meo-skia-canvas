use anyhow::{Context, Result};
use skia_canvas::native::{
    LinearColorSpace, NativeImage, NativePaint, NativeRecorder, PixelFormat, RawFrameOptions, Rect,
    RgbaLinear, SurfaceOptions, TextBoxOptions,
};

#[test]
fn native_facade_renders_tight_rgba8_without_importing_skia_safe() -> Result<()> {
    let mut recorder = NativeRecorder::new(Rect::from_xywh(0.0, 0.0, 8.0, 8.0))?;

    recorder.record(|canvas| {
        canvas.clear(RgbaLinear::opaque(0.0, 0.0, 0.0));
        canvas.draw_rect(
            Rect::from_xywh(2.0, 2.0, 4.0, 4.0),
            &NativePaint::fill(RgbaLinear::opaque(1.0, 0.0, 0.0)),
        );
    });

    let frame = recorder.render_raw(
        SurfaceOptions {
            color_space: LinearColorSpace::Srgb,
            ..SurfaceOptions::default()
        },
        RawFrameOptions {
            pixel_format: PixelFormat::Rgba8UnormUnpremul,
            ..RawFrameOptions::default()
        },
    )?;

    assert_eq!(frame.width(), 8);
    assert_eq!(frame.height(), 8);
    assert_eq!(frame.stride(), 32);
    assert_eq!(frame.pixels().len(), 8 * 32);
    assert!(frame.pixels().iter().any(|channel| *channel != 0));
    Ok(())
}

#[test]
fn native_facade_constructs_required_linear_working_spaces() -> Result<()> {
    for color_space in [
        LinearColorSpace::Srgb,
        LinearColorSpace::DisplayP3,
        LinearColorSpace::Rec2020,
    ] {
        let mut recorder = NativeRecorder::new(Rect::from_xywh(0.0, 0.0, 4.0, 4.0))?;
        recorder.record(|canvas| canvas.clear(RgbaLinear::opaque(0.25, 0.5, 1.5)));
        let frame = recorder.render_raw(
            SurfaceOptions {
                color_space,
                ..SurfaceOptions::default()
            },
            RawFrameOptions::default(),
        )?;
        assert_eq!(frame.width(), 4);
        assert_eq!(frame.height(), 4);
    }
    Ok(())
}

#[test]
fn native_facade_draws_shapes() -> Result<()> {
    let mut recorder = NativeRecorder::new(Rect::from_xywh(0.0, 0.0, 64.0, 64.0))?;
    recorder.record(|canvas| {
        canvas.clear(RgbaLinear::opaque(0.0, 0.0, 0.0));
        canvas.draw_rect(
            Rect::from_xywh(4.0, 4.0, 16.0, 16.0),
            &NativePaint::fill(RgbaLinear::opaque(1.0, 0.0, 0.0)),
        );
        canvas.draw_rounded_rect(
            Rect::from_xywh(24.0, 4.0, 16.0, 16.0),
            4.0,
            &NativePaint::stroke(RgbaLinear::opaque(0.0, 1.0, 0.0), 2.0),
        );
        canvas.draw_oval(
            Rect::from_xywh(44.0, 4.0, 16.0, 16.0),
            &NativePaint::fill(RgbaLinear::opaque(0.0, 0.0, 1.0)),
        );
    });
    let frame = recorder.render_raw(SurfaceOptions::default(), RawFrameOptions::default())?;
    let pixels = frame.pixels();
    let stride = frame.stride();

    let pixel_at = |x: usize, y: usize| -> &[u8] {
        let off = y * stride + x * 4;
        &pixels[off..off + 4]
    };

    assert!(
        pixel_at(12, 12)[0] > 64,
        "expected red center to be visible"
    );
    assert!(
        pixel_at(52, 12)[2] > 64,
        "expected blue center to be visible"
    );
    let stroke_pixel = pixel_at(24, 12);
    assert!(
        stroke_pixel[1] > 32 || stroke_pixel[3] > 32,
        "expected stroked rounded rect to leave green/alpha pixels"
    );
    Ok(())
}

#[test]
fn native_facade_decodes_and_draws_encoded_image() -> Result<()> {
    let bytes = std::fs::read("tests/assets/pentagon.png").context("read fixture")?;
    let image = NativeImage::from_encoded(&bytes).context("decode fixture")?;
    assert!(image.width() > 0);
    assert!(image.height() > 0);

    let mut recorder = NativeRecorder::new(Rect::from_xywh(0.0, 0.0, 32.0, 32.0))?;
    recorder.record(|canvas| {
        canvas.clear(RgbaLinear::opaque(0.0, 0.0, 0.0));
        canvas.draw_image_rect(&image, Rect::from_xywh(0.0, 0.0, 32.0, 32.0), 1.0);
    });

    let frame = recorder.render_raw(SurfaceOptions::default(), RawFrameOptions::default())?;
    assert!(frame.pixels().iter().any(|channel| *channel != 0));
    Ok(())
}

#[test]
fn native_facade_draws_visible_text_pixels() -> Result<()> {
    let mut recorder = NativeRecorder::new(Rect::from_xywh(0.0, 0.0, 128.0, 64.0))?;
    recorder.record(|canvas| {
        canvas.clear(RgbaLinear::opaque(0.0, 0.0, 0.0));
        canvas.draw_text_box(
            "Studio",
            Rect::from_xywh(4.0, 4.0, 120.0, 56.0),
            &TextBoxOptions {
                color: RgbaLinear::opaque(1.0, 1.0, 1.0),
                font_size: 32.0,
                ..TextBoxOptions::default()
            },
        );
    });
    let frame = recorder.render_raw(SurfaceOptions::default(), RawFrameOptions::default())?;
    assert!(frame.pixels().iter().any(|channel| *channel > 32));
    Ok(())
}
