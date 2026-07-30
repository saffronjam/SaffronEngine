use super::*;

/// The authored analytic height/distance fog, resolved from the scene each frame. The broad
/// layer plus an optional ground layer sum into one closed-form optical depth.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FogRenderSettings {
    /// Whether the fog composite runs this frame.
    pub enabled: bool,
    /// Broad-layer sigma at `height`.
    pub density: f32,
    /// In-scatter tint (multiplied by the sky-view LUT when the atmosphere is live).
    pub albedo: Vec3,
    /// World-up reference height of the broad layer.
    pub height: f32,
    /// Exponential density falloff with world-up distance.
    pub height_falloff: f32,
    /// Fog begins this far from the eye.
    pub start_distance: f32,
    /// Clamps `1 - transmittance`.
    pub max_opacity: f32,
    /// Constant in-medium emission.
    pub emissive: Vec3,
    /// Sun-through-haze lobe color.
    pub directional_color: Vec3,
    /// Lobe sharpness.
    pub directional_exponent: f32,
    /// Ground-haze layer sigma; `0` disables it.
    pub layer2_density: f32,
    /// Ground-haze exponential density falloff.
    pub layer2_falloff: f32,
    /// Ground-haze world-up reference height.
    pub layer2_height: f32,
    /// The froxel volumetric path runs instead of the analytic closed form. The analytic height
    /// density is injected as the froxel base medium — never applied twice.
    pub volumetric: bool,
    /// Constant scattering-medium extinction floor (added to the analytic height density).
    pub base_density: f32,
    /// Single-scattering albedo (`sigma_s = albedo * sigma_t`).
    pub scatter_albedo: f32,
    /// Henyey-Greenstein phase anisotropy (`g`), forward-scattering for `g > 0`.
    pub phase_g: f32,
    /// The froxel-grid quality tier (grid dimensions) for the volumetric path.
    pub quality: crate::FroxelQuality,
    /// Temporal reprojection blend: the fresh-sample weight per frame (`0.05` default).
    pub history_blend: f32,
    /// Clamp the reprojected history to a band of the fresh sample (firefly / ghost suppression).
    pub neighborhood_clamp: bool,
    /// Cap each light's per-froxel in-scatter before accumulation (`0` = off).
    pub light_clamp: f32,
    /// Fill + composite the Hillaire-2020 aerial-perspective volume; a no-op without a live
    pub aerial_perspective: bool,
    /// Aerial-perspective in-scatter strength multiplier.
    pub aerial_intensity: f32,
}

impl Default for FogRenderSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            density: 0.02,
            albedo: Vec3::new(0.5, 0.6, 0.7),
            height: 0.0,
            height_falloff: 0.2,
            start_distance: 0.0,
            max_opacity: 1.0,
            emissive: Vec3::ZERO,
            directional_color: Vec3::new(1.0, 0.9, 0.7),
            directional_exponent: 8.0,
            layer2_density: 0.0,
            layer2_falloff: 0.5,
            layer2_height: 0.0,
            volumetric: false,
            base_density: 0.02,
            scatter_albedo: 0.9,
            phase_g: 0.6,
            quality: crate::FroxelQuality::Medium,
            history_blend: 0.05,
            neighborhood_clamp: false,
            light_clamp: 0.0,
            aerial_perspective: false,
            aerial_intensity: 1.0,
        }
    }
}

/// The height-fog compute pass's uniform, matching `height_fog.slang`'s `FogParams`. `layer0` /
/// `layer1` pack `(density, heightFalloff, height, pad)`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct FogParams {
    inv_view_proj: [[f32; 4]; 4],
    camera_pos: [f32; 3],
    max_opacity: f32,
    albedo: [f32; 3],
    start_distance: f32,
    emissive: [f32; 3],
    dir_exponent: f32,
    sun_dir: [f32; 3],
    use_sky_lut: f32,
    dir_color: [f32; 3],
    _pad0: f32,
    layer0: [f32; 4],
    layer1: [f32; 4],
    /// `x` = mode (0 analytic, 1 volumetric), `y` = froxel near, `z` = froxel far (the Z
    /// distribution the composite W mapping inverts), `w` = fog debug view.
    froxel: [f32; 4],
    /// The camera view direction (world), for the froxel W depth projection.
    cam_forward: [f32; 3],
    /// `1` when the analytic/volumetric fog contributes this frame, `0` when the composite runs
    fog_enabled: f32,
    /// Aerial perspective: `x` = enabled, `y` = AP near, `z` = AP far (the exponential-Z
    aerial: [f32; 4],
    /// `x` = full-resolution cloud scatter/transmittance is live.
    cloud: [f32; 4],
    /// Atmosphere physical and camera block shared with the AP volume fill.
    ap: crate::AerialParamsUbo,
}

impl Renderer {
    /// Appends the froxel volumetric-fog inject + integrate compute passes: `fog_inject` fills the
    /// scatter volume (per-froxel extinction + shadowed in-scatter over the clustered light list,
    /// HG phase), `fog_integrate` marches it front-to-back into the integration volume the
    /// composite samples, and a barrier-only pass rests that volume in ShaderReadOnly. Returns the
    /// scatter + integration external slots for the layout write-back, or `None` when it skipped.
    pub(super) fn add_froxel_fog_passes(
        &self,
        graph: &mut RenderGraph,
        pipelines: &FramePipelines,
        cloud_shadow: Option<RgResource>,
        frame: usize,
    ) -> Option<(usize, usize, usize)> {
        let (Some(inject), Some(integrate)) = (&pipelines.fog_inject, &pipelines.fog_integrate)
        else {
            return None;
        };

        // This frame's froxel-grid UBO: the froxel-center reconstruction matrices, the active-tier
        // grid dims, the exponential-Z near/far the composite's W mapping inverts, and the
        // temporal reprojection state (previous view-proj, jitter, blend and clamp knobs).
        let view_m = self.ssao.view();
        let inv_view = view_m.inverse();
        let inv_proj = (self.scene_view_proj_unjittered() * inv_view).inverse();
        let extent = self.views[self.active_view.index()].scaled_render_extent();
        let (gx, gy, gz) = self.froxel.grid();
        let active_view = &self.views[self.active_view.index()];
        let prev_view_proj = active_view.prev_view_proj;
        let jitter_index = active_view.jitter_index;
        // History is reusable only once the previous frame's camera transform is valid (no cut) AND
        // the history volume carries content (not the first frame after a reset / quality switch).
        let history_valid = active_view.prev_view_proj_valid && self.froxel.history_ready();
        let jitter = self.active_view_jitter();
        let f = self.fog;
        let grid_params = crate::FogGridParams {
            inverse_projection: inv_proj,
            inverse_view: inv_view,
            prev_view_proj,
            grid_size: saffron_geometry::glam::UVec4::new(gx, gy, gz, 0),
            screen_size: saffron_geometry::glam::Vec4::new(
                extent.width as f32,
                extent.height as f32,
                0.0,
                0.0,
            ),
            z_planes: saffron_geometry::glam::Vec4::new(
                crate::froxel_fog::FROXEL_NEAR,
                crate::FROXEL_FAR,
                crate::FROXEL_FAR,
                0.0,
            ),
            temporal: saffron_geometry::glam::Vec4::new(
                f.history_blend,
                if history_valid { 1.0 } else { 0.0 },
                if f.neighborhood_clamp { 1.0 } else { 0.0 },
                f.light_clamp,
            ),
            jitter: saffron_geometry::glam::Vec4::new(
                jitter.x,
                jitter.y,
                jitter_index as f32,
                self.fog_time,
            ),
        };
        self.froxel.update_grid(&grid_params);

        // Upload this frame's local fog volumes into the inject SSBO; the count rides the push so the
        // injection loop bounds itself without a separate uniform.
        let volume_count = self.froxel.update_fog_volumes(&self.fog_volumes);

        // The medium push: the analytic height density (injected as the froxel base medium, never
        // applied twice at composite), the scattering albedo/phase, and the constant emission.
        let eye = inv_view.col(3).truncate();
        let push_vals: [f32; 16] = [
            eye.x,
            eye.y,
            eye.z,
            f.base_density,
            f.emissive.x,
            f.emissive.y,
            f.emissive.z,
            f.scatter_albedo,
            f.density,
            f.height_falloff,
            f.height,
            f.phase_g,
            f.layer2_density,
            f.layer2_falloff,
            f.layer2_height,
            f32::from_bits(volume_count),
        ];

        // This frame's inject target (written in GENERAL) and last frame's history (sampled in
        // ShaderReadOnly). Both scatter volumes ride external slots so their layouts survive the
        // frame boundary — the ping-pong swaps their roles.
        let (write_img, write_view, write_layout) = self.froxel.scatter_write_import();
        let write_slot = graph.alloc_external_state(crate::RgExternalState::new(write_layout));
        let write_res =
            graph.import_image_3d(write_img, write_view, write_layout, Some(write_slot));
        let (hist_img, hist_view, hist_layout) = self.froxel.scatter_history_import();
        let hist_slot = graph.alloc_external_state(crate::RgExternalState::new(hist_layout));
        let hist_res = graph.import_image_3d(hist_img, hist_view, hist_layout, Some(hist_slot));
        let (integ_img, integ_view, integ_layout) = self.froxel.integration_import();
        let integ_slot = graph.alloc_external_state(crate::RgExternalState::new(integ_layout));
        let integ_res =
            graph.import_image_3d(integ_img, integ_view, integ_layout, Some(integ_slot));
        let cluster_res = graph.import_buffer(self.lighting.cluster_buffer(frame), None);
        let (light_buf, _) = self.lighting.light_list_buffer(frame);
        let light_res = graph.import_buffer(light_buf, None);

        let light_set = self.lighting.light_set(frame);
        let volume_set = self.froxel.inject_set();
        {
            let inject = Arc::clone(inject);
            let handle = inject.handle();
            let layout = inject.layout();
            let raw_body = self.device.raw().clone();
            let dispatch = (gx.div_ceil(8), gy.div_ceil(8), gz.div_ceil(4));
            let mut pass = RgPass::compute("fog-inject")
                .access(write_res, RgUsage::StorageImageRwCompute)
                .access(hist_res, RgUsage::SampledReadCompute)
                .access(cluster_res, RgUsage::StorageReadCompute)
                .access(light_res, RgUsage::StorageReadCompute);
            if let Some(resource) = cloud_shadow {
                pass = pass.access(resource, RgUsage::SampledReadCompute);
            }
            let pass = pass.body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                // SAFETY: the ash seam. The PSO/sets are valid this frame; the dispatch covers the
                // froxel grid (8×8×4 per group). Set 0 is the reused mesh light set, set 1 the fog.
                unsafe {
                    raw_body.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, handle);
                    raw_body.cmd_bind_descriptor_sets(
                        cmd,
                        vk::PipelineBindPoint::COMPUTE,
                        layout,
                        0,
                        &[light_set, volume_set],
                        &[],
                    );
                    raw_body.cmd_push_constants(
                        cmd,
                        layout,
                        vk::ShaderStageFlags::COMPUTE,
                        0,
                        bytemuck::cast_slice(&push_vals),
                    );
                    raw_body.cmd_dispatch(cmd, dispatch.0, dispatch.1, dispatch.2);
                }
                drop(inject);
            });
            graph.add_pass(pass);
        }
        {
            let integrate = Arc::clone(integrate);
            let handle = integrate.handle();
            let layout = integrate.layout();
            let integrate_set = self.froxel.integrate_set();
            let raw_body = self.device.raw().clone();
            let dispatch = (gx.div_ceil(8), gy.div_ceil(8), 1);
            let pass = RgPass::compute("fog-integrate")
                .access(write_res, RgUsage::StorageImageRwCompute)
                .access(integ_res, RgUsage::StorageImageRwCompute)
                .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                    // SAFETY: the ash seam. One thread per froxel column, serial over Z.
                    unsafe {
                        raw_body.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, handle);
                        raw_body.cmd_bind_descriptor_sets(
                            cmd,
                            vk::PipelineBindPoint::COMPUTE,
                            layout,
                            0,
                            &[integrate_set],
                            &[],
                        );
                        raw_body.cmd_dispatch(cmd, dispatch.0, dispatch.1, dispatch.2);
                    }
                    drop(integrate);
                });
            graph.add_pass(pass);
        }
        // Rest the integration volume in ShaderReadOnly for the composite's trilinear sample.
        let read_pass = RgPass::compute("fog-integration-read")
            .access(integ_res, RgUsage::SampledReadCompute)
            .body(|_cmd, _scopes: &mut NestedScopeRecorder| {});
        graph.add_pass(read_pass);

        Some((write_slot, hist_slot, integ_slot))
    }

    /// Appends the aerial-perspective fill pass: one compute dispatch ray-marches the atmosphere
    /// transmittance + multiscatter LUTs into the `32³` AP volume, bounded at each froxel center's
    /// distance (Hillaire 2020), then rests it in ShaderReadOnly for the composite's binding-5
    /// sample. Returns the volume's external slot for the layout write-back, or `None` when the
    /// atmosphere is not live or AP is not authored.
    pub(super) fn add_aerial_perspective_pass(
        &self,
        graph: &mut RenderGraph,
        pipelines: &FramePipelines,
    ) -> Option<usize> {
        let pipeline = pipelines.aerial.as_ref()?;

        let params = self.aerial_params(self.fog.aerial_intensity);
        self.aerial.update_params(&params);

        let (vol_img, vol_view, vol_layout) = self.aerial.volume_import();
        let vol_slot = graph.alloc_external_state(crate::RgExternalState::new(vol_layout));
        let vol_res = graph.import_image_3d(vol_img, vol_view, vol_layout, Some(vol_slot));

        let fill_set = self.aerial.fill_set();
        {
            let pipeline = Arc::clone(pipeline);
            let handle = pipeline.handle();
            let layout = pipeline.layout();
            let raw_body = self.device.raw().clone();
            let groups = crate::AP_GRID.div_ceil(4);
            let pass = RgPass::compute("aerial-perspective")
                .access(vol_res, RgUsage::StorageImageRwCompute)
                .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                    // SAFETY: the ash seam. One thread per froxel over the 32³ grid (4×4×4/group).
                    unsafe {
                        raw_body.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, handle);
                        raw_body.cmd_bind_descriptor_sets(
                            cmd,
                            vk::PipelineBindPoint::COMPUTE,
                            layout,
                            0,
                            &[fill_set],
                            &[],
                        );
                        raw_body.cmd_dispatch(cmd, groups, groups, groups);
                    }
                    drop(pipeline);
                });
            graph.add_pass(pass);
        }
        // Rest the AP volume in ShaderReadOnly for the composite's trilinear sample (fog set binding 5).
        let read_pass = RgPass::compute("aerial-perspective-read")
            .access(vol_res, RgUsage::SampledReadCompute)
            .body(|_cmd, _scopes: &mut NestedScopeRecorder| {});
        graph.add_pass(read_pass);

        Some(vol_slot)
    }

    fn aerial_params(&self, intensity: f32) -> crate::AerialParamsUbo {
        let inverse_view = self.ssao.view().inverse();
        let inverse_projection = (self.scene_view_proj_unjittered() * inverse_view).inverse();
        let atmosphere = self.scene_ibl().baked_atmosphere();
        let (sun_direction, sun_intensity) = self.scene_ibl().baked_sun();
        crate::AerialParamsUbo {
            inverse_projection,
            inverse_view,
            sun_dir: sun_direction.normalize_or_zero().extend(sun_intensity),
            rayleigh: atmosphere
                .rayleigh_scattering
                .extend(atmosphere.rayleigh_scale_height),
            ozone: atmosphere
                .ozone_absorption
                .extend(atmosphere.mie_scattering),
            params0: Vec4::new(
                atmosphere.planet_radius,
                atmosphere.atmosphere_height,
                atmosphere.mie_scale_height,
                atmosphere.mie_anisotropy,
            ),
            params1: Vec4::new(
                atmosphere.sun_disk_angular_radius,
                atmosphere.sun_disk_intensity,
                0.0,
                intensity,
            ),
            ap_planes: Vec4::new(
                crate::froxel_fog::FROXEL_NEAR,
                crate::AP_FAR_M,
                crate::AP_GRID as f32,
                1.0e-3,
            ),
        }
    }

    /// Appends the analytic height & distance fog composite: an in-place compute pass over the
    /// scene depth that blends `scene*T + inscatter*(1-T)` into the offscreen `color` before
    /// bloom. Reads `color` (`StorageImageRwCompute`, GENERAL) and `depth` (`SampledReadCompute`,
    /// DEPTH aspect); the sky-view LUT is bound on the per-view fog set. Resolves the per-frame
    /// `FogParams` UBO slice from the camera + directional light + authored settings first.
    pub(super) fn add_fog_pass(
        &mut self,
        graph: &mut RenderGraph,
        pipelines: &FramePipelines,
        color: RgResource,
        depth: RgResource,
        cloud: &CloudGraphResult,
        frame: usize,
    ) {
        let Some(pipeline) = &pipelines.fog else {
            return;
        };
        let pipeline = Arc::clone(pipeline);
        let handle = pipeline.handle();
        let layout = pipeline.layout();

        // Resolve the per-frame fog params from the camera + directional light + authored settings.
        let inv_view_proj = self.scene_view_proj_unjittered().inverse();
        let inv_view = self.ssao.view().inverse();
        let eye = inv_view.col(3).truncate();
        let cam_forward = inv_view.transform_vector3(Vec3::NEG_Z).normalize_or_zero();
        // The sun-inscatter lobe points TOWARD the sun: the directional light's travel direction
        // negated (falls back to straight down when there is no directional light).
        let sun_dir = (-self.sun_direction).normalize_or_zero();
        let atmosphere_live = self.scene_ibl().atmosphere_live();
        let use_sky_lut = if atmosphere_live { 1.0 } else { 0.0 };
        let f = self.fog;
        // Aerial perspective folds into this composite only when authored + the atmosphere baked
        // its LUTs; the fog term is neutral when fog itself is off but AP keeps the composite live.
        let ap_active = f.aerial_perspective && atmosphere_live;
        let cloud_present =
            cloud.full_color_resource.is_some() && cloud.full_depth_resource.is_some();
        let params = FogParams {
            inv_view_proj: inv_view_proj.to_cols_array_2d(),
            camera_pos: eye.to_array(),
            max_opacity: f.max_opacity,
            albedo: f.albedo.to_array(),
            start_distance: f.start_distance,
            emissive: f.emissive.to_array(),
            dir_exponent: f.directional_exponent,
            sun_dir: sun_dir.to_array(),
            use_sky_lut,
            dir_color: f.directional_color.to_array(),
            _pad0: 0.0,
            layer0: [f.density, f.height_falloff, f.height, 0.0],
            layer1: [f.layer2_density, f.layer2_falloff, f.layer2_height, 0.0],
            froxel: [
                if f.volumetric { 1.0 } else { 0.0 },
                crate::froxel_fog::FROXEL_NEAR,
                crate::FROXEL_FAR,
                // `.w` = the fog debug view mode (volumetric only): output the froxel in-scatter.
                if self.view_mode == ViewMode::Fog {
                    1.0
                } else {
                    0.0
                },
            ],
            cam_forward: cam_forward.to_array(),
            fog_enabled: if f.enabled { 1.0 } else { 0.0 },
            aerial: [
                if ap_active { 1.0 } else { 0.0 },
                crate::froxel_fog::FROXEL_NEAR,
                crate::AP_FAR_M,
                0.0,
            ],
            cloud: [
                if cloud_present { 1.0 } else { 0.0 },
                if atmosphere_live { 1.0 } else { 0.0 },
                0.0,
                0.0,
            ],
            ap: self.aerial_params(1.0),
        };

        let vi = self.active_view.index();
        self.views[vi].write_fog(frame, &params);
        let set = self.views[vi].fog_set;
        let offset = self.views[vi].fog_ubo_offset(frame);
        let extent = self.views[vi].published_extent();
        let groups = |n: u32| n.div_ceil(8);
        let groups_x = groups(extent.width);
        let groups_y = groups(extent.height);

        let raw_body = self.device.raw().clone();
        let mut pass = RgPass::compute("height-fog")
            .access(color, RgUsage::StorageImageRwCompute)
            .access(depth, RgUsage::SampledReadCompute);
        if let Some(resource) = cloud.full_color_resource {
            pass = pass.access(resource, RgUsage::SampledReadCompute);
        }
        if let Some(resource) = cloud.full_depth_resource {
            pass = pass.access(resource, RgUsage::SampledReadCompute);
        }
        let pass = pass.body(move |cmd, _scopes: &mut NestedScopeRecorder| {
            // SAFETY: the ash seam. The PSO/set are valid this frame; the dispatch covers the
            // viewport (8×8 per group); the dynamic offset addresses this frame's `FogParams` slice.
            unsafe {
                raw_body.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, handle);
                raw_body.cmd_bind_descriptor_sets(
                    cmd,
                    vk::PipelineBindPoint::COMPUTE,
                    layout,
                    0,
                    &[set],
                    &[offset],
                );
                raw_body.cmd_dispatch(cmd, groups_x, groups_y, 1);
            }
            drop(pipeline);
        });
        graph.add_pass(pass);
    }
}
