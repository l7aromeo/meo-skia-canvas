//! A fill on an sRGB float canvas reads back as the colour it was named,
//! sRGB-encoded exactly.
//!
//! Skia stores every paint colour in sRGB, converting it when it is set
//! (`SkPaint::setColor`) through skcms's approximate transfer function, so
//! a linear colour on an sRGB F32 canvas read back as much as 9.6e-5 from
//! its encoding. The colour now reaches Skia already encoded.

use meo_skia_canvas::prelude::*;

/// Both engines.
const ENGINES: [bool; 2] = [false, true];

/// CSS Color 4's `gam_sRGB`, in `f64`, mirrored for a negative value as
/// extended sRGB is: the encoding a fill should read back as.
fn encode(v: f32) -> f32 {
    let v = f64::from(v);
    let a = v.abs();
    let e = match a <= 0.0031308 {
        true => 12.92 * a,
        false => 1.055 * a.powf(1.0 / 2.4) - 0.055,
    };
    e.copysign(v) as f32
}

/// The F32 red a 1x1 fill of linear `red` leaves on an F32 canvas in
/// `space`, read back in that space.
fn read_red_in(space: PixelColorSpace, gpu: bool, red: f32) -> f32 {
    let mut canvas = Canvas::with_options(
        1.0,
        1.0,
        CanvasOptions {
            color_type: PixelDepth::F32,
            color_space: space,
            gpu,
            ..CanvasOptions::default()
        },
    )
    .expect("a canvas");
    let ctx = canvas.context();
    ctx.set_fill_style(RgbaLinear::opaque(red, 0.0, 0.0));
    ctx.fill_rect(0.0, 0.0, 1.0, 1.0);
    let data = ctx
        .get_image_data_as(
            0.0,
            0.0,
            1.0,
            1.0,
            PixelExportOptions {
                depth: PixelDepth::F32,
                color_space: space,
                ..PixelExportOptions::default()
            },
        )
        .expect("a pixel");
    let bytes = data.pixels();
    f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

fn read_red(gpu: bool, red: f32) -> f32 {
    read_red_in(PixelColorSpace::Srgb, gpu, red)
}

/// The `i`th of 401 linear reds from -0.1 to 1.1, the extended range
/// included.
fn reds(i: i32) -> f32 {
    i as f32 / 400.0 * 1.2 - 0.1
}

/// Each red read back as its exact encoding.
#[test]
fn an_srgb_float_fill_reads_back_as_its_encoding() {
    for gpu in ENGINES {
        let missed: Vec<_> = (0..=400)
            .map(reds)
            .filter_map(|red| {
                let (got, want) = (read_red(gpu, red), encode(red));
                (got != want).then(|| format!("{red}: {got} for {want}"))
            })
            .collect();
        assert!(
            missed.is_empty(),
            "gpu {gpu}: {} of 401 missed, first {}",
            missed.len(),
            missed[0]
        );
    }
}

/// The mean error a fill on a linear-sRGB F32 page may carry, which Skia's
/// conversion at the draw puts there whatever it is handed: over these reds,
/// handed the colour tagged linear, as it is, it measured 5.0e-5; handed it
/// already in sRGB, which is exact on an sRGB page, 7.7e-5. Halfway between,
/// so that the closer route is the one taken.
const LINEAR_PAGE_MEAN_ERROR: f64 = 6.3e-5;

/// On a linear-sRGB page the colour goes over as named, which measured
/// closer there than going over in sRGB.
#[test]
fn a_linear_float_fill_takes_the_closer_route() {
    let mean = (0..=400)
        .map(reds)
        .map(|red| {
            let got = read_red_in(PixelColorSpace::SrgbLinear, false, red);
            (f64::from(got) - f64::from(red)).abs()
        })
        .sum::<f64>()
        / 401.0;
    assert!(mean <= LINEAR_PAGE_MEAN_ERROR, "mean error {mean:e}");
}

/// A matte beneath an sRGB float canvas reads back as its exact encoding, as
/// a fill does.
#[test]
fn a_matte_on_an_srgb_float_canvas_reads_back_as_its_encoding() {
    for gpu in ENGINES {
        let missed: Vec<_> = (0..=400)
            .map(reds)
            .filter_map(|red| {
                let mut canvas = Canvas::with_options(
                    1.0,
                    1.0,
                    CanvasOptions {
                        color_type: PixelDepth::F32,
                        gpu,
                        ..CanvasOptions::default()
                    },
                )
                .expect("a canvas");
                let data = canvas
                    .context()
                    .get_image_data_with(
                        0.0,
                        0.0,
                        1.0,
                        1.0,
                        &ImageDataExport {
                            pixels: PixelExportOptions {
                                depth: PixelDepth::F32,
                                ..PixelExportOptions::default()
                            },
                            matte: Some(RgbaLinear::opaque(red, 0.0, 0.0)),
                            ..ImageDataExport::default()
                        },
                    )
                    .expect("a pixel");
                let bytes = data.pixels();
                let got = f32::from_le_bytes([
                    bytes[0], bytes[1], bytes[2], bytes[3],
                ]);
                (got != encode(red))
                    .then(|| format!("{red}: {got} for {}", encode(red)))
            })
            .collect();
        assert!(
            missed.is_empty(),
            "gpu {gpu}: {} of 401 missed, first {}",
            missed.len(),
            missed[0]
        );
    }
}

/// A CSS colour named in `srgb-linear` lands as its exact encoding too: the
/// path a colour named in any space other than sRGB takes.
#[test]
fn a_css_colour_named_in_another_space_reads_back_as_its_encoding() {
    let missed: Vec<_> = (0..=400)
        .map(reds)
        .filter_map(|red| {
            let mut canvas = Canvas::with_options(
                1.0,
                1.0,
                CanvasOptions {
                    color_type: PixelDepth::F32,
                    gpu: false,
                    ..CanvasOptions::default()
                },
            )
            .expect("a canvas");
            let ctx = canvas.context();
            ctx.set_fill_style_css(&format!("color(srgb-linear {red} 0 0)"))
                .expect("a colour");
            ctx.fill_rect(0.0, 0.0, 1.0, 1.0);
            let data = ctx
                .get_image_data_as(
                    0.0,
                    0.0,
                    1.0,
                    1.0,
                    PixelExportOptions {
                        depth: PixelDepth::F32,
                        ..PixelExportOptions::default()
                    },
                )
                .expect("a pixel");
            let bytes = data.pixels();
            let got =
                f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
            (got != encode(red))
                .then(|| format!("{red}: {got} for {}", encode(red)))
        })
        .collect();
    assert!(
        missed.is_empty(),
        "{} of 401 missed, first {}",
        missed.len(),
        missed[0]
    );
}
