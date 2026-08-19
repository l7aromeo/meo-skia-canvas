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
//! One thing the owners took away, which the figures above still assume. The
//! idle watcher reaps a context by `rayon::spawn_broadcast`, so it reaches
//! every worker and no owner -- these are `thread::Builder` threads and the
//! broadcast has no way to touch their thread-locals. An owner's context is
//! held for the life of the process rather than five seconds past its last
//! job, which is what makes the count of them worth caring about at all: see
//! `Owners`, where a job goes to the emptiest queue so that a caller who never
//! overlaps only ever wakes one.
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
        mpsc::{RecvTimeoutError, Sender, channel},
    },
    thread,
    time::Duration,
};

use crate::{
    context::page::{ExportOptions, Page},
    gpu::{Engine, RenderingEngine, autorelease},
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

/// How long an idle owner waits before asking whether its context can go.
///
/// A second, which is the engines' own watcher tick rather than a number of
/// this module's own -- both are asking the same question of the same
/// thread-local, and the answer is gated by the backend's lifespan rather than
/// by how often it is asked. A blocked `recv` costs nothing while it waits, so
/// the only thing the interval buys is how promptly a context is offered up
/// after the work stops.
const IDLE_CHECK: Duration = Duration::from_secs(1);

/// The queues, and the threads draining them.
///
/// One queue per owner rather than one queue several threads take from: a
/// blocked `recv` on a shared receiver holds the lock that the other owners
/// need to reach it, so they would wait on each other rather than on work.
///
/// Jobs go to whichever owner has least in flight, and that choice is worth a
/// context. Dealing in turn meant four exports awaited one after another --
/// no two of them ever overlapping -- still woke all four owners, and each
/// built its own `DirectContext` and Skia resource cache on the way. A tie
/// goes to the lowest index, so a caller whose exports never overlap stays on
/// one owner and the other three are never asked for anything. Under real
/// overlap every owner is busy and the choice is the same one dealing in turn
/// would have made, which is why this costs the concurrent case nothing.
///
/// `Mutex` because `mpsc::Sender` is not `Sync` and this is a `static`. The
/// lock spans one `send` and guards no rendering; the work happens on the
/// owner, after the lock is gone.
struct Owners {
    queues: Vec<Mutex<Sender<Job>>>,
    /// Jobs handed to each owner and not yet finished.
    ///
    /// Advisory, and published to nothing: it steers the next job and guards
    /// no memory, so `Relaxed` is the whole of the ordering requirement.
    inflight: Vec<AtomicUsize>,
}

/// The count an owner is parked at once its queue refuses a job.
///
/// A `send` fails only when the receiver is gone, which means that owner died
/// with a job in hand. A value nothing can count up to keeps it from ever
/// reading as the emptiest queue again, so the dispatcher stops choosing a
/// thread that cannot answer.
const RETIRED: usize = usize::MAX;

/// Which owner a job should go to: the emptiest queue, ties to the lowest
/// index, and `None` when there is nobody left to ask.
///
/// Its own function so that the choice can be tested without spawning a thread
/// or building a `Job`, both of which need a live GPU to mean anything.
fn emptiest(inflight: &[AtomicUsize]) -> Option<usize> {
    inflight
        .iter()
        .enumerate()
        .map(|(at, count)| (at, count.load(Ordering::Relaxed)))
        .min_by_key(|&(_, count)| count)
        .filter(|&(_, count)| count != RETIRED)
        .map(|(at, _)| at)
}

impl Owners {
    /// Hands `job` to the owner with least in flight.
    ///
    /// `Err` when none can take it -- a machine where no thread could be
    /// spawned, or one where every owner has died; the caller renders inline.
    fn send(&self, job: Job) -> Result<(), ()> {
        let at = emptiest(&self.inflight).ok_or(())?;

        self.inflight[at].fetch_add(1, Ordering::Relaxed);
        self.queues[at].lock().send(job).map_err(|_| {
            self.inflight[at].store(RETIRED, Ordering::Relaxed);
        })
    }
}

/// Releases an owner's in-flight count when its job ends, however it ends.
///
/// A panicking job unwinds through the owner's loop and takes the thread with
/// it. The count still has to come back: left raised, that owner would read as
/// permanently busy and the dispatcher would quietly route around a thread
/// that is merely dead, instead of sending to it once and letting the failed
/// `send` retire it. Dropping first is what makes that ordering hold -- the
/// guard unwinds before the `for` loop gives up its receiver.
struct InFlight(usize);

impl Drop for InFlight {
    fn drop(&mut self) {
        // `JOBS` is set before any owner can be reached, because the only
        // handle to one is the queue this returns.
        if let Some(owners) = JOBS.get() {
            owners.inflight[self.0].fetch_sub(1, Ordering::Relaxed);
        }
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
        for _ in 0..wanted {
            // Where this owner will land, which is not the iteration number:
            // a thread that fails to spawn pushes nothing, and the next one
            // takes the index it did not use.
            let at = queues.len();
            let (tx, rx) = channel::<Job>();
            let spawned = thread::Builder::new()
                .name(format!("skia-gpu-{at}"))
                .spawn(move || {
                    IS_OWNER.set(true);
                    // Metal's `objc` allocations need a pool on whatever
                    // thread makes them, and this thread makes all of its
                    // own. One per job rather than one for the thread: a pool
                    // only drains when it is dropped, so wrapping the loop
                    // would hold every export's temporaries until the process
                    // ended.
                    loop {
                        match rx.recv_timeout(IDLE_CHECK) {
                            Ok(job) => {
                                let _release = InFlight(at);
                                autorelease(|| run(job));
                            }
                            // Nothing to do, so ask the backend whether this
                            // thread's context has been idle long enough to
                            // give something back. A `rayon` worker is asked
                            // the same question by the engine's own watcher,
                            // which broadcasts over the pool and so cannot
                            // reach a thread spawned here.
                            Err(RecvTimeoutError::Timeout) => {
                                autorelease(Engine::release_idle_context);
                            }
                            // Every sender is gone, which happens only at
                            // shutdown; there is nothing left to drain.
                            Err(RecvTimeoutError::Disconnected) => break,
                        }
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

        let inflight = (0..queues.len()).map(|_| AtomicUsize::new(0)).collect();
        Owners { queues, inflight }
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

#[cfg(test)]
mod tests {
    use super::{RETIRED, emptiest};
    use std::sync::atomic::AtomicUsize;

    fn owners(counts: &[usize]) -> Vec<AtomicUsize> {
        counts.iter().copied().map(AtomicUsize::new).collect()
    }

    #[test]
    fn nobody_working_means_the_first_owner() {
        // The whole point: a caller whose exports never overlap comes back to
        // an idle set every time and must keep landing on the same owner, or
        // it pays for a context per owner to do work that never overlapped.
        assert_eq!(emptiest(&owners(&[0, 0, 0, 0])), Some(0));
    }

    #[test]
    fn a_busy_owner_is_passed_over() {
        assert_eq!(emptiest(&owners(&[1, 0, 0, 0])), Some(1));
        assert_eq!(emptiest(&owners(&[2, 2, 1, 2])), Some(2));
    }

    #[test]
    fn everyone_busy_still_takes_the_shortest_queue() {
        // Under real overlap this is the balance the round-robin it replaced
        // would have struck anyway, which is why the concurrent case pays
        // nothing for the idle case's saving.
        assert_eq!(emptiest(&owners(&[3, 1, 2, 1])), Some(1));
    }

    #[test]
    fn a_retired_owner_is_never_chosen() {
        // Retired means the thread died with a job in hand, so it reads as
        // busier than any live queue however long that queue grows.
        assert_eq!(emptiest(&owners(&[RETIRED, 9])), Some(1));
        assert_eq!(emptiest(&owners(&[RETIRED, RETIRED])), None);
    }

    #[test]
    fn no_owners_at_all_is_no_answer() {
        // A machine where no thread could be spawned; the caller renders
        // inline rather than waiting on a queue nobody drains.
        assert_eq!(emptiest(&owners(&[])), None);
    }
}
