use std::sync::Arc;

use ash::vk;
use saffron_geometry::glam::Vec3;
use saffron_geometry::{
    Mesh, MeshConditioning, MorphData, MorphDelta, PortableVirtualHierarchy, VertexSkin,
};

use super::hierarchy::{
    assembly_from_hierarchy, cooked_submesh_opacity, validate_upload_hierarchy,
};
use super::staging::{StagingBuffer, free_one, make_device_buffer};
use super::{SdfSource, Uploader};
use crate::descriptors::Descriptors;
use crate::resources::{ConditioningBuffers, GpuMesh, GpuMeshParts, MorphBuffers};
use crate::{Error, Result};

impl Uploader {
    /// Uploads a mesh's vertex + index streams (and the optional [`VertexSkin`]
    /// stream) into device-local buffers, returning a shared [`GpuMesh`].
    ///
    /// One staging buffer holds `[vertices | indices | skin]`; copies fan it out to
    /// the device-local buffers. The skin stream, when present, must parallel the
    /// vertices (one [`VertexSkin`] per vertex); it carries `STORAGE` usage too (the
    /// compute skinning prepass reads it).
    ///
    /// A [`SdfSource::Bake`] GPU jump-flood bakes the per-mesh signed distance fields from the
    /// mesh geometry (or loads them from the sidecar cache) and uploads them into the bindless
    /// SDF arrays of `descriptors`, so a field lives exactly as long as its mesh. A failed
    /// bake or upload is logged, not fatal — the mesh renders without a field.
    ///
    /// # Errors
    ///
    /// Returns [`Error::EmptyMesh`] for an empty mesh, [`Error::SkinMismatch`] when a
    /// skin stream does not parallel the vertices, or [`Error::Vk`] for a failing
    /// Vulkan/VMA call. Resources allocated before a failure are freed before return.
    pub fn upload_mesh(
        &self,
        descriptors: &Descriptors,
        mesh: &Mesh,
        hierarchy: &PortableVirtualHierarchy,
        skin: &[VertexSkin],
        morph: Option<&MorphData>,
        sdf: SdfSource<'_>,
    ) -> Result<Arc<GpuMesh>> {
        if mesh.vertices.is_empty() || mesh.indices.is_empty() {
            return Err(Error::EmptyMesh);
        }
        if !skin.is_empty() && skin.len() != mesh.vertices.len() {
            return Err(Error::SkinMismatch {
                skin: skin.len(),
                vertices: mesh.vertices.len(),
            });
        }
        validate_upload_hierarchy(mesh, hierarchy)?;

        let vertex_bytes = std::mem::size_of_val(mesh.vertices.as_slice()) as vk::DeviceSize;
        let index_bytes = std::mem::size_of_val(mesh.indices.as_slice()) as vk::DeviceSize;
        let skin_bytes = std::mem::size_of_val(skin) as vk::DeviceSize;

        // One staging buffer holds the three streams concatenated.
        let mut staging =
            StagingBuffer::new(self.allocator(), vertex_bytes + index_bytes + skin_bytes)?;
        {
            let bytes = staging.mapped_slice();
            let vb = vertex_bytes as usize;
            let ib = index_bytes as usize;
            bytes[..vb].copy_from_slice(bytemuck::cast_slice(&mesh.vertices));
            bytes[vb..vb + ib].copy_from_slice(bytemuck::cast_slice(&mesh.indices));
            if !skin.is_empty() {
                bytes[vb + ib..].copy_from_slice(bytemuck::cast_slice(skin));
            }
        }
        staging.flush();

        // Compute bounds; the complete CPU vertex stream is retained for surface queries.
        let mut bounds_min = Vec3::splat(f32::MAX);
        let mut bounds_max = Vec3::splat(f32::MIN);
        let mut cpu_positions = Vec::with_capacity(mesh.vertices.len());
        for vertex in &mesh.vertices {
            bounds_min = bounds_min.min(vertex.position);
            bounds_max = bounds_max.max(vertex.position);
            cpu_positions.push(vertex.position);
        }

        // When RT is on, the vertex/index buffers also feed BLAS builds: they need shader
        // device address + AS-build-input usage.
        let rt_usage = if self.accel.is_some() {
            vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS
                | vk::BufferUsageFlags::ACCELERATION_STRUCTURE_BUILD_INPUT_READ_ONLY_KHR
        } else {
            vk::BufferUsageFlags::empty()
        };

        // The base vertex stream is also read as a storage buffer by the compute deform pre-passes:
        // skinning + morph (skinned/morph meshes), and the `displace` pre-pass, which reads the base
        // positions/normals/tangents of *any* mesh carrying a displacement material — displacement is
        // a per-material choice, not a mesh property, so every mesh's vertex buffer carries STORAGE.
        let vertex_usage =
            vk::BufferUsageFlags::VERTEX_BUFFER | vk::BufferUsageFlags::STORAGE_BUFFER | rt_usage;

        // Allocate the device-local buffers; on a later failure free the
        // already-allocated ones (a `GpuMesh` never partially owns the set). Each
        // allocation is uniquely owned (a VMA `Allocation` is not `Copy`), so they are
        // freed directly here rather than tracked by a copied handle.
        let allocator = self.allocator();
        let vertex = make_device_buffer(allocator, vertex_bytes, vertex_usage)?;
        // STORAGE too: the tessellation emit kernel reads the base index stream as a `ByteAddressBuffer`
        // (`baseIndices`) to fetch each base triangle's corners, so a displaced mesh's index buffer is
        // bound as a storage buffer — the same per-material reason the vertex buffer carries STORAGE.
        let index = match make_device_buffer(
            allocator,
            index_bytes,
            vk::BufferUsageFlags::INDEX_BUFFER | vk::BufferUsageFlags::STORAGE_BUFFER | rt_usage,
        ) {
            Ok(buf) => buf,
            Err(err) => {
                free_one(allocator, vertex);
                return Err(err);
            }
        };
        let skin_buf = if skin.is_empty() {
            None
        } else {
            match make_device_buffer(
                allocator,
                skin_bytes,
                vk::BufferUsageFlags::VERTEX_BUFFER | vk::BufferUsageFlags::STORAGE_BUFFER,
            ) {
                Ok(buf) => Some(buf),
                Err(err) => {
                    free_one(allocator, vertex);
                    free_one(allocator, index);
                    return Err(err);
                }
            }
        };

        // Record + submit the staging copies.
        let copy = self.with_one_off_commands("upload_mesh", |cmd| {
            // SAFETY: the ash seam. The buffers outlive the submit-wait; the staging
            // buffer is the upload source.
            unsafe {
                let raw = self.raw();
                raw.cmd_copy_buffer(
                    cmd,
                    staging.handle(),
                    vertex.0,
                    &[vk::BufferCopy::default()
                        .src_offset(0)
                        .dst_offset(0)
                        .size(vertex_bytes)],
                );
                raw.cmd_copy_buffer(
                    cmd,
                    staging.handle(),
                    index.0,
                    &[vk::BufferCopy::default()
                        .src_offset(vertex_bytes)
                        .dst_offset(0)
                        .size(index_bytes)],
                );
                if let Some((skin_buffer, _)) = skin_buf {
                    raw.cmd_copy_buffer(
                        cmd,
                        staging.handle(),
                        skin_buffer,
                        &[vk::BufferCopy::default()
                            .src_offset(vertex_bytes + index_bytes)
                            .dst_offset(0)
                            .size(skin_bytes)],
                    );
                }
            }
        });
        drop(staging);
        if let Err(err) = copy {
            free_one(allocator, vertex);
            free_one(allocator, index);
            if let Some(buf) = skin_buf {
                free_one(allocator, buf);
            }
            return Err(err);
        }

        // Build this mesh's BLAS once (the RT geometry occlusion oracle) when RT is
        // available. A failure is logged, not fatal — the mesh renders without RT shadows.
        // An assembly's shape is its placed uses, not the concatenated prototype streams,
        // so it carries no merged BLAS (its RT representation is per-use instancing).
        // One geometry per submesh, each carrying its own cooked opacity: a single-geometry
        // structure can hold only one class, so one masked submesh would force the coverage
        // classifier onto every other submesh in the mesh.
        let submesh_opaque = cooked_submesh_opacity(hierarchy, mesh.submeshes.len());
        // Build every cooked micromap first, on its own submit. The BLAS build reads them by
        // device address, so they have to be complete and barriered before it starts — and they
        // must outlive every structure referencing them, which is why `GpuMesh` retains them.
        let micromaps = self.build_cooked_micromaps(hierarchy);
        let submesh_geometry = |submesh: usize| crate::MeshBlasGeometry {
            micromap: micromaps
                .iter()
                .find(|(index, _)| *index as usize == submesh)
                .map(|(_, micromap)| micromap.as_ref()),
            opaque: submesh_opaque.get(submesh).copied().unwrap_or(false),
            vertex_buffer: vertex.0,
            vertex_count: mesh.vertices.len() as u32,
            index_buffer: index.0,
            first_index: mesh.submeshes[submesh].first_index,
            index_count: mesh.submeshes[submesh].index_count,
        };
        let mut assembly = assembly_from_hierarchy(hierarchy)?;
        // An assembly's prototypes each own a contiguous run of the flattened submesh table,
        // in prototype-id order, so a prototype's index slice spans its submeshes. This is the
        // range each per-prototype BLAS builds over.
        let mut assembly_blas = Vec::new();
        if let Some(assembly) = assembly.as_mut() {
            let mut submesh = 0_usize;
            let mut prototype_spans = Vec::new();
            for (index, prototype) in hierarchy.prototypes.iter().enumerate() {
                let start = submesh.min(mesh.submeshes.len());
                let end = (submesh + prototype.submesh_count as usize).min(mesh.submeshes.len());
                let span = &mesh.submeshes[start..end];
                let first_index = span.first().map_or(0, |s| s.first_index);
                let index_count: u32 = span.iter().map(|s| s.index_count).sum();
                // The vertex base comes from the part table rather than being re-accumulated
                // here: the executor's vertex fetch reads that one, and two derivations of the
                // same base is how a raster pass and a materialized ray slice come to disagree.
                let first_vertex = assembly
                    .prototypes
                    .get(index)
                    .map_or(0, |record| record.vertex_base);
                assembly
                    .prototype_slices
                    .push(crate::AssemblyPrototypeSlice {
                        first_submesh: start as u32,
                        submesh_count: (end - start) as u32,
                        first_index,
                        index_count,
                        first_vertex,
                        vertex_count: prototype.vertex_count,
                    });
                prototype_spans.push(start..end);
                submesh += prototype.submesh_count as usize;
            }
            // One structure per prototype, one geometry per submesh within it. A failure is
            // logged, not fatal: the family renders without ray-traced shadows rather than not
            // at all. A cluster-AS device composes the structure from the prototype's cooked
            // clusters; anything that disqualifies that build falls back to the KHR triangles —
            // including a span with cooked opacity micromaps, which only the KHR geometry
            // chain attaches.
            for (prototype_index, span) in prototype_spans.into_iter().enumerate() {
                let span_micromapped = micromaps
                    .iter()
                    .any(|(index, _)| span.contains(&(*index as usize)));
                let clustered = if span_micromapped {
                    None
                } else {
                    match self.build_prototype_cluster_blas(
                        hierarchy,
                        prototype_index as u32,
                        span.clone(),
                        mesh,
                        &submesh_opaque,
                    ) {
                        Ok(built) => built,
                        Err(err) => {
                            tracing::warn!("cluster BLAS build failed: {err}; using the KHR build");
                            None
                        }
                    }
                };
                if let Some(blas) = clustered {
                    assembly_blas.push(crate::RtBlas::Cluster(Arc::new(blas)));
                    continue;
                }
                let geometries: Vec<_> = span.map(submesh_geometry).collect();
                match self.build_mesh_blas(&geometries) {
                    Ok(Some(blas)) => assembly_blas.push(crate::RtBlas::Khr(blas)),
                    Ok(None) => {}
                    Err(err) => {
                        tracing::warn!("assembly prototype BLAS build failed: {err}");
                        assembly_blas.clear();
                        break;
                    }
                }
            }
            if assembly_blas.len() != assembly.prototype_slices.len() {
                // A partial set would place some uses and silently drop others, which is worse
                // than placing none: the canopy would cast a half-complete shadow.
                assembly_blas.clear();
            }
        }
        let blas = if assembly.is_some() {
            None
        } else {
            let geometries: Vec<_> = (0..mesh.submeshes.len()).map(submesh_geometry).collect();
            match self.build_mesh_blas(&geometries) {
                Ok(blas) => blas,
                Err(err) => {
                    tracing::warn!("BLAS build failed: {err}");
                    None
                }
            }
        };
        // The aggregate representation: one family-space structure over the root cut's
        // voxel-brick surfaces. TLAS packing swaps a distant instance to it — one instance
        // for the whole family instead of one per use — mirroring the raster traversal,
        // which draws exactly these bricks once the root's projected error fits under the
        // threshold. A failure is logged, not fatal: the instance keeps the fine structures.
        let aggregate_blas = match self.build_aggregate_blas(hierarchy) {
            Ok(aggregate) => aggregate,
            Err(err) => {
                tracing::warn!("aggregate BLAS build failed: {err}");
                None
            }
        };

        // Build the morph buffers (flat delta array + per-target ranges) when the mesh
        // carries blend shapes; free the already-owned buffers on a morph-upload failure.
        let morph_buffers = match morph.filter(|m| !m.targets.is_empty()) {
            Some(data) => match self.upload_morph_buffers(data) {
                Ok(buffers) => Some(buffers),
                Err(err) => {
                    free_one(allocator, vertex);
                    free_one(allocator, index);
                    if let Some(buf) = skin_buf {
                        free_one(allocator, buf);
                    }
                    return Err(err);
                }
            },
            None => None,
        };

        // GPU jump-flood bake (or sidecar-cache load) the per-mesh signed distance fields —
        // one tight field per primitive, and per spatial chunk of an oversized primitive — and
        // upload each into the bindless SDF arrays. A failure is logged, not fatal: the mesh
        // renders without a field (the cone-trace simply omits an instance with no SDF slot). A
        // degenerate mesh (< 1 triangle) bakes no field.
        let gpu_sdfs = match sdf {
            SdfSource::Cooked(fields) => fields
                .iter()
                .filter_map(|field| match self.upload_sdf(descriptors, field) {
                    Ok(uploaded) => Some(uploaded),
                    Err(err) => {
                        tracing::warn!("cooked SDF upload failed: {err}");
                        None
                    }
                })
                .collect(),
            SdfSource::Bake(bake) if mesh.indices.len() >= 3 && !cpu_positions.is_empty() => {
                match self.bake_or_load_sdf(&cpu_positions, &mesh.indices, &mesh.submeshes, bake) {
                    Ok(fields) => fields
                        .iter()
                        .filter_map(|sdf| match self.upload_sdf(descriptors, sdf) {
                            Ok(field) => Some(field),
                            Err(err) => {
                                tracing::warn!("SDF upload failed: {err}");
                                None
                            }
                        })
                        .collect(),
                    Err(err) => {
                        tracing::warn!("SDF bake failed: {err}");
                        Vec::new()
                    }
                }
            }
            _ => Vec::new(),
        };

        // Build + upload the watertight-conditioning buffers (edges/weld/basis). A failure is
        // logged, not fatal — the mesh then carries none.
        let conditioning_buffers = match self.upload_conditioning_buffers(mesh) {
            Ok(buffers) => buffers,
            Err(err) => {
                tracing::warn!("conditioning upload failed: {err}");
                None
            }
        };

        let parts = GpuMeshParts {
            vertex,
            index,
            skin: skin_buf,
            morph: morph_buffers,
            conditioning: conditioning_buffers,
            index_count: mesh.indices.len() as u32,
            vertex_count: mesh.vertices.len() as u32,
            submeshes: mesh.submeshes.clone(),
            submesh_opaque,
            micromaps: micromaps
                .into_iter()
                .map(|(_, micromap)| micromap)
                .collect(),
            bounds_min,
            bounds_max,
            cpu_vertices: mesh.vertices.clone(),
            cpu_indices: mesh.indices.clone(),
            cpu_skin: skin.to_vec(),
            blas,
            assembly_blas,
            aggregate_blas,
            sdfs: gpu_sdfs,
            hierarchy_pages: hierarchy.pages.clone(),
            assembly,
        };
        Ok(Arc::new(GpuMesh::from_parts(&self.resources, parts)))
    }

    /// Builds the device-local morph buffers from [`MorphData`]: the flat `MorphDelta`
    /// array (each target's deltas concatenated) and the per-target `[first_delta,
    /// delta_count]` ranges, both `STORAGE` for the morph compute pass. Frees the delta
    /// buffer if the range buffer or the copy fails.
    fn upload_morph_buffers(&self, data: &MorphData) -> Result<MorphBuffers> {
        let mut deltas: Vec<MorphDelta> = Vec::new();
        let mut ranges: Vec<[u32; 2]> = Vec::with_capacity(data.targets.len());
        for target in &data.targets {
            let first = deltas.len() as u32;
            ranges.push([first, target.deltas.len() as u32]);
            deltas.extend_from_slice(&target.deltas);
        }
        let delta_count = deltas.len() as u32;
        let target_count = ranges.len() as u32;

        // Each device buffer must be non-empty even when a count is zero (a rest-only morph
        // mesh); pad to one record. The shader never reads past the real counts.
        let delta_used = std::mem::size_of_val(deltas.as_slice());
        let range_used = std::mem::size_of_val(ranges.as_slice());
        let delta_bytes = delta_used.max(std::mem::size_of::<MorphDelta>()) as vk::DeviceSize;
        let range_bytes = range_used.max(std::mem::size_of::<[u32; 2]>()) as vk::DeviceSize;

        let mut staging = StagingBuffer::new(self.allocator(), delta_bytes + range_bytes)?;
        {
            let bytes = staging.mapped_slice();
            if delta_used > 0 {
                bytes[..delta_used].copy_from_slice(bytemuck::cast_slice(&deltas));
            }
            if range_used > 0 {
                let base = delta_bytes as usize;
                bytes[base..base + range_used].copy_from_slice(bytemuck::cast_slice(&ranges));
            }
        }
        staging.flush();

        let allocator = self.allocator();
        let deltas_buf =
            make_device_buffer(allocator, delta_bytes, vk::BufferUsageFlags::STORAGE_BUFFER)?;
        let ranges_buf = match make_device_buffer(
            allocator,
            range_bytes,
            vk::BufferUsageFlags::STORAGE_BUFFER,
        ) {
            Ok(buf) => buf,
            Err(err) => {
                free_one(allocator, deltas_buf);
                return Err(err);
            }
        };

        let copy = self.with_one_off_commands("upload_morph_buffers", |cmd| {
            // SAFETY: the ash seam. Both device buffers outlive the submit-wait; the staging
            // buffer is the source.
            unsafe {
                let raw = self.raw();
                raw.cmd_copy_buffer(
                    cmd,
                    staging.handle(),
                    deltas_buf.0,
                    &[vk::BufferCopy::default().size(delta_bytes)],
                );
                raw.cmd_copy_buffer(
                    cmd,
                    staging.handle(),
                    ranges_buf.0,
                    &[vk::BufferCopy::default()
                        .src_offset(delta_bytes)
                        .dst_offset(0)
                        .size(range_bytes)],
                );
            }
        });
        drop(staging);
        if let Err(err) = copy {
            free_one(allocator, deltas_buf);
            free_one(allocator, ranges_buf);
            return Err(err);
        }

        Ok(MorphBuffers {
            deltas: deltas_buf,
            ranges: ranges_buf,
            cpu_ranges: ranges,
            target_count,
            delta_count,
        })
    }

    /// Builds the watertight-conditioning data ([`MeshConditioning`], a pure function of the mesh, as
    /// the cook's clusterer is) and uploads its four arrays as device-local `STORAGE_BUFFER`s: the
    /// unique edges, the per-triangle edge indices, the per-welded-vertex basis, and the base→welded
    /// map. One staging buffer with four contiguous slices, one `cmd_copy_buffer` per slice.
    /// Non-fatal on failure (the mesh then carries no conditioning).
    fn upload_conditioning_buffers(&self, mesh: &Mesh) -> Result<Option<ConditioningBuffers>> {
        if mesh.vertices.is_empty() {
            return Ok(None);
        }
        let cond = MeshConditioning::build(mesh);

        let edges_bytes = std::mem::size_of_val(cond.edges.as_slice());
        let tri_bytes = std::mem::size_of_val(cond.tri_edges.as_slice());
        let welded_bytes = std::mem::size_of_val(cond.welded.as_slice());
        let weld_id_bytes = std::mem::size_of_val(cond.weld_id.as_slice());
        // Grow-to-stride so an empty array (a mesh with no triangles) still backs a valid buffer.
        let edges_size = edges_bytes.max(16) as vk::DeviceSize;
        let tri_size = tri_bytes.max(16) as vk::DeviceSize;
        let welded_size =
            welded_bytes.max(size_of::<saffron_geometry::WeldedVertex>()) as vk::DeviceSize;
        let weld_id_size = weld_id_bytes.max(4) as vk::DeviceSize;

        let mut staging = StagingBuffer::new(
            self.allocator(),
            edges_size + tri_size + welded_size + weld_id_size,
        )?;
        {
            let bytes = staging.mapped_slice();
            let (e, t, w) = (edges_size as usize, tri_size as usize, welded_size as usize);
            if edges_bytes > 0 {
                bytes[..edges_bytes].copy_from_slice(bytemuck::cast_slice(&cond.edges));
            }
            if tri_bytes > 0 {
                bytes[e..e + tri_bytes].copy_from_slice(bytemuck::cast_slice(&cond.tri_edges));
            }
            if welded_bytes > 0 {
                bytes[e + t..e + t + welded_bytes]
                    .copy_from_slice(bytemuck::cast_slice(&cond.welded));
            }
            if weld_id_bytes > 0 {
                bytes[e + t + w..e + t + w + weld_id_bytes]
                    .copy_from_slice(bytemuck::cast_slice(&cond.weld_id));
            }
        }
        staging.flush();

        let allocator = self.allocator();
        let usage = vk::BufferUsageFlags::STORAGE_BUFFER;
        let edges_buf = make_device_buffer(allocator, edges_size, usage)?;
        let tri_buf = match make_device_buffer(allocator, tri_size, usage) {
            Ok(buf) => buf,
            Err(err) => {
                free_one(allocator, edges_buf);
                return Err(err);
            }
        };
        let welded_buf = match make_device_buffer(allocator, welded_size, usage) {
            Ok(buf) => buf,
            Err(err) => {
                free_one(allocator, edges_buf);
                free_one(allocator, tri_buf);
                return Err(err);
            }
        };
        let weld_id_buf = match make_device_buffer(allocator, weld_id_size, usage) {
            Ok(buf) => buf,
            Err(err) => {
                free_one(allocator, edges_buf);
                free_one(allocator, tri_buf);
                free_one(allocator, welded_buf);
                return Err(err);
            }
        };

        let copy = self.with_one_off_commands("upload_conditioning_buffers", |cmd| {
            // SAFETY: the ash seam. All four device buffers outlive the submit-wait; the staging
            // buffer is the source for each contiguous slice.
            unsafe {
                let raw = self.raw();
                raw.cmd_copy_buffer(
                    cmd,
                    staging.handle(),
                    edges_buf.0,
                    &[vk::BufferCopy::default().size(edges_size)],
                );
                raw.cmd_copy_buffer(
                    cmd,
                    staging.handle(),
                    tri_buf.0,
                    &[vk::BufferCopy::default()
                        .src_offset(edges_size)
                        .size(tri_size)],
                );
                raw.cmd_copy_buffer(
                    cmd,
                    staging.handle(),
                    welded_buf.0,
                    &[vk::BufferCopy::default()
                        .src_offset(edges_size + tri_size)
                        .size(welded_size)],
                );
                raw.cmd_copy_buffer(
                    cmd,
                    staging.handle(),
                    weld_id_buf.0,
                    &[vk::BufferCopy::default()
                        .src_offset(edges_size + tri_size + welded_size)
                        .size(weld_id_size)],
                );
            }
        });
        drop(staging);
        if let Err(err) = copy {
            free_one(allocator, edges_buf);
            free_one(allocator, tri_buf);
            free_one(allocator, welded_buf);
            free_one(allocator, weld_id_buf);
            return Err(err);
        }

        Ok(Some(ConditioningBuffers {
            edges: edges_buf,
            tri_edges: tri_buf,
            welded: welded_buf,
            weld_id: weld_id_buf,
            edge_count: cond.edges.len() as u32,
            welded_count: cond.welded.len() as u32,
        }))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::resources::BindlessFreeList;
    use crate::upload::fixtures::{device_or_skip, triangle};
    use crate::upload::hierarchy_for_upload;
    use crate::validation_issue_count;

    /// Uploading a mesh with a skin stream produces a `GpuMesh` with a non-null skin
    /// buffer; uploading without one leaves it null — the phase's named skin gate. The
    /// upload runs the real staging→device-local copy on the queue, validation-clean.
    /// Skips when no Vulkan device is present.
    #[test]
    fn upload_mesh_skin_buffer_presence_tracks_the_stream() {
        let Some(device) = device_or_skip() else {
            return;
        };
        let before = validation_issue_count();
        let free_list: BindlessFreeList = Arc::new(Mutex::new(Vec::new()));
        let descriptors = Descriptors::new(&device, &free_list).expect("Descriptors::new");
        let queue = device.graphics_queue.clone();
        let uploader = Uploader::new(&device, &queue).expect("Uploader::new");
        let mesh = triangle();
        let plain_hierarchy = hierarchy_for_upload(&mesh, &[]).expect("cook hierarchy");

        let plain = uploader
            .upload_mesh(
                &descriptors,
                &mesh,
                &plain_hierarchy,
                &[],
                None,
                crate::SdfSource::None,
            )
            .expect("unskinned upload");
        assert_eq!(plain.index_count, 3);
        assert_eq!(plain.vertex_count, 3);
        assert!(
            plain.skin_buffer().is_none(),
            "no skin stream → null skin buffer"
        );
        assert_eq!(&*plain.cpu_indices, mesh.indices.as_slice());
        assert_eq!(plain.cpu_vertices.len(), 3);

        let skin = vec![VertexSkin::default(); mesh.vertices.len()];
        let skinned_hierarchy = hierarchy_for_upload(&mesh, &skin).expect("cook hierarchy");
        let skinned = uploader
            .upload_mesh(
                &descriptors,
                &mesh,
                &skinned_hierarchy,
                &skin,
                None,
                crate::SdfSource::None,
            )
            .expect("skinned upload");
        assert!(
            skinned.skin_buffer().is_some(),
            "a parallel skin stream → non-null skin buffer"
        );
        assert_eq!(skinned.cpu_skin.len(), 3);

        // A mismatched skin stream is rejected before any allocation.
        let bad = uploader.upload_mesh(
            &descriptors,
            &mesh,
            &plain_hierarchy,
            &[VertexSkin::default()],
            None,
            crate::SdfSource::None,
        );
        assert!(matches!(bad, Err(Error::SkinMismatch { .. })));

        drop(plain);
        drop(skinned);
        drop(uploader);
        drop(descriptors);
        device.wait_idle().expect("idle before teardown");
        drop(device);

        let after = validation_issue_count();
        assert_eq!(
            before,
            after,
            "the mesh uploads must be validation-clean (saw {} new issue(s))",
            after.saturating_sub(before)
        );
    }
}
