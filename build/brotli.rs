//! Which brotli decoder sources a build of this crate compiles beside Skia,
//! decided from its target and the Skia source tree it names, if any.
//!
//! Skia M156 rolls brotli to cfa184a, which moves the decoder's command table,
//! `kCmdLut`, into a new `c/dec/prefix.c` and its static initialisation into a
//! new `c/dec/static_init.c`, and Skia's `third_party/brotli/BUILD.gn` lists
//! neither, so `decode.c`'s references to both are left undefined. Only
//! `freetype-woff2` links brotli, and Cargo.toml turns it on for Linux alone,
//! so the link fails there for every build of skia-bindings from crates.io's
//! source.
//!
//! The two files, and the headers they include, are in `vendor/brotli` at the
//! commit that Skia pins ([`VENDORED_BROTLI`]). They come from this crate
//! rather than from the Skia tree skia-bindings syncs, because that tree lives
//! in the cargo registry's extraction of skia-bindings, which a build cache
//! restored over a fresh registry does not have: skia-bindings' fingerprint is
//! unchanged, so its build script does not run again to sync it.
//!
//! Included by `build.rs`, and compiled on its own by `just check-build-brotli`
//! to run the tests below with no Skia in sight: everything here is a pure
//! function of what it is given.

/// The brotli commit `vendor/brotli` is taken from: what the `DEPS` of
/// skia-bindings 0.156.0's Skia pins `third_party/externals/brotli` to. Fixed,
/// not chosen: the table has to match the decoder Skia compiled.
pub const VENDORED_BROTLI: &str = "cfa184a997ac19454d5a3ae97f6d600aac2445a6";

/// The brotli decoder sources Skia's build list leaves out since M156,
/// relative to brotli's root: `prefix.c` defines the command table `kCmdLut`,
/// and `static_init.c` defines `BrotliDecoderEnsureStaticInit`, both of which
/// `decode.c` calls for.
pub const MISSING_SOURCES: [&str; 2] =
    ["c/dec/prefix.c", "c/dec/static_init.c"];

/// Every file in `vendor/brotli`, relative to brotli's root: the two sources,
/// the headers they include, and the licence.
pub const VENDORED_FILES: [&str; 12] = [
    "LICENSE",
    "c/common/constants.h",
    "c/common/dictionary.h",
    "c/common/platform.h",
    "c/common/static_init.h",
    "c/dec/prefix.c",
    "c/dec/prefix.h",
    "c/dec/prefix_inc.h",
    "c/dec/static_init.c",
    "c/dec/static_init.h",
    "c/include/brotli/port.h",
    "c/include/brotli/types.h",
];

/// What a build does about the sources Skia's build leaves out.
#[derive(Debug, PartialEq, Eq)]
pub enum Plan<'a> {
    /// Nothing to compile: not Linux, or docs.rs, which links no Skia.
    Nothing,
    /// Skia was compiled from the tree at this path, `SKIA_SOURCE_DIR` or a
    /// local rust-skia checkout. Its `BUILD.gn` says which of
    /// [`MISSING_SOURCES`] it leaves out -- a patched tree lists both -- and
    /// `vendor/brotli` has to be the brotli it carries.
    Tree(&'a str),
    /// crates.io's skia-bindings 0.156.0, which Cargo.toml pins exactly:
    /// its Skia's `BUILD.gn` leaves out both.
    Vendored,
}

/// What `plan` reads from a build.
#[derive(Debug, Default)]
pub struct Inputs<'a> {
    /// `CARGO_CFG_TARGET_OS`.
    pub target_os: &'a str,
    /// `DOCS_RS` is set.
    pub docs_rs: bool,
    /// The Skia source tree the build names, when it names one.
    pub tree: Option<&'a str>,
}

/// What a build does about the brotli sources Skia's build leaves out.
pub fn plan<'a>(inputs: &Inputs<'a>) -> Plan<'a> {
    if inputs.target_os != "linux" || inputs.docs_rs {
        return Plan::Nothing;
    }
    match inputs.tree {
        Some(tree) => Plan::Tree(tree),
        None => Plan::Vendored,
    }
}

/// The tree `SKIA_SOURCE_DIR` names, which skia-bindings reads as unset when
/// it is empty.
pub fn source_dir(value: Option<&str>) -> Option<&str> {
    value.filter(|dir| !dir.is_empty())
}

/// Those of [`MISSING_SOURCES`] that `build_gn`, Skia's
/// `third_party/brotli/BUILD.gn`, does not list.
pub fn left_out(build_gn: &str) -> Vec<&'static str> {
    MISSING_SOURCES
        .into_iter()
        .filter(|source| {
            !build_gn.contains(&format!("externals/brotli/{source}"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs(target_os: &str) -> Inputs<'_> {
        Inputs {
            target_os,
            ..Inputs::default()
        }
    }

    #[test]
    fn a_linux_build_naming_no_tree_compiles_the_vendored_sources() {
        assert_eq!(plan(&inputs("linux")), Plan::Vendored);
    }

    #[test]
    fn a_linux_build_naming_a_tree_reads_it() {
        let named = Inputs {
            tree: Some("/skia"),
            ..inputs("linux")
        };
        assert_eq!(plan(&named), Plan::Tree("/skia"));
    }

    #[test]
    fn other_targets_compile_nothing() {
        for os in ["macos", "windows", "ios", "android"] {
            assert_eq!(plan(&inputs(os)), Plan::Nothing, "{os}");
        }
    }

    #[test]
    fn docs_rs_compiles_nothing_whatever_else_is_set() {
        let docs = Inputs {
            docs_rs: true,
            tree: Some("/skia"),
            ..inputs("linux")
        };
        assert_eq!(plan(&docs), Plan::Nothing);
    }

    #[test]
    fn an_empty_source_dir_names_no_tree() {
        assert_eq!(source_dir(Some("")), None);
        assert_eq!(source_dir(None), None);
        assert_eq!(source_dir(Some("/skia")), Some("/skia"));
    }

    /// `third_party/brotli/BUILD.gn`'s source list as skia-bindings 0.156.0's
    /// Skia has it.
    const STOCK_BUILD_GN: &str = r#"
  sources = [
    "../externals/brotli/c/common/constants.c",
    "../externals/brotli/c/common/context.c",
    "../externals/brotli/c/common/dictionary.c",
    "../externals/brotli/c/common/platform.c",
    "../externals/brotli/c/common/shared_dictionary.c",
    "../externals/brotli/c/common/transform.c",
    "../externals/brotli/c/dec/bit_reader.c",
    "../externals/brotli/c/dec/decode.c",
    "../externals/brotli/c/dec/huffman.c",
    "../externals/brotli/c/dec/state.c",
  ]"#;

    #[test]
    fn crates_ios_build_gn_leaves_out_both() {
        assert_eq!(left_out(STOCK_BUILD_GN), MISSING_SOURCES);
    }

    #[test]
    fn a_build_gn_listing_one_leaves_out_the_other() {
        let one = STOCK_BUILD_GN
            .replace("]", "  \"../externals/brotli/c/dec/prefix.c\",\n    ]");
        assert_eq!(left_out(&one), ["c/dec/static_init.c"]);
    }

    #[test]
    fn a_patched_build_gn_leaves_out_neither() {
        let patched = STOCK_BUILD_GN.replace(
            "]",
            "  \"../externals/brotli/c/dec/prefix.c\",\n      \"../externals/brotli/c/dec/static_init.c\",\n    ]",
        );
        assert_eq!(left_out(&patched), [] as [&str; 0]);
    }

    #[test]
    fn the_vendored_files_include_both_sources() {
        for source in MISSING_SOURCES {
            assert!(VENDORED_FILES.contains(&source), "{source}");
        }
    }

    /// Every file under `dir`, relative to it, sorted.
    fn files_under(dir: &std::path::Path) -> Vec<String> {
        let mut found = Vec::new();
        let mut pending = vec![dir.to_path_buf()];
        while let Some(next) = pending.pop() {
            for entry in std::fs::read_dir(&next).expect("readable directory") {
                let path = entry.expect("directory entry").path();
                if path.is_dir() {
                    pending.push(path);
                } else {
                    let relative = path.strip_prefix(dir).expect("under dir");
                    found.push(relative.to_string_lossy().replace('\\', "/"));
                }
            }
        }
        found.sort();
        found
    }

    #[test]
    fn vendor_brotli_holds_exactly_the_vendored_files() {
        // `file!()` is this file's path as rustc was given it, relative to
        // the repository root `just check-build-brotli` runs from.
        let vendor = std::path::Path::new(file!())
            .parent()
            .and_then(std::path::Path::parent)
            .expect("build/brotli.rs has a grandparent")
            .join("vendor/brotli");
        assert_eq!(files_under(&vendor), VENDORED_FILES);
    }
}
