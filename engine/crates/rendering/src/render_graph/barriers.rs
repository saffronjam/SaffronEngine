//! Barrier derivation: the per-usage stage/access/layout table, the declared-access
//! validation, and the resource-state transitions each pass produces. Pure logic on plain data,
//! so a missing or wrong barrier is caught by a test rather than by a data race.

use super::*;

/// The stage/access/layout/is-write contract for each usage — the load-bearing source
/// of truth.
pub(super) fn usage_info(usage: RgUsage) -> RgUsageInfo {
    match usage {
        RgUsage::ColorWrite => RgUsageInfo {
            stage: vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT,
            access: vk::AccessFlags2::COLOR_ATTACHMENT_WRITE,
            layout: vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
            is_write: true,
        },
        RgUsage::DepthWrite => RgUsageInfo {
            stage: vk::PipelineStageFlags2::EARLY_FRAGMENT_TESTS
                | vk::PipelineStageFlags2::LATE_FRAGMENT_TESTS,
            access: vk::AccessFlags2::DEPTH_STENCIL_ATTACHMENT_WRITE,
            layout: vk::ImageLayout::DEPTH_ATTACHMENT_OPTIMAL,
            is_write: true,
        },
        RgUsage::SampledRead => RgUsageInfo {
            stage: vk::PipelineStageFlags2::FRAGMENT_SHADER,
            access: vk::AccessFlags2::SHADER_SAMPLED_READ,
            layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            is_write: false,
        },
        RgUsage::StorageWriteCompute => RgUsageInfo {
            stage: vk::PipelineStageFlags2::COMPUTE_SHADER,
            access: vk::AccessFlags2::SHADER_STORAGE_WRITE,
            layout: vk::ImageLayout::UNDEFINED,
            is_write: true,
        },
        RgUsage::StorageReadCompute => RgUsageInfo {
            stage: vk::PipelineStageFlags2::COMPUTE_SHADER,
            access: vk::AccessFlags2::SHADER_STORAGE_READ,
            layout: vk::ImageLayout::UNDEFINED,
            is_write: false,
        },
        RgUsage::StorageReadFragment => RgUsageInfo {
            stage: vk::PipelineStageFlags2::FRAGMENT_SHADER,
            access: vk::AccessFlags2::SHADER_STORAGE_READ,
            layout: vk::ImageLayout::UNDEFINED,
            is_write: false,
        },
        RgUsage::StorageReadWriteCompute => RgUsageInfo {
            stage: vk::PipelineStageFlags2::COMPUTE_SHADER,
            access: vk::AccessFlags2::SHADER_STORAGE_READ | vk::AccessFlags2::SHADER_STORAGE_WRITE,
            layout: vk::ImageLayout::UNDEFINED,
            is_write: true,
        },
        RgUsage::StorageImageRwCompute => RgUsageInfo {
            stage: vk::PipelineStageFlags2::COMPUTE_SHADER,
            access: vk::AccessFlags2::SHADER_STORAGE_READ | vk::AccessFlags2::SHADER_STORAGE_WRITE,
            layout: vk::ImageLayout::GENERAL,
            is_write: true,
        },
        RgUsage::SampledReadCompute => RgUsageInfo {
            stage: vk::PipelineStageFlags2::COMPUTE_SHADER,
            access: vk::AccessFlags2::SHADER_SAMPLED_READ,
            layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            is_write: false,
        },
        RgUsage::TransferRead => RgUsageInfo {
            stage: vk::PipelineStageFlags2::COPY,
            access: vk::AccessFlags2::TRANSFER_READ,
            layout: vk::ImageLayout::UNDEFINED,
            is_write: false,
        },
        RgUsage::TransferWrite => RgUsageInfo {
            stage: vk::PipelineStageFlags2::COPY,
            access: vk::AccessFlags2::TRANSFER_WRITE,
            layout: vk::ImageLayout::UNDEFINED,
            is_write: true,
        },
        RgUsage::VertexInputRead => RgUsageInfo {
            stage: vk::PipelineStageFlags2::VERTEX_ATTRIBUTE_INPUT,
            access: vk::AccessFlags2::VERTEX_ATTRIBUTE_READ,
            layout: vk::ImageLayout::UNDEFINED,
            is_write: false,
        },
        RgUsage::AccelStructBuildRead => RgUsageInfo {
            stage: vk::PipelineStageFlags2::ACCELERATION_STRUCTURE_BUILD_KHR,
            access: vk::AccessFlags2::SHADER_READ,
            layout: vk::ImageLayout::UNDEFINED,
            is_write: false,
        },
        RgUsage::IndexInputRead => RgUsageInfo {
            stage: vk::PipelineStageFlags2::INDEX_INPUT,
            access: vk::AccessFlags2::INDEX_READ,
            layout: vk::ImageLayout::UNDEFINED,
            is_write: false,
        },
        RgUsage::ShaderDeviceAddressRead => RgUsageInfo {
            stage: vk::PipelineStageFlags2::ALL_COMMANDS,
            access: vk::AccessFlags2::SHADER_READ,
            layout: vk::ImageLayout::UNDEFINED,
            is_write: false,
        },
        // `DRAW_INDIRECT` is the stage where both indirect draw *and* indirect dispatch parameters
        // are consumed, per `VK_PIPELINE_STAGE_2_DRAW_INDIRECT_BIT`.
        RgUsage::IndirectCommandRead => RgUsageInfo {
            stage: vk::PipelineStageFlags2::DRAW_INDIRECT,
            access: vk::AccessFlags2::INDIRECT_COMMAND_READ,
            layout: vk::ImageLayout::UNDEFINED,
            is_write: false,
        },
        RgUsage::MeshExecutorCommandRead => RgUsageInfo {
            stage: vk::PipelineStageFlags2::DRAW_INDIRECT
                | vk::PipelineStageFlags2::MESH_SHADER_EXT,
            access: vk::AccessFlags2::INDIRECT_COMMAND_READ | vk::AccessFlags2::SHADER_STORAGE_READ,
            layout: vk::ImageLayout::UNDEFINED,
            is_write: false,
        },
        RgUsage::IndirectCountRead => RgUsageInfo {
            stage: vk::PipelineStageFlags2::DRAW_INDIRECT,
            access: vk::AccessFlags2::INDIRECT_COMMAND_READ,
            layout: vk::ImageLayout::UNDEFINED,
            is_write: false,
        },
    }
}

pub(super) fn required_buffer_usage(usage: RgUsage) -> Option<vk::BufferUsageFlags> {
    match usage {
        RgUsage::StorageWriteCompute
        | RgUsage::StorageReadCompute
        | RgUsage::StorageReadFragment
        | RgUsage::StorageReadWriteCompute => Some(vk::BufferUsageFlags::STORAGE_BUFFER),
        RgUsage::TransferRead => Some(vk::BufferUsageFlags::TRANSFER_SRC),
        RgUsage::TransferWrite => Some(vk::BufferUsageFlags::TRANSFER_DST),
        RgUsage::VertexInputRead => Some(vk::BufferUsageFlags::VERTEX_BUFFER),
        RgUsage::IndexInputRead => Some(vk::BufferUsageFlags::INDEX_BUFFER),
        RgUsage::ShaderDeviceAddressRead => Some(vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS),
        RgUsage::IndirectCommandRead | RgUsage::IndirectCountRead => {
            Some(vk::BufferUsageFlags::INDIRECT_BUFFER)
        }
        RgUsage::MeshExecutorCommandRead => {
            Some(vk::BufferUsageFlags::INDIRECT_BUFFER | vk::BufferUsageFlags::STORAGE_BUFFER)
        }
        RgUsage::AccelStructBuildRead => {
            Some(vk::BufferUsageFlags::ACCELERATION_STRUCTURE_BUILD_INPUT_READ_ONLY_KHR)
        }
        RgUsage::ColorWrite
        | RgUsage::DepthWrite
        | RgUsage::SampledRead
        | RgUsage::StorageImageRwCompute
        | RgUsage::SampledReadCompute => None,
    }
}

pub(super) fn validate_declared_access(resource: &RgResourceState, usage: RgUsage) {
    match required_buffer_usage(usage) {
        Some(required) => {
            assert!(
                !resource.is_image,
                "buffer usage declared for a graph image"
            );
            // An imported buffer's creation flags are the owner's contract — the
            // tracked usage only accumulates *declared* graph usages, so it cannot
            // prove a flag absent. Only graph-owned allocations are checkable.
            assert!(
                resource.buffer_lifetime == RgBufferLifetime::Imported
                    || resource.buffer_usage.is_empty()
                    || resource.buffer_usage.contains(required),
                "graph buffer was not allocated with the Vulkan usage required by {usage:?}"
            );
        }
        None => assert!(resource.is_image, "image usage declared for a graph buffer"),
    }
}

/// Seeds a freshly-imported image's source scope from its entry layout: a
/// `SHADER_READ_ONLY` image was last sampled by a fragment shader (the
/// write-after-read source), so the first write waits on that read. Any other
/// entry layout has no prior in-frame work to wait on.
pub(super) fn seed_image_state(r: &mut RgResourceState) {
    if r.layout == vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL {
        r.last_stage = vk::PipelineStageFlags2::FRAGMENT_SHADER;
        r.last_access = vk::AccessFlags2::SHADER_SAMPLED_READ;
    } else {
        r.last_stage = vk::PipelineStageFlags2::TOP_OF_PIPE;
        r.last_access = vk::AccessFlags2::empty();
    }
}

/// Pipeline stages only a graphics-capable queue can express.
pub(super) const GRAPHICS_ONLY_STAGES: vk::PipelineStageFlags2 = vk::PipelineStageFlags2::from_raw(
    vk::PipelineStageFlags2::DRAW_INDIRECT.as_raw()
        | vk::PipelineStageFlags2::VERTEX_INPUT.as_raw()
        | vk::PipelineStageFlags2::VERTEX_SHADER.as_raw()
        | vk::PipelineStageFlags2::TESSELLATION_CONTROL_SHADER.as_raw()
        | vk::PipelineStageFlags2::TESSELLATION_EVALUATION_SHADER.as_raw()
        | vk::PipelineStageFlags2::GEOMETRY_SHADER.as_raw()
        | vk::PipelineStageFlags2::FRAGMENT_SHADER.as_raw()
        | vk::PipelineStageFlags2::EARLY_FRAGMENT_TESTS.as_raw()
        | vk::PipelineStageFlags2::LATE_FRAGMENT_TESTS.as_raw()
        | vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT.as_raw()
        | vk::PipelineStageFlags2::ALL_GRAPHICS.as_raw()
        | vk::PipelineStageFlags2::INDEX_INPUT.as_raw()
        | vk::PipelineStageFlags2::VERTEX_ATTRIBUTE_INPUT.as_raw()
        | vk::PipelineStageFlags2::PRE_RASTERIZATION_SHADERS.as_raw()
        | vk::PipelineStageFlags2::MESH_SHADER_EXT.as_raw(),
);

/// The source scope of a same-queue barrier, expressed in terms `queue` can name.
///
/// An image imported for the first time carries an *assumed* source stage describing work
/// outside the graph — `FRAGMENT_SHADER` for one already in `SHADER_READ_ONLY_OPTIMAL`. It
/// has no recorded queue owner, so the barrier lands on whichever queue first touches it,
/// and a graphics stage named on the async-compute queue is invalid
/// (`VUID-vkCmdPipelineBarrier2-srcStageMask-09675`). Widen the whole scope to
/// `ALL_COMMANDS` + `MEMORY_READ|MEMORY_WRITE`: legal on every queue, and a superset of what
/// it replaces. A resource with a recorded owner never reaches this — a queue change goes
/// through the release/acquire pair instead, whose source scope rides its own queue.
pub(super) fn queue_source_scope(
    stage: vk::PipelineStageFlags2,
    access: vk::AccessFlags2,
    queue: RgQueueAssignment,
) -> (vk::PipelineStageFlags2, vk::AccessFlags2) {
    if queue == RgQueueAssignment::AsyncCompute && stage.intersects(GRAPHICS_ONLY_STAGES) {
        (
            vk::PipelineStageFlags2::ALL_COMMANDS,
            vk::AccessFlags2::MEMORY_READ | vk::AccessFlags2::MEMORY_WRITE,
        )
    } else {
        (stage, access)
    }
}

/// The barriers a single pass needs, derived from its declared usage.
#[derive(Default)]
pub(super) struct DerivedBarriers {
    pub(super) image: Vec<vk::ImageMemoryBarrier2<'static>>,
    pub(super) buffer: Vec<vk::BufferMemoryBarrier2<'static>>,
    pub(super) releases: Vec<RgQueueRelease>,
}

#[derive(Clone, Copy)]
pub(super) enum RgQueueReleaseBarrier {
    Image(vk::ImageMemoryBarrier2<'static>),
    Buffer(vk::BufferMemoryBarrier2<'static>),
    Dependency,
}

#[derive(Clone, Copy)]
pub(super) struct RgQueueRelease {
    pub(super) pass: Option<usize>,
    pub(super) source_family: u32,
    pub(super) source_queue: RgQueueAssignment,
    pub(super) barrier: RgQueueReleaseBarrier,
}

impl DerivedBarriers {
    #[cfg(test)]
    pub(super) fn is_empty(&self) -> bool {
        self.image.is_empty() && self.buffer.is_empty()
    }
}

/// Derives a barrier for one `(resource, usage)`, appends it to `barriers`, and
/// advances the resource state.
///
/// The hazard rule: a hazard exists when a write touches
/// an already-touched resource (write-after-anything) or a read follows a write
/// (read-after-write). Images barrier on a layout change *or* a hazard; buffers on
/// a hazard only. A read after a read with no layout change emits nothing.
pub(super) fn apply_access_queued(
    r: &mut RgResourceState,
    target: RgUsageInfo,
    range: Option<RgBufferRange>,
    pass: usize,
    queue: RgQueueAssignment,
    queue_family: u32,
    barriers: &mut DerivedBarriers,
) {
    if !r.is_image {
        apply_buffer_access(r, target, range, pass, queue, queue_family, barriers);
        return;
    }
    let hazard = (target.is_write && r.touched) || (!target.is_write && r.last_was_write);
    let layout_change = target.layout != vk::ImageLayout::UNDEFINED && r.layout != target.layout;
    let queue_change = r.queue.is_some_and(|previous| previous != queue);
    let ownership_transfer =
        queue_change && r.queue_family.is_some_and(|family| family != queue_family);
    let new_layout = if layout_change {
        target.layout
    } else {
        r.layout
    };
    let subresource_range = vk::ImageSubresourceRange {
        aspect_mask: r.aspect,
        base_mip_level: 0,
        level_count: vk::REMAINING_MIP_LEVELS,
        base_array_layer: 0,
        layer_count: vk::REMAINING_ARRAY_LAYERS,
    };
    if queue_change {
        let source_family = r
            .queue_family
            .expect("a queue-owned resource has a queue family");
        let release_barrier = if ownership_transfer {
            RgQueueReleaseBarrier::Image(
                vk::ImageMemoryBarrier2::default()
                    .src_stage_mask(r.last_stage)
                    .src_access_mask(r.last_access)
                    .dst_stage_mask(vk::PipelineStageFlags2::NONE)
                    .dst_access_mask(vk::AccessFlags2::empty())
                    .old_layout(r.layout)
                    .new_layout(new_layout)
                    .src_queue_family_index(source_family)
                    .dst_queue_family_index(queue_family)
                    .image(r.image)
                    .subresource_range(subresource_range),
            )
        } else {
            RgQueueReleaseBarrier::Dependency
        };
        if let Some(source_pass) = r.last_pass {
            barriers.releases.push(RgQueueRelease {
                pass: Some(source_pass),
                source_family,
                source_queue: r.queue.expect("queue change has a source queue"),
                barrier: release_barrier,
            });
        } else {
            barriers.releases.push(RgQueueRelease {
                pass: None,
                source_family,
                source_queue: r.queue.expect("queue change has a source queue"),
                barrier: release_barrier,
            });
        }
        if ownership_transfer || layout_change || hazard {
            barriers.image.push(
                vk::ImageMemoryBarrier2::default()
                    .src_stage_mask(vk::PipelineStageFlags2::NONE)
                    .src_access_mask(vk::AccessFlags2::empty())
                    .dst_stage_mask(target.stage)
                    .dst_access_mask(target.access)
                    .old_layout(r.layout)
                    .new_layout(new_layout)
                    .src_queue_family_index(if ownership_transfer {
                        source_family
                    } else {
                        vk::QUEUE_FAMILY_IGNORED
                    })
                    .dst_queue_family_index(if ownership_transfer {
                        queue_family
                    } else {
                        vk::QUEUE_FAMILY_IGNORED
                    })
                    .image(r.image)
                    .subresource_range(subresource_range),
            );
        }
    } else if layout_change || hazard {
        let (src_stage, src_access) = queue_source_scope(r.last_stage, r.last_access, queue);
        barriers.image.push(
            vk::ImageMemoryBarrier2::default()
                .src_stage_mask(src_stage)
                .src_access_mask(src_access)
                .dst_stage_mask(target.stage)
                .dst_access_mask(target.access)
                .old_layout(r.layout)
                .new_layout(new_layout)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(r.image)
                .subresource_range(subresource_range),
        );
    }
    if layout_change {
        r.layout = target.layout;
    }

    r.last_stage = target.stage;
    r.last_access = target.access;
    r.last_was_write = target.is_write;
    r.touched = true;
    r.queue_family = Some(queue_family);
    r.queue = Some(queue);
    r.last_pass = Some(pass);
}

pub(super) fn apply_buffer_access(
    r: &mut RgResourceState,
    target: RgUsageInfo,
    range: Option<RgBufferRange>,
    pass: usize,
    queue: RgQueueAssignment,
    queue_family: u32,
    barriers: &mut DerivedBarriers,
) {
    let (start, end) = buffer_range_bounds(r.buffer_size, range);
    let mut retained = Vec::with_capacity(r.buffer_accesses.len() + 1);
    let mut inherited_read_stage = target.stage;
    let mut inherited_read_access = target.access;

    for previous in r.buffer_accesses.drain(..) {
        let overlap_start = previous.start.max(start);
        let overlap_end = previous.end.min(end);
        if overlap_start >= overlap_end {
            retained.push(previous);
            continue;
        }

        if previous.start < overlap_start {
            retained.push(RgBufferAccessState {
                end: overlap_start,
                ..previous
            });
        }
        if overlap_end < previous.end {
            retained.push(RgBufferAccessState {
                start: overlap_end,
                ..previous
            });
        }

        let queue_change = previous.queue != queue;
        let ownership_transfer = queue_change && previous.queue_family != queue_family;
        let hazard = previous.is_write || target.is_write;
        let barrier_offset = overlap_start;
        let barrier_size = overlap_end - overlap_start;
        if queue_change {
            let release_barrier = if ownership_transfer {
                RgQueueReleaseBarrier::Buffer(
                    vk::BufferMemoryBarrier2::default()
                        .src_stage_mask(previous.stage)
                        .src_access_mask(previous.access)
                        .dst_stage_mask(vk::PipelineStageFlags2::NONE)
                        .dst_access_mask(vk::AccessFlags2::empty())
                        .src_queue_family_index(previous.queue_family)
                        .dst_queue_family_index(queue_family)
                        .buffer(r.buffer)
                        .offset(barrier_offset)
                        .size(barrier_size),
                )
            } else {
                RgQueueReleaseBarrier::Dependency
            };
            barriers.releases.push(RgQueueRelease {
                pass: previous.last_pass,
                source_family: previous.queue_family,
                source_queue: previous.queue,
                barrier: release_barrier,
            });
            if ownership_transfer || hazard {
                barriers.buffer.push(
                    vk::BufferMemoryBarrier2::default()
                        .src_stage_mask(vk::PipelineStageFlags2::NONE)
                        .src_access_mask(vk::AccessFlags2::empty())
                        .dst_stage_mask(target.stage)
                        .dst_access_mask(target.access)
                        .src_queue_family_index(if ownership_transfer {
                            previous.queue_family
                        } else {
                            vk::QUEUE_FAMILY_IGNORED
                        })
                        .dst_queue_family_index(if ownership_transfer {
                            queue_family
                        } else {
                            vk::QUEUE_FAMILY_IGNORED
                        })
                        .buffer(r.buffer)
                        .offset(barrier_offset)
                        .size(barrier_size),
                );
            }
        } else if hazard {
            barriers.buffer.push(
                vk::BufferMemoryBarrier2::default()
                    .src_stage_mask(previous.stage)
                    .src_access_mask(previous.access)
                    .dst_stage_mask(target.stage)
                    .dst_access_mask(target.access)
                    .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .buffer(r.buffer)
                    .offset(barrier_offset)
                    .size(barrier_size),
            );
        }

        if !target.is_write && !previous.is_write && !queue_change {
            inherited_read_stage |= previous.stage;
            inherited_read_access |= previous.access;
        }
    }

    retained.push(RgBufferAccessState {
        start,
        end,
        stage: inherited_read_stage,
        access: inherited_read_access,
        is_write: target.is_write,
        queue_family,
        queue,
        last_pass: Some(pass),
    });
    r.buffer_accesses = retained;
    r.last_stage = target.stage;
    r.last_access = target.access;
    r.last_was_write = target.is_write;
    r.touched = true;
}

pub(super) fn buffer_range_bounds(
    buffer_size: vk::DeviceSize,
    range: Option<RgBufferRange>,
) -> (vk::DeviceSize, vk::DeviceSize) {
    match range {
        Some(range) => {
            let end = range
                .offset
                .checked_add(range.size)
                .expect("RgBufferRange validates its end");
            assert!(
                buffer_size == vk::WHOLE_SIZE || end <= buffer_size,
                "render-graph buffer access exceeds its declared size"
            );
            (range.offset, end)
        }
        None => (0, buffer_size),
    }
}

pub(super) fn buffer_state_covers(
    resource: &RgResourceState,
    range: Option<RgBufferRange>,
) -> bool {
    let (start, end) = buffer_range_bounds(resource.buffer_size, range);
    let mut spans = resource
        .buffer_accesses
        .iter()
        .filter(|state| state.end > start && state.start < end)
        .map(|state| (state.start.max(start), state.end.min(end)))
        .collect::<Vec<_>>();
    spans.sort_unstable_by_key(|span| span.0);
    let mut cursor = start;
    for (span_start, span_end) in spans {
        if span_start > cursor {
            return false;
        }
        cursor = cursor.max(span_end);
        if cursor >= end {
            return true;
        }
    }
    false
}
