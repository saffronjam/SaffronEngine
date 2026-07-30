//! Scheduled evaluation of one compiled graph unit.

use super::*;

use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use crate::graph::{CompiledDemandSlice, CompiledDemandUnitSlice};
use crate::memory::{checked_memory_sum, requested_btree_bytes_for_len};
use crate::{
    CompiledGraphNode, CompiledGraphUnit, Error, GraphComputeExecutor, GraphExecutionDomain,
    GraphGpuInvocationBatch, GraphGpuProgram, GraphOperator, ProvenanceDecision,
    ProvenanceDecisionOutcome, Result,
};

pub(super) fn evaluate_unit(
    unit: &CompiledGraphUnit,
    unit_demand: &CompiledDemandUnitSlice,
    demand: &CompiledDemandSlice,
    interface_values: &BTreeMap<String, GraphValue>,
    state: &mut EvaluationState<'_>,
) -> Result<BTreeMap<String, GraphValue>> {
    let mut incoming = BTreeMap::<u128, Vec<_>>::new();
    for node in unit
        .nodes
        .iter()
        .filter(|node| unit_demand.contains_node(node.definition.guid))
    {
        let edge_count = unit_demand
            .edges
            .iter()
            .filter(|edge| edge.to_node == node.definition.guid)
            .count();
        let mut edges = Vec::new();
        crate::memory::reserve_exact(&mut edges, edge_count, "incoming graph edges")?;
        incoming.insert(node.definition.guid, edges);
    }
    for edge in &unit_demand.edges {
        incoming
            .get_mut(&edge.to_node)
            .ok_or_else(|| Error::GraphDocument {
                path: "graph.execution".to_owned(),
                reason: "active graph node has no incoming-edge bucket".to_owned(),
            })?
            .push(edge);
    }
    let mut remaining_uses = BTreeMap::<(u128, String), u64>::new();
    for edge in &unit_demand.edges {
        *remaining_uses
            .entry((edge.from_node, edge.from_pin.clone()))
            .or_default() += 1;
    }
    for output in &unit.outputs {
        if unit_demand.outputs.contains(&output.name) {
            *remaining_uses
                .entry((output.node, output.pin.clone()))
                .or_default() += 1;
        }
    }
    let mut values: BTreeMap<(u128, String), GraphValue> = BTreeMap::new();
    let mut live_bytes = 0_u64;
    let mut executed_resident_nodes = BTreeSet::new();
    let execution_plan = state.execution_plan;
    for node in unit
        .nodes
        .iter()
        .filter(|node| unit_demand.contains_node(node.definition.guid))
    {
        state.check_abort()?;
        state.current_node_live_bytes = live_bytes;
        let address = node.address();
        let loads_global = state.should_load_global_node(&address);
        let scheduled_group =
            execution_plan
                .group_for(&address)
                .ok_or_else(|| Error::GraphDocument {
                    path: node.debug_symbol.label.clone(),
                    reason: "execution plan omitted the compiled node".to_owned(),
                })?;
        if !loads_global && scheduled_group.domain == GraphExecutionDomain::SlangCompute {
            if executed_resident_nodes.contains(&node.definition.guid) {
                continue;
            }
            if scheduled_group.nodes.iter().any(|member| {
                !unit_demand.contains_node(member.address.node)
                    || unit
                        .nodes
                        .iter()
                        .find(|candidate| candidate.definition.guid == member.address.node)
                        .is_none_or(|candidate| state.should_load_global_node(&candidate.address()))
            }) {
                return Err(Error::GraphDocument {
                    path: node.debug_symbol.label.clone(),
                    reason: "resident group crosses the active spatial evaluation boundary"
                        .to_owned(),
                });
            }
            let mut boundary_values = BTreeMap::new();
            for edge in unit_demand.edges.iter().filter(|edge| {
                !resident_group_contains(scheduled_group, edge.from_node)
                    && resident_group_contains(scheduled_group, edge.to_node)
            }) {
                let value = if let Some(value) = values
                    .get(&(edge.from_node, edge.from_pin.clone()))
                    .cloned()
                {
                    value
                } else {
                    let source_node = unit
                        .nodes
                        .iter()
                        .find(|candidate| candidate.definition.guid == edge.from_node)
                        .ok_or_else(|| Error::GraphDocument {
                            path: node.debug_symbol.label.clone(),
                            reason: "resident upstream compiled node is missing".to_owned(),
                        })?;
                    state
                        .load_global_node_outputs(source_node, demand)?
                        .remove(&edge.from_pin)
                        .ok_or_else(|| Error::GraphDocument {
                            path: node.debug_symbol.label.clone(),
                            reason: "resident boundary input is missing".to_owned(),
                        })?
                };
                boundary_values.insert((edge.from_node, edge.from_pin.clone()), value);
            }
            let input_value_bytes = boundary_values.values().try_fold(0_u64, |total, value| {
                total
                    .checked_add(value.requested_memory_bytes()?)
                    .ok_or(Error::NumericOverflow)
            })?;
            let started = Instant::now();
            let transferred_before = state.transferred_bytes;
            state.current_node_live_bytes = live_bytes
                .checked_add(input_value_bytes)
                .ok_or(Error::NumericOverflow)?;
            let result = evaluate_resident_group(unit, scheduled_group, &boundary_values, state)?;
            state.current_node_live_bytes = 0;
            state.check_abort()?;
            let transfer_bytes = state
                .transferred_bytes
                .checked_sub(transferred_before)
                .ok_or(Error::NumericOverflow)?;
            let output_bytes = result.outputs.values().try_fold(0_u64, |total, value| {
                total
                    .checked_add(value.requested_memory_bytes()?)
                    .ok_or(Error::NumericOverflow)
            })?;
            let transient_bytes = live_bytes
                .checked_add(input_value_bytes)
                .and_then(|bytes| bytes.checked_add(output_bytes))
                .ok_or(Error::NumericOverflow)?;
            state.check_count(
                "candidate count",
                result
                    .base_candidate_count
                    .max(result.final_candidate_count),
                state.graph.limits.max_candidates,
            )?;
            if transient_bytes > state.graph.limits.max_memory_bytes {
                return Err(Error::GraphLimit {
                    resource: "memory bytes",
                    requested: transient_bytes,
                    limit: state.graph.limits.max_memory_bytes,
                });
            }
            if state.transferred_bytes > state.graph.limits.max_transfer_bytes {
                return Err(Error::GraphLimit {
                    resource: "transfer bytes",
                    requested: state.transferred_bytes,
                    limit: state.graph.limits.max_transfer_bytes,
                });
            }
            let elapsed_micros = started.elapsed().as_micros().try_into().unwrap_or(u64::MAX);
            state
                .diagnostics
                .gpu_groups
                .push(GpuGroupEvaluationDiagnostic {
                    nodes: scheduled_group
                        .nodes
                        .iter()
                        .map(|member| member.address.clone())
                        .collect(),
                    invocation_count: result.invocation_count,
                    transfer_bytes,
                    output_bytes,
                    elapsed_micros,
                });
            for (index, member) in scheduled_group.nodes.iter().enumerate() {
                let compiled = unit
                    .nodes
                    .iter()
                    .find(|candidate| candidate.definition.guid == member.address.node)
                    .ok_or_else(|| Error::GraphDocument {
                        path: node.debug_symbol.label.clone(),
                        reason: "resident diagnostic node is missing".to_owned(),
                    })?;
                let after_mask = result
                    .field_importance_index
                    .is_some_and(|importance| index > importance);
                let is_mask = result.field_importance_index == Some(index);
                let input_candidates = if after_mask {
                    result.final_candidate_count
                } else {
                    result.base_candidate_count
                };
                let output_candidates = if after_mask || is_mask {
                    result.final_candidate_count
                } else {
                    result.base_candidate_count
                };
                let retained_bytes = result
                    .outputs
                    .iter()
                    .filter(|((source, _), _)| *source == member.address.node)
                    .try_fold(0_u64, |total, (_, value)| {
                        total
                            .checked_add(value.requested_memory_bytes()?)
                            .ok_or(Error::NumericOverflow)
                    })?;
                state.diagnostics.nodes.push(NodeEvaluationDiagnostic {
                    module_path: compiled.debug_symbol.module_path.clone(),
                    node: compiled.definition.guid,
                    operator: compiled.definition.operator,
                    symbol: compiled.debug_symbol.label.clone(),
                    input_candidates,
                    output_candidates,
                    output_bytes: retained_bytes,
                    transfer_bytes: 0,
                    predicted_transfer_bytes: compiled.estimate.transfer_bytes,
                    elapsed_micros: 0,
                    execution_domain: GraphExecutionDomain::SlangCompute,
                });
                state.capture_resident_global_outputs(compiled, &result.outputs)?;
            }
            for ((source, pin), value) in result.outputs {
                let source_node = unit
                    .nodes
                    .iter()
                    .find(|candidate| candidate.definition.guid == source)
                    .ok_or_else(|| Error::GraphDocument {
                        path: node.debug_symbol.label.clone(),
                        reason: "resident boundary output node is missing".to_owned(),
                    })?;
                let expected = source_node
                    .outputs
                    .iter()
                    .find(|candidate| candidate.name == pin)
                    .ok_or_else(|| Error::GraphDocument {
                        path: source_node.debug_symbol.label.clone(),
                        reason: format!("resident program emitted unknown pin '{pin}'"),
                    })?;
                if value.domain() != expected.domain {
                    return Err(Error::GraphDocument {
                        path: source_node.debug_symbol.label.clone(),
                        reason: "resident program emitted the wrong value domain".to_owned(),
                    });
                }
                let key = (source, pin);
                if remaining_uses.get(&key).copied().unwrap_or(0) != 0 {
                    live_bytes = live_bytes
                        .checked_add(value.requested_memory_bytes()?)
                        .ok_or(Error::NumericOverflow)?;
                    values.insert(key, value);
                }
            }
            for edge in unit_demand.edges.iter().filter(|edge| {
                !resident_group_contains(scheduled_group, edge.from_node)
                    && resident_group_contains(scheduled_group, edge.to_node)
            }) {
                let key = (edge.from_node, edge.from_pin.clone());
                let Some(uses) = remaining_uses.get_mut(&key) else {
                    continue;
                };
                *uses = uses.checked_sub(1).ok_or_else(|| Error::GraphDocument {
                    path: node.debug_symbol.label.clone(),
                    reason: "resident input live range was consumed more than once".to_owned(),
                })?;
                if *uses == 0
                    && let Some(value) = values.remove(&key)
                {
                    live_bytes = live_bytes
                        .checked_sub(value.requested_memory_bytes()?)
                        .ok_or(Error::NumericOverflow)?;
                }
            }
            executed_resident_nodes.extend(
                scheduled_group
                    .nodes
                    .iter()
                    .map(|member| member.address.node),
            );
            continue;
        }
        let loads_global = state.should_load_global_node(&node.address());
        let mut inputs = BTreeMap::new();
        for edge in incoming
            .get(&node.definition.guid)
            .into_iter()
            .flatten()
            .filter(|_| !loads_global)
        {
            let value = if let Some(value) = values
                .get(&(edge.from_node, edge.from_pin.clone()))
                .cloned()
            {
                value
            } else {
                let source_node = unit
                    .nodes
                    .iter()
                    .find(|candidate| candidate.definition.guid == edge.from_node)
                    .ok_or_else(|| Error::GraphDocument {
                        path: node.debug_symbol.label.clone(),
                        reason: "upstream compiled node is missing".to_owned(),
                    })?;
                let stage = state
                    .graph
                    .spatial_plan()
                    .global_stage_for_node(&source_node.address())
                    .ok_or_else(|| Error::GraphDocument {
                        path: node.debug_symbol.label.clone(),
                        reason: "upstream value is missing".to_owned(),
                    })?;
                if state
                    .scope
                    .current_global_stage()
                    .is_some_and(|current| current.id == stage.id)
                {
                    return Err(Error::GraphDocument {
                        path: node.debug_symbol.label.clone(),
                        reason: "same-stage upstream value was not produced".to_owned(),
                    });
                }
                state
                    .load_global_node_outputs(source_node, demand)?
                    .remove(&edge.from_pin)
                    .ok_or_else(|| Error::GraphDocument {
                        path: node.debug_symbol.label.clone(),
                        reason: "materialized upstream pin is missing".to_owned(),
                    })?
            };
            inputs.insert(edge.to_pin.clone(), value);
        }
        let input_candidates = inputs
            .values()
            .map(GraphValue::candidate_count)
            .max()
            .unwrap_or(0) as u64;
        let input_value_bytes = inputs.values().try_fold(0_u64, |total, value| {
            total
                .checked_add(value.requested_memory_bytes()?)
                .ok_or(Error::NumericOverflow)
        })?;
        let started = Instant::now();
        let transferred_before = state.transferred_bytes;
        state.current_node_live_bytes = live_bytes
            .checked_add(input_value_bytes)
            .ok_or(Error::NumericOverflow)?;
        let (outputs, execution_domain, loaded_global) =
            evaluate_node_scheduled(unit, node, &inputs, interface_values, demand, state)?;
        state.check_abort()?;
        if !loaded_global {
            let decision_candidates = outputs
                .values()
                .map(GraphValue::candidate_count)
                .max()
                .unwrap_or(0) as u64;
            let decision_output_bytes = outputs.values().try_fold(0_u64, |total, value| {
                total
                    .checked_add(value.requested_memory_bytes()?)
                    .ok_or(Error::NumericOverflow)
            })?;
            state.check_transient_memory(checked_memory_sum([
                decision_output_bytes,
                requested_btree_bytes_for_len::<CandidateIdentity, ()>(decision_candidates)?,
            ])?)?;
            record_candidate_decisions(node, &inputs, &outputs, state)?;
            state.capture_global_outputs(node, &outputs)?;
        }
        state.current_node_live_bytes = 0;
        let output_candidates = outputs
            .values()
            .map(GraphValue::candidate_count)
            .max()
            .unwrap_or(0) as u64;
        let output_bytes = outputs.values().try_fold(0_u64, |total, value| {
            total
                .checked_add(value.requested_memory_bytes()?)
                .ok_or(Error::NumericOverflow)
        })?;
        state.check_count(
            "candidate count",
            output_candidates,
            state.graph.limits.max_candidates,
        )?;
        let transient_bytes = live_bytes
            .checked_add(input_value_bytes)
            .and_then(|value| value.checked_add(output_bytes))
            .ok_or(Error::NumericOverflow)?;
        if transient_bytes > state.graph.limits.max_memory_bytes {
            return Err(Error::GraphLimit {
                resource: "memory bytes",
                requested: transient_bytes,
                limit: state.graph.limits.max_memory_bytes,
            });
        }
        let transfer_bytes = state
            .transferred_bytes
            .checked_sub(transferred_before)
            .ok_or(Error::NumericOverflow)?;
        if state.transferred_bytes > state.graph.limits.max_transfer_bytes {
            return Err(Error::GraphLimit {
                resource: "transfer bytes",
                requested: state.transferred_bytes,
                limit: state.graph.limits.max_transfer_bytes,
            });
        }
        if !loaded_global {
            state.diagnostics.nodes.push(NodeEvaluationDiagnostic {
                module_path: node.debug_symbol.module_path.clone(),
                node: node.definition.guid,
                operator: node.definition.operator,
                symbol: node.debug_symbol.label.clone(),
                input_candidates,
                output_candidates,
                output_bytes,
                transfer_bytes,
                predicted_transfer_bytes: node.estimate.transfer_bytes,
                elapsed_micros: started.elapsed().as_micros().try_into().unwrap_or(u64::MAX),
                execution_domain,
            });
        }
        for (pin, value) in outputs {
            let expected = node
                .outputs
                .iter()
                .find(|candidate| candidate.name == pin)
                .ok_or_else(|| Error::GraphDocument {
                    path: node.debug_symbol.label.clone(),
                    reason: format!("operator emitted unknown pin '{pin}'"),
                })?;
            if value.domain() != expected.domain {
                return Err(Error::GraphDocument {
                    path: node.debug_symbol.label.clone(),
                    reason: "operator emitted the wrong value domain".to_owned(),
                });
            }
            let key = (node.definition.guid, pin);
            if remaining_uses.get(&key).copied().unwrap_or(0) != 0 {
                live_bytes = live_bytes
                    .checked_add(value.requested_memory_bytes()?)
                    .ok_or(Error::NumericOverflow)?;
                values.insert(key, value);
            }
        }
        for edge in incoming.get(&node.definition.guid).into_iter().flatten() {
            let key = (edge.from_node, edge.from_pin.clone());
            let Some(uses) = remaining_uses.get_mut(&key) else {
                continue;
            };
            *uses = uses.checked_sub(1).ok_or_else(|| Error::GraphDocument {
                path: node.debug_symbol.label.clone(),
                reason: "input live range was consumed more than once".to_owned(),
            })?;
            if *uses == 0
                && let Some(value) = values.remove(&key)
            {
                live_bytes = live_bytes
                    .checked_sub(value.requested_memory_bytes()?)
                    .ok_or(Error::NumericOverflow)?;
            }
        }
    }
    let mut outputs = BTreeMap::new();
    for output in &unit.outputs {
        if !unit_demand.outputs.contains(&output.name) {
            continue;
        }
        if let Some(value) = values.get(&(output.node, output.pin.clone())).cloned() {
            outputs.insert(output.name.clone(), value);
        } else {
            return Err(Error::GraphDocument {
                path: format!("graph.outputs.{}", output.name),
                reason: "source value is missing".to_owned(),
            });
        }
    }
    Ok(outputs)
}

fn record_candidate_decisions(
    node: &CompiledGraphNode,
    inputs: &BTreeMap<String, GraphValue>,
    outputs: &BTreeMap<String, GraphValue>,
    state: &mut EvaluationState<'_>,
) -> Result<()> {
    let mut seen = BTreeSet::new();
    for value in outputs.values() {
        match value {
            GraphValue::Candidates(stream) => {
                for candidate in &stream.candidates {
                    if seen.insert(candidate.identity) {
                        record_candidate_decision(node, candidate, inputs.values(), state);
                    }
                }
            }
            GraphValue::Surface(surface)
                if node.definition.operator == GraphOperator::SurfaceProjection =>
            {
                for identity in surface.values.keys() {
                    if !seen.insert(*identity) {
                        continue;
                    }
                    let candidate = inputs
                        .values()
                        .filter_map(|value| match value {
                            GraphValue::Candidates(stream) => Some(stream),
                            _ => None,
                        })
                        .find_map(|stream| {
                            stream
                                .candidates
                                .binary_search_by_key(identity, |candidate| candidate.identity)
                                .ok()
                                .and_then(|index| stream.candidates.get(index))
                        })
                        .ok_or_else(|| Error::GraphDocument {
                            path: node.debug_symbol.label.clone(),
                            reason: format!(
                                "projected surface sample {:?} has no input candidate",
                                identity
                            ),
                        })?;
                    record_candidate_decision(node, candidate, inputs.values(), state);
                }
            }
            _ => {}
        }
    }
    Ok(())
}

pub(super) fn record_candidate_decision<'a>(
    node: &CompiledGraphNode,
    candidate: &GraphCandidate,
    inputs: impl IntoIterator<Item = &'a GraphValue>,
    state: &mut EvaluationState<'_>,
) {
    let previous = state.candidate_decisions.get(&candidate.identity).copied();
    let mut parents = BTreeSet::new();
    if let Some(previous) = previous {
        parents.insert(previous);
    }
    for reference in [candidate.parent, candidate.colony].into_iter().flatten() {
        if let Some(parent) = state.candidate_decisions.get(&reference.identity) {
            parents.insert(*parent);
        }
    }
    if parents.is_empty() && candidate.identity.ancestor != 0 {
        for stream in inputs.into_iter().filter_map(|value| match value {
            GraphValue::Candidates(stream) => Some(stream),
            _ => None,
        }) {
            for ancestor in &stream.candidates {
                if ancestor.identity.ordinal == candidate.identity.ancestor
                    && let Some(parent) = state.candidate_decisions.get(&ancestor.identity)
                {
                    parents.insert(*parent);
                }
            }
        }
    }
    let outcome = if previous.is_none() && parents.is_empty() {
        ProvenanceDecisionOutcome::Produced
    } else {
        ProvenanceDecisionOutcome::Retained
    };
    let decision = state.provenance.intern_decision(ProvenanceDecision {
        parents: parents.into_iter().collect(),
        subgraph_path: node.debug_symbol.module_path.clone(),
        node: node.definition.guid,
        operator: node.definition.operator,
        candidate: candidate.identity.ordinal,
        outcome,
    });
    state
        .candidate_decisions
        .insert(candidate.identity, decision);
}

fn evaluate_node_scheduled(
    unit: &CompiledGraphUnit,
    node: &CompiledGraphNode,
    inputs: &BTreeMap<String, GraphValue>,
    interface_values: &BTreeMap<String, GraphValue>,
    demand: &CompiledDemandSlice,
    state: &mut EvaluationState<'_>,
) -> Result<(BTreeMap<String, GraphValue>, GraphExecutionDomain, bool)> {
    if state.should_load_global_node(&node.address()) {
        return Ok((
            state.load_global_node_outputs(node, demand)?,
            GraphExecutionDomain::ReferenceCpu,
            true,
        ));
    }
    let domain = state
        .execution_plan
        .domain_for(&node.debug_symbol.module_path, node.definition.guid)
        .ok_or_else(|| Error::GraphDocument {
            path: node.debug_symbol.label.clone(),
            reason: "execution plan omitted the compiled node".to_owned(),
        })?;
    match domain {
        GraphExecutionDomain::SlangCompute => Err(Error::GraphDocument {
            path: node.debug_symbol.label.clone(),
            reason: "resident groups must execute at the unit scheduler boundary".to_owned(),
        }),
        GraphExecutionDomain::ReferenceCpu => Ok((
            evaluate_node(unit, node, inputs, interface_values, demand, state)?,
            GraphExecutionDomain::ReferenceCpu,
            false,
        )),
        GraphExecutionDomain::ParallelCpu => Ok((
            evaluate_node(unit, node, inputs, interface_values, demand, state)?,
            GraphExecutionDomain::ParallelCpu,
            false,
        )),
    }
}

pub(super) fn execute_compute_program(
    state: &mut EvaluationState<'_>,
    compute: &dyn GraphComputeExecutor,
    program: &GraphGpuProgram,
    invocations: &GraphGpuInvocationBatch,
) -> Result<Vec<crate::GraphGpuOutput>> {
    if invocations.invocation_count() == 0 {
        return Ok(Vec::new());
    }
    state.check_abort()?;
    let invocation_words =
        u64::try_from(invocations.encoded_word_count()).map_err(|_| Error::NumericOverflow)?;
    let output_words = u64::try_from(invocations.invocation_count())
        .map_err(|_| Error::NumericOverflow)?
        .checked_mul(
            u64::try_from(crate::GRAPH_GPU_OUTPUT_WORDS).map_err(|_| Error::NumericOverflow)?,
        )
        .ok_or(Error::NumericOverflow)?;
    let transfer_bytes = u64::try_from(program.encoded_word_count())
        .map_err(|_| Error::NumericOverflow)?
        .checked_add(invocation_words)
        .and_then(|words| words.checked_add(output_words))
        .and_then(|words| words.checked_mul(std::mem::size_of::<u32>() as u64))
        .ok_or(Error::NumericOverflow)?;
    let requested = state
        .transferred_bytes
        .checked_add(transfer_bytes)
        .ok_or(Error::NumericOverflow)?;
    state.check_count(
        "transfer bytes",
        requested,
        state.graph.limits.max_transfer_bytes,
    )?;
    let outputs =
        compute.execute_program(program, invocations, state.cancellation, state.deadline)?;
    state.transferred_bytes = requested;
    state.check_abort()?;
    if outputs.len() != invocations.invocation_count() {
        return Err(Error::GraphDocument {
            path: "slang-compute.outputs".to_owned(),
            reason: "compute executor returned the wrong result count".to_owned(),
        });
    }
    if outputs.iter().any(|output| !output.valid) {
        return Err(Error::NumericOverflow);
    }
    Ok(outputs)
}

pub(super) fn compute_output_type_error(node: &CompiledGraphNode) -> Error {
    Error::GraphDocument {
        path: node.debug_symbol.label.clone(),
        reason: "compute executor returned the wrong output type".to_owned(),
    }
}
