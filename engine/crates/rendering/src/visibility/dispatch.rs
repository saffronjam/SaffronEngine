use ash::vk;

use crate::resources::Buffer;

/// Binds `pipeline` + `set`, pushes `push` when present, and dispatches `groups` along x.
pub(super) fn record_binning_dispatch(
    raw: &ash::Device,
    cmd: vk::CommandBuffer,
    pipeline: &crate::Pipeline,
    set: vk::DescriptorSet,
    push: Option<&[u8]>,
    groups: u32,
) {
    // SAFETY: the ash seam. The PSO/set are valid this frame; any push spans the
    // declared range; the dispatch covers the record capacity or the bin table.
    unsafe {
        raw.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, pipeline.handle());
        raw.cmd_bind_descriptor_sets(
            cmd,
            vk::PipelineBindPoint::COMPUTE,
            pipeline.layout(),
            0,
            &[set],
            &[],
        );
        if let Some(push) = push {
            raw.cmd_push_constants(
                cmd,
                pipeline.layout(),
                vk::ShaderStageFlags::COMPUTE,
                0,
                push,
            );
        }
        raw.cmd_dispatch(cmd, groups, 1, 1);
    }
}

/// [`record_binning_dispatch`] with a typed push block.
pub(super) fn record_dispatch<P: bytemuck::Pod>(
    raw: &ash::Device,
    cmd: vk::CommandBuffer,
    pipeline: &crate::Pipeline,
    set: vk::DescriptorSet,
    push: P,
    groups: u32,
) {
    record_binning_dispatch(
        raw,
        cmd,
        pipeline,
        set,
        Some(bytemuck::bytes_of(&push)),
        groups,
    );
}

pub(super) fn write_storage(
    raw: &ash::Device,
    set: vk::DescriptorSet,
    binding: u32,
    buffer: &Buffer,
) {
    let info = [vk::DescriptorBufferInfo {
        buffer: buffer.handle(),
        offset: 0,
        range: buffer.size(),
    }];
    let write = vk::WriteDescriptorSet::default()
        .dst_set(set)
        .dst_binding(binding)
        .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
        .buffer_info(&info);
    // SAFETY: the ash seam. The set and buffer outlive the call.
    unsafe { raw.update_descriptor_sets(&[write], &[]) };
}
