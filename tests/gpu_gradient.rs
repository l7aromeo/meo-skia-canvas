//! A gradient on a GPU canvas stores the bytes the CPU canvas stores for it
//! (#667): drawn by an effect computing the CPU's arithmetic on the GPU, or,
//! where it cannot, by the CPU over the GPU's pixels. Ganesh's own gradient
//! moved a hard stop by up to a tenth of a degree or half a pixel and landed
//! a smooth ramp up to five levels away.
//!
//! Each test skips where there is no GPU, which is every CI runner, and says
//! so through `engine_kind` rather than assuming.

use meo_skia_canvas::{canvas::EngineKind, prelude::*};

fn pixels(canvas: &mut Canvas) -> Vec<u8> {
    canvas
        .to_buffer(ImageFormat::Raw, &EncodeOptions::default())
        .expect("raw export")
}

/// Whether this machine draws a canvas asking for the GPU on it.
fn has_gpu() -> bool {
    let mut probe = Canvas::new(1.0, 1.0);
    probe.set_gpu(true);
    probe.engine_kind() == EngineKind::Gpu
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
    if !has_gpu() {
        return;
    }
    assert_cpus_bytes(64.0, 64.0, &|ctx| {
        ctx.set_fill_shader(&sweep());
        ctx.fill_rect(0.0, 0.0, 64.0, 64.0);
    });
}

#[test]
fn hard_stops_past_128_colours_store_the_cpus_bytes() {
    if !has_gpu() {
        return;
    }
    assert_cpus_bytes(1024.0, 2.0, &|ctx| {
        ctx.set_fill_shader(&stripes());
        ctx.fill_rect(0.0, 0.0, 1024.0, 2.0);
    });
}

#[test]
fn a_translucent_two_point_conical_gradient_stores_the_cpus_bytes() {
    if !has_gpu() {
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
    if !has_gpu() {
        return;
    }
    assert_cpus_bytes(96.0, 96.0, &|ctx| {
        over_a_fill(ctx, 96.0);
        ctx.set_fill_shader(&radial(GradientColorSpace::Oklab));
        ctx.fill_rect(0.0, 0.0, 96.0, 96.0);
    });
}

/// How many pixels of `gpu` part from `cpu` among those `cpu` inks whole,
/// with all eight neighbours: the edge of a shape is the GPU's own coverage,
/// as it is for a flat colour, and is not counted.
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
/// neighbours: the edge of a shape is the GPU's own coverage, as it is for
/// a flat colour.
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
    if !has_gpu() {
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

#[test]
fn a_canvas_drawn_on_the_cpu_and_read_on_the_gpu_stores_the_cpus_bytes() {
    if !has_gpu() {
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
    if !has_gpu() {
        return;
    }
    let scene = |ctx: &mut Context2D| {
        ctx.set_fill_shader(&sweep());
        ctx.fill_rect(0.0, 0.0, 64.0, 64.0);
    };
    let expected = drawn(false, 64.0, 64.0, &scene);
    for into_gpu in [false, true] {
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
    if !has_gpu() {
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
        // Through another blend, inside the layer the source is drawn
        // through, the draw made on the CPU over transparent is composited
        // through the blend by the GPU: a level apart where its rounding and
        // the CPU's differ. `source-atop` inks nothing over transparent, so
        // the draw is made there with `source-over`.
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
    if !has_gpu() {
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
    if !has_gpu() {
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
    if !has_gpu() {
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
    if !has_gpu() {
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
