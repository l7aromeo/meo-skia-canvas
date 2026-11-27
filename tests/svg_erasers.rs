//! An SVG of a page holding an eraser -- a `clearRect` short of the whole
//! page, or a `putImageData` -- shows what the canvas does, and keeps what
//! lies outside the erased region as vectors.
//!
//! Skia's SVG device writes no blend, so a `BlendMode::Clear` erases
//! nothing in an SVG, and it writes a `ClipOp::Difference` clip as an
//! intersection. So what was drawn before an eraser is written clipped to
//! the page less every region erased after it, and a put's own pixels are
//! written over nothing in its place.
//!
//! Each SVG is rendered by the crate's own `Svg::rasterize`, which
//! composites in sRGB -- see [`common::rendered_svg`] -- and held to the
//! canvas's own pixels.

mod common;

use meo_skia_canvas::{
    export::{EncodeOptions, ImageFormat},
    prelude::*,
};
use skia_safe::{
    ClipOp, Color, Paint, Rect,
    svg::{self, canvas::Flags},
};

/// The side of every page drawn here, in pixels.
const SIDE: f32 = 48.0;

/// Bytes in a pixel of an eight-bit RGBA raster.
const BYTES_PER_PIXEL: usize = 4;

/// How far an SVG's render may land from the canvas at a pixel an
/// antialiased edge of an erased region crosses, where the region is not on
/// whole pixels. Away from such an edge every pixel is held exactly.
///
/// The canvas erases what lies beneath at the edge's coverage `c` once,
/// leaving `c` of the composite. An SVG renderer -- Skia's SVG DOM, and
/// Chrome alike -- clips each element drawn before the eraser by that
/// coverage on its own, so each lets the one beneath it show through: two
/// opaque elements land `1 - (1 - c)^2` of alpha where the canvas lands
/// `1 - c` of it, apart by `c (1 - c)`, at most a quarter, for the two
/// elements the turned scene stacks. The renderer also estimates the
/// edge's coverage apart from the canvas, by up to the sixteenth Skia's
/// supersampled coverage resolves.
const EDGE: u8 = u8::MAX / 4 + u8::MAX / 16;

/// The fills the scenes draw, as Skia's SVG device writes them.
const BLUE: &str = "blue";
/// `rgb(0,160,0)`.
const GREEN: &str = "#00A000";
/// `rgb(200,40,120)`.
const PINK: &str = "#C82878";

/// A CPU canvas of [`SIDE`] by [`SIDE`].
fn page() -> Canvas {
    let mut canvas = Canvas::new(SIDE, SIDE);
    canvas.set_gpu(false);
    canvas
}

/// `canvas`'s pixels, premultiplied RGBA.
fn read(canvas: &mut Canvas) -> Vec<u8> {
    canvas
        .to_image_data(
            None,
            &ImageDataExport {
                pixels: PixelExportOptions {
                    premultiplied: true,
                    ..PixelExportOptions::default()
                },
                ..ImageDataExport::default()
            },
        )
        .expect("pixels")
        .pixels()
        .to_vec()
}

/// `canvas` exported as an SVG.
fn svg_of(canvas: &mut Canvas) -> String {
    let svg = canvas
        .to_buffer(ImageFormat::Svg, &EncodeOptions::default())
        .expect("an SVG");
    String::from_utf8(svg).expect("utf-8")
}

/// Fills the rectangle at `x`, `y` of `w` by `h` with `css`.
fn fill(ctx: &mut Context2D, css: &str, [x, y, w, h]: [f32; 4]) {
    ctx.set_fill_style_css(css).expect("a colour");
    ctx.fill_rect(x, y, w, h);
}

/// `side` by `side` pixels of `rgba`, unpremultiplied.
fn pixels(ctx: &mut Context2D, side: u32, rgba: [u8; 4]) -> ImageData {
    let mut data = ctx.create_image_data(side, side).expect("image data");
    for pixel in data.pixels_mut().chunks_mut(BYTES_PER_PIXEL) {
        pixel.copy_from_slice(&rgba);
    }
    data
}

/// The pixel at `x`, `y` of premultiplied RGBA `bytes` [`SIDE`] across.
fn at(bytes: &[u8], x: usize, y: usize) -> [u8; 4] {
    let i = (y * SIDE as usize + x) * BYTES_PER_PIXEL;
    [bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]
}

/// Whether the pixel at `x`, `y` of `canvas` differs from a neighbour
/// beside it: whether an edge crosses it or one beside it, where the
/// pixels of a page of flat colours change.
fn on_an_edge(canvas: &[u8], x: usize, y: usize) -> bool {
    let side = SIDE as usize;
    let here = at(canvas, x, y);
    [(-1, 0), (1, 0), (0, -1), (0, 1)]
        .into_iter()
        .any(|(dx, dy)| {
            let (nx, ny) = (x as isize + dx, y as isize + dy);
            (0..side as isize).contains(&nx)
                && (0..side as isize).contains(&ny)
                && at(canvas, nx as usize, ny as usize) != here
        })
}

/// Where the SVG of `canvas` renders apart from the canvas by more than
/// `level`: anywhere where `exact`, and otherwise off an edge, or past
/// [`EDGE`] on one, as `(x, y, svg, canvas)`.
fn apart(
    canvas: &mut Canvas,
    exact: bool,
    level: u8,
) -> Vec<(usize, usize, [u8; 4], [u8; 4])> {
    let want = read(canvas);
    let svg = svg_of(canvas);
    let got = common::rendered_svg(&svg, SIDE as u32, SIDE as u32);
    let side = SIDE as usize;
    (0..side * side)
        .map(|i| (i % side, i / side))
        .filter_map(|(x, y)| {
            let (g, w) = (at(&got, x, y), at(&want, x, y));
            let most = g.iter().zip(w).map(|(g, w)| g.abs_diff(w)).max();
            let allowed = match exact || !on_an_edge(&want, x, y) {
                true => level,
                false => EDGE,
            };
            (most > Some(allowed)).then_some((x, y, g, w))
        })
        .collect()
}

/// Asserts the SVG of `canvas` renders as the canvas, to within `level`
/// -- see [`apart`] --
/// writes no `<image>` beyond `images`, and writes each of `fills` as a
/// vector: what is not a put stays vector, rather than going into an image
/// with a put that would keep the count.
fn renders_as_the_canvas(
    canvas: &mut Canvas,
    exact: bool,
    level: u8,
    images: usize,
    fills: &[&str],
) {
    let wrong = apart(canvas, exact, level);
    assert!(
        wrong.is_empty(),
        "{} pixels apart (x, y, svg, canvas), first: {:?}",
        wrong.len(),
        &wrong[..wrong.len().min(6)]
    );
    let svg = svg_of(canvas);
    assert_eq!(
        svg.matches("<image").count(),
        images,
        "images in the SVG: {svg}"
    );
    for fill in fills {
        assert!(
            svg.contains(&format!("fill=\"{fill}\"")),
            "no vector fill {fill} in {svg}"
        );
    }
}

/// A `clearRect` placed on whole pixels erases what was drawn before it and
/// nothing drawn after it, and both stay vectors.
#[test]
fn a_clear_rect_on_whole_pixels_renders_as_the_canvas() {
    let mut canvas = page();
    let ctx = canvas.context();
    fill(ctx, "rgb(0,0,255)", [0.0, 0.0, SIDE, SIDE]);
    fill(ctx, "rgb(0,160,0)", [8.0, 8.0, 32.0, 32.0]);
    ctx.clear_rect(16.0, 16.0, 16.0, 16.0);
    fill(ctx, "rgb(200,40,120)", [20.0, 4.0, 8.0, 40.0]);
    ctx.clear_rect(4.0, 30.0, 10.0, 10.0);
    renders_as_the_canvas(&mut canvas, true, 0, 0, &[BLUE, GREEN, PINK]);
}

/// A turned `clearRect` under a clip that is not a rectangle erases the
/// rectangle within the clip's path, not within its bounds: the corner of
/// the rectangle past the clip's diagonal keeps what lies beneath it.
#[test]
fn a_turned_clear_rect_under_a_path_clip_erases_within_the_path() {
    let mut canvas = page();
    let ctx = canvas.context();
    fill(ctx, "rgb(0,0,255)", [0.0, 0.0, SIDE, SIDE]);
    fill(ctx, "rgb(0,160,0)", [6.0, 6.0, 36.0, 36.0]);
    ctx.save();
    // The page's upper-left half: its bounds are the whole page.
    ctx.begin_path();
    ctx.move_to(0.0, 0.0);
    ctx.line_to(SIDE, 0.0);
    ctx.line_to(0.0, SIDE);
    ctx.close_path();
    ctx.clip(FillRule::NonZero);
    ctx.translate(SIDE / 2.0, SIDE / 2.0);
    ctx.rotate(std::f32::consts::FRAC_PI_6);
    ctx.clear_rect(-14.0, -14.0, 28.0, 28.0);
    ctx.restore();
    fill(ctx, "rgb(200,40,120)", [2.0, 2.0, 4.0, 44.0]);

    // Inside the turned rectangle and past the diagonal: kept.
    let pixels = read(&mut canvas);
    assert_eq!(at(&pixels, 30, 30), [0, 160, 0, 255], "the scene keeps it");
    assert_eq!(at(&pixels, 16, 16)[3], 0, "the scene erases inside");
    renders_as_the_canvas(&mut canvas, false, 0, 0, &[BLUE, GREEN, PINK]);
}

/// A put erases its destination untransformed and unclipped, whatever the
/// transform and the clip, and writes its own pixels over nothing there.
#[test]
fn a_put_under_a_transform_and_a_clip_erases_its_destination() {
    let mut canvas = page();
    let ctx = canvas.context();
    fill(ctx, "rgb(0,0,255)", [0.0, 0.0, SIDE, SIDE]);
    ctx.save();
    ctx.begin_path();
    ctx.rect(30.0, 30.0, 10.0, 10.0);
    ctx.clip(FillRule::NonZero);
    ctx.translate(14.0, 3.0);
    ctx.scale(1.5, 1.5);
    let blank = pixels(ctx, 8, [0, 0, 0, 0]);
    ctx.put_image_data(&blank, 4.0, 4.0).expect("a put");
    let red = pixels(ctx, 8, [255, 0, 0, 128]);
    ctx.put_image_data(&red, 20.0, 20.0).expect("a put");
    ctx.restore();
    fill(ctx, "rgb(0,160,0)", [0.0, 40.0, 8.0, 8.0]);

    let pixels = read(&mut canvas);
    assert_eq!(
        at(&pixels, 6, 6),
        [0, 0, 0, 0],
        "the put lands untransformed"
    );
    assert_eq!(at(&pixels, 22, 22), [128, 0, 0, 128], "and unclipped");
    renders_as_the_canvas(&mut canvas, true, 0, 2, &[BLUE, GREEN]);
}

/// An eraser in a layer erases what the layer holds and nothing beneath
/// it: the page drawn before the layer survives where the layer is cleared,
/// and a sibling drawn after the layer survives the eraser's region.
#[test]
fn an_eraser_in_a_layer_erases_only_the_layer() {
    let mut canvas = page();
    let ctx = canvas.context();
    fill(ctx, "rgb(0,0,255)", [0.0, 0.0, SIDE, SIDE]);
    ctx.save_layer();
    fill(ctx, "rgb(0,160,0)", [4.0, 4.0, 40.0, 40.0]);
    ctx.clear_rect(10.0, 10.0, 12.0, 12.0);
    let red = pixels(ctx, 8, [255, 0, 0, 128]);
    ctx.put_image_data(&red, 28.0, 28.0).expect("a put");
    ctx.restore();
    fill(ctx, "rgb(200,40,120)", [12.0, 12.0, 4.0, 4.0]);

    let pixels = read(&mut canvas);
    assert_eq!(at(&pixels, 20, 20), [0, 0, 255, 255], "the page shows");
    assert_eq!(
        at(&pixels, 13, 13),
        [200, 40, 120, 255],
        "the sibling shows"
    );
    renders_as_the_canvas(&mut canvas, true, 0, 1, &[BLUE, GREEN, PINK]);
}

/// How far a group an SVG writes at an opacity renders from the canvas's
/// layer at that alpha, with or without an eraser in it: half-alpha green
/// over blue renders 0, 80, 126 where the canvas lands 0, 80, 127, the
/// renderer rounding the group's opacity a level apart from the canvas.
const GROUP_OPACITY: u8 = 1;

/// An eraser in a layer composited at an opacity, which an SVG writes as a
/// group of its own at that opacity, erases what that group holds.
#[test]
fn an_eraser_in_a_translucent_layer_erases_only_the_layer() {
    let mut canvas = page();
    let ctx = canvas.context();
    fill(ctx, "rgb(0,0,255)", [0.0, 0.0, SIDE, SIDE]);
    ctx.save_layer_with(0.5, None, None);
    fill(ctx, "rgb(0,160,0)", [4.0, 4.0, 40.0, 40.0]);
    ctx.clear_rect(10.0, 10.0, 12.0, 12.0);
    ctx.restore();
    fill(ctx, "rgb(200,40,120)", [12.0, 12.0, 4.0, 4.0]);

    let pixels = read(&mut canvas);
    assert_eq!(at(&pixels, 20, 20), [0, 0, 255, 255], "the page shows");
    renders_as_the_canvas(
        &mut canvas,
        true,
        GROUP_OPACITY,
        0,
        &[BLUE, GREEN, PINK],
    );
    assert!(
        svg_of(&mut canvas).contains("opacity"),
        "a group at an opacity"
    );
}

/// The density the density case exports at, and renders its SVG at.
const DENSITY: f32 = 2.0;

/// At a density, an SVG is written in the page's own units as it is at 1,
/// and rendered at that density lands as the canvas exported there does: a
/// `clearRect` on half pixels, whose edges cross pixels at 1, lands on
/// whole ones, and is held exactly, as is the put beside it.
#[test]
fn erasers_at_a_density_render_as_the_canvas_exported_there() {
    let mut canvas = page();
    let ctx = canvas.context();
    fill(ctx, "rgb(0,0,255)", [0.0, 0.0, SIDE, SIDE]);
    fill(ctx, "rgb(0,160,0)", [6.0, 6.0, 36.0, 36.0]);
    ctx.clear_rect(10.5, 10.5, 9.0, 9.0);
    let red = pixels(ctx, 8, [255, 0, 0, 128]);
    ctx.put_image_data(&red, 30.0, 30.0).expect("a put");
    fill(ctx, "rgb(200,40,120)", [12.0, 12.0, 4.0, 4.0]);

    let options = EncodeOptions {
        density: DENSITY,
        ..EncodeOptions::default()
    };
    let png = canvas.to_buffer(ImageFormat::Png, &options).expect("a PNG");
    let side = SIDE * DENSITY;
    let mut reference = Canvas::new(side, side);
    reference.set_gpu(false);
    let image = Image::from_encoded(&png).expect("the PNG decodes");
    reference.context().draw_image(&image, 0.0, 0.0);
    let want = read(&mut reference);
    let svg = canvas
        .to_buffer(ImageFormat::Svg, &options)
        .expect("an SVG");
    let svg = String::from_utf8(svg).expect("utf-8");
    let got = common::rendered_svg(&svg, side as u32, side as u32);
    let wrong = got
        .chunks(BYTES_PER_PIXEL)
        .zip(want.chunks(BYTES_PER_PIXEL))
        .enumerate()
        .filter(|(_, (g, w))| g != w)
        .map(|(i, (g, w))| {
            (i % side as usize, i / side as usize, g.to_vec(), w.to_vec())
        })
        .collect::<Vec<_>>();
    assert!(
        wrong.is_empty(),
        "{} pixels apart (x, y, svg, canvas), first: {:?}",
        wrong.len(),
        &wrong[..wrong.len().min(6)]
    );
    assert_eq!(svg.matches("<image").count(), 1, "images in {svg}");
}

/// The defect the SVG export works around, in Skia's SVG device alone: a
/// `ClipOp::Difference` clip is written as an intersection, keeping what it
/// should cut and cutting what it should keep. Passing means the defect is
/// still there; failing means the device writes the difference, and the
/// region an eraser leaves need no longer be made by hand.
#[test]
fn skia_svg_writes_a_difference_clip_as_intersection() {
    let side = SIDE as i32;
    let bounds = Rect::from_wh(SIDE, SIDE);
    let canvas = svg::Canvas::new(bounds, Some(Flags::CONVERT_TEXT_TO_PATHS));
    canvas.clip_rect(
        Rect::from_xywh(8.0, 8.0, 16.0, 16.0),
        ClipOp::Difference,
        false,
    );
    let mut paint = Paint::default();
    paint.set_color(Color::BLUE);
    canvas.draw_rect(bounds, &paint);
    let data = canvas.end();
    let svg = std::str::from_utf8(data.as_bytes()).expect("utf-8");
    let rendered = common::rendered_svg(svg, side as u32, side as u32);
    assert_eq!(
        at(&rendered, 16, 16),
        [0, 0, 255, 255],
        "drawn inside: {svg}"
    );
    assert_eq!(at(&rendered, 40, 40), [0, 0, 0, 0], "cut outside: {svg}");
}
