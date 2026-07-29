//! Recording the executor draws into the scene + depth-family command buffers.
//!
//! Every geometry pass replays the frame's binned counted-indirect commands over the
//! pages-arena index stream ([`record_executor_buckets`] with per-bucket PSOs for the
//! scene, [`record_executor_depth_family`] with one PSO for the depth family), then
//! the tessellation seam's amplified draws ([`record_tess_scene_draws`] /
//! [`record_tess_depth_draws`]). The sorted transparent streams replay through
//! [`record_executor_transparent_stream`].

use ash::vk;
use saffron_geometry::glam::Mat4;

/// Records the indexed-MDI executor draws for one geometry pass: the mesh descriptor
/// roster + viewProj push bound once (all executor mesh PSOs share the mesh layout),
/// the pages arena bound as the index buffer, then each opaque/masked bucket's counted
/// indirect draw with its PSO. `transparent` selects which bucket class draws (the
/// transparent scope replays the sorted stream separately). Returns the recorded
/// draw-bucket count.
#[allow(clippy::too_many_arguments)]
pub fn record_executor_buckets(
    raw: &ash::Device,
    cmd: vk::CommandBuffer,
    view_proj: Mat4,
    sets: MeshPassSets,
    inputs: crate::ExecutorDrawInputs,
    page_index_buffer: vk::Buffer,
    draw_indirect_count: bool,
    draws: &[(crate::ExecutorBucket, bool, std::sync::Arc<crate::Pipeline>)],
    transparent: bool,
    mesh_dispatch: Option<&ash::ext::mesh_shader::Device>,
) -> u32 {
    let live: Vec<_> = draws
        .iter()
        .filter(|(_, blend, _)| *blend == transparent)
        .collect();
    let Some((_, _, first)) = live.first() else {
        return 0;
    };
    bind_mesh_descriptor_sets(
        raw,
        cmd,
        first.layout(),
        view_proj,
        if mesh_dispatch.is_some() {
            vk::ShaderStageFlags::MESH_EXT
        } else {
            vk::ShaderStageFlags::VERTEX
        },
        sets.bindless,
        sets.light,
        sets.instance,
        sets.ibl,
        sets.ssao_mesh,
        sets.ddgi_mesh,
        sets.rt_mesh,
        sets.restir_mesh,
    );
    // SAFETY: the ash seam. The pages arena is the executor's index stream this frame.
    unsafe {
        raw.cmd_bind_index_buffer(cmd, page_index_buffer, 0, vk::IndexType::UINT32);
    }
    let mut recorded = 0;
    for (bucket_index, (bucket, _, pso)) in draws.iter().enumerate() {
        if draws[bucket_index].1 != transparent {
            continue;
        }
        // The mesh PSOs declare dynamic cull: a double-sided bucket disables
        // backface culling, everything else culls BACK.
        // SAFETY: the ash seam; `cmd` is recording.
        unsafe {
            raw.cmd_set_cull_mode(cmd, bucket_cull_mode(*bucket));
        }
        let draw = crate::ExecutorBucketDraw {
            bucket: *bucket,
            index: bucket_index as u32,
            draw_indirect_count,
        };
        if let Some(dispatch) = mesh_dispatch {
            // The mesh entry recovers its draw from `SV_DrawIndex`, which counts from zero
            // within this bucket's slice — so the slice base rides the push, updated per
            // bucket at the tail of the shared viewProj block.
            // SAFETY: the ash seam. The range was declared to cover this offset.
            unsafe {
                raw.cmd_push_constants(
                    cmd,
                    pso.layout(),
                    vk::ShaderStageFlags::MESH_EXT,
                    size_of::<Mat4>() as u32,
                    &bucket.base.to_ne_bytes(),
                );
            }
            crate::record_executor_bucket_draw_mesh(
                raw,
                dispatch,
                cmd,
                (pso.handle(), pso.layout()),
                inputs,
                draw,
            );
        } else {
            crate::record_executor_bucket_draw(
                raw,
                cmd,
                (pso.handle(), pso.layout()),
                inputs,
                draw,
            );
        }
        recorded += 1;
    }
    recorded
}

/// The dynamic cull mode a draw bucket's material class selects: `NONE` for a
/// double-sided class, `BACK` otherwise.
fn bucket_cull_mode(bucket: crate::ExecutorBucket) -> vk::CullModeFlags {
    let class = crate::GpuMaterialClass::from_bits(
        (bucket.pso_bin >> crate::GPU_PSO_MATERIAL_SHIFT) & 0x3f,
    )
    .unwrap_or_default();
    if class.sidedness() == crate::GpuSidedness::Double {
        vk::CullModeFlags::NONE
    } else {
        vk::CullModeFlags::BACK
    }
}

/// Records the depth-family executor draws (depth-prepass, shadow, point-shadow,
/// G-buffer, motion, wireframe overlay, reactive coverage): ONE pipeline for every
/// selected bucket, sets 0 + 2 (their fragments never read set 1), the pass's push
/// bytes, the pages arena as the index stream, then per-bucket counted indirect
/// draws. `transparent` selects which bucket class draws (the reactive-coverage mask
/// draws the blend buckets; every depth pass draws the rest). Returns the recorded
/// bucket count.
#[allow(clippy::too_many_arguments)]
pub fn record_executor_depth_family(
    raw: &ash::Device,
    cmd: vk::CommandBuffer,
    pipeline: (vk::Pipeline, vk::PipelineLayout),
    push_stages: vk::ShaderStageFlags,
    push_bytes: &[u8],
    bindless_set: vk::DescriptorSet,
    instance_set: vk::DescriptorSet,
    inputs: crate::ExecutorDrawInputs,
    page_index_buffer: vk::Buffer,
    draw_indirect_count: bool,
    draws: &[(crate::ExecutorBucket, bool, std::sync::Arc<crate::Pipeline>)],
    transparent: bool,
) -> u32 {
    if draws.iter().all(|(_, blend, _)| *blend != transparent) {
        return 0;
    }
    // SAFETY: the ash seam. The PSO/sets/buffers are valid this frame; the push spans
    // the pass's declared range; the indirect stream and counts were built by the
    // bucket passes this frame.
    unsafe {
        raw.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, pipeline.0);
        raw.cmd_bind_descriptor_sets(
            cmd,
            vk::PipelineBindPoint::GRAPHICS,
            pipeline.1,
            0,
            &[bindless_set],
            &[],
        );
        raw.cmd_bind_descriptor_sets(
            cmd,
            vk::PipelineBindPoint::GRAPHICS,
            pipeline.1,
            2,
            &[instance_set],
            &[],
        );
        raw.cmd_push_constants(cmd, pipeline.1, push_stages, 0, push_bytes);
        raw.cmd_bind_index_buffer(cmd, page_index_buffer, 0, vk::IndexType::UINT32);
    }
    let mut recorded = 0;
    for (bucket_index, (bucket, blend, _)) in draws.iter().enumerate() {
        if *blend != transparent {
            continue;
        }
        // SAFETY: the ash seam, as above.
        unsafe {
            if draw_indirect_count {
                raw.cmd_draw_indexed_indirect_count(
                    cmd,
                    inputs.commands,
                    u64::from(bucket.base) * 20,
                    inputs.bucket_counts,
                    u64::from(bucket_index as u32) * 4,
                    bucket.capacity,
                    20,
                );
            } else {
                raw.cmd_draw_indexed_indirect(
                    cmd,
                    inputs.commands,
                    u64::from(bucket.base) * 20,
                    bucket.capacity,
                    20,
                );
            }
        }
        recorded += 1;
    }
    recorded
}

/// Records the sorted transparent streams: the mesh descriptor roster + viewProj push,
/// the pages arena as the index stream, then per blend bucket (in bucket order — the
/// reorder kernel's group order) its blend PSO over that bucket's full-length
/// back-to-front command slice, counted by counter word 5 (other buckets' slots are
/// zero-masked). Binds its own roster so intervening submit-seam closures cannot
/// clobber its state. Returns the recorded blend-bucket count.
#[allow(clippy::too_many_arguments)]
pub fn record_executor_transparent_stream(
    raw: &ash::Device,
    cmd: vk::CommandBuffer,
    view_proj: Mat4,
    sets: MeshPassSets,
    inputs: crate::ExecutorDrawInputs,
    page_index_buffer: vk::Buffer,
    transparent_commands: vk::Buffer,
    draw_indirect_count: bool,
    draws: &[(crate::ExecutorBucket, bool, std::sync::Arc<crate::Pipeline>)],
) -> u32 {
    let blend: Vec<_> = draws.iter().filter(|(_, blend, _)| *blend).collect();
    let Some((_, _, first)) = blend.first() else {
        return 0;
    };
    bind_mesh_descriptor_sets(
        raw,
        cmd,
        first.layout(),
        view_proj,
        vk::ShaderStageFlags::VERTEX,
        sets.bindless,
        sets.light,
        sets.instance,
        sets.ibl,
        sets.ssao_mesh,
        sets.ddgi_mesh,
        sets.rt_mesh,
        sets.restir_mesh,
    );
    // SAFETY: the ash seam. The sorted streams + their count were built this frame;
    // the pages arena is the executor's index stream.
    unsafe {
        raw.cmd_bind_index_buffer(cmd, page_index_buffer, 0, vk::IndexType::UINT32);
    }
    // The slice stride stays the full record capacity — that is how the reorder pass addresses
    // each bucket's stream — while the draw count follows the records that exist.
    let draws = inputs.draw_bound.min(inputs.record_capacity);
    for (group_slot, (bucket, _, pso)) in blend.iter().enumerate() {
        let offset = group_slot as u64 * u64::from(inputs.record_capacity) * 20;
        // SAFETY: the ash seam, as above.
        unsafe {
            raw.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, pso.handle());
            raw.cmd_set_cull_mode(cmd, bucket_cull_mode(*bucket));
            if draw_indirect_count {
                raw.cmd_draw_indexed_indirect_count(
                    cmd,
                    transparent_commands,
                    offset,
                    inputs.counters,
                    (crate::SCENE_VISIBILITY_COUNTER_TRANSPARENT * 4) as u64,
                    draws,
                    20,
                );
            } else {
                raw.cmd_draw_indexed_indirect(cmd, transparent_commands, offset, draws, 20);
            }
        }
    }
    blend.len() as u32
}

/// Records the tessellation seam's scene draws: per displaced instance of the selected
/// blend class, its mesh PSO (vertex input over the amplified stream), dynamic cull,
/// its transient VB/IB, and the GPU-seeded indirect command. The roster + viewProj
/// push (re)bind through the first draw's layout, so the scope is self-contained.
/// Returns the recorded draw count.
pub fn record_tess_scene_draws(
    raw: &ash::Device,
    cmd: vk::CommandBuffer,
    view_proj: Mat4,
    sets: MeshPassSets,
    draws: &[crate::TessSceneDraw],
    transparent: bool,
) -> u32 {
    let live: Vec<_> = draws
        .iter()
        .filter(|draw| draw.blend == transparent && draw.draw.is_some())
        .collect();
    let Some(first) = live.first() else {
        return 0;
    };
    bind_mesh_descriptor_sets(
        raw,
        cmd,
        first.pso.layout(),
        view_proj,
        vk::ShaderStageFlags::VERTEX,
        sets.bindless,
        sets.light,
        sets.instance,
        sets.ibl,
        sets.ssao_mesh,
        sets.ddgi_mesh,
        sets.rt_mesh,
        sets.restir_mesh,
    );
    for draw in &live {
        let handles = draw.draw.expect("filtered above");
        // SAFETY: the ash seam. The PSO declares dynamic cull; the transient VB/IB +
        // args are pinned by the frame's `RenderGraphResources` until this slot's
        // fence; `cmd` is recording inside the pass's rendering scope.
        unsafe {
            raw.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, draw.pso.handle());
            raw.cmd_set_cull_mode(cmd, draw.cull);
            raw.cmd_bind_vertex_buffers(cmd, 0, &[handles.vertex_buffer], &[0]);
            raw.cmd_bind_index_buffer(cmd, handles.index_buffer, 0, vk::IndexType::UINT32);
            raw.cmd_draw_indexed_indirect(cmd, handles.args_buffer, handles.args_offset, 1, 20);
        }
    }
    live.len() as u32
}

/// Records the tessellation seam's depth-family draws: the pass's vertex-input PSO
/// once (sets 0/2 + the pass's push bytes), then per non-blend displaced instance its
/// transient VB/IB + GPU-seeded indirect command. `motion` also binds the previous
/// micro-vertex stream on binding 1 (the motion PSO's prev-position input). Returns
/// the recorded draw count.
#[allow(clippy::too_many_arguments)]
pub fn record_tess_depth_draws(
    raw: &ash::Device,
    cmd: vk::CommandBuffer,
    pipeline: (vk::Pipeline, vk::PipelineLayout),
    push_stages: vk::ShaderStageFlags,
    push_bytes: &[u8],
    bindless_set: vk::DescriptorSet,
    instance_set: vk::DescriptorSet,
    draws: &[crate::TessSceneDraw],
    motion: bool,
) -> u32 {
    let live: Vec<_> = draws
        .iter()
        .filter(|draw| !draw.blend && draw.draw.is_some())
        .collect();
    if live.is_empty() {
        return 0;
    }
    // SAFETY: the ash seam. The PSO/sets are valid this frame; the push spans the
    // pass's declared range.
    unsafe {
        raw.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, pipeline.0);
        raw.cmd_bind_descriptor_sets(
            cmd,
            vk::PipelineBindPoint::GRAPHICS,
            pipeline.1,
            0,
            &[bindless_set],
            &[],
        );
        raw.cmd_bind_descriptor_sets(
            cmd,
            vk::PipelineBindPoint::GRAPHICS,
            pipeline.1,
            2,
            &[instance_set],
            &[],
        );
        raw.cmd_push_constants(cmd, pipeline.1, push_stages, 0, push_bytes);
    }
    for draw in &live {
        let handles = draw.draw.expect("filtered above");
        // SAFETY: the ash seam. The transient VB/IB + args are pinned by the frame's
        // `RenderGraphResources` until this slot's fence; `cmd` is recording.
        unsafe {
            if motion {
                raw.cmd_bind_vertex_buffers(
                    cmd,
                    0,
                    &[handles.vertex_buffer, handles.prev_vertex_buffer],
                    &[0, 0],
                );
            } else {
                raw.cmd_bind_vertex_buffers(cmd, 0, &[handles.vertex_buffer], &[0]);
            }
            raw.cmd_bind_index_buffer(cmd, handles.index_buffer, 0, vk::IndexType::UINT32);
            raw.cmd_draw_indexed_indirect(cmd, handles.args_buffer, handles.args_offset, 1, 20);
        }
    }
    live.len() as u32
}

/// The mesh pass's full descriptor-set roster for one executor pass record.
#[derive(Clone, Copy)]
pub struct MeshPassSets {
    /// Set 0: the bindless texture array.
    pub bindless: vk::DescriptorSet,
    /// Set 1: the light set.
    pub light: vk::DescriptorSet,
    /// Set 2: the instance set (records + address block ride bindings 3/4).
    pub instance: vk::DescriptorSet,
    /// Set 3: the IBL set.
    pub ibl: vk::DescriptorSet,
    /// Set 4: the screen-space maps.
    pub ssao_mesh: vk::DescriptorSet,
    /// Set 5: the DDGI atlases.
    pub ddgi_mesh: vk::DescriptorSet,
    /// Set 6: the TLAS (RT devices; null otherwise).
    pub rt_mesh: vk::DescriptorSet,
    /// Set 7: the ReSTIR radiance (RT devices; null otherwise).
    pub restir_mesh: vk::DescriptorSet,
}

/// The number of descriptor-set bind operations the scene pass records this frame —
/// constant in the draw count.
///
/// Sets 0, {1,2}, 3, 4, 5 are five bind operations that hold regardless of draw count
/// (bindless textures + per-record indices keep the path O(1) in draws); the RT sets
/// 6 + 7 add one each when present on an RT device. `0` when nothing draws (no draws,
/// no binds). The single source of truth for the renderer's `render-stats` accounting.
#[must_use]
pub fn scene_draw_list_bind_count(
    has_draws: bool,
    rt_mesh_set: vk::DescriptorSet,
    restir_mesh_set: vk::DescriptorSet,
) -> u32 {
    if !has_draws {
        return 0;
    }
    let mut binds = 5u32;
    if rt_mesh_set != vk::DescriptorSet::null() {
        binds += 1;
    }
    if restir_mesh_set != vk::DescriptorSet::null() {
        binds += 1;
    }
    binds
}

/// Binds the mesh descriptor sets (0, {1,2}, 3, 4, 5, and 6/7 when present) and pushes the
/// viewProj constant — the constant prefix both the opaque and translucent scopes share
/// (see [`MeshPassSets`] for the roster).
#[allow(clippy::too_many_arguments)]
fn bind_mesh_descriptor_sets(
    raw: &ash::Device,
    cmd: vk::CommandBuffer,
    layout: vk::PipelineLayout,
    view_proj: saffron_geometry::glam::Mat4,
    push_stage: vk::ShaderStageFlags,
    bindless_set: vk::DescriptorSet,
    light_set: vk::DescriptorSet,
    instance_set: vk::DescriptorSet,
    ibl_set: vk::DescriptorSet,
    ssao_mesh_set: vk::DescriptorSet,
    ddgi_mesh_set: vk::DescriptorSet,
    rt_mesh_set: vk::DescriptorSet,
    restir_mesh_set: vk::DescriptorSet,
) {
    let view_proj = bytemuck::bytes_of(&view_proj);
    // SAFETY: the ash seam. The sets/layout belong to this frame; the push spans the
    // declared vertex range. Sets 1 + 2 bind in one call (consecutive sets). Set 3 = IBL
    // (irradiance + prefiltered + BRDF LUT + reflection probes), baked once, always valid.
    unsafe {
        raw.cmd_bind_descriptor_sets(
            cmd,
            vk::PipelineBindPoint::GRAPHICS,
            layout,
            0,
            &[bindless_set],
            &[],
        );
        raw.cmd_bind_descriptor_sets(
            cmd,
            vk::PipelineBindPoint::GRAPHICS,
            layout,
            1,
            &[light_set, instance_set],
            &[],
        );
        raw.cmd_bind_descriptor_sets(
            cmd,
            vk::PipelineBindPoint::GRAPHICS,
            layout,
            3,
            &[ibl_set],
            &[],
        );
        // Set 4 = screen-space AO + contact + SSGI samplers, bound when the chain is
        // built (the maps are neutral init-transitioned targets when the effects are off).
        if ssao_mesh_set != vk::DescriptorSet::null() {
            raw.cmd_bind_descriptor_sets(
                cmd,
                vk::PipelineBindPoint::GRAPHICS,
                layout,
                4,
                &[ssao_mesh_set],
                &[],
            );
        }
        // Set 5 = the DDGI irradiance + distance atlas samplers, bound when the volume's
        // resources are built. The mesh fragment statically references set 5 (the atlases
        // are the neutral init-transitioned targets when DDGI is off), so bind it whenever
        // present; the sample is gated in-shader by the DDGI `screen_flags.z` flag.
        if ddgi_mesh_set != vk::DescriptorSet::null() {
            raw.cmd_bind_descriptor_sets(
                cmd,
                vk::PipelineBindPoint::GRAPHICS,
                layout,
                5,
                &[ddgi_mesh_set],
                &[],
            );
        }
        // Set 6 = the ray-tracing TLAS, present only on an RT device (the mesh PSO layout
        // includes it then). The mesh fragment statically binds it for inline ray-query
        // shadows, gated at runtime by the `rtShadows` flag; an unbound set would be a
        // validation error, so bind it whenever the layout has it.
        if rt_mesh_set != vk::DescriptorSet::null() {
            raw.cmd_bind_descriptor_sets(
                cmd,
                vk::PipelineBindPoint::GRAPHICS,
                layout,
                6,
                &[rt_mesh_set],
                &[],
            );
        }
        // Set 7 = the ReSTIR resolved-radiance sampler, present only on an RT device and
        // bound only when ReSTIR ran this frame; the mesh fragment then samples the resolved
        // direct radiance instead of the clustered-forward direct term. `null` otherwise.
        if restir_mesh_set != vk::DescriptorSet::null() {
            raw.cmd_bind_descriptor_sets(
                cmd,
                vk::PipelineBindPoint::GRAPHICS,
                layout,
                7,
                &[restir_mesh_set],
                &[],
            );
        }
        raw.cmd_push_constants(cmd, layout, push_stage, 0, view_proj);
    }
}
