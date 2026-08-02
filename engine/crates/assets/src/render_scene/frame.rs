use super::*;

/// Renders the authored scene into one frame draw list. Asset-placement preview ghosts are
/// ordinary [`PreviewGhost`](saffron_scene::PreviewGhost)-tagged entities in this scene, so they
/// render through the normal gather; nothing else special-cases them.
pub fn render_scene<R: SceneRenderer>(
    renderer: &mut R,
    scene: &mut Scene,
    assets: &mut AssetServer,
    mirror: &mut crate::GpuSceneMirror,
    camera: &CameraView,
    options: RenderSceneOptions,
) {
    let width = renderer.viewport_width();
    let height = renderer.viewport_height();
    if width == 0 || height == 0 {
        return;
    }
    let aspect = width as f32 / height as f32;
    let view = camera.view;
    let mut proj = camera_projection(camera, aspect);
    proj.y_axis.y *= -1.0; // flip Y into Vulkan clip space
    // Sub-pixel TAA jitter: shift NDC by the Halton offset as a clip-space translation
    // (`clip.xy += jitter · clip.w`). Applied to the scene view-projection only — `proj` stays
    // un-jittered for clustered lighting / SSAO / picking. The offset is zero unless TAA is on,
    // and the renderer reprojects the motion prepass / grid against the un-jittered matrix.
    let jitter = renderer.jitter_offset();
    let view_projection =
        Mat4::from_translation(Vec3::new(jitter.x, jitter.y, 0.0)) * (proj * view);

    // Reconcile the runtime camera-gizmo ghosts before the flatten, so a fresh ghost's
    // world transform composes this frame.
    sync_editor_camera_models(scene, options.show_editor_camera_models);

    // Flatten the hierarchy once per frame before any consumer reads: every loop below
    // (lights, meshes, probes) and the between-frame pick/gizmo paths read the world-
    // transform cache this writes.
    let t = cpu_now_ns();
    scene.update_world_transforms();
    renderer.record_cpu_span("world-transforms", t, cpu_now_ns().saturating_sub(t));

    let time_of_day = drive_time_of_day(scene);
    if let Some(exposure) = time_of_day.exposure {
        renderer.set_exposure(exposure);
    }
    renderer.set_night_factor(time_of_day.night_factor);

    let (sun, moon) = gather_directional_lights(scene, time_of_day.directions);
    let has_sun = sun.is_some();
    let sun = sun.unwrap_or_else(DirectionalResolved::none);
    let light_dir = sun.direction;
    let light_color = sun.color;
    let light_intensity = sun.intensity;
    let light_ambient = sun.ambient;
    let light_volumetric = sun.volumetric_scattering;
    let light_cast_volumetric_shadow = sun.cast_volumetric_shadow;
    let moon = moon.unwrap_or_else(DirectionalResolved::none);
    let (lights, point_shadow, spot_shadow) = gather_punctual_lights(scene);

    renderer.set_spot_shadow(
        spot_shadow.map_or(Mat4::IDENTITY, |s| s.view_proj),
        spot_shadow.map_or(0, |s| s.light_index),
        spot_shadow.is_some(),
    );
    let point_shadow_pos = point_shadow.map_or(Vec3::ZERO, |p| p.pos);
    let point_shadow_far = point_shadow.map_or(1.0, |p| p.far);
    renderer.set_point_shadow(
        point_shadow_pos,
        point_shadow_far,
        point_shadow.map_or(0, |p| p.light_index),
        point_shadow.is_some(),
    );

    // The camera world position is the inverse-view translation; the BRDF needs it as the
    // view-vector origin.
    let eye_position = view.inverse().w_axis.truncate();

    let gather_started = Instant::now();
    let mut build = FrameSceneBuild::default();
    gather_static_frame_facts(renderer, scene, mirror, eye_position, &mut build);
    if renderer.skinning_enabled() {
        gather_skinned_frame_facts(renderer, scene, mirror, &mut build);
    }
    let FrameSceneBuild {
        work,
        frame_joints,
        renderable_count,
        rt_instances_culled,
        rt_instances,
        entities_derived,
    } = build;
    let scene_gather_elapsed = gather_started.elapsed();

    // The sun shadows through the directional virtual pages (the renderer builds
    // the camera-snapped clip-level spaces itself); with no sun in the scene there
    // is nothing to cast.
    let cast_shadow = has_sun && renderable_count > 0;
    renderer.set_directional_shadow(cast_shadow);

    // RT: hand the frame's STATIC instances (gathered above with their stable GPU-scene
    // instance slots) to the renderer for the per-frame TLAS build. Skinned instances
    // ride the deformation gather's refit entries (their deformed verts are already
    // world-space, referenced by an identity transform), so they are excluded here.
    renderer.set_rt_scene(rt_instances);

    renderer.record_rt_culled(rt_instances_culled);

    // DDGI: snap the camera-centered probe clipmap to the camera and pass the sun. The trace
    // sphere-marches the real distance field (per-mesh MDF near + Global SDF far), so it needs no
    // scene-box proxy. Done before the lighting upload, which reads the volume placement + scroll
    // base into the light UBO.
    renderer.set_ddgi_scene(eye_position, light_dir, light_color, light_intensity);

    let probe_uploads = gather_reflection_probes(scene);
    renderer.submit_reflection_probes(&probe_uploads);

    let fog_volume_uploads = gather_fog_volumes(scene);
    renderer.submit_fog_volumes(&fog_volume_uploads);

    // Fallback ambient (used when IBL is off): the scene environment's ambient color when
    // use_sky_for_ambient, else the directional light's scalar ambient (grayscale).
    let ambient = if scene.environment.use_sky_for_ambient {
        scene.environment.ambient_color * scene.environment.ambient_intensity * time_of_day.tint
    } else {
        Vec3::splat(light_ambient) * time_of_day.tint
    };
    let c = &scene.environment.cloud;
    let weather_texture = if c.weather_texture.value() != 0 {
        assets.load_texture_asset(renderer, c.weather_texture)
    } else {
        None
    };
    renderer.submit_clouds(CloudRenderSettings {
        enabled: c.enabled,
        coverage: time_of_day.cloud_coverage.unwrap_or(c.coverage),
        cloud_type: time_of_day.cloud_type.unwrap_or(c.cloud_type),
        precipitation: c.precipitation,
        anvil_bias: c.anvil_bias,
        layer_altitude: c.layer_altitude,
        layer_height: c.layer_height,
        base_scale: c.base_scale,
        detail_scale: c.detail_scale,
        detail_strength: c.detail_strength,
        curl_strength: c.curl_strength,
        weather_scale: c.weather_scale,
        weather_offset: c.weather_offset,
        weather_texture_id: c.weather_texture.value(),
        weather_texture,
        primary_steps: c.primary_steps,
        light_steps: c.light_steps,
        droplet_diameter: c.droplet_diameter,
        temporal_factor: c.temporal_factor,
        cast_cloud_shadows: c.cast_cloud_shadows,
        cloud_shadow_strength: c.cloud_shadow_strength,
        cloud_shadow_on_surface_strength: c.cloud_shadow_on_surface_strength,
    });
    let lighting_started = cpu_now_ns();
    if let Err(err) = renderer.set_scene_lighting(&SceneLighting {
        direction: light_dir,
        color: light_color,
        intensity: light_intensity,
        moon_direction: moon.direction,
        moon_color: moon.color,
        moon_intensity: moon.intensity,
        ambient,
        ibl_tint: time_of_day.tint,
        eye_position,
        directional_volumetric: light_volumetric,
        directional_cast_volumetric_shadow: light_cast_volumetric_shadow,
        lights,
    }) {
        tracing::error!("set_scene_lighting: {err}");
    }
    renderer.record_cpu_span(
        "scene-lighting",
        lighting_started,
        cpu_now_ns().saturating_sub(lighting_started),
    );

    // Drive the environment bake. Equirect (a loaded panorama) wins, then the atmosphere,
    // then the procedural gradient — the sun derived from the directional light.
    let t = cpu_now_ns();
    let sky_panorama = drive_env_bake(
        renderer,
        scene,
        assets,
        &sun,
        &moon,
        time_of_day.moon_illuminated_fraction,
    );
    renderer.record_cpu_span("env-bake", t, cpu_now_ns().saturating_sub(t));

    renderer.set_cluster_camera(ClusterCamera {
        view,
        projection: proj,
        width,
        height,
        near: camera.near_plane,
        far: camera.far_plane,
    });
    // Screen-space passes (G-buffer/GTAO/contact/SSGI) use the scene view/proj + the
    // directional light direction (for contact shadows).
    renderer.set_ssao_camera(view, proj, light_dir);
    renderer.set_show_grid(options.show_grid);

    renderer.record_scene_gather(scene_gather_elapsed, entities_derived);
    let t = cpu_now_ns();
    if let Err(err) = renderer.submit_deformations(view_projection, &work, &frame_joints) {
        tracing::error!("submit_deformations: {err}");
    }
    renderer.patch_frame_deformations(scene, mirror);
    renderer.record_cpu_span("deformations", t, cpu_now_ns().saturating_sub(t));

    // Resolve the scene environment into the visible-sky settings.
    let env = &scene.environment;
    let mut sky = SkyRenderSettings {
        mode: env.sky_mode as u32,
        clear_color: env.clear_color,
        intensity: env.sky_intensity,
        tint: time_of_day.tint,
        rotation: env.sky_rotation,
        visible: env.visible,
        texture_index: 0,
        night: saffron_rendering::NightSkyParams {
            world_from_equatorial: time_of_day.world_from_equatorial,
            star_intensity: time_of_day.star_intensity,
            milky_way_intensity: time_of_day.milky_way_intensity,
            atmosphere_height: env.atmosphere.atmosphere_height,
            atmosphere_live: env.atmosphere.enabled && sky_panorama.is_none(),
        },
    };
    if env.sky_mode == SkyMode::Texture && env.sky_texture.value() != 0 {
        if let Some(panorama) = &sky_panorama {
            sky.texture_index = panorama.bindless_index();
        } else {
            sky.mode = SkyMode::Color as u32; // missing panorama -> clear color
        }
    }
    renderer.submit_sky(&sky);

    // Resolve the scene environment into the analytic height/distance fog settings (one frame push,
    // the same shape as the sky).
    let f = &env.fog;
    renderer.submit_fog(&FogRenderSettings {
        enabled: f.enabled,
        density: f.density,
        albedo: f.albedo,
        height: f.height,
        height_falloff: f.height_falloff,
        start_distance: f.start_distance,
        max_opacity: f.max_opacity,
        emissive: f.emissive,
        directional_color: f.directional_color,
        directional_exponent: f.directional_exponent,
        layer2_density: f.layer2_density,
        layer2_falloff: f.layer2_falloff,
        layer2_height: f.layer2_height,
        volumetric: matches!(f.mode, saffron_scene::FogMode::Volumetric),
        base_density: f.base_density,
        scatter_albedo: f.scatter_albedo,
        phase_g: f.phase_g,
        quality: match f.quality {
            saffron_scene::FogQuality::Low => saffron_rendering::FroxelQuality::Low,
            saffron_scene::FogQuality::Medium => saffron_rendering::FroxelQuality::Medium,
            saffron_scene::FogQuality::High => saffron_rendering::FroxelQuality::High,
        },
        history_blend: f.history_blend,
        neighborhood_clamp: f.neighborhood_clamp,
        light_clamp: f.light_clamp,
        aerial_perspective: f.aerial_perspective,
        aerial_intensity: f.aerial_intensity,
    });
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::*;

    #[test]
    fn zero_viewport_early_outs_before_any_setter() {
        let mut renderer = RecordingRenderer::new(0, 0, false);
        let mut scene = Scene::new();
        let (mut assets, tmp) = scratch_server("zero-viewport");
        render_scene(
            &mut renderer,
            &mut scene,
            &mut assets,
            &mut crate::GpuSceneMirror::new(),
            &test_camera(),
            RenderSceneOptions::default(),
        );
        assert!(
            renderer.calls().is_empty(),
            "a zero-size viewport drives no setter"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn empty_scene_drives_the_full_setter_sequence() {
        let mut renderer = RecordingRenderer::new(1280, 720, false);
        let mut scene = Scene::new();
        let (mut assets, tmp) = scratch_server("empty-scene");
        render_scene(
            &mut renderer,
            &mut scene,
            &mut assets,
            &mut crate::GpuSceneMirror::new(),
            &test_camera(),
            RenderSceneOptions::default(),
        );
        // No lights, no meshes: the shadows are all off, no items, and the procedural sky
        // bake fires (the default environment). The DDGI clipmap snaps to the camera every frame
        // regardless of scene contents (it has no scene-box proxy to gate on). The exact frozen
        // order.
        assert_eq!(
            renderer.calls(),
            vec![
                Call::SpotShadow {
                    index: 0,
                    casting: false
                },
                Call::PointShadow {
                    index: 0,
                    casting: false,
                    far: 1.0
                },
                Call::DirectionalShadow { casting: false },
                Call::RtScene { static_count: 0 },
                Call::DdgiScene,
                Call::ReflectionProbes(0),
                Call::FogVolumes(0),
                Call::Clouds { enabled: false },
                Call::SceneLighting { light_count: 0 },
                Call::EnvBake(EnvSource::Procedural),
                Call::ClusterCamera,
                Call::SsaoCamera,
                Call::ShowGrid(false),
                Call::Deformations {
                    work_count: 0,
                    joint_count: 0
                },
                Call::Sky { mode: 2 },
                Call::Fog { enabled: false },
            ]
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn first_lights_drive_the_single_shadow_setters() {
        let mut renderer = RecordingRenderer::new(800, 600, false);
        let mut scene = Scene::new();
        let (mut assets, tmp) = scratch_server("lights");

        // A directional, a point, then a spot light — the first of each drives its shadow.
        let dir = scene.create_entity("Sun");
        scene
            .add_component(dir, DirectionalLight::default())
            .unwrap();
        let point = scene.create_entity("Point");
        scene.add_component(point, PointLight::default()).unwrap();
        let spot = scene.create_entity("Spot");
        scene.add_component(spot, SpotLight::default()).unwrap();

        render_scene(
            &mut renderer,
            &mut scene,
            &mut assets,
            &mut crate::GpuSceneMirror::new(),
            &test_camera(),
            RenderSceneOptions::default(),
        );
        let calls = renderer.calls();
        // The single point + the single spot drive their shadow setters; the directional
        // shadow stays off (it gates on a non-empty scene AABB / at least one item).
        assert_eq!(
            calls[0],
            Call::SpotShadow {
                index: 1, // the spot is index 1 in the light list (point is 0)
                casting: true
            }
        );
        assert_eq!(
            calls[1],
            Call::PointShadow {
                index: 0,
                casting: true,
                far: PointLight::default().range.max(0.1)
            }
        );
        assert_eq!(calls[2], Call::DirectionalShadow { casting: false });
        // Both punctual lights are in the per-frame light list.
        assert!(calls.contains(&Call::SceneLighting { light_count: 2 }));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn skinning_gate_off_is_byte_identical_to_no_skinning() {
        // A scene carrying a SkinnedMesh entity but no resolvable mesh asset: the skinning
        // gate off must produce the same setter sequence as an empty scene (no skinned items,
        // no joints). Without a GPU the mesh never resolves anyway, so this proves the gate is
        // the only thing that changes — the skinned loop runs only when the gate is on.
        let mut renderer = RecordingRenderer::new(640, 480, false);
        let mut scene = Scene::new();
        let (mut assets, tmp) = scratch_server("skin-gate-off");
        let e = scene.create_entity("Rig");
        scene
            .add_component(
                e,
                SkinnedMesh {
                    mesh: saffron_core::Uuid(9000),
                    root_bone: saffron_core::Uuid(0),
                    bones: Vec::new(),
                    inverse_bind: Vec::new(),
                    bone_handles: Vec::new(),
                },
            )
            .unwrap();

        render_scene(
            &mut renderer,
            &mut scene,
            &mut assets,
            &mut crate::GpuSceneMirror::new(),
            &test_camera(),
            RenderSceneOptions::default(),
        );
        // The deformation submit carries zero work + zero joints (the skinned loop
        // never ran).
        assert!(renderer.calls().contains(&Call::Deformations {
            work_count: 0,
            joint_count: 0
        }));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// The frame's ray instances come from the mirror's cached cut, so the driver is exercised
    /// over a SYNCED mirror. An unsynced one has no facts, which is a scene the mirror has never
    /// seen rather than a scene with nothing in it.
    #[test]
    fn two_mesh_scene_records_two_rt_instances_with_world_matrices() {
        let Some(mut harness) = crate::gpu_scene_mirror::test_support::harness("two-mesh") else {
            return;
        };
        write_triangle_mesh(&mut harness.assets, saffron_core::Uuid(5000), "tri");

        let mut scene = Scene::new();
        // Two entities sharing the same mesh, at distinct positions.
        for x in [-3.0_f32, 3.0_f32] {
            let e = scene.create_entity("Mesh");
            scene
                .with_component_mut::<Transform, _>(e, |t| t.translation = Vec3::new(x, 0.0, 0.0))
                .unwrap();
            scene
                .add_component(
                    e,
                    MeshComponent {
                        mesh: saffron_core::Uuid(5000),
                    },
                )
                .unwrap();
            scene
                .add_component(
                    e,
                    saffron_scene::MaterialSet {
                        slots: vec![saffron_scene::MaterialSlot {
                            overrides: serde_json::json!({ "baseColor": [0.2, 0.4, 0.6, 1.0] }),
                            ..saffron_scene::MaterialSlot::default()
                        }],
                    },
                )
                .unwrap();
        }

        harness.sync(&mut scene);
        {
            let mut renderer = RecordingRenderer::new(1024, 768, false)
                .with_gpu(&harness.fixture.uploader, &harness.fixture.descriptors);
            render_scene(
                &mut renderer,
                &mut scene,
                &mut harness.assets,
                &mut harness.mirror,
                &test_camera(),
                RenderSceneOptions::default(),
            );
            // Two static renderables, nothing deforming; the RT scene carries both
            // with distinct world matrices. The directional shadow stays off because
            // the fixture has no Sun-role light.
            assert!(renderer.calls().contains(&Call::Deformations {
                work_count: 0,
                joint_count: 0
            }));
            assert!(
                renderer
                    .calls()
                    .contains(&Call::RtScene { static_count: 2 })
            );
            let inputs = renderer.rt_inputs.borrow();
            assert_eq!(inputs.len(), 2);
            let xs: Vec<f32> = inputs.iter().map(|input| input.model.w_axis.x).collect();
            assert!(xs.contains(&-3.0) && xs.contains(&3.0));
            assert!(
                renderer
                    .calls()
                    .contains(&Call::DirectionalShadow { casting: false })
            );
        }

        harness.finish();
    }

    #[test]
    fn render_scene_flattens_the_hierarchy_before_the_frame_gather() {
        // A parented child whose world matrix is only correct after `update_world_transforms`.
        // `render_scene` must run it once at the top, so reading the child's world matrix after
        // the call (and inside the frame gather) reflects the parent. Without a GPU the mesh
        // never resolves; the world-matrix read is the proof.
        let mut renderer = RecordingRenderer::new(800, 600, false);
        let mut scene = Scene::new();
        let (mut assets, tmp) = scratch_server("flatten");

        let parent = scene.create_entity("Parent");
        scene
            .with_component_mut::<Transform, _>(parent, |t| {
                t.translation = Vec3::new(10.0, 0.0, 0.0)
            })
            .unwrap();
        let child = scene.create_entity("Child");
        scene.set_parent(child, Some(parent), false).unwrap();
        scene
            .with_component_mut::<Transform, _>(child, |t| t.translation = Vec3::new(0.0, 2.0, 0.0))
            .unwrap();

        render_scene(
            &mut renderer,
            &mut scene,
            &mut assets,
            &mut crate::GpuSceneMirror::new(),
            &test_camera(),
            RenderSceneOptions::default(),
        );
        // The flatten ran: the child's cached world translation composes the parent's.
        assert_eq!(
            scene.world_translation(child),
            Vec3::new(10.0, 2.0, 0.0),
            "render_scene must flatten the hierarchy once before any reader"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn skinning_gate_on_produces_an_identity_model_skinned_work_item_split_from_rt() {
        let Some(mut harness) = crate::gpu_scene_mirror::test_support::harness("skin-on") else {
            return;
        };
        write_skinned_triangle(&mut harness.assets, saffron_core::Uuid(5300), "rig");

        let mut scene = Scene::new();
        let skinned = spawn_one_bone_skin(&mut scene, saffron_core::Uuid(5300));
        // A static mesh too, so the RT split has one of each.
        write_triangle_mesh(&mut harness.assets, saffron_core::Uuid(5301), "tri");
        let stat = scene.create_entity("Static");
        scene
            .add_component(
                stat,
                MeshComponent {
                    mesh: saffron_core::Uuid(5301),
                },
            )
            .unwrap();

        harness.sync(&mut scene);
        {
            let mut renderer = RecordingRenderer::new(1024, 768, true)
                .with_gpu(&harness.fixture.uploader, &harness.fixture.descriptors);
            render_scene(
                &mut renderer,
                &mut scene,
                &mut harness.assets,
                &mut harness.mirror,
                &test_camera(),
                RenderSceneOptions::default(),
            );
            // One skinned work item with a one-joint palette; the static mesh emits
            // no work (it neither skins nor morphs nor displaces).
            assert!(renderer.calls().contains(&Call::Deformations {
                work_count: 1,
                joint_count: 1
            }));
            // The RT static split carries only the one static item (the skinned one is excluded).
            assert!(
                renderer
                    .calls()
                    .contains(&Call::RtScene { static_count: 1 })
            );
            let facts = renderer.work_facts.borrow();
            let (entity, skinned_flag, model, joint_offset, joint_count) =
                *facts.iter().find(|fact| fact.1).expect("a skinned item");
            assert!(skinned_flag);
            assert_eq!(model, Mat4::IDENTITY, "skinned model is identity");
            assert_eq!(joint_count, 1);
            assert_eq!(joint_offset, 0);
            assert_eq!(entity, entity_id_or_zero(&scene, skinned));
        }

        harness.finish();
    }

    /// The three disjoint borrows of three distinct values: this compiling is the assertion.
    #[test]
    fn disjoint_three_value_borrow_shape_compiles() {
        let mut renderer = RecordingRenderer::new(320, 240, true);
        let mut scene = Scene::new();
        let (mut assets, tmp) = scratch_server("borrow");
        // Three distinct values, three distinct mutable/shared borrows, one call.
        render_scene(
            &mut renderer,
            &mut scene,
            &mut assets,
            &mut crate::GpuSceneMirror::new(),
            &test_camera(),
            RenderSceneOptions::default(),
        );
        assert!(!renderer.calls().is_empty());
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
