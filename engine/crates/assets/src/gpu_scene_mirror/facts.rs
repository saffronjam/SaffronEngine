use super::*;

/// Whether two world-space bounds overlap.
///
/// The ray and SDF occluder lists are cut against the coarsest distance-field cascade window
/// rather than the camera frustum: light reaches a visible surface from off-screen and a
/// reflection shows what the camera cannot see, so a frustum test would delete contributors that
/// legitimately change the picture. An occluder outside the coarsest cascade cannot influence any
/// march, which is what makes this cut sound where a frustum cut would not be.
fn window_intersects(bounds: (Vec3, Vec3), window: (Vec3, Vec3)) -> bool {
    !(bounds.1.cmplt(window.0).any() || bounds.0.cmpgt(window.1).any())
}

/// The world-space bounds of `local_min`..`local_max` under `model`.
pub(super) fn world_bounds(model: &Mat4, local_min: Vec3, local_max: Vec3) -> (Vec3, Vec3) {
    let mut min = Vec3::splat(f32::MAX);
    let mut max = Vec3::splat(f32::MIN);
    saffron_geometry::world_aabb_from_corners(model, local_min, local_max, &mut min, &mut max);
    (min, max)
}

impl GpuSceneMirror {
    fn world_for(&self, scene_instance: Uuid) -> Option<&WorldMirror> {
        self.worlds
            .values()
            .find(|world| world.scene_instance == scene_instance)
    }

    /// Mirrored instances in the world bound to `scene_instance` — mesh instances of both
    /// sources plus resident plants. Drives the "is there anything to cast a shadow" gate.
    #[must_use]
    pub fn world_instance_count(&self, scene_instance: Uuid) -> usize {
        self.world_for(scene_instance)
            .map_or(0, |world| world.instances.len() + world.plants.len())
    }

    /// This frame's ray instances for the scene bound to `scene_instance`, cut against `window`.
    ///
    /// Re-derived only when the mirror changed or the window stepped; otherwise the cached cut is
    /// handed back whole, which is what keeps frame preparation proportional to changes rather
    /// than to how many instances the scene holds.
    pub fn ray_instances(
        &mut self,
        scene_instance: Uuid,
        window: (Vec3, Vec3),
    ) -> FrameRayInstances {
        let Some(key) = self
            .worlds
            .iter()
            .find(|(_, world)| world.scene_instance == scene_instance)
            .map(|(key, _)| *key)
        else {
            return FrameRayInstances {
                instances: Arc::from([]),
                culled: 0,
                derived: 0,
            };
        };
        if let Some(cut) = &self.worlds[&key].rays
            && cut.window == window
        {
            return FrameRayInstances {
                instances: Arc::clone(&cut.instances),
                culled: cut.culled,
                derived: 0,
            };
        }

        let world = &self.worlds[&key];
        let mut instances: Vec<saffron_rendering::RtInstanceInput> = Vec::new();
        let mut culled = 0;
        let mut derived = 0;
        // Static sources only. A skinned instance's ray geometry is its deformed vertex stream,
        // already world-space and referenced by an identity transform through the deformation
        // gather's refit entries; placing it here as well would put the same caster in the
        // structure twice, once at its bind pose.
        for entry in world
            .instances
            .iter()
            .filter(|((_, source), _)| *source == InstanceSource::Static)
            .map(|(_, entry)| entry)
        {
            derived += 1;
            if !window_intersects(entry.facts.bounds, window) {
                culled += 1;
                continue;
            }
            instances.push(saffron_rendering::RtInstanceInput {
                model: entry.facts.model,
                mesh: Arc::clone(&entry.facts.mesh),
                instance_slot: entry.handle.raw().index,
                opacity_override: entry.facts.opacity_override,
                combination: entry.facts.combination,
                // The wind prepass displaces vegetation only; an ECS mesh instance never carries
                // the flag, so its structure at rest pose is the pose every pass draws.
                wind: false,
            });
        }
        // Vegetation never enters the ECS; its placed plants reach the structure through the
        // records the mirror wrote while syncing the resident cells, cut against the same window
        // because a resident cell can extend far past what any ray reaches.
        //
        // A plant never overrides opacity: it binds the materials it was cooked with, so the
        // per-submesh classes baked into its structure stay authoritative and a micromap can
        // refine them.
        for entry in world.plants.values() {
            let Some(mesh) = self.shared.meshes.get(&entry.mesh) else {
                continue;
            };
            if mesh.mesh.assembly_blas.is_empty() && mesh.mesh.blas.is_none() {
                continue;
            }
            let GpuSceneTransform::Static(placement) = entry.record.transform else {
                continue;
            };
            derived += 1;
            let model = placement.to_matrix();
            if !window_intersects(
                world_bounds(&model, mesh.mesh.bounds_min, mesh.mesh.bounds_max),
                window,
            ) {
                culled += 1;
                continue;
            }
            instances.push(saffron_rendering::RtInstanceInput {
                model,
                mesh: Arc::clone(&mesh.mesh),
                instance_slot: entry.handle.raw().index,
                opacity_override: None,
                combination: entry.record.combination,
                // Every placed plant is wind-flagged, so its structure is materialized from the
                // wind record rather than built at rest pose — as far as the frame's budget reaches.
                wind: entry.record.flags & GPU_SCENE_INSTANCE_FLAG_WIND != 0,
            });
        }

        let cut = RayCut {
            window,
            instances: Arc::from(instances),
            culled,
        };
        let published = FrameRayInstances {
            instances: Arc::clone(&cut.instances),
            culled,
            derived,
        };
        self.worlds.get_mut(&key).expect("world present").rays = Some(cut);
        published
    }

    /// Entities in the world bound to `scene_instance` whose resolved materials displace — the
    /// static half of the frame's deformation work list.
    ///
    /// Read straight off the set the resolve maintains, so the cost is the number of displaced
    /// instances rather than the number the world holds.
    #[must_use]
    pub fn displaced_static_entities(&self, scene_instance: Uuid) -> Vec<Entity> {
        self.world_for(scene_instance)
            .map_or_else(Vec::new, |world| world.displaced.iter().copied().collect())
    }

    /// One mirrored instance's deformation inputs, or `None` when the entity is not mirrored
    /// under that source in this scene's world.
    #[must_use]
    pub fn mirrored_instance(
        &self,
        scene_instance: Uuid,
        entity: Entity,
        skinned: bool,
    ) -> Option<MirroredInstance> {
        let source = if skinned {
            InstanceSource::Skinned
        } else {
            InstanceSource::Static
        };
        let entry = self
            .world_for(scene_instance)?
            .instances
            .get(&(entity, source))?;
        Some(MirroredInstance {
            mesh: Arc::clone(&entry.facts.mesh),
            model: entry.facts.model,
            instance_slot: entry.handle.raw().index,
            displace: entry.facts.displace,
        })
    }
}

impl WorldMirror {
    /// Drops the world's ray cut. Called by every path that creates, updates, or removes a
    /// mirrored instance or plant: the cut names concrete transforms and structures, so a stale
    /// one silently keeps a destroyed instance casting or a moved one casting from where it was.
    pub(super) fn invalidate_rays(&mut self) {
        self.rays = None;
    }

    /// Records whether `entity`'s static instance displaces, keeping the displacement set in
    /// step with the facts the resolve just derived.
    pub(super) fn track_displacement(&mut self, entity: Entity, displaces: bool) {
        if displaces {
            self.displaced.insert(entity);
        } else {
            self.displaced.remove(&entity);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::*;
    use saffron_scene::Transform;

    /// The whole point of caching the cut: a frame that changed nothing re-derives nothing, so
    /// frame preparation costs what moved rather than what the scene holds. Every term that can
    /// invalidate it is exercised — a transform, a destroy, and the reach window stepping — because
    /// a cut that went stale on any of them would silently keep a destroyed instance casting.
    #[test]
    fn the_ray_cut_is_reused_until_the_mirror_or_the_reach_window_moves() {
        let Some(mut harness) = harness("ray-cut") else {
            return;
        };
        let mesh_id = Uuid(7701);
        write_triangle_mesh(&mut harness.assets, mesh_id, "tri");

        let mut scene = Scene::new();
        let mut entities = Vec::new();
        for x in [-3.0_f32, 3.0] {
            let entity = scene.create_entity("Mesh");
            scene
                .with_component_mut::<Transform, _>(entity, |t| {
                    t.translation = Vec3::new(x, 0.0, 0.0)
                })
                .unwrap();
            scene
                .add_component(entity, MeshComponent { mesh: mesh_id })
                .unwrap();
            entities.push(entity);
        }
        harness.sync(&mut scene);
        let instance = scene.instance_id();
        let window = (Vec3::splat(-100.0), Vec3::splat(100.0));

        // Scoped: a published cut pins an `Arc<GpuMesh>`, so every one has to be dropped before
        // the fixture tears the device down.
        {
            let first = harness.mirror.ray_instances(instance, window);
            assert_eq!(
                first.instances.len(),
                2,
                "both meshes are inside the window"
            );
            assert_eq!(first.derived, 2, "the first cut walks the mirrored set");

            let cached = harness.mirror.ray_instances(instance, window);
            assert_eq!(
                cached.derived, 0,
                "an unchanged mirror and an unmoved window re-derive nothing"
            );
            assert!(
                Arc::ptr_eq(&cached.instances, &first.instances),
                "the cached cut is handed back as the same allocation"
            );

            let stepped = harness
                .mirror
                .ray_instances(instance, (Vec3::splat(-90.0), Vec3::splat(110.0)));
            assert_eq!(stepped.derived, 2, "a moved reach window re-cuts");

            // A transform touch: the cut must be re-derived and carry the new placement.
            scene
                .with_component_mut::<Transform, _>(entities[0], |t| {
                    t.translation = Vec3::new(-9.0, 0.0, 0.0);
                })
                .unwrap();
            harness.sync(&mut scene);
            let moved = harness.mirror.ray_instances(instance, window);
            assert_eq!(moved.derived, 2, "a moved instance re-cuts");
            let xs: Vec<f32> = moved
                .instances
                .iter()
                .map(|input| input.model.w_axis.x)
                .collect();
            assert!(
                xs.contains(&-9.0),
                "the re-cut carries the moved placement, not the one it was built with ({xs:?})"
            );

            scene.destroy_entity(entities[1]);
            harness.sync(&mut scene);
            let after_destroy = harness.mirror.ray_instances(instance, window);
            assert_eq!(
                after_destroy.instances.len(),
                1,
                "a destroyed instance leaves the cut"
            );

            // Outside the window, the same instance is cut rather than published.
            let elsewhere = harness
                .mirror
                .ray_instances(instance, (Vec3::splat(500.0), Vec3::splat(600.0)));
            assert!(elsewhere.instances.is_empty());
            assert_eq!(elsewhere.culled, 1);
        }

        harness.finish();
    }

    #[test]
    fn the_ray_cut_keeps_what_a_ray_can_reach_and_drops_what_it_cannot() {
        // The ray list is cut against REACH, not visibility, and the difference is the whole point:
        // a reflection shows the camera what it cannot see and a march gathers from behind it, so
        // a frustum test would delete contributors that legitimately change the picture. An
        // occluder outside the coarsest cascade cannot influence either, which is what makes this
        // cut sound where a frustum cut would not be.
        let window = (Vec3::splat(-10.0), Vec3::splat(10.0));
        let unit_min = Vec3::splat(-0.5);
        let unit_max = Vec3::splat(0.5);
        let reachable =
            |model: Mat4| window_intersects(world_bounds(&model, unit_min, unit_max), window);

        // Directly behind the eye, well inside the window: a frustum cull gets this backwards.
        assert!(reachable(Mat4::from_translation(Vec3::new(0.0, 0.0, 9.0))));

        // Far outside the coarsest cascade: provably unable to contribute.
        assert!(!reachable(Mat4::from_translation(Vec3::new(
            0.0, 0.0, 40.0
        ))));

        // Straddling the boundary counts as reachable — the cut may only drop what it can PROVE
        // unreachable, so the inclusive edge is the conservative direction.
        assert!(reachable(Mat4::from_translation(Vec3::new(0.0, 0.0, 10.4))));

        // A scaled instance is tested on its WORLD extent, not its local one: a large object whose
        // origin sits outside the window still reaches into it.
        assert!(reachable(
            Mat4::from_scale(Vec3::splat(40.0)) * Mat4::from_translation(Vec3::new(0.0, 0.0, 0.55))
        ));
    }

    /// The displacement set is maintained by the resolve rather than filtered out of the instance
    /// map per frame, so it has to stay in step with the facts on every term that can change one:
    /// a material that starts displacing, one that stops, and a destroy. A stale set either
    /// amplifies geometry for an entity that no longer displaces or silently drops one that does.
    #[test]
    fn the_displacement_set_follows_the_materials_the_instances_resolve() {
        let Some(mut harness) = harness("displaced-set") else {
            return;
        };
        let mesh_id = Uuid(7801);
        write_triangle_mesh(&mut harness.assets, mesh_id, "tri");
        let displacing = write_displacing_material(&mut harness.assets, "relief");

        let mut scene = Scene::new();
        let plain = scene.create_entity("Plain");
        scene
            .add_component(plain, MeshComponent { mesh: mesh_id })
            .unwrap();
        let relief = scene.create_entity("Relief");
        scene
            .add_component(relief, MeshComponent { mesh: mesh_id })
            .unwrap();
        scene
            .add_component(
                relief,
                saffron_scene::MaterialSet {
                    slots: vec![saffron_scene::MaterialSlot {
                        material: displacing,
                        ..saffron_scene::MaterialSlot::default()
                    }],
                },
            )
            .unwrap();
        harness.sync(&mut scene);
        let instance = scene.instance_id();

        assert_eq!(
            harness.mirror.displaced_static_entities(instance),
            vec![relief],
            "only the entity binding the displacing material is a candidate"
        );

        // The same entity rebound to the built-in default material stops displacing.
        scene
            .with_component_mut::<saffron_scene::MaterialSet, _>(relief, |set| {
                set.slots[0].material = Uuid(0);
            })
            .unwrap();
        harness.sync(&mut scene);
        assert!(
            harness
                .mirror
                .displaced_static_entities(instance)
                .is_empty(),
            "an entity that stopped displacing leaves the set"
        );

        // Back to displacing, then destroyed: the destroy must clear it too.
        scene
            .with_component_mut::<saffron_scene::MaterialSet, _>(relief, |set| {
                set.slots[0].material = displacing;
            })
            .unwrap();
        harness.sync(&mut scene);
        assert_eq!(harness.mirror.displaced_static_entities(instance).len(), 1);
        scene.destroy_entity(relief);
        harness.sync(&mut scene);
        assert!(
            harness
                .mirror
                .displaced_static_entities(instance)
                .is_empty(),
            "a destroyed instance leaves the set"
        );

        harness.finish();
    }
}
