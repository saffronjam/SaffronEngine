//! The canonical mutation preimage: ordering keys, transaction signatures, and the byte encoder.

use saffron_spatial::{DecisionScalar, FieldChannel, UnitInterval, WorldPosition};

use crate::hash::sha256;
use crate::{Error, PlantId, PlantPointColumns, QuantizedOrientation, Result};

use super::{PromotionOriginState, VegetationMutation, VegetationMutationRecord};

pub(crate) fn record_order_key(
    record: &VegetationMutationRecord,
) -> ([u8; 25], u8, [u8; 16], u128) {
    (
        record.header.cell.canonical_bytes(),
        mutation_tag(&record.mutation),
        mutation_plant_id(&record.mutation).map_or([0; 16], PlantId::bytes),
        record.header.idempotency_key,
    )
}

pub(super) fn transaction_signature(records: &[&VegetationMutationRecord]) -> Result<[u8; 32]> {
    let mut bytes = b"saffron-anima/vegetation-transaction/v1\0".to_vec();
    push_len(&mut bytes, records.len())?;
    for record in records {
        push_record(&mut bytes, record)?;
    }
    Ok(sha256(&bytes))
}

pub(super) fn push_record(bytes: &mut Vec<u8>, record: &VegetationMutationRecord) -> Result<()> {
    bytes.extend_from_slice(&record.header.cell.canonical_bytes());
    bytes.extend_from_slice(&record.header.transaction.to_be_bytes());
    bytes.extend_from_slice(&record.header.authority.to_be_bytes());
    bytes.extend_from_slice(&record.header.logical_tick.to_be_bytes());
    bytes.extend_from_slice(&record.header.idempotency_key.to_be_bytes());
    push_option_u64(bytes, record.header.base_revision);
    bytes.push(mutation_tag(&record.mutation));
    match &record.mutation {
        VegetationMutation::FieldTilePatch {
            layer,
            channel,
            tile,
            dimensions,
            quantum_bits,
            values,
        } => {
            bytes.extend_from_slice(&layer.to_be_bytes());
            push_field_channel(bytes, *channel);
            bytes.extend_from_slice(&tile.to_be_bytes());
            for dimension in dimensions {
                bytes.extend_from_slice(&dimension.to_be_bytes());
            }
            bytes.extend_from_slice(&quantum_bits.to_be_bytes());
            push_len(bytes, values.len())?;
            for value in values {
                bytes.extend_from_slice(&value.to_be_bytes());
            }
        }
        VegetationMutation::AnchorAddition(point) | VegetationMutation::Planting(point) => {
            bytes.extend_from_slice(
                &PlantPointColumns::from_points(vec![point.clone()])?.canonical_bytes()?,
            );
        }
        VegetationMutation::Tombstone { plant } => bytes.extend_from_slice(&plant.bytes()),
        VegetationMutation::TransformOverride {
            plant,
            position,
            orientation,
            scale,
        } => {
            bytes.extend_from_slice(&plant.bytes());
            push_position(bytes, *position);
            push_orientation(bytes, *orientation);
            push_scale(bytes, *scale);
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
            bytes.extend_from_slice(&plant.bytes());
            push_option_u32(bytes, lifecycle.map(|value| value as u32));
            push_option_u32(bytes, *phenotype);
            push_option_unit(bytes, *health);
            push_option_unit(bytes, *moisture);
            push_option_unit(bytes, *fuel);
            push_option_u32(bytes, interaction_policy.map(|value| value as u32));
        }
        VegetationMutation::Damage {
            plant,
            amount,
            phenotype,
        } => {
            bytes.extend_from_slice(&plant.bytes());
            bytes.extend_from_slice(&amount.canonical_bytes());
            push_option_u32(bytes, *phenotype);
        }
        VegetationMutation::MoistureFuel {
            plant,
            moisture,
            fuel,
        } => {
            bytes.extend_from_slice(&plant.bytes());
            bytes.extend_from_slice(&moisture.canonical_bytes());
            bytes.extend_from_slice(&fuel.canonical_bytes());
        }
        VegetationMutation::LifecycleTransition {
            plant,
            from,
            to,
            ecology_tick,
        } => {
            bytes.extend_from_slice(&plant.bytes());
            push_option_u32(bytes, from.map(|value| value as u32));
            bytes.extend_from_slice(&(*to as u32).to_be_bytes());
            bytes.extend_from_slice(&ecology_tick.to_be_bytes());
        }
        VegetationMutation::Harvest { plant, phenotype } => {
            bytes.extend_from_slice(&plant.bytes());
            bytes.extend_from_slice(&phenotype.to_be_bytes());
        }
        VegetationMutation::Burn {
            plant,
            phenotype,
            remaining_fuel,
        } => {
            bytes.extend_from_slice(&plant.bytes());
            bytes.extend_from_slice(&phenotype.to_be_bytes());
            bytes.extend_from_slice(&remaining_fuel.canonical_bytes());
        }
        VegetationMutation::Ignite { plant } | VegetationMutation::Extinguish { plant } => {
            bytes.extend_from_slice(&plant.bytes());
        }
        VegetationMutation::Regrow {
            plant,
            lifecycle,
            phenotype,
            ecology_tick,
        } => {
            bytes.extend_from_slice(&plant.bytes());
            bytes.extend_from_slice(&(*lifecycle as u32).to_be_bytes());
            bytes.extend_from_slice(&phenotype.to_be_bytes());
            bytes.extend_from_slice(&ecology_tick.to_be_bytes());
        }
        VegetationMutation::PromotionOriginState { plant, state } => {
            bytes.extend_from_slice(&plant.bytes());
            push_promotion(bytes, *state);
        }
        VegetationMutation::DisturbanceMask {
            categories,
            tile,
            values,
        } => {
            bytes.extend_from_slice(&categories.to_be_bytes());
            bytes.extend_from_slice(&tile.to_be_bytes());
            push_len(bytes, values.len())?;
            for value in values {
                bytes.extend_from_slice(&value.to_be_bytes());
            }
        }
    }
    Ok(())
}

pub(crate) fn mutation_tag(mutation: &VegetationMutation) -> u8 {
    match mutation {
        VegetationMutation::FieldTilePatch { .. } => 0,
        VegetationMutation::AnchorAddition(_) => 1,
        VegetationMutation::Tombstone { .. } => 2,
        VegetationMutation::TransformOverride { .. } => 3,
        VegetationMutation::StateOverride { .. } => 4,
        VegetationMutation::Planting(_) => 5,
        VegetationMutation::Damage { .. } => 6,
        VegetationMutation::MoistureFuel { .. } => 7,
        VegetationMutation::LifecycleTransition { .. } => 8,
        VegetationMutation::Harvest { .. } => 9,
        VegetationMutation::Burn { .. } => 10,
        VegetationMutation::Regrow { .. } => 11,
        VegetationMutation::PromotionOriginState { .. } => 12,
        VegetationMutation::DisturbanceMask { .. } => 13,
        VegetationMutation::Ignite { .. } => 14,
        VegetationMutation::Extinguish { .. } => 15,
    }
}

fn mutation_plant_id(mutation: &VegetationMutation) -> Option<PlantId> {
    match mutation {
        VegetationMutation::AnchorAddition(point) | VegetationMutation::Planting(point) => {
            Some(point.id)
        }
        VegetationMutation::Tombstone { plant }
        | VegetationMutation::TransformOverride { plant, .. }
        | VegetationMutation::StateOverride { plant, .. }
        | VegetationMutation::Damage { plant, .. }
        | VegetationMutation::MoistureFuel { plant, .. }
        | VegetationMutation::LifecycleTransition { plant, .. }
        | VegetationMutation::Harvest { plant, .. }
        | VegetationMutation::Burn { plant, .. }
        | VegetationMutation::Ignite { plant }
        | VegetationMutation::Extinguish { plant }
        | VegetationMutation::Regrow { plant, .. }
        | VegetationMutation::PromotionOriginState { plant, .. } => Some(*plant),
        VegetationMutation::FieldTilePatch { .. } | VegetationMutation::DisturbanceMask { .. } => {
            None
        }
    }
}

fn push_promotion(bytes: &mut Vec<u8>, state: PromotionOriginState) {
    push_position(bytes, state.position);
    push_orientation(bytes, state.orientation);
    push_scale(bytes, state.scale);
    push_scale(bytes, state.linear_velocity);
    push_scale(bytes, state.angular_velocity);
}

fn push_position(bytes: &mut Vec<u8>, position: WorldPosition) {
    for tick in position.global_ticks() {
        bytes.extend_from_slice(&tick.to_be_bytes());
    }
}

fn push_orientation(bytes: &mut Vec<u8>, orientation: QuantizedOrientation) {
    for lane in orientation.bits() {
        bytes.extend_from_slice(&lane.to_be_bytes());
    }
}

fn push_scale(bytes: &mut Vec<u8>, scale: [DecisionScalar; 3]) {
    for value in scale {
        bytes.extend_from_slice(&value.canonical_bytes());
    }
}

fn push_field_channel(bytes: &mut Vec<u8>, channel: FieldChannel) {
    let (tag, user) = channel.canonical_code();
    bytes.push(tag);
    bytes.extend_from_slice(&user.to_be_bytes());
}

fn push_option_u32(bytes: &mut Vec<u8>, value: Option<u32>) {
    match value {
        Some(value) => {
            bytes.push(1);
            bytes.extend_from_slice(&value.to_be_bytes());
        }
        None => bytes.push(0),
    }
}

fn push_option_u64(bytes: &mut Vec<u8>, value: Option<u64>) {
    match value {
        Some(value) => {
            bytes.push(1);
            bytes.extend_from_slice(&value.to_be_bytes());
        }
        None => bytes.push(0),
    }
}

fn push_option_unit(bytes: &mut Vec<u8>, value: Option<UnitInterval>) {
    match value {
        Some(value) => {
            bytes.push(1);
            bytes.extend_from_slice(&value.canonical_bytes());
        }
        None => bytes.push(0),
    }
}

fn push_len(bytes: &mut Vec<u8>, length: usize) -> Result<()> {
    bytes.extend_from_slice(
        &u64::try_from(length)
            .map_err(|_| Error::NumericOverflow)?
            .to_be_bytes(),
    );
    Ok(())
}
