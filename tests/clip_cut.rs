//! A fill or stroke its clip cuts, on the CPU engine: its coverage the
//! unclipped draw's wherever the clip lets it through, and as near the exact
//! coverage of the shape within the clip as the unclipped draw is to the
//! shape -- where the page's edge does not also cut it, which moves pixels as
//! a clip does. The coverage pins draw opaque black on transparent, so that
//! what is compared is coverage alone: a colour, or content beneath, is
//! blended by another route under the clip on an eight-bit sRGB page, and
//! the blend pins hold how far that lands.
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
    framed(draw, clip, density, 0.0)
}

/// As `alpha`, on a canvas `margin` units larger on every side and
/// translated by it, read back over the `W` by `H` page alone: the draw
/// as it is where the page's edge does not cut it.
fn framed(
    draw: fn(&mut Context2D),
    clip: Option<Clip>,
    density: u32,
    margin: f32,
) -> Vec<u8> {
    let mut canvas = Canvas::new(W + 2.0 * margin, H + 2.0 * margin);
    canvas.set_gpu(false);
    assert_eq!(canvas.engine_kind(), EngineKind::Cpu, "a CPU canvas");
    {
        let ctx = canvas.context();
        ctx.translate(margin, margin);
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
    // The whole canvas is read and cropped here: a read of the page's
    // rectangle alone could rasterize only that rectangle, and cut the draw
    // at its edge again.
    let data = canvas.to_image_data(None, &export).expect("pixels");
    let stride = data.width() as usize;
    let (inset, w, h) = (
        (margin * density as f32) as usize,
        (W * density as f32) as usize,
        (H * density as f32) as usize,
    );
    (0..h)
        .flat_map(|y| (0..w).map(move |x| (y + inset) * stride + x + inset))
        .map(|i| data.pixels()[i * 4 + 3])
        .collect()
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
/// stroke covers what the unclipped one does byte for byte, in a layer or
/// not, at densities 1, 2 and 3: the
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

/// Room past the page on every side of the larger canvas the page-edge pin
/// draws on: more than the 30 units `past_the_edge` reaches past the page.
const MARGIN: f32 = 60.0;

/// How far into the page, in canvas units, a moved pixel shows the page's
/// edge reaching past its own pixels.
const FAR: usize = 20;

/// A triangle the page's left edge cuts.
fn past_the_edge(ctx: &mut Context2D) {
    ctx.begin_path();
    ctx.move_to(-30.0, 80.0);
    ctx.line_to(60.0, 15.25);
    ctx.line_to(140.5, 95.0);
    ctx.close_path();
    ctx.fill(FillRule::NonZero);
}

/// The page's own edge cuts a path as a clip does, and nothing here routes
/// it: the triangle crossing the left edge differs, far inside the page,
/// from the same triangle drawn on a larger canvas, and so does the cut
/// draw under a clip at x 82, inside the clip, where the cut alone leaves
/// the unclipped draw's pixels. Skia rasterizes against the device bounds
/// as against any clip that does not contain the path.
///
/// This holds a defect and goes red when Skia draws a path the page cuts as
/// it draws one the page contains.
#[test]
fn the_page_edge_moves_pixels_far_inside_the_page() {
    let clip = Clip::Rect(-100.0, -100.0, 82.0, 300.0);
    for density in [1u32, 2, 3] {
        let w = (W * density as f32) as usize;
        for (label, clip) in [("no clip", None), ("clip at x 82", Some(clip))] {
            let page = alpha(past_the_edge, clip, density);
            let whole = framed(past_the_edge, clip, density, MARGIN);
            let far = (0..page.len())
                .filter(|&i| {
                    clip.is_none_or(|c| deep_inside(c, density, i))
                        && page[i] != whole[i]
                })
                .map(|i| i % w)
                .max();
            let far = far.unwrap_or(0) / density as usize;
            assert!(
                far > FAR,
                "{label} x{density}: the page edge moved nothing past {far} px",
            );
        }
    }
}

/// A fill a blend pin draws, in CSS, over what lies beneath it.
struct Blend {
    name: &'static str,
    beneath: Option<&'static str>,
    fill: &'static str,
}

/// A coloured fill, a translucent one, and a translucent one over
/// translucent content, which differs at full coverage as well as at the
/// triangle's edges.
const BLENDS: [Blend; 3] = [
    Blend {
        name: "coloured",
        beneath: None,
        fill: "rgb(200 40 90)",
    },
    Blend {
        name: "translucent",
        beneath: None,
        fill: "rgb(200 40 90 / 0.5)",
    },
    Blend {
        name: "translucent over translucent",
        beneath: Some("rgb(200 40 90 / 0.6)"),
        fill: "rgb(0 0 0 / 0.33)",
    },
];

/// The triangle filled as `blend` says on a CPU canvas in `space` at
/// `depth`, under `clip` where there is one, read at `density`: every
/// channel premultiplied, in levels of an eight-bit channel.
fn levels(
    blend: &Blend,
    (space, depth): (PixelColorSpace, PixelDepth),
    clip: Option<Clip>,
    density: u32,
) -> Vec<f32> {
    let mut canvas = Canvas::with_options(
        W,
        H,
        CanvasOptions {
            color_space: space,
            color_type: depth,
            gpu: false,
            ..CanvasOptions::default()
        },
    )
    .expect("a canvas");
    assert_eq!(canvas.engine_kind(), EngineKind::Cpu, "a CPU canvas");
    {
        let ctx = canvas.context();
        if let Some(beneath) = blend.beneath {
            ctx.set_fill_style_css(beneath).expect("a colour");
            ctx.fill_rect(0.0, 0.0, W, H);
        }
        if let Some(clip) = clip {
            clip.apply(ctx);
        }
        ctx.set_fill_style_css(blend.fill).expect("a colour");
        triangle(ctx);
    }
    let export = ImageDataExport {
        density,
        pixels: PixelExportOptions {
            depth: PixelDepth::F32,
            color_space: space,
            premultiplied: true,
            ..PixelExportOptions::default()
        },
        ..ImageDataExport::default()
    };
    let data = canvas.to_image_data(None, &export).expect("pixels");
    data.pixels()
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b) * 255.0)
        .collect()
}

/// An eight-bit canvas in sRGB, the one page whose unclipped flat-colour
/// fill Skia blends through its legacy blitter.
const SRGB_8: (PixelColorSpace, PixelDepth) =
    (PixelColorSpace::Srgb, PixelDepth::Uint8);

/// Pages whose unclipped fill Skia already blends through its raster
/// pipeline: a space other than sRGB, and a float depth.
const PIPELINE_PAGES: [(&str, (PixelColorSpace, PixelDepth)); 2] = [
    (
        "Display P3 at eight bits",
        (PixelColorSpace::DisplayP3, PixelDepth::Uint8),
    ),
    ("sRGB at F16", (PixelColorSpace::Srgb, PixelDepth::F16)),
];

/// Inside every clip, more than two pixels from its edge, a cut fill in a
/// colour, translucent, or over translucent content is blended within one
/// premultiplied level of the unclipped fill, at densities 1, 2 and 3.
///
/// The clip shader takes the draw off Skia's legacy blitter, which an
/// unclipped flat colour on an eight-bit sRGB page is blended by, onto its
/// raster pipeline (`SkBlitter::Choose`, which takes the pipeline whenever
/// there is a clip shader), and the two round differently. So on that page
/// some channel differs; on a page whose unclipped fill takes the pipeline
/// already, nothing does.
#[test]
fn a_cut_draw_blends_within_a_level_of_the_unclipped_draw() {
    for blend in &BLENDS {
        for density in [1u32, 2, 3] {
            let free = levels(blend, SRGB_8, None, density);
            let mut differs = 0;
            for (label, clip) in CLIPS {
                let cut = levels(blend, SRGB_8, Some(clip), density);
                let channels = (0..free.len())
                    .filter(|&c| deep_inside(clip, density, c / 4));
                let worst = channels
                    .clone()
                    .map(|c| (free[c] - cut[c]).abs())
                    .fold(0.0f32, f32::max);
                assert!(
                    worst <= 1.0 + 1e-3,
                    "{} under the {label} x{density}: a channel {worst} \
                     levels from the unclipped fill",
                    blend.name,
                );
                differs += channels.filter(|&c| free[c] != cut[c]).count();
            }
            assert!(
                differs > 0,
                "{} x{density}: the cut fill blended as the unclipped one on \
                 an eight-bit sRGB page",
                blend.name,
            );
            for (page, kind) in PIPELINE_PAGES {
                let free = levels(blend, kind, None, density);
                for (label, clip) in CLIPS {
                    let cut = levels(blend, kind, Some(clip), density);
                    let differs = (0..free.len())
                        .filter(|&c| {
                            deep_inside(clip, density, c / 4)
                                && free[c] != cut[c]
                        })
                        .count();
                    assert_eq!(
                        differs, 0,
                        "{} on {page} under the {label} x{density}: channels \
                         differing from the unclipped fill",
                        blend.name,
                    );
                }
            }
        }
    }
}

/// Whether pixel `i` of a `density` read lies more than two pixels inside
/// the triangle, which it covers whole.
fn inside_the_triangle(density: u32, i: usize) -> bool {
    let w = (W * density as f32) as usize;
    let s = f64::from(density);
    let p = (((i % w) as f64 + 0.5) / s, ((i / w) as f64 + 0.5) / s);
    (0..3).all(|k| {
        let (a, b) = (TRIANGLE[k], TRIANGLE[(k + 1) % 3]);
        let (a, b) = (
            (f64::from(a.0), f64::from(a.1)),
            (f64::from(b.0), f64::from(b.1)),
        );
        let cross = (b.0 - a.0) * (p.1 - a.1) - (b.1 - a.1) * (p.0 - a.0);
        cross / (b.0 - a.0).hypot(b.1 - a.1) > 2.0 / s
    })
}

/// The raster pipeline a cut fill is blended by runs at low precision: it
/// holds the paint's colour at eight bits. 0.3 black over `rgb(254 254
/// 254)` comes to 254 x 0.7 = 177.8, so 178, in float; at the eight-bit
/// alpha 77 it is 254 x 178 / 255 = 177.3, so 177, which is what both the
/// cut and the unclipped fill lay down.
#[test]
fn the_cut_blend_holds_the_colour_at_eight_bits() {
    let blend = Blend {
        name: "0.3 black over near white",
        beneath: Some("rgb(254 254 254)"),
        fill: "rgb(0 0 0 / 0.3)",
    };
    let clip = CLIPS[3].1;
    let free = levels(&blend, SRGB_8, None, 1);
    let cut = levels(&blend, SRGB_8, Some(clip), 1);
    let pixels: Vec<usize> = (0..free.len() / 4)
        .filter(|&i| deep_inside(clip, 1, i) && inside_the_triangle(1, i))
        .collect();
    assert!(
        !pixels.is_empty(),
        "pixels inside the triangle and the clip"
    );
    for i in pixels {
        assert_eq!(&free[i * 4..i * 4 + 3], [177.0; 3], "unclipped, pixel {i}");
        assert_eq!(&cut[i * 4..i * 4 + 3], [177.0; 3], "cut, pixel {i}");
    }
}

/// On ARM the cut fill is the nearer of the two to exact source-over, over
/// the triangle's interior, of 0.33 black -- 84 as the eight-bit alpha Skia
/// holds -- over `rgba(200 40 90 / 0.6)`: alpha 187 where the unclipped
/// fill lays down 186 and exact source-over is 186.6, a mean error of 0.294
/// levels a premultiplied channel against 0.344.
///
/// The pipeline's divide by 255 rounds exactly with NEON, and elsewhere is
/// `(v + 255) / 256`, as much as a level high (`div255` in
/// `SkRasterPipeline_opts.h`), so off ARM which of the two is nearer is not
/// established here: `a_cut_draw_blends_within_a_level_of_the_unclipped_draw`
/// bounds it there.
#[cfg(target_arch = "aarch64")]
#[test]
fn on_arm_the_cut_blend_is_nearer_exact_source_over() {
    let blend = &BLENDS[2];
    let under = Blend {
        name: "what lies beneath",
        beneath: blend.beneath,
        fill: "rgb(0 0 0 / 0)",
    };
    let clip = CLIPS[3].1;
    for density in [1u32, 2, 3] {
        let beneath = levels(&under, SRGB_8, None, density);
        let free = levels(blend, SRGB_8, None, density);
        let cut = levels(blend, SRGB_8, Some(clip), density);
        let pixels: Vec<usize> = (0..free.len() / 4)
            .filter(|&i| {
                deep_inside(clip, density, i) && inside_the_triangle(density, i)
            })
            .collect();
        let alpha = (0.33f64 * 255.0).round() / 255.0;
        let error = |drawn: &[f32]| {
            let total: f64 = pixels
                .iter()
                .flat_map(|&i| (0..4).map(move |c| i * 4 + c))
                .map(|c| {
                    let exact = f64::from(beneath[c]) * (1.0 - alpha)
                        + if c % 4 == 3 { alpha * 255.0 } else { 0.0 };
                    (f64::from(drawn[c]) - exact).abs()
                })
                .sum();
            total / (pixels.len() * 4) as f64
        };
        let (unclipped, routed) = (error(&free), error(&cut));
        assert!(
            (unclipped - 0.344).abs() < 5e-4 && (routed - 0.294).abs() < 5e-4,
            "x{density}: mean errors {unclipped:.4} unclipped and {routed:.4} \
             cut, over {} pixels",
            pixels.len(),
        );
    }
}
