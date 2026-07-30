use super::*;

impl Renderer {
    /// Records ray instances the cascade-window gate excluded this frame.
    ///
    /// Named apart from a drop: culling is a claim about REACH, and an instance lost to capacity
    /// is a different event that must not be counted here.
    pub fn record_rt_culled(&mut self, culled: u32) {
        self.rt_instances_culled = culled;
    }

    /// Ray instances the cascade-window gate excluded this frame.
    #[must_use]
    pub fn rt_instances_culled(&self) -> u32 {
        self.rt_instances_culled
    }

    /// Occluders the cascade-window gate excluded this frame — the ones that provably cannot
    /// affect any march, as opposed to the ones dropped for want of capacity.
    pub fn sdf_instances_culled(&self) -> u32 {
        self.sdf_instances_culled
    }

    /// Occluders the scatter dropped this frame for want of capacity.
    ///
    /// Past the cap, occluders vanish from GI with nothing to coarsen into.
    pub fn sdf_instances_dropped(&self) -> u32 {
        self.sdf_instances_dropped
    }

    /// Whether the device supports hardware ray tracing (acceleration-structure + ray-query).
    pub fn rt_supported(&self) -> bool {
        self.rt.supported()
    }

    /// Toggles inline ray-query shadows (clamped off on a non-RT device). When off, the
    /// `tlas-build` pass is skipped and the mesh fragment takes the shadow-map path.
    pub fn set_rt_shadows(&mut self, enabled: bool) {
        self.rt.set_rt_shadows(enabled);
    }

    /// Whether ray-query shadows ran this frame (toggle on, RT supported, TLAS built).
    pub fn rt_shadows_enabled(&self) -> bool {
        self.rt.shadows_enabled()
    }

    /// Toggles inline ray-query reflections (clamped off on a non-RT device). When off, the
    /// mesh fragment keeps the SSR / prefiltered-env reflection path.
    pub fn set_rt_reflections(&mut self, enabled: bool) {
        self.rt.set_rt_reflections(enabled);
    }

    /// Whether the ray-query-reflections toggle is on (independent of TLAS readiness).
    pub fn rt_reflections_enabled(&self) -> bool {
        self.rt.use_rt_reflections()
    }

    /// The built per-mesh BLAS count (rt-stats).
    pub fn rt_blas_count(&self) -> u32 {
        self.rt.blas_count()
    }

    /// The skinned refit BLAS active this frame (rt-stats).
    pub fn rt_skinned_blas_count(&self) -> u32 {
        self.rt.skinned_blas_count()
    }

    /// The TLAS instance count produced by this frame's build (static + skinned).
    pub fn rt_frame_instance_count(&self) -> u32 {
        self.rt.frame_instance_count()
    }

    /// Whether opacity micromaps can be attached to triangle geometry on this device.
    pub fn omm_supported(&self) -> bool {
        self.device.omm_supported()
    }

    /// The tessellated full-rebuild BLAS active this frame (rt-stats).
    pub fn rt_tessellated_blas_count(&self) -> u32 {
        self.rt.tessellated_blas_count()
    }

    /// Whether cluster acceleration structures are enabled on this device.
    pub fn cluster_as_supported(&self) -> bool {
        self.device.cluster_as_supported()
    }

    /// Distinct cluster-composed bottom-level structures referenced this frame.
    pub fn rt_cluster_blas_count(&self) -> u32 {
        self.rt.cluster_blas_count()
    }

    /// Cluster acceleration structures those bottom levels compose.
    pub fn rt_clas_count(&self) -> u32 {
        self.rt.clas_count()
    }

    /// Whether the top-level structure is partitioned on this device.
    pub fn ptlas_supported(&self) -> bool {
        self.device.ptlas_supported()
    }

    /// The partitioned structure's last build as `(partitions, writes, updates)`; all zero
    /// where the top level is the KHR TLAS.
    pub fn rt_ptlas_ops(&self) -> (u32, u32, u32) {
        self.rt.ptlas_stats().map_or((0, 0, 0), |stats| {
            (stats.partitions, stats.writes, stats.updates)
        })
    }

    /// AS-storage bytes the distinct bottom-level structures occupy this frame (rt-stats).
    pub fn rt_blas_bytes(&self) -> u64 {
        self.rt.blas_bytes()
    }

    /// Cumulative GPU microseconds in out-of-graph structure builds and compactions (rt-stats).
    ///
    /// The render graph times the per-frame refits and the TLAS build as named scopes; this covers
    /// the two that run outside it on the uploader's private pool: the initial static build and
    /// its compaction, which scale with content rather than with frame rate.
    #[must_use]
    pub fn rt_accel_build_us(&self) -> u64 {
        self.device.resources().accel_build_nanos() / 1_000
    }

    /// Distinct opacity micromaps this frame's structures reference (rt-stats).
    pub fn rt_omm_micromaps(&self) -> u32 {
        self.rt.omm_micromaps()
    }

    /// Micro-triangles settled opaque, settled transparent, and left unknown (rt-stats).
    #[must_use]
    pub fn rt_omm_classes(&self) -> (u64, u64, u64) {
        self.rt.omm_classes()
    }

    /// What those structures would occupy uncompacted (rt-stats).
    pub fn rt_blas_built_bytes(&self) -> u64 {
        self.rt.blas_built_bytes()
    }

    /// AS-storage bytes this frame's top-level structure occupies (rt-stats).
    pub fn rt_tlas_bytes(&self) -> u64 {
        self.rt.tlas_bytes()
    }

    /// Build-scratch bytes held for this frame's TLAS + BLAS builds (rt-stats).
    pub fn rt_scratch_bytes(&self) -> u64 {
        self.rt.scratch_bytes()
    }

    /// Captures this frame's static mesh instances (parallel world transforms + meshes) for
    /// the `tlas-build` pass, arming the build when RT shadows are on. Skinned instances
    /// ride the frame's deformation state.
    pub fn set_rt_scene(&mut self, instances: std::sync::Arc<[crate::RtInstanceInput]>) {
        self.rt.set_rt_scene(instances);
    }

    /// Drops every per-slot skinned refit BLAS (e.g. on a scene reset).
    pub fn clear_rt_skinned_blas(&mut self) {
        self.rt.clear_skinned_blas();
    }

    /// Toggles ReSTIR DI many-light direct lighting (clamped off on a non-RT device, since the
    /// resolve needs ray-query). Turning it on re-converges the reservoirs from scratch by arming
    /// the active view's temporal reset; when off, direct lighting falls back to clustered.
    pub fn set_restir(&mut self, enabled: bool) {
        // The gate ANDs `rt_supported && active_restir.ready`; the supported half lives on
        // `Restir`, the view-ready half on the active `RestirView`.
        let ready = self.views[self.active_view.index()].restir.ready();
        let armed = self.restir.set_enabled(enabled && ready);
        if armed {
            self.views[self.active_view.index()].restir.reset_history();
        }
    }

    /// Whether ReSTIR is on, the device supports it, and the active view's reservoirs are
    /// built (the mesh-sample gate).
    pub fn restir_enabled(&self) -> bool {
        self.restir.use_restir()
            && self.restir.supported()
            && self.views[self.active_view.index()].restir.ready()
    }
}
