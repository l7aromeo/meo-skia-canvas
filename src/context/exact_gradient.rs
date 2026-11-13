//! A gradient drawn on the GPU in the bytes the CPU stores for it.
//!
//! Ganesh draws a gradient through a layout that computes `t` and a
//! colorizer that finds `t`'s interval, and both lose precision the CPU's
//! raster pipeline keeps: `SweepLayout` computes the angle and `t` in
//! `half`, a colorizer for more than 128 colours keeps its thresholds in an
//! F16 texture (`make_buffered_colorizer`), and a radial or translucent
//! linear ramp lands up to four levels from the CPU's. A hard stop then
//! moves by up to a tenth of a degree or half a pixel, and a pixel beside it
//! takes the colour on the other side.
//!
//! [`GradientSpec::exact`] builds the shader the GPU draws instead: one
//! runtime effect doing in `float` what the CPU's pipeline does, from the
//! data Skia's gradient constructor and colour transform hand that
//! pipeline. `t` is the CPU's -- the sweep's angle is Skia's own seventh
//! degree polynomial, `xy_to_unit_angle` -- the interval is found with
//! `t >= stop`, as the `gradient` stage counts, and each channel is rounded
//! where the CPU rounds it: on the eight-bit pipeline (`lowp`) as
//! `trunc(x * 255 + 0.5)`, premultiplied with `div255_accurate` and scaled
//! by the paint's alpha with `div255`. The effect returns whole levels,
//! which the half-precision fragment output carries to an eight-bit target
//! unchanged.
//!
//! The effect computes them from the coordinates Ganesh hands it, which are
//! each pixel centre's only for an aliased rectangle on whole pixels; so a
//! draw fills its shape with an image of the effect over the pixels it
//! reaches, sampled nearest -- see [`GradientSpec::sampled`].
//!
//! What is left is float noise: Skia composes the gradient's matrix with the
//! device's into one before applying it, and the effect applies the two in
//! turn, so `t` can part from the CPU's in its last bits, and a pixel whose
//! value lies within that of a half level can round the other way.
//!
//! [`GradientSpec::exact`] answers `None` where the effect cannot reproduce
//! the CPU: an interpolation space whose conversion to the page runs through
//! skcms's approximate transfer functions, a two-point conical gradient
//! whose stops are translucent or that is drawn at an alpha (the CPU's
//! `highp` pipeline rounds once, after blending), more stops than the
//! largest effect holds, and Skia's degenerate shapes.

#[cfg(test)]
use std::cell::Cell;
use std::cell::RefCell;

use skia_safe::{
    AlphaType, Color4f, ColorSpace, ColorType, Data, FilterMode, IRect,
    ImageInfo, Matrix, MipmapMode, Paint, PictureRecorder, Pixmap, Point, Rect,
    RuntimeEffect, SamplingOptions, Shader, TileMode,
    gradient::{
        Colors as GradientColors, Gradient as SkGradient, Interpolation,
        interpolation::{ColorSpace as InterpolationSpace, InPremul},
        shaders as gradient_shaders,
    },
    images,
};

use crate::{
    cache_map::{GOLDEN_RATIO_64, KeptSets},
    pixels::PixelColorSpace,
    shader::GradientColorSpace,
    started,
};

/// What a gradient is laid out on.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Geometry {
    /// From `start` to `end`.
    Linear { start: Point, end: Point },
    /// Around `center`, out to `radius` -- Skia's radial gradient.
    Radial { center: Point, radius: f32 },
    /// From the circle `(start, start_radius)` to `(end, end_radius)` --
    /// Skia's two-point conical gradient, which the Canvas API's
    /// `createRadialGradient` makes.
    Conical {
        start: Point,
        start_radius: f32,
        end: Point,
        end_radius: f32,
    },
    /// Around `center`, from `start` to `end` degrees clockwise from +x.
    Sweep { center: Point, start: f32, end: f32 },
}

/// Everything a gradient is made from, kept so that the shader the CPU draws
/// and the one the GPU draws are made from one description.
#[derive(Clone, Debug)]
pub(crate) struct GradientSpec {
    pub geometry: Geometry,
    /// The shader's own matrix: a conic gradient's start angle.
    pub local: Option<Matrix>,
    /// Unpremultiplied, in `color_space`.
    pub colors: Vec<Color4f>,
    /// One per colour, as the caller gave them.
    pub positions: Vec<f32>,
    /// The space `colors` are in.
    pub color_space: ColorSpace,
    /// The interpolation space as this crate names it, which decides the
    /// working space wrapped around the shader -- see
    /// [`GradientColorSpace::in_working_space`].
    pub space: GradientColorSpace,
    pub interpolation: Interpolation,
}

impl GradientSpec {
    /// The shader Skia draws this with: the CPU's, and a document's.
    pub fn native(&self) -> Option<Shader> {
        let stops = GradientColors::new(
            &self.colors,
            Some(self.positions.as_slice()),
            TileMode::Clamp,
            Some(self.color_space.clone()),
        );
        let gradient = SkGradient::new(stops, self.interpolation);
        let local = self.local.as_ref();
        let shader = match self.geometry {
            Geometry::Linear { start, end } => {
                gradient_shaders::linear_gradient(
                    (start, end),
                    &gradient,
                    local,
                )
            }
            Geometry::Radial { center, radius } => {
                gradient_shaders::radial_gradient(
                    (center, radius),
                    &gradient,
                    local,
                )
            }
            Geometry::Conical {
                start,
                start_radius,
                end,
                end_radius,
            } => gradient_shaders::two_point_conical_gradient(
                (start, start_radius),
                (end, end_radius),
                &gradient,
                local,
            ),
            Geometry::Sweep { center, start, end } => {
                gradient_shaders::sweep_gradient(
                    center,
                    (start, end),
                    &gradient,
                    local,
                )
            }
        }?;
        self.space.in_working_space(shader).ok()
    }

    /// The shader a GPU canvas draws this with, under the transform
    /// `matrix`, in the bytes the CPU stores, over `area`, the device pixels
    /// the draw reaches: an image of [`Self::exact`] over `area`, sampled
    /// at each pixel's centre. `None` where the effect cannot reproduce the
    /// CPU, or `matrix` cannot be inverted.
    ///
    /// The effect computes the CPU's bytes from the local coordinates
    /// Ganesh hands it, which it derives exactly for an aliased rectangle
    /// on whole pixels and only nearly elsewhere: through a path renderer,
    /// or an anti-aliased rectangle whose edges fall inside pixels, it
    /// extrapolates them from geometry outset past the shape, and a
    /// page-covering triangle drew 0.5% of its pixels a level off, a
    /// rectangle 0.37 px past each edge of the page 16% -- see
    /// `exact_gradient::tests`. So the effect fills `area` as such a
    /// rectangle, in an image Ganesh renders when it is first drawn -- an
    /// image backed by the picture of it -- and the draw samples it at each
    /// pixel's centre, nearest, which a coordinate under half a pixel off
    /// still finds. The draw's own geometry gives its coverage.
    ///
    /// The image's texels are the device pixels `matrix` maps onto, so it
    /// holds only where the draw is played onto those pixels or a whole
    /// pixel's translation of them -- see `layer::OnGpu::Effect`.
    pub fn sampled(
        &self,
        page: &ColorSpace,
        alpha: f32,
        matrix: &Matrix,
        area: IRect,
    ) -> Option<Shader> {
        let to_area = Matrix::translate((-area.left as f32, -area.top as f32));
        let from_device = matrix.invert()?;
        let effect = self
            .exact(page, alpha)?
            .with_local_matrix(&Matrix::concat(&to_area, matrix));
        let size = Rect::from_wh(area.width() as f32, area.height() as f32);
        let mut recorder = PictureRecorder::new();
        let mut paint = Paint::default();
        paint.set_shader(effect);
        recorder
            .begin_recording(size, false)
            .draw_rect(size, &paint);
        let image = started::deferred_from_picture(
            recorder.finish_recording_as_picture(None)?,
            area.size(),
            None,
            None,
            images::BitDepth::U8,
            Some(page.clone()),
            None,
        )?;
        image.to_shader(
            (TileMode::Decal, TileMode::Decal),
            SamplingOptions::new(FilterMode::Nearest, MipmapMode::None),
            Some(&Matrix::concat(
                &from_device,
                &Matrix::translate((area.left as f32, area.top as f32)),
            )),
        )
    }

    /// The shader that draws this on the GPU in the bytes the CPU stores,
    /// on an eight-bit page in `page`, at `alpha` -- the paint's, which the
    /// shader takes and the paint drawing it then leaves at 1. `None` where
    /// the effect cannot reproduce the CPU -- see the module's notes. Exact
    /// only where Ganesh draws it as an aliased rectangle on whole pixels --
    /// see [`Self::sampled`].
    ///
    /// The answer is the one this thread last made for a gradient whose
    /// every number is the same to the bit, on the same page at the same
    /// alpha, where [`EXACTS`] still keeps it: a page redrawn every frame
    /// makes each of its gradients afresh, as a Canvas API caller makes a
    /// new `CanvasGradient` for every draw, and making one reads its stops
    /// through a colour conversion and uploads its uniforms.
    pub fn exact(&self, page: &ColorSpace, alpha: f32) -> Option<Shader> {
        let alpha = alpha.to_bits();
        let hash = exact_hash(self, page, alpha);
        let is_key = |key: &ExactKey| key.is_for(self, page, alpha);
        #[cfg(test)]
        let keeps = KEEPS_EXACTS.get();
        #[cfg(not(test))]
        let keeps = true;
        if keeps
            && let Some(kept) =
                EXACTS.with_borrow(|kept| kept.find(hash, is_key).cloned())
        {
            return kept;
        }
        let made = self.made(page, f32::from_bits(alpha));
        let key = ExactKey {
            spec: self.clone(),
            page: page.clone(),
            alpha,
        };
        EXACTS.with_borrow_mut(|kept| kept.file(hash, key, made.clone()));
        made
    }

    /// [`Self::exact`], made afresh.
    fn made(&self, page: &ColorSpace, alpha: f32) -> Option<Shader> {
        #[cfg(test)]
        EXACTS_MADE.set(EXACTS_MADE.get() + 1);
        let layout = Layout::of(self)?;
        let ramp = Ramp::of(self, page)?;
        // The `highp` pipeline premultiplies, scales and blends in floats
        // and rounds once, at the store; the effect hands the GPU whole
        // levels, which match it only where none of those happen.
        if layout.highp && (!ramp.opaque || alpha < 1.0) {
            return None;
        }
        let local = match &self.local {
            Some(local) => Matrix::concat(local, &layout.from_unit),
            None => layout.from_unit,
        };
        EFFECTS.with(|effects| {
            let (size, effect, offsets) = effects
                .as_ref()?
                .iter()
                .find(|(size, ..)| ramp.ts.len() <= *size)?;
            effect.make_shader(
                uniforms(offsets, *size, &layout, &ramp, alpha),
                &[],
                Some(&local),
            )
        })
    }

    /// Every number this is made from but its colour space, as bits: the
    /// kind of geometry, whether there is a local matrix, the interpolation
    /// and the two lists' lengths first, so that no two descriptions give
    /// one sequence, then the geometry, the matrix, the colours and the
    /// positions.
    fn words(&self) -> impl Iterator<Item = u64> + '_ {
        let (kind, geometry) = match self.geometry {
            Geometry::Linear { start, end } => {
                (0, [start.x, start.y, end.x, end.y, 0.0, 0.0])
            }
            Geometry::Radial { center, radius } => {
                (1, [center.x, center.y, radius, 0.0, 0.0, 0.0])
            }
            Geometry::Conical {
                start,
                start_radius,
                end,
                end_radius,
            } => (
                2,
                [start.x, start.y, start_radius, end.x, end.y, end_radius],
            ),
            Geometry::Sweep { center, start, end } => {
                (3, [center.x, center.y, start, end, 0.0, 0.0])
            }
        };
        let local = self.local.map(|local| {
            let mut nine = [0.0; 9];
            local.get_9(&mut nine);
            nine
        });
        let Interpolation {
            in_premul,
            color_space,
            hue_method,
        } = self.interpolation;
        let numbers = geometry
            .into_iter()
            .chain(local.into_iter().flatten())
            .chain(self.colors.iter().flat_map(|c| [c.r, c.g, c.b, c.a]))
            .chain(self.positions.iter().copied())
            .map(|number| u64::from(number.to_bits()));
        [
            kind,
            u64::from(local.is_some()),
            self.space as u64,
            in_premul as u64,
            color_space as u64,
            hue_method as u64,
            self.colors.len() as u64,
            self.positions.len() as u64,
        ]
        .into_iter()
        .chain(numbers)
    }
}

/// How many sets of shaders [`GradientSpec::exact`] keeps on a thread, and
/// how many a set holds: 256 shaders, chosen for a page of a few dozen
/// distinct gradients redrawn every frame -- 50 in the scene it was sized
/// on, which fall into 64 sets of 4 with almost none pushed out. A shader
/// holds its uniforms and its key the gradient's stops: about half a KiB
/// for a gradient of a few stops, so about 128 KiB a thread once full, and
/// about 14 KiB for one of 256 stops, the most the effect takes, 3.5 MiB a
/// thread were every shader kept one. Each thread keeps its own, so a page
/// played back on a rayon worker, as an export plays its pages, fills that
/// worker's. A shader filed over another is made again when it is next
/// asked for, which costs what making it always did.
const EXACT_SETS: usize = 64;
const EXACT_WAYS: usize = 4;

/// What [`GradientSpec::exact`] made a shader from: the gradient, the
/// page's space and the paint's alpha, as bits.
struct ExactKey {
    spec: GradientSpec,
    page: ColorSpace,
    alpha: u32,
}

impl ExactKey {
    /// Whether this is the key of `spec` on `page` at the alpha whose bits
    /// are `alpha`: every number the same to the bit, and every colour
    /// space equal.
    fn is_for(
        &self,
        spec: &GradientSpec,
        page: &ColorSpace,
        alpha: u32,
    ) -> bool {
        self.alpha == alpha
            && self.spec.words().eq(spec.words())
            && self.spec.color_space == spec.color_space
            && self.page == *page
    }
}

/// Where [`EXACTS`] files the key of `spec` on `page` at the alpha whose
/// bits are `alpha`: its words and the two spaces' hashes, each folded in
/// and multiplied by [`GOLDEN_RATIO_64`].
fn exact_hash(spec: &GradientSpec, page: &ColorSpace, alpha: u32) -> u64 {
    spec.words()
        .chain([spec.color_space.hash(), page.hash(), u64::from(alpha)])
        .fold(0, |hash, word| (hash ^ word).wrapping_mul(GOLDEN_RATIO_64))
}

/// The shaders [`GradientSpec::exact`] has made, each `None` where it could
/// make none, filed by [`exact_hash`].
type Exacts = KeptSets<ExactKey, Option<Shader>, EXACT_SETS, EXACT_WAYS>;

thread_local! {
    /// The shaders [`GradientSpec::exact`] has made on this thread.
    static EXACTS: RefCell<Exacts> = const { RefCell::new(KeptSets::new()) };
}

#[cfg(test)]
thread_local! {
    /// How many shaders [`GradientSpec::exact`] has made on this thread
    /// rather than found kept.
    pub(crate) static EXACTS_MADE: Cell<usize> = const { Cell::new(0) };
    /// Whether [`GradientSpec::exact`] takes a shader kept: off, every
    /// shader is made afresh, the reference the kept ones are held to.
    pub(crate) static KEEPS_EXACTS: Cell<bool> = const { Cell::new(true) };
}

/// How `t` is computed, in the order the CPU's stages compute it.
#[derive(Clone, Debug, PartialEq)]
struct Layout {
    kind: Kind,
    /// The inverse of the gradient's own matrix, which maps the space `kind`
    /// computes `t` in to the shader's.
    from_unit: Matrix,
    /// A matrix applied to `t` after `kind` -- a partial sweep's range, a
    /// concentric conical gradient's start radius -- as `t * scale + bias`.
    t_scale: f32,
    t_bias: f32,
    /// The two-point conical stages' constants -- see `Conical2PtCtx`.
    p0: f32,
    p1: f32,
    /// Which of the conical gradient's stages follow `kind` -- see
    /// [`Layout::MASK_NAN`] and its siblings.
    flags: i32,
    /// Whether the CPU draws this through its float pipeline.
    highp: bool,
}

/// What computes `t` from the point in the gradient's space.
#[derive(Clone, Copy, Debug, PartialEq)]
#[repr(i32)]
enum Kind {
    /// `t = x`.
    Linear = 0,
    /// `xy_to_radius`.
    Radius = 1,
    /// `xy_to_unit_angle`.
    Angle = 2,
    /// `xy_to_2pt_conical_strip`.
    Strip = 3,
    /// `xy_to_2pt_conical_focal_on_circle`.
    FocalOnCircle = 4,
    /// `xy_to_2pt_conical_well_behaved`.
    WellBehaved = 5,
    /// `xy_to_2pt_conical_greater`.
    Greater = 6,
    /// `xy_to_2pt_conical_smaller`.
    Smaller = 7,
}

impl Layout {
    /// `alter_2pt_conical_compensate_focal`.
    const COMPENSATE: i32 = 8;
    /// `mask_2pt_conical_degenerates`.
    const MASK_DEGENERATES: i32 = 2;
    /// `mask_2pt_conical_nan`.
    const MASK_NAN: i32 = 1;
    /// `negate_x`.
    const NEGATE: i32 = 4;
    /// `alter_2pt_conical_unswap`.
    const UNSWAP: i32 = 16;

    /// Whether `flag` is set, as the effect's uniform takes it.
    fn flag(&self, flag: i32) -> [u8; 4] {
        i32::from(self.flags & flag != 0).to_ne_bytes()
    }

    fn plain(kind: Kind, to_unit: Matrix) -> Option<Layout> {
        Some(Layout {
            kind,
            from_unit: to_unit.invert()?,
            t_scale: 1.0,
            t_bias: 0.0,
            p0: 0.0,
            p1: 0.0,
            flags: 0,
            highp: false,
        })
    }

    /// The layout Skia's factories choose for `spec`, `None` for the shapes
    /// they make something other than a gradient of: a degenerate one.
    fn of(spec: &GradientSpec) -> Option<Layout> {
        match spec.geometry {
            Geometry::Linear { start, end } => {
                let length = (end - start).length();
                if !length.is_finite()
                    || nearly_zero(length, DEGENERATE_THRESHOLD)
                {
                    return None;
                }
                Layout::plain(Kind::Linear, linear_to_unit(start, end))
            }
            Geometry::Radial { center, radius } => radial(center, radius),
            Geometry::Conical {
                start,
                start_radius,
                end,
                end_radius,
            } => conical(start, start_radius, end, end_radius),
            Geometry::Sweep { center, start, end } => {
                if !start.is_finite()
                    || !end.is_finite()
                    || start > end
                    || nearly_equal(start, end, DEGENERATE_THRESHOLD)
                {
                    return None;
                }
                // `SkSweepGradient`: `t` mapped from `[t0, t1]` by
                // `Scale(1 / (t1 - t0)) * Translate(-t0)`, which
                // `appendMatrix` leaves out where it is the identity.
                let (t0, t1) = (start / 360.0, end / 360.0);
                let scale = 1.0 / (t1 - t0);
                let mut layout = Layout::plain(
                    Kind::Angle,
                    Matrix::translate((-center.x, -center.y)),
                )?;
                layout.t_scale = scale;
                layout.t_bias = scale * -t0;
                Some(layout)
            }
        }
    }
}

/// `SkGradientBaseShader::kDegenerateThreshold`, the size under which
/// Skia's factories make a gradient something other than a gradient.
const DEGENERATE_THRESHOLD: f32 = 1.0 / (1 << 15) as f32;

/// `SK_ScalarNearlyZero`, the default tolerance of `SkScalarNearlyZero` and
/// `SkScalarNearlyEqual`.
const NEARLY_ZERO: f32 = 1.0 / (1 << 12) as f32;

fn nearly_zero(x: f32, tolerance: f32) -> bool {
    x.abs() <= tolerance
}

fn nearly_equal(a: f32, b: f32, tolerance: f32) -> bool {
    (a - b).abs() <= tolerance
}

/// Skia's `pts_to_unit_matrix`, which maps `start` to the origin and `end`
/// to `(1, 0)`.
fn linear_to_unit(start: Point, end: Point) -> Matrix {
    let mut vec = end - start;
    let mag = vec.length();
    let inv = if mag != 0.0 { 1.0 / mag } else { 0.0 };
    vec.scale(inv);
    let mut matrix = Matrix::new_identity();
    matrix.set_sin_cos((-vec.y, vec.x), start);
    matrix.post_translate((-start.x, -start.y));
    matrix.post_scale((inv, inv), None);
    matrix
}

/// `SkShaders::RadialGradient`, with its `rad_to_unit_matrix`.
fn radial(center: Point, radius: f32) -> Option<Layout> {
    if radius < 0.0 || nearly_zero(radius, DEGENERATE_THRESHOLD) {
        return None;
    }
    let inv = 1.0 / radius;
    let mut matrix = Matrix::translate((-center.x, -center.y));
    matrix.post_scale((inv, inv), None);
    Layout::plain(Kind::Radius, matrix)
}

/// `SkShaders::TwoPointConicalGradient` and `SkConicalGradient::Create`.
fn conical(start: Point, r0: f32, end: Point, r1: f32) -> Option<Layout> {
    if r0 < 0.0 || r1 < 0.0 {
        return None;
    }
    let apart = (start - end).length();
    if nearly_zero(apart, DEGENERATE_THRESHOLD) {
        if nearly_equal(r0, r1, DEGENERATE_THRESHOLD) {
            return None;
        }
        if nearly_zero(r0, DEGENERATE_THRESHOLD) {
            return radial(start, r1);
        }
    }
    if nearly_zero(apart, NEARLY_ZERO) {
        if nearly_zero(r0.max(r1), NEARLY_ZERO)
            || nearly_equal(r0, r1, NEARLY_ZERO)
        {
            return None;
        }
        // Concentric: a radius, then `Translate(bias) * Scale(scale)`.
        let scale = 1.0 / r0.max(r1);
        let mut matrix = Matrix::translate((-end.x, -end.y));
        matrix.post_scale((scale, scale), None);
        let d_radius = r1 - r0;
        let mut layout = Layout::plain(Kind::Radius, matrix)?;
        layout.t_scale = r0.max(r1) / d_radius;
        layout.t_bias = -r0 / d_radius;
        return Some(layout);
    }
    let mut matrix = Matrix::from_poly_to_poly(
        &[start, end],
        &[Point::new(0.0, 0.0), Point::new(1.0, 0.0)],
    )?;
    if nearly_zero(r1 - r0, NEARLY_ZERO) {
        let scaled = r0 / apart;
        let mut layout = Layout::plain(Kind::Strip, matrix)?;
        layout.p0 = scaled * scaled;
        layout.flags = Layout::MASK_NAN;
        layout.highp = true;
        return Some(layout);
    }
    let focal = Focal::set(r0 / apart, r1 / apart, &mut matrix)?;
    let kind = if focal.on_circle() {
        Kind::FocalOnCircle
    } else if focal.well_behaved() {
        Kind::WellBehaved
    } else if focal.swapped || 1.0 - focal.x < 0.0 {
        Kind::Smaller
    } else {
        Kind::Greater
    };
    let mut flags = 0;
    if !focal.well_behaved() {
        flags |= Layout::MASK_DEGENERATES;
    }
    if 1.0 - focal.x < 0.0 {
        flags |= Layout::NEGATE;
    }
    if !nearly_zero(focal.x, NEARLY_ZERO) {
        flags |= Layout::COMPENSATE;
    }
    if focal.swapped {
        flags |= Layout::UNSWAP;
    }
    let mut layout = Layout::plain(kind, matrix)?;
    layout.p0 = 1.0 / focal.r1;
    layout.p1 = focal.x;
    layout.flags = flags;
    layout.highp = true;
    Some(layout)
}

/// `SkConicalGradient::FocalData`.
struct Focal {
    x: f32,
    r1: f32,
    swapped: bool,
}

impl Focal {
    /// `FocalData::set`, which also moves `matrix` into the focal frame.
    fn set(r0: f32, r1: f32, matrix: &mut Matrix) -> Option<Focal> {
        let mut swapped = false;
        let mut r1 = r1;
        let mut x = r0 / (r0 - r1);
        if nearly_zero(x - 1.0, NEARLY_ZERO) {
            matrix.post_translate((-1.0, 0.0));
            matrix.post_scale((-1.0, 1.0), None);
            r1 = r0;
            x = 0.0;
            swapped = true;
        }
        let focal = Matrix::from_poly_to_poly(
            &[Point::new(x, 0.0), Point::new(1.0, 0.0)],
            &[Point::new(0.0, 0.0), Point::new(1.0, 0.0)],
        )?;
        matrix.post_concat(&focal);
        let data = Focal {
            x,
            r1: r1 / (1.0 - x).abs(),
            swapped,
        };
        if data.on_circle() {
            matrix.post_scale((0.5, 0.5), None);
        } else {
            let r1 = data.r1;
            matrix.post_scale(
                (r1 / (r1 * r1 - 1.0), 1.0 / (r1 * r1 - 1.0).abs().sqrt()),
                None,
            );
        }
        matrix.post_scale(((1.0 - x).abs(), (1.0 - x).abs()), None);
        Some(data)
    }

    fn on_circle(&self) -> bool {
        nearly_zero(1.0 - self.r1, NEARLY_ZERO)
    }

    fn well_behaved(&self) -> bool {
        !self.on_circle() && self.r1 > 1.0
    }
}

/// The intervals the CPU's gradient stage searches, built as
/// `SkGradientBaseShader` and `AppendGradientFillStages` build them.
#[derive(Clone, Debug, PartialEq)]
struct Ramp {
    /// How `t` finds its interval -- see [`Ramp::GENERAL`].
    mode: i32,
    /// Thresholds: interval `i`, for `i` from 1, holds `t >= ts[i]`.
    ts: Vec<f32>,
    /// A factor and a bias per interval, premultiplied where the
    /// interpolation is: `colour = t * factor + bias`.
    factors: Vec<[f32; 4]>,
    biases: Vec<[f32; 4]>,
    /// Whether every stop is opaque, which leaves out the premultiply.
    opaque: bool,
    /// Whether the colours are premultiplied after the interpolation.
    premul_after: bool,
}

impl Ramp {
    /// `evenly_spaced_gradient`: `t` clamped, the interval its index.
    const EVENLY: i32 = 1;
    /// The `gradient` stage: arbitrary stops, searched.
    const GENERAL: i32 = 0;
    /// `evenly_spaced_2_stop_gradient`: `t` clamped, one interval.
    const TWO: i32 = 2;

    fn of(spec: &GradientSpec, page: &ColorSpace) -> Option<Ramp> {
        let (colors, positions, opaque) = stops(spec)?;
        let intermediate = intermediate_space(spec, page)?;
        let mut colors = convert(&colors, &spec.color_space, &intermediate)?;
        let premul = spec.interpolation.in_premul == InPremul::Yes;
        if premul {
            for c in &mut colors {
                *c = [c[0] * c[3], c[1] * c[3], c[2] * c[3], c[3]];
            }
        }
        let premul_after = !premul && !opaque;
        Some(match positions {
            None if colors.len() == 2 => Ramp {
                mode: Ramp::TWO,
                ts: vec![0.0],
                factors: vec![std::array::from_fn(|k| {
                    colors[1][k] - colors[0][k]
                })],
                biases: vec![colors[0]],
                opaque,
                premul_after,
            },
            None => evenly(&colors, opaque, premul_after),
            Some(positions) => {
                general(&colors, &positions, opaque, premul_after)
            }
        })
    }
}

/// `init_stop_evenly` and `add_const_color`, for evenly spaced stops.
fn evenly(colors: &[[f32; 4]], opaque: bool, premul_after: bool) -> Ramp {
    let gaps = (colors.len() - 1) as f32;
    let mut ramp = Ramp {
        mode: Ramp::EVENLY,
        ts: Vec::with_capacity(colors.len()),
        factors: Vec::with_capacity(colors.len()),
        biases: Vec::with_capacity(colors.len()),
        opaque,
        premul_after,
    };
    for (i, pair) in colors.windows(2).enumerate() {
        let (left, right) = (pair[0], pair[1]);
        let factor: [f32; 4] =
            std::array::from_fn(|k| (right[k] - left[k]) * gaps);
        let at = i as f32 / gaps;
        ramp.biases
            .push(std::array::from_fn(|k| left[k] - factor[k] * at));
        ramp.factors.push(factor);
        ramp.ts.push(0.0);
    }
    ramp.factors.push([0.0; 4]);
    ramp.biases.push(colors[colors.len() - 1]);
    ramp.ts.push(0.0);
    ramp
}

/// `init_stop_pos` and `add_const_color`, for arbitrary stops.
///
/// `AppendGradientFillStages` leaves out a first or last stop the same colour
/// as its neighbour, the copy Skia's constructor adds to bracket the stops
/// at 0 and 1. Kept, it is an interval whose factor is zero and whose bias
/// is that colour, which draws what leaving it out draws.
fn general(
    colors: &[[f32; 4]],
    positions: &[f32],
    opaque: bool,
    premul_after: bool,
) -> Ramp {
    let mut ramp = Ramp {
        mode: Ramp::GENERAL,
        ts: vec![0.0],
        factors: vec![[0.0; 4]],
        biases: vec![colors[0]],
        opaque,
        premul_after,
    };
    let mut t_l = positions[0];
    let mut c_l = colors[0];
    for i in 0..colors.len() - 1 {
        let (t_r, c_r) = (positions[i + 1], colors[i + 1]);
        if t_l < t_r {
            let scale = 1.0 / (t_r - t_l);
            if scale.is_finite() {
                let factor: [f32; 4] =
                    std::array::from_fn(|k| (c_r[k] - c_l[k]) * scale);
                ramp.ts.push(t_l);
                ramp.biases
                    .push(std::array::from_fn(|k| c_l[k] - factor[k] * t_l));
                ramp.factors.push(factor);
            }
        }
        t_l = t_r;
        c_l = c_r;
    }
    ramp.ts.push(t_l);
    ramp.factors.push([0.0; 4]);
    ramp.biases.push(c_l);
    ramp
}

/// The stops as `SkGradientBaseShader`'s constructor leaves them: bracketed
/// by stops at 0 and 1, pinned to rise, `None` for positions it finds
/// evenly spaced, and runs of three or more stops at one offset cut to the
/// first and last. With whether every colour is opaque.
fn stops(
    spec: &GradientSpec,
) -> Option<(Vec<Color4f>, Option<Vec<f32>>, bool)> {
    let (given, pos) = (&spec.colors, &spec.positions);
    if given.len() < 2 || pos.len() != given.len() {
        return None;
    }
    let first_implicit = pos[0] > 0.0;
    let mut last_implicit = pos[pos.len() - 1] != 1.0;
    let mut colors = Vec::with_capacity(given.len() + 2);
    if first_implicit {
        colors.push(given[0]);
    }
    colors.extend_from_slice(given);
    if last_implicit {
        colors.push(given[given.len() - 1]);
    }
    let opaque = given.iter().all(|c| c.a == 1.0);

    let mut positions = Vec::with_capacity(colors.len());
    let mut prev = 0.0_f32;
    positions.push(prev);
    let start = usize::from(!first_implicit);
    let end = pos.len() + usize::from(last_implicit);
    let step = pos[start] - prev;
    let mut uniform = true;
    for i in start..end {
        let mut curr = 1.0_f32;
        if i != pos.len() {
            curr = pos[i].max(prev).min(1.0);
            if curr == 1.0 && last_implicit {
                last_implicit = false;
            }
        }
        uniform &= nearly_equal(step, curr - prev, NEARLY_ZERO);
        prev = curr;
        positions.push(curr);
    }
    if uniform {
        return Some((colors, None, opaque));
    }
    // Of three or more stops at one offset, under clamp, the leftmost and
    // the rightmost.
    let mut kept_colors = Vec::with_capacity(colors.len());
    let mut kept_positions = Vec::with_capacity(colors.len());
    let mut i = 0;
    for j in 1..=colors.len() {
        if j == colors.len() || positions[i] != positions[j] {
            kept_positions.push(positions[i]);
            kept_colors.push(colors[i]);
            if j - i > 1 {
                kept_positions.push(positions[j - 1]);
                kept_colors.push(colors[j - 1]);
            }
            i = j;
        }
    }
    Some((kept_colors, Some(kept_positions), opaque))
}

/// The space the CPU interpolates in, where it is the page's own so that
/// nothing converts the result: `None` elsewhere -- the CPU's conversion
/// back runs through skcms's approximate transfer functions, which the
/// effect does not reproduce -- and wherever a working space is wrapped
/// around the shader.
///
/// A space named rather than taken from the destination is the page's own
/// where it equals the page's: sRGB on an sRGB page, Display P3 on a
/// Display P3 one. Skia builds its Display P3 from the sRGB transfer
/// function and `SkNamedGamut::kDisplayP3` (`intermediate_color_space` in
/// `SkGradientBaseShader.cpp`), the gamut its CICP table gives the code
/// point a page's Display P3 is built from (`SkColorSpace.cpp`), so the two
/// compare equal and the CPU converts nothing.
fn intermediate_space(
    spec: &GradientSpec,
    page: &ColorSpace,
) -> Option<ColorSpace> {
    if spec.space.working_space().is_some() {
        return None;
    }
    let intermediate = match spec.interpolation.color_space {
        InterpolationSpace::Destination => page.clone(),
        InterpolationSpace::SRGB => ColorSpace::new_srgb(),
        InterpolationSpace::DisplayP3 => {
            PixelColorSpace::DisplayP3.to_skia_color_space().ok()?
        }
        _ => return None,
    };
    (intermediate == *page).then_some(intermediate)
}

/// `colors` converted from `from` to `to` by `SkConvertPixels`, as
/// `SkColor4fXformer` converts them: unpremultiplied floats both sides.
fn convert(
    colors: &[Color4f],
    from: &ColorSpace,
    to: &ColorSpace,
) -> Option<Vec<[f32; 4]>> {
    let info = |space: &ColorSpace| {
        ImageInfo::new(
            (colors.len() as i32, 1),
            ColorType::RGBAF32,
            AlphaType::Unpremul,
            Some(space.clone()),
        )
    };
    // Skia converts between equal spaces by copying.
    if from == to {
        return Some(colors.iter().map(|c| [c.r, c.g, c.b, c.a]).collect());
    }
    let row = colors.len() * 4 * size_of::<f32>();
    let mut src: Vec<u8> = colors
        .iter()
        .flat_map(|c| [c.r, c.g, c.b, c.a])
        .flat_map(f32::to_ne_bytes)
        .collect();
    let pixmap = Pixmap::new(&info(from), &mut src, row)?;
    let mut dst = vec![0u8; row];
    if !pixmap.read_pixels(&info(to), &mut dst, row, (0, 0)) {
        return None;
    }
    let floats: Vec<f32> = dst
        .as_chunks::<4>()
        .0
        .iter()
        .map(|bytes| f32::from_ne_bytes(*bytes))
        .collect();
    Some(floats.as_chunks::<4>().0.to_vec())
}

/// The array lengths the effect is compiled at: a gradient takes the
/// shortest that holds its intervals, so that a two-stop ramp does not
/// upload the uniforms of a hundred.
const SIZES: [usize; 4] = [4, 16, 64, 256];

thread_local! {
    /// The effect at each of [`SIZES`], compiled once per thread -- a
    /// `RuntimeEffect` is neither `Send` nor `Sync` -- with where each of
    /// its uniforms lies.
    static EFFECTS: Option<Vec<(usize, RuntimeEffect, Offsets)>> = SIZES
        .iter()
        .map(|&size| {
            let effect = RuntimeEffect::make_for_shader(sksl(size), None).ok()?;
            let offsets = Offsets::of(&effect)?;
            Some((size, effect, offsets))
        })
        .collect();
}

/// Where each uniform of the effect lies in its uniform data, in bytes:
/// found by name once, when it is compiled, rather than for every draw.
struct Offsets {
    size: usize,
    kind: usize,
    mask_degenerates: usize,
    negate: usize,
    compensate: usize,
    unswap: usize,
    mode: usize,
    count: usize,
    premul_after: usize,
    exact_scale: usize,
    alpha8: usize,
    t_scale: usize,
    t_bias: usize,
    p0: usize,
    p1: usize,
    ts: usize,
    factors: usize,
    biases: usize,
}

impl Offsets {
    /// `None` where the effect lacks a uniform the source declares.
    fn of(effect: &RuntimeEffect) -> Option<Self> {
        let at = |name: &str| {
            effect
                .uniforms()
                .iter()
                .find(|uniform| uniform.name() == name)
                .map(|uniform| uniform.offset())
        };
        Some(Offsets {
            size: effect.uniform_size(),
            kind: at("kind")?,
            mask_degenerates: at("maskDegenerates")?,
            negate: at("negate")?,
            compensate: at("compensate")?,
            unswap: at("unswap")?,
            mode: at("mode")?,
            count: at("count")?,
            premul_after: at("premulAfter")?,
            exact_scale: at("exactScale")?,
            alpha8: at("alpha8")?,
            t_scale: at("tScale")?,
            t_bias: at("tBias")?,
            p0: at("p0")?,
            p1: at("p1")?,
            ts: at("ts")?,
            factors: at("factors")?,
            biases: at("biases")?,
        })
    }
}

/// The effect, its arrays `size` long.
fn sksl(size: usize) -> String {
    format!(
        r#"
uniform int kind;
uniform int maskDegenerates;
uniform int negate;
uniform int compensate;
uniform int unswap;
uniform int mode;
uniform int count;
uniform int premulAfter;
uniform int exactScale;
uniform float alpha8;
uniform float tScale;
uniform float tBias;
uniform float p0;
uniform float p1;
uniform float ts[{size}];
uniform float4 factors[{size}];
uniform float4 biases[{size}];

// `div255_accurate`, and NEON's `div255`: the nearest whole level.
float4 nearest255(float4 v) {{
    return floor((v + 128.0 + floor((v + 128.0) / 256.0)) / 256.0);
}}

// `div255` off NEON: `(v + 255) >> 8`.
float4 above255(float4 v) {{
    return floor((v + 255.0) / 256.0);
}}

half4 main(float2 p) {{
    float x = p.x;
    float y = p.y;
    float t;
    bool masked = false;
    if (kind == 0) {{
        t = x;
    }} else if (kind == 1) {{
        t = sqrt(x * x + y * y);
    }} else if (kind == 2) {{
        float xa = abs(x);
        float ya = abs(y);
        float big = max(xa, ya);
        float slope = min(xa, ya) / big;
        float s = slope * slope;
        float phi = slope * (0.15912117063999176025390625 + s *
                            (-5.185396969318389892578125e-2 + s *
                            (2.476101927459239959716796875e-2 + s *
                            (-7.0547382347285747528076171875e-3))));
        if (xa < ya) {{ phi = 0.25 - phi; }}
        if (x < 0.0) {{ phi = 0.5 - phi; }}
        if (y < 0.0) {{ phi = 1.0 - phi; }}
        if (big == 0.0) {{ phi = 0.0; }}
        t = phi;
    }} else if (kind == 3) {{
        float q = p0 - y * y;
        masked = q < 0.0;
        t = x + sqrt(max(q, 0.0));
    }} else if (kind == 4) {{
        masked = x == 0.0;
        t = x + y * y / x;
    }} else if (kind == 5) {{
        t = sqrt(x * x + y * y) - x * p0;
    }} else {{
        float q = x * x - y * y;
        masked = q < 0.0;
        float root = sqrt(max(q, 0.0));
        t = (kind == 6 ? root : -root) - x * p0;
    }}
    if (maskDegenerates != 0 && t <= 0.0) {{
        masked = true;
    }}
    if (masked) {{
        return half4(0);
    }}
    if (negate != 0) {{ t = -t; }}
    if (compensate != 0) {{ t = t + p1; }}
    if (unswap != 0) {{ t = 1.0 - t; }}
    t = t * tScale + tBias;

    float4 f = factors[0];
    float4 b = biases[0];
    if (mode == 0) {{
        for (int i = 1; i < {size}; i++) {{
            if (i >= count || t < ts[i]) {{ break; }}
            f = factors[i];
            b = biases[i];
        }}
    }} else {{
        t = clamp(t, 0.0, 1.0);
        if (mode == 1) {{
            int index = int(t * float(count - 1));
            for (int i = 1; i < {size}; i++) {{
                if (i > index) {{ break; }}
                f = factors[i];
                b = biases[i];
            }}
        }}
    }}
    float4 c = t * f + b;
    c.rgb = clamp(c.rgb, 0.0, 1.0);
    float4 l = floor(c * 255.0 + 0.5);
    if (premulAfter != 0) {{
        l.rgb = nearest255(float4(l.rgb * l.a, 0)).rgb;
    }}
    if (alpha8 < 255.0) {{
        l = exactScale != 0 ? nearest255(l * alpha8) : above255(l * alpha8);
    }}
    return half4(l / 255.0);
}}
"#
    )
}

/// The effect's uniforms for `layout`, `ramp` and `alpha`.
fn uniforms(
    offsets: &Offsets,
    size: usize,
    layout: &Layout,
    ramp: &Ramp,
    alpha: f32,
) -> Data {
    // `from_float`: the paint's alpha as the level the pipeline scales by.
    let alpha8 = (alpha * 255.0 + 0.5).trunc();
    let mut data = vec![0u8; offsets.size];
    let mut put = |at: usize, bytes: [u8; 4]| {
        data[at..at + 4].copy_from_slice(&bytes);
    };
    put(offsets.kind, (layout.kind as i32).to_ne_bytes());
    put(
        offsets.mask_degenerates,
        layout.flag(Layout::MASK_DEGENERATES),
    );
    put(offsets.negate, layout.flag(Layout::NEGATE));
    put(offsets.compensate, layout.flag(Layout::COMPENSATE));
    put(offsets.unswap, layout.flag(Layout::UNSWAP));
    put(offsets.mode, ramp.mode.to_ne_bytes());
    put(offsets.count, (ramp.ts.len() as i32).to_ne_bytes());
    put(
        offsets.premul_after,
        i32::from(ramp.premul_after).to_ne_bytes(),
    );
    put(offsets.exact_scale, i32::from(EXACT_SCALE).to_ne_bytes());
    put(offsets.alpha8, alpha8.to_ne_bytes());
    put(offsets.t_scale, layout.t_scale.to_ne_bytes());
    put(offsets.t_bias, layout.t_bias.to_ne_bytes());
    put(offsets.p0, layout.p0.to_ne_bytes());
    put(offsets.p1, layout.p1.to_ne_bytes());
    for (i, t) in ramp.ts.iter().take(size).enumerate() {
        put(offsets.ts + 4 * i, t.to_ne_bytes());
    }
    for (at, rows) in [
        (offsets.factors, &ramp.factors),
        (offsets.biases, &ramp.biases),
    ] {
        for (i, row) in rows.iter().take(size).enumerate() {
            for (k, v) in row.iter().enumerate() {
                put(at + 16 * i + 4 * k, v.to_ne_bytes());
            }
        }
    }
    Data::new_copy(&data)
}

/// Whether the CPU's `div255` rounds to the nearest level, as NEON
/// computes it, or is `(v + 255) >> 8`, as every other backend of
/// `SkRasterPipeline_opts.h` computes it.
const EXACT_SCALE: bool = cfg!(target_arch = "aarch64");

#[cfg(all(test, any(feature = "metal", feature = "vulkan")))]
mod tests;
