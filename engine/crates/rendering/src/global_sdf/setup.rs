//! Cascade volume creation, the compute set layouts, the repeat sampler, and the per-frame
//! buffer writes.

use super::*;
pub(super) use crate::vk_write::write_storage_buffer;

/// Builds one frame-in-flight slot: its GPU-rebuilt cull-list SSBO, its host-mapped params UBO, and
/// the cull + composite compute sets (pool-owned). The sets reference the slot's own cull list, so a
/// frame in flight never shares a GPU-written buffer with another.
pub(super) fn build_frame(
    resources: &Arc<DeviceResources>,
    descriptors: &Descriptors,
    layouts: (
        vk::DescriptorSetLayout,
        vk::DescriptorSetLayout,
        vk::DescriptorSetLayout,
    ),
) -> Result<FrameGdf> {
    // The cull list: header (per-cascade counters + pad) + per-cascade index segments. Device-local
    // — GPU-built (atomic append) + GPU-read (composite); the counters are cleared each frame by a
    // `cmd_fill_buffer` in the cull pass body.
    let cull_bytes =
        u64::from(GDF_CULL_HEADER + GDF_CASCADES * GDF_MAX_CULLED) * size_of::<u32>() as u64;
    let cull_buffer = make_device_storage_buffer(resources, cull_bytes)?;
    let params_ubo = make_mapped_uniform_buffer(resources, size_of::<GdfParamsUbo>() as u64)?;
    let cull_set = descriptors.allocate_set(layouts.0)?;
    let composite_set = descriptors.allocate_set(layouts.1)?;
    let scatter_set = descriptors.allocate_set(layouts.2)?;
    Ok(FrameGdf {
        cull_buffer,
        params_ubo,
        cull_set,
        composite_set,
        scatter_set,
    })
}

/// Builds the cull + composite compute set layouts, freeing the cull layout on a composite
/// failure. Returns `(cull, composite)`.
/// Clears every cascade volume to the open-space value (`1.0` = +max-encode distance),
/// the occupancy volumes to `0`, and the albedo cache to `0`, leaving all of them in
/// `GENERAL` for the composite. One submission at construction, fence-waited.
pub(super) fn initialize_volumes(
    device: &Device,
    cascades: &[Image3D],
    occupancy: &[Image3D],
    albedo: &Image3D,
) -> Result<()> {
    let raw = device.resources().device();
    let pool_info =
        vk::CommandPoolCreateInfo::default().queue_family_index(device.graphics_queue_family);
    // SAFETY: the ash seam. Freed at the end of the function.
    let pool = checked(
        unsafe { raw.create_command_pool(&pool_info, None) },
        "gdf init pool",
    )?;
    let alloc = vk::CommandBufferAllocateInfo::default()
        .command_pool(pool)
        .level(vk::CommandBufferLevel::PRIMARY)
        .command_buffer_count(1);
    // SAFETY: the ash seam. One buffer from the pool above.
    let cmd = match checked(
        unsafe { raw.allocate_command_buffers(&alloc) },
        "gdf init cmd",
    ) {
        Ok(buffers) => buffers[0],
        Err(err) => {
            // SAFETY: the ash seam. Free the pool on this failure path.
            unsafe { raw.destroy_command_pool(pool, None) };
            return Err(err);
        }
    };
    let fence = match checked(
        unsafe { raw.create_fence(&vk::FenceCreateInfo::default(), None) },
        "gdf init fence",
    ) {
        Ok(fence) => fence,
        Err(err) => {
            // SAFETY: the ash seam.
            unsafe { raw.destroy_command_pool(pool, None) };
            return Err(err);
        }
    };
    let result = (|| -> Result<()> {
        let begin = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
        let range = vk::ImageSubresourceRange::default()
            .aspect_mask(vk::ImageAspectFlags::COLOR)
            .level_count(1)
            .layer_count(1);
        let volumes: Vec<(vk::Image, vk::ClearColorValue)> = cascades
            .iter()
            .map(|img| {
                (
                    img.handle(),
                    vk::ClearColorValue {
                        float32: [1.0, 0.0, 0.0, 0.0],
                    },
                )
            })
            .chain(
                occupancy
                    .iter()
                    .chain(std::iter::once(albedo))
                    .map(|img| (img.handle(), vk::ClearColorValue { float32: [0.0; 4] })),
            )
            .collect();
        // SAFETY: the ash seam. The barriers/clears reference this device's images.
        unsafe {
            checked(raw.begin_command_buffer(cmd, &begin), "gdf init begin")?;
            let to_transfer: Vec<vk::ImageMemoryBarrier2> = volumes
                .iter()
                .map(|(image, _)| {
                    vk::ImageMemoryBarrier2::default()
                        .image(*image)
                        .subresource_range(range)
                        .old_layout(vk::ImageLayout::UNDEFINED)
                        .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                        .dst_stage_mask(vk::PipelineStageFlags2::TRANSFER)
                        .dst_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
                })
                .collect();
            raw.cmd_pipeline_barrier2(
                cmd,
                &vk::DependencyInfo::default().image_memory_barriers(&to_transfer),
            );
            for (image, clear) in &volumes {
                raw.cmd_clear_color_image(
                    cmd,
                    *image,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    clear,
                    &[range],
                );
            }
            let to_general: Vec<vk::ImageMemoryBarrier2> = volumes
                .iter()
                .map(|(image, _)| {
                    vk::ImageMemoryBarrier2::default()
                        .image(*image)
                        .subresource_range(range)
                        .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                        .new_layout(vk::ImageLayout::GENERAL)
                        .src_stage_mask(vk::PipelineStageFlags2::TRANSFER)
                        .src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
                        .dst_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER)
                        .dst_access_mask(
                            vk::AccessFlags2::SHADER_STORAGE_READ
                                | vk::AccessFlags2::SHADER_STORAGE_WRITE,
                        )
                })
                .collect();
            raw.cmd_pipeline_barrier2(
                cmd,
                &vk::DependencyInfo::default().image_memory_barriers(&to_general),
            );
            checked(raw.end_command_buffer(cmd), "gdf init end")?;
            let buffers = [vk::CommandBufferSubmitInfo::default().command_buffer(cmd)];
            let submit = [vk::SubmitInfo2::default().command_buffer_infos(&buffers)];
            device
                .graphics_queue
                .submit2(raw, &submit, fence, "gdf init submit")?;
            checked(
                raw.wait_for_fences(&[fence], true, u64::MAX),
                "gdf init wait",
            )?;
        }
        Ok(())
    })();
    // SAFETY: the ash seam. The fence was waited (or the submit never happened).
    unsafe {
        raw.destroy_fence(fence, None);
        raw.destroy_command_pool(pool, None);
    }
    result
}

pub(super) fn build_layouts(
    raw: &ash::Device,
) -> Result<(
    vk::DescriptorSetLayout,
    vk::DescriptorSetLayout,
    vk::DescriptorSetLayout,
)> {
    let sb = vk::DescriptorType::STORAGE_BUFFER;
    let si = vk::DescriptorType::STORAGE_IMAGE;
    let ub = vk::DescriptorType::UNIFORM_BUFFER;
    // Cull set: instances (b0, read) + cull list (b1, rw) + the scatter meta words (b2, read).
    let cull = make_compute_layout(raw, &[(sb, 1), (sb, 1), (sb, 1)])?;
    // Composite set: instances (b0) + cull list (b1) + cascade volumes (b2, array of GDF_CASCADES)
    // + the lite albedo cache (b3, single storage image, written for the finest cascade) + the
    // porous-occupancy volumes (b4, array of GDF_CASCADES).
    let composite = match make_compute_layout(
        raw,
        &[
            (sb, 1),
            (sb, 1),
            (si, GDF_CASCADES),
            (si, 1),
            (si, GDF_CASCADES),
        ],
    ) {
        Ok(layout) => layout,
        Err(err) => {
            // SAFETY: the ash seam. Free the cull layout on this partial-failure path.
            unsafe { raw.destroy_descriptor_set_layout(cull, None) };
            return Err(err);
        }
    };
    // Scatter set: the reach view's counters (b0) + visible list (b1) + the occluder
    // output region (b2, rw) + the meta words (b3, rw) + the scene address block (b4).
    let scatter = match make_compute_layout(raw, &[(sb, 1), (sb, 1), (sb, 1), (sb, 1), (ub, 1)]) {
        Ok(layout) => layout,
        Err(err) => {
            // SAFETY: the ash seam. Free both earlier layouts on this partial-failure path.
            unsafe {
                raw.destroy_descriptor_set_layout(cull, None);
                raw.destroy_descriptor_set_layout(composite, None);
            }
            return Err(err);
        }
    };
    Ok((cull, composite, scatter))
}

/// A compute set layout with one `(type, count)` binding per entry, in order.
pub(super) fn make_compute_layout(
    raw: &ash::Device,
    bindings: &[(vk::DescriptorType, u32)],
) -> Result<vk::DescriptorSetLayout> {
    crate::vk_write::compute_layout_counted(raw, bindings, "gdf compute layout")
}

/// A device-local storage buffer of `size` bytes (the GPU-built cull list).
pub(super) fn make_device_storage_buffer(
    resources: &Arc<DeviceResources>,
    size: vk::DeviceSize,
) -> Result<Buffer> {
    crate::vk_write::device_buffer(
        resources,
        size,
        vk::BufferUsageFlags::STORAGE_BUFFER | vk::BufferUsageFlags::TRANSFER_DST,
    )
}

/// A persistently host-mapped uniform buffer of `size` bytes (the params UBO).
pub(super) fn make_mapped_uniform_buffer(
    resources: &Arc<DeviceResources>,
    size: vk::DeviceSize,
) -> Result<Buffer> {
    crate::vk_write::mapped_buffer(
        resources,
        size,
        vk::BufferUsageFlags::UNIFORM_BUFFER,
        vk_mem::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE,
    )
}

/// The linear, **repeat** cascade sampler — repeat addressing realizes the toroidal wrap (a
/// trilinear tap across the storage wrap reads spatially-adjacent texels).
pub(super) fn create_linear_repeat_sampler(raw: &ash::Device) -> Result<vk::Sampler> {
    let info = vk::SamplerCreateInfo::default()
        .mag_filter(vk::Filter::LINEAR)
        .min_filter(vk::Filter::LINEAR)
        .mipmap_mode(vk::SamplerMipmapMode::NEAREST)
        .address_mode_u(vk::SamplerAddressMode::REPEAT)
        .address_mode_v(vk::SamplerAddressMode::REPEAT)
        .address_mode_w(vk::SamplerAddressMode::REPEAT);
    // SAFETY: the ash seam. The sampler is owned and freed in `Drop`.
    checked(unsafe { raw.create_sampler(&info, None) }, "gdf sampler")
}

/// `UNDEFINED → SHADER_READ_ONLY_OPTIMAL` init barrier for a cascade volume (1 mip, 1 layer,
/// color), made sampler-readable by both the fragment + compute consumers.
pub(super) fn cascade_init_barrier(image: vk::Image) -> vk::ImageMemoryBarrier2<'static> {
    vk::ImageMemoryBarrier2::default()
        .src_stage_mask(vk::PipelineStageFlags2::TOP_OF_PIPE)
        .src_access_mask(vk::AccessFlags2::empty())
        .dst_stage_mask(
            vk::PipelineStageFlags2::FRAGMENT_SHADER | vk::PipelineStageFlags2::COMPUTE_SHADER,
        )
        .dst_access_mask(vk::AccessFlags2::SHADER_SAMPLED_READ)
        .old_layout(vk::ImageLayout::UNDEFINED)
        .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .image(image)
        .subresource_range(vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            base_mip_level: 0,
            level_count: 1,
            base_array_layer: 0,
            layer_count: 1,
        })
}

/// Writes the whole uniform buffer into `(set, binding)`.
pub(super) fn write_uniform_buffer(
    raw: &ash::Device,
    set: vk::DescriptorSet,
    binding: u32,
    buffer: vk::Buffer,
    size: vk::DeviceSize,
) {
    crate::vk_write::write_uniform_buffer(raw, set, binding, buffer, 0, size);
}
