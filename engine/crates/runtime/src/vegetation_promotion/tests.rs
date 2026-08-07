use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use super::*;
use saffron_core::Uuid;
use saffron_spatial::{
    DecisionScalar, QuantizedLocalPosition, ResidencyFacet, ResidencyMask, SourceLevel,
    SpatialSource, SpatialSourceId, WorldBounds, WorldCellKey,
};
use saffron_vegetation::{PlantCollisionProxy, PlantCollisionShape};

fn plant(byte: u8) -> PlantId {
    PlantId::explicit([byte; 16]).expect("plant id")
}

fn scalar(value: f64) -> DecisionScalar {
    DecisionScalar::from_f64(value).expect("finite decision scalar")
}

fn snapshot_fixture(plant: PlantId) -> VegetationPlantSnapshot {
    VegetationPlantSnapshot {
        variation: 0,
        plant,
        ecology_tick: 12,
        handle: saffron_vegetation::VegetationPlantHandle {
            plant,
            generation: saffron_vegetation::VegetationCellGenerationId {
                cell: saffron_spatial::WorldCellKey::base(0, 0, 0),
                generation: 3,
            },
        },
        position: WorldPosition::origin(),
        orientation: QuantizedOrientation::identity(),
        scale: [scalar(1.0); 3],
        bounds: saffron_spatial::WorldBounds::new([0, 0, 0], [1, 1, 1]).expect("bounds"),
        family: Uuid(4),
        tags: Vec::new(),
        lifecycle: PlantLifecycle::Mature,
        phenotype: 1,
        interaction_policy: saffron_vegetation::InteractionPolicy::Harvestable,
        health: UnitInterval::ONE,
        moisture: UnitInterval::ZERO,
        fuel: UnitInterval::ZERO,
        ignited: false,
        linear_velocity: [DecisionScalar::default(); 3],
        angular_velocity: [DecisionScalar::default(); 3],
        provenance: None,
    }
}

fn family_fixture() -> saffron_vegetation::PlantFamilyAsset {
    saffron_vegetation::PlantFamilyAsset {
        role: saffron_vegetation::PlantFamilyRole::Family,
        modules: Vec::new(),
        module_recursion_limit: saffron_vegetation::MAX_PLANT_MODULE_RECURSION,
        version: saffron_vegetation::PLANT_ASSET_VERSION,
        id: Uuid(4),
        name: "fixture".to_owned(),
        tags: Vec::new(),
        source: saffron_vegetation::PlantFamilySource::Native {
            graph: saffron_vegetation::BotanicalGraphDocument::sapling(0x5a11),
            grafts: Vec::new(),
        },
        parts: vec![saffron_vegetation::PlantPart {
            id: 12,
            parent: None,
            semantic: saffron_vegetation::PlantPartSemantic::Trunk,
            material_slot: 0,
            sources: Vec::new(),
        }],
        dimensions: saffron_vegetation::PlantDimensions {
            height: scalar(4.0),
            trunk_radius: scalar(0.3),
            crown_radius: [scalar(1.0); 2],
            root_radius: [scalar(0.5); 2],
            local_bounds_min: [scalar(-1.0), scalar(0.0), scalar(-1.0)],
            local_bounds_max: [scalar(1.0), scalar(4.0), scalar(1.0)],
        },
        material_slots: vec![Uuid(11)],
        spines: Vec::new(),
        mechanics: saffron_vegetation::MechanicalResponse {
            stiffness: scalar(1.0),
            damping: UnitInterval::from_bits(1000),
            drag: scalar(0.5),
            flutter: scalar(0.2),
            bend_limit: UnitInterval::from_bits(2000),
            damage_threshold: scalar(0.5),
            break_threshold: scalar(0.9),
        },
        variations: vec![saffron_vegetation::PlantVariation {
            id: 0,
            name: "Default".to_owned(),
            sources: vec![saffron_vegetation::native_variation_source_id(0)],
            active_parts: Vec::new(),
        }],
        phenotypes: vec![saffron_vegetation::PlantPhenotype {
            id: 0,
            role: saffron_vegetation::PhenotypeRole::Healthy,
            response: saffron_vegetation::PhenotypeResponse::default(),
            variation: 0,
            material_remap: Vec::new(),
            active_parts: Vec::new(),
        }],
        collision_proxies: vec![proxy(PlantCollisionShape::Capsule, 0.3, 2.0)],
        navigation_proxies: Vec::new(),
        interaction_policy: saffron_vegetation::InteractionPolicy::Harvestable,
        habitat: None,
        ecology: saffron_vegetation::PlantEcologyDeclaration::default(),
    }
}

fn proxy(shape: PlantCollisionShape, radius: f64, height: f64) -> PlantCollisionProxy {
    PlantCollisionProxy {
        id: 1,
        shape,
        part: 12,
        center: [scalar(0.0), scalar(height), scalar(0.0)],
        dimensions: [scalar(radius), scalar(height), scalar(radius)],
        breakable: false,
    }
}

// Jolt's `Factory::sInstance` is a process-global the world bring-up touches through `World::new`;
// serialize the tests that build one so they never race it.
static JOLT_GLOBAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Acquires the Jolt serialization lock, recovering from poisoning: the lock guards only the
/// global init race, so a panicking test leaves no shared state behind to corrupt.
fn jolt_guard() -> std::sync::MutexGuard<'static, ()> {
    JOLT_GLOBAL
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
}

static SCRATCH_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// A temporary asset root that goes away with the fixture that made it.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let sequence = SCRATCH_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "saffron-promotion-{tag}-{}-{nanos}-{sequence}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("create scratch directory");
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A whole play world one plant lives in: a `.splant` family the asset server can resolve, a
/// cooked cell holding one mature individual of it, and the scene and Jolt world a promotion
/// spawns its entity view into. Enough to drive `advance` exactly as the session does.
struct PromotionFixture {
    _scratch: Scratch,
    _jolt: std::sync::MutexGuard<'static, ()>,
    assets: AssetServer,
    families: PlantFamilyCache,
    scene: Scene,
    physics: World,
    vegetation: VegetationWorld,
    plant: PlantId,
}

impl PromotionFixture {
    fn new(tag: &str) -> Self {
        let jolt = jolt_guard();
        let scratch = Scratch::new(tag);
        let mut assets = AssetServer::new(scratch.0.join("assets"));
        let family = saffron_assets::save_plant_family_asset(
            &mut assets,
            family_fixture(),
            "Fixture",
            "plants",
        )
        .expect("save the plant family");

        let cell = WorldCellKey::base(0, 0, 0);
        let point = macro_point(cell, family);
        let (bytes, row) = cell_artifact(&point);
        let platform = platform_profile();
        let mut manifest = saffron_vegetation::VegetationBaseManifest::current(
            Uuid(1),
            Uuid(2),
            ContentHash::new([3; 32]),
            saffron_vegetation::CookVersionSet::current(),
            platform,
            ContentHash::new([4; 32]),
        );
        manifest
            .plants
            .push(saffron_vegetation::VegetationManifestPlant {
                family,
                tags: Vec::new(),
                source_hash: ContentHash::new([5; 32]),
                artifact_hash: ContentHash::new([6; 32]),
                local_bounds_min: [scalar(-1.0); 3],
                local_bounds_max: [scalar(1.0); 3],
                variation_count: 1,
                phenotype_count: 1,
                ecology: saffron_vegetation::PlantEcologyDeclaration::default(),
            });
        manifest.cells.push(row);
        let mut vegetation = VegetationWorld::new(
            manifest,
            saffron_vegetation::VegetationResidencyBudgets::UNLIMITED,
        )
        .expect("a world over the cooked cell");
        vegetation
            .update_source(SpatialSource {
                id: SpatialSourceId(1),
                revision: 1,
                position: WorldPosition::new(cell, QuantizedLocalPosition::default())
                    .expect("source position"),
                velocity_mps: DVec3::ZERO,
                prediction_seconds: 0.0,
                levels: vec![SourceLevel {
                    level: 0,
                    load_radius_cells: 0,
                    cleanup_radius_cells: 1,
                }],
                facets: ResidencyMask::one(ResidencyFacet::Physics),
                priority: 10,
            })
            .expect("a resident source");
        let staged = vegetation
            .begin_load(cell, ResidencyMask::one(ResidencyFacet::Physics))
            .expect("load token")
            .stage(&bytes)
            .expect("stage the artifact");
        assert!(vegetation.publish_staged(staged).expect("publish"));

        Self {
            _scratch: scratch,
            _jolt: jolt,
            assets,
            families: PlantFamilyCache::default(),
            scene: Scene::new(),
            physics: World::new().expect("a Jolt world"),
            vegetation,
            plant: point.id,
        }
    }

    /// One fixed synchronization point.
    fn advance(&mut self, promotion: &mut VegetationPromotion) {
        promotion.advance(
            &mut self.vegetation,
            &mut self.scene,
            &self.assets,
            &mut self.families,
            Some(&mut self.physics),
        );
    }

    fn snapshot(&self) -> VegetationPlantSnapshot {
        self.vegetation
            .find_plant(self.plant)
            .expect("plant query")
            .expect("the plant is resident")
    }

    fn live_view(&self, promotion: &VegetationPromotion) -> Uuid {
        match promotion.state(self.plant) {
            PlantPromotionState::Promoted { entity } => entity,
            other => panic!("the plant has no live view: {other:?}"),
        }
    }
}

fn platform_profile() -> saffron_vegetation::CookPlatformProfile {
    saffron_vegetation::CookPlatformProfile {
        target: "test-target".to_owned(),
        content_profile: "portable-vulkan".to_owned(),
        toolchain: "rust-test".to_owned(),
        features: vec!["test".to_owned()],
    }
}

/// One mature individual of `family`, placed inside `cell`.
fn macro_point(cell: WorldCellKey, family: Uuid) -> saffron_vegetation::PlantPoint {
    let min = cell.bounds().min_ticks();
    saffron_vegetation::PlantPoint {
        id: plant(1),
        owner: cell,
        position: WorldPosition::new(
            cell,
            QuantizedLocalPosition::new([10, 20, 30]).expect("local position"),
        )
        .expect("world position"),
        orientation: QuantizedOrientation::identity(),
        scale: [DecisionScalar::from_bits(65_536); 3],
        bounds: WorldBounds::new(min, [min[0] + 100, min[1] + 100, min[2] + 100]).expect("bounds"),
        family,
        lifecycle: PlantLifecycle::Mature,
        variation: 0,
        phenotype: 0,
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
        flags: saffron_vegetation::PlantFlags::AUTHORED,
        interaction_policy: saffron_vegetation::InteractionPolicy::Harvestable,
        provenance: 0,
        attachment: None,
        surface_projection: [DecisionScalar::from_bits(0); 3],
    }
}

/// The cell's artifact bytes and the manifest row naming them.
fn cell_artifact(
    point: &saffron_vegetation::PlantPoint,
) -> (Vec<u8>, saffron_vegetation::VegetationManifestCell) {
    let columns = saffron_vegetation::PlantPointColumns::from_points(vec![point.clone()])
        .expect("canonical point columns");
    let sections = vec![
        saffron_vegetation::VegetationCellSection::new(
            saffron_vegetation::VegetationCellSectionKind::MacroPoints,
            columns.canonical_bytes().expect("column bytes"),
        ),
        saffron_vegetation::VegetationCellSection::new(
            saffron_vegetation::VegetationCellSectionKind::CollisionInputs,
            [b"SVEGCOL1".as_slice(), &0_u64.to_be_bytes()].concat(),
        ),
    ];
    let bytes = saffron_vegetation::write_vegetation_cell_artifact(
        saffron_vegetation::VegetationCellArtifactHeader {
            cell: point.owner,
            cook_key: ContentHash::new([8; 32]),
            platform_profile: platform_profile().identity().expect("platform identity"),
        },
        &sections,
    )
    .expect("artifact bytes");
    let index = saffron_vegetation::VegetationCellArtifactIndex::open(
        &bytes,
        saffron_vegetation::VEGETATION_ARTIFACT_DECODE_LIMITS,
    )
    .expect("artifact index");
    let row = saffron_vegetation::VegetationManifestCell {
        cell: point.owner,
        bounds: point.owner.bounds(),
        artifact_hash: ContentHash::of(&bytes),
        payload_hash: index.payload_hash,
        dependencies: Vec::new(),
        species_counts: vec![saffron_vegetation::ManifestSpeciesCount {
            family: point.family,
            macro_count: 1,
            micro_count: 0,
        }],
        macro_count: 1,
        micro_count: 0,
        resident_memory_bytes: index
            .sections
            .iter()
            .map(|section| section.decoded_size)
            .sum(),
        stored_bytes: bytes.len() as u64,
        estimate: saffron_vegetation::CookWorkEstimate::default(),
        actual: saffron_vegetation::CookWorkActual::default(),
        sections: index
            .sections
            .iter()
            .map(|section| saffron_vegetation::ManifestCellSection {
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

#[test]
fn requests_move_through_the_lifecycle_and_reject_repeats() {
    let mut promotion = VegetationPromotion::default();
    let tree = plant(3);
    assert_eq!(promotion.state(tree), PlantPromotionState::Bulk);

    promotion.request_promotion(tree).expect("promote request");
    assert_eq!(promotion.state(tree), PlantPromotionState::Promoting);
    // A second promotion of the same plant would mean two owners.
    assert!(promotion.request_promotion(tree).is_err());

    // Demoting an uncommitted promotion cancels it outright.
    promotion.request_demotion(tree).expect("cancel request");
    assert_eq!(promotion.state(tree), PlantPromotionState::Bulk);
    assert!(promotion.request_demotion(tree).is_err());
}

#[test]
fn demotion_of_a_live_view_is_cancelled_by_a_new_promotion() {
    let mut promotion = VegetationPromotion::default();
    let tree = plant(4);
    let entity = Uuid(77);
    promotion
        .states
        .insert(tree, PlantPromotionState::Promoted { entity });

    promotion.request_demotion(tree).expect("demote request");
    assert_eq!(
        promotion.state(tree),
        PlantPromotionState::Demoting { entity }
    );
    // Re-promoting before the transition commits keeps the same live entity.
    promotion.request_promotion(tree).expect("re-promote");
    assert_eq!(
        promotion.state(tree),
        PlantPromotionState::Promoted { entity }
    );
}

#[test]
fn felling_requests_queue_once_and_products_carry_no_plant_identity() {
    let mut promotion = VegetationPromotion::default();
    let tree = plant(5);
    promotion.request_felling(tree).expect("felling request");
    // A second request would fell the same plant twice.
    assert!(promotion.request_felling(tree).is_err());
    // Felling is an operation, not a state: the plant is still bulk until it commits.
    assert_eq!(promotion.state(tree), PlantPromotionState::Bulk);

    // The product entity a felling spawns is deliberately not the plant.
    let mut scene = Scene::new();
    let snapshot = snapshot_fixture(tree);
    let family = family_fixture();
    let product = spawn_product(&mut scene, tree, &snapshot, &family).expect("product");
    assert!(
        !scene.has_component::<PlantOrigin>(product),
        "a felled log must never resolve as the rooted plant it came from"
    );
    assert!(
        !scene.has_component::<PlantVitals>(product),
        "the product carries no plant biology — the stump keeps it"
    );
    // It is an ordinary dynamic entity that renders the same mass.
    assert!(scene.has_component::<MeshComponent>(product));
    assert!(scene.has_component::<Rigidbody>(product));
}

#[test]
fn primary_collider_picks_the_largest_scaled_proxy() {
    let proxies = [
        proxy(PlantCollisionShape::Capsule, 0.2, 2.0),
        proxy(PlantCollisionShape::Box, 0.05, 0.05),
        proxy(PlantCollisionShape::ConvexHull, 5.0, 5.0),
    ];
    let (shape, half_extents, offset) =
        primary_collider(&proxies, Vec3::new(2.0, 1.0, 2.0)).expect("a primary shape");
    // The capsule wins; a convex hull has no cooked geometry so it never competes.
    assert_eq!(shape, Shape::Capsule);
    assert!((half_extents.x - 0.4).abs() < 1e-4);
    assert!((half_extents.y - 2.0).abs() < 1e-4);
    // The proxy centre scales per axis into the collider's local offset.
    assert!((offset.y - 2.0).abs() < 1e-4);
}

#[test]
fn hull_only_families_get_no_primary_collider() {
    let proxies = [proxy(PlantCollisionShape::ConvexHull, 1.0, 1.0)];
    assert!(primary_collider(&proxies, Vec3::ONE).is_none());
}

#[test]
fn orientation_round_trips_through_quantization() {
    let rotation = Quat::from_rotation_y(0.75).normalize();
    let quantized = quantize_orientation(rotation).expect("quantized orientation");
    let restored = orientation_quat(quantized);
    assert!(
        restored.dot(rotation).abs() > 0.9999,
        "quantization preserved the orientation ({restored:?} vs {rotation:?})"
    );
}

/// The write-back carries what gameplay did to the view and nothing else. A view nobody
/// touched writes back no biology at all — repeating the snapshot promotion copied out would
/// overwrite whatever the macro row accrued while the view stood.
#[test]
fn write_back_carries_only_what_the_view_changed() {
    let tree = plant(6);
    let baseline = snapshot_vitals(&snapshot_fixture(tree));
    assert!(
        vitals_mutations(tree, baseline, baseline).is_empty(),
        "an untouched view has nothing to say about the plant's biology"
    );

    let mut damaged = baseline;
    damaged.health = 0.25;
    let mutations = vitals_mutations(tree, damaged, baseline);
    assert_eq!(mutations.len(), 1);
    let VegetationMutation::StateOverride {
        health,
        moisture,
        fuel,
        lifecycle,
        ..
    } = &mutations[0]
    else {
        panic!("damage writes back a state override, got {mutations:?}");
    };
    assert_eq!(*health, Some(UnitInterval::from_f64(0.25).expect("unit")));
    assert_eq!(*moisture, None, "moisture nobody moved stays the row's");
    assert_eq!(*fuel, None);
    assert_eq!(*lifecycle, None, "the stage did not move");

    // A change below one quantum of the reducer's vocabulary is not a change. The nudge is far
    // wider than one `f32` step at this magnitude, so the two values genuinely differ and only
    // the quantization keeps them in the same bucket.
    let mut nudged = baseline;
    nudged.health -= 1e-6;
    assert_ne!(nudged.health, baseline.health);
    assert_eq!(
        UnitInterval::from_f64(f64::from(nudged.health)).expect("unit"),
        UnitInterval::from_f64(f64::from(baseline.health)).expect("unit")
    );
    assert!(vitals_mutations(tree, nudged, baseline).is_empty());

    // Biological age travels on the lifecycle transition, which is the only mutation carrying
    // one, even when the stage itself held still.
    let mut aged = baseline;
    aged.ecology_tick += 7;
    let mutations = vitals_mutations(tree, aged, baseline);
    assert_eq!(mutations.len(), 1);
    assert!(matches!(
        mutations[0],
        VegetationMutation::LifecycleTransition {
            ecology_tick,
            to: PlantLifecycle::Mature,
            ..
        } if ecology_tick == baseline.ecology_tick + 7
    ));
}

/// The view is where a promoted plant's biology lives, so the promotion authority is what
/// reads and writes it — a plant with no live view has none to reach.
#[test]
fn vitals_are_readable_and_writable_only_through_a_live_view() {
    let mut scene = Scene::new();
    let tree = plant(7);
    let snapshot = snapshot_fixture(tree);
    let family = family_fixture();
    let entity = spawn_view(&mut scene, tree, &snapshot, &family).expect("view");
    let uuid = scene.component::<IdComponent>(entity).expect("id").id;

    let mut promotion = VegetationPromotion::default();
    assert!(promotion.vitals(&scene, tree).is_none());
    assert!(
        promotion
            .set_vitals(&mut scene, tree, PlantVitals::default())
            .is_err()
    );

    promotion
        .states
        .insert(tree, PlantPromotionState::Promoted { entity: uuid });
    let live = promotion.vitals(&scene, tree).expect("live vitals");
    assert_eq!(live, snapshot_vitals(&snapshot));

    let damaged = PlantVitals {
        health: 0.5,
        ..live
    };
    promotion
        .set_vitals(&mut scene, tree, damaged)
        .expect("gameplay writes the view");
    assert_eq!(promotion.vitals(&scene, tree), Some(damaged));
}

/// A rebind onto a different generation abandons every view instead of demoting it. The plants
/// those views describe belong to a base that is gone, so a write-back would record their state
/// against a world nobody observed — and the entity, its body, and the suppression it stood for
/// all go with it, leaving the plant its bulk representation and no second owner.
#[test]
fn abandoning_views_drops_them_whole_and_writes_no_state_back() {
    let mut scene = Scene::new();
    let empty_scene = scene.len();
    let tree = plant(11);
    let snapshot = snapshot_fixture(tree);
    let entity = spawn_view(&mut scene, tree, &snapshot, &family_fixture()).expect("view");
    let uuid = scene.component::<IdComponent>(entity).expect("id").id;
    assert!(scene.len() > empty_scene);

    let mut promotion = VegetationPromotion::default();
    promotion
        .states
        .insert(tree, PlantPromotionState::Promoted { entity: uuid });
    promotion.refresh_counts();
    assert_eq!(promotion.report().promoted, 1);

    // Gameplay moved the view's biology, so there is state a write-back would carry.
    let live = promotion.vitals(&scene, tree).expect("live vitals");
    promotion
        .set_vitals(
            &mut scene,
            tree,
            PlantVitals {
                health: 0.25,
                ..live
            },
        )
        .expect("gameplay writes the view");
    let mut world = manifest_only_world();
    let untouched = world
        .persistent_state()
        .canonical_bytes()
        .expect("state bytes");

    promotion.abandon(&mut scene, None);

    assert_eq!(promotion.report().promoted, 0);
    assert_eq!(promotion.state(tree), PlantPromotionState::Bulk);
    assert!(
        scene.find_entity_by_uuid(uuid).is_none(),
        "the view died with the generation it described"
    );
    assert_eq!(scene.len(), empty_scene, "and took its entity with it");
    assert_eq!(
        world
            .persistent_state()
            .canonical_bytes()
            .expect("state bytes"),
        untouched,
        "abandoning is not demoting: nothing is written back"
    );
    assert_eq!(
        promotion
            .flush_state(&scene, &mut world, None)
            .expect("flush"),
        0,
        "and a save barrier taken afterwards has no view left to reduce"
    );
    assert_eq!(
        world
            .persistent_state()
            .canonical_bytes()
            .expect("state bytes"),
        untouched
    );
}

/// A world bound to a generation that names no cells: enough to observe what a write-back
/// records, without an artifact to stage.
fn manifest_only_world() -> VegetationWorld {
    VegetationWorld::new(
        saffron_vegetation::VegetationBaseManifest::current(
            Uuid(1),
            Uuid(2),
            ContentHash::new([3; 32]),
            saffron_vegetation::CookVersionSet::current(),
            saffron_vegetation::CookPlatformProfile {
                target: "test-target".to_owned(),
                content_profile: "portable-vulkan".to_owned(),
                toolchain: "rust-test".to_owned(),
                features: vec!["test".to_owned()],
            },
            ContentHash::new([4; 32]),
        ),
        saffron_vegetation::VegetationResidencyBudgets::default(),
    )
    .expect("a world over a cell-less generation")
}

/// Momentum crosses the reducer in the reducer's units and comes back in Jolt's. The write-back
/// and the restore are one round trip: what `origin_state` records off a live body is exactly
/// what `restore_momentum` puts back, so a demote/re-promote cycle continues the motion.
#[test]
fn momentum_round_trips_between_the_live_body_and_the_reducer() {
    let _jolt = jolt_guard();
    let mut world = World::new().expect("a Jolt world");
    let mut scene = Scene::new();
    let tree = plant(21);
    let snapshot = snapshot_fixture(tree);
    let entity = spawn_view(&mut scene, tree, &snapshot, &family_fixture()).expect("view");
    scene.relink_hierarchy();
    scene.update_world_transforms();
    let uuid = scene.component::<IdComponent>(entity).expect("id").id;
    let mut cook = |_: Uuid| Err("analytic shapes only".to_owned());
    world
        .add_entity_body(&scene, entity, &mut cook)
        .expect("a dynamic body for the view");

    let linear = Vec3::new(1.5, -0.75, 2.25);
    let angular = Vec3::new(0.5, -1.25, 0.25);
    let stored = VegetationPlantSnapshot {
        linear_velocity: quantize_vec3(linear * saffron_physics::FIXED_STEP).expect("linear"),
        angular_velocity: quantize_vec3(
            angular * saffron_physics::FIXED_STEP / std::f32::consts::TAU,
        )
        .expect("angular"),
        ..snapshot
    };
    restore_momentum(&mut world, uuid, &stored);
    let restored_linear = world.body_linear_velocity(uuid);
    let restored_angular = world.body_angular_velocity(uuid);
    // One Q15.16 quantum in the stored unit, expressed back in Jolt's per-second units: that
    // is the whole error budget the round trip has.
    let quantum = 1.0 / (65_536.0 * saffron_physics::FIXED_STEP);
    assert!(
        (restored_linear - linear).abs().max_element() <= quantum,
        "the stored linear velocity came back: {restored_linear} vs {linear}"
    );
    assert!(
        (restored_angular - angular).abs().max_element() <= quantum * std::f32::consts::TAU,
        "and the angular one too: {restored_angular} vs {angular}"
    );

    // Reading the same live body back out writes the same quantized payload, so a plant that
    // never stops moving never loses or gains speed across the transition.
    let written = origin_state(&scene, entity, uuid, Some(&world)).expect("origin state");
    assert_eq!(written.linear_velocity, stored.linear_velocity);
    assert_eq!(written.angular_velocity, stored.angular_velocity);
}

/// A view survives only while the promotion authority is still the plant's simulation owner.
/// Another authority claiming the plant — a recook's delta restore, an editor undo, a network
/// snapshot — takes the view down without a write-back, and the plant keeps exactly one owner:
/// the bulk representation the view suppressed comes back in the same pass the entity dies.
#[test]
fn a_foreign_claim_releases_the_view_and_the_authority_s_own_claim_keeps_it() {
    let mut fixture = PromotionFixture::new("foreign-claim");
    let tree = fixture.plant;
    let mut promotion = VegetationPromotion::default();
    promotion.request_promotion(tree).expect("promote request");
    fixture.advance(&mut promotion);
    let uuid = fixture.live_view(&promotion);
    assert!(
        fixture.vegetation.is_bulk_suppressed(tree),
        "the view is the plant's only representation while it stands"
    );

    let snapshot = fixture.snapshot();
    let origin = PromotionOriginState {
        position: snapshot.position,
        orientation: snapshot.orientation,
        scale: snapshot.scale,
        linear_velocity: [scalar(0.0); 3],
        angular_velocity: [scalar(0.0); 3],
    };
    let claim = |authority: u128, key: u128| VegetationMutationRecord {
        header: MutationHeader {
            cell: snapshot.position.cell(),
            transaction: key,
            authority,
            logical_tick: key as u64,
            idempotency_key: key,
            base_revision: None,
        },
        mutation: VegetationMutation::PromotionOriginState {
            plant: tree,
            state: origin,
        },
    };

    // The promotion authority's own write-back keeps the view standing.
    fixture
        .vegetation
        .apply_confirmed_mutations(&[claim(PROMOTION_AUTHORITY, 31)])
        .expect("the authority's own claim");
    fixture.advance(&mut promotion);
    assert_eq!(
        promotion.state(tree),
        PlantPromotionState::Promoted { entity: uuid },
        "the view still owns a plant nobody else claimed"
    );
    assert_eq!(promotion.report().released_total, 0);
    assert!(fixture.vegetation.is_bulk_suppressed(tree));

    // A different authority claiming the same plant takes it away.
    fixture
        .vegetation
        .apply_confirmed_mutations(&[claim(0x9999, 32)])
        .expect("a foreign claim");
    fixture.advance(&mut promotion);
    assert_eq!(promotion.state(tree), PlantPromotionState::Bulk);
    assert_eq!(promotion.report().released_total, 1);
    assert!(
        fixture.scene.find_entity_by_uuid(uuid).is_none(),
        "the entity died with the claim it stood on"
    );
    assert!(
        !fixture.vegetation.is_bulk_suppressed(tree),
        "and the bulk row is the plant's owner again — never neither, never both"
    );
}

/// Momentum is a round trip, not a one-way write. A view that is moving when it demotes hands its
/// velocity to the persistent delta; the cell republishes it, and the next promotion puts it back
/// on the fresh body — so a demote/re-promote cycle continues the motion instead of restarting
/// the plant from rest.
#[test]
fn a_re_promoted_plant_continues_the_motion_its_view_ended_with() {
    let mut fixture = PromotionFixture::new("momentum-cycle");
    let tree = fixture.plant;
    let mut promotion = VegetationPromotion::default();
    promotion.request_promotion(tree).expect("promote request");
    fixture.advance(&mut promotion);
    let first = fixture.live_view(&promotion);
    assert_eq!(
        fixture.physics.body_linear_velocity(first),
        Vec3::ZERO,
        "a plant that never simulated starts at rest"
    );

    let linear = Vec3::new(1.5, -0.75, 2.25);
    let angular = Vec3::new(0.5, -1.25, 0.25);
    fixture.physics.set_linear_velocity(first, linear);
    fixture.physics.set_angular_velocity(first, angular);

    promotion.request_demotion(tree).expect("demote request");
    fixture.advance(&mut promotion);
    assert_eq!(promotion.state(tree), PlantPromotionState::Bulk);
    let stored = fixture.snapshot();
    assert_ne!(
        stored.linear_velocity,
        [DecisionScalar::default(); 3],
        "the write-back read the live body, not the transform alone"
    );

    promotion.request_promotion(tree).expect("re-promote");
    fixture.advance(&mut promotion);
    let second = fixture.live_view(&promotion);
    assert_ne!(second, first, "a re-promotion builds a new view");

    let restored_linear = fixture.physics.body_linear_velocity(second);
    let restored_angular = fixture.physics.body_angular_velocity(second);
    // One Q15.16 quantum in the stored unit, expressed back in Jolt's per-second units: the whole
    // error budget the reducer round trip has.
    let quantum = 1.0 / (65_536.0 * saffron_physics::FIXED_STEP);
    assert!(
        (restored_linear - linear).abs().max_element() <= quantum,
        "the fresh body carries the momentum the old view ended with: \
         {restored_linear} vs {linear}"
    );
    assert!(
        (restored_angular - angular).abs().max_element() <= quantum * std::f32::consts::TAU,
        "and its spin too: {restored_angular} vs {angular}"
    );
}

#[test]
fn content_derived_keys_are_stable_and_non_zero() {
    let state = PromotionOriginState {
        position: WorldPosition::origin(),
        orientation: QuantizedOrientation::identity(),
        scale: [scalar(1.0); 3],
        linear_velocity: [scalar(0.5); 3],
        angular_velocity: [scalar(0.1); 3],
    };
    let first = leading_u128(ContentHash::of(&demotion_digest(plant(9), &state)).bytes());
    let again = leading_u128(ContentHash::of(&demotion_digest(plant(9), &state)).bytes());
    let other = leading_u128(ContentHash::of(&demotion_digest(plant(10), &state)).bytes());
    assert_eq!(first, again, "identical contents replay as one transaction");
    assert_ne!(first, other, "a different plant is a different transaction");
    assert_ne!(first, 0, "a zero transaction id is rejected by the reducer");
}
