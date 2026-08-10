#![allow(non_snake_case)]
use neon::prelude::*;
use skia_safe::{BlurStyle, MaskFilter as SkMaskFilter};
use std::cell::RefCell;

pub type BoxedMaskFilter = JsBox<RefCell<MaskFilter>>;
impl Finalize for MaskFilter {}

/// JS-facing handle for a Skia coverage-mask filter (styled blur).
/// Mirrors CanvasKit's `MaskFilter`; set on a context via
/// `ctx.maskFilter`.
#[derive(Clone)]
pub struct MaskFilter {
    pub inner: SkMaskFilter,
    deleted: bool,
}

impl MaskFilter {
    pub fn is_deleted(&self) -> bool {
        self.deleted
    }
}

/// Parses a `BlurStyle` from a string arg: `normal` (default) | `solid` |
/// `outer` | `inner`. Anything else falls back to `normal`.
fn parse_blur_style(cx: &mut FunctionContext, idx: usize) -> BlurStyle {
    match cx.argument_opt(idx) {
        Some(arg) if arg.is_a::<JsString, _>(cx) => {
            // SAFETY: `is_a::<JsString>` guard on the enclosing match arm.
            let s = arg.downcast::<JsString, _>(cx).unwrap().value(cx);
            match s.to_lowercase().as_str() {
                "solid" => BlurStyle::Solid,
                "outer" => BlurStyle::Outer,
                "inner" => BlurStyle::Inner,
                _ => BlurStyle::Normal,
            }
        }
        _ => BlurStyle::Normal,
    }
}

/// `MaskFilter.MakeBlur(style, sigma, respectCTM?)`. `respectCTM`
/// defaults to `true` (the blur scales with the canvas transform).
pub fn makeBlur(mut cx: FunctionContext) -> JsResult<JsValue> {
    let style = parse_blur_style(&mut cx, 1);
    let sigma = cx.argument::<JsNumber>(2)?.value(&mut cx) as f32;
    let respect_ctm = match cx.argument_opt(3) {
        Some(arg) if arg.is_a::<JsBoolean, _>(&mut cx) => {
            // SAFETY: `is_a::<JsBoolean>` guard on the match arm.
            arg.downcast::<JsBoolean, _>(&mut cx)
                .unwrap()
                .value(&mut cx)
        }
        _ => true,
    };
    match SkMaskFilter::blur(style, sigma, respect_ctm) {
        Some(inner) => {
            let mf = MaskFilter {
                inner,
                deleted: false,
            };
            Ok(cx.boxed(RefCell::new(mf)).upcast())
        }
        None => Ok(cx.null().upcast()),
    }
}

pub fn delete(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let this = cx.argument::<BoxedMaskFilter>(0)?;
    this.borrow_mut().deleted = true;
    Ok(cx.undefined())
}
