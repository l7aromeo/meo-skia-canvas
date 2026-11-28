//! A `source-over` composite on the GPU of an effect's whole levels under a
//! coverage, in the arithmetic the CPU canvas's eight-bit pipeline uses.
//!
//! The CPU composites a draw's bytes `s` over the page's `d` under the
//! coverage `c` in one of two orders (`SkRasterPipelineBlitter.cpp`). A
//! paint the blitter takes as opaque -- its shader opaque and its alpha 1 --
//! has `source-over` drawn as `src`, and the coverage lerps toward the page:
//! `div255(d * (255 - c) + s * c)`. Any other paint has its coverage scaled
//! in first, as `SkBlendMode_ShouldPreScaleCoverage` says for
//! `source-over`: `s' = div255(s * c)`, then
//! `s' + div255_accurate(d * (255 - s'.a))`. The two divisions are the
//! eight-bit ones of `SkRasterPipeline_opts.h`. The coverage's, in the
//! `scale_*` and `lerp` stages, rounds to the nearest level on aarch64 and
//! under the exact-coverage patch, and elsewhere is `(v + 255) >> 8` -- see
//! [`EXACT_COVERAGE`]. `div255_accurate`, which the `source-over` stage
//! takes, rounds to the nearest level everywhere. Measured on the edge scenes
//! of `tests/gpu_gradient.rs` and the scenes of
//! `context2d::exact_composite_tests`, those two land every byte the CPU
//! canvas stores, on aarch64 and on x86-64. Ganesh composites `source-over`
//! in its fixed-function stage instead: Metal in half precision, a level off
//! near a tie; llvmpipe with the two terms rounded apart, and NVIDIA with
//! the source's cut first, a level off at any distance from one (#783).
//!
//! The draws are made two ways. On the canvas's own surface, a [`Batch`]
//! holds the draws that do not meet, takes one snapshot of the pixels they
//! cover, and draws each over it with `src`, through [`SNAPPED`], which
//! reads the destination from the snapshot. Inside a layer, whose pixels
//! the surface does not hold, a draw takes [`blended`]'s blender, which
//! reads the destination itself: a blender sees the source and the
//! destination and nothing else, not even where it is, so the source's four
//! bytes and the coverage's one go to it packed in the source's four
//! channels -- see [`PACKED`]. Either hands on a whole level, which a
//! half-precision output carries to an eight-bit target unchanged.
//!
//! [`float_over`] is the same composite for a source the CPU draws on its
//! float pipeline, rounded once, which a layer takes from a raster of
//! sixteen bits a channel -- see `layer::rasterized_onto`.

#[cfg(test)]
use std::cell::Cell;
use std::collections::HashMap;

use skia_safe::{
    BlendMode, Blender, Canvas as SkCanvas, Color, Data, FilterMode, IRect,
    Matrix, MipmapMode, Paint, RuntimeEffect, SamplingOptions, Shader,
    TileMode, runtime_effect::ChildPtr, shaders,
};

use crate::cache_map::CacheHasher;

/// Whether the CPU's eight-bit pipeline applies a coverage to the nearest
/// level, or as `(v + 255) >> 8`. Its `scale_*` and `lerp` stages divide
/// through `div255`, which NEON computes to the nearest level and every
/// other backend of `SkRasterPipeline_opts.h` as `(v + 255) >> 8` -- unless
/// Skia carries `containers/skia-lowp-exact-coverage.patch`
/// (`cfg(meo_lowp_exact_coverage)`), which takes them through
/// `div255_accurate` on every backend. A paint's alpha is another stage,
/// `scale_1_float`, under another patch -- see `exact_gradient`'s
/// `EXACT_SCALE`.
const EXACT_COVERAGE: bool =
    cfg!(any(target_arch = "aarch64", meo_lowp_exact_coverage));

/// The SkSL of the CPU's composite -- see the module's doc -- with
/// `div255` both ways the CPU computes it, nearest where `nearest` -- see
/// [`EXACT_COVERAGE`] -- `div255_accurate` always nearest, and `opaque`
/// whether the CPU draws the paint as `src`.
const COMPOSITE: &str = r#"
uniform int nearest;
uniform int opaque;
float4 div255_accurate(float4 v) {
    return floor((v + 128.0 + floor((v + 128.0) / 256.0)) / 256.0);
}
float4 div255(float4 v) {
    return nearest != 0 ? div255_accurate(v) : floor((v + 255.0) / 256.0);
}
float4 composite(float4 s, float c, float4 d) {
    if (opaque != 0) {
        return div255(d * (255.0 - c) + s * c);
    }
    float4 scaled = div255(s * c);
    return scaled + div255_accurate(d * (255.0 - scaled.a));
}
"#;

/// The SkSL packing an effect's bytes and a coverage's into a source: each
/// colour byte times eight plus three, three and two bits of the alpha's,
/// over 2048, and the coverage over 255. Every multiple of 1/2048 below 1
/// is exact in half precision, so the packing survives the half the GPU
/// may carry a source in.
const PACKED: &str = r#"
uniform shader effect;
uniform shader coverage;
half4 main(float2 p) {
    float4 s = floor(float4(effect.eval(p)) * 255.0 + 0.5);
    float c = floor(float(coverage.eval(p).a) * 255.0 + 0.5);
    float hi = floor(s.a / 32.0);
    float mid = floor(s.a / 4.0) - hi * 8.0;
    float lo = s.a - floor(s.a / 4.0) * 4.0;
    return half4(float4(s.r * 8.0 + hi, s.g * 8.0 + mid, s.b * 8.0 + lo, 0.0) / 2048.0
                 + float4(0.0, 0.0, 0.0, c / 255.0));
}
"#;

/// The SkSL of the blender: [`PACKED`]'s bytes unpacked and composited
/// over the destination's as the CPU composites them.
const OVER_PACKED: &str = r#"
half4 main(half4 src, half4 dst) {
    float4 q = floor(float4(src.rgb, 0.0) * 2048.0 + 0.5);
    float4 s = floor(q / 8.0);
    float4 bits = q - s * 8.0;
    s.a = bits.r * 32.0 + bits.g * 4.0 + bits.b;
    float c = floor(float(src.a) * 255.0 + 0.5);
    float4 d = floor(float4(dst) * 255.0 + 0.5);
    return half4(composite(s, c, d) / 255.0);
}
"#;

/// The SkSL a [`Batch`] draws with: the effect's bytes, the coverage's and
/// the destination's, read from a snapshot of it, composited as the CPU
/// composites them, to be written with `src`.
const SNAPPED: &str = r#"
uniform shader effect;
uniform shader coverage;
uniform shader destination;
half4 main(float2 p) {
    float4 s = floor(float4(effect.eval(p)) * 255.0 + 0.5);
    float c = floor(float(coverage.eval(p).a) * 255.0 + 0.5);
    float4 d = floor(float4(destination.eval(p)) * 255.0 + 0.5);
    return half4(composite(s, c, d) / 255.0);
}
"#;

/// The SkSL of the CPU's float `source-over` (`highp`): the unrounded
/// source over the destination's whole levels, rounded once as the CPU's
/// store rounds, a half to the even level where `ties_even` -- see
/// `exact_gradient::HIGHP_TIES_TO_EVEN`. Exact to the precision the source
/// reaches it in, which a blender is handed as a half: 32 bits on Vulkan's
/// desktop drivers, 16 on Metal.
const FLOAT_OVER: &str = r#"
uniform int ties_even;
half4 main(half4 src, half4 dst) {
    float4 s = float4(src);
    float4 d = floor(float4(dst) * 255.0 + 0.5) / 255.0;
    float4 v = clamp(s + d * (1.0 - s.a), 0.0, 1.0) * 255.0;
    float4 r = floor(v + 0.5);
    if (ties_even != 0) {
        for (int k = 0; k < 4; k++) {
            if (r[k] - v[k] == 0.5 && mod(r[k], 2.0) == 1.0) {
                r[k] -= 1.0;
            }
        }
    }
    return half4(r / 255.0);
}
"#;

thread_local! {
    /// [`FLOAT_OVER`] compiled, once per thread.
    static FLOAT: Option<RuntimeEffect> =
        RuntimeEffect::make_for_blender(FLOAT_OVER, None).ok();
    /// [`PACKED`] compiled, once per thread.
    static PACKER: Option<RuntimeEffect> =
        RuntimeEffect::make_for_shader(PACKED, None).ok();
    /// [`OVER_PACKED`] compiled, once per thread.
    static OVER: Option<RuntimeEffect> = RuntimeEffect::make_for_blender(
        format!("{COMPOSITE}{OVER_PACKED}"),
        None,
    )
    .ok();
    /// [`SNAPPED`] compiled, once per thread.
    static SNAPPER: Option<RuntimeEffect> = RuntimeEffect::make_for_shader(
        format!("{COMPOSITE}{SNAPPED}"),
        None,
    )
    .ok();
}

#[cfg(test)]
thread_local! {
    /// How many snapshots the batches flushed on this thread took.
    pub(crate) static SNAPSHOTS: Cell<usize> = const { Cell::new(0) };
    /// How many pairs of rectangles [`share_a_pixel`] compared on this
    /// thread.
    static COMPARED: Cell<usize> = const { Cell::new(0) };
    /// How many cells [`Held`] filed a draw in or read for a check on this
    /// thread.
    static CELLS_TOUCHED: Cell<usize> = const { Cell::new(0) };
}

/// The side, in device pixels, of the square cells [`Held`] files a
/// [`Batch`]'s draws by. The batch is for runs of small fills: each of the
/// 2,000 fills of 9.5 by 9.2 pixels in #844's scene lands in at most four
/// cells this size, and as held draws never meet, a cell holds no more of
/// them than fit in it apart -- nine, on that scene's 12-pixel grid. Smaller
/// cells file each draw in more of them; larger ones put more in each.
const HELD_CELL_PX: i32 = 32;

/// The most cells [`Held`] files one draw in, or reads for one check. A
/// draw spanning more -- a page-sized fill spans hundreds -- is kept in a
/// list every check reads, and a check spanning more reads every held draw,
/// as a scan of them does: no draw costs more than this many cells to file
/// or to look up.
const HELD_CELLS_MOST: i64 = 64;

/// The uniforms [`COMPOSITE`] takes, for a paint the CPU draws as `src`
/// where `opaque`.
fn uniforms(opaque: bool) -> Data {
    let values: Vec<u8> = [i32::from(EXACT_COVERAGE), i32::from(opaque)]
        .iter()
        .flat_map(|value| value.to_ne_bytes())
        .collect();
    Data::new_copy(&values)
}

/// Sets `paint` to composite `effect`, whose levels are whole, under
/// `coverage`'s alpha -- opaque where `None` -- `source-over` as the CPU
/// does, through a blender that reads the destination; `false`, `paint`
/// untouched, where Skia refuses either effect. `opaque` is whether the CPU
/// draws the paint as `src` -- see the module's doc.
pub(crate) fn blended(
    paint: &mut Paint,
    effect: Shader,
    coverage: Option<Shader>,
    opaque: bool,
) -> bool {
    let coverage = coverage.unwrap_or_else(|| shaders::color(Color::WHITE));
    let packed = PACKER.with(|compiled| {
        compiled.as_ref()?.make_shader(
            Data::new_empty(),
            &[ChildPtr::Shader(effect), ChildPtr::Shader(coverage)],
            None,
        )
    });
    let blender: Option<Blender> = OVER.with(|compiled| {
        compiled.as_ref()?.make_blender(uniforms(opaque), None)
    });
    let Some((packed, blender)) = packed.zip(blender) else {
        return false;
    };
    paint.set_shader(packed).set_blender(blender);
    true
}

/// A blender compositing an unrounded source `source-over` as the CPU's
/// float pipeline does onto an eight-bit destination -- see [`FLOAT_OVER`]
/// -- its half rounded to the even level where `ties_even`. `None` where
/// Skia refuses the effect.
pub(crate) fn float_over(ties_even: bool) -> Option<Blender> {
    let data = Data::new_copy(&i32::from(ties_even).to_ne_bytes());
    FLOAT.with(|compiled| compiled.as_ref()?.make_blender(data, None))
}

/// A draw waiting in a [`Batch`]: the device pixels it can change, the
/// effect and coverage shaders placed on device pixels, and whether the
/// CPU draws it as `src`.
struct Pending {
    rect: IRect,
    effect: Shader,
    coverage: Option<Shader>,
    opaque: bool,
}

/// Whether `a` and `b` share a pixel, answered as `SkIRect::intersect`
/// (`SkRect.cpp`) answers it: the rectangle between them, worked in 64 bits,
/// has a width and a height above zero, and each fits an `i32` --
/// `SkIRect::isEmpty`. Written out rather than asked of Skia, whose answer
/// crosses the bindings for each pair: asking was most of what holding
/// 2,000 small draws cost (#844).
fn share_a_pixel(a: &IRect, b: &IRect) -> bool {
    #[cfg(test)]
    COMPARED.set(COMPARED.get() + 1);
    let width = i64::from(a.right.min(b.right)) - i64::from(a.left.max(b.left));
    let height =
        i64::from(a.bottom.min(b.bottom)) - i64::from(a.top.max(b.top));
    width > 0 && height > 0 && i32::try_from(width | height).is_ok()
}

/// The cells of side [`HELD_CELL_PX`] a rectangle's pixels lie in, from the
/// first to the last on each axis.
#[derive(Clone, Copy)]
struct Cells {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

impl Cells {
    /// The cells `rect`'s pixels lie in, `None` where it has none.
    fn of(rect: &IRect) -> Option<Self> {
        let cell = |at: i32| at.div_euclid(HELD_CELL_PX);
        (rect.left < rect.right && rect.top < rect.bottom).then(|| Self {
            left: cell(rect.left),
            top: cell(rect.top),
            right: cell(rect.right - 1),
            bottom: cell(rect.bottom - 1),
        })
    }

    /// How many cells these are.
    fn count(self) -> i64 {
        (i64::from(self.right) - i64::from(self.left) + 1)
            * (i64::from(self.bottom) - i64::from(self.top) + 1)
    }

    /// Each of the cells, row by row.
    fn each(self) -> impl Iterator<Item = (i32, i32)> {
        (self.top..=self.bottom)
            .flat_map(move |y| (self.left..=self.right).map(move |x| (x, y)))
            .inspect(|_| {
                #[cfg(test)]
                CELLS_TOUCHED.set(CELLS_TOUCHED.get() + 1);
            })
    }
}

/// The rectangles of a [`Batch`]'s held draws, filed by the cells of side
/// [`HELD_CELL_PX`] their pixels lie in, so that asking whether a draw
/// meets one reads the draws near it rather than every one held. It answers
/// as [`share_a_pixel`] asked of each held rectangle would: two rectangles
/// that share a pixel share that pixel's cell, so the cells hold every one
/// a scan would find, and each is then asked.
///
/// A draw spanning more than [`HELD_CELLS_MOST`] cells is kept apart and
/// read by every check, and a check spanning more reads every held draw:
/// no check and no filing reads more cells than that, and no check makes
/// more comparisons than a scan.
#[derive(Default)]
struct Held {
    /// Each held draw's rectangle, in the order held.
    rects: Vec<IRect>,
    /// By cell, the held draws whose pixels lie in it, as indices into
    /// `rects`.
    cells: HashMap<(i32, i32), Vec<usize>, CacheHasher>,
    /// The held draws spanning more than [`HELD_CELLS_MOST`] cells.
    wide: Vec<usize>,
    /// For each held draw, the check that last compared it, so that one
    /// filed in many cells is compared once a check.
    compared: Vec<u32>,
    /// The check being made, counting from 1: 0 in `compared` is never.
    check: u32,
}

impl Held {
    /// Whether `rect` shares a pixel with a held draw.
    fn meets(&mut self, rect: &IRect) -> bool {
        let Some(cells) = Cells::of(rect) else {
            return false;
        };
        if cells.count() > HELD_CELLS_MOST {
            return self.rects.iter().any(|held| share_a_pixel(held, rect));
        }
        self.check = self.check.checked_add(1).unwrap_or_else(|| {
            self.compared.fill(0);
            1
        });
        let Self {
            rects,
            cells: filed,
            wide,
            compared,
            check,
        } = self;
        cells
            .each()
            .filter_map(|cell| filed.get(&cell))
            .flatten()
            .chain(wide.iter())
            .any(|&at| {
                if compared[at] == *check {
                    return false;
                }
                compared[at] = *check;
                share_a_pixel(&rects[at], rect)
            })
    }

    /// Holds `rect`.
    fn insert(&mut self, rect: IRect) {
        let at = self.rects.len();
        self.rects.push(rect);
        self.compared.push(0);
        match Cells::of(&rect) {
            None => {}
            Some(cells) if cells.count() > HELD_CELLS_MOST => {
                self.wide.push(at)
            }
            Some(cells) => cells
                .each()
                .for_each(|cell| self.cells.entry(cell).or_default().push(at)),
        }
    }

    /// Holds nothing.
    fn clear(&mut self) {
        self.rects.clear();
        self.cells.clear();
        self.wide.clear();
        self.compared.clear();
    }
}

/// Draws that read the page's pixels, held until one would read what
/// another writes: then one snapshot of all their rectangles is taken on
/// the GPU and each is drawn over it with `src`.
///
/// The snapshot is what keeps the route cheap. Through the blender alone,
/// which Metal runs by copying the destination for each draw, the route
/// cost 3.5 times Ganesh's own composite over 200 conic triangles; with one
/// snapshot for the run it costs 1.06 to 1.10 times for 60 to 200 draws,
/// and 1.05 times for 2,000 small ones, each still a draw of its own.
///
/// Held only on the canvas's own surface, never inside a layer -- see
/// `layer::drawn_with_effect` -- and flushed before anything else is drawn
/// over what it holds, and before the canvas's clip changes. Whether a draw
/// meets one held is asked of [`Held`], which reads the held draws near it:
/// asked of each held draw in turn, holding 2,000 that never meet compared
/// some two million pairs a frame.
#[derive(Default)]
pub(crate) struct Batch {
    pending: Vec<Pending>,
    held: Held,
}

impl Batch {
    /// Holds a draw changing no pixel outside `rect`, drawing the held ones
    /// first where it meets one of them.
    pub fn hold(
        &mut self,
        canvas: &SkCanvas,
        rect: IRect,
        effect: Shader,
        coverage: Option<Shader>,
        opaque: bool,
    ) {
        self.flush_meeting(canvas, rect);
        self.held.insert(rect);
        self.pending.push(Pending {
            rect,
            effect,
            coverage,
            opaque,
        });
    }

    /// Draws the held draws first where one of them meets `rect`, which
    /// another draw is about to write.
    pub fn flush_meeting(&mut self, canvas: &SkCanvas, rect: IRect) {
        if self.held.meets(&rect) {
            self.flush(canvas);
        }
    }

    /// Draws every held draw over one snapshot of what they cover, or each
    /// through the blender where the canvas has no surface to snapshot.
    pub fn flush(&mut self, canvas: &SkCanvas) {
        if self.pending.is_empty() {
            return;
        }
        let pending = std::mem::take(&mut self.pending);
        self.held.clear();
        let destination = snapshot(canvas, &pending);
        for p in pending {
            let paint = destination
                .as_ref()
                .and_then(|destination| p.snapped(destination))
                .unwrap_or_else(|| p.unsnapped());
            draw_on_device(canvas, p.rect, &paint);
        }
    }
}

impl Pending {
    /// The paint drawing this over `destination`, a snapshot of what it
    /// covers, with `src`: `None` where Skia refuses [`SNAPPED`].
    fn snapped(&self, destination: &Shader) -> Option<Paint> {
        let coverage = self
            .coverage
            .clone()
            .unwrap_or_else(|| shaders::color(Color::WHITE));
        let shader = SNAPPER.with(|compiled| {
            compiled.as_ref()?.make_shader(
                uniforms(self.opaque),
                &[
                    ChildPtr::Shader(self.effect.clone()),
                    ChildPtr::Shader(coverage),
                    ChildPtr::Shader(destination.clone()),
                ],
                None,
            )
        })?;
        let mut paint = Paint::default();
        paint.set_shader(shader).set_blend_mode(BlendMode::Src);
        Some(paint)
    }

    /// The paint drawing this over the canvas's own pixels: through
    /// [`blended`]'s blender, or Ganesh's own `source-over` where Skia
    /// refuses that too.
    fn unsnapped(&self) -> Paint {
        let mut paint = Paint::default();
        let effect = self.effect.clone();
        if !blended(
            &mut paint,
            effect.clone(),
            self.coverage.clone(),
            self.opaque,
        ) {
            paint.set_shader(match self.coverage.clone() {
                Some(coverage) => {
                    shaders::blend(BlendMode::DstIn, effect, coverage)
                }
                None => effect,
            });
        }
        paint
    }
}

/// A shader reading the pixels under `pending`'s rectangles from one GPU
/// snapshot of the canvas's surface, placed on device pixels, or `None`
/// where the canvas draws into no surface or Skia refuses the snapshot.
fn snapshot(canvas: &SkCanvas, pending: &[Pending]) -> Option<Shader> {
    let union = pending.iter().map(|p| p.rect).reduce(|all, rect| {
        IRect::new(
            all.left.min(rect.left),
            all.top.min(rect.top),
            all.right.max(rect.right),
            all.bottom.max(rect.bottom),
        )
    })?;
    // SAFETY: the handle takes a reference of its own to the surface
    // (`Surface::from_unshared_ptr` refs it) and is dropped before this
    // returns, while `canvas`, which the surface owns, is still borrowed.
    let mut surface = unsafe { canvas.surface() }?;
    let page = IRect::from_wh(surface.width(), surface.height());
    let area = IRect::intersect(&union, &page)?;
    let snapshot = surface.image_snapshot_with_bounds(area)?;
    #[cfg(test)]
    SNAPSHOTS.set(SNAPSHOTS.get() + 1);
    snapshot.to_shader(
        (TileMode::Clamp, TileMode::Clamp),
        SamplingOptions::new(FilterMode::Nearest, MipmapMode::None),
        Some(&Matrix::translate((area.left as f32, area.top as f32))),
    )
}

/// Fills `rect`, in device pixels, with `paint` at the canvas's identity.
fn draw_on_device(canvas: &SkCanvas, rect: IRect, paint: &Paint) {
    canvas.save();
    canvas.reset_matrix();
    canvas.draw_irect(rect, paint);
    canvas.restore();
}

#[cfg(test)]
mod tests {
    use skia_safe::IRect;

    use super::{
        CELLS_TOUCHED, COMPARED, HELD_CELL_PX, HELD_CELLS_MOST, Held,
        share_a_pixel,
    };

    /// Coordinates at the ends of an `i32` and around zero, where a
    /// difference overflows or a rectangle stops having pixels.
    const EDGES: [i32; 9] = [
        i32::MIN,
        i32::MIN + 1,
        -2,
        -1,
        0,
        1,
        2,
        i32::MAX - 1,
        i32::MAX,
    ];

    /// A small generator, so that a sequence of rectangles is the same on
    /// every run.
    struct Lcg(u64);

    impl Lcg {
        /// A number in `0..below`.
        fn below(&mut self, below: i32) -> i32 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            ((self.0 >> 33) % below as u64) as i32
        }
    }

    /// What holding a run of draws cost.
    #[derive(Debug)]
    struct Cost {
        /// The draws held.
        draws: usize,
        /// The pairs of rectangles compared.
        compared: usize,
        /// The cells filed in or read.
        cells: usize,
    }

    /// Holds each of `rects` in turn as a batch does, asking first whether it
    /// meets one held and holding nothing more once one does, checks the
    /// answer against Skia's asked of every held rectangle, and checks that no
    /// draw touched more than [`HELD_CELLS_MOST`] cells to be asked about and
    /// filed. What the answers cost.
    fn hold_all(rects: impl IntoIterator<Item = IRect>) -> Cost {
        let mut held = Held::default();
        hold_all_into(&mut held, rects)
    }

    /// [`hold_all`], into `held`.
    fn hold_all_into(
        held: &mut Held,
        rects: impl IntoIterator<Item = IRect>,
    ) -> Cost {
        let mut scanned: Vec<IRect> = held.rects.clone();
        let mut cost = Cost {
            draws: 0,
            compared: 0,
            cells: 0,
        };
        for (at, rect) in rects.into_iter().enumerate() {
            let (compared, cells) = (COMPARED.get(), CELLS_TOUCHED.get());
            let meets = held.meets(&rect);
            cost.compared += COMPARED.get() - compared;
            let expected = scanned.iter().any(|r| IRect::intersects(r, &rect));
            assert_eq!(meets, expected, "draw {at}, {rect:?}");
            if meets {
                held.clear();
                scanned.clear();
            }
            held.insert(rect);
            scanned.push(rect);
            let touched = CELLS_TOUCHED.get() - cells;
            assert!(
                touched as i64 <= 2 * HELD_CELLS_MOST,
                "draw {at}, {rect:?}: {touched} cells"
            );
            cost.cells += touched;
            cost.draws += 1;
        }
        cost
    }

    /// Rectangles a cell apart, a tenth of a cell apart, overlapping,
    /// touching along an edge or a corner, empty, inverted, negative, and
    /// spanning more than an `i32` can count, each answered as Skia answers.
    #[test]
    fn sharing_a_pixel_is_answered_as_skia_answers_it() {
        let small: Vec<IRect> = (-2..=2)
            .flat_map(|l| (-2..=2).map(move |t| (l, t)))
            .flat_map(|(l, t)| {
                (-2..=2).flat_map(move |r| (-2..=2).map(move |b| (l, t, r, b)))
            })
            .map(|(l, t, r, b)| IRect::new(l, t, r, b))
            .collect();
        let edges: Vec<IRect> = EDGES
            .iter()
            .flat_map(|&l| EDGES.iter().map(move |&r| (l, r)))
            .flat_map(|(l, r)| {
                EDGES
                    .iter()
                    .map(move |&t| IRect::new(l, t, r, t.saturating_add(3)))
            })
            .collect();
        let named = [
            (IRect::new(0, 0, 10, 10), IRect::new(10, 0, 20, 10), false),
            (IRect::new(0, 0, 10, 10), IRect::new(10, 10, 20, 20), false),
            (IRect::new(0, 0, 10, 10), IRect::new(9, 9, 20, 20), true),
            (
                IRect::new(-20, -20, -10, -10),
                IRect::new(-11, -11, 0, 0),
                true,
            ),
            (IRect::new(0, 0, 0, 10), IRect::new(-5, -5, 5, 5), false),
            (IRect::new(5, 5, 0, 0), IRect::new(-10, -10, 10, 10), false),
            (
                IRect::new(i32::MIN, 0, i32::MAX, 10),
                IRect::new(0, 0, 10, 10),
                true,
            ),
            (
                IRect::new(i32::MIN, 0, i32::MAX, 10),
                IRect::new(i32::MIN, 0, i32::MAX, 10),
                false,
            ),
        ];
        for (a, b, shared) in named {
            assert_eq!(IRect::intersects(&a, &b), shared, "Skia, {a:?} {b:?}");
            assert_eq!(share_a_pixel(&a, &b), shared, "{a:?} {b:?}");
        }
        for (a, b) in small
            .iter()
            .flat_map(|a| small.iter().map(move |b| (a, b)))
            .chain(edges.iter().flat_map(|a| edges.iter().map(move |b| (a, b))))
        {
            assert_eq!(
                share_a_pixel(a, b),
                IRect::intersects(a, b),
                "{a:?} {b:?}"
            );
        }
    }

    /// The cells find every meeting a scan of the held draws finds, and no
    /// other: draws of every size up to a page and past an `i32`'s ends,
    /// empty ones, ones straddling the cells' edges and ones on them, at
    /// negative coordinates and off a 600-pixel page.
    #[test]
    fn the_cells_find_every_meeting_a_scan_finds() {
        let mut next = Lcg(0x845);
        let rects = (0..40_000).map(|_| match next.below(20) {
            0 => IRect::new(0, 0, 600, 600),
            1 => IRect::new(i32::MIN, next.below(600), i32::MAX, 700),
            2 => {
                let y = next.below(700) - 50;
                IRect::new(-40, y, 640, y + 1)
            }
            3 => {
                let x = next.below(20) * HELD_CELL_PX;
                IRect::new(x, -40, x + HELD_CELL_PX, 640)
            }
            4 => IRect::new(next.below(700), next.below(700), 3, 3),
            _ => {
                let (x, y) = (next.below(800) - 100, next.below(800) - 100);
                IRect::new(x, y, x + next.below(40), y + next.below(40))
            }
        });
        hold_all(rects);
    }

    /// Holding 2,000 small draws that never meet, laid out as #844's scene
    /// lays them, compares each with the few near it: at most the 36 whose
    /// 12-pixel squares lie in the two cells either way it can span, where
    /// asking every held draw compared 1,999,000 pairs.
    #[test]
    fn small_draws_are_compared_with_those_near_them() {
        let draws: usize = 2_000;
        let rects = (0..draws as i32).map(|i| {
            let (x, y) = ((i % 50) * 12 + 3, (i / 50) * 12 + 3);
            IRect::new(x, y, x + 11, y + 10)
        });
        let cost = hold_all(rects);
        assert!(cost.compared <= 36 * draws, "{cost:?}");
    }

    /// A page-sized draw held, then 2,000 small ones: the first meets it and
    /// the batch starts again, and none of the others is compared with the
    /// page-sized one, gone with it.
    #[test]
    fn a_page_sized_draw_costs_one_comparison() {
        let draws: usize = 2_000;
        let page = std::iter::once(IRect::new(0, 0, 600, 600));
        let small = (0..draws as i32).map(|i| {
            let (x, y) = ((i % 50) * 12 + 3, (i / 50) * 12 + 3);
            IRect::new(x, y, x + 11, y + 10)
        });
        let cost = hold_all(page.chain(small));
        assert!(cost.compared <= 36 * draws + 1, "{cost:?}");
    }

    /// Draws spanning many cells: 1,000 strips 300 pixels by 9 a row apart,
    /// each in up to 20 cells, are compared each with the strips of the two
    /// cell rows it spans, at most 14, once each however many cells they
    /// share, where asking every held draw compared 499,500 pairs.
    #[test]
    fn draws_spanning_many_cells_are_compared_once_each() {
        let draws: usize = 1_000;
        let rects = (0..draws as i32).map(|i| {
            let (x, y) = ((i % 2) * 300, (i / 2) * 10);
            IRect::new(x, y, x + 300, y + 9)
        });
        let cost = hold_all(rects);
        assert!(cost.compared <= 14 * draws, "{cost:?}");
    }

    /// Checks keep their answers across the count of checks wrapping: the
    /// second check, the first after the wrap, meets the draw held before it,
    /// which is compared again rather than taken as compared by a check whose
    /// number came round.
    #[test]
    fn checks_answer_alike_across_their_count_wrapping() {
        let mut held = Held {
            check: u32::MAX - 1,
            ..Held::default()
        };
        let rects = (0..8).flat_map(|i| {
            let x = i * HELD_CELL_PX;
            [
                IRect::new(x, 0, x + 10, 10),
                IRect::new(x + 5, 5, x + 15, 15),
            ]
        });
        let cost = hold_all_into(&mut held, rects);
        assert!(
            held.check < cost.draws as u32,
            "not wrapped: {}",
            held.check
        );
    }
}
