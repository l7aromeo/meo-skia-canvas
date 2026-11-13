//! The pixels the pages on one thread keep from their reads, and the budget
//! they are kept to.
//!
//! A read composites the page into a surface, or for a small read into the
//! tiles it touches, and the page keeps what it composited so that the next
//! read replays only what was drawn since -- see [`RecordingSurface`]. What a
//! page keeps is freed with its canvas, and Node runs a canvas's finalizer
//! only on the turn of the event loop after the collection that finds it
//! dead, so canvases made and read in one synchronous loop would each keep a
//! page until the loop ends.
//!
//! So the pages on a thread share [`PAGE_BUDGET_BYTES`]. Inside it every
//! page keeps what its reads composited. A read that takes the thread past
//! it drops what pages read only once keep, the oldest first, until the
//! thread is inside it again or no such page is left, and a page dropped
//! that way replays every layer at its next read, as it did at its first.
//!
//! A page read more than once is never dropped. It counts against the
//! budget, so pages read once are dropped sooner beside it, but it is not
//! dropped itself: canvases read every frame would otherwise drop each
//! other in turn once there were more of them than the budget holds, each
//! read replaying a page the read before it had dropped -- 1.6 times as
//! long for fifty 600 by 1100 GPU canvases. So the budget bounds canvases
//! read once and left for the collector, which is the loop it exists for,
//! and not canvases read twice and then left, which keep their pages until
//! their finalizers run, as every canvas did before the budget.
//!
//! A thread rather than the process, because a canvas is neither `Send` nor
//! `Sync`: a page is only ever touched by the thread that made it, so the
//! budget reaches into another page's surface without a lock, and each Node
//! worker's canvases are kept to a budget of their own.

use std::{
    cell::{Cell, RefCell},
    collections::{BTreeSet, HashMap},
    ops::Bound,
    rc::{Rc, Weak},
    sync::atomic::{AtomicU64, Ordering},
};

#[cfg(debug_assertions)]
use neon::prelude::{
    Context, FunctionContext, JsNull, JsNumber, JsResult, JsValue,
};

use super::page::RecordingSurface;
use crate::node::external::Reported;

/// What the pages on one thread may keep in pixels from their reads before
/// the oldest are dropped.
///
/// The figure is Skia's own default budget for what a GPU context keeps,
/// `GrResourceCache::kDefaultMaxSize`, so that a thread keeps about as much
/// in pages as its GPU context may in resources. It holds a hundred and one
/// 600 by 1100 pages on the CPU, or thirty-two at 1920 by 1080, and a fifth
/// as many on a GPU multisampling them at the default 4x, whose samples it
/// counts -- see [`RecordingSurface::budgeted_bytes`]. Pages read more than
/// once count against it but are never dropped, so it bounds what canvases
/// read once keep beside them.
pub(crate) const PAGE_BUDGET_BYTES: usize = 256 * (1 << 20);

/// A page's surface as the page and the budget share it: the page owns it,
/// and the budget holds a weak reference through which it can drop what the
/// surface keeps.
pub(crate) type SharedSurface = Rc<RefCell<RecordingSurface>>;

#[cfg(any(test, debug_assertions))]
thread_local! {
    /// A budget a test sets in place of [`PAGE_BUDGET_BYTES`] on this
    /// thread, so that a few small pages can take it past -- see
    /// [`set_page_budget`].
    pub(crate) static BUDGET_OVERRIDE: Cell<Option<usize>> = const { Cell::new(None) };
}

/// The budget in force on this thread.
fn budget() -> usize {
    #[cfg(any(test, debug_assertions))]
    if let Some(bytes) = BUDGET_OVERRIDE.get() {
        return bytes;
    }
    PAGE_BUDGET_BYTES
}

/// How a page is ordered for dropping: the first in order goes first, and
/// no page read again goes at all.
///
/// Fields compare in the order they are declared, so every page read once
/// comes before any page read again, which is where dropping stops, and
/// among pages read once the one read first comes first. `place` breaks no
/// ties -- reads are ticked one at a time -- and makes the key the page's
/// own.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Key {
    read_again: bool,
    last_read: u64,
    place: u64,
}

/// What the budget knows of one page that keeps pixels.
struct Entry {
    surface: Weak<RefCell<RecordingSurface>>,
    /// What the page's canvas has told the JavaScript engine it holds, which
    /// dropping the page frees.
    reported: Reported,
    /// What the page kept when it was last accounted, which every read does,
    /// served or not. Outside a read a page only drops pixels: by being
    /// cleared or resized, which accounts it, or by a read the GPU discarded
    /// a draw of, which leaves it to the next -- so this can be more than
    /// the page keeps, and is never less.
    bytes: usize,
    key: Key,
}

/// The pages on one thread that keep pixels.
#[derive(Default)]
struct Resident {
    entries: HashMap<u64, Entry>,
    order: BTreeSet<Key>,
    /// The sum of every entry's `bytes`, changed only where an entry's is.
    total: usize,
    /// Ticks once a read, to order pages by their last.
    clock: u64,
}

thread_local! {
    static RESIDENT: RefCell<Resident> = RefCell::default();
}

/// Mints each [`Place`] a number of its own.
static PLACES: AtomicU64 = AtomicU64::new(1);

/// How many times the budget has dropped a page, on every thread. Read by
/// tests -- see [`dropped_pages`].
#[cfg(debug_assertions)]
static DROPPED: AtomicU64 = AtomicU64::new(0);

impl Resident {
    /// Takes the entry for `place` out, if it has one.
    fn remove(&mut self, place: u64) {
        if let Some(entry) = self.entries.remove(&place) {
            self.order.remove(&entry.key);
            self.total -= entry.bytes;
        }
    }

    /// Files `place` as keeping `bytes` under `key`, or takes it out where
    /// it keeps nothing.
    fn file(
        &mut self,
        place: &Place,
        surface: &SharedSurface,
        bytes: usize,
        key: Key,
    ) {
        self.remove(place.number);
        if bytes == 0 {
            return;
        }
        self.order.insert(key);
        self.total += bytes;
        let entry = Entry {
            surface: Rc::downgrade(surface),
            reported: place.reported.borrow().clone(),
            bytes,
            key,
        };
        self.entries.insert(place.number, entry);
    }

    /// Drops what other pages read once keep, first in order first, until
    /// the thread is inside its budget or no such page is left.
    ///
    /// `reading` is the page whose read is being served, which keeps what it
    /// just composited. A page whose surface is borrowed is in the middle of
    /// something of its own and is passed over rather than waited for.
    fn drop_past_budget(&mut self, reading: u64) {
        let budget = budget();
        let mut after = Bound::Unbounded;
        while self.total > budget {
            let Some(key) = self
                .order
                .range((after, Bound::Unbounded))
                .take_while(|key| !key.read_again)
                .find(|key| key.place != reading)
                .copied()
            else {
                return;
            };
            let dropped = self.entries.get(&key.place).is_none_or(|entry| {
                match entry.surface.upgrade() {
                    Some(surface) => match surface.try_borrow_mut() {
                        Ok(mut surface) => {
                            surface.drop_kept();
                            entry.reported.freed();
                            true
                        }
                        Err(_) => false,
                    },
                    // Its page went without taking it out, which a page
                    // dropped while this was borrowed does: what it kept
                    // went with it.
                    None => true,
                }
            });
            if dropped {
                self.remove(key.place);
                #[cfg(debug_assertions)]
                DROPPED.fetch_add(1, Ordering::Relaxed);
            } else {
                after = Bound::Excluded(key);
            }
        }
    }
}

/// A page's place in its thread's budget. Dropping it takes the page out.
pub(crate) struct Place {
    number: u64,
    /// How many times the page has been read, kept across what the budget
    /// drops and across clears: a page read more than once is never dropped,
    /// and whether it has been says how the canvas is used, which a clear
    /// does not change.
    reads: Cell<u8>,
    /// What the page's canvas reports to the JavaScript engine -- see
    /// [`Self::reports_through`].
    reported: RefCell<Reported>,
}

impl Place {
    /// A place for a page that has kept nothing yet.
    pub fn new() -> Self {
        Place {
            number: PLACES.fetch_add(1, Ordering::Relaxed),
            reads: Cell::new(0),
            reported: RefCell::default(),
        }
    }

    /// Shares what the page's canvas reports to the JavaScript engine, so
    /// that dropping the page tells the engine of it -- see
    /// [`Reported::freed`]. A page with none, one the crate drew rather than
    /// the binding, is dropped without telling anything.
    pub fn reports_through(&self, reported: Reported) {
        *self.reported.borrow_mut() = reported;
    }

    /// Called once a read of `surface` has run, served or not: accounts what
    /// it keeps now, counts the read, and drops what other pages read once
    /// keep while the thread is past its budget.
    ///
    /// `surface` must not be borrowed, since what it keeps is measured here.
    pub fn served(&self, surface: &SharedSurface) {
        let reads = self.reads.get().saturating_add(1);
        self.reads.set(reads);
        let bytes = surface.borrow().budgeted_bytes();
        RESIDENT.with_borrow_mut(|resident| {
            resident.clock += 1;
            let key = Key {
                read_again: reads > 1,
                last_read: resident.clock,
                place: self.number,
            };
            resident.file(self, surface, bytes, key);
            resident.drop_past_budget(self.number);
        });
    }

    /// Called where the page has dropped what it kept outside a read -- a
    /// resize, or a clear covering the page -- so that the budget stops
    /// counting it. Free where the page kept nothing.
    pub fn emptied(&self) {
        RESIDENT.with_borrow_mut(|resident| resident.remove(self.number));
    }
}

impl Drop for Place {
    /// Takes the page out of its thread's budget. A page dropped while the
    /// budget is borrowed, or while its thread is being torn down, is left
    /// to [`Resident::drop_past_budget`], which takes out a page it finds
    /// gone.
    fn drop(&mut self) {
        let _ = RESIDENT.try_with(|resident| {
            if let Ok(mut resident) = resident.try_borrow_mut() {
                resident.remove(self.number);
            }
        });
    }
}

/// Test-only: how many times the budget has dropped a page, summed over
/// every thread, so that a test can hold pages read inside the budget to
/// never being dropped.
///
/// Exported by debug builds only, as `__cachedParagraphs` is.
#[cfg(debug_assertions)]
pub fn dropped_pages(mut cx: FunctionContext) -> JsResult<JsNumber> {
    Ok(cx.number(DROPPED.load(Ordering::Relaxed) as f64))
}

/// Test-only: sets this thread's budget to the number of bytes given, or
/// back to [`PAGE_BUDGET_BYTES`] given `null`, so that a test can take a few
/// pages past a budget without allocating hundreds of megabytes.
///
/// Exported by debug builds only, as `__cachedParagraphs` is.
#[cfg(debug_assertions)]
pub fn set_page_budget(mut cx: FunctionContext) -> JsResult<JsNull> {
    let bytes = cx.argument::<JsValue>(0)?;
    let budget = match bytes.is_a::<JsNull, _>(&mut cx) {
        true => None,
        false => {
            let bytes = bytes.downcast_or_throw::<JsNumber, _>(&mut cx)?;
            Some(bytes.value(&mut cx).max(0.0) as usize)
        }
    };
    BUDGET_OVERRIDE.set(budget);
    Ok(cx.null())
}

/// Test-only: what the pages on this thread keep in pixels, as the budget
/// counts it.
///
/// Exported by debug builds only, as `__cachedParagraphs` is.
#[cfg(debug_assertions)]
pub fn kept_page_bytes(mut cx: FunctionContext) -> JsResult<JsNumber> {
    Ok(cx.number(kept_on_this_thread() as f64))
}

/// What the pages on this thread keep in pixels, as the budget counts it.
#[cfg(any(test, debug_assertions))]
pub(crate) fn kept_on_this_thread() -> usize {
    RESIDENT.with_borrow(|resident| resident.total)
}
