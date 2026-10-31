//! A `saveLayer` composites everything drawn in it as one, whatever is read
//! or exported while it is open: a read inside one sees it composited as it
//! stands and leaves it open, so what is drawn after the read lands in the
//! same layer. An SVG export rasterizes the layer and nothing else.

use meo_skia_canvas::prelude::*;

/// The page every case draws on: 60 by 20, white.
const WIDTH: f32 = 60.0;
const HEIGHT: f32 = 20.0;

/// The page where nothing is drawn on it.
const WHITE: [u8; 4] = [255; 4];

/// The fill every case draws in.
const RED: [u8; 4] = [255, 0, 0, 255];

/// Nothing at all, as `copy` leaves where an empty layer covers.
const CLEAR: [u8; 4] = [0; 4];

/// Red on white through a layer at alpha 0.5, exactly: green and blue are
/// half of 255.
const HALF_RED: [f32; 4] = [255.0, 127.5, 127.5, 255.0];

/// Red on white through two layers at 0.5, a quarter of the red.
const QUARTER_RED: [f32; 4] = [255.0, 191.25, 191.25, 255.0];

/// Red on white through three layers at 0.5, an eighth of the red.
const EIGHTH_RED: [f32; 4] = [255.0, 223.125, 223.125, 255.0];

/// Both engines, each held to the exact value.
const ENGINES: [bool; 2] = [false, true];

/// How far a pixel may be from the exact value it stands for: half a level,
/// the most that rounding to a byte can account for. At 127.5 both 127 and
/// 128 are within it.
const HALF_A_LEVEL: f32 = 0.5;

/// Whether `got` is what rounding `exact` to bytes could give.
fn rounds_to(got: &[u8], exact: [f32; 4]) -> bool {
    got.iter()
        .zip(exact)
        .all(|(a, b)| (f32::from(*a) - b).abs() <= HALF_A_LEVEL)
}

fn white_page(gpu: bool) -> Canvas {
    let mut canvas = Canvas::with_options(
        WIDTH,
        HEIGHT,
        CanvasOptions {
            gpu,
            ..CanvasOptions::default()
        },
    )
    .expect("a canvas");
    let ctx = canvas.context();
    ctx.set_fill_style(RgbaLinear::opaque(1.0, 1.0, 1.0));
    ctx.fill_rect(0.0, 0.0, WIDTH, HEIGHT);
    ctx.set_fill_style(RgbaLinear::opaque(1.0, 0.0, 0.0));
    canvas
}

/// A red square in the `n`th third of the page.
fn square(ctx: &mut Context2D, n: usize) {
    ctx.fill_rect(20.0 * n as f32, 0.0, 20.0, HEIGHT);
}

/// The middle pixel of the `n`th third of the page.
fn pixel(canvas: &mut Canvas, n: usize) -> Vec<u8> {
    canvas
        .context()
        .get_image_data(20.0 * n as f32 + 10.0, 10.0, 1.0, 1.0)
        .expect("a pixel")
        .pixels()
        .to_vec()
}

#[test]
fn a_read_inside_a_layer_leaves_it_open() {
    for gpu in ENGINES {
        let mut canvas = white_page(gpu);
        let ctx = canvas.context();
        ctx.save_layer_with(0.5, None, None);
        square(ctx, 0);
        let inside = pixel(&mut canvas, 0);
        let ctx = canvas.context();
        square(ctx, 1);
        ctx.restore();
        let first = pixel(&mut canvas, 0);
        assert!(
            rounds_to(&first, HALF_RED),
            "gpu {gpu}: the first square {first:?}"
        );
        assert_eq!(inside, first, "gpu {gpu}: the square read in the layer");
        assert_eq!(
            pixel(&mut canvas, 1),
            first,
            "gpu {gpu}: the square after the read",
        );
    }
}

/// Layers three deep at 0.5 each, a square drawn at each depth and read
/// there, then every square read again once all three are closed.
#[test]
fn nested_layers_survive_a_read_at_each_depth() {
    for gpu in ENGINES {
        let mut canvas = white_page(gpu);
        let inside: Vec<_> = (0..3)
            .map(|depth| {
                canvas.context().save_layer_with(0.5, None, None);
                square(canvas.context(), depth);
                pixel(&mut canvas, depth)
            })
            .collect();
        for _ in 0..3 {
            canvas.context().restore();
        }
        let closed: Vec<_> = (0..3).map(|n| pixel(&mut canvas, n)).collect();
        for (n, want) in
            [HALF_RED, QUARTER_RED, EIGHTH_RED].into_iter().enumerate()
        {
            assert!(
                rounds_to(&closed[n], want),
                "gpu {gpu}: the square at depth {n} read {:?}",
                closed[n],
            );
        }
        assert_eq!(inside, closed, "gpu {gpu}: read inside and after");
    }
}

/// Layers three deep at 0.5 each, a square drawn at each depth and nothing
/// read until the innermost is open: one read there finds each square still
/// in the recording of the layer it was drawn in, and the read after it, and
/// the reads once all three are closed, find the same.
#[test]
fn nested_layers_read_from_the_innermost_show_every_depth() {
    for gpu in ENGINES {
        let mut canvas = white_page(gpu);
        for depth in 0..3 {
            canvas.context().save_layer_with(0.5, None, None);
            square(canvas.context(), depth);
        }
        let all = |canvas: &mut Canvas| {
            (0..3).map(|n| pixel(canvas, n)).collect::<Vec<_>>()
        };
        let inside = all(&mut canvas);
        assert_eq!(all(&mut canvas), inside, "gpu {gpu}: read again inside");
        for _ in 0..3 {
            canvas.context().restore();
        }
        let closed = all(&mut canvas);
        for (n, want) in
            [HALF_RED, QUARTER_RED, EIGHTH_RED].into_iter().enumerate()
        {
            assert!(
                rounds_to(&closed[n], want),
                "gpu {gpu}: the square at depth {n} read {:?}",
                closed[n],
            );
        }
        assert_eq!(inside, closed, "gpu {gpu}: read inside and after");
    }
}

/// A shadow and a blur drawn in a layer reach past what casts them, and the
/// layer keeps what they spread there: the shadow of a square in the first
/// third lands in the middle one, and a square blurred in the middle third
/// spreads into the last, each read inside the layer and after it closes.
#[test]
fn a_layer_keeps_what_its_shadows_and_blurs_spread() {
    type Spread = fn(&mut Context2D);
    let cases: [(&str, Spread, (f32, f32)); 2] = [
        (
            "a shadow",
            |ctx| {
                ctx.set_shadow_color(RgbaLinear::opaque(0.0, 0.0, 1.0));
                ctx.set_shadow_offset(20.0, 0.0);
                ctx.set_shadow_blur(2.0);
                square(ctx, 0);
            },
            (30.0, 10.0),
        ),
        (
            "a blur",
            |ctx| {
                ctx.set_filter_css("blur(4px)").expect("a blur");
                square(ctx, 1);
            },
            (41.0, 10.0),
        ),
    ];
    for gpu in ENGINES {
        for (name, draw, (x, y)) in cases {
            let mut canvas = white_page(gpu);
            canvas.context().save_layer_with(0.5, None, None);
            draw(canvas.context());
            let read = |canvas: &mut Canvas| {
                canvas
                    .context()
                    .get_image_data(x, y, 1.0, 1.0)
                    .expect("a pixel")
                    .pixels()
                    .to_vec()
            };
            let inside = read(&mut canvas);
            canvas.context().restore();
            let closed = read(&mut canvas);
            assert!(
                closed[..3] != WHITE[..3],
                "gpu {gpu}: {name} past what cast it read {closed:?}"
            );
            assert_eq!(inside, closed, "gpu {gpu}: {name} inside and after");
        }
    }
}

/// A layer opened under a transform and a clip, with bounds of its own, reads
/// the same inside as after: translated 20 to the right, its bounds cover
/// the middle and last thirds, and the clip leaves only the middle.
#[test]
fn a_read_inside_a_layer_keeps_its_transform_clip_and_bounds() {
    for gpu in ENGINES {
        let mut canvas = white_page(gpu);
        let ctx = canvas.context();
        ctx.translate(20.0, 0.0);
        ctx.begin_path();
        ctx.rect(-20.0, 0.0, 40.0, HEIGHT);
        ctx.clip(FillRule::NonZero);
        ctx.save_layer_with(
            0.5,
            Some(Rect::from_xywh(0.0, 0.0, 40.0, HEIGHT)),
            None,
        );
        ctx.reset_transform();
        ctx.fill_rect(0.0, 0.0, WIDTH, HEIGHT);
        let inside: Vec<_> = (0..3).map(|n| pixel(&mut canvas, n)).collect();
        canvas.context().restore();
        let closed: Vec<_> = (0..3).map(|n| pixel(&mut canvas, n)).collect();
        assert_eq!(closed[0], WHITE, "gpu {gpu}: outside the bounds");
        assert!(
            rounds_to(&closed[1], HALF_RED),
            "gpu {gpu}: {:?}",
            closed[1]
        );
        assert_eq!(closed[2], WHITE, "gpu {gpu}: outside the clip");
        assert_eq!(inside, closed, "gpu {gpu}: read inside and after");
    }
}

/// A layer composited with `copy` replaces all its clip covers, not only
/// what was drawn in it, so it is the case that shows the clip it was opened
/// under: clipped to the middle third, an empty one clears that third, and
/// one holding red there reads the same inside as after.
#[test]
fn a_copy_layer_replaces_what_its_clip_covers() {
    for gpu in ENGINES {
        for draws in [false, true] {
            let mut canvas = white_page(gpu);
            let ctx = canvas.context();
            ctx.begin_path();
            ctx.rect(20.0, 0.0, 20.0, HEIGHT);
            ctx.clip(FillRule::NonZero);
            ctx.set_global_composite_operation(BlendMode::Copy);
            ctx.save_layer();
            if draws {
                ctx.fill_rect(0.0, 0.0, WIDTH, HEIGHT);
            }
            let inside: Vec<_> =
                (0..3).map(|n| pixel(&mut canvas, n)).collect();
            canvas.context().restore();
            let closed: Vec<_> =
                (0..3).map(|n| pixel(&mut canvas, n)).collect();
            let middle = if draws { RED } else { CLEAR };
            assert_eq!(
                closed,
                [WHITE, middle, WHITE],
                "gpu {gpu}, drawn in: {draws}",
            );
            assert_eq!(inside, closed, "gpu {gpu}, drawn in: {draws}");
        }
    }
}

/// An export taken while a layer is open composites what the layer holds
/// so far, as a read does, and leaves the layer open for what follows.
#[test]
fn an_export_inside_a_layer_leaves_it_open() {
    for gpu in ENGINES {
        for format in [ImageFormat::Png, ImageFormat::Pdf, ImageFormat::Svg] {
            let mut canvas = white_page(gpu);
            let ctx = canvas.context();
            ctx.save_layer_with(0.5, None, None);
            square(ctx, 0);
            let _ = canvas
                .to_buffer(format, &EncodeOptions::default())
                .expect("an export");
            let ctx = canvas.context();
            square(ctx, 1);
            ctx.restore();
            assert_eq!(
                pixel(&mut canvas, 1),
                pixel(&mut canvas, 0),
                "gpu {gpu}: the square after a {format:?} export",
            );
        }
    }
}

/// A PNG taken inside the layer holds the first square as the layer
/// composites it, the same pixel the page reads once the layer is closed.
#[test]
fn a_png_inside_a_layer_shows_it_composited() {
    for gpu in ENGINES {
        let mut canvas = white_page(gpu);
        let ctx = canvas.context();
        ctx.save_layer_with(0.5, None, None);
        square(ctx, 0);
        let png = canvas
            .to_buffer(ImageFormat::Png, &EncodeOptions::default())
            .expect("a png");
        let decoded = Image::from_encoded(&png).expect("a decoded png");
        let mut copy = white_page(gpu);
        copy.context().draw_image(&decoded, 0.0, 0.0);
        canvas.context().restore();
        assert_eq!(pixel(&mut copy, 0), pixel(&mut canvas, 0), "gpu {gpu}");
    }
}

/// A canvas drawn into another while a layer is open on it brings the layer
/// composited as it stands, and keeps it open.
#[test]
fn a_canvas_drawn_in_while_its_layer_is_open_brings_it_composited() {
    for gpu in ENGINES {
        let mut source = white_page(gpu);
        let ctx = source.context();
        ctx.save_layer_with(0.5, None, None);
        square(ctx, 0);
        let mut copy = white_page(gpu);
        copy.context().draw_canvas(&mut source, 0.0, 0.0);
        let ctx = source.context();
        square(ctx, 1);
        ctx.restore();
        assert_eq!(pixel(&mut copy, 0), pixel(&mut source, 0), "gpu {gpu}");
        assert_eq!(pixel(&mut copy, 1), WHITE, "gpu {gpu}: drawn after");
        assert_eq!(pixel(&mut source, 1), pixel(&mut source, 0), "gpu {gpu}");
    }
}

const OSWALD: &str = "tests/assets/fonts/Oswald/Oswald-VariableFont_wght.ttf";

/// Fills the page white inside a layer, so that what is composited in it
/// next lands on white there, as it would on the page: on a layer's own
/// transparent pixels a composite rounds differently.
fn on_white(ctx: &mut Context2D) {
    ctx.set_fill_style(RgbaLinear::opaque(1.0, 1.0, 1.0));
    ctx.fill_rect(0.0, 0.0, WIDTH, HEIGHT);
    ctx.set_fill_style(RgbaLinear::opaque(1.0, 0.0, 0.0));
}

/// Red composited on white at `alpha` by `draw`, which leaves its darkest
/// pixel where green is `255 * (1 - alpha)`, read as that pixel.
type Composite = fn(&mut Canvas, f32) -> Vec<u8>;

/// The ways a group is composited through a paint, each drawing red over
/// white at an alpha: a layer, one nested in a plain layer, a canvas drawn
/// in, one drawn in inside a layer, and a paragraph under `globalAlpha`.
const COMPOSITES: [(&str, Composite); 5] = [
    ("a layer", |canvas, alpha| {
        let ctx = canvas.context();
        ctx.save_layer_with(alpha, None, None);
        square(ctx, 0);
        ctx.restore();
        pixel(canvas, 0)
    }),
    ("a layer nested in a plain one", |canvas, alpha| {
        let ctx = canvas.context();
        ctx.save_layer();
        on_white(ctx);
        ctx.save_layer_with(alpha, None, None);
        square(ctx, 0);
        ctx.restore();
        ctx.restore();
        pixel(canvas, 0)
    }),
    ("a canvas drawn in", |canvas, alpha| {
        let mut source = white_page(canvas.gpu());
        square(source.context(), 0);
        canvas.context().set_global_alpha(alpha);
        canvas.context().draw_canvas(&mut source, 0.0, 0.0);
        pixel(canvas, 0)
    }),
    ("a canvas drawn in inside a layer", |canvas, alpha| {
        let mut source = white_page(canvas.gpu());
        square(source.context(), 0);
        let ctx = canvas.context();
        ctx.save_layer();
        on_white(ctx);
        ctx.set_global_alpha(alpha);
        ctx.draw_canvas(&mut source, 0.0, 0.0);
        ctx.restore();
        pixel(canvas, 0)
    }),
    ("a paragraph", |canvas, alpha| {
        let library = FontLibrary::new();
        library
            .use_paths(Some("LayersOswald"), [OSWALD])
            .expect("Oswald");
        let paragraph = TextEngine::new(&library).layout_text(
            "H",
            &TextStyle {
                font_families: vec!["LayersOswald".into()],
                font_size: 40.0,
                color: RgbaLinear::opaque(1.0, 0.0, 0.0),
                ..TextStyle::default()
            },
            WIDTH,
        );
        canvas.context().set_global_alpha(alpha);
        canvas.context().draw_paragraph(&paragraph, 0.0, -10.0);
        let pixels = canvas
            .context()
            .get_image_data(0.0, 0.0, WIDTH, HEIGHT)
            .expect("pixels");
        // The middle of a stem, which the glyph covers whole.
        pixels
            .pixels()
            .chunks(4)
            .min_by_key(|pixel| pixel[1])
            .expect("a pixel")
            .to_vec()
    }),
];

/// Every way a group is composited, at every alpha a byte holds, on both
/// engines: green is `255 * (1 - alpha)` to within half a level.
///
/// Skia's raster backend composites a layer on an eight-bit page through
/// its sprite blitter, which blends with an alpha out of 256 and truncates:
/// at 0.5 it gave 126 of an exact 127.5, and the error reached 1.5 levels.
#[test]
fn a_layer_composites_at_its_alpha() {
    let mut wrong = Vec::new();
    for gpu in ENGINES {
        for (name, composite) in COMPOSITES {
            let missed: Vec<_> = (1..255u8)
                .filter_map(|byte| {
                    let alpha = f32::from(byte) / 255.0;
                    let green = 255.0 * (1.0 - alpha);
                    let got = composite(&mut white_page(gpu), alpha);
                    (!rounds_to(&got, [255.0, green, green, 255.0]))
                        .then(|| format!("{byte}/255 read {}", got[1]))
                })
                .collect();
            if let Some(first) = missed.first() {
                wrong.push(format!(
                    "gpu {gpu}, {name}: {} of 254 alphas, first {first}",
                    missed.len()
                ));
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// A canvas drawn in under a `globalAlpha` with a shadow, at every alpha a
/// byte holds: an opaque blue shadow one pixel to the right, beneath the red
/// it casts from, on white.
///
/// Two composites, and the shadow is rounded to bytes twice before the red
/// lands on it -- in the eight-bit layer its filter draws in, then on the
/// page -- so the bound is half a level for the last rounding and the share
/// `1 - alpha` the red leaves of the shadow's own level.
///
/// The second rounding is known, not chosen: Skia's raster backend makes
/// any layer under an image filter N32 (`SkBitmapDevice::createDevice`), and
/// a shadow is drawn through one. When the shadow is composited through the
/// exact result blur instead, rounded once, this bound is half a level.
#[test]
fn a_shadowed_canvas_composites_at_its_alpha() {
    let mut wrong = Vec::new();
    for gpu in ENGINES {
        let missed: Vec<_> = (1..255u8)
            .filter_map(|byte| {
                let alpha = f32::from(byte) / 255.0;
                let left = 1.0 - alpha;
                let mut source = white_page(gpu);
                source.context().fill_rect(0.0, 0.0, WIDTH, HEIGHT);
                let mut canvas = white_page(gpu);
                let ctx = canvas.context();
                ctx.set_shadow_color(RgbaLinear::opaque(0.0, 0.0, 1.0));
                ctx.set_shadow_offset(1.0, 0.0);
                ctx.set_global_alpha(alpha);
                ctx.draw_canvas(&mut source, 0.0, 0.0);
                let got = pixel(&mut canvas, 1);
                let exact = [
                    255.0 * (alpha + left * left),
                    255.0 * left * left,
                    255.0 * left,
                    255.0,
                ];
                let bound = HALF_A_LEVEL + left;
                let off = got
                    .iter()
                    .zip(exact)
                    .map(|(a, b)| (f32::from(*a) - b).abs())
                    .fold(0.0, f32::max);
                (off > bound).then(|| format!("{byte}/255 off by {off}"))
            })
            .collect();
        if let Some(first) = missed.first() {
            wrong.push(format!(
                "gpu {gpu}: {} of 254 alphas, first {first}",
                missed.len()
            ));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// A canvas drawn in under a `globalAlpha` with a blend that reaches past
/// what it covers, at every alpha a byte holds: opaque red over opaque white,
/// read where the red lands. Each composites to its Porter-Duff value within
/// half a level, so that a change of route through the picture these take
/// cannot move them silently.
#[test]
fn a_region_blend_composites_at_its_alpha() {
    type Exact = fn(f32) -> [f32; 4];
    const BLENDS: [(BlendMode, Exact); 8] = [
        (BlendMode::SourceIn, |a| [255.0, 0.0, 0.0, 255.0 * a]),
        (BlendMode::SourceOut, |_| [0.0; 4]),
        (BlendMode::DestinationIn, |a| {
            [255.0, 255.0, 255.0, 255.0 * a]
        }),
        (BlendMode::DestinationOut, |a| {
            [255.0, 255.0, 255.0, 255.0 * (1.0 - a)]
        }),
        (BlendMode::DestinationAtop, |a| {
            [255.0, 255.0, 255.0, 255.0 * a]
        }),
        (BlendMode::Copy, |a| [255.0, 0.0, 0.0, 255.0 * a]),
        (BlendMode::Xor, |a| [255.0, 255.0, 255.0, 255.0 * (1.0 - a)]),
        (BlendMode::Multiply, |a| {
            [255.0, 255.0 * (1.0 - a), 255.0 * (1.0 - a), 255.0]
        }),
    ];
    let mut wrong = Vec::new();
    for gpu in ENGINES {
        for (blend, exact) in BLENDS {
            let missed: Vec<_> = (1..255u8)
                .filter_map(|byte| {
                    let alpha = f32::from(byte) / 255.0;
                    let mut source = white_page(gpu);
                    source.context().fill_rect(0.0, 0.0, WIDTH, HEIGHT);
                    let mut canvas = white_page(gpu);
                    let ctx = canvas.context();
                    ctx.set_global_alpha(alpha);
                    ctx.set_global_composite_operation(blend);
                    ctx.draw_canvas(&mut source, 0.0, 0.0);
                    let got = pixel(&mut canvas, 1);
                    (!rounds_to(&got, exact(alpha)))
                        .then(|| format!("{byte}/255 read {got:?}"))
                })
                .collect();
            if let Some(first) = missed.first() {
                wrong.push(format!(
                    "gpu {gpu}, {blend:?}: {} of 254 alphas, first {first}",
                    missed.len()
                ));
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// A PDF keeps a translucent group vector: a layer, a layer nested in
/// another, and a canvas drawn in under a `globalAlpha`. It writes the
/// alpha as an opacity, where a raster composites it exactly -- see
/// [`a_layer_composites_at_its_alpha`].
#[test]
fn a_pdf_keeps_a_translucent_group_vector() {
    for (name, composite) in COMPOSITES.into_iter().take(4) {
        let mut canvas = white_page(false);
        let _ = composite(&mut canvas, 0.5);
        let pdf = canvas
            .to_buffer(ImageFormat::Pdf, &EncodeOptions::default())
            .expect("a pdf");
        assert!(!has(&pdf, "/Subtype /Image"), "{name} is vector");
    }
}

/// A pattern made of a canvas holding a translucent layer composites the
/// layer exactly on pixels: Skia draws a pattern's picture itself, so the
/// picture holds the form for pixels -- see `Page::picture`.
#[test]
fn a_pattern_of_a_translucent_layer_composites_at_its_alpha() {
    for gpu in ENGINES {
        let mut source = white_page(gpu);
        let ctx = source.context();
        ctx.save_layer_with(0.5, None, None);
        ctx.fill_rect(0.0, 0.0, WIDTH, HEIGHT);
        ctx.restore();
        let mut canvas = white_page(gpu);
        let ctx = canvas.context();
        let pattern =
            ctx.create_pattern_from_canvas(&mut source, PatternRepeat::Repeat);
        ctx.set_fill_pattern(&pattern);
        ctx.fill_rect(0.0, 0.0, WIDTH, HEIGHT);
        let got = pixel(&mut canvas, 1);
        assert!(rounds_to(&got, HALF_RED), "gpu {gpu}: {got:?}");
    }
}

/// A group is played back under the transform the page is: a layer lands
/// where it was drawn in an export at a density, and in a read served by a
/// tile away from the page's origin.
#[test]
fn a_group_lands_where_it_was_drawn() {
    for gpu in ENGINES {
        let mut canvas = white_page(gpu);
        let ctx = canvas.context();
        ctx.save_layer_with(0.5, None, None);
        square(ctx, 1);
        ctx.restore();
        let png = canvas
            .to_buffer(
                ImageFormat::Png,
                &EncodeOptions {
                    density: 2.0,
                    ..EncodeOptions::default()
                },
            )
            .expect("a png");
        let decoded = Image::from_encoded(&png).expect("a decoded png");
        let mut copy = Canvas::with_options(
            WIDTH * 2.0,
            HEIGHT * 2.0,
            CanvasOptions {
                gpu,
                ..CanvasOptions::default()
            },
        )
        .expect("a canvas");
        copy.context().draw_image(&decoded, 0.0, 0.0);
        let at = |copy: &mut Canvas, x: f32| {
            copy.context()
                .get_image_data(x, HEIGHT, 1.0, 1.0)
                .expect("a pixel")
                .pixels()
                .to_vec()
        };
        assert_eq!(at(&mut copy, 20.0), WHITE, "gpu {gpu}: beside it");
        let got = at(&mut copy, 60.0);
        assert!(rounds_to(&got, HALF_RED), "gpu {gpu}: at 2x {got:?}");

        // Far enough from the origin that a small read is served by a tile
        // of its own.
        let side = 1024.0;
        let mut canvas = Canvas::with_options(
            side,
            side,
            CanvasOptions {
                gpu,
                ..CanvasOptions::default()
            },
        )
        .expect("a canvas");
        let ctx = canvas.context();
        ctx.set_fill_style(RgbaLinear::opaque(1.0, 1.0, 1.0));
        ctx.fill_rect(0.0, 0.0, side, side);
        ctx.set_fill_style(RgbaLinear::opaque(1.0, 0.0, 0.0));
        ctx.save_layer_with(0.5, None, None);
        ctx.fill_rect(600.0, 600.0, 20.0, 20.0);
        ctx.restore();
        let got = canvas
            .context()
            .get_image_data(610.0, 610.0, 1.0, 1.0)
            .expect("a pixel")
            .pixels()
            .to_vec();
        assert!(rounds_to(&got, HALF_RED), "gpu {gpu}: on a tile {got:?}");
    }
}

/// A canvas drawn in through a paint shows only the region asked for, in
/// the destination asked for.
#[test]
fn a_canvas_drawn_in_shows_only_its_region() {
    for gpu in ENGINES {
        let mut source = white_page(gpu);
        let ctx = source.context();
        ctx.fill_rect(0.0, 0.0, WIDTH, HEIGHT);
        let mut canvas = white_page(gpu);
        let ctx = canvas.context();
        ctx.set_global_alpha(0.5);
        ctx.draw_canvas_region(
            &mut source,
            0.0,
            0.0,
            20.0,
            HEIGHT,
            20.0,
            0.0,
            20.0,
            HEIGHT,
        );
        assert_eq!(pixel(&mut canvas, 0), WHITE, "gpu {gpu}: before it");
        let got = pixel(&mut canvas, 1);
        assert!(rounds_to(&got, HALF_RED), "gpu {gpu}: {got:?}");
        assert_eq!(pixel(&mut canvas, 2), WHITE, "gpu {gpu}: after it");
    }
}

/// What a PNG holds at the middle of the `n`th third of the page.
fn png_pixel(png: &[u8], gpu: bool, n: usize) -> Vec<u8> {
    let decoded = Image::from_encoded(png).expect("a decoded png");
    let mut copy = white_page(gpu);
    copy.context().draw_image(&decoded, 0.0, 0.0);
    pixel(&mut copy, n)
}

/// An export taken inside a layer and encoded later shows the layer as it
/// stood when it was taken, even once the layer has closed and the page has
/// been exported again: the finished layer is at the depth the export holds
/// the unfinished one, and must not be taken for it.
#[test]
fn an_export_taken_inside_a_layer_keeps_the_layer_as_it_stood() {
    for gpu in ENGINES {
        let mut canvas = white_page(gpu);
        let options = EncodeOptions::default();
        let _ = canvas.to_buffer(ImageFormat::Png, &options).expect("a png");
        let ctx = canvas.context();
        ctx.save_layer_with(0.5, None, None);
        square(ctx, 0);
        let taken = canvas
            .prepare_export(ImageFormat::Png, &options)
            .expect("an export");
        let ctx = canvas.context();
        square(ctx, 1);
        ctx.restore();
        let closed =
            canvas.to_buffer(ImageFormat::Png, &options).expect("a png");
        assert!(
            rounds_to(&png_pixel(&closed, gpu, 1), HALF_RED),
            "gpu {gpu}"
        );
        let taken = taken.encode().expect("a png");
        assert_eq!(png_pixel(&taken, gpu, 1), WHITE, "gpu {gpu}: drawn after");
        assert_eq!(
            png_pixel(&taken, gpu, 0),
            png_pixel(&closed, gpu, 0),
            "gpu {gpu}: drawn before",
        );
    }
}

/// What `canvas` reads at a rectangle, as bytes.
fn read(
    canvas: &mut Canvas,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
) -> Vec<u8> {
    canvas
        .context()
        .get_image_data(x, y, width, height)
        .expect("pixels")
        .pixels()
        .to_vec()
}

/// The rows of a page-wide read that a read of `width` by `height` at
/// (`x`, `y`) covers.
fn slice(
    whole: &[u8],
    x: usize,
    y: usize,
    width: usize,
    height: usize,
) -> Vec<u8> {
    let row = WIDTH as usize * 4;
    (y..y + height)
        .flat_map(|at| &whole[at * row + x * 4..at * row + (x + width) * 4])
        .copied()
        .collect()
}

/// A read inside a layer reads, wherever it falls, what the same read of
/// the whole page reads there: at the page's corner and far edge, across
/// the layer's edge, and inside it. With a plain layer, which composes only
/// the region read; with a blurred fill, whose rectangle Skia blurs from its
/// geometry; with a shadow, whose filter Skia gives the input it needs past
/// the read; and with a backdrop, which reads the page beneath past it, and
/// so composes the page.
#[test]
fn a_read_inside_a_layer_reads_what_the_page_does() {
    for gpu in ENGINES {
        for content in ["plain", "blurred", "shadowed", "backdrop"] {
            let mut canvas = white_page(gpu);
            let ctx = canvas.context();
            square(ctx, 1);
            let backdrop = (content == "backdrop").then(|| {
                ImageFilter::blur(4.0, 4.0, None, None, None).expect("a blur")
            });
            ctx.save_layer_with(0.5, None, backdrop.as_ref());
            match content {
                "blurred" => ctx.set_filter_css("blur(3px)").expect("a blur"),
                "shadowed" => {
                    ctx.set_shadow_color(RgbaLinear::opaque(0.0, 0.0, 1.0));
                    ctx.set_shadow_blur(6.0);
                }
                _ => {}
            }
            ctx.fill_rect(15.0, 4.0, 30.0, 12.0);
            let whole = read(&mut canvas, 0.0, 0.0, WIDTH, HEIGHT);
            for (x, y, width, height) in [
                (0, 0, 20, 20),
                (10, 0, 20, 20),
                (40, 0, 20, 20),
                (59, 19, 1, 1),
                (14, 3, 3, 3),
            ] {
                let got = read(
                    &mut canvas,
                    x as f32,
                    y as f32,
                    width as f32,
                    height as f32,
                );
                assert!(
                    got == slice(&whole, x, y, width, height),
                    "gpu {gpu}, {content}: the read at {x},{y} {width}x{height}"
                );
            }
            canvas.context().restore();
        }
    }
}

/// An export inside a layer is the same file however often it is taken --
/// the second is served from the filed page the first cached -- and an
/// export after the layer closes is the one a canvas never exported inside
/// it gives.
#[test]
fn an_export_inside_a_layer_is_the_export_it_was() {
    for gpu in ENGINES {
        for format in [ImageFormat::Png, ImageFormat::Raw] {
            let options = EncodeOptions::default();
            let draw = |canvas: &mut Canvas,
                        exports: bool|
             -> (Vec<Vec<u8>>, Vec<u8>) {
                let ctx = canvas.context();
                for n in 0..3 {
                    square(ctx, n);
                }
                ctx.save_layer_with(0.5, None, None);
                ctx.fill_rect(15.0, 4.0, 30.0, 12.0);
                let inside = match exports {
                    true => (0..2)
                        .map(|_| {
                            canvas
                                .to_buffer(format, &options)
                                .expect("an export")
                        })
                        .collect(),
                    false => Vec::new(),
                };
                canvas.context().restore();
                (
                    inside,
                    canvas.to_buffer(format, &options).expect("an export"),
                )
            };
            let (inside, closed) = draw(&mut white_page(gpu), true);
            let (_, never) = draw(&mut white_page(gpu), false);
            assert!(
                inside[0] == inside[1],
                "gpu {gpu}, {format:?}: inside the layer"
            );
            assert!(closed == never, "gpu {gpu}, {format:?}: after it closes");
        }
    }
}

fn has(bytes: &[u8], needle: &str) -> bool {
    bytes.windows(needle.len()).any(|w| w == needle.as_bytes())
}

/// An SVG export writes a translucent layer as a group at its opacity, and
/// what it holds as vectors beside the square drawn outside it. A PDF keeps
/// both vector.
#[test]
fn an_svg_writes_a_translucent_layer_as_a_group() {
    let mut canvas = white_page(true);
    let ctx = canvas.context();
    square(ctx, 2);
    ctx.save_layer_with(0.5, None, None);
    square(ctx, 0);
    ctx.restore();
    let svg = canvas
        .to_buffer(ImageFormat::Svg, &EncodeOptions::default())
        .expect("an svg");
    assert!(has(&svg, "<g opacity=\"0.5\""), "the layer is in the SVG");
    assert!(!has(&svg, "<image"), "and nothing of it is rasterized");
    assert!(
        String::from_utf8_lossy(&svg).matches("<path").count() >= 3,
        "the page, the square outside the layer and the one in it stay vector",
    );
    let pdf = canvas
        .to_buffer(ImageFormat::Pdf, &EncodeOptions::default())
        .expect("a pdf");
    assert!(!has(&pdf, "/Subtype /Image"), "the PDF stays vector");
}

/// A layer holding one draw composites as one, whether or not Skia's
/// recording optimizer folds the layer into the draw.
#[test]
fn a_layer_of_one_draw_composites_as_a_layer() {
    for gpu in ENGINES {
        let mut canvas = white_page(gpu);
        let ctx = canvas.context();
        ctx.save_layer_with(0.5, None, None);
        square(ctx, 0);
        ctx.restore();
        let got = pixel(&mut canvas, 0);
        assert!(rounds_to(&got, HALF_RED), "gpu {gpu}: {got:?}");
    }
}

/// How far a pixel composited through a layer may be from the same pixel
/// drawn without one: the layer is an eight-bit picture of its own, which
/// rounds once more. A blur inside a plain layer came out 1 level from the
/// blur outside it in 48 of 4800 bytes, and no further.
const LAYER_ROUNDING: u8 = 1;

/// A blurred draw inside a layer is drawn as the same blur outside one, then
/// composited through the layer.
#[test]
fn a_blur_inside_a_layer_is_the_blur_outside_one() {
    for gpu in ENGINES {
        let draw = |in_layer: bool| {
            let mut canvas = white_page(gpu);
            let ctx = canvas.context();
            ctx.set_filter_css("blur(3px)").expect("a blur");
            if in_layer {
                ctx.save_layer();
            }
            ctx.fill_rect(10.0, 5.0, 40.0, 10.0);
            if in_layer {
                ctx.restore();
            }
            canvas
                .to_image_data(None, &ImageDataExport::default())
                .expect("pixels")
                .pixels()
                .to_vec()
        };
        let (inside, outside) = (draw(true), draw(false));
        let worst = inside
            .iter()
            .zip(&outside)
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .expect("pixels");
        assert!(worst <= LAYER_ROUNDING, "gpu {gpu}: {worst} levels apart");
    }
}

/// One draw of the scene the raster cases build, and a read after it.
type Step = fn(&mut Context2D);

/// Draws reaching past their geometry, each in its own colour, put down one
/// at a time inside layers so that a read after each tops up what the
/// layers have drawn rather than starting over.
const STEPS: [Step; 7] = [
    |ctx| {
        ctx.set_fill_style(RgbaLinear::opaque(0.9, 0.1, 0.1));
        ctx.fill_rect(10.3, 7.7, 23.2, 11.9);
    },
    |ctx| {
        ctx.set_stroke_style(RgbaLinear::opaque(0.1, 0.5, 0.9));
        ctx.set_line_width(0.6);
        ctx.begin_path();
        ctx.move_to(4.5, 3.25);
        ctx.line_to(57.75, 16.5);
        ctx.stroke();
    },
    |ctx| {
        ctx.set_fill_style(RgbaLinear::opaque(0.2, 0.7, 0.3));
        ctx.set_font_css("bold 13px sans-serif");
        ctx.fill_text("Wavy", 21.4, 15.6, None);
    },
    |ctx| {
        ctx.set_filter_css("blur(3px)").expect("a blur");
        ctx.set_fill_style(RgbaLinear::opaque(0.6, 0.2, 0.8));
        ctx.fill_rect(30.0, 4.0, 14.0, 9.0);
        ctx.set_filter_css("none").expect("no filter");
    },
    |ctx| {
        ctx.set_shadow_color(RgbaLinear::opaque(0.0, 0.0, 0.0));
        ctx.set_shadow_offset(6.0, 3.0);
        ctx.set_shadow_blur(4.0);
        ctx.set_fill_style(RgbaLinear::opaque(0.9, 0.8, 0.1));
        ctx.fill_rect(42.6, 2.2, 8.0, 8.0);
        ctx.set_shadow_color(RgbaLinear::new_premultiplied(0.0, 0.0, 0.0, 0.0));
    },
    |ctx| {
        // Over the fills before it, across the clip's edge.
        ctx.set_fill_style(RgbaLinear::opaque(0.1, 0.8, 0.8));
        ctx.fill_rect(5.5, 9.5, 50.0, 6.0);
    },
    |ctx| {
        ctx.set_fill_style(RgbaLinear::opaque(0.95, 0.5, 0.0));
        ctx.fill_rect(0.0, 0.0, 7.3, 20.0);
    },
];

/// What `canvas` reads of all of it, in the page's depth, as bytes.
fn page_bytes(canvas: &mut Canvas, depth: PixelDepth) -> Vec<u8> {
    canvas
        .context()
        .get_image_data_as(
            0.0,
            0.0,
            WIDTH,
            HEIGHT,
            PixelExportOptions {
                depth,
                ..PixelExportOptions::default()
            },
        )
        .expect("pixels")
        .pixels()
        .to_vec()
}

/// Reads inside layers read what the layers close to, however many reads
/// come between the draws: three layers nested, at 0.5, 0.75 and no paint,
/// the outer two under clips the draws cross -- rectangles on whole pixels,
/// and circles, whose antialiased edges a raster of the layers would round
/// otherwise -- with a read of part of the page after every draw, on both
/// engines, at eight bits and in F16 and F32.
#[test]
fn reads_between_draws_read_what_layers_close_to() {
    for gpu in ENGINES {
        for depth in [PixelDepth::Uint8, PixelDepth::F16, PixelDepth::F32] {
            for round in [false, true] {
                let mut canvas = Canvas::with_options(
                    WIDTH,
                    HEIGHT,
                    CanvasOptions {
                        gpu,
                        color_type: depth,
                        ..CanvasOptions::default()
                    },
                )
                .expect("a canvas");
                let ctx = canvas.context();
                ctx.set_fill_style(RgbaLinear::opaque(1.0, 1.0, 1.0));
                ctx.fill_rect(0.0, 0.0, WIDTH, HEIGHT);
                for (alpha, clip) in [
                    (Some(0.5), Some((28.0, 10.0, 17.5))),
                    (Some(0.75), Some((24.0, 9.0, 13.25))),
                    (None, None),
                ] {
                    let ctx = canvas.context();
                    if let Some((x, y, r)) = clip {
                        ctx.begin_path();
                        match round {
                            true => ctx
                                .arc(x, y, r, 0.0, std::f32::consts::TAU, false)
                                .expect("an arc"),
                            false => ctx.rect(
                                (x - r).round(),
                                0.0,
                                (2.0 * r).round(),
                                (y + r).round(),
                            ),
                        }
                        ctx.clip(FillRule::NonZero);
                    }
                    match alpha {
                        Some(alpha) => ctx.save_layer_with(alpha, None, None),
                        None => ctx.save_layer(),
                    }
                }
                for (at, step) in STEPS.iter().enumerate() {
                    step(canvas.context());
                    let x = (at * 7) as f32;
                    read(&mut canvas, x, 2.0, 13.0, 11.0);
                }
                let inside = page_bytes(&mut canvas, depth);
                for _ in 0..3 {
                    canvas.context().restore();
                }
                let closed = page_bytes(&mut canvas, depth);
                assert!(
                    inside == closed,
                    "gpu {gpu}, {depth:?}, round {round}: inside and after"
                );
            }
        }
    }
}
