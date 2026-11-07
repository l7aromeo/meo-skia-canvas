//! Blurred runs of text kept for drawing again: a run the coverage route
//! plays -- see `layer::CoverageRun` -- keeps what it composited, so that the
//! same run drawn again where it covers the same pixels composites that
//! rather than blurring its coverage again.
//!
//! What a run covers depends on where it lies, so a kept result is never
//! trusted on its key alone. The key finds the result a run drawn at the same
//! sub-pixel phase left -- see [`RunKey`] -- and, unless where its glyphs lie
//! decides its coverage (below), the run's coverage is drawn again on the
//! plane the route would blur, then compared byte for byte with the coverage
//! the result was blurred from. Only where the two are the same
//! is the kept result composited, moved by the whole pixels between them; any
//! difference blurs afresh. Skia places a glyph's mask by a floor of its
//! device position (`prepare_for_direct_mask_drawing`,
//! `skia/src/core/SkGlyphRunPainter.cpp` under the skia-bindings crate, lines
//! 121 to 163), so a run moved by whole pixels usually covers the same; but
//! that position is summed in floats whose rounding depends on the whole
//! part, and a glyph past 256 device pixels is drawn as a path at its exact
//! place (`SkStrikeSpec::ShouldDrawAsPath`, `SkStrikeSpec.cpp`, lines 101 to
//! 126). The comparison is what makes a hit exact, not the key.
//!
//! Where every glyph of a run is drawn as a mask, the comparison is not
//! needed: a mask is placed by the floor of its device position and drawn
//! from the strike's image for that position's quarter-pixel phase, both
//! read off the same sum (`SkPackedGlyphID::PackIDSkPoint`, `SkGlyph.h`), and
//! every rounding spec Skia has puts the edges between one floor or phase
//! and the next on an eighth of a pixel -- see [`PLACEMENT_STEPS`]. So a run
//! whose glyphs each lie clear of those edges, at the kept place and at this
//! one, and whose eighths all moved by the same whole pixels as its origin,
//! blits the same masks in the same order, moved; its coverage is the kept
//! coverage moved, and [`kept`] composites the kept result without drawing
//! the run again. A glyph near an edge, a run drawn as paths or sheared, and
//! a decorated run fall back to the comparison.

#[cfg(test)]
use std::cell::Cell;
use std::{
    hash::{Hash, Hasher},
    iter::zip,
    sync::{Arc, LazyLock},
};

use parking_lot::Mutex;
use skia_safe::{Canvas as SkCanvas, IRect, Matrix, Paint, PaintStyle};

use crate::{
    cache_store::{Budget, Store, StoreKey, Stored},
    context::chain::{
        Outcome,
        coverage::{Coverage, Placement},
    },
    node::typography::{RunPaint, Shear},
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

/// The steps a device pixel Skia's placement of a glyph's mask turns on.
/// The mask's whole pixels and its sub-pixel phase are both read off the
/// position plus half a sample (`SkGlyphPositionRoundingSpec`, `SkGlyph.cpp`
/// lines 695 to 712): plus a half on an axis without sub-pixel positions,
/// which then turns at whole pixels, so at a position's halves; and plus an
/// eighth on one with them, which turns at quarters
/// (`SkPackedGlyphID::kSubpixelRound`, `SkGlyph.h` line 70), so at its odd
/// eighths. Either way it turns only on an eighth -- see [`turns_at`] -- so a
/// glyph's eighths decide its placement whatever the font's sub-pixel flag
/// and the axis Skia aligns it to.
const PLACEMENT_STEPS: f64 = 8.0;

/// Whether Skia's placement of a mask can turn at `step`
/// [`PLACEMENT_STEPS`]ths of a pixel: at an odd eighth or a half -- see
/// [`PLACEMENT_STEPS`] -- and not at a whole pixel or a quarter, where a
/// run's baseline commonly lies.
fn turns_at(step: i64) -> bool {
    let per_pixel = PLACEMENT_STEPS as i64;
    let within = step.rem_euclid(per_pixel);
    let half = within == per_pixel / 2;
    #[cfg(test)]
    if RULE.get() == Rule::HalvesOnly {
        return half;
    }
    half || within % 2 == 1
}

/// How far Skia's f32 sums of a glyph's position can fall from the exact
/// one, relative to the largest magnitude summed. Skia rounds a position at
/// most a dozen times on its way to a mask: the transforms concatenated, the
/// blob's origin pre-translated and the half sample post-translated, the
/// position mapped, and our own sums of the anchor and the run's origin. Each
/// rounding is within 2^-24 of the value it rounds, so all of them together
/// are within 2^-20; this allows four times that.
const POSITION_SLACK: f64 = 1.0 / (1u32 << 18) as f64;

/// How far `PackIDSkPoint`'s `(x - floor(x)) + 1.0f` can move a phase: one
/// rounding of a number below two, half its last place (`SkGlyph.h` lines
/// 157 to 180).
const PHASE_SLACK: f64 = 1.0 / (1u32 << 23) as f64;

/// The longest a column of a run's text matrix can be on the device for its
/// glyphs to be drawn as masks: Skia draws them as paths past 256
/// (`SkStrikeSpec::ShouldDrawAsPath`, `SkStrikeSpec.cpp` lines 117 to 126),
/// placed at their exact position. A pixel short, for the f32 sum Skia
/// compares.
const MASK_SIDE_LIMIT: f64 = 255.0;

/// [`POSITION_SLACK`] and [`PHASE_SLACK`] for a position whose sums reach
/// `largest`, or none where [`RULE`] says so.
fn slack(largest: f64) -> f64 {
    #[cfg(test)]
    if RULE.get() == Rule::NoSlack {
        return 0.0;
    }
    POSITION_SLACK * largest + PHASE_SLACK
}

/// Each glyph's position on the device in [`PLACEMENT_STEPS`]ths of a pixel,
/// floored, for `kept` drawn under `to_picture`, where every glyph lies
/// further than Skia's sums can err from a step its placement turns at -- see
/// [`turns_at`] -- and is drawn as a mask; `None` otherwise, for the run to be
/// compared instead. `offset` is how far, at most, the plane's origin lies from
/// the device's along each axis: Skia's sums run in the plane's pixels.
fn eighths(
    kept: &Kept,
    to_picture: &Matrix,
    offset: (u32, u32),
) -> Option<Arc<[[i64; 2]]>> {
    let (outer, anchor) = (to_picture, &kept.anchor);
    if !(outer.is_scale_translate() && anchor.is_scale_translate())
        || kept.paint.hairline()
    {
        return None;
    }
    let axis = |m: &Matrix| {
        [
            (f64::from(m.scale_x()), f64::from(m.translate_x())),
            (f64::from(m.scale_y()), f64::from(m.translate_y())),
        ]
    };
    let (outer, anchor) = (axis(outer), axis(anchor));
    let scale = [outer[0].0 * anchor[0].0, outer[1].0 * anchor[1].0];
    let shift = [f64::from(offset.0), f64::from(offset.1)];
    let mut placed = Vec::new();
    for (font, origin, positions, shear) in kept.run.placed_runs()? {
        match shear {
            Shear::None => {}
            // Drawn as nothing.
            Shear::Flat => continue,
            Shear::By(_) => return None,
        }
        // Unskewed under a transform that only scales and translates, no
        // run is drawn from its outlines -- see
        // `typography::past_freetype_shear` -- and the text matrix's columns
        // are its two scales.
        let size = f64::from(font.size());
        if font.skew_x() != 0.0
            || (scale[0] * size * f64::from(font.scale_x())).abs()
                >= MASK_SIDE_LIMIT
            || (scale[1] * size).abs() >= MASK_SIDE_LIMIT
        {
            return None;
        }
        #[cfg(test)]
        if RULE.get() == Rule::OriginOnly {
            continue;
        }
        for position in positions {
            let parts = [(origin.x, position.x), (origin.y, position.y)];
            let mut glyph = [0; 2];
            for (k, step) in glyph.iter_mut().enumerate() {
                let ((o_scale, o_shift), (a_scale, a_shift)) =
                    (outer[k], anchor[k]);
                let (run, at) = (f64::from(parts[k].0), f64::from(parts[k].1));
                let exact =
                    o_scale * (a_scale * (run + at) + a_shift) + o_shift;
                // The largest magnitude any of Skia's sums of this position
                // can reach, the half sample included.
                let largest = o_scale.abs()
                    * (a_scale.abs() * (run.abs() + at.abs()) + a_shift.abs())
                    + o_shift.abs()
                    + shift[k]
                    + 1.0;
                let steps = exact * PLACEMENT_STEPS;
                let below = steps.floor() as i64;
                let fraction = steps - steps.floor();
                // How near the position lies to a step it turns at, in
                // steps: only the two steps about it can be within reach.
                let near = match (turns_at(below), turns_at(below + 1)) {
                    (true, true) => fraction.min(1.0 - fraction),
                    (true, false) => fraction,
                    (false, true) => 1.0 - fraction,
                    (false, false) => 1.0,
                };
                if near < slack(largest) * PLACEMENT_STEPS {
                    return None;
                }
                *step = below;
            }
            placed.push(glyph);
        }
    }
    #[cfg(test)]
    if RULE.get() == Rule::OriginOnly {
        // The run's anchor alone.
        let origin = [0, 1].map(|k| {
            let ((o_scale, o_shift), (_, a_shift)) = (outer[k], anchor[k]);
            ((o_scale * a_shift + o_shift) * PLACEMENT_STEPS).floor() as i64
        });
        return Some([origin].into());
    }
    Some(placed.into())
}

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

    /// Whether the paint strokes a hairline, which Skia draws as paths
    /// wherever it lies (`SkStrikeSpec::ShouldDrawAsPath`).
    fn hairline(&self) -> bool {
        self.style != PaintStyle::Fill as u8
            && f32::from_bits(self.stroke[0]) == 0.0
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

/// A [`RunKey`] and the device pixels the run's coverage covers, is read
/// from and inks, relative to the whole pixels of its origin -- see
/// `chain::coverage::Placement`: what a kept result is filed under. A
/// run whose region a clip or the device's edge cuts otherwise is filed apart,
/// so that a run drawn there and one drawn clear of it each find their own.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Filed {
    key: RunKey,
    region: [i32; 4],
    read: [i32; 4],
    inked: [i32; 4],
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
    /// The run and where it is drawn, for [`eighths`].
    kept: Kept,
    /// The transform its picture is played in.
    to_picture: Matrix,
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
            kept: kept.clone(),
            to_picture: *to_picture,
        })
    }

    /// What this run's result is filed under when its coverage is placed at
    /// `at`: the key and the rectangles the coverage is worked over and read
    /// from, relative to the whole pixels of the run's origin.
    fn filed(&self, at: &Placement) -> Filed {
        let (x, y) = self.origin;
        let relative = |r: IRect| {
            let r = r.with_offset((-x, -y));
            [r.left, r.top, r.right, r.bottom]
        };
        Filed {
            key: self.key.clone(),
            region: relative(at.worked),
            read: relative(at.region),
            inked: relative(at.inked),
        }
    }

    /// Each glyph's eighths where its coverage is placed at `at` -- see
    /// [`eighths`].
    fn eighths(&self, at: &Placement) -> Option<Arc<[[i64; 2]]>> {
        // The plane starts at `drawn`, or at the region where no plane that
        // large could be made -- see `Coverage::of`.
        let farthest = |a: i32, b: i32| a.unsigned_abs().max(b.unsigned_abs());
        eighths(
            &self.kept,
            &self.to_picture,
            (
                farthest(at.drawn.left, at.region.left),
                farthest(at.drawn.top, at.region.top),
            ),
        )
    }
}

/// A kept run's result: what it composited, the coverage of the inked
/// rectangle that was blurred to make it -- see `Coverage::ink` -- and the
/// whole pixels of the origin it was drawn at.
#[derive(Clone)]
pub(crate) struct Mask {
    pub(crate) outcome: Outcome,
    pub(crate) coverage: Arc<[u8]>,
    pub(crate) origin: (i32, i32),
    /// Each glyph's eighths where the run was drawn -- see [`eighths`] --
    /// or `None` where they do not decide its coverage.
    pub(crate) eighths: Option<Arc<[[i64; 2]]>>,
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

/// The result kept for `placed` at `at`, moved to where it is drawn now,
/// where its glyphs' eighths decide its coverage -- see [`eighths`] -- and
/// every glyph moved by the whole pixels its origin did: composited without
/// the run's coverage being drawn again. `None` otherwise, for the run to be
/// drawn and compared -- see [`drawn`].
///
/// `redraw` draws the run's coverage. Only a test build calls it, to hold
/// what this trusts to the comparison it stands in for.
pub(crate) fn kept(
    placed: &Placed,
    at: &Placement,
    redraw: impl FnOnce() -> Option<Coverage>,
) -> Option<Outcome> {
    #[cfg(test)]
    if !MASK_CACHE.get() {
        return None;
    }
    let mask = get(&placed.filed(at))?;
    let was = mask.eighths.as_deref()?;
    let now = placed.eighths(at)?;
    let (x, y) = placed.origin;
    let by = (x - mask.origin.0, y - mask.origin.1);
    let moved = [by.0, by.1].map(|d| i64::from(d) * PLACEMENT_STEPS as i64);
    let alike = was.len() == now.len()
        && zip(was, now.iter())
            .all(|(was, now)| [now[0] - was[0], now[1] - was[1]] == moved);
    if !alike {
        return None;
    }
    let outcome = mask.outcome.at(by)?;
    #[cfg(test)]
    {
        let same = redraw().is_some_and(|sharp| {
            let (ink, inked) = sharp.ink();
            sharp.region() == at.worked
                && inked == at.inked
                && *mask.coverage == *ink
        });
        count(|masks| {
            masks.skipped += 1;
            masks.wrong += usize::from(!same);
        });
    }
    #[cfg(not(test))]
    let _ = redraw;
    Some(outcome)
}

/// What a run whose coverage, placed at `at`, is `sharp` composites: the
/// result kept for `placed` and `at` -- see [`Filed`] -- where the coverage
/// it was blurred from is `sharp`'s to the byte, moved by the whole pixels
/// between the two origins; otherwise `colour` of `sharp`, kept for the next
/// draw in place of the one refused, with its glyphs' eighths for [`kept`].
/// A run with nowhere `placed` is coloured and not kept.
pub(crate) fn drawn(
    placed: Option<Placed>,
    at: &Placement,
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
    let filed = placed.filed(at);
    if let Some(mask) = get(&filed) {
        let alike = *mask.coverage == *sharp.ink().0;
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
    let coverage: Arc<[u8]> = sharp.ink().0.into();
    let outcome = colour(sharp)?;
    put(
        filed,
        Mask {
            outcome: outcome.clone(),
            coverage,
            origin: placed.origin,
            eighths: placed.eighths(at),
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

/// The rule [`kept`] decides by, and the mutants of it the tests must
/// refuse.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Rule {
    /// As built.
    Exact,
    /// No allowance for Skia's own rounding of a position.
    NoSlack,
    /// The run's origin alone, not each glyph.
    OriginOnly,
    /// The halves an axis without sub-pixel positions turns at, not the
    /// odd eighths one with them does.
    HalvesOnly,
}

/// How the runs played on a thread fared against the cache: drawn from
/// what it kept after a comparison; refused, because the coverage kept under
/// the run's key was not the run's; blurred afresh, the refused among them;
/// and drawn from what it kept without a comparison.
#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Masks {
    pub(crate) hits: usize,
    pub(crate) refused: usize,
    pub(crate) misses: usize,
    /// Drawn from the cache by [`kept`], without a comparison.
    pub(crate) skipped: usize,
    /// Of those, the ones whose coverage drawn again was not the kept
    /// coverage: each a wrong draw outside a test.
    pub(crate) wrong: usize,
}

#[cfg(test)]
thread_local! {
    /// Whether a run may be drawn from what this cache keeps, so that a test
    /// can hold the cache to the route it stands in for.
    pub(crate) static MASK_CACHE: Cell<bool> = const { Cell::new(true) };
    /// How the runs played on this thread fared -- see [`Masks`].
    pub(crate) static MASKS: Cell<Masks> = const {
        Cell::new(Masks {
            hits: 0,
            refused: 0,
            misses: 0,
            skipped: 0,
            wrong: 0,
        })
    };
    /// The rule [`kept`] decides by: [`Rule::Exact`], or a mutant of it a
    /// test shows the sweep refuses.
    pub(crate) static RULE: Cell<Rule> = const { Cell::new(Rule::Exact) };
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
        MASK_CACHE, MASKS, Masks, OwnRunCache, RULE, Rule, held,
        release_cached_runs,
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
        assert_eq!(
            masks.wrong, 0,
            "{case}: a run composited without a comparison covered otherwise"
        );
        masks
    }

    const fn at(x: f32, y: f32) -> Draw {
        Draw { at: (x, y), ..RUN }
    }

    #[test]
    fn a_run_drawn_again_in_place_is_drawn_from_the_cache() {
        // The first draw of a run is not kept, so it is not counted; the
        // second keeps its result, and the third and fourth composite it,
        // decided by where their glyphs lie without drawing them again.
        let masks = alike("in place", &[RUN; 4], 1);
        assert_eq!(
            masks,
            Masks {
                misses: 1,
                skipped: 2,
                ..Masks::default()
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
                misses: 1,
                skipped: 3,
                ..Masks::default()
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
                misses: 1,
                skipped: 1,
                ..Masks::default()
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
                misses: 3,
                ..Masks::default()
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
                misses: 2,
                skipped: 1,
                ..Masks::default()
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

    #[test]
    fn a_run_of_any_size_lands_the_bytes_drawn_afresh() {
        // From a size whose masks are a few pixels to sizes past the 256
        // device pixels Skia draws as paths from, kept and moved by whole
        // pixels: masks are trusted, paths are compared.
        for px in ["8", "13", "17.3", "31", "64", "127.5", "200", "254", "260"]
        {
            let font: &'static str =
                Box::leak(format!("{px}px RunMaskOswald").into_boxed_str());
            let run = Draw { font, ..RUN };
            let draws = [
                run,
                run,
                Draw {
                    at: (177.3, 331.6),
                    ..run
                },
                Draw {
                    at: (21.3, 290.6),
                    ..run
                },
            ];
            let masks = alike(font, &draws, 1);
            eprintln!("{font}: {masks:?}");
            // At 254px the moved draws run past the page's edge and are
            // filed apart, so only their bytes are held.
            match px {
                "254" => {}
                "260" => assert_eq!(masks.skipped, 0, "{font}: {masks:?}"),
                _ => assert!(masks.skipped > 0, "{font}: {masks:?}"),
            }
        }
    }

    #[test]
    fn a_page_of_runs_at_whole_pixels_composites_every_kept_run() {
        // The scene `text-blur-sans-*` times: fifty "Ag" in sans-serif on a
        // page of 400 by 300, at whole pixels, cleared and drawn again. By
        // the third page every run is kept where it is drawn, and each is
        // composited from what was kept without drawing it -- but under a
        // blur of 10, past the sigma the coverage route takes, none is.
        for (px, blur, trusted) in [
            (40, 1, 50),
            (40, 3, 50),
            (40, 10, 0),
            (20, 3, 50),
            (80, 3, 50),
        ] {
            let _own = OwnRunCache::new();
            let mut canvas = Canvas::new(400.0, 300.0);
            canvas.set_gpu(false);
            let ctx = canvas.context();
            assert!(ctx.set_font_css(&format!("{px}px sans-serif")));
            ctx.set_filter_css(&format!("blur({blur}px)"))
                .expect("a filter");
            for page in 0..3 {
                if page == 2 {
                    MASKS.set(Masks::default());
                }
                let ctx = canvas.context();
                ctx.clear_rect(0.0, 0.0, 400.0, 300.0);
                for i in 0..50 {
                    let (x, y) = ((i % 10) * 38, 40 + (i / 10) * 50);
                    ctx.fill_text("Ag", x as f32, y as f32, None);
                }
                canvas
                    .to_image_data(None, &ImageDataExport::default())
                    .expect("pixels");
            }
            let masks = MASKS.get();
            eprintln!("{px}px blur({blur}px), third page: {masks:?}");
            assert_eq!(masks.wrong, 0, "{px}px blur({blur}px): {masks:?}");
            assert_eq!(
                masks.skipped, trusted,
                "{px}px blur({blur}px): {masks:?}"
            );
        }
    }

    /// A draw on [`wide`]'s page: where, and the rectangle it is cut to.
    type WideDraw = ((f32, f32), Option<(f32, f32, f32, f32)>);

    /// `draws` of [`RUN`]'s "Ag" on one CPU page 8,400 by 200, under the
    /// rule `rule`, each cut to `clip` where it names one, with the cache on
    /// or off: the bytes and how the runs fared. The run is drawn once first
    /// and covered over, so that every draw counted finds it laid out.
    fn wide(rule: Rule, cache: bool, draws: &[WideDraw]) -> (Vec<u8>, Masks) {
        let library = FontLibrary::new();
        library
            .use_paths(
                Some(OSWALD),
                ["tests/assets/fonts/Oswald/Oswald-VariableFont_wght.ttf"],
            )
            .expect("registers");
        let _own = OwnRunCache::new();
        MASK_CACHE.set(cache);
        RULE.set(rule);
        let mut canvas = Canvas::new(8400.0, 200.0);
        canvas.set_gpu(false);
        let ctx = canvas.context();
        assert!(ctx.set_font_css(RUN.font));
        ctx.set_fill_style_css(RUN.fill).expect("a colour");
        ctx.set_filter_css("blur(3px)").expect("a filter");
        ctx.measure_text(RUN.text, None);
        ctx.fill_text(RUN.text, 4000.0, 100.0, None);
        MASKS.set(Masks::default());
        for &((x, y), clip) in draws {
            ctx.save();
            if let Some((cx, cy, cw, ch)) = clip {
                ctx.begin_path();
                ctx.rect(cx, cy, cw, ch);
                ctx.clip(FillRule::NonZero);
            }
            ctx.fill_text(RUN.text, x, y, None);
            ctx.restore();
        }
        let pixels = canvas
            .to_image_data(None, &ImageDataExport::default())
            .expect("pixels")
            .pixels()
            .to_vec();
        let masks = MASKS.get();
        MASK_CACHE.set(true);
        RULE.set(Rule::Exact);
        (pixels, masks)
    }

    /// How many bytes two pages part by.
    fn parted(a: &[u8], b: &[u8]) -> usize {
        a.iter().zip(b).filter(|(a, b)| a != b).count()
    }

    #[test]
    fn a_glyph_near_where_its_mask_turns_is_compared() {
        // The run's "g" placed at positions stepping through the edges where
        // Skia's placement of its mask turns -- an odd eighth of a pixel
        // where the font is positioned by sub-pixels, a half where it is
        // not -- once near the page's left and once moved by 8,000 pixels,
        // where an f32 holds a position to 2^-11 of a pixel and Skia's sum of
        // the glyph's position rounds across an edge the exact position lies
        // short of. The rule as built refuses a glyph that near an edge and
        // compares it; each mutant trusts one it should not, and composites
        // a result its coverage is not.
        let library = FontLibrary::new();
        library
            .use_paths(
                Some(OSWALD),
                ["tests/assets/fonts/Oswald/Oswald-VariableFont_wght.ttf"],
            )
            .expect("registers");
        let mut probe = Canvas::new(10.0, 10.0);
        let ctx = probe.context();
        assert!(ctx.set_font_css(RUN.font));
        // Where "g" lies after "A", kerning and all.
        let after = ctx.measure_text("Ag", None).width
            - ctx.measure_text("g", None).width;
        let step = 2f32.powi(-13);
        // Every odd eighth and the half, so that the "g" lies near each
        // while the "A", a fixed distance before it, lies clear of some.
        let draws: Vec<_> = [0.125f32, 0.375, 0.5, 0.625, 0.875]
            .into_iter()
            .flat_map(|edge| {
                (-96..96).flat_map(move |k| {
                    let phase = (edge - after.fract() + k as f32 * step)
                        .rem_euclid(1.0);
                    [
                        ((120.0 + phase, 100.0), None),
                        ((8120.0 + phase, 100.0), None),
                    ]
                })
            })
            .collect();
        let (fresh, _) = wide(Rule::Exact, false, &draws);
        let (exact, masks) = wide(Rule::Exact, true, &draws);
        eprintln!("as built: {masks:?}");
        assert_eq!(parted(&exact, &fresh), 0, "the rule moved bytes");
        assert_eq!(masks.wrong, 0, "the rule trusted a glyph: {masks:?}");
        assert!(masks.skipped > 0, "nothing was trusted: {masks:?}");
        assert!(
            masks.hits + masks.refused > 0,
            "nothing near an edge was compared: {masks:?}"
        );
        for rule in [Rule::NoSlack, Rule::OriginOnly] {
            let (_, masks) = wide(rule, true, &draws);
            eprintln!("{rule:?}: {masks:?}");
            assert!(masks.wrong > 0, "{rule:?} drew nothing wrong: {masks:?}");
        }
        // The odd eighths are edges only where the glyphs are positioned by
        // sub-pixels, which this platform's scaler may decline: a run a
        // quarter of a pixel along lands other bytes where it is.
        let (whole, _) = wide(Rule::Exact, false, &[((120.0, 100.0), None)]);
        let (quarter, _) = wide(Rule::Exact, false, &[((120.25, 100.0), None)]);
        let subpixel = parted(&whole, &quarter) > 0;
        let (_, masks) = wide(Rule::HalvesOnly, true, &draws);
        eprintln!("HalvesOnly, sub-pixel positions {subpixel}: {masks:?}");
        match subpixel {
            true => assert!(masks.wrong > 0, "HalvesOnly: {masks:?}"),
            false => assert_eq!(masks.wrong, 0, "HalvesOnly: {masks:?}"),
        }
    }

    #[test]
    fn a_run_cut_anywhere_by_a_clip_lands_the_bytes_drawn_afresh() {
        // Clips ending at each column and row from well before the run to
        // well past it, from each side, each between two draws of it uncut:
        // a cut run is filed apart by what is read of it, and the uncut one
        // still composites what it kept. No clip tried here reads or inks a
        // run otherwise while what its blur works on stays the same, so
        // filing by the rectangles read and inked as well is not shown to be
        // needed; they are kept because the coverage is read from them.
        let clips = (0..100).flat_map(|k| {
            let (x, y) = (100.0 + k as f32, 50.0 + k as f32);
            [
                (0.0, 0.0, x, 200.0),
                (x, 0.0, 400.0, 200.0),
                (0.0, 0.0, 400.0, y),
                (0.0, y, 400.0, 200.0),
            ]
        });
        let draws: Vec<_> = clips
            .flat_map(|clip| {
                [((120.3, 100.6), None), ((120.3, 100.6), Some(clip))]
            })
            .collect();
        let (fresh, _) = wide(Rule::Exact, false, &draws);
        let (exact, masks) = wide(Rule::Exact, true, &draws);
        eprintln!("as built: {masks:?}");
        assert_eq!(parted(&exact, &fresh), 0, "the rule moved bytes");
        assert_eq!(masks.wrong, 0, "a cut run composited otherwise: {masks:?}");
        assert!(masks.skipped > 0, "nothing was trusted: {masks:?}");
    }
}
