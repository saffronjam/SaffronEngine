//! Wire → domain conversion for authored vegetation-map layers: the
//! `vegetation-map-layer-commit` command's rows decode here into the layer algebra
//! (the read direction lives beside `vegetation-asset-summary` in `commands_asset`).

use std::str::FromStr;

use saffron_core::Uuid;
use saffron_protocol::{
    FieldBlendOperatorDto, FieldChannelDto, FieldChannelKindDto, InclusionOperatorDto,
    LayerCoordinateSpaceDto, PlantStateOverrideDto, PlantTransformOverrideDto, SpeciesWeightDto,
    VegetationGuid, VegetationLayerDto, VegetationLayerOperatorDto,
};
use saffron_spatial::{
    DecisionScalar, DecisionVec3, FieldChannel, UnitInterval, WorldBounds, WorldPosition,
};
use saffron_vegetation::{
    FieldBlendOperator, FieldTileLayer, InclusionOperator, LayerCoordinateSpace, PlantId,
    PlantStateOverride, PlantTransformOverride, SpeciesWeight, SplineLayer, VegetationLayer,
    VegetationLayerOperator, VolumeLayer,
};

use crate::error::{Error, Result};

fn parse_guid(value: &VegetationGuid) -> Result<u128> {
    u128::from_str_radix(&value.0, 16)
        .map_err(|_| Error::command("vegetation GUID is not canonical"))
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

fn parse_plant(value: &saffron_protocol::PlantId) -> Result<PlantId> {
    PlantId::from_str(&value.0).map_err(Error::from)
}

fn bounds(value: &saffron_protocol::WorldBoundsDto) -> Result<WorldBounds> {
    let mut minimum = [0_i128; 3];
    let mut maximum = [0_i128; 3];
    for (axis, (low, high)) in minimum.iter_mut().zip(&mut maximum).enumerate() {
        *low = parse_i128(&value.min_ticks[axis], "bounds.minTicks")?;
        *high = parse_i128(&value.max_ticks_exclusive[axis], "bounds.maxTicksExclusive")?;
    }
    WorldBounds::new(minimum, maximum).map_err(|error| Error::command(error.to_string()))
}

fn position(ticks: &[String; 3]) -> Result<WorldPosition> {
    let mut global = [0_i128; 3];
    for (slot, value) in global.iter_mut().zip(ticks) {
        *slot = parse_i128(value, "globalTicks")?;
    }
    WorldPosition::from_global_ticks(global).map_err(|error| Error::command(error.to_string()))
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

fn blend(value: FieldBlendOperatorDto) -> FieldBlendOperator {
    match value {
        FieldBlendOperatorDto::Replace => FieldBlendOperator::Replace,
        FieldBlendOperatorDto::Add => FieldBlendOperator::Add,
        FieldBlendOperatorDto::Multiply => FieldBlendOperator::Multiply,
        FieldBlendOperatorDto::Minimum => FieldBlendOperator::Minimum,
        FieldBlendOperatorDto::Maximum => FieldBlendOperator::Maximum,
    }
}

fn inclusion(value: InclusionOperatorDto) -> InclusionOperator {
    match value {
        InclusionOperatorDto::Include => InclusionOperator::Include,
        InclusionOperatorDto::Exclude => InclusionOperator::Exclude,
    }
}

fn space(value: LayerCoordinateSpaceDto) -> LayerCoordinateSpace {
    match value {
        LayerCoordinateSpaceDto::World => LayerCoordinateSpace::World,
        LayerCoordinateSpaceDto::Surface => LayerCoordinateSpace::Surface,
        LayerCoordinateSpaceDto::OwnerLocal => LayerCoordinateSpace::OwnerLocal,
    }
}

fn field_tile(
    channel_dto: &FieldChannelDto,
    tile_set: &VegetationGuid,
    blend_dto: FieldBlendOperatorDto,
    weight: u16,
) -> Result<FieldTileLayer> {
    Ok(FieldTileLayer {
        channel: channel(channel_dto)?,
        tile_set: parse_guid(tile_set)?,
        blend: blend(blend_dto),
        weight: UnitInterval::from_bits(weight),
    })
}

fn operator(value: &VegetationLayerOperatorDto) -> Result<VegetationLayerOperator> {
    Ok(match value {
        VegetationLayerOperatorDto::ScalarField {
            channel: channel_dto,
            tile_set,
            blend: blend_dto,
            weight,
        } => VegetationLayerOperator::ScalarField(field_tile(
            channel_dto,
            tile_set,
            *blend_dto,
            *weight,
        )?),
        VegetationLayerOperatorDto::VectorField {
            channel: channel_dto,
            tile_set,
            value_bits,
            blend: blend_dto,
        } => VegetationLayerOperator::VectorField {
            channel: channel(channel_dto)?,
            tile_set: parse_guid(tile_set)?,
            value: DecisionVec3 {
                x: DecisionScalar::from_bits(value_bits[0]),
                y: DecisionScalar::from_bits(value_bits[1]),
                z: DecisionScalar::from_bits(value_bits[2]),
            },
            blend: blend(*blend_dto),
        },
        VegetationLayerOperatorDto::SpeciesWeights { weights } => {
            VegetationLayerOperator::SpeciesWeights(
                weights
                    .iter()
                    .map(|weight: &SpeciesWeightDto| SpeciesWeight {
                        family: Uuid(weight.family.value()),
                        weight: UnitInterval::from_bits(weight.weight),
                    })
                    .collect(),
            )
        }
        VegetationLayerOperatorDto::Density {
            channel: channel_dto,
            tile_set,
            blend: blend_dto,
            weight,
        } => VegetationLayerOperator::Density(field_tile(
            channel_dto,
            tile_set,
            *blend_dto,
            *weight,
        )?),
        VegetationLayerOperatorDto::Mask {
            tile_set,
            operation,
        } => VegetationLayerOperator::Mask {
            tile_set: parse_guid(tile_set)?,
            operation: inclusion(*operation),
        },
        VegetationLayerOperatorDto::Volume {
            bounds: bounds_dto,
            operation,
            falloff_bits,
        } => VegetationLayerOperator::Volume(VolumeLayer {
            bounds: bounds(bounds_dto)?,
            operation: inclusion(*operation),
            falloff: DecisionScalar::from_bits(*falloff_bits),
        }),
        VegetationLayerOperatorDto::Spline {
            spline,
            points,
            radius_bits,
            operation,
        } => VegetationLayerOperator::Spline(SplineLayer {
            spline: parse_guid(spline)?,
            points: points.iter().map(position).collect::<Result<Vec<_>>>()?,
            radius: DecisionScalar::from_bits(*radius_bits),
            operation: inclusion(*operation),
        }),
        VegetationLayerOperatorDto::Anchors { plants } => VegetationLayerOperator::Anchors(
            plants.iter().map(parse_plant).collect::<Result<Vec<_>>>()?,
        ),
        VegetationLayerOperatorDto::Pins { plants } => VegetationLayerOperator::Pins(
            plants.iter().map(parse_plant).collect::<Result<Vec<_>>>()?,
        ),
        VegetationLayerOperatorDto::TransformOverrides { overrides } => {
            VegetationLayerOperator::TransformOverrides(
                overrides
                    .iter()
                    .map(|row: &PlantTransformOverrideDto| {
                        Ok(PlantTransformOverride {
                            plant: parse_plant(&row.plant)?,
                            position: position(&row.global_ticks)?,
                            scale: row.scale_bits.map(DecisionScalar::from_bits),
                        })
                    })
                    .collect::<Result<Vec<_>>>()?,
            )
        }
        VegetationLayerOperatorDto::StateOverrides { overrides } => {
            VegetationLayerOperator::StateOverrides(
                overrides
                    .iter()
                    .map(|row: &PlantStateOverrideDto| {
                        Ok(PlantStateOverride {
                            plant: parse_plant(&row.plant)?,
                            health: row.health.map(UnitInterval::from_bits),
                            moisture: row.moisture.map(UnitInterval::from_bits),
                            fuel: row.fuel.map(UnitInterval::from_bits),
                            interaction_policy: row
                                .interaction_policy
                                .map(crate::commands_vegetation_runtime::interaction_from_dto),
                        })
                    })
                    .collect::<Result<Vec<_>>>()?,
            )
        }
        VegetationLayerOperatorDto::Blocker {
            tile_set,
            categories,
        } => VegetationLayerOperator::Blocker {
            tile_set: parse_guid(tile_set)?,
            categories: *categories,
        },
    })
}

fn field_tile_row(
    value: &saffron_protocol::AuthoredFieldTileDto,
) -> Result<saffron_vegetation::AuthoredFieldTile> {
    Ok(saffron_vegetation::AuthoredFieldTile {
        channel: channel(&value.channel)?,
        layer: parse_guid(&value.layer)?,
        dimensions: value.dimensions,
        quantum_bits: value.quantum_bits,
        values: value.values.clone(),
    })
}

/// Decodes one wire chunk payload into the authored map vocabulary. Anchor rows get
/// a synthesized minimal authored-provenance lineage (an `ExplicitAnchors` decision
/// per anchor — the user made the decision, no biome participated), because the
/// codec requires every anchor's provenance handle to resolve in its chunk table.
pub(crate) fn chunk_payload_from_dto(
    map: Uuid,
    layer: u128,
    value: &saffron_protocol::VegetationMapChunkPayloadDto,
) -> Result<saffron_vegetation::VegetationMapChunkPayload> {
    Ok(match value {
        saffron_protocol::VegetationMapChunkPayloadDto::Field { fields, blockers } => {
            saffron_vegetation::VegetationMapChunkPayload::Field(
                saffron_vegetation::VegetationMapFieldChunk {
                    fields: fields
                        .iter()
                        .map(field_tile_row)
                        .collect::<Result<Vec<_>>>()?,
                    blockers: blockers
                        .iter()
                        .map(field_tile_row)
                        .collect::<Result<Vec<_>>>()?,
                },
            )
        }
        saffron_protocol::VegetationMapChunkPayloadDto::AnchorOverride {
            explicit_plants,
            pins,
            transform_overrides,
            state_overrides,
        } => saffron_vegetation::VegetationMapChunkPayload::AnchorOverride({
            let mut provenance = saffron_vegetation::ProvenanceTable::default();
            let rows = explicit_plants
                .iter()
                .map(|anchor| {
                    let mut point = crate::vegetation_mutation_dto::point_from_dto(&anchor.point)?;
                    let decision =
                        provenance.intern_decision(saffron_vegetation::ProvenanceDecision {
                            parents: Vec::new(),
                            subgraph_path: Vec::new(),
                            node: layer,
                            operator: saffron_vegetation::GraphOperator::ExplicitAnchors,
                            candidate: point.candidate,
                            outcome: saffron_vegetation::ProvenanceDecisionOutcome::Accepted,
                        });
                    let record = provenance.intern(saffron_vegetation::ProvenanceRecord {
                        map,
                        layer,
                        biome: Uuid(0),
                        decision,
                        candidate: point.candidate,
                        family: Some(Uuid(anchor.family.value())),
                        plant: Some(parse_plant(&anchor.id)?),
                        variation: point.variation,
                    });
                    point.provenance = record.0;
                    Ok(saffron_vegetation::ExplicitPlantAnchor {
                        id: parse_plant(&anchor.id)?,
                        layer: parse_guid(&anchor.layer)?,
                        family: Uuid(anchor.family.value()),
                        point,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            saffron_vegetation::VegetationMapAnchorChunk {
                explicit_plants: rows,
                pins: pins.iter().map(parse_plant).collect::<Result<Vec<_>>>()?,
                transform_overrides: transform_overrides
                    .iter()
                    .map(|row| {
                        Ok(PlantTransformOverride {
                            plant: parse_plant(&row.plant)?,
                            position: position(&row.global_ticks)?,
                            scale: row.scale_bits.map(DecisionScalar::from_bits),
                        })
                    })
                    .collect::<Result<Vec<_>>>()?,
                state_overrides: state_overrides
                    .iter()
                    .map(|row| {
                        Ok(PlantStateOverride {
                            plant: parse_plant(&row.plant)?,
                            health: row.health.map(UnitInterval::from_bits),
                            moisture: row.moisture.map(UnitInterval::from_bits),
                            fuel: row.fuel.map(UnitInterval::from_bits),
                            interaction_policy: row
                                .interaction_policy
                                .map(crate::commands_vegetation_runtime::interaction_from_dto),
                        })
                    })
                    .collect::<Result<Vec<_>>>()?,
                provenance,
            }
        }),
    })
}

/// Decodes one wire chunk key.
pub(crate) fn chunk_key_from_dto(
    value: &saffron_protocol::VegetationMapChunkKeyDto,
) -> Result<saffron_vegetation::VegetationMapChunkKey> {
    Ok(saffron_vegetation::VegetationMapChunkKey {
        layer: parse_guid(&value.layer)?,
        tile: match &value.tile {
            saffron_protocol::VegetationMapTileKeyDto::Global => {
                saffron_vegetation::VegetationMapTileKey::Global
            }
            saffron_protocol::VegetationMapTileKeyDto::Cell { cell } => {
                saffron_vegetation::VegetationMapTileKey::Cell(cell_from_dto(cell)?)
            }
        },
        kind: match value.kind {
            saffron_protocol::VegetationMapChunkKindDto::Field => {
                saffron_vegetation::VegetationMapChunkKind::Field
            }
            saffron_protocol::VegetationMapChunkKindDto::AnchorOverride => {
                saffron_vegetation::VegetationMapChunkKind::AnchorOverride
            }
            saffron_protocol::VegetationMapChunkKindDto::GraphInstance => {
                saffron_vegetation::VegetationMapChunkKind::GraphInstance
            }
            saffron_protocol::VegetationMapChunkKindDto::LayerMetadata => {
                saffron_vegetation::VegetationMapChunkKind::LayerMetadata
            }
            saffron_protocol::VegetationMapChunkKindDto::EditorMetadata => {
                saffron_vegetation::VegetationMapChunkKind::EditorMetadata
            }
        },
    })
}

fn cell_from_dto(value: &saffron_protocol::WorldCellDto) -> Result<saffron_spatial::WorldCellKey> {
    let mut coordinates = [0_i64; 3];
    for (slot, coordinate) in coordinates.iter_mut().zip(&value.coordinates) {
        *slot = coordinate
            .parse::<i64>()
            .map_err(|_| Error::command("cell coordinate is not an i64"))?;
    }
    saffron_spatial::WorldCellKey::new(coordinates[0], coordinates[1], coordinates[2], value.level)
        .map_err(|error| Error::command(error.to_string()))
}

/// Decodes one wire layer row into the authored layer algebra.
pub(crate) fn layer_from_dto(value: &VegetationLayerDto) -> Result<VegetationLayer> {
    Ok(VegetationLayer {
        id: parse_guid(&value.id)?,
        name: value.name.clone(),
        coordinate_space: space(value.coordinate_space),
        bounds: bounds(&value.bounds)?,
        operator: operator(&value.operator)?,
        dependencies: value
            .dependencies
            .iter()
            .map(parse_guid)
            .collect::<Result<Vec<_>>>()?,
        order: value.order,
        locked: value.locked,
        muted: value.muted,
        revision: parse_u64(&value.revision, "layer.revision")?,
    })
}
