//! Wire conversion for typed vegetation mutations, both ways: the `vegetation-mutate` command's
//! records decode into the reducer's exact vocabulary, and the gesture inverse it replies with
//! encodes back out.

use std::str::FromStr;

use saffron_protocol::{
    FieldChannelDto, FieldChannelKindDto, PlantDeltaDto, PlantPointDto, PlantPromotionOriginDto,
    PlantTransformDto, SurfaceAttachmentDto, VegetationGuid, VegetationMutationDto,
    VegetationMutationHeaderDto, VegetationMutationRecordDto,
};
use saffron_spatial::{
    DecisionScalar, FieldChannel, QuantizedLocalPosition, SurfaceAttachment, SurfacePrimitiveId,
    SurfaceProviderId, SurfaceRevision, UnitInterval, WorldBounds, WorldCellKey, WorldPosition,
};
use saffron_vegetation::{
    MutationHeader, PlantFlags, PlantId, PlantPersistentState, PlantPoint, PromotionOriginState,
    QuantizedOrientation, VegetationMutation, VegetationMutationRecord,
};

use crate::vegetation_cook_dto::world_cell_dto;

use crate::error::{Error, Result};

fn parse_guid(value: &VegetationGuid) -> Result<u128> {
    u128::from_str_radix(&value.0, 16)
        .map_err(|_| Error::command("vegetation GUID is not canonical"))
}

/// Decodes the gesture a mutation batch groups under.
pub(crate) fn parse_gesture(value: &VegetationGuid) -> Result<u128> {
    parse_guid(value)
}

fn parse_u64(value: &str, field: &str) -> Result<u64> {
    value
        .parse::<u64>()
        .map_err(|_| Error::command(format!("{field} is not a u64")))
}

fn parse_i128(value: &str, field: &str) -> Result<i128> {
    value
        .parse::<i128>()
        .map_err(|_| Error::command(format!("{field} is not an i128")))
}

fn parse_cell(value: &saffron_protocol::WorldCellDto) -> Result<WorldCellKey> {
    let mut coordinates = [0_i64; 3];
    for (slot, coordinate) in coordinates.iter_mut().zip(&value.coordinates) {
        *slot = coordinate
            .parse::<i64>()
            .map_err(|_| Error::command("cell coordinate is not an i64"))?;
    }
    WorldCellKey::new(coordinates[0], coordinates[1], coordinates[2], value.level)
        .map_err(Error::command)
}

fn parse_plant(value: &saffron_protocol::PlantId) -> Result<PlantId> {
    PlantId::from_str(&value.0).map_err(Error::from)
}

fn unit(bits: u16) -> UnitInterval {
    UnitInterval::from_bits(bits)
}

fn scalars(bits: [i32; 3]) -> [DecisionScalar; 3] {
    bits.map(DecisionScalar::from_bits)
}

fn orientation(bits: [i16; 4]) -> Result<QuantizedOrientation> {
    QuantizedOrientation::new(bits).map_err(Error::command)
}

fn bounds(value: &saffron_protocol::WorldBoundsDto) -> Result<WorldBounds> {
    let mut minimum = [0_i128; 3];
    let mut maximum = [0_i128; 3];
    for (axis, (low, high)) in minimum.iter_mut().zip(&mut maximum).enumerate() {
        *low = parse_i128(&value.min_ticks[axis], "bounds.minTicks")?;
        *high = parse_i128(&value.max_ticks_exclusive[axis], "bounds.maxTicksExclusive")?;
    }
    WorldBounds::new(minimum, maximum).map_err(Error::command)
}

fn position(transform: &PlantTransformDto) -> Result<WorldPosition> {
    let mut ticks = [0_i128; 3];
    for (tick, value) in ticks.iter_mut().zip(&transform.global_ticks) {
        *tick = parse_i128(value, "transform.globalTicks")?;
    }
    WorldPosition::from_global_ticks(ticks).map_err(Error::command)
}

fn attachment(value: &SurfaceAttachmentDto) -> Result<SurfaceAttachment> {
    SurfaceAttachment::new(
        SurfaceProviderId(parse_u64(&value.provider, "attachment.provider")?),
        SurfacePrimitiveId(parse_u64(&value.primitive, "attachment.primitive")?),
        value.barycentric.map(UnitInterval::from_bits),
        SurfaceRevision(parse_u64(&value.revision, "attachment.revision")?),
    )
    .map_err(Error::command)
}

fn channel(value: &FieldChannelDto) -> Result<FieldChannel> {
    Ok(match value.kind {
        FieldChannelKindDto::Altitude => FieldChannel::Altitude,
        FieldChannelKindDto::Slope => FieldChannel::Slope,
        FieldChannelKindDto::Curvature => FieldChannel::Curvature,
        FieldChannelKindDto::Concavity => FieldChannel::Concavity,
        FieldChannelKindDto::Drainage => FieldChannel::Drainage,
        FieldChannelKindDto::Moisture => FieldChannel::Moisture,
        FieldChannelKindDto::Temperature => FieldChannel::Temperature,
        FieldChannelKindDto::Precipitation => FieldChannel::Precipitation,
        FieldChannelKindDto::Sunlight => FieldChannel::Sunlight,
        FieldChannelKindDto::Exposure => FieldChannel::Exposure,
        FieldChannelKindDto::WaterDistance => FieldChannel::WaterDistance,
        FieldChannelKindDto::WaterDepth => FieldChannel::WaterDepth,
        FieldChannelKindDto::SignedBlocker => FieldChannel::SignedBlocker,
        FieldChannelKindDto::SplineDistance => FieldChannel::SplineDistance,
        FieldChannelKindDto::User => FieldChannel::User(parse_u64(
            value
                .user
                .as_deref()
                .ok_or_else(|| Error::command("user field channel requires a namespace"))?,
            "channel.user",
        )?),
    })
}

pub(crate) fn point_from_dto(value: &PlantPointDto) -> Result<PlantPoint> {
    let owner = parse_cell(&value.owner)?;
    Ok(PlantPoint {
        id: parse_plant(&value.id)?,
        owner,
        position: WorldPosition::new(
            owner,
            QuantizedLocalPosition::new(value.local_position).map_err(Error::command)?,
        )
        .map_err(Error::command)?,
        orientation: orientation(value.orientation)?,
        scale: scalars(value.scale_bits),
        bounds: bounds(&value.bounds)?,
        family: saffron_core::Uuid(value.family.value()),
        variation: value.variation,
        lifecycle: crate::commands_vegetation_runtime::lifecycle_from_dto(value.lifecycle),
        phenotype: value.phenotype,
        representation_class: value.representation_class,
        deterministic_key: parse_guid(&value.deterministic_key)?,
        candidate: parse_u64(&value.candidate, "point.candidate")?,
        parent: value.parent.as_ref().map(parse_plant).transpose()?,
        colony: value.colony.as_ref().map(parse_plant).transpose()?,
        ecology_tick: parse_u64(&value.ecology_tick, "point.ecologyTick")?,
        health: unit(value.health),
        moisture: unit(value.moisture),
        fuel: unit(value.fuel),
        phenology: unit(value.phenology),
        flags: PlantFlags::from_bits(value.flags).map_err(Error::from)?,
        interaction_policy: crate::commands_vegetation_runtime::interaction_from_dto(
            value.interaction_policy,
        ),
        provenance: value.provenance,
        attachment: value.attachment.as_ref().map(attachment).transpose()?,
        surface_projection: scalars(value.surface_projection_bits),
    })
}

fn header(value: &VegetationMutationHeaderDto) -> Result<MutationHeader> {
    Ok(MutationHeader {
        cell: parse_cell(&value.cell)?,
        transaction: parse_guid(&value.transaction)?,
        authority: parse_guid(&value.authority)?,
        logical_tick: parse_u64(&value.logical_tick, "header.logicalTick")?,
        idempotency_key: parse_guid(&value.idempotency_key)?,
        base_revision: value
            .base_revision
            .as_deref()
            .map(|revision| parse_u64(revision, "header.baseRevision"))
            .transpose()?,
    })
}

fn mutation(value: &VegetationMutationDto) -> Result<VegetationMutation> {
    Ok(match value {
        VegetationMutationDto::FieldTilePatch {
            layer,
            channel: channel_dto,
            tile,
            dimensions,
            quantum_bits,
            values,
        } => VegetationMutation::FieldTilePatch {
            layer: parse_guid(layer)?,
            channel: channel(channel_dto)?,
            tile: parse_guid(tile)?,
            dimensions: *dimensions,
            quantum_bits: *quantum_bits,
            values: values.clone(),
        },
        VegetationMutationDto::AnchorAddition { point: dto } => {
            VegetationMutation::AnchorAddition(point_from_dto(dto)?)
        }
        VegetationMutationDto::Tombstone { plant } => VegetationMutation::Tombstone {
            plant: parse_plant(plant)?,
        },
        VegetationMutationDto::TransformOverride { plant, transform } => {
            VegetationMutation::TransformOverride {
                plant: parse_plant(plant)?,
                position: position(transform)?,
                orientation: orientation(transform.orientation)?,
                scale: scalars(transform.scale_bits),
            }
        }
        VegetationMutationDto::StateOverride {
            plant,
            lifecycle,
            phenotype,
            health,
            moisture,
            fuel,
            interaction_policy,
        } => VegetationMutation::StateOverride {
            plant: parse_plant(plant)?,
            lifecycle: lifecycle.map(crate::commands_vegetation_runtime::lifecycle_from_dto),
            phenotype: *phenotype,
            health: health.map(unit),
            moisture: moisture.map(unit),
            fuel: fuel.map(unit),
            interaction_policy: interaction_policy
                .map(crate::commands_vegetation_runtime::interaction_from_dto),
        },
        VegetationMutationDto::Planting { point: dto } => {
            VegetationMutation::Planting(point_from_dto(dto)?)
        }
        VegetationMutationDto::Damage {
            plant,
            amount,
            phenotype,
        } => VegetationMutation::Damage {
            plant: parse_plant(plant)?,
            amount: unit(*amount),
            phenotype: *phenotype,
        },
        VegetationMutationDto::MoistureFuel {
            plant,
            moisture,
            fuel,
        } => VegetationMutation::MoistureFuel {
            plant: parse_plant(plant)?,
            moisture: unit(*moisture),
            fuel: unit(*fuel),
        },
        VegetationMutationDto::LifecycleTransition {
            plant,
            from,
            to,
            ecology_tick,
        } => VegetationMutation::LifecycleTransition {
            plant: parse_plant(plant)?,
            from: from.map(crate::commands_vegetation_runtime::lifecycle_from_dto),
            to: crate::commands_vegetation_runtime::lifecycle_from_dto(*to),
            ecology_tick: parse_u64(ecology_tick, "mutation.ecologyTick")?,
        },
        VegetationMutationDto::Harvest { plant, phenotype } => VegetationMutation::Harvest {
            plant: parse_plant(plant)?,
            phenotype: *phenotype,
        },
        VegetationMutationDto::Burn {
            plant,
            phenotype,
            remaining_fuel,
        } => VegetationMutation::Burn {
            plant: parse_plant(plant)?,
            phenotype: *phenotype,
            remaining_fuel: unit(*remaining_fuel),
        },
        VegetationMutationDto::Ignite { plant } => VegetationMutation::Ignite {
            plant: parse_plant(plant)?,
        },
        VegetationMutationDto::Extinguish { plant } => VegetationMutation::Extinguish {
            plant: parse_plant(plant)?,
        },
        VegetationMutationDto::Regrow {
            plant,
            lifecycle,
            phenotype,
            ecology_tick,
        } => VegetationMutation::Regrow {
            plant: parse_plant(plant)?,
            lifecycle: crate::commands_vegetation_runtime::lifecycle_from_dto(*lifecycle),
            phenotype: *phenotype,
            ecology_tick: parse_u64(ecology_tick, "mutation.ecologyTick")?,
        },
        VegetationMutationDto::PromotionOriginState { plant, origin } => {
            VegetationMutation::PromotionOriginState {
                plant: parse_plant(plant)?,
                state: promotion_from_dto(origin)?,
            }
        }
        VegetationMutationDto::DisturbanceMask {
            categories,
            tile,
            values,
        } => VegetationMutation::DisturbanceMask {
            categories: *categories,
            tile: parse_guid(tile)?,
            values: values.clone(),
        },
        VegetationMutationDto::PlantDeltaRestore { plant, delta } => {
            VegetationMutation::PlantDeltaRestore {
                plant: parse_plant(plant)?,
                delta: delta
                    .as_deref()
                    .map(delta_from_dto)
                    .transpose()?
                    .map(Box::new),
            }
        }
        VegetationMutationDto::FieldTileClear {
            layer,
            channel: channel_dto,
            tile,
        } => VegetationMutation::FieldTileClear {
            layer: parse_guid(layer)?,
            channel: channel(channel_dto)?,
            tile: parse_guid(tile)?,
        },
        VegetationMutationDto::DisturbanceMaskClear { categories, tile } => {
            VegetationMutation::DisturbanceMaskClear {
                categories: *categories,
                tile: parse_guid(tile)?,
            }
        }
    })
}

fn promotion_from_dto(value: &PlantPromotionOriginDto) -> Result<PromotionOriginState> {
    Ok(PromotionOriginState {
        position: position(&value.transform)?,
        orientation: orientation(value.transform.orientation)?,
        scale: scalars(value.transform.scale_bits),
        linear_velocity: scalars(value.linear_velocity_bits),
        angular_velocity: scalars(value.angular_velocity_bits),
    })
}

fn delta_from_dto(value: &PlantDeltaDto) -> Result<PlantPersistentState> {
    let transform = value
        .transform
        .as_ref()
        .map(|transform| {
            Ok::<_, Error>((
                position(transform)?,
                orientation(transform.orientation)?,
                scalars(transform.scale_bits),
            ))
        })
        .transpose()?;
    Ok(PlantPersistentState {
        addition: value.addition.as_ref().map(point_from_dto).transpose()?,
        tombstoned: value.tombstoned,
        transform,
        lifecycle: value
            .lifecycle
            .map(crate::commands_vegetation_runtime::lifecycle_from_dto),
        phenotype: value.phenotype,
        ecology_tick: value
            .ecology_tick
            .as_deref()
            .map(|tick| parse_u64(tick, "delta.ecologyTick"))
            .transpose()?,
        health: value.health.map(unit),
        moisture: value.moisture.map(unit),
        fuel: value.fuel.map(unit),
        interaction_policy: value
            .interaction_policy
            .map(crate::commands_vegetation_runtime::interaction_from_dto),
        ignited: value.ignited,
        promotion_origin: value
            .promotion_origin
            .as_ref()
            .map(promotion_from_dto)
            .transpose()?,
    })
}

/// Decodes one wire mutation record into the reducer's exact vocabulary.
pub(crate) fn record_from_dto(
    value: &VegetationMutationRecordDto,
) -> Result<VegetationMutationRecord> {
    Ok(VegetationMutationRecord {
        header: header(&value.header)?,
        mutation: mutation(&value.mutation)?,
    })
}

/// Encodes one reducer record for the wire.
pub(crate) fn record_to_dto(value: &VegetationMutationRecord) -> VegetationMutationRecordDto {
    VegetationMutationRecordDto {
        header: VegetationMutationHeaderDto {
            cell: world_cell_dto(value.header.cell),
            transaction: guid_dto(value.header.transaction),
            authority: guid_dto(value.header.authority),
            logical_tick: value.header.logical_tick.to_string(),
            idempotency_key: guid_dto(value.header.idempotency_key),
            base_revision: value.header.base_revision.map(|value| value.to_string()),
        },
        mutation: mutation_to_dto(&value.mutation),
    }
}

fn guid_dto(value: u128) -> VegetationGuid {
    VegetationGuid(format!("{value:032x}"))
}

fn transform_dto(
    position: WorldPosition,
    orientation: QuantizedOrientation,
    scale: [DecisionScalar; 3],
) -> PlantTransformDto {
    PlantTransformDto {
        global_ticks: position.global_ticks().map(|tick| tick.to_string()),
        orientation: orientation.bits(),
        scale_bits: scale.map(DecisionScalar::bits),
    }
}

pub(crate) fn promotion_dto(state: PromotionOriginState) -> PlantPromotionOriginDto {
    PlantPromotionOriginDto {
        transform: transform_dto(state.position, state.orientation, state.scale),
        linear_velocity_bits: state.linear_velocity.map(DecisionScalar::bits),
        angular_velocity_bits: state.angular_velocity.map(DecisionScalar::bits),
    }
}

fn delta_dto(delta: &PlantPersistentState) -> PlantDeltaDto {
    PlantDeltaDto {
        addition: delta
            .addition
            .as_ref()
            .map(crate::commands_asset::plant_point_dto),
        tombstoned: delta.tombstoned,
        transform: delta
            .transform
            .map(|(position, orientation, scale)| transform_dto(position, orientation, scale)),
        lifecycle: delta
            .lifecycle
            .map(crate::commands_vegetation_runtime::lifecycle_dto),
        phenotype: delta.phenotype,
        ecology_tick: delta.ecology_tick.map(|tick| tick.to_string()),
        health: delta.health.map(UnitInterval::bits),
        moisture: delta.moisture.map(UnitInterval::bits),
        fuel: delta.fuel.map(UnitInterval::bits),
        interaction_policy: delta
            .interaction_policy
            .map(crate::commands_vegetation_runtime::interaction_dto),
        ignited: delta.ignited,
        promotion_origin: delta.promotion_origin.map(promotion_dto),
    }
}

fn mutation_to_dto(value: &VegetationMutation) -> VegetationMutationDto {
    match value {
        VegetationMutation::FieldTilePatch {
            layer,
            channel,
            tile,
            dimensions,
            quantum_bits,
            values,
        } => VegetationMutationDto::FieldTilePatch {
            layer: guid_dto(*layer),
            channel: crate::vegetation_cook_dto::field_channel_dto(*channel),
            tile: guid_dto(*tile),
            dimensions: *dimensions,
            quantum_bits: *quantum_bits,
            values: values.clone(),
        },
        VegetationMutation::AnchorAddition(point) => VegetationMutationDto::AnchorAddition {
            point: crate::commands_asset::plant_point_dto(point),
        },
        VegetationMutation::Planting(point) => VegetationMutationDto::Planting {
            point: crate::commands_asset::plant_point_dto(point),
        },
        VegetationMutation::Tombstone { plant } => VegetationMutationDto::Tombstone {
            plant: plant_id_dto(*plant),
        },
        VegetationMutation::TransformOverride {
            plant,
            position,
            orientation,
            scale,
        } => VegetationMutationDto::TransformOverride {
            plant: plant_id_dto(*plant),
            transform: transform_dto(*position, *orientation, *scale),
        },
        VegetationMutation::StateOverride {
            plant,
            lifecycle,
            phenotype,
            health,
            moisture,
            fuel,
            interaction_policy,
        } => VegetationMutationDto::StateOverride {
            plant: plant_id_dto(*plant),
            lifecycle: lifecycle.map(crate::commands_vegetation_runtime::lifecycle_dto),
            phenotype: *phenotype,
            health: health.map(UnitInterval::bits),
            moisture: moisture.map(UnitInterval::bits),
            fuel: fuel.map(UnitInterval::bits),
            interaction_policy: interaction_policy
                .map(crate::commands_vegetation_runtime::interaction_dto),
        },
        VegetationMutation::Damage {
            plant,
            amount,
            phenotype,
        } => VegetationMutationDto::Damage {
            plant: plant_id_dto(*plant),
            amount: amount.bits(),
            phenotype: *phenotype,
        },
        VegetationMutation::MoistureFuel {
            plant,
            moisture,
            fuel,
        } => VegetationMutationDto::MoistureFuel {
            plant: plant_id_dto(*plant),
            moisture: moisture.bits(),
            fuel: fuel.bits(),
        },
        VegetationMutation::LifecycleTransition {
            plant,
            from,
            to,
            ecology_tick,
        } => VegetationMutationDto::LifecycleTransition {
            plant: plant_id_dto(*plant),
            from: from.map(crate::commands_vegetation_runtime::lifecycle_dto),
            to: crate::commands_vegetation_runtime::lifecycle_dto(*to),
            ecology_tick: ecology_tick.to_string(),
        },
        VegetationMutation::Harvest { plant, phenotype } => VegetationMutationDto::Harvest {
            plant: plant_id_dto(*plant),
            phenotype: *phenotype,
        },
        VegetationMutation::Burn {
            plant,
            phenotype,
            remaining_fuel,
        } => VegetationMutationDto::Burn {
            plant: plant_id_dto(*plant),
            phenotype: *phenotype,
            remaining_fuel: remaining_fuel.bits(),
        },
        VegetationMutation::Ignite { plant } => VegetationMutationDto::Ignite {
            plant: plant_id_dto(*plant),
        },
        VegetationMutation::Extinguish { plant } => VegetationMutationDto::Extinguish {
            plant: plant_id_dto(*plant),
        },
        VegetationMutation::Regrow {
            plant,
            lifecycle,
            phenotype,
            ecology_tick,
        } => VegetationMutationDto::Regrow {
            plant: plant_id_dto(*plant),
            lifecycle: crate::commands_vegetation_runtime::lifecycle_dto(*lifecycle),
            phenotype: *phenotype,
            ecology_tick: ecology_tick.to_string(),
        },
        VegetationMutation::PromotionOriginState { plant, state } => {
            VegetationMutationDto::PromotionOriginState {
                plant: plant_id_dto(*plant),
                origin: promotion_dto(*state),
            }
        }
        VegetationMutation::DisturbanceMask {
            categories,
            tile,
            values,
        } => VegetationMutationDto::DisturbanceMask {
            categories: *categories,
            tile: guid_dto(*tile),
            values: values.clone(),
        },
        VegetationMutation::PlantDeltaRestore { plant, delta } => {
            VegetationMutationDto::PlantDeltaRestore {
                plant: plant_id_dto(*plant),
                delta: delta.as_deref().map(delta_dto).map(Box::new),
            }
        }
        VegetationMutation::FieldTileClear {
            layer,
            channel,
            tile,
        } => VegetationMutationDto::FieldTileClear {
            layer: guid_dto(*layer),
            channel: crate::vegetation_cook_dto::field_channel_dto(*channel),
            tile: guid_dto(*tile),
        },
        VegetationMutation::DisturbanceMaskClear { categories, tile } => {
            VegetationMutationDto::DisturbanceMaskClear {
                categories: *categories,
                tile: guid_dto(*tile),
            }
        }
    }
}

fn plant_id_dto(value: PlantId) -> saffron_protocol::PlantId {
    saffron_protocol::PlantId(value.to_string())
}
