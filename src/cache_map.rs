//! The maps a cache of this crate's own lookups keeps its answers in: the
//! font, face and collection caches, keyed by what a style asks for, and the
//! runs kept for drawing, filed by a hash of what they lay out; and
//! [`KeptSets`], a fixed number of answers for lookups whose keys repeat.
//!
//! They hash with foldhash rather than std's SipHash. A lookup in these caches
//! hashes a short key -- a family list, a style, a face id -- and SipHash's
//! cost there is its setup and finalisation on every call rather than the
//! bytes it reads, so the hash was a few per cent of a plain `fillText`
//! across them. foldhash is seeded per process, which is not a defence
//! against an adversary who can choose keys to collide.
//!
//! So these maps are for keys the crate builds from fonts and styles, not
//! for keys taken from untrusted input as such: a map keyed by bytes an
//! outside party chooses -- a URL, a request, the contents of a file being
//! decoded -- stays a `std` map with SipHash. The one exception is a map
//! that holds no more than a fixed number of keys which are themselves
//! SipHash digests of such input, taken with keys chosen at random, as the
//! runs kept for drawing in `node::typography` are: what such a key will be
//! cannot be worked out ahead, and a collision among them costs at most a
//! probe through that many entries.

use std::{
    borrow::Borrow,
    collections::{HashMap, HashSet},
    ops::Range,
};

/// The hasher of [`CacheMap`] and [`CacheSet`].
pub(crate) type CacheHasher = foldhash::fast::RandomState;

/// A `HashMap` for a cache of the crate's own lookups -- see the module
/// documentation for where it must not be used.
pub(crate) type CacheMap<K, V> = HashMap<K, V, CacheHasher>;

/// A `HashSet` for a cache of the crate's own lookups -- see [`CacheMap`].
pub(crate) type CacheSet<T> = HashSet<T, CacheHasher>;

/// A fixed number of a lookup's answers, `SETS` sets of `WAYS`, each answer
/// filed in the set its key's hash names: a set holds its last `WAYS`
/// answers, the one last filed first, and an answer filed into a full set
/// pushes out the oldest. A key is looked for only in its own set, so a
/// lookup compares at most `WAYS` keys whatever the table holds.
///
/// Nothing is allocated until the first answer is filed, so a table no draw
/// files into costs only its own size. `SETS` is a power of two, and the set
/// is taken from the top bits of the hash -- the ones a multiplicative hash
/// mixes every bit of the key into.
pub(crate) struct KeptSets<K, V, const SETS: usize, const WAYS: usize> {
    entries: Vec<Option<(K, V)>>,
}

impl<K, V, const SETS: usize, const WAYS: usize> KeptSets<K, V, SETS, WAYS> {
    const SHIFT: u32 = {
        assert!(SETS.is_power_of_two() && WAYS > 0);
        u64::BITS - SETS.trailing_zeros()
    };

    /// An empty table, which allocates nothing yet.
    pub(crate) const fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Where the set `hash` names lies in `entries`.
    fn set(hash: u64) -> Range<usize> {
        // `checked_shr`: one set takes no bits, and a shift by all 64 does
        // not leave zero.
        let set = hash.checked_shr(Self::SHIFT).unwrap_or(0) as usize;
        set * WAYS..(set + 1) * WAYS
    }

    /// The answer kept for `key`, whose hash is `hash`.
    pub(crate) fn get<Q>(&self, hash: u64, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: PartialEq + ?Sized,
    {
        self.entries
            .get(Self::set(hash))?
            .iter()
            .flatten()
            .find(|(kept, _)| kept.borrow() == key)
            .map(|(_, value)| value)
    }

    /// Files `value` as the answer for `key`, whose hash is `hash`, first in
    /// its set, the oldest of the set falling off the end.
    pub(crate) fn file(&mut self, hash: u64, key: K, value: V) {
        if self.entries.is_empty() {
            self.entries.resize_with(SETS * WAYS, || None);
        }
        let set = &mut self.entries[Self::set(hash)];
        set.rotate_right(1);
        set[0] = Some((key, value));
    }
}

impl<K, V, const SETS: usize, const WAYS: usize> Default
    for KeptSets<K, V, SETS, WAYS>
{
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::KeptSets;

    /// A set holds its last `WAYS` keys and pushes out the oldest; a key in
    /// another set is not disturbed; and nothing is allocated before the
    /// first key is filed.
    #[test]
    fn a_set_keeps_its_last_ways_and_no_more() {
        let mut kept = KeptSets::<u32, u32, 4, 2>::new();
        assert_eq!(kept.entries.capacity(), 0, "nothing allocated yet");
        assert_eq!(kept.get(0, &1), None);
        // The top two bits name the set: 0 and 3.
        let [first, last] = [0_u64, 3 << 62];
        kept.file(first, 1, 10);
        kept.file(last, 9, 90);
        kept.file(first, 2, 20);
        assert_eq!(
            (kept.get(first, &1), kept.get(first, &2)),
            (Some(&10), Some(&20))
        );
        kept.file(first, 3, 30);
        assert_eq!(kept.get(first, &1), None, "the oldest of a full set goes");
        assert_eq!(
            (kept.get(first, &2), kept.get(first, &3)),
            (Some(&20), Some(&30))
        );
        assert_eq!(kept.get(last, &9), Some(&90), "another set keeps its own");
        assert_eq!(
            kept.get(last, &2),
            None,
            "a key is looked for in its own set"
        );
    }

    /// One set takes every hash, the shift by all 64 bits that leaves none.
    #[test]
    fn one_set_takes_every_hash() {
        let mut kept = KeptSets::<u32, u32, 1, 2>::new();
        kept.file(u64::MAX, 1, 10);
        assert_eq!(kept.get(0, &1), Some(&10));
    }
}
