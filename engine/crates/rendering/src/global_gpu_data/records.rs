//! The `#[repr(C)]` GPU record structs the device tables hold. Each is byte-asserted against
//! the layout the shaders read, so a wrong stride is a failed assertion rather than corrupt reads.

use super::*;

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
    /// `1.0` when the interaction cascade covering this instance changed since the
    /// previous frame: the stored displacement jumped rather than moved, so the
    /// reactive-coverage pass marks the instance instead of letting TAA reproject it.
    pub interaction_reset: f32,
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
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
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
    /// Reserved for material flags.
    pub flags: u32,
    /// The resolved base color's rgb packed 8:8:8 unorm (low to high), for the GI
    /// occluder's proxy albedo — the lite albedo cache the DDGI trace reads wants a
    /// color, not a texture fetch. High byte reserved.
    pub proxy_albedo: u32,
    /// Aggregate occupancy the distance-field consumers march this matter with:
    /// `1.0` solid; below one, thin-sheet parity occupancy (porous matter never
    /// hardens the field, it extinguishes through it).
    pub occupancy: f32,
}

/// Device form of one baked signed distance field: everything a shader needs to sample
/// the field's brick atlas and to place its local grid in an instance's frame.
///
/// One mesh bakes a LIST of these — one tight field per primitive and per spatial chunk
/// of an oversized primitive — and the tightness is the point: small fields cull
/// independently where one enclosing field would keep the whole mesh resident in every
/// march. The occluder scatter reads this record to compose an [`crate::SdfInstance`]
/// per visible instance per field.
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
#[repr(C, align(16))]
pub struct GpuSdfTableRecord {
    /// The padded grid lower corner in local (rest) space; `w` is the `R16_SNORM`
    /// distance normalization clamp (a sampled `+1.0` denormalizes to this many local
    /// units).
    pub local_min: [f32; 4],
    /// The padded grid upper corner in local space; `w` reserved.
    pub local_max: [f32; 4],
    /// `xyz` the fine voxel count per axis; `w` the shared bindless SDF slot (atlas
    /// binding 1 + indirection binding 2 + coverage binding 3).
    pub voxel_dims: [u32; 4],
    /// `xyz` the brick indirection-volume dims (bricks per axis); `w` the prefiltered
    /// atlas mip count.
    pub indirection_dims: [u32; 4],
    /// `xyz` the atlas tiling (occupied bricks per axis); `w` reserved.
    pub atlas_bricks: [u32; 4],
}

/// The executor shader registry: gives [`GpuMaterialTableRecord::shader_index`] its
/// meaning. Index 0 is the engine übershader; nonzero indices are codegen material
/// shaders registered when the mirror interns them.
#[derive(Debug)]
pub struct ExecutorShaderRegistry {
    pub(super) shaders: Vec<String>,
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
/// the amplification arena.
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
    /// Range of packed scene-SDF handles in [`GlobalGpuData::prototype_sdfs`] — one
    /// per baked field of the prototype's mesh, empty when it baked none. A range
    /// rather than a handle because the fields are many and tight on purpose: each
    /// culls independently where one enclosing field would keep the whole mesh in
    /// every march.
    pub sdf_range: GpuArenaRange,
    /// Packed scene page handle of the guaranteed-resident root.
    pub root_page: GpuHandle,
    /// Source generation of the mirrored asset.
    pub source_generation: u32,
    /// Prototype flags.
    pub flags: u32,
    /// Conservative object-space bounding sphere (16-byte aligned for storage-pointer loads).
    pub bounds: [f32; 4],
    /// The prototype's authored mechanical response, in the authored integer forms so the
    /// GPU reads exactly what was cooked: `[0..3]` are Q15.16 stiffness, drag, and flutter;
    /// `[3]` packs the `[0, 1]` damping in the low half and bend limit in the high half.
    /// All zero when the prototype is not a plant family, which the wind prepass reads as
    /// "derive the response from the plant's height alone".
    pub mechanics: [u32; 4],
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
    /// Alignment padding keeping the transform payload 16-byte aligned. Occluders are
    /// a prototype property, so the instance record carries no SDF reference.
    pub reserved_pad: [u32; 2],
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
