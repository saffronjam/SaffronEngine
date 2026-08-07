//! The GPU min/max height-pyramid build: a compute pipeline over `height_minmax.spv` that
//! turns an uploaded height texture into its `R32G32_SFLOAT` conservative-bound pyramid
//! (min in R / max in G, one mip per level) with one init dispatch plus one reduce dispatch
//! per coarser mip. The pyramid must be point-sampled with explicit LOD — each texel is an
//! exact bound, never a filtered average.

use std::sync::Arc;

use ash::vk;
use vk_mem::Alloc;

use super::Uploader;
use super::bake_pipelines::compute_pipeline;
use super::barriers::{copy_buffer_to_image_mip, transition_image};
use super::staging::StagingBuffer;
use crate::resources::{DeviceResources, MinMaxPyramid};
use crate::{Result, checked};

/// The push-constant block `height_minmax.slang` reads (32 bytes, std430).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct PyramidPush {
    /// `x`/`y` dst level dims, `z` mode (`0` init from the height texture, `1` reduce).
    dst: [u32; 4],
    /// `x`/`y` src level dims (reduce only).
    src: [u32; 4],
}

const _: () = assert!(
    size_of::<PyramidPush>() == 32,
    "PyramidPush must be 32 bytes"
);

/// The min/max pyramid compute pipeline + its descriptor-set layout and pool. Owned by an
/// [`Uploader`] (built in [`Uploader::new`]) so the build runs on the upload path with the
/// SDF bake's one-off submission pattern.
pub(super) struct PyramidPipeline {
    resources: Arc<DeviceResources>,
    set_layout: vk::DescriptorSetLayout,
    pipeline_layout: vk::PipelineLayout,
    pool: vk::DescriptorPool,
    pipeline: vk::Pipeline,
}

impl PyramidPipeline {
    /// Builds the layout (1 sampled + 2 storage images), a `FREE_DESCRIPTOR_SET` pool sized
    /// for one full mip chain, and the compute pipeline from the runtime shader dir.
    pub(super) fn new(resources: &Arc<DeviceResources>) -> Result<Self> {
        let raw = resources.device();
        let binding = |b: u32, ty: vk::DescriptorType| {
            vk::DescriptorSetLayoutBinding::default()
                .binding(b)
                .descriptor_type(ty)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::COMPUTE)
        };
        let bindings = [
            binding(0, vk::DescriptorType::SAMPLED_IMAGE),
            binding(1, vk::DescriptorType::STORAGE_IMAGE),
            binding(2, vk::DescriptorType::STORAGE_IMAGE),
        ];
        let layout_info = vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings);
        // SAFETY: the ash seam. The bindings outlive the call; the layout is freed in `Drop`.
        let set_layout = checked(
            unsafe { raw.create_descriptor_set_layout(&layout_info, None) },
            "pyramid set layout",
        )?;

        let set_layouts = [set_layout];
        let push = [vk::PushConstantRange::default()
            .stage_flags(vk::ShaderStageFlags::COMPUTE)
            .offset(0)
            .size(size_of::<PyramidPush>() as u32)];
        let pl_info = vk::PipelineLayoutCreateInfo::default()
            .set_layouts(&set_layouts)
            .push_constant_ranges(&push);
        // SAFETY: the ash seam. The layout owns the set layout reference for the call.
        let pipeline_layout = match checked(
            unsafe { raw.create_pipeline_layout(&pl_info, None) },
            "pyramid pipeline layout",
        ) {
            Ok(layout) => layout,
            Err(err) => {
                // SAFETY: the ash seam. The set layout was created above; free it once.
                unsafe { raw.destroy_descriptor_set_layout(set_layout, None) };
                return Err(err);
            }
        };

        let pool_sizes = [
            vk::DescriptorPoolSize {
                ty: vk::DescriptorType::SAMPLED_IMAGE,
                descriptor_count: 32,
            },
            vk::DescriptorPoolSize {
                ty: vk::DescriptorType::STORAGE_IMAGE,
                descriptor_count: 64,
            },
        ];
        let pool_info = vk::DescriptorPoolCreateInfo::default()
            .flags(vk::DescriptorPoolCreateFlags::FREE_DESCRIPTOR_SET)
            .max_sets(32)
            .pool_sizes(&pool_sizes);
        // SAFETY: the ash seam. The pool is freed in `Drop`.
        let pool = match checked(
            unsafe { raw.create_descriptor_pool(&pool_info, None) },
            "pyramid pool",
        ) {
            Ok(pool) => pool,
            Err(err) => {
                // SAFETY: the ash seam. The layouts created above are freed once.
                unsafe {
                    raw.destroy_pipeline_layout(pipeline_layout, None);
                    raw.destroy_descriptor_set_layout(set_layout, None);
                }
                return Err(err);
            }
        };

        let dir = crate::pipelines::resolve_shader_dir();
        let pipeline = match compute_pipeline(raw, &dir, "height_minmax.spv", pipeline_layout) {
            Ok(pipeline) => pipeline,
            Err(err) => {
                // SAFETY: the ash seam. Free the pool + layouts on a pipeline-build failure.
                unsafe {
                    raw.destroy_descriptor_pool(pool, None);
                    raw.destroy_pipeline_layout(pipeline_layout, None);
                    raw.destroy_descriptor_set_layout(set_layout, None);
                }
                return Err(err);
            }
        };

        Ok(Self {
            resources: Arc::clone(resources),
            set_layout,
            pipeline_layout,
            pool,
            pipeline,
        })
    }
}

impl Drop for PyramidPipeline {
    fn drop(&mut self) {
        // SAFETY: the ash seam. The bundle keeps the device alive; each handle is freed once.
        unsafe {
            let raw = self.resources.device();
            raw.destroy_pipeline(self.pipeline, None);
            raw.destroy_pipeline_layout(self.pipeline_layout, None);
            raw.destroy_descriptor_pool(self.pool, None);
            raw.destroy_descriptor_set_layout(self.set_layout, None);
        }
    }
}

/// The dims of pyramid level `level` for a `width × height` base (Vulkan mip dims).
fn level_dims(width: u32, height: u32, level: u32) -> (u32, u32) {
    ((width >> level).max(1), (height >> level).max(1))
}

impl Uploader {
    /// Builds the min/max pyramid for an uploaded height texture on the GPU: one storage
    /// image with a mip per level, filled by the `height_minmax` dispatch chain, finished
    /// in `SHADER_READ_ONLY` for the tessellation factor kernel. Without the compute
    /// pipeline (missing/invalid SPIR-V, warned at [`Uploader::new`]) the pyramid degrades
    /// to a 1×1 `(0, 1)` bound — conservative, so displacement stays correct, just without
    /// per-region refinement.
    ///
    /// `texture` must already be uploaded and in `SHADER_READ_ONLY_OPTIMAL`.
    pub(super) fn build_height_pyramid(
        &self,
        texture: vk::Image,
        width: u32,
        height: u32,
    ) -> Result<MinMaxPyramid> {
        if self.pyramid.is_none() {
            return self.fallback_pyramid();
        }
        let mip_levels = super::texture::mip_count(width, height);
        let (image, allocation) = self.create_pyramid_image(width, height, mip_levels)?;

        let raw = self.raw();
        let mut mip_views: Vec<vk::ImageView> = Vec::with_capacity(mip_levels as usize);
        let mut full_view = vk::ImageView::null();
        let mut texture_view = vk::ImageView::null();
        let mut sets: Vec<vk::DescriptorSet> = Vec::new();

        let built = (|| -> Result<()> {
            let pyramid = self.pyramid.as_ref().expect("checked above");
            for level in 0..mip_levels {
                mip_views.push(self.pyramid_view(image, level, 1)?);
            }
            full_view = self.pyramid_view(image, 0, mip_levels)?;
            texture_view = self.texture_mip0_view(texture)?;

            let layouts = vec![pyramid.set_layout; mip_levels as usize];
            let info = vk::DescriptorSetAllocateInfo::default()
                .descriptor_pool(pyramid.pool)
                .set_layouts(&layouts);
            // SAFETY: the ash seam. The layouts outlive the call; the sets are freed below.
            sets = checked(
                unsafe { raw.allocate_descriptor_sets(&info) },
                "allocate pyramid sets",
            )?;
            for (level, &set) in sets.iter().enumerate() {
                let src = mip_views[level.saturating_sub(1)];
                write_pyramid_set(raw, set, texture_view, src, mip_views[level]);
            }

            self.with_one_off_commands("build_height_pyramid", |cmd| {
                // SAFETY: the ash seam. Every resource outlives the submit-wait.
                unsafe {
                    transition_image(
                        raw,
                        cmd,
                        image,
                        mip_levels,
                        vk::ImageLayout::UNDEFINED,
                        vk::ImageLayout::GENERAL,
                        vk::PipelineStageFlags2::TOP_OF_PIPE,
                        vk::AccessFlags2::empty(),
                        vk::PipelineStageFlags2::COMPUTE_SHADER,
                        vk::AccessFlags2::SHADER_STORAGE_WRITE,
                    );
                    raw.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, pyramid.pipeline);
                    for (level, &set) in sets.iter().enumerate() {
                        if level > 0 {
                            compute_chain_barrier(raw, cmd);
                        }
                        let (dw, dh) = level_dims(width, height, level as u32);
                        let (sw, sh) = level_dims(width, height, (level as u32).saturating_sub(1));
                        let push = PyramidPush {
                            dst: [dw, dh, u32::from(level > 0), 0],
                            src: [sw, sh, 0, 0],
                        };
                        raw.cmd_bind_descriptor_sets(
                            cmd,
                            vk::PipelineBindPoint::COMPUTE,
                            pyramid.pipeline_layout,
                            0,
                            &[set],
                            &[],
                        );
                        raw.cmd_push_constants(
                            cmd,
                            pyramid.pipeline_layout,
                            vk::ShaderStageFlags::COMPUTE,
                            0,
                            bytemuck::bytes_of(&push),
                        );
                        raw.cmd_dispatch(cmd, dw.div_ceil(8), dh.div_ceil(8), 1);
                    }
                    transition_image(
                        raw,
                        cmd,
                        image,
                        mip_levels,
                        vk::ImageLayout::GENERAL,
                        vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                        vk::PipelineStageFlags2::COMPUTE_SHADER,
                        vk::AccessFlags2::SHADER_STORAGE_WRITE,
                        vk::PipelineStageFlags2::COMPUTE_SHADER,
                        vk::AccessFlags2::SHADER_SAMPLED_READ,
                    );
                }
            })
        })();

        // The submission has completed (the one-off waits on its fence): free the transient
        // per-dispatch state in every outcome.
        if !sets.is_empty() {
            let pyramid = self.pyramid.as_ref().expect("checked above");
            // SAFETY: the ash seam. The sets came from this FREE pool and are no longer in use.
            let _ = unsafe { raw.free_descriptor_sets(pyramid.pool, &sets) };
        }
        // SAFETY: the ash seam. The transient views are unused past the fence wait; freed once.
        unsafe {
            for view in mip_views {
                raw.destroy_image_view(view, None);
            }
            if texture_view != vk::ImageView::null() {
                raw.destroy_image_view(texture_view, None);
            }
        }
        match built {
            Ok(()) => Ok(MinMaxPyramid {
                image,
                view: full_view,
                allocation,
            }),
            Err(err) => {
                // SAFETY: the ash seam. The full view (when created) is freed once with the image.
                unsafe {
                    if full_view != vk::ImageView::null() {
                        raw.destroy_image_view(full_view, None);
                    }
                }
                self.destroy_image(image, allocation);
                Err(err)
            }
        }
    }

    /// The 1×1 `(0, 1)` staged pyramid used when the compute pipeline is unavailable: the
    /// loosest conservative bound, so the prism march stays correct with no refinement.
    fn fallback_pyramid(&self) -> Result<MinMaxPyramid> {
        let (image, allocation) = self.create_pyramid_image(1, 1, 1)?;
        let staged = (|| -> Result<()> {
            let texel: [f32; 2] = [0.0, 1.0];
            let bytes = std::mem::size_of_val(&texel) as vk::DeviceSize;
            let mut staging = StagingBuffer::new(self.allocator(), bytes)?;
            staging.mapped_slice()[..bytes as usize].copy_from_slice(bytemuck::cast_slice(&texel));
            staging.flush();
            let result = self.with_one_off_commands("fallback_height_pyramid", |cmd| {
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
                    copy_buffer_to_image_mip(
                        raw,
                        cmd,
                        staging.handle(),
                        image,
                        0,
                        0,
                        vk::Extent2D {
                            width: 1,
                            height: 1,
                        },
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
                        vk::PipelineStageFlags2::COMPUTE_SHADER,
                        vk::AccessFlags2::SHADER_SAMPLED_READ,
                    );
                }
            });
            drop(staging);
            result
        })();
        if let Err(err) = staged {
            self.destroy_image(image, allocation);
            return Err(err);
        }
        let view = match self.pyramid_view(image, 0, 1) {
            Ok(view) => view,
            Err(err) => {
                self.destroy_image(image, allocation);
                return Err(err);
            }
        };
        Ok(MinMaxPyramid {
            image,
            view,
            allocation,
        })
    }

    /// Creates the `R32G32_SFLOAT` sampled + storage pyramid image with `mip_levels` mips.
    fn create_pyramid_image(
        &self,
        width: u32,
        height: u32,
        mip_levels: u32,
    ) -> Result<(vk::Image, vk_mem::Allocation)> {
        let info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(vk::Format::R32G32_SFLOAT)
            .extent(vk::Extent3D {
                width,
                height,
                depth: 1,
            })
            .mip_levels(mip_levels)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(
                vk::ImageUsageFlags::TRANSFER_DST
                    | vk::ImageUsageFlags::SAMPLED
                    | vk::ImageUsageFlags::STORAGE,
            )
            .initial_layout(vk::ImageLayout::UNDEFINED);
        let alloc_info = vk_mem::AllocationCreateInfo {
            usage: vk_mem::MemoryUsage::Auto,
            flags: vk_mem::AllocationCreateFlags::DEDICATED_MEMORY,
            ..Default::default()
        };
        // SAFETY: the VMA seam. The create-infos are valid; the image is owned by the
        // returned pyramid (or freed by the caller on a later failure).
        checked(
            unsafe { self.allocator().create_image(&info, &alloc_info) },
            "vmaCreateImage (min/max pyramid)",
        )
    }

    /// A view over `level_count` pyramid mips starting at `base_mip`.
    fn pyramid_view(
        &self,
        image: vk::Image,
        base_mip: u32,
        level_count: u32,
    ) -> Result<vk::ImageView> {
        let info = vk::ImageViewCreateInfo::default()
            .image(image)
            .view_type(vk::ImageViewType::TYPE_2D)
            .format(vk::Format::R32G32_SFLOAT)
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: base_mip,
                level_count,
                base_array_layer: 0,
                layer_count: 1,
            });
        // SAFETY: the ash seam. The view references a live pyramid image; freed by the caller.
        checked(
            unsafe { self.raw().create_image_view(&info, None) },
            "create_image_view (min/max pyramid)",
        )
    }

    /// A transient mip-0 sampled view over the uploaded height texture for the init dispatch.
    fn texture_mip0_view(&self, texture: vk::Image) -> Result<vk::ImageView> {
        let info = vk::ImageViewCreateInfo::default()
            .image(texture)
            .view_type(vk::ImageViewType::TYPE_2D)
            .format(vk::Format::R8G8B8A8_UNORM)
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            });
        // SAFETY: the ash seam. The view references the uploaded height texture; freed after
        // the build's fence wait.
        checked(
            unsafe { self.raw().create_image_view(&info, None) },
            "create_image_view (height mip 0)",
        )
    }
}

/// Writes one dispatch's set: the height texture (init source), the src mip, the dst mip.
fn write_pyramid_set(
    raw: &ash::Device,
    set: vk::DescriptorSet,
    texture_view: vk::ImageView,
    src: vk::ImageView,
    dst: vk::ImageView,
) {
    let sampled = [vk::DescriptorImageInfo {
        sampler: vk::Sampler::null(),
        image_view: texture_view,
        image_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
    }];
    let storage = |view: vk::ImageView| {
        [vk::DescriptorImageInfo {
            sampler: vk::Sampler::null(),
            image_view: view,
            image_layout: vk::ImageLayout::GENERAL,
        }]
    };
    let src_info = storage(src);
    let dst_info = storage(dst);
    let writes = [
        vk::WriteDescriptorSet::default()
            .dst_set(set)
            .dst_binding(0)
            .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
            .image_info(&sampled),
        vk::WriteDescriptorSet::default()
            .dst_set(set)
            .dst_binding(1)
            .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
            .image_info(&src_info),
        vk::WriteDescriptorSet::default()
            .dst_set(set)
            .dst_binding(2)
            .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
            .image_info(&dst_info),
    ];
    // SAFETY: the ash seam. The set + all referenced views outlive the call.
    unsafe { raw.update_descriptor_sets(&writes, &[]) };
}

/// A compute→compute barrier between reduce dispatches: the finer mip's storage writes are
/// made visible to the next level's reads.
fn compute_chain_barrier(raw: &ash::Device, cmd: vk::CommandBuffer) {
    let barrier = vk::MemoryBarrier2::default()
        .src_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER)
        .src_access_mask(vk::AccessFlags2::SHADER_STORAGE_WRITE)
        .dst_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER)
        .dst_access_mask(vk::AccessFlags2::SHADER_STORAGE_READ);
    let barriers = [barrier];
    let dep = vk::DependencyInfo::default().memory_barriers(&barriers);
    // SAFETY: the caller's recording contract.
    unsafe { raw.cmd_pipeline_barrier2(cmd, &dep) };
}
