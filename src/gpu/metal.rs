use objc2::{
    rc::{Retained, autoreleasepool},
    runtime::ProtocolObject,
};
use objc2_metal::{MTLCreateSystemDefaultDevice, MTLDevice, MTLDeviceLocation};
use serde_json::{Value, json};
use skia_safe::{
    ImageInfo, Surface,
    gpu::{
        Budgeted, DirectContext, DirectContextId, SurfaceOrigin,
        direct_contexts, mtl, surfaces,
    },
};
use std::{
    cell::RefCell,
    sync::OnceLock,
    time::{Duration, Instant},
};

use crate::{context::page::ExportOptions, gpu::ThreadBound};

mod recording;

#[cfg(test)]
pub(crate) use recording::{
    BLIND, BLIND_FOR_TEST, BUFFER_DISCARDED, FINISHED, ISSUED, WAITED,
};
pub(crate) use recording::{Mark, RecordingQueue};

thread_local!(
    static MTL_CONTEXT: RefCell<Option<MetalContext>> =
        const { RefCell::new(None) };
    /// This thread's offscreen context's queue of ours, and the id of the
    /// context it belongs to. Held beside `MTL_CONTEXT` rather than in it, so
    /// that a read reaches it without borrowing the context, which a caller
    /// may hold.
    static RECORDING: RefCell<
        Option<(DirectContextId, Retained<RecordingQueue>)>,
    > = const { RefCell::new(None) };
);

/// The queue of ours that `context`'s command buffers come from, where
/// `context` is this thread's offscreen context. `None` for any other: a
/// window's context makes its buffers on Metal's queue directly.
pub(crate) fn recording(
    context: &DirectContext,
) -> Option<Retained<RecordingQueue>> {
    let id = context.id();
    RECORDING.with_borrow(|held| {
        held.as_ref()
            .filter(|(of, _)| *of == id)
            .map(|(_, queue)| queue.clone())
    })
}
static MTL_CONTEXT_LIFESPAN: Duration = Duration::from_secs(5);
static MTL_STATUS: OnceLock<Value> = OnceLock::new();

//
// Offscreen rendering
//
pub struct MetalEngine {}

impl MetalEngine {
    pub fn api() -> Option<String> {
        Some("Metal".to_string())
    }

    /// Whether this engine has a GPU context to draw with, as probed once
    /// for the process -- see [`Self::probed`]. Read in place rather than
    /// through a clone of the status: a page that keeps a recording for its
    /// pixels asks it for every draw -- see `context::floats_draw_on_the_cpu`.
    pub fn supported() -> bool {
        Self::probed()["renderer"] == "GPU"
    }

    pub fn status() -> Value {
        Self::probed().clone()
    }

    /// The status, probed the first time it is asked for: whether a context
    /// can be made, and what it runs on. Set once and never written again
    /// -- `MTL_STATUS` is a `OnceLock` nothing else touches -- so what it says
    /// holds for the process.
    fn probed() -> &'static Value {
        MTL_STATUS.get_or_init(|| {
            // test whether a context can be created and do some one-time
            // init if so
            match MetalContext::new() {
                Some(context) => {
                    Self::spawn_idle_watcher(); // watch for inactive contexts and deallocate them

                    let device_name = format!(
                        "{} ({})",
                        match context.device.location() {
                            MTLDeviceLocation::BuiltIn => "Integrated GPU",
                            MTLDeviceLocation::Slot => "Discrete GPU",
                            MTLDeviceLocation::External => "External GPU",
                            _ => "Other GPU",
                        },
                        context.device.name()
                    );

                    json!({
                        "renderer": "GPU",
                        "api": "Metal",
                        "device": device_name,
                        "threads": rayon::current_num_threads(),
                    })
                }
                None => json!({
                    "renderer": "CPU",
                    "api": "Metal",
                    "device": "CPU-based renderer (Fallback)",
                    "threads": rayon::current_num_threads(),
                    "error": "GPU initialization failed",
                }),
            }
        })
    }

    fn spawn_idle_watcher() {
        // use a non-rayon thread so as not to compete with the worker threads
        std::thread::spawn(move || {
            loop {
                // run forever, watching the other threads in the pool
                std::thread::sleep(Duration::from_secs(1));
                rayon::spawn_broadcast(|_| Self::release_idle_context());
            }
        });
    }

    /// Hands back what this thread's context is holding, and drops the context
    /// itself once it has gone unused for [`MTL_CONTEXT_LIFESPAN`].
    ///
    /// Called for a `rayon` worker by the broadcast above, and by an export
    /// owner for itself -- see [`crate::gpu::owner`]. The owners are not
    /// `rayon` threads, so a broadcast cannot reach their thread-locals, and
    /// without this an owner's context would be held until the process ended.
    pub fn release_idle_context() {
        MTL_CONTEXT.with_borrow_mut(|cell| {
            cell.take_if(|engine| {
                engine.cleanup(); // it's unclear how effective this is...
                engine.last_use.elapsed() > MTL_CONTEXT_LIFESPAN
            });
        });
    }

    pub fn with_context<T, F>(f: F) -> Result<T, String>
    where
        F: FnOnce(&mut MetalContext) -> Result<T, String>,
    {
        match MetalEngine::supported() {
            false => Err("Metal API not supported".to_string()),
            true => MTL_CONTEXT.with_borrow_mut(|local_ctx| {
                autoreleasepool(|_|
                    // lazily initialize this thread's context...
                    local_ctx
                        .take()
                        .or_else(MetalContext::new)
                        .ok_or("Metal initialization failed".to_string())
                        .and_then(|ctx|{
                            f(local_ctx.insert(ctx))
                        }))
            }),
        }
    }

    pub fn with_direct_context<F>(f: F)
    where
        F: FnOnce(Option<&mut DirectContext>),
    {
        Self::with_context(|ctx| {
            f(Some(&mut ctx.context));
            Ok(())
        })
        .ok();
    }

    pub fn make_surface(
        image_info: &ImageInfo,
        opts: &ExportOptions,
    ) -> Result<Surface, String> {
        Self::with_context(|ctx| ctx.surface(image_info, opts))
    }
}

/// An Objective-C object as the opaque handle Skia takes.
///
/// The pointer is handed over unretained, which is what both consumers expect:
/// `mtl::BackendContext::new` and `mtl::TextureInfo::new` each retain what they
/// are given and release it when they drop. So the caller is free to let its
/// own `Retained` go -- as `MetalContext::new` does with Metal's command
/// queue, which only its queue of ours goes on holding.
fn handle_of<T>(object: &T) -> mtl::Handle {
    std::ptr::from_ref(object).cast()
}

pub struct MetalContext {
    device: Retained<ProtocolObject<dyn MTLDevice>>,
    context: DirectContext,
    msaa: Vec<usize>,
    last_use: Instant,
    /// This context is active on the thread that built it. See
    /// [`ThreadBound`].
    _thread: ThreadBound,
}

// UPSTREAM: skia-safe 0.156.0 -- rust-skia/rust-skia#1351 -- not worked around
// Re-check: cargo test a_read_between_draws_changes_no_later_pixel
//
// The page `MetalContext::surface` makes is multisampled, and a read between
// draws ends its render pass. Ganesh keeps the page's samples for the pass
// that follows, so a read changes nothing drawn after it. It does so because
// the Skia skia-safe builds, at rust-skia's fork commit dff0cfc969, replaces
// `GrMtlCaps::initGPUFamily`'s `@available` test with `false`: every
// Apple-silicon GPU is taken for a Mac of the first family, and
// `fPreferDiscardableMSAAAttachment`, which only the Apple family sets, is
// off. With it on, Ganesh resolves inside each pass and opens the next by
// loading the resolved pixel into every sample. A fill drawn over an edge
// after a read then lands up to 94 levels from the same draws unread in the
// scenes tried, and the test fails. Keeping reads invariant costs a resolve
// pass of its own after every pass; which of the two to have is a decision for
// when the family is read as Skia's own code reads it.
// `the_metal_caps_take_this_gpu_for_a_first_family_mac` reads the family.
impl MetalContext {
    fn new() -> Option<Self> {
        autoreleasepool(|_| {
            MTLCreateSystemDefaultDevice().and_then(|device| {
                // Skia makes its buffers on a queue of ours, so that a read
                // can confirm each one -- see `recording`.
                let queue = RecordingQueue::new(device.newCommandQueue()?);
                // SAFETY: `handle_of` explains the contract above --
                // `BackendContext::new` retains both handles and releases
                // them when it drops. Device and queue are live `Retained`
                // values here, so neither pointer dangles for the call.
                let backend = unsafe {
                    mtl::BackendContext::new(
                        handle_of(&*device),
                        handle_of(&*queue),
                    )
                };
                let last_use = Instant::now() + MTL_CONTEXT_LIFESPAN;
                // `1` is on the list because it is a count a caller can
                // reasonably name: it and `0` both mean one sample a pixel,
                // no multisampling. Metal reports it as supported and the
                // Vulkan backend already offered it, so leaving it out was
                // what made `msaa: 1` render on Linux and come back as "1x
                // MSAA not supported by GPU" on macOS.
                let msaa: Vec<usize> = [0, 1, 2, 4, 8, 16, 32]
                    .into_iter()
                    .filter(|s| {
                        *s == 0 || device.supportsTextureSampleCount(*s as _)
                    })
                    .collect();
                direct_contexts::make_metal(&backend, None).map(|context| {
                    RECORDING.set(Some((context.id(), queue)));
                    MetalContext {
                        device,
                        context,
                        msaa,
                        last_use,
                        _thread: ThreadBound::new(),
                    }
                })
            })
        })
    }

    fn surface(
        &mut self,
        image_info: &ImageInfo,
        opts: &ExportOptions,
    ) -> Result<Surface, String> {
        self.last_use = self.last_use.max(Instant::now());
        surfaces::render_target(
            &mut self.context,
            Budgeted::Yes,
            image_info,
            Some(opts.msaa_from(&self.msaa)?),
            SurfaceOrigin::BottomLeft,
            Some(&opts.surface_props()),
            false,
            None,
        )
        .ok_or(format!(
            "Could not allocate new {}×{} bitmap (color type: {:?})",
            image_info.width(),
            image_info.height(),
            image_info.color_type()
        ))
    }

    fn cleanup(&mut self) {
        self.context.free_gpu_resources();
        self.context
            .perform_deferred_cleanup(Duration::from_secs(1), None);
    }
}

//
// Windowed rendering
//

#[cfg(feature = "window")]
use {
    super::{RenderCache, RenderState::Resizing},
    crate::context::{layer::TileScope, page::Page},
    objc2_core_foundation::CGSize,
    objc2_foundation::NSString,
    objc2_metal::{MTLCommandBuffer, MTLCommandQueue, MTLPixelFormat},
    objc2_quartz_core::{CAMetalDrawable, CAMetalLayer},
    raw_window_metal::Layer,
    skia_safe::{
        Color, Matrix, Paint, SurfaceProps, canvas::SrcRectConstraint,
    },
    skia_safe::{ColorType, Image, Size, gpu::backend_render_targets, scalar},
    std::sync::Arc,
    winit::{
        dpi::PhysicalSize,
        event_loop::ActiveEventLoop,
        raw_window_handle::{HasWindowHandle, RawWindowHandle},
        window::Window,
    },
};

// Declared here rather than taken from a binding crate: objc2-quartz-core
// exposes `CALayer::setContentsGravity` but not the gravity constants
// themselves, and these two are the only ones this renderer needs.
#[cfg(feature = "window")]
#[allow(non_upper_case_globals)]
#[link(name = "QuartzCore", kind = "framework")]
unsafe extern "C" {
    static kCAGravityTopLeft: &'static NSString;
    static kCAGravityBottomLeft: &'static NSString;
}

#[cfg(feature = "window")]
pub struct MetalRenderer {
    window: Arc<Window>,
    backend: MetalBackend,
    layer: Retained<CAMetalLayer>,
    cache: RenderCache,
}

#[cfg(feature = "window")]
impl MetalRenderer {
    pub fn for_window(
        _event_loop: &ActiveEventLoop,
        window: Arc<Window>,
    ) -> Self {
        // SAFETY: Metal is always available on supported macOS hardware.
        let device =
            MTLCreateSystemDefaultDevice().expect("Metal device not found");

        let raw_window = window
            .window_handle()
            // SAFETY: Window handle is always available for active windows.
            .expect("Failed to retrieve a window handle")
            .as_raw();

        let raw_layer = match raw_window {
            // SAFETY: the handle comes from `raw-window-handle`, whose
            // contract is that the view pointer is valid for as long as the
            // window is, and the window outlives the layer built from it. The
            // variant is what decides which of the two is the right call.
            RawWindowHandle::AppKit(handle) => unsafe {
                Layer::from_ns_view(handle.ns_view)
            },
            RawWindowHandle::UiKit(handle) => unsafe {
                Layer::from_ui_view(handle.ui_view)
            },
            _ => panic!("Unsupported window handle type"),
        };

        // SAFETY: `into_raw` gives up ownership of a layer
        // `raw-window-metal` has already retained, so this adopts that
        // reference rather than taking a second one -- retaining again here
        // would leak the layer.
        let layer: Retained<CAMetalLayer> = unsafe {
            Retained::from_raw(raw_layer.into_raw().as_ptr().cast())
                // SAFETY: `into_raw` returns a `NonNull`, so the cast cannot
                // produce null.
                .expect("raw-window-metal returned no layer")
        };

        // A flipped layer draws from the bottom-left, so the gravity has to
        // match or the frame lands upside down.
        //
        // SAFETY: a message send to `layer`, which is a live `Retained` for
        // the whole of this scope, and the two gravity constants are statics
        // in the framework rather than anything this code owns.
        let gravity = unsafe {
            match layer.contentsAreFlipped() {
                true => kCAGravityBottomLeft,
                false => kCAGravityTopLeft,
            }
        };
        layer.setContentsGravity(gravity);

        layer.setDevice(Some(&device));
        layer.setPixelFormat(MTLPixelFormat::BGRA8Unorm);
        layer.setPresentsWithTransaction(false);
        layer.setDisplaySyncEnabled(true);
        layer.setFramebufferOnly(false); // to enable blend modes
        layer.setOpaque(false);

        let draw_size = window.inner_size();
        layer.setDrawableSize(CGSize::new(
            draw_size.width as f64,
            draw_size.height as f64,
        ));

        let backend = MetalBackend::for_layer(&layer);
        let cache = RenderCache::default();

        Self {
            window,
            layer,
            backend,
            cache,
        }
    }

    pub fn resize(&mut self, size: PhysicalSize<u32>) {
        let cg_size = CGSize::new(size.width as f64, size.height as f64);
        self.layer.setDrawableSize(cg_size);
        self.cache.state = Resizing;
    }

    pub fn draw(
        &mut self,
        page: &Page,
        matrix: Matrix,
        props: SurfaceProps,
        matte: Color,
    ) {
        let (clip, _) = matrix.map_rect(page.bounds);
        let dpr = self.window.scale_factor() as f32;
        let sync = self.cache.state == Resizing;

        let frame = self.backend.render_to_layer(
            &self.layer,
            &self.window,
            sync,
            &props,
            |canvas| {
                // draw background (either use raster cache or set to
                // window’s background color)
                canvas.clear(Color::TRANSPARENT);
                if let Some((image, src, dst)) =
                    self.cache.validate(page, matte, dpr, clip)
                {
                    canvas.draw_image_rect(
                        image,
                        Some((src, SrcRectConstraint::Strict)),
                        dst,
                        &Paint::default(),
                    );
                } else {
                    canvas.clear(matte);
                }

                // draw newly added vector layers
                canvas.scale((dpr, dpr)).clip_rect(clip, None, Some(true));
                let _tiles = TileScope::open();
                canvas.save();
                canvas.concat(&matrix);
                page.play_layers(self.cache.depth()..page.layers.len(), canvas);
                canvas.restore();
            },
        );

        match frame {
            Ok(frame) => self.cache.update(frame, page, matte, dpr, clip),
            Err(e) => eprintln!("MetalRenderer: draw failed: {}", e),
        }
    }
}

// Only the windowed renderer uses this: it draws into a `CAMetalLayer`, which
// exists to be presented on screen. Without the gate, `metal` on its own fails
// to compile on the winit types below.
#[cfg(feature = "window")]
pub struct MetalBackend {
    skia_ctx: DirectContext,
    queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
}

#[cfg(feature = "window")]
impl Drop for MetalBackend {
    fn drop(&mut self) {
        self.skia_ctx.abandon();
    }
}

#[cfg(feature = "window")]
impl MetalBackend {
    pub fn for_layer(layer: &CAMetalLayer) -> Self {
        let device = layer
            .device()
            // SAFETY: the layer was given a device in `for_window`, above.
            .expect("Metal layer has no device");
        let queue = device
            .newCommandQueue()
            // SAFETY: a queue is only refused once the device is exhausted,
            // and this is the first one asked of it.
            .expect("Could not create a Metal command queue");
        // SAFETY: `handle_of` explains the contract above --
        // `BackendContext::new` retains both handles and releases them when it
        // drops. Device and queue are live `Retained` values here, so neither
        // pointer dangles for the call, and that retain is what lets `device`
        // go out of scope at the end of this function. `queue` is kept in
        // `Self` because `render_to_layer` needs it, not to hold the handle
        // up; `MetalContext::new` makes the same call and drops its own.
        let backend_ctx = unsafe {
            mtl::BackendContext::new(handle_of(&*device), handle_of(&*queue))
        };
        let skia_ctx = direct_contexts::make_metal(&backend_ctx, None)
            // SAFETY: Metal context creation only fails on unsupported
            // hardware.
            .expect("Failed to create Metal Skia context");
        Self { skia_ctx, queue }
    }

    fn render_to_layer<F>(
        &mut self,
        layer: &CAMetalLayer,
        window: &Window,
        sync: bool,
        props: &SurfaceProps,
        f: F,
    ) -> Result<Image, String>
    where
        F: FnOnce(&skia_safe::Canvas),
    {
        let drawable = layer.nextDrawable().ok_or(
            "MetalBackend: could not allocate framebuffer".to_string(),
        )?;

        let drawable_size = {
            let size = layer.drawableSize();
            Size::new(size.width as scalar, size.height as scalar)
        };

        let texture = drawable.texture();
        // SAFETY: `TextureInfo::new` retains the texture it is handed, as
        // `handle_of` records, and `texture` is live for this scope. The
        // dimensions come from the layer that produced the drawable, so the
        // target describes the texture it is built from.
        let backend_render_target = unsafe {
            let texture_info = mtl::TextureInfo::new(handle_of(&*texture));
            backend_render_targets::make_mtl(
                (drawable_size.width as i32, drawable_size.height as i32),
                &texture_info,
            )
        };

        let mut surface = surfaces::wrap_backend_render_target(
            &mut self.skia_ctx,
            &backend_render_target,
            SurfaceOrigin::TopLeft,
            ColorType::BGRA8888,
            None,
            Some(props),
        )
        .ok_or("MetalBackend: could not create render target")?;

        // pass the suface's canvas to the user-provided callback
        f(surface.canvas());

        self.skia_ctx
            .flush_and_submit()
            .map_err(|error| format!("MetalBackend: {error}"))?;
        self.skia_ctx.free_gpu_resources();

        window.pre_present_notify();
        let command_buffer = self
            .queue
            .commandBuffer()
            .ok_or("MetalBackend: could not create a command buffer")?;
        command_buffer.presentDrawable(ProtocolObject::from_ref(&*drawable));
        command_buffer.commit();

        // during resizes, ensure drawing is complete before returning
        if sync {
            command_buffer.waitUntilCompleted();
        }

        Ok(surface.image_snapshot())
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "window")]
    use objc2_metal::MTLCreateSystemDefaultDevice;
    #[cfg(feature = "window")]
    use objc2_quartz_core::CAMetalLayer;
    use skia_safe::ColorType;

    use super::MetalContext;
    #[cfg(feature = "window")]
    use super::{MetalBackend, MetalEngine, recording::MADE};

    /// Ganesh's Metal caps take this GPU for a Mac of the first family --
    /// see the `UPSTREAM:` note in `MetalContext::surface`. The family is
    /// read where it shows: `GrMtlCaps::initFormatTable` makes
    /// `BGR10A2Unorm` texturable only on that family, so a `BGRA1010102`
    /// surface cannot be made, where on the Apple family and on a later Mac
    /// one it can.
    #[test]
    fn the_metal_caps_take_this_gpu_for_a_first_family_mac() {
        let Some(context) = MetalContext::new() else {
            return;
        };
        assert!(
            !context
                .context
                .color_type_supported_as_surface(ColorType::BGRA1010102),
            "Ganesh can make a BGRA1010102 surface, so its Metal caps no \
             longer take this GPU for a first-family Mac: re-check the \
             UPSTREAM note in MetalContext::surface"
        );
    }

    /// A window's context makes its buffers on Metal's queue: nothing reads
    /// back from a drawable, so there is nothing for a queue of ours to
    /// confirm.
    #[cfg(feature = "window")]
    #[test]
    fn a_window_s_context_makes_no_queue_of_ours() {
        if !MetalEngine::supported() {
            eprintln!("skipped: no Metal context");
            return;
        }
        let Some(device) = MTLCreateSystemDefaultDevice() else {
            return;
        };
        let layer = CAMetalLayer::new();
        layer.setDevice(Some(&device));
        let made = MADE.get();
        let backend = MetalBackend::for_layer(&layer);
        assert_eq!(
            MADE.get(),
            made,
            "the window's context made a queue of ours"
        );
        drop(backend);
    }
}
