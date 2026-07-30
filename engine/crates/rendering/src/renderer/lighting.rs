use super::*;

impl Renderer {
    /// Writes the current frame's directional + ambient + eye + punctual lights into the per-frame
    /// light UBO/SSBO. Call once per frame before [`Renderer::render_scene_offscreen`].
    ///
    /// # Errors
    ///
    /// Returns [`Error`] if growing the punctual SSBO fails.
    pub fn set_scene_lighting(&mut self, scene: &SceneLighting) -> Result<()> {
        let frame = self.frames.index();
        self.prepare_vsm_frame(frame, scene.direction);
        let mut scene = scene.clone();
        if self.scene_ibl().atmosphere_live() {
            let atmosphere = self.scene_ibl().baked_atmosphere();
            let sun_to_light = -scene.direction.normalize_or_zero();
            let sun_radiance = sun_transmittance(&atmosphere, sun_to_light) * SOLAR_ILLUMINANCE_TOA;
            (scene.color, scene.intensity) = chromatic_light(sun_radiance, scene.intensity);

            let moon_to_light = -scene.moon_direction.normalize_or_zero();
            let phase = (1.0 - sun_to_light.dot(moon_to_light).clamp(-1.0, 1.0)) * 0.5;
            let moon_radiance =
                sun_transmittance(&atmosphere, moon_to_light) * (LUNAR_ILLUMINANCE_FULL * phase);
            (scene.moon_color, scene.moon_intensity) =
                chromatic_light(moon_radiance, scene.moon_intensity);
        }
        // The directional-light travel direction for the fog pass's sun-inscatter lobe, which
        // points its lobe toward `-direction`.
        self.sun_direction = scene.direction.normalize_or_zero();
        self.sun_color = scene.color;
        self.sun_intensity = scene.intensity;
        self.moon_direction = scene.moon_direction.normalize_or_zero();
        self.moon_color = scene.moon_color;
        self.moon_intensity = scene.moon_intensity;
        let cloud_shadow = self.clouds.shadow_projection(
            scene.eye_position,
            -self.sun_direction,
            self.sun_intensity,
            -self.moon_direction,
            self.moon_intensity,
        );
        self.lighting.set_frame_cloud_shadow(cloud_shadow);
        // Probes contribute only when IBL is baked and their toggle is on.
        let ibl = self.scene_ibl();
        let ibl_enabled = ibl.use_ibl && ibl.ready;
        let probes_on = self.reflection.use_probes && ibl.ready;
        let probe_count = if probes_on {
            self.reflection.frame_probe_count()
        } else {
            0
        };
        self.lighting.set_frame_ibl(ibl_enabled, probe_count);
        let (ddgi_min, ddgi_extent) = self.ddgi.volume();
        self.lighting.set_frame_ddgi(
            self.ddgi.enabled(),
            ddgi_min,
            ddgi_extent,
            self.ddgi.probe_count_ubo(),
            self.ddgi.scroll_base_ubo(),
        );
        // The mesh fragment marches the Global Distance Field along the reflection vector only
        // when its cascade clipmap composited this frame.
        self.lighting
            .set_frame_sdf_occlusion(self.want_sky_occlusion());
        // The SSR flag (extra_flags.x): the mesh blends the SSR map only when the trace ran.
        self.lighting
            .set_frame_ssr(self.ssao.use_ssr && self.ssao.ready);
        // The RT-reflection flag (extra_flags.y) + the previous frame's view-proj for reprojecting
        // an RT hit into prev_color. Gated on the toggle rather than this-frame TLAS readiness,
        // which lags a frame; the set-6 TLAS is always a valid (possibly empty) AS.
        let view = &self.views[self.active_view.index()];
        let rt_refl = self.rt.use_rt_reflections() && self.ssao.ready && view.prev_view_proj_valid;
        let prev_vp = view.prev_view_proj;
        self.lighting.set_frame_rt_reflections(rt_refl, prev_vp);
        // The ray-query shadow gate (point_shadow_meta.z) also requires a TLAS built this frame:
        // the shadow term has no screen-space fallback, so tracing an empty scene would light
        // every surface unshadowed for a frame.
        self.lighting
            .set_frame_rt_shadows(self.rt.shadows_enabled());
        // The forward transparent path samples the integration volume only when volumetric fog is
        // authored, matching the composite's gate.
        self.lighting.set_frame_froxel_fog(
            self.fog.enabled && self.fog.volumetric,
            crate::froxel_fog::FROXEL_NEAR,
            crate::FROXEL_FAR,
        );
        self.lighting
            .set_scene_lighting(&self.descriptors, frame, &scene)
    }

    /// Folds the frame's wind parameters and local sources into the light UBO
    /// words, the deformation pushes, and the source ring every GPU sampler reads.
    pub fn set_wind(
        &mut self,
        wind: &SceneWind,
        sources: &[saffron_wind::LocalWindSource],
    ) -> Result<()> {
        // A live edit to the wind field is a discontinuity, not motion: the deformation jumps
        // rather than sweeping, so last frame's pixels describe positions this frame's geometry
        // never occupied. The comparison is on the authored field and the source set, never the
        // clock — wind advances every frame by design, and keying on it would reset forever.
        let authored_changed = wind_authored_digest(wind) != self.wind_authored_digest;
        let sources_changed = self.wind_source_count != sources.len()
            || sources
                .iter()
                .take(64)
                .zip(self.wind_source_digest.iter())
                .any(|(source, previous)| wind_source_digest(source) != *previous);
        if authored_changed || sources_changed {
            self.wind_discontinuity = true;
        }
        self.wind_authored_digest = wind_authored_digest(wind);
        self.wind_source_count = sources.len();
        self.wind_source_digest = sources.iter().take(64).map(wind_source_digest).collect();
        self.scene_wind = *wind;
        while self.wind_source_ring.len() < crate::MAX_FRAMES_IN_FLIGHT {
            self.wind_source_ring.push(crate::Buffer::new(
                self.device.resources(),
                64 * size_of::<crate::GpuWindSourceRecord>() as u64,
                vk::BufferUsageFlags::STORAGE_BUFFER | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS,
                &vk_mem::AllocationCreateInfo {
                    usage: vk_mem::MemoryUsage::Auto,
                    flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE
                        | vk_mem::AllocationCreateFlags::MAPPED,
                    ..Default::default()
                },
            )?);
        }
        let frame = self.frames.index();
        let records: Vec<crate::GpuWindSourceRecord> = sources
            .iter()
            .take(64)
            .map(|source| crate::GpuWindSourceRecord {
                position: [
                    source.position.x as f32,
                    source.position.y as f32,
                    source.position.z as f32,
                ],
                kind: match source.kind {
                    saffron_wind::WindSourceKind::Directional => 0,
                    saffron_wind::WindSourceKind::Point => 1,
                    saffron_wind::WindSourceKind::Vortex => 2,
                    saffron_wind::WindSourceKind::Wake => 3,
                    saffron_wind::WindSourceKind::Volume => 4,
                },
                direction: source.direction.to_array(),
                strength: source.strength,
                radius: source.radius,
                falloff: source.falloff,
                reserved: [0.0; 2],
            })
            .collect();
        let ring = &self.wind_source_ring[frame];
        if !records.is_empty() {
            // SAFETY: HOST_VISIBLE + MAPPED; the frame slot's fence passed before reuse.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    records.as_ptr().cast::<u8>(),
                    ring.mapped_ptr(),
                    records.len() * size_of::<crate::GpuWindSourceRecord>(),
                );
            }
        }
        let sources_address = self.device.buffer_device_address(ring.handle());
        let radians = wind.orientation.to_radians();
        self.lighting.set_frame_wind(
            Vec4::new(radians.sin(), radians.cos(), wind.speed, wind.gust),
            Vec4::new(
                wind.turbulence_roughness,
                wind.gust_frequency,
                wind.reference_height,
                wind.height_exponent,
            ),
            saffron_geometry::glam::UVec4::new(
                wind.turbulence_octaves,
                wind.seed,
                records.len() as u32,
                0,
            ),
            wind.time_s as f32,
            sources_address,
        );
        Ok(())
    }

    /// Stages world-space interaction impulses for this frame's field step; the
    /// staged list drains when the frame records.
    pub fn submit_interaction_impulses(&mut self, impulses: &[crate::InteractionImpulse]) {
        self.interaction_impulses.extend_from_slice(impulses);
    }

    /// Arms the directional virtual-shadow sampling; `casting` (gated by the master
    /// shadow toggle) drives whether the sun shadows this frame.
    pub fn set_directional_shadow(&mut self, casting: bool) {
        self.lighting.set_directional_shadow(casting);
    }

    /// Arms the spot's virtual-shadow space with its perspective transform + its index
    /// in the per-frame light list.
    pub fn set_spot_shadow(&mut self, light_view_proj: Mat4, light_index: u32, casting: bool) {
        self.lighting
            .set_spot_shadow(light_view_proj, light_index, casting);
    }

    /// Arms the point light's six virtual face spaces with its world position + far
    /// plane + its index.
    pub fn set_point_shadow(
        &mut self,
        light_pos: saffron_geometry::glam::Vec3,
        far_plane: f32,
        light_index: u32,
        casting: bool,
    ) {
        self.lighting
            .set_point_shadow(light_pos, far_plane, light_index, casting);
    }

    /// Whether a wind edit landed since the last frame, clearing the flag.
    ///
    /// Wind advancing on its own clock is motion, not a discontinuity, and never sets this.
    pub fn take_wind_discontinuity(&mut self) -> bool {
        std::mem::take(&mut self.wind_discontinuity)
    }

    /// The active view's world wind sway record buffer, once a frame has created it.
    /// Executor raster passes declare their device-address read on it through this.
    pub(super) fn wind_records_handle(&self) -> Option<vk::Buffer> {
        self.wind_deform_records
            .get(&self.active_view.gpu_scene_world().0)
            .map(|records| records.buffer.handle())
    }

    /// Captures one instance slot's wind prepass record: the sway at both frame times, the
    /// interaction displacement at both, the branch-mode quadrature and amplitudes, the height
    /// scale, and the bounds slack.
    ///
    /// One-shot, never per frame: the records are device-local and this idles the queue to read
    /// them. Returns `None` before the first frame has created the buffer, or for a slot past
    /// its capacity.
    pub fn capture_wind_record(
        &self,
        slot: u32,
    ) -> crate::Result<Option<crate::GpuWindInstanceRecord>> {
        let world = self.active_view.gpu_scene_world();
        let Some(records) = self.wind_deform_records.get(&world.0) else {
            return Ok(None);
        };
        if slot >= records.capacity {
            return Ok(None);
        }
        let stride = size_of::<crate::GpuWindInstanceRecord>() as u64;
        let staging = crate::Buffer::new(
            self.device.resources(),
            stride,
            vk::BufferUsageFlags::TRANSFER_DST,
            &vk_mem::AllocationCreateInfo {
                usage: vk_mem::MemoryUsage::AutoPreferHost,
                flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_RANDOM
                    | vk_mem::AllocationCreateFlags::MAPPED,
                ..Default::default()
            },
        )?;
        let source = records.buffer.handle();
        let destination = staging.handle();
        let offset = u64::from(slot) * stride;
        self.device.one_shot_transfer(|raw, cmd| {
            // SAFETY: the ash seam. Both buffers outlive the submit this records into,
            // and the copy is inside each one's allocated size.
            unsafe {
                raw.cmd_copy_buffer(
                    cmd,
                    source,
                    destination,
                    &[vk::BufferCopy {
                        src_offset: offset,
                        dst_offset: 0,
                        size: stride,
                    }],
                );
            }
        })?;
        // SAFETY: HOST_VISIBLE + MAPPED, one record long, and the transfer's fence was
        // waited before this returns.
        let record =
            unsafe { std::ptr::read(staging.mapped_ptr().cast::<crate::GpuWindInstanceRecord>()) };
        Ok(Some(record))
    }
}

fn chromatic_light(radiance: Vec3, trim: f32) -> (Vec3, f32) {
    let luminance = radiance.dot(Vec3::new(0.2126, 0.7152, 0.0722));
    if luminance <= f32::EPSILON {
        (Vec3::ZERO, 0.0)
    } else {
        (radiance / luminance, luminance * trim.max(0.0))
    }
}

/// A digest of the authored global wind field, excluding `time_s`: the clock advances every
/// frame, and folding it in would make every frame look like an edit.
fn wind_authored_digest(wind: &crate::SceneWind) -> u64 {
    let mut digest = 0xcbf2_9ce4_8422_2325_u64;
    let mut fold = |value: u64| {
        digest ^= value;
        digest = digest.wrapping_mul(0x0000_0100_0000_01b3);
    };
    fold(u64::from(wind.orientation.to_bits()));
    fold(u64::from(wind.speed.to_bits()));
    fold(u64::from(wind.gust.to_bits()));
    fold(u64::from(wind.turbulence_octaves));
    fold(u64::from(wind.turbulence_roughness.to_bits()));
    fold(u64::from(wind.gust_frequency.to_bits()));
    fold(u64::from(wind.reference_height.to_bits()));
    fold(u64::from(wind.height_exponent.to_bits()));
    fold(u64::from(wind.seed));
    digest
}

/// A stable digest of one local wind source's authored state, for detecting an edit. Compares
/// bit patterns rather than floats, so a value that round-trips unchanged compares equal.
fn wind_source_digest(source: &saffron_wind::LocalWindSource) -> u64 {
    let mut digest = 0xcbf2_9ce4_8422_2325_u64;
    let mut fold = |value: u64| {
        digest ^= value;
        digest = digest.wrapping_mul(0x0000_0100_0000_01b3);
    };
    fold(u64::from((source.position.x as f32).to_bits()));
    fold(u64::from((source.position.y as f32).to_bits()));
    fold(u64::from((source.position.z as f32).to_bits()));
    fold(u64::from(source.direction.x.to_bits()));
    fold(u64::from(source.direction.y.to_bits()));
    fold(u64::from(source.direction.z.to_bits()));
    fold(u64::from(source.strength.to_bits()));
    fold(u64::from(source.radius.to_bits()));
    fold(u64::from(source.falloff.to_bits()));
    fold(source.kind as u64);
    digest
}

#[cfg(test)]
mod tests {
    /// The clock must not read as an edit: `SceneWind` carries `time_s`, which advances every
    /// frame, and comparing the whole struct would raise a discontinuity continuously.
    #[test]
    fn the_wind_clock_is_not_an_edit() {
        let mut wind = crate::SceneWind::default();
        let base = super::wind_authored_digest(&wind);
        wind.time_s += 1234.5;
        assert_eq!(
            super::wind_authored_digest(&wind),
            base,
            "the clock is not an edit"
        );
        wind.speed += 1.0;
        assert_ne!(
            super::wind_authored_digest(&wind),
            base,
            "a speed change is"
        );
    }

    /// A wind edit is a discontinuity; wind advancing on its own clock is not. A detector keyed
    /// on the field's value over time would fire continuously and disable temporal accumulation.
    #[test]
    fn only_a_wind_edit_counts_as_a_discontinuity() {
        use saffron_wind::{LocalWindSource, WindSourceKind};
        let source = |strength: f32| LocalWindSource {
            kind: WindSourceKind::Directional,
            position: saffron_geometry::glam::DVec3::new(1.0, 2.0, 3.0),
            direction: saffron_geometry::glam::Vec3::X,
            strength,
            radius: 4.0,
            falloff: 0.5,
        };
        assert_eq!(
            super::wind_source_digest(&source(1.0)),
            super::wind_source_digest(&source(1.0))
        );
        assert_ne!(
            super::wind_source_digest(&source(1.0)),
            super::wind_source_digest(&source(1.5))
        );
        let mut moved = source(1.0);
        moved.radius = 4.25;
        assert_ne!(
            super::wind_source_digest(&source(1.0)),
            super::wind_source_digest(&moved)
        );
        let mut turned = source(1.0);
        turned.direction = saffron_geometry::glam::Vec3::Y;
        assert_ne!(
            super::wind_source_digest(&source(1.0)),
            super::wind_source_digest(&turned)
        );
    }
}
