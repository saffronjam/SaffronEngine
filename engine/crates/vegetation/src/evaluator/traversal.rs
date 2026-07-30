//! Allocation bounds derived from the compiled execution plan.

use super::*;

use std::collections::BTreeMap;

use saffron_spatial::{MAX_HIERARCHY_LEVEL, WorldCellKey, world_cell_count_covering_bounds};

use crate::graph::{CompiledDemandSlice, CompiledDemandUnitSlice};
use crate::memory::{
    checked_memory_sum, requested_btree_bytes, requested_vec_bytes, requested_vec_bytes_for_len,
};
use crate::{
    CompiledBiomeGraph, CompiledGlobalStage, CompiledGraphNode, CompiledGraphUnit, Error,
    GraphDomain, GraphExecutionDomain, GraphExecutionPlan, GraphNodeAddress, GraphOperator,
    QualifiedGraphPin, Result,
};

struct TraversalAllocationSummary {
    emitted_pins: usize,
    emitted_path_words: usize,
    emitted_path_allocations: usize,
    emitted_name_bytes: usize,
    pub(super) executed_nodes: usize,
    executed_path_words: usize,
    executed_path_allocations: usize,
    ancestor_candidate_levels: [bool; HIERARCHY_LEVEL_COUNT],
}

impl Default for TraversalAllocationSummary {
    fn default() -> Self {
        Self {
            emitted_pins: 0,
            emitted_path_words: 0,
            emitted_path_allocations: 0,
            emitted_name_bytes: 0,
            executed_nodes: 0,
            executed_path_words: 0,
            executed_path_allocations: 0,
            ancestor_candidate_levels: [false; HIERARCHY_LEVEL_COUNT],
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct NodeOutputDemand<'a> {
    pub(super) demand: &'a CompiledDemandSlice,
    pub(super) node: &'a CompiledGraphNode,
}

impl<'a> NodeOutputDemand<'a> {
    pub(super) const fn new(demand: &'a CompiledDemandSlice, node: &'a CompiledGraphNode) -> Self {
        Self { demand, node }
    }

    pub(super) fn contains(self, output: &str) -> bool {
        self.demand.output_pins.iter().any(|pin| {
            pin.node.node == self.node.definition.guid
                && pin.node.module_path == self.node.debug_symbol.module_path
                && pin.pin == output
        })
    }
}

pub(super) struct NodeOutputBuilder<'a, T> {
    pub(super) demand: NodeOutputDemand<'a>,
    pub(super) outputs: BTreeMap<String, T>,
}

impl<'a, T> NodeOutputBuilder<'a, T> {
    pub(super) fn new(demand: NodeOutputDemand<'a>) -> Self {
        Self {
            demand,
            outputs: BTreeMap::new(),
        }
    }

    pub(super) fn insert(&mut self, output: String, value: T) -> Result<()> {
        if !self
            .demand
            .node
            .outputs
            .iter()
            .any(|schema| schema.name == output)
        {
            return Err(Error::GraphDocument {
                path: self.demand.node.debug_symbol.label.clone(),
                reason: format!("operator emitted unknown pin '{output}'"),
            });
        }
        if !self.demand.contains(&output) {
            return Err(Error::GraphDocument {
                path: self.demand.node.debug_symbol.label.clone(),
                reason: format!("operator emitted undemanded pin '{output}'"),
            });
        }
        if self.outputs.insert(output.clone(), value).is_some() {
            return Err(Error::GraphDocument {
                path: self.demand.node.debug_symbol.label.clone(),
                reason: format!("operator emitted duplicate pin '{output}'"),
            });
        }
        Ok(())
    }

    pub(super) fn extend(&mut self, outputs: BTreeMap<String, T>) -> Result<()> {
        for (output, value) in outputs {
            self.insert(output, value)?;
        }
        Ok(())
    }

    pub(super) fn finish(self) -> Result<BTreeMap<String, T>> {
        for output in self
            .demand
            .node
            .outputs
            .iter()
            .filter(|output| self.demand.contains(&output.name))
        {
            if !self.outputs.contains_key(&output.name) {
                return Err(Error::GraphDocument {
                    path: self.demand.node.debug_symbol.label.clone(),
                    reason: format!("operator omitted demanded pin '{}'", output.name),
                });
            }
        }
        Ok(self.outputs)
    }
}

fn traversal_allocation_summary(
    graph: &CompiledBiomeGraph,
    demand: &CompiledDemandSlice,
) -> Result<TraversalAllocationSummary> {
    fn visit(
        unit: &CompiledGraphUnit,
        demand: &CompiledDemandSlice,
        summary: &mut TraversalAllocationSummary,
    ) -> Result<()> {
        let module_path = unit
            .nodes
            .first()
            .map_or(&[][..], |node| node.debug_symbol.module_path.as_slice());
        let unit_demand = demand
            .unit(module_path)
            .ok_or_else(|| Error::GraphDocument {
                path: "graph.demandPlan".to_owned(),
                reason: "live unit has no demand slice".to_owned(),
            })?;
        for node in unit
            .nodes
            .iter()
            .filter(|node| unit_demand.contains_node(node.definition.guid))
        {
            if demand.estimates.keys().any(|pin| {
                pin.node.node == node.definition.guid
                    && pin.node.module_path == node.debug_symbol.module_path
                    && node.outputs.iter().any(|output| {
                        output.name == pin.pin
                            && (output.domain == GraphDomain::Candidates
                                || node.definition.operator == GraphOperator::MacroOutput)
                    })
            }) {
                summary.ancestor_candidate_levels[usize::from(node.definition.spatial.level())] =
                    true;
            }
            if demand.executes_node(&node.address()) {
                summary.executed_nodes = summary
                    .executed_nodes
                    .checked_add(1)
                    .ok_or(Error::NumericOverflow)?;
                summary.executed_path_words = summary
                    .executed_path_words
                    .checked_add(node.debug_symbol.module_path.len())
                    .ok_or(Error::NumericOverflow)?;
                summary.executed_path_allocations = summary
                    .executed_path_allocations
                    .checked_add(usize::from(!node.debug_symbol.module_path.is_empty()))
                    .ok_or(Error::NumericOverflow)?;
            }
            for output in node
                .outputs
                .iter()
                .filter(|output| NodeOutputDemand::new(demand, node).contains(&output.name))
            {
                summary.emitted_pins = summary
                    .emitted_pins
                    .checked_add(1)
                    .ok_or(Error::NumericOverflow)?;
                summary.emitted_path_words = summary
                    .emitted_path_words
                    .checked_add(node.debug_symbol.module_path.len())
                    .ok_or(Error::NumericOverflow)?;
                summary.emitted_path_allocations = summary
                    .emitted_path_allocations
                    .checked_add(usize::from(!node.debug_symbol.module_path.is_empty()))
                    .ok_or(Error::NumericOverflow)?;
                summary.emitted_name_bytes = summary
                    .emitted_name_bytes
                    .checked_add(output.name.len())
                    .ok_or(Error::NumericOverflow)?;
            }
            if let Some(module) = node.module.as_deref()
                && find_child_demand_unit(demand, node).is_some()
            {
                visit(module, demand, summary)?;
            }
        }
        Ok(())
    }

    let mut summary = TraversalAllocationSummary::default();
    visit(&graph.root, demand, &mut summary)?;
    Ok(summary)
}

fn qualified_output_domain(
    unit: &CompiledGraphUnit,
    pin: &QualifiedGraphPin,
) -> Option<GraphDomain> {
    for node in &unit.nodes {
        if node.definition.guid == pin.node.node
            && node.debug_symbol.module_path == pin.node.module_path
        {
            return node
                .outputs
                .iter()
                .find(|output| output.name == pin.pin)
                .map(|output| output.domain);
        }
        if let Some(domain) = node
            .module
            .as_deref()
            .and_then(|module| qualified_output_domain(module, pin))
        {
            return Some(domain);
        }
    }
    None
}

pub(super) fn global_stage_has_macro_output(
    graph: &CompiledBiomeGraph,
    stage: &CompiledGlobalStage,
) -> Result<bool> {
    let mut has_macro_output = false;
    for pin in &stage.output_pins {
        let domain =
            qualified_output_domain(&graph.root, pin).ok_or_else(|| Error::GraphDocument {
                path: "graph.spatialPlan".to_owned(),
                reason: "global-stage output pin is absent from the compiled graph".to_owned(),
            })?;
        has_macro_output |= domain == GraphDomain::MacroPoints;
    }
    Ok(has_macro_output)
}

pub(super) fn ancestor_reference_upper_bound(
    graph: &CompiledBiomeGraph,
    inputs: &GraphEvaluationInputs,
    include_global_stage_owners: bool,
) -> Result<u64> {
    let summary = traversal_allocation_summary(graph, graph.demand_plan().execution_slice())?;
    let mut global_macro_levels = [false; HIERARCHY_LEVEL_COUNT];
    if include_global_stage_owners {
        for stage in graph.spatial_plan().global_stages() {
            if global_stage_has_macro_output(graph, stage)? {
                global_macro_levels[usize::from(stage.owner_level)] = true;
            }
        }
    }

    let mut references = 0_u64;
    for level in 0..=MAX_HIERARCHY_LEVEL {
        if global_macro_levels[usize::from(level)] {
            references = references
                .checked_add(world_cell_count_covering_bounds(
                    inputs.read_bounds,
                    level,
                    graph.limits.max_global_stage_tiles,
                )?)
                .ok_or(Error::NumericOverflow)?;
        } else if level > inputs.output_cell.level()
            && summary.ancestor_candidate_levels[usize::from(level)]
        {
            references = references.checked_add(1).ok_or(Error::NumericOverflow)?;
        }
    }
    Ok(references)
}

fn separately_allocated_storage<T>(items: usize, allocations: usize) -> Result<u64> {
    disjoint_vec_bytes::<T>(
        u64::try_from(items).map_err(|_| Error::NumericOverflow)?,
        u64::try_from(allocations).map_err(|_| Error::NumericOverflow)?,
    )
}

fn named_btree_allocation<K, V>(entries: usize, name_bytes: usize) -> Result<u64> {
    checked_memory_sum([
        requested_btree_bytes::<K, V>(entries)?,
        separately_allocated_storage::<u8>(name_bytes, entries)?,
    ])
}

fn unit_demand_slice<'a>(
    unit: &CompiledGraphUnit,
    demand: &'a CompiledDemandSlice,
) -> Result<&'a CompiledDemandUnitSlice> {
    let module_path = unit
        .nodes
        .first()
        .map_or(&[][..], |node| node.debug_symbol.module_path.as_slice());
    demand
        .unit(module_path)
        .ok_or_else(|| Error::GraphDocument {
            path: "graph.demandPlan".to_owned(),
            reason: "live unit has no traversal demand slice".to_owned(),
        })
}

fn emitted_output_stats(
    node: &CompiledGraphNode,
    demand: &CompiledDemandSlice,
) -> Result<(usize, usize)> {
    node.outputs
        .iter()
        .filter(|output| NodeOutputDemand::new(demand, node).contains(&output.name))
        .try_fold((0_usize, 0_usize), |(entries, bytes), output| {
            Ok((
                entries.checked_add(1).ok_or(Error::NumericOverflow)?,
                bytes
                    .checked_add(output.name.len())
                    .ok_or(Error::NumericOverflow)?,
            ))
        })
}

fn retained_output_stats(
    unit: &CompiledGraphUnit,
    unit_demand: &CompiledDemandUnitSlice,
    demand: &CompiledDemandSlice,
) -> Result<(usize, usize)> {
    unit.nodes
        .iter()
        .filter(|node| unit_demand.contains_node(node.definition.guid))
        .flat_map(|node| {
            node.outputs.iter().filter(move |output| {
                NodeOutputDemand::new(demand, node).contains(&output.name)
                    && (unit_demand.edges.iter().any(|edge| {
                        edge.from_node == node.definition.guid && edge.from_pin == output.name
                    }) || unit.outputs.iter().any(|unit_output| {
                        unit_demand.outputs.contains(&unit_output.name)
                            && unit_output.node == node.definition.guid
                            && unit_output.pin == output.name
                    }))
            })
        })
        .try_fold((0_usize, 0_usize), |(entries, bytes), output| {
            Ok((
                entries.checked_add(1).ok_or(Error::NumericOverflow)?,
                bytes
                    .checked_add(output.name.len())
                    .ok_or(Error::NumericOverflow)?,
            ))
        })
}

fn node_input_stats(
    node: &CompiledGraphNode,
    unit_demand: &CompiledDemandUnitSlice,
) -> Result<(usize, usize)> {
    unit_demand
        .edges
        .iter()
        .filter(|edge| edge.to_node == node.definition.guid)
        .try_fold((0_usize, 0_usize), |(entries, bytes), edge| {
            Ok((
                entries.checked_add(1).ok_or(Error::NumericOverflow)?,
                bytes
                    .checked_add(edge.to_pin.len())
                    .ok_or(Error::NumericOverflow)?,
            ))
        })
}

fn unit_output_stats(
    unit: &CompiledGraphUnit,
    unit_demand: &CompiledDemandUnitSlice,
) -> Result<(usize, usize)> {
    unit.outputs
        .iter()
        .filter(|output| unit_demand.outputs.contains(&output.name))
        .try_fold((0_usize, 0_usize), |(entries, bytes), output| {
            Ok((
                entries.checked_add(1).ok_or(Error::NumericOverflow)?,
                bytes
                    .checked_add(output.name.len())
                    .ok_or(Error::NumericOverflow)?,
            ))
        })
}

fn incoming_allocation_bytes(
    unit: &CompiledGraphUnit,
    unit_demand: &CompiledDemandUnitSlice,
) -> Result<u64> {
    let nested = unit
        .nodes
        .iter()
        .filter(|node| unit_demand.contains_node(node.definition.guid))
        .try_fold(0_u64, |total, node| {
            let edges = unit_demand
                .edges
                .iter()
                .filter(|edge| edge.to_node == node.definition.guid)
                .count();
            total
                .checked_add(requested_vec_bytes::<&crate::GraphEdge>(edges)?)
                .ok_or(Error::NumericOverflow)
        })?;
    checked_memory_sum([
        requested_btree_bytes::<u128, Vec<&crate::GraphEdge>>(unit_demand.nodes.len())?,
        nested,
    ])
}

pub(super) fn runtime_traversal_allocation_bound(
    graph: &CompiledBiomeGraph,
    demand: &CompiledDemandSlice,
    plan: &GraphExecutionPlan,
    ancestor_references: u64,
) -> Result<u64> {
    fn visit(
        unit: &CompiledGraphUnit,
        demand: &CompiledDemandSlice,
        plan: &GraphExecutionPlan,
    ) -> Result<u64> {
        let unit_demand = unit_demand_slice(unit, demand)?;
        let (retained_entries, retained_name_bytes) =
            retained_output_stats(unit, unit_demand, demand)?;
        let resident_nodes = unit
            .nodes
            .iter()
            .filter(|node| {
                unit_demand.contains_node(node.definition.guid)
                    && demand.executes_node(&node.address())
                    && plan.domain_for(&node.debug_symbol.module_path, node.definition.guid)
                        == Some(GraphExecutionDomain::SlangCompute)
            })
            .count();
        let base = checked_memory_sum([
            incoming_allocation_bytes(unit, unit_demand)?,
            named_btree_allocation::<(u128, String), u64>(retained_entries, retained_name_bytes)?,
            named_btree_allocation::<(u128, String), GraphValue>(
                retained_entries,
                retained_name_bytes,
            )?,
            requested_btree_bytes::<u128, ()>(resident_nodes)?,
        ])?;
        let (unit_output_entries, unit_output_name_bytes) = unit_output_stats(unit, unit_demand)?;
        let mut peak = named_btree_allocation::<String, GraphValue>(
            unit_output_entries,
            unit_output_name_bytes,
        )?;
        for node in unit
            .nodes
            .iter()
            .filter(|node| unit_demand.contains_node(node.definition.guid))
        {
            let (input_entries, input_name_bytes) = node_input_stats(node, unit_demand)?;
            let inputs =
                named_btree_allocation::<String, GraphValue>(input_entries, input_name_bytes)?;
            let node_peak = if demand.executes_node(&node.address())
                && plan.domain_for(&node.debug_symbol.module_path, node.definition.guid)
                    == Some(GraphExecutionDomain::SlangCompute)
            {
                let group =
                    plan.group_for(&node.address())
                        .ok_or_else(|| Error::GraphDocument {
                            path: node.debug_symbol.label.clone(),
                            reason: "resident node has no execution group".to_owned(),
                        })?;
                let group_contains = |guid| {
                    group.nodes.iter().any(|member| {
                        member.address.module_path == node.debug_symbol.module_path
                            && member.address.node == guid
                    })
                };
                let (boundary_entries, boundary_name_bytes) = unit_demand
                    .edges
                    .iter()
                    .filter(|edge| !group_contains(edge.from_node) && group_contains(edge.to_node))
                    .try_fold((0_usize, 0_usize), |(entries, bytes), edge| {
                        Ok::<_, Error>((
                            entries.checked_add(1).ok_or(Error::NumericOverflow)?,
                            bytes
                                .checked_add(edge.from_pin.len())
                                .ok_or(Error::NumericOverflow)?,
                        ))
                    })?;
                let (output_entries, output_name_bytes) = group
                    .outputs
                    .iter()
                    .filter(|output| output.pin.node.module_path == node.debug_symbol.module_path)
                    .try_fold((0_usize, 0_usize), |(entries, bytes), output| {
                        Ok::<_, Error>((
                            entries.checked_add(1).ok_or(Error::NumericOverflow)?,
                            bytes
                                .checked_add(output.pin.pin.len())
                                .ok_or(Error::NumericOverflow)?,
                        ))
                    })?;
                checked_memory_sum([
                    named_btree_allocation::<(u128, String), GraphValue>(
                        boundary_entries,
                        boundary_name_bytes,
                    )?,
                    named_btree_allocation::<(u128, String), GraphValue>(
                        output_entries,
                        output_name_bytes,
                    )?,
                ])?
            } else if let Some(module) = node.module.as_deref()
                && find_child_demand_unit(demand, node).is_some()
                && demand.executes_node(&node.address())
            {
                inputs
                    .checked_add(visit(module, demand, plan)?)
                    .ok_or(Error::NumericOverflow)?
            } else {
                let (output_entries, output_name_bytes) = emitted_output_stats(node, demand)?;
                checked_memory_sum([
                    inputs,
                    named_btree_allocation::<String, GraphValue>(
                        output_entries,
                        output_name_bytes,
                    )?,
                ])?
            };
            peak = peak.max(node_peak);
        }
        base.checked_add(peak).ok_or(Error::NumericOverflow)
    }

    checked_memory_sum([
        visit(&graph.root, demand, plan)?,
        requested_vec_bytes_for_len::<WorldCellKey>(ancestor_references)?,
    ])
}

pub(super) fn symbolic_traversal_allocation_bound(
    graph: &CompiledBiomeGraph,
    demand: &CompiledDemandSlice,
) -> Result<u64> {
    fn visit(unit: &CompiledGraphUnit, demand: &CompiledDemandSlice) -> Result<u64> {
        let unit_demand = unit_demand_slice(unit, demand)?;
        let (emitted_entries, emitted_name_bytes) = unit
            .nodes
            .iter()
            .filter(|node| unit_demand.contains_node(node.definition.guid))
            .try_fold((0_usize, 0_usize), |(entries, bytes), node| {
                let (node_entries, node_bytes) = emitted_output_stats(node, demand)?;
                Ok::<_, Error>((
                    entries
                        .checked_add(node_entries)
                        .ok_or(Error::NumericOverflow)?,
                    bytes
                        .checked_add(node_bytes)
                        .ok_or(Error::NumericOverflow)?,
                ))
            })?;
        let base = checked_memory_sum([
            incoming_allocation_bytes(unit, unit_demand)?,
            named_btree_allocation::<(u128, String), SymbolicValueBound>(
                emitted_entries,
                emitted_name_bytes,
            )?,
        ])?;
        let (unit_output_entries, unit_output_name_bytes) = unit_output_stats(unit, unit_demand)?;
        let mut peak = named_btree_allocation::<String, SymbolicValueBound>(
            unit_output_entries,
            unit_output_name_bytes,
        )?;
        for node in unit
            .nodes
            .iter()
            .filter(|node| unit_demand.contains_node(node.definition.guid))
        {
            let (output_entries, output_name_bytes) = emitted_output_stats(node, demand)?;
            let outputs = named_btree_allocation::<String, SymbolicValueBound>(
                output_entries,
                output_name_bytes,
            )?;
            let node_peak = if let Some(module) = node.module.as_deref()
                && find_child_demand_unit(demand, node).is_some()
                && demand.executes_node(&node.address())
            {
                let (input_entries, input_name_bytes) = node_input_stats(node, unit_demand)?;
                checked_memory_sum([
                    named_btree_allocation::<String, SymbolicValueBound>(
                        input_entries,
                        input_name_bytes,
                    )?,
                    visit(module, demand)?,
                ])?
            } else {
                outputs
            };
            peak = peak.max(node_peak);
        }
        base.checked_add(peak).ok_or(Error::NumericOverflow)
    }

    let summary = traversal_allocation_summary(graph, demand)?;
    checked_memory_sum([
        visit(&graph.root, demand)?,
        requested_btree_bytes::<QualifiedGraphPin, SymbolicValueBound>(summary.emitted_pins)?,
        separately_allocated_storage::<u128>(
            summary.emitted_path_words,
            summary.emitted_path_allocations,
        )?,
        separately_allocated_storage::<u8>(summary.emitted_name_bytes, summary.emitted_pins)?,
        requested_btree_bytes::<GraphNodeAddress, ()>(summary.executed_nodes)?,
        separately_allocated_storage::<u128>(
            summary.executed_path_words,
            summary.executed_path_allocations,
        )?,
    ])
}

pub(super) fn bound_add(resource: &'static str, left: u64, right: u64, limit: u64) -> Result<u64> {
    let requested = u128::from(left) + u128::from(right);
    if requested > u128::from(limit) {
        return Err(Error::GraphLimit {
            resource,
            requested: u64::try_from(requested).unwrap_or(u64::MAX),
            limit,
        });
    }
    Ok(requested as u64)
}

pub(super) fn bound_mul(resource: &'static str, left: u64, right: u64, limit: u64) -> Result<u64> {
    let requested = u128::from(left) * u128::from(right);
    if requested > u128::from(limit) {
        return Err(Error::GraphLimit {
            resource,
            requested: u64::try_from(requested).unwrap_or(u64::MAX),
            limit,
        });
    }
    Ok(requested as u64)
}
