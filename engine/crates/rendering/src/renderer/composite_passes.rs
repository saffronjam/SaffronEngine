use super::*;

impl Renderer {
    /// Appends the mandatory HDR → display tonemap: an in-place compute pass on the offscreen
    /// `color` (`StorageImageRwCompute`, GENERAL layout) binding the per-view tonemap set + the
    /// `exp2(exposure_ev)` push, dispatched 8×8 over the viewport. The graph derives the layout
    /// transitions around it; a build failure leaves the offscreen linear-HDR (logged).
    pub(super) fn add_tonemap_pass(
        &self,
        graph: &mut RenderGraph,
        pipelines: &FramePipelines,
        color: RgResource,
        frame: usize,
    ) {
        let Some(tonemap) = &pipelines.tonemap else {
            return;
        };
        let view = &self.views[self.active_view.index()];
        // In-place on the DISPLAY-extent offscreen (after the resolve reconstructed it).
        let extent = view.published_extent();
        let push = TonemapPush::new(self.exposure_ev, self.tonemap_mode, self.night_factor);
        let groups = |n: u32| n.div_ceil(8);
        let groups_x = groups(extent.width);
        let groups_y = groups(extent.height);
        // The grade UBO (binding 1) is a dynamic-offset UBO — the dispatch selects this frame's slice.
        let grade_offset = view.grade_ubo_offset(frame);
        let set = view.tonemap_set;
        let raw_body = self.device.raw().clone();
        let pipeline = Arc::clone(tonemap);
        let handle = pipeline.handle();
        let layout = pipeline.layout();
        let pass = RgPass::compute("tonemap")
            .access(color, RgUsage::StorageImageRwCompute)
            .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                // SAFETY: the ash seam. The PSO/set are valid this frame; the dispatch covers the
                // viewport; the dynamic offset addresses this frame's grade slice within the bound UBO.
                unsafe {
                    raw_body.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, handle);
                    raw_body.cmd_bind_descriptor_sets(
                        cmd,
                        vk::PipelineBindPoint::COMPUTE,
                        layout,
                        0,
                        &[set],
                        &[grade_offset],
                    );
                    raw_body.cmd_push_constants(
                        cmd,
                        layout,
                        vk::ShaderStageFlags::COMPUTE,
                        0,
                        bytemuck::bytes_of(&push),
                    );
                    raw_body.cmd_dispatch(cmd, groups_x, groups_y, 1);
                }
                drop(pipeline);
            });
        graph.add_pass(pass);
    }

    /// Appends the optional ground grid + editor overlay (both graphics passes drawing on the
    /// post-tonemap offscreen `color`, depth-testing against the persisted scene `depth`). The
    /// grid runs when shown, the overlay when geometry is queued. Both load the color and load
    /// the depth read-only, so the depth-tested ranges occlude and the on-top range does not.
    pub(super) fn add_grid_overlay_passes(
        &self,
        graph: &mut RenderGraph,
        pipelines: &FramePipelines,
        color: RgResource,
        depth: RgResource,
    ) {
        // Drawn on the DISPLAY-extent resolved color, depth-testing the display-extent overlay
        // depth (`depth` is the point-upscaled `depth_display`, not the input scene depth).
        let extent = self.views[self.active_view.index()].published_extent();

        if let Some(grid) = &pipelines.grid {
            let raw_body = self.device.raw().clone();
            let pipeline = Arc::clone(grid);
            let handle = pipeline.handle();
            let layout = pipeline.layout();
            // The grid composites AFTER TAA on the post-tonemap color, so it must use the
            // UN-jittered camera or it would shimmer (the resolve never un-jitters it).
            let push = GridPush::new(self.scene_view_proj_unjittered());
            let pass = RgPass::graphics("grid", extent)
                .color(color_load_store(color))
                .depth_attachment(depth_load_readonly(depth))
                .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                    crate::record_grid(&raw_body, cmd, handle, layout, &push);
                    drop(pipeline);
                });
            graph.add_pass(pass);
        }

        if let (Some(overlay), Some(overlay_depth), Some(draw)) = (
            &pipelines.overlay,
            &pipelines.overlay_depth,
            pipelines.overlay_draw,
        ) {
            let raw_body = self.device.raw().clone();
            let on_top = Arc::clone(overlay);
            let occluded = Arc::clone(overlay_depth);
            let on_top_handle = on_top.handle();
            let occluded_handle = occluded.handle();
            let pass = RgPass::graphics("editor-overlay", extent)
                .color(color_load_store(color))
                .depth_attachment(depth_load_readonly(depth))
                .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                    crate::record_overlay(&raw_body, cmd, &draw, on_top_handle, occluded_handle);
                    drop(on_top);
                    drop(occluded);
                });
            graph.add_pass(pass);
        }
    }

    /// Appends the Lit Wireframe overlay (the `LitWireframe` view mode): re-draws the scene
    /// geometry in line polygon mode over the post-tonemap `color`, depth-tested read-only
    /// against the persisted `depth` so hidden edges are occluded. A no-op unless the mode's PSO
    /// resolved (a `fill_mode_non_solid` device); one executor PSO replays every opaque bucket.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn add_lit_wireframe_pass(
        &self,
        graph: &mut RenderGraph,
        pipelines: &FramePipelines,
        color: RgResource,
        depth: RgResource,
        bindless_set: vk::DescriptorSet,
        instance_set: vk::DescriptorSet,
        deformed_res: Option<RgResource>,
        executor_inputs: Option<crate::ExecutorDrawInputs>,
        executor_draws: &[(crate::ExecutorBucket, bool, Arc<crate::Pipeline>)],
    ) {
        let Some(pipeline) = &pipelines.wireframe_overlay else {
            return;
        };
        // Re-drawn at DISPLAY extent, depth-tested against the display-extent overlay depth.
        let extent = self.views[self.active_view.index()].published_extent();
        let raw_for_body = self.device.raw().clone();
        let pipeline = Arc::clone(pipeline);
        let handle = pipeline.handle();
        let layout = pipeline.layout();
        let view_proj = self.frame_deformation.view_proj;
        let draws = executor_draws.to_vec();
        let pages_buffer = self.global_gpu_data.pages.buffer();
        let draw_count_supported = self.device.capabilities.draw_indirect_count;
        let mut pass = RgPass::graphics("lit-wireframe", extent)
            .color(color_load_store(color))
            .depth_attachment(depth_load_readonly(depth))
            .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                if let Some(inputs) = executor_inputs {
                    record_executor_depth_family(
                        &raw_for_body,
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
            pass = access_displaced_arena(graph, pass, self.displaced_frame, false);
        }
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
        graph.add_pass(pass);
    }
}
