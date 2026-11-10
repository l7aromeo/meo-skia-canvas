//! Which Skia a build of this crate links, decided from its environment and
//! `skia-binaries.txt`, and what that means for the build script.
//!
//! Included by `build.rs`, and compiled on its own by `just check-skia-pack`
//! to run the tests below with no Skia in sight: everything here is a pure
//! function of what it is given.

/// The Skia a build links.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    /// docs.rs, which builds no Skia.
    Docs,
    /// Compiled from the patched tree `SKIA_SOURCE_DIR` names.
    Source,
    /// Compiled by a rust-skia checkout this crate's Cargo.toml patches
    /// skia-bindings to (`just use-local-skia`).
    Local,
    /// This repository's patched Skia, downloaded by skia-bindings from the
    /// archives `SKIA_BINARIES_URL` names: the tree they are built from and
    /// the cfgs of its patches, as the manifest lists them for the target.
    Binaries {
        /// The tree, `skia-<skia-bindings version>-<patch digest>`.
        tree: String,
        /// The cfgs of the tree's patches.
        cfgs: Vec<String>,
    },
    /// crates.io's Skia, as every crate build was before the archives.
    Stock,
    /// A build that asked for this repository's archives and cannot have
    /// them as asked, with what to change.
    Refused(String),
}

/// What `mode` reads from a build's environment.
#[derive(Debug, Default)]
pub struct Inputs<'a> {
    /// `DOCS_RS` is set.
    pub docs_rs: bool,
    /// `SKIA_SOURCE_DIR`, when set and not empty.
    pub source_dir: Option<&'a str>,
    /// Cargo.toml patches skia-bindings to a local checkout.
    pub local: bool,
    /// `SKIA_BINARIES_URL`, when set.
    pub binaries_url: Option<&'a str>,
    /// This crate's `patched-skia` feature is on.
    pub patched_skia: bool,
    /// `TARGET`.
    pub target: &'a str,
}

/// `skia-binaries.txt`: the release this crate version's archives are
/// uploaded to, the URL a consumer sets to download them, and the targets
/// hosted, each with the tree its archives are built from and the cfgs of
/// that tree's patches.
#[derive(Debug, PartialEq, Eq)]
pub struct Manifest {
    /// The release's tag, `skia-binaries-<skia-bindings version>-<digest>`,
    /// the digest over every platform's tree.
    pub release: String,
    /// The value `SKIA_BINARIES_URL` takes for these archives.
    pub url: String,
    /// Each hosted target.
    pub targets: Vec<Hosted>,
}

/// A target `skia-binaries.txt` hosts archives for.
#[derive(Debug, PartialEq, Eq)]
pub struct Hosted {
    /// Its Rust target triple.
    pub triple: String,
    /// The tree its archives are built from, the one `scripts/skia-source.mjs`
    /// names for the target's platform: each platform takes patches of its
    /// own, so each has a tree of its own.
    pub tree: String,
    /// The cfgs of the tree's patches.
    pub cfgs: Vec<String>,
}

/// Reads `skia-binaries.txt`: a `release <tag>` line, a `url <template>`
/// line, and a `target <triple> <tree> <cfg>...` line for each hosted target.
/// Blank lines and lines opening `#` are skipped.
pub fn parse_manifest(text: &str) -> Result<Manifest, String> {
    let (mut release, mut url, mut targets) = (None, None, Vec::new());
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut words = line.split_whitespace();
        match (words.next(), words.next(), words.clone().next()) {
            (Some("release"), Some(tag), _) => release = Some(tag.to_owned()),
            (Some("url"), Some(template), _) => {
                url = Some(template.to_owned());
            }
            (Some("target"), Some(triple), Some(tree)) => {
                targets.push(Hosted {
                    triple: triple.to_owned(),
                    tree: tree.to_owned(),
                    cfgs: words.skip(1).map(str::to_owned).collect(),
                });
            }
            _ => return Err(format!("a line it does not read: {line:?}")),
        }
    }
    Ok(Manifest {
        release: release.ok_or("no `release` line")?,
        url: url.ok_or("no `url` line")?,
        targets,
    })
}

/// Where every tree's archives are released, the part of the manifest's URL
/// before its release tag: a `SKIA_BINARIES_URL` under it asked for this
/// repository's archives, whichever tree it names.
const RELEASES: &str = "/releases/download/";

/// The Skia a build with `inputs` links, given this crate's `manifest`.
///
/// A build with the `patched-skia` feature has asked for this repository's
/// archives, and is refused unless its URL names them. Otherwise a build that
/// sets nothing, or sets a URL that is not this repository's,
/// takes stock Skia's cfgs: what it links is crates.io's Skia, or whatever
/// that URL serves. A build that asked for this repository's archives is
/// refused where it cannot have them as asked -- its URL names another tree,
/// as a line left from an earlier version of the crate does, or its target
/// is not hosted -- since it would otherwise link Skia it did not ask for and
/// not be told.
pub fn mode(inputs: &Inputs, manifest: &Manifest) -> Mode {
    if inputs.docs_rs {
        return Mode::Docs;
    }
    if inputs.source_dir.is_some_and(|dir| !dir.is_empty()) {
        return Mode::Source;
    }
    if inputs.local {
        return Mode::Local;
    }
    let line = format!("[env]\nSKIA_BINARIES_URL = \"{}\"", manifest.url);
    let ours = inputs.binaries_url.filter(|url| {
        manifest.url.find(RELEASES).is_some_and(|end| {
            url.starts_with(&manifest.url[..end + RELEASES.len()])
        })
    });
    let Some(url) = ours else {
        // `patched-skia` asks for the archives and turns skia-bindings'
        // fallback build into a failure, so without them it would build
        // nothing at all: say what it needs.
        return match inputs.patched_skia {
            true => Mode::Refused(format!(
                "meo-skia-canvas: the `patched-skia` feature links this repository's patched \
                 Skia, which a build downloads from the URL in SKIA_BINARIES_URL. Set, in \
                 .cargo/config.toml:\n\n{line}\n"
            )),
            false => Mode::Stock,
        };
    };
    if url != manifest.url {
        return Mode::Refused(format!(
            "meo-skia-canvas: SKIA_BINARIES_URL names another version's patched Skia:\n  {url}\n\
             This version of the crate is built against {}. Set, in .cargo/config.toml:\n\n{line}\n",
            manifest.release
        ));
    }
    match manifest
        .targets
        .iter()
        .find(|hosted| hosted.triple == inputs.target)
    {
        Some(hosted) => Mode::Binaries {
            tree: hosted.tree.clone(),
            cfgs: hosted.cfgs.clone(),
        },
        None => Mode::Refused(format!(
            "meo-skia-canvas: no patched Skia is hosted for {}. The archives cover {}.\n\
             Unset SKIA_BINARIES_URL to build with crates.io's Skia, or set SKIA_SOURCE_DIR to a \
             patched tree made by this repository's `node scripts/skia-source.mjs prepare`.\n",
            inputs.target,
            manifest
                .targets
                .iter()
                .map(|hosted| hosted.triple.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// The patched tree a directory lies in: the nearest of `ancestors` (each
/// directory's name, innermost first) named as `scripts/skia-source.mjs`
/// names a tree, `skia-<version>-<hex digest>`, or `None`.
pub fn tree_name<'a>(
    ancestors: impl IntoIterator<Item = &'a str>,
) -> Option<&'a str> {
    ancestors.into_iter().find(|name| {
        name.strip_prefix("skia-")
            .and_then(|rest| rest.rsplit_once('-'))
            .is_some_and(|(version, digest)| {
                let parts: Vec<&str> = version.split('.').collect();
                parts.len() == 3
                    && parts.iter().all(|part| {
                        !part.is_empty()
                            && part.bytes().all(|b| b.is_ascii_digit())
                    })
                    && !digest.is_empty()
                    && digest.bytes().all(|b| b.is_ascii_hexdigit())
            })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const URL: &str = "https://github.com/l7aromeo/meo-skia-canvas/releases/download/skia-binaries-0.156.0-0123456789ab/skia-binaries-{key}.tar.gz";

    fn manifest() -> Manifest {
        parse_manifest(&format!(
            "# generated\nrelease skia-binaries-0.156.0-0123456789ab\nurl {URL}\n\n\
             target aarch64-apple-darwin skia-0.156.0-918bd4ffa80d meo_atlas_size meo_exact_blend_division\n\
             target x86_64-unknown-linux-gnu skia-0.156.0-732ea61409ba meo_exact_blend_division\n"
        ))
        .expect("the manifest parses")
    }

    fn inputs(target: &str) -> Inputs<'_> {
        Inputs {
            target,
            ..Inputs::default()
        }
    }

    #[test]
    fn the_manifest_reads_its_release_url_and_each_targets_tree() {
        assert_eq!(
            manifest(),
            Manifest {
                release: "skia-binaries-0.156.0-0123456789ab".into(),
                url: URL.into(),
                targets: vec![
                    Hosted {
                        triple: "aarch64-apple-darwin".into(),
                        tree: "skia-0.156.0-918bd4ffa80d".into(),
                        cfgs: vec![
                            "meo_atlas_size".into(),
                            "meo_exact_blend_division".into()
                        ],
                    },
                    Hosted {
                        triple: "x86_64-unknown-linux-gnu".into(),
                        tree: "skia-0.156.0-732ea61409ba".into(),
                        cfgs: vec!["meo_exact_blend_division".into()],
                    },
                ],
            }
        );
    }

    #[test]
    fn a_manifest_missing_its_release_url_or_a_targets_tree_is_refused() {
        assert!(parse_manifest("url x\ntarget t s a\n").is_err());
        assert!(parse_manifest("release x\ntarget t s a\n").is_err());
        assert!(parse_manifest("release x\nurl y\nother z\n").is_err());
        assert!(parse_manifest("release x\nurl y\ntarget t\n").is_err());
    }

    #[test]
    fn nothing_set_is_stock_skia() {
        assert_eq!(
            mode(&inputs("aarch64-apple-darwin"), &manifest()),
            Mode::Stock
        );
    }

    #[test]
    fn docs_rs_builds_no_skia_whatever_else_is_set() {
        let docs = Inputs {
            docs_rs: true,
            binaries_url: Some(URL),
            ..inputs("aarch64-apple-darwin")
        };
        assert_eq!(mode(&docs, &manifest()), Mode::Docs);
    }

    #[test]
    fn a_named_source_tree_is_compiled() {
        let source = Inputs {
            source_dir: Some("/tree"),
            binaries_url: Some(URL),
            ..inputs("aarch64-apple-darwin")
        };
        assert_eq!(mode(&source, &manifest()), Mode::Source);
    }

    #[test]
    fn a_local_checkout_is_compiled() {
        let local = Inputs {
            local: true,
            ..inputs("x86_64-unknown-linux-gnu")
        };
        assert_eq!(mode(&local, &manifest()), Mode::Local);
    }

    #[test]
    fn our_url_on_a_hosted_target_takes_the_targets_tree_and_patches() {
        let ours = Inputs {
            binaries_url: Some(URL),
            ..inputs("aarch64-apple-darwin")
        };
        assert_eq!(
            mode(&ours, &manifest()),
            Mode::Binaries {
                tree: "skia-0.156.0-918bd4ffa80d".into(),
                cfgs: vec![
                    "meo_atlas_size".into(),
                    "meo_exact_blend_division".into()
                ],
            }
        );
    }

    /// The refusal's text, or a panic naming what came back instead.
    fn refused(mode: Mode) -> String {
        match mode {
            Mode::Refused(why) => why,
            other => panic!("not refused: {other:?}"),
        }
    }

    #[test]
    fn our_url_on_a_target_not_hosted_is_refused_naming_the_target() {
        let ours = Inputs {
            binaries_url: Some(URL),
            ..inputs("riscv64gc-unknown-linux-gnu")
        };
        let why = refused(mode(&ours, &manifest()));
        assert!(why.contains("riscv64gc-unknown-linux-gnu"), "{why}");
        assert!(why.contains("SKIA_SOURCE_DIR"), "{why}");
    }

    #[test]
    fn a_line_naming_another_of_our_releases_is_refused_with_this_ones_line() {
        let stale = Inputs {
            binaries_url: Some(
                "https://github.com/l7aromeo/meo-skia-canvas/releases/download/skia-binaries-0.156.0-f27f9d929c65/skia-binaries-{key}.tar.gz",
            ),
            ..inputs("aarch64-apple-darwin")
        };
        let why = refused(mode(&stale, &manifest()));
        assert!(why.contains("skia-binaries-0.156.0-f27f9d929c65"), "{why}");
        assert!(
            why.contains(&format!("SKIA_BINARIES_URL = \"{URL}\"")),
            "{why}"
        );
    }

    #[test]
    fn another_url_is_stock() {
        let other = Inputs {
            binaries_url: Some(
                "https://example.com/{tag}/skia-binaries-{key}.tar.gz",
            ),
            ..inputs("aarch64-apple-darwin")
        };
        assert_eq!(mode(&other, &manifest()), Mode::Stock);
    }

    #[test]
    fn the_feature_without_a_url_is_refused_naming_the_line() {
        let feature = Inputs {
            patched_skia: true,
            ..inputs("aarch64-apple-darwin")
        };
        let why = refused(mode(&feature, &manifest()));
        assert!(why.contains("patched-skia"), "{why}");
        assert!(
            why.contains(&format!("SKIA_BINARIES_URL = \"{URL}\"")),
            "{why}"
        );
    }

    #[test]
    fn the_feature_with_another_url_is_refused_naming_the_line() {
        let feature = Inputs {
            patched_skia: true,
            binaries_url: Some(
                "https://example.com/{tag}/skia-binaries-{key}.tar.gz",
            ),
            ..inputs("aarch64-apple-darwin")
        };
        let why = refused(mode(&feature, &manifest()));
        assert!(
            why.contains(&format!("SKIA_BINARIES_URL = \"{URL}\"")),
            "{why}"
        );
    }

    #[test]
    fn the_feature_with_our_url_takes_the_archives() {
        let feature = Inputs {
            patched_skia: true,
            binaries_url: Some(URL),
            ..inputs("x86_64-unknown-linux-gnu")
        };
        assert_eq!(
            mode(&feature, &manifest()),
            Mode::Binaries {
                tree: "skia-0.156.0-732ea61409ba".into(),
                cfgs: vec!["meo_exact_blend_division".into()],
            }
        );
    }

    #[test]
    fn the_feature_with_a_source_tree_compiles_it() {
        let feature = Inputs {
            patched_skia: true,
            source_dir: Some("/tree"),
            ..inputs("aarch64-apple-darwin")
        };
        assert_eq!(mode(&feature, &manifest()), Mode::Source);
    }

    #[test]
    fn a_source_trees_name_is_the_tree_directory_above_rust_skia() {
        // `SKIA_SOURCE_DIR` as `skia-source.mjs env` names it, innermost first:
        // rust-skia's own `skia-bindings` starts with `skia-` too.
        let ancestors = [
            "skia",
            "skia-bindings",
            "rust-skia",
            "skia-0.156.0-918bd4ffa80d",
            "meo-skia-canvas",
            ".cache",
        ];
        assert_eq!(tree_name(ancestors), Some("skia-0.156.0-918bd4ffa80d"));
    }

    #[test]
    fn a_checkout_with_no_tree_above_it_has_no_tree_name() {
        assert_eq!(
            tree_name(["skia", "skia-bindings", "rust-skia", "src"]),
            None
        );
    }

    #[test]
    fn an_empty_source_dir_is_not_a_tree() {
        let empty = Inputs {
            source_dir: Some(""),
            ..inputs("aarch64-apple-darwin")
        };
        assert_eq!(mode(&empty, &manifest()), Mode::Stock);
    }
}
