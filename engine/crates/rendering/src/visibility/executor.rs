use std::sync::Arc;

use ash::vk;

use super::ExecutorBucket;

/// Bytes of one `VkDrawMeshTasksIndirectCommandEXT` (three u32 group counts).
pub const MESH_TASK_COMMAND_STRIDE: u64 = 12;

/// Triangles one mesh workgroup emits. Three vertices per triangle are emitted without
/// deduplication — a cluster carries no local vertex table, only a flat index range — so this
/// is bounded by the vertex limit rather than the primitive one: 62 x 3 = 186 output vertices,
/// inside the 256 every supported tier reports, and 2 x 62 covers a full 124-triangle cluster.
pub const MESH_TRIANGLES_PER_GROUP: u32 = 62;

/// Whether a device can run the shaded executor through its mesh stage.
///
/// Each term is a bit or limit the executor's own workgroup shape needs, not a blanket check on
/// the extension: `meshShader` may be advertised while `maxMeshOutputVertices` sits below the
/// 186 one group emits, and a driver may expose the extension for the task stage alone. The
/// dispatch bound is the spec's guaranteed 65535 — the scatter derives `groupCountX` from a
/// draw's index count, so a device below it cannot cover a large draw.
#[must_use]
pub fn mesh_executor_supported(capabilities: &crate::Capabilities) -> bool {
    capabilities.mesh_shader
        && capabilities.max_mesh_work_group_invocations >= MESH_TRIANGLES_PER_GROUP
        && capabilities.max_mesh_output_vertices >= MESH_TRIANGLES_PER_GROUP * 3
        && capabilities.max_mesh_output_primitives >= MESH_TRIANGLES_PER_GROUP
        && capabilities.max_mesh_work_group_count[0] >= 65_535
}

/// The command-arena slot one blend bucket's back-to-front slice starts at. The binner's
/// per-bucket slices occupy the first `record_capacity` slots and the sorted slices follow
/// them, so one arena feeds both scopes and the mesh executor reads either through the same
/// binding.
#[must_use]
pub fn transparent_slice_base(record_capacity: u32, group_slot: u32) -> u32 {
    record_capacity * (1 + group_slot)
}

/// The five transparent-sort pipelines, borrowed for one record call.
pub struct TransparentSortPipelines<'a> {
    /// Key collection over the record stream, and the per-level rekey over the pairs.
    pub keys: &'a Arc<crate::Pipeline>,
    /// Per-workgroup radix histograms.
    pub histogram: &'a Arc<crate::Pipeline>,
    /// The global exclusive scan over the histogram table.
    pub scan: &'a Arc<crate::Pipeline>,
    /// The stable radix scatter.
    pub scatter: &'a Arc<crate::Pipeline>,
    /// The reverse-order command reorder.
    pub reorder: &'a Arc<crate::Pipeline>,
}

/// Value captures for [`record_executor_bucket_draw`] inside a `'static` pass body.
#[derive(Clone, Copy)]
pub struct ExecutorDrawInputs {
    /// The frame slot's indirect command stream.
    pub commands: vk::Buffer,
    /// The frame slot's mesh-task dispatch arguments, parallel to `commands`.
    pub mesh_args: vk::Buffer,
    /// The frame slot's counters buffer (overflow/pressure words).
    pub counters: vk::Buffer,
    /// The frame slot's per-bucket count buffer (the draws' indirect counts).
    pub bucket_counts: vk::Buffer,
    /// The frame's amplified index stream, bound by a displaced draw bucket in place of the
    /// pages arena. Null when nothing displaces this frame.
    pub displaced_indices: vk::Buffer,
    /// Element capacity of the command stream.
    pub record_capacity: u32,
    /// Upper bound on the draws any one bucket slice can hold: the mirror's live draw-record
    /// count, clamped to the slice capacity.
    ///
    /// A device with `drawIndirectCount` reads the exact count on the GPU and treats this as a
    /// ceiling. A device without it must name a count host-side, and the slice capacity is the
    /// wrong one: it issues one command per *slot*, tens of thousands of no-op draws per frame
    /// whatever is on screen. Bounding by the records that actually exist keeps the pass
    /// proportional to the scene while never asking for fewer draws than the GPU wrote.
    pub draw_bound: u32,
}

/// Which bucket a recorder draws, where its count word sits, and whether the device can read
/// that count on the GPU. Shared by both executors so the two draw calls stay the same shape.
#[derive(Clone, Copy)]
pub struct ExecutorBucketDraw {
    /// The bucket's command-slice base and capacity.
    pub bucket: ExecutorBucket,
    /// Its index into the per-bucket count buffer.
    pub index: u32,
    /// Whether `drawIndirectCount` is available; without it the draw count is named host-side.
    pub draw_indirect_count: bool,
}

/// Issues one draw bucket's counted indirect draw into a live graphics pass body. The
/// caller binds the mesh set roster and the push once per pass; each bucket then binds
/// its PSO, the index stream its representation reads
/// ([`crate::bucket_index_buffer`]), and draws its command slice with the bucket's count
/// word (or the fixed-slice variant when the device lacks `drawIndirectCount`; unwritten
/// commands are zero-filled no-ops).
pub fn record_executor_bucket_draw(
    raw: &ash::Device,
    cmd: vk::CommandBuffer,
    pipeline: (vk::Pipeline, vk::PipelineLayout),
    inputs: ExecutorDrawInputs,
    draw: ExecutorBucketDraw,
) {
    let ExecutorBucketDraw {
        bucket,
        index: bucket_index,
        draw_indirect_count,
    } = draw;
    // SAFETY: the ash seam. The PSO/buffers are valid this frame; the indirect stream,
    // counts, and slice bases were built by the bucket passes this frame.
    unsafe {
        raw.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, pipeline.0);
        let draws = bucket.capacity.min(inputs.draw_bound);
        if draw_indirect_count {
            raw.cmd_draw_indexed_indirect_count(
                cmd,
                inputs.commands,
                u64::from(bucket.base) * 20,
                inputs.bucket_counts,
                u64::from(bucket_index) * 4,
                draws,
                20,
            );
        } else {
            raw.cmd_draw_indexed_indirect(
                cmd,
                inputs.commands,
                u64::from(bucket.base) * 20,
                draws,
                20,
            );
        }
    }
}

/// Issues one draw bucket's counted mesh-task dispatch — the mesh executor's counterpart to
/// [`record_executor_bucket_draw`], reading the *same* per-bucket count word against the
/// mesh-args stream the scatter filled beside the indexed commands. One workgroup covers
/// [`MESH_TRIANGLES_PER_GROUP`] triangles of one draw; the shader recovers which draw from
/// `DrawIndex` and which block from its group id.
pub fn record_executor_bucket_draw_mesh(
    raw: &ash::Device,
    dispatch: &ash::ext::mesh_shader::Device,
    cmd: vk::CommandBuffer,
    pipeline: (vk::Pipeline, vk::PipelineLayout),
    inputs: ExecutorDrawInputs,
    draw: ExecutorBucketDraw,
) {
    let ExecutorBucketDraw {
        bucket,
        index: bucket_index,
        draw_indirect_count,
    } = draw;
    // SAFETY: the ash seam. The PSO/buffers are valid this frame; the mesh-args stream, counts,
    // and slice bases were written by the bucket passes this frame.
    unsafe {
        raw.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, pipeline.0);
        let draws = bucket.capacity.min(inputs.draw_bound);
        if draw_indirect_count {
            dispatch.cmd_draw_mesh_tasks_indirect_count(
                cmd,
                inputs.mesh_args,
                u64::from(bucket.base) * MESH_TASK_COMMAND_STRIDE,
                inputs.bucket_counts,
                u64::from(bucket_index) * 4,
                draws,
                MESH_TASK_COMMAND_STRIDE as u32,
            );
        } else {
            dispatch.cmd_draw_mesh_tasks_indirect(
                cmd,
                inputs.mesh_args,
                u64::from(bucket.base) * MESH_TASK_COMMAND_STRIDE,
                draws,
                MESH_TASK_COMMAND_STRIDE as u32,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn qualifying() -> crate::Capabilities {
        crate::Capabilities {
            mesh_shader: true,
            max_mesh_work_group_invocations: 128,
            max_mesh_output_vertices: 256,
            max_mesh_output_primitives: 256,
            max_mesh_work_group_count: [65_535; 3],
            ..crate::Capabilities::default()
        }
    }

    #[test]
    fn a_qualifying_device_runs_the_mesh_executor() {
        assert!(mesh_executor_supported(&qualifying()));
    }

    #[test]
    fn each_limit_the_workgroup_needs_disqualifies_on_its_own() {
        type Disqualifier = (&'static str, fn(&mut crate::Capabilities));
        let cases: [Disqualifier; 5] = [
            ("meshShader", |caps| caps.mesh_shader = false),
            ("invocations", |caps| {
                caps.max_mesh_work_group_invocations = MESH_TRIANGLES_PER_GROUP - 1;
            }),
            ("output vertices", |caps| {
                caps.max_mesh_output_vertices = MESH_TRIANGLES_PER_GROUP * 3 - 1;
            }),
            ("output primitives", |caps| {
                caps.max_mesh_output_primitives = MESH_TRIANGLES_PER_GROUP - 1;
            }),
            ("dispatch bound", |caps| {
                caps.max_mesh_work_group_count[0] = 65_534;
            }),
        ];
        for (name, break_it) in cases {
            let mut caps = qualifying();
            break_it(&mut caps);
            assert!(
                !mesh_executor_supported(&caps),
                "a device short on {name} must stay on the indexed path"
            );
        }
    }

    #[test]
    fn the_task_stage_alone_does_not_qualify() {
        let caps = crate::Capabilities {
            mesh_shader: false,
            task_shader: true,
            ..qualifying()
        };
        assert!(!mesh_executor_supported(&caps));
    }
}
