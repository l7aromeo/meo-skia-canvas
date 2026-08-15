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
        std::fs::read("tests/assets/fonts/Oswald/Oswald-VariableFont_wght.ttf")
            .context("oswald-vf")?;
    let fm = FontLibrary::new();
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
    let fonts = FontLibrary::new();

    // Registered families are visible immediately, with the axis positions
    // the variable font declares.
    fonts.register_font_from_path(
        "OswaldQuery",
        "tests/assets/fonts/Oswald/Oswald-VariableFont_wght.ttf",
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
    let fonts = FontLibrary::new();
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
            // The rest pattern, which is the contract `docs/api/native-rust.md`
            // states: a caller who names the fields they care about keeps
            // compiling when one is added. This test listed every field and
            // stopped compiling the first time one was, which is the breakage
            // the convention exists to prevent -- demonstrated by the test
            // meant to demonstrate the convention.
            ..Default::default()
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

#[test]
fn a_paragraph_relays_out_without_being_rebuilt() -> Result<()> {
    // `build` lays out once, and there was no way to ask for a second
    // width: a caller re-wrapping on a resize had to re-parse the runs,
    // re-resolve the fonts and re-shape every glyph, once per frame. The
    // JavaScript binding's `layout()` has always done this.
    let engine = TextEngine::with_system_fonts();
    let style = TextStyle {
        font_size: 16.0,
        ..TextStyle::default()
    };
    let mut layout = engine.layout_text(
        "A sentence long enough that it has somewhere to wrap.",
        &style,
        400.0,
    );
    let wide = layout.line_count();

    layout.layout(80.0);
    let narrow = layout.line_count();
    assert!(
        narrow > wide,
        "re-laying out narrower should wrap more: {narrow} against {wide}"
    );
    assert!(layout.max_width() <= 80.0, "and the width follows");

    layout.layout(400.0);
    assert_eq!(layout.line_count(), wide, "and it goes back");
    Ok(())
}

#[test]
fn a_line_reports_where_its_whitespace_and_newline_end() -> Result<()> {
    // Three offsets that differ only on a line that wrapped or broke,
    // which is exactly where confusing them shows: a selection drawn to
    // `end_index` covers the trailing spaces at a wrap point.
    let engine = TextEngine::with_system_fonts();
    let style = TextStyle {
        font_size: 16.0,
        ..TextStyle::default()
    };
    let layout = engine.layout_text("hello world\nbye", &style, 60.0);
    let lines = layout.line_metrics();
    assert!(lines.len() >= 2, "the text should wrap or break");

    for line in &lines {
        assert!(
            line.end_excluding_whitespaces <= line.end_index,
            "whitespace end is at or before the line end"
        );
        assert!(
            line.end_index <= line.end_including_newline,
            "the newline end is at or after the line end"
        );
    }
    // The hard break carries a newline the other offsets stop short of.
    let broken = lines.iter().find(|line| line.hard_break);
    if let Some(line) = broken {
        assert!(
            line.end_including_newline > line.end_excluding_whitespaces,
            "a hard break has a newline to include"
        );
    }
    Ok(())
}

#[test]
fn a_text_box_says_which_way_its_run_reads() -> Result<()> {
    // The field this used to drop. Skia hands back a direction per box and
    // `rects_for_range` returned bare rectangles, so a Rust caller could
    // draw a selection over bidirectional text but not tell which runs
    // were right-to-left.
    let engine = TextEngine::with_system_fonts();
    let style = TextStyle {
        font_size: 24.0,
        ..TextStyle::default()
    };
    let text = "abc";
    let layout = engine.layout_text(text, &style, 400.0);

    let tight = layout.rects_for_range(
        0..text.len(),
        RectHeightStyle::Tight,
        RectWidthStyle::Tight,
    );
    assert!(!tight.is_empty(), "the range covers glyphs");
    for one in &tight {
        assert_eq!(one.direction, TextDirection::LeftToRight);
        assert!(one.rect.right > one.rect.left);
    }

    // And the height style is a choice now, not a constant: `Max` reaches
    // the full line where `Tight` stops at the glyphs.
    let max = layout.rects_for_range(
        0..text.len(),
        RectHeightStyle::Max,
        RectWidthStyle::Tight,
    );
    let height = |boxes: &[TextBox]| boxes[0].rect.bottom - boxes[0].rect.top;
    assert!(
        height(&max) >= height(&tight),
        "Max is at least as tall as Tight"
    );
    Ok(())
}

#[test]
fn an_ellipsis_replaces_what_does_not_fit() -> Result<()> {
    // Reachable from JavaScript since before this crate had a Rust text
    // API, and from Rust not at all.
    let engine = TextEngine::with_system_fonts();
    let plain = TextStyle {
        font_size: 16.0,
        max_lines: Some(1),
        ..TextStyle::default()
    };
    let elided = TextStyle {
        ellipsis: Some("...".to_string()),
        ..plain.clone()
    };

    let long = "a sentence with far more words than one line can hold";
    let without = engine.layout_text(long, &plain, 60.0);
    let with = engine.layout_text(long, &elided, 60.0);

    assert_eq!(without.line_count(), 1);
    assert_eq!(with.line_count(), 1);
    assert!(
        without.did_exceed_max_lines() && with.did_exceed_max_lines(),
        "both overflow one line"
    );
    // The ellipsis is drawn inside the same width, so the line is not wider
    // -- what changes is that the last glyphs are replaced rather than cut.
    assert!(with.width() <= 60.0, "the ellipsis fits inside the width");
    Ok(())
}

#[test]
fn a_run_can_be_painted_and_highlighted() -> Result<()> {
    // `foreground_color` overrides the fill, `background_color` paints
    // behind the glyphs. Both were reachable from JavaScript only.
    let engine = TextEngine::with_system_fonts();
    let styled = TextStyle {
        font_size: 20.0,
        color: RgbaLinear::opaque(0.0, 0.0, 0.0),
        foreground_color: Some(RgbaLinear::opaque(1.0, 0.0, 0.0)),
        background_color: Some(RgbaLinear::opaque(0.0, 0.0, 1.0)),
        ..TextStyle::default()
    };
    let layout = engine.layout_text("painted", &styled, 400.0);
    assert!(layout.width() > 0.0, "styled text laid out empty");

    // Unset, both fall back and nothing about the layout changes -- the
    // colours are paint, not metrics.
    let plain = TextStyle {
        foreground_color: None,
        background_color: None,
        ..styled.clone()
    };
    let bare = engine.layout_text("painted", &plain, 400.0);
    assert_eq!(layout.width(), bare.width());
    Ok(())
}

#[test]
fn text_metrics_report_the_em_box() -> Result<()> {
    // Present on the JavaScript `TextMetrics` and absent here, so a caller
    // porting a measurement from the browser found two fields missing.
    let mut canvas = Canvas::new(200.0, 100.0);
    let ctx = canvas.context();
    ctx.set_font(&Font::new("Helvetica", 32.0));
    let metrics = ctx.measure_text("Hxg", None);

    assert!(metrics.width > 0.0, "measured nothing");
    assert!(metrics.em_height_ascent > 0.0, "no ascent");
    assert!(metrics.em_height_descent > 0.0, "no descent");
    // The same numbers the font bounds report, which is what the binding
    // has always answered and what Skia gives per face.
    assert_eq!(metrics.em_height_ascent, metrics.font_bounding_box_ascent);
    assert_eq!(metrics.em_height_descent, metrics.font_bounding_box_descent);
    Ok(())
}

#[test]
fn a_canvas_can_be_built_at_every_layout_the_binding_names() -> Result<()> {
    // `PixelDepth` had three variants against the twenty-six the
    // JavaScript `colorType` accepts, so a Rust caller wanting a
    // single-channel readback took four bytes a pixel and discarded three.
    let cases = [
        (PixelDepth::Uint8, 4),
        (PixelDepth::F16, 8),
        (PixelDepth::F32, 16),
        (PixelDepth::Alpha8, 1),
        (PixelDepth::Gray8, 1),
        (PixelDepth::R8UNorm, 1),
        (PixelDepth::R8G8UNorm, 2),
        (PixelDepth::A16Float, 2),
        (PixelDepth::A16UNorm, 2),
        (PixelDepth::Argb4444, 2),
        (PixelDepth::Rgb565, 2),
        (PixelDepth::Rgb888x, 4),
        (PixelDepth::Bgra8888, 4),
        (PixelDepth::Srgba8888, 4),
        (PixelDepth::N32, 4),
        (PixelDepth::Rgba1010102, 4),
        (PixelDepth::Bgra1010102, 4),
        (PixelDepth::Rgb101010x, 4),
        (PixelDepth::Bgr101010x, 4),
        (PixelDepth::R16G16Float, 4),
        (PixelDepth::R16G16UNorm, 4),
        (PixelDepth::R16G16B16A16UNorm, 8),
        (PixelDepth::F16Norm, 8),
    ];

    for (depth, bytes) in cases {
        // The width comes from Skia rather than a table here, so this is
        // the check that the mapping points at the type it claims to.
        assert_eq!(depth.bytes_per_pixel(), bytes, "{depth:?}");

        let mut canvas = Canvas::with_options(
            8.0,
            8.0,
            CanvasOptions {
                color_type: depth,
                ..CanvasOptions::default()
            },
        )
        .with_context(|| format!("building a {depth:?} canvas"))?;
        canvas.set_gpu(false);
        {
            let ctx = canvas.context();
            ctx.set_fill_style(RgbaLinear::opaque(1.0, 0.5, 0.25));
            ctx.fill_rect(0.0, 0.0, 8.0, 8.0);
        }
        let raw = canvas
            .to_buffer(ImageFormat::Raw, &EncodeOptions::default())
            .with_context(|| format!("reading back a {depth:?} canvas"))?;
        assert_eq!(raw.len(), 8 * 8 * bytes, "{depth:?} readback size");
    }
    Ok(())
}

#[test]
fn text_metrics_report_each_line_and_its_runs() -> Result<()> {
    // The JavaScript surface has reported per-line detail since before this
    // crate had a Rust text API, and this side reported a count. A caller
    // placing something against the second line of a wrapped run had to lay
    // the text out again to find out where it was.
    let mut canvas = Canvas::new(400.0, 200.0);
    let ctx = canvas.context();
    ctx.set_font(&Font::new("Helvetica", 24.0));
    ctx.set_text_wrap(true);

    let text = "one two three four five six seven eight nine";
    let metrics = ctx.measure_text(text, Some(120.0));

    assert!(metrics.line_count > 1, "the run should wrap");
    assert_eq!(
        metrics.lines.len(),
        metrics.line_count,
        "one entry per line, which is what makes the count redundant"
    );

    let mut last_end = 0;
    for (at, line) in metrics.lines.iter().enumerate() {
        assert!(line.width > 0.0, "line {at} measured empty");
        assert!(line.height > 0.0, "line {at} has no height");
        // Lines run down the page in order and cover the string in order.
        assert!(line.end_index >= line.start_index, "line {at} indices");
        assert!(
            line.start_index >= last_end,
            "line {at} starts before the one before it ended"
        );
        last_end = line.end_index;

        // The baselines are ordered as the writing systems put them:
        // hanging above alphabetic above ideographic.
        assert!(line.hanging_baseline < line.alphabetic_baseline);
        assert!(line.alphabetic_baseline < line.ideographic_baseline);
        assert!(line.ascent < line.descent, "ascent is above descent");

        // A single-family run of Latin text is one run per line, and it
        // names the face that was actually resolved.
        assert!(!line.runs.is_empty(), "line {at} has no runs");
        for run in &line.runs {
            assert!(!run.family.is_empty(), "a run with no family");
            assert!(run.width > 0.0, "a run measured empty");
            assert!(run.cap_height < run.x_height, "caps reach above x");
        }
    }

    // The last line ends where the string does, in UTF-16 units.
    assert_eq!(
        last_end,
        text.encode_utf16().count(),
        "the lines should cover the whole string"
    );

    // And an unwrapped measurement is a single line. Its width is the
    // inked bounds, which sit inside the advance width the measurement
    // reports -- the two are different questions, as they are in the
    // JavaScript shape this mirrors.
    let single = ctx.measure_text("one two", None);
    assert_eq!(single.lines.len(), 1);
    assert!(single.lines[0].width > 0.0);
    // Close to the measured width but not equal to it, and not ordered
    // either way: the line reports inked bounds and the measurement reports
    // the laid-out box, and a glyph's ink may overhang its advance where the
    // side bearing is negative.
    assert!(
        (single.lines[0].width - single.width).abs() < 5.0,
        "the ink {} and the advance {} should describe the same run",
        single.lines[0].width,
        single.width
    );
    Ok(())
}

#[test]
fn both_surfaces_measure_the_same_lines() -> Result<()> {
    // The numbers below were read off the JavaScript surface for the same
    // string, font and wrap width. Pinned here rather than compared at
    // runtime -- the binding is a separate build -- so a change to either
    // derivation shows up as the two disagreeing.
    let mut canvas = Canvas::new(400.0, 200.0);
    let ctx = canvas.context();
    ctx.set_font(&Font::new("Helvetica", 24.0));
    ctx.set_text_wrap(true);

    let metrics = ctx.measure_text(
        "one two three four five six seven eight nine",
        Some(120.0),
    );

    let from_javascript = [
        (85.71f32, 0usize, 7usize, 0.0f32),
        (104.73, 8, 18, 24.0),
        (75.35, 19, 27, 48.0),
        (64.70, 28, 33, 72.0),
        (105.74, 34, 44, 96.0),
    ];
    assert_eq!(metrics.lines.len(), from_javascript.len());

    for (at, (width, start, end, baseline)) in
        from_javascript.into_iter().enumerate()
    {
        let line = &metrics.lines[at];
        assert!(
            (line.width - width).abs() < 0.01,
            "line {at} width: {} against {width}",
            line.width
        );
        assert_eq!(line.start_index, start, "line {at} start");
        assert_eq!(line.end_index, end, "line {at} end");
        assert!(
            (line.baseline - baseline).abs() < 0.01,
            "line {at} baseline: {} against {baseline}",
            line.baseline
        );
        assert_eq!(line.runs.len(), 1, "line {at} runs");
    }
    Ok(())
}

/// The AVIF of `pages` coloured frames, and the source pixels of each.
///
/// Encoded through the public API so the test exercises what a caller does,
/// rather than reaching into the encoder.
fn avif_pages(pages: usize) -> Result<(Vec<u8>, Vec<Vec<u8>>)> {
    const COLOURS: [(f32, f32, f32); 4] = [
        (1.0, 0.0, 0.0),
        (0.0, 1.0, 0.0),
        (0.0, 0.0, 1.0),
        (1.0, 1.0, 0.0),
    ];
    let mut canvas = Canvas::new(32.0, 32.0);
    canvas.set_gpu(false);
    for page in 0..pages {
        if page > 0 {
            canvas.new_page();
        }
        let (r, g, b) = COLOURS[page % COLOURS.len()];
        let ctx = canvas.context();
        ctx.set_fill_style(RgbaLinear::opaque(r, g, b));
        ctx.fill_rect(0.0, 0.0, 32.0, 32.0);
        // A white bar, so a frame shifted by a pixel is visible as more than
        // a colour change.
        ctx.set_fill_style(RgbaLinear::opaque(1.0, 1.0, 1.0));
        ctx.fill_rect(4.0, 4.0, 8.0, 24.0);
    }

    let options = EncodeOptions {
        quality: 1.0,
        fps: (pages > 1).then_some(10.0),
        ..EncodeOptions::default()
    };
    let encoded = canvas.to_buffer(ImageFormat::Avif, &options)?;

    let mut sources = Vec::with_capacity(pages);
    for page in 0..pages {
        let raw = canvas.to_buffer(
            ImageFormat::Raw,
            &EncodeOptions {
                // Zero-based here, unlike the JavaScript `page`, which
                // counts from one.
                page: Some(page),
                ..EncodeOptions::default()
            },
        )?;
        sources.push(raw);
    }
    Ok((encoded, sources))
}

/// One decoded frame, drawn 1:1 and read back as RGBA.
fn avif_frame_pixels(image: &Image, frame: usize) -> Result<Vec<u8>> {
    let one = image.frame(frame)?;
    let mut canvas = Canvas::new(one.width() as f32, one.height() as f32);
    canvas.set_gpu(false);
    canvas.context().draw_image(&one, 0.0, 0.0);
    Ok(canvas.to_buffer(ImageFormat::Raw, &EncodeOptions::default())?)
}

#[test]
fn an_avif_can_be_read_back_from_rust() -> Result<()> {
    // Skia decodes no AVIF at all -- not this crate's animations and not its
    // stills either -- so before the decoder existed `Image::from_encoded`
    // refused every file this crate had just written.
    let (still, sources) = avif_pages(1)?;
    let image = Image::from_encoded(&still).context("a still AVIF decodes")?;

    assert_eq!(image.width(), 32);
    assert_eq!(image.height(), 32);
    assert_eq!(image.frame_count(), 1, "a still is one frame");
    assert_eq!(image.frame_delays(), &[0], "and carries no duration");

    // Quality 1.0 round-trips exactly for flat colour, which is what makes
    // an equality assertion the right one here rather than a tolerance.
    assert_eq!(
        avif_frame_pixels(&image, 0)?,
        sources[0],
        "the still's pixels survive the round trip"
    );
    Ok(())
}

#[test]
fn an_animated_avif_reports_and_returns_every_frame() -> Result<()> {
    let (encoded, sources) = avif_pages(4)?;
    let image =
        Image::from_encoded(&encoded).context("an animated AVIF decodes")?;

    assert_eq!(image.frame_count(), 4, "one frame per page");
    assert_eq!(
        image.frame_delays(),
        &[100, 100, 100, 100],
        "ten frames a second, in milliseconds as every timing here is"
    );

    // Each frame against the page it came from. A sequence is coded against
    // the frames before it, so a decoder that mishandled the references
    // would return the first frame four times -- which the differing
    // colours catch.
    //
    // Within a level rather than exactly, for everything after the key
    // frame: rav1e has no lossless mode -- its own source says so -- and
    // applies deblocking and CDEF even at a quantizer of zero, so an
    // inter-coded frame lands a level out on some pixels. The first frame
    // is a key frame and is exact, which is what the tighter assertion
    // below checks.
    for (index, source) in sources.iter().enumerate() {
        let got = avif_frame_pixels(&image, index)?;
        assert_eq!(got.len(), source.len(), "frame {index} size");
        let worst = got
            .iter()
            .zip(source)
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap_or(0);
        let allowed = match index {
            0 => 0,
            _ => 1,
        };
        assert!(
            worst <= allowed,
            "frame {index} differs by {worst}, more than the {allowed} an \
             inter-coded frame may"
        );
    }
    Ok(())
}

#[test]
fn transparency_arriving_late_still_gets_an_alpha_track() -> Result<()> {
    // The sink codes frames as they arrive rather than holding every page's
    // pixels until the end, which means it cannot look ahead to decide
    // whether the animation needs an alpha track. It starts one at the first
    // frame that is not fully opaque -- and the frames before that were
    // opaque by definition, so they are fed in as constant planes rather
    // than remembered.
    //
    // This is the case that proves it: two opaque frames, then one with a
    // hole in it. If the synthesized run were missing or the wrong length,
    // the alpha track would be short and the frames would pair up with the
    // wrong opacity.
    let mut canvas = Canvas::new(32.0, 32.0);
    canvas.set_gpu(false);
    for page in 0..3 {
        if page > 0 {
            canvas.new_page();
        }
        let ctx = canvas.context();
        ctx.set_fill_style(RgbaLinear::opaque(0.0, 0.5, 1.0));
        ctx.fill_rect(0.0, 0.0, 32.0, 32.0);
        if page == 2 {
            // The last frame alone has somewhere transparent.
            ctx.clear_rect(0.0, 0.0, 8.0, 8.0);
        }
    }

    let encoded = canvas.to_buffer(
        ImageFormat::Avif,
        &EncodeOptions {
            quality: 1.0,
            fps: Some(10.0),
            ..EncodeOptions::default()
        },
    )?;
    let image = Image::from_encoded(&encoded).context("it decodes")?;
    assert_eq!(image.frame_count(), 3);

    for frame in 0..3 {
        let pixels = avif_frame_pixels(&image, frame)?;
        let corner = pixels[3];
        match frame {
            // The synthesized frames have to come back fully opaque.
            0 | 1 => assert_eq!(corner, 255, "frame {frame} should be opaque"),
            // And the real one transparent where it was cleared.
            _ => assert_eq!(corner, 0, "frame {frame} keeps its hole"),
        }
        // Every frame is opaque away from the hole, whichever track it came
        // from, so a track that drifted out of step shows here too.
        let middle = ((16 * 32) + 16) * 4;
        assert_eq!(pixels[middle + 3], 255, "frame {frame} centre");
    }
    Ok(())
}

#[test]
fn walking_an_animation_forward_agrees_with_jumping_about() -> Result<()> {
    // Frames are coded against the ones before them, so reaching frame `n`
    // means decoding every sample up to it. That was done from zero on every
    // request, which makes playing an animation quadratic -- the documented
    // loop asks for one frame per output frame, so a 150-frame file cost
    // 11 325 sample decodes where 150 would do.
    //
    // An `Image` now keeps its decoder between calls. The risk that carries
    // is a decoder left in the wrong place, which would show as the wrong
    // picture rather than an error, so this asserts the two orders agree.
    let (encoded, _) = avif_pages(6)?;
    let image = Image::from_encoded(&encoded).context("it decodes")?;
    assert_eq!(image.frame_count(), 6);

    // Forward, which is the order that now resumes.
    let forward: Vec<Vec<u8>> = (0..6)
        .map(|at| avif_frame_pixels(&image, at))
        .collect::<Result<_>>()?;

    // Backward on a fresh image, which cannot resume from anything and so
    // rebuilds every time -- the behaviour this replaced.
    let fresh = Image::from_encoded(&encoded).context("it decodes")?;
    for at in (0..6).rev() {
        assert_eq!(
            avif_frame_pixels(&fresh, at)?,
            forward[at],
            "frame {at} differs depending on the order it was asked for"
        );
    }

    // And again forward on the same image, which is where a decoder left
    // past the end would show: the second pass has to rebuild rather than
    // carry on from frame 5.
    for (at, wanted) in forward.iter().enumerate() {
        assert_eq!(
            &avif_frame_pixels(&image, at)?,
            wanted,
            "frame {at} differs on a second pass"
        );
    }
    Ok(())
}

#[test]
fn an_animated_avif_keeps_its_transparency() -> Result<()> {
    // Alpha travels as a second coded track, which a reader has to find and
    // compose. Ignoring it yields a perfectly good opaque animation, so
    // nothing but the pixels reports the mistake.
    let mut canvas = Canvas::new(32.0, 32.0);
    canvas.set_gpu(false);
    for page in 0..3 {
        if page > 0 {
            canvas.new_page();
        }
        let ctx = canvas.context();
        ctx.set_fill_style(RgbaLinear::opaque(0.0, 0.5, 1.0));
        ctx.fill_rect(8.0, 8.0, 16.0, 16.0);
    }
    let encoded = canvas.to_buffer(
        ImageFormat::Avif,
        &EncodeOptions {
            quality: 1.0,
            fps: Some(10.0),
            ..EncodeOptions::default()
        },
    )?;

    let image = Image::from_encoded(&encoded)?;
    assert_eq!(image.frame_count(), 3);
    for frame in 0..image.frame_count() {
        let pixels = avif_frame_pixels(&image, frame)?;
        // The corner was never drawn, so it must come back transparent.
        assert_eq!(pixels[3], 0, "frame {frame} corner alpha");
        // And the drawn square opaque.
        let middle = ((16 * 32) + 16) * 4;
        assert_eq!(pixels[middle + 3], 255, "frame {frame} centre alpha");
    }
    Ok(())
}

#[test]
fn an_avif_from_another_encoder_decodes() -> Result<()> {
    // The AVIF tests around this one encode with this crate and read the
    // result back, which proves the encoder and decoder agree with each
    // other and nothing more. `foreign.avif` came out of the AVIF encoder
    // macOS ships, from a canvas this repository drew, and is the only AVIF
    // under `tests/assets` whose bytes this code did not write.
    //
    // Four solid quadrants and one off-centre white bar. A rotation permutes
    // the quadrants and a mirror moves the bar, so the pixels report more
    // than "something decoded at the right size".
    const QUADRANTS: [(usize, usize, [u8; 3], &str); 4] = [
        (128, 128, [208, 32, 32], "top left"),
        (384, 128, [32, 160, 64], "top right"),
        (128, 384, [32, 64, 208], "bottom left"),
        (384, 384, [224, 192, 32], "bottom right"),
    ];
    /// Measured at one level on these flat fields. The margin is for a
    /// different libaom, not for a wrong quadrant -- the colours are 100 or
    /// more apart in whichever channel separates any two of them.
    const TOLERANCE: i16 = 4;
    /// The fixture's side, and the stride its rows are read at.
    const SIDE: usize = 512;

    let bytes = std::fs::read("tests/assets/images/foreign.avif")
        .context("the foreign AVIF fixture is readable")?;
    let image =
        Image::from_encoded(&bytes).context("a foreign AVIF decodes")?;

    assert_eq!(image.width(), SIDE as u32);
    assert_eq!(image.height(), SIDE as u32);
    assert_eq!(image.frame_count(), 1, "a still is one frame");

    let pixels = avif_frame_pixels(&image, 0)?;
    let at = |x: usize, y: usize| {
        let start = (y * SIDE + x) * 4;
        &pixels[start..start + 4]
    };

    for (x, y, want, where_) in QUADRANTS {
        let got = at(x, y);
        for (channel, expected) in want.iter().enumerate() {
            let difference = got[channel] as i16 - *expected as i16;
            assert!(
                difference.abs() <= TOLERANCE,
                "{where_} channel {channel}: got {}, want {expected}",
                got[channel]
            );
        }
        assert_eq!(got[3], 255, "{where_} should be opaque");
    }

    assert_eq!(at(60, 30), [255, 255, 255, 255], "the bar is white");
    // Where the bar is not. Reflecting it across either axis lands here, so
    // this is the assertion a mirrored decode fails.
    let bare = at(452, 30);
    assert!(
        (bare[1] as i16 - 160).abs() <= TOLERANCE,
        "mirrored: expected the top-right quadrant, got {bare:?}"
    );
    Ok(())
}

#[test]
fn a_tiled_avif_composes_its_grid() -> Result<()> {
    // Above a few hundred pixels Apple's encoder stops writing one coded
    // image and starts writing a `grid` item that arranges several, which is
    // what a photograph off a phone is. Nothing in the old decode path read
    // one: `avif-parse` refused them by name, so this file failed outright
    // while the 512-pixel version of the same picture decoded.
    //
    // The fixture is a 2x2 grid whose tiles fall exactly on the quadrant
    // boundaries, so a tile placed in the wrong cell shows up as the wrong
    // colour rather than as a subtle seam.
    const QUADRANTS: [(usize, usize, [u8; 3], &str); 4] = [
        (256, 256, [208, 32, 32], "top left"),
        (768, 256, [32, 160, 64], "top right"),
        (256, 768, [32, 64, 208], "bottom left"),
        (768, 768, [224, 192, 32], "bottom right"),
    ];
    const TOLERANCE: i16 = 4;
    /// The composed side, twice the 512-pixel tiles it is built from.
    const SIDE: usize = 1024;

    let bytes = std::fs::read("tests/assets/images/foreign-grid.avif")
        .context("the tiled AVIF fixture is readable")?;
    let image = Image::from_encoded(&bytes).context("a tiled AVIF decodes")?;

    assert_eq!(image.width(), SIDE as u32, "the grid's output width");
    assert_eq!(image.height(), SIDE as u32, "the grid's output height");

    let pixels = avif_frame_pixels(&image, 0)?;
    let at = |x: usize, y: usize| {
        let start = (y * SIDE + x) * 4;
        &pixels[start..start + 4]
    };

    for (x, y, want, where_) in QUADRANTS {
        let got = at(x, y);
        for (channel, expected) in want.iter().enumerate() {
            let difference = got[channel] as i16 - *expected as i16;
            assert!(
                difference.abs() <= TOLERANCE,
                "{where_} channel {channel}: got {}, want {expected}",
                got[channel]
            );
        }
        assert_eq!(got[3], 255, "{where_} should be opaque");
    }

    // The seam between two tiles. A grid composed with a row's worth of
    // stride error, or with a tile written one pixel over, shows here first:
    // both sides of the boundary are flat colour, so any bleed is visible.
    assert_eq!(at(511, 256)[0], at(4, 256)[0], "left of the vertical seam");
    assert_eq!(at(512, 256)[1], at(1019, 256)[1], "right of it");

    // The asymmetric mark, which lives in the first tile only.
    assert_eq!(at(120, 60), [255, 255, 255, 255], "the bar is white");
    Ok(())
}

/// `foreign.avif` with its transform property rewritten to `property`.
///
/// Every AVIF under `tests/assets` is stored upright, so nothing there can
/// tell a decoder that applies the transform properties from one that
/// ignores them. Rather than commit near-identical files for each case, the
/// bytes that differ are patched here, where the test can say what they are.
///
/// The fixture carries an `irot`, and `imir` has the same shape -- a box of
/// nine bytes whose payload is one byte, the low bits of which are the
/// quarter turns for one and the mirror axis for the other. So retyping the
/// box in place is size-preserving, which matters: inserting one would shift
/// `mdat` and invalidate every offset in `iloc`. The property keeps its
/// position in `ipco`, so the `ipma` association still resolves, and the
/// coded image is untouched.
///
/// `sips` cannot produce such a file -- asked to rotate, it rewrites the
/// pixels and stores the result upright.
fn foreign_avif_oriented(property: &[u8; 4], value: u8) -> Result<Vec<u8>> {
    let mut bytes = std::fs::read("tests/assets/images/foreign.avif")
        .context("the foreign AVIF fixture is readable")?;

    let at = bytes
        .windows(4)
        .position(|four| four == b"irot")
        .context("the fixture carries a transform property")?;
    bytes[at..at + 4].copy_from_slice(property);
    // The payload is the byte after the four-character code.
    bytes[at + 4] = value;
    Ok(bytes)
}

#[test]
fn an_avif_is_turned_by_its_irot_property() -> Result<()> {
    // A file that carries `irot` decodes to a perfectly good picture that is
    // simply the wrong way round, and nothing in the pixels says so. The
    // quadrants are what report it: a quarter turn permutes them, so a
    // decoder that skips the property returns red where green belongs.
    //
    // Anticlockwise, per ISO/IEC 23008-12 § 6.5.10. One turn sends the
    // top-right quadrant to the top left.
    const RED: [u8; 3] = [208, 32, 32];
    const GREEN: [u8; 3] = [32, 160, 64];
    const BLUE: [u8; 3] = [32, 64, 208];
    const YELLOW: [u8; 3] = [224, 192, 32];
    const TOLERANCE: i16 = 4;
    const SIDE: usize = 512;

    // What the four quadrants hold after each number of quarter turns,
    // read top left, top right, bottom left, bottom right.
    let expected: [(u8, [[u8; 3]; 4]); 4] = [
        (0, [RED, GREEN, BLUE, YELLOW]),
        (1, [GREEN, YELLOW, RED, BLUE]),
        (2, [YELLOW, BLUE, GREEN, RED]),
        (3, [BLUE, RED, YELLOW, GREEN]),
    ];

    for (quarters, want) in expected {
        let bytes = foreign_avif_oriented(b"irot", quarters)?;
        let image = Image::from_encoded(&bytes)
            .with_context(|| format!("{quarters} quarter turns decode"))?;
        // The fixture is square, so a turn cannot be caught by the size.
        assert_eq!(image.width(), SIDE as u32);
        assert_eq!(image.height(), SIDE as u32);

        let pixels = avif_frame_pixels(&image, 0)?;
        let at = |x: usize, y: usize| {
            let start = (y * SIDE + x) * 4;
            &pixels[start..start + 4]
        };
        let corners = [at(128, 128), at(384, 128), at(128, 384), at(384, 384)];

        for (corner, expect) in corners.iter().zip(want) {
            for (channel, value) in expect.iter().enumerate() {
                let difference = corner[channel] as i16 - *value as i16;
                assert!(
                    difference.abs() <= TOLERANCE,
                    "{quarters} quarter turns, quadrant channel {channel}: \
                     got {}, want {value}",
                    corner[channel]
                );
            }
        }
    }
    Ok(())
}

#[test]
fn a_lossless_avif_returns_exactly_what_was_drawn() -> Result<()> {
    // The thing rav1e could not do at all: its lossless block is
    // unimplemented, so a quantizer of zero still filtered and a round trip
    // landed within a level rather than on it. libaom has the coding tool,
    // and this is the assertion that proves it -- equality, no tolerance.
    //
    // Lossless needs more than the flag. The picture has to reach the
    // encoder unrounded, which means full chroma and the identity matrix,
    // where the three planes are green, blue and red rather than a luma and
    // two differences. `EncodeOptions::lossless` sets both.
    const SIDE: f32 = 48.0;
    let mut canvas = Canvas::new(SIDE, SIDE);
    canvas.set_gpu(false);
    {
        let ctx = canvas.context();
        // Saturated primaries and a gradient: the primaries are where a
        // conversion rounds hardest, and the gradient is where a filter
        // would show. Both survive or neither does.
        for (at, colour) in [
            RgbaLinear::opaque(1.0, 0.0, 0.0),
            RgbaLinear::opaque(0.0, 1.0, 0.0),
            RgbaLinear::opaque(0.0, 0.0, 1.0),
        ]
        .into_iter()
        .enumerate()
        {
            ctx.set_fill_style(colour);
            ctx.fill_rect(at as f32 * 16.0, 0.0, 16.0, SIDE / 2.0);
        }
        for step in 0..48 {
            let value = step as f32 / 47.0;
            ctx.set_fill_style(RgbaLinear::opaque(value, value * 0.5, 1.0));
            ctx.fill_rect(step as f32, SIDE / 2.0, 1.0, SIDE / 2.0);
        }
    }

    let wanted =
        canvas.to_buffer(ImageFormat::Raw, &EncodeOptions::default())?;
    let lossless = canvas.to_buffer(
        ImageFormat::Avif,
        &EncodeOptions {
            lossless: true,
            ..EncodeOptions::default()
        },
    )?;

    let image = Image::from_encoded(&lossless).context("it decodes")?;
    let got = avif_frame_pixels(&image, 0)?;
    assert_eq!(got.len(), wanted.len(), "same number of pixels");

    let worst = got
        .iter()
        .zip(&wanted)
        .map(|(a, b)| a.abs_diff(*b))
        .max()
        .unwrap_or(0);
    assert_eq!(worst, 0, "lossless should mean lossless, not nearly");

    // And it is not free -- but the comparison has to be against a quality
    // that is actually lossy. Against 1.0 it is not: the quantizer is
    // already zero there, and on this drawing lossless came out *smaller*
    // (546 bytes against 560), because coding green, blue and red directly
    // costs less than converting them to BT.601 and filtering the result.
    // That is a property of flat synthetic colour, not a general rule, which
    // is why the assertion below uses a mid-dial quality instead.
    let lossy = canvas.to_buffer(
        ImageFormat::Avif,
        &EncodeOptions {
            quality: 0.5,
            ..EncodeOptions::default()
        },
    )?;
    assert!(
        lossy.len() < lossless.len(),
        "lossless should cost size against a lossy dial: {} against {}",
        lossless.len(),
        lossy.len()
    );

    // Refused where it cannot be honoured, rather than quietly overriding
    // one of the two options the caller named.
    let refused = canvas.to_buffer(
        ImageFormat::Avif,
        &EncodeOptions {
            lossless: true,
            chroma: Some(ChromaSampling::Quarter),
            ..EncodeOptions::default()
        },
    );
    assert!(refused.is_err(), "lossless and subsampled chroma disagree");

    let elsewhere = canvas.to_buffer(
        ImageFormat::Png,
        &EncodeOptions {
            lossless: true,
            ..EncodeOptions::default()
        },
    );
    assert!(elsewhere.is_err(), "png has no lossless option to set");
    Ok(())
}

#[test]
fn avif_chroma_sampling_is_the_callers_choice() -> Result<()> {
    // Full chroma is the default because this library draws canvases, and
    // subsampling ruins exactly what a canvas is good at. The measurement
    // that settled it: on flat UI with text, 4:2:0 came out 22 dB worse and
    // produced a *larger* file, while on a photograph it was 30% smaller for
    // 7 dB.
    //
    // Alternating single-pixel stripes of two saturated colours. One wide
    // edge would not do: 4:2:0 averages chroma over two-by-two cells aligned
    // to even columns, so a split at x = 32 falls exactly on a cell boundary
    // and survives untouched -- the first version of this test measured zero
    // difference for that reason. Stripes a pixel wide guarantee every cell
    // straddles one.
    const SIDE: f32 = 64.0;
    let drawing = |canvas: &mut Canvas| {
        let ctx = canvas.context();
        for column in 0..SIDE as usize {
            match column % 2 {
                0 => ctx.set_fill_style(RgbaLinear::opaque(1.0, 0.0, 0.0)),
                _ => ctx.set_fill_style(RgbaLinear::opaque(0.0, 1.0, 0.0)),
            }
            ctx.fill_rect(column as f32, 0.0, 1.0, SIDE);
        }
    };

    let encoded = |chroma: ChromaSampling| -> Result<Vec<u8>> {
        let mut canvas = Canvas::new(SIDE, SIDE);
        canvas.set_gpu(false);
        drawing(&mut canvas);
        Ok(canvas.to_buffer(
            ImageFormat::Avif,
            &EncodeOptions {
                quality: 1.0,
                chroma: Some(chroma),
                ..EncodeOptions::default()
            },
        )?)
    };

    // The drawing as it was made, to measure each encode against.
    let mut source = Canvas::new(SIDE, SIDE);
    source.set_gpu(false);
    drawing(&mut source);
    let wanted =
        source.to_buffer(ImageFormat::Raw, &EncodeOptions::default())?;

    // Total absolute error over every channel, against the drawing.
    let error = |bytes: &[u8]| -> Result<u64> {
        let image = Image::from_encoded(bytes).context("it decodes")?;
        let got = avif_frame_pixels(&image, 0)?;
        Ok(got
            .iter()
            .zip(&wanted)
            .map(|(a, b)| u64::from(a.abs_diff(*b)))
            .sum())
    };

    let full = error(&encoded(ChromaSampling::Full)?)?;
    let quarter = error(&encoded(ChromaSampling::Quarter)?)?;

    // Not a ratio: the point is that the option reaches the encoder and
    // costs what it is documented to cost. Full chroma at quality 1.0
    // reproduces two flat fields almost exactly; 4:2:0 cannot, because the
    // chroma either side of the seam is averaged before it is ever coded.
    assert!(
        quarter > full,
        "4:2:0 should blur the edge that 4:4:4 keeps: {quarter} against \
         {full}"
    );

    // And the option is refused where it means nothing, rather than being
    // quietly dropped -- the mistake it replaces is a caller believing a PNG
    // was subsampled.
    let mut canvas = Canvas::new(SIDE, SIDE);
    canvas.set_gpu(false);
    let refused = canvas.to_buffer(
        ImageFormat::Png,
        &EncodeOptions {
            chroma: Some(ChromaSampling::Quarter),
            ..EncodeOptions::default()
        },
    );
    assert!(refused.is_err(), "png does not choose its chroma sampling");
    Ok(())
}

#[test]
fn an_avif_is_read_in_the_space_its_profile_names() -> Result<()> {
    // `foreign-p3.avif` is the same drawing as `foreign.avif`, converted to
    // Display P3 by `sips` and carrying that profile in a `colr` box of type
    // `prof`. The coded values are therefore P3 numbers, not sRGB ones, and
    // a decoder that discards the profile hands back a picture whose colours
    // are wrong in a way nothing reports -- it is a valid image of the wrong
    // hue.
    //
    // Drawing it onto an sRGB canvas converts it back, so the quadrants
    // should return to the values the drawing started from. Measured with
    // the profile deliberately ignored, the top-left quadrant reads
    // 191, 52, 45 against the 208, 32, 32 it was drawn with -- twenty levels
    // out, where this allows six.
    const QUADRANTS: [(usize, usize, [u8; 3], &str); 4] = [
        (128, 128, [208, 32, 32], "top left"),
        (384, 128, [32, 160, 64], "top right"),
        (128, 384, [32, 64, 208], "bottom left"),
        (384, 384, [224, 192, 32], "bottom right"),
    ];
    /// Wider than the flat-field tolerance elsewhere, because this crosses
    /// two colour spaces rather than one lossy encode.
    const TOLERANCE: i16 = 6;
    const SIDE: usize = 512;

    let bytes = std::fs::read("tests/assets/images/foreign-p3.avif")
        .context("the Display P3 fixture is readable")?;
    let image =
        Image::from_encoded(&bytes).context("a profiled AVIF decodes")?;
    assert_eq!(image.width(), SIDE as u32);

    let pixels = avif_frame_pixels(&image, 0)?;
    let at = |x: usize, y: usize| {
        let start = (y * SIDE + x) * 4;
        &pixels[start..start + 4]
    };

    for (x, y, want, where_) in QUADRANTS {
        let got = at(x, y);
        for (channel, expected) in want.iter().enumerate() {
            let difference = got[channel] as i16 - *expected as i16;
            assert!(
                difference.abs() <= TOLERANCE,
                "{where_} channel {channel}: got {}, want {expected} -- \
                 the profile looks unread",
                got[channel]
            );
        }
    }
    Ok(())
}

#[test]
fn an_avif_is_converted_by_the_matrix_its_colr_names() -> Result<()> {
    // The `colr` box's nclx form says which matrix mixed the planes. This
    // crate's own conversion is BT.601, which is also libavif's default and
    // what Apple writes, so every fixture here happens to agree with it --
    // and a decoder that ignored the field entirely would pass all of them.
    //
    // Rewriting the code point to BT.709 does not change one coded byte, so
    // any difference in the decoded pixels is the field being read. The
    // matrix only mixes chroma, so a grey stays grey and the saturated
    // quadrants are where it shows.
    const SIDE: usize = 512;
    /// ITU-T H.273 Table 4. Named rather than written as 1 and 6 because
    /// the point of the test is which row of that table is in force.
    use avif_serialize::constants::MatrixCoefficients;

    let recoded = |matrix: MatrixCoefficients| -> Result<Vec<u8>> {
        let mut bytes = std::fs::read("tests/assets/images/foreign.avif")
            .context("the foreign AVIF fixture is readable")?;
        let colr = bytes
            .windows(4)
            .position(|four| four == b"colr")
            .context("the fixture carries a colr box")?;
        assert_eq!(
            &bytes[colr + 4..colr + 8],
            b"nclx",
            "the fixture states code points rather than a profile"
        );
        // After the colour type, the primaries and the transfer function.
        let at = colr + 4 + 4 + 2 + 2;
        bytes[at..at + 2].copy_from_slice(&(matrix as u16).to_be_bytes());
        Ok(bytes)
    };

    let sample = |bytes: &[u8]| -> Result<[u8; 3]> {
        let image = Image::from_encoded(bytes).context("it decodes")?;
        let pixels = avif_frame_pixels(&image, 0)?;
        // The top-left quadrant, drawn as a saturated red.
        let start = (128 * SIDE + 128) * 4;
        Ok([pixels[start], pixels[start + 1], pixels[start + 2]])
    };

    let as_written = sample(&recoded(MatrixCoefficients::Bt601)?)?;
    let as_709 = sample(&recoded(MatrixCoefficients::Bt709)?)?;

    // The file really was coded BT.601, so reading it as such reproduces
    // the red the quadrant was drawn with.
    const DRAWN_RED: i16 = 208;
    const TOLERANCE: i16 = 4;
    assert!(
        (as_written[0] as i16 - DRAWN_RED).abs() <= TOLERANCE,
        "BT.601 red: got {}, want {DRAWN_RED}",
        as_written[0]
    );

    // And reading the same bytes under BT.709 must not agree, or the field
    // is being ignored. The two matrices differ most in how much of red and
    // blue they charge to luma, so a saturated red is where they part.
    assert_ne!(
        as_written, as_709,
        "the matrix code point changed nothing, so it is unread"
    );
    Ok(())
}

#[test]
fn an_avif_opens_out_the_narrow_range_its_colr_declares() -> Result<()> {
    // `colr`'s nclx form carries a full-range flag beside the matrix, and it
    // was ignored: every AVIF met so far sets it, including everything this
    // crate writes, so nothing in the suite could tell. A broadcast-range
    // file read as full leaves black at 16 rather than 0 and white at 235
    // rather than 255 -- a washed-out picture rather than a visible error.
    //
    // Clearing the flag on a file whose samples really are full range is not
    // a picture of anything; the point is that the field reaches the
    // conversion, which it can only do by changing the result. The direction
    // is checkable though: opening out a range that was already open pushes
    // the extremes apart, so a bright quadrant gets brighter.
    const SIDE: usize = 512;
    /// The byte after the three code points; the top bit is the flag.
    const FULL_RANGE: u8 = 0b1000_0000;

    let read = |full: bool| -> Result<Vec<u8>> {
        let mut bytes = std::fs::read("tests/assets/images/foreign.avif")
            .context("the foreign AVIF fixture is readable")?;
        let colr = bytes
            .windows(4)
            .position(|four| four == b"colr")
            .context("the fixture carries a colr box")?;
        assert_eq!(&bytes[colr + 4..colr + 8], b"nclx", "code points, not ICC");
        let at = colr + 4 + 4 + 2 + 2 + 2;
        bytes[at] = match full {
            true => FULL_RANGE,
            false => 0,
        };

        let image = Image::from_encoded(&bytes).context("it decodes")?;
        avif_frame_pixels(&image, 0)
    };

    let full = read(true)?;
    let narrow = read(false)?;
    let at = |pixels: &[u8], x: usize, y: usize| {
        let start = (y * SIDE + x) * 4;
        [pixels[start], pixels[start + 1], pixels[start + 2]]
    };

    // The flag has to reach the conversion at all.
    assert_ne!(
        at(&full, 128, 128),
        at(&narrow, 128, 128),
        "the range flag changed nothing, so it is unread"
    );

    // And in the right direction. The yellow quadrant is the brightest of
    // the four, so stretching 16..235 onto 0..255 has to lift its luma.
    let bright_full = at(&full, 384, 384);
    let bright_narrow = at(&narrow, 384, 384);
    assert!(
        bright_narrow[0] >= bright_full[0]
            && bright_narrow[1] >= bright_full[1],
        "opening the range out should not darken the brightest quadrant: \
         {bright_narrow:?} against {bright_full:?}"
    );
    Ok(())
}

#[test]
fn an_avif_is_flipped_by_its_imir_property() -> Result<()> {
    // `imir` axis 0 exchanges the top and bottom halves, axis 1 the left and
    // right (ISO/IEC 23008-12 § 6.5.12). The quadrants catch either, and the
    // white bar separates a flip from a rotation -- a turn moves it to a
    // different edge, a mirror keeps it on the top edge and slides it across.
    const TOLERANCE: i16 = 4;
    const SIDE: usize = 512;
    /// Axis, then what the top-left quadrant holds once the flip is applied.
    const CASES: [(u8, [u8; 3], &str); 2] = [
        (0, [32, 64, 208], "top and bottom exchanged, so blue rises"),
        (
            1,
            [32, 160, 64],
            "left and right exchanged, so green crosses",
        ),
    ];

    for (axis, want, why) in CASES {
        // The turn property is retyped, so this measures the mirror alone.
        let bytes = foreign_avif_oriented(b"imir", axis)?;
        let image = Image::from_encoded(&bytes)
            .with_context(|| format!("axis {axis} decodes"))?;
        let pixels = avif_frame_pixels(&image, 0)?;
        let at = |x: usize, y: usize| {
            let start = (y * SIDE + x) * 4;
            &pixels[start..start + 4]
        };

        let corner = at(128, 128);
        for (channel, value) in want.iter().enumerate() {
            let difference = corner[channel] as i16 - *value as i16;
            assert!(
                difference.abs() <= TOLERANCE,
                "{why}: channel {channel} got {}, want {value}",
                corner[channel]
            );
        }

        // The bar sits at the top left when upright. A horizontal exchange
        // moves it to the top right; a vertical one to the bottom left.
        let bar = match axis {
            0 => at(60, SIDE - 30),
            _ => at(SIDE - 60, 30),
        };
        assert_eq!(bar, [255, 255, 255, 255], "{why}: the bar moved with it");
    }
    Ok(())
}
