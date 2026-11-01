//! A colour built into a texture or a CanvasKit filter before the page it
//! lands on is known paints there as a fill of the same colour does.
//!
//! Each was handed to Skia when it was built, and Skia converted it through
//! skcms's approximate transfer function: a texture's mark, a blend colour
//! filter and a drop shadow landed 6.3e-5 from a fill of their colour on an
//! sRGB F32 page, and up to 1.8e-4 on a linear, P3 or Rec2020 one. A fill
//! lands exactly on every float page -- see `paint_colour_exact.rs` and
//! `float_page_colour.rs` -- so it is the reference here.

use std::cell::RefCell;

use meo_skia_canvas::prelude::*;

/// Both engines. A float page asking for the GPU is drawn on the raster
/// backend where the GPU cannot composite in float -- see
/// `float_page_colour.rs`.
const ENGINES: [bool; 2] = [false, true];

/// Every space a float page can be in and land a fill exactly.
const SPACES: [PixelColorSpace; 4] = [
    PixelColorSpace::Srgb,
    PixelColorSpace::SrgbLinear,
    PixelColorSpace::DisplayP3,
    PixelColorSpace::Rec2020,
];

/// Both float depths.
const DEPTHS: [PixelDepth; 2] = [PixelDepth::F32, PixelDepth::F16];

/// The side of every page, in pixels.
const SIDE: f32 = 8.0;

fn page(space: PixelColorSpace, depth: PixelDepth, gpu: bool) -> Canvas {
    Canvas::with_options(
        2.0 * SIDE,
        SIDE,
        CanvasOptions {
            color_type: depth,
            color_space: space,
            gpu,
            ..CanvasOptions::default()
        },
    )
    .expect("a canvas")
}

/// The pixel at `(x, SIDE / 2)`, as F32 in the page's own space.
fn read_at(canvas: &mut Canvas, space: PixelColorSpace, x: f32) -> [f32; 4] {
    let data = canvas
        .context()
        .get_image_data_as(
            x,
            SIDE / 2.0,
            1.0,
            1.0,
            PixelExportOptions {
                depth: PixelDepth::F32,
                color_space: space,
                ..PixelExportOptions::default()
            },
        )
        .expect("a pixel");
    let (channels, _) = data.pixels().as_chunks::<4>();
    std::array::from_fn(|i| f32::from_le_bytes(channels[i]))
}

/// Nineteen greys, opaque and at half alpha, linear-light, and colours
/// that are not grey at other alphas, one outside sRGB.
fn colours() -> impl Iterator<Item = RgbaLinear> {
    let greys = [1.0f32, 0.5].into_iter().flat_map(|alpha| {
        (1..=19).map(move |i| {
            let grey = i as f32 / 20.0 * alpha;
            RgbaLinear::new_premultiplied(grey, grey, grey, alpha)
        })
    });
    let others = [
        RgbaLinear::from_srgb8(230, 26, 102, 1.0),
        RgbaLinear::from_srgb8(77, 179, 51, 0.3),
        RgbaLinear::from_srgb8(20, 120, 220, 0.8),
        RgbaLinear::from_css("color(display-p3 0.1 0.85 0.3 / 0.65)")
            .expect("a colour"),
    ];
    greys.chain(others)
}

/// A plain fill of `colour` over the left square.
fn fill(ctx: &mut Context2D, colour: RgbaLinear) {
    ctx.set_fill_style(colour);
    ctx.fill_rect(0.0, 0.0, SIDE, SIDE);
}

/// White over the left square, which a filter turns into its colour.
fn white(ctx: &mut Context2D) {
    ctx.set_fill_style(RgbaLinear::opaque(1.0, 1.0, 1.0));
    ctx.fill_rect(0.0, 0.0, SIDE, SIDE);
}

/// The cases of `spaces` x float depths x engines x greys where `draw`,
/// read at `x`, is not a fill of the grey read in the left square.
fn misses(
    spaces: &[PixelColorSpace],
    x: f32,
    draw: impl Fn(&mut Context2D, RgbaLinear),
) -> Vec<String> {
    misses_against(spaces, &DEPTHS, x, fill, draw)
}

/// [`misses`] at `depths`, against what `reference` draws in place of a
/// fill.
fn misses_against(
    spaces: &[PixelColorSpace],
    depths: &[PixelDepth],
    x: f32,
    reference: impl Fn(&mut Context2D, RgbaLinear),
    draw: impl Fn(&mut Context2D, RgbaLinear),
) -> Vec<String> {
    let mut missed = Vec::new();
    for (&space, depth, gpu) in spaces.iter().flat_map(|space| {
        depths
            .iter()
            .flat_map(move |&depth| ENGINES.map(|gpu| (space, depth, gpu)))
    }) {
        for colour in colours() {
            let mut want = page(space, depth, gpu);
            reference(want.context(), colour);
            let want = read_at(&mut want, space, SIDE / 2.0);
            let mut got = page(space, depth, gpu);
            draw(got.context(), colour);
            let got = read_at(&mut got, space, x);
            if got != want {
                missed.push(format!(
                    "{space:?} {depth:?} gpu {gpu}, {colour:?}: {got:?} for \
                     {want:?}"
                ));
            }
        }
    }
    missed
}

fn assert_none(missed: Vec<String>) {
    assert!(
        missed.is_empty(),
        "{} missed:\n{}",
        missed.len(),
        missed.join("\n")
    );
}

/// A mark that covers its whole tile, so the texture paints every pixel of
/// what it fills.
fn solid_texture(colour: RgbaLinear, outline: bool) -> Texture {
    let mark =
        Path2D::from_svg("M-1 -1H9V9H-1Z", FillRule::NonZero).expect("a path");
    Texture::new(&TextureOptions {
        path: Some(mark),
        color: colour,
        line: 0.0,
        spacing: (SIDE, SIDE),
        outline,
        ..TextureOptions::default()
    })
}

/// A texture's mark, clipped to the fill or traced into its outline, lands
/// as a fill of its colour on every float page.
#[test]
fn a_texture_s_mark_lands_as_a_fill_of_its_colour() {
    for outline in [false, true] {
        assert_none(misses(&SPACES, SIDE / 2.0, |ctx, colour| {
            ctx.set_fill_texture(&solid_texture(colour, outline));
            ctx.fill_rect(0.0, 0.0, SIDE, SIDE);
        }));
    }
}

/// The eight-bit pages that keep a recording for pixels, and so land a fill
/// exactly: a space other than sRGB that a colour converts into exactly.
const EIGHT_BIT_DUAL: [PixelColorSpace; 3] = [
    PixelColorSpace::SrgbLinear,
    PixelColorSpace::DisplayP3,
    PixelColorSpace::Rec2020,
];

/// A texture's mark lands as a fill of its colour on an eight-bit page that
/// keeps a recording for pixels too, translucent marks included, whose
/// colour and alpha the page places on the bytes their exact product rounds
/// to as it does a fill's.
#[test]
fn a_texture_s_mark_lands_as_a_fill_of_its_colour_on_an_eight_bit_dual_page() {
    for outline in [false, true] {
        assert_none(misses_against(
            &EIGHT_BIT_DUAL,
            &[PixelDepth::Uint8],
            SIDE / 2.0,
            fill,
            |ctx, colour| {
                ctx.set_fill_texture(&solid_texture(colour, outline));
                ctx.fill_rect(0.0, 0.0, SIDE, SIDE);
            },
        ));
    }
}

/// A blend colour filter in `copy`, set on the context or wrapped as an
/// image filter, lands as a fill of its colour on every float page.
#[test]
fn a_blend_colour_filter_lands_as_a_fill_of_its_colour() {
    assert_none(misses(&SPACES, SIDE / 2.0, |ctx, colour| {
        let filter = ColorFilter::blend(colour, BlendMode::Copy);
        ctx.set_color_filter(Some(&filter.expect("a filter")));
        white(ctx);
    }));
    assert_none(misses(&SPACES, SIDE / 2.0, |ctx, colour| {
        let filter = ColorFilter::blend(colour, BlendMode::SourceIn)
            .and_then(|cf| ImageFilter::from_color_filter(cf, None, None));
        ctx.set_image_filter(Some(&filter.expect("a filter")));
        white(ctx);
    }));
}

/// A drop shadow lands as a fill of its colour on every float page:
/// alone, and beside what casts it, read where it falls clear of that.
#[test]
fn a_drop_shadow_lands_as_a_fill_of_its_colour() {
    assert_none(misses(&SPACES, SIDE / 2.0, |ctx, colour| {
        let filter = ImageFilter::drop_shadow_only(
            0.0, 0.0, 0.0, 0.0, colour, None, None,
        );
        ctx.set_image_filter(Some(&filter.expect("a filter")));
        white(ctx);
    }));
    let shadowed = |ctx: &mut Context2D, colour| {
        let filter =
            ImageFilter::drop_shadow(SIDE, 0.0, 0.0, 0.0, colour, None, None);
        ctx.set_image_filter(Some(&filter.expect("a filter")));
        white(ctx);
    };
    assert_none(misses(&SPACES, SIDE * 1.5, shadowed));
    // And what casts it, over its shadow's offset, as it is.
    assert_none(misses_against(
        &SPACES,
        &DEPTHS,
        SIDE / 2.0,
        |ctx, _| white(ctx),
        shadowed,
    ));
}

/// A filter built over one holding a colour is built again for the page as
/// that one is: composed, interpolated, merged and offset. The blend is
/// composed outside an identity matrix rather than inside one, which would
/// clamp a colour outside sRGB to it, as CanvasKit's matrix filter does.
#[test]
fn a_filter_over_one_holding_a_colour_lands_as_a_fill_of_it() {
    let blend =
        |colour| ColorFilter::blend(colour, BlendMode::Copy).expect("a filter");
    let identity = || {
        ColorFilter::matrix(ColorMatrix::identity().into()).expect("a filter")
    };
    let colour_filters: [fn(ColorFilter, ColorFilter) -> ColorFilter; 2] = [
        |identity, blend| {
            ColorFilter::compose(blend, identity).expect("a filter")
        },
        |identity, blend| {
            ColorFilter::lerp(1.0, identity, blend).expect("a filter")
        },
    ];
    for build in colour_filters {
        assert_none(misses(&SPACES, SIDE / 2.0, |ctx, colour| {
            ctx.set_color_filter(Some(&build(identity(), blend(colour))));
            white(ctx);
        }));
    }
    let shadow = |colour| {
        ImageFilter::drop_shadow_only(0.0, 0.0, 0.0, 0.0, colour, None, None)
            .expect("a filter")
    };
    let image_filters: [fn(ImageFilter) -> ImageFilter; 3] = [
        |shadow| {
            let offset = ImageFilter::offset(0.0, 0.0, None, None);
            ImageFilter::compose(offset.expect("a filter"), shadow)
                .expect("a filter")
        },
        |shadow| {
            ImageFilter::merge(vec![Some(shadow)], None).expect("a filter")
        },
        |shadow| {
            ImageFilter::offset(0.0, 0.0, Some(shadow), None).expect("a filter")
        },
    ];
    for build in image_filters {
        assert_none(misses(&SPACES, SIDE / 2.0, |ctx, colour| {
            ctx.set_image_filter(Some(&build(shadow(colour))));
            white(ctx);
        }));
    }
}

/// One filter drawn onto a page in another space first still lands as a
/// fill of its colour on an sRGB one: what it builds for a page is kept
/// for that page alone.
#[test]
fn a_filter_drawn_onto_another_page_first_lands_on_an_srgb_one() {
    let colour = RgbaLinear::new_premultiplied(0.3, 0.3, 0.3, 1.0);
    let filter = ColorFilter::blend(colour, BlendMode::Copy).expect("a filter");
    let mut linear = page(PixelColorSpace::SrgbLinear, PixelDepth::F32, false);
    linear.context().set_color_filter(Some(&filter));
    white(linear.context());
    let mut want = page(PixelColorSpace::Srgb, PixelDepth::F32, false);
    fill(want.context(), colour);
    let mut got = page(PixelColorSpace::Srgb, PixelDepth::F32, false);
    got.context().set_color_filter(Some(&filter));
    white(got.context());
    assert_eq!(
        read_at(&mut got, PixelColorSpace::Srgb, SIDE / 2.0),
        read_at(&mut want, PixelColorSpace::Srgb, SIDE / 2.0)
    );
}

/// An opaque white image as large as the left square.
fn white_image() -> Image {
    let side = SIDE as u32;
    let bytes = vec![255u8; (side * side * 4) as usize];
    Image::from_pixels(
        &bytes,
        side,
        side,
        side as usize * 4,
        PixelExportOptions::default(),
    )
    .expect("an image")
}

/// An image drawn under a blend colour filter, and under a drop shadow
/// alone, lands as a fill of the filter's colour on every float page.
#[test]
fn an_image_under_a_filter_holding_a_colour_lands_as_a_fill_of_it() {
    let image = white_image();
    assert_none(misses(&SPACES, SIDE / 2.0, |ctx, colour| {
        let filter = ColorFilter::blend(colour, BlendMode::Copy);
        ctx.set_color_filter(Some(&filter.expect("a filter")));
        ctx.draw_image(&image, 0.0, 0.0);
    }));
    assert_none(misses(&SPACES, SIDE / 2.0, |ctx, colour| {
        let filter = ImageFilter::drop_shadow_only(
            0.0, 0.0, 0.0, 0.0, colour, None, None,
        );
        ctx.set_image_filter(Some(&filter.expect("a filter")));
        ctx.draw_image(&image, 0.0, 0.0);
    }));
    // Through a colour filter and an image filter both: drawn into a group
    // the page runs the image filter on, the image in it through the colour
    // filter.
    assert_none(misses(&SPACES, SIDE / 2.0, |ctx, colour| {
        let filter = ColorFilter::blend(colour, BlendMode::Copy);
        ctx.set_color_filter(Some(&filter.expect("a filter")));
        let offset = ImageFilter::offset(0.0, 0.0, None, None);
        ctx.set_image_filter(Some(&offset.expect("a filter")));
        ctx.draw_image(&image, 0.0, 0.0);
    }));
}

/// A layer's backdrop holding a colour lands as a fill of it on every
/// float page: a drop shadow alone of the white beneath the layer, over
/// that white, as a fill over it. On F32 alone: an F16 layer holds the
/// shadow at half precision and the composite over the white is rounded
/// again, a step from a fill rounded once.
#[test]
fn a_layer_s_backdrop_holding_a_colour_lands_as_a_fill_of_it() {
    let f32 = [PixelDepth::F32];
    let over_white = |ctx: &mut Context2D, colour| {
        white(ctx);
        fill(ctx, colour);
    };
    assert_none(misses_against(
        &SPACES,
        &f32,
        SIDE / 2.0,
        over_white,
        |ctx, colour| {
            white(ctx);
            let backdrop = ImageFilter::drop_shadow_only(
                0.0, 0.0, 0.0, 0.0, colour, None, None,
            )
            .expect("a filter");
            ctx.save_layer_with(1.0, None, Some(&backdrop));
            ctx.restore();
        },
    ));
}

/// Every blend mode the crate names.
const MODES: [BlendMode; 29] = [
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
    BlendMode::Lighter,
    BlendMode::Clear,
    BlendMode::Modulate,
    BlendMode::Destination,
];

/// A fill of `dst` through a blend filter in `colour` and `mode` paints
/// what a fill of `colour` composited over a fill of `dst` in that mode
/// does, in every mode, on an F32 page in every space: the filter blends
/// the colour it holds with what it filters as the page composites one
/// draw over another, and both land exactly. Opaque and translucent
/// colours over opaque and translucent ones, and a colour outside sRGB.
///
/// Not on F16, where the composite is no reference: the page stores the
/// fill beneath at half precision before blending over it, where the
/// filter blends what it filters as it comes and stores the result once,
/// so the two part by up to 2.1e-3 at a colour burn.
#[test]
fn a_blend_filter_blends_as_the_page_composites_in_every_mode() {
    let colours = [
        RgbaLinear::from_srgb8(230, 26, 102, 1.0),
        RgbaLinear::from_srgb8(77, 179, 51, 0.6),
        RgbaLinear::from_css("color(display-p3 0.1 0.85 0.3 / 0.8)")
            .expect("a colour"),
    ];
    let mut missed = Vec::new();
    let mut compared = 0;
    let depth = PixelDepth::F32;
    for (space, mode) in SPACES
        .iter()
        .flat_map(|&space| MODES.map(|mode| (space, mode)))
    {
        for (colour, dst) in colours
            .iter()
            .flat_map(|&colour| colours.iter().map(move |&dst| (colour, dst)))
        {
            // Skia builds no filter for a blend that changes nothing, which
            // `ColorFilter::blend` refuses.
            let Ok(filter) = ColorFilter::blend(colour, mode) else {
                continue;
            };
            compared += 1;
            let mut want = page(space, depth, false);
            fill(want.context(), dst);
            want.context().set_global_composite_operation(mode);
            fill(want.context(), colour);
            let want = read_at(&mut want, space, SIDE / 2.0);
            let mut got = page(space, depth, false);
            got.context().set_color_filter(Some(&filter));
            fill(got.context(), dst);
            let got = read_at(&mut got, space, SIDE / 2.0);
            let close = got
                .iter()
                .zip(&want)
                .all(|(got, want)| (got - want).abs() <= BLEND_TOLERANCE);
            if !close {
                missed.push(format!(
                    "{space:?} {mode:?}, {colour:?} over {dst:?}: \
                     {got:?} for {want:?}"
                ));
            }
        }
    }
    assert!(compared > SPACES.len() * MODES.len(), "{compared} compared");
    assert_none(missed);
}

/// How far a blend filter's pixel may lie from the composite it is held
/// to: two units in the last place at one. The page blends premultiplied
/// pixels and divides the alpha back out when read, and the filter's blend
/// rounds in another order; over every mode, space and colour here they
/// part by 2.4e-7 at most.
const BLEND_TOLERANCE: f32 = 2.0 * f32::EPSILON;

/// Oswald, which the text case sets its glyph in.
const OSWALD: &str = "tests/assets/fonts/Oswald/Oswald-VariableFont_wght.ttf";

/// The family the text case registers Oswald under.
const FAMILY: &str = "CanvasKitColourOswald";

/// Every pixel of `canvas`, as F32 in `space`.
fn read_all(canvas: &mut Canvas, space: PixelColorSpace) -> Vec<f32> {
    let data = canvas
        .context()
        .get_image_data_as(
            0.0,
            0.0,
            2.0 * SIDE,
            SIDE,
            PixelExportOptions {
                depth: PixelDepth::F32,
                color_space: space,
                ..PixelExportOptions::default()
            },
        )
        .expect("the pixels");
    let (channels, _) = data.pixels().as_chunks::<4>();
    channels.iter().map(|c| f32::from_le_bytes(*c)).collect()
}

/// Text in a colour through a blend filter in `copy` of that colour lands
/// as the text does in it unfiltered, on every float page: the filter
/// replaces each glyph's colour with its own, and both land exactly, the
/// glyphs' coverage the same since the paint's colour is. A glyph's
/// partly covered pixel holds its colour divided by the alpha the two reach
/// in another order, a unit in the last place apart; its alpha is the same.
#[test]
fn text_under_a_blend_filter_lands_as_the_text_in_its_colour() {
    FontLibrary::new()
        .use_paths(Some(FAMILY), [OSWALD])
        .expect("Oswald");
    let text = |ctx: &mut Context2D, colour: RgbaLinear| {
        assert!(ctx.set_font_css(&format!("8px {FAMILY}")), "a font");
        ctx.set_fill_style(colour);
        ctx.fill_text("Hm", 0.0, SIDE - 1.0, None);
    };
    let mut missed = Vec::new();
    for (&space, depth, colour) in SPACES.iter().flat_map(|space| {
        DEPTHS.into_iter().flat_map(move |depth| {
            colours().map(move |colour| (space, depth, colour))
        })
    }) {
        let mut want = page(space, depth, false);
        text(want.context(), colour);
        let mut got = page(space, depth, false);
        let filter =
            ColorFilter::blend(colour, BlendMode::Copy).expect("a filter");
        got.context().set_color_filter(Some(&filter));
        text(got.context(), colour);
        let (got, want) =
            (read_all(&mut got, space), read_all(&mut want, space));
        let held = got.chunks(4).zip(want.chunks(4)).all(|(got, want)| {
            got[3] == want[3]
                && got[..3]
                    .iter()
                    .zip(&want[..3])
                    .all(|(got, want)| (got - want).abs() <= BLEND_TOLERANCE)
        });
        if !held {
            missed.push(format!("{space:?} {depth:?} {colour:?}"));
        }
    }
    assert_none(missed);
}

/// A canvas drawn in through a blend filter lands as a fill of the filter's
/// colour, on every float page: drawn as a group, composited through the
/// filter built for the page's pixels.
#[test]
fn a_canvas_drawn_in_under_a_blend_filter_lands_as_a_fill_of_its_colour() {
    let source =
        RefCell::new(page(PixelColorSpace::Srgb, PixelDepth::F32, false));
    white(source.borrow_mut().context());
    assert_none(misses(&SPACES, SIDE / 2.0, |ctx, colour| {
        let filter = ColorFilter::blend(colour, BlendMode::Copy);
        ctx.set_color_filter(Some(&filter.expect("a filter")));
        ctx.draw_canvas_sized(&mut source.borrow_mut(), 0.0, 0.0, SIDE, SIDE);
    }));
    // Under a composite that reaches past the draw, which draws it as it
    // is rather than as a group, as a fill under the same composite.
    let copied = |ctx: &mut Context2D, colour| {
        ctx.set_global_composite_operation(BlendMode::Copy);
        fill(ctx, colour);
    };
    assert_none(misses_against(
        &SPACES,
        &DEPTHS,
        SIDE / 2.0,
        copied,
        |ctx, colour| {
            ctx.set_global_composite_operation(BlendMode::Copy);
            let filter = ColorFilter::blend(colour, BlendMode::Copy);
            ctx.set_color_filter(Some(&filter.expect("a filter")));
            ctx.draw_canvas_sized(
                &mut source.borrow_mut(),
                0.0,
                0.0,
                SIDE,
                SIDE,
            );
        },
    ));
}

/// A fill in a gradient through a blend filter in `copy` lands as a fill of
/// the filter's colour, on every float page: the filter replaces a dye that
/// is not a flat colour on the paint the page's pixels draw with too.
#[test]
fn a_gradient_under_a_blend_filter_lands_as_a_fill_of_its_colour() {
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
    let gradient = Shader::linear_gradient(
        Point::new(0.0, 0.0),
        Point::new(SIDE, 0.0),
        &stops,
        GradientInterpolation::default(),
    )
    .expect("a gradient");
    assert_none(misses(&SPACES, SIDE / 2.0, |ctx, colour| {
        let filter = ColorFilter::blend(colour, BlendMode::Copy);
        ctx.set_color_filter(Some(&filter.expect("a filter")));
        ctx.set_fill_shader(&gradient);
        ctx.fill_rect(0.0, 0.0, SIDE, SIDE);
    }));
}
