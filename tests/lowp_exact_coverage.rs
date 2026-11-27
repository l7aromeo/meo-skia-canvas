//! Coverage -- an antialiased edge's, a mask's, a clip's -- reaches the page
//! with each product correctly rounded to a byte, on every platform.
//!
//! Skia's eight-bit raster pipeline applies coverage through `div255`: the
//! exact two-step division on NEON, and `(v + 255) >> 8` elsewhere, a level
//! off for about a quarter of products. Before a blend that takes coverage as
//! alpha, source-over among them, it scales the colour by it (`scale_1_float`
//! for an edge, `scale_u8` for a mask, `scale_native` for a clip shader
//! under either); after one that cannot, `copy` among them, and for a clip
//! shader over a pixel the draw covers whole, it lerps from the backdrop
//! (`lerp_1_float`, `lerp_u8`, `lerp_native`). So the same edge landed apart
//! on x86 and arm64 (#836). `containers/skia-lowp-exact-coverage.patch`
//! divides each through `div255_accurate`.
//!
//! The references are that arithmetic written out, step by step in the order
//! Skia takes it, each product over 255 rounded once: under `copy`, a lerp
//! from the backdrop by each coverage in turn; under source-over, the colour
//! scaled by each coverage, then composited, whose backdrop share Skia's
//! eight-bit `srcover` already rounds exactly -- except where a draw covers
//! its pixel whole, which may come through `blitRect` and composite before
//! it lerps by the clip. An edge's or a mask's coverage is read from the same
//! draw in white under `copy` onto a transparent alpha-only page, where
//! `lerp(0, 255, c)` is `c` under either division, so it does not depend on
//! the patch it checks; a clip's is its column, as [`ramp`] holds it.
//!
//! A crate build's Skia does not carry the patch, and
//! `cfg(meo_lowp_exact_coverage)`, from `build.rs`, says whether this build
//! compiled it. Without it, off arm64, the test holds the build to bytes off
//! the rounded products instead, so the day crates.io's Skia applies coverage
//! exactly, the test says so.

use skia_safe::{
    AlphaType, BlendMode, BlurStyle, Color, Color4f, ColorSpace, ColorType,
    Data, ImageInfo, MaskFilter, Paint, Path, PathBuilder, Rect,
    SamplingOptions, Surface, images, surfaces,
};

/// Each side of the page.
const SIDE: i32 = 256;

/// Bytes in a pixel of an eight-bit RGBA raster.
const BYTES_PER_PIXEL: usize = 4;

/// The bytes of [`SIDE`] by [`SIDE`] RGBA pixels.
const AREA: usize = (SIDE * SIDE) as usize * BYTES_PER_PIXEL;

/// The colour each route draws, RGBA, unpremultiplied: translucent, so that
/// Skia keeps source-over rather than drawing it as `copy`, which it does for
/// an opaque colour, and chosen so that every premultiplied channel is a whole
/// byte -- see [`INK`].
const INK_UNPREMULTIPLIED: [u8; 4] = [255, 85, 0, 153];

/// [`INK_UNPREMULTIPLIED`] premultiplied: 255 and 85 times 153 over 255 are
/// whole, so no rounding of the premultiplication moves a reference.
const INK: [u8; 4] = [153, 51, 0, 153];

/// How many discs the page holds along each side.
const DISCS: i32 = 16;

/// How far a blurred mask's edge spreads, as a Gaussian sigma in pixels.
const BLUR_SIGMA: f32 = 1.5;

/// Whether this build's Skia applies coverage exactly: with the patch, or on
/// arm64, where `div255` is already the exact division.
const EXACT: bool =
    cfg!(meo_lowp_exact_coverage) || cfg!(target_arch = "aarch64");

/// `n / 255` rounded to the nearest integer. 255 is odd, so no `n` falls
/// halfway and the direction of a tie never matters.
fn over_255(n: u32) -> u32 {
    (2 * n + 255) / 510
}

/// The eight-bit colour type that is not this platform's N32 one, so that
/// Skia draws onto the page through its raster pipeline rather than the N32
/// blitters it keeps for source-over.
fn not_n32() -> ColorType {
    match ColorType::N32 {
        ColorType::RGBA8888 => ColorType::BGRA8888,
        _ => ColorType::RGBA8888,
    }
}

/// [`DISCS`] by [`DISCS`] discs, each off the pixel grid by its own
/// fraction, so that their edges cover pixels by every amount.
fn discs() -> Path {
    let cell = SIDE as f32 / DISCS as f32;
    let mut path = PathBuilder::new();
    for i in 0..DISCS * DISCS {
        let (col, row) = ((i % DISCS) as f32, (i / DISCS) as f32);
        let fraction = i as f32 / (DISCS * DISCS) as f32;
        path.add_circle(
            (
                cell * (col + 0.5) + fraction,
                cell * (row + 0.5) + fraction / 3.0,
            ),
            cell * 0.4 - fraction,
            None,
        );
    }
    path.detach()
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

/// A page of [`not_n32`] holding [`backdrop`].
fn page() -> Surface {
    let info = ImageInfo::new(
        (SIDE, SIDE),
        not_n32(),
        AlphaType::Premul,
        ColorSpace::new_srgb(),
    );
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

/// One way coverage reaches the blitter.
#[derive(Clone, Copy)]
struct Route {
    /// What the route is called in a report.
    name: &'static str,
    /// The discs' antialiased edge, its mask blurred where `Some(true)`, or
    /// `None` for a rectangle covering the page, which has no edge.
    edge: Option<bool>,
    /// Whether the draw is clipped by [`ramp`] as a clip shader.
    clipped: bool,
}

/// The routes. Under source-over Skia scales by an edge's or a mask's
/// coverage before the blend and lerps by a clip shader's after it where the
/// draw covers its pixel, and scales by both where it does not; under `copy`
/// it lerps by each.
const ROUTES: [Route; 4] = [
    // `blitAntiH`: `scale_1_float` or `lerp_1_float`.
    Route {
        name: "an antialiased edge",
        edge: Some(false),
        clipped: false,
    },
    // `blitMask` with an A8 mask: `scale_u8` or `lerp_u8`.
    Route {
        name: "a blurred mask",
        edge: Some(true),
        clipped: false,
    },
    // `blitRect` under a clip shader: `lerp_native`.
    Route {
        name: "a clip shader",
        edge: None,
        clipped: true,
    },
    // `blitAntiH` under a clip shader: `scale_1_float` and `scale_native`,
    // or `lerp_1_float` and `lerp_native`.
    Route {
        name: "an antialiased edge in a clip shader",
        edge: Some(false),
        clipped: true,
    },
];

/// Draws `route` with `paint` onto `page`.
fn draw(route: Route, page: &mut Surface, paint: &Paint) {
    let canvas = page.canvas();
    canvas.save();
    if route.clipped {
        canvas.clip_shader(
            ramp()
                .to_shader(None, SamplingOptions::default(), None)
                .expect("a shader"),
            None,
        );
    }
    let mut paint = paint.clone();
    match route.edge {
        Some(blurred) => {
            if blurred {
                paint.set_mask_filter(MaskFilter::blur(
                    BlurStyle::Normal,
                    BLUR_SIGMA,
                    false,
                ));
            }
            canvas.draw_path(&discs(), &paint);
        }
        None => {
            paint.set_anti_alias(false);
            let side = SIDE as f32;
            canvas.draw_rect(Rect::from_wh(side, side), &paint);
        }
    }
    canvas.restore();
}

/// An alpha-only image whose every pixel in column `x` is `x`: the coverage
/// [`Route::clipped`] clips by.
fn ramp() -> skia_safe::Image {
    let info = ImageInfo::new(
        (SIDE, SIDE),
        ColorType::Alpha8,
        AlphaType::Premul,
        None,
    );
    let pixels: Vec<u8> = (0..SIDE * SIDE).map(|i| (i % SIDE) as u8).collect();
    images::raster_from_data(&info, Data::new_copy(&pixels), SIDE as usize)
        .expect("an alpha image")
}

/// An antialiased paint of `colour` under `blend`.
fn paint(colour: Color, blend: BlendMode) -> Paint {
    let mut paint = Paint::default();
    paint.set_anti_alias(true);
    paint.set_color(colour);
    paint.set_blend_mode(blend);
    paint
}

/// The coverage of `route`'s edge or mask at each pixel, without its clip:
/// what it draws in white under `copy` onto a transparent alpha-only page.
fn edge_coverage(route: Route) -> Vec<u8> {
    let info = ImageInfo::new(
        (SIDE, SIDE),
        ColorType::Alpha8,
        AlphaType::Premul,
        None,
    );
    let mut plane = surfaces::raster(&info, None, None).expect("an alpha page");
    plane.canvas().clear(Color4f::new(0.0, 0.0, 0.0, 0.0));
    let unclipped = Route {
        clipped: false,
        ..route
    };
    draw(unclipped, &mut plane, &paint(Color::WHITE, BlendMode::Src));
    let mut read = vec![0; (SIDE * SIDE) as usize];
    assert!(
        plane.read_pixels(&info, &mut read, SIDE as usize, (0, 0)),
        "the coverage read"
    );
    read
}

/// A product over 255, rounded once.
fn scale(s: u32, coverage: u32) -> u32 {
    over_255(s * coverage)
}

/// `d` lerped to `to` by `coverage`, rounded once.
fn lerp(d: u32, to: u32, coverage: u32) -> u32 {
    over_255(d * (255 - coverage) + to * coverage)
}

/// Source-over of the premultiplied `s` onto `d`, the backdrop's share
/// rounded once, as Skia's eight-bit `srcover` takes it.
fn source_over(s: [u32; 4], d: [u32; 4]) -> [u32; 4] {
    [0, 1, 2, 3].map(|k| s[k] + over_255(d[k] * (255 - s[3])))
}

/// What a route under `blend` may leave on a backdrop pixel `d` where its
/// edge covers `e` and its clip `c`, each step rounded where Skia's eight-bit
/// pipeline rounds it: one result, or two where Skia may take either of two
/// orders.
///
/// Under source-over Skia scales by the edge's coverage and the clip's before
/// it composites. A pixel the edge covers whole may instead come through
/// `blitRect`, which composites and then lerps by the clip: the scan converter
/// hands a whole pixel over in a run of its own or inside a partly covered
/// span, and which is not the blitter's to choose. Both are correctly rounded.
fn expected(blend: BlendMode, e: u32, c: u32, d: [u32; 4]) -> [[u32; 4]; 2] {
    let s = INK.map(u32::from);
    match blend {
        BlendMode::SrcOver => {
            let scaled = source_over(s.map(|v| scale(scale(v, e), c)), d);
            match e {
                255 => {
                    let over = source_over(s, d);
                    [[0, 1, 2, 3].map(|k| lerp(d[k], over[k], c)), scaled]
                }
                _ => [scaled; 2],
            }
        }
        _ => [[0, 1, 2, 3].map(|k| lerp(d[k], lerp(d[k], s[k], e), c)); 2],
    }
}

/// What one route under one blend did: how many channels landed off the
/// reference, the worst by how much, and how many pixels were partly
/// covered.
#[derive(Debug)]
struct Read {
    off: usize,
    worst: u8,
    partly: usize,
}

/// `route` drawn in [`INK`] under `blend` onto [`page`], against
/// [`expected`].
fn read(route: Route, blend: BlendMode) -> Read {
    let [r, g, b, a] = INK_UNPREMULTIPLIED;
    let mut page = page();
    draw(
        route,
        &mut page,
        &paint(Color::from_argb(a, r, g, b), blend),
    );
    let mut pixels = vec![0; AREA];
    assert!(
        page.read_pixels(
            &rgba(),
            &mut pixels,
            SIDE as usize * BYTES_PER_PIXEL,
            (0, 0),
        ),
        "the page read"
    );
    let edges = match route.edge {
        Some(_) => edge_coverage(route),
        None => vec![u8::MAX; (SIDE * SIDE) as usize],
    };
    let back = backdrop();
    let (mut off, mut worst, mut partly) = (0, 0, 0);
    for (i, &e) in edges.iter().enumerate() {
        let c = match route.clipped {
            true => (i as i32 % SIDE) as u8,
            false => u8::MAX,
        };
        partly += usize::from(e.min(c) != 0 && (e, c) != (u8::MAX, u8::MAX));
        let at = i * BYTES_PER_PIXEL;
        let d = [0, 1, 2, 3].map(|k| u32::from(back[at + k]));
        let got = &pixels[at..at + BYTES_PER_PIXEL];
        let wants = expected(blend, u32::from(e), u32::from(c), d);
        let want = wants
            .iter()
            .find(|want| (0..BYTES_PER_PIXEL).all(|k| got[k] == want[k] as u8))
            .unwrap_or(&wants[0]);
        for k in 0..BYTES_PER_PIXEL {
            let apart = got[k].abs_diff(want[k] as u8);
            off += usize::from(apart != 0);
            worst = worst.max(apart);
        }
    }
    Read { off, worst, partly }
}

/// Fewer pixels than this covered partly and a route has not exercised
/// enough products to say anything.
const PARTLY_COVERED: usize = 4_000;

#[test]
fn coverage_scales_and_lerps_with_each_product_rounded_once() {
    let blends = [
        ("source-over", BlendMode::SrcOver),
        ("copy", BlendMode::Src),
    ];
    let report: Vec<_> = ROUTES
        .iter()
        .flat_map(|&route| {
            blends.iter().map(move |&(blend_name, blend)| {
                (route.name, blend_name, read(route, blend))
            })
        })
        .collect();
    for (route, blend, read) in &report {
        eprintln!(
            "{route} under {blend}: {} channels off, worst {}, {} pixels \
             partly covered",
            read.off, read.worst, read.partly
        );
        assert!(
            read.partly > PARTLY_COVERED,
            "{route}: {} pixels partly covered",
            read.partly
        );
    }
    if EXACT {
        assert!(
            report.iter().all(|(.., read)| read.off == 0),
            "coverage applied apart from its rounded product: {report:?}"
        );
    } else {
        assert!(
            report.iter().all(|(.., read)| read.off > 0),
            "a route applied coverage exactly without the patch -- \
             crates.io's Skia may now round coverage exactly: {report:?}"
        );
    }
}
