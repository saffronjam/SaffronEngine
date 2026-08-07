use super::*;

impl Renderer {
    pub(super) fn prepare_cloud_frame(
        &mut self,
        graph: &mut RenderGraph,
        pipelines: &FramePipelines,
        frame: usize,
    ) -> Option<CloudFrameResources> {
        if !self.clouds.settings().enabled {
            return None;
        }
        let view_index = self.active_view.index();
        let inv_view_proj = self.scene_view_proj_unjittered().inverse();
        let camera = self.ssao.view().inverse().col(3).truncate();
        let view = &self.views[view_index];
        let reduced_extent = view.cloud_reduced[0]
            .as_ref()
            .expect("cloud reduced built")
            .extent;
        let atmosphere = self.scene_ibl().baked_atmosphere();
        let current_view_proj = self.scene_view_proj_unjittered();
        // The cloud layer advects on the shared wind field's mean term at its own altitude.
        let scene_wind = self.scene_wind;
        let wind_radians = scene_wind.orientation.to_radians();
        let cloud_settings = self.clouds.settings();
        let layer_mid = cloud_settings.layer_altitude + cloud_settings.layer_height * 0.5;
        let wind_speed =
            scene_wind.speed * saffron_wind::shear_factor(&scene_wind.profile(), layer_mid);
        let state = crate::clouds::CloudFrameState {
            wind_direction: saffron_geometry::glam::Vec2::new(
                wind_radians.sin(),
                wind_radians.cos(),
            ),
            wind_speed,
            wind_gust: scene_wind.gust,
            wind_time_s: scene_wind.time_s as f32,
            inv_view_proj,
            prev_view_proj: if view.prev_view_proj_valid {
                view.prev_view_proj
            } else {
                current_view_proj
            },
            camera,
            sun_direction: (-self.sun_direction).normalize_or_zero(),
            sun_color: self.sun_color,
            sun_intensity: self.sun_intensity,
            moon_direction: (-self.moon_direction).normalize_or_zero(),
            moon_color: self.moon_color,
            moon_intensity: self.moon_intensity,
            planet_radius: atmosphere.planet_radius,
            atmosphere_height: atmosphere.atmosphere_height,
            jitter_index: view.jitter_index,
            reduced_extent,
            history_valid: view.history_valid && view.prev_view_proj_valid,
            atmosphere_live: self.scene_ibl().atmosphere_live(),
        };
        self.clouds.write_params(view_index, frame, &state);
        let params_offset = self.clouds.params_offset(view_index, frame);

        let base = self.clouds.base_noise();
        let base_slot = graph.alloc_external_state(base.graph_state());
        let base_res =
            graph.import_image_3d(base.handle(), base.view(), base.layout, Some(base_slot));
        let detail = self.clouds.detail_noise();
        let detail_slot = graph.alloc_external_state(detail.graph_state());
        let detail_res = graph.import_image_3d(
            detail.handle(),
            detail.view(),
            detail.layout,
            Some(detail_slot),
        );
        let curl = self.clouds.curl_noise();
        let curl_slot = graph.alloc_external_state(curl.graph_state());
        let curl_res = graph.import_image(
            curl.handle(),
            curl.view(),
            vk::ImageAspectFlags::COLOR,
            curl.layout,
            Some(curl_slot),
        );
        let weather = self.clouds.weather_map();
        let weather_slot = graph.alloc_external_state(weather.graph_state());
        let weather_res = graph.import_image(
            weather.handle(),
            weather.view(),
            vk::ImageAspectFlags::COLOR,
            weather.layout,
            Some(weather_slot),
        );
        let shadow = self.clouds.cloud_shadow();
        let shadow_slot = graph.alloc_external_state(shadow.graph_state());
        let shadow_res = graph.import_image(
            shadow.handle(),
            shadow.view(),
            vk::ImageAspectFlags::COLOR,
            shadow.layout,
            Some(shadow_slot),
        );

        if let Some(pipeline) = &pipelines.cloud_weather {
            let pipeline = Arc::clone(pipeline);
            let handle = pipeline.handle();
            let layout = pipeline.layout();
            let set = self.clouds.weather_set();
            let groups = crate::clouds::CLOUD_WEATHER_DIM.div_ceil(8);
            let raw_body = self.device.raw().clone();
            graph.add_pass(
                RgPass::compute("cloud-weather")
                    .access(weather_res, RgUsage::StorageImageRwCompute)
                    .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                        unsafe {
                            raw_body.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, handle);
                            raw_body.cmd_bind_descriptor_sets(
                                cmd,
                                vk::PipelineBindPoint::COMPUTE,
                                layout,
                                0,
                                &[set],
                                &[params_offset],
                            );
                            raw_body.cmd_dispatch(cmd, groups, groups, 1);
                        }
                        drop(pipeline);
                    }),
            );
            self.clouds.mark_weather_clean();
        }

        if let Some(pipeline) = &pipelines.cloud_shadow {
            let pipeline = Arc::clone(pipeline);
            let handle = pipeline.handle();
            let layout = pipeline.layout();
            let set = self.clouds.shadow_set();
            let groups = crate::clouds::CLOUD_SHADOW_DIM.div_ceil(8);
            let raw_body = self.device.raw().clone();
            graph.add_pass(
                RgPass::compute("cloud-shadow")
                    .access(shadow_res, RgUsage::StorageImageRwCompute)
                    .access(base_res, RgUsage::SampledReadCompute)
                    .access(detail_res, RgUsage::SampledReadCompute)
                    .access(curl_res, RgUsage::SampledReadCompute)
                    .access(weather_res, RgUsage::SampledReadCompute)
                    .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                        unsafe {
                            raw_body.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, handle);
                            raw_body.cmd_bind_descriptor_sets(
                                cmd,
                                vk::PipelineBindPoint::COMPUTE,
                                layout,
                                0,
                                &[set],
                                &[params_offset],
                            );
                            raw_body.cmd_dispatch(
                                cmd,
                                groups,
                                groups,
                                crate::clouds::CLOUD_SHADOW_CASCADES,
                            );
                        }
                        drop(pipeline);
                    }),
            );
        }

        Some(CloudFrameResources {
            base: base_res,
            detail: detail_res,
            curl: curl_res,
            weather: weather_res,
            shadow: shadow_res,
            base_slot,
            detail_slot,
            curl_slot,
            weather_slot,
            shadow_slot,
            params_offset,
        })
    }

    /// Appends the weather-map refresh and unlit density debugger. Every persistent image is
    /// imported on an external layout slot so the graph owns the transitions and the resolved
    /// layouts carry across frames.
    pub(super) fn add_cloud_passes(
        &mut self,
        graph: &mut RenderGraph,
        pipelines: &FramePipelines,
        inputs: CloudGraphInputs,
        prepared: Option<CloudFrameResources>,
    ) -> CloudGraphResult {
        let CloudGraphInputs {
            color,
            depth,
            motion,
            sky_sh,
        } = inputs;
        let lit_ready = pipelines.cloud_raymarch.is_some()
            && pipelines.cloud_reconstruct.is_some()
            && pipelines.cloud_upscale.is_some()
            && motion.is_some();
        let Some(prepared) = prepared else {
            return CloudGraphResult::default();
        };
        let view_index = self.active_view.index();
        let reduced_extent = self.views[view_index].cloud_reduced[0]
            .as_ref()
            .expect("cloud reduced built")
            .extent;
        let base_res = prepared.base;
        let detail_res = prepared.detail;
        let curl_res = prepared.curl;
        let weather_res = prepared.weather;
        let params_offset = prepared.params_offset;
        let mut result = CloudGraphResult {
            base: Some(prepared.base_slot),
            detail: Some(prepared.detail_slot),
            curl: Some(prepared.curl_slot),
            weather: Some(prepared.weather_slot),
            shadow: Some(prepared.shadow_slot),
            ..CloudGraphResult::default()
        };
        if pipelines.cloud_debug.is_none() && !lit_ready {
            graph.add_pass(
                RgPass::compute("cloud-resources-read")
                    .access(base_res, RgUsage::SampledReadCompute)
                    .access(detail_res, RgUsage::SampledReadCompute)
                    .access(curl_res, RgUsage::SampledReadCompute)
                    .access(weather_res, RgUsage::SampledReadCompute)
                    .access(prepared.shadow, RgUsage::SampledReadCompute)
                    .body(|_cmd, _scopes: &mut NestedScopeRecorder| {}),
            );
            return result;
        }

        if let Some(pipeline) = &pipelines.cloud_debug {
            let pipeline = Arc::clone(pipeline);
            let handle = pipeline.handle();
            let layout = pipeline.layout();
            let set = self.clouds.debug_set(view_index);
            let extent = self.views[view_index].published_extent();
            let groups_x = extent.width.div_ceil(8);
            let groups_y = extent.height.div_ceil(8);
            let raw_body = self.device.raw().clone();
            graph.add_pass(
                RgPass::compute("cloud-density-debug")
                    .access(color, RgUsage::StorageImageRwCompute)
                    .access(depth, RgUsage::SampledReadCompute)
                    .access(base_res, RgUsage::SampledReadCompute)
                    .access(detail_res, RgUsage::SampledReadCompute)
                    .access(curl_res, RgUsage::SampledReadCompute)
                    .access(weather_res, RgUsage::SampledReadCompute)
                    .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                        unsafe {
                            raw_body.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, handle);
                            raw_body.cmd_bind_descriptor_sets(
                                cmd,
                                vk::PipelineBindPoint::COMPUTE,
                                layout,
                                0,
                                &[set],
                                &[params_offset],
                            );
                            raw_body.cmd_dispatch(cmd, groups_x, groups_y, 1);
                        }
                        drop(pipeline);
                    }),
            );
            return result;
        }

        let motion = motion.expect("lit cloud motion resource");
        let parity = self.views[view_index].history_index;
        let view = &self.views[view_index];
        let mut reduced_resources = [None, None];
        for (index, image) in view.cloud_reduced.iter().enumerate() {
            let image = image.as_ref().expect("cloud reduced built");
            let slot = graph.alloc_external_state(image.graph_state());
            reduced_resources[index] = Some(graph.import_image(
                image.handle(),
                image.view(),
                vk::ImageAspectFlags::COLOR,
                image.layout,
                Some(slot),
            ));
            result.reduced[index] = Some(slot);
        }
        let reduced_resources = reduced_resources.map(|resource| resource.expect("cloud import"));
        let reduced_depth = view
            .cloud_reduced_depth
            .as_ref()
            .expect("cloud reduced depth built");
        let reduced_depth_slot = graph.alloc_external_state(reduced_depth.graph_state());
        let reduced_depth_res = graph.import_image(
            reduced_depth.handle(),
            reduced_depth.view(),
            vk::ImageAspectFlags::COLOR,
            reduced_depth.layout,
            Some(reduced_depth_slot),
        );
        result.reduced_depth = Some(reduced_depth_slot);
        let full_color = view
            .cloud_full_color
            .as_ref()
            .expect("cloud full color built");
        let full_color_slot = graph.alloc_external_state(full_color.graph_state());
        let full_color_res = graph.import_image(
            full_color.handle(),
            full_color.view(),
            vk::ImageAspectFlags::COLOR,
            full_color.layout,
            Some(full_color_slot),
        );
        result.full_color = Some(full_color_slot);
        result.full_color_resource = Some(full_color_res);
        let full_depth = view
            .cloud_full_depth
            .as_ref()
            .expect("cloud full depth built");
        let full_depth_slot = graph.alloc_external_state(full_depth.graph_state());
        let full_depth_res = graph.import_image(
            full_depth.handle(),
            full_depth.view(),
            vk::ImageAspectFlags::COLOR,
            full_depth.layout,
            Some(full_depth_slot),
        );
        result.full_depth = Some(full_depth_slot);
        result.full_depth_resource = Some(full_depth_res);

        let groups_x = reduced_extent.width.div_ceil(8);
        let groups_y = reduced_extent.height.div_ceil(8);
        let raymarch = Arc::clone(
            pipelines
                .cloud_raymarch
                .as_ref()
                .expect("lit cloud raymarch pipeline"),
        );
        let raymarch_handle = raymarch.handle();
        let raymarch_layout = raymarch.layout();
        let raymarch_set = self.clouds.raymarch_set(view_index, parity);
        let raw_body = self.device.raw().clone();
        graph.add_pass(
            RgPass::compute("cloud-raymarch")
                .access(reduced_resources[parity], RgUsage::StorageImageRwCompute)
                .access(reduced_depth_res, RgUsage::StorageImageRwCompute)
                .access(depth, RgUsage::SampledReadCompute)
                .access(base_res, RgUsage::SampledReadCompute)
                .access(detail_res, RgUsage::SampledReadCompute)
                .access(curl_res, RgUsage::SampledReadCompute)
                .access(weather_res, RgUsage::SampledReadCompute)
                .access(prepared.shadow, RgUsage::SampledReadCompute)
                .access(sky_sh, RgUsage::StorageReadCompute)
                .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                    unsafe {
                        raw_body.cmd_bind_pipeline(
                            cmd,
                            vk::PipelineBindPoint::COMPUTE,
                            raymarch_handle,
                        );
                        raw_body.cmd_bind_descriptor_sets(
                            cmd,
                            vk::PipelineBindPoint::COMPUTE,
                            raymarch_layout,
                            0,
                            &[raymarch_set],
                            &[params_offset],
                        );
                        raw_body.cmd_dispatch(cmd, groups_x, groups_y, 1);
                    }
                    drop(raymarch);
                }),
        );

        let reconstruct = Arc::clone(
            pipelines
                .cloud_reconstruct
                .as_ref()
                .expect("lit cloud reconstruct pipeline"),
        );
        let reconstruct_handle = reconstruct.handle();
        let reconstruct_layout = reconstruct.layout();
        let reconstruct_set = self.clouds.reconstruct_set(view_index, parity);
        let raw_body = self.device.raw().clone();
        graph.add_pass(
            RgPass::compute("cloud-reconstruct")
                .access(reduced_resources[parity], RgUsage::StorageImageRwCompute)
                .access(reduced_resources[1 - parity], RgUsage::SampledReadCompute)
                .access(reduced_depth_res, RgUsage::SampledReadCompute)
                .access(motion, RgUsage::SampledReadCompute)
                .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                    unsafe {
                        raw_body.cmd_bind_pipeline(
                            cmd,
                            vk::PipelineBindPoint::COMPUTE,
                            reconstruct_handle,
                        );
                        raw_body.cmd_bind_descriptor_sets(
                            cmd,
                            vk::PipelineBindPoint::COMPUTE,
                            reconstruct_layout,
                            0,
                            &[reconstruct_set],
                            &[params_offset],
                        );
                        raw_body.cmd_dispatch(cmd, groups_x, groups_y, 1);
                    }
                    drop(reconstruct);
                }),
        );

        let extent = self.views[view_index].published_extent();
        let upscale = Arc::clone(
            pipelines
                .cloud_upscale
                .as_ref()
                .expect("lit cloud upscale pipeline"),
        );
        let upscale_handle = upscale.handle();
        let upscale_layout = upscale.layout();
        let upscale_set = self.clouds.upscale_set(view_index, parity);
        let raw_body = self.device.raw().clone();
        graph.add_pass(
            RgPass::compute("cloud-upscale")
                .access(full_color_res, RgUsage::StorageImageRwCompute)
                .access(reduced_resources[parity], RgUsage::SampledReadCompute)
                .access(reduced_depth_res, RgUsage::SampledReadCompute)
                .access(depth, RgUsage::SampledReadCompute)
                .access(full_depth_res, RgUsage::StorageImageRwCompute)
                .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                    unsafe {
                        raw_body.cmd_bind_pipeline(
                            cmd,
                            vk::PipelineBindPoint::COMPUTE,
                            upscale_handle,
                        );
                        raw_body.cmd_bind_descriptor_sets(
                            cmd,
                            vk::PipelineBindPoint::COMPUTE,
                            upscale_layout,
                            0,
                            &[upscale_set],
                            &[params_offset],
                        );
                        raw_body.cmd_dispatch(
                            cmd,
                            extent.width.div_ceil(8),
                            extent.height.div_ceil(8),
                            1,
                        );
                    }
                    drop(upscale);
                }),
        );
        graph.add_pass(
            RgPass::compute("cloud-history-read")
                .access(reduced_resources[0], RgUsage::SampledReadCompute)
                .access(reduced_resources[1], RgUsage::SampledReadCompute)
                .access(reduced_depth_res, RgUsage::SampledReadCompute)
                .access(full_color_res, RgUsage::SampledReadCompute)
                .access(full_depth_res, RgUsage::SampledReadCompute)
                .body(|_cmd, _scopes: &mut NestedScopeRecorder| {}),
        );
        result.temporal = true;
        result
    }
}
