use super::*;

impl Renderer {
    /// Records the instance-visibility half of the frame: the cull against the previous HZB, the
    /// hierarchy traversal into the record stream, micro-field reconstruction, GPU binning, the
    /// transparent radix sort, and the parallel global-illumination reach view whose visible list
    /// becomes this frame's SDF occluder region.
    pub(super) fn record_visibility_passes(
        &mut self,
        graph: &mut RenderGraph,
        frame: usize,
        frame_res: &WindFrameResources,
        psos: &SceneFramePsos,
        micro_template: (u32, u32),
    ) -> Result<VisibilityFrame> {
        // A wind edit lands as a jump in the deformation, so a reprojection that assumes
        // continuous motion describes geometry that no longer exists. Reset before the temporal
        if self.take_wind_discontinuity() {
            let view = self.active_view;
            self.reset_view_temporal(view, crate::GpuSceneHistoryInvalidation::WindDiscontinuity);
        }
        let mut visibility_active = false;
        let mut visibility_history_valid = false;
        let mut executor_buckets: Vec<crate::ExecutorBucket> = Vec::new();
        let mut executor_inputs: Option<crate::ExecutorDrawInputs> = None;
        if self.views[self.active_view.index()].hzb_pyramid.is_none() {
            // Views that never pass through the resize hook (a fixed-size offscreen
            // boot) build their pyramids on first use.
            let extent = self.views[self.active_view.index()].scaled_render_extent();
            self.views[self.active_view.index()].hzb_pyramid =
                match crate::HzbPyramid::new(&self.device, &self.descriptors, &self.hzb, extent) {
                    Ok(pyramid) => Some(pyramid),
                    Err(err) => {
                        tracing::error!("hzb pyramid bring-up: {err}");
                        None
                    }
                };
        }
        if let (Some(cull_pso), Some(_), Some(_), Some(_), Some(_)) = (
            &psos.visibility.0,
            &psos.visibility.1,
            &psos.visibility.2,
            &psos.visibility.3,
            &psos.visibility.4,
        ) && self.views[self.active_view.index()].hzb_pyramid.is_some()
        {
            let instance_capacity = frame_res.address_block.instance_capacity.max(1);
            // The frame's draw buckets derive from the mirror's live (shader, class) pairs alone;
            // the blend subset sizes the sorted transparent stream, so a new blend bucket going
            // live rebuilds the view's lists exactly like instance-capacity growth.
            let displaced = self.displaced_frame;
            let (frame_buckets, bucket_table) = crate::build_executor_buckets(
                &self.live_executor_bins,
                crate::SCENE_VISIBILITY_RECORD_CAPACITY,
                displaced.is_some(),
            );
            let blend_keys: Vec<u32> = frame_buckets
                .iter()
                .filter(|bucket| {
                    crate::bucket_material(&self.global_gpu_data.executor_shaders, **bucket).blend
                })
                .map(|bucket| (bucket.shader_index << 16) | (bucket.pso_bin & 0xFFFF))
                .collect();
            let blend_group_count = (blend_keys.len() as u32).max(1);
            let needs_lists = self.views[self.active_view.index()]
                .visibility_view
                .as_ref()
                .is_none_or(|lists| {
                    lists.capacity() < instance_capacity
                        || lists.transparent_group_capacity() < blend_group_count
                });
            if needs_lists {
                self.device.wait_idle()?;
                if let Some(mut old_lists) =
                    self.views[self.active_view.index()].visibility_view.take()
                {
                    old_lists.free_sets(&self.descriptors);
                }
                self.views[self.active_view.index()].visibility_view =
                    match crate::SceneVisibilityView::new(
                        &self.device,
                        &self.descriptors,
                        &self.scene_visibility,
                        instance_capacity,
                        crate::SCENE_VISIBILITY_RECORD_CAPACITY,
                        blend_group_count,
                    ) {
                        Ok(lists) => Some(lists),
                        Err(err) => {
                            tracing::error!("visibility lists rebuild: {err}");
                            None
                        }
                    };
            }
            // The reach view runs whenever the distance field does — it is the field's occluder
            // set that it culls for — and holds one record slot, because a demand-only walk
            // emits none.
            let gi_reach_active = self.global_sdf.enabled() || self.ddgi.enabled();
            if gi_reach_active
                && self
                    .gi_view
                    .as_ref()
                    .is_none_or(|view| view.capacity() < instance_capacity)
            {
                self.device.wait_idle()?;
                if let Some(mut old) = self.gi_view.take() {
                    old.free_sets(&self.descriptors);
                }
                self.gi_view = match crate::SceneVisibilityView::new(
                    &self.device,
                    &self.descriptors,
                    &self.scene_visibility,
                    instance_capacity,
                    1,
                    1,
                ) {
                    Ok(view) => Some(view),
                    Err(err) => {
                        tracing::error!("gi reach view rebuild: {err}");
                        None
                    }
                };
            }
            // A view that did not run reports zeros rather than the last frame it did: a stale
            // count reads as live telemetry, with nothing in the number itself to say otherwise.
            self.gi_visibility_counters = match self.gi_view.as_ref() {
                Some(view) if gi_reach_active => view.read_counters(frame),
                _ => [0; crate::SCENE_VISIBILITY_COUNTER_WORDS as usize],
            };
            if let Some(pyramid) = self.views[self.active_view.index()].hzb_pyramid.as_mut() {
                pyramid.begin_frame();
            }
            if let Some(lists) = self.views[self.active_view.index()]
                .visibility_view
                .as_ref()
            {
                // The slot's fence completed before this frame reused it, so its
                // readback words are last use's final counters.
                self.visibility_counters = lists.read_counters(frame);
                // The scatter's meta words ride the same fence: culled is a sound claim
                // about reach, dropped is geometry lost to capacity.
                // SAFETY: HOST_VISIBLE + MAPPED, zeroed at construction; the slot's
                // fence completed before reuse.
                let meta = unsafe {
                    self.sdf_meta_readback
                        .mapped_ptr()
                        .add(frame * SDF_META_SLOT_BYTES as usize)
                        .cast::<[u32; 4]>()
                        .read_unaligned()
                };
                self.sdf_instances_culled = meta[1];
                self.sdf_instances_dropped = meta[2];
                self.wind_interaction_resets =
                    self.wind_interaction_resets.saturating_add(u64::from(
                        self.visibility_counters[crate::SCENE_VISIBILITY_COUNTER_INTERACTION_RESET],
                    ));
                // The GPU decides the frame's draws; the stats mirror the slot's last-use
                // readback: emitted records, visible instances, and rasterized triangles.
                self.stats.draw_calls =
                    self.visibility_counters[crate::SCENE_VISIBILITY_COUNTER_RECORDS];
                self.stats.instances =
                    self.visibility_counters[crate::SCENE_VISIBILITY_COUNTER_VISIBLE];
                self.stats.triangles =
                    self.visibility_counters[crate::SCENE_VISIBILITY_COUNTER_TRIANGLES];
            }
            let view_index = self.active_view.index();
            let (previous_view, previous_image, previous_layout, previous_valid) = {
                let pyramid = self.views[view_index]
                    .hzb_pyramid
                    .as_ref()
                    .expect("pyramid checked above");
                let (image, view) = pyramid.previous();
                (
                    view,
                    image,
                    pyramid.previous_layout(),
                    pyramid.previous_valid(),
                )
            };
            visibility_history_valid =
                previous_valid && self.views[view_index].prev_view_proj_valid;
            if let Some(lists) = self.views[view_index].visibility_view.as_ref() {
                let hzb_pyramid = self.views[view_index]
                    .hzb_pyramid
                    .as_ref()
                    .expect("pyramid checked above");
                let address_slice = (
                    self.gpu_scene_uploader.address_buffer(),
                    frame as u64 * self.gpu_scene_uploader.address_block_stride(),
                    size_of::<crate::GpuSceneAddressBlock>() as u64,
                );
                lists.write_frame_bindings(
                    &self.device,
                    &self.scene_visibility,
                    frame,
                    previous_view,
                    hzb_pyramid.current().1,
                    address_slice,
                );
                // The executor vertex path indexes the record stream through the
                // instance set (binding 4).
                self.descriptors.write_storage_buffer(
                    self.instancing.instance_set(frame),
                    4,
                    lists.records(frame),
                    u64::from(lists.record_capacity()) * size_of::<crate::GpuDrawRecord>() as u64,
                );
                // Binding 5 is the same command arena the indexed draws consume as arguments —
                // the binner's bucket slices and the sorted transparent slices both — which the
                // mesh executor reads as data to recover its draw from `SV_DrawIndex`. Bound
                // unconditionally: an unwritten binding is a validation error.
                self.descriptors.write_storage_buffer(
                    self.instancing.instance_set(frame),
                    5,
                    lists.commands(frame),
                    u64::from(lists.command_slots()) * 20,
                );
                // The kernels look records up in the bucket table, so it publishes
                // before the binning passes execute.
                lists.write_bucket_table(frame, &bucket_table);
                executor_buckets = frame_buckets;
                executor_inputs = Some(lists.executor_draw_inputs(
                    frame,
                    self.live_draw_record_bound,
                    displaced.map_or(vk::Buffer::null(), |arena| arena.indices),
                ));
                let camera_view = self.ssao.view();
                let camera_proj = self.ssao.inv_projection().inverse();
                let view_proj = (camera_proj * camera_view).to_cols_array();
                let prev_view_proj = if visibility_history_valid {
                    self.views[view_index].prev_view_proj.to_cols_array()
                } else {
                    view_proj
                };
                let previous_res = graph.import_image(
                    previous_image,
                    previous_view,
                    vk::ImageAspectFlags::COLOR,
                    previous_layout,
                    None,
                );
                // The interaction-field step runs first (scroll reset, impulse splat, damped
                // integration), then the wind deformation prepass samples it into the sway records
                // the cull and every raster pass read.
                if let Some(interact_pso) = &psos.wind_interact {
                    let wind = self.lighting.wind_deform_push();
                    lists.add_wind_interact_pass(
                        &self.device,
                        &mut *graph,
                        interact_pso,
                        frame,
                        frame_res.interaction_field,
                        crate::WindInteractPush {
                            field: frame_res.interaction_address,
                            impulses: frame_res.impulse_address,
                            center0: self.interaction_centers[0],
                            center1: self.interaction_centers[1],
                            impulse_count: frame_res.impulse_count,
                            dt: (wind.time_current - wind.time_previous).max(0.0),
                            reserved: [0; 2],
                        },
                    );
                }
                if let Some(wind_pso) = &psos.wind_deform {
                    lists.add_wind_deform_pass(
                        &self.device,
                        &mut *graph,
                        wind_pso,
                        frame,
                        frame_res.records,
                        frame_res.interaction_field,
                        instance_capacity,
                        crate::WindDeformPush {
                            prev_center0: self.interaction_centers_previous[0],
                            prev_center1: self.interaction_centers_previous[1],
                            ..self.lighting.wind_deform_push()
                        },
                    );
                }
                lists.add_cull_pass(
                    &self.device,
                    &mut *graph,
                    cull_pso,
                    frame,
                    previous_res,
                    frame_res.records,
                    instance_capacity,
                    crate::SceneVisibilityPush {
                        view_proj,
                        prev_view_proj,
                        hzb_extent: [hzb_pyramid.extent().width, hzb_pyramid.extent().height],
                        hzb_mip_count: hzb_pyramid.mip_count(),
                        pass_kind: crate::SCENE_VISIBILITY_PASS_CULL,
                        history_valid: u32::from(visibility_history_valid),
                        list_capacity: lists.capacity(),
                        reserved: [0; 2],
                        reach_min: [0.0; 4],
                        reach_max: [0.0; 4],
                    },
                );
                visibility_active = true;
                // The reach view, beside the camera's: the same instance sweep classified against
                // the window a march can read rather than the frustum and depth pyramid, then
                // walked for page demand alone. Its list is the distance field's occluder set.
                if let (true, Some(gi_view), Some(cull_pso), Some(traversal_pso)) = (
                    gi_reach_active,
                    self.gi_view.as_ref(),
                    psos.visibility.0.as_ref(),
                    psos.visibility.1.as_ref(),
                ) {
                    let demand = self.page_demand_view();
                    let tuning = self.traversal_tuning(crate::SceneViewClass::Gi);
                    gi_view.write_frame_bindings(
                        &self.device,
                        &self.scene_visibility,
                        frame,
                        previous_view,
                        hzb_pyramid.current().1,
                        address_slice,
                    );
                    gi_view.add_cull_pass(
                        &self.device,
                        &mut *graph,
                        cull_pso,
                        frame,
                        previous_res,
                        frame_res.records,
                        instance_capacity,
                        crate::SceneVisibilityPush {
                            view_proj,
                            prev_view_proj: view_proj,
                            hzb_extent: [hzb_pyramid.extent().width, hzb_pyramid.extent().height],
                            hzb_mip_count: hzb_pyramid.mip_count(),
                            pass_kind: crate::SCENE_VISIBILITY_PASS_REACH,
                            // The reach pass reads neither the pyramid nor the history
                            // words, so there is no previous frame for it to trust.
                            history_valid: 0,
                            list_capacity: gi_view.capacity(),
                            reserved: [0; 2],
                            reach_min: demand.gi_min.extend(0.0).to_array(),
                            reach_max: demand.gi_max.extend(0.0).to_array(),
                        },
                    );
                    gi_view.add_traversal_pass(
                        &self.device,
                        &mut *graph,
                        traversal_pso,
                        frame,
                        crate::SceneTraversalPush {
                            view_proj,
                            eye: demand.eye.to_array(),
                            proj_scale: demand.proj_scale,
                            error_threshold_px: tuning.error_threshold_px,
                            record_capacity: gi_view.record_capacity(),
                            list_capacity: gi_view.capacity(),
                            survivor: 0,
                            displaced_records: 0,
                            // Settled cuts only: a reach walk that touched the shared flip-state
                            // table would fabricate camera-view crossfades from its own decisions.
                            transition_frames: 0,
                            frame_stamp: self.frame_serial as u32,
                            representation_override: tuning.representation_override,
                            // The node cull is a frustum test and the reach view has no frustum;
                            // rejecting on one would drop the occluders it exists to keep.
                            node_cull: 0,
                            demand_only: 1,
                            view_class: crate::SceneViewClass::Gi.ordinal(),
                        },
                    );
                    gi_view.add_counters_readback_pass(&self.device, &mut *graph, frame);
                    // The occluder scatter: the reach view's visible list becomes this frame's SDF
                    // occluder region, on device. The GDF cull and the DDGI near-field march read
                    // the region through per-slot descriptor slices, so their passes declare reads
                    // on the same imported buffers and the graph orders them after this write.
                    if let Some(scatter_pso) = &psos.gi_scatter {
                        self.global_sdf.write_scatter_inputs(
                            frame,
                            (
                                gi_view.counters(frame),
                                crate::SCENE_VISIBILITY_COUNTER_WORDS * 4,
                            ),
                            (gi_view.visible(frame), u64::from(gi_view.capacity()) * 4),
                            address_slice,
                        );
                        let instances_res = graph.import_buffer(self.sdf_instances.handle(), None);
                        let meta_res = graph.import_buffer(self.sdf_meta.handle(), None);
                        let counters_res = graph.import_buffer(gi_view.counters(frame), None);
                        let raw_scatter = self.device.raw().clone();
                        let pipeline = Arc::clone(scatter_pso);
                        let set = self.global_sdf.scatter_set(frame);
                        let push = crate::GiOccluderScatterPush {
                            reach_min: demand.gi_min.extend(0.0).to_array(),
                            reach_max: demand.gi_max.extend(0.0).to_array(),
                            capacity: MAX_SDF_INSTANCES,
                            list_capacity: gi_view.capacity(),
                            reserved: [0; 2],
                        };
                        let groups = gi_view.capacity().max(1).div_ceil(64);
                        graph.add_pass(
                            RgPass::compute("gi-occluder-scatter")
                                .access(counters_res, RgUsage::StorageReadCompute)
                                .access(instances_res, RgUsage::StorageWriteCompute)
                                .access(meta_res, RgUsage::StorageReadWriteCompute)
                                .body(move |cmd, _scopes| {
                                    // SAFETY: the ash seam. PSO/set valid this frame;
                                    // the push spans the declared range.
                                    unsafe {
                                        raw_scatter.cmd_bind_pipeline(
                                            cmd,
                                            vk::PipelineBindPoint::COMPUTE,
                                            pipeline.handle(),
                                        );
                                        raw_scatter.cmd_bind_descriptor_sets(
                                            cmd,
                                            vk::PipelineBindPoint::COMPUTE,
                                            pipeline.layout(),
                                            0,
                                            &[set],
                                            &[],
                                        );
                                        raw_scatter.cmd_push_constants(
                                            cmd,
                                            pipeline.layout(),
                                            vk::ShaderStageFlags::COMPUTE,
                                            0,
                                            bytemuck::bytes_of(&push),
                                        );
                                        raw_scatter.cmd_dispatch(cmd, groups, 1, 1);
                                    }
                                }),
                        );
                    }
                    // The slab occluders for the resident micro fields, appended into the same
                    // region through the same meta counter. A reconstructed blade has no CPU
                    // instance for the reach walk to classify, so its aggregate is derived from
                    // the tile directory instead — one occluder per resident tile.
                    if let (Some(micro_pso), Some((directory_offset, directory_count))) =
                        (&psos.gi_micro, self.micro_field_directory)
                    {
                        let instances_res = graph.import_buffer(self.sdf_instances.handle(), None);
                        let meta_res = graph.import_buffer(self.sdf_meta.handle(), None);
                        let raw_micro = self.device.raw().clone();
                        let pipeline = Arc::clone(micro_pso);
                        let set = self.global_sdf.scatter_set(frame);
                        let push = self.gi_occluder_micro_push(
                            (demand.gi_min, demand.gi_max),
                            directory_offset,
                            directory_count,
                        );
                        let groups = directory_count
                            .clamp(1, crate::SCENE_MICRO_DIRECTORY_CAPACITY)
                            .div_ceil(64);
                        graph.add_pass(
                            RgPass::compute("gi-occluder-micro")
                                .access(instances_res, RgUsage::StorageWriteCompute)
                                .access(meta_res, RgUsage::StorageReadWriteCompute)
                                .body(move |cmd, _scopes| {
                                    // SAFETY: the ash seam. PSO/set valid this frame;
                                    // the push spans the declared range.
                                    unsafe {
                                        raw_micro.cmd_bind_pipeline(
                                            cmd,
                                            vk::PipelineBindPoint::COMPUTE,
                                            pipeline.handle(),
                                        );
                                        raw_micro.cmd_bind_descriptor_sets(
                                            cmd,
                                            vk::PipelineBindPoint::COMPUTE,
                                            pipeline.layout(),
                                            0,
                                            &[set],
                                            &[],
                                        );
                                        raw_micro.cmd_push_constants(
                                            cmd,
                                            pipeline.layout(),
                                            vk::ShaderStageFlags::COMPUTE,
                                            0,
                                            bytemuck::bytes_of(&push),
                                        );
                                        raw_micro.cmd_dispatch(cmd, groups, 1, 1);
                                    }
                                }),
                        );
                    }
                }
                // Stages 2-3: traverse the culled instances into the record stream, then bin into
                // indirect commands — the provisional cut the raster passes consume.
                if let Some(traversal_pso) = &psos.visibility.1 {
                    let demand = self.page_demand_view();
                    let camera_tuning = self.traversal_tuning(crate::SceneViewClass::Camera);
                    lists.add_traversal_pass(
                        &self.device,
                        &mut *graph,
                        traversal_pso,
                        frame,
                        crate::SceneTraversalPush {
                            view_proj,
                            eye: demand.eye.to_array(),
                            proj_scale: demand.proj_scale,
                            error_threshold_px: camera_tuning.error_threshold_px,
                            record_capacity: lists.record_capacity(),
                            list_capacity: lists.capacity(),
                            survivor: 0,
                            displaced_records: 1,
                            transition_frames: crate::GPU_TRANSITION_FRAMES,
                            frame_stamp: self.frame_serial as u32,
                            representation_override: camera_tuning.representation_override,
                            node_cull: u32::from(self.node_cull),
                            demand_only: 0,
                            view_class: crate::SceneViewClass::Camera.ordinal(),
                        },
                    );
                    // Micro-field reconstruction appends blade records to the same
                    // stream before binning; the binning re-reads the total.
                    if let (
                        Some((directory_offset, directory_count)),
                        (Some(micro_count), Some(micro_scan), Some(micro_scatter)),
                    ) = (
                        self.micro_field_directory,
                        (
                            psos.micro_field.0.as_ref(),
                            psos.micro_field.1.as_ref(),
                            psos.micro_field.2.as_ref(),
                        ),
                    ) {
                        lists.add_micro_field_passes(
                            &self.device,
                            &mut *graph,
                            (micro_count, micro_scan, micro_scatter),
                            frame,
                            self.global_gpu_data.micro_candidates.handle(),
                            frame_res.interaction_field,
                            {
                                let wind = self.lighting.wind_deform_push();
                                crate::SceneMicroFieldPush {
                                    view_proj,
                                    eye: demand.eye.to_array(),
                                    max_distance: 96.0,
                                    directory_offset,
                                    directory_count,
                                    record_capacity: lists.record_capacity(),
                                    candidate_capacity: crate::SCENE_MICRO_CANDIDATE_CAPACITY,
                                    frame_base: frame as u32
                                        * crate::SCENE_MICRO_CANDIDATE_CAPACITY,
                                    reserved: [0; 3],
                                    wind_dir_speed_gust: wind.dir_speed_gust,
                                    wind_params: wind.params,
                                    wind_octaves: wind.octaves,
                                    wind_seed: wind.seed,
                                    wind_time_current: wind.time_current,
                                    wind_time_previous: wind.time_previous,
                                    wind_sources: wind.sources,
                                    wind_source_count: wind.source_count,
                                    wind_reserved: 0,
                                }
                            },
                        );
                    }
                }
                if let (Some(bin_count), Some(bin_scan), Some(bin_scatter)) =
                    (&psos.visibility.2, &psos.visibility.3, &psos.visibility.4)
                {
                    lists.add_binning_passes(
                        &self.device,
                        &mut *graph,
                        (bin_count, bin_scan, bin_scatter),
                        frame,
                        false,
                        micro_template,
                        displaced,
                    );
                }
                // The transparent back-to-front sort follows the binning: per blend bucket, the
                // sorted command slice the scene pass's translucent scope draws.
                if let (Some(keys), Some(histogram), Some(scan), Some(scatter), Some(reorder)) = (
                    &psos.transparent_sort.0,
                    &psos.transparent_sort.1,
                    &psos.transparent_sort.2,
                    &psos.transparent_sort.3,
                    &psos.transparent_sort.4,
                ) && !blend_keys.is_empty()
                {
                    let camera_view = self.ssao.view();
                    let row2 = camera_view.row(2);
                    lists.add_transparent_sort_passes(
                        &self.device,
                        &mut *graph,
                        crate::TransparentSortPipelines {
                            keys,
                            histogram,
                            scan,
                            scatter,
                            reorder,
                        },
                        frame,
                        [row2.x, row2.y, row2.z, row2.w],
                        &blend_keys,
                    );
                }
            }
        }
        Ok(VisibilityFrame {
            active: visibility_active,
            history_valid: visibility_history_valid,
            buckets: executor_buckets,
            inputs: executor_inputs,
        })
    }
}
