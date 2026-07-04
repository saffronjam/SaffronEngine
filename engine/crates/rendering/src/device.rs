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
use crate::{Error, Result, checked};

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

/// Resolved optional-feature flags, probed once at device creation.
///
/// Optional features never gate device selection — a software (llvmpipe) device
/// reports `rt_supported == false` and is created and used regardless, the
/// degradation the unit test asserts.
#[derive(Debug, Clone, Copy, Default)]
pub struct Capabilities {
    /// KHR acceleration-structure + ray-query present and enabled.
    pub rt_supported: bool,
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
}

/// The GPU-timestamp profiler facts read once from the physical device at init,
/// used to seed [`crate::GpuProfiler`].
#[derive(Debug, Clone, Default)]
pub struct ProfilerFacts {
    /// ns per timestamp tick (the device limit).
    pub timestamp_period: f32,
    /// The graphics-queue `timestampValidBits` mask.
    pub timestamp_mask: u64,
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
    /// The graphics queue (externally synchronized; the README §5 site that the
    /// thumbnail worker will share behind a mutex in a later phase).
    pub graphics_queue: vk::Queue,
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

impl Device {
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
        // SAFETY: the ash seam. `Entry::load` dynamically loads `libvulkan`; the
        // returned entry is held for the whole `Device` lifetime (it owns the
        // loader the instance/device dispatch through).
        let entry = unsafe { ash::Entry::load() }.map_err(|err| Error::Loader(err.to_string()))?;

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
        log_selected_device(&instance, physical_device);

        let (device, calibrated_ts_enabled) = create_logical_device(
            &instance,
            physical_device,
            graphics_queue_family,
            require_present,
        )?;
        // SAFETY: the family/index pair was just used to create the device with one
        // queue at index 0 of that family.
        let graphics_queue = unsafe { device.get_device_queue(graphics_queue_family, 0) };

        let allocator = create_allocator(&instance, &device, physical_device)?;
        let swapchain_loader = swapchain::Device::new(&instance, &device);
        // Resolve the acceleration-structure dispatch only when the RT extensions were
        // enabled on the device.
        let accel = if selection.capabilities.rt_supported {
            Some(accel::Device::new(&instance, &device))
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
        let resources = DeviceResources::new(device, allocator);

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
            ..selection.capabilities
        };
        log_software_gpu(&capabilities);

        Ok(Self {
            capabilities,
            graphics_queue_family,
            graphics_queue,
            surface_format,
            resources: Some(resources),
            swapchain_loader,
            accel,
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

    /// The GPU-timestamp profiler facts read once at init: the ns-per-tick period,
    /// the graphics-queue `timestampValidBits` mask, whether timestamps are usable,
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
        let valid_bits = families
            .get(self.graphics_queue_family as usize)
            .map_or(0, |f| f.timestamp_valid_bits);
        let timestamp_mask = if valid_bits >= 64 {
            u64::MAX
        } else {
            (1u64 << valid_bits) - 1
        };
        let device_name = props
            .device_name_as_c_str()
            .ok()
            .and_then(|name| name.to_str().ok())
            .unwrap_or("")
            .to_owned();
        ProfilerFacts {
            timestamp_period: props.limits.timestamp_period,
            timestamp_mask,
            timestamps_supported: valid_bits != 0,
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
        // SAFETY: the ash seam. The device handle is valid for the call.
        checked(
            unsafe { self.bundle().device().device_wait_idle() },
            "device_wait_idle",
        )
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

    let create_info = vk::InstanceCreateInfo::default()
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
    capabilities: Capabilities,
    preference: DevicePreference,
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
/// `bufferDeviceAddress`, `dynamicRendering`, `synchronization2`) gate selection;
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

    let mut features12 = vk::PhysicalDeviceVulkan12Features::default();
    let mut features13 = vk::PhysicalDeviceVulkan13Features::default();
    let mut features2 = vk::PhysicalDeviceFeatures2::default()
        .push_next(&mut features12)
        .push_next(&mut features13);
    // SAFETY: the ash seam. Fills the chained feature structs for this device.
    unsafe { instance.get_physical_device_features2(physical_device, &mut features2) };

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
    if features13.dynamic_rendering == 0 || features13.synchronization2 == 0 {
        return Err(format!(
            "{name}: missing dynamic rendering / synchronization2"
        ));
    }

    let capabilities = probe_optional_features(instance, physical_device, &props, &name);
    Ok(DeviceSelection {
        physical_device,
        graphics_queue_family,
        capabilities,
        preference: DevicePreference::from_type(props.device_type),
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
    let rt_supported = if has_as && has_rq {
        let mut as_feat = vk::PhysicalDeviceAccelerationStructureFeaturesKHR::default();
        let mut rq_feat = vk::PhysicalDeviceRayQueryFeaturesKHR::default();
        let mut feat2 = vk::PhysicalDeviceFeatures2::default()
            .push_next(&mut as_feat)
            .push_next(&mut rq_feat);
        // SAFETY: the ash seam. Fills the chained RT feature structs.
        unsafe { instance.get_physical_device_features2(physical_device, &mut feat2) };
        as_feat.acceleration_structure != 0 && rq_feat.ray_query != 0
    } else {
        false
    };

    let lower = name.to_ascii_lowercase();
    let software_gpu = lower.contains("llvmpipe")
        || lower.contains("lavapipe")
        || lower.contains("swiftshader")
        || lower.contains("software")
        || props.device_type == vk::PhysicalDeviceType::CPU;

    Capabilities {
        rt_supported,
        fill_mode_non_solid: core_features.fill_mode_non_solid != 0,
        memory_budget: has_ext(ash::ext::memory_budget::NAME),
        pipeline_stats: core_features.pipeline_statistics_query != 0,
        software_gpu,
        capture_supported: false,
        max_anisotropy: if core_features.sampler_anisotropy != 0 {
            props.limits.max_sampler_anisotropy.min(16.0)
        } else {
            1.0
        },
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
    enable_swapchain: bool,
) -> Result<(ash::Device, bool)> {
    let queue_priorities = [1.0_f32];
    let queue_info = vk::DeviceQueueCreateInfo::default()
        .queue_family_index(graphics_queue_family)
        .queue_priorities(&queue_priorities);
    let queue_infos = [queue_info];

    // Re-probe RT extension presence to decide what to enable on the device. The
    // selection step proved the *required* set; this enables the *optional* RT set
    // only when the device advertises both extensions.
    // SAFETY: the ash seam. Extension query on the chosen device.
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
    let enable_rt =
        has_ext(ash::khr::acceleration_structure::NAME) && has_ext(ash::khr::ray_query::NAME);

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
    if has_ext(ash::ext::memory_budget::NAME) {
        device_extensions.push(ash::ext::memory_budget::NAME.as_ptr());
    }
    // VK_EXT_calibrated_timestamps lets the profiler project GPU spans onto the CPU clock.
    // The env var forces the own-axis fallback (testing it on hardware that supports it).
    let enable_calibrated_ts = has_ext(calibrated_timestamps::NAME)
        && std::env::var_os("SAFFRON_DISABLE_CALIBRATION").is_none();
    if enable_calibrated_ts {
        device_extensions.push(calibrated_timestamps::NAME.as_ptr());
    }

    // Enable the optional core features the renderer uses when the device advertises them:
    // `pipelineStatisticsQuery` (the deepest profiler level's input — creating a stats query
    // pool without it is `VUID-VkQueryPoolCreateInfo-queryType-00791`) and `fillModeNonSolid`
    // (the wireframe view mode's `PolygonMode::LINE`). Both are optional, never gating selection.
    // SAFETY: the ash seam. Core feature query on the chosen device.
    let core_features = unsafe { instance.get_physical_device_features(physical_device) };
    let mut enabled_core = vk::PhysicalDeviceFeatures::default();
    if core_features.pipeline_statistics_query != 0 {
        enabled_core = enabled_core.pipeline_statistics_query(true);
    }
    if core_features.fill_mode_non_solid != 0 {
        enabled_core = enabled_core.fill_mode_non_solid(true);
    }
    // Anisotropic filtering for the material sampler: the correct minification filter for
    // high-frequency albedo/AO textures at grazing angles, so they stay band-limited
    // rather than aliasing into the in-motion shimmer TAA would otherwise have to hide.
    if core_features.sampler_anisotropy != 0 {
        enabled_core = enabled_core.sampler_anisotropy(true);
    }

    // Slang's `SV_VertexID` fullscreen-triangle shaders (the sky / post passes) emit the
    // SPIR-V `DrawParameters` capability, so the device must enable `shaderDrawParameters`.
    let mut features11 = vk::PhysicalDeviceVulkan11Features::default().shader_draw_parameters(true);
    let mut features12 = vk::PhysicalDeviceVulkan12Features::default()
        .runtime_descriptor_array(true)
        .descriptor_binding_partially_bound(true)
        .descriptor_binding_sampled_image_update_after_bind(true)
        .shader_sampled_image_array_non_uniform_indexing(true)
        .buffer_device_address(true);
    let mut features13 = vk::PhysicalDeviceVulkan13Features::default()
        .dynamic_rendering(true)
        .synchronization2(true);
    let mut as_feat =
        vk::PhysicalDeviceAccelerationStructureFeaturesKHR::default().acceleration_structure(true);
    let mut rq_feat = vk::PhysicalDeviceRayQueryFeaturesKHR::default().ray_query(true);

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

    // SAFETY: the ash seam. The feature chain + extension pointers outlive the
    // call; the returned device is owned and destroyed in `Device::drop`.
    let device = unsafe { instance.create_device(physical_device, &create_info, None) }.map_err(
        |result| Error::Vk {
            context: "create_device",
            result,
        },
    )?;
    Ok((device, enable_calibrated_ts))
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

    /// The feature-probe chain degrades correctly on a software device: the device
    /// is created regardless of which optional features are present, and the
    /// optional-feature flags never gate selection. On the toolbox's llvmpipe the
    /// `software_gpu` flag is set; whether `rt_supported` is true or false (Mesa's
    /// lavapipe advertises ray-query, so it may be true), the device is still built
    /// and usable. Skips cleanly when no Vulkan device is obtainable.
    #[test]
    fn software_device_probe_does_not_gate_selection() {
        let device = match Device::new(&SurfaceSource::Offscreen) {
            Ok(device) => device,
            Err(err) => {
                eprintln!("skipping: no Vulkan device obtainable ({err})");
                return;
            }
        };

        // The offscreen toolbox device is the llvmpipe software rasterizer.
        assert!(
            device.capabilities.software_gpu,
            "the offscreen toolbox device is the llvmpipe software rasterizer"
        );
        // RT is optional: whatever its probed value, selection still succeeded — the
        // device is fully usable. The offscreen device carries no surface (it never
        // presents), and an idle wait returns cleanly. (`rt_supported` is intentionally
        // not asserted to a fixed value: lavapipe advertises ray-query, a hardware GPU
        // may differ.)
        assert!(
            device.surface().is_none(),
            "the offscreen device creates no surface"
        );
        device.wait_idle().expect("an idle device waits cleanly");
    }
}
