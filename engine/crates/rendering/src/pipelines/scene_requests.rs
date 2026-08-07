//! Compute PSO front doors for the GPU-driven scene: visibility, traversal, binning, the
//! radix sort, virtual shadow-map demand, wind, tessellation, and skinning.

use super::*;

impl Pipelines {
    /// The clustered light-cull compute PSO. Binds the cluster compute set (set 0: params UBO +
    /// light list + cluster lists) and dispatches one invocation per froxel.
    pub fn request_light_cull(&mut self) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.light_cull {
            return Some(Arc::clone(pipeline));
        }
        match self.build_compute("shaders/light_cull.spv", self.cluster_set_layout, 0) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.light_cull = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_light_cull: {err}");
                None
            }
        }
    }

    /// The compute skinning PSO. Binds the skin set layout (four storage buffers: static vertices,
    /// skin, palette, deformed output) + a 16-byte push, and deforms one vertex per invocation.
    /// `skin_set_layout` is owned by [`crate::skinning::Skinning`].
    pub fn request_skin(
        &mut self,
        skin_set_layout: vk::DescriptorSetLayout,
    ) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.skin {
            return Some(Arc::clone(pipeline));
        }
        match self.build_compute("shaders/skin.spv", skin_set_layout, 16) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.skin = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_skin: {err}");
                None
            }
        }
    }

    /// The compute morph PSO (morph set layout, a 20-byte `MorphPush`).
    pub fn request_morph(
        &mut self,
        morph_set_layout: vk::DescriptorSetLayout,
    ) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.morph {
            return Some(Arc::clone(pipeline));
        }
        match self.build_compute("shaders/morph.spv", morph_set_layout, 24) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.morph = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_morph: {err}");
                None
            }
        }
    }

    /// The adaptive-tessellation factor PSO (bindless set 0 for the min/max pyramid + the edge
    /// storage-buffer set 1, a 128-byte push: mvp(64) + camPosLocal(16) + viewport(8) + 10
    /// scalars(40)).
    pub fn request_tess_factor(
        &mut self,
        bindless: vk::DescriptorSetLayout,
        factor: vk::DescriptorSetLayout,
    ) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.tess_factor {
            return Some(Arc::clone(pipeline));
        }
        match self.build_compute_multi("shaders/tess_factor.spv", &[bindless, factor], 128) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.tess_factor = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_tess_factor: {err}");
                None
            }
        }
    }

    /// The predict/scan PSO (5 storage buffers, a 32-byte push).
    pub fn request_tess_scan(&mut self, layout: vk::DescriptorSetLayout) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.tess_scan {
            return Some(Arc::clone(pipeline));
        }
        match self.build_compute_multi("shaders/tess_scan.spv", &[layout], 32) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.tess_scan = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_tess_scan: {err}");
                None
            }
        }
    }

    /// The per-instance finalize PSO (3 storage buffers, a 32-byte push).
    pub fn request_tess_finalize(
        &mut self,
        layout: vk::DescriptorSetLayout,
    ) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.tess_finalize {
            return Some(Arc::clone(pipeline));
        }
        match self.build_compute_multi("shaders/tess_finalize.spv", &[layout], 32) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.tess_finalize = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_tess_finalize: {err}");
                None
            }
        }
    }

    /// The global dispatch-args PSO (2 storage buffers, no push).
    pub fn request_tess_args(&mut self, layout: vk::DescriptorSetLayout) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.tess_args {
            return Some(Arc::clone(pipeline));
        }
        match self.build_compute_multi("shaders/tess_args.spv", &[layout], 0) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.tess_args = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_tess_args: {err}");
                None
            }
        }
    }

    /// The amplifying emit PSO (`tessellate.spv`, bindless set 0 + the emit set 1, a 64-byte push).
    pub fn request_tessellate(
        &mut self,
        bindless: vk::DescriptorSetLayout,
        emit: vk::DescriptorSetLayout,
    ) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.tessellate {
            return Some(Arc::clone(pipeline));
        }
        match self.build_compute_multi("shaders/tessellate.spv", &[bindless, emit], 64) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.tessellate = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_tessellate: {err}");
                None
            }
        }
    }

    /// The instance-visibility compute PSO (lists + history + HZB + address block, a
    /// 160-byte push). One pipeline serves the cull and retest pass kinds.
    pub fn request_scene_visibility(
        &mut self,
        layout: vk::DescriptorSetLayout,
    ) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.scene_visibility {
            return Some(Arc::clone(pipeline));
        }
        match self.build_compute(
            "shaders/scene_visibility.spv",
            layout,
            crate::SCENE_VISIBILITY_PUSH_SIZE,
        ) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.scene_visibility = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_scene_visibility: {err}");
                None
            }
        }
    }

    /// The wind deformation prepass PSO (the visibility set layout for its address
    /// block, a [`crate::WIND_DEFORM_PUSH_SIZE`]-byte push of the frame's wind words).
    pub fn request_wind_deform(
        &mut self,
        layout: vk::DescriptorSetLayout,
    ) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.wind_deform {
            return Some(Arc::clone(pipeline));
        }
        match self.build_compute(
            "shaders/wind_deform.spv",
            layout,
            crate::WIND_DEFORM_PUSH_SIZE,
        ) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.wind_deform = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_wind_deform: {err}");
                None
            }
        }
    }

    /// The ray-geometry materialization PSO (the visibility set layout for its address block, a
    /// [`crate::RtDeformPush`]-sized push naming one placed use's slice).
    pub fn request_rt_deform(&mut self, layout: vk::DescriptorSetLayout) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.rt_deform {
            return Some(Arc::clone(pipeline));
        }
        match self.build_compute(
            "shaders/rt_deform.spv",
            layout,
            size_of::<crate::RtDeformPush>() as u32,
        ) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.rt_deform = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_rt_deform: {err}");
                None
            }
        }
    }

    /// The micro-blade ray-geometry materialization PSO: the micro-field pass family's set layout
    /// (the address block it resolves the tile directory through), a
    /// [`crate::MICRO_RT_DEFORM_PUSH_SIZE`]-byte push.
    pub fn request_micro_rt_deform(
        &mut self,
        layout: vk::DescriptorSetLayout,
    ) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.micro_rt_deform {
            return Some(Arc::clone(pipeline));
        }
        match self.build_compute(
            "shaders/micro_rt_deform.spv",
            layout,
            crate::MICRO_RT_DEFORM_PUSH_SIZE,
        ) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.micro_rt_deform = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_micro_rt_deform: {err}");
                None
            }
        }
    }

    /// The interaction-field step PSO (BDA-only, a
    /// [`crate::WIND_INTERACT_PUSH_SIZE`]-byte push of addresses, centres, and dt).
    pub fn request_wind_interact(
        &mut self,
        layout: vk::DescriptorSetLayout,
    ) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.wind_interact {
            return Some(Arc::clone(pipeline));
        }
        match self.build_compute(
            "shaders/wind_interact.spv",
            layout,
            crate::WIND_INTERACT_PUSH_SIZE,
        ) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.wind_interact = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_wind_interact: {err}");
                None
            }
        }
    }

    /// The VSM receiver-demand mark PSO (a [`crate::VSM_DEMAND_PUSH_SIZE`]-byte push).
    pub fn request_vsm_demand(&mut self, layout: vk::DescriptorSetLayout) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.vsm_demand {
            return Some(Arc::clone(pipeline));
        }
        match self.build_compute(
            "shaders/vsm_demand.spv",
            layout,
            crate::VSM_DEMAND_PUSH_SIZE,
        ) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.vsm_demand = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_vsm_demand: {err}");
                None
            }
        }
    }

    /// The VSM demand-compact PSO (a [`crate::VSM_COMPACT_PUSH_SIZE`]-byte push).
    pub fn request_vsm_demand_compact(
        &mut self,
        layout: vk::DescriptorSetLayout,
    ) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.vsm_demand_compact {
            return Some(Arc::clone(pipeline));
        }
        match self.build_compute(
            "shaders/vsm_demand_compact.spv",
            layout,
            crate::VSM_COMPACT_PUSH_SIZE,
        ) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.vsm_demand_compact = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_vsm_demand_compact: {err}");
                None
            }
        }
    }

    /// The hierarchy-traversal compute PSO (counters + visible list + record stream +
    /// address block, a 32-byte push).
    pub fn request_scene_traversal(
        &mut self,
        layout: vk::DescriptorSetLayout,
    ) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.scene_traversal {
            return Some(Arc::clone(pipeline));
        }
        match self.build_compute(
            "shaders/scene_traversal.spv",
            layout,
            crate::SCENE_TRAVERSAL_PUSH_SIZE,
        ) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.scene_traversal = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_scene_traversal: {err}");
                None
            }
        }
    }

    /// The micro-field survivor-count compute PSO
    /// ([`crate::SCENE_MICRO_FIELD_PUSH_SIZE`]-byte push).
    pub fn request_scene_micro_count(
        &mut self,
        layout: vk::DescriptorSetLayout,
    ) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.scene_micro_count {
            return Some(Arc::clone(pipeline));
        }
        match self.build_compute(
            "shaders/scene_micro_count.spv",
            layout,
            crate::SCENE_MICRO_FIELD_PUSH_SIZE,
        ) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.scene_micro_count = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_scene_micro_count: {err}");
                None
            }
        }
    }

    /// The micro-field exclusive-base scan compute PSO
    /// ([`crate::SCENE_MICRO_FIELD_PUSH_SIZE`]-byte push).
    pub fn request_scene_micro_scan(
        &mut self,
        layout: vk::DescriptorSetLayout,
    ) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.scene_micro_scan {
            return Some(Arc::clone(pipeline));
        }
        match self.build_compute(
            "shaders/scene_micro_scan.spv",
            layout,
            crate::SCENE_MICRO_FIELD_PUSH_SIZE,
        ) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.scene_micro_scan = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_scene_micro_scan: {err}");
                None
            }
        }
    }

    /// The micro-field exact-slot scatter compute PSO
    /// ([`crate::SCENE_MICRO_FIELD_PUSH_SIZE`]-byte push).
    pub fn request_scene_micro_scatter(
        &mut self,
        layout: vk::DescriptorSetLayout,
    ) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.scene_micro_scatter {
            return Some(Arc::clone(pipeline));
        }
        match self.build_compute(
            "shaders/scene_micro_scatter.spv",
            layout,
            crate::SCENE_MICRO_FIELD_PUSH_SIZE,
        ) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.scene_micro_scatter = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_scene_micro_scatter: {err}");
                None
            }
        }
    }

    /// The transparent key-collection compute PSO (a 32-byte push).
    pub fn request_transparent_keys(
        &mut self,
        layout: vk::DescriptorSetLayout,
    ) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.transparent_keys {
            return Some(Arc::clone(pipeline));
        }
        match self.build_compute("shaders/scene_transparent_keys.spv", layout, 32) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.transparent_keys = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_transparent_keys: {err}");
                None
            }
        }
    }

    /// The radix per-workgroup histogram compute PSO (a 16-byte push).
    pub fn request_radix_histogram(
        &mut self,
        layout: vk::DescriptorSetLayout,
    ) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.radix_histogram {
            return Some(Arc::clone(pipeline));
        }
        match self.build_compute("shaders/radix_histogram.spv", layout, 16) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.radix_histogram = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_radix_histogram: {err}");
                None
            }
        }
    }

    /// The radix global-scan compute PSO (a 16-byte push).
    pub fn request_radix_scan(&mut self, layout: vk::DescriptorSetLayout) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.radix_scan {
            return Some(Arc::clone(pipeline));
        }
        match self.build_compute("shaders/radix_scan.spv", layout, 16) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.radix_scan = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_radix_scan: {err}");
                None
            }
        }
    }

    /// The radix stable-scatter compute PSO (a 16-byte push).
    pub fn request_radix_scatter(
        &mut self,
        layout: vk::DescriptorSetLayout,
    ) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.radix_scatter {
            return Some(Arc::clone(pipeline));
        }
        match self.build_compute("shaders/radix_scatter.spv", layout, 16) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.radix_scatter = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_radix_scatter: {err}");
                None
            }
        }
    }

    /// The transparent command-reorder compute PSO (a 16-byte push).
    pub fn request_transparent_reorder(
        &mut self,
        layout: vk::DescriptorSetLayout,
    ) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.transparent_reorder {
            return Some(Arc::clone(pipeline));
        }
        match self.build_compute("shaders/scene_transparent_reorder.spv", layout, 16) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.transparent_reorder = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_transparent_reorder: {err}");
                None
            }
        }
    }

    /// The executor bin-count compute PSO (a 16-byte push).
    pub fn request_scene_bin_count(
        &mut self,
        layout: vk::DescriptorSetLayout,
    ) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.scene_bin_count {
            return Some(Arc::clone(pipeline));
        }
        match self.build_compute("shaders/scene_bin_count.spv", layout, 16) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.scene_bin_count = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_scene_bin_count: {err}");
                None
            }
        }
    }

    /// The executor bucket-seed compute PSO (no push).
    pub fn request_scene_bin_seed(
        &mut self,
        layout: vk::DescriptorSetLayout,
    ) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.scene_bin_seed {
            return Some(Arc::clone(pipeline));
        }
        match self.build_compute("shaders/scene_bin_seed.spv", layout, 0) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.scene_bin_seed = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_scene_bin_seed: {err}");
                None
            }
        }
    }

    /// The executor bin-scatter compute PSO (a 16-byte push).
    pub fn request_scene_bin_scatter(
        &mut self,
        layout: vk::DescriptorSetLayout,
    ) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.scene_bin_scatter {
            return Some(Arc::clone(pipeline));
        }
        match self.build_compute("shaders/scene_bin_scatter.spv", layout, 16) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.scene_bin_scatter = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_scene_bin_scatter: {err}");
                None
            }
        }
    }

    /// The HZB seed compute PSO (depth sampler + mip-0 storage, a 16-byte push).
    pub fn request_hzb_copy(&mut self, layout: vk::DescriptorSetLayout) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.hzb_copy {
            return Some(Arc::clone(pipeline));
        }
        match self.build_compute("shaders/hzb_copy.spv", layout, crate::HZB_PUSH_SIZE) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.hzb_copy = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_hzb_copy: {err}");
                None
            }
        }
    }

    /// The HZB reduce compute PSO (two storage mips, a 16-byte push).
    pub fn request_hzb_reduce(&mut self, layout: vk::DescriptorSetLayout) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.hzb_reduce {
            return Some(Arc::clone(pipeline));
        }
        match self.build_compute("shaders/hzb_reduce.spv", layout, crate::HZB_PUSH_SIZE) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.hzb_reduce = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_hzb_reduce: {err}");
                None
            }
        }
    }
}
