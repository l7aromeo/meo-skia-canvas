//! Graphite, Skia's second GPU backend, as the engine for the pages Ganesh
//! cannot composite.
//!
//! Ganesh's Metal backend has no 32-bit float format, so an `F32` page that
//! asks for the GPU rasterizes on the CPU. Graphite's Metal backend composites
//! one in float, and faster. Which pages it takes is
//! [`RenderingEngine::for_canvas`]'s decision; this module is the engine.
//!
//! The engine is built where `meo_graphite` is set and the backend is Metal.
//! The cfg is set only by the patched skia-bindings that carries the bindings
//! used here (`build.rs`). Elsewhere every function here answers as though
//! no Graphite device exists: [`supported`] is false, a surface is refused,
//! and [`read_pixels`] returns [`GraphiteRead::NotBuilt`]. Callers then need
//! no cfg of their own, and a Graphite page that turns up where none can
//! exist is refused with that cause named.
//!
//! [`RenderingEngine::for_canvas`]: super::RenderingEngine::for_canvas

use std::fmt;

use serde_json::Value;
use skia_safe::{Canvas, IPoint, ImageInfo, Surface};

use crate::context::page::ExportOptions;

#[cfg(all(meo_graphite, feature = "metal"))]
mod metal;
#[cfg(not(all(meo_graphite, feature = "metal")))]
use absent as engine;
#[cfg(all(meo_graphite, feature = "metal"))]
use metal as engine;

/// Why a read of a Graphite page handed back no pixels.
///
/// Without the engine only [`Self::NotBuilt`] is ever answered.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(not(all(meo_graphite, feature = "metal")), allow(dead_code))]
pub(crate) enum GraphiteRead {
    /// This build has no Graphite engine for its GPU backend, so it holds no
    /// Graphite page to read.
    #[cfg_attr(all(meo_graphite, feature = "metal"), allow(dead_code))]
    NotBuilt,
    /// This thread holds no Graphite context. A Graphite surface belongs to
    /// the thread whose context made it and is read there, within the call
    /// that drew it; one read anywhere else ends up here.
    NoContext,
    /// Skia refused the page's draws: the recorder could not snap them, or
    /// the context would not insert or submit the recording. The text says
    /// which, with Skia's status where it gives one.
    Refused(String),
    /// The context declined the read, or the read's wait ran out before Skia
    /// answered.
    Declined,
    /// A command buffer the draws or the read ran in failed, or none was
    /// seen to finish. The text is the queue's reason.
    Unconfirmed(String),
}

impl fmt::Display for GraphiteRead {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotBuilt => f.write_str(
                "this build has no Graphite engine, so it has no Graphite page \
                 to read",
            ),
            Self::NoContext => f.write_str(
                "this thread holds no Graphite context to read the page with",
            ),
            Self::Refused(why) => {
                write!(f, "Graphite refused the page's draws: {why}")
            }
            Self::Declined => f.write_str("Graphite declined the read"),
            Self::Unconfirmed(why) => {
                write!(f, "the GPU did not confirm the page's draws: {why}")
            }
        }
    }
}

impl std::error::Error for GraphiteRead {}

/// Whether a Graphite context can be made on this machine, asked once for the
/// process.
pub(crate) fn supported() -> bool {
    engine::supported()
}

/// What the engine reports for `canvas.engine`.
pub(crate) fn status() -> Value {
    engine::status()
}

/// A page of `info` on this thread's Graphite context, made the first time a
/// thread asks.
///
/// `opts.msaa` does not reach the surface: Graphite takes its sample count
/// from the context, which covers paths and text in a raster atlas, so a
/// Graphite page is antialiased as a CPU page is.
pub(crate) fn make_surface(
    info: &ImageInfo,
    opts: &ExportOptions,
) -> Result<Surface, String> {
    engine::make_surface(info, opts)
}

/// Reads `surface`, a page made by [`make_surface`] on this thread, into
/// `dst` as `info` describes it, from `at` in the page.
///
/// Graphite records draws and runs them only when a recording is inserted
/// and submitted, and `Surface::read_pixels` does not read a Graphite
/// surface at all. So this snaps the thread's recorder, inserts the recording,
/// submits it and waits, then reads through the context. It then confirms
/// every command buffer the draws and the read ran in, since Graphite's read
/// reports success whether or not they ran.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the page's reads do not dispatch on Graphite pages yet"
    )
)]
pub(crate) fn read_pixels(
    surface: &mut Surface,
    info: &ImageInfo,
    dst: &mut [u8],
    row_bytes: usize,
    at: impl Into<IPoint>,
) -> Result<(), GraphiteRead> {
    engine::read_pixels(surface, info, dst, row_bytes, at.into())
}

/// Draws `draw` on a page of `info` on a Graphite context made for this call
/// alone, then reads the page into `dst` as `read` describes it.
///
/// For the engine's probes, which ask a question of Graphite once for the
/// process, on whatever thread asks it. A context of its own leaves nothing
/// behind on that thread for the idle release to find.
pub(crate) fn probe(
    info: &ImageInfo,
    draw: impl FnOnce(&Canvas),
    read: &ImageInfo,
    dst: &mut [u8],
) -> Result<(), GraphiteRead> {
    engine::probe(info, draw, read, dst)
}

/// Drops this thread's Graphite context once it has gone unused for as long
/// as a Ganesh context is kept; called wherever the Ganesh one is released.
#[cfg(feature = "metal")]
pub(crate) fn release_idle_context() {
    engine::release_idle_context()
}

/// The engine where this build has none: nothing to make, read or release.
#[cfg(not(all(meo_graphite, feature = "metal")))]
mod absent {
    use serde_json::Value;
    use skia_safe::{Canvas, IPoint, ImageInfo, Surface};

    use super::GraphiteRead;
    use crate::context::page::ExportOptions;

    pub(super) fn supported() -> bool {
        false
    }

    pub(super) fn status() -> Value {
        serde_json::json!({
            "renderer": "CPU",
            "api": Value::Null,
            "device": "CPU-based renderer (compiled without Graphite)",
            "error": Value::Null,
        })
    }

    pub(super) fn make_surface(
        _info: &ImageInfo,
        _opts: &ExportOptions,
    ) -> Result<Surface, String> {
        Err(GraphiteRead::NotBuilt.to_string())
    }

    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "called only by `super::read_pixels`")
    )]
    pub(super) fn read_pixels(
        _surface: &mut Surface,
        _info: &ImageInfo,
        _dst: &mut [u8],
        _row_bytes: usize,
        _at: IPoint,
    ) -> Result<(), GraphiteRead> {
        Err(GraphiteRead::NotBuilt)
    }

    pub(super) fn probe(
        _info: &ImageInfo,
        _draw: impl FnOnce(&Canvas),
        _read: &ImageInfo,
        _dst: &mut [u8],
    ) -> Result<(), GraphiteRead> {
        Err(GraphiteRead::NotBuilt)
    }

    #[cfg(feature = "metal")]
    pub(super) fn release_idle_context() {}
}

#[cfg(test)]
mod tests {
    use skia_safe::{AlphaType, ColorSpace, ColorType, surfaces};

    use super::*;

    fn f32_info() -> ImageInfo {
        ImageInfo::new(
            (2, 2),
            ColorType::RGBAF32,
            AlphaType::Premul,
            Some(ColorSpace::new_srgb()),
        )
    }

    /// Without the engine, every entry point refuses and names why, rather
    /// than reading a surface it cannot have made.
    #[cfg(not(all(meo_graphite, feature = "metal")))]
    #[test]
    fn a_build_without_graphite_refuses_every_graphite_page() {
        let info = f32_info();
        assert!(!supported());
        assert_eq!(
            make_surface(&info, &ExportOptions::default()).err(),
            Some(GraphiteRead::NotBuilt.to_string())
        );
        let mut raster = surfaces::raster(&info, None, None).expect("raster");
        let mut dst = vec![0u8; info.compute_min_byte_size()];
        assert_eq!(
            read_pixels(
                &mut raster,
                &info,
                &mut dst,
                info.min_row_bytes(),
                (0, 0)
            ),
            Err(GraphiteRead::NotBuilt)
        );
        assert_eq!(
            probe(&info, |_| {}, &info, &mut dst),
            Err(GraphiteRead::NotBuilt)
        );
    }

    /// A thread that never made a Graphite context has none to read with,
    /// and says so rather than making one that holds none of the page's
    /// draws.
    #[cfg(all(meo_graphite, feature = "metal"))]
    #[test]
    fn a_thread_without_a_context_refuses_the_read() {
        let read = std::thread::spawn(|| {
            let info = f32_info();
            let mut raster =
                surfaces::raster(&info, None, None).expect("raster");
            let mut dst = vec![0u8; info.compute_min_byte_size()];
            read_pixels(
                &mut raster,
                &info,
                &mut dst,
                info.min_row_bytes(),
                (0, 0),
            )
        })
        .join()
        .expect("the reading thread");
        assert_eq!(read, Err(GraphiteRead::NoContext));
    }
}
