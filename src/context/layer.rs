//! One entry of a page's recording: a Skia picture, a blurred draw kept as
//! ours so that its blur is made where the page is played back, or a group
//! composited through a paint, kept as ours so that how it composites is
//! chosen for the target it is played back onto.
//!
//! A picture fixes everything about a draw when it is recorded, and a page is
//! played back under transforms the draw never saw: an export at a density,
//! a tile's origin, a canvas drawn into another at a size. Most of a draw
//! scales with them. How a blur is made does not have to: Skia's coverage
//! blur stops at a sigma of 128 device pixels, so a choice of it made at
//! density 1 can ask it for more than it draws at density 4. A [`BlurOp`]
//! makes its blur when it is played back, against the sigma it has there: a
//! rectangle that stays one on the device as its exact blur -- see
//! [`blur_mask`] -- and anything else with the blur the recording chose,
//! checked against Skia's cap.
//!
//! A [`CutOp`] is a fill or stroke its clip cuts, kept as ours so that a
//! raster playback on the CPU applies the clip as coverage rather than as
//! the bounds Skia rasterizes the path against -- see [`CutOp`].

use std::{
    borrow::Cow,
    cell::{Cell, RefCell},
    f32::consts::SQRT_2,
    fmt,
    iter::zip,
    marker::PhantomData,
    sync::{
        Arc, OnceLock, Weak,
        atomic::{AtomicUsize, Ordering},
    },
};

use parking_lot::Mutex;
use skia_safe::{
    AlphaType, Bitmap, BlendMode, BlurStyle, Canvas as SkCanvas, ClipOp, Color,
    Color4f, ColorFilter, ColorSpace, ColorType, Contains, Data, FilterMode,
    IPoint, IRect, ISize, Image, ImageFilter, ImageInfo, MaskFilter, Matrix,
    MipmapMode, Paint, PaintCap, PaintJoin, PaintStyle, Path, PathDirection,
    Picture, PictureRecorder, Point, Rect, RoundOut, RuntimeEffect,
    SamplingOptions, Shader, Surface, TileMode, Vector,
    canvas::{SaveLayerRec, SrcRectConstraint},
    color_filters, image_filters, shaders,
};

use crate::{
    cache_map::{GOLDEN_RATIO_64, KeptSets},
    context::{
        blur_mask,
        chain::{
            self, Depth, Step,
            pool::{self, LayerScope},
        },
        chain_cache, coverage_of, covers_beyond_the_draw, exact_composite,
        exact_gradient::{Exact, GradientSpec, HIGHP_TIES_TO_EVEN},
        faint::{Faint, composited},
        inked_bounds,
        kept::CoverageInput,
        page::{ExportOptions, Page, own_image_type},
        run_mask::{self, Kept},
    },
    export::{VectorFeatures, blend_features},
    font::colr::{
        BatchScope, Format, ImageRoute, blends_by_coefficients, composite_onto,
        grow_sheets,
    },
    gpu::{RenderingEngine, pacing},
    node::{
        filter::MASK_BLUR_MAX_SIGMA,
        typography::{
            ColourGlyphsIn, DrawnText, HeldGroup, RunPaint, WholeRuns,
            held_groups,
        },
        utils::{color_space_name, skia_paint_color_onto_own},
    },
    pixels::PixelColorSpace,
    started,
};

/// One entry of a page's recording, played back in order.
#[derive(Clone, Debug)]
pub enum Layer {
    /// Draws Skia recorded, played back as recorded.
    Picture(Picture),
    /// A blurred flat-colour draw, or a run of them, its blur made at
    /// playback. Under a clip it is a piece of the frame's run, and holds no
    /// clip of its own -- see [`Clipped`].
    ///
    /// Shared, so that a layer is two words whichever kind it is: a page
    /// snapshot copies a pointer for it rather than the op, and every
    /// snapshot of the page draws from the one blur the op makes.
    Blur(Arc<BlurOp>),
    /// What a `saveLayer` holds, or a canvas drawn into another through a
    /// paint, composited through that paint -- see [`Group`]. Shared, as a
    /// blur is.
    Group(Arc<Group>),
    /// The same draws recorded twice, on a page that keeps a recording for
    /// each kind of target, once a draw has forked them -- see
    /// `page::Recorders`: a flat colour held exactly in the page's own space
    /// for pixels, and as a plain colour for a document. Shared, so the two
    /// pictures are one pointer.
    Targeted(Arc<(Picture, Picture)>),
    /// A fill or stroke its clip cuts, or a run of them under one clip, the
    /// clip applied at playback. Filed as a piece of the frame's run -- see
    /// [`Clipped`] -- wherever the page keeps runs.
    Cut(Arc<CutOp>),
    /// Part of what was drawn under one clip and transform, recorded without
    /// the clip and played under it -- see [`Clipped`].
    Clipped(Arc<Clipped>),
    /// A canvas pattern painted over what its group holds, made from the
    /// canvas's page at the scale it is played back at -- see [`PatternDraw`].
    Pattern(Arc<PatternDraw>),
    /// A run of text recorded the first time a target plays it -- see
    /// [`DeferredPicture`]: as a picture once recorded.
    Deferred(Arc<DeferredPicture>),
    /// A run of text holding colour glyphs, drawn again where it is played
    /// back on pixels -- see [`ReplayedText`].
    ReplayedText(Arc<ReplayedText>),
    /// A `putImageData`, or a `clearRect` short of the whole page, with the
    /// region it erases -- see [`Erase`]. Shared, as a blur is.
    Erase(Arc<Erase>),
    /// A run of draws dyed by gradients, or of canvases drawn as images that
    /// hold such draws, drawn on a GPU canvas in the bytes the CPU draws them
    /// in -- see [`GradientDraw`].
    Gradient(Arc<GradientDraw>),
    /// One draw filed as entries of its own for each kind of target -- see
    /// [`Split`]. Shared, as a group is.
    Split(Arc<Split>),
}

impl Layer {
    /// Draws this entry onto `canvas` for `target`, under whatever transform
    /// and clip the canvas already carries.
    pub fn playback(&self, canvas: &SkCanvas, target: Target) {
        match self {
            Layer::Picture(picture) => picture.playback(canvas),
            Layer::Blur(op) => op.playback(canvas, target),
            Layer::Group(group) => group.playback(canvas, target),
            Layer::Erase(erase) => erase.entry.playback(canvas, target),
            Layer::Targeted(pictures) => match target {
                Target::Pixels => pictures.0.playback(canvas),
                Target::Document => pictures.1.playback(canvas),
            },
            Layer::Cut(op) => op.playback(canvas, target),
            Layer::Clipped(piece) => piece.playback(canvas, target),
            Layer::Pattern(tile) => tile.playback(canvas, target),
            Layer::Deferred(deferred) => {
                if let Some(picture) = deferred.played_for(target) {
                    picture.playback(canvas);
                }
            }
            Layer::ReplayedText(text) => text.playback(canvas, target),
            Layer::Gradient(op) => op.playback(canvas, target),
            Layer::Split(split) => split.playback(canvas, target),
        }
    }

    /// Whether a GPU canvas replaying a recording of this entry draws other
    /// bytes than a CPU canvas does: what holds colour glyphs or draws dyed
    /// by gradients, which choose where they are played how a GPU canvas
    /// draws them in the CPU's bytes -- see [`ReplayedText`] and
    /// [`GradientDraw`] -- and a run painted as one image, which a GPU
    /// canvas composites on the CPU -- see [`KeptRun::Image`] -- and have no
    /// such choice inside a recording, which does not know the canvas it
    /// will be played onto. A canvas whose page holds one is drawn as an
    /// image through that choice too -- see `Context2D::draw_source_image`.
    pub(crate) fn plays_apart_on_gpu(&self) -> bool {
        match self {
            Layer::ReplayedText(_) => true,
            Layer::Group(group)
                if matches!(&group.composite,
                    Composite::Layer(rec) if rec.one_image) =>
            {
                true
            }
            Layer::Gradient(op) => op
                .draws
                .iter()
                .any(|piece| !matches!(piece.gpu, OnGpu::Native)),
            Layer::Group(group) => group
                .contents
                .iter()
                .any(|(entry, _)| entry.plays_apart_on_gpu()),
            Layer::Clipped(piece) => piece.entry().plays_apart_on_gpu(),
            Layer::Erase(erase) => erase.entry.plays_apart_on_gpu(),
            Layer::Split(split) => split.plays_apart_on_gpu(),
            Layer::Picture(_)
            | Layer::Blur(_)
            | Layer::Targeted(_)
            | Layer::Cut(_)
            | Layer::Pattern(_)
            | Layer::Deferred(_) => false,
        }
    }

    /// The rectangle this entry can draw in, in the coordinates it is played
    /// back under: a picture's cull rectangle -- a pair's, the one played as
    /// pixels -- which Skia trims to what it draws where the recording was
    /// begun with a bounding hierarchy, and which is the recording's bounds
    /// where it was not, as for a clip run outside a layer, a group's
    /// [`Group::reach`], and what a cut draw paints before its clip applies
    /// -- see [`CutOp::reach`]. `None` where it is not known: a blurred draw
    /// has no such rectangle to take.
    fn reach(&self) -> Option<Rect> {
        match self {
            Layer::Picture(picture) => Some(picture.cull_rect()),
            Layer::Blur(_) => None,
            Layer::Group(group) => group.reach(),
            // The pixels' recording: the document's holds the same draws,
            // and a document is never confined.
            Layer::Targeted(pictures) => Some(pictures.0.cull_rect()),
            Layer::Cut(op) => Some(op.reach()),
            Layer::Clipped(piece) => piece.reach(),
            Layer::Erase(erase) => erase.entry.reach(),
            Layer::Pattern(tile) => Some(tile.reach()),
            // The pixels' recording, as for a pair, recorded to answer.
            Layer::Deferred(deferred) => Some(
                deferred.picture().map_or_else(Rect::new_empty, |picture| {
                    picture.cull_rect()
                }),
            ),
            Layer::ReplayedText(text) => Some(text.reach()),
            Layer::Gradient(op) => Some(op.reach()),
            // What its pixels reach, as for a pair.
            Layer::Split(split) => split.pixels_reach(),
        }
    }

    /// What the entry reaches as a batch of runs drawn on the CPU asks it
    /// where its [`Self::reach`] meets a run -- see
    /// [`replayed_on_their_backdrop`]: a piece of a clip's run, which reaches
    /// its clip -- see `page::Run::cull` -- by what it draws within the clip,
    /// found once -- see [`Clipped::drawn`]; anything else, and a cut draw,
    /// which reaches what it paints before its clip, by its reach.
    pub(crate) fn found_reach(&self) -> Option<Rect> {
        match self {
            Layer::Clipped(piece)
                if !matches!(piece.entry(), Layer::Cut(_)) =>
            {
                piece.drawn()
            }
            entry => entry.reach(),
        }
    }

    /// What the pictures this entry keeps hold, as Skia estimates each --
    /// `SkPicture::approximateBytesUsed`, which a picture works out once,
    /// when it is finished, so this costs a call for each picture and walks
    /// no draws.
    ///
    /// A picture recorded only when a target first plays it -- a deferred
    /// run, a replayed run's recordings for pixels and for a document --
    /// counts once it has been recorded.
    /// Left out: a blur's geometry and the filter it makes at playback,
    /// which hold no pixels, and a pattern's source, which is another
    /// canvas's page, reported by that canvas.
    pub(crate) fn recorded_bytes(&self) -> usize {
        #[cfg(test)]
        ENTRIES_WALKED.set(ENTRIES_WALKED.get() + 1);
        let recorded = LazyPicture::bytes;
        match self {
            Layer::Picture(picture) => picture.approximate_bytes_used(),
            Layer::Blur(_) => 0,
            Layer::Group(group) => group
                .contents
                .layers
                .iter()
                .map(Layer::recorded_bytes)
                .sum(),
            Layer::Targeted(pictures) => {
                pictures.0.approximate_bytes_used()
                    + pictures.1.approximate_bytes_used()
            }
            Layer::Cut(op) => {
                op.draws
                    .iter()
                    .chain(op.documents.iter().flatten())
                    .map(Picture::approximate_bytes_used)
                    .sum::<usize>()
                    + op.gpu
                        .iter()
                        .flatten()
                        .map(OnGpu::recorded_bytes)
                        .sum::<usize>()
            }
            Layer::Gradient(op) => {
                op.draws.iter().map(GradientPiece::recorded_bytes).sum()
            }
            Layer::Clipped(piece) => piece.entry.recorded_bytes(),
            Layer::Erase(erase) => erase.entry.recorded_bytes(),
            Layer::Pattern(tile) => tile.plain.approximate_bytes_used(),
            Layer::Split(split) => split
                .pixels
                .layers
                .iter()
                .chain(split.document.layers.iter())
                .map(Layer::recorded_bytes)
                .sum(),
            Layer::Deferred(deferred) => {
                recorded(&deferred.pixels.picture)
                    + deferred
                        .document
                        .as_ref()
                        .map_or(0, |document| recorded(&document.picture))
            }
            Layer::ReplayedText(text) => {
                recorded(&text.recording)
                    + recorded(&text.whole)
                    + text
                        .document
                        .as_ref()
                        .map_or(0, |document| recorded(&document.picture))
            }
        }
    }

    /// Has every picture this entry records only when a target first plays
    /// it add its bytes to `count` when it is recorded -- see
    /// [`LazyPicture`] -- so that what a page holds is counted as it changes
    /// rather than by walking every entry at each read.
    pub(crate) fn count_into(&self, count: &RecordedCount) {
        match self {
            Layer::Group(group) => group
                .contents
                .layers
                .iter()
                .for_each(|layer| layer.count_into(count)),
            Layer::Clipped(piece) => piece.entry.count_into(count),
            Layer::Erase(erase) => erase.entry.count_into(count),
            Layer::Split(split) => split
                .pixels
                .layers
                .iter()
                .chain(split.document.layers.iter())
                .for_each(|layer| layer.count_into(count)),
            Layer::Deferred(deferred) => {
                deferred.pixels.picture.count_into(count);
                if let Some(document) = &deferred.document {
                    document.picture.count_into(count);
                }
            }
            Layer::ReplayedText(text) => {
                text.recording.count_into(count);
                text.whole.count_into(count);
                if let Some(document) = &text.document {
                    document.picture.count_into(count);
                }
            }
            Layer::Picture(_)
            | Layer::Blur(_)
            | Layer::Targeted(_)
            | Layer::Cut(_)
            | Layer::Gradient(_)
            | Layer::Pattern(_) => {}
        }
    }
}

impl Layer {
    /// Calls `each` with every image this entry holds that its pictures do
    /// not count -- see [`Split::image`], [`Entries::held`] and a put's
    /// pixels, [`PutPixels::image`] -- once for
    /// each time it holds it: the walk the count a page keeps as it files
    /// is held to, and what a document of a canvas is found to hold.
    pub(crate) fn held_images(&self, each: &mut dyn FnMut(HeldImage)) {
        match self {
            Layer::Group(group) => group.contents.held_images(each),
            Layer::Clipped(piece) => piece.entry.held_images(each),
            // A put's pixels, which its entry's picture draws and does not
            // count: a copy of the `ImageData`, held for as long as the entry.
            Layer::Erase(erase) => {
                erase.entry.held_images(each);
                erase
                    .written
                    .as_ref()
                    .and_then(|written| HeldImage::of(&written.image))
                    .into_iter()
                    .for_each(each);
            }
            Layer::Split(split) => split.held_images(each),
            Layer::Pattern(draw) => draw.source.held_images(each),
            Layer::Picture(_)
            | Layer::Blur(_)
            | Layer::Targeted(_)
            | Layer::Cut(_)
            | Layer::Gradient(_)
            | Layer::Deferred(_)
            | Layer::ReplayedText(_) => {}
        }
    }

    /// Calls `each` with the page of every pattern this entry draws from --
    /// see [`TileSource`] -- once for each time it draws from it.
    pub(crate) fn tile_sources(&self, each: &mut dyn FnMut(&Arc<TileSource>)) {
        match self {
            Layer::Group(group) => group
                .contents
                .layers
                .iter()
                .for_each(|layer| layer.tile_sources(each)),
            Layer::Clipped(piece) => piece.entry.tile_sources(each),
            Layer::Erase(erase) => erase.entry.tile_sources(each),
            Layer::Split(split) => split
                .pixels
                .layers
                .iter()
                .chain(split.document.layers.iter())
                .for_each(|layer| layer.tile_sources(each)),
            Layer::Pattern(draw) => each(&draw.source),
            Layer::Picture(_)
            | Layer::Blur(_)
            | Layer::Targeted(_)
            | Layer::Cut(_)
            | Layer::Gradient(_)
            | Layer::Deferred(_)
            | Layer::ReplayedText(_) => {}
        }
    }
}

/// The bytes one generation of a page's recording holds of one kind: its
/// pictures -- see
/// [`PageRecorder::recorded_bytes`](super::page::PageRecorder::recorded_bytes)
/// -- or the rasters of the patterns its entries draw from -- see
/// [`TileSource::count_raster_into`]. Added to as each entry is filed and as
/// each picture or raster made when first played is made, and moved as a
/// raster is replaced. Shared with those pictures and patterns, which an
/// export can play on another thread.
///
/// `Relaxed` throughout: the count is a figure told to the JavaScript
/// engine, and nothing is published through it -- no thread reads a picture
/// because it read the count. Every change to one atomic falls in a single
/// order, so none is lost; a report made while another thread records may
/// miss that one picture or raster, which the next report counts.
#[derive(Clone, Debug, Default)]
pub(crate) struct RecordedCount(Arc<AtomicUsize>);

impl RecordedCount {
    pub(crate) fn add(&self, bytes: usize) {
        self.0.fetch_add(bytes, Ordering::Relaxed);
    }

    pub(crate) fn bytes(&self) -> usize {
        self.0.load(Ordering::Relaxed)
    }

    /// The count, held without keeping it: gone once the generation it
    /// counts is.
    fn weak(&self) -> WeakCount {
        WeakCount(Arc::downgrade(&self.0))
    }
}

/// A [`RecordedCount`] held weakly, by a [`LazyPicture`] or a [`TileSource`]
/// filed into it.
#[derive(Debug)]
struct WeakCount(Weak<AtomicUsize>);

impl WeakCount {
    /// Whether this holds `count`.
    fn holds(&self, count: &RecordedCount) -> bool {
        std::ptr::eq(self.0.as_ptr(), Arc::as_ptr(&count.0))
    }

    /// Whether the generation it counts is gone.
    fn is_gone(&self) -> bool {
        self.0.strong_count() == 0
    }

    fn add(&self, bytes: usize) {
        if let Some(count) = self.0.upgrade() {
            count.fetch_add(bytes, Ordering::Relaxed);
        }
    }

    /// Moves the count from `was` bytes to `now`, where it still counts.
    fn adjust(&self, was: usize, now: usize) {
        if let Some(count) = self.0.upgrade() {
            count.fetch_add(now, Ordering::Relaxed);
            count.fetch_sub(was, Ordering::Relaxed);
        }
    }
}

/// A picture recorded the first time a target asks for it, which then adds
/// its bytes to the count of every page that filed it, once for each time
/// that page filed it -- see [`Layer::count_into`]. A page holds it through
/// another canvas drawn in as well as through its own draws, since a canvas
/// drawn in brings its page's entries without copying them, and a walk of
/// the page counts it once for each entry holding it. The thread that
/// records it adds them after the picture is in place, so a count never
/// runs ahead of the pictures a walk of the page finds.
#[derive(Debug, Default)]
pub struct LazyPicture {
    picture: OnceLock<Option<Picture>>,
    /// Each count the picture was filed into, with how many times. Held
    /// weakly, and those whose generation is gone dropped each time it is
    /// filed into another, so that it holds one for each live generation
    /// filing it -- each of which holds the picture itself -- however many
    /// pages filed it and went: a canvas drawn into a fresh one every frame.
    counted_by: Mutex<Vec<(WeakCount, usize)>>,
}

impl Clone for LazyPicture {
    /// The picture as it stands, counted by the pages that file the clone
    /// rather than by those that filed this.
    fn clone(&self) -> Self {
        Self {
            picture: self.picture.clone(),
            counted_by: Mutex::default(),
        }
    }
}

impl From<Option<Picture>> for LazyPicture {
    /// `picture`, recorded already.
    fn from(picture: Option<Picture>) -> Self {
        Self {
            picture: OnceLock::from(picture),
            counted_by: Mutex::default(),
        }
    }
}

impl LazyPicture {
    /// The picture, `Some` once it has been recorded -- `Some(None)` where
    /// Skia recorded nothing.
    pub(crate) fn get(&self) -> Option<&Option<Picture>> {
        self.picture.get()
    }

    /// The picture, recorded by `record` now if nothing has asked for it
    /// yet, its bytes then added to the counts it was filed into.
    pub(crate) fn get_or_record(
        &self,
        record: impl FnOnce() -> Option<Picture>,
    ) -> Option<&Picture> {
        let mut made = 0;
        let picture = self.picture.get_or_init(|| {
            let picture = record();
            made = picture.as_ref().map_or(0, Picture::approximate_bytes_used);
            picture
        });
        if made > 0 {
            for (count, times) in self.counted_by.lock().iter() {
                count.add(made * times);
            }
        }
        picture.as_ref()
    }

    /// The bytes the picture holds, 0 until it is recorded.
    pub(crate) fn bytes(&self) -> usize {
        self.picture
            .get()
            .and_then(Option::as_ref)
            .map_or(0, Picture::approximate_bytes_used)
    }

    /// Adds the picture's bytes to `count` when it is recorded, once more
    /// for each time this is asked with it.
    fn count_into(&self, count: &RecordedCount) {
        let mut counts = self.counted_by.lock();
        counts.retain(|(count, _)| !count.is_gone());
        match counts.iter_mut().find(|(held, _)| held.holds(count)) {
            Some((_, times)) => *times += 1,
            None => counts.push((count.weak(), 1)),
        }
    }

    /// How many counts the picture is filed into.
    #[cfg(test)]
    pub(crate) fn counts(&self) -> usize {
        self.counted_by.lock().len()
    }
}

#[cfg(test)]
thread_local! {
    /// Where the deferred runs filed on this thread count their recordings,
    /// on whichever thread plays them -- see [`DeferredPicture`]: those
    /// played as pixels, then those played for a document.
    pub(crate) static DEFERRED_WATCH: RefCell<Option<Arc<DeferredCounts>>> =
        const { RefCell::new(None) };
    /// A mutant of [`DeferredPicture::picture`] that records nothing, for the
    /// runs filed on this thread: every target that plays one must then draw
    /// otherwise.
    pub(crate) static DEFERRED_DRAWS_NOTHING: Cell<bool> =
        const { Cell::new(false) };
    /// How many runs this thread has filed to be recorded when first played.
    pub(crate) static DEFERRED_FILED: Cell<usize> = const { Cell::new(0) };
    /// How many entries [`Layer::recorded_bytes`] has been asked of on this
    /// thread, a group's own and each it holds counted apart.
    pub(crate) static ENTRIES_WALKED: Cell<usize> = const { Cell::new(0) };
    /// How many times [`drawn_on_their_backdrop`] has read a GPU canvas's
    /// pixels back on this thread.
    pub(crate) static BACKDROPS_READ: Cell<usize> = const { Cell::new(0) };
    /// How many times [`Clipped::drawn`] has found what a piece draws on
    /// this thread, each piece once.
    pub(crate) static PIECES_FOUND: Cell<usize> = const { Cell::new(0) };
    /// A mutant of [`replayed_on_their_backdrop`] set on this thread: an
    /// entry whose reach meets a run ends the batch there, without asking
    /// what a piece draws within its clip.
    pub(crate) static REACH_UNFOUND: Cell<bool> = const { Cell::new(false) };
    /// A mutant of the batch [`play_entries`] makes of consecutive
    /// [`ReplayedText`] runs on a GPU canvas, set on this thread: each run
    /// read back on its own.
    pub(crate) static BACKDROP_EACH_RUN: Cell<bool> = const { Cell::new(false) };
    /// Whether [`replayed_on_their_backdrop`] draws runs on this thread:
    /// off, every run takes the GPU's own route, the reference the bytes a
    /// batch leaves beside its runs are held to.
    pub(crate) static BACKDROP_ROUTE: Cell<bool> = const { Cell::new(true) };
    /// Whether [`played_on_a_root`] plays a layer's entries on a surface of
    /// their own on this thread: off, every layer's entries play inside it,
    /// where their runs are drawn by the GPU.
    pub(crate) static OWN_ROOTS: Cell<bool> = const { Cell::new(true) };
    /// Whether [`Group::played_unlayered`] draws a plain group on this
    /// thread: off, every group composites through its layer, the reference
    /// the clips standing in for it are held to.
    pub(crate) static UNLAYERED: Cell<bool> = const { Cell::new(true) };
    /// Whether [`drawn_as_bytes`] composites a translucent image on a GPU
    /// canvas in the CPU's arithmetic on this thread: off, Ganesh draws it
    /// as recorded, in its own blend (#837).
    pub(crate) static EXACT_IMAGES: Cell<bool> = const { Cell::new(true) };
    /// How many images [`drawn_as_bytes`] has composited on this thread.
    pub(crate) static IMAGES_AS_BYTES: Cell<usize> = const { Cell::new(0) };
    /// How many layers' entries [`played_on_a_root`] has played on a
    /// surface of their own on this thread.
    pub(crate) static ROOTS_PLAYED: Cell<usize> = const { Cell::new(0) };
}

/// How many of a [`DeferredPicture`]'s recordings were made: those played as
/// pixels, then those played for a document.
#[cfg(test)]
pub(crate) type DeferredCounts = [std::sync::atomic::AtomicUsize; 2];

/// A kept run of text under a filter chain, filed as the draw it is and
/// recorded the first time a target plays it.
///
/// On a page the coverage route plays on, a run drawn again where it was kept
/// composites what it kept without drawing its coverage -- see
/// `run_mask::kept` -- and nothing else of the chain's group is played, so a
/// picture recorded at every draw would be thrown away unplayed. Recorded
/// here, under the transform and over the bounds the draw would have been
/// recorded under, with the same calls -- the run's blobs drawn as
/// [`LaidOut::paint`](crate::node::typography::LaidOut::paint) draws them,
/// then [`reach_the_clip`] -- it is the picture the draw would have
/// recorded. What the run can ink, which bounds the chain's layer, is worked
/// out when it is filed, without drawing.
///
/// On a page that keeps a recording for each kind of target it stands for
/// the pair [`Layer::Targeted`] holds: the run in the paint placed for pixels,
/// and in its own paint for a document, each recorded when a target of its
/// kind first plays it.
pub struct DeferredPicture {
    pixels: Deferral,
    document: Option<Deferral>,
    bounds: Rect,
    matrix: Matrix,
    /// The test switches of the thread that filed it -- see
    /// [`DEFERRED_WATCH`] and [`DEFERRED_DRAWS_NOTHING`].
    #[cfg(test)]
    watch: (Option<Arc<DeferredCounts>>, bool),
}

/// One of a [`DeferredPicture`]'s recordings: the draw, and its picture once
/// a target has played it.
struct Deferral {
    text: DrawnText,
    picture: LazyPicture,
}

impl Deferral {
    fn new(text: DrawnText) -> Self {
        Self {
            text,
            picture: LazyPicture::default(),
        }
    }
}

impl fmt::Debug for DeferredPicture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeferredPicture")
            .field("recorded", &self.pixels.picture.get().is_some())
            .field("pair", &self.document.is_some())
            .finish_non_exhaustive()
    }
}

impl DeferredPicture {
    /// `pixels`, and on a page that keeps a recording for each kind of
    /// target `document`, each recorded over `bounds` under `matrix` when
    /// first played.
    pub(crate) fn new(
        pixels: DrawnText,
        document: Option<DrawnText>,
        bounds: Rect,
        matrix: Matrix,
    ) -> Self {
        #[cfg(test)]
        DEFERRED_FILED.set(DEFERRED_FILED.get() + 1);
        Self {
            pixels: Deferral::new(pixels),
            document: document.map(Deferral::new),
            bounds,
            matrix,
            #[cfg(test)]
            watch: (
                DEFERRED_WATCH.with_borrow(Clone::clone),
                DEFERRED_DRAWS_NOTHING.get(),
            ),
        }
    }

    /// The picture played as pixels, recorded now if no target has played
    /// it yet. `None` where Skia records nothing.
    pub(crate) fn picture(&self) -> Option<&Picture> {
        self.recorded(&self.pixels)
    }

    /// The picture played for `target`: a document's own where the page
    /// keeps one, the pixels' otherwise.
    pub(crate) fn played_for(&self, target: Target) -> Option<&Picture> {
        match (target, &self.document) {
            (Target::Document, Some(document)) => self.recorded(document),
            _ => self.picture(),
        }
    }

    /// Whether this stands for a pair of recordings -- see
    /// [`holds_a_pair`].
    pub(crate) fn is_pair(&self) -> bool {
        self.document.is_some()
    }

    fn recorded<'a>(&self, deferral: &'a Deferral) -> Option<&'a Picture> {
        deferral.picture.get_or_record(|| {
            #[cfg(test)]
            if let Some(watch) = &self.watch.0 {
                let kind = usize::from(!std::ptr::eq(deferral, &self.pixels));
                watch[kind].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            let mut recorder = PictureRecorder::new();
            let canvas = recorder.begin_recording(self.bounds, true);
            canvas.set_matrix(&self.matrix.into());
            #[cfg(test)]
            if self.watch.1 {
                return recorder.finish_recording_as_picture(None);
            }
            deferral.text.draw(canvas);
            reach_the_clip(canvas);
            recorder.finish_recording_as_picture(None)
        })
    }
}

/// What a run's coverage copy is drawn from -- see [`CoverageRun`]: a
/// picture, or the run's own picture recorded when first played.
#[derive(Clone, Debug)]
pub enum CoveragePicture {
    /// Recorded when the run was drawn.
    Recorded(Picture),
    /// The run's own -- see [`DeferredPicture`].
    Deferred(Arc<DeferredPicture>),
}

impl CoveragePicture {
    /// The picture, recorded now where it was deferred.
    pub(crate) fn picture(&self) -> Option<&Picture> {
        match self {
            CoveragePicture::Recorded(picture) => Some(picture),
            CoveragePicture::Deferred(deferred) => deferred.picture(),
        }
    }
}

thread_local! {
    /// Whether [`reach_the_clip`] records nothing on this thread, so that a
    /// recording is bounded by what it inks -- see [`inked_by`].
    static INKED_ONLY: Cell<bool> = const { Cell::new(false) };
}

/// What `draw` inks, recorded onto a canvas over `bounds` under `matrix`:
/// the bounds Skia gives the recording of it with [`reach_the_clip`]
/// recording nothing, which a run of text is otherwise bounded by.
pub(crate) fn inked_by(
    bounds: Rect,
    matrix: &Matrix,
    draw: impl FnOnce(&SkCanvas),
) -> Rect {
    let mut recorder = PictureRecorder::new();
    let canvas = recorder.begin_recording(bounds, true);
    canvas.set_matrix(&(*matrix).into());
    let was = INKED_ONLY.replace(true);
    draw(canvas);
    INKED_ONLY.set(was);
    recorder
        .finish_recording_as_picture(None)
        .map_or_else(Rect::new_empty, |picture| picture.cull_rect())
}

/// The key of the annotation [`reach_the_clip`] records. No backend acts on
/// it: raster and GPU devices draw no annotation, and a PDF or SVG writes
/// only the ones whose keys it knows -- a link, a named destination.
const TEXT_REACH: &str = "meo-skia-canvas/text-reach";

/// Records on `canvas`, beside a text draw, an annotation over what its clip
/// allows, so that the bounds Skia gives the recording -- the rectangle
/// [`LayerRec::footprint`] confines a layer to -- take in all of it.
///
/// Skia bounds a run of text by its glyphs hinted at the scale it is
/// recorded at, and played back at another -- a page exported or read at a
/// density -- hinting puts the ink elsewhere: on FreeType a line of text at
/// density 2 drew a device row above its bounds, which a layer confined to
/// them cut off. Text scaled by a transform where it is recorded is bounded
/// at that scale, and kept to its bounds. The scale a page is played back
/// at is not known when it is recorded, so text is bounded by the clip
/// instead, which scales with the page, and a layer holding text is left as
/// large as the clip, as every layer was before layers were confined. Draws
/// that are not text keep their own bounds.
pub(crate) fn reach_the_clip(canvas: &SkCanvas) {
    if INKED_ONLY.get() {
        return;
    }
    let Some(clip) = canvas.device_clip_bounds() else {
        return;
    };
    canvas.save();
    canvas.reset_matrix();
    canvas.draw_annotation(Rect::from(clip), TEXT_REACH, &Data::new_empty());
    canvas.restore();
}

#[cfg(test)]
thread_local! {
    /// Whether a chain's layer is bounded by the text it holds where it
    /// says what it can ink: off, it is bounded as any other group's, by
    /// what its entries reach, which for text is the clip -- the reference
    /// a confined layer is held to.
    pub(crate) static KEEPS_TEXT_REACH: Cell<bool> = const { Cell::new(true) };
    /// The rectangle, in device pixels, the last chain played on this thread
    /// was given to bound what it filters -- its layer, or the coverage
    /// route's plane -- where it was given one.
    pub(crate) static CHAIN_CONTENT: Cell<Option<Rect>> = const { Cell::new(None) };
    /// Whether a blurred draw may take the coverage route -- see
    /// [`chain::coverage`] -- so that a test can hold it to the chain's layer.
    pub(crate) static COVERAGE_ROUTE: Cell<bool> = const { Cell::new(true) };
    /// How many blurred draws played on this thread took the coverage route.
    pub(crate) static COVERAGE_RUNS: Cell<usize> = const { Cell::new(0) };
    /// How many translucent runs played on this thread were drawn from their
    /// coverage -- see [`FlatRun`].
    pub(crate) static FLAT_RUNS: Cell<usize> = const { Cell::new(0) };
    /// The device pixels the planes of the translucent runs drawn from their
    /// coverage on this thread were read over, joined.
    pub(crate) static FLAT_REGION: Cell<Option<IRect>> = const { Cell::new(None) };
    /// How many shadows played on this thread were cast from a run's opaque
    /// copy -- see [`chain::Cast`].
    pub(crate) static CAST_RUNS: Cell<usize> = const { Cell::new(0) };

    /// Whether the coverage route draws its plane through the chain's own
    /// paint, shader and all, so that a test can hold the plain paint it
    /// draws through -- see [`BlurOp::playback`] -- to it.
    pub(crate) static SHADED_PLANE: Cell<bool> = const { Cell::new(false) };
}

/// Whether [`Composite::Chain`]'s `reach` bounds its layer -- see
/// [`KEEPS_TEXT_REACH`]; always outside a test.
fn keeps_text_reach() -> bool {
    #[cfg(test)]
    let keeps = KEEPS_TEXT_REACH.get();
    #[cfg(not(test))]
    let keeps = true;
    keeps
}

/// How far, in device pixels, a run of text played back at another scale
/// than it was recorded at can ink past its reach -- see `RunPaint::reach` --
/// measured, not chosen, by `text_in_a_chain_is_bounded_by_all_it_inks` with
/// TEXT_REACH_SWEEP=full: text at 8 to 96px, filled, hairline, stroked 1, 12
/// and 40 wide under each join, bold, spaced, upright, turned, skewed and
/// scaled, under a blur and two drop shadows, each run laid out afresh and
/// kept, at densities 1 to 3 and drawn into another canvas at 0.5 and 1.5.
///
/// - On CoreText, in Oswald, Helvetica, Times New Roman, Zapfino, Apple Color
///   Emoji and Monoton, with each glyph's own box and not the font's -- see
///   `typography::keeps_font_box`: on the CPU raster 2,558 of 25,600 playbacks
///   inked past the reach with no margin, and on Metal 2,510 of 25,600, each by
///   at most 1.73 pixels: Helvetica's 40px "fj Wq", scaled 1.7, kept and drawn
///   at 0.5.
/// - On FreeType, on the Linux runner's CPU raster, in Oswald, DejaVu Sans,
///   Noto Color Emoji and Monoton, with the font's box as well, 142 of 16,640
///   did, by at most 1.70: DejaVu Sans's 13px "fj Wq", scaled 1.7 and stroked
///   40, at density 1. The runner's Vulkan device plays the sweep a test run
///   plays, not the full one.
///
/// Two pixels, the smallest whole number at or above the worst measured,
/// 1.73. What a reading measures is the ink's whole-pixel edge less the
/// reach's own, `ceil(reach + s) - reach`, where `s` is how far the ink truly
/// spills past the reach, so a reading of `o` says only `o - 1 < s <= o`; and
/// a pixel is cut past `ceil(reach + HINTED_INK_DRIFT)`, so every phase of the
/// reach against the pixel grid is held exactly when `s` is within this. A
/// reach shrunk by 1.1 pixels on every side fails the CoreText sweep, 15,686
/// playbacks inking past it by up to 7. The worst with the font's box were
/// strokes whose join half their width bounds exactly: the stroke's
/// anti-aliased fringe. On CoreText that case is held by
/// `a_wide_round_stroke_inks_a_pixel_past_its_reach`.
pub(crate) const HINTED_INK_DRIFT: f32 = 2.0;

/// `reach`, in the units `to_device` maps to the device, widened there by
/// [`HINTED_INK_DRIFT`] device pixels and taken back.
pub(crate) fn hinted_reach(reach: Rect, to_device: &Matrix) -> Rect {
    let device = to_device
        .map_rect(reach)
        .0
        .with_outset((HINTED_INK_DRIFT, HINTED_INK_DRIFT));
    to_device
        .invert()
        .map_or(reach, |back| back.map_rect(device).0)
}

/// The paint a chain composites through: `rec`'s, less its image filter,
/// which the chain runs itself -- see [`Composite::Chain`].
fn chain_paint(rec: &LayerRec) -> Paint {
    let mut paint = rec.paint.clone().unwrap_or_default();
    paint.set_image_filter(None);
    paint
}

/// The rectangle everything in `entries` can draw in -- see [`Layer::reach`]
/// -- empty where they are none, `None` where one of them is not known.
pub(crate) fn reach_of(entries: &[Layer]) -> Option<Rect> {
    entries
        .iter()
        .try_fold(Rect::new_empty(), |mut all, entry| {
            all.join(entry.reach()?);
            Some(all)
        })
}

thread_local! {
    /// The tiles rasterized on the GPU in the playback under way on this
    /// thread -- see [`TileScope`].
    static GPU_TILES: RefCell<GpuTiles> = RefCell::default();
}

/// See [`TileScope`].
#[derive(Default)]
struct GpuTiles {
    /// How many scopes are open.
    open: usize,
    /// Each tile under its source's address and its density.
    tiles: Vec<((usize, u32), Image)>,
}

/// Keeps each tile a GPU playback rasterizes for the rest of that playback,
/// so that a pattern drawn many times on a page rasterizes its source once
/// rather than at each draw: 200 small fills dyed by one pattern made a
/// GPU surface and played the source's page onto it 200 times.
///
/// Opened where a page is played onto pixels, and let go there when the
/// playback ends: a GPU image lives on its thread's context and has to
/// be let go on that thread, and a playback whose draws the GPU discarded
/// is drawn again on a fresh surface with tiles of its own. A source is
/// filed by its address, which stays its own while the page that holds it
/// is played. Outside a scope nothing is kept.
pub(crate) struct TileScope(PhantomData<*const ()>);

impl TileScope {
    /// Opens a scope on this thread, with no tile kept yet: a scope opened
    /// inside another starts afresh, and the outer one rasterizes again what
    /// it needs.
    pub(crate) fn open() -> Self {
        GPU_TILES.with_borrow_mut(|held| {
            held.open += 1;
            held.tiles.clear();
        });
        Self(PhantomData)
    }

    fn held(key: (usize, u32)) -> Option<Image> {
        GPU_TILES.with_borrow(|held| {
            held.tiles
                .iter()
                .find(|(at, _)| *at == key)
                .map(|(_, image)| image.clone())
        })
    }

    fn hold(key: (usize, u32), image: &Image) {
        GPU_TILES.with_borrow_mut(|held| {
            if held.open > 0 {
                held.tiles.push((key, image.clone()));
            }
        });
    }
}

impl Drop for TileScope {
    fn drop(&mut self) {
        GPU_TILES.with_borrow_mut(|held| {
            held.open -= 1;
            if held.open == 0 {
                held.tiles.clear();
            }
        });
    }
}

/// A flat colour held exactly in a page's own space, for the recording a
/// page plays as pixels: Skia converts a plain paint colour through an
/// approximation and lands it off -- see `page::Recorders`.
#[derive(Clone, Debug, PartialEq)]
pub struct ExactColour {
    /// The colour's components in `space`, unpremultiplied, opaque.
    pub rgb: [f32; 3],
    /// The page's space.
    pub space: ColorSpace,
    /// The page's space, as the one of this crate's spaces it is.
    pub pixels: PixelColorSpace,
    /// Whether the page composites in eight bits a channel, where a paint
    /// of the colour is placed on the bytes it stores -- see [`Self::at`].
    pub bytes: bool,
}

/// Half the spacing of binary16 between 0.5 and 1, in levels of a byte:
/// its 10-bit fraction spaces values there 2^-11 apart, so rounding moves
/// one by at most 2^-12, 0.0623 of a level. Ganesh may carry a paint's
/// colour at half precision and its premultiplied product once more, so
/// the bound is twice that, 0.1245.
const GPU_HALF_ROUNDING_LEVELS: f64 = 2.0 * 255.0 / 4096.0;

/// How far from its byte a placed colour may land, in Skia's own float
/// conversion, and still store that byte on either engine: half a level,
/// less [`GPU_HALF_ROUNDING_LEVELS`].
const PLACED_MARGIN_LEVELS: f64 = 0.5 - GPU_HALF_ROUNDING_LEVELS;

/// `alpha` on the byte an eight-bit page stores it on, rounded half up as
/// Skia rounds a paint's own colour
/// (`SkRasterPipeline::append_constant_color`): a fraction of 255, so that it
/// multiplies onto a whole byte.
pub(crate) fn byte_alpha(alpha: f32) -> f32 {
    let levels = f32::from(u8::MAX);
    (alpha.clamp(0.0, 1.0) * levels + 0.5).trunc() / levels
}

/// How far inside the edge of a byte's span [`nearest_storing`] keeps a
/// colour, in levels. Skia rounds a paint's colour to a byte from `f32`
/// (`Sk4f_toL32`: `x * 255 + 0.5`, truncated), and a colour placed on the
/// edge itself could round to the byte beside it once through `f32`.
const BYTE_EDGE_SLACK: f64 = 1.0 / 64.0;

/// The colour, in levels, nearest `plain` of those that store `product`
/// premultiplied at the alpha byte `stored`, all three in levels, on
/// either engine: one that Skia's legacy blit rounds to a byte `u` whose
/// `u * stored / 255` rounds to `product` -- `SkMulDiv255Round`, which
/// never meets a tie, 255 being odd -- and whose own product lands within
/// [`PLACED_MARGIN_LEVELS`] of `product`, as Ganesh carries it.
///
/// [`ExactColour::at`]'s colour, `product * 255 / stored`, is always one of
/// them, and is what this gives where the margins leave no span.
fn nearest_storing(plain: f64, product: f64, stored: f64) -> f64 {
    let levels = f64::from(u8::MAX);
    let scale = levels / stored;
    let first = ((product - 0.5) * scale).ceil().max(0.0);
    let last = (((product + 0.5) * scale).ceil() - 1.0).min(levels);
    let low = (first - 0.5 + BYTE_EDGE_SLACK)
        .max((product - PLACED_MARGIN_LEVELS) * scale)
        .max(0.0);
    let high = (last + 0.5 - BYTE_EDGE_SLACK)
        .min((product + PLACED_MARGIN_LEVELS) * scale)
        .min(levels);
    match low <= high {
        true => plain.clamp(low, high),
        false => product * scale,
    }
}

/// How many times a placed colour that lands off its byte is moved back
/// toward it and drawn again before [`ExactColour::shader`] takes it.
const NUDGES: usize = 3;

/// The slots of the table each thread keeps its verdicts in -- see
/// [`ExactColour::verified_paint`]. Per thread, because a verdict is found
/// by drawing on a probe surface that thread owns, and a page is drawn on
/// the thread that holds it. A colour is filed in the one slot
/// [`verdict_slot`] picks, over whatever was filed there before, and a
/// lookup compares the whole key: two colours sharing a slot only cost
/// each other a verdict found again, never one taken for the other. Four
/// thousand hold any palette a page repeats, a few dozen bytes each, and a
/// page of colours never repeated costs no more than the table; with no
/// chain to probe, a lookup is one multiplication and one comparison.
const VERDICTS_KEPT: usize = 4096;

const _: () = assert!(VERDICTS_KEPT.is_power_of_two());

/// A placed colour's page space and the premultiplied bytes it is to store.
type VerdictKey = (PixelColorSpace, [u8; 4]);

/// The paint colour that lands a placed colour on its bytes and the space
/// it is tagged with, or `None` where [`ExactColour::shader`] draws it.
type Verdict = Option<(Color4f, ColorSpace)>;

/// The slot of [`VERDICTS`] `key` is filed in.
fn verdict_slot((pixels, bytes): VerdictKey) -> usize {
    let key = u64::from(u32::from_le_bytes(bytes)) | (pixels as u64) << 32;
    (key.wrapping_mul(GOLDEN_RATIO_64)
        >> (u64::BITS - VERDICTS_KEPT.trailing_zeros())) as usize
}

/// How many sets of dyes [`ExactColour::dye`] keeps on a thread, and how
/// many a set holds: 256 dyes, each 48 bytes with its key, 12 KiB a
/// thread once the first is read, for text set in a palette of a few dozen
/// colours on pages of a few spaces. A dye filed over another is read again
/// when it is next asked for, which costs what reading it always did.
const DYE_SETS: usize = 64;
const DYE_WAYS: usize = 4;

/// An exact colour and the plain colour beside it, by bits, as
/// [`ExactColour::dye`] files the dye it read for them.
type DyeKey = (ExactColour, [u32; 4]);

/// Where [`DYES`] files `key`: its bits folded together and multiplied by
/// [`GOLDEN_RATIO_64`].
fn dye_hash((exact, plain): &DyeKey) -> u64 {
    let [r, g, b] = exact.rgb.map(|c| u64::from(c.to_bits()));
    let [pr, pg, pb, pa] = plain.map(u64::from);
    let key = (r ^ g.rotate_left(21) ^ b.rotate_left(42))
        ^ (pr.rotate_left(11) ^ pg.rotate_left(32) ^ pb.rotate_left(53) ^ pa)
        ^ (exact.pixels as u64) << 59
        ^ u64::from(exact.bytes) << 63;
    key.wrapping_mul(GOLDEN_RATIO_64)
}

/// The pixel [`landed`] draws a colour on for a page space: a float page of
/// that space, one pixel in size, and the space's name in `color()` -- see
/// [`skia_paint_color_onto_own`] -- each found once a thread.
struct Probe {
    pixels: PixelColorSpace,
    surface: Surface,
    name: Option<&'static str>,
}

#[cfg(test)]
thread_local! {
    /// How many dyes [`ExactColour::dye`] has read on this thread rather
    /// than found kept.
    pub(crate) static DYES_READ: Cell<usize> = const { Cell::new(0) };
    /// Whether [`ExactColour::dye`] takes a dye kept: off, every dye is read
    /// afresh, the reference the kept ones are held to.
    pub(crate) static KEEPS_DYES: Cell<bool> = const { Cell::new(true) };
}

thread_local! {
    /// The verdicts this thread has found, filed by [`verdict_slot`].
    static VERDICTS: RefCell<Vec<Option<(VerdictKey, Verdict)>>> =
        RefCell::new(vec![None; VERDICTS_KEPT]);

    /// The dyes this thread has read -- see [`ExactColour::dye`].
    static DYES: RefCell<KeptSets<DyeKey, Option<ColorFilter>, DYE_SETS, DYE_WAYS>> =
        const { RefCell::new(KeptSets::new()) };

    /// The probe of each page space this thread has verified a colour for.
    static PROBES: RefCell<Vec<Probe>> = const { RefCell::new(Vec::new()) };
}

/// The colour shader of `plain` named in sRGB that a dyed text paint draws
/// over -- see `Context2D::exact_text_paint` -- the one this thread made
/// last for that colour. Kept, rather than made for every draw, because a
/// kept run keeps its blobs for a paint only while the next draw's paint
/// compares equal to it, and two paints compare their shaders by identity
/// -- see `LaidOut::kept_blobs` -- so a shader made afresh built the run's
/// blobs again on every draw of a float page.
pub(crate) fn plain_colour_shader(plain: Color4f) -> Shader {
    let key = [plain.r, plain.g, plain.b, plain.a].map(f32::to_bits);
    let hash = plain_shader_hash(&key);
    if let Some(shader) =
        PLAIN_SHADERS.with_borrow(|shaders| shaders.get(hash, &key).cloned())
    {
        return shader;
    }
    let shader = shaders::color_in_space(plain, ColorSpace::new_srgb());
    PLAIN_SHADERS
        .with_borrow_mut(|shaders| shaders.file(hash, key, shader.clone()));
    shader
}

/// Where [`PLAIN_SHADERS`] files a colour's bits: folded together and
/// multiplied by [`GOLDEN_RATIO_64`].
fn plain_shader_hash(key: &[u32; 4]) -> u64 {
    let [r, g, b, a] = key.map(u64::from);
    (r ^ g.rotate_left(16) ^ b.rotate_left(32) ^ a.rotate_left(48))
        .wrapping_mul(GOLDEN_RATIO_64)
}

thread_local! {
    /// The colour shaders [`plain_colour_shader`] has made on this thread,
    /// in as many sets and ways as the dyes they are drawn under.
    static PLAIN_SHADERS: RefCell<KeptSets<[u32; 4], Shader, DYE_SETS, DYE_WAYS>> =
        const { RefCell::new(KeptSets::new()) };
}

/// The paint colour that stores `want`, premultiplied bytes, on a page in
/// `page`, of the pixels `pixels` names, and the space it is tagged with:
/// the placed colour, or one moved toward its bytes up to [`NUDGES`] times,
/// where a float pixel of the page's space lands within
/// [`PLACED_MARGIN_LEVELS`] of each byte. `None` where none does.
///
/// The float pixel stands for an eight-bit one on either engine: a colour
/// that lands within the margin stores its bytes on both, which
/// `a_colour_landing_within_the_margin_stores_its_bytes_on_either_engine`
/// holds over every colour this is asked for. Ganesh converts a paint's
/// colour on the CPU, as the raster pipeline does, and carries it from
/// there; a readback of a GPU surface per colour would stall its pipeline.
fn landed(
    page: &ColorSpace,
    pixels: PixelColorSpace,
    want: [u8; 4],
) -> Verdict {
    let stored = f64::from(want[3]);
    let alpha = (stored / f64::from(u8::MAX)) as f32;
    if want[3] == 0 {
        return Some((Color4f::new(0.0, 0.0, 0.0, 0.0), page.clone()));
    }
    let info = ImageInfo::new(
        (1, 1),
        ColorType::RGBAF32,
        AlphaType::Premul,
        Some(page.clone()),
    );
    PROBES.with(|probes| {
        let mut probes = probes.borrow_mut();
        let at = match probes.iter().position(|probe| probe.pixels == pixels) {
            Some(at) => at,
            None => {
                probes.push(Probe {
                    pixels,
                    surface: started::raster(&info, None, None)?,
                    name: color_space_name(page),
                });
                probes.len() - 1
            }
        };
        let Probe { surface, name, .. } = &mut probes[at];
        let mut rgb = [0, 1, 2].map(|i| f64::from(want[i]) / stored);
        for _ in 0..=NUDGES {
            let (colour, space) = skia_paint_color_onto_own(
                Color4f::new(
                    rgb[0] as f32,
                    rgb[1] as f32,
                    rgb[2] as f32,
                    alpha,
                ),
                page,
                *name,
            );
            let mut paint = Paint::new(colour, Some(&space));
            paint.set_blend_mode(BlendMode::Src);
            surface.canvas().draw_paint(&paint);
            let mut float = [0u8; 16];
            if !surface.read_pixels(&info, &mut float, 16, (0, 0)) {
                return None;
            }
            let (float, _) = float.as_chunks::<4>();
            let off = [0, 1, 2].map(|i| {
                f64::from(f32::from_le_bytes(float[i])) * f64::from(u8::MAX)
                    - f64::from(want[i])
            });
            if off.iter().all(|o| o.abs() <= PLACED_MARGIN_LEVELS) {
                return Some((colour, space));
            }
            for i in 0..3 {
                rgb[i] -= off[i] / stored;
            }
        }
        None
    })
}

/// The verdict [`landed`] finds for `want`, premultiplied bytes, on an sRGB
/// page, found without its draw: Skia hands an sRGB paint colour over to an
/// sRGB page unconverted, so the colour `landed` tries first lands within
/// `f32` rounding of its bytes, and it takes that one as it handed it over.
/// `an_srgb_verdict_is_the_one_the_probe_finds` holds the two equal at every
/// alpha byte, with each channel at every byte it can store.
fn unconverted(want: [u8; 4]) -> Verdict {
    let stored = f64::from(want[3]);
    let alpha = (stored / f64::from(u8::MAX)) as f32;
    if want[3] == 0 {
        return Some((
            Color4f::new(0.0, 0.0, 0.0, 0.0),
            ColorSpace::new_srgb(),
        ));
    }
    let [r, g, b] = [0, 1, 2].map(|i| (f64::from(want[i]) / stored) as f32);
    Some((Color4f::new(r, g, b, alpha), ColorSpace::new_srgb()))
}

/// The SkSL of the step [`ExactColour::dye`] can end with: `plain` at the
/// source's alpha where Skia left `probe` as it was handed over, and the
/// source unchanged where it converted it. Skia converts a `layout(color)`
/// uniform from sRGB into the space it draws in, and leaves it alone when
/// that is sRGB or when there is none -- which is how Skia asks a paint's
/// colour filter for the colour a glyph mask's contrast is set from.
const PLAIN_UNCONVERTED_SKSL: &str = "layout(color) uniform float3 probe; \
    uniform float3 given; uniform float3 plain; \
    float4 main(float4 c) { \
        return probe == given ? float4(plain * c.a, c.a) : c; }";

/// What [`PLAIN_UNCONVERTED_SKSL`] hands Skia as `probe` and `given`: a
/// colour with every component strictly inside (0, 1), so that converting
/// it from sRGB into any space whose primaries or transfer function differ
/// moves it -- as it does into each space a canvas takes.
const UNCONVERTED_PROBE: [f32; 3] = [0.5, 0.25, 0.75];

/// The bits of each channel Skia keeps of the colour it sets a glyph
/// mask's contrast from: `SkMaskGamma` is `SkTMaskGamma<3, 3, 3>`
/// (SkScalerContext.h), whose `CanonicalColor` keeps the top three bits of
/// each byte, and nothing after it reads more.
const CONTRAST_BITS: u32 = 3;

/// The contrast bucket Skia files `rgb` under: each channel rounded to a
/// byte as `SkColor4f::toSkColor` rounds it -- `x * 255 + 0.5`, pinned to
/// the byte's range and truncated (`Sk4f_toL32`) -- and kept to its top
/// [`CONTRAST_BITS`]. Two colours in one bucket give a glyph mask the same
/// contrast on every platform.
fn contrast_bucket(rgb: [f32; 3]) -> [u8; 3] {
    rgb.map(|c| {
        ((c * 255.0 + 0.5).clamp(0.0, 255.0) as u8) >> (8 - CONTRAST_BITS)
    })
}

/// The colour Skia sets a glyph mask's contrast from, for a paint in
/// `plain` whose colour filter is `filter`: `filterColor4f(plain, nullptr,
/// nullptr)`, as `SkPaintPriv::ComputeLuminanceColor` asks it,
/// unpremultiplied.
///
/// Read by drawing, as skia-safe's `filter_color4f` cannot give it: a
/// paint with no shader has its colour filter folded into its colour by
/// `SkPaintPriv::RemoveColorFilter` before it draws, through
/// `filterColor4f(colour, sRGB, destination)`, and a destination with no
/// colour space leaves that the same call. Over 34440 colours at the
/// edges of contrast buckets it read the same bits as the call itself; a
/// colour shader in place of the paint's colour, which runs the filter as
/// pipeline stages instead, parted in 5520 of them.
fn luminance_colour(filter: &ColorFilter, plain: Color4f) -> Option<[f32; 3]> {
    // UPSTREAM: skia-safe 0.156.0 -- unfiled -- worked around
    // Re-check: cargo test a_filters_luminance_colour_is_its_output with
    // this returning `ColorFilter::filter_color4f(plain, None, None)`.
    //
    // `filter_color4f` declares Skia's `SkColor4f` return as four `u32`s,
    // which aarch64 and x86-64 hand back in other registers than four
    // floats, so it returns unrelated numbers.
    let info =
        ImageInfo::new((1, 1), ColorType::RGBAF32, AlphaType::Premul, None);
    let mut surface = started::raster(&info, None, None)?;
    let mut paint = Paint::default();
    paint.set_color4f(Color4f { a: 1.0, ..plain }, &ColorSpace::new_srgb());
    paint.set_color_filter(filter.clone());
    surface.canvas().draw_paint(&paint);
    let mut pixel = [0u8; 16];
    surface
        .read_pixels(&info, &mut pixel, info.min_row_bytes(), (0, 0))
        .then_some(())?;
    let (channels, _) = pixel.as_chunks::<4>();
    let [r, g, b, a] = [0, 1, 2, 3].map(|c| f32::from_le_bytes(channels[c]));
    // Unpremultiplied, as `filterColor4f` answers: a filter leaving the
    // colour translucent -- a `copy` blend of a colour at half alpha --
    // reads back premultiplied here.
    Some(match a > 0.0 {
        true => [r / a, g / a, b / a],
        false => [r, g, b],
    })
}

thread_local! {
    /// A shader returning its uniform colour as it is, unconverted:
    /// [`ExactColour::shader`] works it in the page's space, so Skia takes
    /// what it returns as a colour in that space.
    static FLAT_COLOUR: Option<RuntimeEffect> = RuntimeEffect::make_for_shader(
        "uniform float4 colour; half4 main(float2 p) { return half4(colour); }",
        None,
    )
    .ok();
}
thread_local! {
    /// [`PLAIN_UNCONVERTED_SKSL`] compiled, once per thread.
    static PLAIN_UNCONVERTED: Option<RuntimeEffect> =
        RuntimeEffect::make_for_color_filter(PLAIN_UNCONVERTED_SKSL, None).ok();
    /// [`PLAIN_WHERE_OPAQUE_SKSL`] compiled, once per thread.
    static PLAIN_WHERE_OPAQUE: Option<RuntimeEffect> =
        RuntimeEffect::make_for_color_filter(PLAIN_WHERE_OPAQUE_SKSL, None).ok();
    /// The filters [`plain_where_opaque`] has made on this thread, kept for
    /// the reason [`PLAIN_SHADERS`] keeps its shaders: a kept run keeps its
    /// blobs for a paint only while the next draw's compares equal to it.
    static PLAIN_WHERE_OPAQUE_KEPT:
        RefCell<KeptSets<[u32; 6], Option<ColorFilter>, DYE_SETS, DYE_WAYS>> =
        const { RefCell::new(KeptSets::new()) };
}

/// The SkSL of [`plain_where_opaque`]: `plain` where the colour handed in
/// is opaque, and the premultiplied `product` at the colour's alpha
/// otherwise, premultiplied in and out as a runtime colour filter's colour
/// is. Skia asks a paint's colour filter for the colour a glyph mask's
/// contrast is set from with its colour shader's colour made opaque --
/// `SkShaderBase::asLuminanceColor` from `SkPaintPriv::ComputeLuminanceColor`
/// -- and draws the shader's colour through it times the paint's alpha --
/// `SkColorFilterShader`.
///
/// The product is handed over on its bytes rather than as a colour to
/// multiply by the alpha here: the GPU carries a shader's output unrounded,
/// so a colour placed at the edge of the span storing the product -- grey
/// 127.09 at 77 levels, 38.375 -- stood 0.375 of a level off the byte 38 a
/// fill stores, and partial coverage of it crossed a rounding boundary on
/// 632 of 8,484 pixels of a 44px Oswald run.
const PLAIN_WHERE_OPAQUE_SKSL: &str = "uniform float3 plain; \
    uniform float3 product; \
    float4 main(float4 c) { \
        return c.a == 1 ? float4(plain, 1) : float4(product, c.a); }";

/// A colour filter painting `product`, premultiplied, for a translucent
/// draw over a colour shader, and answering `plain` when Skia asks it for
/// the colour a glyph mask's contrast is set from -- see
/// [`PLAIN_WHERE_OPAQUE_SKSL`]. Kept per thread, as [`plain_colour_shader`]
/// keeps its shaders.
fn plain_where_opaque(
    plain: [f32; 3],
    product: [f32; 3],
) -> Option<ColorFilter> {
    let key = [
        plain[0], plain[1], plain[2], product[0], product[1], product[2],
    ]
    .map(f32::to_bits);
    let hash = key
        .iter()
        .enumerate()
        .fold(0u64, |hash, (i, bits)| {
            hash ^ u64::from(*bits).rotate_left(11 * i as u32)
        })
        .wrapping_mul(GOLDEN_RATIO_64);
    if let Some(filter) = PLAIN_WHERE_OPAQUE_KEPT
        .with_borrow(|kept| kept.get(hash, &key).cloned())
    {
        return filter;
    }
    let uniforms: Vec<u8> = plain
        .into_iter()
        .chain(product)
        .flat_map(f32::to_ne_bytes)
        .collect();
    let filter = PLAIN_WHERE_OPAQUE.with(|effect| {
        let effect = effect.as_ref()?;
        (effect.uniform_size() == uniforms.len()).then_some(())?;
        effect.make_color_filter(Data::new_copy(&uniforms), None)
    });
    PLAIN_WHERE_OPAQUE_KEPT
        .with_borrow_mut(|kept| kept.file(hash, key, filter.clone()));
    filter
}

impl ExactColour {
    /// `rgb`, a colour's exact components in `space`, for a page in that
    /// space -- `pixels`, as the one of this crate's spaces it is -- and
    /// `bytes` where it composites in eight bits a channel.
    pub fn new(
        rgb: [f64; 3],
        space: ColorSpace,
        pixels: PixelColorSpace,
        bytes: bool,
    ) -> Self {
        Self {
            rgb: rgb.map(|c| c as f32),
            space,
            pixels,
            bytes,
        }
    }

    /// The colour painted at `alpha`: the colour to paint and the alpha to
    /// paint it at, which a draw hands [`Self::shader`] or [`Self::dye`] and
    /// its paint.
    ///
    /// A float page takes both as they are. On an eight-bit page both are
    /// placed on the bytes the exact product rounds to: the alpha times 255,
    /// and each component times the alpha times 255, plus a half and
    /// truncated in `f32`, as Skia rounds a paint's own colour
    /// (`SkRasterPipeline::append_constant_color`). The colour handed over is
    /// that byte over the alpha's, so that the shader's product lands on it
    /// whole and its store -- which rounds to the nearest byte with ties to
    /// even on aarch64 and x86-64 (`round` in SkRasterPipeline_opts.h) -- has
    /// nothing to round. Unplaced, a tie went to the even byte:
    /// `color(display-p3 0.00196 0.00196 0.00196)`, half a level, filled 0
    /// where the same grey fills 1 on an sRGB page.
    ///
    /// The exact product on every eight-bit page, sRGB's included, rather
    /// than the bytes Skia's legacy blit stores there: it rounds the colour
    /// and the alpha to bytes first and premultiplies those, so
    /// `color(srgb 0.8732 0.5865 0.0787 / 0.1)` stored red 23 where the
    /// product is 22.27. The raster pipeline premultiplies in float and
    /// rounds once, and so does Chrome's Display P3 canvas:
    /// `color(a98-rgb 0.3 0.6 0.9 / 0.5)` reads back red 50 in Chrome 153 and
    /// here, where the legacy blit's rule reads 52.
    pub fn at(&self, alpha: f32) -> (Self, f32) {
        if !self.bytes {
            return (self.clone(), alpha);
        }
        let levels = f32::from(u8::MAX);
        let byte = |v: f32| (v.clamp(0.0, 1.0) * levels + 0.5).trunc();
        let stored = byte(alpha);
        if stored == 0.0 {
            return (self.clone(), 0.0);
        }
        let rgb = self.rgb.map(|c| byte(c.clamp(0.0, 1.0) * alpha) / stored);
        let placed = Self {
            rgb,
            ..self.clone()
        };
        (placed, byte_alpha(alpha))
    }

    /// `paint` in this colour, placed at its alpha -- see [`Self::at`] -- as
    /// a plain paint colour, where Skia lands it on the placed bytes on
    /// either engine; `None` where it does not, for [`Self::shader`] to
    /// draw. Drawn as a paint's own colour, a flat colour takes the blit a
    /// paint's colour takes -- 8-bit on the CPU, a uniform on the GPU --
    /// which a shader does not.
    ///
    /// Skia converts a paint's colour into the page's space through an
    /// approximation, and whether that leaves it on its byte depends on all
    /// three channels: a dark channel beside bright ones moved up to three
    /// levels on a Rec. 2020 page, and `color(a98-rgb 0 1 0)` one on a
    /// Display P3 page. So each placed colour is drawn once, as a paint's
    /// colour, onto a pixel of the page's space -- see [`landed`] -- and
    /// taken only where it lands; the verdict is kept per thread. An sRGB
    /// page converts nothing, and takes the placed colour without the draw
    /// -- see [`unconverted`].
    pub fn verified_paint(&self, paint: &Paint) -> Option<Paint> {
        let (colour, space) = self.verified_colour(paint.alpha_f())?;
        let mut paint = paint.clone();
        paint.set_color4f(colour, &space);
        Some(paint)
    }

    /// The colour [`Self::verified_paint`] sets on a paint at `alpha`, and
    /// the space it is set in, or `None` where it does not land.
    pub fn verified_colour(&self, alpha: f32) -> Verdict {
        let (placed, alpha) = self.at(alpha);
        let levels = f64::from(u8::MAX);
        let stored = (f64::from(alpha) * levels).round();
        let byte = |c: f32| (f64::from(c) * stored).round() as u8;
        let key = (
            self.pixels,
            [
                byte(placed.rgb[0]),
                byte(placed.rgb[1]),
                byte(placed.rgb[2]),
                stored as u8,
            ],
        );
        let (colour, space) = match self.pixels {
            PixelColorSpace::Srgb => unconverted(key.1),
            _ => VERDICTS.with(|verdicts| {
                let mut verdicts = verdicts.borrow_mut();
                let slot = &mut verdicts[verdict_slot(key)];
                if let Some((filed, verdict)) = slot
                    && *filed == key
                {
                    return verdict.clone();
                }
                let verdict = landed(&self.space, self.pixels, key.1);
                *slot = Some((key, verdict.clone()));
                verdict
            }),
        }?;
        Some((colour, space))
    }

    /// [`Self::verified_paint`] for text in `paint`, where the placed colour
    /// sets the glyph mask's contrast from the same bucket as `paint`'s own
    /// colour -- see [`contrast_bucket`] -- and the paint has no colour
    /// filter of its own; `None` otherwise, for [`Self::dye`] to draw. The
    /// placed colour moved the bucket for 13 of 1110 colours opaque, and 47
    /// at half alpha, on a Display P3 page.
    pub fn verified_text_paint(&self, paint: &Paint) -> Option<Paint> {
        if paint.color_filter().is_some() {
            return None;
        }
        let placed = self.verified_paint(paint)?;
        let bucket = |p: &Paint| {
            let c = p.color4f();
            contrast_bucket([c.r, c.g, c.b])
        };
        (bucket(&placed) == bucket(paint)).then_some(placed)
    }

    /// [`Self::verified_text_paint`] on an eight-bit sRGB page, where the
    /// plain colour's glyph contrast cannot be kept apart from the colour
    /// drawn: Skia hands [`Self::keeping_plain_luminance`]'s probe over
    /// unconverted onto sRGB, so that step would paint the plain colour.
    /// Each channel is placed instead on the colour nearest the plain one
    /// of those that store the exact product on either engine -- see
    /// [`nearest_storing`] -- which keeps the plain colour's contrast
    /// bucket wherever one of them is in it. Over 65,025 greys and alphas
    /// off the byte grid, `(c + 0.37) / 255` at `(a + 0.61) / 255`,
    /// [`Self::at`]'s colour left the bucket for 2,710 and this one for
    /// 637, 456 of them at an alpha under 32 levels.
    ///
    /// Where it leaves the bucket, the paint is a colour shader of the plain
    /// colour, opaque, at the paint's alpha, through [`plain_where_opaque`]:
    /// Skia asks the filter for the contrast colour with the shader's colour
    /// opaque, and draws the shader's colour through it at the alpha, so the
    /// glyph takes the plain colour's contrast and stores the product's
    /// bytes. The filter tells the two apart by alpha, not by whether Skia
    /// converts a uniform as [`Self::keeping_plain_luminance`]'s does:
    /// onto sRGB Skia leaves the uniform unconverted for the draw as well,
    /// and that filter would paint the plain colour. On 44px Oswald in grey
    /// 128 at 0.3, a GPU page stores 48 of 8,484 covered pixels a level off
    /// the exact product through the filter, 47 of them in alpha, which the
    /// GPU carries in half precision -- see [`GPU_HALF_ROUNDING_LEVELS`];
    /// the placed colour as the paint's own stored 2,822 off, by up to 8
    /// levels.
    ///
    /// Elsewhere the colour is handed over as it is, as
    /// [`Self::verified_paint`] hands its own over on such a page: Skia
    /// lands an sRGB paint colour on an sRGB page unconverted -- see
    /// [`unconverted`]. `None` on any other page, and for a paint with a
    /// colour filter of its own.
    pub fn nearest_text_paint(&self, paint: &Paint) -> Option<Paint> {
        if !self.bytes
            || self.pixels != PixelColorSpace::Srgb
            || paint.color_filter().is_some()
        {
            return None;
        }
        let (placed, alpha) = self.at(paint.alpha_f());
        let levels = f64::from(u8::MAX);
        let stored = (f64::from(alpha) * levels).round();
        if stored == 0.0 {
            return self.verified_paint(paint);
        }
        let products =
            [0, 1, 2].map(|i| (f64::from(placed.rgb[i]) * stored).round());
        let rgb = [0, 1, 2].map(|i| {
            let plain = f64::from(self.rgb[i]) * levels;
            (nearest_storing(plain, products[i], stored) / levels) as f32
        });
        let mut paint = paint.clone();
        let named = paint.color4f();
        let plain = [named.r, named.g, named.b];
        // Where none of the colours storing the product is in the plain
        // colour's bucket, the run is drawn over a colour shader of the
        // plain colour, through a filter that paints the product and
        // answers the plain colour when Skia asks for the glyph contrast.
        if stored < levels && contrast_bucket(rgb) != contrast_bucket(plain) {
            let product = products.map(|p| (p / levels) as f32);
            let filter = plain_where_opaque(plain, product)?;
            paint
                .set_shader(plain_colour_shader(Color4f::new(
                    named.r, named.g, named.b, 1.0,
                )))
                .set_color_filter(filter)
                .set_alpha_f(alpha);
            return Some(paint);
        }
        let [r, g, b] = rgb;
        paint.set_color4f(Color4f::new(r, g, b, alpha), &self.space);
        Some(paint)
    }

    /// A shader painting the colour: [`FLAT_COLOUR`] with the colour as its
    /// uniform at full float precision, worked in the page's space. Drawn
    /// onto a page in that space, Skia converts its output by the
    /// identity, so it lands exactly, and onto any other through the colour
    /// space transform; a paint's alpha scales it, the one [`Self::at`]
    /// gives beside it.
    pub fn shader(&self) -> Option<Shader> {
        let [r, g, b] = self.rgb;
        let uniforms: Vec<u8> = [r, g, b, 1.0]
            .into_iter()
            .flat_map(f32::to_ne_bytes)
            .collect();
        let flat = FLAT_COLOUR.with(|effect| {
            effect
                .as_ref()?
                .make_shader(Data::new_copy(&uniforms), &[], None)
        })?;
        Some(flat.with_working_color_space(self.space.clone(), None))
    }

    /// A colour filter replacing a draw's colour with this one, its alpha
    /// kept, for text: a colour matrix worked in the page's space, so on a
    /// surface in that space Skia converts its output by the identity, and
    /// onto any other through the colour space transform. Its weights are
    /// zero and its offset the colour, so `matrix_4x5` sums to the colour
    /// exactly, and Skia premultiplies it; unclamped, so a colour outside
    /// the page's gamut stays where it is.
    ///
    /// Where Skia would read the glyph contrast from it in another bucket
    /// from `plain`'s, it ends in [`Self::keeping_plain_luminance`] -- see
    /// [`luminance_colour`] for how that is read. The dye depends on nothing
    /// but this colour and `plain`, so each pair is read once a thread and
    /// kept in [`DYES`].
    pub fn dye(&self, plain: Color4f) -> Option<ColorFilter> {
        let key = (
            self.clone(),
            [plain.r, plain.g, plain.b, plain.a].map(f32::to_bits),
        );
        let hash = dye_hash(&key);
        #[cfg(test)]
        let keeps = KEEPS_DYES.get();
        #[cfg(not(test))]
        let keeps = true;
        if keeps
            && let Some(dye) =
                DYES.with_borrow(|dyes| dyes.get(hash, &key).cloned())
        {
            return dye;
        }
        #[cfg(test)]
        DYES_READ.set(DYES_READ.get() + 1);
        let dye = self.read_dye(plain);
        DYES.with_borrow_mut(|dyes| dyes.file(hash, key, dye.clone()));
        dye
    }

    /// [`Self::dye`], read rather than found kept.
    fn read_dye(&self, plain: Color4f) -> Option<ColorFilter> {
        let replacement = self.replacement()?;
        match luminance_colour(&replacement, plain) {
            Some(lands)
                if contrast_bucket(lands)
                    == contrast_bucket([plain.r, plain.g, plain.b]) =>
            {
                Some(replacement)
            }
            _ => self.keeping_plain_luminance(replacement, plain),
        }
    }

    /// [`Self::dye`] with the plain colour's glyph contrast whatever bucket
    /// the replacement's would fall in, for a paint whose own colour filter
    /// follows it and could move either.
    pub fn dye_keeping_plain_luminance(
        &self,
        plain: Color4f,
    ) -> Option<ColorFilter> {
        self.keeping_plain_luminance(self.replacement()?, plain)
    }

    /// `filtered`, the dye followed by a colour filter built for a page's
    /// pixels, with the glyph contrast Skia sets for `plain` through
    /// `plain_filter`, the filter as a document draws it: the filter built
    /// for the pixels holds its colour in the page's space, which Skia reads
    /// as sRGB where it sets the contrast -- see [`luminance_colour`] -- and
    /// a gray of 0.05 named in sRGB, replaced by a `copy` blend filter of
    /// itself on a Rec. 2020 page, took the next bucket, its glyph edges up
    /// to 24 levels from the plain text's.
    pub fn keeping_luminance_of(
        &self,
        filtered: ColorFilter,
        plain_filter: &ColorFilter,
        plain: Color4f,
    ) -> Option<ColorFilter> {
        let [r, g, b] = luminance_colour(plain_filter, plain)?;
        self.keeping_plain_luminance(filtered, Color4f::new(r, g, b, 1.0))
    }

    /// The colour as a colour filter: see [`Self::dye`].
    fn replacement(&self) -> Option<ColorFilter> {
        let [r, g, b] = self.rgb;
        #[rustfmt::skip]
        let replace = [
            0.0, 0.0, 0.0, 0.0, r,
            0.0, 0.0, 0.0, 0.0, g,
            0.0, 0.0, 0.0, 0.0, b,
            0.0, 0.0, 0.0, 1.0, 0.0,
        ];
        color_filters::matrix_row_major(&replace, color_filters::Clamp::No)
            .with_working_color_space(&self.space)
    }

    /// `replacement` followed by a step painting `plain` wherever Skia
    /// leaves a `layout(color)` uniform unconverted -- onto sRGB, and where
    /// there is no surface at all -- and passing `replacement` through
    /// everywhere else. Skia sets a glyph mask's contrast from the colour
    /// the paint's colour filter makes, asked for with no space and read as
    /// sRGB, and the page-space colour converted back to sRGB by Skia lands
    /// off `plain` -- 8.1e-4 off a green of 0.1235 named in sRGB on a
    /// Display P3 page -- which puts a colour named near the edge of a
    /// contrast bucket in the next one, and its text's edges on a different
    /// curve from the plain paint's. On an sRGB surface `plain` is also
    /// what the plain paint paints.
    fn keeping_plain_luminance(
        &self,
        replacement: ColorFilter,
        plain: Color4f,
    ) -> Option<ColorFilter> {
        let uniforms: Vec<u8> = [
            UNCONVERTED_PROBE,
            UNCONVERTED_PROBE,
            [plain.r, plain.g, plain.b],
        ]
        .iter()
        .flatten()
        .flat_map(|c| c.to_le_bytes())
        .collect();
        let unconverted = PLAIN_UNCONVERTED.with(|effect| {
            let effect = effect.as_ref()?;
            // The three `float3`s are packed, as they are written above.
            (effect.uniform_size() == uniforms.len()).then_some(())?;
            effect.make_color_filter(Data::new_copy(&uniforms), None)
        })?;
        // UPSTREAM: skia-safe 0.156.0 -- unfiled -- worked around
        // Re-check: cargo test a_float_page_covers_text_as_srgb_does with
        // this returning `Some(replacement)`.
        //
        // `just_a_color` (SkPaintPriv.cpp) runs the paint's colour filter
        // with no colour space, under a TODO calling that space
        // meaningless, so the glyph contrast of a filter worked in another
        // space is set from its output converted back to sRGB.
        color_filters::compose(&unconverted, &replacement)
    }
}

/// The largest tile, in device pixels, a [`TileSource`] rasterizes a page
/// into: the bound Skia's picture shader puts on the tile it rasterizes a
/// pattern's picture into, `kMaxTileArea` in `SkPictureShader.cpp`, so that
/// a pattern made from a page asks for no larger a raster than one made
/// from its picture does.
const TILE_AREA_MAX: f32 = 2048.0 * 2048.0;

/// How far past a whole number a scale may lie and still be taken as that
/// number. A density composed with the transforms a pattern is drawn under
/// reaches [`TileSource::density_for`] a few units in the last place off
/// the whole number it stands for, and rounding that up would rasterize the
/// page at the next density and resample it down.
const SCALE_SLACK: f32 = 1.0 / 1024.0;

/// A canvas's page as a pattern made from it paints it: rasterized as the
/// page composites itself, at a whole density, on the device the pattern is
/// drawn on and in the page's own colour type -- as a canvas drawn into
/// another plays its page there. On the CPU the raster is kept for the
/// density asked for last; on the GPU it is a texture of the context
/// drawing it, kept for the rest of the playback that made it and gone with
/// it -- see [`TileScope`] -- so that nothing here holds a texture past the
/// playback.
///
/// Not a picture shader of the page's picture, which Skia rasterizes once
/// at the scale it is first drawn at and always as RGBA8888: a blur the
/// page makes at playback comes out of it made at that scale and
/// resampled, and on a platform whose native colour type is BGRA8888 a
/// translucent edge blits a level apart from the page's -- see
/// `a_picture_shader_tile_blits_as_rgba`.
///
/// What it holds is held by every page a draw dyed by it is filed into, and
/// counted there: the images its page holds and the one it paints a
/// document with -- see [`Self::held_images`] -- and the raster it makes,
/// which a page counts once however many of its draws the pattern dyes --
/// see [`Self::count_raster_into`] and [`Self::budget_bytes`]. The raster is
/// let go of once no page holds the pattern, and made again if it is drawn
/// again -- see [`Self::release_unheld`].
#[derive(Debug)]
pub struct TileSource {
    page: Page,
    /// What the page is rasterized with: its own colour space, format and
    /// glyph settings. The density is set for each raster.
    options: ExportOptions,
    /// The images the pattern holds that no picture counts, each once: the
    /// one it paints a document with besides the page, where it holds one
    /// -- a nesting canvas's own pixels, see `CanvasPattern::of_page` --
    /// and those the page holds. Collected as the pattern is made, since the
    /// page it was taken as never changes, so that filing a draw dyed by it
    /// does not walk the page again.
    held: Box<[HeldImage]>,
    /// The raster made last on the CPU, and the density it was made at;
    /// `None` before the first and once no page holds the pattern.
    raster: Mutex<Option<(u32, Image)>>,
    /// Each page's counts the pattern was filed into, once each, held
    /// weakly: of rasters, which a raster made or replaced moves by its
    /// bytes, and of the pages they take toward the page's budget, which
    /// the first raster made adds to -- see [`Self::count_raster_into`].
    counted_by: Mutex<Vec<(WeakCount, WeakCount)>>,
}

impl TileSource {
    /// `page`, rasterized with `options` at whatever density a draw asks,
    /// beside `image`, which the pattern holds besides the page.
    pub(crate) fn new(
        page: Page,
        options: ExportOptions,
        image: Option<HeldImage>,
    ) -> Self {
        let mut held: Vec<HeldImage> = image.into_iter().collect();
        page.layers.iter().for_each(|layer| {
            layer.held_images(&mut |image| {
                if !held.iter().any(|seen| seen.id == image.id) {
                    held.push(image);
                }
            })
        });
        Self {
            page,
            options,
            held: held.into(),
            raster: Mutex::new(None),
            counted_by: Mutex::default(),
        }
    }

    /// Calls `each` with every image the pattern holds that no picture
    /// counts, each once: the one it paints a document with, and those its
    /// page holds -- see [`Layer::held_images`] -- which the canvas it was
    /// made from stops counting once it moves on.
    pub(crate) fn held_images(&self, each: &mut dyn FnMut(HeldImage)) {
        self.held.iter().for_each(|image| each(*image));
    }

    /// The bytes of the raster the pattern makes on the CPU at density 1.
    fn page_bytes(&self) -> usize {
        let dims = self.page.scaled_dimensions(1.0);
        self.options
            .compositing_info(dims, &self.options.surface_color_space)
            .compute_min_byte_size()
    }

    /// What the pattern's raster takes toward the budget of a page holding
    /// it, which is in pages of its own pixels at density 1 -- see
    /// `RECORDED_IMAGE_PAGES`: the raster's bytes at density 1 once one is
    /// made on the CPU, whatever density it was made at, so that a page
    /// read at density 3 is not let go of for the nine pages a raster made
    /// there takes; nothing before, and nothing for a pattern drawn only on
    /// the GPU, whose rasters last one playback -- see [`TileScope`].
    pub(crate) fn budget_bytes(&self) -> usize {
        match self.raster.lock().is_some() {
            true => self.page_bytes(),
            false => 0,
        }
    }

    /// The bytes of the raster the pattern holds, 0 before it makes one.
    pub(crate) fn raster_bytes(&self) -> usize {
        self.raster
            .lock()
            .as_ref()
            .and_then(|(_, image)| HeldImage::of(image))
            .map_or(0, |held| held.bytes)
    }

    /// Has a page hold the pattern's raster: `rasters` its bytes now and
    /// the change each raster made after brings, and `budget` what it takes
    /// toward the page's budget -- see [`Self::budget_bytes`] -- now, or
    /// once the first is made. Once for each page, however many times it
    /// is asked with its counts, since a page holds one raster of the
    /// pattern however many of its draws it dyes. Whether the page holds it
    /// now and did not before.
    pub(crate) fn count_raster_into(
        &self,
        rasters: &RecordedCount,
        budget: &RecordedCount,
    ) -> bool {
        // The raster's lock first, as `Self::raster` takes the two, so that
        // no raster is made between the bytes added and the counts held.
        let raster = self.raster.lock();
        let mut counts = self.counted_by.lock();
        counts.retain(|(held, _)| !held.is_gone());
        if counts.iter().any(|(held, _)| held.holds(rasters)) {
            return false;
        }
        counts.push((rasters.weak(), budget.weak()));
        if let Some((_, image)) = raster.as_ref() {
            rasters.add(HeldImage::of(image).map_or(0, |held| held.bytes));
            budget.add(self.page_bytes());
        }
        true
    }

    /// Lets go of the raster where no page holds the pattern any longer --
    /// none of the counts it was filed into is left -- so that a pattern
    /// waiting to be collected holds no page of pixels that nothing draws.
    /// The next draw makes it again, the same bytes, since the page it is
    /// made from never changes. Called by a page as it lets the pattern go
    /// -- see `PageRecorder::set_bounds` -- and a no-op while another page
    /// holds it.
    pub(crate) fn release_unheld(&self) {
        // The raster's lock first, as `Self::raster` takes the two.
        let mut raster = self.raster.lock();
        let mut counts = self.counted_by.lock();
        counts.retain(|(held, _)| !held.is_gone());
        if counts.is_empty() {
            *raster = None;
        }
    }

    /// The identity of the raster the pattern holds, if it holds one.
    #[cfg(test)]
    pub(crate) fn raster_id(&self) -> Option<u32> {
        self.raster
            .lock()
            .as_ref()
            .map(|(_, image)| image.unique_id())
    }

    /// The page rasterized at `density` for a pattern drawn on `canvas`.
    fn raster(&self, canvas: &SkCanvas, density: u32) -> Option<Image> {
        // UPSTREAM: skia-safe 0.156.0 -- unfiled -- worked around
        // Re-check: cargo test a_picture_shader_tile_blits_as_rgba
        // Rasterized here, in the page's own colour type, rather than
        // through Skia's picture shader, whose tile is RGBA8888 whatever
        // the page -- see the type's note.
        if blur_mask::on_gpu(canvas) {
            let key = (self as *const Self as usize, density);
            if let Some(image) = TileScope::held(key) {
                return Some(image);
            }
            let dims = self.page.scaled_dimensions(density as f32);
            let info = self
                .options
                .compositing_info(dims, &self.options.surface_color_space);
            let props = self.options.surface_props();
            let mut surface = canvas.new_surface(&info, Some(&props))?;
            let tile = surface.canvas();
            tile.set_matrix(
                &Matrix::scale((density as f32, density as f32)).into(),
            );
            // The tile's surface is a root of its own, whose pixels its
            // entries read back, whatever layer the pattern is drawn in.
            let _root = OwnRoot::enter();
            for entry in self.page.layers.iter() {
                entry.playback(tile, Target::Pixels);
            }
            let image = surface.image_snapshot();
            TileScope::hold(key, &image);
            return Some(image);
        }
        let mut held = self.raster.lock();
        if let Some((at, image)) = held.as_ref()
            && *at == density
        {
            return Some(image.clone());
        }
        let options = ExportOptions {
            density: density as f32,
            ..self.options.clone()
        };
        let image =
            self.page.rasterized(&options, RenderingEngine::CPU).ok()?;
        let bytes = |raster: Option<&Image>| {
            raster.and_then(HeldImage::of).map_or(0, |held| held.bytes)
        };
        let (first, was, now) = (
            held.is_none(),
            bytes(held.as_ref().map(|(_, image)| image)),
            bytes(Some(&image)),
        );
        *held = Some((density, image.clone()));
        let page = self.page_bytes();
        self.counted_by.lock().iter().for_each(|(rasters, budget)| {
            rasters.adjust(was, now);
            if first {
                budget.add(page);
            }
        });
        Some(image)
    }

    /// The density a tile is rasterized at for a pattern drawn through
    /// `to_device`, the transform from the page's space to the device: the
    /// larger of the two scales it maps by, rounded up to a whole number
    /// unless it lies within [`SCALE_SLACK`] of one, and lowered until the
    /// tile fits [`TILE_AREA_MAX`]; never below 1. A whole density is what
    /// the page draws itself at, so a pattern drawn at it lies over the
    /// page's own pixels exactly.
    fn density_for(&self, to_device: &Matrix) -> u32 {
        let scale = to_device
            .decompose_scale(None)
            .map_or(1.0, |scale| scale.width.max(scale.height));
        let near = scale.round();
        let wanted = match (scale - near).abs() <= SCALE_SLACK {
            true => near,
            false => scale.ceil(),
        };
        let size = self.page.bounds.size();
        let fits = (TILE_AREA_MAX / (size.width * size.height)).sqrt().floor();
        wanted.min(fits).max(1.0) as u32
    }

    /// Whether a pattern of this page, mapped onto the device by
    /// `to_device` and repeated as `repeat` says, lands each tile on whole
    /// device pixels: repeated on both axes, a page whole pixels across,
    /// and a transform that moves by whole pixels and scales both axes by
    /// the density the tile is rasterized at -- see [`Self::density_for`],
    /// which is whole, and lowered to fit [`TILE_AREA_MAX`]. A renderer
    /// drawing it as PDF tiling then has the tile's pixels on the device's
    /// and nothing to resample. A tile under a rotation, a skew, a
    /// fractional scale or step, or repeated on one axis alone does not,
    /// nor one too large to rasterize at its scale, and is embedded as the
    /// page composites it.
    ///
    /// Asked of the transform a draw is made under, and again of the one it
    /// is played under in a document -- see [`tiles_exactly`].
    pub(crate) fn lands_whole(
        &self,
        to_device: &Matrix,
        repeat: (TileMode, TileMode),
    ) -> bool {
        let whole = |v: f32| v.fract() == 0.0;
        let size = self.page.bounds.size();
        let scale = to_device.scale_x();
        repeat == (TileMode::Repeat, TileMode::Repeat)
            && whole(size.width)
            && whole(size.height)
            && to_device.is_scale_translate()
            && to_device.scale_y() == scale
            && whole(to_device.translate_x())
            && whole(to_device.translate_y())
            && self.density_for(to_device) as f32 == scale
    }
}

/// A draw dyed by a canvas pattern, the pattern made where the draw is
/// played: from the canvas's page rasterized as the page composites itself,
/// at the scale it is played at -- see [`TileSource`] -- rather than from a
/// picture of it Skia rasterizes as RGBA8888 at the scale the picture was
/// recorded at.
///
/// On pixels, and so in the raster a document embeds, the draw is kept
/// whole -- see [`Replay`] -- and drawn again with the pattern as its
/// shader, through its own alpha, blend mode and colour filter, as a draw
/// dyed by a pattern of the canvas's pixels is. In a document, an
/// [`aligned`](Self::aligned) draw is drawn the same way from the tile in
/// eight-bit sRGB, which a PDF writes as tiling; both document backends
/// embed any other -- see `VectorFeatures::CANVAS_PATTERN` -- so a document
/// plays it as recorded, with the page's picture as its pattern, only where
/// it was handed it as vector.
#[derive(Clone, Debug)]
pub struct PatternDraw {
    pub source: Arc<TileSource>,
    /// The transform the draw was made under.
    pub matrix: Matrix,
    /// The pattern's own transform, from the page's space to the draw's.
    pub pattern_matrix: Matrix,
    pub repeat: (TileMode, TileMode),
    /// The draw as a document writes it, under the transform it was made
    /// under; its bounds are what the draw reaches.
    pub plain: Picture,
    /// What the pattern is painted through: the draw's alpha, blend mode
    /// and colour filter.
    pub paint: Paint,
    /// The clip the draw was made under, in device pixels of the recording,
    /// where it is not a piece of a run that applies it -- see [`Clipped`].
    pub clip: Option<Data>,
    /// The draw kept whole, to draw on pixels with the pattern as its
    /// shader, as a draw dyed by a pattern of the canvas's pixels is.
    pub replay: Replay,
    /// Whether the draw's shadow was filed just before it, as a group of
    /// its own, which a GPU playback reading the page back for the draw
    /// draws on the CPU with it -- see [`replayed_on_their_backdrop`].
    pub cast_before: bool,
    /// Whether a document paints the draw from the tile as pixels do: the
    /// pattern lands on whole device pixels under the transform it was made
    /// under -- see [`TileSource::lands_whole`] -- and the draw is
    /// composited only where it lands, through no filter, colour filter or
    /// shadow, so that what a document writes of it is the draw's shape, its
    /// alpha and blend mode, and the tile.
    /// `VectorFeatures::CANVAS_PATTERN_ALIGNED` where it is; a document
    /// embeds it all the same where, played there, its tiles do not land
    /// whole -- see [`tiles_exactly`].
    pub aligned: bool,
}

/// A draw kept whole, so that the pattern a [`PatternDraw`] makes at
/// playback is drawn through it as any shader is: a picture of the draw
/// holds the shader it was recorded with, and the pattern's is not made
/// until the scale the draw is played at is known.
#[derive(Clone, Debug)]
pub enum Replay {
    /// A path filled or stroked as the paint says, onto pixels that do with
    /// a faint paint what the [`Faint`] says; serialized, as a group's clip
    /// is, so that a page holding it can be sent to another thread.
    Path(Data, Faint),
    /// A run of text, as `Context2D::draw_text` paints it: what it draws,
    /// with its anchor at `at`, painted `offset` from it and squeezed by
    /// `condensation` about it, its named line colour's alpha multiplied
    /// by `global_alpha`, onto pixels that do with a faint paint what
    /// `faint` says.
    Text {
        run: Arc<RunPaint>,
        at: Point,
        offset: Point,
        condensation: f32,
        global_alpha: f32,
        faint: Faint,
    },
}

impl Replay {
    /// The draw at full alpha: a run's named line colour no longer
    /// multiplied by a `globalAlpha`, which a path's replay does not hold.
    pub fn opaque(&self) -> Self {
        let mut replay = self.clone();
        if let Replay::Text { global_alpha, .. } = &mut replay {
            *global_alpha = 1.0;
        }
        replay
    }

    /// Draws the draw onto `canvas` in `paint`, under `matrix`, the
    /// transform it was made under, concatenated onto the one `canvas`
    /// carries. Leaves `canvas` under that transform.
    fn draw(&self, canvas: &SkCanvas, matrix: &Matrix, paint: &Paint) {
        match self {
            Replay::Path(path, faint) => {
                let Some(path) = Path::deserialize(path) else {
                    return;
                };
                canvas.concat(matrix);
                canvas.draw_path(&path, &faint.drawable(paint));
            }
            Replay::Text {
                run,
                at,
                offset,
                condensation,
                global_alpha,
                faint,
            } => {
                canvas.concat(matrix);
                // Squeezed as `draw_text` squeezes it: by a scale about the
                // anchor, the pen with it.
                if *condensation == 1.0 {
                    run.paint(
                        canvas,
                        *at + *offset,
                        paint,
                        *global_alpha,
                        *faint,
                    );
                } else {
                    canvas.translate(*at).scale((*condensation, 1.0));
                    run.paint(canvas, *offset, paint, *global_alpha, *faint);
                }
            }
        }
    }
}

/// A run of text holding colour glyphs this library draws itself -- see
/// `font::colr::ColourRun` -- drawn again where it is played back, so that
/// its colour glyphs are made for the pixels they land on.
///
/// On pixels each colour glyph is drawn as an image made on the CPU under
/// the transform the run is played back under, in the format of those
/// pixels -- see [`ImageRoute`] -- as Skia draws a colour glyph from the
/// image its scaler context makes. A GPU canvas then composites the pixels
/// the CPU canvas does: composited through a layer of its own on the GPU,
/// a glyph's gradients were drawn by Ganesh, which places a conic
/// gradient's hard stops apart from the CPU's (#667). Made at playback, the
/// image is made at the scale an export's density, a canvas drawn into
/// another or a pattern plays the run at, rather than resampled from the
/// scale it was recorded at.
///
/// On a build that keys their blobs -- see
/// `typography::KEYS_OWN_COLOUR_BLOBS` -- a run holding glyphs the platform
/// draws in its own colours is kept here too, and drawn where it is played
/// from blobs Ganesh keys by the matrix they are drawn under, so that a GPU
/// page read at one scale after another draws from the masks made for it.
///
/// The run is drawn with the shadow it casts and through the blend that
/// covers beyond it, as `Context2D::render_to_canvas` records them, so that
/// every colour glyph of it reaches pixels as an image. A draw that cannot
/// be kept whole, a paragraph's, keeps its recording only, rasterized on
/// the CPU where it is played -- see [`Self::rasterized`].
///
/// A document takes the run as recorded, each colour glyph composited
/// through its layer from its paint graph, which a vector backend writes as
/// such; so do pixels played into a recording, which does not know the
/// pixels it will be played onto.
#[derive(Clone, Debug)]
pub struct ReplayedText {
    /// The transform the run was drawn under.
    pub matrix: Matrix,
    /// The run kept whole, to draw on pixels; `None` for a draw that cannot
    /// be kept, a paragraph's, which pixels take as recorded, rasterized on
    /// the CPU -- see [`Self::rasterized`].
    pub replay: Option<Replay>,
    /// What pixels draw the run in.
    pub paint: Paint,
    /// What the draw inks, under [`Self::matrix`]'s parent transform, where
    /// it has no replay: the raster it is rasterized into.
    pub inked: Rect,
    /// The shadow the run casts on pixels, drawn beneath it.
    pub shadow: Option<ReplayedShadow>,
    /// Whether the run is filed for its shadow alone, the run itself drawn
    /// by nothing here: the first of the two paintings
    /// `Context2D::draw_text` makes of a run whose shadow and image are
    /// composited apart -- see `Context2D::paints_run_whole`.
    pub shadow_alone: bool,
    /// The blend a layer holding the run and its shadow composites through,
    /// where the blend covers beyond the draw -- see
    /// `Context2D::render_to_canvas`.
    pub blend: Option<BlendMode>,
    /// The run as recorded for pixels, under [`Self::matrix`], once
    /// something has asked for it -- see [`Self::recorded`]; its bounds are
    /// what the run reaches.
    pub recording: LazyPicture,
    /// The run recorded whole for a document, on a page that keeps one
    /// recording, where its paint reads the page and the recording for
    /// pixels -- [`Self::recording`] -- draws its glyphs apart -- see
    /// [`Self::recorded_for_document`].
    pub whole: LazyPicture,
    /// The bounds of the page the run is recorded over.
    pub bounds: Rect,
    /// The run as recorded for a document, on a page that keeps a recording
    /// for each kind of target -- see `page::Recorders` -- where it differs.
    pub document: Option<ReplayedRecording>,
    /// The clip the run was drawn under, in device pixels of the recording,
    /// where it is not a piece of a run that applies it -- see [`Clipped`].
    pub clip: Option<Data>,
    /// Whether the run's shadow was filed just before it, as a group cast
    /// through a chain of its own -- see
    /// `Context2D::files_shadow_through_chain` -- which a GPU playback
    /// reading the page back for the run draws on the CPU with it -- see
    /// [`replayed_on_their_backdrop`].
    pub cast_before: bool,
    /// The run's one flat colour, where it is translucent, which an
    /// eight-bit page on the CPU paints from the run's coverage -- see
    /// [`FlatRun`] -- and which the run's recording, and the layer a blend
    /// of its own composites, draw through a colour shader -- see
    /// [`Self::flat_paint`].
    pub flat: Option<FlatRun>,
    /// Where the thread that filed it counts the recordings
    /// [`Self::recorded`] makes -- see [`REPLAYED_WATCH`].
    #[cfg(test)]
    pub watch: Option<Arc<std::sync::atomic::AtomicUsize>>,
}

/// A translucent run of text in one flat colour, kept so that an eight-bit
/// page on the CPU lands each pixel's alpha on the exact product of the
/// paint's alpha and the glyph's coverage, `round(a * c / 255)`, and each
/// premultiplied channel on the colour times that, rounded once, as a GPU
/// canvas does.
///
/// Skia's raster blit of a glyph mask onto eight bits a channel multiplies
/// the two as `a * (c + 1) >> 8` (Skia M156, `blit_mask_d32_a8_general` in
/// `src/opts/SkBlitMask_opts.h`, through `SkAlphaMulQ` and
/// `SkAlpha255To256`), a level under the exact product on about one covered
/// pixel in ten of a run at 0.3; Chrome's software canvas lands the same, and
/// its accelerated one the exact product -- see `docs/browser-differences.md`.
/// So on such a page the run is drawn opaque into a plane of the page's own
/// type, whose alpha is its coverage, coloured from it -- see
/// `chain::coverage::coloured` -- and composited through the run's blend and
/// the page's clip, as a blurred run is from its coverage -- see
/// [`CoverageRun`]: a group of its glyphs at a time where glyphs share a
/// pixel, so that the later is composited over the earlier, as the raster
/// composites a blob's glyphs -- see `typography::held_groups`.
///
/// The plane is drawn in [`Self::opaque`], the colour the run was named in,
/// and not the colour [`ReplayedText::paint`] places for the page's bytes:
/// Skia pre-blends a glyph mask by the paint's colour -- see
/// `chain::coverage::Plane` -- and the placed colour for grey at 0.3 is in
/// another table's bucket, which left 2,815 of 8,484 pixels of a 44px Oswald
/// run off the product Skia's blit makes of the named grey's coverage.
#[derive(Clone, Debug)]
pub struct FlatRun {
    /// The colour, held exactly in the page's space.
    pub exact: ExactColour,
    /// The alpha the run is drawn at, `globalAlpha` included.
    pub alpha: f32,
    /// The run's paint in the colour it was named in, opaque, through
    /// source-over: what the run's coverage is drawn in.
    pub opaque: Paint,
}

#[cfg(test)]
thread_local! {
    /// Where the runs filed on this thread count the recordings made
    /// of them when first asked for, on whichever thread makes them -- see
    /// [`ReplayedText::recorded`].
    pub(crate) static REPLAYED_WATCH: RefCell<
        Option<Arc<std::sync::atomic::AtomicUsize>>,
    > = const { RefCell::new(None) };
}

/// A [`ReplayedText`]'s recording for a document: the paints the run and its
/// shadow are recorded in, and the picture once something has asked for it
/// -- see [`ReplayedText::recorded`].
#[derive(Clone, Debug)]
pub struct ReplayedRecording {
    /// What the run is recorded in.
    pub paint: Paint,
    /// The shadow recorded beneath the run.
    pub shadow: Option<ReplayedShadow>,
    /// The picture, once recorded.
    pub picture: LazyPicture,
}

impl ReplayedRecording {
    /// `picture`, recorded already.
    pub(crate) fn recorded(picture: Picture) -> Self {
        Self {
            paint: Paint::default(),
            shadow: None,
            picture: LazyPicture::from(Some(picture)),
        }
    }

    /// The run in `paint`, `shadow` beneath it, recorded when first asked
    /// for.
    pub(crate) fn later(paint: Paint, shadow: Option<ReplayedShadow>) -> Self {
        Self {
            paint,
            shadow,
            picture: LazyPicture::default(),
        }
    }
}

/// The shadow a [`ReplayedText`] casts: the run in the shadow's paint, which
/// is the run's own at full alpha where the shadow's colour took the
/// `globalAlpha`, moved by the shadow's offset in the page's own units.
#[derive(Clone, Debug)]
pub struct ReplayedShadow {
    pub paint: Paint,
    pub replay: Replay,
    pub offset: Vector,
}

impl ReplayedShadow {
    /// Draws the shadow onto `canvas`, the run under `matrix`, the transform
    /// it was drawn under: cast once from the whole run, as a run of plain
    /// glyphs casts it. The run's pieces -- each colour glyph, and the plain
    /// glyphs between them -- are drawn `source-over` into one layer, which
    /// the shadow's filter casts from and its blend composites. Drawn piece
    /// by piece in the shadow's paint, each piece cast a shadow of its own,
    /// and where a colour glyph overlapped a plain one the two shadows
    /// composited one over the other, 36 levels darker than one.
    ///
    /// The layer is bounded by what the pieces ink on `canvas`, widened by
    /// [`HINTED_INK_DRIFT`] for the reason [`ReplayedText::ink_under`]
    /// gives: unbounded, a layer for a filter takes in the whole clip, every
    /// pixel of the page under a shadow a word wide.
    fn draw(&self, canvas: &SkCanvas, matrix: &Matrix) {
        let Some(clip) = canvas.device_clip_bounds() else {
            return;
        };
        let mut inside = self.paint.clone();
        inside
            .set_image_filter(None)
            .set_blend_mode(BlendMode::SrcOver);
        let mut cast = Paint::default();
        cast.set_anti_alias(true)
            .set_image_filter(self.paint.image_filter())
            .set_blend_mode(self.paint.blend_mode_or(BlendMode::SrcOver));
        canvas.save();
        canvas.translate(self.offset);
        let device = canvas.local_to_device_as_3x3();
        let inked = inked_by(Rect::from(clip), &device, |canvas| {
            self.replay.draw(canvas, matrix, &inside);
        })
        .with_outset((HINTED_INK_DRIFT, HINTED_INK_DRIFT));
        canvas.reset_matrix();
        canvas.save_layer(&SaveLayerRec::default().bounds(&inked).paint(&cast));
        canvas.set_matrix(&device.into());
        self.replay.draw(canvas, matrix, &inside);
        canvas.restore();
        canvas.restore();
    }
}

impl ReplayedText {
    /// Whether the run's draw reads the page in the shader: its paint's
    /// blend, or the blend a layer holding it composites through, past the
    /// blends a GPU does in its fixed-function stage -- see
    /// [`blends_by_coefficients`].
    pub(crate) fn reads_the_page(&self) -> bool {
        !blends_by_coefficients(self.paint.blend_mode_or(BlendMode::SrcOver))
            || self
                .blend
                .is_some_and(|blend| !blends_by_coefficients(blend))
    }

    /// What the run inks on a canvas whose transform is `device`, within
    /// `clip`, in that canvas's pixels: its replay and its shadow recorded
    /// under `device` -- see [`inked_by`] -- bounded as Skia bounds them,
    /// which its own culling depends on, and widened by [`HINTED_INK_DRIFT`]
    /// device pixels, since Skia takes a glyph's bounds at the font's own
    /// size while FreeType hints it at the size it is drawn: the drift that
    /// constant was measured for, at most 1.73 pixels over its CoreText and
    /// FreeType sweeps. Too wide only ends a batch of runs sooner; too
    /// narrow leaves ink unwritten. `None` where the
    /// run has no replay, a paragraph drawn from its raster, or composites
    /// through a layer of its own, which covers the clip.
    pub(crate) fn ink_under(
        &self,
        device: &Matrix,
        clip: IRect,
    ) -> Option<IRect> {
        let replay = self.replay.as_ref().filter(|_| self.blend.is_none())?;
        // Its glyphs drawn whole, as one blob bounded by them all, rather
        // than apart as a recording draws them.
        let _whole = WholeRuns::hold();
        let inked = inked_by(Rect::from(clip), device, |canvas| {
            if let Some(shadow) = &self.shadow {
                shadow.draw(canvas, &self.matrix);
            }
            if !self.shadow_alone {
                replay.draw(canvas, &self.matrix, &self.paint);
            }
        });
        // Skia bounds a run by its glyphs at the font's own size, unhinted,
        // and FreeType hints them at the size they are drawn: at a density
        // of 2 a run inked a row below what its recording reported.
        let inked = inked.with_outset((HINTED_INK_DRIFT, HINTED_INK_DRIFT));
        Some(IRect::intersect(&inked.round_out(), &clip).unwrap_or_default())
    }

    /// Draws `replay`, the run, onto `canvas` from its coverage, where it is
    /// translucent in one flat colour and `canvas` is an eight-bit page on
    /// the CPU in the colour's space -- see [`FlatRun`] -- and reports
    /// whether it did. Under a blend that covers beyond the draw, `canvas`
    /// is drawing into the layer the blend composites, which starts
    /// transparent, and the run is drawn there as on a page of its own: its
    /// groups composited one over another as the page composites them, where
    /// through [`Self::flat_paint`] Skia composites a glyph over one beneath
    /// it before rounding the product, a level off on the pixels two glyphs
    /// share.
    fn drawn_flat(&self, canvas: &SkCanvas, replay: &Replay) -> bool {
        let Some(flat) = &self.flat else {
            return false;
        };
        // Under a perspective no plane is made, and nothing is drawn here: a
        // run's colour glyphs drawn before that is known would be drawn
        // again with the rest.
        if !coverage_plays(canvas, &flat.exact, false)
            || canvas.local_to_device_as_3x3().has_perspective()
        {
            return false;
        }
        // The run's draws held rather than made -- a group of its glyphs at
        // a time, no two of which ink a pixel in common, and the colour
        // glyphs this library draws in their place among them -- and what
        // each can ink in the page's device pixels: from the blobs a raster
        // keeps, so that nothing builds them again -- see `RunPaint::paint`.
        canvas.save();
        let groups = held_groups(canvas, || {
            self.draw_run(canvas, replay, &flat.opaque, true);
        });
        canvas.restore();
        // Skia bounds a glyph at the font's own size, unhinted, and FreeType
        // hints it at the size it is drawn -- see [`Self::ink_under`]. A run
        // of colour glyphs alone has no plain glyph to draw from its
        // coverage, and is drawn as it stands.
        let Some(ink) = groups
            .iter()
            .filter(|group| !group.coloured())
            .map(HeldGroup::bounds)
            .reduce(Rect::join2)
            .map(|ink| {
                ink.with_outset((HINTED_INK_DRIFT, HINTED_INK_DRIFT))
                    .round_out()
            })
        else {
            return false;
        };
        // The colour placed for a fill at the alpha -- see
        // [`ExactColour::at`] -- so that each pixel is a fill's
        // premultiplied bytes times the coverage, rounded once: what a GPU
        // canvas draws the run's paint as, and what a fill of the colour
        // stores where the run covers a pixel whole.
        let (rgb, gain) =
            (flat.exact.at(flat.alpha).0.rgb, byte_alpha(flat.alpha));
        // Through the run's blend alone: its paint carries the colour, and
        // on a page in another space than sRGB a colour filter that places
        // it -- see `Context2D::exact_text_paint` -- which would recolour
        // the coloured image again.
        let mut through = Paint::default();
        through
            .set_anti_alias(true)
            .set_blend_mode(self.paint.blend_mode_or(BlendMode::SrcOver));
        let composited = |draw: &dyn Fn(&SkCanvas)| {
            chain::coverage::unblurred(
                canvas,
                ink,
                chain::coverage::Plane::Alpha,
                draw,
            )
            .and_then(|coverage| {
                #[cfg(test)]
                FLAT_REGION.set(Some(
                    FLAT_REGION.get().map_or(coverage.region(), |region| {
                        IRect::join(&region, &coverage.region())
                    }),
                ));
                chain::coverage::coloured(
                    canvas,
                    &coverage.unblurred(),
                    rgb,
                    gain,
                )
            })
            .map(|outcome| outcome.composite(canvas, &through))
            .is_some()
        };
        // A group of the run's glyphs at a time, no two of which ink a pixel
        // in common, each from its own coverage and composited over the
        // groups before it, as a raster composites a blob's glyphs one after
        // another: drawn as one coverage, glyphs that overlap would cover
        // their shared pixels once where the page covers them twice -- 79
        // levels from Oswald's run at -12px of letter spacing. A run whose
        // glyphs share no pixel is one group.
        //
        // A group that inks nothing the clip shows composites nothing, which
        // drawing it as it stands leaves alone too. `false` where no group
        // composited, a plane not made among the reasons.
        // Counted rather than asked with `any`, which would stop at the
        // first group composited and leave the rest undrawn.
        //
        // Colour glyphs are drawn as they stand, in their place in the run:
        // each composites through a layer of its own in the run's paint --
        // see `ColourRun::draw` -- over the groups before it and under the
        // groups after, as Skia composites a run of them. Drawn, they count
        // as drawn, so that a run whose plain glyphs the clip hides is not
        // drawn again with its colour glyphs on top.
        let drawn = groups
            .iter()
            .filter(|group| match group.coloured() {
                true => {
                    group.draw(canvas);
                    true
                }
                false => composited(&|plane| group.draw(plane)),
            })
            .count()
            > 0;
        #[cfg(test)]
        if drawn {
            FLAT_RUNS.set(FLAT_RUNS.get() + 1);
        }
        drawn
    }

    fn playback(&self, canvas: &SkCanvas, target: Target) {
        // Its runs join a batch only where nothing of its own lies between
        // them and the page: no clip, no layer for a blend, no shadow
        // beneath, drawn again from its replay onto pixels -- see
        // `BatchScope`.
        let joins = self.clip.is_none()
            && self.blend.is_none()
            && self.shadow.is_none()
            && self.replay.is_some()
            && target == Target::Pixels;
        let _suspended = (!joins).then(|| BatchScope::suspend(canvas));
        canvas.save();
        if let Some(clip) = self.clip.as_ref().and_then(Path::deserialize) {
            canvas.clip_path(&clip, ClipOp::Intersect, true);
        }
        match (target, Format::of(canvas)) {
            (Target::Pixels, Some(format)) if self.replay.is_none() => {
                self.rasterized(canvas, &format);
            }
            (Target::Pixels, Some(format)) => {
                let _route = ImageRoute::open(format);
                if let Some(blend) = self.blend {
                    let mut through = Paint::default();
                    through.set_anti_alias(true).set_blend_mode(blend);
                    canvas.save_layer(&SaveLayerRec::default().paint(&through));
                }
                if let Some(shadow) = &self.shadow {
                    shadow.draw(canvas, &self.matrix);
                }
                if let Some(replay) =
                    self.replay.as_ref().filter(|_| !self.shadow_alone)
                    && !self.drawn_flat(canvas, replay)
                {
                    // In the layer the blend opened, where the coverage does
                    // not play -- a GPU page -- a flat run through the paint
                    // its recording is drawn in -- see [`Self::flat_paint`].
                    let shaded =
                        self.flat_paint().filter(|_| self.blend.is_some());
                    canvas.save();
                    self.draw_run(
                        canvas,
                        replay,
                        shaded.as_ref().unwrap_or(&self.paint),
                        shaded.is_some(),
                    );
                    canvas.restore();
                }
                if self.blend.is_some() {
                    canvas.restore();
                }
            }
            (Target::Pixels, None) => {
                if let Some(recorded) = self.recorded() {
                    recorded.playback(canvas);
                }
            }
            (Target::Document, _) => {
                if let Some(recorded) = self.recorded_for_document() {
                    recorded.playback(canvas);
                }
            }
        }
        canvas.restore();
    }

    /// The run as recorded for pixels, recorded now if nothing has asked
    /// for it yet. `None` where Skia records nothing.
    ///
    /// Pixels draw the run again from its replay and never ask, nor does an
    /// SVG, which embeds a colour glyph as pixels; a canvas drawn into
    /// another asks once, as does a PDF for [`Self::recorded_for_document`].
    /// Recorded as each run was drawn, the pictures cost every `fillText`
    /// of such a run a recording with a layer for each colour glyph, mostly
    /// thrown away unplayed. Recorded here, each is
    /// the picture `Context2D::render_to_canvas` records of the draw, in
    /// the same paints: the run's shadow beneath it, each under the
    /// transform the run was drawn under and reaching the clip as
    /// `Context2D::draw_text` has it -- see [`reach_the_clip`] -- through a
    /// picture the blend composites where the blend covers beyond the draw.
    /// A flat run's glyphs are the exception, recorded in
    /// [`Self::flat_paint`], so that a canvas drawn as an image of its page
    /// lands the bytes the page holds.
    pub(crate) fn recorded(&self) -> Option<&Picture> {
        self.recording.get_or_record(|| {
            let shaded = self.flat_paint();
            self.record(
                shaded.as_ref().unwrap_or(&self.paint),
                self.shadow.as_ref(),
                shaded.is_some(),
            )
        })
    }

    /// Draws `replay`, the run, onto `canvas` in `paint`, under the
    /// transform it was drawn under. Where `flat`, `paint` is one a flat
    /// run's plain glyphs are drawn in -- [`FlatRun::opaque`] or
    /// [`Self::flat_paint`] -- and its colour glyphs are drawn in the run's
    /// own paint, as every other draw of the run draws them -- see
    /// [`ColourGlyphsIn`].
    fn draw_run(
        &self,
        canvas: &SkCanvas,
        replay: &Replay,
        paint: &Paint,
        flat: bool,
    ) {
        let _colours = flat.then(|| ColourGlyphsIn::hold(self.paint.clone()));
        replay.draw(canvas, &self.matrix, paint);
    }

    /// The paint a flat run is recorded for pixels in -- see [`FlatRun`] --
    /// so that a picture of the page lands the bytes the page itself does:
    /// its colour through an opaque colour shader, at its alpha as the
    /// paint's, which Skia's raster pipeline multiplies by a glyph's coverage
    /// rounded once, the product [`Self::drawn_flat`] lands, where the run's
    /// own paint is blitted as `a * (c + 1) >> 8`. In the colour the run was
    /// named in, so that Skia pre-blends the mask by the colour the plane is
    /// drawn in. A run under a blend of its own is drawn in it too, into the
    /// layer the blend composites, where that layer is not drawn from the
    /// coverage -- on a GPU page. Over pixels already there the pipeline
    /// composites the product before rounding it, as Skia's blit does,
    /// which is why an eight-bit page on the CPU, and a layer on one, is
    /// drawn from the coverage. `None` for any other run.
    ///
    /// The alpha is the paint's rather than the shader's because a glyph its
    /// scaler makes an image of is drawn in the paint's colour, faded by the
    /// paint's alpha alone, the shader unread: on macOS every glyph of a face
    /// CoreText reports as holding colour glyphs, such as one with a `COLR`
    /// table, its plain glyphs too (`SkTypeface_Mac::onFilterRec`). With the
    /// alpha in the shader such a run at 0.6 landed opaque, 102 levels off
    /// its own page; in the paint, where an anti-aliased edge of the image is
    /// rounded once more than the coverage rounds it, within two levels on
    /// macOS, and a glyph mask lands the exact product either way.
    pub(crate) fn flat_paint(&self) -> Option<Paint> {
        let flat = self.flat.as_ref()?;
        let named = flat.opaque.color4f();
        let mut paint = flat.opaque.clone();
        paint
            .set_shader(shaders::color_in_space(
                Color4f::new(named.r, named.g, named.b, 1.0),
                ColorSpace::new_srgb(),
            ))
            .set_alpha_f(flat.alpha);
        // The blend the run's own paint is drawn through: under a blend that
        // covers beyond the draw that is source-over, into the layer the
        // blend composites, where the blend itself would leave nothing.
        paint.set_blend_mode(self.paint.blend_mode_or(BlendMode::SrcOver));
        Some(paint)
    }

    /// The run as recorded for a document, recorded now if nothing has
    /// asked for it yet, its glyphs drawn whole -- see [`WholeRuns`]: in
    /// the document's own paint on a page that keeps one; otherwise the
    /// recording for pixels -- see [`Self::recorded`] -- where a blend by
    /// coefficients draws its glyphs whole there too, and a recording of
    /// its own where the recording for pixels draws them apart.
    pub(crate) fn recorded_for_document(&self) -> Option<&Picture> {
        let _whole = WholeRuns::hold();
        let Some(document) = &self.document else {
            if blends_by_coefficients(
                self.paint.blend_mode_or(BlendMode::SrcOver),
            ) {
                return self.recorded();
            }
            return self.whole.get_or_record(|| {
                self.record(&self.paint, self.shadow.as_ref(), false)
            });
        };
        document.picture.get_or_record(|| {
            self.record(&document.paint, document.shadow.as_ref(), false)
        })
    }

    /// The run recorded in `paint`, `shadow` beneath it, for
    /// [`Self::recorded`]: its colour glyphs in the run's own paint where
    /// `flat` -- see [`Self::draw_run`].
    fn record(
        &self,
        paint: &Paint,
        shadow: Option<&ReplayedShadow>,
        flat: bool,
    ) -> Option<Picture> {
        #[cfg(test)]
        if let Some(watch) = &self.watch {
            watch.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        let replay = self.replay.as_ref()?;
        // Each draw reaches the clip, as it did where it was drawn, whatever
        // this thread is working out -- see [`inked_by`].
        let inked_only = INKED_ONLY.replace(false);
        let record_run = |canvas: &SkCanvas| {
            if let Some(shadow) = shadow {
                shadow.draw(canvas, &self.matrix);
                canvas.save();
                canvas.translate(shadow.offset);
                reach_the_clip(canvas);
                canvas.restore();
            }
            if !self.shadow_alone {
                canvas.save();
                self.draw_run(canvas, replay, paint, flat);
                reach_the_clip(canvas);
                canvas.restore();
            }
        };
        let mut recorder = PictureRecorder::new();
        let canvas = recorder.begin_recording(self.bounds, true);
        match self.blend {
            Some(blend) => {
                let mut layer = PictureRecorder::new();
                record_run(layer.begin_recording(self.bounds, true));
                if let Some(drawn) = layer.finish_recording_as_picture(None) {
                    let mut through = Paint::default();
                    through.set_anti_alias(true).set_blend_mode(blend);
                    canvas.draw_picture(&drawn, None, Some(&through));
                }
            }
            None => record_run(canvas),
        }
        INKED_ONLY.set(inked_only);
        recorder.finish_recording_as_picture(None)
    }

    /// Draws the recording for pixels onto `canvas` through a raster made
    /// on the CPU in `format`, under the transform `canvas` carries,
    /// covering what the draw inks there, composited as an image at whole
    /// device pixels: the same steps whichever engine `canvas` draws on, so
    /// that a GPU canvas composites the pixels the CPU made, where playing
    /// the recording drew each colour glyph's layer through Ganesh. Played
    /// as recorded under a perspective, which a raster cannot follow.
    ///
    /// The draw's colour glyphs are placed as they were recorded, at the
    /// transform the draw was made under; a run kept whole places them where
    /// it is played -- see [`Replay`].
    fn rasterized(&self, canvas: &SkCanvas, format: &Format) {
        if let Some(recorded) = self.recorded() {
            rasterized_onto(
                canvas,
                recorded,
                Some(
                    self.inked
                        .with_outset((HINTED_INK_DRIFT, HINTED_INK_DRIFT)),
                ),
                format,
                &Paint::default(),
                Pipeline::Bytes,
            );
        }
    }

    /// What the run reaches, under the transform it is played back under:
    /// its recording's cull rectangle -- see [`drawn_by`] -- known without
    /// recording it where nothing has asked for it yet. Each draw of a run
    /// reaches the clip it is recorded under -- see [`reach_the_clip`] --
    /// and a run is recorded under no clip but the page's bounds, so its
    /// recording reaches the whole pixels the bounds cover.
    pub(crate) fn reach(&self) -> Rect {
        match self.recording.get() {
            Some(recorded) => recorded
                .as_ref()
                .map_or_else(Rect::new_empty, Picture::cull_rect),
            None => Rect::from(RoundOut::<IRect>::round_out(&self.bounds)),
        }
    }

    /// Whether this stands for a pair of recordings -- see [`holds_a_pair`].
    pub(crate) fn is_pair(&self) -> bool {
        self.document.is_some()
    }
}

/// The pipeline a CPU canvas draws a draw on, as Skia's raster pipeline
/// chooses it for an eight-bit destination: its eight-bit one (`lowp`) where
/// every stage the draw takes has an eight-bit form, which
/// `SK_RASTER_PIPELINE_OPS_LOWP` in `SkRasterPipelineOpList.h` lists, and its
/// float one (`highp`) where any stage has none -- see [`pipeline_of`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pipeline {
    /// Eight bits a channel, or a draw the CPU rounds to bytes before it
    /// composites it: one whose image filter Skia runs in a layer of its own.
    Bytes,
    /// Floats, rounded once, as the draw is stored onto what it lands on.
    Floats,
}

/// The blend modes whose stages `SK_RASTER_PIPELINE_OPS_LOWP` leaves out, in
/// `SkRasterPipelineOpList.h`: a draw through one is drawn on the CPU's
/// float pipeline whatever else it takes.
const FLOAT_BLENDS: [BlendMode; 7] = [
    BlendMode::ColorDodge,
    BlendMode::ColorBurn,
    BlendMode::SoftLight,
    BlendMode::Hue,
    BlendMode::Saturation,
    BlendMode::Color,
    BlendMode::Luminosity,
];

/// The pipeline the CPU draws a draw on straight onto an eight-bit page in
/// `page` -- see [`Pipeline`] -- one made with `paint`, dyed by `spec` where a
/// gradient dyes it, and composited through `blend`.
///
/// Floats where `spec` is shaded in floats -- see
/// [`GradientSpec::shaded_in_floats`] -- where `blend` is one of
/// [`FLOAT_BLENDS`], or where the paint dithers or has a colour filter other
/// than a blend mode's with an eight-bit stage. An image filter that is a
/// colour filter is one: Skia moves it into the paint's colour filter slot
/// where the paint has no mask filter (`SkCanvasPriv::ImageToColorFilter`).
/// Any other image filter is drawn into a layer of its own and rounded there
/// to its pixels, whose depth is the raster's, so such a draw is `Bytes`. A
/// shader other than a gradient's is taken to have an eight-bit form, which
/// an image's and a colour's have.
pub(crate) fn pipeline_of(
    spec: Option<&GradientSpec>,
    page: &ColorSpace,
    paint: &Paint,
    blend: BlendMode,
) -> Pipeline {
    let folded = match paint.image_filter() {
        None => None,
        Some(filter) => match filter.to_a_color_filter() {
            Some(filter) if paint.mask_filter().is_none() => Some(filter),
            _ => return Pipeline::Bytes,
        },
    };
    let float_filter = |filter: &ColorFilter| {
        filter
            .to_a_color_mode()
            .is_none_or(|(_, mode)| FLOAT_BLENDS.contains(&mode))
    };
    let floats = spec.and_then(|spec| spec.shaded_in_floats(page))
        == Some(true)
        || FLOAT_BLENDS.contains(&blend)
        || paint.is_dither()
        || paint.color_filter().iter().chain(&folded).any(float_filter);
    match floats {
        true => Pipeline::Floats,
        false => Pipeline::Bytes,
    }
}

/// The colour type a draw the CPU makes on its float pipeline is rasterized
/// in, inside a layer a backdrop seeds, on an eight-bit page -- see
/// [`rasterized_onto`]: sixteen bits a channel, unsigned, which holds the
/// CPU's float to within half of 1/65535 -- a 514th of a level.
///
/// Fixed by what Ganesh can sample rather than chosen: neither its Metal
/// caps nor its Vulkan caps list a 32-bit float format (`GrMtlCaps.mm`,
/// `GrVkCaps.cpp`), and it uploads an `RGBAF32` image as eight bits, which
/// rounds what this exists to keep. Half floats hold a level near 255 to
/// within a sixteenth.
const UNROUNDED_SOURCE: ColorType = ColorType::R16G16B16A16UNorm;

/// Draws `recorded` onto `canvas` through a raster made on the CPU in
/// `format`, under the transform `canvas` carries, composited as an image at
/// whole device pixels through `through`: the same steps whichever engine
/// `canvas` draws on, so that a GPU canvas composites the pixels the CPU
/// made. Played as recorded under a perspective, which a raster cannot
/// follow.
///
/// The raster covers `inked`, in the coordinates `canvas` is drawn in, where
/// the draw inks, within the clip; and the clip's bounds whole where `inked`
/// is `None`, for a blend that changes what lies beyond the draw. It starts
/// transparent, so the draw is composited as the CPU composites a layer of
/// it: on pixels it covers whole, through `through`'s blend, the bytes the
/// CPU would draw there.
///
/// Inside a layer a backdrop seeded, where the draw lands on what lies
/// beneath the layer and its pixels cannot be read, a draw `made_in` the
/// CPU's float pipeline is rasterized in [`UNROUNDED_SOURCE`] on an
/// eight-bit page: the CPU blends its unrounded colour onto that backdrop
/// and rounds once, where a raster of bytes rounds the colour first and the
/// GPU's blend rounds again, a level apart wherever the two differ -- 9,689
/// of 65,536 bytes of an Oklab radial. A `source-over` one is composited
/// through [`exact_composite::float_over`]'s blender, which reads the
/// destination and rounds once as the CPU does. The fixed-function blend
/// cannot carry the unrounded source everywhere: llvmpipe and NVIDIA blend
/// an eight-bit target at its own precision, so there the sixteen-bit
/// raster moved none of those bytes. What the blender leaves apart lies
/// within a fraction of a level of a tie: the precision its source arrives
/// in, a half on Metal. Anywhere else the raster is in `format`: a layer
/// that starts transparent stores the draw's colour rounded once, which a
/// raster of bytes holds exactly and the GPU copies.
pub(crate) fn rasterized_onto(
    canvas: &SkCanvas,
    recorded: &Picture,
    inked: Option<Rect>,
    format: &Format,
    through: &Paint,
    made_in: Pipeline,
) {
    let device = canvas.local_to_device_as_3x3();
    if device.has_perspective() {
        recorded.playback(canvas);
        return;
    }
    let Some(clip) = canvas.device_clip_bounds() else {
        return;
    };
    let area = match inked {
        Some(inked) => {
            let Some(area) =
                IRect::intersect(&device.map_rect(inked).0.round_out(), &clip)
            else {
                return;
            };
            area
        }
        None => clip,
    };
    // Under the canvas's own surface properties, so that text the draw holds
    // is rasterized under the page's contrast and gamma rather than Skia's
    // defaults, which differ from the page's on Linux.
    let props = canvas.props();
    let mut info = format.image_info(area.size());
    let unrounded = made_in == Pipeline::Floats
        && InLayer::seeded()
        && matches!(
            info.color_type(),
            ColorType::RGBA8888 | ColorType::BGRA8888
        );
    let mut over = None;
    if unrounded {
        info = info.with_color_type(UNROUNDED_SOURCE);
        if through.as_blend_mode() == Some(BlendMode::SrcOver) {
            over = exact_composite::float_over(HIGHP_TIES_TO_EVEN).map(
                |blender| {
                    let mut over = through.clone();
                    over.set_blender(blender);
                    over
                },
            );
        }
    }
    let Some(mut surface) = started::raster(&info, None, props.as_ref()) else {
        return;
    };
    let raster = surface.canvas();
    raster.translate((-area.left as f32, -area.top as f32));
    raster.concat(&device);
    recorded.playback(raster);
    canvas.save();
    canvas.reset_matrix();
    composite_onto(
        canvas,
        &surface.image_snapshot(),
        IPoint::new(area.left, area.top),
        over.as_ref().unwrap_or(through),
    );
    canvas.restore();
}

thread_local! {
    /// How many layers this thread is playing entries inside, opened by
    /// [`Group::playback_within`] -- see [`InLayer`].
    static IN_LAYERS: Cell<u32> = const { Cell::new(0) };
    /// Whether the innermost of those layers was seeded from what lies
    /// beneath it -- see [`InLayer::seeded`].
    static SEEDED: Cell<bool> = const { Cell::new(false) };
}

/// Counts a layer [`Group::playback_within`] opens on a canvas around the
/// entries it plays, for as long as it lives, so that an entry can tell
/// whether the pixels under it are the canvas's own: `SkCanvas::readPixels`
/// reads the canvas's root device, never an open layer. A layer whose
/// entries play on a surface of their own counts none -- see
/// [`played_on_a_root`].
///
/// Holds whether the layer it counts out of, if any, was seeded.
struct InLayer(bool);

impl InLayer {
    /// Counts a layer, `seeded` where a backdrop seeds it with what lies
    /// beneath it rather than leaving it transparent.
    fn enter(seeded: bool) -> Self {
        IN_LAYERS.set(IN_LAYERS.get() + 1);
        InLayer(SEEDED.replace(seeded))
    }

    /// Whether entries are being played inside a layer on this thread.
    fn open() -> bool {
        IN_LAYERS.get() > 0
    }

    /// Whether entries are being played inside a layer on this thread whose
    /// backdrop seeded it, and so holds what lies beneath it under whatever
    /// a draw inside it lands on.
    fn seeded() -> bool {
        Self::open() && SEEDED.get()
    }
}

impl Drop for InLayer {
    fn drop(&mut self) {
        IN_LAYERS.set(IN_LAYERS.get().saturating_sub(1));
        SEEDED.set(self.0);
    }
}

/// Counts no layer open for as long as it lives, and the layers open before
/// once it goes: entries played onto a surface of their own -- see
/// [`played_on_a_root`] -- are on that surface's root device, which
/// `readPixels` reads, whatever is open on the canvas the surface is copied
/// onto.
struct OwnRoot(u32, bool);

impl OwnRoot {
    fn enter() -> Self {
        OwnRoot(IN_LAYERS.replace(0), SEEDED.replace(false))
    }
}

impl Drop for OwnRoot {
    fn drop(&mut self) {
        IN_LAYERS.set(self.0);
        SEEDED.set(self.1);
    }
}

/// Draws `recorded` onto `canvas` as the CPU draws it, over the pixels
/// `canvas` holds there -- see [`drawn_on_their_backdrop`]: it reaches its
/// cull rectangle, or the clip's bounds whole where `beyond` says its blend
/// changes what lies beyond the draw.
fn drawn_on_its_backdrop(
    canvas: &SkCanvas,
    recorded: &Picture,
    format: &Format,
    beyond: bool,
) -> bool {
    let draw = |raster: &SkCanvas| recorded.playback(raster);
    drawn_on_their_backdrop(
        canvas,
        &[BackdropDraw {
            reach: match beyond {
                true => BackdropReach::Beyond,
                false => BackdropReach::Local(recorded.cull_rect()),
            },
            draw: &draw,
            clip: None,
        }],
        format,
    )
}

/// A run of draws dyed by gradients, kept so that a GPU canvas draws them in
/// the bytes the CPU draws them in (#667).
///
/// Ganesh draws a gradient through a layout and a colorizer that lose the
/// precision the CPU's raster pipeline keeps -- see
/// `exact_gradient` -- so that a hard stop moves by up to a tenth of a
/// degree or half a pixel and a smooth ramp lands up to four levels away.
/// Each draw is kept as recorded, which a CPU canvas and a document play as
/// they always have; and a GPU canvas plays instead either the draw recorded
/// with an image of the effect that computes the CPU's bytes -- see
/// [`GradientSpec::sampled`](super::exact_gradient::GradientSpec::sampled)
/// -- or, where the effect cannot, the draw made on the CPU and composited
/// -- see [`rasterized_onto`].
///
/// Chosen where the page is played back, not where it is drawn, so that a
/// canvas drawn on the CPU and switched to the GPU draws its gradients
/// exactly as one drawn on the GPU throughout.
///
/// A canvas drawn as an image whose page holds such a draw, or colour
/// glyphs, is kept here as well (#703), always made on the CPU on a GPU
/// canvas -- see [`OnGpu::Raster`]: its image is the page's own pixels,
/// which Ganesh would composite in its own arithmetic, and only a raster
/// made on the CPU composites them in the CPU's bytes -- see
/// [`Layer::plays_apart_on_gpu`].
#[derive(Clone, Debug)]
pub struct GradientDraw {
    pub draws: Vec<GradientPiece>,
    /// The clip the run was drawn under, in device pixels of the recording,
    /// where it is not a piece of a run that applies it -- see [`Clipped`].
    pub clip: Option<Data>,
}

/// One draw of a [`GradientDraw`].
#[derive(Clone, Debug)]
pub struct GradientPiece {
    /// The draw as the page's pixels recording records it: what a CPU
    /// canvas plays.
    pub pixels: GradientPixels,
    /// The draw as a document records it, on a page that keeps a recording
    /// for each kind of target -- see `page::Recorders`.
    pub document: Option<Picture>,
    /// How a GPU canvas draws it.
    pub gpu: OnGpu,
}

impl GradientPiece {
    /// The bytes the piece's recordings hold -- see [`Layer::recorded_bytes`]:
    /// the pixels recording's, the document's and what a GPU canvas plays,
    /// a path both keep counted once.
    pub(crate) fn recorded_bytes(&self) -> usize {
        let gpu = match (&self.pixels, &self.gpu) {
            (GradientPixels::Path(pixels), OnGpu::Path(gpu))
                if Arc::ptr_eq(pixels, gpu) =>
            {
                0
            }
            (_, gpu) => gpu.recorded_bytes(),
        };
        self.pixels.recorded_bytes()
            + self
                .document
                .as_ref()
                .map_or(0, Picture::approximate_bytes_used)
            + gpu
    }
}

/// A [`GradientPiece`] as a CPU canvas plays it: recorded, or, for a path,
/// the path kept -- see [`GradientPath`] -- so that a page drawing many
/// small gradient paths records no picture for each. A picture of one costs
/// a recorder begun and finished and a bounding-box hierarchy built, which
/// was about 30% of the drawing thread's time, 1.3 us a draw, for 2000
/// small gradient fills.
#[derive(Clone, Debug)]
pub enum GradientPixels {
    Recorded(Picture),
    Path(Arc<GradientPath>),
}

impl GradientPixels {
    /// Plays the draw onto `canvas`, as a picture of it would.
    pub(crate) fn playback(&self, canvas: &SkCanvas) {
        match self {
            GradientPixels::Recorded(picture) => picture.playback(canvas),
            GradientPixels::Path(draw) => {
                canvas.save();
                canvas.concat(&draw.matrix);
                canvas.draw_path(&draw.path.lock(), &draw.pixels);
                canvas.restore();
            }
        }
    }

    /// The bytes the recording, or the path, holds.
    pub(crate) fn recorded_bytes(&self) -> usize {
        match self {
            GradientPixels::Recorded(picture) => {
                picture.approximate_bytes_used()
            }
            GradientPixels::Path(draw) => {
                draw.path.lock().approximate_bytes_used()
            }
        }
    }

    /// What the draw reaches, in the recording's device pixels.
    pub(crate) fn cull_rect(&self) -> Rect {
        match self {
            GradientPixels::Recorded(picture) => picture.cull_rect(),
            GradientPixels::Path(draw) => draw.bounds,
        }
    }

    /// The draw as a picture, for a route that hands one to Skia: the
    /// recording itself, or one made of the path now.
    fn picture(&self) -> Option<Cow<'_, Picture>> {
        match self {
            GradientPixels::Recorded(picture) => Some(Cow::Borrowed(picture)),
            GradientPixels::Path(draw) => {
                let mut recorder = PictureRecorder::new();
                let canvas = recorder.begin_recording(draw.bounds, false);
                canvas.set_matrix(&draw.matrix.into());
                canvas.draw_path(&draw.path.lock(), &draw.pixels);
                recorder
                    .finish_recording_as_picture(Some(&draw.bounds))
                    .map(Cow::Owned)
            }
        }
    }
}

/// How a GPU canvas draws a [`GradientPiece`].
#[derive(Clone, Debug)]
pub enum OnGpu {
    /// The draw recorded with the effect that computes the CPU's bytes on
    /// the GPU: itself, aliased, for a rectangle on whole device pixels --
    /// see [`GradientDye`](super::GradientDye) -- and otherwise an image of
    /// it -- see
    /// [`GradientSpec::sampled`](super::exact_gradient::GradientSpec::sampled)
    /// -- whose texels are the page's device pixels. A run of glyphs is
    /// recorded as the effect over the pixels it reaches, masked by the
    /// CPU's coverage of the run, whose edges a glyph takes from the
    /// gradient -- see `Context2D::glyphs_with_effect`. Played under any
    /// other mapping than onto whole pixels -- a canvas drawn into another
    /// under a scale, say -- the image would be resampled, so the draw is
    /// made on the CPU instead: over the GPU's pixels, `beyond` saying as
    /// for [`OnGpu::Raster`] what it composites over, or, inside a layer
    /// whose pixels cannot be read -- one not played on a root of its own,
    /// see [`played_on_a_root`] -- over transparent and composited through
    /// `blend` -- `over`, the draw recorded with `SrcOver`, where `blend` is
    /// another -- in a raster of the depth `made_in` asks for, see
    /// [`rasterized_onto`].
    Effect {
        picture: Picture,
        over: Option<Picture>,
        blend: BlendMode,
        beyond: bool,
        made_in: Pipeline,
    },
    /// The draw made on the CPU, its layers -- its shadow, then itself --
    /// each recorded composited with `SrcOver` and composited in turn onto
    /// the GPU's pixels through `blend`: over what it inks, or over the
    /// clip's bounds whole where `beyond` says the blend changes what lies
    /// beyond the draw.
    Raster {
        layers: Vec<RasterLayer>,
        blend: BlendMode,
        beyond: bool,
    },
    /// The draw made on the CPU and composited with `SrcOver` onto a layer
    /// that holds nothing else where it draws -- a chain's input, played
    /// into the chain's own layer, which starts transparent -- so that,
    /// unlike [`OnGpu::Raster`], nothing is read back first: a read back
    /// waits for the GPU, and one each made 200 blurred gradient fills take
    /// three times what Ganesh's own gradient did.
    Alone,
    /// A path dyed by a gradient, kept as what it is made from so that the
    /// effect is made for it where the page is played on the GPU, and a
    /// canvas that never is pays nothing for it -- see [`GradientPath`].
    Path(Arc<GradientPath>),
    /// A draw dyed by no gradient, played as recorded: one asking a vector
    /// backend for what the run asks, drawn straight after it, which the
    /// page would have kept in the same isolated layer -- see
    /// `PageRecorder::append_gradient`.
    Native,
    /// A translucent image drawn `source-over` at whole device pixels,
    /// composited in the CPU's arithmetic -- see [`GpuImage`] -- where
    /// Ganesh's own blend on Metal parts from it a level within 0.18 of a
    /// tie (#837).
    Image(Arc<GpuImage>),
}

/// An image a GPU canvas composites as the CPU does -- see
/// [`OnGpu::Image`]: unscaled, its texels on the recording's device
/// pixels, in eight bits a channel and in the page's colour space.
#[derive(Clone, Debug)]
pub struct GpuImage {
    pub image: Image,
    /// The recording's device pixel the image's own origin lies on.
    pub at: IPoint,
    /// The recording's device pixels the draw covers: the source rectangle
    /// where it lands.
    pub area: IRect,
    /// The draw's alpha, its `globalAlpha` included.
    pub alpha: f32,
}

/// One layer of a draw [`OnGpu::Raster`] makes on the CPU: its recording,
/// composited with `SrcOver`, and the pipeline the CPU draws it on, which
/// says what depth of raster it is made in -- see [`rasterized_onto`].
#[derive(Clone, Debug)]
pub struct RasterLayer {
    pub picture: Picture,
    pub made_in: Pipeline,
}

impl OnGpu {
    /// The bytes the pictures, or the path, a GPU canvas plays hold -- see
    /// [`Layer::recorded_bytes`].
    pub(crate) fn recorded_bytes(&self) -> usize {
        match self {
            OnGpu::Effect { picture, over, .. } => {
                picture.approximate_bytes_used()
                    + over.as_ref().map_or(0, Picture::approximate_bytes_used)
            }
            OnGpu::Raster { layers, .. } => layers
                .iter()
                .map(|layer| layer.picture.approximate_bytes_used())
                .sum(),
            OnGpu::Path(draw) => draw.path.lock().approximate_bytes_used(),
            // The image is the draw's own, which the page counts as held.
            OnGpu::Alone | OnGpu::Native | OnGpu::Image(_) => 0,
        }
    }
}

/// A path filled or stroked with a gradient, as a GPU canvas draws it in the
/// CPU's bytes: the effect that computes them -- see
/// [`GradientSpec::exact`](super::exact_gradient::GradientSpec::exact) --
/// through the CPU's coverage of `path` under `matrix` and `paint` -- see
/// [`drawn_with_effect`].
#[derive(Debug)]
pub struct GradientPath {
    /// The path, behind a lock: a `Path` can be sent between threads but not
    /// shared, and the page that holds this is shared.
    pub path: Mutex<Path>,
    /// What the draw reaches, in the recording's device pixels: the path's
    /// bounds under `matrix`, widened as `context::drawn_bounds` widens them.
    pub bounds: Rect,
    /// The recording's device pixels the CPU's coverage of the path can be
    /// other than zero on, made on first use -- see [`Self::inked`].
    pub inked: OnceLock<IRect>,
    /// The pipeline the CPU draws the path on, made on first use -- see
    /// [`Self::made_in`].
    pub made_in: OnceLock<Pipeline>,
    /// The page's transform the path was drawn under.
    pub matrix: Matrix,
    /// The draw's paint with no shader and an alpha of 1, the effect
    /// carrying the draw's alpha; aliased where `whole_pixels`. Its style,
    /// its stroke and its anti-aliasing are what the coverage is drawn
    /// with.
    pub paint: Paint,
    /// The paint the page's pixels recording draws the path with, from
    /// which a draw made on the CPU is recorded with `SrcOver`.
    pub pixels: Paint,
    pub(crate) spec: Arc<GradientSpec>,
    /// The page's colour space, which the effect computes the bytes in.
    pub page: ColorSpace,
    /// The draw's alpha, its `globalAlpha` included.
    pub alpha: f32,
    /// The device pixels the draw reaches, which an image of the effect and
    /// one of the coverage cover.
    pub area: IRect,
    /// Whether the path is a rectangle on whole device pixels, which takes
    /// the effect itself -- see [`GradientDye`](super::GradientDye).
    pub whole_pixels: bool,
    pub blend: BlendMode,
    /// Whether `blend` changes what lies beyond the draw -- see
    /// [`OnGpu::Raster`].
    pub beyond: bool,
}

impl GradientPath {
    /// The recording's device pixels the CPU's coverage of the path can be
    /// other than zero on -- see `context::inked_bounds` -- which `bounds`
    /// widens by a pixel of anti-aliasing either side. A stroke is bounded
    /// by the outline Skia fills for it: the reach `bounds` allows a mitred
    /// join, ten times its half width at the default limit, is a corner's
    /// and not a curve's, and held that wide, the rectangles of a row of
    /// stroked rings meet. Made where a GPU canvas plays the draw, so that a
    /// CPU canvas never strokes the path twice.
    pub fn inked(&self) -> IRect {
        *self.inked.get_or_init(|| {
            let style = self.paint.style();
            let outline = style != PaintStyle::Fill;
            inked_bounds(
                &self.path.lock(),
                &self.paint,
                style,
                &self.matrix,
                outline,
            )
            .round_out()
        })
    }

    /// The pipeline the CPU draws the path on, with `pixels` through `blend`
    /// -- see [`pipeline_of`]. Made where a GPU canvas first makes the draw
    /// on the CPU, which only a layer it cannot read back from asks for.
    pub fn made_in(&self) -> Pipeline {
        *self.made_in.get_or_init(|| {
            pipeline_of(Some(&self.spec), &self.page, &self.pixels, self.blend)
        })
    }
}

/// Where an image of a [`GradientPath`]'s effect lies in an atlas: `at`, in
/// `image` and in `mask`, holds the device pixel one up and one left of the
/// path's `area`, so that a pixel the path's edge reaches just outside
/// `area` still finds the effect and the coverage rather than another
/// path's.
#[derive(Clone)]
struct Slot {
    image: Image,
    at: IPoint,
    /// The CPU's coverage of each path in the atlas, at the same slots as
    /// `image` -- see [`draw_coverage`]. `None` where it could not be made,
    /// and the path then makes its own -- see [`coverage_image`].
    mask: Option<Image>,
}

/// What a run's [`atlas`] found for a [`GradientPath`], asked once for the
/// draw: the effect that draws it in the CPU's bytes, `None` where the
/// effect cannot reproduce the CPU; and, for one off whole pixels that fits,
/// where the atlas holds an image of it. Asking costs a hash and a compare
/// of the gradient's every number: asked again as the draw is played, 2000
/// small gradients alternating linear and conic spent 2.4 ms a frame on it.
#[derive(Clone)]
struct Found {
    effect: Option<Exact>,
    slot: Option<Slot>,
}

/// How far an atlas slot reaches past the path's area on every side.
const SLOT_MARGIN_PX: i32 = 1;

/// The widest an atlas is made, in pixels, below the GPU's own limit: a
/// shelf this wide holds a run of small draws in a few rows.
const ATLAS_WIDTH_PX: i32 = 2048;

/// One image holding the effect of each of `draws` that needs one, drawn in
/// one pass onto a surface made from `canvas`, and one of the CPU's
/// coverage of each at the same place, drawn on the CPU and uploaded once;
/// and, by index, what was found for each path: its effect, and where its
/// images are -- none for one on whole pixels, which takes the effect
/// itself, or for one that does not fit, which takes the effect itself
/// through a coverage image of its own -- see [`drawn_with_effect`].
/// `None` for a draw that is no path. With an image of its own for each
/// draw, 2000 small gradient fills cost about eight times what Ganesh's own
/// gradient did.
fn atlas(
    canvas: &SkCanvas,
    draws: &[Option<&GradientPath>],
) -> Vec<Option<Found>> {
    // The effect for each path, made first: a draw it cannot be made for
    // takes no slot, and is drawn on the CPU instead; one on whole pixels
    // takes the effect itself and no slot either.
    let effects: Vec<Option<Exact>> = draws
        .iter()
        .map(|draw| {
            draw.and_then(|draw| {
                draw.spec
                    .exact(&draw.page, draw.alpha)
                    .filter(|effect| effect.places(&draw.matrix))
            })
        })
        .collect();
    let mut slots: Vec<Option<IPoint>> = vec![None; draws.len()];
    let limit = max_texture_size(canvas);
    let width_limit = ATLAS_WIDTH_PX.min(limit);
    let (mut x, mut y, mut row, mut width) = (0, 0, 0, 0);
    let mut page = None;
    for (index, (draw, effect)) in draws.iter().zip(&effects).enumerate() {
        let (Some(draw), Some(_)) = (draw, effect) else {
            continue;
        };
        if draw.whole_pixels {
            continue;
        }
        let (w, h) = (
            draw.area.width() + 2 * SLOT_MARGIN_PX,
            draw.area.height() + 2 * SLOT_MARGIN_PX,
        );
        if w > width_limit {
            continue;
        }
        if x + w > width_limit {
            (x, y, row) = (0, y + row, 0);
        }
        if y + h > limit {
            continue;
        }
        slots[index] = Some(IPoint::new(x, y));
        page.get_or_insert_with(|| draw.page.clone());
        (x, row, width) = (x + w, row.max(h), width.max(x + w));
    }
    let height = y + row;
    let made = page.and_then(|page| {
        let info = ImageInfo::new(
            (width, height),
            canvas.image_info().color_type(),
            AlphaType::Premul,
            Some(page),
        );
        let mut surface = canvas.new_surface(&info, None)?;
        let target = surface.canvas();
        target.clear(Color4f::new(0.0, 0.0, 0.0, 0.0));
        for ((draw, effect), at) in draws.iter().zip(&effects).zip(&slots) {
            let (Some(draw), Some(effect), Some(at)) = (draw, effect, at)
            else {
                continue;
            };
            let origin = (
                draw.area.left - SLOT_MARGIN_PX,
                draw.area.top - SLOT_MARGIN_PX,
            );
            // The slot's pixel `at` is the page's `origin`.
            let mut paint = Paint::default();
            paint.set_shader(effect.placed(
                &draw.matrix,
                IPoint::new(origin.0 - at.x, origin.1 - at.y),
            ));
            target.draw_rect(
                Rect::from_xywh(
                    at.x as f32,
                    at.y as f32,
                    (draw.area.width() + 2 * SLOT_MARGIN_PX) as f32,
                    (draw.area.height() + 2 * SLOT_MARGIN_PX) as f32,
                ),
                &paint,
            );
        }
        Some(surface.image_snapshot())
    });
    // The CPU's coverage of each path the atlas holds, at the same slots.
    let mask = made.as_ref().and_then(|_| {
        let mut surface =
            started::raster(&coverage_info((width, height)), None, None)?;
        let target = surface.canvas();
        target.clear(Color4f::new(0.0, 0.0, 0.0, 0.0));
        for (draw, at) in draws.iter().zip(&slots) {
            let (Some(draw), Some(at)) = (draw, at) else {
                continue;
            };
            let origin = IPoint::new(
                draw.area.left - SLOT_MARGIN_PX,
                draw.area.top - SLOT_MARGIN_PX,
            );
            draw_coverage(target, draw, *at, origin);
        }
        Some(surface.image_snapshot())
    });
    draws
        .iter()
        .zip(effects)
        .zip(slots)
        .map(|((draw, effect), at)| {
            draw.map(|_| Found {
                effect,
                slot: at.and_then(|at| {
                    Some(Slot {
                        image: made.clone()?,
                        at,
                        mask: mask.clone(),
                    })
                }),
            })
        })
        .collect()
}

/// The widest and tallest texture the GPU `canvas` draws on takes.
#[cfg(any(feature = "metal", feature = "vulkan"))]
fn max_texture_size(canvas: &SkCanvas) -> i32 {
    canvas
        .recording_context()
        .map_or(0, |context| context.max_texture_size())
}

/// None, in a build without a GPU backend.
#[cfg(not(any(feature = "metal", feature = "vulkan")))]
fn max_texture_size(_canvas: &SkCanvas) -> i32 {
    0
}

/// Draws `draw` with the effect onto `canvas`, which maps the recording's
/// device pixels onto its own whole -- what the run's atlas `found` for the
/// draw where it was made. `false`, having drawn nothing, where no effect
/// can be made.
///
/// A rectangle on whole pixels is the effect itself. Any other path is an
/// aliased rectangle over the pixels it inks, its effect -- the atlas's
/// image of it, or the effect itself for one the atlas does not hold --
/// masked by the CPU's coverage of the path -- see [`draw_coverage`] -- so
/// that its edge is the CPU canvas's, as its shadow and its blur already
/// are, made on the CPU -- see `Context2D::chain_input`. Drawn through the
/// path, it took Ganesh's anti-aliasing instead, up to 125 levels from the
/// CPU canvas along a hard-stop conic's edge.
///
/// A `source-over` draw of an effect the CPU makes in eight bits is
/// composited in the CPU's arithmetic, not Ganesh's -- see
/// [`exact_composite`]: on the canvas's own surface it is held in `batch`,
/// inside a layer it takes the blender. An opaque effect on whole pixels
/// needs neither: it composites to itself, which the fixed-function blend
/// stores exactly.
fn drawn_with_effect(
    canvas: &SkCanvas,
    draw: &GradientPath,
    found: Option<&Found>,
    mut batch: Option<&mut exact_composite::Batch>,
) -> bool {
    let effect = match found {
        Some(found) => found.effect.clone(),
        None => draw.spec.exact(&draw.page, draw.alpha),
    };
    let Some(effect) = effect else {
        return false;
    };
    let opaque = effect.opaque();
    // A `highp` effect is composited on the CPU in float, which the eight-bit
    // composite is not.
    let exact_over = !effect.highp()
        && draw.paint.as_blend_mode() == Some(BlendMode::SrcOver)
        && !(draw.whole_pixels && opaque);
    // A snapshot of the surface holds a layer's pixels only once the layer
    // is drawn into it, so inside one a draw reads its destination itself.
    let held = exact_over && !InLayer::open() && batch.is_some();
    let outer = canvas.local_to_device_as_3x3();
    // The canvas's whole-pixel offset from the page's device pixels.
    let shift =
        IPoint::new(outer.translate_x() as i32, outer.translate_y() as i32);
    let inked = draw.inked().with_offset(shift);
    if draw.whole_pixels {
        let Some(placed) = effect.placed(&draw.matrix, -shift) else {
            return false;
        };
        if held && let Some(batch) = batch.as_deref_mut() {
            batch.hold(canvas, inked, placed, None, opaque);
            return true;
        }
        if let Some(batch) = batch.as_deref_mut() {
            batch.flush_meeting(canvas, inked);
        }
        // Drawn through the rectangle under the page's transform, at the
        // canvas's identity: its local matrix undoes the canvas's, so that
        // the effect is handed each device pixel.
        let device = Matrix::concat(&outer, &draw.matrix);
        let Some(shader) = device
            .invert()
            .map(|device| placed.with_local_matrix(&device))
        else {
            return false;
        };
        let mut paint = draw.paint.clone();
        if !(exact_over
            && exact_composite::blended(
                &mut paint,
                shader.clone(),
                None,
                opaque,
            ))
        {
            paint.set_shader(shader);
        }
        canvas.save();
        canvas.concat(&draw.matrix);
        canvas.draw_path(&draw.path.lock(), &paint);
        canvas.restore();
        return true;
    }
    let reach = draw.area.with_outset((SLOT_MARGIN_PX, SLOT_MARGIN_PX));
    let on_canvas = reach.with_offset(shift);
    let layers = match found.and_then(|found| found.slot.as_ref()) {
        // The atlas's slot `at` holds the page's pixel `reach`'s corner.
        Some(Slot {
            image,
            at,
            mask: Some(mask),
        }) => {
            let local = Matrix::translate((
                (on_canvas.left - at.x) as f32,
                (on_canvas.top - at.y) as f32,
            ));
            image
                .to_shader(DECAL, nearest(), Some(&local))
                .zip(mask.to_shader(DECAL, nearest(), Some(&local)))
        }
        _ => coverage_image(draw, reach).and_then(|mask| {
            let local = Matrix::translate((
                on_canvas.left as f32,
                on_canvas.top as f32,
            ));
            effect.placed(&draw.matrix, -shift).zip(mask.to_shader(
                DECAL,
                nearest(),
                Some(&local),
            ))
        }),
    };
    let Some((effect, coverage)) = layers else {
        return false;
    };
    // The pixels the draw can change. `reach` adds the slot's margin and
    // `draw.area` a pixel of anti-aliasing, and a held rectangle that large
    // meets its neighbours' and flushes the batch draw by draw. A path that
    // inks nothing on the page changes nothing.
    let Some(inked) = IRect::intersect(&inked, &on_canvas) else {
        return true;
    };
    if held && let Some(batch) = batch.as_deref_mut() {
        batch.hold(canvas, inked, effect, Some(coverage), opaque);
        return true;
    }
    if let Some(batch) = batch {
        batch.flush_meeting(canvas, inked);
    }
    let mut paint = draw.paint.clone();
    if !(exact_over
        && exact_composite::blended(
            &mut paint,
            effect.clone(),
            Some(coverage.clone()),
            opaque,
        ))
    {
        paint.set_shader(shaders::blend(BlendMode::DstIn, effect, coverage));
    }
    paint
        .set_anti_alias(false)
        .set_style(PaintStyle::Fill)
        .set_path_effect(None);
    canvas.save();
    canvas.reset_matrix();
    canvas.draw_irect(inked, &paint);
    canvas.restore();
    true
}

/// Composites `draw` onto `canvas`, which maps the recording's device
/// pixels onto its own whole, in the CPU's arithmetic -- see
/// [`exact_composite`]: held in `batch` on the canvas's own surface, through
/// the blender inside a layer. `false`, having drawn nothing, where Skia
/// refuses the effect.
fn drawn_as_bytes(
    canvas: &SkCanvas,
    draw: &GpuImage,
    batch: &mut exact_composite::Batch,
) -> bool {
    #[cfg(test)]
    if !EXACT_IMAGES.get() {
        return false;
    }
    let outer = canvas.local_to_device_as_3x3();
    let shift =
        IPoint::new(outer.translate_x() as i32, outer.translate_y() as i32);
    let at = draw.at + shift;
    let Some(effect) = exact_composite::image_bytes(
        &draw.image,
        &Matrix::translate((at.x as f32, at.y as f32)),
    ) else {
        return false;
    };
    let area = draw.area.with_offset(shift);
    let coverage = exact_composite::alpha_coverage(draw.alpha);
    if InLayer::open() {
        batch.flush_meeting(canvas, area);
        exact_composite::composite_once(canvas, area, effect, coverage, false);
    } else {
        batch.hold(canvas, area, effect, coverage, false);
    }
    #[cfg(test)]
    IMAGES_AS_BYTES.set(IMAGES_AS_BYTES.get() + 1);
    true
}

/// Sampling the effect's image and the coverage take: each pixel's own
/// texel, the two being laid out on the page's device pixels.
fn nearest() -> SamplingOptions {
    SamplingOptions::new(FilterMode::Nearest, MipmapMode::None)
}

/// How the effect's image and the coverage are tiled: transparent outside.
const DECAL: (TileMode, TileMode) = (TileMode::Decal, TileMode::Decal);

/// Draws the CPU's coverage of `draw`'s path onto `target`, an alpha
/// surface whose pixel `at` is the page's device pixel `origin`: the path
/// under the draw's own paint -- its style, its stroke and its
/// anti-aliasing -- in opaque white, which leaves at each pixel the
/// coverage the CPU canvas's scan conversion hands its blend.
///
/// Clipped to the draw's `area`, which ends where the page does, so that a
/// path crossing the page's edge is cut where the CPU canvas cuts it and
/// nowhere else. Skia's analytic anti-aliasing cuts a path's edges at its
/// clip's bounds and snaps the new ends to a quarter pixel, which moves
/// the coverage along the whole edge: cut at the reach, a pixel past the
/// page, a triangle crossing the page's right edge stood up to 52 levels
/// from the CPU canvas, and unclipped in the atlas a path spilled into the
/// slot beside it.
fn draw_coverage(
    target: &SkCanvas,
    draw: &GradientPath,
    at: IPoint,
    origin: IPoint,
) {
    let mut paint = draw.paint.clone();
    paint
        .set_shader(None)
        .set_color(Color::WHITE)
        .set_blend_mode(BlendMode::SrcOver);
    let shift = (at.x - origin.x, at.y - origin.y);
    target.save();
    target.clip_irect(draw.area.with_offset(shift), ClipOp::Intersect);
    target.set_matrix(
        &Matrix::concat(
            &Matrix::translate((shift.0 as f32, shift.1 as f32)),
            &draw.matrix,
        )
        .into(),
    );
    target.draw_path(&draw.path.lock(), &paint);
    target.restore();
}

/// The CPU's coverage of `draw`'s path over `reach`, the page's device
/// pixels, as an alpha image of its own: for a draw the run's atlas does
/// not hold -- see [`atlas`].
fn coverage_image(draw: &GradientPath, reach: IRect) -> Option<Image> {
    let mut surface =
        started::raster(&coverage_info(reach.size()), None, None)?;
    let target = surface.canvas();
    target.clear(Color4f::new(0.0, 0.0, 0.0, 0.0));
    draw_coverage(
        target,
        draw,
        IPoint::new(0, 0),
        IPoint::new(reach.left, reach.top),
    );
    Some(surface.image_snapshot())
}

/// An alpha image `size` pixels, which coverage is drawn into.
fn coverage_info(size: impl Into<ISize>) -> ImageInfo {
    ImageInfo::new(size, ColorType::Alpha8, AlphaType::Premul, None)
}

/// Plays `pixels`, a draw dyed by a gradient as the page's pixels recording
/// records it, onto `canvas`, a GPU canvas, as `gpu` says -- see [`OnGpu`]
/// -- with what the run's atlas `found` for it. A draw made on the CPU, on
/// a canvas whose pixels cannot be read, plays `pixels`. A path the effect
/// draws may be held in `batch` -- see [`drawn_with_effect`]; any other
/// draw flushes it first.
fn played_on_gpu(
    canvas: &SkCanvas,
    pixels: &GradientPixels,
    gpu: &OnGpu,
    found: Option<&Found>,
    batch: &mut exact_composite::Batch,
) {
    let whole = on_whole_pixels(canvas);
    if let OnGpu::Path(draw) = gpu
        && whole
        && drawn_with_effect(canvas, draw, found, Some(&mut *batch))
    {
        return;
    }
    if let OnGpu::Image(draw) = gpu
        && whole
        && drawn_as_bytes(canvas, draw, batch)
    {
        return;
    }
    // Any other draw may read or write what a held draw covers.
    batch.flush(canvas);
    match gpu {
        OnGpu::Effect { picture, .. } if whole => {
            picture.playback(canvas);
            return;
        }
        // An image off whole pixels, or one whose effect Skia refuses, is
        // played as recorded.
        OnGpu::Native | OnGpu::Image(_) => {
            pixels.playback(canvas);
            return;
        }
        _ => {}
    }
    // A route made on the CPU, from a picture of the draw.
    let (Some(format), Some(recorded)) = (Format::of(canvas), pixels.picture())
    else {
        pixels.playback(canvas);
        return;
    };
    let recorded: &Picture = &recorded;
    match gpu {
        OnGpu::Path(draw) => {
            if drawn_on_its_backdrop(canvas, recorded, &format, draw.beyond) {
                return;
            }
            let over = match draw.blend {
                BlendMode::SrcOver => None,
                _ => recorded_over(draw, recorded.cull_rect()),
            };
            let layer = over.as_ref().unwrap_or(recorded);
            let mut through = Paint::default();
            through.set_blend_mode(draw.blend);
            rasterized_onto(
                canvas,
                layer,
                (!draw.beyond).then(|| layer.cull_rect()),
                &format,
                &through,
                draw.made_in(),
            );
        }
        OnGpu::Effect {
            over,
            blend,
            beyond,
            made_in,
            ..
        } => {
            if drawn_on_its_backdrop(canvas, recorded, &format, *beyond) {
                return;
            }
            let layer = over.as_ref().unwrap_or(recorded);
            let mut through = Paint::default();
            through.set_blend_mode(*blend);
            rasterized_onto(
                canvas,
                layer,
                (!beyond).then(|| layer.cull_rect()),
                &format,
                &through,
                *made_in,
            );
        }
        OnGpu::Raster {
            layers,
            blend,
            beyond,
        } => {
            if drawn_on_its_backdrop(canvas, recorded, &format, *beyond) {
                return;
            }
            let mut through = Paint::default();
            through.set_blend_mode(*blend);
            for layer in layers {
                rasterized_onto(
                    canvas,
                    &layer.picture,
                    (!beyond).then(|| layer.picture.cull_rect()),
                    &format,
                    &through,
                    layer.made_in,
                );
            }
        }
        // Into a chain's layer, which starts transparent and holds bytes on
        // the CPU as well: there the CPU rounds the draw's colour once, which
        // a raster of bytes holds exactly.
        OnGpu::Alone => rasterized_onto(
            canvas,
            recorded,
            Some(recorded.cull_rect()),
            &format,
            &Paint::default(),
            Pipeline::Bytes,
        ),
        OnGpu::Native | OnGpu::Image(_) => pixels.playback(canvas),
    }
}

/// `draw` recorded as the page's pixels recording records it, but with
/// `SrcOver`, over `cull`: what a layer, whose pixels cannot be read,
/// composites through the draw's own blend.
fn recorded_over(draw: &GradientPath, cull: Rect) -> Option<Picture> {
    let mut recorder = PictureRecorder::new();
    let canvas = recorder.begin_recording(cull, false);
    canvas.set_matrix(&draw.matrix.into());
    let mut over = draw.pixels.clone();
    over.set_blend_mode(BlendMode::SrcOver);
    canvas.draw_path(&draw.path.lock(), &over);
    recorder.finish_recording_as_picture(None)
}

/// Whether `canvas` maps the recording's device pixels onto its own whole:
/// by a translation of whole pixels, under which an image sampled at its
/// texels' centres lands on the pixels it was made for.
fn on_whole_pixels(canvas: &SkCanvas) -> bool {
    maps_whole_pixels(&canvas.local_to_device_as_3x3())
}

/// Whether `device` is a translation of whole pixels -- see
/// [`on_whole_pixels`].
fn maps_whole_pixels(device: &Matrix) -> bool {
    device.is_translate()
        && [device.translate_x(), device.translate_y()]
            .iter()
            .all(|t| t.fract() == 0.0)
}

impl GradientDraw {
    fn playback(&self, canvas: &SkCanvas, target: Target) {
        self.played(canvas, target, None);
    }

    /// The paths among `draws` whose effect a GPU canvas makes where the
    /// page is played, by index -- see [`atlas`].
    fn paths(&self) -> impl Iterator<Item = Option<&GradientPath>> {
        self.draws.iter().map(|piece| match &piece.gpu {
            OnGpu::Path(draw) => Some(draw.as_ref()),
            _ => None,
        })
    }

    /// Plays the run onto `canvas`, taking its effects and their images
    /// from what an atlas `found`, one for each draw, where one for more
    /// than this run was made -- see [`play_entries`] -- and from one of its
    /// own where not.
    fn played(
        &self,
        canvas: &SkCanvas,
        target: Target,
        found: Option<&[Option<Found>]>,
    ) {
        canvas.save();
        if let Some(clip) = self.clip.as_ref().and_then(Path::deserialize) {
            canvas.clip_path(&clip, ClipOp::Intersect, true);
        }
        let gpu = target == Target::Pixels && blur_mask::on_gpu(canvas);
        let own;
        let found = match found {
            Some(found) => found,
            None if gpu && on_whole_pixels(canvas) => {
                own = atlas(canvas, &self.paths().collect::<Vec<_>>());
                &own
            }
            None => &[],
        };
        let found_for =
            |index: usize| found.get(index).and_then(Option::as_ref);
        let mut batch = exact_composite::Batch::default();
        let mut index = 0;
        while index < self.draws.len() {
            // Draws one after another that the CPU makes over the GPU's
            // pixels share one read back -- see `drawn_on_their_backdrop`.
            if target == Target::Pixels && gpu {
                let span: Vec<(Cow<'_, Picture>, bool)> = self.draws[index..]
                    .iter()
                    .enumerate()
                    .map_while(|(k, piece)| match &piece.gpu {
                        OnGpu::Raster { beyond, .. } => {
                            Some((piece.pixels.picture()?, *beyond))
                        }
                        // A path the effect cannot reproduce: as the atlas
                        // found, or asked of the effect itself, which keeps
                        // its answer, where no atlas was made.
                        OnGpu::Path(draw)
                            if found_for(index + k).map_or_else(
                                || {
                                    draw.spec
                                        .exact(&draw.page, draw.alpha)
                                        .is_none()
                                },
                                |found| found.effect.is_none(),
                            ) =>
                        {
                            Some((piece.pixels.picture()?, draw.beyond))
                        }
                        _ => None,
                    })
                    .collect();
                let plays: Vec<_> = span
                    .iter()
                    .map(|(recorded, _)| {
                        move |raster: &SkCanvas| recorded.playback(raster)
                    })
                    .collect();
                let drawn: Vec<BackdropDraw<'_>> = zip(&span, &plays)
                    .map(|((recorded, beyond), play)| BackdropDraw {
                        reach: match beyond {
                            true => BackdropReach::Beyond,
                            false => BackdropReach::Local(recorded.cull_rect()),
                        },
                        draw: play,
                        clip: None,
                    })
                    .collect();
                if drawn.len() > 1 {
                    batch.flush(canvas);
                }
                if drawn.len() > 1
                    && Format::of(canvas).is_some_and(|format| {
                        drawn_on_their_backdrop(canvas, &drawn, &format)
                    })
                {
                    index += span.len();
                    continue;
                }
            }
            let piece = &self.draws[index];
            match (target, gpu) {
                (Target::Document, _) => match &piece.document {
                    Some(document) => document.playback(canvas),
                    None => piece.pixels.playback(canvas),
                },
                (Target::Pixels, false) => piece.pixels.playback(canvas),
                (Target::Pixels, true) => played_on_gpu(
                    canvas,
                    &piece.pixels,
                    &piece.gpu,
                    found_for(index),
                    &mut batch,
                ),
            }
            index += 1;
        }
        batch.flush(canvas);
        canvas.restore();
    }

    /// Whether a GPU canvas playing the run under `device` draws any of it
    /// on the CPU over the pixels it reads -- see [`played_on_gpu`]: a draw
    /// made on the CPU always; a path whose effect cannot be made; and,
    /// where `device` does not map the recording's device pixels onto whole
    /// ones, a path or a draw recorded with an image of the effect, which
    /// would be resampled there.
    pub(crate) fn reads_the_page(&self, device: &Matrix) -> bool {
        let whole = maps_whole_pixels(device);
        self.draws.iter().any(|piece| match &piece.gpu {
            OnGpu::Raster { .. } => true,
            OnGpu::Path(draw) => {
                !whole || draw.spec.exact(&draw.page, draw.alpha).is_none()
            }
            OnGpu::Effect { .. } => !whole,
            OnGpu::Alone | OnGpu::Native | OnGpu::Image(_) => false,
        })
    }

    /// What the run reaches, under the transform it is played back under:
    /// what its pixels recordings reach.
    pub(crate) fn reach(&self) -> Rect {
        self.draws
            .iter()
            .fold(Rect::new_empty(), |mut reach, piece| {
                reach.join(piece.pixels.cull_rect());
                reach
            })
    }

    /// Whether this stands for a pair of recordings -- see [`holds_a_pair`].
    pub(crate) fn is_pair(&self) -> bool {
        self.draws.iter().any(|piece| piece.document.is_some())
    }
}

impl PatternDraw {
    fn playback(&self, canvas: &SkCanvas, target: Target) {
        canvas.save();
        if let Some(clip) = self.clip.as_ref().and_then(Path::deserialize) {
            canvas.clip_path(&clip, ClipOp::Intersect, true);
        }
        match target {
            Target::Pixels => self.paint_onto(canvas, target),
            // Asked by `tiles_exactly`: whether the tile lands whole here,
            // drawing nothing.
            Target::Document if self.aligned && TILE_PROBE.get().is_some() => {
                if !self.lands_whole_on(canvas) {
                    TILE_PROBE.set(Some(false));
                }
            }
            Target::Document if self.aligned => {
                self.paint_onto(canvas, target);
            }
            Target::Document => self.plain.playback(canvas),
        }
        canvas.restore();
    }

    /// Whether the pattern lands each tile on whole device pixels drawn on
    /// `canvas`, under the transform it carries, the export's density among
    /// it -- see [`TileSource::lands_whole`].
    fn lands_whole_on(&self, canvas: &SkCanvas) -> bool {
        let to_device = Matrix::concat(
            &Matrix::concat(&canvas.local_to_device_as_3x3(), &self.matrix),
            &self.pattern_matrix,
        );
        self.source.lands_whole(&to_device, self.repeat)
    }

    /// Paints the pattern onto `canvas` where the draw covers it, from the
    /// tile as `target` takes it: as the page composites it on pixels, and
    /// in eight-bit sRGB in a document.
    ///
    /// The tile is rasterized at the scale `canvas` maps the page by, which
    /// in a PDF includes the export density: Skia scales a PDF page's
    /// canvas by it and undoes the scale in the device.
    fn paint_onto(&self, canvas: &SkCanvas, target: Target) {
        let to_draw =
            Matrix::concat(&canvas.local_to_device_as_3x3(), &self.matrix);
        let density = self
            .source
            .density_for(&Matrix::concat(&to_draw, &self.pattern_matrix));
        let Some(image) = self.source.raster(canvas, density) else {
            return;
        };
        let mut paint = self.paint.clone();
        let image = match target {
            Target::Pixels => image,
            Target::Document => {
                // UPSTREAM: PDFium 153 -- unfiled -- worked around
                // Re-check: leave the alpha on the paint, export a pattern
                // fill at a `globalAlpha` of 0.6 as PDF tiling, and render
                // it with pypdfium2 against the canvas.
                // The draw's alpha goes into the tile and the pattern is
                // painted at full alpha: PDFium paints a pattern fill at the
                // alpha of the pattern cell's own graphics state, which Skia
                // writes as 1, where poppler and CoreGraphics take the
                // fill's, and so drew a `globalAlpha` of 0.6 up to 84
                // levels from the canvas.
                let alpha = paint.alpha_f();
                paint.set_alpha_f(1.0);
                match in_document_space(&image, alpha) {
                    Some(image) => image,
                    None => return,
                }
            }
        };
        // The raster back to the page's own size, which its pixels cover.
        let size = self.source.page.bounds.size();
        let to_page = Matrix::scale((
            size.width / image.width() as f32,
            size.height / image.height() as f32,
        ));
        let Some(shader) = image.to_shader(
            self.repeat,
            SamplingOptions::from(FilterMode::Linear),
            &Matrix::concat(&self.pattern_matrix, &to_page),
        ) else {
            return;
        };
        paint.set_shader(shader);
        self.replay.draw(canvas, &self.matrix, &paint);
    }

    /// What the draw covers, under the transform it is played under.
    fn reach(&self) -> Rect {
        self.plain.cull_rect()
    }

    /// Whether the draw is a run of text drawn under a blend read in the
    /// shader -- see [`blends_by_coefficients`] -- which a GPU draws apart
    /// where its glyphs overlap, as it does a [`ReplayedText`]'s.
    pub(crate) fn reads_the_page(&self) -> bool {
        matches!(self.replay, Replay::Text { .. })
            && !blends_by_coefficients(
                self.paint.blend_mode_or(BlendMode::SrcOver),
            )
    }

    /// What the draw inks on a canvas whose transform is `device`, within
    /// `clip`, in that canvas's pixels, as [`ReplayedText::ink_under`]
    /// finds it: its replay recorded under `device`, the pattern left out,
    /// which inks nothing the draw's shape does not, and widened by
    /// [`HINTED_INK_DRIFT`]. Its own clip, where it has one, is left out
    /// too, which only widens it.
    fn ink_under(&self, device: &Matrix, clip: IRect) -> IRect {
        let _whole = WholeRuns::hold();
        let inked = inked_by(Rect::from(clip), device, |canvas| {
            self.replay.draw(canvas, &self.matrix, &self.paint);
        });
        let inked = inked.with_outset((HINTED_INK_DRIFT, HINTED_INK_DRIFT));
        IRect::intersect(&inked.round_out(), &clip).unwrap_or_default()
    }
}

thread_local! {
    /// Whether a [`tiles_exactly`] probe is under way on this thread, and
    /// what it has found: `Some(false)` once a pattern's tiles would not
    /// land whole.
    static TILE_PROBE: Cell<Option<bool>> = const { Cell::new(None) };
}

/// Whether every canvas pattern `entry` paints from its tile in a document
/// lands each tile on whole device pixels played on `canvas`, under the
/// transform and density it carries -- see [`PatternDraw::aligned`]. Where
/// one does not, the document embeds the entry as the page composites it.
///
/// Decided by playing `entry` for a document onto a recording under that
/// transform, each such pattern answering for itself and drawing nothing:
/// a pattern inside a canvas drawn into another lands where that canvas's
/// transform puts it, which a recorded draw cannot know -- a 10-pixel tile
/// drawn in under a scale of 1.5 steps 15 pixels -- and the density, and so
/// whether its tile has to be lowered to fit, is the export's.
pub(crate) fn tiles_exactly(entry: &Layer, canvas: &SkCanvas) -> bool {
    let Some(clip) = canvas.device_clip_bounds() else {
        return true; // nothing of it is drawn
    };
    let mut recorder = PictureRecorder::new();
    let probe = recorder.begin_recording(Rect::from(clip), false);
    probe.set_matrix(&canvas.local_to_device());
    let outer = TILE_PROBE.replace(Some(true));
    entry.playback(probe, Target::Document);
    TILE_PROBE.replace(outer) != Some(false)
}

/// `image` at `alpha`, in eight-bit sRGB, the space a document reads an
/// image in, each channel of a colour outside sRGB's gamut clamped -- as
/// `Page::embed_raster` converts the run it embeds. `None` where no surface
/// could be made.
pub(crate) fn in_document_space(image: &Image, alpha: f32) -> Option<Image> {
    let info = image
        .image_info()
        .with_color_type(ColorType::RGBA8888)
        .with_color_space(ColorSpace::new_srgb());
    let mut surface = started::raster(&info, None, None)?;
    let mut copy = Paint::default();
    copy.set_blend_mode(BlendMode::Src).set_alpha_f(alpha);
    surface.canvas().draw_image(image, (0, 0), Some(&copy));
    Some(surface.image_snapshot())
}

/// What a page is played back onto, which decides how a [`Group`]
/// composites.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// Pixels: a raster or GPU surface, a picture Skia itself will draw as
    /// pixels -- a pattern's or an image's -- or the raster a document embeds
    /// for what it cannot write as vectors, which is composited in the
    /// page's space as the page is -- see `Page::embed_raster`.
    Pixels,
    /// A document backend writing vectors: PDF, and SVG for what it does not
    /// rasterize.
    Document,
}

/// What a `saveLayer` is opened with: the paint it composites through, the
/// bounds it is confined to, and the filter it applies to what lies beneath.
#[derive(Clone, Debug, Default)]
pub struct LayerRec {
    pub paint: Option<Paint>,
    pub bounds: Option<Rect>,
    pub backdrop: Option<ImageFilter>,
    /// The paint a page's pixels open the layer with in place of `paint`,
    /// where a CanvasKit colour filter in it is built for those pixels --
    /// see `Context2D::pixels_paint`.
    pub pixels_paint: Option<Paint>,
    /// The backdrop a page's pixels open the layer with in place of
    /// `backdrop`, built for those pixels -- see `Context2D::pixels_onto`.
    pub pixels_backdrop: Option<ImageFilter>,
    /// Whether the layer holds a run of text painted as one image, which
    /// `paint` composites -- see `Context2D::draw_text` -- and which a GPU
    /// canvas composites on the CPU, as the CPU canvas does -- see
    /// [`KeptRun::Image`].
    pub one_image: bool,
}

impl LayerRec {
    /// Whether the layer composites through a blend that changes the page
    /// beyond what it holds -- see `covers_beyond_the_draw` -- on either
    /// target.
    fn covers_beyond(&self) -> bool {
        [&self.pixels_paint, &self.paint]
            .into_iter()
            .flatten()
            .filter_map(Paint::as_blend_mode)
            .any(covers_beyond_the_draw)
    }

    /// Opens the layer on `canvas`, under whatever transform and clip it
    /// carries, compositing as `target` needs: on pixels through the paint
    /// [`composited`] hands over, which keeps the alpha exact; in a
    /// document through the paint as it is, which the backend writes as an
    /// opacity and keeps vector. Confined to `within` where that is `Some`:
    /// a rectangle in device pixels -- see [`Self::footprint`].
    fn open_on(&self, canvas: &SkCanvas, target: Target, within: Option<Rect>) {
        let own = match target {
            Target::Pixels => {
                self.pixels_paint.as_ref().or(self.paint.as_ref())
            }
            Target::Document => self.paint.as_ref(),
        };
        let paint = match (own, target) {
            (Some(paint), Target::Pixels) => Some(composited(paint)),
            (Some(paint), Target::Document) => Some(Cow::Borrowed(paint)),
            (None, _) => None,
        };
        // Skia maps a layer's bounds through the transform it opens under,
        // to the device rectangle around them, so `within` is taken back
        // through it first. A transform with no inverse leaves the layer
        // unconfined, and so does one that turns a layer with bounds of its
        // own: Skia covers the device rectangle around the turned bounds,
        // which reaches past them at the corners, and the rectangle around
        // their intersection with `within` would cut those corners off.
        let matrix = canvas.local_to_device_as_3x3();
        let within = within
            .filter(|_| self.bounds.is_none() || matrix.rect_stays_rect())
            .and_then(|within| {
                let within = matrix.invert()?.map_rect(within).0;
                Some(match self.bounds {
                    Some(mut bounds) => match bounds.intersect(within) {
                        true => bounds,
                        false => Rect::new_empty(),
                    },
                    None => within,
                })
            });
        let mut rec = SaveLayerRec::default();
        if let Some(paint) = &paint {
            rec = rec.paint(paint);
        }
        if let Some(bounds) = within.as_ref().or(self.bounds.as_ref()) {
            rec = rec.bounds(bounds);
        }
        let backdrop = match target {
            Target::Pixels => {
                self.pixels_backdrop.as_ref().or(self.backdrop.as_ref())
            }
            Target::Document => self.backdrop.as_ref(),
        };
        if let Some(backdrop) = backdrop {
            rec = rec.backdrop(backdrop);
        }
        canvas.save_layer(&rec);
    }

    /// The rectangle the layer can be confined to without changing what it
    /// composites, in the coordinates its contents are drawn in: `drawn`,
    /// the rectangle they draw in, where what the layer leaves transparent
    /// composites as nothing. `None` where the layer has to cover what the
    /// clip allows, or `drawn` is not known.
    ///
    /// Unconfined, Skia makes a layer the size of the clip -- the whole page,
    /// for a layer opened on it -- and blends every pixel of it back on
    /// closing, whatever the layer holds: a 10x10 fill in a layer on an
    /// 800x600 page is the blend of 480,000 pixels each time the layer is
    /// drawn. Confining it changes no pixel only where a transparent pixel
    /// of the layer leaves the page as it was: composited source-over,
    /// through nothing but an alpha, with no filter over what lies beneath
    /// it. A colour filter can make a transparent pixel opaque and an image
    /// filter can draw past what it is given, so either leaves the layer
    /// covering the clip, as do a backdrop and any other blend. Skia widens
    /// such a layer past bounds it is given as well -- each of those five
    /// composites a square the same confined to it as not -- but the rule
    /// here does not rest on that. The paint judged is the layer's own:
    /// [`composited`] trades its alpha for a colour filter that leaves a
    /// transparent pixel transparent.
    pub(crate) fn footprint(&self, drawn: Option<Rect>) -> Option<Rect> {
        self.confines().then_some(drawn).flatten()
    }

    /// Whether the layer can be confined to what it holds draws in -- see
    /// [`Self::footprint`].
    fn confines(&self) -> bool {
        let plain = self.paint.as_ref().is_none_or(|paint| {
            paint.image_filter().is_none()
                && paint.color_filter().is_none()
                && paint.as_blend_mode() == Some(BlendMode::SrcOver)
        });
        plain && self.backdrop.is_none()
    }

    /// Whether a layer holding nothing still changes the page: through a
    /// filter over what lies beneath it, or a blend mode that replaces what
    /// it covers with the nothing it holds.
    pub fn marks_when_empty(&self) -> bool {
        let blend = self.paint.as_ref().and_then(Paint::as_blend_mode);
        self.backdrop.is_some()
            || !matches!(blend, None | Some(BlendMode::SrcOver))
    }

    /// What compositing the layer onto what lies beneath it asks of a
    /// vector backend: its paint's blend -- see [`blend_features`] -- with
    /// what it holds taken for translucent, since a layer may hold anything.
    /// A blender that is no blend mode, which no document can write, asks
    /// for a blend it cannot match.
    pub(crate) fn vector_features(&self) -> VectorFeatures {
        let Some(paint) = &self.paint else {
            return VectorFeatures::PLAIN;
        };
        match paint.as_blend_mode() {
            Some(mode) => blend_features(mode, false),
            None => {
                VectorFeatures::BLEND_MODE.with(VectorFeatures::UNMATCHED_BLEND)
            }
        }
    }

    /// The opacity the layer composites what it holds at, where that is
    /// all it does: over no backdrop, through no paint or one that is an
    /// alpha under `source-over` with no shader or filter. `None` for any
    /// other layer. Its bounds still confine what it holds.
    pub(crate) fn opacity(&self) -> Option<f32> {
        if self.backdrop.is_some() {
            return None;
        }
        let Some(paint) = &self.paint else {
            return Some(1.0);
        };
        (paint.as_blend_mode() == Some(BlendMode::SrcOver)
            && paint.shader().is_none()
            && paint.color_filter().is_none()
            && paint.image_filter().is_none()
            && paint.mask_filter().is_none())
        .then(|| paint.alpha_f())
    }
}

/// How a [`Group`] composites onto what lies beneath it.
#[derive(Clone, Debug)]
pub enum Composite {
    /// Through a `saveLayer` opened with this.
    Layer(LayerRec),
    /// Through a filter chain the crate runs on the group's pixels itself
    /// -- see [`chain`] -- and `rec`'s paint, less its image filter, which
    /// is the chain as Skia would run it. A document opens `rec` as a layer,
    /// filter and all.
    ///
    /// `reach` is what a run of text the group holds can ink, in the units
    /// its entries are played under, at the scale they were recorded at --
    /// see `Context2D::files_through_chain` -- which bounds the chain's
    /// layer, widened for hinting at the scale it is played at -- see
    /// [`hinted_reach`]. `None` for any other group, which its entries'
    /// reach bounds.
    ///
    /// `coverage` is the run again in its colour opaque, where the run is
    /// one [`coverage_files`] passes, for a page on eight bits to blur it or
    /// cast its shadow from -- see [`CoverageRun`]. `None` for any other
    /// group.
    Chain {
        steps: Arc<[Step]>,
        rec: LayerRec,
        reach: Option<Rect>,
        coverage: Option<Arc<CoverageRun>>,
    },
}

/// A run of text in one flat colour under a sole `blur()`, drawn in that
/// colour opaque -- recorded a second time, or the picture of its pixels
/// where that is the paint they are drawn in -- so that on a page the
/// coverage route plays on its blur is made from its coverage alone -- see
/// [`chain::coverage`] -- and on any other eight-bit page, on either
/// engine, the chain blurs the copy at the run's alpha rather than the run
/// rounded to a byte. A translucent run under a sole `drop-shadow()` is
/// recorded so too, for its shadow to be cast from its coverage on an
/// eight-bit page, on either engine -- see [`chain::Cast`].
///
/// Opaque because Skia pre-blends a glyph's mask by the paint's colour and
/// never by its alpha: `SkScalerContextRec::setLuminanceColor` keeps only
/// the red, green and blue (`skia/src/core/SkScalerContext.cpp` under the
/// skia-bindings crate). The copy covers what the run
/// covers, and the alpha is applied once, when the blur is coloured, rather
/// than rounded into each edge pixel before the blur and again after it. The
/// copy is drawn through the paint the run is, its alpha set to one, so that
/// a colour painted through [`ExactColour::shader`] -- which Skia pre-blends
/// as grey, a shader naming no colour (`SkPaintPriv::ComputeLuminanceColor`)
/// -- is covered as the run is.
#[derive(Debug)]
pub struct CoverageRun {
    /// The run, drawn opaque: its own picture, or the one of its pixels.
    pub picture: CoveragePicture,
    /// The run's colour, held exactly in the page's space.
    pub exact: ExactColour,
    /// The alpha the run was drawn at.
    pub alpha: f32,
    /// The kept run this draws and where, for the run's blur to be kept --
    /// see [`run_mask`] -- or for a shadow cast from this copy to be kept by
    /// where the run lies -- see [`chain::Cast`]. `None` the first time a
    /// run is drawn, which is not kept.
    pub kept: Option<Kept>,
}

impl Composite {
    /// The rectangle the group can be confined to -- see
    /// [`LayerRec::footprint`] -- the rectangle its entries draw in, which
    /// `drawn` answers, where its layer confines. Asked only
    /// where the layer can be confined: answering it plays nothing, but an
    /// entry recorded when first played -- see [`DeferredPicture`] -- is
    /// recorded to answer, and a chain's layer, under its image filter,
    /// never is confined.
    fn footprint(&self, drawn: impl FnOnce() -> Option<Rect>) -> Option<Rect> {
        match self {
            Composite::Layer(rec) | Composite::Chain { rec, .. } => {
                rec.confines().then(drawn).flatten()
            }
        }
    }
}

/// Entries composited as one through a paint: what a `saveLayer` held when
/// it closed, or a canvas drawn into another through a paint.
///
/// Kept as ours rather than recorded into a picture, because how the group
/// composites depends on where it is played back -- see
/// [`LayerRec::open_on`] -- and once in a picture only Skia plays it.
#[derive(Clone, Debug)]
pub struct Group {
    /// How the group composites onto what lies beneath it.
    pub composite: Composite,
    /// The transform and the clip the group composites under, in page
    /// coordinates; the clip is in device space, as the recorder holds it,
    /// and serialized, as a [`BlurOp`]'s is, so that a page can be sent to
    /// another thread.
    pub matrix: Matrix,
    pub clip: Option<Data>,
    /// Whether every entry was recorded under `clip` already, as what a
    /// layer holds is. A document opening the group -- see
    /// [`Self::open_in_a_document`] -- clips by it only where not: clipped
    /// twice by one path, an SVG writes two clip paths, and a renderer
    /// takes their edges' coverage twice.
    pub entries_clipped: bool,
    /// For a canvas or a vector image drawn in, its own bounds, in the
    /// coordinates its entries are played under -- see [`DrawnIn`]. `None`
    /// for any other group.
    pub source: Option<Rect>,
    /// A rectangle under `matrix` that what the group draws is confined to:
    /// the destination a canvas is drawn into. It bounds what is drawn and
    /// not what a filter makes of it, which spreads past it as a blur of
    /// any other draw spreads past the draw.
    pub within: Option<Rect>,
    /// The transform the entries are played under: the identity for what a
    /// layer held, recorded in page coordinates; for a canvas drawn in, the
    /// mapping from its page onto this one.
    pub contents_matrix: Matrix,
    /// What the group holds, each entry with what it asks of a vector
    /// backend.
    pub(crate) contents: Entries,
}

/// Where a canvas or a vector image drawn into a page lands: inside
/// `within`, its destination, under the transform it is drawn under; and
/// what it lands is the pixels it holds -- its draws inside `source`, its own
/// bounds in the coordinates they are played under, rasterized there as
/// they are on it, and cut by nothing but its own edges -- see
/// [`Group::played_at_source`].
#[derive(Clone, Copy, Debug)]
pub(crate) struct DrawnIn {
    pub(crate) within: Rect,
    pub(crate) source: Rect,
}

/// Entries played in order, each with what it asks of a vector backend: the
/// page's own, or a [`Group`]'s, which a document can then take entry by
/// entry rather than as the group as a whole.
///
/// Shared, so that a canvas drawn in brings its page's lists without copying
/// them.
#[derive(Clone, Debug, Default)]
pub(crate) struct Entries {
    pub(crate) layers: Arc<Vec<Layer>>,
    /// One for each of `layers`, in the same order.
    pub(crate) features: Arc<Vec<VectorFeatures>>,
    /// Whether these are the entries of a page that keeps a recording for
    /// each kind of target, whose draws may not yet have forked them --
    /// see `page::Recorders` -- and so hold no pair to say so.
    pub(crate) apart: bool,
    /// The images these entries draw that no picture of theirs counts and
    /// no entry among them says it holds: a canvas a document of them
    /// writes as its image, past `DOCUMENT_REPLAY_BUDGET` -- the image a
    /// [`Split`] holds and its document draws too. Carried by what a
    /// document plays of a page, which stands in for the split, so that a
    /// page holding those entries counts the images, and so that a flatten
    /// can tell what keeping them costs -- see `page::PageRecorder::flatten`.
    pub(crate) held: Arc<[HeldImage]>,
}

impl FromIterator<(Layer, VectorFeatures)> for Entries {
    fn from_iter<I: IntoIterator<Item = (Layer, VectorFeatures)>>(
        entries: I,
    ) -> Self {
        let (layers, features) = entries.into_iter().unzip();
        Self::new(layers, features)
    }
}

impl Entries {
    /// `layers`, each asking for `features` at the same index.
    pub(crate) fn new(
        layers: Vec<Layer>,
        features: Vec<VectorFeatures>,
    ) -> Self {
        Self::shared(Arc::new(layers), Arc::new(features))
    }

    /// [`Self::new`] for lists already shared.
    pub(crate) fn shared(
        layers: Arc<Vec<Layer>>,
        features: Arc<Vec<VectorFeatures>>,
    ) -> Self {
        debug_assert_eq!(layers.len(), features.len(), "a feature per entry");
        Self {
            layers,
            features,
            apart: false,
            held: Arc::default(),
        }
    }

    /// Calls `each` with every image these entries hold that their
    /// pictures do not count -- see [`Self::held`] and [`Split::image`] --
    /// once for each time they hold it.
    pub(crate) fn held_images(&self, each: &mut dyn FnMut(HeldImage)) {
        self.held.iter().for_each(|held| each(*held));
        self.layers.iter().for_each(|layer| layer.held_images(each));
    }

    /// Adds `next`'s entries after these, with the images it holds.
    pub(crate) fn append(&mut self, next: Entries) {
        Arc::make_mut(&mut self.layers).extend(next.layers.iter().cloned());
        Arc::make_mut(&mut self.features).extend(next.features.iter().copied());
        if !next.held.is_empty() {
            self.held =
                self.held.iter().chain(next.held.iter()).copied().collect();
        }
    }

    /// `layers` asking nothing of a vector backend.
    pub(crate) fn plain(layers: Vec<Layer>) -> Self {
        let features = vec![VectorFeatures::PLAIN; layers.len()];
        Self::new(layers, features)
    }

    /// Every entry, each with what it asks of a vector backend.
    pub(crate) fn iter(
        &self,
    ) -> impl Iterator<Item = (&Layer, VectorFeatures)> {
        self.layers.iter().zip(self.features.iter().copied())
    }

    /// Everything the entries ask of a vector backend, together.
    pub(crate) fn all_features(&self) -> VectorFeatures {
        self.features
            .iter()
            .fold(VectorFeatures::PLAIN, |all, features| all.with(*features))
    }

    /// What a document plays of these entries, as entries of its own: each
    /// [`Split`] stands in its place as its document's entries, the images
    /// its document holds joining these entries' own -- see [`Self::held`]
    /// -- and each group holding one as a group of what its contents
    /// project to. `None`
    /// where nothing would change, so that entries holding no split are
    /// played as they are and shared rather than copied.
    ///
    /// A page drawn into another as an image of its own pixels is written
    /// by a document from these: what the page drew, as vectors and text,
    /// and none of the pixels a split plays for a raster.
    pub(crate) fn projected(&self) -> Option<Entries> {
        let mut changed = false;
        let mut layers = Vec::with_capacity(self.layers.len());
        let mut features = Vec::with_capacity(self.features.len());
        let mut held = self.held.to_vec();
        for (layer, asks) in self.iter() {
            match layer {
                // A split's document holds none of its own: it is drawn from
                // what another page projected, or from an image.
                Layer::Split(split) => {
                    changed = true;
                    layers.extend(split.document.layers.iter().cloned());
                    features.extend(split.document.features.iter().copied());
                    held.extend(split.document.held.iter().copied());
                }
                Layer::Group(group) => match group.contents.projected() {
                    Some(contents) => {
                        changed = true;
                        layers.push(Layer::Group(Arc::new(Group {
                            contents,
                            ..Group::clone(group)
                        })));
                        features.push(asks);
                    }
                    None => {
                        layers.push(layer.clone());
                        features.push(asks);
                    }
                },
                _ => {
                    layers.push(layer.clone());
                    features.push(asks);
                }
            }
        }
        changed.then(|| Entries {
            apart: self.apart,
            held: held.into(),
            ..Entries::new(layers, features)
        })
    }

    /// The entries recorded as one picture of `bounds`, as `target` plays
    /// them: what a draw of them needs where it is not drawn entry by entry
    /// -- see `Context2D::draw_picture_costing`.
    pub(crate) fn picture(
        &self,
        bounds: Rect,
        target: Target,
    ) -> Option<Picture> {
        let mut recorder = PictureRecorder::new();
        let canvas = recorder.begin_recording(bounds, true);
        play_entries(&self.layers, &self.features, canvas, target);
        recorder.finish_recording_as_picture(None)
    }
}

/// One draw filed as entries of its own for each kind of target: what a
/// page plays for pixels, and what a document backend writes.
///
/// A canvas drawn into another as an image is its own pixels on pixels --
/// see `PageRecorder::image` -- and the entries it was drawn from in a
/// document, which writes them as vectors and text. Each half is what the
/// draw files on its own, so each plays as that draw would alone: a
/// document takes its half entry by entry, as it takes a group's.
#[derive(Clone, Debug)]
pub struct Split {
    /// What the draw files for pixels.
    pub(crate) pixels: Entries,
    /// What the draw files for a document.
    pub(crate) document: Entries,
    /// The image the draw holds that no other object reports: a canvas's
    /// own pixels, which its pictures keep alive and do not count -- see
    /// [`Layer::recorded_bytes`]. A document past the replay budget draws
    /// it too, and says so in its own [`Entries::held`], which outlives
    /// the split wherever a document stands in for it.
    pub(crate) image: Option<HeldImage>,
}

impl Split {
    /// Whether a GPU canvas plays the split apart from a picture -- see
    /// [`Layer::plays_apart_on_gpu`]: only its pixels are played there.
    pub(crate) fn plays_apart_on_gpu(&self) -> bool {
        self.pixels
            .iter()
            .any(|(entry, _)| entry.plays_apart_on_gpu())
    }

    /// Calls `each` with every image the split holds that its pictures do
    /// not count -- see [`Self::image`] and [`Entries::held`] -- once for
    /// each time it holds it.
    pub(crate) fn held_images(&self, each: &mut dyn FnMut(HeldImage)) {
        if let Some(held) = self.image {
            each(held);
        }
        self.pixels.held_images(each);
        self.document.held_images(each);
    }

    /// Adds `next`'s entries after this split's own, each half to its
    /// half, where the two can stand as one: they hold the same image, and
    /// each half plays on the targets the other's does. Played, the joined
    /// split draws what the two drew one after the other, since each half
    /// is a list played in order. `Err` gives `next` back where they cannot.
    pub(crate) fn join(&mut self, next: Split) -> Result<(), Split> {
        if self.image != next.image
            || self.pixels.apart != next.pixels.apart
            || self.document.apart != next.document.apart
        {
            return Err(next);
        }
        self.pixels.append(next.pixels);
        self.document.append(next.document);
        Ok(())
    }
}

/// An eraser -- a `putImageData`, or a `clearRect` that does not clear the
/// whole page -- filed with the region it erases, so that a document can
/// write what it does without writing its `BlendMode::Clear`.
///
/// Played on every target as `entry`, the eraser as it was recorded: its
/// clear and, for a put, the put's pixels. A PDF writes that clear as it
/// writes a blend. Skia's SVG device writes no blend, so the clear would
/// erase nothing and the page beneath a put would show through it: an SVG
/// writes `written` instead, and clips what was drawn before the eraser in
/// the same list to lie outside `region` -- see `Page::play_as_document`
/// in `context::page`.
#[derive(Clone, Debug)]
pub struct Erase {
    /// The eraser as recorded, played as it is on every target but an SVG.
    pub(crate) entry: Layer,
    /// What the eraser erases, in the space of the list it is filed in: a
    /// put's destination, which a put writes untransformed and unclipped,
    /// or a `clearRect`'s rectangle where the transform put it, within the
    /// clip. Serialized, as [`Clipped`] holds its clip, since a [`Path`]
    /// is not `Sync`.
    pub(crate) region: Data,
    /// What an SVG writes in the eraser's place: a put's pixels drawn over
    /// nothing, `None` for a `clearRect`, which leaves nothing.
    pub(crate) written: Option<PutPixels>,
}

/// A put's pixels, as an SVG writes them in the put's place -- see
/// [`Erase`]: the region `src` of `image` drawn untransformed onto `dst`.
/// Kept as the image a put draws rather than recorded, so that only an SVG
/// export pays for writing it.
#[derive(Clone, Debug)]
pub(crate) struct PutPixels {
    /// The put's pixels: the image the eraser's entry draws, which the page
    /// counts as held -- see [`Layer::held_images`].
    pub(crate) image: Image,
    /// The region of `image` the put writes.
    pub(crate) src: Rect,
    /// Where the put writes it, in the page's space.
    pub(crate) dst: Rect,
}

impl PutPixels {
    /// The put's pixels drawn onto its destination over nothing.
    pub(crate) fn draw(&self, canvas: &SkCanvas) {
        canvas.draw_image_rect(
            &self.image,
            Some((&self.src, SrcRectConstraint::Strict)),
            self.dst,
            &Paint::default(),
        );
    }
}

/// An image a page's recording holds, by Skia's identity for it and the
/// bytes of its pixels, so that one image drawn many times counts once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HeldImage {
    /// `SkImage::uniqueID`, which no other image alive shares.
    pub(crate) id: u32,
    /// The bytes its pixels take, at the rows Skia keeps them in.
    pub(crate) bytes: usize,
}

impl HeldImage {
    /// `image` as a recording holding it counts it: `None` for an image
    /// whose pixels are not in main memory, which this library makes none
    /// of for a recording to hold.
    pub(crate) fn of(image: &Image) -> Option<Self> {
        let pixmap = image.peek_pixels()?;
        Some(Self {
            id: image.unique_id(),
            bytes: pixmap.compute_byte_size(),
        })
    }
}

impl Split {
    /// Plays the half `target` takes.
    fn playback(&self, canvas: &SkCanvas, target: Target) {
        let half = match target {
            Target::Pixels => &self.pixels,
            Target::Document => &self.document,
        };
        play_entries(&half.layers, &half.features, canvas, target);
    }

    /// What the pixels reach -- see [`Layer::reach`]: every entry's
    /// rectangle joined, and `None` where one has none.
    fn pixels_reach(&self) -> Option<Rect> {
        self.pixels.layers.iter().try_fold(
            Rect::new_empty(),
            |mut reach, entry| {
                reach.join(entry.reach()?);
                Some(reach)
            },
        )
    }
}

impl Group {
    /// What the group's chain can draw on a canvas whose transform is
    /// `device`, in that canvas's pixels: what its entries draw -- the
    /// chain's own reach where it has one, as a run of text answers it, and
    /// what the entries reach otherwise -- through the group's transforms,
    /// widened by as far as its steps carry a pixel -- see
    /// [`chain::canvas_reach`] -- at the device's scale, and a pixel either
    /// side for anti-aliasing. Too wide only reads more of the page. `None`
    /// for a group that runs no chain, or whose entries reach what the clip
    /// allows.
    pub(crate) fn chain_reach_under(&self, device: &Matrix) -> Option<IRect> {
        let Composite::Chain { steps, reach, .. } = &self.composite else {
            return None;
        };
        let drawn = reach.or_else(|| reach_of(&self.contents.layers))?;
        let to_device = Matrix::concat(device, &self.contents_matrix);
        let scale = device
            .decompose_scale(None)
            .map_or(1.0, |scale| scale.width.max(scale.height));
        let spread = chain::canvas_reach(steps, &self.matrix) * scale;
        Some(
            to_device
                .map_rect(drawn)
                .0
                .with_outset((spread.x + 1.0, spread.y + 1.0))
                .round_out(),
        )
    }

    /// Confines `canvas` to the destination the group is drawn into --
    /// `within` -- which `to_device` maps to the canvas's pixels and the
    /// canvas carries as its transform when this is called.
    ///
    /// Applied as coverage where [`Self::crop`] says so, and a clip
    /// elsewhere.
    fn confine(&self, canvas: &SkCanvas, target: Target, to_device: &Matrix) {
        let Some(within) = &self.within else {
            return;
        };
        match self.crop(canvas, target, to_device) {
            Some(coverage) => {
                canvas.reset_matrix();
                coverage.clip(canvas);
                canvas.set_matrix(&(*to_device).into());
            }
            None => {
                canvas.clip_rect(within, ClipOp::Intersect, true);
            }
        }
    }

    /// The destination as coverage in the canvas's pixels, which `to_device`
    /// maps it to, where it cuts what the group draws on pixels: `None`
    /// where it is applied as a clip.
    ///
    /// A clip is what bounds a draw. On a raster on the CPU it cuts every
    /// path drawn across its edge, and Skia's analytic anti-aliasing moves
    /// the coverage of such a path's edges however far from the cut -- see
    /// [`CutOp`]: a triangle drawn in, cropped, parted from its source by up
    /// to 28 levels over 156 pixels more than two from the crop. On either
    /// engine it bounds what a blur inside the group reads, so that a blur
    /// near the crop lost what it spread in from past it: a pixel 4 inside
    /// a crop on the GPU at density 3 parted by 63. As coverage the draws
    /// are made whole and only then confined. A destination that holds what
    /// the group draws cuts nothing and stays a clip, as does every one in
    /// a document, which writes a clip by coverage as an empty `clipPath` --
    /// see [`clips_by_coverage`].
    fn crop(
        &self,
        canvas: &SkCanvas,
        target: Target,
        to_device: &Matrix,
    ) -> Option<Coverage> {
        let within = self.within.as_ref()?;
        let pixels = clips_by_coverage(canvas, target)
            || (target != Target::Document && blur_mask::draws_on(canvas));
        if !pixels || !self.crops() {
            return None;
        }
        Coverage::of(&Path::rect(within, None).make_transform(to_device))
    }

    /// Whether the destination cuts what the group draws: leaves part of it
    /// out, by the bounds its entries reach -- see [`reach_of`] -- or where
    /// those are not known.
    fn crops(&self) -> bool {
        let Some(within) = &self.within else {
            return false;
        };
        let destination = self.matrix.map_rect(within).0;
        match reach_of(&self.contents.layers) {
            Some(reach) => {
                let drawn = self.contents_matrix.map_rect(reach).0;
                !destination.contains(drawn)
            }
            None => true,
        }
    }

    /// The opacity the group composites what it holds at, where that is all
    /// it does -- see [`LayerRec::opacity`]. A document can take such a
    /// group entry by entry, writing what it can of them and embedding the
    /// rest as pixels, rather than whole: opened, at an opacity of 1, and
    /// in an SVG written as a group of its own at any other. `None` for a
    /// filter chain or any other layer, which a document takes whole.
    ///
    /// `None` too for a group holding a blend past source-over. A layer
    /// keeps what it holds from blending with what lies beneath it, so such
    /// a blend has to be composited in it: opened, a canvas drawn in holding
    /// a `destination-out` punched the page beneath it as well as itself,
    /// 215 levels from the canvas's pixels where a PDF wrote the blend. A
    /// blend the backend refuses is embedded with what lies beneath it
    /// before the group is reached -- see `Page::draw_as_document` -- and
    /// one it writes is written inside the group's layer. An eraser blends
    /// under `BlendMode::Clear` as any other blend does -- see
    /// [`VectorFeatures::ERASE`].
    pub(crate) fn opacity(&self) -> Option<f32> {
        self.opacity_clipping_erasers(false)
    }

    /// [`Self::opacity`], where `clipped` for a document that writes an
    /// eraser as what lies beneath it clipped away from what it erases --
    /// see [`Erase`] -- which punches nothing, so that a group holding an
    /// eraser and no other blend can be opened.
    pub(crate) fn opacity_clipping_erasers(
        &self,
        clipped: bool,
    ) -> Option<f32> {
        let blend = match clipped {
            true => VectorFeatures::BLEND_MODE,
            false => VectorFeatures::BLEND_MODE.with(VectorFeatures::ERASE),
        };
        let blends = self
            .contents
            .features
            .iter()
            .any(|features| features.refused_by(blend));
        match &self.composite {
            Composite::Layer(rec) if !blends => rec.opacity(),
            Composite::Layer(_) | Composite::Chain { .. } => None,
        }
    }

    /// Plays the group onto `canvas` as a document does a group it opens --
    /// see [`Self::opacity`]: under its clip, its `within` and
    /// its layer's bounds, its transforms composed as [`Self::playback`]
    /// composes them, with no layer around what `entries` plays of its
    /// contents.
    pub(crate) fn open_in_a_document(
        &self,
        canvas: &SkCanvas,
        entries: impl FnOnce(&SkCanvas, &Entries),
    ) {
        let base = canvas.local_to_device_as_3x3();
        canvas.save();
        if !self.entries_clipped
            && let Some(clip) = self.clip.as_ref().and_then(Path::deserialize)
        {
            canvas.clip_path(&clip, ClipOp::Intersect, true);
        }
        canvas.set_matrix(&Matrix::concat(&base, &self.matrix).into());
        if let Some(within) = &self.within {
            canvas.clip_rect(within, ClipOp::Intersect, true);
        }
        if let Composite::Layer(rec) | Composite::Chain { rec, .. } =
            &self.composite
            && let Some(bounds) = &rec.bounds
        {
            canvas.clip_rect(bounds, ClipOp::Intersect, true);
        }
        canvas.set_matrix(&Matrix::concat(&base, &self.contents_matrix).into());
        entries(canvas, &self.contents);
        canvas.restore();
    }

    /// Draws the group onto `canvas` for `target`, its transforms composed
    /// with the one the canvas carries: a recorded picture replays its
    /// matrices relative to the canvas it is played onto, and these have to
    /// as well, or a density or a tile's origin would be lost.
    ///
    /// On a surface with pixels the layer is confined to what its entries
    /// draw -- see [`LayerRec::footprint`]. Not in a recording: whether a
    /// layer with bounds of its own may be confined depends on the transform
    /// it is drawn under, and a recording is drawn later under one this
    /// cannot see. Not in a document either, which keeps its groups as they
    /// were.
    pub fn playback(self: &Arc<Self>, canvas: &SkCanvas, target: Target) {
        let pixels = target != Target::Document && blur_mask::draws_on(canvas);
        let within = pixels.then(|| {
            self.composite.footprint(|| reach_of(&self.contents.layers))
        });
        self.playback_within(canvas, target, within.flatten());
    }

    /// The rectangle the group can draw in, under the transform it is
    /// played back under: what its entries draw in, through the transform
    /// they are played under, where the layer can be confined to it. `None`
    /// where it covers what the clip allows.
    pub fn reach(&self) -> Option<Rect> {
        let drawn = self
            .composite
            .footprint(|| reach_of(&self.contents.layers))?;
        Some(self.contents_matrix.map_rect(drawn).0)
    }

    /// The steps the group's chain runs on `canvas`'s pixels and the paint
    /// it composites through, where it may share its layer with the chains
    /// beside it -- see [`chain::Plan::joins`], which decides whether it
    /// does. Only a chain on the GPU, run in the canvas's pixels, under no
    /// clip path and no destination of its own: a clip and a destination
    /// each bound a layer in ways no test of sharing covers yet, so those
    /// chains run alone. A run of text shares, bounded by what it can ink --
    /// its `reach`, widened by [`HINTED_INK_DRIFT`] -- as it is alone.
    fn shared_chain(
        &self,
        canvas: &SkCanvas,
        target: Target,
    ) -> Option<SharedChain> {
        let Composite::Chain {
            steps,
            rec,
            reach,
            coverage,
        } = &self.composite
        else {
            return None;
        };
        let alone = target != Target::Pixels
            || !blur_mask::on_gpu(canvas)
            || self.clip.is_some()
            || self.within.is_some()
            || steps.iter().any(Step::runs_in_local_space);
        if alone {
            return None;
        }
        let steps = Step::for_pixels(steps).unwrap_or_else(|| steps.clone());
        // A run that records its opaque copy is blurred from it, or casts
        // its shadow from it, as alone -- see `Composite::Chain`'s playback.
        let copy = coverage.as_deref().filter(|_| eight_bits(canvas)).and_then(
            |run| Some((run.picture.picture()?.clone(), byte_alpha(run.alpha))),
        );
        let mut paint = chain_paint(rec);
        let (copy, blurs) = match (copy, &steps[..]) {
            (Some((copy, alpha)), [Step::Blur(_)]) => {
                // Blurred from the copy, the run's alpha the chain's gain.
                paint.set_alpha_f(paint.alpha_f() * alpha);
                (Some((copy, alpha)), true)
            }
            (copy, steps) => (copy.filter(|_| casts_from_copy(steps)), false),
        };
        Some(SharedChain {
            steps,
            paint,
            reach: *reach,
            copy,
            blurs,
        })
    }

    /// What the group's chain draws, in the units `back` maps the group's
    /// own transforms into, where the canvas it is played onto maps those
    /// to the device by `to_device`; `reach` is the chain's own -- see
    /// [`Composite::Chain`]. A chain played alone and one sharing a layer
    /// both take their content from here, so that each draws the same.
    ///
    /// What is drawn is bounded by `within`, or by what the entries can draw
    /// in where each of them knows -- see [`reach_of`] -- so that the chain's
    /// layer is the draw's and not the page's. Bounded by a single picture
    /// alone, a draw on a page that keeps a recording for pixels, which
    /// files it as a pair, filtered the whole page: 50 rects under a drop
    /// shadow took 1.75 s on an F16 Display P3 page against 26 ms on an sRGB
    /// one.
    fn chain_content(
        &self,
        reach: Option<Rect>,
        back: &Matrix,
        to_device: &Matrix,
    ) -> Option<Rect> {
        let to_space = |matrix: &Matrix| Matrix::concat(back, matrix);
        // UPSTREAM: skia-safe 0.156.0 -- unfiled -- not worked around
        // Re-check: cargo test skia_covers_a_curve_by_where_it_lies
        //
        // Skia's analytic anti-aliasing covers a curved edge by where it lies
        // in the plane it fills, so a layer starting elsewhere would cover a
        // curve's edge pixels otherwise. A chain's layer starts at the
        // device's origin whatever bounds it -- see `chain::placed` -- so
        // text in a layer bounded by its reach is drawn where a layer
        // bounded by the clip draws it: on the CPU, Times New Roman's 96px
        // "Ag" stroked 40 wide with round joins, under a drop shadow at
        // density 3, lands the same bytes either way. The same run filled,
        // or under `blur()`, at that density -- 288 device pixels, past which
        // Skia draws a glyph as a path (`SkStrikeSpec::ShouldDrawAsPath`) --
        // differs on 1 to 3 bytes by a level, which is not traced.
        let content = match (&self.within, reach) {
            (Some(within), _) => {
                Some(to_space(&self.matrix).map_rect(within).0)
            }
            (None, Some(reach)) if keeps_text_reach() => Some(hinted_reach(
                to_space(&self.contents_matrix).map_rect(reach).0,
                to_device,
            )),
            (None, _) => reach_of(&self.contents.layers)
                .map(|reach| to_space(&self.contents_matrix).map_rect(reach).0),
        };
        // A group holding an entry whose reach is not known -- a blurred
        // draw, or a group through a chain of its own -- would be bounded by
        // the clip alone, and that layer moves an anti-aliased edge: a fill's
        // coverage of 0.6 at an alpha of 0.9 landed 0.5365 through an
        // unblurred shadow, where it makes 0.54. No group of a chain holds
        // such an entry, so no draw takes that route, and a debug build fails
        // where one does, for the route to be made exact before it is taken.
        debug_assert!(
            content.is_some(),
            "a chain's group holds an entry with no reach, which bounds its \
             layer by the clip alone"
        );
        #[cfg(test)]
        if UNBOUNDED_CHAIN.with(std::cell::Cell::get) {
            return None;
        }
        content
    }

    /// Draws what the group's chain filters onto `layer`, the chain's layer,
    /// which carries the transform from canvas pixels to its own: the
    /// entries under the group's transforms mapped by `back` -- see
    /// [`Self::chain_content`] -- confined to `within`.
    ///
    /// A canvas drawn in is drawn there as it is into the page, from a
    /// surface of its own -- see [`Self::played_at_source`] -- so that a
    /// shadow or a filter is made from the coverage the draw lands.
    fn draw_chained(&self, layer: &SkCanvas, target: Target, back: &Matrix) {
        let outer = Matrix::concat(&layer.local_to_device_as_3x3(), back);
        self.draw_chained_as(layer, target, back, |layer| {
            if target == Target::Pixels
                && self.played_at_source(layer, &outer, None, None)
            {
                return;
            }
            for entry in self.contents.layers.iter() {
                entry.playback(layer, target);
            }
        });
    }

    /// Draws the group onto `canvas` from a surface of its own -- see
    /// [`Self::played_at_source`] -- with no layer around it, where `rec`
    /// does nothing to the surface's pixels but composite them through its
    /// paint: over no backdrop, within no bounds of its own, and through a
    /// paint that is a blend mode, an alpha and a colour filter at most,
    /// with no image filter, shader or mask. Whether it did; where it did
    /// not, nothing was drawn.
    ///
    /// The surface already composites the group's entries apart from what
    /// lies beneath them, which is what such a layer does before it
    /// composites through its paint, so the surface is drawn through that
    /// paint instead, as the layer would composite -- see [`composited`].
    /// Drawn into the layer instead, its pixels landed through the layer's
    /// own restore, which on a raster on the CPU takes Skia's `N32` sprite
    /// row whatever the surface was made in -- see [`own_image_type`]: a
    /// canvas drawn in at a whole pixel stood 415 bytes a level apart from
    /// its own pixels drawn as an image on an x86-64 machine, and one drawn
    /// in under `destination-atop` 1,567.
    ///
    /// The destination confines the surface as it confines the layer: as a
    /// clip where it holds what the group draws -- none at all under a blend
    /// that changes the page beyond what is drawn, which the clip would
    /// confine and the draw needs no clip for -- and where it cuts it, by
    /// multiplying the surface by its coverage before it is drawn, as the
    /// layer is multiplied before it closes. Clipped by that coverage
    /// instead, a crop to a fractional destination stood a level from the
    /// layer's at the crop's edge. The footprint the layer is confined to is
    /// the reach of the entries, which the surface, bounded by the source,
    /// holds anyway.
    fn played_unlayered(
        &self,
        canvas: &SkCanvas,
        rec: &LayerRec,
        base: &Matrix,
    ) -> bool {
        // The paint a page's pixels composite the layer through, as
        // `LayerRec::open_on` takes it.
        let paint = rec.pixels_paint.as_ref().or(rec.paint.as_ref());
        let through = paint.is_none_or(|paint| {
            paint.as_blend_mode().is_some()
                && paint.image_filter().is_none()
                && paint.shader().is_none()
                && paint.mask_filter().is_none()
        });
        let plain = through
            && rec.backdrop.is_none()
            && rec.bounds.is_none()
            && rec.pixels_backdrop.is_none()
            && !rec.one_image;
        if !plain || !blur_mask::draws_on_cpu(canvas) {
            return false;
        }
        #[cfg(test)]
        if !UNLAYERED.get() {
            return false;
        }
        let to_device = Matrix::concat(base, &self.matrix);
        canvas.save();
        canvas.set_matrix(&to_device.into());
        let crop = self.crop(canvas, Target::Pixels, &to_device);
        // With no crop the destination holds what the group draws, so its
        // clip confines nothing of the draw, and under a blend that changes
        // the page beyond what is drawn it would confine the blend.
        if crop.is_none() && !rec.covers_beyond() {
            self.confine(canvas, Target::Pixels, &to_device);
        }
        let paint = paint.map(composited);
        let drawn = self.played_at_source(
            canvas,
            base,
            crop.as_ref(),
            paint.as_deref(),
        );
        canvas.restore();
        drawn
    }

    /// Draws a canvas or a vector image drawn in onto `canvas` from a surface
    /// of its own, on a raster on the CPU, where `outer` maps the group's own
    /// coordinates to `canvas`'s pixels. Whether it did; where it did not,
    /// nothing was drawn.
    ///
    /// The surface covers the source's own bounds -- see [`DrawnIn`] --
    /// rounded out to whole pixels, and its origin is the pixel the source's
    /// corner lies in. The entries are played on it under the transform they
    /// are played under here, moved to that origin, and under no clip; its
    /// pixels are then drawn at the origin, under whatever clip `canvas`
    /// holds, and the destination, a crop of the source, among it -- or,
    /// where `crop` is given, the surface is multiplied by that coverage
    /// first. They are drawn through `paint` where it is given: as an image,
    /// or, where its blend changes what lies beyond what is drawn -- see
    /// `covers_beyond_the_draw` -- as a shader of the image, transparent
    /// past its edges, filling the clip, as a layer composites over all it
    /// covers. Skia's analytic anti-aliasing covers a curve by where it lies on
    /// the raster it fills and cuts the coverage of a path its clip cuts --
    /// see `skia_covers_a_curve_by_where_it_lies` and [`CutOp`] -- so drawn
    /// straight into a layer, the source's curves were covered by where the
    /// clip, or the page's edge, began the layer and by where they cut it:
    /// an ellipse drawn in at a whole-pixel offset stood up to 27 levels
    /// from the source's own pixels where the page's edge cut it, and up to
    /// 17 where a clip did. From a surface of its own, a canvas drawn in at
    /// a whole-pixel offset lands the source's pixels byte for byte, as
    /// `drawImage` does with the canvas's bitmap.
    ///
    /// What it costs: a surface the size of the source as drawn, and a copy
    /// of it, however little of it the destination or the clip shows, as an
    /// image of the source costs. A surface larger than the page -- a canvas
    /// drawn in under a large scale -- is not made, and the entries are drawn
    /// as they are.
    ///
    /// Not on the GPU, which covers these alike wherever they lie and
    /// whatever cuts them: drawn into a layer there, the same scenes land
    /// the source's pixels as they are. Not in a recording either, which
    /// is played later under a transform and a clip this cannot see, and
    /// has no colour space to make the surface in: a Display P3 canvas drawn
    /// in through one landed sRGB red, and one drawn under a 180 by 24 clip
    /// rasterized its whole 1400 by 1400 page.
    fn played_at_source(
        &self,
        canvas: &SkCanvas,
        outer: &Matrix,
        crop: Option<&Coverage>,
        paint: Option<&Paint>,
    ) -> bool {
        let Some(source) = self.source else {
            return false;
        };
        let contents = Matrix::concat(outer, &self.contents_matrix);
        if !blur_mask::draws_on_cpu(canvas) || contents.has_perspective() {
            return false;
        }
        let bounds: IRect = contents.map_rect(source).0.round_out();
        let page = canvas.base_layer_size();
        let area =
            |width: i32, height: i32| i64::from(width) * i64::from(height);
        if bounds.is_empty()
            || area(bounds.width(), bounds.height())
                > area(page.width, page.height)
        {
            return false;
        }
        // UPSTREAM: skia-safe 0.156.0 -- unfiled -- worked around
        // Re-check: cargo test skia_covers_a_curve_by_where_it_lies
        //
        // The surface is what places the source's curves as the source
        // places them; were they covered alike wherever they lie, the
        // entries could be played into the layer as they are. It is made as
        // the page is, so that the entries are rasterized by the blitters
        // that drew the source's own pixels; only what is drawn back is in
        // the type of `own_image_type`.
        let Some((mut surface, key)) =
            chain::layer_surface(canvas, Depth::Bytes, bounds)
        else {
            return false;
        };
        let origin = Point::new(bounds.left as f32, bounds.top as f32);
        {
            let root = surface.canvas();
            root.set_matrix(
                &Matrix::concat(&Matrix::translate(-origin), &contents).into(),
            );
            let _root = OwnRoot::enter();
            play_entries(
                &self.contents.layers,
                &self.contents.features,
                root,
                Target::Pixels,
            );
            if let Some(crop) = crop {
                root.reset_matrix();
                root.translate(-origin);
                crop.multiply(root);
            }
        }
        let played = in_own_type(&mut surface)
            .unwrap_or_else(|| surface.image_snapshot());
        canvas.save();
        canvas.reset_matrix();
        let beyond = paint
            .and_then(Paint::as_blend_mode)
            .is_some_and(covers_beyond_the_draw);
        match (paint, beyond) {
            (Some(paint), true) => {
                let mut fill = paint.clone();
                fill.set_shader(played.to_shader(
                    (TileMode::Decal, TileMode::Decal),
                    SamplingOptions::default(),
                    &Matrix::translate(origin),
                ));
                canvas.draw_paint(&fill);
            }
            (paint, _) => {
                canvas.draw_image(&played, origin, paint);
            }
        }
        canvas.restore();
        drop(played);
        if let Some(key) = key {
            pool::keep(key, surface);
        }
        true
    }

    /// [`Self::draw_chained`], with `draw` in place of the group's entries,
    /// under the transform they are played under: a run's opaque copy --
    /// see [`CoverageRun`] -- drawn where the run itself is.
    fn draw_chained_as(
        &self,
        layer: &SkCanvas,
        target: Target,
        back: &Matrix,
        draw: impl FnOnce(&SkCanvas),
    ) {
        let to_layer = layer.local_to_device_as_3x3();
        let to_device =
            Matrix::concat(&to_layer, &Matrix::concat(back, &self.matrix));
        layer.set_matrix(&to_device.into());
        self.confine(layer, target, &to_device);
        layer.set_matrix(
            &Matrix::concat(
                &to_layer,
                &Matrix::concat(back, &self.contents_matrix),
            )
            .into(),
        );
        draw(layer);
    }

    /// [`Self::playback`], the layer confined to `within`, a rectangle in
    /// the coordinates the entries are played under, where it is `Some`.
    pub(crate) fn playback_within(
        self: &Arc<Self>,
        canvas: &SkCanvas,
        target: Target,
        within: Option<Rect>,
    ) {
        // A document gives a group that composites at full opacity alone no
        // layer, and needs none: its entries are written as they are, under
        // the group's clip and bounds -- see `Self::open_in_a_document`.
        // Skia's SVG device has no layers and writes nothing drawn inside
        // one, and its PDF device writes one as a transparency group that
        // changes nothing at an opacity of 1.
        if target == Target::Document && self.opacity() == Some(1.0) {
            self.open_in_a_document(canvas, |canvas, entries| {
                play_entries(
                    &entries.layers,
                    &entries.features,
                    canvas,
                    target,
                );
            });
            return;
        }
        let base = canvas.local_to_device_as_3x3();
        let entries = Matrix::concat(&base, &self.contents_matrix);
        let within = within.map(|within| entries.map_rect(within).0);
        canvas.save();
        if let Some(clip) = self.clip.as_ref().and_then(Path::deserialize) {
            // A layer opened under a clip path is made no larger than the
            // clip's bounds, and a cut draw in it is then cut at the layer's
            // edge, as the page's edge cuts one: a triangle clipped at x 82
            // before a layer was opened moved 57 pixels far inside the clip.
            // A clip shader leaves the layer's bounds alone and multiplies
            // what the layer composites, as the cut draw's own clip does.
            let coverage = (target == Target::Pixels
                && clips_by_coverage(canvas, target)
                && holds_a_cut(&self.contents.layers))
            .then(|| Coverage::of(&clip))
            .flatten();
            match coverage {
                Some(coverage) => coverage.clip(canvas),
                None => {
                    canvas.clip_path(&clip, ClipOp::Intersect, true);
                }
            }
        }
        match (&self.composite, target) {
            (
                Composite::Chain {
                    steps,
                    rec,
                    reach,
                    coverage,
                },
                Target::Pixels,
            ) => {
                let paint = chain_paint(rec);
                // The chain's lengths are in the pixels of the canvas the
                // group was drawn on, which `base` maps to the device -- or,
                // for a chain holding a Skia image filter, in the units of
                // the transform the draw was made under, `self.matrix`, as
                // on the layer Skia opens for it: the chain runs in that
                // space, and draws what it filters back through its
                // inverse, which a transform that maps the canvas to
                // nothing has none of and draws nothing through.
                let local = steps.iter().any(Step::runs_in_local_space);
                let back = match local {
                    true => match self.matrix.invert() {
                        Some(inverse) => {
                            canvas.concat(&self.matrix);
                            inverse
                        }
                        None => {
                            canvas.restore();
                            return;
                        }
                    },
                    false => Matrix::new_identity(),
                };
                let content = self.chain_content(
                    *reach,
                    &back,
                    &canvas.local_to_device_as_3x3(),
                );
                #[cfg(test)]
                CHAIN_CONTENT.set(content.map(|content| {
                    canvas.local_to_device_as_3x3().map_rect(content).0
                }));
                // On an eight-bit page on the CPU a run of text in one flat
                // colour is blurred from its coverage alone -- see
                // `chain::coverage` and [`CoverageRun`] -- and the chain's
                // layer is not drawn. The plane is the page's own type,
                // whose alpha is how the run covers its pixels there: an A8
                // plane's glyph masks land a level off it on 2 of the 2,322
                // pixels `tests/blur_text.rs`'s 16px run inks.
                if let Some(run) = coverage.as_deref()
                    && let [Step::Blur(sigma)] = &steps[..]
                    // Undithered: `coverage_files` records no copy of a
                    // dithered run.
                    && coverage_plays(canvas, &run.exact, false)
                    && let Some(at) =
                        chain::coverage::Placement::of(canvas, *sigma, content)
                {
                    let sharp = || {
                        chain::coverage::sharp(
                            canvas,
                            *sigma,
                            content,
                            chain::coverage::Plane::Alpha,
                            |plane| {
                                self.draw_chained_as(
                                    plane,
                                    target,
                                    &back,
                                    |plane| {
                                        if let Some(picture) =
                                            run.picture.picture()
                                        {
                                            plane.draw_picture(
                                                picture, None, None,
                                            );
                                        }
                                    },
                                )
                            },
                        )
                    };
                    let (rgb, gain) =
                        (run.exact.at(run.alpha).0.rgb, byte_alpha(run.alpha));
                    // A kept run is found by where its picture is played on
                    // the device -- see `run_mask::Placed`.
                    let to_picture = Matrix::concat(
                        &canvas.local_to_device_as_3x3(),
                        &Matrix::concat(&back, &self.contents_matrix),
                    );
                    let placed = run.kept.as_ref().and_then(|kept| {
                        run_mask::Placed::of(
                            kept,
                            canvas,
                            &to_picture,
                            *sigma,
                            rgb,
                            gain,
                        )
                    });
                    // Composited from what was kept without drawing the run
                    // where its glyphs decide its coverage -- see
                    // `run_mask::kept` -- but not under a destination, which
                    // `confine` cuts the plane by where the run lies.
                    let kept = placed
                        .as_ref()
                        .filter(|_| self.within.is_none())
                        .and_then(|placed| run_mask::kept(placed, &at, sharp));
                    // `None` where no plane could be made, for the chain to
                    // draw the run.
                    let route = match kept {
                        Some(outcome) => Some(Some(outcome)),
                        None => sharp().map(|sharp| {
                            run_mask::drawn(placed, &at, sharp, |sharp| {
                                chain::coverage::coloured(
                                    canvas,
                                    &sharp.blurred(),
                                    rgb,
                                    gain,
                                )
                            })
                        }),
                    };
                    if let Some(outcome) = route {
                        #[cfg(test)]
                        COVERAGE_RUNS.with(|runs| runs.set(runs.get() + 1));
                        if let Some(outcome) = outcome {
                            outcome.composite(canvas, &paint);
                        }
                        canvas.restore();
                        return;
                    }
                }
                // A group a canvas drawn into another brings with it is played
                // once for each time that canvas is, and its chain is run the
                // first of those -- see `chain_cache`.
                let key = chain_cache::Key::of(self, canvas);
                if let Some(outcome) = key.as_ref().and_then(chain_cache::get) {
                    outcome.composite(canvas, &paint);
                    canvas.restore();
                    return;
                }
                // A page's pixels run the chain's filters built for them.
                let pixels = match target {
                    Target::Pixels => Step::for_pixels(steps),
                    Target::Document => None,
                };
                let steps = pixels.as_deref().unwrap_or(steps);
                // A run in one flat colour the coverage route does not take
                // -- in another space than its colour was held in, or on the
                // GPU, where the route never plays -- is drawn opaque on an
                // eight-bit page, its alpha the chain's gain, so that the
                // blur rounds its alpha once, as `BlurOp::result_chain` does
                // a path's: drawn in its translucent colour, the layer
                // rounded its alpha to a byte before the blur and again
                // after. A translucent run's `drop-shadow()` is cast from it
                // drawn opaque for the same reason, the run itself still
                // drawn translucent over it, as the page draws it -- see
                // `chain::Cast` and [`FlatRun`].
                let opaque = coverage
                    .as_deref()
                    .filter(|_| eight_bits(canvas))
                    .and_then(|run| Some((run, run.picture.picture()?)));
                let opaquely = |picture: &Picture, layer: &SkCanvas| {
                    self.draw_chained_as(layer, target, &back, |layer| {
                        layer.draw_picture(picture, None, None);
                    })
                };
                let outcome = match opaque {
                    Some((run, picture)) if casts_from_copy(steps) => {
                        #[cfg(test)]
                        CAST_RUNS.with(|runs| runs.set(runs.get() + 1));
                        // A kept run is found by where its picture is played
                        // in the canvas's pixels, as the coverage route finds
                        // it -- see `run_mask::Placed` -- the chain's layer
                        // scale applied after; but not under a destination,
                        // which `confine` cuts the layer by where the run
                        // lies, so that where it lies no longer decides the
                        // cast's pixels.
                        let to_canvas =
                            Matrix::concat(&back, &self.contents_matrix);
                        chain::run_cast(
                            canvas,
                            steps,
                            content,
                            Depth::Floats,
                            &paint,
                            |layer| self.draw_chained(layer, target, &back),
                            chain::Cast {
                                canvas,
                                draw: &|layer| opaquely(picture, layer),
                                alpha: byte_alpha(run.alpha),
                                run: run
                                    .kept
                                    .as_ref()
                                    .filter(|_| self.within.is_none())
                                    .map(|kept| (kept, to_canvas)),
                            },
                        )
                    }
                    Some((run, picture))
                        if matches!(steps, [Step::Blur(_)]) =>
                    {
                        let mut gained = paint.clone();
                        gained.set_alpha_f(
                            paint.alpha_f() * byte_alpha(run.alpha),
                        );
                        chain::run(
                            canvas,
                            steps,
                            content,
                            Depth::Floats,
                            &gained,
                            |layer| opaquely(picture, layer),
                        )
                    }
                    _ => chain::run(
                        canvas,
                        steps,
                        content,
                        Depth::Floats,
                        &paint,
                        |layer| self.draw_chained(layer, target, &back),
                    ),
                };
                if let Some(outcome) = outcome {
                    outcome.composite(canvas, &paint);
                    if let Some(key) = key {
                        chain_cache::put(key, outcome);
                    }
                }
            }
            (Composite::Layer(rec), Target::Pixels)
                if self.played_unlayered(canvas, rec, &base) => {}
            (Composite::Layer(rec) | Composite::Chain { rec, .. }, target) => {
                let to_device = Matrix::concat(&base, &self.matrix);
                canvas.set_matrix(&to_device.into());
                // The destination confines what is drawn and not what the
                // layer's filter makes of it, nor what its blend does to the
                // page beyond what is drawn, so under a filter or such a
                // blend it is a clip inside the layer, where a shadow can
                // still be cast past it and the layer still composites over
                // all the clip covers. Where it cuts what is drawn, the layer
                // is multiplied by it before it closes, which is as much
                // inside the layer, and leaves both the draws and the
                // layer's composite whole -- see `Self::crop`.
                let filtered = rec
                    .paint
                    .as_ref()
                    .is_some_and(|paint| paint.image_filter().is_some());
                let inside = filtered || rec.covers_beyond();
                let crop = self.crop(canvas, target, &to_device);
                if crop.is_none() && !inside {
                    self.confine(canvas, target, &to_device);
                }
                rec.open_on(canvas, target, within);
                if crop.is_none() && inside {
                    self.confine(canvas, target, &to_device);
                }
                canvas.set_matrix(&entries.into());
                // A layer seeded from what lies beneath it, or filtered,
                // holds more than its entries draw, which a root of their
                // own would not.
                let rooted = target == Target::Pixels
                    && !filtered
                    && rec.backdrop.is_none()
                    && rec.pixels_backdrop.is_none()
                    && played_on_a_root(canvas, &self.contents);
                // A canvas drawn in lands its own pixels -- see
                // `Self::played_at_source`.
                let placed = !rooted
                    && target == Target::Pixels
                    && self.played_at_source(canvas, &base, None, None);
                if !rooted && !placed {
                    let in_layer = InLayer::enter(
                        target == Target::Pixels
                            && (rec.backdrop.is_some()
                                || rec.pixels_backdrop.is_some()),
                    );
                    play_entries(
                        &self.contents.layers,
                        &self.contents.features,
                        canvas,
                        target,
                    );
                    drop(in_layer);
                }
                if let Some(crop) = crop {
                    canvas.reset_matrix();
                    crop.multiply(canvas);
                }
                canvas.restore();
            }
        }
        canvas.restore();
    }
}

/// `surface`'s pixels as an image in the colour type [`own_image_type`]
/// gives its own, which an eight-bit surface in the page's type is not:
/// `None` where it is already that type, or where the read fails.
///
/// The bytes are read out in the other order rather than drawn there, since
/// a read reorders them exactly; drawn into a surface of that type, the
/// entries were rasterized by the raster pipeline rather than the blitters
/// that drew the source's own pixels, and a canvas drawn in at a whole pixel
/// stood 678 bytes up to two levels from them on an x86-64 machine. What it
/// costs is the read: a copy of the surface's pixels, which the image
/// shares rather than copies again.
fn in_own_type(surface: &mut Surface) -> Option<Image> {
    let info = surface.image_info();
    let own = info.with_color_type(own_image_type(info.color_type()));
    if own.color_type() == info.color_type() {
        return None;
    }
    let mut bitmap = Bitmap::new();
    if !bitmap.try_alloc_pixels_info(&own, None)
        || !surface.read_pixels_to_pixmap(bitmap.pixmap(), IPoint::new(0, 0))
    {
        return None;
    }
    bitmap.set_immutable();
    Some(bitmap.as_image())
}

/// Plays `contents` into the layer open on `canvas` by way of a surface of
/// their own, where they hold a draw made on the CPU over the pixels it
/// reads -- see [`reads_the_page`] -- which inside the layer could not
/// read them: `SkCanvas::readPixels` reads the canvas's root device, never
/// an open layer. A run would then be drawn by the GPU itself, its
/// overlapping glyphs blended as one -- see [`replayed_on_their_backdrop`]
/// -- and a draw dyed by a gradient made on the CPU over transparent and
/// composited into the layer, rounding twice where the page rounds once --
/// see [`drawn_on_their_backdrop`]. Whether it played them; where it did
/// not, nothing was drawn.
///
/// The surface covers the layer's bounds -- the canvas's device clip
/// bounds while the layer is open -- in the canvas's colour type and space,
/// made by the canvas's device at the page's sample count, and taken from
/// and given back to the layers [`chain::pool`] keeps -- see
/// [`chain::layer_surface`]. The entries are played on it under the
/// canvas's transform, moved to the layer's origin, and see no layer open:
/// on it they play as on a page, a canvas drawn into another or a layer
/// within them taking a surface of its own in turn. Its pixels are then
/// copied into the layer, which composites as it always does, so the
/// layer's paint, its clip and its bounds stay Skia's. Skia clips nothing
/// drawn inside a layer by the clip the layer opened under -- the new
/// device's clip starts wide open, `SkCanvas::internalSaveLayer` -- and
/// applies that clip where the layer is drawn back, so an outer clip, an
/// anti-aliased path's included, shapes the copy as it shaped the entries.
///
/// What it costs past the layer: the surface, a copy of the layer's size,
/// and the read backs its draws take, which a page drawn directly pays as
/// well. Where the device makes no surface, the surface is memory, and the
/// entries play there as on a CPU page, the copy then uploading it.
///
/// Only on the GPU, which alone makes such a draw over a read back, and not
/// under a perspective, under which the page reads none back either -- see
/// [`drawn_on_their_backdrop`] and [`replayed_on_their_backdrop`].
fn played_on_a_root(canvas: &SkCanvas, contents: &Entries) -> bool {
    #[cfg(test)]
    if !OWN_ROOTS.get() {
        return false;
    }
    let device = canvas.local_to_device_as_3x3();
    if !blur_mask::on_gpu(canvas)
        || device.has_perspective()
        || !reads_the_page(&contents.layers, &device)
    {
        return false;
    }
    let Some(bounds) = canvas.device_clip_bounds() else {
        return false;
    };
    let Some((mut surface, key)) =
        chain::layer_surface(canvas, Depth::Bytes, bounds)
    else {
        return false;
    };
    let origin = Point::new(bounds.left as f32, bounds.top as f32);
    {
        let root = surface.canvas();
        root.set_matrix(
            &Matrix::concat(&Matrix::translate(-origin), &device).into(),
        );
        let _root = OwnRoot::enter();
        // A chain on the surface keeps its layers as it would on the canvas.
        let _scope = pool::samples(canvas)
            .map(|samples| LayerScope::open(root, samples));
        play_entries(
            &contents.layers,
            &contents.features,
            root,
            Target::Pixels,
        );
    }
    #[cfg(test)]
    ROOTS_PLAYED.set(ROOTS_PLAYED.get() + 1);
    // Drawn source-over onto what the layer holds there, which is nothing,
    // so the layer takes the surface's pixels as they are.
    let played = surface.image_snapshot();
    canvas.save();
    canvas.reset_matrix();
    canvas.draw_image(&played, origin, None);
    canvas.restore();
    drop(played);
    if let Some(key) = key {
        pool::keep(key, surface);
    }
    true
}

/// Whether `entries`, played under `device`, hold a draw a GPU canvas makes
/// on the CPU over the pixels it reads, themselves or in a group they hold,
/// whose entries play under the group's own transform: a run -- see
/// [`read_on_the_cpu`] -- a run painted as one image among them, or a draw
/// dyed by a gradient -- see [`GradientDraw::reads_the_page`].
fn reads_the_page(entries: &[Layer], device: &Matrix) -> bool {
    entries.iter().any(|entry| match entry {
        Layer::Group(group) => {
            read_on_the_cpu(entry).is_some()
                || reads_the_page(
                    &group.contents.layers,
                    &Matrix::concat(device, &group.contents_matrix),
                )
        }
        Layer::Gradient(draw) => draw.reads_the_page(device),
        // Only pixels are played on a GPU canvas.
        Layer::Split(split) => reads_the_page(&split.pixels.layers, device),
        Layer::Clipped(piece) => match piece.entry() {
            Layer::Gradient(draw) => draw.reads_the_page(device),
            _ => read_on_the_cpu(entry).is_some(),
        },
        entry => read_on_the_cpu(entry).is_some(),
    })
}

/// Part of a run: what was drawn under one frame of the page -- one clip
/// and one transform -- recorded without the clip, which is applied where it
/// is played.
///
/// A run the page neither read nor cut part way is drawn into the page's own
/// recording under its clip when its frame ends, and never becomes one of
/// these. A read, an export or a draw the clip cuts files what the run has
/// so far as a piece, and the rest of the run follows as more pieces. Played
/// as pixels each piece is clipped by itself, as the page's recording clips
/// a frame; a document, and a recording a canvas drawn into another is
/// replayed into, play a run's consecutive pieces under the clip once, as
/// for a run nothing split, a recording's cut draws apart -- see
/// [`play_entries`]. A clip applied again for
/// each piece is a clip of its own to a PDF or SVG device, which keys a clip
/// group by the clip's generation, and the document came out with the clip
/// written once per piece.
#[derive(Debug)]
pub struct Clipped {
    /// The run the piece belongs to, unique for the process.
    pub run: u64,
    /// The clip, in device pixels of the recording.
    clip: Data,
    /// The clip's bounds, which bound what the piece can draw.
    bounds: Rect,
    /// What the piece draws: a picture, or a pair of them on a page that
    /// keeps a recording for each kind of target, or a [`BlurOp`], each
    /// recorded without the clip; or a [`CutOp`], which applies the clip
    /// itself as pixels. Read through [`Clipped::entry`], and changed only
    /// through [`Clipped::entry_mut`], which forgets `drawn`.
    entry: Layer,
    /// What the entry draws, found the first time it is asked for -- see
    /// [`Clipped::drawn`].
    drawn: OnceLock<Option<Rect>>,
}

#[cfg(test)]
thread_local! {
    /// Whether a group's chain runs over a layer the clip alone bounds on
    /// this thread, rather than one bounded by what the group's entries
    /// reach: the reference a bounded chain is held to.
    pub(crate) static UNBOUNDED_CHAIN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Whether every group's chain runs in a layer of its own on this
    /// thread, rather than sharing one where [`play_shared_chains`] lets it:
    /// the reference a shared layer is held to.
    pub(crate) static ALONE_CHAINS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };

    /// The most pixels a side a run's shadow atlas may take, where set, so
    /// that a test can hold a run whose shadows one atlas cannot hold to
    /// what it lands -- see [`chain::ShadowAtlas::plan`].
    pub(crate) static ATLAS_SIDE: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
    /// Whether a run's pieces played into a recording bound for pixels have
    /// their clip bounded by what they draw on this thread: off, the clip
    /// covers its own bounds, the reference a bounded clip is held to.
    pub(crate) static BOUNDS_PIECES: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
    /// Whether a run's pieces played onto a raster on the CPU have their
    /// clip bounded by what they draw on this thread: off, as for
    /// [`BOUNDS_PIECES`], the reference a bounded clip is held to.
    pub(crate) static BOUNDS_PAGE: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
    /// Every rectangle a run's clip has been bounded by on this thread.
    pub(crate) static BOUNDED: std::cell::RefCell<Vec<IRect>> = const { std::cell::RefCell::new(Vec::new()) };
    /// How many times a run's clip has been applied for its pieces on this
    /// thread -- see [`Clipped::play_run`].
    pub(crate) static CLIPS_APPLIED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// A clip as the entries played under it keep it: serialized, and its
/// bounds. Made once for a run and shared by its pieces and cut draws --
/// [`Data`] is shared, not copied -- rather than serialized again for each.
#[derive(Clone, Debug)]
pub struct ClipData {
    data: Data,
    bounds: Rect,
}

impl ClipData {
    /// `clip`, in device pixels of the recording.
    pub fn of(clip: &Path) -> Self {
        ClipData {
            data: clip.serialize(),
            bounds: *clip.bounds(),
        }
    }
}

impl Clipped {
    /// `entry`, drawn under `clip` in run `run`.
    pub fn new(run: u64, clip: &ClipData, entry: Layer) -> Self {
        Clipped {
            run,
            clip: clip.data.clone(),
            bounds: clip.bounds,
            entry,
            drawn: OnceLock::new(),
        }
    }

    /// What the piece draws.
    pub(crate) fn entry(&self) -> &Layer {
        &self.entry
    }

    /// What the piece draws, to grow: a run of blurs or of cut draws takes
    /// the next draw into the piece holding the last. What the piece was
    /// found to draw is forgotten, to be found again from what it now does.
    pub(crate) fn entry_mut(&mut self) -> &mut Layer {
        self.drawn = OnceLock::new();
        &mut self.entry
    }

    /// The bounds of the clip the piece is drawn under.
    #[cfg(test)]
    pub(crate) fn clip_bounds(&self) -> Rect {
        self.bounds
    }

    /// What the piece draws, within the clip's bounds, as Skia bounds each
    /// draw -- its stroke, its join and caps, a shadow's offset and blur, a
    /// filter's reach. A run is recorded without a bounding hierarchy, so a
    /// piece's picture culls to the page or its clip -- see
    /// `page::Run::cull`; its draws are replayed once into a
    /// recording with one, which Skia trims to what they draw, and the
    /// rectangle kept. `None` where a draw has no rectangle to take -- see
    /// [`Layer::reach`].
    pub(crate) fn drawn(&self) -> Option<Rect> {
        *self.drawn.get_or_init(|| {
            #[cfg(test)]
            PIECES_FOUND.set(PIECES_FOUND.get() + 1);
            let mut within = match &self.entry {
                Layer::Picture(picture) => drawn_by(picture),
                Layer::Targeted(pictures) => drawn_by(&pictures.0),
                Layer::Deferred(deferred) => {
                    deferred.picture().and_then(drawn_by)
                }
                Layer::ReplayedText(text) => Some(text.reach()),
                entry => entry.reach(),
            }?;
            if !within.intersect(self.bounds) {
                return Some(Rect::new_empty());
            }
            Some(within)
        })
    }

    /// Draws the piece onto `canvas` for `target` under its clip: on a
    /// raster on the CPU, bounded by what it draws -- see [`Self::play_run`]
    /// -- since Skia builds the clip's mask again for each piece played
    /// there, and a raster writes no clip of its own anywhere.
    fn playback(&self, canvas: &SkCanvas, target: Target) {
        if let Layer::Cut(op) = &self.entry {
            op.playback(canvas, target);
            return;
        }
        let bounded = target != Target::Document
            && blur_mask::draws_on_cpu(canvas)
            && bounds_page();
        Self::play_run(
            std::slice::from_ref(self),
            &self.clip,
            canvas,
            target,
            bounded,
        );
    }

    /// Draws `pieces`, of one run, onto `canvas` for `target` under `clip`,
    /// applied once -- and where `bounded`, applied only over what the
    /// pieces draw.
    ///
    /// Skia builds a mask of an anti-aliased clip over the clip's bounds,
    /// whatever is drawn under it, and builds it again each time the clip is
    /// applied: a run whose cut draws split it applies the clip once for
    /// each stretch between them. A rectangle on whole pixels around what
    /// the stretch draws bounds the mask to those rows and columns, where
    /// the clip contains the rectangle: the mask is then whole over every
    /// pixel the stretch inks, as it is unbounded. Where the rectangle cuts
    /// the clip's path, Skia rasterizes the mask's edge against it as it
    /// does a path against a clip, and the edge comes out other than it does
    /// over the clip's bounds -- 26 pixels of a rectangle drawn across a
    /// circle's edge changed -- so the stretch is left unbounded.
    fn play_run(
        pieces: &[impl std::borrow::Borrow<Clipped>],
        clip: &Data,
        canvas: &SkCanvas,
        target: Target,
        bounded: bool,
    ) {
        canvas.save();
        let drawn = bounded
            .then(|| {
                pieces.iter().try_fold(Rect::new_empty(), |mut all, piece| {
                    all.join(piece.borrow().drawn()?);
                    Some(all)
                })
            })
            .flatten();
        if let Some(clip) = Path::deserialize(clip) {
            if let Some(drawn) = drawn {
                let mask: IRect = drawn.round_out();
                if clip.conservatively_contains_rect(Rect::from_irect(mask)) {
                    canvas.clip_irect(mask, ClipOp::Intersect);
                    #[cfg(test)]
                    BOUNDED.with_borrow_mut(|bounded| bounded.push(mask));
                }
            }
            canvas.clip_path(&clip, ClipOp::Intersect, true);
            #[cfg(test)]
            CLIPS_APPLIED.set(CLIPS_APPLIED.get() + 1);
        }
        for piece in pieces {
            match &piece.borrow().entry {
                Layer::Cut(op) => op.playback_unclipped(canvas, target),
                entry => entry.playback(canvas, target),
            }
        }
        canvas.restore();
    }

    /// What the piece can draw in: what its entry draws, within the clip's
    /// bounds as a picture recorded under the clip is bounded, or for a cut
    /// draw what it paints before the clip -- see [`CutOp::reach`].
    fn reach(&self) -> Option<Rect> {
        if let Layer::Cut(op) = &self.entry {
            return Some(op.reach());
        }
        let mut within = self.entry.reach()?;
        if !within.intersect(self.bounds) {
            return Some(Rect::new_empty());
        }
        Some(within)
    }
}

/// A draw [`drawn_on_their_backdrop`] makes: what it can change -- see
/// [`BackdropReach`] -- the draw itself, made onto a canvas under that
/// canvas's transform, and the bounds of a clip path it applies itself, in
/// the coordinates it is made in, where it applies one.
pub(crate) struct BackdropDraw<'a> {
    pub(crate) reach: BackdropReach,
    pub(crate) draw: &'a dyn Fn(&SkCanvas),
    pub(crate) clip: Option<Rect>,
}

/// What a [`BackdropDraw`] can change.
#[derive(Clone)]
pub(crate) enum BackdropReach {
    /// A rectangle in the coordinates the draw is made in, under the
    /// canvas's transform.
    Local(Rect),
    /// A rectangle in the canvas's pixels.
    Device(IRect),
    /// The whole clip: the draw's blend changes what lies beyond it.
    Beyond,
}

/// Draws `draws` onto `canvas` one after another as the CPU draws them, over
/// the pixels `canvas` holds there: those pixels read back once into a raster
/// made on the CPU in `format`, each draw made onto it under the transform
/// `canvas` carries, over what the one before it left there, as reading back
/// between them would have found, and the result written back once as it
/// is. The raster covers what the draws reach within the clip, or the
/// clip's bounds whole where one's blend changes what lies beyond it, and
/// reaches over the bounds of any clip path a draw applies itself.
/// `false`, having drawn nothing, where the pixels cannot be read: inside a
/// layer, under a perspective, or where Skia refuses the read or the write.
///
/// The one route that reproduces a draw the CPU blends in floats and rounds
/// once, after blending, as its `highp` pipeline does: compositing a raster
/// of the draw made over transparent rounds the draw's colour first and the
/// blend again, a level apart wherever the two roundings differ. A read back
/// from the GPU waits for it, so the draws share one: one each made 200
/// Oklab radials cost 23 times what Ganesh's own gradient did.
fn drawn_on_their_backdrop(
    canvas: &SkCanvas,
    draws: &[BackdropDraw<'_>],
    format: &Format,
) -> bool {
    if InLayer::open() {
        return false;
    }
    let device = canvas.local_to_device_as_3x3();
    if device.has_perspective() {
        return false;
    }
    let Some(clip) = canvas.device_clip_bounds() else {
        return true;
    };
    let area = draws.iter().fold(None, |area: Option<IRect>, draw| {
        let reach = match &draw.reach {
            BackdropReach::Beyond => Some(clip),
            BackdropReach::Local(reach) => {
                IRect::intersect(&device.map_rect(reach).0.round_out(), &clip)
            }
            BackdropReach::Device(reach) => IRect::intersect(reach, &clip),
        };
        match (area, reach) {
            (Some(area), Some(reach)) => Some(IRect::new(
                area.left.min(reach.left),
                area.top.min(reach.top),
                area.right.max(reach.right),
                area.bottom.max(reach.bottom),
            )),
            (area, reach) => area.or(reach),
        }
    });
    let Some(area) = area else {
        return true;
    };
    let info = format.image_info(area.size());
    let row = info.min_row_bytes();
    let mut backdrop = vec![0u8; info.compute_byte_size(row)];
    if !canvas.read_pixels(&info, &mut backdrop, row, (area.left, area.top)) {
        return false;
    }
    #[cfg(test)]
    BACKDROPS_READ.set(BACKDROPS_READ.get() + 1);
    // The raster reaches from the canvas's origin, so that the draws are
    // made at the device coordinates the CPU canvas makes them at: offset to
    // the area, the transform inverted with another translation moved a
    // value on a half level to the next, 2 bytes in 40 Oklab radials.
    //
    // A draw applying a clip path of its own finds the raster's bounds
    // where the page's lie, over the path, and no clip to the area: Skia
    // rasterizes an anti-aliased clip's edge against the bounds that cut it
    // as it does a path against a clip, other than over the path's own --
    // 59 channels of a page of runs under a circle, the area cutting it.
    let page = canvas.image_info().bounds();
    let clipped = draws
        .iter()
        .filter_map(|draw| draw.clip)
        .filter_map(|clip| {
            IRect::intersect(&device.map_rect(clip).0.round_out(), &page)
        })
        .map(|clip| (clip.right, clip.bottom))
        .reduce(|a, b| (a.0.max(b.0), a.1.max(b.1)));
    let (right, bottom) = clipped.map_or((area.right, area.bottom), |at| {
        (area.right.max(at.0), area.bottom.max(at.1))
    });
    let whole = format.image_info(ISize::new(right, bottom));
    // Under the canvas's own surface properties, so that text on the raster
    // takes the page's contrast and gamma rather than Skia's defaults, which
    // differ from the page's on Linux.
    let props = canvas.props();
    let Some(mut surface) = started::raster(&whole, None, props.as_ref())
    else {
        return false;
    };
    let raster = surface.canvas();
    if !raster.write_pixels(&info, &backdrop, row, (area.left, area.top)) {
        return false;
    }
    if clipped.is_none() {
        raster.clip_irect(area, ClipOp::Intersect);
    }
    raster.concat(&device);
    for draw in draws {
        raster.save();
        (draw.draw)(raster);
        raster.restore();
    }
    let Some(drawn) = surface.image_snapshot_with_bounds(area) else {
        return false;
    };
    canvas.save();
    canvas.reset_matrix();
    let mut src = Paint::default();
    src.set_blend_mode(BlendMode::Src);
    canvas.draw_image(drawn, (area.left as f32, area.top as f32), Some(&src));
    canvas.restore();
    true
}

/// Plays the runs of text `entries` opens with, each under a blend read in
/// the shader, a [`ReplayedText`] or a run dyed by a canvas pattern, alone
/// or a piece of a clip's run, or painted as one image through a layer of
/// its own under any blend -- see [`read_on_the_cpu`] -- and each with
/// the group casting its shadow where that was filed just before it -- see
/// [`cpu_run_at`] -- onto a GPU canvas as the CPU draws them, over one read
/// back of the pixels they ink -- see [`drawn_on_their_backdrop`]: on the CPU a
/// run is drawn whole and composites each glyph over the last, where Ganesh,
/// with no framebuffer fetch on Metal, copies the page for every draw that
/// reads it and draws a run's overlapping glyphs in groups that share no pixel,
/// each copy ending a render pass. How many entries it played, `0` where
/// `entries` opens with no such run or the pixels cannot be read.
///
/// The runs need not be consecutive. An entry between two of them joins the
/// batch where what it can draw -- see [`Layer::reach`] -- meets what no run
/// of the batch inks, before it or after it; one that meets a run's ink, or
/// whose reach is not known, ends the batch before it, as does a run whose
/// ink meets an entry already taken. The entries between are played first,
/// on the GPU, in their order -- see [`play_entries`] -- then the page read
/// back over the runs' ink, the runs drawn over it in theirs and the result
/// written back: a run reads the page only where it inks, which no entry
/// between touches, so each finds what it would have in the order drawn, and
/// the entries' pixels within the area read come back as they were.
fn replayed_on_their_backdrop(
    entries: &[Layer],
    features: &[VectorFeatures],
    canvas: &SkCanvas,
) -> usize {
    #[cfg(test)]
    if !BACKDROP_ROUTE.get() {
        return 0;
    }
    if entries.is_empty() || cpu_run_at(entries, 0).is_none() {
        return 0;
    }
    let device = canvas.local_to_device_as_3x3();
    let (Some(format), Some(clip)) =
        (Format::of(canvas), canvas.device_clip_bounds())
    else {
        return 0;
    };
    if InLayer::open() || device.has_perspective() {
        return 0;
    }
    // What each run inks, recorded under the transform it is played under,
    // within the bounds of the clip it is drawn under where it has one; what
    // it reaches where that cannot be had.
    let reach_of_run = |run: &KeptRun<'_>, within: Option<Rect>| match (
        run.reach(&device, clip),
        within,
    ) {
        (reach, None) => reach,
        (BackdropReach::Device(inks), Some(within)) => {
            let within = device.map_rect(within).0.round_out();
            BackdropReach::Device(
                IRect::intersect(&inks, &within).unwrap_or_default(),
            )
        }
        (BackdropReach::Local(mut reach), Some(within)) => {
            if !reach.intersect(within) {
                reach = Rect::new_empty();
            }
            BackdropReach::Local(reach)
        }
        (BackdropReach::Beyond, Some(_)) => BackdropReach::Beyond,
    };
    let in_pixels = |reach: &BackdropReach| match reach {
        BackdropReach::Beyond => clip,
        BackdropReach::Device(reach) => *reach,
        BackdropReach::Local(reach) => device.map_rect(reach).0.round_out(),
    };
    let mut runs: Vec<CpuRun> = vec![];
    let mut between: Vec<IRect> = vec![];
    // The batch ends at its last run, so nothing past the last on the page
    // is asked what it reaches.
    let last_run = (0..entries.len())
        .rev()
        .find(|&at| cpu_run_at(entries, at).is_some())
        .unwrap_or_default();
    let mut at = 0;
    while at < entries.len() {
        match cpu_run_at(entries, at) {
            Some((span, run, within)) => {
                let last = at + span - 1;
                // A shadow cast before the run is drawn with it, over what
                // it reaches as well.
                let reach = match (reach_of_run(&run, within), span) {
                    (reach, 1) | (reach @ BackdropReach::Beyond, _) => reach,
                    (reach, _) => {
                        let Layer::Group(group) = &entries[at] else {
                            break;
                        };
                        let Some(cast) = group.chain_reach_under(&device)
                        else {
                            break;
                        };
                        let inks = in_pixels(&reach);
                        BackdropReach::Device(
                            IRect::intersect(
                                &IRect::new(
                                    inks.left.min(cast.left),
                                    inks.top.min(cast.top),
                                    inks.right.max(cast.right),
                                    inks.bottom.max(cast.bottom),
                                ),
                                &clip,
                            )
                            .unwrap_or_default(),
                        )
                    }
                };
                let inks = in_pixels(&reach);
                if between.iter().any(|drawn| IRect::intersects(drawn, &inks)) {
                    break;
                }
                let clip_bounds = entries[at..=last]
                    .iter()
                    .filter_map(clip_path_bounds)
                    .reduce(|mut all, bounds| {
                        all.join(bounds);
                        all
                    });
                runs.push(CpuRun {
                    first: at,
                    last,
                    reach,
                    clip: clip_bounds,
                });
                at = last + 1;
            }
            None if runs.is_empty() || at > last_run => break,
            None => {
                let meets = |reach: Rect| {
                    let drawn = device.map_rect(reach).0.round_out();
                    let met = runs.iter().any(|run| {
                        IRect::intersects(&in_pixels(&run.reach), &drawn)
                    });
                    (met, drawn)
                };
                let Some(reach) = entries[at].reach() else {
                    break;
                };
                let (mut met, mut drawn) = meets(reach);
                // Asked again, as found, only where the reach alone would end
                // the batch -- see `Layer::found_reach`.
                #[cfg(test)]
                let found = !REACH_UNFOUND.get();
                #[cfg(not(test))]
                let found = true;
                if met
                    && found
                    && let Some(within) = entries[at].found_reach()
                {
                    (met, drawn) = meets(within);
                }
                if met {
                    break;
                }
                between.push(drawn);
                at += 1;
            }
        }
        #[cfg(test)]
        if BACKDROP_EACH_RUN.get() && !runs.is_empty() {
            break;
        }
    }
    // The batch ends at its last run; the entries after it play as they do.
    let Some(last) = runs.last().map(|run| run.last) else {
        return 0;
    };
    // The entries between are those between each two runs of the batch.
    let mut between_played = false;
    for pair in runs.windows(2) {
        let (from, to) = (pair[0].last + 1, pair[1].first);
        if from < to {
            play_entries(
                &entries[from..to],
                &features[from..to],
                canvas,
                Target::Pixels,
            );
            between_played = true;
        }
    }
    let plays: Vec<_> = runs
        .iter()
        .map(|run| {
            let drawn = &entries[run.first..=run.last];
            move |raster: &SkCanvas| {
                for entry in drawn {
                    entry.playback(raster, Target::Pixels);
                }
            }
        })
        .collect();
    let draws: Vec<BackdropDraw<'_>> = zip(&runs, &plays)
        .map(|(run, play)| BackdropDraw {
            reach: run.reach.clone(),
            draw: play,
            clip: run.clip,
        })
        .collect();
    // Where the pixels cannot be read, the runs are drawn as they are
    // otherwise: by the caller where nothing has played, and here after the
    // entries between, which they do not meet, so in an order that draws
    // the same.
    if !drawn_on_their_backdrop(canvas, &draws, &format) {
        if !between_played {
            return 0;
        }
        for run in &runs {
            for entry in &entries[run.first..=run.last] {
                entry.playback(canvas, Target::Pixels);
            }
        }
    }
    for run in &runs {
        let drawn = &entries[run.first..=run.last];
        let paced = features.get(run.first..).unwrap_or_default();
        for (entry, features) in zip(drawn, paced) {
            pacing::played(canvas, offscreen_layers(entry, *features));
        }
    }
    last + 1
}

/// A run [`replayed_on_their_backdrop`] draws on the CPU: the entries from
/// `first` to `last` -- the run, and the group casting its shadow where
/// that was filed just before it -- what they can change, and the bounds of
/// any clip path they apply themselves.
struct CpuRun {
    first: usize,
    last: usize,
    reach: BackdropReach,
    clip: Option<Rect>,
}

/// The run `entries` holds at `at` that [`replayed_on_their_backdrop`]
/// draws on the CPU -- see [`read_on_the_cpu`] -- how many entries it
/// spans, and the bounds of the clip it is drawn under where it has one: a
/// run alone, or the group a run's shadow is cast through followed by the
/// run -- see [`ReplayedText::cast_before`] and [`PatternDraw::cast_before`]
/// -- or a run painted as one image -- see [`LayerRec::one_image`] -- which
/// the CPU page composites one after the other, as the raster then does.
/// `None` for any other entry.
fn cpu_run_at(
    entries: &[Layer],
    at: usize,
) -> Option<(usize, KeptRun<'_>, Option<Rect>)> {
    if let Some((run, within)) = read_on_the_cpu(&entries[at]) {
        return Some((1, run, within));
    }
    // A shadow cast through a chain, alone or inside the layer that
    // composites it through a blend that covers beyond the draw -- see
    // `Context2D::files_shadow_through_chain`.
    let chain = |entry: &Layer| {
        matches!(entry, Layer::Group(group)
            if matches!(group.composite, Composite::Chain { .. }))
    };
    let cast = match &entries[at] {
        Layer::Group(group) => {
            chain(&entries[at])
                || matches!(group.composite, Composite::Layer(_))
                    && matches!(&group.contents.layers[..], [only] if chain(only))
        }
        _ => false,
    };
    // A run painted as one image follows its shadow, painted alone just
    // before it -- see `Context2D::draw_text`. A chain before it that is some
    // other draw's is drawn on the CPU with it, as the CPU page draws it.
    let cast_before = |entry: &Layer| match entry {
        Layer::ReplayedText(text) => text.cast_before,
        Layer::Pattern(draw) => draw.cast_before,
        Layer::Group(group) => matches!(&group.composite,
            Composite::Layer(rec) if rec.one_image),
        _ => false,
    };
    let next = entries.get(at + 1).filter(|next| match next {
        Layer::Clipped(piece) => cast_before(piece.entry()),
        next => cast_before(next),
    })?;
    let (run, within) = read_on_the_cpu(next)?;
    cast.then_some((2, run, within))
}

/// The bounds of a clip path `entry` applies itself, in the coordinates it
/// is played back under: a piece's, or the clip a run or a pattern's draw
/// holds where the page keeps no run of it. A group casting a run's shadow
/// is filed under the clip the run is, and needs no bounds of its own.
fn clip_path_bounds(entry: &Layer) -> Option<Rect> {
    let bounds = |clip: &Option<Data>| {
        clip.as_ref()
            .and_then(Path::deserialize)
            .map(|clip| *clip.bounds())
    };
    match entry {
        Layer::Clipped(piece) => Some(piece.bounds),
        Layer::ReplayedText(text) => bounds(&text.clip),
        Layer::Pattern(draw) => bounds(&draw.clip),
        _ => None,
    }
}

/// A draw of text under a blend read in the shader that
/// [`replayed_on_their_backdrop`] draws on the CPU: a kept run -- see
/// [`ReplayedText::reads_the_page`] -- or a run dyed by a canvas pattern --
/// see [`PatternDraw::reads_the_page`], whose tile a raster makes on the CPU
/// as the CPU page makes it; or a run painted as one image, through a
/// layer of its own -- see [`LayerRec::one_image`].
enum KeptRun<'a> {
    Text(&'a ReplayedText),
    Pattern(&'a PatternDraw),
    /// The layer a run painted as one image is drawn into and composited
    /// through. Composited by Ganesh, the layer stood up to 19 levels from
    /// the CPU canvas under `color-burn`, and 2 to 3 under `source-atop`,
    /// where the run drawn glyph by glyph on its backdrop stood none.
    Image(&'a Group),
}

impl KeptRun<'_> {
    /// What the run can change on a canvas whose transform is `device`,
    /// within `clip`: what it inks, the whole clip where its blend covers
    /// beyond the draw, or what it reaches where its ink cannot be had.
    fn reach(&self, device: &Matrix, clip: IRect) -> BackdropReach {
        match self {
            KeptRun::Text(text) if text.blend.is_some() => {
                BackdropReach::Beyond
            }
            KeptRun::Text(text) => text.ink_under(device, clip).map_or(
                BackdropReach::Local(text.reach()),
                BackdropReach::Device,
            ),
            KeptRun::Pattern(draw) => {
                BackdropReach::Device(draw.ink_under(device, clip))
            }
            // A layer under a blend other than `source-over` covers beyond
            // the draw; one under a blend that leaves the page alone where the
            // layer is transparent reaches what its contents reach, and those
            // are the run recorded into the layer's picture, which reaches the
            // whole clip it is recorded under -- see [`reach_the_clip`].
            KeptRun::Image(group) => {
                let Composite::Layer(rec) = &group.composite else {
                    return BackdropReach::Beyond;
                };
                let blend = rec
                    .paint
                    .as_ref()
                    .and_then(Paint::as_blend_mode)
                    .unwrap_or(BlendMode::SrcOver);
                match covers_beyond_the_draw(blend) {
                    true => BackdropReach::Beyond,
                    false => BackdropReach::Device(clip),
                }
            }
        }
    }
}

/// The run `entry` draws that [`replayed_on_their_backdrop`] draws on the
/// CPU -- see [`KeptRun`] -- and the bounds of the clip path it applies
/// itself, in the coordinates it is played back under, where it applies
/// one: as a piece of a clip's run -- see [`Clipped`] -- or under a clip
/// of its own, where the page keeps no run of the clip. Played onto the
/// raster, it applies that clip there as the CPU page applies it. `None`
/// for any other entry.
fn read_on_the_cpu(entry: &Layer) -> Option<(KeptRun<'_>, Option<Rect>)> {
    fn kept(entry: &Layer) -> Option<KeptRun<'_>> {
        match entry {
            Layer::ReplayedText(text) if text.reads_the_page() => {
                Some(KeptRun::Text(text))
            }
            Layer::Pattern(draw) if draw.reads_the_page() => {
                Some(KeptRun::Pattern(draw))
            }
            Layer::Group(group)
                if matches!(&group.composite, Composite::Layer(rec)
                    if rec.one_image) =>
            {
                Some(KeptRun::Image(group))
            }
            _ => None,
        }
    }
    match entry {
        Layer::Clipped(piece) => {
            Some((kept(piece.entry())?, Some(piece.bounds)))
        }
        entry => Some((kept(entry)?, clip_path_bounds(entry))),
    }
}

/// Plays `entries`, each asking for `features` at the same index, onto
/// `canvas` for `target`, in order: each as [`Layer::playback`] plays it,
/// but for consecutive pieces of one run played into a document or a
/// recording, which it plays under their clip once -- see [`Clipped`] --
/// all but a recording's cut draws. On the GPU it counts the layers it
/// renders offscreen, so that the playback flushes every so many -- see
/// [`pacing`].
///
/// A recording is what a canvas drawn into another, or used as a pattern,
/// is replayed into, and Skia rasterizes it later, where every clip applied
/// rebuilds its anti-aliased clip mask: one for each piece, where the page
/// applied one for the frame. A cut draw there clips by coverage, alone --
/// see [`CutOp::playback`] -- and the pieces between cut draws share one
/// clip. A document clips a cut draw by the path, among the run's pieces
/// under their one clip. A raster on the CPU plays the pieces between cut
/// draws under one clip, bounded by what they draw, as it bounds a piece
/// played alone, since it builds the clip's mask each time the clip is
/// applied: applied piece by piece, the clip was 7.3% of the samples of a
/// page of 40 runs, each a piece of one clip's run, drawn and read on the
/// CPU in a release build. A GPU canvas plays each piece under its own
/// clip.
///
/// On pixels it first adds to the colour glyph sheets the glyph images the
/// last playback drew again -- see [`grow_sheets`] -- so that a page adds
/// them once, before any of its runs draws; and it holds consecutive runs of
/// colour text open in one [`BatchScope`], so that those drawn from a sheet
/// are drawn together, before the next entry of another kind plays. On the
/// GPU it plays kept runs under a blend read in the shader on the CPU, over
/// one read back of the pixels they ink, with the entries between them that
/// meet none of them -- see [`replayed_on_their_backdrop`] -- closing an
/// open batch first, so that the read finds what it drew.
pub(crate) fn play_entries(
    entries: &[Layer],
    features: &[VectorFeatures],
    canvas: &SkCanvas,
    target: Target,
) {
    debug_assert_eq!(entries.len(), features.len(), "one feature set a layer");
    if target == Target::Pixels {
        grow_sheets();
    }
    let paced = target == Target::Pixels && blur_mask::on_gpu(canvas);
    let raster = target != Target::Document && blur_mask::draws_on_cpu(canvas);
    let grouped =
        target == Target::Document || !blur_mask::draws_on(canvas) || raster;
    let coverage = clips_by_coverage(canvas, target);
    // Only on a page holding a cut draw: a document walks the picture of a
    // page holding none as vectors -- see `Page::picture` -- and would write
    // the bound as a clip of its own. A raster bounds the clip as it bounds
    // a piece played alone -- see `Clipped::playback`.
    let bounded = match raster {
        true => bounds_page(),
        false => {
            target == Target::Pixels
                && !blur_mask::draws_on(canvas)
                && holds_a_cut(entries)
                && bounds_pieces()
        }
    };
    // On a raster, a cut draw clips itself, as it does played alone, and a
    // piece whose draws have no known rectangle plays alone, unbounded,
    // rather than leaving the pieces beside it unbounded with it.
    let shares_the_clip = |piece: &Clipped| {
        grouped
            && !((coverage || raster) && matches!(piece.entry, Layer::Cut(_)))
            && !(raster && piece.drawn().is_none())
    };
    // Consecutive runs of colour text drawn from one sheet are drawn
    // together -- see `BatchScope` -- the batch drawn before any other
    // entry plays.
    let mut batch = None;
    let mut at = 0;
    while at < entries.len() {
        if paced {
            // A batch of colour runs still open draws when it closes: closed
            // first, so that the read back finds what it drew.
            if cpu_run_at(entries, at).is_some() {
                batch = None;
            }
            let played = replayed_on_their_backdrop(
                &entries[at..],
                &features[at..],
                canvas,
            );
            if played > 0 {
                batch = None;
                at += played;
                continue;
            }
        }
        match &entries[at] {
            Layer::ReplayedText(_) if target == Target::Pixels => {
                batch.get_or_insert_with(|| BatchScope::open(canvas));
            }
            _ => batch = None,
        }
        if let Some(played) = play_shared_chains(
            &entries[at..],
            &features[at..],
            canvas,
            target,
            paced,
        ) {
            at += played;
            continue;
        }
        // Consecutive gradient runs on the GPU take their effects' images
        // from one atlas between them: draws asking a vector backend for
        // different things are runs of their own, and a run each made an
        // atlas each -- 2000 small gradients alternating linear and conic
        // took 6.7 times what Ganesh's own gradient did.
        if let Layer::Gradient(_) = &entries[at]
            && target == Target::Pixels
            && blur_mask::on_gpu(canvas)
            && on_whole_pixels(canvas)
        {
            let ops: Vec<&GradientDraw> = entries[at..]
                .iter()
                .map_while(|entry| match entry {
                    Layer::Gradient(op) => Some(&**op),
                    _ => None,
                })
                .collect();
            if ops.len() > 1 {
                let paths: Vec<Option<&GradientPath>> =
                    ops.iter().flat_map(|op| op.paths()).collect();
                let found = atlas(canvas, &paths);
                let mut from = 0;
                for (k, op) in ops.iter().enumerate() {
                    let to = from + op.draws.len();
                    op.played(canvas, target, Some(&found[from..to]));
                    from = to;
                    if let Some(features) =
                        features.get(at + k).filter(|_| paced)
                    {
                        pacing::played(
                            canvas,
                            offscreen_layers(&entries[at + k], *features),
                        );
                    }
                }
                at += ops.len();
                continue;
            }
        }
        let run = match &entries[at] {
            Layer::Clipped(piece) if shares_the_clip(piece) => piece.run,
            entry => {
                entry.playback(canvas, target);
                if let Some(features) = features.get(at).filter(|_| paced) {
                    pacing::played(canvas, offscreen_layers(entry, *features));
                }
                at += 1;
                continue;
            }
        };
        let pieces: Vec<&Clipped> = entries[at..]
            .iter()
            .map_while(|entry| match entry {
                Layer::Clipped(piece)
                    if piece.run == run && shares_the_clip(piece) =>
                {
                    Some(&**piece)
                }
                _ => None,
            })
            .collect();
        at += pieces.len();
        Clipped::play_run(&pieces, &pieces[0].clip, canvas, target, bounded);
    }
}

/// Plays the entries leading `entries` whose chains may share a layer --
/// see [`Group::shared_chain`] and [`BlurOp::shared_chain`] -- in as few
/// layers as [`chain::Plan::joins`] lets them share, and returns how many
/// entries that is. `None` where they run fewer than two chains, which then
/// play as they do alone. Only on Metal do any share, as
/// [`chain::shares_layers`] says why; elsewhere each takes a layer of its
/// own, as it would alone. Their shadows share passes either way -- see
/// [`chain::ShadowAtlas`].
///
/// Each chain is planned as it would be alone, with the content and the
/// depth it would have, and its layer holds its own draw where it reads, so
/// it lands the bytes it would alone; its result is composited in turn, as
/// it would be. Chains are coloured into layers -- see [`Run`] -- so that two
/// whose reads and draws meet never share one, while chains further apart
/// do: a row of blurred shapes each meeting the next takes two layers, not
/// one each.
///
/// Where `paced`, it counts each entry's layers as it plays them, as any
/// entry is counted -- see [`offscreen_layers`] -- and where the count is due
/// to flush, it first plays the run so far, so that the flush falls inside
/// the run, after the chains it is counted for -- see [`pacing`]. Counted
/// only once the run had played, the flushes all came after it, leaving
/// every chain's passes to the one before: 4,096 rects under a CSS drop
/// shadow, each a group of its own, read in 2.7 s that way and in 0.66 s
/// flushed inside, in a debug test build. A run cut short is played and
/// composited before the next chain draws, in draw order, and each chain
/// reads only its own layer, so the bytes are the ones the whole run lands.
///
/// Do not count each chain instead: a run of blurred rectangles joined in
/// one entry -- 2,000 cards under `blur(4px)` on 800x600 -- then flushed
/// every 256 chains, and its export took 9.7 ms against 6.0 played whole,
/// waiting on the GPU, where its clustering had cost nothing to speak of.
fn play_shared_chains(
    entries: &[Layer],
    features: &[VectorFeatures],
    canvas: &SkCanvas,
    target: Target,
    paced: bool,
) -> Option<usize> {
    // Each leading entry with the index of the entry it is, and of the one
    // after it that a shadow cast alone is followed by, which it takes.
    let mut leading: Vec<(Leading<'_>, usize, Option<usize>)> = Vec::new();
    let mut at = 0;
    while let Some(entry) = entries.get(at) {
        let lead = match entry {
            Layer::Group(group) => group
                .shared_chain(canvas, target)
                .map(|chain| Leading::Group(group, chain, None)),
            Layer::Blur(op) => op
                .shared_chain(canvas, target)
                .map(|(chain, paths)| Leading::Blur(op, chain, paths)),
            _ => None,
        };
        let Some(mut lead) = lead else { break };
        let mut after = None;
        if let Leading::Group(_, chain, then) = &mut lead
            && casts_alone(&chain.steps)
            && let Some(next) = entries.get(at + 1)
            && draws_plainly(next)
        {
            *then = Some(next);
            after = Some(at + 1);
        }
        leading.push((lead, at, after));
        at = after.unwrap_or(at) + 1;
    }
    let chains: usize = leading.iter().map(|(lead, ..)| lead.chains()).sum();
    if chains < 2 {
        return None;
    }
    #[cfg(test)]
    if ALONE_CHAINS.with(std::cell::Cell::get) {
        return None;
    }
    let to_device = canvas.local_to_device_as_3x3();
    let mut run = Run::default();
    for (entry, index, after) in &leading {
        for member in entry.members(&to_device) {
            let Some(plan) =
                chain::plan(canvas, member.steps, member.content, member.depth)
            else {
                // A chain with nothing to work on composites nothing, and
                // what it is followed by lands after the chains before it.
                if let Some(then) = member.then {
                    std::mem::take(&mut run).play(canvas, target);
                    then.playback(canvas, target);
                }
                continue;
            };
            let layer = match run.layer_for(&plan) {
                Some(layer) => layer,
                None => {
                    std::mem::take(&mut run).play(canvas, target);
                    0
                }
            };
            run.push(Chained { member, plan }, layer);
        }
        if paced {
            let layers = std::iter::once(*index)
                .chain(*after)
                .map(|i| offscreen_layers(&entries[i], features[i]))
                .sum();
            if pacing::due(layers) {
                std::mem::take(&mut run).play(canvas, target);
            }
            pacing::played(canvas, layers);
        }
    }
    run.play(canvas, target);
    Some(at)
}

/// Whether a chain of `steps` casts a shadow alone -- see [`Step::Shadow`]:
/// the group of a draw's `shadowColor` shadow, which the draw that casts it
/// follows as an entry of its own.
fn casts_alone(steps: &[Step]) -> bool {
    matches!(
        steps.last(),
        Some(Step::Shadow {
            shadow_only: true,
            ..
        })
    )
}

/// Whether `entry` draws onto the canvas as it is played, through no layer
/// or chain of its own: what a run of chains can play between two of its
/// composites -- see [`play_shared_chains`] -- since no chain reads the
/// canvas it is drawn onto.
fn draws_plainly(entry: &Layer) -> bool {
    matches!(
        entry,
        Layer::Picture(_) | Layer::Targeted(_) | Layer::Deferred(_)
    )
}

/// The most layers a [`Run`] places its chains in. A run draws each layer,
/// finishes its chains and lets it go once the next is drawn -- see
/// [`Run::play`] -- so it holds two layers at most whatever this is, and
/// beside each the layer its chains' casts are drawn in where it has one, but
/// every chain's result until it composites them all: at most this many
/// times [`chain::MAX_SHARED`] results, each the size of its chain's worked
/// rectangle, in the run's atlas or an image of its own. Chosen from what
/// scenes need: lines of text 21 pixels apart under a `shadowBlur` of 6 each
/// meet the two either side and take three, and 60 scattered rounded rectangles
/// under a `shadowBlur` of 12, turned 0.15 radians, took 20 layers at four, 13
/// at eight and 12 at sixteen or more.
pub(crate) const MAX_RUN_LAYERS: usize = 8;

/// Chains of consecutive entries, each placed in the first open layer whose
/// chains it can share with -- see [`chain::Plan::joins`] -- or in a new one,
/// and then all drawn, finished and composited in the order they were
/// drawn. Each chain reads its own layer and never the page, so drawing them
/// all before compositing any changes nothing they read; compositing them in
/// draw order, across layers, is what keeps overlapping results stacked as
/// they would be alone.
#[derive(Default)]
struct Run<'a> {
    chains: Vec<Chained<'a>>,
    /// Each open layer's chains, as indices into `chains`.
    layers: Vec<Vec<usize>>,
}

impl<'a> Run<'a> {
    /// The layer a chain planned as `plan` goes in: the first open one whose
    /// chains it can share with, or a new one -- the index past the last --
    /// while fewer than [`MAX_RUN_LAYERS`] are open. `None` where it shares
    /// none and no other can be opened, so that the run is played first and
    /// the chain starts the next.
    fn layer_for(&self, plan: &chain::Plan) -> Option<usize> {
        let fits = self.layers.iter().position(|layer| {
            let plans: Vec<&chain::Plan> =
                layer.iter().map(|&i| &self.chains[i].plan).collect();
            plan.joins(&plans)
        });
        fits.or(
            (self.layers.len() < MAX_RUN_LAYERS).then_some(self.layers.len())
        )
    }

    /// Places `chained` in `layer` -- see [`Self::layer_for`].
    fn push(&mut self, chained: Chained<'a>, layer: usize) {
        if layer == self.layers.len() {
            self.layers.push(Vec::new());
        }
        self.layers[layer].push(self.chains.len());
        self.chains.push(chained);
    }

    /// Draws each layer, and beside it a layer of the same plans holding the
    /// casts of the chains in it that cast their shadows from a run's copy
    /// -- see [`chain::Cast`] -- and finishes its chains -- those casting a
    /// blurred shadow together, in passes the whole run shares, see
    /// [`chain::ShadowAtlas`] -- letting each layer go once the next is
    /// drawn, then composites every chain onto `canvas` in the order they
    /// were placed. Where Skia refuses a shared layer or its casts' layer,
    /// or a run's shared passes cannot be made, each of the chains involved
    /// is drawn and finished in a layer of its own, its cast with it, as a
    /// chain alone is.
    fn play(self, canvas: &SkCanvas, target: Target) {
        let Run { chains, layers } = self;
        // Each chain's result where it was made.
        let mut outcomes: Vec<Option<Option<chain::Outcome>>> =
            (0..chains.len()).map(|_| None).collect();
        // The chains drawn and finished alone after all.
        let mut alone = vec![false; chains.len()];
        #[cfg(any(feature = "metal", feature = "vulkan"))]
        let mut atlas = {
            let planned: Vec<(&chain::Plan, &[Step], bool)> = chains
                .iter()
                .map(|chained| {
                    let casts = chained.member.cast.is_some();
                    (&chained.plan, chained.member.steps, casts)
                })
                .collect();
            canvas.recording_context().and_then(|context| {
                let max = context.max_render_target_size().max(1) as usize;
                #[cfg(test)]
                let max = ATLAS_SIDE.get().map_or(max, |side| side.min(max));
                chain::ShadowAtlas::plan(&planned, max)
            })
        };
        // The layer drawn last, and the layer its chains' shadows are cast
        // from, held until the next is drawn: let go first, the next takes
        // its surface back from the pool and draws into it while the GPU
        // still reads it for the chains before.
        let mut held: Option<(chain::Drawn, Option<chain::Drawn>)> = None;
        for layer in &layers {
            let plans: Vec<&chain::Plan> =
                layer.iter().map(|&i| &chains[i].plan).collect();
            let Some(mut drawn) =
                chain::Drawn::new(canvas, &plans, |k, offscreen| {
                    chains[layer[k]].member.draw.draw(offscreen, target);
                })
            else {
                if layer.len() > 1 {
                    layer.iter().for_each(|&i| alone[i] = true);
                }
                continue;
            };
            // The layer the casts of the chains that cast from a copy are
            // drawn into, planned as theirs, so that each reads its cast where
            // it reads its own draw; nothing is drawn for the others.
            let casts = layer
                .iter()
                .any(|&i| chains[i].member.cast.is_some())
                .then(|| {
                    chain::Drawn::new(canvas, &plans, |k, offscreen| {
                        if let Some((copy, _)) = &chains[layer[k]].member.cast {
                            copy.draw(offscreen, target);
                        }
                    })
                })
                .flatten();
            for &i in layer {
                let Chained { member, plan } = &chains[i];
                let finishing = (plan, member.steps, member.paint);
                // A chain whose cast's layer could not be made is finished
                // alone, as one refused a shared layer is.
                let cast = match (&member.cast, &casts) {
                    (Some((_, alpha)), Some(casts)) => Some((casts, *alpha)),
                    (Some(_), None) => {
                        alone[i] = true;
                        continue;
                    }
                    (None, _) => None,
                };
                #[cfg(test)]
                if cast.is_some() {
                    CAST_RUNS.with(|runs| runs.set(runs.get() + 1));
                }
                #[cfg(any(feature = "metal", feature = "vulkan"))]
                if let Some(atlas) = atlas.as_mut()
                    && drawn.stage(atlas, i, finishing, cast)
                {
                    continue;
                }
                let (plan, steps, paint) = finishing;
                outcomes[i] = Some(match cast {
                    Some(cast) => {
                        drawn.finish_drawn_cast(plan, steps, paint, cast)
                    }
                    None => drawn.finish(plan, steps, paint),
                });
            }
            held = Some((drawn, casts));
        }
        drop(held);
        #[cfg(any(feature = "metal", feature = "vulkan"))]
        if let Some(atlas) = atlas {
            let (done, failed) =
                atlas.finish(canvas.recording_context().as_mut());
            for (i, outcome) in done {
                outcomes[i] = Some(Some(outcome));
            }
            failed.into_iter().for_each(|i| alone[i] = true);
        }
        for ((chained, outcome), alone) in
            chains.iter().zip(outcomes).zip(alone)
        {
            match outcome {
                Some(Some(outcome)) => {
                    #[cfg(test)]
                    pacing::tests::chain_finished();
                    outcome.composite(canvas, chained.member.paint);
                }
                // A chain whose result is nothing composites nothing.
                Some(None) => {
                    #[cfg(test)]
                    pacing::tests::chain_finished();
                }
                None if alone => {
                    let plans = [&chained.plan];
                    let layer =
                        chain::Drawn::new(canvas, &plans, |_, layer| {
                            chained.member.draw.draw(layer, target);
                        });
                    if let Some(mut layer) = layer {
                        chained.finish(&mut layer, canvas, target);
                    }
                }
                None => {}
            }
            if let Some(then) = chained.member.then {
                then.playback(canvas, target);
            }
        }
    }
}

/// What [`Group::shared_chain`] gives a chain that may share its layer.
struct SharedChain {
    steps: Arc<[Step]>,
    paint: Paint,
    /// What a run of text the group holds can ink -- see
    /// [`Composite::Chain`].
    reach: Option<Rect>,
    /// The run's opaque copy and the alpha the run is drawn at, where it is
    /// blurred from it or casts its shadow from it -- see [`CoverageRun`].
    copy: Option<(Picture, f32)>,
    /// Whether the chain is blurred from `copy`, drawn in the run's place,
    /// rather than casting its shadow from it -- see [`chain::Cast`].
    blurs: bool,
}

/// An entry [`play_shared_chains`] plays: a group through a chain, or a
/// blurred draw whose blur is a chain for each of its `paths`.
enum Leading<'a> {
    /// A group through a chain, and the entry played after its result
    /// where the group casts a shadow alone -- see [`casts_alone`].
    Group(&'a Group, SharedChain, Option<&'a Layer>),
    Blur(&'a BlurOp, BlurChain, Vec<Path>),
}

impl Leading<'_> {
    /// How many chains the entry runs.
    fn chains(&self) -> usize {
        match self {
            Leading::Group(..) => 1,
            Leading::Blur(_, _, paths) => paths.len(),
        }
    }

    /// The entry's chains, in the order it runs them alone, on a canvas
    /// that maps the entry's coordinates to the device by `to_device`.
    fn members(&self, to_device: &Matrix) -> Vec<Member<'_>> {
        match self {
            Leading::Group(
                group,
                SharedChain {
                    steps,
                    paint,
                    reach,
                    copy,
                    blurs,
                },
                then,
            ) => {
                let content = group.chain_content(
                    *reach,
                    &Matrix::new_identity(),
                    to_device,
                );
                #[cfg(test)]
                CHAIN_CONTENT
                    .set(content.map(|content| to_device.map_rect(content).0));
                let copied = copy
                    .as_ref()
                    .map(|(copy, alpha)| (Draw::Copy(group, copy), *alpha));
                let (draw, cast) = match (copied, blurs) {
                    (Some((copied, _)), true) => (copied, None),
                    (cast, _) => (Draw::Group(group), cast),
                };
                vec![Member {
                    draw,
                    cast,
                    steps,
                    content,
                    depth: Depth::Floats,
                    paint,
                    then: *then,
                }]
            }
            Leading::Blur(op, chain, paths) => paths
                .iter()
                .map(|path| Member {
                    draw: Draw::Path(op, path, &chain.draw),
                    cast: None,
                    steps: &chain.steps,
                    content: Some(op.content(path)),
                    depth: chain.depth,
                    paint: &chain.gain,
                    then: None,
                })
                .collect(),
        }
    }
}

/// One chain [`play_shared_chains`] runs: what it draws into its layer, and
/// what it draws into a layer of the same plan for its shadow to be cast
/// from, at what alpha, where its shadow is cast from that -- see
/// [`chain::Cast`]; its steps, what bounds what it draws, the depth of its
/// layer and the paint it composites through, whose alpha the last step
/// takes; and `then`, the entry played as it is right after its result is
/// composited -- the draw that casts a shadow the chain makes alone.
struct Member<'a> {
    draw: Draw<'a>,
    cast: Option<(Draw<'a>, f32)>,
    steps: &'a [Step],
    content: Option<Rect>,
    depth: Depth,
    paint: &'a Paint,
    then: Option<&'a Layer>,
}

/// What a [`Member`] draws into its layer: a group's entries, a run's
/// opaque copy where the group's run is -- see [`CoverageRun`] -- or one
/// path of a blurred draw, through its paint.
enum Draw<'a> {
    Group(&'a Group),
    Copy(&'a Group, &'a Picture),
    Path(&'a BlurOp, &'a Path, &'a Paint),
}

impl Draw<'_> {
    /// Draws onto `layer`, which carries the transform from canvas pixels to
    /// its own.
    fn draw(&self, layer: &SkCanvas, target: Target) {
        match self {
            Draw::Group(group) => {
                group.draw_chained(layer, target, &Matrix::new_identity());
            }
            Draw::Copy(group, copy) => {
                let identity = Matrix::new_identity();
                group.draw_chained_as(layer, target, &identity, |layer| {
                    layer.draw_picture(copy, None, None);
                });
            }
            Draw::Path(op, path, paint) => op.draw_into(layer, path, paint),
        }
    }
}

/// A [`Member`] planned.
struct Chained<'a> {
    member: Member<'a>,
    plan: chain::Plan,
}

impl Chained<'_> {
    /// Finishes this chain from `drawn`, the layer holding its draw, its
    /// shadow cast from a layer of its own where it is cast from a copy, and
    /// composites the result onto `canvas`.
    fn finish(
        &self,
        drawn: &mut chain::Drawn,
        canvas: &SkCanvas,
        target: Target,
    ) {
        #[cfg(test)]
        pacing::tests::chain_finished();
        let Chained { member, plan } = self;
        #[cfg(test)]
        if member.cast.is_some() {
            CAST_RUNS.with(|runs| runs.set(runs.get() + 1));
        }
        let outcome = match &member.cast {
            Some((copy, alpha)) => drawn.finish_cast(
                plan,
                member.steps,
                member.paint,
                Some(chain::Cast {
                    canvas,
                    draw: &|layer| copy.draw(layer, target),
                    alpha: *alpha,
                    run: None,
                }),
            ),
            None => drawn.finish(plan, member.steps, member.paint),
        };
        if let Some(outcome) = outcome {
            outcome.composite(canvas, member.paint);
        }
    }
}

/// The layers a GPU renders offscreen for `entry`, asking for `features` of
/// a vector backend, besides those of the entries it holds, which a playback
/// counts as it plays them, as it paces its flushes by them -- see
/// [`pacing`]: one for a group, which opens a layer on the GPU; for an
/// image-filtered picture
/// its ops, which bound its draws from above -- a filled rectangle under an
/// image filter records one, under a shadow five -- and one for any other
/// image-filtered entry; and none for any other.
fn offscreen_layers(entry: &Layer, features: VectorFeatures) -> usize {
    match entry {
        Layer::Group(_) => 1,
        _ if !features.refused_by(VectorFeatures::IMAGE_FILTER) => 0,
        Layer::Picture(picture) => picture.approximate_op_count_nested(true),
        Layer::Targeted(pictures) => {
            pictures.0.approximate_op_count_nested(true)
        }
        Layer::Split(split) => split
            .pixels
            .iter()
            .map(|(entry, features)| offscreen_layers(entry, features))
            .sum(),
        Layer::Deferred(deferred) => deferred
            .picture()
            .map_or(0, |picture| picture.approximate_op_count_nested(true)),
        // Pixels draw a run's shadow filtered on the CPU and its colour
        // glyphs as images, so its recording's layers are never the GPU's.
        _ => 1,
    }
}

/// A fill or stroke its clip cuts, or a run of them under one clip: the
/// draws, and the clip as coverage.
///
/// Skia's analytic anti-aliasing rasterizes a path against the clip's bounds
/// where they do not contain it, adding edges at the cut, and the strips it
/// steps a row in are cut at every edge's ends: so the edges the clip adds
/// move the coverage of every edge in the rows they span, however far from
/// the cut. A triangle cut 60 pixels right of its left edge drew that edge
/// up to 19 levels away from how it drew it uncut, and on the edge pixels
/// the cut draw was 4.60 levels from the exact coverage on average where
/// the uncut one was 1.55 (`SkScan_AAAPath.cpp`, `containedInClip`).
///
/// On a raster surface on the CPU this applies the clip as a clip shader
/// instead -- a picture of the clip filled, which Skia rasterizes at the
/// scale it is drawn at -- so the device clip leaves the path whole, the
/// draw is the unclipped draw, and the clip multiplies its coverage. Inside
/// the clip the coverage is the uncut draw's, byte for byte, at any density;
/// at its edge it is the draw's times the clip's. The blend can land a level
/// away on an eight-bit sRGB page, where the clip shader takes the draw off
/// Skia's legacy blitter onto its raster pipeline -- see
/// docs/browser-differences.md. A recording bound for pixels -- what a
/// canvas drawn into another, or used as a pattern, is replayed into --
/// applies the clip the same way, so the draw is as exact wherever Skia
/// rasterizes it. Anywhere else -- the GPU, whose multisampled coverage the
/// clip does not move, a vector document -- it clips as the page always
/// has. A run of cut draws shares one op. Under a page's clip the op is a
/// piece of the frame's run -- see [`Clipped`] -- and a document plays its
/// draws under the run's clip with the draws around them.
#[derive(Clone, Debug)]
pub struct CutOp {
    /// The draws, in order, each under the transform it was drawn with: the
    /// only recording of a page that keeps one, or the one played as pixels
    /// on a page that keeps two -- see `page::Recording`.
    draws: Vec<Picture>,
    /// On a page that keeps two recordings, the same draws as recorded for a
    /// document.
    documents: Option<Vec<Picture>>,
    /// The clip, in device pixels of the recording, for a playback that
    /// clips with it.
    clip: Data,
    /// The clip as coverage, for a playback that applies it so: one for a
    /// run, shared by every cut draw in it.
    coverage: Arc<Coverage>,
    /// How a GPU canvas draws each of `draws` dyed by a gradient, in the
    /// bytes the CPU draws it in -- see [`OnGpu`]; `None` for one it plays
    /// as recorded.
    gpu: Vec<Option<OnGpu>>,
}

/// A clip filled, anti-aliased, as coverage a CPU raster playback is clipped
/// by: the path drawn whole and multiplied by it rather than rasterized
/// against it -- see [`CutOp`].
#[derive(Clone, Debug)]
pub struct Coverage {
    /// The clip filled, its tile's corner at the origin.
    picture: Picture,
    /// The pixels the clip covers any part of, from the origin, which the
    /// picture is tiled over and is transparent outside.
    tile: Rect,
    /// Where the tile's corner lies in device pixels of the recording.
    corner: Matrix,
}

impl Coverage {
    /// `clip`, in device pixels of the recording, as coverage; `None` where
    /// a picture could not be made.
    pub fn of(clip: &Path) -> Option<Self> {
        // Recorded with the clip's corner at the origin, where a picture
        // shader puts its tile, and moved back by the shader's matrix.
        let corner: IRect = clip.bounds().round_out();
        let tile = Rect::from_wh(corner.width() as f32, corner.height() as f32);
        let mut recorder = PictureRecorder::new();
        let mut paint = Paint::default();
        paint.set_anti_alias(true);
        let canvas = recorder.begin_recording(tile, false);
        canvas.translate((-corner.left as f32, -corner.top as f32));
        canvas.draw_path(clip, &paint);
        Some(Coverage {
            picture: recorder.finish_recording_as_picture(None)?,
            tile,
            corner: Matrix::translate((corner.left as f32, corner.top as f32)),
        })
    }

    /// Clips `canvas` by the coverage, under its transform: rasterized at
    /// the density it is played at, at the device's own phase.
    pub fn clip(&self, canvas: &SkCanvas) {
        canvas.clip_shader(self.shader(), ClipOp::Intersect);
    }

    /// Multiplies what `canvas` holds by the coverage, under its transform:
    /// a pixel the coverage covers whole keeps its bytes, multiplied by
    /// one, and a pixel it misses is cleared.
    pub fn multiply(&self, canvas: &SkCanvas) {
        let mut paint = Paint::default();
        paint
            .set_shader(self.shader())
            .set_blend_mode(BlendMode::DstIn);
        canvas.draw_paint(&paint);
    }

    /// The coverage as a shader: the picture tiled once, transparent past
    /// its tile.
    fn shader(&self) -> Shader {
        self.picture.to_shader(
            (TileMode::Decal, TileMode::Decal),
            FilterMode::Nearest,
            Some(&self.corner),
            Some(&self.tile),
        )
    }
}

/// Whether a cut draw played onto `canvas` for `target` applies its clip as
/// coverage -- see [`CutOp`]: on a raster surface on the CPU, and in a
/// recording bound for pixels, which Skia rasterizes later as an image's,
/// a pattern's or a canvas's drawn into another, on whichever surface that
/// is drawn on. Not on a GPU surface, and not in a document, which has no
/// way to write a clip by coverage: Skia's SVG device writes one as an
/// empty `clipPath`, hiding what it clips (`SkSVGDevice.cpp`,
/// `DeviceSpaceType::kShader`).
fn clips_by_coverage(canvas: &SkCanvas, target: Target) -> bool {
    blur_mask::draws_on_cpu(canvas)
        || (target == Target::Pixels && !blur_mask::draws_on(canvas))
}

/// What `picture`'s draws reach, as Skia bounds them: the cull rectangle a
/// recording begun with a bounding hierarchy is trimmed to when it finishes
/// (`SkPictureRecorder::finishRecordingAsPicture`), the draws replayed into
/// one. A picture drawn inside it is bounded by its own cull rectangle.
pub(crate) fn drawn_by(picture: &Picture) -> Option<Rect> {
    let mut recorder = PictureRecorder::new();
    let canvas = recorder.begin_recording(picture.cull_rect(), true);
    picture.playback(canvas);
    Some(recorder.finish_recording_as_picture(None)?.cull_rect())
}

/// Whether a run's pieces are bounded -- see [`Clipped::play_run`]: always,
/// but off on a thread a test has turned it off on.
#[cfg(not(test))]
fn bounds_pieces() -> bool {
    true
}

/// Whether a run's pieces are bounded -- see [`Clipped::play_run`]: unless
/// this thread's test turned [`BOUNDS_PIECES`] off.
#[cfg(test)]
fn bounds_pieces() -> bool {
    BOUNDS_PIECES.get()
}

/// Whether a piece played onto a raster on the CPU is bounded -- see
/// [`Clipped::playback`]: always, but off on a thread a test has turned it
/// off on.
#[cfg(not(test))]
fn bounds_page() -> bool {
    true
}

/// Whether a piece played onto a raster on the CPU is bounded -- see
/// [`Clipped::playback`]: unless this thread's test turned [`BOUNDS_PAGE`]
/// off.
#[cfg(test)]
fn bounds_page() -> bool {
    BOUNDS_PAGE.get()
}

/// Whether `entries`, or a group among them, hold a [`CutOp`].
pub(crate) fn holds_a_cut(entries: &[Layer]) -> bool {
    entries.iter().any(|entry| match entry {
        Layer::Cut(_) => true,
        Layer::Group(group) => holds_a_cut(&group.contents.layers),
        Layer::Clipped(piece) => matches!(piece.entry, Layer::Cut(_)),
        Layer::Erase(erase) => holds_a_cut(std::slice::from_ref(&erase.entry)),
        Layer::Split(split) => {
            holds_a_cut(&split.pixels.layers)
                || holds_a_cut(&split.document.layers)
        }
        Layer::Picture(_)
        | Layer::Blur(_)
        | Layer::Targeted(_)
        | Layer::Pattern(_)
        | Layer::Deferred(_)
        | Layer::ReplayedText(_)
        | Layer::Gradient(_) => false,
    })
}

/// Whether `entries`, or a group among them, hold a pair of recordings --
/// one for pixels and one for documents, which a page that keeps a
/// recording for pixels files, or a [`Split`] -- so that a picture of the
/// page played for one target does not stand for the other:
/// [`Page::picture`]'s holds the colours placed for pixels, and a split's
/// pixels.
pub(crate) fn holds_a_pair(entries: &[Layer]) -> bool {
    entries.iter().any(|entry| match entry {
        Layer::Targeted(_) | Layer::Split(_) => true,
        Layer::Deferred(deferred) => deferred.is_pair(),
        Layer::ReplayedText(text) => text.is_pair(),
        Layer::Gradient(op) => op.is_pair(),
        Layer::Cut(op) => op.documents.is_some(),
        Layer::Group(group) => holds_a_pair(&group.contents.layers),
        Layer::Erase(erase) => holds_a_pair(std::slice::from_ref(&erase.entry)),
        Layer::Clipped(piece) => match &piece.entry {
            Layer::Targeted(_) => true,
            Layer::Deferred(deferred) => deferred.is_pair(),
            Layer::ReplayedText(text) => text.is_pair(),
            Layer::Gradient(op) => op.is_pair(),
            Layer::Cut(op) => op.documents.is_some(),
            _ => false,
        },
        Layer::Picture(_) | Layer::Blur(_) | Layer::Pattern(_) => false,
    })
}

impl CutOp {
    /// The draw `drawn` holds, under `clip`, in device pixels of the
    /// recording, played as pixels through `coverage`, and on a GPU canvas
    /// as `gpu` says where it is given: a picture, or on a page that keeps
    /// a recording for each kind of target a pair of them; `None` for
    /// anything else.
    pub fn new(
        clip: &ClipData,
        coverage: Arc<Coverage>,
        drawn: Layer,
        gpu: Option<OnGpu>,
    ) -> Option<Self> {
        let (draws, documents) = match drawn {
            Layer::Picture(picture) => (vec![picture], None),
            Layer::Targeted(pair) => {
                (vec![pair.0.clone()], Some(vec![pair.1.clone()]))
            }
            _ => return None,
        };
        Some(CutOp {
            draws,
            documents,
            clip: clip.data.clone(),
            coverage,
            gpu: vec![gpu],
        })
    }

    /// Adds the draw `drawn` holds to the run, under the op's clip, into each
    /// recording the op keeps, and how a GPU canvas draws it. A draw that
    /// holds other than the recordings the op keeps is dropped, so that the
    /// two hold the same draws.
    ///
    /// Returns the bytes the pictures it took hold -- see
    /// [`Layer::recorded_bytes`].
    pub fn extend(&mut self, drawn: Layer, gpu: Option<OnGpu>) -> usize {
        let bytes = match (drawn, &mut self.documents) {
            (Layer::Picture(picture), None) => {
                let bytes = picture.approximate_bytes_used();
                self.draws.push(picture);
                bytes
            }
            (Layer::Targeted(pair), Some(documents)) => {
                self.draws.push(pair.0.clone());
                documents.push(pair.1.clone());
                pair.0.approximate_bytes_used()
                    + pair.1.approximate_bytes_used()
            }
            _ => return 0,
        };
        let bytes = bytes + gpu.as_ref().map_or(0, OnGpu::recorded_bytes);
        self.gpu.push(gpu);
        bytes
    }

    /// What the draws paint before the clip applies, in device pixels of
    /// the recording: the rectangle a layer holding the op is confined to.
    /// The recording played as pixels, as for a pair -- a document is never
    /// confined. Recorded inside a `saveLayer` the draws keep a bounding
    /// hierarchy, so this is what they draw; on the page they do not, and it
    /// is the page.
    ///
    /// Not the clip's bounds, though nothing the op draws shows outside
    /// them: a layer confined to the clip has its edge there, Skia's
    /// analytic anti-aliasing builds the path's edges against the layer's
    /// bounds as against a clip, and a triangle cut at x 82 in a layer so
    /// confined moved 57 pixels far inside the clip -- the route undone.
    pub fn reach(&self) -> Rect {
        self.draws.iter().fold(Rect::new_empty(), |mut all, draw| {
            all.join(draw.cull_rect());
            all
        })
    }

    /// Whether this op and `other` share one coverage picture, as the cut
    /// draws of a run do.
    #[cfg(test)]
    pub(crate) fn shares_coverage_with(&self, other: &CutOp) -> bool {
        Arc::ptr_eq(&self.coverage, &other.coverage)
    }

    /// The draws recorded for `target`.
    fn draws_for(&self, target: Target) -> &[Picture] {
        match (target, &self.documents) {
            (Target::Document, Some(documents)) => documents,
            _ => &self.draws,
        }
    }

    /// Draws the draws for `target` onto `canvas` under the clip the canvas
    /// already carries, for a run of pieces played under their clip once --
    /// see [`Clipped`].
    fn playback_unclipped(&self, canvas: &SkCanvas, target: Target) {
        self.play_draws(canvas, target);
    }

    /// Draws the draws for `target` onto `canvas`: on a GPU canvas, each
    /// dyed by a gradient as its [`OnGpu`] says.
    fn play_draws(&self, canvas: &SkCanvas, target: Target) {
        let gpu = target == Target::Pixels && blur_mask::on_gpu(canvas);
        let found = match gpu && on_whole_pixels(canvas) {
            true => atlas(
                canvas,
                &self
                    .gpu
                    .iter()
                    .map(|on_gpu| match on_gpu {
                        Some(OnGpu::Path(draw)) => Some(draw.as_ref()),
                        _ => None,
                    })
                    .collect::<Vec<_>>(),
            ),
            false => vec![],
        };
        let draws = self.draws_for(target).iter().zip(&self.gpu);
        let mut batch = exact_composite::Batch::default();
        for (index, (draw, on_gpu)) in draws.enumerate() {
            match on_gpu.as_ref().filter(|_| gpu) {
                Some(on_gpu) => {
                    canvas.save();
                    played_on_gpu(
                        canvas,
                        &GradientPixels::Recorded(draw.clone()),
                        on_gpu,
                        found.get(index).and_then(Option::as_ref),
                        &mut batch,
                    );
                    canvas.restore();
                }
                None => {
                    batch.flush(canvas);
                    canvas.draw_picture(draw, None, None);
                }
            }
        }
        batch.flush(canvas);
    }

    /// Draws the cut draws for `target` onto `canvas`, under whatever
    /// transform and clip the canvas already carries, the op's clip applied
    /// as coverage where [`clips_by_coverage`] says so and by the path
    /// elsewhere.
    pub fn playback(&self, canvas: &SkCanvas, target: Target) {
        canvas.save();
        if clips_by_coverage(canvas, target) {
            self.coverage.clip(canvas);
        } else if let Some(clip) = Path::deserialize(&self.clip) {
            canvas.clip_path(&clip, ClipOp::Intersect, true);
        }
        self.play_draws(canvas, target);
        canvas.restore();
    }
}

/// One blurred draw as `Context2D::draw_path` hands it to the recorder: what
/// it draws and how it paints, before the recorder decides whether it opens
/// a [`BlurOp`] or joins the one before it.
pub struct BlurDraw {
    geometry: Geometry,
    paint: BlurPaint,
}

impl BlurDraw {
    /// The draw of `path` with `paint`, blurred by `sigma` canvas pixels,
    /// onto a canvas whose pixels do with a faint paint what `faint` says.
    /// `rect` is what `Path::is_rect` answered for `path`, asked once by the
    /// caller. `paint` gives the colour and how the path is painted -- a
    /// paint with a shader or a path effect is not one this keeps -- and its
    /// filters, which this replaces, are not read.
    pub fn of(
        path: &Path,
        rect: Option<(Rect, bool, PathDirection)>,
        paint: &Paint,
        sigma: f32,
        faint: Faint,
    ) -> Self {
        let style = paint.style();
        // A fill reads none of these, so it is spared asking for them.
        let stroke = (style != PaintStyle::Fill).then(|| Stroke {
            width: paint.stroke_width(),
            miter: paint.stroke_miter(),
            cap: paint.stroke_cap(),
            join: paint.stroke_join(),
        });
        // A rectangle as Skia reports one: closed, so that a stroke joins
        // its last corner as the path did, and not inverse-filled.
        let rect = rect
            .filter(|(_, closed, _)| *closed)
            .filter(|_| !path.is_inverse_fill_type());
        let geometry = match rect {
            Some((rect, _, direction)) => Geometry::Rect(rect, direction),
            None => Geometry::Path(KeptPath::of(path)),
        };
        BlurDraw {
            geometry,
            paint: BlurPaint {
                color: paint.color4f(),
                style,
                stroke,
                anti_alias: paint.is_anti_alias(),
                dither: paint.is_dither(),
                sigma,
                faint,
                exact: None,
            },
        }
    }

    /// This draw, painted as pixels in `exact` where it is given -- see
    /// [`ExactColour`].
    pub fn exact(mut self, exact: Option<ExactColour>) -> Self {
        self.paint.exact = exact;
        self
    }
}

/// How a blurred draw paints: everything but its geometry and its frame.
/// Draws that agree in all of it, under one frame, are one run.
#[derive(Clone, Debug, PartialEq)]
struct BlurPaint {
    /// The draw's colour, global alpha included, unpremultiplied, in sRGB:
    /// `Paint::set_color4f` converts to it whatever space it was set in.
    color: Color4f,
    /// How the path is painted, and its stroke where it has one.
    style: PaintStyle,
    stroke: Option<Stroke>,
    anti_alias: bool,
    dither: bool,
    /// The blur's standard deviation in canvas pixels: in pixels of the
    /// canvas whatever its transform, as a filter's length is.
    sigma: f32,
    /// What the recording canvas's pixels do with a paint too faint for
    /// eight bits, decided when it was recorded as every other draw's is.
    faint: Faint,
    /// The colour held exactly in the page's space, on a page that keeps a
    /// recording for pixels, which the op paints with when played as pixels.
    exact: Option<ExactColour>,
}

/// A run of blurred rectangles, or one blurred path, in one flat colour and
/// one frame, blurred by a `blur()` filter.
///
/// Holds what the draws need and nothing Skia would have to fix early: the
/// geometry and the frame it was drawn in -- the transform and the clip --
/// and the blur's standard deviation in canvas pixels. A page is read from
/// several threads at once and Skia's `Path` is not `Sync`, so geometry that
/// is not a rectangle is kept behind a lock -- see [`KeptPath`] -- and the
/// clip serialized as `Data`, which is `Sync`.
///
/// Consecutive rectangles drawn alike join one op, so that a run of them
/// costs the recording one entry and its playback one frame, one clip and
/// one filter: on a page of 2,000 blurred cards that makes the CPU export
/// about 4% faster than an op each. Each is still drawn on its own, so
/// overlapping ones composite as separate draws do.
#[derive(Clone, Debug)]
pub struct BlurOp {
    geometry: Geometries,
    /// The transform it was drawn under.
    matrix: Matrix,
    /// The clip it was drawn under, in device pixels of the recording.
    clip: Option<Data>,
    paint: BlurPaint,
    /// The blur the first playback made, and the device sigma it was made
    /// for. A page is played back again for every export and every tile,
    /// mostly at one scale, and the filter is the same object each time; a
    /// playback at another scale makes its own. Held inline rather than
    /// behind an allocation, since every blurred draw may record one.
    made: OnceLock<(u32, Made)>,
}

/// One geometry, or a run of rectangles, without allocating for the one.
#[derive(Clone, Debug)]
enum Geometries {
    One(Geometry),
    Many(Vec<Geometry>),
}

impl Geometries {
    fn first(&self) -> &Geometry {
        match self {
            Geometries::One(geometry) => geometry,
            // SAFETY: a `Many` is made from a `One` and a second geometry,
            // and nothing removes from it.
            Geometries::Many(all) => &all[0],
        }
    }

    fn iter(&self) -> std::slice::Iter<'_, Geometry> {
        match self {
            Geometries::One(geometry) => std::slice::from_ref(geometry).iter(),
            Geometries::Many(all) => all.iter(),
        }
    }

    fn push(&mut self, geometry: Geometry) {
        match self {
            Geometries::One(first) => {
                *self = Geometries::Many(vec![first.clone(), geometry]);
            }
            Geometries::Many(all) => all.push(geometry),
        }
    }
}

/// What a [`BlurOp`] draws: a rectangle, which nearly every blurred draw is
/// and which is plain data, or any other path, kept parsed.
#[derive(Clone, Debug)]
enum Geometry {
    /// A closed rectangle, drawn as the path `Path::rect` makes of it.
    Rect(Rect, PathDirection),
    Path(KeptPath),
}

impl Geometry {
    fn path(&self) -> Option<Path> {
        match self {
            Geometry::Rect(rect, direction) => {
                Some(Path::rect(rect, *direction))
            }
            Geometry::Path(kept) => Some(kept.path()),
        }
    }
}

/// A path a page keeps for every playback of it, shared by the threads that
/// play it.
///
/// Skia's `Path` is `Send` and not `Sync`, so it is held behind a lock, and
/// each playback takes a copy of its own. A copy is one reference count: in
/// Skia M156 an `SkPath` is a reference to an immutable `SkPathData` -- its
/// points, verbs and bounds -- and a fill type (`include/core/SkPath.h`,
/// `src/core/SkPathData.h`), so nothing is parsed and nothing allocated, where
/// a path kept serialized is parsed again, and its points copied, on every
/// draw.
#[derive(Debug)]
struct KeptPath(Mutex<Path>);

impl KeptPath {
    fn of(path: &Path) -> Self {
        KeptPath(Mutex::new(path.clone()))
    }

    /// The path, a copy sharing its points with the one kept.
    fn path(&self) -> Path {
        self.0.lock().clone()
    }
}

impl Clone for KeptPath {
    fn clone(&self) -> Self {
        KeptPath(Mutex::new(self.path()))
    }
}

/// The blur a playback made: on the coverage, or on the result -- with
/// Skia's filter for a document, which [`chain`] replaces on pixels.
#[derive(Clone, Debug)]
enum Made {
    Coverage(Option<MaskFilter>),
    Result(Option<ImageFilter>),
}

/// How a stroke is drawn, where the draw strokes.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Stroke {
    width: f32,
    miter: f32,
    cap: PaintCap,
    join: PaintJoin,
}

/// How far a playback onto `canvas` scales a canvas pixel: the density, a
/// nested canvas's size. A recording's own transform is not part of it,
/// since a filter's length does not follow the canvas transform.
fn replay_scale(canvas: &SkCanvas) -> f32 {
    canvas
        .local_to_device_as_3x3()
        .map_radius(1.0)
        .filter(|s| s.is_normal() && *s > 0.0)
        .unwrap_or(1.0)
}

/// Whether a blur in `exact`, a colour placed on the bytes an eight-bit page
/// stores, is made on `canvas` from its coverage alone -- see
/// [`chain::coverage`]: the half of the route's conditions a draw asks as it
/// is played. A blurred path asks it in [`BlurOp::coverage_colour`] and a run
/// of text in [`Composite::Chain`]'s playback; a run asks [`coverage_files`]
/// as it is recorded, which keeps it from recording a copy this would never
/// play.
///
/// `false` on a page that is not eight bits a channel in RGBA or BGRA; on a
/// canvas that is not a raster on the CPU -- the GPU, a recording, a
/// document; on a page in another space than the colour was held in, as a
/// canvas drawn into one in another space is; and for a dithered paint,
/// whose colour varies from pixel to pixel. Any sigma: through the FFT, as
/// tap by tap, the route rounds each value once from the exact product, but
/// for exact ties.
fn coverage_plays(
    canvas: &SkCanvas,
    exact: &ExactColour,
    dither: bool,
) -> bool {
    #[cfg(test)]
    if !COVERAGE_ROUTE.with(Cell::get) {
        return false;
    }
    let same_space =
        canvas.image_info().color_space().as_ref() == Some(&exact.space);
    eight_bits_on_cpu(canvas) && same_space && !dither
}

/// Whether `canvas` is a raster on the CPU of eight bits a channel in RGBA
/// or BGRA: where a blur's alpha, stored on a byte, could be rounded to one
/// before the blur as well as after it.
fn eight_bits_on_cpu(canvas: &SkCanvas) -> bool {
    eight_bits(canvas) && blur_mask::draws_on_cpu(canvas)
}

/// Whether `canvas` holds eight bits a channel in RGBA or BGRA, on either
/// engine: where a chain's layer, which takes the canvas's type -- see
/// `chain::Plan` -- stores an alpha on a byte.
fn eight_bits(canvas: &SkCanvas) -> bool {
    matches!(
        canvas.image_info().color_type(),
        ColorType::RGBA8888 | ColorType::BGRA8888
    )
}

/// Whether a chain of `steps` casts its shadow from a run's opaque copy
/// where the run records one -- see [`coverage_files`] and [`chain::Cast`]:
/// a sole `drop-shadow()`, drawn under the run.
fn casts_from_copy(steps: &[Step]) -> bool {
    matches!(
        steps,
        [Step::Shadow {
            shadow_only: false,
            ..
        }]
    )
}

/// Whether a run of text filed under `steps`, in `exact` at `alpha`
/// through a paint `dither`ed or not, records the opaque copy a page on
/// eight bits draws it again in -- see [`CoverageRun`]: one under a sole
/// `blur()`, in a colour placed on the bytes an eight-bit page stores,
/// undithered, which [`coverage_plays`] plays on the CPU and a chain blurs
/// elsewhere; and one under a sole `drop-shadow()`, translucent, whose
/// shadow is cast from the copy -- see [`chain::Cast`] -- on a page
/// compositing in eight bits. Dither leaves a pixel's alpha alone, which is
/// all a shadow is cast from. A run that could never draw the copy is
/// recorded once.
pub(crate) fn coverage_files(
    steps: &[Step],
    exact: &ExactColour,
    dither: bool,
    alpha: f32,
) -> bool {
    match steps {
        [Step::Blur(_)] => exact.bytes && !dither,
        [
            Step::Shadow {
                shadow_only: false, ..
            },
        ] => exact.bytes && byte_alpha(alpha) < 1.0,
        _ => false,
    }
}

/// The chain a [`BlurOp`] blurs its result through on pixels -- see
/// [`BlurOp::result_chain`]: its steps, the paint each path is drawn into
/// the layer through, the paint the result composites through, whose alpha
/// is the last step's gain, and the depth of the layer.
struct BlurChain {
    steps: Arc<[Step]>,
    draw: Paint,
    gain: Paint,
    depth: Depth,
}

impl BlurOp {
    /// An op of `draw` alone, under `matrix` and `clip`.
    pub fn start(draw: BlurDraw, matrix: Matrix, clip: Option<&Path>) -> Self {
        BlurOp {
            geometry: Geometries::One(draw.geometry),
            matrix,
            clip: clip.map(Path::serialize),
            paint: draw.paint,
            made: OnceLock::new(),
        }
    }

    /// Adds `draw` to this run where it is a rectangle painted as the run's
    /// rectangles are; hands it back otherwise. The frame is the caller's to
    /// have checked. A path joins no run: whether its blur is made on its
    /// coverage depends on its shape, which a run shares one choice of.
    pub fn extend(&mut self, draw: BlurDraw) -> Result<(), BlurDraw> {
        let joins = matches!(draw.geometry, Geometry::Rect(..))
            && matches!(self.geometry.first(), Geometry::Rect(..))
            && draw.paint == self.paint;
        match joins {
            true => {
                self.geometry.push(draw.geometry);
                Ok(())
            }
            false => Err(draw),
        }
    }

    /// Draws the blurred geometry onto `canvas` for `target`. A run of filled
    /// rectangles that stay rectangles on the device is drawn exactly, on
    /// any surface with pixels -- see [`blur_mask`]. Anything else takes the
    /// coverage blur the recording chose wherever it can draw the sigma this
    /// comes to in `canvas`'s device pixels, and the exact blur of the
    /// result where it cannot. On pixels, a draw that carries an exact colour
    /// paints in it -- see [`ExactColour`].
    pub fn playback(&self, canvas: &SkCanvas, target: Target) {
        let Some(path) = self.geometry.first().path() else {
            return;
        };
        canvas.save();
        if let Some(clip) = self.clip.as_ref().and_then(Path::deserialize) {
            canvas.clip_path(&clip, ClipOp::Intersect, true);
        }
        let replay = replay_scale(canvas);
        if self.draws_exactly(canvas, replay, target) {
            canvas.restore();
            return;
        }
        let paint = self.paint_for(target);
        match (self.made_at(&path, replay), target) {
            (Made::Result(_), Target::Pixels) => {
                let chain = self.result_chain(canvas, paint);
                // On an eight-bit page on the CPU a placed colour's blur is
                // made from the draw's coverage alone -- see
                // `chain::coverage` -- and the chain's layer is not drawn.
                if let Some(rgb) = self.coverage_colour(canvas) {
                    let gain = chain.gain.alpha_f();
                    let info = canvas.image_info();
                    // The plane holds coverage alone, so a fill draws it
                    // opaque without the exact colour's shader: the shader
                    // returns an alpha of 1 -- see `ExactColour::shader` --
                    // and `result_chain` sets the paint's to 1, so either
                    // way a pixel holds its coverage, and Skia builds no
                    // shader stages to find it. A stroke keeps the shader:
                    // Skia draws one thinner than a pixel as a hairline with
                    // its paint's alpha cut to a byte by the width
                    // (`modifyPaintForHairlines`, `src/core/SkDraw.cpp`),
                    // which a shaded paint and a plain one round apart --
                    // a 0.1-wide stroke differed on 178 of 2,304 pages --
                    // while it never touches a fill (`DrawTreatAsHairline`,
                    // `src/core/SkDrawProcs.h`).
                    let mut plane_paint = chain.draw.clone();
                    if self.paint.style == PaintStyle::Fill {
                        plane_paint.set_shader(None).set_alpha_f(1.0);
                    }
                    #[cfg(test)]
                    if SHADED_PLANE.get() {
                        plane_paint = chain.draw.clone();
                    }
                    for path in self.paths() {
                        let sharp = chain::coverage::sharp(
                            canvas,
                            self.paint.sigma,
                            Some(self.content(&path)),
                            chain::coverage::Plane::Drawn,
                            |plane| self.draw_into(plane, &path, &plane_paint),
                        );
                        let Some(sharp) = sharp else {
                            continue;
                        };
                        #[cfg(test)]
                        COVERAGE_RUNS.with(|runs| runs.set(runs.get() + 1));
                        // A coverage blurred and coloured before is not
                        // again, once it is that one to the byte -- see
                        // `kept`.
                        let input = CoverageInput::of(&sharp, rgb, gain, &info);
                        let outcome = match input
                            .as_ref()
                            .and_then(CoverageInput::kept)
                        {
                            Some(kept) => Some(kept),
                            None => {
                                let made = chain::coverage::coloured(
                                    canvas,
                                    &sharp.blurred(),
                                    rgb,
                                    gain,
                                );
                                if let (Some(input), Some(made)) =
                                    (input, made.as_ref())
                                {
                                    input.keep(made.clone());
                                }
                                made
                            }
                        };
                        if let Some(outcome) = outcome {
                            outcome.composite(canvas, &chain.gain);
                        }
                    }
                    canvas.restore();
                    return;
                }
                for path in self.paths() {
                    chain::apply(
                        canvas,
                        &chain.steps,
                        Some(self.content(&path)),
                        chain.depth,
                        &chain.gain,
                        |layer| self.draw_into(layer, &path, &chain.draw),
                    );
                }
            }
            (made, _) => {
                let mut paint = paint;
                canvas.concat(&self.matrix);
                match made {
                    Made::Coverage(filter) => paint.set_mask_filter(filter),
                    Made::Result(filter) => paint.set_image_filter(filter),
                };
                let paint = self.paint.faint.drawable(&paint);
                // Each of a run drawn on its own as it was recorded.
                for path in self.paths() {
                    canvas.draw_path(&path, &paint);
                }
            }
        }
        canvas.restore();
    }

    /// The chain this op's blur runs on `canvas`'s pixels for each of its
    /// [`Self::paths`], and the paths, where it may share its layer with the
    /// chains beside it -- see [`chain::Plan::joins`], which decides whether
    /// it does. Only a blur of the result, on the GPU, under no clip path:
    /// one the op draws exactly, or on its coverage, runs no chain, and a
    /// clip bounds a layer in a way no test of sharing covers yet.
    fn shared_chain(
        &self,
        canvas: &SkCanvas,
        target: Target,
    ) -> Option<(BlurChain, Vec<Path>)> {
        let path = self.geometry.first().path()?;
        if target != Target::Pixels
            || !blur_mask::on_gpu(canvas)
            || self.clip.is_some()
        {
            return None;
        }
        let replay = replay_scale(canvas);
        if self.is_exact(canvas, replay) {
            return None;
        }
        match self.made_at(&path, replay) {
            Made::Result(_) => Some((
                self.result_chain(canvas, self.paint_for(target)),
                self.paths().collect(),
            )),
            Made::Coverage(_) => None,
        }
    }

    /// The placed colour a blurred draw on `canvas` takes the coverage route
    /// in -- see [`chain::coverage`] -- or `None` for the chain's layer: for
    /// a colour not placed on the bytes an eight-bit page stores, and
    /// wherever [`coverage_plays`] says no.
    fn coverage_colour(&self, canvas: &SkCanvas) -> Option<[f32; 3]> {
        let exact = self.paint.exact.as_ref()?;
        let placed = matches!(self.paint.faint, Faint::Dropped) && exact.bytes;
        (placed && coverage_plays(canvas, exact, self.paint.dither))
            .then(|| exact.at(self.paint.color.a).0.rgb)
    }

    /// The paths the op draws: its one, or each of a run's.
    fn paths(&self) -> impl Iterator<Item = Path> + '_ {
        self.geometry.iter().filter_map(Geometry::path)
    }

    /// What drawing `path` paints, under the op's transform: what bounds a
    /// chain drawing it -- see [`Self::painted`].
    fn content(&self, path: &Path) -> Rect {
        self.matrix.map_rect(self.painted(path)).0
    }

    /// Draws `path` through `paint` onto `layer`, a chain's layer, which
    /// carries the transform from canvas pixels to its own.
    fn draw_into(&self, layer: &SkCanvas, path: &Path, paint: &Paint) {
        layer.concat(&self.matrix);
        layer.draw_path(path, paint);
    }

    /// The paint the op draws through for `target`, before any blur: on
    /// pixels, a draw that carries an exact colour paints in it -- see
    /// [`ExactColour`].
    fn paint_for(&self, target: Target) -> Paint {
        let mut paint = Paint::new(self.paint.color, None);
        if target == Target::Pixels
            && let Some((exact, alpha)) =
                self.paint.exact.as_ref().map(|e| e.at(self.paint.color.a))
            && let Some(shader) = exact.shader()
        {
            paint.set_shader(shader).set_alpha_f(alpha);
        }
        paint
            .set_anti_alias(self.paint.anti_alias)
            .set_dither(self.paint.dither);
        paint.set_style(self.paint.style);
        if let Some(stroke) = self.paint.stroke {
            paint
                .set_stroke_width(stroke.width)
                .set_stroke_miter(stroke.miter)
                .set_stroke_cap(stroke.cap)
                .set_stroke_join(stroke.join);
        }
        paint
    }

    /// The blur this op makes at `replay`, `path` its first: on the coverage
    /// where the recording chose it, as long as the sigma it comes to here is
    /// one it can draw. Past `MASK_BLUR_MAX_SIGMA` device pixels it draws the
    /// cap instead, so `blur(40px)` exported at density 4 came out a blur of
    /// 128 pixels where 160 were asked for, and the result is blurred
    /// instead. One choice for the run: its rectangles share a coverage
    /// class.
    fn made_at(&self, path: &Path, replay: f32) -> Made {
        let device_sigma = self.paint.sigma * replay;
        let key = device_sigma.to_bits();
        match self.made.get() {
            Some((at, blur)) if *at == key => blur.clone(),
            _ => {
                let coverage = coverage_of(path, &self.matrix)
                    .blurs_alike(self.paint.sigma)
                    && device_sigma <= MASK_BLUR_MAX_SIGMA;
                let blur = self.make(coverage);
                // Kept for the next playback only if none was kept yet.
                let _ = self.made.set((key, blur.clone()));
                blur
            }
        }
    }

    /// The chain a blur of the result runs on `canvas`'s pixels, from
    /// `paint`, the op's paint -- see [`Self::paint_for`].
    ///
    /// On pixels the result is blurred by `chain`, exactly, each geometry of a
    /// run as a chain of its own, as separate draws composite. The path is
    /// drawn opaque in its colour and the colour's alpha, the global alpha in
    /// it, is the chain's gain, so that the blur is rounded once and not once
    /// more where the draw is.
    ///
    /// On an eight-bit page the gain is the alpha on its byte, rounded half up
    /// as a fill's is, so that the blur scales the colour the page stores: as
    /// given, an alpha of 0.3 or 0.7 lands on a tie the store rounds to even,
    /// and the interior stored a level under a fill. On its byte the gain
    /// multiplies a colour placed on its bytes -- see `ExactColour::at` -- back
    /// onto them from an eight-bit layer on the CPU: the layer is off by at
    /// most half a level, and the alpha, a byte over 255, scales that to under
    /// half. The GPU blurs and scales at lower precision, which took 68 of the
    /// 32,640 placed colours over the line at alphas from 235, so there a
    /// translucent placed colour is drawn into a layer of half floats, which
    /// holds every one exactly. Its edges are rounded twice, to a half float
    /// and then to a byte, and land up to a level off the exact Gaussian where
    /// a byte layer rounds them once. An opaque one takes a byte layer: its
    /// gain is 1, the layer holds the colour's bytes as they are, and interior
    /// and edges are both exact -- through half floats, 91 pixels of a black
    /// rounded rectangle under a blur of 2 left their exact Gaussian.
    fn result_chain(&self, canvas: &SkCanvas, mut paint: Paint) -> BlurChain {
        paint.set_alpha_f(1.0);
        let bytes = matches!(self.paint.faint, Faint::Dropped);
        let placed =
            bytes && self.paint.exact.as_ref().is_some_and(|e| e.bytes);
        let mut gain = Paint::default();
        gain.set_alpha_f(if bytes {
            byte_alpha(self.paint.color.a)
        } else {
            self.paint.color.a
        });
        let depth = match (self.paint.faint, placed) {
            (Faint::Kept, _) => Depth::Floats,
            (Faint::Dropped, true)
                if blur_mask::on_gpu(canvas)
                    && byte_alpha(self.paint.color.a) < 1.0 =>
            {
                Depth::Halves
            }
            (Faint::Dropped, _) => Depth::Bytes,
        };
        BlurChain {
            steps: Arc::from([Step::Blur(self.paint.sigma)]),
            draw: paint,
            gain,
            depth,
        }
    }

    /// What drawing `path` paints, in its own units, before any blur: its
    /// bounds, widened for a stroke by as far as its outline can reach -- a
    /// miter join `miter` half-widths out, a square cap's corner `sqrt(2)`.
    fn painted(&self, path: &Path) -> Rect {
        let bounds = *path.bounds();
        match self.paint.stroke {
            Some(stroke) => {
                let reach = stroke.width / 2.0 * stroke.miter.max(SQRT_2);
                bounds.with_outset((reach, reach))
            }
            None => bounds,
        }
    }

    /// Whether the run is one [`Self::draws_exactly`] draws on `canvas` at
    /// `replay`.
    fn is_exact(&self, canvas: &SkCanvas, replay: f32) -> bool {
        let sigma = self.paint.sigma * replay;
        // A sigma that is not a positive length has no Gaussian to be exact
        // to, and keeps the blur the recording chose.
        let plain = self.paint.style == PaintStyle::Fill
            && self.paint.anti_alias
            && !self.paint.dither
            && sigma.is_finite()
            && sigma > 0.0
            && matches!(self.geometry.first(), Geometry::Rect(..));
        let device =
            Matrix::concat(&canvas.local_to_device_as_3x3(), &self.matrix);
        plain
            && device.rect_stays_rect()
            && !device.has_perspective()
            && blur_mask::draws_on(canvas)
    }

    /// Draws the run exactly where it is one [`blur_mask::draw_rect`] takes:
    /// anti-aliased filled rectangles, undithered, that `canvas`'s transform
    /// keeps rectangles, on a canvas with pixels. Returns whether it drew;
    /// `false` leaves `canvas` as it found it. On an eight-bit page an exact
    /// colour is blurred on the bytes a fill of it stores, as the chain's
    /// gain blurs a path's in [`Self::playback`].
    ///
    /// Dithering is left to the blur that honours it: the exact blur is a
    /// gradient rounded once, and a dither would be noise added on top that
    /// nothing here draws.
    fn draws_exactly(
        &self,
        canvas: &SkCanvas,
        replay: f32,
        target: Target,
    ) -> bool {
        if !self.is_exact(canvas, replay) {
            return false;
        }
        let sigma = self.paint.sigma * replay;
        let device =
            Matrix::concat(&canvas.local_to_device_as_3x3(), &self.matrix);
        // The exact colour only where this plays as pixels, as `playback`
        // paints with it.
        let exact = self
            .paint
            .exact
            .as_ref()
            .filter(|_| target == Target::Pixels);
        canvas.save();
        canvas.reset_matrix();
        self.geometry.iter().for_each(|geometry| {
            if let Geometry::Rect(rect, _) = geometry {
                blur_mask::draw_rect(
                    canvas,
                    device.map_rect(rect).0,
                    sigma,
                    self.paint.color,
                    exact,
                );
            }
        });
        canvas.restore();
        true
    }

    /// The blur itself: on the coverage where `coverage` says so and the
    /// transform maps a length to a positive one, on the result otherwise.
    /// A result blur played back onto pixels is the exact one [`chain`]
    /// makes; the Skia filter made here is what a document draws.
    fn make(&self, coverage: bool) -> Made {
        // The sigma in the path's own units, which Skia maps back through
        // the canvas: `respect_ctm` for the mask filter, and an image
        // filter's lengths are local whatever it is given.
        let local = self
            .matrix
            .map_radius(1.0)
            .filter(|s| s.is_normal() && *s > 0.0);
        match (coverage, local) {
            (true, Some(scale)) => Made::Coverage(MaskFilter::blur(
                BlurStyle::Normal,
                self.paint.sigma / scale,
                true,
            )),
            // Divided as `Filter::filters_for` divides a rendered draw's.
            _ => Made::Result(image_filters::blur(
                (
                    self.paint.sigma / self.matrix.scale_x(),
                    self.paint.sigma / self.matrix.scale_y(),
                ),
                TileMode::Decal,
                None,
                None,
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::luminance_colour;
    use crate::started;
    use skia_safe::{
        AlphaType, BlendMode, Color, Color4f, ColorType, ImageInfo, Matrix,
        Paint, Path, Rect, color_filters,
    };
    use std::iter::zip;

    /// The oval [`skia_covers_a_curve_by_where_it_lies`] fills: its corners
    /// on the 1/64 grid, so that a whole-pixel shift moves every point
    /// exactly.
    const OVAL: Rect = Rect {
        left: 40.25,
        top: 20.5,
        right: 470.75,
        bottom: 300.125,
    };

    /// The raster the oval is filled on, tall enough for it shifted as far
    /// as [`skia_covers_a_curve_by_where_it_lies`] shifts it.
    const RASTER: (i32, i32) = (520, 380);

    #[test]
    fn a_draw_rasterized_onto_a_canvas_takes_its_text_contrast_and_gamma() {
        // `rasterized_onto` draws on a raster of its own and composites it
        // onto the canvas; text the draw holds is rasterized there under the
        // canvas's contrast and gamma, so that it lands the bytes it lands
        // drawn on the canvas itself. Neither value here is a default on any
        // platform, and the page starts transparent, where compositing the
        // raster source-over copies it.
        use skia_safe::{
            Data, Font, FontMgr, PictureRecorder, PixelGeometry, SurfaceProps,
            SurfacePropsFlags,
        };
        let bytes = std::fs::read(
            "tests/assets/fonts/Oswald/Oswald-VariableFont_wght.ttf",
        )
        .expect("the bundled face");
        let face = FontMgr::new()
            .new_from_data(Data::new_copy(&bytes), None)
            .expect("a face");
        let font = Font::from_typeface(face, 18.0);
        let mut recorder = PictureRecorder::new();
        let recording =
            recorder.begin_recording(Rect::from_wh(120.0, 40.0), false);
        let mut ink = Paint::default();
        ink.set_anti_alias(true)
            .set_color(Color::from_rgb(40, 90, 160));
        recording.draw_str("Wgaxe", (6.0, 26.0), &font, &ink);
        let picture = recorder
            .finish_recording_as_picture(None)
            .expect("a picture");
        let props = SurfaceProps::new_with_text_properties(
            SurfacePropsFlags::default(),
            PixelGeometry::Unknown,
            0.7,
            2.0,
        );
        let info = ImageInfo::new(
            (120, 40),
            ColorType::RGBA8888,
            AlphaType::Premul,
            None,
        );
        let read = |surface: &mut skia_safe::Surface| {
            let mut bytes = vec![0u8; 120 * 40 * 4];
            assert!(surface.read_pixels(&info, &mut bytes, 120 * 4, (0, 0)));
            bytes
        };
        let mut direct =
            started::raster(&info, None, Some(&props)).expect("a page");
        picture.playback(direct.canvas());
        let mut routed =
            started::raster(&info, None, Some(&props)).expect("a page");
        let format =
            crate::font::colr::Format::of(routed.canvas()).expect("pixels");
        super::rasterized_onto(
            routed.canvas(),
            &picture,
            Some(picture.cull_rect()),
            &format,
            &Paint::default(),
            super::Pipeline::Bytes,
        );
        let (direct, routed) = (read(&mut direct), read(&mut routed));
        assert!(direct.iter().any(|&b| b != 0), "the run inks the page");
        let most = zip(&direct, &routed)
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap_or(0);
        assert_eq!(most, 0, "levels from the run drawn on the page");
    }

    /// The alpha of [`OVAL`] moved `by` device pixels down, filled
    /// anti-aliased on a CPU raster.
    fn oval_at(by: i32) -> Vec<u8> {
        let mut surface =
            started::raster_n32_premul(RASTER).expect("a surface");
        let mut paint = Paint::default();
        paint.set_anti_alias(true);
        let oval = Path::oval(OVAL.with_offset((0.0, by as f32)), None);
        surface.canvas().draw_path(&oval, &paint);
        let info =
            ImageInfo::new(RASTER, ColorType::Alpha8, AlphaType::Premul, None);
        let mut alpha = vec![0u8; (RASTER.0 * RASTER.1) as usize];
        assert!(surface.read_pixels(
            &info,
            &mut alpha,
            RASTER.0 as usize,
            (0, 0)
        ));
        alpha
    }

    /// Skia's anti-aliasing covers a curved edge by where on the device it
    /// lies, not only by its shape: the same oval, moved down by a whole
    /// number of pixels -- every point exactly, on the 1/64 grid -- is
    /// covered differently along its edge, by up to 13 levels at a shift of
    /// 2. Moved across it was covered alike at every shift tried, while a
    /// larger oval further out flipped across and not down. If this fails,
    /// a curve is covered alike wherever it lies, and the note on a chain's
    /// layer in `Group::chain_content`, the bound `text_reach_tests` hold
    /// two placements to, and the entry in `docs/browser-differences.md`
    /// can go -- and the surface of its own `Group::played_at_source` plays
    /// a canvas drawn in on, unless a cut still moves a path's coverage.
    #[test]
    fn skia_covers_a_curve_by_where_it_lies() {
        let here = oval_at(0);
        let width = RASTER.0 as usize;
        let moved = (1..=70).find(|&by| {
            let there = oval_at(by);
            let by = by as usize;
            (0..RASTER.1 as usize - by)
                .flat_map(|y| (0..width).map(move |x| (x, y)))
                .any(|(x, y)| {
                    here[y * width + x].abs_diff(there[(y + by) * width + x])
                        > 1
                })
        });
        assert!(moved.is_some(), "the oval covered alike at every shift");
    }

    /// Every pixel a kept run inks lies within
    /// [`ReplayedText::ink_under`] for the transform it is played under:
    /// its glyphs into one another, stroked thin and wide, casting a shadow
    /// blurred and sharp, turned, slanted by a synthesized oblique, large
    /// enough to be drawn from its outlines, and played at a density of 2.
    /// A shadow is cast through a chain of its own, an entry apart from the
    /// run, which is then painted as one image through a layer of its own --
    /// see [`KeptRun::Image`] -- except under a CanvasKit filter, where the
    /// run holds it; the last scene is that one. What it inks is read from
    /// the run played alone on a transparent raster, where under `multiply`
    /// it is its own colour. And no run reports the whole page, which every
    /// run's recording reaches and which would hold every scene.
    #[test]
    fn a_kept_run_inks_nothing_past_what_it_reports() {
        use super::{BackdropReach, Composite, KeptRun, Layer, ReplayedText};
        use crate::{canvas::Canvas, font::FontLibrary, paint::BlendMode};
        use skia_safe::{Contains, IRect};
        FontLibrary::new()
            .register_font_from_path(
                "InkOswald",
                "tests/assets/fonts/Oswald/Oswald-VariableFont_wght.ttf",
            )
            .expect("a bundled face registers");
        /// A scene: its name, the font, the spacing, the stroke width (0
        /// for a fill), the shadow's blur (`None` for none), the turn in
        /// radians, and the density it is played at.
        type Scene =
            (&'static str, &'static str, f32, f32, Option<f32>, f32, f32);
        /// The scene whose run holds its shadow, under a CanvasKit filter.
        const HELD: &str = "shadow held by the run";
        const SCENES: [Scene; 11] = [
            ("plain", "32px InkOswald", 0.0, 0.0, None, 0.0, 1.0),
            (
                "into one another",
                "32px InkOswald",
                -12.0,
                0.0,
                None,
                0.0,
                1.0,
            ),
            ("stroked", "32px InkOswald", 0.0, 3.0, None, 0.0, 1.0),
            ("stroked wide", "32px InkOswald", 0.0, 20.0, None, 0.0, 1.0),
            (
                "blurred shadow",
                "32px InkOswald",
                0.0,
                0.0,
                Some(6.0),
                0.0,
                1.0,
            ),
            (
                "sharp shadow",
                "32px InkOswald",
                0.0,
                0.0,
                Some(0.0),
                0.0,
                1.0,
            ),
            ("turned", "32px InkOswald", 0.0, 0.0, None, 0.3, 1.0),
            ("oblique", "italic 32px InkOswald", 0.0, 0.0, None, 0.0, 1.0),
            ("outlines", "300px InkOswald", 0.0, 0.0, None, 0.0, 1.0),
            ("density 2", "32px InkOswald", 0.0, 0.0, None, 0.0, 2.0),
            (HELD, "32px InkOswald", 0.0, 0.0, Some(6.0), 0.0, 1.0),
        ];
        const PAGE: (f32, f32) = (900.0, 420.0);
        let mut wrong = vec![];
        for (name, font, spacing, stroke, shadow, turn, density) in SCENES {
            let mut canvas = Canvas::new(PAGE.0, PAGE.1);
            let ctx = canvas.context();
            ctx.set_global_composite_operation(BlendMode::Multiply);
            assert!(ctx.set_font_css(font), "{font} parses");
            ctx.set_letter_spacing(spacing);
            ctx.set_fill_style_css("rgb(40,90,160)").expect("a colour");
            ctx.set_stroke_style_css("rgb(40,90,160)")
                .expect("a colour");
            ctx.set_line_width(stroke.max(1.0));
            if name == HELD {
                let still =
                    crate::filter::ImageFilter::offset(0.0, 0.0, None, None)
                        .expect("an offset filter");
                ctx.set_image_filter(Some(&still));
            }
            if let Some(blur) = shadow {
                ctx.set_shadow_color_css("rgba(0,0,0,0.8)")
                    .expect("a colour");
                ctx.set_shadow_offset(14.0, 9.0);
                ctx.set_shadow_blur(blur);
            }
            ctx.translate(40.0, 300.0);
            ctx.rotate(turn);
            match stroke > 0.0 {
                true => ctx.stroke_text("Wq jumps", 0.0, 0.0, None),
                false => ctx.fill_text("Wq jumps", 0.0, 0.0, None),
            }
            let entries = ctx.inner.entries();
            let texts: Vec<&ReplayedText> = entries
                .layers
                .iter()
                .filter_map(|entry| match entry {
                    Layer::ReplayedText(text) => Some(&**text),
                    _ => None,
                })
                .collect();
            let size = ((PAGE.0 * density) as i32, (PAGE.1 * density) as i32);
            let device = Matrix::scale((density, density));
            let clip = IRect::from_wh(size.0, size.1);
            // Each kept run and what it reports it inks.
            let kept: Vec<(&Layer, Option<IRect>)> = entries
                .layers
                .iter()
                .filter_map(|entry| match entry {
                    Layer::ReplayedText(text) => {
                        Some((entry, text.ink_under(&device, clip)))
                    }
                    Layer::Group(group)
                        if matches!(&group.composite,
                            Composite::Layer(rec) if rec.one_image) =>
                    {
                        let ink =
                            match KeptRun::Image(group).reach(&device, clip) {
                                BackdropReach::Device(ink) => Some(ink),
                                _ => None,
                            };
                        Some((entry, ink))
                    }
                    _ => None,
                })
                .collect();
            if kept.is_empty() {
                wrong.push(format!("{name}: no run kept"));
                continue;
            }
            if name == HELD && texts.iter().all(|text| text.shadow.is_none()) {
                wrong.push(format!("{name}: no run holds its shadow"));
            }
            for (entry, ink) in kept {
                let Some(ink) = ink else {
                    wrong.push(format!("{name}: no ink reported"));
                    continue;
                };
                let mut surface =
                    started::raster_n32_premul(size).expect("a surface");
                surface.canvas().scale((density, density));
                entry.playback(surface.canvas(), super::Target::Pixels);
                let info = ImageInfo::new(
                    size,
                    ColorType::Alpha8,
                    AlphaType::Premul,
                    None,
                );
                let mut alpha = vec![0u8; (size.0 * size.1) as usize];
                assert!(surface.read_pixels(
                    &info,
                    &mut alpha,
                    size.0 as usize,
                    (0, 0)
                ));
                let width = size.0 as usize;
                let inked = alpha
                    .iter()
                    .enumerate()
                    .filter(|(_, a)| **a > 0)
                    .map(|(at, _)| ((at % width) as i32, (at / width) as i32))
                    .fold(None, |bounds: Option<IRect>, (x, y)| {
                        let pixel = IRect::new(x, y, x + 1, y + 1);
                        Some(match bounds {
                            Some(bounds) => IRect::new(
                                bounds.left.min(pixel.left),
                                bounds.top.min(pixel.top),
                                bounds.right.max(pixel.right),
                                bounds.bottom.max(pixel.bottom),
                            ),
                            None => pixel,
                        })
                    });
                match inked {
                    None => wrong.push(format!("{name}: inked nothing")),
                    Some(inked) if !ink.contains(inked) => wrong
                        .push(format!("{name}: inks {inked:?} past {ink:?}")),
                    // Every scene inks a corner of the page: a run reporting
                    // the whole of it bounds nothing. A layer painting a run
                    // as one image reaches the clip, as its picture does.
                    Some(_)
                        if ink == clip
                            && matches!(entry, Layer::ReplayedText(_)) =>
                    {
                        wrong.push(format!("{name}: reports the whole page"))
                    }
                    Some(_) => {}
                }
            }
        }
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    /// A draw between two kept runs is filed as an entry whose reach, as a
    /// batch asks it -- see [`Layer::found_reach`] -- is what it draws, not
    /// the page, in a blend by coefficients or one read in the shader, under
    /// a clip or not: the page's recording keeps a bounding hierarchy, an
    /// isolated layer -- see `page::PageRecorder::flush` -- does not reach
    /// it as one operation bounded by the page, and a clip's run -- see
    /// `page::Run::cull` -- is found within its clip. The read back that draws
    /// kept runs on the CPU -- see [`super::replayed_on_their_backdrop`] --
    /// can then tell a draw beside the runs from one over them.
    #[test]
    fn a_draw_between_kept_runs_reaches_only_what_it_draws() {
        use super::Layer;
        use crate::{
            canvas::Canvas, font::FontLibrary, paint::BlendMode, path::FillRule,
        };
        use skia_safe::Contains;
        FontLibrary::new()
            .register_font_from_path(
                "ReachOswald",
                "tests/assets/fonts/Oswald/Oswald-VariableFont_wght.ttf",
            )
            .expect("a bundled face registers");
        /// Fills of the rectangle between the runs: more ops than a
        /// recording is filed bare with.
        const FILLS: usize = 24;
        let drawn = Rect::from_xywh(300.0, 10.0, 40.0, 8.0);
        // Anti-aliasing may widen the bounds Skia keeps by a pixel.
        let outset = drawn.with_outset((1.0, 1.0));
        let mut wrong = vec![];
        for blend in [BlendMode::SourceOver, BlendMode::Multiply] {
            for clipped in [false, true] {
                let mut canvas = Canvas::new(400.0, 200.0);
                let ctx = canvas.context();
                // Only a page played on the GPU bounds a clip's run, and
                // only the reach is under test here, not the engine the
                // machine running it has.
                ctx.inner.set_plays_on_gpu(true);
                assert!(ctx.set_font_css("16px ReachOswald"));
                ctx.set_global_composite_operation(BlendMode::Multiply);
                ctx.fill_text("Sphinx of black quartz", 10.0, 20.0, None);
                ctx.set_global_composite_operation(blend);
                ctx.save();
                if clipped {
                    ctx.begin_path();
                    ctx.rect(250.0, 0.0, 120.0, 100.0);
                    ctx.clip(FillRule::NonZero);
                }
                // More fills than a recording is filed bare with, so that
                // the wrapped one is held too -- see `page::UNWRAPPED_OPS`.
                for _ in 0..FILLS {
                    ctx.fill_rect(
                        drawn.left,
                        drawn.top,
                        drawn.width(),
                        drawn.height(),
                    );
                }
                ctx.restore();
                ctx.set_global_composite_operation(BlendMode::Multiply);
                ctx.fill_text("Sphinx of black quartz", 10.0, 40.0, None);
                let entries = ctx.inner.entries();
                let kept =
                    |entry: &&Layer| matches!(entry, Layer::ReplayedText(_));
                let reaches: Vec<_> = entries
                    .layers
                    .iter()
                    .skip_while(|entry| !kept(entry))
                    .skip(1)
                    .take_while(|entry| !kept(entry))
                    .map(Layer::found_reach)
                    .collect();
                // Every entry between the runs within the rectangle, and the
                // rectangle among them.
                let within = reaches.iter().all(|reach| {
                    reach.is_some_and(|reach| {
                        reach.is_empty() || outset.contains(reach)
                    })
                });
                let found = reaches.iter().any(|reach| {
                    reach.is_some_and(|reach| reach.contains(drawn))
                });
                if !(within && found) {
                    wrong.push(format!(
                        "{blend:?} clipped {clipped}: {reaches:?}"
                    ));
                }
            }
        }
        assert!(
            wrong.is_empty(),
            "the rectangle {drawn:?} reaches\n{}",
            wrong.join("\n")
        );
    }

    /// A clip's run between kept runs reaches every pixel its draws ink,
    /// past the geometry they are given, as a batch asks it -- see
    /// [`Layer::found_reach`]: a stroke's half width, a miter join's point
    /// and a square cap -- each recorded into the run's picture and bounded
    /// with its paint, as Skia bounds a draw -- see
    /// `page::PageRecorder::holds_reading_runs`. A shadow, sharp or
    /// blurred, and a blur filter are filed as a [`BlurOp`], whose reach is
    /// not known, which ends a batch of runs drawn on the CPU there instead
    /// -- see [`super::replayed_on_their_backdrop`]. A reach
    /// short of the ink would let the batch read back before the draw inks
    /// under its runs.
    ///
    /// The ink is what the same draw alone inks on a CPU canvas, and every
    /// draw is held to ink past its geometry, so that no case passes on the
    /// geometry alone; and each is held to the way it is bounded, so that
    /// none passes vacuously on an unknown reach.
    #[test]
    fn a_clips_run_reaches_what_its_draws_ink_past_their_geometry() {
        use super::Layer;
        use crate::{
            canvas::Canvas,
            context2d::Context2D,
            font::FontLibrary,
            paint::{BlendMode, StrokeCap, StrokeJoin},
            path::FillRule,
        };
        use skia_safe::Contains;
        FontLibrary::new()
            .register_font_from_path(
                "ReachOswald",
                "tests/assets/fonts/Oswald/Oswald-VariableFont_wght.ttf",
            )
            .expect("a bundled face registers");
        const SIZE: (f32, f32) = (400.0, 200.0);
        /// Each draw, and whether its reach is known: recorded into the
        /// clip's run, or filed as a blur, its shadow's or its own.
        const DRAWS: [(&str, bool); 7] = [
            ("stroke", true),
            ("miter join", true),
            ("square cap", true),
            ("sharp shadow", false),
            ("stroke's sharp shadow", false),
            ("blurred shadow", false),
            ("blur filter", false),
        ];
        // The geometry each draw is given, clear of the runs at its left.
        let geometry = Rect::from_xywh(260.0, 60.0, 40.0, 30.0);
        let draw = |ctx: &mut Context2D, kind: &str| {
            ctx.set_fill_style_css("rgb(40,90,160)").expect("a colour");
            ctx.set_stroke_style_css("rgb(160,40,90)")
                .expect("a colour");
            let (left, top) = (geometry.left, geometry.top);
            let (right, bottom) = (geometry.right, geometry.bottom);
            if kind.contains("shadow") {
                ctx.set_shadow_color_css("rgba(0,0,0,0.8)")
                    .expect("a colour");
                ctx.set_shadow_offset(18.0, 14.0);
                ctx.set_shadow_blur(if kind == "blurred shadow" {
                    6.0
                } else {
                    0.0
                });
            }
            match kind {
                "stroke" | "stroke's sharp shadow" => {
                    // Round joins, which Skia bounds by the half width
                    // alone: a miter join is bounded by the miter limit
                    // too, which would hold a reach short of the stroke's
                    // own width to the ink all the same.
                    ctx.set_line_width(14.0);
                    ctx.set_line_join(StrokeJoin::Round);
                    ctx.stroke_rect(
                        left,
                        top,
                        geometry.width(),
                        geometry.height(),
                    );
                }
                "miter join" => {
                    // A narrow angle, whose miter reaches far past the
                    // points it joins.
                    ctx.set_line_width(8.0);
                    ctx.set_line_join(StrokeJoin::Miter);
                    ctx.set_miter_limit(20.0);
                    ctx.begin_path();
                    ctx.move_to(left, top);
                    ctx.line_to(right, top + 6.0);
                    ctx.line_to(left, top + 12.0);
                    ctx.stroke();
                }
                "square cap" => {
                    ctx.set_line_width(16.0);
                    ctx.set_line_cap(StrokeCap::Square);
                    ctx.begin_path();
                    ctx.move_to(left, bottom);
                    ctx.line_to(right, bottom);
                    ctx.stroke();
                }
                "blur filter" => {
                    ctx.set_filter_css("blur(5px)").expect("a filter");
                    ctx.fill_rect(
                        left,
                        top,
                        geometry.width(),
                        geometry.height(),
                    );
                }
                _ => ctx.fill_rect(
                    left,
                    top,
                    geometry.width(),
                    geometry.height(),
                ),
            }
        };
        // What the draw inks alone, on the CPU: the box of every pixel it
        // leaves with any alpha.
        let inked = |kind: &str| {
            let mut canvas = Canvas::new(SIZE.0, SIZE.1);
            canvas.set_gpu(false);
            let ctx = canvas.context();
            draw(ctx, kind);
            let data = ctx
                .get_image_data(0.0, 0.0, SIZE.0, SIZE.1)
                .expect("the canvas reads back");
            let width = SIZE.0 as usize;
            let mut ink = Rect::new_empty();
            for (at, pixel) in
                data.pixels().as_chunks::<4>().0.iter().enumerate()
            {
                if pixel[3] > 0 {
                    let (x, y) = ((at % width) as f32, (at / width) as f32);
                    ink.join(Rect::new(x, y, x + 1.0, y + 1.0));
                }
            }
            ink
        };
        let mut wrong = vec![];
        for (kind, known) in DRAWS {
            let ink = inked(kind);
            if geometry.contains(ink) {
                wrong
                    .push(format!("{kind}: inks {ink:?}, within its geometry"));
                continue;
            }
            let mut canvas = Canvas::new(SIZE.0, SIZE.1);
            let ctx = canvas.context();
            // Only a page played on the GPU bounds a clip's run.
            ctx.inner.set_plays_on_gpu(true);
            assert!(ctx.set_font_css("16px ReachOswald"));
            ctx.set_global_composite_operation(BlendMode::Multiply);
            ctx.fill_text("Sphinx of black quartz", 10.0, 20.0, None);
            ctx.set_global_composite_operation(BlendMode::SourceOver);
            ctx.save();
            // Clear of the runs and wide of the draw's ink, so that only
            // what the run records bounds its reach.
            ctx.begin_path();
            ctx.rect(200.5, 0.0, 199.0, SIZE.1);
            ctx.clip(FillRule::NonZero);
            draw(ctx, kind);
            ctx.restore();
            ctx.set_global_composite_operation(BlendMode::Multiply);
            ctx.fill_text("Sphinx of black quartz", 10.0, 40.0, None);
            let entries = ctx.inner.entries();
            let kept = |entry: &&Layer| matches!(entry, Layer::ReplayedText(_));
            let reach = entries
                .layers
                .iter()
                .skip_while(|entry| !kept(entry))
                .skip(1)
                .take_while(|entry| !kept(entry))
                .try_fold(Rect::new_empty(), |mut all, entry| {
                    all.join(entry.found_reach()?);
                    Some(all)
                });
            match (reach, known) {
                (None, false) => {}
                (None, true) => wrong.push(format!("{kind}: no reach")),
                (Some(reach), false) => wrong.push(format!(
                    "{kind}: reaches {reach:?}, where a blur's is not known"
                )),
                (Some(reach), true) if !reach.contains(ink) => wrong
                    .push(format!("{kind}: reaches {reach:?}, inks {ink:?}")),
                // Bounded, not the page: a reach of the whole page holds
                // any ink and says nothing.
                (Some(reach), true)
                    if reach.contains(Rect::from_wh(SIZE.0, SIZE.1)) =>
                {
                    wrong.push(format!("{kind}: reaches the page"))
                }
                (Some(_), true) => {}
            }
        }
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    #[test]
    fn a_filters_luminance_colour_is_its_output() {
        // What a colour filter makes of a colour, read against a filter
        // whose output is known: one that puts a constant colour in place
        // of every other. skia-safe's `filter_color4f` would answer the same
        // and returns unrelated numbers instead -- see the note in
        // `luminance_colour`.
        let filter = color_filters::blend(
            Color::from_argb(255, 10, 200, 30),
            BlendMode::Src,
        )
        .expect("a filter");
        let got = luminance_colour(&filter, Color4f::new(0.5, 0.5, 0.5, 1.0))
            .expect("a reading");
        let want = [10.0f32, 200.0, 30.0].map(|c| c / 255.0);
        assert!(
            zip(got, want).all(|(got, want)| (got - want).abs() < 1e-3),
            "{got:?}, where the filter's colour is {want:?}"
        );
    }
}

#[cfg(all(test, any(feature = "metal", feature = "vulkan")))]
mod placed_colour_tests {
    use super::*;
    use crate::{
        context::page::ExportOptions,
        gpu::RenderingEngine,
        node::utils::{
            color_in_canvas_space, css_to_color4f_in_space, opt_color_space,
            skia_paint_color_onto,
        },
    };

    /// Colours named outside the page's gamut or far from its primaries,
    /// whose conversion runs through extended sRGB values.
    const SATURATED: [&str; 10] = [
        "color(a98-rgb 0 1 0)",
        "color(a98-rgb 1 0 0)",
        "color(a98-rgb 0 0 1)",
        "color(prophoto-rgb 0 1 0)",
        "color(prophoto-rgb -0.2 0.5 1.1)",
        "color(rec2020 0 1 0)",
        "color(rec2020 1 0 0)",
        "color(display-p3 0 1 0)",
        "color(display-p3 0.002 0.9 0.95)",
        "color(srgb-linear 0 0.001 1)",
    ];

    /// The bytes of every placed colour of the sweep -- each premultiplied
    /// byte under each alpha byte, as a grey, beside a full and a
    /// complementary channel, and skewed across the three -- and of
    /// [`SATURATED`] at three alphas, on a page in `space`.
    fn wanted(space: &ColorSpace) -> Vec<[u8; 4]> {
        let page = PixelColorSpace::matching(space).expect("a page space");
        let sweep = (1..=255u32).flat_map(|a| {
            (0..=a).flat_map(move |p| {
                [
                    [p, p, p, a],
                    [p, a, a - p, a],
                    [a - p, p, (p * 7) % (a + 1), a],
                ]
            })
        });
        let saturated = SATURATED.iter().flat_map(|css| {
            let (colour, named) =
                css_to_color4f_in_space(css).expect("a colour");
            let rgb =
                color_in_canvas_space(colour, &named, page).expect("converts");
            let exact = ExactColour::new(rgb, space.clone(), page, true);
            [1.0, 0.5, 0.2].map(|alpha| {
                let (placed, alpha) = exact.at(alpha);
                let stored = (alpha * 255.0).round() as u32;
                let [r, g, b] =
                    placed.rgb.map(|c| (c * stored as f32).round() as u32);
                [r, g, b, stored]
            })
        });
        sweep
            .chain(saturated)
            .filter(|want| want[3] > 0)
            .map(|want| want.map(|c| c as u8))
            .collect()
    }

    /// The plain paint colour [`landed`] first tries for each of
    /// [`wanted`].
    fn candidates(space: &ColorSpace) -> Vec<(Color4f, ColorSpace)> {
        wanted(space)
            .into_iter()
            .map(|want| {
                let a = f32::from(want[3]);
                let [r, g, b] = [0, 1, 2].map(|i| f32::from(want[i]) / a);
                skia_paint_color_onto(
                    Color4f::new(r, g, b, a / 255.0),
                    space,
                    space,
                )
            })
            .collect()
    }

    /// Each of `colours` drawn as a paint's colour, in the space it names,
    /// onto its own pixel of a surface in `space` of `ct` made by `make`,
    /// read back premultiplied as `ct`'s bytes.
    fn drawn_bytes(
        space: &ColorSpace,
        colours: &[(Color4f, ColorSpace)],
        ct: ColorType,
        make: impl Fn(&ImageInfo) -> Option<Surface>,
    ) -> Option<Vec<u8>> {
        let w = 256;
        let h = colours.len().div_ceil(w);
        let info = ImageInfo::new(
            (w as i32, h as i32),
            ct,
            AlphaType::Premul,
            Some(space.clone()),
        );
        let mut surface = make(&info)?;
        for (i, (colour, named)) in colours.iter().enumerate() {
            let mut paint = Paint::new(*colour, Some(named));
            paint.set_blend_mode(BlendMode::Src);
            let rect =
                Rect::from_xywh((i % w) as f32, (i / w) as f32, 1.0, 1.0);
            surface.canvas().draw_rect(rect, &paint);
        }
        let row = w * info.bytes_per_pixel();
        let mut bytes = vec![0u8; row * h];
        surface
            .read_pixels(&info, &mut bytes, row, (0, 0))
            .then_some(bytes)
    }

    /// [`drawn_bytes`] onto an F32 surface, as floats.
    fn drawn(
        space: &ColorSpace,
        colours: &[(Color4f, ColorSpace)],
        make: impl Fn(&ImageInfo) -> Option<Surface>,
    ) -> Option<Vec<f32>> {
        drawn_bytes(space, colours, ColorType::RGBAF32, make).map(|bytes| {
            bytes
                .as_chunks::<4>()
                .0
                .iter()
                .map(|b| f32::from_le_bytes(*b))
                .collect()
        })
    }

    /// Every paint colour [`landed`] gives, first try or nudged, stores the
    /// bytes it was asked for on an eight-bit page of its space on the
    /// raster backend and on the GPU, over every placed colour of
    /// [`wanted`]: so the float pixel alone decides a verdict. A Skia whose
    /// eight-bit routes part from its float conversion breaks this.
    #[test]
    fn a_colour_landing_within_the_margin_stores_its_bytes_on_either_engine() {
        let gpu = RenderingEngine::GPU.selectable();
        let mut wrong = Vec::new();
        let (mut verdicts, mut refused) = (0usize, 0usize);
        for space in ["display-p3", "rec2020", "srgb-linear"]
            .map(|name| opt_color_space(name).expect("a space"))
        {
            let page = PixelColorSpace::matching(&space).expect("a page");
            let (wants, colours): (Vec<_>, Vec<_>) = wanted(&space)
                .into_iter()
                .filter_map(|want| {
                    let verdict = landed(&space, page, want);
                    refused += usize::from(verdict.is_none());
                    verdict.map(|colour| (want, colour))
                })
                .unzip();
            verdicts += wants.len();
            let mut engines = vec![(
                "cpu",
                drawn_bytes(&space, &colours, ColorType::RGBA8888, |info| {
                    started::raster(info, None, None)
                })
                .expect("a raster read"),
            )];
            if gpu {
                engines.push((
                    "gpu",
                    drawn_bytes(
                        &space,
                        &colours,
                        ColorType::RGBA8888,
                        |info| {
                            RenderingEngine::GPU
                                .make_surface(info, &ExportOptions::default())
                                .ok()
                        },
                    )
                    .expect("a GPU read"),
                ));
            }
            for (engine, bytes) in &engines {
                wrong.extend(
                    bytes
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .zip(&wants)
                        .filter(|(got, want)| got != want)
                        .map(|(got, want)| {
                            format!("{page:?} {engine}: {got:?} for {want:?}")
                        }),
                );
            }
        }
        println!("{verdicts} verdicts, {refused} refused, GPU: {gpu}");
        assert!(verdicts > 0, "the sweep landed nothing");
        assert!(
            wrong.is_empty(),
            "{} parted: {:?}",
            wrong.len(),
            &wrong[..wrong.len().min(5)]
        );
    }

    /// Ganesh converts a paint's colour into the page's space as the raster
    /// pipeline does, to within [`GPU_HALF_ROUNDING_LEVELS`] -- the bound
    /// [`PLACED_MARGIN_LEVELS`] keeps for the GPU -- with its own rounding to
    /// half precision included, over every placed colour of the sweep and a
    /// saturated set: so the verifier's float pixel, drawn on the raster
    /// backend, stands for both engines.
    #[test]
    fn ganesh_converts_a_paint_colour_as_the_raster_pipeline_does() {
        if !RenderingEngine::GPU.selectable() {
            return;
        }
        let mut worst = Vec::new();
        for space in ["display-p3", "rec2020", "srgb-linear"]
            .map(|name| opt_color_space(name).expect("a space"))
        {
            let colours = candidates(&space);
            let raster = drawn(&space, &colours, |info| {
                started::raster(info, None, None)
            })
            .expect("a raster read");
            // Ganesh on Metal makes no RGBA F32 surface; an F16 one stores
            // each value to binary16, which is the one rounding the bound
            // already allows for.
            let gpu = drawn(&space, &colours, |info| {
                RenderingEngine::GPU
                    .make_surface(
                        &info.with_color_type(ColorType::RGBAF16),
                        &ExportOptions::default(),
                    )
                    .ok()
            })
            .expect("a GPU read");
            let (at, off) = raster
                .iter()
                .zip(&gpu)
                .map(|(r, g)| f64::from((r - g).abs()) * 255.0)
                .enumerate()
                .fold(
                    (0, 0.0f64),
                    |(i, m), (j, d)| if d > m { (j, d) } else { (i, m) },
                );
            worst.push(format!("{space:?}: {off:.5} levels at {}", at / 4));
            assert!(
                off <= GPU_HALF_ROUNDING_LEVELS,
                "{worst:?}: raster {:?} GPU {:?}",
                &raster[at / 4 * 4..at / 4 * 4 + 4],
                &gpu[at / 4 * 4..at / 4 * 4 + 4]
            );
        }
        println!("{worst:#?}");
    }
}

#[cfg(test)]
mod verdict_tests {
    use super::*;
    use crate::node::utils::{
        color_in_canvas_space, css_to_color4f_in_space, opt_color_space,
        skia_paint_color_onto,
    };
    use skia_safe::shaders;

    /// What a text paint in `plain` dyed by `dye` paints on one pixel of a
    /// float page in `onto`, as the text paint in `Context2D` holds them: a
    /// colour shader of `plain` named in sRGB, the dye its colour filter.
    fn dyed_pixel(
        dye: &ColorFilter,
        plain: Color4f,
        onto: &ColorSpace,
    ) -> [f32; 4] {
        let info = ImageInfo::new(
            (1, 1),
            ColorType::RGBAF32,
            AlphaType::Premul,
            Some(onto.clone()),
        );
        let mut surface =
            started::raster(&info, None, None).expect("a surface");
        let mut paint = Paint::default();
        paint
            .set_shader(shaders::color_in_space(plain, ColorSpace::new_srgb()));
        paint.set_color_filter(dye.clone());
        surface.canvas().draw_paint(&paint);
        let mut pixel = [0u8; 16];
        assert!(surface.read_pixels(&info, &mut pixel, 16, (0, 0)));
        let (channels, _) = pixel.as_chunks::<4>();
        [0, 1, 2, 3].map(|c| f32::from_le_bytes(channels[c]))
    }

    /// A dye is kept for the page and the plain colour it was read for: the
    /// same components on a Display P3 and a Rec. 2020 page, and one exact
    /// colour beside two plain colours in different contrast buckets, each
    /// find their own dye after the other was kept -- the one read afresh,
    /// by what it paints on a page where the two differ.
    #[test]
    fn a_dye_is_kept_for_its_page_and_its_plain_colour() {
        let p3 = opt_color_space("display-p3").expect("a space");
        let rec2020 = opt_color_space("rec2020").expect("a space");
        let srgb = ColorSpace::new_srgb();
        let rgb = [0.3, 0.6, 0.9];
        let on_p3 =
            ExactColour::new(rgb, p3, PixelColorSpace::DisplayP3, false);
        let on_rec2020 = ExactColour::new(
            rgb,
            rec2020.clone(),
            PixelColorSpace::Rec2020,
            false,
        );
        let plain = Color4f::new(0.1235, 0.5, 0.2, 1.0);
        let black = Color4f::new(0.0, 0.0, 0.0, 1.0);
        // Kept first, so the second of each pair is asked for after it.
        on_p3.dye(plain).expect("a dye");
        on_rec2020.dye(black).expect("a dye");
        let read = |exact: &ExactColour, plain: Color4f, onto: &ColorSpace| {
            let kept =
                dyed_pixel(&exact.dye(plain).expect("a dye"), plain, onto);
            let afresh =
                dyed_pixel(&exact.read_dye(plain).expect("a dye"), plain, onto);
            (kept, afresh)
        };
        // On a Rec. 2020 page each dye paints its own colour converted there.
        let (kept, afresh) = read(&on_rec2020, plain, &rec2020);
        assert_eq!(kept, afresh, "the Rec. 2020 page takes its own dye");
        let (p3_paints, _) = read(&on_p3, plain, &rec2020);
        assert_ne!(
            p3_paints, afresh,
            "the two pages' dyes differ there, or this asserts nothing"
        );
        // On an sRGB page a dye keeping the plain colour's contrast paints it.
        let (kept, afresh) = read(&on_p3, black, &srgb);
        assert_eq!(kept, afresh, "black takes its own dye");
        let (plain_paints, _) = read(&on_p3, plain, &srgb);
        assert_ne!(
            plain_paints, afresh,
            "the two plain colours' dyes differ there, or this asserts nothing"
        );
    }

    /// Two colours filed in one slot of the verdict table, drawn in turn,
    /// each take their own verdict: a colour finds the one filed for it,
    /// never the one filed over it.
    #[test]
    fn colours_sharing_a_slot_each_take_their_own_verdict() {
        let space = opt_color_space("rec2020").expect("a space");
        let pixels = PixelColorSpace::Rec2020;
        let opaque = |k: u16| {
            let [r, g] = k.to_le_bytes();
            [r, g, 0, u8::MAX]
        };
        let first = opaque(0);
        let other = (1..=u16::MAX)
            .map(opaque)
            .find(|&k| {
                verdict_slot((pixels, k)) == verdict_slot((pixels, first))
            })
            .expect("two colours in one slot");
        let paint = Paint::new(Color4f::new(0.0, 0.0, 0.0, 1.0), None);
        let verified = |bytes: [u8; 4]| {
            let rgb = [0, 1, 2].map(|i| f64::from(bytes[i]) / 255.0);
            ExactColour::new(rgb, space.clone(), pixels, true)
                .verified_paint(&paint)
                .map(|paint| paint.color4f())
        };
        let own = |bytes: [u8; 4]| {
            landed(&space, pixels, bytes).map(|(colour, tagged)| {
                let mut own = paint.clone();
                own.set_color4f(colour, &tagged);
                own.color4f()
            })
        };
        assert_ne!(own(first), own(other), "the two verdicts differ");
        for bytes in [first, other, first, other] {
            assert_eq!(verified(bytes), own(bytes), "{bytes:?}");
        }
    }

    /// An sRGB page takes the verdict [`landed`]'s draw would find, at every
    /// alpha byte, with each channel in turn at every byte it can store
    /// there.
    #[test]
    fn an_srgb_verdict_is_the_one_the_probe_finds() {
        let srgb = ColorSpace::new_srgb();
        let wants = (0..=u8::MAX).flat_map(|a| {
            (0..=a).flat_map(move |v| {
                let (w, m) = (a - v, v / 2 + (a - v) / 2);
                [[v, w, m, a], [m, v, w, a], [w, m, v, a]]
            })
        });
        let mut checked = 0;
        for want in wants {
            assert_eq!(
                unconverted(want),
                landed(&srgb, PixelColorSpace::Srgb, want),
                "{want:?}"
            );
            checked += 1;
        }
        assert_eq!(checked, 3 * 256 * 257 / 2);
    }

    /// `css` on an eight-bit page in `page`, as the exact colour and as the
    /// plain paint the document recording draws it with.
    fn on_page(css: &str, page: &str) -> (ExactColour, Paint) {
        let space = opt_color_space(page).expect("a space");
        let (colour, named) = css_to_color4f_in_space(css).expect("a colour");
        let canvas = PixelColorSpace::matching(&space).expect("a page space");
        let rgb =
            color_in_canvas_space(colour, &named, canvas).expect("converts");
        let (plain, tagged) = skia_paint_color_onto(colour, &named, &space);
        let mut paint = Paint::default();
        paint.set_color4f(plain, &tagged);
        (ExactColour::new(rgb, space, canvas, true), paint)
    }

    /// A colour Skia lands off its byte as a paint's colour, however it is
    /// moved within the byte, is left to the shader.
    #[test]
    fn a_colour_skia_lands_off_its_byte_is_refused() {
        let (exact, paint) =
            on_page("color(rec2020 1 1 0 / 0.34901962)", "rec2020");
        assert!(exact.verified_paint(&paint).is_none());
        let (exact, paint) = on_page("color(rec2020 0.5 0.5 0.5)", "rec2020");
        assert!(exact.verified_paint(&paint).is_some(), "a grey lands");
    }

    /// Text takes the placed colour only where it sets the glyph mask's
    /// contrast from the plain colour's bucket: `color(display-p3 0.05 0.56
    /// 0.5)` lands, but its sRGB blue moves from 127.34 to 127.87, across
    /// the edge of a bucket, so the dye draws it.
    #[test]
    fn text_keeps_the_dye_where_the_placed_colour_moves_its_contrast() {
        let (exact, paint) =
            on_page("color(display-p3 0.05 0.56 0.5)", "display-p3");
        assert!(exact.verified_paint(&paint).is_some());
        assert!(exact.verified_text_paint(&paint).is_none());
        let (exact, paint) =
            on_page("color(display-p3 0.5 0.5 0.5)", "display-p3");
        assert!(exact.verified_text_paint(&paint).is_some());
    }

    /// Only an sRGB page places text on the colour nearest its own: on any
    /// other the dye keeps the plain colour's contrast, so `color(display-p3
    /// 0.05 0.56 0.5)`, whose placed colour leaves its bucket, is left to
    /// the dye there. On an sRGB page grey 127.37 at an alpha of 151.61
    /// levels, whose product over the alpha's byte is 127.5 in the next
    /// bucket, is placed on 127, in its own.
    #[test]
    fn only_an_srgb_page_places_text_on_its_nearest_colour() {
        let (exact, paint) =
            on_page("color(display-p3 0.05 0.56 0.5)", "display-p3");
        assert!(exact.nearest_text_paint(&paint).is_none());
        let (exact, paint) =
            on_page("rgb(127.37 127.37 127.37 / 0.59454906)", "srgb");
        let placed = exact.nearest_text_paint(&paint).expect("placed");
        let bucket = |p: &Paint| {
            let c = p.color4f();
            contrast_bucket([c.r, c.g, c.b])
        };
        assert_eq!(bucket(&placed), bucket(&paint));
        let (at, _) = exact.at(paint.alpha_f());
        assert_ne!(
            contrast_bucket(at.rgb),
            bucket(&paint),
            "a pair that moves"
        );
    }
}

/// Shadows a page's runs make in passes they share -- see
/// [`chain::ShadowAtlas`] -- held to the bytes and the passes each makes
/// alone.
#[cfg(test)]
mod shared_shadow_tests {
    use super::{ALONE_CHAINS, ATLAS_SIDE};
    use crate::{
        context::chain::{
            OFFSCREEN, REFUSE_PLACED, SHARED_LAYERS, UNSHARED_LAYERS,
        },
        prelude::*,
    };

    /// The page every scene is drawn on.
    const PAGE: (f32, f32) = (720.0, 480.0);

    /// Whether chains share layers here: on Metal alone -- see
    /// `chain::shares_layers`.
    const SHARES: bool = cfg!(feature = "metal");

    /// A scene: `n` rounded rectangles in rows of six, `gap` apart, each
    /// casting a shadow -- a `shadowBlur` of `blur` and of `second` in turn,
    /// `period` rectangles at a time, or with `css`, the same
    /// shadow as a `drop-shadow()` filter, the draw over it -- in a fill at
    /// `alpha`, the page turned `turn` radians about its centre. With `text`,
    /// each is a line of text instead, the lines [`LINE`] apart, so that each
    /// line's shadow reaches the lines either side.
    #[derive(Clone, Copy)]
    struct Scene {
        n: usize,
        gap: f32,
        blur: f32,
        second: f32,
        alpha: f32,
        turn: f32,
        css: bool,
        text: bool,
        period: usize,
    }

    /// How far apart lines of text are drawn, in pixels: closer than a line's
    /// shadow reaches at a `shadowBlur` of 6, so that a line shares a layer
    /// only with lines further off.
    const LINE: f32 = 21.0;

    const PLAIN: Scene = Scene {
        n: 24,
        gap: 30.0,
        blur: 12.0,
        second: 12.0,
        alpha: 1.0,
        turn: 0.0,
        css: false,
        text: false,
        period: 1,
    };

    /// [`PLAIN`]'s shadows cast by lines of text.
    const LINES: Scene = Scene {
        n: 20,
        blur: 6.0,
        second: 6.0,
        text: true,
        ..PLAIN
    };

    /// What a page read back: its bytes, how many GPU targets its chains
    /// opened after their layers, and how many layers they were drawn in.
    struct Read {
        bytes: Vec<u8>,
        opened: usize,
        layers: usize,
    }

    /// `scene` read back on a GPU page, its chains sharing layers or
    /// `alone`, and with the shared layer refused where `refuse` -- see
    /// `chain::REFUSE_PLACED`. `None` without a GPU.
    fn page(scene: Scene, alone: bool, refuse: bool) -> Option<Read> {
        let mut canvas = Canvas::new(PAGE.0, PAGE.1);
        canvas.set_gpu(true);
        if canvas.engine_kind() != EngineKind::Gpu {
            return None;
        }
        let ctx = canvas.context();
        ctx.translate(PAGE.0 / 2.0, PAGE.1 / 2.0);
        ctx.rotate(scene.turn);
        ctx.translate(-PAGE.0 / 2.0, -PAGE.1 / 2.0);
        let (w, h) = (90.0, 60.0);
        for i in 0..scene.n {
            let blur = match (i / scene.period) % 2 {
                0 => scene.blur,
                _ => scene.second,
            };
            match scene.css {
                true => ctx
                    .set_filter_css(&format!(
                        "drop-shadow(3px 2px {}px rgb(0 0 0 / 0.5))",
                        blur / 2.0
                    ))
                    .expect("a filter"),
                false => {
                    ctx.set_shadow_color_css("rgb(0 0 0 / 0.5)")
                        .expect("a colour");
                    ctx.set_shadow_blur(blur);
                    ctx.set_shadow_offset(3.0, 2.0);
                }
            }
            ctx.set_fill_style_css(&format!(
                "hsl({} 60% 55% / {})",
                (i * 37) % 360,
                scene.alpha
            ))
            .expect("a colour");
            if scene.text {
                ctx.set_font_css("600 28px sans-serif");
                let y = 30.25 + i as f32 * LINE;
                ctx.fill_text(&format!("Throughput sample {i}"), 40.5, y, None);
                continue;
            }
            ctx.begin_path();
            let (x, y) = (
                12.5 + (i % 6) as f32 * (w + scene.gap),
                12.25 + (i / 6) as f32 * (h + scene.gap),
            );
            ctx.round_rect(x, y, w, h, [10.0; 4]).expect("a rectangle");
            ctx.fill(FillRule::NonZero);
        }
        ALONE_CHAINS.with(|flag| flag.set(alone));
        REFUSE_PLACED.with(|flag| flag.set(refuse));
        let count = || {
            (
                OFFSCREEN.with(std::cell::Cell::get),
                SHARED_LAYERS.with(std::cell::Cell::get),
            )
        };
        let before = count();
        let data = ctx.get_image_data(0.0, 0.0, PAGE.0, PAGE.1);
        let after = count();
        ALONE_CHAINS.with(|flag| flag.set(false));
        REFUSE_PLACED.with(|flag| flag.set(false));
        Some(Read {
            bytes: data.expect("pixels").pixels().to_vec(),
            opened: after.0 - before.0,
            layers: after.1 - before.1,
        })
    }

    /// Shadows sharing a layer share its passes: each pass is one target
    /// holding every chain's output, so the shadows drawn in a layer open no
    /// more targets between them than one shadow alone does -- the region
    /// copied out, padded, the passes of each axis, composed -- where alone
    /// each opens its own. Lines of text share too, each bounded by what it
    /// can ink.
    #[test]
    fn shadows_sharing_a_layer_share_their_passes() {
        for scene in [PLAIN, LINES] {
            let (Some(one), Some(shared), Some(alone)) = (
                page(Scene { n: 1, ..scene }, true, false),
                page(scene, false, false),
                page(scene, true, false),
            ) else {
                return;
            };
            let what = if scene.text { "lines" } else { "shapes" };
            assert_eq!(
                alone.opened,
                scene.n * one.opened,
                "{what} alone, a shadow's own"
            );
            // Where no layer is shared, each chain takes its own, and the
            // passes are still shared -- see the test after this one.
            let layers_shared = !SHARES || shared.layers < scene.n;
            assert!(
                layers_shared && shared.opened <= shared.layers * one.opened,
                "{} {what} in {} layers opened {} targets, one alone {}",
                scene.n,
                shared.layers,
                shared.opened,
                one.opened
            );
        }
    }

    /// Shadows that share passes land the bytes they do alone: each reads
    /// only its own region of the shared layer, though its blur reaches past
    /// it, onto a page turned or not, at any alpha, from a `shadowBlur` and
    /// from a `drop-shadow()`, and where the shadows of a layer are blurred
    /// by two sigmas, each with its own. The gaps run from within a region's
    /// reach -- six sigmas and the offset -- where neighbours never share a
    /// layer, to just past it, at 45 pixels, where they do and a chain
    /// whose passes read past its region takes in its neighbour's ink; at
    /// 75 the rows run past the page's edge.
    #[test]
    fn shadows_sharing_passes_land_the_bytes_they_do_alone() {
        let mut apart = vec![];
        let scenes = [1.0, 6.0, 12.0, 30.0, 45.0, 60.0, 75.0]
            .map(|gap| Scene { gap, ..PLAIN })
            .into_iter()
            .chain([
                Scene {
                    turn: 0.15,
                    ..PLAIN
                },
                Scene {
                    alpha: 0.55,
                    ..PLAIN
                },
                Scene {
                    second: 6.0,
                    ..PLAIN
                },
                Scene {
                    gap: 45.0,
                    second: 6.0,
                    ..PLAIN
                },
                // Every other rectangle shares a layer, and the sigma turns
                // every two, so each layer holds both and each sigma's
                // shadows are made from both layers.
                Scene {
                    gap: 6.0,
                    second: 6.0,
                    period: 2,
                    ..PLAIN
                },
                Scene { css: true, ..PLAIN },
                Scene {
                    css: true,
                    gap: 6.0,
                    turn: 0.15,
                    ..PLAIN
                },
                LINES,
                Scene { css: true, ..LINES },
                Scene {
                    alpha: 0.55,
                    turn: 0.15,
                    ..LINES
                },
            ]);
        for scene in scenes {
            let (Some(shared), Some(alone)) =
                (page(scene, false, false), page(scene, true, false))
            else {
                return;
            };
            let differ = shared
                .bytes
                .iter()
                .zip(&alone.bytes)
                .filter(|(a, b)| a != b)
                .count();
            if differ > 0 {
                apart.push(format!(
                    "gap {}, turn {}, alpha {}, css {}, text {}: {differ} bytes",
                    scene.gap, scene.turn, scene.alpha, scene.css, scene.text
                ));
            }
        }
        assert!(apart.is_empty(), "apart from alone: {apart:#?}");
    }

    /// Where chains share no layer -- as on Vulkan, where none do; see
    /// `chain::shares_layers` -- their shadows still share passes: each
    /// chain's region is copied out of its own layer into the run's atlas,
    /// so a page of them opens a few targets a run where alone each opens
    /// its own, and lands the same bytes.
    #[test]
    fn shadows_in_layers_of_their_own_share_their_passes() {
        for scene in [PLAIN, LINES, Scene { gap: 6.0, ..PLAIN }] {
            UNSHARED_LAYERS.with(|flag| flag.set(true));
            let unshared = page(scene, false, false);
            UNSHARED_LAYERS.with(|flag| flag.set(false));
            let (Some(unshared), Some(alone)) =
                (unshared, page(scene, true, false))
            else {
                return;
            };
            let differ = unshared
                .bytes
                .iter()
                .zip(&alone.bytes)
                .filter(|(a, b)| a != b)
                .count();
            let what = format!("text {}, gap {}", scene.text, scene.gap);
            assert_eq!(differ, 0, "{what}: unshared, apart from alone");
            assert_eq!(unshared.layers, scene.n, "{what}: a layer each");
            assert!(
                unshared.opened * 4 < alone.opened,
                "{what}: unshared opened {} targets, alone {}",
                unshared.opened,
                alone.opened
            );
        }
    }

    /// A run whose shadows one atlas cannot hold puts as many in it as fit,
    /// in order, and finishes the rest alone, and one whose atlas would hold
    /// fewer than two finishes them all alone: the bytes are the same either
    /// way, and the targets opened lie between the shared passes' and each
    /// shadow's own. Both where chains share layers and, through
    /// [`UNSHARED_LAYERS`], where each takes its own, as on Vulkan: there a
    /// run is [`MAX_RUN_LAYERS`](super::MAX_RUN_LAYERS) chains rather than
    /// all of [`PLAIN`]'s.
    ///
    /// [`PART_SIDE`] holds part of a run in either: [`PLAIN`]'s 24 shadows
    /// make one run in four shared layers and three runs of 8 in layers of
    /// their own, and a side of 512 holds all of a run of 8, so that with
    /// layers of their own part of the atlas opened what all of it did.
    #[test]
    fn shadows_past_one_atlas_finish_alone() {
        /// An atlas side that holds two lanes of [`PLAIN`]'s or more, and
        /// fewer than a run of 8 of them.
        const PART_SIDE: usize = 400;
        /// An atlas side that holds no two lanes of [`PLAIN`]'s.
        const NONE_SIDE: usize = 100;
        for (shape, unshared) in [("shared", false), ("their own", true)] {
            let sides = [None, Some(PART_SIDE), Some(NONE_SIDE)];
            let read = sides.map(|side| {
                UNSHARED_LAYERS.with(|flag| flag.set(unshared));
                ATLAS_SIDE.with(|cap| cap.set(side));
                let read = page(PLAIN, false, false);
                ATLAS_SIDE.with(|cap| cap.set(None));
                UNSHARED_LAYERS.with(|flag| flag.set(false));
                read
            });
            let ([Some(whole), Some(part), Some(none)], Some(alone)) =
                (read, page(PLAIN, true, false))
            else {
                return;
            };
            if unshared {
                assert_eq!(
                    whole.layers, PLAIN.n,
                    "layers {shape}: not one each"
                );
            }
            for (what, read) in [("part", &part), ("none", &none)] {
                let differ = read
                    .bytes
                    .iter()
                    .zip(&alone.bytes)
                    .filter(|(a, b)| a != b)
                    .count();
                assert_eq!(
                    differ, 0,
                    "layers {shape}: {what} of the shadows in the atlas, apart"
                );
            }
            assert!(
                whole.opened < part.opened && part.opened < none.opened,
                "layers {shape}: opened {} with the atlas whole, {} with part \
                 of it, {} with none",
                whole.opened,
                part.opened,
                none.opened
            );
            assert!(
                none.opened <= alone.opened,
                "layers {shape}: none, more than alone"
            );
        }
    }

    /// Where Skia refuses the layer the chains would share, each is drawn
    /// and finished in a layer of its own the size of its region, as it is
    /// alone: the same bytes, and no pass shared -- each shadow opens the
    /// same targets, but for the copy a region-sized layer does not need.
    ///
    /// Only where chains share layers: elsewhere no shared layer is asked
    /// for, and a chain's own layer refused falls back to its region alone,
    /// whose shadow the run's atlas still takes.
    #[test]
    fn shadows_whose_shared_layer_is_refused_fall_back_to_their_own() {
        if !SHARES {
            eprintln!("skipped: chains share a layer on Metal alone");
            return;
        }
        let (Some(refused), Some(shared), Some(alone)) = (
            page(PLAIN, false, true),
            page(PLAIN, false, false),
            page(PLAIN, true, false),
        ) else {
            return;
        };
        let differ = refused
            .bytes
            .iter()
            .zip(&alone.bytes)
            .filter(|(a, b)| a != b)
            .count();
        assert_eq!(differ, 0, "refused, apart from alone");
        assert_eq!(refused.opened % PLAIN.n, 0, "refused, not each its own");
        assert_eq!(
            refused.opened / PLAIN.n,
            alone.opened / PLAIN.n - 1,
            "refused, not each alone but the copy"
        );
        assert!(refused.opened > shared.opened, "refused, yet shared");
    }
}

/// Chains a page plays one after another, sharing a GPU layer where
/// [`play_shared_chains`] lets them, held to the bytes each draws in a layer
/// of its own.
#[cfg(test)]
mod shared_chain_tests {
    use super::ALONE_CHAINS;
    use crate::{
        context::chain::SHARED_LAYERS,
        pixels::{PixelColorSpace, PixelDepth},
        prelude::*,
    };
    use std::f32::consts::TAU;

    /// The page every scene is drawn on.
    const PAGE: (f32, f32) = (720.0, 480.0);

    /// Whether chains share layers here: on Metal alone -- see
    /// `chain::shares_layers` -- which is the backend wherever it is built
    /// in, as `crate::gpu` chooses it.
    const SHARES: bool = cfg!(feature = "metal");

    /// The kinds of GPU page the scenes are drawn on. A float canvas has no
    /// GPU engine here -- see `Canvas::engine_kind` -- and JavaScript's
    /// `canvas.gpu = true`, which puts one there, is held to sharing nothing
    /// in `layers.test.js`.
    const PAGES: [(&str, PixelColorSpace); 3] = [
        ("sRGB", PixelColorSpace::Srgb),
        ("Display P3", PixelColorSpace::DisplayP3),
        ("Rec. 2020", PixelColorSpace::Rec2020),
    ];

    /// A scene: rounded rectangles and every third a circle, in rows, each
    /// drawn under the next of `filters` in a colour at the next of `alphas`,
    /// `gap` apart, at `scale`, and how its chains share layers.
    struct Scene {
        name: &'static str,
        filters: &'static [&'static str],
        alphas: &'static [f32],
        gap: f32,
        scale: f32,
        sharing: Sharing,
    }

    /// How a scene's chains share layers.
    #[derive(Clone, Copy)]
    enum Sharing {
        /// Every chain in one layer: each is far from all the others, and
        /// all take a layer of one depth.
        One,
        /// Each chain meets the ones drawn around it, so it shares a layer
        /// only with chains further off: fewer layers than chains, and more
        /// than one.
        Neighbours,
        /// One layer for each depth: the chains alternate between two
        /// depths, and a layer holds one.
        ByDepth,
    }

    /// The colours a scene draws in, at its alphas.
    const OPAQUE: &[f32] = &[1.0];
    const TRANSLUCENT: &[f32] = &[0.7];
    const MIXED: &[f32] = &[1.0, 0.7, 0.45];

    const SCENES: [Scene; 9] = [
        Scene {
            name: "drop shadows far apart",
            filters: &["drop-shadow(6px 4px 5px rgb(200 0 0 / 0.6))"],
            alphas: MIXED,
            gap: 80.0,
            scale: 1.0,
            sharing: Sharing::One,
        },
        Scene {
            name: "drop shadows far apart, scaled",
            filters: &["drop-shadow(-4px 3px 3px rgb(0 120 0 / 0.9))"],
            alphas: MIXED,
            gap: 60.0,
            scale: 1.5,
            sharing: Sharing::One,
        },
        Scene {
            name: "a blur and a shadow chained",
            filters: &[
                "blur(2px) drop-shadow(-5px 3px 4px rgb(0 0 200 / 0.8))",
            ],
            alphas: MIXED,
            gap: 70.0,
            scale: 1.0,
            sharing: Sharing::One,
        },
        Scene {
            name: "a shadow and a colour filter chained",
            filters: &["drop-shadow(3px 3px 2px black) saturate(30%)"],
            alphas: MIXED,
            gap: 60.0,
            scale: 1.0,
            sharing: Sharing::One,
        },
        // Reaches that meet: 6 sigma of 8 is 48 pixels, past the gap.
        Scene {
            name: "drop shadows whose reaches meet",
            filters: &["drop-shadow(4px 4px 8px rgb(0 0 0 / 0.7))"],
            alphas: MIXED,
            gap: 12.0,
            scale: 1.0,
            sharing: Sharing::Neighbours,
        },
        Scene {
            name: "opaque blurred fills far apart",
            filters: &["blur(4px)"],
            alphas: OPAQUE,
            gap: 80.0,
            scale: 1.0,
            sharing: Sharing::One,
        },
        Scene {
            name: "translucent blurred fills far apart, scaled",
            filters: &["blur(3px)"],
            alphas: TRANSLUCENT,
            gap: 60.0,
            scale: 1.5,
            sharing: Sharing::One,
        },
        Scene {
            name: "blurred fills whose reaches meet",
            filters: &["blur(8px)"],
            alphas: OPAQUE,
            gap: 12.0,
            scale: 1.0,
            sharing: Sharing::Neighbours,
        },
        Scene {
            name: "blurred fills opaque and translucent in turn",
            filters: &["blur(4px)"],
            alphas: &[1.0, 0.7],
            gap: 80.0,
            scale: 1.0,
            sharing: Sharing::ByDepth,
        },
    ];

    /// `scene` drawn on a fresh GPU page in `space`, read back as
    /// premultiplied 32-bit floats, with every chain alone where `alone` says
    /// so, and how many chain layers the read made.
    fn drawn(
        scene: &Scene,
        space: PixelColorSpace,
        alone: bool,
    ) -> (Vec<u8>, usize) {
        let mut canvas = Canvas::with_options(
            PAGE.0,
            PAGE.1,
            CanvasOptions {
                gpu: true,
                color_space: space,
                ..CanvasOptions::default()
            },
        )
        .expect("a canvas");
        assert_eq!(canvas.engine_kind(), EngineKind::Gpu, "a CPU page");
        let ctx = canvas.context();
        ctx.scale(scene.scale, scene.scale);
        let (w, h) = (36.0, 22.0);
        let mut i = 0;
        let mut y = 10.25;
        while (y + h) * scene.scale < PAGE.1 {
            let mut x = 10.5;
            while (x + w) * scene.scale < PAGE.0 {
                ctx.set_filter_css(scene.filters[i % scene.filters.len()])
                    .expect("a filter");
                let alpha = scene.alphas[i % scene.alphas.len()];
                ctx.set_fill_style_css(&format!(
                    "rgb({} {} {} / {alpha})",
                    (i * 37) % 256,
                    (i * 91) % 256,
                    (i * 53) % 256
                ))
                .expect("a colour");
                ctx.begin_path();
                match i % 3 {
                    2 => ctx
                        .arc(x + w / 2.0, y + h / 2.0, h / 2.0, 0.0, TAU, false)
                        .expect("a circle"),
                    _ => ctx
                        .round_rect(x, y, w, h, [7.0; 4])
                        .expect("a rounded rectangle"),
                }
                ctx.fill(FillRule::NonZero);
                x += w + scene.gap;
                i += 1;
            }
            y += h + scene.gap;
        }
        ALONE_CHAINS.with(|flag| flag.set(alone));
        let before = SHARED_LAYERS.with(std::cell::Cell::get);
        let data = ctx.get_image_data_with(
            0.0,
            0.0,
            PAGE.0,
            PAGE.1,
            &ImageDataExport {
                pixels: PixelExportOptions {
                    depth: PixelDepth::F32,
                    premultiplied: true,
                    ..PixelExportOptions::default()
                },
                ..ImageDataExport::default()
            },
        );
        let layers = SHARED_LAYERS.with(std::cell::Cell::get) - before;
        ALONE_CHAINS.with(|flag| flag.set(false));
        (data.expect("image data").pixels().to_vec(), layers)
    }

    /// Chains far enough apart share layers and land the bytes they do
    /// alone; chains whose reaches meet, and chains whose layers differ in
    /// depth, land them too, sharing only with the chains they can.
    #[test]
    fn chains_sharing_a_layer_draw_as_they_do_alone() {
        let mut probe = Canvas::new(1.0, 1.0);
        probe.set_gpu(true);
        if probe.engine_kind() != EngineKind::Gpu {
            eprintln!("skipped: no GPU engine");
            return;
        }
        for (page, space) in PAGES {
            for scene in &SCENES {
                let (alone, alone_layers) = drawn(scene, space, true);
                let (shared, shared_layers) = drawn(scene, space, false);
                let differ = alone.iter().zip(&shared).filter(|(a, b)| a != b);
                assert_eq!(
                    differ.count(),
                    0,
                    "{}, on a {page} page: sharing changed the page",
                    scene.name
                );
                assert!(alone_layers > 1, "{}: one layer alone", scene.name);
                let expected = match scene.sharing {
                    _ if !SHARES => shared_layers == alone_layers,
                    Sharing::One => shared_layers == 1,
                    Sharing::Neighbours => {
                        1 < shared_layers && shared_layers < alone_layers
                    }
                    Sharing::ByDepth => shared_layers == 2,
                };
                assert!(
                    expected,
                    "{}, on a {page} page: {shared_layers} layers shared, \
                     {alone_layers} alone",
                    scene.name
                );
            }
        }
    }

    /// 40 rounded rectangles along a line, each 97 pixels on from the last
    /// and `rise` pixels down, under `blur(4px)`, translucent and in turn two
    /// colours, on a page large enough that none wraps round: a `rise` of 61
    /// is #499's scene, the bench row's. Read as premultiplied 32-bit floats
    /// with every chain alone or not, and how many chain layers the read
    /// made.
    fn along_a_line(rise: usize, alone: bool) -> (Vec<u8>, usize) {
        const PAGE: (f32, f32) = (4000.0, 2600.0);
        let mut canvas = Canvas::new(PAGE.0, PAGE.1);
        canvas.set_gpu(true);
        let ctx = canvas.context();
        ctx.set_filter_css("blur(4px)").expect("a filter");
        for i in 0..40 {
            ctx.set_fill_style_css(
                ["rgb(200 40 20 / 0.6)", "rgb(20 60 200 / 0.6)"][i % 2],
            )
            .expect("a colour");
            ctx.begin_path();
            ctx.round_rect(
                40.0 + (i * 97) as f32 + 0.37,
                40.0 + (i * rise) as f32 + 0.25,
                80.0,
                40.0,
                [12.0; 4],
            )
            .expect("a rounded rectangle");
            ctx.fill(FillRule::NonZero);
        }
        ALONE_CHAINS.with(|flag| flag.set(alone));
        let before = SHARED_LAYERS.with(std::cell::Cell::get);
        let data = ctx.get_image_data_with(
            0.0,
            0.0,
            PAGE.0,
            PAGE.1,
            &ImageDataExport {
                pixels: PixelExportOptions {
                    depth: PixelDepth::F32,
                    premultiplied: true,
                    ..PixelExportOptions::default()
                },
                ..ImageDataExport::default()
            },
        );
        let layers = SHARED_LAYERS.with(std::cell::Cell::get) - before;
        ALONE_CHAINS.with(|flag| flag.set(false));
        (data.expect("image data").pixels().to_vec(), layers)
    }

    /// Chains each meeting the next, as #499's shapes along a line do, take
    /// two layers between them rather than one each, and land the bytes they
    /// do alone. On #499's diagonal neighbours' results meet only corner to
    /// corner, where both are under a level; along a row they face each
    /// other, and their results overlap, translucent and in two colours, so
    /// that compositing them in any order but the one they were drawn in
    /// shows -- each still meeting only the chains either side.
    #[test]
    fn chains_along_a_line_share_two_layers() {
        let mut probe = Canvas::new(1.0, 1.0);
        probe.set_gpu(true);
        if probe.engine_kind() != EngineKind::Gpu {
            eprintln!("skipped: no GPU engine");
            return;
        }
        for rise in [61, 0] {
            let (alone, alone_layers) = along_a_line(rise, true);
            let (shared, shared_layers) = along_a_line(rise, false);
            assert_eq!(alone_layers, 40, "rise {rise}: not a layer each alone");
            let differ =
                alone.iter().zip(&shared).filter(|(a, b)| a != b).count();
            assert_eq!(differ, 0, "rise {rise}: sharing changed the page");
            assert_eq!(
                shared_layers,
                if SHARES { 2 } else { 40 },
                "rise {rise}: not two layers between them on Metal, or one \
                 each elsewhere"
            );
        }
    }
}

#[cfg(test)]
mod coverage_run_tests {
    //! Which runs of text under `blur()` the coverage route plays -- see
    //! [`CoverageRun`] -- and that an opaque one keeps the chain's alpha.
    //! The bytes are held to the exact product in `tests/blur_text.rs`.

    use super::{COVERAGE_ROUTE, COVERAGE_RUNS};
    use crate::{
        context::exact_blur,
        pixels::{PixelColorSpace, PixelDepth},
        prelude::*,
    };

    /// The family Oswald is registered under for these runs, so that it
    /// cannot be found as a system font of the same name instead.
    const OSWALD: &str = "CoverageRunOswald";

    /// The page every run is played on, wide enough for 60px Oswald
    /// "Hamburgefonstiv" and a reach of 8 pixels' blur either side.
    const PAGE: (usize, usize) = (220, 70);

    /// Further from a half-level than float noise moves a sum: an alpha
    /// nearer a tie than this may round to either side of it.
    const TIE: f64 = 1e-3;

    /// A run of text through `filter`, how it is filled, the page it is
    /// drawn on, and the depth and space of the page that one is drawn
    /// onto, if any.
    struct Run {
        filter: &'static str,
        fill: &'static str,
        /// The font's size, in pixels.
        size: u32,
        gradient: bool,
        decoration: &'static str,
        depth: PixelDepth,
        onto: Option<(PixelDepth, PixelColorSpace)>,
    }

    const OPAQUE: Run = Run {
        filter: "blur(3px)",
        fill: "rgb(200, 80, 30)",
        size: 32,
        gradient: false,
        decoration: "none",
        depth: PixelDepth::Uint8,
        onto: None,
    };

    /// `run` played on a CPU page, through the coverage route or the
    /// chain's layer, as premultiplied pixels, and how many draws took the
    /// route.
    fn played(run: &Run, coverage: bool) -> (Vec<u8>, usize) {
        FontLibrary::new()
            .use_paths(
                Some(OSWALD),
                ["tests/assets/fonts/Oswald/Oswald-VariableFont_wght.ttf"],
            )
            .expect("registers");
        COVERAGE_ROUTE.with(|route| route.set(coverage));
        let mut canvas = Canvas::with_options(
            PAGE.0 as f32,
            PAGE.1 as f32,
            CanvasOptions {
                color_type: run.depth,
                gpu: false,
                ..CanvasOptions::default()
            },
        )
        .expect("a canvas");
        let ctx = canvas.context();
        assert!(ctx.set_font_css(&format!("{}px {OSWALD}", run.size)));
        match run.gradient {
            true => {
                let stop = |position, css| GradientStop {
                    position,
                    color: RgbaLinear::from_css(css).expect("a colour"),
                };
                let shader = Shader::linear_gradient(
                    Point { x: 10.0, y: 0.0 },
                    Point { x: 200.0, y: 0.0 },
                    &[stop(0.0, "orange"), stop(1.0, "blue")],
                    GradientColorSpace::Srgb,
                )
                .expect("two stops describe a gradient");
                ctx.set_fill_shader(&shader);
            }
            false => ctx.set_fill_style_css(run.fill).expect("a colour"),
        }
        ctx.set_text_decoration_css(run.decoration)
            .expect("a decoration");
        ctx.set_filter_css(run.filter).expect("a filter");
        ctx.fill_text("Hamburgefonstiv", 12.3, 46.6, None);
        let before = COVERAGE_RUNS.with(|runs| runs.get());
        let mut page = match run.onto {
            Some((depth, space)) => {
                let mut page = Canvas::with_options(
                    PAGE.0 as f32,
                    PAGE.1 as f32,
                    CanvasOptions {
                        color_type: depth,
                        color_space: space,
                        gpu: false,
                        ..CanvasOptions::default()
                    },
                )
                .expect("a canvas");
                page.context().draw_canvas(&mut canvas, 0.0, 0.0);
                page
            }
            None => canvas,
        };
        let pixels = page
            .to_image_data(
                None,
                &ImageDataExport {
                    pixels: PixelExportOptions {
                        premultiplied: true,
                        ..PixelExportOptions::default()
                    },
                    ..ImageDataExport::default()
                },
            )
            .expect("pixels")
            .pixels()
            .to_vec();
        COVERAGE_ROUTE.with(|route| route.set(true));
        (pixels, COVERAGE_RUNS.with(|runs| runs.get()) - before)
    }

    /// The alpha bytes of premultiplied `pixels`.
    fn alphas(pixels: &[u8]) -> Vec<u8> {
        pixels.chunks(4).map(|p| p[3]).collect()
    }

    /// The pixels where the alphas of `route` and `chain` part and the
    /// exact blur by `sigma` of `opaque` -- the run in its colour opaque and
    /// unblurred, whose alpha is its coverage -- at `gain` is no tie. Each
    /// rounds the exact product once, but through the FFT a value within
    /// float noise of a half-level lands on either side of it, and which
    /// side depends on the FFT's algorithm, which rustfft picks for the
    /// machine.
    fn apart_off_ties(
        route: &[u8],
        chain: &[u8],
        opaque: &Run,
        sigma: f32,
        gain: f64,
    ) -> Vec<usize> {
        let (route, chain) = (alphas(route), alphas(chain));
        let parted: Vec<usize> =
            (0..route.len()).filter(|&i| route[i] != chain[i]).collect();
        if parted.is_empty() {
            return parted;
        }
        let (plain, _) = played(opaque, false);
        let exact = exact_blur::reference(&plain, PAGE.0, PAGE.1, sigma, sigma);
        parted
            .into_iter()
            .filter(|&i| {
                let v = exact[i * 4 + 3] * gain;
                (v - v.floor() - 0.5).abs() >= TIE
            })
            .collect()
    }

    #[test]
    fn translucent_text_in_the_chain_keeps_the_routes_alpha() {
        // Where the route does not play, a run in one flat colour through
        // the chain's layer is drawn opaque, its alpha the chain's gain, so
        // that its alpha is rounded once, as the route's is: byte for byte
        // the route's but on a tie -- see `apart_off_ties` -- at sigmas tap
        // by tap and through the FFT, at sizes from a hinted 13px to 60px,
        // at alphas on and off a byte's tie.
        let mut apart = Vec::new();
        for (fill, opaque, given) in [
            ("rgba(200, 80, 30, 0.6)", "rgb(200, 80, 30)", 0.6),
            ("rgba(20, 60, 200, 0.3)", "rgb(20, 60, 200)", 0.3),
            ("rgba(128, 128, 128, 0.55)", "rgb(128, 128, 128)", 0.55),
        ] {
            // The alpha the run is drawn at, on its byte.
            let gain = (given * 255.0f64).round() / 255.0;
            for (filter, sigma) in [("blur(3px)", 3.0), ("blur(8px)", 8.0)] {
                for size in [13, 32, 60] {
                    let run = Run {
                        fill,
                        filter,
                        size,
                        ..OPAQUE
                    };
                    let (route, runs) = played(&run, true);
                    let (chain, chained) = played(&run, false);
                    assert_eq!(
                        (runs, chained),
                        (1, 0),
                        "{fill} {filter} {size}px"
                    );
                    let plain = Run {
                        fill: opaque,
                        filter: "none",
                        size,
                        ..OPAQUE
                    };
                    let off =
                        apart_off_ties(&route, &chain, &plain, sigma, gain);
                    if !off.is_empty() {
                        apart.push(format!(
                            "{fill} {filter} {size}px: {} alphas off a tie",
                            off.len()
                        ));
                    }
                }
            }
        }
        assert!(apart.is_empty(), "the chain's alpha moved: {apart:#?}");
    }

    #[test]
    fn translucent_text_drawn_onto_another_space_keeps_the_routes_alpha() {
        // A page drawn onto one in another space is where the route does
        // not play and the chain does: its alpha is the route's, byte for
        // byte but on a tie -- see `apart_off_ties` -- as it would be on a
        // page of its own space.
        let plain = Run {
            fill: "rgb(20, 60, 200)",
            filter: "none",
            ..OPAQUE
        };
        let gain = (0.3f64 * 255.0).round() / 255.0;
        for (filter, sigma) in [("blur(3px)", 3.0), ("blur(8px)", 8.0)] {
            let run = Run {
                fill: "rgba(20, 60, 200, 0.3)",
                filter,
                ..OPAQUE
            };
            let (own, runs) = played(&run, true);
            let onto = Run {
                onto: Some((PixelDepth::Uint8, PixelColorSpace::DisplayP3)),
                ..run
            };
            let (other, chained) = played(&onto, true);
            assert_eq!((runs, chained), (1, 0), "{filter}");
            let off = apart_off_ties(&own, &other, &plain, sigma, gain);
            assert!(off.is_empty(), "{filter}: {} alphas off a tie", off.len());
        }
    }

    #[test]
    fn opaque_text_keeps_the_chains_alpha() {
        // The route blurs the same coverage the chain's layer holds, so an
        // opaque run's alpha is the chain's byte for byte, and only its
        // colour, applied after the blur, moves.
        let (route, runs) = played(&OPAQUE, true);
        let (chain, chained) = played(&OPAQUE, false);
        assert_eq!((runs, chained), (1, 0), "the route took the run once");
        assert_eq!(alphas(&route), alphas(&chain), "the alpha moved");
        assert_ne!(route, chain, "the colour did not move");
    }

    #[test]
    fn a_run_the_route_cannot_take_keeps_the_chain() {
        let cases = [
            (
                "a translucent colour",
                1,
                Run {
                    fill: "rgba(200, 80, 30, 0.6)",
                    ..OPAQUE
                },
            ),
            (
                "a sigma through the FFT",
                1,
                Run {
                    filter: "blur(6px)",
                    ..OPAQUE
                },
            ),
            (
                "a chain of two blurs",
                0,
                Run {
                    filter: "blur(2px) blur(2px)",
                    ..OPAQUE
                },
            ),
            (
                "a blur after another filter",
                0,
                Run {
                    filter: "opacity(0.5) blur(3px)",
                    ..OPAQUE
                },
            ),
            (
                "a gradient",
                0,
                Run {
                    gradient: true,
                    ..OPAQUE
                },
            ),
            (
                "a line in a colour of its own",
                0,
                Run {
                    decoration: "underline blue",
                    ..OPAQUE
                },
            ),
            (
                "a half-float page",
                0,
                Run {
                    depth: PixelDepth::F16,
                    ..OPAQUE
                },
            ),
            (
                "a float page",
                0,
                Run {
                    depth: PixelDepth::F32,
                    ..OPAQUE
                },
            ),
            (
                "a page drawn onto a half-float one",
                0,
                Run {
                    onto: Some((PixelDepth::F16, PixelColorSpace::Srgb)),
                    ..OPAQUE
                },
            ),
            (
                "an sRGB page drawn onto a Display P3 one",
                0,
                Run {
                    onto: Some((PixelDepth::Uint8, PixelColorSpace::DisplayP3)),
                    ..OPAQUE
                },
            ),
            (
                "a page drawn onto one like it",
                1,
                Run {
                    onto: Some((PixelDepth::Uint8, PixelColorSpace::Srgb)),
                    ..OPAQUE
                },
            ),
        ];
        for (case, expected, run) in cases {
            assert_eq!(played(&run, true).1, expected, "{case}");
        }
    }
}

#[cfg(test)]
mod shadow_run_tests {
    //! A translucent run of text under `drop-shadow()` on an eight-bit
    //! page, on the CPU and on the GPU: the run as Skia draws it, over the
    //! exact blur of its coverage times its alpha, tinted, composited and
    //! rounded once -- see `chain::Cast`. And one under `blur()`: the exact
    //! blur of the run drawn opaque times its alpha, rounded once -- see
    //! `CoverageRun`.

    use std::ops::Range;

    use skia_safe::IRect;

    use super::CAST_RUNS;
    use crate::{
        EngineKind,
        context::{REPLAYS_TRANSLUCENT_TEXT, exact_blur},
        pixels::PixelDepth,
        prelude::*,
    };

    /// The family Oswald is registered under for these runs, so that it
    /// cannot be found as a system font of the same name instead.
    const OSWALD: &str = "ShadowRunOswald";

    /// The page every run is played on: 60px Oswald "Hamburgefonstiv" at
    /// [`AT`], its shadow moved by [`OFFSET`] and blurred by 8 pixels, lies
    /// inside it with the blur's reach to spare, so that the page's edges
    /// cut nothing the reference does not see -- which each run checks.
    const PAGE: (usize, usize) = (520, 150);

    /// How far apart a run's lines lie, each a [`PAGE`] of its own stacked
    /// under the one before: far enough that no line's layer reaches
    /// another's ink, so that on the GPU they share a layer and an atlas.
    const LINE_GAP: usize = PAGE.1;

    /// Where each run's baseline starts.
    const AT: (f32, f32) = (30.3, 85.6);

    /// The shadow's offset, in whole pixels, so that it is moved
    /// unresampled.
    const OFFSET: (i32, i32) = (3, 2);

    /// Further from a half-level than float noise moves a sum: a channel
    /// nearer a tie than this may round to either side of it.
    const TIE: f64 = 1e-3;

    /// "Hamburgefonstiv" filled with `fill` through `filter` at `size`
    /// pixels, on a page of `depth`, on the GPU where `gpu` says, once on
    /// each of `lines` lines [`LINE_GAP`] apart.
    #[derive(Clone, Copy)]
    struct Run<'a> {
        fill: &'a str,
        filter: &'a str,
        size: u32,
        depth: PixelDepth,
        dither: bool,
        gradient: bool,
        /// A canvas shadow's blur, in place of a filter's.
        shadow_blur: Option<f32>,
        gpu: bool,
        lines: usize,
    }

    impl Run<'_> {
        /// The page the run is played on: [`PAGE`] for each line.
        fn page(&self) -> (usize, usize) {
            (PAGE.0, PAGE.1 + LINE_GAP * (self.lines - 1))
        }
    }

    /// Whether a canvas on this machine is drawn on the GPU.
    fn gpu_available() -> bool {
        let mut probe = Canvas::new(1.0, 1.0);
        probe.set_gpu(true);
        probe.engine_kind() == EngineKind::Gpu
    }

    /// The engines a run is checked on: the CPU, and the GPU alone and in a
    /// run of three lines sharing a layer and an atlas, where there is one.
    fn engines() -> Vec<(bool, usize)> {
        let mut engines = vec![(false, 1)];
        match gpu_available() {
            true => engines.extend([(true, 1), (true, 3)]),
            false => eprintln!("the GPU unchecked: no GPU engine"),
        }
        engines
    }

    const TRANSLUCENT: Run = Run {
        fill: "rgba(200, 80, 30, 0.6)",
        filter: "drop-shadow(3px 2px 3px black)",
        size: 32,
        depth: PixelDepth::Uint8,
        dither: false,
        gradient: false,
        shadow_blur: None,
        gpu: false,
        lines: 1,
    };

    /// `run` played, as premultiplied pixels, and how many shadows were cast
    /// from its opaque copy.
    fn played(run: &Run) -> (Vec<u8>, usize) {
        FontLibrary::new()
            .use_paths(
                Some(OSWALD),
                ["tests/assets/fonts/Oswald/Oswald-VariableFont_wght.ttf"],
            )
            .expect("registers");
        let (width, height) = run.page();
        let mut canvas = Canvas::with_options(
            width as f32,
            height as f32,
            CanvasOptions {
                color_type: run.depth,
                gpu: run.gpu,
                ..CanvasOptions::default()
            },
        )
        .expect("a canvas");
        // An eight-bit page is drawn on the engine asked for; what a
        // half-float page asking for the GPU is drawn on is not this test's.
        if run.depth == PixelDepth::Uint8 {
            assert_eq!(
                canvas.engine_kind() == EngineKind::Gpu,
                run.gpu,
                "the engine asked for"
            );
        }
        let ctx = canvas.context();
        assert!(ctx.set_font_css(&format!("{}px {OSWALD}", run.size)));
        match run.gradient {
            true => {
                let stop = |position, css| GradientStop {
                    position,
                    color: RgbaLinear::from_css(css).expect("a colour"),
                };
                let shader = Shader::linear_gradient(
                    Point { x: 10.0, y: 0.0 },
                    Point { x: 400.0, y: 0.0 },
                    &[
                        stop(0.0, "rgba(255, 165, 0, 0.6)"),
                        stop(1.0, "rgba(0, 0, 255, 0.6)"),
                    ],
                    GradientColorSpace::Srgb,
                )
                .expect("two stops describe a gradient");
                ctx.set_fill_shader(&shader);
            }
            false => ctx.set_fill_style_css(run.fill).expect("a colour"),
        }
        ctx.set_dither(run.dither);
        ctx.set_filter_css(run.filter).expect("a filter");
        if let Some(blur) = run.shadow_blur {
            ctx.set_shadow_color_css("black").expect("a colour");
            ctx.set_shadow_blur(blur);
            ctx.set_shadow_offset(OFFSET.0 as f32, OFFSET.1 as f32);
        }
        let before = CAST_RUNS.get();
        for line in 0..run.lines {
            let y = AT.1 + (line * LINE_GAP) as f32;
            ctx.fill_text("Hamburgefonstiv", AT.0, y, None);
        }
        let pixels = canvas
            .to_image_data(
                None,
                &ImageDataExport {
                    pixels: PixelExportOptions {
                        premultiplied: true,
                        ..PixelExportOptions::default()
                    },
                    ..ImageDataExport::default()
                },
            )
            .expect("pixels")
            .pixels()
            .to_vec();
        (pixels, CAST_RUNS.get() - before)
    }

    /// How many pixels of `run`, whose shadow in `tint` -- unpremultiplied,
    /// its colour in levels and its alpha a fraction -- is blurred by
    /// `sigma`, part from the exact shadow in any of `channels`: the run as
    /// Skia draws it unfiltered, over the exact blur of its coverage, the
    /// alpha of `opaque`, times `gain` and the tint's alpha, tinted, in
    /// floats, rounded once. A channel within [`TIE`] of a half-level may
    /// land on either side, since through the FFT which side depends on the
    /// machine.
    fn off_the_exact_shadow(
        run: &Run,
        opaque: &Run,
        sigma: f32,
        tint: [f64; 4],
        gain: f64,
        channels: Range<usize>,
    ) -> usize {
        let (page, cast) = played(run);
        assert_eq!(cast, run.lines, "{}: cast once a line", run.filter);
        let (width, height) = run.page();
        let border = (0..width * height).filter(|i| {
            let (x, y) = (i % width, i / width);
            x == 0 || y == 0 || x == width - 1 || y == height - 1
        });
        assert!(
            border.clone().all(|i| page[i * 4 + 3] == 0),
            "{}px {}: the page cuts the shadow",
            run.size,
            run.filter
        );
        let (body, _) = played(&Run {
            filter: "none",
            ..*run
        });
        let (coverage, _) = played(&Run {
            filter: "none",
            ..*opaque
        });
        let blurred =
            exact_blur::reference(&coverage, width, height, sigma, sigma);
        let mut off = 0;
        for y in 0..height as i32 {
            for x in 0..width as i32 {
                let (sx, sy) = (x - OFFSET.0, y - OFFSET.1);
                let inside = (0..width as i32).contains(&sx)
                    && (0..height as i32).contains(&sy);
                let cast = match inside {
                    true => {
                        let at = sy as usize * width + sx as usize;
                        blurred[at * 4 + 3] / 255.0
                    }
                    false => 0.0,
                };
                let alpha = cast * gain * tint[3];
                let i = (y as usize * width + x as usize) * 4;
                let under = 1.0 - f64::from(body[i + 3]) / 255.0;
                let shadow = [
                    tint[0] / 255.0 * alpha,
                    tint[1] / 255.0 * alpha,
                    tint[2] / 255.0 * alpha,
                    alpha,
                ];
                let missed = channels.clone().any(|c| {
                    let v = f64::from(body[i + c]) + 255.0 * shadow[c] * under;
                    let got = f64::from(page[i + c]);
                    let tie = (v - v.floor() - 0.5).abs() < TIE;
                    got != v.round()
                        && !(tie && (got == v.floor() || got == v.ceil()))
                });
                off += usize::from(missed);
            }
        }
        off
    }

    #[test]
    fn a_translucent_runs_shadow_rounds_its_alpha_once() {
        // At sigmas summed tap by tap and through the FFT, in black and in a
        // translucent colour, at sizes from a hinted 13px to 60px, at
        // alphas on and off a byte's tie; on the CPU, and on the GPU alone
        // and sharing a layer and an atlas.
        let engines = engines();
        let mut apart = Vec::new();
        for (rgb, given) in [
            ("200, 80, 30", 0.6),
            ("20, 60, 200", 0.3),
            ("128, 128, 128", 0.55),
        ] {
            // The alpha the run is drawn at, on its byte.
            let gain = (given * 255.0f64).round() / 255.0;
            let fill = format!("rgba({rgb}, {given})");
            let opaque = format!("rgb({rgb})");
            for size in [13, 32, 60] {
                for (filter, sigma, tint) in [
                    (
                        "drop-shadow(3px 2px 3px black)",
                        3.0,
                        [0.0, 0.0, 0.0, 1.0],
                    ),
                    (
                        "drop-shadow(3px 2px 8px rgba(30, 120, 220, 0.75))",
                        8.0,
                        [30.0, 120.0, 220.0, 0.75],
                    ),
                ] {
                    for &(gpu, lines) in &engines {
                        let run = Run {
                            fill: &fill,
                            filter,
                            size,
                            gpu,
                            lines,
                            ..TRANSLUCENT
                        };
                        let opaque = Run {
                            fill: &opaque,
                            ..run
                        };
                        let off = off_the_exact_shadow(
                            &run,
                            &opaque,
                            sigma,
                            tint,
                            gain,
                            0..4,
                        );
                        if off > 0 {
                            apart.push(format!(
                                "{fill} {size}px {filter}, gpu {gpu}, \
                                 {lines} lines: {off}"
                            ));
                        }
                    }
                }
            }
        }
        assert!(apart.is_empty(), "pixels off the exact shadow: {apart:#?}");
    }

    #[test]
    fn a_translucent_run_casting_a_shadow_apart_is_drawn_as_casting_none() {
        // A run casting a `shadowColor` shadow under a `drop-shadow()` of
        // its own has the shadow filed apart and is run through its filter's
        // chain as a run casting none -- see `Context2D::render_to_canvas`
        // -- so it is drawn from its coverage as that run is: cast off the
        // page, the shadow leaves the two pages alike.
        use super::FLAT_RUNS;
        FontLibrary::new()
            .use_paths(
                Some(OSWALD),
                ["tests/assets/fonts/Oswald/Oswald-VariableFont_wght.ttf"],
            )
            .expect("registers");
        let drawn = |shadow: bool| {
            let mut canvas = Canvas::with_options(
                PAGE.0 as f32,
                PAGE.1 as f32,
                CanvasOptions {
                    gpu: false,
                    ..CanvasOptions::default()
                },
            )
            .expect("a canvas");
            let ctx = canvas.context();
            assert!(ctx.set_font_css(SIXTY));
            ctx.set_fill_style_css(TRANSLUCENT.fill).expect("a colour");
            ctx.set_filter_css(TRANSLUCENT.filter).expect("a filter");
            if shadow {
                ctx.set_shadow_color_css("black").expect("a colour");
                // A page's height and more below the run: off the page.
                ctx.set_shadow_offset(0.0, (2 * PAGE.1) as f32);
            }
            let before = FLAT_RUNS.get();
            ctx.fill_text("Hamburgefonstiv", AT.0, AT.1, None);
            let pixels = canvas
                .to_image_data(None, &ImageDataExport::default())
                .expect("pixels")
                .pixels()
                .to_vec();
            (pixels, FLAT_RUNS.get() - before)
        };
        let (alone, flat) = drawn(false);
        assert!(flat > 0, "the control: the run casting none is not flat");
        let (casting, flat) = drawn(true);
        assert!(casting == alone, "the run casting a shadow parts");
        assert!(flat > 0, "the run casting a shadow is not drawn flat");
    }

    #[test]
    fn a_translucent_runs_shadow_on_the_gpu_is_made_there() {
        // Alone, through `shadowed_on_gpu`; sharing a layer, through the
        // atlas, each line with a lane of its own for its cast. Neither reads
        // the cast back for the CPU to make the shadow, the stall the GPU's
        // route exists to spare.
        use crate::context::chain::{CAST_LANES, CASTS_READ_BACK};
        if !gpu_available() {
            eprintln!("skipped: no GPU engine");
            return;
        }
        for (fill, lines, cast, lanes) in [
            (TRANSLUCENT.fill, 1, 1, 0),
            (TRANSLUCENT.fill, 3, 3, 3),
            ("rgb(200, 80, 30)", 3, 0, 0),
        ] {
            let run = Run {
                fill,
                gpu: true,
                lines,
                ..TRANSLUCENT
            };
            let (staged, read) = (CAST_LANES.get(), CASTS_READ_BACK.get());
            assert_eq!(played(&run).1, cast, "{fill}, {lines} lines: cast");
            assert_eq!(
                CAST_LANES.get() - staged,
                lanes,
                "{fill}, {lines} lines: cast lanes"
            );
            assert_eq!(
                CASTS_READ_BACK.get() - read,
                0,
                "{fill}, {lines} lines: read back"
            );
        }
    }

    #[test]
    fn lines_sharing_a_layer_on_the_gpu_draw_as_each_does_alone() {
        // Translucent lines cast from their copies and an opaque one cast
        // from itself, in one run sharing a layer and an atlas: each line's
        // band is the bytes it lands drawn by itself, which is a chain alone.
        // And where the lines share no layer -- Skia refuses the one they
        // would share, or the backend shares none, as Vulkan does -- each line
        // is drawn and finished alone after all, its cast with it.
        use crate::context::chain::{REFUSE_PLACED, UNSHARED_LAYERS};
        if !gpu_available() {
            eprintln!("skipped: no GPU engine");
            return;
        }
        for (refuse, unshared) in [(false, false), (true, false), (false, true)]
        {
            REFUSE_PLACED.set(refuse);
            UNSHARED_LAYERS.set(unshared);
            lines_against_themselves_alone(refuse, unshared);
            REFUSE_PLACED.set(false);
            UNSHARED_LAYERS.set(false);
        }
    }

    /// Whether chains share layers here: on Metal alone -- see
    /// `chain::shares_layers` -- which is the backend wherever it is built
    /// in, as `crate::gpu` chooses it.
    const SHARES: bool = cfg!(feature = "metal");

    /// The body of `lines_sharing_a_layer_on_the_gpu_draw_as_each_does_alone`,
    /// with the shared layer refused where `refused` says, and sharing turned
    /// off where `unshared` does.
    fn lines_against_themselves_alone(refused: bool, unshared: bool) {
        use crate::context::chain::SHARED_LAYERS;
        let fills = [
            TRANSLUCENT.fill,
            "rgb(200, 80, 30)",
            "rgba(20, 60, 200, 0.3)",
        ];
        let run = Run {
            gpu: true,
            lines: fills.len(),
            ..TRANSLUCENT
        };
        let (width, height) = run.page();
        let draw = |only: Option<usize>| {
            let mut canvas = Canvas::with_options(
                width as f32,
                height as f32,
                CanvasOptions {
                    gpu: true,
                    ..CanvasOptions::default()
                },
            )
            .expect("a canvas");
            let ctx = canvas.context();
            assert!(ctx.set_font_css(&format!("{}px {OSWALD}", run.size)));
            ctx.set_filter_css(run.filter).expect("a filter");
            for (line, fill) in fills.iter().enumerate() {
                if only.is_some_and(|only| only != line) {
                    continue;
                }
                ctx.set_fill_style_css(fill).expect("a colour");
                let y = AT.1 + (line * LINE_GAP) as f32;
                ctx.fill_text("Hamburgefonstiv", AT.0, y, None);
            }
            canvas
                .to_image_data(None, &ImageDataExport::default())
                .expect("pixels")
                .pixels()
                .to_vec()
        };
        FontLibrary::new()
            .use_paths(
                Some(OSWALD),
                ["tests/assets/fonts/Oswald/Oswald-VariableFont_wght.ttf"],
            )
            .expect("registers");
        let layers = SHARED_LAYERS.get();
        let casts = CAST_RUNS.get();
        let shared = draw(None);
        // One layer the lines are drawn in, and one their casts are; or
        // sharing none, a layer and a cast's for each translucent line, and a
        // layer for the opaque one.
        let expected = match SHARES && !refused && !unshared {
            true => 2,
            false => 5,
        };
        assert_eq!(
            SHARED_LAYERS.get() - layers,
            expected,
            "layers, refused {refused}, unshared {unshared}"
        );
        assert_eq!(
            CAST_RUNS.get() - casts,
            2,
            "casts, refused {refused}, unshared {unshared}"
        );
        let band = width * LINE_GAP * 4;
        for (line, fill) in fills.iter().enumerate() {
            let alone = draw(Some(line));
            let at = line * band..(line + 1) * band;
            let differ = shared[at.clone()]
                .iter()
                .zip(&alone[at])
                .filter(|(a, b)| a != b)
                .count();
            assert_eq!(
                differ, 0,
                "line {line} in {fill} parts from itself alone, refused \
                 {refused}, unshared {unshared}"
            );
        }
    }

    /// How many alphas of `run`, under a blur of `sigma`, part from the
    /// exact blur of the alpha of `opaque` -- the run in its colour opaque,
    /// whose alpha is its coverage -- times `gain`, rounded once. An alpha
    /// within [`TIE`] of a half-level may land on either side. The colour is
    /// not held here: an edge's colour byte is the coverage route's to move
    /// by a level -- see `chain::coverage`.
    fn off_the_exact_blur(
        run: &Run,
        opaque: &Run,
        sigma: f32,
        gain: f64,
    ) -> usize {
        let (page, _) = played(run);
        let (width, height) = run.page();
        let (copy, _) = played(&Run {
            filter: "none",
            ..*opaque
        });
        let blurred = exact_blur::reference(&copy, width, height, sigma, sigma);
        page.iter()
            .zip(&blurred)
            .skip(3)
            .step_by(4)
            .filter(|&(&got, &exact)| {
                let (got, v) = (f64::from(got), exact * gain);
                let tie = (v - v.floor() - 0.5).abs() < TIE;
                got != v.round()
                    && !(tie && (got == v.floor() || got == v.ceil()))
            })
            .count()
    }

    #[test]
    fn a_translucent_runs_blur_rounds_its_alpha_once() {
        // At sigmas summed tap by tap and through the FFT, at sizes from a
        // hinted 13px to 60px, at alphas on and off a byte's tie; on the
        // CPU, and on the GPU alone and sharing a layer.
        let engines = engines();
        let mut apart = Vec::new();
        for (rgb, given) in [
            ("200, 80, 30", 0.6),
            ("20, 60, 200", 0.3),
            ("128, 128, 128", 0.55),
        ] {
            // The alpha the run is drawn at, on its byte.
            let gain = (given * 255.0f64).round() / 255.0;
            let fill = format!("rgba({rgb}, {given})");
            let opaque = format!("rgb({rgb})");
            for size in [13, 32, 60] {
                for sigma in [3.0f32, 8.0] {
                    let filter = format!("blur({sigma}px)");
                    for &(gpu, lines) in &engines {
                        let run = Run {
                            fill: &fill,
                            filter: &filter,
                            size,
                            gpu,
                            lines,
                            ..TRANSLUCENT
                        };
                        let opaque = Run {
                            fill: &opaque,
                            ..run
                        };
                        let off =
                            off_the_exact_blur(&run, &opaque, sigma, gain);
                        if off > 0 {
                            apart.push(format!(
                                "{fill} {size}px {filter}, gpu {gpu}, \
                                 {lines} lines: {off}"
                            ));
                        }
                    }
                }
            }
        }
        assert!(apart.is_empty(), "alphas off the exact blur: {apart:#?}");
    }

    #[test]
    fn a_dithered_runs_shadow_rounds_its_alpha_once() {
        // Dither moves a pixel's colour and leaves its alpha, which is all a
        // shadow is cast from. Its colour is dithered where the layer lies
        // rather than where the page does, so only the alpha is held here.
        let run = Run {
            dither: true,
            ..TRANSLUCENT
        };
        let opaque = Run {
            fill: "rgb(200, 80, 30)",
            ..run
        };
        let gain = (0.6f64 * 255.0).round() / 255.0;
        let off = off_the_exact_shadow(
            &run,
            &opaque,
            3.0,
            [0.0, 0.0, 0.0, 1.0],
            gain,
            3..4,
        );
        assert_eq!(off, 0, "alphas off the exact shadow");
    }

    #[test]
    fn only_a_translucent_runs_sole_drop_shadow_is_cast_from_its_copy() {
        let cases = [
            ("a translucent run", 1, TRANSLUCENT),
            (
                "an opaque run",
                0,
                Run {
                    fill: "rgb(200, 80, 30)",
                    ..TRANSLUCENT
                },
            ),
            (
                "a drop shadow then a blur",
                0,
                Run {
                    filter: "drop-shadow(3px 2px 3px black) blur(1px)",
                    ..TRANSLUCENT
                },
            ),
            (
                "a blur then a drop shadow",
                0,
                Run {
                    filter: "blur(1px) drop-shadow(3px 2px 3px black)",
                    ..TRANSLUCENT
                },
            ),
            (
                "a canvas shadow",
                0,
                Run {
                    filter: "none",
                    shadow_blur: Some(6.0),
                    ..TRANSLUCENT
                },
            ),
            (
                "a gradient",
                0,
                Run {
                    gradient: true,
                    ..TRANSLUCENT
                },
            ),
            (
                "a half-float page",
                0,
                Run {
                    depth: PixelDepth::F16,
                    ..TRANSLUCENT
                },
            ),
        ];
        for (gpu, lines) in engines() {
            for (case, expected, run) in cases {
                let run = Run { gpu, lines, ..run };
                assert_eq!(
                    played(&run).1,
                    expected * lines,
                    "{case}, gpu {gpu}, {lines} lines"
                );
            }
        }
    }

    /// One page of [`pages`]: `text` in `font` filled with `fill` at `at`.
    #[derive(Clone, Copy)]
    struct Page<'a> {
        text: &'a str,
        font: &'a str,
        fill: &'a str,
        at: (f32, f32),
    }

    /// The font every page is drawn in unless it says otherwise: 60px
    /// Oswald, registered as [`OSWALD`].
    const SIXTY: &str = "60px ShadowRunOswald";

    const HAMBURGEFONSTIV: Page = Page {
        text: "Hamburgefonstiv",
        font: SIXTY,
        fill: TRANSLUCENT.fill,
        at: AT,
    };

    /// What a page of [`pages`] drew: its premultiplied pixels, how many
    /// layers a shadow was cast into, how many kept results it took, and
    /// how many bytes of casts' alphas it kept.
    struct Paged {
        pixels: Vec<u8>,
        cast_layers: usize,
        taken: usize,
        cast_bytes: usize,
        /// The region the page's cast chain ran on, where it ran one.
        region: Option<IRect>,
    }

    /// `pages` drawn in turn through `filter` on one eight-bit CPU page of
    /// [`PAGE`], cleared before each, kept results taken where `kept` says.
    /// One page, its font set only where a page's differs from the one
    /// before, because a run is kept by the canvas state it is laid out
    /// under -- see `node::typography::LaidOutKey`.
    fn pages(pages: &[Page], filter: &str, kept: bool) -> Vec<Paged> {
        use crate::context::{
            chain::{CAST_LAYERS, CAST_REGION},
            kept::{CAST_BYTES, FOUND, KEPT},
        };
        FontLibrary::new()
            .use_paths(
                Some(OSWALD),
                ["tests/assets/fonts/Oswald/Oswald-VariableFont_wght.ttf"],
            )
            .expect("registers");
        KEPT.set(kept);
        let mut canvas = Canvas::with_options(
            PAGE.0 as f32,
            PAGE.1 as f32,
            CanvasOptions {
                gpu: false,
                ..CanvasOptions::default()
            },
        )
        .expect("a canvas");
        canvas.context().set_filter_css(filter).expect("a filter");
        let mut font = "";
        let drawn = pages
            .iter()
            .map(|page| {
                let ctx = canvas.context();
                if page.font != font {
                    assert!(ctx.set_font_css(page.font));
                    font = page.font;
                }
                ctx.clear_rect(0.0, 0.0, PAGE.0 as f32, PAGE.1 as f32);
                ctx.set_fill_style_css(page.fill).expect("a colour");
                let (layers, taken, bytes) =
                    (CAST_LAYERS.get(), FOUND.get().taken, CAST_BYTES.get());
                CAST_REGION.set(None);
                ctx.fill_text(page.text, page.at.0, page.at.1, None);
                let pixels = canvas
                    .to_image_data(
                        None,
                        &ImageDataExport {
                            pixels: PixelExportOptions {
                                premultiplied: true,
                                ..PixelExportOptions::default()
                            },
                            ..ImageDataExport::default()
                        },
                    )
                    .expect("pixels")
                    .pixels()
                    .to_vec();
                Paged {
                    pixels,
                    cast_layers: CAST_LAYERS.get() - layers,
                    taken: FOUND.get().taken - taken,
                    cast_bytes: CAST_BYTES.get() - bytes,
                    region: CAST_REGION.get(),
                }
            })
            .collect();
        KEPT.set(true);
        drawn
    }

    /// `page` drawn through `filter` on a page of its own, nothing kept
    /// taken: what any page drawing it has to land.
    fn alone(page: Page, filter: &str) -> Vec<u8> {
        pages(&[page], filter, false).remove(0).pixels
    }

    /// Has this thread record a translucent run as Skia blits it -- see
    /// `REPLAYS_TRANSLUCENT_TEXT` -- for a test that needs two runs alike
    /// in their input and not in their cast. Blitted, an alpha of a byte of
    /// 1 lands only the pixels a run covers whole -- `(a * (c + 1)) >> 8` --
    /// where drawn from its coverage it lands every pixel covered by half,
    /// and no two runs searched here then land alike. The cast is filed by
    /// the same key either way, and a run the flat route leaves -- dithered,
    /// naming a line colour, on a page that does not place its colour on
    /// its bytes -- is drawn as blitted.
    fn as_skia_blits() {
        REPLAYS_TRANSLUCENT_TEXT.set(false);
    }

    #[test]
    fn a_shadow_cast_from_a_kept_run_is_taken_without_drawing_its_cast() {
        // A run is kept from its second draw -- see `Typesetter::laid_out`
        // -- and a result from its key's second sighting -- see
        // `kept::Seen` -- so the fourth page is the first that can take one.
        // The first, whose run is not yet kept, casts by its copy's alphas,
        // and keeps them where the page drawn alone sighted them before.
        let filter = TRANSLUCENT.filter;
        let reference = alone(HAMBURGEFONSTIV, filter);
        let drawn = pages(&[HAMBURGEFONSTIV; 5], filter, true);
        for (i, page) in drawn.iter().enumerate() {
            assert!(page.pixels == reference, "page {i} parts from its own");
        }
        assert!(drawn[0].cast_layers > 0, "the control: no cast was drawn");
        for page in &drawn[1..] {
            assert_eq!(page.cast_bytes, 0, "a cast's alphas were kept");
        }
        for page in &drawn[3..] {
            assert_eq!(page.taken, 1, "nothing kept was taken");
            assert_eq!(page.cast_layers, 0, "the cast was drawn");
        }
    }

    #[test]
    fn a_shadow_cast_from_a_run_moved_by_whole_pixels_takes_its_result() {
        // Moved by whole pixels, a run's glyphs lie in the classes they did,
        // so its copy blits the same masks, moved -- see
        // `run_mask::CastKey` -- and its input is the same to the byte.
        let filter = TRANSLUCENT.filter;
        let moved = |dx: f32, dy: f32| Page {
            at: (AT.0 + dx, AT.1 + dy),
            ..HAMBURGEFONSTIV
        };
        let places = [moved(1.0, 0.0), moved(0.0, 1.0), moved(-7.0, 3.0)];
        let mut drawn = vec![HAMBURGEFONSTIV; 4];
        drawn.extend(places);
        let drawn = pages(&drawn, filter, true);
        for (page, paged) in places.iter().zip(&drawn[4..]) {
            assert!(
                paged.pixels == alone(*page, filter),
                "{:?} parts from its own",
                page.at
            );
            assert_eq!(paged.taken, 1, "{:?} took nothing kept", page.at);
            assert_eq!(paged.cast_layers, 0, "{:?} drew its cast", page.at);
        }
    }

    #[test]
    fn a_shadow_cast_from_a_run_moved_into_another_placement_misses() {
        // A run whose alpha is a byte of 1 lands that byte where its
        // coverage is whole and nothing elsewhere, so two placements alike
        // in their whole pixels draw the same input; their copies, whose
        // edges part, are told apart by where the run lies alone. "I" at
        // 30.3 lies 242.4 eighths of a pixel in, short of 243, an odd eighth
        // its placement turns at: 30.33 lies in its class, 30.4 past it in
        // the next -- see `run_mask::PLACEMENT_STEPS`.
        as_skia_blits();
        let filter = TRANSLUCENT.filter;
        let at = |x: f32| Page {
            text: "I",
            font: SIXTY,
            fill: "rgba(200, 80, 30, 0.004)",
            at: (x, AT.1),
        };
        let (within, across) = (at(30.33), at(30.4));
        let opaque = |page: Page<'static>| -> Page<'static> {
            Page {
                fill: "rgb(200, 80, 30)",
                ..page
            }
        };
        assert!(
            alone(at(30.3), "none") == alone(across, "none"),
            "the control: the two placements' inputs part"
        );
        assert!(
            alone(opaque(at(30.3)), "none") != alone(opaque(across), "none"),
            "the control: the two placements' copies are alike"
        );
        let mut drawn = vec![at(30.3); 4];
        drawn.extend([across, within]);
        let drawn = pages(&drawn, filter, true);
        assert_eq!(drawn[3].taken, 1, "the control: nothing kept was taken");
        let (moved, inside) = (&drawn[4], &drawn[5]);
        assert!(moved.pixels == alone(across, filter), "moved parts");
        assert_eq!(moved.taken, 0, "taken across a placement");
        assert!(moved.cast_layers > 0, "the cast moved was not drawn");
        assert!(inside.pixels == alone(within, filter), "within parts");
        assert_eq!(inside.taken, 1, "nothing kept was taken within a class");
    }

    /// Whether a position `step` eighths of a pixel in is one Skia's
    /// placement of a glyph's mask turns at: an odd eighth or a half -- see
    /// `run_mask::turns_at`, which this restates for the test to choose
    /// positions by.
    fn turns(step: i64) -> bool {
        let within = step.rem_euclid(8);
        within == 4 || within % 2 == 1
    }

    /// Whether moving a glyph at `x` by `by` takes it past a step its
    /// placement turns at, and how near either place lies to one, in
    /// eighths.
    fn moved_across(x: f64, by: f64) -> (bool, f64) {
        let near = |x: f64| {
            let steps = x * 8.0;
            let below = steps.floor() as i64;
            let fraction = steps - steps.floor();
            [(below, fraction), (below + 1, 1.0 - fraction)]
                .into_iter()
                .filter(|(step, _)| turns(*step))
                .map(|(_, distance)| distance)
                .fold(1.0, f64::min)
        };
        let (from, to) =
            ((x * 8.0).floor() as i64, ((x + by) * 8.0).floor() as i64);
        ((from + 1..=to).any(turns), near(x).min(near(x + by)))
    }

    #[test]
    fn a_shadow_cast_from_a_run_whose_glyphs_part_in_placement_misses() {
        // "II" moved so that its first glyph stays in its placement class
        // and its second crosses into the next: a key that classed the
        // run's origin alone would take the result, the second glyph's
        // edges cast otherwise. The alpha a byte of 1, as in
        // `a_shadow_cast_from_a_run_moved_into_another_placement_misses`,
        // so that only where the run lies tells the two apart.
        use crate::context::run_mask::{RULE, Rule};
        let filter = TRANSLUCENT.filter;
        let advance = {
            FontLibrary::new()
                .use_paths(
                    Some(OSWALD),
                    ["tests/assets/fonts/Oswald/Oswald-VariableFont_wght.ttf"],
                )
                .expect("registers");
            let mut canvas = Canvas::new(10.0, 10.0);
            let ctx = canvas.context();
            assert!(ctx.set_font_css(&format!("60px {OSWALD}")));
            f64::from(ctx.measure_text("I", None).width)
        };
        let at = |x: f64| Page {
            text: "II",
            font: SIXTY,
            fill: "rgba(200, 80, 30, 0.004)",
            at: (x as f32, AT.1),
        };
        // Clear of every turning step by a fifth of an eighth, and drawing
        // the same input where it lands: the first such move found.
        let margin = 0.2;
        let (x, by) = (0..256)
            .flat_map(|i| {
                (1..16).map(move |j| {
                    (30.0 + f64::from(i) / 256.0, f64::from(j) / 64.0)
                })
            })
            .filter(|&(x, by)| {
                let (first, near_first) = moved_across(x, by);
                let (second, near_second) = moved_across(x + advance, by);
                !first && second && near_first > margin && near_second > margin
            })
            .find(|&(x, by)| alone(at(x), "none") == alone(at(x + by), "none"))
            .expect("a move that parts the glyphs and not the input");
        let moved = at(x + by);
        let mut drawn = vec![at(x); 4];
        drawn.push(moved);
        let paged = pages(&drawn, filter, true);
        assert_eq!(paged[3].taken, 1, "the control: nothing kept was taken");
        assert!(paged[4].pixels == alone(moved, filter), "moved parts");
        assert_eq!(paged[4].taken, 0, "taken with a glyph in another class");
        // The control: classed by its origin alone, the run is taken.
        RULE.set(Rule::OriginOnly);
        let origin_only = pages(&drawn, filter, true);
        RULE.set(Rule::Exact);
        assert_eq!(
            origin_only[4].taken, 1,
            "the control: a key of the origin alone misses too"
        );
    }

    #[test]
    fn a_shadow_cast_from_a_run_on_a_placement_step_is_taken_only_where_drawn()
    {
        // A run drawn from whole pixels can lay a glyph on a step its
        // placement turns at, which no class can be trusted for: its cast is
        // filed by exactly where the run lies -- see
        // `run_mask::CastLies::Exactly` -- and taken on a page drawn alike,
        // but not moved by a whole pixel, which the classes would allow.
        use crate::context::run_mask::CAST_EXACTLY;
        let filter = TRANSLUCENT.filter;
        let at = |x: f32| Page {
            at: (x, 86.0),
            ..HAMBURGEFONSTIV
        };
        let x = (20..60)
            .map(|x| x as f32)
            .find(|&x| {
                let before = CAST_EXACTLY.get();
                pages(&[at(x); 2], filter, false);
                CAST_EXACTLY.get() > before
            })
            .expect("a whole-pixel origin that lays a glyph on a step");
        let moved = at(x + 1.0);
        let mut drawn = vec![at(x); 5];
        drawn.push(moved);
        let drawn = pages(&drawn, filter, true);
        let reference = alone(at(x), filter);
        for page in &drawn[3..5] {
            assert!(page.pixels == reference, "a page alike parts");
            assert_eq!(page.taken, 1, "nothing kept was taken");
            assert_eq!(page.cast_layers, 0, "the cast was drawn");
        }
        let last = &drawn[5];
        assert!(last.pixels == alone(moved, filter), "moved parts");
        assert_eq!(last.taken, 0, "taken moved off where it was drawn");
        assert!(last.cast_layers > 0, "the cast moved was not drawn");
    }

    /// The alpha of each of `pixels`, premultiplied RGBA.
    fn alphas(pixels: &[u8]) -> Vec<u8> {
        pixels
            .as_chunks::<4>()
            .0
            .iter()
            .map(|pixel| pixel[3])
            .collect()
    }

    /// The first of `others` -- each a page and its copy drawn opaque --
    /// whose input is `first`'s to the byte while its copy's alphas, all a
    /// cast is read for, are not: a page only the cast's key tells apart
    /// from `first`. `None` where no page is.
    fn alike_but_cast_otherwise<'a>(
        first: (Page<'a>, Page<'a>),
        others: impl IntoIterator<Item = (Page<'a>, Page<'a>)>,
    ) -> Option<Page<'a>> {
        let input = alone(first.0, "none");
        let cast = alphas(&alone(first.1, "none"));
        others
            .into_iter()
            .find(|(page, opaque)| {
                alone(*page, "none") == input
                    && alphas(&alone(*opaque, "none")) != cast
            })
            .map(|(page, _)| page)
    }

    /// Whether `other`, drawn twice after `first` four times through
    /// `filter`, takes a kept result on its second draw -- the first that
    /// can be filed by its run -- and whether it lands what it does drawn
    /// alone.
    fn taken_after(first: Page, other: Page, filter: &str) -> (bool, bool) {
        let mut drawn = vec![first; 4];
        drawn.extend([other; 2]);
        let drawn = pages(&drawn, filter, true);
        assert_eq!(drawn[3].taken, 1, "the control: nothing kept was taken");
        let last = &drawn[5];
        (last.taken > 0, last.pixels == alone(other, filter))
    }

    #[test]
    fn a_shadow_cast_from_a_run_on_a_placement_step_misses_moved_within_a_pixel()
     {
        // "I" laid on one step its placement turns at and moved to another
        // in the same pixel: both filed by exactly where the run lies --
        // see `run_mask::CastLies::Exactly` -- the stem's whole pixels, all
        // an alpha of a byte of 1 lands, as they were and its edges moved.
        // Only where the run lies tells the two casts apart.
        use crate::context::run_mask::{CAST_EXACTLY, PLACEMENT_STEPS};
        as_skia_blits();
        let at = |x: f32, fill| Page {
            text: "I",
            font: SIXTY,
            fill,
            at: (x, AT.1),
        };
        let (translucent, opaque) =
            ("rgba(200, 80, 30, 0.004)", "rgb(200, 80, 30)");
        let steps = (0..PLACEMENT_STEPS as u8)
            .map(|step| 20.0 + f32::from(step) / PLACEMENT_STEPS as f32)
            .filter(|&x| {
                let before = CAST_EXACTLY.get();
                pages(&[at(x, translucent); 2], TRANSLUCENT.filter, false);
                CAST_EXACTLY.get() > before
            })
            .collect::<Vec<_>>();
        // The chain's input is compared to the byte, its size with it, so
        // only a step whose region is the first's tells anything of the key.
        let region = |x: f32| {
            pages(&[at(x, translucent)], TRANSLUCENT.filter, false)[0].region
        };
        let (x, moved) = steps
            .iter()
            .find_map(|&x| {
                let others = steps
                    .iter()
                    .filter(|&&moved| moved != x && region(moved) == region(x))
                    .map(|&moved| (at(moved, translucent), at(moved, opaque)));
                alike_but_cast_otherwise((at(x, translucent), at(x, opaque)), others)
                    .map(|moved| (x, moved))
            })
            .expect("two steps alike in their input and region and not in their cast");
        let (taken, alike) =
            taken_after(at(x, translucent), moved, TRANSLUCENT.filter);
        assert!(alike, "moved to {} parts from its own", moved.at.0);
        assert!(
            !taken,
            "moved to {} took the result where it was",
            moved.at.0
        );
    }

    #[test]
    fn a_shadow_cast_from_another_run_with_the_same_input_misses() {
        // "I" in Oswald at another weight: another run, its stem's edges
        // elsewhere, its whole pixels -- all an alpha of a byte of 1 lands
        // -- the same. Only the run in the cast's key tells them apart.
        as_skia_blits();
        fn at<'a>(font: &'a str, fill: &'a str) -> Page<'a> {
            Page {
                text: "I",
                font,
                fill,
                at: AT,
            }
        }
        let (translucent, opaque) =
            ("rgba(200, 80, 30, 0.004)", "rgb(200, 80, 30)");
        let weights: Vec<String> = (41..=60)
            .map(|weight| format!("{} 60px {OSWALD}", weight * 10))
            .collect();
        let other = alike_but_cast_otherwise(
            (at(SIXTY, translucent), at(SIXTY, opaque)),
            weights
                .iter()
                .map(|font| (at(font, translucent), at(font, opaque))),
        )
        .expect("a weight alike in its input and not in its cast");
        let (taken, alike) =
            taken_after(at(SIXTY, translucent), other, TRANSLUCENT.filter);
        assert!(alike, "{} parts from its own", other.font);
        assert!(!taken, "{} took the other run's result", other.font);
    }

    #[test]
    fn a_shadow_cast_in_another_colour_with_the_same_input_misses() {
        // Two greys that an alpha of a byte of 1 lands alike, premultiplied,
        // but whose masks Skia blends by their luminance otherwise -- see
        // `CoverageRun` -- so that their copies cover otherwise. Only the
        // paint in the cast's key tells them apart.
        as_skia_blits();
        let fills: Vec<(String, String)> = (128..=255)
            .step_by(4)
            .map(|level| {
                (
                    format!("rgba({level}, {level}, {level}, 0.004)"),
                    format!("rgb({level}, {level}, {level})"),
                )
            })
            .collect();
        let at = |fill| Page {
            text: "Hamburgefonstiv",
            font: SIXTY,
            fill,
            at: AT,
        };
        let mut pairs =
            fills.iter().map(|(fill, opaque)| (at(fill), at(opaque)));
        let first = pairs.next().expect("a grey");
        let other = alike_but_cast_otherwise(first, pairs)
            .expect("a grey alike in its input and not in its cast");
        let first = first.0;
        let (taken, alike) = taken_after(first, other, TRANSLUCENT.filter);
        assert!(alike, "{} parts from its own", other.fill);
        assert!(!taken, "{} took the other colour's result", other.fill);
    }

    #[test]
    fn a_shadow_cast_at_another_alpha_with_the_same_input_misses() {
        // Text so small that no pixel is covered by half lands nothing at
        // an alpha of a byte of 1 or 2 -- `(a * (c + 1)) >> 8` is 0 for a
        // coverage `c` under 127 -- so the two draw the same input from the
        // same cast. Their shadows, rounded once from the coverage times
        // the alpha, are a level apart where the coverage is near half:
        // only the alpha in the cast's key tells them apart.
        as_skia_blits();
        let filter = "drop-shadow(3px 2px 1px black)";
        let at = |fill| Page {
            text: "iiiiiiii",
            font: "3px ShadowRunOswald",
            fill,
            at: AT,
        };
        // Black, whose copy is drawn in the same colour at either alpha: a
        // colour drawn at an alpha this small comes back from its byte
        // otherwise, and the paint in the key would part them first.
        let (one, two) =
            (at("rgba(0, 0, 0, 0.004)"), at("rgba(0, 0, 0, 0.008)"));
        assert!(
            alone(one, "none") == alone(two, "none"),
            "the control: the two alphas' inputs part"
        );
        assert!(
            alone(one, filter) != alone(two, filter),
            "the control: the two alphas' shadows are alike"
        );
        let (taken, alike) = taken_after(one, two, filter);
        assert!(alike, "parts from its own");
        assert!(!taken, "took the other alpha's result");
    }

    #[test]
    fn a_shadow_cast_from_another_run_or_at_another_alpha_misses() {
        let filter = TRANSLUCENT.filter;
        let others = [
            Page {
                text: "Hamburgefonstiw",
                ..HAMBURGEFONSTIV
            },
            Page {
                fill: "rgba(200, 80, 30, 0.5)",
                ..HAMBURGEFONSTIV
            },
            Page {
                fill: "rgba(20, 60, 200, 0.6)",
                ..HAMBURGEFONSTIV
            },
        ];
        let mut drawn = vec![HAMBURGEFONSTIV; 4];
        drawn.extend(others);
        let drawn = pages(&drawn, filter, true);
        assert_eq!(drawn[3].taken, 1, "the control: nothing kept was taken");
        for (page, paged) in others.iter().zip(&drawn[4..]) {
            assert!(
                paged.pixels == alone(*page, filter),
                "{} in {} parts from its own",
                page.text,
                page.fill
            );
            assert_eq!(paged.taken, 0, "{} in {} took", page.text, page.fill);
        }
    }
}

#[cfg(test)]
mod colour_glyph_coverage_tests {
    //! A run holding glyphs the platform draws in their own colours -- from
    //! an `sbix`, `CBDT` or `SVG ` table -- is not one flat colour, and the
    //! coverage route, which paints a blur of the run's coverage in the
    //! run's colour -- see [`CoverageRun`] -- would draw it in that colour.

    use super::COVERAGE_RUNS;
    use crate::prelude::*;

    /// Skia's own bitmap and SVG colour fonts, each with the text Skia
    /// draws in it (`tools/fonts/FontToolUtils.cpp`, `EmojiSample`).
    const FACES: [(&str, &str, &str); 3] = [
        ("ColourGlyphsSbix", "sbix.ttf", "\u{1F600} \u{2662}"),
        ("ColourGlyphsCbdt", "cbdt.ttf", "\u{1F600} \u{2662}"),
        ("ColourGlyphsSvg", "SampleSVG.ttf", "abcdefghij"),
    ];

    /// The family Oswald is registered under, for a run of outlines.
    const OSWALD: &str = "ColourGlyphsOswald";

    /// Two fills, far apart in every channel, for a run whose pixels must
    /// not depend on which one it is drawn in.
    const FILLS: [&str; 2] = ["rgb(230, 20, 10)", "rgb(10, 60, 240)"];

    /// `text` in `family` at 48px, filled with `fill` through `filter` on a
    /// CPU canvas, as premultiplied pixels, and how many draws took the
    /// coverage route.
    fn drawn(
        family: &str,
        text: &str,
        fill: &str,
        filter: &str,
    ) -> (Vec<u8>, usize) {
        drawn_times(family, text, fill, filter, 1)
    }

    /// [`drawn`], the run filled `times` times over itself: from the second,
    /// it is the run kept for drawing -- see `Typesetter::laid_out`.
    fn drawn_times(
        family: &str,
        text: &str,
        fill: &str,
        filter: &str,
        times: usize,
    ) -> (Vec<u8>, usize) {
        let library = FontLibrary::new();
        registered();
        library
            .use_paths(
                Some(OSWALD),
                ["tests/assets/fonts/Oswald/Oswald-VariableFont_wght.ttf"],
            )
            .expect("registers");
        let mut canvas = Canvas::with_options(
            480.0,
            80.0,
            CanvasOptions {
                gpu: false,
                ..CanvasOptions::default()
            },
        )
        .expect("a canvas");
        let ctx = canvas.context();
        assert!(ctx.set_font_css(&format!("48px {family}")));
        ctx.set_fill_style_css(fill).expect("a colour");
        ctx.set_filter_css(filter).expect("a filter");
        let before = COVERAGE_RUNS.with(|runs| runs.get());
        (0..times).for_each(|_| ctx.fill_text(text, 10.0, 58.0, None));
        let pixels = canvas
            .to_image_data(
                None,
                &ImageDataExport {
                    pixels: PixelExportOptions {
                        premultiplied: true,
                        ..PixelExportOptions::default()
                    },
                    ..ImageDataExport::default()
                },
            )
            .expect("pixels")
            .pixels()
            .to_vec();
        (pixels, COVERAGE_RUNS.with(|runs| runs.get()) - before)
    }

    /// The faces of [`FACES`] this platform can load, registered under
    /// their families. CoreText refuses a face with no outlines, as
    /// `cbdt.ttf` is.
    fn registered() -> Vec<&'static str> {
        let library = FontLibrary::new();
        FACES
            .iter()
            .filter(|(name, file, _)| {
                library
                    .use_paths(
                        Some(name),
                        [format!("tests/assets/fonts/bitmap-colour/{file}")],
                    )
                    .is_ok()
            })
            .map(|(name, _, _)| *name)
            .collect()
    }

    /// Whether `family` draws `text` in its own colours here: inked, and
    /// the same bytes in either fill.
    fn in_own_colours(family: &str, text: &str) -> bool {
        if !registered().contains(&family) {
            return false;
        }
        let [(a, _), (b, _)] =
            FILLS.map(|fill| drawn(family, text, fill, "none"));
        a.chunks(4).any(|p| p[3] != 0) && a == b
    }

    #[test]
    fn a_platform_colour_glyph_keeps_its_colours_under_a_blur() {
        let coloured: Vec<&str> = FACES
            .iter()
            .filter(|(family, _, text)| in_own_colours(family, text))
            .map(|(family, _, _)| *family)
            .collect();
        eprintln!("drawn in their own colours here: {coloured:?}");
        assert!(!coloured.is_empty(), "no colour face to test");
        for (family, _, text) in FACES {
            if !coloured.contains(&family) {
                continue;
            }
            for filter in ["blur(0.5px)", "blur(2px)"] {
                let [(a, routed), (b, _)] =
                    FILLS.map(|fill| drawn(family, text, fill, filter));
                assert_eq!(routed, 0, "{family} under {filter} took the route");
                assert!(
                    a == b,
                    "{family} under {filter} took its colour from the fill"
                );
            }
        }
    }

    #[test]
    fn a_kept_run_of_colour_glyphs_keeps_their_colours_under_a_blur() {
        // Drawn again, the run is the one kept for drawing, which answers
        // for itself.
        let Some((family, _, text)) = FACES
            .iter()
            .find(|(family, _, text)| in_own_colours(family, text))
        else {
            panic!("no colour face to test");
        };
        let [(a, routed), (b, _)] =
            FILLS.map(|fill| drawn_times(family, text, fill, "blur(2px)", 3));
        assert_eq!(routed, 0, "a kept run took the route");
        assert!(a == b, "a kept run took its colour from the fill");
    }

    #[test]
    fn a_run_of_outlines_beside_colour_glyphs_keeps_their_colours() {
        // One draw, its outlines from Oswald and its colour glyph by
        // fallback: the colour glyph's cell must not take the fill.
        let Some((family, _, _)) = FACES
            .iter()
            .find(|(family, _, text)| in_own_colours(family, text))
        else {
            panic!("no colour face to test");
        };
        let fonts = format!("{OSWALD}, {family}");
        let text = "H            \u{1F600}";
        let [(a, routed), (b, _)] =
            FILLS.map(|fill| drawn(&fonts, text, fill, "blur(2px)"));
        assert_eq!(routed, 0, "the mixed run took the route");
        // Past the H, which ends before x = 45 at 48px from x = 10, and its
        // blur's tail: where the colour glyph lands.
        const PAST_THE_H: usize = 80;
        let right = |pixels: &[u8]| -> Vec<u8> {
            pixels
                .chunks(480 * 4)
                .flat_map(|row| row[PAST_THE_H * 4..].to_vec())
                .collect()
        };
        assert!(right(&a).chunks(4).any(|p| p[3] != 0), "nothing drawn");
        assert!(right(&a) == right(&b), "the colour glyph took the fill");
    }

    #[test]
    fn a_run_of_outlines_still_takes_the_route() {
        // The control: the route stays for a run in one flat colour, whose
        // blur takes the fill.
        let [(a, routed), (b, _)] = FILLS
            .map(|fill| drawn_times(OSWALD, "Hamburg", fill, "blur(2px)", 3));
        assert_eq!(routed, 3, "the run of outlines left the route");
        assert!(a != b);
    }
}

#[cfg(test)]
mod pipeline_tests {
    //! [`pipeline_of`] held to the pipeline Skia's raster pipeline draws each
    //! paint on, read from the bytes it stores (#800).
    use super::{Pipeline, pipeline_of};
    use crate::{
        color::RgbaLinear,
        geometry::Point,
        shader::{
            GradientColorSpace, GradientInterpolation, GradientStop, Shader,
        },
        started,
    };
    use skia_safe::{
        AlphaType, BlendMode, BlurStyle, Color, ColorSpace, ColorType,
        ImageInfo, MaskFilter, Paint, color_filters, image_filters,
    };

    const SIDE: i32 = 64;

    /// How near a level, or a half level, a float lies for the distance to
    /// be arithmetic error rather than value: well under the 1/255 apart two
    /// byte sources' composites lie, and over the error of a float worked
    /// out in levels rather than in Skia's units. A composite this near a
    /// half level is one a store may round either way.
    const AT_A_TIE: f32 = 1e-3;

    /// The identity colour matrix, row-major.
    const IDENTITY: [f32; 20] = [
        1.0, 0.0, 0.0, 0.0, 0.0, //
        0.0, 1.0, 0.0, 0.0, 0.0, //
        0.0, 0.0, 1.0, 0.0, 0.0, //
        0.0, 0.0, 0.0, 1.0, 0.0,
    ];

    fn stop(
        position: f32,
        (r, g, b): (u8, u8, u8),
        alpha: f32,
    ) -> GradientStop {
        GradientStop {
            position,
            color: RgbaLinear::from_srgb8(r, g, b, alpha),
        }
    }

    /// A translucent ramp across the raster, eight-bit where interpolated in
    /// the page's space.
    fn ramp(space: GradientColorSpace) -> Shader {
        Shader::linear_gradient(
            Point::new(0.0, 0.0),
            Point::new(SIDE as f32, 0.0),
            &[
                stop(0.0, (200, 30, 90), 0.7),
                stop(0.5, (0, 255, 0), 0.8),
                stop(1.0, (30, 120, 200), 0.45),
            ],
            GradientInterpolation::new(space),
        )
        .expect("a ramp")
    }

    /// The pixels of a raster of `color_type` in sRGB, premultiplied, in
    /// levels: `paint` drawn over its whole, over an opaque vertical ramp of
    /// bytes where `under`, whose share of a blend is seldom a whole level.
    fn drawn(
        color_type: ColorType,
        under: bool,
        paint: Option<&Paint>,
    ) -> Vec<f32> {
        let info = ImageInfo::new(
            (SIDE, SIDE),
            color_type,
            AlphaType::Premul,
            ColorSpace::new_srgb(),
        );
        let mut surface = started::raster(&info, None, None).expect("a raster");
        let canvas = surface.canvas();
        if under {
            let mut backdrop = Paint::default();
            backdrop.set_shader(
                Shader::linear_gradient(
                    Point::new(0.0, 0.0),
                    Point::new(0.0, SIDE as f32),
                    &[
                        stop(0.0, (37, 101, 173), 1.0),
                        stop(1.0, (219, 7, 66), 1.0),
                    ],
                    GradientInterpolation::default(),
                )
                .expect("a backdrop")
                .inner,
            );
            canvas.draw_paint(&backdrop);
        }
        if let Some(paint) = paint {
            canvas.draw_paint(paint);
        }
        let row = info.min_row_bytes();
        let mut bytes = vec![0u8; info.compute_byte_size(row)];
        assert!(surface.read_pixels(&info, &mut bytes, row, (0, 0)));
        match color_type {
            ColorType::RGBAF32 => bytes
                .as_chunks::<4>()
                .0
                .iter()
                .map(|&word| f32::from_ne_bytes(word) * 255.0)
                .collect(),
            _ => bytes.into_iter().map(f32::from).collect(),
        }
    }

    /// The pipeline the CPU draws `paint` on, from what it stores over the
    /// backdrop: the unrounded source-over of what it draws over transparent
    /// into floats, rounded once, where it blends in floats, and that of
    /// what it draws over transparent into bytes where it blends those.
    /// Panics where both or neither account for every byte, which a scene
    /// that tells the two apart does not, or where the dither of a dithered
    /// paint moves nothing.
    ///
    /// A dithered paint is read differently: Skia does not dither a float
    /// raster, so no source here composites to what it stores, and its
    /// dither stage is one the byte pipeline does not have, so a store the
    /// dither moves is a store of the float pipeline.
    fn cpu_pipeline(name: &str, paint: &Paint) -> Pipeline {
        let page = drawn(ColorType::RGBA8888, true, Some(paint));
        if paint.is_dither() {
            let mut plain = paint.clone();
            plain.set_dither(false);
            return match page == drawn(ColorType::RGBA8888, true, Some(&plain))
            {
                true => panic!("{name}: the dither moved nothing"),
                false => Pipeline::Floats,
            };
        }
        let under = drawn(ColorType::RGBA8888, true, None);
        // The float pipeline clamps a colour into the gamut before an
        // eight-bit store. A composite this close to a half level is one
        // the CPU's own float arithmetic, in other units and order, may round
        // either way; a byte source never lands within it.
        let composited = |source: &[f32]| {
            (0..page.len()).all(|i| {
                let alpha = source[(i & !3) + 3].clamp(0.0, 255.0);
                let colour = source[i].clamp(0.0, alpha);
                let exact = colour + under[i] * (255.0 - alpha) / 255.0;
                (exact - page[i]).abs() < 0.5 + AT_A_TIE
            })
        };
        let float_source = drawn(ColorType::RGBAF32, false, Some(paint));
        let byte_source = drawn(ColorType::RGBA8888, false, Some(paint));
        // A float raster that stores the byte raster's levels was handed a
        // source rounded before it -- an image filter's own eight-bit layer --
        // so the draw is in bytes whatever the raster under it.
        if float_source
            .iter()
            .all(|level| (level - level.round()).abs() < AT_A_TIE)
        {
            return Pipeline::Bytes;
        }
        match (composited(&float_source), composited(&byte_source)) {
            (true, false) => Pipeline::Floats,
            (false, true) => Pipeline::Bytes,
            both => panic!("{name}: floats and bytes fit {both:?}"),
        }
    }

    /// Each paint lands the pipeline `pipeline_of` says it does, on an
    /// eight-bit sRGB page: a ramp in the page's space in bytes, at an alpha
    /// or through a blend mode's colour filter as well; one in Oklab, a
    /// two-point conical one, one through a colour matrix -- as a colour
    /// filter, or as an image filter Skia moves into the paint -- or one
    /// dithered, in floats; and one whose image filter Skia runs in a layer
    /// of its own, which an image filter beside a mask filter is, in bytes.
    #[test]
    fn the_pipeline_a_draw_is_said_to_take_is_the_one_the_cpu_takes() {
        let page = ColorSpace::new_srgb();
        let srgb = ramp(GradientColorSpace::Srgb);
        let oklab = ramp(GradientColorSpace::Oklab);
        let conical = Shader::two_point_conical_gradient(
            Point::new(20.0, 30.0),
            25.0,
            Point::new(40.0, 34.0),
            10.0,
            &[stop(0.0, (200, 30, 90), 0.7), stop(1.0, (0, 255, 0), 0.9)],
            GradientInterpolation::default(),
        )
        .expect("a conical gradient");
        let identity = || {
            image_filters::color_filter(
                color_filters::matrix_row_major(&IDENTITY, None),
                None,
                None,
            )
            .expect("an identity filter")
        };
        fn with<'a>(
            shader: &'a Shader,
            change: &dyn Fn(&mut Paint),
        ) -> (&'a Shader, Paint) {
            let mut paint = Paint::default();
            paint.set_shader(shader.inner.clone());
            change(&mut paint);
            (shader, paint)
        }
        let mut scaled = IDENTITY;
        scaled[0] = 0.9;
        let cases: Vec<(&str, (&Shader, Paint), Pipeline)> = vec![
            ("sRGB ramp", with(&srgb, &|_| {}), Pipeline::Bytes),
            (
                "sRGB ramp at 0.6",
                with(&srgb, &|paint| {
                    paint.set_alpha_f(0.6);
                }),
                Pipeline::Bytes,
            ),
            (
                "sRGB ramp, modulated",
                with(&srgb, &|paint| {
                    paint.set_color_filter(color_filters::blend(
                        Color::from_rgb(30, 200, 90),
                        BlendMode::Modulate,
                    ));
                }),
                Pipeline::Bytes,
            ),
            ("Oklab ramp", with(&oklab, &|_| {}), Pipeline::Floats),
            ("conical", with(&conical, &|_| {}), Pipeline::Floats),
            (
                "sRGB ramp, colour matrix",
                with(&srgb, &|paint| {
                    paint.set_color_filter(color_filters::matrix_row_major(
                        &scaled, None,
                    ));
                }),
                Pipeline::Floats,
            ),
            (
                "sRGB ramp, identity image filter",
                with(&srgb, &|paint| {
                    paint.set_image_filter(identity());
                }),
                Pipeline::Floats,
            ),
            (
                "sRGB ramp, dithered",
                with(&srgb, &|paint| {
                    paint.set_dither(true);
                }),
                Pipeline::Floats,
            ),
            (
                "Oklab ramp, blurred",
                with(&oklab, &|paint| {
                    paint.set_image_filter(image_filters::blur(
                        (0.5, 0.5),
                        None,
                        None,
                        None,
                    ));
                }),
                Pipeline::Bytes,
            ),
            (
                "Oklab ramp, identity image filter and a mask filter",
                with(&oklab, &|paint| {
                    paint.set_image_filter(identity()).set_mask_filter(
                        MaskFilter::blur(BlurStyle::Normal, 0.5, false),
                    );
                }),
                Pipeline::Bytes,
            ),
        ];
        for (name, (shader, paint), expected) in cases {
            assert_eq!(cpu_pipeline(name, &paint), expected, "{name}: the CPU");
            assert_eq!(
                pipeline_of(
                    shader.gradient.as_deref(),
                    &page,
                    &paint,
                    BlendMode::SrcOver
                ),
                expected,
                "{name}: pipeline_of"
            );
        }
    }
}
