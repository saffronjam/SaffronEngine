//! The one persistent vegetation reducer.

use std::collections::{BTreeMap, BTreeSet};

use saffron_spatial::{UnitInterval, WorldCellKey};

use crate::{Error, PlantLifecycle, PlantPoint, Result};

use super::encode::{record_order_key, transaction_signature};
use super::transition::transition_for;
use super::{
    DisturbanceTileKey, FieldTileKey, FieldTileState, PromotionOriginState, VegetationCellState,
    VegetationMutation, VegetationMutationRecord, VegetationState, VegetationTransition,
};

/// Result of applying a mutation batch.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MutationReduction {
    /// Transactions newly committed in deterministic order.
    pub committed_transactions: Vec<u128>,
    /// Exact replays ignored after signature verification.
    pub replayed_transactions: Vec<u128>,
    /// Cells published by newly committed transactions, in canonical order.
    pub changed_cells: Vec<WorldCellKey>,
    /// Typed transitions the committed records produced, in commit order.
    pub transitions: Vec<VegetationTransition>,
}

/// Applies one batch through the only persistent vegetation reducer.
pub fn reduce_mutations(
    state: &mut VegetationState,
    manifest_identity: [u8; 32],
    records: &[VegetationMutationRecord],
) -> Result<MutationReduction> {
    if state.manifest_identity != manifest_identity {
        return Err(Error::ManifestMismatch);
    }

    let mut transactions: BTreeMap<u128, Vec<&VegetationMutationRecord>> = BTreeMap::new();
    for record in records {
        if record.header.transaction == 0
            || record.header.authority == 0
            || record.header.idempotency_key == 0
        {
            return Err(Error::Mutation(
                "transaction, authority, and idempotency keys must be non-zero".to_owned(),
            ));
        }
        transactions
            .entry(record.header.transaction)
            .or_default()
            .push(record);
    }

    let mut ordered = Vec::with_capacity(transactions.len());
    for (transaction, transaction_records) in transactions {
        let mut deduplicated = BTreeMap::new();
        for record in transaction_records {
            if let Some(existing) = deduplicated.insert(record.header.idempotency_key, record)
                && existing != record
            {
                return Err(Error::Mutation(
                    "idempotency key was reused with different operation contents".to_owned(),
                ));
            }
        }
        let mut transaction_records: Vec<_> = deduplicated.into_values().collect();
        transaction_records.sort_by_key(|record| record_order_key(record));
        validate_transaction(&transaction_records)?;
        let signature = transaction_signature(&transaction_records)?;
        let first = transaction_records[0].header;
        ordered.push((
            (first.logical_tick, first.authority, transaction),
            transaction,
            signature,
            transaction_records,
        ));
    }
    ordered.sort_by_key(|entry| entry.0);

    let mut reduction = MutationReduction::default();
    let mut changed_cells = BTreeSet::new();
    for (_, transaction, signature, transaction_records) in ordered {
        if let Some(applied_signature) = state.applied_transactions.get(&transaction) {
            if applied_signature != &signature {
                return Err(Error::Mutation(
                    "transaction ID was reused with different canonical contents".to_owned(),
                ));
            }
            reduction.replayed_transactions.push(transaction);
            continue;
        }

        let mut candidate = state.clone();
        let touched: BTreeSet<_> = transaction_records
            .iter()
            .map(|record| record.header.cell)
            .collect();
        for cell in &touched {
            let expected = transaction_records
                .iter()
                .find(|record| record.header.cell == *cell)
                .and_then(|record| record.header.base_revision);
            let revision = candidate.cells.get(cell).map_or(0, |value| value.revision);
            if expected.is_some_and(|expected| expected != revision) {
                return Err(Error::Mutation(format!(
                    "cell precondition revision {expected:?} does not match {revision}"
                )));
            }
        }
        for record in &transaction_records {
            apply_mutation(&mut candidate, record)?;
            // One transition per committed record, in commit order; a replayed transaction
            // returns above without reaching this loop, so an exact replay emits nothing.
            if let Some(transition) = transition_for(&candidate, record) {
                reduction.transitions.push(transition);
            }
        }
        for cell in &touched {
            let state = candidate.cells.entry(*cell).or_default();
            state.revision = state
                .revision
                .checked_add(1)
                .ok_or(Error::NumericOverflow)?;
        }
        candidate
            .applied_transactions
            .insert(transaction, signature);
        *state = candidate;
        reduction.committed_transactions.push(transaction);
        changed_cells.extend(touched);
    }
    reduction.changed_cells = changed_cells.into_iter().collect();
    Ok(reduction)
}

fn validate_transaction(records: &[&VegetationMutationRecord]) -> Result<()> {
    let first = records[0].header;
    let mut idempotency = BTreeSet::new();
    let mut cell_revisions = BTreeMap::new();
    for record in records {
        if record.header.authority != first.authority
            || record.header.logical_tick != first.logical_tick
            || !idempotency.insert(record.header.idempotency_key)
        {
            return Err(Error::Mutation(
                "one transaction must share authority/tick and have unique operation keys"
                    .to_owned(),
            ));
        }
        if let Some(existing) =
            cell_revisions.insert(record.header.cell, record.header.base_revision)
            && existing != record.header.base_revision
        {
            return Err(Error::Mutation(
                "one transaction declared conflicting cell preconditions".to_owned(),
            ));
        }
        validate_cell_ownership(record)?;
    }
    Ok(())
}

fn validate_cell_ownership(record: &VegetationMutationRecord) -> Result<()> {
    match &record.mutation {
        VegetationMutation::AnchorAddition(point) | VegetationMutation::Planting(point) => {
            point.validate()?;
            if point.owner != record.header.cell {
                return Err(Error::Mutation(
                    "added point is not owned by the mutation cell".to_owned(),
                ));
            }
        }
        VegetationMutation::TransformOverride { position, .. }
        | VegetationMutation::PromotionOriginState {
            state: PromotionOriginState { position, .. },
            ..
        } if position.cell() != record.header.cell => {
            return Err(Error::Mutation(
                "transformed point must be recorded in its canonical owner cell".to_owned(),
            ));
        }
        _ => {}
    }
    Ok(())
}

fn apply_mutation(state: &mut VegetationState, record: &VegetationMutationRecord) -> Result<()> {
    let cell = state.cells.entry(record.header.cell).or_default();
    match &record.mutation {
        VegetationMutation::FieldTilePatch {
            layer,
            channel,
            tile,
            dimensions,
            quantum_bits,
            values,
        } => {
            let expected = dimensions
                .iter()
                .try_fold(1_u64, |product, dimension| {
                    product.checked_mul(u64::from(*dimension))
                })
                .ok_or(Error::NumericOverflow)?;
            if expected == 0 || usize::try_from(expected).ok() != Some(values.len()) {
                return Err(Error::Mutation(
                    "field tile dimensions do not match packed values".to_owned(),
                ));
            }
            cell.field_tiles.insert(
                FieldTileKey {
                    layer: *layer,
                    channel: *channel,
                    tile: *tile,
                },
                FieldTileState {
                    dimensions: *dimensions,
                    quantum_bits: *quantum_bits,
                    values: values.clone(),
                },
            );
        }
        VegetationMutation::AnchorAddition(point) => {
            if point.id.namespace()? != crate::PlantIdNamespace::Explicit {
                return Err(Error::Mutation(
                    "authored anchors require the explicit plant-ID namespace".to_owned(),
                ));
            }
            add_point(cell, point)?;
        }
        VegetationMutation::Planting(point) => {
            if point.id.namespace()? != crate::PlantIdNamespace::Runtime {
                return Err(Error::Mutation(
                    "planting requires the runtime plant-ID namespace".to_owned(),
                ));
            }
            add_point(cell, point)?;
        }
        VegetationMutation::Tombstone { plant } => {
            cell.plants.entry(*plant).or_default().tombstoned = true;
        }
        VegetationMutation::TransformOverride {
            plant,
            position,
            orientation,
            scale,
        } => {
            if scale.iter().any(|value| value.bits() <= 0) {
                return Err(Error::Mutation(
                    "transform override scale must be positive".to_owned(),
                ));
            }
            cell.plants.entry(*plant).or_default().transform =
                Some((*position, *orientation, *scale));
        }
        VegetationMutation::StateOverride {
            plant,
            lifecycle,
            phenotype,
            health,
            moisture,
            fuel,
            interaction_policy,
        } => {
            let target = cell.plants.entry(*plant).or_default();
            if let Some(value) = lifecycle {
                target.lifecycle = Some(*value);
            }
            if let Some(value) = phenotype {
                target.phenotype = Some(*value);
            }
            if let Some(value) = health {
                target.health = Some(*value);
            }
            if let Some(value) = moisture {
                target.moisture = Some(*value);
            }
            if let Some(value) = fuel {
                target.fuel = Some(*value);
            }
            if let Some(value) = interaction_policy {
                target.interaction_policy = Some(*value);
            }
        }
        VegetationMutation::Damage {
            plant,
            amount,
            phenotype,
        } => {
            let target = cell.plants.entry(*plant).or_default();
            let current = target.health.unwrap_or(UnitInterval::ONE).bits();
            target.health = Some(UnitInterval::from_bits(
                current.saturating_sub(amount.bits()),
            ));
            if let Some(phenotype) = phenotype {
                target.phenotype = Some(*phenotype);
            }
        }
        VegetationMutation::MoistureFuel {
            plant,
            moisture,
            fuel,
        } => {
            let target = cell.plants.entry(*plant).or_default();
            target.moisture = Some(*moisture);
            target.fuel = Some(*fuel);
        }
        VegetationMutation::LifecycleTransition {
            plant,
            from,
            to,
            ecology_tick,
        } => {
            let target = cell.plants.entry(*plant).or_default();
            let current = target
                .lifecycle
                .or_else(|| target.addition.as_ref().map(|point| point.lifecycle));
            if from.is_some() && current != *from {
                return Err(Error::Mutation(
                    "lifecycle transition precondition did not match".to_owned(),
                ));
            }
            if target.ecology_tick.is_some_and(|tick| *ecology_tick < tick) {
                return Err(Error::Mutation(
                    "ecology tick cannot move backwards".to_owned(),
                ));
            }
            target.lifecycle = Some(*to);
            target.ecology_tick = Some(*ecology_tick);
            target.tombstoned = *to == PlantLifecycle::Removed;
        }
        VegetationMutation::Harvest { plant, phenotype } => {
            let target = cell.plants.entry(*plant).or_default();
            target.phenotype = Some(*phenotype);
            target.lifecycle = Some(PlantLifecycle::Stump);
        }
        VegetationMutation::Burn {
            plant,
            phenotype,
            remaining_fuel,
        } => {
            let target = cell.plants.entry(*plant).or_default();
            target.phenotype = Some(*phenotype);
            target.fuel = Some(*remaining_fuel);
            target.health = Some(UnitInterval::ZERO);
            target.lifecycle = Some(PlantLifecycle::Dead);
        }
        VegetationMutation::Ignite { plant } => {
            cell.plants.entry(*plant).or_default().ignited = true;
        }
        VegetationMutation::Extinguish { plant } => {
            cell.plants.entry(*plant).or_default().ignited = false;
        }
        VegetationMutation::Regrow {
            plant,
            lifecycle,
            phenotype,
            ecology_tick,
        } => {
            if matches!(
                lifecycle,
                PlantLifecycle::Dead | PlantLifecycle::Stump | PlantLifecycle::Removed
            ) {
                return Err(Error::Mutation(
                    "regrow requires a living lifecycle state".to_owned(),
                ));
            }
            let target = cell.plants.entry(*plant).or_default();
            if target.ecology_tick.is_some_and(|tick| *ecology_tick < tick) {
                return Err(Error::Mutation(
                    "ecology tick cannot move backwards".to_owned(),
                ));
            }
            target.tombstoned = false;
            target.lifecycle = Some(*lifecycle);
            target.phenotype = Some(*phenotype);
            target.ecology_tick = Some(*ecology_tick);
            target.health = Some(UnitInterval::ONE);
        }
        VegetationMutation::PromotionOriginState { plant, state } => {
            let target = cell.plants.entry(*plant).or_default();
            target.transform = Some((state.position, state.orientation, state.scale));
            target.promotion_origin = Some(*state);
        }
        VegetationMutation::DisturbanceMask {
            categories,
            tile,
            values,
        } => {
            if *categories == 0 || values.is_empty() {
                return Err(Error::Mutation(
                    "disturbance mask requires categories and samples".to_owned(),
                ));
            }
            cell.disturbance_masks.insert(
                DisturbanceTileKey {
                    categories: *categories,
                    tile: *tile,
                },
                values.clone(),
            );
        }
    }
    Ok(())
}

fn add_point(cell: &mut VegetationCellState, point: &PlantPoint) -> Result<()> {
    if cell
        .plants
        .get(&point.id)
        .is_some_and(|existing| existing.addition.is_some())
    {
        return Err(Error::DuplicatePlantId(point.id.to_string()));
    }
    let state = cell.plants.entry(point.id).or_default();
    state.tombstoned = false;
    state.addition = Some(point.clone());
    state.lifecycle = Some(point.lifecycle);
    state.phenotype = Some(point.phenotype);
    state.ecology_tick = Some(point.ecology_tick);
    state.health = Some(point.health);
    state.moisture = Some(point.moisture);
    state.fuel = Some(point.fuel);
    state.interaction_policy = Some(point.interaction_policy);
    Ok(())
}
