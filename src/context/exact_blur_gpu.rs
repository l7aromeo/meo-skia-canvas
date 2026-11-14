//! The exact Gaussian blur of pixel content, on the GPU.
//!
//! The same blur as [`exact_blur`](super::exact_blur) -- the Gaussian sampled
//! at whole-pixel offsets over a 6-sigma reach, rounded once -- as runtime
//! effects. Each axis is a pass of its own, and where its sigma allows, the
//! pass is decimated: with a factor `s`, a blur of sigma `a = s` at the kept
//! positions only, a blur of sigma `b / s` along the coarse grid of every
//! `s`-th position, and a sum back to every position of `s` times a blur of
//! sigma `s`, where `a^2 + b^2 + s^2 = sigma^2`.
//!
//! Decimation is not the sampled Gaussian exactly, and cannot be: a blurred
//! signal is not band-limited, so keeping every `s`-th sample aliases. But
//! the aliasing is bounded. The fold at `pi / s` is suppressed by the two
//! first blurs' spectra there, `exp(-(a^2 + b^2) pi^2 / (2 s^2))`, and the
//! images the sum back makes at `2 pi / s` by the last one's,
//! `exp(-2 pi^2)`. With [`MIN_COARSE`] or more coarse samples of blur, the
//! error measured against the sampled Gaussian in f64 stays at or under
//! 6e-6 of a level -- twenty times under the 1e-4 an f32 accumulation
//! carries -- so it rounds to the same byte as the direct sum except on
//! exact ties, which is the criterion the CPU blur is held to as well.
//!
//! Three precision traps, each found by measuring:
//!
//! - Ganesh's Metal backend has no 32-bit float render target. A pass's result
//!   is kept in two half-float targets, the value and its remainder, in units
//!   of levels, which together hold it to about 22 bits. The value is snapped
//!   to its nearest half in float arithmetic, not cast to `half`: SPIR-V marks
//!   `half` relaxed precision only, which a driver may compute in full float,
//!   and on Mesa's lavapipe that left the remainder zero and a blur rounding up
//!   to 0.11 of a level to the wrong side of a half.
//! - A child shader returns half precision, which cannot hold `n / 255`
//!   exactly. An 8-bit source's byte is recovered as the nearest level, which
//!   the half's error, under 0.06 of a level, cannot cross.
//! - The fragment reaches an 8-bit target through half precision, which cannot
//!   hold a level sum to better than 0.06 either. The last pass onto an 8-bit
//!   target rounds to a whole level itself, which survives.

use skia_safe::{
    AlphaType, BlendMode, Color, ColorSpace, ColorType, Data, FilterMode,
    IRect, Image, ImageInfo, MipmapMode, Paint, Recorder, Rect, RuntimeEffect,
    SamplingOptions, TileMode,
    gpu::{Budgeted, RecordingContext, SurfaceOrigin, surfaces},
    runtime_effect::ChildPtr,
};

use std::cell::RefCell;

use super::exact_blur::{REACH_SIGMAS, gaussian_sum};

/// The fewest coarse samples of blur the decimated passes may be left
/// with: `b / s` at or over this keeps the aliasing at or under 6e-6 of a
/// level, measured at sigmas 4 to 100 on noise with hard edges. At 1.4 it
/// reached 6e-5, and at 0.5 three tenths of a level.
pub(crate) const MIN_COARSE: f64 = 2.5;

/// The decimation factor for `sigma`, or `None` where the axis is blurred
/// tap by tap. With `a = c = s`, `b^2 = sigma^2 - 2 s^2`, so `b / s` is at
/// least [`MIN_COARSE`] for every `s` up to `sigma / sqrt(2 + MIN_COARSE^2)`.
pub(crate) fn factor(sigma: f32) -> Option<usize> {
    let s = (f64::from(sigma) / (2.0 + MIN_COARSE * MIN_COARSE).sqrt()).floor();
    (s >= 2.0).then_some(s as usize)
}

/// One pass along `dir`.
///
/// - Mode 0: output sample `o` of the coarse grid is the blur at full-rate
///   position `origin + o * spacing`.
/// - Mode 1: the blur along whatever grid the input is on.
/// - Mode 2: full-rate sample `o` gathers the coarse samples within `reach` of
///   it.
///
/// `source` says what the children hold: 0, a value and its remainder in
/// levels; 1, an 8-bit image, whose byte is recovered; 2, a float image;
/// 3, an alpha's value and remainder packed into the red and green of the
/// first child, which is read into every channel. `part` says what to
/// write: 1 and 2, the value and its remainder in levels; 0, the value
/// rounded to a level, for an 8-bit target; 3, the value unrounded, for a
/// float one; 4, the alpha's value and remainder packed into red and green,
/// for the next pass to read as 3. Outside a texture, decal reads zero.
///
/// Keep the weights as `exp` per tap. Read instead from a table in the
/// uniforms, worked out once on the CPU, a drop shadow drew level with it
/// on Metal -- 1.007 to 1.010 of its cost at 3px and 16px -- so the passes
/// are bound by their texture reads and their count, not by arithmetic.
///
/// The constants it names are declared ahead of it by [`source`].
const PASS: &str = r#"
uniform shader hi;
uniform shader lo;
uniform float2 dir;
uniform float mode;
uniform float spacing;
uniform float origin;
uniform float reach;
uniform float c;
uniform float norm;
uniform float part;
uniform float source;
uniform float outX;
uniform float outY;
uniform float inX;
uniform float inY;
uniform float boxL;
uniform float boxT;
uniform float boxR;
uniform float boxB;
float4 snap(float4 x) {
    float4 a = abs(x);
    float4 e = floor(log2(max(a, HALF_MIN_NORMAL)));
    e += step(exp2(e + 1), a);
    e -= 1 - step(exp2(e), max(a, HALF_MIN_NORMAL));
    float4 ulp = exp2(e - HALF_MANTISSA_BITS);
    return sign(x) * floor(a / ulp + 0.5) * ulp;
}
float4 at(float2 q) {
    if (BOXED && (q.x < boxL || q.y < boxT || q.x >= boxR || q.y >= boxB)) {
        return float4(0);
    }
    q += float2(inX, inY);
    if (source == 3) {
        float4 x = float4(hi.eval(q));
        return float4(x.r + x.g);
    }
    float4 v = float4(hi.eval(q)) + float4(lo.eval(q));
    if (source == 1) { return floor(v * 255 + 0.5); }
    if (source == 2) { return v * 255; }
    return v;
}
float4 main(float2 position) {
    // The pixel's centre exactly, in the lane's own pixels: the position a
    // draw interpolates is off it by an amount that grows with how far the
    // lane lies from the target's origin, and on NVIDIA's Vulkan driver a
    // lane thousands of pixels in moved taps near a tie to its other side.
    float2 p = floor(position) + 0.5 - float2(outX, outY);
    float2 across = p * (float2(1) - dir);
    float o = dot(p, dir) - 0.5;
    float4 acc = float4(0);
    if (mode < 2) {
        float centre = mode == 0 ? origin + o * spacing : o;
        for (int i = 0; i < MAX_TAPS; i++) {
            float d = float(i) - reach;
            if (d > reach) { break; }
            acc += exp(-d * d * c) * at(across + dir * (centre - d + 0.5));
        }
    } else {
        float j = ceil((o - reach - origin) / spacing);
        for (int i = 0; i < MAX_TAPS; i++) {
            float d = o - (origin + j * spacing);
            if (d < -reach) { break; }
            acc += exp(-d * d * c) * at(across + dir * (j + 0.5));
            j += 1;
        }
    }
    acc *= norm;
    if (part == 0) { return floor(acc + 0.5) / 255; }
    if (part == 3) { return acc / 255; }
    float4 h = snap(acc);
    if (part == 4) { return float4(h.a, acc.a - h.a, 0, 0); }
    return part == 1 ? h : acc - h;
}
"#;

/// The most taps one pass reads for a sample. SkSL needs a loop's bound to
/// be a constant, so [`PASS`]'s loops stop here whatever the pass asks for,
/// and a pass that needs more is not made: [`Gpu::pass`] refuses it and the
/// CPU blurs the image. The first pass of a decimated axis is the widest, at
/// `2 * ceil(6 s) + 1` taps for a factor `s`, which passes this from a sigma
/// of about 3924, on a line at least one and a half times that long:
/// [`REACH_PER_LINE`] leaves a shorter line's blur to the CPU already.
const MAX_TAPS: usize = 1 << 14;

/// A half float's exponent bias, from IEEE 754's binary16 format.
const HALF_EXPONENT_BIAS: u32 = 15;

/// The smallest positive normal half float, `2^(1 - bias)`: below it a
/// half's spacing stops shrinking, so [`PASS`]'s snap takes its spacing
/// there.
const HALF_MIN_NORMAL: f64 = 1.0 / (1u32 << (HALF_EXPONENT_BIAS - 1)) as f64;

/// The bits of a half float's mantissa, from binary16: a half's spacing is
/// `2^(e - HALF_MANTISSA_BITS)` for a value of exponent `e`.
const HALF_MANTISSA_BITS: u32 = 10;

/// [`PASS`] behind the constants it names.
fn source(boxed: bool) -> String {
    format!(
        "const int MAX_TAPS = {MAX_TAPS};\n\
         const float HALF_MIN_NORMAL = {HALF_MIN_NORMAL:?};\n\
         const float HALF_MANTISSA_BITS = {HALF_MANTISSA_BITS}.0;\n\
         const bool BOXED = {boxed};\n\
         {PASS}"
    )
}

thread_local! {
    /// [`PASS`], compiled once per thread, holding each lane to its box.
    static EFFECT: Option<RuntimeEffect> = RuntimeEffect::make_for_shader(source(true), None).ok();
    /// [`PASS`] for a pass of one lane reading all of its input, which the
    /// input's own edge holds it to: the box's test, made on every tap, is
    /// left out of the shader rather than made and passed.
    static WHOLE_EFFECT: Option<RuntimeEffect> = RuntimeEffect::make_for_shader(source(false), None).ok();
}

/// What a pass's children hold, as [`PASS`]'s `source` uniform reads it.
#[derive(Clone, Copy)]
enum Input {
    Split = 0,
    Bytes = 1,
    Floats = 2,
    Packed = 3,
}

/// What a pass writes, as [`PASS`]'s `part` uniform reads it.
#[derive(Clone, Copy)]
enum Part {
    Rounded = 0,
    Value = 1,
    Remainder = 2,
    Unrounded = 3,
    Packed = 4,
}

/// A pass's geometry along its axis.
#[derive(Clone, Copy)]
struct Taps {
    mode: f32,
    spacing: f32,
    origin: f32,
    reach: usize,
    sigma: f64,
    norm: f64,
}

impl Taps {
    /// How many taps [`PASS`] reads for one sample: every input within
    /// `reach` of it, or in mode 2 every coarse sample within `reach`.
    fn count(&self) -> usize {
        match self.mode == 2.0 {
            true => (2 * self.reach) / self.spacing as usize + 1,
            false => 2 * self.reach + 1,
        }
    }
}

/// How many times its line's length a blur's reach may be on the GPU.
///
/// The passes' taps and the coarse grid's margins grow with the sigma
/// whatever the image's size, and a CSS length can make a sigma as large as
/// 3.4e38, which would hang the GPU; the CPU holds its taps to the line, so
/// a blur this much wider than the image, nearly all of whose taps read the
/// nothing outside it, costs the CPU less. Four keeps on the GPU every blur
/// up to a sigma of two thirds of its line, at which the blur is all but
/// flat across the image.
const REACH_PER_LINE: usize = 4;

/// `1 / sum` of the sampled Gaussian of `sigma` over its reach.
fn norm_of(sigma: f64) -> f64 {
    1.0 / gaussian_sum(sigma)
}

/// The blur of `image`, `(width, height)`, by `sigma_x` along rows and
/// `sigma_y` along columns, scaled by `gain`, made on `context`, written as
/// `out_type` pixels: rounded to a level for an 8-bit type, unrounded for a
/// float one. The gain is the last pass's, so that a blurred draw composited
/// at an alpha is rounded once.
/// `None` where the effect does not compile or a target cannot be made, for
/// a blur that reaches past the image along either axis by more than
/// [`REACH_PER_LINE`] times its length, and for one with a pass of more than
/// [`MAX_TAPS`] taps, all of which the CPU blur takes.
pub(crate) fn blur(
    context: &mut RecordingContext,
    image: &Image,
    (width, height): (usize, usize),
    sigma_x: f32,
    sigma_y: f32,
    out_type: ColorType,
    gain: f32,
) -> Option<Image> {
    // An infinite sigma reaches past any image, and is the CPU's as well.
    let reaches = |sigma: f32, n: usize| {
        f64::from(sigma) * REACH_SIGMAS > (n * REACH_PER_LINE) as f64
    };
    if reaches(sigma_x, width) || reaches(sigma_y, height) {
        return None;
    }
    let effect = EFFECT.with(Clone::clone)?;
    let whole = WHOLE_EFFECT.with(Clone::clone)?;
    let space = image
        .color_space()
        .or_else(|| Some(ColorSpace::new_srgb()))?;
    let float_out =
        !matches!(out_type, ColorType::RGBA8888 | ColorType::BGRA8888);
    let first = if matches!(
        image.color_type(),
        ColorType::RGBA8888 | ColorType::BGRA8888
    ) {
        Input::Bytes
    } else {
        Input::Floats
    };
    let zero = zero(context)?;
    let mut gpu = Gpu {
        context,
        effect,
        whole,
        space,
        zero: zero.clone(),
        packed: false,
    };
    // Along rows, into a value and its remainder; then along columns, into
    // the target.
    let (rows, lanes) = gpu.axis(
        (image.clone(), zero.clone()),
        first,
        [1.0, 0.0],
        &[Lane::whole((width, height))],
        &[(width, height)],
        sigma_x,
        None,
    )?;
    let last = if float_out {
        Part::Unrounded
    } else {
        Part::Rounded
    };
    gpu.axis(
        rows,
        Input::Split,
        [0.0, 1.0],
        &lanes,
        &[(height, width)],
        sigma_y,
        Some((last, out_type, f64::from(gain))),
    )
    .map(|((image, _), _)| image)
}

/// The drop shadow's last pass: `src`, an 8-bit image whose byte is
/// recovered, over its shadow -- the blurred alpha, its value and remainder
/// in levels packed into the red and green of `blurred`, tinted to `(r, g, b)`
/// at `ca` of that alpha and moved by `(ox, oy)` -- scaled by `gain` and
/// rounded to a level once, as the chain's `shadowed` makes it on the CPU in
/// floats, by drawing the tinted shadow at the offset. As that draw does, the
/// shadow covers the pixels of its `w` by `h` rectangle with each edge rounded
/// to the nearest pixel, `floor(x + 0.5)` -- a pixel whose centre lies on an
/// edge at a half is not covered on the left and top, and is on the right and
/// bottom -- and nothing else; where `bilinear` is set it is resampled from its
/// four nearest pixels, each tinted first and each held to the rectangle's edge
/// pixels, and otherwise read at the one pixel the whole offset lands it on.
///
/// One target holds many shadows, each a lane written at `(outX, outY)`,
/// its blur read from `(blurX, blurY)` and its draw from `(srcX, srcY)`. The
/// blur past a lane never shows -- the four pixels are held to the
/// rectangle, and the one pixel past it is masked out -- so it needs no test
/// of its own; the draw is read only within `srcL..srcR` by `srcT..srcB`
/// and is transparent past it, as in a layer of the lane's own.
const SHADOW: &str = r#"
uniform shader blurred;
uniform shader src;
uniform float r;
uniform float g;
uniform float b;
uniform float ca;
uniform float ox;
uniform float oy;
uniform float bilinear;
uniform float gain;
uniform float w;
uniform float h;
uniform float shadowOnly;
uniform float outX;
uniform float outY;
uniform float blurX;
uniform float blurY;
uniform float srcX;
uniform float srcY;
uniform float srcL;
uniform float srcT;
uniform float srcR;
uniform float srcB;
float4 tinted(float2 q) {
    float4 x = float4(blurred.eval(q + float2(blurX, blurY)));
    float t = ca * ((x.r + x.g) / 255);
    return float4(float3(r, g, b) * t, t);
}
float4 main(float2 position) {
    // The pixel's centre exactly: the position a draw interpolates along a
    // triangle's edge can land an ulp off it, and an edge's test then tips.
    float2 p = floor(position) + 0.5 - float2(outX, outY);
    float2 at = p - float2(ox, oy);
    float2 edge = floor(float2(ox, oy) + 0.5);
    float2 end = floor(float2(ox + w, oy + h) + 0.5);
    float2 px = p - 0.5;
    float4 d;
    if (bilinear == 0) {
        d = tinted(at);
    } else {
        float2 u = at - 0.5;
        float2 f = fract(u);
        float2 lo = float2(0.5);
        float2 hi = float2(w, h) - 0.5;
        float2 c0 = clamp(floor(u) + 0.5, lo, hi);
        float2 c1 = clamp(floor(u) + 1.5, lo, hi);
        d = (1 - f.x) * (1 - f.y) * tinted(c0)
          + f.x * (1 - f.y) * tinted(float2(c1.x, c0.y))
          + (1 - f.x) * f.y * tinted(float2(c0.x, c1.y))
          + f.x * f.y * tinted(c1);
    }
    // Masked rather than branched on, so that every pixel reads its
    // textures alike.
    float2 inside = step(edge, px) * (float2(1) - step(end, px));
    d *= inside.x * inside.y;
    float2 sp = p;
    float4 read = (sp.x < srcL || sp.y < srcT || sp.x >= srcR || sp.y >= srcB)
        ? float4(0) : float4(src.eval(sp + float2(srcX, srcY)));
    float4 s = (1 - shadowOnly) * floor(read * 255 + 0.5) / 255;
    float4 v = s + d * (1 - s.a);
    return clamp(floor(v * 255 * gain + 0.5), 0, 255) / 255;
}
"#;

/// How many floats [`SHADOW`] declares as uniforms, in the order it declares
/// them: `r`, `g`, `b`, `ca`, `ox`, `oy`, `bilinear`, `gain`, `w`, `h`,
/// `shadowOnly`, then where the lane writes (`outX`, `outY`), where its blur
/// lies in the blurred child (`blurX`, `blurY`), where its pixel `(0, 0)` lies
/// in the source (`srcX`, `srcY`), and what of the source it reads (`srcL`,
/// `srcT`, `srcR`, `srcB`, in its own pixels).
const SHADOW_UNIFORMS: usize = 21;

thread_local! {
    /// [`SHADOW`], compiled once per thread.
    static SHADOW_EFFECT: Option<RuntimeEffect> = RuntimeEffect::make_for_shader(SHADOW, None).ok();
}

/// One chain's drop shadow in a batch -- see [`shadows`]: its lane of the
/// source, which is the chain's 8-bit input, `size` of it from `from`, of
/// which only `reads` holds the draw; and how its shadow is cast: moved by
/// `offset`, tinted to `rgb` at `alpha` of the blurred alpha, scaled by
/// `gain`, and with `shadow_only`, without the draw over it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ShadowJob {
    pub(crate) from: (i32, i32),
    pub(crate) reads: IRect,
    pub(crate) size: (usize, usize),
    pub(crate) offset: (f32, f32),
    pub(crate) tint: ([f32; 3], f32),
    pub(crate) gain: f32,
    pub(crate) shadow_only: bool,
}

/// `image`, an 8-bit layer `(width, height)`, over its drop shadow, made on
/// `context`: [`shadows`] of one chain reading all of `image`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn shadow(
    context: &mut RecordingContext,
    image: &Image,
    (width, height): (usize, usize),
    sigma: (f32, f32),
    offset: (f32, f32),
    tint: ([f32; 3], f32),
    gain: f32,
    shadow_only: bool,
) -> Option<Image> {
    let job = ShadowJob {
        from: (0, 0),
        reads: IRect::from_wh(width as i32, height as i32),
        size: (width, height),
        offset,
        tint,
        gain,
        shadow_only,
    };
    shadows(context, image, sigma, &[job]).map(|(image, _)| image)
}

/// Each of `jobs` over its drop shadow, made on `context` from `source`, an
/// 8-bit image holding every job's draw, all of them blurred by the same
/// `(sigma_x, sigma_y)`: each one's alpha blurred along rows and columns by
/// the passes [`Gpu::axis`] makes, its value and remainder packed, tinted,
/// moved and composited under the draw by [`SHADOW`], scaled by its gain and
/// rounded once to `source`'s own 8-bit type. One target per pass serves
/// every job, each writing into a place of its own and reading only its own
/// lane, so a run of shadowed draws pays the passes of one. The bytes are a
/// job's as made alone, which are those the CPU makes in floats but on
/// exact ties. The target and where each job's result lies in it; `None`
/// for a source not 8 bits a channel, a blur reaching past a job's lines by
/// more than [`REACH_PER_LINE`] times their length, results more than one
/// target can hold, and wherever a pass gives `None` -- all of which each
/// job then takes alone, or the CPU.
pub(crate) fn shadows(
    context: &mut RecordingContext,
    source: &Image,
    (sigma_x, sigma_y): (f32, f32),
    jobs: &[ShadowJob],
) -> Option<(Image, Vec<(i32, i32)>)> {
    let out_type = source.color_type();
    if !matches!(out_type, ColorType::RGBA8888 | ColorType::BGRA8888)
        || jobs.is_empty()
    {
        return None;
    }
    let reaches = |sigma: f32, n: usize| {
        f64::from(sigma) * REACH_SIGMAS > (n * REACH_PER_LINE) as f64
    };
    if jobs
        .iter()
        .any(|job| reaches(sigma_x, job.size.0) || reaches(sigma_y, job.size.1))
    {
        return None;
    }
    let effect = EFFECT.with(Clone::clone)?;
    let whole = WHOLE_EFFECT.with(Clone::clone)?;
    let compose = SHADOW_EFFECT.with(Clone::clone)?;
    let space = source
        .color_space()
        .or_else(|| Some(ColorSpace::new_srgb()))?;
    // The shadow needs the alpha alone, so each pass carries just its value
    // and remainder, in one target -- half the passes of four channels'
    // two, and the same sums in the same order.
    let zero = zero(context)?;
    let mut gpu = Gpu {
        context,
        effect,
        whole,
        space,
        zero: zero.clone(),
        packed: true,
    };
    let lanes: Vec<Lane> = jobs
        .iter()
        .map(|job| Lane {
            from: job.from,
            reads: job.reads,
            size: job.size,
        })
        .collect();
    let rows_lines: Vec<(usize, usize)> =
        jobs.iter().map(|job| (job.size.0, job.size.1)).collect();
    let (rows, rows_lanes) = gpu.axis(
        (source.clone(), zero),
        Input::Bytes,
        [1.0, 0.0],
        &lanes,
        &rows_lines,
        sigma_x,
        None,
    )?;
    let column_lines: Vec<(usize, usize)> =
        jobs.iter().map(|job| (job.size.1, job.size.0)).collect();
    let ((blurred, _), blurred_lanes) = gpu.axis(
        rows,
        Input::Packed,
        [0.0, 1.0],
        &rows_lanes,
        &column_lines,
        sigma_y,
        None,
    )?;
    let sizes: Vec<(usize, usize)> = jobs.iter().map(|job| job.size).collect();
    let max = gpu.context.max_render_target_size().max(1) as usize;
    let ((width, height), places) = shelves(&sizes, max)?;
    let nearest = SamplingOptions::new(FilterMode::Nearest, MipmapMode::None);
    let child = |image: &Image| {
        image
            .to_shader((TileMode::Decal, TileMode::Decal), nearest, None)
            .map(ChildPtr::from)
    };
    let (blurred, source_child) = (child(&blurred)?, child(source)?);
    let info = gpu.info(width, height, out_type);
    #[cfg(test)]
    crate::context::chain::opened_offscreen();
    let mut surface = surfaces::render_target(
        gpu.context,
        Budgeted::Yes,
        &info,
        None,
        SurfaceOrigin::TopLeft,
        None,
        false,
        None,
    )?;
    for ((job, blur), &(x, y)) in jobs.iter().zip(&blurred_lanes).zip(&places) {
        let ((ox, oy), ([r, g, b], alpha)) = (job.offset, job.tint);
        let whole = ox.fract() == 0.0 && oy.fract() == 0.0;
        let uniforms: [f32; SHADOW_UNIFORMS] = [
            r,
            g,
            b,
            alpha,
            ox,
            oy,
            if whole { 0.0 } else { 1.0 },
            job.gain,
            job.size.0 as f32,
            job.size.1 as f32,
            if job.shadow_only { 1.0 } else { 0.0 },
            x as f32,
            y as f32,
            blur.from.0 as f32,
            blur.from.1 as f32,
            job.from.0 as f32,
            job.from.1 as f32,
            job.reads.left as f32,
            job.reads.top as f32,
            job.reads.right as f32,
            job.reads.bottom as f32,
        ];
        let uniforms: Vec<u8> =
            uniforms.iter().flat_map(|v| v.to_ne_bytes()).collect();
        let shader = compose.make_shader(
            Data::new_copy(&uniforms),
            &[blurred.clone(), source_child.clone()],
            None,
        )?;
        let mut paint = Paint::default();
        paint.set_shader(shader).set_blend_mode(BlendMode::Src);
        surface.canvas().draw_rect(
            Rect::from_xywh(
                x as f32,
                y as f32,
                job.size.0 as f32,
                job.size.1 as f32,
            ),
            &paint,
        );
    }
    Some((surface.image_snapshot(), places))
}

thread_local! {
    /// The last [`zero`] made on this thread, kept while its context lives.
    static ZERO: RefCell<Option<Image>> = const { RefCell::new(None) };
}

/// A 1x1 transparent image on `context`, the remainder child of a pass
/// whose input has none: made once per thread and context and kept, rather
/// than a target made and cleared for every blur. Transparent reads as
/// zero in any space, so whose space it is tagged with moves nothing.
fn zero(context: &mut RecordingContext) -> Option<Image> {
    if let Some(kept) = ZERO.with(|z| z.borrow().clone())
        && kept.is_valid(Some(context.as_recorder() as &mut dyn Recorder))
    {
        return Some(kept);
    }
    let info = ImageInfo::new(
        (1, 1),
        ColorType::RGBAF16,
        AlphaType::Premul,
        ColorSpace::new_srgb(),
    );
    let mut surface = surfaces::render_target(
        context,
        Budgeted::Yes,
        &info,
        None,
        SurfaceOrigin::TopLeft,
        None,
        false,
        None,
    )?;
    surface.canvas().clear(Color::TRANSPARENT);
    let made = surface.image_snapshot();
    ZERO.with(|z| *z.borrow_mut() = Some(made.clone()));
    Some(made)
}

/// The passes' shared state.
struct Gpu<'a> {
    context: &'a mut RecordingContext,
    effect: RuntimeEffect,
    /// [`WHOLE_EFFECT`]: `effect` for a pass of one lane reading all of its
    /// input.
    whole: RuntimeEffect,
    space: ColorSpace,
    /// A 1x1 transparent image, the second child of a pass whose input has
    /// no remainder of its own.
    zero: Image,
    /// Whether the passes carry an alpha alone, its value and remainder
    /// packed into one target -- see [`Part::Packed`] -- rather than four
    /// channels in two.
    packed: bool,
}

/// One chain's share of a pass that may serve several: where its pixel
/// `(0, 0)` lies in the pass's children, what of them it reads, and how
/// large its output is. A pass writes each lane's output into a place of its
/// own in one target, so that the chains of a run pay one pass between them
/// rather than one each -- see [`shadows`].
#[derive(Clone, Copy, Debug, PartialEq)]
struct Lane {
    /// Where the lane's own pixel `(0, 0)` lies in the children.
    from: (i32, i32),
    /// What the lane reads, in its own pixels; the rest reads as nothing,
    /// as outside a texture of its own does.
    reads: IRect,
    /// The output's width and height.
    size: (usize, usize),
}

impl Lane {
    /// A lane reading all of an image of `size` at the children's origin:
    /// one chain's pass, as a pass with one lane makes it.
    fn whole(size: (usize, usize)) -> Self {
        Lane {
            from: (0, 0),
            reads: IRect::from_wh(size.0 as i32, size.1 as i32),
            size,
        }
    }
}

/// A target's size and where in it each of the sizes [`shelves`] was given
/// goes.
pub(crate) type Shelves = ((usize, usize), Vec<(i32, i32)>);

/// Where each of `sizes` goes in one target: left to right in rows no wider
/// than `max`, a row as tall as its tallest, the rows top to bottom. The
/// target's size and each place, or `None` where they fill more than `max`
/// down -- which the caller then makes in more than one target, or leaves
/// to each chain alone. One size is placed at the origin of a target its
/// own size.
pub(crate) fn shelves(sizes: &[(usize, usize)], max: usize) -> Option<Shelves> {
    let (mut x, mut y, mut row, mut width) = (0usize, 0usize, 0usize, 0usize);
    let mut places = Vec::with_capacity(sizes.len());
    for &(w, h) in sizes {
        if w > max || h > max {
            return None;
        }
        if x + w > max {
            y += row;
            x = 0;
            row = 0;
        }
        places.push((x as i32, y as i32));
        x += w;
        row = row.max(h);
        width = width.max(x);
    }
    let height = y + row;
    (height <= max).then_some(((width.max(1), height.max(1)), places))
}

impl Gpu<'_> {
    fn info(&self, w: usize, h: usize, ct: ColorType) -> ImageInfo {
        ImageInfo::new(
            (w as i32, h as i32),
            ct,
            AlphaType::Premul,
            self.space.clone(),
        )
    }

    /// One pass: `children` read as `input` by each of `lanes`, along
    /// `dir`, each lane's output written into a place of its own in one
    /// target of `ct`, writing `part`. The target and each lane's output, as
    /// a lane the next pass reads. `None` for a pass of more taps than
    /// [`PASS`]'s loops take, and where the outputs fill more than one
    /// target can hold.
    #[allow(clippy::too_many_arguments)]
    fn pass(
        &mut self,
        children: (&Image, &Image),
        input: Input,
        dir: [f32; 2],
        taps: Taps,
        part: Part,
        lanes: &[Lane],
        ct: ColorType,
    ) -> Option<(Image, Vec<Lane>)> {
        if taps.count() > MAX_TAPS {
            return None;
        }
        let sizes: Vec<(usize, usize)> =
            lanes.iter().map(|lane| lane.size).collect();
        let max = self.context.max_render_target_size().max(1) as usize;
        let ((w, h), places) = shelves(&sizes, max)?;
        let info = self.info(w, h, ct);
        #[cfg(test)]
        crate::context::chain::opened_offscreen();
        let mut surface = surfaces::render_target(
            self.context,
            Budgeted::Yes,
            &info,
            None,
            SurfaceOrigin::TopLeft,
            None,
            false,
            None,
        )?;
        let nearest =
            SamplingOptions::new(FilterMode::Nearest, MipmapMode::None);
        let child = |image: &Image| {
            image
                .to_shader((TileMode::Decal, TileMode::Decal), nearest, None)
                .map(ChildPtr::from)
        };
        let (hi, lo) = (child(children.0)?, child(children.1)?);
        // One lane reading all of its input is held to its box by the
        // input's edge, which reads zero past it as the box does.
        let effect = match lanes {
            [lane]
                if lane.from == (0, 0)
                    && lane.reads
                        == IRect::from_size(children.0.dimensions()) =>
            {
                &self.whole
            }
            _ => &self.effect,
        };
        for (lane, &(x, y)) in lanes.iter().zip(&places) {
            let uniforms: Vec<u8> = [
                dir[0],
                dir[1],
                taps.mode,
                taps.spacing,
                taps.origin,
                taps.reach as f32,
                (1.0 / (2.0 * taps.sigma * taps.sigma)) as f32,
                taps.norm as f32,
                part as u8 as f32,
                input as u8 as f32,
                x as f32,
                y as f32,
                lane.from.0 as f32,
                lane.from.1 as f32,
                lane.reads.left as f32,
                lane.reads.top as f32,
                lane.reads.right as f32,
                lane.reads.bottom as f32,
            ]
            .iter()
            .flat_map(|v| v.to_ne_bytes())
            .collect();
            let shader = effect.make_shader(
                Data::new_copy(&uniforms),
                &[hi.clone(), lo.clone()],
                None,
            )?;
            let mut paint = Paint::default();
            paint.set_shader(shader).set_blend_mode(BlendMode::Src);
            surface.canvas().draw_rect(
                Rect::from_xywh(
                    x as f32,
                    y as f32,
                    lane.size.0 as f32,
                    lane.size.1 as f32,
                ),
                &paint,
            );
        }
        let outputs = lanes
            .iter()
            .zip(places)
            .map(|(lane, from)| Lane {
                from,
                reads: IRect::from_wh(lane.size.0 as i32, lane.size.1 as i32),
                size: lane.size,
            })
            .collect();
        Some((surface.image_snapshot(), outputs))
    }

    /// What a pass after the first reads: a value and its remainder, in
    /// two targets or packed into one.
    fn carried(&self) -> Input {
        match self.packed {
            true => Input::Packed,
            false => Input::Split,
        }
    }

    /// Both parts of a pass, as half floats: in two targets, or packed into
    /// one beside the [`zero`] a next pass reads as its second child. The
    /// two targets of a split pass place each lane alike, so one set of
    /// lanes reads both.
    #[allow(clippy::type_complexity)]
    fn split(
        &mut self,
        children: (&Image, &Image),
        input: Input,
        dir: [f32; 2],
        taps: Taps,
        lanes: &[Lane],
    ) -> Option<((Image, Image), Vec<Lane>)> {
        if self.packed {
            let (packed, outputs) = self.pass(
                children,
                input,
                dir,
                taps,
                Part::Packed,
                lanes,
                ColorType::RGBAF16,
            )?;
            return Some(((packed, self.zero.clone()), outputs));
        }
        let (value, outputs) = self.pass(
            children,
            input,
            dir,
            taps,
            Part::Value,
            lanes,
            ColorType::RGBAF16,
        )?;
        let (remainder, _) = self.pass(
            children,
            input,
            dir,
            taps,
            Part::Remainder,
            lanes,
            ColorType::RGBAF16,
        )?;
        Some(((value, remainder), outputs))
    }

    /// One axis for each of `lanes`, every lane blurred by the same `sigma`
    /// along `dir`: lane `i` is `lines[i].0` samples along it across
    /// `lines[i].1` lines, read from `children` as `input`. Ends in a value
    /// and its remainder, or, with `last`, in the one image `last` names
    /// (its second slot then a copy of it), and each lane's output in them.
    #[allow(clippy::too_many_arguments, clippy::type_complexity)]
    fn axis(
        &mut self,
        children: (Image, Image),
        input: Input,
        dir: [f32; 2],
        lanes: &[Lane],
        lines: &[(usize, usize)],
        sigma: f32,
        last: Option<(Part, ColorType, f64)>,
    ) -> Option<((Image, Image), Vec<Lane>)> {
        let size = |(len, other): (usize, usize)| {
            if dir[0] == 1.0 {
                (len, other)
            } else {
                (other, len)
            }
        };
        // `lanes` as read, each writing `len` samples along the axis.
        let sized =
            |lanes: &[Lane], len: &dyn Fn(usize) -> usize| -> Vec<Lane> {
                lanes
                    .iter()
                    .zip(lines)
                    .map(|(lane, &(n, other))| Lane {
                        size: size((len(n), other)),
                        ..*lane
                    })
                    .collect()
            };
        let finish = |gpu: &mut Self,
                      from: (&Image, &Image),
                      input: Input,
                      taps: Taps,
                      lanes: Vec<Lane>| match last {
            Some((part, ct, gain)) => {
                let taps = Taps {
                    norm: taps.norm * gain,
                    ..taps
                };
                gpu.pass(from, input, dir, taps, part, &lanes, ct)
                    .map(|(image, outputs)| ((image.clone(), image), outputs))
            }
            None => gpu.split(from, input, dir, taps, &lanes),
        };
        if !(sigma.is_finite() && sigma > 0.0) {
            // No blur along this axis: a pass of one tap carries the input.
            let taps = Taps {
                mode: 1.0,
                spacing: 1.0,
                origin: 0.0,
                reach: 0,
                sigma: 1.0,
                norm: 1.0,
            };
            let lanes = sized(lanes, &|n| n);
            return finish(
                self,
                (&children.0, &children.1),
                input,
                taps,
                lanes,
            );
        }
        let sigma = f64::from(sigma);
        let reach = |sg: f64| (REACH_SIGMAS * sg).ceil() as usize;
        let Some(s) = factor(sigma as f32) else {
            let taps = Taps {
                mode: 1.0,
                spacing: 1.0,
                origin: 0.0,
                reach: reach(sigma),
                sigma,
                norm: norm_of(sigma),
            };
            let lanes = sized(lanes, &|n| n);
            return finish(
                self,
                (&children.0, &children.1),
                input,
                taps,
                lanes,
            );
        };
        let a = s as f64;
        let b = (sigma * sigma - 2.0 * a * a).sqrt() / a;
        let (ra, rb, rc) = (reach(a), reach(b), reach(a));
        // The coarse grid starts before the line by enough that every coarse
        // sample whose blur reaches the line exists.
        let margin = (ra + rb * s + rc + s).div_ceil(s) * s;
        let origin = -(margin as f32);
        let coarse = |n: usize| (n + 2 * margin) / s + 1;
        let ((h0, l0), first) = self.split(
            (&children.0, &children.1),
            input,
            dir,
            Taps {
                mode: 0.0,
                spacing: s as f32,
                origin,
                reach: ra,
                sigma: a,
                norm: norm_of(a),
            },
            &sized(lanes, &coarse),
        )?;
        let ((h1, l1), second) = self.split(
            (&h0, &l0),
            self.carried(),
            dir,
            Taps {
                mode: 1.0,
                spacing: s as f32,
                origin,
                reach: rb,
                sigma: b,
                norm: norm_of(b),
            },
            &sized(&first, &coarse),
        )?;
        // The sum back carries the factor: each coarse sample stands for `s`.
        let up = Taps {
            mode: 2.0,
            spacing: s as f32,
            origin,
            reach: rc,
            sigma: a,
            norm: a * norm_of(a),
        };
        let lanes = sized(&second, &|n| n);
        finish(self, (&h1, &l1), self.carried(), up, lanes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        context::{exact_blur::reference, page::ExportOptions},
        gpu::RenderingEngine,
        started,
    };
    use skia_safe::Surface;

    /// Float noise moves a value by well under this; a byte that differs
    /// from the reference further from a half-level than this is wrong.
    const TIE: f64 = 1e-3;

    /// A GPU surface `w` by `h` of `ct`, or `None` without a GPU.
    fn surface(w: usize, h: usize, ct: ColorType) -> Option<Surface> {
        if !RenderingEngine::GPU.selectable() {
            return None;
        }
        let info = ImageInfo::new(
            (w as i32, h as i32),
            ct,
            AlphaType::Premul,
            ColorSpace::new_srgb(),
        );
        RenderingEngine::GPU
            .make_surface(&info, &ExportOptions::default())
            .ok()
    }

    #[test]
    fn the_shadow_pass_compiles() {
        // As for the blur's pass: a compile error reads as no GPU shadow,
        // and the CPU makes every one.
        let effect = RuntimeEffect::make_for_shader(SHADOW, None);
        assert!(SHADOW_EFFECT.with(Option::is_some), "{:?}", effect.err());
        let effect = effect.expect("compiled");
        assert_eq!(effect.uniform_size(), SHADOW_UNIFORMS * size_of::<f32>());
    }

    #[test]
    fn the_pass_compiles() {
        // A compile error otherwise reads as no blur at all: `blur` returns
        // `None`, which a caller takes for "no GPU".
        for (effect, boxed) in [(&EFFECT, true), (&WHOLE_EFFECT, false)] {
            assert!(
                effect.with(Option::is_some),
                "boxed {boxed}: {:?}",
                RuntimeEffect::make_for_shader(source(boxed), None).err()
            );
        }
    }

    /// `src`, premultiplied RGBA bytes, blurred on the GPU and read back.
    fn on_gpu(
        src: &[u8],
        w: usize,
        h: usize,
        sx: f32,
        sy: f32,
    ) -> Option<Vec<u8>> {
        let info = ImageInfo::new(
            (w as i32, h as i32),
            ColorType::RGBA8888,
            AlphaType::Premul,
            ColorSpace::new_srgb(),
        );
        let raster =
            started::raster_from_data(&info, Data::new_copy(src), w * 4)?;
        let mut up = surface(w, h, ColorType::RGBA8888)?;
        let mut paint = Paint::default();
        paint.set_blend_mode(BlendMode::Src);
        up.canvas().draw_image(&raster, (0, 0), Some(&paint));
        let texture = up.image_snapshot();
        let mut context = up.recording_context()?;
        let out = blur(
            &mut context,
            &texture,
            (w, h),
            sx,
            sy,
            ColorType::RGBA8888,
            1.0,
        )?;
        let mut down = surface(w, h, ColorType::RGBA8888)?;
        down.canvas().draw_image(&out, (0, 0), Some(&paint));
        let mut px = vec![0u8; w * h * 4];
        down.read_pixels(&info, &mut px, w * 4, (0, 0))
            .then_some(px)
    }

    fn noise(w: usize, h: usize) -> Vec<u8> {
        let mut s = 7u32;
        (0..w * h)
            .flat_map(|i| {
                s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let a = if i % w < w / 2 { (s >> 24) as u8 } else { 255 };
                s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let c = (u32::from((s >> 24) as u8) * u32::from(a) / 255) as u8;
                [c, c / 2, c / 3, a]
            })
            .collect()
    }

    /// Bytes off the reference, and the farthest of them from a tie.
    fn off(got: &[u8], want: &[f64]) -> (usize, f64) {
        got.iter().zip(want).fold((0, 0f64), |(n, far), (&g, &e)| {
            if f64::from(g) != (e + 0.5).floor() {
                (n + 1, far.max((e - e.floor() - 0.5).abs()))
            } else {
                (n, far)
            }
        })
    }

    #[test]
    fn a_gpu_blur_is_the_exact_gaussian_except_on_ties() {
        let (w, h) = (96, 72);
        let src = noise(w, h);
        // Direct below the smallest factor, decimated from it, and each axis
        // its own way.
        for (sx, sy) in [
            (0.5, 0.5),
            (2.0, 2.0),
            (4.0, 4.0),
            (6.0, 6.0),
            (12.0, 12.0),
            (40.0, 40.0),
            (2.0, 20.0),
            (20.0, 2.0),
        ] {
            if !RenderingEngine::GPU.selectable() {
                eprintln!("skipped: no GPU");
                return;
            }
            let got = on_gpu(&src, w, h, sx, sy)
                .expect("a GPU blur where a GPU is selectable");
            let (n, far) = off(&got, &reference(&src, w, h, sx, sy));
            assert!(
                far < TIE,
                "({sx}, {sy}): {n} bytes off, one {far} from a tie"
            );
        }
    }

    #[test]
    fn a_pass_past_the_shaders_loop_is_left_to_the_cpu() {
        if !RenderingEngine::GPU.selectable() {
            eprintln!("skipped: no GPU");
            return;
        }
        // Factors 1357 and 1392, so the first pass takes 16285 taps, within
        // the loop, and 16705, past it. Both reach past the line by less
        // than `REACH_PER_LINE` times its length.
        let (fits, past) = (3900.0f32, 4000.0f32);
        let w = 6000;
        let src: Vec<u8> = [255u8, 0, 0, 255].repeat(w);
        let got = on_gpu(&src, w, 1, fits, 0.0)
            .expect("a GPU blur where a GPU is selectable");
        // The line is uniform, so each pixel's blur is the share of the
        // sampled Gaussian over the line: a difference of its running sum,
        // where `reference` would take the taps one by one.
        let sigma = f64::from(fits);
        let r = (8.0 * sigma).ceil() as i64;
        let mut running = vec![0f64];
        for d in -r..=r {
            let weight = (-((d * d) as f64) / (2.0 * sigma * sigma)).exp();
            running.push(running[running.len() - 1] + weight);
        }
        let total = running[running.len() - 1];
        let upto = |d: i64| running[(d.clamp(-r - 1, r) + r + 1) as usize];
        let want: Vec<f64> = (0..w as i64)
            .flat_map(|x| {
                let v = 255.0 * (upto(x) - upto(x - w as i64)) / total;
                [v, 0.0, 0.0, v]
            })
            .collect();
        let (n, far) = off(&got, &want);
        assert!(
            far < TIE,
            "sigma {fits}: {n} bytes off, one {far} from a tie"
        );
        assert!(
            on_gpu(&src, w, 1, past, 0.0).is_none(),
            "a pass past the loop was made on the GPU"
        );
    }

    #[test]
    fn decimation_keeps_its_coarse_blur_wide_enough() {
        assert_eq!(factor(5.0), None);
        assert_eq!(factor(6.0), Some(2));
        assert_eq!(factor(40.0), Some(13));
        for sigma in [6.0f32, 12.0, 40.0, 100.0, 1000.0] {
            let s = f64::from(factor(sigma).expect("a factor") as u32);
            let b = (f64::from(sigma).powi(2) - 2.0 * s * s).sqrt();
            assert!(b / s >= MIN_COARSE, "sigma {sigma}: b/s {}", b / s);
        }
    }

    /// `src`, premultiplied RGBA bytes `w` by `h`, as a GPU image, with the
    /// surface it was drawn on for its context.
    fn uploaded(src: &[u8], w: usize, h: usize) -> Option<(Surface, Image)> {
        let info = ImageInfo::new(
            (w as i32, h as i32),
            ColorType::RGBA8888,
            AlphaType::Premul,
            ColorSpace::new_srgb(),
        );
        let raster =
            started::raster_from_data(&info, Data::new_copy(src), w * 4)?;
        let mut up = surface(w, h, ColorType::RGBA8888)?;
        let mut paint = Paint::default();
        paint.set_blend_mode(BlendMode::Src);
        up.canvas().draw_image(&raster, (0, 0), Some(&paint));
        let image = up.image_snapshot();
        Some((up, image))
    }

    /// `area` of `image`, a GPU image, read back as RGBA bytes.
    fn read_back(image: &Image, area: IRect) -> Option<Vec<u8>> {
        let (w, h) = (area.width() as usize, area.height() as usize);
        let mut down = surface(w, h, ColorType::RGBA8888)?;
        let mut paint = Paint::default();
        paint.set_blend_mode(BlendMode::Src);
        down.canvas()
            .draw_image(image, (-area.left, -area.top), Some(&paint));
        let info = ImageInfo::new(
            (w as i32, h as i32),
            ColorType::RGBA8888,
            AlphaType::Premul,
            ColorSpace::new_srgb(),
        );
        let mut px = vec![0u8; w * h * 4];
        down.read_pixels(&info, &mut px, w * 4, (0, 0))
            .then_some(px)
    }

    /// Jobs sharing passes each make the shadow they make alone from their
    /// own lane, with what lies past it cleared: where the lanes adjoin and
    /// overlap in the source, where a lane reads less than all of itself,
    /// and where ink runs to a lane's edge, so that every read past a lane
    /// -- the passes' of the source and of each other's output, the
    /// compose's of the blur moved by the offset and of the draw -- picks
    /// up another's ink. A page cannot show all of this: chains share a
    /// layer only where none paints within another's reach, which leaves a
    /// lane's edge clear.
    #[test]
    fn jobs_sharing_passes_read_only_their_own_lanes() {
        if !RenderingEngine::GPU.selectable() {
            eprintln!("skipped: no GPU");
            return;
        }
        let (w, h) = (96, 64);
        let src = noise(w, h);
        let job = |from, reads, size, offset, shadow_only| ShadowJob {
            from,
            reads,
            size,
            offset,
            tint: ([0.1, 0.3, 0.5], 0.6),
            gain: 0.8,
            shadow_only,
        };
        let jobs = [
            job(
                (0, 0),
                IRect::new(5, 4, 35, 26),
                (40, 30),
                (3.0, 2.0),
                false,
            ),
            job(
                (40, 0),
                IRect::new(0, 0, 40, 30),
                (40, 30),
                (-4.0, -1.0),
                false,
            ),
            job(
                (20, 24),
                IRect::new(0, 0, 50, 40),
                (50, 40),
                (2.5, -3.5),
                true,
            ),
            job(
                (60, 20),
                IRect::new(2, 0, 36, 44),
                (36, 44),
                (-1.5, 4.0),
                false,
            ),
        ];
        // Direct and decimated, and each axis its own way.
        for sigma in [(2.0, 3.0), (7.0, 6.5)] {
            let (up, source) = uploaded(&src, w, h).expect("an upload");
            let mut context = up.recording_context().expect("a context");
            let (shared, places) = shadows(&mut context, &source, sigma, &jobs)
                .expect("shared passes");
            for (i, (job, &(x, y))) in jobs.iter().zip(&places).enumerate() {
                let (jw, jh) = job.size;
                // The job's lane, all but what it reads cleared.
                let lane: Vec<u8> = (0..jw * jh)
                    .flat_map(|k| {
                        let (lx, ly) = ((k % jw) as i32, (k / jw) as i32);
                        let (sx, sy) = (lx + job.from.0, ly + job.from.1);
                        let reads = job.reads;
                        match (reads.left..reads.right).contains(&lx)
                            && (reads.top..reads.bottom).contains(&ly)
                            && (0..w as i32).contains(&sx)
                            && (0..h as i32).contains(&sy)
                        {
                            true => {
                                let at = (sy as usize * w + sx as usize) * 4;
                                [src[at], src[at + 1], src[at + 2], src[at + 3]]
                            }
                            false => [0; 4],
                        }
                    })
                    .collect();
                let (up, alone) = uploaded(&lane, jw, jh).expect("an upload");
                let mut context = up.recording_context().expect("a context");
                let alone = shadow(
                    &mut context,
                    &alone,
                    (jw, jh),
                    sigma,
                    job.offset,
                    job.tint,
                    job.gain,
                    job.shadow_only,
                )
                .expect("a shadow alone");
                let want =
                    read_back(&alone, IRect::from_wh(jw as i32, jh as i32))
                        .expect("read back");
                let got = read_back(
                    &shared,
                    IRect::from_xywh(x, y, jw as i32, jh as i32),
                )
                .expect("read back");
                let differ =
                    got.iter().zip(&want).filter(|(a, b)| a != b).count();
                assert_eq!(
                    differ, 0,
                    "sigma {sigma:?}, job {i}: bytes apart from alone"
                );
                assert!(
                    want.chunks(4).any(|p| p[3] > 0),
                    "sigma {sigma:?}, job {i}: drew nothing"
                );
            }
        }
    }
}
