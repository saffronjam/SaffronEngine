//! Pin-level demand slicing: what the graph's demanded outputs actually execute.

use std::collections::{BTreeMap, BTreeSet};

use crate::hash::sha256;
use crate::{Error, Result};

use super::*;

#[derive(Clone, Debug, Default)]
pub(crate) struct CompiledDemandUnitSlice {
    pub(crate) nodes: Vec<u128>,
    pub(crate) edges: Vec<GraphEdge>,
    pub(crate) inputs: BTreeSet<String>,
    pub(crate) outputs: BTreeSet<String>,
}

impl CompiledDemandUnitSlice {
    pub(crate) fn contains_node(&self, node: u128) -> bool {
        self.nodes.contains(&node)
    }

    pub(crate) fn contains_edge(&self, edge: &GraphEdge) -> bool {
        self.edges.iter().any(|candidate| candidate == edge)
    }
}

pub(super) fn compile_demand_slice(
    root: &CompiledGraphUnit,
    seeds: impl IntoIterator<Item = QualifiedGraphPin>,
    stop_pins: &BTreeSet<QualifiedGraphPin>,
) -> Result<CompiledDemandSlice> {
    let mut slice = CompiledDemandSlice::default();
    let mut pending = seeds.into_iter().collect::<Vec<_>>();
    while let Some(output_pin) = pending.pop() {
        if !slice.output_pins.insert(output_pin.clone()) {
            continue;
        }
        let module_path = output_pin.node.module_path.as_slice();
        let unit = compiled_unit_at_path(root, module_path)?;
        let node = unit
            .nodes
            .iter()
            .find(|node| node.definition.guid == output_pin.node.node)
            .ok_or_else(|| {
                graph_document(
                    "graph.demand.output",
                    "demanded output node is missing from its compiled unit",
                )
            })?;
        slice.nodes.insert(node.address());
        {
            let unit_slice = slice.units.entry(module_path.to_vec()).or_default();
            if !unit_slice.nodes.contains(&node.definition.guid) {
                unit_slice.nodes.push(node.definition.guid);
            }
        }
        let exported_outputs = unit
            .outputs
            .iter()
            .filter(|output| output.node == output_pin.node.node && output.pin == output_pin.pin)
            .filter(|output| {
                module_path.is_empty()
                    || slice
                        .units
                        .get(module_path)
                        .is_some_and(|unit| unit.outputs.contains(&output.name))
            })
            .map(|output| output.name.clone())
            .collect::<Vec<_>>();
        for name in exported_outputs {
            slice
                .units
                .entry(module_path.to_vec())
                .or_default()
                .outputs
                .insert(name.clone());
            if !module_path.is_empty() {
                let parent_path = &module_path[..module_path.len() - 1];
                let parent = compiled_unit_at_path(root, parent_path)?;
                let call = module_call_by_guid(parent, module_path[module_path.len() - 1])?;
                pending.push(QualifiedGraphPin {
                    node: call.address(),
                    pin: name,
                });
            }
        }
        if stop_pins.contains(&output_pin) {
            continue;
        }
        slice.executed_nodes.insert(node.address());

        match node.definition.operator {
            GraphOperator::InterfaceInput => {
                let Some(GraphParameterValue::String(name)) = node.definition.parameter("name")
                else {
                    return Err(graph_document(
                        "graph.demand.interfaceInput",
                        "interface input name is missing",
                    ));
                };
                slice
                    .units
                    .entry(module_path.to_vec())
                    .or_default()
                    .inputs
                    .insert(name.clone());
                if module_path.is_empty() {
                    return Err(graph_document(
                        "graph.demand.interfaceInput",
                        "root graph cannot demand an interface input",
                    ));
                }
                let parent_path = &module_path[..module_path.len() - 1];
                let call_guid = module_path[module_path.len() - 1];
                let parent = compiled_unit_at_path(root, parent_path)?;
                let call = module_call_by_guid(parent, call_guid)?;
                let edge = parent
                    .edges
                    .iter()
                    .find(|edge| {
                        edge.to_node == call.definition.guid && edge.to_pin.as_str() == name
                    })
                    .ok_or_else(|| {
                        graph_document(
                            "graph.demand.interfaceInput",
                            "demanded module input edge is missing",
                        )
                    })?;
                let call_input = QualifiedGraphPin {
                    node: call.address(),
                    pin: edge.to_pin.clone(),
                };
                slice.input_pins.insert(call_input);
                let parent_slice = slice.units.entry(parent_path.to_vec()).or_default();
                if !parent_slice.edges.contains(edge) {
                    parent_slice.edges.push(edge.clone());
                }
                let source = parent
                    .nodes
                    .iter()
                    .find(|candidate| candidate.definition.guid == edge.from_node)
                    .ok_or_else(|| {
                        graph_document(
                            "graph.demand.interfaceInput",
                            "module input source node is missing",
                        )
                    })?;
                pending.push(QualifiedGraphPin {
                    node: source.address(),
                    pin: edge.from_pin.clone(),
                });
            }
            GraphOperator::ModuleCall => {
                let module = node.module.as_deref().ok_or_else(|| {
                    graph_document("graph.demand.moduleCall", "compiled module is missing")
                })?;
                let public_output = module
                    .outputs
                    .iter()
                    .find(|output| output.name == output_pin.pin)
                    .ok_or_else(|| {
                        graph_document(
                            "graph.demand.moduleCall",
                            "demanded module output is missing",
                        )
                    })?;
                let call_guid = module_call_guid(node)?;
                let mut child_path = module_path.to_vec();
                child_path.push(call_guid);
                slice
                    .units
                    .entry(child_path.clone())
                    .or_default()
                    .outputs
                    .insert(public_output.name.clone());
                let source = module
                    .nodes
                    .iter()
                    .find(|candidate| candidate.definition.guid == public_output.node)
                    .ok_or_else(|| {
                        graph_document(
                            "graph.demand.moduleCall",
                            "module output source node is missing",
                        )
                    })?;
                pending.push(QualifiedGraphPin {
                    node: source.address(),
                    pin: public_output.pin.clone(),
                });
            }
            _ => {
                for edge in unit
                    .edges
                    .iter()
                    .filter(|edge| edge.to_node == node.definition.guid)
                    .filter(|edge| {
                        node.definition
                            .operator
                            .output_requires_input(&output_pin.pin, &edge.to_pin)
                    })
                {
                    slice.input_pins.insert(QualifiedGraphPin {
                        node: node.address(),
                        pin: edge.to_pin.clone(),
                    });
                    let unit_slice = slice.units.entry(module_path.to_vec()).or_default();
                    if !unit_slice.edges.contains(edge) {
                        unit_slice.edges.push(edge.clone());
                    }
                    let source = unit
                        .nodes
                        .iter()
                        .find(|candidate| candidate.definition.guid == edge.from_node)
                        .ok_or_else(|| {
                            graph_document(
                                "graph.demand.edge",
                                "demanded edge source node is missing",
                            )
                        })?;
                    pending.push(QualifiedGraphPin {
                        node: source.address(),
                        pin: edge.from_pin.clone(),
                    });
                }
            }
        }
    }
    let nested_paths = slice
        .nodes
        .iter()
        .map(|address| address.module_path.clone())
        .collect::<BTreeSet<_>>();
    for nested_path in nested_paths {
        for depth in 0..nested_path.len() {
            let parent_path = &nested_path[..depth];
            let parent = compiled_unit_at_path(root, parent_path)?;
            let call = module_call_by_guid(parent, nested_path[depth])?;
            slice.nodes.insert(call.address());
            slice.executed_nodes.insert(call.address());
            let parent_slice = slice.units.entry(parent_path.to_vec()).or_default();
            if !parent_slice.nodes.contains(&call.definition.guid) {
                parent_slice.nodes.push(call.definition.guid);
            }
        }
    }
    canonicalize_demand_slice(root, &[], &mut slice)?;
    let mut estimates = BTreeMap::new();
    let mut visiting = BTreeSet::new();
    for pin in slice.output_pins.iter().cloned().collect::<Vec<_>>() {
        estimate_demand_pin(root, &slice, &pin, &mut estimates, &mut visiting)?;
    }
    slice.estimates = estimates;
    slice.dependencies = demanded_dependencies(root, &slice)?;
    Ok(slice)
}

fn demanded_dependencies(
    root: &CompiledGraphUnit,
    demand: &CompiledDemandSlice,
) -> Result<Vec<GraphDependencyFingerprint>> {
    let mut dependencies = BTreeSet::new();
    for address in &demand.nodes {
        let unit = compiled_unit_at_path(root, &address.module_path)?;
        let node = unit
            .nodes
            .iter()
            .find(|node| node.definition.guid == address.node)
            .ok_or_else(|| graph_document("graph.demand.dependencies", "live node is missing"))?;
        dependencies.extend(live_node_dependencies(node));
    }
    Ok(dependencies.into_iter().collect())
}

pub(super) fn demand_unit_semantic_hash(
    root: &CompiledGraphUnit,
    demand: &CompiledDemandSlice,
    module_path: &[u128],
) -> Result<[u8; 32]> {
    let unit = compiled_unit_at_path(root, module_path)?;
    let unit_demand = demand
        .unit(module_path)
        .ok_or_else(|| graph_document("graph.demand.identity", "live unit slice is missing"))?;
    let mut bytes = b"saffron-anima/vegetation-live-unit/v1\0".to_vec();
    bytes.extend_from_slice(&(module_path.len() as u64).to_be_bytes());
    for call in module_path {
        bytes.extend_from_slice(&call.to_be_bytes());
    }
    let mut inputs = unit
        .inputs
        .iter()
        .filter(|input| unit_demand.inputs.contains(&input.name))
        .collect::<Vec<_>>();
    inputs.sort_by(|left, right| (left.id, &left.name).cmp(&(right.id, &right.name)));
    append_identity_collection(&mut bytes, "inputs", inputs.len());
    for input in inputs {
        bytes.extend_from_slice(&input.id.to_be_bytes());
        append_identity_text(&mut bytes, &input.name);
        append_identity_text(&mut bytes, input.domain.as_wire());
    }
    let mut outputs = unit
        .outputs
        .iter()
        .filter(|output| unit_demand.outputs.contains(&output.name))
        .collect::<Vec<_>>();
    outputs.sort_by(|left, right| (left.id, &left.name).cmp(&(right.id, &right.name)));
    append_identity_collection(&mut bytes, "outputs", outputs.len());
    for output in outputs {
        bytes.extend_from_slice(&output.id.to_be_bytes());
        append_identity_text(&mut bytes, &output.name);
        append_identity_text(&mut bytes, output.domain.as_wire());
        bytes.extend_from_slice(&output.node.to_be_bytes());
        append_identity_text(&mut bytes, &output.pin);
    }
    let nodes = unit
        .nodes
        .iter()
        .filter(|node| unit_demand.contains_node(node.definition.guid))
        .collect::<Vec<_>>();
    append_identity_collection(&mut bytes, "nodes", nodes.len());
    for node in nodes {
        bytes.extend_from_slice(&live_node_semantic_hash(unit, node, demand, true)?);
        if node.definition.operator == GraphOperator::ModuleCall {
            let mut child_path = module_path.to_vec();
            child_path.push(module_call_guid(node)?);
            bytes.extend_from_slice(&demand_unit_semantic_hash(root, demand, &child_path)?);
        }
    }
    append_identity_collection(&mut bytes, "edges", unit_demand.edges.len());
    for edge in &unit_demand.edges {
        bytes.extend_from_slice(&edge.from_node.to_be_bytes());
        append_identity_text(&mut bytes, &edge.from_pin);
        bytes.extend_from_slice(&edge.to_node.to_be_bytes());
        append_identity_text(&mut bytes, &edge.to_pin);
    }
    Ok(sha256(&bytes))
}

pub(super) fn live_node_semantic_hash(
    unit: &CompiledGraphUnit,
    node: &CompiledGraphNode,
    demand: &CompiledDemandSlice,
    include_ports: bool,
) -> Result<[u8; 32]> {
    let mut bytes = b"saffron-anima/vegetation-live-node/v1\0".to_vec();
    let mut canonical_definition = node.definition.clone();
    canonical_definition.dependencies.sort();
    canonical_definition.dependencies.dedup();
    let definition = canonical_definition.canonical_bytes();
    bytes.extend_from_slice(&(definition.len() as u64).to_be_bytes());
    bytes.extend_from_slice(&definition);
    if include_ports {
        let input_pins = demand
            .input_pins
            .iter()
            .filter(|pin| pin.node == node.address())
            .collect::<Vec<_>>();
        append_identity_collection(&mut bytes, "inputs", input_pins.len());
        for pin in input_pins {
            append_identity_text(&mut bytes, &pin.pin);
        }
        let output_pins = demand
            .output_pins
            .iter()
            .filter(|pin| pin.node == node.address())
            .collect::<Vec<_>>();
        append_identity_collection(&mut bytes, "outputs", output_pins.len());
        for pin in output_pins {
            append_identity_text(&mut bytes, &pin.pin);
        }
    } else {
        append_identity_collection(&mut bytes, "inputs", 0);
        append_identity_collection(&mut bytes, "outputs", 0);
    }
    let reads_palette = matches!(
        node.definition.operator,
        GraphOperator::SpeciesInput | GraphOperator::CommunityBlend
    );
    append_identity_collection(
        &mut bytes,
        "palette",
        if reads_palette { unit.palette.len() } else { 0 },
    );
    if reads_palette {
        let mut palette = unit.palette.clone();
        palette.sort_by_key(|entry| (entry.plant.value(), entry.weight, entry.seed_namespace));
        for entry in &palette {
            bytes.extend_from_slice(&entry.plant.value().to_be_bytes());
            bytes.extend_from_slice(&entry.weight.bits().to_be_bytes());
            bytes.extend_from_slice(&entry.seed_namespace.to_be_bytes());
        }
    }
    let suitability_count = if node.definition.operator == GraphOperator::Suitability {
        unit.suitability
            .iter()
            .filter(|binding| binding.node_guid == node.definition.guid)
            .count()
    } else {
        0
    };
    append_identity_collection(&mut bytes, "suitability", suitability_count);
    if node.definition.operator == GraphOperator::Suitability {
        let mut suitability = unit
            .suitability
            .iter()
            .filter(|binding| binding.node_guid == node.definition.guid)
            .copied()
            .collect::<Vec<_>>();
        suitability.sort_by_key(|binding| {
            (
                binding.node_guid,
                binding.channel,
                binding.minimum,
                binding.maximum,
                binding.falloff,
            )
        });
        for binding in suitability {
            append_identity_text(&mut bytes, &field_channel_wire(binding.channel));
            bytes.extend_from_slice(&binding.minimum.bits().to_be_bytes());
            bytes.extend_from_slice(&binding.maximum.bits().to_be_bytes());
            bytes.extend_from_slice(&binding.falloff.bits().to_be_bytes());
            bytes.extend_from_slice(&binding.node_guid.to_be_bytes());
        }
    }
    if matches!(node.definition.operator, GraphOperator::CommunityInput) {
        append_identity_collection(&mut bytes, "competition", unit.competition.len());
        let mut rules = unit.competition.clone();
        rules.sort_by_key(|rule| {
            (
                rule.first.value(),
                rule.second.value(),
                rule.spacing,
                rule.priority,
            )
        });
        for rule in &rules {
            bytes.extend_from_slice(&rule.first.value().to_be_bytes());
            bytes.extend_from_slice(&rule.second.value().to_be_bytes());
            bytes.extend_from_slice(&rule.spacing.bits().to_be_bytes());
            bytes.extend_from_slice(&rule.priority.to_be_bytes());
        }
    } else {
        append_identity_collection(&mut bytes, "competition", 0);
    }
    if matches!(
        node.definition.operator,
        GraphOperator::RecursiveCompanion | GraphOperator::CommunityInput
    ) {
        append_identity_collection(&mut bytes, "companions", unit.companions.len());
        let mut rules = unit.companions.clone();
        rules.sort_by_key(|rule| {
            (
                rule.parent.value(),
                rule.child.value(),
                rule.minimum_distance,
                rule.maximum_distance,
                rule.probability,
            )
        });
        for rule in &rules {
            bytes.extend_from_slice(&rule.parent.value().to_be_bytes());
            bytes.extend_from_slice(&rule.child.value().to_be_bytes());
            bytes.extend_from_slice(&rule.minimum_distance.bits().to_be_bytes());
            bytes.extend_from_slice(&rule.maximum_distance.bits().to_be_bytes());
            bytes.extend_from_slice(&rule.probability.bits().to_be_bytes());
        }
    } else {
        append_identity_collection(&mut bytes, "companions", 0);
    }
    if matches!(
        node.definition.operator,
        GraphOperator::SuccessionInput | GraphOperator::CommunityInput
    ) {
        append_identity_collection(&mut bytes, "succession", unit.succession.len());
        let mut rules = unit.succession.clone();
        rules.sort_by_key(|rule| {
            (
                rule.from.value(),
                rule.to.value(),
                rule.minimum_tick,
                rule.probability,
            )
        });
        for rule in &rules {
            bytes.extend_from_slice(&rule.from.value().to_be_bytes());
            bytes.extend_from_slice(&rule.to.value().to_be_bytes());
            bytes.extend_from_slice(&rule.minimum_tick.to_be_bytes());
            bytes.extend_from_slice(&rule.probability.bits().to_be_bytes());
        }
    } else {
        append_identity_collection(&mut bytes, "succession", 0);
    }
    if matches!(
        node.definition.operator,
        GraphOperator::FieldSample | GraphOperator::PaintedTile
    ) {
        bytes.push(unit.require_authoritative_fields.into());
    } else {
        bytes.push(0);
    }
    let mut dependencies = live_node_dependencies(node).collect::<Vec<_>>();
    dependencies.sort();
    dependencies.dedup();
    append_identity_collection(&mut bytes, "dependencies", dependencies.len());
    for dependency in dependencies {
        append_dependency_source(&mut bytes, dependency.source);
        bytes.extend_from_slice(&dependency.content_hash);
    }
    Ok(sha256(&bytes))
}

pub(super) fn append_identity_text(bytes: &mut Vec<u8>, value: &str) {
    bytes.extend_from_slice(&(value.len() as u64).to_be_bytes());
    bytes.extend_from_slice(value.as_bytes());
}

pub(super) fn append_identity_collection(bytes: &mut Vec<u8>, name: &str, count: usize) {
    append_identity_text(bytes, name);
    bytes.extend_from_slice(&(count as u64).to_be_bytes());
}

/// A module call reads only the dependencies it declares; every other node reads all of its own.
pub(super) fn live_node_dependencies(
    node: &CompiledGraphNode,
) -> impl Iterator<Item = GraphDependencyFingerprint> + '_ {
    node.dependencies.iter().copied().filter(|dependency| {
        node.definition.operator != GraphOperator::ModuleCall
            || node.definition.dependencies.contains(&dependency.source)
    })
}

fn estimate_demand_pin(
    root: &CompiledGraphUnit,
    demand: &CompiledDemandSlice,
    pin: &QualifiedGraphPin,
    estimates: &mut BTreeMap<QualifiedGraphPin, GraphEstimate>,
    visiting: &mut BTreeSet<QualifiedGraphPin>,
) -> Result<GraphEstimate> {
    if let Some(estimate) = estimates.get(pin).copied() {
        return Ok(estimate);
    }
    if !visiting.insert(pin.clone()) {
        return Err(Error::GraphCycle {
            node: pin.node.node,
        });
    }
    let unit = compiled_unit_at_path(root, &pin.node.module_path)?;
    let node = unit
        .nodes
        .iter()
        .find(|node| node.definition.guid == pin.node.node)
        .ok_or_else(|| graph_document("graph.demand.estimate", "live output node is missing"))?;
    let mut upstream = GraphEstimate::default();
    for source in demanded_upstream_pins(root, demand, pin, "graph.demand.estimate")? {
        upstream = merge_input_estimate(
            upstream,
            estimate_demand_pin(root, demand, &source, estimates, visiting)?,
        )?;
    }
    let estimate = match node.definition.operator {
        GraphOperator::InterfaceInput | GraphOperator::ModuleCall => upstream,
        _ => estimate_node(&node.definition, upstream)?,
    };
    visiting.remove(pin);
    estimates.insert(pin.clone(), estimate);
    Ok(estimate)
}

/// The demanded producers of one pin, resolved across module-call boundaries.
pub(super) fn demanded_upstream_pins(
    root: &CompiledGraphUnit,
    demand: &CompiledDemandSlice,
    pin: &QualifiedGraphPin,
    path: &'static str,
) -> Result<Vec<QualifiedGraphPin>> {
    let unit = compiled_unit_at_path(root, &pin.node.module_path)?;
    let node = unit
        .nodes
        .iter()
        .find(|node| node.definition.guid == pin.node.node)
        .ok_or_else(|| graph_document(path, "live output node is missing"))?;
    match node.definition.operator {
        GraphOperator::InterfaceInput => {
            let Some(GraphParameterValue::String(name)) = node.definition.parameter("name") else {
                return Err(graph_document(path, "interface input name is missing"));
            };
            let module_path = pin.node.module_path.as_slice();
            let parent_path = &module_path[..module_path.len() - 1];
            let parent = compiled_unit_at_path(root, parent_path)?;
            let call = module_call_by_guid(parent, module_path[module_path.len() - 1])?;
            let edge = parent
                .edges
                .iter()
                .find(|edge| edge.to_node == call.definition.guid && edge.to_pin.as_str() == name)
                .ok_or_else(|| graph_document(path, "live module input edge is missing"))?;
            let source = parent
                .nodes
                .iter()
                .find(|source| source.definition.guid == edge.from_node)
                .ok_or_else(|| graph_document(path, "live module input source is missing"))?;
            Ok(vec![QualifiedGraphPin {
                node: source.address(),
                pin: edge.from_pin.clone(),
            }])
        }
        GraphOperator::ModuleCall => {
            let module = node
                .module
                .as_deref()
                .ok_or_else(|| graph_document(path, "compiled module is missing"))?;
            let output = module
                .outputs
                .iter()
                .find(|output| output.name == pin.pin)
                .ok_or_else(|| graph_document(path, "live module output is missing"))?;
            let source = module
                .nodes
                .iter()
                .find(|source| source.definition.guid == output.node)
                .ok_or_else(|| graph_document(path, "live module output source is missing"))?;
            Ok(vec![QualifiedGraphPin {
                node: source.address(),
                pin: output.pin.clone(),
            }])
        }
        _ => {
            let unit_demand = demand
                .unit(&pin.node.module_path)
                .ok_or_else(|| graph_document(path, "live unit slice is missing"))?;
            unit.edges
                .iter()
                .filter(|edge| {
                    edge.to_node == node.definition.guid
                        && unit_demand.contains_edge(edge)
                        && node
                            .definition
                            .operator
                            .output_requires_input(&pin.pin, &edge.to_pin)
                })
                .map(|edge| {
                    let source = unit
                        .nodes
                        .iter()
                        .find(|source| source.definition.guid == edge.from_node)
                        .ok_or_else(|| graph_document(path, "live edge source node is missing"))?;
                    Ok(QualifiedGraphPin {
                        node: source.address(),
                        pin: edge.from_pin.clone(),
                    })
                })
                .collect()
        }
    }
}

pub(super) fn compiled_unit_at_path<'a>(
    root: &'a CompiledGraphUnit,
    module_path: &[u128],
) -> Result<&'a CompiledGraphUnit> {
    let mut unit = root;
    for call_guid in module_path {
        unit = module_call_by_guid(unit, *call_guid)?
            .module
            .as_deref()
            .ok_or_else(|| {
                graph_document("graph.demand.modulePath", "compiled module is missing")
            })?;
    }
    Ok(unit)
}

pub(super) fn module_call_by_guid(
    unit: &CompiledGraphUnit,
    call_guid: u128,
) -> Result<&CompiledGraphNode> {
    unit.nodes
        .iter()
        .find(|node| {
            node.definition.operator == GraphOperator::ModuleCall
                && matches!(
                    node.definition.parameter("callGuid"),
                    Some(GraphParameterValue::Guid(candidate)) if *candidate == call_guid
                )
        })
        .ok_or_else(|| graph_document("graph.demand.modulePath", "module call is missing"))
}

pub(super) fn module_call_guid(node: &CompiledGraphNode) -> Result<u128> {
    match node.definition.parameter("callGuid") {
        Some(GraphParameterValue::Guid(call_guid)) => Ok(*call_guid),
        _ => Err(graph_document(
            "graph.demand.moduleCall",
            "module call GUID is missing",
        )),
    }
}

fn canonicalize_demand_slice(
    unit: &CompiledGraphUnit,
    module_path: &[u128],
    slice: &mut CompiledDemandSlice,
) -> Result<()> {
    if let Some(unit_slice) = slice.units.get_mut(module_path) {
        unit_slice.nodes = unit
            .nodes
            .iter()
            .filter(|node| slice.nodes.contains(&node.address()))
            .map(|node| node.definition.guid)
            .collect();
        unit_slice.edges = unit
            .edges
            .iter()
            .filter(|edge| unit_slice.edges.contains(edge))
            .cloned()
            .collect();
    }
    for node in &unit.nodes {
        if let Some(module) = node.module.as_deref() {
            let mut child_path = module_path.to_vec();
            child_path.push(module_call_guid(node)?);
            canonicalize_demand_slice(module, &child_path, slice)?;
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Default)]
pub(crate) struct CompiledDemandSlice {
    pub(crate) nodes: BTreeSet<GraphNodeAddress>,
    pub(crate) executed_nodes: BTreeSet<GraphNodeAddress>,
    pub(crate) input_pins: BTreeSet<QualifiedGraphPin>,
    pub(crate) output_pins: BTreeSet<QualifiedGraphPin>,
    pub(crate) estimates: BTreeMap<QualifiedGraphPin, GraphEstimate>,
    pub(crate) dependencies: Vec<GraphDependencyFingerprint>,
    pub(crate) units: BTreeMap<Vec<u128>, CompiledDemandUnitSlice>,
}

impl CompiledDemandSlice {
    pub(crate) fn unit(&self, module_path: &[u128]) -> Option<&CompiledDemandUnitSlice> {
        self.units.get(module_path)
    }

    pub(crate) fn contains_node(&self, address: &GraphNodeAddress) -> bool {
        self.nodes.contains(address)
    }

    pub(crate) fn executes_node(&self, address: &GraphNodeAddress) -> bool {
        self.executed_nodes.contains(address)
    }

    pub(crate) fn estimate(&self, pin: &QualifiedGraphPin) -> Option<GraphEstimate> {
        self.estimates.get(pin).copied()
    }

    pub(crate) fn node_estimate(&self, address: &GraphNodeAddress) -> GraphEstimate {
        self.estimates
            .iter()
            .filter(|(pin, _)| pin.node == *address)
            .map(|(_, estimate)| *estimate)
            .fold(GraphEstimate::default(), max_estimate)
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct CompiledDemandPlan {
    pub(super) execution: CompiledDemandSlice,
    pub(super) public: CompiledDemandSlice,
    pub(super) stages: BTreeMap<[u8; 32], CompiledDemandSlice>,
}

impl CompiledDemandPlan {
    pub(crate) const fn execution_slice(&self) -> &CompiledDemandSlice {
        &self.execution
    }

    pub(crate) const fn public_slice(&self) -> &CompiledDemandSlice {
        &self.public
    }

    pub(crate) fn stage_slice(&self, stage: [u8; 32]) -> Option<&CompiledDemandSlice> {
        self.stages.get(&stage)
    }

    pub(crate) fn same_scope_membership(
        &self,
        left: &GraphNodeAddress,
        right: &GraphNodeAddress,
    ) -> bool {
        self.public.executes_node(left) == self.public.executes_node(right)
            && self
                .stages
                .values()
                .all(|stage| stage.executes_node(left) == stage.executes_node(right))
    }
}

pub(super) fn demanded_estimate(demand: &CompiledDemandSlice) -> GraphEstimate {
    demand
        .estimates
        .values()
        .copied()
        .fold(GraphEstimate::default(), max_estimate)
}

pub(super) fn apply_demanded_estimates(
    unit: &mut CompiledGraphUnit,
    module_path: &[u128],
    demand: &CompiledDemandSlice,
) -> Result<()> {
    for node in &mut unit.nodes {
        let address = node.address();
        if demand.contains_node(&address) {
            let mut estimate = GraphEstimate::default();
            for output in &node.outputs {
                let pin = QualifiedGraphPin {
                    node: address.clone(),
                    pin: output.name.clone(),
                };
                if let Some(output_estimate) = demand.estimate(&pin) {
                    node.output_estimates
                        .insert(output.name.clone(), output_estimate);
                    estimate = max_estimate(estimate, output_estimate);
                }
            }
            node.estimate = estimate;
        }
        if node.module.is_some() {
            let call_guid = module_call_guid(node)?;
            let mut child_path = module_path.to_vec();
            child_path.push(call_guid);
            let module = node.module.as_deref_mut().ok_or_else(|| {
                graph_document("graph.demand.estimate", "compiled module is missing")
            })?;
            apply_demanded_estimates(module, &child_path, demand)?;
        }
    }
    for output in &unit.outputs {
        let source = unit
            .nodes
            .iter()
            .find(|node| node.definition.guid == output.node)
            .ok_or_else(|| {
                graph_document(
                    "graph.demand.estimate",
                    "unit output source node is missing",
                )
            })?;
        let pin = QualifiedGraphPin {
            node: source.address(),
            pin: output.pin.clone(),
        };
        if let Some(estimate) = demand.estimate(&pin) {
            unit.output_estimates.insert(output.name.clone(), estimate);
        }
    }
    unit.estimate = demand
        .estimates
        .iter()
        .filter(|(pin, _)| pin.node.module_path.starts_with(module_path))
        .map(|(_, estimate)| *estimate)
        .fold(GraphEstimate::default(), max_estimate);
    unit.dependencies = demand
        .nodes
        .iter()
        .filter(|address| address.module_path.starts_with(module_path))
        .flat_map(|address| {
            compiled_unit_at_path(unit, &address.module_path[module_path.len()..])
                .ok()
                .and_then(|owner| {
                    owner
                        .nodes
                        .iter()
                        .find(|node| node.definition.guid == address.node)
                })
                .into_iter()
                .flat_map(live_node_dependencies)
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    Ok(())
}
