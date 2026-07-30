use std::sync::Arc;

use ash::vk;
use saffron_geometry::glam::Vec3;
use saffron_geometry::{Mesh, PortableVirtualHierarchy, Vertex};

use super::Uploader;
use super::hierarchy::cooked_micromap_builds;
use crate::resources::Buffer;
use crate::{Device, Error, Result, checked};

/// A two-timestamp pool for the out-of-graph structure submits, when the device can time them.
///
/// `None` on a device whose graphics queue reports no timestamp bits — the builds still run, they
/// are simply not measured.
pub(super) fn accel_timestamp_pool(device: &Device) -> Option<(vk::QueryPool, f32)> {
    let facts = device.profiler_facts();
    if device.accel_dispatch().is_none()
        || !facts.timestamps_supported
        || facts.timestamp_period <= 0.0
    {
        return None;
    }
    let info = vk::QueryPoolCreateInfo::default()
        .query_type(vk::QueryType::TIMESTAMP)
        .query_count(2);
    // SAFETY: the ash seam. The create-info is valid; the pool is owned and freed in `Drop`.
    let pool = unsafe { device.raw().create_query_pool(&info, None) }.ok()?;
    Some((pool, facts.timestamp_period))
}

impl Uploader {
    /// Builds every cooked micromap the device can accept, keyed by submesh.
    ///
    /// A failure is logged, not fatal: the family then renders with the coverage classifier doing
    /// the work the micromap would have removed — the same picture at a higher cost.
    pub(super) fn build_cooked_micromaps(
        &self,
        hierarchy: &PortableVirtualHierarchy,
    ) -> Vec<(u32, Arc<crate::Micromap>)> {
        // `SAFFRON_OMM=off` suppresses attachment: a micromap may only remove classifier work,
        // so the two must render the same picture, which is only testable if it can be turned off.
        if std::env::var("SAFFRON_OMM").is_ok_and(|value| value == "off") {
            return Vec::new();
        }
        let Some(dispatch) = self.omm.as_ref() else {
            return Vec::new();
        };
        let builds = cooked_micromap_builds(hierarchy, self.omm_max_subdivision);
        let mut built = Vec::with_capacity(builds.len());
        for (submesh, build) in &builds {
            // The staging buffers the build reads by address must outlive the submit, so they are
            // held across `with_one_off_commands` and dropped only after it returns.
            let mut retained = None;
            let recorded = self.with_one_off_commands_timed("micromap build", true, |cmd| {
                match crate::record_micromap_build(&self.resources, dispatch, cmd, build) {
                    Ok((micromap, data, triangles, indices)) => {
                        retained = Some((micromap, data, triangles, indices));
                    }
                    Err(err) => tracing::warn!("micromap build failed: {err}"),
                }
            });
            match (recorded, retained) {
                (Ok(()), Some((micromap, ..))) => built.push((*submesh, Arc::new(micromap))),
                (Err(err), _) => tracing::warn!("micromap submit failed: {err}"),
                (Ok(()), None) => {}
            }
        }
        built
    }

    /// Builds the aggregate-representation BLAS: one structure over the root cut's
    /// voxel-brick surfaces, in family space, returned with the largest root
    /// appearance-error total — the number TLAS packing projects to choose it. Returns
    /// `None` when RT is off, any root is not a voxel brick (a triangle root already
    /// draws the fine geometry, so nothing coarser stands in), or the surfaces are empty.
    ///
    /// The build inputs are fresh host-visible buffers read by device address during the
    /// synchronous build; the finished structure does not reference them, so they drop
    /// on return.
    pub(super) fn build_aggregate_blas(
        &self,
        hierarchy: &PortableVirtualHierarchy,
    ) -> Result<Option<(Arc<crate::AccelerationStructure>, u32)>> {
        if self.accel.is_none() || hierarchy.roots.is_empty() {
            return Ok(None);
        }
        let mut vertices: Vec<Vertex> = Vec::new();
        let mut indices: Vec<u32> = Vec::new();
        let mut error_total = 0_u32;
        for &root in &hierarchy.roots {
            let Some(node) = hierarchy.nodes.get(root as usize) else {
                return Ok(None);
            };
            let saffron_geometry::HierarchyRepresentation::Voxel { brick } = node.representation
            else {
                return Ok(None);
            };
            let Some(brick) = hierarchy.voxel_bricks.get(brick as usize) else {
                return Ok(None);
            };
            let base = vertices.len() as u32;
            vertices.extend(brick.vertices.iter().map(|vertex| Vertex {
                position: Vec3::new(
                    vertex.position_bits[0] as f32 / 65_536.0,
                    vertex.position_bits[1] as f32 / 65_536.0,
                    vertex.position_bits[2] as f32 / 65_536.0,
                ),
                ..Vertex::default()
            }));
            indices.extend(brick.indices.iter().map(|index| base + index));
            error_total = error_total.max(node.appearance_error.total);
        }
        if indices.len() < 3 {
            return Ok(None);
        }
        let usage = vk::BufferUsageFlags::ACCELERATION_STRUCTURE_BUILD_INPUT_READ_ONLY_KHR
            | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS;
        let vertex_buffer =
            Buffer::from_slice_with_usage(&self.resources, bytemuck::cast_slice(&vertices), usage)?;
        let index_buffer =
            Buffer::from_slice_with_usage(&self.resources, bytemuck::cast_slice(&indices), usage)?;
        let geometry = crate::MeshBlasGeometry {
            micromap: None,
            // An aggregate merges its sources' coverage into solid occupancy, so its
            // triangles commit like the raster brick surface shades: no per-hit classifier.
            opaque: true,
            vertex_buffer: vertex_buffer.handle(),
            vertex_count: vertices.len() as u32,
            index_buffer: index_buffer.handle(),
            first_index: 0,
            index_count: indices.len() as u32,
        };
        Ok(self
            .build_mesh_blas(&[geometry])?
            .map(|blas| (blas, error_total)))
    }

    /// Composes one assembly prototype's bottom-level structure from its cooked triangle
    /// clusters on a cluster-AS device. `Ok(None)` when the device has no cluster support,
    /// the prototype's clusters do not exactly cover its fine index range (a partial
    /// structure would trace geometry the raster never draws), or a cluster exceeds the
    /// device's cluster limits — the caller then takes the KHR triangle build.
    pub(super) fn build_prototype_cluster_blas(
        &self,
        hierarchy: &PortableVirtualHierarchy,
        prototype: u32,
        span: std::ops::Range<usize>,
        mesh: &Mesh,
        submesh_opaque: &[bool],
    ) -> Result<Option<crate::rt_cluster::ClusterBlas>> {
        let Some(builder) = self.cluster.as_ref() else {
            return Ok(None);
        };
        let clusters: Vec<_> = hierarchy
            .triangle_clusters
            .iter()
            .filter(|cluster| cluster.prototype == prototype)
            .collect();
        if clusters.is_empty() {
            return Ok(None);
        }
        let span_indices: u64 = span
            .clone()
            .filter_map(|submesh| mesh.submeshes.get(submesh))
            .map(|submesh| u64::from(submesh.index_count))
            .sum();
        let cluster_indices: u64 = clusters
            .iter()
            .map(|cluster| cluster.local_indices.len() as u64)
            .sum();
        if span_indices != cluster_indices {
            return Ok(None);
        }
        let vertex_base: u64 = hierarchy
            .prototypes
            .iter()
            .take(prototype as usize)
            .map(|prototype| u64::from(prototype.vertex_count))
            .sum();
        let mut inputs = Vec::with_capacity(clusters.len());
        for cluster in &clusters {
            let mut positions = Vec::with_capacity(cluster.source_vertices.len());
            for source in &cluster.source_vertices {
                let index = usize::try_from(vertex_base + u64::from(*source)).map_err(|_| {
                    Error::InvalidUploadData("cluster vertex index exceeds usize".to_owned())
                })?;
                let Some(vertex) = mesh.vertices.get(index) else {
                    return Ok(None);
                };
                positions.push([vertex.position.x, vertex.position.y, vertex.position.z]);
            }
            inputs.push(crate::rt_cluster::ClusterBuildInput {
                cluster_id: cluster.id,
                geometry_index: cluster.source_submesh,
                opaque: submesh_opaque
                    .get(span.start + cluster.source_submesh as usize)
                    .copied()
                    .unwrap_or(false),
                positions,
                local_indices: cluster.local_indices.clone(),
            });
        }
        let Some(plan) = builder.plan(&self.resources, &inputs)? else {
            return Ok(None);
        };
        self.with_one_off_commands_timed("cluster_blas_build", true, |cmd| {
            builder.record(&self.resources, cmd, &plan);
        })?;
        Ok(Some(builder.finish(plan)?))
    }

    pub(super) fn build_mesh_blas(
        &self,
        geometries: &[crate::MeshBlasGeometry<'_>],
    ) -> Result<Option<Arc<crate::AccelerationStructure>>> {
        let index_count: u32 = geometries.iter().map(|geometry| geometry.index_count).sum();
        let Some(dispatch) = self.accel.as_ref() else {
            return Ok(None);
        };
        if index_count < 3 {
            return Ok(None);
        }
        let raw = self.raw();
        let alloc_info = vk::CommandBufferAllocateInfo::default()
            .command_pool(self.command_pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        // SAFETY: the ash seam. One primary buffer from this uploader's own pool.
        let cmd = checked(
            unsafe { raw.allocate_command_buffers(&alloc_info) },
            "allocate_command_buffers (blas)",
        )?[0];
        let begin = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
        let built = (|| -> Result<crate::MeshBlasBuild> {
            // SAFETY: the ash seam. Begin the freshly allocated buffer.
            checked(
                unsafe { raw.begin_command_buffer(cmd, &begin) },
                "begin_command_buffer (blas)",
            )?;
            let timing = self.accel_timestamps.as_ref();
            if let Some((pool, _)) = timing {
                // SAFETY: the ash seam. Resets both queries and opens the span on this buffer.
                unsafe {
                    raw.cmd_reset_query_pool(cmd, *pool, 0, 2);
                    raw.cmd_write_timestamp2(cmd, vk::PipelineStageFlags2::TOP_OF_PIPE, *pool, 0);
                }
            }
            let build = crate::record_mesh_blas_build(
                &self.resources,
                dispatch,
                cmd,
                geometries,
                self.omm_supported,
            )?;
            if let Some((pool, _)) = timing {
                // SAFETY: the ash seam. Closes the span opened above on the same buffer.
                unsafe {
                    raw.cmd_write_timestamp2(
                        cmd,
                        vk::PipelineStageFlags2::BOTTOM_OF_PIPE,
                        *pool,
                        1,
                    );
                }
            }
            // SAFETY: the ash seam. Ends the recording opened above.
            checked(
                unsafe { raw.end_command_buffer(cmd) },
                "end_command_buffer (blas)",
            )?;
            self.submit_and_wait(cmd)?;
            self.accumulate_accel_time();
            Ok(build)
        })();
        // SAFETY: the ash seam. The submit was waited (or never happened), so the buffer is
        // idle and freed exactly once.
        unsafe { raw.free_command_buffers(self.command_pool, &[cmd]) };
        // The build submit completed, so the scratch can go.
        let mut build = built?;
        build.scratch = None;
        // A driver that declines to shrink keeps the built structure — compaction is a memory
        // win, never a correctness precondition.
        let blas = match self.compact_mesh_blas(dispatch, &build)? {
            Some(compacted) => compacted,
            None => build.blas,
        };
        Ok(Some(Arc::new(blas)))
    }

    /// Compacts a freshly built mesh BLAS: reads its compacted size from a device query and
    /// copies it into an exactly-sized structure. Returns `None` when the driver reports no
    /// saving, leaving the caller its built structure.
    ///
    /// A static mesh's structure lives for the whole session, so the slack the build reserves
    /// would be held for the whole session too.
    fn compact_mesh_blas(
        &self,
        dispatch: &ash::khr::acceleration_structure::Device,
        build: &crate::MeshBlasBuild,
    ) -> Result<Option<crate::AccelerationStructure>> {
        let raw = self.raw();
        let pool_info = vk::QueryPoolCreateInfo::default()
            .query_type(vk::QueryType::ACCELERATION_STRUCTURE_COMPACTED_SIZE_KHR)
            .query_count(1);
        // SAFETY: the ash seam. The pool is destroyed below on every path.
        let pool = checked(
            unsafe { raw.create_query_pool(&pool_info, None) },
            "create_query_pool (blas compaction)",
        )?;
        let compacted = (|| -> Result<Option<crate::AccelerationStructure>> {
            let structures = [build.blas.handle()];
            self.with_one_off_commands_timed("blas_compacted_size", true, |cmd| {
                // SAFETY: the ash seam. The pool is reset before the write, and the build
                // completed in the previous submit.
                unsafe {
                    raw.cmd_reset_query_pool(cmd, pool, 0, 1);
                    dispatch.cmd_write_acceleration_structures_properties(
                        cmd,
                        &structures,
                        vk::QueryType::ACCELERATION_STRUCTURE_COMPACTED_SIZE_KHR,
                        pool,
                        0,
                    );
                }
            })?;
            let mut sizes = [0u64; 1];
            // SAFETY: the ash seam. The query was written by the submit just waited.
            checked(
                unsafe {
                    raw.get_query_pool_results(
                        pool,
                        0,
                        &mut sizes,
                        vk::QueryResultFlags::TYPE_64 | vk::QueryResultFlags::WAIT,
                    )
                },
                "get_query_pool_results (blas compaction)",
            )?;
            // A driver may decline to shrink; copying into a same-or-larger structure would
            // only cost memory, so keep the built one.
            if sizes[0] == 0 || sizes[0] >= build.built_size {
                return Ok(None);
            }
            let mut result = None;
            self.with_one_off_commands_timed("blas_compact", true, |cmd| {
                result = Some(crate::record_blas_compaction(
                    &self.resources,
                    dispatch,
                    cmd,
                    &build.blas,
                    sizes[0],
                ));
            })?;
            result.expect("the compaction recorder ran").map(Some)
        })();
        // SAFETY: the ash seam. Every submit above was waited, so the pool is idle.
        unsafe { raw.destroy_query_pool(pool, None) };
        compacted
    }
}
