use ash::vk;
use vk_mem::Alloc;

use crate::{Result, checked};

/// A host-visible, persistently mapped staging buffer that flushes and frees itself.
pub(super) struct StagingBuffer<'a> {
    allocator: &'a vk_mem::Allocator,
    buffer: vk::Buffer,
    allocation: vk_mem::Allocation,
    mapped: *mut u8,
    size: vk::DeviceSize,
}

impl<'a> StagingBuffer<'a> {
    /// Allocates a `TRANSFER_SRC`, host-sequential-write, mapped buffer of `size`.
    pub(super) fn new(allocator: &'a vk_mem::Allocator, size: vk::DeviceSize) -> Result<Self> {
        let info = vk::BufferCreateInfo::default()
            .size(size)
            .usage(vk::BufferUsageFlags::TRANSFER_SRC);
        let alloc_info = vk_mem::AllocationCreateInfo {
            usage: vk_mem::MemoryUsage::Auto,
            flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE
                | vk_mem::AllocationCreateFlags::MAPPED,
            ..Default::default()
        };
        // SAFETY: the VMA seam. The create-infos are valid; the buffer is freed in
        // `Drop`.
        let (buffer, allocation) = checked(
            unsafe { allocator.create_buffer(&info, &alloc_info) },
            "vmaCreateBuffer (staging)",
        )?;
        let mapped = allocator
            .get_allocation_info(&allocation)
            .mapped_data
            .cast::<u8>();
        Ok(Self {
            allocator,
            buffer,
            allocation,
            mapped,
            size,
        })
    }

    pub(super) fn handle(&self) -> vk::Buffer {
        self.buffer
    }

    /// The mapped staging memory as a writable byte slice (the upload source).
    pub(super) fn mapped_slice(&mut self) -> &mut [u8] {
        // SAFETY: the allocation is HOST_VISIBLE + MAPPED for `size` bytes; the
        // `&mut self` borrow makes the slice exclusive.
        unsafe { std::slice::from_raw_parts_mut(self.mapped, self.size as usize) }
    }

    /// Flushes the mapped writes so the GPU copy sees them.
    pub(super) fn flush(&self) {
        // The map may be coherent; the flush is a no-op then. Either way it flushes
        // the whole allocation.
        let _ = self
            .allocator
            .flush_allocation(&self.allocation, 0, self.size);
    }
}

impl Drop for StagingBuffer<'_> {
    fn drop(&mut self) {
        // SAFETY: the VMA seam. The staging buffer is freed exactly once after the
        // copy completed (the one-off submit was waited before this drop).
        unsafe {
            self.allocator
                .destroy_buffer(self.buffer, &mut self.allocation);
        }
    }
}

/// Allocates a device-local buffer (`size`, `usage | TRANSFER_DST`, auto memory).
pub(super) fn make_device_buffer(
    allocator: &vk_mem::Allocator,
    size: vk::DeviceSize,
    usage: vk::BufferUsageFlags,
) -> Result<(vk::Buffer, vk_mem::Allocation)> {
    let info = vk::BufferCreateInfo::default()
        .size(size)
        .usage(usage | vk::BufferUsageFlags::TRANSFER_DST);
    let alloc_info = vk_mem::AllocationCreateInfo {
        usage: vk_mem::MemoryUsage::AutoPreferDevice,
        ..Default::default()
    };
    // SAFETY: the VMA seam. The create-infos are valid; ownership of the returned
    // buffer passes to the caller (the `GpuMesh`, or freed on the error path).
    checked(
        unsafe { allocator.create_buffer(&info, &alloc_info) },
        "vmaCreateBuffer (device)",
    )
}

/// Frees one device buffer directly — the mesh-upload error-path cleanup before a
/// `GpuMesh` takes ownership of the set.
pub(super) fn free_one(allocator: &vk_mem::Allocator, buffer: (vk::Buffer, vk_mem::Allocation)) {
    let (handle, mut allocation) = buffer;
    // SAFETY: the VMA seam. The buffer was created on this allocator and not yet
    // owned by a `GpuMesh`; freed exactly once on the error path.
    unsafe { allocator.destroy_buffer(handle, &mut allocation) };
}
