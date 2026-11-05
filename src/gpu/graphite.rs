//! Graphite, Skia's second GPU backend, as the engine for the pages Ganesh
//! cannot composite.
//!
//! Ganesh's Metal backend has no 32-bit float format, so an `F32` page that
//! asks for the GPU rasterizes on the CPU. Graphite's Metal backend blends one
//! in float, and faster, but narrows the colour of every draw it shades to a
//! half float on the way: Skia's SkSL generator for Metal writes each half as
//! an MSL `half`, the fragment output included. So no canvas is routed to it --
//! which pages it would take is [`RenderingEngine::for_canvas`]'s decision, and
//! [`RenderingEngine::can_composite`] refuses it `F32` while a draw does not
//! keep its colour. This module is the engine.
//!
//! The engine is built where `meo_graphite` is set and the backend is Metal.
//! The cfg is set only by the patched skia-bindings that carries the bindings
//! used here (`build.rs`). Elsewhere every function here answers as though
//! no Graphite device exists: [`supported`] is false, a surface is refused,
//! no surface is a Graphite page, and [`commit`] returns
//! [`GraphiteRead::NotBuilt`]. Callers then need no cfg of their own, and a
//! Graphite page that turns up where none can exist is refused with that
//! cause named.
//!
//! This module decides no waits or confirmations for a page's draws and
//! reads. It gives the operations they are made of -- [`owns`], [`commit`],
//! [`mark`], [`confirm`] and [`read_raw`] -- and leaves which a page needs to
//! its caller, so that one place can decide them for every engine.
//!
//! [`RenderingEngine::for_canvas`]: super::RenderingEngine::for_canvas
//! [`RenderingEngine::can_composite`]: super::RenderingEngine::can_composite

use std::fmt;

#[cfg(all(test, meo_graphite, feature = "metal"))]
use objc2::rc::Retained;
use serde_json::Value;
use skia_safe::{Canvas, IPoint, ImageInfo, Surface};

use crate::context::page::ExportOptions;
#[cfg(all(test, meo_graphite, feature = "metal"))]
use crate::gpu::metal::RecordingQueue;

#[cfg(all(meo_graphite, feature = "metal"))]
mod metal;
#[cfg(not(all(meo_graphite, feature = "metal")))]
use absent as engine;
pub(crate) use engine::Mark;
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

/// Whether `surface` is a Graphite page: one with a recorder rather than a
/// recording context. Says nothing about which thread's context made it.
#[cfg_attr(
    not(any(test, feature = "metal", feature = "vulkan")),
    expect(dead_code, reason = "only a GPU backend's reads are confirmed")
)]
pub(crate) fn owns(surface: &mut Surface) -> bool {
    engine::owns(surface)
}

/// Commits the draws recorded on this thread's context: snaps the recorder,
/// inserts the recording and submits it, waiting for the GPU to finish it
/// where `wait` says so.
///
/// Graphite records draws and runs them only when a recording is inserted
/// and submitted, so a page's draws reach the GPU only through this. A
/// thread with no context has nothing to commit and answers
/// [`GraphiteRead::NoContext`]: a Graphite page reached there was made on
/// another thread, and is refused rather than retried.
#[cfg_attr(
    not(any(test, feature = "metal", feature = "vulkan")),
    expect(dead_code, reason = "only a GPU backend's reads are confirmed")
)]
pub(crate) fn commit(wait: bool) -> Result<(), GraphiteRead> {
    engine::commit(wait)
}

/// Where a confirmation begins on this thread's queue, `None` where the
/// thread holds no context.
#[cfg_attr(
    not(any(test, feature = "metal", feature = "vulkan")),
    expect(dead_code, reason = "only a GPU backend's reads are confirmed")
)]
pub(crate) fn mark() -> Option<Mark> {
    engine::mark()
}

/// Whether every command buffer this thread's queue has seen committed ran,
/// waiting for any still running. With `since`, from [`mark`], no buffer
/// seen to finish since then is a failure too. `Ok` where the thread holds no
/// context, which has committed nothing.
///
/// Graphite's own reads and submits report success whether or not their
/// buffers ran, so this is the only word on whether a page's draws did.
#[cfg_attr(
    not(any(test, feature = "metal", feature = "vulkan")),
    expect(dead_code, reason = "only a GPU backend's reads are confirmed")
)]
pub(crate) fn confirm(since: Option<Mark>) -> Result<(), String> {
    engine::confirm(since)
}

/// Reads `surface`, a page made on this thread, into `dst` as `info`
/// describes it, from `at` in the page, through the context. False where the
/// read is declined or the thread holds no context.
///
/// Reads only what has been committed: the draws reach it through [`commit`]
/// first. Graphite submits and waits for the copy itself, and with it any
/// work submitted before, and says nothing of whether it ran -- see
/// [`confirm`]. `Surface::read_pixels` does not read a Graphite surface at
/// all.
pub(crate) fn read_raw(
    surface: &mut Surface,
    info: &ImageInfo,
    dst: &mut [u8],
    row_bytes: usize,
    at: impl Into<IPoint>,
) -> bool {
    engine::read_raw(surface, info, dst, row_bytes, at.into())
}

/// This thread's queue of ours, `None` where it holds no context, for a test
/// to arm failures on.
#[cfg(all(test, meo_graphite, feature = "metal"))]
pub(crate) fn queue() -> Option<Retained<RecordingQueue>> {
    engine::queue()
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

    /// No mark is ever made where there is no queue to make it on.
    #[derive(Clone, Copy)]
    pub(crate) enum Mark {}

    #[cfg_attr(
        not(any(test, feature = "metal", feature = "vulkan")),
        expect(dead_code, reason = "only a GPU backend's reads are confirmed")
    )]
    pub(super) fn owns(_surface: &mut Surface) -> bool {
        false
    }

    #[cfg_attr(
        not(any(test, feature = "metal", feature = "vulkan")),
        expect(dead_code, reason = "only a GPU backend's reads are confirmed")
    )]
    pub(super) fn commit(_wait: bool) -> Result<(), GraphiteRead> {
        Err(GraphiteRead::NotBuilt)
    }

    #[cfg_attr(
        not(any(test, feature = "metal", feature = "vulkan")),
        expect(dead_code, reason = "only a GPU backend's reads are confirmed")
    )]
    pub(super) fn mark() -> Option<Mark> {
        None
    }

    #[cfg_attr(
        not(any(test, feature = "metal", feature = "vulkan")),
        expect(dead_code, reason = "only a GPU backend's reads are confirmed")
    )]
    pub(super) fn confirm(_since: Option<Mark>) -> Result<(), String> {
        Ok(())
    }

    pub(super) fn read_raw(
        _surface: &mut Surface,
        _info: &ImageInfo,
        _dst: &mut [u8],
        _row_bytes: usize,
        _at: IPoint,
    ) -> bool {
        false
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
    use skia_safe::{AlphaType, ColorSpace, ColorType};

    use super::*;
    use crate::started;

    fn f32_info() -> ImageInfo {
        ImageInfo::new(
            (2, 2),
            ColorType::RGBAF32,
            AlphaType::Premul,
            Some(ColorSpace::new_srgb()),
        )
    }

    /// Without the engine, every entry point refuses and names why, and no
    /// surface is taken for a Graphite page.
    #[cfg(not(all(meo_graphite, feature = "metal")))]
    #[test]
    fn a_build_without_graphite_refuses_every_graphite_page() {
        let info = f32_info();
        assert!(!supported());
        assert_eq!(
            make_surface(&info, &ExportOptions::default()).err(),
            Some(GraphiteRead::NotBuilt.to_string())
        );
        let mut raster = started::raster(&info, None, None).expect("raster");
        let mut dst = vec![0u8; info.compute_min_byte_size()];
        assert!(!owns(&mut raster));
        assert_eq!(commit(true), Err(GraphiteRead::NotBuilt));
        assert!(mark().is_none());
        assert_eq!(confirm(None), Ok(()));
        assert!(!read_raw(
            &mut raster,
            &info,
            &mut dst,
            info.min_row_bytes(),
            (0, 0)
        ));
        assert_eq!(
            probe(&info, |_| {}, &info, &mut dst),
            Err(GraphiteRead::NotBuilt)
        );
    }

    /// A thread that never made a Graphite context has nothing to commit,
    /// mark, confirm or read, and says so rather than making a context that
    /// holds none of a page's draws.
    #[cfg(all(meo_graphite, feature = "metal"))]
    #[test]
    fn a_thread_without_a_context_commits_and_reads_nothing() {
        std::thread::spawn(|| {
            let info = f32_info();
            let mut raster =
                started::raster(&info, None, None).expect("raster");
            let mut dst = vec![0u8; info.compute_min_byte_size()];
            assert!(!owns(&mut raster));
            assert_eq!(commit(false), Err(GraphiteRead::NoContext));
            assert!(mark().is_none());
            assert_eq!(confirm(None), Ok(()));
            assert!(!read_raw(
                &mut raster,
                &info,
                &mut dst,
                info.min_row_bytes(),
                (0, 0)
            ));
        })
        .join()
        .expect("the thread without a context");
    }
}
