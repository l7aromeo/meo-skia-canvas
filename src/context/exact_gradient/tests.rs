use super::*;
use std::{f32::consts::TAU, io::Read, ops::Range, str, sync::Arc};

use crate::{
    canvas::{Canvas, CanvasOptions, EngineKind},
    color::{RgbaLinear, linear_srgb_color_space},
    context::{FILES_GRADIENTS, layer::Layer},
    context2d::Context2D,
    export::{EncodeOptions, ImageFormat, VectorFeatures},
    geometry::Point as CanvasPoint,
    gpu::RenderingEngine,
    paint::BlendMode,
    path::FillRule,
    pixels::{PixelColorSpace, PixelDepth},
    shader::{GradientStop, Shader as CanvasShader},
};

/// The side of the canvas each case is drawn on.
const SIDE: usize = 96;

/// How far, in device pixels, f32 arithmetic may misplace the point a pixel
/// samples: it is mapped through f32 matrices whose translations reach a
/// hundred or so, whose last bit is eight millionths of a pixel there, and
/// the effect maps it through two matrices where the CPU composes one.
const POINT_NOISE_PX: f64 = 1e-4;

/// How many units in the last place of its largest term the formula for `t`
/// may lose: a handful of operations, and a two-point conical gradient's
/// difference of two near-equal terms keeps the error of the larger.
const FORMULA_ULPS: f64 = 8.0;

/// How many of Skia's own GPU gradient's departures off their ties the
/// control counts before it stops judging them: enough to show the judge
/// sees them, without judging a million pixels.
const CONTROL_ENOUGH: usize = 1000;

/// The unit in the last place of 1 in f32.
const F32_EPSILON: f64 = f32::EPSILON as f64;

/// Every pixel of `canvas`, row by row.
fn pixels(canvas: &mut Canvas) -> Vec<u8> {
    let (width, height) = (canvas.width(), canvas.height());
    canvas
        .context()
        .get_image_data(0.0, 0.0, width, height)
        .expect("pixels")
        .pixels()
        .to_vec()
}

/// `shader` filling an eight-bit canvas in `page` at `alpha`, on the GPU or
/// the CPU. `None` where a GPU was asked for and there is none.
/// What a draw fills, each covering the page so that no pixel is an edge:
/// a rectangle on whole pixels, which Ganesh draws aliased; one whose edges
/// fall inside pixels, which it draws anti-aliased from outset geometry; a
/// triangle and a many-sided polygon, which it draws through its path
/// renderers. The effect drawn directly as each of the last three missed
/// the CPU's bytes off their ties -- see `GradientSpec::sampled`.
#[derive(Clone, Copy, Debug)]
enum Shape {
    Rect,
    OffPixels,
    Triangle,
    Polygon,
}

impl Shape {
    fn of(seed: u64) -> Self {
        [
            Shape::Rect,
            Shape::OffPixels,
            Shape::Triangle,
            Shape::Polygon,
        ][(seed % 4) as usize]
    }
}

/// How far past each edge of the page [`Shape::OffPixels`] reaches: a part
/// of a pixel, so that Ganesh draws its edges anti-aliased.
const OFF_PIXELS_PX: f32 = 0.37;

fn drawn(
    gpu: bool,
    page: PixelColorSpace,
    shader: &Shader,
    alpha: f32,
) -> Option<Vec<u8>> {
    drawn_as(gpu, page, shader, alpha, Shape::Rect, None)
}

/// `shader` filled over the page as `shape`, at `alpha`; drawn as the page
/// draws the gradient `spec` where it is given -- see `layer::GradientDraw`
/// -- and as a plain shader where not.
fn drawn_as(
    gpu: bool,
    page: PixelColorSpace,
    shader: &Shader,
    alpha: f32,
    shape: Shape,
    spec: Option<&GradientSpec>,
) -> Option<Vec<u8>> {
    let mut canvas = Canvas::with_options(
        SIDE as f32,
        SIDE as f32,
        CanvasOptions {
            color_space: page,
            color_type: PixelDepth::Uint8,
            gpu,
            ..Default::default()
        },
    )
    .expect("a canvas");
    if gpu && canvas.engine_kind() != EngineKind::Gpu {
        return None;
    }
    {
        let ctx = canvas.context();
        ctx.set_global_alpha(alpha);
        ctx.set_fill_shader(&CanvasShader {
            inner: shader.clone(),
            features: VectorFeatures::PLAIN,
            gradient: spec.cloned().map(Arc::new),
        });
        let side = SIDE as f32;
        match shape {
            Shape::Rect => ctx.fill_rect(0.0, 0.0, side, side),
            Shape::OffPixels => {
                let past = OFF_PIXELS_PX;
                ctx.fill_rect(
                    -past,
                    -past,
                    side + 2.0 * past,
                    side + 2.0 * past,
                );
            }
            Shape::Triangle => {
                ctx.begin_path();
                ctx.move_to(-side, -side);
                ctx.line_to(4.0 * side, -0.5 * side);
                ctx.line_to(-0.5 * side, 4.0 * side);
                ctx.close_path();
                ctx.fill(FillRule::NonZero);
            }
            Shape::Polygon => {
                ctx.begin_path();
                for k in 0..17 {
                    let turn = k as f32 / 17.0 * TAU;
                    let (x, y) =
                        (side * (0.5 + turn.cos()), side * (0.5 + turn.sin()));
                    match k {
                        0 => ctx.move_to(x, y),
                        _ => ctx.line_to(x, y),
                    }
                }
                ctx.close_path();
                ctx.fill(FillRule::NonZero);
            }
        }
    }
    Some(pixels(&mut canvas))
}

/// A small deterministic generator, so that a failing case can be drawn
/// again from its seed.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> f32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((self.0 >> 40) as f32) / (1u64 << 24) as f32
    }

    fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.next()
    }

    fn below(&mut self, n: usize) -> usize {
        ((self.next() * n as f32) as usize).min(n - 1)
    }
}

/// A gradient the corpus draws, made from `seed`, with the page it is drawn
/// on and the paint's alpha.
fn case(seed: u64) -> (GradientSpec, PixelColorSpace, f32) {
    let mut rng = Lcg(seed);
    let s = SIDE as f32;
    let point = |rng: &mut Lcg| {
        Point::new(rng.range(-20.0, s + 20.0), rng.range(-20.0, s + 20.0))
    };
    let geometry = match rng.below(5) {
        0 => Geometry::Linear {
            start: point(&mut rng),
            end: point(&mut rng),
        },
        1 => Geometry::Radial {
            center: point(&mut rng),
            radius: rng.range(5.0, 120.0),
        },
        2 => {
            let center = point(&mut rng);
            let (start, end) = match rng.below(3) {
                0 => (0.0, 360.0),
                _ => {
                    let a = rng.range(0.0, 300.0);
                    (a, a + rng.range(20.0, 360.0 - a))
                }
            };
            Geometry::Sweep { center, start, end }
        }
        3 => {
            // Concentric, with a start radius.
            let c = point(&mut rng);
            Geometry::Conical {
                start: c,
                start_radius: rng.range(2.0, 30.0),
                end: c,
                end_radius: rng.range(40.0, 120.0),
            }
        }
        _ => Geometry::Conical {
            start: point(&mut rng),
            start_radius: [0.0, rng.range(0.0, 40.0)][rng.below(2)],
            end: point(&mut rng),
            end_radius: rng.range(1.0, 90.0),
        },
    };
    let count = match rng.below(8) {
        0 => 2,
        1 => 30 + rng.below(220),
        _ => 2 + rng.below(30),
    };
    let translucent = rng.below(3) == 0;
    let positions: Vec<f32> = match rng.below(4) {
        // Evenly spaced, which Skia keeps as no positions at all; and
        // nearly so, which it treats the same.
        0 => (0..count).map(|i| i as f32 / (count - 1) as f32).collect(),
        1 => (0..count)
            .map(|i| {
                (i as f32 / (count - 1) as f32 + rng.range(-1e-4, 1e-4))
                    .clamp(0.0, 1.0)
            })
            .collect(),
        _ => {
            let mut positions: Vec<f32> =
                (0..count).map(|_| rng.next()).collect();
            positions.sort_by(f32::total_cmp);
            if rng.below(2) == 0 {
                positions[0] = 0.0;
                positions[count - 1] = 1.0;
            }
            // Hard stops: a position repeated.
            for i in 1..count {
                if rng.below(3) == 0 {
                    positions[i] = positions[i - 1];
                }
            }
            positions
        }
    };
    let colors: Vec<Color4f> = (0..count)
        .map(|_| {
            Color4f::new(
                rng.next(),
                rng.next(),
                rng.next(),
                if translucent {
                    rng.range(0.1, 1.0)
                } else {
                    1.0
                },
            )
        })
        .collect();
    let premul = rng.below(4) == 0;
    let page = [
        PixelColorSpace::Srgb,
        PixelColorSpace::Srgb,
        PixelColorSpace::DisplayP3,
    ][rng.below(3)];
    let color_space = match rng.below(3) {
        0 => linear_srgb_color_space(),
        _ => ColorSpace::new_srgb(),
    };
    let (space, named) = match rng.below(4) {
        0 => (GradientColorSpace::Srgb, InterpolationSpace::SRGB),
        _ => (
            GradientColorSpace::Destination,
            InterpolationSpace::Destination,
        ),
    };
    let local = matches!(geometry, Geometry::Sweep { .. }).then(|| {
        Matrix::rotate_deg_pivot(rng.range(0.0, 360.0), point(&mut rng))
    });
    let alpha = [1.0, 1.0, rng.range(0.2, 1.0)][rng.below(3)];
    let spec = GradientSpec {
        geometry,
        local,
        colors,
        positions,
        color_space,
        space,
        interpolation: Interpolation {
            in_premul: if premul { InPremul::Yes } else { InPremul::No },
            color_space: named,
            ..Interpolation::default()
        },
    };
    (spec, page, alpha)
}

/// How far `t` at the centre of pixel `index` would have to move to change
/// channel `channel`'s first rounding, and how far it lies from the nearest
/// threshold: the two places float noise can move a pixel's bytes. Taken in
/// f64 from the data the effect is given, so that it judges the effect's
/// arithmetic, not its inputs.
fn margins(
    spec: &GradientSpec,
    page: &ColorSpace,
    index: usize,
    channel: usize,
) -> Option<(f64, f64)> {
    margins_on(spec, page, index, SIDE, channel)
}

/// [`margins`] on a page `side` pixels wide.
fn margins_on(
    spec: &GradientSpec,
    page: &ColorSpace,
    index: usize,
    side: usize,
    channel: usize,
) -> Option<(f64, f64)> {
    let (dx, dy) = ((index % side) as f64 + 0.5, (index / side) as f64 + 0.5);
    let (to_half, to_stop) = margins_at(spec, page, dx, dy, channel)?;
    // The noise `t` can carry there: the point's, through `t`'s rate of
    // change across the page, and the formula's own.
    let step = 1e-3;
    let t = |x: f64, y: f64| t_at(spec, x, y);
    let gradient = ((t(dx + step, dy)? - t(dx - step, dy)?)
        .hypot(t(dx, dy + step)? - t(dx, dy - step)?))
        / (2.0 * step);
    let noise = gradient * POINT_NOISE_PX
        + FORMULA_ULPS * F32_EPSILON * formula_size(spec, dx, dy)?;
    Some((to_half / noise, to_stop / noise))
}

/// The size of the largest term `t`'s formula adds or subtracts at the
/// device point `(dx, dy)`, scaled as `t` is, which bounds the formula's
/// rounding error.
fn formula_size(spec: &GradientSpec, dx: f64, dy: f64) -> Option<f64> {
    let layout = Layout::of(spec)?;
    let local = match &spec.local {
        Some(local) => Matrix::concat(local, &layout.from_unit),
        None => layout.from_unit,
    };
    let to = local.invert()?;
    let x = f64::from(to.scale_x()) * dx
        + f64::from(to.skew_x()) * dy
        + f64::from(to.translate_x());
    let y = f64::from(to.skew_y()) * dx
        + f64::from(to.scale_y()) * dy
        + f64::from(to.translate_y());
    let p0 = f64::from(layout.p0);
    let size = match layout.kind {
        Kind::Linear => x.abs(),
        Kind::Radius => x.hypot(y),
        Kind::Angle => 1.0,
        Kind::Strip => x.abs() + p0.abs().sqrt() + y * y,
        Kind::FocalOnCircle => x.abs() + (y * y / x).abs(),
        Kind::WellBehaved => x.hypot(y) + (x * p0).abs(),
        Kind::Greater | Kind::Smaller => {
            (x * x + y * y)
                / (x * x - y * y).abs().sqrt().max(f64::MIN_POSITIVE)
                + (x * p0).abs()
        }
    } + f64::from(layout.p1).abs();
    Some(
        size * f64::from(layout.t_scale).abs() + f64::from(layout.t_bias).abs(),
    )
}

/// `t` at the device point `(dx, dy)`, in f64.
fn t_at(spec: &GradientSpec, dx: f64, dy: f64) -> Option<f64> {
    let layout = Layout::of(spec)?;
    let local = match &spec.local {
        Some(local) => Matrix::concat(local, &layout.from_unit),
        None => layout.from_unit,
    };
    let to = local.invert()?;
    let x = f64::from(to.scale_x()) * dx
        + f64::from(to.skew_x()) * dy
        + f64::from(to.translate_x());
    let y = f64::from(to.skew_y()) * dx
        + f64::from(to.scale_y()) * dy
        + f64::from(to.translate_y());
    Some(unit_t(&layout, x, y))
}

/// `t` at the point `(x, y)` of the gradient's own space, in f64.
fn unit_t(layout: &Layout, x: f64, y: f64) -> f64 {
    let (p0, p1) = (f64::from(layout.p0), f64::from(layout.p1));
    let mut t = match layout.kind {
        Kind::Linear => x,
        Kind::Radius => x.hypot(y),
        Kind::Angle => {
            let (xa, ya) = (x.abs(), y.abs());
            let slope = xa.min(ya) / xa.max(ya);
            let s = slope * slope;
            let mut phi = slope
                * (0.159_121_170_639_991_76
                    + s * (-5.185_396_969_318_39e-2
                        + s * (2.476_101_927_459_24e-2
                            + s * -7.054_738_234_728_575e-3)));
            if xa < ya {
                phi = 0.25 - phi;
            }
            if x < 0.0 {
                phi = 0.5 - phi;
            }
            if y < 0.0 {
                phi = 1.0 - phi;
            }
            phi
        }
        Kind::Strip => x + (p0 - y * y).sqrt(),
        Kind::FocalOnCircle => x + y * y / x,
        Kind::WellBehaved => x.hypot(y) - x * p0,
        Kind::Greater => (x * x - y * y).sqrt() - x * p0,
        Kind::Smaller => -(x * x - y * y).sqrt() - x * p0,
    };
    if layout.flags & Layout::NEGATE != 0 {
        t = -t;
    }
    if layout.flags & Layout::COMPENSATE != 0 {
        t += p1;
    }
    if layout.flags & Layout::UNSWAP != 0 {
        t = 1.0 - t;
    }
    t * f64::from(layout.t_scale) + f64::from(layout.t_bias)
}

/// [`margins`] in `t`.
fn margins_at(
    spec: &GradientSpec,
    page: &ColorSpace,
    dx: f64,
    dy: f64,
    channel: usize,
) -> Option<(f64, f64)> {
    let ramp = Ramp::of(spec, page)?;
    let mut t = t_at(spec, dx, dy)?;
    let to_stop = ramp.ts[1..]
        .iter()
        .map(|&s| (t - f64::from(s)).abs())
        .fold(f64::INFINITY, f64::min);
    let interval = match ramp.mode {
        Ramp::GENERAL => ramp.ts[1..]
            .iter()
            .take_while(|&&s| t >= f64::from(s))
            .count(),
        Ramp::EVENLY => {
            t = t.clamp(0.0, 1.0);
            (t * (ramp.ts.len() - 1) as f64) as usize
        }
        _ => {
            t = t.clamp(0.0, 1.0);
            0
        }
    };
    let (f, b) = (ramp.factors[interval], ramp.biases[interval]);
    let level = (t * f64::from(f[channel]) + f64::from(b[channel]))
        .clamp(0.0, 1.0)
        * 255.0;
    let slope = f64::from(f[channel]).abs() * 255.0;
    let to_half = match slope > 0.0 {
        true => (level - level.floor() - 0.5).abs() / slope,
        false => f64::INFINITY,
    };
    Some((to_half, to_stop))
}

/// Over a corpus of every kind of gradient, the effect on the GPU stores the
/// bytes the CPU stores for Skia's own gradient, except on ties: where the
/// two part, the value lies within float noise of a half level or `t` of a
/// stop. Skia's own GPU gradient is counted beside it, which is what this
/// replaces.
#[test]
fn the_effect_stores_the_cpus_bytes_but_on_ties() {
    let (mut cases, mut covered, mut parted, mut ganesh) = (0, 0, 0, 0);
    let mut largest: f64 = 0.0;
    let mut ganesh_off_ties = 0;
    let mut worst: Vec<String> = vec![];
    for seed in 0..600 {
        let (spec, page, alpha) = case(seed);
        let space = page.to_skia_color_space().expect("the page's space");
        let Some(native) = spec.native() else {
            continue;
        };
        let Some(cpu) = drawn(false, page, &native, alpha) else {
            continue;
        };
        if spec.exact(&space, alpha).is_none() {
            cases += 1;
            continue;
        };
        let Some(gpu) =
            drawn_as(true, page, &native, alpha, Shape::of(seed), Some(&spec))
        else {
            return;
        };
        // The control: Skia's own GPU gradient, judged the same way, has to
        // part from the CPU off its ties, or the judge cannot tell.
        if ganesh_off_ties < CONTROL_ENOUGH
            && let Some(theirs) = drawn(true, page, &native, alpha)
        {
            for (index, (a, b)) in
                cpu.chunks(4).zip(theirs.chunks(4)).enumerate()
            {
                if a == b {
                    continue;
                }
                ganesh += 1;
                let tie = (0..4).filter(|&k| a[k] != b[k]).all(|k| {
                    [k, 3].iter().any(|&c| {
                        margins(&spec, &space, index, c).is_some_and(
                            |(to_half, to_stop)| to_half.min(to_stop) <= 1.0,
                        )
                    })
                });
                if !tie {
                    ganesh_off_ties += 1;
                }
            }
        }
        cases += 1;
        covered += 1;
        for (index, (a, b)) in cpu.chunks(4).zip(gpu.chunks(4)).enumerate() {
            if a == b {
                continue;
            }
            parted += 1;
            // Read back unpremultiplied, so a premultiplied channel a level
            // apart reads several apart; and alpha takes part in each.
            let tie = (0..4).filter(|&k| a[k] != b[k]).all(|k| {
                let nearest = [k, 3]
                    .iter()
                    .map(|&c| {
                        let (to_half, to_stop) =
                            margins(&spec, &space, index, c).expect("margins");
                        to_half.min(to_stop)
                    })
                    .fold(f64::INFINITY, f64::min);
                largest = largest.max(nearest);
                nearest <= 1.0
            });
            if !tie {
                worst.push(format!(
                    "seed {seed} pixel {index}: cpu {a:?} gpu {b:?}, {:?}",
                    spec.geometry
                ));
            }
        }
    }
    eprintln!(
        "{covered} of {cases} drawable cases covered; {parted} pixels parted, all ties but {}, the furthest {largest:.2} of its noise from its tie; Skia's GPU gradient, judged until {CONTROL_ENOUGH} were found, parted {ganesh}, {ganesh_off_ties} off their ties",
        worst.len()
    );
    assert!(covered > 0, "no case was drawn through the effect");
    assert!(
        ganesh_off_ties > 0,
        "the judge found no fault in Skia's own GPU gradient"
    );
    assert!(
        worst.is_empty(),
        "{}",
        worst
            .iter()
            .take(20)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// Every size of the effect compiles; a refusal here would leave every
/// gradient to the CPU route in silence.
#[test]
fn the_effect_compiles_at_every_size() {
    for size in SIZES {
        if let Err(error) = RuntimeEffect::make_for_shader(sksl(size), None) {
            panic!("size {size}: {error}");
        }
    }
}

/// Each step a covered gradient takes answers something.
#[test]
fn a_plain_gradient_is_covered() {
    let (mut spec, _, _) = case(1);
    spec.color_space = ColorSpace::new_srgb();
    spec.space = GradientColorSpace::Destination;
    spec.interpolation.color_space = InterpolationSpace::Destination;
    let page = ColorSpace::new_srgb();
    assert!(Layout::of(&spec).is_some(), "layout {:?}", spec.geometry);
    assert!(stops(&spec).is_some(), "stops");
    assert!(intermediate_space(&spec, &page).is_some(), "intermediate");
    assert!(Ramp::of(&spec, &page).is_some(), "ramp");
    assert!(spec.exact(&page, 1.0).is_some(), "shader");
}

/// A pixel whose centre lies exactly on a hard stop takes the colour added
/// last, as the CPU's `gradient` stage counts `t >= stop` and as Chrome's
/// CPU canvas draws it, and the effect stores the CPU's bytes there with
/// no tie to excuse a difference: `t` is exact in f32 on both.
#[test]
fn a_pixel_on_a_stop_takes_the_colour_added_last() {
    let (red, blue) = (
        Color4f::new(1.0, 0.0, 0.0, 1.0),
        Color4f::new(0.0, 0.0, 1.0, 1.0),
    );
    let side = SIDE as f32;
    // Pixel 31's centre is at 31.5, a stop at 31.5 / side of the way along
    // a gradient the canvas's width; pixel 10 of row 48 lies at 180 degrees
    // from a centre on that row's centre line, a sweep's half turn; and
    // pixel 31 of row 0 lies 31.5 from a radial gradient's centre.
    let on = 31.5 / side;
    let cases = [
        (
            Geometry::Linear {
                start: Point::new(0.0, 0.0),
                end: Point::new(side, 0.0),
            },
            on,
            31,
        ),
        (
            Geometry::Sweep {
                center: Point::new(48.0, 48.5),
                start: 0.0,
                end: 360.0,
            },
            0.5,
            48 * SIDE + 10,
        ),
        (
            Geometry::Radial {
                center: Point::new(0.0, 0.5),
                radius: side,
            },
            on,
            31,
        ),
    ];
    for (geometry, stop, pixel) in cases {
        let spec = GradientSpec {
            geometry: geometry.clone(),
            local: None,
            colors: vec![red, red, blue, blue],
            positions: vec![0.0, stop, stop, 1.0],
            color_space: ColorSpace::new_srgb(),
            space: GradientColorSpace::Destination,
            interpolation: Interpolation::default(),
        };
        let page = PixelColorSpace::Srgb;
        let cpu = drawn(false, page, &spec.native().expect("native"), 1.0)
            .expect("cpu");
        let at = |bytes: &[u8]| bytes[pixel * 4..pixel * 4 + 4].to_vec();
        assert_eq!(
            at(&cpu),
            vec![0, 0, 255, 255],
            "{geometry:?}: the CPU takes the colour added last"
        );
        let exact = spec
            .exact(&page.to_skia_color_space().expect("the page's space"), 1.0)
            .expect("an effect");
        let Some(gpu) = drawn(true, page, &exact, 1.0) else {
            return;
        };
        assert_eq!(at(&gpu), at(&cpu), "{geometry:?}");
    }
}

/// A page of draws of every corpus gradient, under a clip, a transform, an
/// alpha and a blend, drawn on the CPU and written to an SVG and a PDF.
fn exported(
    page: PixelColorSpace,
    seeds: Range<u64>,
    flat_first: bool,
) -> [Vec<u8>; 3] {
    let mut canvas = Canvas::with_options(
        SIDE as f32,
        SIDE as f32,
        CanvasOptions {
            color_space: page,
            color_type: PixelDepth::Uint8,
            gpu: false,
            ..Default::default()
        },
    )
    .expect("a canvas");
    {
        let ctx = canvas.context();
        ctx.set_fill_style_css("rgb(20 40 200)").expect("a colour");
        ctx.fill_rect(0.0, 0.0, SIDE as f32, SIDE as f32);
        for seed in seeds {
            let (spec, _, alpha) = case(seed);
            let Some(native) = spec.native() else {
                continue;
            };
            let shader = CanvasShader {
                inner: native,
                features: VectorFeatures::PLAIN,
                gradient: Some(Arc::new(spec)),
            };
            ctx.save();
            ctx.set_global_alpha(alpha);
            let blends = [
                BlendMode::SourceOver,
                BlendMode::Multiply,
                BlendMode::ColorDodge,
                BlendMode::DestinationIn,
            ];
            ctx.set_global_composite_operation(blends[(seed % 4) as usize]);
            if seed % 3 == 0 {
                ctx.begin_path();
                ctx.arc(48.0, 48.0, 20.0 + (seed % 20) as f32, 0.0, TAU, false)
                    .expect("a positive radius");
                ctx.clip(FillRule::NonZero);
            }
            if seed % 5 == 0 {
                ctx.rotate(0.3);
            }
            if seed % 11 == 0 {
                ctx.save_layer_with(0.7, None, None);
            }
            if seed % 13 == 0 {
                ctx.set_shadow_color_css("rgb(0 0 0 / 0.5)")
                    .expect("a colour");
                ctx.set_shadow_blur(3.0);
                ctx.set_shadow_offset(3.0, 2.0);
            }
            let at = (seed % 7) as f32 * 7.0;
            let flat = |ctx: &mut Context2D| {
                ctx.set_fill_style_css("rgb(200 120 40 / 0.8)")
                    .expect("a colour");
                ctx.fill_rect(at + 5.0, at + 3.0, 25.0, 25.0);
            };
            if flat_first && seed % 2 == 1 {
                flat(ctx);
            }
            ctx.set_fill_shader(&shader);
            ctx.set_stroke_shader(&shader);
            ctx.fill_rect(at, at, 40.0, 30.0);
            ctx.set_line_width(4.0);
            // A dash makes the drawn path an outline Skia fills, whose
            // bounds the page takes from the outline rather than the path.
            if seed % 7 == 3 {
                ctx.set_line_dash(&[5.0, 3.0]);
            }
            ctx.stroke_rect(at + 10.0, at + 20.0, 30.0, 20.0);
            if seed % 17 == 0 {
                ctx.set_font_css("bold 30px sans-serif");
                ctx.fill_text("Wg", at, at + 30.0, None);
            }
            if !flat_first && seed % 2 == 1 {
                flat(ctx);
            }
            if seed % 11 == 0 {
                ctx.restore();
            }
            ctx.restore();
        }
    }
    let raw = pixels(&mut canvas);
    let svg = canvas
        .to_buffer(ImageFormat::Svg, &EncodeOptions::default())
        .expect("an SVG");
    let pdf = canvas
        .to_buffer(ImageFormat::Pdf, &EncodeOptions::default())
        .expect("a PDF");
    [raw, svg, pdf]
}

/// What a CPU canvas stores, and what an SVG and a PDF write, for a page of
/// gradients is what they were without `GradientDraw`: a CPU raster and a
/// document play the draw as it was recorded.
#[test]
fn a_cpu_canvas_and_a_document_draw_gradients_as_they_did() {
    for page in [PixelColorSpace::Srgb, PixelColorSpace::DisplayP3] {
        for seeds in [0..40, 40..80, 80..120] {
            FILES_GRADIENTS.set(false);
            let before = exported(page, seeds.clone(), false);
            FILES_GRADIENTS.set(true);
            let after = exported(page, seeds.clone(), false);
            for (name, (a, b)) in ["pixels", "SVG", "PDF"]
                .iter()
                .zip(before.iter().zip(&after))
            {
                assert!(a == b, "{page:?}, seeds {seeds:?}: the {name} moved");
            }
        }
    }
}

/// Scenes drawn as a page draws them, on a GPU canvas and a CPU one, part
/// only on ties: every byte apart, inside what the draw inks whole, lies
/// within float noise of a half level or a stop of the gradient's value
/// before the paint's alpha, which the CPU then scales in whole levels --
/// the judge of [`the_effect_stores_the_cpus_bytes_but_on_ties`]. Inside
/// means a pixel an opaque white draw of the same scene inks whole, with its
/// eight neighbours, on both canvases: the edges are the GPU's coverage, as
/// for a flat colour.
#[test]
fn drawn_scenes_part_only_on_ties() {
    const SIDE_PX: usize = 64;
    let side = SIDE_PX as f32;
    let stop = |position: f32, (r, g, b): (u8, u8, u8)| GradientStop {
        position,
        color: RgbaLinear::from_srgb8(r, g, b, 1.0),
    };
    // The issue's conic: a period of 45 degrees from 292.5, a hard stop at
    // each wrap.
    let conic = {
        let mut stops = vec![];
        for k in -7..=1 {
            for (t, colour) in [
                (0.0, (47, 79, 79)),
                (1.0 / 3.0, (255, 0, 0)),
                (2.0 / 3.0, (0, 0, 255)),
                (1.0, (250, 240, 230)),
            ] {
                let at = (292.5 + (k as f32 + t) * 45.0) / 360.0;
                if (0.0..=1.0).contains(&at) {
                    stops.push(stop(at, colour));
                }
            }
        }
        CanvasShader::sweep_gradient(
            CanvasPoint::new(28.0, 21.2),
            0.0,
            360.0,
            &stops,
            GradientColorSpace::Destination,
        )
        .expect("a conic")
    };
    let sweep = {
        let mut stops: Vec<GradientStop> = (0..6)
            .flat_map(|k| {
                let k = k as f32;
                [
                    stop(k / 6.0, (250, 200, 0)),
                    stop((k + 0.5) / 6.0, (0, 60, 200)),
                ]
            })
            .collect();
        stops.push(stop(1.0, (250, 200, 0)));
        CanvasShader::sweep_gradient(
            CanvasPoint::new(30.25, 33.75),
            0.0,
            360.0,
            &stops,
            GradientColorSpace::Destination,
        )
        .expect("a sweep")
    };
    let clip = |ctx: &mut Context2D| {
        ctx.begin_path();
        ctx.arc(32.0, 32.0, 22.0, 0.0, TAU, false)
            .expect("a positive radius");
        ctx.clip(FillRule::NonZero);
    };
    // Each scene draws its gradient, or opaque white where it is not given.
    type Scene<'a> = (
        &'a str,
        &'a CanvasShader,
        &'a dyn Fn(&mut Context2D, Option<&CanvasShader>),
        bool,
    );
    let fill = |ctx: &mut Context2D, dye: Option<&CanvasShader>| match dye {
        Some(shader) => ctx.set_fill_shader(shader),
        None => ctx.set_fill_style_css("white").expect("a colour"),
    };
    let scenes: [Scene; 5] = [
        (
            "the conic",
            &conic,
            &|ctx, dye| {
                fill(ctx, dye);
                ctx.fill_rect(0.0, 0.0, side, side);
            },
            false,
        ),
        (
            "a stroke at a global alpha",
            &sweep,
            &|ctx, dye| {
                if dye.is_some() {
                    ctx.set_global_alpha(0.6);
                }
                ctx.set_line_width(10.0);
                match dye {
                    Some(shader) => ctx.set_stroke_shader(shader),
                    None => {
                        ctx.set_stroke_style_css("white").expect("a colour")
                    }
                }
                ctx.stroke_rect(10.0, 10.0, 40.0, 40.0);
            },
            false,
        ),
        (
            "a copy",
            &sweep,
            &|ctx, dye| {
                ctx.set_fill_style_css("rgb(20 40 200)").expect("a colour");
                ctx.fill_rect(0.0, 0.0, side, side);
                ctx.set_global_composite_operation(BlendMode::Copy);
                fill(ctx, dye);
                ctx.fill_rect(10.0, 10.0, 40.0, 40.0);
            },
            false,
        ),
        (
            "a fill under a clip",
            &sweep,
            &|ctx, dye| {
                clip(ctx);
                fill(ctx, dye);
                ctx.fill_rect(0.0, 0.0, side, side);
            },
            false,
        ),
        (
            "drawn on the CPU and switched",
            &sweep,
            &|ctx, dye| {
                fill(ctx, dye);
                ctx.fill_rect(0.0, 0.0, side, side);
            },
            true,
        ),
    ];
    let page = PixelColorSpace::Srgb;
    let space = page.to_skia_color_space().expect("the page's space");
    let drawn = |gpu: bool,
                 switched: bool,
                 scene: &dyn Fn(&mut Context2D, Option<&CanvasShader>),
                 dye: Option<&CanvasShader>| {
        let mut canvas = Canvas::with_options(
            side,
            side,
            CanvasOptions {
                color_space: page,
                color_type: PixelDepth::Uint8,
                gpu: gpu && !switched,
                ..Default::default()
            },
        )
        .expect("a canvas");
        scene(canvas.context(), dye);
        if switched {
            canvas.set_gpu(gpu);
        }
        (canvas.engine_kind() == EngineKind::Gpu, pixels(&mut canvas))
    };
    let mut judged = 0;
    for (name, shader, scene, switched) in scenes {
        let spec = shader
            .gradient
            .as_deref()
            .expect("a gradient's description");
        let (on_gpu, gpu) = drawn(true, switched, scene, Some(shader));
        if !on_gpu {
            return;
        }
        let (_, cpu) = drawn(false, switched, scene, Some(shader));
        let (_, gpu_white) = drawn(true, switched, scene, None);
        let (_, cpu_white) = drawn(false, switched, scene, None);
        let inked = |x: isize, y: isize| {
            (0..SIDE_PX as isize).contains(&x)
                && (0..SIDE_PX as isize).contains(&y)
                && [&gpu_white, &cpu_white].iter().all(|white| {
                    white[(y as usize * SIDE_PX + x as usize) * 4 + 3] == 255
                })
        };
        let mut inside = 0;
        for (index, (a, b)) in cpu.chunks(4).zip(gpu.chunks(4)).enumerate() {
            let (x, y) =
                ((index % SIDE_PX) as isize, (index / SIDE_PX) as isize);
            if !(-1..=1).all(|dy| (-1..=1).all(|dx| inked(x + dx, y + dy))) {
                continue;
            }
            inside += 1;
            if a == b {
                continue;
            }
            judged += 1;
            let tie = (0..4).filter(|&k| a[k] != b[k]).all(|k| {
                [k, 3].iter().any(|&c| {
                    margins_on(spec, &space, index, SIDE_PX, c).is_some_and(
                        |(to_half, to_stop)| to_half.min(to_stop) <= 1.0,
                    )
                })
            });
            assert!(tie, "{name}, pixel ({x}, {y}): cpu {a:?} gpu {b:?}");
        }
        assert!(inside > 0, "{name}: no pixel inside to judge");
        eprintln!("{name}: {inside} pixels inside");
    }
    eprintln!("{judged} pixels apart inside the scenes, each a tie");
}

/// A GPU canvas animating gradients does not keep the images it drew them
/// from: each draw's image -- see `GradientSpec::sampled` -- is held only
/// by the page that drew it, and Ganesh frees its texture with it. Over 100
/// frames of 20 gradient fills each, cleared between, the context's cache
/// holds no more after the last frame than halfway: it grows while Ganesh
/// fills its pools, from 48 resources after the first frame to 88 by the
/// twenty-first, and stays there.
#[test]
fn an_animation_of_gradients_keeps_the_gpu_cache_bounded() {
    const FRAMES: usize = 100;
    const SETTLED: usize = FRAMES / 2;
    let (spec, _, _) = case(1);
    let shader = CanvasShader {
        inner: spec.native().expect("native"),
        features: VectorFeatures::PLAIN,
        gradient: Some(Arc::new(spec)),
    };
    let mut canvas = Canvas::new(SIDE as f32, SIDE as f32);
    canvas.set_gpu(true);
    if canvas.engine_kind() != EngineKind::Gpu {
        return;
    }
    let usage = || {
        let mut usage = None;
        RenderingEngine::GPU.with_direct_context(|context| {
            usage = context.map(|context| {
                let cache = context.resource_cache_usage();
                (cache.resource_count, cache.resource_bytes)
            });
        });
        usage.expect("a GPU canvas has a context on its thread")
    };
    let mut seen = vec![];
    for frame in 0..FRAMES {
        let ctx = canvas.context();
        ctx.clear_rect(0.0, 0.0, SIDE as f32, SIDE as f32);
        ctx.set_fill_shader(&shader);
        for k in 0..20 {
            let at = ((frame + 7 * k) % SIDE) as f32;
            ctx.fill_rect(at * 0.5, (k * 4) as f32, 30.0, 12.0);
        }
        pixels(&mut canvas);
        seen.push(usage());
    }
    let (count, bytes) = seen[SETTLED - 1];
    let (last_count, last_bytes) = seen[FRAMES - 1];
    eprintln!(
        "cache after frame {SETTLED}: {count} resources, {bytes} bytes; after {FRAMES}: {last_count}, {last_bytes}"
    );
    assert!(
        last_count <= count && last_bytes <= bytes,
        "the cache grew: {:?}",
        seen.iter().step_by(10).collect::<Vec<_>>()
    );
}

/// The streams of `pdf`, each inflated where it is deflated, in the order
/// the file holds them.
fn pdf_streams(pdf: &[u8]) -> Vec<Vec<u8>> {
    const OBJECT: &[u8] = b" obj";
    const BEGIN: &[u8] = b"stream\n";
    const LENGTH: &[u8] = b"/Length ";
    const DEFLATED: &[u8] = b"/FlateDecode";
    let find = |within: &[u8], what: &[u8]| {
        within.windows(what.len()).position(|w| w == what)
    };
    let mut streams = vec![];
    let mut at = 0;
    while let Some(found) = find(&pdf[at..], OBJECT) {
        let object = at + found + OBJECT.len();
        let next =
            find(&pdf[object..], OBJECT).map_or(pdf.len(), |n| object + n);
        at = next;
        let Some(begin) = find(&pdf[object..next], BEGIN).map(|b| object + b)
        else {
            continue;
        };
        // The data is `/Length` bytes from the end of line after `stream`
        // (ISO 32000-1, 7.3.8.1); searching for `endstream` instead would
        // stop early at those bytes in compressed data.
        let dictionary = &pdf[object..begin];
        let length = find(dictionary, LENGTH)
            .map(|l| &dictionary[l + LENGTH.len()..])
            .and_then(|rest| {
                let digits =
                    rest.iter().take_while(|c| c.is_ascii_digit()).count();
                str::from_utf8(&rest[..digits]).ok()?.parse::<usize>().ok()
            })
            .expect("a stream's direct length");
        let start = begin + BEGIN.len();
        let raw = &pdf[start..start + length];
        streams.push(if find(dictionary, DEFLATED).is_some() {
            let mut inflated = vec![];
            flate2::read::ZlibDecoder::new(raw)
                .read_to_end(&mut inflated)
                .expect("a deflated stream inflates");
            inflated
        } else {
            raw.to_vec()
        });
    }
    streams
}

/// An isolated draw of something else and then one dyed by a gradient,
/// asking a vector backend for the same features under one clip, which the
/// page records into one isolated layer: the gradient is a `GradientDraw` of
/// its own, which holds the clip itself, so a PDF writes the clip a second
/// time for it -- see `PageRecorder::append_gradient`. Pinned as exactly
/// that: the same pixels and SVG, and a PDF whose page content gains one
/// `Q q <the clip> W n` and nothing else.
#[test]
fn a_gradient_after_an_isolated_draw_writes_the_clip_again() {
    // Seed 9 draws a linear gradient through `multiply` under a clip, after
    // a flat fill through the same blend when the flat draw comes first.
    let seeds = 9..10;
    FILES_GRADIENTS.set(false);
    let before = exported(PixelColorSpace::Srgb, seeds.clone(), true);
    FILES_GRADIENTS.set(true);
    let after = exported(PixelColorSpace::Srgb, seeds, true);
    assert!(before[0] == after[0], "the pixels moved");
    assert!(before[1] == after[1], "the SVG moved");
    let (was, is) = (pdf_streams(&before[2]), pdf_streams(&after[2]));
    assert_eq!(was.len(), is.len(), "a stream was added or lost");
    let moved: Vec<usize> =
        (0..was.len()).filter(|&i| was[i] != is[i]).collect();
    assert_eq!(moved.len(), 1, "streams {moved:?} moved");
    let lines = |stream: &[u8]| -> Vec<String> {
        String::from_utf8_lossy(stream)
            .lines()
            .map(str::to_owned)
            .collect()
    };
    let (was, is) = (lines(&was[moved[0]]), lines(&is[moved[0]]));
    let head = was.iter().zip(&is).take_while(|(a, b)| a == b).count();
    let tail = was[head..]
        .iter()
        .rev()
        .zip(is[head..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    assert_eq!(
        head + tail,
        was.len(),
        "lines of the PDF were lost or changed"
    );
    let added = &is[head..is.len() - tail];
    // What was added closes the group the clip was written in and writes it
    // again -- `Q`, `q`, the clip's path as it was first written, `W n` or
    // `W* n` -- and sets again the state the closed group had set, which a
    // PDF's `Q` forgets: lines written before, none of them a draw.
    let first_clip_end = was
        .iter()
        .position(|line| line.starts_with('W') && line.ends_with(" n"))
        .expect("the clip written once");
    let first_clip_start = was[..first_clip_end]
        .iter()
        .rposition(|line| line == "q")
        .expect("the clip's group")
        + 1;
    let first_clip = &was[first_clip_start..=first_clip_end];
    assert!(
        added.starts_with(&["Q".to_owned(), "q".to_owned()]),
        "the clip's group closed and opened: {added:?}"
    );
    assert_eq!(
        &added[2..2 + first_clip.len()],
        first_clip,
        "the clip written again, as it was"
    );
    for line in &added[2 + first_clip.len()..] {
        let draws = ["f", "f*", "S", "B", "B*", "Do"]
            .iter()
            .any(|op| line == op || line.ends_with(&format!(" {op}")));
        assert!(!draws, "a draw added: {line}");
        assert!(was[..head].contains(line), "state never set before: {line}");
    }
}

/// Gradient draws one after another under one frame are one entry, not one
/// each; a draw of anything else between them ends the run.
#[test]
fn consecutive_gradient_draws_share_an_entry() {
    let (spec, _, _) = case(1);
    let shader = CanvasShader {
        inner: spec.native().expect("native"),
        features: VectorFeatures::PLAIN,
        gradient: Some(Arc::new(spec)),
    };
    let mut canvas = Canvas::new(SIDE as f32, SIDE as f32);
    let ctx = canvas.context();
    ctx.set_fill_shader(&shader);
    for i in 0..3 {
        ctx.fill_rect(i as f32 * 10.0, 0.0, 8.0, 8.0);
    }
    ctx.set_fill_style_css("red").expect("a colour");
    ctx.fill_rect(0.0, 40.0, 8.0, 8.0);
    ctx.set_fill_shader(&shader);
    ctx.fill_rect(0.0, 60.0, 8.0, 8.0);
    let runs: Vec<usize> = ctx
        .inner
        .entries()
        .layers
        .iter()
        .filter_map(|entry| match entry {
            Layer::Gradient(op) => Some(op.draws.len()),
            _ => None,
        })
        .collect();
    assert_eq!(runs, vec![3, 1]);
}

/// The sweep of the integration tests, about `center` and turned by `turn`
/// degrees about it.
fn sweep_turned(center: CanvasPoint, turn: f32) -> CanvasShader {
    let stop = |position: f32, (r, g, b): (u8, u8, u8)| GradientStop {
        position,
        color: RgbaLinear::from_srgb8(r, g, b, 1.0),
    };
    let mut stops: Vec<GradientStop> = (0..6)
        .flat_map(|k| {
            let k = k as f32;
            [
                stop(k / 6.0, (250, 200, 0)),
                stop((k + 0.5) / 6.0, (0, 60, 200)),
            ]
        })
        .collect();
    stops.push(stop(1.0, (250, 200, 0)));
    let swept = CanvasShader::sweep_gradient(
        center,
        0.0,
        360.0,
        &stops,
        GradientColorSpace::Destination,
    )
    .expect("a sweep");
    let mut spec = swept.gradient.as_deref().expect("a description").clone();
    spec.local = Some(Matrix::rotate_deg_pivot(turn, (center.x, center.y)));
    CanvasShader {
        inner: spec.native().expect("native"),
        features: VectorFeatures::EXOTIC_SHADER,
        gradient: Some(Arc::new(spec)),
    }
}

/// `spec` as it lands on a page that draws it under `matrix`.
fn under(spec: &GradientSpec, matrix: &Matrix) -> GradientSpec {
    let mut moved = spec.clone();
    moved.local = Some(match &spec.local {
        Some(local) => Matrix::concat(matrix, local),
        None => *matrix,
    });
    moved
}

/// Judges every channel `gpu` parts from `cpu` in, among the pixels `cpu`
/// inks whole with all eight neighbours: each must lie within its float
/// noise of a half level or a stop of the gradient `spec_at` names for the
/// pixel -- the judge of [`the_effect_stores_the_cpus_bytes_but_on_ties`].
/// How many it judged.
fn assert_parts_only_on_ties(
    name: &str,
    cpu: &[u8],
    gpu: &[u8],
    side: usize,
    page: &ColorSpace,
    spec_at: &dyn Fn(usize, usize) -> GradientSpec,
) -> usize {
    let whole = |x: isize, y: isize| {
        (0..side as isize).contains(&x)
            && (0..side as isize).contains(&y)
            && cpu[(y as usize * side + x as usize) * 4 + 3] == 255
    };
    let mut judged = 0;
    for (index, (a, b)) in cpu.chunks(4).zip(gpu.chunks(4)).enumerate() {
        let (x, y) = ((index % side) as isize, (index / side) as isize);
        if a == b
            || !(-1..=1).all(|dy| (-1..=1).all(|dx| whole(x + dx, y + dy)))
        {
            continue;
        }
        let spec = spec_at(x as usize, y as usize);
        for k in (0..4).filter(|&k| a[k] != b[k]) {
            judged += 1;
            let tie = [k, 3].iter().any(|&c| {
                margins_on(&spec, page, index, side, c).is_some_and(
                    |(to_half, to_stop)| to_half.min(to_stop) <= 1.0,
                )
            });
            assert!(
                tie,
                "{name}, pixel ({x}, {y}), channel {k}: cpu {a:?} gpu {b:?}"
            );
        }
    }
    judged
}

/// A GPU canvas drawn with `drawCanvas` into another under a transform
/// other than a whole pixel's translation parts from the CPU only on ties:
/// the bound `tests/gpu_gradient.rs` allows there, proven.
#[test]
fn a_canvas_drawn_under_a_transform_parts_only_on_ties() {
    const SIDE_PX: usize = 128;
    let side = SIDE_PX as f32;
    let page = PixelColorSpace::Srgb.to_skia_color_space().expect("sRGB");
    let swept = sweep_turned(CanvasPoint { x: 30.25, y: 33.75 }, 0.0);
    let spec = swept.gradient.as_deref().expect("a description").clone();
    // Each applied through the context, and as the matrix it makes.
    type Transform<'a> = (&'a str, Matrix, &'a dyn Fn(&mut Context2D));
    let transforms: [Transform; 4] = [
        ("scaled by 2", Matrix::scale((2.0, 2.0)), &|ctx| {
            ctx.scale(2.0, 2.0)
        }),
        ("scaled by 0.5", Matrix::scale((0.5, 0.5)), &|ctx| {
            ctx.scale(0.5, 0.5)
        }),
        (
            "moved part of a pixel",
            Matrix::translate((0.5, 0.25)),
            &|ctx| ctx.translate(0.5, 0.25),
        ),
        (
            "rotated",
            Matrix::concat(
                &Matrix::translate((32.0, 0.0)),
                &Matrix::rotate_rad(0.3),
            ),
            &|ctx| {
                ctx.translate(32.0, 0.0);
                ctx.rotate(0.3);
            },
        ),
    ];
    let mut judged = 0;
    for (name, matrix, apply) in transforms {
        let into = |gpu: bool| {
            let mut source = Canvas::new(64.0, 64.0);
            source.set_gpu(true);
            let drawing = source.context();
            drawing.set_fill_shader(&swept);
            drawing.fill_rect(0.0, 0.0, 64.0, 64.0);
            let mut canvas = Canvas::new(side, side);
            canvas.set_gpu(gpu);
            if gpu && canvas.engine_kind() != EngineKind::Gpu {
                return None;
            }
            let ctx = canvas.context();
            apply(ctx);
            ctx.draw_canvas(&mut source, 0.0, 0.0);
            Some(pixels(&mut canvas))
        };
        let (Some(cpu), Some(gpu)) = (into(false), into(true)) else {
            return;
        };
        let moved = under(&spec, &matrix);
        judged += assert_parts_only_on_ties(
            name,
            &cpu,
            &gpu,
            SIDE_PX,
            &page,
            &|_, _| moved.clone(),
        );
    }
    eprintln!("{judged} channels apart under the transforms, each a tie");
}

/// A run of many gradient draws, which takes the effect's images from one
/// atlas, parts from the CPU only on ties: the bound `tests/gpu_gradient.rs`
/// allows there, proven, on sweeps turned so that ties occur.
#[test]
fn a_run_from_one_atlas_parts_only_on_ties() {
    const SIDE_PX: usize = 256;
    const CELL_PX: f32 = 12.8;
    let side = SIDE_PX as f32;
    let page = PixelColorSpace::Srgb.to_skia_color_space().expect("sRGB");
    let one = sweep_turned(CanvasPoint { x: 30.25, y: 33.75 }, 40.1);
    let other = sweep_turned(CanvasPoint { x: 190.5, y: 70.25 }, 40.1);
    let drawn = |gpu: bool| {
        let mut canvas = Canvas::new(side, side);
        canvas.set_gpu(gpu);
        if gpu && canvas.engine_kind() != EngineKind::Gpu {
            return None;
        }
        let ctx = canvas.context();
        for k in 0..400 {
            let (x, y) = ((k % 20) as f32 * CELL_PX, (k / 20) as f32 * CELL_PX);
            ctx.set_fill_shader(if k % 2 == 0 { &one } else { &other });
            ctx.begin_path();
            ctx.move_to(x + 0.3, y + 0.6);
            ctx.line_to(x + 11.4, y + 1.1);
            ctx.line_to(x + 2.2, y + 11.7);
            ctx.fill(FillRule::NonZero);
        }
        Some(pixels(&mut canvas))
    };
    let (Some(cpu), Some(gpu)) = (drawn(false), drawn(true)) else {
        return;
    };
    let specs = [&one, &other].map(|shader| {
        shader.gradient.as_deref().expect("a description").clone()
    });
    let judged = assert_parts_only_on_ties(
        "the run",
        &cpu,
        &gpu,
        SIDE_PX,
        &page,
        &|x, y| {
            let k = (y as f32 / CELL_PX) as usize * 20
                + (x as f32 / CELL_PX) as usize;
            specs[k % 2].clone()
        },
    );
    eprintln!("{judged} channels apart in the run, each a tie");
}

/// Gradient runs one after another -- here each draw a run of its own,
/// a sweep and a linear gradient asking a vector backend for different
/// things -- take their effects from one atlas between them: each draw
/// must still sample its own slot, and parts from the CPU only on ties.
#[test]
fn consecutive_runs_from_one_atlas_part_only_on_ties() {
    const SIDE_PX: usize = 256;
    const CELL_PX: f32 = 12.8;
    let side = SIDE_PX as f32;
    let page = PixelColorSpace::Srgb.to_skia_color_space().expect("sRGB");
    let swept = sweep_turned(CanvasPoint { x: 30.25, y: 33.75 }, 40.1);
    let stop = |position: f32, (r, g, b): (u8, u8, u8)| GradientStop {
        position,
        color: RgbaLinear::from_srgb8(r, g, b, 1.0),
    };
    let linear = CanvasShader::linear_gradient(
        CanvasPoint { x: 3.3, y: 0.0 },
        CanvasPoint { x: 251.7, y: 37.1 },
        &[
            stop(0.0, (250, 200, 0)),
            stop(0.5, (0, 60, 200)),
            stop(1.0, (200, 30, 90)),
        ],
        GradientColorSpace::Destination,
    )
    .expect("a linear gradient");
    let drawn = |gpu: bool| {
        let mut canvas = Canvas::new(side, side);
        canvas.set_gpu(gpu);
        if gpu && canvas.engine_kind() != EngineKind::Gpu {
            return None;
        }
        let ctx = canvas.context();
        for k in 0..400 {
            let (x, y) = ((k % 20) as f32 * CELL_PX, (k / 20) as f32 * CELL_PX);
            ctx.set_fill_shader(if k % 2 == 0 { &swept } else { &linear });
            ctx.begin_path();
            ctx.move_to(x + 0.3, y + 0.6);
            ctx.line_to(x + 11.4, y + 1.1);
            ctx.line_to(x + 2.2, y + 11.7);
            ctx.fill(FillRule::NonZero);
        }
        let runs = ctx
            .inner
            .entries()
            .layers
            .iter()
            .filter(|entry| matches!(entry, Layer::Gradient(_)))
            .count();
        Some((pixels(&mut canvas), runs))
    };
    let (Some((cpu, runs)), Some((gpu, _))) = (drawn(false), drawn(true))
    else {
        return;
    };
    assert!(runs > 1, "{runs} runs: the draws must be runs apart");
    let specs = [&swept, &linear].map(|shader| {
        shader.gradient.as_deref().expect("a description").clone()
    });
    let judged = assert_parts_only_on_ties(
        "the runs",
        &cpu,
        &gpu,
        SIDE_PX,
        &page,
        &|x, y| {
            let k = (y as f32 / CELL_PX) as usize * 20
                + (x as f32 / CELL_PX) as usize;
            specs[k % 2].clone()
        },
    );
    eprintln!("{runs} runs; {judged} channels apart, each a tie");
}
