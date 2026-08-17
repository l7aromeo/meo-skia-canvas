use parking_lot::ReentrantMutex;
use serde_json::{Value, json};
use skia_safe::{
    ColorSpace, ColorType, ISize, ImageInfo, Surface,
    gpu::{
        Budgeted, DirectContext, SurfaceOrigin, direct_contexts, surfaces,
        vk::{BackendContext, GetProcOf},
    },
};
use std::{
    cell::RefCell,
    mem::ManuallyDrop,
    ptr,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicU32, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use vulkano::{
    Handle, VulkanLibrary, VulkanObject,
    device::{
        Device, DeviceCreateInfo, Queue, QueueCreateInfo, QueueFlags,
        physical::{PhysicalDevice, PhysicalDeviceType},
    },
    instance::{Instance, InstanceCreateFlags, InstanceCreateInfo},
};

use crate::{context::page::ExportOptions, gpu::ThreadBound};

thread_local!(
    static VK_CONTEXT: RefCell<Option<VulkanContext>> =
        const { RefCell::new(None) };
);
static VK_STATUS: OnceLock<Value> = OnceLock::new();
static VK_CONTEXT_LIFESPAN: Duration = Duration::from_secs(5);

/// The process's one handle on the Vulkan loader.
///
/// Held here for the life of the process rather than by each thread's own
/// context. `VulkanLibrary::new` dlopens the loader and the last `Arc` to go
/// closes it again, so loading it per context let the idle watcher unload the
/// loader out from under a thread that was busy opening it: the core dump put
/// the crash inside `vkEnumerateInstanceExtensionProperties`, on a null
/// function pointer, with `VulkanContext::new` two frames below it. Running
/// the test suite four threads wide against a Vulkan device segfaulted in
/// roughly half the runs before this and in none of nineteen after.
///
/// A hang under the same thread pressure was a second fault at a second
/// layer, fixed separately by [`VulkanShared`]: this shares the loader
/// handle, that shares the device the loader hands out.
///
/// The `OnceLock` also serialises the first load, which is where two threads
/// racing to dlopen would otherwise meet. A failed load is remembered as such:
/// a machine without the loader will not grow one part-way through a run, and
/// retrying the dlopen per context only repeated the work.
static VK_LIBRARY: OnceLock<Option<Arc<VulkanLibrary>>> = OnceLock::new();

/// The instance, device and queues this process renders through.
///
/// Built once and held for the life of the process. Each thread used to
/// create its own `Instance` and `Device`; vulkano hands those back as `Arc`s,
/// so the thread that exited last destroyed them -- and a `vkDestroyDevice`
/// running while other threads were mid-submit deadlocked inside the driver.
/// The stacks were unambiguous: one thread in a thread-local destructor,
/// through `drop_in_place<Device>` into `vkDestroyDevice`, and four others in
/// `GrVkGpu::onReadPixels` -> `submitToQueue` -> `submit_to_queue`, all parked
/// on `libnvidia-glsi` mutexes, two of them on the same one. The driver takes
/// process-global locks, so devices that are nominally independent contend.
///
/// Holding the `Arc`s here means a thread's clone is never the last one, so
/// the destroy path is unreachable rather than merely unlikely.
///
/// Vulkan requires a `VkQueue` to be externally synchronised, and Skia
/// submits on the queue its context was built with at moments we do not
/// choose: a `read_pixels` on a surface flushes and submits long after
/// `make_surface` returned and any lock it held was dropped. Skia's own
/// guidance is that a queue shared between contexts is allowed, so long as
/// the client finds a way to keep the submits from overlapping.
///
/// The way found here is that Skia does not resolve Vulkan functions itself:
/// it asks for each one through the `get_proc` callback it is handed. Giving
/// it [`locked_queue_submit`] in place of the driver's `vkQueueSubmit` puts
/// every submit it makes under the lock for that queue, wherever the call
/// comes from. So the queue count bounds contention, not correctness.
///
/// The device is created with as many queues as the family offers, capped, so
/// that ordinary use has one each and the lock is uncontended. Past that,
/// contexts share and the lock does its job. The locks are reentrant because
/// a submit can happen inside `with_context`, which already holds one.
struct VulkanShared {
    library: Arc<VulkanLibrary>,
    instance: Arc<Instance>,
    physical_device: Arc<PhysicalDevice>,
    device: Arc<Device>,
    queues: Vec<Arc<Queue>>,
    /// One lock per queue, taken around every submit and wait on it, and
    /// again while a context on it is torn down or trimmed. Uncontended while
    /// contexts outnumber queues no more than once.
    queue_locks: Vec<ReentrantMutex<()>>,
    /// Which queues a live context already submits through, one bit each.
    ///
    /// Queues used to be handed out as `counter % queues.len()`, where the
    /// counter counted threads ever created rather than threads alive, so a
    /// thread that outlived the next sixteen shared its queue with a newcomer
    /// while nothing serialised the two. The validation layer named it
    /// exactly: `vkQueueSubmit(): THREADING ERROR : object of type VkQueue is
    /// simultaneously used in current thread ... and thread ...`, and the
    /// NVIDIA driver answered a concurrent submit by faulting inside
    /// `libnvidia-eglcore` rather than returning an error.
    ///
    /// Tracking which are spoken for means a queue is shared only once there
    /// is no unused one left, and is free again the moment the context
    /// holding it goes away. Sharing is safe either way -- see
    /// [`locked_queue_submit`] -- this only keeps the lock uncontended.
    claimed: AtomicU32,
}

/// Where to resume sharing queues once every one of them is claimed.
static VK_NEXT_QUEUE: AtomicUsize = AtomicUsize::new(0);

/// How many queues to ask the device for.
///
/// Enough that ordinary use never shares one -- a render pool is sized to the
/// core count -- without asking a driver for more than it wants to give.
const MAX_QUEUES: usize = 16;

const _: () = assert!(
    MAX_QUEUES <= u32::BITS as usize,
    "VulkanShared::claimed holds one bit per queue"
);

static VK_SHARED: OnceLock<Option<VulkanShared>> = OnceLock::new();

/// The driver's own `vkQueueSubmit`, behind [`locked_queue_submit`].
static REAL_QUEUE_SUBMIT: OnceLock<ash::vk::PFN_vkQueueSubmit> =
    OnceLock::new();

/// The driver's own `vkQueueWaitIdle`, behind [`locked_queue_wait_idle`].
static REAL_QUEUE_WAIT_IDLE: OnceLock<ash::vk::PFN_vkQueueWaitIdle> =
    OnceLock::new();

/// Takes the lock belonging to a queue, by its handle.
///
/// `None` before the shared device exists, which is before any queue does,
/// and for a handle that is not one of ours -- neither reachable from Skia,
/// and in both cases the call is simply passed through.
fn lock_for_queue(
    queue: ash::vk::Queue,
) -> Option<parking_lot::ReentrantMutexGuard<'static, ()>> {
    let shared = VulkanShared::get()?;
    let raw = queue.as_raw();
    let index = shared
        .queues
        .iter()
        .position(|q| q.handle().as_raw() == raw)?;
    Some(shared.queue_locks[index].lock())
}

/// `vkQueueSubmit`, serialised against every other use of the same queue.
///
/// Skia resolves every Vulkan function through the `get_proc` callback it is
/// handed, so returning this in place of the driver's entry point is enough
/// to serialise submits it makes from inside a surface, which is where they
/// mostly happen and where no lock of ours could otherwise reach.
///
/// # Safety
///
/// Called only by Skia, with the arguments Vulkan specifies, and forwards
/// them unchanged to the pointer the driver gave for this device.
unsafe extern "system" fn locked_queue_submit(
    queue: ash::vk::Queue,
    submit_count: u32,
    submits: *const ash::vk::SubmitInfo<'_>,
    fence: ash::vk::Fence,
) -> ash::vk::Result {
    let _submitting = lock_for_queue(queue);
    match REAL_QUEUE_SUBMIT.get() {
        // SAFETY: the arguments are the caller's, passed straight through.
        Some(real) => unsafe { real(queue, submit_count, submits, fence) },
        None => ash::vk::Result::ERROR_UNKNOWN,
    }
}

/// `vkQueueWaitIdle`, under the same lock as a submit on that queue.
///
/// A wait is a write to the queue as far as Vulkan's synchronisation rules
/// are concerned, and Skia issues one whenever it finishes outstanding work.
///
/// # Safety
///
/// As [`locked_queue_submit`].
unsafe extern "system" fn locked_queue_wait_idle(
    queue: ash::vk::Queue,
) -> ash::vk::Result {
    let _submitting = lock_for_queue(queue);
    match REAL_QUEUE_WAIT_IDLE.get() {
        // SAFETY: the argument is the caller's, passed straight through.
        Some(real) => unsafe { real(queue) },
        None => ash::vk::Result::ERROR_UNKNOWN,
    }
}

impl VulkanShared {
    fn get() -> Option<&'static VulkanShared> {
        VK_SHARED.get_or_init(|| Self::build().ok()).as_ref()
    }

    fn build() -> Result<Self, String> {
        let library = VK_LIBRARY
            .get_or_init(|| VulkanLibrary::new().ok())
            .clone()
            .ok_or("Vulkan libraries not found on system")?;

        let instance = Instance::new(
            Arc::clone(&library),
            InstanceCreateInfo {
                flags: InstanceCreateFlags::ENUMERATE_PORTABILITY,
                ..Default::default()
            },
        )
        .or(Err("Could not create Vulkan instance"))?;

        let (physical_device, queue_family_index) = instance
            .enumerate_physical_devices()
            .or(Err("Vulkan: No physical devices found"))?
            // No need for swapchain extension support.
            .filter_map(|p| {
                p.queue_family_properties()
                    .iter()
                    .position(|q| {
                        q.queue_flags.intersects(QueueFlags::GRAPHICS)
                    })
                    .map(|i| (p, i as u32))
            })
            .min_by_key(|(p, _)| match p.properties().device_type {
                PhysicalDeviceType::DiscreteGpu => 0,
                PhysicalDeviceType::IntegratedGpu => 1,
                PhysicalDeviceType::VirtualGpu => 2,
                PhysicalDeviceType::Cpu => 3,
                PhysicalDeviceType::Other => 4,
                _ => 5,
            })
            .ok_or("No suitable Vulkan physical device found")?;

        let available = physical_device.queue_family_properties()
            [queue_family_index as usize]
            .queue_count as usize;
        let wanted = available.clamp(1, MAX_QUEUES);

        let (device, queues) = Device::new(
            physical_device.clone(),
            DeviceCreateInfo {
                queue_create_infos: vec![QueueCreateInfo {
                    queue_family_index,
                    queues: vec![0.5; wanted],
                    ..Default::default()
                }],
                ..Default::default()
            },
        )
        .or(Err("Failed to create Vulkan device"))?;

        let queues: Vec<Arc<Queue>> = queues.collect();
        if queues.is_empty() {
            return Err("Failed to create Vulkan graphics queue".to_string());
        }
        let queue_locks =
            (0..queues.len()).map(|_| ReentrantMutex::new(())).collect();

        Ok(Self {
            library,
            instance,
            physical_device,
            device,
            queues,
            queue_locks,
            claimed: AtomicU32::new(0),
        })
    }

    /// Takes a queue for a context to submit through.
    ///
    /// A queue of its own where there is one going spare, because then the
    /// lock around every submit is uncontended. Past that, contexts share,
    /// which costs contention and nothing else: what makes sharing safe is
    /// [`locked_queue_submit`], not exclusivity.
    ///
    /// The second return value says whether the queue is this context's alone
    /// and so whether giving it back is this context's to do.
    fn claim_queue(&self) -> (usize, bool) {
        let count = self.queues.len();
        loop {
            let taken = self.claimed.load(Ordering::Acquire);
            let Some(free) = (0..count).find(|i| taken & (1 << i) == 0) else {
                let shared =
                    VK_NEXT_QUEUE.fetch_add(1, Ordering::Relaxed) % count;
                return (shared, false);
            };

            if self
                .claimed
                .compare_exchange_weak(
                    taken,
                    taken | (1 << free),
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                return (free, true);
            }
        }
    }

    /// Gives a claimed queue back for the next context to take.
    fn release_queue(&self, index: usize) {
        self.claimed.fetch_and(!(1 << index), Ordering::AcqRel);
    }
}

#[derive(Debug)]
#[allow(dead_code)]
pub struct VulkanEngine {
    context: DirectContext,
    library: Arc<VulkanLibrary>,
    instance: Arc<Instance>,
    physical_device: Arc<PhysicalDevice>,
    device: Arc<Device>,
    queue: Arc<Queue>,
    last_use: Instant,
}

impl VulkanEngine {
    pub fn api() -> Option<String> {
        Some("Vulkan".to_string())
    }

    pub fn supported() -> bool {
        Self::status()["renderer"] == "GPU"
    }

    pub fn status() -> Value {
        VK_STATUS.get_or_init(||{
            // test whether a context can be created and do some one-time init if so
            let context = VulkanContext::new()
                .and_then(|mut ctx| match ctx.works(){
                    true => Ok(ctx),
                    false => Err("Vulkan device was instantiated but unable to render".to_string())
                });

            match context {
                Ok(context) => {
                    Self::spawn_idle_watcher(); // watch for inactive contexts and deallocate them

                    let device_props = context.physical_device.properties();
                    let gpu_type = match device_props.device_type {
                        PhysicalDeviceType::IntegratedGpu => Some("Integrated GPU"),
                        PhysicalDeviceType::DiscreteGpu => Some("Discrete GPU"),
                        PhysicalDeviceType::VirtualGpu => Some("Virtual GPU"),
                        _ => Some("Software Rasterizer")
                    };

                    json!({
                        "renderer": "GPU",
                        "api": "Vulkan",
                        "device": gpu_type.map(|t| format!("{} ({})",
                            t, device_props.device_name)
                        ),
                        "driver":format!("{} ({})",
                            device_props.driver_id.map(|id| format!("{:?}", id) ).unwrap_or("Unknown Driver".to_string()),
                            device_props.driver_info.as_ref().unwrap_or(&"Unknown Version".to_string()),
                        ),
                        "threads": rayon::current_num_threads(),
                    })
                },
                Err(msg) => json!({
                    "renderer": "CPU",
                    "api": "Vulkan",
                    "device": "CPU-based renderer (Fallback)",
                    "driver": "N/A",
                    "threads": rayon::current_num_threads(),
                    "error": msg,
                })
            }
        }).clone()
    }

    fn spawn_idle_watcher() {
        // use a non-rayon thread so as not to compete with the worker threads
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(Duration::from_secs(1));
                rayon::spawn_broadcast(|_| {
                    // Hand back what an idle context is holding, but keep the
                    // context: dropping it would free its queue for another
                    // thread while the images it has already handed out --
                    // the page cache keeps texture-backed ones -- can still
                    // reach it. The resources are the part worth reclaiming.
                    VK_CONTEXT.with_borrow_mut(|cell| {
                        if let Some(engine) = cell.as_mut()
                            && engine.last_use.elapsed() > VK_CONTEXT_LIFESPAN
                        {
                            engine.cleanup();
                        }
                    });
                });
            }
        });
    }

    pub fn with_context<T, F>(f: F) -> Result<T, String>
    where
        F: FnOnce(&mut VulkanContext) -> Result<T, String>,
    {
        match VulkanEngine::supported() {
            false => Err("Vulkan API not supported".to_string()),
            true => VK_CONTEXT.with_borrow_mut(|local_ctx| {
                // lazily initialize this thread's context, keeping why it
                // could not be built: `make_surface` answers one of those
                // reasons with a CPU surface rather than an error
                let ctx = match local_ctx.take() {
                    Some(ctx) => ctx,
                    None => VulkanContext::new()?,
                };
                let ctx = local_ctx.insert(ctx);

                // Held for as long as the caller is inside the context,
                // because that is how long Skia may be submitting on the
                // queue behind it. With a queue each there is nobody to
                // contend with and this costs an uncontended lock; only
                // threads past the queue count share one, and then this is
                // what keeps their submissions off each other.
                let shared = VulkanShared::get()
                    .ok_or("Vulkan initialization failed".to_string())?;
                let _submitting = shared.queue_locks[ctx.queue_index].lock();
                f(ctx)
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

#[allow(dead_code)]
pub struct VulkanContext {
    /// Dropped by hand in [`VulkanContext::drop`], under the queue lock.
    ///
    /// Skia's context destructor waits on and submits to the queue the
    /// context was built with, so it is one more caller that has to be
    /// serialised against renders on that queue -- and a field drops after
    /// `Drop::drop` returns, which is to say after the lock has gone.
    context: ManuallyDrop<DirectContext>,
    library: Arc<VulkanLibrary>,
    instance: Arc<Instance>,
    physical_device: Arc<PhysicalDevice>,
    device: Arc<Device>,
    queue: Arc<Queue>,
    queue_index: usize,
    /// Whether [`VulkanShared::release_queue`] is this context's to call.
    queue_claimed: bool,
    vk_sample_counts: vulkano::image::SampleCounts,
    last_use: Instant,
    /// This context is active on the thread that built it. See
    /// [`ThreadBound`].
    _thread: ThreadBound,
}

impl Drop for VulkanContext {
    fn drop(&mut self) {
        let shared = VulkanShared::get();
        let _submitting =
            shared.map(|shared| shared.queue_locks[self.queue_index].lock());

        // SAFETY: the field is never dropped anywhere else, and nothing
        // reads it after this, so this runs exactly once.
        unsafe { ManuallyDrop::drop(&mut self.context) };

        if let (Some(shared), true) = (shared, self.queue_claimed) {
            shared.release_queue(self.queue_index);
        }
    }
}

impl VulkanContext {
    /// Builds this thread's context on the process's shared device.
    ///
    /// Everything below the context -- library, instance, physical device,
    /// device, queue -- is cloned from [`VulkanShared`], so dropping this
    /// context at thread exit drops `Arc` clones and not the objects
    /// themselves. Only the Skia `DirectContext` is per-thread, which is what
    /// Skia asks for: a context belongs to the thread it is active on.
    fn new() -> Result<Self, String> {
        let shared =
            VulkanShared::get().ok_or("Vulkan initialization failed")?;
        let library = Arc::clone(&shared.library);
        let instance = Arc::clone(&shared.instance);
        let physical_device = Arc::clone(&shared.physical_device);
        let device = Arc::clone(&shared.device);
        let (queue_index, queue_claimed) = shared.claim_queue();
        let queue = Arc::clone(&shared.queues[queue_index]);

        let context = {
            let get_proc = |of| unsafe {
                match of {
                    GetProcOf::Instance(instance, name) => {
                        let vk_instance =
                            ash::vk::Instance::from_raw(instance as _);
                        library.get_instance_proc_addr(vk_instance, name)
                    }
                    GetProcOf::Device(device, name) => {
                        let get_device_proc_addr =
                            instance.fns().v1_0.get_device_proc_addr;
                        let vk_device = ash::vk::Device::from_raw(device as _);
                        let resolved = get_device_proc_addr(vk_device, name);

                        // Hand Skia our own entry point for the two calls
                        // that write a queue, remembering the driver's to
                        // forward to. Everything else passes through.
                        let asked_for =
                            std::ffi::CStr::from_ptr(name).to_bytes();
                        match (asked_for, resolved) {
                            (b"vkQueueSubmit", Some(real)) => {
                                REAL_QUEUE_SUBMIT.get_or_init(|| {
                                    std::mem::transmute::<
                                        _,
                                        ash::vk::PFN_vkQueueSubmit,
                                    >(real)
                                });
                                Some(std::mem::transmute::<
                                    ash::vk::PFN_vkQueueSubmit,
                                    unsafe extern "system" fn(),
                                >(
                                    locked_queue_submit
                                ))
                            }
                            (b"vkQueueWaitIdle", Some(real)) => {
                                REAL_QUEUE_WAIT_IDLE.get_or_init(|| {
                                    std::mem::transmute::<
                                        _,
                                        ash::vk::PFN_vkQueueWaitIdle,
                                    >(real)
                                });
                                Some(std::mem::transmute::<
                                    ash::vk::PFN_vkQueueWaitIdle,
                                    unsafe extern "system" fn(),
                                >(
                                    locked_queue_wait_idle
                                ))
                            }
                            _ => resolved,
                        }
                    }
                }
                .map(|f| f as _)
                .unwrap_or_else(|| {
                    println!(
                        "Failed to resolve Vulkan proc `{}`",
                        of.name().to_string_lossy()
                    );
                    ptr::null()
                })
            };
            let backend_context = unsafe {
                BackendContext::new_builder(
                    instance.handle().as_raw() as _,
                    physical_device.handle().as_raw() as _,
                    device.handle().as_raw() as _,
                    (
                        queue.handle().as_raw() as _,
                        queue.queue_family_index() as usize,
                    ),
                    &get_proc,
                    None,
                )
                .build()
            };
            direct_contexts::make_vulkan(&backend_context, None)
        }
        .ok_or_else(|| {
            // the queue is spoken for from the moment it is claimed, so a
            // context that never gets built has to hand it back by hand
            shared.release_queue(queue_index);
            "Failed to create Vulkan backend context"
        })?;

        let vk_sample_counts =
            physical_device.properties().framebuffer_color_sample_counts;

        Ok(Self {
            context: ManuallyDrop::new(context),
            library,
            instance,
            physical_device,
            device,
            queue,
            queue_index,
            queue_claimed,
            vk_sample_counts,
            last_use: Instant::now() + VK_CONTEXT_LIFESPAN,
            _thread: ThreadBound::new(),
        })
    }

    /// Computes valid MSAA sample counts for a given color type.
    fn msaa_for_color_type(&self, color_type: ColorType) -> Vec<usize> {
        let max_sample_count = self
            .context
            .max_surface_sample_count_for_color_type(color_type);
        let mut msaa: Vec<usize> = [1, 2, 4, 8, 16, 32]
            .into_iter()
            .filter(|s| s <= &max_sample_count)
            .filter_map(|s| {
                vulkano::image::SampleCount::try_from(s as u32).ok()
            })
            .filter(|s| self.vk_sample_counts.contains_enum(*s))
            .map(|s| s as usize)
            .collect();
        msaa.insert(0, 0); // also include the shader-based AA option
        msaa
    }

    pub fn works(&mut self) -> bool {
        self.surface(
            &ImageInfo::new_n32_premul(
                ISize::new(100, 100),
                Some(ColorSpace::new_srgb()),
            ),
            &ExportOptions::default(),
        )
        .is_ok()
    }

    pub fn surface(
        &mut self,
        image_info: &ImageInfo,
        opts: &ExportOptions,
    ) -> Result<Surface, String> {
        self.last_use = Instant::now();
        let msaa = self.msaa_for_color_type(image_info.color_type());
        surfaces::render_target(
            &mut self.context,
            Budgeted::Yes,
            image_info,
            Some(opts.msaa_from(&msaa)?),
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

    /// Hands back what this context is holding that it is not using.
    ///
    /// Under the queue lock because freeing Skia's resources waits out the
    /// work still on the queue, and the idle watcher calls this from a rayon
    /// worker that may be sharing its queue with a thread mid-render.
    fn cleanup(&mut self) {
        let _submitting = VulkanShared::get()
            .map(|shared| shared.queue_locks[self.queue_index].lock());

        self.context.free_gpu_resources();
        self.context
            .perform_deferred_cleanup(Duration::from_secs(1), None);
    }
}
