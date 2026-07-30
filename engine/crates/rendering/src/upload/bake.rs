use ash::vk;
use saffron_geometry::glam::Vec3;
use saffron_geometry::{
    GridDesc, Sdf, Submesh, bake_grid, sdf_chunk_cores, sdf_set_from_bytes, sdf_set_to_bytes,
};
use vk_mem::Alloc;

use super::bake_pipelines::{BakeImages, BakePipelines, BakePush};
use super::barriers::transition_image;
use super::staging::{StagingBuffer, free_one, make_device_buffer};
use super::{SdfBake, Uploader};
use crate::resources::Image3D;
use crate::{Error, Result, checked};

impl Uploader {
    /// Returns a mesh's signed distance fields (one tight field per primitive / spatial chunk)
    /// for `positions`/`indices`/`submeshes`: a sidecar-cache hit
    /// (`<cache_dir>/<meshHash>.sdfset`) when present, else a fresh GPU jump-flood
    /// [`Uploader::bake_sdf`] whose bytes are written back to the cache. The content hash keys
    /// the cache on the geometry, so a touched-but-unchanged mesh reuses its bake.
    ///
    /// # Errors
    ///
    /// [`Error::SdfBake`] when no bake pipelines are present, or [`Error::Vk`] from the bake
    /// dispatch. A malformed sidecar is ignored (re-baked), not an error.
    pub fn bake_or_load_sdf(
        &self,
        positions: &[Vec3],
        indices: &[u32],
        submeshes: &[Submesh],
        bake: &SdfBake,
    ) -> Result<Vec<Sdf>> {
        let hash = mesh_content_hash(positions, indices, bake.resolution_scale);
        let cache_path = bake
            .cache_dir
            .as_ref()
            .map(|dir| dir.join(format!("{hash:016x}.sdfset")));

        if let Some(path) = cache_path.as_ref()
            && let Ok(bytes) = std::fs::read(path)
        {
            match sdf_set_from_bytes(&bytes) {
                Ok(fields) => {
                    tracing::debug!(%hash, fields = fields.len(), "SDF sidecar cache hit");
                    return Ok(fields);
                }
                Err(err) => tracing::warn!(%hash, "SDF sidecar unreadable, re-baking: {err}"),
            }
        }

        let fields = self.bake_sdf(positions, indices, submeshes, bake.resolution_scale)?;
        if let Some(path) = cache_path.as_ref() {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if let Err(err) = std::fs::write(path, sdf_set_to_bytes(&fields)) {
                tracing::warn!(%hash, "writing SDF sidecar failed: {err}");
            }
        }
        Ok(fields)
    }

    /// GPU jump-flood bakes a mesh's signed distance fields — the whole mesh partitioned once
    /// into a bounded grid of spatial cells ([`sdf_chunk_cores`]), one fine field per non-empty
    /// cell. The partition is spatial rather than per primitive: a merged mesh batches geometry
    /// by material into scene-spanning primitives, so primitive boundaries would not localize a
    /// field, while a spatial cell always gets a tight AABB (the many-small-fields decomposition
    /// `min`-over-instances combines). A cell that baked no occupied brick is dropped.
    ///
    /// # Errors
    ///
    /// [`Error::SdfBake`] when the bake pipelines are absent; [`Error::Vk`] for a failing
    /// dispatch / allocation.
    pub fn bake_sdf(
        &self,
        positions: &[Vec3],
        indices: &[u32],
        submeshes: &[Submesh],
        resolution_scale: f32,
    ) -> Result<Vec<Sdf>> {
        let bake = self
            .bake
            .as_ref()
            .ok_or_else(|| Error::SdfBake("no bake pipelines (missing shaders)".to_owned()))?;

        // The whole mesh's global triangle list (every primitive), degenerate slivers dropped up
        // front as the `MeshBvh` oracle does: a sliver carries no surface and its unstable normal
        // would poison the sign cross-check. Submesh indices are primitive-local — Vulkan adds
        // `vertex_offset` at fetch — so the global vertex is `vertex_offset + index`. A mesh with
        // no explicit submeshes (a single-primitive source) is one range at offset 0.
        let whole = [Submesh {
            first_index: 0,
            index_count: indices.len() as u32,
            vertex_offset: 0,
            material_slot: 0,
        }];
        let prims: &[Submesh] = if submeshes.is_empty() {
            &whole
        } else {
            submeshes
        };

        let mut tris: Vec<u32> = Vec::with_capacity(indices.len());
        let mut lo = Vec3::splat(f32::INFINITY);
        let mut hi = Vec3::splat(f32::NEG_INFINITY);
        for prim in prims {
            let base = prim.first_index as usize;
            let end = base + prim.index_count as usize;
            let range = indices.get(base..end).unwrap_or(&[]);
            for t in range.chunks_exact(3) {
                let g = [
                    (prim.vertex_offset as u32).wrapping_add(t[0]),
                    (prim.vertex_offset as u32).wrapping_add(t[1]),
                    (prim.vertex_offset as u32).wrapping_add(t[2]),
                ];
                let (a, b, c) = (
                    positions[g[0] as usize],
                    positions[g[1] as usize],
                    positions[g[2] as usize],
                );
                let (e1, e2) = (b - a, c - a);
                if e1.cross(e2).length_squared()
                    <= 1e-10 * e1.length_squared() * e2.length_squared()
                {
                    continue;
                }
                tris.extend_from_slice(&g);
                lo = lo.min(a.min(b).min(c));
                hi = hi.max(a.max(b).max(c));
            }
        }
        if tris.len() < 3 {
            return Ok(Vec::new());
        }

        // Signed volume orients the nearest-triangle-normal sign so an inward-wound source still
        // reads negative inside (mirrors `MeshBvh`). Taken over the whole mesh and shared across
        // its spatial cells — a cell is not a closed volume, so its own signed volume is
        // meaningless.
        let mut vol6 = 0.0f64;
        for t in tris.chunks_exact(3) {
            let a = positions[t[0] as usize];
            let b = positions[t[1] as usize];
            let c = positions[t[2] as usize];
            vol6 += f64::from(a.dot(b.cross(c)));
        }
        let winding = if vol6 >= 0.0 { 1.0 } else { -1.0 };

        let cells = sdf_chunk_cores(lo, hi, resolution_scale);
        let span = tracing::info_span!("sdf_bake", cells = cells.len()).entered();
        let started = std::time::Instant::now();
        let mut fields = Vec::new();
        for (c0, c1) in cells {
            let grid = bake_grid(c0, c1, resolution_scale);
            if let Some(sdf) = self.bake_region(bake, positions, &tris, &grid, winding)? {
                fields.push(sdf);
            }
        }

        drop(span);
        tracing::debug!(
            fields = fields.len(),
            ms = started.elapsed().as_secs_f32() * 1000.0,
            "SDF GPU bake complete"
        );
        Ok(fields)
    }

    /// Bakes one SDF region over the derived `grid`: keeps only the triangles whose AABB
    /// overlaps the padded grid bounds (compacting their vertices so the geometry upload stays
    /// small), then voxelize → jump-flood → sign → compact.
    ///
    /// The GPU owns the nearest-surface search and reads back a per-voxel seed (closest surface
    /// point + packed nearest-triangle normal); the sign is resolved on the host by an
    /// outside-flood cross-checked against that normal, which — unlike a single face normal —
    /// does not read a concave-corner interior voxel as outside. `None` when the region covers
    /// empty air (every brick saturated to `+max`).
    ///
    /// # Errors
    ///
    /// [`Error::Vk`] for a failing bake dispatch / allocation.
    fn bake_region(
        &self,
        bake: &BakePipelines,
        positions: &[Vec3],
        tris: &[u32],
        grid: &GridDesc,
        winding: f32,
    ) -> Result<Option<Sdf>> {
        let (local_positions, local_indices) =
            compact_tris_in_bounds(positions, tris, grid.bounds_min, grid.bounds_max);
        if local_indices.len() < 3 {
            return Ok(None);
        }
        let seeds = self.run_bake_passes(bake, grid, &local_positions, &local_indices)?;
        let dense = sign_field(grid, winding, &seeds);
        let sdf = Sdf::from_dense_field(grid, &dense);
        if sdf.header.occupied_bricks == 0 {
            return Ok(None);
        }
        Ok(Some(sdf))
    }

    /// Records and submits the voxelize → JFA dispatches for `grid` over the mesh geometry,
    /// then reads the final jump-flood seed volume (closest surface point in `xyz`, the
    /// octahedral-packed nearest-triangle normal in `w`, X-fastest) back to the host for the
    /// CPU sign pass. The transient seed 3D images + the geometry storage buffers live only
    /// for this call.
    fn run_bake_passes(
        &self,
        bake: &BakePipelines,
        grid: &GridDesc,
        positions: &[Vec3],
        indices: &[u32],
    ) -> Result<Vec<[f32; 4]>> {
        let [nx, ny, nz] = grid.dims;
        let extent = vk::Extent3D {
            width: nx,
            height: ny,
            depth: nz,
        };
        let voxel_count = nx as usize * ny as usize * nz as usize;
        let tri_count = (indices.len() / 3) as u32;
        let cell = grid.cell();

        // Geometry storage buffers (positions as float4, the index stream), staged + copied.
        let pos4: Vec<[f32; 4]> = positions.iter().map(|p| [p.x, p.y, p.z, 0.0]).collect();
        let pos_bytes = std::mem::size_of_val(pos4.as_slice()) as vk::DeviceSize;
        let idx_bytes = std::mem::size_of_val(indices) as vk::DeviceSize;
        let mut staging = StagingBuffer::new(self.allocator(), pos_bytes + idx_bytes)?;
        {
            let dst = staging.mapped_slice();
            dst[..pos_bytes as usize].copy_from_slice(bytemuck::cast_slice(&pos4));
            dst[pos_bytes as usize..(pos_bytes + idx_bytes) as usize]
                .copy_from_slice(bytemuck::cast_slice(indices));
        }
        staging.flush();
        let allocator = self.allocator();
        let pos_buf =
            make_device_buffer(allocator, pos_bytes, vk::BufferUsageFlags::STORAGE_BUFFER)?;
        let idx_buf =
            match make_device_buffer(allocator, idx_bytes, vk::BufferUsageFlags::STORAGE_BUFFER) {
                Ok(buf) => buf,
                Err(err) => {
                    free_one(allocator, pos_buf);
                    return Err(err);
                }
            };

        // Transient seed / work images (storage; both seed buffers are also a transfer src,
        // since the final jump-flood result lands in A or B by parity and is read back).
        // RAII so an early `?` frees them.
        let bake_images = (|| -> Result<BakeImages> {
            let seed_key = Image3D::new(
                &self.resources,
                extent,
                vk::Format::R32_UINT,
                1,
                vk::ImageUsageFlags::STORAGE | vk::ImageUsageFlags::TRANSFER_DST,
            )?;
            let seed_a = Image3D::new(
                &self.resources,
                extent,
                vk::Format::R32G32B32A32_SFLOAT,
                1,
                vk::ImageUsageFlags::STORAGE | vk::ImageUsageFlags::TRANSFER_SRC,
            )?;
            let seed_b = Image3D::new(
                &self.resources,
                extent,
                vk::Format::R32G32B32A32_SFLOAT,
                1,
                vk::ImageUsageFlags::STORAGE | vk::ImageUsageFlags::TRANSFER_SRC,
            )?;
            Ok(BakeImages {
                seed_key,
                seed_a,
                seed_b,
            })
        })();
        let bake_images = match bake_images {
            Ok(images) => images,
            Err(err) => {
                free_one(allocator, pos_buf);
                free_one(allocator, idx_buf);
                return Err(err);
            }
        };

        // Host-readable destination for the final seed volume (RGBA32F → four f32 per voxel).
        let read_bytes = (voxel_count * 16) as vk::DeviceSize;
        let read_alloc = vk_mem::AllocationCreateInfo {
            usage: vk_mem::MemoryUsage::AutoPreferHost,
            flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_RANDOM
                | vk_mem::AllocationCreateFlags::MAPPED,
            ..Default::default()
        };
        let read_info = vk::BufferCreateInfo::default()
            .size(read_bytes)
            .usage(vk::BufferUsageFlags::TRANSFER_DST);
        // SAFETY: the VMA seam. Owned + freed below after the readback.
        let (read_buf, mut read_allocation) = match checked(
            unsafe { allocator.create_buffer(&read_info, &read_alloc) },
            "vmaCreateBuffer (sdf readback)",
        ) {
            Ok(parts) => parts,
            Err(err) => {
                free_one(allocator, pos_buf);
                free_one(allocator, idx_buf);
                return Err(err);
            }
        };

        // Allocate + write the bake descriptor set (the 6 resources). Freed before return.
        let set = match bake.allocate_set(self.raw()) {
            Ok(set) => set,
            Err(err) => {
                // SAFETY: the VMA seam. The readback buffer + geometry buffers are freed
                // once on this error path before the images drop.
                unsafe { allocator.destroy_buffer(read_buf, &mut read_allocation) };
                free_one(allocator, pos_buf);
                free_one(allocator, idx_buf);
                return Err(err);
            }
        };
        bake.write_set(self.raw(), set, pos_buf.0, idx_buf.0, &bake_images);

        let max_dim = nx.max(ny).max(nz);
        let mut steps: Vec<i32> = Vec::new();
        let mut step = (max_dim.next_power_of_two() / 2).max(1);
        loop {
            steps.push(step as i32);
            if step == 1 {
                break;
            }
            step /= 2;
        }
        // After init (writes A), each prop step alternates the result buffer B,A,B,A…; an
        // even step count leaves the result in A (final parity 0), odd in B (final parity 1).
        let final_parity = if steps.len().is_multiple_of(2) { 0 } else { 1 };

        let base_push = BakePush {
            dims: [nx, ny, nz, tri_count],
            bounds_min: [
                grid.bounds_min.x,
                grid.bounds_min.y,
                grid.bounds_min.z,
                grid.max_dist,
            ],
            cell: [cell.x, cell.y, cell.z, 0.0],
            misc: [0; 4],
        };

        let recorded = self.with_one_off_commands("run_bake_passes", |cmd| {
            // SAFETY: the ash seam. Every resource outlives the submit-wait below.
            unsafe {
                let raw = self.raw();
                // Stage the geometry into the device-local storage buffers the bake reads;
                // `record_bake`'s transfer→compute barrier makes these copies visible.
                raw.cmd_copy_buffer(
                    cmd,
                    staging.handle(),
                    pos_buf.0,
                    &[vk::BufferCopy::default().size(pos_bytes)],
                );
                raw.cmd_copy_buffer(
                    cmd,
                    staging.handle(),
                    idx_buf.0,
                    &[vk::BufferCopy::default()
                        .src_offset(pos_bytes)
                        .dst_offset(0)
                        .size(idx_bytes)],
                );
                self.record_bake(cmd, bake, set, &bake_images, &base_push, &steps);
                // Copy the final jump-flood seed image (GENERAL) into the host-readable
                // buffer. The result lands in A for an even prop-step count, B for odd.
                let final_seed = if final_parity == 0 {
                    bake_images.seed_a.handle()
                } else {
                    bake_images.seed_b.handle()
                };
                let region = vk::BufferImageCopy::default()
                    .image_subresource(vk::ImageSubresourceLayers {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        mip_level: 0,
                        base_array_layer: 0,
                        layer_count: 1,
                    })
                    .image_extent(extent);
                raw.cmd_copy_image_to_buffer(
                    cmd,
                    final_seed,
                    vk::ImageLayout::GENERAL,
                    read_buf,
                    &[region],
                );
            }
        });

        // SAFETY: the ash seam. Free the descriptor set (the submit completed or never ran).
        unsafe {
            let _ = self.raw().free_descriptor_sets(bake.pool, &[set]);
        }

        let result = recorded.and_then(|()| {
            // SAFETY: the VMA seam. Make the GPU writes host-visible, then read the seed
            // volume (four f32 per voxel: closest surface point xyz + packed normal w).
            let seeds = unsafe {
                let _ = allocator.invalidate_allocation(&read_allocation, 0, read_bytes);
                let ptr = allocator
                    .get_allocation_info(&read_allocation)
                    .mapped_data
                    .cast::<[f32; 4]>();
                if ptr.is_null() {
                    return Err(Error::SdfBake("readback buffer not mapped".to_owned()));
                }
                std::slice::from_raw_parts(ptr, voxel_count).to_vec()
            };
            Ok(seeds)
        });

        drop(staging);
        // SAFETY: the VMA seam. The readback + geometry buffers are freed exactly once after
        // the submit completed; the transient images drop with `bake_images`.
        unsafe { allocator.destroy_buffer(read_buf, &mut read_allocation) };
        free_one(allocator, pos_buf);
        free_one(allocator, idx_buf);
        drop(bake_images);
        result
    }

    /// Records the voxelize → JFA(init + propagate) dispatch sequence into `cmd` with the
    /// barriers between every storage-image write and its next read. The final seed image
    /// (A or B by parity) is left in `GENERAL` ready for the readback copy; the host resolves
    /// the sign from it.
    ///
    /// # Safety
    ///
    /// `cmd` must be recording; every resource in `images`/`set` must outlive the submit.
    unsafe fn record_bake(
        &self,
        cmd: vk::CommandBuffer,
        bake: &BakePipelines,
        set: vk::DescriptorSet,
        images: &BakeImages,
        base_push: &BakePush,
        steps: &[i32],
    ) {
        let raw = self.raw();
        // SAFETY: the caller's recording contract.
        unsafe {
            // Storage images to GENERAL. seed_key receives a transfer clear first (so its
            // barrier targets the CLEAR stage + transfer-write access); the rest are first
            // touched by the compute passes.
            let compute_rw =
                vk::AccessFlags2::SHADER_STORAGE_READ | vk::AccessFlags2::SHADER_STORAGE_WRITE;
            for (image, dst_stage, dst_access) in [
                (
                    images.seed_key.handle(),
                    vk::PipelineStageFlags2::CLEAR,
                    vk::AccessFlags2::TRANSFER_WRITE,
                ),
                (
                    images.seed_a.handle(),
                    vk::PipelineStageFlags2::COMPUTE_SHADER,
                    compute_rw,
                ),
                (
                    images.seed_b.handle(),
                    vk::PipelineStageFlags2::COMPUTE_SHADER,
                    compute_rw,
                ),
            ] {
                transition_image(
                    raw,
                    cmd,
                    image,
                    1,
                    vk::ImageLayout::UNDEFINED,
                    vk::ImageLayout::GENERAL,
                    vk::PipelineStageFlags2::TOP_OF_PIPE,
                    vk::AccessFlags2::empty(),
                    dst_stage,
                    dst_access,
                );
            }
            // Seed the key image with 0xFFFFFFFF (no triangle), the InterlockedMin identity.
            let clear = vk::ClearColorValue {
                uint32: [u32::MAX; 4],
            };
            let range = vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            };
            raw.cmd_clear_color_image(
                cmd,
                images.seed_key.handle(),
                vk::ImageLayout::GENERAL,
                &clear,
                &[range],
            );
            // The geometry copies + the key clear (all transfer writes) → compute reads.
            transfer_to_compute_barrier(raw, cmd);

            raw.cmd_bind_descriptor_sets(
                cmd,
                vk::PipelineBindPoint::COMPUTE,
                bake.pipeline_layout,
                0,
                &[set],
                &[],
            );

            let push = |cmd: vk::CommandBuffer, p: &BakePush| {
                raw.cmd_push_constants(
                    cmd,
                    bake.pipeline_layout,
                    vk::ShaderStageFlags::COMPUTE,
                    0,
                    bytemuck::bytes_of(p),
                );
            };
            let groups = |n: u32, local: u32| n.div_ceil(local);
            let voxel_groups = (
                groups(base_push.dims[0], 4),
                groups(base_push.dims[1], 4),
                groups(base_push.dims[2], 4),
            );

            // Voxelize: one thread per triangle.
            raw.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, bake.voxelize);
            let mut vox = *base_push;
            vox.misc = [0; 4];
            push(cmd, &vox);
            raw.cmd_dispatch(cmd, groups(base_push.dims[3], 64).max(1), 1, 1);
            compute_to_compute_barrier(raw, cmd);

            // JFA init: decode the winning triangle per voxel into seed buffer A.
            raw.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, bake.jfa);
            let mut init = *base_push;
            init.misc = [0, 0, 1, 0];
            push(cmd, &init);
            raw.cmd_dispatch(cmd, voxel_groups.0, voxel_groups.1, voxel_groups.2);
            compute_to_compute_barrier(raw, cmd);

            // JFA propagate: step N/2 … 1, ping-ponging A/B by parity.
            let mut parity = 0i32;
            let last = steps.len().saturating_sub(1);
            for (i, &s) in steps.iter().enumerate() {
                let mut prop = *base_push;
                prop.misc = [s, parity, 0, 0];
                push(cmd, &prop);
                raw.cmd_dispatch(cmd, voxel_groups.0, voxel_groups.1, voxel_groups.2);
                if i < last {
                    compute_to_compute_barrier(raw, cmd);
                }
                parity ^= 1;
            }

            // The final jump-flood seed write → the readback transfer copy.
            compute_to_transfer_barrier(raw, cmd);
        }
    }
}

/// A global transfer→compute memory barrier: the geometry copies + the seed-key clear are
/// made visible to the bake's compute reads.
fn transfer_to_compute_barrier(raw: &ash::Device, cmd: vk::CommandBuffer) {
    let barrier = vk::MemoryBarrier2::default()
        .src_stage_mask(vk::PipelineStageFlags2::ALL_TRANSFER)
        .src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
        .dst_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER)
        .dst_access_mask(
            vk::AccessFlags2::SHADER_STORAGE_READ | vk::AccessFlags2::SHADER_STORAGE_WRITE,
        );
    let barriers = [barrier];
    let dep = vk::DependencyInfo::default().memory_barriers(&barriers);
    // SAFETY: the caller's recording contract.
    unsafe { raw.cmd_pipeline_barrier2(cmd, &dep) };
}

/// A global compute→compute memory barrier between successive bake dispatches (one pass's
/// storage-image writes visible to the next pass's reads).
fn compute_to_compute_barrier(raw: &ash::Device, cmd: vk::CommandBuffer) {
    let barrier = vk::MemoryBarrier2::default()
        .src_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER)
        .src_access_mask(vk::AccessFlags2::SHADER_STORAGE_WRITE)
        .dst_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER)
        .dst_access_mask(
            vk::AccessFlags2::SHADER_STORAGE_READ | vk::AccessFlags2::SHADER_STORAGE_WRITE,
        );
    let barriers = [barrier];
    let dep = vk::DependencyInfo::default().memory_barriers(&barriers);
    // SAFETY: the caller's recording contract.
    unsafe { raw.cmd_pipeline_barrier2(cmd, &dep) };
}

/// A global compute→transfer memory barrier: the final jump-flood seed write is made
/// visible to the readback copy.
fn compute_to_transfer_barrier(raw: &ash::Device, cmd: vk::CommandBuffer) {
    let barrier = vk::MemoryBarrier2::default()
        .src_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER)
        .src_access_mask(vk::AccessFlags2::SHADER_STORAGE_WRITE)
        .dst_stage_mask(vk::PipelineStageFlags2::ALL_TRANSFER)
        .dst_access_mask(vk::AccessFlags2::TRANSFER_READ);
    let barriers = [barrier];
    let dep = vk::DependencyInfo::default().memory_barriers(&barriers);
    // SAFETY: the caller's recording contract.
    unsafe { raw.cmd_pipeline_barrier2(cmd, &dep) };
}

/// Decodes the octahedral-packed unit normal the jump-flood seed carries in `w` (the bits
/// of an f32). Mirrors `packNormal`/`octDecode` in `sdf_jfa.slang` so the host sign pass
/// reads the same nearest-triangle normal the GPU stored.
fn unpack_oct_normal(packed: f32) -> Vec3 {
    let p = packed.to_bits();
    let ex = (p & 0xFFFF) as f32 / 65535.0 * 2.0 - 1.0;
    let ey = (p >> 16) as f32 / 65535.0 * 2.0 - 1.0;
    let mut n = Vec3::new(ex, ey, 1.0 - ex.abs() - ey.abs());
    let t = (-n.z).clamp(0.0, 1.0);
    n.x += if n.x >= 0.0 { -t } else { t };
    n.y += if n.y >= 0.0 { -t } else { t };
    n.normalize_or_zero()
}

/// Keeps the triangles of `tris` (global vertex indices into `positions`) whose AABB overlaps
/// `[lo, hi]`, remapping their vertices into a compact position list + local index list so an
/// SDF region bake uploads only the geometry that reaches it. A triangle straddling the bounds
/// is kept whole (AABB overlap is conservative), so a surface passing through the region is
/// never missed; a triangle within the encode clamp of a chunk's core is captured by that
/// chunk's padded grid, which is what keeps a `min`-over-instances sample exact across seams.
fn compact_tris_in_bounds(
    positions: &[Vec3],
    tris: &[u32],
    lo: Vec3,
    hi: Vec3,
) -> (Vec<Vec3>, Vec<u32>) {
    use std::collections::HashMap;
    let mut remap: HashMap<u32, u32> = HashMap::new();
    let mut out_pos: Vec<Vec3> = Vec::new();
    let mut out_idx: Vec<u32> = Vec::new();
    for t in tris.chunks_exact(3) {
        let (a, b, c) = (
            positions[t[0] as usize],
            positions[t[1] as usize],
            positions[t[2] as usize],
        );
        let tlo = a.min(b).min(c);
        let thi = a.max(b).max(c);
        if tlo.x > hi.x
            || thi.x < lo.x
            || tlo.y > hi.y
            || thi.y < lo.y
            || tlo.z > hi.z
            || thi.z < lo.z
        {
            continue;
        }
        for &g in t {
            let local = match remap.get(&g) {
                Some(&l) => l,
                None => {
                    let l = out_pos.len() as u32;
                    out_pos.push(positions[g as usize]);
                    remap.insert(g, l);
                    l
                }
            };
            out_idx.push(local);
        }
    }
    (out_pos, out_idx)
}

/// Resolves the per-voxel signed distance from the GPU jump-flood `seeds` (closest surface
/// point in `xyz`, octahedral-packed nearest-triangle normal in `w`, X-fastest over `grid`),
/// returning the `R16_SNORM` field the brick compactor consumes.
///
/// The magnitude is the unsigned distance `|center − closestPoint|`. The sign comes from a
/// 6-connected outside-flood seeded at the grid border, which stops at the surface band; a
/// voxel the flood reaches is positive, an enclosed voxel is negative, and a band voxel takes
/// the nearest-triangle normal's side (oriented by `winding`). The flood stays right at
/// concave corners, where a single face normal can flip an interior voxel to outside and leak
/// occlusion.
fn sign_field(grid: &GridDesc, winding: f32, seeds: &[[f32; 4]]) -> Vec<i16> {
    let [nx, ny, nz] = grid.dims;
    let (nx, ny, nz) = (nx as usize, ny as usize, nz as usize);
    let count = nx * ny * nz;
    let max_dist = grid.max_dist.max(1e-6);
    // A voxel whose center is within one cell diagonal of the surface joins the band the flood
    // must not cross, so "outside" cannot leak to "inside" through it. The band is generous: an
    // over-thick one only widens the region signed by the normal, which is correct there anyway.
    let band = grid.cell().length();
    const SENTINEL: f32 = 5e29;
    let idx = |x: usize, y: usize, z: usize| (z * ny + y) * nx + x;

    // Unsigned distance + surface-band ("solid") classification per voxel.
    let mut dist = vec![max_dist; count];
    let mut solid = vec![false; count];
    for z in 0..nz {
        for y in 0..ny {
            for x in 0..nx {
                let s = seeds[idx(x, y, z)];
                if s[0] >= SENTINEL {
                    continue; // unseeded → far outside
                }
                let center = grid.voxel_center(x as u32, y as u32, z as u32);
                let d = (center - Vec3::new(s[0], s[1], s[2])).length();
                dist[idx(x, y, z)] = d;
                solid[idx(x, y, z)] = d <= band;
            }
        }
    }

    // Outside-flood: a 6-connected BFS from every non-solid grid-border voxel.
    let mut outside = vec![false; count];
    let mut stack: Vec<(usize, usize, usize)> = Vec::new();
    for z in 0..nz {
        for y in 0..ny {
            for x in 0..nx {
                let border =
                    x == 0 || y == 0 || z == 0 || x == nx - 1 || y == ny - 1 || z == nz - 1;
                let i = idx(x, y, z);
                if border && !solid[i] && !outside[i] {
                    outside[i] = true;
                    stack.push((x, y, z));
                }
            }
        }
    }
    while let Some((x, y, z)) = stack.pop() {
        let mut visit = |nx_: usize, ny_: usize, nz_: usize, outside: &mut [bool]| {
            let j = idx(nx_, ny_, nz_);
            if !solid[j] && !outside[j] {
                outside[j] = true;
                stack.push((nx_, ny_, nz_));
            }
        };
        if x > 0 {
            visit(x - 1, y, z, &mut outside);
        }
        if x + 1 < nx {
            visit(x + 1, y, z, &mut outside);
        }
        if y > 0 {
            visit(x, y - 1, z, &mut outside);
        }
        if y + 1 < ny {
            visit(x, y + 1, z, &mut outside);
        }
        if z > 0 {
            visit(x, y, z - 1, &mut outside);
        }
        if z + 1 < nz {
            visit(x, y, z + 1, &mut outside);
        }
    }

    // Sign each voxel: flood-outside → positive; enclosed → negative; band → the normal.
    let mut field = vec![0i16; count];
    for z in 0..nz {
        for y in 0..ny {
            for x in 0..nx {
                let i = idx(x, y, z);
                let inside = if outside[i] {
                    false
                } else if solid[i] {
                    let s = seeds[i];
                    if s[0] >= SENTINEL {
                        false
                    } else {
                        let center = grid.voxel_center(x as u32, y as u32, z as u32);
                        let delta = center - Vec3::new(s[0], s[1], s[2]);
                        delta.dot(unpack_oct_normal(s[3])) * winding < 0.0
                    }
                } else {
                    true
                };
                let signed = if inside { -dist[i] } else { dist[i] };
                field[i] = ((signed / max_dist).clamp(-1.0, 1.0) * 32767.0).round() as i16;
            }
        }
    }
    field
}

/// An FNV-1a content hash of a mesh's geometry + bake resolution scale — the sidecar cache
/// key. A touched-but-unchanged mesh hashes identically and reuses its baked field.
fn mesh_content_hash(positions: &[Vec3], indices: &[u32], resolution_scale: f32) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    let mut fold = |bytes: &[u8]| {
        for &b in bytes {
            hash ^= u64::from(b);
            hash = hash.wrapping_mul(PRIME);
        }
    };
    for p in positions {
        fold(&p.x.to_le_bytes());
        fold(&p.y.to_le_bytes());
        fold(&p.z.to_le_bytes());
    }
    fold(bytemuck::cast_slice(indices));
    fold(&resolution_scale.to_le_bytes());
    hash
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::descriptors::Descriptors;
    use crate::resources::BindlessFreeList;
    use crate::upload::fixtures::device_or_skip;
    use crate::validation_issue_count;

    /// An axis-aligned box `[lo, hi]` as 12 outward-wound triangles (positions + indices),
    /// the small mesh the GPU bake test drives.
    fn box_geometry(lo: Vec3, hi: Vec3) -> (Vec<Vec3>, Vec<u32>) {
        let v = [
            Vec3::new(lo.x, lo.y, lo.z),
            Vec3::new(hi.x, lo.y, lo.z),
            Vec3::new(hi.x, hi.y, lo.z),
            Vec3::new(lo.x, hi.y, lo.z),
            Vec3::new(lo.x, lo.y, hi.z),
            Vec3::new(hi.x, lo.y, hi.z),
            Vec3::new(hi.x, hi.y, hi.z),
            Vec3::new(lo.x, hi.y, hi.z),
        ];
        let faces = [
            [0, 1, 2, 3],
            [5, 4, 7, 6],
            [4, 0, 3, 7],
            [1, 5, 6, 2],
            [4, 5, 1, 0],
            [3, 2, 6, 7],
        ];
        let mut positions = Vec::new();
        let mut indices = Vec::new();
        for f in faces {
            let base = positions.len() as u32;
            for &i in &f {
                positions.push(v[i]);
            }
            indices.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
        }
        (positions, indices)
    }

    /// A watertight concave L-shaped prism (an L cross-section extruded in z), built outward-
    /// wound from a rectangle decomposition of the caps + the six perimeter walls. Its
    /// re-entrant corner is the case a single nearest-face normal mis-signs but the outside-
    /// flood gets right — the fixture the box (convex) cannot exercise.
    fn l_prism_geometry() -> (Vec<Vec3>, Vec<u32>) {
        let mut positions: Vec<Vec3> = Vec::new();
        let mut indices: Vec<u32> = Vec::new();
        // Emit a planar quad a,b,c,d as two triangles, flipped so its normal faces `outward`.
        let mut quad = |a: Vec3, b: Vec3, c: Vec3, d: Vec3, outward: Vec3| {
            let n = (b - a).cross(c - a);
            let (a, b, c, d) = if n.dot(outward) >= 0.0 {
                (a, b, c, d)
            } else {
                (a, d, c, b)
            };
            let base = positions.len() as u32;
            positions.extend([a, b, c, d]);
            indices.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
        };
        // L cross-section (CCW), centred on the origin and scaled up so the arms span a few
        // voxels; the re-entrant vertex sits at (1, 1) before centring.
        let s = 2.0f32;
        let h = 2.0f32;
        let outline: [(f32, f32); 6] = [
            (0.0, 0.0),
            (2.0, 0.0),
            (2.0, 1.0),
            (1.0, 1.0),
            (1.0, 2.0),
            (0.0, 2.0),
        ];
        let xy = |p: (f32, f32)| Vec3::new((p.0 - 1.0) * s, (p.1 - 1.0) * s, 0.0);
        let z0 = -h * 0.5;
        let z1 = h * 0.5;
        let at = |p: (f32, f32), z: f32| {
            let v = xy(p);
            Vec3::new(v.x, v.y, z)
        };
        // Caps from the two-rectangle decomposition of the L (A: lower full width, B: upper
        // left arm). Bottom faces -z, top faces +z.
        let rects: [[(f32, f32); 4]; 2] = [
            [(0.0, 0.0), (2.0, 0.0), (2.0, 1.0), (0.0, 1.0)],
            [(0.0, 1.0), (1.0, 1.0), (1.0, 2.0), (0.0, 2.0)],
        ];
        for r in rects {
            quad(
                at(r[0], z0),
                at(r[1], z0),
                at(r[2], z0),
                at(r[3], z0),
                Vec3::NEG_Z,
            );
            quad(
                at(r[0], z1),
                at(r[1], z1),
                at(r[2], z1),
                at(r[3], z1),
                Vec3::Z,
            );
        }
        // Side walls: one quad per perimeter edge, outward = the edge rotated −90° in xy.
        for i in 0..outline.len() {
            let a = outline[i];
            let b = outline[(i + 1) % outline.len()];
            let edge = (b.0 - a.0, b.1 - a.1);
            let outward = Vec3::new(edge.1, -edge.0, 0.0).normalize_or_zero();
            quad(at(a, z0), at(b, z0), at(b, z1), at(a, z1), outward);
        }
        (positions, indices)
    }

    /// Asserts the baked field's sign matches the `MeshBvh` oracle's inside/outside at every
    /// voxel away from the surface band (where the discretized sign legitimately flutters),
    /// returning the number of voxels compared. The whole-grid sweep catches an edge/corner
    /// or concave-corner sign flip that a few interior samples would miss.
    fn assert_sign_matches_oracle(sdf: &Sdf, positions: &[Vec3], indices: &[u32]) -> usize {
        let oracle = saffron_geometry::MeshBvh::build(positions, indices).expect("oracle");
        let [nx, ny, nz] = sdf.header.dims;
        let bmin = Vec3::from(sdf.header.bounds_min);
        let bmax = Vec3::from(sdf.header.bounds_max);
        let cell = (bmax - bmin) / Vec3::new(nx as f32, ny as f32, nz as f32);
        let band = cell.length();
        let mut compared = 0usize;
        for z in 0..nz {
            for y in 0..ny {
                for x in 0..nx {
                    let center =
                        bmin + Vec3::new(x as f32 + 0.5, y as f32 + 0.5, z as f32 + 0.5) * cell;
                    let analytic = oracle.nearest_signed_distance(center);
                    if analytic.abs() < band {
                        continue;
                    }
                    let got = sdf.sample_voxel(x, y, z);
                    assert_eq!(
                        got < 0.0,
                        analytic < 0.0,
                        "voxel ({x},{y},{z}) sign mismatch: gpu {got}, oracle {analytic}"
                    );
                    compared += 1;
                }
            }
        }
        compared
    }

    /// The concave-corner sign gate: an L-shaped prism's re-entrant corner is exactly where a
    /// single nearest-face normal can read an interior voxel as outside (a sky-leak). The
    /// outside-flood sign must agree with the `MeshBvh` oracle's inside/outside across the
    /// whole grid, including the notch. Skips when no Vulkan device is present.
    #[test]
    fn gpu_bake_sdf_signs_concave_corner_like_the_oracle() {
        let Some(device) = device_or_skip() else {
            return;
        };
        let queue = device.graphics_queue.clone();
        let uploader = Uploader::new(&device, &queue).expect("Uploader::new");
        let (positions, indices) = l_prism_geometry();
        let fields = uploader
            .bake_sdf(&positions, &indices, &[], 1.0)
            .expect("gpu bake");
        // A small single-primitive mesh bakes exactly one field (below the chunk threshold).
        assert_eq!(fields.len(), 1, "a small mesh bakes one field");
        let sdf = &fields[0];
        assert!(sdf.header.occupied_bricks > 0, "the prism occupies bricks");
        let compared = assert_sign_matches_oracle(sdf, &positions, &indices);
        assert!(
            compared > 0,
            "no voxels were comparable away from the surface"
        );
        drop(uploader);
        device.wait_idle().expect("idle before teardown");
        drop(device);
    }

    /// A small mesh bakes through the GPU jump-flood (voxelize/JFA) validation-clean, the
    /// `GpuSdf` claims + reclaims one slot in both bindless arrays, and the baked field agrees
    /// in sign with the CPU `MeshBvh` oracle away from the surface band.
    #[test]
    fn gpu_bake_sdf_is_validation_clean_signs_correctly_and_reclaims_its_slot() {
        let Some(device) = device_or_skip() else {
            return;
        };
        let before = validation_issue_count();
        let free_list: BindlessFreeList = Arc::new(Mutex::new(Vec::new()));
        let descriptors = Descriptors::new(&device, &free_list).expect("Descriptors::new");
        let queue = device.graphics_queue.clone();
        let uploader = Uploader::new(&device, &queue).expect("Uploader::new");

        let (positions, indices) = box_geometry(Vec3::splat(-1.0), Vec3::splat(1.0));
        let fields = uploader
            .bake_sdf(&positions, &indices, &[], 1.0)
            .expect("gpu bake");
        assert_eq!(fields.len(), 1, "a small mesh bakes one field");
        let sdf = &fields[0];
        assert!(sdf.header.occupied_bricks > 0, "the box occupies bricks");

        // Sign gate: every voxel away from the surface band must match the CPU oracle's
        // inside/outside — not a handful of interior points, so an edge/corner sign flip is
        // caught (the box's exterior edge + corner voxels are sampled too).
        let compared = assert_sign_matches_oracle(sdf, &positions, &indices);
        assert!(
            compared > 0,
            "no voxels were comparable away from the surface"
        );

        let field = uploader.upload_sdf(&descriptors, sdf).expect("sdf upload");
        // The first SDF claims slot 0 of the SDF arrays (its own allocator, distinct from
        // the albedo array's reserved white slot 0).
        assert_eq!(field.bindless_index(), 0);
        assert_eq!(field.voxel_dims, sdf.header.dims);
        assert_eq!(field.indirection_dims, sdf.header.indirection_dims);
        assert_eq!(field.max_dist, sdf.header.max_dist);
        assert_eq!(field.mip_count, sdf.header.mip_count);
        assert_eq!(field.mip_count, saffron_geometry::SDF_MIP_COUNT);

        // A zero-dims field is rejected before any allocation.
        let mut empty = sdf.clone();
        empty.header.dims = [0, 0, 0];
        assert!(matches!(
            uploader.upload_sdf(&descriptors, &empty),
            Err(Error::ZeroSizedImage)
        ));

        // Dropping the field returns its slot to the shared SDF free-list (both arrays share
        // the one slot).
        let slot = field.bindless_index();
        drop(field);
        assert_eq!(free_list.lock().unwrap().as_slice(), &[] as &[u32]); // albedo free-list untouched
        assert_eq!(
            descriptors.sdf_free_list().lock().unwrap().as_slice(),
            &[slot]
        );

        drop(uploader);
        drop(descriptors);
        device.wait_idle().expect("idle before teardown");
        drop(device);

        let after = validation_issue_count();
        assert_eq!(
            before,
            after,
            "the SDF bake + upload must be validation-clean (saw {} new issue(s))",
            after.saturating_sub(before)
        );
    }

    /// The sidecar cache: a second `bake_or_load_sdf` of the same mesh reads the written
    /// `<hash>.sdfset` and returns fields byte-identical to the bake. Skips without a device.
    #[test]
    fn sdf_sidecar_cache_round_trips() {
        let Some(device) = device_or_skip() else {
            return;
        };
        let queue = device.graphics_queue.clone();
        let uploader = Uploader::new(&device, &queue).expect("Uploader::new");
        let dir = std::env::temp_dir().join(format!("saffron-sdf-cache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let bake = SdfBake {
            resolution_scale: 1.0,
            cache_dir: Some(dir.clone()),
        };
        let (positions, indices) = box_geometry(Vec3::splat(-0.5), Vec3::splat(0.5));

        let first = uploader
            .bake_or_load_sdf(&positions, &indices, &[], &bake)
            .expect("first bake writes the sidecar");
        // The sidecar exists now; the second call must read it back identically.
        let entries: Vec<_> = std::fs::read_dir(&dir)
            .expect("cache dir")
            .filter_map(std::result::Result::ok)
            .collect();
        assert_eq!(entries.len(), 1, "one sidecar written");
        let second = uploader
            .bake_or_load_sdf(&positions, &indices, &[], &bake)
            .expect("second load from sidecar");
        assert_eq!(
            first, second,
            "the cached fields round-trip byte-identically"
        );

        drop(uploader);
        device.wait_idle().expect("idle before teardown");
        drop(device);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
