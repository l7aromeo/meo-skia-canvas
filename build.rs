use std::{
    env, fs,
    path::{Path, PathBuf},
};

/// The brotli decoder sources Skia's build list leaves out since M156,
/// relative to Skia's `third_party/externals/brotli`: `prefix.c` defines the
/// command table `kCmdLut`, and `static_init.c` defines
/// `BrotliDecoderEnsureStaticInit`, both of which `decode.c` calls for.
const BROTLI_MISSING_SOURCES: [&str; 2] =
    ["c/dec/prefix.c", "c/dec/static_init.c"];

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

    brotli_command_table();
}

// UPSTREAM: skia-bindings 0.156.0 -- unfiled -- worked around
// Re-check: link a Linux build with `freetype` from an unpatched Skia tree with
// this function returning at once; it fails on undefined `kCmdLut` and
// `BrotliDecoderEnsureStaticInit`
//
// Skia M156 rolls brotli to cfa184a, which moves the decoder's command table,
// `kCmdLut`, into a new `c/dec/prefix.c` and its static initialisation into a
// new `c/dec/static_init.c`, and Skia's `third_party/brotli/BUILD.gn` lists
// neither, so `decode.c`'s references to both are left undefined. Only
// `freetype-woff2` links brotli -- `freetype` turns it on, and skia-bindings
// drops it on macOS -- so the link fails on Linux alone, for every build of
// skia-bindings from source, which is every Linux build with `freetype`: no
// prebuilt archive carries that feature set.
//
// The files are compiled here from the brotli source the Skia build used,
// never from a copy of our own, and only those that build's `BUILD.gn` leaves
// out: a patched tree -- this repository's own builds, through
// `containers/skia-brotli-sources.patch` -- or an upstream fix compiles them
// inside Skia, and a second definition would fail the link the other way.
fn brotli_command_table() {
    println!("cargo:rerun-if-env-changed=SKIA_SOURCE_DIR");
    println!("cargo:rerun-if-env-changed=CARGO_HOME");
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "linux" || env::var_os("CARGO_FEATURE_FREETYPE").is_none() {
        return;
    }
    let skia = skia_source().unwrap_or_else(|reason| {
        panic!(
            "meo-skia-canvas: cannot find the Skia source skia-bindings built from ({reason}). \
             With `freetype` on Linux this build compiles brotli's decoder sources from \
             that source, which Skia's own build leaves out. Set SKIA_SOURCE_DIR to the \
             skia-bindings package's `skia` directory to name it."
        )
    });
    let build_gn = skia.join("third_party/brotli/BUILD.gn");
    println!("cargo:rerun-if-changed={}", build_gn.display());
    let gn = fs::read_to_string(&build_gn).unwrap_or_default();
    let brotli = skia.join("third_party/externals/brotli");
    let missing: Vec<PathBuf> = BROTLI_MISSING_SOURCES
        .iter()
        .filter(|source| !gn.contains(&format!("externals/brotli/{source}")))
        .map(|source| brotli.join(source))
        .filter(|source| source.is_file())
        .collect();
    if missing.is_empty() {
        return;
    }
    cc::Build::new()
        .files(&missing)
        .include(brotli.join("c/include"))
        // Nothing in this crate refers to them, so a linker that reads
        // archives in order would otherwise leave them out before Skia asks.
        .link_lib_modifier("+whole-archive")
        .compile("brotli_command_table");
}

/// The Skia source tree the skia-bindings in this build compiled, or why it
/// cannot be named.
///
/// `SKIA_SOURCE_DIR` when it is set, as skia-bindings itself reads it.
/// Otherwise the skia-bindings this build links: `links = "skia"` allows one
/// in a build graph, and the build's own Cargo.lock says which version -- the
/// first that names it above `OUT_DIR`, where a workspace's target directory
/// is by default, or else above this crate's manifest, which is the
/// workspace itself when this repository is built. A full build from crates.io
/// fetches Skia and syncs its DEPS into that package's own `skia` directory;
/// a checkout named by a `[patch]` path in this crate's Cargo.toml builds its
/// own. Anything else is refused rather than guessed at: a wrong tree would
/// compile a table that does not match the decoder it is linked with.
fn skia_source() -> Result<PathBuf, String> {
    if let Some(dir) = env::var_os("SKIA_SOURCE_DIR") {
        return Ok(PathBuf::from(dir));
    }
    let manifest_dir = PathBuf::from(
        env::var_os("CARGO_MANIFEST_DIR").ok_or("no CARGO_MANIFEST_DIR")?,
    );
    if let Some(checkout) = patched_skia_bindings(&manifest_dir) {
        return Ok(checkout.join("skia"));
    }
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").ok_or("no OUT_DIR")?);
    let version = out_dir
        .ancestors()
        .chain(manifest_dir.ancestors())
        .filter_map(|dir| fs::read_to_string(dir.join("Cargo.lock")).ok())
        .find_map(|lock| locked_skia_bindings(&lock))
        .ok_or(
            "no Cargo.lock above OUT_DIR or the manifest names skia-bindings",
        )?;
    let cargo_home = env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            env::var_os("HOME").map(|home| Path::new(&home).join(".cargo"))
        })
        .ok_or("neither CARGO_HOME nor HOME is set")?;
    let package = format!("skia-bindings-{version}");
    let trees: Vec<PathBuf> = fs::read_dir(cargo_home.join("registry/src"))
        .map_err(|error| format!("cannot list the registry: {error}"))?
        .flatten()
        .map(|registry| registry.path().join(&package).join("skia"))
        .filter(|skia| skia.join("third_party/externals/brotli").is_dir())
        .collect();
    match trees.as_slice() {
        [tree] => Ok(tree.clone()),
        [] => Err(format!("no registry copy of {package} holds a synced Skia")),
        _ => Err(format!(
            "{} registry copies of {package} hold a synced Skia",
            trees.len()
        )),
    }
}

/// The skia-bindings version a Cargo.lock resolves, when it names one.
fn locked_skia_bindings(lock: &str) -> Option<String> {
    let mut lines = lock.lines();
    while let Some(line) = lines.next() {
        if line == "name = \"skia-bindings\"" {
            let version = lines
                .next()?
                .strip_prefix("version = \"")?
                .strip_suffix('"')?;
            return Some(version.to_owned());
        }
    }
    None
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
