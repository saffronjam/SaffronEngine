//! The immutable-after-init Vulkan core: instance, surface, physical device,
//! logical device, graphics queue, the VMA allocator, the resolved feature
//! capabilities, and the loaded extension dispatch tables.
//!
//! The README's borrow strategy (§2) names this the bucket constructed once and then
//! borrowed `&Device` everywhere — never `&mut` after init — which is what lets many
//! passes hold a handle while siblings mutate. The ~150-LOC feature-probe /
//! degradation chain (`enable_*_if_present`) is hand-rolled here.

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

    /// Whether `VK_EXT_mesh_shader::meshShader` is enabled.
    pub fn mesh_shader_enabled(&self) -> bool {
        self.capabilities.mesh_shader
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

/// Picks the platform instance extensions and creates the instance with
/// validation in debug builds.
///
/// The windowed host enables `VK_KHR_surface` + the platform surface extension; the
/// offscreen host enables **no** surface extension at all. A no-surface instance is
/// what lets the editor host boot under the NVIDIA ICD: that driver implements no
/// headless surface, so requesting one would fail `create_instance` with
/// `ERROR_EXTENSION_NOT_PRESENT`.
/// Loads the Vulkan loader (`libvulkan`) into an [`ash::Entry`].
///
/// Everywhere but macOS this is `Entry::load`, which finds the loader on the system library path.
/// macOS has no native Vulkan and no default search entry for Homebrew's `/opt/homebrew/lib`
/// (Apple Silicon) or `/usr/local/lib` (Intel); worse, macOS strips `DYLD_*` env vars across the
/// editor→host spawn (SIP), so a `DYLD_FALLBACK_LIBRARY_PATH` cannot be relied on to reach here.
/// On macOS an exported app's bundled MoltenVK library is loaded directly. Development runs try the
/// known Homebrew Vulkan loader paths so validation layers remain available, then fall back to the
/// default `Entry::load`.
fn load_entry() -> Result<ash::Entry> {
    // SAFETY: the ash seam. `Entry::load*` dynamically loads `libvulkan`; the returned entry owns
    // the loader for the caller's use.
    #[cfg(target_os = "macos")]
    {
        let mut loader_paths = Vec::new();
        if let Some(executable_dir) = std::env::current_exe()
            .ok()
            .and_then(|path| path.parent().map(std::path::Path::to_path_buf))
        {
            let bundled_moltenvk = executable_dir
                .join("..")
                .join("Frameworks")
                .join("libMoltenVK.dylib");
            if bundled_moltenvk.is_file()
                && let Ok(entry) = unsafe { ash::Entry::load_from(&bundled_moltenvk) }
            {
                return Ok(entry);
            }
        }
        loader_paths.extend(
            [
                "/opt/homebrew/lib/libvulkan.dylib",
                "/opt/homebrew/lib/libvulkan.1.dylib",
                "/usr/local/lib/libvulkan.dylib",
                "/usr/local/lib/libvulkan.1.dylib",
            ]
            .into_iter()
            .map(std::path::PathBuf::from),
        );
        for path in loader_paths {
            if path.is_file()
                && let Ok(entry) = unsafe { ash::Entry::load_from(&path) }
            {
                return Ok(entry);
            }
        }
    }
    unsafe { ash::Entry::load() }.map_err(|err| Error::Loader(err.to_string()))
}

fn create_instance(
    entry: &ash::Entry,
    surface_source: &SurfaceSource<'_>,
    validation: bool,
) -> Result<ash::Instance> {
    let app_name = c"Saffron Anima";
    let app_info = vk::ApplicationInfo::default()
        .application_name(app_name)
        .engine_name(app_name)
        .api_version(API_VERSION);

    let mut extensions: Vec<*const c_char> = Vec::new();
    match surface_source {
        SurfaceSource::Window(window) => {
            extensions.push(surface::NAME.as_ptr());
            let display = window
                .display_handle()
                .map_err(|err| Error::NoSurfaceHandle(err.to_string()))?;
            let required =
                ash_window::enumerate_required_extensions(display.as_raw()).map_err(|result| {
                    Error::Vk {
                        context: "enumerate_required_extensions",
                        result,
                    }
                })?;
            extensions.extend_from_slice(required);
        }
        SurfaceSource::Offscreen => {}
    }

    let mut layers: Vec<*const c_char> = Vec::new();
    if validation {
        extensions.push(debug_utils::NAME.as_ptr());
        layers.push(VALIDATION_LAYER.as_ptr());
    }

    // A portability driver (MoltenVK, the only Vulkan on macOS) is hidden from device
    // enumeration unless the instance opts in with `VK_KHR_portability_enumeration` plus the
    // matching create flag. Enable it whenever the loader advertises the extension; on a native
    // ICD the extension is absent and this is a no-op, so there is one code path for every host.
    let portability = instance_extension_available(entry, ash::khr::portability_enumeration::NAME);
    if portability {
        extensions.push(ash::khr::portability_enumeration::NAME.as_ptr());
    }
    let flags = if portability {
        vk::InstanceCreateFlags::ENUMERATE_PORTABILITY_KHR
    } else {
        vk::InstanceCreateFlags::empty()
    };

    let create_info = vk::InstanceCreateInfo::default()
        .flags(flags)
        .application_info(&app_info)
        .enabled_extension_names(&extensions)
        .enabled_layer_names(&layers);

    // SAFETY: the ash seam. The extension/layer name pointers are valid `CStr`s
    // borrowed for the duration of the call; the create-info struct outlives it.
    let instance =
        unsafe { entry.create_instance(&create_info, None) }.map_err(|result| Error::Vk {
            context: "create_instance",
            result,
        })?;
    Ok(instance)
}

/// The single validation layer the engine enables in debug.
const VALIDATION_LAYER: &CStr = c"VK_LAYER_KHRONOS_validation";

/// Whether to enable the Khronos validation layer this run. Debug builds enable it (or any
/// build when `SAFFRON_FORCE_VALIDATION` is set), unless `SAFFRON_DISABLE_VALIDATION` is set;
/// release builds run without it. Validation is a heavy per-command CPU cost, so it must not
/// ship on. A debug build that wants it but lacks the installed layer logs once and continues.
fn validation_enabled(entry: &ash::Entry) -> bool {
    let wanted = (cfg!(debug_assertions) || std::env::var_os("SAFFRON_FORCE_VALIDATION").is_some())
        && std::env::var_os("SAFFRON_DISABLE_VALIDATION").is_none();
    if !wanted {
        return false;
    }
    if validation_layer_available(entry) {
        true
    } else {
        tracing::warn!("validation layer unavailable — running without it");
        false
    }
}

/// Reports whether the loader advertises the given instance extension.
fn instance_extension_available(entry: &ash::Entry, name: &CStr) -> bool {
    // SAFETY: the ash seam. Enumerates instance extensions; no resource is created.
    let Ok(extensions) = (unsafe { entry.enumerate_instance_extension_properties(None) }) else {
        return false;
    };
    extensions.iter().any(|ext| {
        ext.extension_name_as_c_str()
            .map(|n| n == name)
            .unwrap_or(false)
    })
}

/// Reports whether the Khronos validation layer is installed.
fn validation_layer_available(entry: &ash::Entry) -> bool {
    // SAFETY: the ash seam. Enumerates instance layers; no resource is created.
    let Ok(layers) = (unsafe { entry.enumerate_instance_layer_properties() }) else {
        return false;
    };
    layers.iter().any(|layer| {
        layer
            .layer_name_as_c_str()
            .map(|name| name == VALIDATION_LAYER)
            .unwrap_or(false)
    })
}

/// Creates the debug-utils messenger that routes validation messages into the
/// engine log. Returns `(None, None)` when the extension is absent.
fn create_debug_messenger(
    entry: &ash::Entry,
    instance: &ash::Instance,
    validation: bool,
) -> Result<(
    Option<debug_utils::Instance>,
    Option<vk::DebugUtilsMessengerEXT>,
)> {
    // The messenger lives behind the validation layer (it provides `debug_utils`'s dispatch).
    // When validation is off, the extension was not enabled, so creating the messenger would
    // call a null entry point — gate on the same decision the instance used.
    if !validation {
        return Ok((None, None));
    }
    // SAFETY: the ash seam. Enumerates instance extensions; no resource created.
    let extensions =
        unsafe { entry.enumerate_instance_extension_properties(None) }.map_err(|result| {
            Error::Vk {
                context: "enumerate_instance_extension_properties",
                result,
            }
        })?;
    let present = extensions.iter().any(|ext| {
        ext.extension_name_as_c_str()
            .map(|name| name == debug_utils::NAME)
            .unwrap_or(false)
    });
    if !present {
        return Ok((None, None));
    }

    let loader = debug_utils::Instance::new(entry, instance);
    let info = vk::DebugUtilsMessengerCreateInfoEXT::default()
        .message_severity(
            vk::DebugUtilsMessageSeverityFlagsEXT::WARNING
                | vk::DebugUtilsMessageSeverityFlagsEXT::ERROR
                | vk::DebugUtilsMessageSeverityFlagsEXT::INFO,
        )
        .message_type(
            vk::DebugUtilsMessageTypeFlagsEXT::GENERAL
                | vk::DebugUtilsMessageTypeFlagsEXT::VALIDATION
                | vk::DebugUtilsMessageTypeFlagsEXT::PERFORMANCE,
        )
        .pfn_user_callback(Some(debug_callback));

    // SAFETY: the ash seam. The create-info (and its callback pointer) is valid
    // for the call; the returned messenger is owned and destroyed in `Device::drop`.
    let messenger =
        unsafe { loader.create_debug_utils_messenger(&info, None) }.map_err(|result| {
            Error::Vk {
                context: "create_debug_utils_messenger",
                result,
            }
        })?;
    Ok((Some(loader), Some(messenger)))
}

/// The validation-layer message sink. Forwards real validation/performance
/// messages to the engine log under the `vulkan` subsystem so the validation-clean
/// gate parses them, and drops loader chatter (general-type messages below error)
/// unless `SAFFRON_VK_VERBOSE` is set.
/// Always returns `VK_FALSE` (does not abort the triggering call).
unsafe extern "system" fn debug_callback(
    severity: vk::DebugUtilsMessageSeverityFlagsEXT,
    types: vk::DebugUtilsMessageTypeFlagsEXT,
    data: *const vk::DebugUtilsMessengerCallbackDataEXT<'_>,
    _user_data: *mut c_void,
) -> vk::Bool32 {
    // SAFETY: the validation layer guarantees `data` points at a valid callback
    // struct for the duration of this call; the message / id pointers are valid C
    // strings when non-null.
    let (message, id) = unsafe {
        let data = &*data;
        let message = read_c_str(data.p_message);
        let id = read_c_str(data.p_message_id_name);
        (message, id)
    };

    let verbose = std::env::var_os("SAFFRON_VK_VERBOSE").is_some();
    let loader_chatter = types == vk::DebugUtilsMessageTypeFlagsEXT::GENERAL
        && !severity.contains(vk::DebugUtilsMessageSeverityFlagsEXT::ERROR);
    if !verbose && (loader_chatter || id.contains("OutputNotConsumed")) {
        return vk::FALSE;
    }

    let level = if severity.contains(vk::DebugUtilsMessageSeverityFlagsEXT::ERROR) {
        tracing::Level::ERROR
    } else if severity.contains(vk::DebugUtilsMessageSeverityFlagsEXT::WARNING) {
        tracing::Level::WARN
    } else {
        tracing::Level::INFO
    };
    let kind = if types.contains(vk::DebugUtilsMessageTypeFlagsEXT::VALIDATION) {
        "validation"
    } else if types.contains(vk::DebugUtilsMessageTypeFlagsEXT::PERFORMANCE) {
        "performance"
    } else {
        "general"
    };

    // A real validation or performance issue (not filtered loader chatter) at
    // warning-or-error severity fails the validation-clean gate.
    if kind != "general" && level != tracing::Level::INFO {
        VALIDATION_ISSUE_COUNT.fetch_add(1, Ordering::Relaxed);
    }
    // The messenger logs on another subsystem's behalf, so it sets the target
    // explicitly rather than inheriting this crate's module path.
    let body = if id.is_empty() {
        format!("[{kind}] {message}")
    } else {
        format!("[{kind}] {id}: {message}")
    };
    match level {
        tracing::Level::ERROR => tracing::error!(target: "vulkan", "{body}"),
        tracing::Level::WARN => tracing::warn!(target: "vulkan", "{body}"),
        _ => tracing::info!(target: "vulkan", "{body}"),
    }
    vk::FALSE
}

/// Reads a possibly-null Vulkan C string into an owned `String` (empty if null).
fn read_c_str(ptr: *const c_char) -> String {
    if ptr.is_null() {
        return String::new();
    }
    // SAFETY: the validation layer guarantees a non-null pointer is a valid,
    // NUL-terminated C string for the duration of the callback.
    unsafe { CStr::from_ptr(ptr).to_string_lossy().into_owned() }
}

/// Creates the windowed surface from a window's raw display+window handle pair, via
/// `ash-window`. The offscreen host creates no surface, so this is the only surface
/// path.
fn create_window_surface(
    entry: &ash::Entry,
    instance: &ash::Instance,
    window: &dyn WindowSurface,
) -> Result<vk::SurfaceKHR> {
    let display = window
        .display_handle()
        .map_err(|err| Error::NoSurfaceHandle(err.to_string()))?;
    let handle = window
        .window_handle()
        .map_err(|err| Error::NoSurfaceHandle(err.to_string()))?;
    // SAFETY: the ash seam. The display/window handles are valid for the call (the
    // window outlives the device per the host's Drop order); the returned surface is
    // destroyed in `Device::drop`.
    unsafe { ash_window::create_surface(entry, instance, display.as_raw(), handle.as_raw(), None) }
        .map_err(|result| Error::Vk {
            context: "create_surface",
            result,
        })
}

/// The outcome of physical-device selection: the device, its queue family, the
/// probed optional capabilities, and the device-type preference rank used to pick
/// it among the qualifying candidates.
struct DeviceSelection {
    physical_device: vk::PhysicalDevice,
    graphics_queue_family: u32,
    compute_queue: Option<AsyncComputeQueue>,
    capabilities: Capabilities,
    preference: DevicePreference,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AsyncComputeQueue {
    family: u32,
    index: u32,
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
enum DevicePreference {
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
    fn from_type(device_type: vk::PhysicalDeviceType) -> Self {
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
fn select_physical_device(
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
fn evaluate_device(
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

fn choose_async_compute_queue(
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
fn find_graphics_queue_family(
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

/// Probes the optional features that never gate selection but tune the renderer.
fn probe_optional_features(
    instance: &ash::Instance,
    physical_device: vk::PhysicalDevice,
    props: &vk::PhysicalDeviceProperties,
    name: &str,
) -> Capabilities {
    // SAFETY: the ash seam. Core feature + extension queries on the device.
    let core_features = unsafe { instance.get_physical_device_features(physical_device) };
    let extensions = unsafe { instance.enumerate_device_extension_properties(physical_device) }
        .unwrap_or_default();
    let has_ext = |needle: &CStr| {
        extensions.iter().any(|ext| {
            ext.extension_name_as_c_str()
                .map(|n| n == needle)
                .unwrap_or(false)
        })
    };

    let has_as = has_ext(ash::khr::acceleration_structure::NAME);
    let has_rq = has_ext(ash::khr::ray_query::NAME);
    let has_deferred = has_ext(ash::khr::deferred_host_operations::NAME);
    let (rt_supported, acceleration_structure_indirect_build) = if has_as && has_rq && has_deferred
    {
        let mut as_feat = vk::PhysicalDeviceAccelerationStructureFeaturesKHR::default();
        let mut rq_feat = vk::PhysicalDeviceRayQueryFeaturesKHR::default();
        let mut feat2 = vk::PhysicalDeviceFeatures2::default()
            .push_next(&mut as_feat)
            .push_next(&mut rq_feat);
        // SAFETY: the ash seam. Fills the chained RT feature structs.
        unsafe { instance.get_physical_device_features2(physical_device, &mut feat2) };
        resolve_rt_capabilities(
            true,
            as_feat.acceleration_structure != 0,
            rq_feat.ray_query != 0,
            as_feat.acceleration_structure_indirect_build != 0,
        )
    } else {
        (false, false)
    };

    // The micromap extension is only meaningful when acceleration structures are also present,
    // so it inherits the RT gate rather than being probed independently.
    let omm_extension = rt_supported && has_ext(ash::ext::opacity_micromap::NAME);
    let opacity_micromap = if omm_extension {
        let mut omm_feat = vk::PhysicalDeviceOpacityMicromapFeaturesEXT::default();
        let mut omm_feat2 = vk::PhysicalDeviceFeatures2::default().push_next(&mut omm_feat);
        // SAFETY: the ash seam. Fills the chained micromap feature struct.
        unsafe { instance.get_physical_device_features2(physical_device, &mut omm_feat2) };
        omm_feat.micromap != 0
    } else {
        false
    };

    // Cluster acceleration structures inherit the RT gate the same way, plus the spec-revision
    // belt: the transcribed structs follow revision 4 of the header exactly, and a revision
    // bump can move layouts with no generator following it here.
    let cluster_spec = extensions
        .iter()
        .find(|ext| {
            ext.extension_name_as_c_str()
                .map(|n| n == crate::vk_nv_cluster::NAME)
                .unwrap_or(false)
        })
        .map(|ext| ext.spec_version);
    let cluster_extension =
        rt_supported && cluster_spec == Some(crate::vk_nv_cluster::SPEC_VERSION);
    let (cluster_acceleration_structure, cluster_as_limits) = if cluster_extension {
        let mut cluster_feat =
            crate::vk_nv_cluster::PhysicalDeviceClusterAccelerationStructureFeaturesNV::default();
        let mut cluster_feat2 = vk::PhysicalDeviceFeatures2 {
            p_next: (&raw mut cluster_feat).cast(),
            ..Default::default()
        };
        // SAFETY: the extension seam. The chained struct is the extension's feature query
        // shape at the probed spec revision, alive across the call.
        unsafe { instance.get_physical_device_features2(physical_device, &mut cluster_feat2) };
        if cluster_feat.cluster_acceleration_structure != 0 {
            let mut cluster_props =
                crate::vk_nv_cluster::PhysicalDeviceClusterAccelerationStructurePropertiesNV::default();
            let mut props2 = vk::PhysicalDeviceProperties2 {
                p_next: (&raw mut cluster_props).cast(),
                ..Default::default()
            };
            // SAFETY: the extension seam, same shape contract as the feature query.
            unsafe { instance.get_physical_device_properties2(physical_device, &mut props2) };
            (
                true,
                [
                    cluster_props.max_triangles_per_cluster,
                    cluster_props.max_vertices_per_cluster,
                    cluster_props.cluster_scratch_byte_alignment,
                    cluster_props.cluster_byte_alignment,
                    cluster_props.cluster_bottom_level_byte_alignment,
                ],
            )
        } else {
            (false, [0; 5])
        }
    } else {
        (false, [0; 5])
    };

    // Partitioned top-level structures inherit the same RT gate and spec-revision belt, and
    // one more: they are taken only when asked for.
    //
    // The extension executes correctly here — a partitioned structure renders a frame
    // byte-identical to the KHR one — but the SDK's validation layers do not model it: they
    // report a descriptor-type mismatch for a shader variable that has no partitioned SPIR-V
    // form to declare (the extension defines no SPIR-V capability), and cannot resolve the
    // structure's address to an acceleration-structure object because a partitioned structure
    // is memory rather than an object. Neither is fixable from here, and a default-on path
    // that cannot be validated is worse than an opt-in one that can. Remove the flag when the
    // layers catch up.
    let ptlas_spec = extensions
        .iter()
        .find(|ext| {
            ext.extension_name_as_c_str()
                .map(|n| n == crate::vk_nv_ptlas::NAME)
                .unwrap_or(false)
        })
        .map(|ext| ext.spec_version);
    let ptlas_extension = rt_supported
        && ptlas_spec == Some(crate::vk_nv_ptlas::SPEC_VERSION)
        && std::env::var_os("SAFFRON_PTLAS").is_some();
    let (partitioned_acceleration_structure, max_partition_count) = if ptlas_extension {
        let mut ptlas_feat =
            crate::vk_nv_ptlas::PhysicalDevicePartitionedAccelerationStructureFeaturesNV::default();
        let mut ptlas_feat2 = vk::PhysicalDeviceFeatures2 {
            p_next: (&raw mut ptlas_feat).cast(),
            ..Default::default()
        };
        // SAFETY: the extension seam. The chained struct is the extension's feature query
        // shape at the probed spec revision, alive across the call.
        unsafe { instance.get_physical_device_features2(physical_device, &mut ptlas_feat2) };
        if ptlas_feat.partitioned_acceleration_structure != 0 {
            let mut ptlas_props =
                crate::vk_nv_ptlas::PhysicalDevicePartitionedAccelerationStructurePropertiesNV::default();
            let mut props2 = vk::PhysicalDeviceProperties2 {
                p_next: (&raw mut ptlas_props).cast(),
                ..Default::default()
            };
            // SAFETY: the extension seam, same shape contract as the feature query.
            unsafe { instance.get_physical_device_properties2(physical_device, &mut props2) };
            (true, ptlas_props.max_partition_count)
        } else {
            (false, 0)
        }
    } else {
        (false, 0)
    };

    let mesh_extension = has_ext(ash::ext::mesh_shader::NAME);
    let mut features11 = vk::PhysicalDeviceVulkan11Features::default();
    let mut features12 = vk::PhysicalDeviceVulkan12Features::default();
    let mut features13 = vk::PhysicalDeviceVulkan13Features::default();
    let mut mesh_features = vk::PhysicalDeviceMeshShaderFeaturesEXT::default();
    let mut features2 = vk::PhysicalDeviceFeatures2::default()
        .push_next(&mut features11)
        .push_next(&mut features12)
        .push_next(&mut features13);
    if mesh_extension {
        features2 = features2.push_next(&mut mesh_features);
    }
    // SAFETY: the ash seam. Fills the chained core and advertised extension features.
    unsafe { instance.get_physical_device_features2(physical_device, &mut features2) };

    let mut descriptor_properties = vk::PhysicalDeviceDescriptorIndexingProperties::default();
    let mut subgroup_properties = vk::PhysicalDeviceSubgroupProperties::default();
    let mut subgroup_size_properties = vk::PhysicalDeviceSubgroupSizeControlProperties::default();
    let mut mesh_properties = vk::PhysicalDeviceMeshShaderPropertiesEXT::default();
    let mut properties2 = vk::PhysicalDeviceProperties2::default()
        .push_next(&mut descriptor_properties)
        .push_next(&mut subgroup_properties)
        .push_next(&mut subgroup_size_properties);
    if mesh_extension {
        properties2 = properties2.push_next(&mut mesh_properties);
    }
    // SAFETY: the ash seam. Fills the chained core and advertised extension properties.
    unsafe { instance.get_physical_device_properties2(physical_device, &mut properties2) };

    let lower = name.to_ascii_lowercase();
    let software_gpu = lower.contains("llvmpipe")
        || lower.contains("lavapipe")
        || lower.contains("swiftshader")
        || lower.contains("software")
        || props.device_type == vk::PhysicalDeviceType::CPU;

    resolve_capabilities(
        props,
        &core_features,
        &features11,
        &features12,
        &features13,
        &descriptor_properties,
        &subgroup_properties,
        &subgroup_size_properties,
        mesh_extension.then_some((&mesh_features, &mesh_properties)),
        rt_supported,
        opacity_micromap,
        cluster_acceleration_structure,
        cluster_as_limits,
        partitioned_acceleration_structure,
        max_partition_count,
        acceleration_structure_indirect_build,
        has_ext(ash::ext::memory_budget::NAME),
        software_gpu,
    )
}

#[allow(clippy::too_many_arguments)]
fn resolve_capabilities(
    props: &vk::PhysicalDeviceProperties,
    core_features: &vk::PhysicalDeviceFeatures,
    features11: &vk::PhysicalDeviceVulkan11Features<'_>,
    features12: &vk::PhysicalDeviceVulkan12Features<'_>,
    features13: &vk::PhysicalDeviceVulkan13Features<'_>,
    descriptor_properties: &vk::PhysicalDeviceDescriptorIndexingProperties<'_>,
    subgroup_properties: &vk::PhysicalDeviceSubgroupProperties<'_>,
    subgroup_size_properties: &vk::PhysicalDeviceSubgroupSizeControlProperties<'_>,
    mesh: Option<(
        &vk::PhysicalDeviceMeshShaderFeaturesEXT<'_>,
        &vk::PhysicalDeviceMeshShaderPropertiesEXT<'_>,
    )>,
    rt_supported: bool,
    opacity_micromap: bool,
    cluster_acceleration_structure: bool,
    cluster_as_limits: [u32; 5],
    partitioned_acceleration_structure: bool,
    max_partition_count: u32,
    acceleration_structure_indirect_build: bool,
    memory_budget: bool,
    software_gpu: bool,
) -> Capabilities {
    let (mesh_features, mesh_properties) = mesh.unzip();
    let max_bindless_array_elements = [
        descriptor_properties.max_update_after_bind_descriptors_in_all_pools / 5,
        descriptor_properties.max_per_stage_descriptor_update_after_bind_samplers / 4,
        descriptor_properties.max_per_stage_descriptor_update_after_bind_sampled_images / 5,
        descriptor_properties.max_per_stage_update_after_bind_resources / 5,
        descriptor_properties.max_descriptor_set_update_after_bind_samplers / 4,
        descriptor_properties.max_descriptor_set_update_after_bind_sampled_images / 5,
    ]
    .into_iter()
    .min()
    .unwrap_or(0);
    Capabilities {
        rt_supported,
        opacity_micromap,
        // Filled from `VkPhysicalDeviceOpacityMicromapPropertiesEXT` once the device exists;
        // the selection pass only decides whether the extension is enabled at all.
        omm_max_subdivision: 0,
        cluster_acceleration_structure,
        cluster_as_limits,
        partitioned_acceleration_structure,
        max_partition_count,
        mesh_shader: mesh_features.is_some_and(|features| features.mesh_shader != 0),
        task_shader: mesh_features.is_some_and(|features| features.task_shader != 0),
        max_mesh_work_group_count: mesh_properties
            .map_or([0; 3], |properties| properties.max_mesh_work_group_count),
        max_mesh_work_group_invocations: mesh_properties
            .map_or(0, |properties| properties.max_mesh_work_group_invocations),
        max_mesh_output_vertices: mesh_properties
            .map_or(0, |properties| properties.max_mesh_output_vertices),
        max_mesh_output_primitives: mesh_properties
            .map_or(0, |properties| properties.max_mesh_output_primitives),
        max_task_work_group_count: mesh_properties
            .map_or([0; 3], |properties| properties.max_task_work_group_count),
        max_task_work_group_invocations: mesh_properties
            .map_or(0, |properties| properties.max_task_work_group_invocations),
        max_task_payload_size: mesh_properties
            .map_or(0, |properties| properties.max_task_payload_size),
        fill_mode_non_solid: core_features.fill_mode_non_solid != 0,
        memory_budget,
        pipeline_stats: core_features.pipeline_statistics_query != 0,
        software_gpu,
        capture_supported: false,
        max_anisotropy: if core_features.sampler_anisotropy != 0 {
            props.limits.max_sampler_anisotropy.min(16.0)
        } else {
            1.0
        },
        multi_draw_indirect: core_features.multi_draw_indirect != 0,
        draw_indirect_count: {
            let supported = features12.draw_indirect_count != 0;
            tracing::info!(
                "gpu-driven draws: drawIndirectCount {} (maxDrawIndirectCount {})",
                if supported {
                    "supported"
                } else {
                    "UNSUPPORTED — fixed-slice draws"
                },
                props.limits.max_draw_indirect_count
            );
            supported
        },
        max_draw_indirect_count: props.limits.max_draw_indirect_count,
        buffer_device_address: features12.buffer_device_address != 0,
        shader_draw_parameters: features11.shader_draw_parameters != 0,
        runtime_descriptor_array: features12.runtime_descriptor_array != 0,
        descriptor_binding_partially_bound: features12.descriptor_binding_partially_bound != 0,
        descriptor_binding_sampled_image_update_after_bind: features12
            .descriptor_binding_sampled_image_update_after_bind
            != 0,
        shader_sampled_image_array_non_uniform_indexing: features12
            .shader_sampled_image_array_non_uniform_indexing
            != 0,
        max_update_after_bind_descriptors_in_all_pools: descriptor_properties
            .max_update_after_bind_descriptors_in_all_pools,
        max_per_stage_descriptor_update_after_bind_sampled_images: descriptor_properties
            .max_per_stage_descriptor_update_after_bind_sampled_images,
        max_descriptor_set_update_after_bind_sampled_images: descriptor_properties
            .max_descriptor_set_update_after_bind_sampled_images,
        max_per_stage_descriptor_update_after_bind_samplers: descriptor_properties
            .max_per_stage_descriptor_update_after_bind_samplers,
        max_descriptor_set_update_after_bind_samplers: descriptor_properties
            .max_descriptor_set_update_after_bind_samplers,
        max_per_stage_update_after_bind_resources: descriptor_properties
            .max_per_stage_update_after_bind_resources,
        max_bindless_array_elements,
        subgroup_size: subgroup_properties.subgroup_size,
        subgroup_supported_stages: subgroup_properties.supported_stages,
        subgroup_supported_operations: subgroup_properties.supported_operations,
        subgroup_quad_operations_in_all_stages: subgroup_properties.quad_operations_in_all_stages
            != 0,
        subgroup_size_control: features13.subgroup_size_control != 0,
        compute_full_subgroups: features13.compute_full_subgroups != 0,
        min_subgroup_size: subgroup_size_properties.min_subgroup_size,
        max_subgroup_size: subgroup_size_properties.max_subgroup_size,
        max_compute_workgroup_subgroups: subgroup_size_properties.max_compute_workgroup_subgroups,
        required_subgroup_size_stages: subgroup_size_properties.required_subgroup_size_stages,
        acceleration_structure_indirect_build,
        min_uniform_buffer_offset_alignment: props.limits.min_uniform_buffer_offset_alignment,
    }
}

/// Creates the logical device with the required feature chain and (when present)
/// the RT extensions enabled.
///
/// `enable_swapchain` gates `VK_KHR_swapchain`: the windowed host presents through a
/// swapchain and enables it, while the offscreen host never presents and enables no
/// surface extension at instance level, so it must not enable the swapchain device
/// extension either (`VK_KHR_swapchain` requires the instance-level `VK_KHR_surface`,
/// and enabling it without that fails `VUID-vkCreateDevice-ppEnabledExtensionNames-01387`).
/// Returns the device and whether `VK_EXT_calibrated_timestamps` was enabled on it
/// (the caller resolves its dispatch + domain check from that flag).
fn create_logical_device(
    instance: &ash::Instance,
    physical_device: vk::PhysicalDevice,
    graphics_queue_family: u32,
    compute_queue: Option<AsyncComputeQueue>,
    capabilities: &Capabilities,
    enable_swapchain: bool,
) -> Result<(ash::Device, bool, bool, bool)> {
    let single_queue_priority = [1.0_f32];
    let two_queue_priorities = [1.0_f32, 1.0_f32];
    let mut queue_infos = if compute_queue
        .is_some_and(|queue| queue.family == graphics_queue_family && queue.index == 1)
    {
        vec![
            vk::DeviceQueueCreateInfo::default()
                .queue_family_index(graphics_queue_family)
                .queue_priorities(&two_queue_priorities),
        ]
    } else {
        vec![
            vk::DeviceQueueCreateInfo::default()
                .queue_family_index(graphics_queue_family)
                .queue_priorities(&single_queue_priority),
        ]
    };
    if let Some(queue) = compute_queue
        && queue.family != graphics_queue_family
    {
        queue_infos.push(
            vk::DeviceQueueCreateInfo::default()
                .queue_family_index(queue.family)
                .queue_priorities(&single_queue_priority),
        );
    }

    // Extension enumeration is retained only for extensions whose activation is not a
    // renderer feature policy (portability and optional clock calibration).
    let extensions = unsafe { instance.enumerate_device_extension_properties(physical_device) }
        .map_err(|result| Error::Vk {
            context: "enumerate_device_extension_properties",
            result,
        })?;
    let has_ext = |needle: &CStr| {
        extensions.iter().any(|ext| {
            ext.extension_name_as_c_str()
                .map(|n| n == needle)
                .unwrap_or(false)
        })
    };
    let enable_rt = capabilities.rt_supported;
    let enable_mesh_extension = capabilities.mesh_shader || capabilities.task_shader;

    let mut device_extensions: Vec<*const c_char> = Vec::new();
    // The swapchain device extension requires the instance-level `VK_KHR_surface`,
    // which only the windowed host enables; the offscreen host presents nothing.
    if enable_swapchain {
        device_extensions.push(swapchain::NAME.as_ptr());
    }
    if enable_rt {
        device_extensions.push(ash::khr::acceleration_structure::NAME.as_ptr());
        device_extensions.push(ash::khr::ray_query::NAME.as_ptr());
        device_extensions.push(ash::khr::deferred_host_operations::NAME.as_ptr());
    }
    if capabilities.opacity_micromap {
        device_extensions.push(ash::ext::opacity_micromap::NAME.as_ptr());
    }
    if capabilities.cluster_acceleration_structure {
        device_extensions.push(crate::vk_nv_cluster::NAME.as_ptr());
    }
    if capabilities.partitioned_acceleration_structure {
        device_extensions.push(crate::vk_nv_ptlas::NAME.as_ptr());
    }
    if enable_mesh_extension {
        device_extensions.push(ash::ext::mesh_shader::NAME.as_ptr());
    }
    if capabilities.memory_budget {
        device_extensions.push(ash::ext::memory_budget::NAME.as_ptr());
    }
    // A portability physical device (MoltenVK) that advertises `VK_KHR_portability_subset` MUST
    // have it enabled at device creation (`VUID-VkDeviceCreateInfo-pProperties-04451`). It is
    // absent on native drivers, so the presence check keeps one code path across hosts.
    if has_ext(ash::khr::portability_subset::NAME) {
        device_extensions.push(ash::khr::portability_subset::NAME.as_ptr());
    }
    // VK_EXT_calibrated_timestamps lets the profiler project GPU spans onto the CPU clock.
    // The env var forces the own-axis fallback (testing it on hardware that supports it).
    let enable_calibrated_ts = has_ext(calibrated_timestamps::NAME)
        && std::env::var_os("SAFFRON_DISABLE_CALIBRATION").is_none();
    if enable_calibrated_ts {
        device_extensions.push(calibrated_timestamps::NAME.as_ptr());
    }
    // VK_NV_device_diagnostic_checkpoints: named per-pass progress markers so a device loss
    // reports which submission the GPU wedged in.
    let enable_checkpoints = has_ext(ash::nv::device_diagnostic_checkpoints::NAME);
    if enable_checkpoints {
        device_extensions.push(ash::nv::device_diagnostic_checkpoints::NAME.as_ptr());
    }
    // VK_EXT_device_fault: the driver's post-loss fault report (kind + faulting addresses).
    let mut fault_query = vk::PhysicalDeviceFaultFeaturesEXT::default();
    if has_ext(ash::ext::device_fault::NAME) {
        let mut features2 = vk::PhysicalDeviceFeatures2::default().push_next(&mut fault_query);
        // SAFETY: the ash seam. Fills the chained fault feature struct.
        unsafe { instance.get_physical_device_features2(physical_device, &mut features2) };
    }
    let enable_device_fault = fault_query.device_fault != 0;
    if enable_device_fault {
        device_extensions.push(ash::ext::device_fault::NAME.as_ptr());
    }

    let mut enabled_core = vk::PhysicalDeviceFeatures::default().shader_int64(true);
    if capabilities.pipeline_stats {
        enabled_core = enabled_core.pipeline_statistics_query(true);
    }
    if capabilities.fill_mode_non_solid {
        enabled_core = enabled_core.fill_mode_non_solid(true);
    }
    // Anisotropic filtering for the material sampler: the correct minification filter for
    // high-frequency albedo/AO textures at grazing angles, so they stay band-limited
    // rather than aliasing into the in-motion shimmer TAA would otherwise have to hide.
    if capabilities.max_anisotropy > 1.0 {
        enabled_core = enabled_core.sampler_anisotropy(true);
    }
    // `multiDrawIndirect` lets one indirect command issue more than one draw.
    if capabilities.multi_draw_indirect {
        enabled_core = enabled_core.multi_draw_indirect(true);
    }
    let mut features11 = vk::PhysicalDeviceVulkan11Features::default()
        .shader_draw_parameters(capabilities.shader_draw_parameters);
    let mut features12 = vk::PhysicalDeviceVulkan12Features::default()
        .runtime_descriptor_array(capabilities.runtime_descriptor_array)
        .descriptor_binding_partially_bound(capabilities.descriptor_binding_partially_bound)
        .descriptor_binding_sampled_image_update_after_bind(
            capabilities.descriptor_binding_sampled_image_update_after_bind,
        )
        .shader_sampled_image_array_non_uniform_indexing(
            capabilities.shader_sampled_image_array_non_uniform_indexing,
        )
        .timeline_semaphore(true)
        .buffer_device_address(capabilities.buffer_device_address)
        .draw_indirect_count(capabilities.draw_indirect_count);
    let mut features13 = vk::PhysicalDeviceVulkan13Features::default()
        .dynamic_rendering(true)
        .synchronization2(true)
        .subgroup_size_control(capabilities.subgroup_size_control)
        .compute_full_subgroups(capabilities.compute_full_subgroups);
    let mut as_feat = vk::PhysicalDeviceAccelerationStructureFeaturesKHR::default()
        .acceleration_structure(enable_rt)
        .acceleration_structure_indirect_build(capabilities.acceleration_structure_indirect_build);
    let mut rq_feat = vk::PhysicalDeviceRayQueryFeaturesKHR::default().ray_query(enable_rt);
    let mut omm_feat = vk::PhysicalDeviceOpacityMicromapFeaturesEXT::default()
        .micromap(capabilities.opacity_micromap);
    let mut ms_feat = vk::PhysicalDeviceMeshShaderFeaturesEXT::default()
        .mesh_shader(capabilities.mesh_shader)
        .task_shader(capabilities.task_shader);
    let mut fault_feat = vk::PhysicalDeviceFaultFeaturesEXT::default().device_fault(true);
    let mut cluster_feat =
        crate::vk_nv_cluster::PhysicalDeviceClusterAccelerationStructureFeaturesNV {
            cluster_acceleration_structure: vk::TRUE,
            ..Default::default()
        };
    let mut ptlas_feat =
        crate::vk_nv_ptlas::PhysicalDevicePartitionedAccelerationStructureFeaturesNV {
            partitioned_acceleration_structure: vk::TRUE,
            ..Default::default()
        };

    let mut create_info = vk::DeviceCreateInfo::default()
        .queue_create_infos(&queue_infos)
        .enabled_extension_names(&device_extensions)
        .enabled_features(&enabled_core)
        .push_next(&mut features11)
        .push_next(&mut features12)
        .push_next(&mut features13);
    if enable_rt {
        create_info = create_info.push_next(&mut as_feat).push_next(&mut rq_feat);
    }
    if capabilities.opacity_micromap {
        create_info = create_info.push_next(&mut omm_feat);
    }
    if enable_mesh_extension {
        create_info = create_info.push_next(&mut ms_feat);
    }
    if enable_device_fault {
        create_info = create_info.push_next(&mut fault_feat);
    }
    // The transcribed feature structs cannot ride ash's typed `push_next`, so each heads the
    // chain by hand: it points at whatever was built so far, and the create info points at it.
    if capabilities.cluster_acceleration_structure {
        cluster_feat.p_next = create_info.p_next.cast_mut();
        create_info.p_next = (&raw const cluster_feat).cast();
    }
    if capabilities.partitioned_acceleration_structure {
        ptlas_feat.p_next = create_info.p_next.cast_mut();
        create_info.p_next = (&raw const ptlas_feat).cast();
    }

    // SAFETY: the ash seam. The feature chain + extension pointers outlive the
    // call; the returned device is owned and destroyed in `Device::drop`.
    let device = unsafe { instance.create_device(physical_device, &create_info, None) }.map_err(
        |result| Error::Vk {
            context: "create_device",
            result,
        },
    )?;
    Ok((
        device,
        enable_calibrated_ts,
        enable_checkpoints,
        enable_device_fault,
    ))
}

/// Creates the VMA allocator over the ash instance/device.
fn create_allocator(
    instance: &ash::Instance,
    device: &ash::Device,
    physical_device: vk::PhysicalDevice,
) -> Result<vk_mem::Allocator> {
    let mut create_info = vk_mem::AllocatorCreateInfo::new(instance, device, physical_device);
    create_info.vulkan_api_version = API_VERSION;
    // The required feature set enables bufferDeviceAddress, which AS builds need —
    // and VMA must know about it to size BDA-flagged allocations.
    create_info.flags = vk_mem::AllocatorCreateFlags::BUFFER_DEVICE_ADDRESS;

    // SAFETY: the ash seam. The instance/device/physical-device handles are valid
    // for the allocator's whole lifetime (it is dropped before they are destroyed,
    // by the `Device` field order); the allocator captures ash's loaded function
    // pointers at creation.
    let allocator = unsafe { vk_mem::Allocator::new(create_info) }.map_err(|result| Error::Vk {
        context: "vmaCreateAllocator",
        result,
    })?;
    Ok(allocator)
}

/// The desired swapchain / offscreen surface format: `B8G8R8A8_UNORM` with the
/// sRGB-nonlinear color space. Used directly by the
/// offscreen host (which never presents) and preferred by the windowed host.
const PREFERRED_SURFACE_FORMAT: vk::SurfaceFormatKHR = vk::SurfaceFormatKHR {
    format: vk::Format::B8G8R8A8_UNORM,
    color_space: vk::ColorSpaceKHR::SRGB_NONLINEAR,
};

/// Picks the swapchain surface format: prefer [`PREFERRED_SURFACE_FORMAT`], else the
/// first advertised format. Windowed host only — the offscreen host uses the
/// preferred format directly since it has no surface to query.
fn choose_surface_format(
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
fn surface_capture_supported(
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
fn log_selected_device(instance: &ash::Instance, physical_device: vk::PhysicalDevice) {
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
fn log_software_gpu(capabilities: &Capabilities) {
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
fn attachment_usage(format: vk::Format) -> vk::ImageUsageFlags {
    match format {
        vk::Format::D32_SFLOAT
        | vk::Format::D24_UNORM_S8_UINT
        | vk::Format::D32_SFLOAT_S8_UINT
        | vk::Format::D16_UNORM => vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT,
        _ => vk::ImageUsageFlags::COLOR_ATTACHMENT,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The device-type preference order is discrete > integrated > virtual >
    /// cpu/other. This is the ranking
    /// `select_physical_device` uses to prefer a discrete GPU over the software
    /// rasterizer when both qualify (the NVIDIA ICD added next to llvmpipe).
    #[test]
    fn device_preference_ranks_discrete_above_software() {
        use vk::PhysicalDeviceType as T;
        assert_eq!(
            DevicePreference::from_type(T::DISCRETE_GPU),
            DevicePreference::Discrete
        );
        assert_eq!(
            DevicePreference::from_type(T::INTEGRATED_GPU),
            DevicePreference::Integrated
        );
        assert_eq!(
            DevicePreference::from_type(T::VIRTUAL_GPU),
            DevicePreference::Virtual
        );
        assert_eq!(DevicePreference::from_type(T::CPU), DevicePreference::Cpu);
        // `OTHER` (and any unknown type) is the last resort, same rank as CPU.
        assert_eq!(DevicePreference::from_type(T::OTHER), DevicePreference::Cpu);

        // The ordering is what `select_physical_device`'s `selection.preference >
        // current.preference` comparison relies on: a discrete GPU outranks every
        // softer type, and a CPU rasterizer is never preferred over a real GPU.
        assert!(DevicePreference::Discrete > DevicePreference::Integrated);
        assert!(DevicePreference::Integrated > DevicePreference::Virtual);
        assert!(DevicePreference::Virtual > DevicePreference::Cpu);
        assert!(DevicePreference::Discrete > DevicePreference::Cpu);
    }

    /// The feature-probe chain creates an offscreen device regardless of which optional features
    /// are present. Linux may select llvmpipe or a host GPU, while macOS selects MoltenVK; none of
    /// those choices changes the optional-feature invariants. Skips when no device is obtainable.
    #[test]
    fn offscreen_device_probe_does_not_gate_selection() {
        let device = match Device::new(&SurfaceSource::Offscreen) {
            Ok(device) => device,
            Err(err) => {
                eprintln!("skipping: no Vulkan device obtainable ({err})");
                return;
            }
        };

        // RT and software classification are probed properties, not selection gates. The offscreen
        // device carries no surface, and an idle wait returns cleanly on every backend.
        assert!(
            device.surface().is_none(),
            "the offscreen device creates no surface"
        );
        // The indirect-build capability is meaningful only with acceleration structures.
        assert!(
            !device.capabilities.acceleration_structure_indirect_build
                || device.capabilities.rt_supported,
            "accel-struct indirect build is never reported without RT support"
        );
        device.wait_idle().expect("an idle device waits cleanly");
    }

    /// The three indirect-capability fields default off and never gate selection — a device lacking
    /// them (llvmpipe reports them however its driver does) is still selected and used.
    #[test]
    fn indirect_capability_fields_default_off() {
        let caps = Capabilities::default();
        assert!(!caps.multi_draw_indirect);
        assert!(!caps.draw_indirect_count);
        assert!(!caps.acceleration_structure_indirect_build);
    }

    #[test]
    fn capability_resolution_keeps_independent_feature_bits_and_limits() {
        let mut props = vk::PhysicalDeviceProperties::default();
        props.limits.max_draw_indirect_count = 73;
        props.limits.min_uniform_buffer_offset_alignment = 256;
        let core = vk::PhysicalDeviceFeatures::default().multi_draw_indirect(true);
        let features11 = vk::PhysicalDeviceVulkan11Features::default().shader_draw_parameters(true);
        let features12 = vk::PhysicalDeviceVulkan12Features::default()
            .buffer_device_address(true)
            .draw_indirect_count(true)
            .runtime_descriptor_array(true)
            .descriptor_binding_partially_bound(true)
            .descriptor_binding_sampled_image_update_after_bind(true)
            .shader_sampled_image_array_non_uniform_indexing(true);
        let features13 = vk::PhysicalDeviceVulkan13Features::default()
            .subgroup_size_control(true)
            .compute_full_subgroups(true);
        let descriptor = vk::PhysicalDeviceDescriptorIndexingProperties::default()
            .max_update_after_bind_descriptors_in_all_pools(50_000)
            .max_per_stage_descriptor_update_after_bind_samplers(4_096)
            .max_per_stage_descriptor_update_after_bind_sampled_images(8_192)
            .max_per_stage_update_after_bind_resources(2_048)
            .max_descriptor_set_update_after_bind_samplers(12_000)
            .max_descriptor_set_update_after_bind_sampled_images(16_384);
        let subgroup = vk::PhysicalDeviceSubgroupProperties::default()
            .subgroup_size(32)
            .supported_stages(vk::ShaderStageFlags::COMPUTE | vk::ShaderStageFlags::MESH_EXT)
            .supported_operations(
                vk::SubgroupFeatureFlags::BASIC | vk::SubgroupFeatureFlags::BALLOT,
            )
            .quad_operations_in_all_stages(true);
        let subgroup_size = vk::PhysicalDeviceSubgroupSizeControlProperties::default()
            .min_subgroup_size(16)
            .max_subgroup_size(64)
            .max_compute_workgroup_subgroups(8)
            .required_subgroup_size_stages(vk::ShaderStageFlags::COMPUTE);
        let mesh_features = vk::PhysicalDeviceMeshShaderFeaturesEXT::default()
            .mesh_shader(true)
            .task_shader(false);
        let mesh_properties = vk::PhysicalDeviceMeshShaderPropertiesEXT::default()
            .max_mesh_work_group_count([11, 12, 13])
            .max_mesh_work_group_invocations(128)
            .max_mesh_output_vertices(256)
            .max_mesh_output_primitives(128)
            .max_task_work_group_count([21, 22, 23])
            .max_task_work_group_invocations(64)
            .max_task_payload_size(4_096);

        let capabilities = resolve_capabilities(
            &props,
            &core,
            &features11,
            &features12,
            &features13,
            &descriptor,
            &subgroup,
            &subgroup_size,
            Some((&mesh_features, &mesh_properties)),
            true,
            true,
            true,
            [128, 256, 64, 128, 256],
            true,
            8_192,
            true,
            true,
            false,
        );

        assert!(capabilities.mesh_shader);
        assert!(capabilities.opacity_micromap);
        assert!(capabilities.cluster_acceleration_structure);
        assert_eq!(capabilities.cluster_as_limits, [128, 256, 64, 128, 256]);
        assert!(capabilities.partitioned_acceleration_structure);
        assert_eq!(capabilities.max_partition_count, 8_192);
        assert!(!capabilities.task_shader);
        assert_eq!(capabilities.max_mesh_work_group_count, [11, 12, 13]);
        assert_eq!(capabilities.max_task_payload_size, 4_096);
        assert!(capabilities.buffer_device_address);
        assert!(capabilities.shader_draw_parameters);
        assert!(capabilities.runtime_descriptor_array);
        assert_eq!(capabilities.max_draw_indirect_count, 73);
        assert_eq!(capabilities.subgroup_size, 32);
        assert!(capabilities.subgroup_size_control);
        assert!(capabilities.compute_full_subgroups);
        assert_eq!(capabilities.min_subgroup_size, 16);
        assert_eq!(capabilities.max_subgroup_size, 64);
        assert_eq!(capabilities.max_bindless_array_elements, 409);
        assert_eq!(
            capabilities.max_per_stage_descriptor_update_after_bind_samplers,
            4_096
        );
        assert_eq!(
            capabilities.max_descriptor_set_update_after_bind_samplers,
            12_000
        );
        assert_eq!(
            capabilities.max_per_stage_update_after_bind_resources,
            2_048
        );
        assert_eq!(
            capabilities.max_descriptor_set_update_after_bind_sampled_images,
            16_384
        );
    }

    #[test]
    fn profiler_normalizes_queue_timestamp_widths_and_disables_zero_bit_compute() {
        let (common, graphics, compute) = normalized_timestamp_masks(64, Some(32));
        assert_eq!(common, u64::from(u32::MAX));
        assert_eq!(graphics, u64::MAX);
        assert_eq!(compute, Some(u64::from(u32::MAX)));

        let (common, graphics, compute) = normalized_timestamp_masks(48, Some(0));
        assert_eq!(common, (1_u64 << 48) - 1);
        assert_eq!(graphics, common);
        assert_eq!(compute, None);
    }

    #[test]
    fn rt_resolution_requires_extensions_and_both_feature_bits() {
        assert_eq!(
            resolve_rt_capabilities(true, true, false, true),
            (false, false)
        );
        assert_eq!(
            resolve_rt_capabilities(true, false, true, true),
            (false, false)
        );
        assert_eq!(
            resolve_rt_capabilities(false, true, true, true),
            (false, false)
        );
        assert_eq!(
            resolve_rt_capabilities(true, true, true, true),
            (true, true)
        );
    }

    fn queue_family(
        flags: vk::QueueFlags,
        queue_count: u32,
        timestamp_valid_bits: u32,
    ) -> vk::QueueFamilyProperties {
        vk::QueueFamilyProperties {
            queue_flags: flags,
            queue_count,
            timestamp_valid_bits,
            ..Default::default()
        }
    }

    #[test]
    fn async_compute_prefers_a_dedicated_family() {
        let families = [
            queue_family(vk::QueueFlags::GRAPHICS | vk::QueueFlags::COMPUTE, 1, 64),
            queue_family(vk::QueueFlags::COMPUTE | vk::QueueFlags::TRANSFER, 4, 64),
            queue_family(vk::QueueFlags::COMPUTE, 1, 64),
        ];
        assert_eq!(
            choose_async_compute_queue(&families, 0),
            Some(AsyncComputeQueue {
                family: 2,
                index: 0,
            })
        );
    }

    #[test]
    fn async_compute_uses_a_distinct_mixed_family_when_needed() {
        let families = [
            queue_family(vk::QueueFlags::GRAPHICS | vk::QueueFlags::COMPUTE, 1, 64),
            queue_family(vk::QueueFlags::COMPUTE | vk::QueueFlags::TRANSFER, 1, 64),
        ];
        assert_eq!(
            choose_async_compute_queue(&families, 0),
            Some(AsyncComputeQueue {
                family: 1,
                index: 0,
            })
        );
    }

    #[test]
    fn async_compute_falls_back_when_no_compatible_family_exists() {
        let graphics = queue_family(vk::QueueFlags::GRAPHICS | vk::QueueFlags::COMPUTE, 1, 64);
        assert_eq!(choose_async_compute_queue(&[graphics], 0), None);

        let different_timestamp_width = [graphics, queue_family(vk::QueueFlags::COMPUTE, 1, 32)];
        assert_eq!(
            choose_async_compute_queue(&different_timestamp_width, 0),
            Some(AsyncComputeQueue {
                family: 1,
                index: 0,
            })
        );
    }

    #[test]
    fn async_compute_prefers_a_second_graphics_family_queue_over_a_mixed_family() {
        let families = [
            queue_family(vk::QueueFlags::GRAPHICS | vk::QueueFlags::COMPUTE, 2, 64),
            queue_family(vk::QueueFlags::GRAPHICS | vk::QueueFlags::COMPUTE, 1, 64),
        ];
        assert_eq!(
            choose_async_compute_queue(&families, 0),
            Some(AsyncComputeQueue {
                family: 0,
                index: 1,
            })
        );
    }
}
