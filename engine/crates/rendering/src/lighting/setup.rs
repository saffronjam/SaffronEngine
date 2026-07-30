//! Per-frame buffer construction: the mapped light UBO / SSBO, the device-local cluster
//! lists, and the shadow sampler writes.

use super::*;

/// Builds one frame slot's lighting buffers + sets, binding the virtual-shadow atlas
/// into the light set and wiring the cluster set's params/light/cluster bindings.
pub(super) fn build_frame(
    resources: &Arc<DeviceResources>,
    descriptors: &Descriptors,
    vsm_atlas: vk::ImageView,
) -> Result<FrameLighting> {
    let raw = resources.device();

    let light_ubo = make_mapped_uniform_buffer(resources, size_of::<LightUbo>() as vk::DeviceSize)?;
    let light_set = descriptors.allocate_set(descriptors.light_set_layout())?;
    descriptors.write_uniform_buffer(light_set, 0, light_ubo.handle(), light_ubo.size());

    // Binding 13: the virtual-shadow atlas behind its immutable compare sampler. The
    // graph guarantees ShaderReadOnly when the scene samples.
    write_shadow_samplers(raw, light_set, vsm_atlas);

    let light_list = make_mapped_storage_buffer(
        resources,
        u64::from(LIGHT_LIST_INITIAL) * size_of::<GpuLight>() as u64,
    )?;
    descriptors.write_storage_buffer(light_set, 1, light_list.handle(), light_list.size());

    let cluster_buffer =
        make_device_storage_buffer(resources, u64::from(CLUSTER_COUNT) * CLUSTER_STRIDE)?;
    let cluster_params =
        make_mapped_uniform_buffer(resources, size_of::<ClusterParams>() as vk::DeviceSize)?;

    // Light set bindings 2 (cluster lists) + 3 (cluster params).
    descriptors.write_storage_buffer(light_set, 2, cluster_buffer.handle(), cluster_buffer.size());
    descriptors.write_uniform_buffer(light_set, 3, cluster_params.handle(), cluster_params.size());

    // Compute cluster set: params UBO (0) + punctual list read (1) + cluster lists write (2).
    let cluster_set = descriptors.allocate_set(descriptors.cluster_set_layout())?;
    descriptors.write_uniform_buffer(
        cluster_set,
        0,
        cluster_params.handle(),
        cluster_params.size(),
    );
    descriptors.write_storage_buffer(cluster_set, 1, light_list.handle(), light_list.size());
    descriptors.write_storage_buffer(
        cluster_set,
        2,
        cluster_buffer.handle(),
        cluster_buffer.size(),
    );

    Ok(FrameLighting {
        light_set,
        light_ubo,
        light_list,
        light_list_capacity: LIGHT_LIST_INITIAL,
        cluster_set,
        cluster_buffer,
        cluster_params,
    })
}

/// Binds the virtual-shadow atlas (13, behind the layout's immutable compare
/// sampler) into `light_set`.
pub(super) fn write_shadow_samplers(
    raw: &ash::Device,
    light_set: vk::DescriptorSet,
    vsm_atlas: vk::ImageView,
) {
    let atlas = [vk::DescriptorImageInfo {
        sampler: vk::Sampler::null(),
        image_view: vsm_atlas,
        image_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
    }];
    let writes = [vk::WriteDescriptorSet::default()
        .dst_set(light_set)
        .dst_binding(13)
        .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
        .image_info(&atlas)];
    // SAFETY: the ash seam; the set and view outlive the call.
    unsafe { raw.update_descriptor_sets(&writes, &[]) };
}

/// A persistently host-mapped uniform buffer of `size` bytes — the per-frame light UBO, rewritten
/// in place each frame.
pub(super) fn make_mapped_uniform_buffer(
    resources: &Arc<DeviceResources>,
    size: vk::DeviceSize,
) -> Result<Buffer> {
    crate::vk_write::mapped_buffer(
        resources,
        size,
        vk::BufferUsageFlags::UNIFORM_BUFFER,
        vk_mem::AllocationCreateFlags::HOST_ACCESS_RANDOM,
    )
}

/// A persistently host-mapped storage buffer of `size` bytes — the punctual light list backing.
pub(super) fn make_mapped_storage_buffer(
    resources: &Arc<DeviceResources>,
    size: vk::DeviceSize,
) -> Result<Buffer> {
    crate::vk_write::mapped_buffer(
        resources,
        size,
        vk::BufferUsageFlags::STORAGE_BUFFER,
        vk_mem::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE,
    )
}

/// A device-local storage buffer of `size` bytes — the cluster lists the cull compute writes.
pub(super) fn make_device_storage_buffer(
    resources: &Arc<DeviceResources>,
    size: vk::DeviceSize,
) -> Result<Buffer> {
    crate::vk_write::device_buffer(resources, size, vk::BufferUsageFlags::STORAGE_BUFFER)
}
