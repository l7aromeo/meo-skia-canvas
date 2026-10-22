use skia_safe::{ColorType, gpu::vk};
use vulkano::{format::Format as VkFormat, instance::Instance};

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
