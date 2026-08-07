//! The mesh wrapper and the streams it owns: vertices, indices, clusters, blend-shape morph
//! targets, watertight conditioning buffers, and the bottom-level acceleration structure.

use super::*;

/// A device-local mesh: vertex + index (+ optional skin) buffers, the submesh
/// ranges, the local-space AABB, the CPU-side copies retained for triangle-precise
/// picking, and the optional ray-tracing BLAS.
///
/// The three VMA buffers are freed in [`Drop`]; the [`AccelerationStructure`] is an
/// `Arc` (shared, read-only after build) and drops itself.
pub struct GpuMesh {
    pub(super) resources: Arc<DeviceResources>,
    pub(super) vertex_buffer: vk::Buffer,
    pub(super) vertex_alloc: vk_mem::Allocation,
    pub(super) index_buffer: vk::Buffer,
    pub(super) index_alloc: vk_mem::Allocation,
    /// The skin stream buffer + allocation (`None` for unskinned meshes).
    pub(super) skin: Option<(vk::Buffer, vk_mem::Allocation)>,
    /// The morph (blend-shape) buffers (`None` for a mesh without morph targets).
    pub(super) morph: Option<MorphBuffers>,
    /// The watertight-conditioning buffers (`None` for an empty mesh).
    pub(super) conditioning: Option<ConditioningBuffers>,
    /// Number of indices across every submesh.
    pub index_count: u32,
    /// Number of vertices.
    pub vertex_count: u32,
    /// The draw ranges over the shared vertex/index buffers.
    pub submeshes: Vec<Submesh>,
    /// The opacity micromaps the BLAS geometries reference, retained for the mesh's lifetime.
    ///
    /// A built structure holds only device addresses into these, so dropping one while a BLAS
    /// still references it frees memory the traversal reads — a fault that surfaces far from its
    /// cause. They live exactly as long as the mesh whose geometry they refine.
    pub micromaps: Vec<Arc<Micromap>>,
    /// Whether each submesh's BLAS geometry was built `OPAQUE`, from its cooked material class,
    /// parallel to [`GpuMesh::submeshes`].
    ///
    /// Per submesh because that is the granularity a structure carries opacity at: every build
    /// over this mesh — the upload-time static one and each per-frame deforming refit — lays one
    /// geometry per submesh with its own class. An entity compares its resolved materials against
    /// [`GpuMesh::cooked_opaque`] to decide whether it must override the structure's opacity.
    pub submesh_opaque: Vec<bool>,
    /// Local-space AABB minimum (for ray picking).
    pub bounds_min: Vec3,
    /// Local-space AABB maximum (for ray picking).
    pub bounds_max: Vec3,
    /// CPU copy of the complete local/rest vertex stream for surface queries.
    pub cpu_vertices: Arc<[Vertex]>,
    /// CPU copy of the flat index buffer spanning every submesh.
    pub cpu_indices: Arc<[u32]>,
    /// CPU copy of the skin stream parallel to [`GpuMesh::cpu_vertices`] (empty
    /// when unskinned).
    pub cpu_skin: Vec<VertexSkin>,
    /// The ray-tracing BLAS (`None` when RT is unsupported or not yet built, and always
    /// `None` for an assembly — see [`GpuMesh::assembly_blas`]).
    pub blas: Option<Arc<AccelerationStructure>>,
    /// One bottom-level structure per assembly prototype, in prototype-id order; empty for an
    /// ordinary mesh. KHR acceleration structures have no notion of nested micro-instance
    /// parts inside one structure, so a family's ray representation is one structure per
    /// prototype plus one TLAS instance per placed use. On a device with cluster
    /// acceleration structures each entry is the cluster-composed build over the
    /// prototype's cooked clusters; the KHR triangle build everywhere else.
    pub assembly_blas: Vec<RtBlas>,
    /// The aggregate-representation structure: one family-space BLAS over the root cut's
    /// voxel-brick surfaces, with the largest root appearance-error total. TLAS packing
    /// projects that error exactly as the raster traversal does and swaps a distant
    /// instance to this single structure instead of expanding per use. `None` when the
    /// root cut is not fully voxel or RT is off.
    pub aggregate_blas: Option<(Arc<AccelerationStructure>, u32)>,
    /// The per-mesh signed distance fields — one tight field per primitive (and per spatial
    /// chunk of an oversized primitive), empty when the mesh baked none (a degenerate mesh, or
    /// a build without SDF support). Held here so the fields live exactly as long as the mesh
    /// that owns them; the lighting cone-trace indexes each by [`GpuSdf::bindless_index`].
    pub sdfs: Vec<Arc<GpuSdf>>,
    /// The cooked hierarchy page directory (dependencies, guaranteed roots, bounds,
    /// transition errors). The GPU-scene mirror builds the prototype's page graph from this;
    /// page payloads stream from the source artifact, never from mesh memory.
    pub hierarchy_pages: Vec<saffron_geometry::PortableHierarchyPage>,
    /// The assembly-part table for a multi-prototype geometry (a plant family): the
    /// per-prototype vertex bases + use spans and the per-use family-local transforms the
    /// mirror uploads into the parts arena. `None` for a plain single-prototype mesh.
    pub assembly: Option<MeshAssembly>,
}

/// One bottom-level structure a TLAS instance can reference by device address: the KHR
/// triangle build, or the cluster-composed build on a device with
/// `VK_NV_cluster_acceleration_structure`. The two are interchangeable at every consumer —
/// an instance carries only the address — so which one a mesh holds is a device capability,
/// never a content property.
#[derive(Clone)]
pub enum RtBlas {
    /// The `VK_KHR_acceleration_structure` triangle build.
    Khr(Arc<AccelerationStructure>),
    /// The cluster-composed build over the mesh's cooked triangle clusters.
    Cluster(Arc<crate::rt_cluster::ClusterBlas>),
}

impl RtBlas {
    /// The device address a TLAS instance references.
    #[must_use]
    pub fn address(&self) -> vk::DeviceAddress {
        match self {
            Self::Khr(blas) => blas.address,
            Self::Cluster(blas) => blas.address(),
        }
    }

    /// Bytes of bottom-level storage this structure occupies (for a cluster build, the
    /// bottom level plus its CLAS pool — both live for the structure's lifetime).
    #[must_use]
    pub fn size(&self) -> vk::DeviceSize {
        match self {
            Self::Khr(blas) => blas.size(),
            Self::Cluster(blas) => blas.size() + blas.clas_bytes(),
        }
    }

    /// Bytes the original build reserved ([`AccelerationStructure::built_size`]; a cluster
    /// build has no compaction copy, so it equals [`Self::size`]).
    #[must_use]
    pub fn built_size(&self) -> vk::DeviceSize {
        match self {
            Self::Khr(blas) => blas.built_size(),
            Self::Cluster(blas) => blas.size() + blas.clas_bytes(),
        }
    }

    /// `(resolution table, corner stream, cluster count)` for a cluster-composed structure.
    ///
    /// A KHR build reports `None`: its geometry index already names a submesh and its primitive
    /// index is a position in that submesh's slice of the shared stream. A cluster build reports
    /// neither — its primitive index is cluster-local over a cache-optimized permutation — so a
    /// candidate resolves through these two tables instead.
    #[must_use]
    pub fn cluster_resolution(&self) -> Option<(vk::DeviceAddress, vk::DeviceAddress, u32)> {
        match self {
            Self::Khr(_) => None,
            Self::Cluster(blas) => Some((
                blas.resolution_address(),
                blas.corners_address(),
                blas.cluster_count(),
            )),
        }
    }
}

/// The assembly-part table a multi-prototype [`GpuMesh`] carries: the records the mirror
/// packs into the geometry's parts-arena range (prototype records first, then use records).
#[derive(Clone, Debug, Default)]
pub struct MeshAssembly {
    /// Per-prototype `{first_use, use_count, vertex_base}` records, indexed by prototype id.
    pub prototypes: Vec<crate::GpuAssemblyPrototypeRecord>,
    /// Use records grouped by prototype in prototype-id order.
    pub uses: Vec<crate::GpuAssemblyUseRecord>,
    /// The `(variation, phenotype)` identity of each mask-table combination, in table
    /// order — the CPU adapter resolves an instance's combination index against this.
    pub combinations: Vec<(u32, u32)>,
    /// The packed active-use mask words, `mask_words` per combination.
    pub masks: Vec<u32>,
    /// Each prototype's slice of the family's flattened submesh + index streams, in prototype-id
    /// order. One BLAS is built per entry: KHR acceleration structures have no notion of nested
    /// micro-instance parts, so a family's ray representation is one structure per prototype plus
    /// one TLAS instance per placed use.
    pub prototype_slices: Vec<AssemblyPrototypeSlice>,
}

/// One assembly prototype's slice of the family's flattened geometry: the submesh run its
/// bottom-level structure builds one geometry from each, and the index range those submeshes span.
///
/// `first_submesh` is what rebases a traced candidate's geometry index onto the family's submesh
/// table — the structure holds one geometry per submesh of THIS prototype, so geometry 0 is the
/// span's first submesh rather than the family's.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AssemblyPrototypeSlice {
    /// First submesh of the prototype's run in the family's flattened table.
    pub first_submesh: u32,
    /// Submeshes in the run.
    pub submesh_count: u32,
    /// First index of the run within the family's flattened index stream.
    pub first_index: u32,
    /// Indices the run spans.
    pub index_count: u32,
    /// First vertex of the prototype's run within the family's flattened vertex stream. The
    /// index stream addresses family vertices absolutely, so anything that materializes one
    /// prototype's geometry alone rebases by this.
    pub first_vertex: u32,
    /// Vertices the run spans.
    pub vertex_count: u32,
}

impl MeshAssembly {
    /// Mask words per combination.
    #[must_use]
    pub fn mask_words(&self) -> usize {
        self.uses.len().div_ceil(32)
    }

    /// The packed parts-range bytes: the header, the prototype table, the use table,
    /// then the combination mask words.
    #[must_use]
    pub fn packed_bytes(&self) -> Vec<u8> {
        let header = crate::GpuAssemblyHeaderRecord {
            prototype_count: self.prototypes.len() as u32,
            use_count: self.uses.len() as u32,
            mask_words: self.mask_words() as u32,
            combination_count: self.combinations.len() as u32,
        };
        let mut bytes = Vec::with_capacity(self.byte_len());
        bytes.extend_from_slice(bytemuck::bytes_of(&header));
        bytes.extend_from_slice(bytemuck::cast_slice(&self.prototypes));
        bytes.extend_from_slice(bytemuck::cast_slice(&self.uses));
        bytes.extend_from_slice(bytemuck::cast_slice(&self.masks));
        bytes
    }

    /// Total packed byte length of the parts range.
    #[must_use]
    pub fn byte_len(&self) -> usize {
        size_of::<crate::GpuAssemblyHeaderRecord>()
            + self.prototypes.len() * size_of::<crate::GpuAssemblyPrototypeRecord>()
            + self.uses.len() * size_of::<crate::GpuAssemblyUseRecord>()
            + self.masks.len() * size_of::<u32>()
    }
}

impl GpuMesh {
    /// Returns the host bytes retained for exact mesh-surface and deformation queries.
    pub fn retained_query_cpu_bytes(&self) -> u64 {
        fn bytes_for<T>(len: usize) -> u64 {
            u64::try_from(len)
                .unwrap_or(u64::MAX)
                .saturating_mul(size_of::<T>() as u64)
        }

        bytes_for::<Vertex>(self.cpu_vertices.len())
            .saturating_add(bytes_for::<u32>(self.cpu_indices.len()))
            .saturating_add(bytes_for::<VertexSkin>(self.cpu_skin.len()))
            .saturating_add(bytes_for::<Submesh>(self.submeshes.len()))
    }
}

/// The device-local morph buffers a [`GpuMesh`] carries when it has blend shapes: the flat
/// `MorphDelta` array (28 B stride) and the per-target `{first_delta, delta_count}` ranges,
/// plus the counts the deform pass dispatches over.
pub struct MorphBuffers {
    /// The flat `MorphDelta` array buffer + allocation.
    pub deltas: (vk::Buffer, vk_mem::Allocation),
    /// The per-target range array buffer + allocation (`uint2` per target).
    pub ranges: (vk::Buffer, vk_mem::Allocation),
    /// CPU copy of the per-target `[first_delta, delta_count]` ranges, parallel to the GPU
    /// `ranges` buffer — the instancing pass reads these to compute each active target's
    /// scatter base + the total scatter dispatch size.
    pub cpu_ranges: Vec<[u32; 2]>,
    /// Number of morph targets.
    pub target_count: u32,
    /// Total `MorphDelta` records across all targets.
    pub delta_count: u32,
}

/// The device-local watertight-conditioning buffers a [`GpuMesh`] carries: the unique-edge list, the
/// per-triangle edge indices, the per-welded-vertex direction/tangent basis, and the base→welded map
/// ([`saffron_geometry::MeshConditioning`]). All four are plain `STORAGE_BUFFER`s the
/// tessellation factor pass and dicer read.
pub struct ConditioningBuffers {
    /// The `Edge` array buffer + allocation (16 B stride).
    pub edges: (vk::Buffer, vk_mem::Allocation),
    /// The `TriEdges` array buffer + allocation (16 B stride, one per triangle).
    pub tri_edges: (vk::Buffer, vk_mem::Allocation),
    /// The `WeldedVertex` array buffer + allocation (48 B stride).
    pub welded: (vk::Buffer, vk_mem::Allocation),
    /// The `weld_id` array buffer + allocation (`u32` per base vertex).
    pub weld_id: (vk::Buffer, vk_mem::Allocation),
    /// Number of unique edges.
    pub edge_count: u32,
    /// Number of welded vertices.
    pub welded_count: u32,
}

// SAFETY: the buffers/allocations carry no thread-affine state; the CPU-side
// vectors and `Arc<AccelerationStructure>` are `Send`. Meshes are shared as
// `Arc<GpuMesh>` and may be dropped from the worker thread.
unsafe impl Send for GpuMesh {}

// SAFETY: every field is shared read-only after construction (the raw buffers +
// `vk_mem::Allocation` carry no interior mutability and are mutated only through
// `&mut self`); the CPU vectors + `Arc<AccelerationStructure>` are `Sync`. The
// thumbnail worker hands an `Arc<GpuMesh>` back to the main thread through an
// `Arc<Mutex<_>>`, which needs `Sync`.
unsafe impl Sync for GpuMesh {}

/// The buffers and metadata a [`GpuMesh`] is assembled from (the upload path fills
/// this, then [`GpuMesh::from_parts`] takes ownership).
pub struct GpuMeshParts {
    /// The device-local vertex buffer + allocation.
    pub vertex: (vk::Buffer, vk_mem::Allocation),
    /// The device-local index buffer + allocation.
    pub index: (vk::Buffer, vk_mem::Allocation),
    /// The optional device-local skin stream buffer + allocation.
    pub skin: Option<(vk::Buffer, vk_mem::Allocation)>,
    /// The optional device-local morph buffers.
    pub morph: Option<MorphBuffers>,
    /// The optional device-local watertight-conditioning buffers.
    pub conditioning: Option<ConditioningBuffers>,
    /// Number of indices across every submesh.
    pub index_count: u32,
    /// Number of vertices.
    pub vertex_count: u32,
    /// The draw ranges.
    pub submeshes: Vec<Submesh>,
    /// Whether each submesh's geometry was built `OPAQUE` from its cooked material class.
    pub submesh_opaque: Vec<bool>,
    /// The opacity micromaps the BLAS geometries reference; retained for the mesh's lifetime.
    pub micromaps: Vec<Arc<Micromap>>,
    /// Local-space AABB minimum.
    pub bounds_min: Vec3,
    /// Local-space AABB maximum.
    pub bounds_max: Vec3,
    /// Complete CPU vertex stream for surface queries.
    pub cpu_vertices: Vec<Vertex>,
    /// CPU indices for picking.
    pub cpu_indices: Vec<u32>,
    /// CPU skin stream for picking (empty when unskinned).
    pub cpu_skin: Vec<VertexSkin>,
    /// The built ray-tracing BLAS (`None` when RT is unsupported).
    pub blas: Option<Arc<AccelerationStructure>>,
    /// One structure per assembly prototype; empty for a plain mesh.
    pub assembly_blas: Vec<RtBlas>,
    /// The aggregate-representation structure over the root cut's voxel-brick surfaces
    /// (family space), with the largest root appearance-error total the selection projects.
    pub aggregate_blas: Option<(Arc<AccelerationStructure>, u32)>,
    /// The uploaded per-mesh signed distance fields (one per primitive / chunk; empty when
    /// none was baked).
    pub sdfs: Vec<Arc<GpuSdf>>,
    /// The cooked hierarchy page directory retained on the mesh.
    pub hierarchy_pages: Vec<saffron_geometry::PortableHierarchyPage>,
    /// The assembly-part table for a multi-prototype geometry (`None` for a plain mesh).
    pub assembly: Option<MeshAssembly>,
}

impl GpuMesh {
    /// Takes ownership of the uploaded buffers + metadata.
    pub fn from_parts(resources: &Arc<DeviceResources>, parts: GpuMeshParts) -> Self {
        Self {
            resources: Arc::clone(resources),
            vertex_buffer: parts.vertex.0,
            vertex_alloc: parts.vertex.1,
            index_buffer: parts.index.0,
            index_alloc: parts.index.1,
            skin: parts.skin,
            morph: parts.morph,
            conditioning: parts.conditioning,
            index_count: parts.index_count,
            vertex_count: parts.vertex_count,
            submeshes: parts.submeshes,
            submesh_opaque: parts.submesh_opaque,
            micromaps: parts.micromaps,
            bounds_min: parts.bounds_min,
            bounds_max: parts.bounds_max,
            cpu_vertices: parts.cpu_vertices.into(),
            cpu_indices: parts.cpu_indices.into(),
            cpu_skin: parts.cpu_skin,
            blas: parts.blas,
            assembly_blas: parts.assembly_blas,
            aggregate_blas: parts.aggregate_blas,
            sdfs: parts.sdfs,
            hierarchy_pages: parts.hierarchy_pages,
            assembly: parts.assembly,
        }
    }

    /// Whether every submesh classifies opaque, so an instance binding the cooked materials
    /// needs no instance-level opacity override.
    #[must_use]
    pub fn cooked_opaque(&self) -> bool {
        self.submesh_opaque.iter().all(|&opaque| opaque)
    }

    /// The per-mesh signed distance fields — one tight field per primitive (and per spatial
    /// chunk of an oversized primitive); empty when the mesh baked none.
    pub fn sdfs(&self) -> &[Arc<GpuSdf>] {
        &self.sdfs
    }

    /// The vertex buffer handle.
    pub fn vertex_buffer(&self) -> vk::Buffer {
        self.vertex_buffer
    }

    /// The index buffer handle.
    pub fn index_buffer(&self) -> vk::Buffer {
        self.index_buffer
    }

    /// The skin stream buffer handle, or `None` for an unskinned mesh.
    pub fn skin_buffer(&self) -> Option<vk::Buffer> {
        self.skin.as_ref().map(|(buffer, _)| *buffer)
    }

    /// The morph buffers, or `None` for a mesh without blend shapes.
    pub fn morph(&self) -> Option<&MorphBuffers> {
        self.morph.as_ref()
    }

    /// The watertight-conditioning buffers, or `None` for an empty mesh.
    pub fn conditioning(&self) -> Option<&ConditioningBuffers> {
        self.conditioning.as_ref()
    }
}

impl Drop for GpuMesh {
    fn drop(&mut self) {
        // SAFETY: the VMA seam. The bundle keeps the allocator alive; each buffer is
        // destroyed exactly once. The `blas` Arc drops after this body.
        unsafe {
            let allocator = self.resources.allocator();
            allocator.destroy_buffer(self.vertex_buffer, &mut self.vertex_alloc);
            allocator.destroy_buffer(self.index_buffer, &mut self.index_alloc);
            if let Some((buffer, allocation)) = self.skin.as_mut() {
                allocator.destroy_buffer(*buffer, allocation);
            }
            if let Some(morph) = self.morph.as_mut() {
                allocator.destroy_buffer(morph.deltas.0, &mut morph.deltas.1);
                allocator.destroy_buffer(morph.ranges.0, &mut morph.ranges.1);
            }
            if let Some(c) = self.conditioning.as_mut() {
                allocator.destroy_buffer(c.edges.0, &mut c.edges.1);
                allocator.destroy_buffer(c.tri_edges.0, &mut c.tri_edges.1);
                allocator.destroy_buffer(c.welded.0, &mut c.welded.1);
                allocator.destroy_buffer(c.weld_id.0, &mut c.weld_id.1);
            }
        }
    }
}
