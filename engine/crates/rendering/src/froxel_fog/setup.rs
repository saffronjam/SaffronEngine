//! Volume allocation, the compute set layouts, and the per-frame descriptor writes for the
//! froxel grid and the aerial-perspective volume.

use super::*;

/// One-shot init barrier: the AP volume `UNDEFINED → SHADER_READ_ONLY_OPTIMAL` (the composite's
/// resting sample layout), waited idle.
pub(super) fn init_transition_ap(device: &Device, image: vk::Image) -> crate::Result<()> {
    let raw = device.raw();
    let pool_info = vk::CommandPoolCreateInfo::default()
        .flags(vk::CommandPoolCreateFlags::TRANSIENT)
        .queue_family_index(device.graphics_queue_family);
    // SAFETY: the ash seam. The pool is created, used, and destroyed within this call.
    let pool = checked(
        unsafe { raw.create_command_pool(&pool_info, None) },
        "aerial perspective init pool",
    )?;
    let result = (|| -> crate::Result<()> {
        let alloc = vk::CommandBufferAllocateInfo::default()
            .command_pool(pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        // SAFETY: the ash seam. One buffer from the private pool.
        let cmd = checked(
            unsafe { raw.allocate_command_buffers(&alloc) },
            "aerial perspective init cmd",
        )?[0];
        let range = vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            base_mip_level: 0,
            level_count: 1,
            base_array_layer: 0,
            layer_count: 1,
        };
        let barrier = vk::ImageMemoryBarrier2::default()
            .src_stage_mask(vk::PipelineStageFlags2::TOP_OF_PIPE)
            .src_access_mask(vk::AccessFlags2::empty())
            .dst_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER)
            .dst_access_mask(vk::AccessFlags2::SHADER_SAMPLED_READ)
            .old_layout(vk::ImageLayout::UNDEFINED)
            .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(image)
            .subresource_range(range);
        let barriers = [barrier];
        let begin = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
        // SAFETY: the ash seam. Records the init barrier on the fresh buffer.
        unsafe {
            checked(
                raw.begin_command_buffer(cmd, &begin),
                "aerial perspective init begin",
            )?;
            let dep = vk::DependencyInfo::default().image_memory_barriers(&barriers);
            raw.cmd_pipeline_barrier2(cmd, &dep);
            checked(raw.end_command_buffer(cmd), "aerial perspective init end")?;
        }
        let cmd_infos = [vk::CommandBufferSubmitInfo::default().command_buffer(cmd)];
        let submits = [vk::SubmitInfo2::default().command_buffer_infos(&cmd_infos)];
        // SAFETY: the ash seam. The graphics queue is idle at init; drain with wait_idle below.
        device.graphics_queue.submit2(
            raw,
            &submits,
            vk::Fence::null(),
            "aerial perspective init submit",
        )?;
        device.wait_idle()?;
        Ok(())
    })();
    // SAFETY: the ash seam. The queue was drained above, so the pool is idle.
    unsafe { raw.destroy_command_pool(pool, None) };
    result
}

/// A compute set layout over the given `(binding, type)` pairs.
pub(crate) fn create_compute_layout(
    raw: &ash::Device,
    bindings: &[(u32, vk::DescriptorType)],
) -> crate::Result<vk::DescriptorSetLayout> {
    crate::vk_write::compute_layout_sparse(raw, bindings, "froxel fog set layout")
}

/// Allocates the ping-pong scatter pair + the integration volume at `(x, y, z)` froxel dims — the
/// three `rgba16f` `STORAGE | SAMPLED` volumes the inject/integrate passes fill.
pub(super) fn alloc_volumes(
    resources: &Arc<DeviceResources>,
    dims: (u32, u32, u32),
) -> crate::Result<([Image3D; 2], Image3D)> {
    let extent = vk::Extent3D {
        width: dims.0,
        height: dims.1,
        depth: dims.2,
    };
    let usage = vk::ImageUsageFlags::STORAGE | vk::ImageUsageFlags::SAMPLED;
    let scatter = [
        Image3D::new(resources, extent, FROXEL_FORMAT, 1, usage)?,
        Image3D::new(resources, extent, FROXEL_FORMAT, 1, usage)?,
    ];
    let integration = Image3D::new(resources, extent, FROXEL_FORMAT, 1, usage)?;
    Ok((scatter, integration))
}

/// The volumes/buffers/samplers the fog descriptor sets reference, grouped so [`write_fog_sets`]
/// takes one bundle instead of a long argument list.
#[derive(Clone, Copy)]
pub(super) struct FogSetResources<'a> {
    pub(super) sampler: vk::Sampler,
    pub(super) noise_sampler: vk::Sampler,
    pub(super) scatter: &'a [Image3D; 2],
    pub(super) integration: &'a Image3D,
    pub(super) grid_params: &'a Buffer,
    pub(super) fog_volumes: &'a Buffer,
    pub(super) noise: &'a Image3D,
}

/// Rewrites the four fog descriptor sets against the current volumes: for each ping-pong parity
/// `p`, `inject_sets[p]` writes `scatter[p]` (binding 0), reads the grid UBO (binding 1), and
/// samples `scatter[p ^ 1]` as history (binding 2); `integrate_sets[p]` reads `scatter[p]`
/// (binding 0), writes `integration` (binding 1), and reads the grid UBO (binding 2).
pub(super) fn write_fog_sets(
    raw: &ash::Device,
    inject_sets: [vk::DescriptorSet; 2],
    integrate_sets: [vk::DescriptorSet; 2],
    res: &FogSetResources,
) {
    let FogSetResources {
        sampler,
        noise_sampler,
        scatter,
        integration,
        grid_params,
        fog_volumes,
        noise,
    } = *res;
    let storage = |view| {
        [vk::DescriptorImageInfo::default()
            .image_view(view)
            .image_layout(vk::ImageLayout::GENERAL)]
    };
    let sampled = |view| {
        [vk::DescriptorImageInfo::default()
            .sampler(sampler)
            .image_view(view)
            .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)]
    };
    let scatter_storage = [storage(scatter[0].view()), storage(scatter[1].view())];
    let scatter_history = [sampled(scatter[0].view()), sampled(scatter[1].view())];
    let integ_storage = storage(integration.view());
    let grid_info = [vk::DescriptorBufferInfo::default()
        .buffer(grid_params.handle())
        .range(grid_params.size())];
    let volumes_info = [vk::DescriptorBufferInfo::default()
        .buffer(fog_volumes.handle())
        .range(fog_volumes.size())];
    let noise_info = [vk::DescriptorImageInfo::default()
        .sampler(noise_sampler)
        .image_view(noise.view())
        .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)];

    let mut writes = Vec::with_capacity(16);
    for p in 0..2 {
        writes.push(
            vk::WriteDescriptorSet::default()
                .dst_set(inject_sets[p])
                .dst_binding(0)
                .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                .image_info(&scatter_storage[p]),
        );
        writes.push(
            vk::WriteDescriptorSet::default()
                .dst_set(inject_sets[p])
                .dst_binding(1)
                .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER)
                .buffer_info(&grid_info),
        );
        writes.push(
            vk::WriteDescriptorSet::default()
                .dst_set(inject_sets[p])
                .dst_binding(2)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .image_info(&scatter_history[p ^ 1]),
        );
        writes.push(
            vk::WriteDescriptorSet::default()
                .dst_set(inject_sets[p])
                .dst_binding(3)
                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                .buffer_info(&volumes_info),
        );
        writes.push(
            vk::WriteDescriptorSet::default()
                .dst_set(inject_sets[p])
                .dst_binding(4)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .image_info(&noise_info),
        );
        writes.push(
            vk::WriteDescriptorSet::default()
                .dst_set(integrate_sets[p])
                .dst_binding(0)
                .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                .image_info(&scatter_storage[p]),
        );
        writes.push(
            vk::WriteDescriptorSet::default()
                .dst_set(integrate_sets[p])
                .dst_binding(1)
                .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                .image_info(&integ_storage),
        );
        writes.push(
            vk::WriteDescriptorSet::default()
                .dst_set(integrate_sets[p])
                .dst_binding(2)
                .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER)
                .buffer_info(&grid_info),
        );
    }
    // SAFETY: the ash seam. All infos outlive the call (owned above).
    unsafe { raw.update_descriptor_sets(&writes, &[]) };
}

/// One-shot init barrier: both scatter ping-pong volumes `UNDEFINED → GENERAL` (the storage
/// read/write resting state) and the integration volume `UNDEFINED → SHADER_READ_ONLY_OPTIMAL` (the
/// composite sample resting state), waited idle.
pub(super) fn init_transition_volumes(
    device: &Device,
    scatter: [vk::Image; 2],
    integration: vk::Image,
) -> crate::Result<()> {
    let raw = device.raw();
    let pool_info = vk::CommandPoolCreateInfo::default()
        .flags(vk::CommandPoolCreateFlags::TRANSIENT)
        .queue_family_index(device.graphics_queue_family);
    // SAFETY: the ash seam. The pool is created, used, and destroyed within this call.
    let pool = checked(
        unsafe { raw.create_command_pool(&pool_info, None) },
        "froxel fog init pool",
    )?;
    let result = (|| -> crate::Result<()> {
        let alloc = vk::CommandBufferAllocateInfo::default()
            .command_pool(pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        // SAFETY: the ash seam. One buffer from the private pool.
        let cmd = checked(
            unsafe { raw.allocate_command_buffers(&alloc) },
            "froxel fog init cmd",
        )?[0];
        let range = vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            base_mip_level: 0,
            level_count: 1,
            base_array_layer: 0,
            layer_count: 1,
        };
        let barrier = |image, new_layout| {
            vk::ImageMemoryBarrier2::default()
                .src_stage_mask(vk::PipelineStageFlags2::TOP_OF_PIPE)
                .src_access_mask(vk::AccessFlags2::empty())
                .dst_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER)
                .dst_access_mask(
                    vk::AccessFlags2::SHADER_STORAGE_WRITE | vk::AccessFlags2::SHADER_SAMPLED_READ,
                )
                .old_layout(vk::ImageLayout::UNDEFINED)
                .new_layout(new_layout)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(image)
                .subresource_range(range)
        };
        let barriers = [
            barrier(scatter[0], vk::ImageLayout::GENERAL),
            barrier(scatter[1], vk::ImageLayout::GENERAL),
            barrier(integration, vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL),
        ];
        let begin = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
        // SAFETY: the ash seam. Records the three init barriers on the fresh buffer.
        unsafe {
            checked(
                raw.begin_command_buffer(cmd, &begin),
                "froxel fog init begin",
            )?;
            let dep = vk::DependencyInfo::default().image_memory_barriers(&barriers);
            raw.cmd_pipeline_barrier2(cmd, &dep);
            checked(raw.end_command_buffer(cmd), "froxel fog init end")?;
        }
        let cmd_infos = [vk::CommandBufferSubmitInfo::default().command_buffer(cmd)];
        let submits = [vk::SubmitInfo2::default().command_buffer_infos(&cmd_infos)];
        // SAFETY: the ash seam. The graphics queue is idle at init; drain with wait_idle below.
        device.graphics_queue.submit2(
            raw,
            &submits,
            vk::Fence::null(),
            "froxel fog init submit",
        )?;
        device.wait_idle()?;
        Ok(())
    })();
    // SAFETY: the ash seam. The queue was drained above, so the pool is idle.
    unsafe { raw.destroy_command_pool(pool, None) };
    result
}
