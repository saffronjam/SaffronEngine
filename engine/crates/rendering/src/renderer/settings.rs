use super::*;

impl Renderer {
    /// Whether clustered-forward light culling is on; off makes the fragment loop all lights.
    pub fn clustered_enabled(&self) -> bool {
        self.lighting.use_clustered
    }

    /// Toggles clustered-forward culling.
    pub fn set_clustered(&mut self, enabled: bool) {
        self.lighting.use_clustered = enabled;
    }

    /// Whether shadow casting is on (the master toggle).
    pub fn shadows_enabled(&self) -> bool {
        self.lighting.use_shadows
    }

    /// Toggles shadow casting.
    pub fn set_shadows(&mut self, enabled: bool) {
        self.lighting.use_shadows = enabled;
    }

    /// Folds the host's visible-sky settings in (mode / clear / intensity / rotation /
    /// visibility / panorama slot).
    pub fn submit_sky(&mut self, settings: &SkyRenderSettings) {
        self.sky.submit(settings);
    }

    /// Folds the scene's cloud-shape authoring into the persistent density resources. Weather-map
    /// inputs mark the single weather image dirty; all other shape values update the shared UBO.
    pub fn submit_clouds(&mut self, settings: crate::CloudRenderSettings) {
        self.clouds.submit(settings);
    }

    /// Folds this frame's analytic height/distance fog in; the composite pass runs before the
    /// bloom pyramid only while `settings.enabled`.
    pub fn set_fog(&mut self, settings: &FogRenderSettings) {
        self.fog = *settings;
        // Switching the froxel-grid quality tier reallocates the volumes, so both consumers of the
        // integration volume are rebound — every view's fog set (binding 4) and every light set's
        // binding 11. The history reset rides inside `set_quality`.
        match self.froxel.set_quality(&self.device, settings.quality) {
            Ok(true) => {
                let sampler = self.froxel.sampler();
                let view = self.froxel.integration_view();
                for v in &mut self.views {
                    v.write_fog_integration(&self.device, sampler, view);
                }
                self.lighting
                    .bind_froxel_integration(&self.device, view, sampler);
            }
            Ok(false) => {}
            Err(err) => tracing::error!("froxel fog set_quality: {err}"),
        }
    }

    /// The IBL the scene pass binds for the active view: the fixed procedural [`Renderer::preview_ibl`]
    /// on the offscreen [`ViewId::Thumbnail`] view, else the project [`Renderer::ibl`].
    pub(super) fn scene_ibl(&self) -> &Ibl {
        if self.active_view == ViewId::Thumbnail {
            &self.preview_ibl
        } else {
            &self.ibl
        }
    }

    /// The mutable twin of [`Renderer::scene_ibl`] — routes an environment bake to the preview IBL
    /// while a thumbnail renders, so the project IBL is never touched by a thumbnail's env sync.
    pub(super) fn scene_ibl_mut(&mut self) -> &mut Ibl {
        if self.active_view == ViewId::Thumbnail {
            &mut self.preview_ibl
        } else {
            &mut self.ibl
        }
    }

    /// Whether the active view's environment bake and derived lighting capture are complete.
    /// Synchronous thumbnail readback uses this to avoid capturing a partially refreshed IBL.
    pub fn active_environment_converged(&self) -> bool {
        self.scene_ibl().dynamic_lighting_converged()
    }

    /// Re-arms the IBL environment bake when the source / panorama / params change. The bake fires
    /// at the next [`Renderer::render_scene_offscreen`], so the visible sky + IBL relight together.
    /// Routed to the active view's IBL, so a thumbnail render never re-arms the project IBL.
    pub fn request_env_bake(
        &mut self,
        source: EnvSource,
        panorama: Option<Arc<crate::GpuTexture>>,
        params: SkygenParams,
    ) {
        self.scene_ibl_mut()
            .request_env_bake(source, panorama, params);
    }

    /// Whether IBL ambient is on (false = the flat scalar ambient fallback).
    pub fn ibl_enabled(&self) -> bool {
        self.ibl.use_ibl
    }

    /// Toggles IBL ambient.
    pub fn set_ibl(&mut self, enabled: bool) {
        self.ibl.use_ibl = enabled;
    }

    /// Folds the host's per-frame reflection-probe uploads in: arms any dirty slot for capture,
    /// re-uploads the metadata SSBO, and updates the frame probe count.
    pub fn submit_reflection_probes(&mut self, probes: &[ReflectionProbeUpload]) {
        self.reflection.submit(probes);
    }

    /// Folds this frame's local fog volumes in, baking each [`crate::FogVolumeUpload`] into its
    /// std430 record for the inject pass to loop (capped at [`crate::MAX_FOG_VOLUMES`]).
    pub fn submit_fog_volumes(&mut self, volumes: &[crate::FogVolumeUpload]) {
        let cap = crate::MAX_FOG_VOLUMES as usize;
        self.fog_volumes.clear();
        self.fog_volumes.extend(
            volumes
                .iter()
                .take(cap)
                .map(crate::FogVolumeGpu::from_upload),
        );
    }

    /// Whether reflection probes contribute.
    pub fn reflection_probes_enabled(&self) -> bool {
        self.reflection.use_probes
    }

    /// The captured reflection probes in slot order (the `list-probes` source).
    pub fn reflection_probes(&self) -> &[crate::ReflectionProbe] {
        self.reflection.probes()
    }

    /// Toggles reflection probes.
    pub fn set_reflection_probes(&mut self, enabled: bool) {
        self.reflection.use_probes = enabled;
    }

    /// Applies a render-quality tier: the screen-space GI stack's enable flags plus the SSGI and
    /// contact step counts. The single knob for SSGI / GTAO / contact shadows.
    pub fn set_render_quality(&mut self, quality: RenderQuality) {
        self.render_quality = quality;
        self.ssao.apply_quality(&quality);
    }

    /// The current render-quality tier + resolved parameters.
    pub fn render_quality(&self) -> RenderQuality {
        self.render_quality
    }

    /// The active tonemap operator.
    pub fn tonemap_mode(&self) -> TonemapMode {
        self.tonemap_mode
    }

    /// Selects the tonemap operator (applied in the tonemap pass next frame).
    pub fn set_tonemap_mode(&mut self, mode: TonemapMode) {
        self.tonemap_mode = mode;
    }

    /// The scene-linear color grade folded into the tonemap pass.
    pub fn color_grading(&self) -> ColorGrade {
        self.color_grade
    }

    /// Sets the scene-linear color grade (applied in the tonemap pass next frame).
    pub fn set_color_grading(&mut self, grade: ColorGrade) {
        self.color_grade = grade;
    }

    /// The assigned creative-LUT read-back: `(asset id, size, intensity)`, or `None` when no look is
    /// assigned (id `0`).
    pub fn creative_lut(&self) -> Option<(u64, u32, f32)> {
        (self.creative_lut_id != 0).then_some((
            self.creative_lut_id,
            self.creative_lut_size,
            self.creative_lut_intensity,
        ))
    }

    /// Assigns the display-space creative look-up table and its look intensity. `lut` is the
    /// resolved GPU table, or `None` to clear to the identity default; `id` is the asset id (`0`
    /// clears). Only an asset change rebinds the descriptor, idled so no in-flight frame reads it.
    pub fn set_creative_lut_texture(
        &mut self,
        id: u64,
        lut: Option<Arc<crate::GpuLut>>,
        intensity: f32,
    ) {
        self.creative_lut_intensity = intensity.clamp(0.0, 1.0);
        if id == self.creative_lut_id {
            return;
        }
        self.creative_lut_id = id;
        self.creative_lut = lut;
        self.creative_lut_size = self.creative_lut.as_ref().map_or(2, |l| l.size());
        let _ = self.device.wait_idle();
        let view = self
            .creative_lut
            .as_ref()
            .map_or_else(|| self.default_lut.view(), |l| l.view());
        let sampler = self.descriptors.linear_sampler();
        for v in &self.views {
            v.write_tonemap_lut(&self.device, sampler, view);
        }
    }

    /// Bakes the folded look — grade, view transform, creative LUT — into one `33³` display-referred
    /// table over the log2 shaper (EV `[-14, +11]`) on the GPU. Returns `(size, ev_min, ev_max, rgb)`
    /// with `rgb` as red-fastest `[r, g, b]` f16 bits. Idles around the one-off dispatch.
    ///
    /// # Errors
    ///
    /// [`crate::Error`] if the bake PSO is unavailable or a Vulkan/VMA call fails.
    pub fn bake_look_lut(
        &mut self,
        uploader: &crate::Uploader,
    ) -> Result<(u32, f32, f32, Vec<[u16; 3]>)> {
        let pipeline = self
            .pipelines
            .request_lut_bake()
            .ok_or_else(|| crate::Error::LutBake("lut_bake PSO unavailable".to_owned()))?;
        let grade = GradeUniform::from(&self.color_grade).with_look(
            self.creative_lut_intensity,
            self.creative_lut_size,
            false,
        );
        let view = self
            .creative_lut
            .as_ref()
            .map_or_else(|| self.default_lut.view(), |l| l.view());
        let sampler = self.descriptors.linear_sampler();
        let mode = self.tonemap_mode as u32;
        self.device.wait_idle()?;
        let rgb = uploader.bake_look_lut(
            &self.descriptors,
            &pipeline,
            &grade,
            view,
            sampler,
            crate::LUT_BAKE_SIZE,
            mode,
        )?;
        Ok((
            crate::LUT_BAKE_SIZE,
            crate::LUT_SHAPER_EV_MIN,
            crate::LUT_SHAPER_EV_MAX,
            rgb,
        ))
    }

    /// Pushes the per-frame reactive-loop snapshot (idle / converged / active reasons) the host
    /// derives from the run loop's `RedrawController`, for `render-stats` to report.
    pub fn set_reactive_state(&mut self, idle: bool, converged: bool, reasons: Vec<String>) {
        self.reactive.idle = idle;
        self.reactive.converged = converged;
        self.reactive.reasons = reasons;
    }

    /// Whether the reactive loop is idling (not rendering) per the last host snapshot.
    pub fn reactive_idle(&self) -> bool {
        self.reactive.idle
    }

    /// Whether the temporal effects have converged per the last host snapshot.
    pub fn reactive_converged(&self) -> bool {
        self.reactive.converged
    }

    /// The reasons continuous render is currently held per the last host snapshot.
    pub fn reactive_reasons(&self) -> &[String] {
        &self.reactive.reasons
    }

    /// The editor viewport power state (focused / unfocused / occluded), set by the editor's
    /// window-visibility signal; the host reads it each frame to suppress a hidden viewport.
    pub fn power_state(&self) -> PowerState {
        self.reactive.power_state
    }

    /// Sets the editor viewport power state. Leaving the focused state restarts the perf-alarm
    /// settle window, so the re-convergence burst on return cannot fire a false frame-time alarm.
    pub fn set_power_state(&mut self, state: PowerState) {
        if state != PowerState::Focused {
            self.alarms.reset_focus_settle();
        }
        self.reactive.power_state = state;
    }

    /// Whether GTAO is on (per the active tier) and its sets/targets are built.
    pub fn ssao_enabled(&self) -> bool {
        self.ssao.use_ssao && self.ssao.ready
    }

    /// Whether contact shadows are on (per the active tier) and ready.
    pub fn contact_shadows_enabled(&self) -> bool {
        self.ssao.use_contact && self.ssao.ready
    }

    /// Whether SSGI is on (per the active tier) and ready.
    pub fn ssgi_enabled(&self) -> bool {
        self.ssao.use_ssgi && self.ssao.ready
    }

    /// Toggles voxel-traced dynamic diffuse GI; turning it on re-converges the probes
    /// from scratch (a history reset).
    pub fn set_ddgi(&mut self, enabled: bool) {
        self.ddgi.set_enabled(enabled);
    }

    /// Whether DDGI is on and its resources are built.
    pub fn ddgi_enabled(&self) -> bool {
        self.ddgi.enabled()
    }

    /// Toggles GDF reflection occlusion: the per-pixel cone-march along the reflection vector
    /// against the Global Distance Field that occludes the reflected skybox under overhangs.
    pub fn set_sky_occlusion(&mut self, enabled: bool) {
        self.sky_occlusion = enabled;
    }

    /// Whether GDF reflection occlusion (the specular reflection-cone march) is enabled.
    pub fn sky_occlusion_enabled(&self) -> bool {
        self.sky_occlusion
    }

    /// Whether the GDF reflection-occlusion term is active for the active view this frame: IBL is
    /// on, the toggle is set, and the Global Distance Field cascade clipmap the cone-march taps is
    /// ready. The lighting UBO enable bit gates on this predicate, so the fragment marches the GDF
    /// only when its clipmap is valid.
    pub(super) fn want_sky_occlusion(&self) -> bool {
        let ibl = self.scene_ibl();
        let ibl_enabled = ibl.use_ibl && ibl.ready;
        ibl_enabled && self.sky_occlusion_enabled() && self.global_sdf.enabled()
    }

    /// Snaps the camera-centered DDGI probe clipmap to the camera + stores the sun for the
    /// trace. Call before [`Renderer::set_scene_lighting`], which folds the volume + probe grid +
    /// toroidal scroll base into the light UBO.
    pub fn set_ddgi_scene(
        &mut self,
        cam_pos: saffron_geometry::glam::Vec3,
        sun_dir: saffron_geometry::glam::Vec3,
        sun_color: saffron_geometry::glam::Vec3,
        sun_intensity: f32,
    ) {
        self.ddgi
            .set_scene(cam_pos, sun_dir, sun_color, sun_intensity);
    }

    /// Writes this frame's camera transforms + incoming sun direction for the screen-space chain
    /// (the G-buffer prepass view/viewProj, the contact-shadow view-space light direction). Call
    /// before [`Renderer::render_scene_offscreen`].
    pub fn set_ssao_camera(
        &mut self,
        view: Mat4,
        proj: Mat4,
        sun_direction_world: saffron_geometry::glam::Vec3,
    ) {
        self.ssao.set_camera(view, proj, sun_direction_world);
        // Recenter the Global-SDF cascade clipmap on the camera eye (the inverse-view translation),
        // snapping each cascade to its own voxel grid for the toroidal incremental update.
        let eye = view.inverse().col(3).truncate();
        // The recording frame's slot — the params UBO this frame's light set (same slot) reads.
        self.global_sdf.set_camera(eye, self.frames.index());
        self.global_sdf.prepare_frame_regions();
    }

    /// Toggles the Global Distance Field: the camera-centered cascade clipmap the far-field cone
    /// march and the DDGI trace tap as one trilinear read. On by default.
    pub fn set_gdf(&mut self, enabled: bool) {
        self.global_sdf.set_enabled(enabled);
    }

    /// Whether the Global Distance Field is on and its resources are built.
    pub fn gdf_enabled(&self) -> bool {
        self.global_sdf.enabled()
    }

    /// Whether screen-space reflections are enabled.
    pub fn ssr_enabled(&self) -> bool {
        self.ssao.use_ssr
    }

    /// Toggles screen-space reflections (opt-in; off by default).
    pub fn set_ssr(&mut self, enabled: bool) {
        self.ssao.use_ssr = enabled;
    }

    /// Writes the current frame's cluster-cull params from the camera + viewport, arming
    /// the `light-cull` dispatch when clustered is on and at least one punctual light
    /// exists. Call after [`Renderer::set_scene_lighting`].
    pub fn set_cluster_camera(&mut self, camera: ClusterCamera) {
        let frame = self.frames.index();
        // Mirror the camera planes for the TAA resolve's depth linearization (disocclusion test).
        self.camera_near_far = (camera.near, camera.far);
        // Mirror the whole camera for the adaptive-tessellation factor metric.
        self.cluster_camera = camera;
        self.lighting.set_cluster_camera(frame, camera);
    }

    /// Selects the anti-aliasing mode (`msaa_samples` ≥ 2 → MSAA, else `fxaa`, else `taa`,
    /// else off — mutually exclusive). Idles the GPU, recreates the active view's AA
    /// targets, and — when the MSAA sample count changed — clears the sample-count-baked
    /// PSO cache so the mesh + depth-prepass PSOs rebuild for the new count.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] if the device cannot idle or the AA targets cannot be recreated.
    pub fn set_aa(&mut self, msaa_samples: u32, fxaa: bool, taa: bool) -> Result<()> {
        let count_changed = self.aa.set(msaa_samples, fxaa, taa);
        self.device.wait_idle()?;
        if count_changed {
            // The mesh + depth-prepass PSOs bake the sample count — clear them so the next
            // request rebuilds for the new count.
            self.pipelines.set_sample_count(self.aa.sample_count());
            // The sky PSO bakes the sample count too — rebuild it for the new scene-color
            // target, or the sky pass draws MSAA color with a 1× pipeline.
            self.sky
                .set_sample_count(&self.device, &self.descriptors, self.aa.sample_count())?;
            self.stars
                .set_sample_count(&self.device, self.aa.sample_count())?;
        }
        // Both views share the offscreen sample count, so every view's AA targets rebuild: a later
        // `set-active-view` must find the inactive view's MSAA targets already sized.
        for view in &mut self.views {
            view.build_aa_targets(&self.device, &self.descriptors, self.aa)?;
        }
        Ok(())
    }

    /// The current TAA resolve tuning (read by the control `get-taa-params`).
    pub fn taa_params(&self) -> crate::TaaParams {
        self.taa_params
    }

    /// Sets the TAA resolve tuning. Takes effect next frame (the push is rebuilt each frame
    /// from this state); no GPU idle / PSO rebuild — it is push-constant data, not baked state.
    pub fn set_taa_params(&mut self, params: crate::TaaParams) {
        self.taa_params = params;
    }

    /// Selects the AA mode by name (`"off"` / `"fxaa"` / `"taa"` / `"msaa2"` / `"msaa4"` /
    /// `"msaa8"`) — the control-plane / CLI entry.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] if the device cannot idle or the AA targets cannot be recreated.
    pub fn set_aa_mode(&mut self, mode: &str) -> Result<()> {
        let (samples, fxaa, taa) = match mode {
            "fxaa" => (1, true, false),
            "taa" => (1, false, true),
            "msaa2" => (2, false, false),
            "msaa4" => (4, false, false),
            "msaa8" => (8, false, false),
            _ => (1, false, false),
        };
        self.set_aa(samples, fxaa, taa)
    }

    /// The current AA mode as a name (`"off"` / `"fxaa"` / `"taa"` / `"msaaN"`).
    pub fn aa_mode(&self) -> String {
        self.aa.mode()
    }

    /// Toggles the depth pre-pass (lays down scene depth before the shaded scene pass).
    pub fn set_depth_prepass(&mut self, enabled: bool) {
        self.use_depth_prepass = enabled;
    }

    /// Whether the depth pre-pass is on.
    pub fn depth_prepass_enabled(&self) -> bool {
        self.use_depth_prepass
    }

    /// Sets the tonemap exposure in stops; the tonemap pass applies `exp2(this)`.
    pub fn set_exposure(&mut self, ev: f32) {
        self.exposure_ev = ev;
    }

    /// The current tonemap exposure in stops.
    pub fn exposure_ev(&self) -> f32 {
        self.exposure_ev
    }

    /// Sets low-light rod/cone adaptation strength for the next tonemap pass.
    pub fn set_night_factor(&mut self, factor: f32) {
        self.night_factor = factor.clamp(0.0, 1.0);
    }

    /// The current low-light adaptation strength.
    pub fn night_factor(&self) -> f32 {
        self.night_factor
    }

    /// Sets the bloom parameters: the enable flag, the energy-conserving composite `intensity`,
    /// the tent-upsample `scatter` radius (UV units), the `tint`, and the soft-knee `threshold`
    /// (`0.0` = thresholdless). Applied to the next frame's pre-tonemap bloom pass.
    pub fn set_bloom(
        &mut self,
        enabled: bool,
        intensity: f32,
        scatter: f32,
        tint: [f32; 3],
        threshold: f32,
    ) {
        self.bloom_enabled = enabled;
        self.bloom_intensity = intensity.max(0.0);
        self.bloom_scatter = scatter.clamp(0.0, 1.0);
        self.bloom_tint = tint;
        self.bloom_threshold = threshold.max(0.0);
    }

    /// Whether the scene-linear bloom pyramid runs before the tonemap.
    pub fn bloom_enabled(&self) -> bool {
        self.bloom_enabled
    }

    /// The energy-conserving bloom composite weight.
    pub fn bloom_intensity(&self) -> f32 {
        self.bloom_intensity
    }

    /// The bloom tent-upsample scatter radius in UV units.
    pub fn bloom_scatter(&self) -> f32 {
        self.bloom_scatter
    }

    /// The bloom tint (multiplies the composited bloom).
    pub fn bloom_tint(&self) -> [f32; 3] {
        self.bloom_tint
    }

    /// The bloom soft-knee prefilter threshold (`0.0` = thresholdless).
    pub fn bloom_threshold(&self) -> f32 {
        self.bloom_threshold
    }

    /// Sets the lens-dirt mask texture (`id`/`texture` together; `id == 0` + `None` clears it) that
    /// the bloom composite multiplies the accumulated pyramid by. An absent texture binds the 1×1
    /// white fallback (mask = 1 ⇒ identity).
    pub fn set_bloom_dirt_texture(&mut self, id: u64, texture: Option<Arc<crate::GpuTexture>>) {
        self.bloom_dirt_texture_id = id;
        self.bloom_dirt_texture = texture;
    }

    /// Sets the lens-dirt mix (`0.0` = no dirt, clamped to `[0, 1]`) and its tint.
    pub fn set_bloom_dirt_params(&mut self, intensity: f32, tint: [f32; 3]) {
        self.bloom_dirt_intensity = intensity.clamp(0.0, 1.0);
        self.bloom_dirt_tint = tint;
    }

    /// The lens-dirt mask asset id (`0` = none).
    pub fn bloom_dirt_texture(&self) -> u64 {
        self.bloom_dirt_texture_id
    }

    /// The lens-dirt mix fraction.
    pub fn bloom_dirt_intensity(&self) -> f32 {
        self.bloom_dirt_intensity
    }

    /// The lens-dirt tint.
    pub fn bloom_dirt_tint(&self) -> [f32; 3] {
        self.bloom_dirt_tint
    }

    /// Sets the anamorphic streak: the `enabled` toggle, the horizontal `ratio` squeeze (`~2`), the
    /// streak `tint`, and the `intensity` add weight. `ratio` is clamped `≥ 1`, `intensity ≥ 0`.
    pub fn set_bloom_anamorphic(
        &mut self,
        enabled: bool,
        ratio: f32,
        tint: [f32; 3],
        intensity: f32,
    ) {
        self.bloom_anamorphic_enabled = enabled;
        self.bloom_anamorphic_ratio = ratio.max(1.0);
        self.bloom_anamorphic_tint = tint;
        self.bloom_anamorphic_intensity = intensity.max(0.0);
    }

    /// Whether the anamorphic streak runs.
    pub fn bloom_anamorphic_enabled(&self) -> bool {
        self.bloom_anamorphic_enabled
    }

    /// The anamorphic horizontal squeeze.
    pub fn bloom_anamorphic_ratio(&self) -> f32 {
        self.bloom_anamorphic_ratio
    }

    /// The anamorphic streak tint.
    pub fn bloom_anamorphic_tint(&self) -> [f32; 3] {
        self.bloom_anamorphic_tint
    }

    /// The anamorphic streak add weight.
    pub fn bloom_anamorphic_intensity(&self) -> f32 {
        self.bloom_anamorphic_intensity
    }

    /// Sets the per-upsample-step tint stack (identity `1,1,1` when a level is absent). An empty
    /// vector disables per-mip tinting entirely.
    pub fn set_bloom_mip_tint(&mut self, tint: Vec<[f32; 3]>) {
        self.bloom_mip_tint = tint;
    }

    /// The per-upsample-step tint stack.
    pub fn bloom_mip_tint(&self) -> Vec<[f32; 3]> {
        self.bloom_mip_tint.clone()
    }

    /// Selects the debug render-output mode: `Wireframe` arms the wireframe PSO permutation, the
    /// channel modes fold a debug-output index into the light UBO.
    pub fn set_view_mode(&mut self, mode: ViewMode) {
        self.view_mode = mode;
        self.wireframe = mode == ViewMode::Wireframe;
        self.lighting.set_debug_channel(mode.debug_channel());
    }

    /// The current debug render-output mode.
    pub fn view_mode(&self) -> ViewMode {
        self.view_mode
    }

    /// Toggles the GPU compute-skinning path.
    pub fn set_skinning(&mut self, enabled: bool) {
        self.skinning_enabled = enabled;
    }

    /// Whether GPU skinning is on.
    pub fn skinning_enabled(&self) -> bool {
        self.skinning_enabled
    }

    /// Toggles the GPU compute-displacement path.
    pub fn set_displacement(&mut self, enabled: bool) {
        self.displacement_enabled = enabled;
    }

    /// Whether GPU displacement is on.
    pub fn displacement_enabled(&self) -> bool {
        self.displacement_enabled
    }

    /// Sets the tessellation-quality budget for displaced instances. `None` for a field leaves it
    /// unchanged; the values are clamped to a sane range (cap ∈ [1, 64], min ∈ [1, cap], edge target
    /// ≥ 1 px) so a control caller cannot drive the tessellator into a degenerate or runaway state.
    pub fn set_tessellation_quality(
        &mut self,
        factor_cap: Option<f32>,
        min_factor: Option<f32>,
        edge_length_target: Option<f32>,
    ) {
        if let Some(cap) = factor_cap {
            // Integer cap (the snap/scan clamp grids on it) with the split pass's expressible ceiling;
            // the micro-vertex budget — not this — is the real bound on dense scenes.
            self.tess_factor_cap = cap.round().clamp(1.0, 2048.0);
        }
        if let Some(min) = min_factor {
            self.tess_min_factor = min.clamp(1.0, self.tess_factor_cap);
        }
        // A cap change can leave the min above it — re-clamp so `min ≤ cap` always holds.
        self.tess_min_factor = self.tess_min_factor.min(self.tess_factor_cap);
        if let Some(target) = edge_length_target {
            self.tess_edge_length_target = target.max(1.0);
        }
    }

    /// The current tessellation-quality budget `(factor_cap, min_factor, edge_length_target)`.
    pub fn tessellation_quality(&self) -> (f32, f32, f32) {
        (
            self.tess_factor_cap,
            self.tess_min_factor,
            self.tess_edge_length_target,
        )
    }

    /// Toggles the infinite analytic ground-grid debug overlay.
    pub fn set_show_grid(&mut self, enabled: bool) {
        self.show_grid = enabled;
    }

    /// Whether the ground grid is shown.
    pub fn show_grid(&self) -> bool {
        self.show_grid
    }

    /// Selects native-viewport-host present mode: present blits the post-processed offscreen
    /// straight to the swapchain, with no ui pass.
    pub fn set_present_viewport_only(&mut self, enabled: bool) {
        self.present_viewport_only = enabled;
    }

    /// Whether present-only (native-viewport host) mode is active.
    pub fn present_viewport_only(&self) -> bool {
        self.present_viewport_only
    }
}
