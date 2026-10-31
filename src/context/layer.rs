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
    cell::RefCell,
    f32::consts::SQRT_2,
    iter,
    marker::PhantomData,
    sync::{Arc, OnceLock},
};

use parking_lot::Mutex;
use skia_safe::{
    AlphaType, BlendMode, BlurStyle, Canvas as SkCanvas, ClipOp, Color4f,
    ColorFilter, ColorSpace, ColorType, Data, FilterMode, IRect, Image,
    ImageFilter, ImageInfo, MaskFilter, Matrix, Paint, PaintCap, PaintJoin,
    PaintStyle, Path, PathDirection, Picture, PictureRecorder, Point, Rect,
    RoundOut, RuntimeEffect, SamplingOptions, Shader, TileMode,
    canvas::SaveLayerRec, color_filters, image_filters,
};

use crate::{
    context::{
        blur_mask,
        chain::{self, Depth, Step},
        chain_cache, coverage_of,
        faint::{Faint, composited},
        page::{ExportOptions, Page},
    },
    export::VectorFeatures,
    gpu::RenderingEngine,
    node::{filter::MASK_BLUR_MAX_SIGMA, typography::RunPaint},
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
    /// each kind of target -- see `page::Recording`: a flat colour held
    /// exactly in the page's own space for pixels, and as a plain colour for
    /// a document. Shared, so the two pictures are one pointer.
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
            Layer::Cut(op) => op.playback(canvas, target),
            Layer::Clipped(piece) => piece.playback(canvas, target),
            Layer::Pattern(tile) => tile.playback(canvas, target),
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
/// dyed by a pattern of the canvas's pixels is. In a document the draw is
/// played as recorded, with the page's picture as its pattern -- which
/// Skia's PDF backend writes as an image, and its SVG backend drops: a
/// canvas's pattern asks an SVG export to embed the draw -- see
/// `Dye::vector_features`.
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

impl PatternDraw {
    fn playback(&self, canvas: &SkCanvas, target: Target) {
        canvas.save();
        if let Some(clip) = self.clip.as_ref().and_then(Path::deserialize) {
            canvas.clip_path(&clip, ClipOp::Intersect, true);
        }
        match target {
            Target::Pixels | Target::Embedded => self.paint_onto(canvas),
            Target::Document => self.plain.playback(canvas),
        }
        canvas.restore();
    }

    /// Paints the pattern onto `canvas` where the draw covers it.
    fn paint_onto(&self, canvas: &SkCanvas) {
        let to_draw =
            Matrix::concat(&canvas.local_to_device_as_3x3(), &self.matrix);
        let density = self
            .source
            .density_for(&Matrix::concat(&to_draw, &self.pattern_matrix));
        let Some(image) = self.source.raster(canvas, density) else {
            return;
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
        let mut paint = self.paint.clone();
        paint.set_shader(shader);
        match &self.replay {
            Replay::Path(path, faint) => {
                let Some(path) = Path::deserialize(path) else {
                    return;
                };
                canvas.concat(&self.matrix);
                canvas.draw_path(&path, &faint.drawable(&paint));
            }
            Replay::Text {
                run,
                at,
                offset,
                condensation,
                global_alpha,
                faint,
            } => {
                canvas.concat(&self.matrix);
                // Squeezed as `draw_text` squeezes it: by a scale about the
                // anchor, the pen with it.
                if *condensation == 1.0 {
                    run.paint(
                        canvas,
                        *at + *offset,
                        &paint,
                        *global_alpha,
                        *faint,
                    );
                } else {
                    canvas.translate(*at).scale((*condensation, 1.0));
                    run.paint(canvas, *offset, &paint, *global_alpha, *faint);
                }
            }
        }
    }

    /// What the draw covers, under the transform it is played under.
    fn reach(&self) -> Rect {
        self.plain.cull_rect()
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
    Chain { steps: Arc<[Step]>, rec: LayerRec },
}

impl Composite {
    /// The rectangle the group can be confined to -- see
    /// [`LayerRec::footprint`] -- or for a group that opens no layer, the
    /// rectangle its entries draw in.
    fn footprint(&self, drawn: Option<Rect>) -> Option<Rect> {
        match self {
            Composite::Plain => drawn,
            Composite::Layer(rec) | Composite::Chain { rec, .. } => {
                rec.footprint(drawn)
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
        Self { layers, features }
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
        let within = pixels
            .then(|| self.composite.footprint(reach_of(&self.contents.layers)));
        self.playback_within(canvas, target, within.flatten());
    }

    /// The rectangle the group can draw in, under the transform it is
    /// played back under: what its entries draw in, through the transform
    /// they are played under, where the layer can be confined to it. `None`
    /// where it covers what the clip allows.
    pub fn reach(&self) -> Option<Rect> {
        let drawn =
            self.composite.footprint(reach_of(&self.contents.layers))?;
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
                let content =
                    match (&self.within, self.contents.layers.as_slice()) {
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
                        for entry in self.contents.layers.iter() {
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
            (Composite::Plain, target) => {
                canvas.set_matrix(&Matrix::concat(&base, &self.matrix).into());
                if let Some(within) = &self.within {
                    canvas.clip_rect(within, ClipOp::Intersect, true);
                }
                canvas.set_matrix(&entries.into());
                play_entries(&self.contents.layers, canvas, target);
            }
            (Composite::Layer(rec) | Composite::Chain { rec, .. }, target) => {
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
                play_entries(&self.contents.layers, canvas, target);
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

/// Plays `entries` onto `canvas` for `target`, in order: each as
/// [`Layer::playback`] plays it, but for consecutive pieces of one run
/// played into a document or a recording, which it plays under their clip
/// once -- see [`Clipped`] -- all but a recording's cut draws.
///
/// A recording is what a canvas drawn into another, or used as a pattern,
/// is replayed into, and Skia rasterizes it later, where every clip applied
/// rebuilds its anti-aliased clip mask: one for each piece, where the page
/// applied one for the frame. A cut draw there clips by coverage, alone --
/// see [`CutOp::playback`] -- and the pieces between cut draws share one
/// clip. A document clips a cut draw by the path, among the run's pieces
/// under their one clip. A raster or GPU canvas plays each piece under its
/// own clip.
pub(crate) fn play_entries(
    entries: &[Layer],
    canvas: &SkCanvas,
    target: Target,
) {
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
    let mut at = 0;
    while at < entries.len() {
        let run = match &entries[at] {
            Layer::Clipped(piece) if shares_the_clip(piece) => piece.run,
            entry => {
                entry.playback(canvas, target);
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
fn drawn_by(picture: &Picture) -> Option<Rect> {
    let mut recorder = PictureRecorder::new();
    let canvas = recorder.begin_recording(picture.cull_rect(), true);
    picture.playback(canvas);
    Some(recorder.finish_recording_as_picture(None)?.cull_rect())
}

/// Whether a run's pieces are bounded -- see [`Clipped::play_run`]: always,
/// but off on a thread a test has turned it off on.
fn bounds_pieces() -> bool {
    #[cfg(test)]
    if !BOUNDS_PIECES.get() {
        return false;
    }
    true
}

/// Whether a piece played onto a raster on the CPU is bounded -- see
/// [`Clipped::playback`]: always, but off on a thread a test has turned it
/// off on.
fn bounds_page() -> bool {
    #[cfg(test)]
    if !BOUNDS_PAGE.get() {
        return false;
    }
    true
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
        | Layer::Pattern(_) => false,
    })
}

impl CutOp {
    /// The draw `drawn` holds, under `clip`, in device pixels of the
    /// recording, played as pixels through `coverage`: a picture, or on a
    /// page that keeps a recording for each kind of target a pair of them;
    /// `None` for anything else.
    pub fn new(
        clip: &ClipData,
        coverage: Arc<Coverage>,
        drawn: Layer,
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
        })
    }

    /// Adds the draw `drawn` holds to the run, under the op's clip, into each
    /// recording the op keeps. A draw that holds other than the recordings
    /// the op keeps is dropped, so that the two hold the same draws.
    pub fn extend(&mut self, drawn: Layer) {
        match (drawn, &mut self.documents) {
            (Layer::Picture(picture), None) => self.draws.push(picture),
            (Layer::Targeted(pair), Some(documents)) => {
                self.draws.push(pair.0.clone());
                documents.push(pair.1.clone());
            }
            _ => {}
        }
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
        for draw in self.draws_for(target) {
            canvas.draw_picture(draw, None, None);
        }
    }

    /// Draws the cut draws for `target` onto `canvas`, under whatever
    /// transform and clip the canvas already carries, the op's clip applied
    /// as coverage where [`clips_by_coverage`] says so and by the path
    /// elsewhere.
    pub fn playback(&self, canvas: &SkCanvas, target: Target) {
        let draws = self.draws_for(target);
        canvas.save();
        if clips_by_coverage(canvas, target) {
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
    use super::{Composite, Entries, Group, luminance_colour};
    use skia_safe::{BlendMode, Color, Color4f, Matrix, color_filters};
    use std::iter::zip;

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
