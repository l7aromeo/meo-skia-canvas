//! A path drawn on the GPU keeps its coverage whatever the frame drew first.
//!
//! The draws are a page of `examples/node/animated-eye.js`: a blurred blob and
//! the blurred strokes of the shading beside it -- the background -- and a row
//! of eyelashes over them. Drawn source-over, the page's alpha is the
//! background's plus the lashes' over what the background left, at every
//! pixel. On Metal and on Vulkan (lavapipe) it was not: computing the blob's
//! blurred mask in the same flush as the lashes lost one lash's coverage at
//! its tip, (263, 145), where the page read the background's alpha alone. A
//! page whose blob mask was already cached kept the pixel, so the page here
//! is drawn first, in a process that has drawn nothing. Any one of the draws
//! taken away and the pixel came back, so the page is kept whole rather than
//! reduced.

use std::{env, fs};

use meo_skia_canvas::prelude::*;
use serde_json::Value;

/// The page's draws, in order: a verb, the path or the ellipse it fills or
/// strokes, a CSS colour, an alpha, a stroke's width and cap, a mask blur's
/// sigma where there is one, and the part of the page it belongs to.
const DRAWS: &str = "tests/assets/draws/eye-lashes.json";

/// The page the draws were recorded on, in pixels.
const WIDTH: f32 = 640.0;
const HEIGHT: f32 = 500.0;

/// How far a pixel's alpha may lie from the composite of its parts: the
/// rounding of three eight-bit reads, each of a value stored to the nearest
/// 255th.
const ROUNDING: f32 = 1.5;

fn number(draw: &Value, key: &str) -> f32 {
    draw[key]
        .as_f64()
        .unwrap_or_else(|| panic!("{key} in {draw}")) as f32
}

fn draw(ctx: &mut Context2D, draw: &Value) -> Result<(), Error> {
    ctx.save();
    ctx.set_global_alpha(number(draw, "alpha"));
    let style = draw["style"].as_str().expect("a colour");
    let blur = draw["blur"]
        .as_f64()
        .map(|sigma| MaskFilter::blur(BlurStyle::Normal, sigma as f32, true))
        .transpose()?;
    ctx.set_mask_filter(blur.as_ref());
    let path = draw["path"]
        .as_str()
        .map(|data| Path2D::from_svg(data, FillRule::NonZero))
        .transpose()?;
    match draw["verb"].as_str().expect("a verb") {
        "fill" => {
            ctx.set_fill_style_css(style)?;
            match path {
                Some(path) => ctx.fill_path(&path, FillRule::NonZero),
                // An ellipse is built on the context rather than read from
                // SVG path data, which has no conics and would carry it as
                // quads: a different path, and one that does not reproduce.
                None => {
                    let e = |i: usize| {
                        draw["ellipse"][i].as_f64().expect("an ellipse") as f32
                    };
                    ctx.begin_path();
                    ctx.ellipse(
                        e(0),
                        e(1),
                        e(2),
                        e(3),
                        e(4),
                        e(5),
                        e(6),
                        false,
                    )?;
                    ctx.fill(FillRule::NonZero);
                }
            }
        }
        "stroke" => {
            ctx.set_stroke_style_css(style)?;
            ctx.set_line_width(number(draw, "lineWidth"));
            ctx.set_line_cap(match draw["lineCap"].as_str() {
                Some("round") => StrokeCap::Round,
                _ => StrokeCap::Butt,
            });
            ctx.stroke_path(&path.expect("a stroked path"));
        }
        verb => panic!("no draw `{verb}`"),
    }
    ctx.restore();
    Ok(())
}

/// The alpha of every pixel of a GPU page drawn from the `draws` that `keep`
/// keeps.
fn alpha(
    draws: &[Value],
    keep: impl Fn(&str) -> bool,
) -> Result<Vec<u8>, Error> {
    let mut canvas = Canvas::new(WIDTH, HEIGHT);
    canvas.set_gpu(true);
    let ctx = canvas.context();
    for each in draws
        .iter()
        .filter(|d| keep(d["part"].as_str().expect("a part")))
    {
        draw(ctx, each)?;
    }
    let pixels =
        canvas.to_buffer(ImageFormat::Raw, &EncodeOptions::default())?;
    Ok(pixels.chunks(4).map(|px| px[3]).collect())
}

/// Whether Metal has a device to offer, asked of Metal itself rather than of
/// the engine under test.
#[cfg(feature = "metal")]
fn has_metal_device() -> bool {
    objc2_metal::MTLCreateSystemDefaultDevice().is_some()
}

#[cfg(not(feature = "metal"))]
fn has_metal_device() -> bool {
    false
}

/// Text the GPU's device name has to contain where it is set, as on CI's
/// Linux runner, which names lavapipe's `llvmpipe`: a run that has to show it
/// drew on that device, rather than one that skipped for want of a GPU.
const REQUIRE_GPU: &str = "MEO_SKIA_CANVAS_REQUIRE_GPU";

/// Skips where there is no GPU, and fails where Metal has a device but the
/// engine did not start on it: `set_gpu(true)` falls back silently to the
/// raster path, so a skip keyed on `engine_kind` alone would pass on a Mac
/// whose GPU engine broke. Fails as well where [`REQUIRE_GPU`] names a device
/// the engine is not on.
#[test]
fn a_path_over_a_blurred_one_keeps_its_coverage() -> Result<(), Error> {
    let draws: Vec<Value> =
        serde_json::from_str(&fs::read_to_string(DRAWS).expect(DRAWS))
            .expect("the draws parse");
    let mut probe = Canvas::new(1.0, 1.0);
    probe.set_gpu(true);
    if let Ok(required) = env::var(REQUIRE_GPU) {
        let backend = BackendInfo::query();
        assert!(
            probe.engine_kind() == EngineKind::Gpu
                && backend
                    .device
                    .as_deref()
                    .is_some_and(|device| device.contains(&required)),
            "{REQUIRE_GPU} asks for a GPU named {required:?}; the engine is \
             {:?} on {:?}, error {:?}",
            probe.engine_kind(),
            backend.device,
            backend.error,
        );
    }
    if probe.engine_kind() != EngineKind::Gpu {
        assert!(
            !has_metal_device(),
            "Metal has a device, but the GPU engine did not start on it"
        );
        return Ok(());
    }

    // The whole page first: its parts drawn before it would leave the blob's
    // mask cached, and a cached mask hides the loss.
    let page = alpha(&draws, |_| true)?;
    let background = alpha(&draws, |part| part == "background")?;
    let lashes = alpha(&draws, |part| part == "lash")?;

    let lost: Vec<(usize, usize, u8, f32)> = page
        .iter()
        .zip(background.iter().zip(&lashes))
        .enumerate()
        .filter_map(|(i, (&page, (&under, &over)))| {
            let (under, over) = (f32::from(under), f32::from(over));
            let composite = under + over * (1.0 - under / 255.0);
            ((f32::from(page) - composite).abs() > ROUNDING).then_some((
                i % WIDTH as usize,
                i / WIDTH as usize,
                page,
                composite,
            ))
        })
        .collect();
    assert!(
        lost.is_empty(),
        "{} pixels' alpha is not their parts' composite, first (x, y, alpha, \
         composite) {:?}",
        lost.len(),
        lost.first(),
    );
    Ok(())
}
