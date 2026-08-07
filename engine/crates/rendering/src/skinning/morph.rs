//! The blend-shape morph pre-pass: the fixed-point accumulator, its set wiring, and the
//! dispatch recording that resolves it into the deformed stream.

use super::*;

/// The morph kernel's 20-byte push constant, matching `morph.slang`'s `Push`
/// (`vertexCount / scatterCount / activeCount / deformedOffset / pass`).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct MorphPush {
    pub(super) vertex_count: u32,
    pub(super) scatter_count: u32,
    pub(super) active_count: u32,
    pub(super) active_base: u32,
    pub(super) deformed_offset: u32,
    pub(super) pass: u32,
}

/// The morph set layout: six compute-stage storage buffers (base vertices, deltas, target
/// ranges, active targets, accumulator, deformed output) matching `morph.slang` bindings 0-5.
pub(super) fn create_morph_set_layout(raw: &ash::Device) -> Result<vk::DescriptorSetLayout> {
    let bindings: Vec<vk::DescriptorSetLayoutBinding> = (0..6)
        .map(|b| {
            vk::DescriptorSetLayoutBinding::default()
                .binding(b)
                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::COMPUTE)
        })
        .collect();
    let info = vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings);
    // SAFETY: the ash seam.
    checked(
        unsafe { raw.create_descriptor_set_layout(&info, None) },
        "morphSetLayout",
    )
}

/// Allocates the morph scatter scratch buffer: `ACCUM_STRIDE` (6 × `i32`) per vertex,
/// `STORAGE` only.
pub(super) fn make_accum_buffer(resources: &Arc<DeviceResources>, capacity: u32) -> Result<Buffer> {
    let size = u64::from(capacity) * ACCUM_STRIDE;
    let alloc_info = vk_mem::AllocationCreateInfo {
        usage: vk_mem::MemoryUsage::AutoPreferDevice,
        ..Default::default()
    };
    Buffer::new(
        resources,
        size,
        vk::BufferUsageFlags::STORAGE_BUFFER,
        &alloc_info,
    )
}

/// Allocates one morph descriptor set from `pool` and writes its six storage-buffer
/// bindings (base vertices, deltas, ranges, active targets, accumulator, deformed output).
/// Returns `None` if the mesh has no morph buffers or the allocation fails (logged).
#[allow(clippy::too_many_arguments)]
pub fn wire_morph_set(
    raw: &ash::Device,
    pool: vk::DescriptorPool,
    layout: vk::DescriptorSetLayout,
    mesh: &GpuMesh,
    active: vk::Buffer,
    active_size: vk::DeviceSize,
    accum: vk::Buffer,
    accum_size: vk::DeviceSize,
    deformed: vk::Buffer,
    deformed_size: vk::DeviceSize,
) -> Option<vk::DescriptorSet> {
    let morph = mesh.morph()?;
    let layouts = [layout];
    let info = vk::DescriptorSetAllocateInfo::default()
        .descriptor_pool(pool)
        .set_layouts(&layouts);
    // SAFETY: the ash seam. The layout outlives the call; the set lives until the pool is
    // reset (next frame) or destroyed.
    let set = match unsafe { raw.allocate_descriptor_sets(&info) } {
        Ok(sets) => sets[0],
        Err(result) => {
            tracing::error!("morph: allocate morph set failed: {result:?}");
            return None;
        }
    };
    let infos = [
        (mesh.vertex_buffer(), vk::WHOLE_SIZE),
        (morph.deltas.0, vk::WHOLE_SIZE),
        (morph.ranges.0, vk::WHOLE_SIZE),
        (active, active_size),
        (accum, accum_size),
        (deformed, deformed_size),
    ];
    let buffer_infos: Vec<vk::DescriptorBufferInfo> = infos
        .iter()
        .map(|&(buffer, range)| vk::DescriptorBufferInfo {
            buffer,
            offset: 0,
            range,
        })
        .collect();
    let writes: Vec<vk::WriteDescriptorSet> = (0..6)
        .map(|b| {
            vk::WriteDescriptorSet::default()
                .dst_set(set)
                .dst_binding(b as u32)
                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                .buffer_info(std::slice::from_ref(&buffer_infos[b]))
        })
        .collect();
    // SAFETY: the ash seam. The set + buffers outlive the call; each write targets a single
    // binding the layout declares.
    unsafe { raw.update_descriptor_sets(&writes, &[]) };
    Some(set)
}

/// Emits a compute→compute memory barrier over the morph accumulator so the next pass (or
/// the next instance's clear) sees the prior pass's writes — the three morph passes and
/// successive instances share the accumulator region.
pub(super) fn morph_accum_barrier(raw: &ash::Device, cmd: vk::CommandBuffer) {
    let barrier = vk::MemoryBarrier2::default()
        .src_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER)
        .src_access_mask(
            vk::AccessFlags2::SHADER_STORAGE_WRITE | vk::AccessFlags2::SHADER_STORAGE_READ,
        )
        .dst_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER)
        .dst_access_mask(
            vk::AccessFlags2::SHADER_STORAGE_WRITE | vk::AccessFlags2::SHADER_STORAGE_READ,
        );
    let deps = vk::DependencyInfo::default().memory_barriers(std::slice::from_ref(&barrier));
    // SAFETY: the ash seam. A global memory barrier on the bound command buffer.
    unsafe { raw.cmd_pipeline_barrier2(cmd, &deps) };
}

/// Replays the frame's morph dispatches on `cmd`: bind the morph PSO, then per instance run
/// the three passes (clear → scatter → resolve), inserting an accumulator barrier between
/// passes and after each instance (the accumulator region is shared and reused serially).
/// The cur dispatches deform the current weights into the deformed buffer; the prev
/// dispatches deform the previous-frame weights into the prev-deformed buffer (read by the
/// motion pass) — the kernel is identical, only each set's output buffer differs.
pub fn record_morph(
    raw: &ash::Device,
    cmd: vk::CommandBuffer,
    pipeline: vk::Pipeline,
    layout: vk::PipelineLayout,
    dispatches: &[MorphDispatch],
    prev_dispatches: &[MorphDispatch],
) {
    if dispatches.is_empty() {
        return;
    }
    // SAFETY: the ash seam. The PSO is valid this frame.
    unsafe { raw.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, pipeline) };
    for d in dispatches.iter().chain(prev_dispatches) {
        if d.set == vk::DescriptorSet::null() {
            continue;
        }
        // SAFETY: the ash seam. The set wires the instance's buffers; each push spans the
        // declared 24-byte range; the dispatch covers the relevant count (64 per group).
        unsafe {
            raw.cmd_bind_descriptor_sets(
                cmd,
                vk::PipelineBindPoint::COMPUTE,
                layout,
                0,
                &[d.set],
                &[],
            );
        }
        for pass in 0u32..3 {
            let push = MorphPush {
                vertex_count: d.vertex_count,
                scatter_count: d.scatter_count,
                active_count: d.active_count,
                active_base: d.active_base,
                deformed_offset: d.deformed_offset,
                pass,
            };
            let groups = if pass == 1 {
                d.scatter_count.div_ceil(64)
            } else {
                d.vertex_count.div_ceil(64)
            };
            // SAFETY: the ash seam. As above.
            unsafe {
                raw.cmd_push_constants(
                    cmd,
                    layout,
                    vk::ShaderStageFlags::COMPUTE,
                    0,
                    bytemuck::bytes_of(&push),
                );
                if groups > 0 {
                    raw.cmd_dispatch(cmd, groups, 1, 1);
                }
            }
            morph_accum_barrier(raw, cmd);
        }
    }
}
