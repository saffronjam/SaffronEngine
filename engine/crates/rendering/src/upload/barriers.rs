use ash::vk;

/// Records the RGBA8 upload + full mip-chain generation for `image` into `cmd`: all
/// mips → `TRANSFER_DST`, copy mip 0, blit down the chain, then every mip → shader
/// read.
///
/// # Safety
///
/// `cmd` must be in the recording state; `image` (with `mip_levels` mips) and `src`
/// must outlive the submit that consumes `cmd`.
pub(super) unsafe fn record_texture_upload(
    raw: &ash::Device,
    cmd: vk::CommandBuffer,
    image: vk::Image,
    src: vk::Buffer,
    width: u32,
    height: u32,
    mip_levels: u32,
) {
    // All mips start TransferDst: mip 0 receives the copy, the rest receive blits.
    let to_dst = vk::ImageMemoryBarrier2::default()
        .src_stage_mask(vk::PipelineStageFlags2::TOP_OF_PIPE)
        .dst_stage_mask(vk::PipelineStageFlags2::COPY)
        .dst_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
        .old_layout(vk::ImageLayout::UNDEFINED)
        .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .image(image)
        .subresource_range(vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            base_mip_level: 0,
            level_count: mip_levels,
            base_array_layer: 0,
            layer_count: 1,
        });
    let to_dst = [to_dst];
    let dep = vk::DependencyInfo::default().image_memory_barriers(&to_dst);
    // SAFETY: the caller's recording contract; the image outlives the submit.
    unsafe { raw.cmd_pipeline_barrier2(cmd, &dep) };

    // SAFETY: as above; mip 0 is in TRANSFER_DST per the barrier.
    unsafe { copy_buffer_to_image(raw, cmd, src, image, width, height) };

    // SAFETY: as above; generates mips 1..n and transitions every level to read.
    unsafe { record_mip_chain(raw, cmd, image, width, height, mip_levels) };
}

/// Copies the whole of `src` into mip 0 of `image` (in `TRANSFER_DST`).
///
/// # Safety
///
/// `cmd` recording; `src`/`image` outlive the submit.
pub(super) unsafe fn copy_buffer_to_image(
    raw: &ash::Device,
    cmd: vk::CommandBuffer,
    src: vk::Buffer,
    image: vk::Image,
    width: u32,
    height: u32,
) {
    let region = vk::BufferImageCopy::default()
        .image_subresource(vk::ImageSubresourceLayers {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            mip_level: 0,
            base_array_layer: 0,
            layer_count: 1,
        })
        .image_extent(vk::Extent3D {
            width,
            height,
            depth: 1,
        });
    // SAFETY: the caller's recording contract.
    unsafe {
        raw.cmd_copy_buffer_to_image(
            cmd,
            src,
            image,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            &[region],
        );
    }
}

/// Copies `mip_level`'s `width`×`height` texels from `src` at `buffer_offset` into `image` (in
/// `TRANSFER_DST`) — the per-level path the min/max pyramid uses (each level is exact CPU data, so it
/// is copied directly rather than blitted).
///
/// # Safety
///
/// `cmd` recording; `src`/`image` outlive the submit; `buffer_offset` is aligned to the texel block.
pub(super) unsafe fn copy_buffer_to_image_mip(
    raw: &ash::Device,
    cmd: vk::CommandBuffer,
    src: vk::Buffer,
    image: vk::Image,
    buffer_offset: vk::DeviceSize,
    mip_level: u32,
    extent: vk::Extent2D,
) {
    let region = vk::BufferImageCopy::default()
        .buffer_offset(buffer_offset)
        .image_subresource(vk::ImageSubresourceLayers {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            mip_level,
            base_array_layer: 0,
            layer_count: 1,
        })
        .image_extent(vk::Extent3D {
            width: extent.width,
            height: extent.height,
            depth: 1,
        });
    // SAFETY: the caller's recording contract.
    unsafe {
        raw.cmd_copy_buffer_to_image(
            cmd,
            src,
            image,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            &[region],
        );
    }
}

/// Generates mips 1..`mip_levels` by blitting down from mip 0, then transitions every
/// level to `SHADER_READ_ONLY_OPTIMAL`. On entry every level is `TRANSFER_DST`.
///
/// # Safety
///
/// `cmd` recording; `image` (with `mip_levels` mips) outlives the submit.
pub(super) unsafe fn record_mip_chain(
    raw: &ash::Device,
    cmd: vk::CommandBuffer,
    image: vk::Image,
    width: u32,
    height: u32,
    mip_levels: u32,
) {
    let mut mw = width as i32;
    let mut mh = height as i32;
    for i in 1..mip_levels {
        // SAFETY: the caller's recording contract.
        unsafe {
            mip_barrier(
                raw,
                cmd,
                image,
                i - 1,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                vk::PipelineStageFlags2::COPY,
                vk::AccessFlags2::TRANSFER_WRITE,
                vk::PipelineStageFlags2::BLIT,
                vk::AccessFlags2::TRANSFER_READ,
            );
        }
        let nw = if mw > 1 { mw / 2 } else { 1 };
        let nh = if mh > 1 { mh / 2 } else { 1 };
        let blit = vk::ImageBlit::default()
            .src_subresource(vk::ImageSubresourceLayers {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                mip_level: i - 1,
                base_array_layer: 0,
                layer_count: 1,
            })
            .src_offsets([
                vk::Offset3D { x: 0, y: 0, z: 0 },
                vk::Offset3D { x: mw, y: mh, z: 1 },
            ])
            .dst_subresource(vk::ImageSubresourceLayers {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                mip_level: i,
                base_array_layer: 0,
                layer_count: 1,
            })
            .dst_offsets([
                vk::Offset3D { x: 0, y: 0, z: 0 },
                vk::Offset3D { x: nw, y: nh, z: 1 },
            ]);
        // SAFETY: the caller's recording contract; the blit reads mip i-1 (SRC) and
        // writes mip i (DST).
        unsafe {
            raw.cmd_blit_image(
                cmd,
                image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[blit],
                vk::Filter::LINEAR,
            );
        }
        mw = nw;
        mh = nh;
    }
    for i in 0..mip_levels {
        let last = i == mip_levels - 1;
        // The last level only received a copy/blit-dst (TRANSFER_DST); every earlier
        // level was a blit source (TRANSFER_SRC), so its source stage/access differ.
        let (from_layout, src_stage, src_access) = if last {
            (
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::PipelineStageFlags2::COPY,
                vk::AccessFlags2::TRANSFER_WRITE,
            )
        } else {
            (
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                vk::PipelineStageFlags2::BLIT,
                vk::AccessFlags2::TRANSFER_READ,
            )
        };
        // SAFETY: the caller's recording contract.
        unsafe {
            mip_barrier(
                raw,
                cmd,
                image,
                i,
                from_layout,
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                src_stage,
                src_access,
                vk::PipelineStageFlags2::FRAGMENT_SHADER,
                vk::AccessFlags2::SHADER_SAMPLED_READ,
            );
        }
    }
}

/// One sync2 image-memory barrier on a single mip level.
///
/// # Safety
///
/// `cmd` recording; `image` outlives the submit.
#[allow(clippy::too_many_arguments)]
pub(super) unsafe fn mip_barrier(
    raw: &ash::Device,
    cmd: vk::CommandBuffer,
    image: vk::Image,
    mip: u32,
    from: vk::ImageLayout,
    to: vk::ImageLayout,
    src_stage: vk::PipelineStageFlags2,
    src_access: vk::AccessFlags2,
    dst_stage: vk::PipelineStageFlags2,
    dst_access: vk::AccessFlags2,
) {
    let barrier = vk::ImageMemoryBarrier2::default()
        .src_stage_mask(src_stage)
        .src_access_mask(src_access)
        .dst_stage_mask(dst_stage)
        .dst_access_mask(dst_access)
        .old_layout(from)
        .new_layout(to)
        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .image(image)
        .subresource_range(vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            base_mip_level: mip,
            level_count: 1,
            base_array_layer: 0,
            layer_count: 1,
        });
    let barriers = [barrier];
    let dep = vk::DependencyInfo::default().image_memory_barriers(&barriers);
    // SAFETY: the caller's recording contract.
    unsafe { raw.cmd_pipeline_barrier2(cmd, &dep) };
}

/// One whole-image sync2 layout transition (single mip range), used by the float
/// (single-mip) texture path.
///
/// # Safety
///
/// `cmd` recording; `image` outlives the submit.
#[allow(clippy::too_many_arguments)]
pub(super) unsafe fn transition_image(
    raw: &ash::Device,
    cmd: vk::CommandBuffer,
    image: vk::Image,
    mip_levels: u32,
    from: vk::ImageLayout,
    to: vk::ImageLayout,
    src_stage: vk::PipelineStageFlags2,
    src_access: vk::AccessFlags2,
    dst_stage: vk::PipelineStageFlags2,
    dst_access: vk::AccessFlags2,
) {
    let barrier = vk::ImageMemoryBarrier2::default()
        .src_stage_mask(src_stage)
        .src_access_mask(src_access)
        .dst_stage_mask(dst_stage)
        .dst_access_mask(dst_access)
        .old_layout(from)
        .new_layout(to)
        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .image(image)
        .subresource_range(vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            base_mip_level: 0,
            level_count: mip_levels,
            base_array_layer: 0,
            layer_count: 1,
        });
    let barriers = [barrier];
    let dep = vk::DependencyInfo::default().image_memory_barriers(&barriers);
    // SAFETY: the caller's recording contract.
    unsafe { raw.cmd_pipeline_barrier2(cmd, &dep) };
}

/// One whole-image sync2 transition for every array layer of a single-mip image.
#[allow(clippy::too_many_arguments)]
pub(super) unsafe fn transition_image_layers(
    raw: &ash::Device,
    cmd: vk::CommandBuffer,
    image: vk::Image,
    layers: u32,
    from: vk::ImageLayout,
    to: vk::ImageLayout,
    src_stage: vk::PipelineStageFlags2,
    src_access: vk::AccessFlags2,
    dst_stage: vk::PipelineStageFlags2,
    dst_access: vk::AccessFlags2,
) {
    let barrier = vk::ImageMemoryBarrier2::default()
        .src_stage_mask(src_stage)
        .src_access_mask(src_access)
        .dst_stage_mask(dst_stage)
        .dst_access_mask(dst_access)
        .old_layout(from)
        .new_layout(to)
        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .image(image)
        .subresource_range(vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            base_mip_level: 0,
            level_count: 1,
            base_array_layer: 0,
            layer_count: layers,
        });
    let barriers = [barrier];
    let dependency = vk::DependencyInfo::default().image_memory_barriers(&barriers);
    // SAFETY: the caller's recording contract; the image outlives the submit.
    unsafe { raw.cmd_pipeline_barrier2(cmd, &dependency) };
}
