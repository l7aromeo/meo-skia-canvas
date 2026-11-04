//! GPU chain layers kept for the next run rather than made again.
//!
//! A chain on the GPU draws into a layer the page's size -- see
//! [`placed`](super::placed) -- and Skia makes it unbudgeted
//! (`Device::makeSurface`), so once let go it is kept only if it fits the
//! resource cache's budget, 256 MB unless set. A 6000 by 4000 layer at four
//! samples is 480 MB, and the pages' own surfaces, which are budgeted, hold
//! the cache past its budget at that size anyway: every layer was freed as
//! soon as its run ended and made again for the next. On Apple M4 Pro,
//! Metal, a 6000 by 4000 page of 40 blurred shapes made 14 textures a page
//! that way, and 2 with its layers kept.
//!
//! A layer is kept only for the canvas a playback is under way on, named by
//! the [`LayerScope`] the code that made that canvas's surface opens, with
//! the `msaa` it made it with. A layer takes its canvas's sample count, which
//! the canvas does not tell, and one of another count draws edges another
//! way; two surfaces made on one context with the same `msaa` have the same
//! count, so the scope is what makes a kept layer one the canvas would have
//! made. A layer for any other canvas is made as before and let go after.
//!
//! At most [`KEPT`] are kept on a thread, and [`release`] lets them go on
//! the thread they belong to once rendering stops:
//!
//! - on a JavaScript thread, the main one included, by the post
//!   `crate::memory`'s watcher makes when rendering has stopped (see
//!   `node::idle`), which runs between two tasks;
//! - on a `rayon` worker or an export owner, by the GPU's idle release, before
//!   the context goes.
//!
//! A thread neither reaches -- one of a crate user's own -- lets a layer go
//! at its next GPU use once the layer has gone unused for [`KEPT_FOR`], and
//! until then holds at most [`KEPT`] layers of the largest page it drew: at
//! 6000 by 4000 and four samples, 960 MB at eight bits and 1.9 GB in `F16`.

use std::{
    cell::RefCell,
    marker::PhantomData,
    time::{Duration, Instant},
};

#[cfg(any(feature = "metal", feature = "vulkan"))]
use skia_safe::gpu::DirectContextId;
use skia_safe::{Canvas as SkCanvas, ImageInfo, Surface, SurfaceProps};

use crate::context::layer::MAX_RUN_LAYERS;

/// How many layers a thread keeps: as many as a run holds open at once, so
/// that a run of chains like the last finds every layer it needs.
const KEPT: usize = MAX_RUN_LAYERS;

/// How long a kept layer outlives its last use: the span a thread's GPU
/// context is kept unused for (`MTL_CONTEXT_LIFESPAN`,
/// `VK_CONTEXT_LIFESPAN`), so that a layer is held no longer than the
/// context it belongs to would be.
const KEPT_FOR: Duration = Duration::from_secs(5);

/// What a layer is kept under: everything that decides how a draw lands on
/// it, besides where it is drawn.
#[derive(Clone, PartialEq)]
#[cfg_attr(
    not(any(feature = "metal", feature = "vulkan")),
    expect(dead_code, reason = "only a GPU layer is kept")
)]
pub(crate) struct Key {
    #[cfg(any(feature = "metal", feature = "vulkan"))]
    context: DirectContextId,
    /// The `msaa` the scope's canvas was made with, which with the context
    /// decides the sample count.
    msaa: Option<usize>,
    info: ImageInfo,
    props: SurfaceProps,
}

/// A thread's open scopes and the layers it keeps.
#[derive(Default)]
struct Pool {
    /// Each open scope's canvas, by address, and its `msaa`.
    scopes: Vec<(usize, Option<usize>)>,
    /// The layers kept, oldest first, with when each was let go.
    kept: Vec<(Key, Surface, Instant)>,
}

thread_local! {
    static POOL: RefCell<Pool> = RefCell::default();
}

/// Names the canvas a playback is under way on, and the `msaa` its surface
/// was made with, for as long as it is held, so that chain layers for that
/// canvas are kept -- see the module's documentation. Opened by the code that
/// made the surface; scopes nest.
pub(crate) struct LayerScope(PhantomData<*const ()>);

impl LayerScope {
    /// Opens a scope for `canvas`, whose surface was made with `msaa`.
    pub(crate) fn open(canvas: &SkCanvas, msaa: Option<usize>) -> Self {
        let at = address(canvas);
        POOL.with_borrow_mut(|pool| pool.scopes.push((at, msaa)));
        Self(PhantomData)
    }
}

impl Drop for LayerScope {
    fn drop(&mut self) {
        POOL.with_borrow_mut(|pool| {
            pool.scopes.pop();
        });
    }
}

fn address(canvas: &SkCanvas) -> usize {
    std::ptr::from_ref(canvas).addr()
}

/// The key a layer of `info` for `canvas` is kept under, or `None` where it
/// is not kept: off a GPU context, or not the canvas of an open scope.
pub(crate) fn key(canvas: &SkCanvas, info: &ImageInfo) -> Option<Key> {
    #[cfg(test)]
    if !KEEP.get() {
        return None;
    }
    // With no GPU backend compiled in every layer is memory, which is never
    // kept.
    #[cfg(not(any(feature = "metal", feature = "vulkan")))]
    {
        let _ = (canvas, info);
        None
    }
    #[cfg(any(feature = "metal", feature = "vulkan"))]
    {
        let at = address(canvas);
        let msaa = POOL.with_borrow(|pool| {
            pool.scopes
                .iter()
                .rev()
                .find(|(scope, _)| *scope == at)
                .map(|(_, msaa)| *msaa)
        })?;
        Some(Key {
            context: canvas.direct_context()?.id(),
            msaa,
            info: info.clone(),
            props: canvas.props()?,
        })
    }
}

/// A layer kept under `key`, taken out of the pool, if one is. Layers kept
/// past [`KEPT_FOR`] are let go first.
pub(crate) fn take(key: &Key) -> Option<Surface> {
    POOL.with_borrow_mut(|pool| {
        pool.expire();
        let at = pool.kept.iter().position(|(kept, ..)| kept == key)?;
        Some(pool.kept.remove(at).1)
    })
}

/// Keeps `surface` under `key`, letting the oldest go past [`KEPT`].
///
/// `surface` is a GPU target: [`key`] names none but a GPU canvas's layer.
/// One in main memory would break [`Drawn::new`](super::Drawn::new), which
/// takes such a layer as made just before, and so zeros, and skips clearing
/// it.
pub(crate) fn keep(key: Key, mut surface: Surface) {
    debug_assert!(
        surface.peek_pixels().is_none(),
        "a layer in main memory kept"
    );
    POOL.with_borrow_mut(|pool| {
        pool.expire();
        pool.kept.push((key, surface, Instant::now()));
        if pool.kept.len() > KEPT {
            pool.kept.remove(0);
        }
    });
}

/// Lets go every layer this thread keeps. Called by the idle release, before
/// the thread's context goes.
pub(crate) fn release() {
    POOL.with_borrow_mut(|pool| pool.kept.clear());
}

impl Pool {
    fn expire(&mut self) {
        self.kept.retain(|(.., since)| since.elapsed() <= KEPT_FOR);
    }
}

/// How many layers this thread keeps, for the debug addon's `__keptLayers`.
#[cfg(debug_assertions)]
pub(crate) fn kept() -> usize {
    POOL.with_borrow(|pool| pool.kept.len())
}

#[cfg(test)]
thread_local! {
    /// Whether layers are kept on this thread, so that a test can draw the
    /// same page with every layer made afresh.
    pub(crate) static KEEP: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
}

#[cfg(test)]
mod tests {
    use super::KEEP;
    use crate::{context::chain::MADE_LAYERS, prelude::*};

    /// Whether this machine has a GPU engine to keep layers on.
    fn on_gpu() -> bool {
        let mut probe = Canvas::new(1.0, 1.0);
        probe.set_gpu(true);
        let gpu = probe.engine_kind() == EngineKind::Gpu;
        if !gpu {
            eprintln!("skipped: no GPU engine");
        }
        gpu
    }

    /// A GPU page of `size` with `shapes` drawn on it under `blur(4px)`, each
    /// a rounded rectangle at `(x, y, w, h)` in `colour`, read whole at the
    /// default sample count once the draws are made, with layers kept or not;
    /// and how many layer surfaces the read made.
    fn read(
        size: (f32, f32),
        shapes: &[(f32, f32, f32, f32, &str)],
        keep: bool,
    ) -> (Vec<u8>, usize) {
        read_at(size, shapes, keep, None)
    }

    /// [`read`] at `msaa` samples.
    fn read_at(
        size: (f32, f32),
        shapes: &[(f32, f32, f32, f32, &str)],
        keep: bool,
        msaa: Option<usize>,
    ) -> (Vec<u8>, usize) {
        let mut canvas = Canvas::new(size.0, size.1);
        canvas.set_gpu(true);
        let ctx = canvas.context();
        ctx.set_filter_css("blur(4px)").expect("a filter");
        for &(x, y, w, h, colour) in shapes {
            ctx.set_fill_style_css(colour).expect("a colour");
            ctx.begin_path();
            ctx.round_rect(x, y, w, h, [12.0; 4])
                .expect("a rounded rectangle");
            ctx.fill(FillRule::NonZero);
        }
        KEEP.set(keep);
        let made = MADE_LAYERS.get();
        let data = ctx
            .get_image_data_with(
                0.0,
                0.0,
                size.0,
                size.1,
                &ImageDataExport {
                    msaa,
                    ..ImageDataExport::default()
                },
            )
            .expect("image data")
            .pixels()
            .to_vec();
        let made = MADE_LAYERS.get() - made;
        KEEP.set(true);
        (data, made)
    }

    /// A layer kept from a page whose chain filled most of it with an
    /// opaque colour, taken by the next page of that size for a small shape
    /// in the middle of what was filled, reads as a layer made afresh does:
    /// what it held is cleared where the next chain reads it.
    #[test]
    fn a_kept_layer_that_held_opaque_content_reads_as_a_fresh_one() {
        if !on_gpu() {
            return;
        }
        const SIZE: (f32, f32) = (600.0, 400.0);
        // Opaque, as the filling page is: a translucent colour takes a layer
        // of another depth, which the kept one would not serve.
        let small = [(260.3, 170.6, 80.0, 40.0, "rgb(20 60 200)")];
        let (fresh, _) = read(SIZE, &small, false);
        // Fills the layer it leaves kept: opaque red over the small shape's
        // region and well past it.
        let (_, made) =
            read(SIZE, &[(40.0, 40.0, 520.0, 320.0, "rgb(255 0 0)")], true);
        assert_eq!(made, 1, "the filling page made no layer to keep");
        let (kept, made) = read(SIZE, &small, true);
        assert_eq!(made, 0, "the small shape did not take the kept layer");
        let differ = fresh.iter().zip(&kept).filter(|(a, b)| a != b).count();
        assert_eq!(differ, 0, "a kept layer read what it held before");
    }

    /// A layer kept from a page at the default sample count is not taken by a
    /// page read at one sample, whose own layer takes that page's count: a
    /// layer of another count draws edges another way.
    #[test]
    fn a_kept_layer_serves_no_page_of_another_sample_count() {
        if !on_gpu() {
            return;
        }
        const SIZE: (f32, f32) = (600.0, 400.0);
        let shape = [(260.3, 170.6, 80.0, 40.0, "rgb(20 60 200)")];
        let (fresh, _) = read_at(SIZE, &shape, false, Some(1));
        let (_, made) = read(SIZE, &shape, true);
        assert_eq!(made, 1, "the default page made no layer to keep");
        let (single, made) = read_at(SIZE, &shape, true, Some(1));
        assert_eq!(made, 1, "a one-sample page took a four-sample layer");
        let differ = fresh.iter().zip(&single).filter(|(a, b)| a != b).count();
        assert_eq!(differ, 0, "the one-sample page changed with layers kept");
    }

    /// #499's 40 shapes along a line, on two pages one after the other, take
    /// the two layers the first page's run makes, and the second page makes
    /// none; each page reads the bytes it does with every layer made afresh.
    #[test]
    fn forty_blurred_draws_on_two_pages_make_two_layers() {
        if !on_gpu() {
            return;
        }
        const SIZE: (f32, f32) = (4000.0, 2600.0);
        let shapes: Vec<_> = (0..40)
            .map(|i| {
                (
                    40.0 + (i * 97) as f32 + 0.37,
                    40.0 + (i * 61) as f32 + 0.25,
                    80.0,
                    40.0,
                    ["rgb(200 40 20 / 0.6)", "rgb(20 60 200 / 0.6)"][i % 2],
                )
            })
            .collect();
        let (fresh, made_fresh) = read(SIZE, &shapes, false);
        let (_, again) = read(SIZE, &shapes, false);
        // On Metal the run holds two layers open; elsewhere each chain takes
        // one of its own -- see `chain::shares_layers` -- and lets it go
        // before the next is made, so one serves every chain once kept.
        let (layers, kept) = match cfg!(feature = "metal") {
            true => (2, 2),
            false => (40, 1),
        };
        assert_eq!(made_fresh, layers, "not a run's layers made afresh");
        assert_eq!(again, layers, "a page without kept layers made fewer");
        // Whatever an earlier test on this thread left kept goes first.
        super::release();
        let (first, made_first) = read(SIZE, &shapes, true);
        let (second, made_second) = read(SIZE, &shapes, true);
        assert_eq!(
            made_first + made_second,
            kept,
            "two pages made {made_first} and {made_second} layers"
        );
        for (name, page) in [("first", &first), ("second", &second)] {
            let differ = fresh.iter().zip(page).filter(|(a, b)| a != b).count();
            assert_eq!(differ, 0, "the {name} page changed with layers kept");
        }
    }
}
