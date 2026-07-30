//! Execution-domain grouping over the compiled graph IR.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use crate::graph::{CompiledDemandPlan, CompiledDemandSlice, CompiledDemandUnitSlice};
use crate::memory::reserve_exact;
use crate::{
    CompiledBiomeGraph, CompiledGraphNode, CompiledGraphUnit, Error, GpuExecutionProfile,
    GpuQualificationRegistry, GraphAuthority, GraphCancellationToken, GraphDomain, GraphEdge,
    GraphNodeAddress, GraphOperator, GraphParameterValue, QualifiedGraphPin, Result,
};

use super::*;

/// Runtime executor for one resident program over an ordered candidate batch.
pub trait GraphComputeExecutor: Send + Sync {
    /// Exact Vulkan profile against which qualification evidence was captured.
    fn profile(&self) -> &GpuExecutionProfile;
    /// Complete-program evidence admitted for this profile.
    fn qualifications(&self) -> &GpuQualificationRegistry;
    /// Executes one program without CPU round-trips between its instructions.
    fn execute_program(
        &self,
        program: &GraphGpuProgram,
        invocation_batch: &GraphGpuInvocationBatch,
        cancellation: &GraphCancellationToken,
        deadline: Instant,
    ) -> Result<Vec<GraphGpuOutput>>;
}

/// Execution domain chosen for one group without changing graph semantics.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GraphExecutionDomain {
    /// Single-thread canonical Rust interpreter.
    ReferenceCpu,
    /// Cell-parallel canonical Rust interpreter.
    ParallelCpu,
    /// Resident Slang program on a qualified path.
    SlangCompute,
}

/// Stable address of one compiled node, including nested module calls.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphExecutionNode {
    /// Fully qualified node address shared with spatial planning and provenance.
    pub address: GraphNodeAddress,
    pub operator: GraphOperator,
    pub authority: GraphAuthority,
    /// Hash of the complete migrated node document.
    pub node_hash: [u8; 32],
}

/// One exact pin crossing an execution-domain boundary.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct GraphExecutionBoundary {
    /// Fully qualified source or destination pin.
    pub pin: QualifiedGraphPin,
    /// Typed value domain crossing the boundary.
    pub domain: GraphDomain,
}

/// One connected execution group and its explicit transfer boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphExecutionGroup {
    /// Actual execution domain.
    pub domain: GraphExecutionDomain,
    /// Canonical nodes in this connected group.
    pub nodes: Vec<GraphExecutionNode>,
    /// External pins read by this group.
    pub inputs: Vec<GraphExecutionBoundary>,
    /// Pins retained for consumers outside this group.
    pub outputs: Vec<GraphExecutionBoundary>,
    /// Predicted bytes crossing into this group.
    pub transfer_in_bytes: u64,
    /// Predicted retained bytes for the group.
    pub predicted_output_bytes: u64,
}

/// One scheduling plan over the single compiled graph IR.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GraphExecutionPlan {
    /// Connected domain groups in canonical topological/module order.
    pub groups: Vec<GraphExecutionGroup>,
    /// Total predicted transfer traffic.
    pub predicted_transfer_bytes: u64,
}

impl GraphExecutionPlan {
    /// Returns the selected domain for one fully qualified compiled node.
    #[must_use]
    pub fn domain_for(&self, module_path: &[u128], node: u128) -> Option<GraphExecutionDomain> {
        let address = GraphNodeAddress {
            module_path: module_path.to_vec(),
            node,
        };
        self.groups.iter().find_map(|group| {
            group
                .nodes
                .iter()
                .any(|candidate| candidate.address == address)
                .then_some(group.domain)
        })
    }

    /// Returns the complete connected group containing one node.
    #[must_use]
    pub fn group_for(&self, address: &GraphNodeAddress) -> Option<&GraphExecutionGroup> {
        self.groups
            .iter()
            .find(|group| group.nodes.iter().any(|node| &node.address == address))
    }
}

/// Exact device evidence available to execution-plan selection.
#[derive(Clone, Copy)]
pub struct GraphGpuScheduling<'a> {
    /// Active runtime device profile.
    pub profile: &'a GpuExecutionProfile,
    /// Qualification evidence for authoritative resident nodes.
    pub qualifications: &'a GpuQualificationRegistry,
}

/// Builds connected CPU/resident-GPU groups; incompatible topology remains split.
pub fn build_execution_plan(
    graph: &CompiledBiomeGraph,
    parallel_cpu: bool,
    gpu: Option<GraphGpuScheduling<'_>>,
) -> Result<GraphExecutionPlan> {
    let demand_plan = graph.demand_plan();
    let demand = demand_plan.execution_slice();
    fn compiled_node_count(
        unit: &CompiledGraphUnit,
        demand: &CompiledDemandSlice,
    ) -> Result<usize> {
        let unit_demand = demand
            .unit(
                unit.nodes
                    .first()
                    .map_or(&[][..], |node| node.debug_symbol.module_path.as_slice()),
            )
            .ok_or_else(|| program_error("schedule.demand", "compiled demand slice is missing"))?;
        demanded_nodes(unit, unit_demand).try_fold(unit_demand.nodes.len(), |total, node| {
            node.module.as_deref().map_or(Ok(total), |module| {
                total
                    .checked_add(compiled_node_count(module, demand)?)
                    .ok_or(Error::NumericOverflow)
            })
        })
    }

    let mut groups = Vec::new();
    reserve_exact(
        &mut groups,
        compiled_node_count(&graph.root, demand)?,
        "graph execution groups",
    )?;
    schedule_unit(
        &graph.root,
        demand_plan,
        demand,
        parallel_cpu,
        gpu,
        &mut groups,
    )?;
    let predicted_transfer_bytes = groups.iter().try_fold(0_u64, |total, group| {
        total
            .checked_add(group.transfer_in_bytes)
            .ok_or(Error::NumericOverflow)
    })?;
    Ok(GraphExecutionPlan {
        groups,
        predicted_transfer_bytes,
    })
}

fn schedule_unit(
    unit: &CompiledGraphUnit,
    demand_plan: &CompiledDemandPlan,
    demand: &CompiledDemandSlice,
    parallel_cpu: bool,
    gpu: Option<GraphGpuScheduling<'_>>,
    groups: &mut Vec<GraphExecutionGroup>,
) -> Result<()> {
    let module_path = unit
        .nodes
        .first()
        .map_or(&[][..], |node| node.debug_symbol.module_path.as_slice());
    let unit_demand = demand
        .unit(module_path)
        .ok_or_else(|| program_error("schedule.demand", "compiled demand slice is missing"))?;
    for node in demanded_nodes(unit, unit_demand) {
        if let Some(module) = node.module.as_deref() {
            schedule_unit(module, demand_plan, demand, parallel_cpu, gpu, groups)?;
        }
    }
    let nodes = demanded_nodes(unit, unit_demand)
        .map(|node| (node.definition.guid, node))
        .collect::<BTreeMap<_, _>>();
    let gpu_nodes = demanded_nodes(unit, unit_demand)
        .filter(|node| node_gpu_admitted(node, gpu))
        .map(|node| node.definition.guid)
        .collect::<BTreeSet<_>>();
    let mut components = Vec::new();
    reserve_exact(
        &mut components,
        gpu_nodes.len(),
        "GPU scheduling components",
    )?;
    for node in gpu_nodes.iter().copied() {
        let mut component = Vec::new();
        reserve_exact(&mut component, 1, "GPU scheduling component")?;
        component.push(node);
        components.push(component);
    }
    let mut component_by_node = components
        .iter()
        .enumerate()
        .map(|(index, component)| (component[0], index))
        .collect::<BTreeMap<_, _>>();
    loop {
        let mut merged = false;
        for edge in unit
            .edges
            .iter()
            .filter(|edge| unit_demand.contains_edge(edge))
        {
            if gpu_nodes.contains(&edge.from_node)
                && gpu_nodes.contains(&edge.to_node)
                && demand_plan.same_scope_membership(
                    &nodes[&edge.from_node].address(),
                    &nodes[&edge.to_node].address(),
                )
                && program_edge_compatible(nodes[&edge.from_node], nodes[&edge.to_node], edge)
            {
                let source = component_by_node[&edge.from_node];
                let destination = component_by_node[&edge.to_node];
                if source == destination {
                    continue;
                }
                let proposed_capacity = components[source]
                    .len()
                    .checked_add(components[destination].len())
                    .ok_or(Error::NumericOverflow)?;
                let mut proposed = Vec::new();
                reserve_exact(
                    &mut proposed,
                    proposed_capacity,
                    "GPU scheduling proposed component",
                )?;
                proposed.extend(unit.nodes.iter().map(|node| node.definition.guid).filter(
                    |node| {
                        components[source].contains(node) || components[destination].contains(node)
                    },
                ));
                if component_topology_supported(unit, unit_demand, &nodes, &proposed) {
                    components[source] = proposed;
                    components[destination].clear();
                    for node in &components[source] {
                        component_by_node.insert(*node, source);
                    }
                    merged = true;
                }
            }
        }
        if !merged {
            break;
        }
    }
    components.retain(|component| !component.is_empty());
    component_by_node.clear();
    for (index, component) in components.iter().enumerate() {
        for node in component {
            component_by_node.insert(*node, index);
        }
    }

    let mut emitted_components = BTreeSet::new();
    for node in demanded_nodes(unit, unit_demand) {
        let guid = node.definition.guid;
        if let Some(component) = component_by_node.get(&guid).copied() {
            if !emitted_components.insert(component) {
                continue;
            }
            groups.push(execution_group(
                unit,
                demand,
                unit_demand,
                &nodes,
                &components[component],
                GraphExecutionDomain::SlangCompute,
            )?);
        } else {
            let domain = if parallel_cpu {
                GraphExecutionDomain::ParallelCpu
            } else {
                GraphExecutionDomain::ReferenceCpu
            };
            let singleton = [guid];
            let group = execution_group(unit, demand, unit_demand, &nodes, &singleton, domain)?;
            if let Some(previous) = groups.last_mut().filter(|previous| {
                previous.domain == domain
                    && previous
                        .outputs
                        .iter()
                        .any(|output| group.inputs.iter().any(|input| input.pin == output.pin))
            }) {
                reserve_exact(
                    &mut previous.nodes,
                    group.nodes.len(),
                    "coalesced CPU execution group nodes",
                )?;
                previous.nodes.extend(group.nodes);
                previous.outputs = group.outputs;
                previous.predicted_output_bytes = previous
                    .predicted_output_bytes
                    .max(group.predicted_output_bytes);
            } else {
                groups.push(group);
            }
        }
    }
    Ok(())
}

fn demanded_nodes<'a>(
    unit: &'a CompiledGraphUnit,
    demand: &'a CompiledDemandUnitSlice,
) -> impl Iterator<Item = &'a CompiledGraphNode> {
    unit.nodes
        .iter()
        .filter(|node| demand.contains_node(node.definition.guid))
}

fn node_gpu_admitted(node: &CompiledGraphNode, gpu: Option<GraphGpuScheduling<'_>>) -> bool {
    let Some(gpu) = gpu else {
        return false;
    };
    if !node.capabilities.slang_compute || !node_program_compatible(node) {
        return false;
    }
    node.definition.authority == GraphAuthority::Cosmetic
        || (node.definition.authority == GraphAuthority::EquivalentGpu
            && gpu.qualifications.contains(
                node.definition.operator,
                node.definition.version,
                gpu.profile,
            ))
}

fn node_program_compatible(node: &CompiledGraphNode) -> bool {
    if GraphGpuOperator::from_graph(node.definition.operator).is_none() {
        return false;
    }
    if node.definition.operator == GraphOperator::Curve {
        return node
            .definition
            .parameter("curve")
            .and_then(|value| match value {
                GraphParameterValue::Curve(points) => Some(points.len()),
                _ => None,
            })
            .is_some_and(|count| (1..=GRAPH_GPU_MAX_CURVE_POINTS).contains(&count));
    }
    true
}

fn program_edge_compatible(
    source: &CompiledGraphNode,
    destination: &CompiledGraphNode,
    edge: &GraphEdge,
) -> bool {
    if source.definition.spatial != destination.definition.spatial {
        return false;
    }
    let source_domain = source
        .outputs
        .iter()
        .find(|pin| pin.name == edge.from_pin)
        .map(|pin| pin.domain);
    let destination_domain = destination
        .inputs
        .iter()
        .find(|pin| pin.name == edge.to_pin)
        .map(|pin| pin.domain);
    source_domain == destination_domain
        && source_domain.is_some_and(|domain| {
            matches!(domain, GraphDomain::ScalarField | GraphDomain::Candidates)
        })
}

fn component_topology_supported(
    unit: &CompiledGraphUnit,
    demand: &CompiledDemandUnitSlice,
    nodes: &BTreeMap<u128, &CompiledGraphNode>,
    component: &[u128],
) -> bool {
    let is_member = |node| component.contains(&node);
    if component.len() > GRAPH_GPU_MAX_INSTRUCTIONS
        || component
            .iter()
            .filter(|node| nodes[node].definition.operator == GraphOperator::FieldImportance)
            .count()
            > 1
    {
        return false;
    }
    let mut input_lineages = BTreeSet::new();
    let mut external_scalar_inputs = BTreeSet::new();
    let mut external_candidate_inputs = BTreeSet::new();
    let mut synthetic_inputs = 1_usize;
    let mut field_outputs = BTreeSet::new();
    let mut candidate_outputs = BTreeSet::new();
    for node in component {
        synthetic_inputs += match nodes[node].definition.operator {
            GraphOperator::Noise => 11,
            GraphOperator::Gradient => 6,
            _ => 0,
        };
    }
    for edge in unit.edges.iter().filter(|edge| demand.contains_edge(edge)) {
        if !is_member(edge.from_node) && is_member(edge.to_node) {
            let Some(pin) = nodes[&edge.from_node]
                .outputs
                .iter()
                .find(|pin| pin.name == edge.from_pin)
            else {
                return false;
            };
            match pin.domain {
                GraphDomain::ScalarField => {
                    external_scalar_inputs.insert((edge.from_node, edge.from_pin.as_str()));
                }
                GraphDomain::Candidates => {
                    external_candidate_inputs.insert((edge.from_node, edge.from_pin.as_str()));
                }
                _ => return false,
            }
            let Some(lineage) = nodes[&edge.from_node].output_lineage.get(&edge.from_pin) else {
                return false;
            };
            input_lineages.insert(lineage);
        }
        if is_member(edge.from_node) && !is_member(edge.to_node) {
            let Some(pin) = nodes[&edge.from_node]
                .outputs
                .iter()
                .find(|pin| pin.name == edge.from_pin)
            else {
                return false;
            };
            match pin.domain {
                GraphDomain::ScalarField => {
                    field_outputs.insert((edge.from_node, edge.from_pin.as_str()));
                }
                GraphDomain::Candidates => {
                    candidate_outputs.insert((edge.from_node, edge.from_pin.as_str()));
                }
                _ => return false,
            }
        }
    }
    for output in unit
        .outputs
        .iter()
        .filter(|output| demand.outputs.contains(&output.name))
    {
        if is_member(output.node) {
            match output.domain {
                GraphDomain::ScalarField => {
                    field_outputs.insert((output.node, output.pin.as_str()));
                }
                GraphDomain::Candidates => {
                    candidate_outputs.insert((output.node, output.pin.as_str()));
                }
                _ => return false,
            }
        }
    }
    let input_count = synthetic_inputs + external_scalar_inputs.len();
    external_candidate_inputs.len() <= 1
        && input_lineages.len() <= 1
        && field_outputs.len() <= 1
        && candidate_outputs.len() <= 1
        && input_count <= GRAPH_GPU_MAX_INPUTS
        && input_count + component.len() <= GRAPH_GPU_MAX_REGISTERS
}

fn execution_group(
    unit: &CompiledGraphUnit,
    live: &CompiledDemandSlice,
    demand: &CompiledDemandUnitSlice,
    nodes: &BTreeMap<u128, &CompiledGraphNode>,
    members: &[u128],
    domain: GraphExecutionDomain,
) -> Result<GraphExecutionGroup> {
    let is_member = |node| members.contains(&node);
    let mut inputs = BTreeSet::new();
    let mut outputs = BTreeSet::new();
    for edge in unit.edges.iter().filter(|edge| demand.contains_edge(edge)) {
        if !is_member(edge.from_node) && is_member(edge.to_node) {
            let source = nodes[&edge.from_node];
            let pin = source
                .outputs
                .iter()
                .find(|pin| pin.name == edge.from_pin)
                .ok_or_else(|| program_error("schedule.inputs", "source pin is missing"))?;
            inputs.insert(GraphExecutionBoundary {
                pin: QualifiedGraphPin {
                    node: source.address(),
                    pin: edge.from_pin.clone(),
                },
                domain: pin.domain,
            });
        }
        if is_member(edge.from_node) && !is_member(edge.to_node) {
            let source = nodes[&edge.from_node];
            let pin = source
                .outputs
                .iter()
                .find(|pin| pin.name == edge.from_pin)
                .ok_or_else(|| program_error("schedule.outputs", "source pin is missing"))?;
            outputs.insert(GraphExecutionBoundary {
                pin: QualifiedGraphPin {
                    node: source.address(),
                    pin: edge.from_pin.clone(),
                },
                domain: pin.domain,
            });
        }
    }
    for output in unit
        .outputs
        .iter()
        .filter(|output| demand.outputs.contains(&output.name))
    {
        if is_member(output.node) {
            let source = nodes[&output.node];
            outputs.insert(GraphExecutionBoundary {
                pin: QualifiedGraphPin {
                    node: source.address(),
                    pin: output.pin.clone(),
                },
                domain: output.domain,
            });
        }
    }
    let predicted_output_bytes = members
        .iter()
        .map(|node| live.node_estimate(&nodes[node].address()).memory_bytes)
        .max()
        .unwrap_or(0);
    let transfer_in_bytes = if domain == GraphExecutionDomain::SlangCompute {
        predicted_output_bytes
    } else {
        0
    };
    let mut scheduled_nodes = Vec::new();
    reserve_exact(
        &mut scheduled_nodes,
        members.len(),
        "scheduled execution nodes",
    )?;
    for guid in members {
        let node = nodes[guid];
        scheduled_nodes.push(GraphExecutionNode {
            address: node.address(),
            operator: node.definition.operator,
            authority: node.definition.authority,
            node_hash: node.definition_hash,
        });
    }
    let mut scheduled_inputs = Vec::new();
    reserve_exact(
        &mut scheduled_inputs,
        inputs.len(),
        "scheduled execution inputs",
    )?;
    scheduled_inputs.extend(inputs);
    let mut scheduled_outputs = Vec::new();
    reserve_exact(
        &mut scheduled_outputs,
        outputs.len(),
        "scheduled execution outputs",
    )?;
    scheduled_outputs.extend(outputs);
    Ok(GraphExecutionGroup {
        domain,
        nodes: scheduled_nodes,
        inputs: scheduled_inputs,
        outputs: scheduled_outputs,
        transfer_in_bytes,
        predicted_output_bytes,
    })
}
