//! Stable handles, the immutable table, the range allocator, and the device-local growable
//! arena that keeps superseded allocations alive for frames still in flight.

use super::*;

/// Stable index plus generation used by every global GPU table.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Pod, Zeroable)]
#[repr(C, align(8))]
pub struct GpuHandle {
    /// Stable table slot.
    pub index: u32,
    /// Generation that rejects stale references after deferred reuse.
    pub generation: u32,
}

impl GpuHandle {
    /// An invalid handle suitable for optional record fields.
    pub const INVALID: Self = Self {
        index: u32::MAX,
        generation: 0,
    };

    /// Packs the wire/GPU representation into one scalar.
    pub const fn packed(self) -> u64 {
        (self.generation as u64) << 32 | self.index as u64
    }

    /// Restores a handle from its packed representation.
    pub const fn from_packed(value: u64) -> Self {
        Self {
            index: value as u32,
            generation: (value >> 32) as u32,
        }
    }
}

pub(super) struct HandleSlot<T> {
    pub(super) generation: u32,
    pub(super) value: Option<T>,
}

pub(super) struct RetiredHandle {
    pub(super) index: u32,
    pub(super) pending_frame_slots: u64,
}

/// Immutable-record table with generation checks and fence-deferred slot reuse.
pub struct ImmutableGpuTable<T> {
    pub(super) slots: Vec<HandleSlot<T>>,
    pub(super) reusable: Vec<u32>,
    pub(super) retired: Vec<RetiredHandle>,
    pub(super) live: usize,
}

impl<T> Default for ImmutableGpuTable<T> {
    fn default() -> Self {
        Self {
            slots: Vec::new(),
            reusable: Vec::new(),
            retired: Vec::new(),
            live: 0,
        }
    }
}

impl<T> ImmutableGpuTable<T> {
    /// Inserts one immutable record and returns its stable generational handle.
    pub fn insert(&mut self, value: T) -> Result<GpuHandle> {
        let index = if let Some(index) = self.reusable.pop() {
            index
        } else {
            let index = u32::try_from(self.slots.len()).map_err(|_| {
                Error::InvalidUploadData("global GPU table exceeds u32 slots".to_owned())
            })?;
            self.slots.push(HandleSlot {
                generation: 1,
                value: None,
            });
            index
        };
        let slot = &mut self.slots[index as usize];
        debug_assert!(slot.value.is_none());
        slot.value = Some(value);
        self.live += 1;
        Ok(GpuHandle {
            index,
            generation: slot.generation,
        })
    }

    /// Looks up a record only when both slot and generation match.
    pub fn get(&self, handle: GpuHandle) -> Option<&T> {
        let slot = self.slots.get(handle.index as usize)?;
        (slot.generation == handle.generation)
            .then_some(slot.value.as_ref())
            .flatten()
    }

    /// Mutably looks up a record only when both slot and generation match.
    pub fn get_mut(&mut self, handle: GpuHandle) -> Option<&mut T> {
        let slot = self.slots.get_mut(handle.index as usize)?;
        (slot.generation == handle.generation)
            .then_some(slot.value.as_mut())
            .flatten()
    }

    /// Retires a record without making its slot reusable by in-flight frames.
    pub fn retire(&mut self, handle: GpuHandle) -> Option<T> {
        let slot = self.slots.get_mut(handle.index as usize)?;
        if slot.generation != handle.generation {
            return None;
        }
        let value = slot.value.take()?;
        self.live -= 1;
        self.retired.push(RetiredHandle {
            index: handle.index,
            pending_frame_slots: live_frame_mask(),
        });
        Some(value)
    }

    /// Releases retired slots referenced by the frame slot whose fence completed.
    pub fn begin_frame(&mut self, completed_frame_slot: usize) -> Result<()> {
        let completed = frame_slot_bit(completed_frame_slot)?;
        let mut ready = Vec::new();
        self.retired.retain_mut(|retired| {
            retired.pending_frame_slots &= !completed;
            if retired.pending_frame_slots == 0 {
                ready.push(retired.index);
                false
            } else {
                true
            }
        });
        for index in ready {
            let slot = &mut self.slots[index as usize];
            if let Some(generation) = slot.generation.checked_add(1) {
                slot.generation = generation;
                self.reusable.push(index);
            }
        }
        Ok(())
    }

    /// Number of live immutable records.
    pub fn len(&self) -> usize {
        self.live
    }

    /// Whether the table contains no live records.
    pub fn is_empty(&self) -> bool {
        self.live == 0
    }

    /// Iterates live records in stable slot order.
    pub fn iter(&self) -> impl Iterator<Item = (GpuHandle, &T)> {
        self.slots.iter().enumerate().filter_map(|(index, slot)| {
            slot.value.as_ref().map(|value| {
                (
                    GpuHandle {
                        index: index as u32,
                        generation: slot.generation,
                    },
                    value,
                )
            })
        })
    }

    pub(super) fn next_insert_index(&self) -> Result<(u32, bool)> {
        if let Some(&index) = self.reusable.last() {
            return Ok((index, false));
        }
        let index = u32::try_from(self.slots.len()).map_err(|_| {
            Error::InvalidUploadData("global GPU table exceeds u32 slots".to_owned())
        })?;
        Ok((index, true))
    }
}

/// Contiguous global-arena element range.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Pod, Zeroable)]
#[repr(C, align(8))]
pub struct GpuArenaRange {
    /// First element in the arena.
    pub first: u32,
    /// Number of elements.
    pub count: u32,
}

impl GpuArenaRange {
    /// Exclusive end element.
    pub const fn end(self) -> u64 {
        self.first as u64 + self.count as u64
    }
}

pub(super) struct RetiredRange {
    pub(super) range: GpuArenaRange,
    pub(super) pending_frame_slots: u64,
}

#[derive(Clone, Copy)]
pub(super) struct LiveRange {
    pub(super) logical_count: u32,
    pub(super) reserved_count: u32,
}

/// Best-fit range allocator whose freed ranges become reusable only after all frame fences pass.
#[derive(Default)]
pub struct GpuRangeAllocator {
    pub(super) high_water: u64,
    pub(super) live: BTreeMap<u32, LiveRange>,
    pub(super) free: BTreeMap<u32, u32>,
    pub(super) retired: Vec<RetiredRange>,
}

impl GpuRangeAllocator {
    /// Reserves a stable contiguous range.
    pub fn allocate(&mut self, count: u32, alignment: u32) -> Result<GpuArenaRange> {
        self.allocate_reserved(count, count, alignment)
    }

    pub(super) fn allocate_reserved(
        &mut self,
        logical_count: u32,
        reserved_count: u32,
        alignment: u32,
    ) -> Result<GpuArenaRange> {
        if logical_count == 0 || reserved_count < logical_count {
            return Err(Error::InvalidUploadData(
                "global GPU physical range must contain its nonempty logical range".to_owned(),
            ));
        }
        if alignment == 0 || !alignment.is_power_of_two() {
            return Err(Error::InvalidUploadData(
                "global GPU range alignment must be a nonzero power of two".to_owned(),
            ));
        }
        let candidate = self
            .free
            .iter()
            .filter_map(|(&first, &available)| {
                let aligned =
                    align_up(u64::from(first), u64::from(alignment), "global GPU range").ok()?;
                let aligned = u32::try_from(aligned).ok()?;
                let padding = aligned.checked_sub(first)?;
                let consumed = padding.checked_add(reserved_count)?;
                (available >= consumed).then(|| {
                    (
                        available - padding - reserved_count,
                        first,
                        available,
                        aligned,
                        padding,
                    )
                })
            })
            .min_by_key(|candidate| (candidate.0, candidate.2));
        if let Some((_, first, available, aligned, padding)) = candidate {
            self.free.remove(&first);
            if padding != 0 {
                self.free.insert(first, padding);
            }
            let consumed = padding.checked_add(reserved_count).ok_or_else(|| {
                Error::InvalidUploadData("global GPU range size overflow".to_owned())
            })?;
            if available > consumed {
                self.free.insert(
                    aligned.checked_add(reserved_count).ok_or_else(|| {
                        Error::InvalidUploadData("global GPU range end overflow".to_owned())
                    })?,
                    available - consumed,
                );
            }
            let range = GpuArenaRange {
                first: aligned,
                count: logical_count,
            };
            self.live.insert(
                range.first,
                LiveRange {
                    logical_count,
                    reserved_count,
                },
            );
            return Ok(range);
        }
        let aligned = align_up(self.high_water, u64::from(alignment), "global GPU range")?;
        let first = u32::try_from(aligned).map_err(|_| {
            Error::InvalidUploadData("global GPU arena exceeds u32 elements".to_owned())
        })?;
        let end = aligned
            .checked_add(u64::from(reserved_count))
            .ok_or_else(|| {
                Error::InvalidUploadData("global GPU arena element count overflow".to_owned())
            })?;
        if end > u64::from(u32::MAX) + 1 {
            return Err(Error::InvalidUploadData(
                "global GPU arena exceeds u32 elements".to_owned(),
            ));
        }
        if aligned > self.high_water {
            self.insert_free(GpuArenaRange {
                first: u32::try_from(self.high_water).map_err(|_| {
                    Error::InvalidUploadData("global GPU arena exceeds u32 elements".to_owned())
                })?,
                count: u32::try_from(aligned - self.high_water).map_err(|_| {
                    Error::InvalidUploadData("global GPU arena alignment gap overflow".to_owned())
                })?,
            });
        }
        self.high_water = end;
        let range = GpuArenaRange {
            first,
            count: logical_count,
        };
        self.live.insert(
            range.first,
            LiveRange {
                logical_count,
                reserved_count,
            },
        );
        Ok(range)
    }

    /// Defers reuse of a range until every potentially referencing frame completes.
    pub fn retire(&mut self, range: GpuArenaRange) -> Result<()> {
        let Some(live) = self.live.get(&range.first).copied() else {
            return Err(Error::InvalidUploadData(
                "global GPU range retirement must match one live allocation".to_owned(),
            ));
        };
        if live.logical_count != range.count {
            return Err(Error::InvalidUploadData(
                "global GPU range retirement must match one live allocation".to_owned(),
            ));
        }
        self.live.remove(&range.first);
        self.retired.push(RetiredRange {
            range: GpuArenaRange {
                first: range.first,
                count: live.reserved_count,
            },
            pending_frame_slots: live_frame_mask(),
        });
        Ok(())
    }

    /// Releases ranges referenced by the completed frame slot and coalesces adjacent space.
    pub fn begin_frame(&mut self, completed_frame_slot: usize) -> Result<()> {
        let completed = frame_slot_bit(completed_frame_slot)?;
        let mut ready = Vec::new();
        self.retired.retain_mut(|retired| {
            retired.pending_frame_slots &= !completed;
            if retired.pending_frame_slots == 0 {
                ready.push(retired.range);
                false
            } else {
                true
            }
        });
        for range in ready {
            self.insert_free(range);
        }
        Ok(())
    }

    /// Required element capacity including unreclaimed and retired ranges.
    pub fn required_capacity(&self) -> u64 {
        self.high_water
    }

    pub(super) fn reserved_range(&self, range: GpuArenaRange) -> Result<GpuArenaRange> {
        let live = self.live.get(&range.first).ok_or_else(|| {
            Error::InvalidUploadData("global GPU range is not a live allocation".to_owned())
        })?;
        if live.logical_count != range.count {
            return Err(Error::InvalidUploadData(
                "global GPU range does not match its live allocation".to_owned(),
            ));
        }
        Ok(GpuArenaRange {
            first: range.first,
            count: live.reserved_count,
        })
    }

    pub(super) fn insert_free(&mut self, range: GpuArenaRange) {
        let mut first = range.first;
        let mut count = range.count;
        if let Some((&previous_first, &previous_count)) = self.free.range(..first).next_back()
            && previous_first.checked_add(previous_count) == Some(first)
        {
            self.free.remove(&previous_first);
            first = previous_first;
            count = count
                .checked_add(previous_count)
                .expect("valid GPU ranges cannot overflow while coalescing");
        }
        if let Some((&next_first, &next_count)) = self.free.range(first..).next()
            && first.checked_add(count) == Some(next_first)
        {
            self.free.remove(&next_first);
            count = count
                .checked_add(next_count)
                .expect("valid GPU ranges cannot overflow while coalescing");
        }
        self.free.insert(first, count);
    }
}

pub(super) struct RetiredBuffer {
    pub(super) _buffer: Buffer,
    pub(super) pending_frame_slots: u64,
}

/// Device-local growable arena that preserves superseded allocations for in-flight frames.
///
/// Every addressable byte reads as zero until a staged write covers it: creation fills the
/// fresh buffer, and a growth op zero-fills the tail beyond the preserved prefix. Without
/// that, a capacity-wide dispatch scanning slot headers reads whatever the recycled device
/// memory last held — garbage occupancy words whose record contents become wild device
/// addresses.
pub struct GlobalGpuArena<K> {
    pub(super) buffer: Buffer,
    pub(super) element_stride: u64,
    pub(super) capacity: u64,
    pub(super) usage: vk::BufferUsageFlags,
    pub(super) retired: Vec<RetiredBuffer>,
    pub(super) ranges: GpuRangeAllocator,
    pub(super) marker: PhantomData<K>,
}

impl<K> GlobalGpuArena<K> {
    /// Creates one device-local, BDA-addressable global arena.
    pub fn new(device: &Device, element_stride: u64, initial_elements: u64) -> Result<Self> {
        if element_stride == 0 {
            return Err(Error::InvalidUploadData(
                "global GPU arena stride must be nonzero".to_owned(),
            ));
        }
        let required_bytes = element_stride
            .checked_mul(initial_elements.max(1))
            .ok_or_else(|| {
                Error::InvalidUploadData("global GPU arena byte size overflow".to_owned())
            })?;
        let bytes = next_capacity(0, required_bytes, INITIAL_ARENA_BYTES)?;
        let usage = vk::BufferUsageFlags::STORAGE_BUFFER
            | vk::BufferUsageFlags::TRANSFER_DST
            | vk::BufferUsageFlags::TRANSFER_SRC
            | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS
            | vk::BufferUsageFlags::VERTEX_BUFFER
            | vk::BufferUsageFlags::INDEX_BUFFER
            | vk::BufferUsageFlags::INDIRECT_BUFFER;
        let allocation = vk_mem::AllocationCreateInfo {
            usage: vk_mem::MemoryUsage::AutoPreferDevice,
            ..Default::default()
        };
        let buffer = Buffer::new(device.resources(), bytes, usage, &allocation)?;
        // The freshly allocated device memory holds whatever it last held; fill it before any
        // address escapes, so unstaged bytes read as zero. Synchronous, but only at arena
        // creation (renderer init and first use of a world).
        let handle = buffer.handle();
        device.one_shot_transfer(|raw, cmd| {
            // SAFETY: the ash seam. The buffer was just created with TRANSFER_DST and nothing
            // references it yet.
            unsafe { raw.cmd_fill_buffer(cmd, handle, 0, vk::WHOLE_SIZE, 0) };
        })?;
        Ok(Self {
            buffer,
            element_stride,
            capacity: bytes / element_stride,
            usage,
            retired: Vec::new(),
            ranges: GpuRangeAllocator::default(),
            marker: PhantomData,
        })
    }

    /// Allocates a range and returns whether the physical buffer must grow before upload.
    pub fn allocate(
        &mut self,
        count: u32,
        element_alignment: u32,
    ) -> Result<(GpuArenaRange, bool)> {
        let (reserved_count, physical_alignment) =
            physical_range_layout(count, self.element_stride, element_alignment)?;
        let range = self
            .ranges
            .allocate_reserved(count, reserved_count, physical_alignment)?;
        Ok((range, self.ranges.required_capacity() > self.capacity))
    }

    /// Stages the exact bytes of one allocated range for a graph-owned transfer pass.
    pub fn stage(
        &self,
        uploads: &mut FrameUploadRing,
        frame_slot: usize,
        range: GpuArenaRange,
        bytes: &[u8],
    ) -> Result<GpuBufferUpload> {
        let expected = u64::from(range.count)
            .checked_mul(self.element_stride)
            .ok_or_else(|| {
                Error::InvalidUploadData("global GPU range byte size overflow".to_owned())
            })?;
        if bytes.len() as u64 != expected {
            return Err(Error::InvalidUploadData(format!(
                "global GPU range expects {expected} upload bytes, got {}",
                bytes.len()
            )));
        }
        let reserved_range = self.ranges.reserved_range(range)?;
        if reserved_range.end() > self.capacity {
            return Err(Error::InvalidUploadData(
                "global GPU arena must grow before staging its allocated range".to_owned(),
            ));
        }
        let destination_offset = self.byte_offset(range)?;
        let physical_size = u64::from(reserved_range.count)
            .checked_mul(self.element_stride)
            .ok_or_else(|| {
                Error::InvalidUploadData("global GPU physical byte size overflow".to_owned())
            })?;
        if destination_offset % GPU_COPY_ALIGNMENT != 0 || physical_size % GPU_COPY_ALIGNMENT != 0 {
            return Err(Error::InvalidUploadData(
                "global GPU copy offsets and sizes must be four-byte aligned".to_owned(),
            ));
        }
        let padded;
        let staging_bytes = if physical_size == expected {
            bytes
        } else {
            let physical_size = usize::try_from(physical_size).map_err(|_| {
                Error::InvalidUploadData(
                    "global GPU physical upload exceeds address space".to_owned(),
                )
            })?;
            padded = {
                let mut padded = vec![0_u8; physical_size];
                padded[..bytes.len()].copy_from_slice(bytes);
                padded
            };
            &padded
        };
        let source = uploads.write_bytes(frame_slot, staging_bytes, 16)?;
        Ok(GpuBufferUpload {
            source: source.buffer,
            destination: self.buffer(),
            source_offset: source.offset,
            destination_offset,
            size: source.size,
            source_size: source.capacity,
            destination_size: self.buffer.size(),
        })
    }

    /// Defers reuse of a logical range until all frame slots have crossed their fences.
    pub fn retire(&mut self, range: GpuArenaRange) -> Result<()> {
        self.ranges.retire(range)
    }

    /// Grows the physical buffer and returns the graph-owned op that preserves its live prefix
    /// and zero-fills everything beyond it.
    pub fn prepare_growth(&mut self, device: &Device) -> Result<Option<GpuArenaGrowth>> {
        let required_elements = self.ranges.required_capacity();
        if required_elements <= self.capacity {
            return Ok(None);
        }
        let required_bytes = required_elements
            .checked_mul(self.element_stride)
            .ok_or_else(|| {
                Error::InvalidUploadData("global GPU arena byte size overflow".to_owned())
            })?;
        let bytes = next_capacity(self.buffer.size(), required_bytes, INITIAL_ARENA_BYTES)?;
        let allocation = vk_mem::AllocationCreateInfo {
            usage: vk_mem::MemoryUsage::AutoPreferDevice,
            ..Default::default()
        };
        let replacement = Buffer::new(device.resources(), bytes, self.usage, &allocation)?;
        let growth = GpuArenaGrowth {
            source: self.buffer.handle(),
            destination: replacement.handle(),
            size: self.buffer.size(),
            source_size: self.buffer.size(),
            destination_size: replacement.size(),
        };
        let old = std::mem::replace(&mut self.buffer, replacement);
        self.capacity = bytes / self.element_stride;
        self.retired.push(RetiredBuffer {
            _buffer: old,
            pending_frame_slots: live_frame_mask(),
        });
        Ok(Some(growth))
    }

    /// Reclaims logical ranges and old physical allocations after a frame fence signals.
    pub fn begin_frame(&mut self, completed_frame_slot: usize) -> Result<()> {
        self.ranges.begin_frame(completed_frame_slot)?;
        let completed = frame_slot_bit(completed_frame_slot)?;
        for retired in &mut self.retired {
            retired.pending_frame_slots &= !completed;
        }
        self.retired
            .retain(|retired| retired.pending_frame_slots != 0);
        Ok(())
    }

    /// Current Vulkan buffer.
    pub fn buffer(&self) -> vk::Buffer {
        self.buffer.handle()
    }

    /// Current buffer device address used by vertex pulling and immutable tables.
    pub fn address(&self, device: &Device) -> vk::DeviceAddress {
        device.buffer_device_address(self.buffer.handle())
    }

    /// Byte offset of a logical range.
    pub fn byte_offset(&self, range: GpuArenaRange) -> Result<u64> {
        arena_byte_offset(range.first, self.element_stride)
    }

    /// Current physical element capacity.
    pub fn capacity(&self) -> u64 {
        self.capacity
    }

    /// Number of old allocations retained for live frame slots.
    pub fn retired_allocation_count(&self) -> usize {
        self.retired.len()
    }
}
