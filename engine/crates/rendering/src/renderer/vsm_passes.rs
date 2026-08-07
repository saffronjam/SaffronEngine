use super::*;

impl Renderer {
    /// Adds every directional page a swept world AABB overlaps to `pages`.
    fn collect_vsm_directional_swept_bounds(
        &self,
        pages: &mut [bool],
        min: [f32; 3],
        max: [f32; 3],
    ) {
        for level in 0..crate::VSM_DIRECTIONAL_LEVELS {
            let Some((x0, y0, x1, y1)) = self.vsm_space.directional_page_span(level, min, max)
            else {
                continue;
            };
            for y in y0..=y1 {
                for x in x0..=x1 {
                    let index = ((level * crate::VSM_LEVEL_PAGES + y) * crate::VSM_LEVEL_PAGES + x)
                        as usize;
                    if let Some(slot) = pages.get_mut(index) {
                        *slot = true;
                    }
                }
            }
        }
    }

    /// The frame's directional virtual-shadow step, before the light UBO write: rebuild the
    /// snapped space, invalidate levels whose windows moved, seed the conservative camera-centred
    /// demand, stage this frame's dirty pages, and publish the page table the sampler reads.
    pub(super) fn prepare_vsm_frame(
        &mut self,
        frame: usize,
        sun_direction: saffron_geometry::glam::Vec3,
    ) {
        // One global atlas serves the scene view; preview/thumbnail lighting keeps
        // its own fixed behaviour without thrashing the residency.
        if self.active_view.index() != 0 {
            return;
        }
        // The master shadow toggle: off publishes a disabled table (every sampler
        // reads unshadowed via the `vsmParams.z` gate, so the demand marker writes
        // nothing) and stages no pages.
        if !self.lighting.use_shadows {
            self.vsm_render_list = crate::VsmRenderList::default();
            self.lighting.set_frame_vsm(
                saffron_geometry::glam::Mat4::IDENTITY,
                [saffron_geometry::glam::Vec4::ZERO; crate::VSM_DIRECTIONAL_LEVELS as usize],
                saffron_geometry::glam::Vec4::ZERO,
                0,
            );
            return;
        }
        // Work staged last frame that the graph never rasterized stays dirty.
        let unrasterized = std::mem::take(&mut self.vsm_render_list);
        for page in unrasterized.directional {
            self.vsm_residency
                .mark_dirty(page.key, crate::VsmDirtyReason::Restaged);
        }
        if unrasterized.spot {
            self.vsm_residency
                .mark_spot_dirty(crate::VsmDirtyReason::Restaged);
        }
        for (face, staged) in unrasterized.point_faces.into_iter().enumerate() {
            if staged {
                self.vsm_residency
                    .mark_point_face_dirty(face as u32, crate::VsmDirtyReason::Restaged);
            }
        }
        let space = crate::VsmDirectionalSpace::build(sun_direction, self.page_demand_view().eye);
        let serial = self.frame_serial;
        self.vsm_residency.begin_frame(serial);
        for level in 0..crate::VSM_DIRECTIONAL_LEVELS {
            if space.levels[level as usize].snap != self.vsm_space.levels[level as usize].snap {
                self.vsm_residency
                    .invalidate_directional_level(level, serial);
            }
        }
        self.vsm_space = space;
        // Receiver-driven demand from the drained GPU requests, plus a coarse
        // bootstrap ring so the first frames (and un-marked regions) fall back to
        // the outermost level instead of nothing.
        {
            let bootstrap = crate::VSM_LEVEL_PAGES / 2;
            for y in (bootstrap - 4)..(bootstrap + 4) {
                for x in (bootstrap - 4)..(bootstrap + 4) {
                    let _ = self.vsm_residency.demand_directional(
                        crate::VSM_DIRECTIONAL_LEVELS - 1,
                        x,
                        y,
                        serial,
                        crate::VsmDemandReason::Bootstrap,
                    );
                }
            }
        }
        // A moved/re-aimed spot invalidates its whole space (the projective pages
        // are meaningless under the new transform).
        let spot_matrix = self.lighting.spot_shadow_view_proj().to_cols_array();
        if self.vsm_spot_matrix != spot_matrix {
            self.vsm_spot_matrix = spot_matrix;
            self.vsm_residency.invalidate_spot(serial);
        }
        // A moved or re-ranged point light stales all six face spaces.
        let point_key = crate::vsm::vsm_point_light_key(
            self.lighting.point_shadow_pos(),
            self.lighting.point_shadow_far(),
        );
        let point_light_moved = self.vsm_point_key != point_key;
        if point_light_moved {
            self.vsm_point_key = point_key;
            self.vsm_residency
                .mark_point_dirty(crate::VsmDirtyReason::LightTransform);
        }
        // Dynamic content re-dirties the pages it overlaps; static pages stay
        // cached. Discrete movers arrive as swept bounds from the persistent
        // scene's instance deltas; continuous wind sway re-dirties the levels
        // fine enough to resolve it (the render budget paces the churn).
        let (moved, moved_overflow) = self.persistent_gpu_scene.take_moved_bounds();
        let wind_dynamic = self.scene_wind.speed > 0.0 && self.active_world_has_wind_instances();
        if wind_dynamic || moved_overflow {
            self.vsm_residency
                .mark_dynamic_dirty(crate::vsm::VSM_DYNAMIC_MAX_LEVEL);
        }
        let moved_projective = !moved.is_empty();
        if moved_projective {
            let mut directional_pages = vec![
                false;
                (crate::VSM_DIRECTIONAL_LEVELS * crate::VSM_LEVEL_PAGES * crate::VSM_LEVEL_PAGES)
                    as usize
            ];
            for (min, max) in moved {
                self.collect_vsm_directional_swept_bounds(&mut directional_pages, min, max);
            }
            for (index, dirty) in directional_pages.into_iter().enumerate() {
                if !dirty {
                    continue;
                }
                let index = index as u32;
                let level_area = crate::VSM_LEVEL_PAGES * crate::VSM_LEVEL_PAGES;
                let level = index / level_area;
                let page = index % level_area;
                self.vsm_residency.mark_dirty(
                    crate::VsmPageKey::Directional {
                        level,
                        x: page % crate::VSM_LEVEL_PAGES,
                        y: page / crate::VSM_LEVEL_PAGES,
                    },
                    crate::VsmDirtyReason::MovedCaster,
                );
            }
        }
        if moved_projective {
            if self.lighting.spot_shadow_pending() {
                self.vsm_residency
                    .mark_spot_dirty(crate::VsmDirtyReason::MovedCaster);
            }
            if self.lighting.point_shadow_pending() {
                self.vsm_residency
                    .mark_point_dirty(crate::VsmDirtyReason::MovedCaster);
            }
        }
        let point_space_dirty =
            self.lighting.point_shadow_pending() && (point_light_moved || moved_projective);
        let mut demand_spot = false;
        let mut demand_point_faces = [false; crate::vsm::VSM_POINT_FACES as usize];
        for &index in &self.vsm_demanded {
            if let Some(key) = crate::vsm::vsm_demand_key(index) {
                match key {
                    crate::VsmPageKey::Spot { .. } => demand_spot = true,
                    crate::VsmPageKey::PointFace { face, .. } => {
                        if let Some(slot) = demand_point_faces.get_mut(face as usize) {
                            *slot = true;
                        }
                    }
                    crate::VsmPageKey::Directional { level, x, y } => {
                        let _ = self.vsm_residency.demand_directional(
                            level,
                            x,
                            y,
                            serial,
                            crate::VsmDemandReason::Receiver,
                        );
                    }
                }
            }
        }
        if point_space_dirty {
            let recent = self.vsm_residency.recent_point_faces(serial, 2);
            let mut has_point_face = false;
            for (demanded, recent) in demand_point_faces.iter_mut().zip(recent) {
                *demanded |= recent;
                has_point_face |= *demanded;
            }
            if !has_point_face {
                demand_point_faces.fill(true);
            }
        }
        if demand_spot && self.lighting.spot_shadow_pending() {
            self.vsm_residency
                .demand_spot_space(serial, crate::VsmDemandReason::Receiver);
        }
        if self.lighting.point_shadow_pending() {
            for (face, demanded) in demand_point_faces.into_iter().enumerate() {
                if demanded {
                    self.vsm_residency.demand_point_face(
                        face as u32,
                        serial,
                        crate::VsmDemandReason::Receiver,
                    );
                }
            }
        }
        self.vsm_render_list = self
            .vsm_residency
            .take_render_list(serial, self.vsm_page_budget);
        let table = self
            .vsm_gpu
            .publish_table(&self.device, frame, &self.vsm_residency);
        let mut levels =
            [saffron_geometry::glam::Vec4::ZERO; crate::VSM_DIRECTIONAL_LEVELS as usize];
        for (slot, level) in levels.iter_mut().zip(self.vsm_space.levels.iter()) {
            *slot = saffron_geometry::glam::Vec4::new(
                level.origin_light[0],
                level.origin_light[1],
                level.extent_m,
                0.0,
            );
        }
        self.lighting.set_frame_vsm(
            self.vsm_space.basis,
            levels,
            saffron_geometry::glam::Vec4::new(
                self.vsm_space.center_forward,
                crate::vsm::VSM_DIRECTIONAL_HALF_DEPTH_M,
                1.0,
                0.0,
            ),
            table,
        );
    }

    /// Rasterizes this frame's dirty virtual-shadow pages: per directional level, the level's own
    /// small visibility view culls with the level window (no occlusion history), traversal +
    /// binning build the level's indirect stream, and one atlas pass draws every dirty page into
    /// its tile (clear rect + dynamic viewport/scissor + the page's ortho sub-window push).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn add_vsm_page_passes(
        &mut self,
        graph: &mut RenderGraph,
        frame: usize,
        shadow: &Arc<crate::Pipeline>,
        bindless_set: vk::DescriptorSet,
        instance_set: vk::DescriptorSet,
        deformed_res: Option<RgResource>,
        executor_draws: &[(crate::ExecutorBucket, bool, Arc<crate::Pipeline>)],
        wind_records_res: RgResource,
        cull_pso: &Arc<crate::Pipeline>,
        traversal_pso: &Arc<crate::Pipeline>,
        bin_psos: (
            &Arc<crate::Pipeline>,
            &Arc<crate::Pipeline>,
            &Arc<crate::Pipeline>,
        ),
        micro_template: (u32, u32),
        instance_capacity: u32,
    ) -> Result<Option<RgResource>> {
        if self.vsm_render_list.is_empty() {
            return Ok(None);
        }
        let Some(pyramid) = self.views[self.active_view.index()].hzb_pyramid.as_ref() else {
            // No pyramid to bind as the (never-sampled) cull placeholder; the pages
            // stay staged and re-mark dirty at the next prepare.
            return Ok(None);
        };
        let (previous_image, previous_view) = pyramid.previous();
        let previous_layout = pyramid.previous_layout();
        let hzb_extent = [pyramid.extent().width, pyramid.extent().height];
        let hzb_mips = pyramid.mip_count();
        let list = std::mem::take(&mut self.vsm_render_list);
        let address_slice = (
            self.gpu_scene_uploader.address_buffer(),
            frame as u64 * self.gpu_scene_uploader.address_block_stride(),
            size_of::<crate::GpuSceneAddressBlock>() as u64,
        );
        let atlas_slot = graph.alloc_external_state(self.vsm_gpu.atlas_state);
        let atlas_res = graph.import_image(
            self.vsm_gpu.atlas.handle(),
            self.vsm_gpu.atlas.view(),
            vk::ImageAspectFlags::DEPTH,
            self.vsm_gpu.atlas_state.layout,
            Some(atlas_slot),
        );
        let demand = self.page_demand_view();
        let space = self.vsm_space;
        let spot_view_proj = self.lighting.spot_shadow_view_proj();
        let frame_stamp = self.frame_serial as u32;
        // One group per directional level with staged pages, plus one per dirty
        // punctual block — every group culls with its own frustum. A directional
        // group draws each page into its own tile through the page's ortho
        // sub-window; a block group is ONE draw set over its contiguous block, the
        // space matrix mapping the whole grid through the viewport transform.
        enum VsmGroupDraw {
            Pages(Vec<crate::VsmRenderPage>),
            Block(vk::Rect2D),
        }
        let block_rect = |tile: [u32; 2], side: u32| vk::Rect2D {
            offset: vk::Offset2D {
                x: (tile[0] * crate::VSM_PAGE_SIZE) as i32,
                y: (tile[1] * crate::VSM_PAGE_SIZE) as i32,
            },
            extent: vk::Extent2D {
                width: side * crate::VSM_PAGE_SIZE,
                height: side * crate::VSM_PAGE_SIZE,
            },
        };
        let mut groups: Vec<(usize, Mat4, VsmGroupDraw)> = Vec::new();
        for level in 0..crate::VSM_DIRECTIONAL_LEVELS {
            let level_pages: Vec<crate::VsmRenderPage> = list
                .directional
                .iter()
                .copied()
                .filter(|page| {
                    matches!(page.key, crate::VsmPageKey::Directional { level: l, .. } if l == level)
                })
                .collect();
            if !level_pages.is_empty() {
                groups.push((
                    level as usize,
                    space.level_view_proj(level),
                    VsmGroupDraw::Pages(level_pages),
                ));
            }
        }
        if list.spot {
            groups.push((
                crate::VSM_DIRECTIONAL_LEVELS as usize,
                spot_view_proj,
                VsmGroupDraw::Block(block_rect(
                    crate::vsm::VSM_SPOT_BLOCK_TILE,
                    crate::vsm::VSM_SPOT_PAGES,
                )),
            ));
        }
        let point_faces = crate::point_shadow_face_matrices(
            self.lighting.point_shadow_pos(),
            self.lighting.point_shadow_far(),
        );
        for face in 0..crate::vsm::VSM_POINT_FACES {
            if list.point_faces[face as usize] {
                groups.push((
                    (crate::VSM_DIRECTIONAL_LEVELS + 1 + face) as usize,
                    point_faces[face as usize],
                    VsmGroupDraw::Block(block_rect(
                        crate::vsm::VSM_POINT_FACE_BLOCK_TILES[face as usize],
                        crate::vsm::VSM_POINT_FACE_PAGES,
                    )),
                ));
            }
        }
        let shadow_tuning = self.traversal_tuning(crate::SceneViewClass::ShadowPage);
        for (view_slot, cull_view_proj, group_draw) in groups {
            let needs_view = self.vsm_views[view_slot]
                .as_ref()
                .is_none_or(|view| view.capacity() < instance_capacity);
            if needs_view {
                self.device.wait_idle()?;
                if let Some(mut old) = self.vsm_views[view_slot].take() {
                    old.free_sets(&self.descriptors);
                }
                self.vsm_views[view_slot] = Some(crate::SceneVisibilityView::new(
                    &self.device,
                    &self.descriptors,
                    &self.scene_visibility,
                    instance_capacity,
                    crate::SCENE_VISIBILITY_RECORD_CAPACITY,
                    1,
                )?);
            }
            let Some(view) = self.vsm_views[view_slot].as_ref() else {
                continue;
            };
            view.write_frame_bindings(
                &self.device,
                &self.scene_visibility,
                frame,
                previous_view,
                previous_view,
                address_slice,
            );
            // The frame's bucket vocabulary is shared with the camera view; the
            // level's bin passes need the same table.
            let (_, bucket_table) = crate::build_executor_buckets(
                &self.live_executor_bins,
                crate::SCENE_VISIBILITY_RECORD_CAPACITY,
                self.displaced_frame.is_some(),
            );
            view.write_bucket_table(frame, &bucket_table);
            let previous_res = graph.import_image(
                previous_image,
                previous_view,
                vk::ImageAspectFlags::COLOR,
                previous_layout,
                None,
            );
            let level_view_proj = cull_view_proj.to_cols_array();
            view.add_cull_pass(
                &self.device,
                graph,
                cull_pso,
                frame,
                previous_res,
                wind_records_res,
                instance_capacity,
                crate::SceneVisibilityPush {
                    view_proj: level_view_proj,
                    prev_view_proj: level_view_proj,
                    hzb_extent,
                    hzb_mip_count: hzb_mips,
                    pass_kind: crate::SCENE_VISIBILITY_PASS_CULL,
                    history_valid: 0,
                    list_capacity: view.capacity(),
                    reserved: [0; 2],
                    reach_min: [0.0; 4],
                    reach_max: [0.0; 4],
                },
            );
            view.add_traversal_pass(
                &self.device,
                graph,
                traversal_pso,
                frame,
                crate::SceneTraversalPush {
                    view_proj: level_view_proj,
                    eye: demand.eye.to_array(),
                    proj_scale: demand.proj_scale,
                    error_threshold_px: shadow_tuning.error_threshold_px,
                    record_capacity: view.record_capacity(),
                    list_capacity: view.capacity(),
                    survivor: 0,
                    displaced_records: 0,
                    // Shadow pages draw settled cuts: a nonzero value here would let
                    // every page view mutate the shared flip-state table with its own
                    // refine decisions and fabricate camera-view crossfades.
                    transition_frames: 0,
                    frame_stamp,
                    representation_override: shadow_tuning.representation_override,
                    node_cull: u32::from(self.node_cull),
                    demand_only: 0,
                    view_class: crate::SceneViewClass::ShadowPage.ordinal(),
                },
            );
            view.add_binning_passes(
                &self.device,
                graph,
                bin_psos,
                frame,
                false,
                micro_template,
                None,
            );

            // A shadow page draws the undisplaced surface, so no displaced bucket has records
            // and no arena index stream is bound.
            let inputs =
                view.executor_draw_inputs(frame, self.live_draw_record_bound, vk::Buffer::null());
            let raw_body = self.device.raw().clone();
            let shadow_pipeline = shadow.handle();
            let shadow_layout = shadow.layout();
            let shadow_keep = Arc::clone(shadow);
            let draws = executor_draws.to_vec();
            let pages_buffer = self.global_gpu_data.pages.buffer();
            let draw_count_supported = self.device.capabilities.draw_indirect_count;
            let extent = vk::Extent2D {
                width: crate::VSM_ATLAS_SIZE,
                height: crate::VSM_ATLAS_SIZE,
            };
            let pass = RgPass::graphics("vsm-pages", extent).depth_attachment(RgAttachment {
                resource: atlas_res,
                load_op: vk::AttachmentLoadOp::LOAD,
                store_op: vk::AttachmentStoreOp::STORE,
                clear_value: vk::ClearValue::default(),
                resolve: None,
            });
            let pass_inputs = inputs;
            let group_view_proj = cull_view_proj;
            let mut pass = pass.body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                // SAFETY: the ash seam; `cmd` is recording inside the pass.
                unsafe {
                    raw_body.cmd_set_depth_bias(
                        cmd,
                        crate::lighting::SHADOW_DEPTH_BIAS_CONSTANT,
                        0.0,
                        crate::lighting::SHADOW_DEPTH_BIAS_SLOPE,
                    );
                }
                let draw_rect = |rect: vk::Rect2D, view_proj: Mat4| {
                    let viewport = vk::Viewport {
                        x: rect.offset.x as f32,
                        y: rect.offset.y as f32,
                        width: rect.extent.width as f32,
                        height: rect.extent.height as f32,
                        min_depth: 0.0,
                        max_depth: 1.0,
                    };
                    let clear = vk::ClearAttachment {
                        aspect_mask: vk::ImageAspectFlags::DEPTH,
                        color_attachment: 0,
                        clear_value: vk::ClearValue {
                            depth_stencil: vk::ClearDepthStencilValue {
                                depth: 1.0,
                                stencil: 0,
                            },
                        },
                    };
                    let clear_rect = vk::ClearRect {
                        rect,
                        base_array_layer: 0,
                        layer_count: 1,
                    };
                    // SAFETY: the ash seam; the rect lies inside the atlas attachment.
                    unsafe {
                        raw_body.cmd_set_viewport(cmd, 0, &[viewport]);
                        raw_body.cmd_set_scissor(cmd, 0, &[rect]);
                        raw_body.cmd_clear_attachments(cmd, &[clear], &[clear_rect]);
                    }
                    record_executor_depth_family(
                        &raw_body,
                        cmd,
                        (shadow_pipeline, shadow_layout),
                        vk::ShaderStageFlags::VERTEX,
                        bytemuck::bytes_of(&view_proj),
                        bindless_set,
                        instance_set,
                        pass_inputs,
                        pages_buffer,
                        draw_count_supported,
                        &draws,
                        false,
                    );
                };
                match &group_draw {
                    VsmGroupDraw::Pages(pages) => {
                        for page in pages {
                            let crate::VsmPageKey::Directional { level, x, y } = page.key else {
                                continue;
                            };
                            let tile = vk::Rect2D {
                                offset: vk::Offset2D {
                                    x: ((page.tile % crate::VSM_ATLAS_TILES) * crate::VSM_PAGE_SIZE)
                                        as i32,
                                    y: ((page.tile / crate::VSM_ATLAS_TILES) * crate::VSM_PAGE_SIZE)
                                        as i32,
                                },
                                extent: vk::Extent2D {
                                    width: crate::VSM_PAGE_SIZE,
                                    height: crate::VSM_PAGE_SIZE,
                                },
                            };
                            draw_rect(tile, space.page_view_proj(level, x, y));
                        }
                    }
                    VsmGroupDraw::Block(rect) => draw_rect(*rect, group_view_proj),
                }
                drop(shadow_keep);
            });
            let pages_res = graph.import_buffer(pages_buffer, None);
            let commands_res = graph.import_buffer(inputs.commands, None);
            let counters_res = graph.import_buffer(inputs.counters, None);
            let bucket_counts_res = graph.import_buffer(inputs.bucket_counts, None);
            pass = pass
                .access(pages_res, RgUsage::IndexInputRead)
                .access(commands_res, RgUsage::IndirectCommandRead)
                .access(counters_res, RgUsage::IndirectCountRead)
                .access(bucket_counts_res, RgUsage::IndirectCountRead)
                .access(wind_records_res, RgUsage::ShaderDeviceAddressRead);
            let micro_candidates_res =
                graph.import_buffer(self.global_gpu_data.micro_candidates.handle(), None);
            pass = pass.access(micro_candidates_res, RgUsage::ShaderDeviceAddressRead);
            if let Some(deformed) = deformed_res {
                pass = pass.access(deformed, RgUsage::ShaderDeviceAddressRead);
            }
            graph.add_pass(pass);
        }
        Ok(Some(atlas_res))
    }

    /// Shadow pages the frame may render.
    #[must_use]
    pub fn vsm_page_budget(&self) -> usize {
        self.vsm_page_budget
    }

    /// Sets the per-frame shadow-page render budget, clamped to at least one: zero would stall the
    /// atlas permanently rather than throttle it.
    pub fn set_vsm_page_budget(&mut self, budget: usize) {
        self.vsm_page_budget = budget.max(1);
    }

    fn active_world_has_wind_instances(&self) -> bool {
        self.persistent_gpu_scene
            .instances(self.active_view.gpu_scene_world())
            .is_ok_and(|mut instances| {
                instances.any(|(_, record)| record.flags & crate::GPU_SCENE_INSTANCE_FLAG_WIND != 0)
            })
    }

    /// Marks the virtual-shadow pages the freshly seeded HZB depth demands, then compacts the
    /// bitmap into this frame's request ring for the fence-time drain.
    pub(super) fn add_vsm_demand_passes(
        &mut self,
        graph: &mut RenderGraph,
        frame: usize,
        mark_pso: Option<&Arc<crate::Pipeline>>,
        compact_pso: Option<&Arc<crate::Pipeline>>,
        hzb_res: Option<RgResource>,
    ) {
        // Receiver demand for the virtual shadow map: mark needed pages from
        // the freshly seeded pyramid's full-resolution depth, then compact the
        // bitmap into the frame's request ring for the fence-time drain.
        if self.active_view.index() == 0
            && let (Some(mark_pso), Some(compact_pso), Some(hzb_res)) =
                (mark_pso, compact_pso, hzb_res)
            && let Some(pyramid) = self.views[self.active_view.index()].hzb_pyramid.as_ref()
        {
            let (_, current_view) = pyramid.current();
            self.vsm_demand.write_frame(
                &self.device.raw().clone(),
                frame,
                self.lighting.frame_ubo(frame),
                current_view,
                self.scene_visibility.hzb_sampler(),
            );
            let extent = self.views[self.active_view.index()].scaled_render_extent();
            let inv_view_proj = self.scene_view_proj_unjittered().inverse();
            let (mark_set, compact_set) = self.vsm_demand.sets(frame);
            let raw_mark = self.device.raw().clone();
            let mark = Arc::clone(mark_pso);
            let mark_push = crate::VsmDemandPush {
                inv_view_proj: inv_view_proj.to_cols_array(),
                extent: [extent.width, extent.height],
                reserved: [0; 2],
            };
            let groups = (extent.width.div_ceil(8), extent.height.div_ceil(8));
            graph.add_pass(
                RgPass::compute("vsm-demand")
                    .access(hzb_res, RgUsage::StorageImageRwCompute)
                    .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                        // SAFETY: the ash seam; the PSO/set are valid this frame.
                        unsafe {
                            raw_mark.cmd_bind_pipeline(
                                cmd,
                                vk::PipelineBindPoint::COMPUTE,
                                mark.handle(),
                            );
                            raw_mark.cmd_bind_descriptor_sets(
                                cmd,
                                vk::PipelineBindPoint::COMPUTE,
                                mark.layout(),
                                0,
                                &[mark_set],
                                &[],
                            );
                            raw_mark.cmd_push_constants(
                                cmd,
                                mark.layout(),
                                vk::ShaderStageFlags::COMPUTE,
                                0,
                                bytemuck::bytes_of(&mark_push),
                            );
                            raw_mark.cmd_dispatch(cmd, groups.0, groups.1, 1);
                        }
                    }),
            );
            let raw_compact = self.device.raw().clone();
            let compact = Arc::clone(compact_pso);
            let compact_push = crate::VsmCompactPush {
                capacity: crate::VSM_DEMAND_CAPACITY,
                reserved: [0; 3],
            };
            let bitmap_res = graph.import_buffer(self.vsm_demand.bitmap_handle(), None);
            let ring_res = graph.import_buffer(self.vsm_demand.ring_handle(frame), None);
            graph.add_pass(
                RgPass::compute("vsm-demand-compact")
                    .access(bitmap_res, RgUsage::StorageReadWriteCompute)
                    .access(ring_res, RgUsage::StorageReadWriteCompute)
                    .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                        // SAFETY: the ash seam; the PSO/set are valid this frame.
                        unsafe {
                            raw_compact.cmd_bind_pipeline(
                                cmd,
                                vk::PipelineBindPoint::COMPUTE,
                                compact.handle(),
                            );
                            raw_compact.cmd_bind_descriptor_sets(
                                cmd,
                                vk::PipelineBindPoint::COMPUTE,
                                compact.layout(),
                                0,
                                &[compact_set],
                                &[],
                            );
                            raw_compact.cmd_push_constants(
                                cmd,
                                compact.layout(),
                                vk::ShaderStageFlags::COMPUTE,
                                0,
                                bytemuck::bytes_of(&compact_push),
                            );
                            raw_compact.cmd_dispatch(cmd, 8, 1, 1);
                        }
                    }),
            );
        }
    }
}
