//! Test cards: one labelled panel per feature area.
//!
//! Run with:
//!
//!     cargo run --example feature_sheet -- [outdir]
//!
//! The Rust half of `examples/node/feature-sheet.js`. Twenty panels across
//! three sheets, each one exercising a corner of the surface, so a change
//! that alters output shows up as a picture rather than as a number.
//!
//! A panel that fails is drawn as the word "failed" and its reason is printed
//! at the end. That is deliberate: the sheet is a survey, and one dead corner
//! should not take the other nineteen with it.

use std::{error::Error, f32::consts::TAU, fs, path::PathBuf};

use meo_skia_canvas::prelude::*;

const COLUMNS: usize = 4;
/// Side of one panel, including its padding.
const CELL: f32 = 300.0;
/// Margin around the whole grid.
const PAD: f32 = 16.0;
/// Height of a panel's label strip, which its drawing starts below.
const HEAD: f32 = 34.0;
/// Room above the grid for the sheet title.
const TITLE_BAND: f32 = 58.0;
const TITLE_BASELINE: f32 = 38.0;
/// Inset of the rounded panel plate inside its cell.
const PLATE_INSET: f32 = 4.0;
const PLATE_RADIUS: f32 = 10.0;
/// Inset of the clipped drawing area inside its cell.
const BODY_INSET: f32 = 8.0;
/// Room left below a panel's drawing area.
const BODY_FOOT: f32 = 12.0;

const BACKGROUND: &str = "#0d1117";
const PLATE: &str = "#161b22";
const TEXT: &str = "#e6edf3";
const MUTED: &str = "#7d8590";
const RULE: &str = "#30363d";
const FAILED: &str = "#f85149";

/// Width of a panel's drawing area.
const BODY_WIDTH: f32 = CELL - 2.0 * BODY_INSET;
/// Height of a panel's drawing area.
const BODY_HEIGHT: f32 = CELL - HEAD - BODY_FOOT;

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

fn font(shorthand: &str) -> Font {
    Font::parse(shorthand).expect("a literal written in this file")
}

/// A panel: its label, and what it draws inside its own clipped body.
///
/// The body is already translated, so a panel draws from `(0, 0)` and does
/// not need to know where on the sheet it landed.
struct Panel {
    label: &'static str,
    draw: fn(&mut Sheet<'_>) -> Result<(), Box<dyn Error>>,
}

/// What a panel draws into: the context, plus the shared assets several of
/// them need.
///
/// Bundled rather than passed separately because the assets are built once
/// for the whole run -- decoding the swatch twenty times would be the slowest
/// thing this program does.
struct Sheet<'a> {
    ctx: &'a mut Context2D,
    assets: &'a mut Assets,
}

struct Assets {
    /// The gradient swatch, as a decoded image, for the `draw_image` panels.
    swatch: Image,
    /// An 8x8 checker, as an image and as the canvas it was drawn on. Both,
    /// because the point of one panel is that they resample differently.
    checker: Image,
    checker_canvas: Canvas,
    /// Laid-out text needs an engine, and building one scans the system
    /// fonts.
    engine: TextEngine,
}

// ── shared assets ──────────────────────────────────────────────────────────

const SWATCH_DOTS: usize = 5;
const SWATCH_DOT_RADIUS: f32 = 8.0;
const SWATCH_DOT_STEP: f32 = 22.0;
const SWATCH_DOT_LEFT: f32 = 20.0;
const SWATCH_BORDER: f32 = 3.0;

/// A gradient with dots and a border: something with enough structure that a
/// crop, a resample or a colour filter is visible on it.
fn swatch(width: f32, height: f32) -> Result<Canvas, Box<dyn Error>> {
    let mut canvas = Canvas::with_options(width, height, cpu())?;
    let ctx = canvas.context();
    let gradient = Shader::linear_gradient(
        Point::new(0.0, 0.0),
        Point::new(width, height),
        &[
            GradientStop {
                position: 0.0,
                color: hex("#f97316"),
            },
            GradientStop {
                position: 0.5,
                color: hex("#ec4899"),
            },
            GradientStop {
                position: 1.0,
                color: hex("#6366f1"),
            },
        ],
        GradientColorSpace::Srgb,
    )?;
    ctx.set_fill_shader(&gradient);
    ctx.fill_rect(0.0, 0.0, width, height);

    ctx.set_fill_style(RgbaLinear::from_srgb8(255, 255, 255, 0.9));
    for i in 0..SWATCH_DOTS {
        ctx.begin_path();
        ctx.arc(
            SWATCH_DOT_LEFT + i as f32 * SWATCH_DOT_STEP,
            height / 2.0,
            SWATCH_DOT_RADIUS,
            0.0,
            TAU,
            false,
        )?;
        ctx.fill(FillRule::NonZero);
    }
    ctx.set_stroke_style(hex("#111111"));
    ctx.set_line_width(SWATCH_BORDER);
    ctx.stroke_rect(0.0, 0.0, width, height);
    Ok(canvas)
}

/// An 8x8 checker, small enough that upscaling it makes the resampling
/// filter obvious.
const CHECKER: f32 = 8.0;

fn checker() -> Result<Canvas, Box<dyn Error>> {
    let mut canvas = Canvas::with_options(CHECKER, CHECKER, cpu())?;
    let ctx = canvas.context();
    for y in 0..CHECKER as usize {
        for x in 0..CHECKER as usize {
            ctx.set_fill_style(match (x + y) % 2 {
                0 => hex(BACKGROUND),
                _ => hex("#58a6ff"),
            });
            ctx.fill_rect(x as f32, y as f32, 1.0, 1.0);
        }
    }
    Ok(canvas)
}

/// Encodes a canvas and decodes it back, which is how a drawing becomes an
/// [`Image`] -- the type the `draw_image` panels need and the one
/// `loadImage` hands the JavaScript side.
fn to_image(canvas: &mut Canvas) -> Result<Image, Box<dyn Error>> {
    let png = canvas.to_buffer(ImageFormat::Png, &EncodeOptions::default())?;
    Ok(Image::from_encoded(&png)?)
}

// ══════════════════════════════════════════════════════════ TYPOGRAPHY ═════

fn text_align(sheet: &mut Sheet<'_>) -> Result<(), Box<dyn Error>> {
    let ctx = &mut *sheet.ctx;
    ctx.set_fill_style(hex(RULE));
    ctx.fill_rect(BODY_WIDTH / 2.0 - 0.5, 6.0, 1.0, 200.0);
    ctx.set_font(&font("15px Helvetica"));
    // `Start` and `End` follow the text direction, so in a left-to-right
    // context they land on `Left` and `Right` -- which is the point: five
    // values, three positions.
    for (i, (name, align)) in [
        ("left", TextAlign::Left),
        ("center", TextAlign::Center),
        ("right", TextAlign::Right),
        ("start", TextAlign::Start),
        ("end", TextAlign::End),
    ]
    .into_iter()
    .enumerate()
    {
        ctx.set_text_align(align);
        ctx.set_fill_style(hex("#58a6ff"));
        ctx.fill_text(name, BODY_WIDTH / 2.0, 30.0 + i as f32 * 28.0, None);
    }
    ctx.set_text_align(TextAlign::Left);
    Ok(())
}

fn text_baseline(sheet: &mut Sheet<'_>) -> Result<(), Box<dyn Error>> {
    let ctx = &mut *sheet.ctx;
    ctx.set_font(&font("14px Helvetica"));
    for (i, (name, baseline)) in [
        ("top", TextBaseline::Top),
        ("hanging", TextBaseline::Hanging),
        ("middle", TextBaseline::Middle),
        ("alphabetic", TextBaseline::Alphabetic),
        ("ideographic", TextBaseline::Ideographic),
        ("bottom", TextBaseline::Bottom),
    ]
    .into_iter()
    .enumerate()
    {
        let y = 24.0 + i as f32 * 30.0;
        ctx.set_stroke_style(hex(RULE));
        ctx.begin_path();
        ctx.move_to(6.0, y);
        ctx.line_to(BODY_WIDTH - 6.0, y);
        ctx.stroke();
        ctx.set_text_baseline(baseline);
        ctx.set_fill_style(hex("#7ee787"));
        ctx.fill_text(name, 10.0, y, None);
    }
    ctx.set_text_baseline(TextBaseline::Alphabetic);
    Ok(())
}

fn font_variant(sheet: &mut Sheet<'_>) -> Result<(), Box<dyn Error>> {
    let ctx = &mut *sheet.ctx;
    // The JavaScript side takes the CSS `font-variant` shorthand as a string;
    // the typed form here splits it into the caps keyword and the OpenType
    // features it implies, which is what the shorthand compiles to anyway.
    let rows: [(&str, FontVariantCaps, &[FontFeature]); 4] = [
        ("normal", FontVariantCaps::Normal, &[]),
        ("small-caps", FontVariantCaps::SmallCaps, &[]),
        ("titling-caps", FontVariantCaps::TitlingCaps, &[]),
        ("oldstyle-nums", FontVariantCaps::Normal, &[]),
    ];
    for (i, (label, caps, features)) in rows.into_iter().enumerate() {
        ctx.set_font(&font("20px Helvetica"));
        match label {
            // `onum` has no caps keyword to ride in on, so it is named
            // directly. This is the case the string form hides.
            "oldstyle-nums" => {
                ctx.set_font_variant(caps, &[FontFeature::on("onum")])
            }
            _ => ctx.set_font_variant(caps, features),
        }
        ctx.set_fill_style(hex(TEXT));
        ctx.fill_text("Hamburg 2026", 10.0, 34.0 + i as f32 * 42.0, None);
        ctx.set_font(&font("10px Helvetica"));
        ctx.set_fill_style(hex(MUTED));
        ctx.fill_text(label, 10.0, 48.0 + i as f32 * 42.0, None);
    }
    ctx.set_font_variant(FontVariantCaps::Normal, &[]);
    Ok(())
}

fn spacing(sheet: &mut Sheet<'_>) -> Result<(), Box<dyn Error>> {
    let ctx = &mut *sheet.ctx;
    for (i, (letter, word)) in [
        ("0px", "0px"),
        ("4px", "0px"),
        ("0px", "14px"),
        ("-1px", "0px"),
    ]
    .into_iter()
    .enumerate()
    {
        // Through the CSS setters, so the strings read as they do in the
        // JavaScript mirror. `set_letter_spacing(4.0)` is the typed form.
        ctx.set_font(&font("17px Helvetica"));
        ctx.set_letter_spacing_css(letter)?;
        ctx.set_word_spacing_css(word)?;
        ctx.set_fill_style(hex(TEXT));
        ctx.fill_text("spaced out text", 10.0, 34.0 + i as f32 * 44.0, None);

        // The caption is drawn with the row's spacing still applied, which
        // is what the JavaScript mirror does -- so each label is set in the
        // spacing it describes.
        ctx.set_font(&font("10px Helvetica"));
        ctx.set_fill_style(hex(MUTED));
        ctx.fill_text(
            &format!("letter {letter} · word {word}"),
            10.0,
            50.0 + i as f32 * 44.0,
            None,
        );
    }
    ctx.set_letter_spacing(0.0);
    ctx.set_word_spacing(0.0);
    Ok(())
}

fn outline_text(sheet: &mut Sheet<'_>) -> Result<(), Box<dyn Error>> {
    let ctx = &mut *sheet.ctx;
    ctx.set_font(&font("700 46px Helvetica"));
    let glyphs = ctx.outline_text("Glyph", None);
    ctx.save();
    ctx.translate(10.0, 70.0);
    ctx.set_stroke_style(hex("#f778ba"));
    ctx.set_line_width(1.2);
    ctx.stroke_path(&glyphs);
    ctx.translate(0.0, 66.0);
    ctx.set_fill_style(hex("#1f6feb"));
    ctx.fill_path(&glyphs.jitter(4.0, 1.4, 7), FillRule::NonZero);
    ctx.restore();
    ctx.set_fill_style(hex(MUTED));
    ctx.set_font(&font("10px Helvetica"));
    ctx.fill_text("stroked, then jitter()", 10.0, 200.0, None);
    Ok(())
}

fn measure_text(sheet: &mut Sheet<'_>) -> Result<(), Box<dyn Error>> {
    let ctx = &mut *sheet.ctx;
    ctx.set_font(&font("26px Helvetica"));
    let text = "Measure me";
    let metrics = ctx.measure_text(text, None);
    let (x, y) = (12.0, 70.0);

    ctx.set_fill_style(RgbaLinear::from_srgb8(88, 166, 255, 0.18));
    ctx.fill_rect(
        x - metrics.actual_bounding_box_left,
        y - metrics.actual_bounding_box_ascent,
        metrics.actual_bounding_box_left + metrics.actual_bounding_box_right,
        metrics.actual_bounding_box_ascent
            + metrics.actual_bounding_box_descent,
    );
    ctx.set_stroke_style(hex("#f0883e"));
    ctx.begin_path();
    ctx.move_to(x, y);
    ctx.line_to(x + metrics.width, y);
    ctx.stroke();
    ctx.set_fill_style(hex(TEXT));
    ctx.fill_text(text, x, y, None);

    ctx.set_fill_style(hex(MUTED));
    ctx.set_font(&font("11px Helvetica"));
    for (i, line) in [
        format!("width {:.1}", metrics.width),
        format!("ascent {:.1}", metrics.actual_bounding_box_ascent),
        format!("descent {:.1}", metrics.actual_bounding_box_descent),
    ]
    .into_iter()
    .enumerate()
    {
        ctx.fill_text(&line, 12.0, 120.0 + i as f32 * 18.0, None);
    }
    Ok(())
}

fn text_wrap(sheet: &mut Sheet<'_>) -> Result<(), Box<dyn Error>> {
    let ctx = &mut *sheet.ctx;
    ctx.set_text_wrap(true);
    ctx.set_font(&font("14px Helvetica"));
    ctx.set_fill_style(hex(TEXT));
    ctx.fill_text(
        "With text wrap enabled the context breaks a long string across \
         lines by itself, using the width given to fill_text.",
        10.0,
        26.0,
        Some(BODY_WIDTH - 20.0),
    );
    ctx.set_text_wrap(false);
    Ok(())
}

fn decoration_styles(sheet: &mut Sheet<'_>) -> Result<(), Box<dyn Error>> {
    for (i, (name, style)) in [
        ("Solid", TextDecorationStyle::Solid),
        ("Double", TextDecorationStyle::Double),
        ("Dotted", TextDecorationStyle::Dotted),
        ("Dashed", TextDecorationStyle::Dashed),
        ("Wavy", TextDecorationStyle::Wavy),
    ]
    .into_iter()
    .enumerate()
    {
        let laid_out = sheet.assets.engine.layout_text(
            &format!("{name} underline"),
            &TextStyle {
                font_families: vec!["Helvetica".to_string()],
                font_size: 17.0,
                color: hex(TEXT),
                decoration: TextDecoration::underline(),
                decoration_style: style,
                decoration_color: Some(hex("#f778ba")),
                ..TextStyle::default()
            },
            BODY_WIDTH - 20.0,
        );
        sheet
            .ctx
            .draw_paragraph(&laid_out, 10.0, 12.0 + i as f32 * 38.0);
    }
    Ok(())
}

// ═════════════════════════════════════════════════════ IMAGE & COLOUR ══════

fn draw_image_crop(sheet: &mut Sheet<'_>) -> Result<(), Box<dyn Error>> {
    let image = sheet.assets.swatch.clone();
    let ctx = &mut *sheet.ctx;
    ctx.draw_image_region(
        &image, 0.0, 0.0, 60.0, 60.0, 8.0, 10.0, 118.0, 118.0,
    );
    ctx.draw_image_region(
        &image, 60.0, 60.0, 60.0, 60.0, 134.0, 10.0, 118.0, 118.0,
    );
    ctx.set_fill_style(hex(MUTED));
    ctx.set_font(&font("10px Helvetica"));
    ctx.fill_text("top-left crop", 8.0, 146.0, None);
    ctx.fill_text("bottom-right crop", 134.0, 146.0, None);
    // The uncropped source the two above were cut from. Square, because the
    // swatch is: a 244x60 box would flatten its circles into ellipses and
    // look like a rendering fault rather than a deliberate stretch.
    ctx.draw_image_sized(&image, 8.0, 152.0, 94.0, 94.0);
    ctx.fill_text("source, uncropped", 112.0, 202.0, None);
    Ok(())
}

/// Square cells: the source is 8x8, so anything but an equal scale on both
/// axes turns its squares into rectangles and reads as a defect.
const RESAMPLE_CELL: f32 = 104.0;
const RESAMPLE_GAP: f32 = 26.0;

fn smoothing_quality(sheet: &mut Sheet<'_>) -> Result<(), Box<dyn Error>> {
    let checker = sheet.assets.checker.clone();
    let column = |i: usize| 8.0 + i as f32 * (RESAMPLE_CELL + RESAMPLE_GAP);

    {
        let ctx = &mut *sheet.ctx;
        ctx.set_font(&font("10px Helvetica"));
        for (i, (label, quality)) in [
            ("low", SmoothingQuality::Low),
            ("high", SmoothingQuality::High),
        ]
        .into_iter()
        .enumerate()
        {
            ctx.set_image_smoothing_quality(quality);
            ctx.draw_image_sized(
                &checker,
                column(i),
                10.0,
                RESAMPLE_CELL,
                RESAMPLE_CELL,
            );
            ctx.set_fill_style(hex(MUTED));
            ctx.fill_text(label, column(i), 126.0, None);
        }

        ctx.set_image_smoothing_enabled(false);
        ctx.draw_image_sized(
            &checker,
            column(0),
            134.0,
            RESAMPLE_CELL,
            RESAMPLE_CELL,
        );
        ctx.set_image_smoothing_enabled(true);
    }

    // Resampling applies to an *image* source. A canvas source goes through
    // `draw_canvas`, which replays the recording at the destination scale
    // instead of resampling pixels, so the smoothing settings have nothing to
    // filter -- the bottom-right cell is the same checker with no resampling
    // artifacts at all.
    let mut source = checker_canvas_clone()?;
    sheet.ctx.draw_canvas_sized(
        &mut source,
        column(1),
        134.0,
        RESAMPLE_CELL,
        RESAMPLE_CELL,
    );

    let ctx = &mut *sheet.ctx;
    ctx.set_fill_style(hex(MUTED));
    ctx.fill_text("smoothing off", column(0), 250.0, None);
    ctx.fill_text("drawCanvas · replayed", column(1), 250.0, None);
    Ok(())
}

/// `draw_canvas_sized` needs `&mut Canvas`, and the shared assets are already
/// borrowed through the sheet at that point, so this panel redraws its own.
/// Eight by eight -- it costs nothing.
fn checker_canvas_clone() -> Result<Canvas, Box<dyn Error>> {
    checker()
}

const TILE: f32 = 24.0;

fn patterns(sheet: &mut Sheet<'_>) -> Result<(), Box<dyn Error>> {
    let mut tile = Canvas::with_options(TILE, TILE, cpu())?;
    {
        let ctx = tile.context();
        ctx.set_fill_style(hex("#1f6feb"));
        ctx.fill_rect(0.0, 0.0, TILE / 2.0, TILE / 2.0);
        ctx.set_fill_style(hex("#f778ba"));
        ctx.fill_rect(TILE / 2.0, TILE / 2.0, TILE / 2.0, TILE / 2.0);
    }

    let repeat = sheet
        .ctx
        .create_pattern_from_canvas(&mut tile, PatternRepeat::Repeat);
    let repeat_x = sheet
        .ctx
        .create_pattern_from_canvas(&mut tile, PatternRepeat::RepeatX);

    let ctx = &mut *sheet.ctx;
    ctx.set_fill_pattern(&repeat);
    ctx.fill_rect(8.0, 10.0, BODY_WIDTH - 16.0, 100.0);

    // A pattern is anchored to the coordinate origin, not to the rect it
    // fills. Filling at y=120 with repeat-x drew nothing at all: the one
    // tile-high band lives at y=0..24, which that rect never touches.
    ctx.save();
    ctx.translate(8.0, 120.0);
    ctx.set_fill_pattern(&repeat_x);
    ctx.fill_rect(0.0, 0.0, BODY_WIDTH - 16.0, 90.0);
    ctx.restore();

    ctx.set_fill_style(hex(MUTED));
    ctx.set_font(&font("10px Helvetica"));
    ctx.fill_text("repeat", 8.0, 226.0, None);
    ctx.fill_text(
        "repeat-x: one band, then nothing below it",
        8.0,
        240.0,
        None,
    );
    Ok(())
}

const PIXEL_PANEL_HEIGHT: u32 = 150;
/// Periods of the two ripples, in pixels. Different and coprime-ish, so the
/// red and green rings beat against each other instead of overlapping.
const RIPPLE_RED: f32 = 12.0;
const RIPPLE_GREEN: f32 = 18.0;
const CHANNELS: usize = 4;

fn image_data(sheet: &mut Sheet<'_>) -> Result<(), Box<dyn Error>> {
    let width = (BODY_WIDTH - 16.0) as u32;
    let mut pixels = sheet.ctx.create_image_data(width, PIXEL_PANEL_HEIGHT)?;
    {
        let buffer = pixels.pixels_mut();
        for y in 0..PIXEL_PANEL_HEIGHT {
            for x in 0..width {
                let at = ((y * width + x) as usize) * CHANNELS;
                let distance = (x as f32 - width as f32 / 2.0)
                    .hypot(y as f32 - PIXEL_PANEL_HEIGHT as f32 / 2.0);
                buffer[at] =
                    (40.0 + 200.0 * (distance / RIPPLE_RED).sin().abs()) as u8;
                buffer[at + 1] = (60.0
                    + 120.0 * (distance / RIPPLE_GREEN).cos().abs())
                    as u8;
                buffer[at + 2] = 200;
                buffer[at + 3] = 255;
            }
        }
    }

    let mut holder = Canvas::with_options(
        pixels.width() as f32,
        pixels.height() as f32,
        cpu(),
    )?;
    holder.context().put_image_data(&pixels, 0.0, 0.0)?;
    sheet.ctx.draw_canvas(&mut holder, 8.0, 14.0);

    let ctx = &mut *sheet.ctx;
    ctx.set_fill_style(hex(MUTED));
    ctx.set_font(&font("10px Helvetica"));
    ctx.fill_text(
        &format!(
            "{}x{}, {} bytes/px",
            pixels.width(),
            pixels.height(),
            pixels.depth().bytes_per_pixel()
        ),
        8.0,
        186.0,
        None,
    );
    ctx.fill_text(
        "put_image_data ignores the transform, so it goes via a canvas",
        8.0,
        202.0,
        None,
    );
    Ok(())
}

// ═════════════════════════════════════════════════════ EFFECTS & PATHS ═════

fn image_filters(sheet: &mut Sheet<'_>) -> Result<(), Box<dyn Error>> {
    let ctx = &mut *sheet.ctx;
    ctx.set_image_filter(Some(&ImageFilter::drop_shadow(
        6.0,
        6.0,
        5.0,
        5.0,
        hex("#000000"),
        None,
        None,
    )?));
    ctx.set_fill_style(hex("#f0883e"));
    ctx.begin_path();
    ctx.round_rect(20.0, 24.0, 110.0, 90.0, [14.0; 4])?;
    ctx.fill(FillRule::NonZero);

    ctx.set_image_filter(Some(&ImageFilter::drop_shadow_only(
        6.0,
        6.0,
        5.0,
        5.0,
        hex("#58a6ff"),
        None,
        None,
    )?));
    ctx.set_fill_style(RgbaLinear::from_srgb8(255, 255, 255, 1.0));
    ctx.begin_path();
    ctx.round_rect(150.0, 24.0, 110.0, 90.0, [14.0; 4])?;
    ctx.fill(FillRule::NonZero);

    ctx.set_image_filter(None);
    ctx.set_fill_style(hex(MUTED));
    ctx.set_font(&font("10px Helvetica"));
    ctx.fill_text("drop-shadow", 20.0, 132.0, None);
    ctx.fill_text("drop-shadow-only", 150.0, 132.0, None);

    ctx.set_fill_style(hex("#7ee787"));
    ctx.set_font(&font("700 26px Helvetica"));
    ctx.set_image_filter(Some(&ImageFilter::dilate(3.0, 3.0, None, None)?));
    ctx.fill_text("dilate", 20.0, 190.0, None);
    ctx.set_image_filter(Some(&ImageFilter::erode(1.0, 1.0, None, None)?));
    ctx.fill_text("erode", 150.0, 190.0, None);
    ctx.set_image_filter(None);
    Ok(())
}

fn color_filters(sheet: &mut Sheet<'_>) -> Result<(), Box<dyn Error>> {
    let mut source = swatch(120.0, 84.0)?;

    // An inverted ramp: the table filter's canonical demonstration, and the
    // one transform that is obviously itself at a glance.
    let mut inverted = [0u8; 256];
    for (i, entry) in inverted.iter_mut().enumerate() {
        *entry = 255 - i as u8;
    }

    let filters: [(Option<ColorFilter>, f32, f32); 4] = [
        (None, 8.0, 14.0),
        (Some(ColorFilter::luma()), 140.0, 14.0),
        (Some(ColorFilter::table(inverted)?), 8.0, 110.0),
        (
            Some(ColorFilter::blend(hex("#1f6feb"), BlendMode::Multiply)?),
            140.0,
            110.0,
        ),
    ];
    for (filter, x, y) in filters {
        sheet.ctx.set_color_filter(filter.as_ref());
        sheet.ctx.draw_canvas_sized(&mut source, x, y, 120.0, 84.0);
    }

    let ctx = &mut *sheet.ctx;
    ctx.set_color_filter(None);
    ctx.set_fill_style(hex(MUTED));
    ctx.set_font(&font("10px Helvetica"));
    ctx.fill_text("plain · luma · inverted table · blend", 8.0, 210.0, None);
    Ok(())
}

fn noise_shaders(sheet: &mut Sheet<'_>) -> Result<(), Box<dyn Error>> {
    let ctx = &mut *sheet.ctx;
    ctx.set_fill_shader(&Shader::turbulence(0.05, 0.05, 4, 3.0)?);
    ctx.fill_rect(8.0, 14.0, BODY_WIDTH - 16.0, 92.0);
    ctx.set_fill_shader(&Shader::fractal_noise(0.02, 0.02, 5, 9.0)?);
    ctx.fill_rect(8.0, 112.0, BODY_WIDTH - 16.0, 92.0);
    ctx.set_fill_style(hex(MUTED));
    ctx.set_font(&font("10px Helvetica"));
    ctx.fill_text("turbulence / fractal-noise", 8.0, 218.0, None);
    Ok(())
}

fn textures(sheet: &mut Sheet<'_>) -> Result<(), Box<dyn Error>> {
    let mut stroke = PathBuilder::new();
    stroke.move_to(0.0, 0.0);
    stroke.line_to(10.0, 10.0);

    let stamped = Texture::new(&TextureOptions {
        path: Some(stroke.build(FillRule::NonZero)),
        color: hex("#7ee787"),
        line: 2.0,
        spacing: (12.0, 12.0),
        ..TextureOptions::default()
    });
    // No path, so this hatches with plain lines -- and a quarter turn makes
    // them vertical.
    let hatched = Texture::new(&TextureOptions {
        path: None,
        color: hex("#f778ba"),
        line: 3.0,
        angle: TAU / 4.0,
        spacing: (14.0, 8.0),
        ..TextureOptions::default()
    });

    let ctx = &mut *sheet.ctx;
    for (texture, y) in [(&stamped, 14.0), (&hatched, 116.0)] {
        ctx.set_fill_texture(texture);
        ctx.begin_path();
        ctx.round_rect(8.0, y, BODY_WIDTH - 16.0, 92.0, [10.0; 4])?;
        ctx.fill(FillRule::NonZero);
    }
    ctx.set_fill_style(hex(MUTED));
    ctx.set_font(&font("10px Helvetica"));
    ctx.fill_text("path texture / line texture", 8.0, 222.0, None);
    Ok(())
}

fn boolean_ops(sheet: &mut Sheet<'_>) -> Result<(), Box<dyn Error>> {
    let mut square = PathBuilder::new();
    square.rect(30.0, 24.0, 92.0, 92.0);
    let square = square.build(FillRule::NonZero);

    let mut circle = PathBuilder::new();
    circle.arc(122.0, 116.0, 52.0, 0.0, TAU, false)?;
    let circle = circle.build(FillRule::NonZero);

    let ctx = &mut *sheet.ctx;
    for (i, (op, color)) in [
        (PathOp::Union, "#1f6feb"),
        (PathOp::Intersect, "#f0883e"),
        (PathOp::Xor, "#7ee787"),
    ]
    .into_iter()
    .enumerate()
    {
        // `combine` returns `None` when Skia declines the operation, which
        // it does not for these three on well-formed paths -- but the
        // signature says it can, so the panel says what it would do.
        let Some(combined) = square.combine(&circle, op) else {
            continue;
        };
        ctx.save();
        ctx.set_global_alpha(0.55);
        ctx.set_fill_style(hex(color));
        ctx.fill_path(
            &combined.offset(i as f32 * 6.0, i as f32 * 6.0),
            FillRule::NonZero,
        );
        ctx.restore();
    }
    ctx.set_global_alpha(1.0);
    ctx.set_fill_style(hex(MUTED));
    ctx.set_font(&font("10px Helvetica"));
    ctx.fill_text("union · intersect · xor", 8.0, 210.0, None);
    Ok(())
}

/// Points around each ring. Ten, because a five-pointed star alternates two
/// radii around it.
const RING_POINTS: usize = 10;
const STAR_OUTER: f32 = 58.0;
const STAR_INNER: f32 = 26.0;

/// A closed ring of `RING_POINTS` vertices, at `radius` about a centre.
///
/// Both shapes an `interpolate` runs between must have the same verb
/// sequence, which is why the ellipse and the small circle below are built by
/// this one function rather than with `arc`: two paths from `arc` differ in
/// how Skia broke them into conics, and `interpolate` returns `None` for a
/// pair that does not match.
fn ring(
    centre: Point,
    radius_x: f32,
    radius_y: f32,
    quarter_turn_offset: f32,
) -> Result<Path2D, Box<dyn Error>> {
    let mut builder = PathBuilder::new();
    for i in 0..RING_POINTS {
        let angle = (i as f32 / RING_POINTS as f32) * TAU - quarter_turn_offset;
        let x = centre.x + angle.cos() * radius_x;
        let y = centre.y + angle.sin() * radius_y;
        match i {
            0 => builder.move_to(x, y),
            _ => builder.line_to(x, y),
        };
    }
    builder.close_path();
    Ok(builder.build(FillRule::NonZero))
}

fn trim_and_interpolate(sheet: &mut Sheet<'_>) -> Result<(), Box<dyn Error>> {
    let quarter = TAU / 4.0;

    let mut star = PathBuilder::new();
    for i in 0..RING_POINTS {
        let radius = match i % 2 {
            0 => STAR_OUTER,
            _ => STAR_INNER,
        };
        let angle = (i as f32 / RING_POINTS as f32) * TAU - quarter;
        let x = 132.0 + angle.cos() * radius;
        let y = 74.0 + angle.sin() * radius;
        match i {
            0 => star.move_to(x, y),
            _ => star.line_to(x, y),
        };
    }
    star.close_path();
    let star = star.build(FillRule::NonZero);

    let ctx = &mut *sheet.ctx;
    ctx.set_stroke_style(hex(RULE));
    ctx.set_line_width(2.0);
    ctx.stroke_path(&star);
    ctx.set_stroke_style(hex("#f778ba"));
    ctx.set_line_width(4.0);
    ctx.stroke_path(&star.trim(0.0, 0.55, false));

    let wide = ring(Point::new(132.0, 180.0), 44.0, 30.0, quarter)?;
    let small = ring(Point::new(132.0, 180.0), 12.0, 12.0, quarter)?;
    ctx.set_stroke_style(hex("#58a6ff"));
    ctx.set_line_width(2.0);
    for weight in [0.0, 0.35, 0.7, 1.0] {
        if let Some(step) = wide.interpolate(&small, weight) {
            ctx.stroke_path(&step);
        }
    }

    ctx.set_fill_style(hex(MUTED));
    ctx.set_font(&font("10px Helvetica"));
    ctx.fill_text("trim() / interpolate()", 8.0, 218.0, None);
    Ok(())
}

const BOARD: f32 = 120.0;
const BOARD_SQUARES: usize = 6;

fn projection(sheet: &mut Sheet<'_>) -> Result<(), Box<dyn Error>> {
    let mut board = Canvas::with_options(BOARD, BOARD, cpu())?;
    {
        let ctx = board.context();
        let square = BOARD / BOARD_SQUARES as f32;
        for y in 0..BOARD_SQUARES {
            for x in 0..BOARD_SQUARES {
                ctx.set_fill_style(match (x + y) % 2 {
                    0 => hex("#1f6feb"),
                    _ => hex(TEXT),
                });
                ctx.fill_rect(
                    x as f32 * square,
                    y as f32 * square,
                    square,
                    square,
                );
            }
        }
    }

    let quad = [
        Point::new(40.0, 20.0),
        Point::new(240.0, 50.0),
        Point::new(210.0, 190.0),
        Point::new(60.0, 160.0),
    ];
    let basis = [
        Point::new(0.0, 0.0),
        Point::new(BOARD, 0.0),
        Point::new(BOARD, BOARD),
        Point::new(0.0, BOARD),
    ];

    sheet.ctx.save();
    if let Some(perspective) = sheet.ctx.create_projection(quad, Some(basis)) {
        // Multiplies into the panel's own translation rather than
        // replacing it, which is what the JavaScript `ctx.transform` does.
        sheet.ctx.transform_projection(&perspective);
        sheet.ctx.draw_canvas(&mut board, 0.0, 0.0);
    }
    sheet.ctx.restore();

    let ctx = &mut *sheet.ctx;
    ctx.set_fill_style(hex(MUTED));
    ctx.set_font(&font("10px Helvetica"));
    ctx.fill_text("quad-mapped draw_canvas", 8.0, 216.0, None);
    Ok(())
}

fn dashes_caps_joins(sheet: &mut Sheet<'_>) -> Result<(), Box<dyn Error>> {
    let ctx = &mut *sheet.ctx;
    ctx.set_line_width(7.0);
    for (i, (dash, color)) in [
        (&[][..], "#58a6ff"),
        (&[14.0, 8.0][..], "#f0883e"),
        (&[2.0, 8.0][..], "#7ee787"),
    ]
    .into_iter()
    .enumerate()
    {
        ctx.set_line_dash(dash);
        // Round caps on the last one, so the 2px dashes read as dots rather
        // than as very short dashes.
        ctx.set_line_cap(match i {
            2 => StrokeCap::Round,
            _ => StrokeCap::Butt,
        });
        ctx.set_stroke_style(hex(color));
        ctx.begin_path();
        ctx.move_to(14.0, 28.0 + i as f32 * 34.0);
        ctx.line_to(BODY_WIDTH - 14.0, 28.0 + i as f32 * 34.0);
        ctx.stroke();
    }
    ctx.set_line_dash(&[]);

    for (i, (name, join)) in [
        ("miter", StrokeJoin::Miter),
        ("round", StrokeJoin::Round),
        ("bevel", StrokeJoin::Bevel),
    ]
    .into_iter()
    .enumerate()
    {
        ctx.set_line_join(join);
        ctx.set_stroke_style(hex("#f778ba"));
        ctx.set_line_width(10.0);
        ctx.begin_path();
        ctx.move_to(24.0 + i as f32 * 84.0, 190.0);
        ctx.line_to(56.0 + i as f32 * 84.0, 140.0);
        ctx.line_to(88.0 + i as f32 * 84.0, 190.0);
        ctx.stroke();
        ctx.set_fill_style(hex(MUTED));
        ctx.set_font(&font("10px Helvetica"));
        ctx.fill_text(name, 32.0 + i as f32 * 84.0, 208.0, None);
    }
    Ok(())
}

// ── the sheets ─────────────────────────────────────────────────────────────

const TYPOGRAPHY: &[Panel] = &[
    Panel {
        label: "text_align · every value",
        draw: text_align,
    },
    Panel {
        label: "text_baseline · every value",
        draw: text_baseline,
    },
    Panel {
        label: "font_variant · caps & figures",
        draw: font_variant,
    },
    Panel {
        label: "letter_spacing · word_spacing",
        draw: spacing,
    },
    Panel {
        label: "outline_text → Path2D",
        draw: outline_text,
    },
    Panel {
        label: "measure_text · TextMetrics",
        draw: measure_text,
    },
    Panel {
        label: "text_wrap · fill_text",
        draw: text_wrap,
    },
    Panel {
        label: "Paragraph · decoration styles",
        draw: decoration_styles,
    },
];

const IMAGERY: &[Panel] = &[
    Panel {
        label: "draw_image_region · crop",
        draw: draw_image_crop,
    },
    Panel {
        label: "image_smoothing_quality",
        draw: smoothing_quality,
    },
    Panel {
        label: "create_pattern · repetition",
        draw: patterns,
    },
    Panel {
        label: "ImageData · direct pixels",
        draw: image_data,
    },
];

const EFFECTS: &[Panel] = &[
    Panel {
        label: "ImageFilter · drop-shadow",
        draw: image_filters,
    },
    Panel {
        label: "ColorFilter · matrix & table",
        draw: color_filters,
    },
    Panel {
        label: "Shader · noise fills",
        draw: noise_shaders,
    },
    Panel {
        label: "Texture · hatching",
        draw: textures,
    },
    Panel {
        label: "Path2D · boolean ops",
        draw: boolean_ops,
    },
    Panel {
        label: "Path2D · trim & interpolate",
        draw: trim_and_interpolate,
    },
    Panel {
        label: "create_projection · perspective",
        draw: projection,
    },
    Panel {
        label: "line_dash · caps · joins",
        draw: dashes_caps_joins,
    },
];

/// Draws one sheet, returning it and whatever its panels complained about.
fn draw_sheet(
    title: &str,
    panels: &[Panel],
    assets: &mut Assets,
) -> Result<(Canvas, Vec<String>), Box<dyn Error>> {
    let rows = panels.len().div_ceil(COLUMNS);
    let width = COLUMNS as f32 * CELL + PAD * 2.0;
    let height = rows as f32 * CELL + PAD * 2.0 + TITLE_BAND;
    let mut canvas = Canvas::with_options(width, height, cpu())?;
    let mut notes = Vec::new();

    {
        let ctx = canvas.context();
        ctx.set_fill_style(hex(BACKGROUND));
        ctx.fill_rect(0.0, 0.0, width, height);
        ctx.set_fill_style(hex(TEXT));
        ctx.set_font(&font("600 24px Helvetica"));
        ctx.fill_text(title, PAD + 4.0, TITLE_BASELINE, None);
    }

    for (i, panel) in panels.iter().enumerate() {
        let x = PAD + (i % COLUMNS) as f32 * CELL;
        let y = PAD + 50.0 + (i / COLUMNS) as f32 * CELL;

        {
            let ctx = canvas.context();
            ctx.save();
            ctx.set_fill_style(hex(PLATE));
            ctx.begin_path();
            ctx.round_rect(
                x + PLATE_INSET,
                y + PLATE_INSET,
                CELL - 2.0 * PLATE_INSET,
                CELL - 2.0 * PLATE_INSET,
                [PLATE_RADIUS; 4],
            )?;
            ctx.fill(FillRule::NonZero);

            ctx.set_fill_style(hex(MUTED));
            ctx.set_font(&font("500 12px Helvetica"));
            ctx.fill_text(panel.label, x + 16.0, y + 24.0, None);
            ctx.restore();

            ctx.save();
            ctx.begin_path();
            ctx.rect(x + BODY_INSET, y + HEAD, BODY_WIDTH, BODY_HEIGHT);
            ctx.clip(FillRule::NonZero);
            ctx.translate(x + BODY_INSET, y + HEAD);
        }

        // Each panel is given the context already clipped and translated, so
        // it draws from its own origin. A failure is caught and drawn rather
        // than propagated: the sheet is a survey, and one dead corner should
        // not take the other nineteen with it.
        let outcome = {
            let mut sheet = Sheet {
                ctx: canvas.context(),
                assets,
            };
            (panel.draw)(&mut sheet)
        };

        let ctx = canvas.context();
        if let Err(reason) = outcome {
            notes.push(format!("{}: {reason}", panel.label));
            ctx.set_fill_style(hex(FAILED));
            ctx.set_font(&font("12px Helvetica"));
            ctx.fill_text("failed", 8.0, 24.0, None);
        }
        ctx.restore();
    }

    Ok((canvas, notes))
}

const BYTES_PER_KIB: f64 = 1024.0;

fn main() -> Result<(), Box<dyn Error>> {
    let out = PathBuf::from(
        std::env::args().nth(1).unwrap_or_else(|| "out".to_string()),
    );
    fs::create_dir_all(&out)?;

    let mut assets = Assets {
        swatch: to_image(&mut swatch(120.0, 120.0)?)?,
        checker: to_image(&mut checker()?)?,
        checker_canvas: checker()?,
        engine: TextEngine::with_system_fonts(),
    };
    // Built for the panels that need a canvas source rather than an image;
    // the field exists so the assets are constructed in one place.
    let _ = &assets.checker_canvas;

    let mut notes = Vec::new();
    for (title, panels, name) in [
        ("Typography", TYPOGRAPHY, "typography"),
        ("Images & pixels", IMAGERY, "images"),
        ("Effects & paths", EFFECTS, "effects"),
    ] {
        let (mut canvas, sheet_notes) = draw_sheet(title, panels, &mut assets)?;
        notes.extend(sheet_notes);

        let file = out.join(format!("{name}.png"));
        canvas.to_file(&file, &EncodeOptions::default())?;
        println!(
            "{:<18} {}x{} {:.0} KB",
            format!("{name}.png"),
            canvas.width(),
            canvas.height(),
            fs::metadata(&file)?.len() as f64 / BYTES_PER_KIB
        );
    }

    match notes.is_empty() {
        true => println!("\nall panels drew without failing"),
        false => {
            println!("\nfailures:");
            for note in &notes {
                println!("  - {note}");
            }
        }
    }
    Ok(())
}
