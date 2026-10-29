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

/// Red on white through a layer at alpha 0.5.
const HALF_RED: [u8; 4] = [255, 127, 127, 255];

/// Red on white through two layers at 0.5, a quarter of the red.
const QUARTER_RED: [u8; 4] = [255, 191, 191, 255];

/// Red on white through three layers at 0.5, an eighth of the red.
const EIGHTH_RED: [u8; 4] = [255, 223, 223, 255];

/// Both engines, held each to itself: red through a layer at 0.5 reads
/// `127` on the GPU and `126` on the CPU, which a plain fill at
/// `globalAlpha` 0.5 reads as `127` on both.
const ENGINES: [bool; 2] = [false, true];

/// How far a layer's composite may be from the exact value: the CPU's is a
/// level below the GPU's at 0.5.
const ENGINE_ROUNDING: u8 = 1;

fn near(got: &[u8], want: [u8; 4]) -> bool {
    got.iter()
        .zip(want)
        .all(|(a, b)| a.abs_diff(b) <= ENGINE_ROUNDING)
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
            near(&first, HALF_RED),
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
                near(&closed[n], want),
                "gpu {gpu}: the square at depth {n} read {:?}",
                closed[n],
            );
        }
        assert_eq!(inside, closed, "gpu {gpu}: read inside and after");
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
        assert!(near(&closed[1], HALF_RED), "gpu {gpu}: {:?}", closed[1]);
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
        assert!(near(&png_pixel(&closed, gpu, 1), HALF_RED), "gpu {gpu}");
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

/// An SVG export rasterizes the layer and nothing else: a square drawn
/// outside it stays a vector beside the layer's image. A PDF keeps both
/// vector.
#[test]
fn an_svg_rasterizes_only_the_layer() {
    let mut canvas = white_page(true);
    let ctx = canvas.context();
    square(ctx, 2);
    ctx.save_layer_with(0.5, None, None);
    square(ctx, 0);
    ctx.restore();
    let svg = canvas
        .to_buffer(ImageFormat::Svg, &EncodeOptions::default())
        .expect("an svg");
    assert!(has(&svg, "<image"), "the layer is in the SVG");
    assert!(
        String::from_utf8_lossy(&svg).matches("<path").count() >= 2,
        "the page and the square outside the layer stay vector",
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
        assert!(near(&got, HALF_RED), "gpu {gpu}: {got:?}");
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
