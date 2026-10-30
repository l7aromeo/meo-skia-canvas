use super::skia_max_api_version;
use crate::{
    context::{layer::Target, page::Page},
    gpu::{RenderCache, RenderState::Resizing},
};
use ash::vk::Handle;
use skia_safe::{
    Color, ColorType, Image, Matrix, Paint, SurfaceProps,
    canvas::SrcRectConstraint,
    gpu::{self, backend_render_targets, direct_contexts, surfaces, vk},
};
use std::{ptr, sync::Arc};
use vulkano::{
    Validated, VulkanError, VulkanLibrary, VulkanObject,
    device::{
        Device, DeviceCreateInfo, DeviceExtensions, DeviceOwned, Queue,
        QueueCreateInfo, QueueFlags, physical::PhysicalDeviceType,
    },
    format::Format as VkFormat,
    image::{ImageUsage, view::ImageView},
    instance::{Instance, InstanceCreateFlags, InstanceCreateInfo},
    render_pass::{Framebuffer, FramebufferCreateInfo, RenderPass},
    swapchain::{
        CompositeAlpha, Surface, Swapchain, SwapchainAcquireFuture,
        SwapchainCreateInfo, SwapchainPresentInfo, acquire_next_image,
    },
    sync::{self, GpuFuture},
};
use winit::{dpi::PhysicalSize, event_loop::ActiveEventLoop, window::Window};

pub struct VulkanRenderer {
    window: Arc<Window>,
    cache: RenderCache, /* must be listed before backend to ensure proper
                         * drop order */
    backend: VulkanBackend,
}

impl VulkanRenderer {
    pub fn for_window(
        event_loop: &ActiveEventLoop,
        window: Arc<Window>,
    ) -> Self {
        let instance = {
            // SAFETY: Vulkan must be available since status check passed.
            let library = VulkanLibrary::new()
                .expect("Vulkan libraries not found on system");
            let required_extensions = Surface::required_extensions(event_loop)
                // SAFETY: Vulkan must be available since status check passed.
                .expect("Failed to get required Vulkan extensions");

            Instance::new(
                library,
                InstanceCreateInfo {
                    flags: InstanceCreateFlags::ENUMERATE_PORTABILITY, // support MoltenVK
                    enabled_extensions: required_extensions,
                    ..Default::default()
                },
            )
            .unwrap_or_else(|_| {
                panic!(
                    "Vulkan: could not create instance supporting: {:?}",
                    required_extensions
                )
            })
        };

        let device_extensions = DeviceExtensions {
            khr_swapchain: true, /* we need a swapchain to manage repainting
                                  * the window */
            ..DeviceExtensions::empty()
        };

        let surface = Surface::from_window(instance.clone(), window.clone())
            // SAFETY: Surface creation only fails on driver bugs.
            .expect("Vulkan: failed to create window surface");

        // Collect the list of available devices & queues then select ‘best’ one
        // for our needs
        let (physical_device, queue_family_index) = instance
            .enumerate_physical_devices()
            // SAFETY: Vulkan must be available since status check passed.
            .expect("Vulkan: failed to enumerate physical devices")
            .filter(|p| {
                // omit devices that don't support our swapchain requirement
                p.supported_extensions().contains(&device_extensions)
            })
            .filter_map(|p| {
                // for each device, find a graphics queue family that can handle
                // our surface type and filter out any devices
                // that don't have one
                p.queue_family_properties()
                    .iter()
                    .enumerate()
                    .position(|(i, q)| {
                        q.queue_flags.intersects(QueueFlags::GRAPHICS)
                            && p.surface_support(i as u32, &surface)
                                .unwrap_or(false)
                        //  && p.presentation_support(_i as u32,
                        // event_loop).unwrap() // unreleased
                    })
                    .map(|i| (p, i as u32))
            })
            .min_by_key(|(p, _)| {
                // Sort the list of acceptible devices/queues to try to find the
                // fastest
                match p.properties().device_type {
                    PhysicalDeviceType::DiscreteGpu => 0,
                    PhysicalDeviceType::IntegratedGpu => 1,
                    PhysicalDeviceType::VirtualGpu => 2,
                    PhysicalDeviceType::Cpu => 3,
                    PhysicalDeviceType::Other => 4,
                    _ => 5,
                }
            })
            // SAFETY: Device creation failure indicates incompatible hardware.
            .expect("Vulkan: no suitable physical device found");

        // Use the physical device we selected to initialize a device with a
        // single queue
        let (device, mut queues) = Device::new(
            physical_device.clone(),
            DeviceCreateInfo {
                enabled_extensions: device_extensions,
                queue_create_infos: vec![QueueCreateInfo {
                    queue_family_index,
                    ..Default::default()
                }],
                ..Default::default()
            },
        )
        // SAFETY: Device creation failure indicates incompatible hardware.
        .expect("Vulkan: device initialization failed");

        // SAFETY: Device creation failure indicates incompatible hardware.
        let queue = queues.next().expect("Vulkan: device has no queues");

        // Create a swapchain to manage frame buffers and vsync
        let (swapchain, _images) = {
            // inspect the window to determine the type of framebuffer needed
            let surface =
                Surface::from_window(instance.clone(), window.clone())
                    // SAFETY: Surface/framebuffer creation only fails on driver
                    // bugs.
                    .expect("Vulkan: failed to create swapchain surface");
            let surface_capabilities = physical_device
                .surface_capabilities(&surface, Default::default())
                // SAFETY: Swapchain setup failures indicate GPU driver issues.
                .expect("Vulkan: failed to query surface capabilities");

            // choose the first device format that is on the supported list
            let device_formats = physical_device
                .surface_formats(&surface, Default::default())
                // SAFETY: Swapchain setup failures indicate GPU driver issues.
                .expect("Vulkan: failed to query surface formats");
            let (image_format, _) = device_formats.clone()
                .into_iter()
                .find(|(fmt, _)| VK_FORMATS.contains(fmt))
                .unwrap_or_else(||
                    panic!(
                        "Vulkan: no format supported by Skia was found on device.\nSupported formats: {:?}\nDevice formats: {:?}",
                        VK_FORMATS,
                        device_formats
                    )
                );

            Swapchain::new(
                device.clone(),
                surface,
                SwapchainCreateInfo {
                    image_format,
                    image_extent: window.inner_size().into(),
                    image_usage: ImageUsage::COLOR_ATTACHMENT,
                    min_image_count: surface_capabilities
                        .min_image_count
                        .max(2),
                    composite_alpha: surface_capabilities
                        .supported_composite_alpha
                        .into_iter()
                        .min_by_key(|mode| {
                            // prefer transparency (TODO: this should be
                            // dependent on window background…)
                            match mode {
                                CompositeAlpha::PostMultiplied => 1,
                                CompositeAlpha::PreMultiplied => 2,
                                CompositeAlpha::Opaque => 3,
                                _ => 3,
                            }
                        })
                        // SAFETY: Swapchain setup failures indicate GPU driver
                        // issues.
                        .expect("Vulkan: no composite alpha mode available"),
                    ..Default::default()
                },
            )
            // SAFETY: Swapchain setup failures indicate GPU driver issues.
            .expect("Vulkan: failed to create swapchain")
        };

        Self {
            window,
            backend: VulkanBackend::new(queue, swapchain),
            cache: RenderCache::default(),
        }
    }

    pub fn resize(&mut self, size: PhysicalSize<u32>) {
        self.cache.state = Resizing;
        self.backend.swapchain_is_valid = false;
        self.backend.prepare_swapchain(size);
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

        if let Some(frame) =
            self.backend.render_frame(&self.window, &props, |canvas| {
                // draw background (either use raster cache or set to window’s
                // background color)
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
                for layer in page.layers.iter().skip(self.cache.depth()) {
                    canvas.save();
                    canvas.concat(&matrix);
                    layer.playback(canvas, Target::Pixels);
                    canvas.restore();
                }
            })
        {
            self.cache.update(frame, page, matte, dpr, clip);
        }
    }
}

struct VulkanBackend {
    framebuffers: Vec<Arc<Framebuffer>>,
    render_pass: Arc<RenderPass>,
    swapchain: Arc<Swapchain>,
    swapchain_is_valid: bool,
    last_render: Option<Box<dyn GpuFuture>>,
    skia_ctx: gpu::DirectContext, /* must be listed before parent queue to
                                   * ensure proper drop order */
    queue: Arc<Queue>,
}

impl Drop for VulkanBackend {
    fn drop(&mut self) {
        self.skia_ctx.release_resources_and_abandon();
    }
}

impl VulkanBackend {
    fn new(queue: Arc<Queue>, swapchain: Arc<Swapchain>) -> Self {
        let device = queue.device();
        let instance = device.instance();
        let library = instance.library();

        // Define the layout of the framebuffers and their role in the graphics
        // pipeline
        let render_pass = vulkano::single_pass_renderpass!(
            device.clone(),
            attachments: {
                canvas_img: {
                    format: swapchain.image_format(),
                    samples: 1, // no need for MSAA since we're rendering 1:1
                    load_op: DontCare, // don't clear framebuffers ahead of time
                    store_op: DontCare, // we don't need the bitmap back after display
                },
            },
            pass: {
                // the only attachment will be the bitmap rendered by skia
                color: [canvas_img],
                depth_stencil: {},
            },
        )
        // SAFETY: Surface/framebuffer creation only fails on driver bugs.
        .expect("Vulkan: failed to create render pass");

        // Start with no framebuffers and flag that they need to be allocated
        // before rendering
        let framebuffers = vec![];
        let swapchain_is_valid = false;

        // Hold onto the previous GpuFuture so we can wait on its completion
        // before the next frame
        let last_render = Some(sync::now(device.clone()).boxed());

        // Create a DirectContext that will let us use a surface & canvas to
        // draw into framebuffers
        // SAFETY: as in the engine's own context -- Skia hands back the
        // instance or device handle it was given, so `from_raw` sees a live
        // handle, and `instance`, `device` and `library` outlive the context
        // built here.
        let skia_ctx = unsafe {
            let get_proc = |gpo| {
                let get_device_proc_addr =
                    instance.fns().v1_0.get_device_proc_addr;

                match gpo {
                    vk::GetProcOf::Instance(instance, name) => {
                        let vk_instance =
                            ash::vk::Instance::from_raw(instance as _);
                        library.get_instance_proc_addr(vk_instance, name)
                    }
                    vk::GetProcOf::Device(device, name) => {
                        let vk_device = ash::vk::Device::from_raw(device as _);
                        get_device_proc_addr(vk_device, name)
                    }
                }
                // A null answer is how Vulkan says an entry point is not
                // there, and Skia asks for optional ones expecting it.
                .map_or(ptr::null(), |f| f as _)
            };

            direct_contexts::make_vulkan(
                &vk::BackendContext::new_builder(
                    instance.handle().as_raw() as _,
                    device.physical_device().handle().as_raw() as _,
                    device.handle().as_raw() as _,
                    (
                        queue.handle().as_raw() as _,
                        queue.queue_family_index() as usize,
                    ),
                    &get_proc,
                    skia_max_api_version(instance),
                )
                .build(),
                None,
            )
            // SAFETY: Vulkan must be available since status check passed.
            .expect("Vulkan: Failed to create Skia direct context")
        };

        Self {
            queue,
            framebuffers,
            render_pass,
            swapchain,
            swapchain_is_valid,
            last_render,
            skia_ctx,
        }
    }

    fn prepare_swapchain(&mut self, size: PhysicalSize<u32>) {
        // Only regenerate the swapchain/framebuffers if we've flagged that it's
        // necessary
        if size.width > 0 && size.height > 0 && !self.swapchain_is_valid {
            let (new_swapchain, new_images) = self
                .swapchain
                .recreate(SwapchainCreateInfo {
                    image_extent: size.into(),
                    ..self.swapchain.create_info()
                })
                // SAFETY: Swapchain setup failures indicate GPU driver issues.
                .expect("failed to recreate swapchain");

            self.swapchain = new_swapchain;
            self.framebuffers = new_images
                .iter()
                .map(|image| {
                    Framebuffer::new(
                        self.render_pass.clone(),
                        FramebufferCreateInfo {
                            attachments: vec![
                                ImageView::new_default(image.clone())
                                    // SAFETY: Surface/framebuffer creation only
                                    // fails on driver bugs.
                                    .expect(
                                        "Vulkan: failed to create image view",
                                    ),
                            ],
                            ..Default::default()
                        },
                    )
                    // SAFETY: Surface/framebuffer creation only fails on driver
                    // bugs.
                    .expect("Vulkan: failed to create framebuffer")
                })
                .collect();
            self.swapchain_is_valid = true;
        }
    }

    fn render_frame<F>(
        &mut self,
        window: &Window,
        props: &SurfaceProps,
        f: F,
    ) -> Option<Image>
    where
        F: FnOnce(&skia_safe::Canvas),
    {
        // make sure the framebuffers match the current window size
        self.prepare_swapchain(self.swapchain.image_extent().into());

        self.get_next_frame()
            .and_then(|(image_index, acquire_future)| {
                // pull the appropriate framebuffer and create a skia Surface
                // that renders to it
                let framebuffer =
                    self.framebuffers[image_index as usize].clone();
                let mut surface =
                    self.surface_for_framebuffer(framebuffer.clone(), props);

                // pass the suface's canvas to the user-provided callback
                f(surface.canvas());

                // save a copy of the bitmap for the cache
                let image = surface.image_snapshot();

                // display the result, and hand back the frame only if its draws
                // reached the GPU
                self.flush_framebuffer(window, image_index, acquire_future)
                    .then_some(image)
            })
    }

    fn get_next_frame(&mut self) -> Option<(u32, SwapchainAcquireFuture)> {
        // Request the next framebuffer and a GpuFuture for the render pass
        let (image_index, suboptimal, acquire_future) =
            match acquire_next_image(self.swapchain.clone(), None)
                .map_err(Validated::unwrap)
            {
                Ok(r) => r,
                Err(VulkanError::OutOfDate) => {
                    self.swapchain_is_valid = false;
                    return None;
                }
                Err(e) => panic!("failed to acquire next image: {e}"),
            };

        match suboptimal {
            // If the request was successful but suboptimal, schedule a
            // swapchain recreation
            true => {
                self.swapchain_is_valid = false;
                None
            }
            // otherwise proceed with this frame
            false => Some((image_index, acquire_future)),
        }
    }

    fn surface_for_framebuffer(
        &mut self,
        framebuffer: Arc<Framebuffer>,
        props: &SurfaceProps,
    ) -> skia_safe::Surface {
        let [width, height] = framebuffer.extent();
        let image_access = &framebuffer.attachments()[0];
        let image_object = image_access.image().handle().as_raw();

        let format = image_access.format();
        let (vk_format, color_type) =
            to_sk_format(&format).unwrap_or_else(|| {
                panic!("Vulkan: unsupported color format {:?}", format)
            });

        // SAFETY: `image_object` is the handle of a swapchain image held by
        // `image_access` for this scope, and the format and layout are the
        // ones just read from it rather than assumed, so the description
        // matches the image it names.
        let image_info = &unsafe {
            vk::ImageInfo::new(
                image_object as _,
                vk::Alloc::default(),
                vk::ImageTiling::OPTIMAL,
                vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                vk_format,
                1,
                None,
                None,
                None,
                None,
            )
        };

        let render_target = &backend_render_targets::make_vk(
            (
                width
                    .try_into()
                    // SAFETY: Window dimensions should fit in i32.
                    .expect("Vulkan: framebuffer width overflow"),
                height
                    .try_into()
                    // SAFETY: Window dimensions should fit in i32.
                    .expect("Vulkan: framebuffer height overflow"),
            ),
            image_info,
        );

        surfaces::wrap_backend_render_target(
            &mut self.skia_ctx,
            render_target,
            gpu::SurfaceOrigin::TopLeft,
            color_type,
            None,
            Some(props),
        )
        // SAFETY: Surface/framebuffer creation only fails on driver bugs.
        .expect("Vulkan: failed to create render target surface")
    }

    /// Flushes the canvas's contents to the framebuffer and presents it.
    /// Returns whether the flush succeeded: the framebuffer is presented
    /// either way, so the swapchain moves on, but a failed flush left the
    /// frame's draws behind.
    fn flush_framebuffer(
        &mut self,
        window: &Window,
        image_index: u32,
        acquire_future: SwapchainAcquireFuture,
    ) -> bool {
        // flush the canvas's contents to the framebuffer
        let flushed = self.skia_ctx.flush_and_submit().is_ok();
        self.skia_ctx.free_gpu_resources();

        // reclaim leftover resources from the last frame
        if let Some(ref mut last) = self.last_render {
            last.cleanup_finished();
        }

        // let winit know that rendering is complete
        window.pre_present_notify();

        // send the framebuffer to the gpu and display it on screen
        let Some(last_render) = self.last_render.take() else {
            return flushed;
        };
        let future = last_render
            .join(acquire_future)
            .then_swapchain_present(
                self.queue.clone(),
                SwapchainPresentInfo::swapchain_image_index(
                    self.swapchain.clone(),
                    image_index,
                ),
            )
            .then_signal_fence_and_flush();

        match future.map_err(Validated::unwrap) {
            Ok(future) => {
                self.last_render = Some(future.boxed());
            }
            Err(VulkanError::OutOfDate) => {
                let device = self.queue.device();
                self.last_render = Some(sync::now(device.clone()).boxed());
                self.swapchain_is_valid = false;
            }
            Err(e) => {
                panic!("Vulkan: swapchain flush failed: {e}");
            }
        };
        flushed
    }
}

static VK_FORMATS: &[VkFormat] = &[
    VkFormat::R8G8B8A8_UNORM,
    VkFormat::R8G8B8A8_SRGB,
    VkFormat::R8_UNORM,
    VkFormat::B8G8R8A8_UNORM,
    VkFormat::R5G6B5_UNORM_PACK16,
    VkFormat::B5G6R5_UNORM_PACK16,
    VkFormat::R16G16B16A16_SFLOAT,
    VkFormat::R16_SFLOAT,
    VkFormat::R8G8B8_UNORM,
    VkFormat::R8G8_UNORM,
    VkFormat::A2B10G10R10_UNORM_PACK32,
    VkFormat::A2R10G10B10_UNORM_PACK32,
    VkFormat::R10X6G10X6B10X6A10X6_UNORM_4PACK16,
    VkFormat::B4G4R4A4_UNORM_PACK16,
    VkFormat::R4G4B4A4_UNORM_PACK16,
    VkFormat::R16_UNORM,
    VkFormat::R16G16_UNORM,
    VkFormat::G8_B8_R8_3PLANE_420_UNORM,
    VkFormat::G8_B8R8_2PLANE_420_UNORM,
    VkFormat::G10X6_B10X6R10X6_2PLANE_420_UNORM_3PACK16,
    VkFormat::R16G16B16A16_UNORM,
    VkFormat::R16G16_SFLOAT,
];

fn to_sk_format(vulkano_format: &VkFormat) -> Option<(vk::Format, ColorType)> {
    // Format / ColorType pairs
    // https://github.com/google/skia/blob/4f24819404272433687a76e407bcd7877384f512/src/gpu/ganesh/vk/GrVkCaps.cpp#L880
    //
    // GrColorType -> SkColorType mappings
    // https://github.com/google/skia/blob/4f24819404272433687a76e407bcd7877384f512/include/private/gpu/ganesh/GrTypesPriv.h#L590
    //
    // Present in the GrVkCaps 'supported' list but lacking supported
    // GrColorTypes so omitted:
    // - VkFormat::ETC2_R8G8B8_UNORM_BLOCK
    // - VkFormat::BC1_RGB_UNORM_BLOCK
    // - VkFormat::BC1_RGBA_UNORM_BLOCK
    match vulkano_format {
        VkFormat::R8G8B8A8_UNORM => {
            Some((vk::Format::R8G8B8A8_UNORM, ColorType::RGBA8888))
        }
        VkFormat::R8G8B8A8_SRGB => {
            Some((vk::Format::R8G8B8A8_SRGB, ColorType::SRGBA8888))
        }
        VkFormat::R8_UNORM => Some((vk::Format::R8_UNORM, ColorType::R8UNorm)),
        VkFormat::B8G8R8A8_UNORM => {
            Some((vk::Format::B8G8R8A8_UNORM, ColorType::BGRA8888))
        }
        VkFormat::R5G6B5_UNORM_PACK16 => {
            Some((vk::Format::R5G6B5_UNORM_PACK16, ColorType::RGB565))
        }
        VkFormat::B5G6R5_UNORM_PACK16 => {
            Some((vk::Format::B5G6R5_UNORM_PACK16, ColorType::RGB565))
        }
        VkFormat::R16G16B16A16_SFLOAT => {
            Some((vk::Format::R16G16B16A16_SFLOAT, ColorType::RGBAF16))
        }
        VkFormat::R16_SFLOAT => {
            Some((vk::Format::R16_SFLOAT, ColorType::A16Float))
        }
        VkFormat::R8G8B8_UNORM => {
            Some((vk::Format::R8G8B8_UNORM, ColorType::RGB888x))
        }
        VkFormat::R8G8_UNORM => {
            Some((vk::Format::R8G8_UNORM, ColorType::R8G8UNorm))
        }
        VkFormat::A2B10G10R10_UNORM_PACK32 => {
            Some((vk::Format::A2B10G10R10_UNORM_PACK32, ColorType::RGBA1010102))
        }
        VkFormat::A2R10G10B10_UNORM_PACK32 => {
            Some((vk::Format::A2R10G10B10_UNORM_PACK32, ColorType::BGRA1010102))
        }
        VkFormat::R10X6G10X6B10X6A10X6_UNORM_4PACK16 => Some((
            vk::Format::R10X6G10X6B10X6A10X6_UNORM_4PACK16,
            ColorType::RGBA10x6,
        )),
        VkFormat::B4G4R4A4_UNORM_PACK16 => {
            Some((vk::Format::B4G4R4A4_UNORM_PACK16, ColorType::ARGB4444))
        }
        VkFormat::R4G4B4A4_UNORM_PACK16 => {
            Some((vk::Format::R4G4B4A4_UNORM_PACK16, ColorType::ARGB4444))
        }
        VkFormat::R16_UNORM => {
            Some((vk::Format::R16_UNORM, ColorType::A16UNorm))
        }
        VkFormat::R16G16_UNORM => {
            Some((vk::Format::R16G16_UNORM, ColorType::R16G16UNorm))
        }
        VkFormat::G8_B8_R8_3PLANE_420_UNORM => {
            Some((vk::Format::G8_B8_R8_3PLANE_420_UNORM, ColorType::RGB888x))
        }
        VkFormat::G8_B8R8_2PLANE_420_UNORM => {
            Some((vk::Format::G8_B8R8_2PLANE_420_UNORM, ColorType::RGB888x))
        }
        VkFormat::G10X6_B10X6R10X6_2PLANE_420_UNORM_3PACK16 => Some((
            vk::Format::G10X6_B10X6R10X6_2PLANE_420_UNORM_3PACK16,
            ColorType::RGBA1010102,
        )),
        VkFormat::R16G16B16A16_UNORM => {
            Some((vk::Format::R16G16B16A16_UNORM, ColorType::R16G16B16A16UNorm))
        }
        VkFormat::R16G16_SFLOAT => {
            Some((vk::Format::R16G16_SFLOAT, ColorType::R16G16Float))
        }
        _ => None,
    }
}
