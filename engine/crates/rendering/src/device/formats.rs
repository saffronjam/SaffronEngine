//! Surface format selection, capture support, and the selected-device log lines.

use super::*;

/// The desired swapchain / offscreen surface format: `B8G8R8A8_UNORM` with the
/// sRGB-nonlinear color space. Used directly by the
/// offscreen host (which never presents) and preferred by the windowed host.
pub(super) const PREFERRED_SURFACE_FORMAT: vk::SurfaceFormatKHR = vk::SurfaceFormatKHR {
    format: vk::Format::B8G8R8A8_UNORM,
    color_space: vk::ColorSpaceKHR::SRGB_NONLINEAR,
};

/// Picks the swapchain surface format: prefer [`PREFERRED_SURFACE_FORMAT`], else the
/// first advertised format. Windowed host only — the offscreen host uses the
/// preferred format directly since it has no surface to query.
pub(super) fn choose_surface_format(
    surface_loader: &surface::Instance,
    physical_device: vk::PhysicalDevice,
    surface: vk::SurfaceKHR,
) -> Result<vk::SurfaceFormatKHR> {
    // SAFETY: the ash seam. Surface-format query on the chosen device/surface.
    let formats =
        unsafe { surface_loader.get_physical_device_surface_formats(physical_device, surface) }
            .map_err(|result| Error::Vk {
                context: "get_physical_device_surface_formats",
                result,
            })?;
    if formats.is_empty() {
        return Err(Error::NoDevice("surface advertises no formats".into()));
    }
    let preferred = formats.iter().copied().find(|f| {
        f.format == PREFERRED_SURFACE_FORMAT.format
            && f.color_space == PREFERRED_SURFACE_FORMAT.color_space
    });
    Ok(preferred.unwrap_or(formats[0]))
}

/// Reports whether the surface allows `TRANSFER_SRC` swapchain images (the
/// window-screenshot path; an exotic surface that disallows it gives up capture,
/// not the whole swapchain).
pub(super) fn surface_capture_supported(
    surface_loader: &surface::Instance,
    physical_device: vk::PhysicalDevice,
    surface: vk::SurfaceKHR,
) -> bool {
    // SAFETY: the ash seam. Surface-capabilities query.
    let caps = unsafe {
        surface_loader.get_physical_device_surface_capabilities(physical_device, surface)
    };
    caps.map(|c| {
        c.supported_usage_flags
            .contains(vk::ImageUsageFlags::TRANSFER_SRC)
    })
    .unwrap_or(false)
}

/// Logs the chosen GPU's name and type once selection is final ("vulkan ready — gpu
/// '…'"). This is the line the device-selection gate greps to confirm the discrete GPU
/// was preferred over the software rasterizer.
pub(super) fn log_selected_device(instance: &ash::Instance, physical_device: vk::PhysicalDevice) {
    // SAFETY: the ash seam. Read-only property query on the chosen device.
    let props = unsafe { instance.get_physical_device_properties(physical_device) };
    let name = props
        .device_name_as_c_str()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let kind = match props.device_type {
        vk::PhysicalDeviceType::DISCRETE_GPU => "discrete",
        vk::PhysicalDeviceType::INTEGRATED_GPU => "integrated",
        vk::PhysicalDeviceType::VIRTUAL_GPU => "virtual",
        vk::PhysicalDeviceType::CPU => "cpu",
        _ => "other",
    };
    tracing::info!("vulkan ready — gpu '{name}' ({kind})");
}

/// Logs the resolved software-GPU and RT capability once.
pub(super) fn log_software_gpu(capabilities: &Capabilities) {
    if capabilities.software_gpu {
        tracing::info!("software rasterizer detected — GPU timings reflect CPU rasterization time");
    }
    if capabilities.rt_supported {
        tracing::info!("ray tracing available (KHR acceleration_structure + ray_query)");
    } else {
        tracing::info!("ray tracing unavailable — RT passes disabled");
    }
    if capabilities.cluster_acceleration_structure {
        tracing::info!("cluster acceleration structures available");
    }
    if capabilities.partitioned_acceleration_structure {
        tracing::info!(
            "partitioned top-level structure available (up to {} partitions)",
            capabilities.max_partition_count
        );
    }
}

/// The attachment usage a `format` is created with when probing its MSAA support: a
/// depth format as a depth-stencil attachment, anything else as a color attachment.
pub(super) fn attachment_usage(format: vk::Format) -> vk::ImageUsageFlags {
    match format {
        vk::Format::D32_SFLOAT
        | vk::Format::D24_UNORM_S8_UINT
        | vk::Format::D32_SFLOAT_S8_UINT
        | vk::Format::D16_UNORM => vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT,
        _ => vk::ImageUsageFlags::COLOR_ATTACHMENT,
    }
}
