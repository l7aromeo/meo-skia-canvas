//! Timing and memory, measured rather than asserted.
//!
//! Run with:
//!
//!     just bench-crate      # cargo run --release --example benchmark
//!     just bench            # this and examples/node/benchmark.js
//!
//! Release on purpose. A dev build leaves the Rust glue unoptimized, which
//! moves the per-call overhead without touching Skia, so the ratios come out
//! right and the milliseconds do not.
//!
//! Flags, after `--`:
//!
//!     --list         print every case id, one a line, and time nothing
//!     --quick        one run of each case and no warmup: a smoke test
//!     --only PREFIX  run only the cases whose id starts with PREFIX
//!     --out DIR      where the report goes (default target/bench)
//!
//! Every case has an id, and `examples/node/benchmark.js` runs the same ids
//! in the same order through the Node binding, so the two reports line up row
//! by row and their ratio is what the binding costs.
//! `scripts/check-bench-parity.mjs` holds the two lists equal; `just ci` runs
//! it.
//!
//! Each run writes `<date>-<commit>-crate.md` and `.json` to the report
//! directory: per case the median, p10 and p90 of its runs, and for the run
//! the executable, the commit, the machine and the load average at both
//! ends.
//!
//! The median of N runs after a warmup, because the first draw on a backend
//! pays for shader compilation and surface allocation and is not what a
//! caller experiences in steady state. Every figure is one machine and one
//! GPU; treat the ratios as the transferable part, not the milliseconds.

use std::{
    cell::RefCell,
    error::Error,
    f32::consts::PI,
    path::{Path, PathBuf},
    process::Command,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use meo_skia_canvas::prelude::*;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

const WIDTH: f32 = 1200.0;
const HEIGHT: f32 = 900.0;

/// The three formats a page can be held in, widest last.
const DEPTHS: [(&str, PixelDepth); 3] = [
    ("RGBA8888", PixelDepth::Uint8),
    ("RGBAF16", PixelDepth::F16),
    ("RGBAF32", PixelDepth::F32),
];

/// The pages every drawing case runs on: the default, and the two where
/// 7.0's accepted costs live -- an eight-bit wide-gamut page, whose colours
/// are held exactly in their own space, and a half-float one.
const SPACES: [(&str, PixelColorSpace, PixelDepth); 3] = [
    ("srgb", PixelColorSpace::Srgb, PixelDepth::Uint8),
    ("p3", PixelColorSpace::DisplayP3, PixelDepth::Uint8),
    ("p3-f16", PixelColorSpace::DisplayP3, PixelDepth::F16),
];

const ENGINES: [(&str, bool); 2] = [("cpu", false), ("gpu", true)];

/// Runs and warmup for most cases. `--quick` takes one run and no warmup
/// everywhere, which proves every case still runs and measures nothing.
const RUNS: (usize, usize) = (8, 2);
/// For whole animations and AVIF, each most of a second.
const LONG_RUNS: (usize, usize) = (5, 1);

const BYTES_PER_MIB: f64 = 1_048_576.0;

// ── measurement ────────────────────────────────────────────────────────────

/// One case's result: durations in milliseconds, or a value of another unit
/// with no spread.
#[derive(Default)]
struct Measure {
    unit: &'static str,
    median: f64,
    p10: Option<f64>,
    p90: Option<f64>,
    runs: usize,
    bytes: Option<usize>,
    surface: Option<f64>,
}

/// The upper middle on an even count, as `examples/node/benchmark.js` takes
/// it: averaging the two would invent a duration nothing actually took.
fn quantile(samples: &[f64], q: f64) -> f64 {
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    let at = ((q * sorted.len() as f64) as usize).min(sorted.len() - 1);
    sorted[at]
}

/// Runs `work` `warmup` times to settle the caches, then `runs` times for the
/// record.
fn time(
    quick: bool,
    (runs, warmup): (usize, usize),
    mut work: impl FnMut() -> Result<()>,
) -> Result<Measure> {
    let (runs, warmup) = if quick { (1, 0) } else { (runs, warmup) };
    for _ in 0..warmup {
        work()?;
    }
    let mut samples = Vec::with_capacity(runs);
    for _ in 0..runs {
        let started = Instant::now();
        work()?;
        samples.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    Ok(Measure {
        unit: "ms",
        median: quantile(&samples, 0.5),
        p10: Some(quantile(&samples, 0.1)),
        p90: Some(quantile(&samples, 0.9)),
        runs,
        ..Measure::default()
    })
}

/// A measured value that is not a duration: a fidelity in decibels, an
/// error in levels, a residency in megabytes.
fn value(unit: &'static str, median: f64) -> Measure {
    Measure {
        unit,
        median,
        runs: 1,
        ..Measure::default()
    }
}

/// Reading one pixel back forces the recording to rasterize. Without it the
/// timing measures how fast commands are appended to a picture, not drawing.
fn rasterize(canvas: &mut Canvas) -> Result<()> {
    canvas.context().get_image_data(0.0, 0.0, 1.0, 1.0)?;
    Ok(())
}

fn options(
    gpu: bool,
    space: PixelColorSpace,
    depth: PixelDepth,
) -> CanvasOptions {
    CanvasOptions {
        gpu,
        color_space: space,
        color_type: depth,
        ..CanvasOptions::default()
    }
}

fn cpu() -> CanvasOptions {
    options(false, PixelColorSpace::Srgb, PixelDepth::Uint8)
}

/// Scatters `index` across `span` without a random number generator, which
/// would make one run's timing incomparable with the next. The strides are
/// the ones `examples/node/benchmark.js` uses.
fn scatter(index: usize, stride: usize, span: f32) -> f32 {
    (index * stride) as f32 % span
}

// ── what is drawn ──────────────────────────────────────────────────────────

fn curves(ctx: &mut Context2D, count: usize) -> Result<()> {
    for i in 0..count {
        ctx.begin_path();
        ctx.move_to(scatter(i, 37, WIDTH), scatter(i, 53, HEIGHT));
        ctx.bezier_curve_to(
            scatter(i, 71, WIDTH),
            scatter(i, 29, HEIGHT),
            scatter(i, 13, WIDTH),
            scatter(i, 97, HEIGHT),
            scatter(i, 41, WIDTH),
            scatter(i, 61, HEIGHT),
        );
        // Through the CSS string rather than `RgbaLinear`, because `hsl()` is
        // how the JavaScript mirror writes it and the point here is that the
        // two sides describe the same colour.
        ctx.set_stroke_style_css(&format!("hsl({} 70% 60%)", (i * 7) % 360))?;
        ctx.set_line_width(1.0 + (i % 4) as f32);
        ctx.stroke();
    }
    Ok(())
}

fn panels(ctx: &mut Context2D, shadow: bool) -> Result<()> {
    for i in 0..60 {
        ctx.save();
        if shadow {
            ctx.set_shadow_color(RgbaLinear::from_srgb8(0, 0, 0, 0.5));
            ctx.set_shadow_blur(12.0);
        }
        ctx.set_fill_style_css(&format!("hsl({} 60% 55%)", (i * 13) % 360))?;
        ctx.begin_path();
        ctx.round_rect(
            20.0 + scatter(i, 19, WIDTH - 140.0),
            20.0 + scatter(i, 31, HEIGHT - 90.0),
            120.0,
            70.0,
            [10.0; 4],
        )?;
        ctx.fill(FillRule::NonZero);
        ctx.restore();
    }
    Ok(())
}

fn lines(ctx: &mut Context2D, count: usize) -> Result<()> {
    ctx.set_font(&Font::parse("600 28px Helvetica")?);
    ctx.set_fill_style_css("#e6edf3")?;
    for i in 0..count {
        ctx.fill_text(
            &format!("Throughput sample {i}"),
            40.0,
            40.0 + i as f32 * 21.0,
            None,
        );
    }
    Ok(())
}

/// A mixed vector scene: curves, shadowed panels and text, in the proportions
/// a chart or report actually draws them.
fn scene(ctx: &mut Context2D) -> Result<()> {
    let background = Shader::linear_gradient(
        Point::new(0.0, 0.0),
        Point::new(0.0, HEIGHT),
        &[
            GradientStop {
                position: 0.0,
                color: RgbaLinear::from_hex("#0f1b2d")?,
            },
            GradientStop {
                position: 1.0,
                color: RgbaLinear::from_hex("#1b2b45")?,
            },
        ],
        GradientColorSpace::Destination,
    )?;
    ctx.set_fill_shader(&background);
    ctx.fill_rect(0.0, 0.0, WIDTH, HEIGHT);
    curves(ctx, 300)?;
    panels(ctx, true)?;
    lines(ctx, 40)
}

const PARAGRAPH: &str = "Benchmarks measure what they draw and nothing \
    else, so a paragraph that wraps is its own case: breaking lines, shaping \
    each and laying them out is work a single line of text never asks for. ";

fn hsl(css: &str) -> Result<RgbaLinear> {
    RgbaLinear::from_css(css)
        .ok_or_else(|| format!("not a colour: {css}").into())
}

/// Each the same 60 rectangles, so the cases differ in what fills them.
fn rects(
    ctx: &mut Context2D,
    mut style: impl FnMut(&mut Context2D, usize) -> Result<()>,
) -> Result<()> {
    for i in 0..60 {
        style(ctx, i)?;
        ctx.fill_rect(
            scatter(i, 19, WIDTH - 160.0),
            scatter(i, 31, HEIGHT - 120.0),
            160.0,
            120.0,
        );
    }
    Ok(())
}

fn flat(ctx: &mut Context2D, i: usize) -> Result<()> {
    ctx.set_fill_style_css(&format!("hsl({} 60% 55%)", (i * 13) % 360))?;
    Ok(())
}

/// A linear, a radial and a conic gradient in turn, as the JavaScript side
/// builds them with `createLinearGradient`, `createRadialGradient` and
/// `createConicGradient`.
fn gradient(i: usize, space: GradientColorSpace) -> Result<Shader> {
    let x = scatter(i, 19, WIDTH - 160.0);
    let y = scatter(i, 31, HEIGHT - 120.0);
    let stops = [
        GradientStop {
            position: 0.0,
            color: hsl(&format!("hsl({} 80% 50%)", (i * 13) % 360))?,
        },
        GradientStop {
            position: 0.5,
            color: RgbaLinear::from_hex("#f0e6d2")?,
        },
        GradientStop {
            position: 1.0,
            color: hsl(&format!("hsl({} 80% 40%)", (i * 13 + 180) % 360))?,
        },
    ];
    let centre = Point::new(x + 80.0, y + 60.0);
    Ok(match i % 3 {
        0 => Shader::linear_gradient(
            Point::new(x, y),
            Point::new(x + 160.0, y + 120.0),
            &stops,
            space,
        )?,
        1 => Shader::two_point_conical_gradient(
            centre, 4.0, centre, 90.0, &stops, space,
        )?,
        _ => {
            // `createConicGradient` takes its start in radians and sweeps a
            // full turn.
            let start = (i as f32 / 10.0).to_degrees();
            Shader::sweep_gradient(centre, start, start + 360.0, &stops, space)?
        }
    })
}

/// Fixtures a case reads but does not time, made on first use.
struct Lazy<T>(RefCell<Option<T>>, fn() -> Result<T>);

impl<T> Lazy<T> {
    const fn new(make: fn() -> Result<T>) -> Self {
        Self(RefCell::new(None), make)
    }

    fn with<R>(&self, use_it: impl FnOnce(&mut T) -> Result<R>) -> Result<R> {
        let mut held = self.0.borrow_mut();
        if held.is_none() {
            *held = Some((self.1)()?);
        }
        match held.as_mut() {
            Some(made) => use_it(made),
            None => Err("a fixture that was just made is gone".into()),
        }
    }
}

/// A 64-pixel tile: four quadrants and a disc, so a pattern of it has edges.
fn tile() -> Result<Canvas> {
    let mut tile = Canvas::with_options(64.0, 64.0, cpu())?;
    let c = tile.context();
    c.set_fill_style_css("#c82878")?;
    c.fill_rect(0.0, 0.0, 32.0, 32.0);
    c.set_fill_style_css("#14a05a")?;
    c.fill_rect(32.0, 32.0, 32.0, 32.0);
    c.set_fill_style_css("rgba(30 60 220 / 0.8)")?;
    c.begin_path();
    c.arc(34.0, 30.0, 18.0, 0.0, 2.0 * PI, false)?;
    c.fill(FillRule::NonZero);
    Ok(tile)
}

/// A canvas holding 200 rectangles, drawn into the page whole.
fn inner_page() -> Result<Canvas> {
    let mut inner = Canvas::with_options(600.0, 450.0, cpu())?;
    let ic = inner.context();
    ic.set_fill_style_css("#0b0e14")?;
    ic.fill_rect(0.0, 0.0, 600.0, 450.0);
    for i in 0..200 {
        ic.set_fill_style_css(&format!("hsl({} 70% 50%)", (i * 9) % 360))?;
        ic.fill_rect(scatter(i, 31, 600.0), scatter(i, 17, 450.0), 130.0, 70.0);
    }
    Ok(inner)
}

fn photo() -> Result<Image> {
    let mut photo = Canvas::with_options(256.0, 256.0, cpu())?;
    let c = photo.context();
    let wash = Shader::linear_gradient(
        Point::new(0.0, 0.0),
        Point::new(256.0, 256.0),
        &[
            GradientStop {
                position: 0.0,
                color: RgbaLinear::from_hex("#205080")?,
            },
            GradientStop {
                position: 1.0,
                color: RgbaLinear::from_hex("#e0a040")?,
            },
        ],
        GradientColorSpace::Destination,
    )?;
    c.set_fill_shader(&wash);
    c.fill_rect(0.0, 0.0, 256.0, 256.0);
    curves(c, 40)?;
    let png = photo.to_buffer(ImageFormat::Png, &EncodeOptions::default())?;
    Ok(Image::from_encoded(&png)?)
}

fn page() -> Result<Canvas> {
    let mut page = Canvas::with_options(WIDTH, HEIGHT, cpu())?;
    scene(page.context())?;
    Ok(page)
}

/// The 256-pixel page an icon can hold: ICO stores nothing larger.
fn icon_page() -> Result<Canvas> {
    let mut page = Canvas::with_options(256.0, 256.0, cpu())?;
    scene(page.context())?;
    Ok(page)
}

/// A still background with a moving foreground, which is what a
/// dirty-rectangle encoder is actually asked to compress. A scene where the
/// whole page moves would make every rectangle the whole page and measure
/// something nobody exports.
fn reel() -> Result<Canvas> {
    let mut reel = Canvas::with_options(WIDTH, HEIGHT, cpu())?;
    for frame in 0..30 {
        let ctx = match frame {
            0 => reel.context(),
            _ => reel.new_page(),
        };
        scene(ctx)?;
        ctx.set_fill_style_css("#f6c453")?;
        ctx.begin_path();
        ctx.arc(
            100.0 + (WIDTH - 200.0) * frame as f32 / 29.0,
            HEIGHT / 2.0,
            80.0,
            0.0,
            2.0 * PI,
            false,
        )?;
        ctx.fill(FillRule::NonZero);
    }
    Ok(reel)
}

/// Every fixture the cases share.
struct Fixtures {
    tile: Lazy<Canvas>,
    tile_image: Lazy<Image>,
    inner: Lazy<Canvas>,
    photo: Lazy<Image>,
    page: Lazy<Canvas>,
    icon_page: Lazy<Canvas>,
    reel: Lazy<Canvas>,
    heavy_page: RefCell<Option<Canvas>>,
    nested_200: Lazy<Canvas>,
    nested_20000: Lazy<Canvas>,
}

impl Fixtures {
    fn new() -> Self {
        Self {
            tile: Lazy::new(tile),
            tile_image: Lazy::new(|| {
                let png = tile()?
                    .to_buffer(ImageFormat::Png, &EncodeOptions::default())?;
                Ok(Image::from_encoded(&png)?)
            }),
            inner: Lazy::new(inner_page),
            photo: Lazy::new(photo),
            page: Lazy::new(page),
            icon_page: Lazy::new(icon_page),
            reel: Lazy::new(reel),
            heavy_page: RefCell::new(None),
            nested_200: Lazy::new(|| nested_source(200)),
            nested_20000: Lazy::new(|| nested_source(20000)),
        }
    }

    /// A page holding what a document has to work hardest at: a canvas
    /// pattern, blurred and shadowed shapes, gradients and text.
    fn heavy_page<R>(
        &self,
        use_it: impl FnOnce(&mut Canvas) -> Result<R>,
    ) -> Result<R> {
        let mut held = self.heavy_page.borrow_mut();
        if held.is_none() {
            let mut page = Canvas::with_options(WIDTH, HEIGHT, cpu())?;
            let ctx = page.context();
            scene(ctx)?;
            ctx.save();
            let pattern = self.tile.with(|tile| {
                Ok(ctx.create_pattern_from_canvas(tile, PatternRepeat::Repeat))
            })?;
            ctx.set_fill_pattern(&pattern);
            ctx.fill_rect(40.0, 40.0, 500.0, 300.0);
            ctx.set_filter_css("blur(3px)")?;
            rects(ctx, |ctx, i| {
                ctx.set_fill_shader(&gradient(
                    i,
                    GradientColorSpace::Destination,
                )?);
                Ok(())
            })?;
            ctx.restore();
            *held = Some(page);
        }
        match held.as_mut() {
            Some(page) => use_it(page),
            None => Err("the heavy page that was just made is gone".into()),
        }
    }
}

type Paint = fn(&mut Context2D, &Fixtures) -> Result<()>;

/// What each drawing case paints, by name. The two layer workloads pull in
/// opposite directions on a float page -- blending translucent layers against
/// filling opaque ones -- so a single "float costs Nx" figure would be false
/// either way.
const DRAWS: [(&str, Paint); 16] = [
    ("scene", |ctx, _| scene(ctx)),
    ("curves", |ctx, _| curves(ctx, 300)),
    ("dashes", |ctx, _| {
        ctx.set_line_dash(&[12.0, 6.0]);
        ctx.set_line_width(2.0);
        for i in 0..200 {
            ctx.set_stroke_style_css(&format!(
                "hsl({} 70% 55%)",
                (i * 11) % 360
            ))?;
            ctx.begin_path();
            ctx.move_to(scatter(i, 37, WIDTH), scatter(i, 53, HEIGHT));
            ctx.line_to(scatter(i, 71, WIDTH), scatter(i, 29, HEIGHT));
            ctx.stroke();
        }
        Ok(())
    }),
    ("text", |ctx, _| lines(ctx, 120)),
    ("wrapped-text", |ctx, _| {
        if !ctx.set_font_css("18px Helvetica") {
            return Err("18px Helvetica is a font shorthand".into());
        }
        ctx.set_fill_style_css("#1b2b45")?;
        ctx.set_text_wrap(true);
        for i in 0..6 {
            ctx.fill_text(
                &PARAGRAPH.repeat(4),
                20.0,
                30.0 + i as f32 * 140.0,
                Some(WIDTH - 40.0),
            );
        }
        Ok(())
    }),
    ("gradients", |ctx, _| {
        rects(ctx, |ctx, i| {
            ctx.set_fill_shader(&gradient(i, GradientColorSpace::Destination)?);
            Ok(())
        })
    }),
    ("gradients-oklab", |ctx, _| {
        rects(ctx, |ctx, i| {
            ctx.set_fill_shader(&gradient(i, GradientColorSpace::Oklab)?);
            Ok(())
        })
    }),
    ("pattern-image", |ctx, fixtures| {
        let pattern = fixtures.tile_image.with(|image| {
            Ok(ctx.create_pattern(image, PatternRepeat::Repeat))
        })?;
        ctx.set_fill_pattern(&pattern);
        ctx.fill_rect(0.0, 1.0, WIDTH, HEIGHT - 1.0);
        Ok(())
    }),
    ("pattern-canvas", |ctx, fixtures| {
        let pattern = fixtures.tile.with(|tile| {
            Ok(ctx.create_pattern_from_canvas(tile, PatternRepeat::Repeat))
        })?;
        ctx.set_fill_pattern(&pattern);
        ctx.fill_rect(0.0, 1.0, WIDTH, HEIGHT - 1.0);
        Ok(())
    }),
    ("blur", |ctx, _| {
        ctx.set_filter_css("blur(4px)")?;
        rects(ctx, flat)
    }),
    ("shadows", |ctx, _| panels(ctx, true)),
    ("clip", |ctx, _| {
        ctx.begin_path();
        ctx.arc(WIDTH / 2.0, HEIGHT / 2.0, 380.0, 0.0, 2.0 * PI, false)?;
        ctx.clip(FillRule::NonZero);
        curves(ctx, 150)?;
        rects(ctx, flat)
    }),
    ("nested", |ctx, fixtures| {
        fixtures.inner.with(|inner| {
            for i in 0..8 {
                ctx.draw_canvas_sized(
                    inner,
                    scatter(i, 97, 600.0),
                    scatter(i, 61, 450.0),
                    300.0,
                    225.0,
                );
            }
            Ok(())
        })
    }),
    ("images", |ctx, fixtures| {
        fixtures.photo.with(|photo| {
            for i in 0..100 {
                ctx.draw_image_sized(
                    photo,
                    scatter(i, 37, WIDTH - 128.0),
                    scatter(i, 53, HEIGHT - 128.0),
                    128.0,
                    128.0,
                );
            }
            Ok(())
        })
    }),
    ("translucent", |ctx, _| {
        for i in 0..120 {
            ctx.set_global_alpha(0.02);
            ctx.set_fill_style_css(if i % 2 == 1 {
                "#ff8844"
            } else {
                "#4488ff"
            })?;
            ctx.fill_rect(0.0, 0.0, WIDTH, HEIGHT);
        }
        Ok(())
    }),
    // Inset by a pixel on purpose. An opaque fill that covers the whole page
    // lets Skia discard everything recorded under it, so a loop of them
    // measures the cull rather than the fill: 1200 of them came in at 2 ms,
    // which is not 1200 fills of anything. One pixel short and every layer
    // is drawn.
    ("opaque", |ctx, _| {
        for i in 0..120 {
            ctx.set_fill_style_css(if i % 2 == 1 {
                "#ff8844"
            } else {
                "#4488ff"
            })?;
            ctx.fill_rect(0.0, 1.0, WIDTH, HEIGHT - 1.0);
        }
        Ok(())
    }),
];

/// A canvas drawn into a canvas, which has itself drawn a canvas: only such a
/// source takes the clipped path below. One that has only been drawn on
/// travels as a picture and is never rasterized.
fn nested_source(ops: usize) -> Result<Canvas> {
    let mut inner = Canvas::with_options(WIDTH, HEIGHT, cpu())?;
    let ic = inner.context();
    ic.set_fill_style_css("#0b0e14")?;
    ic.fill_rect(0.0, 0.0, WIDTH, HEIGHT);
    for i in 0..ops {
        ic.set_fill_style_css(&format!("hsl({} 70% 50%)", (i * 9) % 360))?;
        ic.fill_rect(
            scatter(i, 31, WIDTH),
            scatter(i, 17, HEIGHT),
            260.0,
            140.0,
        );
    }
    let mut source = Canvas::with_options(WIDTH, HEIGHT, cpu())?;
    source.context().draw_canvas(&mut inner, 0.0, 0.0);
    Ok(source)
}

fn encoding(quality: Option<f32>) -> EncodeOptions {
    EncodeOptions {
        quality,
        ..EncodeOptions::default()
    }
}

/// Each encoder's case name, format and options, and whether it is handed the
/// icon-sized page. AVIF's own dials close the list: the one format whose
/// choices move both time and size.
fn encodes() -> Vec<(&'static str, ImageFormat, EncodeOptions, bool)> {
    let avif = |quality, chroma, lossless| EncodeOptions {
        quality,
        chroma,
        lossless,
        ..EncodeOptions::default()
    };
    vec![
        ("png", ImageFormat::Png, encoding(None), false),
        ("jpeg", ImageFormat::Jpeg, encoding(Some(0.92)), false),
        ("webp", ImageFormat::Webp, encoding(Some(0.9)), false),
        ("avif", ImageFormat::Avif, encoding(Some(0.92)), false),
        ("gif", ImageFormat::Gif, encoding(None), false),
        ("apng", ImageFormat::Apng, encoding(None), false),
        ("tiff", ImageFormat::Tiff, encoding(None), false),
        ("bmp", ImageFormat::Bmp, encoding(None), false),
        ("ico", ImageFormat::Ico, encoding(None), true),
        ("pdf", ImageFormat::Pdf, encoding(None), false),
        ("svg", ImageFormat::Svg, encoding(None), false),
        ("avif-q0.5", ImageFormat::Avif, encoding(Some(0.5)), false),
        ("avif-q1", ImageFormat::Avif, encoding(Some(1.0)), false),
        (
            "avif-422",
            ImageFormat::Avif,
            avif(Some(0.92), Some(ChromaSampling::Half), false),
            false,
        ),
        (
            "avif-420",
            ImageFormat::Avif,
            avif(Some(0.92), Some(ChromaSampling::Quarter), false),
            false,
        ),
        (
            "avif-lossless",
            ImageFormat::Avif,
            avif(None, None, true),
            false,
        ),
    ]
}

/// The four formats that carry a clock, at 30 frames a second.
fn animations() -> Vec<(&'static str, ImageFormat, EncodeOptions)> {
    let at = |quality| EncodeOptions {
        quality,
        fps: Some(30.0),
        ..EncodeOptions::default()
    };
    vec![
        ("webp", ImageFormat::Webp, at(Some(0.9))),
        ("apng", ImageFormat::Apng, at(None)),
        ("gif", ImageFormat::Gif, at(None)),
        ("avif", ImageFormat::Avif, at(Some(0.92))),
    ]
}

/// Each decoder handed a page its own encoder wrote. AVIF and APNG are this
/// library's own code -- Skia reads neither -- and the rest are Skia's.
const DECODES: [&str; 8] =
    ["png", "jpeg", "webp", "avif", "gif", "apng", "bmp", "ico"];

/// User units, no `font-family`, no text: the document where the walk this
/// library makes before Skia parses finds nothing to rewrite, so the row
/// measures the cost of looking. Two sizes, because the walk is per element.
fn svg_doc(elements: usize) -> String {
    let body: String = (0..elements)
        .map(|i| {
            format!(
                r##"<rect x="{}" y="{}" width="20" height="14" fill="#3a7"/>"##,
                (i * 7) % 900,
                (i * 13) % 700
            )
        })
        .collect();
    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="1000" height="800">{body}</svg>"#
    )
}

/// PSNR over the three colour channels, in decibels; alpha is skipped, since
/// the page is opaque.
fn psnr(source: &[u8], decoded: &[u8]) -> f64 {
    let squared: f64 = source
        .as_chunks::<4>()
        .0
        .iter()
        .zip(decoded.as_chunks::<4>().0)
        .flat_map(|(a, b)| {
            (0..3).map(move |c| (f64::from(a[c]) - f64::from(b[c])).powi(2))
        })
        .sum();
    let mse = squared / (source.len() / 4 * 3) as f64;
    match mse == 0.0 {
        true => f64::INFINITY,
        false => 10.0 * ((255.0 * 255.0) / mse).log10(),
    }
}

/// Canvases each memory probe holds.
const MEMORY_CANVASES: usize = 20;

type Case<'a> = (String, Box<dyn Fn() -> Result<Measure> + 'a>);

fn cases<'a>(fixtures: &'a Fixtures, quick: bool) -> Vec<Case<'a>> {
    let mut cases: Vec<Case<'a>> = Vec::new();
    let mut add =
        |id: String, measure: Box<dyn Fn() -> Result<Measure> + 'a>| {
            cases.push((id, measure))
        };

    for (name, paint) in DRAWS {
        for (engine, gpu) in ENGINES {
            for (space, colour, depth) in SPACES {
                add(
                    format!("draw/{name}/{engine}/{space}"),
                    Box::new(move || {
                        time(quick, RUNS, || {
                            let mut canvas = Canvas::with_options(
                                WIDTH,
                                HEIGHT,
                                options(gpu, colour, depth),
                            )?;
                            paint(canvas.context(), fixtures)?;
                            rasterize(&mut canvas)
                        })
                    }),
                );
            }
        }
    }

    // A whole page read back and written in, in the page's format: what a
    // filter or an image pipeline pays per frame.
    for (engine, gpu) in ENGINES {
        for (space, colour, depth) in SPACES {
            add(
                format!("pixels/get/{engine}/{space}"),
                Box::new(move || {
                    let mut canvas = Canvas::with_options(
                        WIDTH,
                        HEIGHT,
                        options(gpu, colour, depth),
                    )?;
                    let ctx = canvas.context();
                    scene(ctx)?;
                    time(quick, RUNS, || {
                        ctx.get_image_data(0.0, 0.0, WIDTH, HEIGHT)?;
                        Ok(())
                    })
                }),
            );
            add(
                format!("pixels/put/{engine}/{space}"),
                Box::new(move || {
                    let mut canvas = Canvas::with_options(
                        WIDTH,
                        HEIGHT,
                        options(gpu, colour, depth),
                    )?;
                    scene(canvas.context())?;
                    let data = canvas
                        .context()
                        .get_image_data(0.0, 0.0, WIDTH, HEIGHT)?;
                    time(quick, RUNS, || {
                        canvas.context().put_image_data(&data, 0.0, 0.0)?;
                        rasterize(&mut canvas)
                    })
                }),
            );
        }
    }

    // A canvas drawn into a canvas, behind a clip showing a sliver of it. The
    // cost belongs to the clip: the visible region is rasterized and the rest
    // of the source is culled. Two source sizes, because the failure this
    // catches is the cost tracking the source instead, and one figure cannot
    // show that -- a heavy source reads as a slow machine.
    for (ops, source) in
        [(200, &fixtures.nested_200), (20000, &fixtures.nested_20000)]
    {
        for (engine, gpu) in ENGINES {
            add(
                format!("nested/clipped-{ops}/{engine}"),
                Box::new(move || {
                    source.with(|source| {
                        time(quick, RUNS, || {
                            let mut dest = Canvas::with_options(
                                WIDTH,
                                HEIGHT,
                                options(
                                    gpu,
                                    PixelColorSpace::Srgb,
                                    PixelDepth::Uint8,
                                ),
                            )?;
                            let ctx = dest.context();
                            ctx.save();
                            ctx.begin_path();
                            ctx.rect(0.0, 0.0, 180.0, 24.0);
                            ctx.clip(FillRule::NonZero);
                            ctx.draw_canvas(source, 0.0, 0.0);
                            ctx.restore();
                            // Reads back a pixel, and refuses a transparent
                            // one: a clip that misses would otherwise be timed
                            // as an empty page.
                            match ctx
                                .get_image_data(0.0, 0.0, 1.0, 1.0)?
                                .pixels()[3]
                            {
                                255 => Ok(()),
                                _ => {
                                    Err("the clipped draw painted nothing"
                                        .into())
                                }
                            }
                        })
                    })
                }),
            );
        }
    }

    // Drawn onto a canvas rather than merely constructed, as the JavaScript
    // side has to: there an image's pixels are not decoded until something
    // asks for them. The blit is the same in every row, so the differences
    // between them are the decoders.
    let encode_list = encodes();
    for name in DECODES {
        let (_, format, encode_options, icon) = encode_list
            .iter()
            .find(|(label, ..)| *label == name)
            .cloned()
            .unwrap_or(("png", ImageFormat::Png, encoding(None), false));
        add(
            format!("decode/{name}/cpu"),
            Box::new(move || {
                let source = if icon {
                    &fixtures.icon_page
                } else {
                    &fixtures.page
                };
                let (encoded, width, height) = source.with(|page| {
                    Ok((
                        page.to_buffer(format, &encode_options)?,
                        page.width(),
                        page.height(),
                    ))
                })?;
                let mut into = Canvas::with_options(width, height, cpu())?;
                let mut measure = time(quick, RUNS, || {
                    let image = Image::from_encoded(&encoded)?;
                    into.context().draw_image(&image, 0.0, 0.0);
                    rasterize(&mut into)
                })?;
                measure.bytes = Some(encoded.len());
                Ok(measure)
            }),
        );
    }

    // Every frame of a 30-frame animation, each drawn: what playing one back
    // costs, which a decode of its first frame says nothing about.
    for (name, format, encode_options) in animations() {
        add(
            format!("decode/{name}-animated/cpu"),
            Box::new(move || {
                let encoded = fixtures.reel.with(|reel| {
                    Ok(reel.to_buffer(format, &encode_options)?)
                })?;
                let mut into = Canvas::with_options(WIDTH, HEIGHT, cpu())?;
                let mut measure = time(quick, LONG_RUNS, || {
                    let image = Image::from_encoded(&encoded)?;
                    for frame in 0..image.frame_count() {
                        into.context().draw_image(
                            &image.frame(frame)?,
                            0.0,
                            0.0,
                        );
                        rasterize(&mut into)?;
                    }
                    Ok(())
                })?;
                measure.bytes = Some(encoded.len());
                Ok(measure)
            }),
        );
    }

    for elements in [60, 2000] {
        add(
            format!("decode/svg-{elements}/cpu"),
            Box::new(move || {
                let source = svg_doc(elements);
                let mut into = Canvas::with_options(1000.0, 800.0, cpu())?;
                time(quick, RUNS, || {
                    let image = Image::from_svg_xml(&source, 1000, 800)?;
                    into.context().draw_image(&image, 0.0, 0.0);
                    rasterize(&mut into)
                })
            }),
        );
    }

    // Size beside the time, because neither figure means much alone: the
    // fastest encoder here writes the largest file and the slowest writes the
    // smallest, and a table of times would report that as a ranking.
    for (name, format, encode_options, icon) in encode_list {
        add(
            format!("encode/{name}/cpu"),
            Box::new(move || {
                let source = if icon {
                    &fixtures.icon_page
                } else {
                    &fixtures.page
                };
                let runs = if name.starts_with("avif") {
                    LONG_RUNS
                } else {
                    RUNS
                };
                source.with(|page| {
                    let mut measure = time(quick, runs, || {
                        page.to_buffer(format, &encode_options)?;
                        Ok(())
                    })?;
                    measure.bytes =
                        Some(page.to_buffer(format, &encode_options)?.len());
                    Ok(measure)
                })
            }),
        );
    }

    // The four formats that carry a clock do their work between frames: each
    // sends the rectangle it differs from its predecessor in, and compresses
    // frames on whatever cores are free. A one-page export reaches none of
    // that.
    for (name, format, encode_options) in animations() {
        add(
            format!("animate/{name}/cpu"),
            Box::new(move || {
                fixtures.reel.with(|reel| {
                    let mut measure = time(quick, LONG_RUNS, || {
                        reel.to_buffer(format, &encode_options)?;
                        Ok(())
                    })?;
                    measure.bytes =
                        Some(reel.to_buffer(format, &encode_options)?.len());
                    Ok(measure)
                })
            }),
        );
    }

    for (name, format) in [("pdf", ImageFormat::Pdf), ("svg", ImageFormat::Svg)]
    {
        add(
            format!("export/{name}-heavy/cpu"),
            Box::new(move || {
                fixtures.heavy_page(|page| {
                    let defaults = EncodeOptions::default();
                    let mut measure = time(quick, RUNS, || {
                        page.to_buffer(format, &defaults)?;
                        Ok(())
                    })?;
                    measure.bytes =
                        Some(page.to_buffer(format, &defaults)?.len());
                    Ok(measure)
                })
            }),
        );
    }

    // What the lossy encoders cost in fidelity, beside what they cost in
    // bytes: +6 dB is half the error, and a difference under about 1 dB is not
    // worth reading. PSNR is per-pixel error and knows nothing about vision.
    for (name, format, quality) in [
        ("jpeg", ImageFormat::Jpeg, 0.92),
        ("webp", ImageFormat::Webp, 0.9),
        ("avif", ImageFormat::Avif, 0.92),
    ] {
        add(
            format!("quality/{name}"),
            Box::new(move || {
                fixtures.page.with(|page| {
                    let source = page
                        .context()
                        .get_image_data(0.0, 0.0, WIDTH, HEIGHT)?;
                    let encoded =
                        page.to_buffer(format, &encoding(Some(quality)))?;
                    let mut into = Canvas::with_options(WIDTH, HEIGHT, cpu())?;
                    let ctx = into.context();
                    ctx.draw_image(&Image::from_encoded(&encoded)?, 0.0, 0.0);
                    let decoded =
                        ctx.get_image_data(0.0, 0.0, WIDTH, HEIGHT)?;
                    Ok(Measure {
                        bytes: Some(encoded.len()),
                        ..value("dB", psnr(source.pixels(), decoded.pixels()))
                    })
                })
            }),
        );
    }

    // How closely each renderer's edge coverage tracks the geometry: a
    // rectangle 0.25 px wide over white should read 25% black. Summed over a
    // sweep from 0.05 px to a full pixel, in 8-bit levels -- comparable
    // between rows, meaningless alone.
    for (name, gpu, msaa) in [
        ("cpu", false, None),
        ("gpu-msaa4", true, Some(4)),
        ("gpu-shader", true, Some(0)),
    ] {
        add(
            format!("aa/{name}"),
            Box::new(move || {
                let widths: Vec<f32> =
                    (1..=20).map(|i| i as f32 / 20.0).collect();
                let strip_width = widths.len() as f32 * 4.0;
                let mut strip = Canvas::with_options(
                    strip_width,
                    8.0,
                    options(gpu, PixelColorSpace::Srgb, PixelDepth::Uint8),
                )?;
                let ctx = strip.context();
                ctx.set_fill_style_css("white")?;
                ctx.fill_rect(0.0, 0.0, strip_width, 8.0);
                ctx.set_fill_style_css("black")?;
                for (i, w) in widths.iter().enumerate() {
                    ctx.fill_rect(i as f32 * 4.0, 0.0, *w, 8.0);
                }
                let read = ctx.get_image_data_with(
                    0.0,
                    0.0,
                    strip_width,
                    8.0,
                    &ImageDataExport {
                        msaa,
                        ..ImageDataExport::default()
                    },
                )?;
                // Row 4 of 8, clear of any edge effect at the top and bottom.
                let row = 4 * strip_width as usize * 4;
                let error: f64 = widths
                    .iter()
                    .enumerate()
                    .map(|(i, w)| {
                        (255.0
                            - f64::from(read.pixels()[row + i * 16])
                            - f64::from(*w) * 255.0)
                            .abs()
                    })
                    .sum();
                Ok(value("levels", error))
            }),
        );
    }

    // Each depth in a process of its own, three of them, and the median
    // taken: run inline, the process holds a pool of freed pages that new
    // canvases are served out of, and the delta measures the pool.
    for (name, depth) in DEPTHS {
        for read in ["tile", "page"] {
            add(
                format!("memory/{name}/{read}"),
                Box::new(move || {
                    let exe = std::env::current_exe()?;
                    let mut readings = Vec::new();
                    for _ in 0..if quick { 1 } else { 3 } {
                        let out = Command::new(&exe)
                            .args(["--memory-probe", name, read])
                            .output()?;
                        readings.push(
                            String::from_utf8_lossy(&out.stdout)
                                .trim()
                                .parse::<f64>()?,
                        );
                    }
                    let surface = f64::from(WIDTH * HEIGHT)
                        * depth.bytes_per_pixel() as f64
                        / BYTES_PER_MIB;
                    Ok(Measure {
                        surface: Some(surface),
                        ..value("MB", quantile(&readings, 0.5) / BYTES_PER_MIB)
                    })
                }),
            );
        }
    }

    cases
}

/// The child half of a memory case: holds twenty canvases of `depth`, each
/// read back by a pixel or whole, and prints the resident bytes per canvas.
fn memory_probe(depth: &str, read: &str) -> Result<()> {
    let depth = DEPTHS
        .iter()
        .find(|(name, _)| *name == depth)
        .map(|(_, depth)| *depth)
        .ok_or_else(|| format!("no depth named {depth}"))?;
    let before =
        resident_bytes().ok_or("no resident-size reader on this platform")?;
    let mut held = Vec::with_capacity(MEMORY_CANVASES);
    for _ in 0..MEMORY_CANVASES {
        let mut canvas = Canvas::with_options(
            WIDTH,
            HEIGHT,
            options(false, PixelColorSpace::Srgb, depth),
        )?;
        let ctx = canvas.context();
        ctx.set_fill_style_css("#345")?;
        ctx.fill_rect(0.0, 0.0, WIDTH, HEIGHT);
        match read {
            "page" => drop(ctx.get_image_data(0.0, 0.0, WIDTH, HEIGHT)?),
            _ => rasterize(&mut canvas)?,
        }
        held.push(canvas);
    }
    let after =
        resident_bytes().ok_or("no resident-size reader on this platform")?;
    print!(
        "{}",
        after.saturating_sub(before) as f64 / held.len() as f64
    );
    Ok(())
}

// ── the run ────────────────────────────────────────────────────────────────

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let [probe, depth, read] = args.as_slice()
        && probe == "--memory-probe"
    {
        return memory_probe(depth, read);
    }
    let flag = |name: &str| args.iter().any(|arg| arg == name);
    let option = |name: &str| {
        args.iter()
            .position(|arg| arg == name)
            .and_then(|at| args.get(at + 1))
            .cloned()
    };
    let quick = flag("--quick");
    let only = option("--only");
    let out = option("--out").map(PathBuf::from).unwrap_or_else(|| {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("target/bench")
    });

    let fixtures = Fixtures::new();
    let cases = cases(&fixtures, quick);
    if flag("--list") {
        for (id, _) in &cases {
            println!("{id}");
        }
        return Ok(());
    }

    let exe = std::env::current_exe()?;
    let exe_bytes = std::fs::read(&exe)?;
    let backend = BackendInfo::query();
    // Asked of the checkout this was built from, not of wherever it was run
    // from: a report names the commit that was measured.
    let checkout = env!("CARGO_MANIFEST_DIR");
    let commit = run_for("git", &["-C", checkout, "rev-parse", "HEAD"]);
    let dirty = run_for(
        "git",
        &[
            "-C",
            checkout,
            "status",
            "--porcelain",
            "--untracked-files=no",
        ],
    )
    .is_some_and(|s| !s.is_empty());
    let load_start = load_average();
    let machine = json!({
        "cpu": cpu_model(),
        "cores": std::thread::available_parallelism().map(usize::from).ok(),
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "runtime": format!("meo-skia-canvas {} crate", env!("CARGO_PKG_VERSION")),
        "gpu": { "api": backend.api, "device": backend.device, "driver": backend.driver },
    });
    println!(
        "{} · {} · {}/{}{}",
        backend.device.as_deref().unwrap_or("unnamed device"),
        backend
            .api
            .as_deref()
            .unwrap_or("no GPU backend compiled in"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        if quick { " · quick" } else { "" },
    );

    let mut results = Vec::new();
    for (id, measure) in &cases {
        if only
            .as_ref()
            .is_some_and(|prefix| !id.starts_with(prefix.as_str()))
        {
            continue;
        }
        let result = match measure() {
            Ok(m) => json!({
                "id": id, "unit": m.unit, "median": m.median, "p10": m.p10, "p90": m.p90,
                "runs": m.runs, "bytes": m.bytes, "surface": m.surface,
            }),
            // A machine with no usable GPU is a normal place to run this.
            Err(err) => json!({ "id": id, "error": err.to_string() }),
        };
        println!("  {id:<36} {}", shown(&result));
        results.push(result);
    }

    let date = utc_now();
    let report = json!({
        "benchmark": "crate",
        "date": date,
        "quick": quick,
        "commit": commit,
        "dirty": dirty,
        "binary": {
            "path": exe.display().to_string(),
            "bytes": exe_bytes.len(),
            "sha256": hex(&Sha256::digest(&exe_bytes)),
        },
        "machine": machine,
        "load": { "start": load_start, "end": load_average() },
        "cases": results,
    });
    let stamp = format!(
        "{}-{}",
        &date[..10],
        commit
            .as_deref()
            .map_or("nocommit", |c| &c[..8.min(c.len())])
    );
    std::fs::create_dir_all(&out)?;
    let base = out.join(format!("{stamp}-crate"));
    let json_path = base.with_extension("json");
    let md_path = base.with_extension("md");
    std::fs::write(
        &json_path,
        format!("{}\n", serde_json::to_string_pretty(&report)?),
    )?;
    std::fs::write(&md_path, markdown(&report))?;
    println!("\n{}\n{}", md_path.display(), json_path.display());
    Ok(())
}

/// One result as the console shows it.
fn shown(result: &Value) -> String {
    if let Some(error) = result["error"].as_str() {
        return format!("unavailable: {error}");
    }
    let unit = result["unit"].as_str().unwrap_or("");
    let median = result["median"].as_f64().unwrap_or(f64::NAN);
    let mut line = format!(
        "{median:>10.prec$} {unit}",
        prec = if unit == "ms" { 2 } else { 1 }
    );
    if let (Some(p10), Some(p90)) =
        (result["p10"].as_f64(), result["p90"].as_f64())
    {
        line += &format!("   p10 {p10:.2}  p90 {p90:.2}");
    }
    if let Some(bytes) = result["bytes"].as_u64() {
        line += &format!("   {:.1} KB", bytes as f64 / 1024.0);
    }
    line
}

/// The report as Markdown, a row a case.
fn markdown(report: &Value) -> String {
    let cell =
        |v: &Value| v.as_f64().map_or(String::new(), |v| format!("{v:.3}"));
    let loads = |v: &Value| {
        v.as_array().map_or("unknown".to_string(), |l| {
            l.iter()
                .map(|x| format!("{:.2}", x.as_f64().unwrap_or(f64::NAN)))
                .collect::<Vec<_>>()
                .join(", ")
        })
    };
    let mut md = vec![
        format!(
            "# crate benchmark, {}",
            report["date"].as_str().unwrap_or("")
        ),
        String::new(),
        format!(
            "- Commit: `{}`{}",
            report["commit"].as_str().unwrap_or("unknown"),
            if report["dirty"].as_bool() == Some(true) {
                " (uncommitted changes)"
            } else {
                ""
            }
        ),
        format!(
            "- Binary: `{}`, {} bytes, sha256 `{}`",
            report["binary"]["path"].as_str().unwrap_or(""),
            report["binary"]["bytes"],
            report["binary"]["sha256"].as_str().unwrap_or("")
        ),
        format!(
            "- Machine: {}, {} cores, {} {}, {}",
            report["machine"]["cpu"].as_str().unwrap_or("unknown"),
            report["machine"]["cores"],
            report["machine"]["os"].as_str().unwrap_or(""),
            report["machine"]["arch"].as_str().unwrap_or(""),
            report["machine"]["runtime"].as_str().unwrap_or("")
        ),
        format!(
            "- GPU: {} {}",
            report["machine"]["gpu"]["api"].as_str().unwrap_or("none"),
            report["machine"]["gpu"]["device"].as_str().unwrap_or("")
        ),
        format!(
            "- Load average (1, 5, 15 min): {} at the start, {} at the end",
            loads(&report["load"]["start"]),
            loads(&report["load"]["end"])
        ),
    ];
    if report["quick"].as_bool() == Some(true) {
        md.push(
            "- **Quick run: one sample a case, no warmup. Not a measurement.**"
                .into(),
        );
    }
    md.push(String::new());
    md.push("| case | unit | median | p10 | p90 | runs | bytes |".into());
    md.push("| --- | --- | ---: | ---: | ---: | ---: | ---: |".into());
    for r in report["cases"].as_array().into_iter().flatten() {
        let id = r["id"].as_str().unwrap_or("");
        md.push(match r["error"].as_str() {
            Some(error) => format!("| {id} | | unavailable: {error} | | | | |"),
            None => format!(
                "| {id} | {} | {} | {} | {} | {} | {} |",
                r["unit"].as_str().unwrap_or(""),
                cell(&r["median"]),
                cell(&r["p10"]),
                cell(&r["p90"]),
                r["runs"],
                r["bytes"].as_u64().map_or(String::new(), |b| b.to_string()),
            ),
        });
    }
    md.push(String::new());
    md.join("\n")
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// What `program` prints, trimmed, or `None` where it cannot run.
fn run_for(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program).args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The 1-, 5- and 15-minute load averages, or `None` where nothing here
/// reads them.
fn load_average() -> Option<Vec<f64>> {
    let text = match std::env::consts::OS {
        // `{ 1.23 2.34 3.45 }`
        "macos" => run_for("sysctl", &["-n", "vm.loadavg"])?,
        _ => std::fs::read_to_string("/proc/loadavg").ok()?,
    };
    let loads: Vec<f64> = text
        .split_whitespace()
        .filter_map(|word| word.parse().ok())
        .take(3)
        .collect();
    (loads.len() == 3).then_some(loads)
}

fn cpu_model() -> Option<String> {
    match std::env::consts::OS {
        "macos" => run_for("sysctl", &["-n", "machdep.cpu.brand_string"]),
        _ => std::fs::read_to_string("/proc/cpuinfo")
            .ok()?
            .lines()
            .find_map(|line| line.strip_prefix("model name"))
            .and_then(|rest| rest.split(':').nth(1))
            .map(|name| name.trim().to_string()),
    }
}

/// Seconds in a day.
const SECONDS_PER_DAY: u64 = 86_400;

/// Now, in UTC, as `YYYY-MM-DDTHH:MM:SSZ` -- the form `Date.toISOString`
/// writes, less its milliseconds.
fn utc_now() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let (year, month, day) =
        civil_from_days((seconds / SECONDS_PER_DAY) as i64);
    let in_day = seconds % SECONDS_PER_DAY;
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        in_day / 3600,
        in_day % 3600 / 60,
        in_day % 60
    )
}

/// The proleptic Gregorian date `days` after 1970-01-01: Howard Hinnant's
/// `civil_from_days` (chrono-Compatible Low-Level Date Algorithms), which
/// counts in 400-year eras of 146,097 days beginning on a 1 March.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    /// Days from 0000-03-01 to 1970-01-01.
    const EPOCH_SHIFT: i64 = 719_468;
    /// Days in a 400-year era.
    const ERA_DAYS: i64 = 146_097;
    let z = days + EPOCH_SHIFT;
    let era = z.div_euclid(ERA_DAYS);
    let doe = z.rem_euclid(ERA_DAYS);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// Resident set size of this process, in bytes.
///
/// There is no portable way to ask: `std` has no memory-statistics API and
/// Skia's surfaces are allocated by C++ `new`, so a counting Rust allocator
/// would report almost none of what is being measured here. Each platform
/// gets the cheapest reader it has, and anything else says so rather than
/// printing a number it did not measure.
fn resident_bytes() -> Option<u64> {
    /// `/proc/self/status` and `ps -o rss=` both report kibibytes.
    const KIB: u64 = 1024;
    #[cfg(target_os = "linux")]
    {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        status
            .lines()
            .find_map(|line| line.strip_prefix("VmRSS:"))?
            .split_whitespace()
            .next()?
            .parse::<u64>()
            .ok()
            .map(|kib| kib * KIB)
    }
    #[cfg(target_os = "macos")]
    {
        run_for("ps", &["-o", "rss=", "-p", &std::process::id().to_string()])?
            .parse::<u64>()
            .ok()
            .map(|kib| kib * KIB)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}
