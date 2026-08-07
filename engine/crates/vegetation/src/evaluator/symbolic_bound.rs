//! Symbolic bounds for one graph unit and one complete evaluation.

use super::*;

use std::collections::{BTreeMap, BTreeSet};

use saffron_spatial::WorldCellKey;

use crate::graph::CompiledDemandUnitSlice;
use crate::memory::{
    ALLOCATION_OVERHEAD_BYTES, checked_memory_sum, requested_btree_bytes_for_len,
    requested_vec_bytes_for_len,
};
use crate::{
    CompiledBiomeGraph, CompiledGraphUnit, Error, GRAPH_GPU_INSTRUCTION_WORDS,
    GRAPH_GPU_INVOCATION_WORDS, GRAPH_GPU_MAX_CURVE_POINTS, GRAPH_GPU_OUTPUT_WORDS,
    GRAPH_GPU_PROGRAM_HEADER_WORDS, GraphDomain, GraphExecutionDomain, GraphExecutionPlan,
    GraphNodeAddress, GraphOperator, PlantPoint, PlantPointColumns, ProvenanceDecision,
    ProvenanceDecisionHandle, ProvenanceRecord, QualifiedGraphPin, Result,
};

fn symbolic_evaluate_unit(
    context: SymbolicTraversalContext<'_>,
    unit: &CompiledGraphUnit,
    unit_demand: &CompiledDemandUnitSlice,
    interface_values: &BTreeMap<String, SymbolicValueBound>,
    bound: &mut SymbolicEvaluationBound,
    values_by_pin: &mut BTreeMap<QualifiedGraphPin, SymbolicValueBound>,
    executed_nodes: &mut BTreeSet<GraphNodeAddress>,
) -> Result<BTreeMap<String, SymbolicValueBound>> {
    let graph = context.graph;
    let scope = context.scope;
    let inputs = context.inputs;
    let limits = graph.limits;
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
        crate::memory::reserve_exact(&mut edges, edge_count, "symbolic incoming graph edges")?;
        incoming.insert(node.definition.guid, edges);
    }
    for edge in &unit_demand.edges {
        incoming
            .get_mut(&edge.to_node)
            .ok_or_else(|| Error::GraphDocument {
                path: "graph.symbolicBound".to_owned(),
                reason: "compiled graph edge targets an unknown node".to_owned(),
            })?
            .push(edge);
    }
    let mut values = BTreeMap::<(u128, String), SymbolicValueBound>::new();
    for node in unit
        .nodes
        .iter()
        .filter(|node| unit_demand.contains_node(node.definition.guid))
    {
        context.guard.check()?;
        let loads_global = symbolic_should_load_global_node(graph, scope, &node.address());
        if !loads_global {
            let input_clone_bytes = incoming
                .get(&node.definition.guid)
                .into_iter()
                .flatten()
                .try_fold(0_u64, |total, edge| {
                    let value = values
                        .get(&(edge.from_node, edge.from_pin.clone()))
                        .ok_or_else(|| Error::GraphDocument {
                            path: node.debug_symbol.label.clone(),
                            reason: format!("symbolic input '{}' is missing", edge.to_pin),
                        })?;
                    bound_add("memory bytes", total, value.bytes, limits.max_memory_bytes)
                })?;
            bound.memory_bytes = bound_add(
                "memory bytes",
                bound.memory_bytes,
                input_clone_bytes,
                limits.max_memory_bytes,
            )?;
        }
        let outputs = if loads_global {
            symbolic_load_global_node_outputs(graph, context.demand, scope, inputs, node, bound)?
        } else {
            executed_nodes.insert(node.address());
            let produced = match node.definition.operator {
                GraphOperator::InterfaceInput => {
                    let name = string_parameter(node, "name", "")?;
                    BTreeMap::from([(
                        "value".to_owned(),
                        *interface_values
                            .get(name)
                            .ok_or_else(|| Error::GraphDocument {
                                path: node.debug_symbol.label.clone(),
                                reason: format!("symbolic module input '{name}' is missing"),
                            })?,
                    )])
                }
                GraphOperator::ModuleCall => {
                    let module = node.module.as_deref().ok_or_else(|| Error::GraphDocument {
                        path: node.debug_symbol.label.clone(),
                        reason: "compiled module is missing".to_owned(),
                    })?;
                    let module_inputs = incoming
                        .get(&node.definition.guid)
                        .into_iter()
                        .flatten()
                        .map(|edge| {
                            Ok((
                                edge.to_pin.clone(),
                                *values
                                    .get(&(edge.from_node, edge.from_pin.clone()))
                                    .ok_or_else(|| Error::GraphDocument {
                                        path: node.debug_symbol.label.clone(),
                                        reason: format!(
                                            "symbolic module input '{}' is missing",
                                            edge.to_pin
                                        ),
                                    })?,
                            ))
                        })
                        .collect::<Result<BTreeMap<_, _>>>()?;
                    let child_demand = child_demand_unit(context.demand, node)?;
                    symbolic_evaluate_unit(
                        context,
                        module,
                        child_demand,
                        &module_inputs,
                        bound,
                        values_by_pin,
                        executed_nodes,
                    )?
                }
                _ => symbolic_node_outputs(context, unit, node, &incoming, &values, bound)?,
            };
            let mut outputs = NodeOutputBuilder::new(NodeOutputDemand::new(context.demand, node));
            outputs.extend(produced)?;
            outputs.finish()?
        };
        if !loads_global {
            let metadata_bytes = checked_memory_sum([
                requested_vec_bytes_for_len::<NodeEvaluationDiagnostic>(1)?,
                requested_vec_bytes_for_len::<u128>(node.debug_symbol.module_path.len() as u64)?,
                requested_vec_bytes_for_len::<u8>(node.debug_symbol.label.len() as u64)?,
            ])?;
            bound.diagnostic_metadata_bytes = bound_add(
                "memory bytes",
                bound.diagnostic_metadata_bytes,
                metadata_bytes,
                limits.max_memory_bytes,
            )?;
        }
        let projection_decision_items =
            if !loads_global && node.definition.operator == GraphOperator::SurfaceProjection {
                outputs.values().map(|value| value.items).max().unwrap_or(0)
            } else {
                0
            };
        if projection_decision_items != 0 {
            bound.candidate_decision_scratch_bytes =
                bound
                    .candidate_decision_scratch_bytes
                    .max(requested_btree_bytes_for_len::<CandidateIdentity, ()>(
                        projection_decision_items,
                    )?);
            bound.candidate_events = bound_add(
                "diagnostic samples",
                bound.candidate_events,
                projection_decision_items,
                u64::MAX,
            )?;
            let provenance = bound_mul(
                "memory bytes",
                projection_decision_items,
                symbolic_provenance_bytes_per_decision(node)?,
                limits.max_memory_bytes,
            )?;
            bound.provenance_bytes = bound_add(
                "memory bytes",
                bound.provenance_bytes,
                provenance,
                limits.max_memory_bytes,
            )?;
        }
        for (pin, value) in outputs {
            let candidate_items = if value.domain == Some(GraphDomain::Candidates) {
                value.items
            } else {
                0
            };
            if candidate_items != 0 {
                bound.candidate_decision_scratch_bytes = bound
                    .candidate_decision_scratch_bytes
                    .max(requested_btree_bytes_for_len::<CandidateIdentity, ()>(
                        candidate_items,
                    )?);
            }
            bound.candidate_peak = bound.candidate_peak.max(candidate_items);
            if !loads_global && node.definition.operator != GraphOperator::SurfaceProjection {
                bound.candidate_events = bound_add(
                    "diagnostic samples",
                    bound.candidate_events,
                    candidate_items,
                    u64::MAX,
                )?;
                let provenance = bound_mul(
                    "memory bytes",
                    candidate_items,
                    symbolic_provenance_bytes_per_decision(node)?,
                    limits.max_memory_bytes,
                )?;
                bound.provenance_bytes = bound_add(
                    "memory bytes",
                    bound.provenance_bytes,
                    provenance,
                    limits.max_memory_bytes,
                )?;
            }
            bound.memory_bytes = bound_add(
                "memory bytes",
                bound.memory_bytes,
                value.bytes,
                limits.max_memory_bytes,
            )?;
            values_by_pin.insert(
                QualifiedGraphPin {
                    node: node.address(),
                    pin: pin.clone(),
                },
                value,
            );
            values.insert((node.definition.guid, pin), value);
        }
    }
    let mut outputs = BTreeMap::new();
    for output in &unit.outputs {
        if !unit_demand.outputs.contains(&output.name) {
            continue;
        }
        if let Some(value) = values.get(&(output.node, output.pin.clone())) {
            outputs.insert(output.name.clone(), *value);
        } else {
            return Err(Error::GraphDocument {
                path: format!("graph.outputs.{}", output.name),
                reason: "symbolic output is missing".to_owned(),
            });
        }
    }
    let unit_output_clone_bytes = outputs.values().try_fold(0_u64, |total, value| {
        bound_add("memory bytes", total, value.bytes, limits.max_memory_bytes)
    })?;
    bound.memory_bytes = bound_add(
        "memory bytes",
        bound.memory_bytes,
        unit_output_clone_bytes,
        limits.max_memory_bytes,
    )?;
    if unit.role == crate::BiomeRole::Root {
        for value in outputs.values() {
            match value.domain {
                Some(GraphDomain::MacroPoints) => {
                    bound.accepted = bound_add(
                        "accepted count",
                        bound.accepted,
                        value.items,
                        limits.max_macro_points,
                    )?;
                }
                Some(GraphDomain::MicroField) => {
                    bound.micro_samples = bound_add(
                        "micro samples",
                        bound.micro_samples,
                        value.items,
                        limits.max_micro_samples,
                    )?;
                }
                _ => {}
            }
        }
    }
    Ok(outputs)
}

pub(super) fn symbolic_evaluation_bound(
    graph: &CompiledBiomeGraph,
    inputs: &GraphEvaluationInputs,
    plan: &GraphExecutionPlan,
    scope: SymbolicEvaluationScope<'_>,
    guard: PreflightGuard<'_>,
) -> Result<SymbolicPlannedEvaluation> {
    guard.check()?;
    let ancestor_references = ancestor_reference_upper_bound(graph, inputs, scope.is_cell())?;
    let mut bound = SymbolicEvaluationBound::default();
    let mut values_by_pin = BTreeMap::new();
    let mut executed_nodes = BTreeSet::new();
    let demand = compiled_demand_slice(graph, scope.current_global_stage())?;
    let context = SymbolicTraversalContext {
        graph,
        demand,
        scope,
        inputs,
        guard,
    };
    let public_outputs = symbolic_evaluate_unit(
        context,
        &graph.root,
        root_demand_unit(demand)?,
        &BTreeMap::new(),
        &mut bound,
        &mut values_by_pin,
        &mut executed_nodes,
    )?;
    let terminal_micro_tiles = public_outputs
        .values()
        .filter(|value| value.domain == Some(GraphDomain::MicroField))
        .try_fold(0_u64, |total, value| {
            total.checked_add(value.items).ok_or(Error::NumericOverflow)
        })?;
    let terminal_diagnostic_streams = public_outputs
        .values()
        .filter(|value| value.domain == Some(GraphDomain::Diagnostics))
        .try_fold(0_u64, |total, value| {
            total.checked_add(value.items).ok_or(Error::NumericOverflow)
        })?;
    let result_assembly_bytes = checked_memory_sum([
        requested_vec_bytes_for_len::<PlantPoint>(bound.accepted)?,
        requested_vec_bytes_for_len::<MicroFieldTile>(terminal_micro_tiles)?,
        requested_vec_bytes_for_len::<NamedDiagnosticStream>(terminal_diagnostic_streams)?,
        requested_vec_bytes_for_len::<QuantizedSurfaceProjectionTile>(bound.published_input_tiles)?,
        requested_vec_bytes_for_len::<QuantizedSurfaceFieldQueryTile>(bound.published_input_tiles)?,
        requested_vec_bytes_for_len::<WorldCellKey>(ancestor_references)?,
    ])?;
    bound.memory_bytes = bound_add(
        "memory bytes",
        bound.memory_bytes,
        result_assembly_bytes,
        graph.limits.max_memory_bytes,
    )?;
    let rejection_bytes = requested_vec_bytes_for_len::<RejectedCandidate>(bound.rejected)?;
    let rejection_lineage_bytes = checked_memory_sum([
        requested_btree_bytes_for_len::<CandidateLineage, Vec<RejectedCandidate>>(bound.rejected)?,
        requested_vec_bytes_for_len::<RejectedCandidate>(bound.rejected)?,
        bound
            .rejected
            .checked_mul(ALLOCATION_OVERHEAD_BYTES)
            .ok_or(Error::NumericOverflow)?,
    ])?;
    bound.memory_bytes = bound_add(
        "memory bytes",
        bound.memory_bytes,
        bound.candidate_decision_scratch_bytes,
        graph.limits.max_memory_bytes,
    )?;
    bound.memory_bytes = bound_add(
        "memory bytes",
        bound.memory_bytes,
        checked_memory_sum([rejection_bytes, rejection_lineage_bytes])?,
        graph.limits.max_memory_bytes,
    )?;
    let provenance_records = bound
        .accepted
        .checked_add(bound.rejected)
        .and_then(|records| records.checked_add(bound.imported_provenance_records))
        .ok_or(Error::NumericOverflow)?;
    let retained_state_bytes = checked_memory_sum([
        requested_vec_bytes_for_len::<ProvenanceDecision>(bound.candidate_events)?,
        requested_vec_bytes_for_len::<ProvenanceRecord>(provenance_records)?,
        bound.provenance_bytes,
        requested_btree_bytes_for_len::<CandidateIdentity, ProvenanceDecisionHandle>(
            bound.candidate_events,
        )?,
        requested_btree_bytes_for_len::<WorldCellKey, ()>(ancestor_references)?,
        bound.diagnostic_metadata_bytes,
    ])?;
    bound.memory_bytes = bound_add(
        "memory bytes",
        bound.memory_bytes,
        retained_state_bytes,
        graph.limits.max_memory_bytes,
    )?;
    for group in plan.groups.iter().filter(|group| {
        group.domain == GraphExecutionDomain::SlangCompute
            && group
                .nodes
                .iter()
                .any(|node| executed_nodes.contains(&node.address))
    }) {
        if group
            .nodes
            .iter()
            .any(|node| !executed_nodes.contains(&node.address))
        {
            return Err(Error::GraphDocument {
                path: "graph.symbolicBound.gpuGroup".to_owned(),
                reason: "resident group crosses the active symbolic spatial boundary".to_owned(),
            });
        }
        let group_metadata = group.nodes.iter().try_fold(
            checked_memory_sum([
                requested_vec_bytes_for_len::<GpuGroupEvaluationDiagnostic>(1)?,
                requested_vec_bytes_for_len::<GraphNodeAddress>(group.nodes.len() as u64)?,
            ])?,
            |total, node| {
                total
                    .checked_add(requested_vec_bytes_for_len::<u128>(
                        node.address.module_path.len() as u64,
                    )?)
                    .ok_or(Error::NumericOverflow)
            },
        )?;
        bound.diagnostic_metadata_bytes = bound_add(
            "memory bytes",
            bound.diagnostic_metadata_bytes,
            group_metadata,
            graph.limits.max_memory_bytes,
        )?;
        let invocations = group
            .inputs
            .iter()
            .filter_map(|input| values_by_pin.get(&input.pin))
            .map(|value| value.items)
            .max()
            .unwrap_or(0);
        if invocations == 0 {
            continue;
        }
        let scalar_inputs = group
            .inputs
            .iter()
            .filter(|input| input.domain == GraphDomain::ScalarField)
            .count() as u64;
        let synthetic_inputs = group.nodes.iter().try_fold(1_u64, |total, node| {
            let extra = match node.operator {
                GraphOperator::Noise => 11,
                GraphOperator::Gradient => 6,
                _ => 0,
            };
            total.checked_add(extra).ok_or(Error::NumericOverflow)
        })?;
        let input_count = scalar_inputs
            .checked_add(synthetic_inputs)
            .ok_or(Error::NumericOverflow)?;
        let curve_instructions = group
            .nodes
            .iter()
            .filter(|node| node.operator == GraphOperator::Curve)
            .count() as u64;
        let allocation_shape = ResidentGroupAllocationShape {
            invocations,
            inputs: input_count,
            instructions: group.nodes.len() as u64,
            curve_instructions,
            curve_points: curve_instructions
                .checked_mul(GRAPH_GPU_MAX_CURVE_POINTS as u64)
                .ok_or(Error::NumericOverflow)?,
            external_inputs: group.inputs.len() as u64,
            external_pin_bytes: group.inputs.iter().try_fold(0_u64, |total, input| {
                total
                    .checked_add(input.pin.pin.len() as u64)
                    .ok_or(Error::NumericOverflow)
            })?,
            output_entries: group.outputs.len() as u64,
            output_pin_bytes: group.outputs.iter().try_fold(0_u64, |total, output| {
                total
                    .checked_add(output.pin.pin.len() as u64)
                    .ok_or(Error::NumericOverflow)
            })?,
            noise_nodes: group
                .nodes
                .iter()
                .filter(|node| node.operator == GraphOperator::Noise)
                .count() as u64,
            gradient_nodes: group
                .nodes
                .iter()
                .filter(|node| node.operator == GraphOperator::Gradient)
                .count() as u64,
            candidate_output: group
                .outputs
                .iter()
                .any(|output| output.domain == GraphDomain::Candidates),
            scalar_output: group
                .outputs
                .iter()
                .any(|output| output.domain == GraphDomain::ScalarField),
        };
        bound.memory_bytes = bound_add(
            "memory bytes",
            bound.memory_bytes,
            resident_group_scratch_bytes(allocation_shape)?,
            graph.limits.max_memory_bytes,
        )?;
        let program_words = (GRAPH_GPU_PROGRAM_HEADER_WORDS as u64)
            .checked_add(input_count)
            .and_then(|words| {
                words.checked_add((group.nodes.len() as u64) * (GRAPH_GPU_INSTRUCTION_WORDS as u64))
            })
            .ok_or(Error::NumericOverflow)?;
        let invocation_words = bound_mul(
            "transfer bytes",
            invocations,
            input_count
                .checked_mul(GRAPH_GPU_INVOCATION_WORDS as u64)
                .ok_or(Error::NumericOverflow)?,
            graph.limits.max_transfer_bytes / 4,
        )?;
        let output_words = bound_mul(
            "transfer bytes",
            invocations,
            GRAPH_GPU_OUTPUT_WORDS as u64,
            graph.limits.max_transfer_bytes / 4,
        )?;
        let words = bound_add(
            "transfer bytes",
            bound_add(
                "transfer bytes",
                program_words,
                invocation_words,
                graph.limits.max_transfer_bytes / 4,
            )?,
            output_words,
            graph.limits.max_transfer_bytes / 4,
        )?;
        let bytes = bound_mul("transfer bytes", words, 4, graph.limits.max_transfer_bytes)?;
        bound.transfer_bytes = bound_add(
            "transfer bytes",
            bound.transfer_bytes,
            bytes,
            graph.limits.max_transfer_bytes,
        )?;
    }
    let materialized_outputs = scope
        .current_global_stage()
        .map_or_else(BTreeMap::new, |stage| {
            stage
                .output_pins
                .iter()
                .filter_map(|pin| {
                    values_by_pin
                        .get(pin)
                        .copied()
                        .map(|value| (pin.clone(), value))
                })
                .collect()
        });
    let rejection_history_bytes = requested_vec_bytes_for_len::<RejectedCandidate>(bound.rejected)?;
    let terminal_value_bytes = public_outputs
        .values()
        .filter(|value| {
            matches!(
                value.domain,
                Some(GraphDomain::MicroField | GraphDomain::Diagnostics)
            )
        })
        .try_fold(0_u64, |total, value| {
            bound_add(
                "memory bytes",
                total,
                value.bytes,
                graph.limits.max_memory_bytes,
            )
        })?;
    let public_result_bytes = checked_memory_sum([
        PlantPointColumns::requested_memory_bytes_for_rows(bound.accepted)?,
        terminal_value_bytes,
        bound.published_input_bytes,
        requested_vec_bytes_for_len::<QuantizedSurfaceProjectionTile>(bound.published_input_tiles)?,
        requested_vec_bytes_for_len::<QuantizedSurfaceFieldQueryTile>(bound.published_input_tiles)?,
        requested_vec_bytes_for_len::<WorldCellKey>(ancestor_references)?,
        requested_vec_bytes_for_len::<ProvenanceDecision>(bound.candidate_events)?,
        requested_vec_bytes_for_len::<ProvenanceRecord>(provenance_records)?,
        bound.provenance_bytes,
        rejection_history_bytes,
        bound.diagnostic_metadata_bytes,
    ])?;
    check_limit(
        "memory bytes",
        public_result_bytes,
        graph.limits.max_memory_bytes,
    )?;
    let materialized_bytes = materialized_outputs
        .values()
        .try_fold(0_u64, |total, value| {
            bound_add(
                "memory bytes",
                total,
                value.bytes,
                graph.limits.max_memory_bytes,
            )
        })?;
    let candidate_decision_bytes = requested_btree_bytes_for_len::<
        CandidateIdentity,
        ProvenanceDecisionHandle,
    >(bound.candidate_events)?;
    let materialized_key_bytes = materialized_outputs.keys().try_fold(0_u64, |total, pin| {
        total
            .checked_add(qualified_graph_pin_memory(pin)?)
            .ok_or(Error::NumericOverflow)
    })?;
    let materialized_container_bytes = checked_memory_sum([
        requested_btree_bytes_for_len::<QualifiedGraphPin, GraphValue>(
            materialized_outputs.len() as u64
        )?,
        materialized_key_bytes,
    ])?;
    bound.memory_bytes = bound_add(
        "memory bytes",
        bound.memory_bytes,
        checked_memory_sum([materialized_bytes, materialized_container_bytes])?,
        graph.limits.max_memory_bytes,
    )?;
    let cloned_provenance_bytes = checked_memory_sum([
        requested_vec_bytes_for_len::<ProvenanceDecision>(bound.candidate_events)?,
        requested_vec_bytes_for_len::<ProvenanceRecord>(provenance_records)?,
        bound.provenance_bytes,
    ])?;
    let global_tile_bytes = [
        public_result_bytes,
        candidate_decision_bytes,
        materialized_container_bytes,
        cloned_provenance_bytes,
    ]
    .into_iter()
    .try_fold(materialized_bytes, |total, bytes| {
        bound_add("memory bytes", total, bytes, graph.limits.max_memory_bytes)
    })?;
    Ok(SymbolicPlannedEvaluation {
        bound,
        materialized_outputs,
        public_result_bytes,
        global_tile_bytes,
        ancestor_references,
    })
}
