//! Raster PSO front doors: the mesh übershader permutations and the depth / G-buffer /
//! motion / overlay passes that draw through them.

use super::*;

impl Pipelines {
    /// A material's mesh PSO: the fragment permutations over the record-driven vertex path
    /// (`vertexMainExecutor`), or the mesh-stage executor when `mesh_shader` is set.
    pub fn request_executor_mesh_pipeline(
        &mut self,
        material: &Material,
        wireframe: bool,
        mesh_shader: bool,
    ) -> Option<Arc<Pipeline>> {
        let wireframe = wireframe && self.fill_mode_non_solid;
        let alpha_to_coverage =
            material.masked && self.sample_count != vk::SampleCountFlags::TYPE_1;
        let key = PsoKey {
            shader: material.shader.clone(),
            unlit: material.unlit,
            wireframe,
            blend: material.blend,
            alpha_to_coverage,
            sample_count: self.sample_count,
            mesh_shader,
        };
        if let Some(pipeline) = self.cache.get(&key) {
            return Some(Arc::clone(pipeline));
        }
        match self.build_mesh_pipeline(&key) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.cache.insert(key, Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_executor_mesh_pipeline: {err}");
                None
            }
        }
    }

    /// The depth pre-pass PSO: the record-driven `vertexMainExecutor` (no vertex input)
    /// over the alpha-clip fragment.
    pub fn request_depth_prepass_executor(&mut self) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.depth_prepass_executor {
            return Some(Arc::clone(pipeline));
        }
        match self.build_depth_prepass() {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.depth_prepass_executor = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_depth_prepass_executor: {err}");
                None
            }
        }
    }

    /// The shadow depth PSO: the record-driven `vertexMainExecutor` (no vertex input)
    /// over the canonical-coverage fragment and dynamic depth bias.
    pub fn request_shadow_depth_executor(&mut self) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.shadow_depth_executor {
            return Some(Arc::clone(pipeline));
        }
        match self.build_shadow_depth() {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.shadow_depth_executor = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_shadow_depth_executor: {err}");
                None
            }
        }
    }

    /// The thin G-buffer prepass PSO (view normal rgb + view-Z in `.a`, roughness in a second
    /// target): the record-driven vertex path, no vertex input.
    pub fn request_gbuffer_executor(&mut self) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.gbuffer_executor {
            return Some(Arc::clone(pipeline));
        }
        match self.build_gbuffer() {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.gbuffer_executor = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_gbuffer_executor: {err}");
                None
            }
        }
    }

    /// The motion-vector prepass PSO: the record-driven vertex path (no vertex input),
    /// depth-tested, rg16f motion from the cur/prev camera reprojection. Built
    /// single-sampled but re-dropped on a sample-count change so it tracks the active count.
    pub fn request_motion_executor(&mut self) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.motion_executor {
            return Some(Arc::clone(pipeline));
        }
        match self.build_motion() {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.motion_executor = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_motion_executor: {err}");
                None
            }
        }
    }

    /// The depth-upscale graphics PSO, point-upscaling the input-extent scene depth into the
    /// display-extent overlay depth. `layout` is [`crate::Descriptors::depth_upscale_layout`].
    pub fn request_depth_upscale(
        &mut self,
        layout: vk::DescriptorSetLayout,
    ) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.depth_upscale {
            return Some(Arc::clone(pipeline));
        }
        match self.build_depth_upscale(layout) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.depth_upscale = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_depth_upscale: {err}");
                None
            }
        }
    }

    /// The transition-reactive graphics PSO: re-draws the opaque buckets through the
    /// degenerate-collapse vertex path, marking only blades and records mid
    /// representation-transition into the reactive mask.
    pub fn request_reactive_transition(&mut self) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.reactive_transition {
            return Some(Arc::clone(pipeline));
        }
        match self.build_reactive_coverage_entry(c"vertexMainReactiveTransition") {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.reactive_transition = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_reactive_transition: {err}");
                None
            }
        }
    }

    /// The TAA reactive-coverage graphics PSO: marks the translucent batches into the r8 reactive
    /// mask.
    pub fn request_reactive_coverage(&mut self) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.reactive_coverage {
            return Some(Arc::clone(pipeline));
        }
        match self.build_reactive_coverage_entry(c"vertexMainExecutor") {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.reactive_coverage = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_reactive_coverage: {err}");
                None
            }
        }
    }

    /// The analytic ground-grid graphics PSO: fullscreen triangle, depth-tested without writing,
    /// alpha-blended, a 2×mat4 push, single-sampled onto the 1× resolved color after tonemap.
    pub fn request_grid(&mut self) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.grid {
            return Some(Arc::clone(pipeline));
        }
        match self.build_grid() {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.grid = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_grid: {err}");
                None
            }
        }
    }

    /// The Lit Wireframe overlay PSO (line polygon mode, depth-tested without write). Returns
    /// `None` when the device lacks `fill_mode_non_solid` (the mode then falls back to plain Lit)
    /// or on a build failure (logged).
    pub fn request_wireframe_overlay(&mut self) -> Option<Arc<Pipeline>> {
        if !self.fill_mode_non_solid {
            return None;
        }
        if let Some(pipeline) = &self.wireframe_overlay {
            return Some(Arc::clone(pipeline));
        }
        match self.build_wireframe_overlay() {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.wireframe_overlay = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_wireframe_overlay: {err}");
                None
            }
        }
    }

    /// The always-on-top editor-overlay graphics PSO (the [`crate::OverlayVertex`] stream, alpha-
    /// blended, no depth test, single-sampled, no descriptor sets).
    pub fn request_overlay(&mut self) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.overlay {
            return Some(Arc::clone(pipeline));
        }
        match self.build_overlay(false) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.overlay = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_overlay: {err}");
                None
            }
        }
    }

    /// The depth-tested editor-overlay graphics PSO (same as `overlay` but depth-tested so scene
    /// geometry occludes it — camera frustums, etc.).
    pub fn request_overlay_depth(&mut self) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.overlay_depth {
            return Some(Arc::clone(pipeline));
        }
        match self.build_overlay(true) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.overlay_depth = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_overlay_depth: {err}");
                None
            }
        }
    }
}
