//! Letting go of what a render left behind, once rendering stops.
//!
//! Two things outlive a finished render, at two different levels.
//!
//! The page cache holds a rasterized bitmap per page so that exporting an
//! unchanged canvas again does not composite it again -- worth 1.9
//! milliseconds on a 400x300 raw export and 1.2 on a 1200x900 PNG. An entry
//! leaves when its page's generation is retired, and a canvas that
//! JavaScript has dropped retires nothing until V8 finalizes the box holding
//! it. V8 sizes that box at a few machine words and cannot see the bitmap
//! behind it, so it feels almost no pressure to collect: thirty 1200x900
//! canvases drawn once each and dropped left fifteen entries holding 61.8 MB,
//! and ten seconds of idle recovered none of it.
//!
//! Underneath that, a C allocator keeps freed pages in its own arenas rather
//! than handing them back, so resident memory is a high-water mark: a batch
//! of large exports reads high long after the pixels are gone. The memory is
//! not lost -- the next export is served out of it, which is why repeating a
//! workload costs nothing after the first pass -- but a process that has
//! finished a job and gone quiet is holding pages for a render that may never
//! come. glibc exposes `malloc_trim` for exactly this, and it releases only
//! whole pages that are already free.
//!
//! The watcher below waits for rendering to stop and then does both, in that
//! order. The order is the point: trimming while the bitmaps are still live
//! allocations cannot hand their pages back, so the two steps were worth far
//! less apart than together.
//!
//! # Why this is not done after every render
//!
//! Measured on glibc after two hundred card exports: `malloc_trim` took 1.41
//! milliseconds and returned 13 MB, and 0.15 milliseconds when there was
//! nothing to give back. A card export is around thirty milliseconds, so
//! trimming after each one would add about five percent to every render to
//! reclaim pages the next render immediately takes back again. Dropping the
//! bitmaps is cheaper but costs more: the next export of that canvas
//! composites from scratch. Both are only worth doing once the process has
//! stopped rendering.
//!
//! It is also why this is not hung off the GPU idle watcher, which would have
//! been the obvious home: that thread only exists once a GPU engine
//! initialises, and a CPU-only machine holds the same pages.

use std::{
    sync::{
        OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::Duration,
};

use crate::context::page::release_cached_pages;

/// Bumped once per rasterization. Only ever compared against itself.
static RENDERS: AtomicU64 = AtomicU64::new(0);

/// Holds the watcher thread, so it is started once and only if something
/// renders. A process that loads the addon and draws nothing pays for no
/// thread at all.
static WATCHER: OnceLock<()> = OnceLock::new();

/// How long rendering has to have stopped before the memory goes back.
///
/// Long enough that a pause between frames of an animation, or between two
/// requests arriving, does not drop a cache entry or trigger a walk of the
/// arenas; short enough that a process which has genuinely finished is not
/// sitting on the memory for a noticeable time.
const IDLE_TICKS_BEFORE_RECLAIM: u32 = 3;

const TICK: Duration = Duration::from_secs(1);

/// Records that a rasterization happened, and starts the watcher on the first
/// one.
///
/// Called from the one place every raster path passes through, so an export,
/// a readback and an animation frame all count. A relaxed increment on a
/// counter nothing reads for correctness -- the watcher only asks whether it
/// changed.
pub(crate) fn note_render() {
    RENDERS.fetch_add(1, Ordering::Relaxed);
    WATCHER.get_or_init(spawn);
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

/// Reclaims once the render count has held still for a few ticks, and not
/// again until it moves.
///
/// The "not again" is the part that matters: without it an idle process drops
/// cache entries it has already dropped and walks its arenas every second
/// forever, which is pure waste once the first pass has taken what there was.
fn spawn() {
    thread::spawn(|| {
        let mut last_seen = 0;
        let mut idle_ticks = 0;
        let mut reclaimed_after = u64::MAX;

        loop {
            thread::sleep(TICK);

            let renders = RENDERS.load(Ordering::Relaxed);
            if renders != last_seen {
                last_seen = renders;
                idle_ticks = 0;
                continue;
            }

            idle_ticks += 1;
            if idle_ticks >= IDLE_TICKS_BEFORE_RECLAIM
                && reclaimed_after != renders
            {
                // Bitmaps first: they are live allocations until they are
                // dropped, and a trim cannot hand back a page something is
                // still using.
                release_cached_pages();
                trim();
                reclaimed_after = renders;
            }
        }
    });
}

#[cfg(target_env = "gnu")]
mod gnu {
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
}
