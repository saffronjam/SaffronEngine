use saffron_spatial::{UnitInterval, WorldCellKey};

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

#[test]
fn state_rejects_a_different_manifest() {
    let mut state = VegetationState::new([1; 32]);
    assert!(matches!(
        reduce_mutations(&mut state, [2; 32], &[]),
        Err(Error::ManifestMismatch)
    ));
}
