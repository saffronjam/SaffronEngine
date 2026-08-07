//! Frame-safe staging: the upload slice/plan, the per-frame-in-flight upload ring, and the
//! resident table that retires records once their referencing frame crosses its fence.

use super::*;

/// One upload allocation within a completed-frame-safe staging slot.
#[must_use]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UploadSlice {
    /// Staging buffer.
    pub buffer: vk::Buffer,
    /// Byte offset in the staging buffer.
    pub offset: u64,
    /// Payload size in bytes.
    pub size: u64,
    /// Total addressable size of the staging buffer.
    pub capacity: u64,
}

/// One staged copy into a global arena or immutable table.
#[must_use]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GpuBufferUpload {
    /// Host-visible staging buffer.
    pub(super) source: vk::Buffer,
    /// Device-local destination buffer.
    pub(super) destination: vk::Buffer,
    /// Staging-buffer byte offset.
    pub(super) source_offset: u64,
    /// Destination-buffer byte offset.
    pub(super) destination_offset: u64,
    /// Number of bytes to copy.
    pub(super) size: u64,
    /// Total addressable size of the source buffer.
    pub(super) source_size: u64,
    /// Total addressable size of the destination buffer.
    pub(super) destination_size: u64,
}

/// Copy plan returned when a global device-local arena grows.
#[must_use]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GpuArenaGrowth {
    /// Superseded arena buffer retained for in-flight frames.
    pub(super) source: vk::Buffer,
    /// New larger arena buffer.
    pub(super) destination: vk::Buffer,
    /// Live prefix to preserve.
    pub(super) size: u64,
    /// Total addressable size of the source buffer.
    pub(super) source_size: u64,
    /// Total addressable size of the destination buffer.
    pub(super) destination_size: u64,
}

impl GpuArenaGrowth {
    /// Enqueues the preservation copy and the zero-fill of the fresh tail beyond it as one
    /// fully declared render-graph transfer pass. The two ranges are disjoint, so the pass
    /// needs no intra-pass hazard ordering.
    pub fn enqueue(self, graph: &mut RenderGraph, device: &Device, name: &'static str) {
        let source = graph.register_buffer(RgBufferResource {
            buffer: self.source,
            size: self.source_size,
            usage: vk::BufferUsageFlags::TRANSFER_SRC,
            lifetime: RgBufferLifetime::Imported,
        });
        let destination = graph.register_buffer(RgBufferResource {
            buffer: self.destination,
            size: self.destination_size,
            usage: vk::BufferUsageFlags::TRANSFER_DST,
            lifetime: RgBufferLifetime::Imported,
        });
        let source_range =
            RgBufferRange::new(0, self.size).expect("validated global arena growth copy range");
        let destination_range = RgBufferRange::new(0, self.destination_size)
            .expect("validated global arena growth destination range");
        let resources = Arc::clone(device.resources());
        let copy = vk::BufferCopy::default().size(self.size);
        graph.add_pass(
            RgPass::compute(name)
                .access_buffer(source, source_range, RgUsage::TransferRead)
                .access_buffer(destination, destination_range, RgUsage::TransferWrite)
                .body(move |command_buffer, _| {
                    // SAFETY: both live imported buffers declare the exact transfer accesses on
                    // this graph pass and were created with the corresponding Vulkan usages.
                    unsafe {
                        resources.device().cmd_copy_buffer(
                            command_buffer,
                            self.source,
                            self.destination,
                            &[copy],
                        );
                        if self.destination_size > self.size {
                            resources.device().cmd_fill_buffer(
                                command_buffer,
                                self.destination,
                                self.size,
                                self.destination_size - self.size,
                                0,
                            );
                        }
                    }
                }),
        );
    }
}

impl GpuBufferUpload {
    /// Enqueues this upload as a fully declared render-graph transfer pass.
    pub fn enqueue(self, graph: &mut RenderGraph, device: &Device, name: &'static str) {
        let source = graph.register_buffer(RgBufferResource {
            buffer: self.source,
            size: self.source_size,
            usage: vk::BufferUsageFlags::TRANSFER_SRC,
            lifetime: RgBufferLifetime::Imported,
        });
        let destination = graph.register_buffer(RgBufferResource {
            buffer: self.destination,
            size: self.destination_size,
            usage: vk::BufferUsageFlags::TRANSFER_DST,
            lifetime: RgBufferLifetime::Imported,
        });
        let source_range = RgBufferRange::new(self.source_offset, self.size)
            .expect("validated global upload source range");
        let destination_range = RgBufferRange::new(self.destination_offset, self.size)
            .expect("validated global upload destination range");
        let resources = Arc::clone(device.resources());
        let copy = vk::BufferCopy::default()
            .src_offset(self.source_offset)
            .dst_offset(self.destination_offset)
            .size(self.size);
        graph.add_pass(
            RgPass::compute(name)
                .access_buffer(source, source_range, RgUsage::TransferRead)
                .access_buffer(destination, destination_range, RgUsage::TransferWrite)
                .body(move |command_buffer, _| {
                    // SAFETY: both live imported buffers declare the exact transfer accesses on
                    // this graph pass and were created with the corresponding Vulkan usages.
                    unsafe {
                        resources.device().cmd_copy_buffer(
                            command_buffer,
                            self.source,
                            self.destination,
                            &[copy],
                        );
                    }
                }),
        );
    }
}

pub(super) struct UploadFrame {
    pub(super) buffer: Buffer,
    pub(super) cursor: u64,
    pub(super) retired: Vec<Buffer>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct UploadPlan {
    pub(super) offset: u64,
    pub(super) end: u64,
    pub(super) required_capacity: u64,
}

pub(super) fn plan_upload(
    cursor: u64,
    capacity: u64,
    size: u64,
    alignment: u64,
) -> Result<UploadPlan> {
    if size == 0 {
        return Err(Error::InvalidUploadData(
            "global GPU upload payload must be nonempty".to_owned(),
        ));
    }
    let offset = align_up(cursor, alignment, "global GPU upload")?;
    let end = offset
        .checked_add(size)
        .ok_or_else(|| Error::InvalidUploadData("global GPU upload size overflow".to_owned()))?;
    let required_capacity = if end > capacity {
        next_capacity(capacity, end, INITIAL_UPLOAD_BYTES)?
    } else {
        capacity
    };
    Ok(UploadPlan {
        offset,
        end,
        required_capacity,
    })
}

/// Per-frame persistently mapped upload ring reset only after the owning fence signals.
pub struct FrameUploadRing {
    pub(super) resources: Arc<DeviceResources>,
    pub(super) frames: Vec<UploadFrame>,
}

impl FrameUploadRing {
    /// Creates one mapped upload buffer per frame-in-flight slot.
    pub fn new(device: &Device, initial_bytes: u64) -> Result<Self> {
        let bytes = next_capacity(0, initial_bytes.max(1), INITIAL_UPLOAD_BYTES)?;
        let mut frames = Vec::with_capacity(MAX_FRAMES_IN_FLIGHT);
        for _ in 0..MAX_FRAMES_IN_FLIGHT {
            frames.push(UploadFrame {
                buffer: create_upload_buffer(device.resources(), bytes)?,
                cursor: 0,
                retired: Vec::new(),
            });
        }
        Ok(Self {
            resources: Arc::clone(device.resources()),
            frames,
        })
    }

    /// Resets the completed slot. Growth here is safe because its fence has signalled.
    pub fn begin_frame(&mut self, completed_frame_slot: usize) -> Result<()> {
        frame_slot_bit(completed_frame_slot)?;
        let frame = self.frames.get_mut(completed_frame_slot).ok_or_else(|| {
            Error::InvalidUploadData("missing global GPU upload frame slot".to_owned())
        })?;
        frame.cursor = 0;
        frame.retired.clear();
        Ok(())
    }

    /// Copies one POD slice into the selected frame staging buffer.
    pub fn write<T: Pod>(
        &mut self,
        frame_slot: usize,
        values: &[T],
        alignment: u64,
    ) -> Result<UploadSlice> {
        let bytes = bytemuck::cast_slice(values);
        self.write_bytes(frame_slot, bytes, alignment)
    }

    /// Copies bytes into the selected frame staging buffer.
    pub fn write_bytes(
        &mut self,
        frame_slot: usize,
        bytes: &[u8],
        alignment: u64,
    ) -> Result<UploadSlice> {
        frame_slot_bit(frame_slot)?;
        let size = u64::try_from(bytes.len()).map_err(|_| {
            Error::InvalidUploadData("global GPU upload exceeds u64 bytes".to_owned())
        })?;
        let frame = self.frames.get_mut(frame_slot).ok_or_else(|| {
            Error::InvalidUploadData("missing global GPU upload frame slot".to_owned())
        })?;
        let plan = plan_upload(frame.cursor, frame.buffer.size(), size, alignment)?;
        if plan.required_capacity > frame.buffer.size() {
            let replacement = create_upload_buffer(&self.resources, plan.required_capacity)?;
            let old = std::mem::replace(&mut frame.buffer, replacement);
            frame.retired.push(old);
        }
        let mapped = frame.buffer.mapped_bytes().ok_or_else(|| {
            Error::InvalidUploadData("global GPU upload ring is not mapped".to_owned())
        })?;
        let offset = usize::try_from(plan.offset).map_err(|_| {
            Error::InvalidUploadData("global GPU upload offset exceeds address space".to_owned())
        })?;
        let end = usize::try_from(plan.end).map_err(|_| {
            Error::InvalidUploadData("global GPU upload end exceeds address space".to_owned())
        })?;
        let destination = mapped.get_mut(offset..end).ok_or_else(|| {
            Error::InvalidUploadData("global GPU upload exceeds mapped storage".to_owned())
        })?;
        destination.copy_from_slice(bytes);
        frame.buffer.flush_mapped()?;
        frame.cursor = plan.end;
        Ok(UploadSlice {
            buffer: frame.buffer.handle(),
            offset: plan.offset,
            size,
            capacity: frame.buffer.size(),
        })
    }
}

pub(super) fn create_upload_buffer(resources: &Arc<DeviceResources>, bytes: u64) -> Result<Buffer> {
    let allocation = vk_mem::AllocationCreateInfo {
        usage: vk_mem::MemoryUsage::AutoPreferHost,
        flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE
            | vk_mem::AllocationCreateFlags::MAPPED,
        ..Default::default()
    };
    Buffer::new(
        resources,
        bytes,
        vk::BufferUsageFlags::TRANSFER_SRC,
        &allocation,
    )
}

/// Generation header stored before every immutable GPU table record.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Pod, Zeroable)]
#[repr(C)]
pub struct GpuTableSlotHeader {
    /// Generation that must match the incoming handle.
    pub generation: u32,
    /// One for a live record and zero for an empty slot.
    pub occupied: u32,
    /// Reserved ABI words.
    pub reserved: [u32; 2],
}

/// Device-backed immutable record table indexed directly by [`GpuHandle::index`].
pub struct ResidentGpuTable<T: Pod, K> {
    pub(super) records: ImmutableGpuTable<T>,
    pub(super) storage: GlobalGpuArena<K>,
    pub(super) slot_stride: u64,
}

/// CPU record and graph-owned tombstone upload produced by table retirement.
#[must_use]
pub struct GpuRecordRetirement<T> {
    /// Retired immutable CPU record.
    pub record: T,
    /// Upload that invalidates the corresponding GPU slot before it can be observed again.
    pub tombstone: GpuBufferUpload,
}

impl<T: Pod, K> ResidentGpuTable<T, K> {
    /// Creates an empty table with a device-local record buffer.
    pub fn new(device: &Device, initial_records: u64) -> Result<Self> {
        let record_bytes = u64::try_from(std::mem::size_of::<T>())
            .map_err(|_| Error::InvalidUploadData("global GPU table stride overflow".to_owned()))?;
        if record_bytes == 0 {
            return Err(Error::InvalidUploadData(
                "global GPU table record must not be zero-sized".to_owned(),
            ));
        }
        let unaligned = 16_u64.checked_add(record_bytes).ok_or_else(|| {
            Error::InvalidUploadData("global GPU table slot size overflow".to_owned())
        })?;
        let slot_stride = unaligned.checked_add(15).ok_or_else(|| {
            Error::InvalidUploadData("global GPU table slot size overflow".to_owned())
        })? & !15;
        Ok(Self {
            records: ImmutableGpuTable::default(),
            storage: GlobalGpuArena::new(device, slot_stride, initial_records.max(1))?,
            slot_stride,
        })
    }

    /// Inserts one immutable record and reserves its direct-index GPU slot.
    pub fn insert(&mut self, record: T) -> Result<GpuHandle> {
        let (expected_index, needs_storage) = self.records.next_insert_index()?;
        if needs_storage {
            let (range, _) = self.storage.allocate(1, 1)?;
            if range.first != expected_index {
                return Err(Error::InvalidUploadData(
                    "global GPU table and storage indices diverged".to_owned(),
                ));
            }
        }
        let handle = self.records.insert(record)?;
        if handle.index != expected_index {
            return Err(Error::InvalidUploadData(
                "global GPU table insert index changed during allocation".to_owned(),
            ));
        }
        Ok(handle)
    }

    /// Returns a live record after generation validation.
    pub fn get(&self, handle: GpuHandle) -> Option<&T> {
        self.records.get(handle)
    }

    /// Overwrites a live record in place, keeping its handle and generation.
    ///
    /// The caller restages the slot so the device copy follows; the page table uses this
    /// for residency publication and eviction (byte span + resident generation).
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidUploadData`] when the handle is stale or unoccupied.
    pub fn update(&mut self, handle: GpuHandle, record: T) -> Result<()> {
        let value = self.records.get_mut(handle).ok_or_else(|| {
            Error::InvalidUploadData("global GPU table update requires a live handle".to_owned())
        })?;
        *value = record;
        Ok(())
    }

    /// Stages a GPU tombstone, then retires the CPU record with fence-deferred reuse.
    pub fn retire(
        &mut self,
        uploads: &mut FrameUploadRing,
        frame_slot: usize,
        handle: GpuHandle,
    ) -> Result<Option<GpuRecordRetirement<T>>> {
        if self.records.get(handle).is_none() {
            return Ok(None);
        }
        let tombstone = self.stage_slot(uploads, frame_slot, handle, 0, None)?;
        let record = self
            .records
            .retire(handle)
            .expect("record validated before its inseparable GPU tombstone was staged");
        Ok(Some(GpuRecordRetirement { record, tombstone }))
    }

    /// Iterates live records in direct GPU slot order.
    pub fn iter(&self) -> impl Iterator<Item = (GpuHandle, &T)> {
        self.records.iter()
    }

    /// Number of live table records.
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// Whether the table contains no live records.
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Grows the device buffer and returns the graph-owned copy for existing slots.
    pub fn prepare_growth(&mut self, device: &Device) -> Result<Option<GpuArenaGrowth>> {
        self.storage.prepare_growth(device)
    }

    /// Stages the current bytes of a live record for a graph-owned transfer pass.
    pub fn stage(
        &self,
        uploads: &mut FrameUploadRing,
        frame_slot: usize,
        handle: GpuHandle,
    ) -> Result<GpuBufferUpload> {
        let record = self
            .get(handle)
            .ok_or_else(|| Error::InvalidUploadData("stale global GPU table handle".to_owned()))?;
        self.stage_slot(
            uploads,
            frame_slot,
            handle,
            1,
            Some(bytemuck::bytes_of(record)),
        )
    }

    pub(super) fn stage_slot(
        &self,
        uploads: &mut FrameUploadRing,
        frame_slot: usize,
        handle: GpuHandle,
        occupied: u32,
        record: Option<&[u8]>,
    ) -> Result<GpuBufferUpload> {
        let slot_bytes = usize::try_from(self.slot_stride).map_err(|_| {
            Error::InvalidUploadData("global GPU table slot exceeds address space".to_owned())
        })?;
        let mut bytes = vec![0_u8; slot_bytes];
        let header = GpuTableSlotHeader {
            generation: handle.generation,
            occupied,
            reserved: [0; 2],
        };
        bytes[..16].copy_from_slice(bytemuck::bytes_of(&header));
        if let Some(record) = record {
            bytes[16..16 + record.len()].copy_from_slice(record);
        }
        let destination_offset = u64::from(handle.index)
            .checked_mul(self.slot_stride)
            .ok_or_else(|| {
                Error::InvalidUploadData("global GPU table byte offset overflow".to_owned())
            })?;
        let destination_end = destination_offset
            .checked_add(self.slot_stride)
            .ok_or_else(|| {
                Error::InvalidUploadData("global GPU table byte range overflow".to_owned())
            })?;
        if destination_end > self.storage.buffer.size() {
            return Err(Error::InvalidUploadData(
                "global GPU table must grow before staging its allocated slot".to_owned(),
            ));
        }
        let source = uploads.write_bytes(frame_slot, &bytes, 16)?;
        Ok(GpuBufferUpload {
            source: source.buffer,
            destination: self.storage.buffer(),
            source_offset: source.offset,
            destination_offset,
            size: source.size,
            source_size: source.capacity,
            destination_size: self.storage.buffer.size(),
        })
    }

    /// Device address of table element zero.
    pub fn address(&self, device: &Device) -> vk::DeviceAddress {
        self.storage.address(device)
    }

    /// Current table buffer.
    pub fn buffer(&self) -> vk::Buffer {
        self.storage.buffer()
    }

    /// Byte stride of a generation-header plus immutable record slot.
    pub fn slot_stride(&self) -> u64 {
        self.slot_stride
    }

    /// Descriptor-ready buffer identity for this immutable table.
    pub fn descriptor(&self, device: &Device) -> GpuTableDescriptor {
        GpuTableDescriptor {
            buffer: self.buffer(),
            offset: 0,
            range: self.storage.buffer.size(),
            address: self.address(device),
            slot_stride: self.slot_stride,
        }
    }

    /// Reclaims handles and superseded physical buffers after a frame fence signals.
    pub fn begin_frame(&mut self, completed_frame_slot: usize) -> Result<()> {
        self.records.begin_frame(completed_frame_slot)?;
        self.storage.begin_frame(completed_frame_slot)
    }
}
