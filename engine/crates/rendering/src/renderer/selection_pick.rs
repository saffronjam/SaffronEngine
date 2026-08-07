use super::*;
use crate::selection::SelectionReadback;

/// Everything one view's last frame left behind that the selection pass needs to replay its
/// binned cut: the draw buckets, the indirect stream they read, and the camera they were binned
/// for. Captured per view so a thumbnail excursion cannot answer a scene-viewport pick.
#[derive(Clone)]
pub(super) struct SelectionSource {
    pub(super) draws: Vec<(crate::ExecutorBucket, bool, Arc<crate::Pipeline>)>,
    pub(super) inputs: crate::ExecutorDrawInputs,
    pub(super) instance_set: vk::DescriptorSet,
    pub(super) pages: vk::Buffer,
    pub(super) view_proj: Mat4,
    pub(super) extent: vk::Extent2D,
}

/// The one-texel pick targets: the identity attachment, the two surface attachments, a private
/// depth, and the host-visible buffer the readback lands in.
pub(super) struct SelectionTargets {
    id: crate::Image,
    position: crate::Image,
    normal: crate::Image,
    depth: crate::Image,
    staging: crate::Buffer,
}

impl SelectionTargets {
    fn new(device: &crate::Device) -> Result<Self> {
        let extent = vk::Extent2D {
            width: 1,
            height: 1,
        };
        let color_usage = vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::TRANSFER_SRC;
        let resources = device.resources();
        let color = |format| {
            crate::Image::new(
                resources,
                &crate::ImageDesc::color_2d(extent, format, color_usage),
            )
        };
        let id = color(crate::SELECTION_ID_FORMAT)?;
        let position = color(crate::SELECTION_SURFACE_FORMAT)?;
        let normal = color(crate::SELECTION_SURFACE_FORMAT)?;
        let depth = crate::Image::new(
            resources,
            &crate::ImageDesc {
                aspect: vk::ImageAspectFlags::DEPTH,
                ..crate::ImageDesc::color_2d(
                    extent,
                    crate::DEPTH_FORMAT,
                    vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT,
                )
            },
        )?;
        let staging = crate::Buffer::new(
            resources,
            size_of::<SelectionReadback>() as u64,
            vk::BufferUsageFlags::TRANSFER_DST,
            &vk_mem::AllocationCreateInfo {
                usage: vk_mem::MemoryUsage::AutoPreferHost,
                flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_RANDOM
                    | vk_mem::AllocationCreateFlags::MAPPED,
                ..Default::default()
            },
        )?;
        Ok(Self {
            id,
            position,
            normal,
            depth,
            staging,
        })
    }
}

/// Emits one dependency for the pick's image transitions.
fn barrier(raw: &ash::Device, cmd: vk::CommandBuffer, images: &[vk::ImageMemoryBarrier2<'static>]) {
    let dependency = vk::DependencyInfo::default().image_memory_barriers(images);
    // SAFETY: the ash seam. The barriers name images owned by the pick targets, which outlive
    // the one-shot submit this records into.
    unsafe { raw.cmd_pipeline_barrier2(cmd, &dependency) };
}

/// A color barrier for one pick attachment, moving it into the layout `dst` needs.
fn color_barrier(
    image: vk::Image,
    old: vk::ImageLayout,
    new: vk::ImageLayout,
    src: (vk::PipelineStageFlags2, vk::AccessFlags2),
    dst: (vk::PipelineStageFlags2, vk::AccessFlags2),
) -> vk::ImageMemoryBarrier2<'static> {
    vk::ImageMemoryBarrier2::default()
        .src_stage_mask(src.0)
        .src_access_mask(src.1)
        .dst_stage_mask(dst.0)
        .dst_access_mask(dst.1)
        .old_layout(old)
        .new_layout(new)
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

fn color_attachment(
    view: vk::ImageView,
    clear: vk::ClearValue,
) -> vk::RenderingAttachmentInfo<'static> {
    vk::RenderingAttachmentInfo::default()
        .image_view(view)
        .image_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
        .load_op(vk::AttachmentLoadOp::CLEAR)
        .store_op(vk::AttachmentStoreOp::STORE)
        .clear_value(clear)
}

impl Renderer {
    /// Records what the active view's frame left behind, so a later pick can replay its binned
    /// cut without re-running the visibility chain.
    pub(super) fn capture_selection_source(
        &mut self,
        draws: &[(crate::ExecutorBucket, bool, Arc<crate::Pipeline>)],
        inputs: Option<crate::ExecutorDrawInputs>,
        instance_set: vk::DescriptorSet,
        extent: vk::Extent2D,
    ) {
        let slot = self.active_view.index();
        let Some(inputs) = inputs.filter(|_| !draws.is_empty()) else {
            self.selection_sources[slot] = None;
            return;
        };
        self.selection_sources[slot] = Some(SelectionSource {
            draws: draws.to_vec(),
            inputs,
            instance_set,
            pages: self.global_gpu_data.pages.buffer(),
            view_proj: self.frame_deformation.view_proj,
            extent,
        });
    }

    /// Resolves one viewport pixel to the draw record drawn there, by replaying the active view's
    /// binned cut through the selection PSO into one-texel targets and reading them back.
    ///
    /// `u`/`v` are viewport-relative in `[0, 1]`, `v = 0` at the top edge. The replay uses a
    /// viewport whose origin is shifted by the picked pixel, so the one rasterized texel carries
    /// the full-resolution derivatives every coverage test needs.
    ///
    /// Out-of-band and never on the present hot path: the cut buffers belong to a frame slot the
    /// GPU may still be reading, so this idles the device before it submits.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Vk`] if the device idle, the pick allocations, or the one-shot submit fail.
    pub fn pick_selection_id(&mut self, u: f32, v: f32) -> Result<Option<crate::SelectionHit>> {
        let Some(source) = self.selection_sources[self.active_view.index()].clone() else {
            return Ok(None);
        };
        if source.extent.width == 0 || source.extent.height == 0 {
            return Ok(None);
        }
        let Some(pipeline) = self.pipelines.request_selection_id() else {
            return Ok(None);
        };
        let pixel_x = (u * source.extent.width as f32).floor() as i32;
        let pixel_y = (v * source.extent.height as f32).floor() as i32;
        if pixel_x < 0
            || pixel_y < 0
            || pixel_x >= source.extent.width as i32
            || pixel_y >= source.extent.height as i32
        {
            return Ok(None);
        }

        self.device.wait_idle()?;
        let targets = match self.selection_targets.take() {
            Some(targets) => targets,
            None => SelectionTargets::new(&self.device)?,
        };
        let bindless_set = self.descriptors.bindless_set();
        let draw_count_supported = self.device.capabilities.draw_indirect_count;
        let result = self.record_selection_pick(
            &targets,
            &source,
            &pipeline,
            bindless_set,
            draw_count_supported,
            (pixel_x, pixel_y),
        );
        self.selection_targets = Some(targets);
        result
    }

    fn record_selection_pick(
        &self,
        targets: &SelectionTargets,
        source: &SelectionSource,
        pipeline: &crate::Pipeline,
        bindless_set: vk::DescriptorSet,
        draw_count_supported: bool,
        pixel: (i32, i32),
    ) -> Result<Option<crate::SelectionHit>> {
        let one = vk::Extent2D {
            width: 1,
            height: 1,
        };
        let clear_id = vk::ClearValue {
            color: vk::ClearColorValue { uint32: [0; 4] },
        };
        let clear_surface = vk::ClearValue {
            color: vk::ClearColorValue { float32: [0.0; 4] },
        };
        self.device.one_shot_transfer(|raw, cmd| {
            let to_attachment = [
                color_barrier(
                    targets.id.handle(),
                    vk::ImageLayout::UNDEFINED,
                    vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                    (
                        vk::PipelineStageFlags2::TOP_OF_PIPE,
                        vk::AccessFlags2::empty(),
                    ),
                    (
                        vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT,
                        vk::AccessFlags2::COLOR_ATTACHMENT_WRITE,
                    ),
                ),
                color_barrier(
                    targets.position.handle(),
                    vk::ImageLayout::UNDEFINED,
                    vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                    (
                        vk::PipelineStageFlags2::TOP_OF_PIPE,
                        vk::AccessFlags2::empty(),
                    ),
                    (
                        vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT,
                        vk::AccessFlags2::COLOR_ATTACHMENT_WRITE,
                    ),
                ),
                color_barrier(
                    targets.normal.handle(),
                    vk::ImageLayout::UNDEFINED,
                    vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                    (
                        vk::PipelineStageFlags2::TOP_OF_PIPE,
                        vk::AccessFlags2::empty(),
                    ),
                    (
                        vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT,
                        vk::AccessFlags2::COLOR_ATTACHMENT_WRITE,
                    ),
                ),
                vk::ImageMemoryBarrier2::default()
                    .src_stage_mask(vk::PipelineStageFlags2::TOP_OF_PIPE)
                    .dst_stage_mask(vk::PipelineStageFlags2::EARLY_FRAGMENT_TESTS)
                    .dst_access_mask(vk::AccessFlags2::DEPTH_STENCIL_ATTACHMENT_WRITE)
                    .old_layout(vk::ImageLayout::UNDEFINED)
                    .new_layout(vk::ImageLayout::DEPTH_ATTACHMENT_OPTIMAL)
                    .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .image(targets.depth.handle())
                    .subresource_range(vk::ImageSubresourceRange {
                        aspect_mask: vk::ImageAspectFlags::DEPTH,
                        base_mip_level: 0,
                        level_count: 1,
                        base_array_layer: 0,
                        layer_count: 1,
                    }),
            ];
            barrier(raw, cmd, &to_attachment);

            let color_infos = [
                color_attachment(targets.id.view(), clear_id),
                color_attachment(targets.position.view(), clear_surface),
                color_attachment(targets.normal.view(), clear_surface),
            ];
            let depth_info = vk::RenderingAttachmentInfo::default()
                .image_view(targets.depth.view())
                .image_layout(vk::ImageLayout::DEPTH_ATTACHMENT_OPTIMAL)
                .load_op(vk::AttachmentLoadOp::CLEAR)
                .store_op(vk::AttachmentStoreOp::DONT_CARE)
                .clear_value(vk::ClearValue {
                    depth_stencil: vk::ClearDepthStencilValue {
                        depth: 1.0,
                        stencil: 0,
                    },
                });
            let rendering = vk::RenderingInfo::default()
                .render_area(vk::Rect2D {
                    offset: vk::Offset2D { x: 0, y: 0 },
                    extent: one,
                })
                .layer_count(1)
                .color_attachments(&color_infos)
                .depth_attachment(&depth_info);
            // The pick viewport is the full render extent translated so the picked pixel lands on
            // the single texel: the scale is unchanged, so screen-space derivatives — and the
            // coverage hash they feed — match the frame the user clicked on.
            let viewport = vk::Viewport {
                x: -(pixel.0 as f32),
                y: -(pixel.1 as f32),
                width: source.extent.width as f32,
                height: source.extent.height as f32,
                min_depth: 0.0,
                max_depth: 1.0,
            };
            let scissor = vk::Rect2D {
                offset: vk::Offset2D { x: 0, y: 0 },
                extent: one,
            };
            // SAFETY: the ash seam. The attachment views outlive this one-shot submit, which is
            // fenced before the readback below.
            unsafe {
                raw.cmd_begin_rendering(cmd, &rendering);
                raw.cmd_set_viewport(cmd, 0, &[viewport]);
                raw.cmd_set_scissor(cmd, 0, &[scissor]);
                raw.cmd_set_cull_mode(cmd, vk::CullModeFlags::NONE);
            }
            for transparent in [false, true] {
                crate::record_executor_depth_family(
                    raw,
                    cmd,
                    (pipeline.handle(), pipeline.layout()),
                    vk::ShaderStageFlags::VERTEX,
                    bytemuck::bytes_of(&source.view_proj),
                    bindless_set,
                    source.instance_set,
                    source.inputs,
                    source.pages,
                    draw_count_supported,
                    &source.draws,
                    transparent,
                );
            }
            // SAFETY: the ash seam. Closes the rendering scope opened above.
            unsafe { raw.cmd_end_rendering(cmd) };

            let to_transfer: Vec<_> = [
                targets.id.handle(),
                targets.position.handle(),
                targets.normal.handle(),
            ]
            .into_iter()
            .map(|image| {
                color_barrier(
                    image,
                    vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    (
                        vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT,
                        vk::AccessFlags2::COLOR_ATTACHMENT_WRITE,
                    ),
                    (
                        vk::PipelineStageFlags2::COPY,
                        vk::AccessFlags2::TRANSFER_READ,
                    ),
                )
            })
            .collect();
            barrier(raw, cmd, &to_transfer);

            let copy = |offset: u64| {
                vk::BufferImageCopy::default()
                    .buffer_offset(offset)
                    .image_subresource(vk::ImageSubresourceLayers {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        mip_level: 0,
                        base_array_layer: 0,
                        layer_count: 1,
                    })
                    .image_extent(vk::Extent3D {
                        width: 1,
                        height: 1,
                        depth: 1,
                    })
            };
            let destination = targets.staging.handle();
            // SAFETY: the ash seam. Each copy writes one 16-byte texel into its own slot of the
            // readback struct, which the staging buffer is sized for.
            unsafe {
                raw.cmd_copy_image_to_buffer(
                    cmd,
                    targets.id.handle(),
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    destination,
                    &[copy(0)],
                );
                raw.cmd_copy_image_to_buffer(
                    cmd,
                    targets.position.handle(),
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    destination,
                    &[copy(16)],
                );
                raw.cmd_copy_image_to_buffer(
                    cmd,
                    targets.normal.handle(),
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    destination,
                    &[copy(32)],
                );
            }
        })?;
        // SAFETY: HOST_VISIBLE + MAPPED, sized for exactly this struct, and the one-shot's fence
        // was waited before this returns.
        let readback =
            unsafe { std::ptr::read(targets.staging.mapped_ptr().cast::<SelectionReadback>()) };
        Ok(readback.decode())
    }
}
