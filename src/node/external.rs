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
//! releases what it still reports in its finalizer. Memory freed for an
//! object by something else -- a canvas's page, dropped by the budget the
//! pages on a thread keep to during another canvas's read -- is released by
//! the next report any object on the thread makes, and the object's own count
//! goes to nothing with it -- see [`Reported`]. Every report and release is
//! made on the engine instance that owns the object, so the totals net to
//! zero per isolate -- a worker thread's canvases count against that worker.

#[cfg(all(debug_assertions, target_os = "linux", target_env = "gnu"))]
use libc::mallinfo2;
#[cfg(all(debug_assertions, target_os = "macos"))]
use libc::{malloc_statistics_t, malloc_zone_statistics};
#[cfg(debug_assertions)]
use std::sync::atomic::{AtomicI64, Ordering};
use std::{cell::Cell, rc::Rc, sync::OnceLock};

use neon::{
    context::Context,
    sys::bindings::{Env, Status},
};
#[cfg(debug_assertions)]
use neon::{context::FunctionContext, result::JsResult, types::JsNumber};

/// `napi_adjust_external_memory`, as `node_api.h` declares it: the engine, the
/// change in bytes, and where to write the new total.
type AdjustExternalMemory = unsafe extern "C" fn(Env, i64, *mut i64) -> Status;

/// The function, looked up once in the host process, or `None` where the host
/// does not export it -- a runtime with partial Node-API coverage -- in which
/// case nothing is reported.
static ADJUST: OnceLock<Option<AdjustExternalMemory>> = OnceLock::new();

/// Test-only: the bytes this addon has reported and the engine accepted,
/// summed over every object on every thread. See [`reported_memory`].
#[cfg(debug_assertions)]
static REPORTED: AtomicI64 = AtomicI64::new(0);

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
///
/// The Windows half is reasoned from Neon's source and has not been run
/// here. What checks it is the `Run tests` workflow's Windows rebuild,
/// dispatched before a release: there `dropped-canvas-memory.test.js` fails
/// if nothing is reported, as it failed on macOS without this module.
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
    reported: Reported,
}

/// What one object has told the engine it holds, shared with what can free
/// that memory without the object's own call: the budget the pages on a
/// thread keep to, which drops one canvas's page during another's read --
/// see `context::resident`.
#[derive(Debug, Default, Clone)]
pub(crate) struct Reported(Rc<Cell<i64>>);

thread_local! {
    /// What objects on this thread reported and has since been freed by
    /// something other than their own call -- see [`Reported::freed`] --
    /// and that the engine has not yet been told of.
    static FREED: Cell<i64> = const { Cell::new(0) };
}

impl Reported {
    /// Records that what this reported has been freed elsewhere. The engine
    /// is told at the next report any object on this thread makes, and the
    /// object goes on from nothing, so that its finalizer releases only what
    /// it reports after.
    pub fn freed(&self) {
        let bytes = self.0.replace(0);
        FREED.set(FREED.get() + bytes);
    }
}

/// Tells the engine `change`, answering whether it took it.
fn tell<'a, C: Context<'a>>(cx: &mut C, change: i64) -> bool {
    let Some(adjust) = adjust() else {
        return false;
    };
    let mut total = 0;
    // SAFETY: `cx` is a live Neon context, so the env it hands over is the
    // engine this call runs on, and Neon only runs a context on that engine's
    // own thread. `total` is a valid place for the result.
    let status = unsafe { adjust(cx.to_raw(), change, &mut total) };
    let told = status == Status::Ok;
    #[cfg(debug_assertions)]
    if told {
        REPORTED.fetch_add(change, Ordering::Relaxed);
    }
    told
}

impl External {
    /// What this object has reported, to share with what can free it -- see
    /// [`Reported`].
    pub fn reported(&self) -> Reported {
        self.reported.clone()
    }

    /// Tells the engine this object now holds `bytes`, by reporting the change
    /// since the last call, and tells it what objects on this thread have had
    /// freed for them since the last report. Nothing is reported when nothing
    /// changed, and a change the engine refuses is retried by the next call
    /// rather than counted.
    pub fn hold<'a, C: Context<'a>>(&mut self, cx: &mut C, bytes: usize) {
        let freed = FREED.get();
        if freed != 0 && tell(cx, -freed) {
            FREED.set(FREED.get() - freed);
        }
        let bytes = i64::try_from(bytes).unwrap_or(i64::MAX);
        let change = bytes - self.reported.0.get();
        if change != 0 && tell(cx, change) {
            self.reported.0.set(bytes);
        }
    }

    /// Releases everything this object reported, for its finalizer.
    pub fn release<'a, C: Context<'a>>(&mut self, cx: &mut C) {
        self.hold(cx, 0);
    }
}

/// Test-only: the bytes this addon's objects hold as the engine was told of
/// them, summed over every thread -- what the engine accepted and nothing
/// else.
///
/// Read here rather than from the total `napi_adjust_external_memory`
/// returns, which also counts every live array buffer in the process: a
/// 146,209-byte `Buffer` held across two reads moved it by exactly that on
/// Node 22, 24 and 26, so whatever else the process holds lands in a figure
/// read from it. Nor from `process.memoryUsage().external`, which on Node 26
/// did not move when a page was reported.
///
/// Exported by debug builds only, as `__cachedParagraphs` is.
#[cfg(debug_assertions)]
pub fn reported_memory(mut cx: FunctionContext) -> JsResult<JsNumber> {
    Ok(cx.number(REPORTED.load(Ordering::Relaxed) as f64))
}

/// The bytes the C allocator holds in use for the process, by its own
/// count: what the tests hold a report against -- see `CONTEXT_BYTES` --
/// since what a process holds in pages does not follow what it allocates.
/// On glibc, 4,000 canvases with a context held 4,590 to 4,613 bytes each
/// by this count over nine batches, where the resident set grew by 854 to
/// 5,890, as earlier batches' freed pages were reused or the heap grew.
///
/// Exported by debug builds on glibc and macOS only, where the allocator
/// answers: glibc's `mallinfo2`, its chunks in use in every arena and its
/// mapped ones, headers included, and macOS's `malloc_zone_statistics`
/// over every zone.
#[cfg(all(
    debug_assertions,
    any(target_os = "macos", all(target_os = "linux", target_env = "gnu"))
))]
pub fn malloc_in_use(mut cx: FunctionContext) -> JsResult<JsNumber> {
    Ok(cx.number(malloc_bytes_in_use() as f64))
}

#[cfg(all(debug_assertions, target_os = "linux", target_env = "gnu"))]
fn malloc_bytes_in_use() -> usize {
    // SAFETY: `mallinfo2` takes nothing and returns its counts by value; it
    // reads the allocator's own bookkeeping under its arena locks.
    let info = unsafe { mallinfo2() };
    info.uordblks + info.hblkhd
}

#[cfg(all(debug_assertions, target_os = "macos"))]
fn malloc_bytes_in_use() -> usize {
    let mut stats = malloc_statistics_t {
        blocks_in_use: 0,
        size_in_use: 0,
        max_size_in_use: 0,
        size_allocated: 0,
    };
    // SAFETY: a null zone asks for every zone's counts, summed, and
    // `stats` is a live struct of the type the call writes.
    unsafe { malloc_zone_statistics(std::ptr::null_mut(), &mut stats) };
    stats.size_in_use
}
