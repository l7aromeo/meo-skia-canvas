use std::{
    env, fs,
    path::{Path, PathBuf},
};

#[path = "build/brotli.rs"]
mod brotli;
#[path = "build/skia_mode.rs"]
mod skia_mode;

use brotli::{Plan, VENDORED_BROTLI, VENDORED_FILES, left_out, plan};
use skia_mode::{Inputs, Mode, mode, parse_manifest};

/// Where `vendor/brotli` is, under this crate's root.
const VENDOR_BROTLI: &str = "vendor/brotli";

fn main() {
    // On Linux, compile the BDF stub to satisfy fontconfig's
    // FT_Get_BDF_Property calls. Skia embeds its own FreeType without BDF
    // support, and linking a full FreeType with BDF causes duplicate symbol
    // conflicts. This stub returns "not found" for BDF queries - we don't
    // use legacy bitmap fonts anyway.
    #[cfg(target_os = "linux")]
    {
        cc::Build::new().file("src/bdf_stub.c").compile("bdf_stub");
    }

    let skia = skia_mode_of_build();
    if let Mode::Refused(why) = &skia {
        // A build that asked for this repository's archives and would get
        // other Skia without being told: see `build/skia_mode.rs`.
        panic!("{why}");
    }
    if skia != Mode::Docs && !matches!(skia, Mode::Binaries { .. }) {
        brotli_command_table();
    }
    graphite_bindings();
    skia_patches(&skia);
}

/// Which Skia this build links: see `build/skia_mode.rs`. A build that sets
/// `SKIA_BINARIES_URL` to the URL `skia-binaries.txt` names, on a target it
/// hosts, links this repository's patched Skia from the archives; one that
/// sets nothing links crates.io's, as every crate build did before them.
fn skia_mode_of_build() -> Mode {
    for name in ["SKIA_BINARIES_URL", "DOCS_RS", "SKIA_SOURCE_DIR"] {
        println!("cargo:rerun-if-env-changed={name}");
    }
    let manifest_dir =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap_or_default());
    let path = manifest_dir.join("skia-binaries.txt");
    println!("cargo:rerun-if-changed={}", path.display());
    let manifest = fs::read_to_string(&path)
        .map_err(|error| error.to_string())
        .and_then(|text| parse_manifest(&text))
        .unwrap_or_else(|why| {
            // A package without it was not built by this repository's release.
            panic!("meo-skia-canvas: {} cannot be read: {why}", path.display())
        });
    let source_dir = env::var("SKIA_SOURCE_DIR").ok();
    let binaries_url = env::var("SKIA_BINARIES_URL").ok();
    let target = env::var("TARGET").unwrap_or_default();
    mode(
        &Inputs {
            docs_rs: env::var_os("DOCS_RS").is_some(),
            source_dir: source_dir.as_deref(),
            local: patched_skia_bindings(&manifest_dir).is_some(),
            binaries_url: binaries_url.as_deref(),
            patched_skia: env::var_os("CARGO_FEATURE_PATCHED_SKIA").is_some(),
            target: &target,
        },
        &manifest,
    )
}

/// One of this repository's patches to Skia, as a build can tell whether the
/// Skia source it compiled carries it.
struct SkiaPatch {
    /// The cfg turned on where it does.
    cfg: &'static str,
    /// The file the patch changes, under Skia's source root.
    file: &'static str,
    /// What only that file as the patch leaves it holds.
    marker: &'static str,
}

/// The patches whose effect a test holds a build to, each with the cfg that
/// says whether the build has it. `scripts/check-patched-skia.mjs` reads the
/// file and marker of each from here rather than keeping its own, so the
/// check a release build runs and the cfg cannot disagree, and its self-test
/// holds each marker to its patch.
const SKIA_PATCHES: [SkiaPatch; 7] = [
    // `containers/skia-atlas-size.patch`: Ganesh's path atlas capped at 512 by
    // 512, where an atlas row of 512 or more can move a path's coverage.
    SkiaPatch {
        cfg: "meo_atlas_size",
        file: "src/gpu/ganesh/ops/AtlasPathRenderer.cpp",
        marker: "fAtlasMaxSize = 512;",
    },
    // `containers/skia-triangulation-tolerance.patch`: a cached triangulation
    // reused only at the tolerance it was cut for.
    SkiaPatch {
        cfg: "meo_triangulation_tolerance",
        file: "src/gpu/ganesh/ops/TriangulatingPathRenderer.cpp",
        marker: "info->fTolerance == tol",
    },
    // `containers/skia-async-transfer-reuse.patch`: the asynchronous read's
    // transfer buffer made `kDynamic`, which Skia caches, where crates.io's
    // Skia makes it `kStream`, which Skia frees once the read lets go. Scoped
    // by the enum's name because the unpatched file already holds a bare
    // `kDynamic_GrAccessPattern`, in the comment the patch replaces.
    SkiaPatch {
        cfg: "meo_async_transfer_reuse",
        file: "src/gpu/ganesh/SurfaceContext.cpp",
        marker: "GrAccessPattern::kDynamic_GrAccessPattern",
    },
    // `containers/skia-text-distance-fields-off.patch`: Ganesh draws text as
    // distance fields only on a surface that asks for device-independent
    // fonts, which none of this crate's do.
    SkiaPatch {
        cfg: "meo_text_distance_fields_off",
        file: "src/gpu/ganesh/GrRecordingContextPriv.cpp",
        marker: "supportsDistanceFieldText() &&",
    },
    // `containers/skia-transformed-mask-reuse.patch`: a cached transformed
    // glyph mask reused only at the scale, rotation and skew it was made for.
    SkiaPatch {
        cfg: "meo_transformed_mask_reuse",
        file: "src/text/gpu/SubRunContainer.cpp",
        marker: "fMadeFor",
    },
    // `containers/skia-exact-blend-division.patch`: the CPU raster's
    // colour-dodge, colour-burn, hue, saturation, color and luminosity stages
    // divide with `/` rather than through a SIMD reciprocal estimate.
    SkiaPatch {
        cfg: "meo_exact_blend_division",
        file: "src/opts/SkRasterPipeline_opts.h",
        marker: "(d*sa) / (sa - s)",
    },
    // `containers/skia-patch-marker.patch`: no fix, one function a crate
    // build that names the patched archives links against, so a build that
    // fell back to crates.io's Skia fails to link -- see
    // `src/patched_skia.rs`.
    SkiaPatch {
        cfg: "meo_skia_marker",
        file: "src/core/SkGraphics.cpp",
        marker: "meo_skia_canvas_needs_its_patched_skia_see_SKIA_BINARIES_URL",
    },
];

/// Turns on each of [`SKIA_PATCHES`]'s cfgs this build's Skia carries.
///
/// From the archives, `skia-binaries.txt`'s list for the target, which the
/// release generates from the same patch list the archives are built from.
/// Otherwise each one whose marker the Skia source this build compiled holds:
/// this repository's own builds compile the patched source, and a crate build
/// that names no archives compiles crates.io's, or links rust-skia's prebuilt
/// archive, which is built from the same unpatched source; it names no tree
/// for [`skia_tree`] to return, and gets none of them. Only tests read the
/// cfgs: the crate draws and reads through Skia the same way on all of them,
/// and what differs is what Skia does underneath.
fn skia_patches(skia: &Mode) {
    for patch in &SKIA_PATCHES {
        println!("cargo:rustc-check-cfg=cfg({})", patch.cfg);
    }
    if let Mode::Binaries { tree, cfgs } = skia {
        for cfg in cfgs {
            println!("cargo:rustc-cfg={cfg}");
        }
        if cfgs.iter().any(|cfg| cfg == SKIA_MARKER_CFG) {
            println!("cargo:rustc-env=MEO_SKIA_TREE={tree}");
        }
        return;
    }
    println!("cargo:rerun-if-env-changed=SKIA_SOURCE_DIR");
    let Some(skia) = skia_tree() else {
        return;
    };
    for patch in &SKIA_PATCHES {
        let file = skia.join(patch.file);
        // Named only where it exists: Cargo reruns a script on every build
        // for a path that does not.
        let Ok(source) = fs::read_to_string(&file) else {
            continue;
        };
        println!("cargo:rerun-if-changed={}", file.display());
        if source.contains(patch.marker) {
            println!("cargo:rustc-cfg={}", patch.cfg);
            if patch.cfg == SKIA_MARKER_CFG {
                println!(
                    "cargo:rustc-env=MEO_SKIA_TREE={}",
                    source_tree_name(&skia)
                );
            }
        }
    }
}

/// The cfg of `containers/skia-patch-marker.patch`, under which the crate
/// links the marker and `skia_patches` names the tree.
const SKIA_MARKER_CFG: &str = "meo_skia_marker";

/// The name of the patched tree `skia` lies in -- the
/// `skia-<skia-bindings version>-<patch digest>` directory
/// `scripts/skia-source.mjs` prepares, above rust-skia's checkout -- or
/// `local` for a rust-skia checkout of its own (`just use-local-skia`).
fn source_tree_name(skia: &Path) -> String {
    skia_mode::tree_name(
        skia.ancestors().filter_map(|dir| dir.file_name()?.to_str()),
    )
    .unwrap_or("local")
    .to_owned()
}

/// What this script prints where the patched Graphite bindings are in the
/// build. The npm build's CI step looks for it, so that a gate which never
/// turns on cannot pass for one that is off by design.
const GRAPHITE_BINDINGS_NOTICE: &str =
    "meo_graphite: the patched Graphite bindings are in this build";

/// Turns on `cfg(meo_graphite)` where the skia-bindings in the build carries
/// the Graphite bindings `containers/rust-skia-graphite-bindings.patch` adds.
///
/// Only that patched skia-bindings prints `meo_graphite_bindings`, which Cargo
/// hands this script as `DEP_SKIA_MEO_GRAPHITE_BINDINGS` through its
/// `links = "skia"`. A build against crates.io's skia-bindings never sees it,
/// so the code calling those bindings is never compiled there, and no feature
/// a dependent can turn on reaches it.
fn graphite_bindings() {
    println!("cargo:rustc-check-cfg=cfg(meo_graphite)");
    if env::var_os("DEP_SKIA_MEO_GRAPHITE_BINDINGS").is_some() {
        println!("cargo:rustc-cfg=meo_graphite");
        println!("cargo:warning={GRAPHITE_BINDINGS_NOTICE}");
    }
}

// UPSTREAM: skia-bindings 0.156.0 -- unfiled -- worked around
// Re-check: link a Linux build from crates.io's Skia with this function
// returning at once; it fails on undefined `kCmdLut` and
// `BrotliDecoderEnsureStaticInit`
//
/// Compiles the brotli decoder sources Skia's build leaves out, from
/// `vendor/brotli`: see `build/brotli.rs` for why they are missing and why
/// they come from this crate rather than the Skia tree skia-bindings syncs.
///
/// Where the build names a Skia tree, `vendor/brotli` is held to the brotli
/// in it first, on every target, so that every build of this repository
/// checks the vendored copy against the source it stands in for. Only the
/// sources that tree's `BUILD.gn` leaves out are compiled: a patched tree --
/// this repository's own builds, through `containers/skia-brotli-sources.patch`
/// -- or an upstream fix compiles them inside Skia, and a second definition
/// would fail the link the other way.
fn brotli_command_table() {
    println!("cargo:rerun-if-env-changed=SKIA_SOURCE_DIR");
    println!("cargo:rerun-if-env-changed=DOCS_RS");
    let tree = skia_tree();
    let tree_text = tree.as_ref().map(|tree| tree.to_string_lossy());
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let docs_rs = env::var_os("DOCS_RS").is_some();
    if let Some(tree) = &tree
        && !docs_rs
    {
        hold_vendored_brotli_to(tree);
    }
    let sources = match plan(&brotli::Inputs {
        target_os: &target_os,
        docs_rs,
        tree: tree_text.as_deref(),
    }) {
        Plan::Nothing => return,
        Plan::Vendored => brotli::MISSING_SOURCES.to_vec(),
        Plan::Tree(tree) => {
            let build_gn = Path::new(tree).join("third_party/brotli/BUILD.gn");
            println!("cargo:rerun-if-changed={}", build_gn.display());
            let gn = fs::read_to_string(&build_gn).unwrap_or_else(|error| {
                panic!(
                    "meo-skia-canvas: cannot read {} ({error}), which says which brotli \
                     sources the Skia tree this build names compiles",
                    build_gn.display()
                )
            });
            left_out(&gn)
        }
    };
    if sources.is_empty() {
        return;
    }
    let vendor = crate_root().join(VENDOR_BROTLI);
    for file in VENDORED_FILES {
        println!("cargo:rerun-if-changed={}", vendor.join(file).display());
    }
    cc::Build::new()
        .files(sources.iter().map(|source| vendor.join(source)))
        .include(vendor.join("c/include"))
        // Nothing in this crate refers to them, so a linker that reads
        // archives in order would otherwise leave them out before Skia asks.
        .link_lib_modifier("+whole-archive")
        .compile("brotli_command_table");
}

/// Refuses a build whose `vendor/brotli` differs from the brotli in `tree`,
/// the Skia source it names: the command table has to match the decoder
/// Skia compiled. A tree with no brotli synced has nothing to compare.
fn hold_vendored_brotli_to(tree: &Path) {
    let synced = tree.join("third_party/externals/brotli");
    if !synced.is_dir() {
        return;
    }
    let vendor = crate_root().join(VENDOR_BROTLI);
    let differ: Vec<&str> = VENDORED_FILES
        .into_iter()
        .filter(|file| {
            fs::read(vendor.join(file)).ok() != fs::read(synced.join(file)).ok()
        })
        .collect();
    if !differ.is_empty() {
        panic!(
            "meo-skia-canvas: {VENDOR_BROTLI} is brotli at {VENDORED_BROTLI}, and {} differs from \
             it in {}. Copy those files from that brotli into {VENDOR_BROTLI} and set \
             `VENDORED_BROTLI` in build/brotli.rs to the commit Skia's DEPS pins.",
            synced.display(),
            differ.join(", ")
        );
    }
}

/// This crate's root.
fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The Skia source tree the skia-bindings in this build compiled, where the
/// build names one: `SKIA_SOURCE_DIR` when it is set and not empty, as
/// skia-bindings itself reads it, or else the `skia` directory of a rust-skia
/// checkout this crate's Cargo.toml patches skia-bindings to. A build from
/// crates.io names neither, and its tree is not looked for: it lives in the
/// cargo registry's extraction of skia-bindings, which a restored build cache
/// can leave without it.
fn skia_tree() -> Option<PathBuf> {
    let source_dir = env::var("SKIA_SOURCE_DIR").ok();
    if let Some(dir) = brotli::source_dir(source_dir.as_deref()) {
        return Some(PathBuf::from(dir));
    }
    patched_skia_bindings(&crate_root()).map(|checkout| checkout.join("skia"))
}

/// The rust-skia `skia-bindings` directory this crate's Cargo.toml patches
/// skia-bindings to, as `just use-local-skia` writes it.
fn patched_skia_bindings(manifest_dir: &Path) -> Option<PathBuf> {
    let manifest = fs::read_to_string(manifest_dir.join("Cargo.toml")).ok()?;
    let line = manifest.lines().find(|line| {
        line.trim_start().starts_with("skia-bindings") && line.contains("path")
    })?;
    let path = line.split('"').nth(1)?;
    Some(manifest_dir.join(path))
}
