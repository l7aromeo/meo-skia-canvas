#![allow(non_snake_case)]
use neon::prelude::*;
use skia_safe::{
    Matrix, PaintStyle,
    PaintStyle::{Fill, Stroke},
    Path, PathBuilder, PathDirection, Point, RRect, Rect, Size,
    path::AddPathMode,
    textlayout::TextDirection,
};
use std::cell::RefCell;

use super::{BoxedContext2D, Context2D, Dye, page::ExportOptions};
use crate::{
    color_filter::BoxedColorFilter,
    export::VectorFeatures,
    image_filter::BoxedImageFilter,
    mask_filter::BoxedMaskFilter,
    node::{
        canvas::BoxedCanvas,
        filter::Filter,
        image::{BoxedImage, Content},
        path::{Path2D, conic_or_line},
    },
    typography::{
        decoration_arg, font_arg, font_features, from_text_align,
        from_text_baseline, from_width, js_text_metrics, opt_spacing_arg,
        to_text_align, to_text_baseline, to_width,
    },
    utils::*,
};
use skia_safe::{FourByteTag, Paint};

//
// The js interface for the Context2D struct
//

//
// -- Drawing verbs
// --------------------------------------------------------------------------
//
// Declared once each, for the entry point Node calls and for the arm a
// decoder reads. See `crate::node::verbs`.
//
// Only the verbs whose arguments are all numbers. `fill`, `stroke`, `clip`,
// `drawImage`, `fillText`, `setLineDash` and the transform pair take a path,
// an image, a string, a sequence or a matrix, and stay hand-written below
// until a queue can carry something other than a number.
//

use crate::node::verbs::verbs;

verbs! {
    ContextVerb for BoxedContext2D => Context2D;

    // Kept wide. Alpha is a double here rather than a byte, which is why a
    // fill at 0.5 lands on 128 where truncating gives 127 -- see the note in
    // AGENTS.md on where this fork's output differs on purpose. Out-of-range
    // values are ignored rather than clamped, as they were before.
    set_globalAlpha as SetGlobalAlpha (globalAlpha @ wide) => |ctx| {
        if (0.0..=1.0).contains(&globalAlpha) {
            ctx.state.global_alpha = globalAlpha;
        }
    },

    // No numbers at all, just the flag: the same trailing boolean an `arc`
    // reads, which is why it is declared the same way.
    set_imageSmoothingEnabled as SetImageSmoothingEnabled (); enabled => |ctx| {
        ctx.state.sampling_filter.smoothing = enabled;
    },

    reset as Reset () => |ctx| {
        let size = ctx.bounds.size();
        ctx.reset_size(size);
    },

    // Property writes are verbs too, and the measurement says they are the
    // ones that matter: a frame of `examples/node/animated-eye.js` sets a
    // property 4915 times and calls a drawing verb 1319 times. A write needs
    // no answer, so nothing about it has to happen before the next statement.
    //
    // Only the ones holding a plain `f32`. `globalAlpha` is deliberately
    // `f64` here -- this fork keeps float alpha rather than truncating it to
    // a byte -- and the rest take a colour, a font, a filter or an enum, so
    // they wait for a lane that can carry something other than a number.

    // Ignored rather than refused when it is not positive, which is what a
    // browser does and what these did before they were declared.
    set_lineWidth as SetLineWidth (lineWidth) => |ctx| {
        if lineWidth > 0.0 {
            ctx.state.paint.set_stroke_width(lineWidth);
        }
    },

    set_miterLimit as SetMiterLimit (miterLimit) => |ctx| {
        if miterLimit > 0.0 {
            ctx.state.paint.set_stroke_miter(miterLimit);
        }
    },

    set_lineDashOffset as SetLineDashOffset (lineDashOffset) => |ctx| {
        ctx.state.line_dash_offset = lineDashOffset;
    },

    set_shadowBlur as SetShadowBlur (shadowBlur) => |ctx| {
        if shadowBlur >= 0.0 {
            ctx.state.shadow_blur = shadowBlur;
        }
    },

    set_shadowOffsetX as SetShadowOffsetX (shadowOffsetX) => |ctx| {
        ctx.state.shadow_offset.x = shadowOffsetX;
    },

    set_shadowOffsetY as SetShadowOffsetY (shadowOffsetY) => |ctx| {
        ctx.state.shadow_offset.y = shadowOffsetY;
    },

    save as Save () => |ctx| {
        ctx.push();
    },

    restore as Restore () => |ctx| {
        ctx.pop();
    },

    beginPath as BeginPath () => |ctx| {
        ctx.path = PathBuilder::new();
    },

    resetTransform as ResetTransform () => |ctx| {
        ctx.with_matrix(|ctm| ctm.reset());
    },

    translate as Translate (x, y) => |ctx| {
        ctx.with_matrix(|ctm| ctm.pre_translate((x, y)));
    },

    scale as Scale (x, y) => |ctx| {
        ctx.with_matrix(|ctm| ctm.pre_scale((x, y), None));
    },

    rotate as Rotate (angle) => |ctx| {
        let degrees = angle.to_degrees();
        ctx.with_matrix(|ctm| ctm.pre_rotate(degrees, None));
    },

    // The context's path is kept in device space, so every point is mapped
    // through the current transform on the way in. That is what makes a
    // `translate` between two `lineTo` calls move the second one.
    moveTo as MoveTo (x, y) => |ctx| {
        if let Some(dst) = ctx.map_points(&[x, y]).first() {
            ctx.path.move_to(*dst);
        }
    },

    lineTo as LineTo (x, y) => |ctx| {
        if let Some(dst) = ctx.map_points(&[x, y]).first() {
            ctx.scoot(*dst);
            ctx.path.line_to(*dst);
        }
    },

    quadraticCurveTo as QuadraticCurveTo (cpx, cpy, x, y) => |ctx| {
        if let [cp, dst] = ctx.map_points(&[cpx, cpy, x, y])[..2] {
            ctx.scoot(cp);
            ctx.path.quad_to(cp, dst);
        }
    },

    bezierCurveTo as BezierCurveTo (cp1x, cp1y, cp2x, cp2y, x, y) => |ctx| {
        let mapped = ctx.map_points(&[cp1x, cp1y, cp2x, cp2y, x, y]);
        if let [cp1, cp2, dst] = mapped[..3] {
            ctx.scoot(cp1);
            ctx.path.cubic_to(cp1, cp2, dst);
        }
    },

    // The weight is not a coordinate and is not mapped with the points.
    conicCurveTo as ConicCurveTo (cpx, cpy, x, y, weight) => |ctx| {
        if let [src, dst] = ctx.map_points(&[cpx, cpy, x, y]).as_slice() {
            let (src, dst) = (*src, *dst);
            ctx.scoot(src);
            conic_or_line(&mut ctx.path, src, dst, weight);
        }
    },

    // Four mapped corners rather than a mapped rectangle: a rotation turns a
    // rectangle into a quadrilateral, and a `Rect` cannot hold one.
    rect as Rect (x, y, width, height) => |ctx| {
        let rect = Rect::from_xywh(x, y, width, height);
        let quad = ctx.state.matrix.map_rect_to_quad(rect);
        ctx.path.move_to(quad[0]);
        ctx.path.line_to(quad[1]);
        ctx.path.line_to(quad[2]);
        ctx.path.line_to(quad[3]);
        ctx.path.close();
    },

    // A negative radius is refused where every other coordinate here is
    // ignored, which is what a browser does.
    arc as Arc (x, y, radius @ non_negative, startAngle, endAngle); ccw => |ctx| {
        let matrix = ctx.state.matrix;
        let mut arc = Path2D::default();
        arc.add_ellipse((x, y), (radius, radius), 0.0, startAngle, endAngle, ccw);
        // Extend, not Append: the arc must continue the current contour.
        // Appending starts a new one, which strokes identically but fills as a
        // separate region -- see #9.
        ctx.path.add_path_with_transform(
            &arc.path(),
            &matrix,
            AddPathMode::Extend,
        );
    },

    arcTo as ArcTo (x1, y1, x2, y2, radius @ non_negative) => |ctx| {
        if let [src, dst] = ctx.map_points(&[x1, y1, x2, y2])[..2] {
            ctx.scoot(src);
            ctx.path.arc_to_tangent(src, dst, radius);
        }
    },

    ellipse as Ellipse (
        x, y, xRadius @ non_negative, yRadius @ non_negative,
        rotation, startAngle, endAngle
    ); ccw => |ctx| {
        let matrix = ctx.state.matrix;
        let mut arc = Path2D::default();
        arc.add_ellipse(
            (x, y),
            (xRadius, yRadius),
            rotation,
            startAngle,
            endAngle,
            ccw,
        );
        ctx.path.add_path_with_transform(
            &arc.path(),
            &matrix,
            AddPathMode::Extend,
        );
    },

    fillRect as FillRect (x, y, width, height) => |ctx| {
        let rect = Rect::from_xywh(x, y, width, height);
        let path = Path::rect(rect, None);
        ctx.draw_path(Some(path), PaintStyle::Fill, None);
    },

    strokeRect as StrokeRect (x, y, width, height) => |ctx| {
        let rect = Rect::from_xywh(x, y, width, height);
        let path = Path::rect(rect, None);
        ctx.draw_path(Some(path), PaintStyle::Stroke, None);
    },

    clearRect as ClearRect (x, y, width, height) => |ctx| {
        let rect = Rect::from_xywh(x, y, width, height);
        ctx.clear_rect(&rect);
    },
}

pub fn new(mut cx: FunctionContext) -> JsResult<BoxedContext2D> {
    let parent = cx.argument::<BoxedCanvas>(1)?;
    let parent = parent.borrow();
    let this = RefCell::new(Context2D::new(parent.color_space.clone()));

    this.borrow_mut().reset_size((parent.width, parent.height));
    Ok(cx.boxed(this))
}

pub fn resetSize(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let parent = cx.argument::<BoxedCanvas>(1)?;
    let parent = parent.borrow();

    this.borrow_mut().reset_size((parent.width, parent.height));
    Ok(cx.undefined())
}

pub fn get_size(mut cx: FunctionContext) -> JsResult<JsArray> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let bounds = this.borrow().bounds;

    let array = JsArray::new(&mut cx, 2);
    let width = cx.number(bounds.size().width);
    let height = cx.number(bounds.size().height);
    array.set(&mut cx, 0, width)?;
    array.set(&mut cx, 1, height)?;
    Ok(array)
}

pub fn set_size(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let this = cx.argument::<BoxedContext2D>(0)?;

    if let [width, height] = opt_float_args(&mut cx, 1..3).as_slice() {
        this.borrow_mut().resize((*width, *height));
    }
    Ok(cx.undefined())
}

//
// Grid State
//

/// `ctx.saveLayer(alpha?, bounds?, backdrop?)` -- push an isolated layer
/// that composites onto the canvas on the matching `restore()`. `alpha`
/// (default 1) and the current `globalCompositeOperation` form the layer
/// paint; `bounds` is an optional `[x, y, w, h]` hint; `backdrop` is an
/// optional `ImageFilter` applied to the content behind the layer.
pub fn saveLayer(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let mut this = this.borrow_mut();

    let alpha = opt_float_arg(&mut cx, 1).unwrap_or(1.0);

    // bounds: optional [x, y, w, h] array.
    let bounds = match cx.argument_opt(2) {
        Some(arg) if arg.is_a::<JsArray, _>(&mut cx) => {
            // SAFETY: the match guard already established `arg` is a
            // `JsArray`, so this downcast cannot fail.
            let arr = arg
                .downcast::<JsArray, _>(&mut cx)
                .unwrap()
                .to_vec(&mut cx)?;
            if arr.len() >= 4 {
                let mut v = [0f32; 4];
                for (i, slot) in v.iter_mut().enumerate() {
                    *slot = arr[i]
                        .downcast::<JsNumber, _>(&mut cx)
                        .map(|n| n.value(&mut cx) as f32)
                        .unwrap_or(0.0);
                }
                Some(Rect::from_xywh(v[0], v[1], v[2], v[3]))
            } else {
                None
            }
        }
        _ => None,
    };

    // backdrop: optional ImageFilter applied to the existing content.
    let backdrop = match cx.argument_opt(3) {
        Some(arg)
            if !arg.is_a::<JsNull, _>(&mut cx)
                && !arg.is_a::<JsUndefined, _>(&mut cx) =>
        {
            let f = arg.downcast_or_throw::<BoxedImageFilter, _>(&mut cx)?;
            if f.borrow().is_deleted() {
                return cx.throw_error("ImageFilter has been deleted");
            }
            Some(f.borrow().inner.clone())
        }
        _ => None,
    };

    let blend_mode = this.state.global_composite_operation;
    let mut paint = Paint::default();
    paint.set_anti_alias(true);
    paint.set_alpha_f(alpha);
    paint.set_blend_mode(blend_mode);

    this.save_layer(Some(paint), bounds, backdrop);
    Ok(cx.undefined())
}

pub fn transform(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let mut this = this.borrow_mut();

    if let Some(matrix) = opt_matrix_arg(&mut cx, 1) {
        this.with_matrix(|ctm| ctm.pre_concat(&matrix));
    }
    Ok(cx.undefined())
}

pub fn createProjection(mut cx: FunctionContext) -> JsResult<JsArray> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let this = this.borrow_mut();
    let dst = points_arg(&mut cx, 1)?;
    let src = points_arg(&mut cx, 2)?;

    let basis: Vec<Point> = match src.len() {
        0 => this.bounds.to_quad(None).to_vec(), // use canvas dims
        1 => Rect::from_wh(src[0].x, src[0].y).to_quad(None).to_vec(), /* implicit 0,0 origin */
        2 => Rect::new(src[0].x, src[0].y, src[1].x, src[1].y)
            .to_quad(None)
            .to_vec(), /* lf/top, rt/bot */
        _ => src.clone(),
    };

    let quad: Vec<Point> = match dst.len() {
        1 => Rect::from_wh(dst[0].x, dst[0].y).to_quad(None).to_vec(), /* implicit 0,0 origin */
        2 => Rect::new(dst[0].x, dst[0].y, dst[1].x, dst[1].y)
            .to_quad(None)
            .to_vec(), /* lf/top, rt/bot */
        _ => dst.clone(),
    };

    match (Matrix::from_poly_to_poly(&basis, &quad), basis.len() == quad.len()){
    (Some(projection), true) => {
      let array = JsArray::new(&mut cx, 9);
      for i in 0..9 {
        let num = cx.number(projection[i as usize]);
        array.set(&mut cx, i as u32, num)?;
      }
      Ok(array)
    },
    _ => cx.throw_type_error(format!(
      "Expected 2 or 4 x/y points for output quad (got {}) and 0, 1, 2, or 4 points for the coordinate basis (got {})",
      quad.len(), basis.len()
    ))
  }
}

// -- ctm property
// ----------------------------------------------------------------------

pub fn get_currentTransform(mut cx: FunctionContext) -> JsResult<JsArray> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let this = this.borrow();

    let array = JsArray::new(&mut cx, 9);
    for i in 0..9 {
        let num = cx.number(this.state.matrix[i as usize]);
        array.set(&mut cx, i as u32, num)?;
    }
    Ok(array)
}

pub fn set_currentTransform(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let mut this = this.borrow_mut();

    if let Some(matrix) = opt_matrix_arg(&mut cx, 1) {
        this.with_matrix(|ctm| ctm.reset().pre_concat(&matrix));
    }
    Ok(cx.undefined())
}

//
// Bézier Paths
//

// -- primitives
// ------------------------------------------------------------------------

pub fn roundRect(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let mut this = this.borrow_mut();

    let nums = float_args(
        &mut cx,
        &[
            "x", "y", "width", "height", "r1x", "r1y", "r2x", "r2y", "r3x",
            "r3y", "r4x", "r4y",
        ],
    )?;
    if let [x, y, w, h] = &nums[..4] {
        let rect = Rect::from_xywh(*x, *y, *w, *h);
        let radii: Vec<Point> = nums[4..]
            .chunks(2)
            .map(|xy| Point::new(xy[0], xy[1]))
            .collect();
        let rrect = RRect::new_rect_radii(
            rect,
            &[radii[0], radii[1], radii[2], radii[3]],
        );
        let direction = if w.signum() == h.signum() {
            PathDirection::CW
        } else {
            PathDirection::CCW
        };

        let matrix = this.state.matrix;
        // Path::rrect, not a PathBuilder with an explicit start index. The
        // two roundRect entry points differ and have to keep differing:
        // Path2D.roundRect
        // pins index 0, while this one takes Skia's legacy 6 (CW) / 7 (CCW).
        // The start corner decides where Extend attaches, where the current
        // point lands, and where dash phase begins.
        let path = Path::rrect(rrect, Some(direction)).make_transform(&matrix);
        // Extend, not Append: the arc must continue the current contour.
        // Appending starts a new one, which strokes identically but
        // fills as a separate region -- see #9.
        this.path.add_path(&path, AddPathMode::Extend);
    }

    Ok(cx.undefined())
}

// contour drawing
// ----------------------------------------------------------------------

pub fn closePath(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let mut this = this.borrow_mut();

    this.path.close();
    Ok(cx.undefined())
}

// hit testing
// --------------------------------------------------------------------------

pub fn isPointInPath(cx: FunctionContext) -> JsResult<JsBoolean> {
    _is_in(cx, Fill)
}

pub fn isPointInStroke(cx: FunctionContext) -> JsResult<JsBoolean> {
    _is_in(cx, Stroke)
}

fn _is_in(mut cx: FunctionContext, style: PaintStyle) -> JsResult<JsBoolean> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let mut this = this.borrow_mut();

    let path = opt_skpath_arg(&mut cx, 1);
    let (rule_idx, mut target) = match path {
        Some(path) => (4, path),
        None => match cx.len() {
            5 => cx.throw_type_error("Expected a Path2D for 1st arg")?,
            _ => (3, this.path.snapshot()),
        },
    };

    let rule = match style {
        Stroke => None,
        _ => Some(fill_rule_arg_or(&mut cx, rule_idx, "nonzero")?),
    };

    if let [x, y] = opt_float_args(&mut cx, 1..4).as_slice() {
        Ok(cx.boolean(this.hit_test_path(&mut target, (*x, *y), rule, style)))
    } else {
        check_argc(&mut cx, 3)?;
        Ok(cx.boolean(false))
    }
}

// masking ------------------------------------------------------------------------------

pub fn clip(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let mut this = this.borrow_mut();

    let mut shift = 1;
    let path = opt_skpath_arg(&mut cx, 1);
    if path.is_some() {
        shift += 1;
    } else if cx.len() > 2 {
        return cx.throw_type_error("Expected a Path2D for 1st arg");
    }
    let rule = fill_rule_arg_or(&mut cx, shift, "nonzero")?;

    this.clip_path(path, rule);
    Ok(cx.undefined())
}

//
// Fill & Stroke
//

pub fn fill(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let mut this = this.borrow_mut();

    let mut shift = 1;
    let path = opt_skpath_arg(&mut cx, 1);
    if path.is_some() {
        shift += 1;
    } else if cx.len() > 2 {
        return cx.throw_type_error("Expected a Path2D for 1st arg");
    }
    let rule = fill_rule_arg_or(&mut cx, shift, "nonzero")?;

    this.draw_path(path, PaintStyle::Fill, Some(rule));
    Ok(cx.undefined())
}

pub fn stroke(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let path = opt_skpath_arg(&mut cx, 1);

    if path.is_none() && cx.len() >= 2 {
        return cx.throw_type_error("Expected a Path2D for 1st arg");
    }

    this.borrow_mut().draw_path(path, PaintStyle::Stroke, None);
    Ok(cx.undefined())
}

// fill & stoke properties
// --------------------------------------------------------------

pub fn get_fillStyle(mut cx: FunctionContext) -> JsResult<JsValue> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let this = this.borrow();
    let dye = this.state.fill_style.clone();
    dye.value(&mut cx)
}

pub fn set_fillStyle(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let arg = cx.argument::<JsValue>(1)?;
    let cs = this.borrow().canvas_color_space.clone();

    if let Some(dye) = Dye::new(&mut cx, arg, &cs) {
        this.borrow_mut().state.fill_style = dye;
    }
    Ok(cx.undefined())
}

pub fn get_strokeStyle(mut cx: FunctionContext) -> JsResult<JsValue> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let this = this.borrow();
    let dye = this.state.stroke_style.clone();
    dye.value(&mut cx)
}

pub fn set_strokeStyle(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let arg = cx.argument::<JsValue>(1)?;
    let cs = this.borrow().canvas_color_space.clone();

    if let Some(dye) = Dye::new(&mut cx, arg, &cs) {
        this.borrow_mut().state.stroke_style = dye;
    }
    Ok(cx.undefined())
}

//
// Line Style
//

pub fn set_lineDashMarker(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let marker = opt_skpath_arg(&mut cx, 1);

    if marker.is_none() {
        let val = cx.argument::<JsValue>(1)?;
        if !(val.is_a::<JsNull, _>(&mut cx) || val.is_a::<JsNull, _>(&mut cx)) {
            return cx.throw_type_error("Expected a Path2D object (or null)");
        }
    }

    this.borrow_mut().state.line_dash_marker = marker;
    Ok(cx.undefined())
}

pub fn get_lineDashMarker(mut cx: FunctionContext) -> JsResult<JsValue> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let this = this.borrow();

    match &this.state.line_dash_marker {
        Some(marker) => {
            let builder = PathBuilder::new_path(marker);
            Ok(cx.boxed(RefCell::new(Path2D { builder })).upcast())
        }
        None => Ok(cx.null().upcast()),
    }
}

pub fn set_lineDashFit(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let style = string_arg(&mut cx, 1, "fitStyle")?;

    if let Some(fit) = to_1d_style(&style) {
        this.borrow_mut().state.line_dash_fit = fit;
    }
    Ok(cx.undefined())
}

pub fn get_lineDashFit(mut cx: FunctionContext) -> JsResult<JsString> {
    let this = cx.argument::<BoxedContext2D>(0)?;

    let fit = from_1d_style(this.borrow().state.line_dash_fit);
    Ok(cx.string(fit))
}

pub fn getLineDash(mut cx: FunctionContext) -> JsResult<JsValue> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let this = this.borrow_mut();
    let dashes = this.state.line_dash_list.clone();
    floats_to_array(&mut cx, &dashes)
}

pub fn setLineDash(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let mut this = this.borrow_mut();
    let arg = cx.argument::<JsValue>(1)?;
    if arg.is_a::<JsArray, _>(&mut cx) {
        let list = cx.argument::<JsArray>(1)?.to_vec(&mut cx)?;
        let mut intervals = floats_in(&mut cx, &list)
            .iter()
            .cloned()
            .filter(|n| *n >= 0.0 && n.is_finite())
            .collect::<Vec<f32>>();

        // only apply if all elements were actually numbers
        if list.len() == intervals.len() {
            if intervals.len() % 2 == 1 {
                intervals.append(&mut intervals.clone());
            }

            this.state.line_dash_list = intervals
        }
    } else {
        cx.throw_type_error("Value is not a sequence")?
    }

    Ok(cx.undefined())
}

// line style properties
// -----------------------------------------------------------

pub fn get_lineCap(mut cx: FunctionContext) -> JsResult<JsString> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let this = this.borrow_mut();

    let mode = this.state.paint.stroke_cap();
    let name = from_stroke_cap(mode);
    Ok(cx.string(name))
}

pub fn set_lineCap(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let mut this = this.borrow_mut();
    let name = string_arg(&mut cx, 1, "lineCap")?;

    if let Some(mode) = to_stroke_cap(&name) {
        this.state.paint.set_stroke_cap(mode);
    }
    Ok(cx.undefined())
}

pub fn get_lineDashOffset(mut cx: FunctionContext) -> JsResult<JsNumber> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let this = this.borrow_mut();

    let num = this.state.line_dash_offset;
    Ok(cx.number(num))
}

pub fn get_lineJoin(mut cx: FunctionContext) -> JsResult<JsString> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let this = this.borrow_mut();

    let mode = this.state.paint.stroke_join();
    let name = from_stroke_join(mode);
    Ok(cx.string(name))
}

pub fn set_lineJoin(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let mut this = this.borrow_mut();
    let name = string_arg(&mut cx, 1, "lineJoin")?;

    if let Some(mode) = to_stroke_join(&name) {
        this.state.paint.set_stroke_join(mode);
    }
    Ok(cx.undefined())
}

pub fn get_lineWidth(mut cx: FunctionContext) -> JsResult<JsNumber> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let this = this.borrow_mut();

    let num = this.state.paint.stroke_width();
    Ok(cx.number(num))
}

pub fn get_miterLimit(mut cx: FunctionContext) -> JsResult<JsNumber> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let this = this.borrow_mut();

    let num = this.state.paint.stroke_miter();
    Ok(cx.number(num))
}

//
// Imagery
//

fn _layout_rects(
    cx: &mut FunctionContext,
    intrinsic: Size,
    nums: &[f32],
) -> NeonResult<(Rect, Rect)> {
    let (src, dst) = match nums.len() {
        2 => (
            Rect::from_xywh(0.0, 0.0, intrinsic.width, intrinsic.height),
            Rect::from_xywh(
                nums[0],
                nums[1],
                intrinsic.width,
                intrinsic.height,
            ),
        ),
        4 => (
            Rect::from_xywh(0.0, 0.0, intrinsic.width, intrinsic.height),
            Rect::from_xywh(nums[0], nums[1], nums[2], nums[3]),
        ),
        8 => (
            Rect::from_xywh(nums[0], nums[1], nums[2], nums[3]),
            Rect::from_xywh(nums[4], nums[5], nums[6], nums[7]),
        ),
        9.. => cx.throw_type_error(format!(
            "⚠️Expected 2, 4, or 8 coordinates (got {})",
            nums.len()
        ))?,
        _ => cx.throw_type_error(format!(
            "not enough arguments: Expected 2, 4, or 8 coordinates (got {})",
            nums.len()
        ))?,
    };

    match intrinsic.is_empty() {
        true => cx.throw_range_error(format!(
            "Dimensions must be non-zero (got {}×{})",
            intrinsic.width, intrinsic.height
        )),
        false => Ok((src, dst)),
    }
}

pub fn drawImage(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let argc = cx.len();
    let source = cx.argument::<JsValue>(1)?;
    let arg_names = [
        "srcX",
        "srcY",
        "srcWidth",
        "srcHeight",
        "dstX",
        "dstY",
        "dstWidth",
        "dstHeight",
    ];
    let nums = float_args_or_bail_at(&mut cx, 2, &arg_names[..argc - 2])?;

    let content = {
        if let Ok(img) = source.downcast::<BoxedImage, _>(&mut cx) {
            img.borrow().content.clone()
        } else if let Ok(ctx) = source.downcast::<BoxedContext2D, _>(&mut cx) {
            Content::from_context(&mut ctx.borrow_mut(), false)
        } else if let Ok(image_data) = image_data_arg(&mut cx, 1) {
            Content::from_image_data(image_data)
        } else {
            Content::default()
        }
    };

    if let Content::Bitmap(img) = &content {
        let bounds_size = content.size();
        let (src, dst) = _layout_rects(&mut cx, bounds_size, &nums)?;

        content.snap_rects_to_bounds(src, dst);
        let mut this = this.borrow_mut();
        this.draw_image(img, &src, &dst);
    } else if let Content::Vector(pict, pict_size) = &content {
        let image = source.downcast_or_throw::<BoxedImage, _>(&mut cx)?;
        let fit_to_canvas = image.borrow().autosized;
        let (mut src, mut dst) = _layout_rects(&mut cx, *pict_size, &nums)?;

        // for SVG images with no intrinsic size, use the canvas size as a
        // default scale
        if fit_to_canvas && nums.len() != 4 {
            let canvas_size = this.borrow().bounds.size();
            let canvas_min = canvas_size.width.min(canvas_size.height);
            let pict_min = pict_size.width.min(pict_size.height);

            if nums.len() == 2 {
                // if the user doesn't specify a size, proportionally scale to
                // fit within canvas
                let factor = canvas_min / pict_min;
                dst = Rect::from_point_and_size(
                    (dst.x(), dst.y()),
                    dst.size() * factor,
                );
            } else if nums.len() == 8 {
                // if clipping out part of the source, map the crop coordinates
                // as if the image is canvas-sized
                let factor = (
                    pict_size.width / canvas_min,
                    pict_size.height / canvas_min,
                );
                (src, _) = Matrix::scale(factor).map_rect(src);
            }
        }

        content.snap_rects_to_bounds(src, dst);
        let mut this = this.borrow_mut();
        this.draw_picture(pict, &src, &dst, VectorFeatures::PLAIN);
    }

    Ok(cx.undefined())
}

pub fn drawCanvas(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let argc = cx.len();
    let this = cx.argument::<BoxedContext2D>(0)?;
    let context = cx.argument::<BoxedContext2D>(1)?;
    let arg_names = [
        "srcX",
        "srcY",
        "srcWidth",
        "srcHeight",
        "dstX",
        "dstY",
        "dstWidth",
        "dstHeight",
    ];
    let nums = float_args_or_bail_at(&mut cx, 2, &arg_names[..argc - 2])?;

    let source = context.borrow_mut().get_page().vector_features();
    let content = Content::from_context(&mut context.borrow_mut(), true);
    if let Content::Vector(pict, size) = &content {
        let (src, dst) = _layout_rects(&mut cx, *size, &nums)?;
        let (src, dst) = content.snap_rects_to_bounds(src, dst);
        this.borrow_mut().draw_picture(pict, &src, &dst, source);
        Ok(cx.undefined())
    } else {
        cx.throw_error("Canvas's PictureRecorder failed to generate an image")
    }
}

pub fn getImageData(mut cx: FunctionContext) -> JsResult<JsBuffer> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let mut x = float_arg(&mut cx, 1, "x")?.floor();
    let mut y = float_arg(&mut cx, 2, "y")?.floor();
    let mut w = float_arg(&mut cx, 3, "width")?.floor();
    let mut h = float_arg(&mut cx, 4, "height")?.floor();
    let (color_type, color_space, matte, density, msaa) =
        image_data_export_arg(&mut cx, 5)?;
    let parent = cx.argument::<BoxedCanvas>(6)?;
    let canvas = &mut parent.borrow_mut();

    // negative dimensions are valid, just shift the origin and absify
    if w < 0.0 {
        x += w;
        w *= -1.0;
    }
    if h < 0.0 {
        y += h;
        h *= -1.0;
    }

    // The canvas's own colorType/colorSpace are the fallback; an explicit
    // option on this call overrides them.
    let base = canvas.export_options();
    let opts = ExportOptions {
        matte,
        density,
        msaa,
        color_type: color_type.unwrap_or(base.color_type),
        color_space: color_space.unwrap_or_else(|| base.color_space.clone()),
        ..base
    };
    // `Rect::round` saturates each edge to i32::MIN/MAX, and the width is
    // then taken as a difference of the two -- so a rect spanning the range
    // panics inside skia-safe rather than reporting anything useful. Neon
    // turns that into "internal error in Neon module", which tells the caller
    // nothing. `density` is what puts it in reach from JavaScript: it scales
    // the rect after the arguments have been validated.
    let limit = f64::from(i32::MAX);
    let (ox, oy) = (
        f64::from(x) * f64::from(density),
        f64::from(y) * f64::from(density),
    );
    let (dw, dh) = (
        f64::from(w) * f64::from(density),
        f64::from(h) * f64::from(density),
    );
    if ox < -limit
        || oy < -limit
        || ox + dw > limit
        || oy + dh > limit
        || dw > limit
        || dh > limit
    {
        return cx.throw_error(format!(
            "Requested image data is out of range: {w}x{h} at ({x}, {y}) \
             scaled by a density of {density} reaches past the {} coordinate \
             limit Skia can address",
            i32::MAX
        ));
    }

    let crop = Rect::from_point_and_size(
        (x * density, y * density),
        (w * density, h * density),
    )
    .round();
    let engine = canvas.engine();

    let data = this
        .borrow_mut()
        .get_pixels(crop, opts, engine)
        .or_else(|e| cx.throw_error(e))?;
    let buffer = JsBuffer::from_slice(&mut cx, &data)?;

    Ok(buffer)
}

pub fn putImageData(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let mut this = this.borrow_mut();
    let img_data = image_data_arg(&mut cx, 1)?;

    // determine geometry
    let x = float_arg(&mut cx, 2, "dx")?;
    let y = float_arg(&mut cx, 3, "dy")?;
    let mut dirty = match cx.len() {
        5.. => float_args_at(
            &mut cx,
            4,
            &["dirtyX", "dirtyY", "dirtyWidth", "dirtyHeight"],
        )?,
        _ => [].to_vec(),
    };
    let (src, dst) = match dirty.as_mut_slice() {
        [dx, dy, dw, dh] => {
            // negative dimensions are valid, just shift the origin and absify
            if *dw < 0.0 {
                *dw *= -1.0;
                *dx -= *dw;
            }
            if *dh < 0.0 {
                *dh *= -1.0;
                *dy -= *dh;
            }
            (
                Rect::from_xywh(*dx, *dy, *dw, *dh),
                Rect::from_xywh(*dx + x, *dy + y, *dw, *dh),
            )
        }
        _ => (
            Rect::from_xywh(0.0, 0.0, img_data.width, img_data.height),
            Rect::from_xywh(x, y, img_data.width, img_data.height),
        ),
    };

    this.blit_pixels(img_data, &src, &dst);
    Ok(cx.undefined())
}

// -- image properties
// --------------------------------------------------------------

pub fn get_imageSmoothingEnabled(
    mut cx: FunctionContext,
) -> JsResult<JsBoolean> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let this = this.borrow_mut();
    // Ok(cx.boolean(this.state.image_smoothing_enabled))
    Ok(cx.boolean(this.state.sampling_filter.smoothing))
}

pub fn get_dither(mut cx: FunctionContext) -> JsResult<JsBoolean> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let this = this.borrow();
    Ok(cx.boolean(this.state.dither))
}

pub fn set_dither(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let mut this = this.borrow_mut();
    let flag = bool_arg(&mut cx, 1, "dither")?;
    this.state.dither = flag;
    Ok(cx.undefined())
}

pub fn get_imageSmoothingQuality(
    mut cx: FunctionContext,
) -> JsResult<JsString> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let this = this.borrow_mut();
    let mode = from_filter_quality(this.state.sampling_filter.quality);
    Ok(cx.string(mode))
}

pub fn set_imageSmoothingQuality(
    mut cx: FunctionContext,
) -> JsResult<JsUndefined> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let mut this = this.borrow_mut();
    let name = string_arg(&mut cx, 1, "imageSmoothingQuality")?;

    if let Some(mode) = to_filter_quality(&name) {
        this.state.sampling_filter.quality = mode;
    }
    Ok(cx.undefined())
}

//
// Typography
//

pub fn fillText(cx: FunctionContext) -> JsResult<JsUndefined> {
    _draw_text(cx, Fill)
}

pub fn strokeText(cx: FunctionContext) -> JsResult<JsUndefined> {
    _draw_text(cx, Stroke)
}

fn _draw_text(
    mut cx: FunctionContext,
    style: PaintStyle,
) -> JsResult<JsUndefined> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let mut this = this.borrow_mut();
    let text = string_arg(&mut cx, 1, "text")?;
    let x = float_arg_or_bail(&mut cx, 2, "x")?;
    let y = float_arg_or_bail(&mut cx, 3, "y")?;
    let width = opt_float_arg(&mut cx, 4);

    // it's fine to include an ignored `undefined` but anything else is invalid
    if width.is_none()
        && cx.len() > 4
        && !cx.argument::<JsValue>(4)?.is_a::<JsUndefined, _>(&mut cx)
    {
        // emoji indicates that it will only throw in strict mode
        cx.throw_type_error("⚠️Expected a number for `width` as 4th arg")?
    }

    this.draw_text(&text, x, y, width, style);
    Ok(cx.undefined())
}

pub fn measureText(mut cx: FunctionContext) -> JsResult<JsValue> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let this = this.borrow();
    let text = string_arg(&mut cx, 1, "text")?;
    let width = opt_float_arg(&mut cx, 2);
    let extents = this.measure_text_extents(&text, width);
    Ok(js_text_metrics(&mut cx, &extents)?.upcast())
}

pub fn outlineText(mut cx: FunctionContext) -> JsResult<JsValue> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let this = this.borrow_mut();

    let text = string_arg(&mut cx, 1, "text")?;
    let width = match cx.len() {
        3 => Some(float_arg_or_bail(&mut cx, 2, "width")?),
        _ => None,
    };
    let path = this.outline_text(&text, width);
    let builder = PathBuilder::new_path(&path);
    Ok(cx.boxed(RefCell::new(Path2D { builder })).upcast())
}

// -- type properties
// ---------------------------------------------------------------

pub fn get_font(mut cx: FunctionContext) -> JsResult<JsString> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let this = this.borrow_mut();
    Ok(cx.string(this.state.font.clone()))
}

pub fn set_font(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let mut this = this.borrow_mut();
    if let Some(spec) = font_arg(&mut cx, 1)? {
        this.set_font(spec);
    }
    Ok(cx.undefined())
}

pub fn get_fontStretch(mut cx: FunctionContext) -> JsResult<JsString> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let this = this.borrow_mut();
    Ok(cx.string(from_width(this.state.font_width)))
}

pub fn set_fontStretch(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    if let Some(stretch) = opt_string_arg(&mut cx, 1) {
        let mut this = this.borrow_mut();
        this.set_font_width(to_width(&stretch));
    }
    Ok(cx.undefined())
}

pub fn get_textAlign(mut cx: FunctionContext) -> JsResult<JsString> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let this = this.borrow_mut();
    let mode = from_text_align(this.state.graf_style.text_align());
    Ok(cx.string(mode))
}

pub fn set_textAlign(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let mut this = this.borrow_mut();
    let name = string_arg(&mut cx, 1, "textAlign")?;

    if let Some(mode) = to_text_align(&name) {
        this.state.graf_style.set_text_align(mode);
    }
    Ok(cx.undefined())
}

pub fn get_textBaseline(mut cx: FunctionContext) -> JsResult<JsString> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let this = this.borrow_mut();
    let mode = from_text_baseline(this.state.text_baseline);
    Ok(cx.string(mode))
}

pub fn set_textBaseline(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let mut this = this.borrow_mut();
    let name = string_arg(&mut cx, 1, "textBaseline")?;

    if let Some(mode) = to_text_baseline(&name) {
        this.state.text_baseline = mode;
    }
    Ok(cx.undefined())
}

pub fn get_direction(mut cx: FunctionContext) -> JsResult<JsString> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let this = this.borrow_mut();
    let name = match this.state.graf_style.text_direction() {
        TextDirection::LTR => "ltr",
        TextDirection::RTL => "rtl",
    };
    Ok(cx.string(name))
}

pub fn set_direction(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let mut this = this.borrow_mut();
    let name = string_arg(&mut cx, 1, "direction")?;

    let direction = match name.to_lowercase().as_str() {
        "ltr" => Some(TextDirection::LTR),
        "rtl" => Some(TextDirection::RTL),
        // The third value the Canvas API defines, and it was being dropped:
        // assigning it left whatever was set, so `direction = "rtl"` then
        // `direction = "inherit"` stayed right-to-left. `inherit` means
        // "take the canvas element's computed direction", and a canvas with
        // no document around it has none -- Chrome resolves that to `ltr`,
        // which is what this now does rather than nothing.
        "inherit" => Some(TextDirection::LTR),
        _ => None,
    };

    if let Some(dir) = direction {
        this.state.graf_style.set_text_direction(dir);
    }
    Ok(cx.undefined())
}

pub fn get_letterSpacing(mut cx: FunctionContext) -> JsResult<JsString> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let this = this.borrow_mut();
    Ok(cx.string(this.state.letter_spacing.to_string()))
}

pub fn set_letterSpacing(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let mut this = this.borrow_mut();

    if let Some(spacing) = opt_spacing_arg(&mut cx, 1)? {
        let em_size = this.state.char_style.font_size();
        this.state
            .char_style
            .set_letter_spacing(spacing.in_px(em_size));
        this.state.letter_spacing = spacing;
    }
    Ok(cx.undefined())
}

pub fn get_wordSpacing(mut cx: FunctionContext) -> JsResult<JsString> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let this = this.borrow_mut();
    Ok(cx.string(this.state.word_spacing.to_string()))
}

pub fn set_wordSpacing(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let mut this = this.borrow_mut();

    if let Some(spacing) = opt_spacing_arg(&mut cx, 1)? {
        let em_size = this.state.char_style.font_size();
        this.state
            .char_style
            .set_word_spacing(spacing.in_px(em_size));
        this.state.word_spacing = spacing;
    }
    Ok(cx.undefined())
}

// -- non-standard typography extensions
// --------------------------------------------

pub fn get_fontHinting(mut cx: FunctionContext) -> JsResult<JsBoolean> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let this = this.borrow_mut();
    Ok(cx.boolean(this.state.font_hinting))
}

pub fn set_fontHinting(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let mut this = this.borrow_mut();
    let flag = bool_arg(&mut cx, 1, "fontHinting")?;
    this.state.font_hinting = flag;
    Ok(cx.undefined())
}

pub fn get_fontVariant(mut cx: FunctionContext) -> JsResult<JsString> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let this = this.borrow_mut();
    Ok(cx.string(this.state.font_variant.clone()))
}

pub fn set_fontVariant(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let mut this = this.borrow_mut();
    let arg = cx.argument::<JsObject>(1)?;

    let variant = string_for_key(&mut cx, &arg, "variant")?;
    let feat_obj: Handle<JsObject> = arg.get(&mut cx, "features")?;
    let features = font_features(&mut cx, &feat_obj)?;
    this.set_font_variant(&variant, &features);
    Ok(cx.undefined())
}

pub fn get_textWrap(mut cx: FunctionContext) -> JsResult<JsBoolean> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let this = this.borrow_mut();
    Ok(cx.boolean(this.state.text_wrap))
}

pub fn set_textWrap(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let mut this = this.borrow_mut();
    let flag = bool_arg(&mut cx, 1, "textWrap")?;
    this.state.text_wrap = flag;
    Ok(cx.undefined())
}

pub fn get_textDecoration(mut cx: FunctionContext) -> JsResult<JsString> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let this = this.borrow_mut();
    Ok(cx.string(this.state.text_decoration.css.clone()))
}

pub fn set_textDecoration(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    if let Ok(Some(deco_style)) = decoration_arg(&mut cx, 1) {
        let mut this = this.borrow_mut();
        this.state.text_decoration = deco_style;
    }

    Ok(cx.undefined())
}

pub fn get_fontVariationSettings(
    mut cx: FunctionContext,
) -> JsResult<JsString> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let this = this.borrow();
    Ok(cx.string(this.state.font_variation_settings.clone()))
}

pub fn set_fontVariationSettings(
    mut cx: FunctionContext,
) -> JsResult<JsUndefined> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let mut this = this.borrow_mut();
    let arg = cx.argument::<JsObject>(1)?;

    let keys = arg.get_own_property_names(&mut cx)?.to_vec(&mut cx)?;
    let mut variations: Vec<(FourByteTag, f32)> = vec![];
    let mut css_parts: Vec<String> = vec![];

    for key_val in keys.iter() {
        let key = key_val
            .downcast_or_throw::<JsString, _>(&mut cx)?
            .value(&mut cx);
        if key.len() == 4 {
            let val: Handle<JsNumber> = arg.get(&mut cx, key.as_str())?;
            let value = val.value(&mut cx) as f32;
            let bytes = key.as_bytes();
            let tag = FourByteTag::from_chars(
                bytes[0] as char,
                bytes[1] as char,
                bytes[2] as char,
                bytes[3] as char,
            );
            variations.push((tag, value));
            css_parts.push(format!("\"{}\" {}", key, value));
        }
    }

    if variations.is_empty() {
        this.state.font_variation_settings = "normal".to_string();
    } else {
        this.state.font_variation_settings = css_parts.join(", ");
    }
    this.state.variations = variations;
    Ok(cx.undefined())
}

//
// Effects
//

// -- compositing properties
// --------------------------------------------------------

pub fn get_globalAlpha(mut cx: FunctionContext) -> JsResult<JsNumber> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let this = this.borrow_mut();
    Ok(cx.number(this.state.global_alpha))
}

pub fn get_globalCompositeOperation(
    mut cx: FunctionContext,
) -> JsResult<JsString> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let this = this.borrow_mut();
    let mode = from_blend_mode(this.state.global_composite_operation);
    Ok(cx.string(mode))
}

pub fn set_globalCompositeOperation(
    mut cx: FunctionContext,
) -> JsResult<JsUndefined> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let mut this = this.borrow_mut();
    let name = string_arg(&mut cx, 1, "globalCompositeOperation")?;

    if let Some(mode) = to_blend_mode(&name) {
        this.state.global_composite_operation = mode;
        this.state.paint.set_blend_mode(mode);
    }
    Ok(cx.undefined())
}

// -- css3 filters
// ------------------------------------------------------------------

pub fn get_filter(mut cx: FunctionContext) -> JsResult<JsString> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let this = this.borrow_mut();
    Ok(cx.string(this.state.filter.to_string()))
}

pub fn set_filter(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let mut this = this.borrow_mut();
    if !cx.argument::<JsValue>(1)?.is_a::<JsNull, _>(&mut cx) {
        // `None` is a declaration that did not parse, and the Canvas API
        // ignores one of those: the filter already in place stands.
        if let Some((filter_text, specs)) = filter_arg(&mut cx, 1)?
            && filter_text != this.state.filter.to_string()
        {
            this.state.filter = Filter::new(&filter_text, &specs);
        }
    }
    Ok(cx.undefined())
}

// -- dropshadow properties
// ---------------------------------------------------------

pub fn get_shadowBlur(mut cx: FunctionContext) -> JsResult<JsNumber> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let this = this.borrow_mut();
    Ok(cx.number(this.state.shadow_blur))
}

pub fn get_shadowColor(mut cx: FunctionContext) -> JsResult<JsValue> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let this = this.borrow_mut();
    let shadow_color = this.state.shadow_color;
    color_to_css(&mut cx, &shadow_color)
}

pub fn set_shadowColor(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let mut this = this.borrow_mut();
    if let Some(color) = opt_color_arg(&mut cx, 1) {
        this.state.shadow_color = color;
    }
    Ok(cx.undefined())
}

pub fn get_shadowOffsetX(mut cx: FunctionContext) -> JsResult<JsNumber> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let this = this.borrow_mut();
    Ok(cx.number(this.state.shadow_offset.x))
}

pub fn get_shadowOffsetY(mut cx: FunctionContext) -> JsResult<JsNumber> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let this = this.borrow_mut();
    Ok(cx.number(this.state.shadow_offset.y))
}

// -- Skia filter properties (CanvasKit parity)
// --------------------------------------------------------

pub fn get_colorFilter(mut cx: FunctionContext) -> JsResult<JsValue> {
    // Return null - the JS wrapper caches the actual object reference
    Ok(cx.null().upcast())
}

pub fn set_colorFilter(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let mut this = this.borrow_mut();

    let arg = cx.argument::<JsValue>(1)?;
    if arg.is_a::<JsNull, _>(&mut cx) || arg.is_a::<JsUndefined, _>(&mut cx) {
        this.state.skia_color_filter = None;
    } else {
        let filter = arg.downcast_or_throw::<BoxedColorFilter, _>(&mut cx)?;
        if filter.borrow().is_deleted() {
            return cx.throw_error("ColorFilter has been deleted");
        }
        this.state.skia_color_filter = Some(filter.borrow().inner.clone());
    }
    Ok(cx.undefined())
}

pub fn get_skiaImageFilter(mut cx: FunctionContext) -> JsResult<JsValue> {
    // Return null - the JS wrapper caches the actual object reference
    Ok(cx.null().upcast())
}

pub fn get_skiaMaskFilter(mut cx: FunctionContext) -> JsResult<JsValue> {
    // Return null - the JS wrapper caches the actual object reference.
    Ok(cx.null().upcast())
}

pub fn set_skiaMaskFilter(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let mut this = this.borrow_mut();

    let arg = cx.argument::<JsValue>(1)?;
    if arg.is_a::<JsNull, _>(&mut cx) || arg.is_a::<JsUndefined, _>(&mut cx) {
        this.state.skia_mask_filter = None;
    } else {
        let filter = arg.downcast_or_throw::<BoxedMaskFilter, _>(&mut cx)?;
        if filter.borrow().is_deleted() {
            return cx.throw_error("MaskFilter has been deleted");
        }
        this.state.skia_mask_filter = Some(filter.borrow().inner.clone());
    }
    Ok(cx.undefined())
}

pub fn set_skiaImageFilter(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let mut this = this.borrow_mut();

    let arg = cx.argument::<JsValue>(1)?;
    if arg.is_a::<JsNull, _>(&mut cx) || arg.is_a::<JsUndefined, _>(&mut cx) {
        this.state.skia_image_filter = None;
    } else {
        let filter = arg.downcast_or_throw::<BoxedImageFilter, _>(&mut cx)?;
        if filter.borrow().is_deleted() {
            return cx.throw_error("ImageFilter has been deleted");
        }
        this.state.skia_image_filter = Some(filter.borrow().inner.clone());
    }
    Ok(cx.undefined())
}
