use super::*;

/// Queue the global-distance-field build runs on.
///
/// The cascade volumes are consumed much later in the frame — the far-field tap in `ddgi-trace`
/// and the lit gather — so the build overlaps the depth and gbuffer raster instead of stalling in
/// front of it. Every buffer and volume it touches is a declared access, which is what lets the
/// graph derive the release/acquire pair and the cross-queue timeline; a device with no
/// independent compute family runs the identical passes on graphics.
const GDF_BUILD_QUEUE: crate::RgQueuePreference = crate::RgQueuePreference::AsyncCompute;

impl Renderer {
    /// The micro-field slab-occluder push for this frame: the reach window, the unit-box brick
    /// every slab is backed by, and the resident-tile directory to walk.
    pub(super) fn gi_occluder_micro_push(
        &self,
        reach: (Vec3, Vec3),
        directory_offset: u32,
        directory_count: u32,
    ) -> crate::GiOccluderMicroPush {
        let field = &self.slab_sdf;
        let dims = |d: [u32; 3]| [d[0], d[1], d[2], 0];
        crate::GiOccluderMicroPush {
            reach_min: reach.0.extend(0.0).to_array(),
            reach_max: reach.1.extend(0.0).to_array(),
            local_min: field.bounds_min.extend(0.0).to_array(),
            local_max: field.bounds_max.extend(0.0).to_array(),
            voxel_dims: dims(field.voxel_dims),
            indirection_dims: dims(field.indirection_dims),
            atlas_bricks: dims(field.atlas_bricks),
            field: [
                field.bindless_index() as f32,
                field.max_dist,
                field.mip_count as f32,
                0.0,
            ],
            capacity: MAX_SDF_INSTANCES,
            directory_offset,
            directory_count,
            reserved: 0,
        }
    }

    /// Builds the four DDGI compute passes into `graph` when the chain runs this frame (DDGI on +
    /// ready + all four PSOs resolved): `ddgi-trace` (the GDF/MDF sphere-march → ray storage),
    /// `ddgi-blend-irr` (ray sampler → irradiance storage), `ddgi-blend-dist` (ray sampler →
    /// distance storage), `ddgi-border` (irradiance octahedral gutter copy). The graph derives
    /// every GENERAL ↔ ShaderReadOnly barrier from the declared usages.
    ///
    /// The trace is a three-set pass reusing the shared `sdf` module's `sampleField` (near MDF →
    /// far GDF), so it declares `SampledRead` on the GDF cascade volumes + the lite albedo cache
    /// and runs after the GDF passes. Returns the irradiance + distance atlases for the scene's
    /// `SampledRead` plus the imported images' external slots; empty when DDGI did not run.
    pub(super) fn add_ddgi_passes(
        &self,
        graph: &mut RenderGraph,
        pipelines: &FramePipelines,
        bindless_set: vk::DescriptorSet,
        light_set: vk::DescriptorSet,
        gdf: &GdfResult,
        sky_sh: RgResource,
    ) -> DdgiResult {
        // The four-pass chain runs only when DDGI is on + all PSOs resolved.
        let Some(ddgi_pipelines) = &pipelines.ddgi else {
            return DdgiResult::default();
        };
        let raw = self.device.raw().clone();

        let (ray_image, ray_view, ray_state) = self.ddgi.rays();
        let rays_slot = graph.alloc_external_state(ray_state);
        let ray_res = graph.import_image(
            ray_image,
            ray_view,
            vk::ImageAspectFlags::COLOR,
            ray_state.layout,
            Some(rays_slot),
        );

        let (irr_image, irr_view, irr_state) = self.ddgi.irradiance();
        let irr_slot = graph.alloc_external_state(irr_state);
        let irr_res = graph.import_image(
            irr_image,
            irr_view,
            vk::ImageAspectFlags::COLOR,
            irr_state.layout,
            Some(irr_slot),
        );

        let (dist_image, dist_view, dist_state) = self.ddgi.distance();
        let dist_slot = graph.alloc_external_state(dist_state);
        let dist_res = graph.import_image(
            dist_image,
            dist_view,
            vk::ImageAspectFlags::COLOR,
            dist_state.layout,
            Some(dist_slot),
        );

        // 1. Trace: sphere-march the shared distance field (near MDF via the bindless bricks + the
        //    light set's instance list; far GDF via the light set's cascade clipmap), reading the
        //    prev-irradiance atlas (multi-bounce) + the lite albedo cache (hit color), writing the
        //    ray image. Three sets, so it records its binds directly (like `sdf-ao`).
        let trace = Arc::clone(&ddgi_pipelines.trace);
        let trace_handle = trace.handle();
        let trace_pipeline_layout = trace.layout();
        let trace_set = self.ddgi.trace_set();
        let trace_push = self.ddgi.trace_push();
        let trace_groups_x = DDGI_RAYS_PER_PROBE.div_ceil(64);
        let raw_body = raw.clone();
        let sdf_instances_res = graph.import_buffer(self.sdf_instances.handle(), None);
        let sdf_meta_res = graph.import_buffer(self.sdf_meta.handle(), None);
        let mut trace_pass = RgPass::compute("ddgi-trace")
            .access(sdf_instances_res, RgUsage::StorageReadCompute)
            .access(sdf_meta_res, RgUsage::StorageReadCompute)
            .access(irr_res, RgUsage::SampledReadCompute)
            .access(ray_res, RgUsage::StorageImageRwCompute)
            .access(sky_sh, RgUsage::StorageReadCompute);
        // When the GDF composited this frame, the trace's far-field tap reads the cascade volumes
        // (light set binding 9) + the albedo cache (trace set 2) — declare the reads so the graph
        // transitions each from GENERAL (composite write) → ShaderReadOnly before the trace.
        if let Some(cascades) = gdf.cascades {
            for cascade in cascades {
                trace_pass = trace_pass.access(cascade, RgUsage::SampledReadCompute);
            }
        }
        if let Some(occupancy) = gdf.occupancy {
            for volume in occupancy {
                trace_pass = trace_pass.access(volume, RgUsage::SampledReadCompute);
            }
        }
        if let Some(albedo) = gdf.albedo {
            trace_pass = trace_pass.access(albedo, RgUsage::SampledReadCompute);
        }
        let trace_pass = trace_pass.body(move |cmd, _scopes: &mut NestedScopeRecorder| {
            // SAFETY: the ash seam. The PSO + three sets are valid this frame; the dispatch
            // covers the round-robin probe-budget slice (the shader offsets the probe index by
            // trace_push's budget offset), not the whole volume.
            unsafe {
                raw_body.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, trace_handle);
                raw_body.cmd_bind_descriptor_sets(
                    cmd,
                    vk::PipelineBindPoint::COMPUTE,
                    trace_pipeline_layout,
                    0,
                    &[bindless_set, light_set, trace_set],
                    &[],
                );
                raw_body.cmd_push_constants(
                    cmd,
                    trace_pipeline_layout,
                    vk::ShaderStageFlags::COMPUTE,
                    0,
                    bytemuck::bytes_of(&trace_push),
                );
                raw_body.cmd_dispatch(cmd, trace_groups_x, crate::ddgi::DDGI_PROBE_BUDGET, 1);
            }
            drop(trace);
        });
        graph.add_pass(trace_pass);

        // 2. Blend irradiance: ray sampler → irradiance storage.
        let irr_w = crate::ddgi::irradiance_atlas_width();
        let irr_h = crate::ddgi::irradiance_atlas_height();
        let blend_irr = Arc::clone(&ddgi_pipelines.blend_irr);
        let blend_irr_handle = blend_irr.handle();
        let blend_irr_layout = blend_irr.layout();
        let blend_irr_set = self.ddgi.blend_irr_set();
        let blend_irr_push = self.ddgi.blend_irradiance_push();
        let raw_body = raw.clone();
        graph.add_pass(
            RgPass::compute("ddgi-blend-irr")
                .access(ray_res, RgUsage::SampledReadCompute)
                .access(irr_res, RgUsage::StorageImageRwCompute)
                .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                    record_ddgi_compute(
                        &raw_body,
                        cmd,
                        blend_irr_handle,
                        blend_irr_layout,
                        blend_irr_set,
                        bytemuck::bytes_of(&blend_irr_push),
                        (irr_w.div_ceil(8), irr_h.div_ceil(8), 1),
                    );
                    drop(blend_irr);
                }),
        );

        // 3. Blend distance: ray sampler → moment (distance) storage.
        let dist_w = crate::ddgi::distance_atlas_width();
        let dist_h = crate::ddgi::distance_atlas_height();
        let blend_dist = Arc::clone(&ddgi_pipelines.blend_dist);
        let blend_dist_handle = blend_dist.handle();
        let blend_dist_layout = blend_dist.layout();
        let blend_dist_set = self.ddgi.blend_dist_set();
        let blend_dist_push = self.ddgi.blend_distance_push();
        let raw_body = raw.clone();
        graph.add_pass(
            RgPass::compute("ddgi-blend-dist")
                .access(ray_res, RgUsage::SampledReadCompute)
                .access(dist_res, RgUsage::StorageImageRwCompute)
                .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                    record_ddgi_compute(
                        &raw_body,
                        cmd,
                        blend_dist_handle,
                        blend_dist_layout,
                        blend_dist_set,
                        bytemuck::bytes_of(&blend_dist_push),
                        (dist_w.div_ceil(8), dist_h.div_ceil(8), 1),
                    );
                    drop(blend_dist);
                }),
        );

        // 4. Border copy: fix the irradiance octahedral gutters (read+write the same
        //    storage image). Leaves irradiance GENERAL; the scene's SampledRead then
        //    transitions it ShaderReadOnly for the mesh sample.
        let border = Arc::clone(&ddgi_pipelines.border);
        let border_handle = border.handle();
        let border_layout = border.layout();
        let border_set = self.ddgi.border_set();
        let border_push = self.ddgi.border_push();
        let raw_body = raw.clone();
        graph.add_pass(
            RgPass::compute("ddgi-border")
                .access(irr_res, RgUsage::StorageImageRwCompute)
                .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                    record_ddgi_compute(
                        &raw_body,
                        cmd,
                        border_handle,
                        border_layout,
                        border_set,
                        bytemuck::bytes_of(&border_push),
                        (irr_w.div_ceil(8), irr_h.div_ceil(8), 1),
                    );
                    drop(border);
                }),
        );

        DdgiResult {
            irradiance: Some(irr_res),
            distance: Some(dist_res),
            rays_slot: Some(rays_slot),
            irradiance_slot: Some(irr_slot),
            distance_slot: Some(dist_slot),
        }
    }

    /// Builds the two Global-SDF compute passes into `graph`: `gdf-cull` bins the per-mesh MDF
    /// instances per cascade into a compacted list, then `gdf-composite` `min()`s the culled bricks
    /// into the toroidal `R16_SNORM` cascade volume for each dirty voxel. The cull list buffer
    /// serializes the two via the graph-derived RAW barrier, and each cascade volume rides its own
    /// external slot for the cross-frame layout write-back. Empty when the GDF did not run.
    pub(super) fn add_gdf_passes(
        &self,
        graph: &mut RenderGraph,
        pipelines: &FramePipelines,
        frame: usize,
    ) -> GdfResult {
        let Some(gdf_pipelines) = &pipelines.gdf else {
            return GdfResult::default();
        };
        let raw = self.device.raw().clone();

        // Import this frame slot's cull-list buffer (the cull writes it, the composite reads it) +
        // each cascade volume (the composite writes them, the downstream consumers sample them). The
        // cull list is per-frame-in-flight so frame N+1's clear/rebuild never races frame N's reads.
        let cull_state_slot =
            graph.alloc_external_buffer_state(self.global_sdf.cull_buffer_state(frame));
        let cull_res =
            graph.import_buffer(self.global_sdf.cull_buffer(frame), Some(cull_state_slot));
        // The occluder region + meta the scatter wrote earlier this frame: declared on
        // the cull (and the composite, which reads instances through the cull list) so
        // the graph orders both after the scatter's write.
        let sdf_instances_res = graph.import_buffer(self.sdf_instances.handle(), None);
        let sdf_meta_res = graph.import_buffer(self.sdf_meta.handle(), None);
        let mut cascade_res = [RgResource { index: 0 }; crate::GDF_CASCADES as usize];
        let mut cascade_slots = [None; crate::GDF_CASCADES as usize];
        let mut occupancy_res = [RgResource { index: 0 }; crate::GDF_CASCADES as usize];
        let mut occupancy_slots = [None; crate::GDF_CASCADES as usize];
        for c in 0..crate::GDF_CASCADES {
            let (image, view, layout) = self.global_sdf.cascade(c);
            let slot = graph.alloc_external_state(crate::RgExternalState::new(layout));
            cascade_res[c as usize] = graph.import_image_3d(image, view, layout, Some(slot));
            cascade_slots[c as usize] = Some(slot);
            let (image, view, layout) = self.global_sdf.occupancy_cascade(c);
            let slot = graph.alloc_external_state(crate::RgExternalState::new(layout));
            occupancy_res[c as usize] = graph.import_image_3d(image, view, layout, Some(slot));
            occupancy_slots[c as usize] = Some(slot);
        }
        // The lite albedo cache (the composite splats it for the finest cascade; the DDGI trace
        // samples it at hit points).
        let (albedo_image, albedo_view, albedo_layout) = self.global_sdf.albedo_cache();
        let albedo_slot = graph.alloc_external_state(crate::RgExternalState::new(albedo_layout));
        let albedo_res =
            graph.import_image_3d(albedo_image, albedo_view, albedo_layout, Some(albedo_slot));

        // 1. Cull: clear the per-cascade counters (a `cmd_fill_buffer` + a transfer→compute barrier,
        //    the one place the graph has no primitive for), then bin every instance into the
        //    cascade(s) it touches. One thread per instance.
        {
            let cull = Arc::clone(&gdf_pipelines.cull);
            let handle = cull.handle();
            let layout = cull.layout();
            let bindless_set = self.descriptors.bindless_set();
            let cull_set = self.global_sdf.cull_set(frame);
            let push = self.global_sdf.cull_push(MAX_SDF_INSTANCES);
            let counter_bytes = self.global_sdf.cull_counter_bytes();
            let cull_buffer = self.global_sdf.cull_buffer(frame);
            let groups = MAX_SDF_INSTANCES.div_ceil(64);
            let raw_body = raw.clone();
            graph.add_pass(
                RgPass::compute("gdf-cull")
                    .queue(GDF_BUILD_QUEUE)
                    .access(sdf_instances_res, RgUsage::StorageReadCompute)
                    .access(sdf_meta_res, RgUsage::StorageReadCompute)
                    .access(cull_res, RgUsage::StorageWriteCompute)
                    .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                        // SAFETY: the ash seam. The PSO/sets are valid this frame. The fill zeroes
                        // the atomic counters; the barrier orders the transfer write before the
                        // cull's atomic reads (the graph has no fill primitive, so this one barrier
                        // is hand-written and local to the pass).
                        unsafe {
                            raw_body.cmd_fill_buffer(cmd, cull_buffer, 0, counter_bytes, 0);
                            let barrier = vk::BufferMemoryBarrier2::default()
                                .src_stage_mask(vk::PipelineStageFlags2::CLEAR)
                                .src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
                                .dst_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER)
                                .dst_access_mask(
                                    vk::AccessFlags2::SHADER_STORAGE_READ
                                        | vk::AccessFlags2::SHADER_STORAGE_WRITE,
                                )
                                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                                .buffer(cull_buffer)
                                .offset(0)
                                .size(counter_bytes);
                            let barriers = [barrier];
                            let dep =
                                vk::DependencyInfo::default().buffer_memory_barriers(&barriers);
                            raw_body.cmd_pipeline_barrier2(cmd, &dep);
                            raw_body.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, handle);
                            raw_body.cmd_bind_descriptor_sets(
                                cmd,
                                vk::PipelineBindPoint::COMPUTE,
                                layout,
                                0,
                                &[bindless_set, cull_set],
                                &[],
                            );
                            raw_body.cmd_push_constants(
                                cmd,
                                layout,
                                vk::ShaderStageFlags::COMPUTE,
                                0,
                                bytemuck::bytes_of(&push),
                            );
                            raw_body.cmd_dispatch(cmd, groups, 1, 1);
                        }
                        drop(cull);
                    }),
            );
        }

        // 2. Composite: per dirty region of each cascade, `min()` the culled bricks → the toroidal
        //    cascade volume. The near cascade updates incrementally; one cascade per frame gets the
        //    staggered full refresh (the round-robin in `dirty_regions`). All cascade writes ride
        //    the same pass declaring StorageImageRwCompute on every cascade (so the graph holds them
        //    GENERAL across the region dispatches), reading the cull list.
        let composite = Arc::clone(&gdf_pipelines.composite);
        let handle = composite.handle();
        let layout = composite.layout();
        let bindless_set = self.descriptors.bindless_set();
        let composite_set = self.global_sdf.composite_set(frame);
        // Gather every dirty region (cascade index + push) so the pass body issues one dispatch each.
        let mut dispatches: Vec<(crate::GdfCompositePush, u32, u32, u32)> = Vec::new();
        for c in 0..crate::GDF_CASCADES {
            for region in self.global_sdf.dirty_regions(c) {
                let push = self.global_sdf.composite_push(c, region);
                let g = (
                    region.size.x.div_ceil(4),
                    region.size.y.div_ceil(4),
                    region.size.z.div_ceil(4),
                );
                dispatches.push((push, g.0, g.1, g.2));
            }
        }
        let mut composite_pass = RgPass::compute("gdf-composite")
            .queue(GDF_BUILD_QUEUE)
            .access(cull_res, RgUsage::StorageReadCompute)
            .access(sdf_instances_res, RgUsage::StorageReadCompute);
        for cascade in cascade_res {
            composite_pass = composite_pass.access(cascade, RgUsage::StorageImageRwCompute);
        }
        for occupancy in occupancy_res {
            composite_pass = composite_pass.access(occupancy, RgUsage::StorageImageRwCompute);
        }
        composite_pass = composite_pass.access(albedo_res, RgUsage::StorageImageRwCompute);
        let raw_body = raw.clone();
        composite_pass = composite_pass.body(move |cmd, _scopes: &mut NestedScopeRecorder| {
            // SAFETY: the ash seam. The PSO/sets are valid this frame; each dispatch covers one
            // dirty region (4³ per group), pushing that region's cascade + bounds.
            unsafe {
                raw_body.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, handle);
                raw_body.cmd_bind_descriptor_sets(
                    cmd,
                    vk::PipelineBindPoint::COMPUTE,
                    layout,
                    0,
                    &[bindless_set, composite_set],
                    &[],
                );
                for (push, gx, gy, gz) in &dispatches {
                    raw_body.cmd_push_constants(
                        cmd,
                        layout,
                        vk::ShaderStageFlags::COMPUTE,
                        0,
                        bytemuck::bytes_of(push),
                    );
                    raw_body.cmd_dispatch(cmd, *gx, *gy, *gz);
                }
            }
            drop(composite);
        });
        graph.add_pass(composite_pass);

        GdfResult {
            cascades: Some(cascade_res),
            cascade_slots,
            occupancy: Some(occupancy_res),
            occupancy_slots,
            albedo: Some(albedo_res),
            albedo_slot: Some(albedo_slot),
            cull_state: Some(GdfCullState {
                slot: cull_state_slot,
                frame,
            }),
        }
    }

    /// Builds the three ReSTIR DI compute passes into `graph`: `restir-initial` (K candidates per
    /// pixel from the froxel light lists), `restir-reuse` (temporal + spatial reservoir reuse), and
    /// `restir-resolve` (one TLAS visibility ray per pixel → the resolved direct radiance image).
    /// The three serialize via graph-derived RAW barriers on the combined-reservoir sentinel buffer.
    ///
    /// The runtime gate ANDs the PSOs, RT support, a TLAS built this frame, the cluster cull, and
    /// the G-buffer prepass; an empty result leaves direct lighting on the clustered-forward path.
    pub(super) fn add_restir_passes(
        &mut self,
        graph: &mut RenderGraph,
        pipelines: &FramePipelines,
        frame: usize,
        motion: Option<RgResource>,
    ) -> RestirResult {
        let Some(restir_pipelines) = &pipelines.restir else {
            return RestirResult::default();
        };
        // The full runtime gate (the PSO presence already implies use_restir + ready +
        // supported + the G-buffer prepass armed): the TLAS must be built (the resolve
        // traces it) and the cull must have armed the froxel candidate lists.
        let gbuffer_ran = pipelines.gbuffer.is_some()
            && self.views[self.active_view.index()].screen_space_ready();
        let cull_ran = pipelines.cull.is_some();
        if !self.rt.tlas_ready() || !gbuffer_ran || !cull_ran {
            return RestirResult::default();
        }
        // The view's reservoirs must be built (sized to this extent) and its radiance present.
        if !self.views[self.active_view.index()].restir.ready() {
            return RestirResult::default();
        }
        let Some((rad_image, rad_view, rad_layout)) =
            self.views[self.active_view.index()].restir.radiance()
        else {
            return RestirResult::default();
        };
        let Some(combined) = self.views[self.active_view.index()]
            .restir
            .combined_buffer()
        else {
            return RestirResult::default();
        };

        // Write this frame's per-view bindings: the G-buffer (set) + motion samplers, the
        // light + cluster SSBOs (they regrow), and the TLAS into the resolve set. Resolved
        // through `&self` reads gathered first so the `&self.views[..].restir` write does not
        // alias a live borrow.
        let g_normal_view = self.views[self.active_view.index()]
            .g_normal
            .as_ref()
            .expect("g_normal built for restir")
            .view();
        let motion_view = self.views[self.active_view.index()]
            .motion
            .as_ref()
            .map(crate::Image::view);
        let light_buffer = self.lighting.light_list_buffer(frame);
        let cluster_buffer = self.lighting.cluster_buffer_with_size(frame);
        let tlas = self.rt.frame_tlas(frame);
        let address_block = (
            self.gpu_scene_uploader.address_buffer(),
            frame as u64 * self.gpu_scene_uploader.address_block_stride(),
            size_of::<crate::GpuSceneAddressBlock>() as u64,
        );
        self.views[self.active_view.index()]
            .restir
            .write_frame_bindings(
                &self.device,
                &self.restir,
                g_normal_view,
                motion_view,
                light_buffer,
                cluster_buffer,
                tlas,
                address_block,
            );

        // The per-frame push inputs (the camera inverses + eye come from the shared SSAO
        // camera the renderer set this frame; the light count from the lighting rig).
        let inv_view = self.ssao.view().inverse();
        let inv_projection = self.ssao.inv_projection();
        let eye = inv_view.col(3).truncate();
        let light_count = self.lighting.frame_light_count();
        let extent = self.views[self.active_view.index()].scaled_render_extent();
        let frame_index = self.views[self.active_view.index()].restir.frame_index();
        let history_valid = !self.views[self.active_view.index()].restir.history_reset();

        let initial_push =
            self.restir
                .initial_push(inv_view, inv_projection, light_count, extent, frame_index);
        let reuse_push =
            self.restir
                .reuse_push(inv_view, inv_projection, extent, frame_index, history_valid);
        let resolve_push = self
            .restir
            .resolve_push(inv_view, inv_projection, extent, eye);

        let initial_set = self.views[self.active_view.index()].restir.initial_set();
        let reuse_set = self.views[self.active_view.index()].restir.reuse_set();
        let resolve_set = self.views[self.active_view.index()].restir.resolve_set();
        let mesh_set = self.views[self.active_view.index()].restir.mesh_set();

        let raw = self.device.raw().clone();
        let groups = |n: u32| n.div_ceil(8);
        let groups_x = groups(extent.width);
        let groups_y = groups(extent.height);

        // The combined-reservoir SSBO is the sentinel: the three passes serialize through
        // RAW barriers the graph derives from StorageWrite → StorageRead on it. The radiance
        // image rides an external slot for the cross-frame
        // General ↔ ShaderReadOnly write-back.
        let sentinel = graph.import_buffer(combined, None);
        let radiance_slot = graph.alloc_external_state(crate::RgExternalState::new(rad_layout));
        let radiance_res = graph.import_image(
            rad_image,
            rad_view,
            vk::ImageAspectFlags::COLOR,
            rad_layout,
            Some(radiance_slot),
        );

        // 1. initial: K candidate lights per pixel → the initial reservoir (storage write).
        let initial = Arc::clone(&restir_pipelines.initial);
        let initial_handle = initial.handle();
        let initial_layout = initial.layout();
        let raw_body = raw.clone();
        graph.add_pass(
            RgPass::compute("restir-initial")
                .access(sentinel, RgUsage::StorageWriteCompute)
                .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                    record_ddgi_compute(
                        &raw_body,
                        cmd,
                        initial_handle,
                        initial_layout,
                        initial_set,
                        bytemuck::bytes_of(&initial_push),
                        (groups_x, groups_y, 1),
                    );
                    drop(initial);
                }),
        );

        // 2. reuse: temporal + spatial reservoir reuse → the combined reservoir. Reads the
        //    sentinel (the graph emits the RAW barrier after the initial write) + the motion
        //    target's sampler (the temporal term reprojects through it). Declaring the motion
        //    SampledRead orders this after the motion prepass (ColorWrite → SampledRead).
        let reuse = Arc::clone(&restir_pipelines.reuse);
        let reuse_handle = reuse.handle();
        let reuse_layout = reuse.layout();
        let raw_body = raw.clone();
        let mut reuse_pass =
            RgPass::compute("restir-reuse").access(sentinel, RgUsage::StorageReadCompute);
        if let Some(motion) = motion {
            reuse_pass = reuse_pass.access(motion, RgUsage::SampledReadCompute);
        }
        graph.add_pass(
            reuse_pass.body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                record_ddgi_compute(
                    &raw_body,
                    cmd,
                    reuse_handle,
                    reuse_layout,
                    reuse_set,
                    bytemuck::bytes_of(&reuse_push),
                    (groups_x, groups_y, 1),
                );
                drop(reuse);
            }),
        );

        // 3. resolve: one TLAS visibility ray per pixel + shade → the radiance image
        //    (storage RW). Reads the sentinel (the combined reservoir) + writes the radiance.
        //    Binds set 1 = the bindless texture array for the non-opaque candidate coverage
        //    confirmation.
        let resolve = Arc::clone(&restir_pipelines.resolve);
        let resolve_handle = resolve.handle();
        let resolve_layout = resolve.layout();
        let bindless_set = self.descriptors.bindless_set();
        let raw_body = raw.clone();
        graph.add_pass(
            RgPass::compute("restir-resolve")
                .access(sentinel, RgUsage::StorageReadCompute)
                .access(radiance_res, RgUsage::StorageImageRwCompute)
                .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                    // SAFETY: the ash seam. The PSO/sets/layout are valid this frame; the
                    // push spans the declared range; the dispatch covers the view grid.
                    unsafe {
                        raw_body.cmd_bind_pipeline(
                            cmd,
                            vk::PipelineBindPoint::COMPUTE,
                            resolve_handle,
                        );
                        raw_body.cmd_bind_descriptor_sets(
                            cmd,
                            vk::PipelineBindPoint::COMPUTE,
                            resolve_layout,
                            0,
                            &[resolve_set, bindless_set],
                            &[],
                        );
                        raw_body.cmd_push_constants(
                            cmd,
                            resolve_layout,
                            vk::ShaderStageFlags::COMPUTE,
                            0,
                            bytemuck::bytes_of(&resolve_push),
                        );
                        raw_body.cmd_dispatch(cmd, groups_x, groups_y, 1);
                    }
                    drop(resolve);
                }),
        );

        // Advance the per-view temporal state (bump the RNG index, clear the history reset)
        // in the graph build, after adding the three passes.
        self.views[self.active_view.index()].restir.advance_frame();

        RestirResult {
            radiance: Some(radiance_res),
            mesh_set,
            radiance_slot: Some(radiance_slot),
        }
    }

    /// Copies this frame slot's SDF-scatter meta words into the fence-gated host mirror, after
    /// every consumer has read the slice.
    pub(super) fn add_sdf_meta_readback_pass(&self, graph: &mut RenderGraph, frame: usize) {
        let meta_res = graph.import_buffer(self.sdf_meta.handle(), None);
        let readback_res = graph.import_buffer(self.sdf_meta_readback.handle(), None);
        let raw_copy = self.device.raw().clone();
        let meta = self.sdf_meta.handle();
        let readback = self.sdf_meta_readback.handle();
        let offset = frame as u64 * SDF_META_SLOT_BYTES;
        graph.add_pass(
            RgPass::compute("sdf-meta-readback")
                .access(meta_res, RgUsage::TransferRead)
                .access(readback_res, RgUsage::TransferWrite)
                .body(move |cmd, _scopes| {
                    let region = vk::BufferCopy {
                        src_offset: offset,
                        dst_offset: offset,
                        size: SDF_META_SLOT_BYTES,
                    };
                    // SAFETY: the ash seam; both buffers outlive the frame.
                    unsafe {
                        raw_copy.cmd_copy_buffer(cmd, meta, readback, &[region]);
                    }
                }),
        );
    }
}

/// Records one single-set DDGI compute dispatch: bind the PSO + its set 0, push the per-pass
/// constants, dispatch `groups`. Shared by the blend + border passes (the trace binds three sets,
/// so it records its binds directly).
fn record_ddgi_compute(
    raw: &ash::Device,
    cmd: vk::CommandBuffer,
    pipeline: vk::Pipeline,
    layout: vk::PipelineLayout,
    set: vk::DescriptorSet,
    push: &[u8],
    groups: (u32, u32, u32),
) {
    // SAFETY: the ash seam. The PSO/set/layout are valid this frame; the push spans the
    // pass's declared range; the dispatch covers the pass's grid.
    unsafe {
        raw.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, pipeline);
        raw.cmd_bind_descriptor_sets(cmd, vk::PipelineBindPoint::COMPUTE, layout, 0, &[set], &[]);
        raw.cmd_push_constants(cmd, layout, vk::ShaderStageFlags::COMPUTE, 0, push);
        raw.cmd_dispatch(cmd, groups.0, groups.1, groups.2);
    }
}
