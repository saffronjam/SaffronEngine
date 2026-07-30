use super::*;

impl Renderer {
    /// Appends the motion-vector prepass when its PSO + targets resolved this frame: clear
    /// the rg16f motion target + its depth scratch, draw every batch with the cur/prev
    /// camera viewProj (the per-view `prev_view_proj`). Returns the imported motion resource
    /// (the TAA / SSGI-accum passes sample it), or `None` when motion did not run.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn add_motion_pass(
        &self,
        graph: &mut RenderGraph,
        pipelines: &FramePipelines,
        bindless_set: vk::DescriptorSet,
        instance_set: vk::DescriptorSet,
        deformed: (Option<RgResource>, Option<vk::Buffer>),
        prev_deformed: (Option<RgResource>, Option<vk::Buffer>),
        executor_inputs: Option<crate::ExecutorDrawInputs>,
        executor_draws: &[(crate::ExecutorBucket, bool, Arc<crate::Pipeline>)],
    ) -> Option<(RgResource, RgResource)> {
        let motion_pipeline = pipelines.motion.as_ref()?;
        let (deformed_res, _) = deformed;
        let (prev_deformed_res, _) = prev_deformed;
        let view = &self.views[self.active_view.index()];
        let (motion_image, motion_depth) = match (&view.motion, &view.motion_depth) {
            (Some(motion), Some(depth)) => (motion, depth),
            _ => return None,
        };
        // The motion prepass rasterises at INPUT extent (with the scene).
        let extent = view.scaled_render_extent();
        let motion = graph.import_image(
            motion_image.handle(),
            motion_image.view(),
            vk::ImageAspectFlags::COLOR,
            vk::ImageLayout::UNDEFINED,
            None,
        );
        let motion_depth = graph.import_image(
            motion_depth.handle(),
            motion_depth.view(),
            vk::ImageAspectFlags::DEPTH,
            vk::ImageLayout::UNDEFINED,
            None,
        );
        // The motion prepass reprojects with the UN-jittered matrices so static geometry keeps
        // exact zero velocity — the sub-pixel scene jitter must not leak into the velocity buffer.
        let cur_view_proj = self.scene_view_proj_unjittered();
        let push = crate::MotionPush {
            cur_view_proj,
            prev_view_proj: if view.prev_view_proj_valid {
                view.prev_view_proj
            } else {
                // The first frame (no history) reprojects against itself → zero motion.
                cur_view_proj
            },
        };
        let raw_body = self.device.raw().clone();
        let pipeline = Arc::clone(motion_pipeline);
        let motion_handle = pipeline.handle();
        let motion_layout = pipeline.layout();
        let draws = executor_draws.to_vec();
        let pages_buffer = self.global_gpu_data.pages.buffer();
        let draw_count_supported = self.device.capabilities.draw_indirect_count;
        let mut pass = RgPass::graphics("motion", extent)
            .color(RgAttachment::clear_store(motion))
            .depth_attachment(depth_clear_store(motion_depth));
        pass = access_displaced_arena(graph, pass, self.displaced_frame, true);
        let mut pass = pass.body(move |cmd, _scopes: &mut NestedScopeRecorder| {
            if let Some(inputs) = executor_inputs {
                record_executor_depth_family(
                    &raw_body,
                    cmd,
                    (motion_handle, motion_layout),
                    vk::ShaderStageFlags::VERTEX,
                    bytemuck::bytes_of(&push),
                    bindless_set,
                    instance_set,
                    inputs,
                    pages_buffer,
                    draw_count_supported,
                    &draws,
                    false,
                );
            }
            drop(pipeline);
        });
        if let Some(inputs) = executor_inputs {
            let pages_res = graph.import_buffer(pages_buffer, None);
            let commands_res = graph.import_buffer(inputs.commands, None);
            let counters_res = graph.import_buffer(inputs.counters, None);
            let bucket_counts_res = graph.import_buffer(inputs.bucket_counts, None);
            pass = pass
                .access(pages_res, RgUsage::IndexInputRead)
                .access(commands_res, RgUsage::IndirectCommandRead)
                .access(counters_res, RgUsage::IndirectCountRead)
                .access(bucket_counts_res, RgUsage::IndirectCountRead);
        }
        // The motion executor pulls BOTH deformed buffers through their device
        // addresses (current + previous position), so declare both reads for the
        // skin-write → pull barrier on each. The micro-blade candidates pull the same
        // way (micro-pass-write → pull).
        let micro_candidates_res =
            graph.import_buffer(self.global_gpu_data.micro_candidates.handle(), None);
        pass = pass.access(micro_candidates_res, RgUsage::ShaderDeviceAddressRead);
        if let Some(wind_records) = self.wind_records_handle() {
            let wind_records_res = graph.import_buffer(wind_records, None);
            pass = pass.access(wind_records_res, RgUsage::ShaderDeviceAddressRead);
        }
        if let Some(deformed) = deformed_res {
            pass = pass.access(deformed, RgUsage::ShaderDeviceAddressRead);
        }
        if let Some(prev_deformed) = prev_deformed_res {
            pass = pass.access(prev_deformed, RgUsage::ShaderDeviceAddressRead);
        }
        graph.add_pass(pass);
        // Return the motion colour + the motion-prepass depth (the TAA resolve reads the depth
        // for closest-depth velocity dilation).
        Some((motion, motion_depth))
    }

    /// Appends the FXAA edge-blur compute pass when its PSO resolved this frame: sample the
    /// scene's 1× result (`scene_output` = scratch) and store the blurred result into the
    /// offscreen (`color`).
    pub(super) fn add_fxaa_pass(
        &self,
        graph: &mut RenderGraph,
        pipelines: &FramePipelines,
        scene_output: RgResource,
        color: RgResource,
    ) {
        let Some(fxaa) = &pipelines.fxaa else {
            return;
        };
        let view = &self.views[self.active_view.index()];
        // Dispatched over the DISPLAY grid: FXAA reads the input scratch by normalized UV (a
        // bilinear upscale of the edge-blurred input) and writes the display-extent offscreen.
        let extent = view.published_extent();
        let groups = |n: u32| n.div_ceil(8);
        self.add_compute_pass(
            graph,
            "fxaa",
            fxaa,
            view.fxaa_set,
            &[
                (scene_output, RgUsage::SampledReadCompute),
                (color, RgUsage::StorageImageRwCompute),
            ],
            None,
            groups(extent.width),
            groups(extent.height),
            1,
        );
    }

    /// Appends the no-AA / MSAA scene-resolve copy: a normalized-UV upscale of the input-extent
    /// scene scratch into the display-extent offscreen, dispatched over the display grid. FXAA and
    /// TAA resolve to the offscreen themselves, so this runs only when neither is active.
    pub(super) fn add_scene_resolve_pass(
        &self,
        graph: &mut RenderGraph,
        pipelines: &FramePipelines,
        scene_output: RgResource,
        color: RgResource,
    ) {
        let Some(resolve) = &pipelines.scene_resolve else {
            return;
        };
        let view = &self.views[self.active_view.index()];
        let extent = view.published_extent();
        let groups = |n: u32| n.div_ceil(8);
        self.add_compute_pass(
            graph,
            "scene-resolve",
            resolve,
            view.scene_resolve_set,
            &[
                (scene_output, RgUsage::SampledReadCompute),
                (color, RgUsage::StorageImageRwCompute),
            ],
            None,
            groups(extent.width),
            groups(extent.height),
            1,
        );
    }

    /// Appends the depth-upscale graphics pass: point-upscales the input-extent scene `depth` into
    /// the view's display-extent `depth_display` so the display-extent overlays occlude correctly
    /// under upsampling. `None` when the PSO or target is unavailable.
    pub(super) fn add_depth_upscale_pass(
        &self,
        graph: &mut RenderGraph,
        pipelines: &FramePipelines,
        depth: RgResource,
    ) -> Option<RgResource> {
        let pipeline = pipelines.depth_upscale.as_ref()?;
        let view = &self.views[self.active_view.index()];
        let depth_display = view.depth_display.as_ref()?;
        let input = view.scaled_render_extent();
        let display = view.published_extent();
        let dd = graph.import_image(
            depth_display.handle(),
            depth_display.view(),
            vk::ImageAspectFlags::DEPTH,
            vk::ImageLayout::UNDEFINED,
            None,
        );
        let raw_body = self.device.raw().clone();
        let pipeline = Arc::clone(pipeline);
        let handle = pipeline.handle();
        let layout = pipeline.layout();
        let set = view.depth_upscale_set;
        let mut push = Vec::with_capacity(8);
        push.extend_from_slice(&(input.width as f32).to_ne_bytes());
        push.extend_from_slice(&(input.height as f32).to_ne_bytes());
        // The pass samples the input scene depth (declared read → the graph transitions it
        // DepthWrite → ShaderReadOnly after the scene) and depth-writes `depth_display`.
        let pass = RgPass::graphics("depth-upscale", display)
            .depth_attachment(depth_clear_store(dd))
            .access(depth, RgUsage::SampledRead)
            .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                // SAFETY: the ash seam. Fullscreen triangle: bind the input-depth sampler set +
                // the inputSize push, then draw 3 vertices (no vertex buffer).
                unsafe {
                    raw_body.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, handle);
                    raw_body.cmd_bind_descriptor_sets(
                        cmd,
                        vk::PipelineBindPoint::GRAPHICS,
                        layout,
                        0,
                        &[set],
                        &[],
                    );
                    raw_body.cmd_push_constants(
                        cmd,
                        layout,
                        vk::ShaderStageFlags::FRAGMENT,
                        0,
                        &push,
                    );
                    raw_body.cmd_draw(cmd, 3, 1, 0, 0);
                }
                drop(pipeline);
            });
        graph.add_pass(pass);
        Some(dd)
    }

    /// Appends the TAA reactive-coverage pass: color-clears the view's input-extent r8 reactive
    /// mask, then re-draws the translucent batches into it (constant-1.0 fragment, depth-tested
    /// read-only against `scene_depth`) so the resolve can bias alpha-blended pixels toward the
    /// current frame. Runs only under TAA. Returns the reactive-mask resource, or `None` when the
    /// PSO / target is unavailable (the resolve then falls back to a fully-cleared mask).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn add_reactive_coverage_pass(
        &self,
        graph: &mut RenderGraph,
        pipelines: &FramePipelines,
        scene_depth: RgResource,
        bindless_set: vk::DescriptorSet,
        instance_set: vk::DescriptorSet,
        executor_inputs: Option<crate::ExecutorDrawInputs>,
        executor_draws: &[(crate::ExecutorBucket, bool, Arc<crate::Pipeline>)],
    ) -> Option<RgResource> {
        let pipeline = pipelines.reactive_coverage.as_ref()?;
        let transition_keepalive = pipelines.reactive_transition.clone();
        let transition_pipeline = transition_keepalive
            .as_ref()
            .map(|pipeline| (pipeline.handle(), pipeline.layout()));
        let view = &self.views[self.active_view.index()];
        let reactive = view.reactive.as_ref()?;
        let input = view.scaled_render_extent();
        // Cleared every frame (LOAD_OP_CLEAR), so the prior content is discarded — import at
        // UNDEFINED and let the clear own it (no cross-frame layout slot needed).
        let reactive_res = graph.import_image(
            reactive.handle(),
            reactive.view(),
            vk::ImageAspectFlags::COLOR,
            vk::ImageLayout::UNDEFINED,
            None,
        );
        let raw_body = self.device.raw().clone();
        let pipeline = Arc::clone(pipeline);
        let handle = pipeline.handle();
        let layout = pipeline.layout();
        let view_proj = self.frame_deformation.view_proj;
        let draws = executor_draws.to_vec();
        let pages_buffer = self.global_gpu_data.pages.buffer();
        let draw_count_supported = self.device.capabilities.draw_indirect_count;
        let mut color_att = RgAttachment::clear_store(reactive_res);
        color_att.clear_value = vk::ClearValue {
            color: vk::ClearColorValue {
                float32: [0.0, 0.0, 0.0, 0.0],
            },
        };
        // Read-only depth test against the resolved scene depth (declared by the attachment), so
        // occluded translucent fragments don't mark coverage. The blend buckets' binned
        // commands are the translucent draws; order is irrelevant for a coverage mask.
        let mut pass = RgPass::graphics("reactive-coverage", input)
            .color(color_att)
            .depth_attachment(depth_load_readonly(scene_depth))
            .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                if let Some(inputs) = executor_inputs {
                    record_executor_depth_family(
                        &raw_body,
                        cmd,
                        (handle, layout),
                        vk::ShaderStageFlags::VERTEX,
                        bytemuck::bytes_of(&view_proj),
                        bindless_set,
                        instance_set,
                        inputs,
                        pages_buffer,
                        draw_count_supported,
                        &draws,
                        true,
                    );
                    // The opaque buckets re-walk through the degenerate-collapse entry:
                    // only blades and records mid representation-transition rasterize.
                    if let Some((transition_handle, transition_layout)) = transition_pipeline {
                        record_executor_depth_family(
                            &raw_body,
                            cmd,
                            (transition_handle, transition_layout),
                            vk::ShaderStageFlags::VERTEX,
                            bytemuck::bytes_of(&view_proj),
                            bindless_set,
                            instance_set,
                            inputs,
                            pages_buffer,
                            draw_count_supported,
                            &draws,
                            false,
                        );
                    }
                }
                drop(pipeline);
                drop(transition_keepalive);
            });
        if let Some(inputs) = executor_inputs {
            let pages_res = graph.import_buffer(pages_buffer, None);
            let commands_res = graph.import_buffer(inputs.commands, None);
            let counters_res = graph.import_buffer(inputs.counters, None);
            let bucket_counts_res = graph.import_buffer(inputs.bucket_counts, None);
            pass = pass
                .access(pages_res, RgUsage::IndexInputRead)
                .access(commands_res, RgUsage::IndirectCommandRead)
                .access(counters_res, RgUsage::IndirectCountRead)
                .access(bucket_counts_res, RgUsage::IndirectCountRead);
        }
        graph.add_pass(pass);
        Some(reactive_res)
    }

    /// Appends the TAA resolve compute pass when its PSO + motion resolved this frame: reproject
    /// the previous history through the motion vector, neighborhood-clamp, and blend with the
    /// current scene into the offscreen plus the next-frame history. Parity `p` reads history
    /// `1 - p` and writes history `p`. Returns the history images' external-layout slots.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn add_taa_pass(
        &self,
        graph: &mut RenderGraph,
        pipelines: &FramePipelines,
        scene_output: RgResource,
        color: RgResource,
        motion: Option<RgResource>,
        motion_depth: Option<RgResource>,
        reactive: Option<RgResource>,
    ) -> Option<TaaResolveSlots> {
        let taa = pipelines.taa.as_ref()?;
        let motion = motion?;
        // The motion prepass produces the colour + depth together; the resolve needs both.
        let motion_depth = motion_depth?;
        let view = &self.views[self.active_view.index()];
        // The resolve dispatches over the DISPLAY grid (one invocation per display pixel); it
        // samples the input-extent scene / motion by normalized UV and reconstructs upward.
        let extent = view.published_extent();
        let p = view.history_index;
        let (history_read, history_write) = match (&view.history[1 - p], &view.history[p]) {
            (Some(read), Some(write)) => (read, write),
            _ => return None,
        };
        // The two history images carry their layout across frames (the graph internally
        // pings ShaderReadOnly → General for the write and back), so each rides an external
        // slot whose resolved exit layout is read back after execute.
        let read_slot = graph.alloc_external_state(history_read.graph_state());
        let write_slot = graph.alloc_external_state(history_write.graph_state());
        let hist_read = graph.import_image(
            history_read.handle(),
            history_read.view(),
            vk::ImageAspectFlags::COLOR,
            history_read.layout,
            Some(read_slot),
        );
        let hist_write = graph.import_image(
            history_write.handle(),
            history_write.view(),
            vk::ImageAspectFlags::COLOR,
            history_write.layout,
            Some(write_slot),
        );
        // The pixel-lock ping-pong (same parity as history): read the opposite parity at the
        // reprojected UV, write this parity. Each carries its layout across frames like history.
        let (lock_read, lock_write) = match (&view.lock[1 - p], &view.lock[p]) {
            (Some(read), Some(write)) => (read, write),
            _ => return None,
        };
        let lock_read_slot = graph.alloc_external_state(lock_read.graph_state());
        let lock_write_slot = graph.alloc_external_state(lock_write.graph_state());
        let lock_read_res = graph.import_image(
            lock_read.handle(),
            lock_read.view(),
            vk::ImageAspectFlags::COLOR,
            lock_read.layout,
            Some(lock_read_slot),
        );
        let lock_write_res = graph.import_image(
            lock_write.handle(),
            lock_write.view(),
            vk::ImageAspectFlags::COLOR,
            lock_write.layout,
            Some(lock_write_slot),
        );
        let params = self.taa_params;
        // `screen_size` is the INPUT/render extent (velocity → pixels, and the source grid the
        // resolve samples); it diverges from the display dispatch/output extent under upsampling.
        let input = view.scaled_render_extent();
        // The upscale ratio `n = displayW / inputW` and the per-output accumulation target
        // (`8·n²`, the jitter cycle length, floored at the native warm-up) the confidence
        // saturates against.
        let n = extent.width.max(1) as f32 / input.width.max(1) as f32;
        let sample_target =
            (crate::TAA_JITTER_PHASES as f32 * n * n).max(crate::TAA_JITTER_PHASES as f32);
        let push = crate::TaaPush {
            feedback: saffron_geometry::glam::Vec2::new(params.feedback_min, params.feedback_max),
            jitter: view.jitter,
            prev_jitter: view.prev_jitter,
            screen_size: saffron_geometry::glam::Vec2::new(input.width as f32, input.height as f32),
            gamma_valid: saffron_geometry::glam::Vec2::new(
                params.clip_gamma,
                if view.history_valid { 1.0 } else { 0.0 },
            ),
            reject_sharp: saffron_geometry::glam::Vec2::new(
                params.velocity_rejection,
                params.sharpness,
            ),
            upscale: saffron_geometry::glam::Vec2::new(n, sample_target),
            // Reconstruction robustness: the lock initial lifetime + reactive scale, the
            // disocclusion + lock-break thresholds, and the camera planes for depth linearization.
            // `current` + `history` are both raw linear-HDR at one scale — a pre-exposure must
            // scale both (and the value written to outHistory), never one, or accumulation drifts.
            lock_reactive: saffron_geometry::glam::Vec2::new(
                params.lock_lifetime,
                params.reactive_scale,
            ),
            disoccl: saffron_geometry::glam::Vec2::new(
                params.disocclusion_threshold,
                params.lock_break_luma,
            ),
            depth_params: saffron_geometry::glam::Vec2::new(
                self.camera_near_far.0,
                self.camera_near_far.1,
            ),
        };
        // The reactive mask feeds slot 6. When the coverage pass didn't run (PSO build failed),
        // `reactive` is None and the slot keeps its placeholder binding (never a real declared
        // access — it rests ShaderReadOnly, so the stale sample is validation-safe but inert).
        let mut accesses = vec![
            (scene_output, RgUsage::SampledReadCompute),
            (motion, RgUsage::SampledReadCompute),
            // Closest-depth dilation source — ordered after the motion prepass's depth store.
            (motion_depth, RgUsage::SampledReadCompute),
            (lock_read_res, RgUsage::SampledReadCompute),
            (hist_read, RgUsage::SampledReadCompute),
            (color, RgUsage::StorageImageRwCompute),
            (hist_write, RgUsage::StorageImageRwCompute),
            (lock_write_res, RgUsage::StorageImageRwCompute),
        ];
        if let Some(reactive) = reactive {
            accesses.push((reactive, RgUsage::SampledReadCompute));
        }
        let groups = |n: u32| n.div_ceil(8);
        self.add_compute_pass(
            graph,
            "taa",
            taa,
            view.taa_sets[p],
            &accesses,
            Some(bytemuck::bytes_of(&push).to_vec()),
            groups(extent.width),
            groups(extent.height),
            1,
        );
        Some(TaaResolveSlots {
            history: TaaHistorySlots {
                read: (1 - p, read_slot),
                write: (p, write_slot),
            },
            lock: TaaHistorySlots {
                read: (1 - p, lock_read_slot),
                write: (p, lock_write_slot),
            },
        })
    }

    /// Appends the motion-vector visualization (the `MotionVectors` view mode): a fullscreen
    /// compute that samples the motion target and overwrites the post-tonemap `color`. A no-op
    /// unless the mode's PSO is resolved and the motion target ran this frame (TAA or SSGI on);
    /// otherwise the shaded scene shows through.
    pub(super) fn add_motion_visualize_pass(
        &self,
        graph: &mut RenderGraph,
        pipelines: &FramePipelines,
        color: RgResource,
        motion: Option<RgResource>,
    ) {
        let (Some(pipeline), Some(motion)) = (&pipelines.motion_visualize, motion) else {
            return;
        };
        let view = &self.views[self.active_view.index()];
        // In-place over the DISPLAY-extent post-tonemap color; motion is sampled by normalized UV.
        let extent = view.published_extent();
        let mut push = Vec::with_capacity(8);
        push.extend_from_slice(&extent.width.to_ne_bytes());
        push.extend_from_slice(&extent.height.to_ne_bytes());
        let groups = |n: u32| n.div_ceil(8);
        self.add_compute_pass(
            graph,
            "motion-visualize",
            pipeline,
            view.motion_vis_set,
            &[
                (motion, RgUsage::SampledReadCompute),
                (color, RgUsage::StorageImageRwCompute),
            ],
            Some(push),
            groups(extent.width),
            groups(extent.height),
            1,
        );
    }
}

/// Writes a TAA history image's resolved exit layout back from the graph's external slot.
/// `(history-index, slot)` selects the image in `view.history` and the slot to read.
pub(super) fn writeback_history_layout(
    view: &mut ViewTarget,
    graph: &RenderGraph,
    slot: &(usize, usize),
) {
    if let Some(image) = view.history[slot.0].as_mut() {
        image.set_graph_state(graph.external_state(slot.1));
    }
}

/// Writes a TAA pixel-lock image's resolved exit layout back from the graph's external slot.
/// `(lock-index, slot)` selects the image in `view.lock` and the slot to read.
pub(super) fn writeback_lock_layout(
    view: &mut ViewTarget,
    graph: &RenderGraph,
    slot: &(usize, usize),
) {
    if let Some(image) = view.lock[slot.0].as_mut() {
        image.set_graph_state(graph.external_state(slot.1));
    }
}
