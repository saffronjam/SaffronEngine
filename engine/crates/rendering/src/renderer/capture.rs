use super::*;

impl Renderer {
    /// Captures the active view's offscreen scene color to a PNG file.
    /// path).
    /// An out-of-band path, never on the present hot path: the offscreen may still be sampled by
    /// an in-flight frame, so it idles the device first and leaves the image in
    /// `ShaderReadOnlyOptimal` so the next frame's producer barrier holds. The offscreen is
    /// already display-range, so its `RGBA16F` halves are clamped, not tonemapped.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Vk`] if the device cannot idle / a Vulkan call fails, or an
    /// [`Error::ShaderLoad`]-shaped wrapper carrying the PNG write failure.
    pub fn capture_viewport(&mut self, path: &std::path::Path) -> Result<()> {
        let (extent, format, pixels) = self.read_active_offscreen()?;
        crate::write_png_file(&pixels, extent.width, extent.height, format, path).map_err(
            |err| Error::ShaderLoad(format!("capture: write {}: {err}", path.display())),
        )?;
        Ok(())
    }

    /// Reads the active view's post-processed offscreen back and encodes it to PNG bytes in
    /// memory — the twin of [`Renderer::capture_viewport`], for a background thumbnail render
    /// whose result ships over the control protocol. The caller selects the active view first;
    /// the offscreen is already display-range, so its `RGBA16F` halves are clamped.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Vk`] if the read-back's device idle / Vulkan calls fail, or an
    /// [`Error::ShaderLoad`]-shaped wrapper carrying a PNG encode failure.
    pub fn encode_active_offscreen_png(&mut self) -> Result<crate::ThumbnailPng> {
        let (extent, format, pixels) = self.read_active_offscreen()?;
        let bytes = crate::encode_to_png(
            &pixels,
            extent.width,
            extent.height,
            format,
            crate::PngTransfer::Clamp,
        )
        .map_err(|err| Error::ShaderLoad(format!("thumbnail: encode png: {err}")))?;
        Ok(crate::ThumbnailPng {
            bytes,
            width: extent.width,
            height: extent.height,
        })
    }

    /// Sets whether a view's shm publish is enabled (the host's segment wiring). The active
    /// view's flag gates whether [`Renderer::render_scene_offscreen`] folds the BGRA8 readback
    /// into the frame command buffer.
    pub fn set_shm_publish_enabled(&mut self, view: ViewId, enabled: bool) {
        let i = view.index();
        if self.shm_publish_enabled[i] == enabled {
            return;
        }
        self.shm_publish_enabled[i] = enabled;
        if !enabled {
            return;
        }
        // Arming is the other seam that may size the capture ring, so it idles first for the
        // same reason the resize does.
        let extent = self.views[i].published_extent();
        if let Err(err) = self
            .device
            .wait_idle()
            .and_then(|()| self.views[i].size_shm_capture(&self.device, extent))
        {
            tracing::error!("shm capture ring: {err}");
        }
    }

    /// Drains the pipelined BGRA8 bytes staged at the last begin-frame fence wait, if any —
    /// `(view, width, height, bgra8)`. The host publishes these into the view's shm segment;
    /// the bytes belong to a frame whose GPU work completed `MAX_FRAMES_IN_FLIGHT` frames ago,
    /// so the read is stall-free.
    pub fn pending_shm_view(&self) -> Option<(ViewId, u32, u32, &[u8])> {
        let (view_idx, slot) = self.pending_shm_publish?;
        let capture = self.views[view_idx].shm_capture.slots[slot].as_ref()?;
        let extent = capture.extent;
        let byte_size = extent.width as usize * extent.height as usize * 4;
        // SAFETY: the staging buffer is HOST_VISIBLE + MAPPED for `byte_size` bytes; this slot's
        // frame fence signalled at the begin-frame wait, so the GPU copy completed. The slice
        // lives until this slot is reused (`MAX_FRAMES_IN_FLIGHT` frames out) — past the publish.
        let pixels = unsafe { std::slice::from_raw_parts(capture.staging.mapped_ptr(), byte_size) };
        Some((
            ViewId::from_index(view_idx),
            extent.width,
            extent.height,
            pixels,
        ))
    }

    /// Stages the just-completed shm-capture slot's BGRA8 bytes for the host to publish, run at
    /// [`Renderer::begin_offscreen_frame`] right after this slot's in-flight fence wait — the
    /// slot's recorded readback is now host-visible, so the read never stalls. A no-op when the
    /// active view's shm publish is off or the slot has no completed readback yet.
    pub(super) fn stage_pending_shm_publish(&mut self, slot: usize) {
        self.pending_shm_publish = None;
        let active = self.active_view.index();
        if !self.shm_publish_enabled[active] {
            return;
        }
        let Some(capture) = self.views[active].shm_capture.slots[slot].as_ref() else {
            return;
        };
        if !capture.valid {
            return;
        }
        // Record the slot only; the host reads the mapped staging directly via
        // `pending_shm_view` and copies straight into the shm ring — one memcpy, no alloc.
        self.pending_shm_publish = Some((active, slot));
    }

    /// Records the active view's BGRA8 shm-publish readback into the frame command buffer `cmd`
    /// for frame slot `slot`: a 1:1 `vkCmdBlitImage` converts `RGBA16F`→BGRA8 into this slot's
    /// persistent image, a device-local transfer buffer receives the linear pixels, and a final
    /// buffer copy lands them in host-visible staging. Folded into the frame's single submit; the
    /// offscreen is left in `TransferSrcOptimal`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Vk`] if the blit format is unsupported or an allocation fails.
    pub(super) fn record_shm_copy(&mut self, cmd: vk::CommandBuffer, slot: usize) -> Result<()> {
        let active = self.active_view.index();
        let render_extent = self.views[active].offscreen.extent;
        let publish_extent = self.views[active].published_extent();
        if render_extent.width == 0 || render_extent.height == 0 {
            return Ok(());
        }
        let raw = self.device.raw();
        let view = &self.views[active];
        let from_layout = view.offscreen.layout;
        let src_image = view.offscreen.handle();
        // The ring is sized at the resize / publish-arm seams, both of which hold a device idle:
        // a slot must never be replaced here, mid-recording, while a readback into the old one
        // can still be in flight. An absent or stale slot means publish was armed after this
        // frame's seam ran; the next frame has it.
        let Some(capture) = view.shm_capture.slots[slot]
            .as_ref()
            .filter(|capture| capture.extent == publish_extent)
        else {
            return Ok(());
        };
        let dst_image = capture.image.handle();
        let readback = capture.readback.handle();
        let staging = capture.staging.handle();
        let byte_size = capture.staging.size();

        let color_range = vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            base_mip_level: 0,
            level_count: 1,
            base_array_layer: 0,
            layer_count: 1,
        };
        // The offscreen rests in COLOR_ATTACHMENT (after the post chain's overlay pass) or
        // SHADER_READ_ONLY (after a prior frame's readback); match the source scope to it.
        let (from_stage, from_access) = match from_layout {
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL => (
                vk::PipelineStageFlags2::FRAGMENT_SHADER,
                vk::AccessFlags2::SHADER_SAMPLED_READ,
            ),
            vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL => (
                vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT
                    | vk::PipelineStageFlags2::COMPUTE_SHADER,
                vk::AccessFlags2::COLOR_ATTACHMENT_WRITE | vk::AccessFlags2::SHADER_STORAGE_WRITE,
            ),
            _ => (vk::PipelineStageFlags2::TOP_OF_PIPE, vk::AccessFlags2::NONE),
        };

        let blit = vk::ImageBlit::default()
            .src_subresource(vk::ImageSubresourceLayers {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                mip_level: 0,
                base_array_layer: 0,
                layer_count: 1,
            })
            .src_offsets([
                vk::Offset3D { x: 0, y: 0, z: 0 },
                vk::Offset3D {
                    x: render_extent.width as i32,
                    y: render_extent.height as i32,
                    z: 1,
                },
            ])
            .dst_subresource(vk::ImageSubresourceLayers {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                mip_level: 0,
                base_array_layer: 0,
                layer_count: 1,
            })
            .dst_offsets([
                vk::Offset3D { x: 0, y: 0, z: 0 },
                vk::Offset3D {
                    x: publish_extent.width as i32,
                    y: publish_extent.height as i32,
                    z: 1,
                },
            ]);
        let copy = vk::BufferImageCopy::default()
            .image_subresource(vk::ImageSubresourceLayers {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                mip_level: 0,
                base_array_layer: 0,
                layer_count: 1,
            })
            .image_extent(vk::Extent3D {
                width: publish_extent.width,
                height: publish_extent.height,
                depth: 1,
            });

        // SAFETY: the ash seam. Barriers / blit / copy recorded into the frame command
        // buffer (already in its begin..end recording); the images + buffer outlive the
        // recorded commands, freed at teardown under `wait_idle`.
        unsafe {
            capture_barrier(
                raw,
                cmd,
                src_image,
                color_range,
                from_layout,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                from_stage,
                from_access,
                vk::PipelineStageFlags2::BLIT,
                vk::AccessFlags2::TRANSFER_READ,
            );
            // The BGRA8 image's contents are overwritten by the blit, so it enters from UNDEFINED.
            capture_barrier(
                raw,
                cmd,
                dst_image,
                color_range,
                vk::ImageLayout::UNDEFINED,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::PipelineStageFlags2::TOP_OF_PIPE,
                vk::AccessFlags2::NONE,
                vk::PipelineStageFlags2::BLIT,
                vk::AccessFlags2::TRANSFER_WRITE,
            );
            raw.cmd_blit_image(
                cmd,
                src_image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                dst_image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[blit],
                // 1:1 — the offscreen is already the display extent, so the blit only converts.
                vk::Filter::NEAREST,
            );
            capture_barrier(
                raw,
                cmd,
                dst_image,
                color_range,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                vk::PipelineStageFlags2::BLIT,
                vk::AccessFlags2::TRANSFER_WRITE,
                vk::PipelineStageFlags2::COPY,
                vk::AccessFlags2::TRANSFER_READ,
            );
            raw.cmd_copy_image_to_buffer(
                cmd,
                dst_image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                readback,
                &[copy],
            );
            let readback_ready = vk::BufferMemoryBarrier2::default()
                .src_stage_mask(vk::PipelineStageFlags2::COPY)
                .src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
                .dst_stage_mask(vk::PipelineStageFlags2::COPY)
                .dst_access_mask(vk::AccessFlags2::TRANSFER_READ)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .buffer(readback)
                .offset(0)
                .size(vk::WHOLE_SIZE);
            let readback_barriers = [readback_ready];
            let dep = vk::DependencyInfo::default().buffer_memory_barriers(&readback_barriers);
            raw.cmd_pipeline_barrier2(cmd, &dep);
            let region = vk::BufferCopy::default().size(byte_size);
            raw.cmd_copy_buffer(cmd, readback, staging, &[region]);
            // Make the staging write visible to host reads once the frame fence signals.
            let host = vk::BufferMemoryBarrier2::default()
                .src_stage_mask(vk::PipelineStageFlags2::COPY)
                .src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
                .dst_stage_mask(vk::PipelineStageFlags2::HOST)
                .dst_access_mask(vk::AccessFlags2::HOST_READ)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .buffer(staging)
                .offset(0)
                .size(vk::WHOLE_SIZE);
            let host_barriers = [host];
            let dep = vk::DependencyInfo::default().buffer_memory_barriers(&host_barriers);
            raw.cmd_pipeline_barrier2(cmd, &dep);
        }

        // The offscreen rests in TRANSFER_SRC after the copy; the next frame's first write
        // transitions from this tracked layout.
        self.views[active].offscreen.layout = vk::ImageLayout::TRANSFER_SRC_OPTIMAL;
        // A readback was recorded into this slot; its bytes are host-visible once this frame's
        // fence signals (published `MAX_FRAMES_IN_FLIGHT` frames later).
        if let Some(capture) = self.views[active].shm_capture.slots[slot].as_mut() {
            capture.valid = true;
        }
        Ok(())
    }

    /// Copies the active view's raw `RGBA16F` offscreen into a host-visible buffer through a
    /// one-off submit and returns `(extent, format, raw bytes)` — the read-back behind
    /// [`Renderer::capture_viewport`], which needs the unconverted halves for tonemap/clamp
    /// encoding. The device is idled first because an in-flight frame may still sample the
    /// offscreen; the image is left in `ShaderReadOnlyOptimal`.
    pub(super) fn read_active_offscreen(&mut self) -> Result<(vk::Extent2D, vk::Format, Vec<u8>)> {
        let raw = self.device.raw();
        let view = &mut self.views[self.active_view.index()];
        let extent = view.offscreen.extent;
        let format = view.offscreen.format;
        let from_layout = view.offscreen.layout;
        let image = view.offscreen.handle();
        let byte_size = extent.width as vk::DeviceSize
            * extent.height as vk::DeviceSize
            * crate::format_pixel_bytes(format) as vk::DeviceSize;

        // The offscreen may still be sampled by an in-flight frame; idle so the read-back's
        // layout transition cannot race that read.
        self.device.wait_idle()?;

        let buffer = crate::Buffer::new(
            self.device.resources(),
            byte_size,
            vk::BufferUsageFlags::TRANSFER_DST,
            &vk_mem::AllocationCreateInfo {
                usage: vk_mem::MemoryUsage::Auto,
                flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_RANDOM
                    | vk_mem::AllocationCreateFlags::MAPPED,
                ..Default::default()
            },
        )?;

        let pool = self.frames.command_pool();
        let alloc = vk::CommandBufferAllocateInfo::default()
            .command_pool(pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        // SAFETY: the ash seam. One primary buffer from the current frame slot's pool;
        // freed below after the submit fence signals.
        let cmd = checked(
            unsafe { raw.allocate_command_buffers(&alloc) },
            "capture: allocate_command_buffers",
        )?[0];
        // SAFETY: the ash seam. A default (unsignaled) fence, destroyed below.
        let fence = checked(
            unsafe { raw.create_fence(&vk::FenceCreateInfo::default(), None) },
            "capture: create_fence",
        )?;

        // The entry barrier's source scope matches the offscreen's current layout:
        // COLOR_ATTACHMENT straight after a scene render, ShaderReadOnly after a prior
        // read-back, or UNDEFINED before any frame rendered. The device is idled above, so this
        // is for layout correctness, not cross-queue sync.
        let (from_stage, from_access) = match from_layout {
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL => (
                vk::PipelineStageFlags2::FRAGMENT_SHADER,
                vk::AccessFlags2::SHADER_SAMPLED_READ,
            ),
            vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL => (
                vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT,
                vk::AccessFlags2::COLOR_ATTACHMENT_WRITE,
            ),
            _ => (vk::PipelineStageFlags2::TOP_OF_PIPE, vk::AccessFlags2::NONE),
        };
        let color_range = vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            base_mip_level: 0,
            level_count: 1,
            base_array_layer: 0,
            layer_count: 1,
        };

        let begin = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
        let recorded = (|| -> Result<()> {
            // SAFETY: the ash seam. Begin / barrier / copy / barrier / end on the one-off
            // buffer; the image + buffer outlive the recorded commands.
            unsafe {
                checked(
                    raw.begin_command_buffer(cmd, &begin),
                    "capture: begin_command_buffer",
                )?;
                capture_barrier(
                    raw,
                    cmd,
                    image,
                    color_range,
                    from_layout,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    from_stage,
                    from_access,
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
                    .image_extent(vk::Extent3D {
                        width: extent.width,
                        height: extent.height,
                        depth: 1,
                    });
                raw.cmd_copy_image_to_buffer(
                    cmd,
                    image,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    buffer.handle(),
                    &[region],
                );
                capture_barrier(
                    raw,
                    cmd,
                    image,
                    color_range,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                    vk::PipelineStageFlags2::COPY,
                    vk::AccessFlags2::TRANSFER_READ,
                    vk::PipelineStageFlags2::FRAGMENT_SHADER,
                    vk::AccessFlags2::SHADER_SAMPLED_READ,
                );
                checked(raw.end_command_buffer(cmd), "capture: end_command_buffer")?;
            }
            let cmd_info = [vk::CommandBufferSubmitInfo::default().command_buffer(cmd)];
            let submit = [vk::SubmitInfo2::default().command_buffer_infos(&cmd_info)];
            // SAFETY: the ash seam. The device was idled above, so the single graphics
            // queue is free; the fence belongs to this device.
            unsafe {
                self.device.graphics_queue.submit2(
                    raw,
                    &submit,
                    fence,
                    "capture: queue_submit2",
                )?;
                checked(
                    raw.wait_for_fences(&[fence], true, u64::MAX),
                    "capture: wait_for_fences",
                )?;
            }
            Ok(())
        })();

        // Reflect the post-capture layout in the tracked state so the next frame's graph
        // import seeds the right entry layout.
        self.views[self.active_view.index()].offscreen.layout =
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL;

        // SAFETY: the ash seam. The fence was waited (or the submit failed before
        // signaling), so the buffer + fence are idle and freed exactly once.
        unsafe {
            raw.free_command_buffers(pool, &[cmd]);
            raw.destroy_fence(fence, None);
        }
        recorded?;

        let pixel_count = byte_size as usize;
        // SAFETY: the buffer is HOST_VISIBLE + MAPPED for `byte_size` bytes; the copy
        // completed (the fence was waited).
        let pixels = unsafe { std::slice::from_raw_parts(buffer.mapped_ptr(), pixel_count) };
        Ok((extent, format, pixels.to_vec()))
    }

    /// Arms a window/composited-output screenshot for the next present: the swapchain image (the
    /// actual composited window output, unlike [`Renderer::capture_viewport`]) is copied to a
    /// host buffer and written to `path` at the next [`Renderer::render_frame`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::ShaderLoad`] if the surface lacks `TRANSFER_SRC` usage (the
    /// swapchain was not created capture-capable, so the image cannot be copied).
    pub fn request_window_capture(&mut self, path: &std::path::Path) -> Result<()> {
        let Some(swapchain) = self.swapchain.as_ref() else {
            return Err(Error::ShaderLoad(
                "window capture unsupported: the editor/headless host has no present swapchain"
                    .to_owned(),
            ));
        };
        if !swapchain.capture_supported {
            return Err(Error::ShaderLoad(
                "window capture unsupported: surface lacks TRANSFER_SRC usage".to_owned(),
            ));
        }
        self.capture_next_window_path = Some(path.to_path_buf());
        Ok(())
    }

    /// Whether a window capture is armed for the next present.
    pub fn window_capture_pending(&self) -> bool {
        self.capture_next_window_path.is_some()
    }

    /// Copies the just-presented swapchain `image` (left in `PRESENT_SRC_KHR`) into a host buffer
    /// and writes it to the armed path as a PNG, then clears the pending path. Called from
    /// [`Renderer::render_frame`] after the present submit's fence has signalled. A failure is
    /// logged, not fatal — a screenshot must never crash the frame loop.
    pub(super) fn run_pending_window_capture(&mut self, image_index: usize) {
        let Some(path) = self.capture_next_window_path.take() else {
            return;
        };
        let swapchain = self.present_swapchain();
        let image = swapchain.image(image_index);
        let extent = swapchain.extent;
        let format = swapchain.format;
        if let Err(err) = self.copy_swapchain_to_png(image, extent, format, &path) {
            tracing::warn!("window capture failed: {err}");
        } else {
            tracing::info!(
                "captured window ({}x{}) to {}",
                extent.width,
                extent.height,
                path.display()
            );
        }
    }

    /// Copies a swapchain image (in `PRESENT_SRC_KHR`) into a host-visible buffer through a
    /// one-off submit and writes it to `path` as a PNG. The device is idled first so the
    /// copy cannot race the presentation engine's read of the image.
    fn copy_swapchain_to_png(
        &self,
        image: vk::Image,
        extent: vk::Extent2D,
        format: vk::Format,
        path: &std::path::Path,
    ) -> Result<()> {
        let raw = self.device.raw();
        let byte_size = extent.width as vk::DeviceSize
            * extent.height as vk::DeviceSize
            * crate::format_pixel_bytes(format) as vk::DeviceSize;

        // The presentation engine may still be reading the image; idle so the capture's
        // transition cannot race it.
        self.device.wait_idle()?;

        let buffer = crate::Buffer::new(
            self.device.resources(),
            byte_size,
            vk::BufferUsageFlags::TRANSFER_DST,
            &vk_mem::AllocationCreateInfo {
                usage: vk_mem::MemoryUsage::Auto,
                flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_RANDOM
                    | vk_mem::AllocationCreateFlags::MAPPED,
                ..Default::default()
            },
        )?;

        // SAFETY: the ash seam. A transient one-off pool freed at the end of this call.
        let pool = checked(
            unsafe {
                raw.create_command_pool(
                    &vk::CommandPoolCreateInfo::default()
                        .flags(vk::CommandPoolCreateFlags::TRANSIENT)
                        .queue_family_index(self.device.graphics_queue_family),
                    None,
                )
            },
            "window capture: create_command_pool",
        )?;
        // SAFETY: the ash seam. One primary buffer from the transient pool.
        let cmd = checked(
            unsafe {
                raw.allocate_command_buffers(
                    &vk::CommandBufferAllocateInfo::default()
                        .command_pool(pool)
                        .level(vk::CommandBufferLevel::PRIMARY)
                        .command_buffer_count(1),
                )
            },
            "window capture: allocate_command_buffers",
        )?;
        let cmd = cmd[0];
        // SAFETY: the ash seam. A default (unsignaled) fence, destroyed below.
        let fence = checked(
            unsafe { raw.create_fence(&vk::FenceCreateInfo::default(), None) },
            "window capture: create_fence",
        )?;

        let color_range = vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            base_mip_level: 0,
            level_count: 1,
            base_array_layer: 0,
            layer_count: 1,
        };
        let begin = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
        let recorded = (|| -> Result<()> {
            // SAFETY: the ash seam. Begin / copy-with-barriers / end; the image + buffer
            // outlive the submit. The image starts in PRESENT_SRC (left by record_clear)
            // and is restored to it so a later present remains valid.
            unsafe {
                checked(
                    raw.begin_command_buffer(cmd, &begin),
                    "window capture: begin_command_buffer",
                )?;
                capture_barrier(
                    raw,
                    cmd,
                    image,
                    color_range,
                    vk::ImageLayout::PRESENT_SRC_KHR,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    vk::PipelineStageFlags2::TOP_OF_PIPE,
                    vk::AccessFlags2::NONE,
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
                    .image_extent(vk::Extent3D {
                        width: extent.width,
                        height: extent.height,
                        depth: 1,
                    });
                raw.cmd_copy_image_to_buffer(
                    cmd,
                    image,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    buffer.handle(),
                    &[region],
                );
                capture_barrier(
                    raw,
                    cmd,
                    image,
                    color_range,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    vk::ImageLayout::PRESENT_SRC_KHR,
                    vk::PipelineStageFlags2::COPY,
                    vk::AccessFlags2::TRANSFER_READ,
                    vk::PipelineStageFlags2::BOTTOM_OF_PIPE,
                    vk::AccessFlags2::NONE,
                );
                checked(
                    raw.end_command_buffer(cmd),
                    "window capture: end_command_buffer",
                )?;
            }
            let cmd_info = [vk::CommandBufferSubmitInfo::default().command_buffer(cmd)];
            let submit = [vk::SubmitInfo2::default().command_buffer_infos(&cmd_info)];
            // SAFETY: the ash seam. The device was idled above, so the queue is free.
            unsafe {
                self.device.graphics_queue.submit2(
                    raw,
                    &submit,
                    fence,
                    "window capture: queue_submit2",
                )?;
                checked(
                    raw.wait_for_fences(&[fence], true, u64::MAX),
                    "window capture: wait_for_fences",
                )?;
            }
            Ok(())
        })();

        // SAFETY: the ash seam. The submit (if any) was waited; the pool + fence are idle
        // and freed exactly once.
        unsafe {
            raw.destroy_command_pool(pool, None);
            raw.destroy_fence(fence, None);
        }
        recorded?;

        // SAFETY: the buffer is HOST_VISIBLE + MAPPED for `byte_size`; the copy completed.
        let pixels = unsafe { std::slice::from_raw_parts(buffer.mapped_ptr(), byte_size as usize) };
        crate::write_png_file(pixels, extent.width, extent.height, format, path).map_err(|err| {
            Error::ShaderLoad(format!("window capture: write {}: {err}", path.display()))
        })
    }
}
