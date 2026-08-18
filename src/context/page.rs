use crc::{CRC_32_ISO_HDLC, Crc};
use dashmap::DashMap;
use little_exif::{
    exif_tag::ExifTag, filetype::FileExtension, metadata::Metadata,
};
use neon::prelude::*;
use parking_lot::RwLock;
use rayon::prelude::*;
use skia_safe::{
    AlphaType, Canvas as SkCanvas, ClipOp, Color, ColorSpace, ColorType,
    Document, IRect, ISize, Image as SkImage, ImageInfo, M44, Matrix, Paint,
    Path, Picture, PictureRecorder, PixelGeometry, Rect, Size, Surface,
    SurfaceProps, SurfacePropsFlags,
    canvas::SrcRectConstraint,
    image::{BitDepth, CachingHint},
    images, jpeg_encoder, pdf, png_encoder, surfaces,
    svg::{self, canvas::Flags},
    webp_encoder,
};
use std::{
    collections::HashMap,
    fs,
    io::{BufWriter, Cursor, Write},
    path::Path as FilePath,
    sync::{
        Arc, OnceLock, Weak,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
};
const CRC32: Crc<u32> = Crc::<u32>::new(&CRC_32_ISO_HDLC);

use crate::{
    context::BoxedContext2D,
    encode::{
        self, Frame, FrameDepth, Pixels, SequenceSpec, Sink,
        color::ColorProfile,
        rowfilter::{
            DEFLATE_LEVEL, PROBE_BANDS, accumulate, band_rows, band_top, pays,
        },
    },
    export::{
        ChromaSampling, Content, EncoderKind, ImageFormat, NOMINAL_DPI,
        QUALITY_SCALE, VectorFeatures, dots_per_inch, encoder_quality,
        pixels_per_metre,
    },
    gpu::{RenderingEngine, owner},
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

/// Drops every cached page bitmap, keeping the identities that hold none.
///
/// Called by the idle watcher in [`crate::memory`] once rendering has stopped
/// for a few seconds, which is the only moment at which a bitmap is known not
/// to be about to pay for itself. The entry it saves is worth 1.9
/// milliseconds on a repeat 400x300 raw export and 1.2 on a 1200x900 PNG, so
/// this is not free -- what makes it worth doing is that most of what is held
/// when a process goes quiet belongs to canvases JavaScript has already
/// dropped, and V8 will not finalize those for as long as it does not feel
/// the weight.
///
/// The identities are left in place. One costs a few words, `set` refuses to
/// file under a generation that no longer exists, and removing a live page's
/// key would only have it put straight back on the next export.
pub(crate) fn release_cached_pages() {
    PageCache::shared().iter_mut().for_each(|mut entry| {
        entry.image = None;
        entry.bytes = 0;
        entry.depth = 0;
    });
}

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

/// Milliseconds in a second, for turning a frame rate into a duration.
///
/// Also the highest rate [`ExportOptions::delay_ms`] will divide by: every
/// animated format here stores whole milliseconds, so a thousand frames a
/// second is one frame per millisecond and the last rate that can be
/// written down. Past it every frame rounds to the same instant.
const MS_PER_SECOND: f64 = 1000.0;

/// The frame rate an animation plays at when the caller names none.
///
/// Thirty, which is what the JavaScript binding has always documented. It
/// appears twice in the same function -- once as the default and once as
/// the fallback for a rate that describes no animation at all -- and those
/// are the same answer to the same question.
const DEFAULT_FPS: f32 = 30.0;

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
    // Every raster path -- export, readback, animation frame -- comes through
    // here, which is what lets the allocator give its pages back once they
    // stop. See `crate::memory`.
    crate::memory::note_render();

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

/// One generation of a page, and the proof that it is still current.
///
/// The recorder holds the only strong reference; every [`Page`] handed to an
/// export holds a weak one. Dropping the strong reference is what retires the
/// generation -- a full-canvas clear replaces it, and collecting the canvas
/// releases it -- and that is what removes the generation's cache entry.
///
/// A weak reference on the export side is what lets a store tell whether the
/// page it is filing a bitmap for still exists. [`PageCache::set`] creates
/// the entry it cannot find, deliberately, so that a page evicted while it is
/// still being drawn can cache again rather than replaying in full for the
/// rest of its life. Without a liveness test that same line resurrected every
/// entry retiring a generation had just removed: measured on thirty-two
/// concurrent exports of one 1200x900 canvas, five times over, 155 of 160
/// stores landed on a generation that no longer existed, and what they left
/// held 57.7 MB no lookup could ever reach.
#[derive(Debug)]
pub(crate) struct PageId(usize);

impl Drop for PageId {
    fn drop(&mut self) {
        PageCache::drop(self.0);
    }
}

pub struct PageRecorder {
    current: PictureRecorder,
    layers: Vec<Picture>,
    /// Index-aligned with `layers`: what each layer's draws asked of a
    /// vector backend. Empty for the ordinary ones. See
    /// [`PageRecorder::append_isolated`].
    features: Vec<VectorFeatures>,
    /// What appeared inside an open `saveLayer`, where a draw cannot be
    /// split into a layer of its own. A backend that refuses any of it
    /// rasterizes the whole page.
    page_features: VectorFeatures,
    bounds: Rect,
    matrix: Matrix,
    clip: Option<Path>,
    surface: RecordingSurface,
    changed: bool,
    /// Set while `current` is recording an isolated layer rather than an
    /// ordinary one, to the features that layer was opened for. Consecutive
    /// isolated draws asking for the same features share it; anything else
    /// closes it. See [`PageRecorder::append_isolated`].
    isolated: Option<VectorFeatures>,
    /// Set when `matrix` or `clip` has moved but the recording canvas has
    /// not been rebuilt to match. See [`PageRecorder::settle`].
    state_dirty: bool,
    id: Arc<PageId>,
    /// Save-stack depth that `restore()` rebuilds from. Normally 1, the
    /// recording canvas's own base. Each open `saveLayer` raises it so the
    /// layer frame survives a matrix or clip change; `layer_floors` holds the
    /// values to fall back to as those layers close.
    base_depth: usize,
    layer_floors: Vec<usize>,
}

impl PageRecorder {
    /// Mints a page identity and registers it with the cache.
    ///
    /// A generation, not a handle: content that has been erased gets a new
    /// one. Exports run concurrently and each holds the layers it was given,
    /// so two generations of one page must not answer to the same key --
    /// otherwise an export in flight can be served the bitmap cached for the
    /// content that replaced it. See [`Self::set_bounds`].
    fn mint_id() -> Arc<PageId> {
        static COUNTER: AtomicUsize = AtomicUsize::new(1);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        PageCache::add(id);
        Arc::new(PageId(id))
    }

    pub fn new(bounds: Rect) -> Self {
        let id = Self::mint_id();

        let mut rec = PictureRecorder::new();
        rec.begin_recording(bounds, true).save(); // start at depth 2

        PageRecorder {
            current: rec,
            layers: vec![],
            features: vec![],
            page_features: VectorFeatures::PLAIN,
            changed: false,
            isolated: None,
            state_dirty: false,
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
        // An ordinary draw cannot join an isolated layer: that layer is
        // marked for rasterization, and everything in it is rasterized with
        // it. Close it first so this draw stays a vector.
        if self.isolated.is_some() {
            self.flush();
        }
        self.settle();
        if let Some(canvas) = self.current.recording_canvas() {
            f(canvas);
            self.changed = true;
        }
    }

    /// Erases the page's content, in place.
    ///
    /// This used to be `*self = PageRecorder::new(bounds)`, which allocated a
    /// `PictureRecorder` and claimed a new cache identity on every call. All
    /// three callers use it to clear at the size the page already has --
    /// `reset()`, an opaque fill that covers the canvas, and a `clearRect`
    /// that does -- and a resize goes through [`Self::update_bounds`]
    /// instead, so the rebuild bought nothing. It was most of what a
    /// full-canvas `clearRect` cost.
    ///
    /// The identity still has to change, and reusing it was a bug: an
    /// opaque fill covering the canvas comes through here too, so a loop
    /// that fills and exports thirty-two times runs thirty-two generations
    /// through one recorder. With one key between them an export in flight
    /// was served the bitmap cached for the frame that replaced it -- the
    /// eighth of thirty-two exports came back holding the seventh's pixels.
    /// What is saved here is the `PictureRecorder` allocation, not the
    /// identity.
    pub fn set_bounds(&mut self, bounds: Rect) {
        // Only a generation that recorded something needs replacing. Nothing
        // has been drawn when `layers` is empty and nothing is pending, so no
        // export can have captured this page and no cache entry can hold
        // content for it -- and clearing a recorder that was just built is
        // the common case, because `getContext` and `newPage` both do it.
        if self.changed || !self.layers.is_empty() {
            // The identity this replaces is the last strong reference to the
            // retired generation, so assigning here is what drops its cache
            // entry and what tells an export still in flight for it that it
            // has nothing left to file.
            self.id = Self::mint_id();
        }

        // Finish before beginning again: `flush` reuses the recorder the same
        // way, and Skia expects the open recording closed first.
        let _ = self.current.finish_recording_as_picture(None);

        self.bounds = bounds;
        self.current.begin_recording(bounds, true).save(); // start at depth 2
        self.layers.clear();
        self.features.clear();
        self.page_features = VectorFeatures::PLAIN;
        self.changed = false;
        self.isolated = None;
        self.state_dirty = false;
        self.matrix = Matrix::default();
        self.clip = None;
        self.surface = RecordingSurface::default();
        self.base_depth = 1;
        self.layer_floors.clear();
    }

    pub fn update_bounds(&mut self, bounds: Rect) {
        self.bounds = bounds; // non-destructively update the size
    }

    pub fn set_matrix(&mut self, matrix: Matrix) {
        self.matrix = matrix;
        self.restore();
    }

    pub fn set_clip(&mut self, clip: &Option<Path>) {
        self.clip.clone_from(clip);
        self.restore();
    }

    /// Rebuilds the recording canvas's frame if the state has moved since it
    /// was last built.
    ///
    /// Every draw goes through here, and nothing else has to: a transform or
    /// a clip that is never drawn under is never observed, so the canvas does
    /// not have to be torn down and rebuilt to record it. A run of
    /// `translate` calls used to do exactly that -- `restore_to_count`, a
    /// `save`, the clip path re-applied and the matrix set, once per call --
    /// and the intermediate frames could not be seen by anything.
    fn settle(&mut self) {
        if !self.state_dirty {
            return;
        }

        // An open isolated layer was framed with the state as it was when it
        // opened, so a change to that state has to close it. `flush` clears
        // `isolated` and marks the state dirty again, which is why the flag
        // is cleared after it rather than before.
        if self.isolated.is_some() {
            self.flush();
        }

        self.state_dirty = false;
        self.rebuild_frame();
    }

    /// Sets the transform and the clip together, rebuilding once.
    ///
    /// Four call sites restore both at once -- `restore()`, `reset()` and the
    /// two layer paths -- and setting them one at a time tore the recording
    /// canvas down and rebuilt it twice for one logical change. A `ctx.save()`
    /// and `ctx.restore()` pair measured 802 ns before and 735 after.
    pub fn set_state(&mut self, matrix: Matrix, clip: &Option<Path>) {
        self.matrix = matrix;
        self.clip.clone_from(clip);
        self.restore();
    }

    /// Notes that the recording canvas no longer matches `matrix` and
    /// `clip`. The rebuild happens in [`Self::settle`], before the next draw.
    pub fn restore(&mut self) {
        self.state_dirty = true;
    }

    fn rebuild_frame(&mut self) {
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
        let page_size = page.scaled_dimensions(opts.density);

        // Small reads are served by the squares they touch, which is the
        // whole point: a hit test asked for sixty-four pixels and used to
        // composite and keep a megapixel. Anything wider than the grid is
        // worth is served by the page, as before.
        if self.surface.try_tiled(&page, &opts, &engine, crop) {
            let served = self.surface.read_tiled(
                &dst_info,
                crop,
                &mut dst_buffer,
                page_size,
            );
            if served {
                return Ok(dst_buffer);
            }
        }

        self.surface.update(&page, &opts, &engine);

        match self.surface.copy_pixels(
            &dst_info,
            crop,
            &mut dst_buffer,
            &engine,
        ) {
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
            self.features
                .push(self.isolated.unwrap_or(VectorFeatures::PLAIN));
        }
        self.isolated = None;

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
    pub fn append_isolated<F>(&mut self, features: VectorFeatures, f: F)
    where
        F: FnOnce(&SkCanvas),
    {
        if !self.layer_floors.is_empty() {
            self.page_features = self.page_features.with(features);
            self.append(f);
            return;
        }

        // A run of these used to become a layer each: one `PictureRecorder`
        // built, filled and finished per call, pushed onto `layers` and never
        // coalesced. A `clearRect` that does not cover the canvas comes
        // through here, so a drawing that clears a region every frame grew
        // its page without bound -- a hundred thousand of them added 115 MB
        // of resident memory.
        //
        // Consecutive draws wanting the same features can share one layer
        // instead. The layer stays open in `current`, and it is closed by
        // whatever would make sharing wrong: an ordinary draw, which must
        // not be rasterized with it; a clip or matrix change, which arrives
        // through `restore` and would not apply to what is already recorded;
        // a different set of features; and `flush` itself, which every
        // export goes through.
        if self.isolated != Some(features) || self.state_dirty {
            self.flush();
            // The frame this layer records under, applied once as it opens.
            // `rebuild_frame` is the same `restore_to_count`, `save`, clip
            // and matrix this used to spell out for itself.
            self.state_dirty = false;
            self.rebuild_frame();
            self.isolated = Some(features);
        }

        if let Some(canvas) = self.current.recording_canvas() {
            f(canvas);
            self.changed = true;
        }
    }

    pub fn get_page(&mut self) -> Page {
        self.settle();
        self.flush();

        Page {
            layers: self.layers.clone(),
            features: self.features.clone(),
            page_features: self.page_features,
            bounds: self.bounds,
            id: self.id.0,
            owner: Arc::downgrade(&self.id),
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
            PageCache::set(&page.owner, image, opts, self.surface.depth);
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

//
// Persistent GPU/CPU surface for caching intermediate results of getImageData()
//

/// One square of the page, composited independently.
///
/// Its own `depth`, so a tile that has been read stays incremental while its
/// neighbours are still empty -- which is the property that makes evicting one
/// cheap. A single page-sized surface has to be kept whole or replayed whole.
struct Tile {
    surface: Surface,
    /// Layers already played into this tile.
    depth: usize,
    /// Ticks on use, so the coldest tile is the one dropped.
    used: u64,
    /// A CPU copy of this tile, read from instead of the GPU.
    ///
    /// The same arrangement [`RecordingSurface::raster`] makes for a whole
    /// page, and for the same reason: reading a GPU surface flushes and
    /// waits for the device, and that wait is nearly all of what a read
    /// costs. The grid was introduced without it, which made a repeated
    /// small read pay a device sync every time -- a 64x64 read of an
    /// unchanged 600x600 page measured 6.9 microseconds when the page
    /// answered it and 143.6 once the grid did.
    ///
    /// A quarter of a megabyte where the page's copy is the whole canvas, so
    /// this is the cheaper of the two on any page bigger than one tile.
    raster: Option<SkImage>,
    /// The depth a read of this tile has already been served at.
    ///
    /// The copy waits for a second read at the same state, exactly as the
    /// page's does: the read that would pay for it may be the only one.
    served_at: Option<usize>,
}

impl Tile {
    /// Reads a rectangle of this tile, from its CPU copy where there is one.
    ///
    /// The page-wide read in [`RecordingSurface::copy_pixels`] spelled out
    /// the same three cases and this is that arrangement per tile: serve a
    /// current copy, and otherwise read the surface and make the copy only
    /// once a second read has arrived at the same state, because the read
    /// that would pay for it may be the only one.
    ///
    /// No engine argument, unlike the page's. A tile surface is only ever
    /// built by `make_compositing_surface` for the engine the read is on, so
    /// asking whether the copy is worth making is the same question as
    /// whether the snapshot can be brought back to the CPU -- and on the
    /// raster engine `make_raster_image` hands back the image it already is,
    /// which the `is_texture_backed` test below declines before allocating.
    fn copy_pixels(
        &mut self,
        info: &ImageInfo,
        pixels: &mut [u8],
        row_bytes: usize,
        at: (i32, i32),
    ) -> bool {
        if let Some(raster) = self.raster.as_ref() {
            return raster.read_pixels(
                info,
                pixels,
                row_bytes,
                at,
                CachingHint::Disallow,
            );
        }

        let repeat = self.served_at == Some(self.depth);
        if !repeat {
            self.served_at = Some(self.depth);
            return self.surface.read_pixels(info, pixels, row_bytes, at);
        }

        let snapshot = self.surface.image_snapshot();
        if !snapshot.is_texture_backed() {
            // Already memory: a copy would cost an allocation and save
            // nothing, which is the whole of what the page's engine test was
            // deciding.
            return self.surface.read_pixels(info, pixels, row_bytes, at);
        }

        match snapshot.make_raster_image(None, None) {
            Some(raster) => {
                let ok = raster.read_pixels(
                    info,
                    pixels,
                    row_bytes,
                    at,
                    CachingHint::Disallow,
                );
                self.raster = Some(raster);
                ok
            }
            // A snapshot Skia declines to bring back is not an error, just no
            // copy: the surface still has the pixels.
            None => self.surface.read_pixels(info, pixels, row_bytes, at),
        }
    }
}

/// The edge of a tile, in page pixels.
///
/// 256 squares, which is what compositors settle on and what the measurement
/// here agrees with: a 256x256 tile composites a twenty-fill page in 0.085 ms
/// against 1.343 for the whole 1200x900, and sixteen of them come to 1.357 --
/// the same total, so the decomposition costs nothing and touching one costs
/// a sixteenth.
///
/// Larger wastes memory on a small read; smaller multiplies the per-tile
/// replay of the command list, which is paid once per tile whatever its size.
const TILE: i32 = 256;

/// How many tiles a read may touch before it is served by the page instead.
///
/// One, because a read costs what it costs per call and not per pixel.
/// `Surface::read_pixels` measured about 430 microseconds on this machine
/// whether it was asked for 32x32 pixels or a whole tile, so a read spanning
/// a 2x2 patch of the grid made four of those calls where the page makes one,
/// and it did that however little was being read: a 64x64 crop straddling
/// four tiles cost 562 microseconds against 144 inside one.
///
/// That is what made `getImageData` over a whole 400x300 page 601
/// microseconds against 185 -- the page is exactly 2x2 tiles, so the full
/// read sat on the old budget of four and took the grid. A page needing more
/// tiles than the budget already fell back and was never slow: the same read
/// on an 800x600 page, which is twelve tiles, was 351 against 313.
///
/// The grid still does what it was built for. A hit test and a sampled pixel
/// are one tile by construction, and they keep the whole page from being
/// composited to answer them. What it must not do is split one read into
/// several.
const TILES_PER_READ: i32 = 1;

/// Tiles the grid keeps between reads.
///
/// Four, which is one megabyte at [`TILE`] square.
///
/// Not the same question as [`TILES_PER_READ`], though one constant used to
/// answer both. That was harmless while a read could touch four, and became a
/// thrash the moment a read was limited to one: the grid then held one tile,
/// so a hit test that moved between quadrants re-composited on every call.
/// Rotating four points around a 1024x1024 canvas cost 309 microseconds a
/// read against 139 for a fixed point.
const TILE_CACHE: usize = 4;

/// Ticks once per tile use, to order them for eviction.
static TILE_USES: AtomicU64 = AtomicU64::new(0);

/// How a PNG of one particular drawing is best encoded.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PngTuning {
    /// The row filters Skia may choose between, or `NONE` to store rows as
    /// they are.
    filter: png_encoder::FilterFlag,
    /// The deflate level, always [`DEFLATE_LEVEL`].
    level: i32,
}

/// How this image should be turned into PNG: whether to filter its rows, and
/// how hard to compress them.
///
/// Skia defaults to trying every row filter on every row and keeping the best,
/// at deflate level six. Each half of that is right for some drawings and
/// expensive for others, so both are measured here rather than assumed.
///
/// **The filter.** On a 1200x900 page, turning it off made a gradient 4.3 times
/// faster to encode and 3.4 times smaller, text 1.8 times faster and 1.6 times
/// smaller, and a flat fill 2.7 times faster at the same size -- while making a
/// photographic page 57% larger. So a few bands of rows are deflated as they
/// are and again after the Up filter, and filtering is asked for only where it
/// shrinks them.
///
/// **The level.** Not probed -- see [`DEFLATE_LEVEL`], which records why the
/// sample cannot answer that question and what pinning it costs.
///
/// The answer is about size and speed alone. PNG is lossless and both
/// filtering and deflate are reversible, so every setting here decodes to the
/// same pixels -- checked with five combinations whose files ran from 8.5 KB to
/// 52 KB and whose decoded bytes had one hash between them.
///
/// The probe costs about a millisecond against the tens it saves, and runs once
/// per export rather than once per page -- see [`FilterChoice`].
fn png_tuning(image: &SkImage) -> PngTuning {
    let (width, height) = (image.width(), image.height());
    // Bands as long as asked for where the page can spare them, and shared
    // out evenly where it cannot. A fixed forty-eight would have meant that
    // any page under ninety-six rows failed the guard below and was never
    // filtered at all -- a thumbnail, a sprite sheet row, a tiny chart. The
    // shape of the sample matters more than its size: two bands of sixteen
    // read a short page the same way two of forty-eight read a tall one.
    let band = band_rows(height);
    // What an unsampled or failed probe falls back to: Skia's own answer, at
    // Skia's own level, which is what this crate did before either was probed.
    let skias_own = PngTuning {
        filter: png_encoder::FilterFlag::ALL,
        level: DEFLATE_LEVEL as i32,
    };
    if width <= 0 || height < band * 2 {
        // Too little to sample, and too little for the choice to matter.
        return PngTuning {
            filter: png_encoder::FilterFlag::NONE,
            ..skias_own
        };
    }

    let info = ImageInfo::new(
        (width, band),
        ColorType::RGBA8888,
        AlphaType::Unpremul,
        None,
    );
    let row = info.min_row_bytes();
    let mut plain: Vec<u8> = Vec::new();
    let mut filtered: Vec<u8> = Vec::new();
    let mut sample = vec![0u8; row * band as usize];

    for n in 0..PROBE_BANDS {
        // Spread down the page, and never past its last full band.
        let top = band_top(n, height, band);
        // No context: the image reached here through
        // [`crate::gpu::owner`], so it is in main memory whichever engine
        // drew it.
        let read = image.read_pixels(
            &info,
            &mut sample,
            row,
            (0, top),
            CachingHint::Disallow,
        );
        if !read {
            // A readback that fails says nothing about the picture, so fall
            // back to Skia's own answer rather than to a guess.
            return skias_own;
        }

        accumulate(&sample, row, band as usize, &mut plain, &mut filtered);
    }

    let Some(filtering) = pays(&plain, &filtered) else {
        return skias_own;
    };

    PngTuning {
        filter: match filtering {
            true => png_encoder::FilterFlag::ALL,
            false => png_encoder::FilterFlag::NONE,
        },
        level: DEFLATE_LEVEL as i32,
    }
}

pub struct RecordingSurface {
    surface: Option<Surface>,
    /// The page in squares, for reads too small to be worth compositing it
    /// whole. Keyed by origin in page space.
    ///
    /// Mutually exclusive with `surface` rather than additional to it: a page
    /// that has been composited already answers every read, so the tiles are
    /// dropped when it appears and vice versa. Memory is the larger of the
    /// two, never their sum.
    tiles: HashMap<(i32, i32), Tile>,
    /// A CPU copy of the surface, read from instead of the GPU.
    ///
    /// `Surface::read_pixels` on a GPU surface flushes and waits for the
    /// device, and that wait is the whole cost: an 8x8 read measured 154
    /// microseconds against 7 on the CPU, flat against both the rectangle
    /// and the canvas -- 146 of which is this one call. Reading the same
    /// unchanged canvas again paid it again.
    ///
    /// Taken once per state and then read many times, so a run of reads
    /// costs one sync rather than one each. Held only on the GPU path,
    /// where a raster surface is already a memory read.
    raster: Option<SkImage>,
    /// The depth a direct read has already been served at.
    ///
    /// The copy is not made until a second read arrives at the same state,
    /// because it is a whole-page allocation and the read that pays for it
    /// may be the only one -- an image diff reads once and would gain
    /// nothing while paying for the page. A hit test reads many times and
    /// pays the sync once.
    served_at: Option<usize>,
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
            tiles: HashMap::new(),
            raster: None,
            served_at: None,
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
    /// The tile origins a page-space rectangle covers.
    ///
    /// Clamped to the page, so a read that runs past the edge asks for no
    /// tile that does not exist.
    fn tiles_covering(crop: IRect, page: ISize) -> Vec<(i32, i32)> {
        let clamped = IRect::new(
            crop.x().max(0),
            crop.y().max(0),
            crop.right().min(page.width),
            crop.bottom().min(page.height),
        );
        if clamped.is_empty() {
            return Vec::new();
        }
        let first = ((clamped.x() / TILE) * TILE, (clamped.y() / TILE) * TILE);
        let last = (
            ((clamped.right() - 1) / TILE) * TILE,
            ((clamped.bottom() - 1) / TILE) * TILE,
        );
        let mut out = Vec::new();
        let mut y = first.1;
        while y <= last.1 {
            let mut x = first.0;
            while x <= last.0 {
                out.push((x, y));
                x += TILE;
            }
            y += TILE;
        }
        out
    }

    /// A tile's rectangle in page space, clipped at the page edge so the
    /// last row and column are not padded.
    fn tile_rect(origin: (i32, i32), page: ISize) -> IRect {
        IRect::new(
            origin.0,
            origin.1,
            (origin.0 + TILE).min(page.width),
            (origin.1 + TILE).min(page.height),
        )
    }

    /// Brings every tile in `wanted` up to the page's current depth,
    /// allocating the ones that do not exist yet.
    ///
    /// Each tile replays only the layers it has not seen, exactly as the page
    /// surface does -- that is what keeps a read incremental after the tile
    /// has been evicted from nothing more than its neighbours.
    fn refresh_tiles(
        &mut self,
        wanted: &[(i32, i32)],
        page: &Page,
        opts: &ExportOptions,
        engine: &RenderingEngine,
    ) -> bool {
        let page_size = page.scaled_dimensions(opts.density);
        let target = page.layers.len();

        for origin in wanted {
            let rect = Self::tile_rect(*origin, page_size);
            if rect.is_empty() {
                return false;
            }

            if !self.tiles.contains_key(origin) {
                let Some(surface) = make_compositing_surface(
                    engine,
                    opts,
                    ISize::new(rect.width(), rect.height()),
                    &opts.surface_color_space,
                )
                .ok() else {
                    return false;
                };
                self.tiles.insert(
                    *origin,
                    Tile {
                        surface,
                        depth: 0,
                        used: TILE_USES.fetch_add(1, Ordering::Relaxed),
                        raster: None,
                        served_at: None,
                    },
                );
            }

            // SAFETY: inserted immediately above when absent.
            let tile = match self.tiles.get_mut(origin) {
                Some(tile) => tile,
                None => return false,
            };
            tile.used = TILE_USES.fetch_add(1, Ordering::Relaxed);
            if tile.depth == target && target > 0 {
                continue;
            }

            let canvas = tile.surface.canvas();
            if tile.depth == 0 {
                canvas.clear(self.matte.unwrap_or(Color::TRANSPARENT));
            }
            canvas.save();
            // The recording is in page coordinates; a tile draws the same
            // commands shifted by its own origin and lets Skia cull the rest.
            canvas.translate((-origin.0 as f32, -origin.1 as f32));
            canvas.scale((self.density, self.density));
            for pict in page.layers.iter().skip(tile.depth) {
                pict.playback(canvas);
            }
            canvas.restore();
            tile.depth = target;
            // Redrawn, so the copy describes a tile that is no longer there.
            // The page-wide copy is dropped on the same rule a few lines
            // below, and for the same reason.
            tile.raster = None;
            tile.served_at = None;
        }

        self.evict_tiles(wanted);
        true
    }

    /// Drops the coldest tiles once more are held than a read may touch.
    ///
    /// Never the ones this read is about to use, which is what `keep` names.
    fn evict_tiles(&mut self, keep: &[(i32, i32)]) {
        while self.tiles.len() > TILE_CACHE {
            let coldest = self
                .tiles
                .iter()
                .filter(|(origin, _)| !keep.contains(origin))
                .min_by_key(|(_, tile)| tile.used)
                .map(|(origin, _)| *origin);
            match coldest {
                Some(origin) => {
                    self.tiles.remove(&origin);
                }
                None => return,
            }
        }
    }

    /// Reads a rectangle out of the grid, one tile at a time.
    ///
    /// Each tile writes its own slice of the destination at the full row
    /// stride, so a read spanning four tiles lands as one image rather than
    /// four that have to be stitched afterwards.
    fn read_from_tiles(
        &mut self,
        dst_info: &ImageInfo,
        src: IRect,
        pixels: &mut [u8],
        page_size: ISize,
        wanted: &[(i32, i32)],
    ) -> bool {
        let row_bytes = dst_info.min_row_bytes();
        let pixel = dst_info.bytes_per_pixel();

        for origin in wanted {
            let rect = Self::tile_rect(*origin, page_size);
            let Some(part) = IRect::intersect(&src, &rect) else {
                continue;
            };
            let Some(tile) = self.tiles.get_mut(origin) else {
                return false;
            };

            let info = dst_info.with_dimensions((part.width(), part.height()));
            let at = (part.y() - src.y()) as usize * row_bytes
                + (part.x() - src.x()) as usize * pixel;
            let inside = (part.x() - origin.0, part.y() - origin.1);
            let read =
                tile.copy_pixels(&info, &mut pixels[at..], row_bytes, inside);
            if !read {
                return false;
            }
        }
        true
    }

    /// Whether the grid can serve this read, preparing it if so.
    ///
    /// False when the page surface already holds a current composite -- it
    /// answers the read for nothing -- when the rectangle is wider than the
    /// grid is worth, or when a tile could not be allocated. Every one of
    /// those falls back to the page rather than failing.
    pub fn try_tiled(
        &mut self,
        page: &Page,
        opts: &ExportOptions,
        engine: &RenderingEngine,
        crop: IRect,
    ) -> bool {
        if self.is_config_stale(opts)
            || self.gpu != Some(matches!(engine, RenderingEngine::GPU))
        {
            // The configuration decides how a tile is allocated, so adopt it
            // before any are, and drop what was composited under the old one.
            self.tiles.clear();
            self.surface = None;
            self.raster = None;
            self.served_at = None;
            self.depth = 0;
            self.gpu = Some(matches!(engine, RenderingEngine::GPU));
            self.color_space = opts.surface_color_space.clone();
            self.density = opts.density;
            self.matte = opts.matte;
            self.msaa = opts.msaa;
        }

        // A current page composite is the cheaper answer; leave it to serve.
        if self.surface.is_some() && self.depth == page.layers.len() {
            return false;
        }

        let page_size = page.scaled_dimensions(opts.density);
        let wanted = Self::tiles_covering(crop, page_size);
        if wanted.is_empty() || wanted.len() > TILES_PER_READ as usize {
            return false;
        }

        self.refresh_tiles(&wanted, page, opts, engine)
    }

    /// Serves a read out of the grid prepared by [`Self::try_tiled`].
    pub fn read_tiled(
        &mut self,
        dst_info: &ImageInfo,
        src: IRect,
        pixels: &mut [u8],
        page_size: ISize,
    ) -> bool {
        let wanted = Self::tiles_covering(src, page_size);
        self.read_from_tiles(dst_info, src, pixels, page_size, &wanted)
    }

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
        let was = (self.depth, reconfigure || recreate);

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
                // A composited page answers every read the tiles could, so
                // holding both would be paying twice for one picture.
                self.tiles.clear();
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

        // Anything that redrew the surface leaves the copy describing a
        // picture that is no longer on it. Keyed on the layer count rather
        // than on a dirty flag because that is what `update` itself uses to
        // decide what to replay -- one notion of "changed", not two.
        if was.1 || was.0 != self.depth {
            self.raster = None;
            self.served_at = None;
        }
    }

    pub fn snapshot_if_valid(
        &mut self,
        page: &Page,
        opts: &ExportOptions,
        engine: &RenderingEngine,
    ) -> Option<SkImage> {
        if self.is_config_stale(opts)
            || self.is_surface_stale(page, opts, engine)
            || self.depth == 0
        {
            return None;
        }

        let image = self
            .surface
            .as_mut()
            .map(|surface| surface.image_snapshot());

        // The shared cache holds pixels in main memory and nothing else -- see
        // [`crate::gpu::owner`] for why. This surface is the JavaScript
        // thread's, so on the GPU its snapshot is a texture belonging to a
        // context no exporting thread can use, and it is downloaded here
        // rather than by whoever finds it later. `PageCache::materialize` used
        // to be that later download, called from three places before an
        // asynchronous export; this is the same work in the one place that
        // knows it is needed.
        image.and_then(|image| match image.is_texture_backed() {
            false => Some(image),
            true => {
                let mut raster = None;
                engine.with_direct_context(|context| {
                    raster = image.make_non_texture_image(context);
                });
                raster
            }
        })
    }

    pub fn copy_pixels(
        &mut self,
        dst_info: &ImageInfo,
        src: IRect,
        pixels: &mut [u8],
        engine: &RenderingEngine,
    ) -> bool {
        let row_bytes = dst_info.min_row_bytes();
        let origin = (src.x(), src.y());

        // Serve from the CPU copy when there is a current one.
        if let Some(raster) = self.raster.as_ref() {
            return raster.read_pixels(
                dst_info,
                pixels,
                row_bytes,
                origin,
                CachingHint::Disallow,
            );
        }

        let Some(surface) = self.surface.as_mut() else {
            return false;
        };

        // The first read at a given state goes straight to the surface, and
        // only a second one is worth a copy of the whole page. On the CPU
        // path there is nothing to win -- the surface is already memory --
        // so the copy is never made and this is the only branch taken.
        let repeat = self.served_at == Some(self.depth);
        if !repeat || !matches!(engine, RenderingEngine::GPU) {
            self.served_at = Some(self.depth);
            return surface.read_pixels(dst_info, pixels, row_bytes, origin);
        }

        let raster = surface.image_snapshot().make_raster_image(None, None);
        match raster {
            Some(raster) => {
                let ok = raster.read_pixels(
                    dst_info,
                    pixels,
                    row_bytes,
                    origin,
                    CachingHint::Disallow,
                );
                self.raster = Some(raster);
                ok
            }
            // A snapshot Skia declines to bring back to the CPU is not an
            // error, just no cache: the surface still has the pixels.
            None => surface.read_pixels(dst_info, pixels, row_bytes, origin),
        }
    }
}

//
// Image generator for a single drawing context
//

#[derive(Debug, Clone)]
pub struct Page {
    pub id: usize,
    /// A weak reference to the generation this page was taken from, held so
    /// a store can tell whether that generation still exists. See
    /// [`PageId`].
    pub(crate) owner: Weak<PageId>,
    pub bounds: Rect,
    pub layers: Vec<Picture>,
    /// Index-aligned with `layers`; see [`PageRecorder::append_isolated`].
    pub(crate) features: Vec<VectorFeatures>,
    /// What a draw inside an open `saveLayer` asked for, which cannot be
    /// isolated and so speaks for the whole page.
    pub(crate) page_features: VectorFeatures,
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
            owner: Weak::new(),
            bounds: skia_safe::Rect::new_empty(),
            layers: vec![],
            features: vec![],
            page_features: VectorFeatures::PLAIN,
        }
    }
}

/// Adds the `viewBox` Skia does not write.
///
/// Skia's SVG writer gives the root a `width` and a `height` and nothing
/// else. Without a `viewBox` the file has no intrinsic ratio to scale by, so
/// `preserveAspectRatio` has nothing to work from and the drawing cannot be
/// fitted to a box of another size -- an `<img>` at 50% width, a container
/// that scales its contents, a design tool placing the file on a page.
///
/// Not a fix for macOS. Quick Look renders any SVG into a square canvas and
/// crops: a minimal file with a correct `width`, `height` and `viewBox`
/// comes back 900x900 from a 900x620 source, exactly as this one does. That
/// is the viewer's own behaviour and nothing written here changes it.
fn with_view_box(svg: &[u8], size: Size) -> Vec<u8> {
    let text = String::from_utf8_lossy(svg);
    let Some(root) = text.find("<svg") else {
        return svg.to_vec();
    };
    let Some(end) = text[root..].find('>').map(|at| root + at) else {
        return svg.to_vec();
    };
    if text[root..end].contains("viewBox") {
        return svg.to_vec();
    }

    // Skia writes `width="900"`, so the box beside it says `900` too rather
    // than `900.0`; a canvas built from a fraction keeps its decimals.
    let number = |value: f32| match value.fract() == 0.0 {
        true => format!("{}", value as i64),
        false => format!("{value}"),
    };
    let attribute = format!(
        r#" viewBox="0 0 {} {}""#,
        number(size.width),
        number(size.height)
    );

    let mut out = Vec::with_capacity(svg.len() + attribute.len());
    out.extend_from_slice(&svg[..end]);
    out.extend_from_slice(attribute.as_bytes());
    out.extend_from_slice(&svg[end..]);
    out
}

impl Page {
    /// Everything the page's draws asked of a vector backend, together.
    ///
    /// A canvas drawn into another arrives as a single flattened picture and
    /// the per-layer marks are lost with it, so the destination asks this
    /// and carries the answer on the draw that replays it.
    pub(crate) fn vector_features(&self) -> VectorFeatures {
        self.features
            .iter()
            .fold(self.page_features, |all, layer| all.with(*layer))
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

    /// Replays the page into a document canvas, rasterizing what that
    /// backend cannot express.
    ///
    /// `backend` is what it refuses -- [`VectorFeatures::SVG_CANNOT`] or
    /// [`VectorFeatures::PDF_CANNOT`] -- and the two are not the same set,
    /// which is the reason this asks rather than assuming. SVG drops sweep
    /// gradients, procedural shaders, filters, shadows and blend modes
    /// alike; PDF renders every one of those correctly and mishandles blend
    /// modes only. Rasterizing a shadowed page for PDF because SVG could not
    /// draw it would cost fidelity and size for nothing.
    ///
    /// The layers holding refused draws are marked when they are recorded;
    /// each run of them is rendered here and drawn in as an image, which
    /// both backends do embed. Everything else stays vector, and the marked
    /// layers are replaced in place rather than painted over, so a
    /// translucent draw is composited once rather than twice.
    fn draw_as_document(
        &self,
        canvas: &SkCanvas,
        backend: VectorFeatures,
        matte: Option<Color>,
        density: f32,
    ) -> Result<(), String> {
        if let Some(color) = matte {
            canvas.clear(color);
        }

        // Something was drawn inside an open `saveLayer`, where it could not
        // be split into a layer of its own, and this backend refuses it. The
        // whole page goes in as pixels.
        if self.page_features.refused_by(backend) {
            return self.embed_raster(canvas, &self.layers, density);
        }

        // Consecutive refused layers go in as one image rather than one
        // each. Every embedded image costs a page-sized surface, a playback
        // and a scan for its bounds, and a scene draws these in runs --
        // sixty shadowed panels in a row are sixty layers with nothing
        // vector between them. Rasterizing them separately took 1.1 seconds
        // where the same page without shadows took 8 milliseconds; as one
        // run it is a single image and the cost stops scaling with the
        // count.
        let refused = |index: usize| {
            self.features
                .get(index)
                .is_some_and(|features| features.refused_by(backend))
        };

        // A blend mode is the one refusal that cannot be rasterized on its
        // own. The others -- an exotic shader, an image filter, a mask filter
        // -- describe how a draw paints itself, so rendering that draw alone
        // and embedding the result is exactly right. A blend past source-over
        // describes how it combines with what is already there, and a layer
        // rendered by itself has nothing there: `multiply` came out blended
        // against transparency, and `clearRect` and `destination-out` embedded
        // nothing at all, because they lay down no ink of their own and
        // `embed_raster` crops to the ink it finds.
        //
        // So everything beneath such a draw has to go into the same image,
        // which is what gives it a backdrop. Rasterizing up to the *last* one
        // rather than the first keeps that true for every blend on the page,
        // and what is drawn after it stays vector -- a page that composites
        // early and draws normally afterwards still exports as mostly vectors.
        let blends = |index: usize| {
            self.features.get(index).is_some_and(|features| {
                features.refused_by(backend.with(VectorFeatures::BLEND_MODE))
                    && features.refused_by(VectorFeatures::BLEND_MODE)
            })
        };
        let backdrop_end = (0..self.layers.len()).rfind(|i| blends(*i));

        let mut index = 0;
        if let Some(last) = backdrop_end {
            self.embed_raster(canvas, &self.layers[..=last], density)?;
            index = last + 1;
        }

        while index < self.layers.len() {
            if !refused(index) {
                self.layers[index].playback(canvas);
                index += 1;
                continue;
            }

            let start = index;
            while index < self.layers.len() && refused(index) {
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

    /// This page's layers composited into one image in main memory.
    ///
    /// GPU work goes to the thread that owns the context -- see
    /// [`crate::gpu::owner`] for why there is only one. On the CPU there is no
    /// context to own, and rasterizing on the calling worker is what makes an
    /// animation export parallel, so it happens here.
    fn rasterized(
        &self,
        options: &ExportOptions,
        engine: RenderingEngine,
    ) -> Result<SkImage, String> {
        match engine {
            RenderingEngine::GPU => owner::composite(self, options),
            RenderingEngine::CPU => self.composite(options, engine),
        }
    }

    /// Composites this page on the thread that calls it, always answering with
    /// pixels in main memory.
    ///
    /// Not to be called directly on the GPU path -- [`Self::rasterized`] is
    /// the door, and it routes to the owner thread. This is what the owner
    /// runs.
    ///
    /// The download at the end is not a new cost. Skia's encoders read a
    /// texture-backed image back themselves, and the page cache used to do it
    /// too, under a `rayon::current_thread_index()` test that stood in for
    /// "am I allowed to share this". Doing it in one place, on the thread that
    /// owns the context, is what retires that question: no image that leaves
    /// here is texture-backed, so nothing downstream has to ask.
    /// Composites every layer onto a fresh surface, using the cached bitmap
    /// for the ones already rendered.
    ///
    /// The shared half of [`Self::composite`] and [`Self::composite_into`],
    /// which differ only in how they take the pixels off the surface
    /// afterwards. Hands back the depth the cache stood in for, because
    /// whether anything new needs caching is decided from it.
    fn composited_surface(
        &self,
        options: &ExportOptions,
        engine: RenderingEngine,
    ) -> Result<(Surface, usize), String> {
        let img_dims = self.scaled_dimensions(options.density);
        let img_scale =
            M44::from(Matrix::scale((options.density, options.density)));
        let mut surface = make_compositing_surface(
            &engine,
            options,
            img_dims,
            &options.surface_color_space,
        )?;
        let canvas = surface.canvas();

        // The cached bitmap stands in for the layers already drawn -- but only
        // where compositing it is the same operation as drawing them.
        //
        // It is not, on a multisampled surface with more layers still to draw.
        // Coverage there is per-sample and binary: drawing an edge twice
        // writes the same samples and resolves to the same value, while
        // compositing an already-resolved bitmap and then drawing over it
        // mixes a partial-alpha texel with fresh sample coverage. Measured on
        // this tree: an arc exported, drawn again and re-exported came out 192
        // bytes and up to 64 levels away from the same picture drawn in one
        // pass, so the same drawing commands gave different pixels depending
        // on whether an export happened in between. Identical at `msaa: 0` and
        // `msaa: 1`, and identical on the CPU, which is what identifies
        // multisampling rather than the cache itself.
        //
        // With nothing left to draw on top there is no second rasterization to
        // disagree with, so the case the cache exists for -- exporting an
        // unchanged canvas again -- keeps it. Only an export that follows
        // further drawing replays, which is work it was going to do for those
        // layers anyway.
        let multisampled = matches!(engine, RenderingEngine::GPU)
            && !matches!(options.msaa, Some(0 | 1));
        let (cache_image, cache_depth) = {
            let (image, depth) = PageCache::get(self.id, options, self.depth());
            match multisampled && depth != self.depth() {
                true => (None, 0),
                false => (image, depth),
            }
        };

        if let Some(image) = cache_image {
            // use the cached bitmap as the background
            canvas.draw_image(image, (0, 0), None);
        } else if let Some(color) = options.matte {
            // otherwise, fill the canvas if requested
            canvas.clear(color);
        }

        // draw newly added layers
        canvas.set_matrix(&img_scale);
        for pict in self.layers.iter().skip(cache_depth) {
            pict.playback(canvas);
        }

        Ok((surface, cache_depth))
    }

    /// Takes the composited page off `surface` as an image: converted into
    /// the requested space if that is not the one it was drawn in, brought
    /// back from the GPU if it is a texture, and filed in the page cache if
    /// there is anything new to file.
    ///
    /// The tail of [`Self::composite`], shared with [`Self::composite_into`],
    /// which needs it whenever it cannot take the shortcut of reading the
    /// surface directly.
    fn image_from(
        &self,
        surface: &mut Surface,
        options: &ExportOptions,
        engine: RenderingEngine,
        cache_depth: usize,
    ) -> Result<SkImage, String> {
        let img_dims = self.scaled_dimensions(options.density);

        let image = surface
            .make_temporary_image()
            .unwrap_or_else(|| surface.image_snapshot());

        // The surface holds the canvas's space; an encoder tags with whatever
        // the image carries. Converting here is what makes a requested output
        // space mean anything -- without it a P3 export of an sRGB canvas came
        // out sRGB, profile and all.
        //
        // Redrawn rather than `Image::make_color_space`, which returns `None`
        // for a GPU-backed image without a graphite recorder: drawing into a
        // surface of the target space converts on both backends.
        let image = match options.surface_color_space == options.color_space {
            true => image,
            false => {
                let out_info = ImageInfo::new_n32_premul(
                    img_dims,
                    options.color_space.clone(),
                );
                match engine.make_surface(&out_info, options) {
                    Ok(mut converted) => {
                        converted.canvas().draw_image(&image, (0, 0), None);
                        converted.image_snapshot()
                    }
                    Err(_) => image,
                }
            }
        };

        let image = match image.is_texture_backed() {
            false => image,
            true => {
                let mut context = surface.direct_context();
                image.make_non_texture_image(context.as_mut()).ok_or_else(
                    || "Could not read the page back from the GPU".to_string(),
                )?
            }
        };

        if self.depth() > cache_depth && !options.single_use {
            PageCache::set(&self.owner, image.clone(), options, self.depth());
        }

        Ok(image)
    }

    pub(crate) fn composite(
        &self,
        options: &ExportOptions,
        engine: RenderingEngine,
    ) -> Result<SkImage, String> {
        let (mut surface, cache_depth) =
            self.composited_surface(options, engine)?;
        self.image_from(&mut surface, options, engine, cache_depth)
    }

    /// Composites this page and reads it straight into `dst_info`'s layout.
    ///
    /// The same compositing as [`Self::composite`], including the cached
    /// bitmap standing in for layers already rendered, but the pixels do not
    /// take the long way round. `composite` has to answer with an image, so on
    /// the GPU it downloads the whole page before handing one back -- and a
    /// caller that only wants bytes then copies out of that image, paying for
    /// the page twice. Reading off the surface pays once.
    ///
    /// The download is not always waste: it is also what puts a bitmap in the
    /// page cache, and what makes the image safe to hand to another thread.
    /// So it still happens, but only when there is something new to cache.
    /// Exporting an unchanged page again -- which is the case this is for --
    /// finds the cache current and skips it entirely.
    ///
    /// The destination space and format are applied by `read_pixels`, which is
    /// where `composite` would have used a second surface to convert.
    pub(crate) fn composite_into(
        &self,
        options: &ExportOptions,
        engine: RenderingEngine,
        dst_info: &ImageInfo,
    ) -> Result<Vec<u8>, String> {
        let (mut surface, cache_depth) =
            self.composited_surface(options, engine)?;

        let stride = dst_info.min_row_bytes();
        let mut buffer: Vec<u8> = vec![0; checked_byte_size(dst_info)?];

        // Reading the surface is only a shortcut where nothing else needs the
        // page as an image. Two things do.
        //
        // A space to convert into, because that conversion is a redraw into a
        // surface of the target space rather than a readback: `read_pixels`
        // will convert too, and it does not answer the same bytes -- a P3
        // canvas asked for sRGB came out different under each.
        //
        // And a cache entry to fill, because filling it means bringing the
        // page back off the GPU anyway, and the pixels are then read out of
        // that copy rather than off the surface as well. Reading both would
        // be the second pass this exists to avoid.
        let converting = options.surface_color_space != options.color_space;
        let caching = self.depth() > cache_depth && !options.single_use;

        let read = match converting || caching {
            false => surface.read_pixels(dst_info, &mut buffer, stride, (0, 0)),
            true => {
                let image = self.image_from(
                    &mut surface,
                    options,
                    engine,
                    cache_depth,
                )?;
                image.read_pixels(
                    dst_info,
                    &mut buffer,
                    stride,
                    (0, 0),
                    CachingHint::Allow,
                )
            }
        };

        match read {
            true => Ok(buffer),
            false => Err(format!(
                "Could not read pixels into destination format ({:?} / {:?})",
                dst_info.color_type(),
                dst_info.alpha_type(),
            )),
        }
    }

    /// Composites every layer of this page and reads it into `info`'s layout.
    ///
    /// The whole page rather than the layers a cache has not seen: the callers
    /// -- `render_raw` and the animation frames -- ask for a frame, not an
    /// update, and nothing here is cached for the next one.
    pub(crate) fn composite_pixels(
        &self,
        options: &ExportOptions,
        engine: RenderingEngine,
        info: &ImageInfo,
    ) -> Result<Vec<u8>, String> {
        let img_dims = self.scaled_dimensions(options.density);
        let img_scale =
            M44::from(Matrix::scale((options.density, options.density)));
        let mut surface = make_compositing_surface(
            &engine,
            options,
            img_dims,
            &options.surface_color_space,
        )?;

        let canvas = surface.canvas();
        if let Some(color) = options.matte {
            canvas.clear(color);
        }
        canvas.set_matrix(&img_scale);
        for pict in self.layers.iter() {
            pict.playback(canvas);
        }

        let stride = info.min_row_bytes();
        let mut buffer: Vec<u8> = vec![0; checked_byte_size(info)?];
        match surface.read_pixels(info, &mut buffer, stride, (0, 0)) {
            true => Ok(buffer),
            false => Err(format!(
                "Could not read pixels into destination format ({:?} / {:?})",
                info.color_type(),
                info.alpha_type(),
            )),
        }
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
                quality: encoder_quality(options.quality),
                density: options.density,
                color: options.encoded_color_profile(),
                space: options.encoded_pixel_space(),
                depth: options.frame_depth(),
                bits: options.bit_depth,
                chroma: options.chroma.unwrap_or_default(),
                lossless: options.lossless,
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
            ..
        } = options;
        let size = self.bounds.size();
        let img_dims = self.scaled_dimensions(density);
        // `color_space` here is the *requested* one, which is what the "raw"
        // branch reads into and what the composited image already carries.
        // Compositing happens in the canvas's own space -- see
        // [`Self::composite`], which builds its surface from
        // `surface_color_space` and converts out of it, the way a browser's
        // canvas does.
        let img_quality = encoder_quality(quality) as u32;

        // Before the rasterizer, because raw pixels are not an encoding: the
        // caller wants the bytes, and going by way of an image means the page
        // is downloaded off the GPU and then copied out of again. Reading off
        // the compositing surface pays for it once. Everything below still
        // takes an image, because an encoder does.
        //
        // The requested space, not sRGB: the surface is built in
        // `surface_color_space` and `read_pixels` converts on the way out, so
        // pinning the destination to sRGB made `toBuffer("raw", {colorSpace})`
        // silently answer in sRGB. `get_pixels_as`, which backs
        // `getImageData`, passes the option through, and the two disagreed
        // about the same picture.
        if matches!(format, ImageFormat::Raw) {
            let dst_info = ImageInfo::new(
                img_dims,
                color_type,
                AlphaType::Unpremul,
                color_space.clone(),
            );
            return match engine {
                RenderingEngine::GPU => {
                    owner::composite_into(self, &options, &dst_info)
                }
                RenderingEngine::CPU => {
                    self.composite_into(&options, engine, &dst_info)
                }
            };
        }

        match format {
            ImageFormat::Pdf => {
                let mut pdf_bytes = Vec::new();
                let metadata = pdf::Metadata {
                    producer: PDF_PRODUCER.to_string(),
                    encoding_quality: Some(encoder_quality(quality) as i32),
                    raster_dpi: Some(density * NOMINAL_DPI),
                    ..Default::default()
                };
                let mut document = pdf_document(&mut pdf_bytes, &metadata)
                    .begin_page(size, None);
                let canvas = document.canvas();
                self.draw_as_document(
                    canvas,
                    VectorFeatures::PDF_CANNOT,
                    matte,
                    density,
                )?;
                document.end_page().close();
                Ok(pdf_bytes)
            }

            ImageFormat::Svg => {
                let canvas = svg::Canvas::new(
                    Rect::from_size(size),
                    options.svg_flags(),
                );
                self.draw_as_document(
                    &canvas,
                    VectorFeatures::SVG_CANNOT,
                    matte,
                    density,
                )?;
                Ok(with_view_box(canvas.end().as_bytes(), size))
            }

            // handle bitmap formats using (potentially gpu-backed) rasterizer
            _ => {
                let image = self.rasterized(&options, engine)?;

                // The image is in main memory whichever engine drew it, so
                // every encoder below is handed `None` where it would take a
                // context. Skia only wants one to read a texture back, and
                // there is no texture left to read.

                // handle image encoding
                match format {
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

                        jpeg_encoder::encode_image(None, &image, &jpg_opts).map(
                            |data| {
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
                            },
                        )
                    }

                    ImageFormat::Png => {
                        // `Options` is `#[non_exhaustive]`, so it is built
                        // and then adjusted rather than named field by field.
                        let mut png_opts = png_encoder::Options::default();
                        // Probed once per export rather than once per page:
                        // the answer is a property of the drawing, and every
                        // page of a written sequence shares these options.
                        let tuning = options.filter_choice.resolve(&image);
                        png_opts.filter_flags = tuning.filter;
                        png_opts.z_lib_level = tuning.level;

                        png_encoder::encode_image(None, &image, &png_opts).map(
                            |data| {
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
                            },
                        )
                    }

                    ImageFormat::Webp => {
                        let mut webp_opts = webp_encoder::Options::default();
                        if img_quality == QUALITY_SCALE as u32 {
                            webp_opts.compression =
                                webp_encoder::Compression::Lossless;
                            // Effort, not quality -- see the constant.
                            webp_opts.quality = WEBP_LOSSLESS_EFFORT;
                        } else {
                            webp_opts.compression =
                                webp_encoder::Compression::Lossy;
                            webp_opts.quality = img_quality as _;
                        }

                        webp_encoder::encode_image(None, &image, &webp_opts)
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
                    // `Raw` never reaches here -- it returns above, before a
                    // page is rasterized into an image it does not need -- but
                    // it is named rather than folded into a wildcard so that
                    // this match stays exhaustive over the enum.
                    ImageFormat::Raw
                    | ImageFormat::Pdf
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
        // The compositing surface takes the canvas's own space and
        // `compositing_color_type`. The destination space and any narrower
        // format are applied on the read_pixels destination, inside
        // [`Self::composite_pixels`], which is where those conversions belong.
        //
        // One surface per frame, and deliberately left that way. An animation
        // export calls this once per page, so a reviewer counting allocations
        // finds N of them and a pool looks obvious -- but the allocation is
        // not what a frame costs. Measured on this machine: 2.6 microseconds
        // on the GPU at both 960x540 and 1920x1080, and 1.2 to 36 on the CPU
        // where the buffer is actually zeroed, against 3.5 to 12.9
        // *milliseconds* for the frame around it. That is at most half a
        // percent, and two hundredths of one on the GPU. Asking for MSAA does
        // not change it.
        //
        // A pool is now possible where it was not -- the owner thread outlives
        // every frame and its context is not reaped under it -- and is still
        // not worth building for half a percent.
        match engine {
            RenderingEngine::GPU => {
                owner::composite_pixels(self, &surface_options, &dst_info)
            }
            RenderingEngine::CPU => {
                self.composite_pixels(&surface_options, engine, &dst_info)
            }
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
        // A float canvas has more than eight bits a channel, and three of
        // the formats written here can carry them -- PNG and TIFF state
        // sixteen outright, AVIF stores ten. Reading back at eight capped
        // every one of them at what the shallowest could take, and made an
        // animated PNG shallower than the still PNG of the same drawing.
        //
        // `R16G16B16A16UNorm` rather than the surface's own float type:
        // Skia converts on readback, and integers are what all three
        // encoders want. An eight-bit canvas still reads back as eight, so
        // nothing about the common case changes.
        let deep = options.frame_depth() == FrameDepth::Sixteen;
        let info = ImageInfo::new(
            dims,
            match deep {
                true => ColorType::R16G16B16A16UNorm,
                false => ColorType::RGBA8888,
            },
            AlphaType::Unpremul,
            options.encoded_color_space()?,
        );
        let bytes = self.render_raw(options.clone(), info, engine)?;
        Ok(Frame {
            pixels: match deep {
                true => Pixels::Sixteen(
                    bytes
                        .chunks_exact(2)
                        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                        .collect(),
                ),
                false => Pixels::Eight(bytes),
            },
            width: dims.width.max(0) as u32,
            height: dims.height.max(0) as u32,
            delay_ms,
        })
    }

    fn append_to<'a>(
        &self,
        doc: Document<'a>,
        matte: Option<Color>,
        density: f32,
    ) -> Result<Document<'a>, String> {
        if !self.bounds.is_empty() {
            let mut doc = doc.begin_page(self.bounds.size(), None);
            let canvas = doc.canvas();
            // The same treatment a one-page PDF gets. Both paths exist --
            // this one writes every page of a canvas, the other answers
            // `to_buffer` -- and a blend mode drawn on page two is no more
            // expressible than one drawn on a page of its own.
            self.draw_as_document(
                canvas,
                VectorFeatures::PDF_CANNOT,
                matte,
                density,
            )?;
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
    /// no reason to do it on one thread. The batch is then handed to the
    /// encoder whole rather than a frame at a time, so that a format whose
    /// frames compress independently can use those workers for that too --
    /// see [`FrameSink::write_batch`]. Writing stays sequential either way:
    /// a container is a single ordered stream, and a frame cannot be written
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
            quality: encoder_quality(options.quality),
            density: options.density,
            color: options.encoded_color_profile(),
            space: options.encoded_pixel_space(),
            depth: options.frame_depth(),
            bits: options.bit_depth,
            chroma: options.chroma.unwrap_or_default(),
            lossless: options.lossless,
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
            sink.write_batch(&frames)?;
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
            encoding_quality: Some(encoder_quality(quality) as i32),
            raster_dpi: Some(density * NOMINAL_DPI),
            ..Default::default()
        };
        self.pages
            .iter()
            .try_fold(pdf_document(out, &metadata), |doc, page| {
                page.append_to(doc, matte, density)
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

        // Each page is written once and never asked for again. See
        // [`ExportOptions::single_use`].
        let options = ExportOptions {
            single_use: true,
            ..options
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
    /// When this entry was last read or written, for eviction. See
    /// [`PAGE_CACHE_SIZE`].
    used: u64,
    /// What [`Self::image`] costs, for [`PAGE_CACHE_BYTES`]. Zero until one
    /// is stored, which is most entries: a page that never rasterizes holds
    /// nothing.
    bytes: usize,
}

impl Default for PageCache {
    fn default() -> Self {
        Self {
            image: None,
            depth: 0,
            density: 1.0,
            matte: None,
            msaa: None,
            used: CACHE_USES.fetch_add(1, Ordering::Relaxed),
            bytes: 0,
        }
    }
}

/// How many pages may hold a cached bitmap at once.
///
/// The map used to be unbounded, and an entry left it in exactly one place:
/// `Drop for PageRecorder`, which runs when V8 finalizes the `JsBox` holding
/// the `Context2D`. V8 sizes that box at a few machine words and cannot see
/// the half-megabyte `SkImage` behind it, so it feels little pressure to
/// collect and is slow to schedule the finalizer.
///
/// Measured before the bound: a thousand fresh 400x300 canvases, each drawn
/// once and exported, settled at 235 MB against 141 with it. The same
/// canvases exported to SVG cost 2 KB apiece, which is what identifies this
/// map rather than the machinery around it -- a vector export never reaches
/// `set`.
///
/// It does level off, so this is a plateau too high rather than a climb
/// without end: V8 gets to the boxes eventually, under pressure from its own
/// heap. What it will not do is get to them promptly, because nothing it can
/// see says they are worth collecting.
///
/// A count rather than a byte budget. Entries are page-sized so a count only
/// approximates bytes, but it needs no size accounting, and the working set
/// that matters -- the pages a frame actually redraws -- is small. A server
/// rendering a page per request keeps its own and evicts the finished ones.
const PAGE_CACHE_SIZE: usize = 64;

/// How much a cached bitmap may cost, in total, across every page.
///
/// A count alone is the wrong bound, because an entry is a whole rasterized
/// page and pages are not one size. Measured by exporting one card at a time
/// a thousand times and reading where resident memory settles: with only the
/// count in force, 0.76 MB pages settled at 184 MB, 3.0 MB pages at 290, and
/// 12 MB pages at 820 -- sixty-four pages every time, which for a server
/// drawing large cards is most of a gigabyte held to save a replay.
///
/// Sixty-four megabytes is the same order as Skia's own default resource
/// cache and covers what the memoization is for: a handful of canvases being
/// re-exported. Twenty-one pages at social-card size, five at four times
/// that, and the count still catches a flood of small ones.
const PAGE_CACHE_BYTES: usize = 64 * 1024 * 1024;

/// Ticks once per cache use, to order entries for eviction.
static CACHE_USES: AtomicU64 = AtomicU64::new(0);

impl PageCache {
    pub fn shared<'a>() -> &'a Arc<DashMap<usize, PageCache>> {
        CACHE.get_or_init(|| Arc::new(DashMap::new()))
    }

    /// Registers a page, and evicts only if that put the count over.
    ///
    /// A fresh entry carries no bitmap, so an `add` cannot put the byte
    /// budget over -- it can only raise the count, and only by one. Asking
    /// the map its length reads a counter per shard where
    /// [`Self::evict_over_bound`] walks every entry holding a guard, and
    /// this runs on every `new Canvas`, every `newPage`, and every
    /// `clearRect` that covers the whole canvas, because those rebuild the
    /// recorder. Walking sixty-four entries to discover that nothing needs
    /// evicting was most of what those three cost.
    pub fn add(id: usize) {
        let shared = Self::shared();
        shared.insert(id, PageCache::default());
        if shared.len() > PAGE_CACHE_SIZE {
            Self::evict_over_bound();
        }
    }

    pub fn drop(id: usize) {
        Self::shared().remove(&id);
    }

    /// Drops least-recently-used entries until the map is inside the bound.
    ///
    /// One walk however many entries have to go. The pass used to be inside
    /// the eviction loop, so each removal re-read every entry to find the
    /// next victim and to discover whether the bound was met -- and the
    /// common case, one entry over, still cost two full walks. That is paid
    /// on every `set`, which is every export of every page, and on every
    /// `add` that runs with the map at its bound, which is every `new
    /// Canvas` and every `newPage` in a process holding more than
    /// [`PAGE_CACHE_SIZE`] of them.
    ///
    /// The totals are still counted here rather than kept as a running
    /// total on the map. A stored total would be a number that can only
    /// drift: every insert, replacement and removal would have to adjust
    /// it, and one that did not would leave the cache either never evicting
    /// or always evicting, with nothing to correct it. What is carried
    /// across removals below is a local, recomputed from the same pass and
    /// discarded with it, so it cannot outlive the truth it was taken from.
    ///
    /// The iterator is dropped before any `remove`, so the shard guards it
    /// holds are released before a write lock is taken on the same map.
    fn evict_over_bound() {
        let shared = Self::shared();

        // (used, id, bytes) for every entry. One allocation of about sixty-
        // four triples, and only on a call that has something to evict --
        // the early return below is the path a `set` inside the bound takes.
        let mut entries: Vec<(u64, usize, usize)> = shared
            .iter()
            .map(|entry| (entry.used, *entry.key(), entry.bytes))
            .collect();

        let mut held: usize = entries.iter().map(|(_, _, bytes)| bytes).sum();
        let mut count = entries.len();

        // Both bounds, because either alone lets the other run away: a count
        // holds sixty-four pages however large, and bytes alone hold any
        // number of pages that carry no bitmap yet.
        if count <= PAGE_CACHE_SIZE && held <= PAGE_CACHE_BYTES {
            return;
        }

        entries.sort_unstable_by_key(|(used, _, _)| *used);

        for &(_, id, bytes) in &entries {
            if count <= PAGE_CACHE_SIZE && held <= PAGE_CACHE_BYTES {
                return;
            }

            // Evicting an entry with no bitmap frees nothing, so while it is
            // the byte budget that is over, only an entry carrying one is
            // worth taking.
            if held > PAGE_CACHE_BYTES && bytes == 0 {
                continue;
            }

            if shared.remove(&id).is_some() {
                held -= bytes;
                count -= 1;
            }
        }
    }

    pub fn get(
        id: usize,
        opts: &ExportOptions,
        depth: usize,
    ) -> (Option<SkImage>, usize) {
        Self::shared()
            .get_mut(&id)
            .map(|mut cache| {
                // Only a hit counts as a use. This marked every lookup,
                // which made the clock run backwards: a page whose entry no
                // longer matches -- a different density, matte or sample
                // count -- misses on every export and was marked fresh by
                // each of those misses, so it outranked a page that was
                // actually being served from the cache and outlived it under
                // eviction. What the bound is for is keeping the entries
                // that save a replay, and a miss saves nothing.
                match cache.is_valid(opts) && depth >= cache.depth {
                    true => {
                        cache.used = CACHE_USES.fetch_add(1, Ordering::Relaxed);
                        (cache.image.clone(), cache.depth)
                    }
                    false => (None, 0),
                }
            })
            .unwrap_or((None, 0))
    }

    /// Files a bitmap for a page, if that page still exists.
    ///
    /// An export runs on a worker and finishes whenever it finishes, so the
    /// generation it was given can be retired while it is still going: a
    /// full-canvas clear replaces it, and collecting the canvas releases it.
    /// Either way the entry has already been removed, and filing a bitmap
    /// under that key would put back memory nothing can read. See
    /// [`PageId`].
    pub fn set(
        owner: &Weak<PageId>,
        image: SkImage,
        opts: &ExportOptions,
        depth: usize,
    ) {
        let Some(owner) = owner.upgrade() else {
            return;
        };
        let id = owner.0;
        {
            // `entry` rather than `get_mut`: eviction can retire a page that
            // is still being drawn, and with `get_mut` alone that page would
            // never cache again -- correct, but paying a full replay on every
            // export for the rest of its life.
            let mut cache = Self::shared().entry(id).or_default();

            // The map is shared across threads and a texture belongs to the
            // context that made it, so an entry has to be in main memory.
            // `Page::composite` downloads before it returns and
            // `RecordingSurface::snapshot_if_valid` downloads before it hands
            // one over, which is the whole of how that holds -- asserted here
            // because it is the invariant, not because either has ever broken
            // it.
            debug_assert!(
                !image.is_texture_backed(),
                "the page cache is shared between threads and cannot hold a \
                 texture"
            );

            // save the bitmap if it's newer than the cached version, or is
            // replacing an invaildated cache
            if !cache.is_valid(opts) || depth > cache.depth {
                let bytes = image.image_info().compute_min_byte_size();
                *cache = Self {
                    image: Some(image),
                    density: opts.density,
                    matte: opts.matte,
                    msaa: opts.msaa,
                    depth,
                    used: CACHE_USES.fetch_add(1, Ordering::Relaxed),
                    bytes,
                }
            }
        }
        // outside the block above, so the entry's guard is released first
        Self::evict_over_bound();
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

/// Where the PNG row-filter probe's answer is kept for the rest of one export.
///
/// `toFile("frame-{}.png")` writes every page of a canvas through its own
/// [`Page::encoded_as`], so the probe used to run once a page: 150 times for
/// the animation it was measured on, at about a millisecond each, every one of
/// them asking the same question about the same drawing.
///
/// A cache keyed on the page would never hit -- `newPage()` builds a fresh
/// `PageRecorder` with a fresh id, so an animation's frames share no identity.
/// What they do share is the [`ExportOptions`] they were called with, cloned
/// per page from one original, so the answer lives here and the first page to
/// reach the encoder settles it for the rest. Two pages racing is harmless:
/// they probe the same drawing and reach the same answer.
///
/// Bounded to the call by construction. A later export builds its own options
/// and probes again, which is what makes this a shared answer rather than a
/// stale one.
#[derive(Debug, Default)]
struct Choice {
    /// The last answer reached, or `None` before any page has been probed.
    answer: RwLock<Option<PngTuning>>,
    /// Pages that have asked, so that one in [`PROBE_EVERY`] probes again.
    asked: AtomicUsize,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct FilterChoice(Arc<Choice>);

/// Pages between probes.
///
/// Sixteen, so a heterogeneous export is never more than fifteen pages behind
/// its own content while the probe still runs a fraction of the time it used
/// to. The cost this bounds is what probing every page costs -- 32 ms over a
/// 150-frame export, measured -- and the risk it bounds is a sequence whose
/// pages are not all the same kind of drawing: a report whose first page is
/// text and whose second is a photograph would otherwise encode the photograph
/// by the text's answer.
///
/// Being wrong here is bounded on both sides. A stale answer is one of the two
/// the probe would ever give, so the file is a little larger or a little slower
/// than it could have been, and never anything else.
const PROBE_EVERY: usize = 16;

impl FilterChoice {
    /// The answer for this page: the one already reached, or a fresh probe of
    /// `image` every [`PROBE_EVERY`] pages.
    ///
    /// Pages encode in parallel, so several early ones can find no answer yet
    /// and probe together. That is harmless -- they are probing the same
    /// drawing and reaching the same answer -- and it settles after the first
    /// few.
    fn resolve(&self, image: &SkImage) -> PngTuning {
        let asked = self.0.asked.fetch_add(1, Ordering::Relaxed);
        if !asked.is_multiple_of(PROBE_EVERY)
            && let Some(answer) = *self.0.answer.read()
        {
            return answer;
        }

        let answer = png_tuning(image);
        *self.0.answer.write() = Some(answer);
        answer
    }
}

/// Ignores the probe's answer.
///
/// `PartialEq` on [`ExportOptions`] is asked whether a cached page is still
/// valid for a call, and what a probe has answered so far has no bearing on
/// that. Two option sets that differ only here are the same request.
impl PartialEq for FilterChoice {
    fn eq(&self, _: &Self) -> bool {
        true
    }
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
    /// Bits a channel AVIF codes at, or `None` to follow the canvas. See
    /// [`EncodeOptions::bit_depth`](crate::export::EncodeOptions::bit_depth).
    pub bit_depth: Option<u8>,
    /// How AVIF samples chroma, or `None` for full. See
    /// [`EncodeOptions::chroma`](crate::export::EncodeOptions::chroma).
    pub chroma: Option<ChromaSampling>,
    /// Whether AVIF codes with no loss. See
    /// [`EncodeOptions::lossless`](crate::export::EncodeOptions::lossless).
    pub lossless: bool,
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
    /// Whether PNG row filtering has already been probed for this export.
    /// See [`FilterChoice`].
    pub(crate) filter_choice: FilterChoice,
    /// Whether this export is the only one these pages will get.
    ///
    /// Set by [`PageSequence::write_sequence`], which writes each page to its
    /// own file exactly once. Those pages are never asked for again -- a
    /// `newPage()` builds a fresh recorder with a fresh id -- so caching their
    /// bitmaps fills [`PAGE_CACHE_BYTES`] at a hit rate of zero. Measured on a
    /// 150-frame sequence: 681 MB of resident memory with the bitmaps kept,
    /// 594 without, same milliseconds and same bytes out.
    ///
    /// Only the store is skipped. A lookup still happens, because a page that
    /// *does* have an entry -- exported once already, then drawn on and
    /// written into a sequence -- should still replay only its new layers.
    pub(crate) single_use: bool,
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
            bit_depth: None,
            chroma: None,
            lossless: false,
            color_space: ColorSpace::new_srgb(),
            surface_color_space: ColorSpace::new_srgb(),
            surface_color_type: ColorType::N32,
            outline: true,
            fps: None,
            frame_delays: Vec::new(),
            loops: None,
            filter_choice: FilterChoice::default(),
            single_use: false,
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
        ColorProfile::of(self.encoded_pixel_space())
    }

    /// How deep the frames an encoder is handed will be.
    ///
    /// The canvas's own depth, not the export's request: `color_type` names
    /// what a *raster* export writes, while these encoders are handed pixels
    /// and decide their own. A canvas composited in float has more than
    /// eight bits to give whatever it is being saved as.
    ///
    /// Written out in full rather than as two names and a fallback. It was
    /// the fallback that was wrong: `_ => Sixteen` read every type but
    /// `RGBA8888` and `BGRA8888` as deep, so a canvas built `SRGBA8888`,
    /// `rgb`, `Gray8`, `R8UNorm`, `R8G8UNorm`, `RGB565` or `ARGB4444` --
    /// eight bits a channel or fewer, every one -- wrote a sixteen-bit APNG
    /// and TIFF holding no more than eight bits of information, at double
    /// the pixel data. The still PNG of the same canvas wrote eight, so the
    /// two disagreed about one drawing, and `bit_depth` is refused for those
    /// formats precisely because the canvas is supposed to answer this.
    ///
    /// Exhaustive, so a `skia-safe` upgrade that adds a colour type stops
    /// the build rather than being guessed at -- which is what a catch-all
    /// arm did here, in the direction that costs information density. The
    /// split follows `SkColorTypeMaxBitsPerChannel` in Skia's own
    /// `SkImageInfoPriv.h`: everything it reports above 8 is deep. That
    /// function is private to Skia and unbound, or this would call it.
    pub(crate) fn frame_depth(&self) -> FrameDepth {
        match self.color_type {
            // 8 bits a channel or fewer. `N32` is one of the two 8888s
            // depending on the platform, so it needs no arm of its own.
            ColorType::Alpha8
            | ColorType::RGB565
            | ColorType::ARGB4444
            | ColorType::RGBA8888
            | ColorType::RGB888x
            | ColorType::BGRA8888
            | ColorType::Gray8
            | ColorType::R8G8UNorm
            | ColorType::SRGBA8888
            | ColorType::R8UNorm => FrameDepth::Eight,

            // 10, 16 or 32 bits a channel: more than eight to give.
            ColorType::RGBA1010102
            | ColorType::BGRA1010102
            | ColorType::RGB101010x
            | ColorType::BGR101010x
            | ColorType::BGR101010xXR
            | ColorType::BGRA10101010XR
            | ColorType::RGBA10x6
            | ColorType::RGBAF16Norm
            | ColorType::RGBAF16
            | ColorType::RGBF16F16F16x
            | ColorType::RGBAF32
            | ColorType::A16Float
            | ColorType::R16Float
            | ColorType::R16G16Float
            | ColorType::A16UNorm
            | ColorType::R16UNorm
            | ColorType::R16G16UNorm
            | ColorType::R16G16B16A16UNorm => FrameDepth::Sixteen,

            // Not a surface format at all. Nothing composites into it, so
            // the shallower answer is the one that cannot waste anything.
            ColorType::Unknown => FrameDepth::Eight,
        }
    }

    /// The space the frames an encoder is handed are actually in.
    pub(crate) fn encoded_pixel_space(&self) -> PixelColorSpace {
        self.encoded_pixel_color_space()
            .unwrap_or(PixelColorSpace::Srgb)
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
        let asked = self.fps.unwrap_or(DEFAULT_FPS);
        let fps = match asked.is_finite() && asked > 0.0 {
            true => f64::from(asked).min(MS_PER_SECOND),
            false => f64::from(DEFAULT_FPS),
        };
        let at = |frame: usize| (frame as f64 * MS_PER_SECOND / fps).round();
        (at(index + 1) - at(index)) as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A deterministic image `rows` tall whose rows either repeat a noisy
    /// pattern shifted by a constant, or are unrelated noise.
    ///
    /// The first is what row filtering exists for: each row is high-entropy on
    /// its own, so the raw bytes resist compression, while the difference
    /// against the row above is nearly constant. The second is what defeats
    /// it -- differencing unrelated noise produces more noise.
    fn probe_image(width: i32, rows: i32, correlated: bool) -> SkImage {
        let info = ImageInfo::new(
            (width, rows),
            ColorType::RGBA8888,
            AlphaType::Unpremul,
            None,
        );
        let row_bytes = info.min_row_bytes();
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed >> 33) as u8
        };
        let pattern: Vec<u8> = (0..row_bytes).map(|_| next()).collect();
        let mut bytes = Vec::with_capacity(row_bytes * rows as usize);
        for y in 0..rows {
            for (x, base) in pattern.iter().enumerate() {
                let value = match correlated {
                    true => base.wrapping_add(y as u8),
                    false => next(),
                };
                // Opaque, so alpha is not what either case is measuring.
                bytes.push(match x % 4 == 3 {
                    true => 0xFF,
                    false => value,
                });
            }
        }
        images::raster_from_data(
            &info,
            skia_safe::Data::new_copy(&bytes),
            row_bytes,
        )
        .expect("a raster image of the bytes just built")
    }

    #[test]
    fn png_row_filtering_is_asked_for_only_where_it_pays() {
        // Skia's default is to try every filter on every row, which is right
        // for continuous-tone pictures and wrong for most of what a canvas
        // draws -- on a 1200x900 page it made a gradient 3.4 times larger and
        // 4.3 times slower to encode. Neither answer is a default, so the
        // encoder measures instead. This pins that it measures the right way
        // round.
        assert_eq!(
            png_tuning(&probe_image(256, 64, true)).filter,
            png_encoder::FilterFlag::ALL,
            "rows that differ from each other by a constant are what row \
             filtering is for"
        );
        assert_eq!(
            png_tuning(&probe_image(256, 64, false)).filter,
            png_encoder::FilterFlag::NONE,
            "differencing unrelated rows only makes more noise"
        );

        // An image too short to hold a pair says nothing either way, and must
        // not read past itself looking for one.
        assert_eq!(
            png_tuning(&probe_image(256, 1, true)).filter,
            png_encoder::FilterFlag::NONE,
        );
    }

    /// A dithered gradient: the one kind of drawing where deflate's deeper
    /// search earns its cost.
    ///
    /// The dither is what makes it, and it is not decoration. A clean ramp is
    /// compressed identically at every level -- measured, 1.000 either way --
    /// because the matches are exact and the first chain walked finds them.
    /// Skia dithers its gradients, and an ordered pattern laid over smooth
    /// colour is long-range structure that only a longer chain search picks up
    /// once the rows are filtered: 1.349 with the pattern below, against 1.000
    /// without it.
    ///
    /// Which is why the full-page measurement it stands for exists at all -- a
    /// 1200x900 gradient is 130 KB at level six and 231 at level four.
    fn dithered_gradient_image(width: i32, rows: i32) -> SkImage {
        let info = ImageInfo::new(
            (width, rows),
            ColorType::RGBA8888,
            AlphaType::Unpremul,
            None,
        );
        let row_bytes = info.min_row_bytes();
        let mut bytes = Vec::with_capacity(row_bytes * rows as usize);
        // The 4x4 ordered matrix, as Bayer defined it.
        const DITHER: [[u8; 4]; 4] =
            [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];
        for y in 0..rows {
            for x in 0..width {
                let noise = DITHER[(y & 3) as usize][(x & 3) as usize];
                let across = (x * 255 / width.max(1)) as u8;
                let down = (y * 255 / rows.max(1)) as u8;
                bytes.extend_from_slice(&[
                    across.wrapping_add(noise >> 2),
                    down.wrapping_add(noise >> 3),
                    (((x * 137 + y * 29) / 7) as u8).wrapping_add(noise),
                    0xFF,
                ]);
            }
        }
        images::raster_from_data(
            &info,
            skia_safe::Data::new_copy(&bytes),
            row_bytes,
        )
        .expect("a raster image of the bytes just built")
    }

    #[test]
    fn a_long_export_looks_at_its_content_again_as_it_goes() {
        // The answer is shared across the pages of one export, because they
        // are usually one drawing moving -- 150 frames of the animated eye
        // reach the same verdict either way, byte for byte. A sequence whose
        // pages differ is the case this guards: a report whose first page is
        // text and whose second is a photograph must not encode the
        // photograph by the text's answer for the rest of the file.
        let choice = FilterChoice::default();
        let filterable = probe_image(256, 64, true);
        let unfilterable = probe_image(256, 64, false);

        let first = choice.resolve(&filterable);
        assert_eq!(first.filter, png_encoder::FilterFlag::ALL);

        // The pages in between are served the answer already reached, whatever
        // they hold.
        for _ in 1..PROBE_EVERY {
            assert_eq!(
                choice.resolve(&unfilterable).filter,
                first.filter,
                "a page inside the interval takes the standing answer"
            );
        }

        assert_eq!(
            choice.resolve(&unfilterable).filter,
            png_encoder::FilterFlag::NONE,
            "and the page that lands on the interval probes for itself"
        );
    }

    #[test]
    fn the_deflate_level_is_pinned_rather_than_sampled() {
        // It was sampled, and the sample could not answer: deflate's deeper
        // search pays off over a whole image, so a few bands of rows put the
        // cheap level within 5.3% on a gradient that really cost 128% more.
        // Whatever the page, the level is now the same one.
        for image in [
            dithered_gradient_image(1200, 64),
            probe_image(256, 64, false),
            probe_image(256, 64, true),
        ] {
            assert_eq!(
                png_tuning(&image).level,
                DEFLATE_LEVEL as i32,
                "the level does not depend on the page"
            );
        }
    }

    #[test]
    fn a_cache_miss_does_not_count_as_a_use() {
        // The eviction clock used to be marked by every lookup rather than
        // by every hit, which made it run backwards. A page whose entry no
        // longer matches -- a different density here -- misses on every
        // export, and each of those misses marked it fresh, so it outranked
        // a page that was actually being served and outlived it under
        // eviction. The bound exists to keep the entries that save a replay.
        //
        // A minted generation rather than a hand-picked number, because the
        // map is process-wide and a store now needs an owner to file under.
        let owner = PageRecorder::mint_id();
        let id = owner.0;
        let weak = Arc::downgrade(&owner);
        let opts = ExportOptions {
            format: ImageFormat::Png,
            ..ExportOptions::default()
        };
        let used = |id| PageCache::shared().get(&id).map(|entry| entry.used);

        let mut surface = surfaces::raster_n32_premul((4, 4)).unwrap();
        PageCache::set(&weak, surface.image_snapshot(), &opts, 8);
        let stored = used(id).expect("the entry was just written");

        let mismatched = ExportOptions {
            density: opts.density + 1.0,
            ..opts.clone()
        };
        assert!(
            PageCache::get(id, &mismatched, 8).0.is_none(),
            "a different density cannot be served from this entry"
        );
        assert_eq!(used(id), Some(stored), "a miss is not a use");

        assert!(
            PageCache::get(id, &opts, 8).0.is_some(),
            "the options it was stored under are served"
        );
        assert!(
            used(id) > Some(stored),
            "a hit moves the entry to the front of the queue"
        );

        drop(owner);
        assert!(
            PageCache::shared().get(&id).is_none(),
            "retiring the generation takes its entry with it"
        );
    }

    #[test]
    fn a_page_written_once_is_not_kept() {
        // A sequence write gives every page its own file and never looks at
        // one again, so keeping its bitmap fills the cache at a hit rate of
        // zero -- 150 frames of the animated eye held 681 MB of resident
        // memory with the bitmaps kept and 594 without, at the same speed and
        // the same bytes out.
        //
        // Composited for real, on the raster engine, so this exercises the
        // path an export takes rather than restating its condition.
        // The recorder is returned with the page and held by the caller: it
        // owns the generation, and a store finds nothing to file under once
        // it is gone. Keeping it alive is what makes `single_use` the only
        // difference between the two exports below.
        let drawn = || {
            let mut recorder = PageRecorder::new(Rect::from_wh(8.0, 8.0));
            recorder.append(|canvas| {
                canvas.draw_rect(Rect::from_wh(4.0, 4.0), &Paint::default());
            });
            let page = recorder.get_page();
            (recorder, page)
        };
        let opts = ExportOptions {
            format: ImageFormat::Png,
            ..ExportOptions::default()
        };
        let once = ExportOptions {
            single_use: true,
            ..opts.clone()
        };
        let held = |id| {
            PageCache::shared()
                .get(&id)
                .is_some_and(|entry| entry.image.is_some())
        };

        let (_kept_recorder, kept) = drawn();
        kept.composite(&opts, RenderingEngine::CPU)
            .expect("a raster composite of an eight-pixel page");
        assert!(held(kept.id), "an ordinary export caches its bitmap");

        let (_once_recorder, discarded) = drawn();
        discarded
            .composite(&once, RenderingEngine::CPU)
            .expect("a raster composite of an eight-pixel page");
        assert!(
            !held(discarded.id),
            "a page written as part of a sequence keeps nothing"
        );
    }

    #[test]
    fn going_quiet_gives_the_bitmaps_back() {
        // What a canvas JavaScript has dropped holds until V8 gets round to
        // finalizing it, which it is slow to do because the box it can see
        // is a few words wide. The idle watcher in `crate::memory` calls
        // this once rendering has stopped; here it is called directly, since
        // waiting three seconds for a thread is not what is being tested.
        let mut recorder = PageRecorder::new(Rect::from_wh(8.0, 8.0));
        recorder.append(|canvas| {
            canvas.draw_rect(Rect::from_wh(4.0, 4.0), &Paint::default());
        });
        let page = recorder.get_page();
        let opts = ExportOptions {
            format: ImageFormat::Png,
            ..ExportOptions::default()
        };

        page.composite(&opts, RenderingEngine::CPU)
            .expect("a raster composite of an eight-pixel page");
        let held = || {
            PageCache::shared()
                .get(&page.id)
                .map(|entry| (entry.image.is_some(), entry.bytes))
        };
        assert_eq!(held(), Some((true, 256)), "the export cached its bitmap");

        release_cached_pages();
        assert_eq!(
            held(),
            Some((false, 0)),
            "going quiet drops the bitmap and stops counting its bytes"
        );
        assert!(
            PageCache::get(page.id, &opts, page.depth()).0.is_none(),
            "an emptied entry serves nothing"
        );

        // The identity stays, so the page caches again rather than replaying
        // in full for the rest of its life.
        page.composite(&opts, RenderingEngine::CPU)
            .expect("a raster composite of an eight-pixel page");
        assert_eq!(held(), Some((true, 256)), "the next export refills it");
    }

    #[test]
    fn a_retired_generation_cannot_be_filed_under() {
        // `set` creates the entry it cannot find so that a page evicted
        // while it is still being drawn can cache again. An export that
        // outlives its generation reaches the same line, and used to put
        // back the entry that retiring the generation had just removed --
        // memory no lookup could reach, freed only by the byte bound
        // evicting it again.
        let mut recorder = PageRecorder::new(Rect::from_wh(8.0, 8.0));
        recorder.append(|canvas| {
            canvas.draw_rect(Rect::from_wh(4.0, 4.0), &Paint::default());
        });
        let page = recorder.get_page();
        let opts = ExportOptions {
            format: ImageFormat::Png,
            ..ExportOptions::default()
        };

        // A full-canvas clear retires the generation `page` was taken from,
        // exactly as an opaque fill covering the canvas does.
        recorder.set_bounds(Rect::from_wh(8.0, 8.0));
        assert!(
            PageCache::shared().get(&page.id).is_none(),
            "retiring the generation removes its entry"
        );

        page.composite(&opts, RenderingEngine::CPU)
            .expect("a raster composite of an eight-pixel page");
        assert!(
            PageCache::shared().get(&page.id).is_none(),
            "an export that outlived its generation files nothing"
        );
    }

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
    fn a_canvas_of_eight_bits_or_fewer_is_never_called_deep() {
        // The list is written out rather than derived from `frame_depth`
        // itself, which would assert nothing. It is Skia's own
        // `SkColorTypeMaxBitsPerChannel` split, and the shallow half is
        // where the bug was: everything but the two 8888s answered
        // `Sixteen`, so seven of these wrote sixteen-bit files holding eight
        // bits of information.
        let depth = |color_type| {
            ExportOptions {
                color_type,
                ..ExportOptions::default()
            }
            .frame_depth()
        };

        for shallow in [
            ColorType::Alpha8,
            ColorType::RGB565,
            ColorType::ARGB4444,
            ColorType::RGBA8888,
            ColorType::RGB888x,
            ColorType::BGRA8888,
            ColorType::Gray8,
            ColorType::R8G8UNorm,
            ColorType::SRGBA8888,
            ColorType::R8UNorm,
            ColorType::N32,
        ] {
            assert_eq!(depth(shallow), FrameDepth::Eight, "{shallow:?}");
        }

        for deep in [
            ColorType::RGBA1010102,
            ColorType::BGRA1010102,
            ColorType::RGB101010x,
            ColorType::BGR101010x,
            ColorType::RGBA10x6,
            ColorType::RGBAF16,
            ColorType::RGBAF16Norm,
            ColorType::RGBAF32,
            ColorType::A16Float,
            ColorType::A16UNorm,
            ColorType::R16Float,
            ColorType::R16UNorm,
            ColorType::R16G16Float,
            ColorType::R16G16UNorm,
            ColorType::R16G16B16A16UNorm,
        ] {
            assert_eq!(depth(deep), FrameDepth::Sixteen, "{deep:?}");
        }
    }

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
