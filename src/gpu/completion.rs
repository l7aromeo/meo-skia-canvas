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
//! So every path that draws a page and then reads it back confirms the draw,
//! by a wait whose answer is the status of the buffer it waited on. A draw
//! that did not run is drawn once more on a fresh surface, and a second
//! failure is reported with [`discarded`] rather than handed on as pixels. A
//! read served by tiles nothing has been drawn into since their last
//! confirmation goes without one, since it would wait on nothing of theirs.
//!
//! A `getImageData` read confirms its draws with its own wait, through
//! [`read_checked`]. The copy is asked for through Skia's asynchronous read,
//! and `submit(SyncCpu::Yes)` commits the buffer it is encoded in, waits for
//! it and answers for it. In Skia M156, under the skia-bindings crate: every
//! encode goes to `GrMtlGpu::commandBuffer`'s current buffer (`skia/src/gpu/
//! ganesh/mtl/GrMtlGpu.mm`, line 188); `SurfaceContext::asyncReadPixels`
//! encodes its transfer there and flushes without submitting (`skia/src/gpu/
//! ganesh/SurfaceContext.cpp`, lines 832 and 871); `GrMtlGpu::
//! submitCommandBuffer` commits that buffer (`GrMtlGpu.mm`, lines 212 to 245);
//! and `GrMtlCommandBuffer::commit` waits and returns whether its status is an
//! error (`GrMtlCommandBuffer.mm`, lines 182 to 206).
//!
//! On Metal the draws are committed first, in a buffer of their own that
//! nothing waits on, because a read that carries its draws keeps Apple's GPU
//! at its lowest clock -- see [`read_checked`]. Skia makes that buffer, like
//! every other, on a queue of ours, which reads its status after the one wait
//! -- see `metal::RecordingQueue`. Elsewhere the copy is encoded in the buffer
//! the draws are already in. Either way one wait confirms the draws and the
//! copy both. Where Skia answers before anything is submitted -- it declined,
//! or read through its own `readPixels` for want of a transfer, which commits
//! the draws in a buffer nothing waits on -- the read is confirmed the other
//! way.
//!
//! An export commits its draws without a wait, as a `getImageData` read does
//! and for the same reason, then reads and confirms after -- see
//! [`read_after_draws`]. Skia's read commits the copy in a buffer of its own,
//! waits on it and drops the answer; the queue of ours holds the draws'
//! buffer and the copy's and reads their status, so the one confirmation
//! after the read answers for both. Where there is no queue of ours, as on
//! Vulkan, the draws are confirmed with [`draws_ran`] before the read, and the
//! copy is confirmed by nothing.
//!
//! A conversion, and a read Skia will not make asynchronously, still flush
//! and wait with [`draws_ran`] first, then read. What that cannot see is the
//! readback's own buffer.
//!
//! **The checked read costs no wait beyond the read's own.** Counted under
//! lldb, a read after a draw waits on the GPU once; the queue of ours waits
//! only for a buffer still running after that wait, which buffers committed
//! in order on one queue have not been seen to be. Measured on a release
//! build, Apple M4 Pro, in three interleaved passes against the wait in front
//! of it: a `getImageData` of 8x8 pixels after a draw takes 185 to 208
//! microseconds against 248 to 253, one of 64x48 199 to 202 against 253 to
//! 258, and one of a whole 1200x900 page 1075 to 1155 against 1237 to 1351.
//!
//! **A wait in front of a read costs one GPU round trip.** The readback
//! already waits for the GPU once; [`draws_ran`] adds a second wait in front
//! of it. Measured on a release build, Apple M4 Pro, with that wait in front
//! of an export's read: a 64x48 PNG export went from about 380 to 500
//! microseconds and a raw export from 240 to 350 -- 30 to 45 percent. On a
//! 2048x1536 page the difference is inside the noise of a 4.6 to 51 ms
//! operation, and the first operation of a process is unchanged within its
//! spread. Flushing and submitting without waiting costs nothing measurable,
//! which is what pins the cost on the wait.

use std::cell::RefCell;
#[cfg(all(feature = "vulkan", not(feature = "metal")))]
use std::convert::Infallible;
#[cfg(any(feature = "metal", feature = "vulkan"))]
use std::rc::Rc;

use rayon::prelude::*;

#[cfg(any(feature = "metal", feature = "vulkan"))]
use skia_safe::{
    IRect,
    gpu::{DirectContext, SyncCpu},
    image::{AsyncReadResult, RescaleGamma, RescaleMode},
};
use skia_safe::{ImageInfo, Surface};

use crate::Error;
#[cfg(feature = "metal")]
use crate::gpu::metal::{self, Mark};
#[cfg(any(feature = "metal", feature = "vulkan"))]
use crate::gpu::pacing;

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
/// stage it was in, and quoting the backend's reason where the last
/// confirmation on this thread gave one.
pub(crate) fn discarded(stage: &str) -> String {
    let reason = FAILURE
        .take()
        .map_or_else(String::new, |reason| format!(" ({reason})"));
    format!(
        "{DISCARDED} {stage} {DRAW_ATTEMPTS} times, so its pixels are not \
         returned{reason}. This happens when the GPU recovers from a fault, \
         often one in another process; trying again, or using the CPU engine, \
         avoids it"
    )
}

thread_local! {
    /// Why the last confirmation on this thread failed, where the backend
    /// said, for [`discarded`]; cleared by every confirmation that passes, so
    /// that it never names an earlier attempt's failure. A thread's GPU
    /// context is used on that thread alone, so the draw, its confirmation
    /// and the error are made on one thread.
    static FAILURE: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Where a read began, for the confirmation after it: on Metal, a mark on the
/// queue of ours; with no Metal there is no such queue, and never a mark.
#[cfg(feature = "metal")]
type Since = Option<Mark>;
#[cfg(all(feature = "vulkan", not(feature = "metal")))]
type Since = Option<Infallible>;

/// Confirms every command buffer `context`'s queue of ours has seen committed,
/// where it has one, and notes why for [`discarded`] where that fails. With
/// `since`, where a read began, the read must have committed one. `true` with
/// no queue of ours: Vulkan, and any context Skia makes its buffers for
/// itself, is confirmed by Skia's wait alone.
#[cfg(any(feature = "metal", feature = "vulkan"))]
fn confirmed(context: &DirectContext, since: Since) -> bool {
    #[cfg(feature = "metal")]
    let answer =
        metal::recording(context).map_or(Ok(()), |queue| queue.confirm(since));
    #[cfg(not(feature = "metal"))]
    let answer: Result<(), String> = {
        let _ = (context, since);
        Ok(())
    };
    let ok = answer.is_ok();
    FAILURE.set(answer.err());
    ok
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
    // A flush that failed left the draws unrun, whatever the submit says:
    // this one, or one the playback made on the way -- see `pacing`.
    let flushed = context.flush(None).is_ok() && !pacing::take_failed();
    let ran = context.submit(SyncCpu::Yes);
    flushed && ran && confirmed(&context, None) && !seam::discards(page)
}

/// With no GPU backend compiled in every surface is raster, so there is no
/// draw the GPU could have discarded.
#[cfg(not(any(feature = "metal", feature = "vulkan")))]
pub(crate) fn draws_ran(_surface: &mut Surface, _page: usize) -> bool {
    true
}

/// `read` of what `surface` holds, made after its draws and confirmed with
/// them: `None` where the GPU did not run the draws or the read, so that the
/// caller draws again, and the read's own answer otherwise. `page` is the
/// page the draws belong to, which only the test seam reads.
///
/// On Metal the draws are committed without a wait, the read is made, and
/// both are confirmed after it through the queue of ours Skia makes every
/// buffer on. Skia's read commits the copy in a buffer of its own, waits on
/// it and drops the answer; the queue holds the draws' buffer and the copy's
/// and reads their status, so one confirmation after the read answers for
/// both. Elsewhere the draws are confirmed by [`draws_ran`] before the read,
/// which cannot see the copy.
#[cfg(any(feature = "metal", feature = "vulkan"))]
pub(crate) fn read_after_draws<T>(
    surface: &mut Surface,
    page: usize,
    read: impl FnOnce(&mut Surface) -> Result<T, String>,
) -> Option<Result<T, String>> {
    // A raster surface has no context, and nothing of its read to confirm.
    // It is the only surface without one this reaches: a Graphite surface,
    // which has no direct context either, would be read here unconfirmed.
    let Some(context) = surface.direct_context() else {
        return Some(read(surface));
    };
    // UPSTREAM: skia-safe 0.156.0 -- unfiled -- worked around
    // Re-check: cargo test an_export_whose_copy_was_discarded_is_read_again
    // with `metal` and the confirmation below made before `read`.
    //
    // Skia M156's `GrMtlGpu::onReadPixels`, in `skia/src/gpu/ganesh/mtl/
    // GrMtlGpu.mm` under the skia-bindings crate, waits on the command buffer
    // that copies the texture out with `submitCommandBuffer(kForce_SyncQueue)`
    // and drops the answer, so a copy the GPU discarded hands back zeros or
    // stale bytes as success. It is told here, after the read, from the
    // buffer's status as the queue of ours reads it.
    #[cfg(feature = "metal")]
    if let Some(queue) = metal::recording(&context) {
        let mut context = context;
        let since = queue.mark();
        // The draws reach the GPU before the read is encoded, in a buffer of
        // their own that nothing waits on, as `read_checked` commits them and
        // for its reason: committed with the copy, they hold Apple's GPU at
        // its lowest clock while reads follow draws.
        let flushed = context.flush(None).is_ok() && !pacing::take_failed();
        if !(flushed && context.submit(SyncCpu::No)) {
            return None;
        }
        let answer = read(surface);
        // A read that failed answers with its own error, which says more
        // than a confirmation would; confirmed all the same, so that a
        // failure the read's buffers met is not left for the next one.
        if answer.is_err() {
            confirmed(&context, None);
            return Some(answer);
        }
        let ran = confirmed(&context, Some(since)) && !seam::discards(page);
        return ran.then_some(answer);
    }
    drop(context);
    if !draws_ran(surface, page) {
        return None;
    }
    Some(read(surface))
}

/// [`read_after_draws`] for a read that answers with what it made rather
/// than why it failed: a copy of the surface, or whether a read of it was
/// made. `None` where the GPU did not run the draws or the copy, so that the
/// caller draws again.
pub(crate) fn read_off_gpu<T>(
    surface: &mut Surface,
    page: usize,
    read: impl FnOnce(&mut Surface) -> T,
) -> Option<T> {
    match read_after_draws(surface, page, |surface| {
        Ok::<_, String>(read(surface))
    }) {
        Some(Ok(answer)) => Some(answer),
        // The read is handed back `Ok` whatever it made, so only a
        // confirmation that failed answers without one.
        Some(Err(_)) | None => None,
    }
}

/// With no GPU backend compiled in every surface is raster, so `read` is
/// made as it is.
#[cfg(not(any(feature = "metal", feature = "vulkan")))]
pub(crate) fn read_after_draws<T>(
    surface: &mut Surface,
    _page: usize,
    read: impl FnOnce(&mut Surface) -> Result<T, String>,
) -> Option<Result<T, String>> {
    Some(read(surface))
}

/// What [`read_checked`] made of a read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(
    not(any(feature = "metal", feature = "vulkan")),
    expect(dead_code, reason = "only a GPU read is discarded or declined")
)]
pub(crate) enum Checked {
    /// The pixels are read, and on the GPU the draws before the read and the
    /// copy out are confirmed to have run. `false` where the surface refused
    /// the read, which a GPU discard is not.
    Read(bool),
    /// The GPU did not run the draws or the copy, so what the destination
    /// holds is unknown.
    Discarded,
    /// Skia would not read the surface this way, and nothing was confirmed:
    /// the caller confirms the draws with [`draws_ran`] and reads as it would
    /// without this.
    Declined,
}

/// How a read's rows are written into the destination: converted on the way
/// rather than copied, so that a conversion made on the CPU is one pass over
/// the rows rather than a copy and then a pass.
#[derive(Clone, Copy)]
pub(crate) struct Rows {
    /// Writes a row as read into a destination row of the same length.
    #[cfg_attr(
        not(any(feature = "metal", feature = "vulkan")),
        expect(
            dead_code,
            reason = "only a GPU read lands in a buffer of its own"
        )
    )]
    pub(crate) into: fn(&[u8], &mut [u8]),
    /// Converts a row already in the destination where it lies, for a read
    /// that writes the destination itself.
    pub(crate) in_place: fn(&mut [u8]),
    /// Whether the rows are shared across threads.
    pub(crate) parallel: bool,
}

/// The bytes of rows one thread converts before the pool hands it more, where
/// a read's rows are shared across threads.
///
/// A row is cheap to convert -- the exact unpremultiply takes about 0.6
/// microseconds for a 3,200-byte row -- so a task a row leaves the pool
/// handing work over more than doing it. Measured in release on Apple silicon
/// (M4 Pro, 14 threads), an 800x600 page of translucent pixels unpremultiplied
/// across the pool, median of 61 passes, three runs: a row a task 109 to 135
/// microseconds, 4 rows 92 to 99, 16 rows 78 to 86, 38 rows 101 to 107, 150
/// rows 149 to 152. 48 KiB rounds up to those 16 rows at that width, and other
/// widths take the rows that make the same bytes.
///
/// In place, a whole `getImageData` of a GPU page after a 1-pixel draw, median
/// of ten rounds against a row a task: 400 microseconds against 428 at
/// 512x512, 454 against 510 at 800x600, 606 against 646 at 1200x900 and 928
/// against 982 at 1920x1080, faster in every round at every size. 512x512 is
/// [`PARALLEL_UNPREMULTIPLY_PIXELS`](crate::context::page::PARALLEL_UNPREMULTIPLY_PIXELS),
/// the smallest read that reaches this; a smaller one is converted on one
/// thread.
pub(crate) const PARALLEL_TASK_BYTES: usize = 48 << 10;

/// The bytes of a plain copy -- a read's rows written as Skia hands them back
/// -- one thread makes before the pool hands it more. A copy no larger is made
/// on the calling thread.
///
/// A copy runs at memory speed, so a task has to be far larger than a
/// conversion's [`PARALLEL_TASK_BYTES`] before handing it over pays. Measured
/// in release on Apple silicon (M4 Pro, 14 threads), a whole premultiplied
/// `getImageData` of a GPU page after a 1-pixel draw, median of ten rounds,
/// tasks of 1, 2 and 4 MiB against the copy on one thread: 334, 344 and 366
/// microseconds against 362 at 800x600, 431, 478 and 460 against 486 at
/// 1024x768, 543, 546 and 605 against 617 at 1200x900, 797, 813 and 938
/// against 938 at 1440x1080, and 1008, 1038 and 1199 against 1222 at
/// 1920x1080. A size that makes one task is copied on one thread, so a figure
/// where it does is one thread's. 1 MiB is faster than one thread in every
/// round from 800x600 up; at 512x512, one 1 MiB task, every way is one thread,
/// 258 to 270 microseconds.
#[cfg(any(feature = "metal", feature = "vulkan"))]
pub(crate) const PARALLEL_COPY_TASK_BYTES: usize = 1 << 20;

/// How many rows of `row_bytes` make a [`PARALLEL_TASK_BYTES`] task: at least
/// one, however wide the row.
fn rows_per_task(row_bytes: usize) -> usize {
    PARALLEL_TASK_BYTES.div_ceil(row_bytes.max(1))
}

impl Rows {
    /// Converts `height` rows of `width` bytes of `pixels`, `row_bytes`
    /// apart, where they lie.
    pub(crate) fn convert_in_place(
        self,
        pixels: &mut [u8],
        row_bytes: usize,
        width: usize,
        height: usize,
    ) {
        let in_place = self.in_place;
        match self.parallel {
            true => pixels
                .par_chunks_mut(row_bytes)
                .take(height)
                .with_min_len(rows_per_task(row_bytes))
                .for_each(|row| in_place(&mut row[..width])),
            false => pixels
                .chunks_mut(row_bytes)
                .take(height)
                .for_each(|row| in_place(&mut row[..width])),
        }
    }
}

/// Reads `info`'s rectangle at `at` of `surface` into `pixels`, `row_bytes`
/// a row, with the GPU's draws confirmed by the read's own wait rather than
/// one in front of it.
///
/// The copy is asked for through Skia's asynchronous read and committed and
/// waited on with `submit(SyncCpu::Yes)`, whose answer is its buffer's
/// status. On Metal the draws before it are committed first, in a buffer of
/// their own that nothing waits on, and the queue of ours Skia made both on
/// answers for that one -- see [`metal::RecordingQueue`]: one wait, and an
/// answer for the draws as well as the copy. Elsewhere the copy is encoded in
/// the buffer the draws are already in, which the one wait answers for. A
/// raster surface reads at once and has nothing to confirm. `page` is the
/// page the draws belong to, which only the test seam reads. `rows`, where
/// given, converts each row as it is written.
#[cfg(any(feature = "metal", feature = "vulkan"))]
pub(crate) fn read_checked(
    surface: &mut Surface,
    info: &ImageInfo,
    pixels: &mut [u8],
    row_bytes: usize,
    at: (i32, i32),
    page: usize,
    rows: Option<Rows>,
) -> Checked {
    let Some(mut context) = surface.direct_context() else {
        return Checked::Read(read_now(
            surface, info, pixels, row_bytes, at, rows,
        ));
    };
    #[cfg(feature = "metal")]
    let since = match metal::recording(&context) {
        // The draws reach the GPU before the read is encoded. Committed with
        // the copy in one buffer, they keep Apple's GPU at its lowest clock
        // for as long as reads follow draws: over 20 s of whole 1200x900
        // reads after a small draw on an M4 Pro, 338 MHz throughout and 723
        // microseconds a read, against 480 MHz and 674 committed first. A
        // buffer of ours committed at this point with no draws in it leaves
        // the clock where it was, so it is the draws that have to go.
        Some(queue) => {
            let since = queue.mark();
            let flushed = context.flush(None).is_ok() && !pacing::take_failed();
            if !(flushed && context.submit(SyncCpu::No)) {
                return Checked::Discarded;
            }
            #[cfg(test)]
            EARLY.set(EARLY.get() + 1);
            Some(since)
        }
        None => None,
    };
    #[cfg(all(feature = "vulkan", not(feature = "metal")))]
    let since: Since = None;
    // The outer `None` until Skia calls back; the inner one where it declined.
    let answer: Rc<RefCell<Option<Option<AsyncReadResult>>>> = Rc::default();
    let sink = Rc::clone(&answer);
    surface.async_rescale_and_read_pixels(
        info,
        IRect::from_pt_size(at, info.dimensions()),
        RescaleGamma::Src,
        RescaleMode::Nearest,
        move |read| *sink.borrow_mut() = Some(read),
    );
    // An answer before anything is submitted is Skia declining, or reading
    // through its own `readPixels` where it could not make a transfer
    // (`SurfaceContext::asyncReadPixels`), which submits the draws in a
    // buffer of their own that nothing waits on. Neither is confirmed here;
    // the caller's `draws_ran` confirms the draws, early buffer and all.
    if answer.borrow().is_some() {
        #[cfg(test)]
        DECLINED.set(DECLINED.get() + 1);
        return Checked::Declined;
    }
    // Asked as `draws_ran` asks it: every draw on the context, and a flush
    // that failed on the way.
    let flushed = context.flush(None).is_ok() && !pacing::take_failed();
    let ran = context.submit(SyncCpu::Yes);
    #[cfg(test)]
    SYNCED.set(SYNCED.get() + 1);
    if !(flushed && ran && confirmed(&context, since)) || seam::discards(page) {
        return Checked::Discarded;
    }
    match answer.borrow_mut().take() {
        Some(Some(read)) => {
            Checked::Read(copy_rows(&read, info, pixels, row_bytes, rows))
        }
        _ => {
            #[cfg(test)]
            DECLINED.set(DECLINED.get() + 1);
            Checked::Declined
        }
    }
}

#[cfg(all(test, any(feature = "metal", feature = "vulkan")))]
thread_local! {
    /// How many GPU reads [`read_checked`] has handed back undone on this
    /// thread, so that a test can tell a read it confirmed from one that
    /// took the two-wait path.
    pub(crate) static DECLINED: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
    /// How many times [`read_checked`] has waited on the GPU through Skia on
    /// this thread.
    pub(crate) static SYNCED: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
    /// How many GPU reads [`read_checked`] has committed the draws for before
    /// encoding the read, on this thread.
    #[cfg(feature = "metal")]
    pub(crate) static EARLY: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// With no GPU backend compiled in every surface is raster: read at once.
#[cfg(not(any(feature = "metal", feature = "vulkan")))]
pub(crate) fn read_checked(
    surface: &mut Surface,
    info: &ImageInfo,
    pixels: &mut [u8],
    row_bytes: usize,
    at: (i32, i32),
    _page: usize,
    rows: Option<Rows>,
) -> Checked {
    Checked::Read(read_now(surface, info, pixels, row_bytes, at, rows))
}

/// A read with nothing to confirm, made by the surface into `pixels` and
/// then converted by `rows` where they lie.
fn read_now(
    surface: &mut Surface,
    info: &ImageInfo,
    pixels: &mut [u8],
    row_bytes: usize,
    at: (i32, i32),
    rows: Option<Rows>,
) -> bool {
    let read = surface.read_pixels(info, pixels, row_bytes, at);
    if let Some(rows) = rows.filter(|_| read) {
        let height = info.height() as usize;
        rows.convert_in_place(pixels, row_bytes, info.min_row_bytes(), height);
    }
    read
}

/// Writes the one plane of `read`, `info`'s rows, into `pixels` at
/// `row_bytes` a row: copied, or converted by `rows` on the way. `false` where
/// it is not one plane of at least `info`'s width, or `pixels` cannot hold
/// it.
///
/// A plain copy is shared across threads in tasks of
/// [`PARALLEL_COPY_TASK_BYTES`] once it makes more than one, and a conversion
/// in tasks of [`PARALLEL_TASK_BYTES`] where `rows` says it is shared.
#[cfg(any(feature = "metal", feature = "vulkan"))]
fn copy_rows(
    read: &AsyncReadResult,
    info: &ImageInfo,
    pixels: &mut [u8],
    row_bytes: usize,
    rows: Option<Rows>,
) -> bool {
    let width = info.min_row_bytes();
    let height = info.height() as usize;
    if height == 0 {
        return true;
    }
    if read.count() != 1
        || read.row_bytes(0) < width
        || pixels.len() < (height - 1) * row_bytes + width
    {
        return false;
    }
    let stride = read.row_bytes(0);
    // SAFETY: Skia's result holds `height` rows of `stride` bytes laid out as
    // the `info` it was asked for, valid while `read` lives -- the borrow
    // here -- and the slice ends at the last row's `width` bytes, no more than
    // its `stride`.
    let plane = unsafe {
        std::slice::from_raw_parts(
            read.data(0).cast::<u8>(),
            (height - 1) * stride + width,
        )
    };
    let write = rows.map_or(
        (|from, to| to.copy_from_slice(from)) as fn(&[u8], &mut [u8]),
        |rows| rows.into,
    );
    let pairs = |(to, from): (&mut [u8], &[u8])| {
        write(&from[..width], &mut to[..width])
    };
    let per_task = match rows {
        Some(rows) => rows.parallel.then(|| rows_per_task(row_bytes)),
        None => {
            let per = PARALLEL_COPY_TASK_BYTES.div_ceil(row_bytes.max(1));
            (height > per).then_some(per)
        }
    };
    match per_task {
        Some(per_task) => pixels
            .par_chunks_mut(row_bytes)
            .zip(plane.par_chunks(stride))
            .with_min_len(per_task)
            .for_each(pairs),
        None => pixels
            .chunks_mut(row_bytes)
            .zip(plane.chunks(stride))
            .for_each(pairs),
    }
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
    use super::{
        DECLINED, DISCARDED, PARALLEL_TASK_BYTES, rows_per_task, seam,
    };
    use crate::{
        Canvas, CanvasOptions, Context2D, EncodeOptions, Error, FillRule,
        ImageFormat, PixelColorSpace, PixelDepth, PixelExportOptions,
        RgbaLinear, canvas::EngineKind,
        context::page::PARALLEL_UNPREMULTIPLY_PIXELS,
    };

    /// The width the task size was measured at gets the rows measured best
    /// there, a row wider than a task is a task of its own, and a zero width
    /// still makes tasks of some rows.
    #[test]
    fn a_task_holds_the_rows_measured_best() {
        assert_eq!(rows_per_task(800 * 4), 16);
        assert_eq!(rows_per_task(PARALLEL_TASK_BYTES + 1), 1);
        assert_eq!(rows_per_task(0), PARALLEL_TASK_BYTES);
    }

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

    /// A read after a draw, on every page kind the GPU draws and at the
    /// depths it is read at, whole, in part and within a tile, is confirmed
    /// by its own wait -- Skia hands none of them back to the two-wait path
    /// -- and reads what a repeat read of the same state reads: one served
    /// from a CPU copy of the page, made by Skia's own readback. A float
    /// page is drawn on the CPU, which has nothing to confirm.
    #[test]
    fn a_read_after_a_draw_is_confirmed_by_its_own_wait() {
        if drawn(1.0, 1.0).is_none() {
            return;
        }
        let mut checked = 0;
        for space in PixelColorSpace::all() {
            for depth in [PixelDepth::Uint8, PixelDepth::F16, PixelDepth::F32] {
                for read in [PixelDepth::Uint8, PixelDepth::F32] {
                    for (x, y, w, h) in [
                        (0.0, 0.0, 600.0, 300.0),
                        (100.0, 50.0, 300.0, 200.0),
                        (37.0, 11.0, 10.0, 10.0),
                    ] {
                        for premultiplied in [false, true] {
                            let mut canvas = Canvas::with_options(
                                600.0,
                                300.0,
                                CanvasOptions {
                                    color_space: space,
                                    color_type: depth,
                                    ..CanvasOptions::default()
                                },
                            )
                            .expect("a canvas");
                            if canvas.engine_kind() != EngineKind::Gpu {
                                continue;
                            }
                            let ctx = canvas.context();
                            ctx.set_fill_style(RgbaLinear::from_srgb8(
                                204, 77, 26, 0.45,
                            ));
                            ctx.fill_rect(20.0, 5.0, 500.0, 250.0);
                            ctx.set_fill_style(RgbaLinear::from_srgb8(
                                26, 128, 230, 0.7,
                            ));
                            ctx.begin_path();
                            ctx.arc(300.0, 150.0, 110.3, 0.0, 7.0, false)
                                .expect("an arc");
                            ctx.fill(FillRule::NonZero);
                            let options = PixelExportOptions {
                                color_space: space,
                                depth: read,
                                premultiplied,
                                ..PixelExportOptions::default()
                            };
                            let before = DECLINED.get();
                            let first = ctx
                                .get_image_data_as(x, y, w, h, options)
                                .expect("a read");
                            assert_eq!(
                                DECLINED.get(),
                                before,
                                "{space:?} {depth:?} read as {read:?} at \
                                 {x},{y} {w}x{h}, premultiplied \
                                 {premultiplied}: Skia declined the read"
                            );
                            let again = ctx
                                .get_image_data_as(x, y, w, h, options)
                                .expect("a repeat read");
                            assert!(
                                first.pixels() == again.pixels(),
                                "{space:?} {depth:?} read as {read:?} at \
                                 {x},{y} {w}x{h}, premultiplied \
                                 {premultiplied}: the read differs from a \
                                 repeat of it"
                            );
                            checked += 1;
                        }
                    }
                }
            }
        }
        // Each space's eight-bit page, read as eight bits and as floats.
        assert_eq!(checked, 8 * 2 * 3 * 2);
    }

    /// A read large enough to be unpremultiplied across threads, as it is
    /// written from the GPU's copy, lands what a repeat of it does.
    #[test]
    fn a_large_read_is_unpremultiplied_as_its_repeat_is() {
        let (width, height) = (1024.0, 600.0);
        assert!(
            (width * height) as usize >= PARALLEL_UNPREMULTIPLY_PIXELS,
            "the read is not large enough to be shared across threads"
        );
        let mut canvas = Canvas::new(width, height);
        if canvas.engine_kind() != EngineKind::Gpu {
            return;
        }
        let ctx = canvas.context();
        ctx.set_fill_style(RgbaLinear::from_srgb8(204, 77, 26, 0.45));
        ctx.fill_rect(20.0, 5.0, 900.0, 550.0);
        ctx.set_fill_style(RgbaLinear::from_srgb8(26, 128, 230, 0.7));
        ctx.begin_path();
        ctx.arc(500.0, 300.0, 210.3, 0.0, 7.0, false)
            .expect("an arc");
        ctx.fill(FillRule::NonZero);
        let before = DECLINED.get();
        let first =
            ctx.get_image_data(0.0, 0.0, width, height).expect("a read");
        assert_eq!(DECLINED.get(), before, "Skia declined the read");
        let again = ctx
            .get_image_data(0.0, 0.0, width, height)
            .expect("a repeat");
        assert!(
            first.pixels() == again.pixels(),
            "the read differs from a repeat"
        );
    }

    /// A read large enough for its plain copy to be shared across threads
    /// lands what the same rows read in three reads too small for that do.
    #[test]
    fn a_large_plain_read_is_its_parts_read_apart() {
        // Three copy tasks' worth of rows, read whole and in thirds, each
        // third one task or less.
        let width = 1024.0;
        let rows = 3 * super::PARALLEL_COPY_TASK_BYTES.div_ceil(1024 * 4);
        let (height, part) = (rows as f32, (rows / 3) as f32);
        assert!(
            (width * height * 4.0) as usize > super::PARALLEL_COPY_TASK_BYTES
        );
        assert!(
            ((width * part * 4.0) as usize) <= super::PARALLEL_COPY_TASK_BYTES
        );
        let mut canvas = Canvas::new(width, height);
        if canvas.engine_kind() != EngineKind::Gpu {
            return;
        }
        let ctx = canvas.context();
        ctx.set_fill_style(RgbaLinear::from_srgb8(204, 77, 26, 0.45));
        ctx.fill_rect(20.0, 5.0, 900.0, 550.0);
        ctx.set_fill_style(RgbaLinear::from_srgb8(26, 128, 230, 0.7));
        ctx.begin_path();
        ctx.arc(500.0, 300.0, 210.3, 0.0, 7.0, false)
            .expect("an arc");
        ctx.fill(FillRule::NonZero);
        let premultiplied = PixelExportOptions {
            premultiplied: true,
            ..PixelExportOptions::default()
        };
        let before = DECLINED.get();
        let whole = ctx
            .get_image_data_as(0.0, 0.0, width, height, premultiplied)
            .expect("a whole read");
        let parts: Vec<u8> = (0..3)
            .flat_map(|i| {
                ctx.get_image_data_as(
                    0.0,
                    part * i as f32,
                    width,
                    part,
                    premultiplied,
                )
                .expect("a part")
                .pixels()
                .to_vec()
            })
            .collect();
        assert_eq!(DECLINED.get(), before, "Skia declined a read");
        assert!(
            whole.pixels() == parts.as_slice(),
            "the whole read differs from its parts"
        );
    }

    #[test]
    fn a_tile_drawn_into_after_its_confirmation_is_confirmed_again() {
        let Some((mut canvas, page)) = drawn(1024.0, 1024.0) else {
            return;
        };
        let ctx = canvas.context();
        let first = ctx.get_image_data(0.0, 0.0, 8.0, 8.0).expect("read");
        // Nothing drawn since that read confirmed the tile, but the repeat
        // copies the tile off the GPU to serve later reads from, and that
        // copy is confirmed: discarded, it is read again.
        seam::discard(Some(page), 1);
        let again = ctx.get_image_data(0.0, 0.0, 8.0, 8.0).expect("read");
        assert_eq!(again.pixels(), first.pixels());
        assert_eq!(seam::unused(page), 0, "the tile's copy went unconfirmed");
        // The read again drew the tile afresh, so the next is the repeat
        // that makes its copy. Served from that copy, a read asks the GPU
        // nothing -- and an armed discard stays unused.
        ctx.get_image_data(0.0, 0.0, 8.0, 8.0).expect("read");
        seam::discard(Some(page), 2);
        let served = ctx.get_image_data(0.0, 0.0, 8.0, 8.0).expect("read");
        assert_eq!(served.pixels(), first.pixels());
        assert_eq!(seam::unused(page), 2, "a read of the tile's copy asked");
        // A draw into the tile re-arms the check, which the discards refuse.
        ctx.fill_rect(0.0, 0.0, 4.0, 4.0);
        let read = ctx.get_image_data(0.0, 0.0, 8.0, 8.0);
        assert_discarded(read, "drawing tiles for a read");
    }

    #[test]
    fn a_conversion_discarded_twice_is_an_error() {
        // A P3 export of an sRGB canvas redraws the composited page into a
        // P3 surface, which is a second draw to confirm. On Metal the
        // conversion is confirmed inside the read, before the composite is,
        // so the first two draws asked about are the conversion's; elsewhere
        // the composite is confirmed before the read, and is let through.
        let Some((mut canvas, page)) = drawn(64.0, 48.0) else {
            return;
        };
        let composite_first = usize::from(!cfg!(feature = "metal"));
        seam::discard_after(Some(page), composite_first, 2);
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

    /// A read's draws, committed before the read in a buffer nothing waits
    /// on, confirmed through the queue of ours Skia makes its buffers on.
    #[cfg(feature = "metal")]
    mod early {
        use objc2::{
            ProtocolType,
            rc::Retained,
            runtime::{NSObjectProtocol, ProtocolObject, Sel},
            sel,
        };
        use objc2_metal::{
            MTLCommandQueue, MTLCreateSystemDefaultDevice, MTLDevice,
        };

        use skia_safe::{
            AlphaType, Color, ColorType, ImageInfo,
            gpu::{Budgeted, SurfaceOrigin, surfaces},
        };

        use super::{DISCARDED, drawn};
        use crate::{
            Canvas, Error, RgbaLinear,
            gpu::{
                completion::{Checked, EARLY, SYNCED, draws_ran, read_checked},
                metal::{
                    self, BLIND, BLIND_FOR_TEST, BUFFER_DISCARDED, FINISHED,
                    MetalEngine, RecordingQueue, WAITED,
                },
            },
        };

        /// The page read whole rather than from tiles, and what that read
        /// is called when it fails.
        const SIZE: (f32, f32) = (600.0, 300.0);
        const STAGE: &str = "drawing the page for a read";

        /// This thread's offscreen context's queue of ours.
        fn queue() -> Retained<RecordingQueue> {
            let mut queue = None;
            MetalEngine::with_direct_context(|context| {
                queue = context.and_then(|context| metal::recording(context));
            });
            queue.expect("the offscreen context makes its buffers on ours")
        }

        /// A drawn page whose earlier buffers are all confirmed, with a
        /// square drawn since: the next read has draws to commit early.
        fn ready() -> Option<Canvas> {
            let (mut canvas, _) = drawn(SIZE.0, SIZE.1)?;
            let ctx = canvas.context();
            ctx.get_image_data(0.0, 0.0, SIZE.0, SIZE.1)
                .expect("a read");
            ctx.set_fill_style(RgbaLinear::opaque(0.9, 0.2, 0.1));
            ctx.fill_rect(10.0, 10.0, 40.0, 40.0);
            Some(canvas)
        }

        fn read(canvas: &mut Canvas) -> Result<Vec<u8>, Error> {
            canvas
                .context()
                .get_image_data(0.0, 0.0, SIZE.0, SIZE.1)
                .map(|data| data.pixels().to_vec())
        }

        /// That `got` is the error for a whole-page read discarded twice,
        /// giving `because` as the reason.
        fn assert_discarded_because(
            got: Result<Vec<u8>, Error>,
            because: &str,
        ) {
            match got {
                Err(Error::Render { reason }) => {
                    assert!(reason.starts_with(DISCARDED), "{reason}");
                    assert!(reason.contains(STAGE), "{reason}");
                    assert!(reason.contains(because), "{reason}");
                }
                other => panic!("expected a discarded read, got {other:?}"),
            }
        }

        #[test]
        fn a_discarded_early_buffer_fails_the_read() {
            let Some(mut canvas) = ready() else {
                return;
            };
            let Some(mut reference) = ready() else {
                return;
            };
            let want = read(&mut reference).expect("reference");
            // The first buffer Skia asks for after this is the early one: the
            // last read's was committed, and drawing asks for none.
            let queue = queue();
            queue.fail_after(0, 1);
            let early = EARLY.get();
            let got = read(&mut canvas).expect("the retry succeeds");
            assert_eq!(queue.unused_failures(), 0, "no buffer was handed out");
            assert_eq!(
                EARLY.get() - early,
                2,
                "the failed early buffer was not caught, so the page was not \
                 drawn again"
            );
            assert_eq!(got, want);
        }

        #[test]
        fn a_buffer_discarded_twice_is_an_error_quoting_metal() {
            let Some(mut canvas) = ready() else {
                return;
            };
            let queue = queue();
            queue.fail_after(0, usize::MAX);
            let got = read(&mut canvas);
            queue.fail_after(0, 0);
            assert_discarded_because(got, BUFFER_DISCARDED);
        }

        /// A read with draws before it commits two buffers, the draws' and
        /// the copy's, and waits through Skia once.
        #[test]
        fn a_read_commits_its_draws_first_and_waits_once() {
            let Some(mut canvas) = ready() else {
                return;
            };
            let (early, synced, waited) =
                (EARLY.get(), SYNCED.get(), WAITED.get());
            read(&mut canvas).expect("a read after a draw");
            // Reported rather than asserted: Metal does not say that a later
            // buffer finishing means an earlier one has.
            eprintln!(
                "buffers waited for after Skia's wait: {}",
                WAITED.get() - waited
            );
            assert_eq!(EARLY.get() - early, 1, "no early commit");
            assert_eq!(SYNCED.get() - synced, 1, "not one wait");
            assert_eq!(
                FINISHED.get(),
                2,
                "not the draws' buffer and the copy's"
            );
        }

        /// A read with nothing drawn since its surface's draws were confirmed
        /// commits no early buffer, Skia committing nothing for an empty
        /// submit (`GrMtlGpu::submitCommandBuffer`, `GrMtlGpu.mm` lines 212
        /// to 224), and still commits the copy's: one buffer, so it never
        /// reads as a queue that has stopped seeing buffers. Made on a bare
        /// surface, since a page read replays the page into it first.
        #[test]
        fn a_read_with_nothing_drawn_still_commits_the_copy() {
            if drawn(1.0, 1.0).is_none() {
                return;
            }
            let info = ImageInfo::new(
                (64, 48),
                ColorType::RGBA8888,
                AlphaType::Premul,
                None,
            );
            let mut surface = None;
            MetalEngine::with_direct_context(|context| {
                surface = context.and_then(|context| {
                    surfaces::render_target(
                        context,
                        Budgeted::Yes,
                        &info,
                        None,
                        SurfaceOrigin::BottomLeft,
                        None,
                        false,
                        None,
                    )
                });
            });
            let mut surface = surface.expect("a GPU surface");
            surface.canvas().clear(Color::from_argb(255, 26, 102, 204));
            assert!(draws_ran(&mut surface, 0), "the clear did not run");
            let mut pixels = vec![0; info.compute_min_byte_size()];
            let early = EARLY.get();
            let checked = read_checked(
                &mut surface,
                &info,
                &mut pixels,
                info.min_row_bytes(),
                (0, 0),
                0,
                None,
            );
            assert_eq!(checked, Checked::Read(true));
            assert_eq!(EARLY.get() - early, 1, "the read was not checked");
            assert_eq!(FINISHED.get(), 1, "not the copy's buffer alone");
            assert_eq!(&pixels[..4], &[26, 102, 204, 255], "not the clear");
        }

        #[test]
        fn a_queue_that_sees_no_buffers_fails_the_read() {
            let Some(mut canvas) = ready() else {
                return;
            };
            BLIND_FOR_TEST.set(true);
            let got = read(&mut canvas);
            BLIND_FOR_TEST.set(false);
            assert_discarded_because(got, BLIND);
        }

        /// The queue of ours answers as Metal's does for every message of
        /// the protocol: those Skia M156 sends (`commandBuffer`, and `label`
        /// in its JSON dump), `commandBufferWithDescriptor:`, which Skia
        /// sends once rust-skia stops patching out its `@available` check,
        /// and the rest.
        #[test]
        fn the_queue_of_ours_answers_as_metal_s_does() {
            let Some(device) = MTLCreateSystemDefaultDevice() else {
                eprintln!("skipped: no Metal device");
                return;
            };
            let inner = device.newCommandQueue().expect("a queue");
            let ours = RecordingQueue::new(inner.clone());
            let ours: &ProtocolObject<dyn MTLCommandQueue> =
                ProtocolObject::from_ref(&*ours);
            let protocol = <dyn MTLCommandQueue>::protocol()
                .expect("MTLCommandQueue is registered");
            assert!(inner.conformsToProtocol(protocol));
            assert!(ours.conformsToProtocol(protocol));
            let selectors: [Sel; 11] = [
                sel!(commandBuffer),
                sel!(commandBufferWithDescriptor:),
                sel!(commandBufferWithUnretainedReferences),
                sel!(label),
                sel!(setLabel:),
                sel!(device),
                sel!(insertDebugCaptureBoundary),
                sel!(addResidencySet:),
                sel!(addResidencySets:count:),
                sel!(removeResidencySet:),
                sel!(removeResidencySets:count:),
            ];
            for selector in selectors {
                assert_eq!(
                    ours.respondsToSelector(selector),
                    inner.respondsToSelector(selector),
                    "{selector:?}"
                );
            }
            assert!(ours.commandBuffer().is_some());
        }
    }

    /// An export's read, confirmed after it through the queue of ours Skia
    /// makes every buffer on: the draws' buffer and the copy's alike.
    #[cfg(feature = "metal")]
    mod exports {
        use objc2::rc::Retained;
        use skia_safe::{AlphaType, ColorType, ImageInfo};

        use super::{DISCARDED, drawn};
        use crate::{
            Canvas,
            context::page::{ExportOptions, Page, PageCache},
            gpu::{
                RenderingEngine,
                metal::{self, ISSUED, MetalEngine, RecordingQueue},
            },
        };

        /// The page, small enough that an export is one draw and one copy.
        const SIZE: (f32, f32) = (64.0, 48.0);

        /// This thread's offscreen context's queue of ours. The exports here
        /// run on this thread, rather than on an owner, so that this is the
        /// queue their buffers come from.
        fn queue() -> Retained<RecordingQueue> {
            let mut queue = None;
            MetalEngine::with_direct_context(|context| {
                queue = context.and_then(|context| metal::recording(context));
            });
            queue.expect("the offscreen context makes its buffers on ours")
        }

        /// A drawn page, on a machine with a GPU engine, and the canvas it
        /// is from, which has to live for an export to file the page in the
        /// page cache.
        fn page() -> Option<(Canvas, Page)> {
            let (mut canvas, _) = drawn(SIZE.0, SIZE.1)?;
            let mut page = None;
            canvas
                .context()
                .inner
                .with_recorder(|mut recorder| page = Some(recorder.page()));
            Some((canvas, page?))
        }

        /// How many buffers `export` of a drawn page commits: the last is the
        /// one its copy is encoded in.
        fn buffers(export: Export) -> usize {
            let (_canvas, page) = page().expect("a GPU engine");
            let before = ISSUED.get();
            export(&page).expect("an export");
            ISSUED.get() - before
        }

        /// An export of a page into a buffer of pixels.
        type Export = fn(&Page) -> Result<Vec<u8>, String>;

        /// The three ways an export takes a page off the GPU: as an image,
        /// read into a buffer, and composited whole into a buffer.
        fn exports() -> [(&'static str, Export); 3] {
            fn info() -> ImageInfo {
                ImageInfo::new(
                    (SIZE.0 as i32, SIZE.1 as i32),
                    ColorType::RGBA8888,
                    AlphaType::Premul,
                    None,
                )
            }
            [
                ("composite", |page| {
                    let image = page.composite(
                        &ExportOptions::default(),
                        RenderingEngine::GPU,
                    )?;
                    let info = info();
                    let mut pixels = vec![0; info.compute_min_byte_size()];
                    image.read_pixels(
                        &info,
                        &mut pixels,
                        info.min_row_bytes(),
                        (0, 0),
                        skia_safe::image::CachingHint::Disallow,
                    );
                    Ok(pixels)
                }),
                ("composite_into", |page| {
                    page.composite_into(
                        &ExportOptions::default(),
                        RenderingEngine::GPU,
                        &info(),
                    )
                }),
                ("composite_pixels", |page| {
                    page.composite_pixels(
                        &ExportOptions::default(),
                        RenderingEngine::GPU,
                        &info(),
                    )
                }),
            ]
        }

        /// An export whose copy Metal discarded is read again, and the
        /// failure is answered by that export rather than left for whatever
        /// confirms next.
        #[test]
        fn an_export_whose_copy_was_discarded_is_read_again() {
            for (name, export) in exports() {
                let (Some((_one, page)), Some((_two, reference))) =
                    (page(), page())
                else {
                    return;
                };
                let want = export(&reference).expect("reference");
                let copy = buffers(export) - 1;
                let queue = queue();
                // Every buffer before the copy's goes through; the copy's
                // fails.
                queue.fail_after(copy, 1);
                let got = export(&page);
                assert_eq!(queue.unused_failures(), 0, "{name}: no copy made");
                let left = queue.confirm(None);
                queue.fail_after(0, 0);
                assert!(
                    left.is_ok(),
                    "{name}: the discarded copy went unnoticed by the export \
                     and was left for the next confirmation: {left:?}"
                );
                assert_eq!(
                    got.expect("the read again succeeds"),
                    want,
                    "{name}"
                );
            }
        }

        /// An export whose copies Metal discards every time is an error,
        /// not pixels, and leaves nothing in the page cache for the next
        /// export to start from.
        #[test]
        fn an_export_whose_copies_are_all_discarded_is_an_error() {
            for (name, export) in exports() {
                let Some((_canvas, page)) = page() else {
                    return;
                };
                let cached = || {
                    PageCache::get(
                        page.id,
                        &ExportOptions::default(),
                        page.depth(),
                    )
                    .0
                    .is_some()
                };
                assert!(!cached(), "{name}: cached before the export");
                let copy = buffers(export) - 1;
                let queue = queue();
                queue.fail_after(copy, usize::MAX);
                let got = export(&page);
                queue.fail_after(0, 0);
                let _ = queue.confirm(None);
                match got {
                    Err(reason) => {
                        assert!(
                            reason.starts_with(DISCARDED),
                            "{name}: {reason}"
                        )
                    }
                    Ok(_) => {
                        panic!("{name}: discarded copies came back as pixels")
                    }
                }
                assert!(!cached(), "{name}: an unconfirmed read was filed");
            }
        }

        /// An export whose draws Metal discarded is drawn again.
        #[test]
        fn an_export_whose_draws_were_discarded_is_drawn_again() {
            for (name, export) in exports() {
                let (Some((_one, page)), Some((_two, reference))) =
                    (page(), page())
                else {
                    return;
                };
                let want = export(&reference).expect("reference");
                let queue = queue();
                queue.fail_after(0, 1);
                let got = export(&page);
                assert_eq!(queue.unused_failures(), 0, "{name}: no draws made");
                let left = queue.confirm(None);
                queue.fail_after(0, 0);
                assert!(
                    left.is_ok(),
                    "{name}: the discarded draws were left: {left:?}"
                );
                assert_eq!(
                    got.expect("the draw again succeeds"),
                    want,
                    "{name}"
                );
            }
        }
    }

    /// A `getImageData` read the checked read does not make, confirmed after
    /// it as an export's is: the copy's buffer as well as the draws'.
    #[cfg(feature = "metal")]
    mod reads {
        use objc2::rc::Retained;

        use super::DISCARDED;
        use crate::{
            Canvas, Error, RgbaLinear,
            canvas::EngineKind,
            gpu::metal::{self, ISSUED, MetalEngine, RecordingQueue},
        };

        /// This thread's offscreen context's queue of ours, which a canvas
        /// drawn and read on this thread makes its buffers on.
        fn queue() -> Retained<RecordingQueue> {
            let mut queue = None;
            MetalEngine::with_direct_context(|context| {
                queue = context.and_then(|context| metal::recording(context));
            });
            queue.expect("the offscreen context makes its buffers on ours")
        }

        /// A read of a canvas drawn as `read` draws it.
        type Read = fn(&mut Canvas) -> Result<Vec<u8>, Error>;

        /// A fresh 600 by 300 GPU canvas with a translucent fill on it, or
        /// `None` on a machine with no GPU engine.
        fn canvas() -> Option<Canvas> {
            let mut canvas = Canvas::new(600.0, 300.0);
            if canvas.engine_kind() != EngineKind::Gpu {
                eprintln!(
                    "skipped: no GPU engine, so no copy can be discarded"
                );
                return None;
            }
            let ctx = canvas.context();
            ctx.set_fill_style(RgbaLinear::from_srgb8(204, 77, 26, 0.45));
            ctx.fill_rect(20.0, 5.0, 500.0, 250.0);
            Some(canvas)
        }

        /// The reads that copy the page off the GPU without the checked
        /// read: one running past the page, which Skia declines to make
        /// asynchronously, the same inside an open layer, and the repeat of
        /// a read, which copies the whole page and keeps the copy -- or the
        /// whole tile, for a read small enough to be served by tiles.
        fn reads() -> [(&'static str, Read); 4] {
            [
                ("past the edge", |canvas| {
                    let data = canvas
                        .context()
                        .get_image_data(500.0, 100.0, 300.0, 100.0)?;
                    Ok(data.pixels().to_vec())
                }),
                ("in a layer", |canvas| {
                    let ctx = canvas.context();
                    ctx.save_layer_with(0.5, None, None);
                    ctx.fill_rect(8.0, 8.0, 32.0, 24.0);
                    let data = ctx.get_image_data(-4.0, -4.0, 40.0, 40.0)?;
                    Ok(data.pixels().to_vec())
                }),
                ("repeated", |canvas| {
                    let ctx = canvas.context();
                    ctx.get_image_data(0.0, 0.0, 600.0, 300.0)?;
                    let data = ctx.get_image_data(0.0, 0.0, 600.0, 300.0)?;
                    Ok(data.pixels().to_vec())
                }),
                ("repeated tile", |canvas| {
                    let ctx = canvas.context();
                    ctx.get_image_data(37.0, 11.0, 8.0, 8.0)?;
                    let data = ctx.get_image_data(37.0, 11.0, 8.0, 8.0)?;
                    Ok(data.pixels().to_vec())
                }),
            ]
        }

        /// How many buffers `read` of a fresh canvas commits: the last is
        /// the one its copy is encoded in.
        fn buffers(read: Read) -> usize {
            let mut canvas = canvas().expect("a GPU engine");
            let before = ISSUED.get();
            read(&mut canvas).expect("a read");
            ISSUED.get() - before
        }

        /// A read whose copy Metal discarded is read again, and the failure
        /// is answered by that read rather than left for whatever confirms
        /// next.
        #[test]
        fn a_read_whose_copy_was_discarded_is_read_again() {
            for (name, read) in reads() {
                let (Some(mut canvas), Some(mut reference)) =
                    (canvas(), canvas())
                else {
                    return;
                };
                let want = read(&mut reference).expect("reference");
                let copy = buffers(read) - 1;
                let queue = queue();
                queue.fail_after(copy, 1);
                let got = read(&mut canvas);
                assert_eq!(queue.unused_failures(), 0, "{name}: no copy made");
                let left = queue.confirm(None);
                queue.fail_after(0, 0);
                assert!(
                    left.is_ok(),
                    "{name}: the discarded copy went unnoticed by the read \
                     and was left for the next confirmation: {left:?}"
                );
                assert!(
                    got.expect("the read again succeeds") == want,
                    "{name}: the read again differs from the reference"
                );
            }
        }

        /// A read whose copies Metal discards every time is an error, not
        /// pixels, and keeps no copy for the next read to be served from.
        #[test]
        fn a_read_whose_copies_are_all_discarded_is_an_error() {
            for (name, read) in reads() {
                let (Some(mut canvas), Some(mut reference)) =
                    (canvas(), canvas())
                else {
                    return;
                };
                read(&mut reference).expect("reference");
                let copy = buffers(read) - 1;
                let queue = queue();
                queue.fail_after(copy, usize::MAX);
                let got = read(&mut canvas);
                queue.fail_after(0, 0);
                let _ = queue.confirm(None);
                match got {
                    Err(Error::Render { reason }) => assert!(
                        reason.starts_with(DISCARDED),
                        "{name}: {reason}"
                    ),
                    other => panic!(
                        "{name}: discarded copies came back as {other:?}"
                    ),
                }
                // The same state, read once nothing fails: a copy the failed
                // read kept would be served here.
                let whole = |canvas: &mut Canvas| {
                    canvas
                        .context()
                        .get_image_data(0.0, 0.0, 600.0, 300.0)
                        .expect("a whole read")
                        .pixels()
                        .to_vec()
                };
                assert!(
                    whole(&mut canvas) == whole(&mut reference),
                    "{name}: a read after served an unconfirmed copy"
                );
            }
        }
    }
}
