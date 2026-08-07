use super::*;

impl Renderer {
    /// The present swapchain on the windowed present path. Every caller runs only when a
    /// swapchain exists, so the `expect` cannot fire in the editor/headless host.
    pub(super) fn present_swapchain(&self) -> &Swapchain {
        self.swapchain
            .as_ref()
            .expect("present swapchain in windowed mode")
    }

    /// Rebuilds the present swapchain at `(width, height)` after a window resize.
    ///
    /// The swapchain is created once in [`Renderer::new`] and is otherwise immutable; a resize
    /// makes it out of date, so [`Renderer::begin_present_frame`] returns `false` and skips the
    /// frame until this rebuilds it at the new surface size. Waits the device idle first (an
    /// in-flight present may still reference the old images), then destroys and rebuilds the
    /// swapchain as a unit. A no-op without a swapchain and for a zero extent (minimized).
    ///
    /// # Errors
    ///
    /// Propagates a device-idle wait failure or any swapchain-creation [`Error`].
    pub fn recreate_swapchain(&mut self, width: u32, height: u32) -> Result<()> {
        if self.swapchain.is_none() || width == 0 || height == 0 {
            return Ok(());
        }
        self.device.wait_idle()?;
        // Rebuilds run between frames. An outstanding acquisition owns a binary semaphore and
        // must be presented rather than discarded, so enforce the transaction boundary.
        if let Some(present_sync) = self.present_sync.as_ref() {
            present_sync.ensure_no_acquired_frame()?;
        }
        if let Some(mut swapchain) = self.swapchain.take() {
            swapchain.destroy(&self.device);
        }
        let swapchain = Swapchain::new(&self.device, width, height)?;
        tracing::info!(
            "swapchain rebuilt {}x{}",
            swapchain.extent.width,
            swapchain.extent.height
        );
        self.swapchain = Some(swapchain);
        Ok(())
    }

    /// Begins a windowed present-only frame: waits + resets the current slot's fence
    /// (so its per-frame buffers are free, the [`Renderer::begin_offscreen_frame`] half) and
    /// acquires the next swapchain image with the slot's image-available semaphore.
    ///
    /// The standalone host renders the scene into the offscreen in `on_ui` and blits it onto this
    /// acquired image in [`Renderer::present_active_view_to_swapchain`] at `end_frame`. Returns
    /// `false` when the swapchain is out of date (a resize the caller should handle).
    /// Returns `false` when the swapchain is out of date (a resize the caller should handle by
    /// rebuilding).
    ///
    /// # Errors
    ///
    /// Returns [`Error::Vk`] for any failing fence / acquire call.
    pub fn begin_present_frame(&mut self) -> Result<bool> {
        // Acquire the swapchain image BEFORE `begin_offscreen_frame` resets the slot fence: an
        // out-of-date swapchain must skip the whole frame without leaving an unsignaled fence
        // behind, or the next frame deadlocks waiting on it.
        //
        // Wait this slot's prior present BEFORE the acquire: that present's blit waited this
        // slot's image-available semaphore, so the acquire cannot reuse the semaphore until that
        // wait completed (`VUID-vkAcquireNextImageKHR-semaphore-01779`), and the present fence is
        // the only thing that orders it. The fence is created signaled, so the first cycle's wait
        // returns immediately; `present_active_view_to_swapchain` resets it before resubmit.

        // Close a slot a previous frame left armed before any of that: the acquire binds this
        // frame's ring index into the present transaction, and closing advances the ring.
        self.finish_unsubmitted_frame()?;
        let present_fence = self
            .present_sync
            .as_ref()
            .map(|present_sync| present_sync.present_fence(self.frames.index()));
        if let Some(present_fence) = present_fence {
            let raw = self.device.raw();
            // SAFETY: the ash seam. The fence belongs to this device (created signaled).
            checked(
                unsafe { raw.wait_for_fences(&[present_fence], true, u64::MAX) },
                "begin_present: wait_for_fences(present)",
            )?;
        }

        let swapchain_loader = self.device.swapchain_loader();
        let image_available = self.frames.image_available();
        // SAFETY: the ash seam. Acquires the next image, signaling image_available. The
        // present blit submit waits on it before touching the swapchain image. Its prior wait
        // is guaranteed complete by the present-fence wait above, so the reuse is valid.
        let acquire = unsafe {
            swapchain_loader.acquire_next_image(
                self.present_swapchain().handle(),
                u64::MAX,
                image_available,
                vk::Fence::null(),
            )
        };
        let image_index = match acquire {
            Ok((index, _suboptimal)) => index,
            Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => return Ok(false),
            Err(result) => {
                return Err(Error::Vk {
                    context: "acquire_next_image (present)",
                    result,
                });
            }
        };
        if let Some(present_sync) = self.present_sync.as_mut() {
            present_sync.set_acquired_frame(image_index, self.frames.index())?;
        }

        // Wait + reset this slot's fence and command pool so the slot is idle before the
        // layers' deformation submit resets per-frame state (the shared offscreen begin).
        self.begin_offscreen_frame()?;
        Ok(true)
    }

    /// Blits the active view's post-processed offscreen onto the acquired swapchain image and
    /// presents — the standalone present-only host's frame transport.
    ///
    /// Runs at `end_frame`, after `on_ui` rendered the scene + native overlay into the offscreen
    /// (which signals the slot's scene-finished semaphore). Records the offscreen → swapchain
    /// `vkCmdBlitImage` into the slot's blit buffer, submits it waiting on both the acquire's
    /// image-available semaphore and the scene-finished semaphore, then presents. A no-op when no
    /// image was acquired this frame.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Vk`] for any failing fence / record / submit / present call.
    pub fn present_active_view_to_swapchain(&mut self) -> Result<()> {
        let Some(acquired) = self
            .present_sync
            .as_mut()
            .and_then(PresentSync::take_acquired_frame)
        else {
            return Ok(()); // No image acquired (out-of-date swapchain): skip the present.
        };
        let slot = acquired.slot;
        let image_index = acquired.image_index;
        let scene_signaled = acquired.scene_finished_signaled;

        let raw = self.device.raw();
        let present_sync = self
            .present_sync
            .as_ref()
            .expect("present sync in windowed mode");
        let blit_cmd = present_sync.command_buffer(slot);
        let blit_pool = present_sync.command_pool(slot);
        let present_fence = present_sync.present_fence(slot);
        let scene_finished = present_sync.scene_finished(slot);
        let image_available = self.frames.image_available_for(slot);

        let swapchain = self.present_swapchain();
        let swap_image = swapchain.image(image_index as usize);
        let swap_extent = swapchain.extent;
        let render_finished = swapchain.render_finished(image_index as usize);
        let tracking = swapchain.image_in_flight(image_index as usize);

        // Both the slot's prior present and the acquired image's prior present must complete
        // before their resources are reused. They may be the same fence, so deduplicate and wait
        // before resetting the slot fence; resetting first would turn the alias case into an
        // infinite wait on the newly-unsignaled fence.
        let (reuse_fences, reuse_fence_count) =
            crate::present::reuse_fences(present_fence, tracking);
        // SAFETY: the ash seam. Every returned fence belongs to this device.
        checked(
            unsafe { raw.wait_for_fences(&reuse_fences[..reuse_fence_count], true, u64::MAX) },
            "present: wait_for_fences(reuse)",
        )?;
        // SAFETY: the ash seam. The slot fence was waited above and is reset before resubmit.
        checked(
            unsafe { raw.reset_fences(&[present_fence]) },
            "present: reset_fences",
        )?;
        self.swapchain
            .as_mut()
            .expect("present swapchain in windowed mode")
            .set_image_in_flight(image_index as usize, present_fence);

        let view = &self.views[self.active_view.index()];
        let offscreen = view.offscreen.handle();
        let offscreen_extent = view.offscreen.extent;
        let from_layout = view.offscreen.layout;
        // The offscreen's last writer matches its tracked layout: COLOR_ATTACHMENT after the
        // post chain's overlay pass, or ShaderReadOnly after a prior read-back.
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

        let begin = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
        // SAFETY: the ash seam. The slot's present fence was waited above, so its pool may be
        // reset; the blit references the acquired image + the offscreen, both of which outlive it.
        unsafe {
            checked(
                raw.reset_command_pool(blit_pool, vk::CommandPoolResetFlags::empty()),
                "present: reset_command_pool",
            )?;
            checked(
                raw.begin_command_buffer(blit_cmd, &begin),
                "present: begin_command_buffer",
            )?;
            crate::present::record_present_blit(
                raw,
                blit_cmd,
                offscreen,
                offscreen_extent,
                from_layout,
                from_stage,
                from_access,
                swap_image,
                swap_extent,
                vk::ImageLayout::PRESENT_SRC_KHR,
            );
            checked(
                raw.end_command_buffer(blit_cmd),
                "present: end_command_buffer",
            )?;
        }

        // Track the offscreen's new layout so the next frame's graph import seeds the right
        // entry layout (the blit left it in TRANSFER_SRC).
        self.views[self.active_view.index()].offscreen.layout =
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL;

        // Wait the acquire (image owned), and the scene-finished semaphore (offscreen rendered)
        // when it was signaled this frame; signal render-finished (the present waits on it);
        // fence the slot. When the offscreen render was skipped the blit reads the prior frame's
        // offscreen and waits the acquire alone — never an unsignaled semaphore (a deadlock).
        let blit_stage = vk::PipelineStageFlags2::BLIT;
        let mut wait = vec![
            vk::SemaphoreSubmitInfo::default()
                .semaphore(image_available)
                .stage_mask(blit_stage),
        ];
        if scene_signaled {
            wait.push(
                vk::SemaphoreSubmitInfo::default()
                    .semaphore(scene_finished)
                    .stage_mask(blit_stage),
            );
        }
        let signal = [vk::SemaphoreSubmitInfo::default()
            .semaphore(render_finished)
            .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)];
        let cmd = [vk::CommandBufferSubmitInfo::default().command_buffer(blit_cmd)];
        let submit = [vk::SubmitInfo2::default()
            .wait_semaphore_infos(&wait)
            .command_buffer_infos(&cmd)
            .signal_semaphore_infos(&signal)];
        // SAFETY: the ash seam. The graphics queue is externally synchronized; the fence was
        // reset above.
        self.device.graphics_queue.submit2(
            raw,
            &submit,
            present_fence,
            "present: queue_submit2",
        )?;

        let swapchains = [self.present_swapchain().handle()];
        let wait_semaphores = [render_finished];
        let image_indices = [image_index];
        let present_info = vk::PresentInfoKHR::default()
            .wait_semaphores(&wait_semaphores)
            .swapchains(&swapchains)
            .image_indices(&image_indices);
        // SAFETY: the ash seam. The swapchain/image-index are valid; the present waits on
        // render_finished signaled by the submit above.
        let present = self
            .device
            .graphics_queue
            .present(self.device.swapchain_loader(), &present_info);
        // A window capture armed by `request_window_capture` reads the just-presented image.
        if self.capture_next_window_path.is_some() {
            self.run_pending_window_capture(image_index as usize);
        }
        match present {
            Ok(_) | Err(vk::Result::ERROR_OUT_OF_DATE_KHR) | Err(vk::Result::SUBOPTIMAL_KHR) => {
                Ok(())
            }
            Err(result) => Err(Error::Vk {
                context: "present: queue_present",
                result,
            }),
        }
    }

    /// Records and submits one acquire → clear → present frame: wait and reset the slot's
    /// in-flight fence, acquire the next swapchain image, wait any fence still tracking it,
    /// record the clear between its two sync2 barriers, submit, and present.
    ///
    /// Returns `true` on a normal frame, `false` when the swapchain is out of date (a resize the
    /// caller should handle by rebuilding).
    ///
    /// # Errors
    ///
    /// Returns [`Error::Vk`] for any failing Vulkan call.
    pub fn render_frame(&mut self) -> Result<bool> {
        // The present path is the standalone windowed host only; the editor/headless host renders
        // offscreen and publishes to shared memory, so it never reaches here.
        if self.swapchain.is_none() {
            return Err(Error::ShaderLoad(
                "render_frame called without a present swapchain (editor/headless mode)".to_owned(),
            ));
        }
        let raw = self.device.raw();
        let in_flight = self.frames.in_flight();

        // SAFETY: the ash seam. The fence belongs to this device; the wait blocks
        // until the slot's prior GPU work completes.
        checked(
            unsafe { raw.wait_for_fences(&[in_flight], true, u64::MAX) },
            "wait_for_fences",
        )?;

        let swapchain_loader = self.device.swapchain_loader();
        let image_available = self.frames.image_available();
        // SAFETY: the ash seam. Acquires the next image, signaling image_available.
        let acquire = unsafe {
            swapchain_loader.acquire_next_image(
                self.present_swapchain().handle(),
                u64::MAX,
                image_available,
                vk::Fence::null(),
            )
        };
        let image_index = match acquire {
            Ok((index, _suboptimal)) => index as usize,
            Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => return Ok(false),
            Err(result) => {
                return Err(Error::Vk {
                    context: "acquire_next_image",
                    result,
                });
            }
        };

        // The fence still tracking this image (from up to MAX_FRAMES_IN_FLIGHT
        // frames ago) must signal before its render-finished semaphore is reused.
        let tracking = self.present_swapchain().image_in_flight(image_index);
        if tracking != vk::Fence::null() {
            // SAFETY: the ash seam. The tracking fence belongs to this device.
            checked(
                unsafe { raw.wait_for_fences(&[tracking], true, u64::MAX) },
                "wait_for_fences(image)",
            )?;
        }
        self.swapchain
            .as_mut()
            .expect("present swapchain in windowed mode")
            .set_image_in_flight(image_index, in_flight);

        // SAFETY: the ash seam. Resetting an unsignaled-after-wait fence is valid
        // and required before resubmitting work that signals it.
        checked(unsafe { raw.reset_fences(&[in_flight]) }, "reset_fences")?;

        self.record_clear(image_index)?;
        self.submit_and_present(image_index)?;
        // A window capture armed by `request_window_capture` reads the just-presented image.
        if self.capture_next_window_path.is_some() {
            self.run_pending_window_capture(image_index);
        }
        self.frames.advance();
        Ok(true)
    }

    /// Records the clear into the current frame's command buffer.
    pub(super) fn record_clear(&self, image_index: usize) -> Result<()> {
        let raw = self.device.raw();
        let command_buffer = self.frames.command_buffer();
        let image = self.present_swapchain().image(image_index);

        // SAFETY: the ash seam. The current frame's fence was waited above, so the
        // pool's buffer is no longer in use and may be reset.
        checked(
            unsafe {
                raw.reset_command_pool(
                    self.frames.command_pool(),
                    vk::CommandPoolResetFlags::empty(),
                )
            },
            "reset_command_pool",
        )?;

        let begin_info = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
        // SAFETY: the ash seam. Begins recording on the freshly reset buffer.
        checked(
            unsafe { raw.begin_command_buffer(command_buffer, &begin_info) },
            "begin_command_buffer",
        )?;

        let full_subresource = vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            base_mip_level: 0,
            level_count: 1,
            base_array_layer: 0,
            layer_count: 1,
        };

        // UNDEFINED → TRANSFER_DST: the swapchain image's contents are not preserved.
        let to_transfer = vk::ImageMemoryBarrier2::default()
            .src_stage_mask(vk::PipelineStageFlags2::TOP_OF_PIPE)
            .src_access_mask(vk::AccessFlags2::empty())
            .dst_stage_mask(vk::PipelineStageFlags2::CLEAR)
            .dst_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
            .old_layout(vk::ImageLayout::UNDEFINED)
            .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(image)
            .subresource_range(full_subresource);
        let to_transfer = [to_transfer];
        let dep_to_transfer = vk::DependencyInfo::default().image_memory_barriers(&to_transfer);
        // SAFETY: the ash seam. The barrier references the acquired swapchain image.
        unsafe { raw.cmd_pipeline_barrier2(command_buffer, &dep_to_transfer) };

        let clear = vk::ClearColorValue {
            float32: self.clear_color,
        };
        let ranges = [full_subresource];
        // SAFETY: the ash seam. The image is in TRANSFER_DST per the barrier above.
        unsafe {
            raw.cmd_clear_color_image(
                command_buffer,
                image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &clear,
                &ranges,
            );
        }

        // TRANSFER_DST → PRESENT_SRC: make the clear visible to the presentation engine.
        let to_present = vk::ImageMemoryBarrier2::default()
            .src_stage_mask(vk::PipelineStageFlags2::CLEAR)
            .src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
            .dst_stage_mask(vk::PipelineStageFlags2::BOTTOM_OF_PIPE)
            .dst_access_mask(vk::AccessFlags2::empty())
            .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .new_layout(vk::ImageLayout::PRESENT_SRC_KHR)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(image)
            .subresource_range(full_subresource);
        let to_present = [to_present];
        let dep_to_present = vk::DependencyInfo::default().image_memory_barriers(&to_present);
        // SAFETY: the ash seam. Same acquired image; recorded after the clear.
        unsafe { raw.cmd_pipeline_barrier2(command_buffer, &dep_to_present) };

        // SAFETY: the ash seam. Ends the recording opened above.
        checked(
            unsafe { raw.end_command_buffer(command_buffer) },
            "end_command_buffer",
        )?;
        Ok(())
    }

    /// Submits the recorded buffer (sync2) and presents the image.
    fn submit_and_present(&self, image_index: usize) -> Result<()> {
        let raw = self.device.raw();
        let command_buffer = self.frames.command_buffer();
        let render_finished = self.present_swapchain().render_finished(image_index);

        let wait = vk::SemaphoreSubmitInfo::default()
            .semaphore(self.frames.image_available())
            .stage_mask(vk::PipelineStageFlags2::CLEAR);
        let signal = vk::SemaphoreSubmitInfo::default()
            .semaphore(render_finished)
            .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS);
        let cmd = vk::CommandBufferSubmitInfo::default().command_buffer(command_buffer);

        let wait = [wait];
        let signal = [signal];
        let cmd = [cmd];
        let submit = vk::SubmitInfo2::default()
            .wait_semaphore_infos(&wait)
            .command_buffer_infos(&cmd)
            .signal_semaphore_infos(&signal);
        let submits = [submit];

        // SAFETY: the ash seam. The graphics queue is externally synchronized; this submit runs
        // on the render thread (the thumbnail worker submits behind the queue mutex).
        self.device.graphics_queue.submit2(
            raw,
            &submits,
            self.frames.in_flight(),
            "queue_submit2",
        )?;

        let swapchains = [self.present_swapchain().handle()];
        let wait_semaphores = [render_finished];
        let image_indices = [image_index as u32];
        let present_info = vk::PresentInfoKHR::default()
            .wait_semaphores(&wait_semaphores)
            .swapchains(&swapchains)
            .image_indices(&image_indices);

        // SAFETY: the ash seam. The swapchain/image-index are valid; the present
        // waits on render_finished signaled by the submit above.
        let present = self
            .device
            .graphics_queue
            .present(self.device.swapchain_loader(), &present_info);
        match present {
            Ok(_) | Err(vk::Result::ERROR_OUT_OF_DATE_KHR) | Err(vk::Result::SUBOPTIMAL_KHR) => {
                Ok(())
            }
            Err(result) => Err(Error::Vk {
                context: "queue_present",
                result,
            }),
        }
    }
}
