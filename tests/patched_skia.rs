//! `skia_patches` names the patched Skia tree a build links, and is empty for
//! crates.io's Skia.
//!
//! A source build of this repository compiles the tree `PATCHES` names for
//! its platform, which is `skia-binaries.txt`'s for its target, and links the
//! marker `containers/skia-patch-marker.patch` adds; a crate build that names
//! no archives links neither. A build that downloaded the archives names the
//! tree too, since the marker is in them, and one whose download fell back to
//! crates.io's Skia does not link at all.

use meo_skia_canvas::skia_patches;

/// This build's target triple as `skia-binaries.txt` spells the targets it
/// hosts, or `None` for one it does not.
fn hosted_triple() -> Option<String> {
    let rest = if cfg!(target_os = "macos") {
        "apple-darwin"
    } else if cfg!(all(target_os = "linux", target_env = "gnu")) {
        "unknown-linux-gnu"
    } else if cfg!(all(target_os = "linux", target_env = "musl")) {
        "unknown-linux-musl"
    } else if cfg!(all(target_os = "windows", target_env = "msvc")) {
        "pc-windows-msvc"
    } else {
        return None;
    };
    Some(format!("{}-{rest}", std::env::consts::ARCH))
}

/// The tree `skia-binaries.txt` names for this build's target.
fn manifest_tree() -> Option<&'static str> {
    let triple = hosted_triple()?;
    include_str!("../skia-binaries.txt")
        .lines()
        .filter_map(|line| line.strip_prefix("target "))
        .find_map(|line| {
            let mut words = line.split_whitespace();
            (words.next() == Some(triple.as_str())).then(|| words.next())?
        })
}

#[test]
fn a_build_names_the_patched_skia_it_links() {
    if cfg!(meo_skia_marker) {
        let tree = manifest_tree()
            .expect("skia-binaries.txt names a tree for this target");
        assert_eq!(skia_patches(), tree);
    } else {
        assert_eq!(skia_patches(), "");
    }
}
