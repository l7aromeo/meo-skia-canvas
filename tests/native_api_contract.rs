use anyhow::{Context, Result};
use skia_canvas::native::{
    EngineKind, FontAxisTag, FontVariation, LinearColorSpace, NativeBackend,
    NativeError, NativeFontManager, NativeImage, NativePaint, NativeRecorder,
    NativeTextEngine, PixelFormat, RawFrameOptions, Rect, RenderEngine,
    RgbaLinear, SurfaceOptions, TextBoxOptions, TextStyle,
};

#[test]
fn native_facade_renders_tight_rgba8_without_importing_skia_safe() -> Result<()>
{
    let mut recorder =
        NativeRecorder::new(Rect::from_xywh(0.0, 0.0, 8.0, 8.0))?;

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
        let mut recorder =
            NativeRecorder::new(Rect::from_xywh(0.0, 0.0, 4.0, 4.0))?;
        recorder
            .record(|canvas| canvas.clear(RgbaLinear::opaque(0.25, 0.5, 1.5)));
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
    let mut recorder =
        NativeRecorder::new(Rect::from_xywh(0.0, 0.0, 64.0, 64.0))?;
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
    let frame = recorder
        .render_raw(SurfaceOptions::default(), RawFrameOptions::default())?;
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
    let bytes =
        std::fs::read("tests/assets/pentagon.png").context("read fixture")?;
    let image = NativeImage::from_encoded(&bytes).context("decode fixture")?;
    assert!(image.width() > 0);
    assert!(image.height() > 0);

    let mut recorder =
        NativeRecorder::new(Rect::from_xywh(0.0, 0.0, 32.0, 32.0))?;
    recorder.record(|canvas| {
        canvas.clear(RgbaLinear::opaque(0.0, 0.0, 0.0));
        canvas.draw_image_rect(
            &image,
            Rect::from_xywh(0.0, 0.0, 32.0, 32.0),
            1.0,
        );
    });

    let frame = recorder
        .render_raw(SurfaceOptions::default(), RawFrameOptions::default())?;
    assert!(frame.pixels().iter().any(|channel| *channel != 0));
    Ok(())
}

#[test]
fn engine_auto_resolves_and_draws() -> Result<()> {
    // Auto must always succeed; surface reports a concrete engine kind.
    let backend = NativeBackend::new();
    let mut surface = backend.create_surface(
        16,
        16,
        SurfaceOptions {
            engine: RenderEngine::Auto,
            ..SurfaceOptions::default()
        },
    )?;
    let kind = surface.engine();
    assert!(matches!(kind, EngineKind::Cpu | EngineKind::Gpu));
    surface.with_canvas(|canvas| {
        canvas.clear(RgbaLinear::opaque(0.0, 0.0, 0.0));
        canvas.draw_rect(
            Rect::from_xywh(2.0, 2.0, 10.0, 10.0),
            &NativePaint::fill(RgbaLinear::opaque(1.0, 0.0, 0.0)),
        );
    });
    surface.flush();
    let frame = surface.read_pixels()?;
    assert!(frame.pixels().iter().any(|channel| *channel != 0));
    Ok(())
}

#[test]
fn engine_cpu_is_always_available() -> Result<()> {
    // CPU must work everywhere, including builds without GPU features.
    let backend = NativeBackend::new();
    let mut surface = backend.create_surface(
        8,
        8,
        SurfaceOptions {
            engine: RenderEngine::Cpu,
            ..SurfaceOptions::default()
        },
    )?;
    assert_eq!(surface.engine(), EngineKind::Cpu);
    surface.with_canvas(|canvas| {
        canvas.clear(RgbaLinear::opaque(0.5, 0.5, 0.5));
    });
    let frame = surface.read_pixels()?;
    assert!(frame.pixels().iter().any(|channel| *channel != 0));
    Ok(())
}

#[test]
fn engine_gpu_either_works_or_returns_engine_unavailable() {
    // The Gpu choice is non-deterministic across CI machines; either it
    // succeeds, or it surfaces EngineUnavailable. Anything else is a
    // contract break.
    let backend = NativeBackend::new();
    let result = backend.create_surface(
        8,
        8,
        SurfaceOptions {
            engine: RenderEngine::Gpu,
            ..SurfaceOptions::default()
        },
    );
    match result {
        Ok(s) => assert_eq!(s.engine(), EngineKind::Gpu),
        Err(NativeError::EngineUnavailable {
            engine: RenderEngine::Gpu,
            ..
        }) => {}
        Err(other) => panic!("unexpected error from Gpu request: {other}"),
    }
}

#[test]
fn engine_status_reports_typed_fields() {
    let backend = NativeBackend::new();
    let auto = backend.engine_status(RenderEngine::Auto);
    let cpu = backend.engine_status(RenderEngine::Cpu);

    // CPU pin always reports Cpu, regardless of GPU availability.
    assert_eq!(cpu.renderer, EngineKind::Cpu);
    assert!(cpu.api.is_none(), "CPU pin should not advertise a GPU API");
    assert!(cpu.threads >= 1);

    // Auto must agree with is_gpu_available about which renderer it picks.
    assert_eq!(
        auto.is_gpu_available,
        matches!(auto.renderer, EngineKind::Gpu),
        "Auto-resolved kind should match is_gpu_available",
    );

    // Gpu pin reports either Gpu (when available) or Cpu fallback (when
    // not), but `is_gpu_available` is the source of truth either way.
    let gpu = backend.engine_status(RenderEngine::Gpu);
    if gpu.is_gpu_available {
        assert_eq!(gpu.renderer, EngineKind::Gpu);
    }
}

#[test]
fn native_facade_draws_visible_text_pixels() -> Result<()> {
    let mut recorder =
        NativeRecorder::new(Rect::from_xywh(0.0, 0.0, 128.0, 64.0))?;
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
    let frame = recorder
        .render_raw(SurfaceOptions::default(), RawFrameOptions::default())?;
    assert!(frame.pixels().iter().any(|channel| *channel > 32));
    Ok(())
}

#[test]
fn font_axis_tag_parsing() {
    assert_eq!("wght".parse::<FontAxisTag>(), Ok(FontAxisTag::WGHT));
    assert_eq!("wdth".parse::<FontAxisTag>(), Ok(FontAxisTag::WDTH));
    // Wrong length / non-ASCII rejected.
    assert!("wgh".parse::<FontAxisTag>().is_err());
    assert!("wghts".parse::<FontAxisTag>().is_err());
    assert!("wgh❤".parse::<FontAxisTag>().is_err());
    assert_eq!(FontAxisTag::WGHT.as_bytes(), b"wght");
}

#[test]
fn text_layout_honors_font_variations_wght_axis() -> Result<()> {
    let font_bytes =
        std::fs::read("tests/assets/Oswald/Oswald-VariableFont_wght.ttf")
            .context("oswald-vf")?;
    let fm = NativeFontManager::new();
    fm.register_font_from_data("Oswald", &font_bytes)?;
    let engine = NativeTextEngine::new(&fm);
    let backend = NativeBackend::new();

    let ink_at = |wght: f32| -> Result<usize> {
        let mut surface = backend.create_surface(
            220,
            60,
            SurfaceOptions {
                engine: RenderEngine::Cpu,
                ..SurfaceOptions::default()
            },
        )?;
        let style = TextStyle {
            font_families: vec!["Oswald".to_string()],
            color: RgbaLinear::opaque(1.0, 1.0, 1.0),
            font_size: 36.0,
            font_variations: vec![FontVariation::new(FontAxisTag::WGHT, wght)],
            ..TextStyle::default()
        };
        let layout = engine.layout_text("Studio", &style, 200.0);
        surface.with_canvas(|canvas| {
            canvas.clear(RgbaLinear::opaque(0.0, 0.0, 0.0));
            canvas.draw_text_layout(&layout, 4.0, 4.0);
        });
        let frame = surface.read_pixels()?;
        Ok(frame.pixels().chunks_exact(4).filter(|p| p[0] > 64).count())
    };

    let thin = ink_at(200.0)?;
    let bold = ink_at(700.0)?;
    assert!(thin > 0, "thin variant rendered no text");
    // Heavier `wght` produces thicker glyph strokes, hence more lit
    // pixels. If the variation axis is being ignored, both renders
    // collapse to the typeface's default master and `thin == bold`.
    assert!(
        bold > thin,
        "wght=700 should produce more ink than wght=200; got thin={thin} bold={bold}",
    );
    Ok(())
}
