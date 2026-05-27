#![allow(non_snake_case)]
use neon::prelude::*;
use skia_safe::{Shader as SkShader, shaders};
use std::cell::RefCell;

pub type BoxedShader = JsBox<RefCell<Shader>>;
impl Finalize for Shader {}

/// JS-facing handle for a reusable Skia shader. Currently the
/// procedural-noise factories (CanvasKit `Shader.MakeFractalNoise` /
/// `MakeTurbulence`); usable as a `fillStyle`/`strokeStyle`. The
/// gradient shader types are reachable via the Canvas2D
/// `createLinear/Radial/ConicGradient` factories.
#[derive(Clone)]
pub struct Shader {
    pub inner: SkShader,
    deleted: bool,
}

impl Shader {
    pub fn is_deleted(&self) -> bool {
        self.deleted
    }
}

fn wrap_shader<'a>(
    cx: &mut FunctionContext<'a>,
    result: Option<SkShader>,
) -> JsResult<'a, JsValue> {
    match result {
        Some(inner) => {
            let s = Shader {
                inner,
                deleted: false,
            };
            Ok(cx.boxed(RefCell::new(s)).upcast())
        }
        None => Ok(cx.null().upcast()),
    }
}

/// `Shader.MakeFractalNoise(baseFreqX, baseFreqY, octaves, seed)`.
pub fn makeFractalNoise(mut cx: FunctionContext) -> JsResult<JsValue> {
    let fx = cx.argument::<JsNumber>(1)?.value(&mut cx) as f32;
    let fy = cx.argument::<JsNumber>(2)?.value(&mut cx) as f32;
    let octaves = cx.argument::<JsNumber>(3)?.value(&mut cx) as usize;
    let seed = cx.argument::<JsNumber>(4)?.value(&mut cx) as f32;
    let result = shaders::fractal_noise((fx, fy), octaves, seed, None);
    wrap_shader(&mut cx, result)
}

/// `Shader.MakeTurbulence(baseFreqX, baseFreqY, octaves, seed)`.
pub fn makeTurbulence(mut cx: FunctionContext) -> JsResult<JsValue> {
    let fx = cx.argument::<JsNumber>(1)?.value(&mut cx) as f32;
    let fy = cx.argument::<JsNumber>(2)?.value(&mut cx) as f32;
    let octaves = cx.argument::<JsNumber>(3)?.value(&mut cx) as usize;
    let seed = cx.argument::<JsNumber>(4)?.value(&mut cx) as f32;
    let result = shaders::turbulence((fx, fy), octaves, seed, None);
    wrap_shader(&mut cx, result)
}

pub fn delete(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let this = cx.argument::<BoxedShader>(0)?;
    this.borrow_mut().deleted = true;
    Ok(cx.undefined())
}
