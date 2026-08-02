use super::*;

impl Renderer {
    /// Pass order (the `beginFrameGraph` slice this phase fills): `light-cull` (compute) → the
    /// virtual-shadow page passes → optional `depth-prepass` → `scene`. The graph derives every
    /// barrier from the declared usage; the atlas's cross-frame layout rides its external slot.
    pub(super) fn record_scene_graph(
        &mut self,
        frame: usize,
        pipelines: FramePipelines,
    ) -> Result<RecordedSceneGraph> {
        // The interaction field re-centres on the camera, and the centres are what the wind
        // prepass compares to find the instances a scroll reset. Advanced here rather than at the
        // interaction pass, which is conditional and would skip the frames it does not run.
        let eye = self.page_demand_view().eye;
        self.interaction_centers_previous = self.interaction_centers;
        self.interaction_centers = [0u32, 1u32].map(|cascade| {
            let texel = 0.25_f32 * (1u32 << (2 * cascade)) as f32;
            [
                (eye.x / texel).floor() as i32,
                (eye.z / texel).floor() as i32,
            ]
        });
        // CPU span over this frame's render-graph construction, closed just before
        // `execute-render-graph` opens — a top-level sibling of it. A no-op when profiling is off.
        let profile_cpu = self.gpu_profiler.mode != ProfilerMode::Off;
        let build_span = if profile_cpu {
            let pending = std::mem::take(&mut self.pending_cpu_spans);
            let CpuProfiler { registry, buffers } = &mut self.cpu_profiler;
            // Spans measured outside this crate land here, in the slot they belong to, so a
            // capture shows them beside the passes rather than as a gap.
            for (name, start_ns, duration_ns) in pending {
                let index = buffers[frame].begin_span(registry, &name, start_ns);
                buffers[frame].end_span(index, start_ns.saturating_add(duration_ns));
            }
            Some(buffers[frame].begin_span(registry, "build-frame-graph", cpu_now_ns()))
        } else {
            None
        };
        let view = &self.views[self.active_view.index()];
        // The scene / sky / depth-prepass rasterise at INPUT extent (into the input scratch +
        // input depth); the resolve reconstructs them up to the display-extent offscreen.
        let extent = view.scaled_render_extent();
        let color_image = view.offscreen.handle();
        let color_view = view.offscreen.view();
        let offscreen_state = view.offscreen.graph_state();
        let depth_image = view.depth.handle();
        let depth_view = view.depth.view();

        let bindless_set = self.descriptors.bindless_set();
        let light_set = self.lighting.light_set(frame);
        let instance_set = self.instancing.instance_set(frame);
        let ibl_set = self.scene_ibl().set(frame);
        let raw = self.device.raw().clone();
        // The mesh fragments read the packed material-parameter blocks from the global arena the
        // mirror uploads into; the arena's buffer changes on growth, so the binding rewrites here.
        self.descriptors.write_storage_buffer(
            self.instancing.instance_set(frame),
            2,
            self.global_gpu_data.material_parameters.buffer(),
            vk::WHOLE_SIZE,
        );

        let mut graph = RenderGraph::new();
        // Page residency runs before the transfer drain: the slot's fence has completed,
        // so its GPU missing-page requests are readable, and any ready payload publishes
        // into this frame's pending queue (parent-before-child inside publish_ready).
        self.page_residency.begin_frame();
        {
            let demanded = self.vsm_demand.drain(frame);
            if !demanded.is_empty() {
                self.vsm_demanded = demanded;
            }
        }
        let requests = self.gpu_scene_uploader.drain_page_requests(frame);
        for (slot, class) in requests.requests {
            self.page_faults += 1;
            self.page_residency
                .demand_slot(slot, class.page_demand_priority());
        }
        self.page_residency
            .note_dropped_requests(requests.dropped, requests.overflow_classes);
        self.page_residency.publish_ready(
            &mut self.global_gpu_data,
            &mut self.pending_gpu_scene_uploads,
        )?;
        // The GPU-scene transfer passes lead the frame: pending resident-record stages,
        // retirements, and arena bytes from the asset mirror, then the persistent scene's
        // coalesced slot writes, all before any pass that could consume the tables.
        crate::gpu_scene_upload::record_pending_global_uploads(
            &mut self.pending_gpu_scene_uploads,
            &self.device,
            &mut graph,
            &mut self.global_gpu_data,
            frame,
        )?;
        // The scatter's meta slice clears every frame whether or not the scatter runs:
        // a frame with no reach view reads zero occluders rather than a stale slot.
        {
            let meta_res = graph.import_buffer(self.sdf_meta.handle(), None);
            let raw_clear = self.device.raw().clone();
            let meta = self.sdf_meta.handle();
            let offset = frame as u64 * SDF_META_SLOT_BYTES;
            graph.add_pass(
                RgPass::compute("sdf-meta-clear")
                    .access(meta_res, RgUsage::TransferWrite)
                    .body(move |cmd, _scopes| {
                        // SAFETY: the ash seam; the buffer is TRANSFER_DST.
                        unsafe {
                            raw_clear.cmd_fill_buffer(cmd, meta, offset, SDF_META_SLOT_BYTES, 0);
                        }
                    }),
            );
        }
        self.last_gpu_scene_upload = self.gpu_scene_uploader.record_frame(
            &self.device,
            &mut graph,
            &mut self.global_gpu_data,
            &mut self.persistent_gpu_scene,
            frame,
        )?;
        // The frame's instance-upload traffic is the GPU-scene table bytes staged this
        // frame — (near-)zero on a steady scene, the O(changes) guarantee.
        self.stats.instance_upload_bytes = self.last_gpu_scene_upload.table_bytes;
        // Adaptive-tessellation prep: the amplification arena is built before the frame's GPU-scene
        // address block is published, because the block carries the arena's addresses and the
        // traversal + binner resolve displaced records through them.
        let tess_rt_res = self.record_tess_prep(&mut graph, frame, &raw);
        let displaced = self.displaced_frame;
        let frame_res = self.record_wind_frame_resources(&mut graph, frame)?;

        // The shared blade-template words every binning scatter needs: index count + first index
        // within the pages arena (u32 units).
        let micro_template = (
            crate::MICRO_BLADE_INDEX_COUNT,
            self.global_gpu_data.micro_blade_template.first / 4,
        );
        let micro_field = (
            self.pipelines
                .request_scene_micro_count(self.scene_visibility.micro_layout()),
            self.pipelines
                .request_scene_micro_scan(self.scene_visibility.micro_layout()),
            self.pipelines
                .request_scene_micro_scatter(self.scene_visibility.micro_layout()),
        );
        let wind_deform = self
            .pipelines
            .request_wind_deform(self.scene_visibility.layout());
        let wind_interact = self
            .pipelines
            .request_wind_interact(self.scene_visibility.layout());
        let vsm_demand_pso = self
            .pipelines
            .request_vsm_demand(self.vsm_demand.mark_layout());
        let vsm_compact_pso = self
            .pipelines
            .request_vsm_demand_compact(self.vsm_demand.compact_layout());
        let visibility_psos = (
            self.pipelines
                .request_scene_visibility(self.scene_visibility.layout()),
            self.pipelines
                .request_scene_traversal(self.scene_visibility.traversal_layout()),
            self.pipelines
                .request_scene_bin_count(self.scene_visibility.bin_count_layout()),
            self.pipelines
                .request_scene_bin_seed(self.scene_visibility.bin_seed_layout()),
            self.pipelines
                .request_scene_bin_scatter(self.scene_visibility.bin_scatter_layout()),
        );
        let gi_scatter = self
            .pipelines
            .request_gi_occluder_scatter(self.global_sdf.scatter_layout());
        let gi_micro = self
            .pipelines
            .request_gi_occluder_micro(self.global_sdf.scatter_layout());
        let micro_rt = self
            .pipelines
            .request_micro_rt_deform(self.scene_visibility.micro_layout());
        let transparent_sort = (
            self.pipelines
                .request_transparent_keys(self.scene_visibility.transparent_keys_layout()),
            self.pipelines
                .request_radix_histogram(self.scene_visibility.radix_histogram_layout()),
            self.pipelines
                .request_radix_scan(self.scene_visibility.radix_scan_layout()),
            self.pipelines
                .request_radix_scatter(self.scene_visibility.radix_scatter_layout()),
            self.pipelines
                .request_transparent_reorder(self.scene_visibility.transparent_reorder_layout()),
        );
        let rt_deform = self
            .pipelines
            .request_rt_deform(self.scene_visibility.layout());
        let psos = SceneFramePsos {
            visibility: visibility_psos,
            micro_field,
            transparent_sort,
            wind_deform,
            wind_interact,
            rt_deform,
            gi_scatter,
            gi_micro,
            micro_rt,
        };
        let visibility =
            self.record_visibility_passes(&mut graph, frame, &frame_res, &psos, micro_template)?;
        let visibility_active = visibility.active;
        let visibility_history_valid = visibility.history_valid;
        let executor_buckets = visibility.buckets;
        let executor_inputs = visibility.inputs;
        // Resolve each frame bucket's executor mesh PSO for the pass bodies; this borrow of
        // `self.pipelines` must not overlap the visibility block's lists. Cloned once per frame so
        // the bodies outlive it; `None` whenever the mesh executor is off or unsupported.
        let scene_mesh_dispatch: Option<ash::ext::mesh_shader::Device> = self
            .mesh_executor
            .then(|| self.device.mesh_shader_dispatch().cloned())
            .flatten();
        let survivor_mesh_dispatch = scene_mesh_dispatch.clone();
        let executor_draws: Vec<(crate::ExecutorBucket, bool, Arc<crate::Pipeline>)> =
            executor_buckets
                .iter()
                .filter_map(|bucket| {
                    let material =
                        crate::bucket_material(&self.global_gpu_data.executor_shaders, *bucket);
                    self.pipelines
                        .request_executor_mesh_pipeline(
                            &material,
                            self.wireframe,
                            self.mesh_executor,
                        )
                        .map(|pso| (*bucket, material.blend, pso))
                })
                .collect();
        self.stats.batches = executor_draws.len() as u32;
        self.capture_selection_source(&executor_draws, executor_inputs, instance_set, extent);
        // Fold the executor buckets into the shadow draw-call stat: one recorded
        // counted-indirect draw per non-blend bucket per shadow pass this frame.
        let non_blend_buckets = executor_draws
            .iter()
            .filter(|(_, blend, _)| !*blend)
            .count() as u32;
        let shadow_passes = u32::try_from(self.vsm_render_pages.len()).unwrap_or(u32::MAX);
        self.stats.shadow_draw_calls = shadow_passes.saturating_mul(non_blend_buckets);
        let ibl_live = self.scene_ibl_mut().add_live_capture_passes(&mut graph);
        let ddgi_sh = if self.active_view == ViewId::Thumbnail {
            graph.import_buffer(self.ibl.sh_coefficients().handle(), None)
        } else {
            ibl_live.sh
        };

        // Light-cull (compute): cull the punctual lights into the froxel grid. The graph emits the
        // compute→fragment barrier on the cluster buffer from the declared StorageWriteCompute.
        if let Some(cull) = &pipelines.cull {
            let cluster_buffer = graph.import_buffer(self.lighting.cluster_buffer(frame), None);
            let cull_set = self.lighting.cluster_set(frame);
            let cull = Arc::clone(cull);
            let cull_pipeline = cull.handle();
            let cull_layout = cull.layout();
            let raw_body = raw.clone();
            let groups = crate::lighting::CLUSTER_COUNT.div_ceil(64);
            let pass = RgPass::compute("light-cull")
                .access(cluster_buffer, RgUsage::StorageWriteCompute)
                .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                    // SAFETY: the ash seam. The PSO/set are valid this frame; the dispatch
                    // covers the froxel grid (one invocation per cluster, 64 per group).
                    unsafe {
                        raw_body.cmd_bind_pipeline(
                            cmd,
                            vk::PipelineBindPoint::COMPUTE,
                            cull_pipeline,
                        );
                        raw_body.cmd_bind_descriptor_sets(
                            cmd,
                            vk::PipelineBindPoint::COMPUTE,
                            cull_layout,
                            0,
                            &[cull_set],
                            &[],
                        );
                        raw_body.cmd_dispatch(cmd, groups, 1, 1);
                    }
                    drop(cull);
                });
            graph.add_pass(pass);
        }

        // Compute skinning pre-pass: deform each skinned mesh-instance once into the frame's
        // deformed + prev-deformed buffers, before every geometry pass reads it as a static vertex
        // stream. The graph derives the compute-write → vertex-input barrier from the usages.
        let do_skin = pipelines.skin.is_some()
            && !self.frame_deformation.skin_dispatches.is_empty()
            && self.skinning.deformed_buffer(frame).is_some()
            && self.skinning.prev_deformed_buffer(frame).is_some();
        let do_morph = pipelines.morph.is_some()
            && !self.frame_deformation.morph_dispatches.is_empty()
            && self.skinning.deformed_buffer(frame).is_some()
            && self.skinning.prev_deformed_buffer(frame).is_some();
        // Ray-geometry materialization writes the same arena, so it joins the deform scope even
        // when nothing skins or morphs: a windy plant with no skinned instance in the scene is the
        // common case, and its structure is the only consumer of the slice.
        let do_rt_deform = psos.rt_deform.is_some() && !self.rt_deform_jobs.is_empty();
        let do_deform = do_skin || do_morph || do_rt_deform;
        let deformed_handle = if do_deform {
            self.skinning.deformed_buffer(frame)
        } else {
            None
        };
        // The prev-deformed buffer carries the previous pose for the motion pass. Both the
        // morph and skin passes write it (each deforms its prev-pose slice), so it is
        // imported once for the whole deform scope and shared between them.
        let prev_deformed_handle = if do_deform {
            self.skinning.prev_deformed_buffer(frame)
        } else {
            None
        };
        // The executor vertex paths read the micro-blade candidates through their
        // device address; every raster pass declares the read so the graph orders it
        // after the micro pass's compute write.
        let micro_candidates_res =
            graph.import_buffer(self.global_gpu_data.micro_candidates.handle(), None);
        let (deformed_res, prev_deformed_res) = if do_deform {
            let deformed = graph.import_buffer(deformed_handle.expect("deformed buffer"), None);
            let prev_deformed =
                graph.import_buffer(prev_deformed_handle.expect("prev-deformed buffer"), None);

            // Morph pre-pass: scatter each active blend-shape's sparse deltas into the deformed and
            // prev-deformed buffers, then resolve to positions/normals — before skin and before any
            // geometry pass reads the stream. It writes the same buffers, so the graph orders both.
            if do_morph {
                let morph = pipelines.morph.as_ref().expect("morph PSO");
                let morph = Arc::clone(morph);
                let morph_handle = morph.handle();
                let morph_layout = morph.layout();
                let raw_morph = raw.clone();
                let morph_list = self.frame_deformation.shallow_clone();
                let pass = RgPass::compute("morph")
                    .access(deformed, RgUsage::StorageWriteCompute)
                    .access(prev_deformed, RgUsage::StorageWriteCompute)
                    .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                        crate::skinning::record_morph(
                            &raw_morph,
                            cmd,
                            morph_handle,
                            morph_layout,
                            &morph_list.morph_dispatches,
                            &morph_list.prev_morph_dispatches,
                        );
                        drop(morph);
                    });
                graph.add_pass(pass);
            }

            if do_skin {
                let skin = pipelines.skin.as_ref().expect("skin PSO");
                let skin = Arc::clone(skin);
                let skin_handle = skin.handle();
                let skin_layout = skin.layout();
                let raw_body = raw.clone();
                let list = self.frame_deformation.shallow_clone();
                // Both deformed buffers are written this pass, so the graph emits a compute-write
                // barrier for each before the consumers read them.
                let pass = RgPass::compute("skin")
                    .access(deformed, RgUsage::StorageWriteCompute)
                    .access(prev_deformed, RgUsage::StorageWriteCompute)
                    .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                        crate::skinning::Skinning::record_skin(
                            &raw_body,
                            cmd,
                            skin_handle,
                            skin_layout,
                            &list.skin_dispatches,
                            &list.prev_skin_dispatches,
                        );
                        drop(skin);
                    });
                graph.add_pass(pass);
            }

            if do_rt_deform {
                let rt_deform = Arc::clone(psos.rt_deform.as_ref().expect("rt-deform PSO"));
                let raw_body = raw.clone();
                let jobs = self.rt_deform_jobs.clone();
                let set = self.views[self.active_view.index()]
                    .visibility_view
                    .as_ref()
                    .map_or(vk::DescriptorSet::null(), |view| view.cull_set(frame));
                if set != vk::DescriptorSet::null() {
                    graph.add_pass(
                        RgPass::compute("rt-deform")
                            .access(deformed, RgUsage::StorageWriteCompute)
                            // The displacement comes from the wind prepass's records, so this
                            // declares the read that orders it after them. Without it the graph is
                            // free to schedule the materialization first and build structures from
                            // last frame's sway.
                            .access(frame_res.records, RgUsage::ShaderDeviceAddressRead)
                            .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                                crate::record_rt_deform(&raw_body, cmd, &rt_deform, set, &jobs);
                            }),
                    );
                }
            }

            (Some(deformed), Some(prev_deformed))
        } else {
            (None, None)
        };

        // The reconstructed micro blades' ray geometry, materialized into its own transient arena
        // before the structures that build from it.
        let micro_rt_res = self.record_micro_rt_prep(
            &mut graph,
            frame,
            psos.micro_rt.as_ref(),
            frame_res.interaction_field,
        );

        // RT: build the per-frame TLAS over the scene's mesh instances (a compute-kind pass; the
        // recorded plan self-manages the AS-build → fragment ray-query barrier). Skinned instances
        // refit a per-slot BLAS from the deformed buffer first, so the pass declares an
        // `AccelStructBuildRead` on it and the graph orders this after `skin`.
        self.rt.reset_frame_ready();
        let deformed_rt = self.frame_deformation.deformed_rt_instances.clone();
        let has_skinned_rt = !deformed_rt.is_empty();
        // Representation selection projects through the camera view's own cut parameters,
        // so the ray representation swaps to the aggregate exactly where the raster
        // traversal draws it.
        let rt_cut_view = self.rt_cut_view();
        let micro_rt_tiles = std::mem::take(&mut self.micro_rt_tiles);
        if self.rt.build_pending()
            && self.rt.has_instances(&deformed_rt, &micro_rt_tiles)
            && let Some(plan) = self.rt.prepare_tlas_build(
                &self.device,
                frame,
                &deformed_rt,
                deformed_handle,
                &micro_rt_tiles,
                rt_cut_view,
            )
        {
            let raw_body = raw.clone();
            let mut tlas_pass = RgPass::compute("tlas-build").body(
                move |_cmd, scopes: &mut NestedScopeRecorder| {
                    crate::record_tlas_build_plan(&raw_body, &plan, scopes);
                },
            );
            // Declare the deformed-buffer read so the graph orders this after the skin
            // pass (the skinned BLAS refit reads the freshly deformed vertices).
            if has_skinned_rt && let Some(deformed) = deformed_res {
                tlas_pass = tlas_pass.access(deformed, RgUsage::AccelStructBuildRead);
            }
            // The tessellated BLAS builds over the coarse VB/IB the tess-emit-rt pass wrote;
            // declaring the read orders this pass after that emit.
            if let Some((vb_rt, ib_rt)) = tess_rt_res {
                tlas_pass = tlas_pass
                    .access(vb_rt, RgUsage::AccelStructBuildRead)
                    .access(ib_rt, RgUsage::AccelStructBuildRead);
            }
            // The materialized micro blades' structures build over the arena the
            // `micro-rt-deform` pass wrote, for the same reason.
            if let Some((vb_micro, ib_micro)) = micro_rt_res {
                tlas_pass = tlas_pass
                    .access(vb_micro, RgUsage::AccelStructBuildRead)
                    .access(ib_micro, RgUsage::AccelStructBuildRead);
            }
            graph.add_pass(tlas_pass);
        }
        self.micro_rt_tiles = micro_rt_tiles;

        // Virtual-shadow pages: each dirty page rasterizes into its atlas tile behind its space's
        // own cull/bin chain; the scene pass then declares the atlas `SampledRead`.
        let mut vsm_atlas_res: Option<RgResource> = None;
        if let (
            Some(shadow),
            Some(cull_pso),
            Some(traversal_pso),
            Some(bin_count),
            Some(bin_seed),
            Some(bin_scatter),
        ) = (
            &pipelines.shadow,
            &psos.visibility.0,
            &psos.visibility.1,
            &psos.visibility.2,
            &psos.visibility.3,
            &psos.visibility.4,
        ) {
            let vsm_capacity = frame_res.address_block.instance_capacity.max(1);
            vsm_atlas_res = self.add_vsm_page_passes(
                &mut graph,
                frame,
                shadow,
                bindless_set,
                instance_set,
                deformed_res,
                &executor_draws,
                frame_res.records,
                cull_pso,
                traversal_pso,
                (bin_count, bin_seed, bin_scatter),
                micro_template,
                vsm_capacity,
            )?;
        }

        // The offscreen color + scene depth are always imported (the present blit samples the
        // offscreen; the post-tonemap overlay reads the depth). The AA mode selects where the scene
        // renders its result and what the scene pass attaches. The offscreen contents are
        // regenerated every frame, so it enters UNDEFINED — but its exit layout must be tracked, so
        // an external slot carries the resolved layout back into `view.offscreen.layout`.
        let offscreen_slot = graph.alloc_external_state(offscreen_state);
        let color = graph.import_image(
            color_image,
            color_view,
            vk::ImageAspectFlags::COLOR,
            vk::ImageLayout::UNDEFINED,
            Some(offscreen_slot),
        );
        let depth = graph.import_image(
            depth_image,
            depth_view,
            vk::ImageAspectFlags::DEPTH,
            vk::ImageLayout::UNDEFINED,
            None,
        );

        let (msaa, fxaa, taa) = {
            let view = &self.views[self.active_view.index()];
            (
                self.aa.msaa() && view.msaa_color.is_some() && view.msaa_depth.is_some(),
                pipelines.fxaa.is_some() && view.scratch.is_some(),
                pipelines.taa.is_some() && view.scratch.is_some(),
            )
        };

        // The scene always renders its result into the INPUT-extent scratch; the resolve stage
        // reconstructs scratch → the display-extent offscreen. `scratch` is unconditionally
        // allocated, so there is one scene→resolve→offscreen path with no `scale == 1` fork.
        let scene_output = {
            let view = &self.views[self.active_view.index()];
            let scratch = view.scratch.as_ref().expect("scratch built");
            graph.import_image(
                scratch.handle(),
                scratch.view(),
                vk::ImageAspectFlags::COLOR,
                vk::ImageLayout::UNDEFINED,
                None,
            )
        };
        // The scene pass attaches the multisampled color (resolving into scene_output) when
        // MSAA is on, else scene_output directly.
        let scene_color_attachment = if msaa {
            let view = &self.views[self.active_view.index()];
            let msaa_color = view.msaa_color.as_ref().expect("msaa color built");
            graph.import_image(
                msaa_color.handle(),
                msaa_color.view(),
                vk::ImageAspectFlags::COLOR,
                vk::ImageLayout::UNDEFINED,
                None,
            )
        } else {
            scene_output
        };
        // The scene depth attaches the multisampled depth (resolving into the 1× depth) when
        // MSAA is on, else the 1× depth directly.
        let scene_depth = if msaa {
            let view = &self.views[self.active_view.index()];
            let msaa_depth = view.msaa_depth.as_ref().expect("msaa depth built");
            graph.import_image(
                msaa_depth.handle(),
                msaa_depth.view(),
                vk::ImageAspectFlags::DEPTH,
                vk::ImageLayout::UNDEFINED,
                None,
            )
        } else {
            depth
        };

        // The motion-vector prepass: reproject this frame's camera against last frame's into the
        // motion target. Runs before the screen-space accumulation and before the scene, so the
        // TAA resolve reads it; the graph derives ColorWrite → SampledReadCompute.
        let (motion_resource, motion_depth_resource) = match self.add_motion_pass(
            &mut graph,
            &pipelines,
            bindless_set,
            instance_set,
            (deformed_res, deformed_handle),
            (prev_deformed_res, prev_deformed_handle),
            executor_inputs,
            &executor_draws,
        ) {
            Some((motion, depth)) => (Some(motion), Some(depth)),
            None => (None, None),
        };

        // Resolve weather and fill the one camera-snapped cloud-shadow cascade array before any
        // mesh, cloud, or froxel consumer reads it.
        let cloud_frame = self.prepare_cloud_frame(&mut graph, &pipelines, frame);

        // Global SDF: the cull + composite passes that bin the per-mesh MDF bricks into the
        // camera-centered cascade clipmap. Runs first, before the DDGI trace and the scene both
        // sample the cascades, so the composite writes are visible before any read.
        let gdf = self.add_gdf_passes(&mut graph, &pipelines, frame);

        // Fill this frame's gi-resolve params UBO and rewrite the shared IBL/DDGI bindings into
        // this frame's set slot; the slot's prior use is fenced, so there is no in-flight hazard.
        if pipelines.gi_resolve.is_some() {
            let inv_view = self.ssao.view().inverse();
            let (vol_min, vol_ext) = self.ddgi.volume();
            let gi_params = crate::ssao::GiParams {
                inv_projection: self.ssao.inv_projection(),
                inv_view,
                volume_min: vol_min.extend(0.0),
                volume_extent: vol_ext.extend(0.0),
                probe_count: self.ddgi.probe_count_ubo(),
                scroll_base: self.ddgi.scroll_base_ubo(),
                eye_position: inv_view.w_axis,
                flags: saffron_geometry::glam::UVec4::new(
                    u32::from(self.ddgi.enabled()),
                    u32::from(pipelines.dfao.is_some()),
                    0,
                    0,
                ),
            };
            let sky_sh = self.scene_ibl().sh_coefficients();
            let sky_sh_handle = sky_sh.handle();
            let sky_sh_size = sky_sh.size();
            let ddgi_irr = self.ddgi.irradiance().1;
            let ddgi_dist = self.ddgi.distance().1;
            let ddgi_sampler = self.ddgi.sampler();
            let active = self.active_view.index();
            if let Some(ubo) = self.views[active].gi_params_ubos.get_mut(frame)
                && let Some(dst) = ubo.mapped_bytes()
            {
                let src = bytemuck::bytes_of(&gi_params);
                dst[..src.len()].copy_from_slice(src);
            }
            self.views[active].write_gi_resolve_shared(
                &self.device,
                frame,
                sky_sh_handle,
                sky_sh_size,
                ddgi_irr,
                ddgi_dist,
                ddgi_sampler,
            );
        }

        let screen = self.add_screen_space_passes(
            &mut graph,
            &pipelines,
            bindless_set,
            instance_set,
            motion_resource,
            (deformed_res, deformed_handle),
            light_set,
            gdf.cascades,
            gdf.occupancy,
            ibl_live.sh,
            executor_inputs,
            &executor_draws,
        );

        // ReSTIR DI: the three-pass reservoir chain (initial candidates → temporal + spatial reuse
        // → resolve with one TLAS visibility ray), writing the per-pixel direct radiance the scene
        // samples via set 7. Runs after the G-buffer prepass and the TLAS build, before the scene.
        let restir = self.add_restir_passes(&mut graph, &pipelines, frame, motion_resource);

        // DDGI: the four-pass GI chain updating the irradiance + distance atlases the mesh samples
        // via set 5. The trace sphere-marches the shared distance field and reads the lite albedo
        // cache, so it runs after the GDF composite, whose cascades and albedo are its inputs.
        let ddgi = self.add_ddgi_passes(
            &mut graph,
            &pipelines,
            bindless_set,
            light_set,
            &gdf,
            ddgi_sh,
        );

        // Visible sky: a fullscreen pass that fills the scene color target before the
        // geometry. It writes the SAME target the scene pass uses, owns the color clear when
        // present, and the scene pass then loads instead of clearing.
        let did_sky = self.sky.should_draw();
        if did_sky {
            let bindless = bindless_set;
            let raw_for_body = raw.clone();
            // The offscreen thumbnail view draws a fixed studio gradient instead of the scene's sky,
            // so the backdrop never samples the IBL cube and a subject always has silhouette
            // contrast. Interactive views keep their submitted sky.
            let sky_mode_override = (self.active_view == ViewId::Thumbnail)
                .then_some(crate::ibl::SKY_MODE_THUMBNAIL_GRADIENT);
            let draw = self
                .sky
                .draw_data(self.frame_deformation.view_proj, sky_mode_override);
            // The sky clears + STORES the (multisampled, under MSAA) scene color; the scene pass
            // then LOADs it and owns the single MSAA resolve. Resolving or discarding here would
            // leave the scene pass loading undefined MSAA color.
            let mut color_att = RgAttachment::clear_store(scene_color_attachment);
            color_att.clear_value = vk::ClearValue {
                color: vk::ClearColorValue {
                    float32: [
                        self.sky.clear_color.x,
                        self.sky.clear_color.y,
                        self.sky.clear_color.z,
                        1.0,
                    ],
                },
            };
            let sky_pass = RgPass::graphics("sky", extent).color(color_att).body(
                move |cmd, _scopes: &mut NestedScopeRecorder| {
                    crate::ibl::record_sky(&raw_for_body, cmd, bindless, &draw);
                },
            );
            graph.add_pass(sky_pass);

            if self.active_view != ViewId::Thumbnail && self.sky.night().star_intensity > 0.0 {
                let draw = self.stars.draw_data(
                    self.frame_deformation.view_proj,
                    extent,
                    self.sky.night(),
                );
                let raw_for_body = raw.clone();
                let stars_pass = RgPass::graphics("stars", extent)
                    .color(color_load_store(scene_color_attachment))
                    .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                        crate::record_stars(&raw_for_body, cmd, &draw);
                    });
                graph.add_pass(stars_pass);
            }
        }

        let did_depth_prepass = pipelines.depth_prepass.is_some() && executor_inputs.is_some();
        if did_depth_prepass {
            let pipeline = pipelines
                .depth_prepass
                .as_ref()
                .expect("prepass PSO gated above");
            let inputs = executor_inputs.expect("executor inputs gated above");
            let raw_for_body = raw.clone();
            let pipeline = Arc::clone(pipeline);
            let prepass_handle = pipeline.handle();
            let prepass_layout = pipeline.layout();
            let bindless = bindless_set;
            let draws = executor_draws.clone();
            let push = self.frame_deformation.view_proj;
            let pages_buffer = self.global_gpu_data.pages.buffer();
            let draw_count_supported = self.device.capabilities.draw_indirect_count;
            let pages_res = graph.import_buffer(pages_buffer, None);
            let commands_res = graph.import_buffer(inputs.commands, None);
            let counters_res = graph.import_buffer(inputs.counters, None);
            let bucket_counts_res = graph.import_buffer(inputs.bucket_counts, None);
            // The depth pre-pass writes the (multisampled, when MSAA) scene depth the scene
            // pass then loads — the same sample count the scene PSO bakes. Draws come from
            // the frame's binned executor commands over the pages-arena index stream.
            let mut depth_pass = RgPass::graphics("depth-prepass", extent)
                .depth_attachment(depth_clear_store(scene_depth))
                .access(pages_res, RgUsage::IndexInputRead)
                .access(commands_res, RgUsage::IndirectCommandRead)
                .access(counters_res, RgUsage::IndirectCountRead)
                .access(bucket_counts_res, RgUsage::IndirectCountRead);
            depth_pass = access_displaced_arena(&mut graph, depth_pass, displaced, false);
            let mut depth_pass = depth_pass.body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                record_executor_depth_family(
                    &raw_for_body,
                    cmd,
                    (prepass_handle, prepass_layout),
                    vk::ShaderStageFlags::VERTEX,
                    bytemuck::bytes_of(&push),
                    bindless,
                    instance_set,
                    inputs,
                    pages_buffer,
                    draw_count_supported,
                    &draws,
                    false,
                );
                drop(pipeline);
            });
            depth_pass = depth_pass.access(micro_candidates_res, RgUsage::ShaderDeviceAddressRead);
            depth_pass = depth_pass.access(frame_res.records, RgUsage::ShaderDeviceAddressRead);
            if let Some(deformed) = deformed_res {
                depth_pass = depth_pass.access(deformed, RgUsage::ShaderDeviceAddressRead);
            }
            graph.add_pass(depth_pass);
        }

        // The scene pass: clear or (after a depth pre-pass) load the depth, clear the color, replay
        // the executor draws then the submit-seam closures. The frame's `live_textures` stay
        // pinned on `self.frame_deformation` until the next frame's fence is waited.
        let list = self.frame_deformation.shallow_clone();
        let submissions = std::mem::take(&mut self.submissions);
        let raw_for_body = raw.clone();
        let clear_color = self.clear_color;

        // The survivor raster redraws the retest survivors over the provisional scene with both
        // attachments LOADed. Whether it runs decides the scene pass's MSAA store ops: the
        // multisampled samples must survive to it, and it then owns the final resolve.
        let hzb_copy_pso = self.pipelines.request_hzb_copy(self.hzb.copy_layout());
        let hzb_reduce_pso = self.pipelines.request_hzb_reduce(self.hzb.reduce_layout());
        let survivor_planned = visibility_active
            && hzb_copy_pso.is_some()
            && hzb_reduce_pso.is_some()
            && psos.visibility.0.is_some()
            && psos.visibility.1.is_some()
            && psos.visibility.2.is_some()
            && psos.visibility.3.is_some()
            && psos.visibility.4.is_some()
            && executor_inputs.is_some();

        let mut color_att = RgAttachment::clear_store(scene_color_attachment);
        color_att.clear_value = vk::ClearValue {
            color: vk::ClearColorValue {
                float32: clear_color,
            },
        };
        // The sky pass owns the color clear when it ran; the scene then loads it.
        // Otherwise the scene clears the color itself.
        if did_sky {
            color_att.load_op = vk::AttachmentLoadOp::LOAD;
        }
        // MSAA: render to the multisampled color and resolve into scene_output, unless the survivor
        // raster still draws over those samples. The graph's `resolve` is the MSAA resolve.
        if msaa {
            color_att.store_op = if survivor_planned {
                vk::AttachmentStoreOp::STORE
            } else {
                vk::AttachmentStoreOp::DONT_CARE
            };
            color_att.resolve = Some(scene_output);
        }
        let mut depth_att = depth_clear_store(scene_depth);
        if did_depth_prepass {
            depth_att.load_op = vk::AttachmentLoadOp::LOAD;
        }
        // Persist the 1× scene depth for the post-tonemap overlay: store it directly (no
        // MSAA), or resolve the multisampled depth into the 1× target (MSAA samples
        // kept only for the survivor raster).
        if msaa {
            depth_att.store_op = if survivor_planned {
                vk::AttachmentStoreOp::STORE
            } else {
                vk::AttachmentStoreOp::DONT_CARE
            };
            depth_att.resolve = Some(depth);
        }
        let ssao_mesh_set = screen.mesh_set;
        // Set 5 (DDGI) — bound whenever the sub-state is built, because the mesh PSO statically
        // references it; the mesh gates the sample on the DDGI flag.
        let ddgi_mesh_set = if self.ddgi.ready {
            self.ddgi.mesh_set()
        } else {
            vk::DescriptorSet::null()
        };
        // Set 6 (the TLAS) — present only on an RT device (`null` otherwise, the scene pass
        // then skips the bind). The mesh fragment gates the ray-query trace on `rtShadows`.
        let rt_mesh_set = self.rt.mesh_set(frame);
        // Set 7 (the ReSTIR resolved-radiance sampler) — the mesh PSO statically references it on
        // an RT device, so it must bind whenever the view's ReSTIR scaffolding is built or the draw
        // reports set 7 unbound (`VUID-vkCmdDrawIndexed-None-08600`). `null` on a non-RT device.
        let restir_mesh_set = if restir.radiance.is_some() {
            restir.mesh_set
        } else {
            self.views[self.active_view.index()].restir.mesh_set()
        };
        // The scene pass binds the mesh roster once — constant in the draw count. Recorded here,
        // where the resolved sets are known, since the pass body's return value is discarded.
        self.stats.descriptor_binds = crate::scene_pass::scene_pass_bind_count(
            executor_inputs.is_some() && !executor_draws.is_empty(),
            rt_mesh_set,
            restir_mesh_set,
        );
        let scene_sets = crate::MeshPassSets {
            bindless: bindless_set,
            light: light_set,
            instance: instance_set,
            ibl: ibl_set,
            ssao_mesh: ssao_mesh_set,
            ddgi_mesh: ddgi_mesh_set,
            rt_mesh: rt_mesh_set,
            restir_mesh: restir_mesh_set,
        };
        let scene_view_proj = self.frame_deformation.view_proj;
        let scene_pages_buffer = self.global_gpu_data.pages.buffer();
        let scene_draw_count_supported = self.device.capabilities.draw_indirect_count;
        let scene_draws = executor_draws.clone();
        let mut scene = RgPass::graphics("scene", extent)
            .color(color_att)
            .depth_attachment(depth_att)
            .body(move |_cmd, scopes: &mut NestedScopeRecorder| {
                let _ = &list;
                scopes.scope("scene-opaque", |cmd| {
                    if let Some(inputs) = executor_inputs {
                        crate::record_executor_buckets(
                            &raw_for_body,
                            cmd,
                            scene_view_proj,
                            scene_sets,
                            inputs,
                            scene_pages_buffer,
                            scene_draw_count_supported,
                            &scene_draws,
                            false,
                            scene_mesh_dispatch.as_ref(),
                        );
                    }
                });
                scopes.scope("scene-submissions", |cmd| {
                    for body in submissions {
                        body(cmd);
                    }
                });
                // Translucent geometry composites last, over the resolved opaque scene: per blend
                // bucket, the GPU-sorted back-to-front command slice with that bucket's blend PSO
                // (depth-write off, same attachments — no separate pass).
                scopes.scope("scene-translucent", |cmd| {
                    if let Some(inputs) = executor_inputs {
                        crate::record_executor_transparent_stream(
                            &raw_for_body,
                            cmd,
                            scene_view_proj,
                            scene_sets,
                            inputs,
                            scene_pages_buffer,
                            scene_draw_count_supported,
                            &scene_draws,
                            scene_mesh_dispatch.as_ref(),
                        );
                    }
                });
            });
        scene = access_displaced_arena(&mut graph, scene, displaced, false);
        if let Some(inputs) = executor_inputs {
            let pages_res = graph.import_buffer(scene_pages_buffer, None);
            let commands_res = graph.import_buffer(inputs.commands, None);
            let counters_res = graph.import_buffer(inputs.counters, None);
            let bucket_counts_res = graph.import_buffer(inputs.bucket_counts, None);
            let mesh_args_res = graph.import_buffer(inputs.mesh_args, None);
            scene = scene
                .access(pages_res, RgUsage::IndexInputRead)
                .access(commands_res, RgUsage::IndirectCommandRead)
                .access(mesh_args_res, RgUsage::IndirectCommandRead)
                .access(counters_res, RgUsage::IndirectCountRead)
                .access(bucket_counts_res, RgUsage::IndirectCountRead);
        }
        // The scene fragment samples the AO / contact / SSGI maps via set 4; declare the reads so
        // the graph transitions each from GENERAL (compute write) to ShaderReadOnly before it.
        for resource in &screen.scene_sampled {
            scene = scene.access(*resource, RgUsage::SampledRead);
        }
        scene = scene
            .access(ibl_live.sh, RgUsage::StorageReadFragment)
            .access(ibl_live.prefiltered, RgUsage::SampledRead);
        // When DDGI ran this frame, the irradiance + distance atlases were storage-written
        // (GENERAL); declare the scene's SampledRead so the graph transitions each back to
        // ShaderReadOnly before the mesh sample (the border pass leaves irradiance GENERAL).
        if let Some(irradiance) = ddgi.irradiance {
            scene = scene.access(irradiance, RgUsage::SampledRead);
        }
        if let Some(distance) = ddgi.distance {
            scene = scene.access(distance, RgUsage::SampledRead);
        }
        // When the GDF composited this frame, the cascade volumes were storage-written; the mesh
        // fragment's reflection-occlusion cone taps them, and the übershader references the
        // samplers statically, so the transition is needed even when the runtime flag gates it off.
        if let Some(cascades) = gdf.cascades {
            for cascade in cascades {
                scene = scene.access(cascade, RgUsage::SampledRead);
            }
        }
        if let Some(occupancy) = gdf.occupancy {
            for volume in occupancy {
                scene = scene.access(volume, RgUsage::SampledRead);
            }
        }
        // When ReSTIR ran this frame, the resolve wrote the radiance image as storage
        // (GENERAL); declare the scene's SampledRead so the graph transitions it back to
        // ShaderReadOnly before the mesh sample (set 7).
        if let Some(radiance) = restir.radiance {
            scene = scene.access(radiance, RgUsage::SampledRead);
        }
        // The mesh fragment samples the virtual-shadow atlas via the light set; declare the read so
        // the graph transitions DepthWrite → ShaderReadOnly before the draw, else the sample sees a
        // DEPTH_ATTACHMENT image (`VUID-vkCmdDrawIndexed-imageLayout-00344`).
        if let Some(res) = vsm_atlas_res {
            scene = scene.access(res, RgUsage::SampledRead);
        }
        if let Some(cloud) = cloud_frame {
            scene = scene.access(cloud.shadow, RgUsage::SampledRead);
        }
        // A skinned draw pulls the deformed buffer through its device address; declare
        // the read so the graph orders the scene pass after the skin compute write.
        scene = scene.access(micro_candidates_res, RgUsage::ShaderDeviceAddressRead);
        scene = scene.access(frame_res.records, RgUsage::ShaderDeviceAddressRead);
        if let Some(deformed) = deformed_res {
            scene = scene.access(deformed, RgUsage::ShaderDeviceAddressRead);
        }
        graph.add_pass(scene);

        // The HZB build follows the scene pass: seed mip 0 from the resolved 1x depth,
        // then the per-mip max reduction. Next frame's visibility reads this pyramid as
        // its previous; a missing PSO invalidates instead so tests bypass a stale pyramid.
        if let (Some(hzb_copy), Some(hzb_reduce)) = (hzb_copy_pso, hzb_reduce_pso) {
            // Stage-5 prologue: snapshot the provisional visible/record counts into counter words
            // 6/7 and zero the bucket counts BEFORE the retest appends survivors. Every consumer of
            // the provisional cut is declared above, so the graph orders their reads first.
            if survivor_planned
                && let Some(lists) = self.views[self.active_view.index()]
                    .visibility_view
                    .as_ref()
            {
                lists.add_survivor_snapshot_pass(&self.device, &mut graph, frame);
            }
            let hzb_depth_view = self.views[self.active_view.index()].depth.view();
            let current_hzb_res = self.views[self.active_view.index()]
                .hzb_pyramid
                .as_mut()
                .map(|pyramid| {
                    pyramid.write_depth_binding(&self.device, &self.hzb, hzb_depth_view);
                    pyramid.add_build_passes(
                        &self.device,
                        &mut graph,
                        (&hzb_copy, &hzb_reduce),
                        depth,
                    )
                });
            self.add_vsm_demand_passes(
                &mut graph,
                frame,
                vsm_demand_pso.as_ref(),
                vsm_compact_pso.as_ref(),
                current_hzb_res,
            );

            // Instance visibility, stage 5: retest the occluded-established list
            // against the freshly built pyramid; survivors merge into the visible
            // list for the survivor traversal.
            if visibility_active && let Some(current_hzb_res) = current_hzb_res {
                let view_index = self.active_view.index();
                let camera_view = self.ssao.view();
                let camera_proj = self.ssao.inv_projection().inverse();
                let view_proj = (camera_proj * camera_view).to_cols_array();
                let lists = self.views[view_index]
                    .visibility_view
                    .as_ref()
                    .expect("visibility lists active");
                let hzb_pyramid = self.views[view_index]
                    .hzb_pyramid
                    .as_ref()
                    .expect("pyramid active");
                if let Some(cull_pso) = &psos.visibility.0 {
                    lists.add_retest_pass(
                        &self.device,
                        &mut graph,
                        cull_pso,
                        frame,
                        current_hzb_res,
                        frame_res.records,
                        crate::SceneVisibilityPush {
                            view_proj,
                            prev_view_proj: view_proj,
                            hzb_extent: [hzb_pyramid.extent().width, hzb_pyramid.extent().height],
                            hzb_mip_count: hzb_pyramid.mip_count(),
                            pass_kind: crate::SCENE_VISIBILITY_PASS_RETEST,
                            history_valid: u32::from(visibility_history_valid),
                            list_capacity: lists.capacity(),
                            reserved: [0; 2],
                            reach_min: [0.0; 4],
                            reach_max: [0.0; 4],
                        },
                    );
                }
                // Stage 6: traverse the retest survivors into records past counter word 7, re-bin
                // them into the re-seeded command slices, redraw them over the provisional scene
                // with both attachments LOADed, then rebuild the HZB for next frame.
                if survivor_planned {
                    if let Some(traversal_pso) = &psos.visibility.1 {
                        let demand = self.page_demand_view();
                        let survivor_tuning = self.traversal_tuning(crate::SceneViewClass::Camera);
                        lists.add_traversal_pass(
                            &self.device,
                            &mut graph,
                            traversal_pso,
                            frame,
                            crate::SceneTraversalPush {
                                view_proj,
                                eye: demand.eye.to_array(),
                                proj_scale: demand.proj_scale,
                                error_threshold_px: survivor_tuning.error_threshold_px,
                                record_capacity: lists.record_capacity(),
                                list_capacity: lists.capacity(),
                                survivor: 1,
                                displaced_records: 1,
                                transition_frames: crate::GPU_TRANSITION_FRAMES,
                                frame_stamp: self.frame_serial as u32,
                                representation_override: survivor_tuning.representation_override,
                                node_cull: u32::from(self.node_cull),
                                demand_only: 0,
                                view_class: crate::SceneViewClass::Camera.ordinal(),
                            },
                        );
                    }
                    if let (Some(bin_count), Some(bin_scan), Some(bin_scatter)) =
                        (&psos.visibility.2, &psos.visibility.3, &psos.visibility.4)
                    {
                        lists.add_binning_passes(
                            &self.device,
                            &mut graph,
                            (bin_count, bin_scan, bin_scatter),
                            frame,
                            true,
                            micro_template,
                            displaced,
                        );
                    }
                    if let Some(inputs) = executor_inputs {
                        let raw_for_body = raw.clone();
                        let survivor_draws = executor_draws.clone();
                        let survivor_pages = self.global_gpu_data.pages.buffer();
                        let survivor_count = self.device.capabilities.draw_indirect_count;
                        let survivor_view_proj = self.frame_deformation.view_proj;
                        let mut survivor_color = color_load_store(scene_color_attachment);
                        let mut survivor_depth = depth_load_store(scene_depth);
                        if msaa {
                            survivor_color.store_op = vk::AttachmentStoreOp::DONT_CARE;
                            survivor_color.resolve = Some(scene_output);
                            survivor_depth.store_op = vk::AttachmentStoreOp::DONT_CARE;
                            survivor_depth.resolve = Some(depth);
                        }
                        let mut survivor_pass = RgPass::graphics("scene-survivors", extent)
                            .color(survivor_color)
                            .depth_attachment(survivor_depth)
                            .body(move |_cmd, scopes: &mut NestedScopeRecorder| {
                                scopes.scope("scene-survivors", |cmd| {
                                    crate::record_executor_buckets(
                                        &raw_for_body,
                                        cmd,
                                        survivor_view_proj,
                                        scene_sets,
                                        inputs,
                                        survivor_pages,
                                        survivor_count,
                                        &survivor_draws,
                                        false,
                                        survivor_mesh_dispatch.as_ref(),
                                    );
                                });
                            });
                        let pages_res = graph.import_buffer(survivor_pages, None);
                        let commands_res = graph.import_buffer(inputs.commands, None);
                        let mesh_args_res = graph.import_buffer(inputs.mesh_args, None);
                        let counters_res = graph.import_buffer(inputs.counters, None);
                        let bucket_counts_res = graph.import_buffer(inputs.bucket_counts, None);
                        survivor_pass = survivor_pass
                            .access(pages_res, RgUsage::IndexInputRead)
                            .access(commands_res, RgUsage::IndirectCommandRead)
                            .access(mesh_args_res, RgUsage::IndirectCommandRead)
                            .access(counters_res, RgUsage::IndirectCountRead)
                            .access(bucket_counts_res, RgUsage::IndirectCountRead);
                        if let Some(deformed) = deformed_res {
                            survivor_pass =
                                survivor_pass.access(deformed, RgUsage::ShaderDeviceAddressRead);
                        }
                        survivor_pass = survivor_pass
                            .access(micro_candidates_res, RgUsage::ShaderDeviceAddressRead)
                            .access(frame_res.records, RgUsage::ShaderDeviceAddressRead);
                        survivor_pass =
                            access_displaced_arena(&mut graph, survivor_pass, displaced, false);
                        graph.add_pass(survivor_pass);
                    }
                    // Restore the complete cut for the passes declared after this
                    // block (reactive coverage, the wireframe overlay): clear the
                    // survivor-only bucket counts and re-bin every record.
                    if let (Some(bin_count), Some(bin_scan), Some(bin_scatter)) =
                        (&psos.visibility.2, &psos.visibility.3, &psos.visibility.4)
                    {
                        lists.add_bucket_count_clear_pass(&self.device, &mut graph, frame);
                        lists.add_binning_passes(
                            &self.device,
                            &mut graph,
                            (bin_count, bin_scan, bin_scatter),
                            frame,
                            false,
                            micro_template,
                            displaced,
                        );
                    }
                }
                lists.add_counters_readback_pass(&self.device, &mut graph, frame);
            }
            self.add_sdf_meta_readback_pass(&mut graph, frame);
            // The final HZB rebuild reads the survivor-updated depth over the same
            // imported pyramid resource, publishing the complete cut as next frame's
            // previous pyramid.
            if survivor_planned
                && let Some(current_hzb_res) = current_hzb_res
                && let Some(pyramid) = self.views[self.active_view.index()].hzb_pyramid.as_mut()
            {
                pyramid.add_rebuild_passes(
                    &self.device,
                    &mut graph,
                    (&hzb_copy, &hzb_reduce),
                    depth,
                    current_hzb_res,
                );
            }
        } else if let Some(pyramid) = self.views[self.active_view.index()].hzb_pyramid.as_mut() {
            pyramid.invalidate();
        }

        // FXAA: edge-blur the scene scratch into the offscreen, then TAA: reproject history through
        // the motion vector + blend with the current scene into the offscreen + next-frame history.
        // Mutually exclusive (only one PSO is resolved); both run after the scene pass.
        self.add_fxaa_pass(&mut graph, &pipelines, scene_output, color);
        // The reactive-coverage pass (TAA only) marks translucent geometry into the input-extent
        // reactive mask, depth-tested against the scene depth; the TAA resolve then biases those
        // pixels toward the current frame.
        let reactive_resource = self.add_reactive_coverage_pass(
            &mut graph,
            &pipelines,
            scene_depth,
            bindless_set,
            instance_set,
            executor_inputs,
            &executor_draws,
        );
        let taa_slots = self.add_taa_pass(
            &mut graph,
            &pipelines,
            scene_output,
            color,
            motion_resource,
            motion_depth_resource,
            reactive_resource,
        );
        // No-AA / MSAA: neither resolve above wrote the offscreen, so upscale the input scene
        // scratch into the display offscreen (one path — at 1:1 it degenerates to a straight copy).
        if !fxaa && !taa {
            self.add_scene_resolve_pass(&mut graph, &pipelines, scene_output, color);
        }

        // SSGI history capture: copy the scene's resolved linear-HDR color into prevColor before any
        // tonemap turns it display-referred. A barrier-only restore pass then declares a compute
        // SampledRead so the graph returns prevColor to its resting layout.
        if let Some(copy) = screen.history_copy {
            let raw_body = raw.clone();
            let handle = copy.pipeline.handle();
            let layout = copy.pipeline.layout();
            let set = copy.set;
            let groups_x = copy.groups_x;
            let groups_y = copy.groups_y;
            let pipeline = copy.pipeline;
            let copy_pass = RgPass::compute("ssgi-history")
                .access(color, RgUsage::SampledReadCompute)
                .access(copy.prev_color, RgUsage::StorageImageRwCompute)
                .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                    // SAFETY: the ash seam. The PSO/set are valid this frame; the dispatch
                    // covers the viewport (8×8 per group).
                    unsafe {
                        raw_body.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, handle);
                        raw_body.cmd_bind_descriptor_sets(
                            cmd,
                            vk::PipelineBindPoint::COMPUTE,
                            layout,
                            0,
                            &[set],
                            &[],
                        );
                        raw_body.cmd_dispatch(cmd, groups_x, groups_y, 1);
                    }
                    drop(pipeline);
                });
            graph.add_pass(copy_pass);
            // Barrier-only: General → ShaderReadOnly for next frame's SSGI sample + seed.
            let restore = RgPass::compute("ssgi-history-restore")
                .access(copy.prev_color, RgUsage::SampledReadCompute)
                .body(|_cmd, _scopes: &mut NestedScopeRecorder| {});
            graph.add_pass(restore);
        }

        // Froxel volumetric fog: inject the shadowed per-froxel in-scatter + extinction over the
        // clustered light list, then front-to-back energy-conserving integration into the volume the
        // composite samples. Runs only in `fog.mode == volumetric` (the PSOs are resolved then).
        let froxel_slots = self.add_froxel_fog_passes(
            &mut graph,
            &pipelines,
            cloud_frame.map(|resources| resources.shadow),
            frame,
        );

        // Aerial perspective: ray-march the atmosphere LUTs into the 32³ AP volume the composite
        // folds onto the shared ledger. Independent of the fog inject/integrate, scheduled here so
        // it completes before the composite reads it.
        let aerial_slot = self.add_aerial_perspective_pass(&mut graph, &pipelines);

        // Clouds first produce full-resolution premultiplied scatter/transmittance + front depth.
        // The CloudDensity view remains an isolated unlit visualizer that overwrites color directly.
        let cloud_slots = self.add_cloud_passes(
            &mut graph,
            &pipelines,
            CloudGraphInputs {
                color,
                depth,
                motion: motion_resource,
                sky_sh: ddgi_sh,
            },
            cloud_frame,
        );

        // Height fog is the sole fog/AP/cloud composite. It folds the already-upscaled cloud tuple
        // onto the shared transmittance ledger before bloom.
        self.add_fog_pass(&mut graph, &pipelines, color, depth, &cloud_slots, frame);

        // Scene-linear bloom: an energy-conserving mip pyramid composited into `color` while it is
        // still unbounded HDR radiance, immediately before the tonemap so the view transform rolls
        // the bloomed highlights off for free. The mip chain comes from the transient pool.
        if pipelines.bloom.is_some() {
            let mips = self.acquire_bloom_mips(&mut graph, frame);
            if !mips.is_empty() {
                let streak = self.acquire_bloom_streak(&mut graph, frame, mips[0].extent);
                self.add_bloom_pass(
                    &mut graph, &pipelines, color, color_view, frame, &mips, &streak,
                );
            }
        }

        // Write this frame's scene-linear grade into the view's grade UBO slice; the tonemap pass
        // binds it by the matching dynamic offset (a neutral grade is a mathematical identity).
        // binds it by the matching dynamic offset (neutral grade → mathematical identity).
        let grade = GradeUniform::from(&self.color_grade).with_look(
            self.creative_lut_intensity,
            self.creative_lut_size,
            false,
        );
        self.views[self.active_view.index()].write_grade(frame, &grade);
        self.add_tonemap_pass(&mut graph, &pipelines, color, frame);
        // The overlays draw at display extent, so they depth-test the display-extent overlay
        // depth — a point-upscale of the input scene depth. When the upscale PSO / target is
        // unavailable, fall back to the input depth (valid at render scale 1, where they match).
        let overlay_depth = self
            .add_depth_upscale_pass(&mut graph, &pipelines, depth)
            .unwrap_or(depth);
        // View-mode overlays on the post-tonemap color: the motion-vector visualization
        // overwrites it; the Lit Wireframe overlay draws edges over it. Both no-op unless
        // their mode is active (the PSO is `None` otherwise).
        self.add_motion_visualize_pass(&mut graph, &pipelines, color, motion_resource);
        self.add_lit_wireframe_pass(
            &mut graph,
            &pipelines,
            color,
            overlay_depth,
            bindless_set,
            instance_set,
            deformed_res,
            executor_inputs,
            &executor_draws,
        );
        self.add_grid_overlay_passes(&mut graph, &pipelines, color, overlay_depth);

        let plan = graph.submission_plan(self.device.render_graph_queue_families());
        self.stats.async_compute_batches = plan.compute_batch_count() as u32;
        let commands = self.frames.prepare_graph_commands(
            &self.device,
            plan.graphics_batch_count(),
            plan.compute_batch_count(),
        )?;

        // Arm the per-frame GPU timestamp recorder (a no-op when the profiler is `Off`): each pass
        // body is then bracketed by a timestamp scope written into this slot's pool. The recorder
        // is stashed back into the profiler for read-back `MAX_FRAMES_IN_FLIGHT` frames later.
        let mut recorder = self.gpu_profiler.frame_recorder(frame);
        // The graph is fully constructed; close the `build-frame-graph` span before the
        // `execute-render-graph` span opens (siblings at top level).
        if let Some(index) = build_span {
            let CpuProfiler { buffers, .. } = &mut self.cpu_profiler;
            buffers[frame].end_span(index, cpu_now_ns());
        }

        // Arm the CPU span recorder on the same gate as the GPU one, so the merged capture carries
        // both lanes. `cpu_profiler` and `gpu_profiler` are distinct fields, so the two recorder
        // borrows are disjoint.
        let profile_cpu = self.gpu_profiler.mode != ProfilerMode::Off;
        let CpuProfiler {
            registry: cpu_registry,
            buffers: cpu_buffers,
        } = &mut self.cpu_profiler;
        let cpu_buffer = &mut cpu_buffers[frame];
        let exec_span = if profile_cpu {
            Some(cpu_buffer.begin_span(cpu_registry, "execute-render-graph", cpu_now_ns()))
        } else {
            None
        };
        let recorded_batches = {
            // Scope the recorders so their `&mut` borrows of `recorder` / `cpu_*` release
            // before `cpu_buffer.end_span` re-borrows the buffer and `recorder` is stashed.
            let mut recorders = crate::render_graph::ProfileRecorders {
                gpu: recorder.armed().then_some(&mut recorder),
                cpu: profile_cpu.then_some((&mut *cpu_registry, &mut *cpu_buffer)),
            };
            graph.record_submission_plan_profiled(
                &self.device,
                plan,
                RgBatchCommandBuffers {
                    graphics: &commands.graphics,
                    compute: &commands.compute,
                },
                &mut recorders,
            )?
        };
        if let Some(index) = exec_span {
            cpu_buffer.end_span(index, cpu_now_ns());
        }
        self.gpu_profiler.stash_recorder(frame, recorder);
        self.transient.resolve_buffer_states(&graph);

        // Track the offscreen color's resolved exit layout so the shm read-back's entry barrier
        // uses the right `old_layout` — a stale tracked layout mis-transitions the image and the
        // next submit flags a mismatch (`VUID-vkCmdDraw-None-09600`).
        self.views[self.active_view.index()]
            .offscreen
            .set_graph_state(graph.external_state(offscreen_slot));

        self.scene_ibl_mut().resolve_live_layouts(&graph, ibl_live);
        self.resolve_frame_volume_layouts(
            &graph,
            &ddgi,
            &gdf,
            froxel_slots,
            aerial_slot,
            &cloud_slots,
        );

        // Read back the ReSTIR radiance image's resolved exit layout (it rode an external
        // slot, ending ShaderReadOnly after the scene's SampledRead). The per-view temporal
        // state was already advanced inside `add_restir_passes`, before execute.
        if let Some(slot) = restir.radiance_slot {
            let layout = graph.external_state(slot).layout;
            self.views[self.active_view.index()]
                .restir
                .set_radiance_layout(layout);
        }

        // Read back the temporal images' resolved exit layouts (the cross-frame
        // ShaderReadOnly ↔ General transition is derived from these slots). The TAA history
        // pair + the SSGI history pair + the ssgi_resolved each rode an external slot.
        let temporal_ran = taa_slots.is_some()
            || screen.ssgi_history_slots.is_some()
            || screen.dfao_history_slots.is_some()
            || cloud_slots.temporal;
        // Store the UN-jittered matrix as this view's previous frame (the motion prepass needs a
        // jitter-free previous camera).
        let frame_view_proj = self.scene_view_proj_unjittered();
        let taa_active = self.aa.taa();
        let view = &mut self.views[self.active_view.index()];
        if let Some(slots) = &taa_slots {
            writeback_history_layout(view, &graph, &slots.history.read);
            writeback_history_layout(view, &graph, &slots.history.write);
            writeback_lock_layout(view, &graph, &slots.lock.read);
            writeback_lock_layout(view, &graph, &slots.lock.write);
        }
        if let Some(slots) = &screen.ssgi_history_slots {
            writeback_ssgi_history_layout(view, &graph, &slots.read);
            writeback_ssgi_history_layout(view, &graph, &slots.write);
        }
        if let (Some(slot), Some(resolved)) =
            (screen.ssgi_resolved_slot, view.ssgi_resolved.as_mut())
        {
            resolved.set_graph_state(graph.external_state(slot));
        }
        if let Some(slots) = &screen.dfao_history_slots {
            writeback_dfao_history_layout(view, &graph, &slots.read);
            writeback_dfao_history_layout(view, &graph, &slots.write);
        }
        if let (Some(slot), Some(resolved)) =
            (screen.dfao_resolved_slot, view.dfao_resolved.as_mut())
        {
            resolved.set_graph_state(graph.external_state(slot));
        }
        if let (Some(slot), Some(ssr_map)) = (screen.ssr_map_slot, view.ssr_map.as_mut()) {
            ssr_map.set_graph_state(graph.external_state(slot));
        }

        // TAA and/or SSGI accumulation consumed this frame's history parity; flip the shared
        // ping-pong index once so next frame reprojects through the buffer just written.
        if temporal_ran {
            view.flip_history();
        }
        // Record this frame's camera viewProj as this view's previous frame for next
        // frame's motion reprojection (per-view: a re-activated view reprojects against its
        // own last frame).
        view.store_prev_view_proj(frame_view_proj);
        // Advance the Halton jitter cycle for next frame — only while TAA is active, so an
        // off/FXAA/MSAA frame renders un-jittered (`jitter` stays zero, the single gate).
        if taa_active || cloud_slots.temporal {
            view.advance_jitter();
        }
        Ok(RecordedSceneGraph {
            batches: recorded_batches,
            tail: commands.tail,
        })
    }
}
