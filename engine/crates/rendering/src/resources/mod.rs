//! The move-only RAII GPU resource wrappers — `Buffer`, `Image`, `Image3D`, `GpuTexture`,
//! `GpuMesh`, `Pipeline`, `AccelerationStructure` — plus the [`DeviceResources`] bundle they hold.
//!
//! Every wrapper holds an [`Arc`]`<`[`DeviceResources`]`>` (the ash device + the VMA allocator), so
//! the device and allocator are destroyed only once the last resource drops — the outlives relation
//! is structural rather than field-order-dependent — and the wrappers are `Send`, which the
//! off-thread `GpuTexture` drop needs.

mod accel;
mod buffer;
mod image;
mod mesh;
mod texture;

use std::sync::{Arc, Mutex};

use ash::vk;
use saffron_geometry::glam::Vec3;
use saffron_geometry::{Submesh, Vertex, VertexSkin};
use vk_mem::{Alloc, Allocator};

pub use accel::*;
pub use buffer::*;
pub use image::*;
pub use mesh::*;
pub use texture::*;

/// The shared bindless texture free-list: returned slot indices a later upload
/// reuses. A [`GpuTexture`]'s `Drop` locks
/// it and pushes its slot back, even off the main thread. Every texture holds a
/// clone of this shared free-list.
pub type BindlessFreeList = Arc<Mutex<Vec<u32>>>;

/// The device + allocator handles a GPU resource needs to free itself, shared
/// behind one `Arc` so the resource can `Drop` without a live `&Device`.
///
/// Rust cannot encode "borrowed but the owner outlives me" for a `Drop` type, so the
/// two handles live behind this `Arc`: a resource clones the `Arc` at construction, and
/// the allocator/device are destroyed only when the last holder (the [`super::Device`]
/// itself, normally the final one after the run loop's `wait_idle` and resource
/// teardown) drops. The [`Drop`] frees the allocator before the device
/// (`vmaDestroyAllocator` before `vkDestroyDevice`).
pub struct DeviceResources {
    /// The VMA allocator. `Option` so [`Drop`] can free it before the device.
    allocator: Option<Allocator>,
    /// The ash logical device (a cheap handle + `Arc`'d fn table — its own clone is
    /// not used; this single owned copy is destroyed in [`Drop`]).
    device: ash::Device,
    /// `minAccelerationStructureScratchOffsetAlignment`, or 1 on a non-RT device.
    scratch_alignment: vk::DeviceSize,
    /// The diagnostic-checkpoint dispatch + marker registry; `None` when the device does not
    /// carry `VK_NV_device_diagnostic_checkpoints`. Lives in the bundle because both halves
    /// need it: the executor and the uploader mark, and the device-loss paths report.
    checkpoints: Option<crate::checkpoints::Checkpoints>,
    /// The `VK_EXT_device_fault` dispatch; `None` when the device does not carry the feature.
    device_fault: Option<crate::checkpoints::DeviceFault>,
    /// Cumulative GPU nanoseconds in out-of-graph acceleration-structure builds and compactions.
    ///
    /// Held here because the two halves live apart: the uploader records the spans, and the
    /// renderer reports the stats, and they share only this bundle. It is a session total rather
    /// than a per-frame figure because that is what the work is — structures are built when content
    /// arrives, not every frame.
    accel_build_ns: std::sync::atomic::AtomicU64,
    /// Cooked opacity micromaps meshes have offered at upload, and the micro-triangle states
    /// their derivation settled, summed over the session.
    ///
    /// Counted from the cooked hierarchy before any device gate, so it measures the derivation
    /// rather than the attachment: a target without `VK_EXT_opacity_micromap` reports the same
    /// figures as one with it, and the two together say whether a zero on the device side is an
    /// absent extension or an empty cook.
    omm_derived: [std::sync::atomic::AtomicU64; 4],
}

/// Device-local memory occupancy and headroom, in bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VramUsage {
    /// Bytes currently occupied across the device-local heaps.
    pub usage_bytes: u64,
    /// Bytes the driver says this process may occupy across those heaps. Smaller than the heap
    /// sizes whenever something else on the adapter holds memory.
    pub budget_bytes: u64,
}

/// Folds the per-heap `(usage, budget)` pairs `vmaGetHeapBudgets` reports, keeping only the heaps
/// flagged `DEVICE_LOCAL`.
///
/// A discrete adapter flags exactly its video memory, which is the figure a VRAM budget means. A
/// unified-memory adapter flags every heap, so the sum is system memory — and that is still the
/// pool the renderer competes for there, so no device class needs a second reading.
fn device_local_vram(
    heaps: &[vk::MemoryHeap],
    budgets: impl IntoIterator<Item = (u64, u64)>,
) -> VramUsage {
    heaps
        .iter()
        .zip(budgets)
        .filter(|(heap, _)| heap.flags.contains(vk::MemoryHeapFlags::DEVICE_LOCAL))
        .fold(VramUsage::default(), |total, (_, (usage, budget))| {
            VramUsage {
                usage_bytes: total.usage_bytes.saturating_add(usage),
                budget_bytes: total.budget_bytes.saturating_add(budget),
            }
        })
}

impl DeviceResources {
    /// Adds GPU nanoseconds to the out-of-graph structure-build total.
    pub fn add_accel_build_nanos(&self, nanos: u64) {
        self.accel_build_ns
            .fetch_add(nanos, std::sync::atomic::Ordering::Relaxed);
    }

    /// Cumulative GPU nanoseconds in out-of-graph structure builds and compactions.
    #[must_use]
    pub fn accel_build_nanos(&self) -> u64 {
        self.accel_build_ns
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Adds one cooked micromap and its settled micro-triangle states to the session totals.
    pub fn add_derived_micromap(&self, opaque: u64, transparent: u64, unknown: u64) {
        for (counter, value) in self
            .omm_derived
            .iter()
            .zip([1, opaque, transparent, unknown])
        {
            counter.fetch_add(value, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Session totals of cooked micromaps and their opaque / transparent / unknown
    /// micro-triangles.
    #[must_use]
    pub fn derived_micromaps(&self) -> (u64, u64, u64, u64) {
        let read =
            |index: usize| self.omm_derived[index].load(std::sync::atomic::Ordering::Relaxed);
        (read(0), read(1), read(2), read(3))
    }

    /// Bundles the allocator + device. Called once by [`super::Device::new`]; the
    /// returned `Arc` is the canonical holder cloned into every resource.
    pub(crate) fn new(
        device: ash::Device,
        allocator: Allocator,
        scratch_alignment: vk::DeviceSize,
        checkpoints: Option<crate::checkpoints::Checkpoints>,
        device_fault: Option<crate::checkpoints::DeviceFault>,
    ) -> Arc<Self> {
        Arc::new(Self {
            allocator: Some(allocator),
            device,
            scratch_alignment,
            checkpoints,
            device_fault,
            accel_build_ns: std::sync::atomic::AtomicU64::new(0),
            omm_derived: std::array::from_fn(|_| std::sync::atomic::AtomicU64::new(0)),
        })
    }

    /// This instant's device-local memory occupancy and headroom, summed over the heaps the
    /// device flags `DEVICE_LOCAL`.
    ///
    /// `vmaGetHeapBudgets` is a cached read rather than a walk of the allocations, so this is a
    /// per-frame call. With `VK_EXT_memory_budget` on the device the two figures are the
    /// driver's, so they cover memory this allocator never handed out — swapchain images,
    /// pipelines, descriptor heaps, and whatever else shares the adapter — which is the number a
    /// residency budget has to be held against.
    #[must_use]
    pub fn vram_usage(&self) -> VramUsage {
        let allocator = self.allocator();
        let Ok(budgets) = allocator.get_heap_budgets() else {
            return VramUsage::default();
        };
        // SAFETY: the VMA seam. Returns the memory properties VMA captured at creation and holds
        // for its whole lifetime; the borrow ends with this expression.
        let properties = unsafe { allocator.get_memory_properties() };
        let heaps = &properties.memory_heaps[..properties.memory_heap_count as usize];
        device_local_vram(
            heaps,
            budgets.iter().map(|budget| (budget.usage, budget.budget)),
        )
    }

    /// The diagnostic-checkpoint dispatch, when the extension is enabled on the device.
    pub(crate) fn checkpoints(&self) -> Option<&crate::checkpoints::Checkpoints> {
        self.checkpoints.as_ref()
    }

    /// Logs the driver's fault report after a device loss. A no-op without
    /// `VK_EXT_device_fault`.
    pub(crate) fn log_device_fault(&self) {
        let Some(device_fault) = self.device_fault.as_ref() else {
            return;
        };
        for line in device_fault.report() {
            tracing::error!("{line}");
        }
    }

    /// The device's acceleration-structure build-scratch alignment. Every scratch address
    /// handed to `vkCmdBuildAccelerationStructuresKHR` must be a multiple of it
    /// (VUID-vkCmdBuildAccelerationStructuresKHR-pInfos-03710); the driver reports 128 on
    /// current NVIDIA hardware, and a misaligned build loses the device.
    pub(crate) fn scratch_alignment(&self) -> vk::DeviceSize {
        self.scratch_alignment
    }

    /// The ash logical device (resource creation / view + handle teardown).
    pub(crate) fn device(&self) -> &ash::Device {
        &self.device
    }

    /// The VMA allocator (image/buffer create + destroy). Present for the whole
    /// bundle lifetime; only [`Drop`] takes it (to free it before the device).
    pub(crate) fn allocator(&self) -> &Allocator {
        self.allocator
            .as_ref()
            .expect("allocator lives until DeviceResources::drop")
    }

    /// The device address of `buffer` (core 1.2 `vkGetBufferDeviceAddress`), for feeding
    /// AS-build vertex / index / scratch input. The buffer must carry
    /// `SHADER_DEVICE_ADDRESS` usage. Lives here so the upload path (which holds only the
    /// bundle, not a `&Device`) can address its mesh buffers for the BLAS build.
    pub(crate) fn buffer_device_address(&self, buffer: vk::Buffer) -> vk::DeviceAddress {
        let info = vk::BufferDeviceAddressInfo::default().buffer(buffer);
        // SAFETY: the ash seam. The buffer was created with `SHADER_DEVICE_ADDRESS` usage;
        // the returned address is valid for the device's lifetime.
        unsafe { self.device.get_buffer_device_address(&info) }
    }
}

impl Drop for DeviceResources {
    fn drop(&mut self) {
        // The VMA allocator frees its own `VkDeviceMemory` through the live device,
        // so it must go before `vkDestroyDevice`.
        // Field order alone cannot guarantee it (the allocator is an `Option` so its
        // `Drop` runs here, ahead of the `device` field's drop).
        drop(self.allocator.take());
        // SAFETY: the ash seam. The run loop idled the device before any teardown, and this is
        // the last `Arc<DeviceResources>` holder, so no handle this device created is still live.
        unsafe { self.device.destroy_device(None) };
    }
}

/// Maps an ash `VkResult<T>` from a VMA create call into this crate's [`super::Error`].
fn checked_vma<T>(
    result: std::result::Result<T, vk::Result>,
    context: &'static str,
) -> crate::Result<T> {
    crate::checked(result, context)
}

#[cfg(test)]
mod tests;
