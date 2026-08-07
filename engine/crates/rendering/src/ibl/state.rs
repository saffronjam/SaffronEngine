//! The [`Ibl`] aggregate: the retained front set, the pending-refresh queue, and the
//! render-graph passes that project diffuse SH and time-slice specular prefiltering.

use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum RefreshKind {
    Rebuild,
    Blend,
}

pub(super) struct PendingRefresh {
    pub(super) scratch: BakeScratch,
    pub(super) params: SkygenParams,
    pub(super) source: EnvSource,
    pub(super) _panorama: Option<Arc<GpuTexture>>,
    pub(super) kind: RefreshKind,
    pub(super) atmosphere_base_written: bool,
    pub(super) first_bake: bool,
}

/// Render-graph handles for the persistent sky-lighting products consumed later in the frame.
#[derive(Clone, Copy)]
pub(crate) struct IblGraphResources {
    /// Raw-radiance second-order spherical harmonics.
    pub sh: RgResource,
    /// Time-sliced prefiltered specular environment.
    pub prefiltered: RgResource,
    pub(super) prefiltered_layout_slot: usize,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct PrefilterPush {
    pub(super) roughness: f32,
    pub(super) blend_alpha: f32,
    pub(super) row_offset: u32,
    pub(super) row_count: u32,
}

pub(super) const PREFILTER_BASE_SLICES: u32 = 9;

/// Image-based lighting: the source environment cube projected into raw-radiance SH and
/// convolved into a roughness-mipped prefiltered specular cube plus a split-sum BRDF LUT and the
/// atmosphere LUT chain. Sampled as the mesh ambient (set 3, bindings 0-2). Baked at
/// startup, re-baked on demand when the sky inputs change.
///
/// Owns its Vulkan handles + an
/// [`Arc`]`<`[`DeviceResources`]`>` so the images (Drop types) free without a live
/// `&Device`; the sampler/set-layout free in [`Drop`], the descriptor set with the pool.
pub struct Ibl {
    pub(super) resources: Arc<DeviceResources>,
    pub(super) live: LiveCapture,
    pub(super) front: IblCubeSet,
    pub(super) back: IblCubeSet,
    pub(super) refresh_target: IblCube,
    pub(super) refresh_target_initialized: bool,
    pub(super) brdf_lut: IblImage,
    pub(super) transmittance_lut: IblImage,
    pub(super) multi_scatter_lut: IblImage,
    pub(super) sky_view_lut: IblImage,
    pub(super) refresh_sky_view_lut: IblImage,
    pub(super) refresh_sky_view_initialized: bool,

    pub(super) sampler: vk::Sampler,
    /// The descriptors' linear repeat sampler (the equirect panorama wraps in longitude;
    /// the clamp IBL sampler would seam the meridian). Borrowed, not owned.
    pub(super) equirect_sampler: vk::Sampler,
    pub(super) sets: [vk::DescriptorSet; MAX_FRAMES_IN_FLIGHT],
    /// Whether the bake has run and set 3 is written.
    pub ready: bool,
    /// Master IBL ambient toggle; false = the flat scalar ambient fallback.
    pub use_ibl: bool,

    pub(super) baked_params: SkygenParams,
    pub(super) pending_params: SkygenParams,
    /// `pending_params` differ from `baked_params` → re-bake armed at the next idle point.
    pub rebake_pending: bool,
    pub(super) atmosphere_dirty: bool,
    pub(super) atmosphere_base_ready: bool,
    pub(super) refresh: Option<PendingRefresh>,
    pub(super) blend_frames_remaining: u8,
    pub(super) source: EnvSource,
    pub(super) baked_source: EnvSource,
    /// The equirect source panorama, held alive across the bake.
    pub(super) env_panorama: Option<Arc<GpuTexture>>,
    pub(super) sh_dirty: bool,
    pub(super) prefilter_active: bool,
    pub(super) prefilter_dirty_during_cycle: bool,
    pub(super) prefilter_slice: u32,
    pub(super) prefilter_frame: u32,
    pub(super) capture_cadence: f32,
}

impl Ibl {
    /// Allocates the IBL images, the linear/clamp/mipped sampler, and the persistent set 3
    /// from the shared pool, then runs the first bake (procedural sky) so set 3 is valid
    /// before the first frame.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] for any failing image/sampler/set/pipeline/bake step.
    pub fn new(device: &Device, descriptors: &Descriptors) -> Result<Self> {
        let resources = Arc::clone(device.resources());
        let mips = IBL_PREFILTER_MIPS;
        // The env cube carries a full mip chain so the prefilter's filtered importance sampling can
        // read coarser (pre-averaged) source mips — the firefly/aliasing fix.
        let front = IblCubeSet::new(&resources)?;
        let back = IblCubeSet::new(&resources)?;
        let refresh_target = IblCube::new(&resources, IBL_ENV_SIZE, 1)?;
        let brdf_lut = IblImage::new(&resources, IBL_LUT_SIZE, IBL_LUT_SIZE)?;
        let transmittance_lut =
            IblImage::new(&resources, ATMOS_TRANSMITTANCE_W, ATMOS_TRANSMITTANCE_H)?;
        let multi_scatter_lut = IblImage::new(
            &resources,
            ATMOS_MULTI_SCATTER_SIZE,
            ATMOS_MULTI_SCATTER_SIZE,
        )?;
        let sky_view_lut = IblImage::new(&resources, ATMOS_SKY_VIEW_W, ATMOS_SKY_VIEW_H)?;
        let refresh_sky_view_lut = IblImage::new(&resources, ATMOS_SKY_VIEW_W, ATMOS_SKY_VIEW_H)?;

        let sampler = create_ibl_sampler(resources.device())?;
        let live = match LiveCapture::new(&resources, descriptors, sampler, front.env.view, mips) {
            Ok(live) => live,
            Err(err) => {
                // SAFETY: the sampler was created above and is not yet owned by `Ibl`.
                unsafe { resources.device().destroy_sampler(sampler, None) };
                return Err(err);
            }
        };
        let mut sets = [vk::DescriptorSet::null(); MAX_FRAMES_IN_FLIGHT];
        for set in &mut sets {
            *set = match descriptors.allocate_set(descriptors.ibl_set_layout()) {
                Ok(set) => set,
                Err(err) => {
                    // SAFETY: the ash seam. The sampler was created just above; free it on
                    // the early return (the images free via their Drop and allocated sets
                    // free with the shared pool).
                    unsafe { resources.device().destroy_sampler(sampler, None) };
                    return Err(err);
                }
            };
        }

        Ok(Self {
            resources,
            live,
            front,
            back,
            refresh_target,
            refresh_target_initialized: false,
            brdf_lut,
            transmittance_lut,
            multi_scatter_lut,
            sky_view_lut,
            refresh_sky_view_lut,
            refresh_sky_view_initialized: false,
            sampler,
            equirect_sampler: descriptors.linear_sampler(),
            sets,
            ready: false,
            use_ibl: true,
            baked_params: SkygenParams::default(),
            pending_params: SkygenParams::default(),
            rebake_pending: false,
            atmosphere_dirty: false,
            atmosphere_base_ready: false,
            refresh: None,
            blend_frames_remaining: 0,
            source: EnvSource::Procedural,
            baked_source: EnvSource::Procedural,
            env_panorama: None,
            sh_dirty: false,
            prefilter_active: false,
            prefilter_dirty_during_cycle: false,
            prefilter_slice: 0,
            prefilter_frame: 0,
            capture_cadence: AtmosphereParams::default().sky_capture_cadence,
        })
    }

    /// The current frame slot's IBL set (set 3 in the mesh pipeline; also the
    /// reflection-probe set).
    pub fn set(&self, frame: usize) -> vk::DescriptorSet {
        self.sets[frame]
    }

    /// The IBL linear/clamp/mipped sampler (shared by the sky + probe sets).
    pub fn sampler(&self) -> vk::Sampler {
        self.sampler
    }

    /// The source environment cube's sampling view (the visible-sky procedural mode).
    pub fn env_cube_view(&self) -> vk::ImageView {
        self.front.env.view
    }

    /// The raw-radiance sky SH buffer.
    pub fn sh_coefficients(&self) -> &Buffer {
        &self.live.sh_coefficients
    }

    /// The global prefiltered specular cube's sampling view (the probe-slot fallback).
    pub fn prefiltered_cube_view(&self) -> vk::ImageView {
        self.live.prefiltered.view
    }

    /// The Hillaire sky-view LUT's sampling view (the height-fog in-scatter tint). Contains a valid
    /// physical-atmosphere evaluation and is in `SHADER_READ_ONLY_OPTIMAL` after the first bake;
    /// consumers use it only when [`Ibl::atmosphere_live`] is true.
    pub fn sky_view_lut_view(&self) -> vk::ImageView {
        self.sky_view_lut.view
    }

    /// The transmittance LUT view (Hillaire 2020), bound into the aerial-perspective pass's descriptor
    /// set with the shared clamp [`Ibl::sampler`]. Contains a valid physical-atmosphere base and is
    /// in `SHADER_READ_ONLY_OPTIMAL` after the first bake.
    pub fn transmittance_view(&self) -> vk::ImageView {
        self.transmittance_lut.view
    }

    /// The multiscatter LUT view (Hillaire 2020), the AP pass's isotropic multiple-scattering source.
    pub fn multi_scatter_view(&self) -> vk::ImageView {
        self.multi_scatter_lut.view
    }

    /// The atmosphere physical params the baked LUTs were computed from — the AP march reuses them so
    /// the froxel volume agrees with the sky the same LUTs shade.
    pub fn baked_atmosphere(&self) -> AtmosphereParams {
        self.baked_params.atmosphere
    }

    /// The baked sun direction (to sun) + intensity, so the AP march samples the same solar transmittance
    /// the sky-view LUT bakes.
    pub fn baked_sun(&self) -> (Vec3, f32) {
        (self.baked_params.sun_dir, self.baked_params.sun_intensity)
    }

    /// Whether the baked env source is the Hillaire atmosphere, so the sky-view LUT holds a live
    /// in-scatter tint the height-fog pass may sample (`useSkyLut = 1`).
    pub fn atmosphere_live(&self) -> bool {
        self.baked_source == EnvSource::Atmosphere
    }

    /// Re-arms the environment bake when the source / panorama / params change. An exact
    /// `!=` over the POD params flags only real user changes —
    /// no per-frame float drift, no churn. The bake fires at the next idle point.
    pub fn request_env_bake(
        &mut self,
        source: EnvSource,
        panorama: Option<Arc<GpuTexture>>,
        params: SkygenParams,
    ) {
        let pano_changed = source == EnvSource::Equirect
            && match (&self.env_panorama, &panorama) {
                (Some(old), Some(new)) => old.bindless_index() != new.bindless_index(),
                _ => true,
            };
        if should_rebake(
            source,
            self.baked_source,
            &params,
            &self.baked_params,
            pano_changed,
        ) {
            self.rebake_pending = true;
            self.atmosphere_dirty |= source == EnvSource::Atmosphere
                && (!self.atmosphere_base_ready
                    || atmosphere_bake_changed(&params.atmosphere, &self.baked_params.atmosphere));
            self.blend_frames_remaining = 0;
        }
        self.source = source;
        self.env_panorama = panorama;
        self.capture_cadence = params.atmosphere.sky_capture_cadence.clamp(1.0, 60.0);
        self.pending_params = params;
    }

    /// Fires the armed re-bake: bakes the pending params, then commits them as the baked
    /// set on success. Clears `rebake_pending`
    /// regardless (a failed bake is logged, not retried in a tight loop).
    ///
    /// # Errors
    ///
    /// Returns [`Error`] if the bake fails (the caller logs it).
    pub fn fire_rebake(&mut self, device: &Device) -> Result<()> {
        if self.refresh.is_some() {
            return Ok(());
        }
        self.rebake_pending = false;
        self.submit_refresh(device, RefreshKind::Rebuild, false)
    }

    /// Polls the fence-owned refresh, commits a completed back set, and schedules the next
    /// rebuild or EMA step without stalling the device.
    pub fn update_refresh(&mut self, device: &Device) -> Result<bool> {
        let committed = self.poll_refresh()?;
        if self.refresh.is_none() {
            if self.rebake_pending {
                self.fire_rebake(device)?;
            } else if self.blend_frames_remaining > 0 {
                self.submit_refresh(device, RefreshKind::Blend, false)?;
            }
        }
        Ok(committed)
    }

    /// Whether a requested environment refresh and its derived SH/specular capture have finished.
    /// A readback taken before this is true can observe an intermediate blend or a partially
    /// convolved specular cube.
    pub(crate) fn dynamic_lighting_converged(&self) -> bool {
        self.refresh.is_none()
            && !self.rebake_pending
            && self.blend_frames_remaining == 0
            && !self.sh_dirty
            && !self.prefilter_active
            && !self.prefilter_dirty_during_cycle
    }

    /// Runs the startup bake synchronously. Dynamic rebuilds are submitted through
    /// [`Ibl::update_refresh`] and committed only after their fence signals.
    pub fn bake(&mut self, device: &Device, first_bake: bool) -> Result<()> {
        if first_bake {
            self.submit_refresh(device, RefreshKind::Rebuild, true)?;
            let fence = self
                .refresh
                .as_ref()
                .expect("startup refresh was submitted")
                .scratch
                .fence;
            let raw = device.raw();
            checked(
                unsafe { raw.wait_for_fences(&[fence], true, u64::MAX) },
                "ibl startup wait",
            )?;
            let committed = self.poll_refresh()?;
            debug_assert!(committed);
            Ok(())
        } else {
            self.fire_rebake(device)
        }
    }

    pub(super) fn poll_refresh(&mut self) -> Result<bool> {
        let Some(refresh) = self.refresh.as_ref() else {
            return Ok(false);
        };
        let signaled = checked(
            unsafe {
                self.resources
                    .device()
                    .get_fence_status(refresh.scratch.fence)
            },
            "ibl refresh fence status",
        )?;
        if !signaled {
            return Ok(false);
        }

        let refresh = self.refresh.take().expect("checked above");
        if refresh.first_bake {
            self.front.initialized = true;
            self.ready = true;
        } else {
            std::mem::swap(&mut self.front, &mut self.back);
            self.front.initialized = true;
            self.live
                .bind_env(self.resources.device(), self.sampler, self.front.env.view);
            self.sh_dirty = true;
            if self.prefilter_active {
                self.prefilter_dirty_during_cycle = true;
            } else {
                self.prefilter_active = true;
                self.prefilter_slice = 0;
                self.prefilter_frame = 0;
            }
        }

        if refresh.kind == RefreshKind::Rebuild {
            self.refresh_target_initialized = !refresh.first_bake;
            self.atmosphere_base_ready |= refresh.atmosphere_base_written;
            if refresh.source == EnvSource::Atmosphere
                && refresh.params.atmosphere.enabled
                && !refresh.first_bake
            {
                std::mem::swap(&mut self.sky_view_lut, &mut self.refresh_sky_view_lut);
                self.refresh_sky_view_initialized = true;
            }
            self.baked_params = refresh.params;
            self.baked_source = refresh.source;
            self.blend_frames_remaining = if refresh.first_bake { 0 } else { 4 };
        } else {
            self.blend_frames_remaining = self.blend_frames_remaining.saturating_sub(1);
        }

        if refresh.first_bake {
            self.write_mesh_set(self.resources.device());
        }
        tracing::debug!(
            "ibl refresh committed — source {:?}, blend steps remaining {}",
            self.baked_source,
            self.blend_frames_remaining
        );
        drop(refresh);
        Ok(true)
    }

    pub(super) fn submit_refresh(
        &mut self,
        device: &Device,
        kind: RefreshKind,
        first_bake: bool,
    ) -> Result<()> {
        let sky = self.pending_params;
        let raw = device.raw().clone();
        let rebuild = kind == RefreshKind::Rebuild;
        let use_atmosphere =
            rebuild && self.source == EnvSource::Atmosphere && sky.atmosphere.enabled;
        let prepare_atmosphere = use_atmosphere || first_bake;
        let use_equirect =
            rebuild && self.source == EnvSource::Equirect && self.env_panorama.is_some();
        if self.source == EnvSource::Equirect && self.env_panorama.is_none() {
            tracing::warn!("ibl bake: Equirect source has no panorama; falling back to procedural");
        }
        let mut scratch = BakeScratch::new(&raw, device, prepare_atmosphere)?;

        let destination = if first_bake { &self.front } else { &self.back };
        let generated = if first_bake {
            &self.front.env
        } else {
            &self.refresh_target
        };
        let generated_store = generated.storage_view(0)?;
        scratch.transient_views.push(generated_store);
        let env_store = destination.env.storage_view(0)?;
        scratch.transient_views.push(env_store);
        let sky_view = if first_bake {
            self.sky_view_lut.view
        } else {
            self.refresh_sky_view_lut.view
        };
        let views = BakeStorageViews {
            generated_env: generated_store,
            env: env_store,
            sky_view,
        };
        scratch.write_sets(
            &raw,
            self,
            &views,
            prepare_atmosphere,
            use_equirect,
            !first_bake,
        )?;

        let atmos = AtmosPush::new(&sky);
        let atmosphere_base_written = prepare_atmosphere
            && (first_bake || self.atmosphere_dirty || !self.atmosphere_base_ready);
        unsafe {
            let begin = vk::CommandBufferBeginInfo::default()
                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
            checked(raw.begin_command_buffer(scratch.cmd, &begin), "ibl begin")?;

            if prepare_atmosphere {
                if atmosphere_base_written {
                    self.record_atmosphere_base(&raw, &scratch, &atmos, self.atmosphere_base_ready);
                }
                let sky_image = if first_bake {
                    self.sky_view_lut.image
                } else {
                    self.refresh_sky_view_lut.image
                };
                self.record_sky_view(
                    &raw,
                    &scratch,
                    &atmos,
                    sky_image,
                    if first_bake {
                        false
                    } else {
                        self.refresh_sky_view_initialized
                    },
                );
            }

            if rebuild {
                writable_image(
                    &raw,
                    scratch.cmd,
                    generated.image,
                    if first_bake {
                        false
                    } else {
                        self.refresh_target_initialized
                    },
                    1,
                );
                if use_atmosphere {
                    bind_dispatch_push(
                        &raw,
                        scratch.cmd,
                        &scratch.atmos_skygen,
                        scratch.atmos_skygen_set,
                        bytemuck::bytes_of(&atmos),
                        group(IBL_ENV_SIZE),
                        group(IBL_ENV_SIZE),
                        6,
                    );
                } else if use_equirect {
                    let push = EquirectPush {
                        params: Vec4::new(0.0, 1.0, 0.0, 0.0),
                    };
                    bind_dispatch_push(
                        &raw,
                        scratch.cmd,
                        &scratch.equirect,
                        scratch.equirect_set,
                        bytemuck::bytes_of(&push),
                        group(IBL_ENV_SIZE),
                        group(IBL_ENV_SIZE),
                        6,
                    );
                } else {
                    let push = SkygenPush {
                        sun_dir: sky.sun_dir.extend(sky.sun_intensity),
                        sun_color: sky.sun_color.extend(1.0),
                    };
                    bind_dispatch_push(
                        &raw,
                        scratch.cmd,
                        &scratch.skygen,
                        scratch.skygen_set,
                        bytemuck::bytes_of(&push),
                        group(IBL_ENV_SIZE),
                        group(IBL_ENV_SIZE),
                        6,
                    );
                }
            }

            let env_mips = IBL_ENV_SIZE.ilog2() + 1;
            if first_bake {
                generate_cube_mips(
                    &raw,
                    scratch.cmd,
                    destination.env.image,
                    IBL_ENV_SIZE,
                    env_mips,
                    false,
                );
            } else {
                if rebuild {
                    readable_image(&raw, scratch.cmd, generated.image, 1);
                }
                writable_image(
                    &raw,
                    scratch.cmd,
                    destination.env.image,
                    destination.initialized,
                    1,
                );
                let alpha = 0.2_f32;
                bind_dispatch_push(
                    &raw,
                    scratch.cmd,
                    &scratch.cube_blend,
                    scratch.cube_blend_set,
                    bytemuck::bytes_of(&alpha),
                    group(IBL_ENV_SIZE),
                    group(IBL_ENV_SIZE),
                    6,
                );
                generate_cube_mips(
                    &raw,
                    scratch.cmd,
                    destination.env.image,
                    IBL_ENV_SIZE,
                    env_mips,
                    destination.initialized,
                );
            }

            if first_bake {
                self.live.record_startup(&raw, scratch.cmd);
            }

            if first_bake {
                writable_image(&raw, scratch.cmd, self.brdf_lut.image, false, 1);
                bind_dispatch(
                    &raw,
                    scratch.cmd,
                    &scratch.brdf,
                    scratch.brdf_set,
                    group(IBL_LUT_SIZE),
                    group(IBL_LUT_SIZE),
                    1,
                );
                readable_image(&raw, scratch.cmd, self.brdf_lut.image, 1);
            }

            checked(raw.end_command_buffer(scratch.cmd), "ibl end")?;
            let cmd_info = [vk::CommandBufferSubmitInfo::default().command_buffer(scratch.cmd)];
            let submit = [vk::SubmitInfo2::default().command_buffer_infos(&cmd_info)];
            device
                .graphics_queue
                .submit2(&raw, &submit, scratch.fence, "ibl submit")?;
        }

        self.atmosphere_dirty = false;
        self.refresh = Some(PendingRefresh {
            scratch,
            params: sky,
            source: self.source,
            _panorama: self.env_panorama.clone(),
            kind,
            atmosphere_base_written,
            first_bake,
        });
        Ok(())
    }

    pub(super) fn record_atmosphere_base(
        &self,
        raw: &ash::Device,
        scratch: &BakeScratch,
        atmos: &AtmosPush,
        initialized: bool,
    ) {
        let push = bytemuck::bytes_of(atmos);
        let chain = [
            (
                self.transmittance_lut.image,
                &scratch.atmos_transmittance,
                scratch.atmos_transmittance_set,
                ATMOS_TRANSMITTANCE_W,
                ATMOS_TRANSMITTANCE_H,
            ),
            (
                self.multi_scatter_lut.image,
                &scratch.atmos_multiscatter,
                scratch.atmos_multiscatter_set,
                ATMOS_MULTI_SCATTER_SIZE,
                ATMOS_MULTI_SCATTER_SIZE,
            ),
        ];
        for (image, pipeline, set, w, h) in chain {
            record_lut(
                raw,
                scratch.cmd,
                image,
                pipeline,
                set,
                push,
                w,
                h,
                initialized,
            );
        }
    }

    pub(super) fn record_sky_view(
        &self,
        raw: &ash::Device,
        scratch: &BakeScratch,
        atmos: &AtmosPush,
        image: vk::Image,
        initialized: bool,
    ) {
        record_lut(
            raw,
            scratch.cmd,
            image,
            &scratch.atmos_skyview,
            scratch.atmos_skyview_set,
            bytemuck::bytes_of(atmos),
            ATMOS_SKY_VIEW_W,
            ATMOS_SKY_VIEW_H,
            initialized,
        );
    }

    /// Imports persistent sky-lighting resources and schedules the frame's SH/specular work.
    pub(crate) fn add_live_capture_passes(&mut self, graph: &mut RenderGraph) -> IblGraphResources {
        let env = graph.import_image(
            self.front.env.image,
            self.front.env.view,
            vk::ImageAspectFlags::COLOR,
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            None,
        );
        let sh = graph.import_buffer(self.live.sh_coefficients.handle(), None);
        let prefiltered_layout_slot =
            graph.alloc_external_state(crate::RgExternalState::new(self.live.prefiltered_layout));
        let prefiltered = graph.import_image(
            self.live.prefiltered.image,
            self.live.prefiltered.view,
            vk::ImageAspectFlags::COLOR,
            self.live.prefiltered_layout,
            Some(prefiltered_layout_slot),
        );

        if self.ready && (self.atmosphere_live() || self.sh_dirty) {
            let raw = self.resources.device().clone();
            let pipeline = self.live.sh_pipeline.handle;
            let layout = self.live.sh_pipeline.layout;
            let set = self.live.sh_set;
            graph.add_pass(
                RgPass::compute("sky sh projection")
                    .access(env, RgUsage::SampledReadCompute)
                    .access(sh, RgUsage::StorageWriteCompute)
                    .body(move |cmd, _| unsafe {
                        raw.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, pipeline);
                        raw.cmd_bind_descriptor_sets(
                            cmd,
                            vk::PipelineBindPoint::COMPUTE,
                            layout,
                            0,
                            &[set],
                            &[],
                        );
                        raw.cmd_dispatch(cmd, 1, 1, 1);
                    }),
            );
            self.sh_dirty = false;
        }

        let scheduled = self.scheduled_prefilter_slices();
        if !scheduled.is_empty() {
            let raw = self.resources.device().clone();
            let pipeline = self.live.prefilter_pipeline.handle;
            let layout = self.live.prefilter_pipeline.layout;
            let dispatches: Vec<_> = scheduled
                .into_iter()
                .map(|slice| {
                    let (mip, row_offset, row_count) = prefilter_slice(slice);
                    let push = PrefilterPush {
                        roughness: mip as f32 / (IBL_PREFILTER_MIPS - 1) as f32,
                        blend_alpha: 0.2,
                        row_offset,
                        row_count,
                    };
                    (
                        self.live.prefilter_sets[mip as usize],
                        push,
                        (IBL_PREFILTER_SIZE >> mip).max(1),
                    )
                })
                .collect();
            graph.add_pass(
                RgPass::compute("sky specular prefilter")
                    .access(env, RgUsage::SampledReadCompute)
                    .access(prefiltered, RgUsage::StorageImageRwCompute)
                    .body(move |cmd, _| unsafe {
                        raw.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, pipeline);
                        for (set, push, size) in dispatches {
                            raw.cmd_push_constants(
                                cmd,
                                layout,
                                vk::ShaderStageFlags::COMPUTE,
                                0,
                                bytemuck::bytes_of(&push),
                            );
                            raw.cmd_bind_descriptor_sets(
                                cmd,
                                vk::PipelineBindPoint::COMPUTE,
                                layout,
                                0,
                                &[set],
                                &[],
                            );
                            raw.cmd_dispatch(cmd, group(size), group(push.row_count), 6);
                        }
                    }),
            );
        }

        IblGraphResources {
            sh,
            prefiltered,
            prefiltered_layout_slot,
        }
    }

    /// Carries the persistent specular cube's resolved graph layout into the next frame.
    pub(crate) fn resolve_live_layouts(
        &mut self,
        graph: &RenderGraph,
        resources: IblGraphResources,
    ) {
        self.live.prefiltered_layout = graph
            .external_state(resources.prefiltered_layout_slot)
            .layout;
    }

    pub(super) fn scheduled_prefilter_slices(&mut self) -> Vec<u32> {
        if !self.prefilter_active {
            return Vec::new();
        }
        let cadence = self.capture_cadence.round() as u32;
        let mut slices = Vec::new();
        while self.prefilter_slice < PREFILTER_BASE_SLICES
            && self.prefilter_slice * cadence / PREFILTER_BASE_SLICES == self.prefilter_frame
        {
            slices.push(self.prefilter_slice);
            self.prefilter_slice += 1;
        }
        self.prefilter_frame += 1;
        if self.prefilter_frame >= cadence {
            if self.prefilter_dirty_during_cycle {
                self.prefilter_dirty_during_cycle = false;
                self.prefilter_slice = 0;
                self.prefilter_frame = 0;
            } else {
                self.prefilter_active = false;
            }
        }
        slices
    }

    /// Writes every per-frame persistent set 3 (bindings 0-2: sky SH / prefiltered / BRDF
    /// LUT) the mesh fragment samples.
    pub(super) fn write_mesh_set(&self, raw: &ash::Device) {
        let sh_info = [vk::DescriptorBufferInfo::default()
            .buffer(self.live.sh_coefficients.handle())
            .offset(0)
            .range(self.live.sh_coefficients.size())];
        let image_infos = [self.live.prefiltered.view, self.brdf_lut.view].map(|view| {
            vk::DescriptorImageInfo::default()
                .sampler(self.sampler)
                .image_view(view)
                .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
        });
        for &set in &self.sets {
            let writes = [
                vk::WriteDescriptorSet::default()
                    .dst_set(set)
                    .dst_binding(0)
                    .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                    .buffer_info(&sh_info),
                vk::WriteDescriptorSet::default()
                    .dst_set(set)
                    .dst_binding(1)
                    .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                    .image_info(&image_infos[0..1]),
                vk::WriteDescriptorSet::default()
                    .dst_set(set)
                    .dst_binding(2)
                    .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                    .image_info(&image_infos[1..2]),
            ];
            // SAFETY: the ash seam. The sets/views/sampler outlive the renderer; host access
            // is single-threaded during the synchronous startup bake.
            unsafe { raw.update_descriptor_sets(&writes, &[]) };
        }
    }
}

impl Drop for Ibl {
    fn drop(&mut self) {
        // SAFETY: the ash seam. The device idled before teardown (the renderer's Drop); the
        // sampler is freed exactly once. The images free via their own Drop; the set frees
        // with the shared descriptor pool.
        unsafe {
            self.resources.device().destroy_sampler(self.sampler, None);
        }
    }
}

/// `(n + 7) / 8` — the 8×8 compute group count covering `n`.
pub(super) fn group(n: u32) -> u32 {
    n.div_ceil(8)
}

/// Maps the nine-frame base schedule to five mip-0 row bands and one whole pass per coarse mip.
pub(super) fn prefilter_slice(slice: u32) -> (u32, u32, u32) {
    if slice < 5 {
        let begin = slice * IBL_PREFILTER_SIZE / 5;
        let end = (slice + 1) * IBL_PREFILTER_SIZE / 5;
        (0, begin, end - begin)
    } else {
        let mip = slice - 4;
        (mip, 0, (IBL_PREFILTER_SIZE >> mip).max(1))
    }
}

/// The re-bake decision, pure so it is unit-testable
/// without a device. A re-bake is armed on a source change, a panorama change (Equirect),
/// a sun change (Procedural/Atmosphere), or an atmosphere-param change (Atmosphere only).
/// Identical inputs → no re-bake (the exact `!=` over the POD params is the whole point).
pub(super) fn should_rebake(
    source: EnvSource,
    baked_source: EnvSource,
    params: &SkygenParams,
    baked: &SkygenParams,
    pano_changed: bool,
) -> bool {
    let source_changed = source != baked_source;
    let sun_moved = direction_moved(params.sun_dir, baked.sun_dir);
    let moon_moved = direction_moved(params.moon_dir, baked.moon_dir);
    let moon_phase_changed =
        (params.moon_illuminated_fraction - baked.moon_illuminated_fraction).abs() > 1.0 / 512.0;
    let sky_changed = sun_moved
        || params.sun_color != baked.sun_color
        || params.sun_intensity != baked.sun_intensity;
    let atmos_changed = atmosphere_bake_changed(&params.atmosphere, &baked.atmosphere);
    source_changed
        || pano_changed
        || (source == EnvSource::Procedural && sky_changed)
        || (source == EnvSource::Atmosphere
            && (sky_changed
                || moon_moved
                || params.moon_intensity != baked.moon_intensity
                || moon_phase_changed
                || atmos_changed))
}

pub(super) fn atmosphere_bake_changed(params: &AtmosphereParams, baked: &AtmosphereParams) -> bool {
    params.enabled != baked.enabled
        || params.planet_radius != baked.planet_radius
        || params.atmosphere_height != baked.atmosphere_height
        || params.rayleigh_scattering != baked.rayleigh_scattering
        || params.rayleigh_scale_height != baked.rayleigh_scale_height
        || params.mie_scattering != baked.mie_scattering
        || params.mie_scale_height != baked.mie_scale_height
        || params.mie_anisotropy != baked.mie_anisotropy
        || params.ozone_absorption != baked.ozone_absorption
        || params.sun_disk_angular_radius != baked.sun_disk_angular_radius
        || params.sun_disk_intensity != baked.sun_disk_intensity
        || params.moon_disk_angular_radius != baked.moon_disk_angular_radius
        || params.moon_disk_intensity != baked.moon_disk_intensity
        || params.moon_earthshine != baked.moon_earthshine
        || params.per_pixel_transmittance != baked.per_pixel_transmittance
}

pub(super) fn direction_moved(direction: Vec3, baked: Vec3) -> bool {
    direction.normalize_or_zero().dot(baked.normalize_or_zero()) < SUN_REFRESH_ANGLE_RADIANS.cos()
}

#[allow(clippy::too_many_arguments)]
pub(super) fn record_lut(
    raw: &ash::Device,
    cmd: vk::CommandBuffer,
    image: vk::Image,
    pipeline: &ComputePso,
    set: vk::DescriptorSet,
    push: &[u8],
    width: u32,
    height: u32,
    initialized: bool,
) {
    cube_barrier(
        raw,
        cmd,
        image,
        if initialized {
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL
        } else {
            vk::ImageLayout::UNDEFINED
        },
        vk::ImageLayout::GENERAL,
        if initialized {
            vk::PipelineStageFlags2::ALL_COMMANDS
        } else {
            vk::PipelineStageFlags2::TOP_OF_PIPE
        },
        if initialized {
            vk::AccessFlags2::SHADER_SAMPLED_READ
        } else {
            vk::AccessFlags2::empty()
        },
        vk::PipelineStageFlags2::COMPUTE_SHADER,
        vk::AccessFlags2::SHADER_STORAGE_WRITE,
        0,
        1,
    );
    bind_dispatch_push(
        raw,
        cmd,
        pipeline,
        set,
        push,
        group(width),
        group(height),
        1,
    );
    cube_barrier(
        raw,
        cmd,
        image,
        vk::ImageLayout::GENERAL,
        vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
        vk::PipelineStageFlags2::COMPUTE_SHADER,
        vk::AccessFlags2::SHADER_STORAGE_WRITE,
        vk::PipelineStageFlags2::COMPUTE_SHADER,
        vk::AccessFlags2::SHADER_SAMPLED_READ,
        0,
        1,
    );
}
