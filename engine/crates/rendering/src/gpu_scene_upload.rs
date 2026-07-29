//! Frame upload translation from the persistent GPU scene to its device tables.
//!
//! [`GpuSceneUploader`] owns the device half of the GPU-scene mirror: slot-indexed tables
//! for the shared prototype/material/deformation/SDF/page records, per-world instance and
//! light tables, and the sparse per-instance override arena. Each frame it drains
//! [`PersistentGpuScene::stage_upload_batch`], serializes every coalesced record into its
//! locked device layout, stages the bytes through the shared [`FrameUploadRing`], and
//! enqueues graph-owned transfer passes with exact byte ranges. Buffer growth enqueues its
//! preserving copy before any write that targets the grown buffer, and a slot's occupancy
//! header travels in the same write as its record, so a slot is never observable
//! half-published.
//!
//! [`GpuScenePendingUploads`] is the companion queue for resident asset records: the
//! journal-driven mirror queues record stages, retirements, and arena byte uploads
//! (vertex/index streams, material parameter blocks) as it resolves assets, and
//! [`record_pending_global_uploads`] drains the queue into the same frame's transfer
//! passes.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use crate::frame::MAX_FRAMES_IN_FLIGHT;
use crate::global_gpu_data::{
    FrameUploadRing, GlobalGpuArena, GlobalGpuData, GlobalGpuTableKind, GpuArenaRange,
    GpuBufferUpload, GpuHandle, GpuSceneInstanceGpuRecord, GpuSceneLightGpuRecord,
    GpuSceneOverrideGpuRecord, GpuScenePageGpuRecord, GpuScenePrototypeGpuRecord,
    GpuSceneReferenceGpuRecord, GpuTableDescriptor, GpuTableSlotHeader, SceneDeformationTable,
    SceneInstanceTable, SceneLightTable, SceneMaterialTable, SceneOverrideArena, ScenePageTable,
    ScenePrototypeTable, SceneSdfTable,
};
use crate::gpu_types::MaterialParamsData;
use crate::persistent_gpu_scene::{
    GpuSceneInstanceRecord, GpuScenePrototypeRecord, GpuSceneTransform, GpuSceneUploadPayload,
    GpuSceneUploadTarget, GpuSceneWorldId, PersistentGpuScene,
};
use crate::render_graph::RenderGraph;
use crate::resources::Buffer;
use crate::{Device, Error, GPU_SCENE_TRANSFORM_DYNAMIC, GPU_SCENE_TRANSFORM_STATIC, Result};
use ash::vk;
use bytemuck::{Pod, Zeroable};

/// Initial slot capacity of the shared scene tables.
const INITIAL_SHARED_SLOTS: u64 = 1_024;
/// Initial slot capacity of a world's instance table.
const INITIAL_INSTANCE_SLOTS: u64 = 4_096;
/// Initial slot capacity of a world's light table.
const INITIAL_LIGHT_SLOTS: u64 = 256;
/// Initial element capacity of the override arena.
const INITIAL_OVERRIDE_ELEMENTS: u64 = 4_096;
/// Stride of one per-frame address-block slice: the smallest multiple of the
/// spec's maximum `minUniformBufferOffsetAlignment` (256) that covers the block.
const ADDRESS_BLOCK_ALIGNMENT: u64 = 512;

const _: () = assert!(size_of::<GpuSceneAddressBlock>() as u64 <= ADDRESS_BLOCK_ALIGNMENT);
/// Entry capacity of ONE VIEW CLASS's region in a frame slot's missing-page request
/// buffer — a per-class ceiling, not a shared one.
pub const PAGE_REQUEST_CAPACITY: u32 = 4_096;
/// Bytes of one request entry: the resident page-table slot. The class is the region the
/// entry sits in, so carrying it again would be a second copy that could disagree with
/// the first.
const PAGE_REQUEST_ENTRY_BYTES: usize = 4;
/// Bytes before the first region: one count word per class.
const PAGE_REQUEST_HEADER_BYTES: usize = crate::SCENE_VIEW_CLASSES * 4;
/// Byte size of one frame slot's request slice: the counts plus one region per class.
const PAGE_REQUEST_SLOT_BYTES: u64 = PAGE_REQUEST_HEADER_BYTES as u64
    + crate::SCENE_VIEW_CLASSES as u64
        * PAGE_REQUEST_CAPACITY as u64
        * PAGE_REQUEST_ENTRY_BYTES as u64;

/// One frame's drained missing-page requests, and what the buffer could not hold.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PageRequestDrain {
    /// Requested page-table slots, deduplicated, each carrying the most urgent class that
    /// missed it — one page cannot be streamed twice, and the cheapest reader must not be
    /// what its priority is set from. Most urgent class first.
    pub requests: Vec<(u32, crate::SceneViewClass)>,
    /// Requests no region had room for, summed over the classes: demand that was raised
    /// and lost, so the page faults again next frame, later than it needed to.
    pub dropped: u32,
    /// Bit per class whose region filled ([`crate::SceneViewClass::bit`]). WHICH class
    /// overflowed is the whole diagnostic — the camera's is a stall in the image, a
    /// gather's is a slightly thinner gather.
    pub overflow_classes: u32,
}

/// Buffer device addresses of every GPU-scene table for one frame, bound as one uniform
/// block so growth never rewrites a descriptor. A zero address with zero capacity marks a
/// table that holds nothing this frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Pod, Zeroable)]
#[repr(C, align(16))]
pub struct GpuSceneAddressBlock {
    /// Resident prototype table.
    pub prototypes: u64,
    /// Resident geometry table.
    pub geometries: u64,
    /// Resident material table.
    pub materials: u64,
    /// Resident texture table.
    pub textures: u64,
    /// Resident coverage table.
    pub coverage: u64,
    /// Resident skeleton table.
    pub skeletons: u64,
    /// Baked signed-distance-field table.
    pub sdfs: u64,
    /// Resident page table.
    pub pages: u64,
    /// Shared scene prototype table.
    pub scene_prototypes: u64,
    /// Shared scene material-reference table.
    pub scene_materials: u64,
    /// Shared scene deformation-reference table.
    pub scene_deformations: u64,
    /// Shared scene SDF-reference table.
    pub scene_sdfs: u64,
    /// Shared scene page table.
    pub scene_pages: u64,
    /// The bound world's instance table.
    pub instances: u64,
    /// The bound world's light table.
    pub lights: u64,
    /// Per-instance override arena.
    pub overrides: u64,
    /// Prototype material-handle arena.
    pub prototype_materials: u64,
    /// Per-prototype packed scene-SDF handle arena.
    pub prototype_sdfs: u64,
    /// Material parameter-block arena.
    pub material_parameters: u64,
    /// Global vertex byte arena.
    pub vertices: u64,
    /// Global index byte arena.
    pub indices: u64,
    /// Plant assembly-part records arena (per-prototype vertex bases + per-use
    /// transforms).
    pub parts: u64,
    /// Geometry submesh-record arena.
    pub submesh_table: u64,
    /// Global page-payload byte arena.
    pub page_bytes: u64,
    /// This frame slot's missing-page request buffer (count word + slot entries).
    pub page_requests: u64,
    /// Stable deformed-vertex output arena.
    pub deformed_vertices: u64,
    /// Previous-frame deformed-vertex arena.
    pub prev_deformed_vertices: u64,
    /// Deformation-provider records arena.
    pub deformation_providers: u64,
    /// Provider parameter words arena.
    pub deformation_parameters: u64,
    /// Micro vegetation field-tile arena (headers + density samples + directory).
    pub fields: u64,
    /// The frame-transient micro-blade candidates buffer.
    pub candidates: u64,
    /// Per-instance-slot wind sway records the wind deformation prepass writes.
    pub wind_records: u64,
    /// The world interaction field (header + damped-oscillator texel cascades).
    pub interaction_field: u64,
    /// Fragment-side coverage counters, or 0 when nothing is measuring.
    ///
    /// A helper invocation's stores and atomics are discarded by the spec, so an atomic here counts
    /// COVERED samples while the `FRAGMENT_SHADER_INVOCATIONS` pipeline statistic counts every lane
    /// including helpers. The ratio is quad utilization, which no pipeline statistic reports on its
    /// own. Zero disables the increment through the same idiom every other optional address uses,
    /// so an unprofiled frame pays nothing. Also keeps the block's 16-byte alignment.
    pub quad_counters: u64,
    /// Slot capacity of the bound world's instance table.
    pub instance_capacity: u32,
    /// Slot capacity of the bound world's light table.
    pub light_capacity: u32,
    /// Deterministic temporal phase for canonical-coverage classification this frame.
    pub coverage_temporal_phase: u32,
    /// Allocated entry capacity of one view class's request region — the region stride.
    pub page_request_capacity: u32,
    /// Entries a class may actually append this frame, at most the capacity. Lowering it
    /// is how the overflow path is reached deliberately; addressing must never use it, or
    /// every region would move the moment it changed.
    pub page_request_budget: u32,
    /// Request regions in the buffer, one per [`crate::SceneViewClass`]. Carried rather
    /// than mirrored as a shader constant so the two halves cannot drift apart.
    pub page_request_classes: u32,
    /// Padding to the block's 16-byte alignment. Named because `bytemuck::Pod` will not
    /// accept a struct whose padding is implicit.
    pub reserved: [u32; 2],
}

const _: () = assert!(
    size_of::<GpuSceneAddressBlock>() == 304,
    "the GPU-scene address block must match the locked std430 layout"
);

/// One slot-indexed device table whose slot allocator is the persistent GPU scene.
///
/// Every slot is a 16-byte [`GpuTableSlotHeader`] followed by the record body, padded to a
/// 16-byte-aligned stride. Capacity follows the scene's high-water slot index; growth
/// preserves existing slots through a graph-owned copy.
pub struct GpuSceneTableStorage<K> {
    storage: GlobalGpuArena<K>,
    slot_stride: u64,
    slots: u64,
}

impl<K> GpuSceneTableStorage<K> {
    /// Creates the device buffer for `record_bytes`-sized slot bodies.
    pub fn new(device: &Device, record_bytes: u64, initial_slots: u64) -> Result<Self> {
        let unaligned = 16_u64
            .checked_add(record_bytes)
            .ok_or_else(|| Error::InvalidUploadData("scene table stride overflow".to_owned()))?;
        let slot_stride = unaligned
            .checked_add(15)
            .ok_or_else(|| Error::InvalidUploadData("scene table stride overflow".to_owned()))?
            & !15;
        Ok(Self {
            storage: GlobalGpuArena::new(device, slot_stride, initial_slots.max(1))?,
            slot_stride,
            slots: 0,
        })
    }

    /// Reserves capacity through `slot_count` exclusive; growth is signalled via
    /// [`Self::prepare_growth`].
    pub fn ensure_slots(&mut self, slot_count: u64) -> Result<()> {
        while self.slots < slot_count {
            self.storage.allocate(1, 1)?;
            self.slots += 1;
        }
        Ok(())
    }

    /// Grows the device buffer when reserved slots exceed it, returning the preserving copy.
    pub fn prepare_growth(&mut self, device: &Device) -> Result<Option<crate::GpuArenaGrowth>> {
        self.storage.prepare_growth(device)
    }

    /// Stages one slot write: the occupancy header plus `body`, zero-padded to the stride.
    pub fn stage_slot(
        &self,
        uploads: &mut FrameUploadRing,
        frame_slot: usize,
        slot: u32,
        generation: u32,
        occupied: u32,
        body: &[u8],
    ) -> Result<GpuBufferUpload> {
        let stride = usize::try_from(self.slot_stride).map_err(|_| {
            Error::InvalidUploadData("scene table slot exceeds address space".to_owned())
        })?;
        if body.len() + 16 > stride {
            return Err(Error::InvalidUploadData(
                "scene table record exceeds its slot stride".to_owned(),
            ));
        }
        let mut bytes = vec![0_u8; stride];
        let header = GpuTableSlotHeader {
            generation,
            occupied,
            reserved: [0; 2],
        };
        bytes[..16].copy_from_slice(bytemuck::bytes_of(&header));
        bytes[16..16 + body.len()].copy_from_slice(body);
        self.storage.stage(
            uploads,
            frame_slot,
            GpuArenaRange {
                first: slot,
                count: 1,
            },
            &bytes,
        )
    }

    /// Descriptor-ready buffer identity for this table.
    pub fn descriptor(&self, device: &Device) -> GpuTableDescriptor {
        GpuTableDescriptor {
            buffer: self.storage.buffer(),
            offset: 0,
            range: self.storage.capacity() * self.slot_stride,
            address: self.storage.address(device),
            slot_stride: self.slot_stride,
        }
    }

    /// Byte stride of one header-plus-record slot.
    pub fn slot_stride(&self) -> u64 {
        self.slot_stride
    }

    /// Current physical slot capacity.
    pub fn slot_capacity(&self) -> u64 {
        self.storage.capacity()
    }

    /// Reclaims superseded physical buffers after a frame fence signals.
    pub fn begin_frame(&mut self, completed_frame_slot: usize) -> Result<()> {
        self.storage.begin_frame(completed_frame_slot)
    }
}

/// The per-world device tables.
pub struct GpuSceneWorldTables {
    /// The world's instance table.
    pub instances: GpuSceneTableStorage<SceneInstanceTable>,
    /// The world's light table.
    pub lights: GpuSceneTableStorage<SceneLightTable>,
}

/// Descriptor-ready identities of the shared scene tables and the override arena.
#[derive(Clone, Copy, Debug)]
pub struct GpuSceneTableDescriptors {
    /// Shared prototype table.
    pub prototypes: GpuTableDescriptor,
    /// Shared material-reference table.
    pub materials: GpuTableDescriptor,
    /// Shared deformation-reference table.
    pub deformations: GpuTableDescriptor,
    /// Shared SDF-reference table.
    pub sdfs: GpuTableDescriptor,
    /// Shared page table.
    pub pages: GpuTableDescriptor,
    /// Per-instance override arena.
    pub overrides: GpuTableDescriptor,
}

/// Descriptor-ready identities of one world's tables.
#[derive(Clone, Copy, Debug)]
pub struct GpuSceneWorldDescriptors {
    /// The world's instance table.
    pub instances: GpuTableDescriptor,
    /// The world's light table.
    pub lights: GpuTableDescriptor,
}

/// Counters for one frame's upload translation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GpuSceneUploadRunStats {
    /// Batches drained from the persistent scene's upload ring.
    pub batches: u32,
    /// Slot records staged.
    pub records: u32,
    /// Estimated table bytes staged.
    pub table_bytes: u64,
    /// Buffer growths enqueued.
    pub growths: u32,
    /// Whether coalesced writes remain for a later frame after the frame budget.
    pub budget_exhausted: bool,
}

/// The device half of the persistent GPU scene and its frame upload translation.
pub struct GpuSceneUploader {
    prototypes: GpuSceneTableStorage<ScenePrototypeTable>,
    materials: GpuSceneTableStorage<SceneMaterialTable>,
    deformations: GpuSceneTableStorage<SceneDeformationTable>,
    sdfs: GpuSceneTableStorage<SceneSdfTable>,
    pages: GpuSceneTableStorage<ScenePageTable>,
    overrides: GlobalGpuArena<SceneOverrideArena>,
    worlds: BTreeMap<u64, GpuSceneWorldTables>,
    prototype_ranges: HashMap<u32, GpuArenaRange>,
    prototype_sdf_ranges: HashMap<u32, GpuArenaRange>,
    override_ranges: HashMap<(u64, u32), GpuArenaRange>,
    address_blocks: Buffer,
    page_requests: Buffer,
    /// Entries one class may append per frame; lowering it makes overflow reachable.
    page_request_budget: u32,
}

impl GpuSceneUploader {
    /// Creates the shared device tables; world tables materialize on first use.
    pub fn new(device: &Device) -> Result<Self> {
        Ok(Self {
            prototypes: GpuSceneTableStorage::new(
                device,
                size_of::<GpuScenePrototypeGpuRecord>() as u64,
                INITIAL_SHARED_SLOTS,
            )?,
            materials: GpuSceneTableStorage::new(
                device,
                size_of::<GpuSceneReferenceGpuRecord>() as u64,
                INITIAL_SHARED_SLOTS,
            )?,
            deformations: GpuSceneTableStorage::new(
                device,
                size_of::<GpuSceneReferenceGpuRecord>() as u64,
                INITIAL_SHARED_SLOTS,
            )?,
            sdfs: GpuSceneTableStorage::new(
                device,
                size_of::<GpuSceneReferenceGpuRecord>() as u64,
                INITIAL_SHARED_SLOTS,
            )?,
            pages: GpuSceneTableStorage::new(
                device,
                size_of::<GpuScenePageGpuRecord>() as u64,
                INITIAL_SHARED_SLOTS,
            )?,
            overrides: GlobalGpuArena::new(
                device,
                size_of::<GpuSceneOverrideGpuRecord>() as u64,
                INITIAL_OVERRIDE_ELEMENTS,
            )?,
            worlds: BTreeMap::new(),
            prototype_ranges: HashMap::new(),
            prototype_sdf_ranges: HashMap::new(),
            override_ranges: HashMap::new(),
            address_blocks: Buffer::new(
                device.resources(),
                ADDRESS_BLOCK_ALIGNMENT * MAX_FRAMES_IN_FLIGHT as u64,
                vk::BufferUsageFlags::UNIFORM_BUFFER,
                &vk_mem::AllocationCreateInfo {
                    usage: vk_mem::MemoryUsage::Auto,
                    flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_RANDOM
                        | vk_mem::AllocationCreateFlags::MAPPED,
                    ..Default::default()
                },
            )?,
            page_requests: Buffer::new(
                device.resources(),
                PAGE_REQUEST_SLOT_BYTES * MAX_FRAMES_IN_FLIGHT as u64,
                vk::BufferUsageFlags::STORAGE_BUFFER | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS,
                &vk_mem::AllocationCreateInfo {
                    usage: vk_mem::MemoryUsage::Auto,
                    flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_RANDOM
                        | vk_mem::AllocationCreateFlags::MAPPED,
                    ..Default::default()
                },
            )?,
            page_request_budget: PAGE_REQUEST_CAPACITY,
        })
    }

    /// The per-frame address-block uniform buffer.
    pub fn address_buffer(&self) -> vk::Buffer {
        self.address_blocks.handle()
    }

    /// Device address of `frame_slot`'s missing-page request slice.
    fn page_request_address(&self, device: &Device, frame_slot: usize) -> u64 {
        device.buffer_device_address(self.page_requests.handle())
            + frame_slot as u64 * PAGE_REQUEST_SLOT_BYTES
    }

    /// Drains `frame_slot`'s missing-page requests appended by the previous use of the
    /// slot, then resets its count words. Call after the slot's fence has completed.
    ///
    /// Each class's region is read in isolation, most urgent class first, so a page
    /// several views missed lands once under the most urgent of them. A region's count
    /// word keeps counting past the budget, which is how the drop count is known at all
    /// — the number of requests ATTEMPTED is on the device already, and reporting only
    /// the ones that fit would discard it.
    pub fn drain_page_requests(&mut self, frame_slot: usize) -> PageRequestDrain {
        let base = frame_slot * PAGE_REQUEST_SLOT_BYTES as usize;
        let budget = self.page_request_budget as usize;
        let mut drain = PageRequestDrain::default();
        let mut seen: std::collections::HashSet<u32> = std::collections::HashSet::new();
        let mut classes = crate::SceneViewClass::ALL;
        classes.sort_by_key(|class| std::cmp::Reverse(class.page_demand_priority()));
        for class in classes {
            let region = class.ordinal() as usize;
            // SAFETY: HOST_VISIBLE + MAPPED; the slot's prior GPU writes completed with
            // its fence before this frame reused the slot. Every offset is inside the
            // slot's own slice by construction.
            let attempted = unsafe {
                let count_ptr = self
                    .page_requests
                    .mapped_ptr()
                    .add(base + region * 4)
                    .cast::<u32>();
                let attempted = count_ptr.read_unaligned();
                let taken = (attempted as usize).min(budget);
                let region_base = base
                    + PAGE_REQUEST_HEADER_BYTES
                    + region * PAGE_REQUEST_CAPACITY as usize * PAGE_REQUEST_ENTRY_BYTES;
                for entry in 0..taken {
                    let slot = self
                        .page_requests
                        .mapped_ptr()
                        .add(region_base + entry * PAGE_REQUEST_ENTRY_BYTES)
                        .cast::<u32>()
                        .read_unaligned();
                    if seen.insert(slot) {
                        drain.requests.push((slot, class));
                    }
                }
                count_ptr.write_unaligned(0);
                attempted as usize
            };
            if attempted > budget {
                drain.dropped = drain
                    .dropped
                    .saturating_add((attempted - budget).min(u32::MAX as usize) as u32);
                drain.overflow_classes |= class.bit();
            }
        }
        drain
    }

    /// Entries one class may append per frame, at most [`PAGE_REQUEST_CAPACITY`].
    #[must_use]
    pub fn page_request_budget(&self) -> u32 {
        self.page_request_budget
    }

    /// Sets that budget, clamped to a usable region.
    pub fn set_page_request_budget(&mut self, entries: u32) {
        self.page_request_budget = entries.clamp(1, PAGE_REQUEST_CAPACITY);
    }

    /// Byte stride between per-frame address-block slices.
    pub fn address_block_stride(&self) -> u64 {
        ADDRESS_BLOCK_ALIGNMENT
    }

    /// Builds this frame's address block for `world`; absent world tables read as empty.
    #[allow(clippy::too_many_arguments)]
    pub fn build_address_block(
        &self,
        device: &Device,
        gpu_data: &GlobalGpuData,
        world: GpuSceneWorldId,
        frame_slot: usize,
        deformed: (u64, u64),
        wind_records: u64,
        interaction_field: u64,
        quad_counters: u64,
        coverage_temporal_phase: u32,
    ) -> GpuSceneAddressBlock {
        let resident = gpu_data.table_descriptors(device);
        let world_tables = self.worlds.get(&world.0);
        GpuSceneAddressBlock {
            prototypes: resident.prototypes.address,
            geometries: resident.geometries.address,
            materials: resident.materials.address,
            textures: resident.textures.address,
            coverage: resident.coverage.address,
            skeletons: resident.skeletons.address,
            sdfs: resident.sdfs.address,
            pages: resident.pages.address,
            scene_prototypes: self.prototypes.descriptor(device).address,
            scene_materials: self.materials.descriptor(device).address,
            scene_deformations: self.deformations.descriptor(device).address,
            scene_sdfs: self.sdfs.descriptor(device).address,
            scene_pages: self.pages.descriptor(device).address,
            instances: world_tables.map_or(0, |tables| tables.instances.descriptor(device).address),
            lights: world_tables.map_or(0, |tables| tables.lights.descriptor(device).address),
            overrides: self.overrides.address(device),
            prototype_materials: gpu_data.prototype_materials.address(device),
            prototype_sdfs: gpu_data.prototype_sdfs.address(device),
            material_parameters: gpu_data.material_parameters.address(device),
            vertices: gpu_data.vertices.address(device),
            indices: gpu_data.indices.address(device),
            parts: gpu_data.parts.address(device),
            submesh_table: gpu_data.submesh_table.address(device),
            fields: gpu_data.fields.address(device),
            candidates: device.buffer_device_address(gpu_data.micro_candidates.handle()),
            wind_records,
            interaction_field,
            quad_counters,
            page_bytes: gpu_data.pages.address(device),
            page_requests: self.page_request_address(device, frame_slot),
            deformed_vertices: deformed.0,
            prev_deformed_vertices: deformed.1,
            deformation_providers: gpu_data.deformation_providers.address(device),
            deformation_parameters: gpu_data.deformation_parameters.address(device),
            instance_capacity: world_tables
                .map_or(0, |tables| tables.instances.slot_capacity() as u32),
            light_capacity: world_tables.map_or(0, |tables| tables.lights.slot_capacity() as u32),
            coverage_temporal_phase,
            page_request_capacity: PAGE_REQUEST_CAPACITY,
            page_request_budget: self.page_request_budget,
            page_request_classes: crate::SCENE_VIEW_CLASSES as u32,
            reserved: [0; 2],
        }
    }

    /// Publishes `block` into the frame slot's mapped slice.
    pub fn write_address_block(&mut self, frame_slot: usize, block: GpuSceneAddressBlock) {
        let offset = (frame_slot as u64 * ADDRESS_BLOCK_ALIGNMENT) as usize;
        let bytes = bytemuck::bytes_of(&block);
        // SAFETY: HOST_VISIBLE + MAPPED; the slice is owned by `frame_slot`, whose prior
        // GPU reads completed with its fence before this frame reused the slot.
        unsafe {
            std::ptr::copy_nonoverlapping(
                bytes.as_ptr(),
                self.address_blocks.mapped_ptr().add(offset),
                bytes.len(),
            );
        }
    }

    /// The shared-table descriptors.
    pub fn descriptors(&self, device: &Device) -> GpuSceneTableDescriptors {
        GpuSceneTableDescriptors {
            prototypes: self.prototypes.descriptor(device),
            materials: self.materials.descriptor(device),
            deformations: self.deformations.descriptor(device),
            sdfs: self.sdfs.descriptor(device),
            pages: self.pages.descriptor(device),
            overrides: GpuTableDescriptor {
                buffer: self.overrides.buffer(),
                offset: 0,
                range: self.overrides.capacity() * size_of::<GpuSceneOverrideGpuRecord>() as u64,
                address: self.overrides.address(device),
                slot_stride: size_of::<GpuSceneOverrideGpuRecord>() as u64,
            },
        }
    }

    /// One world's instance-table slot capacity, zero before the world uploads anything.
    pub fn world_instance_capacity(&self, world: GpuSceneWorldId) -> u32 {
        self.worlds
            .get(&world.0)
            .map_or(0, |tables| tables.instances.slot_capacity() as u32)
    }

    /// One world's table descriptors, or `None` before the world uploads anything.
    pub fn world_descriptors(
        &self,
        device: &Device,
        world: GpuSceneWorldId,
    ) -> Option<GpuSceneWorldDescriptors> {
        self.worlds
            .get(&world.0)
            .map(|tables| GpuSceneWorldDescriptors {
                instances: tables.instances.descriptor(device),
                lights: tables.lights.descriptor(device),
            })
    }

    /// Reclaims superseded physical buffers after a frame fence signals.
    pub fn begin_frame(&mut self, completed_frame_slot: usize) -> Result<()> {
        self.prototypes.begin_frame(completed_frame_slot)?;
        self.materials.begin_frame(completed_frame_slot)?;
        self.deformations.begin_frame(completed_frame_slot)?;
        self.sdfs.begin_frame(completed_frame_slot)?;
        self.pages.begin_frame(completed_frame_slot)?;
        self.overrides.begin_frame(completed_frame_slot)?;
        for tables in self.worlds.values_mut() {
            tables.instances.begin_frame(completed_frame_slot)?;
            tables.lights.begin_frame(completed_frame_slot)?;
        }
        Ok(())
    }

    fn world_tables(&mut self, device: &Device, world: u64) -> Result<&mut GpuSceneWorldTables> {
        match self.worlds.entry(world) {
            std::collections::btree_map::Entry::Occupied(entry) => Ok(entry.into_mut()),
            std::collections::btree_map::Entry::Vacant(entry) => {
                let tables = GpuSceneWorldTables {
                    instances: GpuSceneTableStorage::new(
                        device,
                        size_of::<GpuSceneInstanceGpuRecord>() as u64,
                        INITIAL_INSTANCE_SLOTS,
                    )?,
                    lights: GpuSceneTableStorage::new(
                        device,
                        size_of::<GpuSceneLightGpuRecord>() as u64,
                        INITIAL_LIGHT_SLOTS,
                    )?,
                };
                Ok(entry.insert(tables))
            }
        }
    }

    /// Drains the persistent scene's coalesced writes for `frame_slot` into graph-owned
    /// transfer passes, growing buffers first so every staged copy targets live storage.
    pub fn record_frame(
        &mut self,
        device: &Device,
        graph: &mut RenderGraph,
        gpu_data: &mut GlobalGpuData,
        gpu_scene: &mut PersistentGpuScene,
        frame_slot: usize,
    ) -> Result<GpuSceneUploadRunStats> {
        let mut stats = GpuSceneUploadRunStats::default();
        loop {
            let batch = gpu_scene.stage_upload_batch(frame_slot)?;
            if batch.ranges.is_empty() {
                stats.budget_exhausted |= batch.more_pending;
                break;
            }
            stats.batches += 1;
            stats.records += batch.record_count as u32;
            stats.table_bytes += batch.byte_count as u64;

            self.reserve_batch_capacity(device, gpu_data, &batch)?;
            stats.growths += self.enqueue_growth(device, graph, gpu_data)?;
            self.stage_batch(device, graph, gpu_data, frame_slot, &batch)?;

            if batch.frame_budget_exhausted {
                stats.budget_exhausted = batch.more_pending;
                break;
            }
            if !batch.more_pending {
                break;
            }
        }
        Ok(stats)
    }

    /// Reserves table slots and variable-length arena ranges for every record in `batch`.
    fn reserve_batch_capacity(
        &mut self,
        device: &Device,
        gpu_data: &mut GlobalGpuData,
        batch: &crate::GpuSceneUploadBatch,
    ) -> Result<()> {
        for range in &batch.ranges {
            let end = u64::from(range.first_slot) + range.records.len() as u64;
            match range.target {
                GpuSceneUploadTarget::Prototype => self.prototypes.ensure_slots(end)?,
                GpuSceneUploadTarget::Material => self.materials.ensure_slots(end)?,
                GpuSceneUploadTarget::Deformation => self.deformations.ensure_slots(end)?,
                GpuSceneUploadTarget::Sdf => self.sdfs.ensure_slots(end)?,
                GpuSceneUploadTarget::Page => self.pages.ensure_slots(end)?,
                GpuSceneUploadTarget::Instance(world) => {
                    self.world_tables(device, world.0)?
                        .instances
                        .ensure_slots(end)?;
                }
                GpuSceneUploadTarget::Light(world) => {
                    self.world_tables(device, world.0)?
                        .lights
                        .ensure_slots(end)?;
                }
            }
            for (index, record) in range.records.iter().enumerate() {
                let slot = range.first_slot + index as u32;
                match (&range.target, &record.payload) {
                    (GpuSceneUploadTarget::Prototype, GpuSceneUploadPayload::Prototype(p)) => {
                        ensure_range(
                            &mut self.prototype_ranges,
                            &mut gpu_data.prototype_materials,
                            slot,
                            p.materials.len(),
                        )?;
                        ensure_range(
                            &mut self.prototype_sdf_ranges,
                            &mut gpu_data.prototype_sdfs,
                            slot,
                            p.sdfs.len(),
                        )?;
                    }
                    (GpuSceneUploadTarget::Prototype, GpuSceneUploadPayload::Tombstone) => {
                        if let Some(range) = self.prototype_ranges.remove(&slot) {
                            gpu_data.prototype_materials.retire(range)?;
                        }
                        if let Some(range) = self.prototype_sdf_ranges.remove(&slot) {
                            gpu_data.prototype_sdfs.retire(range)?;
                        }
                    }
                    (
                        GpuSceneUploadTarget::Instance(world),
                        GpuSceneUploadPayload::Instance(instance),
                    ) => {
                        ensure_range(
                            &mut self.override_ranges,
                            &mut self.overrides,
                            (world.0, slot),
                            instance.material_overrides.len(),
                        )?;
                    }
                    (GpuSceneUploadTarget::Instance(world), GpuSceneUploadPayload::Tombstone) => {
                        if let Some(range) = self.override_ranges.remove(&(world.0, slot)) {
                            self.overrides.retire(range)?;
                        }
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    /// Enqueues every pending buffer growth; the preserving copies precede all staged
    /// writes recorded after this call.
    fn enqueue_growth(
        &mut self,
        device: &Device,
        graph: &mut RenderGraph,
        gpu_data: &mut GlobalGpuData,
    ) -> Result<u32> {
        let mut growths = 0;
        let mut push = |growth: Option<crate::GpuArenaGrowth>, name: &str| {
            if let Some(growth) = growth {
                growth.enqueue(graph, device, name);
                growths += 1;
            }
        };
        push(
            self.prototypes.prepare_growth(device)?,
            "scene-prototypes grow",
        );
        push(
            self.materials.prepare_growth(device)?,
            "scene-materials grow",
        );
        push(
            self.deformations.prepare_growth(device)?,
            "scene-deformations grow",
        );
        push(self.sdfs.prepare_growth(device)?, "scene-sdfs grow");
        push(self.pages.prepare_growth(device)?, "scene-pages grow");
        push(
            self.overrides.prepare_growth(device)?,
            "scene-overrides grow",
        );
        push(
            gpu_data.prototype_materials.prepare_growth(device)?,
            "prototype-materials grow",
        );
        push(
            gpu_data.prototype_sdfs.prepare_growth(device)?,
            "prototype-sdfs grow",
        );
        for (world, tables) in &mut self.worlds {
            push(
                tables.instances.prepare_growth(device)?,
                &format!("scene-instances[{world}] grow"),
            );
            push(
                tables.lights.prepare_growth(device)?,
                &format!("scene-lights[{world}] grow"),
            );
        }
        Ok(growths)
    }

    /// Serializes and stages every record in `batch`, enqueuing one transfer per slot
    /// write plus one per variable-length arena range.
    fn stage_batch(
        &mut self,
        device: &Device,
        graph: &mut RenderGraph,
        gpu_data: &mut GlobalGpuData,
        frame_slot: usize,
        batch: &crate::GpuSceneUploadBatch,
    ) -> Result<()> {
        let GlobalGpuData {
            prototype_materials,
            prototype_sdfs,
            uploads,
            ..
        } = gpu_data;
        for range in &batch.ranges {
            for (index, record) in range.records.iter().enumerate() {
                let slot = range.first_slot + index as u32;
                let generation = record.generation;
                let upload = match (&range.target, &record.payload) {
                    (target, GpuSceneUploadPayload::Tombstone) => {
                        self.tombstone(target, uploads, frame_slot, slot, generation)?
                    }
                    (GpuSceneUploadTarget::Prototype, GpuSceneUploadPayload::Prototype(p)) => {
                        let material_range = self
                            .prototype_ranges
                            .get(&slot)
                            .copied()
                            .unwrap_or_default();
                        if material_range.count > 0 {
                            let handles: Vec<GpuHandle> =
                                p.materials.iter().map(|handle| handle.raw()).collect();
                            prototype_materials
                                .stage(
                                    uploads,
                                    frame_slot,
                                    material_range,
                                    bytemuck::cast_slice(&handles),
                                )?
                                .enqueue(graph, device, "scene-prototype materials");
                        }
                        let sdf_range = self
                            .prototype_sdf_ranges
                            .get(&slot)
                            .copied()
                            .unwrap_or_default();
                        if sdf_range.count > 0 {
                            let handles: Vec<GpuHandle> =
                                p.sdfs.iter().map(|handle| handle.raw()).collect();
                            prototype_sdfs
                                .stage(
                                    uploads,
                                    frame_slot,
                                    sdf_range,
                                    bytemuck::cast_slice(&handles),
                                )?
                                .enqueue(graph, device, "scene-prototype sdfs");
                        }
                        let body = prototype_body(p, material_range, sdf_range);
                        self.prototypes.stage_slot(
                            uploads,
                            frame_slot,
                            slot,
                            generation,
                            1,
                            bytemuck::bytes_of(&body),
                        )?
                    }
                    (GpuSceneUploadTarget::Material, GpuSceneUploadPayload::Material(m)) => {
                        let body = GpuSceneReferenceGpuRecord {
                            target: m.table,
                            source_revision: m.source_revision,
                        };
                        self.materials.stage_slot(
                            uploads,
                            frame_slot,
                            slot,
                            generation,
                            1,
                            bytemuck::bytes_of(&body),
                        )?
                    }
                    (GpuSceneUploadTarget::Deformation, GpuSceneUploadPayload::Deformation(d)) => {
                        let body = GpuSceneReferenceGpuRecord {
                            target: d.provider,
                            source_revision: d.source_revision,
                        };
                        self.deformations.stage_slot(
                            uploads,
                            frame_slot,
                            slot,
                            generation,
                            1,
                            bytemuck::bytes_of(&body),
                        )?
                    }
                    (GpuSceneUploadTarget::Sdf, GpuSceneUploadPayload::Sdf(s)) => {
                        let body = GpuSceneReferenceGpuRecord {
                            target: s.resource,
                            source_revision: s.source_revision,
                        };
                        self.sdfs.stage_slot(
                            uploads,
                            frame_slot,
                            slot,
                            generation,
                            1,
                            bytemuck::bytes_of(&body),
                        )?
                    }
                    (GpuSceneUploadTarget::Page, GpuSceneUploadPayload::Page(p)) => {
                        let body = GpuScenePageGpuRecord {
                            table: p.table,
                            parent: p.parent.map_or(GpuHandle::INVALID, |parent| parent.raw()),
                            source_generation: p.source_generation,
                            flags: p.flags,
                        };
                        self.pages.stage_slot(
                            uploads,
                            frame_slot,
                            slot,
                            generation,
                            1,
                            bytemuck::bytes_of(&body),
                        )?
                    }
                    (
                        GpuSceneUploadTarget::Instance(world),
                        GpuSceneUploadPayload::Instance(instance),
                    ) => {
                        let override_range = self
                            .override_ranges
                            .get(&(world.0, slot))
                            .copied()
                            .unwrap_or_default();
                        if override_range.count > 0 {
                            let elements: Vec<GpuSceneOverrideGpuRecord> = instance
                                .material_overrides
                                .iter()
                                .map(|element| GpuSceneOverrideGpuRecord {
                                    slot: element.slot,
                                    reserved: 0,
                                    material: element.material.raw(),
                                })
                                .collect();
                            self.overrides
                                .stage(
                                    uploads,
                                    frame_slot,
                                    override_range,
                                    bytemuck::cast_slice(&elements),
                                )?
                                .enqueue(graph, device, "scene-instance overrides");
                        }
                        let body = instance_body(instance, override_range);
                        let tables = self.worlds.get(&world.0).expect("world tables reserved");
                        tables.instances.stage_slot(
                            uploads,
                            frame_slot,
                            slot,
                            generation,
                            1,
                            bytemuck::bytes_of(&body),
                        )?
                    }
                    (GpuSceneUploadTarget::Light(world), GpuSceneUploadPayload::Light(l)) => {
                        let body = GpuSceneLightGpuRecord {
                            light: l.light,
                            source_revision: l.source_revision,
                            reserved: 0,
                        };
                        let tables = self.worlds.get(&world.0).expect("world tables reserved");
                        tables.lights.stage_slot(
                            uploads,
                            frame_slot,
                            slot,
                            generation,
                            1,
                            bytemuck::bytes_of(&body),
                        )?
                    }
                    (target, payload) => {
                        return Err(Error::InvalidUploadData(format!(
                            "scene upload payload {payload:?} does not belong to target {target:?}"
                        )));
                    }
                };
                upload.enqueue(graph, device, "scene-table slot");
            }
        }
        Ok(())
    }

    fn tombstone(
        &mut self,
        target: &GpuSceneUploadTarget,
        uploads: &mut FrameUploadRing,
        frame_slot: usize,
        slot: u32,
        generation: u32,
    ) -> Result<GpuBufferUpload> {
        match target {
            GpuSceneUploadTarget::Prototype => {
                self.prototypes
                    .stage_slot(uploads, frame_slot, slot, generation, 0, &[])
            }
            GpuSceneUploadTarget::Material => {
                self.materials
                    .stage_slot(uploads, frame_slot, slot, generation, 0, &[])
            }
            GpuSceneUploadTarget::Deformation => {
                self.deformations
                    .stage_slot(uploads, frame_slot, slot, generation, 0, &[])
            }
            GpuSceneUploadTarget::Sdf => {
                self.sdfs
                    .stage_slot(uploads, frame_slot, slot, generation, 0, &[])
            }
            GpuSceneUploadTarget::Page => {
                self.pages
                    .stage_slot(uploads, frame_slot, slot, generation, 0, &[])
            }
            GpuSceneUploadTarget::Instance(world) => {
                let tables = self.worlds.get(&world.0).expect("world tables reserved");
                tables
                    .instances
                    .stage_slot(uploads, frame_slot, slot, generation, 0, &[])
            }
            GpuSceneUploadTarget::Light(world) => {
                let tables = self.worlds.get(&world.0).expect("world tables reserved");
                tables
                    .lights
                    .stage_slot(uploads, frame_slot, slot, generation, 0, &[])
            }
        }
    }
}

/// Reuses or (re)allocates the variable-length arena range backing one slot.
fn ensure_range<K: std::hash::Hash + Eq + Copy, A>(
    map: &mut HashMap<K, GpuArenaRange>,
    arena: &mut GlobalGpuArena<A>,
    key: K,
    element_count: usize,
) -> Result<()> {
    let count = u32::try_from(element_count)
        .map_err(|_| Error::InvalidUploadData("scene range element count overflow".to_owned()))?;
    if let Some(existing) = map.get(&key) {
        if existing.count == count {
            return Ok(());
        }
        arena.retire(*existing)?;
        map.remove(&key);
    }
    if count == 0 {
        return Ok(());
    }
    let (range, _) = arena.allocate(count, 1)?;
    map.insert(key, range);
    Ok(())
}

fn prototype_body(
    record: &GpuScenePrototypeRecord,
    material_range: GpuArenaRange,
    sdf_range: GpuArenaRange,
) -> GpuScenePrototypeGpuRecord {
    GpuScenePrototypeGpuRecord {
        geometry: record.geometry,
        material_range,
        deformation: record
            .deformation
            .map_or(GpuHandle::INVALID, |handle| handle.raw()),
        sdf_range,
        root_page: record.root_page.raw(),
        bounds: record.bounds,
        source_generation: record.source_generation,
        flags: record.flags,
        mechanics: record.mechanics,
    }
}

fn instance_body(
    record: &GpuSceneInstanceRecord,
    override_range: GpuArenaRange,
) -> GpuSceneInstanceGpuRecord {
    let mut body = GpuSceneInstanceGpuRecord {
        prototype: record.prototype.raw(),
        deformation: record
            .deformation
            .map_or(GpuHandle::INVALID, |handle| handle.raw()),
        reserved_pad: [0; 2],
        material_overrides: override_range,
        transform_kind: GPU_SCENE_TRANSFORM_STATIC,
        source_generation: record.source_generation,
        flags: record.flags,
        reserved: record.combination,
        transform: [0.0; 32],
    };
    match record.transform {
        GpuSceneTransform::Static(compact) => {
            body.transform_kind = GPU_SCENE_TRANSFORM_STATIC;
            let bytes = bytemuck::bytes_of(&compact);
            bytemuck::cast_slice_mut::<f32, u8>(&mut body.transform)[..bytes.len()]
                .copy_from_slice(bytes);
            // Vegetation columns ride the static payload's free words: bounds
            // spheres at 16..24, the attachment identity at 24..30, the combination
            // crossfade (previous combination + flip stamp) at 30..32.
            if let Some(vegetation) = &record.vegetation {
                body.transform[16..20].copy_from_slice(&vegetation.bounds_current);
                body.transform[20..24].copy_from_slice(&vegetation.bounds_previous);
                body.transform[30] = f32::from_bits(vegetation.combination_previous);
                body.transform[31] = f32::from_bits(vegetation.flip_stamp);
                if let Some(attachment) = &vegetation.attachment {
                    body.transform[24] = f32::from_bits(attachment.provider as u32);
                    body.transform[25] = f32::from_bits((attachment.provider >> 32) as u32);
                    body.transform[26] = f32::from_bits(attachment.primitive as u32);
                    body.transform[27] = f32::from_bits((attachment.primitive >> 32) as u32);
                    body.transform[28] = f32::from_bits(
                        u32::from(attachment.barycentric[0])
                            | (u32::from(attachment.barycentric[1]) << 16),
                    );
                    body.transform[29] = f32::from_bits(u32::from(attachment.barycentric[2]));
                }
            }
        }
        GpuSceneTransform::Dynamic(dynamic) => {
            body.transform_kind = GPU_SCENE_TRANSFORM_DYNAMIC;
            body.transform[..16].copy_from_slice(&dynamic.current.to_cols_array());
            body.transform[16..].copy_from_slice(&dynamic.previous.to_cols_array());
        }
    }
    body
}

/// One queued arena byte upload from the asset mirror.
pub enum GpuArenaUploadRequest {
    /// Vertex bytes into [`GlobalGpuData::vertices`] at `range`.
    Vertices {
        /// Destination byte range.
        range: GpuArenaRange,
        /// The retained CPU vertex stream.
        data: Arc<[saffron_geometry::Vertex]>,
    },
    /// Index bytes into [`GlobalGpuData::indices`] at `range`.
    Indices {
        /// Destination byte range.
        range: GpuArenaRange,
        /// The retained CPU index stream.
        data: Arc<[u32]>,
    },
    /// One material parameter block into [`GlobalGpuData::material_parameters`] at `range`.
    MaterialParams {
        /// Destination element range (one block).
        range: GpuArenaRange,
        /// The packed std430 parameter block.
        data: Box<MaterialParamsData>,
    },
    /// Submesh records into [`GlobalGpuData::submesh_table`] at `range`.
    Submeshes {
        /// Destination element range.
        range: GpuArenaRange,
        /// The geometry's submesh records.
        data: Vec<crate::GpuSubmeshRecord>,
    },
    /// One resident page's payload bytes into [`GlobalGpuData::pages`] at `range`.
    PageBytes {
        /// Destination byte range.
        range: GpuArenaRange,
        /// The locked page payload.
        data: Vec<u8>,
    },
    /// One geometry's assembly-part table into [`GlobalGpuData::parts`] at `range` —
    /// the prototype records followed by the use records.
    Parts {
        /// Destination byte range.
        range: GpuArenaRange,
        /// The packed prototype + use records.
        data: Vec<u8>,
    },
    /// One cell's packed micro field tiles into [`GlobalGpuData::fields`] at `range`.
    Fields {
        /// Destination byte range.
        range: GpuArenaRange,
        /// The packed tile headers + density samples.
        data: Vec<u8>,
    },
    /// Deformation-provider records into [`GlobalGpuData::deformation_providers`].
    DeformationProviders {
        /// Destination element range.
        range: GpuArenaRange,
        /// The provider records.
        data: Vec<crate::GpuDeformationProviderRecord>,
    },
    /// Provider parameter words into [`GlobalGpuData::deformation_parameters`].
    DeformationParameters {
        /// Destination element range.
        range: GpuArenaRange,
        /// The parameter words.
        data: Vec<u32>,
    },
}

/// Pending resident-table and arena uploads queued by the asset mirror, drained into
/// graph transfer passes by [`record_pending_global_uploads`].
#[derive(Default)]
pub struct GpuScenePendingUploads {
    records: Vec<(GlobalGpuTableKind, GpuHandle)>,
    retires: Vec<(GlobalGpuTableKind, GpuHandle)>,
    arenas: Vec<GpuArenaUploadRequest>,
}

impl GpuScenePendingUploads {
    /// Queues one live record's bytes for staging.
    pub fn stage_record(&mut self, kind: GlobalGpuTableKind, handle: GpuHandle) {
        self.records.push((kind, handle));
    }

    /// Queues one record's fence-safe retirement and GPU tombstone.
    pub fn retire_record(&mut self, kind: GlobalGpuTableKind, handle: GpuHandle) {
        self.retires.push((kind, handle));
    }

    /// Queues one arena byte upload.
    pub fn upload_arena(&mut self, request: GpuArenaUploadRequest) {
        self.arenas.push(request);
    }

    /// Total queued operations.
    pub fn len(&self) -> usize {
        self.records.len() + self.retires.len() + self.arenas.len()
    }

    /// Whether nothing is queued.
    pub fn is_empty(&self) -> bool {
        self.records.is_empty() && self.retires.is_empty() && self.arenas.is_empty()
    }
}

/// Drains the pending resident-table stages, arena uploads, and retirements into graph
/// transfer passes, growing device buffers first.
pub fn record_pending_global_uploads(
    pending: &mut GpuScenePendingUploads,
    device: &Device,
    graph: &mut RenderGraph,
    gpu_data: &mut GlobalGpuData,
    frame_slot: usize,
) -> Result<()> {
    if pending.is_empty() {
        return Ok(());
    }
    let enqueue_growth =
        |growth: Option<crate::GpuArenaGrowth>, name: &str, graph: &mut RenderGraph| {
            if let Some(growth) = growth {
                growth.enqueue(graph, device, name);
            }
        };
    enqueue_growth(
        gpu_data.prototypes.prepare_growth(device)?,
        "prototypes grow",
        graph,
    );
    enqueue_growth(
        gpu_data.geometries.prepare_growth(device)?,
        "geometries grow",
        graph,
    );
    enqueue_growth(
        gpu_data.materials.prepare_growth(device)?,
        "materials grow",
        graph,
    );
    enqueue_growth(
        gpu_data.textures.prepare_growth(device)?,
        "textures grow",
        graph,
    );
    enqueue_growth(
        gpu_data.coverage.prepare_growth(device)?,
        "coverage grow",
        graph,
    );
    enqueue_growth(
        gpu_data.skeletons.prepare_growth(device)?,
        "skeletons grow",
        graph,
    );
    enqueue_growth(
        gpu_data.sdfs.prepare_growth(device)?,
        "sdf-table grow",
        graph,
    );
    enqueue_growth(
        gpu_data.page_table.prepare_growth(device)?,
        "page-table grow",
        graph,
    );
    enqueue_growth(
        gpu_data.vertices.prepare_growth(device)?,
        "vertices grow",
        graph,
    );
    enqueue_growth(
        gpu_data.indices.prepare_growth(device)?,
        "indices grow",
        graph,
    );
    enqueue_growth(
        gpu_data.material_parameters.prepare_growth(device)?,
        "material-parameters grow",
        graph,
    );
    enqueue_growth(
        gpu_data.submesh_table.prepare_growth(device)?,
        "submesh-table grow",
        graph,
    );
    enqueue_growth(gpu_data.pages.prepare_growth(device)?, "pages grow", graph);
    enqueue_growth(gpu_data.parts.prepare_growth(device)?, "parts grow", graph);
    enqueue_growth(
        gpu_data.fields.prepare_growth(device)?,
        "fields grow",
        graph,
    );
    enqueue_growth(
        gpu_data.deformed_vertices.prepare_growth(device)?,
        "deformed-vertices grow",
        graph,
    );
    enqueue_growth(
        gpu_data.prev_deformed_vertices.prepare_growth(device)?,
        "prev-deformed-vertices grow",
        graph,
    );
    enqueue_growth(
        gpu_data.deformation_providers.prepare_growth(device)?,
        "deformation-providers grow",
        graph,
    );
    enqueue_growth(
        gpu_data.deformation_parameters.prepare_growth(device)?,
        "deformation-parameters grow",
        graph,
    );

    let GlobalGpuData {
        prototypes,
        geometries,
        materials,
        textures,
        coverage,
        skeletons,
        sdfs,
        page_table,
        vertices,
        indices,
        material_parameters,
        submesh_table,
        pages,
        parts,
        fields,
        deformation_providers,
        deformation_parameters,
        uploads,
        ..
    } = gpu_data;

    for (kind, handle) in pending.records.drain(..) {
        let upload = match kind {
            GlobalGpuTableKind::Prototype => stage_if_live(prototypes, uploads, frame_slot, handle),
            GlobalGpuTableKind::Geometry => stage_if_live(geometries, uploads, frame_slot, handle),
            GlobalGpuTableKind::Material => stage_if_live(materials, uploads, frame_slot, handle),
            GlobalGpuTableKind::Texture => stage_if_live(textures, uploads, frame_slot, handle),
            GlobalGpuTableKind::Coverage => stage_if_live(coverage, uploads, frame_slot, handle),
            GlobalGpuTableKind::Skeleton => stage_if_live(skeletons, uploads, frame_slot, handle),
            GlobalGpuTableKind::Sdf => stage_if_live(sdfs, uploads, frame_slot, handle),
            GlobalGpuTableKind::Page => stage_if_live(page_table, uploads, frame_slot, handle),
        }?;
        if let Some(upload) = upload {
            upload.enqueue(graph, device, "global-table record");
        }
    }

    for request in pending.arenas.drain(..) {
        let upload = match request {
            GpuArenaUploadRequest::Vertices { range, data } => {
                vertices.stage(uploads, frame_slot, range, bytemuck::cast_slice(&data))?
            }
            GpuArenaUploadRequest::Indices { range, data } => {
                indices.stage(uploads, frame_slot, range, bytemuck::cast_slice(&data))?
            }
            GpuArenaUploadRequest::MaterialParams { range, data } => {
                material_parameters.stage(uploads, frame_slot, range, bytemuck::bytes_of(&*data))?
            }
            GpuArenaUploadRequest::Submeshes { range, data } => {
                submesh_table.stage(uploads, frame_slot, range, bytemuck::cast_slice(&data))?
            }
            GpuArenaUploadRequest::PageBytes { range, data } => {
                pages.stage(uploads, frame_slot, range, &data)?
            }
            GpuArenaUploadRequest::Parts { range, data } => {
                parts.stage(uploads, frame_slot, range, &data)?
            }
            GpuArenaUploadRequest::Fields { range, data } => {
                fields.stage(uploads, frame_slot, range, &data)?
            }
            GpuArenaUploadRequest::DeformationProviders { range, data } => deformation_providers
                .stage(uploads, frame_slot, range, bytemuck::cast_slice(&data))?,
            GpuArenaUploadRequest::DeformationParameters { range, data } => deformation_parameters
                .stage(uploads, frame_slot, range, bytemuck::cast_slice(&data))?,
        };
        upload.enqueue(graph, device, "global-arena bytes");
    }

    for (kind, handle) in pending.retires.drain(..) {
        let tombstone = match kind {
            GlobalGpuTableKind::Prototype => prototypes
                .retire(uploads, frame_slot, handle)?
                .map(|retirement| retirement.tombstone),
            GlobalGpuTableKind::Geometry => geometries
                .retire(uploads, frame_slot, handle)?
                .map(|retirement| retirement.tombstone),
            GlobalGpuTableKind::Material => materials
                .retire(uploads, frame_slot, handle)?
                .map(|retirement| retirement.tombstone),
            GlobalGpuTableKind::Texture => textures
                .retire(uploads, frame_slot, handle)?
                .map(|retirement| retirement.tombstone),
            GlobalGpuTableKind::Coverage => coverage
                .retire(uploads, frame_slot, handle)?
                .map(|retirement| retirement.tombstone),
            GlobalGpuTableKind::Skeleton => skeletons
                .retire(uploads, frame_slot, handle)?
                .map(|retirement| retirement.tombstone),
            GlobalGpuTableKind::Sdf => sdfs
                .retire(uploads, frame_slot, handle)?
                .map(|retirement| retirement.tombstone),
            GlobalGpuTableKind::Page => page_table
                .retire(uploads, frame_slot, handle)?
                .map(|retirement| retirement.tombstone),
        };
        if let Some(tombstone) = tombstone {
            tombstone.enqueue(graph, device, "global-table tombstone");
        }
    }
    Ok(())
}

fn stage_if_live<T: bytemuck::Pod, K>(
    table: &crate::ResidentGpuTable<T, K>,
    uploads: &mut FrameUploadRing,
    frame_slot: usize,
    handle: GpuHandle,
) -> Result<Option<GpuBufferUpload>> {
    if table.get(handle).is_none() {
        return Ok(None);
    }
    table.stage(uploads, frame_slot, handle).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::validation_issue_count;
    use crate::persistent_gpu_scene::{
        GpuSceneDynamicTransform, GpuSceneLightRecord, GpuSceneMaterialOverride,
        GpuSceneMaterialRecord, GpuScenePageRecord, GpuSceneSharedDelta, GpuSceneSharedDeltaResult,
        GpuSceneUploadLimits, GpuSceneWorldDelta, GpuSceneWorldDeltaResult,
    };
    use crate::{Device, GpuLight, SurfaceSource};
    use ash::vk;
    use saffron_geometry::glam::{Mat4, Vec3, Vec4};

    const WORLD: GpuSceneWorldId = GpuSceneWorldId(0);

    fn device_or_skip() -> Option<Device> {
        match Device::new(&SurfaceSource::Offscreen) {
            Ok(device) => Some(device),
            Err(err) => {
                eprintln!("skipping (no Vulkan device): {err}");
                None
            }
        }
    }

    fn device_handle(index: u32) -> GpuHandle {
        GpuHandle {
            index,
            generation: 1,
        }
    }

    /// Appends `slots` to `class`'s region of frame slot 0 exactly as the shader would,
    /// including the count word running past the budget on the ones that did not fit.
    fn append_page_requests(
        uploader: &GpuSceneUploader,
        class: crate::SceneViewClass,
        slots: &[u32],
    ) {
        let region = class.ordinal() as usize;
        let budget = uploader.page_request_budget() as usize;
        // SAFETY: HOST_VISIBLE + MAPPED, nothing in flight in this test, and every offset
        // is inside frame slot 0's own slice.
        unsafe {
            let count_ptr = uploader
                .page_requests
                .mapped_ptr()
                .add(region * 4)
                .cast::<u32>();
            let region_base = PAGE_REQUEST_HEADER_BYTES
                + region * PAGE_REQUEST_CAPACITY as usize * PAGE_REQUEST_ENTRY_BYTES;
            for slot in slots {
                let index = count_ptr.read_unaligned();
                count_ptr.write_unaligned(index + 1);
                if (index as usize) < budget {
                    uploader
                        .page_requests
                        .mapped_ptr()
                        .add(region_base + index as usize * PAGE_REQUEST_ENTRY_BYTES)
                        .cast::<u32>()
                        .write_unaligned(*slot);
                }
            }
        }
    }

    /// A class's region is its own: what it loses is decided by its own volume, and what
    /// it keeps cannot be taken by a louder neighbour.
    ///
    /// This is the property the whole partition exists for. Before it, every view in the
    /// frame appended to one queue in atomic order, so a gather sweeping a hundred-metre
    /// box could crowd out the camera — and a dropped camera request is a page the image
    /// is made of arriving a frame late, repeatedly, with nothing saying so.
    #[test]
    fn a_flooded_class_loses_only_its_own_requests() {
        let Some(device) = device_or_skip() else {
            return;
        };
        let mut uploader = GpuSceneUploader::new(&device).expect("uploader");
        uploader.set_page_request_budget(4);

        append_page_requests(&uploader, crate::SceneViewClass::Camera, &[10, 11]);
        // The gather raises three times what its region holds.
        let flood: Vec<u32> = (100..112).collect();
        append_page_requests(&uploader, crate::SceneViewClass::Gi, &flood);
        append_page_requests(&uploader, crate::SceneViewClass::ShadowPage, &[50]);

        let drain = uploader.drain_page_requests(0);
        assert_eq!(
            drain.dropped, 8,
            "only the gather's overflow is lost (12 raised, 4 held)"
        );
        assert_eq!(
            drain.overflow_classes,
            crate::SceneViewClass::Gi.bit(),
            "and it is named as the gather's"
        );
        let camera: Vec<u32> = drain
            .requests
            .iter()
            .filter(|(_, class)| *class == crate::SceneViewClass::Camera)
            .map(|(slot, _)| *slot)
            .collect();
        assert_eq!(
            camera,
            vec![10, 11],
            "the camera keeps every request it made"
        );
        assert!(
            drain
                .requests
                .iter()
                .any(|(slot, class)| *slot == 50 && *class == crate::SceneViewClass::ShadowPage),
            "so does the shadow view"
        );

        // Draining resets every count word, so the next frame starts clean.
        assert_eq!(uploader.drain_page_requests(0), PageRequestDrain::default());
        device.wait_idle().expect("idle");
    }

    /// A page several classes missed is streamed once, under the most urgent of them.
    /// Taking whichever arrived first would let a gather set the priority of a page the
    /// camera is waiting on, and eviction would then price it as the gather's.
    #[test]
    fn a_page_two_classes_missed_arrives_once_at_the_higher_band() {
        let Some(device) = device_or_skip() else {
            return;
        };
        let mut uploader = GpuSceneUploader::new(&device).expect("uploader");
        append_page_requests(&uploader, crate::SceneViewClass::Gi, &[7]);
        append_page_requests(&uploader, crate::SceneViewClass::Camera, &[7]);

        let drain = uploader.drain_page_requests(0);
        assert_eq!(drain.requests, vec![(7, crate::SceneViewClass::Camera)]);
        assert_eq!(drain.dropped, 0);
        assert_eq!(drain.overflow_classes, 0);
        device.wait_idle().expect("idle");
    }

    /// Records and submits `graph` on a throwaway pool, waiting for completion.
    fn run_graph(device: &Device, graph: &mut RenderGraph) {
        let raw = device.raw();
        let pool_info =
            vk::CommandPoolCreateInfo::default().queue_family_index(device.graphics_queue_family);
        // SAFETY: the ash seam. Pool/cmd/fence are destroyed after the wait below.
        unsafe {
            let pool = raw.create_command_pool(&pool_info, None).expect("pool");
            let alloc = vk::CommandBufferAllocateInfo::default()
                .command_pool(pool)
                .level(vk::CommandBufferLevel::PRIMARY)
                .command_buffer_count(1);
            let cmd = raw.allocate_command_buffers(&alloc).expect("cmd")[0];
            let fence = raw
                .create_fence(&vk::FenceCreateInfo::default(), None)
                .expect("fence");
            raw.begin_command_buffer(
                cmd,
                &vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )
            .expect("begin");
            graph.execute(device, cmd);
            raw.end_command_buffer(cmd).expect("end");
            let cmd_info = [vk::CommandBufferSubmitInfo::default().command_buffer(cmd)];
            let submit = [vk::SubmitInfo2::default().command_buffer_infos(&cmd_info)];
            device
                .graphics_queue
                .submit2(raw, &submit, fence, "gpu-scene upload test")
                .expect("submit");
            raw.wait_for_fences(&[fence], true, u64::MAX).expect("wait");
            raw.destroy_fence(fence, None);
            raw.destroy_command_pool(pool, None);
        }
    }

    /// Copies `bytes` from a device-local buffer into host memory.
    fn read_device_buffer(device: &Device, buffer: vk::Buffer, bytes: u64) -> Vec<u8> {
        let staging = crate::Buffer::new(
            device.resources(),
            bytes,
            vk::BufferUsageFlags::TRANSFER_DST,
            &vk_mem::AllocationCreateInfo {
                usage: vk_mem::MemoryUsage::Auto,
                flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_RANDOM
                    | vk_mem::AllocationCreateFlags::MAPPED,
                ..Default::default()
            },
        )
        .expect("staging");
        let raw = device.raw();
        let pool_info =
            vk::CommandPoolCreateInfo::default().queue_family_index(device.graphics_queue_family);
        // SAFETY: the ash seam. One-off copy; everything destroyed after the wait.
        unsafe {
            let pool = raw.create_command_pool(&pool_info, None).expect("pool");
            let alloc = vk::CommandBufferAllocateInfo::default()
                .command_pool(pool)
                .level(vk::CommandBufferLevel::PRIMARY)
                .command_buffer_count(1);
            let cmd = raw.allocate_command_buffers(&alloc).expect("cmd")[0];
            let fence = raw
                .create_fence(&vk::FenceCreateInfo::default(), None)
                .expect("fence");
            raw.begin_command_buffer(
                cmd,
                &vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )
            .expect("begin");
            let barrier = vk::MemoryBarrier2::default()
                .src_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
                .src_access_mask(vk::AccessFlags2::MEMORY_WRITE)
                .dst_stage_mask(vk::PipelineStageFlags2::TRANSFER)
                .dst_access_mask(vk::AccessFlags2::TRANSFER_READ);
            let barriers = [barrier];
            raw.cmd_pipeline_barrier2(
                cmd,
                &vk::DependencyInfo::default().memory_barriers(&barriers),
            );
            let region = vk::BufferCopy {
                src_offset: 0,
                dst_offset: 0,
                size: bytes,
            };
            raw.cmd_copy_buffer(cmd, buffer, staging.handle(), &[region]);
            raw.end_command_buffer(cmd).expect("end");
            let cmd_info = [vk::CommandBufferSubmitInfo::default().command_buffer(cmd)];
            let submit = [vk::SubmitInfo2::default().command_buffer_infos(&cmd_info)];
            device
                .graphics_queue
                .submit2(raw, &submit, fence, "gpu-scene readback")
                .expect("submit");
            raw.wait_for_fences(&[fence], true, u64::MAX).expect("wait");
            raw.destroy_fence(fence, None);
            raw.destroy_command_pool(pool, None);
        }
        let mut out = vec![0_u8; bytes as usize];
        // SAFETY: HOST_VISIBLE + MAPPED; the copy completed under the fence.
        unsafe {
            std::ptr::copy_nonoverlapping(staging.mapped_ptr(), out.as_mut_ptr(), out.len());
        }
        out
    }

    fn slot_bytes(all: &[u8], stride: u64, slot: u32) -> &[u8] {
        let start = (u64::from(slot) * stride) as usize;
        &all[start..start + stride as usize]
    }

    struct Harness {
        gpu_scene: PersistentGpuScene,
        uploader: GpuSceneUploader,
        gpu_data: GlobalGpuData,
        device: Device,
    }

    fn harness() -> Option<Harness> {
        let device = device_or_skip()?;
        let gpu_data = GlobalGpuData::new(&device).expect("GlobalGpuData");
        let uploader = GpuSceneUploader::new(&device).expect("uploader");
        let mut gpu_scene =
            PersistentGpuScene::new(GpuSceneUploadLimits::default()).expect("scene");
        gpu_scene.create_world(WORLD).expect("world");
        Some(Harness {
            gpu_scene,
            uploader,
            gpu_data,
            device,
        })
    }

    impl Harness {
        fn begin(&mut self, slot: usize) {
            self.gpu_data.begin_frame(slot).expect("gpu data begin");
            self.uploader.begin_frame(slot).expect("uploader begin");
            self.gpu_scene.begin_frame(slot).expect("scene begin");
        }

        fn record_and_run(&mut self, slot: usize) -> GpuSceneUploadRunStats {
            let mut graph = RenderGraph::new();
            let stats = self
                .uploader
                .record_frame(
                    &self.device,
                    &mut graph,
                    &mut self.gpu_data,
                    &mut self.gpu_scene,
                    slot,
                )
                .expect("record frame");
            run_graph(&self.device, &mut graph);
            stats
        }

        fn finish(self) {
            let Harness {
                gpu_scene,
                uploader,
                gpu_data,
                device,
            } = self;
            device.wait_idle().expect("idle");
            drop(gpu_scene);
            drop(uploader);
            drop(gpu_data);
            drop(device);
        }
    }

    #[test]
    fn scene_records_reach_their_device_tables_byte_exact() {
        let Some(mut harness) = harness() else {
            return;
        };
        let before = validation_issue_count();

        let material = match harness
            .gpu_scene
            .apply_shared_delta(GpuSceneSharedDelta::CreateMaterial(
                GpuSceneMaterialRecord {
                    table: device_handle(7),
                    source_revision: 11,
                },
            ))
            .expect("material")
        {
            GpuSceneSharedDeltaResult::MaterialCreated(handle) => handle,
            other => panic!("unexpected {other:?}"),
        };
        let page = match harness
            .gpu_scene
            .apply_shared_delta(GpuSceneSharedDelta::CreatePage(GpuScenePageRecord {
                table: device_handle(9),
                parent: None,
                source_generation: 3,
                flags: crate::GPU_PAGE_FLAG_GUARANTEED_ROOT,
            }))
            .expect("page")
        {
            GpuSceneSharedDeltaResult::PageCreated(handle) => handle,
            other => panic!("unexpected {other:?}"),
        };
        let prototype = match harness
            .gpu_scene
            .apply_shared_delta(GpuSceneSharedDelta::CreatePrototype(
                crate::GpuScenePrototypeRecord {
                    geometry: device_handle(20),
                    materials: std::sync::Arc::from([material]),
                    deformation: None,
                    sdfs: Vec::new().into(),
                    root_page: page,
                    bounds: [1.0, 2.0, 3.0, 4.0],
                    source_generation: 5,
                    flags: 0,
                    mechanics: [0; 4],
                },
            ))
            .expect("prototype")
        {
            GpuSceneSharedDeltaResult::PrototypeCreated(handle) => handle,
            other => panic!("unexpected {other:?}"),
        };
        let current = Mat4::from_translation(Vec3::new(1.0, 2.0, 3.0));
        let previous = Mat4::from_translation(Vec3::new(4.0, 5.0, 6.0));
        let transform = GpuSceneDynamicTransform::new(current, previous).expect("transform");
        let instance = match harness
            .gpu_scene
            .apply_world_delta(
                WORLD,
                GpuSceneWorldDelta::CreateInstance(crate::GpuSceneInstanceRecord {
                    prototype,
                    transform: GpuSceneTransform::Dynamic(transform),
                    material_overrides: std::sync::Arc::from([GpuSceneMaterialOverride {
                        slot: 0,
                        material,
                    }]),
                    deformation: None,
                    source_generation: 6,
                    flags: 0,
                    combination: 0,
                    vegetation: None,
                }),
            )
            .expect("instance")
        {
            GpuSceneWorldDeltaResult::InstanceCreated(handle) => handle,
            other => panic!("unexpected {other:?}"),
        };
        let light_data = GpuLight {
            position_range: Vec4::new(1.0, 2.0, 3.0, 4.0),
            color_intensity: Vec4::new(0.5, 0.25, 0.125, 8.0),
            direction_type: Vec4::ZERO,
            spot_cos: Vec4::new(0.0, 0.0, 1.0, 0.0),
        };
        harness
            .gpu_scene
            .apply_world_delta(
                WORLD,
                GpuSceneWorldDelta::CreateLight(GpuSceneLightRecord {
                    light: light_data,
                    source_revision: 21,
                }),
            )
            .expect("light");

        harness.begin(0);
        let stats = harness.record_and_run(0);
        assert!(stats.records >= 5, "all created records staged");
        assert!(!stats.budget_exhausted);

        let materials_desc = harness.uploader.descriptors(&harness.device).materials;
        let bytes =
            read_device_buffer(&harness.device, materials_desc.buffer, materials_desc.range);
        let slot = slot_bytes(&bytes, materials_desc.slot_stride, material.raw().index);
        let header: &GpuTableSlotHeader = bytemuck::from_bytes(&slot[..16]);
        assert_eq!(header.occupied, 1);
        assert_eq!(header.generation, material.raw().generation);
        let body: &GpuSceneReferenceGpuRecord = bytemuck::from_bytes(&slot[16..32]);
        assert_eq!(body.target, device_handle(7));
        assert_eq!(body.source_revision, 11);

        let proto_desc = harness.uploader.descriptors(&harness.device).prototypes;
        let bytes = read_device_buffer(&harness.device, proto_desc.buffer, proto_desc.range);
        let slot = slot_bytes(&bytes, proto_desc.slot_stride, prototype.raw().index);
        let body: &GpuScenePrototypeGpuRecord =
            bytemuck::from_bytes(&slot[16..16 + size_of::<GpuScenePrototypeGpuRecord>()]);
        assert_eq!(body.geometry, device_handle(20));
        assert_eq!(body.bounds, [1.0, 2.0, 3.0, 4.0]);
        assert_eq!(body.root_page, page.raw());
        assert_eq!(body.material_range.count, 1);
        let material_elements = read_device_buffer(
            &harness.device,
            harness.gpu_data.prototype_materials.buffer(),
            (u64::from(body.material_range.first) + 1) * size_of::<GpuHandle>() as u64,
        );
        let element: &GpuHandle = bytemuck::from_bytes(
            &material_elements[body.material_range.first as usize * size_of::<GpuHandle>()..]
                [..size_of::<GpuHandle>()],
        );
        assert_eq!(*element, material.raw());

        let world_desc = harness
            .uploader
            .world_descriptors(&harness.device, WORLD)
            .expect("world tables");
        let bytes = read_device_buffer(
            &harness.device,
            world_desc.instances.buffer,
            world_desc.instances.range,
        );
        let slot = slot_bytes(
            &bytes,
            world_desc.instances.slot_stride,
            instance.raw().index,
        );
        let body: &GpuSceneInstanceGpuRecord =
            bytemuck::from_bytes(&slot[16..16 + size_of::<GpuSceneInstanceGpuRecord>()]);
        assert_eq!(body.prototype, prototype.raw());
        assert_eq!(body.transform_kind, GPU_SCENE_TRANSFORM_DYNAMIC);
        assert_eq!(body.transform[..16], current.to_cols_array());
        assert_eq!(body.transform[16..], previous.to_cols_array());
        assert_eq!(body.material_overrides.count, 1);

        let bytes = read_device_buffer(
            &harness.device,
            world_desc.lights.buffer,
            world_desc.lights.range,
        );
        let slot = slot_bytes(&bytes, world_desc.lights.slot_stride, 0);
        let body: &GpuSceneLightGpuRecord =
            bytemuck::from_bytes(&slot[16..16 + size_of::<GpuSceneLightGpuRecord>()]);
        assert_eq!(body.light, light_data);
        assert_eq!(body.source_revision, 21);

        harness
            .gpu_scene
            .apply_world_delta(WORLD, GpuSceneWorldDelta::RemoveInstance(instance))
            .expect("remove");
        let stats = harness.record_and_run(0);
        assert_eq!(stats.records, 1, "only the tombstone re-uploads");
        let bytes = read_device_buffer(
            &harness.device,
            world_desc.instances.buffer,
            world_desc.instances.range,
        );
        let slot = slot_bytes(
            &bytes,
            world_desc.instances.slot_stride,
            instance.raw().index,
        );
        let header: &GpuTableSlotHeader = bytemuck::from_bytes(&slot[..16]);
        assert_eq!(header.occupied, 0, "tombstoned slot reads unoccupied");

        harness.finish();
        assert_eq!(validation_issue_count(), before);
    }

    #[test]
    fn storage_growth_preserves_existing_slots() {
        let Some(device) = device_or_skip() else {
            return;
        };
        let before = validation_issue_count();
        {
            let mut gpu_data = GlobalGpuData::new(&device).expect("GlobalGpuData");
            let mut storage: GpuSceneTableStorage<SceneInstanceTable> =
                GpuSceneTableStorage::new(&device, 176, 1).expect("storage");
            let capacity = storage.storage.capacity();
            gpu_data.begin_frame(0).expect("begin");

            storage.ensure_slots(1).expect("slot 0");
            let mut graph = RenderGraph::new();
            let marker = [0xA5_u8; 176];
            storage
                .stage_slot(&mut gpu_data.uploads, 0, 0, 1, 1, &marker)
                .expect("stage slot 0")
                .enqueue(&mut graph, &device, "seed slot 0");
            run_graph(&device, &mut graph);

            let grown_slots = capacity + 8;
            storage.ensure_slots(grown_slots).expect("grow slots");
            let mut graph = RenderGraph::new();
            let growth = storage.prepare_growth(&device).expect("prepare growth");
            let growth = growth.expect("capacity exceeded requires growth");
            growth.enqueue(&mut graph, &device, "grow");
            let last = u32::try_from(grown_slots - 1).expect("slot index");
            storage
                .stage_slot(&mut gpu_data.uploads, 0, last, 2, 1, &[0x5A_u8; 176])
                .expect("stage last")
                .enqueue(&mut graph, &device, "seed last");
            run_graph(&device, &mut graph);

            let stride = storage.slot_stride();
            let bytes = read_device_buffer(&device, storage.storage.buffer(), grown_slots * stride);
            let first = slot_bytes(&bytes, stride, 0);
            assert!(
                first[16..16 + 176].iter().all(|byte| *byte == 0xA5),
                "growth preserved the seeded slot"
            );
            let tail = slot_bytes(&bytes, stride, last);
            assert!(tail[16..16 + 176].iter().all(|byte| *byte == 0x5A));
            device.wait_idle().expect("idle");
        }
        drop(device);
        // Recreate a device to flush validation counters deterministically is unnecessary;
        // the counter is process-wide.
        assert_eq!(validation_issue_count(), before);
    }

    #[test]
    fn pending_queue_drains_records_arenas_and_tombstones() {
        let Some(device) = device_or_skip() else {
            return;
        };
        let before = validation_issue_count();
        {
            let mut gpu_data = GlobalGpuData::new(&device).expect("GlobalGpuData");
            let mut pending = GpuScenePendingUploads::default();
            gpu_data.begin_frame(0).expect("begin");

            let vertex_count = 3_u32;
            let vertex_bytes = vertex_count * size_of::<saffron_geometry::Vertex>() as u32;
            let (vertex_range, _) = gpu_data.vertices.allocate(vertex_bytes, 16).expect("verts");
            let geometry = gpu_data
                .geometries
                .insert(crate::GpuGeometryRecord {
                    vertices: vertex_range,
                    indices: GpuArenaRange::default(),
                    clusters: GpuArenaRange::default(),
                    parts: GpuArenaRange::default(),
                    voxels: GpuArenaRange::default(),
                    submeshes: GpuArenaRange::default(),
                    flags: 0,
                    vertex_stride: size_of::<saffron_geometry::Vertex>() as u32,
                    index_stride: 4,
                    reserved: 0,
                })
                .expect("geometry");
            let vertices: Arc<[saffron_geometry::Vertex]> = std::iter::repeat_n(
                saffron_geometry::Vertex {
                    position: Vec3::new(7.0, 8.0, 9.0),
                    ..Default::default()
                },
                vertex_count as usize,
            )
            .collect();
            pending.stage_record(GlobalGpuTableKind::Geometry, geometry);
            pending.upload_arena(GpuArenaUploadRequest::Vertices {
                range: vertex_range,
                data: Arc::clone(&vertices),
            });

            let mut graph = RenderGraph::new();
            record_pending_global_uploads(&mut pending, &device, &mut graph, &mut gpu_data, 0)
                .expect("drain");
            assert!(
                pending.is_empty(),
                "the drain consumes every queued request"
            );
            run_graph(&device, &mut graph);

            let desc = gpu_data.geometries.descriptor(&device);
            let bytes = read_device_buffer(&device, desc.buffer, desc.range);
            let slot = slot_bytes(&bytes, desc.slot_stride, geometry.index);
            let header: &GpuTableSlotHeader = bytemuck::from_bytes(&slot[..16]);
            assert_eq!(header.occupied, 1);
            let body: &crate::GpuGeometryRecord =
                bytemuck::from_bytes(&slot[16..16 + size_of::<crate::GpuGeometryRecord>()]);
            assert_eq!(body.vertices, vertex_range);

            let arena_bytes = read_device_buffer(
                &device,
                gpu_data.vertices.buffer(),
                u64::from(vertex_range.first) + u64::from(vertex_bytes),
            );
            let uploaded = &arena_bytes[vertex_range.first as usize..];
            assert_eq!(
                uploaded,
                bytemuck::cast_slice::<saffron_geometry::Vertex, u8>(&vertices),
                "vertex bytes reach their arena range exactly"
            );

            pending.retire_record(GlobalGpuTableKind::Geometry, geometry);
            let mut graph = RenderGraph::new();
            record_pending_global_uploads(&mut pending, &device, &mut graph, &mut gpu_data, 0)
                .expect("drain retire");
            run_graph(&device, &mut graph);
            let bytes = read_device_buffer(&device, desc.buffer, desc.range);
            let slot = slot_bytes(&bytes, desc.slot_stride, geometry.index);
            let header: &GpuTableSlotHeader = bytemuck::from_bytes(&slot[..16]);
            assert_eq!(header.occupied, 0, "the retirement tombstones the slot");

            device.wait_idle().expect("idle");
        }
        drop(device);
        assert_eq!(validation_issue_count(), before);
    }

    #[test]
    fn shader_resolves_the_scene_chain_through_buffer_addresses() {
        use crate::compute_dispatch::{ComputeBuffer, run_compute};
        let Some(device) = device_or_skip() else {
            return;
        };
        let device = std::sync::Arc::new(device);
        let before = validation_issue_count();
        {
            let mut gpu_data = GlobalGpuData::new(&device).expect("GlobalGpuData");
            let mut uploader = GpuSceneUploader::new(&device).expect("uploader");
            let mut gpu_scene =
                PersistentGpuScene::new(GpuSceneUploadLimits::default()).expect("scene");
            gpu_scene.create_world(WORLD).expect("world");

            let material = match gpu_scene
                .apply_shared_delta(GpuSceneSharedDelta::CreateMaterial(
                    GpuSceneMaterialRecord {
                        table: device_handle(7),
                        source_revision: 0x1_0000_002B,
                    },
                ))
                .expect("material")
            {
                GpuSceneSharedDeltaResult::MaterialCreated(handle) => handle,
                other => panic!("unexpected {other:?}"),
            };
            let page = match gpu_scene
                .apply_shared_delta(GpuSceneSharedDelta::CreatePage(GpuScenePageRecord {
                    table: device_handle(9),
                    parent: None,
                    source_generation: 3,
                    flags: crate::GPU_PAGE_FLAG_GUARANTEED_ROOT,
                }))
                .expect("page")
            {
                GpuSceneSharedDeltaResult::PageCreated(handle) => handle,
                other => panic!("unexpected {other:?}"),
            };
            let prototype = match gpu_scene
                .apply_shared_delta(GpuSceneSharedDelta::CreatePrototype(
                    crate::GpuScenePrototypeRecord {
                        geometry: device_handle(20),
                        materials: std::sync::Arc::from([material]),
                        deformation: None,
                        sdfs: Vec::new().into(),
                        root_page: page,
                        bounds: [1.0, 2.0, 3.0, 4.5],
                        source_generation: 5,
                        flags: 0,
                        mechanics: [0; 4],
                    },
                ))
                .expect("prototype")
            {
                GpuSceneSharedDeltaResult::PrototypeCreated(handle) => handle,
                other => panic!("unexpected {other:?}"),
            };
            let current = Mat4::from_translation(Vec3::new(1.5, 2.5, 3.5));
            let previous = Mat4::from_translation(Vec3::new(4.0, 5.0, 6.0));
            let transform = GpuSceneDynamicTransform::new(current, previous).expect("transform");
            let instance = match gpu_scene
                .apply_world_delta(
                    WORLD,
                    GpuSceneWorldDelta::CreateInstance(crate::GpuSceneInstanceRecord {
                        prototype,
                        transform: GpuSceneTransform::Dynamic(transform),
                        material_overrides: std::sync::Arc::from([GpuSceneMaterialOverride {
                            slot: 0,
                            material,
                        }]),
                        deformation: None,
                        source_generation: 6,
                        flags: 0,
                        combination: 0,
                        vegetation: None,
                    }),
                )
                .expect("instance")
            {
                GpuSceneWorldDeltaResult::InstanceCreated(handle) => handle,
                other => panic!("unexpected {other:?}"),
            };
            gpu_scene
                .apply_world_delta(
                    WORLD,
                    GpuSceneWorldDelta::CreateLight(GpuSceneLightRecord {
                        light: GpuLight {
                            position_range: Vec4::new(1.0, 2.0, 3.0, 4.0),
                            color_intensity: Vec4::new(0.5, 0.25, 0.125, 8.5),
                            direction_type: Vec4::ZERO,
                            spot_cos: Vec4::new(0.0, 0.0, 1.0, 0.0),
                        },
                        source_revision: 21,
                    }),
                )
                .expect("light");

            gpu_data.begin_frame(0).expect("gpu data begin");
            uploader.begin_frame(0).expect("uploader begin");
            gpu_scene.begin_frame(0).expect("scene begin");
            let mut graph = RenderGraph::new();
            uploader
                .record_frame(&device, &mut graph, &mut gpu_data, &mut gpu_scene, 0)
                .expect("record");
            run_graph(&device, &mut graph);

            let block =
                uploader.build_address_block(&device, &gpu_data, WORLD, 0, (0, 0), 0, 0, 0, 0);
            assert!(block.instances != 0 && block.scene_prototypes != 0);
            let outputs = run_compute(
                std::sync::Arc::clone(&device),
                "gpu_scene_test",
                vec![
                    ComputeBuffer::zeroed(24 * size_of::<u32>()),
                    ComputeBuffer::from_bytes(bytemuck::bytes_of(&block).to_vec()),
                ],
                [1, 1, 1],
            )
            .expect("dispatch");
            let words: Vec<u32> = outputs[0]
                .chunks_exact(4)
                .map(|chunk| u32::from_le_bytes(chunk.try_into().unwrap()))
                .collect();

            assert_eq!(
                words[0],
                instance.raw().generation,
                "instance header generation"
            );
            assert_eq!(words[1], 1, "instance slot occupied");
            assert_eq!(words[2], prototype.raw().index);
            assert_eq!(words[3], prototype.raw().generation);
            assert_eq!(words[4], GPU_SCENE_TRANSFORM_DYNAMIC);
            let cols = current.to_cols_array();
            assert_eq!(words[5], cols[0].to_bits());
            assert_eq!(words[6], cols[1].to_bits());
            assert_eq!(words[7], cols[2].to_bits());
            assert_eq!(words[8], previous.to_cols_array()[15].to_bits());
            assert_eq!(words[9], 1, "one override element");
            assert_eq!(words[10], 20, "prototype geometry index");
            assert_eq!(words[11], 1, "prototype geometry generation");
            assert_eq!(words[12], 4.5_f32.to_bits(), "prototype bounds radius");
            assert_eq!(words[13], 1, "one prototype material element");
            assert_eq!(words[14], material.raw().index);
            assert_eq!(words[15], material.raw().generation);
            assert_eq!(words[16], 7, "material target index");
            assert_eq!(words[17], 1, "material target generation");
            assert_eq!(words[18], 0x2B, "material revision low word");
            assert_eq!(words[19], 0x1, "material revision high word");
            assert_eq!(words[20], 0, "override slot");
            assert_eq!(words[21], material.raw().index, "override material");
            assert_eq!(words[22], 8.5_f32.to_bits(), "light intensity");
            assert_eq!(words[23], 21, "light revision low word");

            device.wait_idle().expect("idle");
            drop(gpu_scene);
            drop(uploader);
            drop(gpu_data);
        }
        drop(device);
        assert_eq!(validation_issue_count(), before);
    }

    #[test]
    fn page_payload_bytes_reach_the_page_arena_byte_exact() {
        let Some(device) = device_or_skip() else {
            return;
        };
        let before = validation_issue_count();
        {
            let mut gpu_data = GlobalGpuData::new(&device).expect("GlobalGpuData");
            let mut pending = GpuScenePendingUploads::default();
            let payload: Vec<u8> = (0..192_u32).flat_map(|word| word.to_le_bytes()).collect();
            let (range, _) = gpu_data
                .pages
                .allocate(payload.len() as u32, 16)
                .expect("page range");
            pending.upload_arena(GpuArenaUploadRequest::PageBytes {
                range,
                data: payload.clone(),
            });
            let mut graph = RenderGraph::new();
            record_pending_global_uploads(&mut pending, &device, &mut graph, &mut gpu_data, 0)
                .expect("drain");
            run_graph(&device, &mut graph);

            let offset = gpu_data.pages.byte_offset(range).expect("offset");
            let bytes = read_device_buffer(
                &device,
                gpu_data.pages.buffer(),
                offset + payload.len() as u64,
            );
            assert_eq!(
                &bytes[offset as usize..],
                payload.as_slice(),
                "page payload bytes reach their arena range exactly"
            );
            device.wait_idle().expect("idle");
        }
        drop(device);
        assert_eq!(validation_issue_count(), before);
    }

    #[test]
    fn resident_table_strides_lock_the_slang_pointer_constants() {
        let Some(device) = device_or_skip() else {
            return;
        };
        let gpu_data = GlobalGpuData::new(&device).expect("GlobalGpuData");
        assert_eq!(
            gpu_data.geometries.slot_stride(),
            80,
            "GPU_GEOMETRY_TABLE_STRIDE"
        );
        assert_eq!(
            gpu_data.materials.slot_stride(),
            64,
            "GPU_MATERIAL_TABLE_STRIDE"
        );
        assert_eq!(
            gpu_data.textures.slot_stride(),
            48,
            "GPU_TEXTURE_TABLE_STRIDE"
        );
        assert_eq!(
            gpu_data.coverage.slot_stride(),
            64,
            "GPU_COVERAGE_TABLE_STRIDE"
        );
        assert_eq!(
            gpu_data.page_table.slot_stride(),
            64,
            "GPU_PAGE_TABLE_STRIDE"
        );
        assert_eq!(gpu_data.sdfs.slot_stride(), 96, "GPU_SDF_TABLE_STRIDE");
        drop(gpu_data);
        drop(device);
    }

    #[test]
    fn ray_candidate_classification_matches_the_cpu_classifier() {
        use crate::compute_dispatch::{ComputeBuffer, run_compute};
        use crate::{
            CoverageSourceKind, GpuCoverageRecord, GpuMaterialClass, GpuMaterialTableRecord,
            GpuSidedness, GpuSubmeshRecord, GpuTransparency, classify_canonical_coverage,
        };
        use saffron_geometry::glam::Vec2;
        use saffron_material::{AlphaClassification, SurfaceModel};

        let Some(device) = device_or_skip() else {
            return;
        };
        let device = std::sync::Arc::new(device);
        let before = validation_issue_count();
        {
            let mut gpu_data = GlobalGpuData::new(&device).expect("GlobalGpuData");
            let mut uploader = GpuSceneUploader::new(&device).expect("uploader");
            let mut pending = GpuScenePendingUploads::default();
            let mut gpu_scene =
                PersistentGpuScene::new(GpuSceneUploadLimits::default()).expect("scene");
            gpu_scene.create_world(WORLD).expect("world");

            // Resident geometry: a unit quad (two triangles, one submesh each) with
            // binary-exact positions/UVs so GPU interpolation matches CPU f32 arithmetic
            // bit-for-bit.
            let vert = |x: f32, y: f32| saffron_geometry::Vertex {
                position: Vec3::new(x, y, 0.0),
                uv0: Vec2::new(x, y),
                ..Default::default()
            };
            let vertices: Arc<[saffron_geometry::Vertex]> = Arc::from([
                vert(0.0, 0.0),
                vert(1.0, 0.0),
                vert(0.0, 1.0),
                vert(1.0, 1.0),
            ]);
            let indices: Arc<[u32]> = Arc::from([0_u32, 1, 2, 1, 3, 2]);
            let vertex_bytes = (vertices.len() * size_of::<saffron_geometry::Vertex>()) as u32;
            let (vertex_range, _) = gpu_data.vertices.allocate(vertex_bytes, 16).expect("verts");
            let (index_range, _) = gpu_data
                .indices
                .allocate((indices.len() * 4) as u32, 4)
                .expect("indices");
            let (submesh_range, _) = gpu_data.submesh_table.allocate(2, 1).expect("submeshes");
            let geometry = gpu_data
                .geometries
                .insert(crate::GpuGeometryRecord {
                    vertices: vertex_range,
                    indices: index_range,
                    clusters: GpuArenaRange::default(),
                    parts: GpuArenaRange::default(),
                    voxels: GpuArenaRange::default(),
                    submeshes: submesh_range,
                    flags: 0,
                    vertex_stride: size_of::<saffron_geometry::Vertex>() as u32,
                    index_stride: 4,
                    reserved: 0,
                })
                .expect("geometry");
            pending.stage_record(GlobalGpuTableKind::Geometry, geometry);
            pending.upload_arena(GpuArenaUploadRequest::Vertices {
                range: vertex_range,
                data: Arc::clone(&vertices),
            });
            pending.upload_arena(GpuArenaUploadRequest::Indices {
                range: index_range,
                data: Arc::clone(&indices),
            });
            pending.upload_arena(GpuArenaUploadRequest::Submeshes {
                range: submesh_range,
                data: vec![
                    GpuSubmeshRecord {
                        first_index: 0,
                        index_count: 3,
                        material_slot: 0,
                        reserved: 0,
                    },
                    GpuSubmeshRecord {
                        first_index: 3,
                        index_count: 3,
                        material_slot: 1,
                        reserved: 0,
                    },
                ],
            });

            // Two resident materials: a masked albedo-alpha default (slot 0) and a
            // thin-sheet canonical-probability override target (for slot 1).
            const MASKED_SALT: [u32; 2] = [0x1234_5678, 0x9abc_def0];
            const CANONICAL_SALT: [u32; 2] = [7, 9];
            let cov_masked = gpu_data
                .coverage
                .insert(GpuCoverageRecord {
                    texture: GpuHandle::INVALID,
                    cutoff: 0.5,
                    classification: AlphaClassification::Masked as u32,
                    source_kind: CoverageSourceKind::AlbedoAlpha as u32,
                    omm_policy: 0,
                    hash_salt: MASKED_SALT,
                    source_extent: [8, 8],
                    omm_thresholds: 0,
                    reserved: 0,
                })
                .expect("masked coverage");
            pending.stage_record(GlobalGpuTableKind::Coverage, cov_masked);
            let cov_canonical = gpu_data
                .coverage
                .insert(GpuCoverageRecord {
                    texture: GpuHandle::INVALID,
                    cutoff: 0.25,
                    classification: AlphaClassification::Masked as u32,
                    source_kind: CoverageSourceKind::Texture as u32,
                    omm_policy: 0,
                    hash_salt: CANONICAL_SALT,
                    source_extent: [16, 16],
                    omm_thresholds: 0,
                    reserved: 0,
                })
                .expect("canonical coverage");
            pending.stage_record(GlobalGpuTableKind::Coverage, cov_canonical);

            let (params_masked, _) = gpu_data
                .material_parameters
                .allocate(1, 1)
                .expect("masked params");
            let mut masked_block = MaterialParamsData::zeroed();
            masked_block.base_color = Vec4::new(1.0, 1.0, 1.0, 0.75);
            pending.upload_arena(GpuArenaUploadRequest::MaterialParams {
                range: params_masked,
                data: Box::new(masked_block),
            });
            let (params_canonical, _) = gpu_data
                .material_parameters
                .allocate(1, 1)
                .expect("canonical params");
            let mut canonical_block = MaterialParamsData::zeroed();
            canonical_block.base_color = Vec4::new(1.0, 1.0, 1.0, 0.5);
            pending.upload_arena(GpuArenaUploadRequest::MaterialParams {
                range: params_canonical,
                data: Box::new(canonical_block),
            });

            let mat_masked = gpu_data
                .materials
                .insert(GpuMaterialTableRecord {
                    base_color_texture: GpuHandle::INVALID,
                    normal_texture: GpuHandle::INVALID,
                    coverage: cov_masked,
                    parameter_index: params_masked.first,
                    material_class: GpuMaterialClass::new(
                        AlphaClassification::Masked,
                        GpuSidedness::Single,
                        SurfaceModel::Standard,
                        GpuTransparency::Opaque,
                        false,
                    ),
                    shader_index: 0,
                    flags: 0,
                    proxy_albedo: 0,
                    occupancy: 1.0,
                })
                .expect("masked material");
            pending.stage_record(GlobalGpuTableKind::Material, mat_masked);
            let mat_canonical = gpu_data
                .materials
                .insert(GpuMaterialTableRecord {
                    base_color_texture: GpuHandle::INVALID,
                    normal_texture: GpuHandle::INVALID,
                    coverage: cov_canonical,
                    parameter_index: params_canonical.first,
                    material_class: GpuMaterialClass::new(
                        AlphaClassification::Masked,
                        GpuSidedness::Double,
                        SurfaceModel::ThinSheetFoliage,
                        GpuTransparency::Opaque,
                        false,
                    ),
                    shader_index: 0,
                    flags: 0,
                    proxy_albedo: 0,
                    occupancy: 1.0,
                })
                .expect("canonical material");
            pending.stage_record(GlobalGpuTableKind::Material, mat_canonical);

            // Scene chain: two prototype default material references plus a per-instance
            // override on slot 1, so the candidate resolve exercises both lookups.
            let scene_material = |scene: &mut PersistentGpuScene, table, revision| match scene
                .apply_shared_delta(GpuSceneSharedDelta::CreateMaterial(
                    GpuSceneMaterialRecord {
                        table,
                        source_revision: revision,
                    },
                ))
                .expect("scene material")
            {
                GpuSceneSharedDeltaResult::MaterialCreated(handle) => handle,
                other => panic!("unexpected {other:?}"),
            };
            let scene_mat_default = scene_material(&mut gpu_scene, mat_masked, 1);
            let scene_mat_shadowed = scene_material(&mut gpu_scene, mat_masked, 2);
            let scene_mat_override = scene_material(&mut gpu_scene, mat_canonical, 3);
            let page = match gpu_scene
                .apply_shared_delta(GpuSceneSharedDelta::CreatePage(GpuScenePageRecord {
                    table: device_handle(9),
                    parent: None,
                    source_generation: 1,
                    flags: crate::GPU_PAGE_FLAG_GUARANTEED_ROOT,
                }))
                .expect("page")
            {
                GpuSceneSharedDeltaResult::PageCreated(handle) => handle,
                other => panic!("unexpected {other:?}"),
            };
            let prototype = match gpu_scene
                .apply_shared_delta(GpuSceneSharedDelta::CreatePrototype(
                    crate::GpuScenePrototypeRecord {
                        geometry,
                        materials: std::sync::Arc::from([scene_mat_default, scene_mat_shadowed]),
                        deformation: None,
                        sdfs: Vec::new().into(),
                        root_page: page,
                        bounds: [0.5, 0.5, 0.0, 1.0],
                        source_generation: 1,
                        flags: 0,
                        mechanics: [0; 4],
                    },
                ))
                .expect("prototype")
            {
                GpuSceneSharedDeltaResult::PrototypeCreated(handle) => handle,
                other => panic!("unexpected {other:?}"),
            };
            let transform =
                GpuSceneDynamicTransform::new(Mat4::IDENTITY, Mat4::IDENTITY).expect("transform");
            let instance = match gpu_scene
                .apply_world_delta(
                    WORLD,
                    GpuSceneWorldDelta::CreateInstance(crate::GpuSceneInstanceRecord {
                        prototype,
                        transform: GpuSceneTransform::Dynamic(transform),
                        material_overrides: std::sync::Arc::from([GpuSceneMaterialOverride {
                            slot: 1,
                            material: scene_mat_override,
                        }]),
                        deformation: None,
                        source_generation: 1,
                        flags: 0,
                        combination: 0,
                        vegetation: None,
                    }),
                )
                .expect("instance")
            {
                GpuSceneWorldDeltaResult::InstanceCreated(handle) => handle,
                other => panic!("unexpected {other:?}"),
            };

            gpu_data.begin_frame(0).expect("gpu data begin");
            uploader.begin_frame(0).expect("uploader begin");
            gpu_scene.begin_frame(0).expect("scene begin");
            let mut graph = RenderGraph::new();
            record_pending_global_uploads(&mut pending, &device, &mut graph, &mut gpu_data, 0)
                .expect("drain pending");
            uploader
                .record_frame(&device, &mut graph, &mut gpu_data, &mut gpu_scene, 0)
                .expect("record");
            run_graph(&device, &mut graph);

            const PHASE: u32 = 5;
            let block =
                uploader.build_address_block(&device, &gpu_data, WORLD, 0, (0, 0), 0, 0, 0, PHASE);

            // (instance slot, primitive, sampled alpha, barycentrics). Cases: the slot-0
            // default material, the slot-1 override, an out-of-capacity instance, and an
            // out-of-range primitive.
            let cases: [(u32, u32, f32, [f32; 2]); 4] = [
                (instance.raw().index, 0, 0.625, [0.25, 0.5]),
                (instance.raw().index, 1, 0.375, [0.5, 0.25]),
                (u32::MAX, 0, 0.5, [0.25, 0.25]),
                (instance.raw().index, 99, 0.5, [0.25, 0.25]),
            ];
            let mut case_bytes = Vec::with_capacity(cases.len() * 32);
            for (slot, primitive, sampled, barycentrics) in cases {
                for word in [
                    slot,
                    primitive,
                    sampled.to_bits(),
                    0,
                    barycentrics[0].to_bits(),
                    barycentrics[1].to_bits(),
                    0,
                    0,
                ] {
                    case_bytes.extend_from_slice(&word.to_le_bytes());
                }
            }
            let outputs = run_compute(
                std::sync::Arc::clone(&device),
                "gpu_scene_candidate_test",
                vec![
                    ComputeBuffer::zeroed(cases.len() * 16 * size_of::<u32>()),
                    ComputeBuffer::from_bytes(bytemuck::bytes_of(&block).to_vec()),
                    ComputeBuffer::from_bytes(case_bytes),
                ],
                [cases.len() as u32, 1, 1],
            )
            .expect("dispatch");
            let words: Vec<u32> = outputs[0]
                .chunks_exact(4)
                .map(|chunk| u32::from_le_bytes(chunk.try_into().unwrap()))
                .collect();

            // Case 0: primitive 0 (v0/v1/v2), weights (0.25, 0.25, 0.5) → uv/anchor
            // (0.25, 0.5); the slot-0 default masked albedo-alpha material.
            let masked_salt = u64::from(MASKED_SALT[0]) | (u64::from(MASKED_SALT[1]) << 32);
            let expected = classify_canonical_coverage(
                0.625,
                [0.25, 0.5],
                [0.25, 0.5, 0.0],
                CoverageSourceKind::AlbedoAlpha,
                AlphaClassification::Masked,
                0.75,
                [8, 8],
                masked_salt,
                PHASE,
                0.5,
                0.0,
                false,
                false,
            );
            let case0 = &words[0..16];
            assert_eq!(case0[0], 1, "case 0 resolves");
            assert_eq!(case0[1], 0.25_f32.to_bits(), "case 0 uv.x");
            assert_eq!(case0[2], 0.5_f32.to_bits(), "case 0 uv.y");
            assert_eq!(case0[3], 0.25_f32.to_bits(), "case 0 anchor.x");
            assert_eq!(case0[4], 0.5_f32.to_bits(), "case 0 anchor.y");
            assert_eq!(case0[5], 0.0_f32.to_bits(), "case 0 anchor.z");
            assert_eq!(case0[6], CoverageSourceKind::AlbedoAlpha as u32);
            assert_eq!(case0[7], AlphaClassification::Masked as u32);
            assert_eq!(case0[8], 8, "case 0 extent.x");
            assert_eq!(case0[9], 8, "case 0 extent.y");
            assert_eq!(case0[10], 0, "case 0 standard surface model");
            assert_eq!(case0[11], 0.75_f32.to_bits(), "case 0 baseColorAlpha");
            assert_eq!(case0[12], expected.alpha.to_bits(), "case 0 alpha");
            assert_eq!(case0[13], u32::from(expected.covered), "case 0 covered");
            assert_eq!(case0[14], 0.5_f32.to_bits(), "case 0 cutoff");
            assert_eq!(case0[15], MASKED_SALT[0], "case 0 salt low word");

            // Case 1: primitive 1 (v1/v3/v2), weights (0.25, 0.5, 0.25) → uv/anchor
            // (0.75, 0.75); the slot-1 override thin-sheet canonical material.
            let canonical_salt =
                u64::from(CANONICAL_SALT[0]) | (u64::from(CANONICAL_SALT[1]) << 32);
            let expected = classify_canonical_coverage(
                0.375,
                [0.75, 0.75],
                [0.75, 0.75, 0.0],
                CoverageSourceKind::Texture,
                AlphaClassification::Masked,
                0.5,
                [16, 16],
                canonical_salt,
                PHASE,
                0.25,
                0.0,
                true,
                false,
            );
            let case1 = &words[16..32];
            assert_eq!(case1[0], 1, "case 1 resolves");
            assert_eq!(case1[1], 0.75_f32.to_bits(), "case 1 uv.x");
            assert_eq!(case1[2], 0.75_f32.to_bits(), "case 1 uv.y");
            assert_eq!(case1[3], 0.75_f32.to_bits(), "case 1 anchor.x");
            assert_eq!(case1[4], 0.75_f32.to_bits(), "case 1 anchor.y");
            assert_eq!(case1[6], CoverageSourceKind::Texture as u32);
            assert_eq!(case1[8], 16, "case 1 extent.x");
            assert_eq!(case1[10], 1, "case 1 canonical probability");
            assert_eq!(case1[11], 0.5_f32.to_bits(), "case 1 baseColorAlpha");
            assert_eq!(case1[12], expected.alpha.to_bits(), "case 1 alpha");
            assert_eq!(case1[13], u32::from(expected.covered), "case 1 covered");
            assert_eq!(case1[14], 0.25_f32.to_bits(), "case 1 cutoff");
            assert_eq!(case1[15], CANONICAL_SALT[0], "case 1 salt low word");

            // Cases 2 and 3 do not resolve; the ray verdict falls back to covered.
            for (index, label) in [
                (2, "out-of-capacity instance"),
                (3, "out-of-range primitive"),
            ] {
                let case = &words[index * 16..(index + 1) * 16];
                assert_eq!(case[0], 0, "{label} stays unresolved");
                assert_eq!(case[12], 1.0_f32.to_bits(), "{label} alpha");
                assert_eq!(case[13], 1, "{label} counts as covered");
            }

            device.wait_idle().expect("idle");
            drop(gpu_scene);
            drop(uploader);
            drop(gpu_data);
        }
        drop(device);
        assert_eq!(validation_issue_count(), before);
    }
}
