//! Colour glyphs of a font with a `COLR` table, drawn by this crate rather
//! than by the platform's scaler.
//!
//! On macOS a typeface is CoreText's, and CoreText on macOS 26 and 15 draws a
//! COLRv1 glyph as nothing while reporting the face as a colour one; on 27.2
//! it draws a COLRv0 face's glyphs as nothing. Chrome hands neither to
//! CoreText: `WebFontTypefaceFactory` in Blink (`web_font_typeface_factory.cc`)
//! makes every COLR web font, of either version, through Fontations on Apple
//! platforms. On Windows DirectWrite draws the COLRv1 test glyphs up to 215
//! levels inside them from Chrome's. On Linux FreeType draws them, with
//! their axes varied, up to 179 levels inside, and inks 412 pixels of the
//! COLRv0 test sheet more than 81 levels from Chrome's, where this painter
//! inks none more than 8 from it. So on every platform this module walks a
//! glyph's paint graph with skrifa, Fontations' own reader, and records it
//! as a [`Picture`], and the text routes draw that picture in place of the
//! platform's glyph -- see `node::typography::draw_colour_run`.
//!
//! What each callback draws follows Skia's painter for Fontations
//! (`ColorPainter` in `skia/src/ports/SkTypeface_fontations.cpp` under the
//! skia-bindings crate), which is what Chrome draws with: the graph recorded
//! with y turned down and every coordinate turned with it, gradients
//! interpolated unpremultiplied in sRGB, composite modes as Skia's blend modes
//! of the same names, clips anti-aliased, a palette index past the palette in
//! magenta, and each glyph composited onto the page through a layer of its
//! own in the paint it is drawn with. The code is this crate's; the behaviour
//! is Fontations', so that the two draw the same glyph alike.

#[cfg(test)]
use std::cell::Cell;
use std::{cell::RefCell, iter::zip, rc::Rc};

use crate::{
    cache_map::{CacheMap, CacheSet},
    export::{VectorFeatures, blend_features},
    started,
};

#[cfg(any(feature = "metal", feature = "vulkan"))]
use skia_safe::gpu::{
    Budgeted, DirectContextId, Mipmapped, images::texture_from_image,
};
use skia_safe::{
    AlphaType, BlendMode, Canvas, ClipOp, Color, Color4f, ColorSpace,
    ColorType, Data, GlyphId, IPoint, IRect, ISize, Image, ImageFilter,
    ImageInfo, Matrix, Paint, Path, PathBuilder, Picture, PictureRecorder,
    Point, RSXform, Rect, RoundOut, SamplingOptions, Shader, TileMode,
    Typeface,
    canvas::SaveLayerRec,
    color_filters,
    gradient::{
        Colors as GradientColors, Gradient, Interpolation, interpolation,
        shaders as gradient_shaders,
    },
    typeface::TypefaceId,
};
use skrifa::{
    FontRef, GlyphId as SkrifaGlyphId, MetadataProvider,
    OutlineGlyphCollection, Tag,
    color::{
        Brush, ColorGlyphCollection, ColorPainter, ColorStop, CompositeMode,
        Extend, Transform,
    },
    instance::{Location, LocationRef, Size},
    outline::{DrawSettings, OutlinePen},
    raw::{
        TableDirectory, TableRecord,
        types::{BoundingBox, FixedSize},
    },
};

/// The palette a glyph is drawn in: the font's first, which CSS Fonts 4's
/// `font-palette: normal` names and which is all this crate offers.
const DEFAULT_PALETTE: u16 = 0;

/// The palette index a `COLR` paint names the text's own colour by: OpenType
/// 1.9, "CPAL", "palette entry index 0xFFFF ... the text foreground color".
const FOREGROUND_INDEX: u16 = 0xFFFF;

/// How far from the origin, in design units, the recording canvas reaches
/// along each axis. A `COLR` coordinate is an `FWORD`, an `i16`, so a glyph
/// lies within 2^15 units, and transforms in the graph can carry it a few
/// times further; 2^20 bounds that with room and is exact in `f32`.
const RECORDING_REACH: f32 = (1u32 << 20) as f32;

/// How many faces a thread keeps the `COLR` answer for before it drops them
/// all and asks again -- a bound, since a variable font makes a typeface
/// for every instance asked for.
const FACES_KEPT: usize = 64;

/// How many recorded glyphs a thread keeps before it drops them all and
/// records again. A glyph's picture is a few hundred bytes to a few
/// kilobytes; the key holds a foreground colour, which can be any colour,
/// so text whose colour animates would add one a frame.
const GLYPHS_KEPT: usize = 2048;

/// How many bytes of glyph images a thread keeps before it drops them all
/// and makes them again -- see [`glyph_image`]. Skia's own glyph cache keeps
/// 2 MiB by default (`SK_DEFAULT_FONT_CACHE_LIMIT` in
/// `skia/src/core/SkGraphics.cpp`); colour glyphs are four bytes a pixel
/// where a mask is one, so this keeps four times that.
const GLYPH_IMAGE_BYTES_KEPT: usize = 8 << 20;

/// The longest side, in device pixels, of the text a colour glyph is kept
/// as an image for: past it, or under perspective, Skia draws a glyph as a
/// path or a drawable rather than from its glyph cache -- "a self-imposed
/// maximum, just to limit memory-usage" (`SkStrikeSpec::ShouldDrawAsPath` in
/// `skia/src/core/SkStrikeSpec.cpp`). A larger one is made as an image all
/// the same, so that it is drawn on the CPU wherever pixels are made, but
/// not kept.
const LARGEST_KEPT_TEXT: f32 = 256.0;

/// The widest and tallest, in pixels, an atlas of a run's glyph images is
/// made -- see [`atlas`]. Chosen, not fixed: an atlas this size is 4 MiB in
/// eight-bit RGBA, within [`ATLAS_BYTES_KEPT`], and a quarter of the 4096
/// every Vulkan device takes a texture to (`maxImageDimension2D` among the
/// Vulkan specification's required limits). A run whose images outgrow it
/// is drawn from as many atlases as it fills.
const ATLAS_SIDE: i32 = 1024;

/// How many bytes of run atlases a thread keeps before it drops them all
/// and makes them again -- see [`atlas`]: as many as the glyph images they
/// are copied from, [`GLYPH_IMAGE_BYTES_KEPT`].
const ATLAS_BYTES_KEPT: usize = GLYPH_IMAGE_BYTES_KEPT;

/// How many bytes a pixel format's [`Sheet`] holds at most: as many as a
/// thread's run atlases together, so that a sheet of a format eight bytes a
/// pixel or more is shorter than it is wide.
const SHEET_BYTES_KEPT: usize = ATLAS_BYTES_KEPT;

/// The tables a face's paint graphs and the outlines they clip to are read
/// from, where the platform cannot hand the font's own file back -- see
/// [`ColrFace::of`].
const TABLES: [&[u8; 4]; 14] = [
    b"head", b"hhea", b"maxp", b"hmtx", b"COLR", b"CPAL", b"fvar", b"avar",
    b"glyf", b"loca", b"gvar", b"CFF ", b"CFF2", b"HVAR",
];

/// A typeface with a `COLR` table: its font file, the place on its axes it is
/// drawn at, and its palette.
pub(crate) struct ColrFace {
    typeface: Typeface,
    /// The font file, and which face in it this is.
    file: Data,
    index: u32,
    /// The typeface's place on the font's axes, normalised.
    location: Location,
    /// The colours of [`DEFAULT_PALETTE`], as Skia's 8-bit colours.
    palette: Vec<Color>,
    /// The face's units per em, which its paint graphs are stated in.
    upem: f32,
}

impl ColrFace {
    /// The `COLR` face `typeface` is, or `None` where it has no `COLR`
    /// table.
    ///
    /// The font's own file where the platform still holds it -- a face made
    /// from data has it -- and otherwise one assembled from the tables the
    /// graphs and their outlines are read from, since skrifa reads a file and
    /// a system font's file is not handed back.
    fn of(typeface: &Typeface) -> Option<Self> {
        typeface.copy_table_data(u32::from_be_bytes(*b"COLR"))?;
        let (file, index) = typeface
            .to_existing_font_data()
            .unwrap_or_else(|| (tables_of(typeface), 0));
        Self::read(typeface, file, index)
    }

    /// The face `typeface` is, read from `file`, face `index` within it.
    fn read(typeface: &Typeface, file: Data, index: u32) -> Option<Self> {
        let font = FontRef::from_index(file.as_bytes(), index).ok()?;
        let location = font.axes().location(
            typeface
                .variation_design_position()
                .unwrap_or_default()
                .into_iter()
                .map(|coordinate| {
                    (Tag::from_u32(*coordinate.axis), coordinate.value)
                }),
        );
        let palette = font
            .color_palettes()
            .get(DEFAULT_PALETTE)
            .map(|palette| {
                palette
                    .colors()
                    .iter()
                    .map(|colour| {
                        Color::from_argb(
                            colour.alpha,
                            colour.red,
                            colour.green,
                            colour.blue,
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        let upem = typeface.units_per_em().filter(|upem| *upem > 0)?;
        Some(Self {
            typeface: typeface.clone(),
            file,
            index,
            location,
            palette,
            upem: upem as f32,
        })
    }

    /// Records `glyph`'s paint graph with `foreground` as the text's colour,
    /// or `None` where the table has no paint for it.
    fn record(&self, glyph: GlyphId, foreground: Color) -> Option<ColrGlyph> {
        let font =
            FontRef::from_index(self.file.as_bytes(), self.index).ok()?;
        let colour = ColorGlyphCollection::new(&font)
            .get(SkrifaGlyphId::new(u32::from(glyph)))?;
        let mut recorder = PictureRecorder::new();
        let reach = Rect::from_ltrb(
            -RECORDING_REACH,
            -RECORDING_REACH,
            RECORDING_REACH,
            RECORDING_REACH,
        );
        let canvas = recorder.begin_recording(reach, false);
        let mut painter = Recorder {
            canvas,
            outlines: font.outline_glyphs(),
            location: LocationRef::from(&self.location),
            palette: &self.palette,
            foreground,
            clips: Vec::new(),
            inked: None,
            features: VectorFeatures::PLAIN,
        };
        let painted = colour.paint(&self.location, &mut painter).is_ok();
        let inked = painter.inked;
        let inner = painter.features;
        // Culled to what it inks rather than to the reach it was recorded
        // over: a picture is written as a form on Skia's PDF device with its
        // cull as the form's box, and one 2^20 units wide is past the
        // coordinates a PDF reader keeps.
        let picture = recorder
            .finish_recording_as_picture(Some(&inked.unwrap_or_default()))?;
        Some(ColrGlyph {
            picture: painted.then_some(picture),
            bounds: inked,
            // The glyph's own layer, and what is drawn inside it, which
            // that layer holds as a group.
            features: VectorFeatures::LAYER.with(inner).with(inner.nested()),
        })
    }
}

/// A font file holding the [`TABLES`] of `typeface` it has, for a face
/// whose own file the platform does not hand back.
fn tables_of(typeface: &Typeface) -> Data {
    let tables: Vec<(&[u8; 4], Data)> = TABLES
        .iter()
        .filter_map(|tag| {
            Some((*tag, typeface.copy_table_data(u32::from_be_bytes(**tag))?))
        })
        .collect();
    Data::new_copy(&assembled(&tables))
}

/// A font file holding `tables`, each tag beside its data, in the order an
/// sfnt's table directory sorts them: OpenType 1.9, "The OpenType font
/// file", "Table Directory". The checksums are left at zero, since nothing
/// here verifies them.
fn assembled(tables: &[(&[u8; 4], Data)]) -> Vec<u8> {
    /// The sfnt version of a font with TrueType or CFF outlines alike, as a
    /// reader takes it: `0x00010000`.
    const SFNT_VERSION: u32 = 0x0001_0000;
    let mut sorted: Vec<&(&[u8; 4], Data)> = tables.iter().collect();
    sorted.sort_by_key(|(tag, _)| **tag);
    let count = sorted.len();
    let mut file = Vec::new();
    file.extend_from_slice(&SFNT_VERSION.to_be_bytes());
    file.extend_from_slice(&(count as u16).to_be_bytes());
    // The search fields are hints for a binary search, which skrifa does not
    // make; zero is a valid table directory to it.
    file.extend_from_slice(&[0; 6]);
    // The table directory before its records, then a record a table: their
    // sizes as read-fonts reads them, fixed by the format.
    let mut at = TableDirectory::MIN_SIZE + TableRecord::RAW_BYTE_LEN * count;
    for (tag, data) in &sorted {
        file.extend_from_slice(*tag);
        file.extend_from_slice(&0u32.to_be_bytes());
        file.extend_from_slice(&(at as u32).to_be_bytes());
        file.extend_from_slice(&(data.len() as u32).to_be_bytes());
        // Each table starts on a four-byte boundary.
        at += data.len().next_multiple_of(4);
    }
    for (_, data) in &sorted {
        file.extend_from_slice(data.as_bytes());
        file.resize(file.len().next_multiple_of(4), 0);
    }
    file
}

/// One glyph's paint graph, recorded in design units with y turned down --
/// see [`down`].
pub(crate) struct ColrGlyph {
    /// The recording, or `None` where the graph failed to read part-way --
    /// a cycle, an offset out of the table -- and draws nothing, as
    /// Fontations draws nothing for a glyph it cannot read.
    picture: Option<Picture>,
    /// What the recording can ink, in its own units: the union of what each
    /// fill covered within its clips. `None` where it inks nothing.
    bounds: Option<Rect>,
    /// What drawing the glyph asks of a vector backend: the layer it is
    /// composited through, the sweep gradients in it, which SVG names no
    /// paint server for, and the blends its composite modes write -- so that
    /// a document a backend cannot write it into embeds it as pixels -- see
    /// `export::VectorFeatures`.
    features: VectorFeatures,
}

/// The `COLR` face `typeface` is, kept for the next ask on this thread, or
/// `None` where it has no `COLR` table.
pub(crate) fn colr_face(typeface: &Typeface) -> Option<Rc<ColrFace>> {
    FACES.with_borrow_mut(|faces| {
        let id = typeface.unique_id();
        if let Some(face) = faces.get(&id) {
            return face.clone();
        }
        if faces.len() >= FACES_KEPT {
            faces.clear();
        }
        let face = ColrFace::of(typeface).map(Rc::new);
        faces.insert(id, face.clone());
        face
    })
}

/// `glyph` of `face` recorded with `foreground` as the text's colour, kept
/// for the next ask on this thread; `None` where the face has no colour
/// paint for it, for the platform to draw.
fn colr_glyph(
    face: &ColrFace,
    glyph: GlyphId,
    foreground: Color,
) -> Option<Rc<ColrGlyph>> {
    GLYPHS.with_borrow_mut(|glyphs| {
        let key = (face.typeface.unique_id(), glyph, packed(foreground));
        if let Some(recorded) = glyphs.get(&key) {
            return recorded.clone();
        }
        if glyphs.len() >= GLYPHS_KEPT {
            glyphs.clear();
        }
        let recorded = face.record(glyph, foreground).map(Rc::new);
        glyphs.insert(key, recorded.clone());
        recorded
    })
}

/// `colour` packed as an `SkColor` is: ARGB, eight bits each.
fn packed(colour: Color) -> u32 {
    u32::from_be_bytes([colour.a(), colour.r(), colour.g(), colour.b()])
}

/// Drops every face and glyph this thread keeps.
pub(crate) fn forget() {
    FACES.with_borrow_mut(|faces| faces.clear());
    GLYPHS.with_borrow_mut(|glyphs| glyphs.clear());
    GLYPH_IMAGES.with_borrow_mut(|(images, held)| {
        images.clear();
        *held = 0;
    });
    drop_atlases();
    release_textures();
}

thread_local! {
    /// The answer [`colr_face`] gave for each typeface, by its unique id,
    /// which no other typeface in the process takes.
    static FACES: RefCell<CacheMap<TypefaceId, Option<Rc<ColrFace>>>> =
        RefCell::new(CacheMap::default());
    /// The glyphs [`colr_glyph`] recorded, by typeface, glyph and
    /// foreground colour -- see [`packed`].
    static GLYPHS: RefCell<CacheMap<GlyphKey, Option<Rc<ColrGlyph>>>> =
        RefCell::new(CacheMap::default());
}

/// What a recorded glyph is kept by: its typeface's unique id, the glyph,
/// and the foreground colour it was recorded with -- see [`packed`].
type GlyphKey = (TypefaceId, GlyphId, u32);

/// A run of glyphs in a face with a `COLR` table, drawn a glyph at a time:
/// its colour glyphs from their paint graphs, and the rest left to the
/// caller to draw as the platform would.
pub(crate) struct ColourRun {
    face: Rc<ColrFace>,
    /// Design units, as recorded with y down, to the run's space.
    to_run: Matrix,
    /// The colour the graph's foreground paints take.
    foreground: Color,
}

impl ColourRun {
    /// The run `font` draws in `paint`, or `None` where its face has no
    /// `COLR` table.
    ///
    /// The paint's colour is the foreground as an 8-bit `SkColor`, as a
    /// scaler context takes it (`SkScalerContextRec::fForegroundColor`):
    /// opaque black under a gradient or a pattern, whose colour Chrome's
    /// canvas sets so.
    pub(crate) fn of(font: &skia_safe::Font, paint: &Paint) -> Option<Self> {
        let face = colr_face(&font.typeface())?;
        let scale = font.size() / face.upem;
        // Skia's text matrix for the font -- its size, its horizontal scale
        // and its skew -- over a recording already turned y down.
        let to_run = Matrix::new_all(
            scale * font.scale_x(),
            scale * font.skew_x(),
            0.0,
            0.0,
            scale,
            0.0,
            0.0,
            0.0,
            1.0,
        );
        Some(Self {
            face,
            to_run,
            foreground: paint.color(),
        })
    }

    /// `glyph` as this run draws it, or `None` where it is not a colour
    /// glyph, for the caller to draw as the platform would.
    pub(crate) fn glyph(&self, glyph: GlyphId) -> Option<Rc<ColrGlyph>> {
        colr_glyph(&self.face, glyph, self.foreground)
    }

    /// What drawing `glyph` asks of a vector backend, or `None` where it is
    /// not a colour glyph -- see [`ColrGlyph::features`].
    pub(crate) fn features(&self, glyph: GlyphId) -> Option<VectorFeatures> {
        Some(self.glyph(glyph)?.features)
    }

    /// What `glyph` can ink with its origin at the run's, in the run's
    /// space -- empty for a colour glyph that inks nothing -- or `None` where
    /// it is not a colour glyph. Drawn, a glyph is placed by [`snapped`],
    /// which can move it by up to a device pixel; that is within the device
    /// margin a reach is given (`layer::HINTED_INK_DRIFT`).
    pub(crate) fn ink(&self, glyph: GlyphId) -> Option<Rect> {
        let recorded = self.glyph(glyph)?;
        Some(recorded.bounds.map_or_else(Rect::new_empty, |bounds| {
            self.to_run.map_rect(bounds).0
        }))
    }

    /// Draws `glyph`, as [`Self::glyph`] gives it, with its origin at `at`
    /// onto `canvas` in `paint`.
    ///
    /// The glyph is composited through a layer of its own, bounded by what
    /// it can ink and made with `paint`, as Skia composites a glyph it draws
    /// as a drawable (`DrawableOpSubmitter::submitDraws` in
    /// `skia/src/text/gpu/SubRunContainer.cpp`, and the raster device's
    /// counterpart in `SkGlyphRunPainter.cpp`): `globalAlpha`, the composite
    /// operation and a filter apply to the glyph whole, and the glyph's own
    /// composite modes see none of the page beneath it. That isolation is
    /// not cosmetic: drawn straight onto the page, the test font's glyphs
    /// come out up to 207 levels apart on a transparent one. While an
    /// [`ImageRoute`] is open the layer's contents are an image made on the
    /// CPU instead -- see [`Self::draw_image`] -- composited as the layer
    /// would be.
    pub(crate) fn draw(
        &self,
        canvas: &Canvas,
        glyph: &ColrGlyph,
        at: Point,
        paint: &Paint,
    ) {
        let (Some(picture), Some(bounds)) = (&glyph.picture, glyph.bounds)
        else {
            return;
        };
        let mut placed = Matrix::translate(snapped(canvas, at));
        placed.pre_concat(&self.to_run);
        if let Some(format) = ROUTE.with_borrow(Clone::clone)
            && self.draw_image(canvas, picture, bounds, &placed, paint, &format)
        {
            return;
        }
        canvas.save_layer(
            &SaveLayerRec::default()
                .bounds(&placed.map_rect(bounds).0)
                .paint(paint),
        );
        canvas.draw_picture(picture, Some(&placed), None);
        canvas.restore();
    }

    /// Draws `glyphs`, each with its origin, onto `canvas` in `paint`, in
    /// order, as [`Self::draw`] draws each.
    ///
    /// While an [`ImageRoute`] is open on a GPU canvas, the glyph images
    /// under a paint that blends by coefficients -- see
    /// [`blends_by_coefficients`] -- and carries no image filter are drawn
    /// together: copied into one image, an atlas, and drawn from it with one
    /// `drawAtlas` -- see [`Self::draw_images`]. Ganesh draws each image it is
    /// handed as an operation of its own, which it cannot merge with the
    /// next where the two read different textures: drawn an image apiece, a
    /// page of 32 runs of 16 glyphs was 512 texture operations, and merging,
    /// binding and drawing them cost about 0.06 milliseconds a page. Drawn
    /// from an atlas, a glyph under a clip edge at a fraction of a pixel
    /// stands from the CPU canvas as one unclipped does, where an image of
    /// its own stands up to 22 levels from it, and 11 at half alpha.
    ///
    /// Under any other paint each glyph keeps its own image. A blend past
    /// the coefficients reads the page in the shader from one copy for each
    /// draw, so the overlapping sprites of one atlas would each read the
    /// page as it was before the others landed, as much as 197 levels from
    /// the CPU canvas; an image filter is applied to a glyph alone -- see
    /// [`filtered`]. The CPU canvas draws each image on its own: its raster
    /// draws an atlas's sprites through an image shader, sampling each
    /// texel, where it blends an image drawn at a whole pixel a row at a
    /// time -- 0.043 milliseconds more a page of 512 glyphs -- and applies
    /// an atlas's alpha up to two levels from an image's, which is Chrome's
    /// bytes.
    pub(crate) fn draw_all(
        &self,
        canvas: &Canvas,
        glyphs: &[(Rc<ColrGlyph>, Point)],
        paint: &Paint,
    ) {
        let atlased = on_gpu(canvas)
            && blends_by_coefficients(paint.blend_mode_or(BlendMode::SrcOver));
        if paint.image_filter().is_none()
            && let Some(format) = ROUTE.with_borrow(Clone::clone)
            && self.draw_images(canvas, glyphs, paint, &format, atlased)
        {
            return;
        }
        for (glyph, at) in glyphs {
            self.draw(canvas, glyph, *at, paint);
        }
    }

    /// Draws `glyphs`, each with its origin, as images made on the CPU in
    /// `format`, onto `canvas` in `paint`, which carries no image filter:
    /// where `atlased`, from the [`Sheet`] where every one of them lies on
    /// it and otherwise from atlases they are copied into, and each on its
    /// own where not, all under one device transform rather than one set up
    /// and torn down for each glyph -- which cost the CPU canvas about 0.045
    /// milliseconds a page of 512 glyphs. `false`, having drawn nothing,
    /// under a perspective, or where the glyphs are past
    /// [`LARGEST_KEPT_TEXT`] and their images are not kept.
    ///
    /// Each image is drawn as [`Self::draw_image`] draws it: at the whole
    /// device pixel its glyph's layer would have landed on, sampled at the
    /// nearest texel, so each pixel reads one texel of the image as drawing
    /// the image there reads it, through `paint`'s alpha, colour filter and
    /// blend. An
    /// atlas's sprites are drawn in order, so where two glyphs overlap the
    /// later lies on top as it did drawn on its own.
    fn draw_images(
        &self,
        canvas: &Canvas,
        glyphs: &[(Rc<ColrGlyph>, Point)],
        paint: &Paint,
        format: &Format,
        atlased: bool,
    ) -> bool {
        #[cfg(test)]
        if ONE_BY_ONE.get() {
            return false;
        }
        let device = canvas.local_to_device_as_3x3();
        if device.has_perspective() {
            return false;
        }
        // The device transform less its translation, which every glyph of
        // the run shares: a glyph is placed by a translation alone.
        let mut shape = device;
        shape.pre_concat(&self.to_run);
        shape.set_translate_x(0.0).set_translate_y(0.0);
        if self.largest(&shape) > LARGEST_KEPT_TEXT {
            return false;
        }
        let placed: Vec<(Image, IPoint)> = glyphs
            .iter()
            .filter_map(|(glyph, at)| {
                let (Some(picture), Some(bounds)) =
                    (&glyph.picture, glyph.bounds)
                else {
                    return None;
                };
                let origin = snapped_on_device(&device, *at);
                let (image, offset) =
                    glyph_image(picture, bounds, &shape, format, true)?;
                Some((image, origin + offset))
            })
            .collect();
        let composite = composite(paint);
        let one = |image: &Image, at: IPoint| {
            let image = resident(canvas, image.clone());
            noted(canvas, &image);
            composite_onto(canvas, &image, at, &composite);
        };
        canvas.save();
        canvas.reset_matrix();
        if !atlased {
            placed.iter().for_each(|(image, at)| one(image, *at));
            canvas.restore();
            return true;
        }
        if let Some((image, sprites)) = sheet_sprites(&placed, format) {
            let image = resident(canvas, image);
            noted(canvas, &image);
            let moved: Vec<RSXform> = placed
                .iter()
                .map(|(_, at)| {
                    RSXform::new(1.0, 0.0, (at.x as f32, at.y as f32))
                })
                .collect();
            canvas.draw_atlas(
                &image,
                &moved,
                &sprites,
                None,
                BlendMode::Dst,
                SamplingOptions::default(),
                None,
                &composite,
            );
            #[cfg(test)]
            {
                ATLAS_DRAWS.set(ATLAS_DRAWS.get() + 1);
                SHEET_DRAWS.set(SHEET_DRAWS.get() + 1);
            }
            canvas.restore();
            return true;
        }
        let mut start = 0;
        while start < placed.len() {
            let end = start + fits(&placed[start..]);
            let chunk = &placed[start..end];
            start = end;
            // Within the bounds `fits` keeps to, only a raster that cannot
            // be had refuses an atlas; its glyphs are drawn one at a time.
            let Some((image, sprites)) = atlas(chunk, format) else {
                chunk.iter().for_each(|(image, at)| one(image, *at));
                continue;
            };
            let image = resident(canvas, image);
            noted(canvas, &image);
            let moved: Vec<RSXform> = chunk
                .iter()
                .map(|(_, at)| {
                    RSXform::new(1.0, 0.0, (at.x as f32, at.y as f32))
                })
                .collect();
            // With no colours given, the blend mode `drawAtlas` takes for
            // them is not read.
            canvas.draw_atlas(
                &image,
                &moved,
                &sprites,
                None,
                BlendMode::Dst,
                SamplingOptions::default(),
                None,
                &composite,
            );
            #[cfg(test)]
            ATLAS_DRAWS.set(ATLAS_DRAWS.get() + 1);
        }
        canvas.restore();
        true
    }

    /// The longest side, in device pixels, of the text `shape` -- the device
    /// transform less its translation, over the run's -- draws this run's
    /// face at: its em square's, mapped.
    fn largest(&self, shape: &Matrix) -> f32 {
        (shape.scale_x().hypot(shape.skew_y()))
            .max(shape.skew_x().hypot(shape.scale_y()))
            * self.face.upem
    }

    /// Draws the glyph `picture` records, inking within `bounds`, under
    /// `placed` as an image made on the CPU in `format`, at the transform
    /// `canvas` carries -- see [`ImageRoute`]. `false`, having drawn
    /// nothing, under a perspective, which an image cannot follow.
    ///
    /// The image is composited as the glyph's layer would be, at the whole
    /// device pixel the layer would have landed on, through `paint`'s alpha,
    /// colour filter and blend; a paint with an image filter, a shadow's, is
    /// applied to the image on the CPU first, under the transform the glyph
    /// is drawn under -- see [`filtered`].
    fn draw_image(
        &self,
        canvas: &Canvas,
        picture: &Picture,
        bounds: Rect,
        placed: &Matrix,
        paint: &Paint,
        format: &Format,
    ) -> bool {
        let mut device = canvas.local_to_device_as_3x3();
        if device.has_perspective() {
            return false;
        }
        // The space a paint's filter is given in: the canvas's, as the
        // glyph's layer has it, less where on the device it lands.
        let mut canvas_shape = device;
        canvas_shape.set_translate_x(0.0).set_translate_y(0.0);
        device.pre_concat(placed);
        // The origin `snapped` placed on a whole device pixel.
        let origin = IPoint::new(
            device.translate_x().round() as i32,
            device.translate_y().round() as i32,
        );
        let mut shape = device;
        shape.set_translate_x(0.0).set_translate_y(0.0);
        let keep = self.largest(&shape) <= LARGEST_KEPT_TEXT;
        let Some((image, offset)) =
            glyph_image(picture, bounds, &shape, format, keep)
        else {
            return true;
        };
        let (image, offset, composite) = match paint.image_filter() {
            Some(filter) => {
                match filtered(image, offset, &canvas_shape, paint, &filter) {
                    Some((image, offset)) => {
                        let mut composite = Paint::default();
                        composite.set_blend_mode(
                            paint.blend_mode_or(BlendMode::SrcOver),
                        );
                        (image, offset, composite)
                    }
                    None => return true,
                }
            }
            None => match keep {
                true => (resident(canvas, image), offset, composite(paint)),
                false => (image, offset, composite(paint)),
            },
        };
        noted(canvas, &image);
        canvas.save();
        canvas.reset_matrix();
        composite_onto(canvas, &image, origin + offset, &composite);
        canvas.restore();
        true
    }
}

/// Panics unless every character of `text` maps, in the font at `path`, to
/// a glyph its `COLR` table paints: a character the font lacks falls back
/// to a platform face and is drawn as plain text, so a test meaning to
/// draw colour glyphs would draw something else and pass for it.
#[cfg(test)]
pub(crate) fn assert_colour_text(path: &str, text: &str) {
    let bytes = std::fs::read(path).expect(path);
    let typeface = skia_safe::FontMgr::new()
        .new_from_data(Data::new_copy(&bytes), None)
        .expect("the font loads");
    let font = skia_safe::Font::from_typeface(&typeface, 16.0);
    let run = ColourRun::of(&font, &Paint::default()).expect("a COLR face");
    for c in text.chars() {
        let glyph = typeface.unichar_to_glyph(c as i32);
        let painted = glyph != 0
            && run
                .glyph(glyph)
                .is_some_and(|glyph| glyph.picture.is_some());
        assert!(
            painted,
            "U+{:04X} is no colour glyph of {path}: a fallback face draws it",
            u32::from(c)
        );
    }
}

/// Whether `canvas` draws on a GPU context.
#[cfg(any(feature = "metal", feature = "vulkan"))]
fn on_gpu(canvas: &Canvas) -> bool {
    canvas.direct_context().is_some()
}

/// Whether `canvas` draws on a GPU context: never, without a GPU engine.
#[cfg(not(any(feature = "metal", feature = "vulkan")))]
fn on_gpu(_canvas: &Canvas) -> bool {
    false
}

/// Whether `mode` is one Skia blends by a coefficient of each colour --
/// one up to `SkBlendMode::kLastCoeffMode`, `kScreen`, in
/// `include/core/SkBlendMode.h` -- which a GPU blends in its fixed-function
/// stage, each fragment over what the one before it wrote. Past it, Ganesh
/// reads the page beneath in the shader, from a copy it takes once for a
/// draw.
fn blends_by_coefficients(mode: BlendMode) -> bool {
    mode as i32 <= BlendMode::Screen as i32
}

/// Counts `image`, about to be drawn on `canvas`, among the raster glyph
/// images drawn on a GPU canvas, each of which Skia uploads unless it still
/// holds the texture -- see `RASTERS_ON_GPU`. Nothing outside the tests.
#[cfg_attr(
    not(all(test, any(feature = "metal", feature = "vulkan"))),
    expect(unused_variables)
)]
fn noted(canvas: &Canvas, image: &Image) {
    #[cfg(all(test, any(feature = "metal", feature = "vulkan")))]
    if canvas.direct_context().is_some() && !image.is_texture_backed() {
        RASTERS_ON_GPU.set(RASTERS_ON_GPU.get() + 1);
    }
}

/// The paint a glyph image is composited onto the page in where `paint`
/// carries no image filter: `paint`'s blend, alpha and colour filter, as
/// the glyph's layer was composited through them.
fn composite(paint: &Paint) -> Paint {
    let mut composite = Paint::default();
    composite.set_blend_mode(paint.blend_mode_or(BlendMode::SrcOver));
    composite.set_alpha(paint.alpha());
    if let Some(filter) = paint.color_filter() {
        composite.set_color_filter(filter);
    }
    composite
}

/// Whether a glyph image composited onto a CPU canvas at full alpha goes
/// through [`IDENTITY_MATRIX`], which moves Skia's composite off its eight-bit
/// path onto its float pipeline. The eight-bit source-over rounds to nearest
/// on aarch64 and truncates on x86-64: over the test font's 183 colour glyph
/// images at 32px, an N32 image on an N32 page lands 14,573 of 282,272
/// channels a level under the exact composite on x86-64 and none on an
/// Apple M4 Pro, and through the identity matrix none on either. On aarch64
/// the float pipeline changes no byte and costs a page of 512 colour glyphs
/// about 0.55 ms, so it is left there.
const ROUNDS_THROUGH_FLOAT: bool = cfg!(not(target_arch = "aarch64"));

/// The identity colour matrix, row-major, as `color_filters::matrix_row_major`
/// takes it: four rows of five, each passing one channel through unscaled
/// and unbiased.
const IDENTITY_MATRIX: [f32; 20] = [
    1.0, 0.0, 0.0, 0.0, 0.0, //
    0.0, 1.0, 0.0, 0.0, 0.0, //
    0.0, 0.0, 1.0, 0.0, 0.0, //
    0.0, 0.0, 0.0, 1.0, 0.0, //
];

/// Composites `image` onto `canvas`, whose transform is the identity, with
/// its top left at the whole device pixel `at`, through `paint`: the exact
/// source-over rounded to nearest at full alpha on every architecture -- see
/// [`ROUNDS_THROUGH_FLOAT`]. Under an alpha the composite is Skia's
/// eight-bit one on the CPU, which is Chrome's CPU canvas's; a GPU canvas
/// blends in its own precision either way.
pub(crate) fn composite_onto(
    canvas: &Canvas,
    image: &Image,
    at: IPoint,
    paint: &Paint,
) {
    let point = (at.x as f32, at.y as f32);
    // A paint with a colour filter is on the float pipeline already.
    if ROUNDS_THROUGH_FLOAT
        && !on_gpu(canvas)
        && paint.alpha() == u8::MAX
        && paint.color_filter().is_none()
    {
        let mut rounded = paint.clone();
        rounded.set_color_filter(color_filters::matrix_row_major(
            &IDENTITY_MATRIX,
            color_filters::Clamp::Yes,
        ));
        canvas.draw_image(image, point, Some(&rounded));
        return;
    }
    canvas.draw_image(image, point, Some(paint));
}

/// Where each of `images` lies in an atlas of them, packed into rows from
/// the top left of a square [`ATLAS_SIDE`] across, each row as tall as its
/// tallest image, and how many of them, from the first, fit: at least one,
/// which is drawn on its own where it does not fit with another.
fn packed_sprites(images: &[(Image, IPoint)]) -> (Vec<IRect>, usize) {
    let (mut x, mut y, mut row) = (0, 0, 0);
    let mut sprites = Vec::with_capacity(images.len());
    for (image, _) in images {
        let (width, height) = (image.width(), image.height());
        if x + width > ATLAS_SIDE {
            (x, y, row) = (0, y + row, 0);
        }
        if x + width > ATLAS_SIDE || y + height > ATLAS_SIDE {
            break;
        }
        sprites.push(IRect::from_xywh(x, y, width, height));
        x += width;
        row = row.max(height);
    }
    let fitting = sprites.len().max(1);
    (sprites, fitting)
}

/// How many of `images`, from the first, one atlas holds -- see
/// [`packed_sprites`].
fn fits(images: &[(Image, IPoint)]) -> usize {
    packed_sprites(images).1
}

/// The glyph images of one pixel format a thread has drawn on GPU canvases
/// more than once, copied into one image, so that the runs drawn from it
/// read one texture and Ganesh can merge their `drawAtlas` calls into one
/// draw. Drawn from a run's own atlas instead, each run read a texture of
/// its own, which a page of 32 runs encoded as 32 draws.
///
/// The image is never written once made: a texture that draws not yet
/// executed read must not change under them. Adding glyphs makes a new
/// generation, the last one copied and the new glyphs copied in, and the
/// textures made of the last one are let go -- see [`grow_sheets`].
struct Sheet {
    /// The format the images on the sheet are in.
    format: Format,
    /// Where each image on the sheet lies, by its unique id.
    at: CacheMap<u32, IRect>,
    /// The current generation, `None` before the first glyph is added.
    image: Option<Image>,
    /// Where the next image goes, packed in rows from the top left as
    /// [`packed_sprites`] packs a run's atlas, and how tall its row is.
    next: IPoint,
    row: i32,
    /// Whether an image has failed to fit, after which none is added.
    full: bool,
    /// The images drawn once, by unique id, not on the sheet: an image
    /// drawn again is added, so that a page whose glyphs are new every
    /// time -- in a colour of their own on every line, say -- adds none.
    seen: CacheSet<u32>,
    /// The images drawn again while not on the sheet, in the order they
    /// were drawn, to add at the next [`grow_sheets`], and their ids.
    wanted: Vec<Image>,
    wanted_ids: CacheSet<u32>,
}

thread_local! {
    /// The sheet of each pixel format this thread has drawn in.
    static SHEETS: RefCell<Vec<Sheet>> = const { RefCell::new(Vec::new()) };
}

#[cfg(test)]
thread_local! {
    /// How many generations of sheets [`grow_sheets`] has made on this
    /// thread.
    static SHEET_GENERATIONS: Cell<usize> = const { Cell::new(0) };
    /// How many runs [`ColourRun::draw_images`] has drawn from a sheet on
    /// this thread.
    static SHEET_DRAWS: Cell<usize> = const { Cell::new(0) };
    /// How many rows deep the tests let a sheet grow, where they set it.
    static SHEET_DEPTH: Cell<Option<i32>> = const { Cell::new(None) };
}

/// The sheet `images` all lie on for `format`, and the rectangle each takes
/// in it; `None` where one of them is not on it, which is noted so that an
/// image drawn again is added at the next [`grow_sheets`].
fn sheet_sprites(
    images: &[(Image, IPoint)],
    format: &Format,
) -> Option<(Image, Vec<Rect>)> {
    SHEETS.with_borrow_mut(|sheets| {
        let at = match sheets.iter().position(|sheet| {
            sheet.format.color_type == format.color_type
                && sheet.format.space == format.space
        }) {
            Some(at) => at,
            None => {
                sheets.push(Sheet {
                    format: format.clone(),
                    at: CacheMap::default(),
                    image: None,
                    next: IPoint::new(0, 0),
                    row: 0,
                    full: false,
                    seen: CacheSet::default(),
                    wanted: Vec::new(),
                    wanted_ids: CacheSet::default(),
                });
                sheets.len() - 1
            }
        };
        let sheet = &mut sheets[at];
        if let Some(image) = &sheet.image {
            let sprites: Option<Vec<Rect>> = images
                .iter()
                .map(|(image, _)| sheet.at.get(&image.unique_id()).copied())
                .map(|rect| rect.map(Rect::from))
                .collect();
            if let Some(sprites) = sprites {
                return Some((image.clone(), sprites));
            }
        }
        if !sheet.full {
            for (image, _) in images {
                let id = image.unique_id();
                if sheet.at.contains_key(&id) || sheet.wanted_ids.contains(&id)
                {
                    continue;
                }
                if !sheet.seen.insert(id) {
                    sheet.wanted_ids.insert(id);
                    sheet.wanted.push(image.clone());
                }
            }
        }
        None
    })
}

/// Adds to each sheet the images drawn again since the last call, in one
/// new generation -- see [`Sheet`]. Called where a list of entries starts
/// to play on pixels, so that a page adds what it wants at most once, not
/// once for each run: the first page that draws a glyph again adds every
/// one it drew again, and the next draws them all from the sheet.
///
/// An image that does not fit leaves the sheet full: no more are added
/// until the sheets are dropped with the atlases -- see [`drop_atlases`].
pub(crate) fn grow_sheets() {
    SHEETS.with_borrow_mut(|sheets| {
        for sheet in sheets.iter_mut().filter(|sheet| !sheet.wanted.is_empty())
        {
            grow(sheet);
        }
    });
}

/// Adds `sheet`'s wanted images to it in a new generation -- see
/// [`grow_sheets`].
fn grow(sheet: &mut Sheet) {
    let wanted = std::mem::take(&mut sheet.wanted);
    sheet.wanted_ids.clear();
    let pixel = sheet.format.image_info(ISize::new(1, 1)).bytes_per_pixel();
    let deepest = (SHEET_BYTES_KEPT / (ATLAS_SIDE as usize * pixel))
        .min(ATLAS_SIDE as usize) as i32;
    #[cfg(test)]
    let deepest = SHEET_DEPTH.get().unwrap_or(deepest);
    let mut added = Vec::with_capacity(wanted.len());
    for image in wanted {
        let (width, height) = (image.width(), image.height());
        let mut next = sheet.next;
        let mut row = sheet.row;
        if next.x + width > ATLAS_SIDE {
            next = IPoint::new(0, next.y + row);
            row = 0;
        }
        if next.x + width > ATLAS_SIDE || next.y + height > deepest {
            sheet.full = true;
            break;
        }
        let rect = IRect::from_xywh(next.x, next.y, width, height);
        sheet.next = IPoint::new(next.x + width, next.y);
        sheet.row = row.max(height);
        added.push((image, rect));
    }
    if added.is_empty() {
        return;
    }
    let size = ISize::new(ATLAS_SIDE, sheet.next.y + sheet.row);
    let Some(mut surface) =
        started::raster(&sheet.format.image_info(size), None, None)
    else {
        sheet.full = true;
        return;
    };
    let canvas = surface.canvas();
    canvas.clear(Color::TRANSPARENT);
    let mut copy = Paint::default();
    copy.set_blend_mode(BlendMode::Src);
    if let Some(last) = &sheet.image {
        canvas.draw_image(last, (0.0, 0.0), Some(&copy));
    }
    for (image, rect) in &added {
        canvas.draw_image(
            image,
            (rect.left as f32, rect.top as f32),
            Some(&copy),
        );
    }
    if let Some(last) = sheet.image.replace(surface.image_snapshot()) {
        release_texture_of(last.unique_id());
    }
    for (image, rect) in added {
        sheet.at.insert(image.unique_id(), rect);
    }
    #[cfg(test)]
    SHEET_GENERATIONS.set(SHEET_GENERATIONS.get() + 1);
}

/// What a run's atlas is kept by: the unique ids of the glyph images it
/// holds, in the order its sprites hold them. A kept glyph image is kept
/// until every one goes at once, and its id is taken by no other image in
/// the process, so the ids name the pixels.
type AtlasKey = Box<[u32]>;

/// Run atlases by [`AtlasKey`], each with the rectangles its sprites take,
/// and how many bytes they hold together.
type KeptAtlases = (CacheMap<AtlasKey, (Image, Rc<[Rect]>)>, usize);

thread_local! {
    /// The atlases [`atlas`] made, and how many bytes they hold.
    static ATLASES: RefCell<KeptAtlases> =
        RefCell::new((CacheMap::default(), 0));
}

#[cfg(test)]
thread_local! {
    /// How many atlases [`atlas`] has made on this thread.
    static ATLASES_MADE: Cell<usize> = const { Cell::new(0) };
    /// How many atlases [`ColourRun::draw_images`] has drawn from on this
    /// thread.
    static ATLAS_DRAWS: Cell<usize> = const { Cell::new(0) };
    /// Whether [`ColourRun::draw_images`] declines, so that the tests can
    /// draw the same glyphs an image at a time to hold the two alike.
    static ONE_BY_ONE: Cell<bool> = const { Cell::new(false) };
}

/// Drops every atlas and [`Sheet`] this thread keeps.
fn drop_atlases() {
    ATLASES.with_borrow_mut(|(atlases, held)| {
        atlases.clear();
        *held = 0;
    });
    SHEETS.set(Vec::new());
}

/// `images`, glyph images [`glyph_image`] keeps, copied into one image in
/// `format`, and the rectangle each takes in it, made once for that list of
/// images in that order and kept within [`ATLAS_BYTES_KEPT`]; `None` where
/// they do not all fit in one -- see [`packed_sprites`] -- or a raster
/// cannot be had. Copied, not drawn: each image's pixels are the atlas's
/// pixels in its rectangle, byte for byte, and transparent around them. A
/// single image is its own atlas.
fn atlas(
    images: &[(Image, IPoint)],
    format: &Format,
) -> Option<(Image, Rc<[Rect]>)> {
    if let [(image, _)] = images {
        let whole = Rect::from_iwh(image.width(), image.height());
        return Some((image.clone(), Rc::from([whole])));
    }
    let key: AtlasKey =
        images.iter().map(|(image, _)| image.unique_id()).collect();
    if let Some(made) =
        ATLASES.with_borrow(|(atlases, _)| atlases.get(&key).cloned())
    {
        return Some(made);
    }
    let (sprites, _) = packed_sprites(images);
    if sprites.len() < images.len() {
        return None;
    }
    let size = sprites.iter().fold(ISize::new(0, 0), |size, sprite| {
        ISize::new(size.width.max(sprite.right), size.height.max(sprite.bottom))
    });
    let info = format.image_info(size);
    let mut surface = started::raster(&info, None, None)?;
    let canvas = surface.canvas();
    canvas.clear(Color::TRANSPARENT);
    let mut copy = Paint::default();
    copy.set_blend_mode(BlendMode::Src);
    for ((image, _), sprite) in zip(images, &sprites) {
        canvas.draw_image(
            image,
            (sprite.left as f32, sprite.top as f32),
            Some(&copy),
        );
    }
    let made: (Image, Rc<[Rect]>) = (
        surface.image_snapshot(),
        sprites.iter().map(|&sprite| Rect::from(sprite)).collect(),
    );
    #[cfg(test)]
    ATLASES_MADE.set(ATLASES_MADE.get() + 1);
    let bytes = info.compute_min_byte_size();
    ATLASES.with_borrow_mut(|(atlases, held)| {
        if *held + bytes > ATLAS_BYTES_KEPT {
            atlases.clear();
            *held = 0;
            release_textures();
        }
        atlases.insert(key, made.clone());
        *held += bytes;
    });
    Some(made)
}

/// `image`, whose top left lies `offset` from the glyph's origin on the
/// device, composited through `paint`'s alpha and colour filter and its
/// image filter `filter` onto nothing, on the CPU, the filter under `shape`
/// -- the canvas's transform less its translation, which the glyph's layer
/// is opened under -- and where the result's top left lies.
///
/// A shadow's paint carries a filter, which a glyph's layer evaluated on
/// the CPU canvas through Skia's raster pipeline, and the GPU in Ganesh: a
/// glyph's sharp shadow stood up to a level from the CPU canvas in 65 bytes
/// of the four test glyphs, and a blurred one up to six in 8,193. Applied
/// here, it leaves the CPU canvas's bytes as the layer left them, and an
/// image that composites as any image does, through the paint's blend
/// alone.
///
/// A paint without a filter is not applied here: its alpha and colour
/// filter composite the image as the glyph's layer did, which is what
/// Chrome's CPU canvas does byte for byte. Applied on the CPU instead, the
/// GPU canvas would draw the CPU canvas's bytes under a `globalAlpha`, and
/// the CPU canvas would stand a level from Chrome's in 2,326 of 118,800
/// channels at 0.5 -- closer to the exact composite there, and further
/// from it under `multiply`.
fn filtered(
    image: Image,
    offset: IPoint,
    shape: &Matrix,
    paint: &Paint,
    filter: &ImageFilter,
) -> Option<(Image, IPoint)> {
    let drawn =
        IRect::from_xywh(offset.x, offset.y, image.width(), image.height());
    // Where the filter can draw, in the canvas's units and back: the
    // filter's reach is given in them.
    let area = match shape.invert() {
        Some(inverse) => {
            let local = inverse.map_rect(Rect::from(drawn)).0;
            let mut reach = shape.map_rect(filter.compute_fast_bounds(local)).0;
            reach.join(Rect::from(drawn));
            reach.round_out()
        }
        None => drawn,
    };
    if area.is_empty() {
        return None;
    }
    let info = image.image_info().with_dimensions(area.size());
    let mut surface = started::raster(&info, None, None)?;
    let canvas = surface.canvas();
    canvas.translate((-area.left as f32, -area.top as f32));
    canvas.concat(shape);
    let mut through = Paint::default();
    through.set_alpha(paint.alpha());
    if let Some(colours) = paint.color_filter() {
        through.set_color_filter(colours);
    }
    through.set_image_filter(filter.clone());
    canvas.save_layer(&SaveLayerRec::default().paint(&through));
    canvas.reset_matrix();
    canvas.draw_image(
        &image,
        ((offset.x - area.left) as f32, (offset.y - area.top) as f32),
        None,
    );
    canvas.restore();
    Some((surface.image_snapshot(), IPoint::new(area.left, area.top)))
}

/// The pixel format a glyph image is made in: the colour type and colour
/// space of the pixels it is drawn for.
#[derive(Clone, Debug)]
pub(crate) struct Format {
    color_type: ColorType,
    color_space: Option<ColorSpace>,
    /// `color_space`'s hash, 0 for none: part of every glyph image's key,
    /// worked out once for the format rather than for each glyph.
    space: u64,
}

impl Format {
    /// The format of pixels of `color_type` in `color_space`.
    fn new(color_type: ColorType, color_space: Option<ColorSpace>) -> Self {
        let space = color_space.as_ref().map_or(0, ColorSpace::hash);
        Format {
            color_type,
            color_space,
            space,
        }
    }

    /// Pixels of `size` in this format, premultiplied.
    pub(crate) fn image_info(&self, size: ISize) -> ImageInfo {
        ImageInfo::new(
            size,
            self.color_type,
            AlphaType::Premul,
            self.color_space.clone(),
        )
    }

    /// The format of the pixels `canvas` draws onto, or `None` where it
    /// draws onto none: a recording, or a document.
    pub(crate) fn of(canvas: &Canvas) -> Option<Self> {
        let info = canvas.image_info();
        (info.color_type() != ColorType::Unknown)
            .then(|| Format::new(info.color_type(), info.color_space()))
    }
}

thread_local! {
    /// The format colour glyphs are drawn as images in while an
    /// [`ImageRoute`] is open on this thread, and `None`, the layer route,
    /// otherwise.
    static ROUTE: RefCell<Option<Format>> = const { RefCell::new(None) };
}

/// While it lives, colour glyphs drawn on this thread are drawn as images
/// made on the CPU in its format, at the transform of the canvas they are
/// drawn on, rather than composited through a layer from their paint
/// graphs: the route a run of text takes where a page is played onto pixels
/// -- see `layer::ColourText`. Skia draws a colour glyph the same way, from
/// an image its scaler context made on the CPU (`generateImage` in
/// `skia/src/ports/SkTypeface_fontations.cpp`), on a raster device and a GPU
/// one alike.
pub(crate) struct ImageRoute(Option<Format>);

impl ImageRoute {
    /// Opens the image route in `format` until the guard is dropped.
    pub(crate) fn open(format: Format) -> Self {
        ImageRoute(ROUTE.replace(Some(format)))
    }
}

impl Drop for ImageRoute {
    fn drop(&mut self) {
        ROUTE.set(self.0.take());
    }
}

/// What a glyph image is kept by: the glyph's recording -- its typeface,
/// which carries the place on the font's axes, its glyph and its foreground
/// colour, as [`colr_glyph`] keys it -- the device transform it is drawn
/// under, without its translation, and the pixel format. The palette is not
/// in the key: every glyph is drawn in [`DEFAULT_PALETTE`], since no canvas
/// API selects a `CPAL` palette today; a `font-palette` feature would add
/// it here.
type ImageKey = (u32, [u32; 4], i32, u64);

/// Glyph images by [`ImageKey`], each with where its top left lands from
/// the glyph's origin, and how many bytes they hold together.
type KeptImages = (CacheMap<ImageKey, (Image, IPoint)>, usize);

thread_local! {
    /// The glyph images [`glyph_image`] made, and how many bytes they hold.
    static GLYPH_IMAGES: RefCell<KeptImages> =
        RefCell::new((CacheMap::default(), 0));
}

#[cfg(test)]
thread_local! {
    /// How many glyph images [`glyph_image`] has made on this thread, for the
    /// tests to tell a kept image from a remade one.
    static IMAGES_MADE: Cell<usize> = const { Cell::new(0) };
}

#[cfg(all(test, any(feature = "metal", feature = "vulkan")))]
thread_local! {
    /// How many glyph images [`resident`] has made textures of on this
    /// thread, for the tests to count uploads.
    static TEXTURES_MADE: Cell<usize> = const { Cell::new(0) };
    /// How many raster glyph images [`ColrFace::draw_image`] has drawn on a
    /// GPU canvas on this thread, each of which Skia uploads unless it still
    /// holds the texture, for the tests to count the draws that could.
    static RASTERS_ON_GPU: Cell<usize> = const { Cell::new(0) };
}

/// The textures this thread has made of kept glyph images on GPU canvases,
/// for each context they belong to: by the unique id of the image
/// [`glyph_image`] keeps. A thread draws on one or two contexts -- its own
/// and a window's -- so they are found by looking.
#[cfg(any(feature = "metal", feature = "vulkan"))]
type Textures = Vec<(DirectContextId, CacheMap<u32, Image>)>;

#[cfg(any(feature = "metal", feature = "vulkan"))]
thread_local! {
    /// The textures [`resident`] made.
    static TEXTURES: RefCell<Textures> = const { RefCell::new(Vec::new()) };
}

/// `image`, a glyph image [`glyph_image`] keeps, as a texture on the GPU
/// context `canvas` draws to, made once for that context and kept beside
/// the image; `image` itself on a canvas with no GPU context, or where the
/// texture could not be made.
///
/// A raster image drawn on a GPU canvas is uploaded into a texture that
/// Ganesh keys by the image's pixels (`GrMakeCachedBitmapProxyView` in
/// `skia/src/gpu/ganesh/SkGr.cpp`), and that texture is the context's to
/// free once no draw holds it: at once while the context's cache is over
/// its budget (`GrResourceCache::notifyARefCntReachedZero` in
/// `skia/src/gpu/ganesh/GrResourceCache.cpp`), and whenever the GPU
/// engine's sweep frees the context's resources. Canvases on a thread share
/// that thread's context, so the surfaces of pages not yet freed put the
/// next page's glyphs over budget, and each playback would upload every
/// glyph again. A texture held here is not the cache's to free.
///
/// Held, the textures still count against the context's budget, which
/// Ganesh sets at 256 MiB (`GrResourceCache::kDefaultMaxSize`) and this
/// crate leaves as it is: no more of it than the glyph images they were
/// made of, within [`GLYPH_IMAGE_BYTES_KEPT`] for each context, since they
/// go when those images go. While the cache is over budget it frees what
/// no one holds, so the textures held here move that point by at most
/// that much and are never what it frees.
///
/// Each context keeps its own, so canvases drawing on two contexts in turn
/// on one thread -- a window's and the thread's own -- upload each glyph
/// once for each, not again at every turn.
#[cfg(any(feature = "metal", feature = "vulkan"))]
fn resident(canvas: &Canvas, image: Image) -> Image {
    let Some(mut context) = canvas.direct_context() else {
        return image;
    };
    let of = context.id();
    TEXTURES.with_borrow_mut(|held| {
        let at = match held.iter().position(|(id, _)| *id == of) {
            Some(at) => at,
            None => {
                held.push((of, CacheMap::default()));
                held.len() - 1
            }
        };
        let textures = &mut held[at].1;
        if let Some(texture) = textures.get(&image.unique_id()) {
            return texture.clone();
        }
        let Some(texture) = texture_from_image(
            &mut context,
            &image,
            Mipmapped::No,
            Budgeted::Yes,
        ) else {
            return image;
        };
        #[cfg(test)]
        TEXTURES_MADE.set(TEXTURES_MADE.get() + 1);
        textures.insert(image.unique_id(), texture.clone());
        texture
    })
}

/// `image` itself: without a GPU engine, every canvas is drawn on the CPU.
#[cfg(not(any(feature = "metal", feature = "vulkan")))]
fn resident(_canvas: &Canvas, image: Image) -> Image {
    image
}

/// Lets go of the textures this thread made of the image whose unique id
/// is `id`, on every context: called as a [`Sheet`] generation is replaced.
/// A draw not yet executed holds its own reference to the texture it reads.
fn release_texture_of(id: u32) {
    #[cfg(any(feature = "metal", feature = "vulkan"))]
    TEXTURES.with_borrow_mut(|held| {
        held.iter_mut().for_each(|(_, textures)| {
            textures.remove(&id);
        });
    });
    #[cfg(not(any(feature = "metal", feature = "vulkan")))]
    let _ = id;
}

/// Lets go of the glyph textures this thread holds, which hold their
/// contexts: called as the glyph images they were made of go, and by a GPU
/// engine before it frees or drops an idle context.
pub(crate) fn release_textures() {
    #[cfg(any(feature = "metal", feature = "vulkan"))]
    TEXTURES.set(Vec::new());
}

/// Lets go of the glyph textures this thread holds on `context`, which hold
/// it: called by a window's renderer as it abandons its context, and by the
/// test that draws on two Metal contexts.
#[cfg(any(
    all(any(feature = "metal", feature = "vulkan"), feature = "window"),
    all(test, feature = "metal", target_os = "macos")
))]
pub(crate) fn release_textures_of(context: DirectContextId) {
    TEXTURES.with_borrow_mut(|held| held.retain(|(id, _)| *id != context));
}

/// The glyph `picture` records, inking within `bounds`, as an image under
/// `shape`, a device transform with no translation, in `format`, and where
/// the image's top left lands from the glyph's origin. Kept for the next ask
/// on this thread where `keep`, within [`GLYPH_IMAGE_BYTES_KEPT`]; `None`
/// where it inks no pixel or a surface could not be made.
fn glyph_image(
    picture: &Picture,
    bounds: Rect,
    shape: &Matrix,
    format: &Format,
    keep: bool,
) -> Option<(Image, IPoint)> {
    let key = (
        picture.unique_id(),
        [
            shape.scale_x().to_bits(),
            shape.skew_x().to_bits(),
            shape.skew_y().to_bits(),
            shape.scale_y().to_bits(),
        ],
        format.color_type as i32,
        format.space,
    );
    if keep
        && let Some(made) =
            GLYPH_IMAGES.with_borrow(|(images, _)| images.get(&key).cloned())
    {
        return Some(made);
    }
    let reach = shape.map_rect(bounds).0;
    let area = IRect::from_ltrb(
        reach.left.floor() as i32,
        reach.top.floor() as i32,
        reach.right.ceil() as i32,
        reach.bottom.ceil() as i32,
    );
    if area.is_empty() {
        return None;
    }
    let info = format.image_info(area.size());
    let mut surface = started::raster(&info, None, None)?;
    let canvas = surface.canvas();
    canvas.translate((-area.left as f32, -area.top as f32));
    canvas.concat(shape);
    canvas.draw_picture(picture, None, None);
    let made = (surface.image_snapshot(), IPoint::new(area.left, area.top));
    #[cfg(test)]
    IMAGES_MADE.set(IMAGES_MADE.get() + 1);
    if keep {
        let bytes = info.compute_min_byte_size();
        GLYPH_IMAGES.with_borrow_mut(|(images, held)| {
            if *held + bytes > GLYPH_IMAGE_BYTES_KEPT {
                images.clear();
                *held = 0;
                drop_atlases();
                release_textures();
            }
            images.insert(key, made.clone());
            *held += bytes;
        });
    }
    Some(made)
}

/// Where Chrome places a COLR glyph whose origin is `at` under `canvas`'s
/// transform, in the canvas's space: its device origin moved to a whole
/// pixel -- across by [`SNAP_ACROSS`] and down by [`SNAP_DOWN`] before the
/// floor -- as Skia places a glyph's mask for text laid out along x
/// (`SkGlyphPositionRoundingSpec` in `skia/src/core/SkGlyph.cpp`), with no
/// sub-pixel phase, since Fontations draws a colour glyph with none.
/// Measured in Chrome 158 at 150px: a glyph whose edges fall on whole
/// pixels at an origin of 10 keeps them there from 10 to 10.75 and moves a
/// whole pixel at 10.875; down, from 85 to 85.375, and a pixel at 85.5.
/// Under a rotation the device origin is snapped all the same: a glyph
/// turned by 0.4 radians lands within an edge's anti-aliasing of Chrome's
/// so placed, and half a pixel off it placed exactly. `at` as it stands
/// where the transform cannot be undone.
///
/// `at` is the run's origin as SkParagraph lays it out, whose baseline it
/// snaps to a whole pixel where Chrome does not (#142), so a glyph is
/// snapped from that baseline: where #142 moves it, this placement moves
/// with it, and the plain glyphs beside it, placed by Skia from the same
/// origin, stay level.
fn snapped(canvas: &Canvas, at: Point) -> Point {
    let matrix = canvas.local_to_device_as_3x3();
    let Some(inverse) = matrix.invert() else {
        return at;
    };
    let pixel = snapped_on_device(&matrix, at);
    inverse.map_point(Point::new(pixel.x as f32, pixel.y as f32))
}

/// The whole device pixel [`snapped`] places a glyph whose origin is `at`
/// on, under `device`, the canvas's transform: worked out once on the
/// device rather than there and back through the transform's inverse.
fn snapped_on_device(device: &Matrix, at: Point) -> IPoint {
    let on_device = device.map_point(at);
    IPoint::new(
        (on_device.x + SNAP_ACROSS).floor() as i32,
        (on_device.y + SNAP_DOWN).floor() as i32,
    )
}

/// What Skia adds to a glyph's device x before it floors it, for text laid
/// out along x: half of the quarter pixel its sub-pixel phases are
/// (`SkGlyphPositionRoundingSpec`, `kSubpixelRounding`).
const SNAP_ACROSS: f32 = 1.0 / 8.0;

/// What Skia adds to a glyph's device y before it floors it, on the axis it
/// gives no sub-pixel phase: half a pixel, a rounding to the nearest.
const SNAP_DOWN: f32 = 0.5;

/// The design-space point `(x, y)`, y up, in the recording's space, y down,
/// as Fontations turns every coordinate a paint states. The graph is
/// recorded with no reflection anywhere in it.
fn down(x: f32, y: f32) -> Point {
    Point::new(x, -y)
}

/// A graph's transform turned y down: `F T F` with `F` = diag(1, -1), whose
/// off-diagonal terms and y translation change sign -- as Fontations'
/// `SkMatrixFromFontationsTransform`.
fn turned(transform: &Transform) -> Matrix {
    Matrix::new_all(
        transform.xx,
        -transform.xy,
        transform.dx,
        -transform.yx,
        transform.yy,
        -transform.dy,
        0.0,
        0.0,
        1.0,
    )
}

/// Walks a paint graph onto a recording canvas, in design units with y
/// turned down, and keeps the bounds of what it inks.
struct Recorder<'a> {
    canvas: &'a Canvas,
    outlines: OutlineGlyphCollection<'a>,
    location: LocationRef<'a>,
    palette: &'a [Color],
    foreground: Color,
    /// What each clip pushed and not yet popped leaves, in the recording's
    /// space, within the clips outside it: `None` for one that leaves
    /// nothing.
    clips: Vec<Option<Rect>>,
    /// The union of what every fill so far covered.
    inked: Option<Rect>,
    /// What the graph so far asks of a vector backend -- see
    /// [`ColrGlyph::features`].
    features: VectorFeatures,
}

impl Recorder<'_> {
    /// `glyph`'s outline in design units, unhinted, at the face's place on
    /// its axes, turned y down -- as Fontations' `generatePathForGlyphId`
    /// at the units per em with no hinting. An empty path for a glyph with
    /// none, which clips to nothing and fills nothing.
    fn outline(&self, glyph: SkrifaGlyphId) -> Path {
        let mut pen = Pen(PathBuilder::new());
        if let Some(outline) = self.outlines.get(glyph) {
            let settings =
                DrawSettings::unhinted(Size::unscaled(), self.location);
            if outline.draw(settings, &mut pen).is_err() {
                return Path::default();
            }
        }
        pen.0.detach()
    }

    /// `bounds` in the current space, as bounds in the recording's within the
    /// innermost clip.
    fn within(&self, bounds: Rect) -> Option<Rect> {
        let mapped = self.canvas.local_to_device_as_3x3().map_rect(bounds).0;
        match self.clips.last() {
            Some(Some(outer)) => {
                let mut within = mapped;
                within.intersect(outer).then_some(within)
            }
            Some(None) => None,
            None => Some(mapped),
        }
    }

    /// Adds `bounds`, in the recording's space, to what the recording inks.
    fn ink(&mut self, bounds: Option<Rect>) {
        if let Some(bounds) = bounds {
            self.inked = Some(match self.inked {
                Some(inked) => Rect::join2(inked, bounds),
                None => bounds,
            });
        }
    }

    /// The colour a stop or a solid fill names, its alpha multiplied in:
    /// the foreground for [`FOREGROUND_INDEX`], magenta past the palette, as
    /// Fontations' `configure_solid_paint` and `populateStopsAndColors`.
    fn colour(&self, palette_index: u16, alpha: f32) -> Color4f {
        let mut colour = match palette_index {
            FOREGROUND_INDEX => Color4f::from(self.foreground),
            index => self
                .palette
                .get(usize::from(index))
                .map_or(Color4f::from(Color::MAGENTA), |&colour| {
                    Color4f::from(colour)
                }),
        };
        colour.a *= alpha;
        colour
    }

    /// The paint `brush` fills with, its shader under `brush_transform`
    /// where it has one; `None` where it fills nothing.
    fn brush(
        &self,
        brush: &Brush<'_>,
        brush_transform: Option<&Matrix>,
    ) -> Option<Paint> {
        let mut paint = Paint::default();
        paint.set_anti_alias(true);
        let shaded = match brush {
            Brush::Solid {
                palette_index,
                alpha,
            } => {
                paint.set_color4f(self.colour(*palette_index, *alpha), None);
                return Some(paint);
            }
            Brush::LinearGradient {
                p0,
                p1,
                color_stops,
                extend,
            } => self.stops(color_stops).shade(|line| {
                gradient_shaders::linear_gradient(
                    (down(p0.x, p0.y), down(p1.x, p1.y)),
                    &line.gradient(*extend),
                    brush_transform,
                )
            }),
            Brush::RadialGradient {
                c0,
                r0,
                c1,
                r1,
                color_stops,
                extend,
            } => self.stops(color_stops).shade(|line| {
                radial(
                    (down(c0.x, c0.y), *r0),
                    (down(c1.x, c1.y), *r1),
                    line,
                    *extend,
                    brush_transform,
                )
            }),
            // skrifa states a sweep's angles clockwise, for a space turned y
            // down, which is Skia's.
            Brush::SweepGradient {
                c0,
                start_angle,
                end_angle,
                color_stops,
                extend,
            } => self.stops(color_stops).shade(|line| {
                gradient_shaders::sweep_gradient(
                    down(c0.x, c0.y),
                    (*start_angle, *end_angle),
                    &line.gradient(*extend),
                    brush_transform,
                )
            }),
        };
        match shaded {
            Some(Shaded::Shader(shader)) => {
                // Opaque, so that the shader is not modulated by an alpha.
                paint.set_color(Color::BLACK).set_shader(shader);
            }
            Some(Shaded::Solid(colour)) => {
                paint.set_color4f(colour, None);
            }
            // A gradient Skia declines to make paints its paint's own
            // colour, the opaque black Fontations sets before the shader.
            None => {
                paint.set_color(Color::BLACK);
            }
        }
        Some(paint)
    }

    /// The colour line `stops` are, or what a line of one stop fills with.
    fn stops(&self, stops: &[ColorStop]) -> Option<Line> {
        Some(Line {
            offsets: stops.iter().map(|stop| stop.offset).collect(),
            colours: stops
                .iter()
                .map(|stop| self.colour(stop.palette_index, stop.alpha))
                .collect(),
        })
    }

    /// Notes a sweep gradient among what the graph asks of a vector
    /// backend.
    fn note(&mut self, brush: &Brush<'_>) {
        if matches!(brush, Brush::SweepGradient { .. }) {
            self.features = self.features.with(VectorFeatures::EXOTIC_SHADER);
        }
    }

    /// Fills the current clip, or `path` where there is one, with `paint`.
    fn fill_with(&mut self, path: Option<&Path>, paint: &Paint) {
        let bounds = match path {
            Some(path) => self.within(*path.bounds()),
            None => self.clips.last().copied().flatten(),
        };
        match path {
            Some(path) => self.canvas.draw_path(path, paint),
            None => self.canvas.draw_paint(paint),
        };
        self.ink(bounds);
    }
}

/// What a brush's gradient comes to.
enum Shaded {
    Shader(Shader),
    /// A line of one stop, which Fontations draws as that colour.
    Solid(Color4f),
}

/// A colour line as skrifa hands it, normalised to `[0, 1]` and in order.
struct Line {
    offsets: Vec<f32>,
    colours: Vec<Color4f>,
}

impl Line {
    /// The gradient this line paints with `extend`, interpolated as
    /// Fontations interpolates a `COLR` gradient: unpremultiplied, in sRGB.
    fn gradient(&self, extend: Extend) -> Gradient<'_> {
        Gradient::new(
            GradientColors::new(
                &self.colours,
                Some(self.offsets.as_slice()),
                tile_mode(extend),
                None,
            ),
            Interpolation {
                in_premul: interpolation::InPremul::No,
                color_space: interpolation::ColorSpace::SRGB,
                hue_method: interpolation::HueMethod::Shorter,
            },
        )
    }
}

/// The gradient a brush paints, or a line of one stop as its colour.
trait Shade {
    fn shade(self, make: impl FnOnce(Line) -> Option<Shader>)
    -> Option<Shaded>;
}

impl Shade for Option<Line> {
    fn shade(
        self,
        make: impl FnOnce(Line) -> Option<Shader>,
    ) -> Option<Shaded> {
        let line = self?;
        if line.colours.len() == 1 {
            return Some(Shaded::Solid(line.colours[0]));
        }
        make(line).map(Shaded::Shader)
    }
}

/// The tile mode a colour line's extend names: pad, repeat and reflect are
/// Skia's clamp, repeat and mirror.
fn tile_mode(extend: Extend) -> TileMode {
    match extend {
        Extend::Repeat => TileMode::Repeat,
        Extend::Reflect => TileMode::Mirror,
        _ => TileMode::Clamp,
    }
}

/// A radial gradient between `start` and `end`, each a centre and a radius,
/// as Fontations' `configure_radial_paint` makes one.
///
/// skrifa normalises the line to `[0, 1]` by moving the circles, which can
/// leave a radius negative, and a negative radius is no cone Skia draws. As
/// colr-gradients-spec#367 resolves it: padded, the line is cut where the
/// radius passes zero, at the colour interpolated there, and the circle
/// moved to that point; two radii both negative and equal paint nothing; and
/// repeated or reflected, both circles are moved by the whole number of
/// periods -- an even number, reflected -- that makes both radii positive.
fn radial(
    (mut c0, mut r0): (Point, f32),
    (mut c1, mut r1): (Point, f32),
    mut line: Line,
    extend: Extend,
    local: Option<&Matrix>,
) -> Option<Shader> {
    if r0 < 0.0 || r1 < 0.0 {
        if r0 == r1 {
            return Some(skia_safe::shaders::color(Color::TRANSPARENT));
        }
        let along = c1 - c0;
        let growth = r1 - r0;
        if extend == Extend::Pad {
            let zero = -r0 / (r1 - r0);
            if r0 < 0.0 {
                c0 += along * zero;
                r0 = 0.0;
                cut(&mut line, zero, Cut::Start);
            } else {
                c1 -= along * (1.0 - zero);
                r1 = 0.0;
                cut(&mut line, zero, Cut::End);
            }
            if r0 == 0.0 && r1 == 0.0 {
                // Both clamped to nothing: a cone of one colour, the one at
                // whichever end still grows.
                if growth > 0.0 {
                    c1 = c0 + along;
                    r1 = growth;
                    line.keep_last();
                } else {
                    c0 -= along;
                    r0 = -growth;
                    line.keep_first();
                }
            }
        } else {
            let crossing = r0 / (r0 - r1);
            let inside = (0.0..=1.0).contains(&crossing);
            let direction = match inside && growth < 0.0 {
                true => -1.0,
                false => 1.0,
            };
            let periods = whole_periods(crossing * direction, extend);
            c0 += along * periods;
            c1 += along * periods;
            r0 += growth * periods;
            r1 += growth * periods;
        }
    }
    gradient_shaders::two_point_conical_gradient(
        (c0, r0),
        (c1, r1),
        &line.gradient(extend),
        local,
    )
}

/// The whole number of periods a repeated or reflected radial gradient's
/// circles move by to clear a crossing `factor` periods along: up from a
/// positive factor, down past a negative one, and kept even under reflect,
/// whose period is two of the line's lengths.
fn whole_periods(factor: f32, extend: Extend) -> f32 {
    let mut periods = match factor > 0.0 {
        true => factor.ceil(),
        false => factor.floor() - 1.0,
    };
    if extend == Extend::Reflect && periods % 2.0 != 0.0 {
        periods += periods.signum();
    }
    periods
}

/// Which end of a colour line a radius passing zero cuts.
enum Cut {
    Start,
    End,
}

/// Cuts `line` at `at`, keeping the side `cut` does not name and ending it at
/// the colour interpolated at `at`, as Fontations'
/// `truncateToStopInterpolating`. A line `at` does not cross is left as it
/// is.
fn cut(line: &mut Line, at: f32, cut: Cut) {
    let offsets = &line.offsets;
    let (Some(&first), Some(&last)) = (offsets.first(), offsets.last()) else {
        return;
    };
    if offsets.len() < 2 || at < first || last < at {
        return;
    }
    let after = match cut {
        Cut::Start => offsets.partition_point(|&offset| offset < at),
        Cut::End => offsets.partition_point(|&offset| offset <= at),
    }
    .clamp(1, offsets.len() - 1);
    let span = offsets[after] - offsets[after - 1];
    let t = match span > 0.0 {
        true => ((at - offsets[after - 1]) / span).clamp(0.0, 1.0),
        false => 0.0,
    };
    let (from, to) = (line.colours[after - 1], line.colours[after]);
    let colour = Color4f::new(
        from.r + (to.r - from.r) * t,
        from.g + (to.g - from.g) * t,
        from.b + (to.b - from.b) * t,
        from.a + (to.a - from.a) * t,
    );
    match cut {
        Cut::Start => {
            line.offsets.drain(..after);
            line.colours.drain(..after);
            line.offsets.insert(0, 0.0);
            line.colours.insert(0, colour);
        }
        Cut::End => {
            line.offsets.truncate(after);
            line.colours.truncate(after);
            line.offsets.push(1.0);
            line.colours.push(colour);
        }
    }
}

impl Line {
    /// Keeps only the last stop.
    fn keep_last(&mut self) {
        let keep = self.offsets.len().saturating_sub(1);
        self.offsets.drain(..keep);
        self.colours.drain(..keep);
    }

    /// Keeps only the first stop.
    fn keep_first(&mut self) {
        self.offsets.truncate(1);
        self.colours.truncate(1);
    }
}

impl ColorPainter for Recorder<'_> {
    fn push_transform(&mut self, transform: Transform) {
        self.canvas.save();
        self.canvas.concat(&turned(&transform));
    }

    fn pop_transform(&mut self) {
        self.canvas.restore();
    }

    fn push_clip_glyph(&mut self, glyph_id: SkrifaGlyphId) {
        let outline = self.outline(glyph_id);
        let bounds = self.within(*outline.bounds());
        self.canvas.save();
        self.canvas.clip_path(&outline, ClipOp::Intersect, true);
        self.clips.push(bounds);
    }

    fn push_clip_box(&mut self, clip_box: BoundingBox<f32>) {
        let clip = Rect::from_ltrb(
            clip_box.x_min,
            -clip_box.y_max,
            clip_box.x_max,
            -clip_box.y_min,
        );
        let bounds = self.within(clip);
        self.canvas.save();
        self.canvas.clip_rect(clip, ClipOp::Intersect, true);
        self.clips.push(bounds);
    }

    fn pop_clip(&mut self) {
        self.clips.pop();
        self.canvas.restore();
    }

    fn fill(&mut self, brush: Brush<'_>) {
        self.note(&brush);
        if let Some(paint) = self.brush(&brush, None) {
            self.fill_with(None, &paint);
        }
    }

    /// A glyph's outline filled as a path, its brush under the transform
    /// given, as Fontations' `fill_glyph_*`: clipping to the outline and
    /// filling the clip covers the same pixels but anti-aliases the edge
    /// twice over.
    fn fill_glyph(
        &mut self,
        glyph_id: SkrifaGlyphId,
        brush_transform: Option<Transform>,
        brush: Brush<'_>,
    ) {
        self.note(&brush);
        let local = brush_transform.as_ref().map(turned);
        if let Some(paint) = self.brush(&brush, local.as_ref()) {
            let outline = self.outline(glyph_id);
            self.fill_with(Some(&outline), &paint);
        }
    }

    fn push_layer(&mut self, composite_mode: CompositeMode) {
        let mode = blend_mode(composite_mode);
        // The layer's own source may be translucent wherever it inks.
        self.features = self.features.with(blend_features(mode, false));
        let mut layer = Paint::default();
        layer.set_blend_mode(mode);
        self.canvas
            .save_layer(&SaveLayerRec::default().paint(&layer));
    }

    fn pop_layer(&mut self) {
        self.canvas.restore();
    }
}

/// A glyph's outline as skrifa draws it, built as a Skia path turned y down
/// -- see [`down`] -- nonzero-filled as both `glyf` and CFF outlines are.
struct Pen(PathBuilder);

impl OutlinePen for Pen {
    fn move_to(&mut self, x: f32, y: f32) {
        self.0.move_to(down(x, y));
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.0.line_to(down(x, y));
    }

    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        self.0.quad_to(down(cx0, cy0), down(x, y));
    }

    fn curve_to(
        &mut self,
        cx0: f32,
        cy0: f32,
        cx1: f32,
        cy1: f32,
        x: f32,
        y: f32,
    ) {
        self.0.cubic_to(down(cx0, cy0), down(cx1, cy1), down(x, y));
    }

    fn close(&mut self) {
        self.0.close();
    }
}

/// The Skia blend mode a `COLR` composite mode names: OpenType 1.9, "COLR",
/// "Compositing and blending", whose modes are the W3C's Compositing and
/// Blending Level 1 operators that Skia's blend modes implement. A mode the
/// table does not define is Skia's destination, as Fontations'
/// `ToSkBlendMode` leaves it: drawing nothing over what is there.
fn blend_mode(mode: CompositeMode) -> BlendMode {
    use BlendMode as B;
    match mode {
        CompositeMode::Clear => B::Clear,
        CompositeMode::Src => B::Src,
        CompositeMode::Dest => B::Dst,
        CompositeMode::SrcOver => B::SrcOver,
        CompositeMode::DestOver => B::DstOver,
        CompositeMode::SrcIn => B::SrcIn,
        CompositeMode::DestIn => B::DstIn,
        CompositeMode::SrcOut => B::SrcOut,
        CompositeMode::DestOut => B::DstOut,
        CompositeMode::SrcAtop => B::SrcATop,
        CompositeMode::DestAtop => B::DstATop,
        CompositeMode::Xor => B::Xor,
        CompositeMode::Plus => B::Plus,
        CompositeMode::Screen => B::Screen,
        CompositeMode::Overlay => B::Overlay,
        CompositeMode::Darken => B::Darken,
        CompositeMode::Lighten => B::Lighten,
        CompositeMode::ColorDodge => B::ColorDodge,
        CompositeMode::ColorBurn => B::ColorBurn,
        CompositeMode::HardLight => B::HardLight,
        CompositeMode::SoftLight => B::SoftLight,
        CompositeMode::Difference => B::Difference,
        CompositeMode::Exclusion => B::Exclusion,
        CompositeMode::Multiply => B::Multiply,
        CompositeMode::HslHue => B::Hue,
        CompositeMode::HslSaturation => B::Saturation,
        CompositeMode::HslColor => B::Color,
        CompositeMode::HslLuminosity => B::Luminosity,
        _ => B::Dst,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use skia_safe::{
        Font, FontArguments, FontMgr, FourByteTag,
        font_arguments::{VariationPosition, variation_position::Coordinate},
    };

    use super::*;

    /// A variable COLRv1 font, from googlefonts/color-fonts.
    const VARIABLE: &str =
        "tests/assets/fonts/color-fonts/test_glyphs-glyf_colr_1_variable.ttf";

    fn face() -> Typeface {
        let bytes = fs::read(VARIABLE).expect(VARIABLE);
        FontMgr::new()
            .new_from_data(Data::new_copy(&bytes), None)
            .expect("the font loads")
    }

    /// Every glyph `face` records, as serialised pictures, read once from
    /// its own file and once from the tables a system face would give.
    fn recorded_both_ways(typeface: &Typeface) -> (Vec<Vec<u8>>, Vec<Vec<u8>>) {
        let (file, index) =
            typeface.to_existing_font_data().expect("a face from data");
        let assembled = tables_of(typeface);
        assert_ne!(
            file.as_bytes(),
            assembled.as_bytes(),
            "the assembled file is a different file, or this proves nothing",
        );
        let from_file =
            ColrFace::read(typeface, file, index).expect("read from its file");
        let from_tables =
            ColrFace::read(typeface, assembled, 0).expect("read from tables");
        let all = |face: &ColrFace| -> Vec<Vec<u8>> {
            (0..typeface.count_glyphs() as GlyphId)
                .filter_map(|glyph| face.record(glyph, Color::BLACK))
                .filter_map(|glyph| glyph.picture)
                .map(|picture| picture.serialize().as_bytes().to_vec())
                .collect()
        };
        (all(&from_file), all(&from_tables))
    }

    #[test]
    fn a_face_read_from_its_tables_records_as_from_its_file() {
        let (from_file, from_tables) = recorded_both_ways(&face());
        // The glyphs the test font's `COLR` table paints, which its 221 code
        // points share.
        assert_eq!(from_file.len(), 199, "glyphs recorded");
        assert!(from_file == from_tables, "the recordings differ");
    }

    #[test]
    fn a_face_read_from_its_tables_records_its_axes_as_from_its_file() {
        // Every axis at its maximum, so that each table carrying variations
        // -- the paint graph's in `COLR`, the outlines' in `gvar` -- has
        // something to vary.
        let bytes = fs::read(VARIABLE).expect(VARIABLE);
        let font = FontRef::new(&bytes).expect("skrifa reads it");
        let coordinates: Vec<Coordinate> = font
            .axes()
            .iter()
            .map(|axis| Coordinate {
                axis: FourByteTag::new(u32::from_be_bytes(
                    axis.tag().to_be_bytes(),
                )),
                value: axis.max_value(),
            })
            .collect();
        let varied = face()
            .clone_with_arguments(
                &FontArguments::new().set_variation_design_position(
                    VariationPosition {
                        coordinates: &coordinates,
                    },
                ),
            )
            .expect("an instance");
        let (from_file, from_tables) = recorded_both_ways(&varied);
        let (at_default, _) = recorded_both_ways(&face());
        assert!(
            from_file != at_default,
            "the axes move nothing, so this cannot tell the files apart",
        );
        assert!(from_file == from_tables, "the recordings differ");
    }

    /// Every colour glyph of the test font at 32px as the image the CPU
    /// canvas composites, in N32.
    fn glyph_images() -> Vec<Image> {
        let typeface = face();
        let colr = colr_face(&typeface).expect("a COLR face");
        let upem = typeface.units_per_em().expect("upem") as f32;
        let shape = Matrix::scale((32.0 / upem, 32.0 / upem));
        let format = Format::new(ColorType::N32, Some(ColorSpace::new_srgb()));
        (0..typeface.count_glyphs() as GlyphId)
            .filter_map(|glyph| {
                let recorded = colr.record(glyph, Color::BLACK)?;
                let picture = recorded.picture.as_ref()?;
                let bounds = recorded.bounds?;
                glyph_image(picture, bounds, &shape, &format, false)
                    .map(|(image, _)| image)
            })
            .collect()
    }

    /// `image`'s premultiplied pixels in RGBA order.
    fn rgba(image: &Image) -> Vec<u8> {
        let info = ImageInfo::new(
            image.dimensions(),
            ColorType::RGBA8888,
            AlphaType::Premul,
            Some(ColorSpace::new_srgb()),
        );
        let mut pixels = vec![0; info.compute_min_byte_size()];
        assert!(image.read_pixels(
            &info,
            &mut pixels,
            info.min_row_bytes(),
            (0, 0),
            skia_safe::image::CachingHint::Allow,
        ));
        pixels
    }

    /// `image` composited at `alpha` by `draw` onto a CPU N32 page of
    /// `rgb(200 120 60)`, three pixels in, and how many of its channels
    /// miss the exact source-over rounded to nearest.
    fn composited(
        image: &Image,
        alpha: f32,
        draw: impl Fn(&Canvas, &Image, IPoint, &Paint),
    ) -> (Vec<u8>, usize) {
        const INSET: i32 = 3;
        const PAGE: [u8; 4] = [200, 120, 60, 255];
        let (width, height) =
            (image.width() + 2 * INSET, image.height() + 2 * INSET);
        let mut surface =
            started::raster_n32_premul((width, height)).expect("a raster");
        let canvas = surface.canvas();
        canvas.clear(Color::from_argb(PAGE[3], PAGE[0], PAGE[1], PAGE[2]));
        let mut paint = Paint::default();
        paint.set_alpha_f(alpha);
        draw(canvas, image, IPoint::new(INSET, INSET), &paint);
        let page = rgba(&surface.image_snapshot());
        let source = rgba(image);
        let scale = f64::from(paint.alpha()) / 255.0;
        let mut off = 0;
        for y in 0..image.height() as usize {
            for x in 0..image.width() as usize {
                let s = (y * image.width() as usize + x) * 4;
                let p = ((y + INSET as usize) * width as usize
                    + x
                    + INSET as usize)
                    * 4;
                let a = f64::from(source[s + 3]) / 255.0 * scale;
                for c in 0..4 {
                    let exact = f64::from(source[s + c]) * scale
                        + f64::from(PAGE[c]) * (1.0 - a);
                    off += usize::from(f64::from(page[p + c]) != exact.round());
                }
            }
        }
        (page, off)
    }

    #[test]
    fn a_glyph_image_composites_at_full_alpha_as_the_exact_composite_rounded() {
        // At full alpha the CPU canvas stores each glyph image's source-over
        // rounded to nearest, on every architecture. Skia's eight-bit
        // composite does on aarch64 and does not on x86-64 -- the control
        // below -- so there the image goes through the float pipeline.
        let images = glyph_images();
        assert!(images.len() > 100, "only {} glyph images", images.len());
        let eight_bit = |canvas: &Canvas,
                         image: &Image,
                         at: IPoint,
                         paint: &Paint| {
            canvas.draw_image(image, (at.x as f32, at.y as f32), Some(paint));
        };
        let (mut off, mut control) = (0, 0);
        for image in &images {
            off += composited(image, 1.0, composite_onto).1;
            control += composited(image, 1.0, eight_bit).1;
            // Under an alpha the composite is Skia's eight-bit one, as
            // Chrome's CPU canvas has it, on every architecture.
            assert!(
                composited(image, 0.5, composite_onto).0
                    == composited(image, 0.5, eight_bit).0,
                "a glyph image under an alpha left Skia's composite"
            );
        }
        assert_eq!(off, 0, "{off} channels off the exact composite rounded");
        // The control: what the eight-bit composite does here.
        if cfg!(target_arch = "x86_64") {
            assert!(
                control > 0,
                "x86-64's eight-bit composite rounds to nearest"
            );
        }
        if cfg!(target_arch = "aarch64") {
            assert_eq!(control, 0, "aarch64's eight-bit composite truncates");
        }
        eprintln!("eight-bit composite: {control} channels off nearest");
    }

    /// The format of an eight-bit sRGB page.
    fn srgb() -> Format {
        Format::new(ColorType::RGBA8888, Some(ColorSpace::new_srgb()))
    }

    /// A picture filling `side` units square, as a glyph's recording does.
    fn square(side: f32) -> (Picture, Rect) {
        let bounds = Rect::from_wh(side, side);
        let mut recorder = PictureRecorder::new();
        let canvas = recorder.begin_recording(bounds, false);
        canvas.draw_rect(bounds, &Paint::default());
        let picture = recorder
            .finish_recording_as_picture(None)
            .expect("a picture");
        (picture, bounds)
    }

    /// How many glyph images `make` made on this thread.
    fn images_made(make: impl FnOnce()) -> usize {
        let before = IMAGES_MADE.get();
        make();
        IMAGES_MADE.get() - before
    }

    #[test]
    fn a_glyph_image_is_kept_for_its_transform_and_format() {
        forget();
        let (picture, bounds) = square(16.0);
        let at = |scale: f32, keep: bool| {
            glyph_image(
                &picture,
                bounds,
                &Matrix::scale((scale, scale)),
                &srgb(),
                keep,
            )
            .expect("an image");
        };
        assert_eq!(images_made(|| at(1.0, true)), 1);
        assert_eq!(images_made(|| at(1.0, true)), 0, "kept");
        assert_eq!(images_made(|| at(2.0, true)), 1, "another transform");
        assert_eq!(images_made(|| at(2.0, true)), 0, "kept as well");
        let wide =
            Format::new(ColorType::RGBAF16, Some(ColorSpace::new_srgb()));
        let made = images_made(|| {
            glyph_image(&picture, bounds, &Matrix::new_identity(), &wide, true)
                .expect("an image");
        });
        assert_eq!(made, 1, "another format");
        assert_eq!(images_made(|| at(1.0, false)), 1, "not kept");
        assert_eq!(images_made(|| at(1.0, false)), 1, "nor reused");
    }

    #[test]
    fn the_glyph_images_kept_stay_within_their_budget() {
        // Each image takes two fifths of the budget, so the third drops the
        // two before it, which are then made again when asked for.
        forget();
        let side = ((GLYPH_IMAGE_BYTES_KEPT * 2 / 5 / 4) as f32).sqrt().floor();
        let squares: Vec<_> = (0..3).map(|_| square(side)).collect();
        let ask = |which: usize| {
            let (picture, bounds) = &squares[which];
            images_made(|| {
                glyph_image(
                    picture,
                    *bounds,
                    &Matrix::new_identity(),
                    &srgb(),
                    true,
                )
                .expect("an image");
            })
        };
        assert_eq!((ask(0), ask(1), ask(0)), (1, 1, 0), "two fit");
        assert_eq!(ask(2), 1);
        let held = GLYPH_IMAGES.with_borrow(|(_, held)| *held);
        assert!(held <= GLYPH_IMAGE_BYTES_KEPT, "{held} bytes kept");
        assert_eq!(
            (ask(2), ask(0)),
            (0, 1),
            "the third kept, the first dropped"
        );
    }

    #[test]
    fn colour_text_past_skias_largest_kept_size_is_made_each_time() {
        // Drawn twice through the image route, a glyph at 48px is made once
        // and kept; one past `LARGEST_KEPT_TEXT` is made each time.
        forget();
        let face = face();
        let glyph = face.unichar_to_glyph(0xf0200);
        let draws_made = |size: f32| {
            let font = Font::from_typeface(&face, size);
            let paint = Paint::default();
            let run = ColourRun::of(&font, &paint).expect("a colour face");
            let colour = run.glyph(glyph).expect("a colour glyph");
            let side = (size * 2.0) as i32;
            let mut surface = started::raster(
                &srgb().image_info((side, side).into()),
                None,
                None,
            )
            .expect("a surface");
            let _route = ImageRoute::open(srgb());
            [0, 1].map(|_| {
                images_made(|| {
                    run.draw(
                        surface.canvas(),
                        &colour,
                        Point::new(0.0, size),
                        &paint,
                    );
                })
            })
        };
        assert_eq!(draws_made(48.0), [1, 0]);
        assert_eq!(draws_made(LARGEST_KEPT_TEXT + 44.0), [1, 1]);
    }

    #[cfg(all(feature = "metal", target_os = "macos"))]
    #[test]
    fn glyphs_drawn_on_two_contexts_in_turn_are_uploaded_once_for_each() {
        // A window's renderer draws on a context of its own, on the thread
        // whose context the offscreen canvases share; drawn in turn, each
        // keeps the textures made for it.
        use crate::gpu::{RenderingEngine, another_context};
        use skia_safe::gpu::{Budgeted, surfaces};
        let mut image_surface = started::raster(
            &ImageInfo::new_n32_premul((24, 24), None),
            None,
            None,
        )
        .expect("a raster");
        image_surface.canvas().clear(Color::RED);
        let image = image_surface.image_snapshot();
        let info = ImageInfo::new_n32_premul((64, 64), None);
        let mut window = another_context().expect("a second context");
        let mut window_page = surfaces::render_target(
            &mut window,
            Budgeted::Yes,
            &info,
            None,
            None,
            None,
            None,
            None,
        )
        .expect("a window's page");
        let mut made = Vec::new();
        RenderingEngine::GPU.with_direct_context(|context| {
            let context = context.expect("the thread's context");
            let mut page = surfaces::render_target(
                context,
                Budgeted::Yes,
                &info,
                None,
                None,
                None,
                None,
                None,
            )
            .expect("a page");
            for _ in 0..4 {
                for surface in [&mut page, &mut window_page] {
                    let before = TEXTURES_MADE.get();
                    let texture = resident(surface.canvas(), image.clone());
                    assert!(texture.is_texture_backed(), "drawn from the CPU");
                    made.push(TEXTURES_MADE.get() - before);
                }
            }
            // The window's renderer lets its context go: its textures go,
            // and the thread's stay.
            release_textures_of(
                window_page
                    .canvas()
                    .direct_context()
                    .expect("a context")
                    .id(),
            );
            for surface in [&mut page, &mut window_page] {
                let before = TEXTURES_MADE.get();
                resident(surface.canvas(), image.clone());
                made.push(TEXTURES_MADE.get() - before);
            }
        });
        release_textures();
        assert_eq!(
            made,
            [1, 1, 0, 0, 0, 0, 0, 0, 0, 1],
            "textures made each turn, then after the window's release"
        );
    }

    #[cfg(any(feature = "metal", feature = "vulkan"))]
    #[test]
    fn a_glyph_texture_stays_on_the_gpu_from_page_to_page() {
        // A GPU canvas whose context is over its resource budget -- a
        // process holding canvases it has not yet freed -- releases a
        // texture Skia keyed for one of these images when its draw ends, and
        // uploaded every glyph again for each new page. Held here, a glyph
        // is uploaded once for its context.
        use crate::{gpu::RenderingEngine, prelude::*};
        if !RenderingEngine::GPU.selectable() {
            // A Mac always has Metal, so there a missing GPU is a failure;
            // elsewhere -- CI's Linux runners -- the test says it did not
            // run.
            if cfg!(target_os = "macos") {
                panic!("no GPU on macOS");
            }
            eprintln!("skipped: no GPU");
            return;
        }
        forget();
        let library = FontLibrary::new();
        library
            .use_paths(Some("ResidentColr"), [VARIABLE])
            .expect("the font registers");
        let mut limit = 0;
        RenderingEngine::GPU.with_direct_context(|context| {
            let context = context.expect("a GPU context");
            limit = context.resource_cache_limit();
            context.set_resource_cache_limit(1);
        });
        let page = || {
            let mut canvas = crate::canvas::Canvas::new(400.0, 200.0);
            canvas.set_gpu(true);
            let ctx = canvas.context();
            assert!(ctx.set_font_css("32px ResidentColr"));
            for line in 0..4u32 {
                let text: String = (0..16)
                    .filter_map(|i| char::from_u32(0xf0100 + line * 16 + i))
                    .collect();
                ctx.fill_text(&text, 4.0, 40.0 + line as f32 * 40.0, None);
            }
            canvas
                .to_image_data(None, &crate::pixels::ImageDataExport::default())
                .expect("pixels");
        };
        // What a page made textures of, what it drew from the CPU, and what
        // atlases it made.
        let uploads = |draw: &dyn Fn()| {
            let before = (
                TEXTURES_MADE.get(),
                RASTERS_ON_GPU.get(),
                ATLASES_MADE.get(),
            );
            draw();
            (
                TEXTURES_MADE.get() - before.0,
                RASTERS_ON_GPU.get() - before.1,
                ATLASES_MADE.get() - before.2,
            )
        };
        let first = uploads(&page);
        let later: Vec<_> = (0..4).map(|_| uploads(&page)).collect();
        RenderingEngine::GPU.with_direct_context(|context| {
            if let Some(context) = context {
                context.set_resource_cache_limit(limit);
            }
        });
        assert!(first.0 > 0, "the first page made no texture");
        assert_eq!(first.1, 0, "the first page drew glyphs from the CPU");
        assert!(first.2 > 0, "the first page made no atlas");
        // The third page adds the glyphs the second drew again to the
        // sheet, which is uploaded once -- see `grow_sheets` -- and draws
        // from it; no page uploads a glyph again.
        assert_eq!(
            later,
            [(0, 0, 0), (1, 0, 0), (0, 0, 0), (0, 0, 0)],
            "later pages uploaded glyphs again"
        );
    }

    /// The pixels of `scene` drawn on a canvas, on the GPU where `gpu`, with
    /// two lines of the test font's code points in it -- the colour glyphs
    /// among them broken by those it gives no colour paint -- and how many
    /// atlases were drawn from; each colour glyph drawn as an image of its
    /// own where `one_by_one`.
    fn atlas_scene(
        gpu: bool,
        one_by_one: bool,
        scene: &AtlasScene,
    ) -> (Vec<u8>, usize) {
        let mut canvas = crate::canvas::Canvas::new(600.0, 240.0);
        canvas.set_gpu(gpu);
        let ctx = canvas.context();
        ctx.set_fill_style_css("rgb(200 120 60)").expect("a colour");
        ctx.fill_rect(0.0, 0.0, 600.0, 240.0);
        ctx.set_fill_style_css("rgb(40 90 160)").expect("a colour");
        assert!(ctx.set_font_css("32px AtlasColr"));
        scene(ctx);
        ONE_BY_ONE.set(one_by_one);
        let before = ATLAS_DRAWS.get();
        // Premultiplied, as the pixels are stored: a translucent result read
        // back unpremultiplied moves a level of difference up to 255.
        let export = crate::pixels::ImageDataExport {
            pixels: crate::pixels::PixelExportOptions {
                premultiplied: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let pixels = canvas
            .to_image_data(None, &export)
            .expect("pixels")
            .pixels()
            .to_vec();
        ONE_BY_ONE.set(false);
        (pixels, ATLAS_DRAWS.get() - before)
    }

    /// `count` of the test font's code points from `first`, as text, each
    /// one a colour glyph of the font -- see [`assert_colour_text`].
    fn code_points(first: u32, count: u32) -> String {
        let text: String =
            (first..first + count).filter_map(char::from_u32).collect();
        assert_colour_text(VARIABLE, &text);
        text
    }

    /// What a test draws on a canvas [`atlas_scene`] sets up.
    type AtlasScene = dyn Fn(&mut crate::context2d::Context2D);

    /// Two lines of 16 of the test font's colour glyphs, every one of them
    /// a code point the font maps: past U+F0103 the font maps none until
    /// U+F0200, and a character it lacks falls back to a platform face.
    fn atlas_lines(ctx: &mut crate::context2d::Context2D) {
        ctx.fill_text(&code_points(0xf0200, 16), 4.0, 40.0, None);
        ctx.fill_text(&code_points(0xf0210, 16), 4.0, 90.0, None);
    }

    /// The scenes the atlas is held to drawing one by one with: each named,
    /// with whether its glyphs are drawn from an atlas.
    fn atlas_scenes() -> Vec<(&'static str, bool, Box<AtlasScene>)> {
        use crate::{filter::ColorFilter, prelude::*};
        let with =
            |set: fn(&mut crate::context2d::Context2D)| -> Box<AtlasScene> {
                Box::new(move |ctx| {
                    set(ctx);
                    atlas_lines(ctx);
                })
            };
        vec![
            ("plain", true, with(|_| {})),
            (
                "overlapping",
                true,
                with(|ctx| ctx.set_letter_spacing(-14.0)),
            ),
            (
                "overlapping at half alpha",
                true,
                with(|ctx| {
                    ctx.set_letter_spacing(-14.0);
                    ctx.set_global_alpha(0.5);
                }),
            ),
            (
                "multiplied",
                false,
                with(|ctx| {
                    ctx.set_global_composite_operation(BlendMode::Multiply);
                }),
            ),
            (
                "copied",
                true,
                with(|ctx| ctx.set_global_composite_operation(BlendMode::Copy)),
            ),
            (
                "source-in",
                true,
                with(|ctx| {
                    ctx.set_global_composite_operation(BlendMode::SourceIn);
                }),
            ),
            (
                "lighter",
                true,
                with(|ctx| {
                    ctx.set_global_composite_operation(BlendMode::Lighter);
                }),
            ),
            (
                "through a colour matrix",
                true,
                with(|ctx| {
                    // Sepia's.
                    let filter = ColorFilter::matrix([
                        0.393, 0.769, 0.189, 0.0, 0.0, 0.349, 0.686, 0.168,
                        0.0, 0.0, 0.272, 0.534, 0.131, 0.0, 0.0, 0.0, 0.0, 0.0,
                        1.0, 0.0,
                    ])
                    .expect("a matrix");
                    ctx.set_color_filter(Some(&filter));
                }),
            ),
            (
                "rotated",
                true,
                with(|ctx| {
                    ctx.translate(60.0, 0.0);
                    ctx.rotate(0.4);
                }),
            ),
            (
                "clipped",
                true,
                with(|ctx| {
                    ctx.begin_path();
                    ctx.rect(30.0, 20.0, 300.0, 50.0);
                    ctx.clip(FillRule::NonZero);
                }),
            ),
            (
                "in reverse after in order",
                true,
                Box::new(|ctx| {
                    let text = code_points(0xf0200, 16);
                    let reversed: String = text.chars().rev().collect();
                    ctx.fill_text(&text, 4.0, 40.0, None);
                    ctx.fill_text(&reversed, 4.0, 90.0, None);
                }),
            ),
            (
                "past one atlas",
                true,
                Box::new(|ctx| {
                    // Forty of one glyph 180 pixels across, overlapping, in
                    // atlases that hold 30.
                    ctx.scale(7.0, 7.0);
                    ctx.set_letter_spacing(-34.0);
                    let text = code_points(0xf0100, 1).repeat(40);
                    ctx.fill_text(&text, 0.0, 30.0, None);
                }),
            ),
        ]
    }

    #[test]
    fn glyphs_drawn_from_an_atlas_are_the_glyphs_drawn_one_by_one() {
        // On the GPU a run's colour glyphs are copied into an atlas and
        // drawn from it with one drawAtlas, and on the CPU drawn under one
        // transform; each must land, blend and clip exactly as its own
        // image drawn on its own did.
        use crate::{gpu::RenderingEngine, prelude::*};
        let library = FontLibrary::new();
        library
            .use_paths(Some("AtlasColr"), [VARIABLE])
            .expect("the font registers");
        for (name, _, scene) in atlas_scenes() {
            // The CPU canvas draws a run's images under one transform, not
            // from an atlas, as each glyph drawn on its own draws its image.
            let (drawn, from_atlases) = atlas_scene(false, false, &scene);
            let (alone, _) = atlas_scene(false, true, &scene);
            assert_eq!(from_atlases, 0, "{name}: an atlas on the CPU");
            let apart = zip(&drawn, &alone).filter(|(a, b)| a != b).count();
            assert_eq!(apart, 0, "{name}: bytes apart on the CPU");
        }
        if !RenderingEngine::GPU.selectable() {
            // A Mac built with Metal always has a GPU, so there a missing one
            // is a failure; a build without a GPU backend, or CI's Linux
            // runners, say the GPU half did not run.
            if cfg!(all(target_os = "macos", feature = "metal")) {
                panic!("no GPU on macOS");
            }
            eprintln!("skipped: no GPU");
            return;
        }
        for (name, atlased, scene) in atlas_scenes() {
            let (drawn, from_atlases) = atlas_scene(true, false, &scene);
            let (alone, none) = atlas_scene(true, true, &scene);
            assert_eq!(none, 0, "{name}: drawn one by one");
            match atlased {
                true => assert!(from_atlases > 0, "{name}"),
                false => assert_eq!(from_atlases, 0, "{name}"),
            }
            if name == "past one atlas" {
                assert!(from_atlases > 1, "{name}: one atlas");
            }
            let apart = zip(&drawn, &alone).filter(|(a, b)| a != b).count();
            assert_eq!(apart, 0, "{name}: bytes apart");
        }
    }

    /// The GPU's [`RenderingEngine`](crate::gpu::RenderingEngine), or
    /// `None` where this machine has none -- a failure on macOS, which
    /// always has Metal.
    #[cfg(any(feature = "metal", feature = "vulkan"))]
    fn gpu_or_skip() -> bool {
        if crate::gpu::RenderingEngine::GPU.selectable() {
            return true;
        }
        if cfg!(target_os = "macos") {
            panic!("no GPU on macOS");
        }
        eprintln!("skipped: no GPU");
        false
    }

    /// Four lines of 16 of the test font's colour glyphs on a GPU canvas,
    /// each line in `colour(line)`, read back premultiplied; each glyph
    /// drawn as an image of its own where `one_by_one`.
    #[cfg(any(feature = "metal", feature = "vulkan"))]
    fn sheet_page(colour: &dyn Fn(u32) -> String, one_by_one: bool) -> Vec<u8> {
        let mut canvas = crate::canvas::Canvas::new(600.0, 200.0);
        canvas.set_gpu(true);
        let ctx = canvas.context();
        assert!(ctx.set_font_css("32px SheetColr"));
        for line in 0..4u32 {
            ctx.set_fill_style_css(&colour(line)).expect("a colour");
            let text = code_points(0xf0200 + line * 16, 16);
            ctx.fill_text(&text, 4.0, 40.0 + line as f32 * 44.0, None);
        }
        ONE_BY_ONE.set(one_by_one);
        let export = crate::pixels::ImageDataExport {
            pixels: crate::pixels::PixelExportOptions {
                premultiplied: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let pixels = canvas
            .to_image_data(None, &export)
            .expect("pixels")
            .pixels()
            .to_vec();
        ONE_BY_ONE.set(false);
        pixels
    }

    /// What `draw` returns, and what it added to the counts: sheet
    /// generations made, runs drawn from a sheet, textures made and run
    /// atlases made.
    #[cfg(any(feature = "metal", feature = "vulkan"))]
    fn sheet_counts<T>(
        draw: impl FnOnce() -> T,
    ) -> (T, (usize, usize, usize, usize)) {
        let before = (
            SHEET_GENERATIONS.get(),
            SHEET_DRAWS.get(),
            TEXTURES_MADE.get(),
            ATLASES_MADE.get(),
        );
        let drawn = draw();
        let counts = (
            SHEET_GENERATIONS.get() - before.0,
            SHEET_DRAWS.get() - before.1,
            TEXTURES_MADE.get() - before.2,
            ATLASES_MADE.get() - before.3,
        );
        (drawn, counts)
    }

    #[test]
    #[cfg(any(feature = "metal", feature = "vulkan"))]
    fn a_page_drawn_again_draws_its_glyphs_from_one_sheet() {
        // Each run drawn from an atlas of its own read a texture of its
        // own, and Ganesh drew a page of runs as that many draws. A page
        // drawn again draws every run from one sheet: the first page that
        // draws a glyph again adds it, once, and the pages after it upload
        // nothing and make no atlas.
        if !gpu_or_skip() {
            return;
        }
        forget();
        crate::prelude::FontLibrary::new()
            .use_paths(Some("SheetColr"), [VARIABLE])
            .expect("the font registers");
        let blue = |_| "rgb(40 90 160)".to_string();
        let alone = sheet_page(&blue, true);
        forget();
        let pages: Vec<_> = (0..5)
            .map(|_| sheet_counts(|| sheet_page(&blue, false)))
            .collect();
        let counts: Vec<_> = pages.iter().map(|(_, counts)| *counts).collect();
        // The first page makes the runs' atlases; the second draws them
        // again and wants their glyphs; the third adds them in one
        // generation, uploaded once, and draws all four runs from it.
        assert_eq!(counts[0].0 + counts[0].1, 0, "a sheet on the first page");
        assert!(counts[0].3 > 0, "the first page made no atlas");
        assert_eq!(counts[1], (0, 0, 0, 0), "the second page");
        assert_eq!(counts[2], (1, 4, 1, 0), "the third page");
        assert_eq!(counts[3..], [(0, 4, 0, 0); 2], "later pages");
        for (at, (pixels, _)) in pages.iter().enumerate() {
            let apart = zip(pixels, &alone).filter(|(a, b)| a != b).count();
            assert_eq!(apart, 0, "page {at}: bytes apart from one by one");
        }
        // A line in another colour makes new glyphs, which the next
        // generation adds beside the first's, copied over; the first
        // generation's texture is let go, and every glyph still lands as
        // drawn one by one.
        let first = sheet_image_id().expect("a sheet");
        let mixed = |line| match line {
            0 => "rgb(160 40 90)".to_string(),
            _ => "rgb(40 90 160)".to_string(),
        };
        let alone = sheet_page(&mixed, true);
        let pages: Vec<_> = (0..3)
            .map(|_| sheet_counts(|| sheet_page(&mixed, false)))
            .collect();
        assert_eq!(pages[2].1, (1, 4, 1, 0), "the second generation's page");
        assert!(held_texture_of(sheet_image_id().expect("a sheet")));
        assert!(!held_texture_of(first), "the first generation kept");
        let apart = zip(&pages[2].0, &alone).filter(|(a, b)| a != b).count();
        assert_eq!(apart, 0, "bytes apart from one by one");
        forget();
        assert!(
            SHEETS.with_borrow(Vec::is_empty),
            "a sheet kept past forget"
        );
    }

    /// The unique id of the first sheet's current generation.
    #[cfg(any(feature = "metal", feature = "vulkan"))]
    fn sheet_image_id() -> Option<u32> {
        SHEETS.with_borrow(|sheets| {
            sheets.first()?.image.as_ref().map(Image::unique_id)
        })
    }

    /// Whether this thread holds a texture made of the image `id` names.
    #[cfg(any(feature = "metal", feature = "vulkan"))]
    fn held_texture_of(id: u32) -> bool {
        TEXTURES.with_borrow(|held| {
            held.iter().any(|(_, textures)| textures.contains_key(&id))
        })
    }

    #[test]
    #[cfg(any(feature = "metal", feature = "vulkan"))]
    fn glyphs_new_on_every_page_add_nothing_to_a_sheet() {
        // A line in a colour of its own on every page makes every glyph
        // image new: none is drawn twice, so none is added, and no page
        // pays for a generation.
        if !gpu_or_skip() {
            return;
        }
        forget();
        crate::prelude::FontLibrary::new()
            .use_paths(Some("SheetColr"), [VARIABLE])
            .expect("the font registers");
        let counts: Vec<_> = (0..4u32)
            .map(|page| {
                let colour = move |line: u32| {
                    format!("rgb({} {} 160)", 10 + page * 40, 10 + line * 40)
                };
                sheet_counts(|| sheet_page(&colour, false)).1
            })
            .collect();
        assert!(
            counts.iter().all(|(made, drawn, _, _)| made + drawn == 0),
            "a sheet for glyphs never drawn twice: {counts:?}"
        );
    }

    #[test]
    #[cfg(any(feature = "metal", feature = "vulkan"))]
    fn a_sheet_draws_every_atlas_scene_as_its_glyphs_drawn_one_by_one() {
        // Drawn three times, a scene's runs come from the sheet wherever
        // they come from an atlas at all; each must land, blend and clip as
        // its glyphs drawn one by one.
        use crate::prelude::*;
        if !gpu_or_skip() {
            return;
        }
        FontLibrary::new()
            .use_paths(Some("AtlasColr"), [VARIABLE])
            .expect("the font registers");
        for (name, atlased, scene) in atlas_scenes() {
            forget();
            let (alone, _) = atlas_scene(true, true, &scene);
            atlas_scene(true, false, &scene);
            atlas_scene(true, false, &scene);
            let before = SHEET_DRAWS.get();
            let (drawn, _) = atlas_scene(true, false, &scene);
            let from_sheet = SHEET_DRAWS.get() - before;
            match atlased {
                true => assert!(from_sheet > 0, "{name}: not from the sheet"),
                false => assert_eq!(from_sheet, 0, "{name}: from the sheet"),
            }
            let apart = zip(&drawn, &alone).filter(|(a, b)| a != b).count();
            assert_eq!(apart, 0, "{name}: bytes apart");
        }
    }

    #[test]
    #[cfg(any(feature = "metal", feature = "vulkan"))]
    fn a_full_sheet_leaves_the_glyphs_past_it_to_their_runs_atlases() {
        // A sheet two rows of glyphs deep holds the first line's glyphs and
        // fills part way through the second's: that line keeps its own
        // atlas, and both land as their glyphs drawn one by one.
        use crate::prelude::*;
        if !gpu_or_skip() {
            return;
        }
        FontLibrary::new()
            .use_paths(Some("AtlasColr"), [VARIABLE])
            .expect("the font registers");
        let scene: Box<AtlasScene> = Box::new(|ctx| {
            ctx.scale(3.0, 3.0);
            ctx.fill_text(&code_points(0xf0200, 16), 2.0, 30.0, None);
            ctx.fill_text(&code_points(0xf0210, 16), 2.0, 70.0, None);
        });
        forget();
        // The glyphs at three times are 68 pixels square, 15 to a row: the
        // first line takes a row and one of the second, and the second line's
        // last two would take a third.
        SHEET_DEPTH.set(Some(136));
        let (alone, _) = atlas_scene(true, true, &scene);
        atlas_scene(true, false, &scene);
        atlas_scene(true, false, &scene);
        let (sheet_draws, atlas_draws) = (SHEET_DRAWS.get(), ATLAS_DRAWS.get());
        let (drawn, _) = atlas_scene(true, false, &scene);
        let from_sheet = SHEET_DRAWS.get() - sheet_draws;
        let from_atlases = ATLAS_DRAWS.get() - atlas_draws - from_sheet;
        let full =
            SHEETS.with_borrow(|sheets| sheets.iter().any(|sheet| sheet.full));
        SHEET_DEPTH.set(None);
        assert!(full, "the sheet never filled");
        assert!(from_sheet > 0, "nothing drawn from the sheet");
        assert!(from_atlases > 0, "nothing past the sheet");
        let apart = zip(&drawn, &alone).filter(|(a, b)| a != b).count();
        assert_eq!(apart, 0, "bytes apart");
    }

    /// How far a premultiplied channel of the GPU canvas may stand from the
    /// CPU canvas's where one colour glyph image composites over another:
    /// a level each way from the exact composite. The GPU misses it only
    /// within [`GPU_TIE_LEVELS`] of a tie -- see
    /// `the_second_of_two_overlapping_glyphs_misses_the_exact_composite_on_the_gpu_only_near_a_tie`
    /// -- and the CPU raster rounds an alpha below 1 into its arithmetic up
    /// to 1.24 levels from it. That raster is Chrome's CPU canvas byte for
    /// byte for one glyph image over the page; for one over another it is
    /// the same raster, not compared with Chrome. Read from a copy of the page
    /// taken before the glyph beneath landed, an overlap stood 11 to 197
    /// levels off.
    const OVERLAP_LEVELS: u8 = 2;

    /// How near a tie of the exact composite, in levels, the GPU may store a
    /// channel a level from it: the bound `GPU_COMPOSITE_TIE_LEVELS` in
    /// `tests/colour_fonts.rs` holds the GPU canvas to for one glyph image
    /// over the page, as `docs/browser-differences.md` gives it for any
    /// image.
    const GPU_TIE_LEVELS: f64 = 0.18;

    /// [`OVERLAP_LEVELS`] on Mesa's llvmpipe: `Exclusion` stands three
    /// levels from the CPU canvas there for colour glyphs and for plain
    /// images alike, where Metal stands two -- see the llvmpipe entry in
    /// `docs/browser-differences.md`.
    const LLVMPIPE_OVERLAP_LEVELS: u8 = 3;

    /// Whether this machine's GPU canvas is Mesa's llvmpipe, the software
    /// Vulkan the Linux binding job draws on. It blends a coefficient blend
    /// at eight bits in its fixed-function stage, as the CPU raster does:
    /// a level from the exact composite at any distance from a tie, never
    /// past it -- see `docs/browser-differences.md`.
    fn on_llvmpipe() -> bool {
        crate::canvas::BackendInfo::query()
            .device
            .is_some_and(|device| device.contains("llvmpipe"))
    }

    #[test]
    fn overlapping_colour_glyphs_blend_on_the_gpu_as_on_the_cpu() {
        // Four of the test font's colour glyphs drawn into one another.
        // Under a blend that reads the page in the shader, the GPU reads one
        // copy of it for each draw, so the glyphs of one atlas drew over a
        // page without the glyphs before them.
        use crate::{gpu::RenderingEngine, paint::BlendMode::*, prelude::*};
        if !RenderingEngine::GPU.selectable() {
            // A Mac built with Metal always has a GPU, so there a missing one
            // is a failure; a build without a GPU backend, or CI's Linux
            // runners, say the GPU half did not run.
            if cfg!(all(target_os = "macos", feature = "metal")) {
                panic!("no GPU on macOS");
            }
            eprintln!("skipped: no GPU");
            return;
        }
        let library = FontLibrary::new();
        library
            .use_paths(Some("AtlasColr"), [VARIABLE])
            .expect("the font registers");
        let every = [
            SourceOver,
            SourceIn,
            SourceOut,
            SourceAtop,
            DestinationOver,
            DestinationIn,
            DestinationOut,
            DestinationAtop,
            Copy,
            Xor,
            Multiply,
            Screen,
            Overlay,
            Darken,
            Lighten,
            ColorDodge,
            ColorBurn,
            HardLight,
            SoftLight,
            Difference,
            Exclusion,
            Hue,
            Saturation,
            Color,
            Luminosity,
            Lighter,
            Clear,
        ];
        for blend in every {
            for alpha in [1.0, 0.5] {
                let scene = move |ctx: &mut crate::context2d::Context2D| {
                    ctx.set_global_composite_operation(blend);
                    ctx.set_global_alpha(alpha);
                    ctx.set_letter_spacing(-18.0);
                    ctx.fill_text(&code_points(0xf0100, 4), 4.0, 40.0, None);
                };
                let (cpu, _) = atlas_scene(false, false, &scene);
                let (gpu, _) = atlas_scene(true, false, &scene);
                let most = zip(&cpu, &gpu).map(|(a, b)| a.abs_diff(*b)).max();
                let bound = match on_llvmpipe() {
                    true => LLVMPIPE_OVERLAP_LEVELS,
                    false => OVERLAP_LEVELS,
                };
                assert!(
                    most <= Some(bound),
                    "{blend:?} at {alpha}: {most:?} levels apart"
                );
            }
        }
    }

    /// The premultiplied pixels of a canvas over `rgb(200 120 60)`, on the
    /// GPU where `gpu`, with the test font set at 32px in `rgb(40 90 160)`
    /// and `draw` drawn on it; over nothing where `bare`.
    fn overlap_page(
        gpu: bool,
        bare: bool,
        draw: &dyn Fn(&mut crate::context2d::Context2D),
    ) -> Vec<u8> {
        let mut canvas = crate::canvas::Canvas::new(120.0, 60.0);
        canvas.set_gpu(gpu);
        let ctx = canvas.context();
        if !bare {
            ctx.set_fill_style_css("rgb(200 120 60)").expect("a colour");
            ctx.fill_rect(0.0, 0.0, 120.0, 60.0);
        }
        ctx.set_fill_style_css("rgb(40 90 160)").expect("a colour");
        assert!(ctx.set_font_css("32px AtlasColr"));
        draw(ctx);
        let export = crate::pixels::ImageDataExport {
            pixels: crate::pixels::PixelExportOptions {
                premultiplied: true,
                ..Default::default()
            },
            ..Default::default()
        };
        canvas
            .to_image_data(None, &export)
            .expect("pixels")
            .pixels()
            .to_vec()
    }

    /// A blend's composite of a premultiplied channel `s` of alpha `sa`
    /// over `d` of alpha `da`, all from 0 to 1; the last argument says
    /// whether the channel is the alpha.
    type Composite = fn(f64, f64, f64, f64, bool) -> f64;

    #[test]
    fn the_second_of_two_overlapping_glyphs_misses_the_exact_composite_on_the_gpu_only_near_a_tie()
     {
        // Two colour glyphs drawn as one run, the second over the first.
        // Each channel the second covers is the exact composite of its
        // premultiplied bytes, scaled by the alpha, over the page the first
        // left, on the GPU a level from that only near a tie -- on llvmpipe,
        // which blends at eight bits, a level from it anywhere. Drawn from
        // one atlas under a blend that reads the page in the shader, the
        // second read the page as it was before the first landed -- see
        // `ColourRun::draw_all`.
        use crate::{gpu::RenderingEngine, paint::BlendMode::*, prelude::*};
        if !RenderingEngine::GPU.selectable() {
            // A Mac built with Metal always has a GPU, so there a missing one
            // is a failure; a build without a GPU backend, or CI's Linux
            // runners, say the GPU half did not run.
            if cfg!(all(target_os = "macos", feature = "metal")) {
                panic!("no GPU on macOS");
            }
            eprintln!("skipped: no GPU");
            return;
        }
        let library = FontLibrary::new();
        library
            .use_paths(Some("AtlasColr"), [VARIABLE])
            .expect("the font registers");
        let (first, second) =
            (code_points(0xf0100, 1), code_points(0xf0101, 1));
        // The second glyph's origin, 18 pixels into the first's 32.
        let (at, overlap) = (4.0, 18.0);
        let spacing = {
            let mut canvas = crate::canvas::Canvas::new(10.0, 10.0);
            let ctx = canvas.context();
            assert!(ctx.set_font_css("32px AtlasColr"));
            overlap - ctx.measure_text(&first, None).width
        };
        let source = overlap_page(false, true, &|ctx| {
            ctx.fill_text(&second, at + overlap, 40.0, None);
        });
        let blends: [(_, Composite); 6] = [
            (SourceOver, |s, sa, d, _, _| s + d * (1.0 - sa)),
            (Multiply, |s, sa, d, da, alpha| match alpha {
                true => sa + da - sa * da,
                false => s * (1.0 - da) + d * (1.0 - sa) + s * d,
            }),
            (Darken, |s, sa, d, da, alpha| match alpha {
                true => sa + da * (1.0 - sa),
                false => s + d - (s * da).max(d * sa),
            }),
            (Difference, |s, sa, d, da, alpha| match alpha {
                true => sa + da * (1.0 - sa),
                false => s + d - 2.0 * (s * da).min(d * sa),
            }),
            (Xor, |s, sa, d, da, _| s * (1.0 - da) + d * (1.0 - sa)),
            (Lighter, |s, _, d, _, _| (s + d).min(1.0)),
        ];
        // On llvmpipe a coefficient blend lands within a level of the
        // exact composite at any distance from a tie.
        let eight_bit = on_llvmpipe();
        let (mut held, mut near) = (0, 0);
        for (blend, composite) in blends {
            // A `globalAlpha` of 0.5 is an alpha byte of 128.
            for (alpha, scale) in [(1.0, 1.0), (0.5, 128.0 / 255.0)] {
                let set = |ctx: &mut crate::context2d::Context2D| {
                    ctx.set_global_composite_operation(blend);
                    ctx.set_global_alpha(alpha);
                };
                let page = overlap_page(true, false, &|ctx| {
                    set(ctx);
                    ctx.fill_text(&first, at, 40.0, None);
                });
                let drawn = overlap_page(true, false, &|ctx| {
                    set(ctx);
                    ctx.set_letter_spacing(spacing);
                    let run = format!("{first}{second}");
                    ctx.fill_text(&run, at, 40.0, None);
                });
                for p in (0..source.len()).step_by(4) {
                    let sa = f64::from(source[p + 3]) / 255.0 * scale;
                    if sa == 0.0 {
                        continue;
                    }
                    let da = f64::from(page[p + 3]) / 255.0;
                    for c in p..p + 4 {
                        let s = f64::from(source[c]) / 255.0 * scale;
                        let d = f64::from(page[c]) / 255.0;
                        let exact = composite(s, sa, d, da, c == p + 3) * 255.0;
                        let got = f64::from(drawn[c]);
                        held += 1;
                        if got == exact.round() {
                            continue;
                        }
                        near += 1;
                        let from_tie = (exact - exact.floor() - 0.5).abs();
                        assert!(
                            (got - exact).abs() < 1.0
                                && (eight_bit || from_tie <= GPU_TIE_LEVELS),
                            "{blend:?} at {alpha}: the GPU stores {got} for \
                             {exact}, {from_tie} from a tie"
                        );
                    }
                }
            }
        }
        assert!(held > 10_000, "only {held} channels to hold");
        eprintln!("{held} channels held, {near} off the exact composite");
    }

    #[test]
    fn plain_glyphs_in_a_run_of_colour_glyphs_are_drawn_where_it_is_played() {
        // A run holding colour glyphs is drawn again where the page is
        // played, and its runs in a plain face -- here letters the colour
        // font lacks, falling back to the next family -- are drawn from
        // their blobs there: drawn alone, the same letters land on the same
        // bytes.
        use crate::{gpu::RenderingEngine, prelude::*};
        let library = FontLibrary::new();
        library
            .use_paths(Some("AtlasColr"), [VARIABLE])
            .expect("the font registers");
        library
            .use_paths(
                Some("AtlasPlain"),
                ["tests/assets/fonts/Oswald/Oswald-VariableFont_wght.ttf"],
            )
            .expect("the font registers");
        let colour = code_points(0xf0200, 1);
        let engines: &[bool] = match RenderingEngine::GPU.selectable() {
            true => &[false, true],
            false => &[false],
        };
        for &gpu in engines {
            let page = |text: &str| {
                let mut canvas = crate::canvas::Canvas::new(200.0, 60.0);
                canvas.set_gpu(gpu);
                let ctx = canvas.context();
                assert!(ctx.set_font_css("32px AtlasColr, AtlasPlain"));
                ctx.set_fill_style_css("rgb(40 90 160)").expect("a colour");
                ctx.fill_text(text, 4.0, 40.0, None);
                canvas
                    .to_image_data(
                        None,
                        &crate::pixels::ImageDataExport::default(),
                    )
                    .expect("pixels")
                    .pixels()
                    .to_vec()
            };
            let (mixed, alone) = (page(&format!("AB{colour}")), page("AB"));
            // The letters' columns, left of where the colour glyph starts.
            let letters = |pixels: &[u8]| -> Vec<u8> {
                pixels
                    .chunks(200 * 4)
                    .flat_map(|row| row[..40 * 4].to_vec())
                    .collect()
            };
            let inked = letters(&alone).chunks(4).filter(|p| p[3] > 0).count();
            assert!(inked > 100, "gpu {gpu}: the letters ink {inked} pixels");
            let (mixed, alone) = (letters(&mixed), letters(&alone));
            let apart = zip(&mixed, &alone).filter(|(a, b)| a != b).count();
            assert_eq!(apart, 0, "gpu {gpu}: the letters' bytes apart");
        }
    }

    #[test]
    fn a_glyph_clipped_at_a_fraction_on_the_gpu_stands_from_the_cpu_as_unclipped()
     {
        // Drawn as an image of its own under a clip edge at a fraction of a
        // pixel, a glyph on the GPU canvas stood up to 22 levels from the
        // CPU canvas, and 11 at half alpha; drawn from an atlas it stands no
        // further from it than unclipped, at either alpha, but for a level at
        // the clip edge on llvmpipe.
        use crate::{gpu::RenderingEngine, prelude::*};
        if !RenderingEngine::GPU.selectable() {
            // A Mac built with Metal always has a GPU, so there a missing one
            // is a failure; a build without a GPU backend, or CI's Linux
            // runners, say the GPU half did not run.
            if cfg!(all(target_os = "macos", feature = "metal")) {
                panic!("no GPU on macOS");
            }
            eprintln!("skipped: no GPU");
            return;
        }
        let library = FontLibrary::new();
        library
            .use_paths(Some("AtlasColr"), [VARIABLE])
            .expect("the font registers");
        let furthest = |scene: &AtlasScene| {
            let (cpu, _) = atlas_scene(false, false, scene);
            let (gpu, _) = atlas_scene(true, false, scene);
            zip(&cpu, &gpu).map(|(a, b)| a.abs_diff(*b)).max()
        };
        let unclipped = furthest(&|ctx| atlas_lines(ctx));
        let clipped = furthest(&|ctx| {
            ctx.begin_path();
            ctx.rect(30.5, 20.0, 300.0, 50.25);
            ctx.clip(FillRule::NonZero);
            atlas_lines(ctx);
        });
        // A clip edge's coverage enters the blend as an alpha does, which
        // llvmpipe blends at eight bits: a level from the CPU canvas there,
        // whose composite at full alpha is the exact one rounded.
        let edge = on_llvmpipe().then_some(1);
        assert!(
            clipped <= unclipped.max(edge),
            "{clipped:?} past {unclipped:?}"
        );
        let half = furthest(&|ctx| {
            ctx.set_global_alpha(0.5);
            atlas_lines(ctx);
        });
        let half_clipped = furthest(&|ctx| {
            ctx.set_global_alpha(0.5);
            ctx.begin_path();
            ctx.rect(30.5, 20.0, 300.0, 50.25);
            ctx.clip(FillRule::NonZero);
            atlas_lines(ctx);
        });
        assert!(half_clipped <= half, "{half_clipped:?} past {half:?}");
    }
}
