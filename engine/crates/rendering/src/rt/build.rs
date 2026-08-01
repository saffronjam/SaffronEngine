//! Recording the acceleration-structure builds: the mesh and micromap batches, the skinned
//! refit ops, the TLAS plan, and the compaction pass, plus the instance packing they share.

use super::*;

/// One geometry of a refit structure: a slice of the index stream and the opacity class its
/// triangles carry.
#[derive(Clone, Copy)]
pub struct RefitGeometry {
    /// First index of the slice within the mesh's stream.
    pub(super) first_index: u32,
    /// Triangles the slice spans.
    pub(super) triangle_count: u32,
    /// Whether every triangle classifies opaque; a non-opaque geometry surfaces ray candidates
    /// for the coverage classifier.
    pub(super) opaque: bool,
}

/// One deforming BLAS refit recorded by [`record_tlas_build_plan`]: the AS to (re)build over a
/// device-address vertex stream and one geometry per submesh of the index stream, and whether it
/// is an in-place `UPDATE`.
pub struct BlasRefitOp {
    pub(super) dst: vk::AccelerationStructureKHR,
    pub(super) vertex_data: vk::DeviceAddress,
    pub(super) vertex_stride: vk::DeviceSize,
    pub(super) max_vertex: u32,
    pub(super) index_data: vk::DeviceAddress,
    pub(super) geometries: Vec<RefitGeometry>,
    pub(super) update: bool,
}

/// The TLAS build recorded by [`record_tlas_build_plan`]: the destination AS, the instance
/// array + scratch device addresses, and the instance count.
pub struct TlasBuildOp {
    pub(super) dst: vk::AccelerationStructureKHR,
    pub(super) instance_address: vk::DeviceAddress,
    pub(super) scratch_address: vk::DeviceAddress,
    pub(super) count: u32,
}

/// An owned, `Send + 'static` plan the `tlas-build` pass replays into its command buffer:
/// the skinned BLAS refits (sharing one scratch region), then the TLAS build, then the
/// AS-build → fragment ray-query barrier. Built by [`Rt::prepare_tlas_build`] (which did the
/// `&mut self` work); recording it only issues commands through resolved handles. It holds
/// the referenced `Arc<AccelerationStructure>`s so they outlive the recording.
pub struct TlasBuildPlan {
    pub(super) dispatch: accel::Device,
    pub(super) blas_ops: Vec<BlasRefitOp>,
    pub(super) blas_scratch_addr: vk::DeviceAddress,
    pub(super) top: TopLevelBuild,
    pub(super) _retained: Vec<crate::RtBlas>,
}

/// The frame's top-level build, in whichever form the device's structure takes. Which arm a
/// device uses is fixed at descriptor-layout creation, so a plan never mixes them.
pub(super) enum TopLevelBuild {
    /// The `VK_KHR_acceleration_structure` TLAS, rebuilt whole.
    Khr(TlasBuildOp),
    /// The partitioned structure, advanced by an op stream naming only what changed.
    Partitioned(crate::rt_ptlas::PtlasBuildOp),
}

// SAFETY: every field is an `Arc` / `Copy` handle / device address with no thread-affine
// state; the dispatch is a Clone fn-pointer table. The plan crosses into the `'static`
// graph closure, which runs on the render thread.
unsafe impl Send for TlasBuildPlan {}

/// Replays a [`TlasBuildPlan`] into `cmd`: each skinned BLAS refit serialized on the shared
/// scratch (AS-build → AS-build barrier between them), an AS-build → AS-build-read barrier
/// handing them to the TLAS build, the TLAS build itself, then the AS-build → fragment
/// ray-query barrier. The record half of the TLAS build. Issues commands only — no
/// resource creation, no `&mut self`.
pub fn record_tlas_build_plan(
    raw: &ash::Device,
    plan: &TlasBuildPlan,
    scopes: &mut crate::nested_scopes::NestedScopeRecorder<'_>,
) {
    // Two named child scopes rather than one pass timing. The graph already brackets the pass, so
    // the split costs nothing and answers the question the pass total cannot: whether a frame got
    // slower from refitting deformed geometry or from rebuilding the instance table.
    scopes.scope("blas-refit", |cmd| record_blas_refits(raw, cmd, plan));
    scopes.scope("tlas-build", |cmd| record_tlas_build(raw, cmd, plan));
}

/// The per-frame BLAS refits, split out so they carry their own timestamp scope.
///
/// Refits grow with deforming instances and the TLAS with total instance count, so one number
/// over the shared pass could not say which of them moved.
pub(super) fn record_blas_refits(raw: &ash::Device, cmd: vk::CommandBuffer, plan: &TlasBuildPlan) {
    let dispatch = &plan.dispatch;
    let scratch_barrier = accel_scratch_barrier();
    for (i, op) in plan.blas_ops.iter().enumerate() {
        if i > 0 {
            let dep = vk::DependencyInfo::default()
                .memory_barriers(std::slice::from_ref(&scratch_barrier));
            // SAFETY: the ash seam. A memory barrier on the active command buffer.
            unsafe { raw.cmd_pipeline_barrier2(cmd, &dep) };
        }
        let inputs: Vec<GeometryInputs<'_>> = op
            .geometries
            .iter()
            .map(|geometry| {
                GeometryInputs::new(
                    op.vertex_data,
                    op.vertex_stride,
                    op.max_vertex + 1,
                    op.index_data,
                    geometry.opaque,
                    None,
                )
            })
            .collect();
        let geoms: Vec<vk::AccelerationStructureGeometryKHR<'_>> =
            inputs.iter().map(GeometryInputs::geometry).collect();
        let build_info = vk::AccelerationStructureBuildGeometryInfoKHR::default()
            .ty(vk::AccelerationStructureTypeKHR::BOTTOM_LEVEL)
            .flags(
                vk::BuildAccelerationStructureFlagsKHR::PREFER_FAST_TRACE
                    | vk::BuildAccelerationStructureFlagsKHR::ALLOW_UPDATE,
            )
            .mode(if op.update {
                vk::BuildAccelerationStructureModeKHR::UPDATE
            } else {
                vk::BuildAccelerationStructureModeKHR::BUILD
            })
            .src_acceleration_structure(if op.update {
                op.dst
            } else {
                vk::AccelerationStructureKHR::null()
            })
            .dst_acceleration_structure(op.dst)
            .geometries(&geoms)
            .scratch_data(vk::DeviceOrHostAddressKHR {
                device_address: plan.blas_scratch_addr,
            });
        // A submesh owns a slice of the shared index stream, so its geometry starts at its own
        // first index. `primitive_offset` counts BYTES.
        let ranges: Vec<vk::AccelerationStructureBuildRangeInfoKHR> = op
            .geometries
            .iter()
            .map(|geometry| {
                vk::AccelerationStructureBuildRangeInfoKHR::default()
                    .primitive_count(geometry.triangle_count)
                    .primitive_offset(geometry.first_index * size_of::<u32>() as u32)
            })
            .collect();
        // SAFETY: the ash seam. One build info; the range slice length equals its
        // `geometry_count`. The vertex/index/scratch addresses reference live buffers.
        unsafe {
            dispatch.cmd_build_acceleration_structures(cmd, &[build_info], &[&ranges]);
        }
    }
    if !plan.blas_ops.is_empty() {
        // Hand the finished BLASes (build write) to the TLAS build (build read).
        let barrier = accel_build_to_build_read_barrier();
        let dep = vk::DependencyInfo::default().memory_barriers(std::slice::from_ref(&barrier));
        // SAFETY: the ash seam. A memory barrier on the active command buffer.
        unsafe { raw.cmd_pipeline_barrier2(cmd, &dep) };
    }
}

/// The top-level build over the packed instance buffer, plus the barrier handing it to ray queries.
pub(super) fn record_tlas_build(raw: &ash::Device, cmd: vk::CommandBuffer, plan: &TlasBuildPlan) {
    let tlas = match &plan.top {
        TopLevelBuild::Khr(tlas) => tlas,
        TopLevelBuild::Partitioned(ptlas) => {
            // SAFETY: the extension seam. The command buffer is recording and every address
            // in the plan references a live per-frame buffer of the structure that produced
            // it (the frame's fence was waited before the slot was reused).
            unsafe { ptlas.record(cmd) };
            let barrier = accel_build_to_fragment_barrier();
            let dep = vk::DependencyInfo::default().memory_barriers(std::slice::from_ref(&barrier));
            // SAFETY: the ash seam. A memory barrier on the active command buffer.
            unsafe { raw.cmd_pipeline_barrier2(cmd, &dep) };
            return;
        }
    };
    let dispatch = &plan.dispatch;
    let geom = instances_geometry(tlas.instance_address);
    let geoms = [geom];
    let build_info = vk::AccelerationStructureBuildGeometryInfoKHR::default()
        .ty(vk::AccelerationStructureTypeKHR::TOP_LEVEL)
        .flags(vk::BuildAccelerationStructureFlagsKHR::PREFER_FAST_BUILD)
        .mode(vk::BuildAccelerationStructureModeKHR::BUILD)
        .dst_acceleration_structure(tlas.dst)
        .geometries(&geoms)
        .scratch_data(vk::DeviceOrHostAddressKHR {
            device_address: tlas.scratch_address,
        });
    let range = vk::AccelerationStructureBuildRangeInfoKHR::default().primitive_count(tlas.count);
    let ranges = [range];
    // SAFETY: the ash seam. One build info; the range slice length equals its
    // `geometry_count` (1).
    unsafe {
        dispatch.cmd_build_acceleration_structures(cmd, &[build_info], &[&ranges]);
    }

    // AS build (write) → fragment ray-query (read).
    let barrier = accel_build_to_fragment_barrier();
    let dep = vk::DependencyInfo::default().memory_barriers(std::slice::from_ref(&barrier));
    // SAFETY: the ash seam. A memory barrier on the active command buffer.
    unsafe { raw.cmd_pipeline_barrier2(cmd, &dep) };
}

/// One placed instance, in the form both top-level structures derive from.
pub(super) struct Placement {
    pub(super) key: crate::rt_ptlas::PtlasKey,
    pub(super) rows: [f32; 12],
    /// The GPU-scene instance slot this placement resolves candidates against, or
    /// [`RT_UNMIRRORED_INSTANCE`].
    pub(super) instance_slot: u32,
    /// The referenced structure's geometry-0 submesh element within the prototype geometry.
    pub(super) first_submesh: u32,
    pub(super) opacity: vk::GeometryInstanceFlagsKHR,
    pub(super) blas: crate::RtBlas,
}

/// One structure over minted topology: the key its per-frame rebuild is cached under, the
/// transient arena slice its geometry occupies, and where the structure sits in the world.
///
/// An amplified instance and a materialized micro-field tile are the same thing from the builder's
/// side — variable topology every frame, no submesh naming any of it — so both arrive here.
pub(super) struct GeneratedRtGeometry {
    pub(super) key: u64,
    pub(super) slice: crate::TessRtSlice,
    pub(super) world_transform: Mat4,
}

/// A generated-topology structure the frame planned a build for, and the transform its placement
/// carries.
///
/// The planner hands these back so the top level places exactly what it planned to rebuild: a
/// structure held over from an earlier frame at the same slot holds that frame's topology, and
/// placing it in a frame whose build was skipped would trace geometry the arena no longer
/// describes.
pub(super) struct PlannedGeneratedBlas {
    pub(super) key: u64,
    pub(super) world_transform: Mat4,
    pub(super) accel: Arc<AccelerationStructure>,
}

/// One placement's identity record: the scene slot and submesh span it resolves candidates
/// against, plus the cluster tables when its structure is cluster-composed.
pub(super) fn ray_instance_record(placement: &Placement) -> GpuRayInstanceRecord {
    let (cluster_records, cluster_corners, cluster_count) =
        placement.blas.cluster_resolution().unwrap_or((0, 0, 0));
    GpuRayInstanceRecord {
        instance_slot: placement.instance_slot,
        first_submesh: placement.first_submesh,
        cluster_count,
        reserved: 0,
        cluster_records,
        cluster_corners,
    }
}

/// Placements whose identity names a live GPU-scene slot: a non-opaque candidate on one of these
/// reaches the coverage classifier, where an unmirrored placement commits unconditionally.
pub(super) fn resolvable_placements(placements: &[Placement]) -> u32 {
    placements
        .iter()
        .filter(|placement| placement.instance_slot != RT_UNMIRRORED_INSTANCE)
        .count() as u32
}

/// The geometries a deforming instance's refit structure covers: one per submesh of its run,
/// each with its own slice of the mesh's index stream and its cooked opacity class.
pub(super) fn refit_geometries(inst: &DeformedRtInstance) -> Vec<RefitGeometry> {
    let first = inst.first_submesh as usize;
    let end = first.saturating_add(inst.submesh_count as usize);
    let Some(submeshes) = inst
        .mesh
        .submeshes
        .get(first..end.min(inst.mesh.submeshes.len()))
    else {
        return Vec::new();
    };
    submeshes
        .iter()
        .enumerate()
        .filter(|(_, submesh)| submesh.index_count >= 3)
        .map(|(offset, submesh)| RefitGeometry {
            first_index: submesh.first_index,
            triangle_count: submesh.index_count / 3,
            opaque: inst
                .mesh
                .submesh_opaque
                .get(first + offset)
                .copied()
                .unwrap_or(false),
        })
        .collect()
}

/// Marks a deforming instance's key, whose primary is an entity rather than a scene slot.
pub(super) const DEFORMED_PRIMARY_BASE: u64 = 1 << 62;

/// Marks a materialized wind use's refit-BLAS key, which names a scene slot and a use ordinal
/// rather than an entity — vegetation never enters the ECS, so it has no entity id to key on.
pub(crate) const WIND_BLAS_KEY_BASE: u64 = 1 << 63;

/// The stable refit-BLAS key of one materialized use: the same slot and ordinal every frame, so
/// the structure is built once and refit in place afterwards.
pub(crate) fn wind_blas_key(instance_slot: u32, use_index: u32) -> u64 {
    WIND_BLAS_KEY_BASE | (u64::from(instance_slot) << 20) | u64::from(use_index & 0xF_FFFF)
}

/// Marks a static instance the GPU scene does not mirror, whose primary is its position in
/// the gather rather than a stable slot.
pub(super) const UNMIRRORED_PRIMARY_BASE: u64 = 1 << 61;

/// The stable half of a static placement's key.
///
/// A mirrored instance keys on its GPU-scene slot, which is stable for as long as the
/// instance exists — the property the partitioned diff rests on. An unmirrored one has no
/// such identity and falls back to its position in the gather; if that order shifts, its
/// placements are rewritten rather than left alone, which costs incrementality and not
/// correctness.
pub(super) fn placement_primary(instance_slot: u32, scene_index: usize) -> u64 {
    if instance_slot < RT_UNMIRRORED_INSTANCE {
        u64::from(instance_slot)
    } else {
        UNMIRRORED_PRIMARY_BASE | scene_index as u64
    }
}

/// Everything the top-level plan needs beyond the placements themselves: the dispatch it
/// records through, the bottom-level work it carries, and the per-frame counts it reports.
pub(super) struct PlanContext {
    pub(super) dispatch: accel::Device,
    pub(super) blas_ops: Vec<BlasRefitOp>,
    pub(super) count: u32,
    pub(super) aggregate_count: u32,
    pub(super) skinned_op_count: u32,
    pub(super) tess_op_count: u32,
}

/// One built BLAS + its build scratch — the upload-time mesh BLAS, returned so the caller
/// (the [`crate::Uploader`]) keeps the scratch alive until its one-off submit completes.
pub struct MeshBlasBuild {
    /// The built bottom-level acceleration structure (shared from the mesh).
    pub blas: AccelerationStructure,
    /// The build scratch — cleared by the caller once the build submit completes.
    pub scratch: Option<Buffer>,
    /// The size the build reserved, before compaction (BLAS telemetry).
    pub built_size: vk::DeviceSize,
}

/// The buffers and index range one bottom-level structure covers. A plain mesh passes its
/// whole stream; an assembly prototype passes the slice it owns.
#[derive(Clone, Copy)]
pub struct MeshBlasGeometry<'a> {
    /// The micromap refining this geometry's coverage, when one was derived.
    pub micromap: Option<&'a Micromap>,
    /// Whether every triangle in this slice classifies opaque. A non-opaque geometry surfaces
    /// ray candidates for the coverage classifier, and is the only kind a micromap refines.
    pub opaque: bool,
    /// The device-local vertex buffer.
    pub vertex_buffer: vk::Buffer,
    /// Vertices in the buffer (the build's `max_vertex` bound).
    pub vertex_count: u32,
    /// The device-local index buffer.
    pub index_buffer: vk::Buffer,
    /// First index of this structure's slice.
    pub first_index: u32,
    /// Indices in the slice.
    pub index_count: u32,
}

/// Records a BLAS build over `geometries` into `cmd` and returns the AS + scratch.
///
/// One geometry per material-homogeneous submesh, each with its own opacity flag. That
/// granularity is the point: a BLAS built as a single geometry can only carry one opacity class,
/// so one masked submesh forces the coverage classifier onto every other submesh in the mesh — and
/// an opacity micromap, which refines a single geometry's coverage, has nothing to attach to.
///
/// Built [`mesh_blas_build_flags`]. The caller submits `cmd` and waits, then drops the returned
/// [`MeshBlasBuild::scratch`]. The mesh's vertex + index buffers must carry
/// `SHADER_DEVICE_ADDRESS` + AS-build-input usage.
///
/// # Errors
///
/// Returns [`crate::Error::Vk`] if the AS or scratch buffer cannot be created, or
/// [`crate::Error::Message`] if `geometries` is empty.
pub fn record_mesh_blas_build(
    resources: &Arc<DeviceResources>,
    dispatch: &accel::Device,
    cmd: vk::CommandBuffer,
    geometries: &[MeshBlasGeometry<'_>],
    omm_supported: bool,
) -> Result<MeshBlasBuild> {
    if geometries.is_empty() {
        return Err(crate::Error::EmptyMesh);
    }
    let vertex_stride = size_of::<Vertex>() as vk::DeviceSize;
    // Each micromap chain is boxed so its address is stable for the whole build: `push_next`
    // stores a RAW POINTER into the triangles descriptor, so growing a `Vec` of these in place
    // would leave every already-chained geometry pointing at freed memory.
    let mut omm_chains: Vec<Option<Box<vk::AccelerationStructureTrianglesOpacityMicromapEXT<'_>>>> =
        geometries
            .iter()
            .map(|geometry| {
                geometry.micromap.map(|micromap| {
                    Box::new(
                        vk::AccelerationStructureTrianglesOpacityMicromapEXT::default()
                            .index_type(vk::IndexType::UINT32)
                            .index_buffer(vk::DeviceOrHostAddressConstKHR {
                                device_address: micromap.index_address(),
                            })
                            .index_stride(size_of::<i32>() as vk::DeviceSize)
                            .base_triangle(0)
                            .usage_counts(micromap.usage())
                            .micromap(micromap.handle()),
                    )
                })
            })
            .collect();
    let inputs: Vec<GeometryInputs<'_>> = geometries
        .iter()
        .zip(omm_chains.iter_mut())
        .map(|(geometry, chain)| {
            GeometryInputs::new(
                resources.buffer_device_address(geometry.vertex_buffer),
                vertex_stride,
                geometry.vertex_count,
                resources.buffer_device_address(geometry.index_buffer),
                geometry.opaque,
                chain.as_deref_mut(),
            )
        })
        .collect();
    let geoms: Vec<vk::AccelerationStructureGeometryKHR<'_>> =
        inputs.iter().map(GeometryInputs::geometry).collect();
    let triangle_counts: Vec<u32> = geometries
        .iter()
        .map(|geometry| geometry.index_count / 3)
        .collect();
    let size_info = vk::AccelerationStructureBuildGeometryInfoKHR::default()
        .ty(vk::AccelerationStructureTypeKHR::BOTTOM_LEVEL)
        .flags(mesh_blas_build_flags(omm_supported))
        .mode(vk::BuildAccelerationStructureModeKHR::BUILD)
        .geometries(&geoms);
    let mut sizes = vk::AccelerationStructureBuildSizesInfoKHR::default();
    // SAFETY: the ash seam. `geometry_count == max_primitive_counts.len()`, both `geometries.len()`.
    unsafe {
        dispatch.get_acceleration_structure_build_sizes(
            vk::AccelerationStructureBuildTypeKHR::DEVICE,
            &size_info,
            &triangle_counts,
            &mut sizes,
        );
    }

    let blas = AccelerationStructure::create(
        resources,
        dispatch,
        sizes.acceleration_structure_size,
        vk::AccelerationStructureTypeKHR::BOTTOM_LEVEL,
    )?;
    let scratch = make_scratch_buffer(resources, sizes.build_scratch_size)?;
    let scratch_addr = resources.buffer_device_address(scratch.handle());

    let build_info = vk::AccelerationStructureBuildGeometryInfoKHR::default()
        .ty(vk::AccelerationStructureTypeKHR::BOTTOM_LEVEL)
        .flags(mesh_blas_build_flags(omm_supported))
        .mode(vk::BuildAccelerationStructureModeKHR::BUILD)
        .dst_acceleration_structure(blas.handle())
        .geometries(&geoms)
        .scratch_data(vk::DeviceOrHostAddressKHR {
            device_address: scratch_addr,
        });
    // A submesh owns a slice of the shared index stream, so its build starts at its own first
    // index rather than at zero. `primitive_offset` counts BYTES.
    let ranges: Vec<vk::AccelerationStructureBuildRangeInfoKHR> = geometries
        .iter()
        .zip(&triangle_counts)
        .map(|(geometry, &triangle_count)| {
            vk::AccelerationStructureBuildRangeInfoKHR::default()
                .primitive_count(triangle_count)
                .primitive_offset(geometry.first_index * size_of::<u32>() as u32)
        })
        .collect();
    // SAFETY: the ash seam. One build info; the range slice length equals its `geometry_count`.
    // The vertex/index addresses are valid for the device lifetime.
    unsafe {
        dispatch.cmd_build_acceleration_structures(cmd, &[build_info], &[&ranges]);
    }
    Ok(MeshBlasBuild {
        blas,
        scratch: Some(scratch),
        built_size: sizes.acceleration_structure_size,
    })
}

/// Records a micromap build for `derived` into `cmd` and returns the structure plus its build
/// scratch, which the caller drops once the submit completes.
///
/// The state data, the per-triangle block descriptors, and the per-triangle index stream are
/// uploaded to device-local buffers first: a micromap build reads all three by device address,
/// exactly as an acceleration-structure build reads vertices and indices.
///
/// # Errors
///
/// Returns [`crate::Error::Vk`] if any buffer or the micromap cannot be created.
pub fn record_micromap_build(
    resources: &Arc<DeviceResources>,
    dispatch: &ash::ext::opacity_micromap::Device,
    cmd: vk::CommandBuffer,
    derived: &saffron_geometry::OpacityMicromapBuild,
) -> Result<(Micromap, Buffer, Buffer, Buffer)> {
    let usage: Vec<vk::MicromapUsageEXT> = derived
        .usage
        .iter()
        .map(|row| {
            vk::MicromapUsageEXT::default()
                .count(row.count)
                .subdivision_level(row.subdivision_level)
                .format(row.format)
        })
        .collect();

    let input_usage = vk::BufferUsageFlags::MICROMAP_BUILD_INPUT_READ_ONLY_EXT
        | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS;
    let data = Buffer::from_slice_with_usage(resources, &derived.data, input_usage)?;
    // `VkMicromapTriangleEXT` is `{u32 dataOffset, u16 subdivisionLevel, u16 format}` — eight
    // bytes, which is also the minimum stride the build accepts.
    let triangle_bytes: Vec<u8> = derived
        .blocks
        .iter()
        .flat_map(|block| {
            let mut row = [0_u8; 8];
            row[0..4].copy_from_slice(&block.data_offset.to_ne_bytes());
            row[4..6].copy_from_slice(&block.subdivision_level.to_ne_bytes());
            row[6..8].copy_from_slice(&block.format.to_ne_bytes());
            row
        })
        .collect();
    let triangles = Buffer::from_slice_with_usage(resources, &triangle_bytes, input_usage)?;
    let index_bytes: Vec<u8> = derived
        .indices
        .iter()
        .flat_map(|index| index.to_ne_bytes())
        .collect();
    let indices = Buffer::from_slice_with_usage(
        resources,
        &index_bytes,
        vk::BufferUsageFlags::ACCELERATION_STRUCTURE_BUILD_INPUT_READ_ONLY_KHR
            | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS,
    )?;

    let mut sizes = vk::MicromapBuildSizesInfoEXT::default();
    let size_info = vk::MicromapBuildInfoEXT::default()
        .ty(vk::MicromapTypeEXT::OPACITY_MICROMAP)
        .mode(vk::BuildMicromapModeEXT::BUILD)
        .usage_counts(&usage);
    // SAFETY: the ash seam, through the raw table. The usage rows outlive the call.
    unsafe {
        (dispatch.fp().get_micromap_build_sizes_ext)(
            dispatch.device(),
            vk::AccelerationStructureBuildTypeKHR::DEVICE,
            &size_info,
            &mut sizes,
        );
    }

    let micromap = Micromap::create(
        resources,
        dispatch,
        sizes.micromap_size,
        indices,
        usage.clone(),
        (
            derived.classes.opaque,
            derived.classes.transparent,
            derived.classes.unknown,
        ),
    )?;
    let scratch = make_scratch_buffer(resources, sizes.build_scratch_size.max(1))?;

    let build_info = vk::MicromapBuildInfoEXT::default()
        .ty(vk::MicromapTypeEXT::OPACITY_MICROMAP)
        .mode(vk::BuildMicromapModeEXT::BUILD)
        .dst_micromap(micromap.handle())
        .usage_counts(&usage)
        .data(vk::DeviceOrHostAddressConstKHR {
            device_address: resources.buffer_device_address(data.handle()),
        })
        .scratch_data(vk::DeviceOrHostAddressKHR {
            device_address: resources.buffer_device_address(scratch.handle()),
        })
        .triangle_array(vk::DeviceOrHostAddressConstKHR {
            device_address: resources.buffer_device_address(triangles.handle()),
        })
        .triangle_array_stride(8);
    // SAFETY: the ash seam. One build info; every referenced buffer outlives the submit the
    // caller waits on.
    unsafe {
        (dispatch.fp().cmd_build_micromaps_ext)(cmd, 1, &build_info);
    }
    Ok((micromap, data, triangles, scratch))
}

/// Instance flags for an entity's opacity decision.
///
/// `None` leaves the geometry's own per-submesh flags in charge, which is what a micromap needs:
/// a per-instance `FORCE_OPAQUE`/`FORCE_NO_OPAQUE` overrides an attached micromap outright per
/// spec, so forcing unconditionally would make every micromap inert. `Some` additionally disables
/// the micromap, because a micromap derived for the cooked material describes coverage this
/// instance's material does not have.
pub(super) fn instance_opacity_flags(
    opacity_override: Option<bool>,
) -> vk::GeometryInstanceFlagsKHR {
    match opacity_override {
        None => vk::GeometryInstanceFlagsKHR::empty(),
        Some(true) => {
            vk::GeometryInstanceFlagsKHR::FORCE_OPAQUE
                | vk::GeometryInstanceFlagsKHR::DISABLE_OPACITY_MICROMAPS_EXT
        }
        Some(false) => {
            vk::GeometryInstanceFlagsKHR::FORCE_NO_OPAQUE
                | vk::GeometryInstanceFlagsKHR::DISABLE_OPACITY_MICROMAPS_EXT
        }
    }
}

/// The build flags every per-mesh BLAS uses. `ALLOW_COMPACTION` is what makes the
/// compacted-size query legal, and compaction is not optional here: a static mesh's
/// structure lives for the whole session, so the memory the build over-reserves is held
/// for the whole session too.
///
/// `ALLOW_DISABLE_OPACITY_MICROMAPS_EXT` is a build-time permission rather than a runtime choice:
/// an instance may only carry `DISABLE_OPACITY_MICROMAPS_EXT` if the structure it references was
/// built allowing it, and an entity binding a material that disagrees with the cooked class needs
/// exactly that bit. It is gated on the device advertising `VK_EXT_opacity_micromap` — the flag is
/// not merely useless without the extension, it is an invalid enum value, so a software adapter
/// that never sees a micromap would fail the build outright.
pub(super) fn mesh_blas_build_flags(omm_supported: bool) -> vk::BuildAccelerationStructureFlagsKHR {
    let base = vk::BuildAccelerationStructureFlagsKHR::PREFER_FAST_TRACE
        | vk::BuildAccelerationStructureFlagsKHR::ALLOW_COMPACTION;
    if omm_supported {
        base | vk::BuildAccelerationStructureFlagsKHR::ALLOW_DISABLE_OPACITY_MICROMAPS_EXT
    } else {
        base
    }
}

/// Records the compacted copy of `source` into a freshly sized structure and returns it.
/// The caller must have waited the build submit and read `compacted_size` from a
/// `ACCELERATION_STRUCTURE_COMPACTED_SIZE_KHR` query.
///
/// # Errors
///
/// Returns [`crate::Error::Vk`] if the destination structure cannot be created.
pub fn record_blas_compaction(
    resources: &Arc<DeviceResources>,
    dispatch: &accel::Device,
    cmd: vk::CommandBuffer,
    source: &AccelerationStructure,
    compacted_size: vk::DeviceSize,
) -> Result<AccelerationStructure> {
    let mut compacted = AccelerationStructure::create(
        resources,
        dispatch,
        compacted_size,
        vk::AccelerationStructureTypeKHR::BOTTOM_LEVEL,
    )?;
    compacted.note_compacted_from(source.size());
    let copy = vk::CopyAccelerationStructureInfoKHR::default()
        .src(source.handle())
        .dst(compacted.handle())
        .mode(vk::CopyAccelerationStructureModeKHR::COMPACT);
    // SAFETY: the ash seam. Both structures are live; the source was built with
    // `ALLOW_COMPACTION` and its build has completed on the device.
    unsafe { dispatch.cmd_copy_acceleration_structure(cmd, &copy) };
    Ok(compacted)
}

/// Whether a cached tessellated BLAS whose backing store fits `cached_prims` worst-case triangles can be
/// reused for a build wanting `wanted_prims`. A tessellated BLAS is a full `MODE_BUILD` every frame, so
/// the *contents* never persist — only the AS backing-store *capacity* does. Reuse exactly when the
/// worst-case bound is unchanged; a bound change (a new factor cap / LOD) forces a fresh, larger AS.
/// `None` (no cached AS) is never reusable.
pub(super) fn tess_blas_reuse(cached_prims: Option<u32>, wanted_prims: u32) -> bool {
    cached_prims == Some(wanted_prims)
}

/// Whether the skinned-refit planner skips an instance: a degenerate / untracked instance (no
/// geometry, no triangle, or entity 0), or a **tessellated** one — the latter takes the full-BUILD
/// path (`plan_generated_blas_builds`) because variable topology forbids the in-place `UPDATE` the
/// refit relies on. This is the sole discriminator between the two BLAS paths.
pub(super) fn skinned_refit_skips(
    vertex_count: u32,
    index_count: u32,
    entity: u64,
    tessellated: bool,
) -> bool {
    vertex_count == 0 || index_count < 3 || entity == 0 || tessellated
}

/// A triangle-geometry descriptor over a device-address vertex + index stream
/// (`R32G32B32_SFLOAT` positions, `UINT32` indices, opaque). The vertex/index addresses
/// must reference live buffers for the build's duration.
/// Owns everything a triangles geometry descriptor borrows: the triangles struct, the optional
/// micromap chain, and the usage rows that chain points at.
///
/// `push_next` stores a raw pointer, so these must outlive the descriptor built from them —
/// which is why this is a struct rather than a function returning `<'static>`.
pub(super) struct GeometryInputs<'a> {
    pub(super) triangles: vk::AccelerationStructureGeometryTrianglesDataKHR<'a>,
    pub(super) opaque: bool,
}

impl<'a> GeometryInputs<'a> {
    /// Builds the inputs, chaining `micromap` onto the triangles when one is supplied.
    pub(super) fn new(
        vertex_data: vk::DeviceAddress,
        vertex_stride: vk::DeviceSize,
        vertex_count: u32,
        index_data: vk::DeviceAddress,
        opaque: bool,
        omm: Option<&'a mut vk::AccelerationStructureTrianglesOpacityMicromapEXT<'a>>,
    ) -> Self {
        let mut triangles = vk::AccelerationStructureGeometryTrianglesDataKHR::default()
            .vertex_format(vk::Format::R32G32B32_SFLOAT)
            .vertex_data(vk::DeviceOrHostAddressConstKHR {
                device_address: vertex_data,
            })
            .vertex_stride(vertex_stride)
            .max_vertex(vertex_count.saturating_sub(1))
            .index_type(vk::IndexType::UINT32)
            .index_data(vk::DeviceOrHostAddressConstKHR {
                device_address: index_data,
            });
        if let Some(omm) = omm {
            triangles = triangles.push_next(omm);
        }
        Self { triangles, opaque }
    }

    /// The descriptor, borrowing this value.
    pub(super) fn geometry(&self) -> vk::AccelerationStructureGeometryKHR<'_> {
        // Opacity lives on the geometry, not the instance. An instance-level
        // `FORCE_OPAQUE`/`FORCE_NO_OPAQUE` overrides any micromap outright per spec, so a
        // micromap under instance-level opacity would be inert.
        let flags = if self.opaque {
            vk::GeometryFlagsKHR::OPAQUE
        } else {
            vk::GeometryFlagsKHR::empty()
        };
        vk::AccelerationStructureGeometryKHR::default()
            .geometry_type(vk::GeometryTypeKHR::TRIANGLES)
            .flags(flags)
            .geometry(vk::AccelerationStructureGeometryDataKHR {
                triangles: self.triangles,
            })
    }
}

/// An instances-geometry descriptor over a device-address instance array (the TLAS input).
pub(super) fn instances_geometry(
    instance_data: vk::DeviceAddress,
) -> vk::AccelerationStructureGeometryKHR<'static> {
    let instances = vk::AccelerationStructureGeometryInstancesDataKHR::default()
        .array_of_pointers(false)
        .data(vk::DeviceOrHostAddressConstKHR {
            device_address: instance_data,
        });
    vk::AccelerationStructureGeometryKHR::default()
        .geometry_type(vk::GeometryTypeKHR::INSTANCES)
        .flags(vk::GeometryFlagsKHR::OPAQUE)
        .geometry(vk::AccelerationStructureGeometryDataKHR { instances })
}

/// The row-major 3×4 transform of an identity placement (a skinned instance: its deformed
/// vertices are already world-space). The placement loop derives every instance's rows from
/// `transform_rows(&inst.world_transform)`, so this is the source-of-truth constant the
/// byte-identity test pins `transform_rows(&Mat4::IDENTITY)` against.
#[cfg(test)]
pub(super) const IDENTITY_ROWS: [f32; 12] = [
    1.0, 0.0, 0.0, 0.0, //
    0.0, 1.0, 0.0, 0.0, //
    0.0, 0.0, 1.0, 0.0,
];

/// Transposes a column-major [`Mat4`] world transform into the row-major 3×4
/// `VkTransformMatrixKHR` layout (12 floats, row 0 first).
pub(super) fn transform_rows(model: &Mat4) -> [f32; 12] {
    let m = model.to_cols_array_2d();
    let mut rows = [0.0_f32; 12];
    for r in 0..3 {
        for c in 0..4 {
            rows[r * 4 + c] = m[c][r];
        }
    }
    rows
}

/// The AS-storage bytes the distinct structures in `retained` occupy, as
/// `(current, as_built)`. The second figure is what the same set would occupy had no
/// compaction copy run, so their difference is the saving compaction realized.
///
/// Deduplicates by device address: one mesh's structure appears once per instance that
/// references it, and charging it each time would report sharing as growth.
pub(super) fn distinct_blas_bytes(retained: &[crate::RtBlas]) -> (u64, u64) {
    let mut seen: Vec<vk::DeviceAddress> = Vec::new();
    let mut current = 0;
    let mut as_built = 0;
    for structure in retained {
        if seen.contains(&structure.address()) {
            continue;
        }
        seen.push(structure.address());
        current += structure.size();
        as_built += structure.built_size();
    }
    (current, as_built)
}

/// `(structures, composed CLAS)` across the distinct cluster-composed bottom levels in
/// `retained`, deduplicated by device address for the same sharing reason as the byte sums.
pub(super) fn distinct_cluster_blas(retained: &[crate::RtBlas]) -> (u32, u32) {
    let mut seen: Vec<vk::DeviceAddress> = Vec::new();
    let mut clusters = 0;
    for structure in retained {
        let crate::RtBlas::Cluster(blas) = structure else {
            continue;
        };
        if seen.contains(&blas.address()) {
            continue;
        }
        seen.push(blas.address());
        clusters += blas.cluster_count();
    }
    (seen.len() as u32, clusters)
}

/// A use record's family-local transform as a matrix. The record stores rows 0-2 of the
/// row-major 3×4; the implicit last row is `[0, 0, 0, 1]`.
pub(super) fn assembly_use_matrix(use_record: &crate::GpuAssemblyUseRecord) -> Mat4 {
    let t = &use_record.transform;
    Mat4::from_cols_array(&[
        t[0], t[4], t[8], 0.0, //
        t[1], t[5], t[9], 0.0, //
        t[2], t[6], t[10], 0.0, //
        t[3], t[7], t[11], 1.0,
    ])
}

/// The number of distinct bottom-level structures a packed instance array references.
/// Instances of one mesh share a BLAS, so this is the count of unique AS device addresses.
pub(super) fn distinct_blas_count(retained: &[crate::RtBlas]) -> u32 {
    let mut seen: Vec<vk::DeviceAddress> = Vec::new();
    for structure in retained {
        if !seen.contains(&structure.address()) {
            seen.push(structure.address());
        }
    }
    seen.len() as u32
}

/// Packs one `VkAccelerationStructureInstanceKHR`: a row-major 3×4 transform, the custom
/// index, a 0xFF mask, the triangle-cull-disable flag, and the referenced AS device address.
pub(super) fn make_instance(
    rows: [f32; 12],
    custom_index: u32,
    opacity: vk::GeometryInstanceFlagsKHR,
    accel_reference: vk::DeviceAddress,
) -> vk::AccelerationStructureInstanceKHR {
    vk::AccelerationStructureInstanceKHR {
        transform: vk::TransformMatrixKHR { matrix: rows },
        instance_custom_index_and_mask: vk::Packed24_8::new(custom_index, 0xFF),
        instance_shader_binding_table_record_offset_and_flags: vk::Packed24_8::new(
            0,
            (vk::GeometryInstanceFlagsKHR::TRIANGLE_FACING_CULL_DISABLE | opacity).as_raw() as u8,
        ),
        acceleration_structure_reference: vk::AccelerationStructureReferenceKHR {
            device_handle: accel_reference,
        },
    }
}

/// A device-local AS build/refit scratch buffer (`STORAGE | SHADER_DEVICE_ADDRESS`),
/// allocated at the device's `minAccelerationStructureScratchOffsetAlignment` — every
/// scratch address here is a buffer base address, and a misaligned one loses the device.
pub(super) fn make_scratch_buffer(
    resources: &Arc<DeviceResources>,
    bytes: vk::DeviceSize,
) -> Result<Buffer> {
    let alloc_info = vk_mem::AllocationCreateInfo {
        usage: vk_mem::MemoryUsage::AutoPreferDevice,
        ..Default::default()
    };
    Buffer::with_alignment(
        resources,
        bytes.max(256),
        vk::BufferUsageFlags::STORAGE_BUFFER | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS,
        &alloc_info,
        resources.scratch_alignment(),
    )
}

/// The shared-scratch reuse barrier: serialize consecutive AS builds sharing one scratch
/// region (build write/read → build write/read).
pub(super) fn accel_scratch_barrier() -> vk::MemoryBarrier2<'static> {
    vk::MemoryBarrier2::default()
        .src_stage_mask(vk::PipelineStageFlags2::ACCELERATION_STRUCTURE_BUILD_KHR)
        .src_access_mask(
            vk::AccessFlags2::ACCELERATION_STRUCTURE_WRITE_KHR
                | vk::AccessFlags2::ACCELERATION_STRUCTURE_READ_KHR,
        )
        .dst_stage_mask(vk::PipelineStageFlags2::ACCELERATION_STRUCTURE_BUILD_KHR)
        .dst_access_mask(
            vk::AccessFlags2::ACCELERATION_STRUCTURE_WRITE_KHR
                | vk::AccessFlags2::ACCELERATION_STRUCTURE_READ_KHR,
        )
}

/// The BLAS-refit → TLAS-build barrier: the refit writes (build stage) feed the TLAS build
/// that reads them as input (build stage).
pub(super) fn accel_build_to_build_read_barrier() -> vk::MemoryBarrier2<'static> {
    vk::MemoryBarrier2::default()
        .src_stage_mask(vk::PipelineStageFlags2::ACCELERATION_STRUCTURE_BUILD_KHR)
        .src_access_mask(vk::AccessFlags2::ACCELERATION_STRUCTURE_WRITE_KHR)
        .dst_stage_mask(vk::PipelineStageFlags2::ACCELERATION_STRUCTURE_BUILD_KHR)
        .dst_access_mask(vk::AccessFlags2::ACCELERATION_STRUCTURE_READ_KHR)
}

/// The TLAS-build → fragment-ray-query barrier: the AS build write feeds the fragment
/// shader's inline ray-query read.
pub(super) fn accel_build_to_fragment_barrier() -> vk::MemoryBarrier2<'static> {
    vk::MemoryBarrier2::default()
        .src_stage_mask(vk::PipelineStageFlags2::ACCELERATION_STRUCTURE_BUILD_KHR)
        .src_access_mask(vk::AccessFlags2::ACCELERATION_STRUCTURE_WRITE_KHR)
        .dst_stage_mask(vk::PipelineStageFlags2::FRAGMENT_SHADER)
        .dst_access_mask(vk::AccessFlags2::ACCELERATION_STRUCTURE_READ_KHR)
}

/// Allocates a transient command buffer, records `record`, submits it, and blocks on a
/// fresh fence — the init-time one-off path for the empty-TLAS seed (a private pool keeps it
/// self-contained, never touching a per-frame pool).
pub(super) fn record_and_submit_oneoff<R: FnOnce(vk::CommandBuffer)>(
    device: &Device,
    record: R,
) -> Result<()> {
    let raw = device.raw();
    let pool_info = vk::CommandPoolCreateInfo::default()
        .flags(vk::CommandPoolCreateFlags::TRANSIENT)
        .queue_family_index(device.graphics_queue_family);
    // SAFETY: the ash seam. The pool is created, used, and destroyed within this call.
    let pool = checked(
        unsafe { raw.create_command_pool(&pool_info, None) },
        "create_command_pool (seed tlas)",
    )?;
    let result = (|| -> Result<()> {
        let alloc_info = vk::CommandBufferAllocateInfo::default()
            .command_pool(pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        // SAFETY: the ash seam. One primary buffer from the private pool.
        let cmd = checked(
            unsafe { raw.allocate_command_buffers(&alloc_info) },
            "allocate_command_buffers (seed tlas)",
        )?[0];
        let begin = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
        // SAFETY: the ash seam. Begin/record/end on the freshly allocated buffer.
        checked(
            unsafe { raw.begin_command_buffer(cmd, &begin) },
            "begin_command_buffer (seed tlas)",
        )?;
        record(cmd);
        // SAFETY: the ash seam. Ends the recording opened above.
        checked(
            unsafe { raw.end_command_buffer(cmd) },
            "end_command_buffer (seed tlas)",
        )?;
        let cmd_infos = [vk::CommandBufferSubmitInfo::default().command_buffer(cmd)];
        let submits = [vk::SubmitInfo2::default().command_buffer_infos(&cmd_infos)];
        // SAFETY: the ash seam. The graphics queue is idle at init (no frame in flight);
        // submit without a fence and drain with `wait_idle` below (an init path).
        device.graphics_queue.submit2(
            raw,
            &submits,
            vk::Fence::null(),
            "queue_submit2 (seed tlas)",
        )?;
        device.wait_idle()
    })();
    // SAFETY: the ash seam. The queue was idled (or the submit never happened), so the pool
    // and its buffer are idle and destroyed exactly once.
    unsafe { raw.destroy_command_pool(pool, None) };
    result
}

/// Counts the distinct micromaps the frame's instances reference and sums what they settled.
///
/// Deduplicated by micromap handle for the same reason the BLAS bytes are: instances of one mesh
/// share its structures, and counting a micromap once per instance would report sharing as work.
pub(super) fn distinct_micromap_classes(instances: &[RtInstanceInput]) -> (u32, (u64, u64, u64)) {
    let mut seen = std::collections::BTreeSet::new();
    let mut classes = (0_u64, 0_u64, 0_u64);
    for input in instances {
        for micromap in &input.mesh.micromaps {
            if !seen.insert(micromap.handle()) {
                continue;
            }
            let (opaque, transparent, unknown) = micromap.classes();
            classes.0 += opaque;
            classes.1 += transparent;
            classes.2 += unknown;
        }
    }
    (u32::try_from(seen.len()).unwrap_or(u32::MAX), classes)
}
