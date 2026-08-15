#![allow(non_snake_case)]
use crate::{
    canvas::{DEFAULT_HEIGHT, DEFAULT_WIDTH},
    context::page::{ExportOptions, pages_arg},
    gpu,
    utils::*,
};
use neon::prelude::*;
use serde_json::json;
use skia_safe::{ColorSpace, ColorType, SurfaceProps};
use std::{
    cell::RefCell,
    panic::{AssertUnwindSafe, catch_unwind},
};

/// Runs the encode for an asynchronous export, off the JavaScript thread.
///
/// Two things have to happen out here and neither is optional.
///
/// The pool needs an autorelease pool, because Metal's `objc` allocations
/// have nowhere to go without one -- see [`gpu::autorelease`].
///
/// And a panic must not cross back into `rayon`. `rayon` aborts the process
/// when one escapes a `spawn`, printing "Rayon: detected unexpected panic;
/// aborting", which is a `SIGABRT` no `catch` or `.catch()` can reach. That
/// is a different outcome from the same panic on the JavaScript thread,
/// where Neon turns it into a catchable `Error: internal error in Neon
/// module` -- so before this, one bug was a rejected promise through
/// `toBufferSync` and a dead process through `toBuffer`, and the
/// asynchronous form is the one the README and the declarations show.
///
/// This does not make panicking acceptable. Every `unwrap` reachable from an
/// export is still a defect, the message a caller gets is still opaque, and
/// the right fix for each is still a `Result`. What it buys is that a
/// process serving requests survives one, and that the two entry points
/// agree about what a failure is.
///
/// `AssertUnwindSafe` is the honest annotation rather than a workaround: the
/// captured state is moved in and dropped here, so nothing observes it after
/// the unwind.
fn encoded_offthread<T>(
    encode: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    catch_unwind(AssertUnwindSafe(|| gpu::autorelease(encode))).unwrap_or_else(
        |panic| {
            // The payload is whatever `panic!` was given: a `&str` for a
            // literal, a `String` for a formatted message, and neither for a
            // panic from somewhere that used something else.
            let detail = panic
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| panic.downcast_ref::<&str>().copied())
                .unwrap_or("no message");
            Err(format!("internal error while encoding: {detail}"))
        },
    )
}

pub type BoxedCanvas = JsBox<RefCell<Canvas>>;
impl Finalize for Canvas {}

pub struct Canvas {
    pub width: f32,
    pub height: f32,
    pub text_contrast: f64,
    pub text_gamma: f64,
    pub gpu_disabled: bool,
    pub color_type: ColorType,
    pub color_space: ColorSpace,
    /// The canonical name the caller asked for, kept so `colorSpace` can be
    /// read back. Skia's `ColorSpace` cannot be reversed to a name.
    pub color_space_name: &'static str,
    engine: Option<gpu::RenderingEngine>,
}

impl Canvas {
    pub fn new(
        text_contrast: f64,
        text_gamma: f64,
        gpu_disabled: bool,
        color_type: ColorType,
        color_space: ColorSpace,
        color_space_name: &'static str,
    ) -> Self {
        Canvas {
            width: DEFAULT_WIDTH,
            height: DEFAULT_HEIGHT,
            text_contrast,
            text_gamma,
            gpu_disabled,
            color_type,
            color_space,
            color_space_name,
            engine: None,
        }
    }

    pub fn engine(&mut self) -> gpu::RenderingEngine {
        // Seeded from `gpu_disabled`, not from the global default. Rendering
        // already honours the flag, so defaulting here reported GPU for a
        // canvas that was rasterizing on the CPU -- `canvas.gpu` disagreed
        // with `canvas.engine.renderer` for the object's whole life.
        let disabled = self.gpu_disabled;
        let color_type = self.color_type;
        *self.engine.get_or_insert_with(|| {
            if disabled {
                return gpu::RenderingEngine::CPU;
            }
            let engine = gpu::RenderingEngine::default();
            // As on the Rust side: a float canvas the GPU cannot composite
            // is rasterised rather than silently narrowed to eight bits.
            match engine.can_composite(color_type) {
                true => engine,
                false => gpu::RenderingEngine::CPU,
            }
        })
    }

    pub fn export_options(&self) -> ExportOptions {
        ExportOptions {
            text_contrast: self.text_contrast as _,
            text_gamma: self.text_gamma as _,
            color_type: self.color_type,
            // Both, and the same: drawing happens in the canvas's space, and
            // an export that names no space of its own stays there.
            color_space: self.color_space.clone(),
            surface_color_space: self.color_space.clone(),
            // The canvas composites in its own format too: a float canvas in
            // float, everything else at eight bits. `color_type` above is
            // what a readback converts into.
            surface_color_type: self.color_type,
            ..Default::default()
        }
    }
}

//
// -- Javascript Methods
// --------------------------------------------------------------------------
//

pub fn new(mut cx: FunctionContext) -> JsResult<BoxedCanvas> {
    let opts = cx.argument::<JsObject>(1)?;
    let text_contrast =
        opt_double_for_key(&mut cx, &opts, "textContrast").unwrap_or(0.0);
    let (min_c, max_c) = (
        SurfaceProps::MIN_CONTRAST_INCLUSIVE as _,
        SurfaceProps::MAX_CONTRAST_INCLUSIVE as _,
    );
    if text_contrast < min_c || text_contrast > max_c {
        return cx.throw_range_error(format!(
            "Expected a number between {} and {} for `textContrast`",
            min_c, max_c
        ));
    }

    let mut text_gamma =
        opt_double_for_key(&mut cx, &opts, "textGamma").unwrap_or(1.4);
    let (min_g, max_g) = (
        SurfaceProps::MIN_GAMMA_INCLUSIVE as _,
        SurfaceProps::MAX_GAMMA_EXCLUSIVE as _,
    );
    if text_gamma == max_g {
        text_gamma -= f32::EPSILON as f64
    }; // nudge down values right at the max
    if text_gamma < min_g || text_contrast > max_g {
        return cx.throw_range_error(format!(
            "Expected a number between {} and {} for `textGamma`",
            min_g, max_g
        ));
    }

    let gpu_enabled = bool_for_key(&mut cx, &opts, "gpu")?;
    // Thrown on rather than substituted, as `colorSpace` below already was.
    // This is the path where the silence cost the most: the canvas keeps the
    // type for its whole life, so a misspelling here quietly composited
    // every page and every export at the default.
    let color_type = match opt_string_for_key(&mut cx, &opts, "colorType") {
        Some(mode) => color_type_or_throw(&mut cx, &mode)?,
        None => ColorType::RGBA8888,
    };
    let requested = opt_string_for_key(&mut cx, &opts, "colorSpace");
    let color_space = match requested.as_deref() {
        Some(mode) => color_space_or_throw(&mut cx, mode)?,
        None => ColorSpace::new_srgb(),
    };
    let color_space_name = requested
        .as_deref()
        .and_then(canonical_color_space)
        .unwrap_or("srgb");
    let this = RefCell::new(Canvas::new(
        text_contrast,
        text_gamma,
        !gpu_enabled,
        color_type,
        color_space,
        color_space_name,
    ));
    Ok(cx.boxed(this))
}

pub fn get_width(mut cx: FunctionContext) -> JsResult<JsNumber> {
    let this = cx.argument::<BoxedCanvas>(0)?;
    let width = this.borrow().width;
    Ok(cx.number(width as f64))
}

pub fn get_height(mut cx: FunctionContext) -> JsResult<JsNumber> {
    let this = cx.argument::<BoxedCanvas>(0)?;
    let height = this.borrow().height;
    Ok(cx.number(height as f64))
}

pub fn set_width(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let this = cx.argument::<BoxedCanvas>(0)?;
    let width = float_arg_or_bail(&mut cx, 1, "size")?;
    if width < 0.0 {
        cx.throw_range_error("⚠️Dimensions must be non-zero")?
    }
    this.borrow_mut().width = width;
    Ok(cx.undefined())
}

pub fn set_height(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let this = cx.argument::<BoxedCanvas>(0)?;
    let height = float_arg_or_bail(&mut cx, 1, "size")?;
    if height < 0.0 {
        cx.throw_range_error("⚠️Dimensions must be non-zero")?
    }
    this.borrow_mut().height = height;
    Ok(cx.undefined())
}

/// The canvas's own pixel format, which every export and `getImageData` from it
/// inherits unless the call names one. Readable so the JS layer can size the
/// `ImageData` it wraps around the returned buffer.
pub fn get_colorType(mut cx: FunctionContext) -> JsResult<JsString> {
    let this = cx.argument::<BoxedCanvas>(0)?;
    let color_type = this.borrow().color_type;
    Ok(cx.string(from_color_type(color_type)))
}

pub fn get_colorSpace(mut cx: FunctionContext) -> JsResult<JsString> {
    let this = cx.argument::<BoxedCanvas>(0)?;
    let name = this.borrow().color_space_name;
    Ok(cx.string(name))
}

pub fn get_engine(mut cx: FunctionContext) -> JsResult<JsString> {
    let this = cx.argument::<BoxedCanvas>(0)?;
    let mut this = this.borrow_mut();
    Ok(cx.string(from_engine(this.engine())))
}

pub fn set_engine(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let this = cx.argument::<BoxedCanvas>(0)?;
    if let Some(engine_name) = opt_string_arg(&mut cx, 1)
        && let Some(new_engine) = to_engine(&engine_name)
        && new_engine.selectable()
    {
        this.borrow_mut().gpu_disabled =
            matches!(new_engine, gpu::RenderingEngine::CPU);
        this.borrow_mut().engine = Some(new_engine)
    }

    Ok(cx.undefined())
}

pub fn get_engine_status(mut cx: FunctionContext) -> JsResult<JsString> {
    let this = cx.argument::<BoxedCanvas>(0)?;
    let mut this = this.borrow_mut();

    let mut details = this.engine().status(this.gpu_disabled);
    details["textContrast"] = json!(this.text_contrast);
    details["textGamma"] = json!(this.text_gamma);
    Ok(cx.string(details.to_string()))
}

pub fn toBuffer(mut cx: FunctionContext) -> JsResult<JsPromise> {
    let this = cx.argument::<BoxedCanvas>(0)?;
    let defaults = this.borrow().export_options();
    let options = export_options_arg(&mut cx, 2, &defaults)?;
    let mut pages = pages_arg(&mut cx, 1, &options, &this)?;

    // ensure cached bitmaps are sendable to other thread
    pages.materialize(&this.borrow_mut().engine(), &options);

    let channel = cx.channel();
    let (deferred, promise) = cx.promise();
    rayon::spawn_fifo(move || {
        let result = encoded_offthread(|| {
            if options.spans_pages() && pages.len() > 1 {
                pages.encoded_spanning(options)
            } else {
                pages.first().encoded_as(options, pages.engine)
            }
        });

        deferred.settle_with(&channel, move |mut cx| {
            let data = result.or_else(|err| cx.throw_error(err))?;
            let buffer = JsBuffer::from_slice(&mut cx, &data)?;
            Ok(buffer)
        });
    });

    Ok(promise)
}

pub fn toBufferSync(mut cx: FunctionContext) -> JsResult<JsValue> {
    let this = cx.argument::<BoxedCanvas>(0)?;
    let defaults = this.borrow().export_options();
    let options = export_options_arg(&mut cx, 2, &defaults)?;
    let pages = pages_arg(&mut cx, 1, &options, &this)?;

    let encoded = gpu::autorelease(|| {
        if options.spans_pages() && pages.len() > 1 {
            pages.encoded_spanning(options)
        } else {
            pages.first().encoded_as(options, pages.engine)
        }
    });

    match encoded {
        Ok(data) => {
            let buffer = JsBuffer::from_slice(&mut cx, &data)?;
            Ok(buffer.upcast::<JsValue>())
        }
        Err(msg) => cx.throw_error(msg),
    }
}

pub fn save(mut cx: FunctionContext) -> JsResult<JsPromise> {
    let this = cx.argument::<BoxedCanvas>(0)?;
    let name_pattern = string_arg(&mut cx, 2, "filePath")?;
    let sequence = !cx.argument::<JsValue>(3)?.is_a::<JsUndefined, _>(&mut cx);
    let padding = opt_float_arg(&mut cx, 3).unwrap_or(-1.0);
    let defaults = this.borrow().export_options();
    let options = export_options_arg(&mut cx, 4, &defaults)?;
    let mut pages = pages_arg(&mut cx, 1, &options, &this)?;

    // ensure cached bitmaps are sendable to other thread
    pages.materialize(&this.borrow_mut().engine(), &options);

    let channel = cx.channel();
    let (deferred, promise) = cx.promise();
    rayon::spawn_fifo(move || {
        let result = encoded_offthread(|| {
            if sequence {
                pages.write_sequence(&name_pattern, padding, options)
            } else if options.spans_pages() {
                pages.write_spanning(&name_pattern, options)
            } else {
                pages.write_image(&name_pattern, options)
            }
        });

        deferred.settle_with(&channel, move |mut cx| match result {
            Err(msg) => cx.throw_error(format!("I/O Error: {}", msg)),
            _ => Ok(cx.undefined()),
        });
    });

    Ok(promise)
}

pub fn saveSync(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let this = cx.argument::<BoxedCanvas>(0)?;
    let name_pattern = string_arg(&mut cx, 2, "filePath")?;
    let sequence = !cx.argument::<JsValue>(3)?.is_a::<JsUndefined, _>(&mut cx);
    let padding = opt_float_arg(&mut cx, 3).unwrap_or(-1.0);
    let defaults = this.borrow().export_options();
    let options = export_options_arg(&mut cx, 4, &defaults)?;
    let pages = pages_arg(&mut cx, 1, &options, &this)?;

    let result = gpu::autorelease(|| {
        if sequence {
            pages.write_sequence(&name_pattern, padding, options)
        } else if options.spans_pages() {
            pages.write_spanning(&name_pattern, options)
        } else {
            pages.write_image(&name_pattern, options)
        }
    });

    match result {
        Ok(_) => Ok(cx.undefined()),
        Err(msg) => cx.throw_error(msg),
    }
}

#[cfg(test)]
mod tests {
    use super::encoded_offthread;

    #[test]
    fn a_panic_becomes_an_error_rather_than_an_unwind() {
        // The whole point of the barrier. Escaping this function on a
        // `rayon` worker is what aborted the process, so what is asserted is
        // that nothing escapes -- reaching the assertion at all is half the
        // test, and the other half is that the payload survives into the
        // message a caller sees.
        let from_literal =
            encoded_offthread(|| -> Result<(), String> { panic!("a literal") });
        assert_eq!(
            from_literal,
            Err("internal error while encoding: a literal".to_string())
        );

        // Formatted panics carry a `String` rather than a `&str`, which is a
        // separate downcast and was worth covering: an index out of bounds,
        // the panic that started this, is one of these.
        let from_format = encoded_offthread(|| -> Result<(), String> {
            // Through `black_box`, or `#[deny(unconditional_panic)]` refuses
            // to compile an index the compiler can prove is out of range --
            // which is the same panic, just decided a phase too early to be
            // the one under test.
            let empty: Vec<u8> = Vec::new();
            let at = std::hint::black_box(0);
            assert_eq!(empty[at], 0);
            Ok(())
        });
        let message = from_format.expect_err("indexing empty should panic");
        assert!(
            message.starts_with("internal error while encoding: "),
            "unexpected message: {message}"
        );
        assert!(
            message.contains("index out of bounds")
                || message.contains("the len is 0"),
            "the cause should survive into the message: {message}"
        );
    }

    #[test]
    fn a_value_passes_through_untouched() {
        // The barrier must not change the ordinary path, in either direction.
        assert_eq!(encoded_offthread(|| Ok::<_, String>(7)), Ok(7));
        assert_eq!(
            encoded_offthread(|| Err::<u8, _>("refused".to_string())),
            Err("refused".to_string())
        );
    }
}
