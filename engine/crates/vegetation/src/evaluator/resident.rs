//! Resident execution groups dispatched as one compute program.

use super::*;

use std::collections::{BTreeMap, BTreeSet};

use saffron_spatial::{DecisionCurve, DecisionScalar, UnitInterval};

use crate::{
    CompiledGraphUnit, Error, GRAPH_GPU_MAX_CURVE_POINTS, GRAPH_GPU_MAX_INSTRUCTIONS, GraphDomain,
    GraphExecutionGroup, GraphGpuInstruction, GraphGpuInvocationBatch, GraphGpuProgram,
    GraphGpuRegister, GraphGpuRegisterType, GraphGpuValue, GraphOperator, Result,
};

#[derive(Clone, Debug)]
pub(super) enum ResidentInputBinding {
    CandidateMask,
    ExternalScalar((u128, String)),
    NoiseCorner { node: u128, corner: usize },
    NoiseBlend { node: u128, axis: usize },
    GradientPosition { node: u128, axis: usize },
    GradientOrigin { node: u128, axis: usize },
}

pub(super) struct ResidentGroupEvaluation {
    pub(super) outputs: BTreeMap<(u128, String), GraphValue>,
    pub(super) base_candidate_count: u64,
    pub(super) final_candidate_count: u64,
    pub(super) field_importance_index: Option<usize>,
    pub(super) invocation_count: u64,
}

fn resident_source_register(
    unit: &CompiledGraphUnit,
    group: &GraphExecutionGroup,
    external_registers: &BTreeMap<(u128, String), GraphGpuRegister>,
    result_registers: &BTreeMap<(u128, String), GraphGpuRegister>,
    node: u128,
    pin: &str,
) -> Result<GraphGpuRegister> {
    let edge = unit
        .edges
        .iter()
        .find(|edge| edge.to_node == node && edge.to_pin == pin)
        .ok_or_else(|| Error::GraphDocument {
            path: format!("graphGpuGroup.{node:032x}.{pin}"),
            reason: "resident input edge is missing".to_owned(),
        })?;
    if resident_group_contains(group, edge.from_node) {
        result_registers
            .get(&(edge.from_node, edge.from_pin.clone()))
            .copied()
    } else {
        external_registers
            .get(&(edge.from_node, edge.from_pin.clone()))
            .copied()
    }
    .ok_or_else(|| Error::GraphDocument {
        path: format!("graphGpuGroup.{node:032x}.{pin}"),
        reason: "resident source register is missing".to_owned(),
    })
}

pub(super) fn resident_group_contains(group: &GraphExecutionGroup, node: u128) -> bool {
    group.nodes.iter().any(|member| member.address.node == node)
}

pub(super) fn evaluate_resident_group(
    unit: &CompiledGraphUnit,
    group: &crate::GraphExecutionGroup,
    boundary_values: &BTreeMap<(u128, String), GraphValue>,
    state: &mut EvaluationState<'_>,
) -> Result<ResidentGroupEvaluation> {
    let compute = state.compute.ok_or_else(|| Error::GraphDocument {
        path: "graphGpuGroup.executor".to_owned(),
        reason: "execution plan selected Slang without a compute executor".to_owned(),
    })?;
    let mut nodes = Vec::new();
    crate::memory::reserve_exact(&mut nodes, group.nodes.len(), "resident group nodes")?;
    nodes.extend(
        unit.nodes
            .iter()
            .filter(|node| resident_group_contains(group, node.definition.guid)),
    );
    if nodes.len() != group.nodes.len() {
        return Err(Error::GraphDocument {
            path: "graphGpuGroup.nodes".to_owned(),
            reason: "resident group does not belong to the evaluated module".to_owned(),
        });
    }

    let external_scalar_keys = boundary_values
        .iter()
        .filter_map(|(key, value)| matches!(value, GraphValue::Scalar(_)).then_some(key.clone()))
        .collect::<BTreeSet<_>>();
    let candidate_stream = boundary_values.values().find_map(|value| match value {
        GraphValue::Candidates(stream) => Some(stream),
        _ => None,
    });
    let field_lineage = boundary_values.values().find_map(|value| match value {
        GraphValue::Scalar(field) => Some(field.lineage),
        _ => None,
    });
    let lineage = candidate_stream
        .map(|stream| stream.lineage)
        .or(field_lineage)
        .ok_or_else(|| Error::GraphDocument {
            path: "graphGpuGroup.inputs".to_owned(),
            reason: "resident group has no candidate-indexed boundary input".to_owned(),
        })?;
    if boundary_values.values().any(|value| match value {
        GraphValue::Candidates(stream) => stream.lineage != lineage,
        GraphValue::Scalar(field) => field.lineage != lineage,
        _ => true,
    }) {
        return Err(Error::GraphDocument {
            path: "graphGpuGroup.inputs".to_owned(),
            reason: "resident boundary inputs do not share one candidate lineage".to_owned(),
        });
    }

    let noise_node_count = nodes
        .iter()
        .filter(|node| node.definition.operator == GraphOperator::Noise)
        .count();
    let gradient_node_count = nodes
        .iter()
        .filter(|node| node.definition.operator == GraphOperator::Gradient)
        .count();
    let input_capacity = 1_usize
        .checked_add(external_scalar_keys.len())
        .and_then(|count| count.checked_add(noise_node_count.checked_mul(11)?))
        .and_then(|count| count.checked_add(gradient_node_count.checked_mul(6)?))
        .ok_or(Error::NumericOverflow)?;
    let mut input_types = Vec::new();
    crate::memory::reserve_exact(&mut input_types, input_capacity, "resident input types")?;
    input_types.push(GraphGpuRegisterType::CandidateMask);
    let mut bindings = Vec::new();
    crate::memory::reserve_exact(&mut bindings, input_capacity, "resident input bindings")?;
    bindings.push(ResidentInputBinding::CandidateMask);
    let mut external_registers = BTreeMap::new();
    external_registers.extend(boundary_values.iter().filter_map(|(key, value)| {
        matches!(value, GraphValue::Candidates(_)).then_some((key.clone(), GraphGpuRegister(0)))
    }));
    for key in &external_scalar_keys {
        let register = GraphGpuRegister(input_types.len() as u32);
        external_registers.insert(key.clone(), register);
        input_types.push(GraphGpuRegisterType::FixedScalar);
        bindings.push(ResidentInputBinding::ExternalScalar(key.clone()));
    }
    let mut noise_inputs = BTreeMap::new();
    let mut gradient_inputs = BTreeMap::new();
    for node in &nodes {
        match node.definition.operator {
            GraphOperator::Noise => {
                let corners = std::array::from_fn(|corner| {
                    let register = GraphGpuRegister(input_types.len() as u32);
                    input_types.push(GraphGpuRegisterType::FixedScalar);
                    bindings.push(ResidentInputBinding::NoiseCorner {
                        node: node.definition.guid,
                        corner,
                    });
                    register
                });
                let blend = std::array::from_fn(|axis| {
                    let register = GraphGpuRegister(input_types.len() as u32);
                    input_types.push(GraphGpuRegisterType::Unit);
                    bindings.push(ResidentInputBinding::NoiseBlend {
                        node: node.definition.guid,
                        axis,
                    });
                    register
                });
                noise_inputs.insert(node.definition.guid, (corners, blend));
            }
            GraphOperator::Gradient => {
                let position = std::array::from_fn(|axis| {
                    let register = GraphGpuRegister(input_types.len() as u32);
                    input_types.push(GraphGpuRegisterType::WorldTick);
                    bindings.push(ResidentInputBinding::GradientPosition {
                        node: node.definition.guid,
                        axis,
                    });
                    register
                });
                let exact_origin = std::array::from_fn(|axis| {
                    let register = GraphGpuRegister(input_types.len() as u32);
                    input_types.push(GraphGpuRegisterType::WorldTick);
                    bindings.push(ResidentInputBinding::GradientOrigin {
                        node: node.definition.guid,
                        axis,
                    });
                    register
                });
                gradient_inputs.insert(node.definition.guid, (position, exact_origin));
            }
            _ => {}
        }
    }

    let mut instructions = Vec::new();
    crate::memory::reserve_exact(
        &mut instructions,
        nodes.len(),
        "resident graph instructions",
    )?;
    let mut result_registers = BTreeMap::new();
    let mut field_importance_index = None;
    for (index, node) in nodes.iter().enumerate() {
        let destination = GraphGpuRegister((input_types.len() + index) as u32);
        let instruction = match node.definition.operator {
            GraphOperator::Noise => {
                let frequency =
                    fixed_parameter(node, "frequency", DecisionScalar::from_bits(65_536))?;
                if frequency.bits() <= 0 {
                    return Err(Error::GraphDocument {
                        path: node.debug_symbol.label.clone(),
                        reason: "noise frequency must be positive".to_owned(),
                    });
                }
                let amplitude =
                    fixed_parameter(node, "amplitude", DecisionScalar::from_bits(65_536))?;
                let (corners, blend) = noise_inputs[&node.definition.guid];
                GraphGpuInstruction::Noise {
                    destination,
                    corners,
                    blend,
                    amplitude: amplitude.bits(),
                }
            }
            GraphOperator::Gradient => {
                let (position, exact_origin) = gradient_inputs[&node.definition.guid];
                GraphGpuInstruction::Gradient {
                    destination,
                    position,
                    exact_origin,
                    direction: fixed_vec3_parameter(
                        node,
                        "direction",
                        [DecisionScalar::from_bits(0); 3],
                    )?
                    .map(DecisionScalar::bits),
                    scale: fixed_parameter(node, "scale", DecisionScalar::from_bits(0))?.bits(),
                    bias: fixed_parameter(node, "bias", DecisionScalar::from_bits(0))?.bits(),
                }
            }
            GraphOperator::Curve => {
                let curve = curve_parameter(node, "curve")?;
                DecisionCurve::validate_points(curve)?;
                GraphGpuInstruction::Curve {
                    destination,
                    input: resident_source_register(
                        unit,
                        group,
                        &external_registers,
                        &result_registers,
                        node.definition.guid,
                        "field",
                    )?,
                    points: {
                        let mut points = Vec::new();
                        crate::memory::reserve_exact(
                            &mut points,
                            curve.len(),
                            "resident curve points",
                        )?;
                        points.extend(curve.iter().map(|(x, y)| (x.bits(), y.bits())));
                        points
                    },
                }
            }
            GraphOperator::Remap => {
                let input_min = fixed_parameter(node, "inputMin", DecisionScalar::from_bits(0))?;
                let input_max = fixed_parameter(node, "inputMax", DecisionScalar::from_bits(0))?;
                let output_min = fixed_parameter(node, "outputMin", DecisionScalar::from_bits(0))?;
                let output_max = fixed_parameter(node, "outputMax", DecisionScalar::from_bits(0))?;
                GraphGpuInstruction::Remap {
                    destination,
                    input: resident_source_register(
                        unit,
                        group,
                        &external_registers,
                        &result_registers,
                        node.definition.guid,
                        "field",
                    )?,
                    input_min: input_min.bits(),
                    input_max: input_max.bits(),
                    output_min: output_min.bits(),
                    output_max: output_max.bits(),
                }
            }
            GraphOperator::Combine => GraphGpuInstruction::Combine {
                destination,
                left: resident_source_register(
                    unit,
                    group,
                    &external_registers,
                    &result_registers,
                    node.definition.guid,
                    "left",
                )?,
                right: resident_source_register(
                    unit,
                    group,
                    &external_registers,
                    &result_registers,
                    node.definition.guid,
                    "right",
                )?,
                operation: combine_operation_parameter(node, "operation")?,
            },
            GraphOperator::Clamp => GraphGpuInstruction::Clamp {
                destination,
                input: resident_source_register(
                    unit,
                    group,
                    &external_registers,
                    &result_registers,
                    node.definition.guid,
                    "field",
                )?,
                minimum: fixed_parameter(node, "minimum", DecisionScalar::from_bits(0))?.bits(),
                maximum: fixed_parameter(node, "maximum", DecisionScalar::from_bits(0))?.bits(),
            },
            GraphOperator::FieldImportance => {
                field_importance_index = Some(index);
                GraphGpuInstruction::FieldImportance {
                    destination,
                    candidates: resident_source_register(
                        unit,
                        group,
                        &external_registers,
                        &result_registers,
                        node.definition.guid,
                        "candidates",
                    )?,
                    weights: resident_source_register(
                        unit,
                        group,
                        &external_registers,
                        &result_registers,
                        node.definition.guid,
                        "weights",
                    )?,
                    threshold: unit_parameter(node, "threshold", UnitInterval::ZERO)?.bits(),
                }
            }
            _ => {
                return Err(Error::GraphDocument {
                    path: node.debug_symbol.label.clone(),
                    reason: "execution group contains an unsupported resident operator".to_owned(),
                });
            }
        };
        let output_pin = if node.definition.operator == GraphOperator::FieldImportance {
            "candidates"
        } else {
            "field"
        };
        result_registers.insert((node.definition.guid, output_pin.to_owned()), destination);
        instructions.push(instruction);
    }

    let field_boundary = group
        .outputs
        .iter()
        .find(|output| output.domain == GraphDomain::ScalarField);
    let output_register = field_boundary
        .map(|output| {
            result_registers
                .get(&(output.pin.node.node, output.pin.pin.clone()))
                .copied()
                .ok_or_else(|| Error::GraphDocument {
                    path: "graphGpuGroup.output".to_owned(),
                    reason: "resident field boundary has no result register".to_owned(),
                })
        })
        .transpose()?;
    let terminal_mask = field_importance_index.map_or(GraphGpuRegister(0), |index| {
        GraphGpuRegister((input_types.len() + index) as u32)
    });
    let program = GraphGpuProgram::new(input_types, instructions, output_register, terminal_mask)?;

    let identity_count = candidate_stream.map_or_else(
        || {
            boundary_values
                .values()
                .find_map(|value| match value {
                    GraphValue::Scalar(field) => Some(field.values.len()),
                    _ => None,
                })
                .unwrap_or(0)
        },
        |stream| stream.candidates.len(),
    );
    let mut identities = Vec::new();
    crate::memory::reserve_exact(&mut identities, identity_count, "resident identities")?;
    if let Some(stream) = candidate_stream {
        identities.extend(stream.candidates.iter().map(|candidate| candidate.identity));
    } else if let Some(field) = boundary_values.values().find_map(|value| match value {
        GraphValue::Scalar(field) => Some(field),
        _ => None,
    }) {
        identities.extend(field.values.keys().copied());
    }
    let mut invocations = GraphGpuInvocationBatch::with_capacity(&program, identities.len())?;
    let mut base_masks = Vec::new();
    crate::memory::reserve_exact(&mut base_masks, identities.len(), "resident base masks")?;
    for identity in &identities {
        let base_mask = external_scalar_keys.iter().all(|key| {
            boundary_values.get(key).is_some_and(|value| match value {
                GraphValue::Scalar(field) => field.values.contains_key(identity),
                _ => false,
            })
        });
        base_masks.push(base_mask);
        let candidate = candidate_stream.and_then(|stream| {
            stream
                .candidates
                .binary_search_by_key(identity, |candidate| candidate.identity)
                .ok()
                .and_then(|index| stream.candidates.get(index))
        });
        let mut noise_components = [None; GRAPH_GPU_MAX_INSTRUCTIONS];
        let mut noise_component_count = 0_usize;
        for node in noise_inputs.keys() {
            let compiled = nodes
                .iter()
                .find(|candidate| candidate.definition.guid == *node)
                .ok_or(Error::NumericOverflow)?;
            let candidate = candidate.ok_or_else(|| Error::GraphDocument {
                path: compiled.debug_symbol.label.clone(),
                reason: "resident noise input has no candidate position".to_owned(),
            })?;
            let frequency =
                fixed_parameter(compiled, "frequency", DecisionScalar::from_bits(65_536))?;
            let channel = u32_parameter(compiled, "channel", 0)?;
            noise_components[noise_component_count] = Some((
                *node,
                coherent_value_noise_components(
                    compiled,
                    state,
                    candidate.position,
                    frequency,
                    channel,
                )?,
            ));
            noise_component_count += 1;
        }
        invocations.push(bindings.iter().map(|binding| -> Result<GraphGpuValue> {
            Ok(match binding {
                ResidentInputBinding::CandidateMask => GraphGpuValue::CandidateMask(base_mask),
                ResidentInputBinding::ExternalScalar(key) => {
                    let value = boundary_values.get(key).and_then(|value| match value {
                        GraphValue::Scalar(field) => field.values.get(identity),
                        _ => None,
                    });
                    GraphGpuValue::FixedScalar(value.map_or(0, |value| value.bits()))
                }
                ResidentInputBinding::NoiseCorner { node, corner } => {
                    let components = noise_components[..noise_component_count]
                        .iter()
                        .flatten()
                        .find(|(candidate, _)| candidate == node)
                        .ok_or(Error::NumericOverflow)?;
                    GraphGpuValue::FixedScalar(components.1.0[*corner].bits())
                }
                ResidentInputBinding::NoiseBlend { node, axis } => {
                    let components = noise_components[..noise_component_count]
                        .iter()
                        .flatten()
                        .find(|(candidate, _)| candidate == node)
                        .ok_or(Error::NumericOverflow)?;
                    GraphGpuValue::Unit(components.1.1[*axis].bits())
                }
                ResidentInputBinding::GradientPosition { node, axis } => {
                    let compiled = nodes
                        .iter()
                        .find(|candidate| candidate.definition.guid == *node)
                        .ok_or(Error::NumericOverflow)?;
                    let candidate = candidate.ok_or_else(|| Error::GraphDocument {
                        path: compiled.debug_symbol.label.clone(),
                        reason: "resident gradient input has no candidate position".to_owned(),
                    })?;
                    GraphGpuValue::WorldTick(candidate.position.global_ticks()[*axis])
                }
                ResidentInputBinding::GradientOrigin { node, axis } => GraphGpuValue::WorldTick(
                    world_position_parameter(
                        nodes
                            .iter()
                            .find(|candidate| candidate.definition.guid == *node)
                            .ok_or(Error::NumericOverflow)?,
                        "exactOrigin",
                    )?[*axis],
                ),
            })
        }))?;
    }
    let allocation_shape = ResidentGroupAllocationShape {
        invocations: identities.len() as u64,
        inputs: input_capacity as u64,
        instructions: nodes.len() as u64,
        curve_instructions: nodes
            .iter()
            .filter(|node| node.definition.operator == GraphOperator::Curve)
            .count() as u64,
        curve_points: nodes
            .iter()
            .filter(|node| node.definition.operator == GraphOperator::Curve)
            .count()
            .checked_mul(GRAPH_GPU_MAX_CURVE_POINTS)
            .ok_or(Error::NumericOverflow)? as u64,
        external_inputs: boundary_values.len() as u64,
        external_pin_bytes: boundary_values.keys().try_fold(0_u64, |total, (_, pin)| {
            total
                .checked_add(pin.len() as u64)
                .ok_or(Error::NumericOverflow)
        })?,
        output_entries: group.outputs.len() as u64,
        output_pin_bytes: group.outputs.iter().try_fold(0_u64, |total, output| {
            total
                .checked_add(output.pin.pin.len() as u64)
                .ok_or(Error::NumericOverflow)
        })?,
        noise_nodes: noise_node_count as u64,
        gradient_nodes: gradient_node_count as u64,
        candidate_output: group
            .outputs
            .iter()
            .any(|output| output.domain == GraphDomain::Candidates),
        scalar_output: group
            .outputs
            .iter()
            .any(|output| output.domain == GraphDomain::ScalarField),
    };
    state.check_transient_memory(resident_group_scratch_bytes(allocation_shape)?)?;
    let gpu_outputs = execute_compute_program(state, compute, &program, &invocations)?;
    let final_candidate_count = gpu_outputs
        .iter()
        .filter(|output| output.candidate_mask)
        .count() as u64;
    let mut outputs = BTreeMap::new();
    if let Some(boundary) = field_boundary {
        let field_node_index = nodes
            .iter()
            .position(|node| node.definition.guid == boundary.pin.node.node)
            .ok_or_else(|| Error::GraphDocument {
                path: "graphGpuGroup.output".to_owned(),
                reason: "resident field boundary node is missing".to_owned(),
            })?;
        let retain_terminal_mask =
            field_importance_index.is_some_and(|importance| importance < field_node_index);
        match boundary.domain {
            GraphDomain::ScalarField => {
                let mut values = BTreeMap::new();
                for ((identity, base_mask), output) in
                    identities.iter().zip(&base_masks).zip(&gpu_outputs)
                {
                    if *base_mask && (!retain_terminal_mask || output.candidate_mask) {
                        if output.value_type != Some(GraphGpuRegisterType::FixedScalar) {
                            return Err(compute_output_type_error(nodes[field_node_index]));
                        }
                        values.insert(*identity, DecisionScalar::from_bits(output.value as i32));
                    }
                }
                outputs.insert(
                    (boundary.pin.node.node, boundary.pin.pin.clone()),
                    GraphValue::Scalar(ScalarFieldSamples { lineage, values }),
                );
            }
            _ => unreachable!("field boundary was filtered by domain"),
        }
    }

    if let Some(importance_index) = field_importance_index {
        let node = nodes[importance_index];
        let stream = candidate_stream.ok_or_else(|| Error::GraphDocument {
            path: node.debug_symbol.label.clone(),
            reason: "resident candidate mask has no candidate metadata".to_owned(),
        })?;
        let mut accepted = Vec::new();
        crate::memory::reserve_exact(
            &mut accepted,
            stream.candidates.len(),
            "resident accepted candidates",
        )?;
        for (candidate, output) in stream.candidates.iter().zip(&gpu_outputs) {
            if output.candidate_mask {
                accepted.push(candidate.clone());
            } else {
                reject_candidate(
                    node,
                    candidate,
                    stream.lineage,
                    CandidateRejectionReason::Threshold,
                    candidate.family,
                    candidate.variation,
                    state,
                )?;
            }
        }
        let accepted = CandidateStream {
            lineage: stream.lineage,
            candidates: accepted,
        };
        for candidate in &accepted.candidates {
            record_candidate_decision(node, candidate, boundary_values.values(), state);
        }
        if let Some(boundary) = group
            .outputs
            .iter()
            .find(|output| output.domain == GraphDomain::Candidates)
        {
            outputs.insert(
                (boundary.pin.node.node, boundary.pin.pin.clone()),
                GraphValue::Candidates(accepted),
            );
        }
    }

    Ok(ResidentGroupEvaluation {
        outputs,
        base_candidate_count: identities.len() as u64,
        final_candidate_count,
        field_importance_index,
        invocation_count: invocations.invocation_count() as u64,
    })
}
