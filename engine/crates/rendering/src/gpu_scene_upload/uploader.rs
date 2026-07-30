use std::collections::{BTreeMap, HashMap};

use ash::vk;

use super::{
    ADDRESS_BLOCK_ALIGNMENT, GpuSceneAddressBlock, GpuSceneTableDescriptors, GpuSceneTableStorage,
    GpuSceneUploadRunStats, GpuSceneWorldDescriptors, GpuSceneWorldTables, INITIAL_INSTANCE_SLOTS,
    INITIAL_LIGHT_SLOTS, INITIAL_OVERRIDE_ELEMENTS, INITIAL_SHARED_SLOTS, PAGE_REQUEST_CAPACITY,
    PAGE_REQUEST_ENTRY_BYTES, PAGE_REQUEST_HEADER_BYTES, PAGE_REQUEST_SLOT_BYTES, PageRequestDrain,
};
use crate::frame::MAX_FRAMES_IN_FLIGHT;
use crate::global_gpu_data::{
    FrameUploadRing, GlobalGpuArena, GlobalGpuData, GpuArenaRange, GpuBufferUpload, GpuHandle,
    GpuSceneInstanceGpuRecord, GpuSceneLightGpuRecord, GpuSceneOverrideGpuRecord,
    GpuScenePageGpuRecord, GpuScenePrototypeGpuRecord, GpuSceneReferenceGpuRecord,
    GpuTableDescriptor, SceneDeformationTable, SceneMaterialTable, SceneOverrideArena,
    ScenePageTable, ScenePrototypeTable, SceneSdfTable,
};
use crate::persistent_gpu_scene::{
    GpuSceneInstanceRecord, GpuScenePrototypeRecord, GpuSceneTransform, GpuSceneUploadPayload,
    GpuSceneUploadTarget, GpuSceneWorldId, PersistentGpuScene,
};
use crate::render_graph::RenderGraph;
use crate::resources::Buffer;
use crate::{Device, Error, GPU_SCENE_TRANSFORM_DYNAMIC, GPU_SCENE_TRANSFORM_STATIC, Result};

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
    pub(super) page_requests: Buffer,
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
        displaced: crate::DisplacedFrameAddresses,
        ray_instances: (u64, u32),
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
            displaced_vertices: displaced.vertices,
            displaced_prev_vertices: displaced.prev_vertices,
            displaced_indices: displaced.indices,
            displaced_draws: displaced.draws,
            displaced_rows: displaced.rows,
            displaced_row_count: displaced.row_count,
            ray_instances: ray_instances.0,
            ray_instance_count: ray_instances.1,
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
            reserved: [0; 4],
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
