//! The few threads that own a GPU context for exporting.
//!
//! Both backends keep their `DirectContext` in a `thread_local`, created by
//! whichever thread asks for it first (`metal.rs`, `vulkan/engine.rs`). An
//! export runs on a `rayon` worker, so the number of contexts was however many
//! workers happened to export, and resident memory grew with it.
//!
//! Measured by exporting 150 frames of `examples/node/animated-eye.js` at
//! 640x500, with `RAYON_NUM_THREADS` pinned: 648 MB at one worker, 728 at
//! four, 800 at eight, 909 at the default. About 22 MB a worker, each context
//! carrying its own Skia resource cache -- and on Apple Silicon the device
//! side of that is the same resident memory. Peak rather than steady state:
//! contexts are reaped after five idle seconds, so the process gives it back
//! once it stops, having already held it.
//!
//! With the owners it is 680 MB at one worker, 714 at four, 731 at eight and
//! 743 at sixteen: about 4 MB a worker rather than 22, which is the encoders'
//! own buffers and not a context.
//!
//! So the GPU gets owners: a bounded few, rather than one per worker. Each
//! holds a context, rasterises, and replies with pixels in main memory.
//! Nothing texture-backed crosses a channel, which is what lets
//! [`crate::context::page::PageCache`] hold images without asking which thread
//! it is on, and what retired both the `rayon::current_thread_index()` test in
//! `Page::composite` and the `PageCache::materialize` pass that three call
//! sites ran before an asynchronous export.
//!
//! Bounded is the whole idea, and the bound was one for a while. That is a
//! worse number than it looks: rasterizing the 150 pages is about 1090 ms of
//! work, one thread does all of it in series, and no amount of encoding
//! behind it can finish the export sooner. Measured with [`OWNERS`] set to
//! each in turn, at the default worker count:
//!
//! | owners | export | peak |
//! | -----: | -----: | ---: |
//! |      1 | 1091 ms | 669 MB |
//! |      2 |  543 |  694 |
//! |      4 |  431 |  744 |
//! |      8 |  536 |  811 |
//!
//! Against 890 ms and 909 MB for the per-worker contexts this replaced. Four
//! is faster than what it replaced and lighter, and eight is worse at both:
//! past four the contexts contend for one device and pay for their own
//! resource caches to do it.
//!
//! The same sweep on Vulkan -- a twelve-core Linux box with a GTX 1050 Ti,
//! so a different backend, vendor and core count -- picks the same number:
//! 2193 ms at one owner, 1636 at two, 1561 at four, 1813 at eight, with peak
//! memory climbing 873, 872, 925, 1006 MB. The gain from one to four is
//! smaller there than on Metal, and the loss at eight is the same shape, so
//! four is not a figure tuned to one machine.
//!
//! One thing this does **not** explain, stated because the first reading of it
//! was wrong. Pinned to a single worker an export is slow -- 7368 ms on the
//! GPU -- and that is not the contexts. The same figure with no GPU at all is
//! 9205 ms, so it is the encoder, and most of it is PNG row filtering: see
//! `png_filter_flags`, which is a deliberate trade of time for 14-18% smaller
//! files and was measured at +80% on the CPU export path.
//!
//! What deliberately stays parallel is encoding. Rasterizing and compressing on
//! the same thread is the obvious simplification and would give up the parallel
//! encoders this crate has for APNG, WebP and GIF. A worker submits, waits for
//! its pixels, and compresses them where it already is -- so with N workers
//! there are up to N jobs queued across the owners and N encoders running, and
//! the GPU stays fed.

use parking_lot::Mutex;
use skia_safe::{Image as SkImage, ImageInfo};
use std::{
    cell::Cell,
    sync::{
        OnceLock,
        atomic::{AtomicUsize, Ordering},
        mpsc::{Sender, channel},
    },
    thread,
};

use crate::{
    context::page::{ExportOptions, Page},
    gpu::{RenderingEngine, autorelease},
};

/// What the owner is asked for.
///
/// Both variants carry owned data rather than borrows, because the owner
/// outlives every caller and a queue cannot hold a lifetime. A `Page` is a
/// `Vec<Picture>` of reference-counted recordings, so the clone is a handful
/// of increments and no pixels.
enum Job {
    /// Composite the page and hand back its pixels.
    Image {
        page: Page,
        options: Box<ExportOptions>,
        reply: Sender<Result<SkImage, String>>,
    },
    /// Composite the page and read it into a caller's own pixel layout.
    Pixels {
        page: Page,
        options: Box<ExportOptions>,
        info: ImageInfo,
        reply: Sender<Result<Vec<u8>, String>>,
    },
    /// As [`Job::Pixels`], but standing the cached bitmap in for the layers
    /// it already holds. A separate job rather than a flag on that one,
    /// because the two answer different questions: a frame wants every layer
    /// drawn, an export wants the page as it is now.
    CachedPixels {
        page: Page,
        options: Box<ExportOptions>,
        info: ImageInfo,
        reply: Sender<Result<Vec<u8>, String>>,
    },
}

thread_local!(
    /// Whether this is the owner thread.
    ///
    /// Work reached from inside a job runs inline instead of queueing:
    /// only this thread drains the queue, so submitting to it from itself
    /// would wait forever.
    static IS_OWNER: Cell<bool> = const { Cell::new(false) };
);

/// How many threads may own a context, at most.
///
/// The point of owning is that the number of contexts stops following the
/// size of the `rayon` pool; it was never that the number had to be one. One
/// costs a long sequence real time, because rasterizing 150 pages is about
/// 1090 ms of work and one owner does all of it in series while the encoders
/// behind it finish in less -- so the export cannot beat that figure however
/// many workers are encoding. Measured on the sequence above: 3183 ms at one
/// `rayon` worker, 1113 at four, 1103 at eight, 1091 at the default, which is
/// the plateau being described.
///
/// Four, then, or fewer on a machine with fewer cores. It is the count at
/// which the encoders rather than the rasterizer became the limit in the
/// old arrangement, and each context past it buys nothing while costing its
/// own resource cache -- about 22 MB apiece, measured as 648 MB at one worker
/// and 800 at eight before any of this.
const OWNERS: usize = 4;

/// The queues, and the threads draining them.
///
/// One queue per owner and jobs dealt round-robin, rather than one queue
/// several threads take from: a blocked `recv` on a shared receiver holds the
/// lock that the other owners need to reach it, so they would wait on each
/// other rather than on work. Any owner can do any job, so which one gets it
/// only matters for balance, and the pages of a sequence cost the same.
///
/// `Mutex` because `mpsc::Sender` is not `Sync` and this is a `static`. The
/// lock spans one `send` and guards no rendering; the work happens on the
/// owner, after the lock is gone.
struct Owners {
    queues: Vec<Mutex<Sender<Job>>>,
    next: AtomicUsize,
}

impl Owners {
    /// Hands `job` to the next owner in turn.
    ///
    /// `Err` when there are no owners at all, which is a machine where no
    /// thread could be spawned; the caller renders inline.
    fn send(&self, job: Job) -> Result<(), ()> {
        if self.queues.is_empty() {
            return Err(());
        }
        let at = self.next.fetch_add(1, Ordering::Relaxed) % self.queues.len();
        self.queues[at].lock().send(job).map_err(|_| ())
    }
}

static JOBS: OnceLock<Owners> = OnceLock::new();

/// Starts the owners on the first export and returns their queues.
fn jobs() -> &'static Owners {
    JOBS.get_or_init(|| {
        let wanted = thread::available_parallelism()
            .map(|cores| cores.get().min(OWNERS))
            .unwrap_or(1);

        let mut queues = Vec::with_capacity(wanted);
        for nth in 0..wanted {
            let (tx, rx) = channel::<Job>();
            let spawned = thread::Builder::new()
                .name(format!("skia-gpu-{nth}"))
                .spawn(move || {
                    IS_OWNER.set(true);
                    // Metal's `objc` allocations need a pool on whatever
                    // thread makes them, and this thread makes all of its
                    // own. One per job rather than one for the thread: a pool
                    // only drains when it is dropped, so wrapping the loop
                    // would hold every export's temporaries until the process
                    // ended.
                    for job in rx {
                        autorelease(|| run(job));
                    }
                });
            match spawned {
                Ok(_) => queues.push(Mutex::new(tx)),
                // Whatever started is enough to export with, and nothing
                // starting means every caller renders inline -- which is what
                // this crate did before the owners existed: correct, and as
                // many contexts as there are threads.
                Err(why) => eprintln!(
                    "meo-skia-canvas: no GPU thread ({why}); rendering inline"
                ),
            }
        }

        Owners {
            queues,
            next: AtomicUsize::new(0),
        }
    })
}

/// Performs one job and answers whoever asked.
///
/// A send failure means the caller gave up -- it cannot happen while the
/// caller is blocked on `recv`, and if it does the reply has nowhere to go, so
/// the result is dropped rather than reported.
fn run(job: Job) {
    match job {
        Job::Image {
            page,
            options,
            reply,
        } => {
            let _ = reply.send(page.composite(&options, RenderingEngine::GPU));
        }
        Job::Pixels {
            page,
            options,
            info,
            reply,
        } => {
            let _ = reply.send(page.composite_pixels(
                &options,
                RenderingEngine::GPU,
                &info,
            ));
        }
        Job::CachedPixels {
            page,
            options,
            info,
            reply,
        } => {
            let _ = reply.send(page.composite_into(
                &options,
                RenderingEngine::GPU,
                &info,
            ));
        }
    }
}

/// Whether this thread may touch a GPU context directly.
///
/// True on the owner, and true when there is no owner to ask -- a thread that
/// could not be spawned is not a reason to fail an export.
fn inline() -> bool {
    IS_OWNER.get()
}

/// This page composited on the GPU, as pixels in main memory.
///
/// Runs on the owner thread and blocks until it answers. Blocking a `rayon`
/// worker is the intent rather than a compromise: the worker has nothing to do
/// until it has pixels, and its wait is what keeps the owner's queue full.
pub fn composite(
    page: &Page,
    options: &ExportOptions,
) -> Result<SkImage, String> {
    if inline() {
        return page.composite(options, RenderingEngine::GPU);
    }

    let (reply, answer) = channel();
    let job = Job::Image {
        page: page.clone(),
        options: Box::new(options.clone()),
        reply,
    };
    match jobs().send(job) {
        Ok(()) => answer
            .recv()
            // The owner is gone, having panicked with a job in hand. Rendering
            // here is the same work in a worse place, and better than a failed
            // export.
            .unwrap_or_else(|_| page.composite(options, RenderingEngine::GPU)),
        Err(_) => page.composite(options, RenderingEngine::GPU),
    }
}

/// This page composited on the GPU and read into `info`'s layout.
///
/// The whole page, with no cache: [`Page::composite_pixels`] is what
/// `render_raw` and the animation frames want, and they ask for every layer.
pub fn composite_pixels(
    page: &Page,
    options: &ExportOptions,
    info: &ImageInfo,
) -> Result<Vec<u8>, String> {
    if inline() {
        return page.composite_pixels(options, RenderingEngine::GPU, info);
    }

    let (reply, answer) = channel();
    let job = Job::Pixels {
        page: page.clone(),
        options: Box::new(options.clone()),
        info: info.clone(),
        reply,
    };
    match jobs().send(job) {
        Ok(()) => answer.recv().unwrap_or_else(|_| {
            page.composite_pixels(options, RenderingEngine::GPU, info)
        }),
        Err(_) => page.composite_pixels(options, RenderingEngine::GPU, info),
    }
}

/// This page composited on the GPU, with its cached bitmap, and read into
/// `info`'s layout.
///
/// What an export of raw pixels wants. [`Page::composite_into`] reads them off
/// the compositing surface, which belongs to this thread and does not leave
/// it, so the page is never downloaded just to be copied out of again.
pub fn composite_into(
    page: &Page,
    options: &ExportOptions,
    info: &ImageInfo,
) -> Result<Vec<u8>, String> {
    if inline() {
        return page.composite_into(options, RenderingEngine::GPU, info);
    }

    let (reply, answer) = channel();
    let job = Job::CachedPixels {
        page: page.clone(),
        options: Box::new(options.clone()),
        info: info.clone(),
        reply,
    };
    match jobs().send(job) {
        Ok(()) => answer.recv().unwrap_or_else(|_| {
            page.composite_into(options, RenderingEngine::GPU, info)
        }),
        Err(_) => page.composite_into(options, RenderingEngine::GPU, info),
    }
}
