//! A GPU canvas draws text the same whatever the context drew before.
//!
//! Two of Ganesh's text routes made a draw's pixels depend on what came
//! before it, in Skia M156 under the skia-bindings crate:
//!
//! - A glyph run from 162 to 324 device pixels was drawn as distance fields
//!   (`SubRunControl`, src/text/gpu/SubRunControl.cpp), whose coverage moves by
//!   a level or two with where in the atlas a glyph was placed -- so the same
//!   `fillText` exported after other text, on this thread or another, read
//!   differently from one exported first. The blob cache also keys such a run
//!   without its matrix (`TextBlob::Key::Make`), so a path blob cached at a
//!   larger scale was drawn in place of a smaller draw's distance fields.
//!   `containers/skia-text-distance-fields-off.patch` turns distance fields off
//!   where a surface does not ask for them, which none of this crate's do: a
//!   run under 254 device pixels is then a direct mask from the scaler the CPU
//!   raster uses, the same bytes as the CPU page.
//! - A glyph with no outline -- a colour bitmap such as an emoji -- at 254
//!   device pixels or more is a transformed mask made for one matrix, and a
//!   cached blob of them was drawn at any other scale.
//!   `containers/skia-transformed-mask-reuse.patch` reuses one only at the
//!   matrix it was made for. Without it, a run whose blobs this library builds
//!   is drawn on a GPU canvas from blobs Ganesh keys by their matrix
//!   (`KEYS_OWN_COLOUR_BLOBS` in src/node/typography.rs), and a paragraph,
//!   whose blobs SkParagraph builds, is not.
//!
//! A crate build's Skia carries neither, and
//! `cfg(meo_text_distance_fields_off)` and `cfg(meo_transformed_mask_reuse)`,
//! from `build.rs`, say which this build compiled. Where a patch is missing,
//! the cross-scale tests hold the build to the defect instead, so the day
//! crates.io's Skia stops showing it, the test says so: the blob cache's keying
//! is Skia's own logic, the same on every GPU. Whether a distance-field glyph's
//! coverage moves with its place in the atlas depends on how the GPU samples
//! it -- it was measured on Apple's Metal, and Mesa's software Vulkan drew
//! every history alike -- so without the patch the history test holds only that
//! each history draws the same page every time.

// UPSTREAM: skia-safe 0.156.0 -- unfiled -- not worked around
// Re-check: cargo test text_after_a_larger_draw_of_it_draws_as_a_fresh_draw
// on a build of crates.io's Skia, which holds it to the defect; when it fails
// there, Ganesh no longer draws these runs as distance fields, or its blob
// cache keys them with their matrix, and which one decides whether the marker
// goes. The crate cannot turn them off through `GrContextOptions` without
// moving other text. Ganesh hands `fGlyphsAsPathsFontSize` to `SubRunControl`
// as the largest distance-field size
// (src/gpu/ganesh/GrRecordingContextPriv.cpp, `getSubRunControl`), and
// src/text/gpu/SubRunContainer.cpp gates direct masks by the same value,
// `approximateDeviceTextSize < maxMaskSize` at line 1455, so lowering it below
// 162 to empty the distance-field range draws every run from there to 254
// device pixels as paths instead of the direct masks the patch gives them.
// `fMinDistanceFieldFontSize` cannot raise the range's floor:
// `SubRunControl::MinSDFTRange` (src/text/gpu/SubRunControl.cpp) ignores it,
// and sets 162, unless the surface asks for distance fields at small sizes. And
// a paint the distance-field route refuses but the mask route takes, a stroke
// and fill of no width, gives the scaler a frame width, so each mask is made
// from the glyph's stroked outline (`fGenerateImageFromPath` in
// src/core/SkScalerContext.cpp) and is not the mask the CPU page draws.

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

/// A bundled colour bitmap face, whose glyphs are PNG images in `sbix`, as
/// Apple Color Emoji's are.
const SBIX: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/assets/fonts/bitmap-colour/sbix.ttf"
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
    // The page after `history`, drawn in a process of its own: its hash, and
    // whether it is the CPU page.
    let page = |history: &str| {
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
        (hash.to_string(), as_cpu == "true")
    };
    let pages: Vec<(&str, String, bool)> = HISTORIES
        .iter()
        .map(|&history| {
            let (hash, as_cpu) = page(history);
            (history, hash, as_cpu)
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
        // Without the patch, a page may differ from one history to another or
        // not, as the GPU samples a distance field; what holds on every GPU is
        // that the same history draws the same page.
        let unsteady: Vec<&str> = pages
            .iter()
            .filter(|(history, hash, _)| page(history).0 != *hash)
            .map(|(history, _, _)| *history)
            .collect();
        assert!(
            unsteady.is_empty(),
            "a history drew another page the second time: {unsteady:?}"
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

/// What a scene sets on the context beside its text, answering the width
/// to condense the text to where it has one.
type Style<'a> = &'a dyn Fn(&mut Context2D) -> Option<f32>;

/// Two colour bitmap glyphs, set in [`SBIX`] at `px` under a `scale` and
/// whatever `style` sets beside, onto `ctx`, condensed to the width `style`
/// answers where it answers one.
fn colour_bitmaps(ctx: &mut Context2D, px: f32, scale: f32, style: Style) {
    ctx.scale(scale, scale);
    ctx.set_font(&Font::new("HistorySbix", px));
    let width = style(ctx);
    ctx.fill_text(
        "\u{1F600}\u{2662}",
        4.0,
        (200.0 / scale - 4.0).min(px),
        width,
    );
}

#[test]
fn colour_bitmaps_after_a_larger_draw_draw_afresh() {
    if !has_gpu() {
        return;
    }
    register("HistorySbix", SBIX);
    let library = FontLibrary::new();
    library
        .register_font_from_data(
            "ParagraphSbix",
            &std::fs::read(SBIX).expect("a bundled face"),
        )
        .expect("a bundled face registers");
    let plain = |_: &mut Context2D| None;
    let shadowed = |ctx: &mut Context2D| {
        ctx.set_filter_css("drop-shadow(0 0 3px black)")
            .expect("a filter");
        None
    };
    let blurred = |ctx: &mut Context2D| {
        ctx.set_filter_css("blur(2px)").expect("a filter");
        None
    };
    let multiplied = |ctx: &mut Context2D| {
        ctx.set_global_composite_operation(BlendMode::Multiply);
        None
    };
    // Narrower than the two glyphs at either size, so the run is squeezed.
    let condensed = |_: &mut Context2D| Some(120.0);
    let styles: [(&str, Style); 5] = [
        ("plain", &plain),
        ("drop-shadow", &shadowed),
        ("blur", &blurred),
        ("multiply", &multiplied),
        ("condensed", &condensed),
    ];
    // From 200px under 1.7 and 400px under 1: 340 and 400 device pixels at
    // density 1, past the 254 where a bitmap glyph becomes a transformed
    // mask, at every density read. Each check sets its run a pixel larger
    // than the last, so that its fresh draw is the run's first, drawn from
    // blobs of its own: Ganesh keeps a blob's masks for each scale it has
    // drawn it at, and one fresh draw sharing the history's blobs puts the
    // mask for its scale there first, which hides the defect.
    let mut apart = Vec::new();
    let mut size = 0.0;
    for (px, scale) in [(200.0, 1.7), (400.0, 1.0)] {
        for (name, style) in styles {
            for route in ROUTES {
                for (first, then) in [(3, 1), (1, 3)] {
                    let px = px + size;
                    size += 1.0;
                    let on_page = || {
                        let mut canvas = Canvas::new(400.0, 200.0);
                        colour_bitmaps(canvas.context(), px, scale, style);
                        canvas
                    };
                    let scene = || match route {
                        "page" => on_page(),
                        "drawCanvas" => {
                            let mut source = on_page();
                            let mut canvas = Canvas::new(400.0, 200.0);
                            canvas.context().draw_canvas(&mut source, 0.0, 0.0);
                            canvas
                        }
                        "pattern" => {
                            let mut source = on_page();
                            let mut canvas = Canvas::new(400.0, 200.0);
                            let ctx = canvas.context();
                            let pattern = ctx.create_pattern_from_canvas(
                                &mut source,
                                PatternRepeat::NoRepeat,
                            );
                            ctx.set_fill_pattern(&pattern);
                            ctx.fill_rect(0.0, 0.0, 400.0, 200.0);
                            canvas
                        }
                        _ => {
                            let mut canvas = Canvas::new(400.0, 200.0);
                            let ctx = canvas.context();
                            // A paragraph has no `maxWidth`: condensed, it
                            // is drawn as it is plain.
                            style(ctx);
                            let layout = sbix_paragraph(&library, px);
                            ctx.scale(scale, scale);
                            ctx.draw_paragraph(&layout, 4.0, 0.0);
                            canvas
                        }
                    };
                    let fresh = read_at(&mut scene(), then);
                    assert!(
                        fresh.iter().any(|&b| b != 0),
                        "{name} {route} at {px}px drew nothing"
                    );
                    let mut canvas = scene();
                    read_at(&mut canvas, first);
                    let worst = fresh
                        .iter()
                        .zip(&read_at(&mut canvas, then))
                        .map(|(a, b)| a.abs_diff(*b))
                        .max()
                        .unwrap_or(0);
                    if worst > 0 {
                        apart.push((px, name, route, first, then, worst));
                    }
                }
            }
        }
    }
    if cfg!(meo_transformed_mask_reuse) {
        assert!(
            apart.is_empty(),
            "drawn apart after another scale: {apart:?}"
        );
    } else {
        // Without the patch, a run this library builds the blobs of is drawn
        // from blobs keyed by their matrix, and a paragraph, whose blobs
        // SkParagraph builds, is not -- see `KEYS_OWN_COLOUR_BLOBS` in
        // src/node/typography.rs.
        let (paragraphs, others): (Vec<_>, Vec<_>) = apart
            .into_iter()
            .partition(|&(_, _, route, ..)| route == "paragraph");
        assert!(
            others.is_empty(),
            "drawn apart after another scale: {others:?}"
        );
        assert!(
            !paragraphs.is_empty(),
            "crates.io's Skia drew a paragraph's colour bitmaps after a larger \
             draw as afresh; it reuses a transformed mask only at its matrix \
             there now"
        );
    }
}

/// The routes [`colour_bitmaps`]' runs are checked on: drawn on the page,
/// drawn onto another canvas, filled as a canvas pattern, and laid out by
/// the Paragraph API -- see [`sbix_paragraph`].
const ROUTES: [&str; 4] = ["page", "drawCanvas", "pattern", "paragraph"];

/// [`colour_bitmaps`]' two glyphs laid out by the Paragraph API at `px`, in
/// [`SBIX`] registered in `library` as `ParagraphSbix`.
///
/// One library for every scene, as one face: on a GPU canvas, a paragraph in
/// one registration of [`SBIX`] read a level apart from the same paragraph
/// in another, with nothing drawn before either, at sizes drawn from
/// transformed masks -- not at one drawn from direct masks, nor on the CPU.
///
/// Checked to have resolved that face, by its width against `fillText`'s:
/// a paragraph that fell back to another face would be checked in it.
fn sbix_paragraph(library: &FontLibrary, px: f32) -> Paragraph {
    let style = TextStyle {
        font_families: vec!["ParagraphSbix".into()],
        font_size: px,
        color: RgbaLinear::from_srgb8(0, 0, 0, 1.0),
        ..TextStyle::default()
    };
    let engine = TextEngine::new(library);
    let mut builder = engine.paragraph_builder(&style);
    builder.add_text("\u{1F600}\u{2662}");
    let layout = builder.build(f32::INFINITY);
    let mut probe = Canvas::new(1.0, 1.0);
    let ctx = probe.context();
    ctx.set_font(&Font::new("HistorySbix", px));
    let set = ctx.measure_text("\u{1F600}\u{2662}", None).width;
    assert!(
        (layout.max_intrinsic_width() - set).abs() < 0.5,
        "the paragraph set {} px wide, fillText {set}",
        layout.max_intrinsic_width()
    );
    layout
}

/// This repository's builds compile the tree `scripts/skia-source.mjs`
/// prepared, which carries both patches on every platform, and name it in
/// `SKIA_SOURCE_DIR`. A build that took the patched archives names no tree,
/// and `build.rs` sets `MEO_SKIA_TREE` for it instead. A build that does
/// neither compiles crates.io's Skia, which carries neither patch, and
/// `build.rs` must say so, or the tests above would hold it to pixels it
/// cannot draw.
#[test]
fn a_build_carries_the_text_patches_where_it_names_a_patched_tree() {
    let patched = option_env!("SKIA_SOURCE_DIR").is_some()
        || option_env!("MEO_SKIA_TREE").is_some();
    assert_eq!(
        (
            cfg!(meo_text_distance_fields_off),
            cfg!(meo_transformed_mask_reuse)
        ),
        (patched, patched),
        "the text patches' cfgs against SKIA_SOURCE_DIR {:?} and MEO_SKIA_TREE {:?}",
        option_env!("SKIA_SOURCE_DIR"),
        option_env!("MEO_SKIA_TREE"),
    );
}
