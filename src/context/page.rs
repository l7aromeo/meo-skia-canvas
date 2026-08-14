use crc::{CRC_32_ISO_HDLC, Crc};
use dashmap::DashMap;
use little_exif::{
    exif_tag::ExifTag, filetype::FileExtension, metadata::Metadata,
};
use neon::prelude::*;
use rayon::prelude::*;
use skia_safe::{
    AlphaType, Canvas as SkCanvas, ClipOp, Color, ColorSpace, ColorType,
    Document, IRect, ISize, Image as SkImage, ImageInfo, Matrix, Paint, Path,
    Picture, PictureRecorder, PixelGeometry, Rect, Size, Surface, SurfaceProps,
    SurfacePropsFlags,
    canvas::SrcRectConstraint,
    image::{BitDepth, CachingHint},
    images, jpeg_encoder, pdf, png_encoder, surfaces,
    svg::{self, canvas::Flags},
    webp_encoder,
};
use std::{
    fs,
    io::{BufWriter, Cursor, Write},
    path::Path as FilePath,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
};
const CRC32: Crc<u32> = Crc::<u32>::new(&CRC_32_ISO_HDLC);

use crate::{
    context::BoxedContext2D,
    encode::{self, Frame, SequenceSpec, Sink, color::ColorProfile},
    export::{
        Content, EncoderKind, ImageFormat, SvgFidelity, dots_per_inch,
        pixels_per_metre,
    },
    gpu::RenderingEngine,
    node::canvas::BoxedCanvas,
    pixels::PixelColorSpace,
};

/// The PDF `Producer` field: per the spec, "the product that is converting
/// this document to PDF", which is this crate. Built from the package
/// metadata so it cannot drift from Cargo.toml.
///
/// No version: the crate and the npm package version independently (0.x
/// against 4.x), so a number here would name a release most callers have
/// never heard of. `Creator` is deliberately left unset -- that field names
/// the application the document came from, which only the caller knows.
const PDF_PRODUCER: &str = concat!(
    env!("CARGO_PKG_NAME"),
    " <",
    env!("CARGO_PKG_REPOSITORY"),
    ">"
);

static CACHE: OnceLock<Arc<DashMap<usize, PageCache>>> = OnceLock::new();

//
// Deferred canvas (records drawing commands for later replay on an output
// surface)
//

/// The byte length `info` needs, refusing one Skia cannot address.
///
/// Skia measures a pixel buffer with a signed 32-bit byte count, so
/// `compute_min_byte_size` wraps past `i32::MAX` and the `vec![0; size]` that
/// follows aborts with "capacity overflow". Asking for a region larger than
/// the page is legitimate -- the part outside reads back transparent -- so it
/// has to fail as an error rather than a panic.
///
/// Every readback allocation goes through here. There were three, and only
/// one was guarded: `toBufferSync("raw", { colorType: "RGBAF32" })` on a
/// 12000-square page still aborted, which is 2.3 GB.
fn checked_byte_size(info: &ImageInfo) -> Result<usize, String> {
    let size = info.compute_min_byte_size();
    if size == 0 && !info.is_empty() || size > i32::MAX as usize {
        return Err(format!(
            "Requested image data is too large: {}x{} at {:?} exceeds the {} \
             byte limit Skia can address",
            info.width(),
            info.height(),
            info.color_type(),
            i32::MAX
        ));
    }
    Ok(size)
}

/// Where the JFIF segment's density fields begin, once the segment has been
/// found.
///
/// The segment runs marker, length, `JFIF\0`, two version bytes, then the
/// units byte and the two sixteen-bit densities this rewrites. Counted from
/// the marker rather than from the start of the file, because the marker is
/// not always at the start of the file.
const JFIF_DENSITY_AT: usize = 11;

/// How many bytes of a JFIF segment this rewrites: the units byte and the
/// two densities.
const JFIF_DENSITY_LEN: usize = 5;

/// The JFIF `units` value saying the two densities that follow are dots per
/// inch. Zero would mean no units and an aspect ratio.
const JFIF_UNITS_DPI: u8 = 1;

/// Where a RIFF file's first chunk begins: `RIFF`, a length, and `WEBP`.
const RIFF_FIRST_CHUNK: usize = 12;

/// The `VP8X` flag saying the file carries an EXIF chunk.
const VP8X_HAS_EXIF: u8 = 1 << 3;

/// The start of the JFIF segment in `jpeg`, if it has one.
///
/// Walked rather than assumed. This was a fixed `13..18`, which is right
/// only while the JFIF segment is the first thing after the two-byte start
/// marker -- true of what Skia writes today, and not something Skia
/// promises. A file that led with an EXIF or ICC segment instead would have
/// had five bytes of that segment overwritten with a density, and JPEG has
/// no checksum to notice.
fn jfif_segment(jpeg: &[u8]) -> Option<usize> {
    // Segments run marker, two-byte length, payload; the length counts
    // itself. `FFDA` starts the compressed data, after which there are no
    // more segment headers to walk.
    let mut at = 2;
    while at + 4 <= jpeg.len() && jpeg[at] == 0xFF {
        let marker = jpeg[at + 1];
        if marker == 0xDA {
            return None;
        }
        let length = u16::from_be_bytes([jpeg[at + 2], jpeg[at + 3]]) as usize;
        // The density fields have to be there, not just the signature. A
        // JFIF APP0 declares 16 bytes of segment for them, and the caller
        // splices five bytes at offset 11 -- so accepting a shorter one on
        // the strength of `JFIF\0` alone would have it write past the end.
        // Walking for the segment exists precisely so the encoder's layout
        // is not assumed; assuming its length instead would give that back.
        let long_enough = length >= JFIF_DENSITY_AT + JFIF_DENSITY_LEN
            && jpeg.len() >= at + JFIF_DENSITY_AT + JFIF_DENSITY_LEN;
        if marker == 0xE0
            && long_enough
            && jpeg[at + 4..].starts_with(b"JFIF\0")
        {
            return Some(at);
        }
        at += 2 + length;
    }
    None
}

/// Whether `webp`'s first chunk is the extended-format header, which is the
/// only place the EXIF flag exists.
///
/// A plain lossy WebP has no `VP8X` and begins its image data where the flag
/// byte would be, so setting the flag unconditionally -- which is what
/// `bytes[20] |= 1 << 3` did -- would have flipped a bit inside the picture.
/// Skia writes `VP8X` today; nothing says it must.
fn webp_has_vp8x(webp: &[u8]) -> bool {
    webp.len() > RIFF_FIRST_CHUNK + 8
        && webp.starts_with(b"RIFF")
        && webp[8..12] == *b"WEBP"
        && webp[RIFF_FIRST_CHUNK..RIFF_FIRST_CHUNK + 4] == *b"VP8X"
}

/// How hard the WebP encoder works when it is being lossless.
///
/// Skia calls the field `quality` and it means something different in each
/// of its two modes: visual quality when lossy, and compression effort when
/// lossless, where the pixels come back identical whatever it is set to.
/// Nothing in `skia_safe` says so, which is why this sat as a bare `75.0`
/// beside a comment about quality it had nothing to do with.
///
/// Measured on a 300-square gradient with sixty hue bands, encoding the same
/// page at each setting:
///
/// | effort | bytes | time |
/// |-------:|------:|-----:|
/// | 0      | 3282  | 8.5ms |
/// | 25     | 2768  | 5.7ms |
/// | 50     | 2708  | 5.8ms |
/// | 75     | 2714  | 5.8ms |
/// | 100    | 2680  | 6.1ms |
///
/// So the dial is worth turning once, off the floor, and is flat after
/// that: everything from 25 up is within two percent of everything else.
/// 75 is kept rather than raised to 100 because the 1.3% it would buy is
/// smaller than the difference between two runs, and changing it would
/// change every lossless WebP this crate has written for no measurable
/// gain.
const WEBP_LOSSLESS_EFFORT: f32 = 75.0;

/// Where a PNG's first chunk after `IHDR` begins.
///
/// The eight-byte signature, then `IHDR`: four bytes of length, four of
/// type, thirteen of payload and four of checksum. Fixed by the format
/// rather than by what an encoder happens to emit -- every PNG has exactly
/// one `IHDR` and it is always first and always thirteen bytes -- so this is
/// derived from the parts instead of written as 33.
const PNG_AFTER_IHDR: usize = 8 + 4 + 4 + 13 + 4;

/// A compositing surface of `dims` in `space`, honouring the caller's float
/// format where the device can provide one.
///
/// Falls back to N32 when it cannot. Metal declines an `RGBAF32` render target
/// outright -- "Could not allocate new 4x4 bitmap (color type: RGBAF32)" --
/// and a canvas that refuses to draw is worse than one that composites at
/// eight bits and converts on the way out, which is what every canvas did
/// before float compositing existed.
fn make_compositing_surface(
    engine: &RenderingEngine,
    opts: &ExportOptions,
    dims: ISize,
    space: &ColorSpace,
) -> Result<Surface, String> {
    let info = opts.compositing_info(dims, space);
    let wanted = info.color_type();
    match engine.make_surface(&info, opts) {
        Ok(surface) => Ok(surface),
        Err(refused) if wanted != ColorType::N32 => engine
            .make_surface(&ImageInfo::new_n32_premul(dims, space.clone()), opts)
            .map_err(|_| refused),
        Err(refused) => Err(refused),
    }
}

pub struct PageRecorder {
    current: PictureRecorder,
    layers: Vec<Picture>,
    /// Index-aligned with `layers`: `true` where the layer holds a single
    /// draw Skia's SVG backend cannot express, which an SVG export
    /// rasterizes rather than writing out wrong. See
    /// [`PageRecorder::append_isolated`].
    rasterized: Vec<bool>,
    /// Set when such a draw appeared inside an open `saveLayer`, where it
    /// cannot be split into a layer of its own. The whole page rasterizes.
    raster_page: bool,
    bounds: Rect,
    matrix: Matrix,
    clip: Option<Path>,
    surface: RecordingSurface,
    changed: bool,
    id: usize,
    /// Save-stack depth that `restore()` rebuilds from. Normally 1, the
    /// recording canvas's own base. Each open `saveLayer` raises it so the
    /// layer frame survives a matrix or clip change; `layer_floors` holds the
    /// values to fall back to as those layers close.
    base_depth: usize,
    layer_floors: Vec<usize>,
}

impl PageRecorder {
    pub fn new(bounds: Rect) -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(1);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        PageCache::add(id);

        let mut rec = PictureRecorder::new();
        rec.begin_recording(bounds, true).save(); // start at depth 2

        PageRecorder {
            current: rec,
            layers: vec![],
            rasterized: vec![],
            raster_page: false,
            changed: false,
            matrix: Matrix::default(),
            clip: None,
            bounds,
            id,
            surface: RecordingSurface::default(),
            base_depth: 1,
            layer_floors: vec![],
        }
    }

    pub fn append<F>(&mut self, f: F)
    where
        F: FnOnce(&SkCanvas),
    {
        if let Some(canvas) = self.current.recording_canvas() {
            f(canvas);
            self.changed = true;
        }
    }

    pub fn set_bounds(&mut self, bounds: Rect) {
        *self = PageRecorder::new(bounds);
    }

    pub fn update_bounds(&mut self, bounds: Rect) {
        self.bounds = bounds; // non-destructively update the size
    }

    pub fn set_matrix(&mut self, matrix: Matrix) {
        self.matrix = matrix;
        self.restore();
    }

    pub fn set_clip(&mut self, clip: &Option<Path>) {
        self.clip = clip.clone();
        self.restore();
    }

    pub fn restore(&mut self) {
        let base_depth = self.base_depth;
        if let Some(canvas) = self.current.recording_canvas() {
            canvas.restore_to_count(base_depth);
            canvas.save();
            if let Some(clip) = &self.clip {
                canvas.clip_path(
                    clip,
                    ClipOp::Intersect,
                    true, /* antialias */
                );
            }
            canvas.set_matrix(&self.matrix.into());
        }
    }

    /// Open a Skia layer, raising the floor `restore()` rebuilds from so the
    /// layer frame is not torn down by the next transform or clip.
    ///
    /// `f` performs the `save_layer` itself. It runs with the current clip and
    /// matrix already applied, so the layer inherits them, and a fresh frame is
    /// opened inside the layer afterwards for its contents.
    pub fn open_layer<F>(&mut self, f: F)
    where
        F: FnOnce(&SkCanvas),
    {
        let base_depth = self.base_depth;
        let clip = self.clip.clone();
        let matrix = self.matrix;
        let mut opened = None;

        if let Some(canvas) = self.current.recording_canvas() {
            canvas.restore_to_count(base_depth);
            canvas.save();
            if let Some(clip) = &clip {
                canvas.clip_path(clip, ClipOp::Intersect, true);
            }
            canvas.set_matrix(&matrix.into());
            f(canvas);
            opened = Some(canvas.save_count());
            self.changed = true;
        }

        if let Some(depth) = opened {
            self.layer_floors.push(base_depth);
            self.base_depth = depth;
            self.restore();
        }
    }

    /// Close the innermost layer opened by [`open_layer`], compositing it onto
    /// what is behind it, and drop the floor back to where it was.
    pub fn close_layer(&mut self) {
        let Some(previous) = self.layer_floors.pop() else {
            return;
        };
        let base_depth = self.base_depth;
        if let Some(canvas) = self.current.recording_canvas() {
            canvas.restore_to_count(base_depth);
            canvas.restore(); // pops the layer frame, compositing it
            self.changed = true;
        }
        self.base_depth = previous;
        self.restore();
    }

    pub fn get_pixels(
        &mut self,
        crop: IRect,
        opts: ExportOptions,
        engine: RenderingEngine,
    ) -> Result<Vec<u8>, String> {
        self.get_pixels_as(crop, opts, engine, AlphaType::Unpremul)
    }

    /// As [`PageRecorder::get_pixels`], with the destination alpha mode
    /// chosen by the caller.
    ///
    /// `getImageData` is unpremultiplied by definition, so the Node path
    /// never asks for anything else. The Rust API can, and Skia converts
    /// during readback.
    pub fn get_pixels_as(
        &mut self,
        crop: IRect,
        opts: ExportOptions,
        engine: RenderingEngine,
        alpha_type: AlphaType,
    ) -> Result<Vec<u8>, String> {
        // return an empty buffer if the requested rect is entirely outside the
        // canvas
        let dst_info = ImageInfo::new(
            (crop.width(), crop.height()),
            opts.color_type,
            alpha_type,
            opts.color_space.clone(),
        );

        let size = checked_byte_size(&dst_info)?;
        let mut dst_buffer: Vec<u8> = vec![0; size];
        if !self.bounds.intersects(Rect::from_irect(crop)) {
            return Ok(dst_buffer);
        }

        let page = self.get_page();
        self.surface.update(&page, &opts, &engine);

        match self.surface.copy_pixels(&dst_info, crop, &mut dst_buffer) {
            true => Ok(dst_buffer),
            false => Err(format!(
                "Could not get image data (format: {:?})",
                dst_info.color_type()
            )),
        }
    }

    /// Closes the open recording and files it as a layer.
    ///
    /// A no-op when nothing has been drawn since the last one, so calling it
    /// to open a segment boundary cannot leave an empty layer behind.
    fn flush(&mut self) {
        if !self.changed {
            return;
        }

        // store layer as a drawable (so copies are deduplicated) wrapped in
        // a picture (so it can be sent to other threads)
        if let Some(pict) = self
            .current
            .finish_recording_as_drawable()
            .and_then(|mut drawable| {
                let mut wrapper = PictureRecorder::new();
                wrapper
                    .begin_recording(self.bounds, true)
                    .draw_drawable(&mut drawable, None);
                wrapper.finish_recording_as_picture(None)
            })
        {
            self.layers.push(pict);
            self.rasterized.push(false);
        }

        // resume recording
        self.current.begin_recording(self.bounds, true);
        self.changed = false;
        self.restore();
    }

    /// Records one draw into a layer of its own, marked for rasterization.
    ///
    /// For the draws Skia's SVG backend would mangle -- see [`SvgFidelity`].
    /// Keeping them in their own layer is what lets an SVG export replace
    /// exactly those and leave every other draw as vectors; a raster patch
    /// painted over finished vector output would double-composite anything
    /// translucent underneath it.
    ///
    /// A draw inside an open `saveLayer` cannot be split out: closing the
    /// recording would composite the half-finished layer early, and the paint
    /// it was opened with is gone by then. Those mark the whole page instead,
    /// and the export rasterizes all of it.
    pub fn append_isolated<F>(&mut self, f: F)
    where
        F: FnOnce(&SkCanvas),
    {
        if !self.layer_floors.is_empty() {
            self.raster_page = true;
            self.append(f);
            return;
        }

        self.flush();

        let mut recorder = PictureRecorder::new();
        {
            let canvas = recorder.begin_recording(self.bounds, true);
            canvas.save();
            if let Some(clip) = &self.clip {
                canvas.clip_path(clip, ClipOp::Intersect, true);
            }
            canvas.set_matrix(&self.matrix.into());
            f(canvas);
        }

        if let Some(pict) = recorder.finish_recording_as_picture(None) {
            self.layers.push(pict);
            self.rasterized.push(true);
        }
    }

    pub fn get_page(&mut self) -> Page {
        self.flush();

        Page {
            layers: self.layers.clone(),
            rasterized: self.rasterized.clone(),
            raster_page: self.raster_page,
            bounds: self.bounds,
            id: self.id,
        }
    }

    pub fn get_page_for_export(
        &mut self,
        opts: &ExportOptions,
        engine: &RenderingEngine,
    ) -> Page {
        // update the PageCache with the surface bitmap (if it's valid for this
        // export)
        let page = self.get_page();
        if opts.is_raster()
            && let Some(image) =
                self.surface.snapshot_if_valid(&page, opts, engine)
        {
            PageCache::set(self.id, image, opts, self.surface.depth);
        }
        page
    }

    pub fn get_image(&mut self) -> Option<SkImage> {
        let size = self.bounds.size().to_floor();
        self.get_page().get_picture(None).and_then(|pict| {
            images::deferred_from_picture(
                pict,
                size,
                None,
                None,
                BitDepth::U8,
                Some(ColorSpace::new_srgb()),
                None,
            )
        })
    }
}

impl Drop for PageRecorder {
    fn drop(&mut self) {
        PageCache::drop(self.id);
    }
}

//
// Persistent GPU/CPU surface for caching intermediate results of getImageData()
//

pub struct RecordingSurface {
    surface: Option<Surface>,
    depth: usize,
    matte: Option<Color>,
    msaa: Option<usize>,
    gpu: Option<bool>,
    color_space: ColorSpace,
    density: f32,
}

impl Default for RecordingSurface {
    fn default() -> Self {
        Self {
            surface: None,
            depth: 0,
            matte: None,
            msaa: None,
            gpu: None,
            color_space: ColorSpace::new_srgb(),
            density: 0.0,
        }
    }
}

impl RecordingSurface {
    fn is_surface_stale(
        &mut self,
        page: &Page,
        opts: &ExportOptions,
        engine: &RenderingEngine,
    ) -> bool {
        let gpu_toggled =
            self.gpu != Some(matches!(engine, RenderingEngine::GPU));
        let page_size = page.scaled_dimensions(opts.density);
        let resized = self
            .surface
            .as_mut()
            .map(|surface| surface.image_info().dimensions() != page_size)
            .unwrap_or(true);

        gpu_toggled || resized
    }

    fn is_config_stale(&self, opts: &ExportOptions) -> bool {
        self.density != opts.density
            || self.matte != opts.matte
            || self.msaa != opts.msaa
            || self.color_space != opts.surface_color_space
    }

    pub fn update(
        &mut self,
        page: &Page,
        opts: &ExportOptions,
        engine: &RenderingEngine,
    ) {
        // check for anything that would invalidate the previous contents
        let reconfigure = self.is_config_stale(opts);
        let recreate = self.is_surface_stale(page, opts, engine);

        // start from scratch if invalidated
        if reconfigure || recreate {
            self.gpu = Some(matches!(engine, RenderingEngine::GPU));
            self.color_space = opts.surface_color_space.clone();
            self.density = opts.density;
            self.matte = opts.matte;
            self.msaa = opts.msaa;
            self.depth = 0;

            // only allocate a new surface if the dimensions (size * density)
            // have changed or engine switched
            if recreate {
                let page_size = page.scaled_dimensions(opts.density);
                // See `ExportOptions::compositing_color_type`: N32 unless a
                // float format was asked for, which is the one case that
                // belongs on the surface rather than on the readback.
                self.surface = make_compositing_surface(
                    engine,
                    opts,
                    page_size,
                    &opts.surface_color_space,
                )
                .ok();
            }
        }

        if let Some(surface) = self.surface.as_mut() {
            let canvas = surface.canvas();
            let (cache_image, cache_depth) =
                PageCache::get(page.id, opts, page.depth());

            if let Some(image) = cache_image {
                // use the cached bitmap as the background (if present)
                canvas.draw_image(image, (0, 0), None);
                self.depth = cache_depth;
            } else if self.depth == 0 {
                // otherwise, fill the canvas if requested
                canvas.clear(self.matte.unwrap_or(Color::TRANSPARENT));
            }

            // only add new layers to surface
            canvas.scale((self.density, self.density));

            // draw newly added layers
            for pict in page.layers.iter().skip(self.depth) {
                pict.playback(canvas);
            }
            self.depth = page.layers.len();
        }
    }

    pub fn snapshot_if_valid(
        &mut self,
        page: &Page,
        opts: &ExportOptions,
        engine: &RenderingEngine,
    ) -> Option<SkImage> {
        match !(self.is_config_stale(opts)
            || self.is_surface_stale(page, opts, engine)
            || self.depth == 0)
        {
            true => self
                .surface
                .as_mut()
                .map(|surface| surface.image_snapshot()),
            false => None,
        }
    }

    pub fn copy_pixels(
        &mut self,
        dst_info: &ImageInfo,
        src: IRect,
        pixels: &mut [u8],
    ) -> bool {
        self.surface
            .as_mut()
            .map(|surface| {
                surface.read_pixels(
                    dst_info,
                    pixels,
                    dst_info.min_row_bytes(),
                    (src.x(), src.y()),
                )
            })
            .unwrap_or(false)
    }
}

//
// Image generator for a single drawing context
//

#[derive(Debug, Clone)]
pub struct Page {
    pub id: usize,
    pub bounds: Rect,
    pub layers: Vec<Picture>,
    /// Index-aligned with `layers`; see [`PageRecorder::append_isolated`].
    pub rasterized: Vec<bool>,
    /// Whether every layer has to be rasterized for an SVG export.
    pub raster_page: bool,
}

impl PartialEq for Page {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id && self.depth() == other.depth()
    }
}

impl Default for Page {
    fn default() -> Self {
        Self {
            id: 0,
            bounds: skia_safe::Rect::new_empty(),
            layers: vec![],
            rasterized: vec![],
            raster_page: false,
        }
    }
}

impl Page {
    /// What an SVG export can do with the page as a whole.
    ///
    /// [`SvgFidelity::Raster`] as soon as one layer holds a draw Skia's SVG
    /// backend cannot express. A canvas drawn into another one arrives as a
    /// single flattened picture, and the marks would be lost with it, so the
    /// destination asks this and marks the whole draw.
    pub(crate) fn svg_fidelity(&self) -> SvgFidelity {
        match self.raster_page || self.rasterized.iter().any(|marked| *marked) {
            true => SvgFidelity::Raster,
            false => SvgFidelity::Vector,
        }
    }

    pub fn depth(&self) -> usize {
        self.layers.len()
    }

    pub fn scaled_dimensions(&self, density: f32) -> ISize {
        Size::new(
            self.bounds.width() * density,
            self.bounds.height() * density,
        )
        .to_floor()
    }

    /// Replays the page into an SVG canvas, rasterizing what Skia cannot say.
    ///
    /// Skia's SVG backend writes four paint servers and one filter (see
    /// [`SvgFidelity`](crate::export::SvgFidelity)) and silently omits the
    /// rest, so a sweep gradient came out black, a shadow came out flat and a
    /// blend mode came out as source-over. The layers holding those draws are
    /// marked when they are recorded; each one is rendered on its own here
    /// and drawn in as an image, which the backend does embed, so the
    /// document ends up saying what the canvas drew.
    ///
    /// Everything else stays vector: the marked layers are single draws, and
    /// they are replaced in place rather than painted over, so a translucent
    /// draw is composited once rather than twice.
    fn draw_as_svg(
        &self,
        canvas: &SkCanvas,
        matte: Option<Color>,
        density: f32,
    ) -> Result<(), String> {
        if let Some(color) = matte {
            canvas.clear(color);
        }

        // One draw could not be split into a layer of its own -- it was
        // inside an open `saveLayer` -- so the whole page goes in as pixels.
        if self.raster_page {
            return self.embed_raster(canvas, &self.layers, density);
        }

        // Consecutive marked layers go in as one image rather than one each.
        // Each embedded image costs a page-sized surface, a playback and a
        // scan for its bounds, and a scene draws these in runs -- sixty
        // shadowed panels in a row are sixty layers with nothing vector
        // between them. Rasterizing them separately took 1.1 seconds where
        // the same page without shadows took 8 milliseconds; as one run it is
        // a single image and the cost stops scaling with the count.
        let marked =
            |index: usize| self.rasterized.get(index).copied() == Some(true);
        let mut index = 0;
        while index < self.layers.len() {
            if !marked(index) {
                self.layers[index].playback(canvas);
                index += 1;
                continue;
            }

            let start = index;
            while index < self.layers.len() && marked(index) {
                index += 1;
            }
            self.embed_raster(canvas, &self.layers[start..index], density)?;
        }
        Ok(())
    }

    /// Renders a run of layers to pixels and draws the result into `canvas`.
    ///
    /// Cropped to the ink it actually laid down: a run covers a fraction of
    /// the page, and embedding the full page for each would bloat the
    /// document by the size of a PNG per run.
    fn embed_raster(
        &self,
        canvas: &SkCanvas,
        layers: &[Picture],
        density: f32,
    ) -> Result<(), String> {
        let dims = self.scaled_dimensions(density);
        let info = ImageInfo::new(
            dims,
            ColorType::RGBA8888,
            AlphaType::Premul,
            Some(ColorSpace::new_srgb()),
        );
        let mut surface = surfaces::raster(&info, None, None)
            .ok_or("Could not allocate a surface for the SVG fallback")?;

        let raster = surface.canvas();
        raster.scale((density, density));
        for layer in layers {
            layer.playback(raster);
        }

        let image = surface.image_snapshot();
        let Some(ink) = self.ink_bounds(&image) else {
            return Ok(()); // the layer drew nothing visible
        };

        let dst = Rect::new(
            ink.left as f32 / density,
            ink.top as f32 / density,
            ink.right as f32 / density,
            ink.bottom as f32 / density,
        );
        canvas.draw_image_rect(
            &image,
            Some((&Rect::from_irect(ink), SrcRectConstraint::Strict)),
            dst,
            &Paint::default(),
        );
        Ok(())
    }

    /// The smallest rectangle holding every non-transparent pixel, or `None`
    /// when the image is empty.
    fn ink_bounds(&self, image: &SkImage) -> Option<IRect> {
        let pixels = image.peek_pixels()?;
        let (width, height) =
            (pixels.width() as usize, pixels.height() as usize);
        let row_bytes = pixels.row_bytes();
        let bytes: &[u8] = pixels.bytes()?;

        let (mut top, mut bottom) = (height, 0usize);
        let (mut left, mut right) = (width, 0usize);
        for y in 0..height {
            let row = &bytes[y * row_bytes..y * row_bytes + width * 4];
            // RGBA8888: alpha is the fourth byte of each pixel.
            let Some(first) =
                row.chunks_exact(4).position(|pixel| pixel[3] != 0)
            else {
                continue;
            };
            let last = row
                .chunks_exact(4)
                .rposition(|pixel| pixel[3] != 0)
                .unwrap_or(first);
            top = top.min(y);
            bottom = y + 1;
            left = left.min(first);
            right = right.max(last + 1);
        }

        (bottom > top).then(|| {
            IRect::new(left as i32, top as i32, right as i32, bottom as i32)
        })
    }

    pub fn get_picture(&self, matte: Option<Color>) -> Option<Picture> {
        let mut compositor = PictureRecorder::new();
        let output = compositor.begin_recording(self.bounds, true);
        matte.map(|c| output.clear(c));
        self.layers.iter().for_each(|pict| pict.playback(output));
        compositor.finish_recording_as_picture(None)
    }

    pub fn encoded_as(
        &self,
        options: ExportOptions,
        engine: RenderingEngine,
    ) -> Result<Vec<u8>, String> {
        if self.bounds.is_empty() {
            return Err(
                "Width and height must be non-zero to generate an image"
                    .to_string(),
            );
        }
        options.check_timing()?;

        // Before anything is rasterized for Skia's encoders, because these
        // formats do not use them: they take pixels, and one page is a
        // one-frame animation.
        if options.format.traits().encoder == EncoderKind::Foreign {
            let frame =
                self.as_frame(&options, engine, options.delay_ms(0, 1))?;
            let spec = SequenceSpec {
                width: frame.width,
                height: frame.height,
                frames: 1,
                loops: options.loops,
                quality: (options.quality * 100.0).clamp(0.0, 100.0),
                density: options.density,
                color: options.encoded_color_profile(),
            };
            let mut bytes = Cursor::new(Vec::new());
            {
                let mut sink =
                    encode::start(options.format, &spec, &mut bytes)?;
                sink.write_frame(&frame)?;
                sink.finish()?;
            }
            return Ok(bytes.into_inner());
        }

        let ExportOptions {
            format,
            quality,
            density,
            matte,
            color_type,
            ref color_space,
            ref surface_color_space,
            ..
        } = options;
        let size = self.bounds.size();
        let img_dims = self.scaled_dimensions(density);
        // The *surface* space, not the requested one: drawing happens in the
        // canvas's own space and an export converts out of it, the way a
        // browser's canvas does. Building the surface in whatever the export
        // asked for made the compositing space a property of the call. The
        // format follows `compositing_color_type`, so a float canvas
        // composites in float and the "raw" branch below still honours
        // `color_type` on its destination info.
        let img_quality = ((quality * 100.0) as u32).clamp(0, 100);
        let img_scale = Matrix::scale((density, density)).into();

        match format {
            ImageFormat::Pdf => {
                let mut pdf_bytes = Vec::new();
                let metadata = pdf::Metadata {
                    producer: PDF_PRODUCER.to_string(),
                    encoding_quality: Some((quality * 100.0) as i32),
                    raster_dpi: Some(density * 72.0),
                    ..Default::default()
                };
                let mut document = pdf_document(&mut pdf_bytes, &metadata)
                    .begin_page(size, None);
                let canvas = document.canvas();
                let picture = self
                    .get_picture(matte)
                    .ok_or("Could not generate an image")?;
                canvas.draw_picture(&picture, None, None);
                document.end_page().close();
                Ok(pdf_bytes)
            }

            ImageFormat::Svg => {
                let canvas = svg::Canvas::new(
                    Rect::from_size(size),
                    options.svg_flags(),
                );
                self.draw_as_svg(&canvas, matte, density)?;
                Ok(canvas.end().as_bytes().to_vec())
            }

            // handle bitmap formats using (potentially gpu-backed) rasterizer
            _ => {
                let mut surface = make_compositing_surface(
                    &engine,
                    &options,
                    img_dims,
                    surface_color_space,
                )?;
                let canvas = surface.canvas();

                let (cache_image, cache_depth) =
                    PageCache::get(self.id, &options, self.depth());
                if let Some(image) = cache_image {
                    // use the cached bitmap as the background
                    canvas.draw_image(image, (0, 0), None);
                } else if let Some(color) = options.matte {
                    // otherwise, fill the canvas if requested
                    canvas.clear(color);
                }

                // draw newly added layers and cache the full-canvas bitmap
                canvas.set_matrix(&img_scale);
                for pict in self.layers.iter().skip(cache_depth) {
                    pict.playback(canvas);
                }

                // extract the results
                let context = &mut surface.direct_context();
                let image = surface
                    .make_temporary_image()
                    .unwrap_or_else(|| surface.image_snapshot());

                // The surface holds the canvas's space; an encoder tags with
                // whatever the image carries. Converting here is what makes a
                // requested output space mean anything -- without it a P3
                // export of an sRGB canvas came out sRGB, profile and all.
                //
                // Redrawn rather than `Image::make_color_space`, which
                // returns `None` for a GPU-backed image without a graphite
                // recorder: drawing into a surface of the target space
                // converts on both backends.
                let image = match surface_color_space == color_space {
                    true => image,
                    false => {
                        let out_info = ImageInfo::new_n32_premul(
                            img_dims,
                            color_space.clone(),
                        );
                        match engine.make_surface(&out_info, &options) {
                            Ok(mut converted) => {
                                converted.canvas().draw_image(
                                    &image,
                                    (0, 0),
                                    None,
                                );
                                converted.image_snapshot()
                            }
                            Err(_) => image,
                        }
                    }
                };

                // update cache
                if self.depth() > cache_depth {
                    if rayon::current_thread_index().is_some() {
                        // move bitmap off GPU if we're in a background thread
                        // and need to share
                        if let Some(raster) = image.make_non_texture_image(
                            &mut surface.direct_context(),
                        ) {
                            PageCache::set(
                                self.id,
                                raster,
                                &options,
                                self.depth(),
                            )
                        }
                    } else {
                        PageCache::set(
                            self.id,
                            image.clone(),
                            &options,
                            self.depth(),
                        );
                    }
                }

                // handle image encoding
                match format {
                    ImageFormat::Raw => {
                        // The requested space, not sRGB: the surface above
                        // was built in `color_space`, and pinning the
                        // destination to sRGB converted every raw export back
                        // down to it -- so `toBuffer("raw", {colorSpace})`
                        // silently returned sRGB bytes. `get_pixels_as`, which
                        // backs `getImageData`, passes the option through, and
                        // the two disagreed about the same picture.
                        let dst_info = ImageInfo::new(
                            img_dims,
                            color_type,
                            AlphaType::Unpremul,
                            color_space.clone(),
                        );
                        let mut buffer: Vec<u8> =
                            vec![0; checked_byte_size(&dst_info)?];
                        match surface.read_pixels(
                            &dst_info,
                            &mut buffer,
                            dst_info.min_row_bytes(),
                            (0, 0),
                        ) {
                            true => Some(buffer),
                            false => {
                                return Err(format!(
                                    "Could not encode as {} ({:?})",
                                    format.as_str(),
                                    color_type
                                ));
                            }
                        }
                    }

                    ImageFormat::Jpeg => {
                        let jpg_opts = jpeg_encoder::Options {
                            quality: img_quality,
                            downsample: match options.jpeg_downsample {
                                true => {
                                    jpeg_encoder::Downsample::BothDirections
                                }
                                false => jpeg_encoder::Downsample::No,
                            },
                            ..jpeg_encoder::Options::default()
                        };

                        jpeg_encoder::encode_image(context, &image, &jpg_opts)
                            .map(|data| {
                                let mut bytes = data.as_bytes().to_vec();
                                // One shared rule for all four formats
                                // that record a resolution -- see
                                // `export::dots_per_inch`. This site used to
                                // write `72 * density as u16`, where `as`
                                // binds tighter than `*` and truncated the
                                // density before it multiplied anything.
                                let [l, r] =
                                    dots_per_inch(density).to_be_bytes();
                                // Found rather than assumed to be at 13.
                                // A file with no JFIF segment keeps its
                                // resolution unstated, which is what it
                                // already said, rather than having five
                                // bytes of some other segment overwritten.
                                if let Some(at) = jfif_segment(&bytes) {
                                    let from = at + JFIF_DENSITY_AT;
                                    bytes.splice(
                                        from..from + JFIF_DENSITY_LEN,
                                        [JFIF_UNITS_DPI, l, r, l, r]
                                            .iter()
                                            .cloned(),
                                    );
                                }
                                bytes
                            })
                    }

                    ImageFormat::Png => {
                        let png_opts = png_encoder::Options::default();

                        png_encoder::encode_image(context, &image, &png_opts)
                            .map(|data| {
                                let mut bytes = data.as_bytes().to_vec();
                                let mut digest = CRC32.digest();
                                let [a, b, c, d] =
                                    pixels_per_metre(density).to_be_bytes();
                                let phys = vec![
                                    b'p', b'H', b'Y', b's', a, b, c,
                                    d, // x-dpi
                                    a, b, c, d, // y-dpi
                                    1, // dots per meter
                                ];
                                digest.update(&phys);

                                let length = 9u32.to_be_bytes().to_vec();
                                let checksum =
                                    digest.finalize().to_be_bytes().to_vec();
                                // Straight after `IHDR`, which is where
                                // every ancillary chunk may sit and where
                                // `pHYs` must sit -- before `IDAT`. The
                                // offset is derived from the format's own
                                // fixed parts rather than written as 33.
                                bytes.splice(
                                    PNG_AFTER_IHDR..PNG_AFTER_IHDR,
                                    [length, phys, checksum].concat(),
                                );
                                bytes
                            })
                    }

                    ImageFormat::Webp => {
                        let mut webp_opts = webp_encoder::Options::default();
                        if img_quality == 100 {
                            webp_opts.compression =
                                webp_encoder::Compression::Lossless;
                            // Effort, not quality -- see the constant.
                            webp_opts.quality = WEBP_LOSSLESS_EFFORT;
                        } else {
                            webp_opts.compression =
                                webp_encoder::Compression::Lossy;
                            webp_opts.quality = img_quality as _;
                        }

                        webp_encoder::encode_image(context, &image, &webp_opts)
                            .map(|data| {
                                let mut bytes = data.as_bytes().to_vec();

                                // The EXIF flag lives in the `VP8X` chunk,
                                // and a WebP without one begins its image
                                // data where that byte would be -- so this
                                // used to flip a bit inside the picture on
                                // any file Skia wrote without `VP8X`.
                                // Without the header there is nowhere to
                                // declare EXIF, so there is no point
                                // appending it either.
                                if !webp_has_vp8x(&bytes) {
                                    return bytes;
                                }
                                bytes[RIFF_FIRST_CHUNK + 8] |= VP8X_HAS_EXIF;

                                // append EXIF chunk with DPI
                                let dpi = f64::from(dots_per_inch(density));
                                let mut exif = Metadata::new();
                                exif.set_tag(ExifTag::XResolution(vec![
                                    dpi.into(),
                                ]));
                                exif.set_tag(ExifTag::YResolution(vec![
                                    dpi.into(),
                                ]));
                                if let Ok(mut exif_bytes) =
                                    exif.as_u8_vec(FileExtension::WEBP)
                                {
                                    bytes.append(&mut exif_bytes);
                                }

                                // update file-length field in RIFF header
                                let file_size =
                                    ((bytes.len() - 8) as u32).to_le_bytes();
                                bytes.splice(4..8, file_size.iter().cloned());

                                bytes
                            })
                    }
                    // Reached only if a format Skia does not encode slips
                    // past the branches above, which cannot happen while
                    // this match stays exhaustive -- but saying so as an
                    // error rather than a panic keeps a future format from
                    // aborting the process on its way in.
                    ImageFormat::Pdf
                    | ImageFormat::Svg
                    | ImageFormat::Gif
                    | ImageFormat::Apng
                    | ImageFormat::Tiff
                    | ImageFormat::Ico
                    | ImageFormat::Bmp
                    | ImageFormat::Avif => {
                        return Err(format!(
                            "{} is not encoded by Skia",
                            format.as_str()
                        ));
                    }
                }
                .ok_or(format!("Could not encode as {}", format.as_str()))
            }
        }
    }

    pub fn write(
        &self,
        filename: &str,
        options: ExportOptions,
        engine: RenderingEngine,
    ) -> Result<(), String> {
        let path = FilePath::new(&filename);
        let data = self.encoded_as(options, engine)?;
        fs::write(path, data)
            .map_err(|why| format!("{}: \"{}\"", why, path.display()))
    }

    /// Renders this page into a raster surface configured from
    /// `surface_options` and read the resulting pixels into the
    /// caller-supplied `dst_info`.
    ///
    /// Splits the two color configurations `encoded_as("raw", ...)`
    /// conflates: `surface_options` decides the compositing pixel
    /// format + color space (e.g. linear F32 for HDR-capable
    /// blending), while `dst_info` decides the wire format
    /// Skia converts the snapshot into (e.g. 8-bit sRGB
    /// premultiplied or unpremultiplied for a canvas paint path).
    ///
    /// Returns the packed pixel buffer sized to
    /// `dst_info.compute_min_byte_size()`.
    pub fn render_raw(
        &self,
        surface_options: ExportOptions,
        dst_info: ImageInfo,
        engine: RenderingEngine,
    ) -> Result<Vec<u8>, String> {
        if self.bounds.is_empty() {
            return Err(
                "Width and height must be non-zero to generate an image"
                    .to_string(),
            );
        }
        // No color_type here: the caller's dst_info already carries it.
        let ExportOptions {
            density,
            matte,
            ref surface_color_space,
            ..
        } = surface_options;
        let img_dims = self.scaled_dimensions(density);
        // The compositing surface takes the canvas's own space and
        // `compositing_color_type`. The destination space and any narrower
        // format are applied below, on the read_pixels destination, which is
        // where those conversions belong.
        let img_scale = Matrix::scale((density, density)).into();

        let mut surface = make_compositing_surface(
            &engine,
            &surface_options,
            img_dims,
            surface_color_space,
        )?;
        let canvas = surface.canvas();
        if let Some(color) = matte {
            canvas.clear(color);
        }
        canvas.set_matrix(&img_scale);
        for pict in self.layers.iter() {
            pict.playback(canvas);
        }

        let stride = dst_info.min_row_bytes();
        let mut buffer: Vec<u8> = vec![0; checked_byte_size(&dst_info)?];
        if surface.read_pixels(&dst_info, &mut buffer, stride, (0, 0)) {
            Ok(buffer)
        } else {
            Err(format!(
                "Could not read pixels into destination format ({:?} / {:?})",
                dst_info.color_type(),
                dst_info.alpha_type(),
            ))
        }
    }

    /// This page rasterized into the one layout every foreign encoder is
    /// promised: eight-bit RGBA, unpremultiplied, in
    /// [`ExportOptions::encoded_color_space`].
    ///
    /// Which is the canvas's own space where the container can declare one,
    /// and sRGB where it cannot. Every format here used to take sRGB
    /// unconditionally, on the grounds that none of them carried a profile
    /// -- true of GIF, and never true of the rest: TIFF has had
    /// `PrimaryChromaticities` since TIFF 6.0, BMP's V4 header has endpoints,
    /// and AVIF and PNG both name a space in four bytes. So a Display P3
    /// canvas exported to any of them was silently narrowed to sRGB.
    pub(crate) fn as_frame(
        &self,
        options: &ExportOptions,
        engine: RenderingEngine,
        delay_ms: u32,
    ) -> Result<Frame, String> {
        let dims = self.scaled_dimensions(options.density);
        let info = ImageInfo::new(
            dims,
            ColorType::RGBA8888,
            AlphaType::Unpremul,
            options.encoded_color_space()?,
        );
        let pixels = self.render_raw(options.clone(), info, engine)?;
        Ok(Frame {
            pixels,
            width: dims.width.max(0) as u32,
            height: dims.height.max(0) as u32,
            delay_ms,
        })
    }

    fn append_to<'a>(
        &self,
        doc: Document<'a>,
        matte: Option<Color>,
    ) -> Result<Document<'a>, String> {
        if !self.bounds.is_empty() {
            let mut doc = doc.begin_page(self.bounds.size(), None);
            let canvas = doc.canvas();
            if let Some(picture) = self.get_picture(matte) {
                canvas.draw_picture(&picture, None, None);
            }
            Ok(doc.end_page())
        } else {
            Err("Width and height must be non-zero to generate a PDF page"
                .to_string())
        }
    }
}

//
// Container for a canvas's entire stack of page contexts
//

pub struct PageSequence {
    pub pages: Vec<Page>,
    pub engine: RenderingEngine,
}

impl PageSequence {
    pub fn from(pages: Vec<Page>, engine: RenderingEngine) -> Self {
        PageSequence { pages, engine }
    }

    pub fn first(&self) -> &Page {
        &self.pages[0]
    }

    pub fn len(&self) -> usize {
        self.pages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pages.is_empty()
    }

    pub fn materialize(
        &mut self,
        engine: &RenderingEngine,
        options: &ExportOptions,
    ) {
        if !options.is_raster() {
            return;
        }
        for page in self.pages.iter_mut() {
            PageCache::materialize(page.id, engine, options);
        }
    }

    /// The bytes of one file holding every page.
    ///
    /// Reached when the format spans pages, which used to mean PDF and now
    /// means an animation too -- hence a dispatcher rather than the direct
    /// call to [`PageSequence::as_pdf`] that was here.
    pub fn encoded_spanning(
        &self,
        options: ExportOptions,
    ) -> Result<Vec<u8>, String> {
        match options.format {
            ImageFormat::Pdf => self.as_pdf(options),
            _ => self.as_animation(options),
        }
    }

    /// Every page as one frame of an animation, written into `out`.
    ///
    /// Frames are rasterized a batch at a time and written as each batch
    /// lands, so what is held is one batch rather than the whole animation.
    /// A thousand frames of 1080p is 8 GB of pixels if they are all
    /// gathered first, which is what this used to do.
    ///
    /// The batch is one frame per worker rather than one frame at a time,
    /// because rasterizing and quantizing is the expensive part and there is
    /// no reason to do it on one thread. Writing stays sequential: both
    /// formats are a single ordered stream, and a frame cannot be written
    /// before the one in front of it.
    pub fn write_animation(
        &self,
        options: &ExportOptions,
        out: &mut dyn Sink,
    ) -> Result<(), String> {
        options.check_timing()?;
        let count = self.pages.len();
        let Some(first) = self.pages.first() else {
            return Err(format!(
                "Cannot encode {} with no pages to draw",
                options.format.as_str()
            ));
        };
        let dims = first.scaled_dimensions(options.density);
        let spec = SequenceSpec {
            width: dims.width.max(0) as u32,
            height: dims.height.max(0) as u32,
            frames: count,
            loops: options.loops,
            quality: (options.quality * 100.0).clamp(0.0, 100.0),
            density: options.density,
            color: options.encoded_color_profile(),
        };

        let mut sink = encode::start(options.format, &spec, out)?;
        let batch = rayon::current_num_threads().max(1);
        for (nth, pages) in self.pages.chunks(batch).enumerate() {
            let first_of_batch = nth * batch;
            let frames = pages
                .par_iter()
                .enumerate()
                .map(|(offset, page)| {
                    let delay =
                        options.delay_ms(first_of_batch + offset, count);
                    page.as_frame(options, self.engine, delay)
                })
                .collect::<Result<Vec<_>, _>>()?;
            for frame in &frames {
                sink.write_frame(frame)?;
            }
        }
        sink.finish()
    }

    /// Every page as one frame of an animation, gathered into memory.
    ///
    /// For the callers that have to hand bytes back. `to_file` writes into
    /// the file instead and never holds the whole thing.
    pub fn as_animation(
        &self,
        options: ExportOptions,
    ) -> Result<Vec<u8>, String> {
        let mut bytes = Cursor::new(Vec::new());
        self.write_animation(&options, &mut bytes)?;
        Ok(bytes.into_inner())
    }

    pub fn as_pdf(&self, options: ExportOptions) -> Result<Vec<u8>, String> {
        let mut bytes = Vec::new();
        self.write_pdf(&mut bytes, options)?;
        Ok(bytes)
    }

    /// Every page as one PDF, written into `out`.
    fn write_pdf(
        &self,
        out: &mut impl std::io::Write,
        options: ExportOptions,
    ) -> Result<(), String> {
        options.check_timing()?;
        let ExportOptions {
            quality,
            density,
            matte,
            ..
        } = options;
        let metadata = pdf::Metadata {
            producer: PDF_PRODUCER.to_string(),
            encoding_quality: Some((quality * 100.0) as i32),
            raster_dpi: Some(density * 72.0),
            ..Default::default()
        };
        self.pages
            .iter()
            .try_fold(pdf_document(out, &metadata), |doc, page| {
                page.append_to(doc, matte)
            })
            .map(|doc| doc.close())
    }

    pub fn write_image(
        &self,
        pattern: &str,
        options: ExportOptions,
    ) -> Result<(), String> {
        self.first().write(pattern, options, self.engine)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn write_sequence(
        &self,
        pattern: &str,
        padding: f32,
        options: ExportOptions,
    ) -> Result<(), String> {
        let padding = match padding as i32 {
            -1 => (1.0 + (self.pages.len() as f32).log10().floor()) as usize,
            pad => pad as usize,
        };

        self.pages
            .par_iter()
            .enumerate()
            .try_for_each(|(pp, page)| {
                let folio = format!("{:0width$}", pp + 1, width = padding);
                let filename = pattern.replace("{}", folio.as_str());
                page.write(&filename, options.clone(), self.engine)
            })
    }

    /// Writes every page to `path` as one file.
    ///
    /// Straight into the file rather than through a `Vec` first, so a long
    /// animation is bounded by disk rather than by memory. PDF takes the
    /// same route: Skia's document backend has always accepted a writer,
    /// and it was being handed a growing buffer for no reason.
    pub fn write_spanning(
        &self,
        path: impl AsRef<FilePath>,
        options: ExportOptions,
    ) -> Result<(), String> {
        let path = path.as_ref();
        let named = |why| format!("{}: \"{}\"", why, path.display());
        let file = fs::File::create(path).map_err(named)?;
        let mut out = BufWriter::new(file);

        match options.format {
            ImageFormat::Pdf => self.write_pdf(&mut out, options)?,
            _ => self.write_animation(&options, &mut out)?,
        }
        out.flush().map_err(named)
    }
}

//
// Cache for the last bitmap generated by a given Page
//

#[derive(Debug, Clone)]
struct PageCache {
    image: Option<SkImage>,
    density: f32,
    matte: Option<Color>,
    msaa: Option<usize>,
    depth: usize,
}

impl Default for PageCache {
    fn default() -> Self {
        Self {
            image: None,
            depth: 0,
            density: 1.0,
            matte: None,
            msaa: None,
        }
    }
}

impl PageCache {
    pub fn shared<'a>() -> &'a Arc<DashMap<usize, PageCache>> {
        CACHE.get_or_init(|| Arc::new(DashMap::new()))
    }

    pub fn add(id: usize) {
        Self::shared().insert(id, PageCache::default());
    }

    pub fn drop(id: usize) {
        Self::shared().remove(&id);
    }

    pub fn get(
        id: usize,
        opts: &ExportOptions,
        depth: usize,
    ) -> (Option<SkImage>, usize) {
        Self::shared()
            .get(&id)
            .map(|cache| match cache.is_valid(opts) && depth >= cache.depth {
                true => (cache.image.clone(), cache.depth),
                false => (None, 0),
            })
            .unwrap_or((None, 0))
    }

    pub fn set(id: usize, image: SkImage, opts: &ExportOptions, depth: usize) {
        if let Some(mut cache) = Self::shared().get_mut(&id) {
            // save the bitmap if it's newer than the cached version, or is
            // replacing an invaildated cache
            if !cache.is_valid(opts) || depth > cache.depth {
                *cache = Self {
                    image: Some(image),
                    density: opts.density,
                    matte: opts.matte,
                    msaa: opts.msaa,
                    depth,
                }
            }
        }
    }

    pub fn materialize(
        id: usize,
        engine: &RenderingEngine,
        options: &ExportOptions,
    ) {
        if let Some(mut cache) = Self::shared().get_mut(&id) {
            // nothing to be done if the image isn't currently in GPU memory
            // or if the options have changed (so the cache is invalid anyway)
            if let Some(ref img) = cache.image
                && (!cache.is_valid(options) || !img.is_texture_backed())
            {
                return;
            }

            // otherwise move the image to main memory
            engine.with_direct_context(|context| {
                cache.image = cache
                    .image
                    .as_ref()
                    .and_then(|img| img.make_non_texture_image(context))
            });
        }
    }

    #[cfg(not(any(feature = "metal", feature = "vulkan")))]
    fn _blit(
        &self,
        _surface: &mut Surface,
        dst_info: &ImageInfo,
        src: IRect,
        pixels: &mut [u8],
    ) -> Option<bool> {
        self.image.as_ref().map(|image| {
            image.read_pixels(
                dst_info,
                pixels,
                dst_info.min_row_bytes(),
                (src.x(), src.y()),
                CachingHint::Allow,
            )
        })
    }

    #[cfg(any(feature = "metal", feature = "vulkan"))]
    fn _blit(
        &self,
        surface: &mut Surface,
        dst_info: &ImageInfo,
        src: IRect,
        pixels: &mut [u8],
    ) -> Option<bool> {
        let context = &mut surface.direct_context();
        self.image.as_ref().map(|image| {
            image.read_pixels_with_context(
                context,
                dst_info,
                pixels,
                dst_info.min_row_bytes(),
                (src.x(), src.y()),
                CachingHint::Allow,
            )
        })
    }

    pub fn is_valid(&self, opts: &ExportOptions) -> bool {
        self.density == opts.density
            && self.matte == opts.matte
            && self.msaa == opts.msaa
            && self.image.is_some()
            && opts.is_raster()
    }
}

//
// Helpers
//

pub fn pages_arg(
    cx: &mut FunctionContext,
    idx: usize,
    opts: &ExportOptions,
    canvas: &BoxedCanvas,
) -> NeonResult<PageSequence> {
    let engine = canvas.borrow_mut().engine();
    let pages = cx
        .argument::<JsArray>(idx)?
        .to_vec(cx)?
        .iter()
        .map(|obj| obj.downcast::<BoxedContext2D, _>(cx))
        .filter(|ctx| ctx.is_ok())
        // SAFETY: `.filter(|ctx| ctx.is_ok())` ensures only `Ok` values reach
        // here.
        .map(|obj| obj.unwrap().borrow().get_page_for_export(opts, &engine))
        .collect();
    Ok(PageSequence::from(pages, engine))
}

fn pdf_document<'a>(
    buffer: &'a mut impl std::io::Write,
    metadata: &'a pdf::Metadata<'a>,
) -> Document<'a> {
    pdf::new_document(buffer, Some(metadata))
}

#[derive(Clone, Debug, PartialEq)]
pub struct ExportOptions {
    pub format: ImageFormat,
    pub quality: f32,
    pub density: f32,
    pub outline: bool,
    pub matte: Option<Color>,
    pub msaa: Option<usize>,
    pub color_type: ColorType,
    /// The space an export or readback is *converted into*.
    ///
    /// Distinct from [`ExportOptions::surface_color_space`], which is the one
    /// drawing happens in. Asking for a wider space here does not widen the
    /// content: it re-expresses what the surface holds.
    pub color_space: ColorSpace,
    /// The space the compositing surface is built in -- the canvas's own,
    /// fixed when it was constructed.
    ///
    /// A colour named in this space survives whole; one outside its gamut is
    /// clipped as it is drawn, which is what a browser's canvas does.
    pub surface_color_space: ColorSpace,
    /// The format the compositing surface is built in -- the canvas's own,
    /// fixed when it was constructed.
    ///
    /// Separate from [`color_type`](Self::color_type) for the same reason
    /// this is separate from [`color_space`](Self::color_space): those are
    /// what a readback or an export converts *into*, and letting them choose
    /// the surface would make the compositing precision a property of the
    /// call -- asking for an F32 readback of an eight-bit canvas would
    /// silently composite the page in float.
    pub surface_color_type: ColorType,
    pub jpeg_downsample: bool,
    pub text_contrast: f32,
    pub text_gamma: f32,
    /// Frames per second for an animated format. `None` means unasked.
    pub fps: Option<f32>,
    /// Per-frame durations in milliseconds, used when there is one per page.
    pub frame_delays: Vec<u32>,
    /// How many times an animation plays; `None` plays it forever.
    pub loops: Option<u32>,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            format: ImageFormat::Raw,
            quality: 0.92,
            density: 1.0,
            matte: None,
            jpeg_downsample: false,
            text_contrast: 0.0,
            text_gamma: 1.4,
            msaa: None,
            color_type: ColorType::RGBA8888,
            color_space: ColorSpace::new_srgb(),
            surface_color_space: ColorSpace::new_srgb(),
            surface_color_type: ColorType::N32,
            outline: true,
            fps: None,
            frame_delays: Vec::new(),
            loops: None,
        }
    }
}

impl ExportOptions {
    pub fn surface_props(&self) -> SurfaceProps {
        SurfaceProps::new_with_text_properties(
            SurfacePropsFlags::default(),
            PixelGeometry::Unknown,
            self.text_contrast,
            self.text_gamma,
        )
    }

    pub fn svg_flags(&self) -> Option<skia_safe::svg::canvas::Flags> {
        match self.outline {
            true => Some(Flags::CONVERT_TEXT_TO_PATHS),
            _ => None,
        }
    }

    pub fn msaa_from(&self, valid_msaa: &Vec<usize>) -> Result<usize, String> {
        // 4x is a good default if available. Where it is not, the nearest
        // multisampled count to it is: falling back to the highest the device
        // reports would ask a driver advertising 32x for eight times the
        // samples -- and eight times the render target -- to draw the same
        // page, which is not what a missing 4x asks for. Counts below two are
        // left out of the search because they are not a coarser 4x, they are
        // no multisampling at all: a different way of drawing an edge, and
        // one a caller has to ask for by name.
        let samples = self.msaa.unwrap_or_else(|| {
            valid_msaa
                .iter()
                .copied()
                .filter(|count| *count > 1)
                .min_by_key(|count| count.abs_diff(4))
                .unwrap_or(0)
        });
        match valid_msaa.contains(&samples) {
            true => Ok(samples),
            false => Err(format!(
                "{}x MSAA not supported by GPU (options: {:?})",
                samples, valid_msaa
            )),
        }
    }

    /// The pixel format a compositing surface is built in.
    ///
    /// N32 unless the caller asked for a float format, which is the one case
    /// where following `color_type` gives more than it costs. The rest of the
    /// formats `color_type` can name are readback formats and nothing else:
    /// rasterising into an opaque one (`RGB565`, `Gray8`, `RGB888x`) turns the
    /// transparent clear black and resolves every blend against it, and a
    /// narrower one quantises each intermediate draw rather than only the
    /// output. Those conversions belong in `read_pixels`, where they happen.
    ///
    /// `F16` and `F32` are the opposite case: strictly wider than N32, so
    /// compositing in them is what asking for them was for. Without this an
    /// `F32` canvas composited at eight bits and converted to float on the way
    /// out -- a fill at alpha 0.002 read back as 1/255, and one at 0.0005 read
    /// back as nothing.
    pub fn compositing_color_type(&self) -> ColorType {
        match self.surface_color_type {
            ColorType::RGBAF16 | ColorType::RGBAF16Norm => ColorType::RGBAF16,
            ColorType::RGBAF32 => ColorType::RGBAF32,
            _ => ColorType::N32,
        }
    }

    /// [`ImageInfo`] for a compositing surface of `dims` in `space`.
    pub fn compositing_info(
        &self,
        dims: impl Into<ISize>,
        space: &ColorSpace,
    ) -> ImageInfo {
        ImageInfo::new(
            dims,
            self.compositing_color_type(),
            AlphaType::Premul,
            Some(space.clone()),
        )
    }

    /// Whether this export rasterizes, and so has pixels worth caching.
    ///
    /// Not the opposite of [`spans_pages`](Self::spans_pages): see
    /// `export::FormatTraits` for why the two were ever the same question.
    pub fn is_raster(&self) -> bool {
        self.format.traits().content == Content::Raster
    }

    /// Whether one file carries every page rather than a chosen one.
    pub fn spans_pages(&self) -> bool {
        self.format.spans_pages()
    }

    /// The space a foreign encoder's frames are rasterized into.
    ///
    /// The requested one where the container can declare which space it
    /// holds, and sRGB where it cannot. Only GIF cannot -- see
    /// [`ColorSignal`](crate::export::ColorSignal) -- and only GIF is
    /// narrowed.
    ///
    /// A space this crate has no name for takes the same route as GIF. It
    /// can be reached from Rust by handing a canvas an ICC profile Skia
    /// parsed, and there is no code point or chromaticity pair to write it
    /// down with, so converting to sRGB is the one answer that leaves the
    /// file honest.
    ///
    /// # Errors
    ///
    /// Returns a message when the space cannot be realized by this build,
    /// which is what [`PixelColorSpace::to_skia_color_space`] reports.
    pub(crate) fn encoded_color_space(&self) -> Result<ColorSpace, String> {
        match self.encoded_pixel_color_space() {
            Some(space) => {
                space.to_skia_color_space().map_err(|why| why.to_string())
            }
            None => Ok(ColorSpace::new_srgb()),
        }
    }

    /// As [`encoded_color_space`](Self::encoded_color_space), as one of this
    /// crate's named spaces, or `None` where the answer is plain sRGB.
    fn encoded_pixel_color_space(&self) -> Option<PixelColorSpace> {
        match self.format.declares_color() {
            true => PixelColorSpace::matching(&self.color_space),
            false => None,
        }
    }

    /// What an encoder is told about the colour of the frames it is handed.
    ///
    /// Always the truth about the pixels: it reports whatever
    /// [`encoded_color_space`](Self::encoded_color_space) actually rendered
    /// into, so a narrowed export says sRGB rather than repeating what was
    /// asked for.
    pub(crate) fn encoded_color_profile(&self) -> ColorProfile {
        ColorProfile::of(
            self.encoded_pixel_color_space()
                .unwrap_or(PixelColorSpace::Srgb),
        )
    }

    /// Refuses timing given to a format that has nowhere to put it.
    ///
    /// PNG, TIFF, ICO and the rest have no clock. Ignoring an `fps` or a
    /// list of frame delays would be the same silent retiming this crate
    /// refuses everywhere else -- a caller who asked for twelve frames a
    /// second and got one still image is owed the reason.
    pub fn check_timing(&self) -> Result<(), String> {
        if self.format.is_animated() {
            return Ok(());
        }
        let named = match (
            self.fps.is_some(),
            !self.frame_delays.is_empty(),
            self.loops.is_some(),
        ) {
            (true, _, _) => "fps",
            (_, true, _) => "frame delays",
            (_, _, true) => "a loop count",
            _ => return Ok(()),
        };
        Err(format!(
            "{} is not an animated format, so {named} would do nothing (it \
             encodes {})",
            self.format.as_str(),
            match self.format.spans_pages() {
                true => "every page, untimed",
                false => "one page",
            }
        ))
    }

    /// How long frame `index` of `count` is shown, in milliseconds.
    ///
    /// An explicit list wins, but only when it has one entry per frame:
    /// a shorter one would silently retime the frames it does not reach,
    /// and a longer one describes an animation the caller did not draw.
    ///
    /// Otherwise the delay is the difference between where this frame ends
    /// and where the one before it did, both rounded from the exact time
    /// the rate implies. Rounding each frame on its own instead looks
    /// simpler and loses the remainder every time: at 30fps every frame
    /// became 33ms rather than 33.33, so an animation ran one percent fast
    /// and a ten second one ended a tenth of a second early. Taking the
    /// difference of two rounded totals spends the remainder instead of
    /// dropping it -- the frames come out 33, 34, 33 -- and the total is
    /// the exact duration rounded once.
    pub fn delay_ms(&self, index: usize, count: usize) -> u32 {
        if self.frame_delays.len() == count
            && let Some(delay) = self.frame_delays.get(index)
        {
            return *delay;
        }
        // A rate of zero, a negative one, or a NaN describes no animation at
        // all, so the default stands rather than dividing by it.
        let asked = self.fps.unwrap_or(30.0);
        let fps = match asked.is_finite() && asked > 0.0 {
            true => f64::from(asked).min(1000.0),
            false => 30.0,
        };
        let at = |frame: usize| (frame as f64 * 1000.0 / fps).round();
        (at(index + 1) - at(index)) as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A JPEG with `segments` before the compressed data, each a marker and
    /// a payload.
    fn jpeg_with(segments: &[(u8, &[u8])]) -> Vec<u8> {
        let mut bytes = vec![0xFF, 0xD8]; // start of image
        for (marker, payload) in segments {
            bytes.extend_from_slice(&[0xFF, *marker]);
            bytes
                .extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
            bytes.extend_from_slice(payload);
        }
        bytes.extend_from_slice(&[0xFF, 0xDA, 0, 2]); // start of scan
        bytes
    }

    /// The payload of a JFIF segment, without its length.
    const JFIF: &[u8] = b"JFIF\0\x01\x02\x00\x00\x01\x00\x01\x00\x00";

    #[test]
    fn the_jfif_segment_is_found_wherever_it_sits() {
        // The density fields were spliced at a fixed 13, which is right only
        // while JFIF is the first segment. Skia puts it first today and
        // promises nothing, and a JPEG carries no checksum -- so a file that
        // led with EXIF or ICC would have had five bytes of that segment
        // quietly overwritten with a resolution.
        let leading = jpeg_with(&[(0xE0, JFIF)]);
        assert_eq!(jfif_segment(&leading), Some(2), "the usual layout");

        // The same file with an EXIF segment in front of it. The old
        // arithmetic would have written into the EXIF payload.
        let exif = [b"Exif\0\0".as_slice(), &[0u8; 20]].concat();
        let behind = jpeg_with(&[(0xE1, &exif), (0xE0, JFIF)]);
        let at = jfif_segment(&behind).expect("still found");
        assert!(at > 13, "the segment starts at {at}, past the old offset");
        assert_eq!(&behind[at + 4..at + 9], b"JFIF\0");
    }

    #[test]
    fn a_jpeg_with_no_jfif_segment_is_left_alone() {
        // Rather than having its first five bytes past the marker rewritten
        // with a density it has nowhere to put.
        let exif = [b"Exif\0\0".as_slice(), &[0u8; 20]].concat();
        assert_eq!(jfif_segment(&jpeg_with(&[(0xE1, &exif)])), None);
        // And nothing walks off the end of a truncated file.
        assert_eq!(jfif_segment(&[0xFF, 0xD8]), None);
        assert_eq!(jfif_segment(&[]), None);
        assert_eq!(jfif_segment(&[0xFF, 0xD8, 0xFF, 0xE0, 0xFF]), None);
    }

    /// A JFIF segment too short to hold a density is not one to splice into.
    ///
    /// `jfif_segment` used to accept anything whose payload began `JFIF\0`,
    /// while the caller wrote five bytes at offset 11 of it. A segment that
    /// stopped before then -- legal, and all a truncated or hand-built file
    /// needs -- sent `Vec::splice` past the end of the buffer, which panics.
    ///
    /// Walking for the segment exists so the encoder's *position* is not
    /// assumed. Assuming its length instead gave the same class of bug back.
    #[test]
    fn a_jfif_segment_with_no_room_for_a_density_is_refused() {
        // Signature, version, and nothing else: a valid APP0 that has
        // nowhere to put a resolution.
        let stunted = jpeg_with(&[(0xE0, b"JFIF\0\x01\x02")]);
        assert_eq!(
            jfif_segment(&stunted),
            None,
            "a segment ending before the density fields is not spliceable"
        );

        // One byte short of the fields is still short.
        let almost =
            jpeg_with(&[(0xE0, b"JFIF\0\x01\x02\x00\x00\x01\x00\x01")]);
        assert_eq!(jfif_segment(&almost), None);

        // And the full one is still found, so the guard did not refuse
        // everything.
        assert_eq!(jfif_segment(&jpeg_with(&[(0xE0, JFIF)])), Some(2));
    }

    #[test]
    fn the_webp_exif_flag_is_only_set_where_there_is_a_header_to_set_it_in() {
        // `bytes[20] |= 1 << 3` assumed a `VP8X` chunk. A plain lossy WebP
        // has none and begins its image data there, so the flag would have
        // flipped a bit inside the picture.
        let extended =
            [b"RIFF".as_slice(), &[0; 4], b"WEBP", b"VP8X", &[0; 10]].concat();
        assert!(webp_has_vp8x(&extended));

        let plain =
            [b"RIFF".as_slice(), &[0; 4], b"WEBP", b"VP8 ", &[0; 10]].concat();
        assert!(!webp_has_vp8x(&plain), "no header, nowhere for the flag");

        assert!(!webp_has_vp8x(&[]), "and nothing indexes off the end");
        assert!(!webp_has_vp8x(b"RIFF"));
        assert!(!webp_has_vp8x(&extended[..RIFF_FIRST_CHUNK + 4]));
    }

    #[test]
    fn the_png_insertion_point_is_the_end_of_the_header_chunk() {
        // Derived from the format's fixed parts rather than written as 33,
        // which is what it was. `pHYs` has to precede `IDAT`, and straight
        // after `IHDR` is the one place that is true of every PNG.
        assert_eq!(PNG_AFTER_IHDR, 33);
    }

    /// The sample count `msaa_from` settles on for a device offering
    /// `valid`, with nothing asked for.
    fn chosen(valid: &[usize]) -> usize {
        ExportOptions::default()
            .msaa_from(&valid.to_vec())
            .expect("a default never asks for a count the device lacks")
    }

    #[test]
    fn an_unset_sample_count_lands_on_four_or_its_nearest_neighbour() {
        // Both backends offer 4x, and it is the default either way.
        assert_eq!(chosen(&[0, 1, 2, 4, 8, 16, 32]), 4);

        // Without it, the nearest count wins rather than the largest: a
        // device advertising 32x is not asking to render every page at
        // eight times the samples.
        assert_eq!(chosen(&[0, 1, 2, 8, 16, 32]), 2);
        assert_eq!(chosen(&[0, 1, 8, 16, 32]), 8);

        // Nearest by count alone would have taken 1 over 8 there. It is the
        // nearer number and the worse answer: one sample a pixel is not a
        // coarser multisampling, it is none.
        assert_eq!(chosen(&[0, 1]), 0);
    }

    #[test]
    fn a_sample_count_the_device_lacks_is_refused_by_name() {
        let asked = ExportOptions {
            msaa: Some(8),
            ..Default::default()
        };
        let refused = asked
            .msaa_from(&vec![0, 1, 2, 4])
            .expect_err("8x is not on offer");
        assert!(refused.contains('8'), "the count is named: {refused}");
    }

    #[test]
    fn one_sample_a_pixel_is_a_count_a_caller_may_ask_for() {
        // `0` and `1` both mean no multisampling. The Metal backend used to
        // list only `0`, so `msaa: 1` -- the plainer way to say it, and the
        // one the Vulkan backend already took -- was refused on macOS.
        let asked = ExportOptions {
            msaa: Some(1),
            ..Default::default()
        };
        assert_eq!(asked.msaa_from(&vec![0, 1, 2, 4]), Ok(1));
    }
}
