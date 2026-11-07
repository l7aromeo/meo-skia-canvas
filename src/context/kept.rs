//! Results kept by what they are made from, so that a draw handing the same
//! input to the same work takes the result already made: the chain a draw
//! runs on the CPU -- see [`chain`] -- and the blurred coverage of the
//! coverage route, coloured -- see [`coverage`].
//!
//! A kept result is filed under a digest of its input and everything else
//! the work reads, and taken only once the input it was made from is the
//! new input to the byte: what decides a hit is that comparison, as for the
//! runs of text `run_mask` keeps, and a digest two inputs share costs a
//! refusal, never a wrong draw. A shape has no object of its own that its
//! draws share, as a laid-out run does, so the input is what is compared.
//!
//! The digest is foldhash, seeded at random once a process. The input is
//! pixels, which can be anyone's, but what a digest files is bounded and
//! taken only after that comparison, so inputs chosen to share a digest
//! cost a comparison and a miss -- see `cache_map` for the rule.

#[cfg(test)]
use std::cell::Cell;
use std::{
    hash::{BuildHasher, Hash, Hasher},
    sync::{Arc, LazyLock},
};

use parking_lot::Mutex;
use skia_safe::{Flattenable, Image, ImageInfo, Vector};

use crate::{
    cache_map::{CacheHasher, KeptSets},
    cache_store::{Budget, Store, StoreKey, Stored},
    context::chain::{Outcome, Step, coverage::Coverage},
};

/// The bytes of chain results kept, across every thread: as many as the
/// run cache keeps -- a few hundred results of the layers a drawn shape
/// makes.
const KEPT_CACHE_BYTES: usize = 16 * 1024 * 1024;

/// The largest result kept, its input and its output together, so that one
/// page-sized layer cannot push out the many small ones a page repeats.
const KEPT_MAX_BYTES: usize = 1024 * 1024;

/// What the kept results are kept to, cut back to three quarters as the
/// chain cache's are.
const KEPT_BUDGET: Budget = Budget {
    bytes: KEPT_CACHE_BYTES,
    most: KEPT_MAX_BYTES,
    low_water: KEPT_CACHE_BYTES * 3 / 4,
};

/// The seed the digest is taken with, chosen at random once a process.
static DIGEST_KEYS: LazyLock<CacheHasher> = LazyLock::new(CacheHasher::default);

/// The digest of `bytes` -- see the module documentation.
fn digest(bytes: &[u8]) -> u64 {
    shared(DIGEST_KEYS.hash_one(bytes))
}

/// `digest`, or one digest for every input where a test has asked for
/// inputs to share one.
#[cfg(test)]
fn shared(digest: u64) -> u64 {
    match SHARED.get() {
        true => 0,
        false => digest,
    }
}

/// `digest`.
#[cfg(not(test))]
fn shared(digest: u64) -> u64 {
    digest
}

#[cfg(test)]
thread_local! {
    /// Whether a draw on this thread takes a kept result, for a test to
    /// draw with and without them.
    pub(crate) static KEPT: Cell<bool> = const { Cell::new(true) };
    /// How the results looked for on this thread fared.
    pub(crate) static FOUND: Cell<Found> = const { Cell::new(Found::NONE) };
    /// Whether every input digested on this thread shares one digest, for
    /// a test to file different inputs under one key.
    pub(crate) static SHARED: Cell<bool> = const { Cell::new(false) };
}

/// How the results looked for on a thread fared: taken; refused, because
/// the input kept under the key was not the new one; and not found.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Found {
    pub(crate) taken: usize,
    pub(crate) refused: usize,
    pub(crate) missed: usize,
}

#[cfg(test)]
impl Found {
    const NONE: Self = Self {
        taken: 0,
        refused: 0,
        missed: 0,
    };
}

/// Counts what a look for a kept result came to, and gives it back.
#[cfg(test)]
fn counted<T>(found: Option<T>, refused: bool) -> Option<T> {
    FOUND.with(|counts| {
        let mut c = counts.get();
        match (&found, refused) {
            (Some(_), _) => c.taken += 1,
            (None, true) => c.refused += 1,
            (None, false) => c.missed += 1,
        }
        counts.set(c);
    });
    found
}

/// A step of a chain by what it does: its numbers by their bits, and a
/// colour filter by its serialized form, which names everything it does.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum StepKey {
    Blur(u32),
    Colour(Vec<u8>),
    Shadow {
        offset: [u32; 2],
        sigma: u32,
        colour: [u32; 4],
        space: (u32, u32),
    },
}

impl StepKey {
    /// `step`'s key, or `None` for an image filter, whose work may read
    /// where its pixels lie and is not named here.
    fn of(step: &Step) -> Option<Self> {
        Some(match step {
            Step::Blur(sigma) => Self::Blur(sigma.to_bits()),
            Step::Colour(filter) => {
                Self::Colour(filter.serialize().as_bytes().to_vec())
            }
            Step::Shadow {
                offset,
                sigma,
                colour,
                space,
            } => Self::Shadow {
                offset: [offset.x, offset.y].map(f32::to_bits),
                sigma: sigma.to_bits(),
                colour: [colour.r, colour.g, colour.b, colour.a]
                    .map(f32::to_bits),
                space: (space.to_xyzd50_hash().0, space.transfer_fn_hash()),
            },
            Step::Image { .. } => return None,
        })
    }
}

/// What `use_rows` makes of a raster image's pixels, row by row, each row
/// its used bytes alone; `None` for an image whose pixels are not in main
/// memory.
fn with_rows<T>(
    image: &Image,
    use_rows: impl FnOnce(&mut dyn Iterator<Item = &[u8]>) -> T,
) -> Option<T> {
    let pixmap = image.peek_pixels()?;
    let info = pixmap.info();
    let (line, stride, height) = (
        info.min_row_bytes(),
        pixmap.row_bytes(),
        info.height() as usize,
    );
    let all = pixmap.bytes()?;
    let mut rows = all.chunks(stride).take(height).map(|row| &row[..line]);
    Some(use_rows(&mut rows))
}

/// The digest of a raster image's pixels, read where they lie -- see
/// [`digest`].
fn digest_rows(image: &Image) -> Option<u64> {
    with_rows(image, |rows| {
        let mut hasher = DIGEST_KEYS.build_hasher();
        rows.for_each(|row| hasher.write(row));
        shared(hasher.finish())
    })
}

/// Whether a raster image's pixels are `bytes`, read where they lie.
fn rows_are(image: &Image, bytes: &[u8]) -> bool {
    with_rows(image, |rows| {
        let mut at = 0;
        for row in rows {
            let next = at + row.len();
            if bytes.get(at..next) != Some(row) {
                return false;
            }
            at = next;
        }
        at == bytes.len()
    })
    .unwrap_or(false)
}

/// A raster image's pixels, row by row with nothing between rows, as the
/// bytes a kept result is compared by.
fn rows_copied(image: &Image) -> Option<Arc<[u8]>> {
    with_rows(image, |rows| rows.flatten().copied().collect())
}

/// How many keys the record of keys seen once holds: 256 sets of
/// [`SEEN_WAYS`].
const SEEN_SETS: usize = 256;

/// How many keys each set of the record of keys seen once holds.
const SEEN_WAYS: usize = 4;

/// The keys seen once lately, for a result to be kept only once its key
/// comes round again: a page that never repeats an input then pays for its
/// digest alone, not for a copy of every input and a store churning
/// through results nothing asks for -- which drew such a page slower than
/// not keeping at all. A key is filed here by a digest of itself, taken as
/// the input's is.
type Seen<K> = KeptSets<K, (), SEEN_SETS, SEEN_WAYS>;

/// Whether `key` has been seen before, filing it as seen if not.
fn seen_before<K: Hash + Eq + Clone>(seen: &Mutex<Seen<K>>, key: &K) -> bool {
    let hash = DIGEST_KEYS.hash_one(key);
    let mut seen = seen.lock();
    if seen.get(hash, key).is_some() {
        return true;
    }
    seen.file(hash, key.clone(), ());
    false
}

/// The colour type, alpha type and space of `info`, as a key's fields.
fn kind_of(info: &ImageInfo) -> (i32, i32, Option<(u32, u32)>) {
    (
        info.color_type() as i32,
        info.alpha_type() as i32,
        info.color_space()
            .map(|s| (s.to_xyzd50_hash().0, s.transfer_fn_hash())),
    )
}

/// What a chain's result on the CPU is filed under: the digest of its
/// input's pixels, the input's size and kind, every step, the scale the
/// layer was drawn at and the alpha the last step takes.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ChainKey {
    input: u64,
    size: (i32, i32),
    kind: (i32, i32, Option<(u32, u32)>),
    steps: Arc<[StepKey]>,
    scale: [u32; 2],
    gain: u32,
}

impl StoreKey for ChainKey {
    /// Never: the key holds what it names.
    fn is_orphaned(&self) -> bool {
        false
    }
}

/// A chain's kept result and the input it was made from.
#[derive(Clone)]
pub(crate) struct KeptChain {
    input: Arc<[u8]>,
    output: Image,
}

impl Stored for KeptChain {
    /// The input kept beside the result, and the result's pixels.
    fn bytes(&self) -> usize {
        self.input.len() + self.output.image_info().compute_min_byte_size()
    }
}

/// Every chain result kept, for every thread.
static CHAINS: LazyLock<Mutex<Store<ChainKey, KeptChain>>> =
    LazyLock::new(|| Mutex::new(Store::new(KEPT_BUDGET)));

/// The chain keys seen once lately -- see [`Seen`].
static SEEN_CHAINS: LazyLock<Mutex<Seen<ChainKey>>> =
    LazyLock::new(|| Mutex::new(Seen::new()));

/// A chain about to run on the CPU, by what decides its result: its key,
/// and the input it is run on.
pub(crate) struct ChainInput {
    key: ChainKey,
    input: Image,
}

impl ChainInput {
    /// `steps` about to run on `image`, a raster image, in a layer drawn
    /// at `scale`, the last taking `gain`. `None` where a step is not named
    /// by [`StepKey`] or the image's pixels are not in main memory.
    pub(crate) fn of(
        image: &Image,
        steps: &[Step],
        scale: Vector,
        gain: f32,
    ) -> Option<Self> {
        let steps: Arc<[StepKey]> =
            steps.iter().map(StepKey::of).collect::<Option<_>>()?;
        let info = image.image_info();
        Some(Self {
            key: ChainKey {
                input: digest_rows(image)?,
                size: (info.width(), info.height()),
                kind: kind_of(info),
                steps,
                scale: [scale.x, scale.y].map(f32::to_bits),
                gain: gain.to_bits(),
            },
            input: image.clone(),
        })
    }

    /// The result kept for this input, where the input it was made from is
    /// this one to the byte.
    pub(crate) fn kept(&self) -> Option<Image> {
        #[cfg(test)]
        if !KEPT.get() {
            return None;
        }
        let kept = CHAINS.lock().get(&self.key);
        let same = kept.as_ref().map(|kept| rows_are(&self.input, &kept.input));
        let found = kept
            .zip(same)
            .filter(|(_, same)| *same)
            .map(|(k, _)| k.output);
        #[cfg(test)]
        let found = counted(found, same == Some(false));
        found
    }

    /// Keeps `output` as this input's result, once its key has been seen
    /// before -- see [`Seen`].
    pub(crate) fn keep(self, output: Image) {
        if !seen_before(&SEEN_CHAINS, &self.key) {
            return;
        }
        let Some(input) = rows_copied(&self.input) else {
            return;
        };
        CHAINS.lock().put(self.key, KeptChain { input, output });
    }

    /// [`Self::keep`], as at the key's second sighting.
    #[cfg(test)]
    pub(crate) fn keep_seen(self, output: Image) {
        seen_before(&SEEN_CHAINS, &self.key);
        self.keep(output);
    }
}

/// What the coverage route's coloured result is filed under: the digest of
/// the coverage it blurred, which is its inked rectangle's, the size of the
/// region it covers and where in it the inked rectangle lies, the device
/// sigmas of its blur, and the colour, alpha and page it was coloured for.
/// Not where the region lies: a result found is moved there by whole pixels.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct CoverageKey {
    input: u64,
    size: (i32, i32),
    /// The inked rectangle within the region, left, top, width and height:
    /// the input is its bytes alone, so the same bytes elsewhere in the
    /// region are another coverage.
    inked: [i32; 4],
    sigma: [u32; 2],
    rgb: [u32; 3],
    gain: u32,
    kind: (i32, i32, Option<(u32, u32)>),
}

impl StoreKey for CoverageKey {
    /// Never: the key holds no reference to anything.
    fn is_orphaned(&self) -> bool {
        false
    }
}

/// A coverage route's kept result, the coverage it was made from, and
/// where its region lay.
#[derive(Clone)]
pub(crate) struct KeptCoverage {
    input: Arc<[u8]>,
    output: Outcome,
    at: (i32, i32),
}

impl Stored for KeptCoverage {
    /// The coverage kept beside the result, and the result's pixels.
    fn bytes(&self) -> usize {
        self.input.len() + self.output.bytes()
    }
}

/// Every coverage route result kept, for every thread.
static COVERAGES: LazyLock<Mutex<Store<CoverageKey, KeptCoverage>>> =
    LazyLock::new(|| Mutex::new(Store::new(KEPT_BUDGET)));

/// The coverage keys seen once lately -- see [`Seen`].
static SEEN_COVERAGES: LazyLock<Mutex<Seen<CoverageKey>>> =
    LazyLock::new(|| Mutex::new(Seen::new()));

/// A coverage about to be blurred and coloured, by what decides its result.
pub(crate) struct CoverageInput {
    key: CoverageKey,
    input: Vec<u8>,
    at: (i32, i32),
}

impl CoverageInput {
    /// `coverage`, to be blurred and coloured in `rgb` at `gain` for a page
    /// of `info`'s kind. `None` where the plane was not drawn in the
    /// device's own pixels, `rest` not the identity: a result moved on the
    /// device would then not be moved on the plane.
    pub(crate) fn of(
        coverage: &Coverage,
        rgb: [f32; 3],
        gain: f32,
        info: &ImageInfo,
    ) -> Option<Self> {
        if !coverage.rest().is_identity() {
            return None;
        }
        let (ink, inked) = coverage.ink();
        let input = ink.to_vec();
        let (region, sigma) = (coverage.region(), coverage.sigma());
        Some(Self {
            key: CoverageKey {
                input: digest(&input),
                size: (region.width(), region.height()),
                inked: [
                    inked.left - region.left,
                    inked.top - region.top,
                    inked.width(),
                    inked.height(),
                ],
                sigma: [sigma.0, sigma.1].map(f32::to_bits),
                rgb: rgb.map(f32::to_bits),
                gain: gain.to_bits(),
                kind: kind_of(info),
            },
            input,
            at: (region.left, region.top),
        })
    }

    /// The result kept for this coverage, moved to where it lies, where the
    /// coverage it was made from is this one to the byte.
    pub(crate) fn kept(&self) -> Option<Outcome> {
        #[cfg(test)]
        if !KEPT.get() {
            return None;
        }
        let kept = COVERAGES.lock().get(&self.key);
        #[cfg(test)]
        let refused = kept.as_ref().is_some_and(|k| *k.input != *self.input);
        let found =
            kept.filter(|kept| *kept.input == *self.input)
                .and_then(|kept| {
                    kept.output
                        .at((self.at.0 - kept.at.0, self.at.1 - kept.at.1))
                });
        #[cfg(test)]
        let found = counted(found, refused);
        found
    }

    /// Keeps `output` as this coverage's result, once its key has been seen
    /// before -- see [`Seen`].
    pub(crate) fn keep(self, output: Outcome) {
        if !seen_before(&SEEN_COVERAGES, &self.key) {
            return;
        }
        COVERAGES.lock().put(
            self.key,
            KeptCoverage {
                input: self.input.into(),
                output,
                at: self.at,
            },
        );
    }

    /// [`Self::keep`], as at the key's second sighting.
    #[cfg(test)]
    pub(crate) fn keep_seen(self, output: Outcome) {
        seen_before(&SEEN_COVERAGES, &self.key);
        self.keep(output);
    }
}

/// Drops every kept result: with the page cache's release, when rendering
/// has been idle.
pub(crate) fn release_kept_results() {
    CHAINS.lock().release();
    COVERAGES.lock().release();
    *SEEN_CHAINS.lock() = Seen::new();
    *SEEN_COVERAGES.lock() = Seen::new();
}

#[cfg(test)]
mod tests {
    use std::f32::consts::TAU;

    use skia_safe::{
        AlphaType, BlendMode, Color, Color4f, ColorSpace, ColorType, Data,
        Image, ImageInfo, Matrix, Paint, Rect, Vector, color_filters,
    };

    use super::{
        ChainInput, CoverageInput, FOUND, Found, KEPT, SHARED, digest,
    };
    use crate::{
        canvas::{Canvas, CanvasOptions},
        context::chain::{
            Outcome, Step,
            coverage::{self, Coverage, Plane},
        },
        path::FillRule,
        pixels::{ImageDataExport, PixelDepth, PixelExportOptions},
        started,
    };

    /// How far every third shape [`page`] draws is turned, in radians.
    const TURN: f32 = 0.4;

    /// `n` shapes drawn on a 160 by 120 CPU page of `colour` type, each
    /// moved by whole pixels from the one before so that every one lies at
    /// the same sub-pixel phase, then the page's premultiplied pixels: with
    /// kept results taken where `kept`, and drawn afresh otherwise. The
    /// shapes take turns through `filters` and through two fills, one
    /// opaque and one translucent, so that a result kept for one filter or
    /// colour and taken for another shows. `ellipse` draws a path, which the
    /// coverage route takes on an eight-bit page; a rect otherwise, which a
    /// drop shadow's chain does.
    fn page(
        filters: [&str; 2],
        colour: PixelDepth,
        ellipse: bool,
        n: usize,
        kept: bool,
    ) -> Vec<u8> {
        KEPT.set(kept);
        let mut canvas = cpu_page(colour);
        let ctx = canvas.context();
        for i in 0..n {
            ctx.set_filter_css(filters[i % 2]).expect("a filter");
            ctx.set_fill_style_css(
                ["rgb(200 120 40)", "rgb(40 120 200 / 0.5)"][i / 2 % 2],
            )
            .expect("a colour");
            let (x, y) =
                ((i * 23 % 120) as f32 + 10.3, (i * 17 % 80) as f32 + 10.6);
            // Every third shape turned about its centre, so that a result
            // kept for a draw the layer holds unturned is not taken for one
            // the transform turns afterwards.
            let turned = i % 3 == 2;
            if turned {
                ctx.save();
                ctx.translate(x + 7.5, y + 6.0);
                ctx.rotate(TURN);
                ctx.translate(-(x + 7.5), -(y + 6.0));
            }
            match ellipse {
                true => {
                    ctx.begin_path();
                    ctx.ellipse(
                        x + 7.5,
                        y + 6.0,
                        7.5,
                        6.0,
                        0.0,
                        0.0,
                        TAU,
                        false,
                    )
                    .expect("an ellipse");
                    ctx.fill(FillRule::NonZero);
                }
                false => ctx.fill_rect(x, y, 15.0, 12.0),
            }
            if turned {
                ctx.restore();
            }
        }
        let pixels = pixels_of(&mut canvas, colour);
        KEPT.set(true);
        pixels
    }

    /// A 160 by 120 CPU page of `colour` type.
    fn cpu_page(colour: PixelDepth) -> Canvas {
        Canvas::with_options(
            160.0,
            120.0,
            CanvasOptions {
                color_type: colour,
                gpu: false,
                ..CanvasOptions::default()
            },
        )
        .expect("a canvas")
    }

    /// The premultiplied pixels of `canvas`, a page of `colour` type.
    fn pixels_of(canvas: &mut Canvas, colour: PixelDepth) -> Vec<u8> {
        canvas
            .to_image_data(
                None,
                &ImageDataExport {
                    pixels: PixelExportOptions {
                        depth: colour,
                        premultiplied: true,
                        ..PixelExportOptions::default()
                    },
                    ..ImageDataExport::default()
                },
            )
            .expect("pixels")
            .pixels()
            .to_vec()
    }

    /// `n` shapes of one size drawn under `filter` on a page of `colour`
    /// type, taking turns between an ellipse and a diamond with the same
    /// bounds, each moved by whole pixels from the one before, then the
    /// page's premultiplied pixels: with kept results taken where `kept`,
    /// and drawn afresh otherwise.
    fn ellipses_and_diamonds(
        filter: &str,
        colour: PixelDepth,
        n: usize,
        kept: bool,
    ) -> Vec<u8> {
        KEPT.set(kept);
        let mut canvas = cpu_page(colour);
        let ctx = canvas.context();
        ctx.set_filter_css(filter).expect("a filter");
        ctx.set_fill_style_css("rgb(200 120 40)").expect("a colour");
        for i in 0..n {
            let (x, y) =
                ((i * 23 % 120) as f32 + 10.3, (i * 17 % 80) as f32 + 10.6);
            ctx.begin_path();
            match i % 2 {
                0 => ctx
                    .ellipse(x + 7.5, y + 6.0, 7.5, 6.0, 0.0, 0.0, TAU, false)
                    .expect("an ellipse"),
                _ => {
                    ctx.move_to(x + 7.5, y);
                    ctx.line_to(x + 15.0, y + 6.0);
                    ctx.line_to(x + 7.5, y + 12.0);
                    ctx.line_to(x, y + 6.0);
                    ctx.close_path();
                }
            }
            ctx.fill(FillRule::NonZero);
        }
        let pixels = pixels_of(&mut canvas, colour);
        KEPT.set(true);
        pixels
    }

    #[test]
    fn a_kept_result_draws_what_the_work_would() {
        // A drop shadow's chain on an eight-bit page, a blurred ellipse's
        // chain on a float one, and the coverage route's blurred ellipse on
        // an eight-bit one -- a blurred rect is baked whole, and takes
        // neither: each
        // shape drawn again at whole-pixel moves takes the kept result, and
        // the page is the page drawn afresh, to the byte.
        for (filters, colour, ellipse) in [
            (
                [
                    "drop-shadow(2px 2px 3px black)",
                    "drop-shadow(1px 3px 2px red)",
                ],
                PixelDepth::Uint8,
                false,
            ),
            (["blur(3px)", "blur(2px)"], PixelDepth::F32, true),
            (["blur(3px)", "blur(2px)"], PixelDepth::Uint8, true),
        ] {
            let fresh = page(filters, colour, ellipse, 24, false);
            FOUND.set(Found::NONE);
            let kept = page(filters, colour, ellipse, 24, true);
            let found = FOUND.get();
            assert_eq!(kept, fresh, "{filters:?} on {colour:?}");
            assert!(found.taken > 0, "{filters:?} on {colour:?}: {found:?}");
        }
    }

    #[test]
    fn inputs_sharing_a_digest_are_refused_not_drawn_wrong() {
        // Every input digested shares one digest, so that an ellipse and a
        // diamond of one size are filed under one key and each finds the
        // other's result there: the comparison refuses it, on each route,
        // and the page is the page drawn afresh.
        for (filter, colour) in [
            ("drop-shadow(2px 2px 3px black)", PixelDepth::Uint8),
            ("blur(3px)", PixelDepth::F32),
            ("blur(3px)", PixelDepth::Uint8),
        ] {
            let fresh = ellipses_and_diamonds(filter, colour, 12, false);
            SHARED.set(true);
            FOUND.set(Found::NONE);
            let kept = ellipses_and_diamonds(filter, colour, 12, true);
            let found = FOUND.get();
            SHARED.set(false);
            assert_eq!(kept, fresh, "{filter} on {colour:?}");
            assert!(found.refused > 0, "{filter} on {colour:?}: {found:?}");
        }
    }

    /// A 24 by 20 raster image of `fill` premultiplied bytes.
    fn image(fill: u8) -> Image {
        sized((24, 20), fill)
    }

    /// A raster image `size` of `fill` premultiplied bytes.
    fn sized(size: (i32, i32), fill: u8) -> Image {
        let info = ImageInfo::new(
            size,
            ColorType::RGBA8888,
            AlphaType::Premul,
            ColorSpace::new_srgb(),
        );
        let bytes = vec![fill; info.compute_min_byte_size()];
        started::raster_from_data(
            &info,
            Data::new_copy(&bytes),
            info.min_row_bytes(),
        )
        .expect("an image")
    }

    #[test]
    fn a_result_is_kept_once_its_input_comes_round_again() {
        // A page that never repeats an input keeps nothing: a result is
        // kept at its key's second sighting, and found from the third.
        let steps = [Step::Blur(3.0)];
        let scale = Vector::new(1.0, 1.0);
        let input =
            || ChainInput::of(&image(123), &steps, scale, 1.0).expect("a key");
        input().keep(image(6));
        assert!(input().kept().is_none(), "kept at its first sighting");
        input().keep(image(6));
        assert!(input().kept().is_some(), "not kept at its second");
    }

    #[test]
    fn a_kept_result_is_refused_for_another_input_under_its_key() {
        // Two inputs filed under one key -- a digest they share -- and the
        // second does not take the first's result.
        let steps = [Step::Blur(1.5)];
        let scale = Vector::new(1.0, 1.0);
        let first =
            ChainInput::of(&image(40), &steps, scale, 1.0).expect("a key");
        let key = first.key.clone();
        first.keep_seen(image(1));
        let mut second =
            ChainInput::of(&image(41), &steps, scale, 1.0).expect("a key");
        second.key = key.clone();
        FOUND.set(Found::NONE);
        assert!(second.kept().is_none(), "another input took the result");
        assert_eq!(FOUND.get().refused, 1);
        let again =
            ChainInput::of(&image(40), &steps, scale, 1.0).expect("a key");
        assert_eq!(again.key, key);
        assert!(again.kept().is_some(), "its own input missed");
    }

    #[test]
    fn a_change_to_anything_a_chain_reads_misses() {
        // The same input kept, then looked for under every key that changes
        // one thing the chain's result depends on.
        let scale = Vector::new(1.0, 1.0);
        let kept = ChainInput::of(&image(77), &[Step::Blur(2.0)], scale, 0.5)
            .expect("a key");
        kept.keep_seen(image(2));
        let found = |steps: &[Step], scale, gain| {
            ChainInput::of(&image(77), steps, scale, gain)
                .expect("a key")
                .kept()
                .is_some()
        };
        assert!(found(&[Step::Blur(2.0)], scale, 0.5), "its own key missed");
        assert!(!found(&[Step::Blur(2.5)], scale, 0.5), "another sigma");
        assert!(
            !found(&[Step::Blur(2.0)], Vector::new(2.0, 1.0), 0.5),
            "another scale"
        );
        assert!(!found(&[Step::Blur(2.0)], scale, 0.75), "another gain");
        assert!(
            !found(&[Step::Blur(2.0), Step::Blur(2.0)], scale, 0.5),
            "another chain"
        );
        let other = ChainInput::of(&image(78), &[Step::Blur(2.0)], scale, 0.5)
            .expect("a key");
        assert!(other.kept().is_none(), "another input");
    }

    #[test]
    fn a_chain_input_of_another_kind_or_filter_misses() {
        // The same bytes in another space, and the same input through
        // another colour filter: neither takes the result kept.
        let scale = Vector::new(1.0, 1.0);
        let filter = |colour: Color| {
            Step::Colour(
                color_filters::blend(colour, BlendMode::SrcIn)
                    .expect("a filter"),
            )
        };
        ChainInput::of(&image(55), &[filter(Color::RED)], scale, 1.0)
            .expect("a key")
            .keep_seen(image(5));
        let found = |image: &Image, step: Step| {
            ChainInput::of(image, &[step], scale, 1.0)
                .expect("a key")
                .kept()
                .is_some()
        };
        assert!(found(&image(55), filter(Color::RED)), "its own key missed");
        assert!(!found(&image(55), filter(Color::BLUE)), "another filter");
        let linear = image(55)
            .reinterpret_color_space(ColorSpace::new_srgb_linear())
            .expect("an image");
        assert!(!found(&linear, filter(Color::RED)), "another space");
    }

    #[test]
    fn a_chain_input_of_another_size_misses() {
        // Two inputs of the same bytes laid out 24 by 20 and 20 by 24, made
        // to share a digest, and the second does not take the first's
        // result.
        let steps = [Step::Blur(1.0)];
        let scale = Vector::new(1.0, 1.0);
        SHARED.set(true);
        let wide = ChainInput::of(&sized((24, 20), 0), &steps, scale, 1.0)
            .expect("a key");
        let tall = ChainInput::of(&sized((20, 24), 0), &steps, scale, 1.0)
            .expect("a key");
        SHARED.set(false);
        assert_eq!(wide.key.input, tall.key.input, "the control");
        wide.keep_seen(image(3));
        assert!(tall.kept().is_none(), "taken at another size");
    }

    #[test]
    fn a_change_to_anything_a_shadow_reads_misses() {
        let shadow =
            |dx: f32, sigma: f32, colour: Color4f, space: ColorSpace| {
                Step::Shadow {
                    offset: Vector::new(dx, 2.0),
                    sigma,
                    colour,
                    space,
                }
            };
        let black = Color4f::new(0.0, 0.0, 0.0, 1.0);
        let base = || shadow(1.0, 2.0, black, ColorSpace::new_srgb());
        let scale = Vector::new(1.0, 1.0);
        ChainInput::of(&image(91), &[base()], scale, 1.0)
            .expect("a key")
            .keep_seen(image(4));
        let found = |step: Step| {
            ChainInput::of(&image(91), &[step], scale, 1.0)
                .expect("a key")
                .kept()
                .is_some()
        };
        assert!(found(base()), "its own key missed");
        assert!(
            !found(shadow(1.5, 2.0, black, ColorSpace::new_srgb())),
            "another offset"
        );
        assert!(
            !found(shadow(1.0, 2.5, black, ColorSpace::new_srgb())),
            "another sigma"
        );
        assert!(
            !found(shadow(
                1.0,
                2.0,
                Color4f::new(1.0, 0.0, 0.0, 1.0),
                ColorSpace::new_srgb()
            )),
            "another colour"
        );
        assert!(
            !found(shadow(1.0, 2.0, black, ColorSpace::new_srgb_linear())),
            "another space"
        );
    }

    /// The sharp coverage of `rect` on a CPU page `size`, for a blur of
    /// `sigma`, under `matrix`.
    fn sharp(
        size: (i32, i32),
        rect: Rect,
        sigma: f32,
        matrix: &Matrix,
    ) -> Coverage {
        let info = ImageInfo::new_n32_premul(size, ColorSpace::new_srgb());
        let mut surface = started::raster(&info, None, None).expect("a page");
        let canvas = surface.canvas();
        canvas.set_matrix(&(*matrix).into());
        let mut paint = Paint::default();
        paint.set_color(Color::BLACK).set_anti_alias(true);
        coverage::sharp(canvas, sigma, Some(rect), Plane::Drawn, |plane| {
            plane.draw_rect(rect, &paint);
        })
        .expect("a coverage")
    }

    #[test]
    fn a_change_to_anything_a_coverage_reads_misses() {
        // One coverage kept, then looked for under every key that changes
        // one thing its coloured result depends on; the coverage itself
        // the same wherever that is not the change.
        let page = ImageInfo::new_n32_premul((64, 48), ColorSpace::new_srgb());
        let rect = Rect::from_xywh(20.3, 15.6, 15.0, 12.0);
        let identity = Matrix::new_identity();
        let rgb = [200.0, 120.0, 40.0];
        let kept = sharp((64, 48), rect, 0.4, &identity);
        CoverageInput::of(&kept, rgb, 1.0, &page)
            .expect("a key")
            .keep_seen(Outcome::blank(8));
        let found = |coverage: &Coverage, rgb, gain, info: &ImageInfo| {
            CoverageInput::of(coverage, rgb, gain, info)
                .expect("a key")
                .kept()
                .is_some()
        };
        assert!(found(&kept, rgb, 1.0, &page), "its own key missed");
        assert!(
            !found(&kept, [40.0, 120.0, 200.0], 1.0, &page),
            "another colour"
        );
        assert!(!found(&kept, rgb, 0.5, &page), "another alpha");
        let linear =
            ImageInfo::new_n32_premul((64, 48), ColorSpace::new_srgb_linear());
        assert!(!found(&kept, rgb, 1.0, &linear), "another page");
        // A sigma whose reach is the same three pixels, so the same
        // coverage over the same region.
        let wider = sharp((64, 48), rect, 0.45, &identity);
        assert_eq!(wider.bytes(), kept.bytes(), "the control");
        assert!(!found(&wider, rgb, 1.0, &page), "another sigma");
    }

    #[test]
    fn a_coverage_of_another_size_misses() {
        // Two pages covered whole, 64 by 48 and 48 by 64: the same bytes,
        // one digest, and the second does not take the first's result.
        let page = ImageInfo::new_n32_premul((64, 48), ColorSpace::new_srgb());
        let identity = Matrix::new_identity();
        let rgb = [0.0, 0.0, 0.0];
        let all =
            |w: f32, h: f32| Rect::from_xywh(-10.0, -10.0, w + 20.0, h + 20.0);
        let wide = sharp((64, 48), all(64.0, 48.0), 0.4, &identity);
        let tall = sharp((48, 64), all(48.0, 64.0), 0.4, &identity);
        let (w, t) = (
            CoverageInput::of(&wide, rgb, 1.0, &page).expect("a key"),
            CoverageInput::of(&tall, rgb, 1.0, &page).expect("a key"),
        );
        assert_eq!(digest(&w.input), digest(&t.input), "the control");
        w.keep_seen(Outcome::blank(8));
        assert!(t.kept().is_none(), "taken at another size");
    }

    #[test]
    fn a_coverage_kept_is_refused_for_another_under_its_key() {
        let page = ImageInfo::new_n32_premul((64, 48), ColorSpace::new_srgb());
        let identity = Matrix::new_identity();
        let rgb = [200.0, 120.0, 40.0];
        let one = sharp(
            (64, 48),
            Rect::from_xywh(20.3, 15.6, 15.0, 12.0),
            1.0,
            &identity,
        );
        let other = sharp(
            (64, 48),
            Rect::from_xywh(20.3, 15.6, 14.0, 12.0),
            1.0,
            &identity,
        );
        let first = CoverageInput::of(&one, rgb, 1.0, &page).expect("a key");
        let key = first.key.clone();
        first.keep_seen(Outcome::blank(8));
        let mut second =
            CoverageInput::of(&other, rgb, 1.0, &page).expect("a key");
        assert_ne!(
            second.input,
            CoverageInput::of(&one, rgb, 1.0, &page)
                .expect("a key")
                .input
        );
        second.key = key;
        FOUND.set(Found::NONE);
        assert!(second.kept().is_none(), "another coverage took the result");
        assert_eq!(FOUND.get().refused, 1);
    }

    #[test]
    fn a_coverage_drawn_turned_is_not_kept() {
        // Under a turn the plane is drawn unturned and the turn is the
        // result's to make, so a result kept for an unturned draw of the
        // same coverage would be composited unturned.
        let page = ImageInfo::new_n32_premul((64, 48), ColorSpace::new_srgb());
        let turned = sharp(
            (64, 48),
            Rect::from_xywh(20.3, 15.6, 15.0, 12.0),
            1.0,
            &Matrix::rotate_deg(30.0),
        );
        assert!(!turned.rest().is_identity(), "the control");
        assert!(CoverageInput::of(&turned, [0.0; 3], 1.0, &page).is_none());
    }
}
