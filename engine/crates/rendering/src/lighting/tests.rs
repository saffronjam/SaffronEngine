use super::*;
use crate::device::SurfaceSource;
use crate::pipelines::Pipelines;
use crate::resources::BindlessFreeList;
use crate::validation_issue_count;
use std::mem::offset_of;
use std::sync::Mutex;

/// `LightUbo` is exactly 656 bytes with each field at the std140 offset the mesh
/// fragment reads — the contract the shaded path reads by raw bytes.
#[test]
fn light_ubo_byte_layout_matches_std140() {
    assert_eq!(size_of::<LightUbo>(), 832);
    assert_eq!(offset_of!(LightUbo, vsm_basis), 608);
    assert_eq!(offset_of!(LightUbo, vsm_levels), 672);
    assert_eq!(offset_of!(LightUbo, vsm_params), 800);
    assert_eq!(offset_of!(LightUbo, vsm_page_table), 816);
    assert_eq!(align_of::<LightUbo>(), 16);
    assert_eq!(offset_of!(LightUbo, direction_ambient), 0);
    assert_eq!(offset_of!(LightUbo, color_intensity), 16);
    assert_eq!(offset_of!(LightUbo, moon_direction_intensity), 32);
    assert_eq!(offset_of!(LightUbo, moon_color), 48);
    assert_eq!(offset_of!(LightUbo, counts), 64);
    assert_eq!(offset_of!(LightUbo, eye_position), 80);
    assert_eq!(offset_of!(LightUbo, spot_shadow_view_proj), 96);
    assert_eq!(offset_of!(LightUbo, spot_shadow), 160);
    assert_eq!(offset_of!(LightUbo, point_shadow), 176);
    assert_eq!(offset_of!(LightUbo, point_shadow_meta), 192);
    assert_eq!(offset_of!(LightUbo, screen_flags), 208);
    assert_eq!(offset_of!(LightUbo, ddgi_volume_min), 224);
    assert_eq!(offset_of!(LightUbo, ddgi_volume_extent), 240);
    assert_eq!(offset_of!(LightUbo, ddgi_probe_count), 256);
    assert_eq!(offset_of!(LightUbo, ddgi_scroll_base), 272);
    assert_eq!(offset_of!(LightUbo, sdf_occlusion), 288);
    assert_eq!(offset_of!(LightUbo, ambient_color), 304);
    assert_eq!(offset_of!(LightUbo, ibl_tint), 320);
    assert_eq!(offset_of!(LightUbo, extra_flags), 336);
    assert_eq!(offset_of!(LightUbo, prev_view_proj), 352);
    assert_eq!(offset_of!(LightUbo, froxel_fog), 416);
    assert_eq!(offset_of!(LightUbo, cloud_shadow_right), 432);
    assert_eq!(offset_of!(LightUbo, cloud_shadow_up), 448);
    assert_eq!(offset_of!(LightUbo, cloud_shadow_centers), 464);
    assert_eq!(offset_of!(LightUbo, cloud_shadow_meta), 512);
    assert_eq!(offset_of!(LightUbo, wind_dir_speed_gust), 528);
    assert_eq!(offset_of!(LightUbo, wind_params), 544);
    assert_eq!(offset_of!(LightUbo, wind_meta), 560);
    assert_eq!(offset_of!(LightUbo, wind_time), 576);
    assert_eq!(offset_of!(LightUbo, wind_sources), 592);
}

/// `ClusterParams` is exactly 192 bytes with each field at the std140 offset both
/// the cull compute and the mesh fragment read.
#[test]
fn cluster_params_byte_layout_matches_std140() {
    assert_eq!(size_of::<ClusterParams>(), 176);
    assert_eq!(align_of::<ClusterParams>(), 16);
    assert_eq!(offset_of!(ClusterParams, view), 0);
    assert_eq!(offset_of!(ClusterParams, inverse_projection), 64);
    assert_eq!(offset_of!(ClusterParams, grid_size), 128);
    assert_eq!(offset_of!(ClusterParams, screen_size), 144);
    assert_eq!(offset_of!(ClusterParams, z_planes), 160);
}

/// The cluster constants match the shader (`light_cull.slang`) + the dispatch group
/// count — a drift here silently mis-sizes the cluster SSBO or culls the wrong grid.
#[test]
fn cluster_grid_matches_shader() {
    assert_eq!(CLUSTER_COUNT, 16 * 9 * 24);
    assert_eq!(MAX_LIGHTS_PER_CLUSTER, 64);
    // One cluster's SSBO record: a count u32 + a 64-slot index array.
    assert_eq!(CLUSTER_STRIDE, 4 * (1 + 64));
}

/// A standard perspective camera looking down −Z with one viewport-sized grid. The
/// cull math is built against this fixture in the tests below.
fn camera_params(lights: u32) -> ClusterParams {
    // A 90° vertical FOV perspective; near 0.1, far 100, 1600×900 (the 16×9 grid's
    // native pixel ratio). The view is identity (camera at the origin looking −Z).
    let proj = Mat4::perspective_rh(std::f32::consts::FRAC_PI_2, 1600.0 / 900.0, 0.1, 100.0);
    ClusterParams {
        view: Mat4::IDENTITY,
        inverse_projection: proj.inverse(),
        grid_size: UVec4::new(CLUSTER_GRID_X, CLUSTER_GRID_Y, CLUSTER_GRID_Z, lights),
        screen_size: UVec4::new(1600, 900, 1, 0),
        z_planes: Vec4::new(0.1, 100.0, 0.0, 0.0),
    }
}

/// The exponential Z slicing places slice 0 at the near plane and the last slice's
/// far edge at the far plane (the froxel depth partition the cull derives), and every
/// cluster AABB is non-degenerate (min < max on each axis the slice spans).
#[test]
fn cluster_aabbs_partition_the_frustum_depth() {
    let params = camera_params(0);
    // Center column/row cluster at the near and far slices.
    let cx = CLUSTER_GRID_X / 2;
    let cy = CLUSTER_GRID_Y / 2;
    let (near_min, near_max) = cluster_aabb(&params, cx, cy, 0);
    let (far_min, far_max) = cluster_aabb(&params, cx, cy, CLUSTER_GRID_Z - 1);

    // The camera looks down −Z, so the near slice straddles smaller |z| than the far.
    assert!(
        near_max.z <= 0.0 && far_min.z < near_min.z,
        "the far slice is deeper (more negative Z) than the near slice \
         (near_max.z={}, far_min.z={})",
        near_max.z,
        far_min.z
    );
    // The near slice begins at ~−near (exponential slice 0 lower edge = −near).
    assert!(
        (near_max.z - (-params.z_planes.x)).abs() < 1.0,
        "slice 0 starts near the near plane (near_max.z={})",
        near_max.z
    );
    // Every AABB spans a real volume in Z.
    assert!(near_min.z < near_max.z);
    assert!(far_min.z < far_max.z);
}

/// A point light placed inside a specific froxel is culled into that cluster, so its list
/// count is ≥ 1: the cull is correct, not merely empty.
#[test]
fn cull_fills_the_froxel_containing_a_point_light() {
    let params = camera_params(1);

    // Pick the center cluster at a mid Z slice; place a light at its AABB center.
    let cx = CLUSTER_GRID_X / 2;
    let cy = CLUSTER_GRID_Y / 2;
    let cz = CLUSTER_GRID_Z / 2;
    let (aabb_min, aabb_max) = cluster_aabb(&params, cx, cy, cz);
    let center = (aabb_min + aabb_max) * 0.5;
    // The view is identity, so the view-space center is also the world position.
    let light = GpuLight {
        position_range: center.extend(1.0),
        color_intensity: Vec4::new(1.0, 1.0, 1.0, 5.0),
        direction_type: Vec4::ZERO,
        spot_cos: Vec4::ZERO,
    };

    let clusters = cull_clusters_cpu(&params, &[light]);
    let target = (cx + cy * CLUSTER_GRID_X + cz * CLUSTER_GRID_X * CLUSTER_GRID_Y) as usize;
    assert_eq!(
        clusters[target],
        vec![0],
        "the light landed in its own froxel's list (cluster {target})"
    );

    // A tiny light far outside every froxel touches no cluster (no spurious fill).
    let far_light = GpuLight {
        position_range: Vec3::new(0.0, 0.0, 1000.0).extend(0.01),
        color_intensity: Vec4::new(1.0, 1.0, 1.0, 5.0),
        direction_type: Vec4::ZERO,
        spot_cos: Vec4::ZERO,
    };
    let empty = cull_clusters_cpu(&params, &[far_light]);
    assert!(
        empty.iter().all(Vec::is_empty),
        "a light behind the camera at radius 0.01 lands in no froxel"
    );
}

/// A large-radius light intersects many clusters; the per-cluster list never exceeds
/// the cap, and a cluster within the light's reach records it. The intersection test
/// is a sphere-vs-AABB, so a light whose sphere overlaps the AABB is recorded.
#[test]
fn cull_respects_the_per_cluster_cap_and_records_overlap() {
    let params = camera_params(1);
    // A light at the frustum center with a huge radius reaches many clusters.
    let light = GpuLight {
        position_range: Vec3::new(0.0, 0.0, -10.0).extend(1000.0),
        color_intensity: Vec4::new(1.0, 1.0, 1.0, 5.0),
        direction_type: Vec4::ZERO,
        spot_cos: Vec4::ZERO,
    };
    let clusters = cull_clusters_cpu(&params, &[light]);
    let touched = clusters.iter().filter(|c| !c.is_empty()).count();
    assert!(touched > 0, "a huge light reaches at least one cluster");
    for list in &clusters {
        assert!(
            list.len() <= MAX_LIGHTS_PER_CLUSTER as usize,
            "a cluster never records more than the cap"
        );
    }
}

/// The 6 point-shadow face matrices are distinct and project the light's own position
/// to the clip origin (each face's eye is the light), so the cube renders 6 valid
/// 90°-FOV views around the light. Pure math, no device.
#[test]
fn point_shadow_faces_are_six_distinct_views_centered_on_the_light() {
    let pos = Vec3::new(2.0, 3.0, -4.0);
    let faces = point_shadow_face_matrices(pos, 50.0);
    for (i, face) in faces.iter().enumerate() {
        // The light's own position maps to clip w≈0 (it is the eye), so the
        // homogeneous w is near zero — distinct from any scene point in front.
        let clip = *face * pos.extend(1.0);
        assert!(
            clip.w.abs() < 1e-3,
            "face {i} places its own eye at the camera (w={})",
            clip.w
        );
    }
    // No two faces are the same transform (each looks a different axis).
    for i in 0..6 {
        for j in (i + 1)..6 {
            assert_ne!(
                faces[i].to_cols_array(),
                faces[j].to_cols_array(),
                "faces {i} and {j} look different directions"
            );
        }
    }
}

/// A world direction from the light must render onto the *same* cube texel the sampler later
/// reads for that direction — otherwise shadows land on the wrong side. This pins the face
/// orientation against the fixed cube-map `(s, t)` selection (Vulkan spec 16.5.4), so a stray
/// window Y-flip on the projection (which vertically mirrors every face) fails here. Pure math.
#[test]
fn point_shadow_faces_match_the_cube_sampling_convention() {
    // The cube-map face + `(s, t)` for a sample direction: major axis picks the face, then
    // `s = (sc/|ma| + 1)/2`, `t = (tc/|ma| + 1)/2`, with `t = 0` at the top of the face image.
    fn cube_st(d: Vec3) -> (usize, f32, f32) {
        let a = d.abs();
        let (face, sc, tc, ma) = if a.x >= a.y && a.x >= a.z {
            if d.x > 0.0 {
                (0, -d.z, -d.y, d.x)
            } else {
                (1, d.z, -d.y, -d.x)
            }
        } else if a.y >= a.z {
            if d.y > 0.0 {
                (2, d.x, d.z, d.y)
            } else {
                (3, d.x, -d.z, -d.y)
            }
        } else if d.z > 0.0 {
            (4, d.x, -d.y, d.z)
        } else {
            (5, -d.x, -d.y, -d.z)
        };
        (face, (sc / ma + 1.0) * 0.5, (tc / ma + 1.0) * 0.5)
    }

    let faces = point_shadow_face_matrices(Vec3::ZERO, 50.0);
    // One off-centre direction per face (so a mirror is visible), incl. the `-Y` face an
    // overhead light's shadow uses — the exact case that rendered on the wrong side.
    let dirs = [
        Vec3::new(1.0, 0.3, -0.4),
        Vec3::new(-1.0, 0.3, 0.4),
        Vec3::new(0.2, 1.0, 0.5),
        Vec3::new(0.2, -1.0, 0.5),
        Vec3::new(0.3, -0.4, 1.0),
        Vec3::new(0.3, -0.4, -1.0),
    ];
    for dir in dirs {
        let d = dir.normalize();
        let (face, s, t) = cube_st(d);
        // Render a point one unit out from the light along `d` through its face.
        let clip = faces[face] * d.extend(1.0);
        let ndc = clip.truncate() / clip.w;
        // Vulkan framebuffer: NDC `(-1, -1)` is the top-left texel, so `u = (x+1)/2`, `v = (y+1)/2`.
        let u = (ndc.x + 1.0) * 0.5;
        let v = (ndc.y + 1.0) * 0.5;
        assert!(
            (u - s).abs() < 1e-3 && (v - t).abs() < 1e-3,
            "face {face}: rendered texel ({u:.3}, {v:.3}) must match the cube sample ({s:.3}, {t:.3})"
        );
    }
}

/// The lighting rig builds against a real device — the per-frame light + cluster sets,
/// the shadow maps bound into every light set — and tears down validation-clean. This
/// is the GPU-side acceptance for the descriptor wiring on llvmpipe. Skips when no
/// Vulkan device is present.
#[test]
fn lighting_rig_builds_and_teardown_is_validation_clean() {
    let device = match Device::new(&SurfaceSource::Offscreen) {
        Ok(device) => device,
        Err(err) => {
            eprintln!("skipping: no Vulkan device obtainable ({err})");
            return;
        }
    };
    let before = validation_issue_count();

    let free_list: BindlessFreeList = Arc::new(Mutex::new(Vec::new()));
    let descriptors = Descriptors::new(&device, &free_list).expect("Descriptors::new");
    let vsm = crate::vsm::VsmGpu::new(&device).expect("VsmGpu::new");
    let mut lighting =
        Lighting::new(&device, &descriptors, vsm.atlas.view()).expect("Lighting::new");

    // Defaults: clustered + shadows on, no lights uploaded yet.
    assert!(lighting.use_clustered);
    assert!(lighting.use_shadows);
    assert_eq!(lighting.frame_light_count(), 0);

    // A scene-lighting write with two punctual lights uploads the list + fills the UBO.
    let lights = vec![
        GpuLight {
            position_range: Vec3::new(1.0, 2.0, -3.0).extend(5.0),
            color_intensity: Vec4::new(1.0, 0.5, 0.25, 10.0),
            direction_type: Vec4::ZERO,
            spot_cos: Vec4::ZERO,
        },
        GpuLight {
            position_range: Vec3::new(-2.0, 1.0, -4.0).extend(3.0),
            color_intensity: Vec4::new(0.2, 0.8, 1.0, 6.0),
            direction_type: Vec4::new(0.0, -1.0, 0.0, 1.0),
            spot_cos: Vec4::new(0.9, 0.8, 0.0, 0.0),
        },
    ];
    let scene = SceneLighting {
        lights,
        ..SceneLighting::default()
    };
    lighting
        .set_scene_lighting(&descriptors, 0, &scene)
        .expect("set_scene_lighting");
    assert_eq!(lighting.frame_light_count(), 2);

    // The cluster camera arms a cull dispatch (clustered on + lights present).
    lighting.set_cluster_camera(
        0,
        ClusterCamera {
            view: Mat4::IDENTITY,
            projection: Mat4::perspective_rh(std::f32::consts::FRAC_PI_2, 16.0 / 9.0, 0.1, 100.0),
            width: 1600,
            height: 900,
            near: 0.1,
            far: 100.0,
        },
    );
    assert!(
        lighting.take_cluster_dispatch_pending(),
        "clustered + lights arms the cull"
    );

    drop(lighting);
    drop(vsm);
    drop(descriptors);
    device.wait_idle().expect("idle before teardown");
    drop(device);

    let after = validation_issue_count();
    assert_eq!(
        before,
        after,
        "the lighting rig's construct + lighting writes + teardown must be \
         validation-clean (saw {} new issue(s))",
        after.saturating_sub(before)
    );
}

/// The light-cull compute dispatch fills the cluster SSBO on a real device: the froxel
/// containing a known point light reports count ≥ 1, matching the [`cull_clusters_cpu`] oracle,
/// validation-clean. Skips when no Vulkan device is present.
#[test]
fn light_cull_dispatch_fills_the_froxel_on_gpu() {
    let device = match Device::new(&SurfaceSource::Offscreen) {
        Ok(device) => device,
        Err(err) => {
            eprintln!("skipping: no Vulkan device obtainable ({err})");
            return;
        }
    };
    let before = validation_issue_count();

    let free_list: BindlessFreeList = Arc::new(Mutex::new(Vec::new()));
    let descriptors = Descriptors::new(&device, &free_list).expect("Descriptors::new");
    let mut pipelines = Pipelines::new(&device, &descriptors, vk::SampleCountFlags::TYPE_1);
    let cull = pipelines
        .request_light_cull()
        .expect("light-cull compute PSO builds on llvmpipe");

    // Place one light at the center of a known froxel (mid grid, mid Z slice).
    let proj = Mat4::perspective_rh(std::f32::consts::FRAC_PI_2, 1600.0 / 900.0, 0.1, 100.0);
    let params = ClusterParams {
        view: Mat4::IDENTITY,
        inverse_projection: proj.inverse(),
        grid_size: UVec4::new(CLUSTER_GRID_X, CLUSTER_GRID_Y, CLUSTER_GRID_Z, 1),
        screen_size: UVec4::new(1600, 900, 1, 0),
        z_planes: Vec4::new(0.1, 100.0, 0.0, 0.0),
    };
    let cx = CLUSTER_GRID_X / 2;
    let cy = CLUSTER_GRID_Y / 2;
    let cz = CLUSTER_GRID_Z / 2;
    let (aabb_min, aabb_max) = cluster_aabb(&params, cx, cy, cz);
    let center = (aabb_min + aabb_max) * 0.5;
    let light = GpuLight {
        position_range: center.extend(1.0),
        color_intensity: Vec4::new(1.0, 1.0, 1.0, 5.0),
        direction_type: Vec4::ZERO,
        spot_cos: Vec4::ZERO,
    };
    let target = (cx + cy * CLUSTER_GRID_X + cz * CLUSTER_GRID_X * CLUSTER_GRID_Y) as usize;

    let counts = run_cull_dispatch(&device, &descriptors, &cull, &params, &[light])
        .expect("cull dispatch + readback");
    assert!(
        counts[target] >= 1,
        "the GPU cull filled the froxel containing the light (cluster {target} count={})",
        counts[target]
    );
    // The CPU oracle agrees: the same froxel is the (only) one with a fill.
    let oracle = cull_clusters_cpu(&params, &[light]);
    assert_eq!(
        oracle[target],
        vec![0],
        "the CPU oracle agrees the light lands in cluster {target}"
    );

    drop(cull);
    drop(pipelines);
    drop(descriptors);
    device.wait_idle().expect("idle before teardown");
    drop(device);

    let after = validation_issue_count();
    assert_eq!(
        before,
        after,
        "the light-cull dispatch must be validation-clean (saw {} new issue(s))",
        after.saturating_sub(before)
    );
}

/// Allocates a host-visible params UBO + light SSBO + cluster SSBO, writes a compute
/// cluster set, runs the cull PSO on a one-off command buffer, and reads back each
/// cluster's `count` (the first u32 of its `CLUSTER_STRIDE` record). The
/// GPU-runtime mirror of [`cull_clusters_cpu`].
fn run_cull_dispatch(
    device: &Device,
    descriptors: &Descriptors,
    cull: &Arc<crate::Pipeline>,
    params: &ClusterParams,
    lights: &[GpuLight],
) -> Result<Vec<u32>> {
    let resources = device.resources();
    let raw = device.raw();

    let mut params_buf =
        make_mapped_uniform_buffer(resources, size_of::<ClusterParams>() as vk::DeviceSize)?;
    let params_bytes = bytemuck::bytes_of(params);
    params_buf.mapped_bytes().expect("params mapped")[..params_bytes.len()]
        .copy_from_slice(params_bytes);
    let mut light_buf = make_mapped_storage_buffer(
        resources,
        (lights.len().max(1) * size_of::<GpuLight>()) as vk::DeviceSize,
    )?;
    let light_bytes: &[u8] = bytemuck::cast_slice(lights);
    light_buf.mapped_bytes().expect("light mapped")[..light_bytes.len()]
        .copy_from_slice(light_bytes);
    // Host-visible cluster buffer (the real rig's is device-local; here it is mapped
    // so the test reads the counts back directly).
    let cluster_buf =
        make_mapped_storage_buffer(resources, u64::from(CLUSTER_COUNT) * CLUSTER_STRIDE)?;

    let set = descriptors.allocate_set(descriptors.cluster_set_layout())?;
    descriptors.write_uniform_buffer(set, 0, params_buf.handle(), params_buf.size());
    descriptors.write_storage_buffer(set, 1, light_buf.handle(), light_buf.size());
    descriptors.write_storage_buffer(set, 2, cluster_buf.handle(), cluster_buf.size());

    let pool_info =
        vk::CommandPoolCreateInfo::default().queue_family_index(device.graphics_queue_family);
    // SAFETY: the ash seam. Freed at the end of the function.
    let pool = crate::checked(unsafe { raw.create_command_pool(&pool_info, None) }, "pool")?;
    let alloc = vk::CommandBufferAllocateInfo::default()
        .command_pool(pool)
        .level(vk::CommandBufferLevel::PRIMARY)
        .command_buffer_count(1);
    // SAFETY: the ash seam. One buffer from the pool above.
    let cmd = crate::checked(unsafe { raw.allocate_command_buffers(&alloc) }, "cmd")?[0];
    // SAFETY: the ash seam. Default fence.
    let fence = crate::checked(
        unsafe { raw.create_fence(&vk::FenceCreateInfo::default(), None) },
        "fence",
    )?;

    let groups = CLUSTER_COUNT.div_ceil(64);
    let record = || -> Result<()> {
        let begin = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
        // SAFETY: the ash seam. The dispatch reads the params/light SSBOs and writes
        // the cluster SSBO; a host-visible buffer needs a host-read barrier after.
        unsafe {
            crate::checked(raw.begin_command_buffer(cmd, &begin), "begin")?;
            raw.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, cull.handle());
            raw.cmd_bind_descriptor_sets(
                cmd,
                vk::PipelineBindPoint::COMPUTE,
                cull.layout(),
                0,
                &[set],
                &[],
            );
            raw.cmd_dispatch(cmd, groups, 1, 1);
            // COMPUTE write → HOST read barrier so the mapped readback sees the cull.
            let barrier = vk::MemoryBarrier2::default()
                .src_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER)
                .src_access_mask(vk::AccessFlags2::SHADER_STORAGE_WRITE)
                .dst_stage_mask(vk::PipelineStageFlags2::HOST)
                .dst_access_mask(vk::AccessFlags2::HOST_READ);
            let barriers = [barrier];
            let dep = vk::DependencyInfo::default().memory_barriers(&barriers);
            raw.cmd_pipeline_barrier2(cmd, &dep);
            crate::checked(raw.end_command_buffer(cmd), "end")?;
        }
        let cmd_info = [vk::CommandBufferSubmitInfo::default().command_buffer(cmd)];
        let submit = [vk::SubmitInfo2::default().command_buffer_infos(&cmd_info)];
        // SAFETY: the ash seam. Single-threaded queue use in the test.
        unsafe {
            device
                .graphics_queue
                .submit2(raw, &submit, fence, "submit")?;
            crate::checked(raw.wait_for_fences(&[fence], true, u64::MAX), "wait")?;
        }
        Ok(())
    };
    let result = record();

    let mut counts = vec![0u32; CLUSTER_COUNT as usize];
    if result.is_ok() {
        let ptr = cluster_buf.mapped_ptr();
        for (i, slot) in counts.iter_mut().enumerate() {
            // The count is the first u32 of each cluster's CLUSTER_STRIDE record.
            let offset = i * CLUSTER_STRIDE as usize;
            // SAFETY: the buffer is HOST_VISIBLE + MAPPED and sized CLUSTER_COUNT *
            // CLUSTER_STRIDE; `offset` is within it and 4-byte aligned.
            *slot = unsafe { std::ptr::read_unaligned(ptr.add(offset).cast::<u32>()) };
        }
    }
    // SAFETY: the ash seam. The fence was waited, so the pool/fence are idle.
    unsafe {
        raw.destroy_fence(fence, None);
        raw.destroy_command_pool(pool, None);
    }
    result.map(|()| counts)
}
