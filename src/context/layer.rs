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
//! checks that choice when it is played back, against the sigma it has
//! there.

use std::{
    borrow::Cow,
    f32::consts::SQRT_2,
    iter,
    sync::{Arc, OnceLock},
};

use skia_safe::{
    BlendMode, BlurStyle, Canvas as SkCanvas, ClipOp, Color4f, Data,
    ImageFilter, MaskFilter, Matrix, Paint, PaintCap, PaintJoin, PaintStyle,
    Path, PathDirection, Picture, Rect, TileMode, canvas::SaveLayerRec,
    image_filters,
};

use crate::{
    context::{
        chain::{self, Depth, Step},
        coverage_of,
        faint::{Faint, composited},
    },
    node::filter::MASK_BLUR_MAX_SIGMA,
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
}

impl Layer {
    /// Draws this entry onto `canvas` for `target`, under whatever transform
    /// and clip the canvas already carries.
    pub fn playback(&self, canvas: &SkCanvas, target: Target) {
        match self {
            Layer::Picture(picture) => picture.playback(canvas),
            Layer::Blur(op) => op.playback(canvas, target),
            Layer::Group(group) => group.playback(canvas, target),
        }
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
    /// opacity and keeps vector.
    fn open_on(&self, canvas: &SkCanvas, target: Target) {
        let paint = match (&self.paint, target) {
            (Some(paint), Target::Pixels) => Some(composited(paint)),
            (Some(paint), Target::Document) => Some(Cow::Borrowed(paint)),
            (None, _) => None,
        };
        let mut rec = SaveLayerRec::default();
        if let Some(paint) = &paint {
            rec = rec.paint(paint);
        }
        if let Some(bounds) = &self.bounds {
            rec = rec.bounds(bounds);
        }
        if let Some(backdrop) = &self.backdrop {
            rec = rec.backdrop(backdrop);
        }
        canvas.save_layer(&rec);
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

/// Entries composited as one through a paint: what a `saveLayer` held when
/// it closed, or a canvas drawn into another through a paint.
///
/// Kept as ours rather than recorded into a picture, because how the group
/// composites depends on where it is played back -- see
/// [`LayerRec::open_on`] -- and once in a picture only Skia plays it.
#[derive(Clone, Debug)]
pub struct Group {
    /// How the group composites onto what lies beneath it.
    pub rec: LayerRec,
    /// The transform and the clip the group composites under, in page
    /// coordinates; the clip is in device space, as the recorder holds it,
    /// and serialized, as a [`BlurOp`]'s is, so that a page can be sent to
    /// another thread.
    pub matrix: Matrix,
    pub clip: Option<Data>,
    /// A rectangle under `matrix` the group is confined to: the destination
    /// a canvas is drawn into.
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
    pub fn playback(&self, canvas: &SkCanvas, target: Target) {
        let base = canvas.local_to_device_as_3x3();
        canvas.save();
        if let Some(clip) = self.clip.as_ref().and_then(Path::deserialize) {
            canvas.clip_path(&clip, ClipOp::Intersect, true);
        }
        canvas.set_matrix(&Matrix::concat(&base, &self.matrix).into());
        if let Some(within) = &self.within {
            canvas.clip_rect(within, ClipOp::Intersect, true);
        }
        self.rec.open_on(canvas, target);
        canvas.set_matrix(&Matrix::concat(&base, &self.contents_matrix).into());
        for entry in self.contents.iter() {
            entry.playback(canvas, target);
        }
        canvas.restore();
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
            },
        }
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

    /// Draws the blurred geometry onto `canvas` for `target`, with the
    /// coverage blur the recording chose wherever it can draw the sigma this
    /// comes to in `canvas`'s device pixels, and the exact blur of the
    /// result where it cannot.
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

        let mut paint = Paint::new(self.paint.color, None);
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
            // composite.
            (Made::Result(_), Target::Pixels) => {
                let paint = self.paint.faint.drawable(&paint);
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
                        &Paint::default(),
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
