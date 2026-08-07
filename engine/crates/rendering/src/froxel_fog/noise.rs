//! The tiling fBm detail-noise volume the fog density modulates by, baked once on the GPU.

use super::*;

/// A periodic (tiling) value-noise fBm sample in `[0, 1]` at normalized position `p ∈ [0, 1)³`. Each
/// octave's lattice period divides [`NOISE_DIM`] and wraps, so the baked volume tiles seamlessly under
/// the inject pass's repeat sampler.
pub(super) fn tiling_fbm(px: f32, py: f32, pz: f32) -> f32 {
    fn hash(x: i32, y: i32, z: i32, period: i32) -> f32 {
        let xi = x.rem_euclid(period) as u32;
        let yi = y.rem_euclid(period) as u32;
        let zi = z.rem_euclid(period) as u32;
        let mut h =
            xi.wrapping_mul(374761393) ^ yi.wrapping_mul(668265263) ^ zi.wrapping_mul(1274126177);
        h = (h ^ (h >> 13)).wrapping_mul(1274126177);
        h ^= h >> 16;
        (h & 0xFFFF) as f32 / 65535.0
    }
    fn octave(px: f32, py: f32, pz: f32, freq: i32) -> f32 {
        let fx = px * freq as f32;
        let fy = py * freq as f32;
        let fz = pz * freq as f32;
        let (x0, y0, z0) = (fx.floor() as i32, fy.floor() as i32, fz.floor() as i32);
        let smooth = |t: f32| t * t * (3.0 - 2.0 * t);
        let (tx, ty, tz) = (
            smooth(fx - x0 as f32),
            smooth(fy - y0 as f32),
            smooth(fz - z0 as f32),
        );
        let c = |dx, dy, dz| hash(x0 + dx, y0 + dy, z0 + dz, freq);
        let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t;
        let x00 = lerp(c(0, 0, 0), c(1, 0, 0), tx);
        let x10 = lerp(c(0, 1, 0), c(1, 1, 0), tx);
        let x01 = lerp(c(0, 0, 1), c(1, 0, 1), tx);
        let x11 = lerp(c(0, 1, 1), c(1, 1, 1), tx);
        lerp(lerp(x00, x10, ty), lerp(x01, x11, ty), tz)
    }
    let mut sum = 0.0;
    let mut amp = 0.5;
    let mut norm = 0.0;
    for &freq in &[4, 8, 16, 32] {
        sum += amp * octave(px, py, pz, freq);
        norm += amp;
        amp *= 0.5;
    }
    (sum / norm).clamp(0.0, 1.0)
}

/// Bakes the tiling erosion-noise volume on the CPU and uploads it into a device-local `R8_UNORM`
/// [`Image3D`] via a one-shot staged copy, resting it in `SHADER_READ_ONLY_OPTIMAL` for the inject
/// pass's linear-repeat fetch.
pub(super) fn bake_noise_volume(
    device: &Device,
    resources: &Arc<DeviceResources>,
) -> crate::Result<Image3D> {
    let dim = NOISE_DIM as usize;
    let mut data = vec![0u8; dim * dim * dim];
    for z in 0..dim {
        for y in 0..dim {
            for x in 0..dim {
                let n = tiling_fbm(
                    x as f32 / dim as f32,
                    y as f32 / dim as f32,
                    z as f32 / dim as f32,
                );
                data[x + y * dim + z * dim * dim] = (n * 255.0).round() as u8;
            }
        }
    }

    let extent = vk::Extent3D {
        width: NOISE_DIM,
        height: NOISE_DIM,
        depth: NOISE_DIM,
    };
    let mut noise = Image3D::new(
        resources,
        extent,
        vk::Format::R8_UNORM,
        1,
        vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_DST,
    )?;

    let staging = {
        let alloc = vk_mem::AllocationCreateInfo {
            usage: vk_mem::MemoryUsage::Auto,
            flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE
                | vk_mem::AllocationCreateFlags::MAPPED,
            ..Default::default()
        };
        let buf = Buffer::new(
            resources,
            data.len() as vk::DeviceSize,
            vk::BufferUsageFlags::TRANSFER_SRC,
            &alloc,
        )?;
        // SAFETY: the staging buffer is HOST_VISIBLE + MAPPED and sized for `data`.
        unsafe {
            std::ptr::copy_nonoverlapping(data.as_ptr(), buf.mapped_ptr(), data.len());
        }
        buf
    };

    let raw = device.raw();
    let pool_info = vk::CommandPoolCreateInfo::default()
        .flags(vk::CommandPoolCreateFlags::TRANSIENT)
        .queue_family_index(device.graphics_queue_family);
    // SAFETY: the ash seam. The pool is created, used, and destroyed within this call.
    let pool = checked(
        unsafe { raw.create_command_pool(&pool_info, None) },
        "froxel noise pool",
    )?;
    let result = (|| -> crate::Result<()> {
        let alloc = vk::CommandBufferAllocateInfo::default()
            .command_pool(pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        // SAFETY: the ash seam. One buffer from the private pool.
        let cmd = checked(
            unsafe { raw.allocate_command_buffers(&alloc) },
            "froxel noise cmd",
        )?[0];
        let range = vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            base_mip_level: 0,
            level_count: 1,
            base_array_layer: 0,
            layer_count: 1,
        };
        let to_dst = vk::ImageMemoryBarrier2::default()
            .src_stage_mask(vk::PipelineStageFlags2::TOP_OF_PIPE)
            .src_access_mask(vk::AccessFlags2::empty())
            .dst_stage_mask(vk::PipelineStageFlags2::COPY)
            .dst_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
            .old_layout(vk::ImageLayout::UNDEFINED)
            .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(noise.handle())
            .subresource_range(range);
        let to_read = vk::ImageMemoryBarrier2::default()
            .src_stage_mask(vk::PipelineStageFlags2::COPY)
            .src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
            .dst_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER)
            .dst_access_mask(vk::AccessFlags2::SHADER_SAMPLED_READ)
            .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(noise.handle())
            .subresource_range(range);
        let copy = vk::BufferImageCopy::default()
            .image_subresource(
                vk::ImageSubresourceLayers::default()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .mip_level(0)
                    .base_array_layer(0)
                    .layer_count(1),
            )
            .image_extent(extent);
        let begin = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
        let dst_barriers = [to_dst];
        let read_barriers = [to_read];
        let copies = [copy];
        // SAFETY: the ash seam. Records the two barriers + the staged copy on the fresh buffer.
        unsafe {
            checked(raw.begin_command_buffer(cmd, &begin), "froxel noise begin")?;
            let dep_dst = vk::DependencyInfo::default().image_memory_barriers(&dst_barriers);
            raw.cmd_pipeline_barrier2(cmd, &dep_dst);
            raw.cmd_copy_buffer_to_image(
                cmd,
                staging.handle(),
                noise.handle(),
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &copies,
            );
            let dep_read = vk::DependencyInfo::default().image_memory_barriers(&read_barriers);
            raw.cmd_pipeline_barrier2(cmd, &dep_read);
            checked(raw.end_command_buffer(cmd), "froxel noise end")?;
        }
        let cmd_infos = [vk::CommandBufferSubmitInfo::default().command_buffer(cmd)];
        let submits = [vk::SubmitInfo2::default().command_buffer_infos(&cmd_infos)];
        // SAFETY: the ash seam. The graphics queue is idle at init; drain with wait_idle below.
        device
            .graphics_queue
            .submit2(raw, &submits, vk::Fence::null(), "froxel noise submit")?;
        device.wait_idle()?;
        Ok(())
    })();
    // SAFETY: the ash seam. The queue was drained above, so the pool is idle.
    unsafe { raw.destroy_command_pool(pool, None) };
    result?;
    noise.layout = vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL;
    Ok(noise)
}
