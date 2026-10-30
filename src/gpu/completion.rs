//! Whether the GPU actually ran a page's draws.
//!
//! A Metal command buffer can fail without anyone being told: when another
//! process hangs the GPU and the driver recovers, the buffers in flight
//! everywhere else are discarded ("victim of GPU error/recovery"), and Skia's
//! Ganesh backend neither checks the status of a buffer it does not wait on nor
//! acts on the one it does. A readback afterwards returns whatever the texture
//! or transfer buffer held -- zeros, or an earlier draw's pixels -- and reports
//! success. Measured on Apple Silicon by hanging the GPU from another process
//! eight times during exports: 24 wrong results in 360,000, no error, and none
//! on the CPU engine.
//!
//! So every path that draws a page and then reads it back confirms the draw
//! first: flush the surface's context and wait for it with
//! `submit(SyncCpu::Yes)`, whose answer is the status of the buffer it waited
//! on. A draw that did not run is drawn once more on a fresh surface, and a
//! second failure is reported with [`discarded`] rather than handed on as
//! pixels. A read served by tiles nothing has been drawn into since their last
//! confirmation goes without one, since it would wait on nothing of theirs.
//!
//! What this cannot see is the readback's own buffer: Skia waits on it and
//! drops the answer -- see the `UPSTREAM:` note at the readbacks in
//! `context::page`.
//!
//! **The check costs one GPU round trip per readback after a draw.** The
//! readback already waits for the GPU once; this adds a second wait in front of
//! it. Measured on a release build, Apple M4 Pro: a 64x48 PNG export goes from
//! about 380 to 500 microseconds, a raw export from 240 to 350, a
//! `getImageData` from 220 to 300 -- 30 to 45 percent. On a 2048x1536 page the
//! difference is inside the noise of a 4.6 to 51 ms operation, and the first
//! operation of a process is unchanged within its spread. Flushing and
//! submitting without waiting costs nothing measurable, which is what pins the
//! cost on the wait.
//!
//! It is paid because the alternative is pixels that are wrong with no error.
//! It stays until Skia reports a failed draw as well as a failed copy: its
//! `SurfaceContext::readPixels` commits the draws in a command buffer of
//! their own without waiting, then the copy in a second one it waits on, so a
//! fix that checks only the copy's buffer still leaves the draws to this wait.

use skia_safe::Surface;
#[cfg(any(feature = "metal", feature = "vulkan"))]
use skia_safe::gpu::SyncCpu;

use crate::Error;

/// How many times a page's draw is attempted before its failure is reported:
/// the draw, and one retry on a fresh surface. A GPU recovery discards the
/// buffers in flight at one instant, so a second attempt runs after it; a draw
/// failing twice is not the transient this exists for, and drawing a third
/// time would only delay the error.
pub(crate) const DRAW_ATTEMPTS: usize = 2;

/// The start of every message reporting a discarded draw, so that the layers
/// turning a message into a typed error can tell it from an encoder's.
pub(crate) const DISCARDED: &str = "The GPU discarded the work of";

/// The error for a draw that failed [`DRAW_ATTEMPTS`] times, naming the
/// stage it was in.
pub(crate) fn discarded(stage: &str) -> String {
    format!(
        "{DISCARDED} {stage} {DRAW_ATTEMPTS} times, so its pixels are not \
         returned. This happens when the GPU recovers from a fault, often one \
         in another process; trying again, or using the CPU engine, avoids it"
    )
}

/// `reason` as the crate's [`Error`]: [`Error::Render`] for a discarded draw
/// -- the drawing is what failed, which is that variant's meaning -- and
/// whatever `otherwise` makes of it for every other failure on the path.
///
/// A message rather than a type crosses the owner thread and the export
/// code, so the prefix [`DISCARDED`] is what tells them apart here.
pub(crate) fn typed(
    reason: String,
    otherwise: impl FnOnce(String) -> Error,
) -> Error {
    match reason.starts_with(DISCARDED) {
        true => Error::Render { reason },
        false => otherwise(reason),
    }
}

/// Whether the GPU ran every draw recorded so far on `surface`'s context.
///
/// Flushes the context and waits for it. A raster surface has no context and
/// nothing to wait for, so it always answers yes and costs nothing. `page` is
/// the page the draws belong to, which only the test seam reads.
#[cfg(any(feature = "metal", feature = "vulkan"))]
pub(crate) fn draws_ran(surface: &mut Surface, page: usize) -> bool {
    let Some(mut context) = surface.direct_context() else {
        return true;
    };
    // A flush that failed left the draws unrun, whatever the submit says.
    let flushed = context.flush(None).is_ok();
    let ran = context.submit(SyncCpu::Yes);
    flushed && ran && !seam::discards(page)
}

/// With no GPU backend compiled in every surface is raster, so there is no
/// draw the GPU could have discarded.
#[cfg(not(any(feature = "metal", feature = "vulkan")))]
pub(crate) fn draws_ran(_surface: &mut Surface, _page: usize) -> bool {
    true
}

/// A way for tests to make [`draws_ran`] answer no, since a real discard needs
/// a GPU fault that no test can cause safely.
///
/// Compiled, with a GPU backend, into debug builds and into test builds of
/// either profile, so the tests below run under `--release` too. Every other
/// release build -- each published binary among them -- gets the stub below,
/// which always answers no discard, so nothing outside a test or a debug
/// build can arm it. Without a GPU backend there are no draws to discard and
/// no seam at all.
#[cfg(all(
    any(test, debug_assertions),
    any(feature = "metal", feature = "vulkan")
))]
pub(crate) mod seam {
    use parking_lot::Mutex;

    /// One armed discard.
    struct Armed {
        /// The page whose draws it covers, or `None` for any page.
        page: Option<usize>,
        /// Draws to let through before discarding any.
        skip: usize,
        /// Draws still to discard.
        left: usize,
    }

    static ARMED: Mutex<Vec<Armed>> = Mutex::new(Vec::new());

    /// Makes the next `times` draws of `page` -- or of any page, with `None`
    /// -- report that the GPU discarded them. Armed by the tests here and by
    /// the debug addon's `__discardGpuDraws`, so built only for those.
    #[cfg(any(test, feature = "node-addon"))]
    pub(crate) fn discard(page: Option<usize>, times: usize) {
        discard_after(page, 0, times);
    }

    /// As [`discard`], letting `skip` draws through first, so that a test can
    /// reach a draw that only happens after an earlier one succeeded.
    #[cfg(any(test, feature = "node-addon"))]
    pub(crate) fn discard_after(
        page: Option<usize>,
        skip: usize,
        times: usize,
    ) {
        ARMED.lock().push(Armed {
            page,
            skip,
            left: times,
        });
    }

    /// How many discards armed for `page` alone have not been used, so a
    /// test can tell a check that consulted the seam from one that never ran.
    #[cfg(test)]
    pub(crate) fn unused(page: usize) -> usize {
        ARMED
            .lock()
            .iter()
            .filter(|armed| armed.page == Some(page))
            .map(|armed| armed.left)
            .sum()
    }

    /// Whether this draw of `page` is one an armed discard covers, using it up.
    pub(crate) fn discards(page: usize) -> bool {
        let mut all = ARMED.lock();
        let Some(armed) = all.iter_mut().find(|armed| {
            armed.left > 0 && armed.page.is_none_or(|id| id == page)
        }) else {
            return false;
        };
        if armed.skip > 0 {
            armed.skip -= 1;
            return false;
        }
        armed.left -= 1;
        all.retain(|armed| armed.left > 0);
        true
    }
}

#[cfg(all(
    not(any(test, debug_assertions)),
    any(feature = "metal", feature = "vulkan")
))]
pub(crate) mod seam {
    /// No seam in a release build: a draw is discarded only if the GPU says so.
    #[inline]
    pub(crate) fn discards(_page: usize) -> bool {
        false
    }
}

#[cfg(all(test, any(feature = "metal", feature = "vulkan")))]
mod tests {
    use super::{DISCARDED, seam};
    use crate::{
        Canvas, Context2D, EncodeOptions, Error, ImageFormat, PixelColorSpace,
        RgbaLinear, canvas::EngineKind,
    };

    /// A canvas with something on it, on the GPU, and the id of its page --
    /// or `None` on a machine with no GPU engine, where nothing here can be
    /// checked, since a raster surface has no draws to discard.
    fn drawn(width: f32, height: f32) -> Option<(Canvas, usize)> {
        let mut canvas = Canvas::new(width, height);
        if canvas.engine_kind() != EngineKind::Gpu {
            eprintln!("skipped: no GPU engine, so no draw can be discarded");
            return None;
        }
        let ctx = canvas.context();
        ctx.set_fill_style(RgbaLinear::opaque(0.1, 0.4, 0.8));
        ctx.fill_rect(0.0, 0.0, width, height);
        let mut page = 0;
        ctx.inner
            .with_recorder(|mut recorder| page = recorder.page().id);
        Some((canvas, page))
    }

    fn png(canvas: &mut Canvas) -> Result<Vec<u8>, Error> {
        canvas.to_buffer(ImageFormat::Png, &EncodeOptions::default())
    }

    fn assert_discarded(
        result: Result<impl std::fmt::Debug, Error>,
        stage: &str,
    ) {
        match result {
            Err(Error::Render { reason }) => {
                assert!(reason.starts_with(DISCARDED), "{reason}");
                assert!(reason.contains(stage), "{reason} names no {stage:?}");
            }
            other => panic!("expected a discarded {stage}, got {other:?}"),
        }
    }

    #[test]
    fn an_export_whose_draw_was_discarded_is_drawn_again() {
        let Some((mut canvas, page)) = drawn(64.0, 48.0) else {
            return;
        };
        let Some((mut reference, _)) = drawn(64.0, 48.0) else {
            return;
        };
        seam::discard(Some(page), 1);
        let got = png(&mut canvas).expect("the retry succeeds");
        assert_eq!(
            seam::unused(page),
            0,
            "the export never asked whether its draw ran"
        );
        assert_eq!(got, png(&mut reference).expect("reference"));
    }

    #[test]
    fn an_export_discarded_twice_is_an_error_not_pixels() {
        let Some((mut canvas, page)) = drawn(64.0, 48.0) else {
            return;
        };
        seam::discard(Some(page), 2);
        assert_discarded(png(&mut canvas), "compositing the page");
    }

    #[test]
    fn a_raw_export_discarded_twice_is_an_error() {
        let Some((mut canvas, page)) = drawn(64.0, 48.0) else {
            return;
        };
        seam::discard(Some(page), 2);
        let raw = canvas.to_buffer(ImageFormat::Raw, &EncodeOptions::default());
        assert_discarded(raw, "compositing the page");
    }

    #[test]
    fn a_frame_for_another_encoder_discarded_twice_is_an_error() {
        // GIF is encoded here rather than by Skia, from a frame the page
        // composites into a pixel buffer of its own.
        let Some((mut canvas, page)) = drawn(64.0, 48.0) else {
            return;
        };
        seam::discard(Some(page), 2);
        let gif = canvas.to_buffer(ImageFormat::Gif, &EncodeOptions::default());
        assert_discarded(gif, "compositing the page");
    }

    #[test]
    fn a_handle_discarded_twice_is_an_error_when_it_encodes() {
        let Some((mut canvas, page)) = drawn(64.0, 48.0) else {
            return;
        };
        let pages = canvas
            .prepare_export(ImageFormat::Png, &EncodeOptions::default())
            .expect("prepared");
        seam::discard(Some(page), 2);
        assert_discarded(pages.encode(), "compositing the page");
    }

    #[test]
    fn a_page_read_whose_draw_was_discarded_is_drawn_again() {
        // Wider than one tile, so the read is served by the whole page.
        let Some((mut canvas, page)) = drawn(600.0, 300.0) else {
            return;
        };
        let Some((mut reference, _)) = drawn(600.0, 300.0) else {
            return;
        };
        seam::discard(Some(page), 1);
        let data = canvas
            .context()
            .get_image_data(0.0, 0.0, 600.0, 300.0)
            .expect("read");
        assert_eq!(
            seam::unused(page),
            0,
            "the read never asked whether its draw ran"
        );
        let want = reference
            .context()
            .get_image_data(0.0, 0.0, 600.0, 300.0)
            .expect("reference");
        assert_eq!(data.pixels(), want.pixels());
    }

    #[test]
    fn a_page_read_discarded_twice_is_an_error() {
        let Some((mut canvas, page)) = drawn(600.0, 300.0) else {
            return;
        };
        seam::discard(Some(page), 2);
        let read = canvas.context().get_image_data(0.0, 0.0, 600.0, 300.0);
        assert_discarded(read, "drawing the page for a read");
    }

    #[test]
    fn a_tiled_read_discarded_twice_is_an_error() {
        // A few pixels off a page much larger than a tile is served by the
        // tiles they touch rather than by a composite of the whole page.
        let Some((mut canvas, page)) = drawn(1024.0, 1024.0) else {
            return;
        };
        seam::discard(Some(page), 2);
        let read = canvas.context().get_image_data(0.0, 0.0, 8.0, 8.0);
        assert_discarded(read, "drawing tiles for a read");
    }

    #[test]
    fn a_tile_drawn_into_after_its_confirmation_is_confirmed_again() {
        let Some((mut canvas, page)) = drawn(1024.0, 1024.0) else {
            return;
        };
        let ctx = canvas.context();
        let first = ctx.get_image_data(0.0, 0.0, 8.0, 8.0).expect("read");
        // Nothing drawn since that read confirmed the tile, so the next read
        // goes without asking -- and an armed discard stays unused.
        seam::discard(Some(page), 2);
        let again = ctx.get_image_data(0.0, 0.0, 8.0, 8.0).expect("read");
        assert_eq!(again.pixels(), first.pixels());
        assert_eq!(seam::unused(page), 2, "an unchanged tile went unconfirmed");
        // A draw into the tile re-arms the check, which the discards refuse.
        ctx.fill_rect(0.0, 0.0, 4.0, 4.0);
        let read = ctx.get_image_data(0.0, 0.0, 8.0, 8.0);
        assert_discarded(read, "drawing tiles for a read");
    }

    #[test]
    fn a_conversion_discarded_twice_is_an_error() {
        // A P3 export of an sRGB canvas redraws the composited page into a
        // P3 surface, which is a second draw to confirm. The composite is let
        // through so that the conversion is what fails.
        let Some((mut canvas, page)) = drawn(64.0, 48.0) else {
            return;
        };
        seam::discard_after(Some(page), 1, 2);
        let options = EncodeOptions {
            color_space: Some(PixelColorSpace::DisplayP3),
            ..EncodeOptions::default()
        };
        let png = canvas.to_buffer(ImageFormat::Png, &options);
        assert_discarded(png, "converting the page to its export colour space");
    }

    /// `canvas` with a layer at 0.5 open over it, holding a square.
    fn in_a_layer(canvas: &mut Canvas) -> &mut Context2D {
        let ctx = canvas.context();
        ctx.save_layer_with(0.5, None, None);
        ctx.fill_rect(8.0, 8.0, 32.0, 24.0);
        ctx
    }

    #[test]
    fn a_read_inside_a_layer_whose_overlay_was_discarded_is_drawn_again() {
        // The page beneath the layer is let through, so that the layer drawn
        // over it is what the GPU discards.
        let Some((mut canvas, page)) = drawn(64.0, 48.0) else {
            return;
        };
        let Some((mut reference, _)) = drawn(64.0, 48.0) else {
            return;
        };
        seam::discard_after(Some(page), 1, 1);
        let data = in_a_layer(&mut canvas)
            .get_image_data(0.0, 0.0, 64.0, 48.0)
            .expect("read");
        assert_eq!(
            seam::unused(page),
            0,
            "the read never asked whether its overlay ran"
        );
        let want = in_a_layer(&mut reference)
            .get_image_data(0.0, 0.0, 64.0, 48.0)
            .expect("reference");
        assert_eq!(data.pixels(), want.pixels());
    }

    #[test]
    fn a_read_inside_a_layer_discarded_twice_is_an_error() {
        let Some((mut canvas, page)) = drawn(64.0, 48.0) else {
            return;
        };
        seam::discard_after(Some(page), 1, 2);
        let read = in_a_layer(&mut canvas).get_image_data(0.0, 0.0, 64.0, 48.0);
        assert_discarded(read, "drawing the open layers for a read");
    }

    #[test]
    fn a_seam_armed_for_one_page_leaves_others_alone() {
        // The guard on the tests above running in parallel: a discard armed
        // for one page must not be spent by another's export.
        let Some((mut armed, page)) = drawn(16.0, 16.0) else {
            return;
        };
        let Some((mut other, _)) = drawn(16.0, 16.0) else {
            return;
        };
        seam::discard(Some(page), 2);
        assert!(png(&mut other).is_ok());
        assert_eq!(seam::unused(page), 2);
        // Spends what was armed, so nothing is left behind for a later test.
        let _ = png(&mut armed);
    }
}
