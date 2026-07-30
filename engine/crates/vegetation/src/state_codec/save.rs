//! The save container: one framed snapshot plus the ordered mutation tail that replays on top
//! of it.

use saffron_spatial::UnitInterval;

use super::frame::{decode_frame, encode_frame};
use super::state::{decode_state, encode_state};
use super::values::*;
use super::{SAVE_COMMIT, SAVE_FORMAT, SAVE_MAGIC, SAVE_VERSION, save_schema_identity};
use crate::binary::{BinaryReader, BinaryWriter};
use crate::mutation::{mutation_tag, record_order_key};
use crate::{
    ContentHash, CookVersionSet, Error, InteractionPolicy, MutationHeader, PlantLifecycle, Result,
    SaveStateEnvelope, VegetationMutation, VegetationMutationRecord, VegetationStateBinding,
};

type PersistenceOrderKey = (u64, u128, u128, ([u8; 25], u8, [u8; 16], u128));

pub(super) fn encode_save(envelope: &SaveStateEnvelope) -> Result<Vec<u8>> {
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

pub(super) fn decode_save(
    bytes: &[u8],
    expected: VegetationStateBinding,
) -> Result<SaveStateEnvelope> {
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

pub(super) fn encode_record(record: &VegetationMutationRecord) -> Result<Vec<u8>> {
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
        VegetationMutation::Ignite { plant } | VegetationMutation::Extinguish { plant } => {
            encode_plant_id(&mut writer, *plant);
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

pub(super) fn decode_record(bytes: &[u8]) -> Result<VegetationMutationRecord> {
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
        14 => VegetationMutation::Ignite {
            plant: decode_plant_id(&mut reader)?,
        },
        15 => VegetationMutation::Extinguish {
            plant: decode_plant_id(&mut reader)?,
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
