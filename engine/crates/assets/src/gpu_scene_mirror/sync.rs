use super::*;

impl GpuSceneMirror {
    /// Synchronizes `world` from `scene` and the shared asset state through a live
    /// renderer: the production entry point for host and player frame loops.
    ///
    /// # Errors
    ///
    /// Propagates GPU-scene delta validation, device-table, and resolution failures.
    pub fn sync_renderer_world(
        &mut self,
        world: GpuSceneWorldId,
        scene: &mut Scene,
        vegetation: Option<&VegetationWorld>,
        assets: &mut AssetServer,
        renderer: &mut Renderer,
        uploader: &Uploader,
    ) -> Result<bool> {
        let descriptors = renderer.descriptors_arc();
        let skinning = renderer.skinning_enabled();
        let default_white = Arc::clone(renderer.default_white());
        let gpu = RendererUploader::new(uploader, &descriptors, skinning);
        let view = renderer.page_demand_view();
        let flip_stamp = renderer.frame_serial() as u32;
        let (gpu_data, gpu_scene, pending, residency) = renderer.gpu_scene_parts_mut();
        let mut target = GpuSceneMirrorTarget {
            gpu_data,
            gpu_scene,
            default_white: &default_white,
            pending,
            residency,
        };
        self.sync_world(world, scene, assets, &gpu, &mut target)?;
        let mut vegetation_mutated = false;
        if let Some(vegetation) = vegetation {
            let calendar = &scene.environment.time_of_day;
            let season_mille = saffron_vegetation::season_phase_mille(
                calendar.year,
                calendar.month,
                calendar.day,
                calendar.latitude,
            );
            vegetation_mutated = self.sync_vegetation(
                world,
                vegetation,
                assets,
                &gpu,
                season_mille,
                flip_stamp,
                &mut target,
            )?;
        }
        self.drive_page_streaming(world, scene, Some(view), &mut target)?;
        let bins = self.live_executor_bins(target.gpu_data);
        let GpuSceneMirrorTarget { .. } = target;
        // Published every frame, complete and empty when nothing is over: an owned alarm
        // resolves by absence, so publishing only on breach would leave one firing forever.
        renderer.set_owned_budgets(self.vegetation_budget_breaches(assets));
        renderer.set_live_executor_bins(bins);
        renderer.set_micro_field_directory(self.micro_field_directory(world));
        renderer.set_live_draw_record_bound(self.live_draw_record_bound());
        Ok(vegetation_mutated)
    }

    /// The world's resident micro-field tile directory (fields-arena byte offset +
    /// entry count), or `None` while no field tiles are resident.
    #[must_use]
    pub fn micro_field_directory(&self, world: GpuSceneWorldId) -> Option<(u32, u32)> {
        self.worlds
            .get(&world.0)
            .and_then(|state| state.field_directory)
            .map(|(range, count)| (range.first, count))
    }

    /// Pushes the frame's deformation-provider parameter patches: each skinned
    /// instance's palette and deformed offsets from the submitted draw list land in
    /// its stable provider params. Unknown entities (not yet mirrored this frame) are
    /// skipped; they patch on a later frame once mirrored.
    pub fn patch_frame_deformations(
        &self,
        world: GpuSceneWorldId,
        scene: &Scene,
        deformations: &[saffron_rendering::SkinnedDeformation],
        pending: &mut GpuScenePendingUploads,
    ) -> Vec<saffron_rendering::GpuSceneInstanceHandle> {
        let Some(world_state) = self.worlds.get(&world.0) else {
            return Vec::new();
        };
        let mut deformed = Vec::with_capacity(deformations.len());
        for deformation in deformations {
            let Some(entry) = scene
                .find_entity_by_uuid(Uuid(deformation.entity))
                .and_then(|entity| {
                    world_state
                        .instances
                        .get(&(entity, InstanceSource::Skinned))
                })
            else {
                continue;
            };
            let Some(allocation) = entry.deformation else {
                continue;
            };
            pending.upload_arena(GpuArenaUploadRequest::DeformationParameters {
                range: allocation.params_range,
                data: vec![
                    deformation.deformed_offset,
                    deformation.deformed_offset,
                    deformation.joint_offset,
                    deformation.joint_count,
                    deformation.vertex_count,
                ],
            });
            deformed.push(entry.handle);
        }
        deformed
    }

    /// Every live (executor shader index, material class bits) pair across the
    /// interned materials — the populated executor bins the draw sites iterate.
    #[must_use]
    pub fn live_executor_bins(&self, gpu_data: &GlobalGpuData) -> Vec<(u32, u32)> {
        let mut bins: Vec<(u32, u32)> = self
            .shared
            .materials
            .values()
            .filter_map(|entry| gpu_data.materials.get(entry.device.table))
            .map(|record| (record.shader_index, record.material_class.bits()))
            .collect();
        bins.sort_unstable();
        bins.dedup();
        bins
    }
    /// Synchronizes `world` from `scene` and the shared asset state.
    ///
    /// Reads both journals from the retained cursors, resolves every touched entity and
    /// invalidated asset, and applies only the resulting deltas. Journal overflow, a
    /// replaced catalog, or a rebound scene instance triggers the matching complete
    /// rebuild.
    ///
    /// # Errors
    ///
    /// Propagates GPU-scene delta validation, device-table, and resolution failures.
    pub fn sync_world(
        &mut self,
        world: GpuSceneWorldId,
        scene: &mut Scene,
        assets: &mut AssetServer,
        gpu: &dyn GpuUploader,
        target: &mut GpuSceneMirrorTarget<'_>,
    ) -> Result<()> {
        scene.update_world_transforms();
        self.consume_asset_journal(assets, gpu, target)?;

        let world_state = self.worlds.entry(world.0).or_default();
        let mut rebuild = world_state.scene_instance != scene.instance_id();
        let skinning = gpu.skinning_enabled();
        if world_state.skinning != Some(skinning) {
            world_state.skinning = Some(skinning);
            if !rebuild {
                scene.for_each::<&SkinnedMesh, _>(|entity, _| {
                    world_state.dirty.insert(entity);
                });
            }
        }
        if !rebuild {
            match scene.read_journal(world_state.cursor) {
                SceneJournalRead::Delta { mutations, next } => {
                    world_state.cursor = next;
                    for mutation in mutations {
                        match classify_mutation(mutation.kind) {
                            Some(Touch::Content) => {
                                world_state.dirty.insert(mutation.entity);
                            }
                            Some(Touch::Transform)
                                if !world_state.dirty.contains(&mutation.entity) =>
                            {
                                apply_transform_update(
                                    world_state,
                                    world,
                                    mutation.entity,
                                    scene,
                                    target,
                                )?;
                            }
                            _ => {}
                        }
                    }
                }
                SceneJournalRead::SnapshotRequired { next } => {
                    world_state.cursor = next;
                    rebuild = true;
                }
            }
        }

        if rebuild {
            self.rebuild_world(world, scene, assets, gpu, target)?;
            return Ok(());
        }

        let world_state = self.worlds.get_mut(&world.0).expect("world synced");
        let dirty = std::mem::take(&mut world_state.dirty);
        let mut ctx = SyncCtx {
            scene,
            assets,
            gpu,
            target,
        };
        for entity in dirty {
            resolve_entity(&mut self.shared, world_state, world, entity, &mut ctx)?;
        }
        Ok(())
    }
    fn consume_asset_journal(
        &mut self,
        assets: &mut AssetServer,
        gpu: &dyn GpuUploader,
        target: &mut GpuSceneMirrorTarget<'_>,
    ) -> Result<()> {
        let Some(cursor) = self.asset_cursor else {
            self.asset_cursor = Some(assets.asset_journal_cursor());
            return Ok(());
        };
        let mut rebuild = false;
        let mut refresh_materials = false;
        let mut invalidated_meshes: HashSet<u64> = HashSet::new();
        let mut dropped_meshes: HashSet<u64> = HashSet::new();
        match assets.read_asset_journal(cursor) {
            AssetJournalRead::Delta { mutations, next } => {
                self.asset_cursor = Some(next);
                for mutation in mutations {
                    match mutation.target {
                        AssetMutationTarget::All => rebuild = true,
                        AssetMutationTarget::Asset { id, .. } => {
                            if mutation
                                .invalidations
                                .contains(AssetInvalidations::MATERIAL)
                                || mutation.invalidations.contains(AssetInvalidations::TEXTURE)
                            {
                                refresh_materials = true;
                            }
                            if mutation
                                .invalidations
                                .contains(AssetInvalidations::PROTOTYPE)
                                || mutation.invalidations.contains(AssetInvalidations::PAGE)
                            {
                                if mutation.kind == AssetMutationKind::Deleted {
                                    dropped_meshes.insert(id.value());
                                } else {
                                    invalidated_meshes.insert(id.value());
                                }
                            }
                        }
                    }
                }
            }
            AssetJournalRead::SnapshotRequired { next } => {
                self.asset_cursor = Some(next);
                rebuild = true;
            }
        }

        if rebuild {
            self.rebuild_shared(assets, target)?;
            return Ok(());
        }
        if refresh_materials {
            self.refresh_all_materials(assets, gpu, target)?;
            // An instance's ray-tracing opacity class and its displacement inputs are DERIVED
            // from its resolved materials, not stored in the material records, so refreshing
            // the records alone leaves them describing a class the entity no longer has — a
            // leaf card that becomes masked at runtime would keep casting a solid ray shadow.
            // Re-resolving the instance re-derives both.
            for world_state in self.worlds.values_mut() {
                let touched: Vec<Entity> = world_state
                    .instances
                    .keys()
                    .map(|(entity, _)| *entity)
                    .collect();
                world_state.dirty.extend(touched);
            }
        }
        for id in dropped_meshes {
            self.drop_mesh(id, target)?;
        }
        for id in invalidated_meshes {
            self.refresh_mesh(id, assets, gpu, target)?;
        }
        Ok(())
    }

    /// Tears down every mirrored record and forces each world to rebuild from live state
    /// at its next sync.
    fn rebuild_shared(
        &mut self,
        assets: &mut AssetServer,
        target: &mut GpuSceneMirrorTarget<'_>,
    ) -> Result<()> {
        for (id, world_state) in &mut self.worlds {
            let world = GpuSceneWorldId(*id);
            for entry in world_state.instances.values() {
                target
                    .gpu_scene
                    .apply_world_delta(world, GpuSceneWorldDelta::RemoveInstance(entry.handle))
                    .map_err(gpu_scene_error)?;
            }
            for entry in world_state.plants.values() {
                target
                    .gpu_scene
                    .apply_world_delta(world, GpuSceneWorldDelta::RemoveInstance(entry.handle))
                    .map_err(gpu_scene_error)?;
            }
            for entry in world_state.plant_fields.values() {
                target.gpu_data.fields.retire(entry.range)?;
            }
            if let Some((range, _)) = world_state.field_directory.take() {
                target.gpu_data.fields.retire(range)?;
            }
            for handle in world_state.field_instances.values() {
                target
                    .gpu_scene
                    .apply_world_delta(world, GpuSceneWorldDelta::RemoveInstance(*handle))
                    .map_err(gpu_scene_error)?;
            }
            for entry in world_state.lights.values() {
                target
                    .gpu_scene
                    .apply_world_delta(world, GpuSceneWorldDelta::RemoveLight(entry.handle))
                    .map_err(gpu_scene_error)?;
            }
            *world_state = WorldMirror::default();
        }
        let meshes: Vec<u64> = self.shared.meshes.keys().copied().collect();
        for id in meshes {
            self.shared.remove_mesh_records(id, target)?;
        }
        let materials: Vec<MaterialKey> = self.shared.materials.keys().cloned().collect();
        for key in materials {
            self.shared.remove_material_records(&key, target)?;
        }
        debug_assert!(self.shared.textures.is_empty());
        self.shared.generation = self.shared.generation.wrapping_add(1);
        self.asset_cursor = Some(assets.asset_journal_cursor());
        self.shared_rebuilds += 1;
        Ok(())
    }

    /// Re-resolves every interned material variant in place; prototype and instance
    /// references stay valid because the persistent-scene handles are stable.
    fn refresh_all_materials(
        &mut self,
        assets: &mut AssetServer,
        gpu: &dyn GpuUploader,
        target: &mut GpuSceneMirrorTarget<'_>,
    ) -> Result<()> {
        let keys: Vec<MaterialKey> = self.shared.materials.keys().cloned().collect();
        for key in keys {
            self.shared
                .refresh_material_content(&key, assets, gpu, target)?;
        }
        Ok(())
    }

    /// Removes every instance of a deleted mesh, then the mesh's shared records; the
    /// affected entities become unresolved and re-resolve if the asset returns.
    fn drop_mesh(&mut self, id: u64, target: &mut GpuSceneMirrorTarget<'_>) -> Result<()> {
        if !self.shared.meshes.contains_key(&id) {
            return Ok(());
        }
        for (world_id, world_state) in &mut self.worlds {
            let world = GpuSceneWorldId(*world_id);
            let affected: Vec<(Entity, InstanceSource)> = world_state
                .instances
                .iter()
                .filter(|(_, entry)| entry.mesh == id)
                .map(|(key, _)| *key)
                .collect();
            for key in affected {
                remove_instance(&mut self.shared, world_state, world, key, target)?;
                world_state.unresolved.insert(key, id);
            }
            let affected_cells: HashSet<WorldCellKey> = world_state
                .plants
                .iter()
                .filter(|(_, entry)| entry.mesh == id)
                .map(|((cell, _), _)| *cell)
                .collect();
            for cell in affected_cells {
                remove_cell_plants(&mut self.shared, world_state, world, cell, target)?;
                // Dropping the generation marker re-translates the cell at the next
                // vegetation sync, so its plants recreate if the family returns.
                world_state.plant_cells.remove(&cell);
            }
        }
        self.shared.remove_mesh_records(id, target)
    }

    /// Reloads an invalidated mesh and swaps its geometry, page, and prototype content in
    /// place; entities using it re-resolve at their world's next sync.
    fn refresh_mesh(
        &mut self,
        id: u64,
        assets: &mut AssetServer,
        gpu: &dyn GpuUploader,
        target: &mut GpuSceneMirrorTarget<'_>,
    ) -> Result<()> {
        for world_state in self.worlds.values_mut() {
            for ((entity, _), mesh_id) in &world_state.unresolved {
                if *mesh_id == id {
                    world_state.dirty.insert(*entity);
                }
            }
            for ((entity, _), entry) in &world_state.instances {
                if entry.mesh == id {
                    world_state.dirty.insert(*entity);
                }
            }
            let refreshed_cells: Vec<WorldCellKey> = world_state
                .plants
                .iter()
                .filter(|(_, entry)| entry.mesh == id)
                .map(|((cell, _), _)| *cell)
                .collect();
            for cell in refreshed_cells {
                world_state.plant_cells.remove(&cell);
            }
        }
        if !self.shared.meshes.contains_key(&id) {
            return Ok(());
        }
        let reloaded = assets
            .load_mesh_asset(gpu, Uuid(id))
            .filter(|mesh| !mesh.hierarchy_pages.is_empty());
        match reloaded {
            Some(mesh) => {
                let payload_source = assets.page_payload_source(Uuid(id));
                let mechanics = packed_mechanics(assets.plant_family_mechanics(Uuid(id)));
                self.shared
                    .replace_mesh_content(id, mesh, payload_source, mechanics, target)
            }
            None => self.drop_mesh(id, target),
        }
    }

    /// Rebuilds one world from the live scene: teardown of every mirrored entry, then a
    /// complete re-resolve of every render-relevant entity.
    fn rebuild_world(
        &mut self,
        world: GpuSceneWorldId,
        scene: &mut Scene,
        assets: &mut AssetServer,
        gpu: &dyn GpuUploader,
        target: &mut GpuSceneMirrorTarget<'_>,
    ) -> Result<()> {
        let world_state = self.worlds.get_mut(&world.0).expect("world synced");
        let existing: Vec<(Entity, InstanceSource)> =
            world_state.instances.keys().copied().collect();
        for key in existing {
            remove_instance(&mut self.shared, world_state, world, key, target)?;
        }
        for entry in world_state.lights.values() {
            target
                .gpu_scene
                .apply_world_delta(world, GpuSceneWorldDelta::RemoveLight(entry.handle))
                .map_err(gpu_scene_error)?;
        }
        world_state.lights.clear();
        world_state.unresolved.clear();
        world_state.dirty.clear();
        world_state.scene_instance = scene.instance_id();
        world_state.cursor = scene.journal_cursor();

        let mut targets: HashSet<Entity> = HashSet::new();
        scene.for_each::<&MeshComponent, _>(|entity, _| {
            targets.insert(entity);
        });
        scene.for_each::<&SkinnedMesh, _>(|entity, _| {
            targets.insert(entity);
        });
        scene.for_each::<&PointLight, _>(|entity, _| {
            targets.insert(entity);
        });
        scene.for_each::<&SpotLight, _>(|entity, _| {
            targets.insert(entity);
        });

        let mut ctx = SyncCtx {
            scene,
            assets,
            gpu,
            target,
        };
        for entity in targets {
            resolve_entity(&mut self.shared, world_state, world, entity, &mut ctx)?;
        }
        self.world_rebuilds += 1;
        Ok(())
    }
}

/// Applies a transform-only update to the entity's mirrored instances and lights.
fn apply_transform_update(
    world_state: &mut WorldMirror,
    world: GpuSceneWorldId,
    entity: Entity,
    scene: &Scene,
    target: &mut GpuSceneMirrorTarget<'_>,
) -> Result<()> {
    let Some(state) = scene.world_transform_state(entity) else {
        return Ok(());
    };
    let mut moved = false;
    for source in [InstanceSource::Static, InstanceSource::Skinned] {
        let Some(entry) = world_state.instances.get_mut(&(entity, source)) else {
            continue;
        };
        if entry.world_revision == state.current_revision
            && entry.previous_world_revision == state.previous_revision
        {
            continue;
        }
        let transform = match GpuSceneDynamicTransform::new(state.current, state.previous) {
            Ok(transform) => transform,
            Err(error) => {
                tracing::warn!(
                    "gpu scene mirror: non-finite world transform for entity {entity:?}: {error}"
                );
                continue;
            }
        };
        let Some(record) = target.gpu_scene.instance(world, entry.handle).cloned() else {
            continue;
        };
        let mut record = record;
        record.transform = GpuSceneTransform::Dynamic(transform);
        target
            .gpu_scene
            .apply_world_delta(
                world,
                GpuSceneWorldDelta::UpdateInstance {
                    handle: entry.handle,
                    record,
                },
            )
            .map_err(gpu_scene_error)?;
        entry.world_revision = state.current_revision;
        entry.previous_world_revision = state.previous_revision;
        entry.facts.model = state.current;
        entry.facts.bounds = super::facts::world_bounds(
            &state.current,
            entry.facts.mesh.bounds_min,
            entry.facts.mesh.bounds_max,
        );
        moved = true;
    }
    if moved {
        world_state.invalidate_rays();
    }
    for kind in [MirrorLightKind::Point, MirrorLightKind::Spot] {
        let Some(entry) = world_state.lights.get_mut(&(entity, kind)) else {
            continue;
        };
        let Some(record) = light_record(scene, entity, kind) else {
            continue;
        };
        if record != entry.record {
            target
                .gpu_scene
                .apply_world_delta(
                    world,
                    GpuSceneWorldDelta::UpdateLight {
                        handle: entry.handle,
                        record,
                    },
                )
                .map_err(gpu_scene_error)?;
            entry.record = record;
        }
    }
    Ok(())
}

/// Builds the current punctual-light record for `entity`, or `None` when the component
/// or a finite world transform is absent.
pub(super) fn light_record(
    scene: &Scene,
    entity: Entity,
    kind: MirrorLightKind,
) -> Option<GpuSceneLightRecord> {
    let state = scene.world_transform_state(entity)?;
    let position = state.current.w_axis.truncate();
    let light = match kind {
        MirrorLightKind::Point => {
            let light = scene.component::<PointLight>(entity).ok()?;
            gpu_point_light(&light, position)
        }
        MirrorLightKind::Spot => {
            let light = scene.component::<SpotLight>(entity).ok()?;
            let direction = (scene.world_rotation(entity) * light.direction).normalize();
            gpu_spot_light(&light, position, direction)
        }
    };
    Some(GpuSceneLightRecord {
        light,
        source_revision: state.current_revision.get(),
    })
}

pub(super) fn remove_instance(
    shared: &mut SharedMirror,
    world_state: &mut WorldMirror,
    world: GpuSceneWorldId,
    key: (Entity, InstanceSource),
    target: &mut GpuSceneMirrorTarget<'_>,
) -> Result<()> {
    let Some(entry) = world_state.instances.remove(&key) else {
        return Ok(());
    };
    world_state.invalidate_rays();
    if key.1 == InstanceSource::Static {
        world_state.track_displacement(key.0, false);
    }
    target
        .gpu_scene
        .apply_world_delta(world, GpuSceneWorldDelta::RemoveInstance(entry.handle))
        .map_err(gpu_scene_error)?;
    if let Some(deformation) = entry.deformation {
        retire_instance_deformation(deformation, target)?;
    }
    for (_, material_key) in &entry.overrides {
        shared.unref_material(material_key, target)?;
    }
    shared.unref_mesh(entry.mesh);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::*;
    use glam::Vec3;
    use saffron_rendering::validation_issue_count;
    use saffron_scene::Transform;

    #[test]
    fn mirror_populates_instances_lights_and_shared_records() {
        let Some(mut harness) = harness("populate") else {
            return;
        };
        let before = validation_issue_count();
        let mesh_id = Uuid(9001);
        write_triangle_mesh(&mut harness.assets, mesh_id, "tri");

        let mut scene = Scene::new();
        let a = scene.create_entity("A");
        scene
            .add_component(a, MeshComponent { mesh: mesh_id })
            .unwrap();
        let b = scene.create_entity("B");
        scene
            .add_component(b, MeshComponent { mesh: mesh_id })
            .unwrap();
        let light = scene.create_entity("Light");
        scene.add_component(light, PointLight::default()).unwrap();
        let spot = scene.create_entity("Spot");
        scene.add_component(spot, SpotLight::default()).unwrap();

        harness.sync(&mut scene);

        let stats = harness.mirror.stats();
        assert_eq!(stats.meshes, 1, "one prototype for the shared mesh");
        assert_eq!(stats.instances, 2);
        assert_eq!(stats.lights, 2);
        assert_eq!(stats.materials, 1, "only the default material variant");
        assert_eq!(harness.gpu_scene.instances(WORLD).unwrap().count(), 2);
        assert_eq!(harness.gpu_scene.lights(WORLD).unwrap().count(), 2);
        assert_eq!(harness.gpu_scene.prototypes().count(), 1);
        assert!(harness.gpu_scene.pages().count() >= 1);
        assert_eq!(
            stats.world_rebuilds, 1,
            "first sync rebuilds from live state"
        );

        let revision = harness.gpu_scene.revision();
        harness.sync(&mut scene);
        assert_eq!(
            harness.gpu_scene.revision(),
            revision,
            "an unchanged scene applies no deltas"
        );

        harness.finish();
        assert_eq!(validation_issue_count(), before);
    }

    #[test]
    fn transform_change_updates_only_the_moved_instance() {
        let Some(mut harness) = harness("move") else {
            return;
        };
        let before = validation_issue_count();
        let mesh_id = Uuid(9002);
        write_triangle_mesh(&mut harness.assets, mesh_id, "tri");

        let mut scene = Scene::new();
        let a = scene.create_entity("A");
        scene
            .add_component(a, MeshComponent { mesh: mesh_id })
            .unwrap();
        let b = scene.create_entity("B");
        scene
            .add_component(b, MeshComponent { mesh: mesh_id })
            .unwrap();
        harness.sync(&mut scene);

        let moved_handle = {
            let entry = &harness.mirror.worlds[&WORLD.0].instances[&(a, InstanceSource::Static)];
            entry.handle
        };
        let old_transform = match &harness
            .gpu_scene
            .instance(WORLD, moved_handle)
            .expect("instance resident")
            .transform
        {
            GpuSceneTransform::Dynamic(dynamic) => *dynamic,
            GpuSceneTransform::Static(_) => panic!("scene entities mirror dynamically"),
        };

        let revision = harness.gpu_scene.revision();
        scene
            .with_component_mut::<Transform, _>(a, |t| t.translation = Vec3::new(3.0, 0.0, 0.0))
            .unwrap();
        harness.sync(&mut scene);

        assert_eq!(
            harness.gpu_scene.revision(),
            revision + 1,
            "exactly one instance record re-uploads"
        );
        let updated = match &harness
            .gpu_scene
            .instance(WORLD, moved_handle)
            .expect("instance resident")
            .transform
        {
            GpuSceneTransform::Dynamic(dynamic) => *dynamic,
            GpuSceneTransform::Static(_) => panic!("scene entities mirror dynamically"),
        };
        assert_eq!(
            updated.previous, old_transform.current,
            "the previous transform is the prior current"
        );
        assert_ne!(updated.current, old_transform.current);

        harness.finish();
        assert_eq!(validation_issue_count(), before);
    }

    #[test]
    fn destroy_and_material_overrides_release_shared_records() {
        let Some(mut harness) = harness("release") else {
            return;
        };
        let before = validation_issue_count();
        let mesh_id = Uuid(9003);
        write_triangle_mesh(&mut harness.assets, mesh_id, "tri");

        let mut scene = Scene::new();
        let a = scene.create_entity("A");
        scene
            .add_component(a, MeshComponent { mesh: mesh_id })
            .unwrap();
        let mut overrides = saffron_json::Map::new();
        overrides.insert(
            "baseColor".to_owned(),
            saffron_json::parse_json("[1.0, 0.0, 0.0, 1.0]").unwrap(),
        );
        scene
            .add_component(
                a,
                MaterialSet {
                    slots: vec![MaterialSlot {
                        material: Uuid(0),
                        overrides: saffron_json::Value::Object(overrides),
                    }],
                },
            )
            .unwrap();
        harness.sync(&mut scene);

        assert_eq!(
            harness.mirror.stats().materials,
            2,
            "the default variant plus the overridden variant"
        );
        let instance = harness
            .gpu_scene
            .instances(WORLD)
            .unwrap()
            .next()
            .map(|(_, record)| record.clone())
            .expect("instance resident");
        assert_eq!(instance.material_overrides.len(), 1);

        scene.add_component(a, MaterialSet::default()).unwrap();
        harness.sync(&mut scene);
        assert_eq!(
            harness.mirror.stats().materials,
            1,
            "clearing the override frees the variant"
        );

        scene.destroy_entity(a);
        harness.sync(&mut scene);
        assert_eq!(harness.mirror.stats().instances, 0);
        assert_eq!(harness.gpu_scene.instances(WORLD).unwrap().count(), 0);

        harness.finish();
        assert_eq!(validation_issue_count(), before);
    }

    #[test]
    fn rebinding_a_different_scene_instance_rebuilds_the_world() {
        let Some(mut harness) = harness("rebind") else {
            return;
        };
        let before = validation_issue_count();
        let mesh_id = Uuid(9004);
        write_triangle_mesh(&mut harness.assets, mesh_id, "tri");

        let mut authored = Scene::new();
        let a = authored.create_entity("A");
        authored
            .add_component(a, MeshComponent { mesh: mesh_id })
            .unwrap();
        harness.sync(&mut authored);
        assert_eq!(harness.mirror.stats().instances, 1);

        let mut play = Scene::new();
        for name in ["P1", "P2"] {
            let entity = play.create_entity(name);
            play.add_component(entity, MeshComponent { mesh: mesh_id })
                .unwrap();
        }
        harness.sync(&mut play);
        assert_eq!(
            harness.mirror.stats().instances,
            2,
            "the play duplicate replaces the authored world's instances"
        );
        assert_eq!(harness.gpu_scene.instances(WORLD).unwrap().count(), 2);
        assert_eq!(harness.mirror.stats().world_rebuilds, 2);

        harness.sync(&mut authored);
        assert_eq!(harness.mirror.stats().instances, 1);
        assert_eq!(harness.mirror.stats().world_rebuilds, 3);

        harness.finish();
        assert_eq!(validation_issue_count(), before);
    }

    #[test]
    fn missing_mesh_is_unresolved_until_it_appears() {
        let Some(mut harness) = harness("unresolved") else {
            return;
        };
        let before = validation_issue_count();
        let mesh_id = Uuid(9005);

        let mut scene = Scene::new();
        let a = scene.create_entity("A");
        scene
            .add_component(a, MeshComponent { mesh: mesh_id })
            .unwrap();
        harness.sync(&mut scene);
        assert_eq!(harness.mirror.stats().instances, 0);
        assert_eq!(harness.mirror.stats().unresolved_instances, 1);

        write_triangle_mesh(&mut harness.assets, mesh_id, "tri");
        let entry = harness
            .assets
            .catalog
            .remove(mesh_id)
            .expect("catalog row present");
        harness.assets.register_imported_asset(entry);
        harness.sync(&mut scene);
        assert_eq!(harness.mirror.stats().instances, 1);
        assert_eq!(harness.mirror.stats().unresolved_instances, 0);

        harness.finish();
        assert_eq!(validation_issue_count(), before);
    }
}
