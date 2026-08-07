use super::*;

impl Renderer {
    /// Appends one compute pass: declare the `(resource, usage)` accesses (the graph derives the
    /// GENERAL ↔ ShaderReadOnly transitions), bind the set, optionally push `push`, and dispatch
    /// `(groups_x, groups_y, groups_z)`.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn add_compute_pass(
        &self,
        graph: &mut RenderGraph,
        name: &'static str,
        pipeline: &Arc<crate::Pipeline>,
        set: vk::DescriptorSet,
        accesses: &[(RgResource, RgUsage)],
        push: Option<Vec<u8>>,
        groups_x: u32,
        groups_y: u32,
        groups_z: u32,
    ) {
        let raw_body = self.device.raw().clone();
        let pipeline = Arc::clone(pipeline);
        let handle = pipeline.handle();
        let layout = pipeline.layout();
        let mut pass = RgPass::compute(name).body(move |cmd, _scopes: &mut NestedScopeRecorder| {
            // SAFETY: the ash seam. The PSO/set are valid this frame.
            unsafe {
                raw_body.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, handle);
                raw_body.cmd_bind_descriptor_sets(
                    cmd,
                    vk::PipelineBindPoint::COMPUTE,
                    layout,
                    0,
                    &[set],
                    &[],
                );
                if let Some(push) = &push {
                    raw_body.cmd_push_constants(
                        cmd,
                        layout,
                        vk::ShaderStageFlags::COMPUTE,
                        0,
                        push,
                    );
                }
                raw_body.cmd_dispatch(cmd, groups_x, groups_y, groups_z);
            }
            drop(pipeline);
        });
        for &(resource, usage) in accesses {
            pass = pass.access(resource, usage);
        }
        graph.add_pass(pass);
    }
}

/// One whole-image sync2 layout transition (single color mip), the capture path's barrier.
///
/// # Safety
///
/// `image` must outlive the recorded command; `cmd` must be in the recording state.
#[allow(clippy::too_many_arguments)]
pub(super) unsafe fn capture_barrier(
    raw: &ash::Device,
    cmd: vk::CommandBuffer,
    image: vk::Image,
    range: vk::ImageSubresourceRange,
    old_layout: vk::ImageLayout,
    new_layout: vk::ImageLayout,
    src_stage: vk::PipelineStageFlags2,
    src_access: vk::AccessFlags2,
    dst_stage: vk::PipelineStageFlags2,
    dst_access: vk::AccessFlags2,
) {
    let b = vk::ImageMemoryBarrier2::default()
        .src_stage_mask(src_stage)
        .src_access_mask(src_access)
        .dst_stage_mask(dst_stage)
        .dst_access_mask(dst_access)
        .old_layout(old_layout)
        .new_layout(new_layout)
        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .image(image)
        .subresource_range(range);
    let barriers = [b];
    let dep = vk::DependencyInfo::default().image_memory_barriers(&barriers);
    // SAFETY: forwarded from this function's contract — the image outlives the command.
    unsafe { raw.cmd_pipeline_barrier2(cmd, &dep) };
}

pub(super) fn depth_clear_store(resource: crate::render_graph::RgResource) -> RgAttachment {
    RgAttachment {
        resource,
        load_op: vk::AttachmentLoadOp::CLEAR,
        store_op: vk::AttachmentStoreOp::STORE,
        clear_value: vk::ClearValue {
            depth_stencil: vk::ClearDepthStencilValue {
                depth: 1.0,
                stencil: 0,
            },
        },
        resolve: None,
    }
}

/// A `LOAD`-then-`STORE` color attachment: composite over the existing contents.
/// grid + overlay draw over the tonemapped color and keep it).
pub(super) fn color_load_store(resource: RgResource) -> RgAttachment {
    RgAttachment {
        resource,
        load_op: vk::AttachmentLoadOp::LOAD,
        store_op: vk::AttachmentStoreOp::STORE,
        clear_value: vk::ClearValue::default(),
        resolve: None,
    }
}

/// A `LOAD`-then-`STORE` depth attachment: continue depth-testing and writing over the scene's
/// laid-down depth.
pub(super) fn depth_load_store(resource: RgResource) -> RgAttachment {
    RgAttachment {
        resource,
        load_op: vk::AttachmentLoadOp::LOAD,
        store_op: vk::AttachmentStoreOp::STORE,
        clear_value: vk::ClearValue::default(),
        resolve: None,
    }
}

/// A `LOAD`-then-`DONT_CARE` depth attachment: load the persisted scene depth so the grid /
/// overlay depth-test against it, but never write it back.
pub(super) fn depth_load_readonly(resource: RgResource) -> RgAttachment {
    RgAttachment {
        resource,
        load_op: vk::AttachmentLoadOp::LOAD,
        store_op: vk::AttachmentStoreOp::DONT_CARE,
        clear_value: vk::ClearValue::default(),
        resolve: None,
    }
}

pub(super) fn merge_timeline_point(
    points: &mut Vec<FrameTimelinePoint>,
    point: FrameTimelinePoint,
) {
    if let Some(existing) = points
        .iter_mut()
        .find(|existing| existing.semaphore == point.semaphore)
    {
        existing.value = existing.value.max(point.value);
    } else {
        points.push(point);
    }
}

pub(super) struct GraphCommandSubmission<'a> {
    pub(super) queue: RgQueueAssignment,
    pub(super) command_buffer: vk::CommandBuffer,
    pub(super) waits: &'a [FrameTimelinePoint],
    pub(super) signals: &'a [FrameTimelinePoint],
    pub(super) binary_signal: Option<vk::Semaphore>,
    pub(super) fence: vk::Fence,
    pub(super) context: &'static str,
}

pub(super) fn submit_graph_command(
    device: &Device,
    submission: GraphCommandSubmission<'_>,
) -> Result<()> {
    let wait_infos = submission
        .waits
        .iter()
        .map(|point| {
            vk::SemaphoreSubmitInfo::default()
                .semaphore(point.semaphore)
                .value(point.value)
                .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
        })
        .collect::<Vec<_>>();
    let mut signal_infos = submission
        .signals
        .iter()
        .map(|point| {
            vk::SemaphoreSubmitInfo::default()
                .semaphore(point.semaphore)
                .value(point.value)
                .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
        })
        .collect::<Vec<_>>();
    if let Some(semaphore) = submission.binary_signal {
        signal_infos.push(
            vk::SemaphoreSubmitInfo::default()
                .semaphore(semaphore)
                .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS),
        );
    }
    let commands =
        [vk::CommandBufferSubmitInfo::default().command_buffer(submission.command_buffer)];
    let submits = [vk::SubmitInfo2::default()
        .wait_semaphore_infos(&wait_infos)
        .command_buffer_infos(&commands)
        .signal_semaphore_infos(&signal_infos)];
    let queue = match submission.queue {
        RgQueueAssignment::Graphics => &device.graphics_queue,
        RgQueueAssignment::AsyncCompute => device.compute_queue.as_ref().ok_or(
            Error::PresentState("async-compute batch has no async-compute queue"),
        )?,
    };
    queue.submit2(device.raw(), &submits, submission.fence, submission.context)
}

/// The validation-clean gate's regression probe seam: when `SAFFRON_VK_PLANT_VALIDATION_ERROR`
/// is set, record one out-of-spec command into the scene frame's command buffer so the
/// validation layer flags exactly one error on submit.
///
/// It proves the gate's detector is live: an e2e test boots with the env set and asserts
/// `validation_errors()` is non-empty. The planted call is a zero-width viewport
/// (`VUID-VkViewport-width-01770`); every pass sets its own viewport inside its render pass, so
/// the bad state never reaches a draw and the rendered output is unaffected.
pub(super) fn plant_validation_error(raw: &ash::Device, command_buffer: vk::CommandBuffer) {
    if std::env::var_os("SAFFRON_VK_PLANT_VALIDATION_ERROR").is_none() {
        return;
    }
    let bad = vk::Viewport {
        x: 0.0,
        y: 0.0,
        width: 0.0,
        height: 1.0,
        min_depth: 0.0,
        max_depth: 1.0,
    };
    // SAFETY: the ash seam. `command_buffer` is recording (begun just above); a zero-width
    // viewport is rejected by the validation layer, which is the whole point — it does not
    // corrupt the device (always `VK_FALSE` from the messenger, no abort).
    unsafe { raw.cmd_set_viewport(command_buffer, 0, &[bad]) };
}
