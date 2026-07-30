//! The immutable-after-init Vulkan core: instance, surface, physical device, logical device,
//! graphics queue, the VMA allocator, the resolved feature capabilities, and the loaded extension
//! dispatch tables. Constructed once, then borrowed `&Device` everywhere — never `&mut`.

mod features;
mod formats;
mod instance;
mod select;

use ash::ext::calibrated_timestamps;
use ash::ext::debug_utils;
use ash::khr::acceleration_structure as accel;
use ash::khr::{surface, swapchain};
use ash::vk;
use raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use std::ffi::{CStr, c_char, c_void};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::resources::DeviceResources;
use crate::{Error, GpuQueue, Result};

use features::*;
use formats::*;
use instance::*;
use select::*;

/// Counts validation/performance messages at warning-or-error severity seen by the
/// debug callback across the process. The validation-clean smoke reads this before
/// and after a run and asserts it did not move — the in-test expression of the
/// "the log is asserted clean" gate. Loader chatter (filtered in the callback) is
/// never counted.
static VALIDATION_ISSUE_COUNT: AtomicU64 = AtomicU64::new(0);

/// The running count of validation/performance warnings + errors the debug
/// messenger has seen this process. Reading it before and after a render and
/// asserting it did not move is the validation-clean gate the host's e2e harness reads.
pub fn validation_issue_count() -> u64 {
    VALIDATION_ISSUE_COUNT.load(Ordering::Relaxed)
}

/// Where the device draws its output to.
///
/// The surface-bound bring-up (the standalone present-only host) and the no-surface
/// offscreen bring-up (the editor native-viewport host, every headless render-and-
/// read-back, and the validation-clean smoke) are a *parameter*, not a fork — so the
/// host and viewport reuse this code with no second path.
///
/// The two paths split on whether a surface exists at all. [`Self::Window`] enables
/// `VK_KHR_surface` + the platform surface extension and creates a real surface for
/// the present swapchain. [`Self::Offscreen`] enables **no** surface extension and
/// creates **no** surface object: it renders into an offscreen color image, reads it
/// back, and (in the host) publishes BGRA8 frames to shared memory — never
/// presenting. A no-surface instance is what lets the editor host boot under the
/// NVIDIA ICD, whose driver does not implement `VK_EXT_headless_surface` (a Mesa
/// extension); requesting any headless surface there fails `create_instance` with
/// `ERROR_EXTENSION_NOT_PRESENT`.
pub enum SurfaceSource<'a> {
    /// Build a surface from a window's raw display+window handle pair (the
    /// standalone present-only host). The platform surface extension is selected
    /// from the display handle, and a present swapchain is built against it.
    Window(&'a (dyn WindowSurface + 'a)),
    /// No surface at all (the editor native-viewport host, the headless smoke, and
    /// every offscreen render-and-read-back test). Renders to an offscreen color
    /// image, reads it back, never presents — so the instance needs no surface
    /// extension and no surface object exists. Device selection prefers the discrete
    /// GPU and does not require present support.
    Offscreen,
}

/// The window-handle pair `ash-window` consumes to create a surface.
///
/// Implemented by `saffron_window::Window` via its `HasDisplayHandle` +
/// `HasWindowHandle` impls; this trait is the object-safe bundle the device takes
/// without depending on the concrete window type.
pub trait WindowSurface: HasDisplayHandle + HasWindowHandle {}

impl<T: HasDisplayHandle + HasWindowHandle> WindowSurface for T {}

/// Resolved required and optional capabilities, probed once at device creation.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Capabilities {
    /// KHR acceleration-structure + ray-query present and enabled.
    pub rt_supported: bool,
    /// `VK_EXT_opacity_micromap::micromap` is enabled: the device can attach opacity
    /// micromaps to triangle geometry, letting a ray resolve coverage without invoking the
    /// any-hit classifier on micro-triangles known to be wholly covered or wholly cut out.
    /// Requires [`Self::rt_supported`]; a micromap is only meaningful under an AS build.
    pub opacity_micromap: bool,
    /// `maxOpacity4StateSubdivisionLevel` — the deepest 4-state subdivision this device accepts.
    ///
    /// A usage row above it is a VU violation, which on this class of driver is DEVICE LOSS rather
    /// than a validation message, so a cooked derivation has to be clamped against it at upload
    /// rather than trusted. Zero when the extension is absent.
    pub omm_max_subdivision: u32,
    /// `VK_NV_cluster_acceleration_structure::clusterAccelerationStructure` is enabled: cooked
    /// triangle clusters build directly into cluster acceleration structures, and per-prototype
    /// bottom-level structures compose from cluster references instead of re-deriving from
    /// triangle streams. Requires [`Self::rt_supported`], and only at the transcribed spec
    /// revision — any other revision refuses to enable (a hand-written binding has no
    /// generator following a layout change).
    pub cluster_acceleration_structure: bool,
    /// `VK_NV_partitioned_acceleration_structure::partitionedAccelerationStructure` is enabled:
    /// the top-level structure is partitioned, so a frame rewrites only the partitions whose
    /// instances changed instead of rebuilding the whole table. Requires [`Self::rt_supported`],
    /// and only at the transcribed spec revision.
    pub partitioned_acceleration_structure: bool,
    /// `maxPartitionCount` from the partitioned-AS properties; zero when the extension is
    /// absent. Partition assignment clamps against it, and a scene needing more partitions
    /// folds the excess into the global partition rather than addressing past the bound.
    pub max_partition_count: u32,
    /// `maxTrianglesPerCluster`, `maxVerticesPerCluster`, `clusterScratchByteAlignment`,
    /// `clusterByteAlignment`, `clusterBottomLevelByteAlignment` from the cluster-AS
    /// properties, in that order. All zero when the extension is absent. A cooked cluster
    /// above either count bound falls back to the KHR triangle build for its whole mesh
    /// rather than splitting.
    pub cluster_as_limits: [u32; 5],
    /// `VK_EXT_mesh_shader::meshShader` is enabled.
    pub mesh_shader: bool,
    /// `VK_EXT_mesh_shader::taskShader` is enabled independently of mesh shaders.
    pub task_shader: bool,
    /// Maximum mesh workgroups dispatched in each dimension.
    pub max_mesh_work_group_count: [u32; 3],
    /// Maximum invocations in one mesh workgroup.
    pub max_mesh_work_group_invocations: u32,
    /// Maximum vertices emitted by one mesh workgroup.
    pub max_mesh_output_vertices: u32,
    /// Maximum primitives emitted by one mesh workgroup.
    pub max_mesh_output_primitives: u32,
    /// Maximum task workgroups dispatched in each dimension.
    pub max_task_work_group_count: [u32; 3],
    /// Maximum invocations in one task workgroup.
    pub max_task_work_group_invocations: u32,
    /// Maximum task payload size in bytes.
    pub max_task_payload_size: u32,
    /// The device supports `PolygonMode::LINE` (the wireframe view mode).
    pub fill_mode_non_solid: bool,
    /// `VK_EXT_memory_budget` is enabled (driver-reported VRAM telemetry).
    pub memory_budget: bool,
    /// `pipelineStatisticsQuery` is enabled (the deepest profiler level).
    pub pipeline_stats: bool,
    /// The device is a software rasterizer (llvmpipe / lavapipe / swiftshader).
    /// GPU timings on such a device are really CPU rasterization time.
    pub software_gpu: bool,
    /// The surface allows `TRANSFER_SRC` swapchain images (window screenshots).
    pub capture_supported: bool,
    /// The effective anisotropic-filtering cap for the material sampler: `1.0` when the
    /// device lacks `samplerAnisotropy`, else `min(16, maxSamplerAnisotropy)`.
    pub max_anisotropy: f32,
    /// Core `multiDrawIndirect`: one indirect command can issue more than one draw.
    pub multi_draw_indirect: bool,
    /// A GPU-written draw count can drive indirect-count commands without CPU readback.
    pub draw_indirect_count: bool,
    /// Maximum draw count accepted by indirect draw commands.
    pub max_draw_indirect_count: u32,
    /// Buffer device addresses are enabled.
    pub buffer_device_address: bool,
    /// Shader draw parameters are enabled.
    pub shader_draw_parameters: bool,
    /// Runtime-sized descriptor arrays are enabled.
    pub runtime_descriptor_array: bool,
    /// Partially bound descriptor arrays are enabled.
    pub descriptor_binding_partially_bound: bool,
    /// Sampled-image descriptors may be updated after binding.
    pub descriptor_binding_sampled_image_update_after_bind: bool,
    /// Sampled-image arrays support non-uniform indexing.
    pub shader_sampled_image_array_non_uniform_indexing: bool,
    /// Maximum update-after-bind descriptors across all descriptor pools.
    pub max_update_after_bind_descriptors_in_all_pools: u32,
    /// Per-stage sampled-image limit for update-after-bind descriptors.
    pub max_per_stage_descriptor_update_after_bind_sampled_images: u32,
    /// Per-set sampled-image limit for update-after-bind descriptors.
    pub max_descriptor_set_update_after_bind_sampled_images: u32,
    /// Per-stage sampler limit for update-after-bind descriptors.
    pub max_per_stage_descriptor_update_after_bind_samplers: u32,
    /// Per-set sampler limit for update-after-bind descriptors.
    pub max_descriptor_set_update_after_bind_samplers: u32,
    /// Total update-after-bind resources visible to one shader stage.
    pub max_per_stage_update_after_bind_resources: u32,
    /// Maximum equal capacity of each bindless array after accounting for the full set layout.
    pub max_bindless_array_elements: u32,
    /// Native subgroup width.
    pub subgroup_size: u32,
    /// Shader stages supporting subgroup operations.
    pub subgroup_supported_stages: vk::ShaderStageFlags,
    /// Supported subgroup operation classes.
    pub subgroup_supported_operations: vk::SubgroupFeatureFlags,
    /// Quad operations are supported in every advertised subgroup stage.
    pub subgroup_quad_operations_in_all_stages: bool,
    /// Required subgroup sizes may be selected per pipeline stage.
    pub subgroup_size_control: bool,
    /// Compute workgroups can require complete subgroups.
    pub compute_full_subgroups: bool,
    /// Smallest selectable subgroup size.
    pub min_subgroup_size: u32,
    /// Largest selectable subgroup size.
    pub max_subgroup_size: u32,
    /// Maximum subgroups in one compute workgroup.
    pub max_compute_workgroup_subgroups: u32,
    /// Stages that accept a required subgroup size.
    pub required_subgroup_size_stages: vk::ShaderStageFlags,
    /// `PhysicalDeviceAccelerationStructureFeaturesKHR::accelerationStructureIndirectBuild`: a BLAS
    /// can be built with a GPU-provided primitive count. Only meaningful when
    /// [`Capabilities::rt_supported`].
    pub acceleration_structure_indirect_build: bool,
    /// `minUniformBufferOffsetAlignment` — the required alignment of a dynamic-UBO offset. The
    /// per-view grade UBO packs one aligned `GradeUniform` per frame-in-flight against it.
    pub min_uniform_buffer_offset_alignment: u64,
}

/// The GPU-timestamp profiler facts read once from the physical device at init,
/// used to seed [`crate::GpuProfiler`].
#[derive(Debug, Clone, Default)]
pub struct ProfilerFacts {
    /// ns per timestamp tick (the device limit).
    pub timestamp_period: f32,
    /// The common timestamp mask used to compare graphics and compute samples.
    pub timestamp_mask: u64,
    /// The graphics queue family's native `timestampValidBits` mask.
    pub graphics_timestamp_mask: u64,
    /// The independent compute queue family's native mask, when its timestamps are usable.
    pub compute_timestamp_mask: Option<u64>,
    /// `validBits != 0` — timestamps are usable on the graphics queue.
    pub timestamps_supported: bool,
    /// The `pipelineStatisticsQuery` feature is enabled (the deepest profiler level).
    pub pipeline_stats_supported: bool,
    /// `VK_EXT_calibrated_timestamps` is enabled and both the device + host
    /// `CLOCK_MONOTONIC` domains are calibrateable — GPU spans can project onto the CPU clock.
    pub calibration_available: bool,
    /// The calibrateable host domain (`CLOCK_MONOTONIC`) when available.
    pub host_domain: vk::TimeDomainEXT,
    /// The physical-device name (for capture metadata).
    pub device_name: String,
}

/// Exact Vulkan device/profile identity used by cross-device conformance evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VulkanDeviceIdentity {
    /// Physical-device name.
    pub name: String,
    /// Vulkan physical-device class.
    pub device_type: vk::PhysicalDeviceType,
    /// PCI/vendor identity reported by Vulkan.
    pub vendor_id: u32,
    /// Device identity reported by Vulkan.
    pub device_id: u32,
    /// Driver version reported by Vulkan.
    pub driver_version: u32,
    /// Vulkan API version exposed by the physical device.
    pub api_version: u32,
    /// Vulkan driver implementation identity.
    pub driver_id: u32,
    /// Stable Vulkan physical-device UUID.
    pub device_uuid: [u8; vk::UUID_SIZE],
    /// Stable Vulkan driver UUID.
    pub driver_uuid: [u8; vk::UUID_SIZE],
}

impl VulkanDeviceIdentity {
    /// Whether this is physical integrated or discrete GPU hardware.
    #[must_use]
    pub fn is_physical_gpu(&self) -> bool {
        matches!(
            self.device_type,
            vk::PhysicalDeviceType::INTEGRATED_GPU | vk::PhysicalDeviceType::DISCRETE_GPU
        )
    }

    /// Stable lowercase Vulkan device-class spelling.
    #[must_use]
    pub fn device_type_name(&self) -> &'static str {
        match self.device_type {
            vk::PhysicalDeviceType::INTEGRATED_GPU => "integrated-gpu",
            vk::PhysicalDeviceType::DISCRETE_GPU => "discrete-gpu",
            vk::PhysicalDeviceType::VIRTUAL_GPU => "virtual-gpu",
            vk::PhysicalDeviceType::CPU => "cpu",
            _ => "other",
        }
    }

    /// Whether Vulkan reports the MoltenVK driver implementation.
    #[must_use]
    pub fn is_molten_vk(&self) -> bool {
        self.driver_id == vk::DriverId::MOLTENVK.as_raw() as u32
    }
}

/// The immutable Vulkan core shared `&Device` by every later sub-state.
///
/// Field order is load-bearing: Rust drops fields top-to-bottom, so the allocator
/// is dropped before the device, the device before the surface/instance.
/// `waitGpuIdle` before any teardown is
/// the run loop's responsibility (the host's `Drop`), so nothing here is freed
/// under a live GPU read.
pub struct Device {
    /// Resolved optional-feature capability flags.
    pub capabilities: Capabilities,
    /// The graphics-and-present queue family index.
    pub graphics_queue_family: u32,
    /// The single externally synchronized graphics-and-present queue.
    pub graphics_queue: GpuQueue,
    /// Dedicated compute queue family used for useful independent overlap.
    pub compute_queue_family: Option<u32>,
    /// Queue index within [`Device::compute_queue_family`].
    pub compute_queue_index: Option<u32>,
    /// Dedicated compute queue; absent when graph compute executes on graphics.
    pub compute_queue: Option<GpuQueue>,
    /// Timestamp-valid bit count for the compute queue family.
    pub compute_timestamp_valid_bits: Option<u32>,
    /// The surface present mode chosen for the swapchain (FIFO).
    pub surface_format: vk::SurfaceFormatKHR,

    // The ash device + VMA allocator, behind one `Arc` so a GPU resource can free
    // itself in its `Drop` without a live `&Device` (the resources clone this `Arc`).
    // The bundle's own `Drop` frees the allocator before the device; this `Arc` is
    // normally the last holder (the run loop's `wait_idle` + resource teardown
    // precede `Device::drop`), so device destruction happens after every resource.
    // `Option` so `Device::drop` can release it (freeing the device) *before*
    // destroying the instance — the device must outlive nothing but its children
    // and die before the instance.
    resources: Option<Arc<DeviceResources>>,
    swapchain_loader: swapchain::Device,
    // The `VK_KHR_acceleration_structure` device dispatch: a cheap handle + resolved
    // fn-pointer table. Present
    // only when `capabilities.rt_supported` — the build path and the `AccelerationStructure`
    // Drop go through it; on a software device it stays `None` and every RT path is a no-op.
    accel: Option<accel::Device>,
    // The `VK_EXT_opacity_micromap` device dispatch (micromap build + copy), present only
    // when `capabilities.opacity_micromap`; `None` everywhere else.
    opacity_micromap: Option<ash::ext::opacity_micromap::Device>,
    // The `VK_NV_cluster_acceleration_structure` dispatch (hand-resolved: the pinned ash
    // ships no binding), present only when `capabilities.cluster_acceleration_structure`
    // and both entry points resolved; `None` everywhere else.
    cluster_as: Option<crate::vk_nv_cluster::Dispatch>,
    // The `VK_NV_partitioned_acceleration_structure` dispatch, hand-resolved on the same
    // terms; `None` everywhere the extension is absent.
    ptlas: Option<crate::vk_nv_ptlas::Dispatch>,
    // The `VK_EXT_mesh_shader` device dispatch (`cmd_draw_mesh_tasks`), present only when
    // mesh shaders are enabled; `None` on hardware/llvmpipe without the extension.
    mesh_shader: Option<ash::ext::mesh_shader::Device>,
    // The `VK_EXT_calibrated_timestamps` device dispatch,
    // present only when the extension is enabled and both the device and the
    // host `CLOCK_MONOTONIC` domains are calibrateable. The profiler's periodic
    // `calibrate` samples through it to project GPU ticks onto the CPU clock; `None`
    // when absent (the software / NVIDIA-without-it path keeps the own-axis fallback).
    calibrated_ts: Option<calibrated_timestamps::Device>,
    // The `VK_KHR_surface` instance dispatch + the surface, present only for the
    // windowed host ([`SurfaceSource::Window`]). The offscreen host
    // ([`SurfaceSource::Offscreen`]) enables no surface extension and creates no
    // surface, so both stay `None` and the swapchain path never runs.
    surface_loader: Option<surface::Instance>,
    surface: Option<vk::SurfaceKHR>,
    debug_messenger: Option<vk::DebugUtilsMessengerEXT>,
    debug_loader: Option<debug_utils::Instance>,
    physical_device: vk::PhysicalDevice,
    instance: ash::Instance,
    entry: ash::Entry,
}

/// The Vulkan API version the engine targets (1.3 — the highest VMA 0.4 supports;
/// 1.3 covers dynamic rendering + sync2 + the descriptor indexing the bindless path
/// needs, and lavapipe exposes a 1.4 device which satisfies a 1.3 instance request).
const API_VERSION: u32 = vk::API_VERSION_1_3;

fn timestamp_valid_mask(valid_bits: u32) -> u64 {
    if valid_bits >= 64 {
        u64::MAX
    } else {
        (1u64 << valid_bits) - 1
    }
}

fn normalized_timestamp_masks(
    graphics_valid_bits: u32,
    compute_valid_bits: Option<u32>,
) -> (u64, u64, Option<u64>) {
    let compute_valid_bits = compute_valid_bits.filter(|bits| *bits != 0);
    let common_valid_bits = compute_valid_bits
        .map(|bits| bits.min(graphics_valid_bits))
        .unwrap_or(graphics_valid_bits);
    (
        timestamp_valid_mask(common_valid_bits),
        timestamp_valid_mask(graphics_valid_bits),
        compute_valid_bits.map(timestamp_valid_mask),
    )
}

fn resolve_rt_capabilities(
    extensions_available: bool,
    acceleration_structure: bool,
    ray_query: bool,
    indirect_build: bool,
) -> (bool, bool) {
    let rt_supported = extensions_available && acceleration_structure && ray_query;
    (rt_supported, rt_supported && indirect_build)
}

impl Device {
    /// Returns the immutable Vulkan identity used to qualify exact compute semantics.
    pub fn device_identity(&self) -> VulkanDeviceIdentity {
        let mut id = vk::PhysicalDeviceIDProperties::default();
        let mut driver = vk::PhysicalDeviceDriverProperties::default();
        let mut properties = vk::PhysicalDeviceProperties2::default()
            .push_next(&mut id)
            .push_next(&mut driver);
        unsafe {
            self.instance
                .get_physical_device_properties2(self.physical_device, &mut properties);
        }
        VulkanDeviceIdentity {
            name: properties
                .properties
                .device_name_as_c_str()
                .ok()
                .and_then(|name| name.to_str().ok())
                .unwrap_or("")
                .to_owned(),
            device_type: properties.properties.device_type,
            vendor_id: properties.properties.vendor_id,
            device_id: properties.properties.device_id,
            driver_version: properties.properties.driver_version,
            api_version: properties.properties.api_version,
            driver_id: driver.driver_id.as_raw() as u32,
            device_uuid: id.device_uuid,
            driver_uuid: id.driver_uuid,
        }
    }

    /// Brings up the full Vulkan core against `surface_source`.
    ///
    /// Creates the instance (validation layer in debug), the surface, selects a
    /// physical device by the required feature set, probes the optional features,
    /// creates the logical device + graphics queue, the VMA allocator, and resolves
    /// the optional extension dispatch tables. Returns the immutable [`Device`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::Loader`] if the Vulkan loader is unavailable,
    /// [`Error::NoDevice`] if no device satisfies the required features,
    /// [`Error::NoQueueFamily`] if no graphics+present family exists, or
    /// [`Error::Vk`] for any failing Vulkan call.
    pub fn new(surface_source: &SurfaceSource<'_>) -> Result<Self> {
        // The entry owns the dynamically-loaded `libvulkan`, held for the whole `Device`
        // lifetime (the instance/device dispatch through it).
        let entry = load_entry()?;

        // Validation runs in debug builds (or when forced) and never in release — it is a
        // heavy per-command CPU cost, not a shipping feature. The instance layer/extension and
        // the debug messenger gate on the one decision so they never disagree.
        let validation = validation_enabled(&entry);
        let instance = create_instance(&entry, surface_source, validation)?;
        let (debug_loader, debug_messenger) =
            create_debug_messenger(&entry, &instance, validation)?;
        // The surface (and its `VK_KHR_surface` dispatch) exist only for the windowed
        // host; the offscreen host has neither.
        let (surface_loader, surface) = match surface_source {
            SurfaceSource::Window(window) => {
                let loader = surface::Instance::new(&entry, &instance);
                let surface = create_window_surface(&entry, &instance, *window)?;
                (Some(loader), Some(surface))
            }
            SurfaceSource::Offscreen => (None, None),
        };

        // Present support gates selection only for the windowed host, which presents
        // through a real swapchain. The offscreen host renders into an offscreen image
        // and reads back (never presents), so it has no surface to query present support
        // against — gating on present there would have no surface and wrongly reject every
        // GPU. The offscreen host drops the surface entirely.
        let require_present = surface.is_some();
        let selection =
            select_physical_device(&instance, surface_loader.as_ref(), surface, require_present)?;
        let physical_device = selection.physical_device;
        let graphics_queue_family = selection.graphics_queue_family;
        let compute_queue_family = selection.compute_queue.map(|queue| queue.family);
        let compute_queue_index = selection.compute_queue.map(|queue| queue.index);
        log_selected_device(&instance, physical_device);

        let (device, calibrated_ts_enabled, checkpoints_enabled, device_fault_enabled) =
            create_logical_device(
                &instance,
                physical_device,
                graphics_queue_family,
                selection.compute_queue,
                &selection.capabilities,
                require_present,
            )?;
        // SAFETY: the family/index pair was just used to create the device with one
        // queue at index 0 of that family.
        let graphics_queue =
            GpuQueue::new(unsafe { device.get_device_queue(graphics_queue_family, 0) });
        let compute_queue = selection.compute_queue.map(|queue| {
            GpuQueue::new(unsafe { device.get_device_queue(queue.family, queue.index) })
        });
        let queue_families =
            unsafe { instance.get_physical_device_queue_family_properties(physical_device) };
        let compute_timestamp_valid_bits =
            compute_queue_family.map(|family| queue_families[family as usize].timestamp_valid_bits);

        let allocator = create_allocator(&instance, &device, physical_device)?;
        let swapchain_loader = swapchain::Device::new(&instance, &device);
        // Resolve the acceleration-structure dispatch only when the RT extensions were
        // enabled on the device.
        let accel = if selection.capabilities.rt_supported {
            Some(accel::Device::new(&instance, &device))
        } else {
            None
        };
        // Resolve the micromap dispatch only when the extension was enabled on the device.
        let opacity_micromap = if selection.capabilities.opacity_micromap {
            Some(ash::ext::opacity_micromap::Device::new(&instance, &device))
        } else {
            None
        };
        // Resolve the cluster-AS dispatch only when the extension was enabled. Unlike ash's
        // generated loaders, a null proc address here yields `None` rather than a panicking
        // stub, and the capability is withdrawn with it so no caller sees "supported" with
        // no way to build.
        let ptlas = if selection.capabilities.partitioned_acceleration_structure {
            let dispatch = crate::vk_nv_ptlas::Dispatch::load(&instance, &device);
            if dispatch.is_none() {
                tracing::warn!(
                    "partitioned acceleration structures advertised but the entry points did \
                     not resolve — disabling"
                );
            }
            dispatch
        } else {
            None
        };
        let cluster_as = if selection.capabilities.cluster_acceleration_structure {
            let dispatch = crate::vk_nv_cluster::Dispatch::load(&instance, &device);
            if dispatch.is_none() {
                tracing::warn!(
                    "cluster acceleration structures advertised but the entry points did not \
                     resolve — disabling"
                );
            }
            dispatch
        } else {
            None
        };
        // Resolve the mesh-shader dispatch (`cmd_draw_mesh_tasks`) only when the extension was
        // enabled on the device.
        let mesh_shader = if selection.capabilities.mesh_shader {
            Some(ash::ext::mesh_shader::Device::new(&instance, &device))
        } else {
            None
        };
        // VK_EXT_calibrated_timestamps: only when the extension was enabled on the device AND
        // both a device domain and the host CLOCK_MONOTONIC domain are calibrateable can the
        // read-back project GPU spans onto the CPU clock. Otherwise correlation stays off
        // and GPU spans keep their own axis.
        let calibrated_ts = calibrated_ts_enabled
            .then(|| {
                let instance_loader = calibrated_timestamps::Instance::new(&entry, &instance);
                // SAFETY: the ash seam. The physical device is valid; the query is read-only.
                let domains = unsafe {
                    instance_loader.get_physical_device_calibrateable_time_domains(physical_device)
                }
                .unwrap_or_default();
                let has_device = domains.contains(&vk::TimeDomainEXT::DEVICE);
                let has_monotonic = domains.contains(&vk::TimeDomainEXT::CLOCK_MONOTONIC);
                (has_device && has_monotonic)
                    .then(|| calibrated_timestamps::Device::new(&instance, &device))
            })
            .flatten();
        if calibrated_ts.is_some() {
            tracing::info!(
                "calibrated timestamps available — GPU spans correlate to the CPU clock"
            );
        } else {
            tracing::info!("calibrated timestamps unavailable — GPU spans stay on their own axis");
        }
        // Every acceleration-structure build scratch address must be a multiple of
        // `minAccelerationStructureScratchOffsetAlignment` (VUID-…-pInfos-03710). The
        // resource bundle carries it so the scratch allocator can demand it from VMA.
        let mut selection = selection;
        let scratch_alignment = if selection.capabilities.rt_supported {
            let mut accel_props = vk::PhysicalDeviceAccelerationStructurePropertiesKHR::default();
            let mut omm_props = vk::PhysicalDeviceOpacityMicromapPropertiesEXT::default();
            let mut props2 = vk::PhysicalDeviceProperties2::default().push_next(&mut accel_props);
            if selection.capabilities.opacity_micromap {
                props2 = props2.push_next(&mut omm_props);
            }
            // SAFETY: the ash seam. Fills the chained AS properties for a device that
            // enabled `VK_KHR_acceleration_structure`, and the micromap properties only when
            // that extension was enabled too.
            unsafe { instance.get_physical_device_properties2(physical_device, &mut props2) };
            selection.capabilities.omm_max_subdivision =
                omm_props.max_opacity4_state_subdivision_level;
            vk::DeviceSize::from(
                accel_props
                    .min_acceleration_structure_scratch_offset_alignment
                    .max(1),
            )
        } else {
            1
        };
        // Resolve the loss-diagnostic dispatches only when their extensions were enabled on the
        // device; a device loss then names the last pass each queue stage reached and the
        // driver's fault report.
        let checkpoints =
            checkpoints_enabled.then(|| crate::checkpoints::Checkpoints::new(&instance, &device));
        let device_fault =
            device_fault_enabled.then(|| crate::checkpoints::DeviceFault::new(&instance, &device));
        let resources = DeviceResources::new(
            device,
            allocator,
            scratch_alignment,
            checkpoints,
            device_fault,
        );

        // The surface queries are valid only when a surface exists. The windowed host
        // queries it for its swapchain format + capture support; the offscreen host has no
        // surface, so it takes the preferred default format directly (used only for the
        // offscreen / read-back target) and reports no window-capture support (a
        // windowed-only feature).
        let (surface_format, capture_supported) = match (&surface_loader, surface) {
            (Some(loader), Some(surface)) => (
                choose_surface_format(loader, physical_device, surface)?,
                surface_capture_supported(loader, physical_device, surface),
            ),
            _ => (PREFERRED_SURFACE_FORMAT, false),
        };

        let capabilities = Capabilities {
            capture_supported,
            cluster_acceleration_structure: cluster_as.is_some(),
            partitioned_acceleration_structure: ptlas.is_some(),
            ..selection.capabilities
        };
        log_software_gpu(&capabilities);

        Ok(Self {
            capabilities,
            graphics_queue_family,
            graphics_queue,
            compute_queue_family,
            compute_queue_index,
            compute_queue,
            compute_timestamp_valid_bits,
            surface_format,
            resources: Some(resources),
            swapchain_loader,
            accel,
            opacity_micromap,
            cluster_as,
            ptlas,
            mesh_shader,
            calibrated_ts,
            surface_loader,
            surface,
            debug_messenger,
            debug_loader,
            physical_device,
            instance,
            entry,
        })
    }

    /// The logical device handle (for resource creation).
    pub fn raw(&self) -> &ash::Device {
        self.bundle().device()
    }

    /// The shared device + allocator bundle a GPU resource wrapper clones so it can
    /// free itself in its own `Drop`. The handle through which
    /// [`crate::Buffer`] / [`crate::Image`] / … resources are created.
    pub fn resources(&self) -> &Arc<DeviceResources> {
        self.bundle()
    }

    /// The shared bundle; present for the whole device lifetime, only `Device::drop`
    /// releases it (to free the device before the instance).
    fn bundle(&self) -> &Arc<DeviceResources> {
        self.resources
            .as_ref()
            .expect("device resources live until Device::drop")
    }

    /// The selected physical device.
    pub fn physical_device(&self) -> vk::PhysicalDevice {
        self.physical_device
    }

    /// The surface the swapchain is built against — present only for the windowed
    /// host ([`SurfaceSource::Window`]). `None` for the offscreen host, which never
    /// presents.
    pub fn surface(&self) -> Option<vk::SurfaceKHR> {
        self.surface
    }

    /// The `VK_KHR_surface` instance dispatch (capabilities / formats queries) —
    /// present only for the windowed host. `None` for the offscreen host.
    pub fn surface_loader(&self) -> Option<&surface::Instance> {
        self.surface_loader.as_ref()
    }

    /// The `VK_KHR_swapchain` device dispatch (create / acquire / present).
    pub fn swapchain_loader(&self) -> &swapchain::Device {
        &self.swapchain_loader
    }

    /// The `VK_KHR_acceleration_structure` device dispatch, present only when
    /// [`Capabilities::rt_supported`]. The BLAS/TLAS build path resolves its commands
    /// here, and an [`crate::AccelerationStructure`] clones it for a self-contained
    /// `Drop`. `None` on a software device.
    pub fn accel_dispatch(&self) -> Option<&accel::Device> {
        self.accel.as_ref()
    }

    /// Whether opacity micromaps can be attached to triangle geometry on this device.
    pub fn omm_supported(&self) -> bool {
        self.capabilities.opacity_micromap
    }

    /// The `VK_EXT_opacity_micromap` device dispatch (micromap build + copy), present only
    /// when [`Capabilities::opacity_micromap`]; `None` everywhere else.
    pub fn omm_dispatch(&self) -> Option<&ash::ext::opacity_micromap::Device> {
        self.opacity_micromap.as_ref()
    }

    /// The deepest 4-state subdivision level this device accepts in a micromap usage row.
    #[must_use]
    pub fn omm_max_subdivision(&self) -> u32 {
        self.capabilities.omm_max_subdivision
    }

    /// The `VK_NV_cluster_acceleration_structure` dispatch, present only when
    /// [`Capabilities::cluster_acceleration_structure`]; `None` everywhere else.
    pub(crate) fn cluster_as_dispatch(&self) -> Option<&crate::vk_nv_cluster::Dispatch> {
        self.cluster_as.as_ref()
    }

    /// Whether cluster acceleration structures are enabled on this device.
    #[must_use]
    pub fn cluster_as_supported(&self) -> bool {
        self.cluster_as.is_some()
    }

    /// The `VK_NV_partitioned_acceleration_structure` dispatch, present only when
    /// [`Capabilities::partitioned_acceleration_structure`]; `None` everywhere else.
    pub(crate) fn ptlas_dispatch(&self) -> Option<&crate::vk_nv_ptlas::Dispatch> {
        self.ptlas.as_ref()
    }

    /// Whether the top-level structure is partitioned on this device.
    #[must_use]
    pub fn ptlas_supported(&self) -> bool {
        self.ptlas.is_some()
    }

    /// The device's `maxPartitionCount`; zero when partitioned structures are absent.
    #[must_use]
    pub fn max_partition_count(&self) -> u32 {
        self.capabilities.max_partition_count
    }

    /// The `VK_EXT_mesh_shader` device dispatch (`cmd_draw_mesh_tasks`), present only when
    /// [`Capabilities::mesh_shader`]; `None` on hardware/llvmpipe without the extension.
    pub fn mesh_shader_dispatch(&self) -> Option<&ash::ext::mesh_shader::Device> {
        self.mesh_shader.as_ref()
    }

    /// Samples the device and host clocks together via `vkGetCalibratedTimestampsEXT`.
    /// Returns `(device_ticks_raw, host_ns,
    /// max_deviation)` — the device sample in query-pool tick units, the host sample on
    /// `host_domain` (`CLOCK_MONOTONIC` ns). `None` when the dispatch is absent or the
    /// call fails, so the profiler keeps GPU spans on their own axis.
    pub fn sample_calibrated_timestamps(
        &self,
        host_domain: vk::TimeDomainEXT,
    ) -> Option<(u64, u64, u64)> {
        let loader = self.calibrated_ts.as_ref()?;
        let infos = [
            vk::CalibratedTimestampInfoEXT::default().time_domain(vk::TimeDomainEXT::DEVICE),
            vk::CalibratedTimestampInfoEXT::default().time_domain(host_domain),
        ];
        // SAFETY: the ash seam. `infos` outlives the call; the extension is enabled
        // (the dispatch exists only then), and the call reads two clocks (no queue work).
        let (timestamps, max_deviation) =
            unsafe { loader.get_calibrated_timestamps(&infos) }.ok()?;
        Some((timestamps[0], timestamps[1], max_deviation))
    }

    /// Whether hardware ray tracing (acceleration-structure + ray-query) is available
    /// and enabled. Shorthand for `capabilities.rt_supported`.
    pub fn rt_supported(&self) -> bool {
        self.capabilities.rt_supported
    }

    /// Queue topology used to resolve render-graph pass assignments.
    pub fn render_graph_queue_families(&self) -> crate::RgQueueFamilies {
        crate::RgQueueFamilies {
            graphics: self.graphics_queue_family,
            async_compute: self.compute_queue_family,
        }
    }

    /// The GPU-timestamp profiler facts read once at init: the ns-per-tick period,
    /// the common queue timestamp mask, whether timestamps are usable,
    /// and the physical-device name.
    pub fn profiler_facts(&self) -> ProfilerFacts {
        // SAFETY: the ash seam. The physical device handle is valid; both queries
        // are read-only.
        let props = unsafe {
            self.instance
                .get_physical_device_properties(self.physical_device)
        };
        let families = unsafe {
            self.instance
                .get_physical_device_queue_family_properties(self.physical_device)
        };
        let graphics_valid_bits = families
            .get(self.graphics_queue_family as usize)
            .map_or(0, |f| f.timestamp_valid_bits);
        let (timestamp_mask, graphics_timestamp_mask, compute_timestamp_mask) =
            normalized_timestamp_masks(graphics_valid_bits, self.compute_timestamp_valid_bits);
        let device_name = props
            .device_name_as_c_str()
            .ok()
            .and_then(|name| name.to_str().ok())
            .unwrap_or("")
            .to_owned();
        ProfilerFacts {
            timestamp_period: props.limits.timestamp_period,
            timestamp_mask,
            graphics_timestamp_mask,
            compute_timestamp_mask,
            timestamps_supported: graphics_valid_bits != 0,
            pipeline_stats_supported: self.capabilities.pipeline_stats,
            calibration_available: self.calibrated_ts.is_some(),
            host_domain: vk::TimeDomainEXT::CLOCK_MONOTONIC,
            device_name,
        }
    }

    /// The device address of `buffer` (core 1.2 `vkGetBufferDeviceAddress`, fed to AS
    /// builds as vertex / index / instance / scratch input). The buffer must carry
    /// `SHADER_DEVICE_ADDRESS` usage.
    pub fn buffer_device_address(&self, buffer: vk::Buffer) -> vk::DeviceAddress {
        self.bundle().buffer_device_address(buffer)
    }

    /// The VMA allocator (image/buffer creation). Lives in the shared bundle,
    /// destroyed before the device when the last holder drops.
    pub fn allocator(&self) -> &vk_mem::Allocator {
        self.bundle().allocator()
    }

    /// The Vulkan instance (for instance-level PFN resolution, e.g. the debug-utils
    /// command-buffer labels that name render-graph passes).
    pub fn instance(&self) -> &ash::Instance {
        &self.instance
    }

    /// The Vulkan loader entry. Held for the whole device lifetime — the instance
    /// and device dispatch through it, and `vkGetInstanceProcAddr` (used to resolve
    /// extension command pointers) needs it.
    pub fn entry(&self) -> &ash::Entry {
        &self.entry
    }

    /// The MSAA sample counts the offscreen color (`color_format`) + depth
    /// (`depth_format`) attachments both accept: the intersection of the device's
    /// framebuffer color/depth sample limits with each format's optimal-tiling
    /// sample support. A count valid as a framebuffer limit can still be unsupported
    /// for a specific format, and creating an image with it is invalid
    /// (`VUID-VkImageCreateInfo-samples`), so the AA selector clamps against this.
    pub fn supported_sample_counts(
        &self,
        color_format: vk::Format,
        depth_format: vk::Format,
    ) -> vk::SampleCountFlags {
        // SAFETY: the ash seam. The physical device handle is valid for the call.
        let limits = unsafe {
            self.instance
                .get_physical_device_properties(self.physical_device)
                .limits
        };
        let mut counts =
            limits.framebuffer_color_sample_counts & limits.framebuffer_depth_sample_counts;
        for format in [color_format, depth_format] {
            // SAFETY: the ash seam. The format-feature query is read-only.
            let props = unsafe {
                self.instance.get_physical_device_image_format_properties(
                    self.physical_device,
                    format,
                    vk::ImageType::TYPE_2D,
                    vk::ImageTiling::OPTIMAL,
                    attachment_usage(format),
                    vk::ImageCreateFlags::empty(),
                )
            };
            match props {
                Ok(props) => counts &= props.sample_counts,
                // A format the device cannot use as an attachment supports no MSAA.
                Err(_) => return vk::SampleCountFlags::TYPE_1,
            }
        }
        // TYPE_1 is always usable even if the AND cleared it (a 1× image is valid).
        counts | vk::SampleCountFlags::TYPE_1
    }

    /// Blocks until the device is idle. The run loop calls this before any
    /// teardown so no resource is freed under a live GPU read.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Vk`] if `vkDeviceWaitIdle` fails.
    pub fn wait_idle(&self) -> Result<()> {
        let result = (|| {
            if let Some(queue) = &self.compute_queue {
                queue.wait_queue_idle(self.bundle().device())?;
            }
            self.graphics_queue.wait_device_idle(self.bundle().device())
        })();
        if result.as_ref().is_err_and(Error::is_device_loss) {
            self.log_device_loss_checkpoints();
        }
        result
    }

    /// Logs every queue's last-reached diagnostic checkpoints and the driver's fault report
    /// after a device loss, naming the submission the GPU wedged in. A no-op without the
    /// diagnostic extensions.
    pub fn log_device_loss_checkpoints(&self) {
        let checkpoints = self.bundle().checkpoints();
        self.graphics_queue.log_device_loss_checkpoints(checkpoints);
        if let Some(queue) = &self.compute_queue {
            queue.log_device_loss_checkpoints(checkpoints);
        }
        self.bundle().log_device_fault();
    }

    /// Records `body` on a throwaway command buffer, submits it, and waits for it.
    ///
    /// For out-of-band work a person asked for — a debug capture, a test — never for
    /// anything on the frame path, which submits through the frame ring instead. The
    /// wait is the whole point: the caller reads the result immediately after.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Vk`] if any of the pool, buffer, fence, submit, or wait fails.
    /// Every resource this creates is destroyed before returning, on success or failure.
    pub fn one_shot_transfer<F>(&self, body: F) -> Result<()>
    where
        F: FnOnce(&ash::Device, vk::CommandBuffer),
    {
        let raw = self.raw();
        let pool_info =
            vk::CommandPoolCreateInfo::default().queue_family_index(self.graphics_queue_family);
        // SAFETY: the ash seam. The pool, buffer, and fence are created here and
        // destroyed below on every path; the fence is waited before anything is freed.
        unsafe {
            let pool = raw
                .create_command_pool(&pool_info, None)
                .map_err(|result| Error::Vk {
                    context: "one-shot command pool",
                    result,
                })?;
            let recorded = (|| -> Result<()> {
                let alloc = vk::CommandBufferAllocateInfo::default()
                    .command_pool(pool)
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_buffer_count(1);
                let cmd = raw
                    .allocate_command_buffers(&alloc)
                    .map_err(|result| Error::Vk {
                        context: "one-shot command buffer",
                        result,
                    })?[0];
                let fence = raw
                    .create_fence(&vk::FenceCreateInfo::default(), None)
                    .map_err(|result| Error::Vk {
                        context: "one-shot fence",
                        result,
                    })?;
                let submitted = (|| -> Result<()> {
                    raw.begin_command_buffer(
                        cmd,
                        &vk::CommandBufferBeginInfo::default()
                            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
                    )
                    .map_err(|result| Error::Vk {
                        context: "one-shot begin",
                        result,
                    })?;
                    body(raw, cmd);
                    raw.end_command_buffer(cmd).map_err(|result| Error::Vk {
                        context: "one-shot end",
                        result,
                    })?;
                    let cmd_info = [vk::CommandBufferSubmitInfo::default().command_buffer(cmd)];
                    let submit = [vk::SubmitInfo2::default().command_buffer_infos(&cmd_info)];
                    self.graphics_queue
                        .submit2(raw, &submit, fence, "one-shot")?;
                    raw.wait_for_fences(&[fence], true, u64::MAX)
                        .map_err(|result| Error::Vk {
                            context: "one-shot wait",
                            result,
                        })
                })();
                raw.destroy_fence(fence, None);
                submitted
            })();
            raw.destroy_command_pool(pool, None);
            recorded
        }
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        // Teardown order, surface → device → instance:
        // the run loop's `wait_idle` ran first and every device-borrowing sub-state
        // (resources, swapchain, frame ring) was already destroyed by the owner.
        //
        // 1. Destroy the surface + debug messenger (instance-children, valid to free
        //    while the device is still alive). The surface exists only for the windowed
        //    host; the offscreen host created none.
        // SAFETY: the ash seam. The device is idle; both were created on this
        // instance and are destroyed exactly once, before the instance below.
        unsafe {
            if let (Some(loader), Some(surface)) = (&self.surface_loader, self.surface) {
                loader.destroy_surface(surface, None);
            }
            if let (Some(loader), Some(messenger)) = (&self.debug_loader, self.debug_messenger) {
                loader.destroy_debug_utils_messenger(messenger, None);
            }
        }

        // 2. Release the shared bundle. When this is the last `Arc<DeviceResources>`
        //    clone (the normal case after the run loop's resource teardown), its
        //    `Drop` frees the allocator then `vkDestroyDevice` here, synchronously,
        //    *before* the instance is destroyed in step 3. A surviving clone (a
        //    resource that outlived the device) is a host-teardown contract
        //    violation; debug builds would surface it as a device alive past its
        //    instance, which the validation layer flags.
        drop(self.resources.take());

        // 3. Destroy the instance last (the device is gone). `ash::Instance` carries
        //    no `Drop`, so this is explicit.
        // SAFETY: the ash seam. The device was destroyed in step 2; the instance is
        // destroyed exactly once after all its children.
        unsafe { self.instance.destroy_instance(None) };
    }
}

#[cfg(test)]
mod tests;
