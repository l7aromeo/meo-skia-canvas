//! Renderer contract tests for the `skia_canvas::native` facade.
//!
//! Tests in this file exercise the surface, pixel IO, and paint/blend
//! subsets (Chunks 2 and 3A of the Studio renderer gap closure plan).
//! Path/filter/shader/font/paragraph tests land alongside their
//! implementation chunks; this file intentionally avoids importing those
//! types so the branch stays compiling and green.
//!
//! Public-leak audit (run from repo root):
//!   rg -n "pub .*skia_safe|pub .*FunctionContext|pub .*JsBox|pub .*Handle<|pub .*RefCell" src/native

use anyhow::Result;
use skia_canvas::native::{
    BlendMode, FillRule, GradientInterpolation, GradientStop, LinearColorSpace, NativeAffine,
    NativeBackend, NativeColorFilter, NativeError, NativeFontManager, NativeImage,
    NativeImageFilter, NativePaint, NativePath, NativeShader, NativeTextEngine, NativeTextLayout,
    PaintStyle, PixelColorSpace, PixelDepth, PixelExportOptions, PixelFormat, Point, Rect,
    RgbaLinear, RichTextSpan, SamplingMode, StrokeCap, SurfaceOptions, TextAlign, TextDecoration,
    TextShadow, TextStyle,
};

const ALPHA_HALF_U8: u8 = 128;

fn red_premul(alpha: f32) -> RgbaLinear {
    // Premultiplied linear red at the given alpha (alpha is straight 0..1).
    RgbaLinear::new_premultiplied(alpha, 0.0, 0.0, alpha)
}

#[test]
fn surface_create_clear_draw_snapshot_compose_readback() -> Result<()> {
    let backend = NativeBackend::new();
    let mut surface = backend.create_surface(8, 8, SurfaceOptions::default())?;
    assert_eq!(surface.width(), 8);
    assert_eq!(surface.height(), 8);

    surface.with_canvas(|canvas| {
        canvas.clear(RgbaLinear::new_premultiplied(0.0, 0.0, 0.0, 0.0));
        canvas.draw_rect(
            Rect::from_xywh(2.0, 2.0, 4.0, 4.0),
            &NativePaint::fill(RgbaLinear::opaque(1.0, 0.0, 0.0)),
        );
    });

    // Snapshot, then composite the snapshot onto a second surface via
    // `draw_image_rect` (default source-over). BlendMode::SourceOver lands
    // with Chunk 3.
    let snapshot = surface.snapshot();
    let mut composed = backend.create_surface(8, 8, SurfaceOptions::default())?;
    composed.with_canvas(|canvas| {
        canvas.clear(RgbaLinear::new_premultiplied(0.0, 0.0, 0.0, 0.0));
        canvas.draw_image_rect(&snapshot, Rect::from_xywh(0.0, 0.0, 8.0, 8.0), 1.0);
    });

    let frame = composed.read_pixels()?;
    assert_eq!(frame.width(), 8);
    assert_eq!(frame.height(), 8);
    assert_eq!(frame.stride(), 8 * 4);
    assert_eq!(frame.depth(), PixelDepth::Uint8);
    assert_eq!(frame.color_space(), PixelColorSpace::Srgb);
    assert!(!frame.premultiplied());
    assert!(frame.pixels().iter().any(|c| *c != 0));
    Ok(())
}

#[test]
fn create_offscreen_inherits_color_space_and_composes() -> Result<()> {
    let backend = NativeBackend::new();
    let mut main = backend.create_surface(
        8,
        8,
        SurfaceOptions {
            color_space: LinearColorSpace::DisplayP3,
            ..SurfaceOptions::default()
        },
    )?;
    let mut offscreen = main.create_offscreen(4, 4)?;
    assert_eq!(offscreen.width(), 4);
    assert_eq!(offscreen.height(), 4);
    assert_eq!(offscreen.color_space(), LinearColorSpace::DisplayP3);

    offscreen.with_canvas(|canvas| {
        canvas.clear(RgbaLinear::opaque(0.0, 1.0, 0.0));
    });
    let off_snapshot = offscreen.snapshot();
    main.with_canvas(|canvas| {
        canvas.clear(RgbaLinear::new_premultiplied(0.0, 0.0, 0.0, 0.0));
        canvas.draw_image_rect(&off_snapshot, Rect::from_xywh(0.0, 0.0, 4.0, 4.0), 1.0);
    });
    let exported = main.read_pixels()?;
    assert!(exported.pixels().iter().any(|c| *c > 32));
    Ok(())
}

#[test]
fn read_pixels_as_supports_required_color_spaces() -> Result<()> {
    let backend = NativeBackend::new();
    for color_space in [
        PixelColorSpace::Srgb,
        PixelColorSpace::SrgbLinear,
        PixelColorSpace::DisplayP3,
        PixelColorSpace::DisplayP3Linear,
        PixelColorSpace::Rec2020,
        PixelColorSpace::Rec2020Linear,
    ] {
        let mut surface = backend.create_surface(2, 2, SurfaceOptions::default())?;
        surface.with_canvas(|canvas| canvas.clear(RgbaLinear::opaque(0.5, 0.5, 0.5)));
        let exported = surface.read_pixels_as(PixelExportOptions {
            color_space,
            depth: PixelDepth::Uint8,
            premultiplied: false,
        })?;
        assert_eq!(exported.color_space(), color_space);
        assert_eq!(exported.width(), 2);
        assert_eq!(exported.height(), 2);
        assert_eq!(exported.stride(), 2 * 4);
    }
    Ok(())
}

#[test]
fn read_write_pixels_linear_round_trips_dimensions() -> Result<()> {
    let backend = NativeBackend::new();
    let mut surface = backend.create_surface(4, 4, SurfaceOptions::default())?;
    surface.with_canvas(|canvas| canvas.clear(RgbaLinear::opaque(0.25, 0.5, 0.75)));
    let exported = surface.read_pixels_linear()?;
    assert_eq!(exported.depth(), PixelDepth::F32);
    assert_eq!(exported.color_space(), PixelColorSpace::SrgbLinear);
    assert!(exported.premultiplied());
    assert_eq!(exported.stride(), 4 * 16);
    assert_eq!(exported.pixels().len(), 4 * 16 * 4);

    // Round-trip back into a fresh surface and re-read; dimensions and depth
    // must be preserved.
    let mut destination = backend.create_surface(4, 4, SurfaceOptions::default())?;
    destination.write_pixels_linear(exported.pixels())?;
    let round_tripped = destination.read_pixels_linear()?;
    assert_eq!(round_tripped.width(), 4);
    assert_eq!(round_tripped.height(), 4);
    assert_eq!(round_tripped.depth(), PixelDepth::F32);
    assert_eq!(round_tripped.pixels().len(), exported.pixels().len());
    Ok(())
}

#[test]
fn linear_working_spaces_accept_hdr_values_above_one() -> Result<()> {
    let backend = NativeBackend::new();
    for color_space in [
        LinearColorSpace::Srgb,
        LinearColorSpace::DisplayP3,
        LinearColorSpace::Rec2020,
    ] {
        let mut surface = backend.create_surface(
            2,
            2,
            SurfaceOptions {
                color_space,
                ..SurfaceOptions::default()
            },
        )?;
        surface.with_canvas(|canvas| canvas.clear(RgbaLinear::opaque(2.0, 0.0, 0.0)));
        let exported = surface.read_pixels_linear()?;
        assert_eq!(exported.depth(), PixelDepth::F32);
        // First pixel's red channel as a little-endian f32.
        let bytes = exported.pixels();
        let r = f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        assert!(r > 1.5, "expected HDR red >1.5 in {color_space:?}, got {r}");
    }
    Ok(())
}

#[test]
fn premultiplied_alpha_preserved_across_read_modes() -> Result<()> {
    let backend = NativeBackend::new();
    let mut surface = backend.create_surface(4, 4, SurfaceOptions::default())?;
    surface.with_canvas(|canvas| {
        canvas.clear(RgbaLinear::new_premultiplied(0.0, 0.0, 0.0, 0.0));
        canvas.draw_rect(
            Rect::from_xywh(0.0, 0.0, 4.0, 4.0),
            &NativePaint::fill(red_premul(0.5)),
        );
    });

    // Premultiplied Uint8 in the surface's own linear space: r ≈ 0.5 * 255.
    let premul = surface.read_pixels_as(PixelExportOptions {
        color_space: PixelColorSpace::SrgbLinear,
        depth: PixelDepth::Uint8,
        premultiplied: true,
    })?;
    let p_r = premul.pixels()[0];
    let p_a = premul.pixels()[3];
    assert!(
        p_r.abs_diff(ALPHA_HALF_U8) <= 4,
        "premul red ≈ 128, got {p_r}"
    );
    assert!(
        p_a.abs_diff(ALPHA_HALF_U8) <= 4,
        "premul alpha ≈ 128, got {p_a}"
    );

    // Unpremultiplied Uint8 in the same color space: r = 1.0 * 255, alpha = 128.
    let unpremul = surface.read_pixels_as(PixelExportOptions {
        color_space: PixelColorSpace::SrgbLinear,
        depth: PixelDepth::Uint8,
        premultiplied: false,
    })?;
    let u_r = unpremul.pixels()[0];
    let u_a = unpremul.pixels()[3];
    assert!(u_r > 240, "unpremul red ≈ 255, got {u_r}");
    assert!(
        u_a.abs_diff(ALPHA_HALF_U8) <= 4,
        "unpremul alpha ≈ 128, got {u_a}"
    );
    Ok(())
}

/// Compile-time leak audit: importing only `skia_canvas::native::*`
/// must be sufficient for surface + pixel IO contract use. This test
/// references the new public types at run time so any accidental private
/// scoping breaks the build.
#[test]
fn public_types_are_reachable_from_native_namespace_only() -> Result<()> {
    let backend = NativeBackend::new();
    let surface = backend.create_surface(1, 1, SurfaceOptions::default())?;
    let _ = (
        surface.color_space(),
        PixelColorSpace::Srgb,
        PixelDepth::F16,
        PixelExportOptions::default(),
    );
    Ok(())
}

// --- Chunk 3A: paint and blend ---------------------------------------------

#[test]
fn native_paint_default_matches_canvas_defaults() {
    let p = NativePaint::default();
    assert_eq!(p.style, PaintStyle::Fill);
    assert_eq!(p.stroke_width, 1.0);
    assert_eq!(p.stroke_cap, StrokeCap::Butt);
    assert!(p.dash.is_none());
    assert!(p.anti_alias);
    assert_eq!(p.alpha, 1.0);
    assert_eq!(p.blend_mode, BlendMode::SourceOver);
    assert!(p.shader.is_none());
    assert!(p.image_filter.is_none());
    assert!(p.color_filter.is_none());
}

#[test]
fn native_paint_constructors_set_style() {
    let f = NativePaint::fill(RgbaLinear::opaque(1.0, 0.0, 0.0));
    assert_eq!(f.style, PaintStyle::Fill);
    let s = NativePaint::stroke(RgbaLinear::opaque(0.0, 1.0, 0.0), 3.5);
    assert_eq!(s.style, PaintStyle::Stroke);
    assert_eq!(s.stroke_width, 3.5);
}

/// `paint.alpha` modulates the final color: a 1.0 paint produces ~255,
/// while alpha 0.5 produces ~half pixel coverage on the same opaque red.
#[test]
fn native_paint_alpha_modulates_output() -> Result<()> {
    let backend = NativeBackend::new();
    let mut full = backend.create_surface(2, 2, SurfaceOptions::default())?;
    full.with_canvas(|canvas| {
        canvas.clear(RgbaLinear::new_premultiplied(0.0, 0.0, 0.0, 0.0));
        canvas.draw_rect(
            Rect::from_xywh(0.0, 0.0, 2.0, 2.0),
            &NativePaint::fill(RgbaLinear::opaque(1.0, 0.0, 0.0)),
        );
    });
    let full_px = full.read_pixels()?;
    assert!(full_px.pixels()[0] > 240, "full alpha red ≈ 255");
    assert_eq!(full_px.pixels()[3], 255);

    let mut half = backend.create_surface(2, 2, SurfaceOptions::default())?;
    half.with_canvas(|canvas| {
        canvas.clear(RgbaLinear::new_premultiplied(0.0, 0.0, 0.0, 0.0));
        let mut paint = NativePaint::fill(RgbaLinear::opaque(1.0, 0.0, 0.0));
        paint.set_alpha(0.5);
        canvas.draw_rect(Rect::from_xywh(0.0, 0.0, 2.0, 2.0), &paint);
    });
    let half_px = half.read_pixels()?;
    let alpha_byte = half_px.pixels()[3];
    assert!(
        (110..=145).contains(&alpha_byte),
        "alpha 0.5 ≈ 128, got {alpha_byte}"
    );
    Ok(())
}

/// Different blend modes produce different output for the same overlap.
/// Asserts SourceOver, Multiply, and PlusLighter all diverge.
#[test]
fn blend_modes_produce_distinct_outputs() -> Result<()> {
    let backend = NativeBackend::new();
    let render_with = |mode: BlendMode| -> Result<[u8; 4]> {
        let mut surface = backend.create_surface(2, 2, SurfaceOptions::default())?;
        surface.with_canvas(|canvas| {
            canvas.clear(RgbaLinear::opaque(0.5, 0.5, 0.5));
            let mut paint = NativePaint::fill(RgbaLinear::opaque(0.5, 0.0, 0.5));
            paint.set_blend_mode(mode);
            canvas.draw_rect(Rect::from_xywh(0.0, 0.0, 2.0, 2.0), &paint);
        });
        let px = surface.read_pixels()?;
        Ok([
            px.pixels()[0],
            px.pixels()[1],
            px.pixels()[2],
            px.pixels()[3],
        ])
    };
    let src_over = render_with(BlendMode::SourceOver)?;
    let multiply = render_with(BlendMode::Multiply)?;
    let plus_lighter = render_with(BlendMode::PlusLighter)?;
    assert_ne!(src_over, multiply, "SourceOver and Multiply must differ");
    assert_ne!(
        src_over, plus_lighter,
        "SourceOver and PlusLighter must differ"
    );
    assert_ne!(
        multiply, plus_lighter,
        "Multiply and PlusLighter must differ"
    );
    Ok(())
}

/// Exhaustive blend-mode plumbing: every Canvas blend mode round-trips
/// through to a non-panicking draw and a successful pixel readback. This
/// catches typos in the `to_skia` mapping.
#[test]
fn every_blend_mode_renders_without_error() -> Result<()> {
    let backend = NativeBackend::new();
    let modes = [
        BlendMode::SourceOver,
        BlendMode::SourceIn,
        BlendMode::SourceOut,
        BlendMode::SourceAtop,
        BlendMode::DestinationOver,
        BlendMode::DestinationIn,
        BlendMode::DestinationOut,
        BlendMode::DestinationAtop,
        BlendMode::Copy,
        BlendMode::Xor,
        BlendMode::Multiply,
        BlendMode::Screen,
        BlendMode::Overlay,
        BlendMode::Darken,
        BlendMode::Lighten,
        BlendMode::ColorDodge,
        BlendMode::ColorBurn,
        BlendMode::HardLight,
        BlendMode::SoftLight,
        BlendMode::Difference,
        BlendMode::Exclusion,
        BlendMode::Hue,
        BlendMode::Saturation,
        BlendMode::Color,
        BlendMode::Luminosity,
        BlendMode::PlusLighter,
    ];
    for mode in modes {
        let mut surface = backend.create_surface(2, 2, SurfaceOptions::default())?;
        surface.with_canvas(|canvas| {
            canvas.clear(RgbaLinear::opaque(0.4, 0.4, 0.4));
            let mut paint = NativePaint::fill(RgbaLinear::opaque(0.6, 0.2, 0.8));
            paint.set_blend_mode(mode);
            canvas.draw_rect(Rect::from_xywh(0.0, 0.0, 2.0, 2.0), &paint);
        });
        let _ = surface.read_pixels()?;
    }
    Ok(())
}

/// State-level checks for stroke cap and dash. Visual verification waits
/// for Chunk 3C (`draw_line` / `draw_path`); rectangles use joins not
/// caps, so stroke caps are not visually exercised on `draw_rect`.
#[test]
fn stroke_cap_state_round_trips_through_paint() {
    let mut paint = NativePaint::stroke(RgbaLinear::opaque(1.0, 1.0, 1.0), 4.0);
    assert_eq!(paint.stroke_cap, StrokeCap::Butt);
    paint.set_stroke_cap(StrokeCap::Round);
    assert_eq!(paint.stroke_cap, StrokeCap::Round);
    paint.set_stroke_cap(StrokeCap::Square);
    assert_eq!(paint.stroke_cap, StrokeCap::Square);
}

#[test]
fn dash_pattern_state_round_trips_through_paint() -> Result<()> {
    let mut paint = NativePaint::stroke(RgbaLinear::opaque(1.0, 1.0, 1.0), 2.0);
    assert!(paint.dash.is_none());
    paint.set_dash(vec![4.0, 4.0], 0.0);
    let dash = paint
        .dash
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("dash should be set after set_dash"))?;
    assert_eq!(dash.intervals, vec![4.0, 4.0]);
    assert_eq!(dash.phase, 0.0);
    let _ = dash; // release immutable borrow before clear_dash takes &mut.
    paint.clear_dash();
    assert!(paint.dash.is_none());
    Ok(())
}

// --- Chunk 3B: canvas state + layer basics ---------------------------------

/// `clip_rect` masks draws to the rectangle. Pixels outside the clip rect
/// must remain at the cleared (transparent) color.
#[test]
fn clip_rect_masks_drawing() -> Result<()> {
    let backend = NativeBackend::new();
    let mut surface = backend.create_surface(8, 8, SurfaceOptions::default())?;
    surface.with_canvas(|canvas| {
        canvas.clear(RgbaLinear::new_premultiplied(0.0, 0.0, 0.0, 0.0));
        canvas.save();
        canvas.clip_rect(Rect::from_xywh(2.0, 2.0, 4.0, 4.0));
        canvas.draw_rect(
            Rect::from_xywh(0.0, 0.0, 8.0, 8.0),
            &NativePaint::fill(RgbaLinear::opaque(1.0, 0.0, 0.0)),
        );
        canvas.restore();
    });
    let px = surface.read_pixels()?;
    let stride = px.stride();
    let alpha_at = |x: usize, y: usize| px.pixels()[y * stride + x * 4 + 3];
    assert_eq!(alpha_at(0, 0), 0, "top-left outside clip stays transparent");
    assert_eq!(
        alpha_at(7, 7),
        0,
        "bottom-right outside clip stays transparent"
    );
    assert!(alpha_at(3, 3) > 240, "inside clip stays red opaque");
    Ok(())
}

/// `clip_rrect` rounds the corners. The four extreme corners of the clip
/// rect must be transparent while the center remains opaque.
#[test]
fn clip_rrect_rounds_corners() -> Result<()> {
    let backend = NativeBackend::new();
    let mut surface = backend.create_surface(16, 16, SurfaceOptions::default())?;
    surface.with_canvas(|canvas| {
        canvas.clear(RgbaLinear::new_premultiplied(0.0, 0.0, 0.0, 0.0));
        canvas.save();
        canvas.clip_rrect(Rect::from_xywh(0.0, 0.0, 16.0, 16.0), 8.0);
        canvas.draw_rect(
            Rect::from_xywh(0.0, 0.0, 16.0, 16.0),
            &NativePaint::fill(RgbaLinear::opaque(1.0, 1.0, 1.0)),
        );
        canvas.restore();
    });
    let px = surface.read_pixels()?;
    let stride = px.stride();
    let alpha_at = |x: usize, y: usize| px.pixels()[y * stride + x * 4 + 3];
    assert_eq!(alpha_at(0, 0), 0, "top-left rounded corner is transparent");
    assert_eq!(
        alpha_at(15, 0),
        0,
        "top-right rounded corner is transparent"
    );
    assert_eq!(
        alpha_at(0, 15),
        0,
        "bottom-left rounded corner is transparent"
    );
    assert_eq!(
        alpha_at(15, 15),
        0,
        "bottom-right rounded corner is transparent"
    );
    assert!(alpha_at(8, 8) > 240, "center is fully opaque");
    Ok(())
}

/// `concat_transform` with a translation moves subsequent draws. Drawing a
/// 4x4 rect at (0,0) with a +6 horizontal translation must hit pixels in
/// the (6..10, 0..4) region instead of the origin.
#[test]
fn concat_transform_translates_subsequent_draws() -> Result<()> {
    let backend = NativeBackend::new();
    let mut surface = backend.create_surface(16, 8, SurfaceOptions::default())?;
    surface.with_canvas(|canvas| {
        canvas.clear(RgbaLinear::new_premultiplied(0.0, 0.0, 0.0, 0.0));
        canvas.save();
        canvas.concat_transform(NativeAffine::translation(6.0, 0.0));
        canvas.draw_rect(
            Rect::from_xywh(0.0, 0.0, 4.0, 4.0),
            &NativePaint::fill(RgbaLinear::opaque(1.0, 0.0, 0.0)),
        );
        canvas.restore();
    });
    let px = surface.read_pixels()?;
    let stride = px.stride();
    let alpha_at = |x: usize, y: usize| px.pixels()[y * stride + x * 4 + 3];
    assert_eq!(alpha_at(0, 0), 0, "origin stays empty after translation");
    assert!(
        alpha_at(7, 1) > 240,
        "translated rect occupies (6..10, 0..4)"
    );
    Ok(())
}

/// `concat_transform` with a scale stretches subsequent draws. A 2x2 rect
/// scaled 3x covers a 6x6 region.
#[test]
fn concat_transform_scales_subsequent_draws() -> Result<()> {
    let backend = NativeBackend::new();
    let mut surface = backend.create_surface(8, 8, SurfaceOptions::default())?;
    surface.with_canvas(|canvas| {
        canvas.clear(RgbaLinear::new_premultiplied(0.0, 0.0, 0.0, 0.0));
        canvas.save();
        canvas.concat_transform(NativeAffine::scale(3.0, 3.0));
        canvas.draw_rect(
            Rect::from_xywh(0.0, 0.0, 2.0, 2.0),
            &NativePaint::fill(RgbaLinear::opaque(1.0, 0.0, 0.0)),
        );
        canvas.restore();
    });
    let px = surface.read_pixels()?;
    let stride = px.stride();
    let alpha_at = |x: usize, y: usize| px.pixels()[y * stride + x * 4 + 3];
    assert!(alpha_at(0, 0) > 240, "scaled rect covers origin");
    assert!(alpha_at(5, 5) > 240, "scaled rect covers (5,5)");
    assert_eq!(alpha_at(7, 7), 0, "outside the 6x6 region stays empty");
    Ok(())
}

/// `canvas.scale(sx, sy)` is a convenience equivalent to
/// `concat_transform(NativeAffine::scale(sx, sy))`.
#[test]
fn scale_method_matches_concat_scale_transform() -> Result<()> {
    let backend = NativeBackend::new();
    let render = |use_scale_helper: bool| -> Result<Vec<u8>> {
        let mut surface = backend.create_surface(8, 8, SurfaceOptions::default())?;
        surface.with_canvas(|canvas| {
            canvas.clear(RgbaLinear::new_premultiplied(0.0, 0.0, 0.0, 0.0));
            canvas.save();
            if use_scale_helper {
                canvas.scale(2.0, 2.0);
            } else {
                canvas.concat_transform(NativeAffine::scale(2.0, 2.0));
            }
            canvas.draw_rect(
                Rect::from_xywh(0.0, 0.0, 3.0, 3.0),
                &NativePaint::fill(RgbaLinear::opaque(0.0, 1.0, 0.0)),
            );
            canvas.restore();
        });
        Ok(surface.read_pixels()?.into_pixels())
    };
    let helper = render(true)?;
    let direct = render(false)?;
    assert_eq!(helper, direct);
    Ok(())
}

/// Layer opacity isolation: drawing two opaque rects inside a layer with
/// `paint.alpha = 0.5` produces a different result than drawing each rect
/// directly with alpha 0.5. The layer composes the two rects internally
/// (last-wins for src-over) and only halves the final layer; the direct
/// approach blends each rect at 0.5 onto the destination, leaving residual
/// color from earlier rects.
#[test]
fn save_layer_opacity_isolates_inner_compositing() -> Result<()> {
    let backend = NativeBackend::new();

    let mut layered = backend.create_surface(4, 4, SurfaceOptions::default())?;
    layered.with_canvas(|canvas| {
        canvas.clear(RgbaLinear::opaque(0.0, 0.0, 0.0));
        let mut layer_paint = NativePaint::default();
        layer_paint.set_alpha(0.5);
        canvas.save_layer(Some(&layer_paint));
        canvas.draw_rect(
            Rect::from_xywh(0.0, 0.0, 4.0, 4.0),
            &NativePaint::fill(RgbaLinear::opaque(1.0, 0.0, 0.0)),
        );
        canvas.draw_rect(
            Rect::from_xywh(0.0, 0.0, 4.0, 4.0),
            &NativePaint::fill(RgbaLinear::opaque(0.0, 1.0, 0.0)),
        );
        canvas.restore();
    });

    let mut direct = backend.create_surface(4, 4, SurfaceOptions::default())?;
    direct.with_canvas(|canvas| {
        canvas.clear(RgbaLinear::opaque(0.0, 0.0, 0.0));
        let mut red = NativePaint::fill(RgbaLinear::opaque(1.0, 0.0, 0.0));
        red.set_alpha(0.5);
        canvas.draw_rect(Rect::from_xywh(0.0, 0.0, 4.0, 4.0), &red);
        let mut green = NativePaint::fill(RgbaLinear::opaque(0.0, 1.0, 0.0));
        green.set_alpha(0.5);
        canvas.draw_rect(Rect::from_xywh(0.0, 0.0, 4.0, 4.0), &green);
    });

    let l = layered.read_pixels()?;
    let d = direct.read_pixels()?;
    let lr = l.pixels()[0];
    let dr = d.pixels()[0];
    assert!(
        lr < dr.saturating_sub(16),
        "layer should hide red residual: layered_r={lr} direct_r={dr}"
    );
    Ok(())
}

/// Layer blend mode applies on layer composite, not per-draw. Drawing a
/// red rect inside a `PlusLighter` layer onto a non-trivial background
/// gives a different result than drawing the red rect directly with
/// `PlusLighter`.
#[test]
fn save_layer_blend_mode_applies_to_layer_composite() -> Result<()> {
    let backend = NativeBackend::new();

    let mut layered = backend.create_surface(4, 4, SurfaceOptions::default())?;
    layered.with_canvas(|canvas| {
        canvas.clear(RgbaLinear::opaque(0.4, 0.4, 0.4));
        let mut layer_paint = NativePaint::default();
        layer_paint.set_blend_mode(BlendMode::PlusLighter);
        canvas.save_layer(Some(&layer_paint));
        // Two draws inside the layer: only the layer composite uses
        // PlusLighter, the inner draws use src-over by default.
        canvas.draw_rect(
            Rect::from_xywh(0.0, 0.0, 4.0, 4.0),
            &NativePaint::fill(RgbaLinear::opaque(0.5, 0.0, 0.0)),
        );
        canvas.draw_rect(
            Rect::from_xywh(0.0, 0.0, 4.0, 4.0),
            &NativePaint::fill(RgbaLinear::opaque(0.0, 0.5, 0.0)),
        );
        canvas.restore();
    });

    let mut direct = backend.create_surface(4, 4, SurfaceOptions::default())?;
    direct.with_canvas(|canvas| {
        canvas.clear(RgbaLinear::opaque(0.4, 0.4, 0.4));
        let mut a = NativePaint::fill(RgbaLinear::opaque(0.5, 0.0, 0.0));
        a.set_blend_mode(BlendMode::PlusLighter);
        canvas.draw_rect(Rect::from_xywh(0.0, 0.0, 4.0, 4.0), &a);
        let mut b = NativePaint::fill(RgbaLinear::opaque(0.0, 0.5, 0.0));
        b.set_blend_mode(BlendMode::PlusLighter);
        canvas.draw_rect(Rect::from_xywh(0.0, 0.0, 4.0, 4.0), &b);
    });

    let l = layered.read_pixels()?;
    let d = direct.read_pixels()?;
    assert_ne!(
        &l.pixels()[..4],
        &d.pixels()[..4],
        "layered PlusLighter must differ from sequential PlusLighter"
    );
    Ok(())
}

/// `draw_surface` composites a source surface's pixels onto this canvas at
/// `(x, y)`. A 4x4 red source drawn at (2, 2) on an 8x8 destination must
/// fill exactly the 4x4 region at that offset.
#[test]
fn draw_surface_composites_at_offset() -> Result<()> {
    let backend = NativeBackend::new();
    let mut source = backend.create_surface(4, 4, SurfaceOptions::default())?;
    source.with_canvas(|canvas| {
        canvas.clear(RgbaLinear::opaque(1.0, 0.0, 0.0));
    });

    let mut dest = backend.create_surface(8, 8, SurfaceOptions::default())?;
    dest.with_canvas(|canvas| {
        canvas.clear(RgbaLinear::new_premultiplied(0.0, 0.0, 0.0, 0.0));
        canvas.draw_surface(&mut source, 2.0, 2.0, None);
    });
    let px = dest.read_pixels()?;
    let stride = px.stride();
    let alpha_at = |x: usize, y: usize| px.pixels()[y * stride + x * 4 + 3];
    let red_at = |x: usize, y: usize| px.pixels()[y * stride + x * 4];
    assert_eq!(alpha_at(0, 0), 0, "outside source bounds stays transparent");
    assert!(red_at(3, 3) > 240, "source red opaque at offset");
    assert!(red_at(5, 5) > 240, "source red opaque at offset");
    assert_eq!(
        alpha_at(6, 6),
        0,
        "right of source bounds stays transparent"
    );
    Ok(())
}

/// `draw_surface` honours the optional paint's alpha multiplier so the
/// destination receives a half-strength composite when alpha is 0.5.
#[test]
fn draw_surface_with_paint_modulates_alpha() -> Result<()> {
    let backend = NativeBackend::new();
    let mut source = backend.create_surface(4, 4, SurfaceOptions::default())?;
    source.with_canvas(|canvas| {
        canvas.clear(RgbaLinear::opaque(1.0, 0.0, 0.0));
    });

    let mut dest = backend.create_surface(4, 4, SurfaceOptions::default())?;
    dest.with_canvas(|canvas| {
        canvas.clear(RgbaLinear::new_premultiplied(0.0, 0.0, 0.0, 0.0));
        let mut paint = NativePaint::default();
        paint.set_alpha(0.5);
        canvas.draw_surface(&mut source, 0.0, 0.0, Some(&paint));
    });
    let px = dest.read_pixels()?;
    let alpha = px.pixels()[3];
    assert!((110..=145).contains(&alpha), "alpha 0.5 ≈ 128, got {alpha}");
    Ok(())
}

// --- Chunk 3C: paths, line, draw_image_src, sampling ----------------------

/// SVG path data renders visible pixels.
#[test]
fn svg_path_draws_visible_pixels() -> Result<()> {
    let backend = NativeBackend::new();
    let mut surface = backend.create_surface(8, 8, SurfaceOptions::default())?;
    let path = NativePath::from_svg("M0 0 L8 0 L8 8 L0 8 Z", FillRule::NonZero)?;
    surface.with_canvas(|canvas| {
        canvas.clear(RgbaLinear::new_premultiplied(0.0, 0.0, 0.0, 0.0));
        canvas.draw_path(&path, &NativePaint::fill(RgbaLinear::opaque(1.0, 0.0, 0.0)));
    });
    let px = surface.read_pixels()?;
    assert!(px.pixels()[3] > 240, "filled square covers (0,0)");
    Ok(())
}

/// On a self-overlapping path (two same-direction concentric squares), the
/// inner region winds twice. `NonZero` fills the inner region (count != 0);
/// `EvenOdd` leaves the inner region empty (count mod 2 == 0). The two
/// renderings must produce different pixels at the inner region's center.
#[test]
fn fill_rule_evenodd_differs_from_nonzero_on_nested_path() -> Result<()> {
    let backend = NativeBackend::new();
    // Two same-direction (CW) concentric squares: outer 8x8, inner 4x4 hole.
    let svg = "M0 0 L8 0 L8 8 L0 8 Z M2 2 L6 2 L6 6 L2 6 Z";

    let nonzero = {
        let mut surface = backend.create_surface(8, 8, SurfaceOptions::default())?;
        let path = NativePath::from_svg(svg, FillRule::NonZero)?;
        surface.with_canvas(|canvas| {
            canvas.clear(RgbaLinear::new_premultiplied(0.0, 0.0, 0.0, 0.0));
            canvas.draw_path(&path, &NativePaint::fill(RgbaLinear::opaque(1.0, 1.0, 1.0)));
        });
        surface.read_pixels()?
    };

    let evenodd = {
        let mut surface = backend.create_surface(8, 8, SurfaceOptions::default())?;
        let path = NativePath::from_svg(svg, FillRule::EvenOdd)?;
        surface.with_canvas(|canvas| {
            canvas.clear(RgbaLinear::new_premultiplied(0.0, 0.0, 0.0, 0.0));
            canvas.draw_path(&path, &NativePaint::fill(RgbaLinear::opaque(1.0, 1.0, 1.0)));
        });
        surface.read_pixels()?
    };

    let stride = nonzero.stride();
    let alpha_at = |buf: &[u8], x: usize, y: usize| buf[y * stride + x * 4 + 3];

    // Inner pixel (4, 4): NonZero fills (winding=2), EvenOdd leaves empty.
    let nz_inner = alpha_at(nonzero.pixels(), 4, 4);
    let eo_inner = alpha_at(evenodd.pixels(), 4, 4);
    assert!(nz_inner > 240, "NonZero fills inner: alpha={nz_inner}");
    assert_eq!(eo_inner, 0, "EvenOdd leaves inner empty: alpha={eo_inner}");

    // Outer ring pixel (1, 1) is filled by both rules.
    assert!(alpha_at(nonzero.pixels(), 1, 1) > 240);
    assert!(alpha_at(evenodd.pixels(), 1, 1) > 240);
    Ok(())
}

/// `clip_path` masks subsequent draws to the path interior.
#[test]
fn clip_path_clips_drawing() -> Result<()> {
    let backend = NativeBackend::new();
    let mut surface = backend.create_surface(8, 8, SurfaceOptions::default())?;
    let clip = NativePath::from_svg("M2 2 L6 2 L6 6 L2 6 Z", FillRule::NonZero)?;
    surface.with_canvas(|canvas| {
        canvas.clear(RgbaLinear::new_premultiplied(0.0, 0.0, 0.0, 0.0));
        canvas.save();
        canvas.clip_path(&clip);
        canvas.draw_rect(
            Rect::from_xywh(0.0, 0.0, 8.0, 8.0),
            &NativePaint::fill(RgbaLinear::opaque(1.0, 0.0, 0.0)),
        );
        canvas.restore();
    });
    let px = surface.read_pixels()?;
    let stride = px.stride();
    let alpha_at = |x: usize, y: usize| px.pixels()[y * stride + x * 4 + 3];
    assert_eq!(alpha_at(0, 0), 0, "outside path stays transparent");
    assert_eq!(alpha_at(7, 7), 0, "outside path stays transparent");
    assert!(alpha_at(4, 4) > 240, "inside path is filled");
    Ok(())
}

/// `draw_line` honours stroke width: a horizontal line with width=4 covers
/// the y rows immediately above/below the line midpoint.
#[test]
fn draw_line_respects_stroke_width() -> Result<()> {
    let backend = NativeBackend::new();
    let mut surface = backend.create_surface(8, 8, SurfaceOptions::default())?;
    let mut paint = NativePaint::stroke(RgbaLinear::opaque(1.0, 1.0, 1.0), 4.0);
    paint.set_anti_alias(false);
    surface.with_canvas(|canvas| {
        canvas.clear(RgbaLinear::new_premultiplied(0.0, 0.0, 0.0, 0.0));
        canvas.draw_line(Point::new(0.0, 4.0), Point::new(8.0, 4.0), &paint);
    });
    let px = surface.read_pixels()?;
    let stride = px.stride();
    let alpha_at = |x: usize, y: usize| px.pixels()[y * stride + x * 4 + 3];
    // Line is centered at y=4 with width 4, so y in {2,3,4,5} are covered.
    assert_eq!(alpha_at(4, 0), 0, "row 0 outside stroke band");
    assert!(alpha_at(4, 3) > 240, "row 3 inside stroke band");
    assert!(alpha_at(4, 4) > 240, "row 4 (line center) covered");
    assert_eq!(alpha_at(4, 7), 0, "row 7 outside stroke band");
    Ok(())
}

/// `draw_line` with a round cap extends past the line endpoints, while a
/// butt cap stops cleanly. Sample alpha just before x=0 with the same line
/// segment under both caps.
#[test]
fn draw_line_round_cap_extends_past_endpoints() -> Result<()> {
    let backend = NativeBackend::new();
    let alpha_at = |cap: StrokeCap| -> Result<u8> {
        let mut surface = backend.create_surface(16, 8, SurfaceOptions::default())?;
        let mut paint = NativePaint::stroke(RgbaLinear::opaque(1.0, 1.0, 1.0), 4.0);
        paint.set_stroke_cap(cap);
        paint.set_anti_alias(false);
        surface.with_canvas(|canvas| {
            canvas.clear(RgbaLinear::new_premultiplied(0.0, 0.0, 0.0, 0.0));
            // Line from (4,4) to (12,4): butt cap stops at x=4, round cap
            // extends ~2px further (radius == half stroke width).
            canvas.draw_line(Point::new(4.0, 4.0), Point::new(12.0, 4.0), &paint);
        });
        let px = surface.read_pixels()?;
        let stride = px.stride();
        Ok(px.pixels()[4 * stride + 2 * 4 + 3])
    };
    let butt = alpha_at(StrokeCap::Butt)?;
    let round = alpha_at(StrokeCap::Round)?;
    assert_eq!(butt, 0, "butt cap stops at x=4");
    assert!(
        round > 200,
        "round cap extends past endpoint: alpha={round}"
    );
    Ok(())
}

/// `draw_line` with a dash leaves periodic gaps along the path.
#[test]
fn draw_line_dash_creates_periodic_gaps() -> Result<()> {
    let backend = NativeBackend::new();
    let mut surface = backend.create_surface(40, 4, SurfaceOptions::default())?;
    let mut paint = NativePaint::stroke(RgbaLinear::opaque(1.0, 1.0, 1.0), 2.0);
    paint.set_dash(vec![4.0, 4.0], 0.0);
    paint.set_anti_alias(false);
    surface.with_canvas(|canvas| {
        canvas.clear(RgbaLinear::new_premultiplied(0.0, 0.0, 0.0, 0.0));
        canvas.draw_line(Point::new(0.0, 2.0), Point::new(40.0, 2.0), &paint);
    });
    let px = surface.read_pixels()?;
    let stride = px.stride();
    // Sample row y=1 (just above the y=2 line center; the 2-px stroke
    // covers y=1 and y=2 with butt caps and no anti-alias).
    let row_start = stride;
    let row = &px.pixels()[row_start..row_start + stride];
    let alphas: Vec<u8> = row.chunks_exact(4).map(|p| p[3]).collect();
    let visible = alphas.iter().filter(|a| **a > 0).count();
    let invisible = alphas.iter().filter(|a| **a == 0).count();
    assert!(visible > 0, "dashed line should still produce some pixels");
    assert!(invisible > 0, "dashed line should leave some gaps");
    Ok(())
}

/// `draw_image_src` crops the source rect when scaling. A 4x4 source with
/// red top-left and blue bottom-right: drawing src=(0,0,2,2) (red region)
/// stretched to fill an 8x8 destination must be uniformly red.
#[test]
fn draw_image_src_crops_source_rect() -> Result<()> {
    let backend = NativeBackend::new();
    let mut source = backend.create_surface(4, 4, SurfaceOptions::default())?;
    source.with_canvas(|canvas| {
        canvas.clear(RgbaLinear::opaque(0.0, 0.0, 1.0));
        canvas.draw_rect(
            Rect::from_xywh(0.0, 0.0, 2.0, 2.0),
            &NativePaint::fill(RgbaLinear::opaque(1.0, 0.0, 0.0)),
        );
    });
    let image = source.snapshot();

    let mut dest = backend.create_surface(8, 8, SurfaceOptions::default())?;
    dest.with_canvas(|canvas| {
        canvas.clear(RgbaLinear::new_premultiplied(0.0, 0.0, 0.0, 0.0));
        canvas.draw_image_src(
            &image,
            Rect::from_xywh(0.0, 0.0, 2.0, 2.0),
            Rect::from_xywh(0.0, 0.0, 8.0, 8.0),
            None,
            SamplingMode::Nearest,
        );
    });
    let px = dest.read_pixels()?;
    let stride = px.stride();
    let pixel_at = |x: usize, y: usize| -> [u8; 4] {
        let off = y * stride + x * 4;
        [
            px.pixels()[off],
            px.pixels()[off + 1],
            px.pixels()[off + 2],
            px.pixels()[off + 3],
        ]
    };
    // Every destination pixel should reflect the red source region. With
    // unpremul Uint8, red opaque is approximately (255, 0, 0, 255).
    for y in 0..8 {
        for x in 0..8 {
            let p = pixel_at(x, y);
            assert!(p[0] > 240, "({x},{y}) red expected, got {p:?}");
            assert!(p[2] < 16, "({x},{y}) blue should be absent, got {p:?}");
        }
    }
    Ok(())
}

/// `SamplingMode::Nearest` preserves the hard edge between adjacent
/// source pixels of different colors when upscaling. A 2x2 red/blue
/// checker scaled to 8x8 must show a sharp red->blue transition at x=4.
#[test]
fn sampling_nearest_preserves_hard_edges() -> Result<()> {
    let backend = NativeBackend::new();
    let mut source = backend.create_surface(2, 2, SurfaceOptions::default())?;
    source.with_canvas(|canvas| {
        canvas.draw_rect(
            Rect::from_xywh(0.0, 0.0, 1.0, 1.0),
            &NativePaint::fill(RgbaLinear::opaque(1.0, 0.0, 0.0)),
        );
        canvas.draw_rect(
            Rect::from_xywh(1.0, 0.0, 1.0, 1.0),
            &NativePaint::fill(RgbaLinear::opaque(0.0, 0.0, 1.0)),
        );
        canvas.draw_rect(
            Rect::from_xywh(0.0, 1.0, 1.0, 1.0),
            &NativePaint::fill(RgbaLinear::opaque(0.0, 0.0, 1.0)),
        );
        canvas.draw_rect(
            Rect::from_xywh(1.0, 1.0, 1.0, 1.0),
            &NativePaint::fill(RgbaLinear::opaque(1.0, 0.0, 0.0)),
        );
    });
    let image = source.snapshot();

    let mut dest = backend.create_surface(8, 8, SurfaceOptions::default())?;
    dest.with_canvas(|canvas| {
        canvas.draw_image_src(
            &image,
            Rect::from_xywh(0.0, 0.0, 2.0, 2.0),
            Rect::from_xywh(0.0, 0.0, 8.0, 8.0),
            None,
            SamplingMode::Nearest,
        );
    });
    let px = dest.read_pixels()?;
    let stride = px.stride();
    // Just before transition (x=3) and just after (x=4) on row 0.
    let left = (px.pixels()[3 * 4], px.pixels()[3 * 4 + 2]);
    let right = (px.pixels()[4 * 4], px.pixels()[4 * 4 + 2]);
    let _ = stride; // unused: row 0 starts at byte 0.
    assert!(left.0 > 240, "left of edge: red dominant, got r={}", left.0);
    assert!(left.1 < 16, "left of edge: blue absent, got b={}", left.1);
    assert!(right.0 < 16, "right of edge: red absent, got r={}", right.0);
    assert!(
        right.1 > 240,
        "right of edge: blue dominant, got b={}",
        right.1
    );
    Ok(())
}

/// `SamplingMode::Linear` and `Mipmapped` produce non-empty output without
/// panicking. Exact pixels are backend-sensitive, so we only smoke-test
/// that the draw succeeds and writes some non-zero pixels.
#[test]
fn sampling_linear_and_mipmapped_smoke() -> Result<()> {
    let backend = NativeBackend::new();
    let mut source = backend.create_surface(2, 2, SurfaceOptions::default())?;
    source.with_canvas(|canvas| {
        canvas.clear(RgbaLinear::opaque(0.5, 0.5, 0.5));
    });
    let image = source.snapshot();

    for mode in [SamplingMode::Linear, SamplingMode::Mipmapped] {
        let mut dest = backend.create_surface(8, 8, SurfaceOptions::default())?;
        dest.with_canvas(|canvas| {
            canvas.clear(RgbaLinear::new_premultiplied(0.0, 0.0, 0.0, 0.0));
            canvas.draw_image_src(
                &image,
                Rect::from_xywh(0.0, 0.0, 2.0, 2.0),
                Rect::from_xywh(0.0, 0.0, 8.0, 8.0),
                None,
                mode,
            );
        });
        let px = dest.read_pixels()?;
        assert!(
            px.pixels().iter().any(|c| *c != 0),
            "{mode:?} should produce non-zero pixels"
        );
    }
    Ok(())
}

// --- Chunk 4A: filters and color filters -----------------------------------

/// Blur softens and expands non-transparent regions: pixels just outside
/// the original sharp rect must gain non-zero alpha.
#[test]
fn image_filter_blur_expands_alpha() -> Result<()> {
    let backend = NativeBackend::new();
    let mut surface = backend.create_surface(16, 16, SurfaceOptions::default())?;
    let mut paint = NativePaint::fill(RgbaLinear::opaque(1.0, 1.0, 1.0));
    paint.set_image_filter(Some(NativeImageFilter::blur(3.0, 3.0, None)?));
    surface.with_canvas(|canvas| {
        canvas.clear(RgbaLinear::new_premultiplied(0.0, 0.0, 0.0, 0.0));
        canvas.draw_rect(Rect::from_xywh(6.0, 6.0, 4.0, 4.0), &paint);
    });
    let px = surface.read_pixels()?;
    let stride = px.stride();
    let alpha_at = |x: usize, y: usize| px.pixels()[y * stride + x * 4 + 3];
    // Interior alpha softens under strong blur but remains substantially
    // above zero.
    assert!(
        alpha_at(7, 7) > 32,
        "rect interior keeps alpha under blur, got {}",
        alpha_at(7, 7)
    );
    // Just outside the rect on each side: blur leaks alpha into the halo.
    assert!(alpha_at(4, 8) > 8, "left of rect should have blur halo");
    assert!(alpha_at(11, 8) > 8, "right of rect should have blur halo");
    assert!(alpha_at(8, 4) > 8, "above rect should have blur halo");
    assert!(alpha_at(8, 11) > 8, "below rect should have blur halo");
    Ok(())
}

/// Drop shadow renders an offset blur of the source. Pixels at the offset
/// position (outside the source rect) must show the shadow color.
#[test]
fn image_filter_drop_shadow_offsets_pixels() -> Result<()> {
    let backend = NativeBackend::new();
    let mut surface = backend.create_surface(16, 16, SurfaceOptions::default())?;
    let mut paint = NativePaint::fill(RgbaLinear::opaque(1.0, 1.0, 1.0));
    paint.set_image_filter(Some(NativeImageFilter::drop_shadow(
        4.0,
        4.0,
        1.0,
        1.0,
        RgbaLinear::opaque(1.0, 0.0, 0.0),
        None,
    )?));
    surface.with_canvas(|canvas| {
        canvas.clear(RgbaLinear::new_premultiplied(0.0, 0.0, 0.0, 0.0));
        canvas.draw_rect(Rect::from_xywh(2.0, 2.0, 4.0, 4.0), &paint);
    });
    let px = surface.read_pixels()?;
    let stride = px.stride();
    // The shadow centers at (offset + rect): around (6..10, 6..10). Sample
    // a pixel inside the shadow region (well outside the source rect).
    let off = 8 * stride + 8 * 4;
    let r = px.pixels()[off];
    let a = px.pixels()[off + 3];
    assert!(a > 32, "drop shadow region has alpha: {a}");
    assert!(r > 64, "drop shadow region carries shadow red: r={r}");
    Ok(())
}

/// A color matrix can replace RGB. Using a matrix that swaps red and blue,
/// drawing a blue rect produces red pixels.
#[test]
fn image_filter_color_matrix_replaces_rgb() -> Result<()> {
    let backend = NativeBackend::new();
    let mut surface = backend.create_surface(4, 4, SurfaceOptions::default())?;
    // Swap red and blue (rows are RGBA; columns are R G B A offset).
    let swap_rb: [f32; 20] = [
        0.0, 0.0, 1.0, 0.0, 0.0, // r_out = b_in
        0.0, 1.0, 0.0, 0.0, 0.0, // g_out = g_in
        1.0, 0.0, 0.0, 0.0, 0.0, // b_out = r_in
        0.0, 0.0, 0.0, 1.0, 0.0, // a_out = a_in
    ];
    let mut paint = NativePaint::fill(RgbaLinear::opaque(0.0, 0.0, 1.0));
    paint.set_image_filter(Some(NativeImageFilter::color_matrix(swap_rb, None)?));
    surface.with_canvas(|canvas| {
        canvas.clear(RgbaLinear::new_premultiplied(0.0, 0.0, 0.0, 0.0));
        canvas.draw_rect(Rect::from_xywh(0.0, 0.0, 4.0, 4.0), &paint);
    });
    let px = surface.read_pixels()?;
    let r = px.pixels()[0];
    let b = px.pixels()[2];
    assert!(r > 240, "red dominant after RB swap: {r}");
    assert!(b < 16, "blue absent after RB swap: {b}");
    Ok(())
}

/// `from_color_filter` wraps a color filter as an image filter and
/// produces the same effect when applied via `paint.image_filter`.
#[test]
fn image_filter_from_color_filter_applies_as_image_filter() -> Result<()> {
    let backend = NativeBackend::new();
    let mut surface = backend.create_surface(4, 4, SurfaceOptions::default())?;
    let cf = NativeColorFilter::linear_to_srgb_gamma();
    let if_ = NativeImageFilter::from_color_filter(cf, None)?;
    let mut paint = NativePaint::fill(RgbaLinear::opaque(0.5, 0.5, 0.5));
    paint.set_image_filter(Some(if_));
    surface.with_canvas(|canvas| {
        canvas.clear(RgbaLinear::new_premultiplied(0.0, 0.0, 0.0, 0.0));
        canvas.draw_rect(Rect::from_xywh(0.0, 0.0, 4.0, 4.0), &paint);
    });
    let px = surface.read_pixels()?;
    // Wrapping a color filter as an image filter should still produce
    // non-zero pixels for an opaque draw.
    assert!(px.pixels()[3] > 240, "opaque draw remains opaque");
    Ok(())
}

/// Composing two image filters chains them: outer(inner(source)).
/// Compose blur(8) outer with color_matrix(swap RB) inner. Inner runs first
/// (turns blue source into red source in the filter pipeline), then blur
/// expands. Sample outside the source rect: blurred red appears.
#[test]
fn image_filter_compose_chains_inner_then_outer() -> Result<()> {
    let backend = NativeBackend::new();
    let mut surface = backend.create_surface(16, 16, SurfaceOptions::default())?;
    let swap_rb: [f32; 20] = [
        0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
        1.0, 0.0,
    ];
    let inner = NativeImageFilter::color_matrix(swap_rb, None)?;
    let outer = NativeImageFilter::blur(2.0, 2.0, None)?;
    let composed = NativeImageFilter::compose(outer, inner)?;
    let mut paint = NativePaint::fill(RgbaLinear::opaque(0.0, 0.0, 1.0));
    paint.set_image_filter(Some(composed));
    surface.with_canvas(|canvas| {
        canvas.clear(RgbaLinear::new_premultiplied(0.0, 0.0, 0.0, 0.0));
        canvas.draw_rect(Rect::from_xywh(6.0, 6.0, 4.0, 4.0), &paint);
    });
    let px = surface.read_pixels()?;
    let stride = px.stride();
    let off = 8 * stride + 8 * 4;
    let r = px.pixels()[off];
    let b = px.pixels()[off + 2];
    assert!(
        r > 200,
        "composed pipeline turned blue input into red: r={r}"
    );
    assert!(b < 32, "blue should be absent: b={b}");
    Ok(())
}

/// Luma color filter: alpha = perceived luminance, RGB = 0. Drawing a white
/// fill with luma applied keeps the surface visible (alpha 1); drawing a
/// black fill becomes invisible (alpha 0). This is the building block for
/// destination-in mask paths.
#[test]
fn color_filter_luma_maps_luminance_to_alpha() -> Result<()> {
    let backend = NativeBackend::new();
    let render_with = |color: RgbaLinear| -> Result<u8> {
        let mut surface = backend.create_surface(2, 2, SurfaceOptions::default())?;
        let mut paint = NativePaint::fill(color);
        paint.set_color_filter(Some(NativeColorFilter::luma()));
        surface.with_canvas(|canvas| {
            canvas.clear(RgbaLinear::new_premultiplied(0.0, 0.0, 0.0, 0.0));
            canvas.draw_rect(Rect::from_xywh(0.0, 0.0, 2.0, 2.0), &paint);
        });
        let px = surface.read_pixels()?;
        Ok(px.pixels()[3])
    };
    let white_alpha = render_with(RgbaLinear::opaque(1.0, 1.0, 1.0))?;
    let black_alpha = render_with(RgbaLinear::opaque(0.0, 0.0, 0.0))?;
    assert!(
        white_alpha > 240,
        "luma maps white -> high alpha, got {white_alpha}"
    );
    assert!(
        black_alpha < 16,
        "luma maps black -> ~0 alpha, got {black_alpha}"
    );
    Ok(())
}

/// `srgb_to_linear_gamma` and `linear_to_srgb_gamma` round-trip: applying
/// inner srgb_to_linear and outer linear_to_srgb produces visually similar
/// output to the original draw on an sRGB-coded readback.
#[test]
fn color_filter_gamma_round_trip_through_compose() -> Result<()> {
    let backend = NativeBackend::new();
    let direct = {
        let mut surface = backend.create_surface(2, 2, SurfaceOptions::default())?;
        surface.with_canvas(|canvas| {
            canvas.clear(RgbaLinear::new_premultiplied(0.0, 0.0, 0.0, 0.0));
            canvas.draw_rect(
                Rect::from_xywh(0.0, 0.0, 2.0, 2.0),
                &NativePaint::fill(RgbaLinear::opaque(0.5, 0.5, 0.5)),
            );
        });
        surface.read_pixels()?
    };

    let composed = {
        let outer = NativeColorFilter::linear_to_srgb_gamma();
        let inner = NativeColorFilter::srgb_to_linear_gamma();
        let cf = NativeColorFilter::compose(outer, inner)?;
        let mut paint = NativePaint::fill(RgbaLinear::opaque(0.5, 0.5, 0.5));
        paint.set_color_filter(Some(cf));
        let mut surface = backend.create_surface(2, 2, SurfaceOptions::default())?;
        surface.with_canvas(|canvas| {
            canvas.clear(RgbaLinear::new_premultiplied(0.0, 0.0, 0.0, 0.0));
            canvas.draw_rect(Rect::from_xywh(0.0, 0.0, 2.0, 2.0), &paint);
        });
        surface.read_pixels()?
    };

    let dr = (direct.pixels()[0] as i16 - composed.pixels()[0] as i16).abs();
    let dg = (direct.pixels()[1] as i16 - composed.pixels()[1] as i16).abs();
    let db = (direct.pixels()[2] as i16 - composed.pixels()[2] as i16).abs();
    assert!(
        dr <= 8 && dg <= 8 && db <= 8,
        "gamma round-trip differs by at most 8/255: dr={dr} dg={dg} db={db}"
    );
    Ok(())
}

// --- Chunk 4B: shaders -----------------------------------------------------

/// Gradient stops must be sorted by position. An out-of-order stop list
/// must surface a typed error rather than silently rendering.
#[test]
fn gradient_unsorted_stops_returns_invalid_gradient_error() {
    let result = NativeShader::linear_gradient(
        Point::new(0.0, 0.0),
        Point::new(16.0, 0.0),
        &[
            GradientStop {
                position: 0.5,
                color: RgbaLinear::opaque(1.0, 0.0, 0.0),
            },
            GradientStop {
                position: 0.0,
                color: RgbaLinear::opaque(0.0, 0.0, 1.0),
            },
        ],
        GradientInterpolation::Srgb,
    );
    assert!(
        matches!(result, Err(NativeError::InvalidGradient { .. })),
        "expected InvalidGradient, got {result:?}"
    );
}

#[test]
fn gradient_requires_at_least_two_stops() {
    let result = NativeShader::linear_gradient(
        Point::new(0.0, 0.0),
        Point::new(16.0, 0.0),
        &[GradientStop {
            position: 0.0,
            color: RgbaLinear::opaque(1.0, 0.0, 0.0),
        }],
        GradientInterpolation::Srgb,
    );
    assert!(matches!(result, Err(NativeError::InvalidGradient { .. })));
}

/// Linear sRGB gradient renders the expected endpoints. A 16x1 horizontal
/// red->blue gradient must show ~red at x=0 and ~blue at x=15.
#[test]
fn linear_gradient_srgb_renders_endpoints() -> Result<()> {
    let backend = NativeBackend::new();
    let mut surface = backend.create_surface(16, 1, SurfaceOptions::default())?;
    let shader = NativeShader::linear_gradient(
        Point::new(0.0, 0.0),
        Point::new(16.0, 0.0),
        &[
            GradientStop {
                position: 0.0,
                color: RgbaLinear::opaque(1.0, 0.0, 0.0),
            },
            GradientStop {
                position: 1.0,
                color: RgbaLinear::opaque(0.0, 0.0, 1.0),
            },
        ],
        GradientInterpolation::Srgb,
    )?;
    let mut paint = NativePaint::default();
    paint.set_shader(Some(shader));
    surface.with_canvas(|canvas| {
        canvas.draw_rect(Rect::from_xywh(0.0, 0.0, 16.0, 1.0), &paint);
    });
    let px = surface.read_pixels()?;
    // Pixel centers are at x=0.5 and x=15.5, so a small linear
    // contribution from the far stop survives. With sRGB-gamma Uint8
    // readback, a 0.03 linear blue rounds to ~50 in u8, so thresholds
    // are loose. The dominant channel must remain clearly dominant.
    let r0 = px.pixels()[0];
    let b0 = px.pixels()[2];
    assert!(r0 > 200, "left endpoint ~ red, got r={r0}");
    assert!(b0 < 80, "left endpoint mostly red, got b={b0}");
    let r15 = px.pixels()[15 * 4];
    let b15 = px.pixels()[15 * 4 + 2];
    assert!(r15 < 80, "right endpoint mostly blue, got r={r15}");
    assert!(b15 > 200, "right endpoint ~ blue, got b={b15}");
    Ok(())
}

/// Stops at non-extreme positions are honoured. Three-stop red->green->blue
/// gradient: x=0 red, x=8 green, x=15 blue.
#[test]
fn linear_gradient_three_stops_renders_in_order() -> Result<()> {
    let backend = NativeBackend::new();
    let mut surface = backend.create_surface(16, 1, SurfaceOptions::default())?;
    let shader = NativeShader::linear_gradient(
        Point::new(0.0, 0.0),
        Point::new(16.0, 0.0),
        &[
            GradientStop {
                position: 0.0,
                color: RgbaLinear::opaque(1.0, 0.0, 0.0),
            },
            GradientStop {
                position: 0.5,
                color: RgbaLinear::opaque(0.0, 1.0, 0.0),
            },
            GradientStop {
                position: 1.0,
                color: RgbaLinear::opaque(0.0, 0.0, 1.0),
            },
        ],
        GradientInterpolation::Srgb,
    )?;
    let mut paint = NativePaint::default();
    paint.set_shader(Some(shader));
    surface.with_canvas(|canvas| {
        canvas.draw_rect(Rect::from_xywh(0.0, 0.0, 16.0, 1.0), &paint);
    });
    let px = surface.read_pixels()?;
    let r0 = px.pixels()[0];
    let g8 = px.pixels()[8 * 4 + 1];
    let b15 = px.pixels()[15 * 4 + 2];
    assert!(r0 > 200, "x=0 ~ red, got r={r0}");
    assert!(g8 > 150, "x=8 ~ green, got g={g8}");
    assert!(b15 > 200, "x=15 ~ blue, got b={b15}");
    Ok(())
}

/// OKLCH interpolation produces a perceptually different intermediate
/// from sRGB interpolation. Red->blue at the midpoint:
/// - sRGB linear: leans toward dark purple/grey, very low green.
/// - OKLCH: passes through more saturated colors with higher visible
///   intensity in non-red, non-blue channels.
///
/// We assert that the midpoint pixel differs by at least one channel
/// across the two interpolations. Exact values are backend-sensitive.
#[test]
fn linear_gradient_oklch_differs_from_srgb_at_midpoint() -> Result<()> {
    let backend = NativeBackend::new();
    let render = |interp: GradientInterpolation| -> Result<[u8; 4]> {
        let mut surface = backend.create_surface(16, 1, SurfaceOptions::default())?;
        let shader = NativeShader::linear_gradient(
            Point::new(0.0, 0.0),
            Point::new(16.0, 0.0),
            &[
                GradientStop {
                    position: 0.0,
                    color: RgbaLinear::opaque(1.0, 0.0, 0.0),
                },
                GradientStop {
                    position: 1.0,
                    color: RgbaLinear::opaque(0.0, 0.0, 1.0),
                },
            ],
            interp,
        )?;
        let mut paint = NativePaint::default();
        paint.set_shader(Some(shader));
        surface.with_canvas(|canvas| {
            canvas.draw_rect(Rect::from_xywh(0.0, 0.0, 16.0, 1.0), &paint);
        });
        let px = surface.read_pixels()?;
        Ok([
            px.pixels()[8 * 4],
            px.pixels()[8 * 4 + 1],
            px.pixels()[8 * 4 + 2],
            px.pixels()[8 * 4 + 3],
        ])
    };
    let srgb = render(GradientInterpolation::Srgb)?;
    let oklch = render(GradientInterpolation::Oklch)?;
    assert_ne!(
        srgb, oklch,
        "OKLCH must produce a different midpoint than sRGB-linear; got equal {srgb:?}"
    );
    Ok(())
}

/// `paint.set_shader(None)` clears the shader so the paint's `color`
/// drives the draw again.
#[test]
fn paint_set_shader_none_falls_back_to_color() -> Result<()> {
    let backend = NativeBackend::new();
    let mut surface = backend.create_surface(2, 1, SurfaceOptions::default())?;
    let mut paint = NativePaint::fill(RgbaLinear::opaque(1.0, 0.0, 0.0));
    let shader = NativeShader::linear_gradient(
        Point::new(0.0, 0.0),
        Point::new(2.0, 0.0),
        &[
            GradientStop {
                position: 0.0,
                color: RgbaLinear::opaque(0.0, 1.0, 0.0),
            },
            GradientStop {
                position: 1.0,
                color: RgbaLinear::opaque(0.0, 1.0, 0.0),
            },
        ],
        GradientInterpolation::Srgb,
    )?;
    paint.set_shader(Some(shader));
    paint.set_shader(None);
    surface.with_canvas(|canvas| {
        canvas.draw_rect(Rect::from_xywh(0.0, 0.0, 2.0, 1.0), &paint);
    });
    let px = surface.read_pixels()?;
    let r0 = px.pixels()[0];
    let g0 = px.pixels()[1];
    assert!(r0 > 200, "color path renders red, got r={r0}");
    assert!(g0 < 32, "no green when shader cleared, got g={g0}");
    Ok(())
}

// --- Chunk 5: NativeImage::from_pixels -------------------------------------

/// Construct a 4x4 raw RGBA8 unpremul image and draw it to a surface.
/// All destination pixels should reflect the source's red region.
#[test]
fn from_pixels_rgba8_unpremul_draws() -> Result<()> {
    // 4x4 solid red, opaque, unpremul Uint8.
    let mut bytes = vec![0u8; 4 * 4 * 4];
    for px in bytes.chunks_exact_mut(4) {
        px[0] = 255;
        px[3] = 255;
    }
    let image = NativeImage::from_pixels(
        &bytes,
        4,
        4,
        4 * 4,
        PixelFormat::Rgba8UnormUnpremul,
        PixelColorSpace::Srgb,
    )?;
    assert_eq!(image.width(), 4);
    assert_eq!(image.height(), 4);
    assert!(
        !image.is_premultiplied(),
        "Unpremul format reports unpremultiplied"
    );

    let backend = NativeBackend::new();
    let mut surface = backend.create_surface(4, 4, SurfaceOptions::default())?;
    surface.with_canvas(|canvas| {
        canvas.clear(RgbaLinear::new_premultiplied(0.0, 0.0, 0.0, 0.0));
        canvas.draw_image_src(
            &image,
            Rect::from_xywh(0.0, 0.0, 4.0, 4.0),
            Rect::from_xywh(0.0, 0.0, 4.0, 4.0),
            None,
            SamplingMode::Nearest,
        );
    });
    let px = surface.read_pixels()?;
    let r = px.pixels()[0];
    let a = px.pixels()[3];
    assert!(r > 240, "destination red, got r={r}");
    assert!(a > 240, "destination opaque, got a={a}");
    Ok(())
}

/// `from_pixels` rejects zero dimensions with `InvalidDimensions`.
#[test]
fn from_pixels_zero_dimensions_returns_error() {
    let result = NativeImage::from_pixels(
        &[],
        0,
        4,
        0,
        PixelFormat::Rgba8UnormUnpremul,
        PixelColorSpace::Srgb,
    );
    assert!(matches!(result, Err(NativeError::InvalidDimensions { .. })));
}

/// `from_pixels` rejects strides smaller than `width * bytes_per_pixel`.
#[test]
fn from_pixels_invalid_stride_returns_error() {
    // 4 width Uint8 RGBA needs >= 16 bytes per row.
    let bytes = vec![0u8; 8 * 4];
    let result = NativeImage::from_pixels(
        &bytes,
        4,
        4,
        8, // too small: would need >= 16
        PixelFormat::Rgba8UnormUnpremul,
        PixelColorSpace::Srgb,
    );
    assert!(matches!(
        result,
        Err(NativeError::InvalidStride {
            expected: 16,
            actual: 8
        })
    ));
}

/// `from_pixels` rejects buffers whose length does not equal `stride * height`.
#[test]
fn from_pixels_invalid_byte_length_returns_error() {
    let bytes = vec![0u8; 32]; // expected 4 * 4 * 4 = 64
    let result = NativeImage::from_pixels(
        &bytes,
        4,
        4,
        16,
        PixelFormat::Rgba8UnormUnpremul,
        PixelColorSpace::Srgb,
    );
    assert!(matches!(
        result,
        Err(NativeError::InvalidByteLength {
            expected: 64,
            actual: 32
        })
    ));
}

/// F32 linear premul preserves HDR values above 1.0 through draw + readback
/// without clamping at the image boundary. Encode a 2x2 image where r = 2.0
/// and read it back through a linear F32 surface.
#[test]
fn from_pixels_f32_preserves_hdr_values() -> Result<()> {
    // Each pixel is 16 bytes (4 f32 channels). 2x2 image.
    let mut bytes = vec![0u8; 2 * 2 * 16];
    for px in bytes.chunks_exact_mut(16) {
        // r = 2.0, g = 0, b = 0, a = 1.0 (premultiplied means a stays 1.0
        // and r already encodes the post-premul value).
        px[0..4].copy_from_slice(&2.0f32.to_le_bytes());
        px[4..8].copy_from_slice(&0.0f32.to_le_bytes());
        px[8..12].copy_from_slice(&0.0f32.to_le_bytes());
        px[12..16].copy_from_slice(&1.0f32.to_le_bytes());
    }
    let image = NativeImage::from_pixels(
        &bytes,
        2,
        2,
        2 * 16,
        PixelFormat::Rgba32fPremul,
        PixelColorSpace::SrgbLinear,
    )?;
    assert_eq!(image.width(), 2);
    assert!(image.is_premultiplied(), "F32Premul reports premultiplied");

    let backend = NativeBackend::new();
    let mut surface = backend.create_surface(2, 2, SurfaceOptions::default())?;
    surface.with_canvas(|canvas| {
        canvas.clear(RgbaLinear::new_premultiplied(0.0, 0.0, 0.0, 0.0));
        canvas.draw_image_src(
            &image,
            Rect::from_xywh(0.0, 0.0, 2.0, 2.0),
            Rect::from_xywh(0.0, 0.0, 2.0, 2.0),
            None,
            SamplingMode::Nearest,
        );
    });

    let exported = surface.read_pixels_linear()?;
    let r = f32::from_le_bytes([
        exported.pixels()[0],
        exported.pixels()[1],
        exported.pixels()[2],
        exported.pixels()[3],
    ]);
    assert!(
        r > 1.5,
        "HDR red preserved through F32 image+surface, got r={r}"
    );
    Ok(())
}

/// F16 premul also preserves HDR values. Use half-precision encoding to
/// store r = 2.0 and verify it survives the round trip.
#[test]
fn from_pixels_f16_preserves_hdr_values() -> Result<()> {
    fn f32_to_f16_bits(v: f32) -> u16 {
        // IEEE 754 binary16 round-to-nearest-even encoder, sufficient for
        // exact representation of 2.0.
        let bits = v.to_bits();
        let sign = (bits >> 16) as u16 & 0x8000;
        let mantissa = bits & 0x007f_ffff;
        let exp = ((bits >> 23) & 0xff) as i32 - 127 + 15;
        if exp <= 0 {
            sign
        } else if exp >= 31 {
            sign | 0x7c00
        } else {
            sign | ((exp as u16) << 10) | ((mantissa >> 13) as u16)
        }
    }

    let r_bits = f32_to_f16_bits(2.0);
    let zero_bits = f32_to_f16_bits(0.0);
    let one_bits = f32_to_f16_bits(1.0);

    let mut bytes = vec![0u8; 2 * 2 * 8];
    for px in bytes.chunks_exact_mut(8) {
        px[0..2].copy_from_slice(&r_bits.to_le_bytes());
        px[2..4].copy_from_slice(&zero_bits.to_le_bytes());
        px[4..6].copy_from_slice(&zero_bits.to_le_bytes());
        px[6..8].copy_from_slice(&one_bits.to_le_bytes());
    }

    let image = NativeImage::from_pixels(
        &bytes,
        2,
        2,
        2 * 8,
        PixelFormat::Rgba16fPremul,
        PixelColorSpace::SrgbLinear,
    )?;

    let backend = NativeBackend::new();
    let mut surface = backend.create_surface(2, 2, SurfaceOptions::default())?;
    surface.with_canvas(|canvas| {
        canvas.clear(RgbaLinear::new_premultiplied(0.0, 0.0, 0.0, 0.0));
        canvas.draw_image_src(
            &image,
            Rect::from_xywh(0.0, 0.0, 2.0, 2.0),
            Rect::from_xywh(0.0, 0.0, 2.0, 2.0),
            None,
            SamplingMode::Nearest,
        );
    });

    let exported = surface.read_pixels_linear()?;
    let r = f32::from_le_bytes([
        exported.pixels()[0],
        exported.pixels()[1],
        exported.pixels()[2],
        exported.pixels()[3],
    ]);
    assert!(
        r > 1.5,
        "HDR red preserved through F16 image+surface, got r={r}"
    );
    Ok(())
}

/// Premultiplied alpha mode is reflected by `is_premultiplied()` and round
/// trips through draw/readback without unintended unpremultiplication.
/// Construct an Rgba8UnormPremul image with r = 0.5, a = 0.5 (premul values
/// 128 / 128) and verify that a premultiplied readback returns ~ (128,
/// 128) for r and a.
#[test]
fn from_pixels_premultiplied_round_trips() -> Result<()> {
    let mut bytes = vec![0u8; 4 * 4 * 4];
    for px in bytes.chunks_exact_mut(4) {
        // Already premul: r=0.5, alpha=0.5 => stored r=128, a=128.
        px[0] = 128;
        px[3] = 128;
    }
    let image = NativeImage::from_pixels(
        &bytes,
        4,
        4,
        16,
        PixelFormat::Rgba8UnormPremul,
        PixelColorSpace::SrgbLinear,
    )?;
    assert!(image.is_premultiplied());

    let backend = NativeBackend::new();
    let mut surface = backend.create_surface(4, 4, SurfaceOptions::default())?;
    surface.with_canvas(|canvas| {
        canvas.clear(RgbaLinear::new_premultiplied(0.0, 0.0, 0.0, 0.0));
        canvas.draw_image_src(
            &image,
            Rect::from_xywh(0.0, 0.0, 4.0, 4.0),
            Rect::from_xywh(0.0, 0.0, 4.0, 4.0),
            None,
            SamplingMode::Nearest,
        );
    });
    let exported = surface.read_pixels_as(PixelExportOptions {
        color_space: PixelColorSpace::SrgbLinear,
        depth: PixelDepth::Uint8,
        premultiplied: true,
    })?;
    let r = exported.pixels()[0];
    let a = exported.pixels()[3];
    assert!(
        r.abs_diff(128) <= 4,
        "premul red round-trips ~ 128, got {r}"
    );
    assert!(
        a.abs_diff(128) <= 4,
        "premul alpha round-trips ~ 128, got {a}"
    );
    Ok(())
}

// --- Chunk 6: SVG path/image behavior --------------------------------------

/// Studio's `ShapeItem.pathData` exercises the full SVG path mini-language:
/// uppercase absolute commands and lowercase relative commands, with curves.
/// This test parses such a path and renders it: the result must show
/// non-trivial fill coverage.
#[test]
fn native_path_from_svg_handles_relative_and_curve_commands() -> Result<()> {
    let backend = NativeBackend::new();
    let mut surface = backend.create_surface(16, 16, SurfaceOptions::default())?;
    // Move to (2,2), draw a small loop using mixed relative line and cubic
    // curve commands, close. Equivalent shape: roughly a rounded blob.
    let svg_path = "M2 2 l 12 0 q 0 12 -12 12 z";
    let path = NativePath::from_svg(svg_path, FillRule::NonZero)?;
    surface.with_canvas(|canvas| {
        canvas.clear(RgbaLinear::new_premultiplied(0.0, 0.0, 0.0, 0.0));
        canvas.draw_path(&path, &NativePaint::fill(RgbaLinear::opaque(1.0, 0.0, 0.0)));
    });
    let px = surface.read_pixels()?;
    let stride = px.stride();
    let alpha_at = |x: usize, y: usize| px.pixels()[y * stride + x * 4 + 3];
    assert!(
        alpha_at(4, 4) > 200,
        "interior of relative-path blob is filled"
    );
    assert_eq!(alpha_at(0, 0), 0, "outside the path stays transparent");
    Ok(())
}

/// Pin Skia's behavior: `Image::from_encoded` does NOT decode SVG XML
/// (it is a raster-codec API). The wrapper surfaces this as a typed
/// `DecodeImage` error rather than silently returning a blank image.
/// Callers needing SVG must use `NativeImage::from_svg_xml`.
#[test]
fn from_encoded_does_not_decode_svg_xml() {
    let svg = b"<svg xmlns='http://www.w3.org/2000/svg' width='4' height='4'>\
                <rect width='4' height='4' fill='red'/>\
                </svg>";
    let result = NativeImage::from_encoded(svg);
    assert!(
        matches!(result, Err(NativeError::DecodeImage { .. })),
        "expected DecodeImage error, got {result:?}"
    );
}

/// `NativeImage::from_svg_xml` rasterizes a minimal SVG document into an
/// image of the requested dimensions. A red 4x4 rect should produce
/// non-zero red pixels when drawn onto a surface.
#[test]
fn from_svg_xml_rasterizes_minimal_svg() -> Result<()> {
    let svg = "<svg xmlns='http://www.w3.org/2000/svg' width='4' height='4'>\
               <rect width='4' height='4' fill='red'/>\
               </svg>";
    let image = NativeImage::from_svg_xml(svg, 4, 4)?;
    assert_eq!(image.width(), 4);
    assert_eq!(image.height(), 4);

    let backend = NativeBackend::new();
    let mut surface = backend.create_surface(4, 4, SurfaceOptions::default())?;
    surface.with_canvas(|canvas| {
        canvas.clear(RgbaLinear::new_premultiplied(0.0, 0.0, 0.0, 0.0));
        canvas.draw_image_src(
            &image,
            Rect::from_xywh(0.0, 0.0, 4.0, 4.0),
            Rect::from_xywh(0.0, 0.0, 4.0, 4.0),
            None,
            SamplingMode::Nearest,
        );
    });
    let px = surface.read_pixels()?;
    let r = px.pixels()[0];
    let a = px.pixels()[3];
    assert!(r > 200, "SVG rect renders red, got r={r}");
    assert!(a > 240, "SVG rect renders opaque, got a={a}");
    Ok(())
}

/// `from_svg_xml` rejects zero dimensions with `InvalidDimensions`.
#[test]
fn from_svg_xml_zero_dimensions_returns_error() {
    let svg = "<svg xmlns='http://www.w3.org/2000/svg' width='4' height='4'></svg>";
    let result = NativeImage::from_svg_xml(svg, 0, 4);
    assert!(matches!(result, Err(NativeError::InvalidDimensions { .. })));
}

/// `from_svg_xml` rejects malformed SVG XML with `DecodeImage`.
#[test]
fn from_svg_xml_malformed_xml_returns_decode_error() {
    let result = NativeImage::from_svg_xml("not actually svg", 4, 4);
    assert!(
        matches!(result, Err(NativeError::DecodeImage { .. })),
        "expected DecodeImage, got {result:?}"
    );
}

// --- Chunk 7A: NativeFontManager -------------------------------------------

const FONT_FIXTURE: &str = "tests/assets/Oswald/static/Oswald-Bold.ttf";

/// A fresh `NativeFontManager` reports no registered families.
#[test]
fn font_manager_starts_empty() {
    let mgr = NativeFontManager::new();
    assert!(mgr.families().is_empty());
    assert!(!mgr.has_font("Oswald"));
}

/// Registering a font from in-memory bytes makes the family visible via
/// `has_font` and `families`.
#[test]
fn font_manager_register_from_data_lists_family() -> Result<()> {
    let bytes = std::fs::read(FONT_FIXTURE)?;
    let mgr = NativeFontManager::new();
    mgr.register_font_from_data("Oswald Test", &bytes)?;
    assert!(mgr.has_font("Oswald Test"));
    assert_eq!(mgr.families(), vec!["Oswald Test".to_string()]);
    Ok(())
}

/// Registering from a file path is equivalent to registering from data.
#[test]
fn font_manager_register_from_path_lists_family() -> Result<()> {
    let mgr = NativeFontManager::new();
    mgr.register_font_from_path("Oswald Path", FONT_FIXTURE)?;
    assert!(mgr.has_font("Oswald Path"));
    Ok(())
}

/// Calling register twice for the same family does not duplicate the
/// alias in the families list.
#[test]
fn font_manager_register_same_family_does_not_duplicate() -> Result<()> {
    let bytes = std::fs::read(FONT_FIXTURE)?;
    let mgr = NativeFontManager::new();
    mgr.register_font_from_data("Oswald Dup", &bytes)?;
    mgr.register_font_from_data("Oswald Dup", &bytes)?;
    let families = mgr.families();
    assert_eq!(families, vec!["Oswald Dup".to_string()]);
    Ok(())
}

/// Multiple distinct families are tracked in registration order.
#[test]
fn font_manager_tracks_multiple_families_in_order() -> Result<()> {
    let bytes = std::fs::read(FONT_FIXTURE)?;
    let mgr = NativeFontManager::new();
    mgr.register_font_from_data("Family A", &bytes)?;
    mgr.register_font_from_data("Family B", &bytes)?;
    mgr.register_font_from_data("Family C", &bytes)?;
    assert_eq!(
        mgr.families(),
        vec![
            "Family A".to_string(),
            "Family B".to_string(),
            "Family C".to_string(),
        ]
    );
    assert!(mgr.has_font("Family A"));
    assert!(mgr.has_font("Family B"));
    assert!(mgr.has_font("Family C"));
    assert!(!mgr.has_font("Family D"));
    Ok(())
}

/// Garbage bytes return `FontRegister`, not a panic or silent success.
#[test]
fn font_manager_invalid_data_returns_font_register_error() {
    let mgr = NativeFontManager::new();
    let result = mgr.register_font_from_data("Garbage", b"not a real font");
    assert!(matches!(result, Err(NativeError::FontRegister { .. })));
    assert!(mgr.families().is_empty());
}

/// A non-existent path returns `FontRegister` with the IO error reason.
#[test]
fn font_manager_missing_path_returns_font_register_error() {
    let mgr = NativeFontManager::new();
    let result = mgr.register_font_from_path("Missing", "tests/assets/does_not_exist.ttf");
    assert!(matches!(result, Err(NativeError::FontRegister { .. })));
}

/// WOFF and WOFF2 byte streams are accepted by the registered Skia
/// build (`freetype` + `freetype-woff2` features). This pins WOFF/WOFF2
/// support so the public claim does not drift from what Skia decodes.
#[test]
fn font_manager_accepts_woff_and_woff2() -> Result<()> {
    let mgr = NativeFontManager::new();
    let woff = std::fs::read("tests/assets/fonts/Monoton-Regular.woff")?;
    mgr.register_font_from_data("Monoton WOFF", &woff)?;
    assert!(mgr.has_font("Monoton WOFF"));
    let woff2 = std::fs::read("tests/assets/fonts/Monoton-Regular.woff2")?;
    mgr.register_font_from_data("Monoton WOFF2", &woff2)?;
    assert!(mgr.has_font("Monoton WOFF2"));
    Ok(())
}

/// `NativeFontManager` uses `parking_lot::Mutex` for interior
/// mutability rather than `RefCell`, so registration and queries can
/// share the same `&NativeFontManager` reference. (Cross-thread
/// sharing is not promised: skia-safe's `TypefaceFontProvider` is
/// not `Send`, so `NativeFontManager` is single-threaded.) The audit
/// at the top of this file (`pub .*RefCell` against `src/native`)
/// statically enforces no `RefCell` leaks through the public API.
#[test]
fn font_manager_uses_interior_mutability_through_immutable_ref() -> Result<()> {
    let mgr = NativeFontManager::new();
    let bytes = std::fs::read(FONT_FIXTURE)?;
    let mgr_ref = &mgr;
    mgr_ref.register_font_from_data("Interior", &bytes)?;
    assert!(mgr_ref.has_font("Interior"));
    assert_eq!(mgr_ref.families(), vec!["Interior".to_string()]);
    Ok(())
}

// --- Chunk 7B: Plain paragraph layout --------------------------------------

fn render_layout(
    text: &str,
    style: &TextStyle,
    max_width: f32,
    surface_w: u32,
    surface_h: u32,
) -> Result<NativeTextLayout> {
    let engine = NativeTextEngine::with_system_fonts();
    let layout = engine.layout_text(text, style, max_width);
    let backend = NativeBackend::new();
    let mut surface = backend.create_surface(surface_w, surface_h, SurfaceOptions::default())?;
    surface.with_canvas(|canvas| {
        canvas.clear(RgbaLinear::new_premultiplied(0.0, 0.0, 0.0, 0.0));
        canvas.draw_text_layout(&layout, 0.0, 0.0);
    });
    Ok(layout)
}

/// Plain text renders visible pixels through the layout API.
#[test]
fn text_layout_draws_visible_pixels() -> Result<()> {
    let backend = NativeBackend::new();
    let mut surface = backend.create_surface(128, 32, SurfaceOptions::default())?;
    let engine = NativeTextEngine::with_system_fonts();
    let style = TextStyle {
        color: RgbaLinear::opaque(1.0, 1.0, 1.0),
        font_size: 24.0,
        ..TextStyle::default()
    };
    let layout = engine.layout_text("Studio", &style, 128.0);
    surface.with_canvas(|canvas| {
        canvas.clear(RgbaLinear::new_premultiplied(0.0, 0.0, 0.0, 0.0));
        canvas.draw_text_layout(&layout, 0.0, 0.0);
    });
    let px = surface.read_pixels()?;
    assert!(
        px.pixels().iter().any(|c| *c > 32),
        "text layout should produce visible pixels"
    );
    Ok(())
}

/// Center alignment puts pixels near the horizontal middle of the
/// max-width band; left alignment hugs the left edge. Compare the column
/// that holds the first non-empty pixel.
#[test]
fn text_layout_center_alignment_shifts_pixels_right() -> Result<()> {
    let backend = NativeBackend::new();
    let measure_first_inked_column = |align: TextAlign| -> Result<usize> {
        let mut surface = backend.create_surface(128, 24, SurfaceOptions::default())?;
        let engine = NativeTextEngine::with_system_fonts();
        let style = TextStyle {
            color: RgbaLinear::opaque(1.0, 1.0, 1.0),
            font_size: 18.0,
            align,
            ..TextStyle::default()
        };
        let layout = engine.layout_text("X", &style, 128.0);
        surface.with_canvas(|canvas| {
            canvas.clear(RgbaLinear::new_premultiplied(0.0, 0.0, 0.0, 0.0));
            canvas.draw_text_layout(&layout, 0.0, 0.0);
        });
        let px = surface.read_pixels()?;
        let stride = px.stride();
        for row in 0..24usize {
            for col in 0..128usize {
                let alpha = px.pixels()[row * stride + col * 4 + 3];
                if alpha > 32 {
                    return Ok(col);
                }
            }
        }
        Ok(usize::MAX)
    };
    let left_col = measure_first_inked_column(TextAlign::Left)?;
    let center_col = measure_first_inked_column(TextAlign::Center)?;
    let right_col = measure_first_inked_column(TextAlign::Right)?;
    assert!(
        left_col < 16,
        "left alignment hugs the left edge, got column {left_col}"
    );
    assert!(
        center_col > left_col + 16,
        "center alignment shifts text right, got left={left_col} center={center_col}"
    );
    assert!(
        right_col > center_col,
        "right alignment shifts text further right than center, got center={center_col} right={right_col}"
    );
    Ok(())
}

/// Wrapping at a narrower width increases line count and total height.
#[test]
fn text_layout_wrapping_changes_line_count_and_height() -> Result<()> {
    let engine = NativeTextEngine::with_system_fonts();
    let style = TextStyle {
        color: RgbaLinear::opaque(1.0, 1.0, 1.0),
        font_size: 20.0,
        ..TextStyle::default()
    };
    let text = "the quick brown fox jumps over the lazy dog";
    let wide = engine.layout_text(text, &style, 1024.0);
    let narrow = engine.layout_text(text, &style, 80.0);
    assert!(
        narrow.line_count() > wide.line_count(),
        "narrow width should wrap to more lines: wide={} narrow={}",
        wide.line_count(),
        narrow.line_count()
    );
    assert!(
        narrow.height() > wide.height(),
        "narrow width should be taller: wide={} narrow={}",
        wide.height(),
        narrow.height()
    );
    Ok(())
}

/// `first_line_ascent` is reported as a non-zero value for non-empty
/// text. Skia uses negative ascent values to express the distance above
/// the baseline; the magnitude grows with font size.
#[test]
fn text_layout_first_line_ascent_grows_with_font_size() -> Result<()> {
    let engine = NativeTextEngine::with_system_fonts();
    let small = engine.layout_text(
        "Mg",
        &TextStyle {
            font_size: 12.0,
            ..TextStyle::default()
        },
        128.0,
    );
    let large = engine.layout_text(
        "Mg",
        &TextStyle {
            font_size: 48.0,
            ..TextStyle::default()
        },
        128.0,
    );
    assert!(small.first_line_ascent().abs() > 0.0);
    assert!(
        large.first_line_ascent().abs() > small.first_line_ascent().abs(),
        "larger font size should produce a larger ascent magnitude"
    );
    Ok(())
}

/// `width()` returns the measured longest-line width (not the wrapping
/// budget); `max_width()` returns the budget. `height()` is finite and
/// non-negative; `line_count()` is at least 1 for non-empty input.
#[test]
fn text_layout_metrics_are_well_formed() -> Result<()> {
    let layout = render_layout(
        "Studio",
        &TextStyle {
            color: RgbaLinear::opaque(1.0, 1.0, 1.0),
            font_size: 20.0,
            ..TextStyle::default()
        },
        100.0,
        128,
        32,
    )?;
    assert_eq!(layout.max_width(), 100.0);
    assert!(layout.width().is_finite() && layout.width() >= 0.0);
    assert!(
        layout.width() <= layout.max_width(),
        "measured width {} should not exceed max_width {}",
        layout.width(),
        layout.max_width()
    );
    assert!(
        layout.width() > 0.0,
        "non-empty text should have positive measured width"
    );
    assert!(layout.height().is_finite() && layout.height() >= 0.0);
    assert!(layout.line_count() >= 1);
    Ok(())
}

/// `width()` is strictly less than `max_width()` when the laid-out text
/// is shorter than the wrapping budget. This pins the new measured
/// semantics against the pre-fix behavior where `width()` returned
/// `max_width` regardless of content.
#[test]
fn text_layout_width_reflects_measured_content_not_max_width() -> Result<()> {
    let engine = NativeTextEngine::with_system_fonts();
    let layout = engine.layout_text(
        "i", // single-character paragraph; well under any sane budget
        &TextStyle {
            font_size: 16.0,
            ..TextStyle::default()
        },
        4096.0,
    );
    assert_eq!(layout.max_width(), 4096.0);
    assert!(
        layout.width() < 200.0,
        "measured width should be small for one glyph, got {}",
        layout.width()
    );
    Ok(())
}

/// A registered font selected via `NativeTextEngine::new(&manager)` and
/// requested by family name in `TextStyle.font_families` is used for
/// layout: the rendered ink differs from a layout that requested an
/// unknown family (which falls back to a system font).
#[test]
fn text_layout_uses_registered_font_when_requested() -> Result<()> {
    let mgr = NativeFontManager::new();
    mgr.register_font_from_path("Studio Test Font", FONT_FIXTURE)?;

    let backend = NativeBackend::new();
    let render_with_engine = |engine: NativeTextEngine, family: &str| -> Result<Vec<u8>> {
        let mut surface = backend.create_surface(96, 32, SurfaceOptions::default())?;
        let style = TextStyle {
            color: RgbaLinear::opaque(1.0, 1.0, 1.0),
            font_size: 24.0,
            font_families: vec![family.to_string()],
            ..TextStyle::default()
        };
        let layout = engine.layout_text("Studio", &style, 96.0);
        surface.with_canvas(|canvas| {
            canvas.clear(RgbaLinear::new_premultiplied(0.0, 0.0, 0.0, 0.0));
            canvas.draw_text_layout(&layout, 0.0, 0.0);
        });
        Ok(surface.read_pixels()?.into_pixels())
    };

    let registered = render_with_engine(NativeTextEngine::new(&mgr), "Studio Test Font")?;
    // Same engine, but request a family that the registry does not have:
    // the engine falls back to a system font, producing different ink.
    let fallback = render_with_engine(NativeTextEngine::new(&mgr), "Definitely Not A Real Family")?;

    assert_ne!(
        registered, fallback,
        "registered Oswald glyphs should differ from system-fallback rendering"
    );
    Ok(())
}

// --- Color-space tagging regression (post-7C review) -----------------------

/// Linear `RgbaLinear` values must round-trip through the rendering
/// pipeline to the same sRGB byte that linearizes back. Pre-fix, paint's
/// `set_color4f(.., None)` gamma-decoded the linear value a second time,
/// so a linear input of ≈ 0.198 (sRGB byte 124) read back as byte ≈ 52.
/// This test pins the fix.
#[test]
fn rgba_linear_round_trips_to_srgb_uint8_byte() -> Result<()> {
    // Linearize sRGB byte 124: ((124/255 + 0.055)/1.055)^2.4 ≈ 0.1981.
    let linear_input = 0.198_069_25_f32;
    let backend = NativeBackend::new();
    let mut surface = backend.create_surface(2, 2, SurfaceOptions::default())?;
    surface.with_canvas(|canvas| {
        canvas.draw_rect(
            Rect::from_xywh(0.0, 0.0, 2.0, 2.0),
            &NativePaint::fill(RgbaLinear::opaque(linear_input, linear_input, linear_input)),
        );
    });
    let frame = surface.read_pixels()?;
    let r = frame.pixels()[0];
    let g = frame.pixels()[1];
    let b = frame.pixels()[2];
    assert!(
        r.abs_diff(124) <= 2,
        "linear 0.198 should round-trip to sRGB byte ≈ 124, got {r}"
    );
    assert!(g.abs_diff(124) <= 2, "g={g}");
    assert!(b.abs_diff(124) <= 2, "b={b}");
    Ok(())
}

/// `clear` honours the destination's working color space too. Same
/// linear input → same sRGB byte 124 readback as `draw_rect`.
#[test]
fn rgba_linear_clear_round_trips_to_srgb_uint8_byte() -> Result<()> {
    let linear_input = 0.198_069_25_f32;
    let backend = NativeBackend::new();
    let mut surface = backend.create_surface(2, 2, SurfaceOptions::default())?;
    surface.with_canvas(|canvas| {
        canvas.clear(RgbaLinear::opaque(linear_input, linear_input, linear_input));
    });
    let frame = surface.read_pixels()?;
    let r = frame.pixels()[0];
    assert!(
        r.abs_diff(124) <= 2,
        "linear 0.198 cleared on sRGB surface round-trips to ≈ 124, got {r}"
    );
    Ok(())
}

// --- Chunk 7C: rich spans + metrics ----------------------------------------

/// `layout_rich_text` honours per-span color overrides: a paragraph with
/// a red span and a green span shows both colors in the rendered output.
#[test]
fn rich_text_spans_render_distinct_colors() -> Result<()> {
    let backend = NativeBackend::new();
    let mut surface = backend.create_surface(160, 32, SurfaceOptions::default())?;
    let engine = NativeTextEngine::with_system_fonts();
    let base = TextStyle {
        font_size: 24.0,
        ..TextStyle::default()
    };
    let spans = vec![
        RichTextSpan {
            text: "RED ".to_string(),
            style: TextStyle {
                color: RgbaLinear::opaque(1.0, 0.0, 0.0),
                font_size: 24.0,
                ..TextStyle::default()
            },
        },
        RichTextSpan {
            text: "GREEN".to_string(),
            style: TextStyle {
                color: RgbaLinear::opaque(0.0, 1.0, 0.0),
                font_size: 24.0,
                ..TextStyle::default()
            },
        },
    ];
    let layout = engine.layout_rich_text(&spans, &base, 160.0);
    surface.with_canvas(|canvas| {
        canvas.clear(RgbaLinear::new_premultiplied(0.0, 0.0, 0.0, 0.0));
        canvas.draw_text_layout(&layout, 0.0, 0.0);
    });
    let px = surface.read_pixels()?;
    let stride = px.stride();
    let mut saw_red = false;
    let mut saw_green = false;
    for chunk in px.pixels()[..stride * 32].chunks_exact(4) {
        if chunk[0] > 150 && chunk[1] < 80 {
            saw_red = true;
        }
        if chunk[1] > 150 && chunk[0] < 80 {
            saw_green = true;
        }
    }
    assert!(saw_red, "expected red ink from the first span");
    assert!(saw_green, "expected green ink from the second span");
    Ok(())
}

/// `letter_spacing > 0` widens the rendered paragraph compared to the
/// default zero spacing.
#[test]
fn text_letter_spacing_widens_layout() -> Result<()> {
    let engine = NativeTextEngine::with_system_fonts();
    let base = TextStyle {
        font_size: 24.0,
        ..TextStyle::default()
    };
    let spaced = TextStyle {
        font_size: 24.0,
        letter_spacing: 8.0,
        ..TextStyle::default()
    };
    let plain_layout = engine.layout_text("Studio", &base, 4096.0);
    let spaced_layout = engine.layout_text("Studio", &spaced, 4096.0);
    assert!(
        spaced_layout.width() > plain_layout.width() + 16.0,
        "letter spacing should noticeably widen the layout: plain={} spaced={}",
        plain_layout.width(),
        spaced_layout.width()
    );
    Ok(())
}

/// `word_spacing > 0` widens multi-word text more than single-word text
/// and noticeably more than the default.
#[test]
fn text_word_spacing_widens_multi_word_layout() -> Result<()> {
    let engine = NativeTextEngine::with_system_fonts();
    let base = TextStyle {
        font_size: 24.0,
        ..TextStyle::default()
    };
    let spaced = TextStyle {
        font_size: 24.0,
        word_spacing: 24.0,
        ..TextStyle::default()
    };
    let plain_layout = engine.layout_text("hello world", &base, 4096.0);
    let spaced_layout = engine.layout_text("hello world", &spaced, 4096.0);
    assert!(
        spaced_layout.width() > plain_layout.width() + 12.0,
        "word spacing should widen multi-word layout: plain={} spaced={}",
        plain_layout.width(),
        spaced_layout.width()
    );
    Ok(())
}

/// Underline decoration paints below the baseline. With underline on,
/// the paragraph occupies more vertical pixels (alpha extends below
/// the glyph's natural descent).
#[test]
fn text_underline_decoration_renders_below_baseline() -> Result<()> {
    let backend = NativeBackend::new();
    let render = |decoration: TextDecoration| -> Result<Vec<u8>> {
        let mut surface = backend.create_surface(96, 64, SurfaceOptions::default())?;
        let engine = NativeTextEngine::with_system_fonts();
        let layout = engine.layout_text(
            "abc",
            &TextStyle {
                color: RgbaLinear::opaque(1.0, 1.0, 1.0),
                font_size: 24.0,
                decoration,
                decoration_color: Some(RgbaLinear::opaque(1.0, 1.0, 1.0)),
                ..TextStyle::default()
            },
            96.0,
        );
        surface.with_canvas(|canvas| {
            canvas.clear(RgbaLinear::new_premultiplied(0.0, 0.0, 0.0, 0.0));
            canvas.draw_text_layout(&layout, 0.0, 0.0);
        });
        Ok(surface.read_pixels()?.into_pixels())
    };
    let plain = render(TextDecoration::default())?;
    let underlined = render(TextDecoration::underline())?;
    assert_ne!(
        plain, underlined,
        "underline decoration must change rendered ink"
    );
    Ok(())
}

/// Drop shadow: rendering a single span with a shadow produces ink
/// outside the un-shadowed glyph region.
#[test]
fn text_shadow_renders_offset_ink() -> Result<()> {
    let backend = NativeBackend::new();
    let render = |shadows: Vec<TextShadow>| -> Result<Vec<u8>> {
        let mut surface = backend.create_surface(96, 48, SurfaceOptions::default())?;
        let engine = NativeTextEngine::with_system_fonts();
        let layout = engine.layout_text(
            "S",
            &TextStyle {
                color: RgbaLinear::opaque(1.0, 1.0, 1.0),
                font_size: 32.0,
                shadows,
                ..TextStyle::default()
            },
            96.0,
        );
        surface.with_canvas(|canvas| {
            canvas.clear(RgbaLinear::new_premultiplied(0.0, 0.0, 0.0, 0.0));
            canvas.draw_text_layout(&layout, 0.0, 0.0);
        });
        Ok(surface.read_pixels()?.into_pixels())
    };
    let plain = render(Vec::new())?;
    let with_shadow = render(vec![TextShadow {
        color: RgbaLinear::opaque(1.0, 0.0, 0.0),
        offset_x: 4.0,
        offset_y: 4.0,
        blur_sigma: 1.0,
    }])?;
    assert_ne!(plain, with_shadow, "shadow must change rendered ink");
    // Count non-zero alpha pixels: shadow should add coverage beyond glyphs.
    let nonzero = |bytes: &[u8]| bytes.chunks_exact(4).filter(|p| p[3] > 0).count();
    assert!(
        nonzero(&with_shadow) > nonzero(&plain),
        "shadow should add covered pixels beyond plain glyph coverage"
    );
    Ok(())
}

/// `baseline_shift` moves a span vertically without affecting the
/// paragraph's overall height. A negative shift (superscript) on a span
/// produces ink above the baseline of the surrounding text.
#[test]
fn text_baseline_shift_moves_span_vertically() -> Result<()> {
    let backend = NativeBackend::new();
    let render = |shift: f32| -> Result<Vec<u8>> {
        let mut surface = backend.create_surface(96, 48, SurfaceOptions::default())?;
        let engine = NativeTextEngine::with_system_fonts();
        let base = TextStyle {
            color: RgbaLinear::opaque(1.0, 1.0, 1.0),
            font_size: 24.0,
            ..TextStyle::default()
        };
        let spans = vec![
            RichTextSpan {
                text: "x".to_string(),
                style: base.clone(),
            },
            RichTextSpan {
                text: "2".to_string(),
                style: TextStyle {
                    baseline_shift: shift,
                    font_size: 16.0,
                    ..base.clone()
                },
            },
        ];
        let layout = engine.layout_rich_text(&spans, &base, 96.0);
        surface.with_canvas(|canvas| {
            canvas.clear(RgbaLinear::new_premultiplied(0.0, 0.0, 0.0, 0.0));
            canvas.draw_text_layout(&layout, 0.0, 24.0);
        });
        Ok(surface.read_pixels()?.into_pixels())
    };
    let inline = render(0.0)?;
    let superscript = render(-12.0)?;
    assert_ne!(
        inline, superscript,
        "baseline shift must change rendered ink positions"
    );
    Ok(())
}

/// `get_rects_for_range` returns at least one bounding box for a
/// non-empty character range. The returned rect's height matches the
/// glyph's metric range and its left edge falls inside the paragraph.
#[test]
fn text_get_rects_for_range_returns_glyph_bounds() -> Result<()> {
    let engine = NativeTextEngine::with_system_fonts();
    let layout = engine.layout_text(
        "Studio",
        &TextStyle {
            font_size: 24.0,
            ..TextStyle::default()
        },
        256.0,
    );
    let rects = layout.get_rects_for_range(0..6);
    assert!(
        !rects.is_empty(),
        "non-empty range should produce at least one rect"
    );
    let r = rects[0];
    assert!(r.width() > 0.0 && r.height() > 0.0);
    assert!(r.left >= 0.0);
    Ok(())
}

/// `get_rects_for_range` over a single mid-paragraph character returns
/// rects that fit inside the full-paragraph rect.
#[test]
fn text_get_rects_for_range_single_char_inside_full() -> Result<()> {
    let engine = NativeTextEngine::with_system_fonts();
    let layout = engine.layout_text(
        "Studio",
        &TextStyle {
            font_size: 24.0,
            ..TextStyle::default()
        },
        256.0,
    );
    let full = layout.get_rects_for_range(0..6);
    let single = layout.get_rects_for_range(2..3);
    assert!(!full.is_empty());
    assert!(!single.is_empty());
    let f = full[0];
    let s = single[0];
    assert!(s.left >= f.left && s.right <= f.right);
    assert!(s.top >= f.top && s.bottom <= f.bottom);
    Ok(())
}

/// `line_metrics` returns one entry per laid-out line. Wrapping at
/// narrower widths yields more entries.
#[test]
fn text_line_metrics_match_line_count() -> Result<()> {
    let engine = NativeTextEngine::with_system_fonts();
    let style = TextStyle {
        font_size: 16.0,
        ..TextStyle::default()
    };
    let text = "the quick brown fox jumps over the lazy dog";
    let wide = engine.layout_text(text, &style, 4096.0);
    let narrow = engine.layout_text(text, &style, 80.0);
    assert_eq!(wide.line_metrics().len(), wide.line_count());
    assert_eq!(narrow.line_metrics().len(), narrow.line_count());
    assert!(narrow.line_metrics().len() > wide.line_metrics().len());
    let m0 = wide.line_metrics()[0];
    assert_eq!(m0.line_number, 0);
    assert!(m0.height > 0.0);
    assert!(m0.ascent > 0.0);
    Ok(())
}

/// Variable font weight values can pass through as non-100-step
/// integers (e.g., 350). The layout call must not panic and produce
/// finite metrics. When the active typeface supports it, the rendered
/// width can differ from a coarser weight.
#[test]
fn text_variable_font_weight_passes_through() -> Result<()> {
    let engine = NativeTextEngine::with_system_fonts();
    let layout = engine.layout_text(
        "Studio",
        &TextStyle {
            font_size: 24.0,
            font_weight: 350,
            ..TextStyle::default()
        },
        256.0,
    );
    assert!(layout.width().is_finite());
    assert!(layout.height() > 0.0);
    Ok(())
}
