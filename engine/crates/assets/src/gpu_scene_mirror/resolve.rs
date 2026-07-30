use super::*;

/// Re-resolves one entity's complete mirrored state (instances of both sources plus both
/// punctual-light kinds) against the live scene.
pub(super) fn resolve_entity(
    shared: &mut SharedMirror,
    world_state: &mut WorldMirror,
    world: GpuSceneWorldId,
    entity: Entity,
    ctx: &mut SyncCtx<'_, '_>,
) -> Result<()> {
    let alive = ctx.scene.valid(entity);
    for source in [InstanceSource::Static, InstanceSource::Skinned] {
        resolve_instance(shared, world_state, world, entity, source, alive, ctx)?;
    }
    for kind in [MirrorLightKind::Point, MirrorLightKind::Spot] {
        let record = if alive {
            light_record(ctx.scene, entity, kind)
        } else {
            None
        };
        let key = (entity, kind);
        match (world_state.lights.get_mut(&key), record) {
            (None, None) => {}
            (Some(entry), None) => {
                ctx.target
                    .gpu_scene
                    .apply_world_delta(world, GpuSceneWorldDelta::RemoveLight(entry.handle))
                    .map_err(gpu_scene_error)?;
                world_state.lights.remove(&key);
            }
            (None, Some(record)) => {
                let result = ctx
                    .target
                    .gpu_scene
                    .apply_world_delta(world, GpuSceneWorldDelta::CreateLight(record))
                    .map_err(gpu_scene_error)?;
                let GpuSceneWorldDeltaResult::LightCreated(handle) = result else {
                    unreachable!("create light returns LightCreated");
                };
                world_state
                    .lights
                    .insert(key, LightEntry { handle, record });
            }
            (Some(entry), Some(record)) => {
                if record != entry.record {
                    ctx.target
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
        }
    }
    Ok(())
}

/// Allocates a skinned instance's stable deformation state: deformed + prev vertex
/// ranges sized to the mesh, five provider parameter words, the provider record, and
/// the scene deformation record.
fn allocate_instance_deformation(
    vertex_count: u32,
    generation: u32,
    target: &mut GpuSceneMirrorTarget<'_>,
) -> Result<DeformationEntry> {
    let (params_range, _) = target.gpu_data.deformation_parameters.allocate(5, 1)?;
    // Words 0-3 (deformed/prev first vertex, joint first/count) patch per frame with
    // the skinning plan's offsets; only the vertex count is load-time state.
    let params = vec![0, 0, 0, 0, vertex_count];
    target
        .pending
        .upload_arena(GpuArenaUploadRequest::DeformationParameters {
            range: params_range,
            data: params,
        });
    let (provider_range, _) = target.gpu_data.deformation_providers.allocate(1, 1)?;
    target
        .pending
        .upload_arena(GpuArenaUploadRequest::DeformationProviders {
            range: provider_range,
            data: vec![saffron_rendering::GpuDeformationProviderRecord {
                provider_mask: saffron_rendering::GPU_DEFORMATION_PROVIDER_SKINNING,
                first_parameter: params_range.first,
                parameter_count: 5,
                flags: 0,
            }],
        });
    let result = target
        .gpu_scene
        .apply_shared_delta(GpuSceneSharedDelta::CreateDeformation(
            saffron_rendering::GpuSceneDeformationRecord {
                provider: GpuHandle {
                    index: provider_range.first,
                    generation: 1,
                },
                source_revision: u64::from(generation),
            },
        ))
        .map_err(gpu_scene_error)?;
    let GpuSceneSharedDeltaResult::DeformationCreated(scene) = result else {
        unreachable!("create deformation returns DeformationCreated");
    };
    Ok(DeformationEntry {
        scene,
        provider_range,
        params_range,
    })
}

/// Retires a deformation allocation (reverse of [`allocate_instance_deformation`]).
pub(super) fn retire_instance_deformation(
    entry: DeformationEntry,
    target: &mut GpuSceneMirrorTarget<'_>,
) -> Result<()> {
    target
        .gpu_scene
        .apply_shared_delta(GpuSceneSharedDelta::RemoveDeformation(entry.scene))
        .map_err(gpu_scene_error)?;
    target
        .gpu_data
        .deformation_providers
        .retire(entry.provider_range)?;
    target
        .gpu_data
        .deformation_parameters
        .retire(entry.params_range)?;
    Ok(())
}

fn resolve_instance(
    shared: &mut SharedMirror,
    world_state: &mut WorldMirror,
    world: GpuSceneWorldId,
    entity: Entity,
    source: InstanceSource,
    alive: bool,
    ctx: &mut SyncCtx<'_, '_>,
) -> Result<()> {
    let key = (entity, source);
    let mesh_id = if alive {
        match source {
            InstanceSource::Static => ctx
                .scene
                .component::<MeshComponent>(entity)
                .ok()
                .map(|m| m.mesh.value()),
            InstanceSource::Skinned => ctx
                .scene
                .with_component::<SkinnedMesh, _>(entity, |skin| skin.mesh.value())
                .ok(),
        }
    } else {
        None
    };
    let state = ctx.scene.world_transform_state(entity);
    let desired = match (mesh_id, state) {
        (Some(mesh), Some(state)) => {
            match GpuSceneDynamicTransform::new(state.current, state.previous) {
                Ok(transform) => Some((mesh, state, transform)),
                Err(error) => {
                    tracing::warn!(
                        "gpu scene mirror: non-finite world transform for entity {entity:?}: {error}"
                    );
                    None
                }
            }
        }
        _ => None,
    };

    let Some((mesh_id, state, transform)) = desired else {
        world_state.unresolved.remove(&key);
        if world_state.instances.contains_key(&key) {
            remove_instance(shared, world_state, world, key, ctx.target)?;
        }
        return Ok(());
    };

    if !shared.ensure_mesh(mesh_id, ctx.assets, ctx.gpu, ctx.target)? {
        if world_state.instances.contains_key(&key) {
            remove_instance(shared, world_state, world, key, ctx.target)?;
        }
        world_state.unresolved.insert(key, mesh_id);
        return Ok(());
    }
    world_state.unresolved.remove(&key);

    let slot_count = shared.meshes[&mesh_id].slot_count;
    let slots: Vec<MaterialSlot> = ctx
        .scene
        .with_component::<MaterialSet, _>(entity, |set| set.slots.clone())
        .unwrap_or_default();
    let mut overrides: Vec<(u32, MaterialKey)> = Vec::new();
    for slot in 0..slot_count {
        let material_key = if slots.is_empty() {
            MaterialKey::default_key()
        } else {
            let index = (slot as usize).min(slots.len() - 1);
            MaterialKey::new(&slots[index])
        };
        if material_key.is_default() {
            continue;
        }
        overrides.push((slot, material_key));
    }
    let mut override_records = Vec::with_capacity(overrides.len());
    for (slot, material_key) in &overrides {
        let handle = shared.intern_material(material_key, ctx.assets, ctx.gpu, ctx.target, None)?;
        override_records.push(GpuSceneMaterialOverride {
            slot: *slot,
            material: handle,
        });
    }

    let prototype = shared.meshes[&mesh_id].prototype;
    // A skinned instance owns a stable deformation allocation, reused while its mesh
    // is unchanged and reallocated (old retired) on a mesh swap.
    let existing_deformation = world_state
        .instances
        .get(&key)
        .filter(|entry| entry.mesh == mesh_id)
        .and_then(|entry| entry.deformation);
    let deformation = if source == InstanceSource::Skinned && ctx.gpu.skinning_enabled() {
        match existing_deformation {
            Some(entry) => Some(entry),
            None => Some(allocate_instance_deformation(
                shared.meshes[&mesh_id].mesh.vertex_count,
                shared.generation,
                ctx.target,
            )?),
        }
    } else {
        None
    };
    // The entity's PlantVariant selects an assembly combination exactly like a
    // cooked point: exact (variation, phenotype) pair, else the phenotype alone,
    // else the first authored combination.
    let combination = ctx
        .scene
        .component::<PlantVariant>(entity)
        .ok()
        .and_then(|variant| {
            shared.meshes[&mesh_id]
                .mesh
                .assembly
                .as_ref()
                .map(|assembly| {
                    assembly
                        .combinations
                        .iter()
                        .position(|entry| *entry == (variant.variation, variant.phenotype))
                        .or_else(|| {
                            assembly
                                .combinations
                                .iter()
                                .position(|entry| entry.1 == variant.phenotype)
                        })
                        .unwrap_or(0) as u32
                })
        })
        .unwrap_or(0);
    let record = GpuSceneInstanceRecord {
        prototype,
        transform: GpuSceneTransform::Dynamic(transform),
        material_overrides: Arc::from(override_records),
        deformation: deformation.map(|entry| entry.scene),
        source_generation: shared.generation,
        flags: 0,
        combination,
        vegetation: None,
    };
    let facts = instance_facts(shared, mesh_id, entity, state.current, combination, ctx);
    world_state.invalidate_rays();

    if let Some(entry) = world_state.instances.get(&key) {
        let unchanged = ctx
            .target
            .gpu_scene
            .instance(world, entry.handle)
            .is_some_and(|current| *current == record);
        if unchanged {
            let entry = world_state.instances.get_mut(&key).expect("entry present");
            entry.world_revision = state.current_revision;
            entry.previous_world_revision = state.previous_revision;
            entry.facts = facts;
            return Ok(());
        }
        let handle = entry.handle;
        let old_mesh = entry.mesh;
        let old_overrides = entry.overrides.clone();
        for (_, material_key) in &overrides {
            shared.ref_material(material_key);
        }
        if old_mesh != mesh_id {
            shared.ref_mesh(mesh_id);
        }
        ctx.target
            .gpu_scene
            .apply_world_delta(world, GpuSceneWorldDelta::UpdateInstance { handle, record })
            .map_err(gpu_scene_error)?;
        for (_, material_key) in &old_overrides {
            shared.unref_material(material_key, ctx.target)?;
        }
        if old_mesh != mesh_id {
            shared.unref_mesh(old_mesh);
        }
        let entry = world_state.instances.get_mut(&key).expect("entry present");
        let old_deformation = entry.deformation;
        entry.mesh = mesh_id;
        entry.overrides = overrides;
        entry.deformation = deformation;
        entry.world_revision = state.current_revision;
        entry.previous_world_revision = state.previous_revision;
        entry.facts = facts;
        if let Some(old) = old_deformation
            && deformation.map(|entry| entry.scene) != Some(old.scene)
        {
            retire_instance_deformation(old, ctx.target)?;
        }
    } else {
        for (_, material_key) in &overrides {
            shared.ref_material(material_key);
        }
        shared.ref_mesh(mesh_id);
        let result = ctx
            .target
            .gpu_scene
            .apply_world_delta(world, GpuSceneWorldDelta::CreateInstance(record))
            .map_err(gpu_scene_error)?;
        let GpuSceneWorldDeltaResult::InstanceCreated(handle) = result else {
            unreachable!("create instance returns InstanceCreated");
        };
        world_state.instances.insert(
            key,
            InstanceEntry {
                handle,
                mesh: mesh_id,
                overrides,
                deformation,
                world_revision: state.current_revision,
                previous_world_revision: state.previous_revision,
                facts,
            },
        );
    }
    Ok(())
}

/// Derives one instance's [`InstanceFacts`] from its mesh, world transform, and the materials
/// the entity resolves — the per-frame derivation the frame driver used to repeat, done once per
/// journal touch instead.
fn instance_facts(
    shared: &SharedMirror,
    mesh_id: u64,
    entity: Entity,
    model: Mat4,
    combination: u32,
    ctx: &mut SyncCtx<'_, '_>,
) -> InstanceFacts {
    let mesh = Arc::clone(&shared.meshes[&mesh_id].mesh);
    let submeshes = mesh.submeshes.clone();
    let materials = ctx
        .assets
        .resolve_entity_materials(ctx.gpu, ctx.scene, entity, &submeshes);
    // Opacity itself lives on the structure's geometry, per submesh, from the cooked material
    // class. An entity may bind a material the cook did not see — one mesh instanced with
    // materials chosen at runtime — so the instance only overrides when the two DISAGREE.
    // Agreement is the common case and the one that matters: it leaves the geometry flags in
    // charge, which is the only way an attached micromap governs anything, since a per-instance
    // force overrides a micromap outright per spec.
    let resolved_opaque = materials
        .submeshes
        .iter()
        .all(|material| material.blend_mode == BlendMode::Opaque && material.thin_sheet.is_none());
    InstanceFacts {
        bounds: super::facts::world_bounds(&model, mesh.bounds_min, mesh.bounds_max),
        opacity_override: (resolved_opaque != mesh.cooked_opaque).then_some(resolved_opaque),
        displace: saffron_rendering::displace_info_from(&materials.submeshes),
        mesh,
        model,
        combination,
    }
}
