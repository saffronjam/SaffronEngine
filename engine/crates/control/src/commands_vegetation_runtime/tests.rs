use saffron_core::Uuid;
use saffron_protocol::{
    InteractionPolicyDto, PlantLifecycleDto, Uuid as WireUuid, VegetationRuntimeQueryFilterDto,
};
use saffron_spatial::{DecisionScalar, UnitInterval, WorldBounds, WorldCellKey, WorldPosition};
use saffron_vegetation::{
    ContentHash, CookPlatformProfile, CookVersionSet, InteractionPolicy, MutationHeader,
    PlantFlags, PlantId, PlantLifecycle, PlantPoint, QuantizedOrientation, VegetationBaseManifest,
    VegetationMutation, VegetationMutationRecord, VegetationResidencyBudgets, VegetationWorld,
};

use super::*;
use crate::registry::CommandRegistry;
use serde_json::json;

use crate::test_support::{StubRenderer, with_stub, with_stub_world};

#[test]
fn snapshot_hex_is_strict_and_roundtrips() {
    let bytes = b"canonical vegetation state";
    let encoded = encode_hex(bytes);
    assert_eq!(decode_hex(&encoded).unwrap(), bytes);
    assert!(decode_hex("AA").is_err());
    assert!(decode_hex("0").is_err());
}

#[test]
fn filter_mapping_is_exhaustive_and_validates_tags() {
    let filter = query_filter(VegetationRuntimeQueryFilterDto {
        families: vec![WireUuid::from(7_u64)],
        required_tags: vec!["42".to_owned()],
        lifecycles: vec![PlantLifecycleDto::Mature],
        interaction_policies: vec![InteractionPolicyDto::Structural],
    })
    .unwrap();
    assert_eq!(filter.families[0].value(), 7);
    assert_eq!(filter.required_tags.iter().next().unwrap().value(), 42);
    assert!(filter.lifecycles.contains(&PlantLifecycle::Mature));
    assert_eq!(filter.interaction_policies, [InteractionPolicy::Structural]);
    assert!(
        query_filter(VegetationRuntimeQueryFilterDto {
            required_tags: vec!["0".to_owned()],
            ..Default::default()
        })
        .is_err()
    );
}

#[test]
fn status_reports_the_runtime_sessions_typed_unavailability() {
    let mut registry = CommandRegistry::new();
    register_runtime_vegetation_commands(&mut registry);
    let mut renderer = StubRenderer::default();
    with_stub(&mut renderer, |context| {
        let reply = registry.dispatch(
            context,
            &json!({ "cmd": "vegetation-runtime-status", "params": {} }),
        );
        assert_eq!(reply["ok"], json!(true));
        assert_eq!(reply["result"]["state"], json!("unavailable"));
        assert_eq!(reply["result"]["reason"], json!("no-project"));
    });
}

fn empty_world() -> VegetationWorld {
    let manifest = VegetationBaseManifest::current(
        Uuid(1),
        Uuid(2),
        ContentHash::new([3; 32]),
        CookVersionSet::current(),
        CookPlatformProfile {
            target: "x86_64-unknown-linux-gnu".to_owned(),
            content_profile: "portable-vulkan".to_owned(),
            toolchain: "rust".to_owned(),
            features: Vec::new(),
        },
        ContentHash::new([4; 32]),
    );
    VegetationWorld::new(manifest, VegetationResidencyBudgets::UNLIMITED).unwrap()
}

fn runtime_point(id: PlantId, cell: WorldCellKey) -> PlantPoint {
    let position = WorldPosition::from_global_ticks(
        cell.coordinates()
            .map(|coordinate| i128::from(coordinate) * 262_144 + 1),
    )
    .unwrap();
    PlantPoint {
        id,
        owner: cell,
        position,
        orientation: QuantizedOrientation::identity(),
        scale: [DecisionScalar::from_integer(1).unwrap(); 3],
        bounds: WorldBounds::new(
            position.global_ticks().map(|tick| tick - 1),
            position.global_ticks().map(|tick| tick + 2),
        )
        .unwrap(),
        family: Uuid(7),
        variation: 0,
        lifecycle: PlantLifecycle::Mature,
        phenotype: 0,
        representation_class: 0,
        deterministic_key: 8,
        candidate: 9,
        parent: None,
        colony: None,
        ecology_tick: 10,
        health: UnitInterval::ONE,
        moisture: UnitInterval::from_bits(20_000),
        fuel: UnitInterval::from_bits(30_000),
        phenology: UnitInterval::ZERO,
        flags: PlantFlags::RUNTIME,
        interaction_policy: InteractionPolicy::Interactive,
        provenance: 0,
        attachment: None,
        surface_projection: [DecisionScalar::from_bits(0); 3],
    }
}

/// The gesture inverse the reply carries is what the editor's undo replays, so it has to describe
/// the preimage rather than the operation: planting inverts to a plant with no delta at all, and
/// tombstoning that plant inverts to the whole delta the planting wrote.
#[test]
fn a_mutation_gesture_replies_with_the_records_that_undo_it() {
    let mut registry = CommandRegistry::new();
    register_runtime_vegetation_commands(&mut registry);
    let mut renderer = StubRenderer::default();
    let cell = WorldCellKey::base(0, 0, 0);
    let plant = PlantId::runtime([21; 16]).unwrap();
    let header = |transaction: u128, key: u128| MutationHeader {
        cell,
        transaction,
        authority: 17,
        logical_tick: transaction as u64,
        idempotency_key: key,
        base_revision: None,
    };
    let mutate = |mutation, transaction, key| {
        json!({
            "cmd": "vegetation-mutate",
            "params": {
                "gesture": format!("{transaction:032x}"),
                "records": [serde_json::to_value(crate::vegetation_mutation_dto::record_to_dto(
                    &VegetationMutationRecord { header: header(transaction, key), mutation },
                ))
                .unwrap()],
            },
        })
    };

    with_stub_world(&mut renderer, Some(empty_world()), |context| {
        let planted = registry.dispatch(
            context,
            &mutate(
                VegetationMutation::Planting(runtime_point(plant, cell)),
                1,
                11,
            ),
        );
        assert_eq!(planted["ok"], json!(true), "{planted}");
        assert_eq!(planted["result"]["applied"], json!(1));
        let inverse = &planted["result"]["inverse"];
        assert_eq!(inverse.as_array().unwrap().len(), 1);
        assert_eq!(inverse[0]["mutation"]["kind"], json!("plant-delta-restore"));
        assert_eq!(inverse[0]["mutation"]["plant"], json!(plant.to_string()));
        assert!(
            inverse[0]["mutation"]["delta"].is_null(),
            "the plant had no delta before it was planted"
        );

        let removed = registry.dispatch(
            context,
            &mutate(VegetationMutation::Tombstone { plant }, 2, 12),
        );
        assert_eq!(removed["ok"], json!(true), "{removed}");
        let delta = &removed["result"]["inverse"][0]["mutation"]["delta"];
        assert_eq!(delta["tombstoned"], json!(false));
        assert_eq!(delta["addition"]["id"], json!(plant.to_string()));
        assert_eq!(delta["lifecycle"], json!("mature"));
    });
}
