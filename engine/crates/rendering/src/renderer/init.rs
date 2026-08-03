use super::*;

impl Renderer {
    /// Brings up the renderer against `surface_source` at `(width, height)`.
    ///
    /// Creates the [`Device`] (instance/surface/device/allocator + feature probe), the
    /// per-frame command/sync ring, and — for [`SurfaceSource::Window`] only — the present
    /// [`Swapchain`]. The offscreen editor host builds none: it has no surface, renders
    /// offscreen, and publishes BGRA8 frames to shared memory.
    ///
    /// # Errors
    ///
    /// Propagates any [`Error`] from device, swapchain, or frame-ring creation.
    pub fn new(surface_source: &SurfaceSource<'_>, width: u32, height: u32) -> Result<Self> {
        let device = Arc::new(Device::new(surface_source)?);
        crate::watchdog::attach_device(&device);
        let mut swapchain = match surface_source {
            SurfaceSource::Window(_) => Some(Swapchain::new(&device, width, height)?),
            SurfaceSource::Offscreen => None,
        };
        if let Some(swapchain) = swapchain.as_ref() {
            tracing::info!("{} swapchain images", swapchain.image_count());
        }
        let frames = match FrameRing::new(&device) {
            Ok(frames) => frames,
            Err(err) => {
                if let Some(swapchain) = swapchain.as_mut() {
                    swapchain.destroy(&device);
                }
                return Err(err);
            }
        };

        let mut present_sync = match swapchain {
            Some(_) => match PresentSync::new(&device) {
                Ok(present_sync) => Some(present_sync),
                Err(err) => {
                    let mut frames = frames;
                    frames.destroy(&device);
                    if let Some(swapchain) = swapchain.as_mut() {
                        swapchain.destroy(&device);
                    }
                    return Err(err);
                }
            },
            None => None,
        };

        type BuildParts = (
            Arc<Descriptors>,
            Lighting,
            Pipelines,
            Instancing,
            Skinning,
            Tessellation,
            RenderGraphResources,
            crate::GlobalGpuData,
            crate::GpuSceneUploader,
            crate::PageResidency,
            crate::Hzb,
            crate::SceneVisibility,
            crate::PersistentGpuScene,
            Ibl,
            Ibl,
            Sky,
            crate::StarCatalog,
            ReflectionProbes,
            Ssao,
            crate::Ddgi,
            crate::GlobalSdf,
            crate::Rt,
            crate::Restir,
            crate::FroxelFog,
            crate::AerialPerspective,
            crate::Clouds,
            Vec<ViewTarget>,
            BindlessFreeList,
            crate::Aa,
            Arc<crate::GpuTexture>,
            Arc<crate::GpuSdf>,
            Arc<crate::GpuSdf>,
            crate::resources::DefaultHeightMinMax,
            Arc<crate::GpuLut>,
            crate::vsm::VsmGpu,
            crate::VsmDemand,
        );
        let build = || -> Result<BuildParts> {
            let free_list: BindlessFreeList = Arc::new(Mutex::new(Vec::new()));
            let descriptors = Arc::new(Descriptors::new(&device, &free_list)?);

            // The default white texture takes slot 0 and is seeded into every other bindless
            // slot, so an untextured material always samples a valid descriptor.
            let queue = device.graphics_queue.clone();
            let uploader = crate::Uploader::new(&device, &queue)?;
            let default_white = uploader.upload_default_white(&descriptors)?;
            // The bindless SDF array (binding 1) is partially bound; seed every slot with a 1×1×1
            // empty-space field, because sampling an unbound slot faults on lavapipe and is UB
            // on real hardware. A per-mesh SDF overwrites its own slot as its `GpuMesh` is built.
            let default_sdf = uploader.upload_default_sdf(&descriptors)?;
            // The unit-box brick the micro-field slab occluders are backed by; claimed once, so
            // every slab an occluder pass emits names the same bindless slot.
            let slab_sdf = uploader.upload_unit_box_sdf(&descriptors)?;
            // The per-height min/max pyramid array (binding 4) is partially bound; seed every slot
            // with a 1×1 `(0, 0)` default for the same reason.
            let default_height_minmax = uploader.upload_default_height_minmax(&descriptors)?;
            // The neutral identity creative LUT, always bound at binding 2 of every view's
            // tonemap set, so the shader never samples an unbound descriptor.
            let default_lut = uploader.upload_identity_lut()?;

            let vsm_gpu = crate::vsm::VsmGpu::new(&device)?;
            let vsm_demand = crate::VsmDemand::new(&device, &descriptors)?;
            let lighting = Lighting::new(&device, &descriptors, vsm_gpu.atlas.view())?;
            let pipelines = Pipelines::new(&device, &descriptors, vk::SampleCountFlags::TYPE_1);
            let instancing = Instancing::new(&device, &descriptors)?;
            let skinning = Skinning::new(&device)?;
            let tessellation = Tessellation::new(&device)?;
            let transient = RenderGraphResources::new(device.resources().clone());
            let global_gpu_data = crate::GlobalGpuData::new(&device)?;
            let gpu_scene_uploader = crate::GpuSceneUploader::new(&device)?;
            let page_residency = crate::PageResidency::new(crate::PageResidencyBudgets::default());
            let hzb = crate::Hzb::new(&device)?;
            let scene_visibility = crate::SceneVisibility::new(&device)?;
            for frame in 0..crate::MAX_FRAMES_IN_FLIGHT {
                descriptors.write_uniform_buffer_at(
                    instancing.instance_set(frame),
                    3,
                    gpu_scene_uploader.address_buffer(),
                    frame as u64 * gpu_scene_uploader.address_block_stride(),
                    size_of::<crate::GpuSceneAddressBlock>() as u64,
                );
            }
            let mut persistent_gpu_scene =
                crate::PersistentGpuScene::new(crate::GpuSceneUploadLimits::default())?;
            for view in [ViewId::Scene, ViewId::AssetPreview, ViewId::Thumbnail] {
                persistent_gpu_scene.create_world(view.gpu_scene_world())?;
                persistent_gpu_scene.create_view(view.gpu_scene_view(), view.gpu_scene_world())?;
            }

            // The first (procedural) IBL bake, so set 3 is valid before the first frame. The sky
            // reuses the env cube; the reflection probes ride the IBL set, seeded after the bake.
            let mut ibl = Ibl::new(&device, &descriptors)?;
            ibl.bake(&device, true)?;
            let stars = crate::StarCatalog::new(
                &device,
                &descriptors,
                &uploader,
                ibl.transmittance_view(),
                ibl.sky_view_lut_view(),
                ibl.sampler(),
                vk::SampleCountFlags::TYPE_1,
            )?;
            let mut sky = Sky::new(&device, &descriptors, vk::SampleCountFlags::TYPE_1)?;
            sky.bind_env_cube(&ibl);
            sky.bind_night_sky(&ibl, &stars);
            let reflection = ReflectionProbes::new(&device)?;
            reflection.seed(&ibl);

            // The offscreen thumbnail preview IBL: validated with the procedural bake now, and
            // re-baked to the fixed preview environment on the first thumbnail render.
            let mut preview_ibl = Ibl::new(&device, &descriptors)?;
            preview_ibl.bake(&device, true)?;
            reflection.seed(&preview_ibl);

            // Screen-space effects: the device-shared sampler + compute layouts. `ready` flips
            // once the views are built.
            let mut ssao = Ssao::new(&device)?;
            // The AA capability + initial mode (off). The per-view AA targets follow it.
            let aa = crate::Aa::new(
                device.supported_sample_counts(crate::OFFSCREEN_COLOR_FORMAT, crate::DEPTH_FORMAT),
            );
            // DDGI: device-shared (one camera-centered probe clipmap), on by default. Built after
            // descriptors — it needs the mesh set-5 layout and the shared pool.
            let ddgi = crate::Ddgi::new(&device, &descriptors)?;
            ddgi.bind_sky_sh(ibl.sh_coefficients());
            // Global SDF: device-shared (one clipmap), off by default. Built after descriptors —
            // it needs the shared pool and the light layout it writes the cascade samplers into.
            let global_sdf = crate::GlobalSdf::new(&device, &descriptors)?;
            // RT: a no-op sub-state on a software device. Built after descriptors (it needs set 6).
            let rt = crate::Rt::new(&device, &descriptors)?;
            // ReSTIR DI: device-shared scaffolding, off by default and inert on a software device
            // (the resolve needs ray-query). The per-view reservoirs are built per view below.
            let restir = crate::Restir::new(&device, &descriptors)?;

            // The froxel volumetric-fog volumes. Created eagerly so the composite's integration
            // sampler (fog set binding 4) is always bound — the shader references it statically.
            let froxel = crate::FroxelFog::new(&device)?;

            // The aerial-perspective volume. Created eagerly so the composite's AP sampler (fog
            // set binding 5) is always bound; its LUT bindings persist across atmosphere bakes.
            let aerial = crate::AerialPerspective::new(&device)?;
            aerial.bind_luts(
                &device,
                ibl.sampler(),
                ibl.transmittance_view(),
                ibl.multi_scatter_view(),
            );

            // Static cloud noise is authored once by GPU compute; the weather map is one
            // persistent image refilled only when its authoring inputs change.
            let clouds = crate::Clouds::new(&device, &pipelines, &ibl, VIEW_COUNT)?;
            lighting.bind_cloud_shadow(
                &device,
                clouds.cloud_shadow().view(),
                clouds.shadow_sampler(),
            );

            // One view per editor pane, each with its own offscreen + screen-space + AA + ReSTIR
            // targets, so a view switch never aliases another view's images.
            let mut views = Vec::with_capacity(VIEW_COUNT);
            for _ in 0..VIEW_COUNT {
                let mut view = ViewTarget::new(&device, width, height)?;
                view.allocate_screen_space_sets(&descriptors, &ssao)?;
                view.build_screen_space(&device, &descriptors, &ssao)?;
                // The tonemap set is allocated once per view and never reallocated (a resize
                // rewrites bindings 0/1 only), so this write persists until a look replaces it.
                view.write_tonemap_lut(&device, descriptors.linear_sampler(), default_lut.view());
                // The sky-view LUT image is allocated once and reused across bakes, so this write
                // persists across resizes (the fog pass gates its use by `useSkyLut`).
                view.write_fog_sky_lut(&device, ibl.sampler(), ibl.sky_view_lut_view());
                // The froxel integration volume, rewritten by `set_fog` when a quality switch
                // reallocates it.
                view.write_fog_integration(&device, froxel.sampler(), froxel.integration_view());
                // The aerial-perspective volume is fixed-size and never reallocated, so this
                // write persists.
                view.write_fog_aerial(&device, aerial.sampler(), aerial.volume_view());
                view.write_fog_atmosphere_luts(
                    &device,
                    ibl.sampler(),
                    ibl.transmittance_view(),
                    ibl.multi_scatter_view(),
                );
                // The AA targets, built after the screen-space chain (it reads the SSGI maps).
                view.build_aa_targets(&device, &descriptors, aa)?;
                view.restir.allocate_sets(&descriptors, &restir)?;
                view.restir
                    .build(&device, &descriptors, &restir, view.scaled_render_extent())?;
                views.push(view);
            }
            for (index, view) in views.iter().enumerate() {
                clouds.bind_view(
                    index,
                    crate::clouds::CloudViewBindings {
                        color: view.offscreen.view(),
                        depth: view.depth.view(),
                        motion: view.motion.as_ref().expect("cloud motion built").view(),
                        reduced: [
                            view.cloud_reduced[0]
                                .as_ref()
                                .expect("cloud reduced 0 built")
                                .view(),
                            view.cloud_reduced[1]
                                .as_ref()
                                .expect("cloud reduced 1 built")
                                .view(),
                        ],
                        reduced_depth: view
                            .cloud_reduced_depth
                            .as_ref()
                            .expect("cloud reduced depth built")
                            .view(),
                        full_color: view
                            .cloud_full_color
                            .as_ref()
                            .expect("cloud full color built")
                            .view(),
                        full_depth: view
                            .cloud_full_depth
                            .as_ref()
                            .expect("cloud full depth built")
                            .view(),
                    },
                );
            }
            ssao.ready = true;
            Ok((
                descriptors,
                lighting,
                pipelines,
                instancing,
                skinning,
                tessellation,
                transient,
                global_gpu_data,
                gpu_scene_uploader,
                page_residency,
                hzb,
                scene_visibility,
                persistent_gpu_scene,
                ibl,
                preview_ibl,
                sky,
                stars,
                reflection,
                ssao,
                ddgi,
                global_sdf,
                rt,
                restir,
                froxel,
                aerial,
                clouds,
                views,
                free_list,
                aa,
                default_white,
                default_sdf,
                slab_sdf,
                default_height_minmax,
                default_lut,
                vsm_gpu,
                vsm_demand,
            ))
        };
        let (
            descriptors,
            lighting,
            pipelines,
            instancing,
            skinning,
            tessellation,
            transient,
            global_gpu_data,
            gpu_scene_uploader,
            page_residency,
            hzb,
            scene_visibility,
            persistent_gpu_scene,
            ibl,
            preview_ibl,
            sky,
            stars,
            reflection,
            ssao,
            ddgi,
            global_sdf,
            rt,
            restir,
            froxel,
            aerial,
            clouds,
            views,
            bindless_free_list,
            aa,
            default_white,
            default_sdf,
            slab_sdf,
            default_height_minmax,
            default_lut,
            vsm_gpu,
            vsm_demand,
        ) = match build() {
            Ok(parts) => parts,
            Err(err) => {
                let _ = device.wait_idle();
                let mut frames = frames;
                frames.destroy(&device);
                if let Some(present_sync) = present_sync.as_mut() {
                    present_sync.destroy(&device);
                }
                if let Some(swapchain) = swapchain.as_mut() {
                    swapchain.destroy(&device);
                }
                return Err(err);
            }
        };

        let overlay = OverlayState::new(device.resources());

        let facts = device.profiler_facts();
        let gpu_profiler = GpuProfiler::with_facts(
            facts.timestamp_period,
            facts.timestamp_mask,
            facts.timestamps_supported,
            facts.pipeline_stats_supported,
            facts.calibration_available,
            facts.host_domain,
        );
        let software_gpu = device.capabilities.software_gpu;
        let device_name = facts.device_name;

        // The SDF-occluder SSBO the scatter writes on device: one region per frame in flight,
        // each holding up to [`MAX_SDF_INSTANCES`] occluders.
        let sdf_slot_bytes = u64::from(MAX_SDF_INSTANCES) * size_of::<crate::SdfInstance>() as u64;
        let sdf_alloc = vk_mem::AllocationCreateInfo {
            usage: vk_mem::MemoryUsage::AutoPreferDevice,
            ..Default::default()
        };
        let sdf_instances = match crate::Buffer::new(
            device.resources(),
            sdf_slot_bytes * crate::MAX_FRAMES_IN_FLIGHT as u64,
            vk::BufferUsageFlags::STORAGE_BUFFER,
            &sdf_alloc,
        ) {
            Ok(buffer) => buffer,
            Err(err) => {
                let _ = device.wait_idle();
                return Err(err);
            }
        };
        // The scatter's meta words, one 16-byte slice per frame slot: [0] occluders written, [1]
        // culled against the reach window, [2] dropped to capacity. The consumers read the count
        // from here because it is GPU-produced; the readback twin feeds render-stats on slot reuse.
        let sdf_meta = match crate::Buffer::new(
            device.resources(),
            SDF_META_SLOT_BYTES * crate::MAX_FRAMES_IN_FLIGHT as u64,
            vk::BufferUsageFlags::STORAGE_BUFFER
                | vk::BufferUsageFlags::TRANSFER_DST
                | vk::BufferUsageFlags::TRANSFER_SRC,
            &sdf_alloc,
        ) {
            Ok(buffer) => buffer,
            Err(err) => {
                let _ = device.wait_idle();
                return Err(err);
            }
        };
        let sdf_meta_readback = match crate::Buffer::new(
            device.resources(),
            SDF_META_SLOT_BYTES * crate::MAX_FRAMES_IN_FLIGHT as u64,
            vk::BufferUsageFlags::TRANSFER_DST,
            &vk_mem::AllocationCreateInfo {
                usage: vk_mem::MemoryUsage::Auto,
                flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_RANDOM
                    | vk_mem::AllocationCreateFlags::MAPPED,
                ..Default::default()
            },
        ) {
            Ok(buffer) => buffer,
            Err(err) => {
                let _ = device.wait_idle();
                return Err(err);
            }
        };
        // SAFETY: HOST_VISIBLE + MAPPED, zeroed before any read.
        unsafe {
            std::ptr::write_bytes(
                sdf_meta_readback.mapped_ptr(),
                0,
                sdf_meta_readback.size() as usize,
            );
        }
        // Wire the occluder SSBO slices into binding 8 (+ the meta at 15) of every frame slot's
        // light set, and into the GDF cull + composite + scatter sets. The buffers never realloc.
        lighting.bind_sdf_instances(
            &descriptors,
            sdf_instances.handle(),
            sdf_slot_bytes,
            sdf_meta.handle(),
            SDF_META_SLOT_BYTES,
        );
        global_sdf.bind_scene(
            sdf_instances.handle(),
            sdf_slot_bytes,
            sdf_meta.handle(),
            SDF_META_SLOT_BYTES,
        );
        lighting.bind_gdf(&global_sdf);
        // The froxel integration volume at binding 11 of every light set, so the forward
        // transparent path samples the same volumetric fog the composite applies to opaque.
        lighting.bind_froxel_integration(&device, froxel.integration_view(), froxel.sampler());
        // The GDF lite albedo cache at DDGI trace set binding 0: the trace reads it as the hit
        // radiance's per-cell base color.
        ddgi.bind_gdf_albedo(&global_sdf);

        let mut renderer = Self {
            selection_sources: [const { None }; crate::VIEW_COUNT],
            selection_targets: None,
            mesh_executor: crate::mesh_executor_supported(&device.capabilities),
            sdf_instances_dropped: 0,
            sdf_instances_culled: 0,
            rt_instances_culled: 0,
            wind_discontinuity: false,
            wind_source_count: 0,
            wind_source_digest: Vec::new(),
            wind_authored_digest: 0,
            node_cull: std::env::var("SAFFRON_NODE_CULL").as_deref() != Ok("off"),
            micro_field_enabled: std::env::var("SAFFRON_MICRO_FIELD").as_deref() != Ok("off"),
            cut_override: [match std::env::var("SAFFRON_CUT_OVERRIDE").as_deref() {
                Ok("coarse") => crate::SCENE_CUT_FORCE_COARSE,
                Ok("fine") => crate::SCENE_CUT_FORCE_FINE,
                _ => crate::SCENE_CUT_AUTO,
            }; crate::SCENE_VIEW_CLASSES],
            clear_color: [0.05, 0.06, 0.08, 1.0],
            wireframe: false,
            use_depth_prepass: true,
            exposure_ev: 0.0,
            night_factor: 0.0,
            bloom_enabled: false,
            bloom_intensity: 0.05,
            bloom_scatter: 0.005,
            bloom_tint: [1.0, 1.0, 1.0],
            bloom_threshold: 0.0,
            bloom_dirt_texture: None,
            bloom_dirt_texture_id: 0,
            bloom_dirt_intensity: 0.0,
            bloom_dirt_tint: [1.0, 1.0, 1.0],
            bloom_anamorphic_enabled: false,
            bloom_anamorphic_ratio: 2.0,
            bloom_anamorphic_tint: [0.6, 0.8, 1.0],
            bloom_anamorphic_intensity: 0.0,
            bloom_mip_tint: Vec::new(),
            fog: FogRenderSettings::default(),
            froxel,
            aerial,
            clouds,
            fog_volumes: Vec::new(),
            fog_time: 0.0,
            sun_direction: Vec3::new(0.0, -1.0, 0.0),
            sun_color: Vec3::ONE,
            sun_intensity: 0.0,
            moon_direction: Vec3::Y,
            moon_color: Vec3::ZERO,
            moon_intensity: 0.0,
            show_grid: false,
            present_viewport_only: false,
            slot_fence_armed: false,
            pending_compute_signal: None,
            view_mode: ViewMode::Lit,
            skinning_enabled: true,
            displacement_enabled: true,
            tess_factor_cap: crate::tessellation::TESS_DEFAULT_FACTOR_CAP,
            tess_min_factor: crate::tessellation::TESS_DEFAULT_MIN_FACTOR,
            tess_edge_length_target: crate::tessellation::TESS_DEFAULT_EDGE_LENGTH_TARGET,
            software_gpu,
            frame_ms: 0.0,
            cpu_frame_ms: 0.0,
            scene_gather_ms: 0.0,
            scene_gather_entities: 0,
            frame_serial: 0,
            gpu_frame_ms: 0.0,
            cpu_wait_ms: 0.0,
            vram_usage_bytes: 0,
            vram_budget_bytes: 0,
            perf_config: PerfConfig::default(),
            frame_history: FrameHistory::default(),
            telemetry_warmup: 0,
            alarms: AlarmState::default(),
            gpu_profiler,
            cpu_profiler: CpuProfiler::default(),
            last_frame_ns: 0,
            capture: CaptureRecorder::default(),
            device_name,
            overlay,
            submissions: Vec::new(),
            frame_deformation: FrameDeformation::default(),
            rt_deform_jobs: Vec::new(),
            micro_rt_tiles: Vec::new(),
            render_quality: RenderQuality::default(),
            budget_controller: BudgetController::new(),
            pending_render_scale: None,
            pending_view_size: [None; crate::VIEW_COUNT],
            tonemap_mode: TonemapMode::default(),
            color_grade: ColorGrade::default(),
            default_lut,
            creative_lut: None,
            creative_lut_id: 0,
            creative_lut_intensity: 0.0,
            creative_lut_size: 2,
            reactive: ReactiveState::default(),
            stats: RenderStats::default(),
            aa,
            taa_params: crate::TaaParams::default(),
            camera_near_far: (0.1, 100.0),
            cluster_camera: ClusterCamera {
                view: Mat4::IDENTITY,
                projection: Mat4::IDENTITY,
                width,
                height,
                near: 0.1,
                far: 100.0,
            },
            views,
            global_gpu_data,
            gpu_scene_uploader,
            pending_cpu_spans: Vec::new(),
            vsm_page_budget: crate::VSM_DEFAULT_PAGE_BUDGET,
            wind_deform_records: std::collections::HashMap::new(),
            scene_wind: SceneWind::default(),
            vsm_gpu,
            owned_budgets: Vec::new(),
            wind_interaction_resets: 0,
            interaction_centers: [[0; 2]; 2],
            interaction_centers_previous: [[0; 2]; 2],
            vsm_residency: crate::VsmResidency::default(),
            vsm_views: (0..crate::VSM_DIRECTIONAL_LEVELS + 1 + crate::vsm::VSM_POINT_FACES)
                .map(|_| None)
                .collect(),
            gi_view: None,
            gi_visibility_counters: [0; crate::SCENE_VISIBILITY_COUNTER_WORDS as usize],
            vsm_render_pages: Vec::new(),
            vsm_space: crate::VsmDirectionalSpace::build(
                saffron_geometry::glam::Vec3::NEG_Y,
                saffron_geometry::glam::Vec3::ZERO,
            ),
            vsm_demand,
            vsm_demanded: Vec::new(),
            vsm_spot_matrix: [0.0; 16],
            vsm_point_key: [0; 4],
            interaction_fields: std::collections::HashMap::new(),
            interaction_impulses: Vec::new(),
            interaction_impulse_ring: Vec::new(),
            wind_source_ring: Vec::new(),
            page_residency,
            hzb,
            scene_visibility,
            live_executor_bins: Vec::new(),
            live_draw_record_bound: 0,
            visibility_counters: [0; crate::SCENE_VISIBILITY_COUNTER_WORDS as usize],
            page_faults: 0,
            pending_gpu_scene_uploads: crate::GpuScenePendingUploads::default(),
            micro_field_directory: None,
            last_gpu_scene_upload: crate::GpuSceneUploadRunStats::default(),
            persistent_gpu_scene,
            active_view: ViewId::Scene,
            shm_publish_enabled: [false; VIEW_COUNT],
            pending_shm_publish: None,
            lighting,
            instancing,
            skinning,
            tessellation,
            displaced_frame: None,
            displaced_addresses: crate::DisplacedFrameAddresses::default(),
            transient,
            pipelines,
            ibl,
            preview_ibl,
            sky,
            stars,
            reflection,
            ssao,
            ddgi,
            global_sdf,
            rt,
            restir,
            sdf_instances,
            sdf_meta,
            sdf_meta_readback,
            sky_occlusion: true,
            descriptors,
            bindless_free_list,
            default_white,
            default_sdf,
            slab_sdf,
            default_height_minmax,
            capture_next_window_path: None,
            frames,
            swapchain,
            present_sync,
            device,
        };
        renderer
            .pending_gpu_scene_uploads
            .upload_arena(crate::GpuArenaUploadRequest::PageBytes {
                range: renderer.global_gpu_data.micro_blade_template,
                data: crate::micro_blade_template_indices()
                    .iter()
                    .flat_map(|index| index.to_le_bytes())
                    .collect(),
            });
        Ok(renderer)
    }
}
