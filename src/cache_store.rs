//! A cache of values counted by their bytes, kept to a [`Budget`]: the
//! store behind the chain results of
//! [`chain_cache`](crate::context::chain_cache) and the baked rectangles of
//! [`blur_mask`](crate::context::blur_mask).
//!
//! Each cache is a [`Store`] of its own, behind a process-wide lock of its
//! own and released by an idle release of its own. Two stores share
//! nothing, so neither's entries can evict the other's.

use std::hash::Hash;

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
}

/// Entries filed under keys of `K`, kept to a [`Budget`] of their bytes:
/// past it, an insert lets go of those whose key is orphaned, then of the
/// least recently used down to the budget's low-water mark.
pub(crate) struct Store<K, V> {
    budget: Budget,
    entries: CacheMap<K, Entry<V>>,
    /// The bytes of every entry's value, kept as entries come and go.
    bytes: usize,
    /// How many entries were left by the last sweep for entries whose key is
    /// orphaned -- see [`Store::put`].
    swept_at: usize,
    /// Ticks once per use, to order entries for eviction.
    uses: u64,
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
        let entry = self.entries.get_mut(key)?;
        entry.used = self.uses;
        self.uses += 1;
        Some(entry.value.clone())
    }

    /// Files `value` under `key`, unless it is past the budget's
    /// [`most`](Budget::most).
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
        self.bytes += bytes;
        if let Some(old) =
            self.entries.insert(key, Entry { value, bytes, used })
        {
            self.bytes -= old.bytes;
        }
        if self.bytes > self.budget.bytes {
            // Orphaned entries go before any that may be used.
            self.sweep();
        }
        if self.bytes > self.budget.bytes {
            let mut order: Vec<(u64, K, usize)> = self
                .entries
                .iter()
                .map(|(key, entry)| (entry.used, key.clone(), entry.bytes))
                .collect();
            order.sort_unstable_by_key(|(used, _, _)| *used);
            for (_, key, bytes) in order {
                if self.bytes <= self.budget.low_water {
                    break;
                }
                self.entries.remove(&key);
                self.bytes -= bytes;
            }
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
