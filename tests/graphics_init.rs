//! Every public way into Skia starts it first.
//!
//! `SkGraphics::Init` selects the raster pipeline's CPU-specific stages, and
//! it is process-wide: once anything has called it, everything after has the
//! stages. So each entry is tried in a process of its own -- this test binary
//! run again with [`CHILD`] naming the entry -- where it is the first thing
//! to reach Skia, and the child then asks Skia directly, never through this
//! crate, whether a half-float conversion rounds to nearest.
//!
//! The probe tells the stages apart on x86, where the portable stage an x86
//! build is compiled with truncates and flushes subnormals and the AVX2 and
//! AVX-512 stages round to nearest. NEON rounds to nearest whether or not
//! anything started Skia, so on an ARM machine every child passes and only an
//! x86 run can show an entry that does not start it. An x86 machine without
//! AVX2 and F16C keeps the portable stage even once started, so there the
//! entries skip, saying why, rather than fail. libtest has no skip, so a
//! skipped entry prints the reason and counts as passed: on such a machine a
//! green run says only that nothing ran, and the message says so.

use meo_skia_canvas::prelude::*;
use skia_safe::{
    AlphaType, ColorType, Data, ImageInfo, image::CachingHint, images,
};
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
use std::arch::is_x86_feature_detected;
use std::{env, process::Command};

/// Whether Skia has a stage on this CPU that rounds a half to nearest: on
/// x86 the AVX2 one, which needs AVX2 and F16C. NEON always does.
///
/// Detected, not assumed: an x86 machine without both keeps the portable
/// stage even once Skia is started, and the tests skip there rather than
/// pass, saying why. libtest has no skip, so it is a message and an early
/// return.
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
fn has_rounding_stage() -> bool {
    is_x86_feature_detected!("avx2") && is_x86_feature_detected!("f16c")
}

/// Whether Skia has a stage on this CPU that rounds a half to nearest; on
/// ARM, NEON's, always.
#[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
fn has_rounding_stage() -> bool {
    true
}

/// Why a test skipped on a CPU without [`has_rounding_stage`].
const NO_ROUNDING_STAGE: &str = "skipped: this x86 CPU lacks AVX2 or F16C, so Skia keeps \
     its portable half-float stage, which truncates (see the UPSTREAM note on \
     `start_skia`)";

/// Set in a child to the entry it tries, by [`run_child`].
const CHILD: &str = "MEO_SKIA_CANVAS_INIT_CHILD";

/// Exactly between the halves `0x3801` and `0x3802`, `0.5 + 2^-11` and
/// `0.5 + 2 * 2^-11`. Round-to-nearest-even gives the even one; truncation
/// the odd.
const TIE: f32 = 0.5 + 1.5 / 2048.0;

/// [`TIE`] as a half under round-to-nearest-even.
const TIE_AS_HALF: u16 = 0x3802;

/// `2^-20`, the subnormal half `0x0010`, which a flushing store makes zero.
const SUBNORMAL: f32 = 1.0 / 1_048_576.0;

/// [`SUBNORMAL`] as a half.
const SUBNORMAL_AS_HALF: u16 = 0x0010;

/// Whether Skia, asked directly, narrows floats to the nearest half: a 1x1
/// float image read back as half floats.
fn rounds_to_nearest() -> bool {
    let info = |color_type| {
        ImageInfo::new((1, 1), color_type, AlphaType::Premul, None)
    };
    let floats: Vec<u8> = [TIE, SUBNORMAL, 0.0, 1.0]
        .iter()
        .flat_map(|v| v.to_ne_bytes())
        .collect();
    let image = images::raster_from_data(
        &info(ColorType::RGBAF32),
        Data::new_copy(&floats),
        16,
    )
    .expect("a float image");
    let mut halves = [0u8; 8];
    assert!(image.read_pixels(
        &info(ColorType::RGBAF16),
        &mut halves,
        8,
        (0, 0),
        CachingHint::Allow
    ));
    let half =
        |i: usize| u16::from_ne_bytes([halves[i * 2], halves[i * 2 + 1]]);
    half(0) == TIE_AS_HALF && half(1) == SUBNORMAL_AS_HALF
}

/// The entries, each the first thing its child does.
fn enter(entry: &str) {
    match entry {
        "none" => {}
        "canvas_new" => drop(Canvas::new(1.0, 1.0)),
        "canvas_with_options" => drop(
            Canvas::with_options(1.0, 1.0, CanvasOptions::default())
                .expect("a canvas"),
        ),
        "image_from_encoded" => {
            // Not decodable: the call reaches Skia before it can refuse.
            let _ = Image::from_encoded(b"not an image");
        }
        "image_from_pixels" => {
            let data = ImageData::blank(1, 1, PixelExportOptions::default())
                .expect("a pixel");
            drop(Image::try_from(&data).expect("an image"));
        }
        "image_from_svg_xml" => {
            let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width="1" height="1"/>"#;
            drop(Image::from_svg_xml(svg, 1, 1).expect("an image"));
        }
        "svg_parse" => {
            let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width="1" height="1"/>"#;
            drop(Svg::parse(svg).expect("an svg"));
        }
        other => panic!("no entry named {other}"),
    }
}

/// In a child, tries its entry and probes, and exits; in the parent, runs
/// `test` again as a child for `entry` and returns whether the probe saw the
/// nearest half.
fn run_child(test: &str, entry: &str) -> bool {
    if let Ok(entry) = env::var(CHILD) {
        enter(&entry);
        std::process::exit(if rounds_to_nearest() { 0 } else { 1 });
    }
    Command::new(env::current_exe().expect("a test binary has a path"))
        .args(["--exact", test, "--test-threads=1", "--nocapture"])
        .env(CHILD, entry)
        .status()
        .expect("the test binary runs")
        .success()
}

macro_rules! starts_skia {
    ($($name:ident: $entry:literal;)*) => {$(
        #[test]
        fn $name() {
            if !has_rounding_stage() {
                eprintln!("{NO_ROUNDING_STAGE}");
                return;
            }
            assert!(
                run_child(stringify!($name), $entry),
                "{} reached Skia without starting it: a half-float store truncated",
                $entry
            );
        }
    )*};
}

starts_skia! {
    a_canvas_from_new_starts_skia: "canvas_new";
    a_canvas_from_options_starts_skia: "canvas_with_options";
    an_encoded_image_starts_skia: "image_from_encoded";
    an_image_from_pixels_starts_skia: "image_from_pixels";
    an_image_from_svg_starts_skia: "image_from_svg_xml";
    a_parsed_svg_starts_skia: "svg_parse";
}

/// The control: with no entry, the probe sees the stages the build was
/// compiled for. On x86 that is the portable one, which truncates -- so this
/// is the case that shows the probe can fail.
#[test]
fn without_a_start_x86_truncates() {
    let rounds = run_child("without_a_start_x86_truncates", "none");
    if cfg!(target_arch = "x86_64") {
        assert!(
            !rounds,
            "the probe found the nearest half with nothing started"
        );
    } else {
        assert!(
            rounds,
            "{} rounds to nearest without a start",
            env::consts::ARCH
        );
    }
}
