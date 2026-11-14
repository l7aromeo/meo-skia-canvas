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
//! by the paint's alpha with `div255`; on the float pipeline (`highp`) at
//! the store, which takes a half level to the even one on aarch64 and
//! x86-64. The effect returns whole levels,
//! which the half-precision fragment output carries to an eight-bit target
//! unchanged.
//!
//! The effect is drawn at its target's identity and takes the coordinate
//! Ganesh hands it to that pixel's centre, which it then maps to the
//! gradient's space as the CPU does, by the one matrix the CPU composes --
//! see [`Exact::placed`]. That coordinate is a pixel's own only where the
//! effect fills an aliased rectangle on whole pixels, so a draw fills its
//! shape with an image of the effect over the pixels it reaches, sampled
//! nearest -- see [`GradientSpec::sampled`].
//!
//! What is left is float noise: a GPU may fuse a product into the sum that
//! follows it where the CPU rounds the two apart, in the matrix and in the
//! formula for `t`, so `t` can part from the CPU's in its last bits, and a
//! pixel whose value lies within that of a half level, or whose `t` lies
//! within that of a stop, can round the other way.
//!
//! A gradient interpolated in a space other than the page's -- linear sRGB,
//! sRGB, Display P3, A98 RGB or ProPhoto RGB -- is taken to the page as the
//! CPU's `SkColorSpaceXformSteps` takes it: its transfer function undone, the
//! gamut matrix, the page's transfer function applied, each curve through
//! Skia's own `approx_powf` rebuilt in arithmetic -- see `Toward`. One in
//! Oklab, Lab, LCH, OkLCH, HSL or HWB is first converted back to the space
//! its stops were converted from, as the CPU's own stages do -- see `Css`;
//! the polar four with their stops split and their hues walked on the host
//! as `SkColor4fXformer` does both -- see `split_powerless` and
//! `walk_hues`. That runs on the float pipeline, so it is taken only where
//! that pipeline rounds nothing before the store.
//!
//! [`GradientSpec::exact`] answers `None` where the effect cannot reproduce
//! the CPU: an interpolation space it does not convert from (a working
//! space wrapped around the shader, a PQ or HLG curve); a
//! gradient on the float pipeline -- a
//! two-point conical, or one converted to the page -- whose stops are
//! translucent or that is drawn at an alpha (that pipeline rounds once,
//! after blending); more stops than the largest effect holds; and Skia's
//! degenerate shapes.

#[cfg(test)]
use std::cell::Cell;
use std::{cell::RefCell, sync::Arc};

use skia_safe::{
    AlphaType, Color4f, ColorSpace, ColorSpacePrimaries, ColorSpaceTransferFn,
    ColorType, Data, FilterMode, IPoint, IRect, ImageInfo, Matrix, MipmapMode,
    Paint, PictureRecorder, Pixmap, Point, Rect, RuntimeEffect,
    SamplingOptions, Shader, TileMode,
    gradient::{
        Colors as GradientColors, Gradient as SkGradient, Interpolation,
        interpolation::{
            ColorSpace as InterpolationSpace, HueMethod, InPremul,
        },
        shaders as gradient_shaders,
    },
    images,
    matrix::TypeMask,
    named_primaries, named_transfer_fn,
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
    /// The effect computes the CPU's bytes from the coordinates Ganesh
    /// hands it, taken to their pixel's centre -- see [`Exact::placed`] --
    /// which are that pixel's for an aliased rectangle on whole pixels and
    /// not always elsewhere: through a path renderer,
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
        let from_device = matrix.invert()?;
        let effect = self
            .exact(page, alpha)?
            .placed(matrix, IPoint::new(area.left, area.top))?;
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

    /// The effect that draws this on the GPU in the bytes the CPU stores,
    /// on an eight-bit page in `page`, at `alpha` -- the paint's, which the
    /// shader takes and the paint drawing it then leaves at 1 -- to be
    /// placed where a draw puts it: see [`Exact::placed`]. `None` where the
    /// effect cannot reproduce the CPU -- see the module's notes. Exact only
    /// where Ganesh draws it as an aliased rectangle on whole pixels -- see
    /// [`Self::sampled`].
    ///
    /// The answer is the one this thread last made for a gradient whose
    /// every number is the same to the bit, on the same page at the same
    /// alpha, where [`EXACTS`] still keeps it: a page redrawn every frame
    /// makes each of its gradients afresh, as a Canvas API caller makes a
    /// new `CanvasGradient` for every draw, and making one reads its stops
    /// through a colour conversion and uploads its uniforms.
    pub fn exact(&self, page: &ColorSpace, alpha: f32) -> Option<Exact> {
        #[cfg(test)]
        EXACTS_ASKED.set(EXACTS_ASKED.get() + 1);
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
    fn made(&self, page: &ColorSpace, alpha: f32) -> Option<Exact> {
        #[cfg(test)]
        EXACTS_MADE.set(EXACTS_MADE.get() + 1);
        let layout = Layout::of(self)?;
        let ramp = Ramp::of(self, page)?;
        // The `highp` pipeline premultiplies, scales and blends in floats
        // and rounds once, at the store; the effect hands the GPU whole
        // levels, which match it only where none of those happen.
        // A conversion to the page runs on the float pipeline too, as
        // does Lab's or Oklab's back to the space it was converted from.
        let highp =
            layout.highp || !ramp.toward.is_identity() || ramp.css != Css::None;
        if highp && (!ramp.opaque || alpha < 1.0) {
            return None;
        }
        EFFECTS.with(|effects| {
            let (size, _, offsets) = effects
                .as_ref()?
                .iter()
                .find(|(size, ..)| ramp.ts.len() <= *size)?;
            Some(Exact {
                size: *size,
                uniforms: uniforms(
                    offsets, *size, &layout, &ramp, alpha, highp,
                )
                .into(),
                to_unit: layout.to_unit,
                local: self.local,
            })
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

/// The effect for a gradient on a page at an alpha, made and kept but not
/// yet placed on the page: [`Exact::placed`] gives the shader that draws it
/// where a draw puts it.
#[derive(Clone)]
pub(crate) struct Exact {
    /// The length of the effect's arrays, which picks it from [`EFFECTS`].
    size: usize,
    /// Its uniforms but the placement's.
    uniforms: Arc<[u8]>,
    /// The gradient's own matrix -- see [`Layout::to_unit`].
    to_unit: Matrix,
    /// The shader's local matrix -- see [`GradientSpec::local`].
    local: Option<Matrix>,
}

impl Exact {
    /// The shader that draws the gradient onto a target whose device pixel
    /// `(x, y)` is the page's `(x + offset.x, y + offset.y)`, as the page
    /// draws it under `matrix`, drawn at the target's identity: `None`
    /// where the CPU's matrix has no inverse.
    ///
    /// The effect takes the coordinate it is handed to its pixel's centre,
    /// `floor(p) + 0.5`, which is the target's pixel centre wherever the
    /// coordinate is off it by under half a pixel: an NVIDIA GPU hands a
    /// runtime effect an `x` a little below each centre, and a pixel whose
    /// `t` the CPU finds exactly on a stop then took the colour before it.
    /// It adds `offset`, which is the page's pixel centre exactly, as
    /// `seed_shader` makes it, and maps it to the gradient's space by the
    /// one matrix the CPU maps it by, `SkShaders::MatrixRec::apply`'s
    /// `Concat(fPtsToUnit, inverse(Concat(CTM, localMatrix)))`, made here
    /// with Skia's own `SkMatrix::Concat` and `invert`, in the form
    /// `appendMatrix` picks for it: a translation, a scale and a
    /// translation, a 2x3, or a perspective, each in its stage's order of
    /// operations. `matrix_perspective` multiplies by `rcp_precise` of the
    /// third coordinate, a hardware estimate refined -- twice on NEON, once
    /// on SSE -- which the effect cannot reproduce to the bit and takes as
    /// the quotient: a perspective's `t` parts from the CPU's in its last
    /// bits there too.
    pub fn placed(&self, matrix: &Matrix, offset: IPoint) -> Option<Shader> {
        let to_unit = self.unit_matrix(matrix)?;
        #[cfg(test)]
        if to_unit.has_perspective() {
            PLACED_IN_PERSPECTIVE.set(PLACED_IN_PERSPECTIVE.get() + 1);
        }
        EFFECTS.with(|effects| {
            let (_, effect, offsets) = effects
                .as_ref()?
                .iter()
                .find(|(size, ..)| *size == self.size)?;
            let mut data = self.uniforms.to_vec();
            placement(&mut data, offsets, &to_unit, offset);
            effect.make_shader(Data::new_copy(&data), &[], None)
        })
    }
}

impl Exact {
    /// Whether the effect can be placed under `matrix` -- see
    /// [`Self::placed`].
    pub fn places(&self, matrix: &Matrix) -> bool {
        self.unit_matrix(matrix).is_some()
    }

    /// The CPU's matrix from the page's pixels to the gradient's space,
    /// for a draw under `matrix` -- see [`Self::placed`].
    fn unit_matrix(&self, matrix: &Matrix) -> Option<Matrix> {
        let total = match &self.local {
            Some(local) => Matrix::concat(matrix, local),
            None => *matrix,
        };
        Some(Matrix::concat(&self.to_unit, &total.invert()?))
    }
}

/// `matrix`'s form, as `appendMatrix` picks the stage that applies it.
fn matrix_form(matrix: &Matrix) -> i32 {
    let kind = matrix.get_type();
    if matrix.has_perspective() {
        MatrixForm::PERSPECTIVE
    } else if kind.is_empty() {
        MatrixForm::IDENTITY
    } else if kind == TypeMask::TRANSLATE {
        MatrixForm::TRANSLATE
    } else if (kind - (TypeMask::SCALE | TypeMask::TRANSLATE)).is_empty() {
        MatrixForm::SCALE_TRANSLATE
    } else {
        MatrixForm::AFFINE
    }
}

/// The stage `appendMatrix` applies a matrix through, as the effect's
/// `matrixForm()` names it.
struct MatrixForm;

impl MatrixForm {
    /// `matrix_2x3`.
    const AFFINE: i32 = 3;
    /// None: the identity is left out.
    const IDENTITY: i32 = 0;
    /// `matrix_perspective`.
    const PERSPECTIVE: i32 = 4;
    /// `matrix_scale_translate`.
    const SCALE_TRANSLATE: i32 = 2;
    /// `matrix_translate`.
    const TRANSLATE: i32 = 1;
}

/// `data`, an effect's uniforms, placed by `to_unit` and `offset` -- see
/// [`Exact::placed`].
fn placement(
    data: &mut [u8],
    offsets: &Offsets,
    to_unit: &Matrix,
    offset: IPoint,
) {
    let mut put = |at: usize, bytes: [u8; 4]| {
        data[at..at + 4].copy_from_slice(&bytes);
    };
    put(offsets.matrix_form, matrix_form(to_unit).to_ne_bytes());
    // `get9`'s order: `scaleX, skewX, transX`, then `skewY, scaleY,
    // transY`, then the perspective's three, a row each.
    let mut nine = [0.0; 9];
    to_unit.get_9(&mut nine);
    for (at, row) in [
        (offsets.m_row0, &nine[..3]),
        (offsets.m_row1, &nine[3..6]),
        (offsets.m_row2, &nine[6..]),
    ] {
        for (k, v) in row.iter().enumerate() {
            put(at + 4 * k, v.to_ne_bytes());
        }
    }
    put(offsets.offset, (offset.x as f32).to_ne_bytes());
    put(offsets.offset + 4, (offset.y as f32).to_ne_bytes());
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
type Exacts = KeptSets<ExactKey, Option<Exact>, EXACT_SETS, EXACT_WAYS>;

thread_local! {
    /// The shaders [`GradientSpec::exact`] has made on this thread.
    static EXACTS: RefCell<Exacts> = const { RefCell::new(KeptSets::new()) };
}

#[cfg(test)]
thread_local! {
    /// How many shaders [`GradientSpec::exact`] has made on this thread
    /// rather than found kept.
    pub(crate) static EXACTS_MADE: Cell<usize> = const { Cell::new(0) };
    /// How many times [`GradientSpec::exact`] has been asked on this thread,
    /// made or found.
    pub(crate) static EXACTS_ASKED: Cell<usize> = const { Cell::new(0) };
    /// How many shaders [`Exact::placed`] has given under a perspective on
    /// this thread.
    pub(crate) static PLACED_IN_PERSPECTIVE: Cell<usize> =
        const { Cell::new(0) };
    /// Whether [`GradientSpec::exact`] takes a shader kept: off, every
    /// shader is made afresh, the reference the kept ones are held to.
    pub(crate) static KEEPS_EXACTS: Cell<bool> = const { Cell::new(true) };
}

/// How `t` is computed, in the order the CPU's stages compute it.
#[derive(Clone, Debug, PartialEq)]
struct Layout {
    kind: Kind,
    /// The gradient's own matrix, `fPtsToUnit`, which maps the shader's
    /// space to the one `kind` computes `t` in.
    to_unit: Matrix,
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

    /// Whether `flag` is set, as the effect's flags take it.
    fn flag(&self, flag: i32) -> [u8; 4] {
        i32::from(self.flags & flag != 0).to_ne_bytes()
    }

    /// `None` where `to_unit` has no inverse, which Skia's factories refuse
    /// as degenerate.
    fn plain(kind: Kind, to_unit: Matrix) -> Option<Layout> {
        to_unit.invert()?;
        Some(Layout {
            kind,
            to_unit,
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
    /// What converts an interpolated colour back to the space it was
    /// converted from before interpolating -- see [`Css`].
    css: Css,
    /// What converts an interpolated colour to the page -- see [`Toward`].
    toward: Toward,
}

impl Ramp {
    /// `evenly_spaced_gradient`: `t` clamped, the interval its index.
    const EVENLY: i32 = 1;
    /// The `gradient` stage: arbitrary stops, searched.
    const GENERAL: i32 = 0;
    /// `evenly_spaced_2_stop_gradient`: `t` clamped, one interval.
    const TWO: i32 = 2;

    fn of(spec: &GradientSpec, page: &ColorSpace) -> Option<Ramp> {
        let Stops {
            colors,
            positions,
            opaque,
            first_implicit,
            last_implicit,
        } = stops(spec)?;
        let intermediate = intermediate_space(spec, page)?;
        let toward = Toward::of(&intermediate, page)?;
        let css = Css::of(spec.interpolation.color_space);
        let converted: Vec<([f32; 4], bool)> =
            convert(&colors, &spec.color_space, &intermediate)?
                .into_iter()
                .map(|c| css.converted(c))
                .collect();
        let (mut colors, positions) =
            match converted.iter().any(|&(_, powerless)| powerless) {
                true => {
                    let split =
                        split_powerless(&converted, positions.as_deref());
                    (split.0, Some(split.1))
                }
                false => {
                    (converted.into_iter().map(|(c, _)| c).collect(), positions)
                }
            };
        if css.is_polar() {
            walk_hues(
                &mut colors,
                spec.interpolation.hue_method,
                first_implicit,
                last_implicit,
            );
        }
        let premul = spec.interpolation.in_premul == InPremul::Yes;
        if premul {
            // `premul_polar` leaves the hue alone; `premul_rgb` scales all
            // three.
            let from = usize::from(css.is_polar());
            for c in &mut colors {
                let a = c[3];
                c[from..3].iter_mut().for_each(|v| *v *= a);
            }
        }
        let premul_after = !premul && !opaque;
        let mut ramp = match positions {
            None if colors.len() == 2 => Ramp {
                mode: Ramp::TWO,
                ts: vec![0.0],
                factors: vec![std::array::from_fn(|k| {
                    colors[1][k] - colors[0][k]
                })],
                biases: vec![colors[0]],
                opaque,
                premul_after,
                css: Css::None,
                toward: Toward::default(),
            },
            None => evenly(&colors, opaque, premul_after),
            Some(positions) => {
                general(&colors, &positions, opaque, premul_after)
            }
        };
        ramp.css = css;
        ramp.toward = toward;
        Some(ramp)
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
        css: Css::None,
        toward: Toward::default(),
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
        css: Css::None,
        toward: Toward::default(),
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

/// The stops as `SkGradientBaseShader`'s constructor leaves them -- see
/// [`stops`].
struct Stops {
    colors: Vec<Color4f>,
    /// `None` where the constructor finds the positions evenly spaced.
    positions: Option<Vec<f32>>,
    /// Whether every colour is opaque.
    opaque: bool,
    /// Whether the first stop is the copy added at 0, and the last the copy
    /// added at 1: the longer hue method walks neither pair of hues the
    /// long way -- see [`walk_hues`].
    first_implicit: bool,
    last_implicit: bool,
}

/// The stops as `SkGradientBaseShader`'s constructor leaves them: bracketed
/// by stops at 0 and 1, pinned to rise, `None` for positions it finds
/// evenly spaced, and runs of three or more stops at one offset cut to the
/// first and last.
fn stops(spec: &GradientSpec) -> Option<Stops> {
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
        return Some(Stops {
            colors,
            positions: None,
            opaque,
            first_implicit,
            last_implicit,
        });
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
    Some(Stops {
        colors: kept_colors,
        positions: Some(kept_positions),
        opaque,
        first_implicit,
        last_implicit,
    })
}

/// The stops with each whose hue is powerless split in two at its
/// position, as `SkColor4fXformer` splits them: one taking the hue of the
/// stop before it, one the hue of the stop after, the first stop keeping
/// only the second and the last only the first. The positions are always
/// returned, as Skia then hands the gradient stage positions even where
/// the stops were evenly spaced.
///
/// A neighbour's hue may itself be powerless; Skia copies it all the same,
/// and so does this.
fn split_powerless(
    stops: &[([f32; 4], bool)],
    positions: Option<&[f32]>,
) -> (Vec<[f32; 4]>, Vec<f32>) {
    let n = stops.len();
    // `getPos`: evenly spaced stops are at `i / (n - 1)`, in f32.
    let at = |i: usize| match positions {
        Some(positions) => positions[i],
        None => i as f32 / (n - 1) as f32,
    };
    let with_hue_of = |c: [f32; 4], from: [f32; 4]| [from[0], c[1], c[2], c[3]];
    let mut colors = Vec::with_capacity(2 * n);
    let mut kept = Vec::with_capacity(2 * n);
    for (i, &(c, powerless)) in stops.iter().enumerate() {
        if !powerless {
            colors.push(c);
            kept.push(at(i));
            continue;
        }
        if i != 0 {
            colors.push(with_hue_of(c, stops[i - 1].0));
            kept.push(at(i));
        }
        if i != n - 1 {
            colors.push(with_hue_of(c, stops[i + 1].0));
            kept.push(at(i));
        }
    }
    (colors, kept)
}

/// The hues of `colors` adjusted for `method`, as `SkColor4fXformer` does:
/// a pair's second hue moved by a whole turn where the method takes the
/// other way round, and each later hue by the turns before it, so that the
/// interpolation needs no adjustment per pixel. The longer method leaves the
/// pair an implicit first or last stop makes as it is (skbug.com/40044215).
///
/// `first_implicit` and `last_implicit` are tested against the first and
/// last pair of `colors` after [`split_powerless`], which is where Skia
/// tests them.
fn walk_hues(
    colors: &mut [[f32; 4]],
    method: HueMethod,
    first_implicit: bool,
    last_implicit: bool,
) {
    let n = colors.len();
    let mut delta = 0.0_f32;
    for i in 0..n - 1 {
        let h1 = colors[i][0];
        let mut h2 = colors[i + 1][0] + delta;
        let d = h2 - h1;
        let turn = match method {
            HueMethod::Shorter if d > HALF_TURN => -TURN,
            HueMethod::Shorter if d < -HALF_TURN => TURN,
            HueMethod::Longer
                if (i == 0 && first_implicit)
                    || (i == n - 2 && last_implicit) =>
            {
                0.0
            }
            HueMethod::Longer if 0.0 < d && d < HALF_TURN => -TURN,
            HueMethod::Longer if -HALF_TURN < d && d <= 0.0 => TURN,
            HueMethod::Increasing if h2 < h1 => TURN,
            HueMethod::Decreasing if h1 < h2 => -TURN,
            _ => 0.0,
        };
        h2 += turn;
        delta += turn;
        colors[i + 1][0] = h2;
    }
}

/// A whole turn of hue, in the degrees the polar spaces keep it in.
const TURN: f32 = 360.0;

/// Half a turn: two hues further apart than this are nearer the other way.
const HALF_TURN: f32 = 180.0;

/// The space the CPU interpolates in, as `intermediate_color_space` in
/// `SkGradientBaseShader.cpp` picks it for the interpolation space `spec`
/// names: the page's own for the destination, and otherwise the space named,
/// built as Skia builds it -- see [`skia_space`]. The CPU converts an
/// interpolated colour from it to the page -- see [`Toward`].
///
/// Lab's and LCH's is XYZ D50 made linear, Oklab's and OkLCH's linear sRGB,
/// and HSL's and HWB's sRGB, the spaces the stops are converted into and
/// out of -- see [`Css`]. ProPhoto RGB's gamut is derived from its
/// primaries, as Skia derives it -- see [`primaries_to_xyzd50`]. `None`
/// where a working space is wrapped around the shader.
fn intermediate_space(
    spec: &GradientSpec,
    page: &ColorSpace,
) -> Option<ColorSpace> {
    if spec.space.working_space().is_some() {
        return None;
    }
    match spec.interpolation.color_space {
        InterpolationSpace::Destination => Some(page.clone()),
        InterpolationSpace::SRGB => Some(ColorSpace::new_srgb()),
        InterpolationSpace::SRGBLinear => Some(ColorSpace::new_srgb_linear()),
        InterpolationSpace::DisplayP3 => {
            PixelColorSpace::DisplayP3.to_skia_color_space().ok()
        }
        InterpolationSpace::A98RGB => skia_space(
            &transfer_floats(&named_transfer_fn::A98_RGB),
            &from_fixed(&ADOBE_RGB_GAMUT),
        ),
        InterpolationSpace::ProphotoRGB => skia_space(
            &transfer_floats(&named_transfer_fn::PRO_PHOTO_RGB),
            &primaries_to_xyzd50(&named_primaries::PRO_PHOTO_RGB)?,
        ),
        InterpolationSpace::Lab | InterpolationSpace::LCH => {
            skia_space(&LINEAR_TF, &from_fixed(&XYZ_GAMUT))
        }
        InterpolationSpace::OKLab | InterpolationSpace::OKLCH => {
            Some(ColorSpace::new_srgb_linear())
        }
        InterpolationSpace::HSL | InterpolationSpace::HWB => {
            Some(ColorSpace::new_srgb())
        }
        _ => None,
    }
}

/// `SkNamedGamut::kAdobeRGB` in `SkColorSpace.h`: the gamut's matrix to XYZ
/// D50 in ICC 16.16 fixed point, row by row, which Skia turns into floats
/// with `SkFixedToFloat`.
const ADOBE_RGB_GAMUT: [[i32; 3]; 3] = [
    [0x9c18, 0x348d, 0x2631],
    [0x4fa5, 0xa02c, 0x102f],
    [0x04fc, 0x0f95, 0xbe9c],
];

/// The version `SkColorSpace::Deserialize` reads (`k1_Version` in
/// `SkColorSpace.cpp`): a four-byte header, then the transfer function's
/// seven floats and the matrix's nine.
const SERIALIZED_VERSION: u8 = 1;

/// The space Skia's `SkColorSpace::MakeRGB` makes from the transfer function
/// `tf`, as `g, a, b, c, d, e, f`, and the matrix to XYZ D50 `gamut`: built
/// through `SkColorSpace::Deserialize`, which takes the same sixteen floats
/// and calls `MakeRGB` with them, so that it compares equal to the one
/// Skia's gradient builds -- skia-safe does not bind `MakeRGB` itself.
fn skia_space(tf: &[f32; 7], gamut: &Matrix3) -> Option<ColorSpace> {
    let mut bytes = vec![SERIALIZED_VERSION, 0, 0, 0];
    bytes.extend(tf.iter().flat_map(|v| v.to_ne_bytes()));
    bytes.extend(gamut.iter().flatten().flat_map(|v| v.to_ne_bytes()));
    Some(ColorSpace::deserialize(Data::new_copy(&bytes)))
}

/// `tf` as `g, a, b, c, d, e, f`.
fn transfer_floats(tf: &ColorSpaceTransferFn) -> [f32; 7] {
    [tf.g, tf.a, tf.b, tf.c, tf.d, tf.e, tf.f]
}

/// A gamut Skia writes in ICC 16.16 fixed point, as `SkFixedToFloat` turns
/// it into floats: each value times 2^-16.
fn from_fixed(gamut: &[[i32; 3]; 3]) -> Matrix3 {
    gamut.map(|row| row.map(|v| v as f32 * (1.0 / (1u32 << 16) as f32)))
}

/// A 3x3 matrix as skcms keeps one, row by row.
type Matrix3 = [[f32; 3]; 3];

/// The D50 white skcms adapts to, `wXYZD50` in `skcms_AdaptToXYZD50`.
const D50_XYZ: [f32; 3] = [0.96422, 1.0, 0.82521];

/// Bradford's cone response matrix, `xyz_to_lms` in `skcms_AdaptToXYZD50`.
const BRADFORD: Matrix3 = [
    [0.8951, 0.2664, -0.1614],
    [-0.7502, 1.7135, 0.0367],
    [0.0389, -0.0685, 1.0296],
];

/// Its inverse as skcms writes it, `lms_to_xyz`: to seven figures, not the
/// inverse of [`BRADFORD`] in f32.
#[allow(clippy::excessive_precision)]
const BRADFORD_INVERSE: Matrix3 = [
    [0.9869929, -0.1470543, 0.1599627],
    [0.4323053, 0.5183603, 0.0492912],
    [-0.0085287, 0.0400428, 0.9684867],
];

/// `mv_mul` in `skcms.cc`: `m` times `v`.
fn mv_mul(m: &Matrix3, v: &[f32; 3]) -> [f32; 3] {
    m.map(|row| row[0] * v[0] + row[1] * v[1] + row[2] * v[2])
}

/// `skcms_Matrix3x3_concat`: `a` times `b`.
fn concat(a: &Matrix3, b: &Matrix3) -> Matrix3 {
    std::array::from_fn(|r| {
        std::array::from_fn(|c| {
            a[r][0] * b[0][c] + a[r][1] * b[1][c] + a[r][2] * b[2][c]
        })
    })
}

/// `skcms_Matrix3x3_invert`: by cofactors in f64, each entry rounded to f32
/// at the end, the names its own -- `a01` is row 1, column 0. `None` where
/// skcms refuses: a singular matrix, or an inverse f32 cannot hold.
fn invert(m: &Matrix3) -> Option<Matrix3> {
    let a = |r: usize, c: usize| f64::from(m[r][c]);
    let (a00, a01, a02) = (a(0, 0), a(1, 0), a(2, 0));
    let (a10, a11, a12) = (a(0, 1), a(1, 1), a(2, 1));
    let (a20, a21, a22) = (a(0, 2), a(1, 2), a(2, 2));
    let b = [
        a00 * a11 - a01 * a10,
        a00 * a12 - a02 * a10,
        a01 * a12 - a02 * a11,
        a20,
        a21,
        a22,
    ];
    let determinant = b[0] * b[5] - b[1] * b[4] + b[2] * b[3];
    if determinant == 0.0 {
        return None;
    }
    let invdet = 1.0 / determinant;
    let limit = f64::from(f32::MAX);
    if invdet > limit || invdet < -limit || !(invdet as f32).is_finite() {
        return None;
    }
    let [b0, b1, b2, b3, b4, b5] = b.map(|v| v * invdet);
    let inverse = [
        [
            a11 * b5 - a12 * b4,
            a12 * b3 - a10 * b5,
            a10 * b4 - a11 * b3,
        ],
        [
            a02 * b4 - a01 * b5,
            a00 * b5 - a02 * b3,
            a01 * b3 - a00 * b4,
        ],
        [b2, -b1, b0],
    ]
    .map(|row| row.map(|v| v as f32));
    inverse
        .iter()
        .flatten()
        .all(|v| v.is_finite())
        .then_some(inverse)
}

/// `skcms_AdaptToXYZD50`: Bradford's adaptation from the white `wx, wy` to
/// D50.
fn adapt_to_d50(wx: f32, wy: f32) -> Matrix3 {
    let white = [wx / wy, 1.0, (1.0 - wx - wy) / wy];
    let from = mv_mul(&BRADFORD, &white);
    let to = mv_mul(&BRADFORD, &D50_XYZ);
    let scale = [
        [to[0] / from[0], 0.0, 0.0],
        [0.0, to[1] / from[1], 0.0],
        [0.0, 0.0, to[2] / from[2]],
    ];
    concat(&BRADFORD_INVERSE, &concat(&scale, &BRADFORD))
}

/// `skcms_PrimariesToXYZD50`, which `SkColorSpacePrimaries::toXYZD50` calls:
/// the matrix from linear RGB on `p`'s primaries to XYZ D50, in skcms's f32
/// order of operations, which Skia builds with `-ffp-contract=off` outside
/// Windows. Skia derives ProPhoto RGB's gamut this way when a gradient is
/// interpolated in it, rather than from a table.
fn primaries_to_xyzd50(p: &ColorSpacePrimaries) -> Option<Matrix3> {
    let in_unit = |v: f32| (0.0..=1.0).contains(&v);
    if ![p.rx, p.ry, p.gx, p.gy, p.bx, p.by, p.wx, p.wy]
        .into_iter()
        .all(in_unit)
    {
        return None;
    }
    let xy = [
        [p.rx, p.gx, p.bx],
        [p.ry, p.gy, p.by],
        [1.0 - p.rx - p.ry, 1.0 - p.gx - p.gy, 1.0 - p.bx - p.by],
    ];
    let white = [p.wx / p.wy, 1.0, (1.0 - p.wx - p.wy) / p.wy];
    let xyz = mv_mul(&invert(&xy)?, &white);
    let scale = [[xyz[0], 0.0, 0.0], [0.0, xyz[1], 0.0], [0.0, 0.0, xyz[2]]];
    Some(concat(&adapt_to_d50(p.wx, p.wy), &concat(&xy, &scale)))
}

/// What the CPU converts an interpolated colour through to reach the page,
/// as `SkColorSpaceXformSteps` decides it between the space interpolated in
/// and the page's: decode the source's transfer function, change gamut, and
/// encode the page's, each only where it does something. Every step runs on
/// the float pipeline, through Skia's `approx_powf` for a curve --
/// `parametric` and `gamma_` in `SkRasterPipeline_opts.h`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Toward {
    /// The source's transfer function, as `g, a, b, c, d, e, f`.
    linearize: Option<[f32; 7]>,
    /// The gamut change, as `matrix_3x3` takes it: column by column.
    gamut: Option<[f32; 9]>,
    /// The inverse of the page's transfer function.
    encode: Option<[f32; 7]>,
}

impl Toward {
    /// The steps from `from` to `to`; `None` for a curve Skia converts
    /// through other stages than `parametric` and `gamma_` -- PQ and HLG.
    fn of(from: &ColorSpace, to: &ColorSpace) -> Option<Toward> {
        if from.hash() == to.hash() {
            return Some(Toward::default());
        }
        let (source, page) = (
            transfer_floats(&from.transfer_fn()),
            transfer_floats(&to.transfer_fn()),
        );
        // `skcms_TransferFunction_getType` tells PQ and HLG by a negative
        // `g`.
        if source[0] < 0.0 || page[0] < 0.0 {
            return None;
        }
        let linear =
            |f: &[f32; 7]| f.map(f32::to_bits) == LINEAR_TF.map(f32::to_bits);
        let mut toward = Toward {
            linearize: (!linear(&source)).then_some(source),
            gamut: None,
            encode: (!linear(&page))
                .then(|| transfer_floats(&to.inv_transfer_fn())),
        };
        if from.to_xyzd50_hash() != to.to_xyzd50_hash() {
            // The matrix's columns are what it makes of each primary,
            // converted between the two spaces made linear: one channel
            // times a column entry plus zeros, so each comes out as the
            // entry itself.
            let (from, to) = (from.with_linear_gamma(), to.with_linear_gamma());
            let basis = [
                Color4f::new(1.0, 0.0, 0.0, 1.0),
                Color4f::new(0.0, 1.0, 0.0, 1.0),
                Color4f::new(0.0, 0.0, 1.0, 1.0),
            ];
            let columns = convert(&basis, &from, &to)?;
            toward.gamut = Some(std::array::from_fn(|i| columns[i / 3][i % 3]));
        } else if from.transfer_fn_hash() == to.transfer_fn_hash() {
            toward.linearize = None;
            toward.encode = None;
        }
        Some(toward)
    }

    /// Whether this converts nothing.
    fn is_identity(&self) -> bool {
        *self == Toward::default()
    }
}

/// `SkNamedTransferFn::kLinear`, as `g, a, b, c, d, e, f`.
const LINEAR_TF: [f32; 7] = [1.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0];

/// `SkNamedGamut::kXYZ`, the identity, in the 16.16 fixed point
/// [`skia_space`] takes: Lab's intermediate space is XYZ D50 itself.
const XYZ_GAMUT: [[i32; 3]; 3] =
    [[1 << 16, 0, 0], [0, 1 << 16, 0], [0, 0, 1 << 16]];

/// A space the CPU interpolates in through code of its own: the stops are
/// converted into it from the intermediate space once, on the host --
/// `xyzd50_to_lab`, `xyzd50_to_hcl`, `lin_srgb_to_oklab`,
/// `lin_srgb_to_okhcl`, `srgb_to_hsl` and `srgb_to_hwb` in
/// `SkGradientBaseShader.cpp` -- and each interpolated colour back --
/// `css_lab_to_xyz`, `css_oklab_to_linear_srgb`, `css_hcl_to_lab` before
/// either, `css_hsl_to_srgb` and `css_hwb_to_srgb` in
/// `SkRasterPipeline_opts.h`. Skia builds both with `-ffp-contract=off`
/// outside Windows, so each operation here is the one it makes, rounded
/// where it rounds; the host's `atan2f` and `cbrtf` are the platform's, as
/// Rust's are.
///
/// The polar spaces keep their hue first, as Skia does: LCH and OkLCH are
/// held as hue, chroma, lightness.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Css {
    #[default]
    None,
    Lab,
    OkLab,
    Lch,
    OkLch,
    Hsl,
    Hwb,
}

impl Css {
    fn of(space: InterpolationSpace) -> Css {
        match space {
            InterpolationSpace::Lab => Css::Lab,
            InterpolationSpace::OKLab => Css::OkLab,
            InterpolationSpace::LCH => Css::Lch,
            InterpolationSpace::OKLCH => Css::OkLch,
            InterpolationSpace::HSL => Css::Hsl,
            InterpolationSpace::HWB => Css::Hwb,
            _ => Css::None,
        }
    }

    /// Whether this keeps a hue, which `color_space_is_polar` in
    /// `SkGradientBaseShader.cpp` asks.
    fn is_polar(self) -> bool {
        matches!(self, Css::Lch | Css::OkLch | Css::Hsl | Css::Hwb)
    }

    /// The value the effect selects its conversion back by, its `css()`.
    fn index(self) -> i32 {
        self as i32
    }

    /// `c`, unpremultiplied in the intermediate space, in this space, with
    /// whether its hue is powerless. Ottosson's constants are written as
    /// `lin_srgb_to_oklab` writes them, so that each rounds to the f32
    /// Skia's does and can be checked against it.
    fn converted(self, c: [f32; 4]) -> ([f32; 4], bool) {
        match self {
            Css::None => (c, false),
            Css::Lab => (lab(c), false),
            Css::OkLab => (oklab(c), false),
            Css::Lch => hcl(lab(c), LCH_POWERLESS),
            Css::OkLch => hcl(oklab(c), OKLCH_POWERLESS),
            Css::Hsl => hsl(c),
            Css::Hwb => {
                let (hsl, powerless) = hsl(c);
                let white = c[0].min(c[1]).min(c[2]);
                let black = 1.0 - c[0].max(c[1]).max(c[2]);
                ([hsl[0], white * 100.0, black * 100.0, c[3]], powerless)
            }
        }
    }
}

/// `xyzd50_to_lab`.
fn lab(c: [f32; 4]) -> [f32; 4] {
    let f: [f32; 3] = std::array::from_fn(|i| {
        let v = c[i] / LAB_D50[i];
        match v > LAB_E {
            true => v.cbrt(),
            false => (LAB_K * v + 16.0) / 116.0,
        }
    });
    [
        (116.0 * f[1]) - 16.0,
        500.0 * (f[0] - f[1]),
        200.0 * (f[1] - f[2]),
        c[3],
    ]
}

/// `lin_srgb_to_oklab`.
#[allow(clippy::excessive_precision)]
fn oklab(c: [f32; 4]) -> [f32; 4] {
    let [r, g, b] = [c[0], c[1], c[2]];
    let l = (0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b).cbrt();
    let m = (0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b).cbrt();
    let s = (0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b).cbrt();
    [
        0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s,
        1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s,
        0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s,
        c[3],
    ]
}

/// `xyzd50_to_hcl` and `lin_srgb_to_okhcl` after the conversion to Lab or
/// Oklab: the hue in degrees, from 0 to under 360, the chroma and the
/// lightness, the hue powerless at a chroma of `powerless` or less.
fn hcl(lab: [f32; 4], powerless: f32) -> ([f32; 4], bool) {
    let hue = lab[2].atan2(lab[1]) * DEGREES_PER_RADIAN;
    let chroma = (lab[1] * lab[1] + lab[2] * lab[2]).sqrt();
    let hue = match hue >= 0.0 {
        true => hue,
        false => hue + TURN,
    };
    ([hue, chroma, lab[0], lab[3]], chroma <= powerless)
}

/// `srgb_to_hsl`: the hue in degrees, the saturation and the lightness in
/// percent, the hue powerless where the saturation is 0.
fn hsl(c: [f32; 4]) -> ([f32; 4], bool) {
    let [r, g, b] = [c[0], c[1], c[2]];
    let mx = r.max(g).max(b);
    let mn = r.min(g).min(b);
    let light = (mn + mx) / 2.0;
    let d = mx - mn;
    let (mut hue, mut sat) = (0.0_f32, 0.0_f32);
    if d != 0.0 {
        sat = match light == 0.0 || light == 1.0 {
            true => 0.0,
            false => (mx - light) / light.min(1.0 - light),
        };
        hue = if mx == r {
            (g - b) / d + if g < b { 6.0 } else { 0.0 }
        } else if mx == g {
            (b - r) / d + 2.0
        } else {
            (r - g) / d + 4.0
        };
        hue *= 60.0;
    }
    ([hue, sat * 100.0, light * 100.0, c[3]], sat == 0.0)
}

/// `sk_float_radians_to_degrees`'s factor, `180 / SK_FloatPI` in f32.
const DEGREES_PER_RADIAN: f32 = 180.0 / std::f32::consts::PI;

/// `sk_float_degrees_to_radians`'s factor, `SK_FloatPI / 180` in f32, which
/// `css_hcl_to_lab` turns a hue into radians by.
const RADIANS_PER_DEGREE: f32 = std::f32::consts::PI / 180.0;

/// `1 / (2 * SK_FloatPI)`, in f32, which `sin_` and `cos_` take an angle to
/// turns by.
const TURNS_PER_RADIAN: f32 = 1.0 / (2.0 * std::f32::consts::PI);

/// `sin5q_`'s coefficients, `A`, `B` and `C`: its polynomial for
/// `sin(x * 2 * pi)` on a quarter turn, from SwiftShader's
/// Sin-Cos-Optimization note, exact at 0, 1/12, 1/6 and 1/4.
#[allow(clippy::excessive_precision)]
const SIN5Q: [f32; 3] = [6.28230858, -41.1693687, 74.4388885];

/// `0.01f`, which `css_hsl_to_srgb_` and `css_hwb_to_srgb` take a percent
/// to a fraction by.
const HSL_PERCENT: f32 = 0.01;

/// The chroma at or below which `xyzd50_to_hcl` calls a hue powerless:
/// LCH's arithmetic leaves an achromatic colour a small chroma.
const LCH_POWERLESS: f32 = 1e-2;

/// The same for `lin_srgb_to_okhcl`, whose arithmetic leaves less.
const OKLCH_POWERLESS: f32 = 1e-6;

/// The D50 white `xyzd50_to_lab` and `css_lab_to_xyz` divide and multiply
/// by: CSS Color 4's `0.3457 / 0.3585`, 1, `(1 - 0.3457 - 0.3585) /
/// 0.3585`, each worked out in f32 as Skia's `constexpr float` is.
const LAB_D50: [f32; 3] =
    [0.3457 / 0.3585, 1.0, (1.0 - 0.3457 - 0.3585) / 0.3585];

/// CIE's epsilon, `216 / 24389`, below which Lab's curve is linear.
const LAB_E: f32 = 216.0 / 24389.0;

/// CIE's kappa, `24389 / 27`, the linear segment's slope.
const LAB_K: f32 = 24389.0 / 27.0;

/// `css_oklab_to_linear_srgb`'s first step, Oklab to cone responses
/// (`l_ = r + 0.3963377774f * g + 0.2158037573f * b` and its rows), one row
/// a response: the literals as Skia writes them, so that each is the f32 it
/// computes with.
#[allow(clippy::excessive_precision)]
const OKLAB_IN: [[f32; 3]; 3] = [
    [1.0, 0.3963377774, 0.2158037573],
    [1.0, -0.1055613458, -0.0638541728],
    [1.0, -0.0894841775, -1.2914855480],
];

/// Its last step, the cubed cone responses to linear sRGB, one row a
/// channel.
#[allow(clippy::excessive_precision)]
const OKLAB_OUT: [[f32; 3]; 3] = [
    [4.0767416621, -3.3077115913, 0.2309699292],
    [-1.2684380046, 2.6097574011, -0.3413193965],
    [-0.0041960863, -0.7034186147, 1.7076147010],
];

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
    /// `RuntimeEffect` is neither `Send` nor `Sync` -- with where each
    /// value it takes lies.
    static EFFECTS: Option<Vec<(usize, RuntimeEffect, Offsets)>> = SIZES
        .iter()
        .map(|&size| {
            let effect = RuntimeEffect::make_for_shader(sksl(size), None).ok()?;
            let offsets = Offsets::of(&effect)?;
            Some((size, effect, offsets))
        })
        .collect();
}

/// Where each value the effect takes lies in its uniform data, in bytes:
/// found by its uniform's name and its place in it once, when the effect
/// is compiled, rather than for every draw.
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
    ties_even: usize,
    alpha8: usize,
    t_scale: usize,
    t_bias: usize,
    p0: usize,
    p1: usize,
    linearize: usize,
    gamut_step: usize,
    encode: usize,
    lin_a: usize,
    lin_b: usize,
    gam_r: usize,
    gam_g: usize,
    gam_b: usize,
    enc_a: usize,
    enc_b: usize,
    css: usize,
    lab_a: usize,
    lab_b: usize,
    ok_in: [usize; 3],
    ok_out: [usize; 3],
    matrix_form: usize,
    m_row0: usize,
    m_row1: usize,
    m_row2: usize,
    offset: usize,
    polar: usize,
    sinq: usize,
    hsl_k: usize,
    ts: usize,
    factors: usize,
    biases: usize,
}

impl Offsets {
    /// `None` where the effect lacks a uniform the source declares, or
    /// declares one too short for what is written to it.
    fn of(effect: &RuntimeEffect) -> Option<Self> {
        // The byte `within` bytes into the uniform `name`, which has to
        // hold the `len` bytes written there: a field's place in the
        // vector, matrix or array the effect packs it into.
        let at = |name: &str, within: usize, len: usize| {
            effect
                .uniforms()
                .iter()
                .find(|uniform| uniform.name() == name)
                .filter(|uniform| within + len <= uniform.size_in_bytes())
                .map(|uniform| uniform.offset() + within)
        };
        let scalar = |name: &str, k: usize| at(name, 4 * k, 4);
        // A `float3x3` keeps its columns 12 bytes apart and a `float4`
        // array its elements 16.
        let column = |name: &str, k: usize| at(name, 12 * k, 12);
        let element = |name: &str, k: usize, len: usize| at(name, 16 * k, len);
        Some(Offsets {
            size: effect.uniform_size(),
            kind: scalar("shapeFlags", 0)?,
            mask_degenerates: scalar("shapeFlags", 1)?,
            negate: scalar("shapeFlags", 2)?,
            compensate: scalar("shapeFlags", 3)?,
            unswap: scalar("rampFlags", 0)?,
            mode: scalar("rampFlags", 1)?,
            count: scalar("rampFlags", 2)?,
            matrix_form: scalar("rampFlags", 3)?,
            premul_after: scalar("storeFlags", 0)?,
            exact_scale: scalar("storeFlags", 1)?,
            ties_even: scalar("storeFlags", 2)?,
            css: scalar("storeFlags", 3)?,
            linearize: scalar("xformFlags", 0)?,
            gamut_step: scalar("xformFlags", 1)?,
            encode: scalar("xformFlags", 2)?,
            t_scale: scalar("tParams", 0)?,
            t_bias: scalar("tParams", 1)?,
            p0: scalar("tParams", 2)?,
            p1: scalar("tParams", 3)?,
            offset: at("place", 0, 8)?,
            alpha8: scalar("place", 2)?,
            lin_a: element("curves", 0, 16)?,
            lin_b: element("curves", 1, 12)?,
            enc_a: element("curves", 2, 16)?,
            enc_b: element("curves", 3, 12)?,
            gam_r: column("gamut", 0)?,
            gam_g: column("gamut", 1)?,
            gam_b: column("gamut", 2)?,
            lab_a: element("labK", 0, 16)?,
            lab_b: element("labK", 1, 16)?,
            ok_in: [column("okIn", 0)?, column("okIn", 1)?, column("okIn", 2)?],
            ok_out: [
                column("okOut", 0)?,
                column("okOut", 1)?,
                column("okOut", 2)?,
            ],
            m_row0: column("toUnit", 0)?,
            m_row1: column("toUnit", 1)?,
            m_row2: column("toUnit", 2)?,
            polar: element("hue", 0, 8)?,
            sinq: element("hue", 1, 12)?,
            hsl_k: element("hue", 2, 16)?,
            ts: at("ts", 0, 4)?,
            factors: at("factors", 0, 4)?,
            biases: at("biases", 0, 4)?,
        })
    }
}

/// The effect, its arrays `size` long.
fn sksl(size: usize) -> String {
    format!(
        r#"
// Ganesh keys a runtime effect's program and uploads its uniforms one
// uniform at a time on every draw, so the flags and scalars go four to a
// vector and each matrix whole: the effect's cost per draw follows how
// many uniforms it declares, not how many bytes.
//
// The CPU's matrix from the page to the gradient's space, a row in each
// column, and the whole-pixel offset to the page, `alpha8`, and none.
// They come first, as what differs most from one draw to the next: Ganesh
// compares a draw's uniforms with each recent draw's, bytes in order, to
// merge them, and two draws of the same gradient part only here.
uniform float3x3 toUnit;
uniform float4 place;
// `kind`, `maskDegenerates`, `negate`, `compensate`.
uniform int4 shapeFlags;
// `unswap`, `mode`, `count`, `matrixForm`.
uniform int4 rampFlags;
// `premulAfter`, `exactScale`, `tiesEven`, `css`.
uniform int4 storeFlags;
// `linearize`, `gamutStep`, `encode`, and none.
uniform int4 xformFlags;
// `tScale`, `tBias`, `p0`, `p1`.
uniform float4 tParams;
// The curve undone, `g, a, b, c` then `d, e, f`, and the curve applied.
uniform float4 curves[4];
// The gamut matrix, a row in each column.
uniform float3x3 gamut;
// `css_lab_to_xyz`'s constants.
uniform float4 labK[2];
// `css_oklab_to_linear_srgb`'s matrices, a row in each column.
uniform float3x3 okIn;
uniform float3x3 okOut;
// The hue's turn and radians, `sin5q_`'s coefficients, and the HSL
// constants.
uniform float4 hue[3];
uniform float ts[{size}];
uniform float4 factors[{size}];
uniform float4 biases[{size}];

int kind() {{ return shapeFlags.x; }}
int maskDegenerates() {{ return shapeFlags.y; }}
int negate() {{ return shapeFlags.z; }}
int compensate() {{ return shapeFlags.w; }}
int unswap() {{ return rampFlags.x; }}
int mode() {{ return rampFlags.y; }}
int count() {{ return rampFlags.z; }}
int matrixForm() {{ return rampFlags.w; }}
int premulAfter() {{ return storeFlags.x; }}
int exactScale() {{ return storeFlags.y; }}
int tiesEven() {{ return storeFlags.z; }}
int css() {{ return storeFlags.w; }}
int linearize() {{ return xformFlags.x; }}
int gamutStep() {{ return xformFlags.y; }}
int encode() {{ return xformFlags.z; }}
float tScale() {{ return tParams.x; }}
float tBias() {{ return tParams.y; }}
float p0() {{ return tParams.z; }}
float p1() {{ return tParams.w; }}
float alpha8() {{ return place.z; }}

// `div255_accurate`, and NEON's `div255`: the nearest whole level.
float4 nearest255(float4 v) {{
    return floor((v + 128.0 + floor((v + 128.0) / 256.0)) / 256.0);
}}

// `div255` off NEON: `(v + 255) >> 8`.
float4 above255(float4 v) {{
    return floor((v + 255.0) / 256.0);
}}

// The biased exponent and the mantissa of `x > 0`, as whole floats: what
// `approx_log2` reads from the float's bits, which a public effect cannot
// cast, worked out from exact powers of two instead.
float2 bitsOf(float x) {{
    if (x < 1.17549435e-38) {{
        return float2(0.0, (x * 8.507059173e37) * 8388608.0);
    }}
    float k = floor(log2(x));
    if (exp2(k) > x) {{ k -= 1.0; }} else if (exp2(k + 1.0) <= x) {{ k += 1.0; }}
    float q = k > 126.0 ? (x * 0.5) * exp2(-126.0) : x * exp2(-k);
    return float2(k + 127.0, (q - 1.0) * 8388608.0);
}}

// `approx_log2` in `SkRasterPipeline_opts.h`.
float approxLog2(float x) {{
    float2 b = bitsOf(x);
    float e = (b.x * 8388608.0 + b.y) * (1.0 / 8388608.0);
    float m = 0.5 + b.y * (1.0 / 16777216.0);
    float n = (e - 124.225514990) - m * 1.498030302;
    return n - 1.725879990 / (0.3520887068 + m);
}}

// `approx_pow2`: the float whose bits are the rounded value, built from its
// exponent and mantissa.
float approxPow2(float x) {{
    float f = x - floor(x);
    float a = (x + 121.274057500) - f * 1.490129070;
    a = (a + 27.728023300 / (4.84252568 - f)) * 8388608.0;
    a = min(max(a, 0.0), 2139095040.0);
    float lo = floor(a);
    float d = a - lo;
    float n = d > 0.5 ? lo + 1.0
            : (d < 0.5 ? lo : (mod(lo, 2.0) == 0.0 ? lo : lo + 1.0));
    float e = floor(n * (1.0 / 8388608.0));
    float m = n - e * 8388608.0;
    if (e == 0.0) {{ return (m * 1.17549435e-38) * (1.0 / 8388608.0); }}
    return exp2(e - 127.0) * (1.0 + m * (1.0 / 8388608.0));
}}

float approxPowf(float x, float y) {{
    return (x == 0.0 || x == 1.0) ? x : approxPow2(approxLog2(x) * y);
}}

// `parametric` and `gamma_`: the curve `g, a, b, c` and `d, e, f` of
// `|x|`, the sign put back.
float curve(float x, float4 gabc, float3 def) {{
    float v = abs(x);
    float r;
    if (gabc.y == 1.0 && gabc.z == 0.0 && gabc.w == 0.0
            && def.x == 0.0 && def.y == 0.0 && def.z == 0.0) {{
        r = approxPowf(v, gabc.x);
    }} else {{
        r = v <= def.x ? gabc.w * v + def.z
                       : approxPowf(gabc.y * v + gabc.z, gabc.x) + def.y;
    }}
    return x < 0.0 ? -r : r;
}}

// `css_lab_to_xyz` and `css_oklab_to_linear_srgb`: an interpolated colour
// back to the space the stops were converted from, in Skia's order of
// operations, its constants handed in as the f32 it computes them as.
float3 lab_to_xyz(float3 c) {{
    float f1 = (c.r + 16.0) * labK[0].x;
    float f0 = (c.g * labK[0].y) + f1;
    float f2 = f1 - (c.b * labK[0].z);
    float3 f = float3(f0, f1, f2);
    float3 cubed = f * f * f;
    float x = cubed.x > labK[1].x ? cubed.x : (116.0 * f0 - 16.0) * labK[0].w;
    float y = c.r > labK[1].y ? cubed.y : c.r * labK[0].w;
    float z = cubed.z > labK[1].x ? cubed.z : (116.0 * f2 - 16.0) * labK[0].w;
    return float3(x * labK[1].z, y, z * labK[1].w);
}}

float row(float3 m, float3 v) {{
    return (m.x * v.x + m.y * v.y) + m.z * v.z;
}}

float3 oklab_to_linear_srgb(float3 c) {{
    float3 lms = float3(row(okIn[0], c), row(okIn[1], c), row(okIn[2], c));
    lms = lms * lms * lms;
    return float3(row(okOut[0], lms), row(okOut[1], lms), row(okOut[2], lms));
}}

// `sin5q_`, Skia's polynomial for `sin(x * 2 * pi)` on a quarter turn.
float sin5q(float x) {{
    float x2 = x * x;
    return x * ((x2 * hue[1].z + hue[1].y) * x2 + hue[1].x);
}}

// `css_hcl_to_lab`, through Skia's `cos_` and `sin_`: hue, chroma and
// lightness to Lab or Oklab.
float3 hcl_to_lab(float3 c) {{
    float h = c.r * hue[0].x;
    float k = h * hue[0].y;
    k = 0.25 - abs(k - floor(k + 0.5));
    float s = h * -hue[0].y + 0.25;
    s = 0.25 - abs(s - floor(s + 0.5));
    return float3(c.b, c.g * sin5q(k), c.g * sin5q(s));
}}

// `mod_`: `x - y * floor(x * (1 / y))`, the reciprocal handed in.
float modOf(float x, float y, float inverse) {{
    return x - y * floor(x * inverse);
}}

// `css_hsl_to_srgb_`.
float3 hsl_to_srgb(float h, float s, float l) {{
    h = modOf(h, 360.0, hue[2].z);
    s *= hue[2].x;
    l *= hue[2].x;
    float3 k = float3(modOf(0.0 + h * hue[2].y, 12.0, hue[2].w),
                      modOf(8.0 + h * hue[2].y, 12.0, hue[2].w),
                      modOf(4.0 + h * hue[2].y, 12.0, hue[2].w));
    float a = s * min(l, 1.0 - l);
    return l - a * max(float3(-1.0), min(min(k - 3.0, 9.0 - k), float3(1.0)));
}}

// `css_hwb_to_srgb`.
float3 hwb_to_srgb(float3 c) {{
    float w = c.g * hue[2].x;
    float b = c.b * hue[2].x;
    float gray = w / (w + b);
    float3 rgb = hsl_to_srgb(c.r, 100.0, 50.0);
    rgb = rgb * (1.0 - w - b) + w;
    return (w + b) >= 1.0 ? float3(gray) : rgb;
}}

half4 main(float2 q) {{
    // The pixel's centre on the page, as `seed_shader` makes it, mapped by
    // the CPU's matrix as `appendMatrix`'s stage for it maps it.
    float2 p = (floor(q) + 0.5) + place.xy;
    float x = p.x;
    float y = p.y;
    if (matrixForm() == 1) {{
        x = p.x + toUnit[0].z;
        y = p.y + toUnit[1].z;
    }} else if (matrixForm() == 2) {{
        x = p.x * toUnit[0].x + toUnit[0].z;
        y = p.y * toUnit[1].y + toUnit[1].z;
    }} else if (matrixForm() == 3) {{
        x = p.x * toUnit[0].x + (p.y * toUnit[0].y + toUnit[0].z);
        y = p.x * toUnit[1].x + (p.y * toUnit[1].y + toUnit[1].z);
    }} else if (matrixForm() == 4) {{
        float r = 1.0 / (p.x * toUnit[2].x + (p.y * toUnit[2].y + toUnit[2].z));
        x = (p.x * toUnit[0].x + (p.y * toUnit[0].y + toUnit[0].z)) * r;
        y = (p.x * toUnit[1].x + (p.y * toUnit[1].y + toUnit[1].z)) * r;
    }}
    float t;
    bool masked = false;
    if (kind() == 0) {{
        t = x;
    }} else if (kind() == 1) {{
        t = sqrt(x * x + y * y);
    }} else if (kind() == 2) {{
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
    }} else if (kind() == 3) {{
        float q = p0() - y * y;
        masked = q < 0.0;
        t = x + sqrt(max(q, 0.0));
    }} else if (kind() == 4) {{
        masked = x == 0.0;
        t = x + y * y / x;
    }} else if (kind() == 5) {{
        t = sqrt(x * x + y * y) - x * p0();
    }} else {{
        float q = x * x - y * y;
        masked = q < 0.0;
        float root = sqrt(max(q, 0.0));
        t = (kind() == 6 ? root : -root) - x * p0();
    }}
    if (maskDegenerates() != 0 && t <= 0.0) {{
        masked = true;
    }}
    if (masked) {{
        return half4(0);
    }}
    if (negate() != 0) {{ t = -t; }}
    if (compensate() != 0) {{ t = t + p1(); }}
    if (unswap() != 0) {{ t = 1.0 - t; }}
    t = t * tScale() + tBias();

    float4 f = factors[0];
    float4 b = biases[0];
    if (mode() == 0) {{
        for (int i = 1; i < {size}; i++) {{
            if (i >= count() || t < ts[i]) {{ break; }}
            f = factors[i];
            b = biases[i];
        }}
    }} else {{
        t = clamp(t, 0.0, 1.0);
        if (mode() == 1) {{
            int index = int(t * float(count() - 1));
            for (int i = 1; i < {size}; i++) {{
                if (i > index) {{ break; }}
                f = factors[i];
                b = biases[i];
            }}
        }}
    }}
    float4 c = t * f + b;
    if (css() == 1) {{
        c.rgb = lab_to_xyz(c.rgb);
    }} else if (css() == 2) {{
        c.rgb = oklab_to_linear_srgb(c.rgb);
    }} else if (css() == 3) {{
        c.rgb = lab_to_xyz(hcl_to_lab(c.rgb));
    }} else if (css() == 4) {{
        c.rgb = oklab_to_linear_srgb(hcl_to_lab(c.rgb));
    }} else if (css() == 5) {{
        c.rgb = hsl_to_srgb(c.r, c.g, c.b);
    }} else if (css() == 6) {{
        c.rgb = hwb_to_srgb(c.rgb);
    }}
    // `SkColorSpaceXformSteps`, from the space interpolated in to the page.
    if (linearize() != 0) {{
        float4 gabc = curves[0];
        float3 def = curves[1].xyz;
        c.rgb = float3(curve(c.r, gabc, def), curve(c.g, gabc, def),
                       curve(c.b, gabc, def));
    }}
    if (gamutStep() != 0) {{
        c.rgb = float3(c.r * gamut[0].x + (c.g * gamut[0].y + c.b * gamut[0].z),
                       c.r * gamut[1].x + (c.g * gamut[1].y + c.b * gamut[1].z),
                       c.r * gamut[2].x + (c.g * gamut[2].y + c.b * gamut[2].z));
    }}
    if (encode() != 0) {{
        float4 gabc = curves[2];
        float3 def = curves[3].xyz;
        c.rgb = float3(curve(c.r, gabc, def), curve(c.g, gabc, def),
                       curve(c.b, gabc, def));
    }}
    c.rgb = clamp(c.rgb, 0.0, 1.0);
    float4 s = c * 255.0;
    float4 l = floor(s + 0.5);
    if (tiesEven() != 0) {{
        // `l - s` is at most a half, and a half only on a tie.
        l -= step(0.5, l - s) * (l - 2.0 * floor(l * 0.5));
    }}
    if (premulAfter() != 0) {{
        l.rgb = nearest255(float4(l.rgb * l.a, 0)).rgb;
    }}
    if (alpha8() < 255.0) {{
        l = exactScale() != 0 ? nearest255(l * alpha8())
                              : above255(l * alpha8());
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
    highp: bool,
) -> Vec<u8> {
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
    put(
        offsets.ties_even,
        i32::from(highp && HIGHP_TIES_TO_EVEN).to_ne_bytes(),
    );
    put(offsets.alpha8, alpha8.to_ne_bytes());
    put(offsets.t_scale, layout.t_scale.to_ne_bytes());
    put(offsets.t_bias, layout.t_bias.to_ne_bytes());
    put(offsets.p0, layout.p0.to_ne_bytes());
    put(offsets.p1, layout.p1.to_ne_bytes());
    let toward = &ramp.toward;
    put(
        offsets.linearize,
        i32::from(toward.linearize.is_some()).to_ne_bytes(),
    );
    put(
        offsets.gamut_step,
        i32::from(toward.gamut.is_some()).to_ne_bytes(),
    );
    put(
        offsets.encode,
        i32::from(toward.encode.is_some()).to_ne_bytes(),
    );
    put(offsets.css, ramp.css.index().to_ne_bytes());
    let mut floats = |at: usize, values: &[f32]| {
        for (k, v) in values.iter().enumerate() {
            put(at + 4 * k, v.to_ne_bytes());
        }
    };
    let curve = toward.linearize.unwrap_or(LINEAR_TF);
    floats(offsets.lin_a, &curve[..4]);
    floats(offsets.lin_b, &curve[4..]);
    let curve = toward.encode.unwrap_or(LINEAR_TF);
    floats(offsets.enc_a, &curve[..4]);
    floats(offsets.enc_b, &curve[4..]);
    floats(
        offsets.lab_a,
        &[1.0 / 116.0, 1.0 / 500.0, 1.0 / 200.0, 1.0 / LAB_K],
    );
    floats(
        offsets.lab_b,
        &[LAB_E, LAB_K * LAB_E, LAB_D50[0], LAB_D50[2]],
    );
    for (ats, rows) in
        [(offsets.ok_in, &OKLAB_IN), (offsets.ok_out, &OKLAB_OUT)]
    {
        for (at, row) in ats.iter().zip(rows) {
            floats(*at, row);
        }
    }
    floats(offsets.polar, &[RADIANS_PER_DEGREE, TURNS_PER_RADIAN]);
    floats(offsets.sinq, &SIN5Q);
    // `css_hsl_to_srgb_`'s `1 / 30.0f`, a hue in hours of a twelve-hour
    // wheel, and `mod_`'s `1 / y` for its 360 and its 12, each divided in
    // f32 as the CPU divides it.
    floats(
        offsets.hsl_k,
        &[HSL_PERCENT, 1.0 / 30.0, 1.0 / TURN, 1.0 / 12.0],
    );
    // `matrix_3x3`'s `m` is column by column; the effect takes a row of
    // the matrix each.
    let m = toward.gamut.unwrap_or([0.0; 9]);
    floats(offsets.gam_r, &[m[0], m[3], m[6]]);
    floats(offsets.gam_g, &[m[1], m[4], m[7]]);
    floats(offsets.gam_b, &[m[2], m[5], m[8]]);
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
    data
}

/// Whether the CPU's `div255` rounds to the nearest level, as NEON
/// computes it, or is `(v + 255) >> 8`, as every other backend of
/// `SkRasterPipeline_opts.h` computes it.
const EXACT_SCALE: bool = cfg!(target_arch = "aarch64");

/// Whether the CPU's `highp` store rounds a half level to the even one, as
/// NEON's `vcvtnq` and x86's `cvtps` do, or up, as `round` in
/// `SkRasterPipeline_opts.h` does on 32-bit ARM, LoongArch and the scalar
/// backend. Its `lowp` pipeline rounds a half up everywhere.
const HIGHP_TIES_TO_EVEN: bool =
    cfg!(any(target_arch = "aarch64", target_arch = "x86_64"));

#[cfg(all(test, any(feature = "metal", feature = "vulkan")))]
mod tests;
