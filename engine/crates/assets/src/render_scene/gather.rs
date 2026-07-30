use std::collections::HashSet;

use super::*;

/// A scene's directional light resolved for one frame.
///
/// `direction` is the world-space travel direction, guaranteed non-zero so every downstream
/// `normalize` stays finite. Absent (see [`gather_directional_light`]) when the scene carries
/// no directional light, in which case the scene has no direct sun.
pub(super) struct DirectionalResolved {
    pub(super) direction: Vec3,
    pub(super) color: Vec3,
    pub(super) intensity: f32,
    pub(super) ambient: f32,
    pub(super) volumetric_scattering: f32,
    pub(super) cast_volumetric_shadow: bool,
}

impl DirectionalResolved {
    /// The dark placeholder for a scene with no directional light: zero intensity and zero
    /// ambient (no direct sun), a valid direction so the sky/LUT math stays finite.
    pub(super) fn none() -> Self {
        Self {
            direction: DirectionalLight::DEFAULT_DIRECTION.normalize(),
            color: Vec3::ONE,
            intensity: 0.0,
            ambient: 0.0,
            volumetric_scattering: 0.0,
            cast_volumetric_shadow: false,
        }
    }
}

/// Resolves the first directional light for each atmosphere role, including entity rotation.
pub(super) fn gather_directional_lights(
    scene: &mut Scene,
    overrides: CelestialDirectionOverrides,
) -> (Option<DirectionalResolved>, Option<DirectionalResolved>) {
    let mut sun: Option<(Entity, DirectionalLight)> = None;
    let mut moon: Option<(Entity, DirectionalLight)> = None;
    scene.for_each::<&DirectionalLight, _>(|entity, light| {
        let slot = match light.atmosphere_role {
            AtmosphereRole::Sun => &mut sun,
            AtmosphereRole::Moon => &mut moon,
        };
        if slot.is_none() {
            *slot = Some((entity, *light));
        }
    });
    (
        resolve_directional(scene, sun, overrides.sun),
        resolve_directional(scene, moon, overrides.moon),
    )
}

fn resolve_directional(
    scene: &Scene,
    found: Option<(Entity, DirectionalLight)>,
    direction_override: Option<Vec3>,
) -> Option<DirectionalResolved> {
    let (entity, light) = found?;
    let aimed = direction_override.unwrap_or_else(|| {
        if scene.has_component::<Transform>(entity) {
            scene.world_rotation(entity) * light.direction
        } else {
            light.direction
        }
    });
    // A degenerate authored direction would `normalize` to NaN downstream; fall back to the
    // canonical aim so the sun stays finite regardless of what the user typed.
    let direction = aimed
        .try_normalize()
        .unwrap_or_else(|| DirectionalLight::DEFAULT_DIRECTION.normalize());
    Some(DirectionalResolved {
        direction,
        color: light.color,
        intensity: light.intensity,
        ambient: light.ambient,
        volumetric_scattering: light.volumetric_scattering,
        cast_volumetric_shadow: light.cast_volumetric_shadow,
    })
}

/// The first point light's shadow inputs (the single shadowed point in v1).
#[derive(Clone, Copy)]
pub(super) struct PointShadow {
    pub(super) pos: Vec3,
    pub(super) far: f32,
    pub(super) light_index: u32,
}

/// The first spot light's shadow inputs (the single shadowed spot in v1).
#[derive(Clone, Copy)]
pub(super) struct SpotShadow {
    pub(super) view_proj: Mat4,
    pub(super) light_index: u32,
}

/// Packs one point light into the shared punctual-light GPU layout.
pub(crate) fn gpu_point_light(light: &PointLight, position: Vec3) -> GpuLight {
    GpuLight {
        position_range: position.extend(light.range),
        color_intensity: light.color.extend(light.intensity),
        direction_type: Vec4::ZERO,
        spot_cos: Vec4::new(
            0.0,
            0.0,
            light.volumetric_scattering,
            if light.cast_volumetric_shadow {
                1.0
            } else {
                0.0
            },
        ),
    }
}

/// Packs one spot light into the shared punctual-light GPU layout.
pub(crate) fn gpu_spot_light(light: &SpotLight, position: Vec3, direction: Vec3) -> GpuLight {
    GpuLight {
        position_range: position.extend(light.range),
        color_intensity: light.color.extend(light.intensity),
        direction_type: direction.extend(1.0),
        spot_cos: Vec4::new(
            light.inner_angle.to_radians().cos(),
            light.outer_angle.to_radians().cos(),
            light.volumetric_scattering,
            if light.cast_volumetric_shadow {
                1.0
            } else {
                0.0
            },
        ),
    }
}

/// Gathers the punctual (point + spot) lights into the per-frame [`GpuLight`] list, tracking
/// the first point's position/range and the first spot's perspective light-space transform.
pub(super) fn gather_punctual_lights(
    scene: &mut Scene,
) -> (Vec<GpuLight>, Option<PointShadow>, Option<SpotShadow>) {
    let mut points: Vec<(Entity, PointLight)> = Vec::new();
    scene.for_each::<(&Transform, &PointLight), _>(|entity, (_, light)| {
        points.push((entity, *light));
    });
    let mut spots: Vec<(Entity, SpotLight)> = Vec::new();
    scene.for_each::<(&Transform, &SpotLight), _>(|entity, (_, light)| {
        spots.push((entity, *light));
    });

    let mut lights: Vec<GpuLight> = Vec::new();
    let mut point_shadow: Option<PointShadow> = None;
    for (entity, light) in points {
        let pos = scene.world_translation(entity);
        lights.push(gpu_point_light(&light, pos));
        if point_shadow.is_none() {
            point_shadow = Some(PointShadow {
                pos,
                far: light.range.max(0.1),
                light_index: (lights.len() - 1) as u32,
            });
        }
    }

    let mut spot_shadow: Option<SpotShadow> = None;
    for (entity, light) in spots {
        let pos = scene.world_translation(entity);
        let dir = (scene.world_rotation(entity) * light.direction).normalize();
        let index = lights.len() as u32;
        lights.push(gpu_spot_light(&light, pos, dir));
        if spot_shadow.is_none() {
            // A perspective frustum down the spot cone: fov = 2 x outer angle (a small pad so
            // the penumbra sits inside the map), aspect 1, near/far from range.
            let fov = (2.0 * light.outer_angle + 2.0).min(179.0).to_radians();
            let up = look_at_up_for_dir(dir);
            let light_view = look_at(pos, pos + dir, up);
            let light_proj = perspective(fov, 1.0, 0.05, light.range.max(0.1));
            spot_shadow = Some(SpotShadow {
                view_proj: light_proj * light_view,
                light_index: index,
            });
        }
    }
    (lights, point_shadow, spot_shadow)
}

/// The accumulating deformation-work + ray-instance state built across the static and
/// skinned passes.
#[derive(Default)]
pub(super) struct FrameSceneBuild {
    /// The record-driven deformation work (skinned / morph / displaced instances).
    pub(super) work: Vec<saffron_rendering::DeformationWork>,
    /// The concatenated frame joint palette the skinned work indexes.
    pub(super) frame_joints: Vec<Mat4>,
    /// Mirrored instances in the scene's world (drives the shadow "anything to cast" gate).
    pub(super) renderable_count: usize,
    /// Ray instances dropped for sitting outside the reachable GI window.
    pub(super) rt_instances_culled: u32,
    /// The frame's static RT instances (skinned casters ride the deformation gather's
    /// refit entries instead).
    pub(super) rt_instances: Arc<[saffron_rendering::RtInstanceInput]>,
    /// Instances whose frame facts this gather derived — zero on a frame that reused the
    /// mirror's cut and has nothing deforming.
    pub(super) entities_derived: u32,
}

/// Gathers the frame's ray instances and one [`DeformationWork`] item per morphing or
/// displaced static instance. Draw commands come from the GPU scene's visibility traversal;
/// nothing here builds a draw list.
///
/// Every per-instance fact — world bounds, opacity class, displacement — is read from the
/// mirror, which derived it when the journal last touched that entity. This gather therefore
/// costs what the frame *changed*, not what the scene holds: it touches the ECS only for the
/// components no journal covers (an animated clip rewrites morph weights every frame), and
/// those queries visit only the entities carrying them.
pub(super) fn gather_static_frame_facts<R: SceneRenderer>(
    renderer: &R,
    scene: &mut Scene,
    mirror: &mut crate::GpuSceneMirror,
    eye: Vec3,
    build: &mut FrameSceneBuild,
) {
    let scene_instance = scene.instance_id();
    let rays = mirror.ray_instances(scene_instance, saffron_rendering::gi_occluder_bounds(eye));
    build.rt_instances = rays.instances;
    build.rt_instances_culled = rays.culled;
    build.entities_derived += rays.derived;
    build.renderable_count = mirror.world_instance_count(scene_instance);

    let displacement = renderer.displacement_enabled();
    let mut candidates: Vec<Entity> = if displacement {
        mirror.displaced_static_entities(scene_instance)
    } else {
        Vec::new()
    };
    let mut seen: HashSet<Entity> = candidates.iter().copied().collect();
    // Morph weights are rewritten by whatever plays the clip, under no journal the mirror reads,
    // so they are resolved live. Both queries are archetype-scoped: they visit the entities
    // carrying morph state, never the scene.
    scene.for_each::<(&Transform, &MeshComponent, &MorphComponent), _>(|entity, _| {
        if seen.insert(entity) {
            candidates.push(entity);
        }
    });
    scene.for_each::<(&Transform, &MeshComponent, &MorphWeightOverride), _>(|entity, _| {
        if seen.insert(entity) {
            candidates.push(entity);
        }
    });

    for entity in candidates {
        let Some(instance) = mirror.mirrored_instance(scene_instance, entity, false) else {
            continue;
        };
        let morph_weights = morph_weights_for(scene, entity);
        let displace = if displacement {
            instance.displace
        } else {
            None
        };
        if morph_weights.is_empty() && displace.is_none() {
            continue;
        }
        build.entities_derived += 1;
        build.work.push(saffron_rendering::DeformationWork {
            mesh: instance.mesh,
            entity: entity_id_or_zero(scene, entity),
            skinned: false,
            joint_offset: 0,
            joint_count: 0,
            morph_weights,
            model: instance.model,
            displace,
            instance_slot: instance.instance_slot,
        });
    }
}

/// Gathers the skinned `Transform + SkinnedMesh` renderables' frame facts (identity model,
/// joint palette via [`Scene::joint_matrices`]): one [`DeformationWork`] item per skinned
/// instance. Called only when skinning is on. The query is archetype-scoped to the skinned
/// entities, whose palettes have to be composed every frame regardless.
pub(super) fn gather_skinned_frame_facts<R: SceneRenderer>(
    renderer: &R,
    scene: &mut Scene,
    mirror: &crate::GpuSceneMirror,
    build: &mut FrameSceneBuild,
) {
    let scene_instance = scene.instance_id();
    let displacement = renderer.displacement_enabled();
    let mut skins: Vec<(Entity, SkinnedMesh)> = Vec::new();
    scene.for_each::<(&Transform, &SkinnedMesh), _>(|entity, (_, skin)| {
        skins.push((entity, skin.clone()));
    });

    for (entity, skin) in skins {
        let Some(instance) = mirror.mirrored_instance(scene_instance, entity, true) else {
            continue;
        };
        if instance.mesh.skin_buffer().is_none() {
            continue; // baked without a skin stream
        }
        let palette = scene.joint_matrices(&skin);
        if palette.is_empty() {
            continue;
        }
        build.entities_derived += 1;
        build.work.push(saffron_rendering::DeformationWork {
            mesh: instance.mesh,
            entity: entity_id_or_zero(scene, entity),
            skinned: true,
            joint_offset: build.frame_joints.len() as u32,
            joint_count: palette.len() as u32,
            morph_weights: morph_weights_for(scene, entity),
            model: Mat4::IDENTITY,
            displace: if displacement {
                instance.displace
            } else {
                None
            },
            instance_slot: instance.instance_slot,
        });
        build.frame_joints.extend_from_slice(&palette);
    }
}

/// Snapshots each [`ReflectionProbe`] (positioned by its [`Transform`]) into a per-frame
/// upload list, consuming each probe's `dirty` flag (capped at [`MAX_REFLECTION_PROBES`]).
pub(super) fn gather_reflection_probes(scene: &mut Scene) -> Vec<ReflectionProbeUpload> {
    let mut probes: Vec<(Entity, ReflectionProbe)> = Vec::new();
    scene.for_each::<(&Transform, &mut ReflectionProbe), _>(|entity, (_, probe)| {
        if probes.len() < MAX_REFLECTION_PROBES as usize {
            probes.push((entity, *probe));
            probe.dirty = false; // consumed; the renderer tracks capture state from here
        }
    });
    probes
        .into_iter()
        .map(|(entity, probe)| ReflectionProbeUpload {
            entity: entity_id_or_zero(scene, entity),
            origin: scene.world_translation(entity),
            influence_radius: probe.influence_radius,
            intensity: probe.intensity,
            box_projection: probe.box_projection,
            box_extent: probe.box_extent,
            dirty: probe.dirty,
        })
        .collect()
}

/// Snapshots each [`FogVolume`] (positioned by its [`Transform`]) into a per-frame upload list, its
/// world transform baked for the froxel injection bounds test + noise frame (capped at
/// [`MAX_FOG_VOLUMES`]).
pub(super) fn gather_fog_volumes(scene: &mut Scene) -> Vec<FogVolumeUpload> {
    let mut volumes: Vec<(Entity, FogVolume)> = Vec::new();
    scene.for_each::<(&Transform, &FogVolume), _>(|entity, (_, volume)| {
        if volumes.len() < MAX_FOG_VOLUMES as usize {
            volumes.push((entity, *volume));
        }
    });
    volumes
        .into_iter()
        .map(|(entity, volume)| {
            let world_from_local = scene.world_matrix(entity);
            FogVolumeUpload {
                world_from_local,
                center: world_from_local.col(3).truncate(),
                shape: match volume.shape {
                    FogShape::Box => FOG_SHAPE_BOX,
                    FogShape::Sphere => FOG_SHAPE_SPHERE,
                },
                extents: volume.extents,
                radius: volume.radius,
                edge_falloff: volume.edge_falloff,
                density: volume.density,
                albedo: volume.albedo,
                emissive: volume.emissive,
                phase_g: volume.phase_g,
                height_falloff: volume.height_falloff,
                noise_scale: volume.noise_scale,
                noise_intensity: volume.noise_intensity,
                noise_detail: volume.noise_detail,
                wind: volume.wind,
                speed: volume.speed,
            }
        })
        .collect()
}

/// Drives the environment bake from the scene environment + the sun derived from the
/// directional light, returning the loaded sky panorama (Texture mode) for the visible-sky
/// resolve below.
pub(super) fn drive_env_bake<R: SceneRenderer>(
    renderer: &mut R,
    scene: &Scene,
    assets: &mut AssetServer,
    sun: &DirectionalResolved,
    moon: &DirectionalResolved,
    moon_illuminated_fraction: f32,
) -> Option<Arc<saffron_rendering::GpuTexture>> {
    let env = &scene.environment;
    let at = env.atmosphere;
    let sky_bake = SkygenParams {
        sun_dir: -sun.direction,
        sun_intensity: sun.intensity,
        sun_color: sun.color,
        moon_dir: -moon.direction,
        moon_intensity: moon.intensity,
        moon_illuminated_fraction,
        atmosphere: saffron_rendering::AtmosphereParams {
            enabled: at.enabled,
            planet_radius: at.planet_radius,
            atmosphere_height: at.atmosphere_height,
            rayleigh_scattering: at.rayleigh_scattering,
            rayleigh_scale_height: at.rayleigh_scale_height,
            mie_scattering: at.mie_scattering,
            mie_scale_height: at.mie_scale_height,
            mie_anisotropy: at.mie_anisotropy,
            ozone_absorption: at.ozone_absorption,
            sun_disk_angular_radius: at.sun_disk_angular_radius,
            sun_disk_intensity: at.sun_disk_intensity,
            moon_disk_angular_radius: at.moon_disk_angular_radius,
            moon_disk_intensity: at.moon_disk_intensity,
            moon_earthshine: at.moon_earthshine,
            per_pixel_transmittance: at.per_pixel_transmittance,
            sky_capture_cadence: at.sky_capture_cadence,
        },
    };
    // Resolution order: a user equirect panorama wins, then the atmosphere, then the
    // gradient. Only a valid loaded panorama claims Equirect.
    let want_equirect = env.sky_mode == SkyMode::Texture && env.sky_texture.value() != 0;
    let sky_panorama = if want_equirect {
        assets.load_texture_asset(renderer, env.sky_texture)
    } else {
        None
    };
    if let Some(panorama) = &sky_panorama {
        renderer.request_env_bake(EnvSource::Equirect, Some(Arc::clone(panorama)), sky_bake);
    } else if at.enabled {
        renderer.request_env_bake(EnvSource::Atmosphere, None, sky_bake);
    } else {
        renderer.request_env_bake(EnvSource::Procedural, None, sky_bake);
    }
    sky_panorama
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_directional_light_resolves_to_none() {
        let mut scene = Scene::new();
        let (sun, moon) = gather_directional_lights(&mut scene, CelestialDirectionOverrides::NONE);
        assert!(sun.is_none());
        assert!(moon.is_none());
    }

    #[test]
    fn directional_light_resolves_with_a_normalized_direction() {
        let mut scene = Scene::new();
        let sun = scene.create_entity("Sun");
        scene
            .add_component(
                sun,
                DirectionalLight {
                    direction: Vec3::new(0.0, -2.0, 0.0),
                    ..DirectionalLight::default()
                },
            )
            .unwrap();
        let (resolved, moon) =
            gather_directional_lights(&mut scene, CelestialDirectionOverrides::NONE);
        let resolved = resolved.expect("a sun");
        assert!(moon.is_none());
        assert_eq!(resolved.direction, Vec3::new(0.0, -1.0, 0.0));
        assert_eq!(resolved.intensity, 1.0);
    }

    #[test]
    fn degenerate_direction_falls_back_to_a_finite_unit_aim() {
        let mut scene = Scene::new();
        let sun = scene.create_entity("Sun");
        scene
            .add_component(
                sun,
                DirectionalLight {
                    direction: Vec3::ZERO,
                    ..DirectionalLight::default()
                },
            )
            .unwrap();
        let (resolved, _) =
            gather_directional_lights(&mut scene, CelestialDirectionOverrides::NONE);
        let resolved = resolved.expect("a sun");
        assert!(resolved.direction.is_finite());
        assert!((resolved.direction.length() - 1.0).abs() < 1e-5);
    }

    #[test]
    fn directional_lights_resolve_by_atmosphere_role() {
        let mut scene = Scene::new();
        let moon = scene.create_entity("Moon");
        scene
            .add_component(
                moon,
                DirectionalLight {
                    atmosphere_role: AtmosphereRole::Moon,
                    intensity: 0.5,
                    ..DirectionalLight::default()
                },
            )
            .unwrap();
        let sun = scene.create_entity("Sun");
        scene
            .add_component(sun, DirectionalLight::default())
            .unwrap();

        let (sun, moon) = gather_directional_lights(&mut scene, CelestialDirectionOverrides::NONE);
        assert_eq!(sun.expect("sun").intensity, 1.0);
        assert_eq!(moon.expect("moon").intensity, 0.5);
    }

    #[test]
    fn celestial_override_is_frame_local_and_preserves_authored_direction() {
        let mut scene = Scene::new();
        let sun = scene.create_entity("Sun");
        let authored = Vec3::new(0.25, -0.9, 0.35);
        scene
            .add_component(
                sun,
                DirectionalLight {
                    direction: authored,
                    ..DirectionalLight::default()
                },
            )
            .unwrap();

        let overrides = CelestialDirectionOverrides {
            sun: Some(Vec3::X),
            moon: None,
        };
        let (resolved, _) = gather_directional_lights(&mut scene, overrides);

        assert_eq!(resolved.expect("sun").direction, Vec3::X);
        let stored = scene
            .with_component::<DirectionalLight, _>(sun, |light| light.direction)
            .expect("authored light");
        assert_eq!(stored, authored);
    }
}
