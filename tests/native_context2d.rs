//! Behaviour of the Canvas-shaped Rust facade.
//!
//! These assert what came out, not what was set. A facade that forwards state
//! into a lower layer can compile perfectly and still be a no-op -- if the
//! draw path rebuilds the value from somewhere else, the setter writes to a
//! field nobody reads. Sampling the rendered pixels is what catches that;
//! asserting `state.foo == bar` would not.

use meo_skia_canvas::prelude::*;

/// Renders and returns unencoded RGBA, so a test can sample a pixel.
fn pixels(canvas: &mut Canvas) -> Vec<u8> {
    canvas
        .to_buffer(ImageFormat::Raw, &EncodeOptions::default())
        .expect("raw export")
}

/// The RGBA at (`x`, `y`) of a `width`-wide raw buffer.
fn at(buffer: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * width + x) * 4) as usize;
    [buffer[i], buffer[i + 1], buffer[i + 2], buffer[i + 3]]
}

fn red() -> RgbaLinear {
    RgbaLinear::opaque(1.0, 0.0, 0.0)
}

#[test]
fn fill_rect_paints_the_fill_style() {
    let mut canvas = Canvas::new(10.0, 10.0);
    canvas.context().set_fill_style(red());
    canvas.context().fill_rect(0.0, 0.0, 10.0, 10.0);

    let px = at(&pixels(&mut canvas), 10, 5, 5);
    assert_eq!(px[0], 255, "red channel");
    assert_eq!(px[3], 255, "opaque");
}

#[test]
fn fill_rect_covers_only_its_own_rectangle() {
    let mut canvas = Canvas::new(10.0, 10.0);
    canvas.context().set_fill_style(red());
    canvas.context().fill_rect(0.0, 0.0, 4.0, 4.0);

    let buffer = pixels(&mut canvas);
    assert_eq!(at(&buffer, 10, 1, 1)[0], 255, "inside is painted");
    assert_eq!(at(&buffer, 10, 8, 8)[3], 0, "outside stays transparent");
}

#[test]
fn clear_rect_erases_back_to_transparent() {
    let mut canvas = Canvas::new(10.0, 10.0);
    {
        let ctx = canvas.context();
        ctx.set_fill_style(red());
        ctx.fill_rect(0.0, 0.0, 10.0, 10.0);
        ctx.clear_rect(0.0, 0.0, 5.0, 5.0);
    }

    let buffer = pixels(&mut canvas);
    assert_eq!(at(&buffer, 10, 2, 2)[3], 0, "cleared region");
    assert_eq!(at(&buffer, 10, 8, 8)[3], 255, "untouched region");
}

#[test]
fn translate_moves_subsequent_drawing() {
    let mut canvas = Canvas::new(10.0, 10.0);
    {
        let ctx = canvas.context();
        ctx.translate(5.0, 5.0);
        ctx.set_fill_style(red());
        ctx.fill_rect(0.0, 0.0, 5.0, 5.0);
    }

    let buffer = pixels(&mut canvas);
    assert_eq!(at(&buffer, 10, 1, 1)[3], 0, "origin is now empty");
    assert_eq!(at(&buffer, 10, 7, 7)[0], 255, "drawing landed offset");
}

#[test]
fn restore_undoes_a_transform() {
    let mut canvas = Canvas::new(10.0, 10.0);
    {
        let ctx = canvas.context();
        ctx.save();
        ctx.translate(5.0, 5.0);
        ctx.restore();
        ctx.set_fill_style(red());
        ctx.fill_rect(0.0, 0.0, 4.0, 4.0);
    }

    // Had `restore` not popped the translate, this pixel would be empty.
    assert_eq!(at(&pixels(&mut canvas), 10, 1, 1)[0], 255);
}

#[test]
fn get_transform_reports_what_was_set() {
    let mut canvas = Canvas::new(10.0, 10.0);
    let ctx = canvas.context();

    ctx.translate(3.0, 4.0);
    let t = ctx.get_transform();
    assert_eq!((t.tx, t.ty), (3.0, 4.0));

    ctx.reset_transform();
    let t = ctx.get_transform();
    assert_eq!((t.a, t.d, t.tx, t.ty), (1.0, 1.0, 0.0, 0.0));
}

#[test]
fn global_alpha_modulates_what_is_drawn() {
    let mut canvas = Canvas::new(10.0, 10.0);
    {
        let ctx = canvas.context();
        ctx.set_global_alpha(0.5);
        ctx.set_fill_style(red());
        ctx.fill_rect(0.0, 0.0, 10.0, 10.0);
    }

    let alpha = at(&pixels(&mut canvas), 10, 5, 5)[3];
    assert!(
        (100..=155).contains(&alpha),
        "expected roughly half alpha, got {alpha}"
    );
}

#[test]
fn stroke_style_and_line_width_reach_the_stroke() {
    // A hairline would leave the sampled row empty; a wide stroke will not.
    let mut canvas = Canvas::new(20.0, 20.0);
    {
        let ctx = canvas.context();
        ctx.set_stroke_style(red());
        ctx.set_line_width(8.0);
        ctx.begin_path();
        ctx.move_to(0.0, 10.0);
        ctx.line_to(20.0, 10.0);
        ctx.stroke();
    }

    let buffer = pixels(&mut canvas);
    assert_eq!(at(&buffer, 20, 10, 10)[0], 255, "on the line");
    assert!(at(&buffer, 20, 10, 7)[3] > 0, "within the 8px width");
}

#[test]
fn line_dash_leaves_gaps() {
    let mut canvas = Canvas::new(40.0, 10.0);
    {
        let ctx = canvas.context();
        ctx.set_stroke_style(red());
        ctx.set_line_width(6.0);
        ctx.set_line_dash(&[4.0, 4.0]);
        ctx.begin_path();
        ctx.move_to(0.0, 5.0);
        ctx.line_to(40.0, 5.0);
        ctx.stroke();
    }

    let buffer = pixels(&mut canvas);
    let painted = (0..40).filter(|x| at(&buffer, 40, *x, 5)[3] > 0).count();
    assert!(
        (10..=30).contains(&painted),
        "a dashed line should cover part of the row, covered {painted}/40"
    );
}

#[test]
fn get_line_dash_returns_the_pattern() {
    let mut canvas = Canvas::new(10.0, 10.0);
    let ctx = canvas.context();
    ctx.set_line_dash(&[3.0, 2.0]);
    assert_eq!(ctx.get_line_dash(), vec![3.0, 2.0]);
}

#[test]
fn fill_paints_a_constructed_path() {
    let mut canvas = Canvas::new(20.0, 20.0);
    {
        let ctx = canvas.context();
        ctx.set_fill_style(red());
        ctx.begin_path();
        ctx.move_to(0.0, 0.0);
        ctx.line_to(20.0, 0.0);
        ctx.line_to(20.0, 20.0);
        ctx.close_path();
        ctx.fill(FillRule::NonZero);
    }

    let buffer = pixels(&mut canvas);
    assert_eq!(at(&buffer, 20, 17, 10)[0], 255, "inside the triangle");
    assert_eq!(at(&buffer, 20, 2, 15)[3], 0, "outside the triangle");
}

#[test]
fn arc_fills_as_one_contour_with_the_line_before_it() {
    // The regression this guards: `add_path` appending instead of extending
    // makes the arc a separate region, so the wedge fills wrong.
    let mut canvas = Canvas::new(40.0, 40.0);
    {
        let ctx = canvas.context();
        ctx.set_fill_style(red());
        ctx.begin_path();
        ctx.move_to(20.0, 20.0);
        ctx.line_to(38.0, 20.0);
        ctx.arc(20.0, 20.0, 18.0, 0.0, std::f32::consts::FRAC_PI_2, false);
        ctx.close_path();
        ctx.fill(FillRule::NonZero);
    }

    let buffer = pixels(&mut canvas);
    assert_eq!(at(&buffer, 40, 26, 26)[0], 255, "inside the quarter wedge");
    assert_eq!(at(&buffer, 40, 6, 6)[3], 0, "opposite quadrant is empty");
}

#[test]
fn round_rect_rounds_its_corners() {
    let mut canvas = Canvas::new(20.0, 20.0);
    {
        let ctx = canvas.context();
        ctx.set_fill_style(red());
        ctx.begin_path();
        ctx.round_rect(0.0, 0.0, 20.0, 20.0, [8.0; 4])
            .expect("valid radii");
        ctx.fill(FillRule::NonZero);
    }

    let buffer = pixels(&mut canvas);
    assert_eq!(at(&buffer, 20, 10, 10)[0], 255, "centre is filled");
    assert_eq!(at(&buffer, 20, 0, 0)[3], 0, "corner is rounded away");
}

#[test]
fn is_point_in_path_follows_the_geometry() {
    let mut canvas = Canvas::new(20.0, 20.0);
    let ctx = canvas.context();
    ctx.begin_path();
    ctx.rect(4.0, 4.0, 8.0, 8.0);

    assert!(ctx.is_point_in_path(8.0, 8.0, FillRule::NonZero), "inside");
    assert!(
        !ctx.is_point_in_path(1.0, 1.0, FillRule::NonZero),
        "outside"
    );
}

#[test]
fn text_draws_something() {
    let mut canvas = Canvas::new(120.0, 40.0);
    {
        let ctx = canvas.context();
        ctx.set_fill_style(red());
        ctx.set_font(&Font::new("Helvetica", 28.0).weight(700));
        ctx.fill_text("Hi", 8.0, 30.0, None);
    }

    let buffer = pixels(&mut canvas);
    let painted = buffer.chunks_exact(4).filter(|px| px[3] > 0).count();
    assert!(
        painted > 20,
        "glyphs should cover pixels, covered {painted}"
    );
}

#[test]
fn each_format_encodes_to_its_own_container() {
    let mut canvas = Canvas::new(20.0, 20.0);
    canvas.context().set_fill_style(red());
    canvas.context().fill_rect(0.0, 0.0, 20.0, 20.0);

    let options = EncodeOptions::default();
    let png = canvas.to_buffer(ImageFormat::Png, &options).unwrap();
    let webp = canvas.to_buffer(ImageFormat::Webp, &options).unwrap();
    let svg = canvas.to_buffer(ImageFormat::Svg, &options).unwrap();
    let pdf = canvas.to_buffer(ImageFormat::Pdf, &options).unwrap();

    assert_eq!(&png[..4], b"\x89PNG");
    assert_eq!(&webp[..4], b"RIFF");
    assert_eq!(&svg[..5], b"<?xml");
    assert_eq!(&pdf[..5], b"%PDF-");
}

#[test]
fn density_scales_the_rasterized_output() {
    let mut canvas = Canvas::new(20.0, 20.0);
    canvas.context().set_fill_style(red());
    canvas.context().fill_rect(0.0, 0.0, 20.0, 20.0);

    let one = canvas
        .to_buffer(ImageFormat::Raw, &EncodeOptions::default())
        .unwrap();
    let two = canvas
        .to_buffer(
            ImageFormat::Raw,
            &EncodeOptions {
                density: 2.0,
                ..EncodeOptions::default()
            },
        )
        .unwrap();

    assert_eq!(
        two.len(),
        one.len() * 4,
        "twice the density is 4x the pixels"
    );
}

#[test]
fn a_multi_page_pdf_carries_every_page() {
    let mut canvas = Canvas::new(20.0, 20.0);
    canvas.context().set_fill_style(red());
    canvas.context().fill_rect(0.0, 0.0, 20.0, 20.0);
    canvas.new_page();
    canvas
        .context()
        .set_fill_style(RgbaLinear::opaque(0.0, 0.0, 1.0));
    canvas.context().fill_rect(0.0, 0.0, 20.0, 20.0);

    assert_eq!(canvas.page_count(), 2);

    let pdf = canvas
        .to_buffer(ImageFormat::Pdf, &EncodeOptions::default())
        .unwrap();
    let text = String::from_utf8_lossy(&pdf);
    let pages = text.matches("/Type /Page\n").count();
    assert!(pages >= 2, "expected two page objects, found {pages}");
}

#[test]
fn to_file_refuses_an_unknown_extension() {
    let mut canvas = Canvas::new(10.0, 10.0);
    let result = canvas.to_file("/tmp/x.wepb", &EncodeOptions::default());

    // Silently defaulting to PNG behind a typo'd name is the failure mode
    // this guards against.
    assert!(result.is_err(), "a typo must not silently produce a file");
}

#[test]
fn text_align_moves_the_run_against_its_x() {
    let render = |align: TextAlign| {
        let mut canvas = Canvas::new(120.0, 40.0);
        {
            let ctx = canvas.context();
            ctx.set_fill_style(red());
            ctx.set_font(&Font::new("Helvetica", 20.0));
            ctx.set_text_align(align);
            ctx.fill_text("Hi", 60.0, 28.0, None);
        }
        let buffer = pixels(&mut canvas);
        // Centre of mass along x, so the assertion does not depend on glyphs.
        let (mut sum, mut n) = (0u64, 0u64);
        for x in 0..120 {
            for y in 0..40 {
                if at(&buffer, 120, x, y)[3] > 0 {
                    sum += u64::from(x);
                    n += 1;
                }
            }
        }
        assert!(n > 0, "expected glyphs");
        sum as f32 / n as f32
    };

    let left = render(TextAlign::Left);
    let right = render(TextAlign::Right);
    assert!(
        left > right,
        "left-aligned sits right of right-aligned ({left} vs {right})"
    );
}

#[test]
fn text_baseline_moves_the_run_vertically() {
    let render = |baseline: TextBaseline| {
        let mut canvas = Canvas::new(80.0, 60.0);
        {
            let ctx = canvas.context();
            ctx.set_fill_style(red());
            ctx.set_font(&Font::new("Helvetica", 20.0));
            ctx.set_text_baseline(baseline);
            ctx.fill_text("Hi", 8.0, 30.0, None);
        }
        let buffer = pixels(&mut canvas);
        (0..60).find(|y| (0..80).any(|x| at(&buffer, 80, x, *y)[3] > 0))
    };

    let top = render(TextBaseline::Top).expect("top drew");
    let alphabetic = render(TextBaseline::Alphabetic).expect("alphabetic drew");
    assert!(
        top > alphabetic,
        "Top pushes the run down ({top} vs {alphabetic})"
    );
}

#[test]
fn reset_clears_the_page_and_the_state() {
    let mut canvas = Canvas::new(10.0, 10.0);
    {
        let ctx = canvas.context();
        ctx.translate(5.0, 5.0);
        ctx.set_fill_style(red());
        ctx.fill_rect(0.0, 0.0, 5.0, 5.0);
        ctx.reset();
        // The transform is gone, so this lands at the true origin.
        ctx.set_fill_style(red());
        ctx.fill_rect(0.0, 0.0, 4.0, 4.0);
    }

    let buffer = pixels(&mut canvas);
    assert_eq!(at(&buffer, 10, 1, 1)[0], 255, "drew at the reset origin");
    assert_eq!(at(&buffer, 10, 8, 8)[3], 0, "the pre-reset drawing is gone");
}

#[test]
fn font_parse_reads_the_shorthand_and_rejects_junk() {
    let font = Font::parse("italic 700 44px Helvetica, Arial").expect("parses");
    assert_eq!(font.size, 44.0);
    assert_eq!(font.weight, 700);
    assert!(font.italic);
    assert_eq!(font.families, vec!["Helvetica", "Arial"]);

    assert!(Font::parse("Helvetica").is_err(), "no size");
    assert!(Font::parse("20px").is_err(), "no family");
    assert!(Font::parse("wobbly 20px Helvetica").is_err(), "bad token");
}

#[test]
fn letter_spacing_widens_a_run() {
    let width_of = |spacing: f32| {
        let mut canvas = Canvas::new(300.0, 40.0);
        {
            let ctx = canvas.context();
            ctx.set_fill_style(red());
            ctx.set_font(&Font::new("Helvetica", 20.0));
            ctx.set_letter_spacing(spacing);
            ctx.fill_text("wide", 4.0, 28.0, None);
        }
        let buffer = pixels(&mut canvas);
        (0..300)
            .rfind(|x| (0..40).any(|y| at(&buffer, 300, *x, y)[3] > 0))
            .expect("something drew")
    };

    let tight = width_of(0.0);
    let loose = width_of(8.0);
    assert!(
        loose > tight,
        "spacing should widen the run ({loose} vs {tight})"
    );
}

#[test]
fn smoothing_quality_changes_how_an_image_is_resampled() {
    // Quality only has an effect on a *resampled* image. The earlier version
    // of this test drew no image at all, so the resampler never ran and
    // every setting trivially produced the same flat fill.
    let render = |quality: SmoothingQuality| {
        let mut canvas = Canvas::new(40.0, 40.0);
        {
            let ctx = canvas.context();
            ctx.set_image_smoothing_enabled(true);
            ctx.set_image_smoothing_quality(quality);
            // A 2x2 tile blown up 20x: the interpolation between texels is
            // what the quality setting governs.
            ctx.draw_image_sized(&quad_tile(), 0.0, 0.0, 40.0, 40.0);
        }
        pixels(&mut canvas)
    };

    let low = render(SmoothingQuality::Low);
    let high = render(SmoothingQuality::High);

    assert!(
        low.chunks(4).any(|texel| texel[3] > 0),
        "the image was drawn at all"
    );
    assert_ne!(low, high, "the quality setting reaches the resampler");
}

#[test]
fn disabling_smoothing_gives_hard_texel_edges() {
    let render = |smoothing: bool| {
        let mut canvas = Canvas::new(40.0, 40.0);
        {
            let ctx = canvas.context();
            ctx.set_image_smoothing_enabled(smoothing);
            ctx.draw_image_sized(&quad_tile(), 0.0, 0.0, 40.0, 40.0);
        }
        pixels(&mut canvas)
    };

    // Nearest-neighbour leaves each texel a flat block, so the midpoint
    // between two texels is one of them rather than a blend.
    let sharp = at(&render(false), 40, 19, 5);
    let blended = at(&render(true), 40, 19, 5);

    assert_eq!(sharp[0], 255, "unsmoothed keeps the left texel pure red");
    assert_ne!(sharp, blended, "smoothing blends across the texel boundary");
}

#[test]
fn text_direction_places_the_run_relative_to_the_origin() {
    // "rtl" is Latin, so it lays out left-to-right whatever the direction
    // says -- the earlier version of this test used it and could not have
    // observed anything. Arabic script actually reorders.
    let ink_bounds = |direction: TextDirection| {
        let mut canvas = Canvas::new(120.0, 40.0);
        {
            let ctx = canvas.context();
            ctx.set_direction(direction);
            ctx.set_fill_style(red());
            ctx.set_font(&Font::new("Helvetica", 20.0));
            // `Start`, not `Right`: Left and Right are absolute, so
            // direction cannot move them. Start/End are the only
            // alignments that follow it -- confirmed against the binding,
            // where ltr and rtl agree under `textAlign = "right"` and
            // differ under `"start"`.
            ctx.set_text_align(TextAlign::Start);
            ctx.fill_text("مرحبا abc", 60.0, 28.0, None);
        }
        let buffer = pixels(&mut canvas);
        let painted = |x: u32| (0..40).any(|y| at(&buffer, 120, x, y)[3] > 0);
        let left = (0..120).find(|&x| painted(x));
        let right = (0..120).rev().find(|&x| painted(x));
        (left, right)
    };

    let ltr = ink_bounds(TextDirection::LeftToRight);
    let rtl = ink_bounds(TextDirection::RightToLeft);

    assert!(ltr.0.is_some() && rtl.0.is_some(), "both directions render");
    assert_ne!(ltr, rtl, "direction changes where the run sits");
}

#[test]
fn font_hinting_and_dither_are_accepted() {
    // Neither has a reliably observable effect at this size on every
    // platform, so this asserts only that they are wired and harmless --
    // and says so, rather than implying coverage it does not have.
    let mut canvas = Canvas::new(60.0, 30.0);
    {
        let ctx = canvas.context();
        ctx.set_dither(true);
        ctx.set_font_hinting(true);
        ctx.set_fill_style(red());
        ctx.set_font(&Font::new("Helvetica", 16.0));
        ctx.fill_text("Hinted", 4.0, 20.0, None);
    }

    let painted = pixels(&mut canvas)
        .chunks_exact(4)
        .filter(|px| px[3] > 0)
        .count();
    assert!(painted > 0, "text still renders with these set");
}

#[test]
fn a_gradient_fill_varies_across_the_shape() {
    let shader = Shader::linear_gradient(
        Point::new(0.0, 0.0),
        Point::new(40.0, 0.0),
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
        GradientInterpolation::default(),
    )
    .expect("gradient builds");

    let mut canvas = Canvas::new(40.0, 10.0);
    {
        let ctx = canvas.context();
        ctx.set_fill_shader(&shader);
        ctx.fill_rect(0.0, 0.0, 40.0, 10.0);
    }

    let buffer = pixels(&mut canvas);
    let left = at(&buffer, 40, 2, 5);
    let right = at(&buffer, 40, 37, 5);
    assert!(left[0] > right[0], "red falls off to the right");
    assert!(right[2] > left[2], "blue rises to the right");
}

#[test]
fn a_mask_filter_spreads_coverage_beyond_the_shape() {
    let render = |blurred: bool| {
        let mut canvas = Canvas::new(40.0, 40.0);
        {
            let ctx = canvas.context();
            if blurred {
                let filter = MaskFilter::blur(BlurStyle::Normal, 4.0, true)
                    .expect("mask filter builds");
                ctx.set_mask_filter(Some(&filter));
            }
            ctx.set_fill_style(red());
            ctx.fill_rect(14.0, 14.0, 12.0, 12.0);
        }
        pixels(&mut canvas)
            .chunks_exact(4)
            .filter(|px| px[3] > 0)
            .count()
    };

    let sharp = render(false);
    let blurred = render(true);
    assert!(
        blurred > sharp,
        "a blur should touch more pixels ({blurred} vs {sharp})"
    );
}

#[test]
fn draw_image_places_and_scales_the_source() {
    // Build a 2x2 source through a canvas, then draw it enlarged.
    let mut source = Canvas::new(2.0, 2.0);
    source.context().set_fill_style(red());
    source.context().fill_rect(0.0, 0.0, 2.0, 2.0);
    let png = source
        .to_buffer(ImageFormat::Png, &EncodeOptions::default())
        .unwrap();
    let image = Image::from_encoded(&png).expect("decodes");

    let mut canvas = Canvas::new(20.0, 20.0);
    canvas
        .context()
        .draw_image_sized(&image, 4.0, 4.0, 12.0, 12.0);

    let buffer = pixels(&mut canvas);
    assert_eq!(at(&buffer, 20, 10, 10)[0], 255, "inside the drawn image");
    assert_eq!(at(&buffer, 20, 1, 1)[3], 0, "outside it");
}

#[test]
fn measure_text_reports_a_plausible_run() {
    let mut canvas = Canvas::new(200.0, 60.0);
    let ctx = canvas.context();
    ctx.set_font(&Font::new("Helvetica", 24.0));

    let short = ctx.measure_text("i", None);
    let long = ctx.measure_text("wwwwwwww", None);

    assert!(short.width > 0.0, "a glyph has width");
    assert!(long.width > short.width, "more glyphs are wider");
    assert!(
        long.actual_bounding_box_ascent > 0.0,
        "ascent above baseline"
    );
    assert!(long.height > 0.0, "a line has height");
    assert_eq!(long.lines, 1, "unwrapped text is one line");
}

#[test]
fn measure_text_follows_the_current_font_size() {
    let mut canvas = Canvas::new(200.0, 60.0);
    let ctx = canvas.context();

    ctx.set_font(&Font::new("Helvetica", 12.0));
    let small = ctx.measure_text("measure", None);
    ctx.set_font(&Font::new("Helvetica", 36.0));
    let large = ctx.measure_text("measure", None);

    assert!(
        large.width > small.width * 2.0,
        "tripling the size should more than double the width ({} vs {})",
        large.width,
        small.width
    );
}

#[test]
fn actual_bounds_track_the_glyphs_while_font_bounds_do_not() {
    let mut canvas = Canvas::new(200.0, 60.0);
    let ctx = canvas.context();
    ctx.set_font(&Font::new("Helvetica", 40.0));

    let x = ctx.measure_text("x", None);
    let h = ctx.measure_text("H", None);
    let g = ctx.measure_text("g", None);

    // Ink: "H" reaches higher than "x", and "g" drops below the baseline.
    assert!(
        h.actual_bounding_box_ascent > x.actual_bounding_box_ascent,
        "H should ink higher than x ({} vs {})",
        h.actual_bounding_box_ascent,
        x.actual_bounding_box_ascent
    );
    assert!(
        g.actual_bounding_box_descent > x.actual_bounding_box_descent,
        "g should ink below the baseline ({} vs {})",
        g.actual_bounding_box_descent,
        x.actual_bounding_box_descent
    );

    // Font: identical for every string, because it describes the face.
    assert_eq!(
        x.font_bounding_box_ascent, h.font_bounding_box_ascent,
        "font ascent must not depend on the string"
    );
    assert_eq!(
        x.font_bounding_box_descent, g.font_bounding_box_descent,
        "font descent must not depend on the string"
    );
    assert!(
        x.font_bounding_box_ascent > x.actual_bounding_box_ascent,
        "the face reaches above what an x inks"
    );
}

#[test]
fn bounding_box_left_and_right_bracket_the_ink() {
    let mut canvas = Canvas::new(200.0, 60.0);
    let ctx = canvas.context();
    ctx.set_font(&Font::new("Helvetica", 40.0));

    let m = ctx.measure_text("ll", None);
    let span = m.actual_bounding_box_left + m.actual_bounding_box_right;

    assert!(span > 0.0, "the ink spans a positive width");
    assert!(
        span <= m.width + 1.0,
        "ink ({span}) should not exceed the advance ({}) by much",
        m.width
    );
}

#[test]
fn conic_curve_to_bends_and_degenerates_to_a_line() {
    let paint_count = |weight: f32| {
        let mut canvas = Canvas::new(40.0, 40.0);
        {
            let ctx = canvas.context();
            ctx.set_stroke_style(red());
            ctx.set_line_width(2.0);
            ctx.begin_path();
            ctx.move_to(4.0, 36.0);
            ctx.conic_curve_to(4.0, 4.0, 36.0, 4.0, weight);
            ctx.stroke();
        }
        pixels(&mut canvas)
            .chunks_exact(4)
            .filter(|px| px[3] > 0)
            .count()
    };

    let curved = paint_count(1.0);
    // A non-positive weight must draw the straight chord, not a rational
    // curve whose denominator crosses zero.
    let straight = paint_count(0.0);

    assert!(curved > 0 && straight > 0, "both draw something");
    assert!(
        curved != straight,
        "a weighted conic should not match the degenerate line"
    );
}

#[test]
fn outline_text_returns_a_path_with_extent() {
    let mut canvas = Canvas::new(200.0, 60.0);
    let ctx = canvas.context();
    ctx.set_font(&Font::new("Helvetica", 40.0));

    let path = ctx.outline_text("Ag", None);
    let bounds = path.bounds();

    assert!(bounds.width() > 0.0, "outlined glyphs have width");
    assert!(bounds.height() > 0.0, "outlined glyphs have height");
}

#[test]
fn save_layer_applies_one_alpha_to_the_whole_group() {
    // The distinction this proves: per-draw alpha accumulates where shapes
    // overlap, group alpha does not. The earlier version of this test set
    // globalAlpha *before* opening the layer, so both arms measured per-draw
    // alpha and it passed against a `save_layer` that did nothing at all.
    let sample = |layered: bool| {
        let mut canvas = Canvas::new(30.0, 30.0);
        {
            let ctx = canvas.context();
            if layered {
                ctx.save_layer_with(0.5, None, None);
            } else {
                ctx.set_global_alpha(0.5);
            }
            ctx.set_fill_style(red());
            ctx.fill_rect(0.0, 0.0, 20.0, 20.0);
            ctx.fill_rect(10.0, 10.0, 20.0, 20.0);
            if layered {
                ctx.restore();
            }
        }
        let buffer = pixels(&mut canvas);
        // (overlap, single-coverage)
        (at(&buffer, 30, 15, 15)[3], at(&buffer, 30, 5, 5)[3])
    };

    let (plain_overlap, plain_single) = sample(false);
    let (layer_overlap, layer_single) = sample(true);

    assert!(
        plain_overlap > plain_single + 20,
        "per-draw alpha accumulates over the overlap: \
         {plain_overlap} vs {plain_single}"
    );
    assert_eq!(
        layer_overlap, layer_single,
        "group alpha is flat across the whole layer"
    );
    assert!(
        layer_overlap < plain_overlap,
        "the layer does not double the overlap ({layer_overlap} vs \
         {plain_overlap})"
    );
}

#[test]
fn save_layer_defaults_to_full_opacity() {
    let sample = |layered: bool| {
        let mut canvas = Canvas::new(20.0, 20.0);
        {
            let ctx = canvas.context();
            if layered {
                ctx.save_layer();
            }
            ctx.set_fill_style(red());
            ctx.fill_rect(0.0, 0.0, 10.0, 10.0);
            if layered {
                ctx.restore();
            }
        }
        pixels(&mut canvas)
    };

    assert_eq!(
        sample(true),
        sample(false),
        "a default layer composites as if it were not there"
    );
}

#[test]
fn save_layer_bounds_are_advisory_not_a_clip() {
    // Skia treats the layer bounds as a sizing hint for the offscreen
    // target, not as a clip, and the JavaScript `saveLayer(1.0, [0,0,10,10])`
    // behaves the same way -- verified against the binding rather than
    // assumed. This pins that contract so the doc comment stays honest.
    let sample = |bounds: Option<Rect>| {
        let mut canvas = Canvas::new(30.0, 30.0);
        {
            let ctx = canvas.context();
            ctx.save_layer_with(1.0, bounds, None);
            ctx.set_fill_style(red());
            ctx.fill_rect(0.0, 0.0, 30.0, 30.0);
            ctx.restore();
        }
        pixels(&mut canvas)
    };

    assert_eq!(
        sample(Some(Rect::from_xywh(0.0, 0.0, 10.0, 10.0))),
        sample(None),
        "a bounds hint does not restrict what the layer paints"
    );
}

#[test]
fn the_composite_operation_reaches_the_layer_paint() {
    // The layer paint carries the blend mode as well as the alpha. Multiply
    // over an existing green background darkens it; SrcOver replaces it.
    let sample = |mode: BlendMode| {
        let mut canvas = Canvas::new(20.0, 20.0);
        {
            let ctx = canvas.context();
            ctx.set_fill_style(RgbaLinear::opaque(0.0, 1.0, 0.0));
            ctx.fill_rect(0.0, 0.0, 20.0, 20.0);

            ctx.set_global_composite_operation(mode);
            ctx.save_layer_with(1.0, None, None);
            ctx.set_fill_style(red());
            ctx.fill_rect(0.0, 0.0, 20.0, 20.0);
            ctx.restore();
        }
        at(&pixels(&mut canvas), 20, 10, 10)
    };

    let over = sample(BlendMode::SourceOver);
    let multiplied = sample(BlendMode::Multiply);

    assert_eq!(over, [255, 0, 0, 255], "SrcOver replaces the background");
    assert_ne!(
        multiplied, over,
        "Multiply composites the layer against what is underneath"
    );
    assert_eq!(
        multiplied[0], 0,
        "red times green leaves no red: {multiplied:?}"
    );
}

#[test]
fn create_projection_produces_a_perspective_transform() {
    let mut canvas = Canvas::new(40.0, 40.0);
    let ctx = canvas.context();

    // Map the canvas onto a trapezoid: the top edge narrows, which is what a
    // receding plane looks like. An affine transform cannot express this.
    let projection = ctx
        .create_projection(
            [
                Point { x: 10.0, y: 0.0 },
                Point { x: 30.0, y: 0.0 },
                Point { x: 40.0, y: 40.0 },
                Point { x: 0.0, y: 40.0 },
            ],
            None,
        )
        .expect("a non-degenerate quad has a projection");

    // The projective row is what distinguishes this from an affine: at least
    // one of the first two perspective terms must be non-zero.
    let p = projection.values;
    assert!(
        p[6].abs() > f32::EPSILON || p[7].abs() > f32::EPSILON,
        "expected a perspective term, got {p:?}"
    );

    ctx.set_projection(&projection);
    ctx.set_fill_style(red());
    ctx.fill_rect(0.0, 0.0, 40.0, 40.0);

    let buffer = pixels(&mut canvas);
    // Under the trapezoid the bottom row is wider than the top.
    let row_width =
        |y: u32| (0..40).filter(|x| at(&buffer, 40, *x, y)[3] > 0).count();
    assert!(
        row_width(38) > row_width(1),
        "the mapped shape should widen downwards ({} vs {})",
        row_width(38),
        row_width(1)
    );
}

#[test]
fn create_projection_rejects_a_degenerate_quad() {
    let mut canvas = Canvas::new(40.0, 40.0);
    let ctx = canvas.context();

    // All four corners collinear: no transform maps a rectangle onto this.
    let collapsed = [
        Point { x: 0.0, y: 0.0 },
        Point { x: 10.0, y: 0.0 },
        Point { x: 20.0, y: 0.0 },
        Point { x: 30.0, y: 0.0 },
    ];
    assert!(ctx.create_projection(collapsed, None).is_none());
}

// -- Image data --------------------------------------------------------------

#[test]
fn get_image_data_reads_back_what_was_drawn() {
    let mut canvas = Canvas::new(10.0, 10.0);
    let ctx = canvas.context();
    ctx.set_fill_style(red());
    ctx.fill_rect(0.0, 0.0, 4.0, 4.0);

    let data = ctx.get_image_data(0.0, 0.0, 10.0, 10.0).expect("readback");

    assert_eq!(data.width(), 10);
    assert_eq!(data.height(), 10);
    assert_eq!(data.stride(), 40, "tightly packed RGBA rows");

    let inside = at(data.pixels(), 10, 1, 1);
    assert_eq!(inside[0], 255, "painted red");
    assert_eq!(inside[3], 255, "opaque");
    assert_eq!(at(data.pixels(), 10, 8, 8)[3], 0, "unpainted stays clear");
}

#[test]
fn get_image_data_crops_to_the_requested_rect() {
    let mut canvas = Canvas::new(10.0, 10.0);
    let ctx = canvas.context();
    ctx.set_fill_style(red());
    ctx.fill_rect(0.0, 0.0, 10.0, 10.0);

    let data = ctx.get_image_data(2.0, 3.0, 4.0, 5.0).expect("readback");

    assert_eq!((data.width(), data.height()), (4, 5));
    assert_eq!(data.pixels().len(), 4 * 5 * 4);
}

#[test]
fn get_image_data_normalizes_an_inverted_rect() {
    let mut canvas = Canvas::new(10.0, 10.0);
    let ctx = canvas.context();

    // Negative extents describe the same region backwards, which the Canvas
    // API accepts by shifting the origin.
    let data = ctx.get_image_data(6.0, 8.0, -4.0, -5.0).expect("readback");

    assert_eq!((data.width(), data.height()), (4, 5));
}

#[test]
fn get_image_data_ignores_the_current_transform() {
    let mut canvas = Canvas::new(10.0, 10.0);
    let ctx = canvas.context();
    ctx.set_fill_style(red());
    ctx.fill_rect(0.0, 0.0, 2.0, 2.0);

    // A transform that would shift the read window if readback honoured it.
    ctx.translate(5.0, 5.0);
    let data = ctx.get_image_data(0.0, 0.0, 2.0, 2.0).expect("readback");

    assert_eq!(
        at(data.pixels(), 2, 0, 0)[0],
        255,
        "read from canvas coordinates, not transformed ones"
    );
}

#[test]
fn get_image_data_rejects_an_empty_rect() {
    let mut canvas = Canvas::new(10.0, 10.0);
    assert!(matches!(
        canvas.context().get_image_data(1.0, 1.0, 0.0, 4.0),
        Err(Error::InvalidDimensions { .. })
    ));
}

#[test]
fn premultiplied_readback_scales_color_by_alpha() {
    let mut canvas = Canvas::new(4.0, 4.0);
    let ctx = canvas.context();
    ctx.set_fill_style(RgbaLinear::new_premultiplied(0.5, 0.0, 0.0, 0.5));
    ctx.fill_rect(0.0, 0.0, 4.0, 4.0);

    let straight = ctx
        .get_image_data(0.0, 0.0, 4.0, 4.0)
        .expect("unpremultiplied readback");
    let scaled = ctx
        .get_image_data_as(
            0.0,
            0.0,
            4.0,
            4.0,
            PixelExportOptions {
                premultiplied: true,
                ..PixelExportOptions::default()
            },
        )
        .expect("premultiplied readback");

    let straight_red = at(straight.pixels(), 4, 1, 1)[0];
    let scaled_red = at(scaled.pixels(), 4, 1, 1)[0];

    assert!(straight_red > 250, "unpremultiplied keeps full red");
    assert!(
        scaled_red < straight_red / 2 + 8 && scaled_red > straight_red / 2 - 8,
        "premultiplied halves it at 50% alpha: {scaled_red} vs {straight_red}"
    );
    assert!(straight.premultiplied() != scaled.premultiplied());
}

#[test]
fn create_image_data_is_transparent_and_correctly_sized() {
    let mut canvas = Canvas::new(10.0, 10.0);
    let data = canvas.context().create_image_data(3, 2).expect("allocate");

    assert_eq!((data.width(), data.height()), (3, 2));
    assert_eq!(data.pixels().len(), 3 * 2 * 4);
    assert!(data.pixels().iter().all(|&b| b == 0), "transparent black");
}

#[test]
fn create_image_data_rejects_a_zero_dimension() {
    let mut canvas = Canvas::new(10.0, 10.0);

    assert!(matches!(
        canvas.context().create_image_data(0, 4),
        Err(Error::InvalidDimensions { .. })
    ));
}

#[test]
fn put_image_data_writes_the_pixels_it_was_given() {
    let mut canvas = Canvas::new(10.0, 10.0);
    {
        let ctx = canvas.context();
        let mut patch = ctx.create_image_data(2, 2).expect("allocate");
        // Opaque green, unpremultiplied RGBA.
        for texel in patch.pixels_mut().chunks_mut(4) {
            texel.copy_from_slice(&[0, 255, 0, 255]);
        }
        ctx.put_image_data(&patch, 3.0, 4.0).expect("write");
    }

    let buffer = pixels(&mut canvas);
    assert_eq!(at(&buffer, 10, 3, 4), [0, 255, 0, 255], "at the origin");
    assert_eq!(
        at(&buffer, 10, 4, 5),
        [0, 255, 0, 255],
        "and its far corner"
    );
    assert_eq!(at(&buffer, 10, 5, 6)[3], 0, "nothing outside it");
}

#[test]
fn put_image_data_bypasses_transform_alpha_and_clip() {
    let mut canvas = Canvas::new(10.0, 10.0);
    {
        let ctx = canvas.context();
        let mut patch = ctx.create_image_data(2, 2).expect("allocate");
        patch.pixels_mut().fill(255); // opaque white

        // Every one of these would change an ordinary draw.
        ctx.translate(5.0, 5.0);
        ctx.set_global_alpha(0.25);
        ctx.rect(0.0, 0.0, 1.0, 1.0);
        ctx.clip(FillRule::NonZero);

        ctx.put_image_data(&patch, 0.0, 0.0).expect("write");
    }

    let buffer = pixels(&mut canvas);
    assert_eq!(
        at(&buffer, 10, 0, 0),
        [255, 255, 255, 255],
        "landed at the untransformed origin, fully opaque, unclipped"
    );
    assert_eq!(at(&buffer, 10, 7, 7)[3], 0, "not at the translated origin");
}

#[test]
fn put_image_data_clears_what_it_covers() {
    let mut canvas = Canvas::new(10.0, 10.0);
    {
        let ctx = canvas.context();
        ctx.set_fill_style(red());
        ctx.fill_rect(0.0, 0.0, 10.0, 10.0);

        // Fully transparent, so a blend would leave the red showing through.
        let patch = ctx.create_image_data(4, 4).expect("allocate");
        ctx.put_image_data(&patch, 0.0, 0.0).expect("write");
    }

    let buffer = pixels(&mut canvas);
    assert_eq!(at(&buffer, 10, 1, 1)[3], 0, "replaced, not composited");
    assert_eq!(at(&buffer, 10, 8, 8)[3], 255, "the rest is untouched");
}

#[test]
fn put_image_data_region_writes_only_the_dirty_part() {
    let mut canvas = Canvas::new(10.0, 10.0);
    {
        let ctx = canvas.context();
        let mut patch = ctx.create_image_data(4, 4).expect("allocate");
        patch.pixels_mut().fill(255);

        // Bottom-right quadrant of the patch only.
        ctx.put_image_data_region(&patch, 0.0, 0.0, 2.0, 2.0, 2.0, 2.0)
            .expect("write");
    }

    let buffer = pixels(&mut canvas);
    assert_eq!(at(&buffer, 10, 2, 2)[3], 255, "the dirty region landed");
    assert_eq!(at(&buffer, 10, 0, 0)[3], 0, "the rest of the patch did not");
}

#[test]
fn image_data_survives_a_round_trip() {
    let mut canvas = Canvas::new(10.0, 10.0);
    let ctx = canvas.context();
    ctx.set_fill_style(red());
    ctx.fill_rect(0.0, 0.0, 3.0, 3.0);

    let read = ctx.get_image_data(0.0, 0.0, 3.0, 3.0).expect("readback");
    ctx.put_image_data(&read, 5.0, 5.0).expect("write");

    let again = ctx
        .get_image_data(5.0, 5.0, 3.0, 3.0)
        .expect("second readback");

    assert_eq!(read.pixels(), again.pixels(), "written back byte for byte");
}

#[test]
fn from_pixels_rejects_a_buffer_of_the_wrong_length() {
    let short = ExportedPixels::from_pixels(
        2,
        2,
        PixelExportOptions::default(),
        vec![0; 8],
    );

    assert!(matches!(
        short,
        Err(Error::InvalidByteLength {
            expected: 16,
            actual: 8
        })
    ));
}

// -- Filter chain ------------------------------------------------------------

#[test]
fn set_filter_blurs_the_edge_it_is_given() {
    let sample = |ops: &[FilterOp]| {
        let mut canvas = Canvas::new(20.0, 20.0);
        let ctx = canvas.context();
        ctx.set_filter(ops).expect("valid filter");
        ctx.set_fill_style(red());
        ctx.fill_rect(5.0, 5.0, 10.0, 10.0);
        pixels(&mut canvas)
    };

    // Just outside the rectangle: sharp leaves it clear, blur bleeds into it.
    let sharp = at(&sample(&[]), 20, 3, 10);
    let blurred = at(&sample(&[FilterOp::Blur(4.0)]), 20, 3, 10);

    assert_eq!(sharp[3], 0, "unfiltered edge is hard");
    assert!(blurred[3] > 0, "blur spreads past the rectangle");
}

#[test]
fn grayscale_removes_the_color_it_is_told_to() {
    let sample = |ops: &[FilterOp]| {
        let mut canvas = Canvas::new(10.0, 10.0);
        let ctx = canvas.context();
        ctx.set_filter(ops).expect("valid filter");
        ctx.set_fill_style(red());
        ctx.fill_rect(0.0, 0.0, 10.0, 10.0);
        at(&pixels(&mut canvas), 10, 5, 5)
    };

    let color = sample(&[]);
    let gray = sample(&[FilterOp::Grayscale(1.0)]);

    assert!(color[0] > color[1], "unfiltered red dominates");
    assert_eq!(gray[0], gray[1], "fully gray: red equals green");
    assert_eq!(gray[1], gray[2], "and green equals blue");
}

#[test]
fn filter_ops_apply_in_slice_order() {
    // Mid-gray, so both operations have room to move it either way.
    let sample = |ops: &[FilterOp]| {
        let mut canvas = Canvas::new(10.0, 10.0);
        let ctx = canvas.context();
        ctx.set_filter(ops).expect("valid filter");
        ctx.set_fill_style(RgbaLinear::opaque(0.5, 0.5, 0.5));
        ctx.fill_rect(0.0, 0.0, 10.0, 10.0);
        at(&pixels(&mut canvas), 10, 5, 5)[0]
    };

    // Darkening then inverting lands high; inverting then darkening lands
    // low. Composition does not commute, and the chain has to respect that.
    let dark_then_invert =
        sample(&[FilterOp::Brightness(0.5), FilterOp::Invert(1.0)]);
    let invert_then_dark =
        sample(&[FilterOp::Invert(1.0), FilterOp::Brightness(0.5)]);

    assert!(
        dark_then_invert > invert_then_dark,
        "order changes the result: {dark_then_invert} vs {invert_then_dark}"
    );
}

#[test]
fn drop_shadow_paints_behind_the_drawing() {
    let mut canvas = Canvas::new(30.0, 30.0);
    {
        let ctx = canvas.context();
        ctx.set_filter(&[FilterOp::DropShadow {
            offset_x: 8.0,
            offset_y: 8.0,
            blur: 0.0,
            color: RgbaLinear::opaque(0.0, 0.0, 1.0),
        }])
        .expect("valid filter");
        ctx.set_fill_style(red());
        ctx.fill_rect(2.0, 2.0, 10.0, 10.0);
    }

    let buffer = pixels(&mut canvas);
    assert_eq!(at(&buffer, 30, 5, 5)[0], 255, "the drawing itself is red");
    assert_eq!(
        at(&buffer, 30, 15, 15)[2],
        255,
        "and the shadow is blue, offset by 8"
    );
}

#[test]
fn set_filter_reports_the_css_it_was_built_from() {
    let mut canvas = Canvas::new(10.0, 10.0);
    let ctx = canvas.context();

    assert_eq!(ctx.filter(), "none", "cleared by default");

    ctx.set_filter(&[FilterOp::Blur(4.0), FilterOp::Saturate(1.5)])
        .expect("valid filter");
    assert_eq!(ctx.filter(), "blur(4px) saturate(1.5)");

    ctx.set_filter(&[]).expect("valid filter");
    assert_eq!(ctx.filter(), "none", "an empty slice clears it");
}

#[test]
fn an_empty_filter_chain_clears_a_previous_one() {
    // Both halves matter. The earlier version compared `&[]` against
    // `&[Blur(0.0)]`, which are identical even when `set_filter` does
    // nothing at all, so it passed against a no-op setter.
    let sample = |apply: &dyn Fn(&mut Context2D)| {
        let mut canvas = Canvas::new(20.0, 20.0);
        {
            let ctx = canvas.context();
            apply(ctx);
            ctx.set_fill_style(red());
            ctx.fill_rect(5.0, 5.0, 10.0, 10.0);
        }
        pixels(&mut canvas)
    };

    let unfiltered = sample(&|_| {});
    let blurred = sample(&|ctx| {
        ctx.set_filter(&[FilterOp::Blur(4.0)])
            .expect("valid filter");
    });
    let cleared = sample(&|ctx| {
        ctx.set_filter(&[FilterOp::Blur(4.0)])
            .expect("valid filter");
        ctx.set_filter(&[]).expect("valid filter");
    });

    assert_ne!(blurred, unfiltered, "the blur reaches the draw");
    assert_eq!(cleared, unfiltered, "an empty slice takes it back off");
}

#[test]
fn filter_state_is_saved_and_restored() {
    let mut canvas = Canvas::new(10.0, 10.0);
    let ctx = canvas.context();

    ctx.set_filter(&[FilterOp::Blur(4.0)])
        .expect("valid filter");
    ctx.save();
    ctx.set_filter(&[FilterOp::Sepia(1.0)])
        .expect("valid filter");
    assert_eq!(ctx.filter(), "sepia(1)");

    ctx.restore();
    assert_eq!(ctx.filter(), "blur(4px)", "restore brings the chain back");
}

// -- Text styling ------------------------------------------------------------

/// Underline, solid, inheriting the fill color and the font's thickness.
fn plain_underline(ctx: &mut Context2D) {
    ctx.set_text_decoration(
        TextDecoration::underline(),
        TextDecorationStyle::Solid,
        None,
        None,
    );
}

fn clear_decoration(ctx: &mut Context2D) {
    ctx.set_text_decoration(
        TextDecoration::default(),
        TextDecorationStyle::Solid,
        None,
        None,
    );
}

#[test]
fn text_decoration_draws_ink_below_the_baseline() {
    let sample = |decorate: bool| {
        let mut canvas = Canvas::new(120.0, 60.0);
        let ctx = canvas.context();
        ctx.set_font(&Font::new("Helvetica", 24.0));
        ctx.set_fill_style(red());
        if decorate {
            plain_underline(ctx);
        }
        ctx.fill_text("nnn", 10.0, 30.0, None);
        pixels(&mut canvas)
    };

    // "nnn" has no descender, so anything painted below the baseline came
    // from the decoration.
    let count_below = |buffer: &[u8]| {
        (30..40)
            .flat_map(|y| (0..120).map(move |x| (x, y)))
            .filter(|&(x, y)| at(buffer, 120, x, y)[3] > 0)
            .count()
    };

    assert_eq!(
        count_below(&sample(false)),
        0,
        "undecorated text leaves the underline row clear"
    );
    assert!(
        count_below(&sample(true)) > 0,
        "underline paints below the baseline"
    );
}

#[test]
fn text_decoration_lines_choose_where_the_line_goes() {
    let topmost_row = |lines: TextDecoration| {
        let mut canvas = Canvas::new(120.0, 60.0);
        let ctx = canvas.context();
        ctx.set_font(&Font::new("Helvetica", 24.0));
        ctx.set_fill_style(red());
        ctx.set_text_decoration(lines, TextDecorationStyle::Solid, None, None);
        ctx.fill_text("nnn", 10.0, 30.0, None);
        let buffer = pixels(&mut canvas);
        (0..60)
            .find(|&y| (0..120).any(|x| at(&buffer, 120, x, y)[3] > 0))
            .expect("something was painted")
    };

    assert!(
        topmost_row(TextDecoration::overline())
            < topmost_row(TextDecoration::underline()),
        "an overline starts higher up than an underline"
    );
}

#[test]
fn decoration_lines_combine() {
    let both = TextDecoration {
        underline: true,
        line_through: true,
        overline: false,
    };

    // Pixels, not rows: a line-through runs across rows the glyphs already
    // paint, so it adds ink without adding a row.
    let ink = |lines: TextDecoration| {
        let mut canvas = Canvas::new(120.0, 60.0);
        let ctx = canvas.context();
        ctx.set_font(&Font::new("Helvetica", 24.0));
        ctx.set_fill_style(red());
        ctx.set_text_decoration(lines, TextDecorationStyle::Solid, None, None);
        ctx.fill_text("nnn", 10.0, 30.0, None);
        let buffer = pixels(&mut canvas);
        (0..60)
            .flat_map(|y| (0..120).map(move |x| (x, y)))
            .filter(|&(x, y)| at(&buffer, 120, x, y)[3] > 0)
            .count()
    };

    assert!(
        ink(both) > ink(TextDecoration::underline()),
        "a bitmask draws both lines, not just the first"
    );
}

#[test]
fn text_decoration_color_overrides_the_fill() {
    let mut canvas = Canvas::new(120.0, 60.0);
    {
        let ctx = canvas.context();
        ctx.set_font(&Font::new("Helvetica", 24.0));
        ctx.set_fill_style(red());
        ctx.set_text_decoration(
            TextDecoration::underline(),
            TextDecorationStyle::Solid,
            Some(RgbaLinear::opaque(0.0, 0.0, 1.0)),
            None,
        );
        ctx.fill_text("nnn", 10.0, 30.0, None);
    }

    let buffer = pixels(&mut canvas);
    let underline = (30..40)
        .flat_map(|y| (0..120).map(move |x| (x, y)))
        .map(|(x, y)| at(&buffer, 120, x, y))
        .find(|px| px[3] > 150)
        .expect("underline was painted");

    assert!(underline[2] > underline[0], "blue line under red text");
}

#[test]
fn an_empty_decoration_clears_it() {
    let mut canvas = Canvas::new(120.0, 60.0);
    let ctx = canvas.context();

    assert_eq!(ctx.text_decoration(), "none", "cleared by default");

    plain_underline(ctx);
    assert_eq!(ctx.text_decoration(), "underline solid");

    clear_decoration(ctx);
    assert_eq!(ctx.text_decoration(), "none");
}

#[test]
fn thicker_decorations_paint_more_rows() {
    let rows_below_baseline = |thickness: Option<f32>| {
        let mut canvas = Canvas::new(120.0, 60.0);
        let ctx = canvas.context();
        ctx.set_font(&Font::new("Helvetica", 24.0));
        ctx.set_fill_style(red());
        ctx.set_text_decoration(
            TextDecoration::underline(),
            TextDecorationStyle::Solid,
            None,
            thickness,
        );
        ctx.fill_text("nnn", 10.0, 30.0, None);
        let buffer = pixels(&mut canvas);
        (30..60)
            .filter(|&y| (0..120).any(|x| at(&buffer, 120, x, y)[3] > 0))
            .count()
    };

    assert!(
        rows_below_baseline(Some(6.0)) > rows_below_baseline(None),
        "an explicit thickness overrides the font's own metric"
    );
}

#[test]
fn font_stretch_selects_a_narrower_face_when_the_family_has_one() {
    let width = |stretch: FontStretch| {
        let mut canvas = Canvas::new(10.0, 10.0);
        let ctx = canvas.context();
        // Futura ships a condensed face. Helvetica and Helvetica Neue do
        // not, and render identically at every stretch -- confirmed against
        // the JavaScript `fontStretch` on the same machine, so this is the
        // font catalogue rather than the setter.
        ctx.set_font(&Font::new("Futura", 40.0));
        ctx.set_font_stretch(stretch);
        ctx.measure_text("wwwwwwww", None).width
    };

    let condensed = width(FontStretch::Condensed);
    let normal = width(FontStretch::Normal);
    assert!(
        condensed < normal,
        "condensed sets narrower: {condensed} vs {normal}"
    );
}

#[test]
fn font_variant_caps_changes_the_glyphs_chosen() {
    let width = |caps: FontVariantCaps| {
        let mut canvas = Canvas::new(10.0, 10.0);
        let ctx = canvas.context();
        // Baskerville ships a small-caps feature table. Helvetica does not,
        // and measures identically at every setting -- confirmed against the
        // JavaScript `fontVariantCaps` on the same machine, so a null result
        // there would be the font, not the setter.
        ctx.set_font(&Font::new("Baskerville", 32.0));
        ctx.set_font_variant_caps(caps);
        ctx.measure_text("abc", None).width
    };

    let lowercase = width(FontVariantCaps::Normal);
    let small_caps = width(FontVariantCaps::SmallCaps);

    assert_ne!(
        lowercase, small_caps,
        "small caps substitute different glyphs, so the run measures \
         differently"
    );
}

#[test]
fn font_variant_caps_normal_puts_the_glyphs_back() {
    let mut canvas = Canvas::new(10.0, 10.0);
    let ctx = canvas.context();
    ctx.set_font(&Font::new("Baskerville", 32.0));

    let plain = ctx.measure_text("abc", None).width;
    ctx.set_font_variant_caps(FontVariantCaps::SmallCaps);
    assert_ne!(ctx.measure_text("abc", None).width, plain);

    ctx.set_font_variant_caps(FontVariantCaps::Normal);
    assert_eq!(
        ctx.measure_text("abc", None).width,
        plain,
        "clearing the variant drops the feature rather than stacking on it"
    );
}

#[test]
fn text_decoration_is_saved_and_restored() {
    let mut canvas = Canvas::new(10.0, 10.0);
    let ctx = canvas.context();

    ctx.set_text_decoration(
        TextDecoration::overline(),
        TextDecorationStyle::Solid,
        None,
        None,
    );
    ctx.save();
    clear_decoration(ctx);
    assert_eq!(ctx.text_decoration(), "none");

    ctx.restore();
    assert_eq!(ctx.text_decoration(), "overline solid");
}

// -- Patterns and textures ---------------------------------------------------

/// A 2x2 image: red, green on the top row; blue, white on the bottom.
fn quad_tile() -> Image {
    #[rustfmt::skip]
    let pixels = vec![
        255, 0, 0, 255,    0, 255, 0, 255,
        0, 0, 255, 255,    255, 255, 255, 255,
    ];
    Image::from_pixels(
        &pixels,
        2,
        2,
        8,
        PixelFormat::Rgba8UnormUnpremul,
        PixelColorSpace::Srgb,
    )
    .expect("2x2 image")
}

#[test]
fn a_pattern_tiles_across_the_fill() {
    let mut canvas = Canvas::new(8.0, 8.0);
    {
        let ctx = canvas.context();
        let pattern = ctx.create_pattern(&quad_tile(), PatternRepeat::Repeat);
        ctx.set_fill_pattern(&pattern);
        ctx.fill_rect(0.0, 0.0, 8.0, 8.0);
    }

    let buffer = pixels(&mut canvas);
    // The same texel recurs every two pixels in both directions.
    assert_eq!(at(&buffer, 8, 0, 0), at(&buffer, 8, 2, 0), "tiles across");
    assert_eq!(at(&buffer, 8, 0, 0), at(&buffer, 8, 0, 2), "tiles down");
    assert_eq!(at(&buffer, 8, 0, 0)[0], 255, "top-left texel is red");
    assert_eq!(at(&buffer, 8, 1, 0)[1], 255, "its neighbour is green");
}

#[test]
fn no_repeat_draws_the_tile_once() {
    let mut canvas = Canvas::new(8.0, 8.0);
    {
        let ctx = canvas.context();
        let pattern = ctx.create_pattern(&quad_tile(), PatternRepeat::NoRepeat);
        ctx.set_fill_pattern(&pattern);
        ctx.fill_rect(0.0, 0.0, 8.0, 8.0);
    }

    let buffer = pixels(&mut canvas);
    assert_eq!(at(&buffer, 8, 0, 0)[0], 255, "the tile itself is drawn");
    // Beyond the 2x2 tile nothing is painted. An earlier version of this
    // test asserted the far corner carried the tile's edge texel, which
    // encoded a Clamp tile mode the Canvas API does not specify.
    assert_eq!(at(&buffer, 8, 7, 7)[3], 0, "nothing beyond the tile");
    assert_eq!(at(&buffer, 8, 3, 0)[3], 0, "not smeared sideways");
    assert_eq!(at(&buffer, 8, 0, 3)[3], 0, "not smeared downwards");
}

#[test]
fn repeat_x_tiles_on_one_axis_only() {
    let mut canvas = Canvas::new(8.0, 8.0);
    {
        let ctx = canvas.context();
        let pattern = ctx.create_pattern(&quad_tile(), PatternRepeat::RepeatX);
        ctx.set_fill_pattern(&pattern);
        ctx.fill_rect(0.0, 0.0, 8.0, 8.0);
    }

    let buffer = pixels(&mut canvas);
    assert_eq!(at(&buffer, 8, 0, 0), at(&buffer, 8, 2, 0), "wraps across");
    assert_eq!(at(&buffer, 8, 6, 0)[3], 255, "and keeps wrapping across");
    assert_eq!(at(&buffer, 8, 0, 3)[3], 0, "but does not repeat downwards");
}

#[test]
fn pattern_transform_moves_the_tiling() {
    let sample = |shift: f32| {
        let mut canvas = Canvas::new(8.0, 8.0);
        {
            let ctx = canvas.context();
            let mut pattern =
                ctx.create_pattern(&quad_tile(), PatternRepeat::Repeat);
            pattern.set_transform(Affine::translation(shift, 0.0));
            ctx.set_fill_pattern(&pattern);
            ctx.fill_rect(0.0, 0.0, 8.0, 8.0);
        }
        at(&pixels(&mut canvas), 8, 0, 0)
    };

    // One pixel across, the red texel gives way to the green one.
    assert_eq!(sample(0.0)[0], 255, "unshifted starts on red");
    assert_eq!(sample(1.0)[1], 255, "shifted by one starts on green");
}

#[test]
fn pattern_reports_its_tile_size() {
    let mut canvas = Canvas::new(8.0, 8.0);
    let pattern = canvas
        .context()
        .create_pattern(&quad_tile(), PatternRepeat::Repeat);

    assert_eq!((pattern.width(), pattern.height()), (2.0, 2.0));
}

#[test]
fn a_pattern_from_a_canvas_carries_its_drawing() {
    let mut source = Canvas::new(4.0, 4.0);
    {
        let ctx = source.context();
        ctx.set_fill_style(red());
        ctx.fill_rect(0.0, 0.0, 2.0, 2.0);
    }

    let mut canvas = Canvas::new(8.0, 8.0);
    {
        let ctx = canvas.context();
        let pattern =
            ctx.create_pattern_from_canvas(&mut source, PatternRepeat::Repeat);
        ctx.set_fill_pattern(&pattern);
        ctx.fill_rect(0.0, 0.0, 8.0, 8.0);
    }

    let buffer = pixels(&mut canvas);
    assert_eq!(at(&buffer, 8, 1, 1)[0], 255, "the source's red square");
    assert_eq!(at(&buffer, 8, 3, 3)[3], 0, "and its empty quadrant");
    assert_eq!(at(&buffer, 8, 5, 5)[0], 255, "repeated one tile over");
}

#[test]
fn a_texture_leaves_gaps_a_solid_fill_would_not() {
    let coverage = |use_texture: bool| {
        let mut canvas = Canvas::new(40.0, 40.0);
        {
            let ctx = canvas.context();
            if use_texture {
                let hatch = Texture::new(&TextureOptions {
                    color: RgbaLinear::opaque(1.0, 0.0, 0.0),
                    line: 1.0,
                    spacing: (8.0, 8.0),
                    ..TextureOptions::default()
                });
                ctx.set_fill_texture(&hatch);
            } else {
                ctx.set_fill_style(red());
            }
            ctx.fill_rect(0.0, 0.0, 40.0, 40.0);
        }
        let buffer = pixels(&mut canvas);
        (0..40)
            .flat_map(|y| (0..40).map(move |x| (x, y)))
            .filter(|&(x, y)| at(&buffer, 40, x, y)[3] > 0)
            .count()
    };

    let hatched = coverage(true);
    let solid = coverage(false);
    assert_eq!(solid, 1600, "a solid fill covers everything");
    assert!(hatched > 0, "the hatching is drawn at all");
    assert!(hatched < solid / 2, "and leaves gaps: {hatched}/{solid}");
}

#[test]
fn texture_spacing_controls_how_dense_the_hatching_is() {
    let coverage = |spacing: f32| {
        let mut canvas = Canvas::new(40.0, 40.0);
        {
            let ctx = canvas.context();
            let hatch = Texture::new(&TextureOptions {
                color: RgbaLinear::opaque(1.0, 0.0, 0.0),
                spacing: (spacing, spacing),
                ..TextureOptions::default()
            });
            ctx.set_fill_texture(&hatch);
            ctx.fill_rect(0.0, 0.0, 40.0, 40.0);
        }
        let buffer = pixels(&mut canvas);
        (0..40)
            .flat_map(|y| (0..40).map(move |x| (x, y)))
            .filter(|&(x, y)| at(&buffer, 40, x, y)[3] > 0)
            .count()
    };

    assert!(
        coverage(4.0) > coverage(16.0),
        "tighter spacing paints more: {} vs {}",
        coverage(4.0),
        coverage(16.0)
    );
}

#[test]
fn a_texture_can_stroke_as_well_as_fill() {
    let mut canvas = Canvas::new(40.0, 40.0);
    {
        let ctx = canvas.context();
        let hatch = Texture::new(&TextureOptions {
            color: RgbaLinear::opaque(1.0, 0.0, 0.0),
            spacing: (4.0, 4.0),
            ..TextureOptions::default()
        });
        ctx.set_stroke_texture(&hatch);
        ctx.set_line_width(10.0);
        ctx.begin_path();
        ctx.move_to(0.0, 20.0);
        ctx.line_to(40.0, 20.0);
        ctx.stroke();
    }

    let buffer = pixels(&mut canvas);
    let on_band = (0..40)
        .flat_map(|y| (15..25).map(move |x| (x, y)))
        .filter(|&(x, y)| at(&buffer, 40, y, x)[3] > 0)
        .count();
    let off_band = (0..40)
        .flat_map(|y| (0..8).map(move |x| (x, y)))
        .filter(|&(x, y)| at(&buffer, 40, y, x)[3] > 0)
        .count();

    assert!(on_band > 0, "the stroked band carries texture ink");
    assert_eq!(off_band, 0, "nothing outside the stroke");
}

// -- Drawing another canvas --------------------------------------------------

#[test]
fn draw_canvas_copies_the_source_drawing() {
    let mut source = Canvas::new(10.0, 10.0);
    {
        let ctx = source.context();
        ctx.set_fill_style(red());
        ctx.fill_rect(0.0, 0.0, 5.0, 5.0);
    }

    let mut canvas = Canvas::new(20.0, 20.0);
    canvas.context().draw_canvas(&mut source, 10.0, 10.0);

    let buffer = pixels(&mut canvas);
    assert_eq!(at(&buffer, 20, 12, 12)[0], 255, "landed at the offset");
    assert_eq!(at(&buffer, 20, 2, 2)[3], 0, "not at the origin");
    assert_eq!(at(&buffer, 20, 18, 18)[3], 0, "and the empty part is empty");
}

#[test]
fn draw_canvas_sized_scales_the_source() {
    let mut source = Canvas::new(10.0, 10.0);
    {
        let ctx = source.context();
        ctx.set_fill_style(red());
        ctx.fill_rect(0.0, 0.0, 5.0, 5.0);
    }

    let mut canvas = Canvas::new(20.0, 20.0);
    canvas
        .context()
        .draw_canvas_sized(&mut source, 0.0, 0.0, 20.0, 20.0);

    let buffer = pixels(&mut canvas);
    // The source's top-left quarter now covers the destination's.
    assert_eq!(at(&buffer, 20, 8, 8)[0], 255, "scaled up 2x");
    assert_eq!(at(&buffer, 20, 12, 12)[3], 0, "past the scaled square");
}

#[test]
fn draw_canvas_region_crops_the_source() {
    let mut source = Canvas::new(10.0, 10.0);
    {
        let ctx = source.context();
        ctx.set_fill_style(red());
        ctx.fill_rect(0.0, 0.0, 5.0, 5.0);
        ctx.set_fill_style(RgbaLinear::opaque(0.0, 0.0, 1.0));
        ctx.fill_rect(5.0, 5.0, 5.0, 5.0);
    }

    let mut canvas = Canvas::new(10.0, 10.0);
    canvas.context().draw_canvas_region(
        &mut source,
        5.0,
        5.0,
        5.0,
        5.0,
        0.0,
        0.0,
        10.0,
        10.0,
    );

    let buffer = pixels(&mut canvas);
    assert_eq!(at(&buffer, 10, 5, 5)[2], 255, "only the blue quadrant");
    assert_eq!(at(&buffer, 10, 5, 5)[0], 0, "no red from the other one");
}

#[test]
fn draw_canvas_honours_the_transform() {
    let mut source = Canvas::new(10.0, 10.0);
    {
        let ctx = source.context();
        ctx.set_fill_style(red());
        ctx.fill_rect(0.0, 0.0, 4.0, 4.0);
    }

    let mut canvas = Canvas::new(20.0, 20.0);
    {
        let ctx = canvas.context();
        ctx.translate(10.0, 0.0);
        ctx.draw_canvas(&mut source, 0.0, 0.0);
    }

    let buffer = pixels(&mut canvas);
    assert_eq!(at(&buffer, 20, 12, 2)[0], 255, "moved by the translate");
    assert_eq!(at(&buffer, 20, 2, 2)[3], 0, "not at the untransformed spot");
}

#[test]
fn drawing_an_empty_canvas_paints_nothing() {
    let render = |draw_into_source: bool| {
        let mut source = Canvas::new(10.0, 10.0);
        if draw_into_source {
            let ctx = source.context();
            ctx.set_fill_style(red());
            ctx.fill_rect(0.0, 0.0, 10.0, 10.0);
        }
        let mut canvas = Canvas::new(10.0, 10.0);
        canvas.context().draw_canvas(&mut source, 0.0, 0.0);
        pixels(&mut canvas)
    };

    // The "paints something" half is what makes the "paints nothing" half
    // meaningful: on its own the latter passes against a `draw_canvas` with
    // an empty body.
    assert!(
        render(true).chunks(4).any(|texel| texel[3] > 0),
        "a source with content does paint"
    );
    assert!(
        render(false).chunks(4).all(|texel| texel[3] == 0),
        "a blank source leaves the destination blank"
    );
}

// -- Paragraphs --------------------------------------------------------------

#[test]
fn draw_paragraph_paints_the_laid_out_text() {
    let engine = TextEngine::new(&FontManager::new());
    let layout = engine.layout_text(
        "Studio",
        &TextStyle {
            font_families: vec!["Helvetica".to_string()],
            font_size: 24.0,
            color: RgbaLinear::opaque(1.0, 0.0, 0.0),
            ..TextStyle::default()
        },
        200.0,
    );

    let mut canvas = Canvas::new(200.0, 60.0);
    canvas.context().draw_paragraph(&layout, 10.0, 10.0);

    let buffer = pixels(&mut canvas);
    let ink = (0..60)
        .flat_map(|y| (0..200).map(move |x| (x, y)))
        .filter(|&(x, y)| at(&buffer, 200, x, y)[3] > 0)
        .count();

    assert!(ink > 0, "the paragraph was painted");
    assert!(
        (0..10).all(|y| (0..200).all(|x| at(&buffer, 200, x, y)[3] == 0)),
        "and nothing above its origin"
    );
}

#[test]
fn draw_paragraph_takes_its_color_from_the_layout() {
    let engine = TextEngine::new(&FontManager::new());
    let layout = engine.layout_text(
        "Studio",
        &TextStyle {
            font_families: vec!["Helvetica".to_string()],
            font_size: 24.0,
            color: RgbaLinear::opaque(0.0, 0.0, 1.0),
            ..TextStyle::default()
        },
        200.0,
    );

    let mut canvas = Canvas::new(200.0, 60.0);
    {
        let ctx = canvas.context();
        // The context's own fill style must not win: the paragraph carries
        // its own paints.
        ctx.set_fill_style(red());
        ctx.draw_paragraph(&layout, 10.0, 10.0);
    }

    let buffer = pixels(&mut canvas);
    let solid = (0..60)
        .flat_map(|y| (0..200).map(move |x| (x, y)))
        .map(|(x, y)| at(&buffer, 200, x, y))
        .find(|px| px[3] > 200)
        .expect("something was painted");

    assert!(solid[2] > solid[0], "blue from the layout, not the context");
}

#[test]
fn draw_paragraph_is_composited_through_the_context() {
    let alpha_at = |global_alpha: f32| {
        let engine = TextEngine::new(&FontManager::new());
        let layout = engine.layout_text(
            "Studio",
            &TextStyle {
                font_families: vec!["Helvetica".to_string()],
                font_size: 24.0,
                color: RgbaLinear::opaque(1.0, 0.0, 0.0),
                ..TextStyle::default()
            },
            200.0,
        );

        let mut canvas = Canvas::new(200.0, 60.0);
        {
            let ctx = canvas.context();
            ctx.set_global_alpha(global_alpha);
            ctx.draw_paragraph(&layout, 10.0, 10.0);
        }
        let buffer = pixels(&mut canvas);
        (0..60)
            .flat_map(|y| (0..200).map(move |x| (x, y)))
            .map(|(x, y)| at(&buffer, 200, x, y)[3])
            .max()
            .unwrap_or(0)
    };

    let opaque = alpha_at(1.0);
    let faded = alpha_at(0.25);
    assert!(opaque > 200, "an unfaded paragraph is solid");
    assert!(
        faded < opaque / 2,
        "globalAlpha reaches the paragraph: {faded} vs {opaque}"
    );
}

// -- Dash markers ------------------------------------------------------------

#[test]
fn a_dash_marker_replaces_the_dashes() {
    let render = |marker: Option<&Path>| {
        let mut canvas = Canvas::new(40.0, 20.0);
        {
            let ctx = canvas.context();
            ctx.set_stroke_style(red());
            ctx.set_line_width(2.0);
            ctx.set_line_dash(&[6.0, 6.0]);
            ctx.set_line_dash_marker(marker);
            ctx.begin_path();
            ctx.move_to(0.0, 10.0);
            ctx.line_to(40.0, 10.0);
            ctx.stroke();
        }
        pixels(&mut canvas)
    };

    let square = Path::from_svg("M-3 -3 L3 -3 L3 3 L-3 3 Z", FillRule::NonZero)
        .expect("marker path");

    assert_ne!(
        render(None),
        render(Some(&square)),
        "stamping a marker draws something other than plain dashes"
    );
}

#[test]
fn clearing_the_dash_marker_restores_plain_dashes() {
    let render = |marker: Option<&Path>| {
        let mut canvas = Canvas::new(40.0, 20.0);
        {
            let ctx = canvas.context();
            ctx.set_stroke_style(red());
            ctx.set_line_width(2.0);
            ctx.set_line_dash(&[6.0, 6.0]);
            ctx.set_line_dash_marker(marker);
            ctx.begin_path();
            ctx.move_to(0.0, 10.0);
            ctx.line_to(40.0, 10.0);
            ctx.stroke();
        }
        pixels(&mut canvas)
    };

    let square = Path::from_svg("M-3 -3 L3 -3 L3 3 L-3 3 Z", FillRule::NonZero)
        .expect("marker path");
    let plain = render(None);
    let stamped = render(Some(&square));

    // Setting then clearing must land back on plain. Asserting only that
    // last equality passes against a setter that never did anything, so the
    // difference is asserted first.
    assert_ne!(stamped, plain, "the marker changes the stroke");

    let mut canvas = Canvas::new(40.0, 20.0);
    {
        let ctx = canvas.context();
        ctx.set_stroke_style(red());
        ctx.set_line_width(2.0);
        ctx.set_line_dash(&[6.0, 6.0]);
        ctx.set_line_dash_marker(Some(&square));
        ctx.set_line_dash_marker(None);
        ctx.begin_path();
        ctx.move_to(0.0, 10.0);
        ctx.line_to(40.0, 10.0);
        ctx.stroke();
    }
    assert_eq!(pixels(&mut canvas), plain, "None puts the dashes back");
}

#[test]
fn dash_fit_changes_how_the_marker_sits_on_a_curve() {
    let render = |fit: DashFit| {
        // An asymmetric marker on a curve, so orientation is visible.
        let marker =
            Path::from_svg("M-6 -1 L6 -1 L6 1 L-6 1 Z", FillRule::NonZero)
                .expect("marker path");
        let mut canvas = Canvas::new(60.0, 60.0);
        {
            let ctx = canvas.context();
            ctx.set_stroke_style(red());
            ctx.set_line_width(1.0);
            ctx.set_line_dash(&[12.0, 12.0]);
            ctx.set_line_dash_marker(Some(&marker));
            ctx.set_line_dash_fit(fit);
            ctx.begin_path();
            ctx.arc(30.0, 30.0, 20.0, 0.0, std::f32::consts::TAU, false);
            ctx.stroke();
        }
        pixels(&mut canvas)
    };

    assert_ne!(
        render(DashFit::Move),
        render(DashFit::Turn),
        "an unrotated marker differs from one turned to the tangent"
    );
}

// -- Font variants and variations --------------------------------------------

#[test]
fn font_features_reach_the_shaper() {
    let width = |features: &[FontFeature]| {
        let mut canvas = Canvas::new(10.0, 10.0);
        let ctx = canvas.context();
        ctx.set_font(&Font::new("Baskerville", 32.0));
        ctx.set_font_variant(FontVariantCaps::Normal, features);
        ctx.measure_text("abc", None).width
    };

    assert_ne!(
        width(&[]),
        width(&[FontFeature::on("smcp")]),
        "an explicit feature tag substitutes different glyphs"
    );
}

#[test]
fn setting_caps_preserves_other_font_features() {
    // The JS `fontVariantCaps` setter rewrites only the caps token of the
    // `font-variant` string, leaving the rest -- confirmed against the
    // binding, where `oldstyle-nums` survives both a caps change and a caps
    // clear. This used to clobber every other feature.
    let mut canvas = Canvas::new(10.0, 10.0);
    let ctx = canvas.context();
    ctx.set_font(&Font::new("Baskerville", 32.0));

    let plain = ctx.measure_text("abc123", None).width;

    ctx.set_font_variant(FontVariantCaps::Normal, &[FontFeature::on("onum")]);
    let with_figures = ctx.measure_text("abc123", None).width;
    assert_ne!(with_figures, plain, "old-style figures changed the run");

    // Changing the caps must not drop `onum`.
    ctx.set_font_variant_caps(FontVariantCaps::SmallCaps);
    let caps_and_figures = ctx.measure_text("abc123", None).width;

    ctx.set_font_variant(
        FontVariantCaps::SmallCaps,
        &[FontFeature::on("onum")],
    );
    assert_eq!(
        ctx.measure_text("abc123", None).width,
        caps_and_figures,
        "setting caps kept the other features"
    );

    // Clearing the caps must also leave them alone.
    ctx.set_font_variant_caps(FontVariantCaps::Normal);
    assert_eq!(
        ctx.measure_text("abc123", None).width,
        with_figures,
        "clearing caps kept the other features"
    );
}

#[test]
fn font_variation_settings_move_along_the_axis() {
    let width = |variations: &[FontVariation]| {
        let mut canvas = Canvas::new(10.0, 10.0);
        let ctx = canvas.context();
        // macOS ships "Skia" as a variable font with a `wdth` axis.
        // Helvetica and Menlo are static and would measure identically at
        // every setting -- confirmed against the JavaScript
        // `fontVariationSettings` on the same machine.
        ctx.set_font(&Font::new("Skia", 40.0));
        ctx.set_font_variation_settings(variations);
        ctx.measure_text("Studio", None).width
    };

    let base = width(&[]);
    let narrow = width(&[FontVariation::new(FontAxisTag::WDTH, 0.5)]);

    assert_ne!(base, narrow, "the width axis changed the advance");
}

#[test]
fn clearing_font_variation_settings_restores_the_default_instance() {
    let mut canvas = Canvas::new(10.0, 10.0);
    let ctx = canvas.context();
    ctx.set_font(&Font::new("Skia", 40.0));

    let base = ctx.measure_text("Studio", None).width;
    ctx.set_font_variation_settings(&[FontVariation::new(
        FontAxisTag::WDTH,
        0.5,
    )]);
    assert_ne!(ctx.measure_text("Studio", None).width, base);

    ctx.set_font_variation_settings(&[]);
    assert_eq!(
        ctx.measure_text("Studio", None).width,
        base,
        "an empty slice returns the face to its default instance"
    );
}

// -- Page selection ----------------------------------------------------------

#[test]
fn a_raster_export_encodes_the_current_page() {
    let mut canvas = Canvas::new(10.0, 10.0);
    {
        let ctx = canvas.context();
        ctx.set_fill_style(red());
        ctx.fill_rect(0.0, 0.0, 10.0, 10.0);
    }
    {
        let ctx = canvas.new_page();
        ctx.set_fill_style(RgbaLinear::opaque(0.0, 0.0, 1.0));
        ctx.fill_rect(0.0, 0.0, 10.0, 10.0);
    }

    // The page just drawn into, not the one the canvas started with.
    let px = at(&pixels(&mut canvas), 10, 5, 5);
    assert_eq!(px[2], 255, "blue, from the current page");
    assert_eq!(px[0], 0, "not red, from the first page");
}

#[test]
fn a_new_blank_page_hides_the_earlier_drawing() {
    let mut canvas = Canvas::new(10.0, 10.0);
    {
        let ctx = canvas.context();
        ctx.set_fill_style(red());
        ctx.fill_rect(0.0, 0.0, 10.0, 10.0);
    }
    canvas.new_page();

    // Nothing was drawn on the new page, so the export is blank -- the
    // earlier page must not leak through.
    assert!(
        pixels(&mut canvas).chunks(4).all(|texel| texel[3] == 0),
        "a fresh current page exports blank"
    );
}

#[test]
fn a_multi_page_pdf_keeps_every_page() {
    let pdf_with = |extra_pages: usize| {
        let mut canvas = Canvas::new(10.0, 10.0);
        canvas.context().fill_rect(0.0, 0.0, 5.0, 5.0);
        for _ in 0..extra_pages {
            canvas.new_page().fill_rect(0.0, 0.0, 5.0, 5.0);
        }
        canvas
            .to_buffer(ImageFormat::Pdf, &EncodeOptions::default())
            .expect("pdf export")
    };

    let one = pdf_with(0);
    let three = pdf_with(2);

    assert!(one.starts_with(b"%PDF"), "a PDF was produced");
    assert!(
        count_occurrences(&three, b"/Page") > count_occurrences(&one, b"/Page"),
        "three pages carry more page objects than one"
    );
    assert!(
        three.len() > one.len(),
        "and the document is larger: {} vs {}",
        three.len(),
        one.len()
    );
}

/// Non-overlapping occurrences of `needle` in `haystack`.
fn count_occurrences(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|window| *window == needle)
        .count()
}

// -- Filter validation -------------------------------------------------------

#[test]
fn non_finite_filter_amounts_are_rejected() {
    let mut canvas = Canvas::new(10.0, 10.0);
    let ctx = canvas.context();

    // Each of these reaches Skia as a colour matrix or blur sigma. Skia
    // returns a null filter for a non-finite entry and skia-safe unwraps it,
    // so an accepted value here aborts on the next draw rather than at the
    // call site.
    for op in [
        FilterOp::Blur(f32::NAN),
        FilterOp::Blur(f32::INFINITY),
        FilterOp::Brightness(f32::INFINITY),
        FilterOp::Contrast(f32::NAN),
        FilterOp::Grayscale(f32::NAN),
        FilterOp::HueRotate(f32::NAN),
        FilterOp::HueRotate(f32::NEG_INFINITY),
        FilterOp::Invert(f32::NAN),
        FilterOp::Opacity(f32::NAN),
        FilterOp::Saturate(f32::INFINITY),
        FilterOp::Sepia(f32::NAN),
    ] {
        assert!(
            matches!(ctx.set_filter(&[op]), Err(Error::FilterCreate { .. })),
            "{op:?} should be rejected"
        );
    }
}

#[test]
fn non_finite_drop_shadow_fields_are_rejected() {
    let mut canvas = Canvas::new(10.0, 10.0);
    let ctx = canvas.context();

    let shadow = |offset_x, offset_y, blur, color| FilterOp::DropShadow {
        offset_x,
        offset_y,
        blur,
        color,
    };
    let red = RgbaLinear::opaque(1.0, 0.0, 0.0);

    assert!(
        ctx.set_filter(&[shadow(2.0, 2.0, 1.0, red)]).is_ok(),
        "the finite case is accepted"
    );

    for broken in [
        shadow(f32::NAN, 2.0, 1.0, red),
        shadow(2.0, f32::INFINITY, 1.0, red),
        shadow(2.0, 2.0, f32::NAN, red),
        shadow(
            2.0,
            2.0,
            1.0,
            RgbaLinear::new_premultiplied(f32::NAN, 0.0, 0.0, 1.0),
        ),
    ] {
        assert!(
            matches!(
                ctx.set_filter(&[broken]),
                Err(Error::FilterCreate { .. })
            ),
            "{broken:?} should be rejected"
        );
    }
}

#[test]
fn a_rejected_filter_leaves_the_previous_chain_intact() {
    let mut canvas = Canvas::new(10.0, 10.0);
    let ctx = canvas.context();

    ctx.set_filter(&[FilterOp::Blur(4.0)])
        .expect("valid filter");
    let _ =
        ctx.set_filter(&[FilterOp::Saturate(1.5), FilterOp::Sepia(f32::NAN)]);

    assert_eq!(
        ctx.filter(),
        "blur(4px)",
        "a rejected chain must not half-apply"
    );
}

#[test]
fn a_rejected_filter_does_not_poison_later_drawing() {
    // The regression this guards: an accepted NaN builds a null Skia filter
    // that aborts on the next draw, far from the call that caused it.
    let mut canvas = Canvas::new(20.0, 20.0);
    {
        let ctx = canvas.context();
        let _ = ctx.set_filter(&[FilterOp::Opacity(f32::NAN)]);
        ctx.set_fill_style(red());
        ctx.fill_rect(0.0, 0.0, 20.0, 20.0);
    }

    assert_eq!(
        at(&pixels(&mut canvas), 20, 10, 10)[0],
        255,
        "drawing proceeds normally after a rejected filter"
    );
}

// -- Drawing an explicit path ------------------------------------------------

fn triangle() -> Path {
    Path::from_svg("M0 0 L20 0 L20 20 Z", FillRule::NonZero).expect("svg path")
}

#[test]
fn fill_path_paints_the_path_it_is_given() {
    let mut canvas = Canvas::new(30.0, 30.0);
    {
        let ctx = canvas.context();
        ctx.set_fill_style(red());
        ctx.fill_path(&triangle(), FillRule::NonZero);
    }

    let buffer = pixels(&mut canvas);
    assert_eq!(at(&buffer, 30, 17, 10)[0], 255, "inside the triangle");
    assert_eq!(at(&buffer, 30, 2, 15)[3], 0, "outside it");
}

#[test]
fn fill_path_leaves_the_current_path_alone() {
    let mut canvas = Canvas::new(30.0, 30.0);
    {
        let ctx = canvas.context();
        ctx.set_fill_style(red());
        // A current path is under construction; filling an explicit path
        // must not consume or disturb it.
        ctx.begin_path();
        ctx.rect(0.0, 20.0, 10.0, 10.0);
        ctx.fill_path(&triangle(), FillRule::NonZero);
        ctx.fill(FillRule::NonZero);
    }

    let buffer = pixels(&mut canvas);
    assert_eq!(at(&buffer, 30, 17, 10)[0], 255, "the explicit path landed");
    assert_eq!(
        at(&buffer, 30, 5, 25)[0],
        255,
        "and the current path still did"
    );
}

#[test]
fn stroke_path_outlines_without_filling() {
    let mut canvas = Canvas::new(30.0, 30.0);
    {
        let ctx = canvas.context();
        ctx.set_stroke_style(red());
        ctx.set_line_width(2.0);
        ctx.stroke_path(&triangle());
    }

    let buffer = pixels(&mut canvas);
    // A 2px stroke centred on the edge at y=0 covers row 0; row 1 is already
    // outside it.
    assert!(
        (2..18).any(|x| at(&buffer, 30, x, 0)[3] > 0),
        "the top edge is stroked"
    );
    assert_eq!(at(&buffer, 30, 16, 8)[3], 0, "the interior is not filled");
}

#[test]
fn clip_to_path_restricts_later_drawing() {
    let mut canvas = Canvas::new(30.0, 30.0);
    {
        let ctx = canvas.context();
        ctx.save();
        ctx.clip_to_path(&triangle(), FillRule::NonZero);
        ctx.set_fill_style(red());
        ctx.fill_rect(0.0, 0.0, 30.0, 30.0);
        ctx.restore();
    }

    let buffer = pixels(&mut canvas);
    assert_eq!(at(&buffer, 30, 17, 10)[0], 255, "inside the clip");
    assert_eq!(at(&buffer, 30, 2, 15)[3], 0, "outside it");
    assert_eq!(at(&buffer, 30, 25, 25)[3], 0, "well outside it");
}

#[test]
fn clip_to_path_is_undone_by_restore() {
    let mut canvas = Canvas::new(30.0, 30.0);
    {
        let ctx = canvas.context();
        ctx.save();
        ctx.clip_to_path(&triangle(), FillRule::NonZero);
        ctx.restore();
        ctx.set_fill_style(red());
        ctx.fill_rect(0.0, 0.0, 30.0, 30.0);
    }

    assert_eq!(
        at(&pixels(&mut canvas), 30, 25, 25)[0],
        255,
        "the clip lifted with the restore"
    );
}

#[test]
fn hit_testing_an_explicit_path() {
    let mut canvas = Canvas::new(30.0, 30.0);
    let ctx = canvas.context();
    let path = triangle();

    assert!(
        ctx.is_point_in_filled_path(&path, 17.0, 10.0, FillRule::NonZero),
        "inside the filled triangle"
    );
    assert!(
        !ctx.is_point_in_filled_path(&path, 2.0, 15.0, FillRule::NonZero),
        "outside it"
    );

    ctx.set_line_width(4.0);
    assert!(
        ctx.is_point_in_stroked_path(&path, 10.0, 0.0),
        "on the stroked top edge"
    );
    assert!(
        !ctx.is_point_in_stroked_path(&path, 16.0, 8.0),
        "in the interior, which the stroke does not cover"
    );
}

#[test]
fn outline_text_produces_a_path_the_context_can_draw() {
    // The gap this closes: outline_text returned a Path that nothing in the
    // facade accepted, so its output could only be inspected, never drawn.
    let mut canvas = Canvas::new(200.0, 60.0);
    {
        let ctx = canvas.context();
        ctx.set_font(&Font::new("Helvetica", 32.0));
        let glyphs = ctx.outline_text("Rust", None);

        ctx.set_fill_style(red());
        ctx.translate(10.0, 40.0);
        ctx.fill_path(&glyphs, FillRule::NonZero);
    }

    let ink = pixels(&mut canvas)
        .chunks(4)
        .filter(|texel| texel[3] > 0)
        .count();
    assert!(ink > 0, "the outlined glyphs were painted");
}

// -- Canvas sizing -----------------------------------------------------------

#[test]
fn new_page_with_resizes_the_canvas() {
    let mut canvas = Canvas::new(40.0, 40.0);
    canvas.new_page_with(20.0, 10.0);

    assert_eq!((canvas.width(), canvas.height()), (20.0, 10.0));
}

#[test]
fn a_later_page_inherits_the_resized_dimensions() {
    let mut canvas = Canvas::new(40.0, 40.0);
    canvas.new_page_with(20.0, 10.0);
    canvas.new_page();

    // The bug this guards: `new_page` read the canvas's stale size, so a
    // page added after a resize reverted to the original dimensions.
    canvas.context().fill_rect(0.0, 0.0, 20.0, 10.0);
    let raw = canvas
        .to_buffer(ImageFormat::Raw, &EncodeOptions::default())
        .expect("raw export");

    assert_eq!((canvas.width(), canvas.height()), (20.0, 10.0));
    assert_eq!(raw.len(), 20 * 10 * 4, "the exported page is 20x10");
}

#[test]
fn earlier_pages_keep_their_own_size() {
    let mut canvas = Canvas::new(40.0, 40.0);
    canvas.context().fill_rect(0.0, 0.0, 40.0, 40.0);
    canvas.new_page_with(20.0, 10.0);
    canvas.context().fill_rect(0.0, 0.0, 20.0, 10.0);

    // A resize applies onward, not retroactively -- which is what lets a
    // multi-page PDF carry pages of differing dimensions.
    assert_eq!(canvas.page_count(), 2);
    let pdf = canvas
        .to_buffer(ImageFormat::Pdf, &EncodeOptions::default())
        .expect("pdf export");
    assert!(pdf.starts_with(b"%PDF"));
}

// -- Pixel buffer sizing -----------------------------------------------------

#[test]
fn an_overflowing_pixel_buffer_is_rejected() {
    // `stride * height` overflows usize here. A release build wraps rather
    // than panicking, and it wrapped to exactly zero -- so this returned Ok
    // with an empty Vec that still reported its full width, height and
    // stride, breaking the one invariant the type promises.
    let huge = 1u32 << 30;
    let options = PixelExportOptions {
        depth: PixelDepth::F32,
        ..PixelExportOptions::default()
    };

    assert!(
        matches!(
            ExportedPixels::blank(huge, huge, options),
            Err(Error::InvalidDimensions { .. })
        ),
        "an unrepresentable buffer size is an error, not an empty buffer"
    );
    assert!(
        matches!(
            ExportedPixels::from_pixels(huge, huge, options, Vec::new()),
            Err(Error::InvalidDimensions { .. })
        ),
        "and an empty Vec does not satisfy it either"
    );
}

#[test]
fn a_representable_pixel_buffer_still_works() {
    let data = ExportedPixels::blank(4, 3, PixelExportOptions::default())
        .expect("small buffer");

    assert_eq!(data.stride(), 16);
    assert_eq!(data.pixels().len(), 4 * 3 * 4);
}

// -- Filter CSS --------------------------------------------------------------

#[test]
fn drop_shadow_css_reports_an_srgb_color() {
    let mut canvas = Canvas::new(10.0, 10.0);
    let ctx = canvas.context();

    ctx.set_filter(&[FilterOp::DropShadow {
        offset_x: 2.0,
        offset_y: 3.0,
        blur: 4.0,
        color: RgbaLinear::opaque(1.0, 0.0, 0.0),
    }])
    .expect("valid filter");

    // Not the struct's premultiplied linear floats: CSS rgb() is straight
    // sRGB on 0..255, so emitting the raw fields parsed back as near-black.
    let css = ctx.filter();
    assert!(
        css.contains("rgba(255 0 0 / 1)"),
        "expected an sRGB triple, got {css}"
    );
    assert!(css.starts_with("drop-shadow(2px 3px 4px "), "got {css}");
}

#[test]
fn drop_shadow_css_survives_a_half_alpha_color() {
    let mut canvas = Canvas::new(10.0, 10.0);
    let ctx = canvas.context();

    ctx.set_filter(&[FilterOp::DropShadow {
        offset_x: 0.0,
        offset_y: 0.0,
        blur: 1.0,
        color: RgbaLinear::new_premultiplied(0.5, 0.0, 0.0, 0.5),
    }])
    .expect("valid filter");

    let css = ctx.filter();
    assert!(
        css.contains("rgba(255 0 0 / 0.5"),
        "unpremultiplied to full red at half alpha, got {css}"
    );
}

#[test]
fn start_and_end_alignment_follow_the_reading_direction() {
    let ink_start = |align: TextAlign, direction: TextDirection| {
        let mut canvas = Canvas::new(120.0, 40.0);
        {
            let ctx = canvas.context();
            ctx.set_direction(direction);
            ctx.set_text_align(align);
            ctx.set_fill_style(red());
            ctx.set_font(&Font::new("Helvetica", 20.0));
            ctx.fill_text("abc", 60.0, 28.0, None);
        }
        let buffer = pixels(&mut canvas);
        (0..120)
            .find(|&x| (0..40).any(|y| at(&buffer, 120, x, y)[3] > 0))
            .expect("text was painted")
    };

    // Left and Right are absolute: direction cannot move them.
    assert_eq!(
        ink_start(TextAlign::Left, TextDirection::LeftToRight),
        ink_start(TextAlign::Left, TextDirection::RightToLeft),
        "Left is absolute"
    );
    assert_eq!(
        ink_start(TextAlign::Right, TextDirection::LeftToRight),
        ink_start(TextAlign::Right, TextDirection::RightToLeft),
        "Right is absolute"
    );

    // Start and End are relative, which is the whole point of them.
    assert_ne!(
        ink_start(TextAlign::Start, TextDirection::LeftToRight),
        ink_start(TextAlign::Start, TextDirection::RightToLeft),
        "Start follows the direction"
    );
    assert_ne!(
        ink_start(TextAlign::End, TextDirection::LeftToRight),
        ink_start(TextAlign::End, TextDirection::RightToLeft),
        "End follows the direction"
    );

    // And they mirror each other.
    assert_eq!(
        ink_start(TextAlign::Start, TextDirection::LeftToRight),
        ink_start(TextAlign::Left, TextDirection::LeftToRight),
        "Start is Left under ltr"
    );
    assert_eq!(
        ink_start(TextAlign::End, TextDirection::LeftToRight),
        ink_start(TextAlign::Right, TextDirection::LeftToRight),
        "End is Right under ltr"
    );
}

#[test]
fn justify_stretches_wrapped_lines() {
    let render = |align: TextAlign| {
        let mut canvas = Canvas::new(140.0, 80.0);
        {
            let ctx = canvas.context();
            ctx.set_text_wrap(true);
            ctx.set_text_align(align);
            ctx.set_fill_style(red());
            ctx.set_font(&Font::new("Helvetica", 16.0));
            ctx.fill_text(
                "one two three four five six seven",
                4.0,
                20.0,
                Some(120.0),
            );
        }
        pixels(&mut canvas)
    };

    assert_ne!(
        render(TextAlign::Justify),
        render(TextAlign::Left),
        "justified lines are spaced differently from ragged-right ones"
    );
}

#[test]
fn fractional_read_rects_floor_like_the_canvas_api() {
    let mut canvas = Canvas::new(20.0, 20.0);
    let ctx = canvas.context();

    // Measured against the JavaScript `getImageData` on the same build.
    // Every value is floored; rounding the four edges of a rectangle instead
    // reads (2.2, 2.2, 4.4, 4.4) as 5x5.
    for (x, y, w, h, expected) in [
        (2.2, 2.2, 4.4, 4.4, (4, 4)),
        (2.9, 2.9, 4.9, 4.9, (4, 4)),
        (0.0, 0.0, 3.5, 3.5, (3, 3)),
        (6.0, 8.0, -4.0, -5.0, (4, 5)),
    ] {
        let data = ctx.get_image_data(x, y, w, h).expect("readback");
        assert_eq!(
            (data.width(), data.height()),
            expected,
            "getImageData({x}, {y}, {w}, {h})"
        );
    }
}

// -- Encoded output ----------------------------------------------------------

#[test]
fn drawn_content_survives_every_raster_format() {
    // Nothing else asserts this: every other content check reads back
    // through ImageFormat::Raw, so an encoder that dropped the drawing
    // entirely would go unnoticed.
    let encode = |format: ImageFormat| {
        let mut canvas = Canvas::new(20.0, 20.0);
        {
            let ctx = canvas.context();
            ctx.set_fill_style(red());
            ctx.fill_rect(0.0, 0.0, 20.0, 20.0);
        }
        canvas
            .to_buffer(format, &EncodeOptions::default())
            .expect("export")
    };

    let blank = |format: ImageFormat| {
        let mut canvas = Canvas::new(20.0, 20.0);
        canvas
            .to_buffer(format, &EncodeOptions::default())
            .expect("export")
    };

    for format in [ImageFormat::Png, ImageFormat::Jpeg, ImageFormat::Webp] {
        let drawn = encode(format);
        assert!(!drawn.is_empty(), "{format:?} produced bytes");
        assert_ne!(
            drawn,
            blank(format),
            "{format:?} carries the drawing, not a blank page"
        );
    }
}

#[test]
fn encoded_output_carries_the_format_signature() {
    let encode = |format: ImageFormat| {
        let mut canvas = Canvas::new(10.0, 10.0);
        canvas.context().fill_rect(0.0, 0.0, 10.0, 10.0);
        canvas
            .to_buffer(format, &EncodeOptions::default())
            .expect("export")
    };

    assert!(
        encode(ImageFormat::Png).starts_with(b"\x89PNG"),
        "png magic"
    );
    assert!(
        encode(ImageFormat::Jpeg).starts_with(b"\xff\xd8\xff"),
        "jpeg"
    );
    assert!(encode(ImageFormat::Pdf).starts_with(b"%PDF"), "pdf");
    assert!(
        encode(ImageFormat::Webp).windows(4).any(|w| w == b"WEBP"),
        "webp"
    );

    let svg = encode(ImageFormat::Svg);
    assert!(String::from_utf8_lossy(&svg).contains("<svg"), "svg markup");
}

#[test]
fn a_vector_export_keeps_text_as_text_unless_outlined() {
    let export = |outline: bool| {
        let mut canvas = Canvas::new(200.0, 60.0);
        {
            let ctx = canvas.context();
            ctx.set_font(&Font::new("Helvetica", 24.0));
            ctx.fill_text("Rust", 10.0, 40.0, None);
        }
        canvas
            .to_buffer(
                ImageFormat::Svg,
                &EncodeOptions {
                    outline,
                    ..EncodeOptions::default()
                },
            )
            .expect("svg export")
    };

    let outlined = String::from_utf8_lossy(&export(true)).into_owned();
    let as_text = String::from_utf8_lossy(&export(false)).into_owned();

    assert!(outlined.contains("<path"), "outlined text becomes geometry");
    assert!(
        as_text.contains("<text") || as_text.contains("Rust"),
        "un-outlined text stays selectable"
    );
    assert_ne!(outlined, as_text, "the option changes the output");
}

#[test]
fn density_scales_the_exported_image() {
    let raw = |density: f32| {
        let mut canvas = Canvas::new(10.0, 10.0);
        canvas.context().fill_rect(0.0, 0.0, 10.0, 10.0);
        canvas
            .to_buffer(
                ImageFormat::Raw,
                &EncodeOptions {
                    density,
                    ..EncodeOptions::default()
                },
            )
            .expect("raw export")
    };

    assert_eq!(raw(1.0).len(), 10 * 10 * 4);
    assert_eq!(raw(2.0).len(), 20 * 20 * 4, "twice the resolution");
}

#[test]
fn a_matte_fills_the_transparent_background() {
    let mut canvas = Canvas::new(10.0, 10.0);
    canvas.context().fill_rect(0.0, 0.0, 4.0, 4.0);

    let raw = canvas
        .to_buffer(
            ImageFormat::Raw,
            &EncodeOptions {
                matte: Some(RgbaLinear::opaque(0.0, 0.0, 1.0)),
                ..EncodeOptions::default()
            },
        )
        .expect("raw export");

    assert_eq!(
        at(&raw, 10, 8, 8),
        [0, 0, 255, 255],
        "matte behind the draw"
    );
}

// -- Coverage for the remaining members --------------------------------------

#[test]
fn curves_bend_away_from_the_straight_chord() {
    let painted_at = |build: &dyn Fn(&mut Context2D)| {
        let mut canvas = Canvas::new(40.0, 40.0);
        {
            let ctx = canvas.context();
            ctx.set_fill_style(red());
            ctx.begin_path();
            ctx.move_to(0.0, 20.0);
            build(ctx);
            ctx.line_to(40.0, 40.0);
            ctx.close_path();
            ctx.fill(FillRule::NonZero);
        }
        at(&pixels(&mut canvas), 40, 20, 8)[3] > 0
    };

    // A straight run leaves the sampled point above the shape; both curve
    // kinds bow upwards far enough to cover it.
    assert!(
        !painted_at(&|ctx| ctx.line_to(40.0, 20.0)),
        "chord stays low"
    );
    assert!(
        painted_at(&|ctx| ctx.bezier_curve_to(10.0, 0.0, 30.0, 0.0, 40.0, 20.0)),
        "a cubic bows upwards"
    );
    assert!(
        painted_at(&|ctx| ctx.quadratic_curve_to(20.0, -6.0, 40.0, 20.0)),
        "a quadratic bows upwards"
    );
}

#[test]
fn arc_to_rounds_the_corner_between_two_lines() {
    let sample = |radius: f32| {
        let mut canvas = Canvas::new(40.0, 40.0);
        {
            let ctx = canvas.context();
            ctx.set_stroke_style(red());
            ctx.set_line_width(2.0);
            ctx.begin_path();
            ctx.move_to(2.0, 20.0);
            ctx.arc_to(20.0, 20.0, 20.0, 2.0, radius)
                .expect("valid radius");
            ctx.stroke();
        }
        at(&pixels(&mut canvas), 40, 19, 19)[3]
    };

    // A zero radius keeps the sharp corner at (20,20); a large one cuts it.
    assert!(sample(0.0) > 0, "the sharp corner reaches the sample point");
    assert_eq!(sample(12.0), 0, "a wide radius rounds it away");
}

#[test]
fn ellipse_is_wider_than_it_is_tall_when_told_to_be() {
    let mut canvas = Canvas::new(40.0, 40.0);
    {
        let ctx = canvas.context();
        ctx.set_fill_style(red());
        ctx.begin_path();
        ctx.ellipse(
            20.0,
            20.0,
            18.0,
            6.0,
            0.0,
            0.0,
            std::f32::consts::TAU,
            false,
        );
        ctx.fill(FillRule::NonZero);
    }

    let buffer = pixels(&mut canvas);
    assert!(at(&buffer, 40, 4, 20)[3] > 0, "wide on the x axis");
    assert_eq!(at(&buffer, 40, 20, 4)[3], 0, "narrow on the y axis");
}

#[test]
fn rotate_scale_and_transform_move_the_drawing() {
    let corner = |apply: &dyn Fn(&mut Context2D)| {
        let mut canvas = Canvas::new(40.0, 40.0);
        {
            let ctx = canvas.context();
            apply(ctx);
            ctx.set_fill_style(red());
            ctx.fill_rect(0.0, 0.0, 10.0, 10.0);
        }
        let buffer = pixels(&mut canvas);
        (0..40)
            .flat_map(|y| (0..40).map(move |x| (x, y)))
            .filter(|&(x, y)| at(&buffer, 40, x, y)[3] > 0)
            .count()
    };

    let plain = corner(&|_| {});
    assert_eq!(plain, 100, "an untransformed 10x10 covers 100 pixels");
    assert!(
        corner(&|ctx| ctx.scale(2.0, 2.0)) > plain,
        "scale enlarges it"
    );
    assert_eq!(
        corner(&|ctx| ctx.transform(Affine::scale(2.0, 2.0))),
        corner(&|ctx| ctx.scale(2.0, 2.0)),
        "transform with a scale matrix matches scale()"
    );

    // Rotating about the origin swings most of the square off-canvas.
    assert!(
        corner(&|ctx| ctx.rotate(std::f32::consts::FRAC_PI_4)) < plain,
        "rotation moves it partly out of view"
    );
}

#[test]
fn shadows_paint_offset_from_the_shape() {
    let mut canvas = Canvas::new(40.0, 40.0);
    {
        let ctx = canvas.context();
        ctx.set_shadow_color(RgbaLinear::opaque(0.0, 0.0, 1.0));
        ctx.set_shadow_offset(10.0, 10.0);
        ctx.set_shadow_blur(0.0);
        ctx.set_fill_style(red());
        ctx.fill_rect(2.0, 2.0, 10.0, 10.0);
    }

    let buffer = pixels(&mut canvas);
    assert_eq!(at(&buffer, 40, 5, 5)[0], 255, "the shape itself");
    assert_eq!(at(&buffer, 40, 16, 16)[2], 255, "the shadow, offset by 10");
}

#[test]
fn shadow_blur_softens_the_edge() {
    let spread = |blur: f32| {
        let mut canvas = Canvas::new(40.0, 40.0);
        {
            let ctx = canvas.context();
            ctx.set_shadow_color(RgbaLinear::opaque(0.0, 0.0, 1.0));
            ctx.set_shadow_offset(6.0, 6.0);
            ctx.set_shadow_blur(blur);
            ctx.set_fill_style(red());
            ctx.fill_rect(4.0, 4.0, 10.0, 10.0);
        }
        let buffer = pixels(&mut canvas);
        (0..40)
            .flat_map(|y| (0..40).map(move |x| (x, y)))
            .filter(|&(x, y)| at(&buffer, 40, x, y)[3] > 0)
            .count()
    };

    assert!(spread(6.0) > spread(0.0), "a blurred shadow covers more");
}

#[test]
fn line_cap_and_join_change_the_stroke_outline() {
    let coverage = |apply: &dyn Fn(&mut Context2D)| {
        let mut canvas = Canvas::new(40.0, 40.0);
        {
            let ctx = canvas.context();
            ctx.set_stroke_style(red());
            ctx.set_line_width(8.0);
            apply(ctx);
            ctx.begin_path();
            ctx.move_to(10.0, 10.0);
            ctx.line_to(30.0, 10.0);
            ctx.line_to(30.0, 30.0);
            ctx.stroke();
        }
        let buffer = pixels(&mut canvas);
        (0..40)
            .flat_map(|y| (0..40).map(move |x| (x, y)))
            .filter(|&(x, y)| at(&buffer, 40, x, y)[3] > 0)
            .count()
    };

    let butt_miter = coverage(&|_| {});
    assert!(
        coverage(&|ctx| ctx.set_line_cap(StrokeCap::Square)) > butt_miter,
        "square caps extend past the endpoints"
    );
    assert!(
        coverage(&|ctx| ctx.set_line_join(StrokeJoin::Bevel)) < butt_miter,
        "a bevel cuts the corner a miter fills"
    );
}

#[test]
fn miter_limit_falls_back_to_bevel_when_the_join_exceeds_it() {
    let coverage = |limit: f32| {
        let mut canvas = Canvas::new(60.0, 60.0);
        {
            let ctx = canvas.context();
            ctx.set_stroke_style(red());
            ctx.set_line_width(10.0);
            ctx.set_line_join(StrokeJoin::Miter);
            ctx.set_miter_limit(limit);
            ctx.begin_path();
            ctx.move_to(10.0, 20.0);
            ctx.line_to(40.0, 20.0);
            ctx.line_to(40.0, 50.0);
            ctx.stroke();
        }
        let buffer = pixels(&mut canvas);
        (0..60)
            .flat_map(|y| (0..60).map(move |x| (x, y)))
            .filter(|&(x, y)| at(&buffer, 60, x, y)[3] > 0)
            .count()
    };

    // A right-angle join needs a miter ratio of 1/sin(45 degrees) = 1.414,
    // so a limit of 1.0 bevels it and a limit of 4.0 does not. A sharper
    // corner would exceed both and bevel either way, proving nothing.
    assert!(
        coverage(4.0) > coverage(1.0),
        "the limit governs whether the corner is filled: {} vs {}",
        coverage(4.0),
        coverage(1.0)
    );
}

#[test]
fn line_dash_offset_shifts_the_pattern() {
    let render = |offset: f32| {
        let mut canvas = Canvas::new(40.0, 10.0);
        {
            let ctx = canvas.context();
            ctx.set_stroke_style(red());
            ctx.set_line_width(6.0);
            ctx.set_line_dash(&[6.0, 6.0]);
            ctx.set_line_dash_offset(offset);
            ctx.begin_path();
            ctx.move_to(0.0, 5.0);
            ctx.line_to(40.0, 5.0);
            ctx.stroke();
        }
        pixels(&mut canvas)
    };

    assert_ne!(render(0.0), render(6.0), "the dashes move along the line");
}

#[test]
fn stroke_rect_outlines_without_filling() {
    let mut canvas = Canvas::new(30.0, 30.0);
    {
        let ctx = canvas.context();
        ctx.set_stroke_style(red());
        ctx.set_line_width(2.0);
        ctx.stroke_rect(5.0, 5.0, 20.0, 20.0);
    }

    let buffer = pixels(&mut canvas);
    assert!(at(&buffer, 30, 5, 5)[3] > 0, "the edge is drawn");
    assert_eq!(at(&buffer, 30, 15, 15)[3], 0, "the middle is not");
}

#[test]
fn stroke_text_outlines_the_glyphs() {
    let ink = |stroked: bool| {
        let mut canvas = Canvas::new(120.0, 50.0);
        {
            let ctx = canvas.context();
            ctx.set_font(&Font::new("Helvetica", 32.0));
            if stroked {
                ctx.set_stroke_style(red());
                ctx.set_line_width(1.0);
                ctx.stroke_text("O", 10.0, 40.0, None);
            } else {
                ctx.set_fill_style(red());
                ctx.fill_text("O", 10.0, 40.0, None);
            }
        }
        let buffer = pixels(&mut canvas);
        (0..50)
            .flat_map(|y| (0..120).map(move |x| (x, y)))
            .filter(|&(x, y)| at(&buffer, 120, x, y)[3] > 0)
            .count()
    };

    let outlined = ink(true);
    assert!(outlined > 0, "stroked text is drawn");
    assert!(outlined < ink(false), "an outline covers less than a fill");
}

#[test]
fn draw_image_places_an_image_at_its_natural_size() {
    let mut canvas = Canvas::new(20.0, 20.0);
    canvas.context().draw_image(&quad_tile(), 4.0, 4.0);

    let buffer = pixels(&mut canvas);
    assert_eq!(at(&buffer, 20, 4, 4)[0], 255, "the top-left texel landed");
    assert_eq!(at(&buffer, 20, 0, 0)[3], 0, "nothing before the origin");
    assert_eq!(at(&buffer, 20, 8, 8)[3], 0, "and nothing past 2x2");
}

#[test]
fn draw_image_region_crops_the_source() {
    let mut canvas = Canvas::new(20.0, 20.0);
    // Bottom-right texel of the tile (white) blown up to fill the canvas.
    canvas.context().draw_image_region(
        &quad_tile(),
        1.0,
        1.0,
        1.0,
        1.0,
        0.0,
        0.0,
        20.0,
        20.0,
    );

    assert_eq!(
        at(&pixels(&mut canvas), 20, 10, 10),
        [255, 255, 255, 255],
        "only the cropped texel"
    );
}

#[test]
fn stroke_styles_accept_a_shader_and_a_pattern() {
    let varied = |apply: &dyn Fn(&mut Context2D)| {
        let mut canvas = Canvas::new(20.0, 20.0);
        {
            let ctx = canvas.context();
            apply(ctx);
            ctx.set_line_width(6.0);
            ctx.begin_path();
            ctx.move_to(0.0, 10.0);
            ctx.line_to(20.0, 10.0);
            ctx.stroke();
        }
        let buffer = pixels(&mut canvas);
        at(&buffer, 20, 2, 10) != at(&buffer, 20, 17, 10)
    };

    let gradient = Shader::linear_gradient(
        Point::new(0.0, 0.0),
        Point::new(20.0, 0.0),
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
        GradientInterpolation::default(),
    )
    .expect("gradient");

    assert!(
        varied(&|ctx| ctx.set_stroke_shader(&gradient)),
        "a gradient stroke varies along the line"
    );

    let mut canvas = Canvas::new(20.0, 20.0);
    {
        let ctx = canvas.context();
        let pattern = ctx.create_pattern(&quad_tile(), PatternRepeat::Repeat);
        ctx.set_stroke_pattern(&pattern);
        ctx.set_line_width(6.0);
        ctx.begin_path();
        ctx.move_to(0.0, 10.0);
        ctx.line_to(20.0, 10.0);
        ctx.stroke();
    }
    let buffer = pixels(&mut canvas);
    assert_ne!(
        at(&buffer, 20, 2, 10),
        at(&buffer, 20, 3, 10),
        "a patterned stroke alternates texels"
    );
}

#[test]
fn color_and_image_filters_change_the_drawing() {
    let render = |apply: &dyn Fn(&mut Context2D)| {
        let mut canvas = Canvas::new(20.0, 20.0);
        {
            let ctx = canvas.context();
            apply(ctx);
            ctx.set_fill_style(red());
            ctx.fill_rect(5.0, 5.0, 10.0, 10.0);
        }
        pixels(&mut canvas)
    };

    let plain = render(&|_| {});
    let luma = ColorFilter::luma();
    let blur = ImageFilter::blur(3.0, 3.0, None).expect("blur filter");

    assert_ne!(
        render(&|ctx| ctx.set_color_filter(Some(&luma))),
        plain,
        "a color filter reaches the paint"
    );
    assert_ne!(
        render(&|ctx| ctx.set_image_filter(Some(&blur))),
        plain,
        "an image filter reaches the paint"
    );
    assert_eq!(
        render(&|ctx| {
            ctx.set_color_filter(Some(&luma));
            ctx.set_color_filter(None);
        }),
        plain,
        "None removes it again"
    );
}

#[test]
fn word_spacing_widens_the_gaps_between_words() {
    let mut canvas = Canvas::new(10.0, 10.0);
    let ctx = canvas.context();
    ctx.set_font(&Font::new("Helvetica", 20.0));

    let tight = ctx.measure_text("a b c", None).width;
    ctx.set_word_spacing(10.0);
    let loose = ctx.measure_text("a b c", None).width;

    assert!(
        loose > tight,
        "word spacing widens the run: {loose} vs {tight}"
    );
}

#[test]
fn is_point_in_stroke_follows_the_current_stroke_width() {
    let mut canvas = Canvas::new(20.0, 20.0);
    let ctx = canvas.context();
    ctx.begin_path();
    ctx.move_to(0.0, 10.0);
    ctx.line_to(20.0, 10.0);

    ctx.set_line_width(2.0);
    assert!(ctx.is_point_in_stroke(10.0, 10.0), "on the line");
    assert!(!ctx.is_point_in_stroke(10.0, 5.0), "outside a thin stroke");

    ctx.set_line_width(16.0);
    assert!(ctx.is_point_in_stroke(10.0, 5.0), "inside a thick one");
}

#[test]
fn a_context_is_never_lost() {
    let mut canvas = Canvas::new(10.0, 10.0);
    assert!(
        !canvas.context().is_context_lost(),
        "there is no compositor to lose the surface to"
    );
}

#[test]
fn set_gpu_keeps_rendering_correct() {
    let render = |gpu: bool| {
        let mut canvas = Canvas::new(20.0, 20.0);
        canvas.set_gpu(gpu);
        {
            let ctx = canvas.context();
            ctx.set_fill_style(red());
            ctx.fill_rect(0.0, 0.0, 20.0, 20.0);
        }
        at(&pixels(&mut canvas), 20, 10, 10)
    };

    assert_eq!(render(true), [255, 0, 0, 255], "gpu path");
    assert_eq!(render(false), [255, 0, 0, 255], "cpu path");
}

#[test]
fn create_image_data_as_honours_the_layout() {
    let mut canvas = Canvas::new(10.0, 10.0);
    let data = canvas
        .context()
        .create_image_data_as(
            2,
            2,
            PixelExportOptions {
                depth: PixelDepth::F32,
                premultiplied: true,
                ..PixelExportOptions::default()
            },
        )
        .expect("allocate");

    assert_eq!(data.stride(), 2 * 16, "F32 is 16 bytes per pixel");
    assert!(data.premultiplied());
}

#[test]
fn font_builder_selects_an_italic_face() {
    let mut canvas = Canvas::new(10.0, 10.0);
    let ctx = canvas.context();

    ctx.set_font(&Font::new("Times", 32.0));
    let upright = ctx.measure_text("italic", None).width;
    ctx.set_font(&Font::new("Times", 32.0).italic());
    let slanted = ctx.measure_text("italic", None).width;

    assert_ne!(upright, slanted, "a different face was selected");
}

#[test]
fn image_format_describes_itself() {
    assert!(ImageFormat::Pdf.is_vector() && ImageFormat::Svg.is_vector());
    assert!(!ImageFormat::Png.is_vector() && !ImageFormat::Raw.is_vector());

    assert_eq!(ImageFormat::Png.mime_type(), "image/png");
    assert_eq!(ImageFormat::Jpeg.mime_type(), "image/jpeg");
    assert_eq!(ImageFormat::Raw.mime_type(), "application/octet-stream");

    assert_eq!(ImageFormat::Jpeg.extension(), "jpg");
    assert_eq!(ImageFormat::Svg.extension(), "svg");

    assert_eq!(ImageFormat::from_extension("PNG"), Some(ImageFormat::Png));
    assert_eq!(
        ImageFormat::from_extension(".jpeg"),
        Some(ImageFormat::Jpeg)
    );
    assert_eq!(ImageFormat::from_extension("jpg"), Some(ImageFormat::Jpeg));
    assert_eq!(ImageFormat::from_extension("bin"), None, "raw has no name");
    assert_eq!(ImageFormat::from_extension("tiff"), None);
}

#[test]
fn texture_reports_its_spacing() {
    let hatch = Texture::new(&TextureOptions {
        spacing: (6.0, 9.0),
        ..TextureOptions::default()
    });

    assert_eq!(hatch.spacing(), (6.0, 9.0));
}

#[test]
fn to_file_writes_the_format_the_extension_names() {
    let dir = std::env::temp_dir().join("meo-skia-canvas-to-file");
    std::fs::create_dir_all(&dir).expect("temp dir");

    for (name, magic) in [
        ("out.png", &b"\x89PNG"[..]),
        ("out.jpg", &b"\xff\xd8\xff"[..]),
        ("out.pdf", &b"%PDF"[..]),
    ] {
        let path = dir.join(name);
        let mut canvas = Canvas::new(10.0, 10.0);
        canvas.context().fill_rect(0.0, 0.0, 10.0, 10.0);
        canvas
            .to_file(&path, &EncodeOptions::default())
            .expect("write");

        let bytes = std::fs::read(&path).expect("read back");
        assert!(bytes.starts_with(magic), "{name} has the right magic");
        std::fs::remove_file(&path).ok();
    }
    std::fs::remove_dir(&dir).ok();
}

#[test]
fn to_file_reports_a_path_it_cannot_write() {
    let mut canvas = Canvas::new(10.0, 10.0);
    let result = canvas
        .to_file("/nonexistent-dir-xyz/out.png", &EncodeOptions::default());

    assert!(
        matches!(result, Err(Error::Encode { .. })),
        "an unwritable path is reported, not ignored"
    );
}

#[test]
fn to_file_refuses_a_path_with_no_extension() {
    let mut canvas = Canvas::new(10.0, 10.0);

    assert!(
        matches!(
            canvas.to_file("/tmp/no-extension", &EncodeOptions::default()),
            Err(Error::Encode { .. })
        ),
        "a missing extension is an error rather than a silent PNG"
    );
}

#[test]
fn encoding_a_zero_sized_canvas_is_an_error() {
    let mut canvas = Canvas::new(0.0, 10.0);

    assert!(
        matches!(
            canvas.to_buffer(ImageFormat::Png, &EncodeOptions::default()),
            Err(Error::Encode { .. })
        ),
        "a page with no area cannot be encoded"
    );
}

#[test]
fn a_readback_past_the_page_edge_reads_back_transparent() {
    let mut canvas = Canvas::new(10.0, 10.0);
    let ctx = canvas.context();
    ctx.set_fill_style(red());
    ctx.fill_rect(0.0, 0.0, 10.0, 10.0);

    // The Canvas API allows asking for more than the page holds; the excess
    // is transparent rather than an error or a clamp.
    let data = ctx
        .get_image_data(-5.0, -5.0, 30.0, 30.0)
        .expect("readback");
    assert_eq!((data.width(), data.height()), (30, 30));
    assert_eq!(
        at(data.pixels(), 30, 1, 1)[3],
        0,
        "above and left of the page"
    );
    assert_eq!(at(data.pixels(), 30, 10, 10)[0], 255, "the page itself");

    let outside = ctx
        .get_image_data(100.0, 100.0, 4.0, 4.0)
        .expect("readback");
    assert!(outside.pixels().iter().all(|&b| b == 0), "wholly outside");
}

#[test]
fn a_readback_too_large_to_address_is_an_error_not_a_panic() {
    // Skia addresses pixel buffers with a signed 32-bit byte count, so this
    // used to abort with `capacity overflow` -- reachable from JavaScript
    // too, as `getImageData(0, 0, 100000, 100000)`.
    let mut canvas = Canvas::new(50.0, 50.0);
    let ctx = canvas.context();

    for n in [30000.0, 100000.0, 1e9] {
        assert!(
            ctx.get_image_data(0.0, 0.0, n, n).is_err(),
            "{n}x{n} must report an error rather than abort"
        );
    }

    // Just under the limit still works, so the guard is not over-eager.
    assert!(
        ctx.get_image_data(0.0, 0.0, 20000.0, 20000.0).is_ok(),
        "a large but addressable readback still succeeds"
    );
}

#[test]
fn a_non_finite_readback_rect_is_rejected() {
    let mut canvas = Canvas::new(50.0, 50.0);
    let ctx = canvas.context();

    for (x, y, w, h) in [
        (f32::NAN, 0.0, 4.0, 4.0),
        (0.0, f32::NAN, 4.0, 4.0),
        (0.0, 0.0, f32::INFINITY, 4.0),
        (0.0, 0.0, 4.0, f32::NEG_INFINITY),
        (
            f32::NEG_INFINITY,
            f32::NEG_INFINITY,
            f32::INFINITY,
            f32::INFINITY,
        ),
    ] {
        assert!(
            matches!(
                ctx.get_image_data(x, y, w, h),
                Err(Error::InvalidDimensions { .. })
            ),
            "({x}, {y}, {w}, {h}) must be rejected"
        );
    }
}

#[test]
fn font_parse_reports_what_it_could_not_read() {
    for bad in [
        "Helvetica",
        "20 Helvetica",
        "italic 700 44px",
        "44px ",
        "notaweight 44px Helvetica",
    ] {
        assert!(
            matches!(Font::parse(bad), Err(Error::FontRegister { .. })),
            "{bad:?} should be rejected"
        );
    }

    let font = Font::parse("italic 700 44px Helvetica, Arial").expect("parses");
    assert_eq!(font.size, 44.0);
    assert_eq!(font.weight, 700);
    assert!(font.italic);
    assert_eq!(font.families, vec!["Helvetica", "Arial"]);
}

// -- Documented behaviour ----------------------------------------------------
//
// Each of these pins a claim a doc comment makes. All three were written
// backwards at one point and went unnoticed because nothing asserted them.

#[test]
fn path_bounds_measure_the_curve_not_its_control_points() {
    // The control points sit at y=100; the curve itself only reaches 75.
    let curve = Path::from_svg("M0 0 C0 100 40 100 40 0", FillRule::NonZero)
        .expect("path");
    let bounds = curve.bounds();

    assert_eq!(bounds.left, 0.0);
    assert_eq!(bounds.right, 40.0);
    assert!(
        bounds.bottom < 100.0,
        "control points are excluded, got {}",
        bounds.bottom
    );
    assert!(bounds.bottom > 50.0, "but the curve's own extent is kept");
}

#[test]
fn path_bounds_exclude_stroke_width() {
    let line = Path::from_svg("M0 0 L10 0", FillRule::NonZero).expect("path");
    let bounds = line.bounds();

    assert_eq!(bounds.height(), 0.0, "geometry only, not painted coverage");
    assert_eq!(bounds.width(), 10.0);
}

#[test]
fn filter_identity_values_differ_by_group() {
    let sample = |ops: &[FilterOp]| {
        let mut canvas = Canvas::new(4.0, 4.0);
        {
            let ctx = canvas.context();
            ctx.set_filter(ops).expect("valid filter");
            ctx.set_fill_style(RgbaLinear::opaque(0.8, 0.3, 0.1));
            ctx.fill_rect(0.0, 0.0, 4.0, 4.0);
        }
        at(&pixels(&mut canvas), 4, 1, 1)
    };
    let plain = sample(&[]);

    // Scaling filters: 1.0 is the identity.
    for op in [
        FilterOp::Brightness(1.0),
        FilterOp::Contrast(1.0),
        FilterOp::Opacity(1.0),
        FilterOp::Saturate(1.0),
    ] {
        assert_eq!(sample(&[op]), plain, "{op:?} at 1.0 is the identity");
    }

    // Degree filters: 0.0 is the identity, and 1.0 is not.
    for op in [
        FilterOp::Blur(0.0),
        FilterOp::Grayscale(0.0),
        FilterOp::HueRotate(0.0),
        FilterOp::Invert(0.0),
        FilterOp::Sepia(0.0),
    ] {
        assert_eq!(sample(&[op]), plain, "{op:?} at 0.0 is the identity");
    }
    for op in [
        FilterOp::Grayscale(1.0),
        FilterOp::Invert(1.0),
        FilterOp::Sepia(1.0),
    ] {
        assert_ne!(sample(&[op]), plain, "{op:?} at 1.0 is not the identity");
    }
}

#[test]
fn a_dash_marker_needs_a_dash_list_to_repeat_along() {
    let painted = |dashes: &[f32], marker: bool| {
        let square =
            Path::from_svg("M-3 -3 L3 -3 L3 3 L-3 3 Z", FillRule::NonZero)
                .expect("marker path");
        let mut canvas = Canvas::new(40.0, 20.0);
        {
            let ctx = canvas.context();
            ctx.set_stroke_style(red());
            ctx.set_line_width(2.0);
            ctx.set_line_dash(dashes);
            if marker {
                ctx.set_line_dash_marker(Some(&square));
            }
            ctx.begin_path();
            ctx.move_to(0.0, 10.0);
            ctx.line_to(40.0, 10.0);
            ctx.stroke();
        }
        pixels(&mut canvas)
            .chunks(4)
            .filter(|texel| texel[3] > 0)
            .count()
    };

    // With no dash list there is no period, so the marker is ignored and the
    // stroke draws solid -- not, as the doc once claimed, nothing at all.
    assert_eq!(
        painted(&[], true),
        painted(&[], false),
        "an empty dash list ignores the marker and strokes solid"
    );
    assert!(painted(&[], true) > 0, "and it does draw");
    assert!(
        painted(&[6.0, 6.0], true) > painted(&[], true),
        "with a dash list the marker is stamped along it"
    );
}

// -- Spec conformance --------------------------------------------------------

#[test]
fn an_out_of_range_global_alpha_is_ignored_not_clamped() {
    let sample = |value: f32| {
        let mut canvas = Canvas::new(10.0, 10.0);
        {
            let ctx = canvas.context();
            ctx.set_global_alpha(0.5);
            ctx.set_global_alpha(value);
            ctx.set_fill_style(red());
            ctx.fill_rect(0.0, 0.0, 10.0, 10.0);
        }
        at(&pixels(&mut canvas), 10, 5, 5)[3]
    };

    let half = sample(0.5);
    // The Canvas standard says an out-of-range assignment is ignored, so the
    // earlier 0.5 must stand. Clamping would give 255 for 1.5 and 0 for -1.
    assert_eq!(sample(1.5), half, "1.5 is ignored, not clamped to opaque");
    assert_eq!(sample(-1.0), half, "-1.0 is ignored, not clamped to zero");
    assert_eq!(sample(f32::NAN), half, "NaN is ignored");
    assert_ne!(sample(1.0), half, "an in-range value does apply");
}

#[test]
fn round_rect_rejects_a_negative_radius() {
    let mut canvas = Canvas::new(40.0, 40.0);
    let ctx = canvas.context();
    ctx.begin_path();

    // Skia clamps a negative radius to zero and draws a square corner. The
    // Canvas API throws instead, and drawing the wrong shape in silence is
    // the worse failure.
    assert!(matches!(
        ctx.round_rect(5.0, 5.0, 30.0, 30.0, [-10.0, 0.0, 0.0, 0.0]),
        Err(Error::InvalidRect { .. })
    ));
    assert!(matches!(
        ctx.round_rect(5.0, 5.0, 30.0, 30.0, [f32::NAN, 0.0, 0.0, 0.0]),
        Err(Error::InvalidRect { .. })
    ));
    assert!(ctx.round_rect(5.0, 5.0, 30.0, 30.0, [4.0; 4]).is_ok());
}

#[test]
fn round_rect_accepts_elliptical_corners() {
    let render = |apply: &dyn Fn(&mut Context2D)| {
        let mut canvas = Canvas::new(40.0, 40.0);
        {
            let ctx = canvas.context();
            ctx.set_fill_style(red());
            ctx.begin_path();
            apply(ctx);
            ctx.fill(FillRule::NonZero);
        }
        pixels(&mut canvas)
    };

    let circular = render(&|ctx| {
        ctx.round_rect(5.0, 5.0, 30.0, 30.0, [12.0; 4])
            .expect("radii");
    });
    let elliptical = render(&|ctx| {
        ctx.round_rect_elliptical(5.0, 5.0, 30.0, 30.0, [(12.0, 4.0); 4])
            .expect("radii");
    });

    assert_ne!(
        circular, elliptical,
        "a corner with different x and y radii is not the circular case"
    );
}

#[test]
fn get_projection_keeps_the_row_get_transform_drops() {
    let mut canvas = Canvas::new(40.0, 40.0);
    let ctx = canvas.context();

    let quad = [
        Point { x: 0.0, y: 0.0 },
        Point { x: 40.0, y: 6.0 },
        Point { x: 40.0, y: 34.0 },
        Point { x: 0.0, y: 40.0 },
    ];
    let projection = ctx.create_projection(quad, None).expect("projection");
    ctx.set_projection(&projection);

    let read_back = ctx.get_projection();
    assert_eq!(
        read_back.values, projection.values,
        "the full 3x3 survives the round trip"
    );

    // The affine reader cannot carry the projective row, which is why it
    // must not be the only way to read the transform back.
    let affine = ctx.get_transform();
    let perspective_row = &projection.values[6..9];
    assert!(
        perspective_row[0] != 0.0 || perspective_row[1] != 0.0,
        "the test projection really is projective: {perspective_row:?}"
    );
    assert_eq!(affine.a, projection.values[0], "the affine part matches");
}

#[test]
fn put_image_data_reports_a_layout_skia_refuses() {
    let mut canvas = Canvas::new(20.0, 20.0);
    let ctx = canvas.context();

    // A well-formed buffer still writes.
    let patch = ctx.create_image_data(2, 2).expect("allocate");
    assert!(ctx.put_image_data(&patch, 0.0, 0.0).is_ok());
}

// -- sRGB colour entry -------------------------------------------------------

#[test]
fn srgb_constructors_round_trip_through_a_draw() {
    // The trap this closes: `opaque(0.5, 0.5, 0.5)` is linear-light and reads
    // back as byte 188, so JavaScript muscle memory produced a visibly
    // different grey that still compiled and rendered.
    let drawn = |color: RgbaLinear| {
        let mut canvas = Canvas::new(4.0, 4.0);
        {
            let ctx = canvas.context();
            ctx.set_fill_style(color);
            ctx.fill_rect(0.0, 0.0, 4.0, 4.0);
        }
        at(&pixels(&mut canvas), 4, 1, 1)
    };

    assert_eq!(
        drawn(RgbaLinear::from_srgb8(0x80, 0x80, 0x80, 1.0)),
        [128, 128, 128, 255],
        "#808080 reads back as 0x80"
    );
    assert_eq!(
        drawn(RgbaLinear::opaque(0.5, 0.5, 0.5))[0],
        188,
        "linear 0.5 is the lighter grey people hit by accident"
    );
    assert_eq!(
        drawn(RgbaLinear::from_srgb8(200, 60, 130, 1.0)),
        [200, 60, 130, 255],
        "every channel survives"
    );
}

#[test]
fn srgb_alpha_premultiplies_the_way_css_means_it() {
    let mut canvas = Canvas::new(4.0, 4.0);
    {
        let ctx = canvas.context();
        // rgba(255, 0, 0, 0.5)
        ctx.set_fill_style(RgbaLinear::from_srgb8(255, 0, 0, 0.5));
        ctx.fill_rect(0.0, 0.0, 4.0, 4.0);
    }

    let px = at(&pixels(&mut canvas), 4, 1, 1);
    assert_eq!(px[0], 255, "unpremultiplied readback keeps full red");
    assert!((120..=136).contains(&px[3]), "at half alpha, got {}", px[3]);
}

#[test]
fn hex_colors_parse_in_every_css_length() {
    let red = RgbaLinear::from_srgb8(255, 0, 0, 1.0);

    assert_eq!(RgbaLinear::from_hex("#f00").expect("short"), red);
    assert_eq!(RgbaLinear::from_hex("#ff0000").expect("long"), red);
    assert_eq!(RgbaLinear::from_hex("ff0000").expect("no hash"), red);
    assert_eq!(RgbaLinear::from_hex("#FF0000").expect("uppercase"), red);

    let half = RgbaLinear::from_hex("#ff000080").expect("with alpha");
    assert!((0.49..=0.51).contains(&half.a), "alpha {}", half.a);
    // Shorthand doubles each digit, so #f008 is #ff000088 -- not #ff000080.
    assert_eq!(
        RgbaLinear::from_hex("#f008").expect("short with alpha"),
        RgbaLinear::from_hex("#ff000088").expect("doubled"),
    );
}

#[test]
fn a_bad_hex_color_is_rejected() {
    for bad in ["#not", "#12345", "", "#", "#1234567", "zzz"] {
        assert!(
            matches!(
                RgbaLinear::from_hex(bad),
                Err(Error::InvalidColor { .. })
            ),
            "{bad:?} should be rejected"
        );
    }
}

// -- Reaching every page -----------------------------------------------------

#[test]
fn an_earlier_page_stays_reachable() {
    let mut canvas = Canvas::new(10.0, 10.0);
    canvas.new_page();
    canvas.new_page();

    // Without `page`, `context()` only ever reaches the newest, so a page
    // became unreachable the moment another was added.
    canvas
        .page(0)
        .expect("page 0")
        .fill_rect(0.0, 0.0, 10.0, 10.0);
    assert!(canvas.page(2).is_some(), "the newest is addressable too");
    assert!(canvas.page(3).is_none(), "past the end is None");
    assert_eq!(canvas.page_count(), 3);
}

#[test]
fn export_can_select_which_page_it_encodes() {
    let mut canvas = Canvas::new(10.0, 10.0);
    {
        let ctx = canvas.context();
        ctx.set_fill_style(red());
        ctx.fill_rect(0.0, 0.0, 10.0, 10.0);
    }
    {
        let ctx = canvas.new_page();
        ctx.set_fill_style(RgbaLinear::opaque(0.0, 0.0, 1.0));
        ctx.fill_rect(0.0, 0.0, 10.0, 10.0);
    }

    let first = canvas
        .to_buffer(
            ImageFormat::Raw,
            &EncodeOptions {
                page: Some(0),
                ..EncodeOptions::default()
            },
        )
        .expect("page 0");
    assert_eq!(at(&first, 10, 5, 5)[0], 255, "page 0 is the red one");

    let current = canvas
        .to_buffer(ImageFormat::Raw, &EncodeOptions::default())
        .expect("current page");
    assert_eq!(at(&current, 10, 5, 5)[2], 255, "the default is the newest");
}

#[test]
fn selecting_a_page_past_the_end_is_an_error() {
    let mut canvas = Canvas::new(10.0, 10.0);

    assert!(matches!(
        canvas.to_buffer(
            ImageFormat::Raw,
            &EncodeOptions {
                page: Some(7),
                ..EncodeOptions::default()
            }
        ),
        Err(Error::Encode { .. })
    ));
}

#[test]
fn set_size_resizes_and_clears_the_current_page() {
    let mut canvas = Canvas::new(20.0, 20.0);
    {
        let ctx = canvas.context();
        ctx.set_fill_style(red());
        ctx.fill_rect(0.0, 0.0, 20.0, 20.0);
    }

    canvas.set_size(8.0, 6.0);
    assert_eq!((canvas.width(), canvas.height()), (8.0, 6.0));

    // Assigning canvas.width in HTML discards the drawing; so does this.
    let raw = canvas
        .to_buffer(ImageFormat::Raw, &EncodeOptions::default())
        .expect("raw export");
    assert_eq!(raw.len(), 8 * 6 * 4, "the page is the new size");
    assert!(
        raw.chunks(4).all(|texel| texel[3] == 0),
        "and it is cleared"
    );
}

#[test]
fn font_parse_strips_quotes_from_family_names() {
    // A family kept with its quotes matches no installed face and falls
    // through to the next one in silence -- the exact failure this
    // constructor is meant to surface.
    let font = Font::parse("bold 16px \"Helvetica Neue\", sans-serif")
        .expect("parses");
    assert_eq!(font.families, vec!["Helvetica Neue", "sans-serif"]);
    assert_eq!(font.weight, 700);

    let single = Font::parse("16px 'Comic Sans MS'").expect("parses");
    assert_eq!(single.families, vec!["Comic Sans MS"]);
}

#[test]
fn font_parse_rejects_a_weight_outside_the_css_range() {
    for bad in ["12000 44px Helvetica", "0 44px Helvetica"] {
        assert!(
            matches!(Font::parse(bad), Err(Error::FontRegister { .. })),
            "{bad:?} should be rejected"
        );
    }
    assert_eq!(
        Font::parse("1000 44px Helvetica").expect("parses").weight,
        1000,
        "the top of the range is allowed"
    );
}

#[test]
fn font_weight_builder_clamps_into_range() {
    assert_eq!(Font::new("Helvetica", 12.0).weight(12000).weight, 1000);
    assert_eq!(Font::new("Helvetica", 12.0).weight(0).weight, 1);
    assert_eq!(Font::new("Helvetica", 12.0).weight(700).weight, 700);
}

#[test]
fn font_parse_does_not_care_about_token_order() {
    let a = Font::parse("italic 700 44px Helvetica").expect("parses");
    let b = Font::parse("700 italic 44px Helvetica").expect("parses");
    assert_eq!(a, b, "order among the leading tokens is not significant");
}

#[test]
fn arc_to_reports_a_negative_radius() {
    let mut canvas = Canvas::new(20.0, 20.0);
    let ctx = canvas.context();
    ctx.begin_path();
    ctx.move_to(2.0, 10.0);

    assert!(matches!(
        ctx.arc_to(10.0, 10.0, 10.0, 2.0, -4.0),
        Err(Error::InvalidRect { .. })
    ));
    assert!(matches!(
        ctx.arc_to(10.0, 10.0, 10.0, 2.0, f32::NAN),
        Err(Error::InvalidRect { .. })
    ));
    assert!(ctx.arc_to(10.0, 10.0, 10.0, 2.0, 4.0).is_ok());
}

#[test]
fn font_carries_its_own_stretch_so_set_font_cannot_undo_it() {
    let width = |font: &Font| {
        let mut canvas = Canvas::new(10.0, 10.0);
        let ctx = canvas.context();
        ctx.set_font(font);
        ctx.measure_text("wwwwwwww", None).width
    };

    // Setting stretch *before* set_font used to be silently undone, because
    // the CSS shorthand resets that axis. Carrying it on Font is the fix.
    let normal = width(&Font::new("Futura", 40.0));
    let condensed =
        width(&Font::new("Futura", 40.0).stretch(FontStretch::Condensed));

    assert!(
        condensed < normal,
        "the stretch survived set_font: {condensed} vs {normal}"
    );
}

#[test]
fn font_line_height_reaches_wrapped_layout() {
    let height = |font: &Font| {
        let mut canvas = Canvas::new(200.0, 200.0);
        let ctx = canvas.context();
        ctx.set_text_wrap(true);
        ctx.set_font(font);
        ctx.measure_text("one two three four five six seven", Some(60.0))
            .height
    };

    let natural = height(&Font::new("Helvetica", 16.0));
    let loose = height(&Font::new("Helvetica", 16.0).line_height(40.0));

    assert!(loose > natural, "line height applies: {loose} vs {natural}");
}

// -- Reading the graphics state ----------------------------------------------

#[test]
fn every_scalar_setter_has_a_reader_that_agrees_with_it() {
    let mut canvas = Canvas::new(20.0, 20.0);
    let ctx = canvas.context();

    ctx.set_global_alpha(0.25);
    assert_eq!(ctx.global_alpha(), 0.25);

    ctx.set_global_composite_operation(BlendMode::Multiply);
    assert_eq!(ctx.global_composite_operation(), BlendMode::Multiply);

    ctx.set_line_width(7.5);
    assert_eq!(ctx.line_width(), 7.5);

    ctx.set_line_cap(StrokeCap::Round);
    assert_eq!(ctx.line_cap(), StrokeCap::Round);

    ctx.set_line_join(StrokeJoin::Bevel);
    assert_eq!(ctx.line_join(), StrokeJoin::Bevel);

    ctx.set_miter_limit(3.5);
    assert_eq!(ctx.miter_limit(), 3.5);

    ctx.set_line_dash_offset(4.0);
    assert_eq!(ctx.line_dash_offset(), 4.0);

    ctx.set_shadow_blur(6.0);
    assert_eq!(ctx.shadow_blur(), 6.0);

    ctx.set_shadow_offset(3.0, -2.0);
    assert_eq!(ctx.shadow_offset(), (3.0, -2.0));

    ctx.set_image_smoothing_enabled(false);
    assert!(!ctx.image_smoothing_enabled());

    ctx.set_image_smoothing_quality(SmoothingQuality::High);
    assert_eq!(ctx.image_smoothing_quality(), SmoothingQuality::High);

    ctx.set_dither(true);
    assert!(ctx.dither());

    ctx.set_font_hinting(true);
    assert!(ctx.font_hinting());

    ctx.set_text_wrap(true);
    assert!(ctx.text_wrap());

    ctx.set_text_align(TextAlign::End);
    assert_eq!(ctx.text_align(), TextAlign::End);

    ctx.set_text_baseline(TextBaseline::Hanging);
    assert_eq!(ctx.text_baseline(), TextBaseline::Hanging);

    ctx.set_font_stretch(FontStretch::Condensed);
    assert_eq!(ctx.font_stretch(), FontStretch::Condensed);
}

#[test]
fn shadow_color_reads_back_the_colour_it_was_given() {
    let mut canvas = Canvas::new(10.0, 10.0);
    let ctx = canvas.context();

    let blue = RgbaLinear::from_srgb8(0, 0, 255, 1.0);
    ctx.set_shadow_color(blue);

    let read = ctx.shadow_color();
    assert!((read.b - blue.b).abs() < 0.01, "{read:?} vs {blue:?}");
    assert!((read.a - 1.0).abs() < 0.01);
}

#[test]
fn spacing_readers_report_pixels() {
    let mut canvas = Canvas::new(10.0, 10.0);
    let ctx = canvas.context();
    ctx.set_font(&Font::new("Helvetica", 20.0));

    ctx.set_letter_spacing(3.0);
    ctx.set_word_spacing(5.0);

    assert_eq!(ctx.letter_spacing(), 3.0);
    assert_eq!(ctx.word_spacing(), 5.0);
}

#[test]
fn state_readers_follow_save_and_restore() {
    // This is what the readers are for: the narrow save-modify-restore idiom
    // that `save()`/`restore()` can only do all-or-nothing.
    let mut canvas = Canvas::new(10.0, 10.0);
    let ctx = canvas.context();

    ctx.set_line_width(2.0);
    let previous = ctx.line_width();

    ctx.set_line_width(9.0);
    assert_eq!(ctx.line_width(), 9.0);

    ctx.set_line_width(previous);
    assert_eq!(ctx.line_width(), 2.0);

    // And they track the state stack.
    ctx.save();
    ctx.set_line_width(11.0);
    assert_eq!(ctx.line_width(), 11.0);
    ctx.restore();
    assert_eq!(ctx.line_width(), 2.0);
}

#[test]
fn font_readers_report_what_was_set() {
    let mut canvas = Canvas::new(10.0, 10.0);
    let ctx = canvas.context();

    ctx.set_font(&Font::new("Helvetica", 22.0));
    assert!(ctx.font().contains("22"), "got {}", ctx.font());

    ctx.set_font_variant(
        FontVariantCaps::SmallCaps,
        &[FontFeature::on("onum")],
    );
    assert!(
        ctx.font_variant().contains("small-caps"),
        "{}",
        ctx.font_variant()
    );

    assert_eq!(ctx.font_variation_settings(), "normal");
    ctx.set_font_variation_settings(&[FontVariation::new(
        FontAxisTag::WGHT,
        537.0,
    )]);
    assert!(
        ctx.font_variation_settings().contains("537"),
        "{}",
        ctx.font_variation_settings()
    );
}
