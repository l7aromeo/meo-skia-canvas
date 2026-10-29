//! A fill or stroke its clip cuts, on the CPU engine: drawn as the unclipped
//! draw is wherever the clip lets it through, and as near the exact coverage
//! of the shape within the clip as the unclipped draw is to the shape.
//!
//! Skia's analytic anti-aliasing builds a path's edges against the clip
//! where the clip does not contain the path, and the edges it adds at the
//! cut re-slice every row they span: on the page as Skia draws it, the left
//! edge of a triangle cut 60 pixels to its right moves by up to 19 levels.

use meo_skia_canvas::prelude::*;

const W: f32 = 200.0;
const H: f32 = 120.0;

/// A clip, in canvas units.
#[derive(Clone, Copy, Debug)]
enum Clip {
    Rect(f32, f32, f32, f32),
    Circle(f32, f32, f32),
}

impl Clip {
    fn apply(self, ctx: &mut Context2D) {
        ctx.begin_path();
        match self {
            Clip::Rect(l, t, r, b) => ctx.rect(l, t, r - l, b - t),
            Clip::Circle(x, y, r) => {
                ctx.arc(x, y, r, 0.0, std::f32::consts::TAU, false)
                    .expect("an arc");
            }
        }
        ctx.clip(FillRule::NonZero);
    }

    /// The clip as a polygon, for the reference.
    fn polygon(self) -> Vec<(f64, f64)> {
        match self {
            Clip::Rect(l, t, r, b) => {
                let [l, t, r, b] = [l, t, r, b].map(f64::from);
                vec![(l, t), (r, t), (r, b), (l, b)]
            }
            Clip::Circle(x, y, r) => (0..8192)
                .map(|i| {
                    let a = f64::from(i) / 8192.0 * std::f64::consts::TAU;
                    (
                        f64::from(x) + f64::from(r) * a.cos(),
                        f64::from(y) + f64::from(r) * a.sin(),
                    )
                })
                .collect(),
        }
    }
}

/// The clips the pins run under: an integer cut through the shapes, a
/// half-pixel one, a rectangle with every edge between pixels, and a circle.
const CLIPS: [(&str, Clip); 4] = [
    ("integer cut", Clip::Rect(-10.0, -10.0, 82.0, 200.0)),
    ("half-pixel cut", Clip::Rect(-10.0, -10.0, 82.5, 200.0)),
    ("rectangle", Clip::Rect(30.3, 20.2, 150.7, 100.6)),
    ("circle", Clip::Circle(90.0, 60.0, 45.3)),
];

const TRIANGLE: [(f32, f32); 3] = [(20.0, 80.0), (80.0, 15.25), (140.5, 85.0)];

/// One draw: a name, and what it puts on the context.
type Draw = (&'static str, fn(&mut Context2D));

fn triangle(ctx: &mut Context2D) {
    ctx.begin_path();
    ctx.move_to(TRIANGLE[0].0, TRIANGLE[0].1);
    ctx.line_to(TRIANGLE[1].0, TRIANGLE[1].1);
    ctx.line_to(TRIANGLE[2].0, TRIANGLE[2].1);
    ctx.close_path();
    ctx.fill(FillRule::NonZero);
}

fn cubic(ctx: &mut Context2D) {
    ctx.begin_path();
    ctx.move_to(15.0, 95.0);
    ctx.bezier_curve_to(40.0, 5.0, 110.0, 30.0, 165.0, 12.0);
    ctx.line_to(165.0, 105.0);
    ctx.bezier_curve_to(120.0, 85.0, 60.0, 118.0, 15.0, 105.0);
    ctx.close_path();
    ctx.fill(FillRule::NonZero);
}

fn polyline(ctx: &mut Context2D) {
    ctx.begin_path();
    ctx.move_to(10.2, 100.4);
    ctx.line_to(70.7, 20.1);
    ctx.line_to(150.3, 90.9);
}

fn stroke(ctx: &mut Context2D) {
    ctx.set_line_width(3.3);
    polyline(ctx);
    ctx.stroke();
}

fn dashed(ctx: &mut Context2D) {
    ctx.set_line_width(2.7);
    ctx.set_line_dash(&[9.0, 4.0]);
    polyline(ctx);
    ctx.stroke();
}

fn marked(ctx: &mut Context2D) {
    let marker =
        Path2D::from_svg("M-1.5 -1.5 H1.5 V1.5 H-1.5 Z", FillRule::NonZero)
            .expect("a marker");
    ctx.set_line_width(2.0);
    ctx.set_line_dash(&[7.0, 3.0]);
    ctx.set_line_dash_marker(Some(&marker));
    polyline(ctx);
    ctx.stroke();
}

/// The triangle inside a layer at half alpha, which composites what it
/// holds as one.
fn layered(ctx: &mut Context2D) {
    ctx.save_layer_with(0.5, None, None);
    triangle(ctx);
    ctx.restore();
}

/// The triangle inside a layer inside another.
fn nested(ctx: &mut Context2D) {
    ctx.save_layer();
    ctx.save_layer_with(0.75, None, None);
    triangle(ctx);
    ctx.restore();
    ctx.restore();
}

const DRAWS: [Draw; 7] = [
    ("triangle", triangle),
    ("cubic", cubic),
    ("stroke", stroke),
    ("dashed stroke", dashed),
    ("marker stroke", marked),
    ("triangle in a layer", layered),
    ("triangle in nested layers", nested),
];

/// The alpha of `draw` in black on a CPU canvas, under `clip` where there
/// is one, read at `density`.
fn alpha(
    draw: fn(&mut Context2D),
    clip: Option<Clip>,
    density: u32,
) -> Vec<u8> {
    let mut canvas = Canvas::new(W, H);
    canvas.set_gpu(false);
    assert_eq!(canvas.engine_kind(), EngineKind::Cpu, "a CPU canvas");
    {
        let ctx = canvas.context();
        if let Some(clip) = clip {
            clip.apply(ctx);
        }
        ctx.set_fill_style(RgbaLinear::opaque(0.0, 0.0, 0.0));
        ctx.set_stroke_style(RgbaLinear::opaque(0.0, 0.0, 0.0));
        draw(ctx);
    }
    let export = ImageDataExport {
        density,
        ..ImageDataExport::default()
    };
    let data = canvas.to_image_data(None, &export).expect("pixels");
    data.pixels().chunks(4).map(|p| p[3]).collect()
}

/// Sutherland-Hodgman of `subject` against the convex `window`.
fn clip_by(subject: &[(f64, f64)], window: &[(f64, f64)]) -> Vec<(f64, f64)> {
    let n = window.len();
    let turn: f64 = (0..n)
        .map(|i| {
            let (a, b) = (window[i], window[(i + 1) % n]);
            a.0 * b.1 - b.0 * a.1
        })
        .sum();
    let mut out = subject.to_vec();
    for i in 0..n {
        if out.is_empty() {
            break;
        }
        let (a, b) = (window[i], window[(i + 1) % n]);
        let side = |p: (f64, f64)| {
            turn.signum()
                * ((b.0 - a.0) * (p.1 - a.1) - (b.1 - a.1) * (p.0 - a.0))
        };
        let input = std::mem::take(&mut out);
        for j in 0..input.len() {
            let (p, q) = (input[j], input[(j + 1) % input.len()]);
            let (sp, sq) = (side(p), side(q));
            if sp >= 0.0 {
                out.push(p);
            }
            if (sp >= 0.0) != (sq >= 0.0) {
                let t = sp / (sp - sq);
                out.push((p.0 + t * (q.0 - p.0), p.1 + t * (q.1 - p.1)));
            }
        }
    }
    out
}

fn area(p: &[(f64, f64)]) -> f64 {
    (0..p.len())
        .map(|i| {
            let (a, b) = (p[i], p[(i + 1) % p.len()]);
            a.0 * b.1 - b.0 * a.1
        })
        .sum::<f64>()
        .abs()
        / 2.0
}

/// The exact coverage, 0 to 255, of the triangle within `clip` at
/// `density`, per pixel.
fn exact(clip: Clip, density: u32) -> Vec<f64> {
    let s = f64::from(density);
    let scale = |p: &[(f64, f64)]| -> Vec<(f64, f64)> {
        p.iter().map(|&(x, y)| (x * s, y * s)).collect()
    };
    let triangle: Vec<(f64, f64)> = TRIANGLE
        .iter()
        .map(|&(x, y)| (f64::from(x), f64::from(y)))
        .collect();
    let shape = clip_by(&scale(&triangle), &scale(&clip.polygon()));
    let (w, h) = ((W * density as f32) as i32, (H * density as f32) as i32);
    let mut out = vec![0.0; (w * h) as usize];
    for y in 0..h {
        let (y0, y1) = (f64::from(y), f64::from(y) + 1.0);
        let band =
            clip_by(&shape, &[(-1e5, y0), (1e5, y0), (1e5, y1), (-1e5, y1)]);
        if band.is_empty() {
            continue;
        }
        for x in 0..w {
            let (x0, x1) = (f64::from(x), f64::from(x) + 1.0);
            let pixel = clip_by(
                &band,
                &[
                    (x0, y0 - 1.0),
                    (x1, y0 - 1.0),
                    (x1, y1 + 1.0),
                    (x0, y1 + 1.0),
                ],
            );
            out[(y * w + x) as usize] = area(&pixel) * 255.0;
        }
    }
    out
}

/// Whether pixel `i` of a `density` read lies more than two pixels inside
/// `clip`: every pixel within two of it is wholly covered by the clip.
fn deep_inside(clip: Clip, density: u32, i: usize) -> bool {
    let w = (W * density as f32) as i64;
    let (x, y) = (i as i64 % w, i as i64 / w);
    let s = density as f32;
    let inside = |px: i64, py: i64| -> bool {
        let corners = [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)];
        corners.iter().all(|(dx, dy)| {
            let (cx, cy) = ((px as f32 + dx) / s, (py as f32 + dy) / s);
            match clip {
                Clip::Rect(l, t, r, b) => {
                    cx >= l && cx <= r && cy >= t && cy <= b
                }
                Clip::Circle(ox, oy, r) => {
                    // A pixel corner well inside, against the polygon the
                    // circle is drawn as.
                    (cx - ox).hypot(cy - oy) <= r - 0.05
                }
            }
        })
    };
    (-2..=2).all(|dy| (-2..=2).all(|dx| inside(x + dx, y + dy)))
}

/// Inside every clip, more than two pixels from its edge, a cut fill or
/// stroke is the unclipped one byte for byte, at densities 1, 2 and 3: the
/// clip decides only where the draw is let through, not how its edges are
/// laid down. Under Skia's clipped rasterization the cut moved pixels far
/// from it.
#[test]
fn a_cut_draw_is_the_unclipped_draw_inside_the_clip() {
    let mut moved = Vec::new();
    for density in [1u32, 2, 3] {
        for (name, draw) in DRAWS {
            let free = alpha(draw, None, density);
            for (label, clip) in CLIPS {
                let cut = alpha(draw, Some(clip), density);
                let differs = (0..free.len())
                    .filter(|&i| {
                        deep_inside(clip, density, i) && free[i] != cut[i]
                    })
                    .count();
                if differs > 0 {
                    moved.push(format!(
                        "{name} under the {label} at x{density}: {differs}"
                    ));
                }
            }
        }
    }
    assert!(moved.is_empty(), "pixels moved inside the clip: {moved:#?}");
}

/// The triangle under each clip is as near the exact coverage of the
/// triangle within the clip as the unclipped triangle is to the triangle,
/// at each density: the mean error over the pixels either one leaves
/// partly covered, in levels. The bounds are this fix's measured errors
/// with a margin; Skia's clipped rasterization measured 4.85, 5.79 and 6.70
/// at density 1.
#[test]
fn a_cut_triangle_meets_its_exact_coverage_at_every_density() {
    let bounds: [(&str, [f64; 3]); 3] = [
        ("half-pixel cut", [1.6, 1.7, 1.5]),
        ("rectangle", [2.2, 2.5, 2.3]),
        ("circle", [3.6, 3.9, 5.5]),
    ];
    for (label, limits) in bounds {
        let clip = CLIPS.iter().find(|c| c.0 == label).expect("a clip").1;
        for (density, limit) in [1u32, 2, 3].into_iter().zip(limits) {
            let got = alpha(triangle, Some(clip), density);
            let want = exact(clip, density);
            let errors: Vec<f64> = got
                .iter()
                .zip(&want)
                .filter(|(g, w)| {
                    (**w > 0.5 && **w < 254.5)
                        || (f64::from(**g) - **w).abs() > 1.0
                })
                .map(|(g, w)| (f64::from(*g) - w).abs())
                .collect();
            assert!(
                errors.len() > 50,
                "{label} x{density}: {} edge pixels",
                errors.len()
            );
            let mean = errors.iter().sum::<f64>() / errors.len() as f64;
            assert!(
                mean <= limit,
                "{label} x{density}: mean {mean:.3} levels, over {limit}"
            );
        }
    }
}
