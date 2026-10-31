//! A colour built into a texture or a CanvasKit filter before the page it
//! lands on is known paints there as a fill of the same colour does.
//!
//! Each was handed to Skia when it was built, and Skia converted it through
//! skcms's approximate transfer function: a texture's mark, a blend colour
//! filter and a drop shadow landed 6.3e-5 from a fill of their colour on an
//! sRGB F32 page, and up to 1.8e-4 on a linear, P3 or Rec2020 one. A fill
//! lands exactly on every float page -- see `paint_colour_exact.rs` and
//! `float_page_colour.rs` -- so it is the reference here.

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

/// Nineteen greys, opaque and at half alpha, linear-light.
fn greys() -> impl Iterator<Item = RgbaLinear> {
    [1.0f32, 0.5].into_iter().flat_map(|alpha| {
        (1..=19).map(move |i| {
            let grey = i as f32 / 20.0 * alpha;
            RgbaLinear::new_premultiplied(grey, grey, grey, alpha)
        })
    })
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
        for colour in greys() {
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
        "{} missed, first {}",
        missed.len(),
        missed[0]
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

/// A blend colour filter in `copy`, set on the context or wrapped as an
/// image filter, lands as a fill of its colour on an sRGB float page.
#[test]
fn a_blend_colour_filter_lands_as_a_fill_of_its_colour() {
    let srgb = [PixelColorSpace::Srgb];
    assert_none(misses(&srgb, SIDE / 2.0, |ctx, colour| {
        let filter = ColorFilter::blend(colour, BlendMode::Copy);
        ctx.set_color_filter(Some(&filter.expect("a filter")));
        white(ctx);
    }));
    assert_none(misses(&srgb, SIDE / 2.0, |ctx, colour| {
        let filter = ColorFilter::blend(colour, BlendMode::SourceIn)
            .and_then(|cf| ImageFilter::from_color_filter(cf, None, None));
        ctx.set_image_filter(Some(&filter.expect("a filter")));
        white(ctx);
    }));
}

/// A drop shadow lands as a fill of its colour on an sRGB float page:
/// alone, and beside what casts it, read where it falls clear of that.
#[test]
fn a_drop_shadow_lands_as_a_fill_of_its_colour() {
    let srgb = [PixelColorSpace::Srgb];
    assert_none(misses(&srgb, SIDE / 2.0, |ctx, colour| {
        let filter = ImageFilter::drop_shadow_only(
            0.0, 0.0, 0.0, 0.0, colour, None, None,
        );
        ctx.set_image_filter(Some(&filter.expect("a filter")));
        white(ctx);
    }));
    assert_none(misses(&srgb, SIDE * 1.5, |ctx, colour| {
        let filter =
            ImageFilter::drop_shadow(SIDE, 0.0, 0.0, 0.0, colour, None, None);
        ctx.set_image_filter(Some(&filter.expect("a filter")));
        white(ctx);
    }));
}

/// A filter built over one holding a colour is built again for the page as
/// that one is: composed, interpolated, merged and offset.
#[test]
fn a_filter_over_one_holding_a_colour_lands_as_a_fill_of_it() {
    let srgb = [PixelColorSpace::Srgb];
    let blend =
        |colour| ColorFilter::blend(colour, BlendMode::Copy).expect("a filter");
    let identity = || {
        ColorFilter::matrix(ColorMatrix::identity().into()).expect("a filter")
    };
    let colour_filters: [fn(ColorFilter, ColorFilter) -> ColorFilter; 2] = [
        |identity, blend| {
            ColorFilter::compose(identity, blend).expect("a filter")
        },
        |identity, blend| {
            ColorFilter::lerp(1.0, identity, blend).expect("a filter")
        },
    ];
    for build in colour_filters {
        assert_none(misses(&srgb, SIDE / 2.0, |ctx, colour| {
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
        assert_none(misses(&srgb, SIDE / 2.0, |ctx, colour| {
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
/// alone, lands as a fill of the filter's colour on an sRGB float page.
#[test]
fn an_image_under_a_filter_holding_a_colour_lands_as_a_fill_of_it() {
    let srgb = [PixelColorSpace::Srgb];
    let image = white_image();
    assert_none(misses(&srgb, SIDE / 2.0, |ctx, colour| {
        let filter = ColorFilter::blend(colour, BlendMode::Copy);
        ctx.set_color_filter(Some(&filter.expect("a filter")));
        ctx.draw_image(&image, 0.0, 0.0);
    }));
    assert_none(misses(&srgb, SIDE / 2.0, |ctx, colour| {
        let filter = ImageFilter::drop_shadow_only(
            0.0, 0.0, 0.0, 0.0, colour, None, None,
        );
        ctx.set_image_filter(Some(&filter.expect("a filter")));
        ctx.draw_image(&image, 0.0, 0.0);
    }));
}

/// A layer's backdrop holding a colour lands as a fill of it on an sRGB
/// float page: a drop shadow alone of the white beneath the layer, over
/// that white, as a fill over it. On F32 alone: an F16 layer holds the
/// shadow at half precision and the composite over the white is rounded
/// again, a step from a fill rounded once.
#[test]
fn a_layer_s_backdrop_holding_a_colour_lands_as_a_fill_of_it() {
    let (srgb, f32) = ([PixelColorSpace::Srgb], [PixelDepth::F32]);
    let over_white = |ctx: &mut Context2D, colour| {
        white(ctx);
        fill(ctx, colour);
    };
    assert_none(misses_against(
        &srgb,
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
