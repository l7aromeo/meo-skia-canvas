#![allow(non_snake_case)]
use neon::prelude::*;
use skia_safe::{
    Color, Color4f, ColorChannel, CubicResampler, FilterMode, IPoint, ISize,
    ImageFilter as SkImageFilter, Matrix, MipmapMode, Point3, SamplingOptions,
    TileMode, image_filters,
};
use std::cell::RefCell;

use crate::{
    color_filter::{BoxedColorFilter, checkDeleted as checkColorFilterDeleted},
    utils::{css_to_color, enum_arg_or, filter_blend_mode_arg},
};

pub type BoxedImageFilter = JsBox<RefCell<ImageFilter>>;
impl Finalize for ImageFilter {}

#[derive(Clone)]
pub struct ImageFilter {
    pub inner: SkImageFilter,
    deleted: bool,
}

impl ImageFilter {
    pub fn is_deleted(&self) -> bool {
        self.deleted
    }
}

/// Wraps an `Option<SkImageFilter>` result: `Some` -> boxed `ImageFilter`,
/// `None` -> null.
macro_rules! wrap_image_filter {
    ($cx:expr, $result:expr) => {
        match $result {
            Some(filter) => {
                let imgf = ImageFilter {
                    inner: filter,
                    deleted: false,
                };
                Ok($cx.boxed(RefCell::new(imgf)).upcast())
            }
            None => Ok($cx.null().upcast()),
        }
    };
}

/// Parses optional input `ImageFilter` from argument at index.
macro_rules! opt_input_filter {
    ($cx:expr, $idx:expr) => {
        match $cx.argument_opt($idx) {
            Some(arg)
                if !arg.is_a::<JsNull, _>($cx)
                    && !arg.is_a::<JsUndefined, _>($cx) =>
            {
                let prev = arg.downcast_or_throw::<BoxedImageFilter, _>($cx)?;
                checkDeleted($cx, &prev)?;
                Some(prev.borrow().inner.clone())
            }
            _ => None,
        }
    };
}

/// Parses color from argument (CSS string or [r,g,b,a] array) -> `Color4f`.
macro_rules! parse_color4f {
    ($cx:expr, $idx:expr, $default:expr) => {
        match $cx.argument_opt($idx) {
            Some(arg) if arg.is_a::<JsString, _>($cx) => {
                let color_str =
                    arg.downcast_or_throw::<JsString, _>($cx)?.value($cx);
                // An unparseable color used to take the default, so a typo in
                // a shadow color drew a black shadow and reported nothing.
                match css_to_color(&color_str) {
                    Some(c) => Color4f::from(c),
                    None => {
                        return $cx.throw_type_error(format!(
                            "Could not parse color \"{color_str}\""
                        ));
                    }
                }
            }
            Some(arg) if arg.is_a::<JsArray, _>($cx) => {
                // CanvasKit style: [r, g, b, a] as floats 0-1
                let arr = arg.downcast_or_throw::<JsArray, _>($cx)?;
                let r = arr.get::<JsNumber, _, _>($cx, 0)?.value($cx) as f32;
                let g = arr.get::<JsNumber, _, _>($cx, 1)?.value($cx) as f32;
                let b = arr.get::<JsNumber, _, _>($cx, 2)?.value($cx) as f32;
                let a = arr.get::<JsNumber, _, _>($cx, 3)?.value($cx) as f32;
                Color4f::new(r, g, b, a)
            }
            _ => $default,
        }
    };
}

const TILE_MODES: &[(&str, TileMode)] = &[
    ("clamp", TileMode::Clamp),
    ("repeat", TileMode::Repeat),
    ("mirror", TileMode::Mirror),
    ("decal", TileMode::Decal),
];

/// The four samplers the Rust API's `SamplingMode` names.
///
/// Two of them were unreachable from JavaScript: `matrix_transform` and
/// `magnifier` take a `SamplingMode` on both surfaces, and this list stopped
/// at the two filter modes -- so a caller could ask Rust for a mipmapped or
/// bicubic transform and could not ask the binding for either.
///
/// Filter mode and mipmap mode are separate fields in Skia, and a cubic
/// resampler is neither: it replaces both. Hence the three-way value rather
/// than a pair of names.
const SAMPLING_MODES: &[(&str, Sampling)] = &[
    (
        "linear",
        Sampling::Filter(FilterMode::Linear, MipmapMode::None),
    ),
    (
        "nearest",
        Sampling::Filter(FilterMode::Nearest, MipmapMode::None),
    ),
    (
        "mipmap",
        Sampling::Filter(FilterMode::Linear, MipmapMode::Linear),
    ),
    ("cubic", Sampling::Cubic),
];

/// How a sampler is spelled before it becomes `SamplingOptions`.
#[derive(Clone, Copy)]
enum Sampling {
    /// A filter mode and a mipmap mode, which is Skia's ordinary pair.
    Filter(FilterMode, MipmapMode),
    /// Mitchell-Netravali bicubic, which sets `use_cubic` and makes Skia
    /// ignore the mipmap chain entirely -- so it cannot be spelled as a
    /// filter and a mipmap mode.
    Cubic,
}

const COLOR_CHANNELS: &[(&str, ColorChannel)] = &[
    ("r", ColorChannel::R),
    ("red", ColorChannel::R),
    ("g", ColorChannel::G),
    ("green", ColorChannel::G),
    ("b", ColorChannel::B),
    ("blue", ColorChannel::B),
    ("a", ColorChannel::A),
    ("alpha", ColorChannel::A),
];

/// Parses `TileMode` from string argument.
fn parse_tile_mode(
    cx: &mut FunctionContext,
    idx: usize,
) -> NeonResult<TileMode> {
    enum_arg_or(cx, idx, "tileMode", TILE_MODES, TileMode::Decal)
}

/// Parses `SamplingOptions` from a string argument naming the filter mode.
fn parse_sampling(
    cx: &mut FunctionContext,
    idx: usize,
) -> NeonResult<SamplingOptions> {
    let sampling = enum_arg_or(
        cx,
        idx,
        "sampling",
        SAMPLING_MODES,
        Sampling::Filter(FilterMode::Linear, MipmapMode::None),
    )?;
    Ok(match sampling {
        Sampling::Filter(filter, mipmap) => {
            SamplingOptions::new(filter, mipmap)
        }
        Sampling::Cubic => SamplingOptions::from(CubicResampler::mitchell()),
    })
}

/// `ImageFilter.MakeColorFilter(colorFilter, input?)`.
pub fn makeColorFilter(mut cx: FunctionContext) -> JsResult<JsValue> {
    let cf = cx.argument::<BoxedColorFilter>(1)?;
    checkColorFilterDeleted(&mut cx, &cf)?;
    let input = opt_input_filter!(&mut cx, 2);
    wrap_image_filter!(
        cx,
        image_filters::color_filter(cf.borrow().inner.clone(), input, None)
    )
}

/// `ImageFilter.MakeCompose(outer, inner)`.
pub fn makeCompose(mut cx: FunctionContext) -> JsResult<JsValue> {
    let outer = cx.argument::<BoxedImageFilter>(1)?;
    let inner = cx.argument::<BoxedImageFilter>(2)?;
    checkDeleted(&mut cx, &outer)?;
    checkDeleted(&mut cx, &inner)?;
    wrap_image_filter!(
        cx,
        image_filters::compose(
            outer.borrow().inner.clone(),
            inner.borrow().inner.clone()
        )
    )
}

/// `ImageFilter.MakeBlur(sigmaX, sigmaY, tileMode?, input?)`.
pub fn makeBlur(mut cx: FunctionContext) -> JsResult<JsValue> {
    let sigma_x = cx.argument::<JsNumber>(1)?.value(&mut cx) as f32;
    let sigma_y = cx.argument::<JsNumber>(2)?.value(&mut cx) as f32;
    let tile_mode = parse_tile_mode(&mut cx, 3)?;
    let input = opt_input_filter!(&mut cx, 4);
    wrap_image_filter!(
        cx,
        image_filters::blur((sigma_x, sigma_y), tile_mode, input, None)
    )
}

/// `ImageFilter.MakeDropShadow(dx, dy, sigmaX, sigmaY, color, input?)`.
pub fn makeDropShadow(mut cx: FunctionContext) -> JsResult<JsValue> {
    let dx = cx.argument::<JsNumber>(1)?.value(&mut cx) as f32;
    let dy = cx.argument::<JsNumber>(2)?.value(&mut cx) as f32;
    let sigma_x = cx.argument::<JsNumber>(3)?.value(&mut cx) as f32;
    let sigma_y = cx.argument::<JsNumber>(4)?.value(&mut cx) as f32;
    let color = parse_color4f!(&mut cx, 5, Color4f::new(0.0, 0.0, 0.0, 1.0));
    let input = opt_input_filter!(&mut cx, 6);
    // Args: offset, sigma, color, color_space, input, crop_rect
    wrap_image_filter!(
        cx,
        image_filters::drop_shadow(
            (dx, dy),
            (sigma_x, sigma_y),
            color,
            None,
            input,
            None
        )
    )
}

/// `ImageFilter.MakeDropShadowOnly(dx, dy, sigmaX, sigmaY, color, input?)`.
pub fn makeDropShadowOnly(mut cx: FunctionContext) -> JsResult<JsValue> {
    let dx = cx.argument::<JsNumber>(1)?.value(&mut cx) as f32;
    let dy = cx.argument::<JsNumber>(2)?.value(&mut cx) as f32;
    let sigma_x = cx.argument::<JsNumber>(3)?.value(&mut cx) as f32;
    let sigma_y = cx.argument::<JsNumber>(4)?.value(&mut cx) as f32;
    let color = parse_color4f!(&mut cx, 5, Color4f::new(0.0, 0.0, 0.0, 1.0));
    let input = opt_input_filter!(&mut cx, 6);
    wrap_image_filter!(
        cx,
        image_filters::drop_shadow_only(
            (dx, dy),
            (sigma_x, sigma_y),
            color,
            None,
            input,
            None
        )
    )
}

/// `ImageFilter.MakeOffset(dx, dy, input?)`.
pub fn makeOffset(mut cx: FunctionContext) -> JsResult<JsValue> {
    let dx = cx.argument::<JsNumber>(1)?.value(&mut cx) as f32;
    let dy = cx.argument::<JsNumber>(2)?.value(&mut cx) as f32;
    let input = opt_input_filter!(&mut cx, 3);
    wrap_image_filter!(cx, image_filters::offset((dx, dy), input, None))
}

/// An optional `[x, y, width, height]` crop rectangle.
///
/// The three filters that take one here -- dilate, erode and matrix
/// convolution -- are the ones where the crop is not the same as composing
/// a separate `"crop"` filter afterwards: it bounds the domain the kernel
/// reads from as well as clipping the output, so a dilation stops spreading
/// at the edge rather than spreading and then being cut. The Rust API has
/// always taken it and the binding passed `None`.
fn parse_crop(
    cx: &mut FunctionContext,
    idx: usize,
) -> NeonResult<Option<image_filters::CropRect>> {
    let Some(arg) = cx.argument_opt(idx) else {
        return Ok(None);
    };
    if arg.is_a::<JsNull, _>(cx) || arg.is_a::<JsUndefined, _>(cx) {
        return Ok(None);
    }
    let rect = arg.downcast_or_throw::<JsArray, _>(cx)?;
    let mut sides = [0f32; 4];
    for (at, side) in sides.iter_mut().enumerate() {
        *side = rect.get::<JsNumber, _, _>(cx, at as u32)?.value(cx) as f32;
    }
    let [x, y, width, height] = sides;
    Ok(Some(skia_safe::Rect::from_xywh(x, y, width, height).into()))
}

/// `ImageFilter.MakeDilate(radiusX, radiusY, input?)`.
pub fn makeDilate(mut cx: FunctionContext) -> JsResult<JsValue> {
    let rx = cx.argument::<JsNumber>(1)?.value(&mut cx) as f32;
    let ry = cx.argument::<JsNumber>(2)?.value(&mut cx) as f32;
    let input = opt_input_filter!(&mut cx, 3);
    let crop = parse_crop(&mut cx, 4)?;
    wrap_image_filter!(cx, image_filters::dilate((rx, ry), input, crop))
}

/// `ImageFilter.MakeErode(radiusX, radiusY, input?)`.
pub fn makeErode(mut cx: FunctionContext) -> JsResult<JsValue> {
    let rx = cx.argument::<JsNumber>(1)?.value(&mut cx) as f32;
    let ry = cx.argument::<JsNumber>(2)?.value(&mut cx) as f32;
    let input = opt_input_filter!(&mut cx, 3);
    let crop = parse_crop(&mut cx, 4)?;
    wrap_image_filter!(cx, image_filters::erode((rx, ry), input, crop))
}

/// `ImageFilter.MakeMerge(filters)` -- merge multiple filters. A `cropRect`
/// argument is accepted for signature parity and ignored.
pub fn makeMerge(mut cx: FunctionContext) -> JsResult<JsValue> {
    let arr = cx.argument::<JsArray>(1)?;
    let len = arr.len(&mut cx);
    let mut filters: Vec<Option<SkImageFilter>> =
        Vec::with_capacity(len as usize);

    for i in 0..len {
        let val: Handle<JsValue> = arr.get(&mut cx, i)?;
        if val.is_a::<JsNull, _>(&mut cx) || val.is_a::<JsUndefined, _>(&mut cx)
        {
            filters.push(None);
        } else {
            let filter =
                val.downcast_or_throw::<BoxedImageFilter, _>(&mut cx)?;
            checkDeleted(&mut cx, &filter)?;
            filters.push(Some(filter.borrow().inner.clone()));
        }
    }

    wrap_image_filter!(cx, image_filters::merge(filters, None))
}

/// `ImageFilter.MakeEmpty()` -- yields transparent black, erasing the input
/// rather than passing it through.
pub fn makeEmpty(mut cx: FunctionContext) -> JsResult<JsValue> {
    let filter = image_filters::empty();
    let imgf = ImageFilter {
        inner: filter,
        deleted: false,
    };
    Ok(cx.boxed(RefCell::new(imgf)).upcast())
}

/// `ImageFilter.MakeTile(src, dst, input?)`.
pub fn makeTile(mut cx: FunctionContext) -> JsResult<JsValue> {
    let src_arr = cx.argument::<JsArray>(1)?;
    let dst_arr = cx.argument::<JsArray>(2)?;

    let src = skia_safe::Rect::from_xywh(
        src_arr.get::<JsNumber, _, _>(&mut cx, 0)?.value(&mut cx) as f32,
        src_arr.get::<JsNumber, _, _>(&mut cx, 1)?.value(&mut cx) as f32,
        src_arr.get::<JsNumber, _, _>(&mut cx, 2)?.value(&mut cx) as f32,
        src_arr.get::<JsNumber, _, _>(&mut cx, 3)?.value(&mut cx) as f32,
    );
    let dst = skia_safe::Rect::from_xywh(
        dst_arr.get::<JsNumber, _, _>(&mut cx, 0)?.value(&mut cx) as f32,
        dst_arr.get::<JsNumber, _, _>(&mut cx, 1)?.value(&mut cx) as f32,
        dst_arr.get::<JsNumber, _, _>(&mut cx, 2)?.value(&mut cx) as f32,
        dst_arr.get::<JsNumber, _, _>(&mut cx, 3)?.value(&mut cx) as f32,
    );

    let input = opt_input_filter!(&mut cx, 3);
    wrap_image_filter!(cx, image_filters::tile(src, dst, input))
}

pub fn repr(mut cx: FunctionContext) -> JsResult<JsString> {
    let this = cx.argument::<BoxedImageFilter>(0)?;
    let b = this.borrow();
    let label = if b.deleted { "deleted" } else { "active" };
    Ok(cx.string(label))
}

pub fn delete(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let this = cx.argument::<BoxedImageFilter>(0)?;
    this.borrow_mut().deleted = true;
    Ok(cx.undefined())
}

fn checkDeleted(
    cx: &mut FunctionContext,
    filter: &BoxedImageFilter,
) -> NeonResult<()> {
    if filter.borrow().deleted {
        cx.throw_error("ImageFilter has been deleted")
    } else {
        Ok(())
    }
}

/// Parses `ColorChannel` from string argument.
fn parse_color_channel(
    cx: &mut FunctionContext,
    idx: usize,
    attr: &str,
) -> NeonResult<ColorChannel> {
    enum_arg_or(cx, idx, attr, COLOR_CHANNELS, ColorChannel::R)
}

/// Parses a `Color` from the argument (CSS string) -> `skia_safe::Color`,
/// 32-bit.
macro_rules! parse_color {
    ($cx:expr, $idx:expr, $default:expr) => {
        match $cx.argument_opt($idx) {
            Some(arg) if arg.is_a::<JsString, _>($cx) => {
                let color_str =
                    arg.downcast_or_throw::<JsString, _>($cx)?.value($cx);
                match css_to_color(&color_str) {
                    Some(c) => c,
                    None => {
                        return $cx.throw_type_error(format!(
                            "Could not parse color \"{color_str}\""
                        ));
                    }
                }
            }
            _ => $default,
        }
    };
}

/// Parses `Point3` from argument [x, y, z] array.
fn parse_point3(cx: &mut FunctionContext, idx: usize) -> NeonResult<Point3> {
    let arr = cx.argument::<JsArray>(idx)?;
    let x = arr.get::<JsNumber, _, _>(cx, 0)?.value(cx) as f32;
    let y = arr.get::<JsNumber, _, _>(cx, 1)?.value(cx) as f32;
    let z = arr.get::<JsNumber, _, _>(cx, 2)?.value(cx) as f32;
    Ok(Point3::new(x, y, z))
}

// ==================== Advanced ImageFilter methods ====================

/// `ImageFilter.MakeBlend(mode, background?, foreground?)`.
pub fn makeBlend(mut cx: FunctionContext) -> JsResult<JsValue> {
    let mode = filter_blend_mode_arg(&mut cx, 1, "mode")?;
    let background = opt_input_filter!(&mut cx, 2);
    let foreground = opt_input_filter!(&mut cx, 3);
    wrap_image_filter!(
        cx,
        image_filters::blend(mode, background, foreground, None)
    )
}

/// `ImageFilter.MakeArithmetic(k1, k2, k3, k4, enforcePMColor, background?,
/// foreground?)`.
pub fn makeArithmetic(mut cx: FunctionContext) -> JsResult<JsValue> {
    let k1 = cx.argument::<JsNumber>(1)?.value(&mut cx) as f32;
    let k2 = cx.argument::<JsNumber>(2)?.value(&mut cx) as f32;
    let k3 = cx.argument::<JsNumber>(3)?.value(&mut cx) as f32;
    let k4 = cx.argument::<JsNumber>(4)?.value(&mut cx) as f32;
    let enforce_pm_color = cx
        .argument_opt(5)
        .and_then(|v| v.downcast::<JsBoolean, _>(&mut cx).ok())
        .map(|b| b.value(&mut cx))
        .unwrap_or(true);
    let background = opt_input_filter!(&mut cx, 6);
    let foreground = opt_input_filter!(&mut cx, 7);
    wrap_image_filter!(
        cx,
        image_filters::arithmetic(
            k1,
            k2,
            k3,
            k4,
            enforce_pm_color,
            background,
            foreground,
            None
        )
    )
}

/// `ImageFilter.MakeDisplacementMap(xChannel, yChannel, scale, displacement?,
/// color?)`.
pub fn makeDisplacementMap(mut cx: FunctionContext) -> JsResult<JsValue> {
    let x_channel = parse_color_channel(&mut cx, 1, "xChannel")?;
    let y_channel = parse_color_channel(&mut cx, 2, "yChannel")?;
    let scale = cx.argument::<JsNumber>(3)?.value(&mut cx) as f32;
    let displacement = opt_input_filter!(&mut cx, 4);
    let color = opt_input_filter!(&mut cx, 5);
    wrap_image_filter!(
        cx,
        image_filters::displacement_map(
            (x_channel, y_channel),
            scale,
            displacement,
            color,
            None
        )
    )
}

/// `ImageFilter.MakeMatrixConvolution(kernelSize, kernel, gain, bias,
/// kernelOffset, tileMode, convolveAlpha, input?)`.
#[allow(clippy::too_many_arguments)]
pub fn makeMatrixConvolution(mut cx: FunctionContext) -> JsResult<JsValue> {
    // kernel_size: [width, height]
    let size_arr = cx.argument::<JsArray>(1)?;
    let kw = size_arr.get::<JsNumber, _, _>(&mut cx, 0)?.value(&mut cx) as i32;
    let kh = size_arr.get::<JsNumber, _, _>(&mut cx, 1)?.value(&mut cx) as i32;
    let kernel_size = ISize::new(kw, kh);

    // kernel: Float32Array or number[]
    let kernel_arr = cx.argument::<JsArray>(2)?;
    let kernel_len = kernel_arr.len(&mut cx);
    let mut kernel: Vec<f32> = Vec::with_capacity(kernel_len as usize);
    for i in 0..kernel_len {
        let val =
            kernel_arr.get::<JsNumber, _, _>(&mut cx, i)?.value(&mut cx) as f32;
        kernel.push(val);
    }

    let gain = cx.argument::<JsNumber>(3)?.value(&mut cx) as f32;
    let bias = cx.argument::<JsNumber>(4)?.value(&mut cx) as f32;

    // kernel_offset: [x, y]
    let offset_arr = cx.argument::<JsArray>(5)?;
    let ox =
        offset_arr.get::<JsNumber, _, _>(&mut cx, 0)?.value(&mut cx) as i32;
    let oy =
        offset_arr.get::<JsNumber, _, _>(&mut cx, 1)?.value(&mut cx) as i32;
    let kernel_offset = IPoint::new(ox, oy);

    let tile_mode = parse_tile_mode(&mut cx, 6)?;
    let convolve_alpha = cx
        .argument_opt(7)
        .and_then(|v| v.downcast::<JsBoolean, _>(&mut cx).ok())
        .map(|b| b.value(&mut cx))
        .unwrap_or(true);
    let input = opt_input_filter!(&mut cx, 8);
    let crop = parse_crop(&mut cx, 9)?;

    wrap_image_filter!(
        cx,
        image_filters::matrix_convolution(
            kernel_size,
            &kernel,
            gain,
            bias,
            kernel_offset,
            tile_mode,
            convolve_alpha,
            input,
            crop
        )
    )
}

/// `ImageFilter.MakeMatrixTransform(matrix, sampling?, input?)`.
pub fn makeMatrixTransform(mut cx: FunctionContext) -> JsResult<JsValue> {
    // matrix: 6 or 9 element array for 2D affine/perspective transform
    let matrix_arr = cx.argument::<JsArray>(1)?;
    let len = matrix_arr.len(&mut cx);

    let matrix = if len == 6 {
        // Affine: [a, b, c, d, e, f] -> [a, c, e; b, d, f; 0, 0, 1]
        let a =
            matrix_arr.get::<JsNumber, _, _>(&mut cx, 0)?.value(&mut cx) as f32;
        let b =
            matrix_arr.get::<JsNumber, _, _>(&mut cx, 1)?.value(&mut cx) as f32;
        let c =
            matrix_arr.get::<JsNumber, _, _>(&mut cx, 2)?.value(&mut cx) as f32;
        let d =
            matrix_arr.get::<JsNumber, _, _>(&mut cx, 3)?.value(&mut cx) as f32;
        let e =
            matrix_arr.get::<JsNumber, _, _>(&mut cx, 4)?.value(&mut cx) as f32;
        let f =
            matrix_arr.get::<JsNumber, _, _>(&mut cx, 5)?.value(&mut cx) as f32;
        Matrix::new_all(a, c, e, b, d, f, 0.0, 0.0, 1.0)
    } else if len == 9 {
        // Full 3x3 matrix row-major
        let mut vals = [0f32; 9];
        for (i, val) in vals.iter_mut().enumerate() {
            *val = matrix_arr
                .get::<JsNumber, _, _>(&mut cx, i as u32)?
                .value(&mut cx) as f32;
        }
        Matrix::new_all(
            vals[0], vals[1], vals[2], vals[3], vals[4], vals[5], vals[6],
            vals[7], vals[8],
        )
    } else {
        return cx.throw_error("Matrix must have 6 or 9 elements");
    };

    // sampling: an optional `"nearest"` or `"linear"`, defaulting to linear.
    let sampling = parse_sampling(&mut cx, 2)?;

    let input = opt_input_filter!(&mut cx, 3);
    wrap_image_filter!(
        cx,
        image_filters::matrix_transform(&matrix, sampling, input)
    )
}

/// `ImageFilter.MakeMagnifier(lensBounds, zoomAmount, inset, sampling?,
/// input?)`.
pub fn makeMagnifier(mut cx: FunctionContext) -> JsResult<JsValue> {
    // lensBounds: [x, y, width, height]
    let bounds_arr = cx.argument::<JsArray>(1)?;
    let x = bounds_arr.get::<JsNumber, _, _>(&mut cx, 0)?.value(&mut cx) as f32;
    let y = bounds_arr.get::<JsNumber, _, _>(&mut cx, 1)?.value(&mut cx) as f32;
    let w = bounds_arr.get::<JsNumber, _, _>(&mut cx, 2)?.value(&mut cx) as f32;
    let h = bounds_arr.get::<JsNumber, _, _>(&mut cx, 3)?.value(&mut cx) as f32;
    let lens_bounds = skia_safe::Rect::from_xywh(x, y, w, h);

    let zoom_amount = cx.argument::<JsNumber>(2)?.value(&mut cx) as f32;
    let inset = cx.argument::<JsNumber>(3)?.value(&mut cx) as f32;

    // sampling: an optional `"nearest"` or `"linear"`, defaulting to linear.
    let sampling = parse_sampling(&mut cx, 4)?;

    let input = opt_input_filter!(&mut cx, 5);
    wrap_image_filter!(
        cx,
        image_filters::magnifier(
            lens_bounds,
            zoom_amount,
            inset,
            sampling,
            input,
            None
        )
    )
}

/// `ImageFilter.MakeCrop(rect, tileMode?, input?)`.
pub fn makeCrop(mut cx: FunctionContext) -> JsResult<JsValue> {
    let rect_arr = cx.argument::<JsArray>(1)?;
    let rect = skia_safe::Rect::from_xywh(
        rect_arr.get::<JsNumber, _, _>(&mut cx, 0)?.value(&mut cx) as f32,
        rect_arr.get::<JsNumber, _, _>(&mut cx, 1)?.value(&mut cx) as f32,
        rect_arr.get::<JsNumber, _, _>(&mut cx, 2)?.value(&mut cx) as f32,
        rect_arr.get::<JsNumber, _, _>(&mut cx, 3)?.value(&mut cx) as f32,
    );
    let tile_mode = parse_tile_mode(&mut cx, 2)?;
    let input = opt_input_filter!(&mut cx, 3);
    wrap_image_filter!(cx, image_filters::crop(rect, tile_mode, input))
}

// ==================== Lighting ImageFilter methods ====================

/// `ImageFilter.MakeDistantLitDiffuse(direction, lightColor, surfaceScale, kd,
/// input?)`.
pub fn makeDistantLitDiffuse(mut cx: FunctionContext) -> JsResult<JsValue> {
    let direction = parse_point3(&mut cx, 1)?;
    let light_color = parse_color!(&mut cx, 2, Color::WHITE);
    let surface_scale = cx.argument::<JsNumber>(3)?.value(&mut cx) as f32;
    let kd = cx.argument::<JsNumber>(4)?.value(&mut cx) as f32;
    let input = opt_input_filter!(&mut cx, 5);
    wrap_image_filter!(
        cx,
        image_filters::distant_lit_diffuse(
            direction,
            light_color,
            surface_scale,
            kd,
            input,
            None
        )
    )
}

/// `ImageFilter.MakePointLitDiffuse(location, lightColor, surfaceScale, kd,
/// input?)`.
pub fn makePointLitDiffuse(mut cx: FunctionContext) -> JsResult<JsValue> {
    let location = parse_point3(&mut cx, 1)?;
    let light_color = parse_color!(&mut cx, 2, Color::WHITE);
    let surface_scale = cx.argument::<JsNumber>(3)?.value(&mut cx) as f32;
    let kd = cx.argument::<JsNumber>(4)?.value(&mut cx) as f32;
    let input = opt_input_filter!(&mut cx, 5);
    wrap_image_filter!(
        cx,
        image_filters::point_lit_diffuse(
            location,
            light_color,
            surface_scale,
            kd,
            input,
            None
        )
    )
}

/// `ImageFilter.MakeSpotLitDiffuse(location, target, falloffExponent,
/// cutoffAngle, lightColor, surfaceScale, kd, input?)`.
#[allow(clippy::too_many_arguments)]
pub fn makeSpotLitDiffuse(mut cx: FunctionContext) -> JsResult<JsValue> {
    let location = parse_point3(&mut cx, 1)?;
    let target = parse_point3(&mut cx, 2)?;
    let falloff_exponent = cx.argument::<JsNumber>(3)?.value(&mut cx) as f32;
    let cutoff_angle = cx.argument::<JsNumber>(4)?.value(&mut cx) as f32;
    let light_color = parse_color!(&mut cx, 5, Color::WHITE);
    let surface_scale = cx.argument::<JsNumber>(6)?.value(&mut cx) as f32;
    let kd = cx.argument::<JsNumber>(7)?.value(&mut cx) as f32;
    let input = opt_input_filter!(&mut cx, 8);
    wrap_image_filter!(
        cx,
        image_filters::spot_lit_diffuse(
            location,
            target,
            falloff_exponent,
            cutoff_angle,
            light_color,
            surface_scale,
            kd,
            input,
            None
        )
    )
}

/// `ImageFilter.MakeDistantLitSpecular(direction, lightColor, surfaceScale, ks,
/// shininess, input?)`.
pub fn makeDistantLitSpecular(mut cx: FunctionContext) -> JsResult<JsValue> {
    let direction = parse_point3(&mut cx, 1)?;
    let light_color = parse_color!(&mut cx, 2, Color::WHITE);
    let surface_scale = cx.argument::<JsNumber>(3)?.value(&mut cx) as f32;
    let ks = cx.argument::<JsNumber>(4)?.value(&mut cx) as f32;
    let shininess = cx.argument::<JsNumber>(5)?.value(&mut cx) as f32;
    let input = opt_input_filter!(&mut cx, 6);
    wrap_image_filter!(
        cx,
        image_filters::distant_lit_specular(
            direction,
            light_color,
            surface_scale,
            ks,
            shininess,
            input,
            None
        )
    )
}

/// `ImageFilter.MakePointLitSpecular(location, lightColor, surfaceScale, ks,
/// shininess, input?)`.
pub fn makePointLitSpecular(mut cx: FunctionContext) -> JsResult<JsValue> {
    let location = parse_point3(&mut cx, 1)?;
    let light_color = parse_color!(&mut cx, 2, Color::WHITE);
    let surface_scale = cx.argument::<JsNumber>(3)?.value(&mut cx) as f32;
    let ks = cx.argument::<JsNumber>(4)?.value(&mut cx) as f32;
    let shininess = cx.argument::<JsNumber>(5)?.value(&mut cx) as f32;
    let input = opt_input_filter!(&mut cx, 6);
    wrap_image_filter!(
        cx,
        image_filters::point_lit_specular(
            location,
            light_color,
            surface_scale,
            ks,
            shininess,
            input,
            None
        )
    )
}

/// `ImageFilter.MakeSpotLitSpecular(location, target, falloffExponent,
/// cutoffAngle, lightColor, surfaceScale, ks, shininess, input?)`.
#[allow(clippy::too_many_arguments)]
pub fn makeSpotLitSpecular(mut cx: FunctionContext) -> JsResult<JsValue> {
    let location = parse_point3(&mut cx, 1)?;
    let target = parse_point3(&mut cx, 2)?;
    let falloff_exponent = cx.argument::<JsNumber>(3)?.value(&mut cx) as f32;
    let cutoff_angle = cx.argument::<JsNumber>(4)?.value(&mut cx) as f32;
    let light_color = parse_color!(&mut cx, 5, Color::WHITE);
    let surface_scale = cx.argument::<JsNumber>(6)?.value(&mut cx) as f32;
    let ks = cx.argument::<JsNumber>(7)?.value(&mut cx) as f32;
    let shininess = cx.argument::<JsNumber>(8)?.value(&mut cx) as f32;
    let input = opt_input_filter!(&mut cx, 9);
    wrap_image_filter!(
        cx,
        image_filters::spot_lit_specular(
            location,
            target,
            falloff_exponent,
            cutoff_angle,
            light_color,
            surface_scale,
            ks,
            shininess,
            input,
            None
        )
    )
}
