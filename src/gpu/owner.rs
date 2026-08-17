//! The one thread that owns a GPU context for exporting.
//!
//! Both backends keep their `DirectContext` in a `thread_local`, created by
//! whichever thread asks for it first (`metal.rs`, `vulkan/engine.rs`). An
//! export runs on a `rayon` worker, so the number of contexts was however many
//! workers happened to export, and resident memory grew with it.
//!
//! Measured by exporting 150 frames of `examples/node/animated-eye.js` at
//! 640x500 under `/usr/bin/time -l`, with `RAYON_NUM_THREADS` pinned: 694 MB at
//! one worker, 717 at two, 759 at four, 836 at eight, 956 at the default. About
//! 20 MB a worker, each context carrying its own Skia resource cache -- and on
//! Apple Silicon the device side of that is the same resident memory. Peak
//! rather than steady state: contexts are reaped after five idle seconds, so
//! the process gives it back once it stops, having already held it.
//!
//! So the GPU gets an owner. One thread holds the context, rasterises, and
//! replies with pixels in main memory. Nothing texture-backed crosses the
//! channel, which is what lets [`crate::context::page::PageCache`] hold images
//! without asking which thread it is on, and what retired both the
//! `rayon::current_thread_index()` test in `Page::composite` and the
//! `PageCache::materialize` pass that three call sites ran before an
//! asynchronous export.
//!
//! Same run afterwards: 681 MB at the default, and flat at 665-688 across one
//! to eight workers. Export time is unchanged -- 1360 ms against 1372 -- and
//! the bytes are identical, so this is memory bought for nothing rather than
//! traded against speed.
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
//! there are up to N jobs queued at the owner and N encoders running, and the
//! GPU stays fed.

use parking_lot::Mutex;
use skia_safe::{Image as SkImage, ImageInfo};
use std::{
    cell::Cell,
    sync::{
        OnceLock,
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
}

thread_local!(
    /// Whether this is the owner thread.
    ///
    /// Work reached from inside a job runs inline instead of queueing:
    /// only this thread drains the queue, so submitting to it from itself
    /// would wait forever.
    static IS_OWNER: Cell<bool> = const { Cell::new(false) };
);

/// The queue, and the thread draining it.
///
/// `Mutex` because `mpsc::Sender` is not `Sync` and this is a `static`. The
/// lock spans one `send` and guards no rendering; the work happens on the
/// owner, after the lock is gone.
static JOBS: OnceLock<Mutex<Sender<Job>>> = OnceLock::new();

/// Starts the owner on the first export and returns its queue.
fn jobs() -> &'static Mutex<Sender<Job>> {
    JOBS.get_or_init(|| {
        let (tx, rx) = channel::<Job>();
        let spawned = thread::Builder::new()
            .name("skia-gpu".to_string())
            .spawn(move || {
                IS_OWNER.set(true);
                // Metal's `objc` allocations need a pool on whatever thread
                // makes them, and this thread makes all of them. One per job
                // rather than one for the thread: a pool only drains when it
                // is dropped, so wrapping the loop would hold every export's
                // temporaries until the process ended.
                for job in rx {
                    autorelease(|| run(job));
                }
            });
        if let Err(why) = spawned {
            // Every caller falls back to rendering inline, which is what this
            // crate did before the owner existed: correct, and as many
            // contexts as there are threads.
            eprintln!(
                "meo-skia-canvas: no GPU thread ({why}); rendering inline"
            );
        }
        Mutex::new(tx)
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
    match jobs().lock().send(job) {
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
    match jobs().lock().send(job) {
        Ok(()) => answer.recv().unwrap_or_else(|_| {
            page.composite_pixels(options, RenderingEngine::GPU, info)
        }),
        Err(_) => page.composite_pixels(options, RenderingEngine::GPU, info),
    }
}
