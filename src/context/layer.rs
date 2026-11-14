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
        Arc, OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
};

use parking_lot::Mutex;
use skia_safe::{
    AlphaType, BlendMode, BlurStyle, Canvas as SkCanvas, ClipOp, Color4f,
    ColorFilter, ColorSpace, ColorType, Contains, Data, FilterMode, IPoint,
    IRect, ISize, Image, ImageFilter, ImageInfo, MaskFilter, Matrix,
    MipmapMode, Paint, PaintCap, PaintJoin, PaintStyle, Path, PathDirection,
    Picture, PictureRecorder, Point, Rect, RoundOut, RuntimeEffect,
    SamplingOptions, Shader, Surface, TileMode, Vector, canvas::SaveLayerRec,
    color_filters, image_filters, shaders,
};

use crate::{
    cache_map::{GOLDEN_RATIO_64, KeptSets},
    context::{
        blur_mask,
        chain::{self, Depth, Step},
        chain_cache, coverage_of, exact_blur,
        exact_gradient::GradientSpec,
        faint::{Faint, composited},
        kept::CoverageInput,
        page::{ExportOptions, Page},
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
        typography::{DrawnText, RunPaint, WholeRuns},
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
    /// A run of draws dyed by gradients, drawn on a GPU canvas in the bytes
    /// the CPU draws them in -- see [`GradientDraw`].
    Gradient(Arc<GradientDraw>),
}

impl Layer {
    /// Draws this entry onto `canvas` for `target`, under whatever transform
    /// and clip the canvas already carries.
    pub fn playback(&self, canvas: &SkCanvas, target: Target) {
        match self {
            Layer::Picture(picture) => picture.playback(canvas),
            Layer::Blur(op) => op.playback(canvas, target),
            Layer::Group(group) => group.playback(canvas, target),
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
            Layer::Pattern(tile) => Some(tile.reach()),
            // The pixels' recording, as for a pair, recorded to answer.
            Layer::Deferred(deferred) => Some(
                deferred.picture().map_or_else(Rect::new_empty, |picture| {
                    picture.cull_rect()
                }),
            ),
            Layer::ReplayedText(text) => Some(text.reach()),
            Layer::Gradient(op) => Some(op.reach()),
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
            Layer::Pattern(tile) => tile.plain.approximate_bytes_used(),
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

/// The bytes the pictures of one generation of a page's recording hold --
/// see [`PageRecorder::recorded_bytes`](super::page::PageRecorder::recorded_bytes)
/// -- added to as each entry is filed and as each picture recorded when first
/// played is recorded. Shared with those pictures, which an export can
/// record on another thread.
///
/// `Relaxed` throughout: the count is a figure told to the JavaScript
/// engine, and nothing is published through it -- no thread reads a picture
/// because it read the count. Every addition to one atomic falls in a single
/// order, so none is lost; a report made while another thread records may
/// miss that one picture, which the next report counts.
#[derive(Clone, Debug, Default)]
pub(crate) struct RecordedCount(Arc<AtomicUsize>);

impl RecordedCount {
    pub(crate) fn add(&self, bytes: usize) {
        self.0.fetch_add(bytes, Ordering::Relaxed);
    }

    pub(crate) fn bytes(&self) -> usize {
        self.0.load(Ordering::Relaxed)
    }
}

/// A picture recorded the first time a target asks for it, which then adds
/// its bytes to the count of the page that filed it -- see
/// [`Layer::count_into`]. The thread that records it adds them after the
/// picture is in place, so a count never runs ahead of the pictures a walk
/// of the page finds.
#[derive(Debug, Default)]
pub struct LazyPicture {
    picture: OnceLock<Option<Picture>>,
    counted_by: OnceLock<RecordedCount>,
}

impl Clone for LazyPicture {
    /// The picture as it stands, counted by the page that files the clone
    /// rather than by the one that filed this.
    fn clone(&self) -> Self {
        Self {
            picture: self.picture.clone(),
            counted_by: OnceLock::new(),
        }
    }
}

impl From<Option<Picture>> for LazyPicture {
    /// `picture`, recorded already.
    fn from(picture: Option<Picture>) -> Self {
        Self {
            picture: OnceLock::from(picture),
            counted_by: OnceLock::new(),
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
    /// yet, its bytes then added to the count it was filed into.
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
        if made > 0
            && let Some(count) = self.counted_by.get()
        {
            count.add(made);
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

    /// Adds the picture's bytes to `count` when it is recorded. Kept for the
    /// first page that files it, the only one that holds it.
    fn count_into(&self, count: &RecordedCount) {
        let _ = self.counted_by.set(count.clone());
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
    /// A mutant of the batch [`play_entries`] makes of consecutive
    /// [`ReplayedText`] runs on a GPU canvas, set on this thread: each run
    /// read back on its own.
    pub(crate) static BACKDROP_EACH_RUN: Cell<bool> = const { Cell::new(false) };
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
fn byte_alpha(alpha: f32) -> f32 {
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
    /// 637, 456 of them at an alpha under 32 levels: there the glyph's
    /// interior stores the exact product and its edges take the contrast of
    /// the next bucket.
    ///
    /// The colour is handed over as it is, as [`Self::verified_paint`] hands
    /// its own over on such a page: Skia lands an sRGB paint colour on an
    /// sRGB page unconverted -- see [`unconverted`]. `None` on any other
    /// page, and for a paint with a colour filter of its own.
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
        let [r, g, b] = [0, 1, 2].map(|i| {
            let product = (f64::from(placed.rgb[i]) * stored).round();
            let plain = f64::from(self.rgb[i]) * levels;
            (nearest_storing(plain, product, stored) / levels) as f32
        });
        let mut paint = paint.clone();
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
#[derive(Debug)]
pub struct TileSource {
    page: Page,
    /// What the page is rasterized with: its own colour space, format and
    /// glyph settings. The density is set for each raster.
    options: ExportOptions,
    /// The raster made last on the CPU, and the density it was made at.
    raster: Mutex<Option<(u32, Image)>>,
}

impl TileSource {
    /// `page`, rasterized with `options` at whatever density a draw asks.
    pub fn new(page: Page, options: ExportOptions) -> Self {
        Self {
            page,
            options,
            raster: Mutex::new(None),
        }
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
        *held = Some((density, image.clone()));
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
    /// Where the thread that filed it counts the recordings
    /// [`Self::recorded`] makes -- see [`REPLAYED_WATCH`].
    #[cfg(test)]
    pub watch: Option<Arc<std::sync::atomic::AtomicUsize>>,
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
                    canvas.save();
                    canvas.translate(shadow.offset);
                    shadow.replay.draw(canvas, &self.matrix, &shadow.paint);
                    canvas.restore();
                }
                if let Some(replay) = &self.replay {
                    canvas.save();
                    replay.draw(canvas, &self.matrix, &self.paint);
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
    pub(crate) fn recorded(&self) -> Option<&Picture> {
        self.recording
            .get_or_record(|| self.record(&self.paint, self.shadow.as_ref()))
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
                self.record(&self.paint, self.shadow.as_ref())
            });
        };
        document.picture.get_or_record(|| {
            self.record(&document.paint, document.shadow.as_ref())
        })
    }

    /// The run recorded in `paint`, `shadow` beneath it, for
    /// [`Self::recorded`].
    fn record(
        &self,
        paint: &Paint,
        shadow: Option<&ReplayedShadow>,
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
                canvas.save();
                canvas.translate(shadow.offset);
                shadow.replay.draw(canvas, &self.matrix, &shadow.paint);
                reach_the_clip(canvas);
                canvas.restore();
            }
            canvas.save();
            replay.draw(canvas, &self.matrix, paint);
            reach_the_clip(canvas);
            canvas.restore();
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
pub(crate) fn rasterized_onto(
    canvas: &SkCanvas,
    recorded: &Picture,
    inked: Option<Rect>,
    format: &Format,
    through: &Paint,
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
    let Some(mut surface) =
        started::raster(&format.image_info(area.size()), None, None)
    else {
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
        through,
    );
    canvas.restore();
}

thread_local! {
    /// How many layers this thread is playing entries inside, opened by
    /// [`Group::playback_within`] -- see [`InLayer`].
    static IN_LAYERS: Cell<u32> = const { Cell::new(0) };
}

/// Counts a layer [`Group::playback_within`] opens on a canvas around the
/// entries it plays, for as long as it lives, so that an entry can tell
/// whether the pixels under it are the canvas's own: `SkCanvas::readPixels`
/// reads the canvas's root device, never an open layer.
struct InLayer;

impl InLayer {
    fn enter() -> Self {
        IN_LAYERS.set(IN_LAYERS.get() + 1);
        InLayer
    }

    /// Whether entries are being played inside a layer on this thread.
    fn open() -> bool {
        IN_LAYERS.get() > 0
    }
}

impl Drop for InLayer {
    fn drop(&mut self) {
        IN_LAYERS.set(IN_LAYERS.get().saturating_sub(1));
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
            reach: recorded.cull_rect(),
            beyond,
            draw: &draw,
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
    /// -- whose texels are the page's device pixels. Played under any
    /// other mapping than onto whole pixels -- a canvas drawn into another
    /// under a scale, say -- the image would be resampled, so the draw is
    /// made on the CPU instead: over the GPU's pixels, `beyond` saying as
    /// for [`OnGpu::Raster`] what it composites over, or, inside a layer,
    /// whose pixels cannot be read, over transparent and composited through
    /// `blend` -- `over`, the draw recorded with `SrcOver`, where `blend` is
    /// another.
    Effect {
        picture: Picture,
        over: Option<Picture>,
        blend: BlendMode,
        beyond: bool,
    },
    /// The draw made on the CPU, its layers -- its shadow, then itself --
    /// each recorded composited with `SrcOver` and composited in turn onto
    /// the GPU's pixels through `blend`: over what it inks, or over the
    /// clip's bounds whole where `beyond` says the blend changes what lies
    /// beyond the draw.
    Raster {
        layers: Vec<Picture>,
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
            OnGpu::Raster { layers, .. } => {
                layers.iter().map(Picture::approximate_bytes_used).sum()
            }
            OnGpu::Path(draw) => draw.path.lock().approximate_bytes_used(),
            OnGpu::Alone | OnGpu::Native => 0,
        }
    }
}

/// A path filled or stroked with a gradient, as a GPU canvas draws it in the
/// CPU's bytes: `path` under `matrix`, through `paint` with the effect that
/// computes them as its shader -- see
/// [`GradientSpec::exact`](super::exact_gradient::GradientSpec::exact).
#[derive(Debug)]
pub struct GradientPath {
    /// The path, behind a lock: a `Path` can be sent between threads but not
    /// shared, and the page that holds this is shared.
    pub path: Mutex<Path>,
    /// What the draw reaches, in the recording's device pixels: the path's
    /// bounds under `matrix`, widened as `context::drawn_bounds` widens them.
    pub bounds: Rect,
    /// The page's transform the path was drawn under.
    pub matrix: Matrix,
    /// The draw's paint with no shader and an alpha of 1, the effect
    /// carrying the draw's alpha; aliased where `whole_pixels`.
    pub paint: Paint,
    /// The paint the page's pixels recording draws the path with, from
    /// which a draw made on the CPU is recorded with `SrcOver`.
    pub pixels: Paint,
    pub(crate) spec: Arc<GradientSpec>,
    /// The page's colour space, which the effect computes the bytes in.
    pub page: ColorSpace,
    /// The draw's alpha, its `globalAlpha` included.
    pub alpha: f32,
    /// The device pixels the draw reaches, which an image of the effect
    /// covers.
    pub area: IRect,
    /// Whether the path is a rectangle on whole device pixels, which takes
    /// the effect itself -- see [`GradientDye`](super::GradientDye).
    pub whole_pixels: bool,
    pub blend: BlendMode,
    /// Whether `blend` changes what lies beyond the draw -- see
    /// [`OnGpu::Raster`].
    pub beyond: bool,
}

/// Where an image of a [`GradientPath`]'s effect lies in an atlas: `at`, in
/// `image`, holds the device pixel one up and one left of the path's
/// `area`, so that a pixel the path's edge reaches just outside `area`
/// still finds the effect rather than another path's.
#[derive(Clone)]
struct Slot {
    image: Image,
    at: IPoint,
}

/// What a run's [`atlas`] found for a [`GradientPath`], asked once for the
/// draw: the effect that draws it in the CPU's bytes, `None` where the
/// effect cannot reproduce the CPU; and, for one off whole pixels that fits,
/// where the atlas holds an image of it. Asking costs a hash and a compare
/// of the gradient's every number: asked again as the draw is played, 2000
/// small gradients alternating linear and conic spent 2.4 ms a frame on it.
#[derive(Clone)]
struct Found {
    effect: Option<Shader>,
    slot: Option<Slot>,
}

/// How far an atlas slot reaches past the path's area on every side.
const SLOT_MARGIN_PX: i32 = 1;

/// The widest an atlas is made, in pixels, below the GPU's own limit: a
/// shelf this wide holds a run of small draws in a few rows.
const ATLAS_WIDTH_PX: i32 = 2048;

/// One image holding the effect of each of `draws` that needs one, drawn in
/// one pass onto a surface made from `canvas`; and, by index, what was
/// found for each path: its effect, and where its image is -- none for one
/// that takes the effect itself, or that does not fit, which then makes an
/// image of its own -- see [`GradientSpec::sampled`]. `None` for a draw
/// that is no path. With an
/// image of its own for each draw, 2000 small gradient fills cost about
/// eight times what Ganesh's own gradient did.
///
/// [`GradientSpec::sampled`]: super::exact_gradient::GradientSpec::sampled
fn atlas(
    canvas: &SkCanvas,
    draws: &[Option<&GradientPath>],
) -> Vec<Option<Found>> {
    // The effect for each path, made first: a draw it cannot be made for
    // takes no slot, and is drawn on the CPU instead; one on whole pixels
    // takes the effect itself and no slot either.
    let effects: Vec<Option<Shader>> = draws
        .iter()
        .map(|draw| {
            draw.and_then(|draw| draw.spec.exact(&draw.page, draw.alpha))
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
            let to_slot = Matrix::translate((
                (at.x - origin.0) as f32,
                (at.y - origin.1) as f32,
            ));
            let mut paint = Paint::default();
            paint.set_shader(
                effect
                    .with_local_matrix(&Matrix::concat(&to_slot, &draw.matrix)),
            );
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
/// device pixels onto its own whole: the effect itself for a rectangle on
/// whole pixels, and otherwise an image of it, in an atlas or one of its
/// own, sampled nearest at each pixel's centre -- what the run's atlas
/// `found` for the draw where it was made. `false`, having drawn nothing,
/// where no effect can be made.
fn drawn_with_effect(
    canvas: &SkCanvas,
    draw: &GradientPath,
    found: Option<&Found>,
) -> bool {
    let outer = canvas.local_to_device_as_3x3();
    let slot = found.and_then(|found| found.slot.as_ref());
    let shader = match (draw.whole_pixels, found, slot) {
        (_, Some(Found { effect: None, .. }), _) => None,
        (true, Some(found), _) => found.effect.clone(),
        (true, None, _) => draw.spec.exact(&draw.page, draw.alpha),
        (false, _, Some(slot)) => {
            let origin = (
                draw.area.left - SLOT_MARGIN_PX,
                draw.area.top - SLOT_MARGIN_PX,
            );
            let to_device = Matrix::translate((
                (origin.0 - slot.at.x) as f32 + outer.translate_x(),
                (origin.1 - slot.at.y) as f32 + outer.translate_y(),
            ));
            Matrix::concat(&outer, &draw.matrix)
                .invert()
                .and_then(|from| {
                    slot.image.to_shader(
                        (TileMode::Decal, TileMode::Decal),
                        SamplingOptions::new(
                            FilterMode::Nearest,
                            MipmapMode::None,
                        ),
                        Some(&Matrix::concat(&from, &to_device)),
                    )
                })
        }
        (false, _, None) => {
            draw.spec
                .sampled(&draw.page, draw.alpha, &draw.matrix, draw.area)
        }
    };
    let Some(shader) = shader else {
        return false;
    };
    let mut paint = draw.paint.clone();
    paint.set_shader(shader);
    canvas.save();
    canvas.concat(&draw.matrix);
    canvas.draw_path(&draw.path.lock(), &paint);
    canvas.restore();
    true
}

/// Plays `pixels`, a draw dyed by a gradient as the page's pixels recording
/// records it, onto `canvas`, a GPU canvas, as `gpu` says -- see [`OnGpu`]
/// -- with what the run's atlas `found` for it. A draw made on the CPU, on
/// a canvas whose pixels cannot be read, plays `pixels`.
fn played_on_gpu(
    canvas: &SkCanvas,
    pixels: &GradientPixels,
    gpu: &OnGpu,
    found: Option<&Found>,
) {
    let whole = on_whole_pixels(canvas);
    match gpu {
        OnGpu::Path(draw)
            if whole && drawn_with_effect(canvas, draw, found) =>
        {
            return;
        }
        OnGpu::Effect { picture, .. } if whole => {
            picture.playback(canvas);
            return;
        }
        OnGpu::Native => {
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
            );
        }
        OnGpu::Effect {
            over,
            blend,
            beyond,
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
                    layer,
                    (!beyond).then(|| layer.cull_rect()),
                    &format,
                    &through,
                );
            }
        }
        OnGpu::Alone => rasterized_onto(
            canvas,
            recorded,
            Some(recorded.cull_rect()),
            &format,
            &Paint::default(),
        ),
        OnGpu::Native => pixels.playback(canvas),
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
    let device = canvas.local_to_device_as_3x3();
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
                        reach: recorded.cull_rect(),
                        beyond: *beyond,
                        draw: play,
                    })
                    .collect();
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
                ),
            }
            index += 1;
        }
        canvas.restore();
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
}

impl LayerRec {
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
    /// Through no layer: the entries played as they are, under the group's
    /// transform and clip. A canvas drawn into another through a plain
    /// paint, kept as its entries because they hold a cut draw, which each
    /// target clips its own way -- see [`CutOp::playback`] -- where the
    /// picture they would otherwise be replayed into fixes one way for all.
    Plain,
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
    /// one [`coverage_files`] passes, for a page the coverage route plays on
    /// -- see [`CoverageRun`]. `None` for any other group.
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
/// [`chain::coverage`].
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
    /// see [`run_mask`]. `None` the first time a run is drawn, which is not
    /// kept.
    pub kept: Option<Kept>,
}

impl Composite {
    /// The rectangle the group can be confined to -- see
    /// [`LayerRec::footprint`] -- or for a group that opens no layer, the
    /// rectangle its entries draw in, which `drawn` answers. Asked only
    /// where the layer can be confined: answering it plays nothing, but an
    /// entry recorded when first played -- see [`DeferredPicture`] -- is
    /// recorded to answer, and a chain's layer, under its image filter,
    /// never is confined.
    fn footprint(&self, drawn: impl FnOnce() -> Option<Rect>) -> Option<Rect> {
        match self {
            Composite::Plain => drawn(),
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
}

impl Group {
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
    /// A layer keeps what it holds from blending with what lies beneath it,
    /// so a blend inside one has to be composited in it. A document never
    /// asks this of a group holding one: it embeds every entry up to the
    /// last that blends, with what lies beneath it, first.
    pub(crate) fn opacity(&self) -> Option<f32> {
        match &self.composite {
            Composite::Plain => Some(1.0),
            Composite::Layer(rec) => rec.opacity(),
            Composite::Chain { .. } => None,
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
    /// clip path and no destination of its own and bounded by what its
    /// entries reach: a text run's reach, a clip and a destination each bound
    /// a layer in ways no test of sharing covers yet, so those chains run
    /// alone.
    fn shared_chain(
        &self,
        canvas: &SkCanvas,
        target: Target,
    ) -> Option<SharedChain> {
        let Composite::Chain {
            steps,
            rec,
            reach: None,
            ..
        } = &self.composite
        else {
            return None;
        };
        let alone = target != Target::Pixels
            || !blur_mask::on_gpu(canvas)
            || self.clip.is_some()
            || self.within.is_some()
            || steps.iter().any(Step::runs_in_local_space);
        match alone {
            true => None,
            false => Some(SharedChain {
                steps: Step::for_pixels(steps).unwrap_or_else(|| steps.clone()),
                paint: chain_paint(rec),
            }),
        }
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
    fn draw_chained(&self, layer: &SkCanvas, target: Target, back: &Matrix) {
        self.draw_chained_as(layer, target, back, |layer| {
            for entry in self.contents.layers.iter() {
                entry.playback(layer, target);
            }
        });
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
                    && coverage_plays(
                        canvas,
                        &run.exact,
                        false,
                        sigma * replay_scale(canvas),
                    )
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
                let outcome = chain::run(
                    canvas,
                    pixels.as_deref().unwrap_or(steps),
                    content,
                    Depth::Floats,
                    &paint,
                    |layer| self.draw_chained(layer, target, &back),
                );
                if let Some(outcome) = outcome {
                    outcome.composite(canvas, &paint);
                    if let Some(key) = key {
                        chain_cache::put(key, outcome);
                    }
                }
            }
            (Composite::Plain, target) => {
                let to_device = Matrix::concat(&base, &self.matrix);
                canvas.set_matrix(&to_device.into());
                match self.crop(canvas, target, &to_device) {
                    // On the CPU, played into a layer as large as what they
                    // draw, which cuts none of it, and the layer multiplied
                    // by the crop rather than clipped by it: a clip shader
                    // there would put every draw through Skia's raster
                    // pipeline, whose blend lands a level from its legacy
                    // blitter's on an eight-bit page, once for each draw a
                    // pixel lies under -- three and four over `CLIPPED`.
                    // Multiplied, a pixel the crop covers whole keeps the
                    // bytes the draws left it. The layer composites the
                    // draws as one, which over what the page already holds
                    // rounds within two levels of drawing them one by one.
                    Some(coverage) if clips_by_coverage(canvas, target) => {
                        canvas.set_matrix(&entries.into());
                        let reach = reach_of(&self.contents.layers);
                        let mut rec = SaveLayerRec::default();
                        if let Some(reach) = &reach {
                            rec = rec.bounds(reach);
                        }
                        canvas.save_layer(&rec);
                        let in_layer = InLayer::enter();
                        play_entries(
                            &self.contents.layers,
                            &self.contents.features,
                            canvas,
                            target,
                        );
                        drop(in_layer);
                        canvas.reset_matrix();
                        coverage.multiply(canvas);
                        canvas.restore();
                    }
                    // On the GPU, whose blend a clip shader leaves as it is,
                    // the draws are clipped by the crop's coverage, which
                    // leaves the device's clip -- what a blur reads -- whole.
                    Some(coverage) => {
                        canvas.reset_matrix();
                        coverage.clip(canvas);
                        canvas.set_matrix(&entries.into());
                        play_entries(
                            &self.contents.layers,
                            &self.contents.features,
                            canvas,
                            target,
                        );
                    }
                    None => {
                        self.confine(canvas, target, &to_device);
                        canvas.set_matrix(&entries.into());
                        play_entries(
                            &self.contents.layers,
                            &self.contents.features,
                            canvas,
                            target,
                        );
                    }
                }
            }
            (Composite::Layer(rec) | Composite::Chain { rec, .. }, target) => {
                let to_device = Matrix::concat(&base, &self.matrix);
                canvas.set_matrix(&to_device.into());
                // The destination confines what is drawn and not what the
                // layer's filter makes of it, so under a filter it is a clip
                // inside the layer, where a shadow can still be cast past it.
                // Where it cuts what is drawn, the layer is multiplied by it
                // before it closes, which is as much inside the layer, and
                // leaves both the draws and the layer's composite whole --
                // see `Self::crop`.
                let filtered = rec
                    .paint
                    .as_ref()
                    .is_some_and(|paint| paint.image_filter().is_some());
                let crop = self.crop(canvas, target, &to_device);
                if crop.is_none() && !filtered {
                    self.confine(canvas, target, &to_device);
                }
                rec.open_on(canvas, target, within);
                if crop.is_none() && filtered {
                    self.confine(canvas, target, &to_device);
                }
                canvas.set_matrix(&entries.into());
                let in_layer = InLayer::enter();
                play_entries(
                    &self.contents.layers,
                    &self.contents.features,
                    canvas,
                    target,
                );
                drop(in_layer);
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
    /// piece's picture culls to the page; its draws are replayed once into a
    /// recording with one, which Skia trims to what they draw, and the
    /// rectangle kept. `None` where a draw has no rectangle to take -- see
    /// [`Layer::reach`].
    pub(crate) fn drawn(&self) -> Option<Rect> {
        *self.drawn.get_or_init(|| {
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

/// A draw [`drawn_on_their_backdrop`] makes: what it reaches, in the
/// coordinates of the canvas it is made on; whether its blend changes what
/// lies beyond it; and the draw itself, made onto a canvas under that
/// canvas's transform.
pub(crate) struct BackdropDraw<'a> {
    pub(crate) reach: Rect,
    pub(crate) beyond: bool,
    pub(crate) draw: &'a dyn Fn(&SkCanvas),
}

/// Draws `draws` onto `canvas` one after another as the CPU draws them, over
/// the pixels `canvas` holds there: those pixels read back once into a raster
/// made on the CPU in `format`, each draw made onto it under the transform
/// `canvas` carries, over what the one before it left there, as reading back
/// between them would have found, and the result written back once as it
/// is. The raster covers what the draws reach within the clip, or the
/// clip's bounds whole where one's blend changes what lies beyond it.
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
        let reach = match draw.beyond {
            true => Some(clip),
            false => IRect::intersect(
                &device.map_rect(draw.reach).0.round_out(),
                &clip,
            ),
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
    let whole = format.image_info(ISize::new(area.right, area.bottom));
    let Some(mut surface) = started::raster(&whole, None, None) else {
        return false;
    };
    let raster = surface.canvas();
    if !raster.write_pixels(&info, &backdrop, row, (area.left, area.top)) {
        return false;
    }
    raster.clip_irect(area, ClipOp::Intersect);
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

/// Plays the consecutive [`ReplayedText`] runs `entries` opens with, each
/// under a blend read in the shader -- see [`ReplayedText::reads_the_page`]
/// -- onto a GPU canvas as the CPU draws them, over one read back of the
/// pixels they reach -- see [`drawn_on_their_backdrop`]: on the CPU a run is
/// drawn whole and composites each glyph over the last, where Ganesh, with
/// no framebuffer fetch on Metal, copies the page for every draw that reads
/// it and draws a run's overlapping glyphs in groups that share no pixel,
/// each copy ending a render pass. How many it played, `0` where `entries`
/// opens with no such run or the pixels cannot be read.
fn replayed_on_their_backdrop(entries: &[Layer], canvas: &SkCanvas) -> usize {
    let runs: Vec<&Layer> = entries
        .iter()
        .take_while(|entry| {
            matches!(entry, Layer::ReplayedText(text) if text.reads_the_page())
        })
        .collect();
    if runs.is_empty() {
        return 0;
    }
    #[cfg(test)]
    let runs = match BACKDROP_EACH_RUN.get() {
        true => runs[..1].to_vec(),
        false => runs,
    };
    let Some(format) = Format::of(canvas) else {
        return 0;
    };
    let plays: Vec<_> = runs
        .iter()
        .map(|&entry| {
            move |raster: &SkCanvas| entry.playback(raster, Target::Pixels)
        })
        .collect();
    let draws: Vec<BackdropDraw<'_>> = zip(&runs, &plays)
        .map(|(entry, play)| BackdropDraw {
            reach: match entry {
                Layer::ReplayedText(text) => text.reach(),
                _ => Rect::new_empty(),
            },
            beyond: matches!(entry, Layer::ReplayedText(text) if text.blend.is_some()),
            draw: play,
        })
        .collect();
    match drawn_on_their_backdrop(canvas, &draws, &format) {
        true => runs.len(),
        false => 0,
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
/// under their one clip. A raster or GPU canvas plays each piece under its
/// own clip.
///
/// On pixels it first adds to the colour glyph sheets the glyph images the
/// last playback drew again -- see [`grow_sheets`] -- so that a page adds
/// them once, before any of its runs draws; and it holds consecutive runs of
/// colour text open in one [`BatchScope`], so that those drawn from a sheet
/// are drawn together, before the next entry of another kind plays. On the
/// GPU it plays consecutive kept runs under a blend read in the shader on
/// the CPU, over one read back of the pixels they reach -- see
/// [`replayed_on_their_backdrop`] -- closing an open batch first, so that
/// the read finds what it drew.
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
    let grouped = target == Target::Document || !blur_mask::draws_on(canvas);
    let coverage = clips_by_coverage(canvas, target);
    // Only on a page holding a cut draw: a document walks the picture of a
    // page holding none as vectors -- see `Page::picture` -- and would write
    // the bound as a clip of its own.
    let bounded = target == Target::Pixels
        && !blur_mask::draws_on(canvas)
        && holds_a_cut(entries)
        && bounds_pieces();
    let shares_the_clip = |piece: &Clipped| {
        grouped && !(coverage && matches!(piece.entry, Layer::Cut(_)))
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
            if matches!(entries[at], Layer::ReplayedText(ref text) if text.reads_the_page())
            {
                batch = None;
            }
            let played = replayed_on_their_backdrop(&entries[at..], canvas);
            if played > 0 {
                let kept =
                    zip(&entries[at..at + played], features.iter().skip(at));
                for (entry, features) in kept {
                    pacing::played(canvas, offscreen_layers(entry, *features));
                }
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
/// own, as it would alone.
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

/// The most layers a [`Run`] keeps open at once. Chosen, not measured: two
/// takes in a row of chains each meeting the next and the one before, as
/// blurred shapes along a line do, and each layer open is a snapshot the size
/// of the page held until every chain in the run is composited, so more would
/// hold more of them without a scene here that needs it.
pub(crate) const MAX_RUN_LAYERS: usize = 2;

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

    /// Draws each layer, then finishes and composites every chain onto
    /// `canvas` in the order they were placed -- or in a layer of its own
    /// where Skia refuses its shared one, which then falls back as a chain
    /// alone does.
    fn play(self, canvas: &SkCanvas, target: Target) {
        let Run { chains, layers } = self;
        let mut drawn: Vec<Option<chain::Drawn>> = layers
            .iter()
            .map(|layer| {
                let plans: Vec<&chain::Plan> =
                    layer.iter().map(|&i| &chains[i].plan).collect();
                chain::Drawn::new(canvas, &plans, |k, offscreen| {
                    chains[layer[k]].member.draw.draw(offscreen, target);
                })
            })
            .collect();
        let mut layer_of = vec![0; chains.len()];
        for (l, layer) in layers.iter().enumerate() {
            for &i in layer {
                layer_of[i] = l;
            }
        }
        for (chained, l) in chains.iter().zip(layer_of) {
            match drawn[l].as_mut() {
                Some(drawn) => chained.finish(drawn, canvas),
                None if layers[l].len() > 1 => {
                    let plans = [&chained.plan];
                    let alone =
                        chain::Drawn::new(canvas, &plans, |_, layer| {
                            chained.member.draw.draw(layer, target);
                        });
                    if let Some(mut alone) = alone {
                        chained.finish(&mut alone, canvas);
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
            Leading::Group(group, SharedChain { steps, paint }, then) => {
                let content = group.chain_content(
                    None,
                    &Matrix::new_identity(),
                    to_device,
                );
                #[cfg(test)]
                CHAIN_CONTENT
                    .set(content.map(|content| to_device.map_rect(content).0));
                vec![Member {
                    draw: Draw::Group(group),
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

/// One chain [`play_shared_chains`] runs: what it draws into its layer, its
/// steps, what bounds what it draws, the depth of its layer and the paint
/// it composites through, whose alpha the last step takes; and `then`, the
/// entry played as it is right after its result is composited -- the draw
/// that casts a shadow the chain makes alone.
struct Member<'a> {
    draw: Draw<'a>,
    steps: &'a [Step],
    content: Option<Rect>,
    depth: Depth,
    paint: &'a Paint,
    then: Option<&'a Layer>,
}

/// What a [`Member`] draws into its layer: a group's entries, or one path
/// of a blurred draw, through its paint.
enum Draw<'a> {
    Group(&'a Group),
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
    /// Finishes this chain from `drawn`, the layer holding its draw, and
    /// composites the result onto `canvas`.
    fn finish(&self, drawn: &mut chain::Drawn, canvas: &SkCanvas) {
        #[cfg(test)]
        pacing::tests::chain_finished();
        let Chained { member, plan } = self;
        if let Some(outcome) = drawn.finish(plan, member.steps, member.paint) {
            outcome.composite(canvas, member.paint);
        }
    }
}

/// The layers a GPU renders offscreen for `entry`, asking for `features` of
/// a vector backend, besides those of the entries it holds, which a playback
/// counts as it plays them, as it paces its flushes by them -- see
/// [`pacing`]: one for a group, which opens a layer on the GPU unless it is
/// [`Composite::Plain`], counted all the same; for an image-filtered picture
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
/// recording for pixels files -- so that a picture of the page played for
/// one target does not stand for the other: [`Page::picture`]'s holds the
/// colours placed for pixels.
pub(crate) fn holds_a_pair(entries: &[Layer]) -> bool {
    entries.iter().any(|entry| match entry {
        Layer::Targeted(_) => true,
        Layer::Deferred(deferred) => deferred.is_pair(),
        Layer::ReplayedText(text) => text.is_pair(),
        Layer::Gradient(op) => op.is_pair(),
        Layer::Cut(op) => op.documents.is_some(),
        Layer::Group(group) => holds_a_pair(&group.contents.layers),
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
        for (index, (draw, on_gpu)) in draws.enumerate() {
            match on_gpu.as_ref().filter(|_| gpu) {
                Some(on_gpu) => {
                    canvas.save();
                    played_on_gpu(
                        canvas,
                        &GradientPixels::Recorded(draw.clone()),
                        on_gpu,
                        found.get(index).and_then(Option::as_ref),
                    );
                    canvas.restore();
                }
                None => {
                    canvas.draw_picture(draw, None, None);
                }
            }
        }
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
/// [`chain::coverage`] -- at `device_sigma`, in `canvas`'s device pixels:
/// the half of the route's conditions a draw asks as it is played. A
/// blurred path asks it in [`BlurOp::coverage_colour`] and a run of text in
/// [`Composite::Chain`]'s playback; a run asks [`coverage_files`] as it is
/// recorded, which keeps it from recording a copy this would never play.
///
/// `false` on a page that is not eight bits a channel in RGBA or BGRA; on a
/// canvas that is not a raster on the CPU -- the GPU, a recording, a
/// document; on a page in another space than the colour was held in, as a
/// canvas drawn into one in another space is; for a dithered paint, whose
/// colour varies from pixel to pixel; and for a sigma through the FFT, which
/// blurs a plane's rows in pairs and so rounds an alpha unlike the chain's
/// sums.
fn coverage_plays(
    canvas: &SkCanvas,
    exact: &ExactColour,
    dither: bool,
    device_sigma: f32,
) -> bool {
    #[cfg(test)]
    if !COVERAGE_ROUTE.with(Cell::get) {
        return false;
    }
    let eight = matches!(
        canvas.image_info().color_type(),
        ColorType::RGBA8888 | ColorType::BGRA8888
    );
    let same_space =
        canvas.image_info().color_space().as_ref() == Some(&exact.space);
    eight
        && same_space
        && !dither
        && device_sigma < exact_blur::FFT_FROM_SIGMA
        && blur_mask::draws_on_cpu(canvas)
}

/// Whether a run of text filed under `steps`, in `exact` through a paint
/// `dither`ed or not, is one [`coverage_plays`] could play: under a sole
/// `blur()`, in a colour placed on the bytes an eight-bit page stores,
/// undithered. Only such a run records the opaque copy the route draws --
/// see [`CoverageRun`] -- so a run that could never take the route is
/// recorded once.
pub(crate) fn coverage_files(
    steps: &[Step],
    exact: &ExactColour,
    dither: bool,
) -> bool {
    matches!(steps, [Step::Blur(_)]) && exact.bytes && !dither
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
                if let Some(rgb) =
                    self.coverage_colour(canvas, self.paint.sigma * replay)
                {
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
    fn coverage_colour(
        &self,
        canvas: &SkCanvas,
        device_sigma: f32,
    ) -> Option<[f32; 3]> {
        let exact = self.paint.exact.as_ref()?;
        let placed = matches!(self.paint.faint, Faint::Dropped) && exact.bytes;
        (placed
            && coverage_plays(canvas, exact, self.paint.dither, device_sigma))
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
    use super::{Composite, Entries, Group, luminance_colour};
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
    /// can go.
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

    /// A group through no layer composites what it holds as it is, so a
    /// document opens it at an opacity of 1. `Context2D` makes one only for
    /// a source asking nothing of a backend, which a document plays without
    /// asking this; it holds what the answer is where it is asked.
    #[test]
    fn a_plain_group_opens_at_an_opacity_of_one() {
        let group = Group {
            composite: Composite::Plain,
            matrix: Matrix::new_identity(),
            clip: None,
            entries_clipped: false,
            within: None,
            contents_matrix: Matrix::new_identity(),
            contents: Entries::plain(Vec::new()),
        };
        assert_eq!(group.opacity(), Some(1.0));
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
        pixels::{PixelColorSpace, PixelDepth},
        prelude::*,
    };

    /// The family Oswald is registered under for these runs, so that it
    /// cannot be found as a system font of the same name instead.
    const OSWALD: &str = "CoverageRunOswald";

    /// A run of text through `filter`, how it is filled, the page it is
    /// drawn on, and the depth and space of the page that one is drawn
    /// onto, if any.
    struct Run {
        filter: &'static str,
        fill: &'static str,
        gradient: bool,
        decoration: &'static str,
        depth: PixelDepth,
        onto: Option<(PixelDepth, PixelColorSpace)>,
    }

    const OPAQUE: Run = Run {
        filter: "blur(3px)",
        fill: "rgb(200, 80, 30)",
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
            220.0,
            70.0,
            CanvasOptions {
                color_type: run.depth,
                gpu: false,
                ..CanvasOptions::default()
            },
        )
        .expect("a canvas");
        let ctx = canvas.context();
        assert!(ctx.set_font_css(&format!("32px {OSWALD}")));
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
                    220.0,
                    70.0,
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

    #[test]
    fn opaque_text_keeps_the_chains_alpha() {
        // The route blurs the same coverage the chain's layer holds, so an
        // opaque run's alpha is the chain's byte for byte, and only its
        // colour, applied after the blur, moves.
        let (route, runs) = played(&OPAQUE, true);
        let (chain, chained) = played(&OPAQUE, false);
        assert_eq!((runs, chained), (1, 0), "the route took the run once");
        let alpha = |pixels: &[u8]| -> Vec<u8> {
            pixels.chunks(4).map(|p| p[3]).collect()
        };
        assert_eq!(alpha(&route), alpha(&chain), "the alpha moved");
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
                0,
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
