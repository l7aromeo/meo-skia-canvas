//! Colour glyphs of a font with a `COLR` table, drawn by this crate rather
//! than by the platform's scaler.
//!
//! On macOS a typeface is CoreText's, and CoreText on macOS 26 and 15 draws a
//! COLRv1 glyph as nothing while reporting the face as a colour one; on 27.2
//! it draws a COLRv0 face's glyphs as nothing. Chrome hands neither to
//! CoreText: `WebFontTypefaceFactory` in Blink (`web_font_typeface_factory.cc`)
//! makes every COLR web font, of either version, through Fontations on Apple
//! platforms. On Windows DirectWrite draws the COLRv1 test glyphs up to 215
//! levels inside them from Chrome's. So on macOS and Windows this module
//! walks a glyph's paint graph with skrifa, Fontations' own reader, and
//! records it as a [`Picture`], and the text routes draw that picture in
//! place of the platform's glyph -- see `node::typography::draw_colour_run`.
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
use std::{cell::RefCell, collections::HashMap, rc::Rc};

use crate::{
    export::{VectorFeatures, blend_features},
    started,
};

use skia_safe::{
    AlphaType, BlendMode, Canvas, ClipOp, Color, Color4f, ColorSpace,
    ColorType, Data, GlyphId, IPoint, IRect, ISize, Image, ImageFilter,
    ImageInfo, Matrix, Paint, Path, PathBuilder, Picture, PictureRecorder,
    Point, Rect, RoundOut, Shader, TileMode, Typeface,
    canvas::SaveLayerRec,
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
    raw::types::BoundingBox,
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
    /// The bytes of the offset table before the directory, and of each
    /// directory record.
    const OFFSET_TABLE: usize = 12;
    const RECORD: usize = 16;
    let mut sorted: Vec<&(&[u8; 4], Data)> = tables.iter().collect();
    sorted.sort_by_key(|(tag, _)| **tag);
    let count = sorted.len();
    let mut file = Vec::new();
    file.extend_from_slice(&SFNT_VERSION.to_be_bytes());
    file.extend_from_slice(&(count as u16).to_be_bytes());
    // The search fields are hints for a binary search, which skrifa does not
    // make; zero is a valid table directory to it.
    file.extend_from_slice(&[0; 6]);
    let mut at = OFFSET_TABLE + RECORD * count;
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
    FACES.with_borrow_mut(HashMap::clear);
    GLYPHS.with_borrow_mut(HashMap::clear);
    GLYPH_IMAGES.with_borrow_mut(|(images, held)| {
        images.clear();
        *held = 0;
    });
}

thread_local! {
    /// The answer [`colr_face`] gave for each typeface, by its unique id,
    /// which no other typeface in the process takes.
    static FACES: RefCell<HashMap<TypefaceId, Option<Rc<ColrFace>>>> =
        RefCell::new(HashMap::new());
    /// The glyphs [`colr_glyph`] recorded, by typeface, glyph and
    /// foreground colour -- see [`packed`].
    static GLYPHS: RefCell<HashMap<GlyphKey, Option<Rc<ColrGlyph>>>> =
        RefCell::new(HashMap::new());
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
        let largest = (shape.scale_x().hypot(shape.skew_y()))
            .max(shape.skew_x().hypot(shape.scale_y()))
            * self.face.upem;
        let Some((image, offset)) = glyph_image(
            picture,
            bounds,
            &shape,
            format,
            largest <= LARGEST_KEPT_TEXT,
        ) else {
            return true;
        };
        let mut composite = Paint::default();
        composite.set_blend_mode(paint.blend_mode_or(BlendMode::SrcOver));
        let (image, offset) = match paint.image_filter() {
            Some(filter) => {
                match filtered(image, offset, &canvas_shape, paint, &filter) {
                    Some(filtered) => filtered,
                    None => return true,
                }
            }
            None => {
                composite.set_alpha(paint.alpha());
                if let Some(filter) = paint.color_filter() {
                    composite.set_color_filter(filter);
                }
                (image, offset)
            }
        };
        canvas.save();
        canvas.reset_matrix();
        canvas.draw_image(
            &image,
            ((origin.x + offset.x) as f32, (origin.y + offset.y) as f32),
            Some(&composite),
        );
        canvas.restore();
        true
    }
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
}

impl Format {
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
        (info.color_type() != ColorType::Unknown).then(|| Format {
            color_type: info.color_type(),
            color_space: info.color_space(),
        })
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
type KeptImages = (HashMap<ImageKey, (Image, IPoint)>, usize);

thread_local! {
    /// The glyph images [`glyph_image`] made, and how many bytes they hold.
    static GLYPH_IMAGES: RefCell<KeptImages> =
        RefCell::new((HashMap::new(), 0));
}

#[cfg(test)]
thread_local! {
    /// How many glyph images [`glyph_image`] has made on this thread, for the
    /// tests to tell a kept image from a remade one.
    static IMAGES_MADE: Cell<usize> = const { Cell::new(0) };
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
        format.color_space.as_ref().map_or(0, ColorSpace::hash),
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
    let device = matrix.map_point(at);
    inverse.map_point(Point::new(
        (device.x + SNAP_ACROSS).floor(),
        (device.y + SNAP_DOWN).floor(),
    ))
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

    /// The format of an eight-bit sRGB page.
    fn srgb() -> Format {
        Format {
            color_type: ColorType::RGBA8888,
            color_space: Some(ColorSpace::new_srgb()),
        }
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
        let wide = Format {
            color_type: ColorType::RGBAF16,
            color_space: Some(ColorSpace::new_srgb()),
        };
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
}
