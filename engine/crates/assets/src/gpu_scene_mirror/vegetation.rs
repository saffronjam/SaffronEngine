use super::*;

impl GpuSceneMirror {
    /// Translates the authoritative macro-plant snapshot into persistent-scene instance
    /// deltas: cells diff purely by published generation id, and a changed cell's
    /// instances remove + recreate in one pass, keyed `(cell, PlantId)`. Families
    /// resolve through the manifest's exact `.splantc` identity into the shared mesh
    /// path, so a plant prototype is a mesh prototype. Returns whether any cell
    /// translated or retired — the caller's repaint signal.
    #[allow(clippy::too_many_arguments)]
    pub fn sync_vegetation(
        &mut self,
        world: GpuSceneWorldId,
        vegetation: &VegetationWorld,
        assets: &mut AssetServer,
        gpu: &dyn GpuUploader,
        season_mille: u16,
        flip_stamp: u32,
        target: &mut GpuSceneMirrorTarget<'_>,
    ) -> Result<bool> {
        let mut mutated = false;
        let shared = &mut self.shared;
        let world_state = self.worlds.entry(world.0).or_default();
        let families: HashMap<u64, ContentHash> = vegetation
            .manifest()
            .plants
            .iter()
            .map(|plant| (plant.family.value(), plant.artifact_hash))
            .collect();

        let resident: Vec<_> = vegetation.resident_cells().collect();
        let resident_keys: HashSet<WorldCellKey> = resident.iter().map(|(cell, _)| *cell).collect();
        let stale: Vec<WorldCellKey> = world_state
            .plant_cells
            .keys()
            .filter(|cell| !resident_keys.contains(cell))
            .copied()
            .collect();
        for cell in stale {
            remove_cell_plants(shared, world_state, world, cell, target)?;
            remove_cell_fields(world_state, cell, target)?;
            world_state.plant_cells.remove(&cell);
            mutated = true;
        }

        for (cell, generation) in &resident {
            let cell = *cell;
            let current = (
                generation.id().generation,
                vegetation.cell_bulk_revision(cell),
            );
            if world_state.plant_cells.get(&cell) == Some(&current) {
                continue;
            }
            remove_cell_plants(shared, world_state, world, cell, target)?;
            remove_cell_fields(world_state, cell, target)?;
            mutated = true;
            if let Some(tiles) = generation.micro_fields().filter(|tiles| !tiles.is_empty()) {
                let (packed, tile_slots) = pack_field_tiles(tiles)?;
                let byte_len = u32::try_from(packed.len())
                    .map_err(|_| mirror_error("micro field tiles exceed the fields arena"))?;
                let (range, _) = target.gpu_data.fields.allocate(byte_len, 16)?;
                target.pending.upload_arena(GpuArenaUploadRequest::Fields {
                    range,
                    data: packed,
                });
                world_state.plant_fields.insert(
                    cell,
                    CellFieldEntry {
                        range,
                        tiles: tile_slots,
                    },
                );
            }
            let points = generation.macro_points();
            for index in 0..points.ids.len() {
                if matches!(
                    points.lifecycles[index],
                    PlantLifecycle::Seed | PlantLifecycle::Removed
                ) {
                    continue;
                }
                // A promoted plant renders as its entity view instead, so the bulk instance
                // stays absent for exactly as long as that entity owns it.
                if vegetation.is_bulk_suppressed(points.ids[index]) {
                    continue;
                }
                let family = points.families[index];
                let Some(artifact) = families.get(&family.value()) else {
                    tracing::warn!(
                        "gpu scene mirror: plant family {family:?} is not in the manifest"
                    );
                    continue;
                };
                let Some(render) = assets.load_plant_family(gpu, family, *artifact) else {
                    continue;
                };
                if !shared.ensure_mesh(family.value(), assets, gpu, target)? {
                    continue;
                }
                let slot_count = shared.meshes[&family.value()].slot_count;
                // A family that cooked an atlas addresses it from its UVs, so every slot of every
                // point in this family reads the atlas rather than the slot's own image.
                let atlas = render.atlas.clone();
                // The rendered phenotype derives from typed lifecycle state and the
                // seasonal phase — never inferred from the active mesh.
                let rendered_phenotype = saffron_vegetation::resolve_rendered_phenotype(
                    render
                        .phenotypes
                        .iter()
                        .map(|row| (row.id, row.role, row.season_window)),
                    points.phenotypes[index],
                    points.lifecycles[index],
                    season_mille,
                );
                // The rendered phenotype remaps material slots before slot resolution.
                let phenotype_remap = render
                    .phenotypes
                    .iter()
                    .find(|phenotype| phenotype.id == rendered_phenotype)
                    .map(|phenotype| Arc::clone(&phenotype.material_remap));
                let mut overrides: Vec<(u32, MaterialKey)> = Vec::new();
                for slot in 0..slot_count {
                    let resolved_slot = phenotype_remap
                        .as_ref()
                        .and_then(|remap| {
                            remap
                                .iter()
                                .find(|(from, _)| *from == slot)
                                .map(|(_, to)| *to)
                        })
                        .unwrap_or(slot);
                    let Some(material) = render.materials.get(resolved_slot as usize) else {
                        continue;
                    };
                    if material.value() == 0 {
                        continue;
                    }
                    overrides.push((
                        slot,
                        atlas.as_ref().map_or_else(
                            || MaterialKey::from_material(*material),
                            |_| MaterialKey::from_atlased_material(*material, family),
                        ),
                    ));
                }
                let mut override_records = Vec::with_capacity(overrides.len());
                for (slot, material_key) in &overrides {
                    let handle = shared.intern_material(
                        material_key,
                        assets,
                        gpu,
                        target,
                        atlas.as_ref(),
                    )?;
                    override_records.push(GpuSceneMaterialOverride {
                        slot: *slot,
                        material: handle,
                    });
                }
                // The point's (variation, rendered phenotype) resolves to the
                // family's combination-mask index — an exact pair match wins, else
                // the phenotype alone, else the first authored combination.
                let combination = render
                    .combinations
                    .iter()
                    .position(|entry| *entry == (points.variations[index], rendered_phenotype))
                    .or_else(|| {
                        render
                            .combinations
                            .iter()
                            .position(|entry| entry.1 == rendered_phenotype)
                    })
                    .unwrap_or(0) as u32;
                // The point's conservative world bounds become an instance-local
                // pre-scale sphere the cull composes through the transform; previous
                // equals current at rest (deformation drift writes the delta).
                let bounds = plant_bounds_sphere(
                    points.positions[index],
                    points.orientations[index],
                    points.scales[index],
                    points.bounds[index],
                );
                let attachment = points.attachments[index].as_ref().map(|attachment| {
                    GpuSceneAttachmentColumns {
                        provider: attachment.provider.0,
                        primitive: attachment.primitive.0,
                        barycentric: attachment.barycentric.map(|value| value.bits()),
                    }
                });
                let flags = ((points.interaction_policies[index] as u32)
                    << GPU_SCENE_INSTANCE_POLICY_SHIFT)
                    | GPU_SCENE_INSTANCE_FLAG_EXPLICIT_BOUNDS
                    | GPU_SCENE_INSTANCE_FLAG_WIND
                    | if attachment.is_some() {
                        GPU_SCENE_INSTANCE_FLAG_ATTACHED
                    } else {
                        0
                    };
                let placement = GpuSceneStaticTransform::new(
                    points.positions[index],
                    points.orientations[index],
                    points.scales[index],
                    0,
                );
                let record = GpuSceneInstanceRecord {
                    prototype: shared.meshes[&family.value()].prototype,
                    transform: GpuSceneTransform::Static(placement),
                    material_overrides: Arc::from(override_records),
                    deformation: None,
                    source_generation: shared.generation,
                    flags,
                    combination,
                    vegetation: Some(GpuSceneVegetationColumns {
                        bounds_current: bounds,
                        bounds_previous: bounds,
                        attachment,
                        combination_previous: combination,
                        flip_stamp: 0,
                    }),
                };
                for (_, material_key) in &overrides {
                    shared.ref_material(material_key);
                }
                shared.ref_mesh(family.value());
                let result = target
                    .gpu_scene
                    .apply_world_delta(world, GpuSceneWorldDelta::CreateInstance(record.clone()))
                    .map_err(gpu_scene_error)?;
                let GpuSceneWorldDeltaResult::InstanceCreated(handle) = result else {
                    unreachable!("create instance returns InstanceCreated");
                };
                world_state.plants.insert(
                    (cell, points.ids[index]),
                    PlantInstanceEntry {
                        handle,
                        mesh: family.value(),
                        overrides,
                        record,
                    },
                );
            }
            world_state.plant_cells.insert(cell, current);
        }
        // A seasonal phase change flips combinations in place: every live plant whose
        // resolved combination moved gets one UpdateInstance carrying the previous
        // combination and the flip stamp, so the traversal crossfades the assembly uses
        // instead of popping.
        if world_state.season_mille != season_mille {
            for (cell, generation) in &resident {
                let cell = *cell;
                let points = generation.macro_points();
                for index in 0..points.ids.len() {
                    let Some(entry) = world_state.plants.get_mut(&(cell, points.ids[index])) else {
                        continue;
                    };
                    let family = points.families[index];
                    let Some(artifact) = families.get(&family.value()) else {
                        continue;
                    };
                    let Some(render) = assets.load_plant_family(gpu, family, *artifact) else {
                        continue;
                    };
                    let rendered_phenotype = saffron_vegetation::resolve_rendered_phenotype(
                        render
                            .phenotypes
                            .iter()
                            .map(|row| (row.id, row.role, row.season_window)),
                        points.phenotypes[index],
                        points.lifecycles[index],
                        season_mille,
                    );
                    let combination = render
                        .combinations
                        .iter()
                        .position(|candidate| {
                            *candidate == (points.variations[index], rendered_phenotype)
                        })
                        .or_else(|| {
                            render
                                .combinations
                                .iter()
                                .position(|candidate| candidate.1 == rendered_phenotype)
                        })
                        .unwrap_or(0) as u32;
                    if entry.record.combination == combination {
                        continue;
                    }
                    let mut record = entry.record.clone();
                    if let Some(vegetation) = record.vegetation.as_mut() {
                        vegetation.combination_previous = entry.record.combination;
                        vegetation.flip_stamp = flip_stamp;
                    }
                    record.combination = combination;
                    target
                        .gpu_scene
                        .apply_world_delta(
                            world,
                            GpuSceneWorldDelta::UpdateInstance {
                                handle: entry.handle,
                                record: record.clone(),
                            },
                        )
                        .map_err(gpu_scene_error)?;
                    entry.record = record;
                    mutated = true;
                }
            }
            world_state.season_mille = season_mille;
        }
        if mutated {
            world_state.invalidate_rays();
            reconcile_field_instances(shared, world_state, world, &families, assets, gpu, target)?;
            rebuild_field_directory(world_state, target)?;
        }
        Ok(mutated)
    }

    /// This frame's vegetation budget breaches, named by the content that owns them.
    ///
    /// Each family resolves to its catalog name so the alarm reads as the asset an author knows;
    /// a family no longer in the catalog keeps its id.
    #[must_use]
    pub fn vegetation_budget_breaches(
        &self,
        assets: &AssetServer,
    ) -> Vec<saffron_rendering::OwnedBudgetBreach> {
        let budgets = self.vegetation_budgets;
        let breakdown = self.vegetation_breakdown();
        let mut breaches = Vec::new();
        let mut push = |metric: &str, owner: String, value: f64, threshold: f64| {
            if threshold <= 0.0 || value <= threshold {
                return;
            }
            breaches.push(saffron_rendering::OwnedBudgetBreach {
                metric: metric.to_owned(),
                owner,
                // Over budget warns; half again over is an error an author has to act on.
                severity: if value >= threshold * 1.5 {
                    saffron_rendering::AlarmSeverity::Critical
                } else {
                    saffron_rendering::AlarmSeverity::Warning
                },
                value: value as f32,
                threshold: threshold as f32,
            });
        };
        for row in &breakdown.cells {
            let coordinates = row.cell.coordinates();
            push(
                "vegetation-cell-plants",
                format!(
                    "cell {},{},{} L{}",
                    coordinates[0],
                    coordinates[1],
                    coordinates[2],
                    row.cell.level()
                ),
                f64::from(row.plants),
                f64::from(budgets.cell_plants),
            );
        }
        for row in &breakdown.families {
            let owner = assets
                .catalog()
                .entries
                .iter()
                .find(|entry| entry.id.value() == row.family)
                .map_or_else(
                    || format!("family {}", row.family),
                    |entry| format!("family {} ({})", entry.name, row.family),
                );
            push(
                "vegetation-family-instances",
                owner.clone(),
                f64::from(row.instances),
                f64::from(budgets.family_instances),
            );
            push(
                "vegetation-family-blades",
                owner,
                row.micro_predicted as f64,
                budgets.family_micro_predicted as f64,
            );
        }
        breaches
    }

    /// The budgets [`Self::vegetation_budget_breaches`] measures against.
    #[must_use]
    pub fn vegetation_budgets(&self) -> VegetationBudgets {
        self.vegetation_budgets
    }

    /// Replaces the vegetation budgets. A zero disables that budget rather than alarming on
    /// everything, which is the only reading that lets a project opt one out.
    pub fn set_vegetation_budgets(&mut self, budgets: VegetationBudgets) {
        self.vegetation_budgets = budgets;
    }

    /// Per-family and per-cell vegetation population across every synced world:
    /// `(family, plant instances, field tiles, predicted micro candidates)` rows and
    /// `(cell, plants, field tiles)` rows, both in stable sorted order.
    pub fn vegetation_breakdown(&self) -> VegetationRenderBreakdown {
        use std::collections::BTreeMap;
        let mut families: BTreeMap<u64, (u32, u32, u64)> = BTreeMap::new();
        let mut cells: BTreeMap<WorldCellKey, (u32, u32)> = BTreeMap::new();
        for world in self.worlds.values() {
            for ((cell, _), entry) in &world.plants {
                let family = families.entry(entry.mesh).or_default();
                family.0 += 1;
                cells.entry(*cell).or_default().0 += 1;
            }
            for (cell, entry) in &world.plant_fields {
                for (family, _, predicted) in &entry.tiles {
                    let row = families.entry(*family).or_default();
                    row.1 += 1;
                    row.2 += u64::from(*predicted);
                }
                cells.entry(*cell).or_default().1 += entry.tiles.len() as u32;
            }
        }
        VegetationRenderBreakdown {
            families: families
                .into_iter()
                .map(
                    |(family, (instances, tiles, predicted))| VegetationFamilyRenderRow {
                        family,
                        instances,
                        field_tiles: tiles,
                        micro_predicted: predicted,
                    },
                )
                .collect(),
            cells: cells
                .into_iter()
                .map(|(cell, (plants, tiles))| VegetationCellRenderRow {
                    cell,
                    plants,
                    field_tiles: tiles,
                })
                .collect(),
        }
    }
}

/// One plant's conservative bounds as an instance-local pre-scale sphere: the world
/// AABB's containing sphere, its center carried into the point's local frame
/// (inverse-rotated, de-scaled) so the cull composes it exactly like a prototype
/// sphere.
fn plant_bounds_sphere(
    position: saffron_spatial::WorldPosition,
    orientation: saffron_vegetation::QuantizedOrientation,
    scale: [saffron_spatial::DecisionScalar; 3],
    bounds: saffron_spatial::WorldBounds,
) -> [f32; 4] {
    use saffron_geometry::glam::{DQuat, DVec3};
    let tick = 1.0 / f64::from(saffron_spatial::LOCAL_TICKS_PER_METER);
    let minimum = bounds.min_ticks().map(|value| value as f64 * tick);
    let maximum = bounds
        .max_ticks_exclusive()
        .map(|value| value as f64 * tick);
    let center = DVec3::new(
        (minimum[0] + maximum[0]) * 0.5,
        (minimum[1] + maximum[1]) * 0.5,
        (minimum[2] + maximum[2]) * 0.5,
    );
    let extent = DVec3::new(
        maximum[0] - minimum[0],
        maximum[1] - minimum[1],
        maximum[2] - minimum[2],
    );
    let radius = extent.length() * 0.5;
    let quantized = orientation.bits();
    let rotation = DQuat::from_xyzw(
        f64::from(quantized[0]) / 32_767.0,
        f64::from(quantized[1]) / 32_767.0,
        f64::from(quantized[2]) / 32_767.0,
        f64::from(quantized[3]) / 32_767.0,
    )
    .normalize();
    let uniform_scale = scale
        .iter()
        .map(|value| f64::from(value.bits()) / 65_536.0)
        .fold(f64::MIN, f64::max)
        .max(1e-4);
    let local = rotation.inverse() * (center - position.world_meters()) / uniform_scale;
    [
        local.x as f32,
        local.y as f32,
        local.z as f32,
        (radius / uniform_scale) as f32,
    ]
}

/// Removes every mirrored plant instance of one cell, releasing its shared references.
pub(super) fn remove_cell_plants(
    shared: &mut SharedMirror,
    world_state: &mut WorldMirror,
    world: GpuSceneWorldId,
    cell: WorldCellKey,
    target: &mut GpuSceneMirrorTarget<'_>,
) -> Result<()> {
    let keys: Vec<(WorldCellKey, PlantId)> = world_state
        .plants
        .keys()
        .filter(|(owner, _)| *owner == cell)
        .copied()
        .collect();
    if !keys.is_empty() {
        world_state.invalidate_rays();
    }
    for key in keys {
        let entry = world_state
            .plants
            .remove(&key)
            .expect("plant entry present");
        target
            .gpu_scene
            .apply_world_delta(world, GpuSceneWorldDelta::RemoveInstance(entry.handle))
            .map_err(gpu_scene_error)?;
        for (_, material_key) in &entry.overrides {
            shared.unref_material(material_key, target)?;
        }
        shared.unref_mesh(entry.mesh);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::*;
    use saffron_rendering::validation_issue_count;

    /// The macro snapshot adapter: a resident vegetation cell's plants become
    /// persistent-scene instances keyed `(cell, PlantId)`, diffed purely by published
    /// generation — an unchanged cell re-syncs to the identical instance, and a
    /// republished cell (a tombstone mutation) removes its plant atomically.
    #[test]
    fn vegetation_sync_translates_resident_cells_by_generation() {
        use saffron_spatial::{
            QuantizedLocalPosition, ResidencyFacet, ResidencyMask, SourceLevel, SpatialSource,
            SpatialSourceId,
        };
        use saffron_spatial::{WorldBounds, WorldPosition};
        use saffron_vegetation::QuantizedOrientation;
        use saffron_vegetation::{
            CookPlatformProfile, CookVersionSet, CookWorkActual, CookWorkEstimate,
            InteractionPolicy, ManifestCellSection, ManifestSpeciesCount, PlantFlags,
            PlantLifecycle, PlantPoint, PlantPointColumns, VegetationBaseManifest,
            VegetationCellArtifactHeader, VegetationCellArtifactIndex, VegetationCellSection,
            VegetationCellSectionKind, VegetationManifestCell, VegetationManifestPlant,
            VegetationMutation, VegetationMutationRecord, VegetationResidencyBudgets,
            VegetationWorld, write_vegetation_cell_artifact,
        };

        let Some(mut harness) = harness("vegetation") else {
            return;
        };
        let before = validation_issue_count();

        // Register the assembly family under its id and seed the loader cache with the
        // manifest's exact artifact identity, so the sync resolves it without a store.
        let family_id = Uuid(9_888);
        let artifact_hash = saffron_vegetation::ContentHash::new([6; 32]);
        let (flat, hierarchy) = assembly_family_fixture();
        let gpu = RendererUploader::new(
            &harness.fixture.uploader,
            &harness.fixture.descriptors,
            false,
        );
        let uploaded = gpu
            .upload_mesh(
                &flat,
                &hierarchy,
                &[],
                None,
                saffron_rendering::SdfSource::None,
            )
            .expect("assembly upload");
        harness.assets.register_family_render(
            family_id,
            Arc::clone(&uploaded),
            Arc::new(hierarchy),
            mechanics_fixture(),
        );
        harness.assets.plant_render_by_hash.insert(
            artifact_hash,
            Some(crate::PlantFamilyRender {
                mesh: Arc::clone(&uploaded),
                atlas: None,
                materials: Arc::from([] as [Uuid; 0]),
                combinations: Arc::from([(0_u32, 0_u32), (0, 1)]),
                phenotypes: Arc::from([
                    crate::PlantPhenotypeRender {
                        id: 0,
                        role: saffron_vegetation::PhenotypeRole::Healthy,
                        season_window: None,
                        variation: 0,
                        material_remap: Arc::from([]),
                    },
                    crate::PlantPhenotypeRender {
                        id: 1,
                        role: saffron_vegetation::PhenotypeRole::Senescent,
                        season_window: None,
                        variation: 0,
                        material_remap: Arc::from([]),
                    },
                ]),
                mechanics: mechanics_fixture(),
            }),
        );

        // One resident cell with one mature plant of that family.
        let cell = saffron_spatial::WorldCellKey::base(0, 0, 0);
        let plant_id = saffron_vegetation::PlantId::explicit([1; 16]).unwrap();
        let position =
            WorldPosition::new(cell, QuantizedLocalPosition::new([10, 20, 30]).unwrap()).unwrap();
        let point = PlantPoint {
            id: plant_id,
            owner: cell,
            position,
            orientation: QuantizedOrientation::identity(),
            scale: [saffron_spatial::DecisionScalar::from_bits(65_536); 3],
            bounds: WorldBounds::new([0, 0, 0], [100, 100, 100]).unwrap(),
            family: family_id,
            variation: 0,
            lifecycle: PlantLifecycle::Mature,
            phenotype: 0,
            representation_class: 0,
            deterministic_key: 3,
            candidate: 4,
            parent: None,
            colony: None,
            ecology_tick: 5,
            health: UnitInterval::ONE,
            moisture: UnitInterval::ONE,
            fuel: UnitInterval::ONE,
            phenology: UnitInterval::ZERO,
            flags: PlantFlags::AUTHORED,
            interaction_policy: InteractionPolicy::Decorative,
            provenance: 0,
            attachment: None,
            surface_projection: [saffron_spatial::DecisionScalar::from_bits(0); 3],
        };
        let platform = CookPlatformProfile {
            target: "test-target".to_owned(),
            content_profile: "portable-vulkan".to_owned(),
            toolchain: "rust-test".to_owned(),
            features: vec!["canonical-fixed".to_owned()],
        };
        let columns = PlantPointColumns::from_points(vec![point.clone()]).unwrap();
        let tile = MicroFieldTile {
            cell,
            family: family_id,
            dimensions: [4, 1, 4],
            density: vec![32_768; 16],
            attributes: std::collections::BTreeMap::from([(7_u128, vec![1_i32; 16])]),
            reconstruction_seed: 0x0102_0304_0506_0708_090a_0b0c_0d0e_0f10,
        };
        let sections = vec![
            VegetationCellSection::new(
                VegetationCellSectionKind::MacroPoints,
                columns.canonical_bytes().unwrap(),
            ),
            VegetationCellSection::new(
                VegetationCellSectionKind::MicroFields,
                saffron_vegetation::encode_vegetation_micro_fields(std::slice::from_ref(&tile))
                    .unwrap(),
            ),
            VegetationCellSection::new(
                VegetationCellSectionKind::RenderReferences,
                [b"SVEGRRF1".as_slice(), &0_u64.to_be_bytes()].concat(),
            ),
            VegetationCellSection::new(
                VegetationCellSectionKind::RenderBounds,
                [b"SVEGRBD1".as_slice(), &0_u64.to_be_bytes()].concat(),
            ),
        ];
        let artifact = write_vegetation_cell_artifact(
            VegetationCellArtifactHeader {
                cell,
                cook_key: saffron_vegetation::ContentHash::new([8; 32]),
                platform_profile: platform.identity().unwrap(),
            },
            &sections,
        )
        .unwrap();
        let index = VegetationCellArtifactIndex::open(
            &artifact,
            saffron_vegetation::VEGETATION_ARTIFACT_DECODE_LIMITS,
        )
        .unwrap();
        let mut manifest = VegetationBaseManifest::current(
            Uuid(1),
            Uuid(2),
            saffron_vegetation::ContentHash::new([3; 32]),
            CookVersionSet::current(),
            platform,
            saffron_vegetation::ContentHash::new([4; 32]),
        );
        manifest.plants.push(VegetationManifestPlant {
            family: family_id,
            tags: Vec::new(),
            source_hash: saffron_vegetation::ContentHash::new([5; 32]),
            artifact_hash,
            local_bounds_min: [saffron_spatial::DecisionScalar::from_bits(-65_536); 3],
            local_bounds_max: [saffron_spatial::DecisionScalar::from_bits(65_536); 3],
            variation_count: 1,
            phenotype_count: 1,
            ecology: saffron_vegetation::PlantEcologyDeclaration::default(),
        });
        manifest.cells.push(VegetationManifestCell {
            cell,
            bounds: cell.bounds(),
            artifact_hash: saffron_vegetation::ContentHash::of(&artifact),
            payload_hash: index.payload_hash,
            dependencies: Vec::new(),
            species_counts: vec![ManifestSpeciesCount {
                family: family_id,
                macro_count: 1,
                micro_count: 0,
            }],
            macro_count: 1,
            micro_count: 0,
            resident_memory_bytes: index.sections.iter().map(|value| value.decoded_size).sum(),
            stored_bytes: artifact.len() as u64,
            estimate: CookWorkEstimate::default(),
            actual: CookWorkActual::default(),
            sections: index
                .sections
                .iter()
                .map(|section| ManifestCellSection {
                    kind: section.kind,
                    version: section.version,
                    codec: section.codec,
                    alignment: section.alignment,
                    stored_size: section.stored_size,
                    decoded_size: section.decoded_size,
                    content_hash: section.content_hash,
                })
                .collect(),
        });
        let mut vegetation =
            VegetationWorld::new(manifest, VegetationResidencyBudgets::UNLIMITED).unwrap();
        vegetation
            .update_source(SpatialSource {
                id: SpatialSourceId(1),
                revision: 1,
                position: WorldPosition::origin(),
                velocity_mps: saffron_geometry::glam::DVec3::ZERO,
                prediction_seconds: 0.0,
                levels: vec![SourceLevel {
                    level: 0,
                    load_radius_cells: 0,
                    cleanup_radius_cells: 1,
                }],
                facets: ResidencyMask::one(ResidencyFacet::Render),
                priority: 10,
            })
            .unwrap();
        let staged = vegetation
            .begin_load(cell, ResidencyMask::one(ResidencyFacet::Render))
            .unwrap()
            .stage(&artifact)
            .unwrap();
        assert!(vegetation.publish_staged(staged).unwrap());

        let sync =
            |harness: &mut MirrorHarness, vegetation: &VegetationWorld, season: u16, stamp: u32| {
                let gpu = RendererUploader::new(
                    &harness.fixture.uploader,
                    &harness.fixture.descriptors,
                    false,
                );
                let mut target = GpuSceneMirrorTarget {
                    gpu_data: &mut harness.gpu_data,
                    gpu_scene: &mut harness.gpu_scene,
                    default_white: &harness.default_white,
                    pending: &mut harness.pending,
                    residency: &mut harness.residency,
                };
                harness
                    .mirror
                    .sync_vegetation(
                        WORLD,
                        vegetation,
                        &mut harness.assets,
                        &gpu,
                        season,
                        stamp,
                        &mut target,
                    )
                    .expect("vegetation sync");
            };
        sync(&mut harness, &vegetation, 0, 0);

        assert_eq!(harness.mirror.stats().instances, 1, "one mirrored plant");
        // The cell's micro tile packed into the fields arena: 64 B header + 16 padded
        // u16 density samples + one channel (16 B id + 16 i32 values).
        let field_range = harness.mirror.worlds[&WORLD.0].plant_fields[&cell].range;
        assert_eq!(field_range.count, 64 + 32 + 16 + 64);
        // The family gained one flagged identity field instance, and the resident-tile
        // directory carries one entry referencing it.
        let field_handle = harness.mirror.worlds[&WORLD.0].field_instances[&family_id.value()];
        let field_record = harness
            .gpu_scene
            .instance(WORLD, field_handle)
            .expect("field instance record");
        assert_eq!(
            field_record.flags,
            saffron_rendering::GPU_SCENE_INSTANCE_FLAG_MICRO_FIELD
        );
        let (directory_range, directory_count) = harness.mirror.worlds[&WORLD.0]
            .field_directory
            .expect("directory");
        assert_eq!(directory_count, 1);
        assert_eq!(directory_range.count, 16);
        // The predicted budget: 16 texels at density 32768 → two blades each.
        assert_eq!(harness.mirror.stats().micro_predicted, 32);
        let entry = &harness.mirror.worlds[&WORLD.0].plants[&(cell, plant_id)];
        let handle = entry.handle;
        let record = harness
            .gpu_scene
            .instance(WORLD, handle)
            .expect("plant instance record");
        assert!(
            matches!(record.transform, GpuSceneTransform::Static(_)),
            "macro plants place with the compact exact transform"
        );
        // The vegetation columns: the fixture's decorative unattached point uploads
        // explicit conservative bounds (previous equals current at rest) and rides
        // the wind deformation prepass.
        assert_eq!(
            record.flags,
            GPU_SCENE_INSTANCE_FLAG_EXPLICIT_BOUNDS | GPU_SCENE_INSTANCE_FLAG_WIND
        );
        let columns = record.vegetation.expect("vegetation columns");
        assert!(columns.bounds_current[3] > 0.0, "a real bounds radius");
        assert_eq!(columns.bounds_current, columns.bounds_previous);
        assert!(columns.attachment.is_none());

        // An unchanged generation is a no-op: the same handle survives.
        sync(&mut harness, &vegetation, 0, 1);
        assert_eq!(
            harness.mirror.worlds[&WORLD.0].plants[&(cell, plant_id)].handle,
            handle
        );

        // An autumn season resolves the mature plant to the Senescent phenotype:
        // the flip updates the SAME instance in place, carrying the previous
        // combination and the flip stamp for the traversal's crossfade.
        sync(&mut harness, &vegetation, 800, 42);
        let entry = &harness.mirror.worlds[&WORLD.0].plants[&(cell, plant_id)];
        assert_eq!(
            entry.handle, handle,
            "the flip never recreates the instance"
        );
        assert_eq!(entry.record.combination, 1, "the Senescent combination");
        let flipped = entry
            .record
            .vegetation
            .as_ref()
            .expect("vegetation columns");
        assert_eq!(flipped.combination_previous, 0);
        assert_eq!(flipped.flip_stamp, 42);
        let device_record = harness
            .gpu_scene
            .instance(WORLD, handle)
            .expect("updated plant record");
        assert_eq!(device_record.combination, 1);

        // Scrubbing back to summer flips again, previous now the autumn pair.
        sync(&mut harness, &vegetation, 100, 60);
        let entry = &harness.mirror.worlds[&WORLD.0].plants[&(cell, plant_id)];
        assert_eq!(entry.handle, handle);
        assert_eq!(entry.record.combination, 0);
        let restored = entry
            .record
            .vegetation
            .as_ref()
            .expect("vegetation columns");
        assert_eq!(restored.combination_previous, 1);
        assert_eq!(restored.flip_stamp, 60);

        // A tombstone republishes the cell; the re-translation removes the plant.
        vegetation
            .apply_confirmed_mutations(&[VegetationMutationRecord {
                header: saffron_vegetation::MutationHeader {
                    cell,
                    transaction: 1,
                    authority: 2,
                    logical_tick: 3,
                    idempotency_key: 4,
                    base_revision: None,
                },
                mutation: VegetationMutation::Tombstone { plant: plant_id },
            }])
            .unwrap();
        sync(&mut harness, &vegetation, 100, 61);
        assert_eq!(
            harness.mirror.stats().instances,
            0,
            "the tombstoned plant left the scene"
        );
        assert!(harness.gpu_scene.instance(WORLD, handle).is_none());
        // The republished cell re-packed its (unchanged) micro tile into a fresh range;
        // the field instance and directory survive (the family still has tiles).
        let republished_range = harness.mirror.worlds[&WORLD.0].plant_fields[&cell].range;
        assert_eq!(republished_range.count, field_range.count);
        assert_ne!(republished_range.first, field_range.first);
        assert_eq!(
            harness.mirror.worlds[&WORLD.0].field_instances[&family_id.value()],
            field_handle
        );
        assert!(harness.mirror.worlds[&WORLD.0].field_directory.is_some());

        drop(uploaded);
        harness.finish();
        assert_eq!(validation_issue_count(), before);
    }
}
