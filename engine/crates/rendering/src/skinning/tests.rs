use super::*;

/// The grow policy seeds from the initial capacity when empty, doubles to cover the
/// vertex count, and never shrinks below the existing capacity — the deformed buffer
/// grows to fit peak vertices and is not shrunk.
#[test]
fn grow_capacity_doubles_and_never_shrinks() {
    assert_eq!(grow_capacity(0, 1), INITIAL_DEFORMED_CAPACITY);
    assert_eq!(grow_capacity(0, INITIAL_DEFORMED_CAPACITY + 1), 8192);
    assert_eq!(grow_capacity(8192, 100), 8192, "never shrinks");
    assert_eq!(grow_capacity(4096, 4096), 4096, "exact fit holds");
    assert_eq!(grow_capacity(4096, 20000), 32768);
}

/// Golden deterministic morph math: mirror `morph.slang`'s fixed-point scatter +
/// resolve on the CPU and prove (a) the integer atomic accumulation is bit-identical
/// regardless of scatter order — the property that makes the GPU pass reproducible on
/// llvmpipe and a hardware GPU alike — and (b) the dequantized blend reproduces the
/// analytic `base + Σ wᵢ·δᵢ` within the `1/MORPH_FIXED_SCALE` quantization step.
#[test]
fn morph_fixed_point_scatter_is_order_independent_and_matches_golden() {
    // One vertex, three active targets contributing position deltas at distinct weights.
    let base = [1.0_f32, -2.0, 0.5];
    let targets: [([f32; 3], f32); 3] = [
        ([0.25, 0.0, -0.5], 0.8), // (δ, weight)
        ([-0.125, 0.75, 0.0], 0.3),
        ([0.0625, -0.25, 1.0], 1.0),
    ];

    // Quantize one weighted delta lane exactly as the shader: round(w·δ·scale) as i32.
    let quant = |w: f32, d: f32| (w * d * MORPH_FIXED_SCALE).round() as i32;

    // Scatter in forward order vs. reverse order: integer adds commute, so the two
    // accumulators must be bit-identical (no float-atomic nondeterminism).
    let scatter = |order: &[usize]| -> [i32; 3] {
        let mut acc = [0_i32; 3];
        for &t in order {
            let (d, w) = targets[t];
            for lane in 0..3 {
                acc[lane] = acc[lane].wrapping_add(quant(w, d[lane]));
            }
        }
        acc
    };
    let forward = scatter(&[0, 1, 2]);
    let reverse = scatter(&[2, 1, 0]);
    assert_eq!(
        forward, reverse,
        "fixed-point integer scatter must be order-independent"
    );

    // Resolve: dequantize and add the base, as the shader's pass 2 does.
    let resolved: [f32; 3] =
        std::array::from_fn(|lane| base[lane] + forward[lane] as f32 / MORPH_FIXED_SCALE);

    // Analytic reference blend.
    let mut golden = base;
    for (d, w) in targets {
        for lane in 0..3 {
            golden[lane] += w * d[lane];
        }
    }

    // Each lane sums three rounded quantities, so the error is bounded by
    // 3·(0.5/scale). Assert well inside that.
    let eps = 3.0 * 0.5 / MORPH_FIXED_SCALE;
    for lane in 0..3 {
        assert!(
            (resolved[lane] - golden[lane]).abs() <= eps,
            "lane {lane}: resolved {} vs golden {} exceeds {eps}",
            resolved[lane],
            golden[lane]
        );
    }
}

/// `swap_morph_weights` mirrors the palette swap: an uncached entity returns `current`
/// (prev == cur ⇒ zero deformation motion); a length change (a different mesh binding)
/// returns `current`; a same-length second call returns the previously stored slice; and
/// the cache holds the latest weights after each call.
#[test]
fn swap_morph_weights_mirrors_palette_swap() {
    let device = match Device::new(&crate::SurfaceSource::Offscreen) {
        Ok(device) => device,
        Err(err) => {
            eprintln!("skipping: no Vulkan device obtainable ({err})");
            return;
        }
    };
    let mut skinning = Skinning::new(&device).expect("skinning");

    // Uncached: prev == cur.
    let first = [0.2_f32, 0.8];
    assert_eq!(
        skinning.swap_morph_weights(7, &first),
        first.to_vec(),
        "uncached entity returns current (zero motion on frame 1)"
    );

    // Same-length second call returns the previously stored slice.
    let second = [0.5_f32, 0.1];
    assert_eq!(
        skinning.swap_morph_weights(7, &second),
        first.to_vec(),
        "the previously stored weights are returned as prev"
    );

    // A length change (different mesh binding) returns current, not the stale slice.
    let third = [0.3_f32, 0.4, 0.5];
    assert_eq!(
        skinning.swap_morph_weights(7, &third),
        third.to_vec(),
        "a length change yields prev == cur"
    );

    // The cache now holds the latest (length-3) weights.
    let fourth = [0.9_f32, 0.0, 0.1];
    assert_eq!(
        skinning.swap_morph_weights(7, &fourth),
        third.to_vec(),
        "the cache held the latest weights after the length change"
    );
}

/// Dispatches past the per-frame set budget are clamped (and logged), not an error.
#[test]
fn clamp_to_set_budget_caps_at_max() {
    assert_eq!(clamp_to_set_budget(10), 10, "under the cap is unchanged");
    assert_eq!(
        clamp_to_set_budget(SKIN_MAX_SETS_PER_FRAME as usize),
        SKIN_MAX_SETS_PER_FRAME as usize,
        "exactly at the cap is unchanged"
    );
    assert_eq!(
        clamp_to_set_budget(SKIN_MAX_SETS_PER_FRAME as usize + 50),
        SKIN_MAX_SETS_PER_FRAME as usize,
        "past the cap clamps to the budget"
    );
}

/// The skin compute kernel deforms a known bind pose with a known palette to a committed
/// golden. A translation joint matrix must shift every vertex position by exactly that
/// translation, since the kernel applies the skin matrix without the model matrix, and leave
/// the normal + UV untouched, validation-clean.
#[test]
fn skin_kernel_deforms_to_golden_validation_clean() {
    use crate::device::SurfaceSource;
    use crate::pipelines::Pipelines;
    use crate::resources::BindlessFreeList;
    use crate::validation_issue_count;
    use ash::vk;
    use saffron_geometry::glam::{Vec2, Vec3};
    use std::sync::Mutex;

    let device = match Device::new(&SurfaceSource::Offscreen) {
        Ok(device) => device,
        Err(err) => {
            eprintln!("skipping: no Vulkan device obtainable ({err})");
            return;
        }
    };
    let before = validation_issue_count();

    let free_list: BindlessFreeList = Arc::new(Mutex::new(Vec::new()));
    let descriptors = crate::Descriptors::new(&device, &free_list).expect("Descriptors");
    let mut pipelines = Pipelines::new(&device, &descriptors, vk::SampleCountFlags::TYPE_1);
    let skinning = Skinning::new(&device).expect("Skinning");
    let skin_pso = request_skin_pipeline(&mut pipelines, &skinning).expect("skin PSO");

    // Three bind-pose vertices weighted fully on joint 0, a palette translating by
    // (10, 20, 30), and a host-visible deformed output we read back.
    let positions = [
        Vec3::new(1.0, 2.0, 3.0),
        Vec3::new(-1.0, 0.0, 5.0),
        Vec3::new(0.5, -2.5, 1.0),
    ];
    let normals = Vec3::new(0.0, 0.0, 1.0);
    let verts: Vec<Vertex> = positions
        .iter()
        .map(|&p| Vertex {
            position: p,
            normal: normals,
            uv0: Vec2::new(0.25, 0.75),
            ..Vertex::default()
        })
        .collect();
    let skins = vec![
        saffron_geometry::VertexSkin {
            joints: [0, 0, 0, 0],
            weights: [1.0, 0.0, 0.0, 0.0],
        };
        3
    ];
    let translation = Vec3::new(10.0, 20.0, 30.0);
    let palette = [Mat4::from_translation(translation)];

    let resources = device.resources();
    let host = |bytes: &[u8], usage: vk::BufferUsageFlags| -> Buffer {
        let info = vk_mem::AllocationCreateInfo {
            usage: vk_mem::MemoryUsage::Auto,
            flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_RANDOM
                | vk_mem::AllocationCreateFlags::MAPPED,
            ..Default::default()
        };
        let mut buffer =
            Buffer::new(resources, bytes.len() as u64, usage, &info).expect("host-visible buffer");
        buffer.mapped_bytes().expect("mapped")[..bytes.len()].copy_from_slice(bytes);
        buffer
    };
    let storage = vk::BufferUsageFlags::STORAGE_BUFFER;
    let in_verts = host(bytemuck::cast_slice(&verts), storage);
    let in_skins = host(bytemuck::cast_slice(&skins), storage);
    let in_palette = host(bytemuck::cast_slice(&palette), storage);
    let out_bytes = vec![0u8; verts.len() * size_of::<Vertex>()];
    let mut out_verts = host(&out_bytes, storage);

    // Wire the skin set against the four raw host buffers (the production path wires
    // it off a GpuMesh; here the bind-pose lives in plain host buffers), then dispatch.
    let set = alloc_and_wire_raw(
        resources.device(),
        skinning.frames[0].pool,
        skinning.set_layout,
        in_verts.handle(),
        in_skins.handle(),
        in_palette.handle(),
        in_palette.size(),
        out_verts.handle(),
        out_verts.size(),
    );

    dispatch_skin(&device, &skin_pso, set, verts.len() as u32);

    device.wait_idle().expect("idle after dispatch");
    let deformed: &[Vertex] = bytemuck::cast_slice(out_verts.mapped_bytes().expect("mapped"));
    for (i, src) in positions.iter().enumerate() {
        let got = deformed[i].position;
        let want = *src + translation;
        assert!(
            (got - want).length() < 1e-4,
            "vertex {i}: deformed {got:?} != golden {want:?}"
        );
        assert!(
            (deformed[i].normal - normals).length() < 1e-4,
            "vertex {i}: normal must survive the identity-rotation skin"
        );
        assert_eq!(
            deformed[i].uv0,
            Vec2::new(0.25, 0.75),
            "vertex {i}: uv passes through unchanged"
        );
    }

    drop(in_verts);
    drop(in_skins);
    drop(in_palette);
    drop(out_verts);
    drop(skin_pso);
    drop(skinning);
    drop(pipelines);
    drop(descriptors);
    drop(device);

    let after = validation_issue_count();
    assert_eq!(
        before,
        after,
        "the skin dispatch must be validation-clean (saw {} new issue(s))",
        after.saturating_sub(before)
    );
}

/// Allocates one skin set and writes the four raw storage buffers directly — the
/// test-only sibling of [`wire_set`] that takes raw handles rather than a `GpuMesh`.
#[allow(clippy::too_many_arguments)]
fn alloc_and_wire_raw(
    raw: &ash::Device,
    pool: vk::DescriptorPool,
    layout: vk::DescriptorSetLayout,
    verts: vk::Buffer,
    skins: vk::Buffer,
    palette: vk::Buffer,
    palette_size: vk::DeviceSize,
    out: vk::Buffer,
    out_size: vk::DeviceSize,
) -> vk::DescriptorSet {
    let layouts = [layout];
    let info = vk::DescriptorSetAllocateInfo::default()
        .descriptor_pool(pool)
        .set_layouts(&layouts);
    // SAFETY: the ash seam. The layout outlives the call; the set lives until the pool
    // is reset / destroyed.
    let set = unsafe { raw.allocate_descriptor_sets(&info) }.expect("alloc skin set")[0];
    let whole = vk::WHOLE_SIZE;
    let infos = [
        vk::DescriptorBufferInfo {
            buffer: verts,
            offset: 0,
            range: whole,
        },
        vk::DescriptorBufferInfo {
            buffer: skins,
            offset: 0,
            range: whole,
        },
        vk::DescriptorBufferInfo {
            buffer: palette,
            offset: 0,
            range: palette_size,
        },
        vk::DescriptorBufferInfo {
            buffer: out,
            offset: 0,
            range: out_size,
        },
    ];
    let writes: Vec<vk::WriteDescriptorSet> = (0..4)
        .map(|b| {
            vk::WriteDescriptorSet::default()
                .dst_set(set)
                .dst_binding(b as u32)
                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                .buffer_info(std::slice::from_ref(&infos[b]))
        })
        .collect();
    // SAFETY: the ash seam. Each write targets a binding the layout declares.
    unsafe { raw.update_descriptor_sets(&writes, &[]) };
    set
}

/// Records the skin compute dispatch on a one-off command buffer and submits it,
/// waiting the fence. The deformed buffer is host-visible (mapped), so no copy-out is
/// needed; a `SHADER_WRITE → HOST_READ` barrier orders the read after the dispatch.
fn dispatch_skin(
    device: &Device,
    pipeline: &Arc<crate::Pipeline>,
    set: vk::DescriptorSet,
    vertex_count: u32,
) {
    use ash::vk;
    let raw = device.resources().device();
    let pool_info =
        vk::CommandPoolCreateInfo::default().queue_family_index(device.graphics_queue_family);
    // SAFETY: the ash seam. Freed at the end.
    let pool = unsafe { raw.create_command_pool(&pool_info, None) }.expect("pool");
    let alloc = vk::CommandBufferAllocateInfo::default()
        .command_pool(pool)
        .level(vk::CommandBufferLevel::PRIMARY)
        .command_buffer_count(1);
    // SAFETY: the ash seam. One buffer from the pool.
    let cmd = unsafe { raw.allocate_command_buffers(&alloc) }.expect("cmd")[0];
    // SAFETY: the ash seam. Default fence.
    let fence = unsafe { raw.create_fence(&vk::FenceCreateInfo::default(), None) }.expect("fence");

    let begin =
        vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
    let push = SkinPush {
        vertex_count,
        joint_offset: 0,
        deformed_offset: 0,
        pad: 0,
    };
    // SAFETY: the ash seam. The PSO/set are valid; the dispatch covers the vertices;
    // the host-read barrier orders the mapped read after the compute write.
    unsafe {
        raw.begin_command_buffer(cmd, &begin).expect("begin");
        raw.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, pipeline.handle());
        raw.cmd_bind_descriptor_sets(
            cmd,
            vk::PipelineBindPoint::COMPUTE,
            pipeline.layout(),
            0,
            &[set],
            &[],
        );
        raw.cmd_push_constants(
            cmd,
            pipeline.layout(),
            vk::ShaderStageFlags::COMPUTE,
            0,
            bytemuck::bytes_of(&push),
        );
        raw.cmd_dispatch(cmd, vertex_count.div_ceil(64), 1, 1);
        let mem = vk::MemoryBarrier2::default()
            .src_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER)
            .src_access_mask(vk::AccessFlags2::SHADER_STORAGE_WRITE)
            .dst_stage_mask(vk::PipelineStageFlags2::HOST)
            .dst_access_mask(vk::AccessFlags2::HOST_READ);
        let barriers = [mem];
        let dep = vk::DependencyInfo::default().memory_barriers(&barriers);
        raw.cmd_pipeline_barrier2(cmd, &dep);
        raw.end_command_buffer(cmd).expect("end");
        let cmd_info = [vk::CommandBufferSubmitInfo::default().command_buffer(cmd)];
        let submit = [vk::SubmitInfo2::default().command_buffer_infos(&cmd_info)];
        device
            .graphics_queue
            .submit2(raw, &submit, fence, "submit")
            .expect("submit");
        raw.wait_for_fences(&[fence], true, u64::MAX).expect("wait");
        raw.destroy_fence(fence, None);
        raw.destroy_command_pool(pool, None);
    }
}
