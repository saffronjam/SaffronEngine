//! The RAII buffer wrapper: a VMA allocation plus its optional persistent host mapping.

use super::*;

/// A move-only VMA buffer. When [`Buffer::mapped`] is non-null the allocation is
/// persistently mapped for per-frame host writes; [`Drop`] frees it before the
/// allocator is destroyed (the bundle's `Arc` keeps the allocator alive until then).
pub struct Buffer {
    pub(super) resources: Arc<DeviceResources>,
    pub(super) buffer: vk::Buffer,
    pub(super) allocation: vk_mem::Allocation,
    pub(super) mapped: *mut u8,
    pub(super) size: vk::DeviceSize,
    pub(super) graph_state: Mutex<crate::RgExternalBufferState>,
}

// SAFETY: the raw `mapped` pointer is into VMA-owned, allocation-lifetime memory;
// the allocation/buffer handles are `Send` (vk-mem marks `Allocation` Send/Sync).
// The buffer carries no thread-affine state, so moving it across threads is sound.
unsafe impl Send for Buffer {}

impl Buffer {
    /// Creates a host-visible buffer holding `bytes` with `usage`.
    ///
    /// Build inputs that are written once and read by the device — micromap state blocks,
    /// triangle descriptors, index streams — want exactly this shape: no staging copy, no
    /// transfer pass, just a mapped write the build reads by device address.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Vk`] if the buffer cannot be created.
    pub fn from_slice_with_usage(
        resources: &Arc<DeviceResources>,
        bytes: &[u8],
        usage: vk::BufferUsageFlags,
    ) -> crate::Result<Self> {
        // 256-byte aligned: micromap and acceleration-structure build inputs are read by
        // device address, and the spec requires those addresses aligned. A misaligned input
        // is invalid rather than merely slow.
        let buffer = Self::with_alignment(
            resources,
            bytes.len().max(1) as vk::DeviceSize,
            usage,
            &vk_mem::AllocationCreateInfo {
                usage: vk_mem::MemoryUsage::Auto,
                flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE
                    | vk_mem::AllocationCreateFlags::MAPPED,
                ..Default::default()
            },
            256,
        )?;
        if !bytes.is_empty() {
            // SAFETY: the allocation is `MAPPED` and at least `bytes.len()` long.
            unsafe {
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), buffer.mapped_ptr(), bytes.len());
            }
        }
        Ok(buffer)
    }

    /// Creates a buffer of `size` with `usage`, allocated per `alloc_info`.
    ///
    /// When `alloc_info` requests `MAPPED`, [`Buffer::mapped`] returns the persistent
    /// host pointer; otherwise it is null.
    ///
    /// # Errors
    ///
    /// Returns [`super::Error::Vk`] if `vmaCreateBuffer` fails.
    pub fn new(
        resources: &Arc<DeviceResources>,
        size: vk::DeviceSize,
        usage: vk::BufferUsageFlags,
        alloc_info: &vk_mem::AllocationCreateInfo,
    ) -> crate::Result<Self> {
        Self::with_alignment(resources, size, usage, alloc_info, 1)
    }

    /// Creates a buffer whose device address is a multiple of `min_alignment`.
    ///
    /// VMA suballocates from larger blocks, so a plain [`Buffer::new`] only inherits the
    /// buffer's own memory-requirement alignment. Acceleration-structure build scratch
    /// needs more than that ([`DeviceResources::scratch_alignment`]).
    ///
    /// # Errors
    ///
    /// Returns [`super::Error::Vk`] if `vmaCreateBufferWithAlignment` fails.
    pub fn with_alignment(
        resources: &Arc<DeviceResources>,
        size: vk::DeviceSize,
        usage: vk::BufferUsageFlags,
        alloc_info: &vk_mem::AllocationCreateInfo,
        min_alignment: vk::DeviceSize,
    ) -> crate::Result<Self> {
        let buffer_info = vk::BufferCreateInfo::default().size(size).usage(usage);
        // SAFETY: the VMA seam. The create-infos are valid for the call; the
        // returned buffer + allocation are owned and freed in `Drop`.
        let (buffer, allocation) = checked_vma(
            unsafe {
                resources.allocator().create_buffer_with_alignment(
                    &buffer_info,
                    alloc_info,
                    min_alignment.max(1),
                )
            },
            "vmaCreateBufferWithAlignment",
        )?;
        let mapped = resources
            .allocator()
            .get_allocation_info(&allocation)
            .mapped_data
            .cast::<u8>();
        Ok(Self {
            resources: Arc::clone(resources),
            buffer,
            allocation,
            mapped,
            size,
            graph_state: Mutex::new(crate::RgExternalBufferState::default()),
        })
    }

    /// The buffer handle.
    pub fn handle(&self) -> vk::Buffer {
        self.buffer
    }

    /// The buffer size in bytes.
    pub fn size(&self) -> vk::DeviceSize {
        self.size
    }

    /// Complete cross-frame render-graph state for this buffer.
    pub fn graph_state(&self) -> crate::RgExternalBufferState {
        self.graph_state
            .lock()
            .expect("buffer graph-state mutex poisoned")
            .clone()
    }

    /// Stores the byte-range state resolved by the render graph.
    pub fn set_graph_state(&self, state: crate::RgExternalBufferState) {
        *self
            .graph_state
            .lock()
            .expect("buffer graph-state mutex poisoned") = state;
    }

    /// The persistent host-mapped pointer, or null when the buffer was not created
    /// `MAPPED`.
    pub fn mapped_ptr(&self) -> *mut u8 {
        self.mapped
    }

    /// The mapped allocation as a writable byte slice, or `None` when unmapped.
    ///
    /// Callers writing GPU-visible structs through this must respect the std430
    /// layout contract; the slice spans the full [`Buffer::size`].
    pub fn mapped_bytes(&mut self) -> Option<&mut [u8]> {
        if self.mapped.is_null() {
            return None;
        }
        // SAFETY: the allocation is HOST_VISIBLE + persistently MAPPED for `size`
        // bytes; the `&mut self` borrow makes the slice exclusive.
        Some(unsafe { std::slice::from_raw_parts_mut(self.mapped, self.size as usize) })
    }

    /// Flushes the complete mapped allocation before GPU reads.
    pub(crate) fn flush_mapped(&self) -> crate::Result<()> {
        checked_vma(
            self.resources
                .allocator()
                .flush_allocation(&self.allocation, 0, self.size),
            "vmaFlushAllocation",
        )
    }

    /// Invalidates the complete mapped allocation before host readback.
    pub(crate) fn invalidate_mapped(&self) -> crate::Result<()> {
        checked_vma(
            self.resources
                .allocator()
                .invalidate_allocation(&self.allocation, 0, self.size),
            "vmaInvalidateAllocation",
        )
    }
}

impl Drop for Buffer {
    fn drop(&mut self) {
        // SAFETY: the VMA seam. The `Arc<DeviceResources>` keeps the allocator alive
        // for this call; the buffer/allocation are destroyed exactly once.
        unsafe {
            self.resources
                .allocator()
                .destroy_buffer(self.buffer, &mut self.allocation);
        }
    }
}
