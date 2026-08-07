//! Descriptor-set-layout, buffer, and descriptor-write assembly shared by the pass sub-states.
//!
//! Every write here targets one binding the set's layout declares. They are recorded
//! single-threaded at the frame build point, after the frame's fence is waited, so no lock guards
//! the externally-synchronized `vkUpdateDescriptorSets`.

use std::sync::Arc;

use ash::vk;

use crate::resources::{Buffer, DeviceResources};
use crate::{Result, checked};

/// A compute set layout whose bindings are `types` in order, one descriptor each.
pub(crate) fn compute_layout(
    raw: &ash::Device,
    types: &[vk::DescriptorType],
    context: &'static str,
) -> Result<vk::DescriptorSetLayout> {
    let counted: Vec<(vk::DescriptorType, u32)> = types.iter().map(|&ty| (ty, 1)).collect();
    compute_layout_counted(raw, &counted, context)
}

/// A compute set layout whose bindings are the given `(index, type)` pairs, one descriptor each —
/// the sparse form, for a layout that leaves binding indices unused.
pub(crate) fn compute_layout_sparse(
    raw: &ash::Device,
    bindings: &[(u32, vk::DescriptorType)],
    context: &'static str,
) -> Result<vk::DescriptorSetLayout> {
    let bindings: Vec<vk::DescriptorSetLayoutBinding> = bindings
        .iter()
        .map(|&(binding, ty)| {
            vk::DescriptorSetLayoutBinding::default()
                .binding(binding)
                .descriptor_type(ty)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::COMPUTE)
        })
        .collect();
    create_layout(raw, &bindings, context)
}

/// A compute set layout whose bindings are `(type, count)` in order — the array-descriptor form.
pub(crate) fn compute_layout_counted(
    raw: &ash::Device,
    bindings: &[(vk::DescriptorType, u32)],
    context: &'static str,
) -> Result<vk::DescriptorSetLayout> {
    let bindings: Vec<vk::DescriptorSetLayoutBinding> = bindings
        .iter()
        .enumerate()
        .map(|(i, &(ty, count))| {
            vk::DescriptorSetLayoutBinding::default()
                .binding(i as u32)
                .descriptor_type(ty)
                .descriptor_count(count)
                .stage_flags(vk::ShaderStageFlags::COMPUTE)
        })
        .collect();
    create_layout(raw, &bindings, context)
}

fn create_layout(
    raw: &ash::Device,
    bindings: &[vk::DescriptorSetLayoutBinding],
    context: &'static str,
) -> Result<vk::DescriptorSetLayout> {
    let info = vk::DescriptorSetLayoutCreateInfo::default().bindings(bindings);
    // SAFETY: the ash seam. The bindings outlive the call; the layout is freed in the owner's
    // `Drop` (or its partial-failure cleanup).
    checked(
        unsafe { raw.create_descriptor_set_layout(&info, None) },
        context,
    )
}

/// A device-local buffer of `size` bytes with the given usage — never host-mapped.
pub(crate) fn device_buffer(
    resources: &Arc<DeviceResources>,
    size: vk::DeviceSize,
    usage: vk::BufferUsageFlags,
) -> Result<Buffer> {
    let alloc_info = vk_mem::AllocationCreateInfo {
        usage: vk_mem::MemoryUsage::AutoPreferDevice,
        ..Default::default()
    };
    Buffer::new(resources, size, usage, &alloc_info)
}

/// A persistently host-mapped buffer of `size` bytes. `access` picks the host-access pattern —
/// `HOST_ACCESS_RANDOM` for a buffer the host rewrites in place, `HOST_ACCESS_SEQUENTIAL_WRITE` for
/// one it refills front to back.
pub(crate) fn mapped_buffer(
    resources: &Arc<DeviceResources>,
    size: vk::DeviceSize,
    usage: vk::BufferUsageFlags,
    access: vk_mem::AllocationCreateFlags,
) -> Result<Buffer> {
    let alloc_info = vk_mem::AllocationCreateInfo {
        usage: vk_mem::MemoryUsage::Auto,
        flags: access | vk_mem::AllocationCreateFlags::MAPPED,
        ..Default::default()
    };
    Buffer::new(resources, size, usage, &alloc_info)
}

/// Writes the storage-buffer range `[offset, offset + size)` into `(set, binding)`.
pub(crate) fn write_storage_buffer(
    raw: &ash::Device,
    set: vk::DescriptorSet,
    binding: u32,
    buffer: vk::Buffer,
    offset: vk::DeviceSize,
    size: vk::DeviceSize,
) {
    let info = [vk::DescriptorBufferInfo {
        buffer,
        offset,
        range: size,
    }];
    let write = vk::WriteDescriptorSet::default()
        .dst_set(set)
        .dst_binding(binding)
        .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
        .buffer_info(&info);
    // SAFETY: the ash seam. The set + buffer outlive the call.
    unsafe { raw.update_descriptor_sets(&[write], &[]) };
}

/// Writes the uniform-buffer range `[offset, offset + range)` into `(set, binding)`.
pub(crate) fn write_uniform_buffer(
    raw: &ash::Device,
    set: vk::DescriptorSet,
    binding: u32,
    buffer: vk::Buffer,
    offset: vk::DeviceSize,
    range: vk::DeviceSize,
) {
    let info = [vk::DescriptorBufferInfo {
        buffer,
        offset,
        range,
    }];
    let write = vk::WriteDescriptorSet::default()
        .dst_set(set)
        .dst_binding(binding)
        .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER)
        .buffer_info(&info);
    // SAFETY: the ash seam. The set + buffer outlive the call.
    unsafe { raw.update_descriptor_sets(&[write], &[]) };
}

/// Writes a storage image into `(set, binding)` at `layout` (no sampler).
pub(crate) fn write_storage_image(
    raw: &ash::Device,
    set: vk::DescriptorSet,
    binding: u32,
    view: vk::ImageView,
    layout: vk::ImageLayout,
) {
    let info = [vk::DescriptorImageInfo::default()
        .image_view(view)
        .image_layout(layout)];
    let write = vk::WriteDescriptorSet::default()
        .dst_set(set)
        .dst_binding(binding)
        .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
        .image_info(&info);
    // SAFETY: the ash seam. The set + view outlive the call.
    unsafe { raw.update_descriptor_sets(&[write], &[]) };
}

/// Writes a combined-image-sampler into `(set, binding)` at `layout`.
pub(crate) fn write_combined_sampler(
    raw: &ash::Device,
    set: vk::DescriptorSet,
    binding: u32,
    view: vk::ImageView,
    layout: vk::ImageLayout,
    sampler: vk::Sampler,
) {
    let info = [vk::DescriptorImageInfo::default()
        .sampler(sampler)
        .image_view(view)
        .image_layout(layout)];
    let write = vk::WriteDescriptorSet::default()
        .dst_set(set)
        .dst_binding(binding)
        .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
        .image_info(&info);
    // SAFETY: the ash seam. The set + view + sampler outlive the call.
    unsafe { raw.update_descriptor_sets(&[write], &[]) };
}
