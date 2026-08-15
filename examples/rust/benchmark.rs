//! Timing and memory, measured rather than asserted.
//!
//! Run with:
//!
//!     cargo run --release --example benchmark
//!
//! Release on purpose. A dev build leaves the Rust glue unoptimized, which
//! moves the per-call overhead without touching Skia, so the ratios come out
//! right and the milliseconds do not.
//!
//! Reports the median of N runs after a warmup, because the first draw on a
//! backend pays for shader compilation and surface allocation and is not what
//! a caller experiences in steady state. Every figure is one machine and one
//! GPU; treat the ratios as the transferable part, not the milliseconds.
//!
//! `examples/node/benchmark.js` measures the same scenes through the Node
//! binding. Run both to see what the binding costs; run either alone and the
//! numbers still describe Skia.

use std::{
    error::Error,
    time::{Duration, Instant},
};

use meo_skia_canvas::prelude::*;

const WIDTH: f32 = 1200.0;
const HEIGHT: f32 = 900.0;

/// The three formats a page can be handed back in, widest last.
const DEPTHS: [(&str, PixelDepth); 3] = [
    ("RGBA8888", PixelDepth::Uint8),
    ("RGBAF16", PixelDepth::F16),
    ("RGBAF32", PixelDepth::F32),
];

/// Bytes per pixel each depth needs, which is what the surface arithmetic at
/// the end compares the measured residency against.
///
/// The crate answers it, rather than a copy of the three this benchmark
/// runs: `PixelDepth` names every layout a canvas can be built in, and a
/// match here would need every arm to measure three of them.
fn bytes_per_pixel(depth: PixelDepth) -> f32 {
    depth.bytes_per_pixel() as f32
}

const BYTES_PER_MIB: f32 = 1_048_576.0;

/// Width the first column is padded to, so the numbers line up under each
/// other whatever the label says.
const LABEL_WIDTH: usize = 22;

// ── measurement ────────────────────────────────────────────────────────────

/// Median of a sample.
///
/// The upper middle element on an even count rather than the mean of the two,
/// which is what the JavaScript mirror's `s[s.length >> 1]` picks. Averaging
/// would invent a duration nothing actually took.
fn median(mut runs: Vec<Duration>) -> Duration {
    runs.sort_unstable();
    runs[runs.len() / 2]
}

/// Runs `work` `warmup` times to settle the caches, then `iterations` times
/// for the record.
fn time(iterations: usize, warmup: usize, mut work: impl FnMut()) -> Duration {
    for _ in 0..warmup {
        work();
    }
    let mut runs = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let started = Instant::now();
        work();
        runs.push(started.elapsed());
    }
    median(runs)
}

/// Iterations and warmup for the headline GPU-against-CPU comparison, which
/// is the one figure worth the most samples.
const HEADLINE_RUNS: (usize, usize) = (10, 3);
/// Iterations and warmup everywhere else. Fewer, because the pixel-format
/// sweep runs the same scene nine times over and the export sweep five.
const SWEEP_RUNS: (usize, usize) = (8, 2);

fn millis(elapsed: Duration) -> f64 {
    elapsed.as_secs_f64() * 1000.0
}

fn row(label: &str, elapsed: Duration, ratio: Option<f64>) {
    let ms = format!("{:.1}", millis(elapsed));
    print!("  {label:<LABEL_WIDTH$} {ms:>7} ms");
    match ratio {
        Some(ratio) => println!("   {ratio:.2}x"),
        None => println!(),
    }
}

// ── the scenes ─────────────────────────────────────────────────────────────

/// Scatters `index` across `span` without a random number generator, which
/// would make one run's timing incomparable with the next.
///
/// `stride` is coprime with nothing in particular -- the primes below are
/// chosen only so that successive shapes land somewhere else rather than
/// marching in a line, which would let the rasterizer reuse coverage it has
/// no business reusing.
fn scatter(index: usize, stride: usize, span: f32) -> f32 {
    (index * stride) as f32 % span
}

const CURVE_COUNT: usize = 300;
const CURVE_START_X_STRIDE: usize = 37;
const CURVE_START_Y_STRIDE: usize = 53;
const CURVE_CONTROL1_X_STRIDE: usize = 71;
const CURVE_CONTROL1_Y_STRIDE: usize = 29;
const CURVE_CONTROL2_X_STRIDE: usize = 13;
const CURVE_CONTROL2_Y_STRIDE: usize = 97;
const CURVE_END_X_STRIDE: usize = 41;
const CURVE_END_Y_STRIDE: usize = 61;
/// Degrees between one curve's hue and the next.
const CURVE_HUE_STEP: usize = 7;
const DEGREES_PER_TURN: usize = 360;
/// Line widths cycle 1, 2, 3, 4 so the stroker is not measured at one width.
const CURVE_WIDTH_STEPS: usize = 4;

const PANEL_COUNT: usize = 60;
const PANEL_WIDTH: f32 = 120.0;
const PANEL_HEIGHT: f32 = 70.0;
const PANEL_RADIUS: f32 = 10.0;
const PANEL_MARGIN: f32 = 20.0;
const PANEL_X_STRIDE: usize = 19;
const PANEL_Y_STRIDE: usize = 31;
const PANEL_HUE_STEP: usize = 13;
const PANEL_SHADOW_BLUR: f32 = 12.0;
const PANEL_SHADOW_ALPHA: f32 = 0.5;

const TEXT_LINE_COUNT: usize = 40;
const TEXT_LEFT: f32 = 40.0;
const TEXT_TOP: f32 = 40.0;
const TEXT_LEADING: f32 = 21.0;

/// A mixed vector scene: curves, shadowed panels and text, in the proportions
/// a chart or report actually draws them.
fn scene(ctx: &mut Context2D) -> Result<(), Box<dyn Error>> {
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
        GradientColorSpace::Srgb,
    )?;
    ctx.set_fill_shader(&background);
    ctx.fill_rect(0.0, 0.0, WIDTH, HEIGHT);

    for i in 0..CURVE_COUNT {
        ctx.begin_path();
        ctx.move_to(
            scatter(i, CURVE_START_X_STRIDE, WIDTH),
            scatter(i, CURVE_START_Y_STRIDE, HEIGHT),
        );
        ctx.bezier_curve_to(
            scatter(i, CURVE_CONTROL1_X_STRIDE, WIDTH),
            scatter(i, CURVE_CONTROL1_Y_STRIDE, HEIGHT),
            scatter(i, CURVE_CONTROL2_X_STRIDE, WIDTH),
            scatter(i, CURVE_CONTROL2_Y_STRIDE, HEIGHT),
            scatter(i, CURVE_END_X_STRIDE, WIDTH),
            scatter(i, CURVE_END_Y_STRIDE, HEIGHT),
        );
        // Through the CSS string rather than `RgbaLinear`, because `hsl()` is
        // how the JavaScript mirror writes it and the point here is that the
        // two sides describe the same colour.
        ctx.set_stroke_style_css(&format!(
            "hsl({} 70% 60%)",
            (i * CURVE_HUE_STEP) % DEGREES_PER_TURN
        ))?;
        ctx.set_line_width(1.0 + (i % CURVE_WIDTH_STEPS) as f32);
        ctx.stroke();
    }

    for i in 0..PANEL_COUNT {
        ctx.save();
        ctx.set_shadow_color(RgbaLinear::from_srgb8(
            0,
            0,
            0,
            PANEL_SHADOW_ALPHA,
        ));
        ctx.set_shadow_blur(PANEL_SHADOW_BLUR);
        ctx.set_fill_style_css(&format!(
            "hsl({} 60% 55%)",
            (i * PANEL_HUE_STEP) % DEGREES_PER_TURN
        ))?;
        ctx.begin_path();
        ctx.round_rect(
            PANEL_MARGIN
                + scatter(
                    i,
                    PANEL_X_STRIDE,
                    WIDTH - PANEL_WIDTH - PANEL_MARGIN,
                ),
            PANEL_MARGIN
                + scatter(
                    i,
                    PANEL_Y_STRIDE,
                    HEIGHT - PANEL_HEIGHT - PANEL_MARGIN,
                ),
            PANEL_WIDTH,
            PANEL_HEIGHT,
            [PANEL_RADIUS; 4],
        )?;
        ctx.fill(FillRule::NonZero);
        ctx.restore();
    }

    ctx.set_font(&Font::parse("600 28px Helvetica")?);
    ctx.set_fill_style_css("#e6edf3")?;
    for i in 0..TEXT_LINE_COUNT {
        ctx.fill_text(
            &format!("Throughput sample {i}"),
            TEXT_LEFT,
            TEXT_TOP + i as f32 * TEXT_LEADING,
            None,
        );
    }
    Ok(())
}

const LAYER_COUNT: usize = 120;
/// Low enough that 120 of them still do not saturate, so every layer is a
/// blend rather than a replace.
const LAYER_ALPHA: f32 = 0.02;

fn alternating(i: usize) -> &'static str {
    match i % 2 {
        0 => "#4488ff",
        _ => "#ff8844",
    }
}

fn translucent(ctx: &mut Context2D) -> Result<(), Box<dyn Error>> {
    for i in 0..LAYER_COUNT {
        ctx.set_global_alpha(LAYER_ALPHA);
        ctx.set_fill_style_css(alternating(i))?;
        ctx.fill_rect(0.0, 0.0, WIDTH, HEIGHT);
    }
    Ok(())
}

/// Inset by a pixel on purpose. An opaque fill that covers the whole page lets
/// Skia discard everything recorded under it, so a loop of them measures the
/// cull rather than the fill: 1200 of them came in at 2 ms, which is not 1200
/// fills of anything. One pixel short of the bounds and every layer is drawn.
const OPAQUE_INSET: f32 = 1.0;

fn opaque(ctx: &mut Context2D) -> Result<(), Box<dyn Error>> {
    for i in 0..LAYER_COUNT {
        ctx.set_fill_style_css(alternating(i))?;
        ctx.fill_rect(0.0, OPAQUE_INSET, WIDTH, HEIGHT - OPAQUE_INSET);
    }
    Ok(())
}

// ── driving them ───────────────────────────────────────────────────────────

/// Reading one pixel back forces the recording to rasterize. Without it the
/// timing measures how fast commands are appended to a picture, not drawing.
fn rasterize(canvas: &mut Canvas) {
    canvas
        .context()
        .get_image_data(0.0, 0.0, 1.0, 1.0)
        .expect("a 1x1 readback at the origin is inside every page");
}

fn draw(
    options: CanvasOptions,
    paint: fn(&mut Context2D) -> Result<(), Box<dyn Error>>,
) {
    let mut canvas = Canvas::with_options(WIDTH, HEIGHT, options)
        .expect("sRGB is constructible in every Skia build");
    paint(canvas.context()).expect("the scenes use only literals this accepts");
    rasterize(&mut canvas);
}

fn cpu(color_type: PixelDepth) -> CanvasOptions {
    CanvasOptions {
        gpu: false,
        color_type,
        ..CanvasOptions::default()
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let backend = BackendInfo::query();
    println!(
        "{} · {} raster threads · {} · {}/{}",
        backend.device.as_deref().unwrap_or("unnamed device"),
        backend.threads,
        backend
            .api
            .as_deref()
            .unwrap_or("no GPU backend compiled in"),
        std::env::consts::OS,
        std::env::consts::ARCH,
    );

    // ── vector scene: GPU against CPU ──────────────────────────────────────
    println!("\nmixed vector scene, {WIDTH}x{HEIGHT}");
    let (iterations, warmup) = HEADLINE_RUNS;
    let on_gpu = time(iterations, warmup, || {
        draw(
            CanvasOptions {
                gpu: true,
                ..CanvasOptions::default()
            },
            scene,
        )
    });
    let on_cpu =
        time(iterations, warmup, || draw(cpu(PixelDepth::Uint8), scene));
    row("RGBA8888 gpu", on_gpu, None);
    row(
        "RGBA8888 cpu",
        on_cpu,
        Some(millis(on_cpu) / millis(on_gpu)),
    );

    // ── float cost, two workloads that disagree ────────────────────────────
    // Blending translucent layers and filling opaque ones pull in opposite
    // directions, so a single "float costs Nx" number would be false either
    // way.
    let (iterations, warmup) = SWEEP_RUNS;
    for (name, paint) in [
        (
            "mixed vector scene",
            scene as fn(&mut Context2D) -> Result<(), Box<dyn Error>>,
        ),
        ("120 translucent layers", translucent),
        ("120 opaque fills", opaque),
    ] {
        println!("\n{name}, cpu, by pixel format");
        let mut base = None;
        for (label, depth) in DEPTHS {
            let elapsed = time(iterations, warmup, || draw(cpu(depth), paint));
            // The first depth is the baseline every later one is quoted
            // against, so it always reads 1.00x rather than being omitted --
            // a column of ratios with a hole in it is harder to scan.
            let base = *base.get_or_insert(elapsed);
            row(label, elapsed, Some(millis(elapsed) / millis(base)));
        }
    }

    // ── export ─────────────────────────────────────────────────────────────
    println!("\nencode a drawn {WIDTH}x{HEIGHT} page");
    let mut page = Canvas::with_options(WIDTH, HEIGHT, cpu(PixelDepth::Uint8))?;
    scene(page.context())?;
    for (label, format, quality) in [
        ("png", ImageFormat::Png, None),
        ("jpg", ImageFormat::Jpeg, Some(0.92)),
        ("webp", ImageFormat::Webp, Some(0.9)),
        ("pdf", ImageFormat::Pdf, None),
        ("svg", ImageFormat::Svg, None),
    ] {
        let options = EncodeOptions {
            quality: quality.unwrap_or(EncodeOptions::default().quality),
            ..EncodeOptions::default()
        };
        let elapsed = time(iterations, warmup, || {
            page.to_buffer(format, &options).expect(
                "the page has both dimensions and every format here \
                 can encode it",
            );
        });
        row(label, elapsed, None);
    }

    // ── memory ─────────────────────────────────────────────────────────────
    println!("\nresident memory per {WIDTH}x{HEIGHT} canvas");
    if resident_bytes().is_none() {
        println!(
            "  (no resident-size reader on {}; surface arithmetic only)",
            std::env::consts::OS
        );
    }
    /// Enough canvases that one page's allocation is large against the noise
    /// of everything else the process is doing, and few enough that the
    /// widest format still fits in a couple of hundred megabytes.
    const HELD_CANVASES: usize = 20;
    for (label, depth) in DEPTHS {
        let before = resident_bytes();
        let mut held = Vec::with_capacity(HELD_CANVASES);
        for _ in 0..HELD_CANVASES {
            let mut canvas = Canvas::with_options(WIDTH, HEIGHT, cpu(depth))?;
            let ctx = canvas.context();
            ctx.set_fill_style_css("#345")?;
            ctx.fill_rect(0.0, 0.0, WIDTH, HEIGHT);
            rasterize(&mut canvas);
            held.push(canvas);
        }
        let surface = WIDTH * HEIGHT * bytes_per_pixel(depth) / BYTES_PER_MIB;
        let measured = match (before, resident_bytes()) {
            (Some(before), Some(after)) => format!(
                "{:>6.2} MB",
                (after.saturating_sub(before) as f32
                    / held.len() as f32
                    / BYTES_PER_MIB)
            ),
            _ => format!("{:>9}", "--"),
        };
        println!(
            "  {label:<LABEL_WIDTH$} {measured}   surface alone {surface:.2} MB"
        );
        drop(held);
    }

    Ok(())
}

/// Resident set size of this process, in bytes.
///
/// There is no portable way to ask: `std` has no memory-statistics API and
/// Skia's surfaces are allocated by C++ `new`, so a counting Rust allocator
/// would report almost none of what is being measured here. Each platform
/// gets the cheapest reader it has, and anything else says so rather than
/// printing a number it did not measure.
fn resident_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        /// `/proc/self/status` reports `VmRSS` in kibibytes.
        const KIB: u64 = 1024;
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
        /// `ps -o rss=` reports kibibytes, and is the only reader that does
        /// not need a `libc` dependency this crate does not otherwise have.
        const KIB: u64 = 1024;
        let output = std::process::Command::new("ps")
            .args(["-o", "rss=", "-p"])
            .arg(std::process::id().to_string())
            .output()
            .ok()?;
        String::from_utf8(output.stdout)
            .ok()?
            .trim()
            .parse::<u64>()
            .ok()
            .map(|kib| kib * KIB)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}
