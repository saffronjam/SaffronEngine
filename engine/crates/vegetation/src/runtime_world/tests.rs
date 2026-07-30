use std::collections::{BTreeMap, BTreeSet};

use glam::DVec3;
use saffron_core::Uuid;
use saffron_spatial::{
    DecisionScalar, QuantizedLocalPosition, ResidencyFacet, ResidencyMask, SourceLevel,
    SpatialSource, SpatialSourceId, UnitInterval, WorldBounds, WorldCellKey, WorldPosition,
};

use super::*;
use crate::{
    ContentHash, CookPlatformProfile, CookVersionSet, CookWorkActual, CookWorkEstimate,
    EcologyCatchUp, EcologyRelations, Error, InteractionPolicy, ManifestCellSection,
    ManifestSpeciesCount, PlantFlags, PlantId, PlantLifecycle, PlantPoint, PlantPointColumns,
    PlantTagId, QuantizedOrientation, VEGETATION_ARTIFACT_DECODE_LIMITS, VegetationBaseManifest,
    VegetationCellArtifactHeader, VegetationCellArtifactIndex, VegetationCellSection,
    VegetationCellSectionKind, VegetationManifestCell, VegetationManifestPlant,
    VegetationMutationRecord, write_vegetation_cell_artifact,
};

fn point() -> PlantPoint {
    point_in(WorldCellKey::base(0, 0, 0), 1)
}

/// One mature plant of the shared family, placed the same way inside whichever cell owns it.
fn point_in(cell: WorldCellKey, identity: u8) -> PlantPoint {
    let position =
        WorldPosition::new(cell, QuantizedLocalPosition::new([10, 20, 30]).unwrap()).unwrap();
    let min = cell.bounds().min_ticks();
    PlantPoint {
        id: PlantId::explicit([identity; 16]).unwrap(),
        owner: cell,
        position,
        orientation: QuantizedOrientation::identity(),
        scale: [DecisionScalar::from_bits(65_536); 3],
        bounds: WorldBounds::new(min, [min[0] + 100, min[1] + 100, min[2] + 100]).unwrap(),
        family: Uuid(7),
        lifecycle: PlantLifecycle::Mature,
        variation: 0,
        phenotype: 2,
        representation_class: 1,
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
        interaction_policy: InteractionPolicy::Interactive,
        provenance: 0,
        attachment: None,
        surface_projection: [DecisionScalar::from_bits(0); 3],
    }
}

fn fixture() -> (VegetationWorld, Vec<u8>, PlantId) {
    fixture_with_micro(false)
}

fn platform() -> CookPlatformProfile {
    CookPlatformProfile {
        target: "aarch64-apple-darwin".to_owned(),
        content_profile: "portable-vulkan".to_owned(),
        toolchain: "rust-1.96".to_owned(),
        features: vec!["canonical-fixed".to_owned()],
    }
}

fn manifest_plant() -> VegetationManifestPlant {
    VegetationManifestPlant {
        family: Uuid(7),
        tags: vec![PlantTagId::new(11).unwrap(), PlantTagId::new(13).unwrap()],
        source_hash: ContentHash::new([5; 32]),
        artifact_hash: ContentHash::new([6; 32]),
        local_bounds_min: [DecisionScalar::from_bits(-65_536); 3],
        local_bounds_max: [DecisionScalar::from_bits(65_536); 3],
        variation_count: 1,
        phenotype_count: 3,
        ecology: crate::PlantEcologyDeclaration::default(),
    }
}

/// One cell's artifact bytes and the manifest row naming them.
fn cell_artifact(
    point: &PlantPoint,
    platform: &CookPlatformProfile,
    micro: bool,
) -> (Vec<u8>, VegetationManifestCell) {
    let columns = PlantPointColumns::from_points(vec![point.clone()]).unwrap();
    let mut sections = vec![
        VegetationCellSection::new(
            VegetationCellSectionKind::MacroPoints,
            columns.canonical_bytes().unwrap(),
        ),
        VegetationCellSection::new(
            VegetationCellSectionKind::CollisionInputs,
            [b"SVEGCOL1".as_slice(), &0_u64.to_be_bytes()].concat(),
        ),
    ];
    if micro {
        let mut density = vec![32_768_u16; 16];
        density[0] = 0;
        let tile = crate::MicroFieldTile {
            cell: point.owner,
            family: point.family,
            dimensions: [4, 1, 4],
            density,
            attributes: BTreeMap::new(),
            reconstruction_seed: 0x0102_0304_0506_0708_090a_0b0c_0d0e_0f10,
        };
        sections.push(VegetationCellSection::new(
            VegetationCellSectionKind::MicroFields,
            crate::encode_vegetation_micro_fields(std::slice::from_ref(&tile)).unwrap(),
        ));
        sections.push(VegetationCellSection::new(
            VegetationCellSectionKind::RenderReferences,
            [b"SVEGRRF1".as_slice(), &0_u64.to_be_bytes()].concat(),
        ));
        sections.push(VegetationCellSection::new(
            VegetationCellSectionKind::RenderBounds,
            [b"SVEGRBD1".as_slice(), &0_u64.to_be_bytes()].concat(),
        ));
    }
    let bytes = write_vegetation_cell_artifact(
        VegetationCellArtifactHeader {
            cell: point.owner,
            cook_key: ContentHash::new([8; 32]),
            platform_profile: platform.identity().unwrap(),
        },
        &sections,
    )
    .unwrap();
    let index =
        VegetationCellArtifactIndex::open(&bytes, VEGETATION_ARTIFACT_DECODE_LIMITS).unwrap();
    let row = VegetationManifestCell {
        cell: point.owner,
        bounds: point.owner.bounds(),
        artifact_hash: ContentHash::of(&bytes),
        payload_hash: index.payload_hash,
        dependencies: Vec::new(),
        species_counts: vec![ManifestSpeciesCount {
            family: point.family,
            macro_count: 1,
            micro_count: 0,
        }],
        macro_count: 1,
        micro_count: 0,
        resident_memory_bytes: index.sections.iter().map(|value| value.decoded_size).sum(),
        stored_bytes: bytes.len() as u64,
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
    };
    (bytes, row)
}

fn world_from(cells: Vec<VegetationManifestCell>) -> VegetationWorld {
    let mut manifest = VegetationBaseManifest::current(
        Uuid(1),
        Uuid(2),
        ContentHash::new([3; 32]),
        CookVersionSet::current(),
        platform(),
        ContentHash::new([4; 32]),
    );
    manifest.plants.push(manifest_plant());
    manifest.cells = cells;
    VegetationWorld::new(manifest, VegetationResidencyBudgets::UNLIMITED).unwrap()
}

fn fixture_with_micro(micro: bool) -> (VegetationWorld, Vec<u8>, PlantId) {
    let point = point();
    let (bytes, row) = cell_artifact(&point, &platform(), micro);
    (world_from(vec![row]), bytes, point.id)
}

/// A world planted in every named cell, so a catch-up has one dependency region per cell as long as
/// the cells are farther apart than the influence radius.
fn multi_region_world(cells: &[WorldCellKey]) -> (VegetationWorld, Vec<Vec<u8>>) {
    let platform = platform();
    let mut rows = Vec::new();
    let mut artifacts = Vec::new();
    for (index, cell) in cells.iter().enumerate() {
        let point = point_in(*cell, u8::try_from(index).unwrap() + 1);
        let (bytes, row) = cell_artifact(&point, &platform, false);
        rows.push(row);
        artifacts.push(bytes);
    }
    (world_from(rows), artifacts)
}

/// A world with every named cell resident on the physics facet, one source per cell.
fn loaded_multi_region_world(cells: &[WorldCellKey]) -> VegetationWorld {
    let (mut world, artifacts) = multi_region_world(cells);
    for (index, cell) in cells.iter().enumerate() {
        let mut source = source();
        source.id = SpatialSourceId(u64::try_from(index).unwrap() + 1);
        source.position = WorldPosition::new(*cell, QuantizedLocalPosition::default()).unwrap();
        world.update_source(source).unwrap();
    }
    for (cell, artifact) in cells.iter().zip(&artifacts) {
        let staged = world
            .begin_load(*cell, ResidencyMask::one(ResidencyFacet::Physics))
            .unwrap()
            .stage(artifact)
            .unwrap();
        assert!(world.publish_staged(staged).unwrap());
    }
    world
}

fn source() -> SpatialSource {
    source_with_facet(ResidencyFacet::Physics)
}

fn source_with_facet(facet: ResidencyFacet) -> SpatialSource {
    SpatialSource {
        id: SpatialSourceId(1),
        revision: 1,
        position: WorldPosition::origin(),
        velocity_mps: DVec3::ZERO,
        prediction_seconds: 0.0,
        levels: vec![SourceLevel {
            level: 0,
            load_radius_cells: 0,
            cleanup_radius_cells: 1,
        }],
        facets: ResidencyMask::one(facet),
        priority: 10,
    }
}

/// A confirmed mutation must bump its cell's bulk revision: a render adapter caches the
/// revision beside the generation id and re-derives the cell when either moves, so a
/// phenotype override that left the revision alone would keep rendering the old
/// combination while the runtime reported the new one.
#[test]
fn a_confirmed_mutation_bumps_the_cell_bulk_revision() {
    let (mut world, artifact, plant) = fixture();
    world.update_source(source()).unwrap();
    let cell = WorldCellKey::base(0, 0, 0);
    let staged = world
        .begin_load(cell, ResidencyMask::one(ResidencyFacet::Physics))
        .unwrap()
        .stage(&artifact)
        .unwrap();
    assert!(world.publish_staged(staged).unwrap());
    let before = world.cell_bulk_revision(cell);
    world
        .apply_confirmed_mutations(&[VegetationMutationRecord {
            header: crate::MutationHeader {
                cell,
                transaction: 7,
                authority: 2,
                logical_tick: 3,
                idempotency_key: 9,
                base_revision: None,
            },
            mutation: crate::VegetationMutation::StateOverride {
                plant,
                lifecycle: None,
                phenotype: Some(1),
                health: None,
                moisture: None,
                fuel: None,
                interaction_policy: None,
            },
        }])
        .unwrap();
    assert_eq!(world.cell_bulk_revision(cell), before + 1);
}

#[test]
fn load_query_unload_and_reload_preserve_persistent_tombstones() {
    let (mut world, artifact, plant) = fixture();
    world.update_source(source()).unwrap();
    let report = world.residency_report().unwrap();
    assert_eq!(report.pending.len(), 1);
    assert_eq!(
        report.pending[0].facets,
        ResidencyMask::one(ResidencyFacet::Physics)
    );

    let staged = world
        .begin_load(
            WorldCellKey::base(0, 0, 0),
            ResidencyMask::one(ResidencyFacet::Physics),
        )
        .unwrap()
        .stage(&artifact)
        .unwrap();
    assert!(world.publish_staged(staged).unwrap());
    let found = world.find_plant(plant).unwrap().unwrap();
    assert_eq!(
        found.tags,
        vec![PlantTagId::new(11).unwrap(), PlantTagId::new(13).unwrap()]
    );

    world
        .apply_confirmed_mutations(&[VegetationMutationRecord {
            header: crate::MutationHeader {
                cell: WorldCellKey::base(0, 0, 0),
                transaction: 1,
                authority: 2,
                logical_tick: 3,
                idempotency_key: 4,
                base_revision: None,
            },
            mutation: crate::VegetationMutation::Tombstone { plant },
        }])
        .unwrap();
    assert!(world.find_plant(plant).unwrap().is_none());
    assert!(matches!(
        world.resolve_handle(found.handle),
        Err(Error::StaleGeneration { .. })
    ));

    let snapshot = world.export_state_snapshot().unwrap();
    let (mut restored, restored_artifact, restored_plant) = fixture();
    restored.import_state_snapshot(&snapshot).unwrap();
    restored.update_source(source()).unwrap();
    let staged = restored
        .begin_load(
            WorldCellKey::base(0, 0, 0),
            ResidencyMask::one(ResidencyFacet::Physics),
        )
        .unwrap()
        .stage(&restored_artifact)
        .unwrap();
    restored.publish_staged(staged).unwrap();
    assert!(restored.find_plant(restored_plant).unwrap().is_none());

    assert!(world.remove_source(SpatialSourceId(1)).unwrap());
    assert_eq!(
        world
            .cell_snapshot(WorldCellKey::base(0, 0, 0))
            .unwrap()
            .resident_facets(),
        ResidencyMask::NONE
    );
    world.update_source(source()).unwrap();
    let staged = world
        .begin_load(
            WorldCellKey::base(0, 0, 0),
            ResidencyMask::one(ResidencyFacet::Physics),
        )
        .unwrap()
        .stage(&artifact)
        .unwrap();
    assert!(world.publish_staged(staged).unwrap());
    assert!(world.find_plant(plant).unwrap().is_none());
}

#[test]
fn prediction_overlay_is_transient_until_authority_confirmation() {
    let (mut world, artifact, plant) = fixture();
    world.update_source(source()).unwrap();
    let staged = world
        .begin_load(
            WorldCellKey::base(0, 0, 0),
            ResidencyMask::one(ResidencyFacet::Physics),
        )
        .unwrap()
        .stage(&artifact)
        .unwrap();
    world.publish_staged(staged).unwrap();
    let record = VegetationMutationRecord {
        header: crate::MutationHeader {
            cell: WorldCellKey::base(0, 0, 0),
            transaction: 10,
            authority: 20,
            logical_tick: 30,
            idempotency_key: 40,
            base_revision: None,
        },
        mutation: crate::VegetationMutation::Tombstone { plant },
    };

    world
        .apply_prediction(std::slice::from_ref(&record))
        .unwrap();
    assert_eq!(world.prediction_count(), 1);
    assert!(world.find_plant(plant).unwrap().is_none());
    assert!(world.persistent_state().cells().is_empty());

    let snapshot = world.export_state_snapshot().unwrap();
    let (mut restored, restored_artifact, _) = fixture();
    restored.import_state_snapshot(&snapshot).unwrap();
    restored.update_source(source()).unwrap();
    let staged = restored
        .begin_load(
            WorldCellKey::base(0, 0, 0),
            ResidencyMask::one(ResidencyFacet::Physics),
        )
        .unwrap()
        .stage(&restored_artifact)
        .unwrap();
    restored.publish_staged(staged).unwrap();
    assert!(restored.find_plant(plant).unwrap().is_some());

    assert!(world.reject_prediction(10).unwrap());
    assert_eq!(world.prediction_count(), 0);
    assert!(world.find_plant(plant).unwrap().is_some());

    world
        .apply_prediction(std::slice::from_ref(&record))
        .unwrap();
    world.confirm_prediction(10).unwrap();
    assert_eq!(world.prediction_count(), 0);
    assert!(world.find_plant(plant).unwrap().is_none());
    assert!(!world.persistent_state().cells().is_empty());
}

#[test]
fn spatial_queries_filter_tags_and_return_only_stable_identity() {
    let (mut world, artifact, plant) = fixture();
    world.update_source(source()).unwrap();
    let staged = world
        .begin_load(
            WorldCellKey::base(0, 0, 0),
            ResidencyMask::one(ResidencyFacet::Physics),
        )
        .unwrap()
        .stage(&artifact)
        .unwrap();
    world.publish_staged(staged).unwrap();
    let filter = VegetationQueryFilter {
        required_tags: BTreeSet::from([PlantTagId::new(13).unwrap()]),
        ..VegetationQueryFilter::default()
    };
    let bounds = WorldBounds::new([0, 0, 0], [50, 50, 50]).unwrap();
    assert_eq!(world.query_bounds(bounds, &filter).unwrap()[0].plant, plant);
    assert_eq!(
        world
            .query_radius(WorldPosition::origin(), 1.0, &filter)
            .unwrap()[0]
            .plant,
        plant
    );
    let ray = VegetationQueryRay::new(WorldPosition::origin(), DVec3::X, 1.0).unwrap();
    assert_eq!(world.query_ray(ray, &filter).unwrap()[0].plant.plant, plant);
    assert_eq!(
        world
            .query_nearest(WorldPosition::origin(), Some(1.0), &filter)
            .unwrap()
            .unwrap()
            .plant
            .plant,
        plant
    );
}

#[test]
fn micro_ray_lands_on_the_nearest_dense_floor_texel() {
    let (mut world, artifact, _) = fixture_with_micro(true);
    world
        .update_source(source_with_facet(ResidencyFacet::Render))
        .unwrap();
    let staged = world
        .begin_load(
            WorldCellKey::base(0, 0, 0),
            ResidencyMask::one(ResidencyFacet::Render),
        )
        .unwrap()
        .stage(&artifact)
        .unwrap();
    world.publish_staged(staged).unwrap();

    let down = |x: f64, z: f64, maximum: f64| {
        VegetationQueryRay::new(
            WorldPosition::from_world_meters(DVec3::new(x, 10.0, z)).unwrap(),
            -DVec3::Y,
            maximum,
        )
        .unwrap()
    };
    let hit = world.query_micro_ray(down(40.0, 40.0, 100.0)).unwrap();
    assert_eq!(hit.position, DVec3::new(40.0, 0.0, 40.0));
    assert_eq!(hit.distance_m, 10.0);
    assert_eq!(hit.family, Uuid(7));
    assert_eq!(hit.cell, WorldCellKey::base(0, 0, 0));
    // The (0, 0) texel carries zero density; the crossing there reports no hit.
    assert!(world.query_micro_ray(down(8.0, 8.0, 100.0)).is_none());
    // The floor crossing past the ray's maximum reports no hit.
    assert!(world.query_micro_ray(down(40.0, 40.0, 5.0)).is_none());
    let level = VegetationQueryRay::new(
        WorldPosition::from_world_meters(DVec3::new(40.0, 10.0, 40.0)).unwrap(),
        DVec3::X,
        100.0,
    )
    .unwrap();
    assert!(world.query_micro_ray(level).is_none());
}

#[test]
fn source_budget_limits_admission_without_losing_demand() {
    let (mut world, _, _) = fixture();
    world.budgets.physics = 1;
    world.update_source(source()).unwrap();
    let report = world.residency_report().unwrap();
    assert_eq!(report.source_count, 1);
    assert!(report.requested_bytes[ResidencyFacet::Physics as usize] > 1);
    assert_eq!(report.resident_bytes[ResidencyFacet::Physics as usize], 0);
    assert!(report.pending.is_empty());
}

#[test]
fn corrupt_artifact_never_reaches_publication() {
    let (mut world, mut artifact, _) = fixture();
    world.update_source(source()).unwrap();
    let load = world
        .begin_load(
            WorldCellKey::base(0, 0, 0),
            ResidencyMask::one(ResidencyFacet::Physics),
        )
        .unwrap();
    let last = artifact.len() - 1;
    artifact[last] ^= 1;
    assert!(matches!(
        load.stage(&artifact),
        Err(Error::ArtifactHashMismatch { .. })
    ));
    assert_eq!(
        world
            .cell_snapshot(WorldCellKey::base(0, 0, 0))
            .unwrap()
            .resident_facets(),
        ResidencyMask::NONE
    );
}

#[test]
fn residency_revision_discards_late_staged_work() {
    let (mut world, artifact, _) = fixture();
    world.update_source(source()).unwrap();
    let staged = world
        .begin_load(
            WorldCellKey::base(0, 0, 0),
            ResidencyMask::one(ResidencyFacet::Physics),
        )
        .unwrap()
        .stage(&artifact)
        .unwrap();
    let mut moved = source();
    moved.revision = 2;
    moved.position = WorldPosition::from_world_meters(DVec3::new(64.0, 0.0, 0.0)).unwrap();
    world.update_source(moved).unwrap();
    assert!(!world.publish_staged(staged).unwrap());
    assert_eq!(
        world
            .cell_snapshot(WorldCellKey::base(0, 0, 0))
            .unwrap()
            .resident_facets(),
        ResidencyMask::NONE
    );
}

fn loaded_world() -> VegetationWorld {
    let (mut world, artifact, _) = fixture();
    world.update_source(source()).unwrap();
    let staged = world
        .begin_load(
            WorldCellKey::base(0, 0, 0),
            ResidencyMask::one(ResidencyFacet::Physics),
        )
        .unwrap()
        .stage(&artifact)
        .unwrap();
    assert!(world.publish_staged(staged).unwrap());
    world
}

fn catch_up_plan(target: u64, max_ticks: u32) -> EcologyCatchUp<'static> {
    catch_up_plan_with_workers(target, max_ticks, 1)
}

fn catch_up_plan_with_workers(
    target: u64,
    max_ticks: u32,
    workers: u32,
) -> EcologyCatchUp<'static> {
    static RULES: std::sync::OnceLock<BTreeMap<u64, crate::EcologySpeciesRules>> =
        std::sync::OnceLock::new();
    static RELATIONS: std::sync::OnceLock<EcologyRelations> = std::sync::OnceLock::new();
    EcologyCatchUp {
        target_tick: target,
        budget: crate::EcologyCatchUpBudget { max_ticks, workers },
        influence: crate::EcologyInfluence::default(),
        rules: RULES.get_or_init(|| BTreeMap::from([(7, crate::EcologySpeciesRules::default())])),
        relations: RELATIONS.get_or_init(EcologyRelations::new),
        weather: crate::EcologyWeather {
            water: UnitInterval::from_bits(40_000),
            warmth: UnitInterval::from_bits(45_000),
        },
    }
}

/// The phase's central claim: biology reached by running every tick as time passes and biology
/// reached by jumping time and catching up land on the same bytes.
#[test]
fn catch_up_equals_continuous_simulation() {
    let mut continuous = loaded_world();
    for tick in 1..=8_u64 {
        let plan = catch_up_plan(tick, 1);
        let report = continuous.advance_ecology(&plan).unwrap();
        assert_eq!(report.ticks_run, 1);
        assert_eq!(report.regions_caught_up, 1);
    }

    let mut caught_up = loaded_world();
    let plan = catch_up_plan(8, 8);
    let report = caught_up.advance_ecology(&plan).unwrap();
    assert_eq!(report.ticks_run, 8);
    assert_eq!(report.ticks_owed, 0);
    assert_eq!(report.regions, 1);

    assert!(
        !caught_up.persistent_state().cells().is_empty(),
        "the run committed real plant changes, so the comparison has something to compare",
    );
    assert_eq!(
        continuous
            .persistent_state()
            .ecology()
            .checkpoint_identity(),
        caught_up.persistent_state().ecology().checkpoint_identity(),
    );
    assert_eq!(
        continuous.persistent_state().canonical_bytes().unwrap(),
        caught_up.persistent_state().canonical_bytes().unwrap(),
        "both routes committed the same persistent state, byte for byte",
    );
}

/// A budget bounds the work per call. It delays when the region is readable; it never drops a
/// tick or changes where the region ends up.
#[test]
fn a_catch_up_budget_delays_readiness_without_changing_results() {
    let mut world = loaded_world();
    let plan = catch_up_plan(8, 3);
    let first = world.advance_ecology(&plan).unwrap();
    assert_eq!(first.ticks_run, 3);
    assert_eq!(first.ticks_owed, 5);
    assert_eq!(first.regions_caught_up, 0);
    assert!(
        !world.simulation_facet_is_settled(
            WorldCellKey::base(0, 0, 0),
            crate::EcologyInfluence::default()
        ),
        "a resident region behind world time is mid-catch-up, so it publishes for no facet",
    );

    let second = world.advance_ecology(&plan).unwrap();
    assert_eq!(second.ticks_run, 3);
    let third = world.advance_ecology(&plan).unwrap();
    assert_eq!(third.ticks_run, 2, "the remainder, not a whole budget");
    assert_eq!(third.ticks_owed, 0);
    assert_eq!(third.regions_caught_up, 1);
    assert!(
        world.simulation_facet_is_settled(
            WorldCellKey::base(0, 0, 0),
            crate::EcologyInfluence::default()
        ),
        "reaching world time settles the cell's biology again",
    );

    let mut unbudgeted = loaded_world();
    let whole = catch_up_plan(8, 64);
    unbudgeted.advance_ecology(&whole).unwrap();
    assert_eq!(
        world.persistent_state().canonical_bytes().unwrap(),
        unbudgeted.persistent_state().canonical_bytes().unwrap(),
        "three budgeted calls and one unbudgeted call agree",
    );
}

/// World time advances whether or not anything is loaded, but a region whose cells are not
/// resident owes its ticks rather than running them against absent neighbours.
#[test]
fn a_region_awaiting_residency_owes_its_ticks() {
    let (mut world, artifact, _) = fixture();
    let plan = catch_up_plan(5, 16);
    let report = world.advance_ecology(&plan).unwrap();
    assert_eq!(report.regions_awaiting_residency, 1);
    assert_eq!(report.ticks_run, 0);
    assert_eq!(
        report.ticks_awaiting_residency, 5,
        "the ground is not loaded, so its ticks are owed rather than absent",
    );
    assert_eq!(
        report.ticks_owed, 0,
        "and they are not actionable arrears: no budget can spend them until it loads",
    );
    assert_eq!(report.world_tick, 5);
    assert_eq!(world.persistent_state().ecology().clock().tick(), 5);

    // Loading the cell lets the same call finish the owed ticks, reaching the state a world
    // that never unloaded would hold.
    world.update_source(source()).unwrap();
    let staged = world
        .begin_load(
            WorldCellKey::base(0, 0, 0),
            ResidencyMask::one(ResidencyFacet::Physics),
        )
        .unwrap()
        .stage(&artifact)
        .unwrap();
    assert!(world.publish_staged(staged).unwrap());
    let after = world.advance_ecology(&plan).unwrap();
    assert_eq!(after.ticks_run, 5);
    assert_eq!(after.regions_caught_up, 1);

    let mut resident_throughout = loaded_world();
    let same = catch_up_plan(5, 16);
    resident_throughout.advance_ecology(&same).unwrap();
    assert_eq!(
        world.persistent_state().canonical_bytes().unwrap(),
        resident_throughout
            .persistent_state()
            .canonical_bytes()
            .unwrap(),
        "the residency path taken to a tick does not change the tick's result",
    );
}

/// Regions are disjoint and a region tick is a pure function of immutable state, so how many
/// threads ran the rules cannot reach the committed bytes. Eight distant cells give eight regions
/// and three workers shard them unevenly, so results come back interleaved and the commit order has
/// to be restored from the region order rather than taken from the completion order.
#[test]
fn catch_up_is_identical_across_worker_counts() {
    let cells = [
        WorldCellKey::base(0, 0, 0),
        WorldCellKey::base(40, 0, 0),
        WorldCellKey::base(80, 0, 0),
        WorldCellKey::base(120, 0, 0),
        WorldCellKey::base(0, 0, 40),
        WorldCellKey::base(0, 0, 80),
        WorldCellKey::base(-40, 0, 0),
        WorldCellKey::base(-40, 0, -40),
    ];
    let mut single = loaded_multi_region_world(&cells);
    let mut many = loaded_multi_region_world(&cells);
    assert_eq!(
        single
            .ecology_region_standings(crate::EcologyInfluence::default())
            .unwrap()
            .len(),
        cells.len(),
        "the cells are farther apart than the influence radius, so each is its own region",
    );

    let one_worker = single
        .advance_ecology(&catch_up_plan_with_workers(6, 64, 1))
        .unwrap();
    let many_workers = many
        .advance_ecology(&catch_up_plan_with_workers(6, 64, 3))
        .unwrap();
    assert_eq!(one_worker.workers, 1, "one worker is the serial path");
    assert_eq!(
        many_workers.workers, 3,
        "three workers really ran: a call that quietly fell back to the serial path would say 1",
    );
    assert_eq!(
        one_worker,
        crate::EcologyCatchUpReport {
            workers: 1,
            ..many_workers
        },
        "the thread count is the only thing the two calls disagree on",
    );
    assert_eq!(one_worker.ticks_run, 6 * cells.len() as u64);
    assert!(
        !single.persistent_state().cells().is_empty(),
        "the run committed real plant changes, so the comparison has something to compare",
    );
    assert_eq!(
        single.persistent_state().canonical_bytes().unwrap(),
        many.persistent_state().canonical_bytes().unwrap(),
        "one worker and three workers committed the same persistent state, byte for byte",
    );
}

/// Streaming a cell in moves the ground revision the catch-up poll keys on and refreshes which
/// regions can run, without redrawing the region closure behind it — that closure is a walk of every
/// planted cell in the world, and it only changes when the planted set does.
#[test]
fn streaming_refreshes_region_residency_without_redrawing_the_closure() {
    let cells = [WorldCellKey::base(0, 0, 0), WorldCellKey::base(40, 0, 0)];
    let (mut world, artifacts) = multi_region_world(&cells);
    let influence = crate::EcologyInfluence::default();
    world.update_source(source()).unwrap();

    assert_eq!(world.ecology_region_standings(influence).unwrap().len(), 2);
    let before = memoized_partition(&world);
    assert_eq!(before.resident, [false, false]);
    let ground = world.ecology_ground_revision();
    let planted = world.ecology_planted_revision;

    let staged = world
        .begin_load(cells[0], ResidencyMask::one(ResidencyFacet::Physics))
        .unwrap()
        .stage(&artifacts[0])
        .unwrap();
    assert!(world.publish_staged(staged).unwrap());
    assert_ne!(
        world.ecology_ground_revision(),
        ground,
        "the catch-up poll has to notice a cell arriving",
    );
    assert_eq!(
        world.ecology_planted_revision, planted,
        "streaming changes nothing about which cells carry plants",
    );

    assert_eq!(world.ecology_region_standings(influence).unwrap().len(), 2);
    let after = memoized_partition(&world);
    assert!(
        std::sync::Arc::ptr_eq(&before.closure, &after.closure),
        "the region closure is carried forward across a residency change",
    );
    let loaded = after.region_of(cells[0]).expect("the cell is planted");
    assert!(after.resident[loaded], "the loaded region can run now");
    assert!(
        after.resident.iter().filter(|resident| **resident).count() == 1,
        "and the region whose cell never loaded still cannot",
    );
}

fn memoized_partition(world: &VegetationWorld) -> std::sync::Arc<super::EcologyRegionPartition> {
    world
        .ecology_partition
        .read()
        .expect("the partition memo is only locked to read or replace it")
        .clone()
        .expect("a reader populated the memo")
}

/// A region that cannot run is not mid-catch-up. Its resident cells keep publishing the last
/// generation they committed, because that is the newest one there will be until the ground they
/// depend on loads. Gating them on world time instead would delete collision and navigation for
/// every loaded cell whose planted neighbours sit outside the streaming window.
#[test]
fn a_stalled_region_publishes_its_last_committed_generation() {
    let cells = [WorldCellKey::base(0, 0, 0), WorldCellKey::base(1, 0, 0)];
    let (mut world, artifacts) = multi_region_world(&cells);
    world.update_source(source()).unwrap();
    let staged = world
        .begin_load(cells[0], ResidencyMask::one(ResidencyFacet::Physics))
        .unwrap()
        .stage(&artifacts[0])
        .unwrap();
    assert!(world.publish_staged(staged).unwrap());

    let influence = crate::EcologyInfluence::default();
    assert_eq!(
        world.ecology_region_standings(influence).unwrap().len(),
        1,
        "adjacent planted cells are one region, so the absent one blocks the loaded one",
    );

    let report = world.advance_ecology(&catch_up_plan(5, 16)).unwrap();
    assert_eq!(report.regions_awaiting_residency, 1);
    assert_eq!(report.ticks_run, 0);
    assert_eq!(report.ticks_awaiting_residency, 5);
    assert_eq!(
        report.ticks_owed, 0,
        "no budget can spend them, so they are not work to poll for",
    );
    assert_eq!(world.persistent_state().ecology().cell_tick(cells[0]), 0);
    assert!(
        world.simulation_facet_is_settled(cells[0], influence),
        "the loaded cell is behind a world time it cannot reach, and still publishes",
    );
}

/// A budget is spent one round at a time across the regions that owe a tick, so a region cannot
/// starve another out of the call — and the remainder is still owed, not lost.
#[test]
fn a_budget_is_spent_a_round_at_a_time_across_regions() {
    let cells = [WorldCellKey::base(0, 0, 0), WorldCellKey::base(40, 0, 0)];
    let mut world = loaded_multi_region_world(&cells);
    let report = world.advance_ecology(&catch_up_plan(4, 2)).unwrap();
    assert_eq!(report.ticks_run, 2);
    assert_eq!(report.ticks_owed, 6, "two regions owe three ticks each");
    let ecology = world.persistent_state().ecology();
    for cell in cells {
        assert_eq!(
            ecology.cell_tick(cell),
            1,
            "both regions advanced once rather than one taking the whole budget",
        );
    }
}
