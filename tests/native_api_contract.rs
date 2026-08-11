use anyhow::{Context, Result};
use meo_skia_canvas::prelude::*;

#[test]
fn native_facade_renders_tight_rgba8_without_importing_skia_safe() -> Result<()>
{
    let mut recorder = Recorder::new(Rect::from_xywh(0.0, 0.0, 8.0, 8.0))?;

    recorder.record(|canvas| {
        canvas.clear(RgbaLinear::opaque(0.0, 0.0, 0.0));
        canvas.draw_rect(
            Rect::from_xywh(2.0, 2.0, 4.0, 4.0),
            &Paint::fill(RgbaLinear::opaque(1.0, 0.0, 0.0)),
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
        let mut recorder = Recorder::new(Rect::from_xywh(0.0, 0.0, 4.0, 4.0))?;
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
    let mut recorder = Recorder::new(Rect::from_xywh(0.0, 0.0, 64.0, 64.0))?;
    recorder.record(|canvas| {
        canvas.clear(RgbaLinear::opaque(0.0, 0.0, 0.0));
        canvas.draw_rect(
            Rect::from_xywh(4.0, 4.0, 16.0, 16.0),
            &Paint::fill(RgbaLinear::opaque(1.0, 0.0, 0.0)),
        );
        canvas.draw_rounded_rect(
            Rect::from_xywh(24.0, 4.0, 16.0, 16.0),
            4.0,
            &Paint::stroke(RgbaLinear::opaque(0.0, 1.0, 0.0), 2.0),
        );
        canvas.draw_oval(
            Rect::from_xywh(44.0, 4.0, 16.0, 16.0),
            &Paint::fill(RgbaLinear::opaque(0.0, 0.0, 1.0)),
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
    let image = Image::from_encoded(&bytes).context("decode fixture")?;
    assert!(image.width() > 0);
    assert!(image.height() > 0);

    let mut recorder = Recorder::new(Rect::from_xywh(0.0, 0.0, 32.0, 32.0))?;
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
    let backend = Backend::new();
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
            &Paint::fill(RgbaLinear::opaque(1.0, 0.0, 0.0)),
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
    let backend = Backend::new();
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
    let backend = Backend::new();
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
        Err(Error::EngineUnavailable {
            engine: RenderEngine::Gpu,
            ..
        }) => {}
        Err(other) => panic!("unexpected error from Gpu request: {other}"),
    }
}

#[test]
fn engine_status_reports_typed_fields() {
    let backend = Backend::new();
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
    let mut recorder = Recorder::new(Rect::from_xywh(0.0, 0.0, 128.0, 64.0))?;
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
    let fm = FontManager::new();
    fm.register_font_from_data("Oswald", &font_bytes)?;
    let engine = TextEngine::new(&fm);
    let backend = Backend::new();

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

#[test]
fn text_layout_font_features_apply_without_error() -> Result<()> {
    // Features that the typeface may or may not implement must never
    // break layout; they're applied on the layout `TextStyle` directly.
    let engine = TextEngine::with_system_fonts();
    let style = TextStyle {
        font_size: 32.0,
        color: RgbaLinear::opaque(1.0, 1.0, 1.0),
        font_features: vec![
            FontFeature::on("smcp"),
            FontFeature::off("liga"),
            FontFeature::new("ss01", 1),
        ],
        ..TextStyle::default()
    };
    let layout = engine.layout_text("Studio Figures 1234", &style, 400.0);
    assert!(layout.width() > 0.0, "feature-styled text laid out empty");
    assert_eq!(FontFeature::on("tnum"), FontFeature::new("tnum", 1));
    assert_eq!(FontFeature::off("tnum"), FontFeature::new("tnum", 0));
    Ok(())
}

#[test]
fn text_layout_strut_forces_line_height() -> Result<()> {
    let engine = TextEngine::with_system_fonts();
    let base = TextStyle {
        font_size: 16.0,
        ..TextStyle::default()
    };
    let strutted = TextStyle {
        strut: Some(StrutStyle {
            font_size: Some(64.0),
            height: Some(1.0),
            force_height: true,
            ..StrutStyle::default()
        }),
        ..base.clone()
    };
    let plain_h = engine.layout_text("One line", &base, 400.0).height();
    let strut_h = engine.layout_text("One line", &strutted, 400.0).height();
    // A forced 64px strut line box must be taller than the natural 16px
    // line. If the strut were ignored the two heights would match.
    assert!(
        strut_h > plain_h * 2.0,
        "strut should force a taller line box; plain={plain_h} strut={strut_h}",
    );
    Ok(())
}

#[test]
fn text_layout_reports_max_line_overflow() -> Result<()> {
    let engine = TextEngine::with_system_fonts();
    let style = TextStyle {
        font_size: 20.0,
        max_lines: Some(1),
        ..TextStyle::default()
    };
    // Force wrapping into multiple lines by giving a narrow budget, then
    // cap at one line: the layout must report the overflow.
    let layout = engine.layout_text(
        "The quick brown fox jumps over the lazy dog",
        &style,
        80.0,
    );
    assert_eq!(layout.line_count(), 1, "max_lines=1 should clamp to 1 line");
    assert!(
        layout.did_exceed_max_lines(),
        "wrapped text capped at 1 line should report did_exceed_max_lines",
    );
    Ok(())
}

#[test]
fn text_layout_unresolved_codepoints_empty_for_latin() -> Result<()> {
    let engine = TextEngine::with_system_fonts();
    let style = TextStyle {
        font_size: 24.0,
        ..TextStyle::default()
    };
    let mut layout = engine.layout_text("Hello", &style, 400.0);
    // With system-font fallback enabled, plain Latin must resolve fully.
    assert!(
        layout.unresolved_codepoints().is_empty(),
        "basic Latin should have no unresolved codepoints with fallback on",
    );
    Ok(())
}

#[test]
fn paint_mask_blur_spreads_ink_beyond_rect() -> Result<()> {
    let mut recorder = Recorder::new(Rect::from_xywh(0.0, 0.0, 64.0, 64.0))?;
    let blur = MaskFilter::blur(BlurStyle::Normal, 6.0, true)?;
    recorder.record(|canvas| {
        canvas.clear(RgbaLinear::opaque(0.0, 0.0, 0.0));
        let mut paint = Paint::fill(RgbaLinear::opaque(1.0, 1.0, 1.0));
        paint.set_mask_filter(Some(blur.clone()));
        canvas.draw_rect(Rect::from_xywh(24.0, 24.0, 16.0, 16.0), &paint);
    });
    let frame = recorder
        .render_raw(SurfaceOptions::default(), RawFrameOptions::default())?;
    let pixels = frame.pixels();
    let stride = frame.stride();
    let lum = |x: usize, y: usize| pixels[y * stride + x * 4] as u32;
    // The rect spans x 24..40; a point 4px outside the left edge is pure
    // black without a mask filter. The Normal blur (sigma 6) bleeds the
    // white fill outward, so it must be lit.
    assert!(
        lum(20, 32) > 8,
        "mask blur halo should light pixels outside the rect; got {}",
        lum(20, 32),
    );
    Ok(())
}

#[test]
fn paint_compositing_extras_render() -> Result<()> {
    // Exercise setDither, the Clear blend mode, and saveLayer-with-bounds
    // in one pass: clear white, dithered fill, then Clear-erase a hole,
    // grouped inside an explicit layer. The center must end up non-white.
    let mut recorder = Recorder::new(Rect::from_xywh(0.0, 0.0, 32.0, 32.0))?;
    recorder.record(|canvas| {
        canvas.clear(RgbaLinear::opaque(1.0, 1.0, 1.0));
        let mut group = Paint::fill(RgbaLinear::opaque(0.0, 0.0, 0.0));
        group.set_alpha(0.5);
        canvas.save_layer_with(SaveLayerOptions {
            paint: Some(&group),
            bounds: Some(Rect::from_xywh(0.0, 0.0, 32.0, 32.0)),
            backdrop: None,
        });
        let mut fill = Paint::fill(RgbaLinear::opaque(0.2, 0.4, 0.8));
        fill.set_dither(true);
        canvas.draw_rect(Rect::from_xywh(0.0, 0.0, 32.0, 32.0), &fill);
        let mut eraser = Paint::fill(RgbaLinear::opaque(0.0, 0.0, 0.0));
        eraser.set_blend_mode(BlendMode::Clear);
        canvas.draw_rect(Rect::from_xywh(8.0, 8.0, 16.0, 16.0), &eraser);
        canvas.restore();
    });
    let frame = recorder
        .render_raw(SurfaceOptions::default(), RawFrameOptions::default())?;
    let px = frame.pixels();
    let stride = frame.stride();
    // A corner sits under the dithered fill (not the erased hole). The
    // fill composited through the 0.5-alpha layer must leave it tinted,
    // not the original white -- proving setDither + saveLayer_with ran.
    let corner = &px[2 * stride + 2 * 4..2 * stride + 2 * 4 + 4];
    assert!(
        corner[0] < 245,
        "0.5-alpha layer fill should tint the corner off-white; got {corner:?}",
    );
    // The Clear-erased hole exposes the white backdrop on restore.
    let center = &px[16 * stride + 16 * 4..16 * stride + 16 * 4 + 4];
    assert!(
        center[0] > 245 && center[1] > 245 && center[2] > 245,
        "Clear inside the layer should expose the white backdrop; got {center:?}",
    );
    Ok(())
}

#[test]
fn shader_gradient_variants_and_noise() -> Result<()> {
    let stops = [
        GradientStop {
            position: 0.0,
            color: RgbaLinear::opaque(1.0, 0.0, 0.0),
        },
        GradientStop {
            position: 1.0,
            color: RgbaLinear::opaque(0.0, 0.0, 1.0),
        },
    ];
    let interp = GradientColorSpace::Srgb;
    // Every factory must build a shader from valid stops.
    let radial =
        Shader::radial_gradient(Point::new(32.0, 32.0), 30.0, &stops, interp)?;
    Shader::sweep_gradient(Point::new(32.0, 32.0), 0.0, 360.0, &stops, interp)?;
    Shader::two_point_conical_gradient(
        Point::new(16.0, 32.0),
        0.0,
        Point::new(48.0, 32.0),
        24.0,
        &stops,
        interp,
    )?;
    Shader::fractal_noise(0.1, 0.1, 2, 1.0)?;
    Shader::turbulence(0.2, 0.2, 3, 7.0)?;
    // A single stop is rejected.
    assert!(
        Shader::radial_gradient(
            Point::new(0.0, 0.0),
            1.0,
            &stops[..1],
            interp,
        )
        .is_err(),
        "a one-stop gradient should be rejected",
    );

    // Painting the radial gradient over the surface fills it with color.
    let mut recorder = Recorder::new(Rect::from_xywh(0.0, 0.0, 64.0, 64.0))?;
    recorder.record(|canvas| {
        canvas.clear(RgbaLinear::opaque(0.0, 0.0, 0.0));
        let mut paint = Paint::fill(RgbaLinear::opaque(1.0, 1.0, 1.0));
        paint.set_shader(Some(radial.clone()));
        canvas.draw_rect(Rect::from_xywh(0.0, 0.0, 64.0, 64.0), &paint);
    });
    let frame = recorder
        .render_raw(SurfaceOptions::default(), RawFrameOptions::default())?;
    let lit = frame
        .pixels()
        .chunks_exact(4)
        .filter(|p| p[0] > 16 || p[2] > 16)
        .count();
    assert!(
        lit > 1000,
        "radial gradient should fill most pixels; lit={lit}"
    );
    Ok(())
}

#[test]
fn image_cubic_sampling_renders() -> Result<()> {
    // Cubic (Mitchell) sampling must produce a visible downscale -- the
    // highest-quality sampler for shrinking / moving imagery.
    let bytes =
        std::fs::read("tests/assets/pentagon.png").context("read fixture")?;
    let image = Image::from_encoded(&bytes).context("decode fixture")?;
    let mut recorder = Recorder::new(Rect::from_xywh(0.0, 0.0, 32.0, 32.0))?;
    recorder.record(|canvas| {
        canvas.clear(RgbaLinear::opaque(0.0, 0.0, 0.0));
        canvas.draw_image_src(
            &image,
            Rect::from_xywh(
                0.0,
                0.0,
                image.width() as f32,
                image.height() as f32,
            ),
            Rect::from_xywh(2.0, 2.0, 28.0, 28.0),
            None,
            SamplingMode::Cubic,
        );
    });
    let frame = recorder
        .render_raw(SurfaceOptions::default(), RawFrameOptions::default())?;
    assert!(
        frame.pixels().iter().any(|c| *c != 0),
        "cubic-sampled image should render visible pixels",
    );
    Ok(())
}

/// Every readback allocation has to refuse a size Skia cannot address, not
/// abort on it. There were three such allocations and only one was guarded,
/// so `render_raw` at a large frame with a 16-byte pixel format still died
/// with "capacity overflow" -- surfacing through the binding as
/// "internal error in Neon module".
#[test]
fn an_oversized_raw_frame_is_an_error_at_every_readback() -> Result<()> {
    // 12000 square at 16 bytes a pixel is 2.3 GB, past the signed 32-bit
    // byte count Skia measures a buffer with.
    let mut recorder =
        Recorder::new(Rect::from_xywh(0.0, 0.0, 12000.0, 12000.0))?;
    recorder.record(|canvas| {
        canvas.clear(RgbaLinear::opaque(0.0, 0.0, 0.0));
    });

    let refused = recorder.render_raw(
        SurfaceOptions::default(),
        RawFrameOptions {
            pixel_format: PixelFormat::Rgba32fPremul,
            ..RawFrameOptions::default()
        },
    );
    assert!(
        refused.is_err(),
        "a 2.3 GB frame should be refused, not aborted"
    );

    // The same frame at four bytes a pixel is 576 MB, which fits, and must
    // still work -- the guard is a ceiling, not a size limit on the page.
    let mut small = Recorder::new(Rect::from_xywh(0.0, 0.0, 64.0, 64.0))?;
    small.record(|canvas| {
        canvas.clear(RgbaLinear::opaque(0.0, 0.0, 0.0));
    });
    let frame = small.render_raw(
        SurfaceOptions::default(),
        RawFrameOptions {
            pixel_format: PixelFormat::Rgba32fPremul,
            ..RawFrameOptions::default()
        },
    )?;
    assert_eq!(frame.width(), 64);
    Ok(())
}

//
// -- The same contracts, through the Canvas facade
// ------------------------------------------------
//
// These cover types the facade exposes -- mask filters, the shader factories,
// cubic sampling, the compositing extras, variable-font axes -- which had
// their only coverage through `Recorder` and `Surface`.
//

/// The raw RGBA of a `width` x `height` canvas, rendered on the CPU so the
/// figures below are exact rather than backend-dependent.
fn facade_pixels(
    width: f32,
    height: f32,
    draw: impl FnOnce(&mut Context2D),
) -> Result<Vec<u8>> {
    let mut canvas = Canvas::with_options(
        width,
        height,
        CanvasOptions {
            gpu: false,
            ..CanvasOptions::default()
        },
    )?;
    {
        let ctx = canvas.context();
        draw(ctx);
    }
    Ok(canvas.to_buffer(ImageFormat::Raw, &EncodeOptions::default())?)
}

#[test]
fn facade_mask_blur_spreads_ink_beyond_the_rect() -> Result<()> {
    let blur = MaskFilter::blur(BlurStyle::Normal, 6.0, true)?;
    let pixels = facade_pixels(64.0, 64.0, |ctx| {
        ctx.set_fill_style(RgbaLinear::opaque(0.0, 0.0, 0.0));
        ctx.fill_rect(0.0, 0.0, 64.0, 64.0);
        ctx.set_mask_filter(Some(&blur));
        ctx.set_fill_style(RgbaLinear::opaque(1.0, 1.0, 1.0));
        ctx.fill_rect(24.0, 24.0, 16.0, 16.0);
    })?;

    // The rect spans x 24..40; four pixels outside its left edge is pure
    // black without a mask filter. A Normal blur at sigma 6 bleeds the white
    // fill outward, so it has to be lit.
    let lum = pixels[32 * 64 * 4 + 20 * 4] as u32;
    assert!(
        lum > 8,
        "mask blur should light pixels outside the rect, got {lum}"
    );
    Ok(())
}

#[test]
fn facade_paints_every_shader_the_factories_build() -> Result<()> {
    let stops = [
        GradientStop {
            position: 0.0,
            color: RgbaLinear::opaque(1.0, 0.0, 0.0),
        },
        GradientStop {
            position: 1.0,
            color: RgbaLinear::opaque(0.0, 0.0, 1.0),
        },
    ];
    let interp = GradientColorSpace::Srgb;

    let radial =
        Shader::radial_gradient(Point::new(32.0, 32.0), 30.0, &stops, interp)?;
    Shader::sweep_gradient(Point::new(32.0, 32.0), 0.0, 360.0, &stops, interp)?;
    Shader::two_point_conical_gradient(
        Point::new(16.0, 32.0),
        0.0,
        Point::new(48.0, 32.0),
        24.0,
        &stops,
        interp,
    )?;
    Shader::fractal_noise(0.1, 0.1, 2, 1.0)?;
    Shader::turbulence(0.2, 0.2, 3, 7.0)?;

    assert!(
        Shader::radial_gradient(Point::new(0.0, 0.0), 1.0, &stops[..1], interp)
            .is_err(),
        "a one-stop gradient should be rejected",
    );

    let pixels = facade_pixels(64.0, 64.0, |ctx| {
        ctx.set_fill_shader(&radial);
        ctx.fill_rect(0.0, 0.0, 64.0, 64.0);
    })?;
    let lit = pixels.chunks_exact(4).filter(|px| px[3] > 0).count();
    assert_eq!(lit, 64 * 64, "the gradient covers the page");
    Ok(())
}

#[test]
fn facade_image_sampling_follows_the_smoothing_quality() -> Result<()> {
    // Only an *upscale* can show this. A minification resolves to
    // `ScalingOperation::Unknown`, where `High` deliberately means linear --
    // Chrome's behaviour -- so Low, Medium and High agree there and a
    // downscale test would pass with the setting ignored. Scaling up, `High`
    // is Mitchell cubic and Low is bilinear.
    let tile = Image::from_pixels(
        &[
            255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 0, 255,
        ],
        2,
        2,
        8,
        PixelFormat::Rgba8UnormUnpremul,
        PixelColorSpace::Srgb,
    )?;

    let upscaled = |quality: SmoothingQuality| -> Result<Vec<u8>> {
        facade_pixels(32.0, 32.0, |ctx| {
            ctx.set_image_smoothing_quality(quality);
            ctx.draw_image_sized(&tile, 0.0, 0.0, 32.0, 32.0);
        })
    };

    let low = upscaled(SmoothingQuality::Low)?;
    let high = upscaled(SmoothingQuality::High)?;
    assert!(
        low.iter().any(|channel| *channel != 0),
        "the image rendered at all",
    );
    assert_ne!(low, high, "the quality reaches the sampler");

    // And turning smoothing off entirely is a third result -- nearest
    // neighbour, which keeps the four source pixels as hard squares.
    let off = facade_pixels(32.0, 32.0, |ctx| {
        ctx.set_image_smoothing_enabled(false);
        ctx.draw_image_sized(&tile, 0.0, 0.0, 32.0, 32.0);
    })?;
    assert_ne!(off, low, "smoothing off is not bilinear");
    Ok(())
}

#[test]
fn facade_composites_a_bounded_layer_and_an_erase() -> Result<()> {
    // A bounded half-alpha layer and the Clear blend mode in one pass: fill
    // white, open the layer, draw into it, erase a hole, close it.
    //
    // Dither is set as well but not asserted -- it makes no difference to a
    // flat fill at this size, and `dither_changes_a_shallow_gradient` in the
    // Context2D suite is where it is actually observable.
    let pixels = facade_pixels(32.0, 32.0, |ctx| {
        ctx.set_fill_style(RgbaLinear::opaque(1.0, 1.0, 1.0));
        ctx.fill_rect(0.0, 0.0, 32.0, 32.0);

        ctx.save_layer_with(
            0.5,
            Some(Rect {
                left: 0.0,
                top: 0.0,
                right: 32.0,
                bottom: 32.0,
            }),
            None,
        );
        ctx.set_dither(true);
        ctx.set_fill_style(RgbaLinear::opaque(0.2, 0.4, 0.8));
        ctx.fill_rect(0.0, 0.0, 32.0, 32.0);

        ctx.set_global_composite_operation(BlendMode::Clear);
        ctx.fill_rect(8.0, 8.0, 16.0, 16.0);
        ctx.restore();
    })?;

    // A corner sits under the fill rather than the erased hole. Pinned
    // exactly: the same draw through a *full*-alpha layer reads
    // [124, 170, 231], so a layer that dropped its alpha would pass a
    // "not white any more" check.
    let corner = &pixels[2 * 32 * 4 + 2 * 4..2 * 32 * 4 + 2 * 4 + 4];
    assert_eq!(
        corner,
        [188, 212, 242, 255],
        "the fill composited through a half-alpha layer",
    );

    // And the erased square is a hole in the layer, not in the page beneath.
    let middle = &pixels[16 * 32 * 4 + 16 * 4..16 * 32 * 4 + 16 * 4 + 4];
    assert_eq!(
        middle,
        [255, 255, 255, 255],
        "clear inside a layer leaves the page it composites onto",
    );
    Ok(())
}

#[test]
fn facade_draws_a_layout_at_the_axis_it_was_laid_out_with() -> Result<()> {
    let font_bytes =
        std::fs::read("tests/assets/Oswald/Oswald-VariableFont_wght.ttf")
            .context("oswald-vf")?;
    let fm = FontManager::new();
    fm.register_font_from_data("Oswald", &font_bytes)?;
    let engine = TextEngine::new(&fm);

    let ink_at = |wght: f32| -> Result<usize> {
        let style = TextStyle {
            font_families: vec!["Oswald".to_string()],
            color: RgbaLinear::opaque(1.0, 1.0, 1.0),
            font_size: 36.0,
            font_variations: vec![FontVariation::new(FontAxisTag::WGHT, wght)],
            ..TextStyle::default()
        };
        let layout = engine.layout_text("Studio", &style, 200.0);
        let pixels = facade_pixels(220.0, 60.0, |ctx| {
            ctx.set_fill_style(RgbaLinear::opaque(0.0, 0.0, 0.0));
            ctx.fill_rect(0.0, 0.0, 220.0, 60.0);
            ctx.draw_paragraph(&layout, 4.0, 4.0);
        })?;
        Ok(pixels.chunks_exact(4).filter(|px| px[0] > 64).count())
    };

    // A font registered from a buffer reaches the facade through a layout,
    // and a heavier axis position puts more ink on the page.
    let light = ink_at(200.0)?;
    let heavy = ink_at(700.0)?;
    assert!(light > 0, "the light weight rendered");
    assert!(
        heavy > light,
        "700 should ink more than 200, got {heavy} against {light}",
    );
    Ok(())
}
