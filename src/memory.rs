//! Returning freed memory to the operating system once rendering stops.
//!
//! A C allocator keeps freed pages in its own arenas rather than handing them
//! back, so resident memory is a high-water mark: a batch of large exports
//! reads high long after the pixels are gone. The memory is not lost -- the
//! next export is served out of it, which is why repeating a workload costs
//! nothing after the first pass -- but a process that has finished a job and
//! gone quiet is holding pages for a render that may never come.
//!
//! glibc exposes `malloc_trim` for exactly this. It releases only whole pages
//! that are already free: no live allocation moves, no cache is emptied, and
//! nothing has to be re-rendered afterwards.
//!
//! # Why this is not done after every render
//!
//! Measured on glibc after two hundred card exports: the call took 1.41
//! milliseconds and returned 13 MB, and 0.15 milliseconds when there was
//! nothing to give back. A card export is around thirty milliseconds, so
//! trimming after each one would add about five percent to every render to
//! reclaim pages the next render immediately takes back again. The work is
//! only worth doing once the process has stopped rendering, which is what the
//! watcher below waits for.
//!
//! It is also why this is not hung off the GPU idle watcher, which would have
//! been the obvious home: that thread only exists once a GPU engine
//! initialises, and a CPU-only machine holds the same pages.

/// Records that a rasterization happened, and starts the watcher on the first
/// one.
///
/// Called from the one place every raster path passes through, so an export,
/// a readback and an animation frame all count. A relaxed increment on a
/// counter nothing reads for correctness -- the watcher only asks whether it
/// changed.
pub(crate) fn note_render() {
    #[cfg(target_env = "gnu")]
    gnu::note_render();
}

/// Hands back memory this process has freed but is still holding.
///
/// Releases only pages that are already free, so live allocations are
/// untouched and no cache is emptied. Happens by itself a couple of seconds
/// after rendering stops; this is here for a caller who knows better than the
/// clock does -- a batch that has just finished and will not be followed by
/// another.
///
/// Returns whether the platform had anything to try: `true` on glibc, `false`
/// on macOS, Windows and musl, where it does nothing. macOS was measured with
/// `malloc_zone_pressure_relief` and returned nothing for this workload;
/// musl's allocator is a different design and wants its own measurement
/// before being included.
pub fn trim() -> bool {
    #[cfg(target_env = "gnu")]
    {
        gnu::trim();
        true
    }

    #[cfg(not(target_env = "gnu"))]
    false
}

#[cfg(target_env = "gnu")]
mod gnu {
    use std::{
        sync::{
            OnceLock,
            atomic::{AtomicU64, Ordering},
        },
        thread,
        time::Duration,
    };

    /// Bumped once per rasterization. Only ever compared against itself.
    static RENDERS: AtomicU64 = AtomicU64::new(0);

    /// Holds the watcher thread, so it is started once and only if something
    /// renders. A process that loads the addon and draws nothing pays for no
    /// thread at all.
    static WATCHER: OnceLock<()> = OnceLock::new();

    /// How long rendering has to have stopped before the pages go back.
    ///
    /// Long enough that a pause between frames of an animation, or between
    /// two requests arriving, does not trigger a walk of the arenas; short
    /// enough that a process which has genuinely finished is not sitting on
    /// the memory for a noticeable time.
    const IDLE_TICKS_BEFORE_TRIM: u32 = 3;

    const TICK: Duration = Duration::from_secs(1);

    pub(super) fn note_render() {
        RENDERS.fetch_add(1, Ordering::Relaxed);
        WATCHER.get_or_init(|| {
            spawn();
        });
    }

    pub(super) fn trim() {
        unsafe extern "C" {
            fn malloc_trim(pad: usize) -> i32;
        }

        // SAFETY: glibc's own entry point, documented as safe to call at any
        // time from any thread. It releases whole free pages from the arenas
        // and leaves every live allocation where it is.
        unsafe {
            malloc_trim(0);
        }
    }

    /// Trims once the render count has held still for a few ticks, and not
    /// again until it moves.
    ///
    /// The "not again" is the part that matters: without it an idle process
    /// walks its arenas every second forever, which is pure waste once the
    /// first walk has taken what there was.
    fn spawn() {
        thread::spawn(|| {
            let mut last_seen = 0;
            let mut idle_ticks = 0;
            let mut trimmed_after = u64::MAX;

            loop {
                thread::sleep(TICK);

                let renders = RENDERS.load(Ordering::Relaxed);
                if renders != last_seen {
                    last_seen = renders;
                    idle_ticks = 0;
                    continue;
                }

                idle_ticks += 1;
                if idle_ticks >= IDLE_TICKS_BEFORE_TRIM
                    && trimmed_after != renders
                {
                    trim();
                    trimmed_after = renders;
                }
            }
        });
    }
}
