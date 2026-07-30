use std::sync::Arc;

use ash::vk;
use vk_mem::Alloc;

use super::barriers::transition_image;
use super::staging::StagingBuffer;
use super::{Uploader, float_to_half};
use crate::descriptors::Descriptors;
use crate::resources::{GpuLut, Image3D};
use crate::{Error, GradeUniform, Pipeline, Result, checked};

impl Uploader {
    /// Uploads a creative look-up table — `size³` red-fastest `[r, g, b]` triples — as an
    /// `R16G16B16A16_SFLOAT` `TYPE_3D` sampled image (`SAMPLED | TRANSFER_DST`, clamp addressed by the
    /// linear sampler at bind), returning the [`GpuLut`] the tonemap pass binds at set 0 binding 2. The
    /// half-float format removes banding on smooth skies/skin for a trivial VRAM cost and keeps the
    /// bake round-trip clean.
    ///
    /// # Errors
    ///
    /// [`Error::ZeroSizedImage`] for a zero size, or [`Error::Vk`] for a failing Vulkan/VMA call.
    pub fn upload_lut_3d(&self, rgb: &[[f32; 3]], size: u32) -> Result<Arc<GpuLut>> {
        if size == 0 {
            return Err(Error::ZeroSizedImage);
        }
        let count = (size as usize).pow(3);
        // RGBA16F, alpha = 1; narrow each channel to f16 with the same rounding the GPU produces.
        let mut half: Vec<u16> = Vec::with_capacity(count * 4);
        for texel in rgb.iter().take(count) {
            half.push(float_to_half(texel[0]));
            half.push(float_to_half(texel[1]));
            half.push(float_to_half(texel[2]));
            half.push(float_to_half(1.0));
        }
        // A short source pads to neutral opaque black rather than reading uninitialized bytes.
        half.resize(count * 4, float_to_half(0.0));
        let (image, view, allocation) = self.create_lut_3d_image(size, &half)?;
        Ok(Arc::new(GpuLut::from_parts(
            &self.resources,
            image,
            view,
            allocation,
            size,
        )))
    }

    /// Bakes the folded look — grade + view transform + creative LUT — into a `size³` display-referred
    /// table over the log2 shaper, on the GPU, and reads it back as red-fastest `[r, g, b]` f16 bits
    /// (alpha dropped). `pipeline` is [`crate::Pipelines::request_lut_bake`]; `grade` the frozen grade
    /// uniform (its `look` block carries the creative-LUT intensity + size); `creative_lut_view` the
    /// bound creative LUT (the identity default when none); `mode` the view/display transform. The
    /// transient output image, UBO, descriptor set, and readback buffer live only for this call.
    ///
    /// # Errors
    ///
    /// [`Error::Vk`] for a failing allocation/dispatch/readback.
    #[allow(clippy::too_many_arguments)]
    pub fn bake_look_lut(
        &self,
        descriptors: &Descriptors,
        pipeline: &Pipeline,
        grade: &GradeUniform,
        creative_lut_view: vk::ImageView,
        sampler: vk::Sampler,
        size: u32,
        mode: u32,
    ) -> Result<Vec<[u16; 3]>> {
        let extent = vk::Extent3D {
            width: size,
            height: size,
            depth: size,
        };
        let cell_count = (size as usize).pow(3);

        // The baked output: an rgba16f storage 3D image, read back after the dispatch.
        let output = Image3D::new(
            &self.resources,
            extent,
            vk::Format::R16G16B16A16_SFLOAT,
            1,
            vk::ImageUsageFlags::STORAGE | vk::ImageUsageFlags::TRANSFER_SRC,
        )?;

        // The frozen grade uniform, host-visible so the bake dispatch reads this exact grade.
        let range = size_of::<GradeUniform>() as vk::DeviceSize;
        let mut ubo = crate::Buffer::new(
            &self.resources,
            range,
            vk::BufferUsageFlags::UNIFORM_BUFFER,
            &vk_mem::AllocationCreateInfo {
                usage: vk_mem::MemoryUsage::Auto,
                flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE
                    | vk_mem::AllocationCreateFlags::MAPPED,
                ..Default::default()
            },
        )?;
        ubo.mapped_bytes()
            .expect("bake grade UBO is MAPPED")
            .copy_from_slice(bytemuck::bytes_of(grade));

        // Host-readable destination for the baked volume (rgba16f → four u16 per cell).
        let read_bytes = (cell_count * 8) as vk::DeviceSize;
        let allocator = self.allocator();
        let read_alloc = vk_mem::AllocationCreateInfo {
            usage: vk_mem::MemoryUsage::AutoPreferHost,
            flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_RANDOM
                | vk_mem::AllocationCreateFlags::MAPPED,
            ..Default::default()
        };
        let read_info = vk::BufferCreateInfo::default()
            .size(read_bytes.max(4))
            .usage(vk::BufferUsageFlags::TRANSFER_DST);
        // SAFETY: the VMA seam. Owned + freed below after the readback.
        let (read_buf, mut read_allocation) = checked(
            unsafe { allocator.create_buffer(&read_info, &read_alloc) },
            "vmaCreateBuffer (lut bake readback)",
        )?;

        let set = match descriptors.allocate_set(descriptors.tonemap_set_layout()) {
            Ok(set) => set,
            Err(err) => {
                // SAFETY: the VMA seam. Free the readback buffer once before the image/UBO drop.
                unsafe { allocator.destroy_buffer(read_buf, &mut read_allocation) };
                return Err(err);
            }
        };
        // binding 0: the storage output (GENERAL); binding 2: the creative LUT (SHADER_READ_ONLY).
        let out_info = [vk::DescriptorImageInfo {
            sampler: vk::Sampler::null(),
            image_view: output.view(),
            image_layout: vk::ImageLayout::GENERAL,
        }];
        let lut_info = [vk::DescriptorImageInfo {
            sampler,
            image_view: creative_lut_view,
            image_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
        }];
        let writes = [
            vk::WriteDescriptorSet::default()
                .dst_set(set)
                .dst_binding(0)
                .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                .image_info(&out_info),
            vk::WriteDescriptorSet::default()
                .dst_set(set)
                .dst_binding(2)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .image_info(&lut_info),
        ];
        // SAFETY: the ash seam. The set + views outlive the call; single-threaded here.
        unsafe { self.raw().update_descriptor_sets(&writes, &[]) };
        descriptors.write_dynamic_uniform_buffer(set, 1, ubo.handle(), range);

        let push: [u32; 2] = [size, mode];
        let groups = size.div_ceil(4);
        let handle = pipeline.handle();
        let layout = pipeline.layout();

        let recorded = self.with_one_off_commands("bake_look_lut", |cmd| {
            // SAFETY: the ash seam. Every resource outlives the submit-wait.
            unsafe {
                let raw = self.raw();
                transition_image(
                    raw,
                    cmd,
                    output.handle(),
                    1,
                    vk::ImageLayout::UNDEFINED,
                    vk::ImageLayout::GENERAL,
                    vk::PipelineStageFlags2::TOP_OF_PIPE,
                    vk::AccessFlags2::empty(),
                    vk::PipelineStageFlags2::COMPUTE_SHADER,
                    vk::AccessFlags2::SHADER_STORAGE_WRITE,
                );
                raw.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, handle);
                raw.cmd_bind_descriptor_sets(
                    cmd,
                    vk::PipelineBindPoint::COMPUTE,
                    layout,
                    0,
                    &[set],
                    &[0],
                );
                raw.cmd_push_constants(
                    cmd,
                    layout,
                    vk::ShaderStageFlags::COMPUTE,
                    0,
                    bytemuck::bytes_of(&push),
                );
                raw.cmd_dispatch(cmd, groups, groups, groups);
                transition_image(
                    raw,
                    cmd,
                    output.handle(),
                    1,
                    vk::ImageLayout::GENERAL,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    vk::PipelineStageFlags2::COMPUTE_SHADER,
                    vk::AccessFlags2::SHADER_STORAGE_WRITE,
                    vk::PipelineStageFlags2::COPY,
                    vk::AccessFlags2::TRANSFER_READ,
                );
                let region = vk::BufferImageCopy::default()
                    .image_subresource(vk::ImageSubresourceLayers {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        mip_level: 0,
                        base_array_layer: 0,
                        layer_count: 1,
                    })
                    .image_extent(extent);
                raw.cmd_copy_image_to_buffer(
                    cmd,
                    output.handle(),
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    read_buf,
                    &[region],
                );
            }
        });

        // SAFETY: the ash seam. Free the descriptor set (the submit completed or never ran).
        unsafe {
            let _ = self
                .raw()
                .free_descriptor_sets(descriptors.descriptor_pool(), &[set]);
        }

        let result = recorded.map(|()| {
            // SAFETY: the VMA seam. Make the GPU writes host-visible, then read four u16 per cell and
            // keep the rgb (alpha dropped).
            unsafe {
                let _ = allocator.invalidate_allocation(&read_allocation, 0, read_bytes);
                let ptr = allocator.get_allocation_info(&read_allocation).mapped_data as *const u16;
                let words = std::slice::from_raw_parts(ptr, cell_count * 4);
                words
                    .chunks_exact(4)
                    .map(|c| [c[0], c[1], c[2]])
                    .collect::<Vec<[u16; 3]>>()
            }
        });

        // SAFETY: the VMA seam. Free the readback buffer once; the image + UBO Drop after.
        unsafe { allocator.destroy_buffer(read_buf, &mut read_allocation) };
        result
    }

    /// Uploads the neutral identity LUT — a `2×2×2` ramp whose corner `i` is the RGB of that corner, so
    /// a tetrahedral sample of `c ∈ [0,1]` returns `c` unchanged. The always-bound default at binding 2
    /// when no creative look is assigned, so the tonemap shader never branches on presence.
    ///
    /// # Errors
    ///
    /// [`Error::Vk`] for a failing Vulkan/VMA call.
    pub fn upload_identity_lut(&self) -> Result<Arc<GpuLut>> {
        let mut rgb = Vec::with_capacity(8);
        for z in 0..2u32 {
            for y in 0..2u32 {
                for x in 0..2u32 {
                    rgb.push([x as f32, y as f32, z as f32]);
                }
            }
        }
        self.upload_lut_3d(&rgb, 2)
    }

    /// Creates an `R16G16B16A16_SFLOAT` `TYPE_3D` image of `size³` and uploads `half` (RGBA f16 bytes,
    /// red-fastest), leaving it `SHADER_READ_ONLY_OPTIMAL`. The shared body of
    /// [`Self::upload_lut_3d`] and the bake's readback source allocation.
    fn create_lut_3d_image(
        &self,
        size: u32,
        half: &[u16],
    ) -> Result<(vk::Image, vk::ImageView, vk_mem::Allocation)> {
        let extent = vk::Extent3D {
            width: size,
            height: size,
            depth: size,
        };
        let bytes = std::mem::size_of_val(half) as vk::DeviceSize;
        let mut staging = StagingBuffer::new(self.allocator(), bytes.max(4))?;
        staging.mapped_slice()[..bytes as usize].copy_from_slice(bytemuck::cast_slice(half));
        staging.flush();

        let format = vk::Format::R16G16B16A16_SFLOAT;
        let info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_3D)
            .format(format)
            .extent(extent)
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::SAMPLED)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        let alloc_info = vk_mem::AllocationCreateInfo {
            usage: vk_mem::MemoryUsage::Auto,
            flags: vk_mem::AllocationCreateFlags::DEDICATED_MEMORY,
            ..Default::default()
        };
        // SAFETY: the VMA seam. The create-infos are valid; the image is owned by the caller (the
        // returned `GpuLut`, or freed on a later failure).
        let (image, allocation) = checked(
            unsafe { self.allocator().create_image(&info, &alloc_info) },
            "vmaCreateImage (lut)",
        )?;

        let recorded = self.with_one_off_commands("create_lut_3d_image", |cmd| {
            // SAFETY: the ash seam. The image/staging buffer outlive the submit-wait.
            unsafe {
                let raw = self.raw();
                transition_image(
                    raw,
                    cmd,
                    image,
                    1,
                    vk::ImageLayout::UNDEFINED,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    vk::PipelineStageFlags2::TOP_OF_PIPE,
                    vk::AccessFlags2::empty(),
                    vk::PipelineStageFlags2::COPY,
                    vk::AccessFlags2::TRANSFER_WRITE,
                );
                let region = vk::BufferImageCopy::default()
                    .image_subresource(vk::ImageSubresourceLayers {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        mip_level: 0,
                        base_array_layer: 0,
                        layer_count: 1,
                    })
                    .image_extent(extent);
                raw.cmd_copy_buffer_to_image(
                    cmd,
                    staging.handle(),
                    image,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &[region],
                );
                transition_image(
                    raw,
                    cmd,
                    image,
                    1,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                    vk::PipelineStageFlags2::COPY,
                    vk::AccessFlags2::TRANSFER_WRITE,
                    vk::PipelineStageFlags2::FRAGMENT_SHADER
                        | vk::PipelineStageFlags2::COMPUTE_SHADER,
                    vk::AccessFlags2::SHADER_SAMPLED_READ,
                );
            }
        });
        drop(staging);
        if let Err(err) = recorded {
            self.destroy_image(image, allocation);
            return Err(err);
        }

        let view_info = vk::ImageViewCreateInfo::default()
            .image(image)
            .view_type(vk::ImageViewType::TYPE_3D)
            .format(format)
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            });
        // SAFETY: the ash seam. The view references the 3D image just uploaded.
        let view = match unsafe { self.raw().create_image_view(&view_info, None) } {
            Ok(view) => view,
            Err(result) => {
                self.destroy_image(image, allocation);
                return Err(Error::Vk {
                    context: "create_image_view (lut)",
                    result,
                });
            }
        };
        Ok((image, view, allocation))
    }
}
