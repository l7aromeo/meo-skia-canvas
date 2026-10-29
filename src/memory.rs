//! Letting go of what a render left behind, once rendering stops.
//!
//! Three things outlive a finished render, at three different levels.
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
//! The font caches sit beside both. Each collection keeps the paragraphs it
//! has laid out, and the faces and variable-font instances it has cloned, so
//! that the next layout of the same text costs nothing. After sixty
//! text-heavy pages they held 40.4 MB of the 78.1 the allocator had in use,
//! in a debug build on macOS. They belong to the thread that filled them, so
//! the watcher's own thread cannot empty them: in the Node addon it posts the
//! release to each JavaScript thread, where it runs between two tasks -- see
//! `node::idle`.
//!
//! The watcher below waits for rendering to stop and then does all three,
//! bitmaps and fonts before the trim. The order is the point: trimming while
//! the bitmaps are still live allocations cannot hand their pages back, so
//! the steps were worth far less apart than together. A font release runs on
//! its own thread after the watcher has moved on, so it trims again there.
//!
//! On glibc that second trim is what hands the caches' pages back: after the
//! same sixty pages, also in a debug build, it takes the heap in use from 66
//! to 29 MB and resident memory from 198 to 172, since `malloc_trim` returns
//! only whole free pages and the rest of what was freed shares its pages with
//! live allocations. On macOS the 40 MB goes back to the allocator for the
//! next work to reuse, and the physical footprint, which Activity Monitor and
//! jetsam read, does not fall with it in the thirty seconds after.
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
//!
//! The font release has a price of its own, paid once, by the frame after
//! it, which lays its text out again. Forty labels in two variable faces,
//! redrawn on a CPU canvas in a release build on macOS, take about 0.16
//! milliseconds longer in the frame after a release than after the same
//! call releasing nothing, a frame of about 0.28; the frames after that cost
//! nothing extra. Most of it is the paragraphs Skia lays out again, the rest
//! the faces and shaping data it builds again. That is the layout alone,
//! timed with the release made between two frames: where the release has
//! handed pages back to the system, as the trim does on glibc, the next work
//! faults them in again, and that cost is not measured. Against the 26 to
//! 40 MB above, it is the trade.

use std::{
    sync::{
        OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::Duration,
};

use crate::context::{
    blur_mask::release_cached_masks, page::release_cached_pages,
};
#[cfg(feature = "node-addon")]
use crate::node::idle::release_font_caches;

/// Bumped once per rasterization and once per use of a thread's fonts. Only
/// ever compared against itself.
static ACTIVITY: AtomicU64 = AtomicU64::new(0);

/// Holds the watcher thread, so it is started once and only if something
/// renders or reaches for a font. A process that loads the addon and does
/// neither pays for no thread at all.
static WATCHER: OnceLock<()> = OnceLock::new();

/// How long rendering and text layout have to have stopped before the memory
/// goes back, in ticks.
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
    ACTIVITY.fetch_add(1, Ordering::Relaxed);
    WATCHER.get_or_init(spawn);
}

/// Records that a thread used its fonts, and starts the watcher on the first
/// use, so that laying text out holds a release off as rendering does.
///
/// Called from `FontLibrary::with_shared`, which a canvas's text and the
/// addon's `Paragraph` reach their fonts through. The release itself does not
/// pass through it, or it would count as the activity that holds off the next.
pub(crate) fn note_font_use() {
    #[cfg(test)]
    FONT_USES.set(FONT_USES.get() + 1);
    note_render();
}

#[cfg(test)]
thread_local! {
    /// How many times this thread has noted a use of its fonts, for the
    /// tests that hold a path skipping the library to noting one anyway.
    pub(crate) static FONT_USES: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// The activity count, for the debug addon's `__activity`, which a test reads
/// either side of a posted font release to see that the release is not
/// counted as activity itself.
#[cfg(all(debug_assertions, feature = "node-addon"))]
pub(crate) fn activity() -> u64 {
    ACTIVITY.load(Ordering::Relaxed)
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
/// on macOS, Windows and musl, where it does nothing.
///
/// macOS was measured with `malloc_zone_pressure_relief` and returned nothing,
/// and that has been checked twice because the first check could have been
/// wrong for a reason worth writing down. Resident size is the wrong meter on
/// that platform: `MADV_FREE_REUSABLE` pages leave the resident set while
/// staying mapped, so a release can look like nothing and a retention can look
/// like a leak. Re-measured against physical footprint, which is what Activity
/// Monitor and jetsam read: two million small blocks took the footprint to
/// 386.8 MB, freeing them returned 239.4 of it on its own, and
/// `malloc_zone_pressure_relief(NULL, 0)` then returned 0 bytes and moved the
/// footprint not at all. There is nothing left for it to reclaim -- that
/// allocator hands a block back when it is freed rather than holding it in an
/// arena, which is the whole reason glibc needs a trim and it does not.
///
/// None of which means the watcher does nothing on macOS. It empties the page
/// cache first, and those are megabyte-scale allocations that are mapped on
/// their own and unmapped on free, so they come back on every platform: two
/// hundred card exports settle at a 206.9 MB footprint here against 469.4
/// before that release existed.
///
/// musl's allocator is a different design and wants its own measurement before
/// being included.
pub fn trim() -> bool {
    #[cfg(target_env = "gnu")]
    {
        gnu::trim();
        true
    }

    #[cfg(not(target_env = "gnu"))]
    false
}

/// Reclaims once the activity count has held still for a few ticks, and not
/// again until it moves.
fn spawn() {
    thread::spawn(|| {
        let mut quiet = Quiet::default();
        loop {
            thread::sleep(TICK);
            if quiet.tick(ACTIVITY.load(Ordering::Relaxed)) {
                reclaim();
            }
        }
    });
}

/// Hands back what the last spell of rendering left behind.
fn reclaim() {
    // Posted, so it runs on each JavaScript thread after this returns, and
    // trims there once its caches have gone.
    #[cfg(feature = "node-addon")]
    release_font_caches();
    // Bitmaps first: they are live allocations until they are dropped, and a
    // trim cannot hand back a page something is still using.
    release_cached_pages();
    release_cached_masks();
    trim();
}

/// When the watcher reclaims, from the activity count it reads once a tick:
/// once the count has held still for [`IDLE_TICKS_BEFORE_RECLAIM`] ticks in a
/// row, and not again until it moves.
///
/// The "not again" is the part that matters: without it an idle process drops
/// cache entries it has already dropped and walks its arenas every second
/// forever, which is pure waste once the first pass has taken what there was.
#[derive(Debug, Default)]
struct Quiet {
    /// The count at the last tick. Zero to begin with, which the count has
    /// already passed when the watcher starts, since the first bump starts it.
    last_seen: u64,
    /// Ticks in a row the count has held still for.
    idle_ticks: u32,
    /// The count the last reclaim ran at, if any has.
    reclaimed_at: Option<u64>,
}

impl Quiet {
    /// Takes one tick's reading of the count, and answers whether to reclaim
    /// now.
    fn tick(&mut self, count: u64) -> bool {
        if count != self.last_seen {
            self.last_seen = count;
            self.idle_ticks = 0;
            return false;
        }
        self.idle_ticks = self.idle_ticks.saturating_add(1);
        let due = self.idle_ticks >= IDLE_TICKS_BEFORE_RECLAIM
            && self.reclaimed_at != Some(count);
        if due {
            self.reclaimed_at = Some(count);
        }
        due
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The interval, in ticks.
    const INTERVAL: usize = IDLE_TICKS_BEFORE_RECLAIM as usize;

    /// The ticks on which the watcher reclaims, reading `counts` one a tick.
    fn reclaims(counts: impl IntoIterator<Item = u64>) -> Vec<usize> {
        let mut quiet = Quiet::default();
        counts
            .into_iter()
            .enumerate()
            .filter_map(|(at, count)| quiet.tick(count).then_some(at))
            .collect()
    }

    #[test]
    fn nothing_is_reclaimed_while_the_count_moves_every_tick() {
        assert_eq!(reclaims(1..=20), Vec::<usize>::new());
    }

    #[test]
    fn nothing_is_reclaimed_while_the_count_moves_once_an_interval() {
        // A frame or a request every few ticks holds it off, the count
        // standing still for one tick fewer than the interval each time.
        let counts = (0..20).map(|tick| (tick / INTERVAL) as u64 + 1);
        assert_eq!(reclaims(counts), Vec::<usize>::new());
    }

    #[test]
    fn a_quiet_spell_reclaims_once_after_the_interval() {
        // The count moves on the first tick and then holds still.
        assert_eq!(reclaims([5; 20]), vec![INTERVAL]);
    }

    #[test]
    fn the_next_spell_reclaims_again_after_its_own_interval() {
        let counts = [vec![5; 10], vec![6; 11]].concat();
        assert_eq!(reclaims(counts), vec![INTERVAL, 10 + INTERVAL]);
    }
}
