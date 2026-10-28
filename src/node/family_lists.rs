//! The family lists a library has resolved while its fonts stand, each filed
//! once and handed out as a [`ListRef`]: what a cache keyed on a family list
//! holds in place of the list's strings.
//!
//! A list is filed by the names it resolves to, so two lists the library
//! asks for in the same names share one entry. Lists that name one
//! registered family in two cases resolve to its registered spelling, and so
//! share -- CSS Fonts 4 section 5.1 matches family names without regard to
//! case, by the rule in `font::caseless`. Lists whose names resolve apart do
//! not: a quoted `"serif"`, which CSS matches as a keyword in ASCII case
//! alone, is asked for by a name nothing has unless a family is registered
//! under it, while `"ſerif"` is passed to the platform as written. Filing by
//! the names rather than by any reading of the families keeps every entry
//! exact by construction: whatever a search answers for a list, it answers
//! from those names.

use crate::{font::FontFamily, node::font_library::MATCHES_KEPT};
use std::{
    collections::HashMap,
    num::NonZeroU32,
    rc::Rc,
    sync::atomic::{AtomicU64, Ordering},
};

/// A family list's place in the arena that filed it, from 1.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) struct ListId(NonZeroU32);

/// A family list as one generation of a [`FamilyLists`] filed it.
///
/// Equal refs name the same list: a generation is drawn from a counter the
/// whole process shares, so no two arenas -- the shared library's on each
/// thread, or one built for a passed epoch -- nor any arena before and after
/// a clear, hand out the same one. A ref kept past a clear is a miss in a
/// cache keyed on it, never another list's answer.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) struct ListRef {
    generation: u64,
    id: ListId,
}

/// The next generation any [`FamilyLists`] takes, process-wide.
static GENERATIONS: AtomicU64 = AtomicU64::new(1);

/// The family lists filed in one generation -- see the module documentation.
pub(crate) struct FamilyLists {
    generation: u64,
    /// Each list as asked, to the list it is filed as.
    asked: HashMap<Vec<FontFamily>, ListId>,
    /// Each list's names, to the list they are filed as.
    resolved: HashMap<Rc<[String]>, ListId>,
    /// Each list's names, at its place less one.
    names: Vec<Rc<[String]>>,
}

impl Default for FamilyLists {
    fn default() -> Self {
        Self {
            generation: GENERATIONS.fetch_add(1, Ordering::Relaxed),
            asked: HashMap::new(),
            resolved: HashMap::new(),
            names: Vec::new(),
        }
    }
}

impl FamilyLists {
    /// The list `families` is filed as, and its names, where it has been
    /// asked for in this generation.
    pub(crate) fn get(
        &self,
        families: &[FontFamily],
    ) -> Option<(ListRef, Rc<[String]>)> {
        let id = *self.asked.get(families)?;
        Some((self.list_ref(id), self.names_of(id)))
    }

    /// Files `families`, which resolve to `names`: as the list already filed
    /// under those names where there is one, and otherwise as a new list.
    /// Past [`MATCHES_KEPT`] lists as asked, the arena is cleared first,
    /// the bound the library's other memos keep.
    pub(crate) fn file(
        &mut self,
        families: &[FontFamily],
        names: Rc<[String]>,
    ) -> (ListRef, Rc<[String]>) {
        if self.asked.len() >= MATCHES_KEPT {
            self.clear();
        }
        let id = match self.resolved.get(&names) {
            Some(id) => *id,
            None => {
                // SAFETY: the clear above keeps fewer than `MATCHES_KEPT`
                // lists as asked, and so fewer names, far below `u32::MAX`.
                let place = u32::try_from(self.names.len() + 1)
                    .expect("fewer lists than MATCHES_KEPT");
                // SAFETY: one more than a length is never zero.
                let id = ListId(NonZeroU32::new(place).expect("from 1"));
                self.names.push(names.clone());
                self.resolved.insert(names, id);
                id
            }
        };
        self.asked.insert(families.to_vec(), id);
        (self.list_ref(id), self.names_of(id))
    }

    /// Forgets every list and starts a new generation, so that no ref
    /// handed out before names a list filed after.
    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }

    /// How many lists, as asked, are filed.
    pub(crate) fn len(&self) -> usize {
        self.asked.len()
    }

    fn list_ref(&self, id: ListId) -> ListRef {
        ListRef {
            generation: self.generation,
            id,
        }
    }

    /// The names of `id`, which this generation handed out: no id from
    /// another reaches here, since both maps are this generation's own.
    fn names_of(&self, id: ListId) -> Rc<[String]> {
        let place = id.0.get() as usize - 1;
        self.names[place].clone()
    }
}
