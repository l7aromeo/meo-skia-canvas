#![allow(non_snake_case)]
use neon::prelude::*;
use skia_safe::{
    IRect, Matrix, PaintStyle,
    PaintStyle::{Fill, Stroke},
    Path, PathBuilder, PathDirection, PathFillType, Point, RRect, Rect, Size,
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
        font_library::FontLibrary,
        image::{Content, Source},
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
    // Enum names, which travel beside the numbers rather than in them. These
    // are the writes that made recording worth almost nothing before there was
    // somewhere to put a string: a drawing sets a style far more often than it
    // draws, and a setter that has to cross hands over everything queued
    // behind it.
    //
    // A name the enum does not have is ignored, as it was before -- the Canvas
    // API says an unrecognised value leaves the property alone.
    // Colours, which is what a drawing sets most often -- 4915 property
    // writes a frame on `examples/node/animated-eye.js`, nearly all of them
    // one of these two.
    //
    // Declared under its own name rather than replacing `set_fillStyle`,
    // because that one also takes a gradient, a pattern, a texture or a
    // shader, and a handle has nowhere to go in a batch yet. The writer picks
    // this verb only when the value is a string and lets everything else cross
    // as it always did.
    set_fillStyleText as SetFillStyleText (fillStyle @ text) => |ctx| {
        if let Some((color, space)) = css_to_color4f_in_space(fillStyle) {
            ctx.state.fill_style = Dye::Color(color, Some(space));
        }
    },

    set_strokeStyleText as SetStrokeStyleText (strokeStyle @ text) => |ctx| {
        if let Some((color, space)) = css_to_color4f_in_space(strokeStyle) {
            ctx.state.stroke_style = Dye::Color(color, Some(space));
        }
    },

    // Text state, all of it names from a fixed set. An unrecognised name
    // leaves the property alone, as the Canvas API says and as these did
    // before they were declared.
    // Drawing what has been built. The no-argument forms are ordinary verbs;
    // the ones taking a `Path2D` carry it in the lane beside the numbers,
    // copied as it stood when the call was made.
    //
    // `fill` and `stroke` themselves stay hand-written below, because their
    // argument list is variable -- a path, a rule, both or neither -- and a
    // record has one fixed shape. The JavaScript side picks the verb that
    // matches the call it was given.
    fillPage as FillPage () => |ctx| {
        ctx.draw_path(None, PaintStyle::Fill, Some(PathFillType::Winding));
    },

    fillPageEvenOdd as FillPageEvenOdd () => |ctx| {
        ctx.draw_path(None, PaintStyle::Fill, Some(PathFillType::EvenOdd));
    },

    strokePage as StrokePage () => |ctx| {
        ctx.draw_path(None, PaintStyle::Stroke, None);
    },

    fillPath2D as FillPath2D (path @ handle, rule @ text) => |ctx| {
        let rule = match rule {
            "evenodd" => PathFillType::EvenOdd,
            _ => PathFillType::Winding,
        };
        ctx.draw_path(Some(path), PaintStyle::Fill, Some(rule));
    },

    strokePath2D as StrokePath2D (path @ handle) => |ctx| {
        ctx.draw_path(Some(path), PaintStyle::Stroke, None);
    },

    // Clipping, in the same three shapes as filling.
    // A dash pattern, which is a list rather than a number, so it travels in
    // the lane beside the buffer. An odd-length pattern is doubled, as the
    // Canvas API says: five on, five off means the same as five on, five off,
    // five on, five off.
    setLineDash as SetLineDash (segments @ numbers) => |ctx| {
        let mut intervals: Vec<f32> = segments
            .iter()
            .copied()
            .filter(|n| *n >= 0.0 && n.is_finite())
            .collect();
        if intervals.len() == segments.len() {
            if intervals.len() % 2 == 1 {
                intervals.append(&mut intervals.clone());
            }
            ctx.state.line_dash_list = intervals;
        }
    },

    clipPage as ClipPage () => |ctx| {
        ctx.clip_path(None, PathFillType::Winding);
    },

    clipPageEvenOdd as ClipPageEvenOdd () => |ctx| {
        ctx.clip_path(None, PathFillType::EvenOdd);
    },

    clipPath2D as ClipPath2D (path @ handle, rule @ text) => |ctx| {
        let rule = match rule {
            "evenodd" => PathFillType::EvenOdd,
            _ => PathFillType::Winding,
        };
        ctx.clip_path(Some(path), rule);
    },

    // The six-number form. A `DOMMatrix` is an object, so a call written that
    // way crosses as it always did.
    transformNumbers as TransformNumbers (a, b, c, d, e, f) => |ctx| {
        let matrix = Matrix::new_all(a, c, e, b, d, f, 0.0, 0.0, 1.0);
        ctx.with_matrix(|ctm| ctm.pre_concat(&matrix));
    },

    setTransformNumbers as SetTransformNumbers (a, b, c, d, e, f) => |ctx| {
        let matrix = Matrix::new_all(a, c, e, b, d, f, 0.0, 0.0, 1.0);
        ctx.with_matrix(|ctm| ctm.reset().pre_concat(&matrix));
    },

    // A rounded rectangle whose corners are all the same radius, which is
    // what a number rather than an array means. Its `RRect` normalises the
    // rectangle, so the winding a negative dimension asks for has to be
    // carried by the direction rather than by the rectangle itself.
    roundRectUniform as RoundRectUniform (
        x, y, width, height, radius @ non_negative
    ) => |ctx| {
        let rect = Rect::from_xywh(x, y, width, height);
        let radii = [Point::new(radius, radius); 4];
        let rrect = RRect::new_rect_radii(rect, &radii);
        let direction = if width.signum() == height.signum() {
            PathDirection::CW
        } else {
            PathDirection::CCW
        };
        let path = Path::rrect(rrect, Some(direction));
        ctx.path.add_path_with_transform(
            &path,
            &ctx.state.matrix,
            AddPathMode::Extend,
        );
    },

    // Text state that is a name from a fixed set, and ignored when it is not
    // one -- as these did before they were declared.
    set_direction as SetDirection (direction @ text) => |ctx| {
        let direction = match direction.to_lowercase().as_str() {
            "ltr" => Some(TextDirection::LTR),
            "rtl" => Some(TextDirection::RTL),
            // `inherit` means the canvas element's computed direction, and a
            // canvas with no document around it has none -- Chrome resolves
            // that to `ltr`, which is what this does.
            "inherit" => Some(TextDirection::LTR),
            _ => None,
        };
        if let Some(direction) = direction {
            ctx.state.graf_style.set_text_direction(direction);
        }
    },

    set_lineDashFit as SetLineDashFit (lineDashFit @ text) => |ctx| {
        if let Some(fit) = to_1d_style(lineDashFit) {
            ctx.state.line_dash_fit = fit;
        }
    },

    set_textAlign as SetTextAlign (textAlign @ text) => |ctx| {
        if let Some(mode) = to_text_align(textAlign) {
            ctx.state.graf_style.set_text_align(mode);
        }
    },

    set_textBaseline as SetTextBaseline (textBaseline @ text) => |ctx| {
        if let Some(mode) = to_text_baseline(textBaseline) {
            ctx.state.text_baseline = mode;
        }
    },

    set_imageSmoothingQuality as SetImageSmoothingQuality (
        imageSmoothingQuality @ text
    ) => |ctx| {
        if let Some(mode) = to_filter_quality(imageSmoothingQuality) {
            ctx.state.sampling_filter.quality = mode;
        }
    },

    set_shadowColorText as SetShadowColorText (shadowColor @ text) => |ctx| {
        // A colour, so the same treatment as `fillStyle`: recorded when it is
        // written as a string, and crossing when it is anything else.
        if let Some(color) = css_to_color(shadowColor) {
            ctx.state.shadow_color = color;
        }
    },

    set_lineCap as SetLineCap (lineCap @ text) => |ctx| {
        if let Some(mode) = to_stroke_cap(lineCap) {
            ctx.state.paint.set_stroke_cap(mode);
        }
    },

    set_lineJoin as SetLineJoin (lineJoin @ text) => |ctx| {
        if let Some(mode) = to_stroke_join(lineJoin) {
            ctx.state.paint.set_stroke_join(mode);
        }
    },

    set_globalCompositeOperation as SetGlobalCompositeOperation (
        globalCompositeOperation @ text
    ) => |ctx| {
        if let Some(mode) = to_blend_mode(globalCompositeOperation) {
            ctx.state.global_composite_operation = mode;
            ctx.state.paint.set_blend_mode(mode);
        }
    },

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

    // An image and the numbers that place it. Three verbs rather than one,
    // because the count of coordinates is what says whether they are a
    // position, a destination rect, or a crop and a rect -- and a declared
    // verb has one arity. The counts that are none of those three keep their
    // error, which only `drawImage` itself can raise.
    //
    // `drawCanvas` is not here. It takes the same three shapes but wants its
    // source canvas as a picture rather than as pixels, and a slot resolves
    // what it was handed without being told which of the two the verb
    // wanted. Recording it would mean a second image kind, or resolving both
    // forms of every source -- and `drawCanvas` composites a page where
    // these place a sprite, so it is called once where they are called
    // thousands of times.
    //
    // The argument names are the ones this call has always reported. A
    // two-coordinate `drawImage` names them `srcX` and `srcY` even though
    // they place the destination, because the entry point takes the first
    // two names off one list of eight; renaming them here would reword an
    // error for no reason but tidiness.
    drawImageAt as DrawImageAt (source @ image, srcX, srcY) => |ctx| {
        _draw_source(ctx, &source, &[srcX, srcY]);
    },

    drawImageIn as DrawImageIn (
        source @ image, srcX, srcY, srcWidth, srcHeight
    ) => |ctx| {
        _draw_source(ctx, &source, &[srcX, srcY, srcWidth, srcHeight]);
    },

    drawImageCropped as DrawImageCropped (
        source @ image,
        srcX, srcY, srcWidth, srcHeight,
        dstX, dstY, dstWidth, dstHeight
    ) => |ctx| {
        _draw_source(ctx, &source, &[
            srcX, srcY, srcWidth, srcHeight, dstX, dstY, dstWidth, dstHeight,
        ]);
    },

    // Text, in the two shapes the call takes: with a width to fit into and
    // without one. Split for the same reason `drawImage` is, and the
    // argument names are again the ones the call has always reported.
    //
    // Laying a run out is most of what these cost -- 2230 ns for a
    // `fillText` against the 82 a crossing takes -- so the saving is not the
    // call's own. It is that a call which crosses has to hand over whatever
    // was queued behind it first, and a drawing that labels what it draws
    // ends a batch on every label.
    fillTextAt as FillTextAt (text @ text, x, y) => |ctx| {
        ctx.draw_text(text, x, y, None, Fill);
    },

    fillTextIn as FillTextIn (text @ text, x, y, width) => |ctx| {
        ctx.draw_text(text, x, y, Some(width), Fill);
    },

    strokeTextAt as StrokeTextAt (text @ text, x, y) => |ctx| {
        ctx.draw_text(text, x, y, None, Stroke);
    },

    strokeTextIn as StrokeTextIn (text @ text, x, y, width) => |ctx| {
        ctx.draw_text(text, x, y, Some(width), Stroke);
    },

    // Declared under its own name for the same reason the colours are: the
    // JavaScript side parses the CSS and hands over whatever it made of it,
    // which for a name this property does not have is `undefined`. A string
    // is recorded; anything else crosses to the hand-written setter, which
    // ignores it as it always has.
    set_fontStretchText as SetFontStretchText (fontStretch @ text) => |ctx| {
        ctx.set_font_width(to_width(fontStretch));
    },

    // The flags. Each arrives as a real boolean -- the JavaScript setter
    // coerces with `!!` before anything crosses -- so the trailing-flag form
    // that `arc` uses for `counterclockwise` reads them exactly. It is the
    // only thing that changes about them: `bool_arg` refused a non-boolean
    // and `bool_arg_or` reads one or takes false, which no caller going
    // through the property can tell apart.
    set_dither as SetDither (); dither => |ctx| {
        ctx.state.dither = dither;
    },

    set_fontHinting as SetFontHinting (); fontHinting => |ctx| {
        ctx.state.font_hinting = fontHinting;
    },

    set_textWrap as SetTextWrap (); textWrap => |ctx| {
        ctx.state.text_wrap = textWrap;
    },

    closePath as ClosePath () => |ctx| {
        ctx.path.close();
    },

    // The form with no bounds and no backdrop filter, which is the one a
    // compositing loop uses. The other two arguments are an array and a
    // boxed `ImageFilter`, and the call keeps them.
    //
    // Only when the alpha is a number a record can hold, and this one is
    // stricter than the others about it: a dropped record here would leave
    // no layer for the matching `restore` to pop, where a dropped `fillRect`
    // just paints nothing.
    saveLayerAlpha as SaveLayerAlpha (alpha) => |ctx| {
        let mut paint = Paint::default();
        paint.set_anti_alias(true);
        paint.set_alpha_f(alpha);
        paint.set_blend_mode(ctx.state.global_composite_operation);
        ctx.save_layer(Some(paint), None, None);
    },
}

pub fn new(mut cx: FunctionContext) -> JsResult<BoxedContext2D> {
    let parent = cx.argument::<BoxedCanvas>(1)?;
    let parent = parent.borrow();
    let this = RefCell::new(Context2D::new(
        parent.color_space.clone(),
        parent.color_type,
        (parent.width, parent.height),
    ));

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

/// The current transform, as the nine numbers `Matrix` holds.
///
/// Packed rather than set one at a time. A `JsArray` of nine costs nine
/// property sets through the binding, and this is a read a drawing makes
/// often -- `getTransform` is the same call. The same packing is what
/// `measureText` hands its numbers back in.
pub fn get_currentTransform(
    mut cx: FunctionContext,
) -> JsResult<JsFloat64Array> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let this = this.borrow();

    let matrix: [f64; 9] = std::array::from_fn(|i| this.state.matrix[i] as f64);
    JsFloat64Array::from_slice(&mut cx, &matrix)
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

    // A non-finite coordinate makes the call a no-op, as it does in the
    // other eight path methods and in a browser. It has to be detected
    // before the reader runs: `float_args` refuses a non-finite number and a
    // `Symbol` with the same message, because `_as_double` maps both to
    // `None`, so switching this method to the strict-only reader would have
    // made `roundRect` ignore a `Symbol` too -- which
    // `tests/suite/arguments.test.js` pins as a throw, and which a browser
    // throws for.
    if converts_to_non_finite(&mut cx, 1..13) {
        return Ok(cx.undefined());
    }

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
    // Which space the target is in, which decides what happens to the point
    // below. A `Path2D` argument is in its own user space and takes the
    // current transform at query time; the context's own path is accumulated
    // in device space, because every builder transforms a segment as it adds
    // it.
    let (rule_idx, mut target, target_is_path2d) = match path {
        Some(path) => (4, path, true),
        None => match cx.len() {
            5 => cx.throw_type_error("Expected a Path2D for 1st arg")?,
            _ => (3, this.path.snapshot(), false),
        },
    };

    let rule = match style {
        Stroke => None,
        _ => Some(fill_rule_arg_or(&mut cx, rule_idx, "nonzero")?),
    };

    if let [x, y] = opt_float_args(&mut cx, 1..4).as_slice() {
        // The standard says the point is "treated as coordinates in the
        // canvas coordinate space unaffected by the current transformation",
        // for both overloads. Against the context's own path that means
        // passing it through untouched -- it used to be mapped by the
        // inverse, so a matrix set after the path was built compared a
        // user-space point against device-space geometry. Against a `Path2D`
        // the mapping is what puts the two in the same space, and stays.
        let point = match target_is_path2d {
            true => this.in_local_coordinates(*x, *y),
            false => Point::new(*x, *y),
        };
        Ok(cx.boolean(this.hit_test_path(&mut target, point, rule, style)))
    } else {
        // Named rather than counted, so the message can say which argument
        // is missing. `_is_in` serves both methods, so the name follows the
        // style the caller reached it through.
        let method = match style {
            Stroke => "isPointInStroke",
            _ => "isPointInPath",
        };
        check_argc(&mut cx, method, &["x", "y"])?;
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
        Some(marker) => Ok(cx
            .boxed(RefCell::new(Path2D::from(marker.clone())))
            .upcast()),
        None => Ok(cx.null().upcast()),
    }
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

// line style properties
// -----------------------------------------------------------

pub fn get_lineCap(mut cx: FunctionContext) -> JsResult<JsString> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let this = this.borrow_mut();

    let mode = this.state.paint.stroke_cap();
    let name = from_stroke_cap(mode);
    Ok(cx.string(name))
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

/// The source and destination rects a `drawImage` maps between.
///
/// `None` where there is nothing to map: a coordinate count that is not 2, 4
/// or 8, or a source with no size of its own. Both are refused by name at the
/// entry point below, which is where a caller can still be told about them --
/// a declared verb has a fixed arity, so the first cannot reach this from a
/// record, and the second is a source that would paint nothing anyway.
fn _map_rects(intrinsic: Size, nums: &[f32]) -> Option<(Rect, Rect)> {
    if intrinsic.is_empty() {
        return None;
    }
    let whole = Rect::from_xywh(0.0, 0.0, intrinsic.width, intrinsic.height);
    Some(match nums {
        [x, y] => (
            whole,
            Rect::from_xywh(*x, *y, intrinsic.width, intrinsic.height),
        ),
        [x, y, width, height] => {
            (whole, Rect::from_xywh(*x, *y, *width, *height))
        }
        [sx, sy, sw, sh, dx, dy, dw, dh] => (
            Rect::from_xywh(*sx, *sy, *sw, *sh),
            Rect::from_xywh(*dx, *dy, *dw, *dh),
        ),
        _ => return None,
    })
}

/// The same, for a call that can still be told what was wrong with it.
fn _layout_rects(
    cx: &mut FunctionContext,
    intrinsic: Size,
    nums: &[f32],
) -> NeonResult<(Rect, Rect)> {
    match nums.len() {
        2 | 4 | 8 => (),
        9.. => cx.throw_type_error(format!(
            "⚠️Expected 2, 4, or 8 coordinates (got {})",
            nums.len()
        ))?,
        _ => cx.throw_type_error(format!(
            "not enough arguments: expected 2, 4, or 8 coordinates (got {})",
            nums.len()
        ))?,
    }

    match _map_rects(intrinsic, nums) {
        Some(rects) => Ok(rects),
        // The only way left for the mapping to fail, the count having just
        // been checked.
        None => cx.throw_range_error(format!(
            "Dimensions must be non-zero (got {}×{})",
            intrinsic.width, intrinsic.height
        )),
    }
}

/// Paints a resolved source, laid out by `nums`.
///
/// What `drawImage` does once its first argument has become something to
/// paint, shared by the entry point and by the three recorded verbs so that
/// the same sprite cannot land in two places depending on how it was called.
///
/// Paints nothing for a source still loading, for a broken one, and for a
/// count of coordinates that maps to no rectangle -- which is what the entry
/// point has always done with all three.
fn _draw_source(ctx: &mut Context2D, source: &Source, nums: &[f32]) {
    let Some((mut src, mut dst)) = _map_rects(source.content.size(), nums)
    else {
        return;
    };

    // A source that carries a picture rather than pixels is replayed wherever
    // this page is, so the page is charged what the source costs rather than
    // for one draw. Zero for an ordinary image, and for a canvas that has
    // already been flattened.
    if source.replay_cost > 0 {
        ctx.charge_replay(source.replay_cost);
    }

    match &source.content {
        // A source carrying someone else's picture is rasterized before it is
        // drawn, and only as far as this draw can show -- see
        // `Context2D::draw_nested_image`.
        Content::Bitmap(image) if source.nested => {
            let (src, dst) = source.content.snap_rects_to_bounds(src, dst);
            ctx.draw_nested_image(image, source.picture.as_ref(), &src, &dst);
        }
        Content::Bitmap(image) => {
            // A crop reaching outside the image is clipped to it, and the
            // destination clipped in the same proportion, which is what the
            // HTML specification says to do with one. Redundant here, and
            // kept for the shape: Skia hands the source rect to
            // `drawImageRect` under a `Strict` constraint and does that
            // clipping itself, so of thirty-five crops measured across every
            // kind of source, not one bitmap case moves. The picture below is
            // the one that needs it.
            let (src, dst) = source.content.snap_rects_to_bounds(src, dst);
            ctx.draw_image(image, &src, &dst);
        }
        Content::Vector(picture, size) => {
            // An SVG with no intrinsic size is scaled to the canvas instead,
            // except where the call gave a destination rect to scale to.
            if source.autosized && nums.len() != 4 {
                let canvas = ctx.bounds.size();
                let canvas_min = canvas.width.min(canvas.height);
                let picture_min = size.width.min(size.height);

                if nums.len() == 2 {
                    // No size given, so fit proportionally within the canvas.
                    let factor = canvas_min / picture_min;
                    dst = Rect::from_point_and_size(
                        (dst.x(), dst.y()),
                        dst.size() * factor,
                    );
                } else {
                    // Cropping, so map the crop as if the source were the
                    // size of the canvas.
                    let factor =
                        (size.width / canvas_min, size.height / canvas_min);
                    (src, _) = Matrix::scale(factor).map_rect(src);
                }
            }

            // Nothing constrains a picture the way `Strict` constrains a
            // bitmap. `draw_picture` maps the source rect onto the
            // destination and clips to the destination, and that clip is the
            // only bound there is -- so an SVG painting outside its own
            // viewport reached the part of the destination the crop had
            // excluded, until this started taking the clipped pair.
            let (src, dst) = source.content.snap_rects_to_bounds(src, dst);
            ctx.draw_picture(picture, &src, &dst, VectorFeatures::PLAIN);
        }
        Content::Loading | Content::Broken => (),
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

    // An `ImageData` reaches this and not the recorded verbs: its pixels are
    // a JavaScript array, and the caller can change one without crossing
    // anything that could hand a pending batch over first.
    let source = match Source::of(&mut cx, source) {
        Some(source) => source,
        None => Source {
            content: match image_data_arg(&mut cx, 1) {
                Ok(image_data) => Content::from_image_data(image_data),
                Err(_) => Content::default(),
            },
            autosized: false,
            replay_cost: 0,
            nested: false,
            picture: None,
        },
    };

    // Called for the messages it raises, which the shared painter has no
    // context to raise for itself.
    _layout_rects(&mut cx, source.content.size(), &nums)?;
    _draw_source(&mut this.borrow_mut(), &source, &nums);

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

    // How much of the source is nesting rather than drawing. A canvas is
    // kept as a picture so that a vector backend can still see through it,
    // and a picture drawn twice is replayed twice while being recorded once
    // -- so copying a page into a fresh canvas and drawing it back, round
    // after round, doubles the work of the eventual rasterization while the
    // recording grows by a constant. Twelve rounds took 3.5 seconds where
    // eleven took 1.8 and ten took 0.9.
    //
    // Past the cap the source is rasterized instead, which costs one replay
    // now and leaves the destination holding a bitmap that replays once
    // however often it is copied again. Only nesting is counted, so a page
    // with a hundred thousand ordinary draws on it never reaches this.
    let cost = context.borrow().replay_cost();
    let vector = cost == 0;

    let content = Content::from_context(&mut context.borrow_mut(), vector);
    let (src, dst) = _layout_rects(&mut cx, content.size(), &nums)?;
    let (src, dst) = content.snap_rects_to_bounds(src, dst);
    match &content {
        Content::Vector(pict, _) => {
            this.borrow_mut()
                .draw_picture_costing(pict, &src, &dst, source, cost);
            Ok(cx.undefined())
        }
        Content::Bitmap(image) => {
            this.borrow_mut().draw_image(image, &src, &dst);
            Ok(cx.undefined())
        }
        _ => cx.throw_error(
            "Canvas's PictureRecorder failed to generate an image",
        ),
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

    // Built from the `f64` products above rather than multiplied out again
    // in `f32`. Every edge here is a whole number -- the four arguments were
    // floored and `density` is a positive integer -- but `Rect` holds
    // `f32`s, which stop representing consecutive integers past 2^24. That
    // is reachable: #111 clamps a canvas dimension to exactly 2^24, so a
    // six-pixel read at x=16777213 has a right edge of 16777219, which is
    // not representable, rounds to 16777220, and returns a seven-pixel row
    // for a six-pixel request. `f64` holds every sum these can produce, and
    // the range check above has already bounded all four edges into `i32`.
    let crop =
        IRect::new(ox as i32, oy as i32, (ox + dw) as i32, (oy + dh) as i32);
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

pub fn get_imageSmoothingQuality(
    mut cx: FunctionContext,
) -> JsResult<JsString> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let this = this.borrow_mut();
    let mode = from_filter_quality(this.state.sampling_filter.quality);
    Ok(cx.string(mode))
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
    Ok(cx.boxed(RefCell::new(Path2D::from(path))).upcast())
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

    // The canonical string arrives on its own, ahead of the object it was
    // taken from, because it is the whole of the fast path: it names the
    // specification uniquely, so the object behind it only has to be read
    // the first time that name is seen. Reading it costs over a microsecond
    // -- ten keyed property lookups at roughly a hundred nanoseconds each,
    // then a typeface lookup -- where the CSS parse that produced it,
    // memoized on the JavaScript side, costs five nanoseconds.
    let font = match opt_string_arg(&mut cx, 1)
        .and_then(|name| FontLibrary::with_shared(|lib| lib.resolved(&name)))
    {
        Some(font) => Some(font),
        // Either the first time this font was named, or the string was not
        // one -- `null` for a specification JavaScript could not parse,
        // which reads the object at index 2 and finds nothing there either.
        None => match font_arg(&mut cx, 2)? {
            Some(spec) => FontLibrary::with_shared(|lib| lib.resolve(spec)),
            None => None,
        },
    };

    if let Some(font) = font {
        this.borrow_mut().set_font(&font);
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

pub fn get_textBaseline(mut cx: FunctionContext) -> JsResult<JsString> {
    let this = cx.argument::<BoxedContext2D>(0)?;
    let this = this.borrow_mut();
    let mode = from_text_baseline(this.state.text_baseline);
    Ok(cx.string(mode))
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
