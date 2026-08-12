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

/// Every colour space the JavaScript side names can be built from Rust too.
///
/// The two surfaces are one library, and `hdr10`/`rec2020-pq` and
/// `rec2020-hlg`/`hlg` were reachable only from JavaScript: `PixelColorSpace`
/// had six variants and neither HDR transfer function, so a crates.io caller
/// could not ask for a canvas an npm caller could.
///
/// Each space is drawn with the same colour and then exported into one common
/// space, which is what makes the primaries visible. Read back in its own
/// space every canvas returns the numbers it was given -- `RgbaLinear` means
/// whatever the destination says it means -- so an in-space comparison would
/// pass whether or not the space was real.
#[test]
fn every_documented_color_space_builds_a_canvas() -> Result<()> {
    let painted_in_srgb = |space: PixelColorSpace| -> Result<[u8; 4]> {
        let mut canvas = Canvas::with_options(
            8.0,
            8.0,
            CanvasOptions {
                color_space: space,
                ..CanvasOptions::default()
            },
        )?;
        {
            let ctx = canvas.context();
            // Mid-level so a transfer function shows, saturated so primaries
            // do: neither shows at 0.0 or 1.0, and neither shows on grey.
            ctx.set_fill_style(RgbaLinear::opaque(0.5, 0.2, 0.05));
            ctx.fill_rect(0.0, 0.0, 8.0, 8.0);
        }
        let pixels = canvas.to_buffer(
            ImageFormat::Raw,
            &EncodeOptions {
                color_space: Some(PixelColorSpace::Srgb),
                ..EncodeOptions::default()
            },
        )?;
        Ok([pixels[0], pixels[1], pixels[2], pixels[3]])
    };

    let srgb = painted_in_srgb(PixelColorSpace::Srgb)?;
    for wider in [
        PixelColorSpace::DisplayP3,
        PixelColorSpace::Rec2020,
        PixelColorSpace::Rec2020Pq,
        PixelColorSpace::Rec2020Hlg,
    ] {
        assert_ne!(
            painted_in_srgb(wider)?,
            srgb,
            "{wider:?} has its own primaries, so the same colour lands \
             elsewhere once both are brought back to sRGB",
        );
    }

    // And the default export space is the canvas's own, not sRGB. This is
    // the JavaScript behaviour: a wide-gamut canvas hands back wide-gamut
    // pixels unless asked otherwise. Comparing the two exports of one canvas
    // is what pins it -- comparing two canvases passed even with the default
    // forced back to sRGB, because linear sRGB converts to within a level of
    // sRGB and `assert_ne!` was happy with the difference of one.
    for space in [
        PixelColorSpace::SrgbLinear,
        PixelColorSpace::DisplayP3,
        PixelColorSpace::Rec2020,
        PixelColorSpace::Rec2020Pq,
        PixelColorSpace::Rec2020Hlg,
    ] {
        let mut canvas = Canvas::with_options(
            8.0,
            8.0,
            CanvasOptions {
                color_space: space,
                ..CanvasOptions::default()
            },
        )?;
        {
            let ctx = canvas.context();
            ctx.set_fill_style(RgbaLinear::opaque(0.5, 0.2, 0.05));
            ctx.fill_rect(0.0, 0.0, 8.0, 8.0);
        }
        let inherited =
            canvas.to_buffer(ImageFormat::Raw, &EncodeOptions::default())?;
        let as_srgb = canvas.to_buffer(
            ImageFormat::Raw,
            &EncodeOptions {
                color_space: Some(PixelColorSpace::Srgb),
                ..EncodeOptions::default()
            },
        )?;
        assert_ne!(
            inherited[..4],
            as_srgb[..4],
            "{space:?} exported unasked should stay in its own space, not \
             be converted to sRGB",
        );
    }

    Ok(())
}

/// The HDR transfer functions are the ones the Node binding builds.
///
/// Both sides ask Skia for Rec. 2020 primaries with the PQ or HLG curve, so a
/// canvas made from Rust and one made from JavaScript are the same canvas.
/// This pins the curve rather than the name: PQ and HLG hold a mid grey at
/// visibly different levels, and both differ from Rec. 2020's own BT.709
/// curve.
#[test]
fn the_hdr_transfer_functions_are_distinct_curves() -> Result<()> {
    let mid_grey = |space: PixelColorSpace| -> Result<[u8; 4]> {
        let mut canvas = Canvas::with_options(
            4.0,
            4.0,
            CanvasOptions {
                color_space: space,
                ..CanvasOptions::default()
            },
        )?;
        {
            let ctx = canvas.context();
            ctx.set_fill_style(RgbaLinear::from_srgb(0.5, 0.5, 0.5, 1.0));
            ctx.fill_rect(0.0, 0.0, 4.0, 4.0);
        }
        let pixels =
            canvas.to_buffer(ImageFormat::Raw, &EncodeOptions::default())?;
        Ok([pixels[0], pixels[1], pixels[2], pixels[3]])
    };

    let rec709 = mid_grey(PixelColorSpace::Rec2020)?;
    let pq = mid_grey(PixelColorSpace::Rec2020Pq)?;
    let hlg = mid_grey(PixelColorSpace::Rec2020Hlg)?;

    assert_ne!(pq, rec709, "PQ is not Rec. 709: {pq:?} against {rec709:?}");
    assert_ne!(hlg, rec709, "HLG is not Rec. 709: {hlg:?}");
    assert_ne!(pq, hlg, "PQ and HLG are different curves: {pq:?} / {hlg:?}");
    Ok(())
}

/// A float canvas composites in float, and an eight-bit one does not.
///
/// `colorType` used to select only the readback format: the page was always
/// composited at eight bits and converted on the way out, so `F32` bought
/// nothing but a wider buffer to put the same rounded values in. Sixty faint
/// layers is where that shows -- each one rounds to a whole level, and the
/// error compounds.
#[test]
fn a_float_canvas_composites_without_rounding_every_layer() -> Result<()> {
    let accumulated = |depth: PixelDepth| -> Result<f32> {
        let mut canvas = Canvas::with_options(
            8.0,
            8.0,
            CanvasOptions {
                color_type: depth,
                // GPU allowed on purpose. Asking for a float canvas is a
                // request about precision, not about a backend: where the
                // GPU cannot composite in float, `Canvas` hands the page to
                // the raster backend rather than narrowing it to eight bits,
                // and `engine_kind` reports which one took it. So this
                // passes whichever engine answers -- including on a Skia
                // whose Metal and Vulkan backends grow the format they lack
                // today.
                gpu: true,
                ..CanvasOptions::default()
            },
        )?;
        let ctx = canvas.context();
        for _ in 0..60 {
            ctx.set_fill_style(RgbaLinear::new_premultiplied(
                0.006, 0.006, 0.006, 0.006,
            ));
            ctx.fill_rect(0.0, 0.0, 8.0, 8.0);
        }
        let pixels = ctx.get_image_data_as(
            0.0,
            0.0,
            1.0,
            1.0,
            PixelExportOptions {
                depth: PixelDepth::F32,
                premultiplied: true,
                ..PixelExportOptions::default()
            },
        )?;
        let bytes = pixels.pixels();
        Ok(f32::from_le_bytes([
            bytes[12], bytes[13], bytes[14], bytes[15],
        ]))
    };

    // Sixty layers of 0.006 over each other: 1 - 0.994^60.
    let ideal = 1.0 - 0.994f32.powi(60);
    let eight_bit = accumulated(PixelDepth::Uint8)?;
    let float16 = accumulated(PixelDepth::F16)?;
    let float32 = accumulated(PixelDepth::F32)?;

    assert!(
        (float32 - ideal).abs() < 0.001,
        "an F32 canvas should land on {ideal:.5}, got {float32:.5}",
    );
    assert!(
        (float16 - ideal).abs() < 0.005,
        "an F16 canvas should be close to {ideal:.5}, got {float16:.5}",
    );
    assert!(
        (eight_bit - ideal).abs() > 0.05,
        "an eight-bit canvas rounds every layer, so it should be well short \
         of {ideal:.5}; got {eight_bit:.5}, which suggests it is compositing \
         in float after all",
    );
    Ok(())
}

/// Asking to read back in float does not make the page composite in float.
///
/// The compositing format follows the canvas, the way the compositing space
/// does. When the readback request chose it instead, an eight-bit canvas read
/// through `PixelExportOptions { depth: F32 }` silently composited the whole
/// page in float -- a precision that depended on how it was later measured.
#[test]
fn a_float_readback_does_not_upgrade_an_eight_bit_canvas() -> Result<()> {
    let mut canvas = Canvas::with_options(
        8.0,
        8.0,
        CanvasOptions {
            color_type: PixelDepth::Uint8,
            gpu: false,
            ..CanvasOptions::default()
        },
    )?;
    let ctx = canvas.context();
    // A fifth of an eight-bit level. An eight-bit surface has nowhere to put
    // it and rounds to one level; a float surface would keep it.
    ctx.set_fill_style(RgbaLinear::new_premultiplied(
        0.002, 0.002, 0.002, 0.002,
    ));
    ctx.fill_rect(0.0, 0.0, 8.0, 8.0);

    let pixels = ctx.get_image_data_as(
        0.0,
        0.0,
        1.0,
        1.0,
        PixelExportOptions {
            depth: PixelDepth::F32,
            premultiplied: true,
            ..PixelExportOptions::default()
        },
    )?;
    let bytes = pixels.pixels();
    let alpha =
        f32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]);
    assert!(
        (alpha - 1.0 / 255.0).abs() < 0.0001,
        "an eight-bit canvas should hold one level, {:.5}, not {alpha:.5}",
        1.0 / 255.0,
    );
    Ok(())
}

/// The Rust surface takes the CSS colors the JavaScript one takes.
///
/// All of CSS Color 4 -- `oklch()`, `lab()`, `color(<space> ...)` and the rest
/// -- lived in the Node binding and was reachable only from JavaScript:
/// `set_fill_style` takes a value already in the canvas's space, so there was
/// no way to say "this colour, named the way CSS names it" from Rust. Both
/// surfaces now go through one parser, and these figures were checked against
/// the JavaScript side drawing the same strings: byte for byte the same.
#[test]
fn the_rust_surface_takes_css_colors() -> Result<()> {
    let painted = |css: &str, space: PixelColorSpace| -> Result<[u8; 4]> {
        let mut canvas = Canvas::with_options(
            4.0,
            4.0,
            CanvasOptions {
                color_space: space,
                gpu: false,
                ..CanvasOptions::default()
            },
        )?;
        {
            let ctx = canvas.context();
            ctx.set_fill_style_css(css)?;
            ctx.fill_rect(0.0, 0.0, 4.0, 4.0);
        }
        let pixels =
            canvas.to_buffer(ImageFormat::Raw, &EncodeOptions::default())?;
        Ok([pixels[0], pixels[1], pixels[2], pixels[3]])
    };

    for (css, srgb) in [
        ("red", [255, 0, 0, 255]),
        ("#3a7", [51, 170, 119, 255]),
        ("rgb(0 128 255)", [0, 128, 255, 255]),
        ("hsl(200 70% 50%)", [38, 157, 217, 255]),
        ("oklch(70% 0.2 140)", [77, 186, 48, 255]),
        ("lab(60% 40 -30)", [189, 119, 198, 255]),
        ("hwb(90 10% 20%)", [115, 204, 26, 255]),
        ("color(srgb 0.2 0.4 0.9)", [51, 102, 230, 255]),
    ] {
        assert_eq!(painted(css, PixelColorSpace::Srgb)?, srgb, "{css}");
    }

    // The space the string names is kept rather than routed through sRGB:
    // P3 red on a P3 canvas is that canvas's own red, where `"red"` is sRGB
    // red converted into it.
    assert_eq!(
        painted("color(display-p3 1 0 0)", PixelColorSpace::DisplayP3)?,
        [255, 0, 0, 255],
    );
    assert_eq!(
        painted("red", PixelColorSpace::DisplayP3)?,
        [234, 51, 35, 255],
    );

    // A browser keeps the previous fill when the string will not parse. Rust
    // gets told, since it has somewhere to put the answer.
    let mut canvas = Canvas::new(4.0, 4.0);
    let ctx = canvas.context();
    assert!(ctx.set_fill_style_css("not-a-color").is_err());
    assert!(ctx.set_stroke_style_css("also not").is_err());
    assert!(ctx.set_shadow_color_css("nope").is_err());
    Ok(())
}

/// A font family can be asked what it offers, as `FontLibrary.family()` does.
///
/// The Rust surface could list what it had registered and nothing else: no way
/// to ask what the platform installed, or whether a family ships the narrower
/// face `set_font_stretch` would need. That left the stretch tests naming a
/// macOS font and skipping everywhere else.
#[test]
fn a_family_reports_the_faces_it_offers() -> Result<()> {
    let fonts = FontManager::new();

    // Registered families are visible immediately, with the axis positions
    // the variable font declares.
    fonts.register_font_from_path(
        "OswaldQuery",
        "tests/assets/Oswald/Oswald-VariableFont_wght.ttf",
    )?;
    let oswald = fonts
        .family_details("OswaldQuery")
        .expect("a family registered a moment ago resolves");
    assert_eq!(oswald.family, "OswaldQuery");
    assert!(
        oswald.weights.len() > 1,
        "Oswald's weight axis should offer more than one: {:?}",
        oswald.weights,
    );

    // A name nobody registered and no platform ships resolves to nothing,
    // rather than to whatever would have been substituted for it.
    assert!(fonts.family_details("ZzNoSuchFamilyIsInstalled").is_none());

    // `families` is what this registry was given; `installed_families` is
    // what a draw can actually match against.
    assert_eq!(fonts.families(), vec!["OswaldQuery".to_string()]);
    assert!(
        fonts.installed_families().len() >= fonts.families().len(),
        "the installed list includes the registered ones",
    );
    Ok(())
}

/// `font-stretch` reaches a variable font's width axis, not only a separate
/// condensed face.
///
/// It used to select among faces and nothing else, so a family that carries
/// its widths on a `wdth` axis -- which is how most variable fonts ship, and
/// how Ubuntu ships on Linux -- measured the same at every setting. A browser
/// applies the property to the axis, and fontconfig already resolved the named
/// instance: `fc-match "Ubuntu:width=condensed"` picks `Ubuntu[wdth,wght].ttf`.
/// We were the ones ignoring it.
#[test]
fn font_stretch_reaches_a_variable_width_axis() -> Result<()> {
    let fonts = FontManager::new();
    fonts.register_font_from_path(
        "AmstelvarStretch",
        "tests/assets/fonts/AmstelvarAlpha-VF.ttf",
    )?;

    let width_at = |stretch: FontStretch| -> f32 {
        let mut canvas = Canvas::new(10.0, 10.0);
        let ctx = canvas.context();
        ctx.set_font(&Font::new("AmstelvarStretch", 40.0).stretch(stretch));
        ctx.measure_text("wwwwwwww", None).width
    };

    let normal = width_at(FontStretch::Normal);
    let condensed = width_at(FontStretch::Condensed);
    let ultra = width_at(FontStretch::UltraCondensed);

    assert!(
        condensed < normal,
        "condensed should narrow the axis: {condensed} against {normal}",
    );
    assert!(
        ultra < condensed,
        "ultra-condensed narrows further still: {ultra} against {condensed}",
    );
    Ok(())
}
/// The crate root is a door: no module paths, no prelude.
///
/// The modules group the types by subject, which is how they are documented,
/// but one draw reaches across four of them -- `Canvas::to_buffer` alone
/// speaks `ImageFormat`, `EncodeOptions`, `Error` and the pixel types. A
/// caller should not have to learn that `FillRule` lives in `path` and
/// `BlendMode` in `paint` to write it. This imports the way a reader of the
/// README would.
#[test]
fn the_crate_root_is_a_door() -> Result<()> {
    let mut canvas = meo_skia_canvas::Canvas::with_options(
        60.0,
        40.0,
        meo_skia_canvas::CanvasOptions {
            color_space: meo_skia_canvas::PixelColorSpace::DisplayP3,
            color_type: meo_skia_canvas::PixelDepth::Uint8,
            gpu: false,
        },
    )?;
    {
        let ctx = canvas.context();
        ctx.set_fill_style_css("oklch(70% 0.2 140)")?;
        let mut path = meo_skia_canvas::PathBuilder::new();
        path.round_rect(4.0, 4.0, 52.0, 32.0, [6.0; 4])?;
        ctx.fill_path(
            &path.build(meo_skia_canvas::FillRule::NonZero),
            meo_skia_canvas::FillRule::NonZero,
        );
        ctx.set_text_align(meo_skia_canvas::TextAlign::Center);
        ctx.set_fill_style(meo_skia_canvas::RgbaLinear::opaque(1.0, 1.0, 1.0));
    }
    let png = canvas.to_buffer(
        meo_skia_canvas::ImageFormat::Png,
        &meo_skia_canvas::EncodeOptions::default(),
    )?;
    assert!(png.len() > 100);
    Ok(())
}
