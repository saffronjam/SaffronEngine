use std::collections::BTreeSet;

use saffron_spatial::{FieldChannel, UnitInterval, WorldCellKey};

use super::*;
use crate::point::sample_plant_point;
use crate::{ContentHash, CookVersionSet, Error, PlantId, PlantIdNamespace};

fn record(
    cell: WorldCellKey,
    transaction: u128,
    idempotency_key: u128,
    mutation: VegetationMutation,
) -> VegetationMutationRecord {
    VegetationMutationRecord {
        header: MutationHeader {
            cell,
            transaction,
            authority: 17,
            logical_tick: transaction as u64,
            idempotency_key,
            base_revision: Some(0),
        },
        mutation,
    }
}

#[test]
fn committed_records_emit_one_typed_transition_and_replays_emit_none() {
    let manifest = [5; 32];
    let cell = WorldCellKey::base(1, 0, -2);
    let id = PlantId::runtime([6; 16]).unwrap();
    let mut state = VegetationState::new(manifest);

    // Plant it, then damage it, then harvest it: three records in one transaction.
    let planting = record(
        cell,
        1,
        21,
        VegetationMutation::Planting(sample_plant_point(id, cell)),
    );
    let damage = record(
        cell,
        1,
        22,
        VegetationMutation::Damage {
            plant: id,
            amount: UnitInterval::from_bits(16_000),
            phenotype: Some(3),
        },
    );
    let harvest = record(
        cell,
        1,
        23,
        VegetationMutation::Harvest {
            plant: id,
            phenotype: 4,
        },
    );
    let records = [planting, damage, harvest];
    let reduction = reduce_mutations(&mut state, manifest, &records).unwrap();

    assert_eq!(reduction.committed_transactions, vec![1]);
    assert_eq!(reduction.transitions.len(), 3, "one per committed record");
    assert!(
        reduction
            .transitions
            .iter()
            .all(|transition| transition.cell == cell
                && transition.plant == Some(id)
                && transition.transaction == 1)
    );
    assert_eq!(
        reduction.transitions[0].kind,
        VegetationTransitionKind::Planted
    );
    // The damage transition reports the health the plant settled at, not just the amount.
    let VegetationTransitionKind::Damaged { amount, health } = reduction.transitions[1].kind else {
        panic!(
            "expected a damage transition, got {:?}",
            reduction.transitions[1].kind
        );
    };
    assert_eq!(amount, UnitInterval::from_bits(16_000));
    assert_eq!(
        health,
        state.cells()[&cell].plants[&id].health.unwrap(),
        "the emitted health matches the reduced state"
    );
    assert_eq!(
        reduction.transitions[2].kind,
        VegetationTransitionKind::Harvested { phenotype: 4 }
    );

    // An exact replay of the same transaction is idempotent, so it emits nothing.
    let replay = reduce_mutations(&mut state, manifest, &records).unwrap();
    assert_eq!(replay.replayed_transactions, vec![1]);
    assert!(
        replay.transitions.is_empty(),
        "a replay must not re-fire events"
    );
}

#[test]
fn shuffled_transactions_and_exact_replays_reduce_identically() {
    let manifest = [3; 32];
    let cell = WorldCellKey::base(0, 0, 0);
    let id = PlantId::runtime([4; 16]).unwrap();
    assert_eq!(id.namespace().unwrap(), PlantIdNamespace::Runtime);
    let planting = record(
        cell,
        1,
        11,
        VegetationMutation::Planting(sample_plant_point(id, cell)),
    );
    let moisture = VegetationMutationRecord {
        header: MutationHeader {
            cell,
            transaction: 2,
            authority: 17,
            logical_tick: 2,
            idempotency_key: 12,
            base_revision: Some(1),
        },
        mutation: VegetationMutation::MoistureFuel {
            plant: id,
            moisture: UnitInterval::from_bits(5),
            fuel: UnitInterval::from_bits(6),
        },
    };
    let mut ordered = VegetationState::new(manifest);
    reduce_mutations(
        &mut ordered,
        manifest,
        &[planting.clone(), moisture.clone()],
    )
    .unwrap();

    let mut shuffled = VegetationState::new(manifest);
    reduce_mutations(
        &mut shuffled,
        manifest,
        &[moisture.clone(), planting.clone(), moisture, planting],
    )
    .unwrap();
    assert_eq!(
        ordered.canonical_bytes().unwrap(),
        shuffled.canonical_bytes().unwrap()
    );
}

#[test]
fn cross_cell_transaction_is_atomic() {
    let manifest = [5; 32];
    let a = WorldCellKey::base(0, 0, 0);
    let b = WorldCellKey::base(1, 0, 0);
    let id_a = PlantId::runtime([6; 16]).unwrap();
    let id_b = PlantId::runtime([7; 16]).unwrap();
    let mut second = record(
        b,
        1,
        12,
        VegetationMutation::Planting(sample_plant_point(id_b, b)),
    );
    second.header.base_revision = Some(99);
    let mut state = VegetationState::new(manifest);
    assert!(
        reduce_mutations(
            &mut state,
            manifest,
            &[
                record(
                    a,
                    1,
                    11,
                    VegetationMutation::Planting(sample_plant_point(id_a, a))
                ),
                second
            ]
        )
        .is_err()
    );
    assert!(state.cells().is_empty());
}

#[test]
fn snapshot_tail_compaction_matches_full_reduction() {
    let manifest = [8; 32];
    let cell = WorldCellKey::base(0, 0, 0);
    let id = PlantId::runtime([9; 16]).unwrap();
    let planting = record(
        cell,
        1,
        11,
        VegetationMutation::Planting(sample_plant_point(id, cell)),
    );
    let moisture = VegetationMutationRecord {
        header: MutationHeader {
            cell,
            transaction: 2,
            authority: 17,
            logical_tick: 2,
            idempotency_key: 12,
            base_revision: Some(1),
        },
        mutation: VegetationMutation::MoistureFuel {
            plant: id,
            moisture: UnitInterval::from_bits(5),
            fuel: UnitInterval::from_bits(6),
        },
    };
    let mut full = VegetationState::new(manifest);
    reduce_mutations(&mut full, manifest, &[planting.clone(), moisture.clone()]).unwrap();

    let compacted = SaveStateEnvelope {
        binding: VegetationStateBinding {
            manifest_identity: ContentHash::new(manifest),
            cook_graph_identity: ContentHash::new([9; 32]),
            versions: CookVersionSet::current(),
            seed_namespaces_identity: ContentHash::new([10; 32]),
        },
        snapshot: VegetationState::new(manifest),
        tail: vec![moisture.clone(), planting.clone(), moisture, planting],
    }
    .compact()
    .unwrap();
    assert!(compacted.tail.is_empty());
    assert_eq!(
        full.canonical_bytes().unwrap(),
        compacted.snapshot.canonical_bytes().unwrap()
    );
}

/// The persistent content of every non-empty cell, with the transaction revision zeroed.
///
/// An undo is a new transaction rather than a rollback of history, so revisions and the applied
/// set move forward across one; what has to come back is the content those transactions wrote.
fn content(state: &VegetationState) -> Vec<(WorldCellKey, VegetationCellState)> {
    state
        .cells()
        .iter()
        .filter(|(_, cell)| {
            !cell.plants.is_empty()
                || !cell.field_tiles.is_empty()
                || !cell.disturbance_masks.is_empty()
        })
        .map(|(key, cell)| {
            (
                *key,
                VegetationCellState {
                    revision: 0,
                    ..cell.clone()
                },
            )
        })
        .collect()
}

/// A world holding one plant and one field tile, and a five-record gesture over it that plants,
/// tombstones, overwrites a tile, adds a tile, and adds a disturbance mask across two cells.
fn gesture_fixture() -> (
    [u8; 32],
    VegetationState,
    Vec<(WorldCellKey, VegetationCellState)>,
    EditorJournalEnvelope,
) {
    let manifest = [11; 32];
    let a = WorldCellKey::base(0, 0, 0);
    let b = WorldCellKey::base(1, 0, 0);
    let existing = PlantId::runtime([12; 16]).unwrap();
    let planted = PlantId::runtime([13; 16]).unwrap();
    let tile = |values: Vec<i32>| VegetationMutation::FieldTilePatch {
        layer: 1,
        channel: FieldChannel::Moisture,
        tile: 7,
        dimensions: [2, 1, 1],
        quantum_bits: 8,
        values,
    };
    let mut state = VegetationState::new(manifest);
    reduce_mutations(
        &mut state,
        manifest,
        &[
            record(
                a,
                1,
                11,
                VegetationMutation::Planting(sample_plant_point(existing, a)),
            ),
            record(a, 1, 12, tile(vec![1, 2])),
            record(
                a,
                1,
                13,
                VegetationMutation::DisturbanceMask {
                    categories: 3,
                    tile: 9,
                    values: vec![7, -7],
                },
            ),
        ],
    )
    .unwrap();
    let before = content(&state);

    let gesture_record = |cell, key, mutation| VegetationMutationRecord {
        header: MutationHeader {
            cell,
            transaction: 5,
            authority: 17,
            logical_tick: 5,
            idempotency_key: key,
            base_revision: None,
        },
        mutation,
    };
    let forward = vec![
        gesture_record(a, 21, VegetationMutation::Tombstone { plant: existing }),
        gesture_record(
            b,
            22,
            VegetationMutation::Planting(sample_plant_point(planted, b)),
        ),
        gesture_record(a, 23, tile(vec![9, 9])),
        gesture_record(
            b,
            24,
            VegetationMutation::FieldTilePatch {
                layer: 1,
                channel: FieldChannel::Moisture,
                tile: 8,
                dimensions: [1, 1, 1],
                quantum_bits: 8,
                values: vec![4],
            },
        ),
        gesture_record(
            a,
            25,
            VegetationMutation::DisturbanceMask {
                categories: 3,
                tile: 9,
                values: vec![-1, 1],
            },
        ),
    ];
    let journal = EditorJournalEnvelope::capture(&state, 0x9e37_79b9, forward).unwrap();
    (manifest, state, before, journal)
}

#[test]
fn a_gesture_inverse_returns_every_touched_address_to_its_preimage() {
    let (manifest, mut state, before, journal) = gesture_fixture();
    assert_eq!(journal.inverse.len(), 5, "one restore per touched address");
    assert_eq!(
        journal
            .inverse
            .iter()
            .map(|record| record.header.transaction)
            .collect::<BTreeSet<_>>()
            .len(),
        1,
        "the gesture undoes as one atomic transaction"
    );

    reduce_mutations(&mut state, manifest, &journal.forward).unwrap();
    assert_ne!(content(&state), before, "the gesture changed the world");
    reduce_mutations(&mut state, manifest, &journal.inverse).unwrap();
    assert_eq!(content(&state), before);
}

#[test]
fn a_gesture_inverse_missing_one_record_leaves_that_address_changed() {
    let (manifest, mut state, before, journal) = gesture_fixture();
    reduce_mutations(&mut state, manifest, &journal.forward).unwrap();
    for dropped in 0..journal.inverse.len() {
        let mut partial = journal.inverse.clone();
        partial.remove(dropped);
        let mut candidate = state.clone();
        reduce_mutations(&mut candidate, manifest, &partial).unwrap();
        assert_ne!(
            content(&candidate),
            before,
            "dropping inverse record {dropped} must leave the world changed"
        );
    }
}

#[test]
fn state_rejects_a_different_manifest() {
    let mut state = VegetationState::new([1; 32]);
    assert!(matches!(
        reduce_mutations(&mut state, [2; 32], &[]),
        Err(Error::ManifestMismatch)
    ));
}
