//! Blurred runs of text kept for drawing again: a run the coverage route
//! plays -- see `layer::CoverageRun` -- keeps what it composited, so that the
//! same run drawn again where it covers the same pixels composites that
//! rather than blurring its coverage again.
//!
//! What a run covers depends on where it lies, so a kept result is never
//! trusted on its key alone. The key finds the result a run drawn at the same
//! sub-pixel phase left -- see [`RunKey`] -- and the run's coverage is drawn
//! again on the plane the route would blur, then compared byte for byte with
//! the coverage the result was blurred from. Only where the two are the same
//! is the kept result composited, moved by the whole pixels between them; any
//! difference blurs afresh. Skia places a glyph's mask by a floor of its
//! device position (`prepare_for_direct_mask_drawing`,
//! `skia/src/core/SkGlyphRunPainter.cpp` under the skia-bindings crate, lines
//! 121 to 163), so a run moved by whole pixels usually covers the same; but
//! that position is summed in floats whose rounding depends on the whole
//! part, and a glyph past 256 device pixels is drawn as a path at its exact
//! place (`SkStrikeSpec::ShouldDrawAsPath`, `SkStrikeSpec.cpp`, lines 101 to
//! 126). The comparison is what makes a hit exact, not the key.

#[cfg(test)]
use std::cell::Cell;
use std::{
    hash::{Hash, Hasher},
    sync::{Arc, LazyLock},
};

use parking_lot::Mutex;
use skia_safe::{Canvas as SkCanvas, Matrix, Paint};

use crate::{
    cache_store::{Budget, Store, StoreKey, Stored},
    context::chain::{Outcome, coverage::Coverage},
    node::typography::RunPaint,
};

/// The bytes of kept runs, across every thread: a quarter of what the page
/// cache keeps -- see `page::PAGE_CACHE_BYTES`. A 40px "Ag" blurred by 3
/// keeps 35,260 bytes, its result and its coverage, so this holds some four
/// hundred and seventy of them.
const RUN_CACHE_BYTES: usize = 16 * 1024 * 1024;

/// The largest kept run, so that one long line cannot push out a page of
/// words: five bytes a pixel, its result's four and its coverage's one, over
/// a plane some 458 pixels square.
const RUN_MAX_BYTES: usize = 1024 * 1024;

/// What the run cache keeps its runs to, cut back to three quarters as the
/// chain cache's is.
const RUN_BUDGET: Budget = Budget {
    bytes: RUN_CACHE_BYTES,
    most: RUN_MAX_BYTES,
    low_water: RUN_CACHE_BYTES * 3 / 4,
};

/// How finely [`RunKey`] tells sub-pixel phases apart, in steps a pixel: far
/// finer than the quarter of a pixel Skia places a glyph's mask by, and far
/// coarser than the float error of a position, which moves the same
/// fraction's bits with its whole part -- 120.3 and 177.3 part in their last
/// bits as floats. A run is found by its step and then held to its coverage,
/// so a step shared by two placements that cover otherwise costs a refusal,
/// never a wrong draw.
const PHASE_STEPS: f32 = 256.0;

/// What a draw's paint decides of the coverage a run inks: its style, its
/// stroke, its anti-aliasing and its colour. The colour is kept although
/// FreeType covers a run alike in some colours that CoreText does not --
/// see `node::typography`'s
/// `a_runs_coverage_depends_on_the_colour_it_is_drawn_in`: filing two such
/// colours apart costs a hit and never a wrong one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct PaintKey {
    style: u8,
    anti_alias: bool,
    shaded: bool,
    stroke: [u32; 2],
    join: u8,
    cap: u8,
    colour: [u32; 3],
}

impl PaintKey {
    /// What `paint` decides of a run's coverage, or `None` for a paint
    /// whose path effect redraws the run in a way this does not name.
    fn of(paint: &Paint) -> Option<Self> {
        if paint.path_effect().is_some() {
            return None;
        }
        let colour = paint.color4f();
        Some(Self {
            style: paint.style() as u8,
            anti_alias: paint.is_anti_alias(),
            shaded: paint.shader().is_some(),
            stroke: [paint.stroke_width(), paint.stroke_miter()]
                .map(f32::to_bits),
            join: paint.stroke_join() as u8,
            cap: paint.stroke_cap() as u8,
            colour: [colour.r, colour.g, colour.b].map(f32::to_bits),
        })
    }
}

/// A kept run of text, by what it draws, and where: `anchor` maps the run's
/// origin, under the transform it was drawn under, into the coordinates its
/// picture is played in. The run is held, so that the address a key
/// compares by is not reused while the key lives.
#[derive(Clone, Debug)]
pub struct Kept {
    run: Arc<RunPaint>,
    anchor: Matrix,
    paint: PaintKey,
}

impl Kept {
    /// `run`, drawn at `anchor` through `paint`, the paint its coverage is
    /// drawn in -- see [`Kept`]. `None` where `paint` covers the run in a
    /// way [`PaintKey`] cannot name.
    pub(crate) fn new(
        run: Arc<RunPaint>,
        anchor: Matrix,
        paint: &Paint,
    ) -> Option<Self> {
        Some(Self {
            run,
            anchor,
            paint: PaintKey::of(paint)?,
        })
    }
}

/// What a kept run's result is filed under: the run and its paint, the part
/// of the device transform that is not a translation, the sub-pixel phase of
/// the run's origin on the device to a [`PHASE_STEPS`]th of a pixel, the blur,
/// the colour and alpha it was composited in, and the colour type and space of
/// the page. Not the whole pixels the origin lies at: a run drawn elsewhere
/// finds the result, and the comparison in [`drawn`] decides whether it may
/// be used. Filed with the run's region -- see [`Filed`].
#[derive(Clone, Debug)]
pub(crate) struct RunKey {
    run: Arc<RunPaint>,
    paint: PaintKey,
    linear: [u32; 4],
    phase: [u16; 2],
    sigma: u32,
    rgb: [u32; 3],
    gain: u32,
    color_type: i32,
    space: Option<(u32, u32)>,
}

impl PartialEq for RunKey {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.run, &other.run)
            && self.paint == other.paint
            && self.linear == other.linear
            && self.phase == other.phase
            && self.sigma == other.sigma
            && self.rgb == other.rgb
            && self.gain == other.gain
            && self.color_type == other.color_type
            && self.space == other.space
    }
}

impl Eq for RunKey {}

impl Hash for RunKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        Arc::as_ptr(&self.run).hash(state);
        self.paint.hash(state);
        self.linear.hash(state);
        self.phase.hash(state);
        self.sigma.hash(state);
        self.rgb.hash(state);
        self.gain.hash(state);
        self.color_type.hash(state);
        self.space.hash(state);
    }
}

/// A [`RunKey`] and the device pixels the run's coverage covers, relative
/// to the whole pixels of its origin -- see
/// `chain::coverage::Coverage::region`: what a kept result is filed under. A
/// run whose region a clip or the device's edge cuts otherwise is filed apart,
/// so that a run drawn there and one drawn clear of it each find their own.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Filed {
    key: RunKey,
    region: [i32; 4],
}

impl StoreKey for Filed {
    /// Never: the key holds the run it names.
    fn is_orphaned(&self) -> bool {
        false
    }
}

/// Where a kept run lies on the device: the whole pixels of its origin, and
/// the key its result is filed under there.
pub(crate) struct Placed {
    pub(crate) key: RunKey,
    pub(crate) origin: (i32, i32),
}

impl Placed {
    /// `kept` played onto `canvas` under `to_picture`, the transform its
    /// picture is played in, blurred by `sigma` and composited in `rgb` at
    /// `gain`. `None` where the transform does more than scale and
    /// translate, for which a kept result could not be moved by whole
    /// pixels.
    pub(crate) fn of(
        kept: &Kept,
        canvas: &SkCanvas,
        to_picture: &Matrix,
        sigma: f32,
        rgb: [f32; 3],
        gain: f32,
    ) -> Option<Self> {
        let device = Matrix::concat(to_picture, &kept.anchor);
        if !device.is_scale_translate() {
            return None;
        }
        let (x, y) = (device.translate_x(), device.translate_y());
        let (whole_x, whole_y) = (x.floor(), y.floor());
        let info = canvas.image_info();
        Some(Self {
            key: RunKey {
                run: Arc::clone(&kept.run),
                paint: kept.paint,
                linear: [
                    device.scale_x(),
                    device.skew_x(),
                    device.skew_y(),
                    device.scale_y(),
                ]
                .map(f32::to_bits),
                phase: [x - whole_x, y - whole_y]
                    .map(|fraction| (fraction * PHASE_STEPS).round() as u16),
                sigma: sigma.to_bits(),
                rgb: rgb.map(f32::to_bits),
                gain: gain.to_bits(),
                color_type: info.color_type() as i32,
                space: info
                    .color_space()
                    .map(|s| (s.to_xyzd50_hash().0, s.transfer_fn_hash())),
            },
            origin: (whole_x as i32, whole_y as i32),
        })
    }
}

/// A kept run's result: what it composited, the coverage that was blurred
/// to make it, and the whole pixels of the origin it was drawn at.
#[derive(Clone)]
pub(crate) struct Mask {
    pub(crate) outcome: Outcome,
    pub(crate) coverage: Arc<[u8]>,
    pub(crate) origin: (i32, i32),
}

impl Stored for Mask {
    /// The result's pixels and the coverage kept beside them.
    fn bytes(&self) -> usize {
        self.outcome.bytes() + self.coverage.len()
    }
}

/// Every kept run, for every thread: pages are played back on rayon
/// workers.
static RUNS: LazyLock<Mutex<Store<Filed, Mask>>> =
    LazyLock::new(|| Mutex::new(Store::new(RUN_BUDGET)));

/// Counts one run against [`MASKS`].
#[cfg(test)]
fn count(change: impl FnOnce(&mut Masks)) {
    let mut masks = MASKS.get();
    change(&mut masks);
    MASKS.set(masks);
}

#[cfg(test)]
thread_local! {
    /// A store of this thread's own, which [`get`] and [`put`] use in place
    /// of [`RUNS`] while an [`OwnRunCache`] lives.
    static OWN: std::cell::RefCell<Option<Store<Filed, Mask>>> =
        const { std::cell::RefCell::new(None) };
}

/// A store of the calling thread's own for as long as the guard lives: for
/// a test that counts the runs drawn from it. The shared store is emptied
/// whenever another test registers a font, which would make the count's
/// hits miss.
#[cfg(test)]
pub(crate) struct OwnRunCache;

#[cfg(test)]
impl OwnRunCache {
    pub(crate) fn new() -> Self {
        OWN.with_borrow_mut(|own| *own = Some(Store::new(RUN_BUDGET)));
        Self
    }
}

#[cfg(test)]
impl Drop for OwnRunCache {
    fn drop(&mut self) {
        OWN.with_borrow_mut(|own| *own = None);
    }
}

/// The result filed under `key`, if one is.
fn get(key: &Filed) -> Option<Mask> {
    #[cfg(test)]
    if let Some(found) =
        OWN.with_borrow_mut(|own| own.as_mut().map(|store| store.get(key)))
    {
        return found;
    }
    RUNS.lock().get(key)
}

/// Files `mask` under `key`.
fn put(key: Filed, mask: Mask) {
    #[cfg(test)]
    let Some((key, mask)) = OWN.with_borrow_mut(|own| match own {
        Some(store) => {
            store.put(key, mask);
            None
        }
        None => Some((key, mask)),
    }) else {
        return;
    };
    RUNS.lock().put(key, mask);
}

/// What a run whose coverage is `sharp` composites: the result kept for
/// `placed` and `sharp`'s region about the run's origin -- see [`Filed`] --
/// where the coverage it was blurred from is `sharp`'s to the byte, moved by
/// the whole pixels between the two origins; otherwise `colour` of `sharp`,
/// kept for the next draw in place of the one refused. A run with nowhere
/// `placed` is coloured and not kept.
pub(crate) fn drawn(
    placed: Option<Placed>,
    sharp: Coverage,
    colour: impl FnOnce(Coverage) -> Option<Outcome>,
) -> Option<Outcome> {
    #[cfg(test)]
    if !MASK_CACHE.get() {
        return colour(sharp);
    }
    let Some(placed) = placed else {
        return colour(sharp);
    };
    let (x, y) = placed.origin;
    let region = sharp.region().with_offset((-x, -y));
    let filed = Filed {
        key: placed.key,
        region: [region.left, region.top, region.right, region.bottom],
    };
    if let Some(mask) = get(&filed) {
        let alike = *mask.coverage == *sharp.bytes();
        let moved = alike
            .then(|| mask.outcome.at((x - mask.origin.0, y - mask.origin.1)))
            .flatten();
        #[cfg(test)]
        count(|masks| match moved {
            Some(_) => masks.hits += 1,
            None => masks.refused += 1,
        });
        if moved.is_some() {
            return moved;
        }
    }
    #[cfg(test)]
    count(|masks| masks.misses += 1);
    let coverage: Arc<[u8]> = sharp.bytes().into();
    let outcome = colour(sharp)?;
    put(
        filed,
        Mask {
            outcome: outcome.clone(),
            coverage,
            origin: placed.origin,
        },
    );
    Some(outcome)
}

/// Drops every kept run: with the page cache's release, when rendering has
/// been idle, and when the fonts change, since a kept run holds the faces
/// it was laid out in.
pub(crate) fn release_cached_runs() {
    #[cfg(test)]
    OWN.with_borrow_mut(|own| own.as_mut().map(Store::release));
    RUNS.lock().release();
}

/// How many runs this thread's own store holds -- see [`OwnRunCache`].
#[cfg(test)]
fn held() -> usize {
    OWN.with_borrow(|own| own.as_ref().map_or(0, Store::len))
}

/// How the runs played on a thread fared against the cache: drawn from
/// what it kept; refused, because the coverage kept under the run's key was
/// not the run's; and blurred afresh, the refused among them.
#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Masks {
    pub(crate) hits: usize,
    pub(crate) refused: usize,
    pub(crate) misses: usize,
}

#[cfg(test)]
thread_local! {
    /// Whether a run may be drawn from what this cache keeps, so that a test
    /// can hold the cache to the route it stands in for.
    pub(crate) static MASK_CACHE: Cell<bool> = const { Cell::new(true) };
    /// How the runs played on this thread fared -- see [`Masks`].
    pub(crate) static MASKS: Cell<Masks> =
        const { Cell::new(Masks { hits: 0, refused: 0, misses: 0 }) };
}

#[cfg(test)]
mod tests {
    //! A run drawn from the cache lands the bytes the coverage route draws
    //! it in afresh, wherever the two could part: in place, moved by whole
    //! and by fractional pixels, near the device's origin, cut by a clip,
    //! large enough to be drawn as paths, in a colour font, scaled, at a
    //! density, and in another colour, alpha, sigma or stroke. Where the
    //! counts are decided by what the run covers, they are held too: a run
    //! moved by whole pixels from masks is drawn from the cache, and one
    //! whose clip cuts it otherwise is refused.

    use super::{
        MASK_CACHE, MASKS, Masks, OwnRunCache, held, release_cached_runs,
    };
    use crate::{
        context::layer::COVERAGE_RUNS,
        path::FillRule,
        pixels::{ImageDataExport, PixelColorSpace, PixelExportOptions},
        prelude::*,
    };

    /// The family Oswald is registered under for these runs, so that it
    /// cannot be found as a system font of the same name instead.
    const OSWALD: &str = "RunMaskOswald";

    /// The family the bundled COLRv1 sample font is registered under.
    const COLOUR_FONT: &str = "RunMaskColr";

    /// One run of text drawn on a page: what, where, in what, and under
    /// what.
    #[derive(Clone, Copy)]
    struct Draw {
        text: &'static str,
        font: &'static str,
        at: (f32, f32),
        fill: &'static str,
        blur: f32,
        stroke: bool,
        scale: f32,
        clip: Option<(f32, f32, f32, f32)>,
    }

    const RUN: Draw = Draw {
        text: "Ag",
        font: "40px RunMaskOswald",
        at: (120.3, 90.6),
        fill: "rgb(200, 80, 30)",
        blur: 3.0,
        stroke: false,
        scale: 1.0,
        clip: None,
    };

    /// `draws` on one 8-bit sRGB CPU page of 400 by 400, each under its own
    /// `save`, read back premultiplied at `density`, with the cache on or
    /// off, how many runs took the coverage route, and how they fared
    /// against the cache. One page, because a run is kept by the canvas
    /// state it is laid out under -- see `node::typography::LaidOutKey` --
    /// so a run is drawn from the cache only on the canvas that kept it.
    fn page(
        draws: &[Draw],
        density: u32,
        space: PixelColorSpace,
        cache: bool,
    ) -> (Vec<u8>, usize, Masks) {
        let library = FontLibrary::new();
        library
            .use_paths(
                Some(OSWALD),
                ["tests/assets/fonts/Oswald/Oswald-VariableFont_wght.ttf"],
            )
            .expect("registers");
        library
            .use_paths(
                Some(COLOUR_FONT),
                ["tests/assets/fonts/color-fonts/samples-glyf_colr_1.ttf"],
            )
            .expect("registers");
        MASK_CACHE.set(cache);
        MASKS.set(Masks::default());
        let runs = COVERAGE_RUNS.get();
        let mut canvas = Canvas::with_options(
            400.0,
            400.0,
            CanvasOptions {
                color_space: space,
                gpu: false,
                ..CanvasOptions::default()
            },
        )
        .expect("a canvas");
        let ctx = canvas.context();
        // Once, and built once, outside the draws' `save`s: a saved state
        // shares only a typography already built, and a run is kept by the
        // typography it is laid out under -- see
        // `node::typography::LaidOutKey`.
        let font = draws[0].font;
        assert!(
            draws.iter().all(|draw| draw.font == font),
            "one font a page"
        );
        assert!(ctx.set_font_css(font), "{font}");
        ctx.measure_text(draws[0].text, None);
        for draw in draws {
            ctx.save();
            ctx.set_fill_style_css(draw.fill).expect("a colour");
            ctx.set_stroke_style_css(draw.fill).expect("a colour");
            ctx.set_line_width(2.0);
            ctx.scale(draw.scale, draw.scale);
            if let Some((x, y, w, h)) = draw.clip {
                ctx.begin_path();
                ctx.rect(x, y, w, h);
                ctx.clip(FillRule::NonZero);
            }
            ctx.set_filter_css(&format!("blur({}px)", draw.blur))
                .expect("a filter");
            let (x, y) = draw.at;
            match draw.stroke {
                true => ctx.stroke_text(draw.text, x, y, None),
                false => ctx.fill_text(draw.text, x, y, None),
            }
            ctx.restore();
        }
        let pixels = canvas
            .to_image_data(
                None,
                &ImageDataExport {
                    density,
                    pixels: PixelExportOptions {
                        premultiplied: true,
                        ..PixelExportOptions::default()
                    },
                    ..ImageDataExport::default()
                },
            )
            .expect("pixels")
            .pixels()
            .to_vec();
        MASK_CACHE.set(true);
        (pixels, COVERAGE_RUNS.get() - runs, MASKS.get())
    }

    /// `draws` with the cache and without it land the same bytes, every
    /// run takes the coverage route, and how the runs fared against the
    /// cache.
    fn alike(case: &str, draws: &[Draw], density: u32) -> Masks {
        alike_in(case, draws, density, PixelColorSpace::Srgb)
    }

    /// [`alike`] on a page in `space`.
    fn alike_in(
        case: &str,
        draws: &[Draw],
        density: u32,
        space: PixelColorSpace,
    ) -> Masks {
        let (kept, runs, masks) = {
            let _own = OwnRunCache::new();
            page(draws, density, space, true)
        };
        let (fresh, fresh_runs, fresh_masks) = {
            let _own = OwnRunCache::new();
            page(draws, density, space, false)
        };
        assert_eq!(
            (runs, fresh_runs),
            (draws.len(), draws.len()),
            "{case}: not every run took the route"
        );
        assert_eq!(
            fresh_masks,
            Masks::default(),
            "{case}: the cache was asked while off"
        );
        let off = kept.iter().zip(&fresh).filter(|(a, b)| a != b).count();
        assert_eq!(off, 0, "{case}: the cache moved {off} bytes");
        masks
    }

    const fn at(x: f32, y: f32) -> Draw {
        Draw { at: (x, y), ..RUN }
    }

    #[test]
    fn a_run_drawn_again_in_place_is_drawn_from_the_cache() {
        // The first draw of a run is not kept, so it is not counted; the
        // second keeps its result, and the third and fourth composite it.
        let masks = alike("in place", &[RUN; 4], 1);
        assert_eq!(
            masks,
            Masks {
                hits: 2,
                refused: 0,
                misses: 1
            }
        );
    }

    #[test]
    fn a_run_moved_by_whole_pixels_is_drawn_from_the_cache() {
        // Glyph masks are placed by a floor of their device position, so a
        // run moved by whole pixels covers the same and its result moves
        // with it -- across and down, far and near.
        let draws = [
            RUN,
            RUN,
            at(177.3, 131.6),
            at(121.3, 90.6),
            at(301.3, 330.6),
        ];
        let masks = alike("moved by whole pixels", &draws, 1);
        assert_eq!(
            masks,
            Masks {
                hits: 3,
                refused: 0,
                misses: 1
            }
        );
        let stroked = Draw {
            stroke: true,
            ..RUN
        };
        let draws = [
            stroked,
            stroked,
            Draw {
                at: (187.3, 141.6),
                ..stroked
            },
        ];
        let masks = alike("stroked, moved by whole pixels", &draws, 1);
        assert_eq!(
            masks,
            Masks {
                hits: 1,
                refused: 0,
                misses: 1
            }
        );
    }

    #[test]
    fn a_run_moved_by_a_fraction_is_blurred_afresh() {
        // Another sub-pixel phase is another key: nothing is found.
        let draws = [RUN, RUN, at(120.8, 90.6), at(120.3, 90.85)];
        let masks = alike("moved by a fraction", &draws, 1);
        assert_eq!(
            masks,
            Masks {
                hits: 0,
                refused: 0,
                misses: 3
            }
        );
    }

    #[test]
    fn a_run_cut_otherwise_is_filed_apart() {
        // A clip cutting the run changes the region its coverage covers, so
        // its result is filed apart from the run's unclipped one, and the
        // run drawn clear of the clip again still finds its own.
        let cut = Draw {
            clip: Some((0.0, 0.0, 140.0, 400.0)),
            ..RUN
        };
        let masks = alike("cut by a clip", &[RUN, RUN, cut, RUN], 1);
        assert_eq!(
            masks,
            Masks {
                hits: 1,
                refused: 0,
                misses: 2
            }
        );
    }

    #[test]
    fn a_run_whose_coverage_differs_is_refused() {
        // Far into a wide page Skia draws a path-drawn glyph's curves
        // otherwise at some whole-pixel origins -- see `chain::placed` --
        // so a 300px "Og" moved by whole pixels finds a result its coverage
        // is not, which is refused and blurred afresh: the comparison, not
        // the key, keeps the draw exact. Measured on CoreText: refused at 2
        // of 59 moves.
        let run = |cache: bool| {
            let library = FontLibrary::new();
            library
                .use_paths(
                    Some(OSWALD),
                    ["tests/assets/fonts/Oswald/Oswald-VariableFont_wght.ttf"],
                )
                .expect("registers");
            let _own = OwnRunCache::new();
            MASK_CACHE.set(cache);
            MASKS.set(Masks::default());
            let mut canvas = Canvas::new(8200.0, 420.0);
            canvas.set_gpu(false);
            let ctx = canvas.context();
            ctx.set_filter_css("blur(3px)").expect("a filter");
            ctx.set_fill_style_css(RUN.fill).expect("a colour");
            assert!(ctx.set_font_css("300px RunMaskOswald"));
            ctx.measure_text("Og", None);
            for k in 0..60 {
                ctx.fill_text("Og", 7600.3 + (k * 7) as f32, 330.6, None);
            }
            let pixels = canvas
                .to_image_data(None, &ImageDataExport::default())
                .expect("pixels")
                .pixels()
                .to_vec();
            MASK_CACHE.set(true);
            (pixels, MASKS.get())
        };
        let ((kept, masks), (fresh, _)) = (run(true), run(false));
        let off = kept.iter().zip(&fresh).filter(|(a, b)| a != b).count();
        assert_eq!(off, 0, "the cache moved {off} bytes");
        assert!(masks.refused > 0, "nothing was refused: {masks:?}");
        assert!(masks.hits > masks.refused, "the moves hit: {masks:?}");
    }

    #[test]
    fn a_font_change_lets_every_kept_run_go() {
        // Registering a font lets the kept runs go -- see
        // `node::typography::forget_laid_out` -- and the blurred runs kept
        // from them with them, since each holds the faces it was laid out
        // in; and so does an idle release.
        let _own = OwnRunCache::new();
        page(&[RUN; 3], 1, PixelColorSpace::Srgb, true);
        assert_eq!(held(), 1, "the run is kept");
        FontLibrary::new()
            .use_paths(
                Some(OSWALD),
                ["tests/assets/fonts/Oswald/Oswald-VariableFont_wght.ttf"],
            )
            .expect("registers");
        assert_eq!(held(), 0, "a font change kept the run");
        page(&[RUN; 3], 1, PixelColorSpace::Srgb, true);
        assert_eq!(held(), 1, "the run is kept again");
        // What `memory::reclaim` calls on an idle release.
        release_cached_runs();
        assert_eq!(held(), 0, "an idle release kept the run");
    }

    #[test]
    fn a_run_drawn_from_the_cache_lands_the_bytes_drawn_afresh() {
        // Where the counts depend on Skia's coverage, only the bytes are
        // held, and the counts are printed for the record.
        let big = Draw {
            font: "300px RunMaskOswald",
            text: "g",
            at: (40.3, 260.6),
            ..RUN
        };
        let colour = Draw {
            font: "40px RunMaskColr",
            text: "ABC",
            ..RUN
        };
        let cases: [(&str, Vec<Draw>, u32); 9] = [
            (
                "near the device's origin",
                vec![RUN, RUN, at(2.3, 30.6), at(0.3, 20.6)],
                1,
            ),
            (
                "past the device's origin",
                vec![RUN, RUN, at(-20.7, 10.6)],
                1,
            ),
            (
                "drawn as paths",
                vec![
                    big,
                    big,
                    Draw {
                        at: (61.3, 297.6),
                        ..big
                    },
                    Draw {
                        at: (93.3, 263.6),
                        ..big
                    },
                ],
                1,
            ),
            (
                "in a colour font",
                vec![
                    colour,
                    colour,
                    Draw {
                        at: (61.3, 297.6),
                        ..colour
                    },
                ],
                1,
            ),
            (
                "in other colours",
                vec![
                    RUN,
                    RUN,
                    Draw {
                        fill: "rgb(30, 160, 220)",
                        ..RUN
                    },
                    Draw {
                        fill: "rgba(200, 80, 30, 0.6)",
                        ..RUN
                    },
                ],
                1,
            ),
            (
                "at other sigmas",
                vec![
                    RUN,
                    RUN,
                    Draw { blur: 2.0, ..RUN },
                    Draw { blur: 4.5, ..RUN },
                ],
                1,
            ),
            (
                "scaled",
                vec![
                    RUN,
                    RUN,
                    Draw { scale: 1.5, ..RUN },
                    Draw {
                        scale: 1.5,
                        at: (121.3, 91.6),
                        ..RUN
                    },
                ],
                1,
            ),
            (
                "at a density of 2",
                vec![
                    Draw { blur: 2.0, ..RUN },
                    Draw { blur: 2.0, ..RUN },
                    Draw {
                        blur: 2.0,
                        at: (150.3, 70.6),
                        ..RUN
                    },
                ],
                2,
            ),
            ("at a density of 3", vec![Draw { blur: 1.5, ..RUN }; 3], 3),
        ];
        for (case, draws, density) in cases {
            let masks = alike(case, &draws, density);
            eprintln!("{case}: {masks:?}");
        }
        // On a Display P3 page, where a colour can be painted through a
        // shader that names it, so that only the key's colour tells two
        // apart.
        let p3 = [
            RUN,
            RUN,
            Draw {
                fill: "rgb(30, 160, 220)",
                ..RUN
            },
            Draw {
                fill: "rgb(30, 160, 220)",
                ..RUN
            },
            Draw {
                fill: "rgba(200, 80, 30, 0.6)",
                ..RUN
            },
        ];
        let masks = alike_in(
            "on a Display P3 page",
            &p3,
            1,
            PixelColorSpace::DisplayP3,
        );
        eprintln!("on a Display P3 page: {masks:?}");
    }
}
