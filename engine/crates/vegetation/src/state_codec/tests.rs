//! Round trips, canonical-byte re-encoding, and binding-mismatch rejection for the
//! persistent-state and save containers.

use saffron_core::Uuid;
use saffron_spatial::{DecisionScalar, FieldChannel, UnitInterval, WorldCellKey};

use super::save::{decode_record, encode_record};
use super::*;
use crate::point::sample_plant_point;
use crate::{
    CookPlatformProfile, CookVersionSet, InteractionPolicy, MutationHeader, PlantId,
    PlantLifecycle, PromotionOriginState, QuantizedOrientation, VegetationMutation,
    VegetationMutationRecord, VegetationSeedNamespace, reduce_mutations,
};

fn binding(seed: u128) -> VegetationStateBinding {
    let mut manifest = VegetationBaseManifest::current(
        Uuid(1),
        Uuid(2),
        ContentHash::new([3; 32]),
        CookVersionSet::current(),
        CookPlatformProfile {
            target: "test-target".to_owned(),
            content_profile: "test-profile".to_owned(),
            toolchain: "rust-1.96".to_owned(),
            features: vec!["persistence".to_owned()],
        },
        ContentHash::new([4; 32]),
    );
    manifest.seed_namespaces = vec![VegetationSeedNamespace {
        name: "ecology".to_owned(),
        namespace: seed,
    }];
    VegetationStateBinding::from_manifest(&manifest).unwrap()
}

fn record(
    cell: WorldCellKey,
    transaction: u128,
    base_revision: u64,
    mutation: VegetationMutation,
) -> VegetationMutationRecord {
    VegetationMutationRecord {
        header: MutationHeader {
            cell,
            transaction,
            authority: 17,
            logical_tick: transaction as u64,
            idempotency_key: transaction + 100,
            base_revision: Some(base_revision),
        },
        mutation,
    }
}

fn envelope() -> SaveStateEnvelope {
    let binding = binding(41);
    let cell = WorldCellKey::base(-2, 1, 3);
    let id = PlantId::runtime([9; 16]).unwrap();
    let planting = record(
        cell,
        1,
        0,
        VegetationMutation::Planting(sample_plant_point(id, cell)),
    );
    let moisture = record(
        cell,
        2,
        1,
        VegetationMutation::MoistureFuel {
            plant: id,
            moisture: UnitInterval::from_bits(5),
            fuel: UnitInterval::from_bits(6),
        },
    );
    SaveStateEnvelope {
        binding,
        snapshot: VegetationState::new(binding.manifest_identity.bytes()),
        tail: vec![moisture, planting],
    }
}

#[test]
fn state_and_save_round_trip_canonical_bytes() {
    let envelope = envelope();
    let bytes = envelope.canonical_bytes().unwrap();
    let decoded = SaveStateEnvelope::from_canonical_bytes(&bytes, envelope.binding).unwrap();
    assert_eq!(decoded.canonical_bytes().unwrap(), bytes);

    let compacted = decoded.compact().unwrap();
    let state_bytes = compacted.snapshot.canonical_bytes().unwrap();
    let state = VegetationState::from_canonical_bytes(
        &state_bytes,
        compacted.binding.manifest_identity.bytes(),
    )
    .unwrap();
    assert_eq!(state.canonical_bytes().unwrap(), state_bytes);
}

#[test]
fn the_ecology_section_round_trips_and_preserves_the_checkpoint_identity() {
    let manifest = [11_u8; 32];
    let mut state = VegetationState::new(manifest);
    let cell_a = WorldCellKey::base(0, 0, 0);
    let cell_b = WorldCellKey::base(1, 0, -2);
    state.ecology_mut().advance_world_to(3).unwrap();
    for tick in 1..=3_u64 {
        let summaries = [cell_a, cell_b]
            .into_iter()
            .map(|cell| {
                (
                    cell,
                    crate::EcologyCellSummary {
                        tick,
                        plants: 5 + tick as u32,
                        canopy: UnitInterval::from_bits(1_000 * tick as u16),
                        roots: UnitInterval::from_bits(500 * tick as u16),
                        health: UnitInterval::ONE,
                        moisture: UnitInterval::from_bits(30_000),
                        fuel: UnitInterval::from_bits(40_000),
                        families: vec![crate::EcologyFamilyPresence {
                            family: 7,
                            canopy: UnitInterval::from_bits(1_000 * tick as u16),
                            health: UnitInterval::ONE,
                        }],
                    },
                )
            })
            .collect();
        state
            .ecology_mut()
            .publish_region_tick(tick, &summaries)
            .unwrap();
    }
    let identity = state.ecology().checkpoint_identity();

    let bytes = state.canonical_bytes().unwrap();
    let decoded = VegetationState::from_canonical_bytes(&bytes, manifest).unwrap();
    assert_eq!(decoded.canonical_bytes().unwrap(), bytes);
    assert_eq!(decoded.ecology().clock().tick(), 3);
    assert_eq!(decoded.ecology().summaries().len(), 2);
    assert_eq!(decoded.ecology().checkpoint_identity(), identity);
}

#[test]
fn snapshot_compaction_and_duplicate_tail_replay_are_equivalent() {
    let envelope = envelope();
    let mut duplicated = envelope.clone();
    duplicated.tail.extend(envelope.tail.clone());

    let expected = envelope.reduced_state().unwrap();
    let replayed = duplicated.reduced_state().unwrap();
    let compacted = duplicated.compact().unwrap();
    assert_eq!(expected, replayed);
    assert_eq!(expected, compacted.snapshot);
    assert!(compacted.tail.is_empty());
}

#[test]
fn interrupted_frames_are_rejected_at_every_boundary() {
    let envelope = envelope();
    let bytes = envelope.canonical_bytes().unwrap();
    for length in 0..bytes.len() {
        assert!(
            SaveStateEnvelope::from_canonical_bytes(&bytes[..length], envelope.binding).is_err()
        );
    }
    assert!(SaveStateEnvelope::from_canonical_bytes(&bytes, envelope.binding).is_ok());
}

#[test]
fn corruption_and_trailing_bytes_are_rejected() {
    let envelope = envelope();
    let mut corrupt = envelope.canonical_bytes().unwrap();
    let payload_offset = 8 + 4 + 32 + 8;
    corrupt[payload_offset + 1] ^= 0x40;
    assert!(matches!(
        SaveStateEnvelope::from_canonical_bytes(&corrupt, envelope.binding),
        Err(Error::ArtifactHashMismatch { .. })
    ));

    let mut trailing = envelope.canonical_bytes().unwrap();
    trailing.push(0);
    assert!(SaveStateEnvelope::from_canonical_bytes(&trailing, envelope.binding).is_err());
}

#[test]
fn every_header_binding_mismatch_is_rejected() {
    let envelope = envelope();
    let bytes = envelope.canonical_bytes().unwrap();

    let mut manifest = envelope.binding;
    manifest.manifest_identity = ContentHash::new([99; 32]);
    assert!(matches!(
        SaveStateEnvelope::from_canonical_bytes(&bytes, manifest),
        Err(Error::ManifestMismatch)
    ));

    let mut graph = envelope.binding;
    graph.cook_graph_identity = ContentHash::new([98; 32]);
    assert!(SaveStateEnvelope::from_canonical_bytes(&bytes, graph).is_err());

    let mut versions = envelope.binding;
    versions.versions.simulation += 1;
    assert!(SaveStateEnvelope::from_canonical_bytes(&bytes, versions).is_err());

    let other_seed = binding(42);
    let mut seeds = envelope.binding;
    seeds.seed_namespaces_identity = other_seed.seed_namespaces_identity;
    assert!(SaveStateEnvelope::from_canonical_bytes(&bytes, seeds).is_err());
}

#[test]
fn decoded_tail_uses_the_one_reducer() {
    let envelope = envelope();
    let bytes = envelope.canonical_bytes().unwrap();
    let decoded = SaveStateEnvelope::from_canonical_bytes(&bytes, envelope.binding).unwrap();
    let decoded_state = decoded.reduced_state().unwrap();

    let mut direct = VegetationState::new(envelope.binding.manifest_identity.bytes());
    reduce_mutations(
        &mut direct,
        envelope.binding.manifest_identity.bytes(),
        &envelope.tail,
    )
    .unwrap();
    assert_eq!(decoded_state, direct);
}

#[test]
fn every_mutation_variant_round_trips_exactly() {
    let cell = WorldCellKey::base(0, 0, 0);
    let runtime = PlantId::runtime([31; 16]).unwrap();
    let explicit = PlantId::explicit([32; 16]).unwrap();
    let position = sample_plant_point(runtime, cell).position;
    let promotion = PromotionOriginState {
        position,
        orientation: QuantizedOrientation::identity(),
        scale: [DecisionScalar::from_integer(1).unwrap(); 3],
        linear_velocity: [DecisionScalar::from_bits(-1); 3],
        angular_velocity: [DecisionScalar::from_bits(2); 3],
    };
    let mutations = vec![
        VegetationMutation::FieldTilePatch {
            layer: 1,
            channel: FieldChannel::User(44),
            tile: 2,
            dimensions: [2, 1, 1],
            quantum_bits: 8,
            values: vec![-3, 4],
        },
        VegetationMutation::AnchorAddition(sample_plant_point(explicit, cell)),
        VegetationMutation::Tombstone { plant: runtime },
        VegetationMutation::TransformOverride {
            plant: runtime,
            position,
            orientation: QuantizedOrientation::identity(),
            scale: [DecisionScalar::from_integer(2).unwrap(); 3],
        },
        VegetationMutation::StateOverride {
            plant: runtime,
            lifecycle: Some(PlantLifecycle::Mature),
            phenotype: Some(3),
            health: Some(UnitInterval::from_bits(4)),
            moisture: Some(UnitInterval::from_bits(5)),
            fuel: Some(UnitInterval::from_bits(6)),
            interaction_policy: Some(InteractionPolicy::Structural),
        },
        VegetationMutation::Planting(sample_plant_point(runtime, cell)),
        VegetationMutation::Damage {
            plant: runtime,
            amount: UnitInterval::from_bits(7),
            phenotype: Some(8),
        },
        VegetationMutation::MoistureFuel {
            plant: runtime,
            moisture: UnitInterval::from_bits(9),
            fuel: UnitInterval::from_bits(10),
        },
        VegetationMutation::LifecycleTransition {
            plant: runtime,
            from: Some(PlantLifecycle::Sprout),
            to: PlantLifecycle::Juvenile,
            ecology_tick: 11,
        },
        VegetationMutation::Harvest {
            plant: runtime,
            phenotype: 12,
        },
        VegetationMutation::Burn {
            plant: runtime,
            phenotype: 13,
            remaining_fuel: UnitInterval::from_bits(14),
        },
        VegetationMutation::Regrow {
            plant: runtime,
            lifecycle: PlantLifecycle::Sprout,
            phenotype: 15,
            ecology_tick: 16,
        },
        VegetationMutation::PromotionOriginState {
            plant: runtime,
            state: promotion,
        },
        VegetationMutation::DisturbanceMask {
            categories: 17,
            tile: 18,
            values: vec![-19, 20],
        },
    ];
    for (index, mutation) in mutations.into_iter().enumerate() {
        let record = VegetationMutationRecord {
            header: MutationHeader {
                cell,
                transaction: index as u128 + 1,
                authority: 21,
                logical_tick: index as u64,
                idempotency_key: index as u128 + 100,
                base_revision: Some(index as u64),
            },
            mutation,
        };
        let bytes = encode_record(&record).unwrap();
        assert_eq!(decode_record(&bytes).unwrap(), record);
    }
}

#[test]
fn complete_reduced_snapshot_round_trips_every_delta_family() {
    let binding = binding(51);
    let manifest = binding.manifest_identity.bytes();
    let cell = WorldCellKey::base(0, 0, 0);
    let id = PlantId::runtime([52; 16]).unwrap();
    let planted = sample_plant_point(id, cell);
    let position = planted.position;
    let scale = [DecisionScalar::from_integer(2).unwrap(); 3];
    let records = vec![
        record(cell, 1, 0, VegetationMutation::Planting(planted)),
        record(
            cell,
            2,
            1,
            VegetationMutation::FieldTilePatch {
                layer: 53,
                channel: FieldChannel::User(54),
                tile: 55,
                dimensions: [1, 1, 2],
                quantum_bits: 12,
                values: vec![-56, 57],
            },
        ),
        record(
            cell,
            3,
            2,
            VegetationMutation::TransformOverride {
                plant: id,
                position,
                orientation: QuantizedOrientation::identity(),
                scale,
            },
        ),
        record(
            cell,
            4,
            3,
            VegetationMutation::StateOverride {
                plant: id,
                lifecycle: Some(PlantLifecycle::Mature),
                phenotype: Some(58),
                health: Some(UnitInterval::from_bits(59)),
                moisture: Some(UnitInterval::from_bits(60)),
                fuel: Some(UnitInterval::from_bits(61)),
                interaction_policy: Some(InteractionPolicy::Structural),
            },
        ),
        record(
            cell,
            5,
            4,
            VegetationMutation::PromotionOriginState {
                plant: id,
                state: PromotionOriginState {
                    position,
                    orientation: QuantizedOrientation::identity(),
                    scale,
                    linear_velocity: [DecisionScalar::from_bits(62); 3],
                    angular_velocity: [DecisionScalar::from_bits(-63); 3],
                },
            },
        ),
        record(
            cell,
            6,
            5,
            VegetationMutation::DisturbanceMask {
                categories: 64,
                tile: 65,
                values: vec![-66, 67],
            },
        ),
        record(cell, 7, 6, VegetationMutation::Tombstone { plant: id }),
    ];
    let mut state = VegetationState::new(manifest);
    reduce_mutations(&mut state, manifest, &records).unwrap();
    let bytes = state.canonical_bytes().unwrap();
    assert_eq!(
        VegetationState::from_canonical_bytes(&bytes, manifest).unwrap(),
        state
    );
}
