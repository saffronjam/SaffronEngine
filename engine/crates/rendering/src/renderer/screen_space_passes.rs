use super::*;

impl Renderer {
    /// Builds the thin G-buffer prepass + the screen-space compute chain (gtao → ao-blur,
    /// contact, ssgi → ssgi-blur → ssgi-accum) into `graph`, importing the active view's
    /// screen-space images and binding the per-view sets. `motion` is the resource the SSGI
    /// temporal accumulation reprojects through. Returns the per-view mesh set 4, the maps the
    /// scene declares `SampledRead` on, the prev-color history-copy info the caller schedules
    /// after the scene pass, and the external-layout slots to read back after execute.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn add_screen_space_passes(
        &self,
        graph: &mut RenderGraph,
        pipelines: &FramePipelines,
        bindless_set: vk::DescriptorSet,
        instance_set: vk::DescriptorSet,
        motion: Option<RgResource>,
        deformed: (Option<RgResource>, Option<vk::Buffer>),
        light_set: vk::DescriptorSet,
        gdf_cascades: Option<[RgResource; crate::GDF_CASCADES as usize]>,
        gdf_occupancy: Option<[RgResource; crate::GDF_CASCADES as usize]>,
        sky_sh: RgResource,
        executor_inputs: Option<crate::ExecutorDrawInputs>,
        executor_draws: &[(crate::ExecutorBucket, bool, Arc<crate::Pipeline>)],
    ) -> ScreenSpaceResult {
        let (deformed_res, _deformed_handle) = deformed;
        let mut result = ScreenSpaceResult::default();
        let Some(gbuffer) = &pipelines.gbuffer else {
            // The screen-space prepass is skipped this frame, but the übershader's layout always
            // declares set 4, so it must still be bound or the draw reports set 4 unbound
            // (`VUID-vkCmdDrawIndexed-None-08600`). The per-view set 4 is written to the neutral
            // init-transitioned maps at view bring-up, and the in-shader flags gate the reads.
            let view = &self.views[self.active_view.index()];
            if view.screen_space_ready() {
                result.mesh_set = view.mesh_set;
            }
            return result;
        };
        let view = &self.views[self.active_view.index()];
        let extent = view.scaled_render_extent();
        // SSGI + GTAO trace into half-resolution targets (matching `build_screen_space`), so their
        // dispatch covers the half extent; the bilateral blur/upsample passes stay full-res.
        let half_extent = vk::Extent2D {
            width: extent.width.div_ceil(2).max(1),
            height: extent.height.div_ceil(2).max(1),
        };
        let raw = self.device.raw();
        let groups = |n: u32| n.div_ceil(8);
        result.mesh_set = view.mesh_set;

        // The G-buffer prepass: write view normal (rgb) + view-Z (.a) + roughness + its own depth.
        let g_normal = graph.import_image(
            view.g_normal.as_ref().expect("g_normal built").handle(),
            view.g_normal.as_ref().expect("g_normal built").view(),
            vk::ImageAspectFlags::COLOR,
            vk::ImageLayout::UNDEFINED,
            None,
        );
        let g_roughness = graph.import_image(
            view.g_roughness
                .as_ref()
                .expect("g_roughness built")
                .handle(),
            view.g_roughness.as_ref().expect("g_roughness built").view(),
            vk::ImageAspectFlags::COLOR,
            vk::ImageLayout::UNDEFINED,
            None,
        );
        let g_depth = graph.import_image(
            view.g_depth.as_ref().expect("g_depth built").handle(),
            view.g_depth.as_ref().expect("g_depth built").view(),
            vk::ImageAspectFlags::DEPTH,
            vk::ImageLayout::UNDEFINED,
            None,
        );
        {
            let raw_body = raw.clone();
            let push = self.ssao.gbuffer_push();
            let pipeline = Arc::clone(gbuffer);
            let gbuffer_pipeline = pipeline.handle();
            let gbuffer_layout = pipeline.layout();
            let draws = executor_draws.to_vec();
            let pages_buffer = self.global_gpu_data.pages.buffer();
            let draw_count_supported = self.device.capabilities.draw_indirect_count;
            let mut pass = RgPass::graphics("gbuffer", extent)
                .color(RgAttachment::clear_store(g_normal))
                .color(RgAttachment::clear_store(g_roughness))
                .depth_attachment(depth_clear_store(g_depth));
            pass = access_displaced_arena(graph, pass, self.displaced_frame, false);
            let mut pass = pass.body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                if let Some(inputs) = executor_inputs {
                    record_executor_depth_family(
                        &raw_body,
                        cmd,
                        (gbuffer_pipeline, gbuffer_layout),
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

        // GTAO + bilateral denoise: g_normal → ao_raw → ao_map.
        if let (Some(gtao), Some(ao_blur)) = (&pipelines.gtao, &pipelines.ao_blur) {
            let ao_raw = graph.import_image(
                view.ao_raw.as_ref().expect("ao_raw built").handle(),
                view.ao_raw.as_ref().expect("ao_raw built").view(),
                vk::ImageAspectFlags::COLOR,
                vk::ImageLayout::UNDEFINED,
                None,
            );
            let ao_map_slot = graph
                .alloc_external_state(view.ao_map.as_ref().expect("ao_map built").graph_state());
            let ao_map = graph.import_image(
                view.ao_map.as_ref().expect("ao_map built").handle(),
                view.ao_map.as_ref().expect("ao_map built").view(),
                vk::ImageAspectFlags::COLOR,
                view.ao_map.as_ref().expect("ao_map built").layout,
                Some(ao_map_slot),
            );
            self.add_compute_pass(
                graph,
                "gtao",
                gtao,
                view.gtao_set,
                &[
                    (g_normal, RgUsage::SampledReadCompute),
                    (ao_raw, RgUsage::StorageImageRwCompute),
                ],
                Some(bytemuck::bytes_of(&self.ssao.gtao_push()).to_vec()),
                groups(half_extent.width),
                groups(half_extent.height),
                1,
            );
            self.add_compute_pass(
                graph,
                "ao-blur",
                ao_blur,
                view.ao_blur_set,
                &[
                    (ao_raw, RgUsage::SampledReadCompute),
                    (g_normal, RgUsage::SampledReadCompute),
                    (ao_map, RgUsage::StorageImageRwCompute),
                ],
                None,
                groups(extent.width),
                groups(extent.height),
                1,
            );
            result.scene_sampled.push(ao_map);
        }

        // Directional contact shadows: g_normal → contact_map.
        if let Some(contact) = &pipelines.contact {
            let contact_slot = graph.alloc_external_state(
                view.contact_map
                    .as_ref()
                    .expect("contact_map built")
                    .graph_state(),
            );
            let contact_map = graph.import_image(
                view.contact_map
                    .as_ref()
                    .expect("contact_map built")
                    .handle(),
                view.contact_map.as_ref().expect("contact_map built").view(),
                vk::ImageAspectFlags::COLOR,
                view.contact_map.as_ref().expect("contact_map built").layout,
                Some(contact_slot),
            );
            self.add_compute_pass(
                graph,
                "contact-shadows",
                contact,
                view.contact_set,
                &[
                    (g_normal, RgUsage::SampledReadCompute),
                    (contact_map, RgUsage::StorageImageRwCompute),
                ],
                Some(bytemuck::bytes_of(&self.ssao.contact_push()).to_vec()),
                groups(extent.width),
                groups(extent.height),
                1,
            );
            result.scene_sampled.push(contact_map);
        }

        // SSGI, SSR, and RT reflections all gather from the previous frame's color. Import
        // prevColor once here (read now, written by the copy-color pass after the scene); it rests
        // ShaderReadOnly between frames, so the import seeds that and does not write it back.
        let rt_refl = self.rt.use_rt_reflections() && view.prev_view_proj_valid;
        let prev_color = if pipelines.ssgi.is_some() || pipelines.ssr.is_some() || rt_refl {
            Some(graph.import_image(
                view.prev_color.as_ref().expect("prev_color built").handle(),
                view.prev_color.as_ref().expect("prev_color built").view(),
                vk::ImageAspectFlags::COLOR,
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                None,
            ))
        } else {
            None
        };

        // One-bounce SSGI: g_normal + prevColor → ssgi_map → ssgi_denoised.
        if let (Some(ssgi), Some(ssgi_blur)) = (&pipelines.ssgi, &pipelines.ssgi_blur) {
            let prev_color = prev_color.expect("prev_color imported when SSGI on");
            let ssgi_slot = graph.alloc_external_state(
                view.ssgi_map
                    .as_ref()
                    .expect("ssgi_map built")
                    .graph_state(),
            );
            let ssgi_map = graph.import_image(
                view.ssgi_map.as_ref().expect("ssgi_map built").handle(),
                view.ssgi_map.as_ref().expect("ssgi_map built").view(),
                vk::ImageAspectFlags::COLOR,
                view.ssgi_map.as_ref().expect("ssgi_map built").layout,
                Some(ssgi_slot),
            );
            let denoised_slot = graph.alloc_external_state(
                view.ssgi_denoised
                    .as_ref()
                    .expect("ssgi_denoised built")
                    .graph_state(),
            );
            let ssgi_denoised = graph.import_image(
                view.ssgi_denoised
                    .as_ref()
                    .expect("ssgi_denoised built")
                    .handle(),
                view.ssgi_denoised
                    .as_ref()
                    .expect("ssgi_denoised built")
                    .view(),
                vk::ImageAspectFlags::COLOR,
                view.ssgi_denoised
                    .as_ref()
                    .expect("ssgi_denoised built")
                    .layout,
                Some(denoised_slot),
            );
            // The SSGI trace push was built (frame index bumped) at PSO-resolve time.
            self.add_compute_pass(
                graph,
                "ssgi",
                ssgi,
                view.ssgi_set,
                &[
                    (g_normal, RgUsage::SampledReadCompute),
                    (prev_color, RgUsage::SampledReadCompute),
                    (ssgi_map, RgUsage::StorageImageRwCompute),
                ],
                Some(bytemuck::bytes_of(&pipelines.ssgi_push).to_vec()),
                groups(half_extent.width),
                groups(half_extent.height),
                1,
            );
            self.add_compute_pass(
                graph,
                "ssgi-blur",
                ssgi_blur,
                view.ssgi_blur_set,
                &[
                    (ssgi_map, RgUsage::SampledReadCompute),
                    (g_normal, RgUsage::SampledReadCompute),
                    (ssgi_denoised, RgUsage::StorageImageRwCompute),
                ],
                None,
                groups(extent.width),
                groups(extent.height),
                1,
            );
            // SSGI temporal accumulation (when motion ran): reproject the SSGI history through
            // motion, neighborhood-clamp, EMA into the stable ssgi_resolved map. It runs whenever
            // SSGI + motion is on, independent of the final-image AA mode, sharing the ping-pong
            // parity flipped after the scene.
            if let (Some(accum), Some(motion)) = (&pipelines.ssgi_accum, motion) {
                let p = view.history_index;
                let ssgi_resolved_slot = graph.alloc_external_state(
                    view.ssgi_resolved
                        .as_ref()
                        .expect("ssgi_resolved built")
                        .graph_state(),
                );
                let ssgi_resolved = graph.import_image(
                    view.ssgi_resolved
                        .as_ref()
                        .expect("ssgi_resolved built")
                        .handle(),
                    view.ssgi_resolved
                        .as_ref()
                        .expect("ssgi_resolved built")
                        .view(),
                    vk::ImageAspectFlags::COLOR,
                    view.ssgi_resolved
                        .as_ref()
                        .expect("ssgi_resolved built")
                        .layout,
                    Some(ssgi_resolved_slot),
                );
                let (read_slot, read) = import_ssgi_history(graph, &view.ssgi_history[1 - p]);
                let (write_slot, write) = import_ssgi_history(graph, &view.ssgi_history[p]);
                let push = crate::SsgiAccumPush {
                    params: saffron_geometry::glam::Vec4::new(
                        crate::SSGI_HISTORY_WEIGHT,
                        if view.history_valid { 1.0 } else { 0.0 },
                        0.0,
                        0.0,
                    ),
                };
                self.add_compute_pass(
                    graph,
                    "ssgi-accum",
                    accum,
                    view.ssgi_accum_sets[p],
                    &[
                        (ssgi_denoised, RgUsage::SampledReadCompute),
                        (read, RgUsage::SampledReadCompute),
                        (motion, RgUsage::SampledReadCompute),
                        (ssgi_resolved, RgUsage::StorageImageRwCompute),
                        (write, RgUsage::StorageImageRwCompute),
                    ],
                    Some(bytemuck::bytes_of(&push).to_vec()),
                    groups(extent.width),
                    groups(extent.height),
                    1,
                );
                // The scene SampledReads the resolved map (the accum's output). The mesh
                // set-4 SSGI sampler points at it under TAA, else the denoised map.
                result.scene_sampled.push(ssgi_resolved);
                result.ssgi_resolved_slot = Some(ssgi_resolved_slot);
                result.ssgi_history_slots = Some(TaaHistorySlots {
                    read: (1 - p, read_slot),
                    write: (p, write_slot),
                });
            } else {
                // No motion this frame: the scene samples the spatially denoised map.
                result.scene_sampled.push(ssgi_denoised);
            }
        }

        // DFAO diffuse sky-visibility: the reduced-resolution GDF cone trace (Wright 2015),
        // mirroring the SSGI chain — a half-res trace, a bilateral upsample (reusing the ssgi-blur
        // PSO), then temporal accumulation through the motion vectors. The trace is a three-set
        // pass like the DDGI trace: it taps the GDF cascade clipmap via the light set, so it
        // declares SampledRead on the cascades. The chain's product is the accumulated map, which
        // the gi-resolve pass (added after this block) samples as its sky-visibility input; `None`
        // when the chain did not run, and gi-resolve's skyVis flag is 0.
        let mut dfao_sky_vis: Option<RgResource> = None;
        if let (Some(dfao), Some(motion)) = (&pipelines.dfao, motion) {
            let bindless_set = self.descriptors.bindless_set();
            let dfao_raw = graph.import_image(
                view.dfao_raw.as_ref().expect("dfao_raw built").handle(),
                view.dfao_raw.as_ref().expect("dfao_raw built").view(),
                vk::ImageAspectFlags::COLOR,
                vk::ImageLayout::UNDEFINED,
                None,
            );
            let dfao_denoised = graph.import_image(
                view.dfao_denoised
                    .as_ref()
                    .expect("dfao_denoised built")
                    .handle(),
                view.dfao_denoised
                    .as_ref()
                    .expect("dfao_denoised built")
                    .view(),
                vk::ImageAspectFlags::COLOR,
                view.dfao_denoised
                    .as_ref()
                    .expect("dfao_denoised built")
                    .layout,
                None,
            );

            // 1. Trace: reconstruct world pos/normal from the G-buffer, cone-trace the GDF sky
            //    visibility into the half-res raw map. Records its three sets directly.
            let trace = Arc::clone(&dfao.trace);
            let trace_handle = trace.handle();
            let trace_layout = trace.layout();
            let trace_set = view.dfao_set;
            let trace_push = pipelines.dfao_push;
            let raw_body = raw.clone();
            let trace_gx = groups(half_extent.width);
            let trace_gy = groups(half_extent.height);
            let mut trace_pass = RgPass::compute("dfao")
                .access(g_normal, RgUsage::SampledReadCompute)
                .access(dfao_raw, RgUsage::StorageImageRwCompute);
            // The trace taps the GDF cascade clipmap (light set binding 9); declare the reads so
            // the graph transitions each cascade from GENERAL (composite write) → ShaderReadOnly
            // before the trace, exactly as the DDGI trace does.
            if let Some(cascades) = gdf_cascades {
                for cascade in cascades {
                    trace_pass = trace_pass.access(cascade, RgUsage::SampledReadCompute);
                }
            }
            if let Some(occupancy) = gdf_occupancy {
                for volume in occupancy {
                    trace_pass = trace_pass.access(volume, RgUsage::SampledReadCompute);
                }
            }
            let trace_pass = trace_pass.body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                // SAFETY: the ash seam. The PSO + three sets are valid this frame; the dispatch
                // covers the half-res trace target.
                unsafe {
                    raw_body.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, trace_handle);
                    raw_body.cmd_bind_descriptor_sets(
                        cmd,
                        vk::PipelineBindPoint::COMPUTE,
                        trace_layout,
                        0,
                        &[bindless_set, light_set, trace_set],
                        &[],
                    );
                    raw_body.cmd_push_constants(
                        cmd,
                        trace_layout,
                        vk::ShaderStageFlags::COMPUTE,
                        0,
                        bytemuck::bytes_of(&trace_push),
                    );
                    raw_body.cmd_dispatch(cmd, trace_gx, trace_gy, 1);
                }
                drop(trace);
            });
            graph.add_pass(trace_pass);

            // 2. Bilateral upsample: dfao_raw (half-res) + g_normal → dfao_denoised (full-res).
            self.add_compute_pass(
                graph,
                "dfao-blur",
                &dfao.blur,
                view.dfao_blur_set,
                &[
                    (dfao_raw, RgUsage::SampledReadCompute),
                    (g_normal, RgUsage::SampledReadCompute),
                    (dfao_denoised, RgUsage::StorageImageRwCompute),
                ],
                None,
                groups(extent.width),
                groups(extent.height),
                1,
            );

            // 3. Temporal accumulation: reproject the DFAO history through motion and EMA it into
            //    dfao_resolved — the stable map every consumer of the term samples.
            let p = view.history_index;
            let dfao_resolved_slot = graph.alloc_external_state(
                view.dfao_resolved
                    .as_ref()
                    .expect("dfao_resolved built")
                    .graph_state(),
            );
            let dfao_resolved = graph.import_image(
                view.dfao_resolved
                    .as_ref()
                    .expect("dfao_resolved built")
                    .handle(),
                view.dfao_resolved
                    .as_ref()
                    .expect("dfao_resolved built")
                    .view(),
                vk::ImageAspectFlags::COLOR,
                view.dfao_resolved
                    .as_ref()
                    .expect("dfao_resolved built")
                    .layout,
                Some(dfao_resolved_slot),
            );
            let (read_slot, read) = import_ssgi_history(graph, &view.dfao_history[1 - p]);
            let (write_slot, write) = import_ssgi_history(graph, &view.dfao_history[p]);
            let push = crate::SsgiAccumPush {
                params: saffron_geometry::glam::Vec4::new(
                    crate::SSGI_HISTORY_WEIGHT,
                    if view.history_valid { 1.0 } else { 0.0 },
                    0.0,
                    0.0,
                ),
            };
            self.add_compute_pass(
                graph,
                "dfao-accum",
                &dfao.accum,
                view.dfao_accum_sets[p],
                &[
                    (dfao_denoised, RgUsage::SampledReadCompute),
                    (read, RgUsage::SampledReadCompute),
                    (motion, RgUsage::SampledReadCompute),
                    (dfao_resolved, RgUsage::StorageImageRwCompute),
                    (write, RgUsage::StorageImageRwCompute),
                ],
                Some(bytemuck::bytes_of(&push).to_vec()),
                groups(extent.width),
                groups(extent.height),
                1,
            );
            result.scene_sampled.push(dfao_resolved);
            result.dfao_resolved_slot = Some(dfao_resolved_slot);
            result.dfao_history_slots = Some(TaaHistorySlots {
                read: (1 - p, read_slot),
                write: (p, write_slot),
            });
            // The accumulated map is what gi-resolve samples (its set binds `dfao_resolved`), so
            // the pass added after this block declares its read on the resource written here.
            dfao_sky_vis = Some(dfao_resolved);
        }

        // Screen-space indirect-diffuse resolve: reconstruct worldPos/n from the G-buffer, integrate
        // the DDGI cage + IBL diffuse × the DFAO sky-visibility (when it ran) into the half-res
        // gi_indirect. Runs whenever the screen chain does; its skyVis flag is 0 when DFAO is off.
        if let Some(gi_pso) = &pipelines.gi_resolve {
            // External-layout slot so the graph transitions gi_indirect from this pass's storage
            // write to SHADER_READ_ONLY for the scene pass's `SampledRead`.
            let gi_slot = graph.alloc_external_state(
                view.gi_indirect
                    .as_ref()
                    .expect("gi_indirect built")
                    .graph_state(),
            );
            let gi_indirect = graph.import_image(
                view.gi_indirect
                    .as_ref()
                    .expect("gi_indirect built")
                    .handle(),
                view.gi_indirect.as_ref().expect("gi_indirect built").view(),
                vk::ImageAspectFlags::COLOR,
                view.gi_indirect.as_ref().expect("gi_indirect built").layout,
                Some(gi_slot),
            );
            let mut accesses = vec![
                (g_normal, RgUsage::SampledReadCompute),
                (gi_indirect, RgUsage::StorageImageRwCompute),
                (sky_sh, RgUsage::StorageReadCompute),
            ];
            if let Some(dfao_res) = dfao_sky_vis {
                accesses.push((dfao_res, RgUsage::SampledReadCompute));
            }
            self.add_compute_pass(
                graph,
                "gi-resolve",
                gi_pso,
                view.gi_resolve_sets[self.frames.index()],
                &accesses,
                None,
                groups(half_extent.width),
                groups(half_extent.height),
                1,
            );
            // The scene fragment samples gi_indirect (set 4 binding 7), so declare it — the graph
            // barriers it ShaderReadOnly before the scene pass.
            result.scene_sampled.push(gi_indirect);
        }

        // Specular reflection-occlusion: the reflection-vector twin of the DFAO chain (Wright
        // 2015) — a half-res trace along the per-pixel reflection vector against the GDF, a
        // bilateral upsample, then temporal accumulation. The mesh samples the resolved occlusion
        // (set 4 binding 6) to occlude the reflected skybox; the trace also reads the roughness.
        if let Some(specocc) = &pipelines.specocc {
            let bindless_set = self.descriptors.bindless_set();
            let specocc_raw = graph.import_image(
                view.specocc_raw
                    .as_ref()
                    .expect("specocc_raw built")
                    .handle(),
                view.specocc_raw.as_ref().expect("specocc_raw built").view(),
                vk::ImageAspectFlags::COLOR,
                vk::ImageLayout::UNDEFINED,
                None,
            );
            let specocc_denoised = graph.import_image(
                view.specocc_denoised
                    .as_ref()
                    .expect("specocc_denoised built")
                    .handle(),
                view.specocc_denoised
                    .as_ref()
                    .expect("specocc_denoised built")
                    .view(),
                vk::ImageAspectFlags::COLOR,
                view.specocc_denoised
                    .as_ref()
                    .expect("specocc_denoised built")
                    .layout,
                None,
            );

            // 1. Trace: reconstruct world pos/normal + the reflection vector from the G-buffer,
            //    cone-trace the GDF occlusion into the half-res raw map. Records its three sets.
            let trace = Arc::clone(specocc);
            let trace_handle = trace.handle();
            let trace_layout = trace.layout();
            let trace_set = view.specocc_set;
            let trace_push = pipelines.specocc_push;
            let raw_body = raw.clone();
            let trace_gx = groups(half_extent.width);
            let trace_gy = groups(half_extent.height);
            let mut trace_pass = RgPass::compute("specocc")
                .access(g_normal, RgUsage::SampledReadCompute)
                .access(g_roughness, RgUsage::SampledReadCompute)
                .access(specocc_raw, RgUsage::StorageImageRwCompute);
            // The trace taps the GDF cascade clipmap (light set binding 9); declare the reads so
            // the graph transitions each cascade GENERAL (composite) → ShaderReadOnly first.
            if let Some(cascades) = gdf_cascades {
                for cascade in cascades {
                    trace_pass = trace_pass.access(cascade, RgUsage::SampledReadCompute);
                }
            }
            if let Some(occupancy) = gdf_occupancy {
                for volume in occupancy {
                    trace_pass = trace_pass.access(volume, RgUsage::SampledReadCompute);
                }
            }
            let trace_pass = trace_pass.body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                // SAFETY: the ash seam. The PSO + three sets are valid this frame; the dispatch
                // covers the half-res trace target.
                unsafe {
                    raw_body.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, trace_handle);
                    raw_body.cmd_bind_descriptor_sets(
                        cmd,
                        vk::PipelineBindPoint::COMPUTE,
                        trace_layout,
                        0,
                        &[bindless_set, light_set, trace_set],
                        &[],
                    );
                    raw_body.cmd_push_constants(
                        cmd,
                        trace_layout,
                        vk::ShaderStageFlags::COMPUTE,
                        0,
                        bytemuck::bytes_of(&trace_push),
                    );
                    raw_body.cmd_dispatch(cmd, trace_gx, trace_gy, 1);
                }
                drop(trace);
            });
            graph.add_pass(trace_pass);

            // 2. Bilateral upsample: specocc_raw (half-res) + g_normal → specocc_denoised (full-res).
            if let Some(specocc_blur) = &pipelines.specocc_blur {
                self.add_compute_pass(
                    graph,
                    "specocc-blur",
                    specocc_blur,
                    view.specocc_blur_set,
                    &[
                        (specocc_raw, RgUsage::SampledReadCompute),
                        (g_normal, RgUsage::SampledReadCompute),
                        (specocc_denoised, RgUsage::StorageImageRwCompute),
                    ],
                    None,
                    groups(extent.width),
                    groups(extent.height),
                    1,
                );
            }

            // Specular reflection occlusion is view-dependent; reprojecting it through surface
            // motion would smear it under camera motion. It is a low-frequency scalar once
            // bilaterally upsampled, so the mesh samples the spatially-denoised map directly.
            result.scene_sampled.push(specocc_denoised);
        }

        // Screen-space reflections: g_normal + prevColor → ssr_map. The mesh blends ssr_map
        // over the prefiltered-env specular, weighted by hit confidence × (1 - roughness),
        // so only smooth surfaces use it. No separate denoise — TAA cleans the march jitter.
        if let Some(ssr) = &pipelines.ssr {
            let prev_color = prev_color.expect("prev_color imported when SSR on");
            let ssr_slot = graph
                .alloc_external_state(view.ssr_map.as_ref().expect("ssr_map built").graph_state());
            let ssr_map = graph.import_image(
                view.ssr_map.as_ref().expect("ssr_map built").handle(),
                view.ssr_map.as_ref().expect("ssr_map built").view(),
                vk::ImageAspectFlags::COLOR,
                view.ssr_map.as_ref().expect("ssr_map built").layout,
                Some(ssr_slot),
            );
            self.add_compute_pass(
                graph,
                "ssr",
                ssr,
                view.ssr_set,
                &[
                    (g_normal, RgUsage::SampledReadCompute),
                    (prev_color, RgUsage::SampledReadCompute),
                    (ssr_map, RgUsage::StorageImageRwCompute),
                ],
                Some(bytemuck::bytes_of(&pipelines.ssr_push).to_vec()),
                groups(extent.width),
                groups(extent.height),
                1,
            );
            result.scene_sampled.push(ssr_map);
            result.ssr_map_slot = Some(ssr_slot);
        }

        // RT reflections sample prev_color directly in the mesh fragment (set-4 binding 4),
        // so the scene pass must SampledRead it (transition to ShaderReadOnly before the draw).
        if rt_refl && let Some(pc) = prev_color {
            result.scene_sampled.push(pc);
        }

        // The prev-color history copy runs AFTER the scene (it reads the scene's linear-HDR
        // color) for whichever of SSGI / SSR / RT reflections is on; hand the caller the info
        // to schedule it.
        if let (Some(copy), Some(prev_color)) = (&pipelines.copy_color, prev_color) {
            result.history_copy = Some(HistoryCopy {
                prev_color,
                pipeline: Arc::clone(copy),
                set: view.copy_color_set,
                groups_x: groups(extent.width),
                groups_y: groups(extent.height),
            });
        }

        result
    }
}

/// Imports one SSGI history image into `graph` on an external layout slot (its layout crosses
/// frames: ShaderReadOnly ↔ General for the accum write). Returns the slot index and the imported
/// resource. Panics if the image is not built — the accum pass runs only once the chain is.
fn import_ssgi_history(
    graph: &mut RenderGraph,
    image: &Option<crate::Image>,
) -> (usize, RgResource) {
    let image = image.as_ref().expect("ssgi history built");
    let slot = graph.alloc_external_state(image.graph_state());
    let resource = graph.import_image(
        image.handle(),
        image.view(),
        vk::ImageAspectFlags::COLOR,
        image.layout,
        Some(slot),
    );
    (slot, resource)
}

/// Writes an SSGI history image's resolved exit layout back from the graph's external slot.
/// `(history-index, slot)` selects the image in `view.ssgi_history` and the slot to read.
pub(super) fn writeback_ssgi_history_layout(
    view: &mut ViewTarget,
    graph: &RenderGraph,
    slot: &(usize, usize),
) {
    if let Some(image) = view.ssgi_history[slot.0].as_mut() {
        image.set_graph_state(graph.external_state(slot.1));
    }
}

/// Writes a DFAO history image's resolved exit layout back from the graph's external slot.
/// `(history-index, slot)` selects the image in `view.dfao_history` and the slot to read.
pub(super) fn writeback_dfao_history_layout(
    view: &mut ViewTarget,
    graph: &RenderGraph,
    slot: &(usize, usize),
) {
    if let Some(image) = view.dfao_history[slot.0].as_mut() {
        image.set_graph_state(graph.external_state(slot.1));
    }
}
