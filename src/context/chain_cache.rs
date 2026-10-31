//! What a [`Group`]'s filter chain left, kept so that playing the group
//! again does not run the chain again.
//!
//! A canvas drawn into another brings its page's entries with it, shared,
//! and so does a canvas drawn into itself: each round of drawing a page into
//! itself through a blur files a group holding every group the rounds before
//! it filed. Played back, each round's group replays all of those, and
//! running their chains again at each replay grows with the square of the
//! rounds: 64 chains for eight rounds and 256 for sixteen, and on a
//! 900-pixel page in a debug build 1735 ms against 6755 ms. Kept, each
//! group's chain runs once onto pixels and once into the picture a canvas
//! drawn into another is recorded as, and the same page takes 412 ms and
//! 844 ms.
//!
//! A result is filed under what decides it: which group, the transform and
//! the clip's bounds the chain is played under -- which fix where its layer
//! lies and what it reads -- and the colour type and space of what it is
//! played onto. The clip's bounds and not its shape: the chain reads past
//! the clip, and the shape only decides what of the result shows, which the
//! composite applies afresh each time.
//!
//! On the CPU only. A GPU result lives on its thread's context and has to
//! be let go there, and a GPU chain's layer takes the page's sample count,
//! which a canvas does not tell without handing out its surface, which
//! skia-safe does only unsafely.

use std::{
    hash::{Hash, Hasher},
    sync::{
        Arc, LazyLock, Weak,
        atomic::{AtomicU64, Ordering},
    },
};

#[cfg(test)]
use std::cell::RefCell;

use parking_lot::Mutex;
use skia_safe::{Canvas as SkCanvas, IRect};

use crate::{
    cache_map::CacheMap,
    context::{
        chain::{Outcome, is_on_gpu},
        layer::Group,
        page::PAGE_CACHE_BYTES,
    },
};

/// How many bytes of chain results are kept, across every group and every
/// thread: as many as the page cache keeps, since a result is at most a
/// page and a page drawn into itself repeatedly needs one per round it
/// replays. A 900-pixel square at 8 bits is 3.2 MB.
const CHAIN_CACHE_BYTES: usize = PAGE_CACHE_BYTES;

/// The largest single result that is kept, so that one page-sized F32
/// result cannot push out every smaller one: a 1920 by 1080 page is 8.3 MB
/// at 8 bits and 33.2 MB in F32, and both are kept; a 2560 by 1440 page in
/// F32 is 59 MB and is not.
const CHAIN_MAX_BYTES: usize = CHAIN_CACHE_BYTES / 2;

/// What a fill of the cache is cut back to, as a share of
/// [`CHAIN_CACHE_BYTES`], as the mask cache's low-water mark does.
const CHAIN_CACHE_LOW_WATER: usize = CHAIN_CACHE_BYTES * 3 / 4;

/// What decides a group's chain result. The group is held weakly: it keeps
/// the address the key compares by from being reused while the key lives,
/// and does not keep the group alive.
#[derive(Clone, Debug)]
pub(crate) struct Key {
    group: Weak<Group>,
    matrix: [u32; 9],
    clip: [i32; 4],
    color_type: i32,
    space: Option<(u32, u32)>,
}

impl PartialEq for Key {
    fn eq(&self, other: &Self) -> bool {
        Weak::ptr_eq(&self.group, &other.group)
            && self.matrix == other.matrix
            && self.clip == other.clip
            && self.color_type == other.color_type
            && self.space == other.space
    }
}

impl Eq for Key {}

impl Hash for Key {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.group.as_ptr().hash(state);
        self.matrix.hash(state);
        self.clip.hash(state);
        self.color_type.hash(state);
        self.space.hash(state);
    }
}

impl Key {
    /// The key `group`'s chain has played onto `canvas` as it stands, or
    /// `None` where it is not kept: on the GPU, or where nothing would show.
    pub(crate) fn of(group: &Arc<Group>, canvas: &SkCanvas) -> Option<Self> {
        if is_on_gpu(canvas) {
            return None;
        }
        let clip: IRect = canvas.device_clip_bounds()?;
        let matrix = canvas.local_to_device_as_3x3();
        let info = canvas.image_info();
        Some(Self {
            group: Arc::downgrade(group),
            matrix: std::array::from_fn(|i| matrix[i].to_bits()),
            clip: [clip.left, clip.top, clip.right, clip.bottom],
            color_type: info.color_type() as i32,
            space: info
                .color_space()
                .map(|s| (s.to_xyzd50_hash().0, s.transfer_fn_hash())),
        })
    }

    /// Whether the group the key files under is gone.
    fn is_orphaned(&self) -> bool {
        self.group.strong_count() == 0
    }
}

struct Entry {
    outcome: Outcome,
    bytes: usize,
    used: u64,
}

#[derive(Default)]
pub(crate) struct ChainCache {
    entries: CacheMap<Key, Entry>,
    bytes: usize,
}

/// Every result kept, for every thread: pages are played back on rayon
/// workers.
static CHAINS: LazyLock<Mutex<ChainCache>> = LazyLock::new(Mutex::default);

#[cfg(test)]
thread_local! {
    /// A cache of this thread's own, which [`get`] and [`put`] use in place
    /// of [`CHAINS`] while an [`OwnChainCache`] lives.
    static OWN: RefCell<Option<ChainCache>> = const { RefCell::new(None) };
}

/// A cache of the calling thread's own for as long as the guard lives: for
/// a test that counts the chains it runs. In the shared cache, what tests
/// drawing in parallel file evicts what the count relies on, and the
/// memory watcher's idle release empties it; either makes the count's
/// chains run again. With the shared cache filled past its bytes from
/// another thread, a page drawn into itself eight times ran 63 chains
/// where 16 run.
#[cfg(test)]
pub(crate) struct OwnChainCache;

#[cfg(test)]
impl OwnChainCache {
    pub(crate) fn new() -> Self {
        OWN.with_borrow_mut(|own| *own = Some(ChainCache::default()));
        Self
    }
}

#[cfg(test)]
impl Drop for OwnChainCache {
    fn drop(&mut self) {
        OWN.with_borrow_mut(|own| *own = None);
    }
}

/// Ticks once per use, to order entries for eviction.
static CHAIN_USES: AtomicU64 = AtomicU64::new(0);

/// The result filed under `key`, if one is.
pub(crate) fn get(key: &Key) -> Option<Outcome> {
    #[cfg(test)]
    if let Some(found) =
        OWN.with_borrow_mut(|own| own.as_mut().map(|cache| cache.get(key)))
    {
        return found;
    }
    CHAINS.lock().get(key)
}

/// Files `outcome` under `key`.
pub(crate) fn put(key: Key, outcome: Outcome) {
    #[cfg(test)]
    let Some((key, outcome)) = OWN.with_borrow_mut(|own| match own {
        Some(cache) => {
            cache.put(key, outcome);
            None
        }
        None => Some((key, outcome)),
    }) else {
        return;
    };
    CHAINS.lock().put(key, outcome);
}

/// Drops every result. Called with the page cache's release, when rendering
/// has been idle.
pub(crate) fn release_cached_chains() {
    let mut cache = CHAINS.lock();
    cache.entries.clear();
    cache.bytes = 0;
}

impl ChainCache {
    fn get(&mut self, key: &Key) -> Option<Outcome> {
        let entry = self.entries.get_mut(key)?;
        entry.used = CHAIN_USES.fetch_add(1, Ordering::Relaxed);
        Some(entry.outcome.clone())
    }

    fn put(&mut self, key: Key, outcome: Outcome) {
        // A group that has gone takes its results with it, here rather than
        // in its `Drop`, which would have to reach this lock from wherever
        // the last reference falls.
        self.entries.retain(|key, _| !key.is_orphaned());
        self.bytes = self.entries.values().map(|entry| entry.bytes).sum();
        let bytes = outcome.bytes();
        if bytes > CHAIN_MAX_BYTES {
            return;
        }
        let used = CHAIN_USES.fetch_add(1, Ordering::Relaxed);
        self.bytes += bytes;
        if let Some(old) = self.entries.insert(
            key,
            Entry {
                outcome,
                bytes,
                used,
            },
        ) {
            self.bytes -= old.bytes;
        }
        if self.bytes > CHAIN_CACHE_BYTES {
            let mut order: Vec<(u64, Key, usize)> = self
                .entries
                .iter()
                .map(|(key, entry)| (entry.used, key.clone(), entry.bytes))
                .collect();
            order.sort_unstable_by_key(|(used, _, _)| *used);
            for (_, key, bytes) in order {
                if self.bytes <= CHAIN_CACHE_LOW_WATER {
                    break;
                }
                self.entries.remove(&key);
                self.bytes -= bytes;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use skia_safe::{
        AlphaType, ClipOp, ColorSpace, ColorType, ImageInfo, Matrix, Rect,
    };

    use super::*;
    use crate::{
        Canvas,
        context::{
            chain::RUNS,
            layer::{Composite, LayerRec},
        },
        path::FillRule,
        started,
    };

    /// A group with nothing in it: the cache files by its identity alone.
    fn group() -> Arc<Group> {
        Arc::new(Group {
            composite: Composite::Layer(LayerRec::default()),
            matrix: Matrix::new_identity(),
            clip: None,
            within: None,
            contents_matrix: Matrix::new_identity(),
            contents: Arc::new(Vec::new()),
        })
    }

    /// The key `group` files under on a raster surface of `info`, clipped to
    /// `clip` in its pixels and then put under `matrix`, so that the two
    /// vary apart.
    fn key(
        group: &Arc<Group>,
        info: &ImageInfo,
        matrix: &Matrix,
        clip: Rect,
    ) -> Key {
        let mut surface = started::raster(info, None, None).expect("a surface");
        let canvas = surface.canvas();
        canvas.clip_rect(clip, ClipOp::Intersect, false);
        canvas.set_matrix(&(*matrix).into());
        Key::of(group, canvas).expect("a key")
    }

    fn n32() -> ImageInfo {
        ImageInfo::new_n32_premul((64, 64), ColorSpace::new_srgb())
    }

    /// Where each key's surface is clipped, but for the key that differs
    /// by its clip.
    fn clip() -> Rect {
        Rect::from_xywh(4.0, 4.0, 40.0, 40.0)
    }

    #[test]
    fn a_result_is_found_under_its_own_key_and_no_other() {
        let (one, other) = (group(), group());
        let identity = Matrix::new_identity();
        let mut cache = ChainCache::default();
        cache.put(key(&one, &n32(), &identity, clip()), Outcome::blank(8));
        assert!(cache.get(&key(&one, &n32(), &identity, clip())).is_some());
        let misses = [
            ("another group", key(&other, &n32(), &identity, clip())),
            (
                "another transform",
                key(&one, &n32(), &Matrix::translate((1.0, 0.0)), clip()),
            ),
            (
                "another clip",
                key(&one, &n32(), &identity, clip().with_offset((0.0, 1.0))),
            ),
            (
                "another colour type",
                key(
                    &one,
                    &ImageInfo::new(
                        (64, 64),
                        ColorType::RGBAF32,
                        AlphaType::Premul,
                        ColorSpace::new_srgb(),
                    ),
                    &identity,
                    clip(),
                ),
            ),
            (
                "another space",
                key(
                    &one,
                    &n32().with_color_space(ColorSpace::new_srgb_linear()),
                    &identity,
                    clip(),
                ),
            ),
        ];
        for (what, key) in misses {
            assert!(cache.get(&key).is_none(), "{what} found the result");
        }
    }

    #[test]
    fn a_result_goes_with_its_group() {
        let (gone, kept) = (group(), group());
        let identity = Matrix::new_identity();
        let mut cache = ChainCache::default();
        cache.put(key(&gone, &n32(), &identity, clip()), Outcome::blank(8));
        drop(gone);
        cache.put(key(&kept, &n32(), &identity, clip()), Outcome::blank(8));
        assert_eq!(cache.entries.len(), 1);
        assert_eq!(cache.bytes, Outcome::blank(8).bytes());
    }

    #[test]
    fn the_chain_cache_keeps_to_its_bytes_and_keeps_the_latest() {
        // A side whose N32 square is a sixteenth of the budget and a little.
        let side = ((CHAIN_CACHE_BYTES / 16 / 4) as f64).sqrt() as i32 + 1;
        let groups: Vec<_> = (0..40).map(|_| group()).collect();
        let identity = Matrix::new_identity();
        let mut cache = ChainCache::default();
        for one in &groups {
            let key = key(one, &n32(), &identity, clip());
            cache.put(key.clone(), Outcome::blank(side));
            assert!(cache.bytes <= CHAIN_CACHE_BYTES, "{}", cache.bytes);
            assert!(cache.get(&key).is_some(), "the latest is kept");
        }
        let first = key(&groups[0], &n32(), &identity, clip());
        assert!(cache.get(&first).is_none(), "the first is let go");
    }

    #[test]
    fn the_chain_cache_does_not_keep_one_result_past_its_cap() {
        let side = ((CHAIN_MAX_BYTES / 4) as f64).sqrt() as i32 + 1;
        let one = group();
        let mut cache = ChainCache::default();
        let key = key(&one, &n32(), &Matrix::new_identity(), clip());
        cache.put(key.clone(), Outcome::blank(side));
        assert!(cache.get(&key).is_none());
        assert_eq!(cache.bytes, 0);
    }

    #[test]
    fn the_idle_release_empties_the_chain_cache() {
        // The shared cache, which other tests fill as they draw; only that a
        // result this test filed is gone afterwards is its to assert.
        let one = group();
        let key = key(&one, &n32(), &Matrix::new_identity(), clip());
        put(key.clone(), Outcome::blank(8));
        assert!(get(&key).is_some());
        release_cached_chains();
        assert!(get(&key).is_none());
    }

    /// How many chains drawing a 300-pixel page into itself `rounds` times
    /// through a blur runs, each round under a clip of its own, as
    /// `tests/suite/canvas.test.js` times it.
    fn runs_for(rounds: usize) -> usize {
        let mut page = Canvas::new(300.0, 300.0);
        page.set_gpu(false);
        page.context().scale(2.0, 2.0);
        page.context().set_fill_style_css("#742").expect("a colour");
        page.context().fill_rect(0.0, 0.0, 150.0, 150.0);
        let before = RUNS.with(|runs| runs.get());
        for round in 0..rounds {
            let mut copy = Canvas::new(300.0, 300.0);
            copy.set_gpu(false);
            copy.context().draw_canvas(&mut page, 0.0, 0.0);
            let ctx = page.context();
            ctx.save();
            ctx.begin_path();
            ctx.rect(20.0, round as f32 * 12.0, 200.0, 10.0);
            ctx.clip(FillRule::NonZero);
            ctx.reset_transform();
            ctx.set_filter_css("blur(10px)").expect("a filter");
            ctx.draw_canvas(&mut copy, 0.0, 0.0);
            ctx.restore();
        }
        page.context()
            .get_image_data(0.0, 0.0, 300.0, 300.0)
            .expect("image data");
        RUNS.with(|runs| runs.get()) - before
    }

    /// Runs `count` while another thread files results into the shared
    /// cache past its bytes, over and over, as tests drawing in parallel
    /// do: each of a quarter of the budget, under a group of its own kept
    /// alive, so that every few of them evict the least recently used.
    fn while_the_shared_cache_fills<T>(count: impl FnOnce() -> T) -> T {
        let side = ((CHAIN_CACHE_BYTES / 4 / 4) as f64).sqrt() as i32;
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let filling = Arc::clone(&stop);
        let filler = std::thread::spawn(move || {
            let mut kept = vec![];
            while !filling.load(Ordering::Relaxed) {
                let one = group();
                put(
                    key(&one, &n32(), &Matrix::new_identity(), clip()),
                    Outcome::blank(side),
                );
                kept.push(one);
            }
        });
        let counted = count();
        stop.store(true, Ordering::Relaxed);
        filler.join().expect("the filling thread");
        counted
    }

    #[test]
    fn a_threads_own_chain_cache_keeps_its_results_to_itself() {
        let identity = Matrix::new_identity();
        let (mine, shared) = (group(), group());
        let own = OwnChainCache::new();
        put(key(&mine, &n32(), &identity, clip()), Outcome::blank(8));
        assert!(get(&key(&mine, &n32(), &identity, clip())).is_some());
        assert!(
            CHAINS
                .lock()
                .get(&key(&mine, &n32(), &identity, clip()))
                .is_none(),
            "filed in the shared cache"
        );
        drop(own);
        assert!(
            get(&key(&mine, &n32(), &identity, clip())).is_none(),
            "kept past its guard"
        );
        put(key(&shared, &n32(), &identity, clip()), Outcome::blank(8));
        assert!(
            CHAINS
                .lock()
                .get(&key(&shared, &n32(), &identity, clip()))
                .is_some(),
            "not filed in the shared cache once the guard is gone"
        );
    }

    #[test]
    fn a_canvas_drawn_into_itself_runs_each_rounds_chain_once() {
        // Each round's group holds every earlier round's, so a replay that
        // ran their chains again would run about rounds squared over two.
        let own = OwnChainCache::new();
        for rounds in [8, 16] {
            let runs = while_the_shared_cache_fills(|| runs_for(rounds));
            assert!(runs <= 2 * rounds, "{rounds} rounds ran {runs} chains");
        }
        drop(own);
    }

    #[test]
    fn the_idle_release_leaves_a_test_its_own_chain_cache() {
        // What the memory watcher's idle release does from its own thread,
        // over and over, while a test with a cache of its own counts its
        // chains: in the shared cache, a page drawn into itself sixteen
        // times ran 245.
        let own = OwnChainCache::new();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let releasing = Arc::clone(&stop);
        let releaser = std::thread::spawn(move || {
            while !releasing.load(Ordering::Relaxed) {
                release_cached_chains();
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        });
        let runs = runs_for(16);
        stop.store(true, Ordering::Relaxed);
        releaser.join().expect("the releasing thread");
        drop(own);
        assert!(runs <= 32, "16 rounds ran {runs} chains");
    }
}
