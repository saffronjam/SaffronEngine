//! The DDGI descriptor layouts and sets, the atlas storage images and sampler, and the
//! clear barriers plus descriptor writes the passes bind through.

use super::*;
pub(super) use crate::vk_write::{write_combined_sampler, write_storage_image};

/// The four DDGI compute set layouts (the mesh set 5 layout is owned by `Descriptors`).
pub(super) struct DdgiLayouts {
    pub(super) trace: vk::DescriptorSetLayout,
    pub(super) blend_irr: vk::DescriptorSetLayout,
    pub(super) blend_dist: vk::DescriptorSetLayout,
    pub(super) border: vk::DescriptorSetLayout,
}

impl DdgiLayouts {
    /// Frees every created layout (the partial-failure cleanup path).
    ///
    /// # Safety
    ///
    /// The device must be idle and each layout created exactly once.
    pub(super) unsafe fn destroy(&self, raw: &ash::Device) {
        // SAFETY: forwarded from the caller's contract — each layout is freed once.
        unsafe {
            raw.destroy_descriptor_set_layout(self.trace, None);
            raw.destroy_descriptor_set_layout(self.blend_irr, None);
            raw.destroy_descriptor_set_layout(self.blend_dist, None);
            raw.destroy_descriptor_set_layout(self.border, None);
        }
    }
}

/// Builds the four compute set layouts, freeing what was created so far on any failure.
pub(super) fn build_layouts(raw: &ash::Device) -> Result<DdgiLayouts> {
    let si = vk::DescriptorType::STORAGE_IMAGE;
    let cs = vk::DescriptorType::COMBINED_IMAGE_SAMPLER;

    // trace set 2: albedo cache sampler (b0) + prev-irradiance sampler (b1) + ray storage (b2) +
    // live sky SH storage buffer (b3).
    let trace = make_compute_layout(raw, &[cs, cs, si, vk::DescriptorType::STORAGE_BUFFER])?;
    let blend_irr = match make_compute_layout(raw, &[cs, si]) {
        Ok(layout) => layout,
        Err(err) => {
            // SAFETY: the ash seam. Free the prior layout on this partial-failure path.
            unsafe { raw.destroy_descriptor_set_layout(trace, None) };
            return Err(err);
        }
    };
    let blend_dist = match make_compute_layout(raw, &[cs, si]) {
        Ok(layout) => layout,
        Err(err) => {
            // SAFETY: the ash seam. Free the prior layouts.
            unsafe {
                raw.destroy_descriptor_set_layout(blend_irr, None);
                raw.destroy_descriptor_set_layout(trace, None);
            }
            return Err(err);
        }
    };
    let border = match make_compute_layout(raw, &[si]) {
        Ok(layout) => layout,
        Err(err) => {
            // SAFETY: the ash seam. Free the prior layouts.
            unsafe {
                raw.destroy_descriptor_set_layout(blend_dist, None);
                raw.destroy_descriptor_set_layout(blend_irr, None);
                raw.destroy_descriptor_set_layout(trace, None);
            }
            return Err(err);
        }
    };

    Ok(DdgiLayouts {
        trace,
        blend_irr,
        blend_dist,
        border,
    })
}

/// Allocates the five DDGI sets (the four compute sets + the mesh set 5) from the shared descriptor
/// pool. The sets are pool-owned (freed with the pool in teardown).
pub(super) fn allocate_sets(
    descriptors: &Descriptors,
    layouts: &DdgiLayouts,
) -> Result<(
    vk::DescriptorSet,
    vk::DescriptorSet,
    vk::DescriptorSet,
    vk::DescriptorSet,
    vk::DescriptorSet,
)> {
    let trace = descriptors.allocate_set(layouts.trace)?;
    let blend_irr = descriptors.allocate_set(layouts.blend_irr)?;
    let blend_dist = descriptors.allocate_set(layouts.blend_dist)?;
    let border = descriptors.allocate_set(layouts.border)?;
    let mesh = descriptors.allocate_set(descriptors.ddgi_mesh_set_layout())?;
    Ok((trace, blend_irr, blend_dist, border, mesh))
}

/// A compute set layout with one binding per `types` entry, in order.
pub(super) fn make_compute_layout(
    raw: &ash::Device,
    types: &[vk::DescriptorType],
) -> Result<vk::DescriptorSetLayout> {
    crate::vk_write::compute_layout(raw, types, "ddgi compute layout")
}

/// A single-mip, single-layer 2D color image with the given storage/sampled usage — the DDGI
/// atlases + ray image.
pub(super) fn make_storage_image(
    resources: &Arc<DeviceResources>,
    width: u32,
    height: u32,
    format: vk::Format,
    usage: vk::ImageUsageFlags,
) -> Result<Image> {
    Image::new(
        resources,
        &ImageDesc::color_2d(vk::Extent2D { width, height }, format, usage),
    )
}

/// The linear, clamp-to-edge sampler the mesh + trace passes read the atlases with.
pub(super) fn create_linear_clamp_sampler(raw: &ash::Device) -> Result<vk::Sampler> {
    let info = vk::SamplerCreateInfo::default()
        .mag_filter(vk::Filter::LINEAR)
        .min_filter(vk::Filter::LINEAR)
        .mipmap_mode(vk::SamplerMipmapMode::NEAREST)
        .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
        .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
        .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE);
    // SAFETY: the ash seam. The sampler is owned and freed in `Drop`.
    checked(unsafe { raw.create_sampler(&info, None) }, "ddgi sampler")
}

/// `UNDEFINED → GENERAL` barrier readying an image (1 mip, 1 layer, color) for the init clear (a
/// transfer write).
pub(super) fn clear_pre_barrier(image: vk::Image) -> vk::ImageMemoryBarrier2<'static> {
    vk::ImageMemoryBarrier2::default()
        .src_stage_mask(vk::PipelineStageFlags2::TOP_OF_PIPE)
        .src_access_mask(vk::AccessFlags2::empty())
        .dst_stage_mask(vk::PipelineStageFlags2::CLEAR)
        .dst_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
        .old_layout(vk::ImageLayout::UNDEFINED)
        .new_layout(vk::ImageLayout::GENERAL)
        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .image(image)
        .subresource_range(color_subresource())
}

/// `GENERAL → new_layout` barrier parking an image in its resting layout after the init clear,
/// making the cleared zeros visible to `dst_stage`/`dst_access`. The `GENERAL` source preserves the
/// cleared contents (unlike an `UNDEFINED` source, which the driver may discard).
pub(super) fn clear_post_barrier(
    image: vk::Image,
    new_layout: vk::ImageLayout,
    dst_stage: vk::PipelineStageFlags2,
    dst_access: vk::AccessFlags2,
) -> vk::ImageMemoryBarrier2<'static> {
    vk::ImageMemoryBarrier2::default()
        .src_stage_mask(vk::PipelineStageFlags2::CLEAR)
        .src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
        .dst_stage_mask(dst_stage)
        .dst_access_mask(dst_access)
        .old_layout(vk::ImageLayout::GENERAL)
        .new_layout(new_layout)
        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .image(image)
        .subresource_range(color_subresource())
}

/// The full color subresource range (1 mip, 1 layer) shared by the init barriers.
pub(super) fn color_subresource() -> vk::ImageSubresourceRange {
    vk::ImageSubresourceRange {
        aspect_mask: vk::ImageAspectFlags::COLOR,
        base_mip_level: 0,
        level_count: 1,
        base_array_layer: 0,
        layer_count: 1,
    }
}
