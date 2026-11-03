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
    iter,
    marker::PhantomData,
    sync::{Arc, OnceLock},
};

use parking_lot::Mutex;
use skia_safe::{
    AlphaType, BlendMode, BlurStyle, Canvas as SkCanvas, ClipOp, Color4f,
    ColorFilter, ColorSpace, ColorType, Contains, Data, FilterMode, IRect,
    Image, ImageFilter, ImageInfo, MaskFilter, Matrix, Paint, PaintCap,
    PaintJoin, PaintStyle, Path, PathDirection, Picture, PictureRecorder,
    Point, Rect, RoundOut, RuntimeEffect, SamplingOptions, Shader, Surface,
    TileMode, canvas::SaveLayerRec, color_filters, image_filters,
};

use crate::{
    cache_map::KeptSets,
    context::{
        blur_mask,
        chain::{self, Depth, Step},
        chain_cache, coverage_of,
        faint::{Faint, composited},
        page::{ExportOptions, Page},
    },
    export::{VectorFeatures, blend_features},
    gpu::{RenderingEngine, pacing},
    node::{
        filter::MASK_BLUR_MAX_SIGMA,
        typography::RunPaint,
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

#[cfg(test)]
thread_local! {
    /// Whether a chain's layer is bounded by the text it holds where it
    /// says what it can ink: off, it is bounded as any other group's, by
    /// what its entries reach, which for text is the clip -- the reference
    /// a confined layer is held to.
    pub(crate) static KEEPS_TEXT_REACH: Cell<bool> = const { Cell::new(true) };
    /// The rectangle, in device pixels, the last chain played on this thread
    /// was given to bound what it filters, where it was given one.
    pub(crate) static CHAIN_CONTENT: Cell<Option<Rect>> = const { Cell::new(None) };
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
/// than it was recorded at can ink past its reach rounded out to whole
/// pixels -- see `RunPaint::reach` -- measured, not chosen, by
/// `text_in_a_chain_is_bounded_by_all_it_inks` with TEXT_REACH_SWEEP=full:
/// text at 8 to 96px, filled, hairline, stroked 1, 12 and 40 wide under each
/// join, bold, spaced, upright, turned, skewed and scaled, under a blur and two
/// drop shadows, each run laid out afresh and kept, at densities 1 to 3 and
/// drawn into another canvas at 0.5 and 1.5.
///
/// - On CoreText, in Oswald, Helvetica, Times New Roman, Zapfino, Apple Color
///   Emoji and Monoton, 98 of 25,600 playbacks inked past the reach with no
///   margin, by at most 1.29 pixels.
/// - On FreeType, on the Linux runner, in Oswald, DejaVu Sans, Noto Color Emoji
///   and Monoton, 142 of 16,640 did, by at most 1.70: DejaVu Sans's 13px "fj
///   Wq", scaled 1.7 and stroked 40, at density 1.
///
/// None inked past the reach rounded out and widened by this, and the worst
/// on each are strokes whose join half their width bounds exactly: the
/// stroke's anti-aliased fringe. On CoreText that case is held by
/// `a_wide_round_stroke_inks_a_pixel_past_its_reach`.
pub(crate) const HINTED_INK_DRIFT: f32 = 1.0;

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

/// 2^64 divided by the golden ratio, rounded to odd: Knuth's multiplier for
/// hashing by multiplication (The Art of Computer Programming, vol. 3,
/// section 6.4), whose product carries every bit of a key into its top
/// bits, which [`verdict_slot`] takes.
const GOLDEN_RATIO_64: u64 = 0x9E37_79B9_7F4A_7C15;

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
    Chain {
        steps: Arc<[Step]>,
        rec: LayerRec,
        reach: Option<Rect>,
    },
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
            (Composite::Chain { steps, rec, reach }, Target::Pixels) => {
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
                // What is drawn is bounded by `within`, or by what the
                // entries can draw in where each of them knows -- see
                // `reach_of` -- so that the chain's layer is the draw's and
                // not the page's. Bounded by a single picture alone, a draw
                // on a page that keeps a recording for pixels, which files
                // it as a pair, filtered the whole page: 50 rects under a
                // drop shadow took 1.75 s on an F16 Display P3 page against
                // 26 ms on an sRGB one.
                let to_space = |matrix: &Matrix| Matrix::concat(&back, matrix);
                let to_device = canvas.local_to_device_as_3x3();
                // UPSTREAM: skia-safe 0.156.0 -- unfiled -- not worked around
                // Re-check: cargo test skia_covers_a_curve_by_where_it_lies
                //
                // Skia's analytic anti-aliasing covers a curved edge by where
                // on the device it lies, so text whose layer is bounded by its
                // reach draws some edge pixels otherwise than in a layer
                // bounded by the clip: Times New Roman's 96px "Ag" stroked 40
                // wide with round joins, under a drop shadow at density 3, in
                // 16 pixels by up to 25 levels. Neither is the edge's
                // coverage -- 114 and 146 where 8 by 8 samples give 133.8 --
                // and no class of origin keeps it: moved 3 or 6 device pixels
                // the stroke is covered alike, moved 15 to 300 it is not, and
                // an oval flips at shifts of 1 and 2.
                let content = match (&self.within, reach) {
                    (Some(within), _) => {
                        Some(to_space(&self.matrix).map_rect(within).0)
                    }
                    (None, Some(reach)) if keeps_text_reach() => {
                        Some(hinted_reach(
                            to_space(&self.contents_matrix).map_rect(reach).0,
                            &to_device,
                        ))
                    }
                    (None, _) => reach_of(&self.contents.layers).map(|reach| {
                        to_space(&self.contents_matrix).map_rect(reach).0
                    }),
                };
                // A group holding an entry whose reach is not known -- a
                // blurred draw, or a group through a chain of its own -- would
                // be bounded by the clip alone, and that layer moves an
                // anti-aliased edge: a fill's coverage of 0.6 at an alpha of
                // 0.9 landed 0.5365 through an unblurred shadow, where it
                // makes 0.54. No group of a chain holds such an entry, so no
                // draw takes that route, and a debug build fails where one
                // does, for the route to be made exact before it is taken.
                debug_assert!(
                    content.is_some(),
                    "a chain's group holds an entry with no reach, which \
                     bounds its layer by the clip alone"
                );
                #[cfg(test)]
                let content = content
                    .filter(|_| !UNBOUNDED_CHAIN.with(std::cell::Cell::get));
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
                #[cfg(test)]
                CHAIN_CONTENT.set(content.map(|content| {
                    canvas.local_to_device_as_3x3().map_rect(content).0
                }));
                let outcome = chain::run(
                    canvas,
                    pixels.as_deref().unwrap_or(steps),
                    content,
                    Depth::Floats,
                    &paint,
                    |layer| {
                        let to_layer = layer.local_to_device_as_3x3();
                        let to_device =
                            Matrix::concat(&to_layer, &to_space(&self.matrix));
                        layer.set_matrix(&to_device.into());
                        self.confine(layer, target, &to_device);
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
                        play_entries(
                            &self.contents.layers,
                            &self.contents.features,
                            canvas,
                            target,
                        );
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
                play_entries(
                    &self.contents.layers,
                    &self.contents.features,
                    canvas,
                    target,
                );
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
pub(crate) fn play_entries(
    entries: &[Layer],
    features: &[VectorFeatures],
    canvas: &SkCanvas,
    target: Target,
) {
    debug_assert_eq!(entries.len(), features.len(), "one feature set a layer");
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
    let mut at = 0;
    while at < entries.len() {
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

/// Whether `entries`, or a group among them, hold a pair of recordings --
/// one for pixels and one for documents, which a page that keeps a
/// recording for pixels files -- so that a picture of the page played for
/// one target does not stand for the other: [`Page::picture`]'s holds the
/// colours placed for pixels.
pub(crate) fn holds_a_pair(entries: &[Layer]) -> bool {
    entries.iter().any(|entry| match entry {
        Layer::Targeted(_) => true,
        Layer::Cut(op) => op.documents.is_some(),
        Layer::Group(group) => holds_a_pair(&group.contents.layers),
        Layer::Clipped(piece) => match &piece.entry {
            Layer::Targeted(_) => true,
            Layer::Cut(op) => op.documents.is_some(),
            _ => false,
        },
        Layer::Picture(_) | Layer::Blur(_) | Layer::Pattern(_) => false,
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
            //
            // On an eight-bit page the gain is the alpha on its byte, rounded
            // half up as a fill's is, so that the blur scales the colour the
            // page stores: as given, an alpha of 0.3 or 0.7 lands on a tie
            // the store rounds to even, and the interior stored a level under
            // a fill. On its byte the gain multiplies a colour placed on its
            // bytes -- see `ExactColour::at` -- back onto them from an
            // eight-bit layer on the CPU: the layer is off by at most half a
            // level, and the alpha, a byte over 255, scales that to under
            // half. The GPU blurs and scales at lower precision, which took
            // 68 of the 32,640 placed colours over the line at alphas from
            // 235, so there a translucent placed colour is drawn into a layer
            // of half floats, which holds every one exactly. Its edges are
            // rounded twice, to a half float and then to a byte, and land up
            // to a level off the exact Gaussian where a byte layer rounds them
            // once. An opaque one takes a byte layer: its gain is 1, the
            // layer holds the colour's bytes as they are, and interior and
            // edges are both exact -- through half floats, 91 pixels of a
            // black rounded rectangle under a blur of 2 left their exact
            // Gaussian.
            (Made::Result(_), Target::Pixels) => {
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
                let steps = [Step::Blur(self.paint.sigma)];
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
    use skia_safe::{
        AlphaType, BlendMode, Color, Color4f, ColorType, ImageInfo, Matrix,
        Paint, Path, Rect, color_filters, surfaces,
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
            surfaces::raster_n32_premul(RASTER).expect("a surface");
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
    /// layer in `Group::playback_within`, the bound `text_reach_tests` hold
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
