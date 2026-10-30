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
    f32::consts::SQRT_2,
    iter,
    sync::{Arc, OnceLock},
};

use skia_safe::{
    AlphaType, BlendMode, BlurStyle, Canvas as SkCanvas, ClipOp, Color4f,
    ColorSpace, ColorType, Data, FilterMode, IRect, ImageFilter, ImageInfo,
    MaskFilter, Matrix, Paint, PaintCap, PaintJoin, PaintStyle, Path,
    PathDirection, Picture, PictureRecorder, Rect, RoundOut, SamplingOptions,
    Shader, TileMode, canvas::SaveLayerRec, image_filters,
};

use crate::{
    context::{
        blur_mask,
        chain::{self, Depth, Step},
        chain_cache, coverage_of,
        faint::{Faint, composited},
        page::Recording,
    },
    node::filter::MASK_BLUR_MAX_SIGMA,
    started,
};

/// One entry of a page's recording, played back in order.
#[derive(Clone, Debug)]
pub enum Layer {
    /// Draws Skia recorded, played back as recorded.
    Picture(Picture),
    /// A blurred flat-colour draw, or a run of them, its blur made at
    /// playback.
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
    /// each kind of target -- see `page::Recording`: a flat colour held
    /// exactly in the page's own space for pixels, and as a plain colour for
    /// a document. Shared, so the two pictures are one pointer.
    Targeted(Arc<(Picture, Picture)>),
    /// A fill or stroke its clip cuts, or a run of them under one clip, the
    /// clip applied at playback.
    Cut(Arc<CutOp>),
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
        }
    }

    /// The rectangle this entry can draw in, in the coordinates it is played
    /// back under: a picture's cull rectangle -- a pair's, the one played as
    /// pixels -- which Skia computes from what it draws because every
    /// recording here is begun with a bounding hierarchy, a group's
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
        }
    }
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
    let Some(clip) = canvas.device_clip_bounds() else {
        return;
    };
    canvas.save();
    canvas.reset_matrix();
    canvas.draw_annotation(Rect::from(clip), TEXT_REACH, &Data::new_empty());
    canvas.restore();
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

/// A flat colour held exactly in a page's own space, for the recording a
/// page plays as pixels: Skia converts a plain paint colour through an
/// approximation and lands it off -- see `page::Recorders`.
#[derive(Clone, Debug, PartialEq)]
pub struct ExactColour {
    /// The colour's components in `space`, unpremultiplied, opaque.
    pub rgb: [f32; 3],
    /// The page's space.
    pub space: ColorSpace,
}

impl ExactColour {
    /// A shader painting the colour: a 1x1 image in the page's space at
    /// full float precision, repeated. Drawn onto a page in that space,
    /// Skia converts it by the identity, so it lands exactly; a paint's
    /// alpha scales it.
    pub fn shader(&self) -> Option<Shader> {
        let info = ImageInfo::new(
            (1, 1),
            ColorType::RGBAF32,
            AlphaType::Premul,
            Some(self.space.clone()),
        );
        let [r, g, b] = self.rgb;
        let pixel: Vec<u8> = [r, g, b, 1.0]
            .iter()
            .flat_map(|c| c.to_le_bytes())
            .collect();
        started::raster_from_data(
            &info,
            Data::new_copy(&pixel),
            info.min_row_bytes(),
        )?
        .to_shader(
            (TileMode::Repeat, TileMode::Repeat),
            SamplingOptions::default(),
            None,
        )
    }
}

/// What a page is played back onto, which decides how a [`Group`]
/// composites.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// Pixels: a raster or GPU surface, or a picture Skia itself will draw
    /// as pixels -- a pattern's or an image's.
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
}

impl LayerRec {
    /// Opens the layer on `canvas`, under whatever transform and clip it
    /// carries, compositing as `target` needs: on pixels through the paint
    /// [`composited`] hands over, which keeps the alpha exact; in a
    /// document through the paint as it is, which the backend writes as an
    /// opacity and keeps vector. Confined to `within` where that is `Some`:
    /// a rectangle in device pixels -- see [`Self::footprint`].
    fn open_on(&self, canvas: &SkCanvas, target: Target, within: Option<Rect>) {
        let paint = match (&self.paint, target) {
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
        if let Some(backdrop) = &self.backdrop {
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
        let plain = self.paint.as_ref().is_none_or(|paint| {
            paint.image_filter().is_none()
                && paint.color_filter().is_none()
                && paint.as_blend_mode() == Some(BlendMode::SrcOver)
        });
        (plain && self.backdrop.is_none())
            .then_some(drawn)
            .flatten()
    }

    /// Whether a layer holding nothing still changes the page: through a
    /// filter over what lies beneath it, or a blend mode that replaces what
    /// it covers with the nothing it holds.
    pub fn marks_when_empty(&self) -> bool {
        let blend = self.paint.as_ref().and_then(Paint::as_blend_mode);
        self.backdrop.is_some()
            || !matches!(blend, None | Some(BlendMode::SrcOver))
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
    Chain { steps: Arc<[Step]>, rec: LayerRec },
}

impl Composite {
    /// What the group opens as a layer where it opens one.
    fn rec(&self) -> &LayerRec {
        match self {
            Composite::Layer(rec) | Composite::Chain { rec, .. } => rec,
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
    /// A rectangle under `matrix` that what the group draws is confined to:
    /// the destination a canvas is drawn into. It bounds what is drawn and
    /// not what a filter makes of it, which spreads past it as a blur of
    /// any other draw spreads past the draw.
    pub within: Option<Rect>,
    /// The transform the entries are played under: the identity for what a
    /// layer held, recorded in page coordinates; for a canvas drawn in, the
    /// mapping from its page onto this one.
    pub contents_matrix: Matrix,
    /// Shared, so that a canvas drawn in brings its page's list without
    /// copying it.
    pub contents: Arc<Vec<Layer>>,
}

impl Group {
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
        let pixels = target == Target::Pixels && blur_mask::draws_on(canvas);
        let within = pixels
            .then(|| self.composite.rec().footprint(reach_of(&self.contents)));
        self.playback_within(canvas, target, within.flatten());
    }

    /// The rectangle the group can draw in, under the transform it is
    /// played back under: what its entries draw in, through the transform
    /// they are played under, where the layer can be confined to it. `None`
    /// where it covers what the clip allows.
    pub fn reach(&self) -> Option<Rect> {
        let drawn = self.composite.rec().footprint(reach_of(&self.contents))?;
        Some(self.contents_matrix.map_rect(drawn).0)
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
                && blur_mask::draws_on_cpu(canvas)
                && holds_a_cut(&self.contents))
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
            (Composite::Chain { steps, rec }, Target::Pixels) => {
                let mut paint = rec.paint.clone().unwrap_or_default();
                paint.set_image_filter(None);
                // The chain's lengths are in the pixels of the canvas the
                // group was drawn on, which `base` maps to the device.
                // What is drawn is bounded by `within`, or for a single
                // picture by what it was recorded within.
                let content = match (&self.within, self.contents.as_slice()) {
                    (Some(within), _) => Some(self.matrix.map_rect(within).0),
                    (None, [Layer::Picture(picture)]) => Some(
                        self.contents_matrix.map_rect(picture.cull_rect()).0,
                    ),
                    (None, _) => None,
                };
                // A group a canvas drawn into another brings with it is played
                // once for each time that canvas is, and its chain is run the
                // first of those -- see `chain_cache`.
                let key = chain_cache::Key::of(self, canvas);
                if let Some(outcome) = key.as_ref().and_then(chain_cache::get) {
                    outcome.composite(canvas, &paint);
                    canvas.restore();
                    return;
                }
                let outcome = chain::run(
                    canvas,
                    steps,
                    content,
                    Depth::Floats,
                    &paint,
                    |layer| {
                        let to_layer = layer.local_to_device_as_3x3();
                        layer.set_matrix(
                            &Matrix::concat(&to_layer, &self.matrix).into(),
                        );
                        if let Some(within) = &self.within {
                            layer.clip_rect(within, ClipOp::Intersect, true);
                        }
                        layer.set_matrix(
                            &Matrix::concat(&to_layer, &self.contents_matrix)
                                .into(),
                        );
                        for entry in self.contents.iter() {
                            entry.playback(layer, Target::Pixels);
                        }
                    },
                );
                if let Some(outcome) = outcome {
                    outcome.composite(canvas, &paint);
                    if let Some(key) = key {
                        chain_cache::put(key, outcome);
                    }
                }
            }
            (composite, target) => {
                let rec = composite.rec();
                canvas.set_matrix(&Matrix::concat(&base, &self.matrix).into());
                // The destination confines what is drawn and not what the
                // layer's filter makes of it, so under a filter it is a clip
                // inside the layer, where a shadow can still be cast past it.
                let filtered = rec
                    .paint
                    .as_ref()
                    .is_some_and(|paint| paint.image_filter().is_some());
                let clip_within = |canvas: &SkCanvas| {
                    if let Some(within) = &self.within {
                        canvas.clip_rect(within, ClipOp::Intersect, true);
                    }
                };
                if !filtered {
                    clip_within(canvas);
                }
                rec.open_on(canvas, target, within);
                if filtered {
                    clip_within(canvas);
                }
                canvas.set_matrix(&entries.into());
                for entry in self.contents.iter() {
                    entry.playback(canvas, target);
                }
                canvas.restore();
            }
        }
        canvas.restore();
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
/// docs/browser-differences.md. Anywhere else --
/// the GPU, whose multisampled coverage the clip does not move, a vector
/// document, a recording -- it clips as the page always has. A run of cut
/// draws shares one op, so that a document clips the run once, as it clips
/// the draws of an ordinary layer, rather than once for each.
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
    /// The clip as coverage, for a playback that applies it so.
    coverage: Coverage,
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
        let shader = self.picture.to_shader(
            (TileMode::Decal, TileMode::Decal),
            FilterMode::Nearest,
            Some(&self.corner),
            Some(&self.tile),
        );
        canvas.clip_shader(shader, ClipOp::Intersect);
    }
}

/// Whether `entries`, or a group among them, hold a [`CutOp`].
fn holds_a_cut(entries: &[Layer]) -> bool {
    entries.iter().any(|entry| match entry {
        Layer::Cut(_) => true,
        Layer::Group(group) => holds_a_cut(&group.contents),
        Layer::Picture(_) | Layer::Blur(_) | Layer::Targeted(_) => false,
    })
}

impl CutOp {
    /// The draw `draw` records, under `clip`, in device pixels of the
    /// recording, and `matrix`, into one recording or, with `dual`, one for
    /// each kind of target; `None` where a picture could not be made.
    pub fn new(
        clip: &Path,
        matrix: &Matrix,
        bounds: Rect,
        dual: bool,
        draw: impl Fn(&SkCanvas, Recording),
    ) -> Option<Self> {
        let (pixels, document) = match dual {
            false => (Recording::Shared, None),
            true => (Recording::Pixels, Some(Recording::Document)),
        };
        let documents = match document {
            Some(document) => {
                Some(vec![Self::record(matrix, bounds, document, &draw)?])
            }
            None => None,
        };
        Some(CutOp {
            draws: vec![Self::record(matrix, bounds, pixels, &draw)?],
            documents,
            clip: clip.serialize(),
            coverage: Coverage::of(clip)?,
        })
    }

    /// Adds the draw `draw` records under `matrix` to the run, under the
    /// op's clip, into each recording the op keeps, unless a picture could
    /// not be made of it -- which drops it, as [`CutOp::new`] does.
    pub fn extend(
        &mut self,
        matrix: &Matrix,
        bounds: Rect,
        draw: impl Fn(&SkCanvas, Recording),
    ) {
        let Some(documents) = &mut self.documents else {
            self.draws.extend(Self::record(
                matrix,
                bounds,
                Recording::Shared,
                &draw,
            ));
            return;
        };
        // Both or neither, so that the two recordings hold the same draws.
        if let (Some(pixels), Some(document)) = (
            Self::record(matrix, bounds, Recording::Pixels, &draw),
            Self::record(matrix, bounds, Recording::Document, &draw),
        ) {
            self.draws.push(pixels);
            documents.push(document);
        }
    }

    /// What the draws paint before the clip applies, in device pixels of
    /// the recording: the rectangle a layer holding the op is confined to.
    /// The recording played as pixels, as for a pair -- a document is never
    /// confined.
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

    /// The draw `draw` makes into `recording` under `matrix`, recorded in
    /// `bounds` with a bounding hierarchy, so that its cull rectangle is
    /// what it draws -- see [`CutOp::reach`].
    fn record(
        matrix: &Matrix,
        bounds: Rect,
        recording: Recording,
        draw: &impl Fn(&SkCanvas, Recording),
    ) -> Option<Picture> {
        let mut recorder = PictureRecorder::new();
        let canvas = recorder.begin_recording(bounds, true);
        canvas.concat(matrix);
        draw(canvas, recording);
        recorder.finish_recording_as_picture(None)
    }

    /// Draws the cut draws for `target` onto `canvas`, under whatever
    /// transform and clip the canvas already carries.
    pub fn playback(&self, canvas: &SkCanvas, target: Target) {
        let draws = match (target, &self.documents) {
            (Target::Document, Some(documents)) => documents,
            _ => &self.draws,
        };
        canvas.save();
        if blur_mask::draws_on_cpu(canvas) {
            self.coverage.clip(canvas);
        } else if let Some(clip) = Path::deserialize(&self.clip) {
            canvas.clip_path(&clip, ClipOp::Intersect, true);
        }
        for draw in draws {
            canvas.draw_picture(draw, None, None);
        }
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
            None => Geometry::Path(path.serialize()),
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
/// is not a rectangle, and the clip, are kept serialized as `Data`, which is.
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
/// and which is plain data, or any other path, serialized.
#[derive(Clone, Debug)]
enum Geometry {
    /// A closed rectangle, drawn as the path `Path::rect` makes of it.
    Rect(Rect, PathDirection),
    Path(Data),
}

impl Geometry {
    fn path(&self) -> Option<Path> {
        match self {
            Geometry::Rect(rect, direction) => {
                Some(Path::rect(rect, *direction))
            }
            Geometry::Path(data) => Path::deserialize(data),
        }
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
        // How far the playback scales a canvas pixel: the density, a nested
        // canvas's size. The recording's own transform is not part of it,
        // since a filter's length does not follow the canvas transform.
        let replay = canvas
            .local_to_device_as_3x3()
            .map_radius(1.0)
            .filter(|s| s.is_normal() && *s > 0.0)
            .unwrap_or(1.0);
        if self.draws_exactly(canvas, replay, target) {
            canvas.restore();
            return;
        }

        let mut paint = Paint::new(self.paint.color, None);
        if target == Target::Pixels
            && let Some(shader) =
                self.paint.exact.as_ref().and_then(ExactColour::shader)
        {
            paint.set_shader(shader);
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

        // The coverage blur where the recording chose it, as long as the
        // sigma it comes to here is one it can draw: past
        // `MASK_BLUR_MAX_SIGMA` device pixels it draws the cap instead, so
        // `blur(40px)` exported at density 4 came out a blur of 128 pixels
        // where 160 were asked for, and the result is blurred instead.
        // One choice for the run: its rectangles share a coverage class.
        let device_sigma = self.paint.sigma * replay;
        let key = device_sigma.to_bits();
        let made = match self.made.get() {
            Some((at, blur)) if *at == key => blur.clone(),
            _ => {
                let coverage = coverage_of(&path, &self.matrix)
                    .blurs_alike(self.paint.sigma)
                    && device_sigma <= MASK_BLUR_MAX_SIGMA;
                let blur = self.make(coverage);
                // Kept for the next playback only if none was kept yet.
                let _ = self.made.set((key, blur.clone()));
                blur
            }
        };
        let paths = || {
            iter::once(path.clone())
                .chain(self.geometry.iter().skip(1).filter_map(Geometry::path))
        };
        match (made, target) {
            // On pixels the result is blurred by `chain`, exactly, each
            // geometry of a run in a layer of its own, as separate draws
            // composite. The path is drawn opaque in its colour and the
            // colour's alpha, the global alpha in it, is the chain's gain,
            // so that the blur is rounded once and not once more where the
            // draw is.
            (Made::Result(_), Target::Pixels) => {
                paint.set_alpha_f(1.0);
                let mut gain = Paint::default();
                gain.set_alpha_f(self.paint.color.a);
                let steps = [Step::Blur(self.paint.sigma)];
                let depth = match self.paint.faint {
                    Faint::Dropped => Depth::Bytes,
                    Faint::Kept => Depth::Floats,
                };
                for path in paths() {
                    let content = self.matrix.map_rect(self.painted(&path)).0;
                    chain::apply(
                        canvas,
                        &steps,
                        Some(content),
                        depth,
                        &gain,
                        |layer| {
                            layer.concat(&self.matrix);
                            layer.draw_path(&path, &paint);
                        },
                    );
                }
            }
            (made, _) => {
                canvas.concat(&self.matrix);
                match made {
                    Made::Coverage(filter) => paint.set_mask_filter(filter),
                    Made::Result(filter) => paint.set_image_filter(filter),
                };
                let paint = self.paint.faint.drawable(&paint);
                // Each of a run drawn on its own as it was recorded.
                for path in paths() {
                    canvas.draw_path(&path, &paint);
                }
            }
        }
        canvas.restore();
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

    /// Draws the run exactly where it is one [`blur_mask::draw_rect`] takes:
    /// anti-aliased filled rectangles, undithered, that `canvas`'s transform
    /// keeps rectangles, on a canvas with pixels. Returns whether it drew;
    /// `false` leaves `canvas` as it found it.
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
        if !plain
            || !device.rect_stays_rect()
            || device.has_perspective()
            || !blur_mask::draws_on(canvas)
        {
            return false;
        }
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
