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

use std::{
    borrow::Cow,
    f32::consts::SQRT_2,
    iter,
    sync::{Arc, OnceLock},
};

use skia_safe::{
    AlphaType, BlendMode, BlurStyle, Canvas as SkCanvas, ClipOp, Color4f,
    ColorFilter, ColorSpace, ColorType, Data, ImageFilter, ImageInfo,
    MaskFilter, Matrix, Paint, PaintCap, PaintJoin, PaintStyle, Path,
    PathDirection, Picture, Rect, RuntimeEffect, SamplingOptions, Shader,
    TileMode, canvas::SaveLayerRec, color_filters, image_filters,
};

use crate::{
    context::{
        blur_mask,
        chain::{self, Depth, Step},
        chain_cache, coverage_of,
        faint::{Faint, composited},
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
                Target::Embedded | Target::Document => {
                    pictures.1.playback(canvas)
                }
            },
        }
    }

    /// The rectangle this entry can draw in, in the coordinates it is played
    /// back under: a picture's cull rectangle -- a pair's, the one played as
    /// pixels -- which Skia computes from what it draws because every
    /// recording here is begun with a bounding hierarchy, and a group's
    /// [`Group::reach`]. `None` where it is not
    /// known: a blurred draw has no such rectangle to take.
    fn reach(&self) -> Option<Rect> {
        match self {
            Layer::Picture(picture) => Some(picture.cull_rect()),
            Layer::Blur(_) => None,
            Layer::Group(group) => group.reach(),
            // The pixels' recording: the document's holds the same draws,
            // and a document is never confined.
            Layer::Targeted(pictures) => Some(pictures.0.cull_rect()),
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
/// nullptr)`, as `SkPaintPriv::ComputeLuminanceColor` asks it.
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
    Some([0, 1, 2].map(|c| f32::from_le_bytes(channels[c])))
}

thread_local! {
    /// [`PLAIN_UNCONVERTED_SKSL`] compiled, once per thread.
    static PLAIN_UNCONVERTED: Option<RuntimeEffect> =
        RuntimeEffect::make_for_color_filter(PLAIN_UNCONVERTED_SKSL, None).ok();
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
    /// [`luminance_colour`] for how that is read, once per colour.
    pub fn dye(&self, plain: Color4f) -> Option<ColorFilter> {
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

/// What a page is played back onto, which decides how a [`Group`]
/// composites.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// Pixels: a raster or GPU surface, or a picture Skia itself will draw
    /// as pixels -- a pattern's or an image's.
    Pixels,
    /// Pixels a document embeds for what it cannot write as vectors: an
    /// eight-bit sRGB raster, composited as pixels are, in the colours the
    /// document holds. Played from a pair's document recording: from its
    /// pixels recording, with text drawn through its dye, the raster an SVG
    /// embeds parted from an eight-bit page's in all 41 colours swept.
    Embedded,
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
            (Some(paint), Target::Pixels | Target::Embedded) => {
                Some(composited(paint))
            }
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
        let pixels = target != Target::Document && blur_mask::draws_on(canvas);
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
            canvas.clip_path(&clip, ClipOp::Intersect, true);
        }
        match (&self.composite, target) {
            (
                Composite::Chain { steps, rec },
                Target::Pixels | Target::Embedded,
            ) => {
                let mut paint = rec.paint.clone().unwrap_or_default();
                paint.set_image_filter(None);
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
                // What is drawn is bounded by `within`, or for a single
                // picture by what it was recorded within.
                let to_space = |matrix: &Matrix| Matrix::concat(&back, matrix);
                let content = match (&self.within, self.contents.as_slice()) {
                    (Some(within), _) => {
                        Some(to_space(&self.matrix).map_rect(within).0)
                    }
                    (None, [Layer::Picture(picture)]) => Some(
                        to_space(&self.contents_matrix)
                            .map_rect(picture.cull_rect())
                            .0,
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
                            &Matrix::concat(&to_layer, &to_space(&self.matrix))
                                .into(),
                        );
                        if let Some(within) = &self.within {
                            layer.clip_rect(within, ClipOp::Intersect, true);
                        }
                        layer.set_matrix(
                            &Matrix::concat(
                                &to_layer,
                                &to_space(&self.contents_matrix),
                            )
                            .into(),
                        );
                        for entry in self.contents.iter() {
                            entry.playback(layer, target);
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
            (Made::Result(_), Target::Pixels | Target::Embedded) => {
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

#[cfg(test)]
mod tests {
    use super::luminance_colour;
    use skia_safe::{BlendMode, Color, Color4f, color_filters};
    use std::iter::zip;

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
