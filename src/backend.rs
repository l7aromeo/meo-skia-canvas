use std::fmt;

use serde_json::Value;

use crate::{
    error::Error,
    gpu::{RenderingEngine, get_backend_status},
    pixels::SurfaceOptions,
    surface::Surface,
};

/// Selects the rasterizer that backs a `Surface`.
///
/// - `Auto` picks a GPU backend when one is compiled in *and*
///   runtime-available, falling back to CPU otherwise. This is the default and
///   the right choice for almost every consumer.
/// - `Cpu` forces the raster path. Useful for deterministic snapshots and tests
///   where GPU drivers would introduce variance.
/// - `Gpu` requires GPU acceleration. Surface construction returns
///   [`Error::EngineUnavailable`] when no GPU backend is compiled in or the
///   runtime cannot reach a device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum RenderEngine {
    /// Uses a GPU backend when one is available, CPU otherwise.
    #[default]
    Auto,
    /// Always rasterize on the CPU.
    Cpu,
    /// Require a GPU backend, failing if none can be reached.
    Gpu,
}

/// The renderer that an `Auto` resolution settled on, or that a
/// caller-fixed `Cpu` / `Gpu` choice represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EngineKind {
    /// Skia's raster backend.
    Cpu,
    /// A hardware backend -- Metal on macOS, Vulkan elsewhere.
    Gpu,
}

impl fmt::Display for EngineKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Cpu => "cpu",
            Self::Gpu => "gpu",
        })
    }
}

/// Diagnostic snapshot of the renderer that a [`RenderEngine`] would
/// resolve to. Carries the same information the Node-side
/// `gpu::get_backend_status` JSON exposes, in typed form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineStatus {
    /// Which rasterizer this resolves to.
    pub renderer: EngineKind,
    /// Concrete API name when `renderer == Gpu` (`"vulkan"`, `"metal"`).
    /// `None` for CPU.
    pub api: Option<String>,
    /// Human-readable adapter name, e.g. `"Apple M2"`. For a caller-pinned
    /// CPU renderer this says so rather than naming the idle GPU.
    pub device: String,
    /// Driver version string when the backend reports a usable one.
    pub driver: Option<String>,
    /// Worker threads in the rasterization pool.
    pub threads: usize,
    /// `true` when a GPU backend is compiled in *and* runtime-reachable.
    /// Independent of the requested [`RenderEngine`].
    pub is_gpu_available: bool,
    /// Sets when a GPU backend is compiled in but failed to initialize at
    /// runtime (driver mismatch, missing libs, ...). For pure-CPU builds
    /// or successful GPU init this is `None`.
    pub error: Option<String>,
}

/// Entry point for the Rust-only `meo-skia-canvas` API. Owns construction
/// of surfaces and reports renderer status; cheap to create, no GPU
/// context until a surface is built.
#[derive(Debug, Default)]
pub struct Backend {
    _private: (),
}

impl Backend {
    /// Creates a backend handle. Does no GPU work.
    pub fn new() -> Self {
        Self { _private: () }
    }

    /// Builds a surface `width` by `height` pixels.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidDimensions`] if either dimension is zero,
    /// [`Error::EngineUnavailable`] if `options` pins
    /// [`RenderEngine::Gpu`] and no device can be reached, and
    /// [`Error::SurfaceCreate`] if Skia declines the allocation.
    pub fn create_surface(
        &self,
        width: u32,
        height: u32,
        options: SurfaceOptions,
    ) -> Result<Surface, Error> {
        Surface::new(width, height, options)
    }

    /// Diagnostic snapshot of the renderer that `engine` resolves to.
    /// Side-effect free; safe to call before `create_surface`.
    pub fn engine_status(&self, engine: RenderEngine) -> EngineStatus {
        engine_status(engine)
    }
}

/// Internal mapping from the public [`RenderEngine`] to the Node-side
/// [`RenderingEngine`]. `Gpu` returns an error when the runtime cannot
/// reach a device; `Auto` quietly falls back to CPU.
pub(crate) fn resolve_engine(
    engine: RenderEngine,
) -> Result<RenderingEngine, Error> {
    match engine {
        RenderEngine::Auto => Ok(RenderingEngine::default()),
        RenderEngine::Cpu => Ok(RenderingEngine::CPU),
        RenderEngine::Gpu => {
            if RenderingEngine::GPU.selectable() {
                Ok(RenderingEngine::GPU)
            } else {
                Err(Error::EngineUnavailable {
                    engine,
                    reason: RenderingEngine::GPU
                        .lacks_gpu_support()
                        .unwrap_or_else(|| {
                            "GPU backend not selectable".to_string()
                        }),
                })
            }
        }
    }
}

/// Narrows the internal engine enum to the public one.
pub(crate) fn engine_kind_from(engine: RenderingEngine) -> EngineKind {
    match engine {
        RenderingEngine::CPU => EngineKind::Cpu,
        RenderingEngine::GPU => EngineKind::Gpu,
    }
}

fn engine_status(engine: RenderEngine) -> EngineStatus {
    let raw = get_backend_status();
    let is_gpu_available = raw
        .get("gpuAvailable")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let renderer = match (engine, is_gpu_available) {
        (RenderEngine::Cpu, _)
        | (RenderEngine::Auto, false)
        | (RenderEngine::Gpu, false) => EngineKind::Cpu,
        (RenderEngine::Auto, true) | (RenderEngine::Gpu, true) => {
            EngineKind::Gpu
        }
    };
    let device = match (renderer, is_gpu_available) {
        (EngineKind::Cpu, true) => {
            // GPU compiled and reachable, but caller asked for Cpu. The
            // raw blob describes the GPU; substitute a CPU label so
            // callers do not see "Apple M2" when they pinned to Cpu.
            "CPU-based renderer (manually selected)".to_string()
        }
        _ => raw
            .get("device")
            .and_then(Value::as_str)
            .unwrap_or("unknown device")
            .to_string(),
    };
    let api = if matches!(renderer, EngineKind::Gpu) {
        raw.get("api").and_then(Value::as_str).map(str::to_owned)
    } else {
        None
    };
    let driver = raw
        .get("driver")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty() && *s != "N/A")
        .map(str::to_owned);
    let threads = raw
        .get("threads")
        .and_then(Value::as_u64)
        .map(|n| n as usize)
        .unwrap_or_else(rayon::current_num_threads);
    let error = raw.get("error").and_then(Value::as_str).map(str::to_owned);
    EngineStatus {
        renderer,
        api,
        device,
        driver,
        threads,
        is_gpu_available,
        error,
    }
}
