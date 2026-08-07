//! Physical-device selection: the preference rank, the per-device evaluation, and the
//! graphics / async-compute queue-family search.

use super::*;

/// The outcome of physical-device selection: the device, its queue family, the
/// probed optional capabilities, and the device-type preference rank used to pick
/// it among the qualifying candidates.
pub(super) struct DeviceSelection {
    pub(super) physical_device: vk::PhysicalDevice,
    pub(super) graphics_queue_family: u32,
    pub(super) compute_queue: Option<AsyncComputeQueue>,
    pub(super) capabilities: Capabilities,
    pub(super) preference: DevicePreference,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct AsyncComputeQueue {
    pub(super) family: u32,
    pub(super) index: u32,
}

/// The device-type preference order: prefer a discrete GPU and fall back down the
/// type ladder. Higher is better, so `Ord` ranks a discrete GPU above an integrated
/// one above a virtual one above a CPU/software rasterizer.
///
/// This is a *preference*, never a gate: when the only qualifying device is the CPU
/// rasterizer (the CI toolbox with no hardware ICD), it still scores `Cpu` and is
/// selected. With an NVIDIA ICD added alongside Mesa llvmpipe, both qualify and the
/// discrete GPU wins on rank.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(super) enum DevicePreference {
    /// A software/CPU rasterizer (llvmpipe) or `OTHER`/unknown — the last resort.
    Cpu,
    /// A `VIRTUAL_GPU` (a paravirtualized device).
    Virtual,
    /// An `INTEGRATED_GPU` (an on-die GPU).
    Integrated,
    /// A `DISCRETE_GPU` — the strongly preferred type.
    Discrete,
}

impl DevicePreference {
    /// Ranks a `VkPhysicalDeviceType` into the preference order: discrete > integrated
    /// > virtual > cpu/other.
    pub(super) fn from_type(device_type: vk::PhysicalDeviceType) -> Self {
        match device_type {
            vk::PhysicalDeviceType::DISCRETE_GPU => Self::Discrete,
            vk::PhysicalDeviceType::INTEGRATED_GPU => Self::Integrated,
            vk::PhysicalDeviceType::VIRTUAL_GPU => Self::Virtual,
            _ => Self::Cpu,
        }
    }
}

/// Selects the best physical device that has a graphics (and, when `require_present`,
/// present) queue family and the required Vulkan 1.2/1.3 feature set, then probes its
/// optional features.
///
/// The required features (`runtimeDescriptorArray`, `descriptorBindingPartiallyBound`,
/// `bufferDeviceAddress`, `timelineSemaphore`, `dynamicRendering`, `synchronization2`) gate selection;
/// RT / fill-mode-non-solid / memory-budget / pipeline-stats are probed and never
/// gate (the degradation the unit test asserts on llvmpipe).
///
/// Among the qualifying devices it prefers by [`DevicePreference`] (discrete GPU
/// first). With an NVIDIA ICD added next to Mesa's
/// llvmpipe the loader enumerates both; this picks the discrete 3070 Ti rather than
/// whichever the loader listed first. When the only qualifying device is the
/// software rasterizer (the CI toolbox), it is still selected — preference, never
/// exclusion.
///
/// `require_present` gates on a present-capable queue family (the windowed host's
/// swapchain). The offscreen host passes `false` and a `None` surface: it renders
/// offscreen and reads back, never presenting, so it gates on a graphics queue only.
pub(super) fn select_physical_device(
    instance: &ash::Instance,
    surface_loader: Option<&surface::Instance>,
    surface: Option<vk::SurfaceKHR>,
    require_present: bool,
) -> Result<DeviceSelection> {
    // SAFETY: the ash seam. Enumerates physical devices on the live instance.
    let devices = unsafe { instance.enumerate_physical_devices() }.map_err(|result| Error::Vk {
        context: "enumerate_physical_devices",
        result,
    })?;
    if devices.is_empty() {
        return Err(Error::NoDevice("no Vulkan physical devices present".into()));
    }

    // `SAFFRON_VK_VERBOSE` traces each candidate's verdict — the diagnostic that
    // pinned the discrete GPU being silently rejected behind a qualifying llvmpipe.
    let verbose = std::env::var_os("SAFFRON_VK_VERBOSE").is_some();
    let mut best: Option<DeviceSelection> = None;
    let mut last_reason = String::from("no device enumerated");
    for physical_device in devices {
        match evaluate_device(
            instance,
            surface_loader,
            surface,
            physical_device,
            require_present,
        ) {
            // Keep the highest-ranked qualifying device; the first seen wins a tie,
            // so the loader's order is preserved within one device type.
            Ok(selection) => {
                if verbose {
                    tracing::info!("device qualifies ({:?})", selection.preference);
                }
                if best
                    .as_ref()
                    .is_none_or(|current| selection.preference > current.preference)
                {
                    best = Some(selection);
                }
            }
            Err(reason) => {
                if verbose {
                    tracing::info!("device rejected: {reason}");
                }
                last_reason = reason;
            }
        }
    }
    best.ok_or(Error::NoDevice(last_reason))
}

/// Evaluates one physical device: a graphics (and, when `require_present`, present)
/// family plus the required feature bits. Returns the selection or a human reason it
/// was rejected.
pub(super) fn evaluate_device(
    instance: &ash::Instance,
    surface_loader: Option<&surface::Instance>,
    surface: Option<vk::SurfaceKHR>,
    physical_device: vk::PhysicalDevice,
    require_present: bool,
) -> std::result::Result<DeviceSelection, String> {
    // SAFETY: the ash seam. Property/feature queries on the candidate device.
    let props = unsafe { instance.get_physical_device_properties(physical_device) };
    let name = props
        .device_name_as_c_str()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();

    if props.api_version < API_VERSION {
        return Err(format!("{name}: api version below 1.3"));
    }

    let graphics_queue_family = find_graphics_queue_family(
        instance,
        surface_loader,
        surface,
        physical_device,
        require_present,
    )
    .ok_or_else(|| {
        if require_present {
            format!("{name}: no graphics+present queue family")
        } else {
            format!("{name}: no graphics queue family")
        }
    })?;

    let mut features11 = vk::PhysicalDeviceVulkan11Features::default();
    let mut features12 = vk::PhysicalDeviceVulkan12Features::default();
    let mut features13 = vk::PhysicalDeviceVulkan13Features::default();
    let mut features2 = vk::PhysicalDeviceFeatures2::default()
        .push_next(&mut features11)
        .push_next(&mut features12)
        .push_next(&mut features13);
    // SAFETY: the ash seam. Fills the chained feature structs for this device.
    unsafe { instance.get_physical_device_features2(physical_device, &mut features2) };

    if features2.features.shader_int64 == 0 {
        return Err(format!(
            "{name}: missing required shaderInt64 for authoritative spatial numerics"
        ));
    }
    if features11.shader_draw_parameters == 0 {
        return Err(format!("{name}: missing required shaderDrawParameters"));
    }

    if features12.runtime_descriptor_array == 0
        || features12.descriptor_binding_partially_bound == 0
        || features12.descriptor_binding_sampled_image_update_after_bind == 0
        || features12.shader_sampled_image_array_non_uniform_indexing == 0
        || features12.buffer_device_address == 0
    {
        return Err(format!(
            "{name}: missing required descriptor-indexing features"
        ));
    }
    if features12.timeline_semaphore == 0 {
        return Err(format!("{name}: missing required timelineSemaphore"));
    }
    if features13.dynamic_rendering == 0 || features13.synchronization2 == 0 {
        return Err(format!(
            "{name}: missing dynamic rendering / synchronization2"
        ));
    }

    let capabilities = probe_optional_features(instance, physical_device, &props, &name);
    let queue_families =
        unsafe { instance.get_physical_device_queue_family_properties(physical_device) };
    let compute_queue = choose_async_compute_queue(&queue_families, graphics_queue_family);
    Ok(DeviceSelection {
        physical_device,
        graphics_queue_family,
        compute_queue,
        capabilities,
        preference: DevicePreference::from_type(props.device_type),
    })
}

pub(super) fn choose_async_compute_queue(
    families: &[vk::QueueFamilyProperties],
    graphics_queue_family: u32,
) -> Option<AsyncComputeQueue> {
    let distinct_dedicated = families
        .iter()
        .enumerate()
        .filter(|(index, family)| {
            *index != graphics_queue_family as usize
                && family.queue_count != 0
                && family.queue_flags.contains(vk::QueueFlags::COMPUTE)
                && !family.queue_flags.contains(vk::QueueFlags::GRAPHICS)
        })
        .max_by_key(|(_, family)| {
            (
                !family.queue_flags.contains(vk::QueueFlags::TRANSFER),
                family.queue_count,
            )
        })
        .map(|(index, _)| AsyncComputeQueue {
            family: index as u32,
            index: 0,
        });
    if distinct_dedicated.is_some() {
        return distinct_dedicated;
    }

    if let Some(graphics) = families.get(graphics_queue_family as usize)
        && graphics.queue_count >= 2
        && graphics.queue_flags.contains(vk::QueueFlags::COMPUTE)
    {
        return Some(AsyncComputeQueue {
            family: graphics_queue_family,
            index: 1,
        });
    }

    families
        .iter()
        .enumerate()
        .filter(|(index, family)| {
            *index != graphics_queue_family as usize
                && family.queue_count != 0
                && family.queue_flags.contains(vk::QueueFlags::COMPUTE)
        })
        .max_by_key(|(_, family)| family.queue_count)
        .map(|(index, _)| AsyncComputeQueue {
            family: index as u32,
            index: 0,
        })
}

/// Finds a graphics-capable queue family, additionally requiring present support on
/// `surface` when `require_present`.
///
/// The windowed host needs present (it drives a swapchain), so it requires both. The
/// offscreen host renders offscreen and reads back — it never presents — so it asks
/// only for graphics (and passes a `None` surface, since none exists).
pub(super) fn find_graphics_queue_family(
    instance: &ash::Instance,
    surface_loader: Option<&surface::Instance>,
    surface: Option<vk::SurfaceKHR>,
    physical_device: vk::PhysicalDevice,
    require_present: bool,
) -> Option<u32> {
    // SAFETY: the ash seam. Queue-family property query on the candidate device.
    let families = unsafe { instance.get_physical_device_queue_family_properties(physical_device) };
    for (index, family) in families.iter().enumerate() {
        if !family.queue_flags.contains(vk::QueueFlags::GRAPHICS) {
            continue;
        }
        if !require_present {
            return Some(index as u32);
        }
        // The windowed host requires present; it always has a surface + loader.
        let (Some(loader), Some(surface)) = (surface_loader, surface) else {
            continue;
        };
        // SAFETY: the ash seam. Present-support query for this family/surface.
        let supports_present = unsafe {
            loader.get_physical_device_surface_support(physical_device, index as u32, surface)
        }
        .unwrap_or(false);
        if supports_present {
            return Some(index as u32);
        }
    }
    None
}
