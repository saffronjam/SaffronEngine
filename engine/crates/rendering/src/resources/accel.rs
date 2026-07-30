//! The pipeline, acceleration-structure, and opacity-micromap wrappers.

use super::*;

/// A graphics or compute pipeline owning its `vk::Pipeline` + `vk::PipelineLayout`.
///
/// Owned by the renderer (never crosses to client code); [`Drop`] frees the pipeline
/// then the layout through the device.
pub struct Pipeline {
    pub(super) resources: Arc<DeviceResources>,
    pub(super) pipeline: vk::Pipeline,
    pub(super) layout: vk::PipelineLayout,
}

// SAFETY: the pipeline/layout handles carry no thread-affine state; cached PSOs are
// shared as `Arc<Pipeline>` across the upload + render threads.
unsafe impl Send for Pipeline {}

impl Pipeline {
    /// Wraps an already-created pipeline + its layout. The PSO-cache phase creates
    /// the pipeline (graphics or compute) and its layout, then hands them here.
    pub fn from_parts(
        resources: &Arc<DeviceResources>,
        pipeline: vk::Pipeline,
        layout: vk::PipelineLayout,
    ) -> Self {
        Self {
            resources: Arc::clone(resources),
            pipeline,
            layout,
        }
    }

    /// The pipeline handle.
    pub fn handle(&self) -> vk::Pipeline {
        self.pipeline
    }

    /// The pipeline layout.
    pub fn layout(&self) -> vk::PipelineLayout {
        self.layout
    }
}

impl Drop for Pipeline {
    fn drop(&mut self) {
        // SAFETY: the ash seam. The bundle keeps the device alive; pipeline then
        // layout, in that order, each freed exactly once.
        unsafe {
            self.resources
                .device()
                .destroy_pipeline(self.pipeline, None);
            self.resources
                .device()
                .destroy_pipeline_layout(self.layout, None);
        }
    }
}

/// A ray-tracing acceleration structure (BLAS or TLAS): the `vk` handle, its device
/// address, and its backing device buffer.
///
/// It clones the ash `acceleration_structure::Device` (a cheap handle + fn-pointer
/// table) at construction so [`Drop`] is self-contained — no live dispatch needed.
/// [`Drop`] destroys the handle (through the cloned dispatch) then the backing buffer
/// (through the allocator).
pub struct AccelerationStructure {
    pub(super) resources: Arc<DeviceResources>,
    pub(super) dispatch: ash::khr::acceleration_structure::Device,
    pub(super) handle: vk::AccelerationStructureKHR,
    pub(super) buffer: vk::Buffer,
    pub(super) allocation: vk_mem::Allocation,
    /// The device address (for TLAS instance references / shader binding).
    pub address: vk::DeviceAddress,
    /// Bytes of AS storage this structure occupies.
    pub(super) size: vk::DeviceSize,
    /// Bytes the original build reserved. Equal to [`Self::size`] unless the structure came
    /// out of a compaction copy, so the difference is the saving that copy realized.
    pub(super) built_size: vk::DeviceSize,
}

// SAFETY: the dispatch is a handle + fn-pointer table (Clone, no thread affinity);
// the buffer/allocation carry no thread-affine state. A BLAS is shared as
// `Arc<AccelerationStructure>` from `GpuMesh`, which may drop off the worker thread.
unsafe impl Send for AccelerationStructure {}

// SAFETY: every field is shared read-only after construction. A BLAS rides inside an
// `Arc<GpuMesh>` that the thumbnail worker hands back to the main thread through an
// `Arc<Mutex<_>>`, so `GpuMesh: Sync` requires `AccelerationStructure: Sync`.
unsafe impl Sync for AccelerationStructure {}

/// A built opacity micromap plus the per-triangle index buffer the geometry chain points at.
///
/// Modelled on [`AccelerationStructure`], including the **dedicated** allocation: micromap
/// storage sits alongside acceleration-structure storage in the driver's world, and sharing a
/// memory block with ordinary buffers is what wedged the GPU when acceleration structures did
/// it. The dispatch is cloned at construction because the destroy entry point is an extension
/// command.
pub struct Micromap {
    pub(super) resources: Arc<DeviceResources>,
    pub(super) dispatch: ash::ext::opacity_micromap::Device,
    pub(super) handle: vk::MicromapEXT,
    pub(super) buffer: vk::Buffer,
    pub(super) allocation: vk_mem::Allocation,
    /// The `i32` per-triangle index stream, referenced by the geometry chain.
    pub(super) index_buffer: Buffer,
    pub(super) index_address: vk::DeviceAddress,
    /// Usage rows the geometry chain must repeat verbatim.
    pub(super) usage: Vec<vk::MicromapUsageEXT>,
    /// What the derivation settled, for telemetry.
    pub(super) classes: (u64, u64, u64),
    pub(super) size: vk::DeviceSize,
}

// SAFETY: the dispatch is a handle + fn-pointer table; the buffer/allocation carry no
// thread-affine state. A micromap rides inside an `Arc` shared exactly like a BLAS.
unsafe impl Send for Micromap {}

// SAFETY: every field is shared read-only after construction.
unsafe impl Sync for Micromap {}

impl Micromap {
    /// The micromap handle.
    pub fn handle(&self) -> vk::MicromapEXT {
        self.handle
    }

    /// The per-triangle index buffer's device address.
    pub fn index_address(&self) -> vk::DeviceAddress {
        self.index_address
    }

    /// The per-triangle index buffer. Owned here so it outlives every geometry chain that
    /// references its address.
    pub fn index_buffer(&self) -> vk::Buffer {
        self.index_buffer.handle()
    }

    /// The usage rows the geometry chain repeats.
    pub fn usage(&self) -> &[vk::MicromapUsageEXT] {
        &self.usage
    }

    /// Bytes of micromap storage.
    pub fn size(&self) -> vk::DeviceSize {
        self.size
    }

    /// `(opaque, transparent, unknown)` micro-triangle counts the derivation produced.
    pub fn classes(&self) -> (u64, u64, u64) {
        self.classes
    }

    /// Creates the backing storage and the micromap object over it of `size` bytes. The build
    /// is recorded separately.
    ///
    /// # Errors
    ///
    /// Returns [`super::Error::Vk`] if the storage buffer or the micromap cannot be created.
    pub fn create(
        resources: &Arc<DeviceResources>,
        dispatch: &ash::ext::opacity_micromap::Device,
        size: vk::DeviceSize,
        index_buffer: Buffer,
        usage: Vec<vk::MicromapUsageEXT>,
        classes: (u64, u64, u64),
    ) -> crate::Result<Self> {
        let buffer_info = vk::BufferCreateInfo::default().size(size.max(1)).usage(
            vk::BufferUsageFlags::MICROMAP_STORAGE_EXT
                | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS,
        );
        let alloc_info = vk_mem::AllocationCreateInfo {
            usage: vk_mem::MemoryUsage::AutoPreferDevice,
            flags: vk_mem::AllocationCreateFlags::DEDICATED_MEMORY,
            ..Default::default()
        };
        // SAFETY: the VMA seam. The create-info is valid; both are freed in `Drop` (or below
        // on a create failure).
        let (buffer, allocation) = checked_vma(
            unsafe {
                resources
                    .allocator()
                    .create_buffer(&buffer_info, &alloc_info)
            },
            "vmaCreateBuffer (micromap storage)",
        )?;
        let create_info = vk::MicromapCreateInfoEXT::default()
            .buffer(buffer)
            .size(size.max(1))
            .ty(vk::MicromapTypeEXT::OPACITY_MICROMAP);
        // SAFETY: the ash seam, through the raw function table — ash 0.38 ships no high-level
        // wrapper for this extension. The backing buffer covers `size`; the handle is destroyed
        // in `Drop` through the cloned dispatch.
        let mut handle = vk::MicromapEXT::null();
        let created = unsafe {
            (dispatch.fp().create_micromap_ext)(
                dispatch.device(),
                &create_info,
                std::ptr::null(),
                &mut handle,
            )
        };
        let handle = match created.result().map(|()| handle) {
            Ok(handle) => handle,
            Err(result) => {
                let mut allocation = allocation;
                // SAFETY: the VMA seam. Free the storage before the early return.
                unsafe {
                    resources
                        .allocator()
                        .destroy_buffer(buffer, &mut allocation)
                };
                return Err(crate::Error::Vk {
                    context: "create_micromap",
                    result,
                });
            }
        };
        let index_address = resources.buffer_device_address(index_buffer.handle());
        Ok(Self {
            resources: Arc::clone(resources),
            dispatch: dispatch.clone(),
            handle,
            buffer,
            allocation,
            index_buffer,
            index_address,
            usage,
            classes,
            size,
        })
    }
}

impl Drop for Micromap {
    fn drop(&mut self) {
        // SAFETY: the ash/VMA seam. The device is idle (the loop waits before teardown); the
        // handle and buffer are owned here and freed exactly once.
        unsafe {
            (self.dispatch.fp().destroy_micromap_ext)(
                self.dispatch.device(),
                self.handle,
                std::ptr::null(),
            );
            self.resources
                .allocator()
                .destroy_buffer(self.buffer, &mut self.allocation);
        }
    }
}

impl AccelerationStructure {
    /// Allocates an AS-storage backing buffer of `size`, creates the acceleration
    /// structure of `kind` over it, and queries its device address. The backing buffer
    /// carries `ACCELERATION_STRUCTURE_STORAGE | SHADER_DEVICE_ADDRESS` usage; the build is
    /// recorded separately by the caller.
    ///
    /// The allocation is **dedicated**: acceleration-structure storage does not share a
    /// memory block with ordinary buffers. Suballocating it alongside them wedges the GPU —
    /// not at the build, but on an unrelated later submission — so the isolation is a
    /// correctness requirement here, not a tuning choice.
    ///
    /// # Errors
    ///
    /// Returns [`super::Error::Vk`] if the storage buffer or the AS cannot be created.
    pub fn create(
        resources: &Arc<DeviceResources>,
        dispatch: &ash::khr::acceleration_structure::Device,
        size: vk::DeviceSize,
        kind: vk::AccelerationStructureTypeKHR,
    ) -> crate::Result<Self> {
        let buffer_info = vk::BufferCreateInfo::default().size(size).usage(
            vk::BufferUsageFlags::ACCELERATION_STRUCTURE_STORAGE_KHR
                | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS,
        );
        let alloc_info = vk_mem::AllocationCreateInfo {
            usage: vk_mem::MemoryUsage::AutoPreferDevice,
            flags: vk_mem::AllocationCreateFlags::DEDICATED_MEMORY,
            ..Default::default()
        };
        // SAFETY: the VMA seam. The create-info is valid; the buffer + allocation are
        // owned and freed in `Drop` (or below on a create-AS failure).
        let (buffer, allocation) = checked_vma(
            unsafe {
                resources
                    .allocator()
                    .create_buffer(&buffer_info, &alloc_info)
            },
            "vmaCreateBuffer (accel storage)",
        )?;

        let create_info = vk::AccelerationStructureCreateInfoKHR::default()
            .buffer(buffer)
            .size(size)
            .ty(kind);
        // SAFETY: the ash seam. The backing buffer covers `size` with AS-storage usage;
        // the returned handle is destroyed in `Drop` through the cloned dispatch.
        let handle = match unsafe { dispatch.create_acceleration_structure(&create_info, None) } {
            Ok(handle) => handle,
            Err(result) => {
                let mut allocation = allocation;
                // SAFETY: the VMA seam. Free the storage buffer before the early return.
                unsafe {
                    resources
                        .allocator()
                        .destroy_buffer(buffer, &mut allocation)
                };
                return Err(crate::Error::Vk {
                    context: "create_acceleration_structure",
                    result,
                });
            }
        };

        let address_info =
            vk::AccelerationStructureDeviceAddressInfoKHR::default().acceleration_structure(handle);
        // SAFETY: the ash seam. The handle was just created on this dispatch's device.
        let address = unsafe { dispatch.get_acceleration_structure_device_address(&address_info) };

        Ok(Self {
            resources: Arc::clone(resources),
            dispatch: dispatch.clone(),
            handle,
            buffer,
            allocation,
            address,
            size,
            built_size: size,
        })
    }

    /// Bytes of AS storage this structure occupies.
    pub fn size(&self) -> vk::DeviceSize {
        self.size
    }

    /// Bytes the original build reserved, before any compaction copy.
    pub fn built_size(&self) -> vk::DeviceSize {
        self.built_size
    }

    /// Records that this structure is the compacted copy of a build that reserved
    /// `built_size` bytes, so the saving stays attributable after the source is dropped.
    pub fn note_compacted_from(&mut self, built_size: vk::DeviceSize) {
        self.built_size = built_size;
    }

    /// The acceleration-structure handle.
    pub fn handle(&self) -> vk::AccelerationStructureKHR {
        self.handle
    }
}

impl Drop for AccelerationStructure {
    fn drop(&mut self) {
        // SAFETY: the ash/VMA seam. The bundle keeps the device + allocator alive;
        // the cloned dispatch resolves `vkDestroyAccelerationStructureKHR`. The
        // handle is destroyed then the backing buffer, each exactly once.
        unsafe {
            self.dispatch
                .destroy_acceleration_structure(self.handle, None);
            self.resources
                .allocator()
                .destroy_buffer(self.buffer, &mut self.allocation);
        }
    }
}
