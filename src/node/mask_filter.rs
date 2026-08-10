#![allow(non_snake_case)]
use neon::prelude::*;
use skia_safe::{BlurStyle, MaskFilter as SkMaskFilter};
use std::cell::RefCell;

use crate::utils::{bool_arg_or, enum_arg_or, float_arg};

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

/// The `BlurStyle` names the JS side declares.
const BLUR_STYLES: &[(&str, BlurStyle)] = &[
    ("normal", BlurStyle::Normal),
    ("solid", BlurStyle::Solid),
    ("outer", BlurStyle::Outer),
    ("inner", BlurStyle::Inner),
];

/// `MaskFilter.MakeBlur(style, sigma, respectCTM?)`. `respectCTM`
/// defaults to `true` (the blur scales with the canvas transform).
pub fn makeBlur(mut cx: FunctionContext) -> JsResult<JsValue> {
    let style =
        enum_arg_or(&mut cx, 1, "style", BLUR_STYLES, BlurStyle::Normal)?;
    let sigma = float_arg(&mut cx, 2, "sigma")?;
    let respect_ctm = bool_arg_or(&mut cx, 3, true);
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
