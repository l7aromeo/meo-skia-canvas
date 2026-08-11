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

/// Runs of painted/unpainted pixels along the middle row of a 120x20 canvas
/// stroked with `segments`, after `[6.0, 2.0]` was already in place. The
/// prior pattern is what makes a rejected call visible: the standard leaves
/// it standing, so an ignored call keeps drawing `6 on, 2 off`.
fn dash_runs(segments: &[f32]) -> (Vec<f32>, Vec<(bool, u32)>) {
    let mut canvas = Canvas::new(120.0, 20.0);
    let stored;
    {
        let ctx = canvas.context();
        ctx.set_stroke_style(red());
        ctx.set_line_width(6.0);
        ctx.set_line_dash(&[6.0, 2.0]);
        ctx.set_line_dash(segments);
        stored = ctx.get_line_dash();
        ctx.begin_path();
        ctx.move_to(0.0, 10.0);
        ctx.line_to(120.0, 10.0);
        ctx.stroke();
    }

    let buffer = pixels(&mut canvas);
    let mut runs: Vec<(bool, u32)> = vec![];
    for x in 0..120 {
        let on = at(&buffer, 120, x, 10)[3] > 128;
        match runs.last_mut() {
            Some((value, len)) if *value == on => *len += 1,
            _ => runs.push((on, 1)),
        }
    }
    (stored, runs)
}

#[test]
fn an_odd_length_dash_pattern_repeats_instead_of_drawing_solid() {
    // Skia takes no odd-length dash list -- it returns no effect, which the
    // paint then applies as "no dashes at all". Every dash test before this
    // one passed an even-length list, so a pattern that silently drew solid
    // went unnoticed. The standard says to repeat the list once.
    let (stored, runs) = dash_runs(&[6.0, 2.0, 1.0]);
    assert_eq!(
        stored,
        vec![6.0, 2.0, 1.0, 6.0, 2.0, 1.0],
        "the pattern reads back repeated"
    );
    assert_eq!(
        runs.iter().take(6).copied().collect::<Vec<_>>(),
        vec![
            (true, 6),
            (false, 2),
            (true, 1),
            (false, 6),
            (true, 2),
            (false, 1)
        ],
        "six on, two off, one on, six off, two on, one off"
    );

    // Writing the repeat out by hand has to give exactly the same drawing.
    let (_, spelled_out) = dash_runs(&[6.0, 2.0, 1.0, 6.0, 2.0, 1.0]);
    assert_eq!(runs, spelled_out);

    // A single length is the smallest odd case and the easiest to get wrong.
    let (stored, runs) = dash_runs(&[10.0]);
    assert_eq!(stored, vec![10.0, 10.0]);
    assert_eq!(
        runs.iter().take(2).copied().collect::<Vec<_>>(),
        vec![(true, 10), (false, 10)]
    );
}

#[test]
fn an_invalid_dash_pattern_leaves_the_previous_one_standing() {
    // The standard returns without touching the dash list, so the earlier
    // `[6.0, 2.0]` keeps drawing. Assigning the bad list instead cleared the
    // effect and turned the stroke solid, and dropping just the offending
    // entries would have reshaped a pattern the caller never replaced.
    let (_, untouched) = dash_runs(&[6.0, 2.0]);
    for bad in [
        vec![5.0, 3.0, -1.0],
        vec![5.0, -3.0],
        vec![5.0, 3.0, f32::NAN],
        vec![f32::INFINITY, 2.0],
        vec![f32::NEG_INFINITY],
    ] {
        let (stored, runs) = dash_runs(&bad);
        assert_eq!(stored, vec![6.0, 2.0], "{bad:?} left the pattern alone");
        assert_eq!(
            runs, untouched,
            "{bad:?} draws the earlier pattern along the whole line"
        );
    }

    // An empty list is valid, and does clear back to solid.
    let (stored, runs) = dash_runs(&[]);
    assert!(stored.is_empty());
    assert_eq!(runs, vec![(true, 120)], "solid again");
}

/// Midpoint and quarter-point greys of a black-to-white gradient drawn 101
/// pixels wide in `space`. The stops themselves are identical in every
/// space, so only the samples between them can tell the spaces apart -- and
/// only exact values can, since any two distinct spaces differ.
fn gradient_ramp(space: GradientColorSpace) -> (u8, u8) {
    let mut canvas = Canvas::new(101.0, 10.0);
    {
        let ctx = canvas.context();
        let shader = Shader::linear_gradient(
            Point { x: 0.0, y: 0.0 },
            Point { x: 101.0, y: 0.0 },
            &[
                GradientStop {
                    position: 0.0,
                    color: RgbaLinear::opaque(0.0, 0.0, 0.0),
                },
                GradientStop {
                    position: 1.0,
                    color: RgbaLinear::opaque(1.0, 1.0, 1.0),
                },
            ],
            space,
        )
        .expect("gradient");
        ctx.set_fill_shader(&shader);
        ctx.fill_rect(0.0, 0.0, 101.0, 10.0);
    }
    let buffer = pixels(&mut canvas);
    (at(&buffer, 101, 50, 5)[0], at(&buffer, 101, 25, 5)[0])
}

#[test]
fn the_default_gradient_interpolation_matches_a_browser() {
    // `Srgb` used to map to Skia's `SRGBLinear`, so the default gradient
    // interpolated in linear light and came out visibly washed out: 188 at
    // the midpoint of black to white where CSS, Canvas and every browser
    // give 128. The existing tests missed it because one samples only the
    // endpoints, which every space shares, and the other asserts merely that
    // two spaces differ.
    assert_eq!(
        GradientColorSpace::default(),
        GradientColorSpace::Srgb,
        "gamma-encoded sRGB is the Canvas default"
    );
    assert_eq!(gradient_ramp(GradientColorSpace::Srgb).0, 128);
    assert_eq!(gradient_ramp(GradientColorSpace::SrgbLinear).0, 188);
}

/// The RGB at the midpoint of a red-to-blue gradient drawn 101 pixels wide
/// under `interpolation`.
fn hue_midpoint(interpolation: GradientInterpolation) -> [u8; 4] {
    let mut canvas = Canvas::new(101.0, 10.0);
    {
        let ctx = canvas.context();
        let shader = Shader::linear_gradient(
            Point { x: 0.0, y: 0.0 },
            Point { x: 101.0, y: 0.0 },
            &[
                GradientStop {
                    position: 0.0,
                    color: RgbaLinear::from_srgb8(255, 0, 0, 1.0),
                },
                GradientStop {
                    position: 1.0,
                    color: RgbaLinear::from_srgb8(0, 0, 255, 1.0),
                },
            ],
            interpolation,
        )
        .expect("gradient");
        ctx.set_fill_shader(&shader);
        ctx.fill_rect(0.0, 0.0, 101.0, 10.0);
    }
    at(&pixels(&mut canvas), 101, 50, 5)
}

#[test]
fn a_gradient_can_take_the_long_way_round_the_hue_wheel() {
    // Hue was pinned to the shorter arc, so the three other CSS methods were
    // unreachable from Rust while the JavaScript side had all four.
    let space = GradientColorSpace::Oklch;
    let shorter = hue_midpoint(space.hue(HueMethod::Shorter));
    let longer = hue_midpoint(space.hue(HueMethod::Longer));

    assert_eq!(
        hue_midpoint(space.into()),
        shorter,
        "a bare colour space still takes the shorter arc"
    );
    assert_ne!(shorter, longer, "the two arcs are different colours");
    // Red to blue the short way passes through magenta; the long way goes
    // round through green, which is the whole point of asking for it.
    assert!(
        longer[1] > shorter[1],
        "the long way is greener: {longer:?} against {shorter:?}"
    );

    // Increasing and decreasing name the two directions outright, so for
    // this pair they land on one arc each.
    assert_eq!(hue_midpoint(space.hue(HueMethod::Decreasing)), shorter);
    assert_eq!(hue_midpoint(space.hue(HueMethod::Increasing)), longer);
}

#[test]
fn a_hue_method_is_inert_where_there_is_no_hue_axis() {
    for space in [
        GradientColorSpace::Srgb,
        GradientColorSpace::SrgbLinear,
        GradientColorSpace::Lab,
        GradientColorSpace::Oklab,
    ] {
        assert_eq!(
            hue_midpoint(space.hue(HueMethod::Longer)),
            hue_midpoint(space.into()),
            "{space:?} has no hue to walk around"
        );
    }
}

#[test]
fn every_gradient_interpolation_space_lands_where_it_should() {
    // Each figure was matched against the JavaScript binding's `interpolation`
    // property, which takes the same eight CSS names.
    let expected = [
        (GradientColorSpace::Srgb, 128, 64),
        (GradientColorSpace::SrgbLinear, 188, 138),
        (GradientColorSpace::Lab, 119, 60),
        (GradientColorSpace::Oklab, 99, 34),
        (GradientColorSpace::Lch, 119, 60),
        (GradientColorSpace::Oklch, 99, 34),
        (GradientColorSpace::Hsl, 128, 64),
        (GradientColorSpace::Hwb, 128, 64),
    ];

    for (space, mid, quarter) in expected {
        assert_eq!(
            gradient_ramp(space),
            (mid, quarter),
            "{space:?} ramps through the wrong greys"
        );
    }
}

/// Applies `spoil` to a context already holding known-good values, and
/// reports what the getters read back afterwards. The standard has these
/// setters ignore a value they cannot use, so every field should be
/// untouched.
fn after_spoiling(
    spoil: impl Fn(&mut Context2D),
) -> (f32, f32, f32, (f32, f32), f32) {
    let mut canvas = Canvas::new(40.0, 20.0);
    let ctx = canvas.context();
    ctx.set_line_width(4.0);
    ctx.set_miter_limit(10.0);
    ctx.set_shadow_blur(3.0);
    ctx.set_shadow_offset(2.0, 2.0);
    ctx.set_line_dash_offset(1.0);

    spoil(ctx);

    (
        ctx.line_width(),
        ctx.miter_limit(),
        ctx.shadow_blur(),
        ctx.shadow_offset(),
        ctx.line_dash_offset(),
    )
}

#[test]
fn a_setter_ignores_a_value_it_cannot_use() {
    let good = (4.0, 10.0, 3.0, (2.0, 2.0), 1.0);
    assert_eq!(after_spoiling(|_| {}), good, "the known-good state");

    // Every one of these was stored verbatim, so the getter reported a value
    // that could not be drawn with. Each matches what the JavaScript binding
    // does with the same input.
    macro_rules! ignored {
        ($($method:ident($($arg:expr),*)),+ $(,)?) => {$(
            assert_eq!(
                after_spoiling(|c: &mut Context2D| c.$method($($arg),*)),
                good,
                concat!(
                    stringify!($method),
                    "(", stringify!($($arg),*), ") should have been ignored",
                ),
            );
        )+};
    }

    ignored!(
        set_line_width(0.0),
        set_line_width(-3.0),
        set_line_width(f32::NAN),
        set_line_width(f32::INFINITY),
        set_miter_limit(0.0),
        set_miter_limit(-1.0),
        set_miter_limit(f32::NAN),
        set_miter_limit(f32::INFINITY),
        set_shadow_blur(-5.0),
        set_shadow_blur(f32::NAN),
        set_shadow_blur(f32::INFINITY),
        set_shadow_offset(f32::NAN, 1.0),
        set_shadow_offset(1.0, f32::INFINITY),
        set_line_dash_offset(f32::NAN),
        set_line_dash_offset(f32::INFINITY),
    );
}

#[test]
fn line_width_reports_the_width_that_paints() {
    // The getter used to read a field kept alongside the paint rather than
    // the paint itself, so the two could drift and the reported width was
    // not necessarily the drawn one.
    for width in [2.0_f32, 6.0, 10.0] {
        let mut canvas = Canvas::new(20.0, 20.0);
        let reported = {
            let ctx = canvas.context();
            ctx.set_stroke_style(red());
            ctx.set_line_width(width);
            ctx.begin_path();
            ctx.move_to(0.0, 10.0);
            ctx.line_to(20.0, 10.0);
            ctx.stroke();
            ctx.line_width()
        };

        let buffer = pixels(&mut canvas);
        let painted =
            (0..20).filter(|y| at(&buffer, 20, 10, *y)[3] > 0).count();
        assert_eq!(reported, width, "the getter reports what was set");
        assert_eq!(
            painted, width as usize,
            "and a {width}px stroke covers that many rows"
        );
    }
}

#[test]
fn a_setter_that_accepts_a_zero_keeps_accepting_it() {
    // Zero is meaningless for a width or a miter ratio and meaningful for a
    // blur and both offsets, so the guards are deliberately not uniform.
    let mut canvas = Canvas::new(40.0, 20.0);
    let ctx = canvas.context();
    ctx.set_shadow_blur(0.0);
    ctx.set_shadow_offset(0.0, 0.0);
    ctx.set_line_dash_offset(0.0);
    assert_eq!(ctx.shadow_blur(), 0.0, "zero blur means no blur");
    assert_eq!(ctx.shadow_offset(), (0.0, 0.0));
    assert_eq!(ctx.line_dash_offset(), 0.0);

    // And a negative offset is legitimate for both of those.
    ctx.set_shadow_offset(-3.0, -4.0);
    ctx.set_line_dash_offset(-2.0);
    assert_eq!(ctx.shadow_offset(), (-3.0, -4.0));
    assert_eq!(ctx.line_dash_offset(), -2.0);
}

#[test]
fn an_ignored_setter_value_leaves_the_drawing_alone() {
    // Two of these did visible damage, not just untidy state: an infinite
    // width erased the stroke, and a non-finite dash offset destroyed the
    // pattern and left the line solid.
    let painted = |spoil: &dyn Fn(&mut Context2D)| {
        let mut canvas = Canvas::new(40.0, 20.0);
        {
            let ctx = canvas.context();
            ctx.set_stroke_style(red());
            ctx.set_line_width(4.0);
            ctx.set_line_dash(&[4.0, 4.0]);
            ctx.set_line_dash_offset(1.0);
            spoil(ctx);
            ctx.begin_path();
            ctx.move_to(0.0, 10.0);
            ctx.line_to(40.0, 10.0);
            ctx.stroke();
        }
        let buffer = pixels(&mut canvas);
        (0..40).filter(|x| at(&buffer, 40, *x, 10)[3] > 0).count()
    };

    let untouched = painted(&|_| {});
    assert!(
        (1..40).contains(&untouched),
        "the dashed line covers part of the row, covered {untouched}/40"
    );

    assert_eq!(
        painted(&|c| c.set_line_width(f32::INFINITY)),
        untouched,
        "an infinite width erased the stroke"
    );
    assert_eq!(
        painted(&|c| c.set_line_dash_offset(f32::NAN)),
        untouched,
        "a NaN dash offset drew the line solid"
    );
    assert_eq!(
        painted(&|c| c.set_line_dash_offset(f32::INFINITY)),
        untouched,
        "an infinite dash offset drew the line solid"
    );
}

/// Alpha at (2, 2) after filling `fill` square on a 20x20 page inside a
/// layer of `alpha`. The page size is the point: the full-page-opaque
/// shortcut only engages when the fill covers the whole thing.
fn layered_fill_alpha(fill: f32, alpha: f32) -> u8 {
    let mut canvas = Canvas::new(20.0, 20.0);
    {
        let ctx = canvas.context();
        ctx.save_layer_with(alpha, None, None);
        ctx.set_fill_style(red());
        ctx.fill_rect(0.0, 0.0, fill, fill);
        ctx.restore();
    }
    at(&pixels(&mut canvas), 20, 2, 2)[3]
}

#[test]
fn a_full_page_fill_inside_a_layer_keeps_the_layer_alpha() {
    // The shortcut that drops recorded content on an opaque full-page fill
    // resets the recorder, and the open layer lives on the recorder's save
    // stack -- so the layer went with it. Not just its alpha: the fill
    // landed straight on the page at 255 whatever the layer asked for.
    // Anything short of full coverage missed the shortcut and was correct,
    // which is why 19.5 worked and 20.0 did not.
    for fill in [18.0, 19.0, 19.5, 20.0, 21.0, 40.0] {
        assert_eq!(
            layered_fill_alpha(fill, 0.5),
            128,
            "a {fill}-square fill on a 20x20 page should carry the layer's \
             half alpha"
        );
    }

    // And the alpha has to scale, not merely be non-opaque -- every one of
    // these read 255 before.
    for (alpha, expected) in [(0.25, 64), (0.5, 128), (0.75, 191), (1.0, 255)] {
        assert_eq!(
            layered_fill_alpha(20.0, alpha),
            expected,
            "a full-page fill inside a layer of {alpha}"
        );
    }
}

#[test]
fn a_full_page_fill_outside_a_layer_is_still_opaque() {
    // The other half of the guard: with no layer open the shortcut must
    // still engage, and an opaque full-page fill stays opaque.
    let mut canvas = Canvas::new(20.0, 20.0);
    {
        let ctx = canvas.context();
        ctx.set_fill_style(RgbaLinear::opaque(0.0, 1.0, 0.0));
        ctx.fill_rect(3.0, 3.0, 4.0, 4.0);
        ctx.set_fill_style(red());
        ctx.fill_rect(0.0, 0.0, 20.0, 20.0);
    }
    let buffer = pixels(&mut canvas);
    assert_eq!(at(&buffer, 20, 2, 2), [255, 0, 0, 255]);
    assert_eq!(
        at(&buffer, 20, 4, 4),
        [255, 0, 0, 255],
        "the earlier draw is covered"
    );

    // A plain `save`/`restore` opens no layer, so the shortcut still applies.
    let mut canvas = Canvas::new(20.0, 20.0);
    {
        let ctx = canvas.context();
        ctx.save();
        ctx.set_fill_style(red());
        ctx.fill_rect(0.0, 0.0, 20.0, 20.0);
        ctx.restore();
    }
    assert_eq!(at(&pixels(&mut canvas), 20, 2, 2), [255, 0, 0, 255]);
}

/// The SVG a scene serializes to, as text.
fn svg_of(build: impl Fn(&mut Context2D)) -> String {
    let mut canvas = Canvas::new(20.0, 20.0);
    {
        let ctx = canvas.context();
        build(ctx);
    }
    String::from_utf8(
        canvas
            .to_buffer(ImageFormat::Svg, &EncodeOptions::default())
            .expect("svg export"),
    )
    .expect("utf-8")
}

#[test]
fn an_opaque_full_page_fill_drops_the_drawing_it_covers() {
    // Guarding the shortcut against open layers must not switch it off. Its
    // effect is invisible in pixels -- covered content looks the same either
    // way -- and shows only in what gets recorded, so this reads the vector
    // output. Without the shortcut the buried green rect is still emitted.
    let covered = svg_of(|ctx| {
        ctx.set_fill_style(RgbaLinear::opaque(0.0, 1.0, 0.0));
        ctx.fill_rect(3.0, 3.0, 4.0, 4.0);
        ctx.set_fill_style(red());
        ctx.fill_rect(0.0, 0.0, 20.0, 20.0);
    });
    assert!(
        !covered.contains("lime"),
        "the covered draw should not survive into the SVG: {covered}"
    );
    assert_eq!(covered.matches("<path").count(), 1, "one path: {covered}");

    // The same scene with the fill one pixel short keeps both draws, which
    // is what tells the two apart.
    let uncovered = svg_of(|ctx| {
        ctx.set_fill_style(RgbaLinear::opaque(0.0, 1.0, 0.0));
        ctx.fill_rect(3.0, 3.0, 4.0, 4.0);
        ctx.set_fill_style(red());
        ctx.fill_rect(0.0, 0.0, 19.0, 19.0);
    });
    assert!(
        uncovered.contains("lime"),
        "a fill short of the page keeps what it does not cover: {uncovered}"
    );
    assert_eq!(uncovered.matches("<path").count(), 2);
}

/// Mean alpha over the textured square, x1000, for a line texture whose mark
/// is `line` wide on a `period` grid. Coverage is `line / period`, so at a
/// fixed period the tone should be proportional to the width.
fn line_texture_tone(period: f32, line: f32, outline: bool) -> u32 {
    let mut canvas = Canvas::new(60.0, 60.0);
    {
        let ctx = canvas.context();
        let hatch = Texture::new(&TextureOptions {
            path: None,
            line,
            angle: 0.0,
            outline,
            spacing: (period, period),
            color: red(),
            ..TextureOptions::default()
        });
        ctx.set_fill_texture(&hatch);
        ctx.fill_rect(10.0, 10.0, 40.0, 40.0);
    }
    let buffer = pixels(&mut canvas);
    let total: u32 = (15..45)
        .flat_map(|y| (15..45).map(move |x| (x, y)))
        .map(|(x, y)| at(&buffer, 60, x, y)[3] as u32)
        .sum();
    total * 1000 / (30 * 30)
}

#[test]
fn a_line_texture_holds_its_coverage_below_a_pixel() {
    // `line_2d_path_effect` loses a mark thinner than a device pixel instead
    // of antialiasing it, and loses it faster the thinner it gets: at a
    // period of 8 the tone went 34133, 8533, 2133, 0 for widths 1, 0.5, 0.25,
    // 0.125 -- roughly the square of the width, and gone entirely at the end.
    // Nothing else in the pipeline does this. An ordinary stroke keeps its
    // coverage to within 6% at 0.125, and the path grid is exact, so the
    // remedy is the usual one for a hairline: draw it a pixel wide and take
    // the difference out of its alpha.
    for outline in [false, true] {
        let full = line_texture_tone(8.0, 1.0, outline);
        assert_eq!(full, 34133, "a one-pixel mark is the reference");

        for (width, divisor) in [(0.5, 2), (0.25, 4), (0.125, 8)] {
            let tone = line_texture_tone(8.0, width, outline);
            let expected = full / divisor;
            assert!(
                tone.abs_diff(expected) <= 2,
                "a {width}-wide mark should tone {expected}, not {tone} \
                 (outline={outline})"
            );
            assert!(tone > 0, "a {width}-wide mark is drawn at all");
        }
    }

    // The two draw branches take their colour from different paints, so they
    // have to be checked apart as well as together.
    for width in [2.0, 1.0, 0.5, 0.25, 0.125] {
        assert_eq!(
            line_texture_tone(8.0, width, false),
            line_texture_tone(8.0, width, true),
            "clipped and outlined textures agree at width {width}"
        );
    }
}

#[test]
fn a_line_texture_wider_than_a_pixel_is_untouched() {
    // The widening must not reach a mark that never needed it: everything at
    // or above a device pixel goes through unchanged.
    assert_eq!(line_texture_tone(8.0, 2.0, false), 68000);
    assert_eq!(line_texture_tone(8.0, 1.0, false), 34133);
    assert_eq!(line_texture_tone(16.0, 4.0, false), 68000);
}

#[test]
fn a_gradient_fading_to_transparent_carries_its_colour_down() {
    // Interpolation runs unpremultiplied, so a stop's colour travels toward
    // the next stop's colour as its alpha falls. Premultiplied instead holds
    // the hue and reads [255, 0, 0, a] the whole way -- which is what this
    // built before, and what neither Chrome nor the JavaScript binding does.
    // Canvas gradients are not CSS gradients; CSS Color 4's premultiplied
    // rule does not govern them.
    let mut canvas = Canvas::new(101.0, 8.0);
    {
        let ctx = canvas.context();
        let shader = Shader::linear_gradient(
            Point { x: 0.0, y: 0.0 },
            Point { x: 101.0, y: 0.0 },
            &[
                GradientStop {
                    position: 0.0,
                    color: RgbaLinear::from_srgb8(255, 0, 0, 1.0),
                },
                GradientStop {
                    position: 1.0,
                    color: RgbaLinear::new_premultiplied(0.0, 0.0, 0.0, 0.0),
                },
            ],
            GradientColorSpace::Srgb,
        )
        .expect("gradient");
        ctx.set_fill_shader(&shader);
        ctx.fill_rect(0.0, 0.0, 101.0, 8.0);
    }

    let buffer = pixels(&mut canvas);
    // Red falls with alpha rather than holding at 255. Both were measured
    // against Chrome and the binding, which agree with each other.
    for (x, expected) in [(25, [191, 0, 0, 191]), (75, [64, 0, 0, 64])] {
        let got = at(&buffer, 101, x, 4);
        assert_eq!(got, expected, "at x={x}");
        assert!(
            got[0] < 255,
            "premultiplied interpolation would hold red at 255"
        );
    }
}

#[test]
fn a_readback_rect_that_spans_the_coordinate_range_is_rejected() {
    // `SkRect::round` saturates each edge to i32::MIN/MAX, and `IRect::width`
    // then subtracts them -- so a rect that *spans* the range panicked inside
    // skia-safe with "attempt to subtract with overflow" before any check
    // here ran. Rejecting non-finite values was not enough: -3e9 is finite.
    // The earlier test only tried (0, 0, n, n), which saturates one edge and
    // leaves the other at zero, so the subtraction stayed in range.
    let mut canvas = Canvas::new(10.0, 10.0);
    let ctx = canvas.context();

    for (x, y, w, h) in [
        (-3e9, -3e9, 6e9, 6e9),
        (-2.5e9, 0.0, 5e9, 4.0),
        (f32::MIN, 0.0, f32::MAX, 4.0),
        (0.0, 0.0, 3e9, 4.0),
        (-3e9, 0.0, 4.0, 4.0),
    ] {
        assert!(
            matches!(
                ctx.get_image_data(x, y, w, h),
                Err(Error::InvalidRect { .. })
            ),
            "({x:e}, {y:e}, {w:e}, {h:e}) should be refused, not panic"
        );
    }

    // A rect the page can express still works, including one reaching past
    // the page, which the Canvas API allows and reads back transparent.
    assert!(ctx.get_image_data(0.0, 0.0, 10.0, 10.0).is_ok());
    assert!(ctx.get_image_data(-5.0, -5.0, 20.0, 20.0).is_ok());
}

#[test]
fn a_projection_that_cannot_be_solved_is_none() {
    // Skia reports success for some quads it cannot solve and hands back a
    // matrix of NaN. Four identical corners does it; so does one non-finite
    // corner. Collinear corners it does reject, which is why the earlier
    // coverage looked sufficient.
    let mut canvas = Canvas::new(100.0, 100.0);
    let ctx = canvas.context();
    let p = |x, y| Point { x, y };

    let solvable = ctx
        .create_projection(
            [p(10.0, 10.0), p(90.0, 20.0), p(80.0, 90.0), p(20.0, 80.0)],
            None,
        )
        .expect("a trapezoid is solvable");
    assert!(solvable.values.iter().all(|v| v.is_finite()));

    for (label, quad) in [
        ("four identical corners", [p(5.0, 5.0); 4]),
        (
            "collinear corners",
            [p(0.0, 0.0), p(10.0, 10.0), p(20.0, 20.0), p(30.0, 30.0)],
        ),
        (
            "a non-finite corner",
            [p(0.0, 0.0), p(f32::NAN, 0.0), p(1.0, 1.0), p(0.0, 1.0)],
        ),
    ] {
        assert!(
            ctx.create_projection(quad, None).is_none(),
            "{label} has no projection"
        );
    }
}

#[test]
fn a_non_finite_transform_is_ignored_rather_than_poisoning_the_canvas() {
    // Storing one mapped every later draw to NaN, so the context painted
    // nothing for the rest of its life. `Projection` has a public field, so
    // it can be built by hand as well as returned by `create_projection`.
    let painted = |spoil: &dyn Fn(&mut Context2D)| {
        let mut canvas = Canvas::new(20.0, 20.0);
        {
            let ctx = canvas.context();
            spoil(ctx);
            ctx.set_fill_style(red());
            ctx.fill_rect(0.0, 0.0, 20.0, 20.0);
        }
        let buffer = pixels(&mut canvas);
        (0..20)
            .flat_map(|y| (0..20).map(move |x| (x, y)))
            .filter(|&(x, y)| at(&buffer, 20, x, y)[3] > 0)
            .count()
    };

    assert_eq!(painted(&|_| {}), 400, "the untouched canvas fills");

    assert_eq!(
        painted(&|c| c.set_transform(Affine {
            a: f32::NAN,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            tx: 0.0,
            ty: 0.0,
        })),
        400,
        "a NaN transform is ignored"
    );
    assert_eq!(
        painted(&|c| c.set_transform(Affine {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            tx: f32::INFINITY,
            ty: 0.0,
        })),
        400,
        "an infinite translation is ignored"
    );
    assert_eq!(
        painted(&|c| c.set_projection(&Projection {
            values: [f32::NAN; 9]
        })),
        400,
        "a NaN projection is ignored"
    );

    // A transform that is merely unusual still applies.
    assert!(
        painted(&|c| c.set_transform(Affine {
            a: 0.5,
            b: 0.0,
            c: 0.0,
            d: 0.5,
            tx: 0.0,
            ty: 0.0,
        })) < 400,
        "a half-scale transform shrinks the fill"
    );
}

#[test]
fn a_shadow_colour_round_trips_only_as_far_as_eight_bits_reach() {
    // The getter goes through `skia_color_to_rgba_linear`, whose doc claimed
    // a value "reads back as it was written". It does not below alpha 1: the
    // colour is stored as 8-bit sRGB with 8-bit alpha, so anything off a
    // whole 255th is quantised, and the premultiplied components shift with
    // it because they are re-derived from the stored alpha.
    let mut canvas = Canvas::new(10.0, 10.0);
    let ctx = canvas.context();

    // Every byte round-trips exactly at full alpha.
    for byte in [0u8, 1, 127, 128, 200, 254, 255] {
        let set = RgbaLinear::from_srgb8(byte, byte, byte, 1.0);
        ctx.set_shadow_color(set);
        let got = ctx.shadow_color();
        assert!(
            (got.r - set.r).abs() < 1e-6 && (got.a - 1.0).abs() < 1e-6,
            "byte {byte} should survive: set {set:?}, got {got:?}"
        );
    }

    // So does any alpha that is a whole 255th.
    for step in [0u8, 1, 64, 128, 255] {
        let alpha = f32::from(step) / 255.0;
        ctx.set_shadow_color(RgbaLinear::from_srgb8(200, 100, 50, alpha));
        assert!(
            (ctx.shadow_color().a - alpha).abs() < 1e-6,
            "alpha {step}/255 should survive"
        );
    }

    // An alpha between two of them does not, and that is what the doc used
    // to deny.
    ctx.set_shadow_color(RgbaLinear::from_srgb8(230, 120, 40, 0.5));
    let got = ctx.shadow_color();
    assert_ne!(got.a, 0.5, "0.5 is not a whole 255th");
    assert!(
        (got.a - 128.0 / 255.0).abs() < 1e-6,
        "it lands on the nearest one, {}",
        got.a
    );
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
        ctx.arc(20.0, 20.0, 18.0, 0.0, std::f32::consts::FRAC_PI_2, false)
            .expect("positive radius");
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

    // A number is bound to its unit in CSS. Splitting on `px ` let a space
    // in front of the unit through, so this parsed as 44 pixels.
    assert!(
        Font::parse("44 px Helvetica").is_err(),
        "size split from unit"
    );

    // And the reason has to name the end of the string that was wrong.
    let reason = |shorthand: &str| match Font::parse(shorthand) {
        Err(Error::FontRegister { reason }) => reason,
        other => {
            panic!("{shorthand:?} should have been rejected, got {other:?}")
        }
    };
    assert!(
        reason("20px").contains("no font family"),
        "got {:?}",
        reason("20px")
    );
    assert!(
        reason("Helvetica").contains("no `<size>px`"),
        "got {:?}",
        reason("Helvetica")
    );
}

#[test]
fn font_parse_reads_the_stretch_and_the_line_height() {
    let font = Font::parse("condensed 16px/24px Helvetica").expect("parses");
    assert_eq!(font.stretch, FontStretch::Condensed);
    assert_eq!(font.size, 16.0);
    assert_eq!(font.line_height, Some(24.0));

    assert_eq!(
        Font::parse("ultra-expanded 12px X")
            .expect("parses")
            .stretch,
        FontStretch::UltraExpanded
    );
    assert_eq!(
        Font::parse("12px X").expect("parses").stretch,
        FontStretch::Normal,
        "absent means the family's own width"
    );
    assert!(
        Font::parse("16px/wide Helvetica").is_err(),
        "an unparseable line height is rejected, not dropped"
    );
}

#[test]
fn the_font_string_carries_everything_the_font_holds() {
    // Every field has to survive the trip out through the getter and back in
    // through the parser; a slant or a stretch missing from the string was
    // silently lost. The strings are what the JavaScript binding reports for
    // the same input, so both halves of the project say the same thing.
    let cases = [
        ("16px Helvetica", "normal 400 16px Helvetica"),
        ("italic 16px Helvetica", "italic normal 400 16px Helvetica"),
        ("bold 16px Helvetica", "normal 700 16px Helvetica"),
        (
            "italic 700 44px Helvetica",
            "italic normal 700 44px Helvetica",
        ),
        (
            "condensed 16px Helvetica",
            "normal 400 condensed 16px Helvetica",
        ),
        (
            "italic condensed 700 44px Helvetica",
            "italic normal 700 condensed 44px Helvetica",
        ),
        ("16px/24px Helvetica", "normal 400 16px/24px Helvetica"),
        (
            "300 12px Comic Sans, serif",
            "normal 300 12px \"Comic Sans\", serif",
        ),
    ];

    let mut canvas = Canvas::new(10.0, 10.0);
    let ctx = canvas.context();
    for (shorthand, expected) in cases {
        let font = Font::parse(shorthand).expect("parses");
        ctx.set_font(&font);
        assert_eq!(ctx.font(), expected, "canonical form of {shorthand:?}");
        assert_eq!(
            Font::parse(&ctx.font()).expect("re-parses"),
            font,
            "round trip of {shorthand:?}"
        );
    }
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
fn dither_changes_a_shallow_gradient() {
    // Dithering only shows where banding would: a gradient across two nearby
    // greys, wide enough that each 8-bit step spans many pixels. Asserting
    // that text still renders with the flag set -- which is what this used to
    // do -- passes with the flag ignored.
    let ramp = |dither: bool| {
        let mut canvas = Canvas::new(120.0, 8.0);
        canvas.set_gpu(false);
        {
            let ctx = canvas.context();
            ctx.set_dither(dither);
            let shader = Shader::linear_gradient(
                Point { x: 0.0, y: 0.0 },
                Point { x: 120.0, y: 0.0 },
                &[
                    GradientStop {
                        position: 0.0,
                        color: RgbaLinear::from_srgb8(40, 40, 44, 1.0),
                    },
                    GradientStop {
                        position: 1.0,
                        color: RgbaLinear::from_srgb8(48, 48, 52, 1.0),
                    },
                ],
                GradientColorSpace::Srgb,
            )
            .expect("gradient");
            ctx.set_fill_shader(&shader);
            ctx.fill_rect(0.0, 0.0, 120.0, 8.0);
        }
        pixels(&mut canvas)
    };

    assert_ne!(ramp(true), ramp(false), "the flag reaches the paint");
}

#[test]
fn font_hinting_is_carried_even_where_the_rasterizer_ignores_it() {
    // Hinting is a request the platform's font engine may decline -- macOS
    // CoreText does, and the same glyphs come out to the pixel either way.
    // What can be asserted is that the flag is stored, read back, and
    // saved and restored with the rest of the state.
    let mut canvas = Canvas::new(60.0, 30.0);
    let ctx = canvas.context();
    assert!(!ctx.font_hinting(), "off by default");

    ctx.set_font_hinting(true);
    assert!(ctx.font_hinting());

    ctx.save();
    ctx.set_font_hinting(false);
    assert!(!ctx.font_hinting());
    ctx.restore();
    assert!(ctx.font_hinting(), "restored with the state");

    ctx.set_fill_style(red());
    ctx.set_font(&Font::new("Helvetica", 16.0));
    ctx.fill_text("Hinted", 4.0, 20.0, None);
    let painted = pixels(&mut canvas)
        .chunks_exact(4)
        .filter(|px| px[3] > 0)
        .count();
    assert!(painted > 0, "and text still renders with it set");
}

#[test]
fn set_transform_replaces_the_matrix_rather_than_concatenating() {
    // `transform` multiplies onto what is there; `set_transform` throws that
    // away. Neither had a direct test, and the pair is easy to confuse.
    let painted_at = |replace: bool| {
        let mut canvas = Canvas::new(30.0, 30.0);
        {
            let ctx = canvas.context();
            ctx.translate(10.0, 10.0);
            let shift = Affine {
                a: 1.0,
                b: 0.0,
                c: 0.0,
                d: 1.0,
                tx: 5.0,
                ty: 5.0,
            };
            if replace {
                ctx.set_transform(shift);
            } else {
                ctx.transform(shift);
            }
            ctx.set_fill_style(red());
            ctx.fill_rect(0.0, 0.0, 4.0, 4.0);
        }
        let buffer = pixels(&mut canvas);
        (0..30)
            .flat_map(|y| (0..30).map(move |x| (x, y)))
            .find(|&(x, y)| at(&buffer, 30, x, y)[3] > 0)
            .expect("something was painted")
    };

    assert_eq!(painted_at(true), (5, 5), "the earlier translate is gone");
    assert_eq!(painted_at(false), (15, 15), "where transform adds to it");
}

#[test]
fn begin_path_discards_what_was_traced_before_it() {
    let mut canvas = Canvas::new(30.0, 30.0);
    {
        let ctx = canvas.context();
        ctx.set_fill_style(red());
        ctx.rect(2.0, 2.0, 8.0, 8.0);
        ctx.begin_path();
        ctx.rect(18.0, 18.0, 8.0, 8.0);
        ctx.fill(FillRule::NonZero);
    }

    let buffer = pixels(&mut canvas);
    assert_eq!(at(&buffer, 30, 5, 5)[3], 0, "the discarded rect is gone");
    assert_eq!(at(&buffer, 30, 22, 22)[0], 255, "the one after it is not");
}

#[test]
fn close_path_joins_the_contour_back_to_its_start() {
    let stroked = |closed: bool| {
        let mut canvas = Canvas::new(30.0, 30.0);
        {
            let ctx = canvas.context();
            ctx.set_stroke_style(red());
            ctx.set_line_width(2.0);
            ctx.begin_path();
            ctx.move_to(5.0, 5.0);
            ctx.line_to(25.0, 5.0);
            ctx.line_to(25.0, 25.0);
            if closed {
                ctx.close_path();
            }
            ctx.stroke();
        }
        // The closing edge runs diagonally from (25, 25) back to (5, 5).
        at(&pixels(&mut canvas), 30, 15, 15)[3]
    };

    assert!(stroked(true) > 0, "the closing edge is stroked");
    assert_eq!(stroked(false), 0, "and is absent without close_path");
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
        GradientColorSpace::default(),
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
    assert_eq!(long.line_count, 1, "unwrapped text is one line");
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
fn a_zero_bound_is_reported_as_positive_zero() {
    // Negating a zero gives a negative zero. It compares equal to zero, so
    // arithmetic never notices, but it formats as `-0` and JavaScript can
    // see it through `Object.is` -- and a browser reports `0` here.
    let mut canvas = Canvas::new(50.0, 50.0);
    let ctx = canvas.context();
    ctx.set_font(&Font::new("Helvetica", 20.0));

    for text in ["", " "] {
        let metrics = ctx.measure_text(text, None);
        for (name, value) in [
            ("left", metrics.actual_bounding_box_left),
            ("ascent", metrics.actual_bounding_box_ascent),
        ] {
            assert_eq!(value, 0.0, "{name} of {text:?}");
            assert!(
                value.is_sign_positive(),
                "{name} of {text:?} is a negative zero"
            );
        }
    }
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
    // Tied to the rendered pixels, not just to `width`. The earlier version
    // asserted `span <= width + 1.0`, which had ~16% slack and would have
    // passed with left and right swapped, or either one scaled.
    const ORIGIN_X: f32 = 40.0;
    let mut canvas = Canvas::new(200.0, 80.0);
    let metrics;
    {
        let ctx = canvas.context();
        ctx.set_font(&Font::new("Helvetica", 40.0));
        ctx.set_fill_style(red());
        metrics = ctx.measure_text("lloq", None);
        ctx.fill_text("lloq", ORIGIN_X, 55.0, None);
    }

    let buffer = pixels(&mut canvas);
    let painted = |x: u32| (0..80).any(|y| at(&buffer, 200, x, y)[3] > 0);
    let leftmost = (0..200).find(|&x| painted(x)).expect("ink") as f32;
    let rightmost = (0..200).rev().find(|&x| painted(x)).expect("ink") as f32;

    // The box is measured outwards from the alignment point, so left grows
    // leftwards and right grows rightwards.
    let box_left = ORIGIN_X - metrics.actual_bounding_box_left;
    let box_right = ORIGIN_X + metrics.actual_bounding_box_right;

    // Containment, not equality: this reports the laid-out box, as the JS
    // side does, so a trailing glyph's right side bearing leaves the box a
    // little wider than the ink. Swapping left and right, or scaling either,
    // breaks the containment.
    assert!(
        box_left <= leftmost,
        "the box must start at or before the ink: {box_left} vs {leftmost}"
    );
    assert!(
        box_right >= rightmost,
        "and end at or after it: {box_right} vs {rightmost}"
    );
    assert!(
        leftmost - box_left <= 8.0 && box_right - rightmost <= 8.0,
        "but only by a side bearing, not by an arbitrary amount: \
         [{box_left}, {box_right}] around [{leftmost}, {rightmost}]"
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
    // Two draws over a backdrop, once with a default layer around them and
    // once without. Under source-over the two are identical, which is the
    // property being named -- and on its own it is also what a `save_layer`
    // that did nothing at all produces, so the second half is what makes the
    // test bite: a composite operation applies inside the layer and reaches
    // the page only through the composite at `restore`.
    let sample = |layered: bool, mode: BlendMode| {
        let mut canvas = Canvas::new(20.0, 20.0);
        {
            let ctx = canvas.context();
            ctx.set_fill_style(RgbaLinear::from_srgb8(0, 0, 255, 1.0));
            ctx.fill_rect(0.0, 0.0, 20.0, 20.0);

            if layered {
                ctx.save_layer();
            }
            ctx.set_global_composite_operation(mode);
            ctx.set_fill_style(RgbaLinear::from_srgb8(255, 0, 0, 0.5));
            ctx.fill_rect(0.0, 0.0, 10.0, 10.0);
            if layered {
                ctx.restore();
            }
        }
        let buffer = pixels(&mut canvas);
        (at(&buffer, 20, 2, 2), at(&buffer, 20, 17, 17))
    };

    assert_eq!(
        sample(true, BlendMode::SourceOver),
        sample(false, BlendMode::SourceOver),
        "a default layer composites as if it were not there"
    );

    let (inside, outside) = sample(true, BlendMode::Copy);
    assert_eq!(
        inside,
        [128, 0, 127, 255],
        "copy replaces the layer's own contents, which then composite over \
         the backdrop at half alpha"
    );
    assert_eq!(
        outside,
        [0, 0, 255, 255],
        "and the backdrop the layer never covered survives"
    );
    assert_eq!(
        sample(false, BlendMode::Copy),
        ([255, 0, 0, 128], [0, 0, 0, 0]),
        "where without a layer the same copy replaces the page itself"
    );
}

#[test]
fn save_layer_bounds_clip_what_the_layer_paints() {
    // Skia calls the layer bounds a sizing hint for the offscreen, and this
    // test used to assert they were advisory -- "verified against the
    // binding", which agreed only because both sides shared a defect. It
    // filled the whole page, which was the one geometry where an opaque
    // full-page fill made `draw_path` reset the recorder and throw the layer
    // away, bounds and all. Any smaller fill was clipped even then. They are
    // a clip.
    let sample = |fill: f32, bounds: Option<Rect>| {
        let mut canvas = Canvas::new(30.0, 30.0);
        {
            let ctx = canvas.context();
            ctx.save_layer_with(1.0, bounds, None);
            ctx.set_fill_style(red());
            ctx.fill_rect(0.0, 0.0, fill, fill);
            ctx.restore();
        }
        let buffer = pixels(&mut canvas);
        (at(&buffer, 30, 5, 5)[3], at(&buffer, 30, 20, 20)[3])
    };

    let hint = Some(Rect {
        left: 0.0,
        top: 0.0,
        right: 10.0,
        bottom: 10.0,
    });

    // Both a full-page fill and one short of it are clipped alike. The pair
    // matters: only the second was ever correct before.
    for fill in [29.0, 30.0] {
        assert_eq!(
            sample(fill, hint),
            (255, 0),
            "a {fill}-square fill is clipped to the layer bounds"
        );
        assert_eq!(
            sample(fill, None),
            (255, 255),
            "and reaches everywhere without them"
        );
    }
}

/// The three sample columns of the composite scene: the destination alone,
/// the overlap, and the source alone. Opaque throughout, so the readback is
/// exact rather than an unpremultiply of a rounded product.
fn composite_columns(mode: BlendMode) -> [[u8; 4]; 3] {
    let mut canvas = Canvas::new(30.0, 30.0);
    {
        let ctx = canvas.context();
        ctx.set_fill_style(RgbaLinear::from_srgb8(230, 120, 40, 1.0));
        ctx.fill_rect(0.0, 0.0, 20.0, 30.0);

        ctx.set_global_composite_operation(mode);
        ctx.set_fill_style(RgbaLinear::from_srgb8(40, 160, 180, 1.0));
        ctx.fill_rect(10.0, 0.0, 20.0, 30.0);
    }
    let buffer = pixels(&mut canvas);
    [
        at(&buffer, 30, 5, 15),
        at(&buffer, 30, 15, 15),
        at(&buffer, 30, 25, 15),
    ]
}

#[test]
fn an_ordinary_draw_honours_the_composite_operation() {
    // The setter has to write the blend onto the state paint as well as the
    // state field. Only the field was written, which left the six modes that
    // take a layer of their own working -- and those are the ones the layer
    // test below exercises -- while twenty-two others silently drew
    // source-over. The getter read the field too, so it agreed with the
    // caller the whole time.
    //
    // Every expectation here was matched against the JavaScript binding,
    // which sets both and is the reference for this behaviour.
    let over = composite_columns(BlendMode::SourceOver);
    assert_eq!(
        over,
        [
            [230, 120, 40, 255],
            [40, 160, 180, 255],
            [40, 160, 180, 255]
        ],
        "the source covers the overlap"
    );

    // Porter-Duff modes that do not take the layer path.
    assert_eq!(
        composite_columns(BlendMode::SourceAtop)[2],
        [0, 0, 0, 0],
        "source-atop paints nothing where the destination is transparent"
    );
    assert_eq!(
        composite_columns(BlendMode::DestinationOver)[1],
        [230, 120, 40, 255],
        "destination-over leaves the destination on top in the overlap"
    );
    assert_eq!(
        composite_columns(BlendMode::Xor)[1],
        [0, 0, 0, 0],
        "xor cancels where both are opaque"
    );

    // A separable blend, computed rather than selected.
    assert_eq!(
        composite_columns(BlendMode::Multiply)[1],
        [36, 75, 28, 255],
        "multiply darkens the overlap"
    );
    assert_eq!(
        composite_columns(BlendMode::Lighten)[1],
        [230, 160, 180, 255],
        "lighten takes the larger of each channel"
    );

    // The two that are meant to ignore the source's colour entirely.
    assert_eq!(
        composite_columns(BlendMode::Clear),
        [[230, 120, 40, 255], [0, 0, 0, 0], [0, 0, 0, 0]],
        "clear erases everything the source covers"
    );
    assert_eq!(
        composite_columns(BlendMode::Destination),
        [[230, 120, 40, 255], [230, 120, 40, 255], [0, 0, 0, 0]],
        "destination keeps what was there and adds nothing"
    );
}

#[test]
fn lighter_adds_where_lighten_only_takes_the_larger() {
    // One letter apart and both real, which is why the additive one is named
    // for the Canvas keyword rather than the CSS one: reaching for `Lighter`
    // and landing on `Lighten` compiles and draws the wrong thing.
    let [_, lighter, _] = composite_columns(BlendMode::Lighter);
    let [_, lighten, _] = composite_columns(BlendMode::Lighten);

    assert!(
        lighter != lighten,
        "the additive mode and the separable one are not the same draw"
    );
    assert!(
        lighter
            .iter()
            .zip(&lighten)
            .all(|(sum, larger)| sum >= larger),
        "a sum is never below the larger of its parts, got \
         {lighter:?} against {lighten:?}"
    );
    assert!(
        lighter[..3].contains(&255),
        "and it clamps where the sum runs past the top, got {lighter:?}"
    );
}

#[test]
fn no_composite_operation_silently_falls_back_to_source_over() {
    // The failure this guards against was uniform: a mode that never reached
    // the paint rendered exactly as source-over. Twenty-two of these did.
    // Naming them individually would have missed whichever one was not on
    // the list, so this asserts over the whole enum instead.
    let over = composite_columns(BlendMode::SourceOver);
    let indistinguishable: Vec<_> = [
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
        BlendMode::Lighter,
        BlendMode::Clear,
        BlendMode::Modulate,
        BlendMode::Destination,
    ]
    .into_iter()
    .filter(|&mode| composite_columns(mode) == over)
    .collect();

    assert!(
        indistinguishable.is_empty(),
        "these render as source-over: {indistinguishable:?}"
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

    // Two marks the read window can tell apart, so the assertions below turn
    // on *where* it read and not only how big the result was. Dropping the
    // `x += w` that shifts the origin left the size right and the region
    // wrong, and a dimensions-only check could not see it.
    ctx.set_fill_style(red());
    ctx.fill_rect(2.0, 3.0, 4.0, 5.0);
    ctx.set_fill_style(RgbaLinear::opaque(0.0, 0.0, 1.0));
    ctx.fill_rect(6.0, 8.0, 4.0, 2.0);

    // Negative extents describe the same region backwards, which the Canvas
    // API accepts by shifting the origin.
    let data = ctx.get_image_data(6.0, 8.0, -4.0, -5.0).expect("readback");
    assert_eq!((data.width(), data.height()), (4, 5));

    let forwards = ctx.get_image_data(2.0, 3.0, 4.0, 5.0).expect("readback");
    assert_eq!(
        data.pixels(),
        forwards.pixels(),
        "the inverted rect reads the region its far corner describes"
    );
    assert_eq!(
        at(data.pixels(), 4, 1, 1),
        [255, 0, 0, 255],
        "which is the red mark, not the blue one at the origin given"
    );
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
fn create_image_data_rejects_more_than_skia_can_address() {
    let mut canvas = Canvas::new(10.0, 10.0);
    let ctx = canvas.context();

    // A pixel buffer is measured in signed 32-bit bytes, which at four bytes
    // a pixel puts the largest square at 23170.
    assert!(
        ctx.create_image_data(23170, 23170).is_ok(),
        "the last size that fits"
    );
    assert!(matches!(
        ctx.create_image_data(23171, 23171),
        Err(Error::InvalidDimensions { .. })
    ));

    // 4x10^18 bytes fits a `usize`, so the overflow check passed it through
    // and the allocation aborted the process -- rc=134, not an `Error`.
    assert!(matches!(
        ctx.create_image_data(1_000_000_000, 1_000_000_000),
        Err(Error::InvalidDimensions { .. })
    ));

    // And the quiet one in between. `vec![0; n]` allocates zeroed, so this
    // used to return `Ok` holding 40 GB of lazily mapped address space that
    // would have died on the first write to it.
    assert!(matches!(
        ctx.create_image_data(100_000, 100_000),
        Err(Error::InvalidDimensions { .. })
    ));
}

#[test]
fn pixel_buffer_dimensions_cannot_overflow_an_i32() {
    // `put_image_data` hands both dimensions to Skia as `i32`. A width past
    // `i32::MAX` truncated to a negative and panicked. Nothing guards that
    // cast directly -- it is safe because no such buffer can be built, since
    // four bytes a pixel puts anything that wide past the byte ceiling. This
    // pins that reasoning: if the ceiling is ever relaxed, this fails, and
    // the cast in `blit` needs its own check.
    for width in [3_000_000_000u32, u32::MAX] {
        assert!(
            ExportedPixels::blank(width, 1, PixelExportOptions::default())
                .is_err(),
            "blank({width}, 1) must be refused"
        );
        assert!(
            ExportedPixels::from_pixels(
                width,
                1,
                PixelExportOptions::default(),
                vec![0; 16],
            )
            .is_err(),
            "from_pixels({width}, 1) must be refused"
        );
    }

    // The largest buffer that does fit stays constructible, so the ceiling
    // is not simply refusing everything large.
    assert!(
        ExportedPixels::blank(23170, 23170, PixelExportOptions::default())
            .is_ok()
    );
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

    // The same quadrant described backwards, from its far corner. The origin
    // has to shift with the sign; keeping it and only flipping the extent
    // wrote a different part of the patch.
    let mut backwards = Canvas::new(10.0, 10.0);
    {
        let ctx = backwards.context();
        let mut patch = ctx.create_image_data(4, 4).expect("allocate");
        patch.pixels_mut().fill(255);
        ctx.put_image_data_region(&patch, 0.0, 0.0, 4.0, 4.0, -2.0, -2.0)
            .expect("write");
    }
    assert_eq!(
        pixels(&mut backwards),
        buffer,
        "an inverted dirty rect writes the region its far corner describes"
    );
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
    // Asserts on the rendered pixels, not only on `filter()`. The string and
    // the Skia filter chain are two halves of one value, and a restore that
    // put back the string while leaving the chain stale would look correct
    // to a getter-only test.
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

    let blurred = render(&|ctx| {
        ctx.set_filter(&[FilterOp::Blur(4.0)])
            .expect("valid filter");
    });
    let restored = render(&|ctx| {
        ctx.set_filter(&[FilterOp::Blur(4.0)])
            .expect("valid filter");
        ctx.save();
        ctx.set_filter(&[FilterOp::Sepia(1.0)])
            .expect("valid filter");
        ctx.restore();
    });

    assert_eq!(restored, blurred, "the blur draws again after the restore");
    assert_ne!(
        restored,
        render(&|ctx| {
            ctx.set_filter(&[FilterOp::Sepia(1.0)])
                .expect("valid filter");
        }),
        "and the sepia did not survive it"
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
    // Same reasoning as the filter case: the CSS string alone would not
    // catch a restore that left the laid-out decoration stale.
    let render = |apply: &dyn Fn(&mut Context2D)| {
        let mut canvas = Canvas::new(140.0, 60.0);
        {
            let ctx = canvas.context();
            ctx.set_font(&Font::new("Helvetica", 24.0));
            ctx.set_fill_style(red());
            apply(ctx);
            ctx.fill_text("nnn", 10.0, 30.0, None);
        }
        pixels(&mut canvas)
    };

    let overlined = render(&|ctx| {
        ctx.set_text_decoration(
            TextDecoration::overline(),
            TextDecorationStyle::Solid,
            None,
            None,
        );
    });
    let restored = render(&|ctx| {
        ctx.set_text_decoration(
            TextDecoration::overline(),
            TextDecorationStyle::Solid,
            None,
            None,
        );
        ctx.save();
        clear_decoration(ctx);
        ctx.restore();
    });

    assert_eq!(restored, overlined, "the overline draws again");
    assert_ne!(restored, render(&|_| {}), "and it is really drawing one");
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
    // The band has to come out *hatched*, not merely inked: with
    // `set_stroke_texture` stubbed, a plain stroke covers the same band
    // solidly and in the default colour, which is what this used to accept.
    let banded = |textured: bool| {
        let mut canvas = Canvas::new(40.0, 40.0);
        {
            let ctx = canvas.context();
            let hatch = Texture::new(&TextureOptions {
                color: red(),
                spacing: (4.0, 4.0),
                ..TextureOptions::default()
            });
            if textured {
                ctx.set_stroke_texture(&hatch);
            } else {
                ctx.set_stroke_style(red());
            }
            ctx.set_line_width(10.0);
            ctx.begin_path();
            ctx.move_to(0.0, 20.0);
            ctx.line_to(40.0, 20.0);
            ctx.stroke();
        }
        pixels(&mut canvas)
    };

    let buffer = banded(true);
    let band: Vec<[u8; 4]> = (15..25)
        .flat_map(|y| (0..40).map(move |x| (x, y)))
        .map(|(x, y)| at(&buffer, 40, x, y))
        .collect();
    let inked = band.iter().filter(|px| px[3] > 0).count();
    let off_band = (0..40)
        .flat_map(|y| (0..8).map(move |x| (x, y)))
        .filter(|&(x, y)| at(&buffer, 40, y, x)[3] > 0)
        .count();

    assert_eq!(off_band, 0, "nothing outside the stroke");
    assert!(
        band.iter()
            .filter(|px| px[3] > 0)
            .all(|px| px[0] > px[1] && px[0] > px[2]),
        "what ink there is carries the texture's colour"
    );

    // A solid stroke covers the whole band. A 1px mark on a 4px grid covers
    // a quarter of it, so anything near the solid figure means the texture
    // never reached the paint.
    let solid = banded(false);
    let solid_inked = (15..25)
        .flat_map(|y| (0..40).map(move |x| (x, y)))
        .filter(|&(x, y)| at(&solid, 40, x, y)[3] > 0)
        .count();
    assert_eq!(solid_inked, 400, "the plain stroke covers the whole band");
    assert!(
        (20..=200).contains(&inked),
        "the textured band is hatched, covered {inked}/400"
    );
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
            ctx.arc(30.0, 30.0, 20.0, 0.0, std::f32::consts::TAU, false)
                .expect("positive radius");
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
    // Asserted through what it paints as well as what it reports: the string
    // is a field, and a field can agree with the caller while the paint the
    // draw actually uses has already been rebuilt from the rejected chain.
    let square = |spoil: bool| {
        let mut canvas = Canvas::new(30.0, 30.0);
        {
            let ctx = canvas.context();
            ctx.set_filter(&[FilterOp::Blur(4.0)])
                .expect("valid filter");
            if spoil {
                let _ = ctx.set_filter(&[
                    FilterOp::Saturate(1.5),
                    FilterOp::Sepia(f32::NAN),
                ]);
                assert_eq!(
                    ctx.filter(),
                    "blur(4px)",
                    "a rejected chain must not half-apply"
                );
            }
            ctx.set_fill_style(red());
            ctx.fill_rect(10.0, 10.0, 10.0, 10.0);
        }
        pixels(&mut canvas)
    };

    let blurred = square(false);
    assert!(
        at(&blurred, 30, 8, 15)[3] > 0,
        "the blur reaches outside the square it was applied to"
    );
    assert_eq!(
        square(true),
        blurred,
        "and the rejected chain changed neither the state nor the drawing"
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
fn clip_restricts_later_drawing_to_the_current_path() {
    // The method had one call site in the whole suite: the test asserting
    // that `put_image_data` *ignores* the clip. A `clip` that did nothing
    // passed it.
    let mut canvas = Canvas::new(30.0, 30.0);
    {
        let ctx = canvas.context();
        ctx.begin_path();
        ctx.rect(5.0, 5.0, 10.0, 10.0);
        ctx.clip(FillRule::NonZero);
        ctx.set_fill_style(red());
        ctx.fill_rect(0.0, 0.0, 30.0, 30.0);
    }

    let buffer = pixels(&mut canvas);
    assert_eq!(at(&buffer, 30, 10, 10)[0], 255, "inside the clip");
    assert_eq!(at(&buffer, 30, 20, 20)[3], 0, "outside it");
}

#[test]
fn clip_intersects_rather_than_replaces() {
    let mut canvas = Canvas::new(30.0, 30.0);
    {
        let ctx = canvas.context();
        ctx.begin_path();
        ctx.rect(0.0, 0.0, 20.0, 20.0);
        ctx.clip(FillRule::NonZero);

        // Overlapping the first by a quarter. Only the overlap survives.
        ctx.begin_path();
        ctx.rect(10.0, 10.0, 20.0, 20.0);
        ctx.clip(FillRule::NonZero);

        ctx.set_fill_style(red());
        ctx.fill_rect(0.0, 0.0, 30.0, 30.0);
    }

    let buffer = pixels(&mut canvas);
    assert_eq!(at(&buffer, 30, 15, 15)[0], 255, "the overlap is drawn");
    assert_eq!(at(&buffer, 30, 5, 5)[3], 0, "the first clip alone is not");
    assert_eq!(at(&buffer, 30, 25, 25)[3], 0, "nor the second alone");
}

#[test]
fn clip_honours_the_fill_rule_it_is_given() {
    // A doubly-wound ring clips as solid under NonZero and as a hollow frame
    // under EvenOdd, so the rule argument has to reach the clip.
    let clipped = |rule: FillRule| {
        let mut canvas = Canvas::new(30.0, 30.0);
        {
            let ctx = canvas.context();
            ctx.begin_path();
            ctx.rect(2.0, 2.0, 26.0, 26.0);
            ctx.rect(8.0, 8.0, 14.0, 14.0);
            ctx.clip(rule);
            ctx.set_fill_style(red());
            ctx.fill_rect(0.0, 0.0, 30.0, 30.0);
        }
        let buffer = pixels(&mut canvas);
        (at(&buffer, 30, 15, 15)[3], at(&buffer, 30, 4, 4)[3])
    };

    assert_eq!(clipped(FillRule::NonZero), (255, 255), "solid through");
    assert_eq!(clipped(FillRule::EvenOdd), (0, 255), "hollow in the middle");
}

#[test]
fn clip_is_undone_by_restore() {
    let mut canvas = Canvas::new(30.0, 30.0);
    {
        let ctx = canvas.context();
        ctx.set_fill_style(red());

        ctx.save();
        ctx.begin_path();
        ctx.rect(5.0, 5.0, 5.0, 5.0);
        ctx.clip(FillRule::NonZero);
        // Inside the scope the clip is in force, which is what makes the
        // assertion after the restore mean something.
        ctx.fill_rect(0.0, 0.0, 30.0, 30.0);
        ctx.restore();

        ctx.fill_rect(20.0, 20.0, 8.0, 8.0);
    }

    let buffer = pixels(&mut canvas);
    assert_eq!(at(&buffer, 30, 7, 7)[0], 255, "the clipped fill landed");
    assert_eq!(at(&buffer, 30, 15, 15)[3], 0, "and went no further");
    assert_eq!(
        at(&buffer, 30, 24, 24)[0],
        255,
        "the clip lifted with the restore"
    );
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

// -- Building a path ---------------------------------------------------------

/// Fills `path` on a 30x30 canvas and returns the raw pixels.
fn filled(path: &Path) -> Vec<u8> {
    let mut canvas = Canvas::new(30.0, 30.0);
    {
        let ctx = canvas.context();
        ctx.set_fill_style(red());
        ctx.fill_path(path, FillRule::NonZero);
    }
    pixels(&mut canvas)
}

/// Fills `path` under the rule the path itself carries, which is what
/// `fill_path` overrides when it is given one.
fn filled_with_own_rule(path: &Path) -> Vec<u8> {
    let mut canvas = Canvas::new(30.0, 30.0);
    {
        let ctx = canvas.context();
        ctx.set_fill_style(red());
        ctx.fill_path(path, path.fill_rule());
    }
    pixels(&mut canvas)
}

/// Traces `trace` onto an untransformed context, fills it, and returns the
/// raw pixels -- the reference a built path is compared against.
fn traced(trace: impl FnOnce(&mut Context2D)) -> Vec<u8> {
    let mut canvas = Canvas::new(30.0, 30.0);
    {
        let ctx = canvas.context();
        ctx.set_fill_style(red());
        ctx.begin_path();
        trace(ctx);
        ctx.fill(FillRule::NonZero);
    }
    pixels(&mut canvas)
}

#[test]
fn a_built_path_is_the_path_the_same_svg_describes() {
    let mut builder = PathBuilder::new();
    builder
        .move_to(0.0, 0.0)
        .line_to(20.0, 0.0)
        .line_to(20.0, 20.0);
    builder.close_path();

    assert_eq!(
        filled(&builder.build(FillRule::NonZero)),
        filled(&triangle()),
        "built and parsed paths render identically"
    );
}

#[test]
fn a_segment_added_to_an_empty_builder_opens_a_subpath_there() {
    let mut implicit = PathBuilder::new();
    implicit
        .line_to(5.0, 5.0)
        .line_to(25.0, 5.0)
        .line_to(25.0, 25.0);

    let mut explicit = PathBuilder::new();
    explicit
        .move_to(5.0, 5.0)
        .line_to(25.0, 5.0)
        .line_to(25.0, 25.0);

    assert_eq!(
        filled(&implicit.build(FillRule::NonZero)),
        filled(&explicit.build(FillRule::NonZero)),
        "the first point is a move, not a line from nowhere"
    );
}

#[test]
fn a_built_arc_matches_the_arc_a_context_traces() {
    let mut builder = PathBuilder::new();
    builder.move_to(5.0, 15.0);
    builder
        .arc(15.0, 15.0, 8.0, 0.0, std::f32::consts::PI, false)
        .expect("positive radius");

    assert_eq!(
        filled(&builder.build(FillRule::NonZero)),
        traced(|ctx| {
            ctx.move_to(5.0, 15.0);
            ctx.arc(15.0, 15.0, 8.0, 0.0, std::f32::consts::PI, false)
                .expect("positive radius");
        }),
        "including the leading line an extended contour draws"
    );
}

#[test]
fn a_built_round_rect_matches_the_one_a_context_traces() {
    let mut builder = PathBuilder::new();
    builder
        .round_rect(5.0, 5.0, 20.0, 20.0, [6.0, 0.0, 6.0, 0.0])
        .expect("finite radii");

    assert_eq!(
        filled(&builder.build(FillRule::NonZero)),
        traced(|ctx| {
            ctx.round_rect(5.0, 5.0, 20.0, 20.0, [6.0, 0.0, 6.0, 0.0])
                .expect("finite radii");
        }),
        "same corners, same geometry"
    );
}

#[test]
fn a_round_rect_leaves_the_pen_at_the_start_corner() {
    // The contour's start index decides where a following segment attaches.
    // Pinned to 0, that is the top-left corner, so the line below runs
    // diagonally toward the origin; from any other corner it would not.
    let mut builder = PathBuilder::new();
    builder
        .round_rect(20.0, 20.0, 8.0, 8.0, [0.0; 4])
        .expect("finite radii");
    builder.line_to(2.0, 2.0);

    let mut canvas = Canvas::new(30.0, 30.0);
    {
        let ctx = canvas.context();
        ctx.set_stroke_style(red());
        ctx.set_line_width(3.0);
        ctx.stroke_path(&builder.build(FillRule::NonZero));
    }

    let buffer = pixels(&mut canvas);
    assert!(
        at(&buffer, 30, 11, 11)[3] > 0,
        "the tail runs from the top-left corner"
    );
    assert_eq!(
        at(&buffer, 30, 11, 24)[3],
        0,
        "not from the bottom-left one"
    );
}

#[test]
fn a_negative_size_rect_winds_the_other_way() {
    // Two rects wound the same way fill solid under NonZero; wound opposite,
    // the inner one punches a hole. Which happens is the whole observable
    // difference a negative dimension makes, and a browser makes it too.
    let mut same = PathBuilder::new();
    same.rect(2.0, 2.0, 26.0, 26.0).rect(10.0, 10.0, 10.0, 10.0);

    let mut opposed = PathBuilder::new();
    opposed
        .rect(2.0, 2.0, 26.0, 26.0)
        .rect(10.0, 20.0, 10.0, -10.0);

    let mut cancelled = PathBuilder::new();
    cancelled
        .rect(2.0, 2.0, 26.0, 26.0)
        .rect(20.0, 20.0, -10.0, -10.0);

    assert_eq!(
        at(&filled(&same.build(FillRule::NonZero)), 30, 15, 15)[3],
        255,
        "same winding fills through"
    );
    assert_eq!(
        at(&filled(&opposed.build(FillRule::NonZero)), 30, 15, 15)[3],
        0,
        "one negative dimension leaves a hole"
    );
    assert_eq!(
        at(&filled(&cancelled.build(FillRule::NonZero)), 30, 15, 15)[3],
        255,
        "two cancel back to the original winding"
    );
}

#[test]
fn a_negative_size_round_rect_winds_the_other_way_too() {
    let mut opposed = PathBuilder::new();
    opposed.rect(2.0, 2.0, 26.0, 26.0);
    opposed
        .round_rect(10.0, 20.0, 10.0, -10.0, [2.0; 4])
        .expect("finite radii");

    assert_eq!(
        at(&filled(&opposed.build(FillRule::NonZero)), 30, 15, 15)[3],
        0,
        "a rounded rectangle reverses on the same rule a plain one does"
    );
}

#[test]
fn add_path_starts_a_new_contour() {
    let square =
        Path::from_svg("M18 18 L26 18 L26 26 L18 26 Z", FillRule::NonZero)
            .expect("svg path");

    let mut builder = PathBuilder::new();
    builder
        .move_to(2.0, 2.0)
        .line_to(10.0, 2.0)
        .line_to(10.0, 10.0);
    builder.add_path(&square);

    let buffer = filled(&builder.build(FillRule::NonZero));
    assert_eq!(at(&buffer, 30, 8, 5)[3], 255, "the first contour filled");
    assert_eq!(at(&buffer, 30, 22, 22)[3], 255, "the added one did too");
    assert_eq!(
        at(&buffer, 30, 14, 14)[3],
        0,
        "and nothing joined them across the gap"
    );
}

#[test]
fn build_snapshots_without_ending_the_build() {
    let mut builder = PathBuilder::new();
    builder
        .move_to(2.0, 2.0)
        .line_to(10.0, 2.0)
        .line_to(10.0, 10.0);
    let first = builder.build(FillRule::NonZero);

    builder.add_path(
        &Path::from_svg("M18 18 L26 18 L26 26 Z", FillRule::NonZero)
            .expect("svg path"),
    );
    let second = builder.build(FillRule::NonZero);

    assert_eq!(
        at(&filled(&first), 30, 22, 21)[3],
        0,
        "the earlier snapshot is untouched by later segments"
    );
    assert_eq!(
        at(&filled(&second), 30, 22, 21)[3],
        255,
        "the later one has them"
    );
}

#[test]
fn build_applies_the_fill_rule_it_is_given() {
    // A doubly-wound ring: solid under NonZero, hollow under EvenOdd.
    let mut builder = PathBuilder::new();
    builder
        .rect(2.0, 2.0, 26.0, 26.0)
        .rect(8.0, 8.0, 14.0, 14.0);

    let non_zero = builder.build(FillRule::NonZero);
    let even_odd = builder.build(FillRule::EvenOdd);

    assert_eq!(non_zero.fill_rule(), FillRule::NonZero);
    assert_eq!(even_odd.fill_rule(), FillRule::EvenOdd);
    assert_eq!(
        at(&filled_with_own_rule(&non_zero), 30, 15, 15)[3],
        255,
        "filled through"
    );
    assert_eq!(
        at(&filled_with_own_rule(&even_odd), 30, 15, 15)[3],
        0,
        "hollow"
    );
}

#[test]
fn an_arc_rejects_a_radius_it_cannot_draw() {
    // Every sibling rejected a negative radius and these two drew something
    // anyway; a browser throws for all of them.
    let mut canvas = Canvas::new(40.0, 40.0);
    let ctx = canvas.context();

    assert_eq!(
        ctx.arc(20.0, 20.0, -5.0, 0.0, 1.0, false).err(),
        Some(Error::InvalidRect {
            rect: Rect {
                left: 25.0,
                top: 25.0,
                right: 15.0,
                bottom: 15.0,
            },
        }),
        "a negative radius, reported as the ellipse asked for"
    );
    assert!(
        ctx.arc(20.0, 20.0, f32::NAN, 0.0, 1.0, false).is_err(),
        "a non-finite one"
    );
    assert!(
        ctx.ellipse(20.0, 20.0, 5.0, -5.0, 0.0, 0.0, 1.0, false)
            .is_err(),
        "either radius of an ellipse"
    );
    assert!(
        ctx.ellipse(20.0, 20.0, 5.0, 5.0, 0.0, 0.0, 1.0, false)
            .is_ok(),
        "and a usable pair still draws"
    );

    let mut builder = PathBuilder::new();
    assert!(
        builder.arc(20.0, 20.0, -5.0, 0.0, 1.0, false).is_err(),
        "the builder rejects the same radius"
    );
    assert!(
        builder
            .ellipse(20.0, 20.0, 5.0, -5.0, 0.0, 0.0, 1.0, false)
            .is_err()
    );
    assert!(
        builder.build(FillRule::NonZero).bounds().right <= 0.0,
        "and neither added geometry"
    );
}

#[test]
fn a_radius_a_context_rejects_the_builder_rejects_too() {
    let mut builder = PathBuilder::new();
    builder.move_to(5.0, 5.0);

    assert_eq!(
        builder.arc_to(20.0, 5.0, 20.0, 20.0, -1.0).err(),
        Some(Error::InvalidRect {
            rect: Rect {
                left: 20.0,
                top: 5.0,
                right: 20.0,
                bottom: 20.0,
            },
        }),
        "a negative arc_to radius"
    );
    assert!(
        builder.arc_to(20.0, 5.0, 20.0, 20.0, f32::NAN).is_err(),
        "a non-finite one"
    );
    assert!(
        builder
            .round_rect(5.0, 5.0, 20.0, 20.0, [4.0, -4.0, 4.0, 4.0])
            .is_err(),
        "a negative corner radius"
    );
    assert!(
        builder.build(FillRule::NonZero).bounds().right <= 5.0,
        "and none of them added geometry"
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
    //
    // Asserted through the size of each exported page, since a count and a
    // `%PDF` header hold just as well when every page is 20x10 -- the very
    // thing this is named for.
    assert_eq!(canvas.page_count(), 2);
    for (index, width, height) in [(0usize, 40usize, 40usize), (1, 20, 10)] {
        let raw = canvas
            .to_buffer(
                ImageFormat::Raw,
                &EncodeOptions {
                    page: Some(index),
                    ..EncodeOptions::default()
                },
            )
            .expect("raw export");
        assert_eq!(
            raw.len(),
            width * height * 4,
            "page {index} is {width}x{height}"
        );
        assert_eq!(
            at(&raw, width as u32, width as u32 - 1, height as u32 - 1)[3],
            255,
            "and is filled to its own far corner"
        );
    }

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
    // The comma form is the one the JavaScript getter reports.
    assert_eq!(ctx.filter(), "drop-shadow(2px 3px 4px rgba(255,0,0,1))");
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

    // The colour's own alpha, not the byte it rounds to: a round trip
    // through 8 bits reported `0.5019608` for a half-alpha shadow.
    assert_eq!(ctx.filter(), "drop-shadow(0px 0px 1px rgba(255,0,0,0.5))");
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

    // A context that was never told an alignment lays out as Start does,
    // and `TextAlign::default()` says so too.
    assert_eq!(TextAlign::default(), TextAlign::Start);
    let untouched = |direction: TextDirection| {
        let mut canvas = Canvas::new(120.0, 40.0);
        {
            let ctx = canvas.context();
            ctx.set_direction(direction);
            ctx.set_fill_style(red());
            ctx.set_font(&Font::new("Helvetica", 20.0));
            ctx.fill_text("abc", 60.0, 28.0, None);
        }
        let buffer = pixels(&mut canvas);
        (0..120)
            .find(|&x| (0..40).any(|y| at(&buffer, 120, x, y)[3] > 0))
            .expect("text was painted")
    };
    for direction in [TextDirection::LeftToRight, TextDirection::RightToLeft] {
        assert_eq!(
            untouched(direction),
            ink_start(TextAlign::Start, direction),
            "a fresh context starts aligned to Start"
        );
    }

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
        )
        .expect("positive radii");
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
        GradientColorSpace::default(),
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
    // The method returns a constant, so asking once proves nothing beyond
    // that it compiles. What can be asserted is that the operations which
    // rebuild or resize a surface -- the ones that would invalidate a
    // browser's context -- leave it reporting the same thing.
    let mut canvas = Canvas::new(10.0, 10.0);
    assert!(
        !canvas.context().is_context_lost(),
        "there is no compositor to lose the surface to"
    );

    canvas.set_size(400.0, 400.0);
    canvas.set_gpu(false);
    canvas.new_page();
    canvas
        .context()
        .get_image_data(0.0, 0.0, 4.0, 4.0)
        .expect("readback");
    assert!(
        canvas
            .to_buffer(
                ImageFormat::Raw,
                &EncodeOptions {
                    page: Some(9),
                    ..EncodeOptions::default()
                }
            )
            .is_err(),
        "a failed export is a failure, not a lost context"
    );

    assert!(
        !canvas.context().is_context_lost(),
        "and none of that loses it"
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

    // Both arms asserting the same constant is exactly what a `set_gpu` that
    // did nothing produces, so the flag has to be observable too. Asking for
    // the CPU always gets it; asking for the GPU gets whatever the machine
    // has, which is why only one direction can be asserted outright.
    let mut canvas = Canvas::new(4.0, 4.0);
    canvas.set_gpu(false);
    assert!(!canvas.gpu(), "the request is what was asked for");
    assert_eq!(
        canvas.engine_kind(),
        EngineKind::Cpu,
        "and refusing the GPU always resolves to the raster backend"
    );

    canvas.set_gpu(true);
    assert!(canvas.gpu(), "and back again");
    assert_eq!(
        canvas.engine_kind(),
        Canvas::new(4.0, 4.0).engine_kind(),
        "asking for the GPU lands where a fresh canvas does -- which is the \
         CPU in a build with no GPU backend compiled in"
    );
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
fn a_translucent_fill_composites_the_way_a_browser_does() {
    // Canvas composites in the sRGB values themselves, not in linear light,
    // which is why the mix below is a plain interpolation of the encoded
    // channels. The four exact rows were read off Chrome 141 for the same
    // draw; the sweep around them allows two levels for Skia's rounding.
    let backdrop = [40u8, 90, 160];
    let painted = |source: [u8; 3], alpha: f32| {
        let mut canvas = Canvas::new(4.0, 4.0);
        // Pinned to the raster backend: a GPU rounds a level differently and
        // the exact rows below would be a coin toss.
        canvas.set_gpu(false);
        let ctx = canvas.context();
        ctx.set_fill_style(RgbaLinear::from_srgb8(
            backdrop[0],
            backdrop[1],
            backdrop[2],
            1.0,
        ));
        ctx.fill_rect(0.0, 0.0, 4.0, 4.0);
        ctx.set_fill_style(RgbaLinear::from_srgb8(
            source[0], source[1], source[2], alpha,
        ));
        ctx.fill_rect(0.0, 0.0, 4.0, 4.0);
        let data = ctx.get_image_data(0.0, 0.0, 1.0, 1.0).expect("readback");
        let px = data.pixels();
        [px[0], px[1], px[2], px[3]]
    };

    for (alpha, browser) in [
        (0.05, [47u8, 88, 153, 255]),
        (0.25, [80, 82, 128, 255]),
        (0.5, [120, 75, 95, 255]),
        (0.75, [160, 67, 62, 255]),
    ] {
        assert_eq!(
            painted([200, 60, 30], alpha),
            browser,
            "alpha {alpha} against the browser"
        );
    }

    for source in [[200u8, 60, 30], [0, 0, 0], [255, 255, 255], [12, 200, 190]]
    {
        for step in 0..=20u32 {
            let alpha = step as f32 / 20.0;
            let px = painted(source, alpha);
            for channel in 0..3 {
                let mixed = f32::from(source[channel]) * alpha
                    + f32::from(backdrop[channel]) * (1.0 - alpha);
                assert!(
                    (f32::from(px[channel]) - mixed).abs() <= 2.0,
                    "{source:?} at alpha {alpha}, channel {channel}: \
                     painted {} against {mixed}",
                    px[channel]
                );
            }
            assert_eq!(px[3], 255, "over an opaque backdrop");
        }
    }
}

#[test]
fn texture_reports_the_period_it_draws() {
    let stipple = Texture::new(&TextureOptions {
        path: Some(
            Path::from_svg("M0 0 L2 0 L2 2 Z", FillRule::NonZero)
                .expect("path"),
        ),
        spacing: (6.0, 9.0),
        ..TextureOptions::default()
    });
    assert_eq!(stipple.spacing(), (6.0, 9.0), "a stamped tile keeps both");

    // A line tile has one period, and the renderer takes the wider one. The
    // reader used to hand back the pair as given, describing a grid that was
    // never drawn -- and the draw path sized its lattice off that same pair,
    // so a narrow first component magnified a grid that did not need it.
    let hatch = Texture::new(&TextureOptions {
        spacing: (6.0, 9.0),
        ..TextureOptions::default()
    });
    assert_eq!(hatch.spacing(), (9.0, 9.0));
}

#[test]
fn a_line_texture_draws_its_wider_period_on_both_axes() {
    let hatched = |spacing: (f32, f32)| {
        let mut canvas = Canvas::new(100.0, 100.0);
        {
            let ctx = canvas.context();
            let hatch = Texture::new(&TextureOptions {
                spacing,
                color: red(),
                ..TextureOptions::default()
            });
            ctx.set_fill_texture(&hatch);
            ctx.fill_rect(0.0, 0.0, 100.0, 100.0);
        }
        pixels(&mut canvas)
    };

    let square = hatched((12.0, 12.0));
    for spacing in [(4.0, 12.0), (12.0, 4.0), (0.5, 12.0), (12.0, 0.5)] {
        assert!(
            hatched(spacing) == square,
            "{spacing:?} draws what (12, 12) draws"
        );
    }

    // Not vacuous: a different period is a different drawing.
    assert!(
        hatched((8.0, 8.0)) != square,
        "and a real change still shows"
    );
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

    // A bad origin is a bad rectangle; a bad extent is a bad dimension. The
    // payload has to carry the value that was wrong, and this reported an
    // `InvalidDimensions` holding the width and height -- both of them fine
    // -- while saying nothing about the x that was NaN.
    for (x, y, w, h) in [
        (f32::NAN, 0.0, 4.0, 4.0),
        (0.0, f32::NAN, 4.0, 4.0),
        (f32::NEG_INFINITY, 0.0, 4.0, 4.0),
    ] {
        let Err(Error::InvalidRect { rect }) = ctx.get_image_data(x, y, w, h)
        else {
            panic!("({x}, {y}, {w}, {h}) must be rejected as a rect");
        };
        assert!(
            !rect.left.is_finite() || !rect.top.is_finite(),
            "the rect carries the origin that was wrong, got {rect:?}"
        );
    }

    for (x, y, w, h) in [
        (0.0, 0.0, f32::INFINITY, 4.0),
        (0.0, 0.0, 4.0, f32::NEG_INFINITY),
    ] {
        let Err(Error::InvalidDimensions { width, height }) =
            ctx.get_image_data(x, y, w, h)
        else {
            panic!("({x}, {y}, {w}, {h}) must be rejected as dimensions");
        };
        assert!(
            !width.is_finite() || !height.is_finite(),
            "and the dimensions carry the extent that was, got {width}x{height}"
        );
    }

    assert!(
        ctx.get_image_data(
            f32::NEG_INFINITY,
            f32::NEG_INFINITY,
            f32::INFINITY,
            f32::INFINITY,
        )
        .is_err(),
        "and all four at once is still rejected"
    );
}

#[test]
fn an_unparseable_svg_path_reports_what_it_was_given() {
    let Err(Error::InvalidSvgPath { reason }) =
        Path::from_svg("not a path", FillRule::NonZero)
    else {
        panic!("junk should not parse as a path");
    };
    assert!(
        reason.contains("not a path"),
        "the reason quotes the input, got {reason:?}"
    );

    assert!(
        Path::from_svg("M0 0 L4 4", FillRule::NonZero).is_ok(),
        "and a real one still parses"
    );
}

#[test]
fn a_backdrop_filter_reads_what_is_already_on_the_page() {
    // `save_layer_with`'s third parameter had never been passed anything but
    // `None` anywhere in the suite.
    let sample = |backdrop: Option<&ImageFilter>| {
        let mut canvas = Canvas::new(30.0, 30.0);
        {
            let ctx = canvas.context();
            // A hard edge for the backdrop filter to blur across.
            ctx.set_fill_style(red());
            ctx.fill_rect(0.0, 0.0, 15.0, 30.0);

            ctx.save_layer_with(1.0, None, backdrop);
            ctx.restore();
        }
        at(&pixels(&mut canvas), 30, 16, 15)
    };

    let blur = ImageFilter::blur(6.0, 6.0, None).expect("blur");
    assert_eq!(sample(None)[3], 0, "the page is untouched without one");
    assert!(
        sample(Some(&blur))[3] > 0,
        "and the backdrop filter pulls the edge across the boundary"
    );
}

#[test]
fn a_projection_basis_maps_from_the_quad_it_names() {
    // `create_projection`'s optional `basis` had never been passed anything
    // anywhere in the suite, so every projection mapped from the default --
    // the canvas rectangle.
    let mut canvas = Canvas::new(40.0, 40.0);
    let ctx = canvas.context();

    let point = |x: f32, y: f32| Point { x, y };
    let target = [
        point(0.0, 0.0),
        point(20.0, 0.0),
        point(20.0, 20.0),
        point(0.0, 20.0),
    ];
    let page = [
        point(0.0, 0.0),
        point(40.0, 0.0),
        point(40.0, 40.0),
        point(0.0, 40.0),
    ];
    let half = [
        point(0.0, 0.0),
        point(20.0, 0.0),
        point(20.0, 20.0),
        point(0.0, 20.0),
    ];

    let implicit = ctx
        .create_projection(target, None)
        .expect("a projection from the page");
    let explicit = ctx
        .create_projection(target, Some(page))
        .expect("the same, said out loud");
    assert_eq!(
        implicit.values, explicit.values,
        "the default basis is the canvas rectangle"
    );

    let from_half = ctx
        .create_projection(target, Some(half))
        .expect("a projection from a quarter of the page");
    assert_ne!(
        implicit.values, from_half.values,
        "and a different basis is a different projection"
    );
}

#[test]
fn fill_text_condenses_a_run_into_its_max_width() {
    // The third argument to `fill_text` was `None` at every call site.
    let inked = |max_width: Option<f32>| {
        let mut canvas = Canvas::new(200.0, 40.0);
        {
            let ctx = canvas.context();
            ctx.set_fill_style(red());
            ctx.set_font(&Font::new("Helvetica", 24.0));
            ctx.fill_text("wide enough to squeeze", 4.0, 28.0, max_width);
        }
        let buffer = pixels(&mut canvas);
        (0..200)
            .rev()
            .find(|&x| (0..40).any(|y| at(&buffer, 200, x, y)[3] > 0))
            .expect("text was painted")
    };

    let natural = inked(None);
    assert!(natural > 60, "the run is wider than the cap below");
    assert!(
        inked(Some(60.0)) <= 66,
        "a capped run ends within its width, ended at {}",
        inked(Some(60.0))
    );
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
fn projection_keeps_the_row_get_transform_drops() {
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

    let read_back = ctx.projection();
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
fn put_image_data_accepts_every_layout_it_can_produce() {
    // This asserted that one default buffer writes, under a name about the
    // error path. `Error::PixelWrite` fires when Skia declines the
    // `ImageInfo`, and every `ExportedPixels` is tightly packed and
    // length-checked at construction, so no buffer the crate can hand out
    // reaches it -- the reachable risk is a depth or colour space that Skia
    // will not build an `ImageInfo` from, which is what this sweeps.
    let mut canvas = Canvas::new(20.0, 20.0);
    let ctx = canvas.context();

    for depth in [PixelDepth::Uint8, PixelDepth::F16, PixelDepth::F32] {
        for color_space in [
            PixelColorSpace::Srgb,
            PixelColorSpace::SrgbLinear,
            PixelColorSpace::DisplayP3,
            PixelColorSpace::Rec2020,
        ] {
            for premultiplied in [false, true] {
                let options = PixelExportOptions {
                    depth,
                    color_space,
                    premultiplied,
                };
                let Ok(mut patch) = ctx.create_image_data_as(2, 2, options)
                else {
                    // A combination this build cannot allocate never reaches
                    // the blit, and `create_image_data_as` is where it is
                    // reported.
                    continue;
                };
                patch.pixels_mut().fill(255);

                assert!(
                    ctx.put_image_data(&patch, 0.0, 0.0).is_ok(),
                    "{depth:?} / {color_space:?} / premultiplied \
                     {premultiplied} was declined"
                );
            }
        }
    }

    // And a write actually lands, rather than being accepted and dropped.
    // Eight-bit, because filling a float buffer with `0xff` bytes spells NaN
    // rather than white.
    let mut opaque = ctx.create_image_data(2, 2).expect("allocate");
    opaque.pixels_mut().fill(255);
    ctx.put_image_data(&opaque, 0.0, 0.0).expect("write");
    assert_eq!(
        at(&pixels(&mut canvas), 20, 1, 1),
        [255, 255, 255, 255],
        "the patch is on the canvas"
    );
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
    // `###f00` used to parse: the hash was stripped by
    // `trim_start_matches`, which takes as many as it finds.
    for bad in [
        "#not", "#12345", "", "#", "#1234567", "zzz", "###f00", "##f00",
    ] {
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
    //
    // Each page gets its own colour and is read back through the exporter,
    // because drawing into `page(0)` and never looking proves only that the
    // call returned something -- it passes just as well if every index
    // hands back the same page.
    for (index, blue) in [(0usize, 0.0_f32), (1, 0.5), (2, 1.0)] {
        let page = canvas.page(index).expect("an addressable page");
        page.set_fill_style(RgbaLinear::opaque(1.0, 0.0, blue));
        page.fill_rect(0.0, 0.0, 10.0, 10.0);
    }

    assert!(canvas.page(3).is_none(), "past the end is None");
    assert_eq!(canvas.page_count(), 3);

    for (index, blue) in [(0usize, 0u8), (1, 188), (2, 255)] {
        let buffer = canvas
            .to_buffer(
                ImageFormat::Raw,
                &EncodeOptions {
                    page: Some(index),
                    ..EncodeOptions::default()
                },
            )
            .expect("raw export");
        assert_eq!(
            at(&buffer, 10, 5, 5),
            [255, 0, blue, 255],
            "page {index} kept what was drawn into it"
        );
    }
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
fn a_pdf_ignores_the_page_index_only_when_it_has_pages_to_merge() {
    let mut canvas = Canvas::new(10.0, 10.0);
    canvas.context().fill_rect(0.0, 0.0, 10.0, 10.0);
    let past_the_end = EncodeOptions {
        page: Some(7),
        ..EncodeOptions::default()
    };

    // One page: the index is still checked, because the merge branch that
    // discards it is only taken when there is something to merge.
    assert!(
        canvas.to_buffer(ImageFormat::Pdf, &past_the_end).is_err(),
        "an index past the end of a one-page canvas"
    );

    canvas.new_page().fill_rect(0.0, 0.0, 10.0, 10.0);
    assert!(
        canvas
            .to_buffer(ImageFormat::Pdf, &past_the_end)
            .is_ok_and(|pdf| pdf.starts_with(b"%PDF")),
        "and past the end of a two-page one is ignored"
    );
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

#[test]
fn the_paint_source_readers_name_what_is_installed() {
    let mut canvas = Canvas::new(20.0, 20.0);
    let tile = quad_tile();
    let ctx = canvas.context();

    // An alpha of one, and of one half, survive the unpremultiplied form the
    // state keeps exactly; an arbitrary alpha rounds, as the reader says.
    let colour = RgbaLinear::opaque(1.0, 0.25, 0.5);
    let translucent = RgbaLinear::new_premultiplied(0.25, 0.125, 0.0625, 0.5);
    ctx.set_fill_style(colour);
    ctx.set_stroke_style(translucent);
    assert_eq!(ctx.fill_style(), PaintSource::Color(colour));
    assert_eq!(
        ctx.stroke_style(),
        PaintSource::Color(translucent),
        "the stroke has its own source"
    );

    let shader = Shader::linear_gradient(
        Point { x: 0.0, y: 0.0 },
        Point { x: 20.0, y: 0.0 },
        &[
            GradientStop {
                position: 0.0,
                color: red(),
            },
            GradientStop {
                position: 1.0,
                color: RgbaLinear::opaque(0.0, 0.0, 1.0),
            },
        ],
        GradientColorSpace::default(),
    )
    .expect("gradient");
    ctx.set_fill_shader(&shader);
    assert_eq!(ctx.fill_style(), PaintSource::Shader);
    assert!(
        matches!(ctx.stroke_style(), PaintSource::Color(_)),
        "and setting one leaves the other alone"
    );

    let pattern = ctx.create_pattern(&tile, PatternRepeat::Repeat);
    ctx.set_fill_pattern(&pattern);
    assert_eq!(ctx.fill_style(), PaintSource::Pattern);

    let hatch = Texture::new(&TextureOptions {
        spacing: (4.0, 4.0),
        color: red(),
        ..TextureOptions::default()
    });
    ctx.set_stroke_texture(&hatch);
    assert_eq!(ctx.stroke_style(), PaintSource::Texture);

    // And the readers follow the state stack, which is what a caller saving
    // and restoring around a change is relying on.
    ctx.save();
    ctx.set_fill_style(colour);
    assert_eq!(ctx.fill_style(), PaintSource::Color(colour));
    ctx.restore();
    assert_eq!(ctx.fill_style(), PaintSource::Pattern);
}

#[test]
fn the_remaining_state_readers_report_what_was_set() {
    let mut canvas = Canvas::new(20.0, 20.0);
    let ctx = canvas.context();

    assert_eq!(ctx.direction(), TextDirection::LeftToRight, "the default");
    ctx.set_direction(TextDirection::RightToLeft);
    assert_eq!(ctx.direction(), TextDirection::RightToLeft);

    assert_eq!(ctx.font_variant_caps(), FontVariantCaps::Normal);
    ctx.set_font_variant_caps(FontVariantCaps::AllSmallCaps);
    assert_eq!(ctx.font_variant_caps(), FontVariantCaps::AllSmallCaps);
    // Set through the other door: the reader derives the keyword from the
    // features themselves rather than from a copy kept beside them.
    ctx.set_font_variant(
        FontVariantCaps::PetiteCaps,
        &[FontFeature::on("onum")],
    );
    assert_eq!(ctx.font_variant_caps(), FontVariantCaps::PetiteCaps);
    ctx.set_font_variant_caps(FontVariantCaps::Normal);
    assert_eq!(
        ctx.font_variant_caps(),
        FontVariantCaps::Normal,
        "and the feature left alongside does not read as a caps variant"
    );

    assert_eq!(ctx.line_dash_fit(), DashFit::Turn, "the default");
    ctx.set_line_dash_fit(DashFit::Follow);
    assert_eq!(ctx.line_dash_fit(), DashFit::Follow);

    assert!(ctx.line_dash_marker().is_none(), "no marker by default");
    let marker =
        Path::from_svg("M0 0 L4 0 L4 4 Z", FillRule::NonZero).expect("path");
    ctx.set_line_dash_marker(Some(&marker));
    assert_eq!(
        ctx.line_dash_marker().expect("a marker").bounds().right,
        marker.bounds().right,
        "the marker that came back is the one that went in"
    );
    ctx.set_line_dash_marker(None);
    assert!(ctx.line_dash_marker().is_none(), "and it can be cleared");

    ctx.save();
    ctx.set_line_dash_fit(DashFit::Move);
    ctx.restore();
    assert_eq!(
        ctx.line_dash_fit(),
        DashFit::Follow,
        "restored with the state"
    );
}

#[test]
fn measured_line_count_follows_the_wrap_mode() {
    let mut canvas = Canvas::new(200.0, 200.0);
    let ctx = canvas.context();
    ctx.set_font(&Font::new("Helvetica", 16.0));

    // Wrapping off replaces newlines with spaces, so nothing can produce a
    // second line -- not even an explicit one.
    ctx.set_text_wrap(false);
    assert_eq!(ctx.measure_text("a\nb", None).line_count, 1);
    assert_eq!(
        ctx.measure_text("one two three four five", Some(40.0))
            .line_count,
        1
    );

    // Wrapping on honours an explicit newline even with no width given,
    // which the doc used to say was impossible.
    ctx.set_text_wrap(true);
    assert_eq!(ctx.measure_text("a\nb", None).line_count, 2);
    assert!(
        ctx.measure_text("one two three four five", Some(40.0))
            .line_count
            > 1
    );
}

/// Mean alpha over the painted square, times 1000, for a texture whose tile is
/// `tile` units across on a grid of `spacing`. Holding the ratio between the
/// two fixed holds the pattern's coverage fixed, whatever the scale.
fn texture_tone(tile: f32, spacing: f32) -> u32 {
    let mut canvas = Canvas::new(60.0, 60.0);
    {
        let ctx = canvas.context();
        let dots = Texture::new(&TextureOptions {
            path: Some(
                Path::from_svg(
                    &format!("M0 0 H{tile} V{tile} H0 Z"),
                    FillRule::NonZero,
                )
                .expect("tile path"),
            ),
            color: RgbaLinear::opaque(1.0, 0.0, 0.0),
            line: 0.0,
            spacing: (spacing, spacing),
            ..TextureOptions::default()
        });
        ctx.set_fill_texture(&dots);
        ctx.fill_rect(10.0, 10.0, 40.0, 40.0);
    }
    let buffer = pixels(&mut canvas);
    let total: u32 = (15..45)
        .flat_map(|y| (15..45).map(move |x| (x, y)))
        .map(|(x, y)| at(&buffer, 60, x, y)[3] as u32)
        .sum();
    total * 1000 / (30 * 30)
}

#[test]
fn a_sub_pixel_texture_grid_keeps_its_tone() {
    // The grid is magnified until its period clears a device pixel, which
    // only leaves the drawing alone because the tile is magnified with it.
    // Widening the period on its own would bound the work just as well and
    // wash the pattern out, so this holds the tile-to-period ratio fixed and
    // walks the scale down two decades past the raster.
    let resolvable = texture_tone(0.25, 1.0);
    assert!(resolvable > 0, "the reference pattern paints at all");

    for (tile, spacing) in [(0.05, 0.2), (0.02, 0.08), (0.005, 0.02)] {
        assert_eq!(
            texture_tone(tile, spacing),
            resolvable,
            "tile {tile} on a {spacing} grid holds the tone of the same \
             pattern at a resolvable scale",
        );
    }
}

#[test]
fn a_sub_pixel_texture_spacing_does_not_abort_the_process() {
    // `spacing: 0.001` on this page is on the order of 10^10 grid positions.
    // Before the grid was bounded, Skia took 29 GB and then called SK_ABORT
    // from SkContainers.cpp -- not a panic, and nothing `catch_unwind` could
    // have seen. Reaching the assertions at all is most of what this checks.
    let mut canvas = Canvas::new(60.0, 60.0);
    {
        let ctx = canvas.context();
        let hatch = Texture::new(&TextureOptions {
            color: RgbaLinear::opaque(1.0, 0.0, 0.0),
            line: 1.0,
            spacing: (0.001, 0.001),
            ..TextureOptions::default()
        });
        ctx.set_fill_texture(&hatch);
        ctx.fill_rect(10.0, 10.0, 40.0, 40.0);
    }

    // A mark as wide as its own period covers everything, at any scale.
    let buffer = pixels(&mut canvas);
    assert_eq!(at(&buffer, 60, 30, 30)[3], 255, "the fill is solid");
    assert_eq!(at(&buffer, 60, 5, 5)[3], 0, "and stays inside its rect");
}

#[test]
fn a_texture_under_a_tiny_transform_stays_bounded() {
    // Nothing is wrong with this texture -- it is the default spacing. The
    // transform alone is what drove the position count to 6.2 GB, which is
    // why no check on the texture itself could have caught it.
    let mut canvas = Canvas::new(60.0, 60.0);
    {
        let ctx = canvas.context();
        let stipple = Texture::new(&TextureOptions {
            path: Some(
                Path::from_svg("M0 0 H4 V4 H0 Z", FillRule::NonZero)
                    .expect("tile path"),
            ),
            color: RgbaLinear::opaque(1.0, 0.0, 0.0),
            line: 0.0,
            spacing: (8.0, 8.0),
            ..TextureOptions::default()
        });
        ctx.scale(0.002, 0.002);
        ctx.set_fill_texture(&stipple);
        ctx.fill_rect(5000.0, 5000.0, 20000.0, 20000.0);
    }

    let buffer = pixels(&mut canvas);
    assert!(at(&buffer, 60, 30, 30)[3] > 0, "the texture is drawn");
    assert_eq!(at(&buffer, 60, 5, 5)[3], 0, "and stays inside its rect");
}
