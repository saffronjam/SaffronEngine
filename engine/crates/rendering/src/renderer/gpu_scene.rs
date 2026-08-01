use super::*;

impl Renderer {
    /// Device-global geometry arenas and immutable metadata tables.
    pub fn global_gpu_data(&self) -> &crate::GlobalGpuData {
        &self.global_gpu_data
    }

    /// Mutable device-global tables used by the asset delta adapter.
    pub fn global_gpu_data_mut(&mut self) -> &mut crate::GlobalGpuData {
        &mut self.global_gpu_data
    }

    /// Descriptor-ready immutable-table bindings for visibility and draw executors.
    pub fn global_gpu_table_descriptors(&self) -> crate::GlobalGpuTableDescriptors {
        self.global_gpu_data.table_descriptors(&self.device)
    }

    /// Sole persistent renderer-derived scene mirror.
    pub fn persistent_gpu_scene(&self) -> &crate::PersistentGpuScene {
        &self.persistent_gpu_scene
    }

    /// Mutable scene mirror used by typed world and asset delta adapters.
    pub fn persistent_gpu_scene_mut(&mut self) -> &mut crate::PersistentGpuScene {
        &mut self.persistent_gpu_scene
    }

    /// The latest fence-completed visibility counters for the active view:
    /// `[visible, retest, list overflow flags, records, record/bucket pressure,
    /// transparent, _, _]`.
    pub fn visibility_counters(&self) -> [u32; crate::SCENE_VISIBILITY_COUNTER_WORDS as usize] {
        self.visibility_counters
    }

    /// The same, for the global-illumination reach view; all zero while the distance field is off.
    pub fn gi_visibility_counters(&self) -> [u32; crate::SCENE_VISIBILITY_COUNTER_WORDS as usize] {
        self.gi_visibility_counters
    }

    /// Replaces the budget breaches the alarm tick raises alongside its own detectors.
    ///
    /// The set is complete rather than incremental, so the caller publishes every frame with an
    /// empty list when nothing is over budget; a breach that stops being reported resolves.
    pub fn set_owned_budgets(&mut self, breaches: Vec<crate::OwnedBudgetBreach>) {
        self.owned_budgets = breaches;
    }

    /// Deformed instances the interaction field's re-centring scroll has reset since boot.
    pub fn wind_interaction_resets(&self) -> u64 {
        self.wind_interaction_resets
    }

    /// GPU missing-page requests drained since startup (the page-fault total).
    pub fn page_faults(&self) -> u64 {
        self.page_faults
    }

    /// Replaces the populated executor bin set (the mirror pushes it per sync).
    pub fn set_live_executor_bins(&mut self, bins: Vec<(u32, u32)>) {
        self.live_executor_bins = bins;
    }

    /// Publishes the mirror's upper bound on emitted draw records, which bounds the fixed-slice
    /// indirect draws on a device without `drawIndirectCount`.
    pub fn set_live_draw_record_bound(&mut self, bound: u32) {
        self.live_draw_record_bound = bound;
    }

    /// The page-payload residency counters (registered/resident/bytes/evictions).
    pub fn page_residency_stats(&self) -> crate::PageResidencyStats {
        self.page_residency.stats()
    }

    /// Missing-page requests one view class may append per frame.
    #[must_use]
    pub fn page_request_budget(&self) -> u32 {
        self.gpu_scene_uploader.page_request_budget()
    }

    /// Sets that budget, clamped to a usable region.
    ///
    /// A scene big enough to fill a 4096-entry region from one class is not one a test can
    /// conjure, so the overflow path is unreachable from outside without this.
    pub fn set_page_request_budget(&mut self, entries: u32) {
        self.gpu_scene_uploader.set_page_request_budget(entries);
    }

    /// The active view's page-demand context: the eye for projected error, the
    /// projection scale (pixels per metre at unit distance), and the view-projection
    /// for frustum visibility probability.
    pub fn page_demand_view(&self) -> crate::PageDemandView {
        let view = self.ssao.view();
        let inv_projection = self.ssao.inv_projection();
        let extent = self.views[self.active_view.index()].scaled_render_extent();
        let inv_scale = inv_projection.col(1).y;
        let proj_scale = if inv_scale.abs() > f32::EPSILON {
            (1.0 / inv_scale).abs() * extent.height as f32 * 0.5
        } else {
            0.0
        };
        let eye = view.inverse().col(3).truncate();
        let (gi_min, gi_max) = crate::gi_occluder_bounds(eye);
        crate::PageDemandView {
            eye,
            proj_scale,
            view_proj: inv_projection.inverse() * view,
            gi_min,
            gi_max,
            frame_seconds: self.frame_ms / 1000.0,
        }
    }

    /// The GPU-scene halves the delta adapter writes in one borrow: device tables for
    /// record inserts, the persistent mirror for typed deltas, the pending-upload queue
    /// for staged bytes, retirements, and arena data, and the page-residency authority.
    pub fn gpu_scene_parts_mut(
        &mut self,
    ) -> (
        &mut crate::GlobalGpuData,
        &mut crate::PersistentGpuScene,
        &mut crate::GpuScenePendingUploads,
        &mut crate::PageResidency,
    ) {
        (
            &mut self.global_gpu_data,
            &mut self.persistent_gpu_scene,
            &mut self.pending_gpu_scene_uploads,
            &mut self.page_residency,
        )
    }

    /// The persistent GPU scene's device tables and upload translation.
    pub fn gpu_scene_uploader(&self) -> &crate::GpuSceneUploader {
        &self.gpu_scene_uploader
    }

    /// Publishes the resident micro-field tile directory (byte offset within the
    /// fields arena + entry count) the micro reconstruction pass dispatches over,
    /// or `None` while no field tiles are resident.
    pub fn set_micro_field_directory(&mut self, directory: Option<(u32, u32)>) {
        // `SAFFRON_MICRO_FIELD=off` suppresses the reconstructed blade passes so a test can observe
        // an aggregate plant alone: micro blades scatter across the whole ground plane, so no
        // screen region contains one without the other.
        self.micro_field_directory = if self.micro_field_enabled {
            directory
        } else {
            None
        };
    }

    /// The most recent frame's GPU-scene upload-translation counters.
    pub fn gpu_scene_upload_stats(&self) -> crate::GpuSceneUploadRunStats {
        self.last_gpu_scene_upload
    }

    /// The active view's GPU-scene world identity.
    pub fn active_gpu_scene_world(&self) -> crate::GpuSceneWorldId {
        self.active_view.gpu_scene_world()
    }

    /// The record-driven deformation frame: uploads the palettes and wires the skin / morph /
    /// tessellation work for `work`. The outputs land on the frame's [`FrameDeformation`];
    /// the draws come from the GPU scene's visibility traversal.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error`] on buffer growth or dispatch wiring failure.
    pub fn submit_gpu_scene_deformations(
        &mut self,
        view_proj: Mat4,
        work: &[crate::DeformationWork],
        joints: &[Mat4],
    ) -> Result<()> {
        let frame = self.frames.index();
        let mut gather = crate::DeformationGather::default();
        let mut prev_joints: Vec<Mat4> = joints.to_vec();
        let params = crate::TessGatherParams {
            rt_skinned: self.rt.use_rt_shadows() || self.rt.use_rt_reflections(),
            factor_cap: self.tess_factor_cap,
            min_factor: self.tess_min_factor,
            edge_length_target: self.tess_edge_length_target,
        };
        for item in work {
            crate::gather_instance_deformation(
                &mut gather,
                &mut self.skinning,
                item,
                joints,
                &mut prev_joints,
                params,
            );
        }
        let mut list = FrameDeformation {
            view_proj,
            ..FrameDeformation::default()
        };
        // Planned before the wiring, so the arena is sized once for skinning, morph, and the
        // materialized wind slices together: growing it afterwards would replace the buffer the
        // wiring has already written into the skin and morph descriptor sets.
        let rt_plan = params.rt_skinned.then(|| {
            crate::plan_wind_deformation(
                self.rt.scene_instances(),
                &self.rt_cut_view(),
                self.scene_wind.speed,
                gather.deformed_cursor,
            )
        });
        let rt_plan = rt_plan.flatten();
        if let Some(plan) = &rt_plan {
            gather.deformed_cursor = plan.high_water;
        }
        self.instancing.wire_gathered_deformations(
            &mut self.skinning,
            frame,
            gather,
            joints,
            prev_joints,
            &mut list,
        )?;
        self.rt_deform_jobs.clear();
        if let Some(plan) = rt_plan {
            self.adopt_rt_deformation(frame, plan, &mut list)?;
        }
        list.valid = true;
        self.frame_deformation = list;
        Ok(())
    }

    /// Adopts the frame's wind materialization: sizes the arena for the slices, resolves each
    /// dispatch's addresses, and hands the deforming instances to the refit path.
    ///
    /// The materialized instances leave the captured static scene: a plant placed in both would
    /// appear twice, once swaying and once at rest.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error`] if the deformed arena cannot grow to hold the plan.
    fn adopt_rt_deformation(
        &mut self,
        frame: usize,
        mut plan: crate::RtDeformPlan,
        list: &mut FrameDeformation,
    ) -> Result<()> {
        // A no-op whenever skinning or morph already sized the arena to the plan's high-water; the
        // create for a frame whose only deformation is materialized wind.
        self.skinning
            .ensure_deformed_buffers(frame, plan.high_water)?;
        let Some(deformed) = self.skinning.deformed_buffer(frame) else {
            return Ok(());
        };
        crate::resolve_job_addresses(&mut plan, &self.device, deformed);
        self.rt
            .set_rt_scene(std::sync::Arc::from(std::mem::take(&mut plan.statics)));
        list.deformed_rt_instances.extend(plan.instances);
        self.rt_deform_jobs = plan.jobs;
        Ok(())
    }

    /// The camera cut parameters ray-representation selection projects through — the same eye,
    /// projection scale, threshold, and override the raster traversal's refine test uses.
    pub(super) fn rt_cut_view(&self) -> crate::RtCutView {
        let demand = self.page_demand_view();
        let tuning = self.traversal_tuning(crate::SceneViewClass::Camera);
        crate::RtCutView {
            eye: demand.eye.to_array(),
            proj_scale: demand.proj_scale,
            error_threshold_px: tuning.error_threshold_px,
            representation_override: tuning.representation_override,
        }
    }

    /// Records the retained-mesh host-byte figure the mirror reports (render stats).
    pub fn record_retained_mesh_bytes(&mut self, bytes: u64) {
        self.stats.retained_mesh_cpu_bytes = bytes;
    }

    /// The submitted frame's skinned palette/deformed offsets (the provider-params
    /// patch input).
    pub fn skinned_deformations(&self) -> &[crate::SkinnedDeformation] {
        &self.frame_deformation.skinned_deformations
    }

    /// `view`'s pinned hierarchy cut, or [`crate::SCENE_CUT_AUTO`] when projected error chooses
    /// it. The ray gather reads the same override the camera traversal is pushed, so pinning the
    /// cut pins both representations rather than leaving rays on triangles the raster dropped.
    #[must_use]
    pub fn cut_override(&self, view: crate::SceneViewClass) -> u32 {
        self.cut_override[view.ordinal() as usize]
    }

    /// Pins `view`'s hierarchy cut, or returns it to following projected error.
    ///
    /// An author comparing a plant's triangle and aggregate forms needs the cut to move while the
    /// camera holds still: flying out shrinks the subject at the same time and conflates the two.
    pub fn set_cut_override(&mut self, view: crate::SceneViewClass, cut: u32) {
        self.cut_override[view.ordinal() as usize] = match cut {
            crate::SCENE_CUT_FORCE_COARSE => crate::SCENE_CUT_FORCE_COARSE,
            crate::SCENE_CUT_FORCE_FINE => crate::SCENE_CUT_FORCE_FINE,
            // Anything else follows the error threshold, which is the only safe reading of a value
            // the traversal would otherwise compare against constants it does not know.
            _ => crate::SCENE_CUT_AUTO,
        };
    }

    /// How `view`'s hierarchy walk refines: the projected-error threshold it refines under and the
    /// cut it is pinned to. The classes read the same scene for different ends, and a walk tuned
    /// for the image is the wrong walk for anything that is not the image.
    #[must_use]
    pub fn traversal_tuning(&self, view: crate::SceneViewClass) -> crate::TraversalTuning {
        crate::TraversalTuning {
            error_threshold_px: match view {
                crate::SceneViewClass::Camera | crate::SceneViewClass::ShadowPage => {
                    crate::SCENE_ERROR_THRESHOLD_IMAGE_PX
                }
                crate::SceneViewClass::Gi => crate::SCENE_ERROR_THRESHOLD_GI_PX,
            },
            representation_override: self.cut_override(view),
        }
    }
}
