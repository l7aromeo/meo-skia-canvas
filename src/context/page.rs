use crc::{CRC_32_ISO_HDLC, Crc};
use dashmap::DashMap;
use little_exif::{
    exif_tag::ExifTag, filetype::FileExtension, metadata::Metadata,
};
use neon::prelude::*;
use parking_lot::RwLock;
use rayon::prelude::*;
use skia_safe::{
    AlphaType, BlendMode, Canvas as SkCanvas, ClipOp, Color, Color4f,
    ColorSpace, ColorType, Document, IRect, ISize, Image as SkImage, ImageInfo,
    M44, Matrix, Paint, Path, Picture, PictureRecorder, PixelGeometry, Pixmap,
    Point, Rect, Size, Surface, SurfaceProps, SurfacePropsFlags, XYZD50Hash,
    canvas::SrcRectConstraint,
    image::{BitDepth, CachingHint},
    jpeg_encoder, pdf, png_encoder,
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
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
};
const CRC32: Crc<u32> = Crc::<u32>::new(&CRC_32_ISO_HDLC);

use crate::{
    canvas::{DEFAULT_TEXT_CONTRAST, DEFAULT_TEXT_GAMMA},
    color::{
        RgbaLinear, linear_srgb_color_space, rgba_linear_to_unpremul_color4f,
    },
    context::{
        BoxedContext2D,
        faint::composited,
        layer::{
            BlurDraw, BlurOp, ClipData, Clipped, Composite, Coverage, CutOp,
            Group, Layer, LayerRec, Target, holds_a_cut, play_entries,
        },
    },
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
    gpu::{
        RenderingEngine,
        completion::{DRAW_ATTEMPTS, discarded, draws_ran},
        owner,
    },
    node::{
        canvas::BoxedCanvas,
        utils::{skia_paint_color, skia_paint_color_onto},
    },
    pixels::PixelColorSpace,
    started,
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
/// Called by the idle watcher in [`crate::memory`] once rendering and text
/// layout have stopped for a few seconds, which is the only moment at which a
/// bitmap is known not to be about to pay for itself. The entry it saves is
/// worth 1.9 milliseconds on a repeat 400x300 raw export and 1.2 on a 1200x900
/// PNG, so this is not free -- what makes it worth doing is that most of what
/// is held when a process goes quiet belongs to canvases JavaScript has already
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

/// The draws made on one page, recorded rather than rasterised.
///
/// Kept until the page is erased -- by `reset()`, an opaque fill covering the
/// page, or a `clearRect` covering it, all through
/// [`set_bounds`](Self::set_bounds) -- because vector export replays them;
/// the raster surface only caches a replay. Nothing rasterises and drops
/// them early.
///
/// So what a recorded draw holds lives as long as the page does, and a text
/// draw holds the typeface it was laid out in. A face with an `opsz` axis is
/// a typeface of its own at every font size, so an uncleared page of such
/// text keeps one instance per distinct size it was drawn at, about 36 KB
/// each: ten thousand `fillText` calls at distinct Amstelvar sizes measured
/// 403 MB of resident memory, where the same draws laid out in one typeface
/// across every size, as they were before `opsz` followed the size,
/// measured 50.
pub struct PageRecorder {
    current: Recorders,
    /// The recording so far, shared with every [`Page`] taken of it since
    /// it last grew, so that taking a page copies a pointer whatever its
    /// length. Growing it copies the list only while a page taken before
    /// still holds it -- see [`PageRecorder::page`].
    layers: Arc<Vec<Layer>>,
    /// Index-aligned with `layers`: what each layer's draws asked of a
    /// vector backend. Empty for the ordinary ones. See
    /// [`PageRecorder::append_isolated`]. Shared as `layers` is.
    features: Arc<Vec<VectorFeatures>>,
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
    /// Counts changes to `matrix` and `clip`, so that a draw can tell the
    /// frame is the one an earlier draw was made under.
    frame: u64,
    /// The frame the last entry filed, a [`BlurOp`], was opened in while it
    /// can still be joined; `None` once anything could have seen it -- a
    /// flush, which every export goes through, records it for good.
    blur_run: Option<u64>,
    /// The frame the last layer, a [`CutOp`], was opened in while it can
    /// still be joined, as `blur_run` is for a [`BlurOp`].
    cut_run: Option<u64>,
    /// What is drawn under the frame's clip, recorded without it, while the
    /// frame lasts -- see [`Run`].
    run: Option<Run>,
    /// The recorders the last run closed with, for the next -- see
    /// [`Spare`].
    spare: Option<Spare>,
    /// The recorders the last cut draw was recorded with, for the next --
    /// see [`Self::record_cut`].
    cut_spare: Option<Spare>,
    /// How many times one replay of this page walks a nested picture.
    ///
    /// Zero for a page that has only been drawn on directly, however much has
    /// been drawn: those draws replay once each and nothing multiplies. It
    /// counts only what nesting costs, because that is the part that is not
    /// linear.
    ///
    /// Drawing one canvas into another keeps the source as a nested picture,
    /// and a picture reached by two paths is replayed twice while being
    /// recorded once. So a page copied into a fresh canvas and drawn back,
    /// round after round, records a structure that grows by a constant and
    /// replays one that doubles. Skia's `approximate_op_count_nested` counts
    /// the nodes and read 4, 13, 22, 31, 40 across five rounds whose
    /// rasterization went 0.06, 0.27, 0.94, 1.78, 3.54 seconds -- which is
    /// why the size of the recording cannot be the trigger.
    ///
    /// Any value above zero means this page already carries someone else's
    /// picture, and `node::image::Source::of` rasterizes it rather than let a
    /// second level of nesting form. The count is kept rather than a flag
    /// because it says how much would be replayed, which is what the
    /// measurement in that function is about.
    replay_cost: usize,
    id: Arc<PageId>,
    /// The `saveLayer`s open on the page, outermost first. While one is
    /// open `current` records the innermost one's contents, and what each
    /// holds besides is filed in its [`OpenLayer`] -- see
    /// [`PageRecorder::open_layer`].
    open: Vec<OpenLayer>,
}

/// A `saveLayer` open on a [`PageRecorder`].
///
/// The layer is drawn -- opened, its contents drawn into it, closed -- only
/// once it closes, as a [`Group`] filed where it was opened: on the page, or
/// among the contents of the layer around it. A recording that a read or an
/// export has to finish part way therefore never holds a layer open: what
/// it holds of an open layer is a copy of the contents so far, composited
/// through a copy of the group for that read alone, while the contents go
/// on recording into the one layer they belong to.
struct OpenLayer {
    /// Names the layer to the raster a read keeps of what it has drawn --
    /// see [`RecordingSurface::read_rastered`]. Unique for the process, so
    /// that a layer opened where another closed is never taken for it.
    id: u64,
    /// What the layer holds, filed in order: what `current` recorded before
    /// a group was filed among it, and the groups -- a layer opened inside
    /// this one, a canvas drawn in through a paint.
    entries: Vec<Layer>,
    rec: LayerRec,
    /// The transform and clip the layer was opened under.
    matrix: Matrix,
    clip: Option<Path>,
    /// What the layer's contents asked of a vector backend, which the layer
    /// asks of it in turn.
    features: VectorFeatures,
}

impl OpenLayer {
    /// Whether a read can keep a raster of what the layer has drawn and
    /// composite it as the layer would -- see
    /// [`RecordingSurface::read_rastered`]: a layer that composites
    /// source-over through nothing but an alpha, onto nothing it filters,
    /// confined to nothing of its own, opened under a clip that covers each
    /// pixel of `density`'s device whole or not at all, and holding no draw
    /// the clip cuts. Its device then starts transparent, holds what is drawn
    /// in it and nothing else, and on closing is drawn through its paint,
    /// which a raster drawn the same way reproduces. Any other is replayed.
    ///
    /// The clip, because Skia clips an antialiased edge as it blends each
    /// draw, at eight bits: a raster of the draws clipped afterwards rounds
    /// that edge another way, and one clipped as it is drawn and again as it
    /// is composited rounds it twice. A layer opened under a round clip read
    /// from a raster came out up to 12 levels from the layer closed at its
    /// edge, either way. A clip on whole pixels clips nothing by part.
    ///
    /// A cut draw, because a layer holding one is closed under its clip as
    /// a clip shader -- see `Group::playback_within` -- which Skia blends
    /// through its raster pipeline, where a raster composited under the clip
    /// path takes the legacy blitter on an eight-bit sRGB page, and the two
    /// round apart.
    fn rasters(&self, density: f32) -> bool {
        let whole_pixels = |clip: &Path| {
            !clip.is_inverse_fill_type()
                && clip.is_rect().is_some_and(|(rect, ..)| {
                    let device =
                        Matrix::scale((density, density)).map_rect(rect).0;
                    [device.left, device.top, device.right, device.bottom]
                        .iter()
                        .all(|edge| edge.fract() == 0.0)
                })
        };
        self.rec.bounds.is_none()
            && self.rec.footprint(Some(Rect::new_empty())).is_some()
            && self.clip.as_ref().is_none_or(whole_pixels)
            && !holds_a_cut(&self.entries)
    }
}

/// The depth a deferred picture image can carry for a canvas of `color_type`.
///
/// [`BitDepth`] names only U8 and F16, so a canvas at `RGBAF32` is carried at
/// F16 -- narrower than the surface it draws on, and the widest the
/// deferred-image API offers. F16 holds values outside `[0, 1]`, which is
/// what an extended-range canvas is for, and its exponent puts eight levels
/// between two eight-bit steps near 1.0 and two thousand near 0.002, where a
/// canvas built for low-alpha accumulation needs them.
fn source_bit_depth(color_type: ColorType) -> BitDepth {
    match color_type {
        ColorType::RGBAF16 | ColorType::RGBAF16Norm | ColorType::RGBAF32 => {
            BitDepth::F16
        }
        _ => BitDepth::U8,
    }
}

/// Which of a page's recordings a draw is being recorded into, and so which
/// paint it records with -- see [`Recorders`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recording {
    /// The only recording of a page that keeps one, played onto every
    /// target: every colour as a plain paint colour.
    Shared,
    /// The recording a page that keeps two plays as pixels: a flat colour
    /// held exactly in the page's space, where it has one.
    Pixels,
    /// The recording a page that keeps two hands a document backend: every
    /// colour as a plain paint colour.
    Document,
}

impl Recording {
    /// The target a [`Layer`] recorded into this recording is played for.
    pub fn target(self) -> Target {
        match self {
            Recording::Pixels => Target::Pixels,
            Recording::Shared | Recording::Document => Target::Document,
        }
    }
}

/// The recording a page is drawn into: one, or on a page whose colours Skia
/// cannot land exactly, two -- see [`Recording`].
///
/// Skia stores a paint's colour in sRGB and converts it into the page's
/// space at the draw through skcms's approximate transfer function, whose
/// output lies on a grid 64 ulps wide, so no plain colour lands exactly on a
/// float page in another space. An image shader in the page's own space
/// does, but a document backend writes it as a pattern. So such a page
/// records every draw twice, and a draw is handed each recording in turn to
/// record with the paint that recording needs.
struct Recorders {
    pixels: PictureRecorder,
    document: Option<PictureRecorder>,
}

impl Recorders {
    fn new(bounds: Rect, dual: bool) -> Self {
        Self::new_with(bounds, dual, true)
    }

    /// As [`Self::new`], with a bounding hierarchy where `bbh` asks for one.
    fn new_with(bounds: Rect, dual: bool, bbh: bool) -> Self {
        let mut recorders = Recorders {
            pixels: PictureRecorder::new(),
            document: dual.then(PictureRecorder::new),
        };
        recorders.begin_with(bounds, bbh);
        recorders.each(|canvas, _| {
            canvas.save(); // start at depth 2
        });
        recorders
    }

    /// Hands `f` each recording's canvas, and which recording it is.
    fn each(&mut self, f: impl Fn(&SkCanvas, Recording)) {
        let shared = self.document.is_none();
        if let Some(canvas) = self.pixels.recording_canvas() {
            f(
                canvas,
                match shared {
                    true => Recording::Shared,
                    false => Recording::Pixels,
                },
            );
        }
        if let Some(canvas) = self
            .document
            .as_mut()
            .and_then(PictureRecorder::recording_canvas)
        {
            f(canvas, Recording::Document);
        }
    }

    fn begin(&mut self, bounds: Rect) {
        self.begin_with(bounds, true);
    }

    /// Begins every recording in `bounds`, with a bounding hierarchy where
    /// `bbh` asks for one.
    fn begin_with(&mut self, bounds: Rect, bbh: bool) {
        self.pixels.begin_recording(bounds, bbh);
        if let Some(document) = &mut self.document {
            document.begin_recording(bounds, bbh);
        }
    }

    /// Finishes every recording with `finish` and files what they hold as
    /// one entry: a picture, or on a page that keeps two, both.
    fn finish(
        &mut self,
        finish: impl Fn(&mut PictureRecorder) -> Option<Picture>,
    ) -> Option<Layer> {
        let pixels = finish(&mut self.pixels)?;
        Some(match &mut self.document {
            None => Layer::Picture(pixels),
            Some(document) => {
                let document = finish(document)?;
                Layer::Targeted(Arc::new((pixels, document)))
            }
        })
    }

    /// Whether this page keeps a recording for each kind of target.
    fn dual(&self) -> bool {
        self.document.is_some()
    }
}

#[cfg(test)]
thread_local! {
    /// Whether a page records what is drawn under a clip as a [`Run`] on
    /// this thread: off, every frame's clip is recorded into the page's own
    /// recording, the reference a run's pixels are held to.
    pub(crate) static KEEPS_RUNS: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
}

/// The draws of one frame under a clip -- one clip, one transform --
/// recorded without the clip while the frame lasts.
///
/// A PDF or SVG device writes a clip group for each clip it is handed, and
/// keys it by the clip's generation, so a clip recorded again in a second
/// picture is a second group even where it is the same path. A page that
/// filed its recording part way through a frame -- for a read, an export,
/// or a draw the clip cuts -- recorded the clip again in the next, and its
/// document wrote the frame's clip as many times as it was filed.
///
/// Recorded without the clip, a run is filed whole when its frame ends:
/// drawn into the page's recording under the clip, so that the page holds
/// it as it would have, where nothing split it; and as [`Clipped`] pieces
/// where something did, which a document plays under the clip once. Ends at
/// every change of frame, as the page's recording applies the clip again
/// at each, so that a document keeps the groups it had.
struct Run {
    /// Names the run, so that a document groups its pieces -- see
    /// [`Clipped::run`].
    id: u64,
    /// The frame the run records -- see [`PageRecorder::frame`].
    frame: u64,
    clip: Path,
    matrix: Matrix,
    recorders: Recorders,
    /// Whether `recorders` holds a draw.
    changed: bool,
    /// Whether the run has filed a piece, so that what is left of it is
    /// filed as one too.
    split: bool,
    /// How many runs before this one recorded nothing into its recording --
    /// see [`Spare`].
    idle: u32,
    /// The clip as its pieces keep it, made once, where the run has filed
    /// one.
    shape: Option<ClipData>,
    /// The clip as coverage, made once, where the run holds a cut draw.
    coverage: Option<Arc<Coverage>>,
    /// Whether the run records with a bounding hierarchy, which it does
    /// inside an open `saveLayer`. Without one its picture is bounded by the
    /// page, and so by the clip's bounds where it is played under the clip,
    /// rather than by what it draws, and finishing it skips computing the
    /// bounds of every operation. Inside a layer those bounds are what the
    /// layer is confined to -- see [`LayerRec::footprint`] -- and a layer the
    /// size of the clip around one small draw cost a page of translucent
    /// layers under a large clip some fifteen times what it did.
    bbh: bool,
}

impl Run {
    /// A run of the frame `frame`, recording into `spare`'s recorders --
    /// recording already, as a run closing hands them back.
    fn open(frame: u64, clip: &Path, matrix: Matrix, spare: Spare) -> Self {
        let Spare {
            mut recorders,
            idle,
            bbh,
        } = spare;
        static RUNS: AtomicU64 = AtomicU64::new(0);
        // A run's recording holds no save of its own: finishing one then has
        // nothing to restore, which in a sampled profile was 0.03 of the
        // 0.27 microseconds closing a run cost.
        // A spare made for the run was begun at depth 2, as every recording
        // is, and is brought back to 1 here.
        recorders.each(|canvas, _| {
            canvas.restore_to_count(1);
            canvas.set_matrix(&matrix.into());
        });
        Run {
            id: RUNS.fetch_add(1, Ordering::Relaxed),
            frame,
            clip: clip.clone(),
            matrix,
            recorders,
            changed: false,
            split: false,
            idle,
            shape: None,
            coverage: None,
            bbh,
        }
    }

    /// What the run has recorded since it opened or this was last asked,
    /// its recorders recording again, begun afresh; `None` where it has
    /// recorded nothing, its recorders left as they were.
    fn finish(&mut self, bounds: Rect) -> Option<Layer> {
        if !self.changed {
            return None;
        }
        let entry = self
            .recorders
            .finish(|recorder| recorder.finish_recording_as_picture(None));
        // Begun again at depth 1 -- see `Run::open`.
        self.recorders.begin_with(bounds, self.bbh);
        self.changed = false;
        entry
    }

    /// The clip as the run's pieces keep it, made the first time it is
    /// asked for.
    fn shape(&mut self) -> ClipData {
        self.shape
            .get_or_insert_with(|| ClipData::of(&self.clip))
            .clone()
    }

    /// The clip as coverage for the run's cut draws, made the first time it
    /// is asked for; `None` where a picture of it could not be made.
    fn coverage(&mut self) -> Option<Arc<Coverage>> {
        if self.coverage.is_none() {
            self.coverage = Coverage::of(&self.clip).map(Arc::new);
        }
        self.coverage.clone()
    }

    /// As [`Self::finish`], recording on under the run's transform: for a
    /// run that goes on past a split.
    fn take(&mut self, bounds: Rect) -> Option<Layer> {
        let entry = self.finish(bounds)?;
        let matrix = self.matrix;
        self.recorders.each(|canvas, _| {
            canvas.set_matrix(&matrix.into());
        });
        Some(entry)
    }
}

/// The recorders the last run closed with, recording, kept for the next run
/// to record into.
///
/// A `PictureRecorder` carries a recording canvas of its own, and making one
/// for every frame drawn under a clip was most of what a run cost. A run
/// that recorded something hands its recorders back begun afresh; one that
/// recorded nothing hands them back as they were, holding only the frames
/// opened in them, which cost a picture nothing when it is finished but
/// grow the recording -- so after [`Spare::IDLE_FRAMES`] of them the
/// recording is begun afresh.
struct Spare {
    recorders: Recorders,
    /// How many runs have recorded nothing into the recording since it was
    /// begun.
    idle: u32,
    /// Whether the recording keeps a bounding hierarchy -- see [`Run::bbh`].
    /// A run takes the spare only where it wants the same.
    bbh: bool,
}

impl Spare {
    /// How many runs that recorded nothing a spare recording carries before
    /// it is begun afresh: enough that a frame drawn under a clip seldom
    /// pays for it, few enough that the frames left in it stay a few
    /// hundred operations.
    const IDLE_FRAMES: u32 = 64;
}

impl PageRecorder {
    /// The bytes this page holds in pixels. See
    /// [`RecordingSurface::resident_bytes`].
    pub fn resident_bytes(&self) -> usize {
        self.surface.resident_bytes()
    }

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
        Self::recording(bounds, false)
    }

    /// A recorder for a page that keeps one recording, or with `dual`, one
    /// for each kind of target -- see [`Recorders`].
    pub fn recording(bounds: Rect, dual: bool) -> Self {
        let id = Self::mint_id();

        PageRecorder {
            current: Recorders::new(bounds, dual),
            layers: Arc::default(),
            features: Arc::default(),
            changed: false,
            isolated: None,
            state_dirty: false,
            frame: 0,
            blur_run: None,
            cut_run: None,
            run: None,
            spare: None,
            cut_spare: None,
            matrix: Matrix::default(),
            clip: None,
            bounds,
            id,
            surface: RecordingSurface::default(),
            replay_cost: 0,
            open: Vec::new(),
        }
    }

    /// Records `f` into every recording the page keeps, each handing it
    /// the canvas and which recording it is -- see [`Recording`] -- so that a
    /// draw cannot reach one and miss the other.
    pub fn append<F>(&mut self, f: F)
    where
        F: Fn(&SkCanvas, Recording),
    {
        // An ordinary draw cannot join an isolated layer: that layer is
        // marked for rasterization, and everything in it is rasterized with
        // it. Close it first so this draw stays a vector.
        if self.isolated.is_some() {
            self.flush();
        }
        self.settle();
        match &mut self.run {
            Some(run) => {
                run.recorders.each(f);
                run.changed = true;
            }
            None => {
                self.current.each(f);
                self.changed = true;
            }
        }
    }

    /// Records `f` outside the frame, with neither its transform nor its
    /// clip: `putImageData`, which the Canvas standard has write the page
    /// unclipped. A draw cannot drop a clip applied outside its recording,
    /// which is where a run's is -- see [`Run`] -- so the run is closed and
    /// `f` recorded into the page's own recording, as every draw was before
    /// runs. The next draw frames that recording again.
    pub fn append_unframed<F>(&mut self, f: F)
    where
        F: Fn(&SkCanvas, Recording),
    {
        if self.isolated.is_some() {
            self.flush();
        }
        self.close_run();
        self.current.each(|canvas, recording| {
            canvas.restore_to_count(1);
            f(canvas, recording);
        });
        self.changed = true;
        self.reframe();
    }

    /// Whether anything is recorded and not yet filed: in the page's
    /// recording, or in the run of the frame.
    fn pending(&self) -> bool {
        self.changed || self.run.as_ref().is_some_and(|run| run.changed)
    }

    /// Whether this page keeps a recording for each kind of target -- see
    /// [`Recorders`].
    pub fn dual(&self) -> bool {
        self.current.dual()
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
        if self.pending() || !self.layers.is_empty() {
            // The identity this replaces is the last strong reference to the
            // retired generation, so assigning here is what drops its cache
            // entry and what tells an export still in flight for it that it
            // has nothing left to file.
            self.id = Self::mint_id();
        }

        // Finish before beginning again: `flush` reuses the recorder the same
        // way, and Skia expects the open recording closed first.
        let _ = self
            .current
            .finish(|recorder| recorder.finish_recording_as_picture(None));

        if let Some(mut run) = self.run.take() {
            let _ = run
                .recorders
                .finish(|recorder| recorder.finish_recording_as_picture(None));
        }
        // Begun at the old bounds.
        self.spare = None;
        self.cut_spare = None;

        self.bounds = bounds;
        self.current.begin(bounds);
        self.current.each(|canvas, _| {
            canvas.save(); // start at depth 2
        });
        // Replaced rather than cleared, so that a page taken before keeps
        // what it was given.
        self.layers = Arc::default();
        self.features = Arc::default();
        self.changed = false;
        self.isolated = None;
        self.state_dirty = false;
        self.matrix = Matrix::default();
        self.clip = None;
        self.surface = RecordingSurface::default();
        // The layers open on the erased content go with it, and so do the
        // recordings they were opened inside.
        self.open.clear();
    }

    pub fn update_bounds(&mut self, bounds: Rect) {
        self.bounds = bounds; // non-destructively update the size
        // A spare recording was begun at the old bounds.
        self.spare = None;
        self.cut_spare = None;
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
        self.frame = self.frame.wrapping_add(1);
    }

    /// Notes that the recording canvas was begun again, in the frame it
    /// recorded, and has to be framed before the next draw -- without
    /// starting a frame, so that a run of it goes on. See [`Run`].
    fn reframe(&mut self) {
        self.state_dirty = true;
    }

    fn rebuild_frame(&mut self) {
        if self.run.as_ref().is_some_and(|run| run.frame != self.frame) {
            self.close_run();
        }
        #[cfg(test)]
        let keeps_runs = KEEPS_RUNS.get();
        #[cfg(not(test))]
        let keeps_runs = true;
        // An isolated layer is recorded in the page's recording as it opens
        // and rasterized whole, so it is never a run.
        let run = self
            .clip
            .as_ref()
            .filter(|_| keeps_runs && self.isolated.is_none());
        let (matrix, clip) = (self.matrix, &self.clip);
        self.current.each(|canvas, _| {
            canvas.restore_to_count(1);
            canvas.save();
            if run.is_some() {
                return;
            }
            if let Some(clip) = clip {
                canvas.clip_path(
                    clip,
                    ClipOp::Intersect,
                    true, /* antialias */
                );
            }
            canvas.set_matrix(&matrix.into());
        });
        if let Some(clip) = run
            && self.run.is_none()
        {
            let bbh = !self.open.is_empty();
            let dual = self.current.dual();
            let bounds = self.bounds;
            let spare = self
                .spare
                .take()
                .filter(|spare| spare.bbh == bbh)
                .unwrap_or_else(|| Spare {
                    recorders: Recorders::new_with(bounds, dual, bbh),
                    idle: 0,
                    bbh,
                });
            self.run = Some(Run::open(self.frame, clip, matrix, spare));
        }
    }

    /// Files `entry` where the recording stands: among the page's layers,
    /// asking `features` of a vector backend for it, or among the contents
    /// of the innermost open `saveLayer`, which asks for them in turn.
    fn file_entry(&mut self, entry: Layer, features: VectorFeatures) {
        match self.open.last_mut() {
            Some(layer) => {
                layer.entries.push(entry);
                layer.features = layer.features.with(features);
            }
            None => {
                Arc::make_mut(&mut self.layers).push(entry);
                Arc::make_mut(&mut self.features).push(features);
            }
        }
    }

    /// Ends the frame's run: drawn into the page's recording under its clip
    /// where nothing split it, as the page would have recorded it; filed as
    /// its last piece where something did -- see [`Run`].
    fn close_run(&mut self) {
        let Some(mut run) = self.run.take() else {
            return;
        };
        let entry = run.finish(self.bounds);
        let idle = match entry {
            Some(_) => 0,
            None => run.idle + 1,
        };
        let shape = run.split.then(|| run.shape());
        let bbh = run.bbh;
        let mut recorders = run.recorders;
        let idle = match idle > Spare::IDLE_FRAMES {
            true => {
                let _ = recorders.finish(|recorder| {
                    recorder.finish_recording_as_picture(None)
                });
                recorders.begin_with(self.bounds, bbh);
                0
            }
            false => idle,
        };
        self.spare = Some(Spare {
            recorders,
            idle,
            bbh,
        });
        let Some(entry) = entry else {
            return;
        };
        if let Some(shape) = shape {
            // After what the page recorded before the run, which the split
            // filed and nothing has recorded into since.
            self.file_current();
            let piece = Clipped::new(run.id, &shape, entry);
            self.file_entry(
                Layer::Clipped(Arc::new(piece)),
                VectorFeatures::PLAIN,
            );
            return;
        }
        let clip = &run.clip;
        self.current.each(|canvas, recording| {
            let picture = match (&entry, recording) {
                (Layer::Picture(picture), _) => picture,
                (Layer::Targeted(pair), Recording::Document) => &pair.1,
                (Layer::Targeted(pair), _) => &pair.0,
                _ => return,
            };
            canvas.save();
            canvas.clip_path(
                clip,
                ClipOp::Intersect,
                true, /* antialias */
            );
            canvas.draw_picture(picture, None, None);
            canvas.restore();
        });
        self.changed = true;
    }

    /// Files what the frame's run has recorded as a piece, after what the
    /// page's recording holds, and goes on recording the run: for a read or
    /// an export, which has to see what was drawn, and a draw the clip cuts,
    /// which is filed as a piece of its own. A run whose frame has ended is
    /// closed instead -- see [`Self::close_run`].
    fn split_run(&mut self) {
        let Some(run) = &self.run else {
            return;
        };
        if run.frame != self.frame {
            self.close_run();
            return;
        }
        // What the page recorded before the run comes first.
        self.file_current();
        let bounds = self.bounds;
        let Some(run) = &mut self.run else {
            return;
        };
        run.split = true;
        let Some(entry) = run.take(bounds) else {
            return;
        };
        let piece = Clipped::new(run.id, &run.shape(), entry);
        self.file_entry(Layer::Clipped(Arc::new(piece)), VectorFeatures::PLAIN);
    }

    /// Opens a `saveLayer` with `rec`, under the transform and clip the page
    /// holds now.
    ///
    /// Nothing of the layer is drawn yet. What is drawn until
    /// [`Self::close_layer`] is filed as the layer's contents, and the layer
    /// is drawn around them once it closes -- see [`OpenLayer`]. What was
    /// recorded before it is filed first, where the layer is opened, so that
    /// a read taken while it is open finds the page beneath it already
    /// filed.
    pub fn open_layer(&mut self, rec: LayerRec) {
        static LAYERS: AtomicU64 = AtomicU64::new(0);
        self.close_run();
        self.file_current();
        self.open.push(OpenLayer {
            id: LAYERS.fetch_add(1, Ordering::Relaxed),
            entries: Vec::new(),
            rec,
            matrix: self.matrix,
            clip: self.clip.clone(),
            features: VectorFeatures::PLAIN,
        });
        self.restore();
    }

    /// Closes the innermost layer [`Self::open_layer`] opened, filing it as
    /// a [`Group`] where it was opened: on the page, or among the contents
    /// of the layer around it.
    ///
    /// The group asks a vector backend for [`VectorFeatures::LAYER`] and
    /// whatever its contents asked, so that an SVG export rasterizes the
    /// layer and leaves the rest of the page vector. One holding nothing
    /// that would change nothing is dropped.
    pub fn close_layer(&mut self) {
        if self.open.is_empty() {
            return;
        }
        self.close_run();
        self.file_current();
        let Some(layer) = self.open.pop() else {
            return;
        };
        self.surface.forget_layers_past(self.open.len());
        if !layer.entries.is_empty() || layer.rec.marks_when_empty() {
            let group = Group {
                composite: Composite::Layer(layer.rec),
                matrix: layer.matrix,
                clip: layer.clip.as_ref().map(Path::serialize),
                within: None,
                contents_matrix: Matrix::new_identity(),
                contents: Arc::new(layer.entries),
            };
            self.file_group(group, layer.features.with(VectorFeatures::LAYER));
        }
        self.restore();
    }

    /// Files `contents` as a [`Group`] composited by `composite`, under the
    /// transform and clip the recorder holds and inside `within` under that
    /// transform, its entries played under `contents_matrix`; asking
    /// `features` of a vector backend for it. Filed where the recording
    /// stands: on the page, or among the contents of the innermost open
    /// layer.
    ///
    /// `shift` moves the group in device space, both transforms with it, as
    /// a shadow is offset from what casts it.
    pub fn append_group(
        &mut self,
        composite: Composite,
        within: Option<Rect>,
        contents_matrix: Matrix,
        contents: Arc<Vec<Layer>>,
        features: VectorFeatures,
        shift: Point,
    ) {
        self.close_run();
        self.file_current();
        let shift = Matrix::translate(shift);
        let group = Group {
            composite,
            matrix: Matrix::concat(&shift, &self.matrix),
            clip: self.clip.as_ref().map(Path::serialize),
            within,
            contents_matrix: Matrix::concat(&shift, &contents_matrix),
            contents,
        };
        self.file_group(group, features);
        self.restore();
    }

    /// Files `group` after what [`Self::file_current`] has filed.
    fn file_group(&mut self, group: Group, features: VectorFeatures) {
        self.file_entry(Layer::Group(Arc::new(group)), features);
    }

    /// Files what `current` has recorded: as a layer of the page where no
    /// `saveLayer` is open -- see [`Self::flush`] -- and otherwise among the
    /// contents of the innermost one.
    fn file_current(&mut self) {
        let Some(layer) = self.open.last_mut() else {
            self.flush();
            return;
        };
        if !self.changed {
            return;
        }
        if let Some(entry) = self
            .current
            .finish(|recorder| recorder.finish_recording_as_picture(None))
        {
            layer.entries.push(entry);
        }
        self.current.begin(self.bounds);
        self.changed = false;
        self.reframe();
    }

    /// The layers open on the page as they stand, as one [`Group`] over the
    /// filed page: what a read taken while they are open sees. `None` where
    /// none is open.
    ///
    /// What the innermost layer has recorded is filed among its entries, so
    /// the layers stay open for what is drawn next and are filed once, whole,
    /// when they close. What this replays is the open layers' own contents
    /// and nothing of the page beneath them.
    ///
    /// Filed, rather than finished and begun again holding what it had:
    /// that nested the recording one picture deeper per read, and playing it
    /// back recursed as deep -- thousands of reads inside a layer overran
    /// the stack.
    fn open_layers_entry(&mut self) -> Option<Layer> {
        if self.open.is_empty() {
            return None;
        }
        self.split_run();
        self.file_current();
        self.reframe();
        self.open.iter().rev().fold(None, |inner, layer| {
            let mut contents = layer.entries.clone();
            contents.extend(inner);
            Some(Layer::Group(Arc::new(Group {
                composite: Composite::Layer(layer.rec.clone()),
                matrix: layer.matrix,
                clip: layer.clip.as_ref().map(Path::serialize),
                within: None,
                contents_matrix: Matrix::new_identity(),
                contents: Arc::new(contents),
            })))
        })
    }

    /// What the open layers ask of a vector backend, as the page they are
    /// drawn into by [`Self::open_layers_entry`] has to.
    fn open_layer_features(&self) -> VectorFeatures {
        self.open
            .iter()
            .fold(VectorFeatures::LAYER, |all, layer| all.with(layer.features))
    }

    pub fn pixels(
        &mut self,
        crop: IRect,
        opts: ExportOptions,
        engine: RenderingEngine,
    ) -> Result<Vec<u8>, String> {
        let alpha_type = opts.alpha_type;
        self.pixels_as(crop, opts, engine, alpha_type)
    }

    /// As [`PageRecorder::pixels`], with the destination alpha mode
    /// given directly rather than read off the options.
    ///
    /// Both surfaces can ask. `getImageData` is unpremultiplied in the Canvas
    /// standard and stays so by default, but this library already answers it
    /// outside the standard -- F32 where the standard allows eight bits -- so
    /// the alpha mode is now a setting a JavaScript caller can make too,
    /// through `alphaType` on the settings object `getImageData` takes. Skia
    /// converts during readback either way.
    ///
    /// The way back in reads the mode off the `ImageData` itself, which
    /// records it as `alphaType`, so a premultiplied readback put back
    /// through `putImageData` is not taken for a darker colour.
    ///
    /// This form exists for the Rust API, whose `PixelExportOptions` carries
    /// the flag on a different type; the Node path sets
    /// [`ExportOptions::alpha_type`] and calls
    /// [`pixels`](PageRecorder::pixels).
    pub fn pixels_as(
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

        // `crop` is in device pixels and `bounds` is in canvas units, so the
        // two describe the same rectangle only at density 1. Compared
        // directly, a read at density 2 starting at canvas x=10 on a
        // 20-wide canvas has a crop beginning at device 20, which misses
        // unscaled bounds of 0 to 20 and takes this early return -- so the
        // ink it asked for came back as zeroes, while any read starting
        // further left returned those same pixels correctly.
        let device_bounds = Rect::new(
            self.bounds.left * opts.density,
            self.bounds.top * opts.density,
            self.bounds.right * opts.density,
            self.bounds.bottom * opts.density,
        );
        if !device_bounds.intersects(Rect::from_irect(crop)) {
            return Ok(dst_buffer);
        }

        if !self.open.is_empty() {
            return self.pixels_in_layer(crop, &opts, &engine, &dst_info);
        }

        let page = self.page();
        let page_size = page.scaled_dimensions(opts.density);

        // Small reads are served by the squares they touch, which is the
        // whole point: a hit test asked for sixty-four pixels and used to
        // composite and keep a megapixel. Anything wider than the grid is
        // worth is served by the page, as before.
        if self.surface.try_tiled(&page, &opts, &engine, crop)? {
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

        self.surface.confirmed_update(&page, &opts, &engine)?;

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

    /// [`Self::pixels_as`] while a `saveLayer` is open: the filed page from
    /// the surface, as any read takes it, with the open layers drawn over a
    /// copy of it for this read alone -- see [`Self::open_layers_entry`] and
    /// [`RecordingSurface::read_overlaid`]. The surface and the caches are
    /// only ever given the filed page, so nothing keeps a layer as it stood
    /// part way. What the read replays beyond an ordinary one is the open
    /// layers' contents, and a copy of the page on its own engine.
    fn pixels_in_layer(
        &mut self,
        crop: IRect,
        opts: &ExportOptions,
        engine: &RenderingEngine,
        dst_info: &ImageInfo,
    ) -> Result<Vec<u8>, String> {
        let filed = self.filed_page();
        self.split_run();
        self.file_current();
        self.reframe();
        self.surface.confirmed_update(&filed, opts, engine)?;
        let mut pixels = vec![0; checked_byte_size(dst_info)?];
        let origin = (crop.x(), crop.y());
        let rastered =
            self.open.iter().all(|layer| layer.rasters(opts.density))
                && self.surface.read_rastered(
                    &self.open,
                    opts.density,
                    (dst_info, origin),
                    &mut pixels,
                )?;
        if rastered {
            return Ok(pixels);
        }
        let whole = self.open_layers_reach_past_a_read();
        let drawn = self.open_layers_entry();
        let read = Read {
            info: dst_info,
            origin,
            whole,
        };
        self.surface.read_overlaid(
            drawn.as_ref(),
            opts.density,
            filed.id,
            read,
            &mut pixels,
        )?;
        Ok(pixels)
    }

    /// Whether what the open layers draw at a pixel can depend on the page
    /// outside a read around it, which only a backdrop does: it filters what
    /// lies beneath the layer, and composed on a surface the size of the read
    /// it would find nothing past the read's edge. A filter on what a layer
    /// draws needs no such page -- Skia gives a filtered draw the input it
    /// reaches for, measured identical at a read's edge for a blur and a
    /// shadow. Where none has one, a read inside a layer composes only the
    /// region it reads -- see [`RecordingSurface::read_overlaid`].
    fn open_layers_reach_past_a_read(&self) -> bool {
        self.open.iter().any(|layer| layer.rec.backdrop.is_some())
    }

    /// Closes the open recording and files it as a layer.
    ///
    /// A no-op when nothing has been drawn since the last one, so calling it
    /// to open a segment boundary cannot leave an empty layer behind.
    fn flush(&mut self) {
        // A page taken now may hold the last op; extending it afterwards
        // would change a layer a cache has already drawn.
        self.blur_run = None;
        self.cut_run = None;
        if !self.changed {
            return;
        }

        // store layer as a drawable (so copies are deduplicated) wrapped in
        // a picture (so it can be sent to other threads)
        let bounds = self.bounds;
        if let Some(entry) = self.current.finish(|recorder| {
            recorder
                .finish_recording_as_drawable()
                .and_then(|mut drawable| {
                    let mut wrapper = PictureRecorder::new();
                    wrapper
                        .begin_recording(bounds, true)
                        .draw_drawable(&mut drawable, None);
                    wrapper.finish_recording_as_picture(None)
                })
        }) {
            Arc::make_mut(&mut self.layers).push(entry);
            Arc::make_mut(&mut self.features)
                .push(self.isolated.unwrap_or(VectorFeatures::PLAIN));
        }
        self.isolated = None;

        // resume recording
        self.current.begin(self.bounds);
        self.changed = false;
        self.reframe();
    }

    /// Records one draw into a layer of its own, marked for rasterization.
    ///
    /// For the draws Skia's SVG backend would mangle, which
    /// [`VectorFeatures::SVG_CANNOT`] names one by one.
    /// Keeping them in their own layer is what lets an SVG export replace
    /// exactly those and leave every other draw as vectors; a raster patch
    /// painted over finished vector output would double-composite anything
    /// translucent underneath it.
    ///
    /// A draw inside an open `saveLayer` is recorded with the layer's
    /// contents, and the layer asks the backend for its features when it is
    /// filed -- see [`Self::close_layer`].
    pub fn append_isolated<F>(&mut self, features: VectorFeatures, f: F)
    where
        F: Fn(&SkCanvas, Recording),
    {
        if let Some(layer) = self.open.last_mut() {
            layer.features = layer.features.with(features);
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
            self.close_run();
            self.flush();
            // The frame this layer records under, applied once as it opens.
            // `rebuild_frame` is the same `restore_to_count`, `save`, clip
            // and matrix this used to spell out for itself, and records the
            // clip into the layer rather than opening a run for it.
            self.isolated = Some(features);
            self.state_dirty = false;
            self.rebuild_frame();
        }

        self.current.each(f);
        self.changed = true;
    }

    /// Records `draw` as a [`BlurOp`], so that its blur is made where the
    /// page is played back, under `matrix` and `clip` -- the frame the
    /// recorder holds -- and filed as [`Self::file_current`] files: among
    /// the page's layers, or the contents of the innermost open `saveLayer`,
    /// which asks for its features. Under a clip it is a piece of the
    /// frame's run, the op holding no clip of its own -- see [`Run`] -- so
    /// that a document writes the clip once around it and the draws beside
    /// it.
    ///
    /// A draw straight after a `BlurOp` it can join -- nothing recorded
    /// between them, the frame and the layer it is filed in unchanged since,
    /// and on the page the same `features` -- joins it rather than opening
    /// an op of its own: see [`BlurOp::extend`].
    pub fn append_blur(
        &mut self,
        draw: BlurDraw,
        matrix: Matrix,
        clip: Option<&Path>,
        features: VectorFeatures,
    ) {
        if self.isolated.is_some() {
            self.flush();
        }
        self.settle();
        let mut draw = draw;
        // An op a page snapshot also holds is not grown, so that the
        // snapshot keeps what it was given: the draw opens an op of its own
        // instead. `flush` seals a run before any page is taken, so this is
        // a guard rather than a path a run takes.
        if self.blur_run == Some(self.frame) && !self.pending() {
            let run = self.run.as_ref().map(|run| run.id);
            let (entries, last_features) = match self.open.last_mut() {
                Some(layer) => (&mut layer.entries, None),
                None => (Arc::make_mut(&mut self.layers), self.features.last()),
            };
            let op = match entries.last_mut() {
                Some(Layer::Blur(op)) if run.is_none() => Some(op),
                Some(Layer::Clipped(piece)) if Some(piece.run) == run => {
                    Arc::get_mut(piece).and_then(|piece| {
                        match &mut piece.entry {
                            Layer::Blur(op) => Some(op),
                            _ => None,
                        }
                    })
                }
                _ => None,
            };
            if let Some(op) = op
                && last_features.is_none_or(|last| *last == features)
                && let Some(op) = Arc::get_mut(op)
            {
                match op.extend(draw) {
                    Ok(()) => {
                        if let Some(layer) = self.open.last_mut() {
                            layer.features = layer.features.with(features);
                        }
                        return;
                    }
                    Err(back) => draw = back,
                }
            }
        }
        // Filed after what is recorded, which filing closes no layer to do.
        let entry = match self.run.as_mut() {
            Some(run) => {
                let (id, shape) = (run.id, run.shape());
                self.split_run();
                let op =
                    Layer::Blur(Arc::new(BlurOp::start(draw, matrix, None)));
                Layer::Clipped(Arc::new(Clipped::new(id, &shape, op)))
            }
            None => {
                self.file_current();
                Layer::Blur(Arc::new(BlurOp::start(draw, matrix, clip)))
            }
        };
        self.file_entry(entry, features);
        self.blur_run = Some(self.frame);
    }

    /// Records the draw `draw` makes, under `matrix`, as a [`CutOp`]: a fill
    /// or stroke `clip`, the frame's clip, cuts, so that a raster playback
    /// on the CPU applies the clip as coverage and draws the path whole.
    ///
    /// The op is a piece of the frame's run -- see [`Run`] -- so that a
    /// document plays it under the run's clip with the draws around it, and
    /// writes the clip once. A draw straight after a `CutOp` it can join --
    /// nothing recorded between them, the frame and the layer it is filed in
    /// unchanged since, and on the page the same `features` -- joins it
    /// rather than opening an op of its own.
    ///
    /// Inside an open `saveLayer` the op is filed among the layer's entries,
    /// as a blur is, and played inside the layer when it closes.
    pub fn append_cut<F>(
        &mut self,
        clip: &Path,
        matrix: Matrix,
        features: VectorFeatures,
        draw: F,
    ) where
        F: Fn(&SkCanvas, Recording),
    {
        if self.isolated.is_some() {
            self.flush();
        }
        self.settle();
        let Some(drawn) = self.record_cut(&matrix, draw) else {
            return;
        };
        // An op a page snapshot also holds is not grown, as with a run of
        // blurs in `append_blur`.
        if self.cut_run == Some(self.frame) && !self.pending() {
            let run = self.run.as_ref().map(|run| run.id);
            let (entries, last_features) = match self.open.last_mut() {
                Some(layer) => (&mut layer.entries, None),
                None => (Arc::make_mut(&mut self.layers), self.features.last()),
            };
            let op = match entries.last_mut() {
                Some(Layer::Cut(op)) if run.is_none() => Some(op),
                Some(Layer::Clipped(piece)) if Some(piece.run) == run => {
                    Arc::get_mut(piece).and_then(|piece| {
                        match &mut piece.entry {
                            Layer::Cut(op) => Some(op),
                            _ => None,
                        }
                    })
                }
                _ => None,
            };
            if let Some(op) = op
                && last_features.is_none_or(|last| *last == features)
                && let Some(op) = Arc::get_mut(op)
            {
                op.extend(drawn);
                if let Some(layer) = self.open.last_mut() {
                    layer.features = layer.features.with(features);
                }
                return;
            }
        }
        // One serialized clip and one coverage picture for the run, shared
        // by each of its cut draws.
        let (shape, coverage) = match self.run.as_mut() {
            Some(run) => (run.shape(), run.coverage()),
            None => (ClipData::of(clip), Coverage::of(clip).map(Arc::new)),
        };
        let Some(coverage) = coverage else {
            return;
        };
        let Some(op) = CutOp::new(&shape, coverage, drawn) else {
            return;
        };
        let op = Layer::Cut(Arc::new(op));
        // Filed after what is recorded, which filing closes no layer to do.
        let entry = match &self.run {
            Some(run) => {
                let id = run.id;
                self.split_run();
                Layer::Clipped(Arc::new(Clipped::new(id, &shape, op)))
            }
            None => {
                self.file_current();
                op
            }
        };
        self.file_entry(entry, features);
        self.cut_run = Some(self.frame);
    }

    /// The draw `draw` makes under `matrix`, recorded on its own for a
    /// [`CutOp`] -- into a recording for each kind of target the page keeps
    /// -- reusing the recorders the last cut draw finished with, as a run
    /// reuses its own -- see [`Spare`]. Inside an open `saveLayer` with a
    /// bounding hierarchy, which the layer is confined by -- see
    /// [`Run::bbh`] -- and on the page without one.
    fn record_cut<F>(&mut self, matrix: &Matrix, draw: F) -> Option<Layer>
    where
        F: Fn(&SkCanvas, Recording),
    {
        let bbh = !self.open.is_empty();
        let (bounds, dual) = (self.bounds, self.dual());
        let mut recorders = self
            .cut_spare
            .take()
            .filter(|spare| spare.bbh == bbh)
            .map(|spare| spare.recorders)
            .unwrap_or_else(|| Recorders::new_with(bounds, dual, bbh));
        recorders.each(|canvas, recording| {
            canvas.restore_to_count(1);
            canvas.save();
            canvas.set_matrix(&(*matrix).into());
            draw(canvas, recording);
        });
        let drawn = recorders
            .finish(|recorder| recorder.finish_recording_as_picture(None));
        recorders.begin_with(bounds, bbh);
        recorders.each(|canvas, _| {
            canvas.save(); // start at depth 2
        });
        self.cut_spare = Some(Spare {
            recorders,
            idle: 0,
            bbh,
        });
        drawn
    }

    /// What one replay of this page costs; see the field.
    pub fn replay_cost(&self) -> usize {
        self.replay_cost
    }

    /// Charges this page for replaying a picture that costs `cost` of its own.
    ///
    /// Called instead of counting the draw as one, because a nested picture is
    /// replayed whole every time the page around it is.
    pub fn charge_replay(&mut self, cost: usize) {
        self.replay_cost = self.replay_cost.saturating_add(cost.max(1));
    }

    /// The page as recorded so far: a snapshot that a later draw does not
    /// change.
    ///
    /// It shares the recorder's lists rather than copying them, so taking
    /// one costs the same at ten layers as at ten thousand -- every read of
    /// the canvas takes one. The next draw copies the lists only if a page
    /// taken before is still held then, as a window's renderer holds the
    /// frame it is drawing; a read that has finished holds nothing.
    ///
    /// While a `saveLayer` is open the page also holds the open layers as
    /// they stand -- see [`Self::open_layers_entry`] -- under an identity of
    /// its own that nothing keeps, so that no cache files a layer that is
    /// still being drawn as though it were finished. It starts from the
    /// filed page's cached bitmap where there is one, so that compositing it
    /// replays the open layers and nothing filed -- see [`Page::cache_key`].
    pub fn page(&mut self) -> Page {
        let filed = self.filed_page();
        let Some(drawn) = self.open_layers_entry() else {
            return filed;
        };
        let mut layers = Vec::clone(&filed.layers);
        layers.push(drawn);
        let mut features = Vec::clone(&filed.features);
        features.push(self.open_layer_features());
        let id = Self::mint_id();
        Page {
            layers: Arc::new(layers),
            features: Arc::new(features),
            bounds: self.bounds,
            id: id.0,
            starts_from: Some(FiledPrefix {
                id: filed.id,
                depth: filed.depth(),
                owner: filed.owner.clone(),
            }),
            owner: Arc::downgrade(&id),
        }
    }

    /// The page as filed: everything but the layers still open. The
    /// recording in progress, and what the frame's run holds as a piece of
    /// it, are filed first where no layer is open; while one is, everything
    /// before the outermost was filed when it opened.
    fn filed_page(&mut self) -> Page {
        if self.open.is_empty() {
            self.settle();
            self.split_run();
            self.flush();
        }
        Page {
            layers: Arc::clone(&self.layers),
            features: Arc::clone(&self.features),
            bounds: self.bounds,
            id: self.id.0,
            starts_from: None,
            owner: Arc::downgrade(&self.id),
        }
    }

    pub fn page_for_export(
        &mut self,
        opts: &ExportOptions,
        engine: &RenderingEngine,
    ) -> Page {
        // update the PageCache with the surface bitmap (if it's valid for this
        // export). The surface only ever holds the filed page, which is what a
        // page taken inside a layer starts from.
        let filed = self.filed_page();
        if opts.is_raster()
            && let Some(image) =
                self.surface.snapshot_if_valid(&filed, opts, engine)
        {
            PageCache::set(&filed.owner, image, opts, self.surface.depth);
        }
        self.page()
    }

    pub fn image(
        &mut self,
        color_type: ColorType,
        space: &ColorSpace,
    ) -> Option<SkImage> {
        self.image_flattened(false, color_type, space)
    }

    /// This page as an image, optionally rasterized on the spot.
    ///
    /// The ordinary answer is deferred: an image backed by the page's
    /// picture, which costs nothing to make and is replayed wherever it is
    /// drawn. That is what makes a canvas cheap to use as a source, and it is
    /// also what compounds -- a deferred image drawn into a second canvas
    /// carries this page's whole picture with it, so copying a page into a
    /// fresh canvas and drawing it back doubles the work of the eventual
    /// rasterization each round while the recording grows by a constant.
    ///
    /// `flatten` pays for the pixels now instead. One replay, and what comes
    /// back is a bitmap that costs the same wherever it is drawn however many
    /// times it is copied again. `node::image::Source::of` asks for it on
    /// any page that already carries a nested picture of its own.
    ///
    /// The image carries the canvas's own depth and color space. It is what
    /// the picture is replayed into when it is finally drawn, so its format
    /// bounds what the source can hand the destination: an eight-bit sRGB
    /// image clips a `display-p3` canvas to the smaller gamut and puts an
    /// `RGBAF32` canvas on the 1/255 grid, whatever either canvas is made
    /// with. `ExportOptions::compositing_color_type` bounds the same two
    /// properties for the surface a page draws into.
    pub fn image_flattened(
        &mut self,
        flatten: bool,
        color_type: ColorType,
        space: &ColorSpace,
    ) -> Option<SkImage> {
        let size = self.bounds.size().to_floor();
        let deferred = self.page().picture(None).and_then(|pict| {
            started::deferred_from_picture(
                pict,
                size,
                None,
                None,
                source_bit_depth(color_type),
                Some(space.clone()),
                None,
            )
        })?;
        match flatten {
            // `Allow`, so a source drawn more than once pays for its pixels
            // once. The fallback hands back the deferred image, which draws
            // the same pixels and goes on carrying its picture -- slow rather
            // than wrong, and unreachable: `make_raster_image` on a picture
            // image allocates its own raster canvas and replays into it,
            // whatever the size. Asserted so that stops being folklore.
            true => Some(
                deferred
                    .clone()
                    .make_raster_image(None, CachingHint::Allow)
                    .unwrap_or_else(|| {
                        debug_assert!(
                            false,
                            "a picture-backed page must rasterize"
                        );
                        deferred
                    }),
            ),
            false => Some(deferred),
        }
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
    /// Layers already played into this tile, or `None` before it has been
    /// painted at all.
    ///
    /// `None` rather than zero, because zero is a real depth: a page that has
    /// been cleared and not drawn on since has no layers, and a tile holding
    /// that page is finished rather than unpainted. Spelling both as zero
    /// meant the skip below had to exclude it -- `depth == target && target >
    /// 0` -- so an empty page re-cleared and re-read its tile on every call,
    /// which no cache above it could survive. A 64x64 read of a cleared
    /// 600x600 page measured 234 microseconds that way and 6.5 once the tile
    /// could tell "no layers yet" from "no layers".
    depth: Option<usize>,
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
    served_at: Option<Option<usize>>,
    /// Whether a read has confirmed the GPU ran every draw this tile holds
    /// -- see [`crate::gpu::completion`]. Unset when the tile is made and
    /// whenever a layer is played into it, so only a read of a tile nothing
    /// has been drawn into since its last confirmation goes without one.
    confirmed: bool,
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
            return read_converted_on_cpu(
                &mut self.surface,
                info,
                pixels,
                row_bytes,
                at,
            );
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

/// One 8-bit channel unpremultiplied, indexed by alpha and then by the
/// premultiplied channel, rounded exactly as Skia's raster pipeline rounds it:
/// both normalised by multiplying by 1/255, the channel multiplied by the
/// alpha's reciprocal, held at one, and scaled back with ties to even.
/// `unpremultiplying_matches_the_raster_pipeline` holds it to that pipeline
/// for all 65,536 pairs. Alpha 0 gives 0, as the pipeline does.
fn unpremultiply_table() -> &'static [[u8; 256]; 256] {
    static TABLE: OnceLock<Box<[[u8; 256]; 256]>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let scale = 1.0 / f32::from(u8::MAX);
        let mut table = Box::new([[0u8; 256]; 256]);
        for (alpha, row) in table.iter_mut().enumerate().skip(1) {
            let reciprocal = 1.0 / (alpha as f32 * scale);
            for (channel, out) in row.iter_mut().enumerate() {
                let value = (channel as f32 * scale * reciprocal).min(1.0);
                *out = (value * f32::from(u8::MAX)).round_ties_even() as u8;
            }
        }
        table
    })
}

/// The read, in pixels, from which [`read_converted_on_cpu`] unpremultiplies
/// its rows across threads rather than on the calling one.
///
/// Measured in release on Apple silicon, first GPU reads of a 1200x900 page,
/// medians of three rounds: handing rows to the pool costs about 100
/// microseconds a read -- a 16x16 read takes 135 to 156 on one thread and
/// 237 to 265 across them -- and the threads earn it back only on the
/// largest reads. At 600x600, 360,000 pixels, the two are level, 682 to 782
/// against 746 to 777; at 800x900, 720,000, threads take 937 to 988 against
/// 1109 to 1138, and a full read 1270 to 1317 against 1617 to 1638. So the
/// line is drawn between those two sizes, at 2^19.
const PARALLEL_UNPREMULTIPLY_PIXELS: usize = 1 << 19;

/// Unpremultiplies one row of eight-bit pixels in place through
/// [`unpremultiply_table`], alpha fourth.
fn unpremultiply_row(row: &mut [u8]) {
    let table = unpremultiply_table();
    for pixel in row.as_chunks_mut::<4>().0 {
        // Opaque needs nothing, and so does clear: a premultiplied pixel
        // with no alpha has no colour either.
        let alpha = pixel[3];
        if alpha == u8::MAX || alpha == 0 {
            continue;
        }
        let divide = &table[usize::from(alpha)];
        for channel in &mut pixel[..3] {
            *channel = divide[usize::from(*channel)];
        }
    }
}

/// Reads `surface` into `pixels` as `info` describes, with any conversion
/// made on the CPU.
///
/// On a raster surface that is `read_pixels` itself. On a GPU surface the
/// pixels come back as the surface holds them, which converts nothing, and
/// are converted here. Do not hand the conversion back to the GPU: its
/// readback unpremultiplies by rounding down, so a premultiplied 64 at an
/// alpha of 128 comes back 127 where the raster path and Chrome 153 give
/// 128, and a repeated read of the same state, served from a raster copy,
/// gives 128 -- the answer would depend on how many times it was asked.
///
/// A read that differs from the surface only in wanting unpremultiplied
/// pixels, which is what `getImageData` asks of an eight-bit canvas, reads
/// premultiplied pixels into the destination and unpremultiplies them there
/// through [`unpremultiply_table`]; anything else goes through Skia's CPU
/// conversion from a copy. The cost is the GPU's: on Apple silicon a
/// premultiplied read of a 1200x900 page costs about 0.95 milliseconds, where
/// the GPU's own unpremultiplying read costs 0.78 and rounds down, and the
/// table adds about 0.55 nanoseconds a pixel on one thread -- shared across
/// threads from [`PARALLEL_UNPREMULTIPLY_PIXELS`], which brings the whole
/// read to about 1.3 milliseconds. An 8x8 read costs about 130 microseconds
/// against that conversion's 200.
fn read_converted_on_cpu(
    surface: &mut Surface,
    info: &ImageInfo,
    pixels: &mut [u8],
    row_bytes: usize,
    at: (i32, i32),
) -> bool {
    if surface.recording_context().is_none() {
        return surface.read_pixels(info, pixels, row_bytes, at);
    }
    let native = surface.image_info().with_dimensions(info.dimensions());
    if &native == info {
        return surface.read_pixels(info, pixels, row_bytes, at);
    }
    let eight_bit = |kind: ColorType| {
        matches!(kind, ColorType::RGBA8888 | ColorType::BGRA8888)
    };
    let only_unpremultiplied = eight_bit(native.color_type())
        && eight_bit(info.color_type())
        && native.color_space() == info.color_space()
        && native.alpha_type() == AlphaType::Premul
        && info.alpha_type() == AlphaType::Unpremul;
    if only_unpremultiplied {
        // Premultiplied straight into the destination, in its own byte
        // order, and unpremultiplied where it lies: one pass, no copy.
        let premultiplied = info.with_alpha_type(AlphaType::Premul);
        if !surface.read_pixels(&premultiplied, pixels, row_bytes, at) {
            return false;
        }
        let width = info.min_row_bytes();
        let height = info.height() as usize;
        let area = info.width() as usize * height;
        match area >= PARALLEL_UNPREMULTIPLY_PIXELS {
            true => pixels
                .par_chunks_mut(row_bytes)
                .take(height)
                .for_each(|row| unpremultiply_row(&mut row[..width])),
            false => pixels
                .chunks_mut(row_bytes)
                .take(height)
                .for_each(|row| unpremultiply_row(&mut row[..width])),
        }
        return true;
    }
    let native_row_bytes = native.min_row_bytes();
    let mut held = vec![0u8; native.compute_byte_size(native_row_bytes)];
    surface.read_pixels(&native, &mut held, native_row_bytes, at)
        && Pixmap::new(&native, &mut held, native_row_bytes).is_some_and(
            |pixmap| pixmap.read_pixels(info, pixels, row_bytes, (0, 0)),
        )
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
///
/// Measured on 1200x900 drawings, its cost is 11 to 48% of the one encode that
/// follows it: 1.86 ms against 16.51 on a page of panels, 3.79 against 7.92 on
/// a gradient, 8.25 against 49.77 on a photograph. What it buys is the other
/// setting not being used -- that gradient encodes in 33.52 ms filtered
/// against 7.92 unfiltered, and 3.4 times larger.
///
/// Three cheaper-looking arrangements were measured and are all worse, which
/// is worth stating here because each looks obviously right until it is timed:
///
/// - **Encode both ways and keep the smaller.** Exact rather than sampled, and
///   4 to 9 times more expensive: the extra it pays is a whole encode in the
///   losing setting, 33.52 ms on that gradient against a 3.79 ms probe.
/// - **Probe with the encoder instead of `flate2`.** Skia links a SIMD zlib and
///   compresses about 4 times faster per byte, so this reads like free speed.
///   It is not: two encodes of the 96 sampled rows come to 7.9 ms on the
///   photograph against the 8.25 the probe already takes, because the probe
///   deflates one cheap row difference where the encoder tries five filters a
///   row.
/// - **Sample fewer rows.** Every band length from 8 to 48 gives the same
///   answer on all ten drawings the probe is scored against, so the corpus
///   would license cutting it sixfold. It is not evidence enough: the length
///   was set against a page of flat blocks that read the wrong way at two rows,
///   and nothing in that corpus reproduces it. The saving would be under 1% of
///   a 150-page export in any case.
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

/// A colour composited under a page as it is exported or read: unpremultiplied,
/// in the space it is tagged with -- the one a CSS string named, or
/// linear-light sRGB for an `RgbaLinear`.
///
/// Painted through a tagged paint rather than `Canvas::clear`, which takes an
/// eight-bit sRGB `Color` and so clipped a wide matte before a wide surface
/// saw it.
#[derive(Clone, Debug, PartialEq)]
pub struct Matte {
    color: Color4f,
    space: ColorSpace,
}

impl Matte {
    pub fn new(color: Color4f, space: ColorSpace) -> Self {
        Self { color, space }
    }

    /// A matte in `color`, linear-light sRGB as every `RgbaLinear` is.
    pub fn from_rgba_linear(color: RgbaLinear) -> Self {
        Self::new(
            rgba_linear_to_unpremul_color4f(color),
            linear_srgb_color_space(),
        )
    }

    /// Covers `canvas` with the matte, replacing what is there, in the colour
    /// the canvas's own space needs -- see `skia_paint_color_onto`.
    pub fn paint(&self, canvas: &SkCanvas) {
        let (color, space) = match canvas.image_info().color_space() {
            Some(destination) => {
                skia_paint_color_onto(self.color, &self.space, &destination)
            }
            None => skia_paint_color(self.color, &self.space),
        };
        let mut paint = Paint::default();
        paint
            .set_color4f(color, &space)
            .set_blend_mode(BlendMode::Src);
        canvas.draw_paint(&paint);
    }
}

/// Clears `canvas` to `matte`, or to transparent where there is none.
fn clear_to(canvas: &SkCanvas, matte: Option<&Matte>) {
    match matte {
        Some(matte) => matte.paint(canvas),
        None => {
            canvas.clear(Color::TRANSPARENT);
        }
    }
}

/// The pixels a read inside a layer asks for -- see
/// [`RecordingSurface::read_overlaid`].
pub struct Read<'a> {
    /// The layout and size of the pixels returned.
    pub info: &'a ImageInfo,
    /// Where on the page, in device pixels, the read's top-left corner lies.
    pub origin: (i32, i32),
    /// Whether the page is composed whole rather than at the read's size.
    pub whole: bool,
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
    matte: Option<Matte>,
    msaa: Option<usize>,
    gpu: Option<bool>,
    color_space: ColorSpace,
    density: f32,
    /// What the layers open on the page have drawn so far, outermost first,
    /// for each that a read has been taken inside -- see
    /// [`Self::read_rastered`]. Made like `surface` and dropped with it.
    layers: Vec<LayerRaster>,
}

/// Clips `canvas` as the layers in `open` clip what is drawn inside the
/// last of them: by each one's clip, outermost first, under the transform
/// the canvas carries.
fn clip_to(canvas: &SkCanvas, open: &[OpenLayer]) {
    for clip in open.iter().filter_map(|layer| layer.clip.as_ref()) {
        canvas.clip_path(clip, ClipOp::Intersect, true);
    }
}

/// What an open layer has drawn, rastered: see
/// [`RecordingSurface::read_rastered`].
struct LayerRaster {
    /// The [`OpenLayer`] it holds.
    id: u64,
    /// The size of the page's surface, and at its origin: the layer's
    /// entries are drawn at the very device pixels they land on when the
    /// layer closes onto the page, so an antialiased edge, a dither or a
    /// gradient far from the origin rounds as it does there.
    surface: Surface,
    /// How many of the layer's entries are drawn in it.
    drawn: usize,
}

/// The most a page's surface keeps of the layers open on it, in bytes, as
/// rasters of what each has drawn -- see
/// [`RecordingSurface::read_rastered`]. A raster is the page's surface again,
/// so three nested layers on an eight-bit 4000x4000 page keep 192 MB and fit,
/// where one on an F32 8000x8000 page would keep a gigabyte. Past it a read
/// inside a layer replays the layers instead: exact, and slower the more
/// they hold, rather than larger.
const LAYER_RASTER_BUDGET: usize = 256 << 20;

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
            layers: Vec::new(),
        }
    }
}

impl RecordingSurface {
    /// The bytes this holds in pixels: the page surface, its CPU copy and
    /// every tile.
    ///
    /// What the surfaces' own pixels take, which on the GPU leaves out a
    /// multisampled attachment Skia keeps beside the target and does not
    /// expose.
    pub fn resident_bytes(&self) -> usize {
        let surfaces = self
            .surface
            .iter()
            .chain(self.tiles.values().map(|tile| &tile.surface))
            .map(|surface| surface.image_info().compute_min_byte_size());
        let raster = self
            .raster
            .iter()
            .map(|image| image.image_info().compute_min_byte_size());
        surfaces.chain(raster).sum()
    }

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
                        depth: None,
                        used: TILE_USES.fetch_add(1, Ordering::Relaxed),
                        raster: None,
                        served_at: None,
                        confirmed: false,
                    },
                );
            }

            // SAFETY: inserted immediately above when absent.
            let tile = match self.tiles.get_mut(origin) {
                Some(tile) => tile,
                None => return false,
            };
            tile.used = TILE_USES.fetch_add(1, Ordering::Relaxed);
            if tile.depth == Some(target) {
                continue;
            }

            let canvas = tile.surface.canvas();
            let played = tile.depth.unwrap_or(0);
            if tile.depth.is_none() {
                clear_to(canvas, self.matte.as_ref());
            }
            canvas.save();
            // The recording is in page coordinates; a tile draws the same
            // commands shifted by its own origin and lets Skia cull the rest.
            canvas.translate((-origin.0 as f32, -origin.1 as f32));
            canvas.scale((self.density, self.density));
            for pict in page.layers.iter().skip(played) {
                pict.playback(canvas, Target::Pixels);
            }
            canvas.restore();
            tile.depth = Some(target);
            // Redrawn, so the copy describes a tile that is no longer there.
            // The page-wide copy is dropped on the same rule a few lines
            // below, and for the same reason.
            tile.raster = None;
            tile.served_at = None;
            tile.confirmed = false;
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
    ) -> Result<bool, String> {
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
            self.matte = opts.matte.clone();
            self.msaa = opts.msaa;
        }

        // A current page composite is the cheaper answer; leave it to serve.
        if self.surface.is_some() && self.depth == page.layers.len() {
            return Ok(false);
        }

        let page_size = page.scaled_dimensions(opts.density);
        let wanted = Self::tiles_covering(crop, page_size);
        if wanted.is_empty() || wanted.len() > TILES_PER_READ as usize {
            return Ok(false);
        }

        // The tiles share this thread's context, so one wait confirms every
        // one just drawn. A failed wait leaves them holding anything, so they
        // are dropped and drawn again from nothing. See
        // `crate::gpu::completion`.
        //
        // Tiles nothing has been drawn into since their last confirmation go
        // without one: the wait would flush nothing of theirs and answer for
        // a buffer that finished before it. A repeated 8x8 read of an
        // unchanged 1200x900 page, in a release build on Apple silicon,
        // measured 2.4 microseconds confirmed and 2.2 without.
        for _ in 0..DRAW_ATTEMPTS {
            if !self.refresh_tiles(&wanted, page, opts, engine) {
                return Ok(false);
            }
            let confirmed = wanted.iter().all(|origin| {
                self.tiles.get(origin).is_some_and(|tile| tile.confirmed)
            });
            let ran = confirmed
                || wanted
                    .first()
                    .and_then(|origin| self.tiles.get_mut(origin))
                    .is_none_or(|tile| draws_ran(&mut tile.surface, page.id));
            if ran {
                for origin in &wanted {
                    if let Some(tile) = self.tiles.get_mut(origin) {
                        tile.confirmed = true;
                    }
                }
                return Ok(true);
            }
            self.tiles.clear();
        }
        Err(discarded("drawing tiles for a read"))
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

    /// [`Self::update`], with the draws confirmed to have run on the GPU
    /// before a read depends on them: drawn again on a fresh surface if not,
    /// and an error if that fails too. See [`crate::gpu::completion`].
    pub fn confirmed_update(
        &mut self,
        page: &Page,
        opts: &ExportOptions,
        engine: &RenderingEngine,
    ) -> Result<(), String> {
        for _ in 0..DRAW_ATTEMPTS {
            self.update(page, opts, engine);
            let ran = self
                .surface
                .as_mut()
                .is_none_or(|surface| draws_ran(surface, page.id));
            if ran {
                return Ok(());
            }
            // What the surface holds is unknown, so start it again: no
            // surface means `update` allocates one and replays every layer.
            self.surface = None;
            self.raster = None;
            self.served_at = None;
            self.depth = 0;
            self.layers.clear();
        }
        Err(discarded("drawing the page for a read"))
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
            self.matte = opts.matte.clone();
            self.msaa = opts.msaa;
            self.depth = 0;
            self.layers.clear();

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

            match cache_image {
                // A cached bitmap replaces what the surface holds: it is the
                // page as it stood at its depth, background included. Drawn
                // over a surface that already had it, or part of it, a
                // translucent fill composited twice -- half alpha read back
                // as three quarters after an export. The clear is what makes
                // that right; the depth test only skips the redraw when the
                // surface is as far on as the cache already.
                Some(image) if cache_depth > self.depth => {
                    canvas.clear(Color::TRANSPARENT);
                    canvas.draw_image(image, (0, 0), None);
                    self.depth = cache_depth;
                }
                // Nothing on the surface yet, and no cache to start from.
                _ if self.depth == 0 => {
                    clear_to(canvas, self.matte.as_ref());
                }
                // The surface is as far on as the cache or further.
                _ => {}
            }

            // Draw only the layers the surface has not seen, at the read's
            // density. Inside a save, since the scale is the canvas's own
            // state: left on it, every later update multiplied it again, and
            // a page read twice at density two drew its new layers at four.
            canvas.save();
            canvas.scale((self.density, self.density));
            for pict in page.layers.iter().skip(self.depth) {
                pict.playback(canvas, Target::Pixels);
            }
            canvas.restore();
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

    /// Drops the rasters of open layers past the first `open`, which have
    /// closed -- see [`Self::read_rastered`].
    fn forget_layers_past(&mut self, open: usize) {
        self.layers.truncate(open);
    }

    /// Reads the page as composited with the layers `open` on it drawn over
    /// it, at `density`, into `pixels`, laid out as `read`'s image info and
    /// starting at its origin. `false`, having read nothing, where the read
    /// is to be replayed instead -- see [`Self::read_overlaid`]: on the GPU,
    /// and where the rasters would come to more than [`LAYER_RASTER_BUDGET`].
    ///
    /// Each open layer keeps a raster of what it has drawn, and a read draws
    /// into it only the entries it has not seen, so that a read costs what
    /// was drawn since the last rather than everything the layer holds: n
    /// reads inside a layer, a draw apiece, cost O(n) rather than O(n^2).
    /// The raster is drawn as the layer's device is -- transparent to begin
    /// with, at the page's scale, under the clip of every layer around it
    /// and its own -- and composited as the layer is on closing: the
    /// raster's pixels of the read drawn through the layer's paint, under
    /// those clips, onto what the layer is opened over, innermost first.
    /// Which layers that reproduces is [`OpenLayer::rasters`].
    fn read_rastered(
        &mut self,
        open: &[OpenLayer],
        density: f32,
        read: (&ImageInfo, (i32, i32)),
        pixels: &mut [u8],
    ) -> Result<bool, String> {
        let unreadable = || "Could not read the page beneath an open layer";
        let surface = self.surface.as_mut().ok_or_else(unreadable)?;
        // UPSTREAM: skia-safe 0.153.3 -- unfiled -- worked around
        // Re-check: cargo test reads_between_draws_read_what_layers_close_to
        // with `metal` and this return dropped. On the GPU Skia draws a
        // layer into a texture it sizes up to a bucket of the surface's, and
        // a raster the page's size is not that bucket: an exact blur read
        // from rasters came out a level from the layers closed, at 8 of
        // 4800 bytes. The GPU replays -- see `Self::read_overlaid`.
        if surface.recording_context().is_some() {
            return Ok(false);
        }
        let page_bytes = surface.image_info().compute_min_byte_size();
        if open.len().saturating_mul(page_bytes) > LAYER_RASTER_BUDGET {
            self.layers.clear();
            return Ok(false);
        }
        self.draw_rasters(open, density)?;
        self.compose_rastered(open, density, read, pixels)?;
        Ok(true)
    }

    /// Brings the raster of each layer in `open` up to what it has drawn,
    /// making the ones a read has not yet been taken inside.
    fn draw_rasters(
        &mut self,
        open: &[OpenLayer],
        density: f32,
    ) -> Result<(), String> {
        let unreadable = || "Could not read the page beneath an open layer";
        let surface = self.surface.as_mut().ok_or_else(unreadable)?;
        let page_info = surface.image_info();
        // The rasters of the layers still open, a stack like them.
        let still = self
            .layers
            .iter()
            .zip(open)
            .take_while(|(raster, layer)| raster.id == layer.id)
            .count();
        self.layers.truncate(still);
        for layer in &open[still..] {
            let mut raster =
                surface.new_surface(&page_info).ok_or_else(unreadable)?;
            raster.canvas().clear(Color::TRANSPARENT);
            self.layers.push(LayerRaster {
                id: layer.id,
                surface: raster,
                drawn: 0,
            });
        }
        for (at, raster) in self.layers.iter_mut().enumerate() {
            let entries = &open[at].entries;
            if raster.drawn == entries.len() {
                continue;
            }
            let canvas = raster.surface.canvas();
            canvas.save();
            canvas.scale((density, density));
            clip_to(canvas, &open[..=at]);
            for entry in &entries[raster.drawn..] {
                entry.playback(canvas, Target::Pixels);
            }
            canvas.restore();
            raster.drawn = entries.len();
        }
        Ok(())
    }

    /// Composites the rasters of the layers in `open` over the page, for the
    /// read's rectangle alone, and reads the result into `pixels`.
    fn compose_rastered(
        &mut self,
        open: &[OpenLayer],
        density: f32,
        (dst_info, origin): (&ImageInfo, (i32, i32)),
        pixels: &mut [u8],
    ) -> Result<(), String> {
        let unreadable = || "Could not read the page beneath an open layer";
        let surface = self.surface.as_mut().ok_or_else(unreadable)?;
        let crop = surface.image_info().with_dimensions(dst_info.dimensions());
        let shift = (-origin.0 as f32, -origin.1 as f32);
        let mut copy = Paint::default();
        copy.set_blend_mode(BlendMode::Src);
        // What each layer holds of the read, with the layers inside it
        // composited in, innermost first; then the page, with the outermost.
        let mut inside: Option<SkImage> = None;
        for at in (0..=open.len()).rev() {
            let mut composed =
                surface.new_surface(&crop).ok_or_else(unreadable)?;
            let canvas = composed.canvas();
            // At `at`, what layer `at - 1` holds -- or, at 0, the page --
            // and over it, through its paint, what layer `at` holds.
            let beneath = match at {
                0 => surface.image_snapshot(),
                _ => self.layers[at - 1].surface.image_snapshot(),
            };
            canvas.draw_image(beneath, shift, Some(&copy));
            if let Some(layer) = inside.take() {
                canvas.save();
                canvas.translate(shift);
                canvas.scale((density, density));
                clip_to(canvas, &open[..=at]);
                canvas.reset_matrix();
                let paint = open[at].rec.paint.as_ref().map(composited);
                canvas.draw_image(layer, (0, 0), paint.as_deref());
                canvas.restore();
            }
            if at > 0 {
                inside = Some(composed.image_snapshot());
                continue;
            }
            let row_bytes = dst_info.min_row_bytes();
            return match read_converted_on_cpu(
                &mut composed,
                dst_info,
                pixels,
                row_bytes,
                (0, 0),
            ) {
                true => Ok(()),
                false => Err(format!(
                    "Could not get image data (format: {:?})",
                    dst_info.color_type()
                )),
            };
        }
        Err(unreadable().to_string())
    }

    /// Reads the page as composited, with `over` drawn on top of it at
    /// `density`, leaving what the surface holds untouched. `page` is the
    /// identity of the page the surface holds.
    ///
    /// Composed on a surface the size of `read`, translated to its origin,
    /// so that its cost follows the read rather than the page, which a 1x1
    /// hit test inside a layer otherwise pays for whole. The translation is
    /// by whole pixels, so each pixel is rasterized as it is on the page.
    /// `read.whole` composes the page instead, for an `over` that can draw at
    /// a pixel what lies outside the read -- see
    /// `PageRecorder::open_layers_reach_past_a_read` -- and so does a GPU
    /// surface, where a layer composites by the size of the surface it is
    /// composed on.
    ///
    /// `over` is drawn on a surface made like this one, so on the GPU it is
    /// composited on the GPU, as it would be once filed: the engines round a
    /// composite differently, and composited on the CPU instead a GPU
    /// canvas read a layer at another level from the same layer read after
    /// it closed. Retried as [`Self::confirmed_update`] is,
    /// since the GPU can discard these draws as it can those.
    pub fn read_overlaid(
        &mut self,
        over: Option<&Layer>,
        density: f32,
        page: usize,
        read: Read<'_>,
        pixels: &mut [u8],
    ) -> Result<(), String> {
        let Read {
            info: dst_info,
            origin,
            whole,
        } = read;
        let unreadable = || "Could not read the page beneath an open layer";
        let surface = self.surface.as_mut().ok_or_else(unreadable)?;
        let mut copy = Paint::default();
        copy.set_blend_mode(BlendMode::Src);
        let page_info = surface.image_info();
        // UPSTREAM: skia-safe 0.153.3 -- unfiled -- worked around
        // Re-check: cargo test a_read_inside_a_layer_reads_what_the_page_does
        // with `metal` and the `||` below dropped. On the GPU Skia draws
        // a layer into a texture it sizes up to a bucket of the surface's,
        // and what the layer composites rounds by that bucket: an exact blur
        // in a layer read 227 at one pixel on a surface 20, 30 or 32 wide,
        // and 228 at 16, 33 or 60. The page is composed whole there, as it
        // is once the layer closes.
        let whole = whole || surface.recording_context().is_some();
        let (info, at) = match whole {
            true => (page_info, origin),
            false => (page_info.with_dimensions(dst_info.dimensions()), (0, 0)),
        };
        for _ in 0..DRAW_ATTEMPTS {
            let mut composed =
                surface.new_surface(&info).ok_or_else(unreadable)?;
            let canvas = composed.canvas();
            if !whole {
                canvas.translate((-origin.0 as f32, -origin.1 as f32));
            }
            canvas.draw_image(surface.image_snapshot(), (0, 0), Some(&copy));
            if let Some(over) = over {
                canvas.scale((density, density));
                over.playback(canvas, Target::Pixels);
            }
            if !draws_ran(&mut composed, page) {
                continue;
            }
            let row_bytes = dst_info.min_row_bytes();
            return match read_converted_on_cpu(
                &mut composed,
                dst_info,
                pixels,
                row_bytes,
                at,
            ) {
                true => Ok(()),
                false => Err(format!(
                    "Could not get image data (format: {:?})",
                    dst_info.color_type()
                )),
            };
        }
        Err(discarded("drawing the open layers for a read"))
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
        let gpu = matches!(engine, RenderingEngine::GPU);
        let repeat = self.served_at == Some(self.depth);
        if !repeat || !gpu {
            self.served_at = Some(self.depth);
            if read_converted_on_cpu(
                surface, dst_info, pixels, row_bytes, origin,
            ) {
                return true;
            }
            // A GPU surface refuses destination formats that the same pixels
            // answer for once they are in main memory -- `BGR101010x` is the
            // one in the published set. Falling through to the copy below is
            // what makes a first read agree with a second, which reaches it
            // anyway and succeeds. Without this the same call on the same
            // canvas failed and then worked, decided by how many times it
            // had been made.
            //
            // Only on the GPU. A raster surface that refuses a format will
            // refuse it again through a raster copy of itself, so there is
            // nothing below worth reaching.
            if !gpu {
                return false;
            }
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
    /// For a page taken while a `saveLayer` is open, the filed page it holds
    /// the open layers over: the cached bitmap it may start from, how deep
    /// that bitmap may be, and whose it is to file. `None` for any other
    /// page, which starts from its own. See [`Page::cache_key`].
    pub(crate) starts_from: Option<FiledPrefix>,
    /// A weak reference to the generation this page was taken from, held so
    /// a store can tell whether that generation still exists. See
    /// [`PageId`].
    pub(crate) owner: Weak<PageId>,
    pub bounds: Rect,
    /// The recording in order: Skia pictures, and the blurred draws kept as
    /// ours so that their blur is made at playback -- see [`Layer`]. Shared
    /// with the recorder it was taken from -- see [`PageRecorder::page`].
    pub layers: Arc<Vec<Layer>>,
    /// Index-aligned with `layers`; see [`PageRecorder::append_isolated`].
    pub(crate) features: Arc<Vec<VectorFeatures>>,
}

/// The filed page a page taken inside a `saveLayer` holds the open layers
/// over: the first `depth` of its layers are that page's, under that page's
/// identity.
///
/// A composite of such a page files that prefix under `owner` as it passes
/// it -- see `Page::composited_surface` -- so that the next export, inside
/// the layer or after it closes, starts from it rather than from nothing.
/// The entry is the filed page's own, and goes with that page's generation
/// as any of its entries does.
#[derive(Debug, Clone)]
pub(crate) struct FiledPrefix {
    id: usize,
    depth: usize,
    owner: Weak<PageId>,
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
            starts_from: None,
            owner: Weak::new(),
            bounds: skia_safe::Rect::new_empty(),
            layers: Arc::default(),
            features: Arc::default(),
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
            .fold(VectorFeatures::PLAIN, |all, layer| all.with(*layer))
    }

    pub fn depth(&self) -> usize {
        self.layers.len()
    }

    /// The page whose cached bitmap this one may start from, and the
    /// deepest that bitmap may be.
    ///
    /// Capped at the filed depth for a page taken inside a layer: the layer
    /// closes into the filed page at the depth this one holds it at, so a
    /// bitmap cached as that closes is the finished layer, not the one this
    /// page holds part way.
    fn cache_key(&self) -> (usize, usize) {
        self.starts_from
            .as_ref()
            .map_or((self.id, self.depth()), |filed| (filed.id, filed.depth))
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
        matte: Option<&Matte>,
        density: f32,
    ) -> Result<(), String> {
        if let Some(matte) = matte {
            matte.paint(canvas);
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
                // Consecutive entries none of which is refused, so that the
                // pieces of a run among them share their clip -- see
                // `Clipped`.
                let start = index;
                while index < self.layers.len() && !refused(index) {
                    index += 1;
                }
                play_entries(
                    &self.layers[start..index],
                    canvas,
                    Target::Document,
                );
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
        layers: &[Layer],
        density: f32,
    ) -> Result<(), String> {
        let dims = self.scaled_dimensions(density);
        let info = ImageInfo::new(
            dims,
            ColorType::RGBA8888,
            AlphaType::Premul,
            Some(ColorSpace::new_srgb()),
        );
        let mut surface = started::raster(&info, None, None)
            .ok_or("Could not allocate a surface for the SVG fallback")?;

        let raster = surface.canvas();
        raster.scale((density, density));
        for layer in layers {
            layer.playback(raster, Target::Embedded);
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
            let Some(first) = row
                .as_chunks::<4>()
                .0
                .iter()
                .position(|pixel| pixel[3] != 0)
            else {
                continue;
            };
            let last = row
                .as_chunks::<4>()
                .0
                .iter()
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

    pub fn picture(&self, matte: Option<&Matte>) -> Option<Picture> {
        let mut compositor = PictureRecorder::new();
        let output = compositor.begin_recording(self.bounds, true);
        if let Some(matte) = matte {
            matte.paint(output);
        }
        // Pixels: Skia draws this picture itself, as a pattern's or an
        // image's, and draws that as pixels, so a cut draw in it clips by
        // coverage. A document walks it as vectors only for a canvas drawn
        // in with `draw_canvas` that holds no cut draw -- see
        // `Context2D::draw_picture_costing`. Its PDF backend writes such a
        // pattern as an image whichever form the picture holds: the same
        // file either way, measured. A run's pieces between its cut draws
        // share their clip here -- see `play_entries`.
        play_entries(&self.layers, output, Target::Pixels);
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
            let (id, depth) = self.cache_key();
            let (image, depth) = PageCache::get(id, options, depth);
            match multisampled && depth != self.depth() {
                true => (None, 0),
                false => (image, depth),
            }
        };

        if let Some(image) = cache_image {
            // use the cached bitmap as the background
            canvas.draw_image(image, (0, 0), None);
        } else if let Some(matte) = &options.matte {
            // otherwise, fill the canvas if requested
            matte.paint(canvas);
        }

        // draw newly added layers, filing the filed page beneath any open
        // layers as it is passed -- see `FiledPrefix`
        canvas.set_matrix(&img_scale);
        let prefix = self
            .starts_from
            .as_ref()
            .filter(|filed| filed.depth > cache_depth && !options.single_use);
        let split = prefix.map_or(cache_depth, |filed| filed.depth);
        for pict in self.layers.iter().take(split).skip(cache_depth) {
            pict.playback(canvas, Target::Pixels);
        }
        if let Some(filed) = prefix {
            // A snapshot, which Skia keeps as it stood while the open layers
            // draw over the surface next: an entry holding them would show
            // a layer still being drawn as though it were filed.
            let image = surface.image_snapshot();
            let image = self.cacheable(&mut surface, image, options, engine)?;
            PageCache::set(&filed.owner, image, options, filed.depth);
        }
        let canvas = surface.canvas();
        for pict in self.layers.iter().skip(split.max(cache_depth)) {
            pict.playback(canvas, Target::Pixels);
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
        let image = surface
            .make_temporary_image()
            .unwrap_or_else(|| surface.image_snapshot());
        let image = self.cacheable(surface, image, options, engine)?;

        if self.depth() > cache_depth && !options.single_use {
            PageCache::set(&self.owner, image.clone(), options, self.depth());
        }

        Ok(image)
    }

    /// `image`, taken off a compositing surface, as the page cache and an
    /// encoder take it: in the requested space, and in main memory.
    fn cacheable(
        &self,
        surface: &mut Surface,
        image: SkImage,
        options: &ExportOptions,
        engine: RenderingEngine,
    ) -> Result<SkImage, String> {
        let img_dims = self.scaled_dimensions(options.density);

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
                self.converted(image, &out_info, options, engine)?
            }
        };

        let image = match image.is_texture_backed() {
            false => image,
            true => {
                // UPSTREAM: skia-safe 0.153.3 -- unfiled -- not worked around
                // Re-check: GrMtlGpu::onReadPixels checks its submit
                //
                // Skia M153's `GrMtlGpu::onReadPixels`, in
                // `skia/src/gpu/ganesh/mtl/GrMtlGpu.mm` under the
                // skia-bindings crate, waits on the command buffer that
                // copies the texture out with
                // `submitCommandBuffer(kForce_SyncQueue)`, drops the answer,
                // and copies whatever the transfer buffer holds. So a
                // readback whose own buffer the GPU discarded hands back
                // zeros or stale bytes as success, and nothing here can tell.
                // The draws before it are confirmed by `confirmed_surface`;
                // this copy is not, and it is short-lived, which is the whole
                // of the argument for living with it. A readback seen
                // transparent is told from this by running it again under
                // `node scripts/gpu-events.mjs --run -- <command>`, which on
                // a failure prints any GPU recovery while it ran.
                //
                // Checking that submit covers the copy only. The draws were
                // committed earlier in a buffer of their own, by the
                // `submit()` `SurfaceContext::readPixels` makes before the
                // copy, and nothing waits on that one -- so
                // `crate::gpu::completion::draws_ran` and its extra wait stay
                // until Skia reports that buffer too.
                let mut context = surface.direct_context();
                image.make_non_texture_image(context.as_mut()).ok_or_else(
                    || "Could not read the page back from the GPU".to_string(),
                )?
            }
        };

        Ok(image)
    }

    /// `image` redrawn into a surface of `out_info`'s space, with the draw
    /// confirmed to have run on the GPU, or `image` itself when no such
    /// surface can be made.
    fn converted(
        &self,
        image: SkImage,
        out_info: &ImageInfo,
        options: &ExportOptions,
        engine: RenderingEngine,
    ) -> Result<SkImage, String> {
        for _ in 0..DRAW_ATTEMPTS {
            let Ok(mut surface) = engine.make_surface(out_info, options) else {
                return Ok(image);
            };
            surface.canvas().draw_image(&image, (0, 0), None);
            if draws_ran(&mut surface, self.id) {
                return Ok(surface.image_snapshot());
            }
        }
        Err(discarded("converting the page to its export colour space"))
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
    pub(crate) fn composite(
        &self,
        options: &ExportOptions,
        engine: RenderingEngine,
    ) -> Result<SkImage, String> {
        let (mut surface, cache_depth) =
            self.confirmed_surface(options, engine)?;
        self.image_from(&mut surface, options, engine, cache_depth)
    }

    /// [`Self::composited_surface`], with its draws confirmed to have run
    /// on the GPU before anything reads them: drawn once more on a fresh
    /// surface if they did not, and an error if that fails too. See
    /// [`crate::gpu::completion`].
    fn confirmed_surface(
        &self,
        options: &ExportOptions,
        engine: RenderingEngine,
    ) -> Result<(Surface, usize), String> {
        for _ in 0..DRAW_ATTEMPTS {
            let (mut surface, cache_depth) =
                self.composited_surface(options, engine)?;
            if draws_ran(&mut surface, self.id) {
                return Ok((surface, cache_depth));
            }
        }
        Err(discarded("compositing the page"))
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
            self.confirmed_surface(options, engine)?;

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
        let draw = || -> Result<Surface, String> {
            let mut surface = make_compositing_surface(
                &engine,
                options,
                img_dims,
                &options.surface_color_space,
            )?;
            let canvas = surface.canvas();
            if let Some(matte) = &options.matte {
                matte.paint(canvas);
            }
            canvas.set_matrix(&img_scale);
            for pict in self.layers.iter() {
                pict.playback(canvas, Target::Pixels);
            }
            Ok(surface)
        };
        let mut surface = None;
        for _ in 0..DRAW_ATTEMPTS {
            let mut drawn = draw()?;
            if draws_ran(&mut drawn, self.id) {
                surface = Some(drawn);
                break;
            }
        }
        let mut surface =
            surface.ok_or_else(|| discarded("compositing the page"))?;

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
            ref matte,
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
        // silently answer in sRGB. `pixels_as`, which backs
        // `getImageData`, passes the option through, and the two disagreed
        // about the same picture.
        if matches!(format, ImageFormat::Raw) {
            let dst_info = ImageInfo::new(
                img_dims,
                color_type,
                options.alpha_type,
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
                    matte.as_ref(),
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
                    matte.as_ref(),
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
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|pair| u16::from_le_bytes(*pair))
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
        matte: Option<&Matte>,
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
                page.append_to(doc, matte.as_ref(), density)
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

    /// Writes each page to its own file, numbering them into `pattern`.
    ///
    /// `pattern` carries a `{}` where the number goes. `width` zero-pads the
    /// number to that many digits; `None` uses as many as the page count
    /// needs, so ten pages number `1` to `10` and a hundred number `001` to
    /// `100`.
    ///
    /// # Errors
    ///
    /// Returns the encoder's or the filesystem's own message, and refuses a
    /// `width` past [`MAX_FOLIO_WIDTH`] rather than trying to build it.
    pub fn write_sequence(
        &self,
        pattern: &str,
        width: Option<usize>,
        options: ExportOptions,
    ) -> Result<(), String> {
        let padding = match width {
            Some(width) if width > MAX_FOLIO_WIDTH => {
                return Err(format!(
                    "page-number padding must be at most {MAX_FOLIO_WIDTH} \
                     digits (got {width})"
                ));
            }
            Some(width) => width,
            // `log10` of zero is negative infinity, which saturates to zero
            // rather than wrapping, so a sequence with no pages asks for no
            // padding and writes nothing.
            None => (1.0 + (self.pages.len() as f32).log10().floor()) as usize,
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

/// How a cache entry records which colour space its bitmap holds.
///
/// The pair is `to_xyzd50_hash` and `transfer_fn_hash`, in that order.
type ColorSpaceKey = (XYZD50Hash, u32);

/// The pair identifying `space`, for [`PageCache::color_space`].
fn space_key(space: &ColorSpace) -> ColorSpaceKey {
    (space.to_xyzd50_hash(), space.transfer_fn_hash())
}

#[derive(Debug, Clone)]
struct PageCache {
    image: Option<SkImage>,
    density: f32,
    matte: Option<Matte>,
    msaa: Option<usize>,
    /// The space [`Self::image`] was rasterized into, as the two hashes
    /// Skia identifies one by.
    ///
    /// Part of what makes an entry valid, because the pixels are in this
    /// space rather than convertible from it: an export asking for another
    /// one and served this bitmap gets the first export's conversion, and
    /// where that conversion clipped -- any colour outside the narrower
    /// gamut -- the clipped value is what it re-encodes.
    ///
    /// Hashes rather than the `ColorSpace` itself, for the reason
    /// [`PixelColorSpace::matching`] compares the same two: a space built
    /// from code points and one built by `new_srgb` are the same space and
    /// not the same object, and these are what Skia's own conversion check
    /// reads. Two spaces it calls equal convert identically, so a bitmap is
    /// genuinely reusable between them.
    color_space: Option<ColorSpaceKey>,
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
            color_space: None,
            used: CACHE_USES.fetch_add(1, Ordering::Relaxed),
            bytes: 0,
        }
    }
}

/// The widest zero-padded page number [`PageSequence::write_sequence`] builds.
///
/// A folio is one component of a filename, and the filesystems this runs on
/// stop a component at 255 bytes -- `NAME_MAX` on Linux and macOS, and the
/// same figure for a path segment on NTFS -- so a wider one cannot name a
/// file that could be created.
///
/// The bound is on the allocation rather than on the result. The width is a
/// `format!` field width, so an unbounded one asks for a string the process
/// cannot survive requesting, and an allocation failure aborts where an
/// error would have been caught.
const MAX_FOLIO_WIDTH: usize = 255;

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
pub(crate) const PAGE_CACHE_BYTES: usize = 64 * 1024 * 1024;

/// How far below the count bound an eviction pass takes the cache.
///
/// A pass used to stop the moment the map was inside its bound again, which
/// meant it removed exactly one entry and did a full walk to choose it --
/// every entry collected into a vector, summed and sorted. Once the map sits
/// at its bound, which is where any workload that makes more than sixty-four
/// pages leaves it, that walk was paid on every `new Canvas`, every
/// `getContext`, every `newPage` and every full-canvas clear.
///
/// Measured inside the binding, on a loop making sixty thousand contexts:
/// `PageRecorder::new` was 3.2 microseconds of a 3.4 microsecond
/// construction, against 580 nanoseconds for the same call in a process
/// where the map stayed small. `State::default` -- the paint, the paragraph
/// style, the text style -- was 207.
///
/// Three quarters, so one walk serves sixteen insertions rather than one.
/// Lower would start discarding pages a drawing is still exporting; the
/// coldest quarter is what the clock already orders for removal.
const PAGE_CACHE_LOW_WATER: usize = PAGE_CACHE_SIZE * 3 / 4;

/// Whether an eviction pass is running.
///
/// A pass reads every entry, decides from that snapshot, and only then
/// removes. Two threads acting on the same snapshot each take the map down to
/// [`PAGE_CACHE_LOW_WATER`], so it lands at twice the intended depth and live
/// pages re-rasterize for nothing. Concurrent entry is the ordinary case
/// rather than a corner: [`Pages`](crate::export::Pages) is `Send` and
/// encoding runs on `rayon` workers.
///
/// A thread that finds a pass running skips rather than waiting. The running
/// pass is walking the same map and will take it inside its bounds; a second
/// pass behind it would decide from a snapshot the first has already
/// invalidated, which is the thing being prevented.
static EVICTING: AtomicBool = AtomicBool::new(false);

/// Held for the length of one eviction pass. See [`EVICTING`].
struct EvictionPass;

impl EvictionPass {
    /// The pass, or `None` when another thread is already running one.
    fn begin() -> Option<Self> {
        match EVICTING.swap(true, Ordering::Acquire) {
            true => None,
            false => Some(EvictionPass),
        }
    }
}

impl Drop for EvictionPass {
    /// Releases the pass on every exit, including the early returns inside
    /// it and an unwind through it.
    fn drop(&mut self) {
        EVICTING.store(false, Ordering::Release);
    }
}

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
        // Nothing to do that the running pass is not already doing.
        let Some(_pass) = EvictionPass::begin() else {
            return;
        };

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

        // Below the bound rather than to it, but only for the count -- see
        // [`PAGE_CACHE_LOW_WATER`]. A pass the byte budget asked for stops at
        // the bound, because an entry carrying no bitmap frees nothing and
        // taking the count down would discard live pages for no bytes.
        let target = match count > PAGE_CACHE_SIZE {
            true => PAGE_CACHE_LOW_WATER,
            false => PAGE_CACHE_SIZE,
        };

        entries.sort_unstable_by_key(|(used, _, _)| *used);

        for &(_, id, bytes) in &entries {
            if count <= target && held <= PAGE_CACHE_BYTES {
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
                    matte: opts.matte.clone(),
                    msaa: opts.msaa,
                    color_space: Some(space_key(&opts.color_space)),
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
            // The bitmap holds one space's pixels and cannot stand in for
            // another's. Left out of this comparison, a canvas exported to
            // sRGB and then to its own wider space was served the sRGB
            // rasterization for the second: in-gamut colours survived the
            // round trip and everything outside it came back clipped, so the
            // defect was invisible to any test whose fill was in both gamuts.
            && self.color_space == Some(space_key(&opts.color_space))
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
        .map(|obj| obj.unwrap().borrow().page_for_export(opts, &engine))
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
    pub matte: Option<Matte>,
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
    /// Whether the bytes handed back have colour scaled by alpha.
    ///
    /// [`AlphaType::Unpremul`] is the default and is what `putImageData`
    /// means, so a caller who never asks gets what a browser gives. The
    /// default lives in [`ExportOptions::default`] and nowhere else, which is
    /// what makes "unset" and "set to unpremultiplied" the same request
    /// rather than two code paths.
    pub alpha_type: AlphaType,
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
            text_contrast: DEFAULT_TEXT_CONTRAST,
            text_gamma: DEFAULT_TEXT_GAMMA,
            msaa: None,
            color_type: ColorType::RGBA8888,
            bit_depth: None,
            chroma: None,
            lossless: false,
            color_space: ColorSpace::new_srgb(),
            alpha_type: AlphaType::Unpremul,
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
        // UPSTREAM: skia-safe 0.153.3 -- unfiled -- not worked around
        // Re-check: cargo test hinted_macos_text_ignores_contrast
        // The contrast reaches only unhinted text on macOS: Skia's CoreText
        // scaler sets a hinted glyph's contrast to 0, leaving its weight to
        // CoreGraphics' dilation of smoothed text, in `SkTypeface_mac_ct.cpp`.
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
    ///
    /// # What the narrow formats cost, measured
    ///
    /// Skia refuses none of them: a raster surface builds in every published
    /// format, at every alpha type, with or without a colour space. The
    /// substitution here is this crate's own choice, so the two objections
    /// above are the whole of the case for it and are recorded rather than
    /// asserted. Each row is a surface built in that format, cleared to
    /// transparent and read back as RGBA, then drawn red, green and blue and
    /// read back again:
    ///
    /// | format                         | clear(TRANSPARENT) | three colours    |
    /// | ------------------------------ | ------------------ | ---------------- |
    /// | `Gray8` `RGB565` `R8UNorm` `R8G8UNorm` | `[0,0,0,255]` opaque | (varies) |
    /// | `Alpha8` `A16Float` `A16UNorm` | `[0,0,0,0]` kept   | all `[0,0,0,255]` |
    /// | `ARGB4444`                     | `[0,0,0,0]` kept   | all three kept   |
    ///
    /// So the group that keeps transparency discards colour, and the group
    /// that keeps colour discards transparency. `ARGB4444` is the only format
    /// below N32 that keeps both, and it is not composited in either, for two
    /// reasons that are not about correctness. Four bits a channel quantises
    /// every intermediate blend and not only the output -- a fill at 50%
    /// alpha reads back at 136 rather than 128, because `0.5 * 15` rounds to
    /// 8 and `8/15` is `0.533`. And the Metal backend refuses an `ARGB4444`
    /// surface outright, so the 8 MB it would save on a 2048x2048 raster
    /// canvas is zero on the default backend here, and buying it would mean
    /// routing such a canvas away from the GPU -- memory at the cost of
    /// rendering speed, which is the wrong trade.
    ///
    /// Metal's acceptances do not follow the correctness result either: it
    /// takes `Alpha8`, `R8UNorm` and `R8G8UNorm`, all of which fail above,
    /// and refuses every format that passes. There is no backend-shaped rule
    /// hiding here, which is the next thing worth not looking for.
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
mod unpremultiply_tests {
    use super::*;

    #[test]
    fn unpremultiplying_matches_the_raster_pipeline() {
        // Every channel against every alpha, through Skia's own raster
        // conversion: the table must agree byte for byte, including where
        // Skia's rounding is not round-to-nearest -- 1 over an alpha of 6 is
        // 42, not 43 -- and where a channel above its alpha is held at 255.
        let premul = ImageInfo::new(
            (256, 256),
            ColorType::RGBA8888,
            AlphaType::Premul,
            None,
        );
        let unpremul = premul.with_alpha_type(AlphaType::Unpremul);
        let mut src = vec![0u8; 256 * 256 * 4];
        for (i, px) in src.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let (alpha, channel) = ((i / 256) as u8, (i % 256) as u8);
            px.copy_from_slice(&[channel, channel, channel, alpha]);
        }
        let mut dst = vec![0u8; src.len()];
        let pixmap = Pixmap::new(&premul, &mut src, 256 * 4).expect("pixmap");
        assert!(pixmap.read_pixels(&unpremul, &mut dst, 256 * 4, (0, 0)));
        let table = unpremultiply_table();
        for (i, px) in dst.as_chunks::<4>().0.iter().enumerate() {
            let (alpha, channel) = (i / 256, i % 256);
            let want = match alpha {
                0 => 0,
                _ => table[alpha][channel],
            };
            assert_eq!(px[0], want, "channel {channel} at alpha {alpha}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{
        faint::Faint,
        layer::{reach_of, reach_the_clip},
    };
    use parking_lot::Mutex;
    use skia_safe::{
        BlurStyle, FilterMode, Font, MaskFilter, MipmapMode, PaintCap,
        PaintJoin, PaintStyle, Point, SamplingOptions, TileMode, color_filters,
        gradient::{Colors, Gradient, Interpolation, shaders},
        image_filters,
    };
    use std::{collections::BTreeSet, ptr, thread};

    /// Serialises the tests that assert on the process-wide page cache.
    ///
    /// `release_cached_pages` empties every entry rather than one, so the test
    /// that calls it can clear a bitmap another test has just filed and is
    /// about to assert on. That is what it is for in production -- the idle
    /// watcher wants the whole cache back -- so the tests take turns instead
    /// of the cache growing a narrower door for their benefit.
    ///
    /// Found as one failure in fourteen runs of the suite, always
    /// `a_page_written_once_is_not_kept` losing its entry to
    /// `going_quiet_gives_the_bitmaps_back`. `parking_lot` because a test that
    /// fails while holding this should fail alone, rather than poisoning the
    /// lock and taking the other three with it.
    static SHARED_CACHE: Mutex<()> = Mutex::new(());

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
        started::raster_from_data(
            &info,
            skia_safe::Data::new_copy(&bytes),
            row_bytes,
        )
        .expect("a raster image of the bytes just built")
    }

    /// A page of the corpus, drawn at the size the probe was tuned against.
    ///
    /// 1200x900 is not decoration. The sample is a fixed count of rows, so
    /// its share of a page -- and with it both what the probe costs and how
    /// much of the drawing it has seen -- depends entirely on the page's
    /// size. Scoring at a smaller one would be scoring a different question.
    const PAGE: (i32, i32) = (1200, 900);

    /// A raster surface to draw a corpus page on.
    fn corpus_surface() -> Surface {
        started::raster_n32_premul(PAGE)
            .expect("a raster surface for a corpus page")
    }

    /// A typeface out of `tests/assets`, not a system one.
    ///
    /// The corpus has to draw the same glyphs on every machine: text is one
    /// of the cases the probe answers differently from a photograph, and a
    /// fallback font would make that case mean something else on a runner
    /// than it means here.
    fn corpus_typeface() -> skia_safe::Typeface {
        let bytes = fs::read(
            "tests/assets/fonts/Raleway/Raleway-VariableFont_wght.ttf",
        )
        .expect("the Raleway variable font out of tests/assets");
        skia_safe::FontMgr::new()
            .new_from_data(skia_safe::Data::new_copy(&bytes), None)
            .expect("a typeface from the Raleway bytes")
    }

    /// A photograph, tiled at its own size to fill the page.
    ///
    /// Tiled rather than scaled up: resampling a 400x266 JPEG to fill 1200x900
    /// smooths it into something that compresses like a gradient, and a
    /// photograph is the case the probe most has to get right -- it is the one
    /// where filtering wins.
    fn photograph_page() -> SkImage {
        let bytes = fs::read("tests/assets/images/globe.jpg")
            .expect("the globe photograph out of tests/assets");
        let photo = started::deferred_from_encoded_data(
            skia_safe::Data::new_copy(&bytes),
            None,
        )
        .expect("a decodable JPEG");

        let mut surface = corpus_surface();
        let canvas = surface.canvas();
        let tiles = |page: i32, tile: i32| (page + tile - 1) / tile;
        for down in 0..tiles(PAGE.1, photo.height()) {
            for across in 0..tiles(PAGE.0, photo.width()) {
                canvas.draw_image(
                    &photo,
                    (across * photo.width(), down * photo.height()),
                    None,
                );
            }
        }
        surface.image_snapshot()
    }

    /// A page of flat panels: the interface case, whose rows repeat.
    fn interface_page() -> SkImage {
        let mut surface = corpus_surface();
        let canvas = surface.canvas();
        canvas.clear(Color::from_rgb(0xF5, 0xF6, 0xFA));

        let mut paint = Paint::default();
        paint.set_anti_alias(false);
        for row in 0..6 {
            for column in 0..4 {
                paint.set_color(match (row + column) % 3 {
                    0 => Color::from_rgb(0x27, 0x3C, 0x75),
                    1 => Color::from_rgb(0x40, 0x73, 0x9E),
                    _ => Color::from_rgb(0xDC, 0xDD, 0xE1),
                });
                let (x, y) =
                    (40.0 + column as f32 * 290.0, 30.0 + row as f32 * 145.0);
                canvas.draw_rect(Rect::from_xywh(x, y, 260.0, 120.0), &paint);
            }
        }
        surface.image_snapshot()
    }

    /// A gradient as Skia's own shader paints one, along the axis given.
    ///
    /// Not the hand-built ramp in `dithered_gradient_image`: that one varies
    /// per pixel along the row and comes out wanting filtering by a factor of
    /// 130, where what a caller's `createLinearGradient` produces is a page of
    /// nearly repeating rows -- which is the case the module doc describes,
    /// and the opposite answer.
    ///
    /// The axis is a parameter because it changes the answer rather than the
    /// picture. Diagonally the rows all differ and the probe reads the page
    /// exactly; straight down the page every row is one flat colour, and that
    /// is the drawing it reads worst.
    fn shader_gradient_page(across: f32, down: f32) -> SkImage {
        use skia_safe::{
            Point as SkPoint, TileMode,
            gradient::{Colors, Gradient, Interpolation, shaders},
        };

        let ends = [
            Color::from_rgb(0x1E, 0x37, 0x99).into(),
            Color::from_rgb(0xE5, 0x50, 0x39).into(),
        ];
        let stops = Colors::new(
            &ends,
            Some([0.0f32, 1.0].as_slice()),
            TileMode::Clamp,
            None,
        );
        let gradient = Gradient::new(stops, Interpolation::default());
        let shader = shaders::linear_gradient(
            (SkPoint::new(0.0, 0.0), SkPoint::new(across, down)),
            &gradient,
            None,
        )
        .expect("a linear gradient shader");

        let mut surface = corpus_surface();
        let mut paint = Paint::default();
        paint.set_shader(shader);
        surface
            .canvas()
            .draw_rect(Rect::from_wh(PAGE.0 as f32, PAGE.1 as f32), &paint);
        surface.image_snapshot()
    }

    /// A page of one flat colour.
    ///
    /// The simplest drawing there is, and one the module doc names: every row
    /// identical, so the unfiltered stream is one match repeated and there is
    /// nothing for differencing to remove.
    fn flat_fill_page() -> SkImage {
        let mut surface = corpus_surface();
        let mut paint = Paint::default();
        paint.set_color(Color::from_rgb(0x27, 0x3C, 0x75));
        surface
            .canvas()
            .draw_rect(Rect::from_wh(PAGE.0 as f32, PAGE.1 as f32), &paint);
        surface.image_snapshot()
    }

    /// A striped table: rows of flat colour on a 28-pixel pitch.
    ///
    /// The page that says how long a band has to be. Deflate exploits whole
    /// repeated rows in the unfiltered stream, and can only find a repeat the
    /// sample actually holds two of -- so a pitch narrower than the band is
    /// read correctly and the same drawing sampled a few rows at a time is
    /// not. This is the shape behind the measurement in
    /// `crate::encode::rowfilter`: a page of flat blocks probed 0.24, meaning
    /// filtering should shrink it to a quarter, where filtering really took it
    /// from 45 KB to 67.
    fn striped_table_page() -> SkImage {
        let mut surface = corpus_surface();
        let canvas = surface.canvas();
        canvas.clear(Color::WHITE);

        let mut paint = Paint::default();
        paint.set_anti_alias(false);
        for stripe in 0..PAGE.1 / 28 {
            paint.set_color(match stripe % 2 {
                0 => Color::from_rgb(0xEC, 0xEF, 0xF1),
                _ => Color::from_rgb(0xFF, 0xFF, 0xFF),
            });
            canvas.draw_rect(
                Rect::from_xywh(0.0, stripe as f32 * 28.0, PAGE.0 as f32, 28.0),
                &paint,
            );
            paint.set_color(Color::from_rgb(0x37, 0x47, 0x4F));
            for cell in 0..7 {
                canvas.draw_rect(
                    Rect::from_xywh(
                        60.0 + cell as f32 * 160.0,
                        stripe as f32 * 28.0 + 9.0,
                        90.0 + ((stripe * 31 + cell * 17) % 40) as f32,
                        10.0,
                    ),
                    &paint,
                );
            }
        }
        surface.image_snapshot()
    }

    /// A bar chart: flat above, dense along the baseline, and ruled.
    ///
    /// The case that made two bands worth having rather than one -- a drawing
    /// whose top and bottom are nothing alike.
    fn chart_page() -> SkImage {
        let mut surface = corpus_surface();
        let canvas = surface.canvas();
        canvas.clear(Color::WHITE);

        let mut rule = Paint::default();
        rule.set_color(Color::from_rgb(0xDD, 0xDD, 0xDD));
        rule.set_stroke_width(1.0);
        rule.set_anti_alias(false);
        for line in 0..18 {
            let y = 60.0 + line as f32 * 45.0;
            canvas.draw_line((60.0, y), (1160.0, y), &rule);
        }

        let mut bar = Paint::default();
        bar.set_anti_alias(true);
        for column in 0..48 {
            let height = 60.0 + ((column * 137) % 640) as f32;
            bar.set_color(match column % 2 {
                0 => Color::from_rgb(0x1E, 0x37, 0x99),
                _ => Color::from_rgb(0xE5, 0x50, 0x39),
            });
            canvas.draw_rect(
                Rect::from_xywh(
                    62.0 + column as f32 * 23.0,
                    840.0 - height,
                    18.0,
                    height,
                ),
                &bar,
            );
        }
        surface.image_snapshot()
    }

    /// A page of body text.
    fn text_page() -> SkImage {
        let mut surface = corpus_surface();
        let canvas = surface.canvas();
        canvas.clear(Color::WHITE);

        let font = skia_safe::Font::from_typeface(corpus_typeface(), 21.0);
        let mut ink = Paint::default();
        ink.set_color(Color::from_rgb(0x2F, 0x36, 0x40));
        ink.set_anti_alias(true);

        let line = "The quick brown fox jumps over the lazy dog, and the \
                    typesetter counts the glyphs it left behind.";
        for row in 0..34 {
            canvas.draw_str(
                line,
                (60.0, 60.0 + row as f32 * 25.0),
                &font,
                &ink,
            );
        }
        surface.image_snapshot()
    }

    /// A report page: a photograph over a block of text.
    ///
    /// The heterogeneous case. Neither half's answer is the page's answer, and
    /// a sample that only saw one of them would give the wrong one.
    fn report_page() -> SkImage {
        let mut surface = corpus_surface();
        let canvas = surface.canvas();
        canvas.clear(Color::WHITE);
        // Tiled at native size and clipped, not scaled into the band: a
        // photograph resampled to fit is smoother than a photograph, and the
        // point of this page is that its halves are genuinely unalike.
        let bytes = fs::read("tests/assets/images/globe.jpg")
            .expect("the globe photograph out of tests/assets");
        let photo = started::deferred_from_encoded_data(
            skia_safe::Data::new_copy(&bytes),
            None,
        )
        .expect("a decodable JPEG");
        canvas.save();
        canvas.clip_rect(
            Rect::from_xywh(0.0, 0.0, PAGE.0 as f32, 360.0),
            None,
            false,
        );
        let tiles = |page: i32, tile: i32| (page + tile - 1) / tile;
        for down in 0..tiles(360, photo.height()) {
            for across in 0..tiles(PAGE.0, photo.width()) {
                canvas.draw_image(
                    &photo,
                    (across * photo.width(), down * photo.height()),
                    None,
                );
            }
        }
        canvas.restore();

        let font = skia_safe::Font::from_typeface(corpus_typeface(), 20.0);
        let mut ink = Paint::default();
        ink.set_color(Color::from_rgb(0x2F, 0x36, 0x40));
        ink.set_anti_alias(true);
        for row in 0..20 {
            canvas.draw_str(
                "Figures follow overleaf; the caption is set in the same face.",
                (60.0, 420.0 + row as f32 * 25.0),
                &font,
                &ink,
            );
        }
        surface.image_snapshot()
    }

    /// How far over the smaller of the two encodings the probe may land on any
    /// one drawing.
    ///
    /// Ten percent, and a ceiling rather than a target. The probe reads two
    /// bands and answers for a whole page, so it is sometimes wrong -- and
    /// wrong is bounded on both sides, because what it returns is always one
    /// of the two settings the encoder would ever use. What this pins is that
    /// the miss stays small.
    ///
    /// It is currently exact on nine of the ten drawings below and 5.9% over
    /// on the tenth, so the ceiling is mostly headroom: a page whose two
    /// encodings are within a few percent of each other can land either way
    /// on a machine whose rasteriser puts the pixels down differently, and a
    /// bound set at what this machine measures would fail there for no
    /// defect. The mutants it is sized to catch are much further out -- a
    /// probe that stopped measuring and always answered `ALL` costs 41.2% on
    /// the page of body text.
    ///
    /// Scored this way rather than against which setting is smaller, because
    /// the two are not the same question and only one of them matters. The
    /// flat fill encodes to 6416 bytes filtered and 6417 unfiltered: a
    /// disagreement, worth nothing, and a test that failed on it would be
    /// reporting a rounding error as a defect.
    const OVER_AN_ORACLE: f64 = 10.0;

    /// `image` as PNG with `filter`, at the pinned deflate level.
    fn encoded_at(image: &SkImage, filter: png_encoder::FilterFlag) -> usize {
        let mut opts = png_encoder::Options::default();
        opts.filter_flags = filter;
        opts.z_lib_level = DEFLATE_LEVEL as i32;
        png_encoder::encode_image(None, image, &opts)
            .expect("a PNG of the corpus page")
            .as_bytes()
            .len()
    }

    /// The setting that actually encodes `image` smaller, and both sizes.
    ///
    /// Ground truth, and the only kind there is: the probe reads a sample and
    /// predicts, so the thing it has to be scored against is the whole page
    /// compressed both ways. Everything else is a restatement of the probe.
    fn smaller_of(image: &SkImage) -> (png_encoder::FilterFlag, usize, usize) {
        let (all, none) = (
            encoded_at(image, png_encoder::FilterFlag::ALL),
            encoded_at(image, png_encoder::FilterFlag::NONE),
        );
        match all <= none {
            true => (png_encoder::FilterFlag::ALL, all, none),
            false => (png_encoder::FilterFlag::NONE, none, all),
        }
    }

    #[test]
    fn the_probe_never_costs_much_over_the_smaller_encoding() {
        // What the tuning in `crate::encode::rowfilter` rests on, and what
        // nothing checked until now. Its constants -- 48 rows, two bands, a
        // threshold of one, level six pinned -- were each measured once
        // against pages scored this way and then written down in prose. A
        // change that breaks one of them breaks no test and reads as a
        // simplification, which is how the threshold has already been
        // "corrected" once and had to be put back.
        //
        // Real drawings rather than generated bytes, because generated bytes
        // do not stand in: six synthetic shapes, a gradient and a page of flat
        // blocks among them, all read as worth filtering, where the drawings
        // they were meant to stand for do not. The gradient here is Skia's own
        // shader for the same reason -- the hand-built ramp two helpers up
        // comes out wanting filtering by a factor of 130, and what a caller's
        // `createLinearGradient` paints does not.
        //
        // Each of the four constants the probe is tuned by was broken in turn
        // and this watched to fail, which is the only way to know a test of a
        // measurement measures anything:
        //
        //   always `ALL`             41.2% on the text page, 198.4% on the
        //                            diagonal gradient
        //   always `NONE`            up to 13106%, across five drawings
        //   `PROBE_FILTER_BELOW` 0.8  18.6% on the photograph, which is the
        //                            value it used to hold
        //   `PROBE_BAND_ROWS` 4      198.4% on the diagonal gradient
        //
        // That last is the one the corpus took three attempts to cover, and
        // the reason is worth keeping. Bands have to be long enough to hold
        // two of whatever the drawing repeats, and a page of panels on a
        // 145-pixel pitch or stripes on a 28 does not test that -- both want
        // filtering outright, whatever the sample sees. A gradient does: its
        // rows nearly repeat, so unfiltered is 3 times smaller, and a sample
        // a few rows deep cannot tell.

        let corpus: [(&str, SkImage); 10] = [
            ("a photograph", photograph_page()),
            ("a page of flat panels", interface_page()),
            ("a striped table", striped_table_page()),
            ("a bar chart", chart_page()),
            ("a page of body text", text_page()),
            ("a report page", report_page()),
            (
                "a diagonal gradient",
                shader_gradient_page(PAGE.0 as f32, PAGE.1 as f32),
            ),
            (
                "a vertical gradient",
                shader_gradient_page(0.0, PAGE.1 as f32),
            ),
            ("a flat fill", flat_fill_page()),
            (
                "a dithered gradient",
                dithered_gradient_image(PAGE.0, PAGE.1),
            ),
        ];

        // A corpus that wanted one answer everywhere would be satisfied by a
        // probe that always gave it, and would go on passing after the
        // measurement it stands for had been broken.
        let truths: Vec<png_encoder::FilterFlag> = corpus
            .iter()
            .map(|(_, image)| smaller_of(image).0)
            .collect();
        assert!(
            truths.contains(&png_encoder::FilterFlag::ALL)
                && truths.contains(&png_encoder::FilterFlag::NONE),
            "the corpus has to want both answers to be able to fail: {truths:?}"
        );

        let overpaid: Vec<String> = corpus
            .iter()
            .filter_map(|(what, image)| {
                let (_, best, _) = smaller_of(image);
                let chose = encoded_at(image, png_tuning(image).filter);
                let over = (chose - best) as f64 / best as f64 * 100.0;
                (over > OVER_AN_ORACLE).then(|| {
                    format!("{what}: {over:.1}% over ({chose} against {best})")
                })
            })
            .collect();

        assert!(
            overpaid.is_empty(),
            "the probe cost more than {OVER_AN_ORACLE}% over the smaller \
             encoding on {} of {} drawings:\n  {}",
            overpaid.len(),
            corpus.len(),
            overpaid.join("\n  ")
        );
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
        started::raster_from_data(
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
        let _shared = SHARED_CACHE.lock();
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

        let mut surface = started::raster_n32_premul((4, 4)).unwrap();
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
        let _shared = SHARED_CACHE.lock();
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
            recorder.append(|canvas, _| {
                canvas.draw_rect(Rect::from_wh(4.0, 4.0), &Paint::default());
            });
            let page = recorder.page();
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

    /// The crop `getImageData` builds, spelled the way `context::api` does.
    ///
    /// `Rect::round` rounds all four edges independently, so the width of a
    /// region depends on where it starts and not only on how wide it is.
    fn crop_for(x: f32, y: f32, w: f32, h: f32, density: f32) -> IRect {
        Rect::from_point_and_size(
            (x * density, y * density),
            (w * density, h * density),
        )
        .round()
    }

    fn read(recorder: &mut PageRecorder, crop: IRect, density: f32) -> Vec<u8> {
        let opts = ExportOptions {
            density,
            ..ExportOptions::default()
        };
        recorder
            .pixels(crop, opts, RenderingEngine::CPU)
            .expect("a raster readback")
    }

    /// A first read answers, whatever destination format the GPU surface
    /// refuses.
    ///
    /// `Surface::read_pixels` on a Metal-backed surface declines
    /// `BGR101010x`, while `Image::read_pixels` on a raster copy of the same
    /// pixels accepts it. Since the copy is only made on a *second* read,
    /// the first call failed and the second succeeded -- the same call on the
    /// same canvas answering differently by attempt.
    ///
    /// Skipped rather than failed where there is no GPU to refuse anything:
    /// the raster surface accepts the format, so the branch under test is
    /// not reached and passing would mean nothing.
    #[test]
    fn a_first_read_answers_a_format_the_gpu_surface_refuses() {
        if !RenderingEngine::GPU.selectable() {
            return;
        }

        let mut recorder = PageRecorder::new(Rect::from_wh(4.0, 4.0));
        recorder.append(|canvas, _| {
            canvas.draw_rect(Rect::from_wh(4.0, 4.0), &Paint::default());
        });

        let opts = ExportOptions {
            color_type: ColorType::BGR101010x,
            ..ExportOptions::default()
        };
        let crop = IRect::from_xywh(0, 0, 4, 4);

        let first = recorder.pixels(crop, opts.clone(), RenderingEngine::GPU);
        assert!(
            first.is_ok(),
            "the first read must answer: {:?}",
            first.err()
        );
        // And the second still does, which is what it always did.
        assert!(recorder.pixels(crop, opts, RenderingEngine::GPU).is_ok());
    }

    /// A readback's width is a function of its origin, not only its width.
    ///
    /// Pinned because it looks like a rounding mode and is not one. The
    /// export path scales a *size* -- see [`Page::scaled_dimensions`] -- and
    /// a page has no origin for that to depend on, so the two rules cannot
    /// be stated in the same terms. Anyone reproducing this in JavaScript
    /// with a `Math.ceil` on the width will be wrong at every odd origin.
    #[test]
    fn a_fractional_density_read_rounds_each_edge() {
        let mut recorder = PageRecorder::new(Rect::from_wh(20.0, 20.0));
        recorder.append(|canvas, _| {
            canvas.draw_rect(Rect::from_wh(20.0, 20.0), &Paint::default());
        });

        // A five-wide region at 1.5, walked across the canvas. 0.0->0 and
        // 7.5->8 is eight columns; 1.5->2 and 9.0->9 is seven.
        for (x, expected) in [(0.0, 8), (1.0, 7), (2.0, 8), (3.0, 7)] {
            let crop = crop_for(x, 0.0, 5.0, 5.0, 1.5);
            assert_eq!(crop.width(), expected, "five wide at 1.5 from x={x}");
            assert_eq!(
                read(&mut recorder, crop, 1.5).len(),
                (crop.width() * crop.height() * 4) as usize,
            );
        }

        // The same at a density below one, where the effect is larger
        // relative to the region.
        for (x, expected) in [(0.0, 3), (1.0, 2)] {
            let crop = crop_for(x, 0.0, 5.0, 5.0, 0.5);
            assert_eq!(crop.width(), expected, "five wide at 0.5 from x={x}");
        }
    }

    /// Abutting reads tile the canvas exactly, at any density.
    ///
    /// This is what rounding the edges buys, and it is the reason not to
    /// replace it with a floor on the size: a shared edge rounds to one
    /// integer from both sides, so two neighbouring reads meet with no gap
    /// and no overlap. Flooring each region's size independently loses that
    /// -- the halves of an eight-wide canvas at 1.5 would come back five and
    /// six against a whole of twelve.
    #[test]
    fn two_abutting_reads_lose_no_column_and_share_none() {
        let mut recorder = PageRecorder::new(Rect::from_wh(8.0, 8.0));
        recorder.append(|canvas, _| {
            canvas.draw_rect(Rect::from_wh(8.0, 8.0), &Paint::default());
        });

        for density in [1.5, 0.5, 1.25, 2.5] {
            let whole = crop_for(0.0, 0.0, 8.0, 8.0, density).width();
            for split in 1..8 {
                let at = split as f32;
                let left = crop_for(0.0, 0.0, at, 8.0, density).width();
                let right = crop_for(at, 0.0, 8.0 - at, 8.0, density).width();
                assert_eq!(
                    left + right,
                    whole,
                    "density {density}, split at {split}"
                );
            }
        }
    }

    /// A whole-canvas read is wider than the image the same density exports.
    ///
    /// Both are right for what they are. The export produces an image, and
    /// its dimensions are the image's; the read covers a region, and a five
    /// wide region at 1.5 spans `[0, 7.5)`, which touches eight columns. The
    /// two agree exactly when the product is already whole.
    #[test]
    fn a_whole_canvas_read_and_the_export_measure_different_things() {
        for (side, density, read_px, exported) in [
            (5.0_f32, 1.5_f32, 8, 7),
            (5.0, 0.5, 3, 2),
            (3.0, 2.5, 8, 7),
            (7.0, 1.25, 9, 8),
            // Whole product, so nothing to disagree about.
            (10.0, 1.5, 15, 15),
        ] {
            let mut recorder = PageRecorder::new(Rect::from_wh(side, side));
            let page = recorder.page();
            assert_eq!(
                crop_for(0.0, 0.0, side, side, density).width(),
                read_px,
                "{side} at {density} reads"
            );
            assert_eq!(
                page.scaled_dimensions(density).width,
                exported,
                "{side} at {density} exports"
            );
        }
    }

    #[test]
    fn going_quiet_gives_the_bitmaps_back() {
        let _shared = SHARED_CACHE.lock();
        // What a canvas JavaScript has dropped holds until V8 gets round to
        // finalizing it, which it is slow to do because the box it can see
        // is a few words wide. The idle watcher in `crate::memory` calls
        // this once rendering has stopped; here it is called directly, since
        // waiting three seconds for a thread is not what is being tested.
        let mut recorder = PageRecorder::new(Rect::from_wh(8.0, 8.0));
        recorder.append(|canvas, _| {
            canvas.draw_rect(Rect::from_wh(4.0, 4.0), &Paint::default());
        });
        let page = recorder.page();
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

    /// Frames under a clip that only read hand the next run a recording that
    /// holds nothing but their frames, and after `Spare::IDLE_FRAMES` of
    /// them it is begun afresh, so that a page read in a loop of transforms
    /// does not grow a recording no draw ever finishes.
    #[test]
    fn a_spare_recording_left_idle_is_begun_again() {
        let mut recorder = PageRecorder::new(Rect::from_wh(64.0, 64.0));
        recorder.set_clip(&Some(Path::circle((32.0, 32.0), 20.5, None)));
        let mut most = 0;
        for at in 0..(Spare::IDLE_FRAMES * 3) {
            recorder.set_matrix(Matrix::translate((at as f32, 0.0)));
            let _ = recorder.page();
            // The count travels with the recording: on the run that took
            // it, or on the spare a run handed back.
            let idle = match (&recorder.run, &recorder.spare) {
                (Some(run), _) => run.idle,
                (None, Some(spare)) => spare.idle,
                (None, None) => 0,
            };
            most = most.max(idle);
        }
        assert!(
            most > 1,
            "runs that drew nothing left a spare {most} times idle"
        );
        assert!(
            most <= Spare::IDLE_FRAMES,
            "a spare went {most} runs idle, past {}",
            Spare::IDLE_FRAMES
        );
    }

    #[test]
    fn a_retired_generation_cannot_be_filed_under() {
        let _shared = SHARED_CACHE.lock();
        // `set` creates the entry it cannot find so that a page evicted
        // while it is still being drawn can cache again. An export that
        // outlives its generation reaches the same line, and used to put
        // back the entry that retiring the generation had just removed --
        // memory no lookup could reach, freed only by the byte bound
        // evicting it again.
        let mut recorder = PageRecorder::new(Rect::from_wh(8.0, 8.0));
        recorder.append(|canvas, _| {
            canvas.draw_rect(Rect::from_wh(4.0, 4.0), &Paint::default());
        });
        let page = recorder.page();
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

    /// Reads inside one open layer leave its recording no deeper than one
    /// read does. Each read finishes what the layer has recorded; drawing
    /// that picture into the recording begun after it nested the layer one
    /// level per read, and playing it back recursed once per level -- 4,000
    /// reads in a layer overran a 1 MB stack. On a thread with too little
    /// stack for that, the reads and the read after the layer closes all
    /// return.
    #[test]
    fn reads_inside_a_layer_keep_its_recording_flat() {
        /// Small enough that a recursion of [`READS`] levels overruns it,
        /// and enough for one read of a flat recording.
        const STACK: usize = 256 * 1024;
        /// Far more reads than [`STACK`] holds levels of playback.
        const READS: usize = 1500;
        let pixel = thread::Builder::new()
            .stack_size(STACK)
            .spawn(|| {
                let mut recorder = PageRecorder::new(Rect::from_wh(8.0, 8.0));
                let crop = IRect::from_xywh(0, 0, 1, 1);
                let read = |recorder: &mut PageRecorder| {
                    recorder
                        .pixels(
                            crop,
                            ExportOptions::default(),
                            RenderingEngine::CPU,
                        )
                        .expect("a read of the page")
                };
                let mut half = Paint::default();
                half.set_alpha_f(0.5);
                recorder.open_layer(LayerRec {
                    paint: Some(half),
                    ..LayerRec::default()
                });
                for _ in 0..READS {
                    recorder.append(|canvas, _| {
                        canvas.draw_rect(
                            Rect::from_wh(1.0, 1.0),
                            &Paint::default(),
                        );
                    });
                    read(&mut recorder);
                }
                recorder.close_layer();
                read(&mut recorder)
            })
            .expect("a thread")
            .join()
            .expect("the reads return");
        assert_eq!(pixel[3], 128, "the layer composites at its alpha");
    }

    /// The page the confined-layer cases draw on.
    const CONFINED_PAGE: Rect = Rect {
        left: 0.0,
        top: 0.0,
        right: 48.0,
        bottom: 40.0,
    };

    /// The contents a confined layer is held to, each drawn in page
    /// coordinates and recorded as the recorder records them: the draws
    /// whose pixels reach past their geometry, where a layer's bounds could
    /// clip them if Skia's bounds for the draw fell short of what it marks.
    fn confined_contents() -> Vec<(&'static str, Picture)> {
        let typeface = corpus_typeface();
        let stroke = |width: f32| {
            let mut paint = Paint::default();
            paint
                .set_anti_alias(true)
                .set_style(PaintStyle::Stroke)
                .set_stroke_width(width);
            paint
        };
        type Draw = Box<dyn Fn(&SkCanvas)>;
        let photo = {
            let mut tiny =
                started::raster_n32_premul((7, 5)).expect("a raster surface");
            for (x, y) in (0..7).flat_map(|x| (0..5).map(move |y| (x, y))) {
                let mut paint = Paint::default();
                paint.set_color(Color::from_rgb(x * 36, y * 50, 128));
                tiny.canvas().draw_rect(
                    Rect::from_xywh(f32::from(x), f32::from(y), 1.0, 1.0),
                    &paint,
                );
            }
            tiny.image_snapshot()
        };
        let draws: [(&'static str, Draw); 11] = [
            (
                "an antialiased rect at fractions",
                Box::new(|canvas| {
                    let mut paint = Paint::default();
                    paint.set_anti_alias(true).set_color(Color::RED);
                    canvas.draw_rect(
                        Rect::from_xywh(10.3, 7.7, 5.2, 3.9),
                        &paint,
                    );
                }),
            ),
            (
                "a hairline",
                Box::new(move |canvas| {
                    canvas.draw_line((5.5, 5.5), (30.2, 12.7), &stroke(0.0));
                }),
            ),
            (
                "a wide mitred stroke",
                Box::new(move |canvas| {
                    let mut paint = stroke(9.0);
                    paint
                        .set_stroke_join(PaintJoin::Miter)
                        .set_stroke_miter(10.0);
                    let v = [(12.0, 30.0), (20.0, 12.0), (28.0, 30.0)]
                        .map(Point::from);
                    canvas.draw_path(
                        &Path::polygon(&v, false, None, None),
                        &paint,
                    );
                }),
            ),
            (
                "square caps",
                Box::new(move |canvas| {
                    let mut paint = stroke(6.0);
                    paint.set_stroke_cap(PaintCap::Square);
                    canvas.draw_line((14.0, 20.0), (34.0, 20.0), &paint);
                }),
            ),
            (
                "a blur past the shape",
                Box::new(|canvas| {
                    let mut paint = Paint::default();
                    paint.set_mask_filter(MaskFilter::blur(
                        BlurStyle::Normal,
                        5.0,
                        false,
                    ));
                    canvas.draw_rect(
                        Rect::from_xywh(16.0, 12.0, 10.0, 8.0),
                        &paint,
                    );
                }),
            ),
            (
                "a shadow past the shape",
                Box::new(|canvas| {
                    let mut paint = Paint::default();
                    paint.set_image_filter(image_filters::drop_shadow(
                        (14.0, 9.0),
                        (3.0, 3.0),
                        Color4f::from(Color::BLUE),
                        None,
                        None,
                        None,
                    ));
                    canvas
                        .draw_rect(Rect::from_xywh(8.0, 6.0, 8.0, 8.0), &paint);
                }),
            ),
            (
                "text",
                Box::new({
                    let typeface = typeface.clone();
                    move |canvas| {
                        let font = Font::from_typeface(typeface.clone(), 23.0);
                        canvas.draw_str(
                            "Wavy",
                            (3.0, 30.0),
                            &font,
                            &Paint::default(),
                        );
                        reach_the_clip(canvas);
                    }
                }),
            ),
            (
                "text scaled where it is recorded",
                Box::new(move |canvas| {
                    let font = Font::from_typeface(typeface.clone(), 11.5);
                    canvas.save();
                    canvas.scale((2.0, 2.0));
                    canvas.draw_str(
                        "Wavy",
                        (1.5, 15.0),
                        &font,
                        &Paint::default(),
                    );
                    canvas.restore();
                    reach_the_clip(canvas);
                }),
            ),
            (
                "a gradient",
                Box::new(|canvas| {
                    let ends = [Color::BLUE.into(), Color::RED.into()];
                    let stops = Colors::new(
                        &ends,
                        Some([0.0f32, 1.0].as_slice()),
                        TileMode::Clamp,
                        None,
                    );
                    let gradient =
                        Gradient::new(stops, Interpolation::default());
                    let mut paint = Paint::default();
                    paint.set_anti_alias(true).set_shader(
                        shaders::linear_gradient(
                            ((9.3, 0.0), (31.7, 0.0)),
                            &gradient,
                            None,
                        ),
                    );
                    canvas.draw_rect(
                        Rect::from_xywh(9.3, 7.2, 22.4, 17.9),
                        &paint,
                    );
                }),
            ),
            (
                "an image, sampled",
                Box::new(move |canvas| {
                    canvas.draw_image_rect_with_sampling_options(
                        &photo,
                        None,
                        Rect::from_xywh(11.37, 8.61, 19.3, 14.2),
                        SamplingOptions::new(
                            FilterMode::Linear,
                            MipmapMode::None,
                        ),
                        &Paint::default(),
                    );
                }),
            ),
            ("nothing", Box::new(|_| {})),
        ];
        draws
            .into_iter()
            .map(|(name, draw)| {
                let mut recorder = PictureRecorder::new();
                draw(recorder.begin_recording(CONFINED_PAGE, true));
                let picture = recorder
                    .finish_recording_as_picture(None)
                    .expect("a picture of the contents");
                (name, picture)
            })
            .collect()
    }

    /// `contents` drawn in a [`Group`] opened by `rec` under `matrix` and
    /// `clip`, as `PageRecorder::close_layer` files it, confined to `within`,
    /// over a page of two colours at `density`: the bytes of the page.
    fn composited(
        rec: &LayerRec,
        contents: &Picture,
        within: Play,
        (matrix, clip): (&Matrix, Option<&Path>),
        density: f32,
        engine: RenderingEngine,
    ) -> Vec<u8> {
        let group = Arc::new(Group {
            composite: Composite::Layer(rec.clone()),
            matrix: *matrix,
            clip: clip.map(Path::serialize),
            within: None,
            contents_matrix: Matrix::new_identity(),
            contents: Arc::new(vec![Layer::Picture(contents.clone())]),
        });
        let size = CONFINED_PAGE.size() * density;
        let info = ImageInfo::new_n32_premul(
            (size.width as i32, size.height as i32),
            None,
        );
        let mut surface = engine
            .make_surface(&info, &ExportOptions::default())
            .expect("a surface");
        let canvas = surface.canvas();
        canvas.clear(Color::from_rgb(40, 150, 220));
        canvas.scale((density, density));
        let mut green = Paint::default();
        green.set_color(Color::from_rgb(60, 200, 90));
        canvas.draw_rect(Rect::from_xywh(0.0, 20.0, 48.0, 20.0), &green);
        match within {
            Play::Within(within) => {
                group.playback_within(canvas, Target::Pixels, within)
            }
            Play::AsPlayed => group.playback(canvas, Target::Pixels),
        }
        let mut pixels = vec![0; info.compute_min_byte_size()];
        assert!(
            surface.read_pixels(
                &info,
                &mut pixels,
                info.min_row_bytes(),
                (0, 0)
            ),
            "the surface's pixels"
        );
        pixels
    }

    /// How [`composited`] plays its group back: confined to a rectangle, or
    /// not, or as [`Group::playback`] chooses.
    #[derive(Clone, Copy)]
    enum Play {
        Within(Option<Rect>),
        AsPlayed,
    }

    /// A layer confined to what it drew composites to the bytes of the same
    /// layer covering the page, for every draw that reaches past its own
    /// geometry -- an antialiased edge, a hairline, a mitre, a square cap, a
    /// blur, a shadow, a glyph -- under a turned and fractionally shifted
    /// transform and under a round clip, with bounds of the layer's own,
    /// and at density 2.
    #[test]
    fn a_confined_layer_composites_as_one_covering_the_page() {
        let mut half = Paint::default();
        half.set_alpha_f(0.5);
        let recs = [
            (
                "at alpha 0.5",
                LayerRec {
                    paint: Some(half.clone()),
                    ..LayerRec::default()
                },
            ),
            (
                "with bounds of its own",
                LayerRec {
                    paint: Some(half),
                    bounds: Some(Rect::from_xywh(14.0, 8.0, 26.0, 30.0)),
                    ..LayerRec::default()
                },
            ),
            ("with no paint", LayerRec::default()),
        ];
        let mut turned = Matrix::rotate_deg_pivot(30.0, (24.0, 20.0));
        turned.post_translate((0.37, 0.61));
        let round = Path::circle((22.0, 18.0), 12.0, None);
        let frames = [
            ("untransformed", (&Matrix::default(), None)),
            ("turned and shifted", (&turned, None)),
            ("clipped round", (&Matrix::default(), Some(&round))),
        ];
        let mut wrong = Vec::new();
        let engines = match RenderingEngine::default() {
            RenderingEngine::GPU => {
                vec![RenderingEngine::CPU, RenderingEngine::GPU]
            }
            RenderingEngine::CPU => vec![RenderingEngine::CPU],
        };
        for (drawn, contents) in confined_contents() {
            for ((layer, rec), engine) in recs
                .iter()
                .flat_map(|rec| engines.iter().map(move |e| (rec, *e)))
            {
                let within = rec
                    .footprint(reach_of(&[Layer::Picture(contents.clone())]));
                assert!(within.is_some(), "{drawn} {layer}: confined");
                for (frame, at) in frames {
                    for density in [1.0, 2.0] {
                        let covering = composited(
                            rec,
                            &contents,
                            Play::Within(None),
                            at,
                            density,
                            engine,
                        );
                        let confined = composited(
                            rec,
                            &contents,
                            Play::AsPlayed,
                            at,
                            density,
                            engine,
                        );
                        if covering != confined {
                            wrong.push(format!(
                                "{drawn}, {layer}, {frame}, density {density}, {engine:?}"
                            ));
                        }
                    }
                }
            }
        }
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));

        // The filter cases can fail: confined to the shape alone rather
        // than to what Skia says it draws, the blur and the shadow lose
        // what they spread past it.
        let shapes = [
            (
                "a blur past the shape",
                Rect::from_xywh(16.0, 12.0, 10.0, 8.0),
            ),
            (
                "a shadow past the shape",
                Rect::from_xywh(8.0, 6.0, 8.0, 8.0),
            ),
        ];
        let contents = confined_contents();
        let (_, rec) = &recs[0];
        for (drawn, shape) in shapes {
            let (_, picture) = contents
                .iter()
                .find(|(name, _)| *name == drawn)
                .expect("the filter case");
            let at = (&Matrix::default(), None);
            assert_ne!(
                composited(
                    rec,
                    picture,
                    Play::Within(Some(shape)),
                    at,
                    1.0,
                    RenderingEngine::CPU
                ),
                composited(
                    rec,
                    picture,
                    Play::Within(None),
                    at,
                    1.0,
                    RenderingEngine::CPU
                ),
                "{drawn}: confined to its shape"
            );
        }
    }

    /// Text in a layer, however it is drawn, reaches as far as the clip it is
    /// drawn under: `fillText`, `strokeText` and a paragraph each leave the
    /// layer reaching the page -- see [`reach_the_clip`] -- where a
    /// rectangle in a layer reaches only the rectangle.
    #[test]
    fn text_in_a_layer_reaches_the_clip_it_is_drawn_under() {
        let side = 64.0;
        let reach = |draw: &dyn Fn(&mut crate::Context2D)| {
            let mut canvas = crate::Canvas::new(side, side);
            let ctx = canvas.context();
            ctx.save_layer_with(0.5, None, None);
            draw(ctx);
            ctx.restore();
            let mut reach = None;
            canvas.context().inner.with_recorder(|mut recorder| {
                reach =
                    recorder.page().layers.iter().find_map(
                        |layer| match layer {
                            Layer::Group(group) => Some(group.reach()),
                            _ => None,
                        },
                    );
            });
            reach.expect("the layer, filed as a group")
        };
        assert_eq!(
            reach(&|ctx| ctx.fill_rect(10.0, 10.0, 5.0, 5.0)),
            Some(Rect::from_xywh(10.0, 10.0, 5.0, 5.0)),
            "a rectangle"
        );
        type Text = fn(&mut crate::Context2D);
        let texts: [(&str, Text); 3] = [
            ("fillText", |ctx| ctx.fill_text("Wavy", 4.0, 30.0, None)),
            ("strokeText", |ctx| ctx.stroke_text("Wavy", 4.0, 30.0, None)),
            ("a paragraph", |ctx| {
                let engine = crate::TextEngine::new(&crate::FontLibrary::new());
                let mut builder =
                    engine.paragraph_builder(&crate::TextStyle::default());
                builder.add_text("Wavy");
                ctx.draw_paragraph(&builder.build(60.0), 4.0, 10.0);
            }),
        ];
        for (name, draw) in texts {
            assert_eq!(reach(&draw), Some(Rect::from_wh(side, side)), "{name}");
        }
    }

    /// A group played into a recording is left covering its clip, so that
    /// it composites as it would anywhere the recording is drawn: a group
    /// with bounds of its own, recorded untransformed and drawn turned, as
    /// a pattern of a canvas is, reads as the same group drawn turned
    /// straight onto the page.
    #[test]
    fn a_group_recorded_for_later_is_not_confined() {
        let mut half = Paint::default();
        half.set_alpha_f(0.5);
        let mut turned = Matrix::rotate_deg_pivot(30.0, (24.0, 20.0));
        turned.post_translate((0.37, 0.61));
        for (drawn, contents) in confined_contents() {
            let group = Arc::new(Group {
                composite: Composite::Layer(LayerRec {
                    paint: Some(half.clone()),
                    bounds: Some(Rect::from_xywh(14.0, 8.0, 26.0, 30.0)),
                    ..LayerRec::default()
                }),
                matrix: Matrix::new_identity(),
                clip: None,
                within: None,
                contents_matrix: Matrix::new_identity(),
                contents: Arc::new(vec![Layer::Picture(contents)]),
            });
            let mut recorder = PictureRecorder::new();
            group.playback(
                recorder.begin_recording(CONFINED_PAGE, true),
                Target::Pixels,
            );
            let recorded = recorder
                .finish_recording_as_picture(None)
                .expect("a picture of the group");
            let page = |draw: &dyn Fn(&SkCanvas)| {
                let mut surface = started::raster_n32_premul((48, 40))
                    .expect("a raster surface");
                let canvas = surface.canvas();
                canvas.clear(Color::WHITE);
                canvas.concat(&turned);
                draw(canvas);
                let pixmap = surface.peek_pixels().expect("the pixels");
                pixmap.bytes().expect("the bytes").to_vec()
            };
            assert!(
                page(&|canvas| {
                    canvas.draw_picture(&recorded, None, None);
                }) == page(&|canvas| {
                    group.playback_within(canvas, Target::Pixels, None)
                }),
                "{drawn}"
            );
        }
    }

    /// A group in a document is written as it was, covering its clip: a
    /// PDF of a group holding a square is the PDF the group writes with no
    /// rectangle to keep to.
    #[test]
    fn a_group_in_a_document_is_not_confined() {
        let mut half = Paint::default();
        half.set_alpha_f(0.5);
        let mut recorder = PictureRecorder::new();
        recorder.begin_recording(CONFINED_PAGE, true).draw_rect(
            Rect::from_xywh(16.0, 12.0, 10.0, 8.0),
            &Paint::default(),
        );
        let group = Arc::new(Group {
            composite: Composite::Layer(LayerRec {
                paint: Some(half),
                ..LayerRec::default()
            }),
            matrix: Matrix::new_identity(),
            clip: None,
            within: None,
            contents_matrix: Matrix::new_identity(),
            contents: Arc::new(vec![Layer::Picture(
                recorder
                    .finish_recording_as_picture(None)
                    .expect("a picture of the square"),
            )]),
        });
        let pdf = |draw: &dyn Fn(&SkCanvas)| {
            let mut bytes = Vec::new();
            let document = pdf::new_document(&mut bytes, None);
            let mut page = document.begin_page(CONFINED_PAGE.size(), None);
            draw(page.canvas());
            page.end_page().close();
            bytes
        };
        assert!(
            pdf(&|canvas| group.playback(canvas, Target::Document))
                == pdf(&|canvas| group.playback_within(
                    canvas,
                    Target::Document,
                    None
                )),
            "the PDF of the group"
        );
    }

    /// A layer whose transparent pixels can mark the page covers the clip:
    /// through a colour filter, an image filter, a backdrop or a blend other
    /// than source-over. One that can confine composites with nothing more
    /// than an alpha.
    #[test]
    fn only_a_layer_that_leaves_the_page_where_it_is_transparent_is_confined() {
        let painted = |change: fn(&mut Paint)| {
            let mut paint = Paint::default();
            change(&mut paint);
            LayerRec {
                paint: Some(paint),
                ..LayerRec::default()
            }
        };
        let drawn = Some(Rect::from_xywh(1.0, 2.0, 3.0, 4.0));
        let unconfined = [
            (
                "a colour filter",
                painted(|paint| {
                    paint.set_color_filter(color_filters::blend(
                        Color::RED,
                        BlendMode::DstOver,
                    ));
                }),
            ),
            (
                "an image filter",
                painted(|paint| {
                    paint.set_image_filter(image_filters::blur(
                        (2.0, 2.0),
                        None,
                        None,
                        None,
                    ));
                }),
            ),
            (
                "a copy",
                painted(|paint| {
                    paint.set_blend_mode(BlendMode::Src);
                }),
            ),
            (
                "destination-in",
                painted(|paint| {
                    paint.set_blend_mode(BlendMode::DstIn);
                }),
            ),
            (
                "a backdrop",
                LayerRec {
                    backdrop: image_filters::blur((2.0, 2.0), None, None, None),
                    ..LayerRec::default()
                },
            ),
        ];
        for (name, rec) in unconfined {
            assert_eq!(rec.footprint(drawn), None, "{name}");
        }
        let confined = painted(|paint| {
            paint.set_alpha_f(0.25);
        });
        assert_eq!(confined.footprint(drawn), drawn, "an alpha alone");
        let nothing = Some(Rect::new_empty());
        assert_eq!(confined.footprint(nothing), nothing, "nothing drawn");
        assert_eq!(confined.footprint(None), None, "what it draws not known");
    }

    /// What a group's entries draw in: a picture's cull rectangle, the
    /// union of them, a nested group's own entries through the transform
    /// they are played under, and nothing known where a blurred draw or a
    /// group that covers the clip is among them.
    #[test]
    fn a_group_reaches_as_far_as_its_entries_draw() {
        let picture = |rect: Rect| {
            let mut recorder = PictureRecorder::new();
            recorder
                .begin_recording(CONFINED_PAGE, true)
                .draw_rect(rect, &Paint::default());
            Layer::Picture(
                recorder
                    .finish_recording_as_picture(None)
                    .expect("a picture of the rectangle"),
            )
        };
        let group = |rec: LayerRec, contents_matrix: Matrix, contents| {
            Layer::Group(Arc::new(Group {
                composite: Composite::Layer(rec),
                matrix: Matrix::new_identity(),
                clip: None,
                within: None,
                contents_matrix,
                contents: Arc::new(contents),
            }))
        };
        let (a, b) = (
            Rect::from_xywh(2.0, 3.0, 4.0, 5.0),
            Rect::from_xywh(10.0, 1.0, 2.0, 2.0),
        );
        let mut both = a;
        both.join(b);
        assert_eq!(reach_of(&[]), Some(Rect::new_empty()), "nothing");
        assert_eq!(reach_of(&[picture(a), picture(b)]), Some(both), "two");
        let moved = Matrix::translate((20.0, 0.0));
        let inner = group(LayerRec::default(), moved, vec![picture(a)]);
        assert_eq!(
            reach_of(&[picture(b), inner]),
            Some(Rect::from_xywh(10.0, 1.0, 16.0, 7.0)),
            "a nested group, moved"
        );
        let blurred = Layer::Blur(Arc::new(BlurOp::start(
            blurred_square(0.0),
            Matrix::new_identity(),
            None,
        )));
        assert_eq!(reach_of(&[picture(a), blurred]), None, "a blur");
        let copy = LayerRec {
            backdrop: image_filters::blur((2.0, 2.0), None, None, None),
            ..LayerRec::default()
        };
        let covering = group(copy, Matrix::new_identity(), vec![picture(a)]);
        assert_eq!(reach_of(&[picture(b), covering]), None, "a backdrop");
    }

    /// Ten thousand reads inside one layer, each after a draw, leave the
    /// layer's recording one picture per read, each holding that read's draw
    /// and nothing of the reads before it. Drawn in a corner the reads never
    /// look at, so that Skia rejects each picture by its bounds and the
    /// reads stay cheap.
    #[test]
    fn ten_thousand_reads_inside_a_layer_keep_each_draw_its_own_picture() {
        /// Many reads, for a pin on depth rather than on stack.
        const READS: usize = 10_000;
        let mut recorder = PageRecorder::new(Rect::from_wh(8.0, 8.0));
        let crop = IRect::from_xywh(7, 7, 1, 1);
        recorder.open_layer(LayerRec::default());
        for _ in 0..READS {
            recorder.append(|canvas, _| {
                canvas.draw_rect(Rect::from_wh(1.0, 1.0), &Paint::default());
            });
            recorder
                .pixels(crop, ExportOptions::default(), RenderingEngine::CPU)
                .expect("a read of the page");
        }
        let entries = &recorder.open[0].entries;
        assert_eq!(entries.len(), READS, "one picture per read");
        // The frame the draw is recorded under, and the draw: as many
        // operations in the last picture as in the first.
        let ops: BTreeSet<_> = entries
            .iter()
            .map(|entry| match entry {
                Layer::Picture(picture) => Some(picture.approximate_op_count()),
                _ => None,
            })
            .collect();
        assert_eq!(
            ops.len(),
            1,
            "every picture holds one read's draw: {ops:?}"
        );
    }

    /// A recorder with `depth` layers open at 0.5, each opened under `clip`
    /// and drawn in, a read of a corner taken after each draw.
    fn read_in_layers(
        side: f32,
        depth: usize,
        clip: Option<Path>,
    ) -> PageRecorder {
        let mut recorder = PageRecorder::new(Rect::from_wh(side, side));
        recorder.set_clip(&clip);
        let mut half = Paint::default();
        half.set_alpha_f(0.5);
        for at in 0..depth {
            recorder.open_layer(LayerRec {
                paint: Some(half.clone()),
                ..LayerRec::default()
            });
            recorder.append(|canvas, _| {
                canvas.draw_rect(
                    Rect::from_xywh(at as f32, 1.5, 3.25, 2.0),
                    &Paint::default(),
                );
            });
            recorder
                .pixels(
                    IRect::from_xywh(0, 0, 4, 4),
                    ExportOptions::default(),
                    RenderingEngine::CPU,
                )
                .expect("a read of the page");
        }
        recorder
    }

    /// Reads inside layers keep a raster of each open layer, drawn as far as
    /// it has been: under no clip, and under a clip on whole pixels.
    #[test]
    fn reads_inside_layers_keep_a_raster_of_each() {
        let square = Path::rect(Rect::from_xywh(1.0, 0.0, 6.0, 7.0), None);
        for clip in [None, Some(square)] {
            let recorder = read_in_layers(8.0, 3, clip.clone());
            let rasters = &recorder.surface.layers;
            assert_eq!(rasters.len(), 3, "{clip:?}: one raster a layer");
            for (raster, layer) in rasters.iter().zip(&recorder.open) {
                assert_eq!(raster.id, layer.id, "{clip:?}: the layer's own");
                assert_eq!(
                    raster.drawn,
                    layer.entries.len(),
                    "{clip:?}: drawn as far as the layer"
                );
            }
        }
    }

    /// Reads inside layers keep no raster where it would not composite as
    /// the layer closes: under a clip that covers pixels by part, and past
    /// [`LAYER_RASTER_BUDGET`] -- seventeen layers on a 2048x2048 page, at
    /// 16 MB a raster -- where the read replays the layers instead.
    #[test]
    fn reads_inside_layers_replay_what_a_raster_cannot_hold() {
        let round = Path::circle((4.0, 4.0), 3.5, None);
        let recorder = read_in_layers(8.0, 2, Some(round));
        assert!(recorder.surface.layers.is_empty(), "under a round clip");
        let side = 2048.0;
        let over = LAYER_RASTER_BUDGET / (side * side * 4.0) as usize + 1;
        let mut recorder = read_in_layers(side, over, None);
        assert!(recorder.surface.layers.is_empty(), "past the budget");
        let crop = IRect::from_xywh(0, 0, 24, 4);
        let read = |recorder: &mut PageRecorder| {
            recorder
                .pixels(crop, ExportOptions::default(), RenderingEngine::CPU)
                .expect("a read of the page")
        };
        let inside = read(&mut recorder);
        for _ in 0..over {
            recorder.close_layer();
        }
        assert_eq!(inside, read(&mut recorder), "replayed as they close");
    }

    /// A layer's raster goes when the layer closes, and a layer opened where
    /// it was is drawn into a raster of its own: what a read inside it finds
    /// is what it drew, not what the one before it did.
    #[test]
    fn a_layer_opened_where_one_closed_reads_its_own() {
        let mut recorder = read_in_layers(8.0, 2, None);
        recorder.close_layer();
        assert_eq!(recorder.surface.layers.len(), 1, "the closed one goes");
        let mut half = Paint::default();
        half.set_alpha_f(0.5);
        recorder.open_layer(LayerRec {
            paint: Some(half),
            ..LayerRec::default()
        });
        recorder.append(|canvas, _| {
            canvas.draw_rect(
                Rect::from_xywh(5.0, 5.0, 2.0, 2.0),
                &Paint::default(),
            );
        });
        let crop = IRect::from_xywh(0, 0, 8, 8);
        let read = |recorder: &mut PageRecorder| {
            recorder
                .pixels(crop, ExportOptions::default(), RenderingEngine::CPU)
                .expect("a read of the page")
        };
        let inside = read(&mut recorder);
        assert_eq!(recorder.surface.layers.len(), 2, "a raster of its own");
        recorder.close_layer();
        recorder.close_layer();
        assert!(recorder.surface.layers.is_empty(), "both go");
        assert_eq!(inside, read(&mut recorder), "inside and after");
    }

    /// Reads inside a layer with nothing drawn between them keep nothing
    /// more of it: a read files what the layer recorded since the last one,
    /// and an empty recording is not kept.
    #[test]
    fn reads_with_nothing_drawn_between_add_nothing_to_a_layer() {
        let mut recorder = PageRecorder::new(Rect::from_wh(8.0, 8.0));
        let crop = IRect::from_xywh(0, 0, 1, 1);
        let read = |recorder: &mut PageRecorder| {
            recorder
                .pixels(crop, ExportOptions::default(), RenderingEngine::CPU)
                .expect("a read of the page");
        };
        recorder.open_layer(LayerRec::default());
        recorder.append(|canvas, _| {
            canvas.draw_rect(Rect::from_wh(1.0, 1.0), &Paint::default());
        });
        // A layer inside, so that the recording around it is filed too.
        recorder.open_layer(LayerRec::default());
        read(&mut recorder);
        let kept = |recorder: &PageRecorder| {
            recorder
                .open
                .iter()
                .map(|layer| layer.entries.len())
                .collect::<Vec<_>>()
        };
        let once = kept(&recorder);
        for _ in 0..3 {
            read(&mut recorder);
        }
        assert_eq!(kept(&recorder), once);
        assert_eq!(once, [1, 0], "the outer layer's fill is kept once");
    }

    #[test]
    fn a_read_inside_a_layer_commits_nothing() {
        let _shared = SHARED_CACHE.lock();
        // What the surface and the page cache hold is the filed page and
        // nothing of a layer still open: a read or an export inside one
        // composites the layer over a copy, and neither moves the depth
        // either has committed to. Closing the layer is what files it.
        // Wider than one tile, so that a read is served by the whole page.
        let mut recorder = PageRecorder::new(Rect::from_wh(600.0, 300.0));
        let square = |canvas: &SkCanvas, _: Recording| {
            canvas.draw_rect(Rect::from_wh(4.0, 4.0), &Paint::default());
        };
        recorder.append(square);
        let opts = ExportOptions::default();
        let crop = IRect::from_xywh(0, 0, 600, 300);
        let read = |recorder: &mut PageRecorder| {
            recorder
                .pixels(crop, opts.clone(), RenderingEngine::CPU)
                .expect("a read of the page");
        };
        // The depth the page cache holds for the page, after an export has
        // offered it what the surface holds.
        let cached = |recorder: &mut PageRecorder| {
            recorder.page_for_export(&opts, &RenderingEngine::CPU);
            PageCache::shared()
                .get(&recorder.id.0)
                .map(|entry| entry.depth)
        };
        read(&mut recorder);
        let committed = (recorder.surface.depth, cached(&mut recorder));
        assert_eq!(committed, (1, Some(1)), "the filed page is committed");

        let mut half = Paint::default();
        half.set_alpha_f(0.5);
        recorder.open_layer(LayerRec {
            paint: Some(half),
            ..LayerRec::default()
        });
        for _ in 0..2 {
            recorder.append(square);
            read(&mut recorder);
            assert_eq!(
                (recorder.surface.depth, cached(&mut recorder)),
                committed,
                "a read and an export inside the layer"
            );
        }

        recorder.close_layer();
        read(&mut recorder);
        assert!(
            recorder.surface.depth > committed.0,
            "the closed layer is filed and read"
        );
    }

    #[test]
    fn a_page_inside_a_layer_files_the_page_beneath_it() {
        let _shared = SHARED_CACHE.lock();
        // An export inside a layer files the page beneath the layer, at its
        // depth, under its identity -- not the page it was taken as, which
        // nothing keeps -- and the entry goes when the canvas does, as every
        // entry of its generation does.
        let mut recorder = PageRecorder::new(Rect::from_wh(8.0, 8.0));
        recorder.append(|canvas, _| {
            canvas.draw_rect(Rect::from_wh(4.0, 4.0), &Paint::default());
        });
        let filed = recorder.page();
        let (id, depth) = (filed.id, filed.depth());
        recorder.open_layer(LayerRec::default());
        // Translucent and beside the square, so that drawing it twice shows.
        recorder.append(|canvas, _| {
            let mut half = Paint::default();
            half.set_alpha_f(0.5);
            canvas.draw_rect(Rect::from_xywh(2.0, 2.0, 6.0, 6.0), &half);
        });
        let page = recorder.page();
        let opts = ExportOptions {
            format: ImageFormat::Png,
            ..ExportOptions::default()
        };
        let first = page
            .composite(&opts, RenderingEngine::CPU)
            .expect("a raster composite of an eight-pixel page");
        // The next composite starts from that entry, and comes out as the
        // first did: an entry that changed as the open layer drew over it
        // would hold the layer twice.
        let again = recorder.page();
        let (_, started) = again
            .composited_surface(&opts, RenderingEngine::CPU)
            .expect("a raster composite of an eight-pixel page");
        assert_eq!(
            started, depth,
            "the next page inside the layer starts from it"
        );
        let second = again
            .composite(&opts, RenderingEngine::CPU)
            .expect("a raster composite of an eight-pixel page");
        let bytes = |image: &SkImage| {
            let info = image.image_info();
            let mut pixels = vec![0; info.compute_min_byte_size()];
            assert!(image.read_pixels(
                info,
                &mut pixels,
                info.min_row_bytes(),
                (0, 0),
                CachingHint::Disallow,
            ));
            pixels
        };
        assert!(bytes(&first) == bytes(&second), "both composites alike");
        let held = |id: usize| {
            PageCache::shared()
                .get(&id)
                .map(|entry| (entry.image.is_some(), entry.depth))
        };
        assert_eq!(held(id), Some((true, depth)), "the page beneath is cached");
        assert_eq!(
            held(page.id),
            None,
            "the page inside the layer files nothing"
        );

        drop((page, again, filed, recorder));
        assert_eq!(held(id), None, "the entry goes with the canvas");
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

    /// A blurred black 10px square at `x`, as `Context2D::draw_path` hands
    /// one to the recorder.
    fn blurred_square(x: f32) -> BlurDraw {
        let path = Path::rect(Rect::from_xywh(x, 0.0, 10.0, 10.0), None);
        let mut paint = Paint::default();
        paint.set_color(Color::BLACK);
        BlurDraw::of(&path, path.is_rect(), &paint, 4.0, Faint::Dropped)
    }

    /// Blurred squares inside an open `saveLayer` are filed among its
    /// contents as ops, so that their blur is made where the page is played
    /// back, and join a run there as they do on the page; a plain draw
    /// between two ends the run.
    #[test]
    fn a_blur_op_in_a_layer_is_filed_among_its_contents() {
        let features = VectorFeatures::default();
        let entries = |between: bool| {
            let mut recorder = PageRecorder::new(Rect::from_wh(100.0, 100.0));
            recorder.open_layer(LayerRec::default());
            for at in [0.0, 20.0] {
                recorder.append_blur(
                    blurred_square(at),
                    Matrix::new_identity(),
                    None,
                    features,
                );
                if between {
                    recorder.append(|canvas, _| {
                        canvas.draw_rect(
                            Rect::from_xywh(50.0, 50.0, 5.0, 5.0),
                            &Paint::default(),
                        );
                    });
                }
            }
            recorder.file_current();
            let kinds: Vec<&str> = recorder.open[0]
                .entries
                .iter()
                .map(|entry| match entry {
                    Layer::Picture(_) => "picture",
                    Layer::Blur(_) => "blur",
                    Layer::Group(_) => "group",
                    Layer::Targeted(_) => "pair",
                    Layer::Cut(_) => "cut",
                    Layer::Clipped(_) => "clipped",
                })
                .collect();
            kinds.join(" ")
        };
        assert_eq!(entries(false), "blur", "the run is joined");
        assert_eq!(entries(true), "blur picture blur picture");
    }

    /// How many layers `recorder` holds after two blurred squares in one
    /// frame, the first one's layer held elsewhere between them where
    /// `held`, as a page snapshot would hold it.
    fn layers_after_two_squares(held: bool) -> usize {
        let mut recorder = PageRecorder::new(Rect::from_wh(100.0, 100.0));
        let features = VectorFeatures::default();
        recorder.append_blur(
            blurred_square(0.0),
            Matrix::new_identity(),
            None,
            features,
        );
        let holder = held.then(|| recorder.layers.last().cloned());
        recorder.append_blur(
            blurred_square(20.0),
            Matrix::new_identity(),
            None,
            features,
        );
        drop(holder);
        recorder.layers.len()
    }

    /// A blurred square joins the run before it, unless something else
    /// holds that run's op: then it is drawn as an op of its own rather than
    /// grown into what the holder was given, or dropped.
    #[test]
    fn a_blur_op_held_elsewhere_is_not_grown() {
        assert_eq!(layers_after_two_squares(false), 1, "the run is joined");
        assert_eq!(layers_after_two_squares(true), 2, "a new op is opened");
    }

    /// A recorder that has drawn and been read `reads` times, as a canvas
    /// read every frame and never cleared is: one layer a read.
    fn read_every_draw(reads: usize) -> PageRecorder {
        let mut recorder = PageRecorder::new(Rect::from_wh(100.0, 100.0));
        for at in 0..reads {
            recorder.append(|canvas, _| {
                canvas.draw_rect(
                    Rect::from_xywh((at % 90) as f32, 0.0, 10.0, 10.0),
                    &Paint::default(),
                );
            });
            recorder.page();
        }
        recorder
    }

    /// Taking a page copies no layer, however many the page has: two pages
    /// taken in a row, and the recorder itself, hold one list. A copy per
    /// read made a canvas read every frame slower with every frame.
    #[test]
    fn taking_a_page_copies_no_layer() {
        for reads in [10, 10_000] {
            let mut recorder = read_every_draw(reads);
            let (first, second) = (recorder.page(), recorder.page());
            assert_eq!(first.layers.len(), reads, "{reads}: a layer a read");
            assert!(
                ptr::eq(first.layers.as_ptr(), second.layers.as_ptr())
                    && ptr::eq(first.layers.as_ptr(), recorder.layers.as_ptr()),
                "{reads} layers: copied"
            );
        }
    }

    /// A page keeps what it was given while the recorder draws on, and a
    /// page taken after sees the new draw.
    #[test]
    fn a_page_keeps_what_it_was_given() {
        let mut recorder = read_every_draw(3);
        let before = recorder.page();
        recorder.append(|canvas, _| {
            canvas.draw_rect(Rect::from_wh(5.0, 5.0), &Paint::default());
        });
        let after = recorder.page();
        assert_eq!(before.layers.len(), 3, "the page taken before");
        assert_eq!(after.layers.len(), 4, "the page taken after");
        assert_eq!(before.features.len(), 3, "its features, before");
        assert_eq!(after.features.len(), 4, "its features, after");
    }

    /// Draws a square at `at` into `recorder` and closes the layer it is
    /// in, as the next read would.
    fn draw_and_close(recorder: &mut PageRecorder, at: usize) {
        recorder.append(|canvas, _| {
            canvas.draw_rect(
                Rect::from_xywh((at % 90) as f32, 10.0, 10.0, 10.0),
                &Paint::default(),
            );
        });
        recorder.flush();
    }

    /// While a page is held, the first draw after it copies the recorder's
    /// list once, and the draws after that grow the copy in place: the
    /// list is copied once per page held, never once per draw.
    #[test]
    fn a_held_page_costs_one_copy() {
        let mut recorder = read_every_draw(5);
        let held = recorder.page();
        assert!(Arc::ptr_eq(&held.layers, &recorder.layers), "shared");
        draw_and_close(&mut recorder, 0);
        let copy = Arc::as_ptr(&recorder.layers);
        assert!(!Arc::ptr_eq(&held.layers, &recorder.layers), "copied");
        for at in 1..4 {
            draw_and_close(&mut recorder, at);
            assert!(
                ptr::eq(copy, Arc::as_ptr(&recorder.layers)),
                "draw {at} after the first copied again"
            );
        }
        assert_eq!(held.layers.len(), 5, "the held page");
        assert_eq!(recorder.layers.len(), 9, "the recorder");
    }

    /// A window's renderer holds frame N while frame N + 1 is drawn: the
    /// lists alive are frame N's and the recorder's, two however many
    /// draws frame N + 1 takes, and once frame N is let go and N + 1 taken
    /// the recorder shares its list with N + 1 alone.
    #[test]
    fn a_frame_held_while_the_next_is_drawn_keeps_two_lists() {
        let mut recorder = read_every_draw(3);
        let frame = recorder.page();
        for at in 0..20 {
            draw_and_close(&mut recorder, at);
        }
        assert_eq!(Arc::strong_count(&frame.layers), 1, "frame N alone");
        assert_eq!(Arc::strong_count(&recorder.layers), 1, "the recorder");
        drop(frame);
        let next = recorder.page();
        assert!(Arc::ptr_eq(&next.layers, &recorder.layers), "N + 1 shares");
        assert_eq!(Arc::strong_count(&recorder.layers), 2, "and no other");
    }
}
