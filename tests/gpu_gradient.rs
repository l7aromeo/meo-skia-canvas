//! A gradient on a GPU canvas stores the bytes the CPU canvas stores for it
//! (#667): drawn by an effect computing the CPU's arithmetic on the GPU, or,
//! where it cannot, by the CPU over the GPU's pixels. Ganesh's own gradient
//! moved a hard stop by up to a tenth of a degree or half a pixel and landed
//! a smooth ramp up to five levels away.
//!
//! Each test skips where there is no GPU, which is every CI runner, and says
//! so through `engine_kind` rather than assuming.

use meo_skia_canvas::prelude::*;

mod common;

fn pixels(canvas: &mut Canvas) -> Vec<u8> {
    canvas
        .to_buffer(ImageFormat::Raw, &EncodeOptions::default())
        .expect("raw export")
}

fn stop(position: f32, r: u8, g: u8, b: u8, alpha: f32) -> GradientStop {
    GradientStop {
        position,
        color: RgbaLinear::from_srgb8(r, g, b, alpha),
    }
}

/// A smooth sweep steep enough that Ganesh's half-precision angle shows.
fn sweep() -> Shader {
    sweep_at(Point { x: 30.25, y: 33.75 })
}

/// [`sweep`] about `center`.
fn sweep_at(center: Point) -> Shader {
    let mut stops: Vec<GradientStop> = (0..6)
        .flat_map(|k| {
            let k = k as f32;
            [
                stop(k / 6.0, 250, 200, 0, 1.0),
                stop((k + 0.5) / 6.0, 0, 60, 200, 1.0),
            ]
        })
        .collect();
    stops.push(stop(1.0, 250, 200, 0, 1.0));
    Shader::sweep_gradient(
        center,
        0.0,
        360.0,
        &stops,
        GradientColorSpace::Destination,
    )
    .expect("a sweep")
}

/// A translucent two-point conical gradient, which the CPU draws in floats.
fn radial(space: GradientColorSpace) -> Shader {
    let stops: Vec<GradientStop> = (0..12)
        .flat_map(|k| {
            let k = k as f32;
            [
                stop(k / 12.0, 200, 30, 90, 0.7),
                stop((k + 0.5) / 12.0, 0, 255, 0, 1.0),
            ]
        })
        .collect();
    Shader::two_point_conical_gradient(
        Point { x: 40.0, y: 50.0 },
        3.0,
        Point { x: 70.0, y: 60.0 },
        90.0,
        &stops,
        space,
    )
    .expect("a radial gradient")
}

/// Stripes whose 340 hard stops each lie 0.2 px past a pixel's centre: past
/// the 128 colours Ganesh keeps in uniforms, beyond which it holds their
/// offsets in half floats.
fn stripes() -> Shader {
    let (black, white) = ((0, 0, 0), (255, 255, 255));
    let mut stops = vec![];
    let mut flip = false;
    let mut j = 3.0;
    while j < 1020.0 {
        let at = (j + 0.5 + 0.2) / 1024.0;
        let (a, b) = if flip { (white, black) } else { (black, white) };
        stops.push(stop(at, a.0, a.1, a.2, 1.0));
        stops.push(stop(at, b.0, b.1, b.2, 1.0));
        flip = !flip;
        j += 3.0;
    }
    Shader::linear_gradient(
        Point { x: 0.0, y: 0.0 },
        Point { x: 1024.0, y: 0.0 },
        &stops,
        GradientColorSpace::Destination,
    )
    .expect("stripes")
}

/// `scene` drawn on a `width` by `height` canvas on the GPU or the CPU.
fn drawn(
    gpu: bool,
    width: f32,
    height: f32,
    scene: &dyn Fn(&mut Context2D),
) -> Vec<u8> {
    let mut canvas = Canvas::new(width, height);
    canvas.set_gpu(gpu);
    scene(canvas.context());
    pixels(&mut canvas)
}

fn assert_cpus_bytes(width: f32, height: f32, scene: &dyn Fn(&mut Context2D)) {
    let cpu = drawn(false, width, height, scene);
    let gpu = drawn(true, width, height, scene);
    let apart = cpu.iter().zip(&gpu).filter(|(a, b)| a != b).count();
    assert_eq!(apart, 0, "{apart} bytes apart from the CPU canvas's");
}

fn over_a_fill(ctx: &mut Context2D, side: f32) {
    ctx.set_fill_style_css("rgb(20 40 200)").expect("a colour");
    ctx.fill_rect(0.0, 0.0, side, side);
}

#[test]
fn a_smooth_sweep_stores_the_cpus_bytes() {
    if !common::has_gpu() {
        return;
    }
    assert_cpus_bytes(64.0, 64.0, &|ctx| {
        ctx.set_fill_shader(&sweep());
        ctx.fill_rect(0.0, 0.0, 64.0, 64.0);
    });
}

#[test]
fn hard_stops_past_128_colours_store_the_cpus_bytes() {
    if !common::has_gpu() {
        return;
    }
    assert_cpus_bytes(1024.0, 2.0, &|ctx| {
        ctx.set_fill_shader(&stripes());
        ctx.fill_rect(0.0, 0.0, 1024.0, 2.0);
    });
}

#[test]
fn a_translucent_two_point_conical_gradient_stores_the_cpus_bytes() {
    if !common::has_gpu() {
        return;
    }
    assert_cpus_bytes(96.0, 96.0, &|ctx| {
        over_a_fill(ctx, 96.0);
        ctx.set_fill_shader(&radial(GradientColorSpace::Destination));
        ctx.fill_rect(0.0, 0.0, 96.0, 96.0);
    });
}

#[test]
fn a_gradient_interpolated_in_oklab_stores_the_cpus_bytes() {
    if !common::has_gpu() {
        return;
    }
    assert_cpus_bytes(96.0, 96.0, &|ctx| {
        over_a_fill(ctx, 96.0);
        ctx.set_fill_shader(&radial(GradientColorSpace::Oklab));
        ctx.fill_rect(0.0, 0.0, 96.0, 96.0);
    });
}

/// How many pixels of `gpu` part from `cpu` among those `cpu` inks whole,
/// with all eight neighbours: the edge of a shape is not counted -- see
/// [`interior`].
fn interior_apart(cpu: &[u8], gpu: &[u8], side: usize) -> usize {
    assert_eq!(cpu.len(), gpu.len(), "the two reads are the same size");
    interior(cpu, side)
        .filter(|&i| cpu[i * 4..i * 4 + 4] != gpu[i * 4..i * 4 + 4])
        .count()
}

/// How many levels apart, at most, `gpu` is from `cpu` in any channel of
/// the pixels `cpu` inks whole, with all eight neighbours.
fn interior_most_apart(cpu: &[u8], gpu: &[u8], side: usize) -> u8 {
    assert_eq!(cpu.len(), gpu.len(), "the two reads are the same size");
    interior(cpu, side)
        .flat_map(|i| (i * 4..i * 4 + 4).map(|k| cpu[k].abs_diff(gpu[k])))
        .max()
        .unwrap_or(0)
}

/// The pixels of `cpu`, `side` pixels square, it inks whole, with all eight
/// neighbours. The edge of a path is held apart -- see [`edge_misses`] --
/// and the edge of a glyph is the GPU's own coverage, as it is for a flat
/// colour.
fn interior(cpu: &[u8], side: usize) -> impl Iterator<Item = usize> + '_ {
    let whole = move |x: isize, y: isize| {
        (0..side as isize).contains(&x)
            && (0..side as isize).contains(&y)
            && cpu[(y as usize * side + x as usize) * 4 + 3] == 255
    };
    (0..side * side).filter(move |&i| {
        let (x, y) = ((i % side) as isize, (i / side) as isize);
        (-1..=1).all(|dy| (-1..=1).all(|dx| whole(x + dx, y + dy)))
    })
}

/// A scene by name: what it draws on a context.
type Named<'a> = (&'a str, &'a dyn Fn(&mut Context2D));

fn clipped(ctx: &mut Context2D) {
    ctx.begin_path();
    ctx.arc(32.0, 32.0, 22.0, 0.0, std::f32::consts::TAU, false)
        .expect("a positive radius");
    ctx.clip(FillRule::NonZero);
}

fn triangle(ctx: &mut Context2D) {
    ctx.set_fill_shader(&sweep());
    ctx.begin_path();
    ctx.move_to(0.0, 0.0);
    ctx.line_to(64.0, 10.0);
    ctx.line_to(10.0, 64.0);
    ctx.fill(FillRule::NonZero);
}

/// Ganesh draws each of these from geometry it derives the shader's
/// coordinates for only nearly: a rectangle whose edges fall inside pixels,
/// a path, a fill and a stroke the clip cuts, and glyphs.
#[test]
fn every_shape_stores_the_cpus_bytes_inside_it() {
    if !common::has_gpu() {
        return;
    }
    let shapes: [Named; 5] = [
        ("a rectangle off whole pixels", &|ctx| {
            ctx.set_fill_shader(&sweep());
            ctx.fill_rect(3.37, 2.63, 57.5, 58.25);
        }),
        ("a triangle", &triangle),
        ("a triangle the clip cuts", &|ctx| {
            clipped(ctx);
            triangle(ctx);
        }),
        ("a stroke the clip cuts", &|ctx| {
            clipped(ctx);
            ctx.set_line_width(8.0);
            ctx.set_stroke_shader(&sweep());
            ctx.stroke_rect(10.0, 10.0, 40.0, 40.0);
        }),
        ("text", &|ctx| {
            assert!(ctx.set_font_css("bold 40px sans-serif"));
            ctx.set_fill_shader(&sweep());
            ctx.fill_text("Wg", 2.0, 48.0, None);
        }),
    ];
    for (name, scene) in shapes {
        let cpu = drawn(false, 64.0, 64.0, scene);
        let gpu = drawn(true, 64.0, 64.0, scene);
        assert_eq!(interior_apart(&cpu, &gpu, 64), 0, "{name}");
    }
}

/// `pixels`, read unpremultiplied, premultiplied and rounded: a level of a
/// translucent pixel reads unpremultiplied as more than one.
fn premultiplied(pixels: &[u8]) -> Vec<u8> {
    let (pixels, _) = pixels.as_chunks::<4>();
    pixels
        .iter()
        .flat_map(|p| {
            let alpha = f32::from(p[3]) / 255.0;
            let colour = |c: u8| (f32::from(c) * alpha).round() as u8;
            [colour(p[0]), colour(p[1]), colour(p[2]), p[3]]
        })
        .collect()
}

/// Text dyed by a gradient lands the CPU canvas's bytes at its glyphs'
/// edges too (#803), where Skia makes a glyph's coverage by the luminance
/// of the colour it is drawn in: a gradient's the mean of its stops, an
/// image's none, which Skia reads as mid grey. Drawn through an image of
/// the effect, the run took mid grey's edges, 24 levels off under `xor`.
#[test]
fn text_dyed_by_a_gradient_stores_the_cpus_bytes_at_its_edges() {
    if !common::has_gpu() {
        return;
    }
    let dye = Shader::linear_gradient(
        Point { x: 0.0, y: 0.0 },
        Point { x: 64.0, y: 0.0 },
        &[stop(0.0, 40, 90, 160, 1.0), stop(1.0, 160, 40, 90, 1.0)],
        GradientColorSpace::Destination,
    )
    .expect("a linear gradient");
    for blend in [BlendMode::SourceOver, BlendMode::Xor, BlendMode::Lighter] {
        let scene = |ctx: &mut Context2D| {
            ctx.set_fill_style_css("rgb(120 80 60)").expect("a colour");
            ctx.fill_rect(0.0, 0.0, 64.0, 64.0);
            assert!(ctx.set_font_css("bold 40px sans-serif"));
            ctx.set_fill_shader(&dye);
            ctx.set_global_composite_operation(blend);
            ctx.fill_text("Wg", 2.0, 48.0, None);
        };
        let cpu = premultiplied(&drawn(false, 64.0, 64.0, &scene));
        let gpu = premultiplied(&drawn(true, 64.0, 64.0, &scene));
        let most = cpu.iter().zip(&gpu).map(|(a, b)| a.abs_diff(*b)).max();
        assert!(most <= Some(1), "{blend:?}: {most:?} levels apart");
    }
}

#[test]
fn a_canvas_drawn_on_the_cpu_and_read_on_the_gpu_stores_the_cpus_bytes() {
    if !common::has_gpu() {
        return;
    }
    let scene = |ctx: &mut Context2D| {
        ctx.set_fill_shader(&sweep());
        ctx.fill_rect(0.0, 0.0, 64.0, 64.0);
    };
    let mut canvas = Canvas::new(64.0, 64.0);
    canvas.set_gpu(false);
    scene(canvas.context());
    canvas.set_gpu(true);
    let switched = pixels(&mut canvas);
    assert_eq!(switched, drawn(false, 64.0, 64.0, &scene));
}

#[test]
fn a_gpu_canvas_drawn_into_another_stores_the_cpus_bytes() {
    if !common::has_gpu() {
        return;
    }
    let scene = |ctx: &mut Context2D| {
        ctx.set_fill_shader(&sweep());
        ctx.fill_rect(0.0, 0.0, 64.0, 64.0);
    };
    let expected = drawn(false, 64.0, 64.0, &scene);
    for into_gpu in common::engines() {
        let mut source = Canvas::new(64.0, 64.0);
        source.set_gpu(true);
        scene(source.context());
        let mut canvas = Canvas::new(64.0, 64.0);
        canvas.set_gpu(into_gpu);
        canvas.context().draw_canvas(&mut source, 0.0, 0.0);
        assert_eq!(pixels(&mut canvas), expected, "into the GPU: {into_gpu}");
    }
}

/// A GPU canvas drawn into another under a transform other than a whole
/// pixel's translation: the image the effect is sampled from has the
/// source's device pixels for texels, so there the draw is made on the CPU
/// at the destination's pixels instead -- see `OnGpu::Path`.
#[test]
fn a_canvas_drawn_into_a_gpu_canvas_under_a_transform_stores_the_cpus_bytes_inside()
 {
    if !common::has_gpu() {
        return;
    }
    const SIDE_PX: usize = 128;
    let side = SIDE_PX as f32;
    let transforms: [Named; 4] = [
        ("scaled by 2", &|ctx| ctx.scale(2.0, 2.0)),
        ("scaled by 0.5", &|ctx| ctx.scale(0.5, 0.5)),
        ("moved part of a pixel", &|ctx| ctx.translate(0.5, 0.25)),
        ("rotated", &|ctx| {
            ctx.translate(32.0, 0.0);
            ctx.rotate(0.3);
        }),
    ];
    for (name, transform) in transforms {
        let into = |gpu: bool| {
            drawn(gpu, side, side, &|ctx| {
                let mut source = Canvas::new(64.0, 64.0);
                source.set_gpu(true);
                let drawing = source.context();
                drawing.set_fill_shader(&sweep());
                drawing.fill_rect(0.0, 0.0, 64.0, 64.0);
                transform(ctx);
                ctx.draw_canvas(&mut source, 0.0, 0.0);
            })
        };
        // The CPU raster is made at the destination's pixels from an origin
        // of its own, and the CPU canvas draws into the layer from another:
        // the two invert the transform with different translations, which
        // can move a value on a half level to the next. Nothing further.
        let most = interior_most_apart(&into(false), &into(true), SIDE_PX);
        assert!(most <= 1, "{name}: {most} levels apart");
        // Through another blend as well: the layer the source is drawn
        // through plays on a surface of its own, where the draw is made on
        // the CPU over the pixels it blends with, as on the page, rather than
        // over transparent and composited through the blend by the GPU,
        // which rounds twice. Bounded as the draw above, by the two origins.
        for blend in [BlendMode::Multiply, BlendMode::SourceAtop] {
            let blended = |gpu: bool| {
                drawn(gpu, side, side, &|ctx| {
                    let mut source = Canvas::new(64.0, 64.0);
                    source.set_gpu(true);
                    let drawing = source.context();
                    over_a_fill(drawing, 64.0);
                    drawing.set_global_composite_operation(blend);
                    drawing.set_fill_shader(&sweep());
                    drawing.fill_rect(0.0, 0.0, 64.0, 64.0);
                    transform(ctx);
                    ctx.draw_canvas(&mut source, 0.0, 0.0);
                })
            };
            let most =
                interior_most_apart(&blended(false), &blended(true), SIDE_PX);
            assert!(
                most <= 1,
                "{name}, through {blend:?}: {most} levels apart"
            );
        }
    }
}

/// A GPU canvas read at a density plays its page onto its own pixels under
/// that scale, outside any layer: the effect's image would be resampled, so
/// the draw is made on the CPU over the GPU's pixels read back -- see
/// `OnGpu::Effect`.
#[test]
fn a_gpu_canvas_read_at_a_density_stores_the_cpus_bytes_inside() {
    if !common::has_gpu() {
        return;
    }
    // Through a blend, or translucent, the draw made over transparent and
    // composited rounds twice: only the pixels read back give the CPU's
    // bytes.
    let multiplied = |ctx: &mut Context2D| {
        over_a_fill(ctx, 64.0);
        ctx.set_global_composite_operation(BlendMode::Multiply);
        ctx.set_fill_shader(&sweep());
        ctx.fill_rect(0.0, 0.0, 64.0, 64.0);
    };
    let translucent = |ctx: &mut Context2D| {
        over_a_fill(ctx, 64.0);
        ctx.set_global_alpha(0.6);
        ctx.set_fill_shader(&sweep());
        ctx.fill_rect(0.0, 0.0, 64.0, 64.0);
    };
    let scenes: [Named; 3] = [
        ("a triangle", &triangle),
        ("a multiply", &multiplied),
        ("a translucent fill", &translucent),
    ];
    for (density, (name, scene)) in [2.0f32, 1.5]
        .into_iter()
        .flat_map(|density| scenes.map(|scene| (density, scene)))
    {
        let read = |gpu: bool| {
            let mut canvas = Canvas::new(64.0, 64.0);
            canvas.set_gpu(gpu);
            scene(canvas.context());
            canvas
                .to_buffer(
                    ImageFormat::Raw,
                    &EncodeOptions {
                        density,
                        ..Default::default()
                    },
                )
                .expect("raw export")
        };
        let side = (64.0 * density) as usize;
        let (cpu, gpu) = (read(false), read(true));
        assert_eq!(cpu.len(), side * side * 4, "{name} read at {density}");
        assert_eq!(
            interior_apart(&cpu, &gpu, side),
            0,
            "{name} read at {density}"
        );
    }
}

/// A run of many gradient draws, each a path Ganesh derives coordinates for
/// only nearly, takes the effect's images from one atlas: each draw must
/// sample its own slot, in rows the atlas packs them into.
#[test]
fn a_run_of_many_gradient_draws_stores_the_cpus_bytes_inside() {
    if !common::has_gpu() {
        return;
    }
    const SIDE_PX: usize = 256;
    /// A cell of the grid the triangles are laid on, each apart from the
    /// rest, so that every edge lies over the transparent page and only the
    /// triangles' insides are compared.
    const CELL_PX: f32 = 12.8;
    let side = SIDE_PX as f32;
    let scene = |ctx: &mut Context2D| {
        // Two sweeps, which ask a vector backend for the same, so that the
        // draws are one run, and the atlas holds them all.
        let (one, other) = (sweep(), sweep_at(Point { x: 190.5, y: 70.25 }));
        for k in 0..400 {
            let (x, y) = ((k % 20) as f32 * CELL_PX, (k / 20) as f32 * CELL_PX);
            ctx.set_fill_shader(if k % 2 == 0 { &one } else { &other });
            ctx.begin_path();
            ctx.move_to(x + 0.3, y + 0.6);
            ctx.line_to(x + 11.4, y + 1.1);
            ctx.line_to(x + 2.2, y + 11.7);
            ctx.fill(FillRule::NonZero);
        }
    };
    let (cpu, gpu) = (
        drawn(false, side, side, &scene),
        drawn(true, side, side, &scene),
    );
    // Each slot maps the effect through a translation of its own, which can
    // move a value on a half level to the next: no further.
    let most = interior_most_apart(&cpu, &gpu, SIDE_PX);
    assert!(most <= 1, "{most} levels apart");
}

/// A gradient through a blend that changes what lies beyond the draw --
/// `copy`, `source-in` -- clears the page outside it as the CPU canvas
/// does: every byte, not only those inside.
#[test]
fn a_gradient_through_a_blend_beyond_the_draw_stores_the_cpus_bytes() {
    if !common::has_gpu() {
        return;
    }
    for blend in [BlendMode::Copy, BlendMode::SourceIn] {
        let scene = |ctx: &mut Context2D| {
            over_a_fill(ctx, 64.0);
            ctx.set_global_composite_operation(blend);
            ctx.set_fill_shader(&sweep());
            ctx.fill_rect(10.0, 12.0, 30.0, 25.0);
        };
        let (cpu, gpu) = (
            drawn(false, 64.0, 64.0, &scene),
            drawn(true, 64.0, 64.0, &scene),
        );
        let apart = cpu.iter().zip(&gpu).filter(|(a, b)| a != b).count();
        assert_eq!(apart, 0, "{blend:?}: {apart} bytes apart");
        // The Canvas standard's result, which both engines could miss alike:
        // the fill outside the draw is gone.
        let outside = (2 * 64 + 2) * 4;
        assert_eq!(
            &cpu[outside..outside + 4],
            &[0; 4],
            "{blend:?}: kept outside"
        );
    }
}

/// A run of gradient draws the CPU makes over the GPU's pixels -- here
/// interpolated in Oklab and translucent, which the effect does not
/// reproduce -- each overlapping the last: the run is read back once and
/// drawn in order over what each draw before it left, and stores the CPU's
/// bytes, every one.
#[test]
fn a_run_drawn_on_the_cpu_over_the_gpus_pixels_stores_the_cpus_bytes() {
    if !common::has_gpu() {
        return;
    }
    let scene = |ctx: &mut Context2D| {
        over_a_fill(ctx, 96.0);
        let swirled = radial(GradientColorSpace::Oklab);
        ctx.set_fill_shader(&swirled);
        for k in 0..40 {
            let at = k as f32;
            ctx.fill_rect((at * 7.3) % 70.0, (at * 4.1) % 70.0, 26.0, 22.0);
        }
    };
    assert_cpus_bytes(96.0, 96.0, &scene);
}

/// A canvas holding a canvas drawn into it is rasterized where it is drawn
/// in turn, and its gradients with it: drawn into a GPU canvas, that raster
/// is made and composited on the CPU, at the destination's pixels, as the
/// draw a GPU canvas makes of the gradients themselves is (#703). Ganesh
/// drawing the raster put 4,800 bytes of this one apart from the CPU's.
#[test]
fn a_canvas_holding_one_drawn_into_a_gpu_canvas_stores_the_cpus_bytes() {
    if !common::has_gpu() {
        return;
    }
    let mut source = Canvas::new(64.0, 64.0);
    {
        let ctx = source.context();
        ctx.set_fill_shader(&stripes());
        ctx.fill_rect(0.0, 0.0, 64.0, 64.0);
    }
    let mut holder = Canvas::new(64.0, 64.0);
    holder.context().draw_canvas(&mut source, 0.0, 0.0);
    let mut draw = |gpu: bool| {
        let mut canvas = Canvas::new(96.0, 96.0);
        canvas.set_gpu(gpu);
        {
            let ctx = canvas.context();
            over_a_fill(ctx, 96.0);
            ctx.set_global_alpha(0.7);
            ctx.draw_canvas_sized(&mut holder, 0.0, 0.0, 96.0, 96.0);
        }
        pixels(&mut canvas)
    };
    let (cpu, gpu) = (draw(false), draw(true));
    let apart = cpu.iter().zip(&gpu).filter(|(a, b)| a != b).count();
    assert_eq!(apart, 0, "{apart} bytes apart from the CPU canvas's");
}

/// The spacing of a half float between 128 and 256, in levels: 2^(7 - 10),
/// a half float carrying ten bits of fraction. Metal composites at half
/// precision, so a byte the GPU rounds the other way from the exact
/// composite lies near a half level, by at most half this spacing for each
/// rounding the composite takes.
const HALF_FLOAT_SPACING_ABOVE_128: f64 = 1.0 / (1u32 << (10 - 7)) as f64;

/// How many times a composite through the coverage mask rounds to half
/// precision: the source's product with the coverage, `1 - a`, `d * (1 -
/// a)` and the sum. Measured, a `multiply` byte the GPU rounds the other way
/// lies 0.143 of a level from a tie at most, inside the bound this gives.
const BLEND_ROUNDINGS: u32 = 4;

/// The backdrop [`edge_misses`] draws over.
const BACKDROP: [u8; 3] = [200, 220, 240];

/// A sweep about `center` with a hard stop, opaque.
fn hard_stop(center: Point) -> Shader {
    Shader::sweep_gradient(
        center,
        0.0,
        360.0,
        &[
            stop(0.0, 255, 255, 0, 1.0),
            stop(0.5, 255, 255, 0, 1.0),
            stop(0.5, 0, 128, 0, 1.0),
            stop(1.0, 0, 128, 0, 1.0),
        ],
        GradientColorSpace::Destination,
    )
    .expect("a sweep")
}

/// A linear ramp across `width` pixels, opaque.
fn ramp(width: f32) -> Shader {
    Shader::linear_gradient(
        Point { x: 10.0, y: 0.0 },
        Point {
            x: width - 10.0,
            y: 0.0,
        },
        &[stop(0.0, 250, 200, 0, 1.0), stop(1.0, 0, 60, 200, 1.0)],
        GradientColorSpace::Destination,
    )
    .expect("a ramp")
}

/// The bytes a GPU canvas stores for `draw`, dyed by `shader` through
/// `blend` over [`BACKDROP`] on a `width` by `height` page, that are neither
/// the CPU canvas's nor explained: each as (pixel, CPU, GPU, exact).
///
/// A byte is explained where the gradient's own byte already parts from the
/// CPU's, which `every_shape_stores_the_cpus_bytes_inside_it` holds. A
/// `source-over` byte is explained by nothing else: the GPU composites it
/// in the CPU's arithmetic (#783).
///
/// Under any other blend a byte is explained too where the exact composite
/// `d + c * (blend(s, d) - d)` -- `s` the gradient's own bytes, `c` the
/// CPU's coverage -- lies within the GPU's blend noise of a half level.
/// Under a blend the CPU rounds before it takes the coverage, so a byte the
/// GPU stores as the exact composite rounded once may stand a level from
/// the CPU's: that is the CPU's eight-bit order of rounding, #736's case,
/// and is explained too.
fn edge_misses(
    (width, height): (f32, f32),
    draw: &dyn Fn(&mut Context2D),
    shader: &dyn Fn() -> Shader,
    blend: BlendMode,
) -> Vec<(usize, u8, u8, f64)> {
    let page =
        |gpu, scene: &dyn Fn(&mut Context2D)| drawn(gpu, width, height, scene);
    // The CPU's coverage of the shape, and the gradient's own bytes, the
    // page covered, on each engine.
    let coverage = page(false, draw);
    let whole = |ctx: &mut Context2D| {
        ctx.set_fill_shader(&shader());
        ctx.fill_rect(0.0, 0.0, width, height);
    };
    let (cpu_source, gpu_source) = (page(false, &whole), page(true, &whole));
    let scene = |ctx: &mut Context2D| {
        let [r, g, b] = BACKDROP;
        ctx.set_fill_style(RgbaLinear::from_srgb8(r, g, b, 1.0));
        ctx.fill_rect(0.0, 0.0, width, height);
        ctx.set_global_composite_operation(blend);
        ctx.set_fill_shader(&shader());
        ctx.set_stroke_shader(&shader());
        draw(ctx);
    };
    let (cpu, gpu) = (page(false, &scene), page(true, &scene));
    let exact = |i: usize| {
        let c = f64::from(coverage[i | 3]) / 255.0;
        let d = f64::from(BACKDROP[i % 4]);
        let s = f64::from(cpu_source[i]);
        let sa = f64::from(cpu_source[i | 3]) / 255.0;
        let blended = match blend {
            BlendMode::Multiply => s * d / 255.0 + d * (1.0 - sa),
            _ => s + d * (1.0 - sa),
        };
        d + c * (blended - d)
    };
    let noise = BLEND_ROUNDINGS as f64 * HALF_FLOAT_SPACING_ABOVE_128 / 2.0;
    (0..cpu.len())
        .filter(|&i| i % 4 != 3 && cpu[i] != gpu[i])
        .filter(|&i| cpu_source[i] == gpu_source[i])
        .map(|i| (i / 4, cpu[i], gpu[i], exact(i)))
        .filter(|&(_, cpu, gpu, exact)| {
            if blend == BlendMode::SourceOver {
                return true;
            }
            let rounded_once =
                f64::from(gpu) == exact.round() && cpu.abs_diff(gpu) == 1;
            let tie = (exact.fract() - 0.5).abs() < noise;
            !(rounded_once
                || tie && (f64::from(gpu) - exact).abs() < 0.5 + noise)
        })
        .collect()
}

/// The coverage of `draw`'s paths held to the CPU's byte for byte: on a
/// transparent page an opaque gradient's alpha is its coverage, which no
/// blend rounds.
fn assert_cpus_coverage(
    size: (f32, f32),
    name: &str,
    draw: &dyn Fn(&mut Context2D),
    shader: &dyn Fn() -> Shader,
) {
    let alone = |gpu| {
        drawn(gpu, size.0, size.1, &|ctx: &mut Context2D| {
            ctx.set_fill_shader(&shader());
            ctx.set_stroke_shader(&shader());
            draw(ctx);
        })
    };
    let (cpu, gpu) = (alone(false), alone(true));
    let apart = (3..cpu.len())
        .step_by(4)
        .filter(|&i| cpu[i] != gpu[i])
        .map(|i| (i / 4, cpu[i], gpu[i]))
        .collect::<Vec<_>>();
    assert!(
        apart.is_empty(),
        "{name}: {} pixels' coverage off the CPU canvas's, the first \
         (pixel, CPU, GPU) {:?}",
        apart.len(),
        apart.first(),
    );
}

/// [`assert_cpus_coverage`], and `edge_misses` for each blend asserted
/// empty. The paths are to lie apart, so that each composites over the
/// backdrop alone and rounds once.
fn assert_cpus_edge(
    size: (f32, f32),
    name: &str,
    draw: &dyn Fn(&mut Context2D),
    shader: &dyn Fn() -> Shader,
) {
    assert_cpus_coverage(size, name, draw, shader);
    for blend in [BlendMode::SourceOver, BlendMode::Multiply] {
        let misses = edge_misses(size, draw, shader, blend);
        assert!(
            misses.is_empty(),
            "{name}, {blend:?}: {} bytes off the CPU canvas's away from a \
             tie, the first (pixel, CPU, GPU, exact) {:?}",
            misses.len(),
            misses.first(),
        );
    }
}

/// A path dyed by a gradient, drawn on a GPU canvas, takes the CPU's coverage
/// of its edge, so that its shadow and its blur, which a chain makes on the
/// CPU, are cast from the edge it draws (#766) -- see [`edge_misses`] for
/// what is held. Drawn through Ganesh's coverage, the triangle under a hard
/// stop parted from the CPU's coverage on 506 pixels.
#[test]
fn a_gradient_paths_edge_is_the_cpus() {
    if !common::has_gpu() {
        return;
    }
    let shapes: [Named; 7] = [
        ("a triangle", &|ctx| {
            ctx.begin_path();
            ctx.move_to(20.0, 140.0);
            ctx.line_to(100.0, 15.25);
            ctx.line_to(180.5, 145.0);
            ctx.fill(FillRule::NonZero);
        }),
        // Cut where the page ends, as the CPU canvas cuts it -- see
        // `layer::DeviceCoverage`.
        ("a triangle past every edge", &|ctx| {
            ctx.begin_path();
            ctx.move_to(-40.5, 190.0);
            ctx.line_to(100.25, -30.7);
            ctx.line_to(250.4, 140.2);
            ctx.fill(FillRule::NonZero);
        }),
        // A run whose paths share the atlas, each held to its own slot. They
        // lie apart, so that each composites over the backdrop alone.
        ("a run past the edges", &|ctx| {
            for i in 0..6u8 {
                let (x, y) = (
                    f32::from(i % 3) * 80.0 - 25.3,
                    f32::from(i / 3) * 115.0 - 30.6,
                );
                ctx.begin_path();
                ctx.move_to(x, y + 85.0);
                ctx.line_to(x + 30.25, y + 0.7);
                ctx.line_to(x + 70.4, y + 80.2);
                ctx.fill(FillRule::NonZero);
            }
        }),
        ("a disc", &|ctx| {
            ctx.begin_path();
            ctx.arc(100.3, 80.6, 60.2, 0.0, std::f32::consts::TAU, false)
                .expect("a positive radius");
            ctx.fill(FillRule::NonZero);
        }),
        ("a rectangle off whole pixels", &|ctx| {
            ctx.fill_rect(20.3, 15.6, 150.4, 120.2);
        }),
        // Turned by its corners rather than by the transform, so that the
        // gradient lies on the page as it does for the page-covering fill
        // its own bytes are read from.
        ("a turned rectangle", &|ctx| {
            let (sin, cos) = 0.3f32.sin_cos();
            let corner = |x: f32, y: f32| {
                (100.0 + x * cos - y * sin, 80.0 + x * sin + y * cos)
            };
            let corners = [
                corner(-60.0, -40.0),
                corner(60.0, -40.0),
                corner(60.0, 40.0),
                corner(-60.0, 40.0),
            ];
            ctx.begin_path();
            ctx.move_to(corners[0].0, corners[0].1);
            for (x, y) in &corners[1..] {
                ctx.line_to(*x, *y);
            }
            ctx.fill(FillRule::NonZero);
        }),
        ("a stroke", &|ctx| {
            ctx.set_line_width(7.5);
            ctx.begin_path();
            ctx.move_to(20.0, 140.0);
            ctx.bezier_curve_to(40.0, 0.0, 160.0, 0.0, 180.0, 140.0);
            ctx.stroke();
        }),
    ];
    let size = (200.0, 160.0);
    let hard = || hard_stop(Point { x: 100.0, y: 80.0 });
    let smooth = || ramp(size.0);
    for (shape, draw) in shapes {
        assert_cpus_edge(size, &format!("{shape}, a hard stop"), draw, &hard);
        assert_cpus_edge(size, &format!("{shape}, a ramp"), draw, &smooth);
    }
}

/// A path wider than a run's atlas -- see `layer::atlas` -- takes the CPU's
/// edge as well, cut where the page ends for one that crosses it.
#[test]
fn a_gradient_path_past_the_atlas_takes_the_cpus_edge() {
    if !common::has_gpu() {
        return;
    }
    let size = (2600.0, 80.0);
    let triangle = |ctx: &mut Context2D| {
        ctx.begin_path();
        ctx.move_to(10.5, 75.0);
        ctx.line_to(1300.25, 4.7);
        ctx.line_to(2590.4, 72.2);
        ctx.fill(FillRule::NonZero);
    };
    let past = |ctx: &mut Context2D| {
        ctx.begin_path();
        ctx.move_to(10.5, 75.0);
        ctx.line_to(1300.25, 4.7);
        ctx.line_to(2900.4, 60.2);
        ctx.fill(FillRule::NonZero);
    };
    let hard = || hard_stop(Point { x: 1300.0, y: 40.0 });
    let smooth = || ramp(size.0);
    assert_cpus_edge(size, "a hard stop", &triangle, &hard);
    assert_cpus_edge(size, "a ramp", &triangle, &smooth);
    assert_cpus_edge(size, "past the edge, a hard stop", &past, &hard);
    assert_cpus_edge(size, "past the edge, a ramp", &past, &smooth);
}

/// Paths wider than a run's atlas, several in one run, share an alpha
/// atlas of their coverage -- see `layer::atlas` -- and each takes the CPU's
/// edge from it, the one crossing the page's edge cut where the page ends.
#[test]
fn wide_gradient_paths_in_one_run_take_the_cpus_edge() {
    if !common::has_gpu() {
        return;
    }
    let size = (2600.0, 200.0);
    let triangles = |ctx: &mut Context2D| {
        for (i, right) in
            [2590.4, 2210.7, 2900.4, 2450.1].into_iter().enumerate()
        {
            let top = 48.0 * i as f32 + 4.7;
            ctx.begin_path();
            ctx.move_to(10.5 + 3.3 * i as f32, top + 40.3);
            ctx.line_to(1300.25, top);
            ctx.line_to(right, top + 37.5);
            ctx.fill(FillRule::NonZero);
        }
    };
    let hard = || {
        hard_stop(Point {
            x: 1300.0,
            y: 100.0,
        })
    };
    let smooth = || ramp(size.0);
    assert_cpus_edge(size, "a hard stop", &triangles, &hard);
    assert_cpus_edge(size, "a ramp", &triangles, &smooth);
}

/// A gradient path's edge on a GPU canvas is the CPU canvas's wherever the
/// run's atlas places it (#813): alone, in a grid of its own kind, and
/// beside a larger path, which moves its slot.
#[test]
fn a_gradient_paths_edge_is_the_cpus_wherever_its_slot_lies() {
    if !common::has_gpu() {
        return;
    }
    let disc = |ctx: &mut Context2D, x: f32, y: f32, r: f32| {
        ctx.begin_path();
        ctx.arc(x, y, r, 0.0, std::f32::consts::TAU, false)
            .expect("a positive radius");
        ctx.fill(FillRule::NonZero);
    };
    let scenes: [Named; 4] = [
        ("one disc", &|ctx| disc(ctx, 16.4, 16.7, 10.3)),
        ("sixteen discs", &|ctx| {
            for i in 0..16u8 {
                let (x, y) = (f32::from(i % 4) * 32.0, f32::from(i / 4) * 32.0);
                disc(ctx, 16.4 + x, 16.7 + y, 10.3);
            }
        }),
        ("a disc alone", &|ctx| disc(ctx, 106.2, 106.5, 12.3)),
        ("a disc after a larger one", &|ctx| {
            disc(ctx, 60.7, 55.1, 25.4);
            disc(ctx, 106.2, 106.5, 12.3);
        }),
    ];
    let size = (128.0, 128.0);
    let smooth = || ramp(size.0);
    for (scene, draw) in scenes {
        assert_cpus_edge(size, &format!("{scene}, a ramp"), draw, &smooth);
    }
    // Their areas overlap, so that the second is drawn where the first's
    // coverage was. Over a backdrop the second composites over the first,
    // which is not what `edge_misses` holds, so only the coverage is.
    let across = |ctx: &mut Context2D| {
        disc(ctx, 40.3, 44.6, 20.1);
        disc(ctx, 58.7, 52.2, 18.4);
    };
    assert_cpus_coverage(size, "two discs across each other", &across, &smooth);
}
