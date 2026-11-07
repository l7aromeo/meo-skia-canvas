//! A GPU canvas draws text the same whatever the context drew before.
//!
//! Ganesh drew a glyph run from 162 to 324 device pixels as distance fields
//! (`SubRunControl`, src/text/gpu/SubRunControl.cpp), in Skia M156 under the
//! skia-bindings crate. Their coverage moves by a level or two with where in
//! the atlas a glyph was placed, so the same `fillText` exported after other
//! text, on this thread or another, read differently from one exported first.
//! The blob cache also keys such a run without its matrix
//! (`TextBlob::Key::Make`), so a path blob cached at a larger scale was drawn
//! in place of a smaller draw's distance fields.
//! `containers/skia-text-distance-fields-off.patch` turns distance fields off
//! where a surface does not ask for them, which none of this crate's do: a run
//! under 254 device pixels is then a direct mask from the scaler the CPU
//! raster uses, the same bytes as the CPU page.
//!
//! A crate build's Skia does not carry it, and
//! `cfg(meo_text_distance_fields_off)`, from `build.rs`, says which this
//! build compiled. Where the patch is missing, each test holds the build to
//! the defect instead, so the day crates.io's Skia stops showing it, the test
//! says so.

use meo_skia_canvas::{
    export::{EncodeOptions, ImageFormat},
    prelude::*,
};

/// The bundled variable face the history scenes are set in.
const OSWALD: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/assets/fonts/Oswald/Oswald-VariableFont_wght.ttf"
);

/// Another bundled face, for the text drawn before or beside the scene.
const RALEWAY: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/assets/fonts/Raleway/Raleway-VariableFont_wght.ttf"
);

/// The page of issue #622: 96px text under a 2.4 scale, 230 device pixels,
/// inside the range Ganesh drew as distance fields.
const PAGE: (f32, f32) = (1008.0, 336.0);

/// Whether this machine has a GPU engine, saying so where it has not.
fn has_gpu() -> bool {
    let mut probe = Canvas::new(1.0, 1.0);
    probe.set_gpu(true);
    let gpu = probe.engine_kind() == EngineKind::Gpu;
    if !gpu {
        eprintln!("skipped: no GPU engine");
    }
    gpu
}

/// Registers `path`'s bytes under `family`.
fn register(family: &str, path: &str) {
    register_bytes(family, &std::fs::read(path).expect("a bundled face"));
}

/// `canvas` encoded raw, as an export renders it.
fn exported(canvas: &mut Canvas) -> Vec<u8> {
    canvas
        .prepare_export(ImageFormat::Raw, &EncodeOptions::default())
        .and_then(|prepared| prepared.encode())
        .expect("a raw export")
}

/// The page of issue #622, exported on the GPU or the CPU.
fn the_page(gpu: bool) -> Vec<u8> {
    let mut canvas = Canvas::new(PAGE.0, PAGE.1);
    canvas.set_gpu(gpu);
    let ctx = canvas.context();
    ctx.scale(2.4, 2.4);
    let mut font = Font::new("HistoryOswald", 96.0);
    font.weight = 900;
    font.slant = FontSlant::Italic;
    ctx.set_font(&font);
    ctx.fill_text("N.T.E", 16.0, 110.0, None);
    exported(&mut canvas)
}

/// Some other text exported on the GPU, at `px`.
fn other_text(family: &str, px: f32) {
    let mut canvas = Canvas::new(240.0, 60.0);
    canvas.set_gpu(true);
    let ctx = canvas.context();
    ctx.set_font(&Font::new(family, px));
    ctx.fill_text("Hamburgefonstiv", 4.0, 40.0, None);
    exported(&mut canvas);
}

/// The page exported once, after another thread has drawn text in `family`,
/// or, where `family` is `None`, in a family it registers from the scene's
/// own bytes under another name.
fn beside_another_thread(family: Option<&'static str>) -> Vec<u8> {
    let bytes = std::fs::read(OSWALD).expect("a bundled face");
    std::thread::spawn(move || match family {
        Some(family) => other_text(family, 24.0),
        None => {
            register_bytes("HistoryOther", &bytes);
            other_text("HistoryOther", 24.0);
        }
    })
    .join()
    .expect("the other thread finishes");
    the_page(true)
}

/// Registers `bytes` under `family`.
fn register_bytes(family: &str, bytes: &[u8]) {
    FontLibrary::new()
        .register_font_from_data(family, bytes)
        .expect("a bundled face registers");
}

/// Names the history a child process draws the page after.
const HISTORY: &str = "MEO_TEST_TEXT_HISTORY";

/// The histories of issue #622, each run in a process of its own: a glyph is
/// placed in the atlas the first time a context draws it and found there
/// after, so only the first draw in a process shows what came before it.
const HISTORIES: [&str; 7] = [
    "first",
    "after 1 export",
    "after 3 exports",
    "after 20 exports",
    "beside a thread drawing another family of the same bytes",
    "beside a thread drawing another face",
    "beside a thread drawing the same family",
];

/// In a child, the page after `history`, on the GPU and then the CPU.
fn page_after(history: &str) -> (Vec<u8>, Vec<u8>) {
    let exports = |n: u32| {
        (0..n).for_each(|k| other_text("HistoryRaleway", 24.0 + k as f32));
        the_page(true)
    };
    let gpu = match history {
        "first" => the_page(true),
        "after 1 export" => exports(1),
        "after 3 exports" => exports(3),
        "after 20 exports" => exports(20),
        "beside a thread drawing another family of the same bytes" => {
            beside_another_thread(None)
        }
        "beside a thread drawing another face" => {
            beside_another_thread(Some("HistoryRaleway"))
        }
        "beside a thread drawing the same family" => {
            beside_another_thread(Some("HistoryOswald"))
        }
        other => panic!("no history named {other}"),
    };
    (gpu, the_page(false))
}

/// FNV-1a over `bytes`, to name a page in a child's output.
fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

#[test]
fn text_on_the_gpu_draws_as_the_cpu_draws_it_whatever_came_before() {
    const TEST: &str =
        "text_on_the_gpu_draws_as_the_cpu_draws_it_whatever_came_before";
    if !has_gpu() {
        return;
    }
    register("HistoryOswald", OSWALD);
    register("HistoryRaleway", RALEWAY);
    if let Ok(history) = std::env::var(HISTORY) {
        let (gpu, cpu) = page_after(&history);
        println!("PAGE {:016x} {}", fnv(&gpu), gpu == cpu);
        return;
    }
    let pages: Vec<(&str, String, bool)> = HISTORIES
        .iter()
        .map(|&history| {
            let child = std::process::Command::new(
                std::env::current_exe().expect("a test binary has a path"),
            )
            .args(["--exact", TEST, "--test-threads=1", "--nocapture"])
            .env(HISTORY, history)
            .output()
            .expect("the test binary runs");
            assert!(child.status.success(), "the child for {history} failed");
            let out = String::from_utf8_lossy(&child.stdout);
            let line = out
                .lines()
                // The harness prints the test's name on the same line.
                .find_map(|line| Some(line.split_once("PAGE ")?.1))
                .unwrap_or_else(|| panic!("no page from {history}: {out}"));
            let (hash, as_cpu) =
                line.split_once(' ').expect("a hash and a verdict");
            (history, hash.to_string(), as_cpu == "true")
        })
        .collect();
    let apart: Vec<&str> = pages
        .iter()
        .filter(|(_, _, as_cpu)| !as_cpu)
        .map(|(history, _, _)| *history)
        .collect();
    if cfg!(meo_text_distance_fields_off) {
        assert!(apart.is_empty(), "GPU text apart from the CPU's: {apart:?}");
    } else {
        let first = &pages[0].1;
        assert!(
            pages.iter().any(|(_, hash, _)| hash != first),
            "crates.io's Skia drew the page alike after every history: \
             {pages:?}; where it places a glyph no longer moves its pixels"
        );
    }
}

/// `canvas` read back on the GPU at `density`, premultiplied.
fn read_at(canvas: &mut Canvas, density: u32) -> Vec<u8> {
    canvas.set_gpu(true);
    canvas
        .to_image_data(
            None,
            &ImageDataExport {
                density,
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

/// The largest difference between a scene read at `then` on a canvas that
/// was read at `first` before, and the scene read at `then` on its own.
fn after_another_scale(
    scene: &dyn Fn() -> Canvas,
    first: u32,
    then: u32,
) -> u8 {
    let fresh = read_at(&mut scene(), then);
    let mut canvas = scene();
    read_at(&mut canvas, first);
    let after = read_at(&mut canvas, then);
    fresh
        .iter()
        .zip(&after)
        .map(|(a, b)| a.abs_diff(*b))
        .max()
        .unwrap_or(0)
}

#[test]
fn text_after_a_larger_draw_of_it_draws_as_a_fresh_draw() {
    if !has_gpu() {
        return;
    }
    register("HistoryOswald", OSWALD);
    // 96px under a 1.7 scale: 163 device pixels at density 1, inside the
    // range Ganesh drew as distance fields, and 490 -- a path -- at 3.
    let scene = || {
        let mut canvas = Canvas::new(400.0, 200.0);
        let ctx = canvas.context();
        ctx.scale(1.7, 1.7);
        assert!(ctx.set_font_css("96px HistoryOswald"));
        ctx.set_filter_css("drop-shadow(0 0 3px black)")
            .expect("a filter");
        ctx.fill_text("fj Wq", 4.0, 96.0, None);
        canvas
    };
    let worst = [(3, 1), (2, 1), (3, 2)]
        .map(|(first, then)| after_another_scale(&scene, first, then));
    if cfg!(meo_text_distance_fields_off) {
        assert_eq!(worst, [0, 0, 0]);
    } else {
        assert!(
            worst.iter().any(|&w| w > 0),
            "crates.io's Skia drew the text after a larger draw of it as \
             afresh; its blob cache keeps the matrix there now"
        );
    }
}

/// This repository's builds compile the tree `scripts/skia-source.mjs`
/// prepared, which carries the distance-field patch on every platform, and
/// name it in `SKIA_SOURCE_DIR`; a build that names no tree compiles
/// crates.io's Skia, which does not carry it, and `build.rs` must say so, or
/// the tests above would hold it to pixels it cannot draw.
#[test]
fn a_build_carries_the_text_patches_where_it_names_a_patched_tree() {
    assert_eq!(
        cfg!(meo_text_distance_fields_off),
        option_env!("SKIA_SOURCE_DIR").is_some(),
        "cfg(meo_text_distance_fields_off) against SKIA_SOURCE_DIR {:?}",
        option_env!("SKIA_SOURCE_DIR"),
    );
}
