//! Stable handles, global GPU arenas, immutable tables, and frame-safe uploads.

use std::collections::BTreeMap;
use std::marker::PhantomData;
use std::sync::Arc;

use ash::vk;
use bytemuck::{Pod, Zeroable};
use saffron_material::{
    AlphaClassification, CoverageMipMetadata, CoverageSource, OpacityMicromapDerivation,
    SurfaceModel,
};

use crate::frame::MAX_FRAMES_IN_FLIGHT;
use crate::gpu_types::MaterialParamsData;
use crate::render_graph::{
    RenderGraph, RgBufferLifetime, RgBufferRange, RgBufferResource, RgPass, RgUsage,
};
use crate::resources::{Buffer, DeviceResources};
use crate::{Device, Error, Result};

const INITIAL_ARENA_BYTES: u64 = 64 * 1024;
const INITIAL_UPLOAD_BYTES: u64 = 256 * 1024;
const GPU_COPY_ALIGNMENT: u64 = 4;

/// Version of the byte-locked Rust/Slang global GPU data ABI.
pub const GLOBAL_GPU_DATA_ABI_VERSION: u32 = 1;
/// PSO-bin shift for geometry representation.
pub const GPU_PSO_REPRESENTATION_SHIFT: u32 = 0;
/// PSO-bin shift for the pass-independent material class.
pub const GPU_PSO_MATERIAL_SHIFT: u32 = 2;
/// PSO-bin shift for canonical coverage classification.
pub const GPU_PSO_COVERAGE_SHIFT: u32 = 2;
/// PSO-bin shift for raster sidedness.
pub const GPU_PSO_SIDEDNESS_SHIFT: u32 = 4;
/// PSO-bin shift for the canonical material surface model.
pub const GPU_PSO_SURFACE_MODEL_SHIFT: u32 = 5;
/// PSO-bin shift for transparency compositing.
pub const GPU_PSO_TRANSPARENCY_SHIFT: u32 = 6;
/// PSO-bin shift for deformation provider.
pub const GPU_PSO_DEFORMATION_SHIFT: u32 = 8;
/// PSO-bin shift for pass class.
pub const GPU_PSO_PASS_SHIFT: u32 = 9;
/// Material-class shift for canonical coverage.
pub const GPU_MATERIAL_COVERAGE_SHIFT: u32 = 0;
/// Material-class shift for sidedness.
pub const GPU_MATERIAL_SIDEDNESS_SHIFT: u32 = 2;
/// Material-class shift for the surface model.
pub const GPU_MATERIAL_SURFACE_MODEL_SHIFT: u32 = 3;
/// Material-class shift for transparency.
pub const GPU_MATERIAL_TRANSPARENCY_SHIFT: u32 = 4;
/// Unlit shading bit within [`GpuMaterialClass`].
pub const GPU_MATERIAL_UNLIT_SHIFT: u32 = 5;

fn live_frame_mask() -> u64 {
    (1_u64 << MAX_FRAMES_IN_FLIGHT) - 1
}

fn frame_slot_bit(frame_slot: usize) -> Result<u64> {
    if frame_slot >= MAX_FRAMES_IN_FLIGHT || frame_slot >= u64::BITS as usize {
        return Err(Error::InvalidUploadData(format!(
            "frame slot {frame_slot} exceeds the {MAX_FRAMES_IN_FLIGHT}-slot GPU lifetime ring"
        )));
    }
    Ok(1_u64 << frame_slot)
}

fn align_up(value: u64, alignment: u64, subject: &'static str) -> Result<u64> {
    if alignment == 0 || !alignment.is_power_of_two() {
        return Err(Error::InvalidUploadData(format!(
            "{subject} alignment must be a nonzero power of two"
        )));
    }
    value
        .checked_add(alignment - 1)
        .map(|aligned| aligned & !(alignment - 1))
        .ok_or_else(|| Error::InvalidUploadData(format!("{subject} alignment overflow")))
}

fn greatest_common_divisor(mut left: u64, mut right: u64) -> u64 {
    while right != 0 {
        (left, right) = (right, left % right);
    }
    left
}

fn physical_range_layout(
    logical_count: u32,
    element_stride: u64,
    requested_alignment: u32,
) -> Result<(u32, u32)> {
    if element_stride == 0 {
        return Err(Error::InvalidUploadData(
            "global GPU arena stride must be nonzero".to_owned(),
        ));
    }
    if requested_alignment == 0 || !requested_alignment.is_power_of_two() {
        return Err(Error::InvalidUploadData(
            "global GPU range alignment must be a nonzero power of two".to_owned(),
        ));
    }
    let copy_elements =
        GPU_COPY_ALIGNMENT / greatest_common_divisor(element_stride, GPU_COPY_ALIGNMENT);
    let reserved = align_up(
        u64::from(logical_count),
        copy_elements,
        "global GPU physical range",
    )?;
    let reserved = u32::try_from(reserved).map_err(|_| {
        Error::InvalidUploadData("global GPU physical range exceeds u32 elements".to_owned())
    })?;
    let copy_alignment = u32::try_from(copy_elements).map_err(|_| {
        Error::InvalidUploadData("global GPU copy alignment exceeds u32 elements".to_owned())
    })?;
    Ok((reserved, requested_alignment.max(copy_alignment)))
}

fn next_capacity(current: u64, needed: u64, floor: u64) -> Result<u64> {
    let mut capacity = current.max(floor);
    while capacity < needed {
        capacity = capacity.checked_mul(2).ok_or_else(|| {
            Error::InvalidUploadData("global GPU arena capacity overflow".to_owned())
        })?;
    }
    Ok(capacity)
}

fn arena_byte_offset(first: u32, element_stride: u64) -> Result<u64> {
    u64::from(first)
        .checked_mul(element_stride)
        .ok_or_else(|| Error::InvalidUploadData("global GPU byte offset overflow".to_owned()))
}

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

struct HandleSlot<T> {
    generation: u32,
    value: Option<T>,
}

struct RetiredHandle {
    index: u32,
    pending_frame_slots: u64,
}

/// Immutable-record table with generation checks and fence-deferred slot reuse.
pub struct ImmutableGpuTable<T> {
    slots: Vec<HandleSlot<T>>,
    reusable: Vec<u32>,
    retired: Vec<RetiredHandle>,
    live: usize,
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

    fn next_insert_index(&self) -> Result<(u32, bool)> {
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

struct RetiredRange {
    range: GpuArenaRange,
    pending_frame_slots: u64,
}

#[derive(Clone, Copy)]
struct LiveRange {
    logical_count: u32,
    reserved_count: u32,
}

/// Best-fit range allocator whose freed ranges become reusable only after all frame fences pass.
#[derive(Default)]
pub struct GpuRangeAllocator {
    high_water: u64,
    live: BTreeMap<u32, LiveRange>,
    free: BTreeMap<u32, u32>,
    retired: Vec<RetiredRange>,
}

impl GpuRangeAllocator {
    /// Reserves a stable contiguous range.
    pub fn allocate(&mut self, count: u32, alignment: u32) -> Result<GpuArenaRange> {
        self.allocate_reserved(count, count, alignment)
    }

    fn allocate_reserved(
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

    fn reserved_range(&self, range: GpuArenaRange) -> Result<GpuArenaRange> {
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

    fn insert_free(&mut self, range: GpuArenaRange) {
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

struct RetiredBuffer {
    _buffer: Buffer,
    pending_frame_slots: u64,
}

/// Device-local growable arena that preserves superseded allocations for in-flight frames.
pub struct GlobalGpuArena<K> {
    buffer: Buffer,
    element_stride: u64,
    capacity: u64,
    usage: vk::BufferUsageFlags,
    retired: Vec<RetiredBuffer>,
    ranges: GpuRangeAllocator,
    marker: PhantomData<K>,
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

    /// Grows the physical buffer and returns the graph-owned copy that preserves its live prefix.
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
    source: vk::Buffer,
    /// Device-local destination buffer.
    destination: vk::Buffer,
    /// Staging-buffer byte offset.
    source_offset: u64,
    /// Destination-buffer byte offset.
    destination_offset: u64,
    /// Number of bytes to copy.
    size: u64,
    /// Total addressable size of the source buffer.
    source_size: u64,
    /// Total addressable size of the destination buffer.
    destination_size: u64,
}

/// Copy plan returned when a global device-local arena grows.
#[must_use]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GpuArenaGrowth {
    /// Superseded arena buffer retained for in-flight frames.
    source: vk::Buffer,
    /// New larger arena buffer.
    destination: vk::Buffer,
    /// Live prefix to preserve.
    size: u64,
    /// Total addressable size of the source buffer.
    source_size: u64,
    /// Total addressable size of the destination buffer.
    destination_size: u64,
}

impl GpuArenaGrowth {
    /// Enqueues the preservation copy as a fully declared render-graph transfer pass.
    pub fn enqueue(self, graph: &mut RenderGraph, device: &Device, name: impl Into<String>) {
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
        let destination_range =
            RgBufferRange::new(0, self.size).expect("validated global arena growth copy range");
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
                    }
                }),
        );
    }
}

impl GpuBufferUpload {
    /// Enqueues this upload as a fully declared render-graph transfer pass.
    pub fn enqueue(self, graph: &mut RenderGraph, device: &Device, name: impl Into<String>) {
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

struct UploadFrame {
    buffer: Buffer,
    cursor: u64,
    retired: Vec<Buffer>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct UploadPlan {
    offset: u64,
    end: u64,
    required_capacity: u64,
}

fn plan_upload(cursor: u64, capacity: u64, size: u64, alignment: u64) -> Result<UploadPlan> {
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
    resources: Arc<DeviceResources>,
    frames: Vec<UploadFrame>,
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

fn create_upload_buffer(resources: &Arc<DeviceResources>, bytes: u64) -> Result<Buffer> {
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
    records: ImmutableGpuTable<T>,
    storage: GlobalGpuArena<K>,
    slot_stride: u64,
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

    fn stage_slot(
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

/// Immutable prototype-table record shared by every world.
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
#[repr(C, align(16))]
pub struct GpuPrototypeRecord {
    /// Geometry-table handle.
    pub geometry: GpuHandle,
    /// Range of [`GpuHandle`] elements in [`GlobalGpuData::prototype_materials`].
    pub material_range: GpuArenaRange,
    /// Skeleton-table handle or [`GpuHandle::INVALID`].
    pub skeleton: GpuHandle,
    /// Guaranteed-resident root page.
    pub root_page: GpuHandle,
    /// Conservative object-space bounding sphere.
    pub bounds: [f32; 4],
    /// Prototype flags.
    pub flags: u32,
    /// Reserved ABI words.
    pub reserved: [u32; 3],
}

/// Immutable geometry-table record addressing global arenas.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Pod, Zeroable)]
#[repr(C, align(8))]
pub struct GpuGeometryRecord {
    /// Vertex arena range.
    pub vertices: GpuArenaRange,
    /// Index arena range.
    pub indices: GpuArenaRange,
    /// Cluster arena range.
    pub clusters: GpuArenaRange,
    /// Assembly-part arena range.
    pub parts: GpuArenaRange,
    /// Aggregate-voxel arena range.
    pub voxels: GpuArenaRange,
    /// Submesh-record range in [`GlobalGpuData::submesh_table`].
    pub submeshes: GpuArenaRange,
    /// Geometry format and topology flags.
    pub flags: u32,
    /// Vertex stride in bytes.
    pub vertex_stride: u32,
    /// Index stride in bytes.
    pub index_stride: u32,
    /// Reserved ABI word.
    pub reserved: u32,
}

/// The parts range's fixed header: the table split the shaders derive offsets from.
/// The range lays this header first, then the prototype table, the use records, and
/// the per-combination active-use mask words; the prototype count also rides
/// [`GpuGeometryRecord::reserved`].
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Pod, Zeroable)]
pub struct GpuAssemblyHeaderRecord {
    /// Prototype records following the header.
    pub prototype_count: u32,
    /// Use records following the prototypes.
    pub use_count: u32,
    /// Mask words per combination (`ceil(use_count / 32)`).
    pub mask_words: u32,
    /// Combinations in the mask table.
    pub combination_count: u32,
}

const _: () = assert!(size_of::<GpuAssemblyHeaderRecord>() == 16);

/// One assembly prototype's entry in a geometry's parts range: its slice of the use
/// records and its base vertex within the geometry's flattened vertex range. The parts
/// range lays the header first, then the prototype table, then every use record; the
/// prototype count rides [`GpuGeometryRecord::reserved`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Pod, Zeroable)]
#[repr(C, align(4))]
pub struct GpuAssemblyPrototypeRecord {
    /// First use record (global across the geometry's prototypes).
    pub first_use: u32,
    /// Number of uses placing this prototype.
    pub use_count: u32,
    /// The prototype's base vertex within the geometry's vertex range.
    pub vertex_base: u32,
    /// Reserved ABI word.
    pub reserved: u32,
}

const _: () = assert!(size_of::<GpuAssemblyPrototypeRecord>() == 16);

/// One assembly use: the prototype it places and its family-local transform (rows 0-2
/// of the row-major matrix; the implicit last row is `[0, 0, 0, 1]`). The visibility
/// traversal emits one draw record per use of a cut node's prototype, and the executor
/// vertex path premultiplies the use transform before the instance transform.
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
#[repr(C, align(4))]
pub struct GpuAssemblyUseRecord {
    /// Rows 0-2 of the row-major family-local transform.
    pub transform: [f32; 12],
    /// The placed prototype.
    pub prototype: u32,
    /// Reserved ABI words.
    pub reserved: [u32; 3],
}

const _: () = assert!(size_of::<GpuAssemblyUseRecord>() == 64);

/// `GpuDrawRecord::clusterState`: the record draws outside any assembly (an ordinary
/// mesh, or a family node spanning prototypes).
pub const GPU_ASSEMBLY_NO_USE: u32 = u32::MAX;

/// Frames one representation crossfade sweeps: the traversal advances a flip node's
/// phase once per frame until it reaches this total and the transition settles.
pub const GPU_TRANSITION_FRAMES: u32 = 16;

/// Instance-flag shift of the two-bit vegetation interaction policy.
pub const GPU_SCENE_INSTANCE_POLICY_SHIFT: u32 = 1;
/// Instance flag: the static payload carries explicit conservative bounds spheres
/// (words 16..24) the cull uses instead of the prototype sphere.
pub const GPU_SCENE_INSTANCE_FLAG_EXPLICIT_BOUNDS: u32 = 8;
/// Instance flag: the static payload carries a surface attachment (words 24..30).
pub const GPU_SCENE_INSTANCE_FLAG_ATTACHED: u32 = 16;
/// Instance flag: the wind deformation prepass writes this instance's sway record,
/// every raster pass applies it, and the visibility cull adds its bounds slack.
pub const GPU_SCENE_INSTANCE_FLAG_WIND: u32 = 32;

/// One wind-deformed instance's prepass output: the full sway displacement at the
/// instance's bounds top for the current and previous frame times, the reciprocal
/// of the local bounds-top height, and the world-space cull slack covering both
/// sways. The device buffer holds one record per instance slot.
#[repr(C, align(16))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct GpuWindInstanceRecord {
    /// World-space sway at the bounds top, current frame time.
    pub sway_current: [f32; 3],
    /// Reciprocal of the instance-local bounds-top height.
    pub height_scale: f32,
    /// World-space sway at the bounds top, previous frame time.
    pub sway_previous: [f32; 3],
    /// Cull slack in metres: the larger sway magnitude plus the larger interaction
    /// magnitude.
    pub bounds_inflation: f32,
    /// World interaction-field displacement at the root, current frame.
    pub interaction_current: [f32; 3],
    /// Reserved ABI word.
    pub reserved0: f32,
    /// The previous frame's interaction displacement (the field is stateful, so the
    /// prepass carries it forward from the record rather than recomputing).
    pub interaction_previous: [f32; 3],
    /// Reserved ABI word.
    pub reserved1: f32,
    /// Branch-mode quadrature (sin, cos of the mode angle) at the current and the
    /// previous frame's time — a per-use phase offset applies as
    /// `sin(ωt+φ) = s·cosφ + c·sinφ`, so time never reaches the vertex path.
    pub branch_quadrature: [f32; 4],
    /// Branch-mode amplitude in metres at the bounds top.
    pub branch_amplitude: f32,
    /// Leaf-flutter amplitude in metres.
    pub flutter_amplitude: f32,
    /// Reserved ABI words.
    pub reserved2: [f32; 2],
}

const _: () = assert!(size_of::<GpuWindInstanceRecord>() == 96);

/// Cascades of the world interaction field.
pub const GPU_INTERACTION_CASCADES: u32 = 2;
/// Texels per interaction-cascade side.
pub const GPU_INTERACTION_TEXELS: u32 = 256;
/// Byte size of one interaction texel.
pub const GPU_INTERACTION_TEXEL_SIZE: u32 = 32;
/// Byte size of the interaction field's header.
pub const GPU_INTERACTION_HEADER_SIZE: u32 = 32;

/// Total byte size of one world's interaction field buffer (header + cascades).
pub const GPU_INTERACTION_FIELD_BYTES: u64 = GPU_INTERACTION_HEADER_SIZE as u64
    + GPU_INTERACTION_CASCADES as u64
        * GPU_INTERACTION_TEXELS as u64
        * GPU_INTERACTION_TEXELS as u64
        * GPU_INTERACTION_TEXEL_SIZE as u64;

/// One resident micro field tile's header in the fields arena. The header is followed
/// by `sample_count` `u16` density samples (padded to a four-byte boundary), then
/// `attribute_count` typed channels, each a 16-byte channel id plus `sample_count`
/// `i32` values. The tile grid spans its owner cell's bounds.
#[repr(C, align(8))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Pod, Zeroable)]
pub struct GpuFieldTileRecord {
    /// Signed level-zero owner-cell coordinates.
    pub cell: [i64; 3],
    /// Density-grid dimensions.
    pub dims: [u32; 3],
    /// Density samples in the grid.
    pub sample_count: u32,
    /// The 128-bit cosmetic reconstruction seed as four little-endian words.
    pub seed: [u32; 4],
    /// Typed attribute channels following the density samples.
    pub attribute_count: u32,
    /// Reserved ABI word.
    pub reserved: u32,
}

const _: () = assert!(size_of::<GpuFieldTileRecord>() == 64);

/// `GpuSceneInstanceRecord::flags` bit: the instance anchors a micro vegetation field.
/// The visibility cull skips it (field tiles cull per texel in the micro pass); only
/// micro-blade records reference it.
pub const GPU_SCENE_INSTANCE_FLAG_MICRO_FIELD: u32 = 1;

/// Vertices in the shared micro-blade template: a four-segment tapered strip the
/// executor derives procedurally per candidate (no vertex data exists — the template
/// only enumerates triangle corners).
pub const MICRO_BLADE_VERTEX_COUNT: u32 = 10;

/// Indices in the shared micro-blade template (eight triangles over the strip).
pub const MICRO_BLADE_INDEX_COUNT: u32 = 24;

/// The template's triangle corners: per strip segment, two counter-clockwise
/// triangles over vertex pairs `(2i, 2i+1)` → `(2i+2, 2i+3)`.
#[must_use]
pub fn micro_blade_template_indices() -> Vec<u32> {
    let mut indices = Vec::with_capacity(MICRO_BLADE_INDEX_COUNT as usize);
    for segment in 0..4_u32 {
        let base = segment * 2;
        indices.extend_from_slice(&[base, base + 1, base + 2]);
        indices.extend_from_slice(&[base + 1, base + 3, base + 2]);
    }
    indices
}

/// One generated micro-blade candidate: the frame-transient placement the executor
/// vertex paths reconstruct the blade from (`GpuDrawRecord::content_index` slots).
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct GpuMicroCandidate {
    /// World-space root position.
    pub position: [f32; 3],
    /// Blade height in metres.
    pub height: f32,
    /// Facing yaw in radians.
    pub yaw: f32,
    /// Blade width in metres.
    pub width: f32,
    /// Phenotype selector.
    pub phenotype: u32,
    /// Reserved ABI word.
    pub reserved: u32,
    /// Horizontal analytic wind bend (x, z) at the current frame time.
    pub wind_bend: [f32; 2],
    /// Horizontal analytic wind bend (x, z) at the previous frame time.
    pub wind_bend_previous: [f32; 2],
}

const _: () = assert!(size_of::<GpuMicroCandidate>() == 48);

/// One resident micro field tile's directory entry: the tile's byte offset in the
/// fields arena and the per-family field instance its blade records reference.
#[repr(C, align(8))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Pod, Zeroable)]
pub struct GpuFieldDirectoryEntry {
    /// The per-family field instance (index, generation).
    pub instance: GpuHandle,
    /// The tile header's byte offset within the fields arena.
    pub tile_offset: u32,
    /// Cooked density upper bound of the tile's blade candidates (no view term).
    pub predicted: u32,
}

const _: () = assert!(size_of::<GpuFieldDirectoryEntry>() == 16);

/// One submesh of a geometry: its index-range slice and the prototype material slot it
/// draws with.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Pod, Zeroable)]
#[repr(C, align(8))]
pub struct GpuSubmeshRecord {
    /// First index within the geometry's index range.
    pub first_index: u32,
    /// Number of indices.
    pub index_count: u32,
    /// Prototype material slot.
    pub material_slot: u32,
    /// Reserved ABI word.
    pub reserved: u32,
}

/// Immutable bindless material-table record.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Pod, Zeroable)]
#[repr(C, align(8))]
pub struct GpuMaterialTableRecord {
    /// Base-color texture table handle.
    pub base_color_texture: GpuHandle,
    /// Normal texture table handle.
    pub normal_texture: GpuHandle,
    /// Canonical coverage-table handle.
    pub coverage: GpuHandle,
    /// Index into [`GlobalGpuData::material_parameters`].
    pub parameter_index: u32,
    /// Pass-independent immutable material pipeline dimensions.
    pub material_class: GpuMaterialClass,
    /// Executor shader identity: 0 is the engine übershader, nonzero indexes the
    /// renderer's registered codegen shader table.
    pub shader_index: u32,
    /// [`GPU_MATERIAL_TABLE_FLAG_TESSELLATED`] and future material flags.
    pub flags: u32,
}

/// The executor shader registry: gives [`GpuMaterialTableRecord::shader_index`] its
/// meaning. Index 0 is the engine übershader; nonzero indices are codegen material
/// shaders registered when the mirror interns them.
#[derive(Debug)]
pub struct ExecutorShaderRegistry {
    shaders: Vec<String>,
}

impl Default for ExecutorShaderRegistry {
    fn default() -> Self {
        Self {
            shaders: vec!["shaders/mesh.spv".to_owned()],
        }
    }
}

impl ExecutorShaderRegistry {
    /// Registers `shader`, returning its stable index (existing entries dedup).
    pub fn register(&mut self, shader: &str) -> u32 {
        if let Some(index) = self.shaders.iter().position(|entry| entry == shader) {
            return index as u32;
        }
        self.shaders.push(shader.to_owned());
        (self.shaders.len() - 1) as u32
    }

    /// The shader path behind `index` (the übershader for an unknown index).
    #[must_use]
    pub fn get(&self, index: u32) -> &str {
        self.shaders
            .get(index as usize)
            .map_or("shaders/mesh.spv", String::as_str)
    }
}

/// Immutable texture-table record.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Pod, Zeroable)]
#[repr(C)]
pub struct GpuTextureTableRecord {
    /// Bindless descriptor-array slot.
    pub descriptor_index: u32,
    /// Width in texels.
    pub width: u32,
    /// Height in texels.
    pub height: u32,
    /// Mip count.
    pub mip_count: u32,
    /// Texture format/classification flags.
    pub flags: u32,
    /// Reserved ABI words.
    pub reserved: [u32; 3],
}

/// Immutable coverage-table record used by raster, picking, shadows, and ray hits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
#[repr(C, align(8))]
pub struct GpuCoverageRecord {
    /// Coverage texture handle.
    pub texture: GpuHandle,
    /// Alpha cutoff.
    pub cutoff: f32,
    /// Canonical [`AlphaClassification`] value.
    pub classification: u32,
    /// Canonical [`CoverageSource`] kind.
    pub source_kind: u32,
    /// Optional OMM permission and subdivision policy; coverage correctness never depends on it.
    pub omm_policy: u32,
    /// Stable object-space hash salt.
    pub hash_salt: [u32; 2],
    /// Source texture extent.
    pub source_extent: [u32; 2],
    /// Packed transparent/opaque OMM thresholds as two canonical u16 values.
    pub omm_thresholds: u32,
    /// Reserved ABI word.
    pub reserved: u32,
}

impl GpuCoverageRecord {
    /// Derives the sole raster, shadow, picking, voxel, and ray-hit coverage contract.
    #[must_use]
    pub fn from_metadata(
        texture: GpuHandle,
        source: &CoverageSource,
        metadata: &CoverageMipMetadata,
        opacity_micromap: OpacityMicromapDerivation,
    ) -> Self {
        let classification = metadata.classification as u32;
        let source_kind = match source {
            CoverageSource::AlbedoAlpha => 0,
            CoverageSource::Texture(_) => 1,
            CoverageSource::ModeledGeometry => 2,
        };
        let omm_policy = u32::from(opacity_micromap.enabled)
            | (u32::from(opacity_micromap.max_subdivision) << 8);
        let omm_thresholds = u32::from(opacity_micromap.transparent_threshold.bits())
            | (u32::from(opacity_micromap.opaque_threshold.bits()) << 16);
        Self {
            texture,
            cutoff: f32::from(metadata.reference_cutoff.bits()) / f32::from(u16::MAX),
            classification,
            source_kind,
            omm_policy,
            hash_salt: [
                metadata.spatial_hash_salt as u32,
                (metadata.spatial_hash_salt >> 32) as u32,
            ],
            source_extent: metadata.source_extent,
            omm_thresholds,
            reserved: 0,
        }
    }
}

/// Immutable skeleton-table record.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Pod, Zeroable)]
#[repr(C, align(8))]
pub struct GpuSkeletonRecord {
    /// Range in [`GlobalGpuData::skeleton_joints`].
    pub joints: GpuArenaRange,
    /// Range in [`GlobalGpuData::inverse_binds`].
    pub inverse_binds: GpuArenaRange,
    /// Range in [`GlobalGpuData::deformation_providers`].
    pub deformation: GpuArenaRange,
    /// Skeleton flags.
    pub flags: u32,
    /// Reserved ABI word.
    pub reserved: u32,
}

/// One skeleton joint in the global joint-hierarchy arena.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Pod, Zeroable)]
#[repr(C, align(8))]
pub struct GpuSkeletonJointRecord {
    /// Parent joint index, or `u32::MAX` for a root.
    pub parent: u32,
    /// Stable joint flags.
    pub flags: u32,
}

/// One column-major inverse-bind matrix in the global inverse-bind arena.
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
#[repr(C, align(16))]
pub struct GpuInverseBindRecord {
    /// Column-major 4x4 matrix.
    pub matrix: [f32; 16],
}

/// [`GpuDeformationProviderRecord::provider_mask`]: compute skinning (joint palettes
/// over the skin stream).
pub const GPU_DEFORMATION_PROVIDER_SKINNING: u32 = 1 << 0;
/// [`GpuDeformationProviderRecord::provider_mask`]: morph-target blending (before skin).
pub const GPU_DEFORMATION_PROVIDER_MORPH: u32 = 1 << 1;
/// [`GpuDeformationProviderRecord::provider_mask`]: material height displacement through
/// the tessellation seam.
pub const GPU_DEFORMATION_PROVIDER_DISPLACEMENT: u32 = 1 << 2;
/// [`GpuDeformationProviderRecord::provider_mask`]: wind sway sampled from the shared
/// deterministic wind field.
pub const GPU_DEFORMATION_PROVIDER_WIND: u32 = 1 << 3;
/// [`GpuDeformationProviderRecord::provider_mask`]: the world interaction field
/// (impulse displacement with damped recovery).
pub const GPU_DEFORMATION_PROVIDER_INTERACTION: u32 = 1 << 4;

/// One composed deformation-provider chain in the global deformation arena.
///
/// The contract every provider composes through: current AND previous outputs
/// (vertices or transforms) so motion vectors read the same deformation the passes
/// draw, cluster-tight swept bounds for visibility, and optional BLAS inputs for the
/// ray-traced mirror. `provider_mask` names the composed providers; the parameter
/// words at `first_parameter` belong to them in mask-bit order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Pod, Zeroable)]
#[repr(C, align(16))]
pub struct GpuDeformationProviderRecord {
    /// Provider-presence bits; composition order is defined by the deformation system.
    pub provider_mask: u32,
    /// First word in [`GlobalGpuData::deformation_parameters`].
    pub first_parameter: u32,
    /// Number of provider parameter words.
    pub parameter_count: u32,
    /// Deformation-output flags.
    pub flags: u32,
}

/// Page-record flag: the page is a guaranteed root that stays drawable under every
/// residency pressure condition.
pub const GPU_PAGE_FLAG_GUARANTEED_ROOT: u32 = 1 << 0;

/// [`GpuMaterialTableRecord::flags`]: the material displaces real geometry
/// (`HeightMode::Displacement` with a bound height map). The visibility traversal
/// skips records carrying it while the tessellation seam is active — the amplified
/// transient geometry draws through the tess indirect draws instead.
pub const GPU_MATERIAL_TABLE_FLAG_TESSELLATED: u32 = 1 << 0;

/// Immutable page-table record with parent-before-child residency.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Pod, Zeroable)]
#[repr(C, align(8))]
pub struct GpuPageRecord {
    /// Parent page or [`GpuHandle::INVALID`] for a root.
    pub parent: GpuHandle,
    /// Range of [`GpuHandle`] elements in [`GlobalGpuData::page_dependencies`].
    pub dependencies: GpuArenaRange,
    /// Byte offset in the global page arena.
    pub byte_offset: u64,
    /// Page byte length.
    pub byte_length: u32,
    /// Resident generation.
    pub resident_generation: u32,
    /// Page flags.
    pub flags: u32,
    /// Reserved ABI word.
    pub reserved: u32,
}

/// Semantic draw record produced by visibility and consumed by every executor.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Pod, Zeroable)]
#[repr(C, align(8))]
pub struct GpuDrawRecord {
    /// Geometry-table handle.
    pub geometry: GpuHandle,
    /// Material-table handle.
    pub material: GpuHandle,
    /// Per-world instance handle.
    pub instance: GpuHandle,
    /// Deformation output handle or [`GpuHandle::INVALID`].
    pub deformation: GpuHandle,
    /// Local cluster or aggregate-voxel index within the generational geometry record.
    pub content_index: u32,
    /// Canonical local or assembly-part index within the geometry prototype.
    pub part: u32,
    /// [`GpuRepresentation`] value.
    pub representation: u32,
    /// Source cell/scene generation.
    pub source_generation: u32,
    /// Fixed PSO bin.
    pub pso_bin: GpuPsoBin,
    /// Temporal representation transition state.
    pub transition: u32,
    /// Visibility, residency, and hierarchy-cut state.
    pub cluster_state: u32,
    /// Reserved ABI word.
    pub reserved: u32,
}

/// Device form of one GPU-scene prototype slot.
///
/// Handle fields typed [`GpuHandle`] address the resident asset tables; `root_page` is a
/// packed GPU-scene page handle resolved through the scene page table.
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
#[repr(C, align(16))]
pub struct GpuScenePrototypeGpuRecord {
    /// Geometry-table handle.
    pub geometry: GpuHandle,
    /// Range of packed scene-material handles in [`GlobalGpuData::prototype_materials`].
    pub material_range: GpuArenaRange,
    /// Packed scene deformation handle or [`GpuHandle::INVALID`].
    pub deformation: GpuHandle,
    /// Packed scene SDF handle or [`GpuHandle::INVALID`].
    pub sdf: GpuHandle,
    /// Packed scene page handle of the guaranteed-resident root.
    pub root_page: GpuHandle,
    /// Source generation of the mirrored asset.
    pub source_generation: u32,
    /// Prototype flags.
    pub flags: u32,
    /// Conservative object-space bounding sphere (16-byte aligned for storage-pointer loads).
    pub bounds: [f32; 4],
    /// Reserved ABI words.
    pub reserved: [u32; 4],
}

/// Device form of one GPU-scene reference slot (material, deformation, or SDF): a
/// resident-table handle plus the revision of its source data.
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
#[repr(C, align(8))]
pub struct GpuSceneReferenceGpuRecord {
    /// Resident-table handle the reference resolves to.
    pub target: GpuHandle,
    /// Monotonic source-data revision.
    pub source_revision: u64,
}

/// Device form of one GPU-scene page slot: the resident page-table record it wraps and
/// its packed parent scene-page handle.
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
#[repr(C, align(8))]
pub struct GpuScenePageGpuRecord {
    /// Resident [`GlobalGpuData::page_table`] handle.
    pub table: GpuHandle,
    /// Packed parent scene-page handle or [`GpuHandle::INVALID`] for a root.
    pub parent: GpuHandle,
    /// Source generation of the mirrored page.
    pub source_generation: u32,
    /// Page flags.
    pub flags: u32,
}

/// The `transform_kind` value of a compact static placement transform.
pub const GPU_SCENE_TRANSFORM_STATIC: u32 = 0;
/// The `transform_kind` value of a current/previous dynamic matrix pair.
pub const GPU_SCENE_TRANSFORM_DYNAMIC: u32 = 1;

/// Device form of one per-world GPU-scene instance slot.
///
/// `transform` stores either the 64-byte compact static placement (remaining words zero)
/// or the current and previous column-major world matrices, selected by `transform_kind`.
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
#[repr(C, align(16))]
pub struct GpuSceneInstanceGpuRecord {
    /// Packed scene prototype handle.
    pub prototype: GpuHandle,
    /// Packed scene deformation handle or [`GpuHandle::INVALID`].
    pub deformation: GpuHandle,
    /// Packed scene SDF handle or [`GpuHandle::INVALID`].
    pub sdf: GpuHandle,
    /// Range of [`GpuSceneOverrideGpuRecord`] elements in the override arena.
    pub material_overrides: GpuArenaRange,
    /// [`GPU_SCENE_TRANSFORM_STATIC`] or [`GPU_SCENE_TRANSFORM_DYNAMIC`].
    pub transform_kind: u32,
    /// Source generation of the mirrored instance.
    pub source_generation: u32,
    /// Instance flags.
    pub flags: u32,
    /// Reserved ABI word.
    pub reserved: u32,
    /// The transform payload words.
    pub transform: [f32; 32],
}

impl Default for GpuSceneInstanceGpuRecord {
    fn default() -> Self {
        Zeroable::zeroed()
    }
}

/// Device form of one per-world GPU-scene light slot.
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
#[repr(C, align(16))]
pub struct GpuSceneLightGpuRecord {
    /// The packed punctual light.
    pub light: crate::GpuLight,
    /// Monotonic source-data revision.
    pub source_revision: u64,
    /// Reserved ABI words.
    pub reserved: u64,
}

/// One sparse per-instance material override element.
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
#[repr(C, align(8))]
pub struct GpuSceneOverrideGpuRecord {
    /// Prototype material slot the override replaces.
    pub slot: u32,
    /// Reserved ABI word.
    pub reserved: u32,
    /// Packed scene material handle.
    pub material: GpuHandle,
}

const _: () = assert!(
    size_of::<GpuScenePrototypeGpuRecord>() == 80
        && size_of::<GpuSceneReferenceGpuRecord>() == 16
        && size_of::<GpuScenePageGpuRecord>() == 24
        && size_of::<GpuSceneInstanceGpuRecord>() == 176
        && size_of::<GpuSceneLightGpuRecord>() == 80
        && size_of::<GpuSceneOverrideGpuRecord>() == 16,
    "GPU-scene device records must match the locked std430 slot layouts"
);

/// Fixed semantic PSO-bin vocabulary independent of the draw executor.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Pod, Zeroable)]
#[repr(transparent)]
pub struct GpuPsoBin(u32);

/// Pass-independent immutable material pipeline dimensions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Pod, Zeroable)]
#[repr(transparent)]
pub struct GpuMaterialClass(u32);

/// Geometry representation consumed by a draw executor.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u32)]
pub enum GpuRepresentation {
    /// Portable triangle-cluster content, independent of indexed or mesh-shader execution.
    #[default]
    TriangleCluster = 0,
    /// Aggregate voxel surface.
    AggregateVoxel = 1,
    /// Procedural micro vegetation blade reconstructed from a resident field tile.
    MicroBlade = 2,
}

/// Raster sidedness.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u32)]
pub enum GpuSidedness {
    /// Back-face culling is enabled.
    #[default]
    Single = 0,
    /// Both sides are rasterized.
    Double = 1,
}

/// Alpha compositing class kept distinct from coverage classification.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u32)]
pub enum GpuTransparency {
    /// Depth-writing opaque or cutout geometry.
    #[default]
    Opaque = 0,
    /// Back-to-front alpha blending.
    AlphaBlended = 1,
}

/// Shared deformation-provider output class.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u32)]
pub enum GpuDeformation {
    /// Rigid geometry.
    #[default]
    Rigid = 0,
    /// Common deformed output produced by one or more composed deformation providers.
    Deformed = 1,
}

/// Render-pass class sharing one semantic visibility record.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u32)]
pub enum GpuPassClass {
    /// Depth prepass.
    #[default]
    Depth = 0,
    /// Forward main pass.
    Main = 1,
    /// Motion-vector pass.
    Motion = 2,
    /// Directional, spot, point, or virtual shadow depth.
    Shadow = 3,
    /// Deferred G-buffer pass.
    GBuffer = 4,
    /// Sorted alpha-blended pass.
    Transparent = 5,
    /// Selection-ID pass.
    Selection = 6,
    /// Asset thumbnail or preview pass.
    Preview = 7,
    /// Ray-tracing geometry/hit classification.
    RayTracing = 8,
    /// Wireframe and scene-debug geometry.
    WireDebug = 9,
}

impl GpuMaterialClass {
    /// Builds one immutable class from the canonical material dimensions.
    pub const fn new(
        coverage: AlphaClassification,
        sidedness: GpuSidedness,
        surface_model: SurfaceModel,
        transparency: GpuTransparency,
        unlit: bool,
    ) -> Self {
        let surface_model = match surface_model {
            SurfaceModel::Standard => 0,
            SurfaceModel::ThinSheetFoliage => 1,
        };
        Self(
            ((coverage as u32) << GPU_MATERIAL_COVERAGE_SHIFT)
                | ((sidedness as u32) << GPU_MATERIAL_SIDEDNESS_SHIFT)
                | (surface_model << GPU_MATERIAL_SURFACE_MODEL_SHIFT)
                | ((transparency as u32) << GPU_MATERIAL_TRANSPARENCY_SHIFT)
                | ((unlit as u32) << GPU_MATERIAL_UNLIT_SHIFT),
        )
    }

    /// Strictly restores a bounded material class from GPU bits.
    #[must_use]
    pub const fn from_bits(bits: u32) -> Option<Self> {
        if bits & !0x3f != 0 || bits & 0x3 > 2 {
            return None;
        }
        Some(Self(bits))
    }

    /// The unlit shading permutation.
    #[must_use]
    pub const fn unlit(self) -> bool {
        self.0 & (1 << GPU_MATERIAL_UNLIT_SHIFT) != 0
    }

    /// Canonical scalar embedded in material and full PSO records.
    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// Canonical coverage classification.
    #[must_use]
    pub const fn coverage(self) -> AlphaClassification {
        match self.0 & 0x3 {
            0 => AlphaClassification::Opaque,
            1 => AlphaClassification::Masked,
            2 => AlphaClassification::Transmissive,
            _ => panic!("GpuMaterialClass must contain validated coverage bits"),
        }
    }

    /// Raster sidedness.
    #[must_use]
    pub const fn sidedness(self) -> GpuSidedness {
        if (self.0 >> 2) & 1 == 0 {
            GpuSidedness::Single
        } else {
            GpuSidedness::Double
        }
    }

    /// Canonical material surface model.
    #[must_use]
    pub const fn surface_model(self) -> SurfaceModel {
        if (self.0 >> 3) & 1 == 0 {
            SurfaceModel::Standard
        } else {
            SurfaceModel::ThinSheetFoliage
        }
    }

    /// Transparency compositing class.
    #[must_use]
    pub const fn transparency(self) -> GpuTransparency {
        if (self.0 >> 4) & 1 == 0 {
            GpuTransparency::Opaque
        } else {
            GpuTransparency::AlphaBlended
        }
    }
}

impl GpuPsoBin {
    /// Builds a bin from the canonical bounded dimensions.
    pub const fn new(
        representation: GpuRepresentation,
        material: GpuMaterialClass,
        deformation: GpuDeformation,
        pass: GpuPassClass,
    ) -> Self {
        Self(
            ((representation as u32) << GPU_PSO_REPRESENTATION_SHIFT)
                | (material.bits() << GPU_PSO_MATERIAL_SHIFT)
                | ((deformation as u32) << GPU_PSO_DEFORMATION_SHIFT)
                | ((pass as u32) << GPU_PSO_PASS_SHIFT),
        )
    }

    /// Validates and restores the complete bounded PSO vocabulary from GPU bits.
    #[must_use]
    pub const fn from_bits(bits: u32) -> Option<Self> {
        if bits & !0x1fff != 0
            || bits & 0x3 > 1
            || (bits >> 2) & 0x3 > 2
            || (bits >> GPU_PSO_PASS_SHIFT) & 0xf > 9
        {
            return None;
        }
        Some(Self(bits))
    }

    /// Canonical scalar stored in table and draw records.
    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// Representation dimension.
    #[must_use]
    pub const fn representation(self) -> GpuRepresentation {
        match self.0 & 0x3 {
            0 => GpuRepresentation::TriangleCluster,
            1 => GpuRepresentation::AggregateVoxel,
            _ => panic!("GpuPsoBin must contain validated representation bits"),
        }
    }

    /// Coverage dimension.
    #[must_use]
    pub const fn coverage(self) -> AlphaClassification {
        self.material_class().coverage()
    }

    /// Sidedness dimension.
    #[must_use]
    pub const fn sidedness(self) -> GpuSidedness {
        self.material_class().sidedness()
    }

    /// Surface-model dimension.
    #[must_use]
    pub const fn surface_model(self) -> SurfaceModel {
        self.material_class().surface_model()
    }

    /// Transparency dimension.
    #[must_use]
    pub const fn transparency(self) -> GpuTransparency {
        self.material_class().transparency()
    }

    /// Pass-independent dimensions sourced from the immutable material record.
    #[must_use]
    pub const fn material_class(self) -> GpuMaterialClass {
        GpuMaterialClass((self.0 >> GPU_PSO_MATERIAL_SHIFT) & 0x1f)
    }

    /// Deformation dimension.
    #[must_use]
    pub const fn deformation(self) -> GpuDeformation {
        match (self.0 >> GPU_PSO_DEFORMATION_SHIFT) & 0x1 {
            0 => GpuDeformation::Rigid,
            1 => GpuDeformation::Deformed,
            _ => panic!("GpuPsoBin must contain validated deformation bits"),
        }
    }

    /// Render-pass dimension.
    #[must_use]
    pub const fn pass(self) -> GpuPassClass {
        match (self.0 >> GPU_PSO_PASS_SHIFT) & 0xf {
            0 => GpuPassClass::Depth,
            1 => GpuPassClass::Main,
            2 => GpuPassClass::Motion,
            3 => GpuPassClass::Shadow,
            4 => GpuPassClass::GBuffer,
            5 => GpuPassClass::Transparent,
            6 => GpuPassClass::Selection,
            7 => GpuPassClass::Preview,
            8 => GpuPassClass::RayTracing,
            9 => GpuPassClass::WireDebug,
            _ => panic!("GpuPsoBin must contain validated pass bits"),
        }
    }
}

/// Marker for the global vertex arena.
pub enum VertexArena {}
/// Marker for the global index arena.
pub enum IndexArena {}
/// Marker for the global cluster arena.
pub enum ClusterArena {}
/// Marker for the global assembly-part arena.
pub enum PartArena {}

/// Marker for the micro vegetation field-tile arena.
#[derive(Debug)]
pub enum FieldArena {}
/// Marker for the global aggregate-voxel arena.
pub enum VoxelArena {}
/// Marker for the global page-byte arena.
pub enum PageArena {}
/// Stable deformed-vertex output arena marker.
pub enum DeformedVertexArena {}
/// Previous-frame deformed-vertex arena marker.
pub enum PrevDeformedVertexArena {}
/// Marker for the page dependency-handle arena.
pub enum PageDependencyArena {}
/// Marker for the skeleton joint-hierarchy arena.
pub enum SkeletonJointArena {}
/// Marker for the inverse-bind-matrix arena.
pub enum InverseBindArena {}
/// Marker for the composed deformation-provider arena.
pub enum DeformationProviderArena {}
/// Marker for deformation-provider parameter words.
pub enum DeformationParameterArena {}
/// Marker for per-prototype material handles.
pub enum PrototypeMaterialArena {}
/// Marker for immutable byte-locked material parameters.
pub enum MaterialParameterArena {}
/// Marker for the prototype table.
pub enum PrototypeTable {}
/// Marker for the geometry table.
pub enum GeometryTable {}
/// Marker for the material table.
pub enum MaterialTable {}
/// Marker for the texture table.
pub enum TextureTable {}
/// Marker for the coverage table.
pub enum CoverageTable {}
/// Marker for the skeleton table.
pub enum SkeletonTable {}
/// Marker for the page table.
pub enum PageTable {}
/// Marker for the geometry submesh-record arena.
pub enum SubmeshArena {}
/// Marker for the GPU-scene prototype table.
pub enum ScenePrototypeTable {}
/// Marker for the GPU-scene material-reference table.
pub enum SceneMaterialTable {}
/// Marker for the GPU-scene deformation-reference table.
pub enum SceneDeformationTable {}
/// Marker for the GPU-scene SDF-reference table.
pub enum SceneSdfTable {}
/// Marker for the GPU-scene page table.
pub enum ScenePageTable {}
/// Marker for a per-world GPU-scene instance table.
pub enum SceneInstanceTable {}
/// Marker for a per-world GPU-scene light table.
pub enum SceneLightTable {}
/// Marker for the per-instance material-override arena.
pub enum SceneOverrideArena {}

/// One resident device table addressed by the pending-upload queues.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GlobalGpuTableKind {
    /// [`GlobalGpuData::prototypes`].
    Prototype,
    /// [`GlobalGpuData::geometries`].
    Geometry,
    /// [`GlobalGpuData::materials`].
    Material,
    /// [`GlobalGpuData::textures`].
    Texture,
    /// [`GlobalGpuData::coverage`].
    Coverage,
    /// [`GlobalGpuData::skeletons`].
    Skeleton,
    /// [`GlobalGpuData::page_table`].
    Page,
}

/// Descriptor-ready identity of one global immutable table buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GpuTableDescriptor {
    /// Vulkan storage buffer.
    pub buffer: vk::Buffer,
    /// First bound byte.
    pub offset: vk::DeviceSize,
    /// Bound byte range.
    pub range: vk::DeviceSize,
    /// Element-zero device address, or zero without BDA support.
    pub address: vk::DeviceAddress,
    /// Generation-header plus record stride.
    pub slot_stride: u64,
}

/// Complete immutable-table descriptor set shared by every GPU-scene world and view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GlobalGpuTableDescriptors {
    /// Prototype records.
    pub prototypes: GpuTableDescriptor,
    /// Geometry records.
    pub geometries: GpuTableDescriptor,
    /// Material records.
    pub materials: GpuTableDescriptor,
    /// Texture records.
    pub textures: GpuTableDescriptor,
    /// Coverage records.
    pub coverage: GpuTableDescriptor,
    /// Skeleton records.
    pub skeletons: GpuTableDescriptor,
    /// Resident-page records.
    pub pages: GpuTableDescriptor,
}

/// Complete device-global geometry arenas and immutable metadata tables.
pub struct GlobalGpuData {
    /// Raw packed vertex data.
    pub vertices: GlobalGpuArena<VertexArena>,
    /// Raw packed index data.
    pub indices: GlobalGpuArena<IndexArena>,
    /// Virtual-geometry cluster records.
    pub clusters: GlobalGpuArena<ClusterArena>,
    /// Plant assembly-part records.
    pub parts: GlobalGpuArena<PartArena>,
    /// Micro vegetation field tiles (quantized density + reconstruction headers).
    pub fields: GlobalGpuArena<FieldArena>,
    /// The shared micro-blade template's index block within the pages arena, seeded
    /// once at renderer startup.
    pub micro_blade_template: GpuArenaRange,
    /// The frame-transient micro-blade candidates: one
    /// [`crate::SCENE_MICRO_CANDIDATE_CAPACITY`]-slot region per frame in flight,
    /// written by the micro pass and read by the executor vertex paths through the
    /// address block.
    pub micro_candidates: crate::Buffer,
    /// Aggregate-voxel records and portable surface output.
    pub voxels: GlobalGpuArena<VoxelArena>,
    /// Content-addressed resident page bytes.
    pub pages: GlobalGpuArena<PageArena>,
    /// Stable per-instance deformed-vertex output (the skinning compute writes it; the
    /// executor pulls it).
    pub deformed_vertices: GlobalGpuArena<DeformedVertexArena>,
    /// Last frame's deformed vertices (deformation motion vectors).
    pub prev_deformed_vertices: GlobalGpuArena<PrevDeformedVertexArena>,
    /// Page dependency handles addressed by [`GpuPageRecord::dependencies`].
    pub page_dependencies: GlobalGpuArena<PageDependencyArena>,
    /// Skeleton joints addressed by [`GpuSkeletonRecord::joints`].
    pub skeleton_joints: GlobalGpuArena<SkeletonJointArena>,
    /// Inverse-bind matrices addressed by [`GpuSkeletonRecord::inverse_binds`].
    pub inverse_binds: GlobalGpuArena<InverseBindArena>,
    /// Composed provider chains addressed by [`GpuSkeletonRecord::deformation`].
    pub deformation_providers: GlobalGpuArena<DeformationProviderArena>,
    /// Provider parameter words addressed by [`GpuDeformationProviderRecord`].
    pub deformation_parameters: GlobalGpuArena<DeformationParameterArena>,
    /// Material handles addressed by [`GpuPrototypeRecord::material_range`].
    pub prototype_materials: GlobalGpuArena<PrototypeMaterialArena>,
    /// Immutable parameters addressed by [`GpuMaterialTableRecord::parameter_index`].
    pub material_parameters: GlobalGpuArena<MaterialParameterArena>,
    /// Geometry submesh records referenced by [`GpuGeometryRecord::submeshes`].
    pub submesh_table: GlobalGpuArena<SubmeshArena>,
    /// The executor shader registry [`GpuMaterialTableRecord::shader_index`] indexes.
    pub executor_shaders: ExecutorShaderRegistry,
    /// Prototype records.
    pub prototypes: ResidentGpuTable<GpuPrototypeRecord, PrototypeTable>,
    /// Geometry records.
    pub geometries: ResidentGpuTable<GpuGeometryRecord, GeometryTable>,
    /// Material records.
    pub materials: ResidentGpuTable<GpuMaterialTableRecord, MaterialTable>,
    /// Texture records.
    pub textures: ResidentGpuTable<GpuTextureTableRecord, TextureTable>,
    /// Coverage records.
    pub coverage: ResidentGpuTable<GpuCoverageRecord, CoverageTable>,
    /// Skeleton records.
    pub skeletons: ResidentGpuTable<GpuSkeletonRecord, SkeletonTable>,
    /// Page-table records.
    pub page_table: ResidentGpuTable<GpuPageRecord, PageTable>,
    /// Frame-safe staging uploads.
    pub uploads: FrameUploadRing,
}

impl GlobalGpuData {
    /// Creates the global substrate with byte-addressable arenas.
    pub fn new(device: &Device) -> Result<Self> {
        let mut pages = GlobalGpuArena::new(device, 1, INITIAL_ARENA_BYTES)?;
        let micro_candidates = crate::Buffer::new(
            device.resources(),
            u64::from(crate::MAX_FRAMES_IN_FLIGHT as u32)
                * u64::from(crate::SCENE_MICRO_CANDIDATE_CAPACITY)
                * size_of::<GpuMicroCandidate>() as u64,
            vk::BufferUsageFlags::STORAGE_BUFFER | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS,
            &vk_mem::AllocationCreateInfo {
                usage: vk_mem::MemoryUsage::AutoPreferDevice,
                ..Default::default()
            },
        )?;
        // The shared micro-blade template's index block lives at a fixed pages-arena
        // range; the renderer seeds its bytes once at startup.
        let (micro_blade_template, _) =
            pages.allocate(MICRO_BLADE_INDEX_COUNT * size_of::<u32>() as u32, 16)?;
        Ok(Self {
            vertices: GlobalGpuArena::new(device, 1, INITIAL_ARENA_BYTES)?,
            deformed_vertices: GlobalGpuArena::new(device, 1, INITIAL_ARENA_BYTES)?,
            prev_deformed_vertices: GlobalGpuArena::new(device, 1, INITIAL_ARENA_BYTES)?,
            indices: GlobalGpuArena::new(device, 1, INITIAL_ARENA_BYTES)?,
            clusters: GlobalGpuArena::new(device, 1, INITIAL_ARENA_BYTES)?,
            parts: GlobalGpuArena::new(device, 1, INITIAL_ARENA_BYTES)?,
            fields: GlobalGpuArena::new(device, 1, INITIAL_ARENA_BYTES)?,
            voxels: GlobalGpuArena::new(device, 1, INITIAL_ARENA_BYTES)?,
            pages,
            micro_blade_template,
            micro_candidates,
            page_dependencies: GlobalGpuArena::new(
                device,
                std::mem::size_of::<GpuHandle>() as u64,
                4_096,
            )?,
            skeleton_joints: GlobalGpuArena::new(
                device,
                std::mem::size_of::<GpuSkeletonJointRecord>() as u64,
                4_096,
            )?,
            inverse_binds: GlobalGpuArena::new(
                device,
                std::mem::size_of::<GpuInverseBindRecord>() as u64,
                4_096,
            )?,
            deformation_providers: GlobalGpuArena::new(
                device,
                std::mem::size_of::<GpuDeformationProviderRecord>() as u64,
                4_096,
            )?,
            deformation_parameters: GlobalGpuArena::new(
                device,
                std::mem::size_of::<u32>() as u64,
                16_384,
            )?,
            prototype_materials: GlobalGpuArena::new(
                device,
                std::mem::size_of::<GpuHandle>() as u64,
                16_384,
            )?,
            material_parameters: GlobalGpuArena::new(
                device,
                std::mem::size_of::<MaterialParamsData>() as u64,
                4_096,
            )?,
            submesh_table: GlobalGpuArena::new(
                device,
                std::mem::size_of::<GpuSubmeshRecord>() as u64,
                4_096,
            )?,
            executor_shaders: ExecutorShaderRegistry::default(),
            prototypes: ResidentGpuTable::new(device, 1_024)?,
            geometries: ResidentGpuTable::new(device, 1_024)?,
            materials: ResidentGpuTable::new(device, 4_096)?,
            textures: ResidentGpuTable::new(device, 4_096)?,
            coverage: ResidentGpuTable::new(device, 4_096)?,
            skeletons: ResidentGpuTable::new(device, 1_024)?,
            page_table: ResidentGpuTable::new(device, 4_096)?,
            uploads: FrameUploadRing::new(device, INITIAL_UPLOAD_BYTES)?,
        })
    }

    /// Reclaims all handles, ranges, buffers, and staging space owned by a completed frame slot.
    pub fn begin_frame(&mut self, completed_frame_slot: usize) -> Result<()> {
        frame_slot_bit(completed_frame_slot)?;
        self.vertices.begin_frame(completed_frame_slot)?;
        self.indices.begin_frame(completed_frame_slot)?;
        self.clusters.begin_frame(completed_frame_slot)?;
        self.parts.begin_frame(completed_frame_slot)?;
        self.fields.begin_frame(completed_frame_slot)?;
        self.voxels.begin_frame(completed_frame_slot)?;
        self.pages.begin_frame(completed_frame_slot)?;
        self.page_dependencies.begin_frame(completed_frame_slot)?;
        self.skeleton_joints.begin_frame(completed_frame_slot)?;
        self.inverse_binds.begin_frame(completed_frame_slot)?;
        self.deformation_providers
            .begin_frame(completed_frame_slot)?;
        self.deformation_parameters
            .begin_frame(completed_frame_slot)?;
        self.prototype_materials.begin_frame(completed_frame_slot)?;
        self.material_parameters.begin_frame(completed_frame_slot)?;
        self.submesh_table.begin_frame(completed_frame_slot)?;
        self.prototypes.begin_frame(completed_frame_slot)?;
        self.geometries.begin_frame(completed_frame_slot)?;
        self.materials.begin_frame(completed_frame_slot)?;
        self.textures.begin_frame(completed_frame_slot)?;
        self.coverage.begin_frame(completed_frame_slot)?;
        self.skeletons.begin_frame(completed_frame_slot)?;
        self.page_table.begin_frame(completed_frame_slot)?;
        self.uploads.begin_frame(completed_frame_slot)
    }

    /// Captures descriptor-ready identities for every immutable global table.
    #[must_use]
    pub fn table_descriptors(&self, device: &Device) -> GlobalGpuTableDescriptors {
        GlobalGpuTableDescriptors {
            prototypes: self.prototypes.descriptor(device),
            geometries: self.geometries.descriptor(device),
            materials: self.materials.descriptor(device),
            textures: self.textures.descriptor(device),
            coverage: self.coverage.descriptor(device),
            skeletons: self.skeletons.descriptor(device),
            pages: self.page_table.descriptor(device),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::mem::{align_of, offset_of, size_of};

    use saffron_spatial::UnitInterval;

    use super::*;

    #[test]
    fn handle_reuse_waits_for_every_frame_slot_and_bumps_generation() {
        let mut table = ImmutableGpuTable::default();
        let first = table.insert(7_u32).unwrap();
        assert_eq!(table.retire(first), Some(7));
        assert!(table.get(first).is_none());

        table.begin_frame(0).unwrap();
        let second = table.insert(8_u32).unwrap();
        assert_ne!(second.index, first.index);

        for frame in 1..MAX_FRAMES_IN_FLIGHT {
            table.begin_frame(frame).unwrap();
        }
        let reused = table.insert(9_u32).unwrap();
        assert_eq!(reused.index, first.index);
        assert_ne!(reused.generation, first.generation);
        assert!(table.get(first).is_none());
        assert_eq!(table.get(reused), Some(&9));
    }

    #[test]
    fn range_reuse_waits_for_every_frame_slot_and_coalesces() {
        let mut ranges = GpuRangeAllocator::default();
        let first = ranges.allocate(4, 1).unwrap();
        let second = ranges.allocate(6, 1).unwrap();
        ranges.retire(first).unwrap();
        ranges.retire(second).unwrap();
        ranges.begin_frame(0).unwrap();
        assert_eq!(ranges.allocate(10, 1).unwrap().first, 10);

        for frame in 1..MAX_FRAMES_IN_FLIGHT {
            ranges.begin_frame(frame).unwrap();
        }
        assert_eq!(
            ranges.allocate(10, 1).unwrap(),
            GpuArenaRange {
                first: 0,
                count: 10
            }
        );
    }

    #[test]
    fn aligned_range_allocation_splits_and_reuses_gaps() {
        let mut ranges = GpuRangeAllocator::default();
        assert_eq!(ranges.allocate(3, 1).unwrap().first, 0);
        assert_eq!(ranges.allocate(2, 8).unwrap().first, 8);
        assert_eq!(ranges.required_capacity(), 10);
        assert_eq!(ranges.allocate(5, 1).unwrap().first, 3);
    }

    #[test]
    fn odd_byte_ranges_reserve_nonoverlapping_copy_aligned_spans() {
        let mut ranges = GpuRangeAllocator::default();
        let mut allocate = |count| {
            let (reserved, alignment) = physical_range_layout(count, 1, 1).unwrap();
            let logical = ranges
                .allocate_reserved(count, reserved, alignment)
                .unwrap();
            let physical = ranges.reserved_range(logical).unwrap();
            (logical, physical)
        };
        let (one, one_physical) = allocate(1);
        let (three, three_physical) = allocate(3);
        let (five, five_physical) = allocate(5);
        assert_eq!((one.first, one.count, one_physical.count), (0, 1, 4));
        assert_eq!((three.first, three.count, three_physical.count), (4, 3, 4));
        assert_eq!((five.first, five.count, five_physical.count), (8, 5, 8));
        assert_eq!(ranges.required_capacity(), 16);
    }

    #[test]
    fn range_retirement_rejects_duplicate_or_partial_allocations() {
        let mut ranges = GpuRangeAllocator::default();
        let range = ranges.allocate(8, 4).unwrap();
        assert!(
            ranges
                .retire(GpuArenaRange {
                    first: range.first,
                    count: range.count - 1,
                })
                .is_err()
        );
        ranges.retire(range).unwrap();
        assert!(ranges.retire(range).is_err());
    }

    #[test]
    fn invalid_frame_slots_are_rejected_without_shifting() {
        let mut table = ImmutableGpuTable::<u32>::default();
        assert!(table.begin_frame(MAX_FRAMES_IN_FLIGHT).is_err());
        let mut ranges = GpuRangeAllocator::default();
        assert!(ranges.begin_frame(MAX_FRAMES_IN_FLIGHT).is_err());
    }

    #[test]
    fn exhausted_generation_is_never_reused() {
        let mut table = ImmutableGpuTable::default();
        let handle = table.insert(7_u32).unwrap();
        table.slots[handle.index as usize].generation = u32::MAX;
        let exhausted = GpuHandle {
            index: handle.index,
            generation: u32::MAX,
        };
        assert_eq!(table.retire(exhausted), Some(7));
        for frame in 0..MAX_FRAMES_IN_FLIGHT {
            table.begin_frame(frame).unwrap();
        }
        assert_ne!(table.insert(8).unwrap().index, exhausted.index);
    }

    #[test]
    fn upload_planning_preserves_cursor_across_same_frame_growth() {
        let first = plan_upload(0, 16, 12, 4).unwrap();
        assert_eq!(first.offset, 0);
        assert_eq!(first.required_capacity, 16);
        let second = plan_upload(first.end, first.required_capacity, 12, 16).unwrap();
        assert_eq!(second.offset, 16);
        assert!(second.required_capacity >= 28);
        let third = plan_upload(second.end, second.required_capacity, 5, 8).unwrap();
        assert_eq!(third.offset, 32);
        assert!(third.end > second.end);
    }

    #[test]
    fn pso_bin_dimensions_round_trip_without_aliasing() {
        let representations = [
            GpuRepresentation::TriangleCluster,
            GpuRepresentation::AggregateVoxel,
        ];
        let coverage = [
            AlphaClassification::Opaque,
            AlphaClassification::Masked,
            AlphaClassification::Transmissive,
        ];
        let sidedness = [GpuSidedness::Single, GpuSidedness::Double];
        let surface_models = [SurfaceModel::Standard, SurfaceModel::ThinSheetFoliage];
        let transparency = [GpuTransparency::Opaque, GpuTransparency::AlphaBlended];
        let deformation = [GpuDeformation::Rigid, GpuDeformation::Deformed];
        let passes = [
            GpuPassClass::Depth,
            GpuPassClass::Main,
            GpuPassClass::Motion,
            GpuPassClass::Shadow,
            GpuPassClass::GBuffer,
            GpuPassClass::Transparent,
            GpuPassClass::Selection,
            GpuPassClass::Preview,
            GpuPassClass::RayTracing,
            GpuPassClass::WireDebug,
        ];
        let mut bits = HashSet::new();
        let mut material_bits = HashSet::new();
        for representation in representations {
            for coverage in coverage {
                for sidedness in sidedness {
                    for surface_model in surface_models {
                        for transparency in transparency {
                            let material = GpuMaterialClass::new(
                                coverage,
                                sidedness,
                                surface_model,
                                transparency,
                                false,
                            );
                            material_bits.insert(material.bits());
                            assert_eq!(
                                GpuMaterialClass::from_bits(material.bits()),
                                Some(material)
                            );
                            for deformation in deformation {
                                for pass in passes {
                                    let bin =
                                        GpuPsoBin::new(representation, material, deformation, pass);
                                    assert!(bits.insert(bin.bits()));
                                    assert_eq!(GpuPsoBin::from_bits(bin.bits()), Some(bin));
                                    assert_eq!(bin.representation(), representation);
                                    assert_eq!(bin.coverage(), coverage);
                                    assert_eq!(bin.sidedness(), sidedness);
                                    assert_eq!(bin.surface_model(), surface_model);
                                    assert_eq!(bin.transparency(), transparency);
                                    assert_eq!(bin.deformation(), deformation);
                                    assert_eq!(bin.pass(), pass);
                                }
                            }
                        }
                    }
                }
            }
        }
        assert_eq!(bits.len(), 2 * 3 * 2 * 2 * 2 * 2 * 10);
        assert_eq!(material_bits.len(), 3 * 2 * 2 * 2);
        assert_eq!(GpuMaterialClass::from_bits(3), None);
        assert_eq!(GpuPsoBin::from_bits(3), None);
        assert_eq!(GpuPsoBin::from_bits(1 << 15), None);
    }

    #[test]
    fn gpu_abi_layouts_are_byte_locked() {
        assert_eq!((size_of::<GpuHandle>(), align_of::<GpuHandle>()), (8, 8));
        assert_eq!(offset_of!(GpuHandle, index), 0);
        assert_eq!(offset_of!(GpuHandle, generation), 4);
        assert_eq!(
            (size_of::<GpuArenaRange>(), align_of::<GpuArenaRange>()),
            (8, 8)
        );
        assert_eq!(offset_of!(GpuArenaRange, first), 0);
        assert_eq!(offset_of!(GpuArenaRange, count), 4);
        assert_eq!(
            (
                size_of::<GpuTableSlotHeader>(),
                align_of::<GpuTableSlotHeader>()
            ),
            (16, 4)
        );
        assert_eq!(offset_of!(GpuTableSlotHeader, generation), 0);
        assert_eq!(offset_of!(GpuTableSlotHeader, occupied), 4);
        assert_eq!(offset_of!(GpuTableSlotHeader, reserved), 8);
        assert_eq!(
            (
                size_of::<GpuPrototypeRecord>(),
                align_of::<GpuPrototypeRecord>()
            ),
            (64, 16)
        );
        assert_eq!(offset_of!(GpuPrototypeRecord, geometry), 0);
        assert_eq!(offset_of!(GpuPrototypeRecord, material_range), 8);
        assert_eq!(offset_of!(GpuPrototypeRecord, skeleton), 16);
        assert_eq!(offset_of!(GpuPrototypeRecord, root_page), 24);
        assert_eq!(offset_of!(GpuPrototypeRecord, bounds), 32);
        assert_eq!(offset_of!(GpuPrototypeRecord, flags), 48);
        assert_eq!(offset_of!(GpuPrototypeRecord, reserved), 52);
        assert_eq!(
            (
                size_of::<GpuGeometryRecord>(),
                align_of::<GpuGeometryRecord>()
            ),
            (64, 8)
        );
        assert_eq!(offset_of!(GpuGeometryRecord, vertices), 0);
        assert_eq!(offset_of!(GpuGeometryRecord, indices), 8);
        assert_eq!(offset_of!(GpuGeometryRecord, clusters), 16);
        assert_eq!(offset_of!(GpuGeometryRecord, parts), 24);
        assert_eq!(offset_of!(GpuGeometryRecord, voxels), 32);
        assert_eq!(offset_of!(GpuGeometryRecord, submeshes), 40);
        assert_eq!(
            (
                size_of::<GpuSubmeshRecord>(),
                align_of::<GpuSubmeshRecord>()
            ),
            (16, 8)
        );
        assert_eq!(
            (
                size_of::<GpuMaterialTableRecord>(),
                align_of::<GpuMaterialTableRecord>()
            ),
            (40, 8)
        );
        assert_eq!(offset_of!(GpuMaterialTableRecord, base_color_texture), 0);
        assert_eq!(offset_of!(GpuMaterialTableRecord, normal_texture), 8);
        assert_eq!(offset_of!(GpuMaterialTableRecord, coverage), 16);
        assert_eq!(offset_of!(GpuMaterialTableRecord, parameter_index), 24);
        assert_eq!(offset_of!(GpuMaterialTableRecord, material_class), 28);
        assert_eq!(offset_of!(GpuMaterialTableRecord, shader_index), 32);
        assert_eq!(offset_of!(GpuMaterialTableRecord, flags), 36);
        assert_eq!(
            (
                size_of::<GpuTextureTableRecord>(),
                align_of::<GpuTextureTableRecord>()
            ),
            (32, 4)
        );
        assert_eq!(offset_of!(GpuTextureTableRecord, descriptor_index), 0);
        assert_eq!(offset_of!(GpuTextureTableRecord, width), 4);
        assert_eq!(offset_of!(GpuTextureTableRecord, height), 8);
        assert_eq!(offset_of!(GpuTextureTableRecord, mip_count), 12);
        assert_eq!(offset_of!(GpuTextureTableRecord, flags), 16);
        assert_eq!(offset_of!(GpuTextureTableRecord, reserved), 20);
        assert_eq!(
            (
                size_of::<GpuCoverageRecord>(),
                align_of::<GpuCoverageRecord>()
            ),
            (48, 8)
        );
        assert_eq!(offset_of!(GpuCoverageRecord, texture), 0);
        assert_eq!(offset_of!(GpuCoverageRecord, classification), 12);
        assert_eq!(offset_of!(GpuCoverageRecord, omm_policy), 20);
        assert_eq!(offset_of!(GpuCoverageRecord, source_extent), 32);
        assert_eq!(offset_of!(GpuCoverageRecord, reserved), 44);
        assert_eq!(
            (
                size_of::<GpuSkeletonRecord>(),
                align_of::<GpuSkeletonRecord>()
            ),
            (32, 8)
        );
        assert_eq!(offset_of!(GpuSkeletonRecord, joints), 0);
        assert_eq!(offset_of!(GpuSkeletonRecord, inverse_binds), 8);
        assert_eq!(offset_of!(GpuSkeletonRecord, deformation), 16);
        assert_eq!(offset_of!(GpuSkeletonRecord, flags), 24);
        assert_eq!(offset_of!(GpuSkeletonRecord, reserved), 28);
        assert_eq!(
            (size_of::<GpuPageRecord>(), align_of::<GpuPageRecord>()),
            (40, 8)
        );
        assert_eq!(offset_of!(GpuPageRecord, parent), 0);
        assert_eq!(offset_of!(GpuPageRecord, dependencies), 8);
        assert_eq!(offset_of!(GpuPageRecord, byte_offset), 16);
        assert_eq!(offset_of!(GpuPageRecord, byte_length), 24);
        assert_eq!(offset_of!(GpuPageRecord, resident_generation), 28);
        assert_eq!(offset_of!(GpuPageRecord, flags), 32);
        assert_eq!(offset_of!(GpuPageRecord, reserved), 36);
        assert_eq!(
            (size_of::<GpuDrawRecord>(), align_of::<GpuDrawRecord>()),
            (64, 8)
        );
        assert_eq!((size_of::<GpuPsoBin>(), align_of::<GpuPsoBin>()), (4, 4));
        assert_eq!(
            (
                size_of::<GpuMaterialClass>(),
                align_of::<GpuMaterialClass>()
            ),
            (4, 4)
        );
        assert_eq!(offset_of!(GpuMaterialClass, 0), 0);
        assert_eq!(offset_of!(GpuGeometryRecord, flags), 48);
        assert_eq!(offset_of!(GpuGeometryRecord, vertex_stride), 52);
        assert_eq!(offset_of!(GpuGeometryRecord, index_stride), 56);
        assert_eq!(offset_of!(GpuGeometryRecord, reserved), 60);
        assert_eq!(offset_of!(GpuCoverageRecord, cutoff), 8);
        assert_eq!(offset_of!(GpuCoverageRecord, source_kind), 16);
        assert_eq!(offset_of!(GpuCoverageRecord, hash_salt), 24);
        assert_eq!(offset_of!(GpuCoverageRecord, omm_thresholds), 40);
        assert_eq!(
            (
                size_of::<GpuSkeletonJointRecord>(),
                align_of::<GpuSkeletonJointRecord>()
            ),
            (8, 8)
        );
        assert_eq!(offset_of!(GpuSkeletonJointRecord, parent), 0);
        assert_eq!(offset_of!(GpuSkeletonJointRecord, flags), 4);
        assert_eq!(
            (
                size_of::<GpuInverseBindRecord>(),
                align_of::<GpuInverseBindRecord>()
            ),
            (64, 16)
        );
        assert_eq!(offset_of!(GpuInverseBindRecord, matrix), 0);
        assert_eq!(
            (
                size_of::<GpuDeformationProviderRecord>(),
                align_of::<GpuDeformationProviderRecord>()
            ),
            (16, 16)
        );
        assert_eq!(offset_of!(GpuDeformationProviderRecord, provider_mask), 0);
        assert_eq!(offset_of!(GpuDeformationProviderRecord, first_parameter), 4);
        assert_eq!(offset_of!(GpuDeformationProviderRecord, parameter_count), 8);
        assert_eq!(offset_of!(GpuDeformationProviderRecord, flags), 12);
        assert_eq!(offset_of!(GpuDrawRecord, geometry), 0);
        assert_eq!(offset_of!(GpuDrawRecord, material), 8);
        assert_eq!(offset_of!(GpuDrawRecord, instance), 16);
        assert_eq!(offset_of!(GpuDrawRecord, deformation), 24);
        assert_eq!(offset_of!(GpuDrawRecord, pso_bin), 48);
        assert_eq!(offset_of!(GpuDrawRecord, content_index), 32);
        assert_eq!(offset_of!(GpuDrawRecord, part), 36);
        assert_eq!(offset_of!(GpuDrawRecord, representation), 40);
        assert_eq!(offset_of!(GpuDrawRecord, source_generation), 44);
        assert_eq!(offset_of!(GpuDrawRecord, transition), 52);
        assert_eq!(offset_of!(GpuDrawRecord, cluster_state), 56);
        assert_eq!(offset_of!(GpuDrawRecord, reserved), 60);
    }

    #[test]
    fn slang_global_gpu_abi_is_locked_to_rust_constants_and_records() {
        let source = include_str!("../../../assets/shaders/global_gpu_data.slang");
        for declaration in [
            format!("GLOBAL_GPU_DATA_ABI_VERSION = {GLOBAL_GPU_DATA_ABI_VERSION}u"),
            format!("GPU_PSO_REPRESENTATION_SHIFT = {GPU_PSO_REPRESENTATION_SHIFT}u"),
            format!("GPU_PSO_MATERIAL_SHIFT = {GPU_PSO_MATERIAL_SHIFT}u"),
            format!("GPU_PSO_COVERAGE_SHIFT = {GPU_PSO_COVERAGE_SHIFT}u"),
            format!("GPU_PSO_SIDEDNESS_SHIFT = {GPU_PSO_SIDEDNESS_SHIFT}u"),
            format!("GPU_PSO_SURFACE_MODEL_SHIFT = {GPU_PSO_SURFACE_MODEL_SHIFT}u"),
            format!("GPU_PSO_TRANSPARENCY_SHIFT = {GPU_PSO_TRANSPARENCY_SHIFT}u"),
            format!("GPU_PSO_DEFORMATION_SHIFT = {GPU_PSO_DEFORMATION_SHIFT}u"),
            format!("GPU_PSO_PASS_SHIFT = {GPU_PSO_PASS_SHIFT}u"),
            format!("GPU_MATERIAL_COVERAGE_SHIFT = {GPU_MATERIAL_COVERAGE_SHIFT}u"),
            format!("GPU_MATERIAL_SIDEDNESS_SHIFT = {GPU_MATERIAL_SIDEDNESS_SHIFT}u"),
            format!("GPU_MATERIAL_SURFACE_MODEL_SHIFT = {GPU_MATERIAL_SURFACE_MODEL_SHIFT}u"),
            format!("GPU_MATERIAL_TRANSPARENCY_SHIFT = {GPU_MATERIAL_TRANSPARENCY_SHIFT}u"),
        ] {
            assert!(source.contains(&declaration), "missing `{declaration}`");
        }
        for (record, expected_body) in [
            ("GpuHandle", "public uint index; public uint generation;"),
            ("GpuArenaRange", "public uint first; public uint count;"),
            (
                "GpuTableSlotHeader",
                "public uint generation; public uint occupied; public uint2 reserved;",
            ),
            (
                "GpuPrototypeRecord",
                "public GpuHandle geometry; public GpuArenaRange materialRange; public GpuHandle skeleton; public GpuHandle rootPage; public float4 bounds; public uint flags; public uint3 reserved;",
            ),
            (
                "GpuGeometryRecord",
                "public GpuArenaRange vertices; public GpuArenaRange indices; public GpuArenaRange clusters; public GpuArenaRange parts; public GpuArenaRange voxels; public GpuArenaRange submeshes; public uint flags; public uint vertexStride; public uint indexStride; public uint reserved;",
            ),
            (
                "GpuMaterialTableRecord",
                "public GpuHandle baseColorTexture; public GpuHandle normalTexture; public GpuHandle coverage; public uint parameterIndex; public uint materialClass; public uint shaderIndex; public uint flags;",
            ),
            (
                "GpuTextureTableRecord",
                "public uint descriptorIndex; public uint width; public uint height; public uint mipCount; public uint flags; public uint3 reserved;",
            ),
            (
                "GpuCoverageRecord",
                "public GpuHandle texture; public float cutoff; public uint classification; public uint sourceKind; public uint ommPolicy; public uint2 hashSalt; public uint2 sourceExtent; public uint ommThresholds; public uint reserved;",
            ),
            (
                "GpuSkeletonRecord",
                "public GpuArenaRange joints; public GpuArenaRange inverseBinds; public GpuArenaRange deformation; public uint flags; public uint reserved;",
            ),
            (
                "GpuSkeletonJointRecord",
                "public uint parent; public uint flags;",
            ),
            ("GpuInverseBindRecord", "public float4x4 matrix;"),
            (
                "GpuDeformationProviderRecord",
                "public uint providerMask; public uint firstParameter; public uint parameterCount; public uint flags;",
            ),
            (
                "GpuPageRecord",
                "public GpuHandle parent; public GpuArenaRange dependencies; public uint64_t byteOffset; public uint byteLength; public uint residentGeneration; public uint flags; public uint reserved;",
            ),
            (
                "GpuDrawRecord",
                "public GpuHandle geometry; public GpuHandle material; public GpuHandle instance; public GpuHandle deformation; public uint contentIndex; public uint part; public uint representation; public uint sourceGeneration; public uint psoBin; public uint transition; public uint clusterState; public uint reserved;",
            ),
        ] {
            let marker = format!("public struct {record}");
            let declaration = source
                .split_once(&marker)
                .unwrap_or_else(|| panic!("missing `{marker}`"))
                .1;
            let body = declaration
                .split_once('{')
                .expect("Slang record opening brace")
                .1
                .split_once('}')
                .expect("Slang record closing brace")
                .0
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            assert_eq!(body, expected_body, "Slang fields for {record}");
        }
        assert!(source.contains("gpuHandleMatches(GpuHandle handle"));
        assert!(source.contains("header.occupied != 0u && header.generation == handle.generation"));
        assert!(source.contains("composeGpuMaterialClass("));
        assert!(source.contains("composeGpuPsoBin("));
        for semantic in [
            "GPU_REPRESENTATION_TRIANGLE_CLUSTER = 0u",
            "GPU_REPRESENTATION_AGGREGATE_VOXEL = 1u",
            "GPU_DEFORMATION_RIGID = 0u",
            "GPU_DEFORMATION_COMMON_OUTPUT = 1u",
            "GPU_PASS_WIRE_DEBUG = 9u",
        ] {
            assert!(source.contains(semantic));
        }
    }

    #[test]
    fn coverage_record_is_derived_from_canonical_metadata() {
        let metadata = CoverageMipMetadata {
            reference_cutoff: UnitInterval::from_bits(32_768),
            source_extent: [1024, 512],
            spatial_hash_salt: 0x0123_4567_89ab_cdef,
            classification: AlphaClassification::Masked,
            mip_hashes: vec![[7; 32]],
        };
        let record = GpuCoverageRecord::from_metadata(
            GpuHandle::INVALID,
            &CoverageSource::AlbedoAlpha,
            &metadata,
            OpacityMicromapDerivation::default(),
        );
        assert_eq!(record.classification, 1);
        assert_eq!(record.source_kind, 0);
        assert_eq!(record.hash_salt, [0x89ab_cdef, 0x0123_4567]);
        assert_eq!(record.source_extent, metadata.source_extent);
    }

    #[test]
    fn packed_handle_round_trips() {
        let handle = GpuHandle {
            index: 0x89ab_cdef,
            generation: 0x0123_4567,
        };
        assert_eq!(GpuHandle::from_packed(handle.packed()), handle);
    }

    #[test]
    fn arena_bda_byte_offsets_are_exact_and_checked() {
        assert_eq!(arena_byte_offset(0, 48).unwrap(), 0);
        assert_eq!(arena_byte_offset(1, 48).unwrap(), 48);
        assert_eq!(arena_byte_offset(0x0123_4567, 64).unwrap(), 0x48d1_59c0);
        assert!(arena_byte_offset(u32::MAX, u64::MAX).is_err());
    }

    #[test]
    fn arena_growth_and_reclamation_preserve_live_addresses_and_generations() {
        let device = match Device::new(&crate::SurfaceSource::Offscreen) {
            Ok(device) => device,
            Err(error) => {
                eprintln!("skipping: no Vulkan device obtainable ({error})");
                return;
            }
        };
        let before = crate::validation_issue_count();
        let mut arena = GlobalGpuArena::<VertexArena>::new(&device, 16, 1).unwrap();
        let first = arena.allocate(8, 1).unwrap().0;
        let second = arena.allocate(8, 1).unwrap().0;
        assert_eq!((first.first, second.first), (0, 8));

        let original_buffer = arena.buffer();
        let original_address = arena.address(&device);
        let growth_count = u32::try_from(arena.capacity()).unwrap() + 1;
        let (_, needs_growth) = arena.allocate(growth_count, 1).unwrap();
        assert!(needs_growth);
        let growth = arena
            .prepare_growth(&device)
            .unwrap()
            .expect("the range exceeds the original arena");
        assert_eq!(growth.source, original_buffer);
        assert_eq!(growth.destination, arena.buffer());
        assert_eq!(growth.size, growth.source_size);
        assert!(growth.destination_size > growth.source_size);
        assert_eq!(arena.retired_allocation_count(), 1);
        if device.capabilities.buffer_device_address {
            assert_ne!(original_address, 0);
            assert_eq!(
                device.buffer_device_address(growth.source),
                original_address
            );
            assert_ne!(arena.address(&device), original_address);
        }

        arena.retire(first).unwrap();
        arena.retire(second).unwrap();
        let before_fences = arena.allocate(16, 1).unwrap().0;
        assert_ne!(before_fences.first, first.first);

        let mut handles = ImmutableGpuTable::default();
        let retired_handle = handles.insert(7_u32).unwrap();
        assert_eq!(handles.retire(retired_handle), Some(7));
        assert_ne!(handles.insert(8).unwrap().index, retired_handle.index);

        for frame_slot in 0..MAX_FRAMES_IN_FLIGHT {
            if frame_slot + 1 < MAX_FRAMES_IN_FLIGHT && device.capabilities.buffer_device_address {
                assert_eq!(
                    device.buffer_device_address(growth.source),
                    original_address
                );
            }
            arena.begin_frame(frame_slot).unwrap();
            handles.begin_frame(frame_slot).unwrap();
            let expected_retired = usize::from(frame_slot + 1 < MAX_FRAMES_IN_FLIGHT);
            assert_eq!(arena.retired_allocation_count(), expected_retired);
            if frame_slot + 1 < MAX_FRAMES_IN_FLIGHT {
                assert_ne!(handles.insert(9_u32).unwrap().index, retired_handle.index);
            }
        }

        let compacted = arena.allocate(16, 1).unwrap().0;
        assert_eq!(compacted.first, first.first);
        let reused_handle = handles.insert(10_u32).unwrap();
        assert_eq!(reused_handle.index, retired_handle.index);
        assert_ne!(reused_handle.generation, retired_handle.generation);
        assert!(handles.get(retired_handle).is_none());

        drop(arena);
        device.wait_idle().expect("idle before teardown");
        drop(device);
        assert_eq!(crate::validation_issue_count(), before);
    }
}
