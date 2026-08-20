//! A report card: charts, type and effects composed into one image.
//!
//! Run with:
//!
//!     cargo run --example report_card -- [outdir]
//!
//! The Rust half of `examples/node/report-card.js`. Same picture, same
//! exports, same checks at the end -- so the two files read as a translation
//! of each other rather than two programs that happen to draw a chart.

use std::{error::Error, f32::consts::TAU, fs, path::PathBuf};

use meo_skia_canvas::prelude::*;

const WIDTH: f32 = 900.0;
const HEIGHT: f32 = 620.0;

/// Requests served per day, in thousands.
const DATA: [(&str, f32); 7] = [
    ("Mon", 62.0),
    ("Tue", 78.0),
    ("Wed", 45.0),
    ("Thu", 91.0),
    ("Fri", 84.0),
    ("Sat", 33.0),
    ("Sun", 51.0),
];

/// Every canvas here is drawn on the CPU so the committed images are
/// reproducible on any machine. The GPU path antialiases differently -- it
/// resolves partial coverage in a shader rather than by sampling -- so the
/// same program on a GPU box would rewrite these files without a code change.
fn cpu() -> CanvasOptions {
    CanvasOptions {
        gpu: false,
        ..CanvasOptions::default()
    }
}

fn hex(value: &str) -> RgbaLinear {
    RgbaLinear::from_hex(value).expect("a literal written in this file")
}

/// Interpolating in sRGB is what a browser does, and what the JavaScript
/// mirror's `addColorStop` gets by default.
fn stops(entries: &[(f32, &str)]) -> Vec<GradientStop> {
    entries
        .iter()
        .map(|(position, color)| GradientStop {
            position: *position,
            color: hex(color),
        })
        .collect()
}

// ── the logo ───────────────────────────────────────────────────────────────

const LOGO_SIZE: f32 = 120.0;
const LOGO_CENTRE: f32 = LOGO_SIZE / 2.0;
const LOGO_OUTER_RADIUS: f32 = 52.0;
const LOGO_INNER_RADIUS: f32 = 30.0;
/// Stops around the sweep. Seven, so the first and last meet at the seam.
const LOGO_STOPS: usize = 6;
const LOGO_HUE_START: f32 = 200.0;
const LOGO_HUE_STEP: f32 = 25.0;

/// A logo mark, drawn once on its own canvas and reused as an image -- the
/// pattern anyone building a report generator ends up with.
fn make_logo() -> Result<Canvas, Box<dyn Error>> {
    let mut canvas = Canvas::with_options(LOGO_SIZE, LOGO_SIZE, cpu())?;
    let ctx = canvas.context();

    let ring: Vec<GradientStop> = (0..=LOGO_STOPS)
        .map(|i| {
            let hue = LOGO_HUE_START + i as f32 * LOGO_HUE_STEP;
            // `from_hsl` is not on RgbaLinear, and a CSS string cannot be a
            // gradient stop, so the conversion is spelled out once here.
            GradientStop {
                position: i as f32 / LOGO_STOPS as f32,
                color: hsl(hue, 0.85, 0.60),
            }
        })
        .collect();

    let sweep = Shader::sweep_gradient(
        Point::new(LOGO_CENTRE, LOGO_CENTRE),
        0.0,
        360.0,
        &ring,
        GradientColorSpace::Srgb,
    )?;
    ctx.set_fill_shader(&sweep);
    ctx.begin_path();
    ctx.arc(LOGO_CENTRE, LOGO_CENTRE, LOGO_OUTER_RADIUS, 0.0, TAU, false)?;
    ctx.fill(FillRule::NonZero);

    // Punch the middle out, leaving a ring.
    ctx.set_global_composite_operation(BlendMode::DestinationOut);
    ctx.begin_path();
    ctx.arc(LOGO_CENTRE, LOGO_CENTRE, LOGO_INNER_RADIUS, 0.0, TAU, false)?;
    ctx.fill(FillRule::NonZero);
    ctx.set_global_composite_operation(BlendMode::SourceOver);

    Ok(canvas)
}

/// CSS `hsl()` to a linear-light premultiplied color.
///
/// `hue` in degrees, `saturation` and `lightness` on 0..1. The algorithm is
/// the one CSS Color 4 defines; the result goes through
/// [`RgbaLinear::from_srgb`] because HSL is defined over gamma-encoded sRGB,
/// not over light.
fn hsl(hue: f32, saturation: f32, lightness: f32) -> RgbaLinear {
    /// Degrees per sector of the hue wheel. CSS Color 4 defines the helper
    /// over `hue / 30`, which is this.
    const SECTOR: f32 = 30.0;
    const TURN: f32 = 360.0;
    let hue = hue.rem_euclid(TURN);
    let chroma = saturation * lightness.min(1.0 - lightness);
    let channel = |n: f32| {
        let k = (n + hue / SECTOR) % 12.0;
        lightness - chroma * (k - 3.0).min(9.0 - k).clamp(-1.0, 1.0)
    };
    RgbaLinear::from_srgb(channel(0.0), channel(8.0), channel(4.0), 1.0)
}

// ── the card ───────────────────────────────────────────────────────────────

const NOISE_ALPHA: f32 = 0.06;
const NOISE_FREQUENCY: f32 = 0.9;
const NOISE_OCTAVES: usize = 3;
const NOISE_SEED: f32 = 4.0;

const HEADER: Rect = Rect {
    left: 40.0,
    top: 36.0,
    right: WIDTH - 40.0,
    bottom: 36.0 + 96.0,
};
const HEADER_RADIUS: f32 = 18.0;
const HEADER_SHADOW_BLUR: f32 = 24.0;
const HEADER_SHADOW_DROP: f32 = 8.0;
const HEADER_SHADOW_ALPHA: f32 = 0.45;

const PANEL: Rect = Rect {
    left: 40.0,
    top: 160.0,
    right: WIDTH - 40.0,
    bottom: 160.0 + 300.0,
};
const PANEL_RADIUS: f32 = 16.0;
/// Horizontal inset the gridlines stop short of the panel edge by.
const GRID_INSET: f32 = 20.0;
const GRID_LINES: usize = 4;
/// Vertical room the gridlines leave above and below themselves.
const GRID_MARGIN: f32 = 40.0;

/// Room below the bars for the day labels.
const BAR_FOOT: f32 = 46.0;
/// Vertical space the bars give up so the tallest does not touch the panel.
const BAR_HEADROOM: f32 = 100.0;
/// Left inset of the first slot within the panel.
const BAR_GUTTER: f32 = 30.0;
/// Fraction of its slot a bar is inset by, and the fraction it then occupies.
const BAR_INSET: f32 = 0.18;
const BAR_WIDTH: f32 = 0.64;
const BAR_RADIUS: f32 = 7.0;
/// Sigma of the halo drawn behind the tallest bar.
const BAR_GLOW: f32 = 9.0;
/// Gap between a bar's top and its value label.
const BAR_LABEL_GAP: f32 = 10.0;
/// Gap between the panel's bottom and the day labels.
const DAY_LABEL_GAP: f32 = 20.0;

/// How far above each bar the trend line runs.
const TREND_LIFT: f32 = 24.0;
const TREND_WIDTH: f32 = 2.5;
/// Corner radius the polyline is rounded with, so it reads as a trend rather
/// than as seven straight segments.
const TREND_ROUNDING: f32 = 10.0;

const FOOTNOTE_LEFT: f32 = 60.0;
const FOOTNOTE_TOP: f32 = 496.0;
const FOOTNOTE_SIZE: f32 = 15.0;

fn draw_card(
    ctx: &mut Context2D,
    engine: &TextEngine,
) -> Result<Paragraph, Box<dyn Error>> {
    // Background: vertical gradient plus a noise shader for texture.
    let background = Shader::linear_gradient(
        Point::new(0.0, 0.0),
        Point::new(0.0, HEIGHT),
        &stops(&[(0.0, "#0f1b2d"), (1.0, "#1b2b45")]),
        GradientColorSpace::Srgb,
    )?;
    ctx.set_fill_shader(&background);
    ctx.fill_rect(0.0, 0.0, WIDTH, HEIGHT);

    ctx.save();
    ctx.set_global_alpha(NOISE_ALPHA);
    ctx.set_fill_shader(&Shader::fractal_noise(
        NOISE_FREQUENCY,
        NOISE_FREQUENCY,
        NOISE_OCTAVES,
        NOISE_SEED,
    )?);
    ctx.fill_rect(0.0, 0.0, WIDTH, HEIGHT);
    ctx.restore();

    // Header panel with a soft shadow.
    ctx.save();
    ctx.set_shadow_color(RgbaLinear::from_srgb8(0, 0, 0, HEADER_SHADOW_ALPHA));
    ctx.set_shadow_blur(HEADER_SHADOW_BLUR);
    ctx.set_shadow_offset(0.0, HEADER_SHADOW_DROP);
    ctx.set_fill_style(hex("#16233a"));
    ctx.begin_path();
    ctx.round_rect(
        HEADER.left,
        HEADER.top,
        HEADER.width(),
        HEADER.height(),
        [HEADER_RADIUS; 4],
    )?;
    ctx.fill(FillRule::NonZero);
    ctx.restore();

    // Logo, drawn from the other canvas.
    let mut logo = make_logo()?;
    ctx.draw_canvas_sized(&mut logo, 62.0, 48.0, 72.0, 72.0);

    ctx.set_fill_style(hex("#eaf2ff"));
    ctx.set_font(&Font::parse("600 30px Helvetica")?);
    ctx.fill_text("Weekly throughput", 156.0, 82.0, None);

    ctx.set_fill_style(hex("#7f9ac0"));
    ctx.set_font(&Font::parse("16px Helvetica")?);
    ctx.fill_text("Requests served per day, in thousands", 156.0, 108.0, None);

    ctx.set_text_align(TextAlign::Right);
    ctx.set_fill_style(hex("#4ade80"));
    ctx.set_font(&Font::parse("600 26px Helvetica")?);
    ctx.fill_text("+18.4%", WIDTH - 68.0, 92.0, None);
    ctx.set_text_align(TextAlign::Left);

    // Chart panel.
    ctx.set_fill_style(RgbaLinear::from_srgb8(255, 255, 255, 0.04));
    ctx.begin_path();
    ctx.round_rect(
        PANEL.left,
        PANEL.top,
        PANEL.width(),
        PANEL.height(),
        [PANEL_RADIUS; 4],
    )?;
    ctx.fill(FillRule::NonZero);

    // Gridlines, clipped to the panel.
    ctx.save();
    ctx.begin_path();
    ctx.round_rect(
        PANEL.left,
        PANEL.top,
        PANEL.width(),
        PANEL.height(),
        [PANEL_RADIUS; 4],
    )?;
    ctx.clip(FillRule::NonZero);

    ctx.set_stroke_style(RgbaLinear::from_srgb8(255, 255, 255, 0.08));
    ctx.set_line_width(1.0);
    for i in 0..=GRID_LINES {
        let y = PANEL.top
            + GRID_MARGIN
            + (i as f32 * (PANEL.height() - 2.0 * GRID_MARGIN))
                / GRID_LINES as f32;
        ctx.begin_path();
        ctx.move_to(PANEL.left + GRID_INSET, y);
        ctx.line_to(PANEL.right - GRID_INSET, y);
        ctx.stroke();
    }

    // Bars, each a rounded path with a gradient and a glow on the tallest.
    let peak = DATA
        .iter()
        .map(|(_, value)| *value)
        .fold(f32::MIN, f32::max);
    let slot = (PANEL.width() - 2.0 * BAR_GUTTER) / DATA.len() as f32;

    for (i, (label, value)) in DATA.iter().enumerate() {
        let tallest = *value == peak;
        let height = (PANEL.height() - BAR_HEADROOM) * value / peak;
        let x = PANEL.left + BAR_GUTTER + i as f32 * slot + slot * BAR_INSET;
        let width = slot * BAR_WIDTH;
        let y = PANEL.bottom - BAR_FOOT - height;

        if tallest {
            ctx.save();
            ctx.set_mask_filter(Some(&MaskFilter::blur(
                BlurStyle::Outer,
                BAR_GLOW,
                false,
            )?));
            ctx.set_fill_style(hex("#38bdf8"));
            ctx.begin_path();
            ctx.round_rect(x, y, width, height, [BAR_RADIUS; 4])?;
            ctx.fill(FillRule::NonZero);
            ctx.restore();
        }

        let bar = Shader::linear_gradient(
            Point::new(0.0, y),
            Point::new(0.0, y + height),
            &stops(match tallest {
                true => &[(0.0, "#7dd3fc"), (1.0, "#2563eb")],
                false => &[(0.0, "#3b82f6"), (1.0, "#1e3a8a")],
            }),
            GradientColorSpace::Srgb,
        )?;
        ctx.set_fill_shader(&bar);
        ctx.begin_path();
        ctx.round_rect(x, y, width, height, [BAR_RADIUS; 4])?;
        ctx.fill(FillRule::NonZero);

        ctx.set_text_align(TextAlign::Center);
        ctx.set_fill_style(hex("#9fb6d4"));
        ctx.set_font(&Font::parse("14px Helvetica")?);
        ctx.fill_text(
            label,
            x + width / 2.0,
            PANEL.bottom - DAY_LABEL_GAP,
            None,
        );
        ctx.set_fill_style(hex("#dbeafe"));
        ctx.set_font(&Font::parse("600 14px Helvetica")?);
        ctx.fill_text(
            &format!("{value}"),
            x + width / 2.0,
            y - BAR_LABEL_GAP,
            None,
        );
        ctx.set_text_align(TextAlign::Left);
    }
    ctx.restore();

    // A trend line over the bars, built segment by segment and then rounded.
    let mut trend = PathBuilder::new();
    for (i, (_, value)) in DATA.iter().enumerate() {
        let x = PANEL.left + BAR_GUTTER + i as f32 * slot + slot * 0.5;
        let y = PANEL.bottom
            - BAR_FOOT
            - (PANEL.height() - BAR_HEADROOM) * value / peak
            - TREND_LIFT;
        match i {
            0 => trend.move_to(x, y),
            _ => trend.line_to(x, y),
        };
    }
    ctx.set_stroke_style(RgbaLinear::from_srgb8(250, 204, 21, 0.9));
    ctx.set_line_width(TREND_WIDTH);
    ctx.set_line_join(StrokeJoin::Round);
    ctx.stroke_path(&trend.build(FillRule::NonZero).round(TREND_ROUNDING));

    // Footnote, laid out as a wrapping paragraph with a styled run.
    let base = TextStyle {
        font_families: vec!["Helvetica".to_string()],
        font_size: FOOTNOTE_SIZE,
        color: hex("#8fa8c8"),
        align: TextAlign::Left,
        ..TextStyle::default()
    };
    let mut paragraph = engine.paragraph_builder(&base);
    paragraph
        .add_text("Figures are provisional and exclude cached responses. ");
    paragraph.push_style(&TextStyle {
        color: hex("#facc15"),
        decoration: TextDecoration::underline(),
        ..base.clone()
    });
    paragraph.add_text("Thursday's peak");
    paragraph.pop();
    paragraph.add_text(
        " coincided with the scheduled reindex, which is expected to recur \
         next week.",
    );

    let laid_out = paragraph.build(WIDTH - 120.0);
    ctx.draw_paragraph(&laid_out, FOOTNOTE_LEFT, FOOTNOTE_TOP);
    Ok(laid_out)
}

// ── exports and checks ─────────────────────────────────────────────────────

const BYTES_PER_KIB: f64 = 1024.0;
/// Width the first column of both tables is padded to.
const LABEL_WIDTH: usize = 24;

const BOOK_WIDTH: f32 = 400.0;
const BOOK_HEIGHT: f32 = 300.0;
const BOOK_PAGES: [&str; 3] = ["#334155", "#475569", "#64748b"];

fn main() -> Result<(), Box<dyn Error>> {
    let out = PathBuf::from(
        std::env::args().nth(1).unwrap_or_else(|| "out".to_string()),
    );
    fs::create_dir_all(&out)?;

    // The system fonts alone: nothing here registers a face, and `Helvetica`
    // is the platform's. `TextEngine::new` would be the call if it did.
    let engine = TextEngine::with_system_fonts();

    let mut canvas = Canvas::with_options(WIDTH, HEIGHT, cpu())?;
    let paragraph = draw_card(canvas.context(), &engine)?;

    let mut results: Vec<(String, u64)> = Vec::new();

    // Every export format a consumer might reach for.
    for (extension, format, quality) in [
        ("png", ImageFormat::Png, None),
        ("jpg", ImageFormat::Jpeg, Some(0.92)),
        ("webp", ImageFormat::Webp, Some(0.9)),
        ("pdf", ImageFormat::Pdf, None),
        ("svg", ImageFormat::Svg, None),
    ] {
        let file = out.join(format!("report.{extension}"));
        let options = EncodeOptions {
            quality: quality.unwrap_or(EncodeOptions::default().quality),
            ..EncodeOptions::default()
        };
        canvas.to_file(&file, &options)?;
        let _ = format;
        results.push((extension.to_string(), fs::metadata(&file)?.len()));
    }

    // Multi-page PDF through `new_page`, as the docs describe it.
    let mut book = Canvas::with_options(BOOK_WIDTH, BOOK_HEIGHT, cpu())?;
    for (page, color) in BOOK_PAGES.iter().enumerate() {
        let ctx = match page {
            0 => book.context(),
            _ => book.new_page_with(BOOK_WIDTH, BOOK_HEIGHT),
        };
        ctx.set_fill_style(hex(color));
        ctx.fill_rect(0.0, 0.0, BOOK_WIDTH, BOOK_HEIGHT);
        ctx.set_fill_style(RgbaLinear::from_srgb8(255, 255, 255, 1.0));
        ctx.set_font(&Font::parse("28px Helvetica")?);
        ctx.fill_text(
            &format!("Page {} of {}", page + 1, BOOK_PAGES.len()),
            40.0,
            160.0,
            None,
        );
    }
    let book_file = out.join("book.pdf");
    book.to_file(&book_file, &EncodeOptions::default())?;
    results.push((
        format!("pdf ({} pages)", BOOK_PAGES.len()),
        fs::metadata(&book_file)?.len(),
    ));

    // Round-trip: encode, reload, redraw, read back. `loadImage` on the
    // JavaScript side also takes a path or a URL; here the bytes are already
    // in hand, which is the case this checks.
    let png = canvas.to_buffer(ImageFormat::Png, &EncodeOptions::default())?;
    let reloaded = Image::from_encoded(&png)?;
    let mut check = Canvas::with_options(
        reloaded.width() as f32,
        reloaded.height() as f32,
        cpu(),
    )?;
    check.context().draw_image(&reloaded, 0.0, 0.0);

    let before = canvas.context().get_image_data(0.0, 0.0, WIDTH, HEIGHT)?;
    let after = check.context().get_image_data(0.0, 0.0, WIDTH, HEIGHT)?;
    // Red and alpha only, as the JavaScript mirror does: a channel that
    // survives tells you the encode round-tripped, and comparing all four
    // would report the same pixels four times.
    let differing = before
        .pixels()
        .as_chunks::<4>()
        .0
        .iter()
        .zip(after.pixels().as_chunks::<4>().0.iter())
        .filter(|(a, b)| a[0] != b[0] || a[3] != b[3])
        .count();

    let data_url =
        canvas.to_data_url(ImageFormat::Png, &EncodeOptions::default())?;

    println!("exports");
    for (label, size) in &results {
        let kb = format!("{:.1}", *size as f64 / BYTES_PER_KIB);
        println!("  {label:<14} {kb:>8} KB");
    }

    let backend = BackendInfo::query();
    println!("\nchecks");
    println!(
        "  {:<LABEL_WIDTH$}{:.1} px over {} lines",
        "paragraph height",
        paragraph.height(),
        paragraph.line_count()
    );
    println!(
        "  {:<LABEL_WIDTH$}{}x{}",
        "reloaded image",
        reloaded.width(),
        reloaded.height()
    );
    println!(
        "  {:<LABEL_WIDTH$}{differing} of {} pixels",
        "png round-trip differs",
        (WIDTH * HEIGHT) as usize
    );
    println!(
        "  {:<LABEL_WIDTH$}{}… ({:.0} KB)",
        "data URL",
        &data_url[..data_url.len().min(30)],
        data_url.len() as f64 / BYTES_PER_KIB
    );
    println!(
        "  {:<LABEL_WIDTH$}{}",
        "is_context_lost()",
        canvas.context().is_context_lost()
    );
    println!(
        "  {:<LABEL_WIDTH$}{:?} | {}",
        "engine",
        canvas.engine_kind(),
        backend.device.as_deref().unwrap_or("unnamed device")
    );

    Ok(())
}
