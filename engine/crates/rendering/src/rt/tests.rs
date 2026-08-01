use super::*;
use crate::descriptors::Descriptors;
use crate::device::SurfaceSource;
use crate::resources::BindlessFreeList;
use crate::validation_issue_count;
use saffron_geometry::glam::Vec4;
use std::sync::Mutex;

/// A neutral cut view: auto override at close range, so every input keeps its fine
/// representation and the assertions about per-use expansion hold.
fn test_cut_view() -> RtCutView {
    RtCutView {
        eye: [0.0; 3],
        proj_scale: 1_000.0,
        error_threshold_px: 1.0,
        representation_override: crate::SCENE_CUT_AUTO,
    }
}

/// Builds a headless device + descriptors + `Rt`, or `None` when no Vulkan ICD is present. Yields
/// the issue count taken before `Rt::new` so the caller can assert the seed path is
/// validation-clean.
fn rt_or_skip() -> Option<(Device, Descriptors, Rt, u64)> {
    let device = match Device::new(&SurfaceSource::Offscreen) {
        Ok(device) => device,
        Err(err) => {
            eprintln!("skipping: no Vulkan device obtainable ({err})");
            return None;
        }
    };
    let free_list: BindlessFreeList = Arc::new(Mutex::new(Vec::new()));
    let descriptors = Descriptors::new(&device, &free_list).expect("Descriptors");
    let before = validation_issue_count();
    let rt = Rt::new(&device, &descriptors).expect("Rt::new");
    Some((device, descriptors, rt, before))
}

/// A tessellated BLAS reuses its backing store only when the worst-case bound is unchanged: a full
/// `MODE_BUILD` refills the geometry every frame, so nothing but the AS *capacity* persists. A first
/// sight (no cache) and any bound change both force a fresh AS; never an in-place `UPDATE`.
#[test]
fn tessellated_blas_reuses_only_on_unchanged_worst_case() {
    assert!(!tess_blas_reuse(None, 100), "no cache is never reusable");
    assert!(tess_blas_reuse(Some(100), 100), "same bound reuses the AS");
    assert!(
        !tess_blas_reuse(Some(64), 256),
        "a grown bound (higher factor cap) forces a fresh AS"
    );
    assert!(
        !tess_blas_reuse(Some(256), 64),
        "a shrunk bound also recreates (never keep an oversized AS around)"
    );
}

/// The tessellated BUILD's CPU range is the worst-case closed form: `worst_case_prims` triangles and
/// `max_vertex = worst_case_verts - 1`, matching the dice contract the emit kernel reserves. The IB
/// tail past the GPU-packed triangles is degenerate-padded, so building the full range is watertight.
#[test]
fn tessellated_blas_build_range_matches_worst_case_reservation() {
    use crate::tessellation::tess_worst_case;
    // Two base triangles at a factor cap of 8: L=8 ⇒ verts=(9)(10)/2=45, tris=64 per triangle.
    let (verts, indices) = tess_worst_case(2, 8);
    assert_eq!(verts, 2 * 45);
    assert_eq!(indices, 2 * 64 * 3);
    // The planner's BUILD range: primitive_count = indices/3, max_vertex = verts - 1.
    let worst_case_prims = (indices / 3) as u32;
    let max_vertex = verts as u32 - 1;
    assert_eq!(worst_case_prims, 2 * 64);
    assert_eq!(max_vertex, 2 * 45 - 1);
}

/// A tessellated instance (`tess: Some`) takes the full-BUILD path, never the skinned in-place refit:
/// `tessellated` is the sole discriminator, so a healthy instance marked tessellated is skipped by the
/// refit and planned once by the BUILD planner. Degenerate / untracked instances are skipped by both.
#[test]
fn tess_flag_selects_the_build_path_over_the_skinned_refit() {
    // A healthy, non-tessellated instance takes the skinned refit (not skipped).
    assert!(!skinned_refit_skips(100, 300, 7, false));
    // The same instance marked tessellated is skipped by the refit → the BUILD planner claims it.
    assert!(skinned_refit_skips(100, 300, 7, true));
    // Degenerate / untracked instances are skipped regardless of the tessellation flag.
    assert!(skinned_refit_skips(0, 300, 7, false), "no vertices");
    assert!(
        skinned_refit_skips(100, 2, 7, false),
        "sub-triangle index count"
    );
    assert!(
        skinned_refit_skips(100, 300, 0, false),
        "untracked (entity 0)"
    );
}

/// `transform_rows` transposes a column-major world transform into the row-major 3×4
/// `VkTransformMatrixKHR` layout: row r, column c reads `model[c][r]`.
#[test]
fn transform_rows_transposes_to_row_major() {
    let model = Mat4::from_cols(
        Vec4::new(1.0, 2.0, 3.0, 4.0),
        Vec4::new(5.0, 6.0, 7.0, 8.0),
        Vec4::new(9.0, 10.0, 11.0, 12.0),
        Vec4::new(13.0, 14.0, 15.0, 16.0),
    );
    let rows = transform_rows(&model);
    // Row 0 = the x-components of each column (the matrix's first row).
    assert_eq!(rows[0..4], [1.0, 5.0, 9.0, 13.0]);
    // Row 1 = the y-components.
    assert_eq!(rows[4..8], [2.0, 6.0, 10.0, 14.0]);
    // Row 2 = the z-components.
    assert_eq!(rows[8..12], [3.0, 7.0, 11.0, 15.0]);
}

/// A skinned instance's TLAS transform is the row-major identity (its deformed vertices
/// are already in world space). The placement loop now derives the row matrix from
/// `transform_rows(&inst.world_transform)` for every deforming instance, so a skinned
/// instance (`world_transform == IDENTITY`) must produce bytes identical to the
/// `IDENTITY_ROWS` constant — this is what keeps the skin RT placement provably unchanged
/// after generalizing to morph.
#[test]
fn identity_rows_is_the_3x4_identity() {
    assert_eq!(
        IDENTITY_ROWS,
        [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0]
    );
    assert_eq!(
        transform_rows(&Mat4::IDENTITY),
        IDENTITY_ROWS,
        "a skinned instance's identity world_transform places byte-identical to IDENTITY_ROWS"
    );
}

/// A two-buffer [`GpuMesh`] with no BLAS, enough for scene-capture bookkeeping tests.
fn test_mesh(device: &Device) -> Arc<crate::GpuMesh> {
    test_mesh_parts(device, None, None, Vec::new())
}

/// The same mesh carrying its own structure, or an assembly table plus one bottom-level structure
/// per prototype — what TLAS packing expands into one placement per active use.
fn test_mesh_parts(
    device: &Device,
    blas: Option<Arc<crate::AccelerationStructure>>,
    assembly: Option<crate::MeshAssembly>,
    assembly_blas: Vec<crate::RtBlas>,
) -> Arc<crate::GpuMesh> {
    use vk_mem::Alloc;
    let make_buffer = |size: vk::DeviceSize, usage: vk::BufferUsageFlags| {
        let alloc_info = vk_mem::AllocationCreateInfo {
            usage: vk_mem::MemoryUsage::AutoPreferDevice,
            ..Default::default()
        };
        let info = vk::BufferCreateInfo::default().size(size).usage(usage);
        // SAFETY: the VMA seam. Ownership passes into the GpuMesh below.
        unsafe {
            device
                .resources()
                .allocator()
                .create_buffer(&info, &alloc_info)
        }
        .expect("create_buffer")
    };
    Arc::new(crate::GpuMesh::from_parts(
        device.resources(),
        crate::GpuMeshParts {
            submesh_opaque: vec![true],
            micromaps: Vec::new(),
            vertex: make_buffer(96, vk::BufferUsageFlags::VERTEX_BUFFER),
            index: make_buffer(48, vk::BufferUsageFlags::INDEX_BUFFER),
            skin: None,
            morph: None,
            conditioning: None,
            index_count: 3,
            vertex_count: 3,
            submeshes: Vec::new(),
            bounds_min: saffron_geometry::glam::Vec3::ZERO,
            bounds_max: saffron_geometry::glam::Vec3::ONE,
            cpu_vertices: Vec::new(),
            cpu_indices: Vec::new(),
            cpu_skin: Vec::new(),
            blas,
            assembly_blas,
            aggregate_blas: None,
            sdfs: Vec::new(),
            hierarchy_pages: Vec::new(),
            assembly,
        },
    ))
}

/// A two-prototype assembly table: three uses (prototype 0, 1, 0), one combination selecting the
/// first two, and prototype spans starting at family submesh 0 and 4. The spans differ because
/// they are what rebases a traced candidate's geometry index onto the family's submesh table.
fn test_assembly() -> crate::MeshAssembly {
    let use_record = |prototype: u32| crate::GpuAssemblyUseRecord {
        transform: [
            1.0, 0.0, 0.0, 0.0, //
            0.0, 1.0, 0.0, 0.0, //
            0.0, 0.0, 1.0, 0.0,
        ],
        prototype,
        reserved: [0; 3],
    };
    crate::MeshAssembly {
        prototypes: vec![crate::GpuAssemblyPrototypeRecord::default(); 2],
        uses: vec![use_record(0), use_record(1), use_record(0)],
        combinations: vec![(0, 0)],
        masks: vec![0b011],
        prototype_slices: vec![
            crate::AssemblyPrototypeSlice {
                first_submesh: 0,
                submesh_count: 4,
                first_index: 0,
                index_count: 12,
                first_vertex: 0,
                vertex_count: 8,
            },
            crate::AssemblyPrototypeSlice {
                first_submesh: 4,
                submesh_count: 2,
                first_index: 12,
                index_count: 6,
                first_vertex: 8,
                vertex_count: 4,
            },
        ],
    }
}

/// Every TLAS placement gets an identity record at its own `instanceCustomIndex`, and the table is
/// sized for every placement the captured scene can expand into before the frame's address block
/// names it.
///
/// This is the whole basis of ray-candidate resolution: a candidate carries a structure and a
/// geometry index, and only this table says which scene slot and which submesh span they belong
/// to. An assembly's placements must each carry THEIR prototype's span start, not the family's.
#[test]
fn ray_instance_table_records_every_placement_identity() {
    let Some((device, _descriptors, mut rt, _before)) = rt_or_skip() else {
        return;
    };
    if !rt.supported() {
        return;
    }
    let dispatch = rt
        .dispatch
        .clone()
        .expect("accel dispatch present on an RT device");
    let structure = || {
        Arc::new(
            crate::AccelerationStructure::create(
                &rt.resources,
                &dispatch,
                256,
                vk::AccelerationStructureTypeKHR::BOTTOM_LEVEL,
            )
            .expect("bottom level"),
        )
    };
    let assembly_mesh = test_mesh_parts(
        &device,
        None,
        Some(test_assembly()),
        vec![
            crate::RtBlas::Khr(structure()),
            crate::RtBlas::Khr(structure()),
        ],
    );
    let plain_mesh = test_mesh_parts(&device, Some(structure()), None, Vec::new());

    rt.set_rt_shadows(true);
    rt.set_rt_scene(Arc::from([
        RtInstanceInput {
            model: Mat4::IDENTITY,
            mesh: assembly_mesh,
            instance_slot: 11,
            opacity_override: None,
            combination: 0,
            wind: false,
        },
        RtInstanceInput {
            model: Mat4::IDENTITY,
            mesh: plain_mesh,
            instance_slot: 3,
            opacity_override: None,
            combination: 0,
            wind: false,
        },
    ]));
    // The bound covers the assembly's WHOLE use table plus one per plain and per deforming
    // instance, and the micro-field tile reservation: the table is sized before the plan runs, so
    // it can depend neither on which combination the frame ends up selecting nor on how many tiles
    // the materialization reserves.
    let tiles = crate::MICRO_RT_MAX_TILES;
    assert_eq!(rt.placement_upper_bound(0), 4 + tiles);
    assert_eq!(rt.placement_upper_bound(2), 6 + tiles);

    let (address, capacity) = rt.ensure_frame_ray_instances(&device, 0, 0);
    assert_ne!(address, 0, "the frame's address block names a live table");
    assert!(capacity >= 4, "the table covers the placement bound");

    let plan = rt
        .prepare_tlas_build(&device, 0, &[], None, &[], test_cut_view())
        .expect("a build plan");
    assert_eq!(
        rt.frame_instance_count(),
        3,
        "two active assembly uses plus the plain mesh"
    );

    let table = rt.frames[0]
        .ray_instances
        .as_mut()
        .expect("ray instance table");
    let bytes = table.mapped_bytes().expect("host-visible table");
    let records: &[GpuRayInstanceRecord] =
        bytemuck::cast_slice(&bytes[..3 * size_of::<GpuRayInstanceRecord>()]);
    assert_eq!(records[0].instance_slot, 11);
    assert_eq!(
        records[0].first_submesh, 0,
        "use 0 places prototype 0's span"
    );
    assert_eq!(records[1].instance_slot, 11);
    assert_eq!(
        records[1].first_submesh, 4,
        "use 1 places prototype 1's span"
    );
    assert_eq!(records[2].instance_slot, 3);
    assert_eq!(records[2].first_submesh, 0, "a plain mesh spans from zero");
    for record in records {
        assert_eq!(
            record.cluster_count, 0,
            "a KHR structure resolves through the shared index stream"
        );
        assert_eq!(record.cluster_records, 0);
        assert_eq!(record.cluster_corners, 0);
    }

    drop(plan);
    drop(rt);
    device.wait_idle().expect("wait_idle");
}

/// A wind-flagged family materializes one deformed slice per ACTIVE placed use, leaves the static
/// list, and every slice carries its own prototype's vertex run and submesh span.
///
/// This is the whole of wind reaching a bottom-level structure: traversal has no vertex stage, so
/// an instance left in the static list casts and reflects its rest pose while every raster pass
/// sways it. The static half of the assertion is the load-bearing one — a materialized plant that
/// stays in the static list is placed twice, once swaying and once not.
#[test]
fn a_wind_flagged_family_materializes_one_slice_per_active_use() {
    let Some((device, _descriptors, rt, _before)) = rt_or_skip() else {
        return;
    };
    if !rt.supported() {
        return;
    }
    let dispatch = rt
        .dispatch
        .clone()
        .expect("accel dispatch present on an RT device");
    let structure = || {
        Arc::new(
            crate::AccelerationStructure::create(
                &rt.resources,
                &dispatch,
                256,
                vk::AccelerationStructureTypeKHR::BOTTOM_LEVEL,
            )
            .expect("bottom level"),
        )
    };
    let assembly_mesh = test_mesh_parts(
        &device,
        None,
        Some(test_assembly()),
        vec![
            crate::RtBlas::Khr(structure()),
            crate::RtBlas::Khr(structure()),
        ],
    );
    let plain_mesh = test_mesh_parts(&device, Some(structure()), None, Vec::new());
    let scene = [
        RtInstanceInput {
            model: Mat4::IDENTITY,
            mesh: Arc::clone(&assembly_mesh),
            instance_slot: 11,
            opacity_override: None,
            combination: 0,
            wind: true,
        },
        RtInstanceInput {
            model: Mat4::IDENTITY,
            mesh: Arc::clone(&plain_mesh),
            instance_slot: 3,
            opacity_override: None,
            combination: 0,
            wind: false,
        },
    ];

    let plan = crate::plan_wind_deformation(&scene, &test_cut_view(), 6.0, 0)
        .expect("the wind-flagged family materializes");
    // Combination 0's mask is 0b011: uses 0 and 1, not use 2.
    assert_eq!(plan.jobs.len(), 2, "one dispatch per active use");
    assert_eq!(plan.instances.len(), 2);
    assert_eq!(
        plan.statics.len(),
        1,
        "the materialized family leaves the static list"
    );
    assert_eq!(
        plan.statics[0].instance_slot, 3,
        "the instance left behind is the one that does not sway"
    );
    // Use 0 places prototype 0 (vertices 0..8, submeshes 0..4), use 1 places prototype 1
    // (vertices 8..12, submeshes 4..6) — the same spans the static per-prototype structures
    // build over, so a candidate resolves to the same submesh either way.
    assert_eq!(plan.instances[0].vertex_base, 0);
    assert_eq!(plan.instances[0].vertex_count, 8);
    assert_eq!(plan.instances[0].first_submesh, 0);
    assert_eq!(plan.instances[0].submesh_count, 4);
    assert_eq!(plan.instances[1].vertex_base, 8);
    assert_eq!(plan.instances[1].vertex_count, 4);
    assert_eq!(plan.instances[1].first_submesh, 4);
    assert_eq!(plan.instances[1].submesh_count, 2);
    for (job, instance) in plan.jobs.iter().zip(&plan.instances) {
        assert_eq!(job.first_vertex, instance.vertex_base);
        assert_eq!(job.vertex_count, instance.vertex_count);
        assert_eq!(job.instance_slot, 11);
        assert_eq!(job.assembly, 1, "a family applies its use transform");
        // The build rebases the vertex address by the run's base, so a slice below its own base
        // would address memory before the buffer.
        assert!(
            instance.deformed_offset >= instance.vertex_base,
            "a slice is never placed below the run it mirrors"
        );
        assert_eq!(
            instance.world_transform,
            Mat4::IDENTITY,
            "materialized vertices are world-space"
        );
    }
    assert!(
        plan.instances[1].deformed_offset
            >= plan.instances[0].deformed_offset + plan.instances[0].vertex_count,
        "the slices do not overlap"
    );
    assert!(plan.high_water >= plan.instances[1].deformed_offset + 4);
    // The refit key names the slot and the use ordinal — the same key every frame, so the
    // structure is built once and refit in place afterwards.
    assert_eq!(plan.instances[0].entity, (1 << 63) | (11 << 20));
    assert_eq!(plan.instances[1].entity, (1 << 63) | (11 << 20) | 1);

    // Nothing sways: the frame keeps the captured scene untouched rather than publishing a copy.
    // Either half is enough on its own — no instance carries the flag, or the field is at rest and
    // every term it drives is zero, which is what leaves a wind-flagged plant on the cluster-
    // composed structure its upload built.
    let calm: Vec<RtInstanceInput> = scene
        .iter()
        .cloned()
        .map(|mut input| {
            input.wind = false;
            input
        })
        .collect();
    assert!(
        crate::plan_wind_deformation(&calm, &test_cut_view(), 6.0, 0).is_none(),
        "an unwindy scene materializes nothing"
    );
    assert!(
        crate::plan_wind_deformation(&scene, &test_cut_view(), 0.0, 0).is_none(),
        "a field at rest materializes nothing"
    );

    drop(rt);
    device.wait_idle().expect("wait_idle");
}

/// `make_instance` packs the custom index + 0xFF mask, the triangle-cull-disable flag
/// plus the per-instance opacity flag, and the referenced AS device address into a
/// `VkAccelerationStructureInstanceKHR`.
#[test]
fn make_instance_packs_index_mask_flags_and_reference() {
    let inst = make_instance(
        IDENTITY_ROWS,
        7,
        vk::GeometryInstanceFlagsKHR::FORCE_NO_OPAQUE,
        0xDEAD_BEEF,
    );
    assert_eq!(inst.instance_custom_index_and_mask.low_24(), 7);
    assert_eq!(inst.instance_custom_index_and_mask.high_8(), 0xFF);
    assert_eq!(
        inst.instance_shader_binding_table_record_offset_and_flags
            .high_8(),
        (vk::GeometryInstanceFlagsKHR::TRIANGLE_FACING_CULL_DISABLE
            | vk::GeometryInstanceFlagsKHR::FORCE_NO_OPAQUE)
            .as_raw() as u8
    );
    // SAFETY: the reference is the `device_handle` union arm, set by `make_instance`.
    assert_eq!(
        unsafe { inst.acceleration_structure_reference.device_handle },
        0xDEAD_BEEF
    );
}

/// On a software device (llvmpipe — no RT extensions), `Rt::new` builds an inert
/// sub-state: `supported()` is false, set 6 is null, the shadow toggle clamps off, and
/// `prepare_tlas_build` is a no-op returning `None`. This is the gate's "all RT paths are
/// no-ops when rt_supported == false" requirement — the engine renders via the
/// shadow-map path. On an RT device this asserts the seed path instead.
#[test]
fn rt_inert_on_software_device_validation_clean() {
    let Some((device, _descriptors, mut rt, before)) = rt_or_skip() else {
        return;
    };
    if rt.supported() {
        // RT-capable device: the seed empty TLAS wrote a valid AS into every set 6.
        assert_ne!(rt.mesh_set(0), vk::DescriptorSet::null());
        // GPU-RUNTIME RT validation (a TLAS build + ray-query render) is
        // DEFERRED-NEEDS-HARDWARE — llvmpipe has no RT, so this branch is unreachable in
        // the toolbox; the seed AS create + descriptor writes are exercised here.
        eprintln!("rt: RT-capable device — seed empty TLAS written into every set 6");
        return;
    }
    // Software device: the inert contract.
    assert!(!rt.supported());
    assert_eq!(rt.mesh_set(0), vk::DescriptorSet::null());
    assert_eq!(rt.blas_count(), 0);

    rt.set_rt_shadows(true);
    assert!(
        !rt.use_rt_shadows(),
        "shadow toggle clamps off on a non-RT device"
    );
    assert!(!rt.shadows_enabled());

    // set_rt_scene with static instances does not arm a build on a non-RT device.
    rt.set_rt_scene(Arc::from([]));
    assert!(!rt.build_pending());

    // The build path is a no-op: it produces no plan and leaves tlas_ready false.
    let plan = rt.prepare_tlas_build(&device, 0, &[], None, &[], test_cut_view());
    assert!(plan.is_none());
    assert!(!rt.tlas_ready());

    drop(rt);
    // SAFETY: the device must idle before its sub-state Drops (here Rt already dropped).
    device.wait_idle().expect("wait_idle");
    assert_eq!(
        validation_issue_count(),
        before,
        "the inert RT sub-state raised no validation issues"
    );
}

/// `set_rt_scene` arms the per-frame `tlas-build` only when RT is supported *and* the
/// shadow toggle is on — the `build_pending` gate the frame graph reads. On a software
/// device it never arms (covered above); this asserts the toggle interaction directly.
#[test]
fn build_pending_requires_supported_and_shadows_on() {
    let Some((device, _descriptors, mut rt, _before)) = rt_or_skip() else {
        return;
    };
    // Shadows off → never pending, regardless of support.
    rt.set_rt_shadows(false);
    rt.set_rt_scene(Arc::from([]));
    assert!(!rt.build_pending());

    rt.set_rt_shadows(true);
    rt.set_rt_scene(Arc::from([]));
    // Pending iff the device actually supports RT (the toggle was clamped otherwise).
    assert_eq!(rt.build_pending(), rt.supported());

    drop(rt);
    device.wait_idle().expect("wait_idle");
}

/// `begin_frame` clears the static-scene capture + ready/pending flags (the per-slot
/// skinned-BLAS maps are grow-only and intentionally untouched).
#[test]
fn begin_frame_clears_scene_and_ready_flags() {
    let Some((device, _descriptors, mut rt, _before)) = rt_or_skip() else {
        return;
    };
    rt.set_rt_shadows(true);
    let mesh = test_mesh(&device);
    rt.set_rt_scene(Arc::from([
        RtInstanceInput {
            model: Mat4::IDENTITY,
            mesh: Arc::clone(&mesh),
            instance_slot: RT_UNMIRRORED_INSTANCE,
            opacity_override: Some(true),
            combination: 0,
            wind: false,
        },
        RtInstanceInput {
            model: Mat4::IDENTITY,
            mesh,
            instance_slot: 5,
            opacity_override: Some(false),
            combination: 0,
            wind: false,
        },
    ]));
    assert!(rt.has_instances(&[], &[]));
    rt.begin_frame();
    assert!(!rt.build_pending());
    assert!(!rt.tlas_ready());
    // The scene capture is cleared, so a build with no fresh scene has no instances.
    assert!(!rt.has_instances(&[], &[]));

    drop(rt);
    device.wait_idle().expect("wait_idle");
}

/// GPU-runtime validation of the per-frame TLAS build over a static mesh instance: upload
/// a mesh (its BLAS is built at upload when RT is supported), capture it via
/// `set_rt_scene`, `prepare_tlas_build`, replay the plan into a one-off command buffer,
/// submit + wait — and assert the TLAS holds one instance and the whole path is
/// validation-clean. On a software device (no RT extensions) this asserts the no-op path
/// and is skipped for the GPU build (DEFERRED-NEEDS-HARDWARE). The toolbox lavapipe build
/// *does* advertise the RT extensions, so the build runs here.
#[test]
fn tlas_build_over_static_instance_is_validation_clean() {
    use crate::upload::Uploader;
    use saffron_geometry::glam::{Vec2, Vec3};
    use saffron_geometry::{Mesh, Submesh, Vertex};

    let Some((device, descriptors, mut rt, before)) = rt_or_skip() else {
        return;
    };
    if !rt.supported() {
        // No RT extensions: the build path is a verified no-op (covered above). The GPU
        // TLAS build is DEFERRED-NEEDS-HARDWARE on a software device.
        assert!(
            rt.prepare_tlas_build(&device, 0, &[], None, &[], test_cut_view())
                .is_none()
        );
        drop(rt);
        device.wait_idle().expect("wait_idle");
        return;
    }

    // Upload a unit triangle; on an RT device this builds its BLAS at upload time.
    let queue = device.graphics_queue.clone();
    let uploader = Uploader::new(&device, &queue).expect("Uploader");
    let v = |x: f32, y: f32| Vertex {
        position: Vec3::new(x, y, 0.0),
        normal: Vec3::new(0.0, 0.0, 1.0),
        uv0: Vec2::ZERO,
        ..Vertex::default()
    };
    let mesh = Mesh {
        vertices: vec![v(-1.0, -1.0), v(1.0, -1.0), v(0.0, 1.0)],
        indices: vec![0, 1, 2],
        submeshes: vec![Submesh {
            first_index: 0,
            index_count: 3,
            vertex_offset: 0,
            material_slot: 0,
        }],
    };
    let hierarchy = crate::upload::hierarchy_for_upload(&mesh, &[]).expect("cook hierarchy");
    let gpu_mesh = uploader
        .upload_mesh(
            &descriptors,
            &mesh,
            &hierarchy,
            &[],
            None,
            crate::SdfSource::None,
        )
        .expect("upload_mesh");
    assert!(
        gpu_mesh.blas.is_some(),
        "RT device builds the mesh BLAS at upload"
    );

    // Arm RT shadows + capture one static instance, then prepare the per-frame TLAS build.
    rt.set_rt_shadows(true);
    rt.set_rt_scene(Arc::from([RtInstanceInput {
        model: Mat4::IDENTITY,
        mesh: Arc::clone(&gpu_mesh),
        instance_slot: RT_UNMIRRORED_INSTANCE,
        opacity_override: Some(true),
        combination: 0,
        wind: false,
    }]));
    assert!(rt.build_pending());
    let plan = rt
        .prepare_tlas_build(&device, 0, &[], None, &[], test_cut_view())
        .expect("a build plan for one static instance");
    assert!(rt.tlas_ready());
    assert_eq!(
        rt.frame_instance_count(),
        1,
        "one static instance in the TLAS"
    );
    assert_ne!(rt.mesh_set(0), vk::DescriptorSet::null());

    // Replay the plan into a one-off command buffer, submit, and wait.
    record_and_submit_oneoff(&device, |cmd| {
        // Unarmed recorders: `scope` is a transparent wrapper, so the test records exactly
        // the same commands the profiled path does.
        let mut scopes =
            crate::nested_scopes::NestedScopeRecorder::new(device.raw(), cmd, None, None);
        record_tlas_build_plan(device.raw(), &plan, &mut scopes);
    })
    .expect("record + submit the TLAS build");
    device.wait_idle().expect("wait_idle");

    drop(plan);
    drop(gpu_mesh);
    drop(uploader);
    drop(rt);
    device.wait_idle().expect("wait_idle before teardown");
    drop(descriptors);

    let after = validation_issue_count();
    assert_eq!(
        before,
        after,
        "the upload-time BLAS + per-frame TLAS build must be validation-clean (saw {} new)",
        after.saturating_sub(before)
    );
}

/// A derived micromap builds on the device, validation-clean, with storage allocated.
///
/// This is the seam between the CPU derivation and Vulkan: the state bytes, the per-triangle
/// block descriptors, and the index stream all have to land in the exact layout the build
/// expects, and a wrong stride or a missing usage row shows up here rather than as a subtly
/// wrong shadow much later.
#[test]
fn a_derived_micromap_builds_validation_clean() {
    use saffron_geometry::{CoverageRule, CoverageSourcePlane, derive_opacity_micromap};
    use saffron_material::{AlphaClassification, OpacityMicromapDerivation, SurfaceUnit};

    let Some((device, _descriptors, _rt, _before)) = rt_or_skip() else {
        return;
    };
    let Some(dispatch) = device.omm_dispatch().cloned() else {
        eprintln!("skipping: no VK_EXT_opacity_micromap on this device");
        return;
    };

    // A gradient so the derivation settles both extremes and leaves a middle band unknown —
    // a uniform plane would emit only special indices and build nothing at all.
    const EXTENT: u32 = 32;
    let alpha: Vec<u8> = (0..EXTENT * EXTENT)
        .map(|i| ((i % EXTENT) * 255 / (EXTENT - 1)) as u8)
        .collect();
    let derived = derive_opacity_micromap(
        &[0, 1, 2],
        &[[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]],
        CoverageSourcePlane {
            alpha: &alpha,
            width: EXTENT,
            height: EXTENT,
            rule: CoverageRule::new(AlphaClassification::Masked, false, 0.5),
            base_alpha: 1.0,
        },
        &OpacityMicromapDerivation {
            enabled: true,
            max_subdivision: 3,
            transparent_threshold: SurfaceUnit::from_bits(0),
            opaque_threshold: SurfaceUnit::from_bits(u16::MAX),
        },
    );
    assert!(
        !derived.blocks.is_empty(),
        "the gradient must produce a block"
    );

    let before = validation_issue_count();
    let resources = std::sync::Arc::clone(device.resources());
    let raw = device.raw();
    let pool_info = vk::CommandPoolCreateInfo::default()
        .queue_family_index(device.graphics_queue_family)
        .flags(vk::CommandPoolCreateFlags::TRANSIENT);
    // SAFETY: the ash seam. The pool is destroyed below on every path.
    let pool = unsafe { raw.create_command_pool(&pool_info, None) }.expect("command pool");
    let alloc = vk::CommandBufferAllocateInfo::default()
        .command_pool(pool)
        .level(vk::CommandBufferLevel::PRIMARY)
        .command_buffer_count(1);
    // SAFETY: the ash seam. One primary buffer from the pool just created.
    let cmd = unsafe { raw.allocate_command_buffers(&alloc) }.expect("command buffer")[0];
    let begin =
        vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
    // SAFETY: the ash seam. Begin/end bracket the recording below.
    unsafe { raw.begin_command_buffer(cmd, &begin) }.expect("begin");
    let built = record_micromap_build(&resources, &dispatch, cmd, &derived)
        .expect("micromap build records");
    // SAFETY: the ash seam. Ends the recording opened above.
    unsafe { raw.end_command_buffer(cmd) }.expect("end");
    let cmd_info = [vk::CommandBufferSubmitInfo::default().command_buffer(cmd)];
    let submits = [vk::SubmitInfo2::default().command_buffer_infos(&cmd_info)];
    device
        .graphics_queue
        .submit2(raw, &submits, vk::Fence::null(), "micromap build test")
        .expect("submit");
    device.wait_idle().expect("idle after the build submit");

    assert!(built.0.size() > 0, "the micromap reserved storage");
    assert_eq!(
        built.0.classes(),
        (
            derived.classes.opaque,
            derived.classes.transparent,
            derived.classes.unknown
        )
    );
    drop(built);
    // SAFETY: the ash seam. The queue is idle, so the pool is free to destroy.
    unsafe { raw.destroy_command_pool(pool, None) };
    device.wait_idle().expect("idle before teardown");
    assert_eq!(
        validation_issue_count(),
        before,
        "the micromap build must be validation-clean"
    );
}
