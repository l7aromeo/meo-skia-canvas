//! Native memory the JavaScript engine is told about.
//!
//! V8 schedules a collection by the size of its own heap, and a context is a
//! small object there however large the page it holds natively -- 3.2 MB for
//! a 1000 by 800 page once it has been read back. Unreported, a process that
//! made canvases without allocating much JavaScript was never collected:
//! eighty such canvases held 400 MB and none of them had been finalized.
//!
//! So each object that owns native memory keeps an [`External`], reports what
//! it holds through `napi_adjust_external_memory` as that changes, and
//! releases the same amount in its finalizer. What one object reports is what
//! it releases, on the engine instance that owns it, so the totals net to zero
//! per isolate -- a worker thread's canvases count against that worker.

use std::sync::OnceLock;

use neon::{
    context::Context,
    sys::bindings::{Env, Status},
};
#[cfg(debug_assertions)]
use neon::{context::FunctionContext, result::JsResult, types::JsValue};

/// `napi_adjust_external_memory`, as `node_api.h` declares it: the engine, the
/// change in bytes, and where to write the new total.
type AdjustExternalMemory = unsafe extern "C" fn(Env, i64, *mut i64) -> Status;

/// The function, looked up once in the host process, or `None` where the host
/// does not export it -- a runtime with partial Node-API coverage -- in which
/// case nothing is reported.
static ADJUST: OnceLock<Option<AdjustExternalMemory>> = OnceLock::new();

// UPSTREAM: neon 1.1.2 -- unfiled -- worked around
// Re-check: grep -rn adjust_external_memory in an upgraded neon's src
/// Looks the function up where Neon looks up every other Node-API function.
///
/// Neon links against no Node library: it resolves Node-API from the host
/// process when the module loads, so an `extern "C"` declaration of our own
/// would have nothing to link to on Windows. This resolves the one function
/// Neon does not bind the same way. On Windows the host is the executable,
/// which exports Node-API for addons -- the same handle every Neon call
/// already goes through; elsewhere it is the process's global symbol scope.
fn adjust() -> Option<AdjustExternalMemory> {
    *ADJUST.get_or_init(|| {
        #[cfg(not(windows))]
        let host: libloading::Library =
            libloading::os::unix::Library::this().into();
        #[cfg(windows)]
        let host: libloading::Library =
            libloading::os::windows::Library::this().ok()?.into();

        // SAFETY: `AdjustExternalMemory` restates the signature `node_api.h`
        // gives `napi_adjust_external_memory`, Node-API version 1. The pointer
        // outlives `host` because it points into the host executable, which
        // stays loaded for as long as any addon it loaded does.
        unsafe {
            host.get::<AdjustExternalMemory>(b"napi_adjust_external_memory\0")
        }
        .ok()
        .map(|function| *function)
    })
}

/// The native bytes one JavaScript object has reported, so that its finalizer
/// releases exactly that.
#[derive(Debug, Default)]
pub(crate) struct External {
    reported: i64,
}

impl External {
    /// Tells the engine this object now holds `bytes`, by reporting the change
    /// since the last call. Nothing is reported when nothing changed, and a
    /// change the engine refuses is retried by the next call rather than
    /// counted.
    pub fn hold<'a, C: Context<'a>>(&mut self, cx: &mut C, bytes: usize) {
        let bytes = i64::try_from(bytes).unwrap_or(i64::MAX);
        let change = bytes - self.reported;
        if change == 0 {
            return;
        }
        let Some(adjust) = adjust() else {
            return;
        };
        let mut total = 0;
        // SAFETY: `cx` is a live Neon context, so the env it hands over is the
        // engine this call runs on, and Neon only runs a context on that
        // engine's own thread. `total` is a valid place for the result.
        let status = unsafe { adjust(cx.to_raw(), change, &mut total) };
        if status == Status::Ok {
            self.reported = bytes;
        }
    }

    /// Releases everything this object reported, for its finalizer.
    pub fn release<'a, C: Context<'a>>(&mut self, cx: &mut C) {
        self.hold(cx, 0);
    }
}

/// Test-only: the engine's total of memory reported through Node-API, read
/// by adjusting it by nothing, or `null` where the host has no such function.
///
/// Read here rather than from `process.memoryUsage().external`, which on
/// Node 26 counts the engine's own external allocations -- array buffers --
/// and not what an addon reports: a page reported here moved this total by
/// its 3.2 MB and that figure by nothing.
///
/// Exported by debug builds only, as `__cachedParagraphs` is.
#[cfg(debug_assertions)]
pub fn external_memory(mut cx: FunctionContext) -> JsResult<JsValue> {
    let Some(adjust) = adjust() else {
        return Ok(cx.null().upcast());
    };
    let mut total = 0;
    // SAFETY: as in `External::hold`: a live context, on its own thread, and
    // a valid place for the result. A change of zero moves nothing.
    let status = unsafe { adjust(cx.to_raw(), 0, &mut total) };
    match status == Status::Ok {
        true => Ok(cx.number(total as f64).upcast()),
        false => Ok(cx.null().upcast()),
    }
}
