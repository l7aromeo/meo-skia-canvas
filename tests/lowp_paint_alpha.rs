//! A paint's alpha below one scales an image's premultiplied bytes with each
//! product correctly rounded to a byte, on every platform, before it is
//! composited.
//!
//! Skia's eight-bit raster pipeline applies a paint's alpha in
//! `scale_1_float`, which divides by 255 through `div255`: the exact two-step
//! division on NEON, and `(v + 255) >> 8` elsewhere, a level off for about a
//! quarter of products. So the same image drawn at the same alpha landed
//! apart on x86 and arm64 (#828). `containers/skia-lowp-paint-alpha.patch`
//! applies a paint's alpha through `scale_1_float_accurate`, the same stage
//! through `div255_accurate`, wherever Skia's CPU raster applies one: the
//! raster-pipeline sprite blitter, a shader's paint colour and `drawAtlas`.
//!
//! The reference is that arithmetic written out: each premultiplied channel
//! times the alpha byte, over 255, rounded; then `source-over` onto the
//! backdrop, rounded again, which the pipeline's `srcover` already does
//! exactly. It is the two roundings the eight-bit pipeline makes, not one
//! rounding of the whole composite, which no eight-bit intermediate can give.
//!
//! A crate build's Skia does not carry the patch, and
//! `cfg(meo_lowp_paint_alpha)`, from `build.rs`, says whether this build
//! compiled it. Without it, off arm64, the test holds the build to bytes off
//! the rounded product instead, so the day crates.io's Skia scales exactly,
//! the test says so.

use skia_safe::{
    AlphaType, BlendMode, ColorSpace, ColorType, Data, IRect, Image, ImageInfo,
    Paint, RSXform, Rect, SamplingOptions, Surface, images, surfaces,
};

/// Each side of the image and the page: one column for every source alpha,
/// one row for every backdrop value.
const SIDE: i32 = 256;

/// Bytes in a pixel of an eight-bit RGBA or BGRA raster.
const BYTES_PER_PIXEL: usize = 4;

/// The bytes of [`SIDE`] by [`SIDE`] pixels.
const AREA: usize = (SIDE * SIDE) as usize * BYTES_PER_PIXEL;

/// Whether this build's Skia scales by a paint's alpha exactly: with the
/// patch, or on arm64, where `div255` is already the exact division.
const EXACT: bool = cfg!(meo_lowp_paint_alpha) || cfg!(target_arch = "aarch64");

/// `n / 255` rounded to the nearest integer. 255 is odd, so no `n` falls
/// halfway and the direction of a tie never matters.
fn over_255(n: u32) -> u32 {
    (2 * n + 255) / 510
}

/// The eight-bit colour type that is not this platform's N32 one.
///
/// Skia draws an N32 image onto an N32 page at a whole-pixel offset through
/// its own sprite rows (`SkBlendARGB32`), which the patch does not change; an
/// image of the other order takes the raster pipeline, as every image the
/// library draws in a colour type of its own does.
fn not_n32() -> ColorType {
    match ColorType::N32 {
        ColorType::RGBA8888 => ColorType::BGRA8888,
        _ => ColorType::RGBA8888,
    }
}

/// An `SIDE` by `SIDE` premultiplied sRGB raster of `colour_type`, holding
/// for each column `a` the premultiplied colour `[a/2, a/3, a, a]`, red to
/// alpha.
fn source(colour_type: ColorType) -> Image {
    let swap = colour_type == ColorType::BGRA8888;
    let pixels: Vec<u8> = (0..SIDE * SIDE)
        .flat_map(|i| {
            let a = (i % SIDE) as u8;
            let [r, g, b] = [a / 2, a / 3, a];
            if swap { [b, g, r, a] } else { [r, g, b, a] }
        })
        .collect();
    let info = ImageInfo::new(
        (SIDE, SIDE),
        colour_type,
        AlphaType::Premul,
        ColorSpace::new_srgb(),
    );
    images::raster_from_data(
        &info,
        Data::new_copy(&pixels),
        SIDE as usize * BYTES_PER_PIXEL,
    )
    .expect("a raster image")
}

/// The opaque backdrop, RGBA: for each row `d`, `[d, 255 - d, d / 2, 255]`.
fn backdrop() -> Vec<u8> {
    (0..SIDE * SIDE)
        .flat_map(|i| {
            let d = (i / SIDE) as u8;
            [d, u8::MAX - d, d / 2, u8::MAX]
        })
        .collect()
}

/// RGBA, premultiplied, in sRGB: how the backdrop is written and the page
/// read.
fn rgba() -> ImageInfo {
    ImageInfo::new(
        (SIDE, SIDE),
        ColorType::RGBA8888,
        AlphaType::Premul,
        ColorSpace::new_srgb(),
    )
}

/// An N32 sRGB page holding [`backdrop`].
fn page() -> Surface {
    let info = ImageInfo::new_n32_premul((SIDE, SIDE), ColorSpace::new_srgb());
    let mut page = surfaces::raster(&info, None, None).expect("a raster page");
    assert!(
        page.canvas().write_pixels(
            &rgba(),
            &backdrop(),
            SIDE as usize * BYTES_PER_PIXEL,
            (0, 0),
        ),
        "the backdrop written"
    );
    page
}

/// What the page holds after [`source`] is drawn over [`backdrop`] at
/// `alpha`, RGBA: each channel scaled and rounded, then composited and
/// rounded.
fn expected(alpha: u8) -> Vec<u8> {
    let alpha = u32::from(alpha);
    let back = backdrop();
    (0..SIDE * SIDE)
        .flat_map(|i| {
            let a = (i % SIDE) as u32;
            let scaled = [a / 2, a / 3, a, a].map(|s| over_255(s * alpha));
            let inverse = u32::from(u8::MAX) - scaled[3];
            let at = i as usize * BYTES_PER_PIXEL;
            [0, 1, 2, 3].map(|c| {
                let d = u32::from(back[at + c]);
                (scaled[c] + over_255(d * inverse)) as u8
            })
        })
        .collect()
}

/// One of the ways Skia's CPU raster applies a paint's alpha to an image.
type Route = fn(&mut Surface, &Paint);

/// The routes, by name: each reaches a different site the patch changes.
const ROUTES: [(&str, Route); 3] = [
    // `SkRasterPipelineSpriteBlitter::setup`.
    ("an image drawn at a whole-pixel offset", |page, paint| {
        page.canvas()
            .draw_image(source(not_n32()), (0, 0), Some(paint));
    }),
    // `SkRasterPipelineBlitter`'s paint colour after a shader's stages.
    ("a rectangle filled with an image shader", |page, paint| {
        let mut paint = paint.clone();
        paint.set_shader(source(ColorType::N32).to_shader(
            None,
            SamplingOptions::default(),
            None,
        ));
        let side = SIDE as f32;
        page.canvas().draw_rect(Rect::from_wh(side, side), &paint);
    }),
    // `SkDraw::drawAtlas`.
    ("an image drawn as an atlas", |page, paint| {
        let image = source(ColorType::N32);
        page.canvas().draw_atlas(
            &image,
            &[RSXform::new(1.0, 0.0, (0.0, 0.0))],
            &[Rect::from(IRect::from_wh(SIDE, SIDE))],
            None,
            BlendMode::SrcOver,
            SamplingOptions::default(),
            None,
            paint,
        );
    }),
];

#[test]
fn a_paint_alpha_scales_an_image_with_each_product_rounded_once() {
    let mut report = Vec::new();
    for (name, route) in ROUTES {
        let (mut off, mut worst) = (0usize, 0u8);
        for alpha in 0..=u8::MAX {
            let mut page = page();
            let mut paint = Paint::default();
            paint.set_alpha(alpha);
            route(&mut page, &paint);
            let mut read = vec![0; AREA];
            assert!(
                page.read_pixels(
                    &rgba(),
                    &mut read,
                    SIDE as usize * BYTES_PER_PIXEL,
                    (0, 0),
                ),
                "the page read"
            );
            for (got, want) in read.iter().zip(expected(alpha)) {
                let apart = got.abs_diff(want);
                off += usize::from(apart != 0);
                worst = worst.max(apart);
            }
        }
        report.push((name, off, worst));
    }
    for (name, off, worst) in &report {
        eprintln!("{name}: {off} channels off, worst {worst}");
    }
    if EXACT {
        assert!(
            report.iter().all(|&(_, off, _)| off == 0),
            "a paint's alpha rounded apart from its product: {report:?}"
        );
    } else {
        assert!(
            report.iter().all(|&(_, off, _)| off > 0),
            "a route scaled exactly without the patch -- crates.io's Skia may \
             now round a paint's alpha exactly: {report:?}"
        );
    }
}
