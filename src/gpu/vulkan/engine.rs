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
    ptr,
    sync::{Arc, OnceLock},
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

use crate::context::page::ExportOptions;

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
/// A separate hang remains under that same thread pressure -- worker threads
/// blocked on one of the driver's own locks, which this does not touch and is
/// not claimed to fix.
///
/// The `OnceLock` also serialises the first load, which is where two threads
/// racing to dlopen would otherwise meet. A failed load is remembered as such:
/// a machine without the loader will not grow one part-way through a run, and
/// retrying the dlopen per context only repeated the work.
static VK_LIBRARY: OnceLock<Option<Arc<VulkanLibrary>>> = OnceLock::new();

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
                    // drop contexts that haven't been used in a while to free
                    // resources
                    VK_CONTEXT.with_borrow_mut(|cell| {
                        cell.take_if(|engine| {
                            engine.cleanup(); // it's unclear how effective this is
                            engine.last_use.elapsed() > VK_CONTEXT_LIFESPAN
                        });
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
                local_ctx
                    // lazily initialize this thread's context...
                    .take()
                    .or_else(|| VulkanContext::new().ok())
                    .ok_or("Vulkan initialization failed".to_string())
                    .and_then(|ctx| f(local_ctx.insert(ctx)))
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
    context: DirectContext,
    library: Arc<VulkanLibrary>,
    instance: Arc<Instance>,
    physical_device: Arc<PhysicalDevice>,
    device: Arc<Device>,
    queue: Arc<Queue>,
    vk_sample_counts: vulkano::image::SampleCounts,
    last_use: Instant,
}

impl VulkanContext {
    fn new() -> Result<Self, String> {
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

        let (device, mut queues) = Device::new(
            physical_device.clone(),
            DeviceCreateInfo {
                queue_create_infos: vec![QueueCreateInfo {
                    queue_family_index,
                    ..Default::default()
                }],
                ..Default::default()
            },
        )
        .or(Err("Failed to create Vulkan device"))?;

        let queue = queues
            .next()
            .ok_or("Failed to create Vulkan graphics queue")?;

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
                        get_device_proc_addr(vk_device, name)
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
        .ok_or("Failed to create Vulkan backend context")?;

        let vk_sample_counts =
            physical_device.properties().framebuffer_color_sample_counts;

        Ok(Self {
            context,
            library,
            instance,
            physical_device,
            device,
            queue,
            vk_sample_counts,
            last_use: Instant::now() + VK_CONTEXT_LIFESPAN,
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

    fn cleanup(&mut self) {
        self.context.free_gpu_resources();
        self.context
            .perform_deferred_cleanup(Duration::from_secs(1), None);
    }
}
