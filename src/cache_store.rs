//! A cache of values counted by their bytes, kept to a [`Budget`]: the
//! store behind the chain results of
//! [`chain_cache`](crate::context::chain_cache), the baked rectangles of
//! [`blur_mask`](crate::context::blur_mask) and the blurred runs of text of
//! [`run_mask`](crate::context::run_mask).
//!
//! Each cache is a [`Store`] of its own, behind a process-wide lock of its
//! own and released by an idle release of its own. Two stores share
//! nothing, so neither's entries can evict the other's.
//!
//! # What a full store lets in
//!
//! A page replayed in the same order asks for its entries in a cycle. Where
//! the cycle does not fit, letting go of the least recently used entry lets
//! go of the next one the page asks for, so each insert evicts what the rest
//! of the page needs and the page finds nothing at all. So a full store
//! weighs a newcomer against the entry it would evict, by how far apart
//! requests for each fall, counted in requests to the store:
//!
//! - Its reach is the longest gap between two requests for one key it has seen
//!   lately.
//! - An entry is in use if it was asked for again after it was filed and has
//!   been idle no longer than the reach. One that is not is out of whatever
//!   cycle is being replayed, and goes for anything, as it would least recently
//!   used.
//! - An entry in use is protected: a newcomer takes its place only if the
//!   newcomer was asked for again sooner than the entry has been idle, which
//!   the store knows from the keys it remembers having turned away or let go
//!   of. Every request takes a tick of its own, so the two are never equal.
//!   Making room for a newcomer lets go of entries in use only down to the
//!   budget, not to its low-water mark.
//!
//! A cycle larger than the store then keeps the share that fits and finds it
//! on every page from the third, and the rest miss. Entries never asked for
//! again are never in use, so a store of them keeps the newest, as least
//! recently used eviction does. A page that changes leaves the old entries
//! idle past the reach within the page, and the new ones take their place
//! by the second page.
//!
//! TinyLFU's rule, admitting a newcomer asked for more often than its
//! victim, was weighed and not taken: counts of the old page's entries
//! outweigh the new page's until ageing halves them, so a page that changes
//! is displaced only after as many pages as the counts took to build.

use std::hash::{BuildHasher, Hash};

use crate::cache_map::CacheMap;

/// How many bytes a [`Store`] keeps, and what it cuts them back to.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Budget {
    /// The bytes of every entry together, past which an insert evicts.
    pub(crate) bytes: usize,
    /// The largest single entry that is kept at all, so that one large
    /// entry cannot push out every smaller one.
    pub(crate) most: usize,
    /// What an eviction cuts the entries back to, below [`Budget::bytes`]
    /// so that the inserts after it do not each evict.
    pub(crate) low_water: usize,
}

/// What a [`Store`] files its entries under.
pub(crate) trait StoreKey: Clone + Eq + Hash {
    /// Whether what the entry was made for has gone, so that nothing can
    /// ask for it again: such entries go at the next sweep -- see
    /// [`Store::put`].
    fn is_orphaned(&self) -> bool;
}

/// What a [`Store`] keeps: cloned out on a hit, and counted by its bytes.
pub(crate) trait Stored: Clone {
    /// The bytes the entry holds, counted against its store's [`Budget`].
    fn bytes(&self) -> usize;
}

struct Entry<V> {
    value: V,
    bytes: usize,
    used: u64,
    /// Whether the key was asked for again after it was first filed: found,
    /// filed again, or filed after the store had turned it away or let it go.
    /// Only such an entry is protected, so that one key asked for again does
    /// not protect every entry filed once.
    reused: bool,
}

/// How many keys a [`Store`] remembers having turned away or let go of,
/// with when each was last asked for. Chosen to outlast a cycle of a few
/// hundred entries, the most a store's budget holds of the results a page
/// repeats, at 24 bytes a slot; a slot two keys share remembers the later,
/// and the earlier is a newcomer it has never seen.
const REMEMBERED: usize = 1024;

/// How fast a [`Store`]'s reach forgets a long gap: each gap seen takes
/// `1 / 2^REACH_DECAY` of the reach off before the gap is weighed, so one
/// key asked for again after a long while cannot protect every entry for
/// long.
const REACH_DECAY: u32 = 4;

/// Entries filed under keys of `K`, kept to a [`Budget`] of their bytes.
/// Past it, an insert lets go of those whose key is orphaned, then of the
/// least recently used down to the budget's low-water mark, and a newcomer
/// that would evict an entry still in use is turned away -- see the module
/// documentation.
pub(crate) struct Store<K, V> {
    budget: Budget,
    entries: CacheMap<K, Entry<V>>,
    /// The bytes of every entry's value, kept as entries come and go.
    bytes: usize,
    /// How many entries were left by the last sweep for entries whose key is
    /// orphaned -- see [`Store::put`].
    swept_at: usize,
    /// Ticks once per request found or filed, to order entries for
    /// eviction and to measure the gaps between requests.
    uses: u64,
    /// The longest gap lately between two requests for one key, in ticks
    /// of [`uses`](Self::uses).
    reach: u64,
    /// Keys turned away or let go of, by a slot their hash picks, with their
    /// hash and the tick they were last asked for at: empty until the store
    /// first fills, and [`REMEMBERED`] long after.
    remembered: Vec<Option<(u64, u64)>>,
    /// How many sweeps the store has made, for the tests to count.
    #[cfg(test)]
    sweeps: usize,
}

impl<K: StoreKey, V: Stored> Store<K, V> {
    /// An empty store kept to `budget`.
    pub(crate) fn new(budget: Budget) -> Self {
        Self {
            budget,
            entries: CacheMap::default(),
            bytes: 0,
            swept_at: 0,
            uses: 0,
            reach: 0,
            remembered: Vec::new(),
            #[cfg(test)]
            sweeps: 0,
        }
    }

    /// Drops every entry: what each store's idle release does, when
    /// rendering has been idle.
    pub(crate) fn release(&mut self) {
        self.entries.clear();
        self.bytes = 0;
        self.swept_at = 0;
        self.reach = 0;
        self.remembered = Vec::new();
    }

    /// Weighs `gap`, ticks between two requests for one key, into the reach.
    fn saw_gap(&mut self, gap: u64) {
        self.reach = gap.max(self.reach - (self.reach >> REACH_DECAY));
    }

    /// Remembers that the key hashing to `hash` was last asked for at `at`.
    fn remember(&mut self, hash: u64, at: u64) {
        if self.remembered.is_empty() {
            self.remembered = vec![None; REMEMBERED];
        }
        let slot = (hash % REMEMBERED as u64) as usize;
        self.remembered[slot] = Some((hash, at));
    }

    /// How many ticks before `now` the key hashing to `hash` was last asked
    /// for, if it is remembered. The slot is left as it is: a key recalled
    /// is either turned away or let in, and remembered afresh when it is
    /// turned away or let go of again.
    fn recall(&self, hash: u64, now: u64) -> Option<u64> {
        match self.remembered.get((hash % REMEMBERED as u64) as usize)? {
            Some((held, at)) if *held == hash => Some(now - at),
            _ => None,
        }
    }

    /// Whether an entry last used at `used`, and `reused` or not, is still
    /// in use at `now`.
    fn in_use(&self, used: u64, reused: bool, now: u64) -> bool {
        reused && now - used <= self.reach
    }

    /// Whether a newcomer asked for again `gap` ticks after it was last, if
    /// it was, takes the place of the least recently used entry at `now`.
    fn admits(&self, gap: Option<u64>, now: u64) -> bool {
        let Some(oldest) = self.entries.values().min_by_key(|entry| entry.used)
        else {
            return true;
        };
        !self.in_use(oldest.used, oldest.reused, now)
            || gap.is_some_and(|gap| gap < now - oldest.used)
    }

    /// Drops every entry whose key is orphaned, and notes how many are left.
    fn sweep(&mut self) {
        let mut freed = 0;
        self.entries.retain(|key, entry| {
            let orphaned = key.is_orphaned();
            if orphaned {
                freed += entry.bytes;
            }
            !orphaned
        });
        self.bytes -= freed;
        self.swept_at = self.entries.len();
        #[cfg(test)]
        {
            self.sweeps += 1;
        }
    }

    /// The value filed under `key`, if one is, counted as a use.
    pub(crate) fn get(&mut self, key: &K) -> Option<V> {
        let now = self.uses;
        let entry = self.entries.get_mut(key)?;
        let gap = now - entry.used;
        entry.used = now;
        entry.reused = true;
        let value = entry.value.clone();
        self.uses += 1;
        self.saw_gap(gap);
        Some(value)
    }

    /// Files `value` under `key`, unless it is past the budget's
    /// [`most`](Budget::most) or the store is full of entries still in use
    /// -- see the module documentation.
    pub(crate) fn put(&mut self, key: K, value: V) {
        // An orphaned key's entry goes here rather than when what it was
        // made for goes, which would have to reach this lock from wherever
        // that happens: once the entries have doubled since the last sweep,
        // so that a sweep costs each insert a constant share of one and the
        // orphaned entries never outnumber those left by the sweep before.
        // Swept on every insert, each filtered draw of a page walked every
        // chain result kept, 9% of a page of drop shadows' time.
        if self.entries.len() >= 2 * self.swept_at {
            self.sweep();
        }
        let bytes = value.bytes();
        if bytes > self.budget.most {
            return;
        }
        let used = self.uses;
        self.uses += 1;
        let hash = self.entries.hasher().hash_one(&key);
        let gap = match self.entries.get(&key) {
            Some(held) => Some(used - held.used),
            None => self.recall(hash, used),
        };
        if let Some(gap) = gap {
            self.saw_gap(gap);
        }
        let fits = |store: &Self| {
            let held = store.entries.get(&key).map_or(0, |entry| entry.bytes);
            store.bytes - held + bytes <= store.budget.bytes
        };
        if !fits(self) {
            // Orphaned entries go before any that may be used.
            self.sweep();
            if !fits(self)
                && !self.entries.contains_key(&key)
                && !self.admits(gap, used)
            {
                self.remember(hash, used);
                return;
            }
        }
        self.bytes += bytes;
        if let Some(old) = self.entries.insert(
            key,
            Entry {
                value,
                bytes,
                used,
                reused: gap.is_some(),
            },
        ) {
            self.bytes -= old.bytes;
        }
        if self.bytes > self.budget.bytes {
            self.evict(used);
        }
    }

    /// Lets go of the least recently used entries at `now`: down to the
    /// budget's low-water mark while they are not in use, and only down to
    /// the budget once they are, so that making room for one
    /// newcomer does not let go of a quarter of a cycle still being
    /// replayed.
    fn evict(&mut self, now: u64) {
        let mut order: Vec<(u64, bool, K, usize)> = self
            .entries
            .iter()
            .map(|(key, entry)| {
                (entry.used, entry.reused, key.clone(), entry.bytes)
            })
            .collect();
        order.sort_unstable_by_key(|(used, ..)| *used);
        for (used, reused, key, bytes) in order {
            if self.bytes <= self.budget.low_water
                || (self.in_use(used, reused, now)
                    && self.bytes <= self.budget.bytes)
            {
                break;
            }
            let hash = self.entries.hasher().hash_one(&key);
            self.entries.remove(&key);
            self.bytes -= bytes;
            self.remember(hash, used);
        }
    }
}

#[cfg(test)]
impl<K, V> Store<K, V> {
    /// The bytes the store counts.
    pub(crate) fn bytes(&self) -> usize {
        self.bytes
    }

    /// How many entries the store holds.
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    /// The bytes of every entry the store holds, summed afresh.
    pub(crate) fn held(&self) -> usize {
        self.entries.values().map(|entry| entry.bytes).sum()
    }

    /// How many sweeps the store has made.
    pub(crate) fn sweeps(&self) -> usize {
        self.sweeps
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Debug, PartialEq, Eq, Hash)]
    struct Key(u32);

    impl StoreKey for Key {
        fn is_orphaned(&self) -> bool {
            false
        }
    }

    #[derive(Clone)]
    struct Held(usize);

    impl Stored for Held {
        fn bytes(&self) -> usize {
            self.0
        }
    }

    /// The bytes of every entry these tests file.
    const ENTRY: usize = 100;

    /// Room for ten entries, cut back to seven and a half as the stores'
    /// own budgets cut back to three quarters.
    const BUDGET: Budget = Budget {
        bytes: 10 * ENTRY,
        most: 10 * ENTRY,
        low_water: 10 * ENTRY * 3 / 4,
    };

    /// Asks `store` for each of `keys` in turn and files each it misses, as
    /// a page replaying its draws does; how many it found.
    fn page(
        store: &mut Store<Key, Held>,
        keys: impl IntoIterator<Item = u32>,
    ) -> usize {
        keys.into_iter()
            .filter(|&n| {
                let found = store.get(&Key(n)).is_some();
                if !found {
                    store.put(Key(n), Held(ENTRY));
                }
                found
            })
            .count()
    }

    #[test]
    fn a_repeated_page_larger_than_the_store_keeps_what_fits() {
        let mut store = Store::new(BUDGET);
        let found: Vec<usize> =
            (0..8).map(|_| page(&mut store, 0..12)).collect();
        // Ten of the twelve fit. Evicting the least recently used, a page
        // replayed in order lets go of each entry just before it is asked
        // for, and finds none.
        assert!(
            found[2..].iter().all(|&n| n == 10),
            "a page of twelve over room for ten found {found:?}"
        );
        assert!(store.bytes() <= BUDGET.bytes);
    }

    #[test]
    fn a_page_that_changes_displaces_the_last_within_two_pages() {
        let mut store = Store::new(BUDGET);
        for _ in 0..4 {
            page(&mut store, 0..12);
        }
        let found: Vec<usize> =
            (0..4).map(|_| page(&mut store, 100..108)).collect();
        assert!(
            found[2..].iter().all(|&n| n == 8),
            "a page of eight that fits, after one of twelve, found {found:?}"
        );
    }

    #[test]
    fn keys_never_asked_for_again_are_kept_newest_first() {
        let mut store = Store::new(BUDGET);
        page(&mut store, 0..50);
        let kept: Vec<u32> =
            (0..50).filter(|&n| store.get(&Key(n)).is_some()).collect();
        assert!(
            (43..50).all(|n| kept.contains(&n)),
            "the newest seven are kept: {kept:?}"
        );
    }

    #[test]
    fn a_key_asked_for_more_often_than_the_oldest_is_let_in() {
        let mut store = Store::new(BUDGET);
        // Twelve asked for once a page, and one asked for three times late
        // in it, after the store has filled.
        let keys = || (0..12).chain([99, 50, 99, 51, 99]);
        let found: Vec<bool> = (0..8)
            .map(|_| {
                page(&mut store, keys());
                store.get(&Key(99)).is_some()
            })
            .collect();
        assert!(
            found[2..].iter().all(|&kept| kept),
            "the key asked for three times a page was kept on {found:?}"
        );
        assert!(page(&mut store, keys()) >= 9, "the rest still found");
    }
    /// A store holding as many entries as it has room for, each asked for
    /// again once, so that every one is in use.
    fn full_and_in_use() -> Store<Key, Held> {
        let mut store = Store::new(BUDGET);
        let room = (BUDGET.bytes / ENTRY) as u32;
        for _ in 0..2 {
            page(&mut store, 0..room);
        }
        store
    }

    #[test]
    fn a_newcomer_asked_for_again_soon_takes_one_place_in_use() {
        let mut store = full_and_in_use();
        let room = store.len();
        store.put(Key(99), Held(ENTRY));
        // The oldest asked for again, so that the entry 99 would evict is
        // idle no longer than the reach and still in use.
        assert!(store.get(&Key(0)).is_some());
        assert!(
            store.get(&Key(99)).is_none(),
            "turned away, never asked for before"
        );
        store.put(Key(99), Held(ENTRY));
        assert!(
            store.get(&Key(99)).is_some(),
            "let in, asked for again sooner"
        );
        // Room for it is made by one entry in use, not by cutting back to
        // the low-water mark.
        assert_eq!(store.len(), room);
    }

    #[test]
    fn one_long_gap_does_not_protect_the_entries_for_long() {
        let mut store = full_and_in_use();
        // Key 0 asked for again after a thousand requests for the others
        // stretches the reach to a thousand.
        for _ in 0..1000 / 9 {
            page(&mut store, 1..10);
        }
        page(&mut store, [0]);
        // Gaps of ten from then on bring it back down.
        for _ in 0..20 {
            page(&mut store, 0..10);
        }
        page(&mut store, 100..150);
        let kept: Vec<u32> = (100..150)
            .filter(|&n| store.get(&Key(n)).is_some())
            .collect();
        assert!(
            (143..150).all(|n| kept.contains(&n)),
            "the newest seven are kept once the old entries go idle: {kept:?}"
        );
    }
}
