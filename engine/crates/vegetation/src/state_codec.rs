//! Canonical persistent-state containers for runtime vegetation.

use std::collections::BTreeMap;

use saffron_spatial::{DecisionScalar, FieldChannel, UnitInterval, WorldCellKey, WorldPosition};

use crate::binary::{BinaryReader, BinaryWriter};
use crate::mutation::{mutation_tag, record_order_key};
use crate::{
    ContentHash, CookVersionSet, DisturbanceTileKey, Error, FieldTileKey, FieldTileState,
    InteractionPolicy, MutationHeader, PlantId, PlantLifecycle, PlantPersistentState, PlantPoint,
    PlantPointColumns, PromotionOriginState, QuantizedOrientation, Result, SaveStateEnvelope,
    VegetationBaseManifest, VegetationCellState, VegetationMutation, VegetationMutationRecord,
    VegetationState, VegetationStateBinding,
};

const STATE_FORMAT: &str = "vegetation persistent state";
const STATE_MAGIC: &[u8; 8] = b"SVEGST01";
const STATE_COMMIT: &[u8; 8] = b"SVEGSC01";
const STATE_VERSION: u32 = 1;
const SAVE_FORMAT: &str = "vegetation save state";
const SAVE_MAGIC: &[u8; 8] = b"SVEGSV01";
const SAVE_COMMIT: &[u8; 8] = b"SVEGVC01";
const SAVE_VERSION: u32 = 1;

type PersistenceOrderKey = (u64, u128, u128, ([u8; 25], u8, [u8; 16], u128));

fn state_schema_identity() -> ContentHash {
    ContentHash::of(
        b"saffron-anima/vegetation-state/schema/v1/manifest+cells+field-tiles+plant-deltas+disturbance-masks+applied-transactions",
    )
}

fn save_schema_identity() -> ContentHash {
    ContentHash::of(
        b"saffron-anima/vegetation-save/schema/v1/manifest+graph+versions+seed-namespaces+framed-snapshot+ordered-mutation-tail",
    )
}

impl VegetationStateBinding {
    /// Builds the exact persistence compatibility identity for one immutable base manifest.
    pub fn from_manifest(manifest: &VegetationBaseManifest) -> Result<Self> {
        let manifest_identity = manifest.identity()?;
        let mut seeds = manifest.seed_namespaces.clone();
        seeds.sort_unstable_by(|left, right| {
            left.name
                .cmp(&right.name)
                .then(left.namespace.cmp(&right.namespace))
        });
        let mut writer = BinaryWriter::new();
        writer.bytes(b"saffron-anima/vegetation-seed-namespaces/v1\0");
        writer.length(seeds.len())?;
        for seed in seeds {
            writer.string(&seed.name)?;
            writer.u128(seed.namespace);
        }
        Ok(Self {
            manifest_identity,
            cook_graph_identity: manifest.cook_graph_hash,
            versions: manifest.versions,
            seed_namespaces_identity: ContentHash::of(&writer.finish()),
        })
    }

    fn validate(self) -> Result<()> {
        if self.manifest_identity.is_zero()
            || self.cook_graph_identity.is_zero()
            || self.seed_namespaces_identity.is_zero()
        {
            return Err(Error::ArtifactFormat {
                format: SAVE_FORMAT,
                field: "binding.identity".to_owned(),
            });
        }
        self.versions.validate()
    }

    fn require_exact(self, expected: Self) -> Result<()> {
        if self.manifest_identity != expected.manifest_identity {
            return Err(Error::ManifestMismatch);
        }
        if self.cook_graph_identity != expected.cook_graph_identity {
            return Err(Error::ArtifactFormat {
                format: SAVE_FORMAT,
                field: "binding.cookGraphIdentity".to_owned(),
            });
        }
        if self.versions != expected.versions {
            return Err(Error::ArtifactFormat {
                format: SAVE_FORMAT,
                field: "binding.versions".to_owned(),
            });
        }
        if self.seed_namespaces_identity != expected.seed_namespaces_identity {
            return Err(Error::ArtifactFormat {
                format: SAVE_FORMAT,
                field: "binding.seedNamespacesIdentity".to_owned(),
            });
        }
        Ok(())
    }
}

impl VegetationState {
    /// Strictly decodes a canonical snapshot for the expected immutable manifest.
    pub fn from_canonical_bytes(bytes: &[u8], expected_manifest: [u8; 32]) -> Result<Self> {
        decode_state(bytes, expected_manifest)
    }
}

impl SaveStateEnvelope {
    /// Writes the canonical, interruption-detecting snapshot-plus-tail container.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        encode_save(self)
    }

    /// Strictly decodes a save only when every deterministic binding matches.
    pub fn from_canonical_bytes(bytes: &[u8], expected: VegetationStateBinding) -> Result<Self> {
        decode_save(bytes, expected)
    }
}

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
    encode_frame(
        STATE_FORMAT,
        STATE_MAGIC,
        STATE_VERSION,
        state_schema_identity(),
        &payload.finish(),
        STATE_COMMIT,
    )
}

fn decode_state(bytes: &[u8], expected_manifest: [u8; 32]) -> Result<VegetationState> {
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
    reader.complete()?;
    let state =
        VegetationState::from_canonical_parts(manifest_identity, cells, applied_transactions);
    if encode_state(&state)? != bytes {
        return non_canonical(STATE_FORMAT, "canonicalBytes");
    }
    Ok(state)
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
        id.namespace()?;
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
        let plant = decode_plant_state(reader)?;
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

fn encode_plant_state(writer: &mut BinaryWriter, state: &PlantPersistentState) -> Result<()> {
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
    writer.bool(state.promotion_origin.is_some());
    if let Some(promotion) = state.promotion_origin {
        encode_promotion(writer, promotion);
    }
    Ok(())
}

fn decode_plant_state(reader: &mut BinaryReader<'_>) -> Result<PlantPersistentState> {
    let addition = decode_optional_point(reader, STATE_FORMAT)?;
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
        promotion_origin,
    })
}

fn encode_save(envelope: &SaveStateEnvelope) -> Result<Vec<u8>> {
    envelope.binding.validate()?;
    envelope.reduced_state()?;
    let mut tail = envelope.tail.clone();
    tail.sort_by_key(persistence_order_key);

    let mut payload = BinaryWriter::new();
    encode_binding(&mut payload, envelope.binding);
    let snapshot = encode_state(&envelope.snapshot)?;
    payload.length(snapshot.len())?;
    payload.bytes(&snapshot);
    payload.length(tail.len())?;
    for record in &tail {
        let encoded = encode_record(record)?;
        payload.length(encoded.len())?;
        payload.bytes(&encoded);
    }
    encode_frame(
        SAVE_FORMAT,
        SAVE_MAGIC,
        SAVE_VERSION,
        save_schema_identity(),
        &payload.finish(),
        SAVE_COMMIT,
    )
}

fn decode_save(bytes: &[u8], expected: VegetationStateBinding) -> Result<SaveStateEnvelope> {
    expected.validate()?;
    let payload = decode_frame(
        bytes,
        SAVE_FORMAT,
        SAVE_MAGIC,
        SAVE_VERSION,
        save_schema_identity(),
        SAVE_COMMIT,
    )?;
    let mut reader = BinaryReader::new(payload, SAVE_FORMAT);
    let binding = decode_binding(&mut reader)?;
    binding.validate()?;
    binding.require_exact(expected)?;
    let snapshot_length = reader.length()?;
    let snapshot = decode_state(
        reader.take(snapshot_length)?,
        binding.manifest_identity.bytes(),
    )?;
    let tail_count = reader.count(98)?;
    let mut tail = Vec::with_capacity(tail_count);
    for _ in 0..tail_count {
        let record_length = reader.length()?;
        tail.push(decode_record(reader.take(record_length)?)?);
    }
    reader.complete()?;
    if !tail.is_sorted_by_key(persistence_order_key) {
        return non_canonical(SAVE_FORMAT, "tail.order");
    }
    let envelope = SaveStateEnvelope {
        binding,
        snapshot,
        tail,
    };
    envelope.reduced_state()?;
    if encode_save(&envelope)? != bytes {
        return non_canonical(SAVE_FORMAT, "canonicalBytes");
    }
    Ok(envelope)
}

fn encode_binding(writer: &mut BinaryWriter, binding: VegetationStateBinding) {
    writer.bytes(&binding.manifest_identity.bytes());
    writer.bytes(&binding.cook_graph_identity.bytes());
    binding.versions.encode(writer);
    writer.bytes(&binding.seed_namespaces_identity.bytes());
}

fn decode_binding(reader: &mut BinaryReader<'_>) -> Result<VegetationStateBinding> {
    Ok(VegetationStateBinding {
        manifest_identity: ContentHash::new(reader.array()?),
        cook_graph_identity: ContentHash::new(reader.array()?),
        versions: CookVersionSet::decode(reader)?,
        seed_namespaces_identity: ContentHash::new(reader.array()?),
    })
}

fn persistence_order_key(record: &VegetationMutationRecord) -> PersistenceOrderKey {
    (
        record.header.logical_tick,
        record.header.authority,
        record.header.transaction,
        record_order_key(record),
    )
}

fn encode_record(record: &VegetationMutationRecord) -> Result<Vec<u8>> {
    let mut writer = BinaryWriter::new();
    writer.cell(record.header.cell);
    writer.u128(record.header.transaction);
    writer.u128(record.header.authority);
    writer.u64(record.header.logical_tick);
    writer.u128(record.header.idempotency_key);
    encode_option_u64(&mut writer, record.header.base_revision);
    writer.u8(mutation_tag(&record.mutation));
    match &record.mutation {
        VegetationMutation::FieldTilePatch {
            layer,
            channel,
            tile,
            dimensions,
            quantum_bits,
            values,
        } => {
            writer.u128(*layer);
            encode_field_channel(&mut writer, *channel);
            writer.u128(*tile);
            for dimension in dimensions {
                writer.u32(*dimension);
            }
            writer.i32(*quantum_bits);
            writer.length(values.len())?;
            for value in values {
                writer.i32(*value);
            }
        }
        VegetationMutation::AnchorAddition(point) | VegetationMutation::Planting(point) => {
            encode_point(&mut writer, point)?;
        }
        VegetationMutation::Tombstone { plant } => encode_plant_id(&mut writer, *plant),
        VegetationMutation::TransformOverride {
            plant,
            position,
            orientation,
            scale,
        } => {
            encode_plant_id(&mut writer, *plant);
            encode_position(&mut writer, *position);
            encode_orientation(&mut writer, *orientation);
            encode_vec3(&mut writer, *scale);
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
            encode_plant_id(&mut writer, *plant);
            encode_option_u32(&mut writer, lifecycle.map(|value| value as u32));
            encode_option_u32(&mut writer, *phenotype);
            encode_option_unit(&mut writer, *health);
            encode_option_unit(&mut writer, *moisture);
            encode_option_unit(&mut writer, *fuel);
            encode_option_u32(&mut writer, interaction_policy.map(|value| value as u32));
        }
        VegetationMutation::Damage {
            plant,
            amount,
            phenotype,
        } => {
            encode_plant_id(&mut writer, *plant);
            writer.u16(amount.bits());
            encode_option_u32(&mut writer, *phenotype);
        }
        VegetationMutation::MoistureFuel {
            plant,
            moisture,
            fuel,
        } => {
            encode_plant_id(&mut writer, *plant);
            writer.u16(moisture.bits());
            writer.u16(fuel.bits());
        }
        VegetationMutation::LifecycleTransition {
            plant,
            from,
            to,
            ecology_tick,
        } => {
            encode_plant_id(&mut writer, *plant);
            encode_option_u32(&mut writer, from.map(|value| value as u32));
            writer.u32(*to as u32);
            writer.u64(*ecology_tick);
        }
        VegetationMutation::Harvest { plant, phenotype } => {
            encode_plant_id(&mut writer, *plant);
            writer.u32(*phenotype);
        }
        VegetationMutation::Burn {
            plant,
            phenotype,
            remaining_fuel,
        } => {
            encode_plant_id(&mut writer, *plant);
            writer.u32(*phenotype);
            writer.u16(remaining_fuel.bits());
        }
        VegetationMutation::Regrow {
            plant,
            lifecycle,
            phenotype,
            ecology_tick,
        } => {
            encode_plant_id(&mut writer, *plant);
            writer.u32(*lifecycle as u32);
            writer.u32(*phenotype);
            writer.u64(*ecology_tick);
        }
        VegetationMutation::PromotionOriginState { plant, state } => {
            encode_plant_id(&mut writer, *plant);
            encode_promotion(&mut writer, *state);
        }
        VegetationMutation::DisturbanceMask {
            categories,
            tile,
            values,
        } => {
            writer.u32(*categories);
            writer.u128(*tile);
            writer.length(values.len())?;
            for value in values {
                writer.u16(*value as u16);
            }
        }
    }
    Ok(writer.finish())
}

fn decode_record(bytes: &[u8]) -> Result<VegetationMutationRecord> {
    let mut reader = BinaryReader::new(bytes, SAVE_FORMAT);
    let header = MutationHeader {
        cell: reader.cell()?,
        transaction: reader.u128()?,
        authority: reader.u128()?,
        logical_tick: reader.u64()?,
        idempotency_key: reader.u128()?,
        base_revision: decode_option_u64(&mut reader)?,
    };
    let mutation = match reader.u8()? {
        0 => VegetationMutation::FieldTilePatch {
            layer: reader.u128()?,
            channel: decode_field_channel(&mut reader, SAVE_FORMAT)?,
            tile: reader.u128()?,
            dimensions: [reader.u32()?, reader.u32()?, reader.u32()?],
            quantum_bits: reader.i32()?,
            values: read_i32_values(&mut reader)?,
        },
        1 => VegetationMutation::AnchorAddition(decode_point(&mut reader, SAVE_FORMAT)?),
        2 => VegetationMutation::Tombstone {
            plant: decode_plant_id(&mut reader)?,
        },
        3 => VegetationMutation::TransformOverride {
            plant: decode_plant_id(&mut reader)?,
            position: decode_position(&mut reader)?,
            orientation: decode_orientation(&mut reader)?,
            scale: decode_vec3(&mut reader)?,
        },
        4 => VegetationMutation::StateOverride {
            plant: decode_plant_id(&mut reader)?,
            lifecycle: decode_option_u32(&mut reader)?
                .map(PlantLifecycle::try_from)
                .transpose()?,
            phenotype: decode_option_u32(&mut reader)?,
            health: decode_option_unit(&mut reader)?,
            moisture: decode_option_unit(&mut reader)?,
            fuel: decode_option_unit(&mut reader)?,
            interaction_policy: decode_option_u32(&mut reader)?
                .map(InteractionPolicy::try_from)
                .transpose()?,
        },
        5 => VegetationMutation::Planting(decode_point(&mut reader, SAVE_FORMAT)?),
        6 => VegetationMutation::Damage {
            plant: decode_plant_id(&mut reader)?,
            amount: UnitInterval::from_bits(reader.u16()?),
            phenotype: decode_option_u32(&mut reader)?,
        },
        7 => VegetationMutation::MoistureFuel {
            plant: decode_plant_id(&mut reader)?,
            moisture: UnitInterval::from_bits(reader.u16()?),
            fuel: UnitInterval::from_bits(reader.u16()?),
        },
        8 => VegetationMutation::LifecycleTransition {
            plant: decode_plant_id(&mut reader)?,
            from: decode_option_u32(&mut reader)?
                .map(PlantLifecycle::try_from)
                .transpose()?,
            to: PlantLifecycle::try_from(reader.u32()?)?,
            ecology_tick: reader.u64()?,
        },
        9 => VegetationMutation::Harvest {
            plant: decode_plant_id(&mut reader)?,
            phenotype: reader.u32()?,
        },
        10 => VegetationMutation::Burn {
            plant: decode_plant_id(&mut reader)?,
            phenotype: reader.u32()?,
            remaining_fuel: UnitInterval::from_bits(reader.u16()?),
        },
        11 => VegetationMutation::Regrow {
            plant: decode_plant_id(&mut reader)?,
            lifecycle: PlantLifecycle::try_from(reader.u32()?)?,
            phenotype: reader.u32()?,
            ecology_tick: reader.u64()?,
        },
        12 => VegetationMutation::PromotionOriginState {
            plant: decode_plant_id(&mut reader)?,
            state: decode_promotion(&mut reader)?,
        },
        13 => {
            let categories = reader.u32()?;
            let tile = reader.u128()?;
            let value_count = reader.count(2)?;
            let mut values = Vec::with_capacity(value_count);
            for _ in 0..value_count {
                values.push(reader.u16()? as i16);
            }
            VegetationMutation::DisturbanceMask {
                categories,
                tile,
                values,
            }
        }
        _ => {
            return Err(Error::ArtifactFormat {
                format: SAVE_FORMAT,
                field: "tail.mutationTag".to_owned(),
            });
        }
    };
    reader.complete()?;
    let record = VegetationMutationRecord { header, mutation };
    if encode_record(&record)? != bytes {
        return non_canonical(SAVE_FORMAT, "tail.record");
    }
    Ok(record)
}

fn encode_frame(
    format: &'static str,
    magic: &[u8; 8],
    version: u32,
    schema: ContentHash,
    payload: &[u8],
    commit: &[u8; 8],
) -> Result<Vec<u8>> {
    if payload.is_empty() {
        return Err(Error::ArtifactFormat {
            format,
            field: "payload".to_owned(),
        });
    }
    let mut writer = BinaryWriter::with_capacity(
        8_usize
            .checked_add(4 + 32 + 8 + payload.len() + 32 + 8)
            .ok_or(Error::NumericOverflow)?,
    );
    writer.bytes(magic);
    writer.u32(version);
    writer.bytes(&schema.bytes());
    writer.length(payload.len())?;
    writer.bytes(payload);
    writer.bytes(&ContentHash::of(payload).bytes());
    writer.bytes(commit);
    Ok(writer.finish())
}

fn decode_frame<'a>(
    bytes: &'a [u8],
    format: &'static str,
    magic: &[u8; 8],
    version: u32,
    schema: ContentHash,
    commit: &[u8; 8],
) -> Result<&'a [u8]> {
    let mut reader = BinaryReader::new(bytes, format);
    reader.expect(magic, "magic")?;
    let found = reader.u32()?;
    if found != version {
        return Err(Error::FormatVersion {
            format,
            found,
            expected: version,
        });
    }
    if ContentHash::new(reader.array()?) != schema {
        return Err(Error::ArtifactSchema { format });
    }
    let payload_length = reader.length()?;
    let payload = reader.take(payload_length)?;
    let payload_hash = ContentHash::new(reader.array()?);
    reader.expect(commit, "commitMarker")?;
    reader.complete()?;
    if payload_hash != ContentHash::of(payload) {
        return Err(Error::ArtifactHashMismatch {
            format,
            subject: "payload".to_owned(),
        });
    }
    Ok(payload)
}

fn encode_point(writer: &mut BinaryWriter, point: &PlantPoint) -> Result<()> {
    point.validate()?;
    let bytes = PlantPointColumns::from_points(vec![point.clone()])?.canonical_bytes()?;
    writer.length(bytes.len())?;
    writer.bytes(&bytes);
    Ok(())
}

fn decode_point(reader: &mut BinaryReader<'_>, format: &'static str) -> Result<PlantPoint> {
    let length = reader.length()?;
    let columns = PlantPointColumns::from_canonical_bytes(reader.take(length)?)?;
    if columns.row_count()? != 1 {
        return Err(Error::ArtifactFormat {
            format,
            field: "point.rows".to_owned(),
        });
    }
    columns.point(0)
}

fn encode_optional_point(writer: &mut BinaryWriter, point: Option<&PlantPoint>) -> Result<()> {
    writer.bool(point.is_some());
    if let Some(point) = point {
        encode_point(writer, point)?;
    }
    Ok(())
}

fn decode_optional_point(
    reader: &mut BinaryReader<'_>,
    format: &'static str,
) -> Result<Option<PlantPoint>> {
    if reader.bool()? {
        Ok(Some(decode_point(reader, format)?))
    } else {
        Ok(None)
    }
}

fn encode_plant_id(writer: &mut BinaryWriter, plant: PlantId) {
    writer.bytes(&plant.bytes());
}

fn decode_plant_id(reader: &mut BinaryReader<'_>) -> Result<PlantId> {
    PlantId::from_bytes(reader.array()?)
}

fn encode_position(writer: &mut BinaryWriter, position: WorldPosition) {
    for tick in position.global_ticks() {
        writer.i128(tick);
    }
}

fn decode_position(reader: &mut BinaryReader<'_>) -> Result<WorldPosition> {
    WorldPosition::from_global_ticks([reader.i128()?, reader.i128()?, reader.i128()?])
        .map_err(Into::into)
}

fn encode_orientation(writer: &mut BinaryWriter, orientation: QuantizedOrientation) {
    for lane in orientation.bits() {
        writer.u16(lane as u16);
    }
}

fn decode_orientation(reader: &mut BinaryReader<'_>) -> Result<QuantizedOrientation> {
    QuantizedOrientation::new([
        reader.u16()? as i16,
        reader.u16()? as i16,
        reader.u16()? as i16,
        reader.u16()? as i16,
    ])
}

fn encode_vec3(writer: &mut BinaryWriter, values: [DecisionScalar; 3]) {
    for value in values {
        writer.i32(value.bits());
    }
}

fn decode_vec3(reader: &mut BinaryReader<'_>) -> Result<[DecisionScalar; 3]> {
    Ok([
        DecisionScalar::from_bits(reader.i32()?),
        DecisionScalar::from_bits(reader.i32()?),
        DecisionScalar::from_bits(reader.i32()?),
    ])
}

fn encode_promotion(writer: &mut BinaryWriter, state: PromotionOriginState) {
    encode_position(writer, state.position);
    encode_orientation(writer, state.orientation);
    encode_vec3(writer, state.scale);
    encode_vec3(writer, state.linear_velocity);
    encode_vec3(writer, state.angular_velocity);
}

fn decode_promotion(reader: &mut BinaryReader<'_>) -> Result<PromotionOriginState> {
    Ok(PromotionOriginState {
        position: decode_position(reader)?,
        orientation: decode_orientation(reader)?,
        scale: decode_vec3(reader)?,
        linear_velocity: decode_vec3(reader)?,
        angular_velocity: decode_vec3(reader)?,
    })
}

fn encode_field_channel(writer: &mut BinaryWriter, channel: FieldChannel) {
    let (tag, user) = match channel {
        FieldChannel::Altitude => (0, 0),
        FieldChannel::Slope => (1, 0),
        FieldChannel::Curvature => (2, 0),
        FieldChannel::Concavity => (3, 0),
        FieldChannel::Drainage => (4, 0),
        FieldChannel::Moisture => (5, 0),
        FieldChannel::Temperature => (6, 0),
        FieldChannel::Precipitation => (7, 0),
        FieldChannel::Sunlight => (8, 0),
        FieldChannel::Exposure => (9, 0),
        FieldChannel::WaterDistance => (10, 0),
        FieldChannel::WaterDepth => (11, 0),
        FieldChannel::SignedBlocker => (12, 0),
        FieldChannel::SplineDistance => (13, 0),
        FieldChannel::User(value) => (14, value),
    };
    writer.u8(tag);
    writer.u64(user);
}

fn decode_field_channel(
    reader: &mut BinaryReader<'_>,
    format: &'static str,
) -> Result<FieldChannel> {
    let tag = reader.u8()?;
    let user = reader.u64()?;
    match (tag, user) {
        (0, 0) => Ok(FieldChannel::Altitude),
        (1, 0) => Ok(FieldChannel::Slope),
        (2, 0) => Ok(FieldChannel::Curvature),
        (3, 0) => Ok(FieldChannel::Concavity),
        (4, 0) => Ok(FieldChannel::Drainage),
        (5, 0) => Ok(FieldChannel::Moisture),
        (6, 0) => Ok(FieldChannel::Temperature),
        (7, 0) => Ok(FieldChannel::Precipitation),
        (8, 0) => Ok(FieldChannel::Sunlight),
        (9, 0) => Ok(FieldChannel::Exposure),
        (10, 0) => Ok(FieldChannel::WaterDistance),
        (11, 0) => Ok(FieldChannel::WaterDepth),
        (12, 0) => Ok(FieldChannel::SignedBlocker),
        (13, 0) => Ok(FieldChannel::SplineDistance),
        (14, value) => Ok(FieldChannel::User(value)),
        _ => Err(Error::ArtifactFormat {
            format,
            field: "fieldChannel".to_owned(),
        }),
    }
}

fn read_i32_values(reader: &mut BinaryReader<'_>) -> Result<Vec<i32>> {
    let count = reader.count(4)?;
    let mut values = Vec::with_capacity(count);
    for _ in 0..count {
        values.push(reader.i32()?);
    }
    Ok(values)
}

fn encode_option_u32(writer: &mut BinaryWriter, value: Option<u32>) {
    writer.bool(value.is_some());
    if let Some(value) = value {
        writer.u32(value);
    }
}

fn decode_option_u32(reader: &mut BinaryReader<'_>) -> Result<Option<u32>> {
    if reader.bool()? {
        Ok(Some(reader.u32()?))
    } else {
        Ok(None)
    }
}

fn encode_option_u64(writer: &mut BinaryWriter, value: Option<u64>) {
    writer.bool(value.is_some());
    if let Some(value) = value {
        writer.u64(value);
    }
}

fn decode_option_u64(reader: &mut BinaryReader<'_>) -> Result<Option<u64>> {
    if reader.bool()? {
        Ok(Some(reader.u64()?))
    } else {
        Ok(None)
    }
}

fn encode_option_unit(writer: &mut BinaryWriter, value: Option<UnitInterval>) {
    writer.bool(value.is_some());
    if let Some(value) = value {
        writer.u16(value.bits());
    }
}

fn decode_option_unit(reader: &mut BinaryReader<'_>) -> Result<Option<UnitInterval>> {
    if reader.bool()? {
        Ok(Some(UnitInterval::from_bits(reader.u16()?)))
    } else {
        Ok(None)
    }
}

fn non_canonical<T>(format: &'static str, field: &str) -> Result<T> {
    Err(Error::ArtifactFormat {
        format,
        field: field.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use saffron_core::Uuid;
    use saffron_spatial::{DecisionScalar, WorldBounds};

    use super::*;
    use crate::{
        CookPlatformProfile, PlantFlags, VegetationBaseManifest, VegetationSeedNamespace,
        reduce_mutations,
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

    fn point(id: PlantId, cell: WorldCellKey) -> PlantPoint {
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
            lifecycle: PlantLifecycle::Sprout,
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
        let planting = record(cell, 1, 0, VegetationMutation::Planting(point(id, cell)));
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
                SaveStateEnvelope::from_canonical_bytes(&bytes[..length], envelope.binding)
                    .is_err()
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
        let position = point(runtime, cell).position;
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
            VegetationMutation::AnchorAddition(point(explicit, cell)),
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
            VegetationMutation::Planting(point(runtime, cell)),
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
        let planted = point(id, cell);
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
}
