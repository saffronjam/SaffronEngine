//! Compute PSO front doors for the lighting and post chains: screen-space effects, DDGI,
//! the Global SDF, ReSTIR, volumetrics, and the resolve / tonemap stages.

use super::*;

impl Pipelines {
    /// The GTAO compute PSO (compute2 layout, an 80-byte push).
    pub fn request_gtao(&mut self, layout: vk::DescriptorSetLayout) -> Option<Arc<Pipeline>> {
        self.request_screen_compute(ScreenCompute::Gtao, "shaders/gtao.spv", layout, 80)
    }

    /// The AO bilateral-blur compute PSO (compute3 layout, no push).
    pub fn request_ao_blur(&mut self, layout: vk::DescriptorSetLayout) -> Option<Arc<Pipeline>> {
        self.request_screen_compute(ScreenCompute::AoBlur, "shaders/ao_blur.spv", layout, 0)
    }

    /// The directional contact-shadow compute PSO (compute2 layout, a 160-byte push).
    pub fn request_contact(&mut self, layout: vk::DescriptorSetLayout) -> Option<Arc<Pipeline>> {
        self.request_screen_compute(ScreenCompute::Contact, "shaders/contact.spv", layout, 160)
    }

    /// The one-bounce SSGI trace compute PSO (compute3 layout, a 144-byte push).
    pub fn request_ssgi(&mut self, layout: vk::DescriptorSetLayout) -> Option<Arc<Pipeline>> {
        self.request_screen_compute(ScreenCompute::Ssgi, "shaders/ssgi.spv", layout, 160)
    }

    /// The screen-space indirect-diffuse resolve compute PSO (the bespoke `gi_resolve` single-set
    /// layout; no push — its params ride a UBO in the set, being 224 bytes).
    pub fn request_gi_resolve(&mut self, layout: vk::DescriptorSetLayout) -> Option<Arc<Pipeline>> {
        self.request_screen_compute(
            ScreenCompute::GiResolve,
            "shaders/gi_resolve.spv",
            layout,
            0,
        )
    }

    /// The SSGI bilateral-blur compute PSO (compute3 layout, no push).
    pub fn request_ssgi_blur(&mut self, layout: vk::DescriptorSetLayout) -> Option<Arc<Pipeline>> {
        self.request_screen_compute(ScreenCompute::SsgiBlur, "shaders/ssgi_blur.spv", layout, 0)
    }

    /// The SSGI temporal-accumulation compute PSO (taa-shape layout, a 16-byte push).
    pub fn request_ssgi_accum(&mut self, layout: vk::DescriptorSetLayout) -> Option<Arc<Pipeline>> {
        self.request_screen_compute(
            ScreenCompute::SsgiAccum,
            "shaders/ssgi_accum.spv",
            layout,
            16,
        )
    }

    /// The DFAO temporal-accumulation compute PSO (taa-shape layout, a 16-byte push). Its own
    /// clamp-free EMA kernel — DFAO sky visibility is a rotating low-frequency Monte-Carlo estimate
    /// that must be averaged (not neighborhood-clamped like SSGI radiance) to converge on a static
    /// surface, so it does not reuse `ssgi_accum`.
    pub fn request_dfao_accum(&mut self, layout: vk::DescriptorSetLayout) -> Option<Arc<Pipeline>> {
        self.request_screen_compute(
            ScreenCompute::DfaoAccum,
            "shaders/dfao_accum.spv",
            layout,
            16,
        )
    }

    /// The screen-space reflection trace compute PSO (compute3 layout, a 160-byte push,
    /// same shape as the SSGI trace; the shared `params2` slot is unused here).
    pub fn request_ssr(&mut self, layout: vk::DescriptorSetLayout) -> Option<Arc<Pipeline>> {
        self.request_screen_compute(ScreenCompute::Ssr, "shaders/ssr.spv", layout, 160)
    }

    /// The SSGI prev-color history-copy compute PSO (compute2 layout, no push).
    pub fn request_copy_color(&mut self, layout: vk::DescriptorSetLayout) -> Option<Arc<Pipeline>> {
        self.request_screen_compute(
            ScreenCompute::CopyColor,
            "shaders/copy_color.spv",
            layout,
            0,
        )
    }

    /// The DDGI trace compute PSO. A three-set pipeline: set 0 = the bindless brick array, set 1 =
    /// the light set (the per-mesh SDF instance list + the GDF cascade clipmap + params, via the
    /// shared `sdf` module), set 2 = `trace_layout` (the albedo cache + prev-irradiance samplers +
    /// the ray-image storage). The 112-byte push carries the probe grid + volume + sun/sky + scroll.
    pub fn request_ddgi_trace(
        &mut self,
        trace_layout: vk::DescriptorSetLayout,
    ) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.ddgi_trace {
            return Some(Arc::clone(pipeline));
        }
        let set_layouts = [self.set_layouts[0], self.set_layouts[1], trace_layout];
        match self.build_compute_multi("shaders/ddgi_trace.spv", &set_layouts, 112) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.ddgi_trace = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_ddgi_trace: {err}");
                None
            }
        }
    }

    /// The DFAO sky-visibility cone-trace PSO. A three-set pipeline mirroring the DDGI trace:
    /// set 0 = the bindless brick array, set 1 = the light set (the GDF cascade clipmap + params
    /// at bindings 9/10, via the shared `sdf` module), set 2 = `io_layout` (the compute2 shape:
    /// the G-buffer sampler + the half-res sky-visibility storage). A 144-byte push (the camera
    /// inverses + the frame index).
    pub fn request_dfao(&mut self, io_layout: vk::DescriptorSetLayout) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.dfao {
            return Some(Arc::clone(pipeline));
        }
        let set_layouts = [self.set_layouts[0], self.set_layouts[1], io_layout];
        match self.build_compute_multi("shaders/dfao.spv", &set_layouts, 144) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.dfao = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_dfao: {err}");
                None
            }
        }
    }

    /// The specular reflection-occlusion cone-trace PSO. A three-set pipeline mirroring
    /// [`Pipelines::request_dfao`]: set 0 = the bindless brick array, set 1 = the light set (the GDF
    /// cascade clipmap + params at bindings 9/10, via the shared `sdf` module), set 2 = `io_layout`
    /// (the compute3 shape: the G-buffer + roughness samplers + the half-res occlusion storage). A
    /// 144-byte push (the camera inverses + the frame index).
    pub fn request_specocc(&mut self, io_layout: vk::DescriptorSetLayout) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.specocc {
            return Some(Arc::clone(pipeline));
        }
        let set_layouts = [self.set_layouts[0], self.set_layouts[1], io_layout];
        match self.build_compute_multi("shaders/specocc.spv", &set_layouts, 144) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.specocc = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_specocc: {err}");
                None
            }
        }
    }

    /// The DDGI blend-irradiance compute PSO (its set layout, an 80-byte push).
    pub fn request_ddgi_blend_irr(
        &mut self,
        layout: vk::DescriptorSetLayout,
    ) -> Option<Arc<Pipeline>> {
        self.request_screen_compute(
            ScreenCompute::DdgiBlendIrr,
            "shaders/ddgi_blend_irradiance.spv",
            layout,
            80,
        )
    }

    /// The DDGI blend-distance compute PSO (its set layout, an 80-byte push).
    pub fn request_ddgi_blend_dist(
        &mut self,
        layout: vk::DescriptorSetLayout,
    ) -> Option<Arc<Pipeline>> {
        self.request_screen_compute(
            ScreenCompute::DdgiBlendDist,
            "shaders/ddgi_blend_distance.spv",
            layout,
            80,
        )
    }

    /// The DDGI octahedral-border compute PSO (its set layout, a 32-byte push).
    pub fn request_ddgi_border(
        &mut self,
        layout: vk::DescriptorSetLayout,
    ) -> Option<Arc<Pipeline>> {
        self.request_screen_compute(
            ScreenCompute::DdgiBorder,
            "shaders/ddgi_border.spv",
            layout,
            32,
        )
    }

    /// The Global-SDF cull compute PSO: set 0 = the bindless brick array (for the shared
    /// `SdfInstance` struct), set 1 = `gdf_layout` (instances + cull list), a 64-byte push.
    pub fn request_gdf_cull(
        &mut self,
        gdf_layout: vk::DescriptorSetLayout,
    ) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.gdf_cull {
            return Some(Arc::clone(pipeline));
        }
        let set_layouts = [self.set_layouts[0], gdf_layout];
        match self.build_compute_multi("shaders/gdf_cull.spv", &set_layouts, 64) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.gdf_cull = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_gdf_cull: {err}");
                None
            }
        }
    }

    /// The GI occluder-scatter compute PSO: one set (`scatter_layout` — the reach
    /// view's lists, the occluder output, the meta words, and the scene address block),
    /// a 48-byte push.
    pub fn request_gi_occluder_scatter(
        &mut self,
        scatter_layout: vk::DescriptorSetLayout,
    ) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.gi_occluder_scatter {
            return Some(Arc::clone(pipeline));
        }
        let set_layouts = [scatter_layout];
        match self.build_compute_multi("shaders/gi_occluder_scatter.spv", &set_layouts, 48) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.gi_occluder_scatter = Some(Arc::clone(&pipeline));
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_gi_occluder_scatter: {err}");
                None
            }
        }
    }

    /// The Global-SDF composite compute PSO: set 0 = the bindless brick array (the `sampleMdfBrick`
    /// taps), set 1 = `gdf_layout` (instances + cull list + cascade storage images), a 48-byte push.
    pub fn request_gdf_composite(
        &mut self,
        gdf_layout: vk::DescriptorSetLayout,
    ) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.gdf_composite {
            return Some(Arc::clone(pipeline));
        }
        let set_layouts = [self.set_layouts[0], gdf_layout];
        match self.build_compute_multi("shaders/gdf_composite.spv", &set_layouts, 48) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.gdf_composite = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_gdf_composite: {err}");
                None
            }
        }
    }

    /// The ReSTIR initial-candidate-sampling compute PSO (its set layout, a 176-byte push).
    pub fn request_restir_initial(
        &mut self,
        layout: vk::DescriptorSetLayout,
    ) -> Option<Arc<Pipeline>> {
        self.request_screen_compute(
            ScreenCompute::RestirInitial,
            "shaders/restir_initial.spv",
            layout,
            crate::RESTIR_INITIAL_PUSH_SIZE,
        )
    }

    /// The ReSTIR temporal+spatial reuse compute PSO (its set layout, a 160-byte push).
    pub fn request_restir_reuse(
        &mut self,
        layout: vk::DescriptorSetLayout,
    ) -> Option<Arc<Pipeline>> {
        self.request_screen_compute(
            ScreenCompute::RestirReuse,
            "shaders/restir_reuse.spv",
            layout,
            crate::RESTIR_REUSE_PUSH_SIZE,
        )
    }

    /// The ReSTIR resolve compute PSO: set 0 = the resolve layout (incl. the TLAS + the
    /// GPU-scene address block), set 1 = the bindless texture array (the candidate coverage
    /// confirmation samples it), a 160-byte push. RT-only — the resolve traces one
    /// visibility ray via the TLAS.
    pub fn request_restir_resolve(
        &mut self,
        layout: vk::DescriptorSetLayout,
    ) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.restir_resolve {
            return Some(Arc::clone(pipeline));
        }
        let set_layouts = [layout, self.set_layouts[0]];
        match self.build_compute_multi(
            "shaders/restir_resolve.spv",
            &set_layouts,
            crate::RESTIR_RESOLVE_PUSH_SIZE,
        ) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.restir_resolve = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_restir_resolve: {err}");
                None
            }
        }
    }

    /// The TAA resolve compute PSO (the taa-shape set layout: 3 samplers + 2 storage + the motion-
    /// depth sampler, a 48-byte push).
    pub fn request_taa(&mut self, layout: vk::DescriptorSetLayout) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.taa {
            return Some(Arc::clone(pipeline));
        }
        match self.build_compute(
            "shaders/taa.spv",
            layout,
            size_of::<crate::TaaPush>() as u32,
        ) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.taa = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_taa: {err}");
                None
            }
        }
    }

    /// The FXAA edge-blur compute PSO (the fxaa set layout: source sampler + offscreen storage, no
    /// push).
    pub fn request_fxaa(&mut self, layout: vk::DescriptorSetLayout) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.fxaa {
            return Some(Arc::clone(pipeline));
        }
        match self.build_compute("shaders/fxaa.spv", layout, 0) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.fxaa = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_fxaa: {err}");
                None
            }
        }
    }

    /// The bloom pyramid compute PSO (bloom set layout, a 32-byte [`crate::BloomPush`]). One PSO
    /// covers the downsample / upsample / composite passes.
    pub fn request_bloom(&mut self, layout: vk::DescriptorSetLayout) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.bloom {
            return Some(Arc::clone(pipeline));
        }
        match self.build_compute(
            "shaders/bloom.spv",
            layout,
            size_of::<crate::BloomPush>() as u32,
        ) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.bloom = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_bloom: {err}");
                None
            }
        }
    }

    /// The mandatory tonemap compute PSO (tonemap set layout, an 8-byte exposure+mode push).
    /// Returns `None` only on a build failure (logged) — the tonemap is otherwise always present.
    pub fn request_tonemap(&mut self) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.tonemap {
            return Some(Arc::clone(pipeline));
        }
        match self.build_compute("shaders/tonemap.spv", self.tonemap_set_layout, 16) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.tonemap = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_tonemap: {err}");
                None
            }
        }
    }

    /// The analytic height-fog composite compute PSO (`height_fog.spv`, the fog set layout, no
    /// push).
    pub fn request_fog(&mut self) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.fog {
            return Some(Arc::clone(pipeline));
        }
        match self.build_compute("shaders/height_fog.spv", self.fog_set_layout, 0) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.fog = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_fog: {err}");
                None
            }
        }
    }

    /// The weather-map resolve compute PSO.
    pub fn request_cloud_weather(
        &mut self,
        layout: vk::DescriptorSetLayout,
    ) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.cloud_weather {
            return Some(Arc::clone(pipeline));
        }
        match self.build_compute("shaders/cloud_weather.spv", layout, 0) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.cloud_weather = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_cloud_weather: {err}");
                None
            }
        }
    }

    /// The unlit density-debug compute PSO.
    pub fn request_cloud_debug(
        &mut self,
        layout: vk::DescriptorSetLayout,
    ) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.cloud_debug {
            return Some(Arc::clone(pipeline));
        }
        match self.build_compute("shaders/cloud_density_debug.spv", layout, 0) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.cloud_debug = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_cloud_debug: {err}");
                None
            }
        }
    }

    /// The adaptive lit cloud raymarch PSO.
    pub fn request_cloud_raymarch(
        &mut self,
        layout: vk::DescriptorSetLayout,
    ) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.cloud_raymarch {
            return Some(Arc::clone(pipeline));
        }
        match self.build_compute("shaders/cloud_raymarch.spv", layout, 0) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.cloud_raymarch = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_cloud_raymarch: {err}");
                None
            }
        }
    }

    /// The reduced cloud temporal reconstruction PSO.
    pub fn request_cloud_reconstruct(
        &mut self,
        layout: vk::DescriptorSetLayout,
    ) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.cloud_reconstruct {
            return Some(Arc::clone(pipeline));
        }
        match self.build_compute("shaders/cloud_reconstruct.spv", layout, 0) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.cloud_reconstruct = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_cloud_reconstruct: {err}");
                None
            }
        }
    }

    /// The bilateral cloud upscale and HDR composite PSO.
    pub fn request_cloud_upscale(
        &mut self,
        layout: vk::DescriptorSetLayout,
    ) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.cloud_upscale {
            return Some(Arc::clone(pipeline));
        }
        match self.build_compute("shaders/cloud_upscale.spv", layout, 0) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.cloud_upscale = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_cloud_upscale: {err}");
                None
            }
        }
    }

    /// The cascaded density-integrated cloud-shadow fill PSO.
    pub fn request_cloud_shadow(
        &mut self,
        layout: vk::DescriptorSetLayout,
    ) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.cloud_shadow {
            return Some(Arc::clone(pipeline));
        }
        match self.build_compute("shaders/cloud_shadow.spv", layout, 0) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.cloud_shadow = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_cloud_shadow: {err}");
                None
            }
        }
    }

    /// The froxel fog-injection compute PSO (`fog_inject.spv`): set 0 is the mesh light set layout
    /// (globals/lights/clusters/cluster params + shadow maps, reused verbatim), set 1 is the fog
    /// volume set (scatter storage image + grid UBO), plus a 64-byte medium push.
    pub fn request_fog_inject(
        &mut self,
        volume_layout: vk::DescriptorSetLayout,
    ) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.fog_inject {
            return Some(Arc::clone(pipeline));
        }
        let set_layouts = [self.set_layouts[1], volume_layout];
        match self.build_compute_multi("shaders/fog_inject.spv", &set_layouts, 64) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.fog_inject = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_fog_inject: {err}");
                None
            }
        }
    }

    /// The froxel fog-integration compute PSO (`fog_integrate.spv`, the integrate set layout, no
    /// push).
    pub fn request_fog_integrate(
        &mut self,
        integrate_layout: vk::DescriptorSetLayout,
    ) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.fog_integrate {
            return Some(Arc::clone(pipeline));
        }
        match self.build_compute("shaders/fog_integrate.spv", integrate_layout, 0) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.fog_integrate = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_fog_integrate: {err}");
                None
            }
        }
    }

    /// The aerial-perspective fill compute PSO (`aerial_perspective.spv`, the AP fill set, no push)
    /// and resolved only while an active atmosphere + authored AP arm the fill.
    pub fn request_aerial(
        &mut self,
        fill_layout: vk::DescriptorSetLayout,
    ) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.aerial {
            return Some(Arc::clone(pipeline));
        }
        match self.build_compute("shaders/aerial_perspective.spv", fill_layout, 0) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.aerial = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_aerial: {err}");
                None
            }
        }
    }

    /// The look-bake compute PSO (folds grade + view transform + creative LUT into a `33³` table)
    /// on the first `bake-look`.
    pub fn request_lut_bake(&mut self) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.lut_bake {
            return Some(Arc::clone(pipeline));
        }
        match self.build_compute("shaders/lut_bake.spv", self.tonemap_set_layout, 8) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.lut_bake = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_lut_bake: {err}");
                None
            }
        }
    }

    /// The motion-vector visualization compute PSO. Binds the copy_color-shaped set (one sampler +
    /// one storage image); `layout` is [`crate::Ssao::compute2_layout`].
    pub fn request_motion_visualize(
        &mut self,
        layout: vk::DescriptorSetLayout,
    ) -> Option<Arc<Pipeline>> {
        if let Some(pipeline) = &self.motion_visualize {
            return Some(Arc::clone(pipeline));
        }
        match self.build_compute("shaders/motion_visualize.spv", layout, 8) {
            Ok(pipeline) => {
                let pipeline = Arc::new(pipeline);
                self.motion_visualize = Some(Arc::clone(&pipeline));
                self.pipelines_created += 1;
                Some(pipeline)
            }
            Err(err) => {
                tracing::error!("request_motion_visualize: {err}");
                None
            }
        }
    }
}
