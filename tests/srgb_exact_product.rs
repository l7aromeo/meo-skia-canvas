//! A translucent draw on an eight-bit sRGB canvas stores the exact product
//! of its colour and alpha, rounded once, on either engine.
//!
//! Skia blits a source-over paint with no shader onto eight-bit sRGB pixels
//! through its legacy blitter, which rounds the colour and the alpha to
//! bytes before it multiplies them. A colour and alpha on the byte grid
//! cannot tell that from the exact product -- every product of two bytes
//! over 255 is a whole number or at least half a 255th from a tie -- so the
//! sweep here is off it: grey `(c + 0.37) / 255` at alpha `(a + 0.61) /
//! 255`, over every `c` and `a` below 255, which the legacy blit stored a
//! level off for 8,260 of the 65,025 pairs.

use meo_skia_canvas::prelude::*;

/// Both engines.
const ENGINES: [bool; 2] = [false, true];

/// How far off the byte grid a channel and the alpha are named: far enough
/// from a byte that rounding first moves the product, and apart from each
/// other so the two offsets do not cancel.
const OFF_GRID: (f64, f64) = (0.37, 0.61);

/// The canvas is one pixel per colour and three rows, the middle one read,
/// so that a stroke's caps and a draw's edges stay off it.
const ROWS: f32 = 3.0;

/// The levels of an eight-bit channel.
const LEVELS: f64 = 255.0;

#[derive(Clone, Copy, Debug)]
enum Draw {
    Fill,
    Stroke,
    GlobalAlpha,
    Texture,
    Text,
    Filter,
}

const DRAWS: [Draw; 6] = [
    Draw::Fill,
    Draw::Stroke,
    Draw::GlobalAlpha,
    Draw::Texture,
    Draw::Text,
    Draw::Filter,
];

/// A face with a full block, U+2588, on macOS and on Linux, at a size whose
/// block covers the rows read: what text sets its glyphs in for the sweep.
const BLOCK_FONT: &str = "40px Menlo, \"DejaVu Sans Mono\", monospace";

/// The full block, whose glyph covers every pixel the sweep reads.
const BLOCK: &str = "\u{2588}";

/// `rgb()` naming grey `c + c_off` at alpha `(a + a_off) / 255`.
fn grey(c: usize, a: usize, (c_off, a_off): (f64, f64)) -> String {
    let level = c as f64 + c_off;
    let alpha = (a as f64 + a_off) / LEVELS;
    format!("rgb({level} {level} {level} / {alpha})")
}

/// A texture with one solid mark far larger than the canvas, so each pixel
/// it fills is covered once.
fn solid(colour: RgbaLinear) -> Texture {
    let mark = Path2D::from_svg("M-1 -1H601V601H-1Z", FillRule::NonZero)
        .expect("a path");
    Texture::new(&TextureOptions {
        path: Some(mark),
        color: colour,
        line: 0.0,
        spacing: (600.0, 600.0),
        ..TextureOptions::default()
    })
}

/// The premultiplied middle row of a canvas where column `c` is grey `c`
/// drawn at alpha `a` by `draw`.
fn row(draw: Draw, gpu: bool, a: usize, off: (f64, f64)) -> Vec<u8> {
    let mut canvas = Canvas::with_options(
        LEVELS as f32,
        ROWS,
        CanvasOptions {
            gpu,
            ..CanvasOptions::default()
        },
    )
    .expect("a canvas");
    let ctx = canvas.context();
    for c in 0..u8::MAX as usize {
        let x = c as f32;
        let css = grey(c, a, off);
        match draw {
            Draw::Fill => {
                ctx.set_fill_style_css(&css).expect("a colour");
                ctx.fill_rect(x, 0.0, 1.0, ROWS);
            }
            Draw::Stroke => {
                ctx.set_stroke_style_css(&css).expect("a colour");
                ctx.begin_path();
                ctx.move_to(x + 0.5, -2.0);
                ctx.line_to(x + 0.5, ROWS + 2.0);
                ctx.stroke();
            }
            Draw::GlobalAlpha => {
                let level = c as f64 + off.0;
                ctx.set_fill_style_css(&format!(
                    "rgb({level} {level} {level})"
                ))
                .expect("a colour");
                ctx.set_global_alpha(((a as f64 + off.1) / LEVELS) as f32);
                ctx.fill_rect(x, 0.0, 1.0, ROWS);
                ctx.set_global_alpha(1.0);
            }
            Draw::Texture => {
                let colour = RgbaLinear::from_css(&css).expect("a colour");
                ctx.set_fill_texture(&solid(colour));
                ctx.fill_rect(x, 0.0, 1.0, ROWS);
            }
            Draw::Filter => {
                // A CanvasKit blend that takes the colour wherever a fill
                // lands, over a white one.
                let colour = RgbaLinear::from_css(&css).expect("a colour");
                let filter = ColorFilter::blend(colour, BlendMode::SourceIn)
                    .expect("a filter");
                ctx.set_color_filter(Some(&filter));
                ctx.set_fill_style_css("white").expect("a colour");
                ctx.fill_rect(x, 0.0, 1.0, ROWS);
                ctx.set_color_filter(None);
            }
            Draw::Text => {
                // The block clipped to its column, so that each column is
                // one colour's glyph.
                ctx.set_fill_style_css(&css).expect("a colour");
                assert!(ctx.set_font_css(BLOCK_FONT), "the font parses");
                ctx.save();
                ctx.begin_path();
                ctx.rect(x, 0.0, 1.0, ROWS);
                ctx.clip(FillRule::NonZero);
                ctx.fill_text(BLOCK, x - 10.0, 20.0, None);
                ctx.restore();
            }
        }
    }
    ctx.get_image_data_as(
        0.0,
        1.0,
        LEVELS as f32,
        1.0,
        PixelExportOptions {
            premultiplied: true,
            ..PixelExportOptions::default()
        },
    )
    .expect("a row")
    .pixels()
    .to_vec()
}

/// The pairs `draw` stores apart from the exact product rounded half up,
/// over every grey and alpha below 255, each as `(c, a, stored, exact)`.
fn misses(
    draw: Draw,
    gpu: bool,
    off: (f64, f64),
) -> Vec<(usize, usize, [u8; 4], [u8; 4])> {
    (0..u8::MAX as usize)
        .flat_map(|a| {
            let pixels = row(draw, gpu, a, off);
            let alpha = a as f64 + off.1;
            let stored_alpha = (alpha + 0.5).floor() as u8;
            (0..u8::MAX as usize)
                .filter_map(move |c| {
                    let level = ((c as f64 + off.0) * alpha / LEVELS + 0.5)
                        .floor() as u8;
                    let exact = [level, level, level, stored_alpha];
                    let stored: [u8; 4] =
                        pixels[c * 4..c * 4 + 4].try_into().expect("a pixel");
                    (stored != exact).then_some((c, a, stored, exact))
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

/// No pair off the grid is within a billionth of a tie, so every expected
/// byte is the product's and not a coin toss between two.
#[test]
fn the_off_grid_sweep_has_no_ties() {
    let near = (0..255)
        .flat_map(|a| (0..255).map(move |c| (c, a)))
        .filter(|&(c, a)| {
            let product =
                (c as f64 + OFF_GRID.0) * (a as f64 + OFF_GRID.1) / LEVELS;
            (product.fract() - 0.5).abs() < 1e-9
        })
        .count();
    assert_eq!(near, 0);
}

/// The parser keeps a fraction of a level: an opaque grey of 100.37 stores
/// 100 and one of 100.63 stores 101, which it could not if the sweep's
/// colours reached the page already rounded.
#[test]
fn a_colour_s_fraction_of_a_level_reaches_the_page() {
    let mut canvas = Canvas::with_options(2.0, 1.0, CanvasOptions::default())
        .expect("a canvas");
    let ctx = canvas.context();
    for (x, level) in [(0.0, 100.37), (1.0, 100.63)] {
        ctx.set_fill_style_css(&format!("rgb({level} {level} {level})"))
            .expect("a colour");
        ctx.fill_rect(x, 0.0, 1.0, 1.0);
    }
    let data = ctx
        .get_image_data_as(0.0, 0.0, 2.0, 1.0, PixelExportOptions::default())
        .expect("pixels");
    assert_eq!(data.pixels(), [100, 100, 100, 255, 101, 101, 101, 255]);
}

/// Every draw stores the exact product of a colour and alpha on the byte
/// grid -- the control, which the legacy blit meets as well -- and off it.
#[test]
fn a_translucent_draw_stores_its_exact_product() {
    let mut failures = vec![];
    for gpu in ENGINES {
        for draw in DRAWS {
            for off in [(0.0, 0.0), OFF_GRID] {
                let missed = misses(draw, gpu, off);
                if let Some(first) = missed.first() {
                    failures.push(format!(
                        "{draw:?} gpu {gpu} off {off:?}: {} of 65025, first (c, a, stored, exact) {first:?}",
                        missed.len()
                    ));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// The alpha of every pixel of an "O" drawn in `grey` at `alpha`, both in
/// levels, on an eight-bit sRGB canvas on `gpu`'s engine: its coverage, in
/// the contrast Skia sets its glyph mask from the colour drawn.
fn edges(grey: f64, alpha: f64, gpu: bool) -> Vec<u8> {
    let mut canvas = Canvas::with_options(
        48.0,
        48.0,
        CanvasOptions {
            gpu,
            ..CanvasOptions::default()
        },
    )
    .expect("a canvas");
    let ctx = canvas.context();
    ctx.set_fill_style_css(&format!(
        "rgb({grey} {grey} {grey} / {})",
        alpha / LEVELS
    ))
    .expect("a colour");
    assert!(ctx.set_font_css(BLOCK_FONT), "the font parses");
    ctx.fill_text("O", 8.0, 38.0, None);
    ctx.get_image_data(0.0, 0.0, 48.0, 48.0)
        .expect("pixels")
        .pixels()
        .chunks(4)
        .map(|pixel| pixel[3])
        .collect()
}

/// Text in a colour whose exact product a colour inside its own contrast
/// bucket stores draws the glyph's edges as any colour in that bucket does.
/// Grey 127.37 at an alpha of 151.61 levels is 75.73, which 127 stores at
/// the alpha's byte, 152, as well as 127.5, the colour the product over the
/// alpha would be, in the next bucket; drawn in 127, its "O" takes the
/// coverage grey 112 at the same alpha does.
#[test]
fn text_in_its_buckets_exact_colour_keeps_its_edges() {
    for gpu in ENGINES {
        assert_eq!(
            edges(127.37, 151.61, gpu),
            edges(112.0, 151.61, gpu),
            "gpu {gpu}"
        );
    }
}

/// Text in a colour whose exact product no colour of its own contrast
/// bucket stores still stores the product inside the glyph: grey 31.37 at
/// an alpha of 151.61 levels is 18.65, which rounds to 19, and of the
/// colours that store 19 at the alpha's byte only 32, in the next bucket,
/// does.
#[test]
fn text_with_no_exact_colour_in_its_bucket_stores_the_product_inside() {
    for gpu in ENGINES {
        let pixels = row(Draw::Text, gpu, 151, OFF_GRID);
        assert_eq!(&pixels[31 * 4..32 * 4], [19, 19, 19, 152], "gpu {gpu}");
    }
}

/// The side of the page [`a_whole_page_fill_on_the_gpu_rounds_a_tie_up`]
/// fills, large enough that Ganesh takes a fill covering it as a clear.
const CLEARED_SIDE: f32 = 300.0;

/// The byte at the middle of a `CLEARED_SIDE` page after an opaque fill of
/// `css` over `rect` on `gpu`'s engine, or `None` where that engine is not
/// available.
fn filled_middle(gpu: bool, css: &str, rect: [f32; 4]) -> Option<u8> {
    let mut canvas = Canvas::with_options(
        CLEARED_SIDE,
        CLEARED_SIDE,
        CanvasOptions {
            gpu,
            ..CanvasOptions::default()
        },
    )
    .expect("a canvas");
    if gpu && canvas.engine_kind() != EngineKind::Gpu {
        return None;
    }
    let ctx = canvas.context();
    ctx.set_fill_style_css(css).expect("a colour");
    let [x, y, w, h] = rect;
    ctx.fill_rect(x, y, w, h);
    let middle = CLEARED_SIDE / 2.0;
    Some(
        ctx.get_image_data(middle, middle, 1.0, 1.0)
            .expect("a pixel")
            .pixels()[0],
    )
}

/// An opaque grey on a tie filling the whole page on the GPU rounds up, as a
/// fill short of the page does. Ganesh draws a fill covering the page as a
/// clear, which stored each of the 127 ties from 128.5 to 254.5 a level low
/// where a fill half a pixel short of the left edge stored it rounded up;
/// the pixels recording hands it the colour placed on its byte, which the
/// clear stores as it is.
#[test]
fn a_whole_page_fill_on_the_gpu_rounds_a_tie_up() {
    let page = [0.0, 0.0, CLEARED_SIDE, CLEARED_SIDE];
    let short = [0.5, 0.0, CLEARED_SIDE, CLEARED_SIDE];
    let mut low = vec![];
    for n in 128..255u32 {
        let level = f64::from(n) + 0.5;
        let css = format!("rgb({level} {level} {level})");
        let (Some(cleared), Some(control)) = (
            filled_middle(true, &css, page),
            filled_middle(true, &css, short),
        ) else {
            return;
        };
        let want = (level + 0.5).floor() as u8;
        assert_eq!(control, want, "the control at {level}");
        if cleared != want {
            low.push((level, cleared));
        }
    }
    assert!(
        low.is_empty(),
        "{} of 127 ties stored off: {low:?}",
        low.len()
    );
}
