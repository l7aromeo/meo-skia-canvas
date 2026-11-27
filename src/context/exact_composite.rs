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
//! eight-bit ones of `SkRasterPipeline_opts.h`. `div255` rounds to the
//! nearest level on aarch64, and elsewhere is `(v + 255) >> 8` -- see
//! [`EXACT_SCALE`]. `div255_accurate`, which the `source-over` stage takes,
//! rounds to the nearest level everywhere. Measured on the edge scenes of
//! `tests/gpu_gradient.rs` and the scenes of
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
//! An image is such an effect too, read through [`image_bytes`]: a
//! translucent image drawn `source-over` at whole pixels -- see
//! `layer::OnGpu::Image` -- composited through [`composite_once`]. The
//! draw's alpha is the coverage, as the CPU scales a source by its paint's
//! alpha in the same `div255` (#837).
//!
//! [`float_over`] is the same composite for a source the CPU draws on its
//! float pipeline, rounded once, which a layer takes from a raster of
//! sixteen bits a channel -- see `layer::rasterized_onto`.

#[cfg(test)]
use std::cell::Cell;

use skia_safe::{
    AlphaType, BlendMode, Blender, Canvas as SkCanvas, Color, Data, FilterMode,
    IRect, Image, Matrix, MipmapMode, Paint, RuntimeEffect, SamplingOptions,
    Shader, TileMode, runtime_effect::ChildPtr, shaders,
};

use crate::context::{exact_gradient::EXACT_SCALE, layer::byte_alpha};

/// The SkSL of the CPU's composite -- see the module's doc -- with
/// `div255` both ways the CPU computes it, nearest where `nearest` -- see
/// [`EXACT_SCALE`] -- `div255_accurate` always nearest, and `opaque`
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

/// The SkSL handing on an image's texels as the bytes the CPU composites:
/// read raw -- no colour conversion and no premultiplication of Skia's --
/// and, where `straight` -- the image unpremultiplied -- premultiplied as the
/// CPU's eight-bit `premul` stage does it, `div255(c * a)` -- see
/// [`COMPOSITE`]'s `div255`. Skia premultiplies an unpremultiplied image on the
/// GPU in floats, which rounds otherwise.
const IMAGE_BYTES: &str = r#"
uniform shader image;
uniform int nearest;
uniform int straight;
half4 main(float2 p) {
    float4 c = floor(float4(image.eval(p)) * 255.0 + 0.5);
    if (straight != 0) {
        float3 v = c.rgb * c.a;
        c.rgb = nearest != 0
            ? floor((v + 128.0 + floor((v + 128.0) / 256.0)) / 256.0)
            : floor((v + 255.0) / 256.0);
    }
    return half4(c / 255.0);
}
"#;

thread_local! {
    /// [`IMAGE_BYTES`] compiled, once per thread.
    static IMAGE: Option<RuntimeEffect> =
        RuntimeEffect::make_for_shader(IMAGE_BYTES, None).ok();
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
}

/// The uniforms [`COMPOSITE`] takes, for a paint the CPU draws as `src`
/// where `opaque`.
fn uniforms(opaque: bool) -> Data {
    let values: Vec<u8> = [i32::from(EXACT_SCALE), i32::from(opaque)]
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

/// An effect of `image`'s bytes as the CPU composites them -- see
/// [`IMAGE_BYTES`] -- each texel on the pixel `local` maps it to and
/// transparent past the image's edges: for an image in eight bits a
/// channel, drawn onto a page in its own colour space, whose texels are
/// whole levels. `None` where Skia refuses either shader.
pub(crate) fn image_bytes(image: &Image, local: &Matrix) -> Option<Shader> {
    let unpremul = image.alpha_type() == AlphaType::Unpremul;
    let raw = image.to_raw_shader(
        (TileMode::Decal, TileMode::Decal),
        SamplingOptions::new(FilterMode::Nearest, MipmapMode::None),
        Some(local),
    )?;
    let values: Vec<u8> = [i32::from(EXACT_SCALE), i32::from(unpremul)]
        .iter()
        .flat_map(|value| value.to_ne_bytes())
        .collect();
    IMAGE.with(|compiled| {
        compiled.as_ref()?.make_shader(
            Data::new_copy(&values),
            &[ChildPtr::Shader(raw)],
            None,
        )
    })
}

/// The coverage a draw at `alpha` composites under: `None` for an opaque
/// one, which [`COMPOSITE`] then takes as full coverage. The CPU scales a
/// source by its paint's alpha as it scales it by coverage,
/// `div255(s * a)`, the alpha first taken to a byte -- see `byte_alpha`.
pub(crate) fn alpha_coverage(alpha: f32) -> Option<Shader> {
    let byte = (byte_alpha(alpha) * f32::from(u8::MAX)).round() as u8;
    (byte < u8::MAX)
        .then(|| shaders::color(Color::from_argb(byte, 255, 255, 255)))
}

/// Composites `effect`, whose levels are whole, `source-over` under
/// `coverage` onto `canvas` within `rect`, device pixels, in the CPU's
/// arithmetic: over one snapshot of `rect` where `snapshot` -- the
/// canvas's own surface holds the pixels beneath -- and through
/// [`blended`]'s blender, which reads the destination itself, where not.
pub(crate) fn composite_once(
    canvas: &SkCanvas,
    rect: IRect,
    effect: Shader,
    coverage: Option<Shader>,
    snapshot: bool,
) {
    let pending = Pending {
        rect,
        effect,
        coverage,
        opaque: false,
    };
    if snapshot {
        let mut batch = Batch {
            pending: vec![pending],
        };
        batch.flush(canvas);
        return;
    }
    draw_on_device(canvas, rect, &pending.unsnapped());
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

/// Draws that read the page's pixels, held until one would read what
/// another writes: then one snapshot of all their rectangles is taken on
/// the GPU and each is drawn over it with `src`.
///
/// The snapshot is what keeps the route cheap. Through the blender alone,
/// which Metal runs by copying the destination for each draw, the route
/// cost 3.5 times Ganesh's own composite over 200 conic triangles; with one
/// snapshot for the run it costs 1.03 to 1.07 times for 60 to 200 draws,
/// and 1.31 times for 2,000 small ones, each still a draw of its own.
///
/// Held only on the canvas's own surface, never inside a layer -- see
/// `layer::drawn_with_effect` -- and flushed before anything else is drawn
/// over what it holds, and before the canvas's clip changes.
#[derive(Default)]
pub(crate) struct Batch {
    pending: Vec<Pending>,
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
        if self
            .pending
            .iter()
            .any(|p| IRect::intersects(&p.rect, &rect))
        {
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
