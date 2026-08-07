//! The typed arena and table markers, their descriptors, and the [`GlobalGpuData`] aggregate
//! that owns one arena per marker.

use super::*;

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

/// Marker for per-prototype SDF-reference handles.
pub enum PrototypeSdfArena {}

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

/// Marker for the SDF table.
pub enum SdfTable {}

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
    /// [`GlobalGpuData::sdfs`].
    Sdf,
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
    /// Baked signed-distance-field records.
    pub sdfs: GpuTableDescriptor,
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
    /// Packed scene-SDF handles addressed by
    /// [`GpuScenePrototypeGpuRecord::sdf_range`] — one per baked field of the
    /// prototype's mesh, empty for a mesh that baked none.
    pub prototype_sdfs: GlobalGpuArena<PrototypeSdfArena>,
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
    /// Baked signed-distance-field records — the resident target the scene SDF
    /// references resolve to, and the occluder scatter's source of grid placement.
    pub sdfs: ResidentGpuTable<GpuSdfTableRecord, SdfTable>,
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
            prototype_sdfs: GlobalGpuArena::new(
                device,
                std::mem::size_of::<GpuHandle>() as u64,
                4_096,
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
            sdfs: ResidentGpuTable::new(device, 4_096)?,
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
        self.prototype_sdfs.begin_frame(completed_frame_slot)?;
        self.material_parameters.begin_frame(completed_frame_slot)?;
        self.submesh_table.begin_frame(completed_frame_slot)?;
        self.prototypes.begin_frame(completed_frame_slot)?;
        self.geometries.begin_frame(completed_frame_slot)?;
        self.materials.begin_frame(completed_frame_slot)?;
        self.textures.begin_frame(completed_frame_slot)?;
        self.coverage.begin_frame(completed_frame_slot)?;
        self.skeletons.begin_frame(completed_frame_slot)?;
        self.sdfs.begin_frame(completed_frame_slot)?;
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
            sdfs: self.sdfs.descriptor(device),
            pages: self.page_table.descriptor(device),
        }
    }
}
