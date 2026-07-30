//! Baking the persistent cloud shape fields: the sampler and pool they bind through, and the
//! one-off dispatch that fills the base / detail / curl volumes.

use super::*;

pub(super) fn create_sampler(
    raw: &ash::Device,
    filter: vk::Filter,
    address: vk::SamplerAddressMode,
) -> crate::Result<vk::Sampler> {
    let info = vk::SamplerCreateInfo::default()
        .mag_filter(filter)
        .min_filter(filter)
        .address_mode_u(address)
        .address_mode_v(address)
        .address_mode_w(address);
    checked(unsafe { raw.create_sampler(&info, None) }, "cloud sampler")
}

pub(super) fn create_pool(
    raw: &ash::Device,
    view_count: usize,
) -> crate::Result<vk::DescriptorPool> {
    let sizes = [
        vk::DescriptorPoolSize::default()
            .ty(vk::DescriptorType::STORAGE_IMAGE)
            .descriptor_count((view_count * 11 + 2) as u32),
        vk::DescriptorPoolSize::default()
            .ty(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .descriptor_count((view_count * 31 + 5) as u32),
        vk::DescriptorPoolSize::default()
            .ty(vk::DescriptorType::UNIFORM_BUFFER_DYNAMIC)
            .descriptor_count((view_count * 7 + 2) as u32),
        vk::DescriptorPoolSize::default()
            .ty(vk::DescriptorType::STORAGE_BUFFER)
            .descriptor_count((view_count * 2) as u32),
    ];
    let info = vk::DescriptorPoolCreateInfo::default()
        .max_sets((view_count * 7 + 2) as u32)
        .pool_sizes(&sizes);
    checked(
        unsafe { raw.create_descriptor_pool(&info, None) },
        "cloud descriptor pool",
    )
}

pub(super) fn bake_static_fields(
    device: &Device,
    pipelines: &Pipelines,
    base: &Image3D,
    detail: &Image3D,
    curl: &Image,
    weather: &Image,
    shadow: &Image,
) -> crate::Result<()> {
    let raw = device.raw();
    let layout = create_compute_layout(raw, &[(0, vk::DescriptorType::STORAGE_IMAGE)])?;
    let sizes = [vk::DescriptorPoolSize::default()
        .ty(vk::DescriptorType::STORAGE_IMAGE)
        .descriptor_count(3)];
    let pool_info = vk::DescriptorPoolCreateInfo::default()
        .max_sets(3)
        .pool_sizes(&sizes);
    let pool = match checked(
        unsafe { raw.create_descriptor_pool(&pool_info, None) },
        "cloud bake descriptor pool",
    ) {
        Ok(pool) => pool,
        Err(err) => {
            unsafe { raw.destroy_descriptor_set_layout(layout, None) };
            return Err(err);
        }
    };
    let result = (|| -> crate::Result<()> {
        let layouts = [layout; 3];
        let alloc = vk::DescriptorSetAllocateInfo::default()
            .descriptor_pool(pool)
            .set_layouts(&layouts);
        let sets = checked(
            unsafe { raw.allocate_descriptor_sets(&alloc) },
            "cloud bake descriptor sets",
        )?;
        let infos = [base.view(), detail.view(), curl.view()].map(|view| {
            [vk::DescriptorImageInfo::default()
                .image_view(view)
                .image_layout(vk::ImageLayout::GENERAL)]
        });
        let writes: Vec<_> = sets
            .iter()
            .zip(&infos)
            .map(|(&set, info)| {
                vk::WriteDescriptorSet::default()
                    .dst_set(set)
                    .dst_binding(0)
                    .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                    .image_info(info)
            })
            .collect();
        unsafe { raw.update_descriptor_sets(&writes, &[]) };

        let base_pipeline = pipelines.build_compute("shaders/cloud_noise_base.spv", layout, 0)?;
        let detail_pipeline =
            pipelines.build_compute("shaders/cloud_noise_detail.spv", layout, 0)?;
        let curl_pipeline = pipelines.build_compute("shaders/cloud_curl.spv", layout, 0)?;
        record_bake(
            device,
            &[
                (&base_pipeline, sets[0], (16, 16, 16)),
                (&detail_pipeline, sets[1], (4, 4, 4)),
                (&curl_pipeline, sets[2], (16, 16, 1)),
            ],
            &[
                (base.handle(), 1),
                (detail.handle(), 1),
                (curl.handle(), 1),
                (weather.handle(), 1),
                (shadow.handle(), CLOUD_SHADOW_CASCADES),
            ],
        )
    })();
    unsafe {
        raw.destroy_descriptor_pool(pool, None);
        raw.destroy_descriptor_set_layout(layout, None);
    }
    result
}

pub(super) fn record_bake(
    device: &Device,
    dispatches: &[(&Pipeline, vk::DescriptorSet, (u32, u32, u32))],
    images: &[(vk::Image, u32)],
) -> crate::Result<()> {
    let raw = device.raw();
    let pool_info = vk::CommandPoolCreateInfo::default()
        .flags(vk::CommandPoolCreateFlags::TRANSIENT)
        .queue_family_index(device.graphics_queue_family);
    let pool = checked(
        unsafe { raw.create_command_pool(&pool_info, None) },
        "cloud bake command pool",
    )?;
    let result = (|| -> crate::Result<()> {
        let alloc = vk::CommandBufferAllocateInfo::default()
            .command_pool(pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        let cmd = checked(
            unsafe { raw.allocate_command_buffers(&alloc) },
            "cloud bake command buffer",
        )?[0];
        let to_general: Vec<_> = images
            .iter()
            .map(|&(image, layer_count)| {
                vk::ImageMemoryBarrier2::default()
                    .src_stage_mask(vk::PipelineStageFlags2::TOP_OF_PIPE)
                    .dst_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER)
                    .dst_access_mask(vk::AccessFlags2::SHADER_STORAGE_WRITE)
                    .old_layout(vk::ImageLayout::UNDEFINED)
                    .new_layout(vk::ImageLayout::GENERAL)
                    .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .image(image)
                    .subresource_range(vk::ImageSubresourceRange {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        base_mip_level: 0,
                        level_count: 1,
                        base_array_layer: 0,
                        layer_count,
                    })
            })
            .collect();
        let begin = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
        unsafe {
            checked(raw.begin_command_buffer(cmd, &begin), "cloud bake begin")?;
            let dep = vk::DependencyInfo::default().image_memory_barriers(&to_general);
            raw.cmd_pipeline_barrier2(cmd, &dep);
            for &(pipeline, set, groups) in dispatches {
                raw.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, pipeline.handle());
                raw.cmd_bind_descriptor_sets(
                    cmd,
                    vk::PipelineBindPoint::COMPUTE,
                    pipeline.layout(),
                    0,
                    &[set],
                    &[],
                );
                raw.cmd_dispatch(cmd, groups.0, groups.1, groups.2);
            }
            let to_read: Vec<_> = images
                .iter()
                .map(|&(image, layer_count)| {
                    vk::ImageMemoryBarrier2::default()
                        .src_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER)
                        .src_access_mask(vk::AccessFlags2::SHADER_STORAGE_WRITE)
                        .dst_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER)
                        .dst_access_mask(vk::AccessFlags2::SHADER_SAMPLED_READ)
                        .old_layout(vk::ImageLayout::GENERAL)
                        .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                        .image(image)
                        .subresource_range(vk::ImageSubresourceRange {
                            aspect_mask: vk::ImageAspectFlags::COLOR,
                            base_mip_level: 0,
                            level_count: 1,
                            base_array_layer: 0,
                            layer_count,
                        })
                })
                .collect();
            let dep = vk::DependencyInfo::default().image_memory_barriers(&to_read);
            raw.cmd_pipeline_barrier2(cmd, &dep);
            checked(raw.end_command_buffer(cmd), "cloud bake end")?;
        }
        let cmds = [vk::CommandBufferSubmitInfo::default().command_buffer(cmd)];
        let submits = [vk::SubmitInfo2::default().command_buffer_infos(&cmds)];
        device
            .graphics_queue
            .submit2(raw, &submits, vk::Fence::null(), "cloud bake submit")?;
        device.wait_idle()
    })();
    unsafe { raw.destroy_command_pool(pool, None) };
    result
}

pub(super) const fn align_up(value: u64, align: u64) -> u64 {
    if align <= 1 {
        value
    } else {
        value.div_ceil(align) * align
    }
}
