//! Scalar-field operators: sampling, noise, curves, remap, combine, and distance.

use super::*;

use std::collections::BTreeMap;

use saffron_spatial::{
    DecisionCurve, DecisionHessian3, DecisionScalar, DecisionVec3, FieldAvailability, FieldChannel,
    FieldDerivative, LOCAL_TICKS_PER_METER, RandomDomain, RandomStream, SurfaceField, UnitInterval,
    WorldCellKey, WorldPosition, div_round_ties_even,
};

use crate::{
    CompiledGraphNode, Error, FieldBlendOperator, GraphAuthority, GraphCombineOperation,
    GraphDistanceSource, Result,
};

pub(super) fn sample_field(
    node: &CompiledGraphNode,
    candidates: &CandidateStream,
    require_authoritative: bool,
    state: &mut EvaluationState<'_>,
) -> Result<GraphValue> {
    let channel = field_parameter(node, "channel")?;
    let derivative = field_derivative_parameter(node, "derivative", FieldDerivative::Value)?;
    let authoritative = node.definition.authority != GraphAuthority::Cosmetic;
    let mut sampling = FieldSamplingContext {
        node,
        candidates,
        authoritative,
        require_authoritative,
        channel,
        derivative,
        state,
    };
    match derivative {
        FieldDerivative::Value => {
            let values = sampling.sample(
                EvaluationFieldTile::sample_scalar,
                |provider, position| {
                    provider
                        .sample_scalar(channel, derivative, position)
                        .map(|sample| sample.value)
                },
                |value| match value {
                    QuantizedSurfaceFieldValue::Scalar(value) => {
                        Some(DecisionScalar::from_bits(value))
                    }
                    _ => None,
                },
                |value| QuantizedSurfaceFieldValue::Scalar(value.bits()),
                DecisionScalar::from_bits(0),
            )?;
            Ok(GraphValue::Scalar(ScalarFieldSamples {
                lineage: candidates.lineage,
                values,
            }))
        }
        FieldDerivative::Gradient => {
            let values = sampling.sample(
                EvaluationFieldTile::sample_vector,
                |provider, position| {
                    provider
                        .sample_vector(channel, derivative, position)
                        .map(|sample| sample.value)
                },
                |value| match value {
                    QuantizedSurfaceFieldValue::Gradient(value) => Some(DecisionVec3 {
                        x: DecisionScalar::from_bits(value[0]),
                        y: DecisionScalar::from_bits(value[1]),
                        z: DecisionScalar::from_bits(value[2]),
                    }),
                    _ => None,
                },
                |value| {
                    QuantizedSurfaceFieldValue::Gradient([
                        value.x.bits(),
                        value.y.bits(),
                        value.z.bits(),
                    ])
                },
                DecisionVec3::default(),
            )?;
            Ok(GraphValue::Vector(VectorFieldSamples {
                lineage: candidates.lineage,
                values,
            }))
        }
        FieldDerivative::Hessian => {
            let values = sampling.sample(
                EvaluationFieldTile::sample_hessian,
                |provider, position| {
                    provider
                        .sample_hessian(channel, position)
                        .map(|sample| sample.value)
                },
                |value| match value {
                    QuantizedSurfaceFieldValue::Hessian(value) => Some(DecisionHessian3 {
                        xx: DecisionScalar::from_bits(value[0]),
                        xy: DecisionScalar::from_bits(value[1]),
                        xz: DecisionScalar::from_bits(value[2]),
                        yy: DecisionScalar::from_bits(value[3]),
                        yz: DecisionScalar::from_bits(value[4]),
                        zz: DecisionScalar::from_bits(value[5]),
                    }),
                    _ => None,
                },
                |value| {
                    QuantizedSurfaceFieldValue::Hessian([
                        value.xx.bits(),
                        value.xy.bits(),
                        value.xz.bits(),
                        value.yy.bits(),
                        value.yz.bits(),
                        value.zz.bits(),
                    ])
                },
                DecisionHessian3::default(),
            )?;
            Ok(GraphValue::Hessian(HessianFieldSamples {
                lineage: candidates.lineage,
                values,
            }))
        }
    }
}

struct FieldSamplingContext<'a, 'b> {
    pub(super) node: &'a CompiledGraphNode,
    pub(super) candidates: &'a CandidateStream,
    pub(super) authoritative: bool,
    require_authoritative: bool,
    pub(super) channel: FieldChannel,
    pub(super) derivative: FieldDerivative,
    pub(super) state: &'a mut EvaluationState<'b>,
}

impl FieldSamplingContext<'_, '_> {
    pub(super) fn sample<T: Copy + PartialEq>(
        &mut self,
        sample_tile: impl Fn(&EvaluationFieldTile, WorldPosition) -> Option<T>,
        sample_provider: impl Fn(&dyn SurfaceField, WorldPosition) -> saffron_spatial::Result<T>,
        decode_query: impl Fn(QuantizedSurfaceFieldValue) -> Option<T>,
        encode_query: impl Fn(T) -> QuantizedSurfaceFieldValue,
        cosmetic_default: T,
    ) -> Result<BTreeMap<CandidateIdentity, T>> {
        let mut values = BTreeMap::new();
        for candidate in &self.candidates.candidates {
            let sample = if self.authoritative {
                let exact = self
                    .state
                    .inputs
                    .surface_field_query_tiles
                    .iter()
                    .filter(|tile| {
                        tile.node == self.node.definition.guid
                            && tile.node_semantic_revision == self.node.definition.semantic_revision
                            && tile.channel == self.channel
                            && tile.derivative == self.derivative
                    })
                    .find_map(|tile| tile.sample(candidate.identity, candidate.position))
                    .and_then(&decode_query);
                let tiled = self
                    .state
                    .inputs
                    .fields
                    .iter()
                    .filter(|tile| {
                        matches!(tile.source, EvaluationFieldSource::SurfaceProvider { .. })
                            && tile.channel == self.channel
                            && tile.derivative == self.derivative
                    })
                    .find_map(|tile| sample_tile(tile, candidate.position));
                if exact.is_some() && tiled.is_some() && exact != tiled {
                    return Err(Error::GraphDocument {
                        path: self.node.debug_symbol.label.clone(),
                        reason: "exact and lattice field inputs disagree".to_owned(),
                    });
                }
                if let Some(value) = exact {
                    self.retain_query(candidate, encode_query(value))?;
                    Some(value)
                } else if let Some(value) = tiled {
                    Some(value)
                } else if self.state.pass == EvaluationPass::PrepareCanonicalInputs {
                    let mut prepared = None;
                    for provider in &self.state.inputs.surface_providers {
                        let descriptor = provider.descriptor();
                        if !descriptor.capabilities.authoritative_fields
                            || provider.availability(
                                self.channel,
                                self.derivative,
                                self.state.inputs.read_bounds,
                            ) == FieldAvailability::Unavailable
                        {
                            continue;
                        }
                        prepared = Some(sample_provider(provider.as_ref(), candidate.position)?);
                        break;
                    }
                    if let Some(value) = prepared {
                        self.retain_query(candidate, encode_query(value))?;
                        Some(value)
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                self.state
                    .inputs
                    .surface_providers
                    .iter()
                    .find_map(|provider| {
                        if provider.availability(
                            self.channel,
                            self.derivative,
                            self.state.inputs.read_bounds,
                        ) == FieldAvailability::Unavailable
                        {
                            return None;
                        }
                        sample_provider(provider.as_ref(), candidate.position).ok()
                    })
            };
            if let Some(value) = sample {
                if self.authoritative && self.state.pass == EvaluationPass::PrepareCanonicalInputs {
                    self.retain_query(candidate, encode_query(value))?;
                }
                values.insert(candidate.identity, value);
            } else if self.authoritative || self.require_authoritative {
                return Err(Error::GraphAuthoritativeInput {
                    node: self.node.definition.guid,
                    input: format!(
                        "{} {:?} tile",
                        field_channel_name(self.channel),
                        self.derivative
                    ),
                });
            } else {
                values.insert(candidate.identity, cosmetic_default);
            }
        }
        Ok(values)
    }

    fn retain_query(
        &mut self,
        candidate: &GraphCandidate,
        encoded: QuantizedSurfaceFieldValue,
    ) -> Result<()> {
        let previous = self
            .state
            .prepared_surface_fields
            .entry((
                self.node.definition.guid,
                self.node.definition.semantic_revision,
                self.channel,
                self.derivative,
                self.state.inputs.surface_provider_set_hash,
            ))
            .or_default()
            .insert((candidate.identity, candidate.position), encoded);
        if previous.is_some_and(|previous| previous != encoded) {
            return Err(Error::GraphDocument {
                path: self.node.debug_symbol.label.clone(),
                reason: "canonical field replay produced conflicting values".to_owned(),
            });
        }
        Ok(())
    }
}

pub(super) fn sample_painted_tile(
    node: &CompiledGraphNode,
    candidates: &CandidateStream,
    require_authoritative: bool,
    state: &mut EvaluationState<'_>,
) -> Result<ScalarFieldSamples> {
    let authoritative = node.definition.authority != GraphAuthority::Cosmetic;
    let channel = field_parameter(node, "channel")?;
    let layer = guid_parameter(node, "layer", 0)?;
    let mut values = BTreeMap::new();
    for candidate in &candidates.candidates {
        if let Some(value) = sample_ordered_scalar_tiles(
            state.inputs.fields.iter().filter(|tile| {
                tile.source == EvaluationFieldSource::MapLayer(layer)
                    && tile.channel == channel
                    && tile.derivative == FieldDerivative::Value
            }),
            candidate.position,
        )? {
            values.insert(candidate.identity, value);
        } else if authoritative || require_authoritative {
            return Err(Error::GraphAuthoritativeInput {
                node: node.definition.guid,
                input: format!(
                    "painted {} tile for layer {layer:032x}",
                    field_channel_name(channel)
                ),
            });
        } else {
            values.insert(candidate.identity, DecisionScalar::from_bits(0));
        }
    }
    Ok(ScalarFieldSamples {
        lineage: candidates.lineage,
        values,
    })
}

fn sample_ordered_scalar_tiles<'a>(
    tiles: impl IntoIterator<Item = &'a EvaluationFieldTile>,
    position: WorldPosition,
) -> Result<Option<DecisionScalar>> {
    let mut result = None;
    for tile in tiles {
        let Some(value) = tile.sample_scalar(position) else {
            continue;
        };
        let weighted = scale_field_value(value, tile.weight)?;
        result = Some(match (result, tile.blend) {
            (None, FieldBlendOperator::Multiply) => {
                lerp_field_value(DecisionScalar::from_bits(65_536), value, tile.weight)?
            }
            (None, _) => weighted,
            (Some(current), FieldBlendOperator::Replace) => {
                lerp_field_value(current, value, tile.weight)?
            }
            (Some(current), FieldBlendOperator::Add) => current.checked_add(weighted)?,
            (Some(current), FieldBlendOperator::Multiply) => current.checked_mul(
                lerp_field_value(DecisionScalar::from_bits(65_536), value, tile.weight)?,
            )?,
            (Some(current), FieldBlendOperator::Minimum) => current.min(weighted),
            (Some(current), FieldBlendOperator::Maximum) => current.max(weighted),
        });
    }
    Ok(result)
}

fn scale_field_value(value: DecisionScalar, weight: UnitInterval) -> Result<DecisionScalar> {
    let bits = div_round_ties_even(
        i128::from(value.bits()) * i128::from(weight.bits()),
        i128::from(u16::MAX),
    )?;
    Ok(DecisionScalar::from_bits(
        i32::try_from(bits).map_err(|_| Error::NumericOverflow)?,
    ))
}

fn lerp_field_value(
    start: DecisionScalar,
    end: DecisionScalar,
    weight: UnitInterval,
) -> Result<DecisionScalar> {
    let delta = i128::from(end.bits()) - i128::from(start.bits());
    let bits = i128::from(start.bits())
        + div_round_ties_even(delta * i128::from(weight.bits()), i128::from(u16::MAX))?;
    Ok(DecisionScalar::from_bits(
        i32::try_from(bits).map_err(|_| Error::NumericOverflow)?,
    ))
}

pub(super) fn noise_field(
    node: &CompiledGraphNode,
    candidates: &CandidateStream,
    state: &EvaluationState<'_>,
) -> Result<ScalarFieldSamples> {
    let frequency = fixed_parameter(node, "frequency", DecisionScalar::from_bits(65_536))?;
    let amplitude = fixed_parameter(node, "amplitude", DecisionScalar::from_bits(65_536))?;
    let channel = u32_parameter(node, "channel", 0)?;
    if frequency.bits() <= 0 {
        return Err(Error::GraphDocument {
            path: node.debug_symbol.label.clone(),
            reason: "noise frequency must be positive".to_owned(),
        });
    }
    let mut values = BTreeMap::new();
    for candidate in &candidates.candidates {
        values.insert(
            candidate.identity,
            coherent_value_noise(node, state, candidate.position, frequency, channel)?
                .checked_mul(amplitude)?,
        );
    }
    Ok(ScalarFieldSamples {
        lineage: candidates.lineage,
        values,
    })
}

fn coherent_value_noise(
    node: &CompiledGraphNode,
    state: &EvaluationState<'_>,
    position: WorldPosition,
    frequency: DecisionScalar,
    channel: u32,
) -> Result<DecisionScalar> {
    let (corners, blend) =
        coherent_value_noise_components(node, state, position, frequency, channel)?;
    let x00 = corners[0].lerp(corners[1], blend[0])?;
    let x10 = corners[2].lerp(corners[3], blend[0])?;
    let x01 = corners[4].lerp(corners[5], blend[0])?;
    let x11 = corners[6].lerp(corners[7], blend[0])?;
    let y0 = x00.lerp(x10, blend[1])?;
    let y1 = x01.lerp(x11, blend[1])?;
    y0.lerp(y1, blend[2]).map_err(Into::into)
}

pub(super) fn coherent_value_noise_components(
    node: &CompiledGraphNode,
    state: &EvaluationState<'_>,
    position: WorldPosition,
    frequency: DecisionScalar,
    channel: u32,
) -> Result<([DecisionScalar; 8], [UnitInterval; 3])> {
    let ticks = position.global_ticks();
    let mut lattice = [0_i128; 3];
    let mut blend = [UnitInterval::ZERO; 3];
    for axis in 0..3 {
        let scaled = ticks[axis]
            .checked_mul(i128::from(frequency.bits()))
            .ok_or(Error::NumericOverflow)?;
        let coordinate = div_round_ties_even(scaled, i128::from(LOCAL_TICKS_PER_METER))?;
        lattice[axis] = coordinate.div_euclid(65_536);
        let fraction =
            u16::try_from(coordinate.rem_euclid(65_536)).map_err(|_| Error::NumericOverflow)?;
        blend[axis] = smooth_unit(UnitInterval::from_bits(fraction))?;
    }
    let mut corners = [DecisionScalar::from_bits(0); 8];
    for (index, value) in corners.iter_mut().enumerate() {
        let coordinate = [
            lattice[0] + i128::from((index & 1) as u8),
            lattice[1] + i128::from(((index >> 1) & 1) as u8),
            lattice[2] + i128::from(((index >> 2) & 1) as u8),
        ];
        let address = node_execution_address(node, state).to_be_bytes();
        let x = coordinate[0].to_be_bytes();
        let y = coordinate[1].to_be_bytes();
        let z = coordinate[2].to_be_bytes();
        let channel_bytes = channel.to_be_bytes();
        let ordinal = stable_ordinal(&[&address, &x, &y, &z, &channel_bytes])?;
        let stream = RandomStream::new(RandomDomain {
            map: u128::from(state.inputs.map.value()),
            node_guid: node_execution_address(node, state),
            node_semantic_revision: node.definition.semantic_revision,
            seed_namespace: seed_namespace(node, "noise")?,
            cell: WorldCellKey::base(0, 0, 0),
            candidate: ordinal,
            ancestor: 0,
            species: 0,
            channel,
        });
        let unit = i32::from(stream.unit(0, 0).bits());
        *value = DecisionScalar::from_bits(
            unit.checked_mul(2)
                .and_then(|value| value.checked_sub(i32::from(u16::MAX)))
                .ok_or(Error::NumericOverflow)?,
        );
    }
    Ok((corners, blend))
}

fn smooth_unit(value: UnitInterval) -> Result<UnitInterval> {
    let fixed = DecisionScalar::from_bits(i32::from(value.bits()));
    let squared = fixed.checked_mul(fixed)?;
    let factor = DecisionScalar::from_bits(3 * 65_536)
        .checked_sub(DecisionScalar::from_bits(2 * 65_536).checked_mul(fixed)?)?;
    let bits = squared
        .checked_mul(factor)?
        .bits()
        .clamp(0, i32::from(u16::MAX));
    Ok(UnitInterval::from_bits(
        u16::try_from(bits).map_err(|_| Error::NumericOverflow)?,
    ))
}

pub(super) fn gradient_field(
    node: &CompiledGraphNode,
    candidates: &CandidateStream,
) -> Result<ScalarFieldSamples> {
    let direction = fixed_vec3_parameter(node, "direction", [DecisionScalar::from_bits(0); 3])?;
    let exact_origin = world_position_parameter(node, "exactOrigin")?;
    let scale = fixed_parameter(node, "scale", DecisionScalar::from_bits(0))?;
    let bias = fixed_parameter(node, "bias", DecisionScalar::from_bits(0))?;
    let mut values = BTreeMap::new();
    for candidate in &candidates.candidates {
        values.insert(
            candidate.identity,
            DecisionScalar::from_bits(crate::evaluate_gradient_ramp(
                candidate.position.global_ticks(),
                exact_origin,
                direction.map(DecisionScalar::bits),
                scale.bits(),
                bias.bits(),
            )?),
        );
    }
    Ok(ScalarFieldSamples {
        lineage: candidates.lineage,
        values,
    })
}

pub(super) fn curve_field(
    node: &CompiledGraphNode,
    input: &ScalarFieldSamples,
) -> Result<ScalarFieldSamples> {
    let curve = curve_parameter(node, "curve")?;
    let values = input
        .values
        .iter()
        .map(|(identity, value)| {
            let bits = value.bits().clamp(0, i32::from(u16::MAX)) as u16;
            Ok((
                *identity,
                DecisionCurve::sample_points(curve, UnitInterval::from_bits(bits))?,
            ))
        })
        .collect::<Result<_>>()?;
    Ok(ScalarFieldSamples {
        lineage: input.lineage,
        values,
    })
}

pub(super) fn remap_field(
    node: &CompiledGraphNode,
    input: &ScalarFieldSamples,
) -> Result<ScalarFieldSamples> {
    let input_min = fixed_parameter(node, "inputMin", DecisionScalar::from_bits(0))?;
    let input_max = fixed_parameter(node, "inputMax", DecisionScalar::from_bits(0))?;
    let output_min = fixed_parameter(node, "outputMin", DecisionScalar::from_bits(0))?;
    let output_max = fixed_parameter(node, "outputMax", DecisionScalar::from_bits(0))?;
    if input_min >= input_max {
        return Err(Error::GraphDocument {
            path: node.debug_symbol.label.clone(),
            reason: "remap input range is empty".to_owned(),
        });
    }
    let input_span = input_max.checked_sub(input_min)?;
    let output_span = output_max.checked_sub(output_min)?;
    let values = input
        .values
        .iter()
        .map(|(identity, value)| {
            let clamped = (*value).clamp(input_min, input_max);
            let ratio = clamped.checked_sub(input_min)?.checked_div(input_span)?;
            Ok((
                *identity,
                output_min.checked_add(output_span.checked_mul(ratio)?)?,
            ))
        })
        .collect::<Result<_>>()?;
    Ok(ScalarFieldSamples {
        lineage: input.lineage,
        values,
    })
}

pub(super) fn combine_fields(
    node: &CompiledGraphNode,
    left: &ScalarFieldSamples,
    right: &ScalarFieldSamples,
) -> Result<ScalarFieldSamples> {
    ensure_lineage(node, "right", left.lineage, right.lineage)?;
    let operation = combine_operation_parameter(node, "operation")?;
    let mut values = BTreeMap::new();
    for (identity, left) in &left.values {
        let Some(right) = right.values.get(identity) else {
            continue;
        };
        let value = match operation {
            GraphCombineOperation::Add => left.checked_add(*right)?,
            GraphCombineOperation::Multiply => left.checked_mul(*right)?,
            GraphCombineOperation::Minimum => (*left).min(*right),
            GraphCombineOperation::Maximum => (*left).max(*right),
        };
        values.insert(*identity, value);
    }
    Ok(ScalarFieldSamples {
        lineage: left.lineage,
        values,
    })
}

pub(super) fn clamp_field(
    node: &CompiledGraphNode,
    input: &ScalarFieldSamples,
) -> Result<ScalarFieldSamples> {
    let minimum = fixed_parameter(node, "minimum", DecisionScalar::from_bits(0))?;
    let maximum = fixed_parameter(node, "maximum", DecisionScalar::from_bits(0))?;
    if minimum > maximum {
        return Err(Error::GraphDocument {
            path: node.debug_symbol.label.clone(),
            reason: "clamp minimum exceeds maximum".to_owned(),
        });
    }
    Ok(ScalarFieldSamples {
        lineage: input.lineage,
        values: input
            .values
            .iter()
            .map(|(identity, value)| (*identity, (*value).clamp(minimum, maximum)))
            .collect(),
    })
}

pub(super) fn distance_field(
    node: &CompiledGraphNode,
    candidates: &CandidateStream,
    state: &EvaluationState<'_>,
) -> Result<ScalarFieldSamples> {
    let source = distance_source_parameter(node, "source")?;
    let source_guid = guid_parameter(node, "sourceGuid", 0)?;
    let maximum_distance_value =
        fixed_parameter(node, "maximumDistance", DecisionScalar::from_bits(0))?;
    if maximum_distance_value.bits() < 0 {
        return Err(Error::GraphDocument {
            path: node.debug_symbol.label.clone(),
            reason: "maximum distance must be nonnegative".to_owned(),
        });
    }
    let maximum_distance = fixed_meters_to_ticks(maximum_distance_value)?;
    let mut values = BTreeMap::new();
    for candidate in &candidates.candidates {
        let value = match source {
            GraphDistanceSource::Spline => {
                let distance_ticks = state
                    .inputs
                    .splines
                    .iter()
                    .filter(|spline| {
                        source_guid == 0 || spline.id == source_guid || spline.layer == source_guid
                    })
                    .flat_map(|spline| spline.points.windows(2))
                    .try_fold(None, |minimum, segment| {
                        let distance = point_segment_distance_ticks(
                            candidate.position,
                            segment[0],
                            segment[1],
                        )?;
                        Ok::<_, Error>(Some(
                            minimum.map_or(distance, |value: i128| value.min(distance)),
                        ))
                    })?
                    .unwrap_or(i128::MAX);
                DecisionScalar::from_bits(ticks_to_fixed_meters(
                    distance_ticks.min(maximum_distance),
                )?)
            }
            GraphDistanceSource::Shape => {
                let distance_ticks = state
                    .inputs
                    .regions
                    .iter()
                    .filter(|region| {
                        region.kind == EvaluationRegionKind::Shape
                            && (source_guid == 0
                                || region.id == source_guid
                                || region.layer == source_guid)
                    })
                    .try_fold(None, |minimum, region| {
                        let distance =
                            point_bounds_distance_ticks(candidate.position, region.bounds)?;
                        Ok::<_, Error>(Some(
                            minimum.map_or(distance, |value: i128| value.min(distance)),
                        ))
                    })?
                    .unwrap_or(i128::MAX);
                DecisionScalar::from_bits(ticks_to_fixed_meters(
                    distance_ticks.min(maximum_distance),
                )?)
            }
            GraphDistanceSource::Water | GraphDistanceSource::Blocker => {
                let channel = if source == GraphDistanceSource::Water {
                    FieldChannel::WaterDistance
                } else {
                    FieldChannel::SignedBlocker
                };
                let tiles = state.inputs.fields.iter().filter(|tile| {
                    tile.channel == channel
                        && tile.derivative == FieldDerivative::Value
                        && match tile.source {
                            EvaluationFieldSource::MapLayer(layer) => {
                                source_guid == 0 || layer == source_guid
                            }
                            EvaluationFieldSource::SurfaceProvider { .. } => source_guid == 0,
                        }
                });
                let sampled =
                    sample_ordered_scalar_tiles(tiles, candidate.position)?.ok_or_else(|| {
                        Error::GraphAuthoritativeInput {
                            node: node.definition.guid,
                            input: format!("{} distance field", field_channel_name(channel)),
                        }
                    })?;
                if source == GraphDistanceSource::Blocker {
                    sampled.clamp(
                        DecisionScalar::from_bits(
                            maximum_distance_value
                                .bits()
                                .checked_neg()
                                .ok_or(Error::NumericOverflow)?,
                        ),
                        maximum_distance_value,
                    )
                } else {
                    sampled.clamp(DecisionScalar::from_bits(0), maximum_distance_value)
                }
            }
        };
        values.insert(candidate.identity, value);
    }
    Ok(ScalarFieldSamples {
        lineage: candidates.lineage,
        values,
    })
}
