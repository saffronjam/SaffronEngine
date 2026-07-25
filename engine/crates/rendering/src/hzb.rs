//! Hierarchical-Z pyramids for occlusion visibility.
//!
//! Each view owns two full-mip R32F max pyramids sized to the input extent: the one
//! built this frame ("current") and last frame's completed pyramid ("previous"), swapped
//! at frame start. The build runs as graph compute passes after the scene pass: a copy
//! seeds mip 0 from the 1x scene depth, then one reduce per level folds a conservative
//! MAX (depth convention LESS, far = 1.0, so a box whose nearest depth exceeds the
//! covering tile's value is occluded). Visibility tests established candidates against
//! the previous pyramid; a resize or explicit history invalidation clears validity so
//! tests bypass a stale pyramid.

use std::sync::Arc;

use ash::vk;

use crate::descriptors::Descriptors;
use crate::device::Device;
use crate::nested_scopes::NestedScopeRecorder;
use crate::render_graph::{RenderGraph, RgPass, RgResource, RgUsage};
use crate::resources::{DeviceResources, Image, ImageDesc};
use crate::{Result, checked};

/// Pyramid mip ceiling (32k × 32k source).
pub const HZB_MAX_MIPS: usize = 16;

/// The build push: source and destination texel extents.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct HzbPush {
    src_extent: [u32; 2],
    dst_extent: [u32; 2],
}

const _: () = assert!(size_of::<HzbPush>() == 16);

/// The byte size of the build push, for the PSO push ranges.
pub const HZB_PUSH_SIZE: u32 = size_of::<HzbPush>() as u32;

/// Device-shared HZB scaffolding: the depth sampler and the two build set layouts.
pub struct Hzb {
    resources: Arc<DeviceResources>,
    sampler: vk::Sampler,
    copy_layout: vk::DescriptorSetLayout,
    reduce_layout: vk::DescriptorSetLayout,
}

impl Hzb {
    /// Creates the sampler and the copy/reduce set layouts.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Vk`] for any failing Vulkan call; partially created handles are
    /// freed before returning.
    pub fn new(device: &Device) -> Result<Self> {
        let raw = device.raw();
        let sampler_info = vk::SamplerCreateInfo::default()
            .mag_filter(vk::Filter::NEAREST)
            .min_filter(vk::Filter::NEAREST)
            .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
            .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
            .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE);
        // SAFETY: the ash seam. Freed in `Drop`.
        let sampler = checked(
            unsafe { raw.create_sampler(&sampler_info, None) },
            "hzb sampler",
        )?;
        let copy_layout = match make_layout(
            raw,
            &[
                vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
                vk::DescriptorType::STORAGE_IMAGE,
            ],
        ) {
            Ok(layout) => layout,
            Err(err) => {
                // SAFETY: the ash seam. Free the sampler on this partial-failure path.
                unsafe { raw.destroy_sampler(sampler, None) };
                return Err(err);
            }
        };
        let reduce_layout = match make_layout(
            raw,
            &[
                vk::DescriptorType::STORAGE_IMAGE,
                vk::DescriptorType::STORAGE_IMAGE,
            ],
        ) {
            Ok(layout) => layout,
            Err(err) => {
                // SAFETY: the ash seam. Free prior handles on this partial-failure path.
                unsafe {
                    raw.destroy_descriptor_set_layout(copy_layout, None);
                    raw.destroy_sampler(sampler, None);
                }
                return Err(err);
            }
        };
        Ok(Self {
            resources: Arc::clone(device.resources()),
            sampler,
            copy_layout,
            reduce_layout,
        })
    }

    /// The copy (depth → mip 0) set layout.
    pub fn copy_layout(&self) -> vk::DescriptorSetLayout {
        self.copy_layout
    }

    /// The reduce (mip N-1 → mip N) set layout.
    pub fn reduce_layout(&self) -> vk::DescriptorSetLayout {
        self.reduce_layout
    }
}

impl Drop for Hzb {
    fn drop(&mut self) {
        let raw = self.resources.device();
        // SAFETY: the ash seam. Teardown after `wait_gpu_idle`.
        unsafe {
            raw.destroy_descriptor_set_layout(self.reduce_layout, None);
            raw.destroy_descriptor_set_layout(self.copy_layout, None);
            raw.destroy_sampler(self.sampler, None);
        }
    }
}

/// One pyramid's images, per-mip storage views, and build sets.
struct HzbTarget {
    image: Image,
    mip_views: Vec<vk::ImageView>,
    /// Set 0: the copy set; sets 1.. are per-destination-mip reduce sets.
    sets: Vec<vk::DescriptorSet>,
    /// The image's actual layout (UNDEFINED before its first build, GENERAL after).
    layout: vk::ImageLayout,
    valid: bool,
}

/// A view's ping-pong HZB pair.
pub struct HzbPyramid {
    resources: Arc<DeviceResources>,
    targets: [HzbTarget; 2],
    current: usize,
    extent: vk::Extent2D,
    mip_count: u32,
}

fn mip_count_for(extent: vk::Extent2D) -> u32 {
    let longest = extent.width.max(extent.height).max(1);
    (32 - longest.leading_zeros()).min(HZB_MAX_MIPS as u32)
}

fn mip_extent(extent: vk::Extent2D, mip: u32) -> vk::Extent2D {
    vk::Extent2D {
        width: (extent.width >> mip).max(1),
        height: (extent.height >> mip).max(1),
    }
}

impl HzbPyramid {
    /// Builds the two pyramids for `extent`, allocating per-mip views and build sets
    /// and writing the stable storage bindings (the depth binding is per-frame).
    ///
    /// # Errors
    ///
    /// Returns [`Error::Vk`] on image/view creation or set allocation failure.
    pub fn new(
        device: &Device,
        descriptors: &Descriptors,
        hzb: &Hzb,
        extent: vk::Extent2D,
    ) -> Result<Self> {
        let mip_count = mip_count_for(extent);
        let mut targets = Vec::with_capacity(2);
        for _ in 0..2 {
            targets.push(HzbTarget::new(device, descriptors, hzb, extent, mip_count)?);
        }
        let targets = match <[HzbTarget; 2]>::try_from(targets) {
            Ok(targets) => targets,
            Err(_) => unreachable!("exactly two pyramids are built"),
        };
        Ok(Self {
            resources: Arc::clone(device.resources()),
            targets,
            current: 0,
            extent,
            mip_count,
        })
    }

    /// The pyramid extent (mip 0).
    pub fn extent(&self) -> vk::Extent2D {
        self.extent
    }

    /// Mip levels per pyramid.
    pub fn mip_count(&self) -> u32 {
        self.mip_count
    }

    /// Swaps current and previous; call once per rendered frame before the build.
    pub fn begin_frame(&mut self) {
        self.current ^= 1;
        self.targets[self.current].valid = false;
    }

    /// Returns both pyramids' build sets to the pool before replacement; the caller
    /// holds the resize idle wait.
    pub fn free_sets(&mut self, descriptors: &Descriptors) {
        for target in &mut self.targets {
            descriptors.free_sets(&std::mem::take(&mut target.sets));
        }
    }

    /// Invalidates both pyramids (camera cut, resize, origin shift, scene rebuild).
    pub fn invalidate(&mut self) {
        self.targets[0].valid = false;
        self.targets[1].valid = false;
    }

    /// Whether last frame's completed pyramid is valid for occlusion tests.
    pub fn previous_valid(&self) -> bool {
        self.targets[self.current ^ 1].valid
    }

    /// Last frame's completed pyramid image + full view (GENERAL layout).
    pub fn previous(&self) -> (vk::Image, vk::ImageView) {
        let target = &self.targets[self.current ^ 1];
        (target.image.handle(), target.image.view())
    }

    /// This frame's pyramid image + full view (GENERAL layout after the build).
    pub fn current(&self) -> (vk::Image, vk::ImageView) {
        let target = &self.targets[self.current];
        (target.image.handle(), target.image.view())
    }

    /// Records the copy + per-mip reduce passes for this frame's pyramid into `graph`,
    /// reading `scene_depth` (the 1x depth after the scene pass). Marks the pyramid
    /// valid for next frame's tests and returns the current pyramid's graph resource
    /// (the retest pass declares its read on it).
    pub fn add_build_passes(
        &mut self,
        device: &Device,
        graph: &mut RenderGraph,
        pipelines: (&Arc<crate::Pipeline>, &Arc<crate::Pipeline>),
        scene_depth: RgResource,
    ) -> RgResource {
        let target = &mut self.targets[self.current];
        let hzb_res = graph.import_image(
            target.image.handle(),
            target.image.view(),
            vk::ImageAspectFlags::COLOR,
            target.layout,
            None,
        );
        target.layout = vk::ImageLayout::GENERAL;
        self.add_build_chain(device, graph, pipelines, scene_depth, hzb_res);
        hzb_res
    }

    /// Re-records the copy + reduce chain over the already-imported current pyramid
    /// resource — the final rebuild after the survivor raster updates the depth, so
    /// next frame's previous pyramid holds the complete cut.
    pub fn add_rebuild_passes(
        &mut self,
        device: &Device,
        graph: &mut RenderGraph,
        pipelines: (&Arc<crate::Pipeline>, &Arc<crate::Pipeline>),
        scene_depth: RgResource,
        hzb_res: RgResource,
    ) {
        self.add_build_chain(device, graph, pipelines, scene_depth, hzb_res);
    }

    fn add_build_chain(
        &mut self,
        device: &Device,
        graph: &mut RenderGraph,
        pipelines: (&Arc<crate::Pipeline>, &Arc<crate::Pipeline>),
        scene_depth: RgResource,
        hzb_res: RgResource,
    ) {
        let (copy_pipeline, reduce_pipeline) = pipelines;
        let target = &mut self.targets[self.current];
        let raw = device.raw().clone();
        let extent = self.extent;
        let copy = Arc::clone(copy_pipeline);
        let copy_set = target.sets[0];
        graph.add_pass(
            RgPass::compute("hzb-copy")
                .access(scene_depth, RgUsage::SampledReadCompute)
                .access(hzb_res, RgUsage::StorageImageRwCompute)
                .body({
                    let raw = raw.clone();
                    move |cmd, _scopes: &mut NestedScopeRecorder| {
                        dispatch(
                            &raw,
                            cmd,
                            &copy,
                            copy_set,
                            HzbPush {
                                src_extent: [extent.width, extent.height],
                                dst_extent: [extent.width, extent.height],
                            },
                        );
                    }
                }),
        );
        for mip in 1..self.mip_count {
            let reduce = Arc::clone(reduce_pipeline);
            let reduce_set = target.sets[mip as usize];
            let src = mip_extent(extent, mip - 1);
            let dst = mip_extent(extent, mip);
            graph.add_pass(
                RgPass::compute("hzb-reduce")
                    .access(hzb_res, RgUsage::StorageImageRwCompute)
                    .body({
                        let raw = raw.clone();
                        move |cmd, _scopes: &mut NestedScopeRecorder| {
                            dispatch(
                                &raw,
                                cmd,
                                &reduce,
                                reduce_set,
                                HzbPush {
                                    src_extent: [src.width, src.height],
                                    dst_extent: [dst.width, dst.height],
                                },
                            );
                        }
                    }),
            );
        }
        target.valid = true;
    }

    /// Last frame's completed pyramid's tracked image layout (for graph imports).
    pub fn previous_layout(&self) -> vk::ImageLayout {
        self.targets[self.current ^ 1].layout
    }

    /// Writes the per-frame depth binding (the 1x depth image view) into this frame's
    /// copy set. Call before [`Self::add_build_passes`] whenever the depth view exists.
    pub fn write_depth_binding(&mut self, device: &Device, hzb: &Hzb, depth_view: vk::ImageView) {
        let raw = device.raw();
        let target = &mut self.targets[self.current];
        let info = [vk::DescriptorImageInfo::default()
            .image_view(depth_view)
            .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
            .sampler(hzb.sampler)];
        let write = vk::WriteDescriptorSet::default()
            .dst_set(target.sets[0])
            .dst_binding(0)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .image_info(&info);
        // SAFETY: the ash seam. Written at the fence-waited frame build point.
        unsafe { raw.update_descriptor_sets(&[write], &[]) };
    }
}

impl HzbTarget {
    fn new(
        device: &Device,
        descriptors: &Descriptors,
        hzb: &Hzb,
        extent: vk::Extent2D,
        mip_count: u32,
    ) -> Result<Self> {
        let image = Image::new(
            device.resources(),
            &ImageDesc {
                extent,
                format: vk::Format::R32_SFLOAT,
                usage: vk::ImageUsageFlags::STORAGE
                    | vk::ImageUsageFlags::SAMPLED
                    | vk::ImageUsageFlags::TRANSFER_SRC,
                aspect: vk::ImageAspectFlags::COLOR,
                view_type: vk::ImageViewType::TYPE_2D,
                mip_levels: mip_count,
                array_layers: 1,
                samples: vk::SampleCountFlags::TYPE_1,
            },
        )?;
        let raw = device.raw();
        let mut mip_views = Vec::with_capacity(mip_count as usize);
        for mip in 0..mip_count {
            let view_info = vk::ImageViewCreateInfo::default()
                .image(image.handle())
                .view_type(vk::ImageViewType::TYPE_2D)
                .format(vk::Format::R32_SFLOAT)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: mip,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: 1,
                });
            // SAFETY: the ash seam. Freed in `Drop` after `wait_gpu_idle`.
            let view = match checked(
                unsafe { raw.create_image_view(&view_info, None) },
                "hzb mip view",
            ) {
                Ok(view) => view,
                Err(err) => {
                    // SAFETY: the ash seam. Free the views created so far.
                    unsafe {
                        for view in mip_views.drain(..) {
                            raw.destroy_image_view(view, None);
                        }
                    }
                    return Err(err);
                }
            };
            mip_views.push(view);
        }

        let mut sets = Vec::with_capacity(mip_count as usize);
        sets.push(descriptors.allocate_set(hzb.copy_layout)?);
        for _ in 1..mip_count {
            sets.push(descriptors.allocate_set(hzb.reduce_layout)?);
        }
        // Stable storage bindings: the copy writes mip 0; reduce N reads mip N-1 and
        // writes mip N. The copy's depth sampler binding is rewritten per frame.
        let mut writes = Vec::new();
        let mut infos = Vec::new();
        for view in &mip_views {
            infos.push([vk::DescriptorImageInfo::default()
                .image_view(*view)
                .image_layout(vk::ImageLayout::GENERAL)]);
        }
        for (mip, set) in sets.iter().enumerate() {
            if mip == 0 {
                writes.push(
                    vk::WriteDescriptorSet::default()
                        .dst_set(*set)
                        .dst_binding(1)
                        .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                        .image_info(&infos[0]),
                );
            } else {
                writes.push(
                    vk::WriteDescriptorSet::default()
                        .dst_set(*set)
                        .dst_binding(0)
                        .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                        .image_info(&infos[mip - 1]),
                );
                writes.push(
                    vk::WriteDescriptorSet::default()
                        .dst_set(*set)
                        .dst_binding(1)
                        .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                        .image_info(&infos[mip]),
                );
            }
        }
        // SAFETY: the ash seam. The sets and views outlive the call.
        unsafe { raw.update_descriptor_sets(&writes, &[]) };

        Ok(Self {
            image,
            mip_views,
            sets,
            layout: vk::ImageLayout::UNDEFINED,
            valid: false,
        })
    }
}

impl Drop for HzbPyramid {
    fn drop(&mut self) {
        let raw = self.resources.device();
        for target in &mut self.targets {
            // SAFETY: the ash seam. Teardown after `wait_gpu_idle`; the pool frees sets.
            unsafe {
                for view in target.mip_views.drain(..) {
                    raw.destroy_image_view(view, None);
                }
            }
        }
    }
}

fn dispatch(
    raw: &ash::Device,
    cmd: vk::CommandBuffer,
    pipeline: &crate::Pipeline,
    set: vk::DescriptorSet,
    push: HzbPush,
) {
    let groups = |n: u32| n.div_ceil(8);
    // SAFETY: the ash seam. The PSO/set are valid this frame; the push spans the
    // declared range; the dispatch covers the destination extent.
    unsafe {
        raw.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, pipeline.handle());
        raw.cmd_bind_descriptor_sets(
            cmd,
            vk::PipelineBindPoint::COMPUTE,
            pipeline.layout(),
            0,
            &[set],
            &[],
        );
        raw.cmd_push_constants(
            cmd,
            pipeline.layout(),
            vk::ShaderStageFlags::COMPUTE,
            0,
            bytemuck::bytes_of(&push),
        );
        raw.cmd_dispatch(
            cmd,
            groups(push.dst_extent[0]),
            groups(push.dst_extent[1]),
            1,
        );
    }
}

fn make_layout(raw: &ash::Device, types: &[vk::DescriptorType]) -> Result<vk::DescriptorSetLayout> {
    let bindings: Vec<vk::DescriptorSetLayoutBinding> = types
        .iter()
        .enumerate()
        .map(|(index, &ty)| {
            vk::DescriptorSetLayoutBinding::default()
                .binding(index as u32)
                .descriptor_type(ty)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::COMPUTE)
        })
        .collect();
    let info = vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings);
    // SAFETY: the ash seam. The bindings outlive the call; freed in `Drop`.
    checked(
        unsafe { raw.create_descriptor_set_layout(&info, None) },
        "hzb layout",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resources::{BindlessFreeList, Buffer};
    use crate::{Device, Pipelines, SurfaceSource, validation_issue_count};
    use std::sync::Mutex;

    fn one_shot<F: FnOnce(vk::CommandBuffer)>(device: &Device, record: F) {
        let raw = device.raw();
        let pool_info =
            vk::CommandPoolCreateInfo::default().queue_family_index(device.graphics_queue_family);
        // SAFETY: the ash seam. Everything is destroyed after the fence wait.
        unsafe {
            let pool = raw.create_command_pool(&pool_info, None).expect("pool");
            let alloc = vk::CommandBufferAllocateInfo::default()
                .command_pool(pool)
                .level(vk::CommandBufferLevel::PRIMARY)
                .command_buffer_count(1);
            let cmd = raw.allocate_command_buffers(&alloc).expect("cmd")[0];
            let fence = raw
                .create_fence(&vk::FenceCreateInfo::default(), None)
                .expect("fence");
            raw.begin_command_buffer(
                cmd,
                &vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )
            .expect("begin");
            record(cmd);
            raw.end_command_buffer(cmd).expect("end");
            let cmd_info = [vk::CommandBufferSubmitInfo::default().command_buffer(cmd)];
            let submit = [vk::SubmitInfo2::default().command_buffer_infos(&cmd_info)];
            device
                .graphics_queue
                .submit2(raw, &submit, fence, "hzb test")
                .expect("submit");
            raw.wait_for_fences(&[fence], true, u64::MAX).expect("wait");
            raw.destroy_fence(fence, None);
            raw.destroy_command_pool(pool, None);
        }
    }

    fn image_barrier(
        raw: &ash::Device,
        cmd: vk::CommandBuffer,
        image: vk::Image,
        aspect: vk::ImageAspectFlags,
        old_layout: vk::ImageLayout,
        new_layout: vk::ImageLayout,
    ) {
        let barrier = vk::ImageMemoryBarrier2::default()
            .src_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
            .src_access_mask(vk::AccessFlags2::MEMORY_WRITE | vk::AccessFlags2::MEMORY_READ)
            .dst_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
            .dst_access_mask(vk::AccessFlags2::MEMORY_WRITE | vk::AccessFlags2::MEMORY_READ)
            .old_layout(old_layout)
            .new_layout(new_layout)
            .image(image)
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: aspect,
                base_mip_level: 0,
                level_count: vk::REMAINING_MIP_LEVELS,
                base_array_layer: 0,
                layer_count: 1,
            });
        let barriers = [barrier];
        // SAFETY: the ash seam. Recorded into the live one-shot command buffer.
        unsafe {
            raw.cmd_pipeline_barrier2(
                cmd,
                &vk::DependencyInfo::default().image_memory_barriers(&barriers),
            );
        }
    }

    fn run_graph(device: &Device, graph: &mut RenderGraph) {
        one_shot(device, |cmd| graph.execute(device, cmd));
    }

    fn read_mip(device: &Device, image: vk::Image, mip: u32, extent: vk::Extent2D) -> Vec<f32> {
        let texels = (extent.width * extent.height) as usize;
        let staging = Buffer::new(
            device.resources(),
            (texels * 4) as u64,
            vk::BufferUsageFlags::TRANSFER_DST,
            &vk_mem::AllocationCreateInfo {
                usage: vk_mem::MemoryUsage::Auto,
                flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_RANDOM
                    | vk_mem::AllocationCreateFlags::MAPPED,
                ..Default::default()
            },
        )
        .expect("staging");
        let raw = device.raw().clone();
        one_shot(device, |cmd| {
            image_barrier(
                &raw,
                cmd,
                image,
                vk::ImageAspectFlags::COLOR,
                vk::ImageLayout::GENERAL,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            );
            let region = vk::BufferImageCopy {
                buffer_offset: 0,
                buffer_row_length: 0,
                buffer_image_height: 0,
                image_subresource: vk::ImageSubresourceLayers {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    mip_level: mip,
                    base_array_layer: 0,
                    layer_count: 1,
                },
                image_offset: vk::Offset3D::default(),
                image_extent: vk::Extent3D {
                    width: extent.width,
                    height: extent.height,
                    depth: 1,
                },
            };
            // SAFETY: the ash seam. The image is TRANSFER_SRC for the copy.
            unsafe {
                raw.cmd_copy_image_to_buffer(
                    cmd,
                    image,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    staging.handle(),
                    &[region],
                );
            }
            image_barrier(
                &raw,
                cmd,
                image,
                vk::ImageAspectFlags::COLOR,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                vk::ImageLayout::GENERAL,
            );
        });
        let mut out = vec![0_u8; texels * 4];
        // SAFETY: HOST_VISIBLE + MAPPED; the copy completed under the fence.
        unsafe {
            std::ptr::copy_nonoverlapping(staging.mapped_ptr(), out.as_mut_ptr(), out.len());
        }
        out.chunks_exact(4)
            .map(|chunk| f32::from_le_bytes(chunk.try_into().unwrap()))
            .collect()
    }

    #[test]
    fn pyramid_reduces_a_conservative_max_with_odd_edges() {
        let device = match Device::new(&SurfaceSource::Offscreen) {
            Ok(device) => device,
            Err(err) => {
                eprintln!("skipping (no Vulkan device): {err}");
                return;
            }
        };
        let before = validation_issue_count();
        {
            let free_list: BindlessFreeList = Arc::new(Mutex::new(Vec::new()));
            let descriptors = Descriptors::new(&device, &free_list).expect("descriptors");
            let hzb = Hzb::new(&device).expect("hzb");
            let mut pipelines = Pipelines::new(&device, &descriptors, vk::SampleCountFlags::TYPE_1);
            let copy = pipelines.request_hzb_copy(hzb.copy_layout()).expect("copy");
            let reduce = pipelines
                .request_hzb_reduce(hzb.reduce_layout())
                .expect("reduce");

            let extent = vk::Extent2D {
                width: 5,
                height: 3,
            };
            let mut pyramid =
                HzbPyramid::new(&device, &descriptors, &hzb, extent).expect("pyramid");
            assert_eq!(pyramid.mip_count(), 3);

            // Depth source with known texels (row-major, 0.01 .. 0.15).
            let depth = Image::new(
                device.resources(),
                &ImageDesc {
                    extent,
                    format: vk::Format::D32_SFLOAT,
                    usage: vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::SAMPLED,
                    aspect: vk::ImageAspectFlags::DEPTH,
                    view_type: vk::ImageViewType::TYPE_2D,
                    mip_levels: 1,
                    array_layers: 1,
                    samples: vk::SampleCountFlags::TYPE_1,
                },
            )
            .expect("depth");
            let texels: Vec<f32> = (0..15).map(|i| (i as f32 + 1.0) / 100.0).collect();
            let upload = Buffer::new(
                device.resources(),
                (texels.len() * 4) as u64,
                vk::BufferUsageFlags::TRANSFER_SRC,
                &vk_mem::AllocationCreateInfo {
                    usage: vk_mem::MemoryUsage::Auto,
                    flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_RANDOM
                        | vk_mem::AllocationCreateFlags::MAPPED,
                    ..Default::default()
                },
            )
            .expect("upload");
            // SAFETY: HOST_VISIBLE + MAPPED, written before the submit below.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    texels.as_ptr().cast::<u8>(),
                    upload.mapped_ptr(),
                    texels.len() * 4,
                );
            }
            let raw = device.raw().clone();
            one_shot(&device, |cmd| {
                image_barrier(
                    &raw,
                    cmd,
                    depth.handle(),
                    vk::ImageAspectFlags::DEPTH,
                    vk::ImageLayout::UNDEFINED,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                );
                let region = vk::BufferImageCopy {
                    buffer_offset: 0,
                    buffer_row_length: 0,
                    buffer_image_height: 0,
                    image_subresource: vk::ImageSubresourceLayers {
                        aspect_mask: vk::ImageAspectFlags::DEPTH,
                        mip_level: 0,
                        base_array_layer: 0,
                        layer_count: 1,
                    },
                    image_offset: vk::Offset3D::default(),
                    image_extent: vk::Extent3D {
                        width: extent.width,
                        height: extent.height,
                        depth: 1,
                    },
                };
                // SAFETY: the ash seam. The depth image is TRANSFER_DST for the copy.
                unsafe {
                    raw.cmd_copy_buffer_to_image(
                        cmd,
                        upload.handle(),
                        depth.handle(),
                        vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                        &[region],
                    );
                }
                image_barrier(
                    &raw,
                    cmd,
                    depth.handle(),
                    vk::ImageAspectFlags::DEPTH,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                );
            });

            pyramid.begin_frame();
            assert!(!pyramid.previous_valid(), "no completed pyramid yet");
            pyramid.write_depth_binding(&device, &hzb, depth.view());
            let mut graph = RenderGraph::new();
            let depth_res = graph.import_image(
                depth.handle(),
                depth.view(),
                vk::ImageAspectFlags::DEPTH,
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                None,
            );
            pyramid.add_build_passes(&device, &mut graph, (&copy, &reduce), depth_res);
            run_graph(&device, &mut graph);

            let (built, _) = pyramid.current();
            let mip0 = read_mip(&device, built, 0, extent);
            assert_eq!(mip0, texels, "mip 0 copies the depth exactly");
            // Destination 2x1: texel 0 covers x0..1 (y folds 0..2), texel 1 absorbs the
            // odd trailing column (x2..4). Row-major texels: max at the bottom rows.
            let mip1 = read_mip(
                &device,
                built,
                1,
                vk::Extent2D {
                    width: 2,
                    height: 1,
                },
            );
            assert_eq!(mip1, [0.12, 0.15], "conservative max with odd-edge folding");
            let mip2 = read_mip(
                &device,
                built,
                2,
                vk::Extent2D {
                    width: 1,
                    height: 1,
                },
            );
            assert_eq!(mip2, [0.15], "the apex holds the farthest depth");

            // The next frame swap makes the built pyramid the valid previous.
            pyramid.begin_frame();
            assert!(pyramid.previous_valid());
            pyramid.invalidate();
            assert!(!pyramid.previous_valid());

            device.wait_idle().expect("idle");
            drop(pyramid);
            drop(depth);
            drop(upload);
            drop(copy);
            drop(reduce);
            drop(pipelines);
            drop(hzb);
            drop(descriptors);
        }
        device.wait_idle().expect("idle before teardown");
        drop(device);
        assert_eq!(validation_issue_count(), before);
    }
}
