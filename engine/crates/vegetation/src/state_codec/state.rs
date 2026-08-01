//! The `.svegstate` snapshot: cells, field tiles, plant deltas, disturbance masks, applied
//! transactions, and the ecology checkpoint.

use std::collections::BTreeMap;

use saffron_spatial::{UnitInterval, WorldCellKey};

use super::frame::{decode_frame, encode_frame};
use super::values::*;
use super::{STATE_COMMIT, STATE_FORMAT, STATE_MAGIC, STATE_VERSION, state_schema_identity};
use crate::binary::{BinaryReader, BinaryWriter};
use crate::{
    DisturbanceTileKey, Error, FieldTileKey, FieldTileState, InteractionPolicy, PlantId,
    PlantLifecycle, PlantPersistentState, Result, VegetationCellState, VegetationState,
};

pub(crate) fn encode_state(state: &VegetationState) -> Result<Vec<u8>> {
    if state.manifest_identity() == [0; 32] {
        return Err(Error::ArtifactFormat {
            format: STATE_FORMAT,
            field: "manifestIdentity".to_owned(),
        });
    }
    let mut payload = BinaryWriter::new();
    payload.bytes(&state.manifest_identity());
    payload.length(state.cells().len())?;
    for (cell, cell_state) in state.cells() {
        encode_cell_state(&mut payload, *cell, cell_state)?;
    }
    payload.length(state.applied_transactions().len())?;
    for (transaction, signature) in state.applied_transactions() {
        if *transaction == 0 {
            return Err(Error::ArtifactFormat {
                format: STATE_FORMAT,
                field: "appliedTransactions.transaction".to_owned(),
            });
        }
        payload.u128(*transaction);
        payload.bytes(signature);
    }
    encode_ecology(&mut payload, state.ecology())?;
    encode_frame(
        STATE_FORMAT,
        STATE_MAGIC,
        STATE_VERSION,
        state_schema_identity(),
        &payload.finish(),
        STATE_COMMIT,
    )
}

/// The generation identity a framed snapshot names, read before deciding what to do about it.
pub(super) fn declared_manifest_identity(bytes: &[u8]) -> Result<[u8; 32]> {
    let payload = decode_frame(
        bytes,
        STATE_FORMAT,
        STATE_MAGIC,
        STATE_VERSION,
        state_schema_identity(),
        STATE_COMMIT,
    )?;
    BinaryReader::new(payload, STATE_FORMAT).array()
}

pub(super) fn decode_state(bytes: &[u8], expected_manifest: [u8; 32]) -> Result<VegetationState> {
    let payload = decode_frame(
        bytes,
        STATE_FORMAT,
        STATE_MAGIC,
        STATE_VERSION,
        state_schema_identity(),
        STATE_COMMIT,
    )?;
    let mut reader = BinaryReader::new(payload, STATE_FORMAT);
    let manifest_identity = reader.array()?;
    if manifest_identity != expected_manifest {
        return Err(Error::ManifestMismatch);
    }
    let cell_count = reader.count(57)?;
    let mut cells = BTreeMap::new();
    let mut previous_cell = None;
    for _ in 0..cell_count {
        let (cell, state) = decode_cell_state(&mut reader)?;
        if previous_cell.is_some_and(|previous| previous >= cell)
            || cells.insert(cell, state).is_some()
        {
            return non_canonical(STATE_FORMAT, "cells.order");
        }
        previous_cell = Some(cell);
    }
    let transaction_count = reader.count(48)?;
    let mut applied_transactions = BTreeMap::new();
    let mut previous_transaction = None;
    for _ in 0..transaction_count {
        let transaction = reader.u128()?;
        let signature = reader.array()?;
        if transaction == 0
            || previous_transaction.is_some_and(|previous| previous >= transaction)
            || applied_transactions
                .insert(transaction, signature)
                .is_some()
        {
            return non_canonical(STATE_FORMAT, "appliedTransactions.order");
        }
        previous_transaction = Some(transaction);
    }
    let ecology = decode_ecology(&mut reader)?;
    reader.complete()?;
    let state = VegetationState::from_canonical_parts(
        manifest_identity,
        cells,
        applied_transactions,
        ecology,
    );
    if encode_state(&state)? != bytes {
        return non_canonical(STATE_FORMAT, "canonicalBytes");
    }
    Ok(state)
}

/// The ecology section, written after the transactions so the frame stays append-structured.
fn encode_ecology(writer: &mut BinaryWriter, ecology: &crate::EcologyState) -> Result<()> {
    writer.u32(ecology.version());
    writer.u64(ecology.clock().tick());
    writer.length(ecology.summaries().len())?;
    for (cell, summary) in ecology.summaries() {
        writer.cell(*cell);
        writer.u64(summary.tick);
        writer.u32(summary.plants);
        for value in [
            summary.canopy,
            summary.roots,
            summary.health,
            summary.moisture,
            summary.fuel,
        ] {
            writer.bytes(&value.canonical_bytes());
        }
        writer.length(summary.families.len())?;
        for presence in &summary.families {
            writer.u64(presence.family);
            writer.bytes(&presence.canopy.canonical_bytes());
            writer.bytes(&presence.health.canonical_bytes());
        }
    }
    Ok(())
}

fn decode_ecology(reader: &mut BinaryReader<'_>) -> Result<crate::EcologyState> {
    let version = reader.u32()?;
    let clock = crate::EcologyClock::at(reader.u64()?);
    let count = reader.count(26)?;
    let mut summaries = BTreeMap::new();
    let mut previous = None;
    for _ in 0..count {
        let cell = reader.cell()?;
        let summary = crate::EcologyCellSummary {
            tick: reader.u64()?,
            plants: reader.u32()?,
            canopy: UnitInterval::from_bits(reader.u16()?),
            roots: UnitInterval::from_bits(reader.u16()?),
            health: UnitInterval::from_bits(reader.u16()?),
            moisture: UnitInterval::from_bits(reader.u16()?),
            fuel: UnitInterval::from_bits(reader.u16()?),
            families: {
                let count = reader.count(12)?;
                let mut families: Vec<crate::EcologyFamilyPresence> = Vec::with_capacity(count);
                for _ in 0..count {
                    let presence = crate::EcologyFamilyPresence {
                        family: reader.u64()?,
                        canopy: UnitInterval::from_bits(reader.u16()?),
                        health: UnitInterval::from_bits(reader.u16()?),
                    };
                    if families
                        .last()
                        .is_some_and(|last| last.family >= presence.family)
                    {
                        return non_canonical(STATE_FORMAT, "ecology.summaries.families.order");
                    }
                    families.push(presence);
                }
                families
            },
        };
        if previous.is_some_and(|previous| previous >= cell)
            || summaries.insert(cell, summary).is_some()
        {
            return non_canonical(STATE_FORMAT, "ecology.summaries.order");
        }
        previous = Some(cell);
    }
    crate::EcologyState::from_parts(version, clock, summaries)
}

fn encode_cell_state(
    writer: &mut BinaryWriter,
    cell: WorldCellKey,
    state: &VegetationCellState,
) -> Result<()> {
    if state.revision == 0 {
        return Err(Error::ArtifactFormat {
            format: STATE_FORMAT,
            field: "cells.revision".to_owned(),
        });
    }
    writer.cell(cell);
    writer.u64(state.revision);
    writer.length(state.field_tiles.len())?;
    for (key, tile) in &state.field_tiles {
        validate_field_tile(tile)?;
        writer.u128(key.layer);
        encode_field_channel(writer, key.channel);
        writer.u128(key.tile);
        for dimension in tile.dimensions {
            writer.u32(dimension);
        }
        writer.i32(tile.quantum_bits);
        writer.length(tile.values.len())?;
        for value in &tile.values {
            writer.i32(*value);
        }
    }
    writer.length(state.plants.len())?;
    for (id, plant) in &state.plants {
        validate_plant_state(cell, *id, plant)?;
        writer.bytes(&id.bytes());
        encode_plant_state(writer, plant)?;
    }
    writer.length(state.disturbance_masks.len())?;
    for (key, values) in &state.disturbance_masks {
        if key.categories == 0 || values.is_empty() {
            return Err(Error::ArtifactFormat {
                format: STATE_FORMAT,
                field: "cells.disturbanceMasks".to_owned(),
            });
        }
        writer.u32(key.categories);
        writer.u128(key.tile);
        writer.length(values.len())?;
        for value in values {
            writer.u16(*value as u16);
        }
    }
    Ok(())
}

fn decode_cell_state(reader: &mut BinaryReader<'_>) -> Result<(WorldCellKey, VegetationCellState)> {
    let cell = reader.cell()?;
    let revision = reader.u64()?;
    if revision == 0 {
        return non_canonical(STATE_FORMAT, "cells.revision");
    }
    let field_count = reader.count(61)?;
    let mut field_tiles = BTreeMap::new();
    let mut previous_field = None;
    for _ in 0..field_count {
        let key = FieldTileKey {
            layer: reader.u128()?,
            channel: decode_field_channel(reader, STATE_FORMAT)?,
            tile: reader.u128()?,
        };
        let tile = FieldTileState {
            dimensions: [reader.u32()?, reader.u32()?, reader.u32()?],
            quantum_bits: reader.i32()?,
            values: read_i32_values(reader)?,
        };
        validate_field_tile(&tile)?;
        if previous_field.is_some_and(|previous| previous >= key)
            || field_tiles.insert(key, tile).is_some()
        {
            return non_canonical(STATE_FORMAT, "cells.fieldTiles.order");
        }
        previous_field = Some(key);
    }
    let plant_count = reader.count(20)?;
    let mut plants = BTreeMap::new();
    let mut previous_plant = None;
    for _ in 0..plant_count {
        let id = PlantId::from_bytes(reader.array()?)?;
        let plant = decode_plant_state(reader, STATE_FORMAT)?;
        validate_plant_state(cell, id, &plant)?;
        if previous_plant.is_some_and(|previous| previous >= id)
            || plants.insert(id, plant).is_some()
        {
            return non_canonical(STATE_FORMAT, "cells.plants.order");
        }
        previous_plant = Some(id);
    }
    let mask_count = reader.count(30)?;
    let mut disturbance_masks = BTreeMap::new();
    let mut previous_mask = None;
    for _ in 0..mask_count {
        let key = DisturbanceTileKey {
            categories: reader.u32()?,
            tile: reader.u128()?,
        };
        let value_count = reader.count(2)?;
        let mut values = Vec::with_capacity(value_count);
        for _ in 0..value_count {
            values.push(reader.u16()? as i16);
        }
        if key.categories == 0
            || values.is_empty()
            || previous_mask.is_some_and(|previous| previous >= key)
            || disturbance_masks.insert(key, values).is_some()
        {
            return non_canonical(STATE_FORMAT, "cells.disturbanceMasks.order");
        }
        previous_mask = Some(key);
    }
    Ok((
        cell,
        VegetationCellState {
            revision,
            field_tiles,
            plants,
            disturbance_masks,
        },
    ))
}

fn validate_field_tile(tile: &FieldTileState) -> Result<()> {
    let expected = tile
        .dimensions
        .iter()
        .try_fold(1_u64, |product, dimension| {
            product.checked_mul(u64::from(*dimension))
        })
        .ok_or(Error::NumericOverflow)?;
    if expected == 0 || usize::try_from(expected).ok() != Some(tile.values.len()) {
        return Err(Error::ArtifactFormat {
            format: STATE_FORMAT,
            field: "cells.fieldTiles.dimensions".to_owned(),
        });
    }
    Ok(())
}

fn validate_plant_state(
    cell: WorldCellKey,
    id: PlantId,
    state: &PlantPersistentState,
) -> Result<()> {
    id.namespace()?;
    if let Some(point) = &state.addition {
        point.validate()?;
        if point.id != id || point.owner != cell {
            return Err(Error::ArtifactFormat {
                format: STATE_FORMAT,
                field: "cells.plants.additionIdentity".to_owned(),
            });
        }
    }
    if let Some((position, _, scale)) = state.transform
        && (position.cell() != cell || scale.iter().any(|value| value.bits() <= 0))
    {
        return Err(Error::ArtifactFormat {
            format: STATE_FORMAT,
            field: "cells.plants.transform".to_owned(),
        });
    }
    if let Some(promotion) = state.promotion_origin
        && (promotion.position.cell() != cell
            || promotion.scale.iter().any(|value| value.bits() <= 0))
    {
        return Err(Error::ArtifactFormat {
            format: STATE_FORMAT,
            field: "cells.plants.promotionOrigin".to_owned(),
        });
    }
    Ok(())
}

pub(super) fn encode_plant_state(
    writer: &mut BinaryWriter,
    state: &PlantPersistentState,
) -> Result<()> {
    encode_optional_point(writer, state.addition.as_ref())?;
    writer.bool(state.tombstoned);
    writer.bool(state.transform.is_some());
    if let Some((position, orientation, scale)) = state.transform {
        encode_position(writer, position);
        encode_orientation(writer, orientation);
        encode_vec3(writer, scale);
    }
    encode_option_u32(writer, state.lifecycle.map(|value| value as u32));
    encode_option_u32(writer, state.phenotype);
    encode_option_u64(writer, state.ecology_tick);
    encode_option_unit(writer, state.health);
    encode_option_unit(writer, state.moisture);
    encode_option_unit(writer, state.fuel);
    encode_option_u32(writer, state.interaction_policy.map(|value| value as u32));
    writer.bool(state.ignited);
    writer.bool(state.promotion_origin.is_some());
    if let Some(promotion) = state.promotion_origin {
        encode_promotion(writer, promotion);
    }
    Ok(())
}

pub(super) fn decode_plant_state(
    reader: &mut BinaryReader<'_>,
    format: &'static str,
) -> Result<PlantPersistentState> {
    let addition = decode_optional_point(reader, format)?;
    let tombstoned = reader.bool()?;
    let transform = if reader.bool()? {
        Some((
            decode_position(reader)?,
            decode_orientation(reader)?,
            decode_vec3(reader)?,
        ))
    } else {
        None
    };
    let lifecycle = decode_option_u32(reader)?
        .map(PlantLifecycle::try_from)
        .transpose()?;
    let phenotype = decode_option_u32(reader)?;
    let ecology_tick = decode_option_u64(reader)?;
    let health = decode_option_unit(reader)?;
    let moisture = decode_option_unit(reader)?;
    let fuel = decode_option_unit(reader)?;
    let interaction_policy = decode_option_u32(reader)?
        .map(InteractionPolicy::try_from)
        .transpose()?;
    let ignited = reader.bool()?;
    let promotion_origin = if reader.bool()? {
        Some(decode_promotion(reader)?)
    } else {
        None
    };
    Ok(PlantPersistentState {
        addition,
        tombstoned,
        transform,
        lifecycle,
        phenotype,
        ecology_tick,
        health,
        moisture,
        fuel,
        interaction_policy,
        ignited,
        promotion_origin,
    })
}
