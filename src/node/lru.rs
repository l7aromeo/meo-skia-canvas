//! A slot arena that keeps its values in order of use, for caches bounded by
//! count and by estimated bytes.
//!
//! Every value lives in a slot of one `Vec`, linked to the values used just
//! before and after it by slot index, so that using a value, adding one and
//! letting the least recently used go are each a constant amount of work, and
//! the bytes held are a running total rather than a sum over the table. A
//! value leaves in any order -- by age, or all at once -- so a handle carries
//! the generation its slot was filled in, and one whose value has gone reads
//! as `None` rather than as whatever filled the slot after it.

#[cfg(test)]
use std::iter::successors;
use std::{mem, num::NonZeroU32};

/// A slot's place in an [`Lru`]: its index plus one, so that an
/// `Option<SlotIndex>` is four bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SlotIndex(NonZeroU32);

impl SlotIndex {
    fn new(index: usize) -> Self {
        let index = u32::try_from(index + 1)
            .ok()
            .and_then(NonZeroU32::new)
            // SAFETY: `u32::MAX` slots would take more than 64 GB for the
            // slots alone, and every owner bounds its `Lru` to hundreds.
            .expect("an Lru holds fewer than u32::MAX slots");
        Self(index)
    }

    fn index(self) -> usize {
        self.0.get() as usize - 1
    }
}

/// A value's handle in an [`Lru`]: the slot, and the generation the value was
/// put in it with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LruId {
    slot: SlotIndex,
    generation: u32,
}

/// A filled slot: the value, what it is counted as holding, and its
/// neighbours in order of use.
struct Kept<T> {
    value: T,
    bytes: usize,
    older: Option<SlotIndex>,
    newer: Option<SlotIndex>,
}

enum Slot<T> {
    Kept { generation: u32, kept: Kept<T> },
    Vacant { next_free: Option<SlotIndex> },
}

/// Values kept least recently used first -- see the module documentation.
pub(crate) struct Lru<T> {
    slots: Vec<Slot<T>>,
    free: Option<SlotIndex>,
    oldest: Option<SlotIndex>,
    newest: Option<SlotIndex>,
    len: usize,
    bytes: usize,
    /// The generation the next value is put in with. One count for the whole
    /// arena rather than one per slot, and never reset, so that a handle from
    /// before [`Lru::clear`] cannot match a slot filled after it. It wraps
    /// after 2^32 values, which a handle held across one draw never sees.
    generations: u32,
}

impl<T> Default for Lru<T> {
    fn default() -> Self {
        Self {
            slots: Vec::new(),
            free: None,
            oldest: None,
            newest: None,
            len: 0,
            bytes: 0,
            generations: 0,
        }
    }
}

impl<T> Lru<T> {
    /// How many values are kept.
    pub(crate) fn len(&self) -> usize {
        self.len
    }

    /// What the kept values are counted as holding together.
    pub(crate) fn bytes(&self) -> usize {
        self.bytes
    }

    fn kept(&self, id: LruId) -> Option<&Kept<T>> {
        match self.slots.get(id.slot.index())? {
            Slot::Kept { generation, kept } if *generation == id.generation => {
                Some(kept)
            }
            _ => None,
        }
    }

    fn kept_mut(&mut self, id: LruId) -> Option<&mut Kept<T>> {
        match self.slots.get_mut(id.slot.index())? {
            Slot::Kept { generation, kept } if *generation == id.generation => {
                Some(kept)
            }
            _ => None,
        }
    }

    /// The links of the value in `slot`, which the caller knows is kept.
    fn links(&mut self, slot: SlotIndex) -> Option<&mut Kept<T>> {
        match self.slots.get_mut(slot.index())? {
            Slot::Kept { kept, .. } => Some(kept),
            Slot::Vacant { .. } => None,
        }
    }

    /// The value `id` was given for, if it is still kept.
    pub(crate) fn get(&self, id: LruId) -> Option<&T> {
        self.kept(id).map(|kept| &kept.value)
    }

    /// The value `id` was given for, to change, if it is still kept.
    pub(crate) fn get_mut(&mut self, id: LruId) -> Option<&mut T> {
        self.kept_mut(id).map(|kept| &mut kept.value)
    }

    /// Takes the value in `slot` out of the order of use, leaving its own
    /// links as they were.
    fn unlink(&mut self, slot: SlotIndex) {
        let Some(kept) = self.links(slot) else {
            return;
        };
        let (older, newer) = (kept.older, kept.newer);
        match older.and_then(|older| self.links(older)) {
            Some(older) => older.newer = newer,
            None => self.oldest = newer,
        }
        match newer.and_then(|newer| self.links(newer)) {
            Some(newer) => newer.older = older,
            None => self.newest = older,
        }
    }

    /// Puts the value in `slot` at the newest end of the order of use.
    fn link_newest(&mut self, slot: SlotIndex) {
        let newest = self.newest;
        if let Some(kept) = self.links(slot) {
            kept.older = newest;
            kept.newer = None;
        }
        match newest.and_then(|newest| self.links(newest)) {
            Some(newest) => newest.newer = Some(slot),
            None => self.oldest = Some(slot),
        }
        self.newest = Some(slot);
    }

    /// Marks the value `id` was given for as the most recently used,
    /// answering it, or `None` where it has gone.
    pub(crate) fn touch(&mut self, id: LruId) -> Option<&T> {
        self.kept(id)?;
        if self.newest != Some(id.slot) {
            self.unlink(id.slot);
            self.link_newest(id.slot);
        }
        self.get(id)
    }

    /// Keeps `value`, counted as holding `bytes`, as the most recently used.
    pub(crate) fn insert(&mut self, value: T, bytes: usize) -> LruId {
        let generation = self.generations;
        self.generations = self.generations.wrapping_add(1);
        let filled = Slot::Kept {
            generation,
            kept: Kept {
                value,
                bytes,
                older: None,
                newer: None,
            },
        };
        let slot = match self.free {
            Some(slot) => {
                if let Some(Slot::Vacant { next_free }) =
                    self.slots.get(slot.index())
                {
                    self.free = *next_free;
                }
                self.slots[slot.index()] = filled;
                slot
            }
            None => {
                self.slots.push(filled);
                SlotIndex::new(self.slots.len() - 1)
            }
        };
        self.link_newest(slot);
        self.len += 1;
        self.bytes += bytes;
        LruId { slot, generation }
    }

    /// Lets the value `id` was given for go, answering it, or `None` where
    /// it has gone already.
    pub(crate) fn remove(&mut self, id: LruId) -> Option<T> {
        self.kept(id)?;
        self.unlink(id.slot);
        let vacant = Slot::Vacant {
            next_free: self.free,
        };
        let Slot::Kept { kept, .. } =
            mem::replace(&mut self.slots[id.slot.index()], vacant)
        else {
            return None;
        };
        self.free = Some(id.slot);
        self.len -= 1;
        self.bytes -= kept.bytes;
        Some(kept.value)
    }

    /// The least recently used value's handle.
    pub(crate) fn oldest(&self) -> Option<LruId> {
        let slot = self.oldest?;
        match self.slots.get(slot.index())? {
            Slot::Kept { generation, .. } => Some(LruId {
                slot,
                generation: *generation,
            }),
            Slot::Vacant { .. } => None,
        }
    }

    /// Lets every value go, and the slots with them.
    pub(crate) fn clear(&mut self) {
        *self = Self {
            generations: self.generations,
            ..Self::default()
        };
    }

    /// The kept values, least recently used first.
    #[cfg(test)]
    pub(crate) fn values(&self) -> impl Iterator<Item = &T> {
        successors(self.oldest, |slot| match &self.slots[slot.index()] {
            Slot::Kept { kept, .. } => kept.newer,
            Slot::Vacant { .. } => None,
        })
        .filter_map(|slot| match &self.slots[slot.index()] {
            Slot::Kept { kept, .. } => Some(&kept.value),
            Slot::Vacant { .. } => None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::Lru;

    /// The values, least recently used first.
    fn order(lru: &Lru<&'static str>) -> Vec<&'static str> {
        lru.values().copied().collect()
    }

    #[test]
    fn values_are_kept_least_recently_used_first() {
        let mut lru = Lru::default();
        let a = lru.insert("a", 1);
        let _b = lru.insert("b", 1);
        let _c = lru.insert("c", 1);
        assert_eq!(order(&lru), ["a", "b", "c"]);
        assert_eq!(lru.touch(a), Some(&"a"));
        assert_eq!(order(&lru), ["b", "c", "a"], "a used again is the newest");
        let oldest = lru.oldest().expect("a value is kept");
        assert_eq!(lru.remove(oldest), Some("b"), "and b is the oldest");
        assert_eq!(order(&lru), ["c", "a"]);
    }

    #[test]
    fn a_value_removed_from_the_middle_leaves_the_order_whole() {
        let mut lru = Lru::default();
        let ids: Vec<_> = ["a", "b", "c", "d"]
            .into_iter()
            .map(|value| lru.insert(value, 1))
            .collect();
        assert_eq!(lru.remove(ids[1]), Some("b"));
        assert_eq!(order(&lru), ["a", "c", "d"]);
        assert_eq!(lru.remove(ids[3]), Some("d"));
        assert_eq!(order(&lru), ["a", "c"]);
        assert_eq!(lru.remove(ids[0]), Some("a"));
        assert_eq!(order(&lru), ["c"]);
        assert_eq!(lru.len(), 1);
    }

    #[test]
    fn the_bytes_are_a_running_total() {
        let mut lru = Lru::default();
        let a = lru.insert("a", 100);
        let b = lru.insert("b", 20);
        assert_eq!(lru.bytes(), 120);
        lru.remove(b);
        assert_eq!(lru.bytes(), 100, "a value let go is no longer counted");
        lru.remove(a);
        assert_eq!(lru.bytes(), 0);
        lru.insert("c", 7);
        lru.clear();
        assert_eq!((lru.len(), lru.bytes()), (0, 0));
    }

    #[test]
    fn a_handle_to_a_value_that_has_gone_reads_as_none() {
        // b's slot is reused by c: the handle to b must not reach c.
        let mut lru = Lru::default();
        let _a = lru.insert("a", 1);
        let b = lru.insert("b", 10);
        lru.remove(b);
        let c = lru.insert("c", 1);
        assert_eq!(lru.get(b), None, "not the value now in its slot");
        assert_eq!(lru.touch(b), None);
        assert_eq!(lru.remove(b), None);
        assert_eq!(lru.get(c), Some(&"c"));
        assert_eq!(lru.bytes(), 2);
    }

    #[test]
    fn a_handle_from_before_a_clear_reads_as_none() {
        let mut lru = Lru::default();
        let a = lru.insert("a", 1);
        lru.clear();
        let b = lru.insert("b", 1);
        assert_eq!(lru.get(a), None, "the same slot, filled after the clear");
        assert_eq!(lru.get(b), Some(&"b"));
    }
}
