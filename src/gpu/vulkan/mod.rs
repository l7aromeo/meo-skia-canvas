use skia_safe::gpu::vk;
use vulkano::instance::Instance;

pub mod engine;

#[cfg(feature = "window")]
pub mod renderer;

/// The Vulkan API version to give Skia as its maximum: the one `instance` put
/// in `VkApplicationInfo::apiVersion`, which is what Skia's
/// `fMaxAPIVersion` is documented to match.
///
/// Left unset, Skia uses the loader's version from `vkEnumerateInstanceVersion`
/// instead, capped by what the device reports. vulkano creates an instance for
/// its own header version unless told otherwise, so a loader newer than that
/// header makes the two disagree: on a 1.4 loader with a 1.4 device Skia
/// requires the 1.4 core entry points, a device created for an earlier version
/// returns those as null, and Skia refuses to build a context at all.
///
/// The bits come from the conversion vulkano itself uses to fill
/// `VkApplicationInfo`, so Skia is told exactly what Vulkan was told.
fn skia_max_api_version(instance: &Instance) -> Option<vk::Version> {
    u32::try_from(instance.max_api_version())
        .ok()
        .map(vk::Version::from)
}

#[cfg(test)]
mod tests {
    use super::skia_max_api_version;
    use vulkano::{
        VulkanLibrary,
        instance::{Instance, InstanceCreateInfo},
    };

    // Needs a Vulkan loader and a driver to create an instance, but no device:
    // the version is fixed when the instance is created, before any device is
    // chosen. Without a loader it fails rather than passing on nothing.
    #[test]
    fn skia_is_given_the_api_version_the_instance_was_created_for() {
        let library = VulkanLibrary::new().expect("a Vulkan loader");
        let instance = Instance::new(library, InstanceCreateInfo::default())
            .expect("a Vulkan instance");
        let created_for = instance.max_api_version();
        let given = skia_max_api_version(&instance)
            .expect("Skia is given a maximum version rather than none");
        // Read back through Skia's accessors, so the packing is checked
        // against Skia's reading of it and not against the conversion that
        // wrote it.
        assert_eq!(
            (given.major(), given.minor(), given.patch()),
            (
                created_for.major as usize,
                created_for.minor as usize,
                created_for.patch as usize,
            ),
        );
    }
}
