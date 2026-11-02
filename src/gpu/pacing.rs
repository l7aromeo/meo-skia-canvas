//! Flushing a GPU playback every so many image-filtered draws.
//!
//! Ganesh, Skia's GPU backend, holds a page's draws as render tasks until its
//! context is flushed, and a flush first reorders every task it holds
//! (`GrDrawingManager::reorderTasks`) by clustering them
//! (`GrClusterRenderTasks`), which walks each task against what the others
//! depend on: the work grows with the square of the tasks in one flush. An
//! image-filtered draw makes tasks of its own -- what it filters, the
//! filter's passes, and its return to the page -- so a page of them played
//! onto a GPU surface and flushed once, at the read, grew quadratically. On
//! an Apple M4 Pro, N rects under a drop shadow drawn and read back took 84
//! microseconds a draw at 256 and 2.1 milliseconds a draw at 16,384, every
//! working sample inside that one flush's clustering, where the CPU stays
//! near 15 microseconds a draw.
//!
//! So a playback flushes its context once it has played
//! [`FILTERED_DRAWS_PER_FLUSH`] image-filtered draws since the last flush,
//! and each flush clusters a bounded number of tasks. It counts an entry's
//! ops rather than its draws, which a page does not keep -- see
//! `layer::filtered_draws` -- and the ops bound the draws from above: about
//! one a draw in a long run under a CanvasKit filter, five under a shadow,
//! and a few more where each draw is an entry of its own, which holds the
//! ops of its frame too. So a playback can flush several times as often as
//! the count says, and still a fixed number of times a draw, which keeps
//! the growth linear: what the count is for. It
//! flushes without submitting: the work is recorded into the command buffer the
//! read's confirmation waits on -- see `completion::draws_ran` -- so that wait
//! still covers every draw, and a flush that fails, which leaves its draws
//! unrun, fails the confirmation that follows it -- see [`take_failed`].
//!
//! A page records a run of filtered draws as one entry, which a playback
//! plays whole, so the recorder files such a run in entries of at most
//! [`FILTERED_DRAWS_PER_FLUSH`] draws -- see `PageRecorder::append_isolated`
//! -- and a playback can flush between them.

use std::cell::Cell;

use skia_safe::Canvas as SkCanvas;

/// How many image-filtered draws a page files in one entry, and how many a
/// GPU playback counts -- by their ops -- between flushes.
///
/// Counted in ops, a playback flushes more often than once every this many
/// draws -- five times as often under a shadow, and more where each draw is
/// an entry of its own -- but still a fixed number of times a draw, so its
/// time stays linear in the draws.
///
/// Chosen, not fixed: small enough that one flush's clustering stays cheap
/// -- 256 shadowed rects drawn and read back took 84 microseconds a draw,
/// against 245 at 4,096 -- and large enough that a page of a few filtered
/// draws, the common case, never flushes before its read.
pub(crate) const FILTERED_DRAWS_PER_FLUSH: usize = 256;

thread_local! {
    /// The image-filtered draws played on this thread since its playback
    /// last flushed, or began.
    static SINCE_FLUSH: Cell<usize> = const { Cell::new(0) };
    /// Whether a flush made on this thread failed since a confirmation last
    /// asked -- see [`take_failed`].
    static FAILED: Cell<bool> = const { Cell::new(false) };
    /// The flushes the playback running on this thread has made.
    #[cfg(test)]
    static FLUSHES: Cell<usize> = const { Cell::new(0) };
    /// Whether the playback running on this thread flushes at all: off for
    /// the page a test plays as a reference -- see [`tests::unpaced`].
    #[cfg(test)]
    static PACED: Cell<bool> = const { Cell::new(true) };
    /// Whether the flushes of the playback running on this thread report
    /// failure -- see [`tests::fail_flushes`].
    #[cfg(test)]
    static FAILING: Cell<bool> = const { Cell::new(false) };
}

/// Starts the count for a playback of page `page` on this thread.
pub(crate) fn begin(page: usize) {
    SINCE_FLUSH.set(0);
    #[cfg(test)]
    {
        FLUSHES.set(0);
        PACED.set(!tests::UNPACED.lock().contains(&page));
        FAILING.set(tests::FAILING.lock().contains(&page));
    }
    #[cfg(not(test))]
    let _ = page;
}

/// Ends the playback of page `page` begun on this thread, filing the
/// flushes it made for a test to read -- see [`tests::flushes`].
pub(crate) fn end(page: usize) {
    #[cfg(test)]
    tests::PLAYED.lock().push((page, FLUSHES.get()));
    #[cfg(not(test))]
    let _ = page;
}

/// Counts `filtered` image-filtered draws just played onto `canvas` -- see
/// `layer::filtered_draws` -- and flushes its GPU context once
/// [`FILTERED_DRAWS_PER_FLUSH`] have been counted since the last flush. A
/// canvas with no GPU context has nothing to flush.
pub(crate) fn played(canvas: &SkCanvas, filtered: usize) {
    if filtered == 0 {
        return;
    }
    let since = SINCE_FLUSH.get() + filtered;
    if since < FILTERED_DRAWS_PER_FLUSH {
        SINCE_FLUSH.set(since);
        return;
    }
    SINCE_FLUSH.set(0);
    #[cfg(test)]
    if !PACED.get() {
        return;
    }
    // UPSTREAM: skia-safe 0.156.0 -- unfiled -- worked around
    // Re-check: cargo test ganesh_clusters_a_flush_s_tasks_quadratically
    //
    // in a release build, with `-- --ignored`: Skia M156's
    // `GrClusterRenderTasks`, run by every flush of a context made with
    // `reduceOpsTaskSplitting` on, as Ganesh's are by default.
    let flushed = flush(canvas);
    #[cfg(test)]
    let flushed = flushed.map(|flushed| flushed && !FAILING.get());
    if let Some(flushed) = flushed {
        if !flushed {
            FAILED.set(true);
        }
        #[cfg(test)]
        FLUSHES.set(FLUSHES.get() + 1);
    }
}

/// Whether a flush a playback made on this thread failed since this was
/// last asked, which it answers once.
///
/// A failed flush left the draws it held unrun, and the confirmation after
/// a playback flushes only what is left -- see `completion::draws_ran` --
/// so it asks this too. A flush can only fail on this thread for a playback
/// whose confirmation is next to ask: a stale answer costs that one a
/// redraw, never a wrong image. Without a GPU backend nothing flushes, and
/// nothing asks.
#[cfg(any(feature = "metal", feature = "vulkan"))]
pub(crate) fn take_failed() -> bool {
    FAILED.replace(false)
}

/// Flushes `canvas`'s GPU context without submitting it, answering whether
/// the flush succeeded, or `None` where the canvas has no context.
#[cfg(any(feature = "metal", feature = "vulkan"))]
fn flush(canvas: &SkCanvas) -> Option<bool> {
    let mut context = canvas.direct_context()?;
    Some(context.flush(None).is_ok())
}

/// With no GPU backend compiled in every canvas is raster or a recording,
/// with no context to flush.
#[cfg(not(any(feature = "metal", feature = "vulkan")))]
fn flush(_canvas: &SkCanvas) -> Option<bool> {
    None
}

/// What the tests read and set of the playbacks of their own pages, which
/// may run on the GPU's owner thread rather than the test's, and of the
/// recordings made on theirs.
#[cfg(test)]
pub(crate) mod tests {
    use std::{cell::Cell, collections::BTreeSet};

    use parking_lot::Mutex;

    thread_local! {
        /// Whether a page recorded on this thread files a run of filtered
        /// draws in one entry however long it is, as the reference the
        /// split one is held to.
        pub(crate) static UNSPLIT: Cell<bool> = const { Cell::new(false) };
    }

    /// The pages played so far, each with the flushes its playback made.
    pub(super) static PLAYED: Mutex<Vec<(usize, usize)>> =
        Mutex::new(Vec::new());
    /// The pages whose playbacks do not flush.
    pub(super) static UNPACED: Mutex<BTreeSet<usize>> =
        Mutex::new(BTreeSet::new());

    /// The flushes each playback of `page` made, in order.
    pub(crate) fn flushes(page: usize) -> Vec<usize> {
        PLAYED
            .lock()
            .iter()
            .filter(|(played, _)| *played == page)
            .map(|(_, flushes)| *flushes)
            .collect()
    }

    /// The pages whose playbacks' flushes report failure.
    pub(super) static FAILING: Mutex<BTreeSet<usize>> =
        Mutex::new(BTreeSet::new());

    /// Makes every later playback of `page` play without flushing.
    pub(crate) fn unpaced(page: usize) {
        UNPACED.lock().insert(page);
    }

    /// Makes every flush a later playback of `page` makes report failure,
    /// as a GPU fault would.
    pub(crate) fn fail_flushes(page: usize) {
        FAILING.lock().insert(page);
    }
}
