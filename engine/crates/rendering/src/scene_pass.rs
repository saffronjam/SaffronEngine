//! Recording the executor draws into the scene + depth-family command buffers.
//!
//! Every geometry pass replays the frame's binned counted-indirect commands over the
//! index stream each bucket's representation reads — the pages arena for page-resident
//! geometry, the displacement arena for a displaced bucket ([`record_executor_buckets`]
//! with per-bucket PSOs for the scene, [`record_executor_depth_family`] with one PSO for
//! the depth family). The sorted transparent streams replay through
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
    let Some((_, _, first)) = draws.iter().find(|(_, blend, _)| *blend == transparent) else {
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
    let mut recorded = 0;
    for (bucket_index, (bucket, blend, pso)) in draws.iter().enumerate() {
        if *blend != transparent {
            continue;
        }
        // The mesh PSOs declare dynamic cull: a double-sided bucket disables
        // backface culling, everything else culls BACK.
        // SAFETY: the ash seam; `cmd` is recording.
        unsafe {
            raw.cmd_bind_index_buffer(
                cmd,
                bucket_index_buffer(*bucket, page_index_buffer, inputs.displaced_indices),
                0,
                vk::IndexType::UINT32,
            );
        }
        let draw = crate::ExecutorBucketDraw {
            bucket: *bucket,
            index: bucket_index as u32,
            cull_mode: bucket_cull_mode(*bucket),
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

/// The index stream a draw bucket fetches through: the displacement arena for a displaced
/// bucket, the pages arena for every page-resident representation. Both are u32 streams, so
/// the bucket's counted-indirect draw is otherwise identical.
///
/// A view that reads the undisplaced surface (a shadow page, the GI reach walk) passes a null
/// `displaced_indices`; the arena is a per-frame allocation those views never bind, and a
/// displaced bucket there falls back to the pages arena.
pub fn bucket_index_buffer(
    bucket: crate::ExecutorBucket,
    page_index_buffer: vk::Buffer,
    displaced_indices: vk::Buffer,
) -> vk::Buffer {
    let displaced = (bucket.pso_bin >> crate::GPU_PSO_REPRESENTATION_SHIFT) & 0x3
        == crate::GpuRepresentation::DisplacedMicro as u32;
    if displaced && displaced_indices != vk::Buffer::null() {
        displaced_indices
    } else {
        page_index_buffer
    }
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

/// Records the depth-family executor draws (depth-prepass, the `vsm-pages` shadow
/// pass, G-buffer, motion, wireframe overlay, reactive coverage): one pipeline for
/// every selected bucket, sets 0 + 2 (their fragments never read set 1), the pass's
/// push bytes, then per-bucket counted indirect draws over the index stream
/// [`bucket_index_buffer`] selects. `transparent` selects which bucket class draws
/// (the reactive-coverage mask draws the blend buckets; every depth pass draws the
/// rest). Returns the recorded bucket count.
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
    }
    let mut recorded = 0;
    for (bucket_index, (bucket, blend, _)) in draws.iter().enumerate() {
        if *blend != transparent {
            continue;
        }
        // SAFETY: the ash seam, as above.
        unsafe {
            raw.cmd_bind_index_buffer(
                cmd,
                bucket_index_buffer(*bucket, page_index_buffer, inputs.displaced_indices),
                0,
                vk::IndexType::UINT32,
            );
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
    draw_indirect_count: bool,
    draws: &[(crate::ExecutorBucket, bool, std::sync::Arc<crate::Pipeline>)],
    mesh_dispatch: Option<&ash::ext::mesh_shader::Device>,
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
    // The slice stride stays the full record capacity — that is how the reorder pass addresses
    // each bucket's stream — while the draw count follows the records that exist.
    let draw_count = inputs.draw_bound.min(inputs.record_capacity);
    for (group_slot, (bucket, _, pso)) in blend.iter().enumerate() {
        let slice_base = crate::transparent_slice_base(inputs.record_capacity, group_slot as u32);
        // SAFETY: the ash seam. The sorted streams + their count were built this frame; each
        // bucket binds the index stream its representation reads.
        unsafe {
            raw.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, pso.handle());
            raw.cmd_set_cull_mode(cmd, bucket_cull_mode(*bucket));
            raw.cmd_bind_index_buffer(
                cmd,
                bucket_index_buffer(*bucket, page_index_buffer, inputs.displaced_indices),
                0,
                vk::IndexType::UINT32,
            );
        }
        let count_offset = (crate::SCENE_VISIBILITY_COUNTER_TRANSPARENT * 4) as u64;
        if let Some(dispatch) = mesh_dispatch {
            // `SV_DrawIndex` counts from zero within this slice, so the mesh entry needs the
            // slice's arena base to reach its own command — the sorted counterpart of the
            // per-bucket base the opaque scope pushes.
            // SAFETY: the ash seam. The range was declared to cover this offset; the sorted
            // mesh-task slice was written by the reorder pass this frame.
            unsafe {
                raw.cmd_push_constants(
                    cmd,
                    pso.layout(),
                    vk::ShaderStageFlags::MESH_EXT,
                    size_of::<Mat4>() as u32,
                    &slice_base.to_ne_bytes(),
                );
                let offset = u64::from(slice_base) * crate::MESH_TASK_COMMAND_STRIDE;
                let stride = crate::MESH_TASK_COMMAND_STRIDE as u32;
                if draw_indirect_count {
                    dispatch.cmd_draw_mesh_tasks_indirect_count(
                        cmd,
                        inputs.mesh_args,
                        offset,
                        inputs.counters,
                        count_offset,
                        draw_count,
                        stride,
                    );
                } else {
                    dispatch.cmd_draw_mesh_tasks_indirect(
                        cmd,
                        inputs.mesh_args,
                        offset,
                        draw_count,
                        stride,
                    );
                }
            }
        } else {
            let offset = u64::from(slice_base) * 20;
            // SAFETY: the ash seam, as above.
            unsafe {
                if draw_indirect_count {
                    raw.cmd_draw_indexed_indirect_count(
                        cmd,
                        inputs.commands,
                        offset,
                        inputs.counters,
                        count_offset,
                        draw_count,
                        20,
                    );
                } else {
                    raw.cmd_draw_indexed_indirect(cmd, inputs.commands, offset, draw_count, 20);
                }
            }
        }
    }
    blend.len() as u32
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
/// 6 + 7 add one each when present on an RT device. `0` when nothing draws.
#[must_use]
pub fn scene_pass_bind_count(
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
