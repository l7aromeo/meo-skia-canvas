//! The maps a cache of this crate's own lookups keeps its answers in: the
//! font, face and collection caches, keyed by what a style asks for.
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
//! decoded -- stays a `std` map with SipHash.

use std::collections::{HashMap, HashSet};

/// The hasher of [`CacheMap`] and [`CacheSet`].
pub(crate) type CacheHasher = foldhash::fast::RandomState;

/// A `HashMap` for a cache of the crate's own lookups -- see the module
/// documentation for where it must not be used.
pub(crate) type CacheMap<K, V> = HashMap<K, V, CacheHasher>;

/// A `HashSet` for a cache of the crate's own lookups -- see [`CacheMap`].
pub(crate) type CacheSet<T> = HashSet<T, CacheHasher>;
