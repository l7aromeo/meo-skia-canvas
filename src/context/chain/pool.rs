//! GPU chain layers kept for the next run rather than made again, and CPU
//! coverage planes kept for the rest of a playback.
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
//!
//! The coverage route's planes on the CPU -- see
//! [`coverage`](super::coverage) -- are kept too, but only while a
//! [`PlaneScope`] is open. A plane runs from the device's origin to the far
//! corner of what its draw reads, nearly the page. Made afresh for every
//! blurred draw, it is a large allocation each time, zeroed -- which macOS's
//! allocator does with `madvise` for a block of that size -- and let go
//! after: making and dropping one a draw takes 35% of the samples of playing
//! 50 runs of 40px "Ag" under `blur(3px)` that find their results kept. One
//! plane of each kind, as large as the page, is kept for the rest of the
//! outermost open plane scope and let go when it closes, so a thread holds
//! none between playbacks; a draw that takes it clears what it reads, then
//! cuts what it draws where its own plane would have ended.

use std::{
    cell::RefCell,
    marker::PhantomData,
    time::{Duration, Instant},
};

#[cfg(any(feature = "metal", feature = "vulkan"))]
use skia_safe::gpu::DirectContextId;
use skia_safe::{
    Canvas as SkCanvas, ColorType, ISize, ImageInfo, Surface, SurfaceProps,
};

use crate::context::{chain::coverage::Plane, layer::MAX_RUN_LAYERS};

/// How many layers a thread keeps: as many as a run holds open at once, so
/// that a run of chains like the last finds every layer it needs.
const KEPT: usize = MAX_RUN_LAYERS;

/// How long a kept layer outlives its last use: the span a thread's GPU
/// context is kept unused for (`MTL_CONTEXT_LIFESPAN`,
/// `VK_CONTEXT_LIFESPAN`), so that a layer is held no longer than the
/// context it belongs to would be.
///
/// The span starts when a run lets the layer go, before the page's read
/// waits on the GPU, so where drawing a page takes longer than this the
/// next page finds nothing kept: on llvmpipe a 4000 by 2600 page of 40
/// blurred shapes took over 15 s beside other tests.
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
    /// How many [`PlaneScope`]s are open.
    plane_scopes: usize,
    /// The coverage planes kept for the open plane scopes, one of each kind.
    planes: Vec<(Plane, Surface)>,
}

thread_local! {
    static POOL: RefCell<Pool> = RefCell::default();
}

/// Names the canvas a playback is under way on, and the `msaa` its surface
/// was made with, for as long as it is held, so that chain layers for that
/// canvas are kept -- see the module's documentation. Opened by the code that
/// made the surface; scopes nest.
///
/// A layer scope is a [`PlaneScope`] too.
pub(crate) struct LayerScope(PhantomData<*const ()>, PlaneScope);

impl LayerScope {
    /// Opens a scope for `canvas`, whose surface was made with `msaa`.
    pub(crate) fn open(canvas: &SkCanvas, msaa: Option<usize>) -> Self {
        let at = address(canvas);
        POOL.with_borrow_mut(|pool| pool.scopes.push((at, msaa)));
        Self(PhantomData, PlaneScope::open())
    }
}

impl Drop for LayerScope {
    fn drop(&mut self) {
        POOL.with_borrow_mut(|pool| {
            pool.scopes.pop();
        });
    }
}

/// A playback onto pixels on this thread, for as long as it is held: the
/// coverage planes its draws make are kept until the outermost scope closes
/// -- see the module's documentation. Opened where a page is played onto
/// pixels; scopes nest.
pub(crate) struct PlaneScope(PhantomData<*const ()>);

impl PlaneScope {
    /// Opens a scope on this thread.
    pub(crate) fn open() -> Self {
        POOL.with_borrow_mut(|pool| pool.plane_scopes += 1);
        Self(PhantomData)
    }
}

impl Drop for PlaneScope {
    fn drop(&mut self) {
        let planes = POOL.with_borrow_mut(|pool| {
            pool.plane_scopes -= 1;
            match pool.plane_scopes {
                0 => std::mem::take(&mut pool.planes),
                _ => Vec::new(),
            }
        });
        // Let go outside the borrow, which a surface's release does not need.
        drop(planes);
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
    POOL.with_borrow_mut(|pool| {
        pool.kept.clear();
        pool.planes.clear();
    });
}

/// A coverage plane of kind `plane` this thread keeps that a draw needing
/// `size` of `info`'s colour type, alpha type and space, made with `props`,
/// can take, taken out of the pool: one in main memory, made alike and at
/// least as large. `None` where none is kept.
///
/// What it held is still in it: the caller clears what it reads.
pub(crate) fn take_plane(
    plane: Plane,
    info: &ImageInfo,
    props: &SurfaceProps,
    size: ISize,
) -> Option<Surface> {
    if !keeps_planes() {
        return None;
    }
    POOL.with_borrow_mut(|pool| {
        let at = pool.planes.iter_mut().position(|(kind, surface)| {
            let own = surface.image_info();
            *kind == plane
                && own.color_type() == info.color_type()
                && own.alpha_type() == info.alpha_type()
                && own.color_space() == info.color_space()
                && surface.props() == props
                && own.width() >= size.width
                && own.height() >= size.height
        })?;
        #[cfg(test)]
        PLANES_TAKEN.set(PLANES_TAKEN.get() + 1);
        Some(pool.planes.swap_remove(at).1)
    })
}

/// Keeps `surface`, a coverage plane of kind `plane` in main memory, for the
/// rest of the outermost open [`PlaneScope`], in place of any of its kind
/// kept before. Let go at once where no plane scope is open.
pub(crate) fn keep_plane(plane: Plane, mut surface: Surface) {
    debug_assert!(
        surface.image_info().color_type() != ColorType::Unknown
            && surface.peek_pixels().is_some(),
        "a coverage plane not in main memory"
    );
    if !keeps_planes() {
        return;
    }
    let before = POOL.with_borrow_mut(|pool| {
        if pool.plane_scopes == 0 {
            return Some(surface);
        }
        let before = pool
            .planes
            .iter()
            .position(|(kind, _)| *kind == plane)
            .map(|at| pool.planes.swap_remove(at).1);
        pool.planes.push((plane, surface));
        before
    });
    drop(before);
}

/// Whether this thread keeps coverage planes: [`KEEP_PLANES`] in a test,
/// always otherwise.
#[cfg(test)]
pub(crate) fn keeps_planes() -> bool {
    KEEP_PLANES.get()
}

/// Whether this thread keeps coverage planes: always outside a test.
#[cfg(not(test))]
pub(crate) fn keeps_planes() -> bool {
    true
}

impl Pool {
    fn expire(&mut self) {
        let kept_for = kept_for();
        self.kept.retain(|(.., since)| since.elapsed() <= kept_for);
    }
}

/// How long a layer is kept on this thread: [`KEPT_FOR`].
#[cfg(not(test))]
fn kept_for() -> Duration {
    KEPT_FOR
}

/// How long a layer is kept on this thread: what a test set in
/// [`KEPT_FOR_HERE`].
#[cfg(test)]
fn kept_for() -> Duration {
    KEPT_FOR_HERE.get()
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

    /// How long a layer is kept on this thread, [`KEPT_FOR`] unless a test
    /// sets it, so that a test can count what a page takes on a device that
    /// draws it more slowly than that.
    pub(crate) static KEPT_FOR_HERE: std::cell::Cell<Duration> =
        const { std::cell::Cell::new(KEPT_FOR) };

    /// Whether coverage planes are kept on this thread, so that a test can
    /// draw the same page with every plane made afresh.
    pub(crate) static KEEP_PLANES: std::cell::Cell<bool> =
        const { std::cell::Cell::new(true) };

    /// How many kept coverage planes draws on this thread have taken.
    pub(crate) static PLANES_TAKEN: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use skia_safe::ImageInfo;

    use super::{KEEP, KEPT_FOR, KEPT_FOR_HERE};
    use crate::{
        context::{
            chain::{MADE_LAYERS, shares_layers},
            layer::MAX_RUN_LAYERS,
            page::ExportOptions,
        },
        gpu::RenderingEngine,
        prelude::*,
    };

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
        // Counted however slowly the device draws -- see `KEPT_FOR_HERE`.
        KEPT_FOR_HERE.set(Duration::MAX);
        // Fills the layer it leaves kept: opaque red over the small shape's
        // region and well past it.
        let (_, made) =
            read(SIZE, &[(40.0, 40.0, 520.0, 320.0, "rgb(255 0 0)")], true);
        assert_eq!(made, 1, "the filling page made no layer to keep");
        let (kept, made) = read(SIZE, &small, true);
        KEPT_FOR_HERE.set(KEPT_FOR);
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

    /// A layer kept longer than its span is let go, and the next page of its
    /// size makes its own.
    #[test]
    fn a_layer_kept_past_its_span_is_made_again() {
        if !on_gpu() {
            return;
        }
        const SIZE: (f32, f32) = (600.0, 400.0);
        let shape = [(260.3, 170.6, 80.0, 40.0, "rgb(20 60 200)")];
        super::release();
        let (_, made) = read(SIZE, &shape, true);
        assert_eq!(made, 1, "the first page made no layer to keep");
        KEPT_FOR_HERE.set(Duration::ZERO);
        let (_, made) = read(SIZE, &shape, true);
        KEPT_FOR_HERE.set(KEPT_FOR);
        assert_eq!(made, 1, "a layer kept past its span was taken");
    }

    /// #499's 40 shapes along a line, on two pages one after the other: the
    /// second page takes the layers the first page's first run made, and
    /// makes none; each page reads the bytes it does with every layer made
    /// afresh.
    ///
    /// A run holds at most [`MAX_RUN_LAYERS`] layers open whether or not its
    /// chains share them -- see `layer::Run` -- so the first run makes that
    /// many with layers kept, and every later run takes them. Made afresh,
    /// what a page makes follows from [`shares_layers`]: where chains share,
    /// the shapes, each meeting the next and not the one after, take two
    /// layers between them in one run; where they do not, each takes its
    /// own.
    #[test]
    fn forty_blurred_draws_on_two_pages_make_two_layers() {
        if !on_gpu() {
            return;
        }
        const SIZE: (f32, f32) = (4000.0, 2600.0);
        const SHAPES: usize = 40;
        let shapes: Vec<_> = (0..SHAPES)
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
        let shares = RenderingEngine::GPU
            .make_surface(
                &ImageInfo::new_n32_premul((1, 1), None),
                &ExportOptions::default(),
            )
            .map(|mut surface| shares_layers(surface.canvas()))
            .expect("a GPU surface");
        let fresh_layers = if shares { 2 } else { SHAPES };
        let (fresh, made_fresh) = read(SIZE, &shapes, false);
        let (_, again) = read(SIZE, &shapes, false);
        assert_eq!(made_fresh, fresh_layers, "not a page's layers made afresh");
        assert_eq!(
            again, fresh_layers,
            "a page without kept layers made fewer"
        );
        // Counted however slowly the device draws -- see `KEPT_FOR_HERE`.
        KEPT_FOR_HERE.set(Duration::MAX);
        // Whatever an earlier test on this thread left kept goes first.
        super::release();
        let (first, made_first) = read(SIZE, &shapes, true);
        let (second, made_second) = read(SIZE, &shapes, true);
        KEPT_FOR_HERE.set(KEPT_FOR);
        assert_eq!(
            (made_first, made_second),
            (MAX_RUN_LAYERS, 0),
            "two pages made {made_first} and {made_second} layers"
        );
        for (name, page) in [("first", &first), ("second", &second)] {
            let differ = fresh.iter().zip(page).filter(|(a, b)| a != b).count();
            assert_eq!(differ, 0, "the {name} page changed with layers kept");
        }
    }
}
