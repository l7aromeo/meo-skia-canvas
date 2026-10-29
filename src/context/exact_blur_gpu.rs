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
//!   of levels, which together hold it to about 22 bits.
//! - A child shader returns half precision, which cannot hold `n / 255`
//!   exactly. An 8-bit source's byte is recovered as the nearest level, which
//!   the half's error, under 0.06 of a level, cannot cross.
//! - The fragment reaches an 8-bit target through half precision, which cannot
//!   hold a level sum to better than 0.06 either. The last pass onto an 8-bit
//!   target rounds to a whole level itself, which survives.

use skia_safe::{
    AlphaType, BlendMode, Color, ColorSpace, ColorType, Data, FilterMode,
    Image, ImageInfo, MipmapMode, Paint, Rect, RuntimeEffect, SamplingOptions,
    TileMode,
    gpu::{Budgeted, RecordingContext, SurfaceOrigin, surfaces},
    runtime_effect::ChildPtr,
};

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
/// levels; 1, an 8-bit image, whose byte is recovered; 2, a float image.
/// `part` says what to write: 1 and 2, the value and its remainder in
/// levels; 0, the value rounded to a level, for an 8-bit target; 3, the
/// value unrounded, for a float one. Outside a texture, decal reads zero.
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
float4 at(float2 q) {
    float4 v = float4(hi.eval(q)) + float4(lo.eval(q));
    if (source == 1) { return floor(v * 255 + 0.5); }
    if (source == 2) { return v * 255; }
    return v;
}
float4 main(float2 p) {
    float2 across = p * (float2(1) - dir);
    float o = dot(p, dir) - 0.5;
    float4 acc = float4(0);
    if (mode < 2) {
        float centre = mode == 0 ? origin + o * spacing : o;
        for (int i = 0; i < 16384; i++) {
            float d = float(i) - reach;
            if (d > reach) { break; }
            acc += exp(-d * d * c) * at(across + dir * (centre - d + 0.5));
        }
    } else {
        float j = ceil((o - reach - origin) / spacing);
        for (int i = 0; i < 16384; i++) {
            float d = o - (origin + j * spacing);
            if (d < -reach) { break; }
            acc += exp(-d * d * c) * at(across + dir * (j + 0.5));
            j += 1;
        }
    }
    acc *= norm;
    if (part == 0) { return floor(acc + 0.5) / 255; }
    if (part == 3) { return acc / 255; }
    float4 h = float4(half4(acc));
    return part == 1 ? h : acc - h;
}
"#;

thread_local! {
    /// [`PASS`], compiled once per thread.
    static EFFECT: Option<RuntimeEffect> = RuntimeEffect::make_for_shader(PASS, None).ok();
}

/// What a pass's children hold, as [`PASS`]'s `source` uniform reads it.
#[derive(Clone, Copy)]
enum Input {
    Split = 0,
    Bytes = 1,
    Floats = 2,
}

/// What a pass writes, as [`PASS`]'s `part` uniform reads it.
#[derive(Clone, Copy)]
enum Part {
    Rounded = 0,
    Value = 1,
    Remainder = 2,
    Unrounded = 3,
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

/// The blur of `image`, `width` by `height`, by `sigma_x` along rows and
/// `sigma_y` along columns, made on `context`, written as `out_type`
/// pixels: rounded to a level for an 8-bit type, unrounded for a float one.
/// `None` where the effect does not compile or a target cannot be made, and
/// for a blur that reaches past the image along either axis by more than
/// [`REACH_PER_LINE`] times its length, which the CPU blur takes.
pub(crate) fn blur(
    context: &mut RecordingContext,
    image: &Image,
    width: usize,
    height: usize,
    sigma_x: f32,
    sigma_y: f32,
    out_type: ColorType,
) -> Option<Image> {
    // An infinite sigma reaches past any image, and is the CPU's as well.
    let reaches = |sigma: f32, n: usize| {
        f64::from(sigma) * REACH_SIGMAS > (n * REACH_PER_LINE) as f64
    };
    if reaches(sigma_x, width) || reaches(sigma_y, height) {
        return None;
    }
    let effect = EFFECT.with(Clone::clone)?;
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
    let mut gpu = Gpu {
        context,
        effect,
        space,
    };
    let zero = gpu.zero()?;
    // Along rows, into a value and its remainder; then along columns, into
    // the target.
    let (hi, lo) = gpu.axis(
        (image.clone(), zero.clone()),
        first,
        [1.0, 0.0],
        width,
        height,
        sigma_x,
        None,
    )?;
    let last = if float_out {
        Part::Unrounded
    } else {
        Part::Rounded
    };
    gpu.axis(
        (hi, lo),
        Input::Split,
        [0.0, 1.0],
        height,
        width,
        sigma_y,
        Some((last, out_type)),
    )
    .map(|(image, _)| image)
}

/// The passes' shared state.
struct Gpu<'a> {
    context: &'a mut RecordingContext,
    effect: RuntimeEffect,
    space: ColorSpace,
}

impl Gpu<'_> {
    /// A 1x1 transparent image, the remainder child of a pass whose input
    /// has none.
    fn zero(&mut self) -> Option<Image> {
        let info = self.info(1, 1, ColorType::RGBAF16);
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
        surface.canvas().clear(Color::TRANSPARENT);
        Some(surface.image_snapshot())
    }

    fn info(&self, w: usize, h: usize, ct: ColorType) -> ImageInfo {
        ImageInfo::new(
            (w as i32, h as i32),
            ct,
            AlphaType::Premul,
            self.space.clone(),
        )
    }

    /// One pass: `children` read as `input`, along `dir`, into a `w` x `h`
    /// target of `ct`, writing `part`.
    #[allow(clippy::too_many_arguments)]
    fn pass(
        &mut self,
        children: (&Image, &Image),
        input: Input,
        dir: [f32; 2],
        taps: Taps,
        part: Part,
        (w, h): (usize, usize),
        ct: ColorType,
    ) -> Option<Image> {
        let info = self.info(w, h, ct);
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
        ]
        .iter()
        .flat_map(|v| v.to_ne_bytes())
        .collect();
        let nearest =
            SamplingOptions::new(FilterMode::Nearest, MipmapMode::None);
        let child = |image: &Image| {
            image
                .to_shader((TileMode::Decal, TileMode::Decal), nearest, None)
                .map(ChildPtr::from)
        };
        let shader = self.effect.make_shader(
            Data::new_copy(&uniforms),
            &[child(children.0)?, child(children.1)?],
            None,
        )?;
        let mut paint = Paint::default();
        paint.set_shader(shader).set_blend_mode(BlendMode::Src);
        surface
            .canvas()
            .draw_rect(Rect::from_wh(w as f32, h as f32), &paint);
        Some(surface.image_snapshot())
    }

    /// Both parts of a pass, as half floats.
    fn split(
        &mut self,
        children: (&Image, &Image),
        input: Input,
        dir: [f32; 2],
        taps: Taps,
        size: (usize, usize),
    ) -> Option<(Image, Image)> {
        Some((
            self.pass(
                children,
                input,
                dir,
                taps,
                Part::Value,
                size,
                ColorType::RGBAF16,
            )?,
            self.pass(
                children,
                input,
                dir,
                taps,
                Part::Remainder,
                size,
                ColorType::RGBAF16,
            )?,
        ))
    }

    /// One axis of `n` samples across `other` lines, from `children` read as
    /// `input`. Ends in a value and its remainder, or, with `last`, in the
    /// one image `last` names (its second slot then a copy of it).
    #[allow(clippy::too_many_arguments)]
    fn axis(
        &mut self,
        children: (Image, Image),
        input: Input,
        dir: [f32; 2],
        n: usize,
        other: usize,
        sigma: f32,
        last: Option<(Part, ColorType)>,
    ) -> Option<(Image, Image)> {
        let size = |len: usize| {
            if dir[0] == 1.0 {
                (len, other)
            } else {
                (other, len)
            }
        };
        let finish = |gpu: &mut Self,
                      from: (&Image, &Image),
                      input: Input,
                      taps: Taps| match last {
            Some((part, ct)) => gpu
                .pass(from, input, dir, taps, part, size(n), ct)
                .map(|image| (image.clone(), image)),
            None => gpu.split(from, input, dir, taps, size(n)),
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
            return finish(self, (&children.0, &children.1), input, taps);
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
            return finish(self, (&children.0, &children.1), input, taps);
        };
        let a = s as f64;
        let b = (sigma * sigma - 2.0 * a * a).sqrt() / a;
        let (ra, rb, rc) = (reach(a), reach(b), reach(a));
        // The coarse grid starts before the line by enough that every coarse
        // sample whose blur reaches the line exists.
        let margin = (ra + rb * s + rc + s).div_ceil(s) * s;
        let origin = -(margin as f32);
        let coarse = (n + 2 * margin) / s + 1;
        let (h0, l0) = self.split(
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
            size(coarse),
        )?;
        let (h1, l1) = self.split(
            (&h0, &l0),
            Input::Split,
            dir,
            Taps {
                mode: 1.0,
                spacing: s as f32,
                origin,
                reach: rb,
                sigma: b,
                norm: norm_of(b),
            },
            size(coarse),
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
        finish(self, (&h1, &l1), Input::Split, up)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        context::{exact_blur::reference, page::ExportOptions},
        gpu::RenderingEngine,
    };
    use skia_safe::{Surface, images};

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
    fn the_pass_compiles() {
        // A compile error otherwise reads as no blur at all: `blur` returns
        // `None`, which a caller takes for "no GPU".
        assert!(
            EFFECT.with(Option::is_some),
            "{:?}",
            RuntimeEffect::make_for_shader(PASS, None).err()
        );
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
            images::raster_from_data(&info, Data::new_copy(src), w * 4)?;
        let mut up = surface(w, h, ColorType::RGBA8888)?;
        let mut paint = Paint::default();
        paint.set_blend_mode(BlendMode::Src);
        up.canvas().draw_image(&raster, (0, 0), Some(&paint));
        let texture = up.image_snapshot();
        let mut context = up.recording_context()?;
        let out =
            blur(&mut context, &texture, w, h, sx, sy, ColorType::RGBA8888)?;
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
}
