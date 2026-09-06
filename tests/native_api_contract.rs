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
    let lit = pixels
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|px| px[3] > 0)
        .count();
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
        Ok(pixels
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|px| px[0] > 64)
            .count())
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

/// One system `FontMgr` shared across layouts still measures each axis
/// position on its own.
///
/// `collection_for` called `FontMgr::new()` for every layout carrying
/// `font_variations`. That is not a handle to a singleton -- it stands up a
/// manager over the installed fonts, and it measured 9.0 ms of a 9.6 ms
/// layout on macOS, so a Rust caller drawing variable text in a loop paid it
/// per draw. The engine holds one now.
///
/// What that could plausibly break is a manager carrying state from one
/// layout into the next, so this asks for the same axis twice with a
/// different one in between and requires the two answers to be equal exactly.
/// A third layout that drifts is the failure this exists for; the
/// heavier-than-lighter check alone would not see it, because both readings
/// would still be lighter than 700.
///
/// The signal is inked pixels rather than advance width. Oswald's advances do
/// not move with `wght` -- 200 and 700 both measure 296.35 for the same
/// string -- so a width assertion would fail on a correct build.
#[test]
fn a_shared_font_manager_keeps_each_axis_position_apart() -> Result<()> {
    let font_bytes =
        std::fs::read("tests/assets/fonts/Oswald/Oswald-VariableFont_wght.ttf")
            .context("oswald-vf")?;
    let fonts = FontLibrary::new();
    fonts.register_font_from_data("Oswald", &font_bytes)?;
    let engine = TextEngine::new(&fonts);

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
        Ok(pixels
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|px| px[0] > 64)
            .count())
    };

    let light = ink_at(200.0)?;
    let heavy = ink_at(700.0)?;
    let light_again = ink_at(200.0)?;

    assert!(light > 0, "the light weight rendered");
    assert!(
        heavy > light,
        "700 should ink more than 200: got {heavy} against {light}",
    );
    assert_eq!(
        light, light_again,
        "the same axis position drawn twice, with another in between, should \
         ink the same",
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
        let inked = pixels
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|px| px[3] > 0)
            .count();
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
/// surfaces now go through one parser.
///
/// **Each figure below is computed from CSS Color 4's own conversion code,
/// not read back from either surface.** That the two surfaces also agree
/// byte for byte is checked, and is what this test is named for -- but it is
/// not evidence that either is right. They share a parser, so a defect in it
/// produces identical bytes on both sides and an agreement test passes.
///
/// That is not hypothetical. The `lab()` row here asserted `[189, 119, 198]`,
/// which is exactly what a D65-direct conversion gives; CSS Color 4 requires
/// D50 followed by a Bradford adaptation, and the answer is `[193, 117, 199]`.
/// The expectation had been taken from the other surface, so it recorded the
/// defect instead of catching it.
///
/// The Lab rows are therefore chosen off the neutral axis, where the two
/// conversions differ. `lab(50% 0 0)` is kept as the case that *cannot*
/// discriminate -- both give `[119, 119, 119]` -- so that a later reader
/// adding greys can see why they prove nothing here.
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
        // Lab, D50 reference adapted to D65. Each of these differs from the
        // unadapted conversion; the deltas on red are 4, 32 and 26.
        ("lab(60% 40 -30)", [193, 117, 199, 255]),
        ("lab(30% 30 -60)", [63, 56, 167, 255]),
        ("lab(70% -30 -30)", [26, 188, 225, 255]),
        // The maintainer's own case in csscolorparser-rs#14, which the
        // adapted conversion reproduces and the unadapted one misses.
        ("lab(44.36% 36.05 -58.99)", [118, 84, 205, 255]),
        // Polar form of the same conversion.
        ("lch(50% 40 30)", [178, 93, 87, 255]),
        // On the neutral axis the two conversions coincide exactly. Kept to
        // record that this row proves nothing about the white point.
        ("lab(50% 0 0)", [119, 119, 119, 255]),
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
            // The rest pattern, which is the contract `docs/rust.md`
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
    //
    // A font out of `tests/assets`, not a system one. This pinned Helvetica
    // and Helvetica is a macOS font: on Linux fontconfig substitutes Nimbus
    // Sans, whose metrics are close but not equal, and the first line
    // measured 84.04 against the 85.71 written here. The test was not
    // wrong about the two surfaces agreeing -- they still did -- it was
    // asserting numbers that only exist on one platform, and it took until
    // this branch ran off a Mac for anything to say so.
    let fonts = FontLibrary::new();
    fonts.register_font_from_path(
        "MeoTestSans",
        "tests/assets/fonts/Raleway/Raleway-VariableFont_wght.ttf",
    )?;

    let mut canvas = Canvas::new(400.0, 200.0);
    let ctx = canvas.context();
    ctx.set_font(&Font::new("MeoTestSans", 24.0));
    ctx.set_text_wrap(true);

    let metrics = ctx.measure_text(
        "one two three four five six seven eight nine",
        Some(120.0),
    );

    let from_javascript = [
        (91.59f32, 0usize, 7usize, -0.18f32),
        (111.41, 8, 18, 27.82),
        (75.47, 19, 27, 55.82),
        (66.78, 28, 33, 83.82),
        (111.34, 34, 44, 111.82),
    ];
    assert_eq!(metrics.lines.len(), from_javascript.len());

    // `width` is read only where the numbers were taken; see below
    for (at, (_width, start, end, baseline)) in
        from_javascript.into_iter().enumerate()
    {
        let line = &metrics.lines[at];
        // Widths only where the numbers were taken. An advance is what the
        // shaper produced, and the shaper is not the same on every platform:
        // this same font file, wrapped at this same width, measures 91.59 on
        // macOS through CoreText and 89.59 on Linux through FreeType. Where a
        // line *breaks* and where its baseline *sits* do not move -- the
        // breaks came out identical on both, and the baselines are the font's
        // own metrics read out of the file -- so those are asserted
        // everywhere and are what actually catches the two surfaces drifting
        // apart.
        #[cfg(target_os = "macos")]
        assert!(
            (line.width - _width).abs() < 0.01,
            "line {at} width: {} against {_width}",
            line.width
        );
        assert!(
            line.width <= 120.0,
            "line {at} is {} wide, past the 120 it was wrapped to",
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
    // frame: this sequence is coded lossily, and deblocking and CDEF run
    // even at a quantizer of zero, so an inter-coded frame lands a level
    // out on some pixels. The first frame is a key frame and is exact,
    // which is what the tighter assertion below checks. Coding without
    // loss is a separate tool -- `lossless` reaches it -- rather than
    // somewhere the quantizer goes.
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
fn apng_timings_come_from_the_chunks_not_the_pixels() -> Result<()> {
    // `frame_delays` runs on every image this crate constructs, and for an
    // APNG it used to inflate the whole animation to reach the timings --
    // every frame's pixels alive at once to produce one integer each. Sixty
    // frames of 960x540 is about 248 MB held to answer with sixty numbers.
    //
    // The timings live in the `fcTL` chunks, which a walk reaches without
    // decoding anything. What that walk has to get right is the *per-frame*
    // value rather than the rate it was asked for: 30fps does not divide
    // into whole milliseconds, so the encoder hands out the remainder and
    // the frames differ from each other. A reader recomputing from the rate
    // would return four equal numbers and look correct.
    let mut canvas = Canvas::new(16.0, 16.0);
    canvas.set_gpu(false);
    for page in 0..4 {
        if page > 0 {
            canvas.new_page();
        }
        let ctx = canvas.context();
        ctx.set_fill_style(RgbaLinear::opaque(page as f32 / 4.0, 0.2, 0.6));
        ctx.fill_rect(0.0, 0.0, 16.0, 16.0);
    }

    let uneven = canvas.to_buffer(
        ImageFormat::Apng,
        &EncodeOptions {
            fps: Some(30.0),
            ..EncodeOptions::default()
        },
    )?;
    let image = Image::from_encoded(&uneven).context("it decodes")?;
    assert_eq!(
        image.frame_delays(),
        &[33, 34, 33, 33],
        "the delays each frame was written with, not the rate"
    );

    // And an explicit per-frame list comes back as it went in.
    let named = canvas.to_buffer(
        ImageFormat::Apng,
        &EncodeOptions {
            frame_delays: vec![10, 20, 30, 40],
            ..EncodeOptions::default()
        },
    )?;
    let image = Image::from_encoded(&named).context("it decodes")?;
    assert_eq!(image.frame_delays(), &[10, 20, 30, 40]);
    Ok(())
}

#[test]
fn walking_an_apng_forward_agrees_with_jumping_about() -> Result<()> {
    // The same shape as the AVIF test below, for the other decoder that
    // codes each frame against what came before. APNG kept rebuilding its
    // reader and inflating from zero on every call.
    //
    // Resuming carries a composited canvas as well as a reader, and the
    // canvas handed to a caller differs from the one carried on: disposal
    // happens *after* a frame is shown, so the picture is taken before it
    // and the state after. Getting that backwards would show as a frame
    // carrying the previous one's leftovers, which is a wrong picture
    // rather than an error -- hence comparing the two orders.
    let mut canvas = Canvas::new(24.0, 24.0);
    canvas.set_gpu(false);
    for page in 0..5 {
        if page > 0 {
            canvas.new_page();
        }
        let ctx = canvas.context();
        ctx.set_fill_style(RgbaLinear::opaque(0.1, 0.1, 0.1));
        ctx.fill_rect(0.0, 0.0, 24.0, 24.0);
        // A square that moves, so each frame differs from its neighbours.
        ctx.set_fill_style(RgbaLinear::opaque(1.0, 0.0, 0.0));
        ctx.fill_rect(page as f32 * 4.0, 4.0, 6.0, 6.0);
    }
    let encoded = canvas.to_buffer(
        ImageFormat::Apng,
        &EncodeOptions {
            fps: Some(10.0),
            ..EncodeOptions::default()
        },
    )?;

    let image = Image::from_encoded(&encoded).context("it decodes")?;
    assert_eq!(image.frame_count(), 5);
    let forward: Vec<Vec<u8>> = (0..5)
        .map(|at| avif_frame_pixels(&image, at))
        .collect::<Result<_>>()?;

    // Backward on a fresh image, which cannot resume and so rebuilds.
    let fresh = Image::from_encoded(&encoded).context("it decodes")?;
    for at in (0..5).rev() {
        assert_eq!(
            avif_frame_pixels(&fresh, at)?,
            forward[at],
            "frame {at} differs depending on the order it was asked for"
        );
    }

    // And a second forward pass on the image that has already walked once,
    // which has to notice its reader is past the frame being asked for.
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

/// Every export entry point that can reach Metal wraps its work in
/// `gpu::autorelease`.
///
/// Structural rather than behavioural, and deliberately so. What this guards
/// is a memory leak of about 3.9 MB a canvas -- the whole surface -- on
/// synchronous GPU export, and the only direct evidence of it is RSS climbing
/// over hundreds of exports on a machine with Metal. That is a measurement
/// this suite cannot make: it needs a GPU, it needs minutes, and a threshold
/// loose enough not to flap is loose enough to miss a partial regression.
///
/// The invariant underneath is exact, though. Metal's `objc` allocations are
/// autoreleased, nothing drains a pool on either node's main thread or a rayon
/// worker, and so an entry point that omits the wrapper leaks for the life of
/// the process. `toBuffer` and `save` were wrapped from the start; their
/// synchronous twins were not, and the asymmetry survived review because both
/// pairs read alike at the call site.
///
/// So the file is read and each of the four is checked for the call between
/// its own signature and the next. Cheap, deterministic, needs no GPU, and it
/// fails on exactly the edit that reintroduced the bug.
#[test]
fn every_export_entry_point_holds_an_autorelease_pool() {
    const SOURCE: &str = include_str!("../src/node/canvas.rs");
    const ENTRY_POINTS: [&str; 4] =
        ["toBuffer", "toBufferSync", "save", "saveSync"];

    // Signatures in definition order, so a body runs to the next one.
    let mut bounds: Vec<(&str, usize)> = ENTRY_POINTS
        .iter()
        .map(|name| {
            let at = SOURCE
                .find(&format!("pub fn {name}("))
                .unwrap_or_else(|| panic!("{name} is no longer in canvas.rs"));
            (*name, at)
        })
        .collect();
    bounds.sort_by_key(|(_, at)| *at);

    // Either the pool directly, or the off-thread helper that opens one and
    // adds a panic barrier the synchronous pair does not need. The
    // asynchronous entry points went through the second once `rayon` aborts
    // became catchable, so checking only for the literal call started
    // failing on a change that kept the invariant -- which is the failure
    // mode of a structural test, and the reason the helper is asserted to
    // carry the pool just below.
    for (index, (name, from)) in bounds.iter().enumerate() {
        let to = bounds
            .get(index + 1)
            .map_or(SOURCE.len(), |(_, next)| *next);
        let body = &SOURCE[*from..to];
        assert!(
            body.contains("gpu::autorelease(")
                || body.contains("encoded_offthread("),
            "{name} reaches neither gpu::autorelease nor encoded_offthread, \
             so Metal's autoreleased allocations accumulate for the life of \
             the process"
        );
    }

    let helper = SOURCE
        .find("fn encoded_offthread")
        .map(|at| &SOURCE[at..])
        .unwrap_or_else(|| {
            panic!("encoded_offthread is no longer in canvas.rs")
        });
    assert!(
        helper[..helper.find("\npub type").unwrap_or(helper.len())]
            .contains("gpu::autorelease("),
        "encoded_offthread stopped opening an autorelease pool, so the two \
         entry points that rely on it no longer have one"
    );
}

/// A right-to-left paragraph lays out from the right.
///
/// The direction had no Rust field at all, so every paragraph built from
/// this side was left-to-right whatever it contained. Digits are the
/// discriminator: they are bidi-neutral, so nothing in the text itself
/// decides, and the base direction is the only thing that can. Latin or
/// Arabic would have been resolved from the characters and passed even with
/// the field ignored.
#[test]
fn a_paragraph_lays_out_in_the_direction_it_was_given() -> Result<()> {
    const WIDTH: f32 = 400.0;

    let laid_out = |direction| {
        let style = TextStyle {
            font_size: 24.0,
            // Start, not Left: this is the alignment whose meaning the base
            // direction decides, so it is where a dropped field shows.
            align: TextAlign::Start,
            direction,
            ..TextStyle::default()
        };
        let engine = TextEngine::with_system_fonts();
        let mut builder = engine.paragraph_builder(&style);
        builder.add_text("12 34");
        let paragraph = builder.build(WIDTH);
        paragraph.rects_for_range(
            0..5,
            RectHeightStyle::Tight,
            RectWidthStyle::Tight,
        )
    };

    let ltr = laid_out(TextDirection::LeftToRight);
    let rtl = laid_out(TextDirection::RightToLeft);
    assert!(!ltr.is_empty() && !rtl.is_empty(), "both should lay out");

    let left_edge = |boxes: &[TextBox]| {
        boxes.iter().fold(f32::MAX, |at, b| at.min(b.rect.left))
    };
    let right_edge = |boxes: &[TextBox]| {
        boxes.iter().fold(0.0f32, |at, b| at.max(b.rect.right))
    };

    // Left-to-right starts at the origin; right-to-left ends at the far edge.
    // Asserted against the box rather than against each other, so a layout
    // that merely shifted a little would still fail.
    assert!(
        left_edge(&ltr) < 1.0,
        "left-to-right should start at the origin, got {}",
        left_edge(&ltr)
    );
    assert!(
        right_edge(&rtl) > WIDTH - 1.0,
        "right-to-left should end at the far edge, got {}",
        right_edge(&rtl)
    );

    // What is deliberately *not* asserted: that the boxes come back marked
    // right-to-left. They do not, and should not. European digits take a
    // left-to-right embedding level inside a right-to-left paragraph
    // (UAX #9), so the run reads left to right while the line it sits on
    // starts from the right -- which is the whole distinction between a base
    // direction and a run's own. Asserting otherwise passed nothing and
    // claimed the opposite of the standard.
    Ok(())
}

/// The width axis reaches the typeface Skia matches.
///
/// `TextStyle` carried weight and slant and hardcoded `Width::NORMAL` at both
/// places it built an `SkFontStyle`, so a Rust caller asking for condensed got
/// the regular face back and no indication it had been ignored.
///
/// Skia matches the nearest width a family ships rather than synthesizing one,
/// so observing this needs a family that ships more than one -- and nothing in
/// `tests/assets` does, since Oswald and Raleway are both `wght`-only variable
/// fonts. Which family that is depends on the machine, so it is discovered
/// rather than named.
///
/// It is discovered through `FontLibrary`, and that detail is the test. An
/// earlier version probed by laying text out at two widths and using whichever
/// family measured differently -- which is the same signal the assertion then
/// checked, so with the axis hardcoded no family discriminated, the search
/// found nothing, and the test skipped and passed. It reported success on the
/// bug it existed to catch. `family_details` reads what the font manager says
/// the family offers and cannot be affected by the axis under test, so a
/// machine that has such a family now fails when the axis is dropped.
#[test]
fn a_condensed_face_is_selected_when_the_family_has_one() -> Result<()> {
    let fonts = FontLibrary::new();
    // Asked of the font manager, not of a layout: this must stay independent
    // of the thing being asserted.
    let multi_width = fonts.installed_families().into_iter().find(|family| {
        fonts
            .family_details(family)
            .is_some_and(|detail| detail.widths.len() > 1)
    });

    let Some(family) = multi_width else {
        eprintln!(
            "no installed family ships more than one width, so this machine \
             cannot observe the axis -- the assertion was skipped, not met"
        );
        return Ok(());
    };

    let engine = TextEngine::with_system_fonts();
    let measured = |stretch| {
        let style = TextStyle {
            font_families: vec![family.clone()],
            font_size: 24.0,
            stretch,
            ..TextStyle::default()
        };
        engine
            .layout_text("Hamburgefonstiv", &style, 1000.0)
            .max_intrinsic_width()
    };

    let normal = measured(FontStretch::Normal);
    let condensed = measured(FontStretch::UltraCondensed);
    assert!(normal > 0.0, "{family} should lay out at all");
    assert!(
        condensed < normal,
        "{family} offers a condensed face, so ultra-condensed should measure \
         narrower than normal: got {condensed} against {normal}"
    );
    Ok(())
}

/// A variable font is instanced under the name it was registered under.
///
/// The instance built for a `font_variations` request is registered into a
/// dynamic provider, and it has to go in under a name the subsequent lookup
/// will search by -- which is the family the caller asked for, not the name
/// inside the font file. Those differ whenever a caller registers under an
/// alias, and the two used to be conflated: the alias was recovered by
/// matching the typeface's intrinsic `family_name()` against the registered
/// list, so a mismatch found nothing, filed the instance under the intrinsic
/// name, and left the request falling through to the uninstanced face.
///
/// Silent, and only visible as the wrong weight on the page. Advance width
/// is what catches it here: Oswald's `wght` moves it about 24% across the
/// axis, so an ignored axis shows up as two identical measurements at 200
/// and 700 rather than as an error. Registering after the engine is built is
/// covered in the same test because it exercises the same lookup, and the
/// documentation on `register_font_from_data` claimed for a long time that
/// it could not work.
#[test]
fn a_variable_font_is_instanced_under_the_name_it_was_given() -> Result<()> {
    const TTF: &str = "tests/assets/fonts/Oswald/Oswald-VariableFont_wght.ttf";

    let width_at = |engine: &TextEngine, family: &str, wght: f32| {
        let style = TextStyle {
            font_families: vec![family.to_string()],
            font_size: 64.0,
            font_variations: vec![FontVariation::new(FontAxisTag::WGHT, wght)],
            ..TextStyle::default()
        };
        engine
            .layout_text("Hamburgefonstiv", &style, 4000.0)
            .max_intrinsic_width()
    };

    // Registered under its own family name, before the engine: the control,
    // and the numbers the other two must reproduce.
    let plain = FontLibrary::new();
    plain.register_font_from_path("Oswald", TTF)?;
    let engine = TextEngine::new(&plain);
    let (thin, thick) = (
        width_at(&engine, "Oswald", 200.0),
        width_at(&engine, "Oswald", 700.0),
    );
    assert!(
        thick > thin + 1.0,
        "the axis must move the advance for this test to mean anything: \
         200 gave {thin}, 700 gave {thick}"
    );

    // Registered under an alias unlike the name inside the file.
    let aliased = FontLibrary::new();
    aliased.register_font_from_path("OswaldAlias", TTF)?;
    let engine = TextEngine::new(&aliased);
    assert_eq!(
        (
            width_at(&engine, "OswaldAlias", 200.0),
            width_at(&engine, "OswaldAlias", 700.0)
        ),
        (thin, thick),
        "an alias unlike the font's own family name dropped the axis"
    );

    // Registered after the engine was built.
    let late = FontLibrary::new();
    let engine = TextEngine::new(&late);
    late.register_font_from_path("Oswald", TTF)?;
    assert_eq!(
        (
            width_at(&engine, "Oswald", 200.0),
            width_at(&engine, "Oswald", 700.0)
        ),
        (thin, thick),
        "a font registered after the engine was built dropped the axis"
    );

    // One face reachable from two requested names, which resolving per
    // family makes reachable twice -- the one behavioural difference from
    // resolving the whole list at once, so it is pinned rather than assumed
    // harmless. Both orders, because a fallback list is ordered and an
    // instance filed under the first name must not shadow the second.
    let both = FontLibrary::new();
    both.register_font_from_path("Oswald", TTF)?;
    both.register_font_from_path("OswaldAlias", TTF)?;
    let engine = TextEngine::new(&both);
    for families in [["OswaldAlias", "Oswald"], ["Oswald", "OswaldAlias"]] {
        let measured = |wght: f32| {
            let style = TextStyle {
                font_families: families
                    .iter()
                    .map(|family| family.to_string())
                    .collect(),
                font_size: 64.0,
                font_variations: vec![FontVariation::new(
                    FontAxisTag::WGHT,
                    wght,
                )],
                ..TextStyle::default()
            };
            engine
                .layout_text("Hamburgefonstiv", &style, 4000.0)
                .max_intrinsic_width()
        };
        assert_eq!(
            (measured(200.0), measured(700.0)),
            (thin, thick),
            "a face reachable from two requested names measured differently"
        );
    }
    Ok(())
}

/// Every `fcTL` rectangle in an APNG: x, y, width, height, dispose, blend.
///
/// Read out of the container rather than through a decoder, because the
/// question is what was written rather than what a reader makes of it.
fn frame_controls_of_apng(bytes: &[u8]) -> Vec<(u32, u32, u32, u32, u8, u8)> {
    let word = |at: usize| {
        u32::from_be_bytes([
            bytes[at],
            bytes[at + 1],
            bytes[at + 2],
            bytes[at + 3],
        ])
    };
    let mut found = Vec::new();
    // Past the eight-byte signature.
    let mut at = 8;
    while at + 8 <= bytes.len() {
        let length = word(at) as usize;
        let body = at + 8;
        if &bytes[at + 4..body] == b"fcTL" {
            found.push((
                // Sequence number first, then size, then offset.
                word(body + 12),
                word(body + 16),
                word(body + 4),
                word(body + 8),
                bytes[body + 24],
                bytes[body + 25],
            ));
        }
        // Length, type, data, CRC.
        at = body + length + 4;
    }
    found
}

/// An APNG carries only the rectangle each frame changed, and decodes back
/// to the pages that went in.
///
/// The first half is the saving; the second is what makes it allowed. A
/// frame that says it covers a rectangle is asserting that everything
/// outside it is unchanged, so an off-by-one in the diff, or a blend mode
/// that composites where it should replace, shows up as a decoded frame
/// that no longer matches the page it came from.
#[test]
fn an_apng_carries_the_changed_rectangle_and_still_decodes_whole() -> Result<()>
{
    let mut canvas = Canvas::new(40.0, 40.0);
    canvas.set_gpu(false);
    // A translucent square that moves, over a background that does not, and
    // a still passage where two pages are identical. The translucency is the
    // part that separates replacing a rectangle from blending one: composited
    // twice, a half-alpha red over grey is a different colour.
    let steps = [0.0, 7.0, 7.0, 21.0, 33.0];
    for (page, left) in steps.iter().enumerate() {
        if page > 0 {
            canvas.new_page();
        }
        let ctx = canvas.context();
        ctx.set_fill_style(RgbaLinear::opaque(0.4, 0.4, 0.45));
        ctx.fill_rect(0.0, 0.0, 40.0, 40.0);
        ctx.set_fill_style(RgbaLinear::from_srgb(1.0, 0.0, 0.0, 0.5));
        ctx.fill_rect(*left, 5.0, 6.0, 6.0);
    }

    let encoded = canvas.to_buffer(
        ImageFormat::Apng,
        &EncodeOptions {
            fps: Some(10.0),
            ..EncodeOptions::default()
        },
    )?;

    let controls = frame_controls_of_apng(&encoded);
    assert_eq!(controls.len(), steps.len(), "one fcTL per page");

    // The first frame is the whole canvas: there is nothing before it to
    // differ from, and it is what a wrap of the animation repaints from.
    assert_eq!(
        (controls[0].0, controls[0].1, controls[0].2, controls[0].3),
        (0, 0, 40, 40),
        "the first frame has to stand alone"
    );

    for (at, control) in controls.iter().enumerate().skip(1) {
        let (x, y, width, height, dispose, blend) = *control;
        assert!(
            width < 40 || height < 40,
            "frame {at} covers the whole canvas at {width}x{height}"
        );
        assert!(x + width <= 40 && y + height <= 40, "frame {at} overflows");
        // Dispose nothing, blend nothing. Either one wrong is a picture that
        // decodes but is not the one that was drawn.
        assert_eq!((dispose, blend), (0, 0), "frame {at} composes wrongly");
    }

    // The still passage changes nothing, so its frame is the one-pixel
    // minimum rather than an empty rectangle the format cannot express.
    assert_eq!(
        (controls[2].2, controls[2].3),
        (1, 1),
        "two identical pages should cost one pixel"
    );

    // And every frame still decodes to the page it was made from. Compared
    // against the pages themselves, not against each other: two frames that
    // are wrong in the same way agree perfectly.
    let image = Image::from_encoded(&encoded).context("it decodes")?;
    assert_eq!(image.frame_count(), steps.len());
    for page in 0..steps.len() {
        let wanted = canvas.to_buffer(
            ImageFormat::Raw,
            &EncodeOptions {
                page: Some(page),
                ..EncodeOptions::default()
            },
        )?;
        assert_eq!(
            avif_frame_pixels(&image, page)?,
            wanted,
            "frame {page} decodes to something other than the page drawn"
        );
    }
    Ok(())
}

/// A change of one pixel costs one pixel, wherever it lands.
///
/// The diff walks rows and then columns within a row, and an off-by-one in
/// either direction is invisible on a rectangle with even bounds. An odd
/// column of an odd row is where it shows -- and it is also where APNG
/// differs from WebP, which halves its offsets and cannot start there.
#[test]
fn one_changed_pixel_at_an_odd_coordinate_is_one_pixel() -> Result<()> {
    let mut canvas = Canvas::new(16.0, 16.0);
    canvas.set_gpu(false);
    for page in 0..2 {
        if page > 0 {
            canvas.new_page();
        }
        let ctx = canvas.context();
        ctx.set_fill_style(RgbaLinear::opaque(0.0, 0.0, 0.0));
        ctx.fill_rect(0.0, 0.0, 16.0, 16.0);
        if page == 1 {
            ctx.set_fill_style(RgbaLinear::opaque(1.0, 1.0, 1.0));
            ctx.fill_rect(7.0, 9.0, 1.0, 1.0);
        }
    }

    let encoded = canvas.to_buffer(
        ImageFormat::Apng,
        &EncodeOptions {
            fps: Some(10.0),
            ..EncodeOptions::default()
        },
    )?;
    let controls = frame_controls_of_apng(&encoded);
    assert_eq!(
        (controls[1].0, controls[1].1, controls[1].2, controls[1].3),
        (7, 9, 1, 1),
        "the rectangle should be the pixel that changed and nothing else"
    );

    let image = Image::from_encoded(&encoded).context("it decodes")?;
    let wanted = canvas.to_buffer(
        ImageFormat::Raw,
        &EncodeOptions {
            page: Some(1),
            ..EncodeOptions::default()
        },
    )?;
    assert_eq!(avif_frame_pixels(&image, 1)?, wanted);
    Ok(())
}

/// Drawing one canvas into another must not compound.
///
/// The Rust API reaches this by its own door -- `draw_canvas` and
/// `create_pattern` take a `Canvas` directly rather than through the binding's
/// source resolver -- so the rule that stops a nested picture nesting again
/// has to hold here too. It did not: both doors kept the source's picture
/// unconditionally, and a page copied into a canvas and drawn back doubled the
/// work of the eventual rasterization every round.
///
/// Timed, because there is nothing to count: the recording is the same size
/// either way. The ratio is what the assertion is about, so a slow machine
/// moves every number and changes nothing.
#[test]
fn a_canvas_drawn_into_a_canvas_does_not_compound() {
    use std::time::Instant;

    fn rounds(n: usize) -> f64 {
        let mut page = Canvas::new(600.0, 600.0);
        page.set_gpu(false);
        {
            let ctx = page.context();
            ctx.set_fill_style_css("#742").expect("a css colour");
            ctx.fill_rect(0.0, 0.0, 600.0, 600.0);
        }

        let started = Instant::now();
        for _ in 0..n {
            let mut copy = Canvas::new(600.0, 600.0);
            copy.set_gpu(false);
            copy.context().draw_canvas(&mut page, 0.0, 0.0);

            let ctx = page.context();
            ctx.save();
            ctx.set_filter(&[FilterOp::Blur(10.0)])
                .expect("a blur is a filter");
            ctx.draw_canvas(&mut copy, 0.0, 0.0);
            ctx.restore();
        }
        page.context()
            .get_image_data(0.0, 0.0, 4.0, 4.0)
            .expect("the page rasterizes");
        started.elapsed().as_secs_f64() * 1e3
    }

    rounds(4);
    let short = rounds(8);
    let long = rounds(14);
    assert!(
        long < short * 4.0,
        "14 rounds must not cost squarely more than 8: {long:.0}ms against \
         {short:.0}ms"
    );
}

/// What this process has resident, in megabytes, or `None` where the platform
/// is one this does not know how to ask.
///
/// Shelling out on macOS rather than calling `task_info`: this is a test, the
/// cost is one process per call against a test that allocates hundreds of
/// megabytes, and it keeps `unsafe` and a `mach` dependency out of the tree
/// for the sake of an assertion.
fn resident_mb() -> Option<f64> {
    #[cfg(target_os = "linux")]
    {
        let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
        let pages: f64 = statm.split_whitespace().nth(1)?.parse().ok()?;
        return Some(pages * 4096.0 / (1024.0 * 1024.0));
    }
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("ps")
            .args(["-o", "rss=", "-p", &std::process::id().to_string()])
            .output()
            .ok()?;
        let kb: f64 =
            String::from_utf8_lossy(&out.stdout).trim().parse().ok()?;
        return Some(kb / 1024.0);
    }
    #[allow(unreachable_code)]
    None
}

/// A nested source drawn through a clip must rasterize only the clip.
///
/// The crate takes a canvas by its own door -- `draw_canvas`, not the
/// binding's `drawImage` -- and that door has twice been left behind by a fix
/// to the other one. This is the axis that catches it.
///
/// Resident memory, and not the clock, because the clock cannot see this:
/// Skia serves a repeated rasterization from its own cache, so sixty
/// whole-page flattens run in 0.26 seconds against 0.24 for sixty slivers
/// while holding an order of magnitude more. Three earlier attempts at a timed
/// version of this passed against the unfixed code.
#[test]
fn a_nested_canvas_drawn_through_a_clip_is_not_rasterized_whole() {
    const SIDE: f32 = 1400.0;

    let Some(before) = resident_mb() else {
        return; // A platform this cannot measure says nothing either way.
    };

    let mut inner = Canvas::new(SIDE, SIDE);
    inner.set_gpu(false);
    inner.context().fill_rect(0.0, 0.0, SIDE, SIDE);

    let mut source = Canvas::new(SIDE, SIDE);
    source.set_gpu(false);
    source.context().draw_canvas(&mut inner, 0.0, 0.0);
    {
        let ctx = source.context();
        ctx.set_fill_style_css("#1e3799").expect("a css colour");
        ctx.fill_rect(0.0, 0.0, SIDE, SIDE);
    }

    let mut page = Canvas::new(SIDE, SIDE);
    page.set_gpu(false);
    for i in 0..60 {
        let ctx = page.context();
        ctx.save();
        ctx.begin_path();
        ctx.rect((i * 17 % 1200) as f32, (i * 23 % 1300) as f32, 180.0, 24.0);
        ctx.clip(FillRule::NonZero);
        ctx.draw_canvas(&mut source, 0.0, 0.0);
        ctx.restore();
    }
    // The whole page: reading a corner composites only the tiles it touches,
    // so the draws never happen and the measurement is of nothing.
    page.context()
        .get_image_data(0.0, 0.0, SIDE, SIDE)
        .expect("the page rasterizes");

    let grew = resident_mb().unwrap_or(before) - before;
    // Sixty whole 1400-square rasterizations held about 490 MB; sixty slivers
    // hold about 40. The bound sits far from both.
    assert!(
        grew < 250.0,
        "sixty clipped draws must not each rasterize the whole source: grew \
         {grew:.0}MB"
    );
}

/// A canvas handed to another canvas keeps its own gamut.
///
/// The picture behind a source canvas was given to Skia as an eight-bit sRGB
/// image whatever the canvas was made with, so a `display-p3` source drawn
/// into a `display-p3` destination went out through sRGB and came back --
/// P3 red arriving as sRGB red converted up, with every colour outside the
/// smaller gamut gone. Only a source that has itself drawn a canvas takes
/// this path: `capture` hands over the picture directly when nothing is
/// nested, so the nesting here is what makes the bitmap arm reachable.
#[test]
fn a_wide_gamut_canvas_survives_being_drawn_into_another() -> Result<()> {
    let p3 = || {
        Canvas::with_options(
            2.0,
            2.0,
            CanvasOptions {
                color_space: PixelColorSpace::DisplayP3,
                gpu: false,
                ..CanvasOptions::default()
            },
        )
    };
    let read = |canvas: &mut Canvas| -> Result<[u8; 4]> {
        let pixels = canvas.to_buffer(
            ImageFormat::Raw,
            &EncodeOptions {
                color_space: Some(PixelColorSpace::DisplayP3),
                ..EncodeOptions::default()
            },
        )?;
        Ok([pixels[0], pixels[1], pixels[2], pixels[3]])
    };

    // Red named in the canvas's own space, so it is P3 red rather than sRGB
    // red converted into P3 -- the difference the round trip destroys.
    let mut inner = p3()?;
    {
        let ctx = inner.context();
        ctx.set_fill_style(RgbaLinear::opaque(1.0, 0.0, 0.0));
        ctx.fill_rect(0.0, 0.0, 2.0, 2.0);
    }
    assert_eq!(read(&mut inner)?, [255, 0, 0, 255], "the source is P3 red");

    // Nested once, which is what makes `capture` answer with pixels.
    let mut source = p3()?;
    source.context().draw_canvas(&mut inner, 0.0, 0.0);

    let mut dest = p3()?;
    dest.context().draw_canvas(&mut source, 0.0, 0.0);

    // [234, 51, 35] is sRGB red expressed in P3: what a trip through the
    // smaller gamut leaves behind.
    assert_eq!(
        read(&mut dest)?,
        [255, 0, 0, 255],
        "a nested P3 source must not be clipped to sRGB on the way in"
    );
    Ok(())
}

/// A clipped nested draw costs the region rather than the whole page.
///
/// Rasterizing the visible region drew the source's deferred image into a
/// region-sized surface, and Skia answers that by materializing the whole
/// page and copying the sliver out -- so every op in the source ran however
/// little of it showed. Replaying the picture into that surface lets Skia
/// cull against its bounds instead.
///
/// A ratio, so the machine cancels out: a hundredfold heavier source cost 66
/// times more before and does not now. The crate reaches this by its own
/// door, `capture`/`place_capture`, which handed over pixels without the
/// picture beside them and so could not replay anything.
#[test]
fn a_clipped_nested_draw_does_not_cost_the_whole_page() {
    use std::time::Instant;

    fn elapsed(ops: usize) -> f64 {
        let mut inner = Canvas::new(1400.0, 1400.0);
        inner.set_gpu(false);
        {
            let ctx = inner.context();
            ctx.set_fill_style_css("#742").expect("a css colour");
            ctx.fill_rect(0.0, 0.0, 1400.0, 1400.0);
            for i in 0..ops {
                let shade = (i % 12) as f32 / 12.0;
                ctx.set_fill_style(RgbaLinear::opaque(shade, 0.4, 1.0 - shade));
                ctx.fill_rect(
                    ((i * 31) % 1400) as f32,
                    ((i * 17) % 1400) as f32,
                    260.0,
                    140.0,
                );
            }
        }
        let mut source = Canvas::new(1400.0, 1400.0);
        source.set_gpu(false);
        source.context().draw_canvas(&mut inner, 0.0, 0.0);

        let mut draw = || {
            let mut dest = Canvas::new(1400.0, 1400.0);
            dest.set_gpu(false);
            {
                let ctx = dest.context();
                ctx.save();
                ctx.begin_path();
                ctx.rect(0.0, 0.0, 180.0, 24.0);
                ctx.clip(FillRule::NonZero);
                ctx.draw_canvas(&mut source, 0.0, 0.0);
                ctx.restore();
            }
            let data = dest
                .context()
                .get_image_data(0.0, 0.0, 4.0, 4.0)
                .expect("the page rasterizes");
            data.pixels()[3]
        };

        draw(); // warm, so the first page's setup is not in the number

        // The fastest of several passes rather than one. `cargo test` runs
        // these in parallel, so any single pass can be stretched by whatever
        // else holds a core -- and it stretches the cheap leg proportionally
        // more than the expensive one, which is exactly the ratio being
        // asserted. Load only ever adds time, so the minimum is the reading
        // least contaminated by it.
        let mut best = f64::INFINITY;
        for _ in 0..5 {
            let started = Instant::now();
            let mut seen = 0u32;
            for _ in 0..20 {
                seen += draw() as u32;
            }
            assert_eq!(seen, 20 * 255, "every round actually drew");
            best = best.min(started.elapsed().as_secs_f64() * 1e3);
        }
        eprintln!("ops={ops} -> {best:.1}ms");
        best
    }

    let light = elapsed(200);
    let heavy = elapsed(20000);
    assert!(
        heavy < light * 5.0,
        "a hundredfold heavier source must not cost proportionally more: \
         {light:.1}ms against {heavy:.1}ms"
    );
}

/// The format table answers page and animation questions to a Rust caller.
///
/// A caller deciding how to write a multi-page canvas has to know which
/// formats gather every page into one file. Testing the name instead --
/// `format == Pdf` -- decides correctly for the formats that existed when the
/// test was written and quietly keeps the last page alone for any added
/// after, which is the failure the shared table exists to prevent. The
/// binding asks the same table through `formats()`; this asserts the crate
/// can ask it too.
#[test]
fn the_format_table_answers_page_and_animation_questions() {
    let all: Vec<ImageFormat> = ImageFormat::all().collect();
    assert!(
        all.len() >= 11,
        "every format should enumerate, got {}",
        all.len()
    );

    // An animated format's frames are pages carrying durations, so one file
    // holding frames is one file holding pages. A format that animated
    // without spanning would have nowhere to put the second frame.
    for format in ImageFormat::all().filter(|f| f.is_animated()) {
        assert!(
            format.spans_pages(),
            "{} animates, so it must gather its pages",
            format.extension()
        );
    }

    // A vector format describes one page as marks; none of them carries a
    // clock.
    for format in ImageFormat::all().filter(|f| f.is_vector()) {
        assert!(
            !format.is_animated(),
            "{} is vector and cannot animate",
            format.extension()
        );
    }

    // Both predicates have to discriminate, or the loops above pass by being
    // empty.
    assert!(ImageFormat::all().any(ImageFormat::is_animated));
    assert!(ImageFormat::all().any(|f| !f.is_animated()));
    assert!(ImageFormat::all().any(ImageFormat::spans_pages));
    assert!(ImageFormat::all().any(|f| !f.spans_pages()));

    // APNG carries its own extension rather than PNG's. A caller inferring it
    // from the format's name gets "png" and writes over the still.
    let apng = ImageFormat::from_extension("apng").expect("apng is a format");
    assert_eq!(apng.extension(), "apng");
    assert!(apng.is_animated() && apng.spans_pages());
    assert_ne!(
        apng.extension(),
        ImageFormat::from_extension("png")
            .expect("png is a format")
            .extension()
    );
}

/// `Error`'s `Display` reaches a JavaScript or Python caller, so it may not
/// leak a Rust identifier.
///
/// `InvalidRect` rendered its rectangle with the derived `Debug`, giving
/// `Rect { left: 0.0, top: 0.0, right: 40.0, bottom: 20.0 }` to a caller who
/// wrote `borderRadius: NaN` -- while the variant directly above it already
/// printed `40x20`. The asymmetry is what made it read as an oversight.
///
/// A bad radius no longer arrives here at all: it is `InvalidRadius`, which
/// carries the radius rather than a rectangle built out of it. The row for
/// that is below, and it is about the subject rather than the formatting --
/// the old message named a rectangle nothing had rejected.
///
/// The colour-space case is the same defect in a quieter form: a fieldless
/// enum's `Debug` is a bare variant name, which looks acceptable until you
/// notice it is the *Rust* name. A caller writes `display-p3-linear`.
///
/// Both are asserted, and each is proven separately by mutation -- the first
/// assertion would otherwise mask the second.
#[test]
fn error_display_uses_the_caller_s_vocabulary() {
    let rect = Error::InvalidRect {
        rect: meo_skia_canvas::geometry::Rect {
            left: 0.0,
            top: 0.0,
            right: 40.0,
            bottom: 20.0,
        },
    };
    let shown = rect.to_string();
    assert_eq!(shown, "invalid rect: 40x20 at 0,0");
    assert!(
        !shown.contains("Rect {"),
        "a Rust struct dump reached the caller: {shown}"
    );

    // The value that was refused, not the shape it would have described.
    // `round_rect(5, 5, 30, 30, [NaN, 0, 0, 0])` used to report "invalid
    // rect: 30x30 at 5,5" -- a true statement about a rectangle that is
    // perfectly valid, and a false one about what went wrong.
    let radius = Error::InvalidRadius { radius: -10.0 };
    assert_eq!(radius.to_string(), "invalid radius: -10");
    assert!(
        !radius.to_string().contains("rect"),
        "a radius must not be reported as a rectangle: {radius}"
    );

    let space = Error::UnsupportedPixelColorSpace {
        color_space: PixelColorSpace::DisplayP3Linear,
    };
    let shown = space.to_string();
    assert_eq!(shown, "unsupported pixel color space: display-p3-linear");
    assert!(
        !shown.contains("DisplayP3Linear"),
        "the Rust spelling reached the caller: {shown}"
    );
}
