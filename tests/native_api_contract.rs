use anyhow::{Context, Result};
use meo_skia_canvas::prelude::*;

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

/// Every sample count the backend offers renders, including the two that mean
/// no multisampling.
///
/// `0` and `1` both ask for one sample a pixel. The Vulkan backend listed
/// both; the Metal backend listed only `0`, so `msaa: 1` came back as "1x MSAA
/// not supported by GPU" on macOS while rendering on Linux. This asks the
/// canvas for each count in turn, so it runs against whatever device the build
/// actually has -- and on a CPU-only build it still checks the counts are
/// accepted rather than refused.
#[test]
fn a_canvas_renders_at_every_sample_count_the_backend_offers() -> Result<()> {
    let mut canvas = Canvas::with_options(
        24.0,
        24.0,
        CanvasOptions {
            gpu: true,
            ..CanvasOptions::default()
        },
    )?;
    {
        let ctx = canvas.context();
        ctx.set_fill_style(RgbaLinear::opaque(1.0, 0.0, 0.0));
        ctx.fill_rect(4.0, 4.0, 16.0, 16.0);
    }

    for msaa in [None, Some(0), Some(1), Some(2), Some(4)] {
        let pixels = canvas
            .to_buffer(
                ImageFormat::Raw,
                &EncodeOptions {
                    msaa,
                    ..EncodeOptions::default()
                },
            )
            .map_err(|e| anyhow::anyhow!("msaa {msaa:?}: {e}"))?;
        let inked = pixels.chunks_exact(4).filter(|px| px[3] > 0).count();
        assert_eq!(inked, 256, "msaa {msaa:?} should ink the whole 16x16 rect",);
    }
    Ok(())
}
