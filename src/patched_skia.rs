//! Which of this repository's patched Skia trees a build links.
//!
//! This crate's own builds, and a crate build that names the patched archives
//! in `SKIA_BINARIES_URL`, link Skia built with `containers/*.patch`, which
//! draws what the npm package draws. A crate build that names nothing links
//! crates.io's Skia, as every crate build did before the archives.

#[cfg(meo_skia_marker)]
use std::ffi::c_char;

#[cfg(meo_skia_marker)]
unsafe extern "C" {
    /// Defined only by Skia built with `containers/skia-patch-marker.patch`.
    /// Referenced wherever this crate starts Skia, so that a build whose
    /// download fell back to crates.io's Skia fails to link, and the linker
    /// prints this name, which says what to check.
    fn meo_skia_canvas_needs_its_patched_skia_see_SKIA_BINARIES_URL()
    -> *const c_char;
}

/// The patched Skia tree this build links, as `skia-binaries.txt` names it
/// for the build's target -- `skia-<skia-bindings version>-<patch digest>` --
/// or an empty string for crates.io's Skia.
///
/// The tree names the version of skia-bindings it is for and a digest of
/// the patches its platform takes, and a crate version is built against
/// exactly one per platform: the one its `SKIA_BINARIES_URL` line's archives
/// were built from. Log it, or compare it, to tell a build that draws what
/// the npm package draws from one that links stock Skia.
pub fn skia_patches() -> &'static str {
    #[cfg(meo_skia_marker)]
    {
        link_marker();
        env!("MEO_SKIA_TREE")
    }
    #[cfg(not(meo_skia_marker))]
    {
        ""
    }
}

/// References the marker, so that every build that starts Skia needs it at
/// link time.
pub(crate) fn link_marker() {
    #[cfg(meo_skia_marker)]
    {
        // SAFETY: the function takes nothing, touches no state and returns a
        // pointer to a string literal; calling it only needs the symbol,
        // which is the point.
        let marker = unsafe {
            meo_skia_canvas_needs_its_patched_skia_see_SKIA_BINARIES_URL()
        };
        std::hint::black_box(marker);
    }
}
