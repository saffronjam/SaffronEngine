//! The compiler-owned spatial schedule: global stages and their closures.

use std::collections::{BTreeMap, BTreeSet};

use saffron_core::Uuid;
use saffron_spatial::DecisionScalar;

use crate::Result;
use crate::hash::sha256;

use super::*;

/// One immutable ancestor/global execution stage compiled from the canonical graph IR.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompiledGlobalStage {
    /// Stable identity derived from the complete graph and stage topology.
    pub id: [u8; 32],
    /// Canonical ancestor-cell level forming the closed solve domain.
    pub owner_level: u8,
    /// Maximal connected same-level global node slice in canonical order.
    pub nodes: Vec<GraphNodeAddress>,
    /// Complete upstream execution closure, stopping at earlier global stages.
    pub closure: Vec<GraphNodeAddress>,
    /// Immutable outputs from earlier stages required by this closure.
    pub input_pins: Vec<QualifiedGraphPin>,
    /// Global outputs materialized for downstream stages, cells, or graph outputs.
    pub output_pins: Vec<QualifiedGraphPin>,
    /// Exact immutable dependency fingerprints visible to the stage closure.
    pub dependencies: Vec<GraphDependencyFingerprint>,
    /// Finest hierarchy level required by any node in the closure.
    pub minimum_input_level: u8,
    /// Exact composed finite halo required around the closed owner-cell solve bounds.
    pub upstream_halo: DecisionScalar,
    /// Conservative aggregate work and retained-memory estimate.
    pub estimate: GraphEstimate,
}

/// Compiler-owned spatial schedule over the single canonical graph IR.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CompiledSpatialPlan {
    global_stages: Vec<CompiledGlobalStage>,
    global_stage_by_node: BTreeMap<GraphNodeAddress, [u8; 32]>,
}

impl CompiledSpatialPlan {
    /// Global stages in canonical dependency/topological order.
    #[must_use]
    pub fn global_stages(&self) -> &[CompiledGlobalStage] {
        &self.global_stages
    }

    /// Finds one global stage by its stable identity.
    #[must_use]
    pub fn global_stage(&self, id: [u8; 32]) -> Option<&CompiledGlobalStage> {
        self.global_stages.iter().find(|stage| stage.id == id)
    }

    /// Finds the global stage containing one fully qualified compiled node.
    #[must_use]
    pub fn global_stage_for_node(
        &self,
        address: &GraphNodeAddress,
    ) -> Option<&CompiledGlobalStage> {
        let id = self.global_stage_by_node.get(address)?;
        self.global_stage(*id)
    }
}

#[derive(Clone)]
struct SpatialPlanNode {
    address: GraphNodeAddress,
    semantic_hash: [u8; 32],
    spatial: NodeSpatialPolicy,
    estimate: GraphEstimate,
    dependencies: Vec<GraphDependencyFingerprint>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct SpatialPlanEdge {
    from: QualifiedGraphPin,
    to: QualifiedGraphPin,
}

#[derive(Default)]
struct FlattenedSpatialGraph {
    nodes: Vec<SpatialPlanNode>,
    edges: Vec<SpatialPlanEdge>,
    outputs: Vec<QualifiedGraphPin>,
}

pub(super) fn compile_spatial_plan(
    root: &CompiledGraphUnit,
    demand: &CompiledDemandSlice,
) -> Result<CompiledSpatialPlan> {
    let mut flattened = FlattenedSpatialGraph::default();
    let outputs = flatten_spatial_unit(root, &BTreeMap::new(), &[], demand, &mut flattened)?;
    flattened.outputs = root
        .outputs
        .iter()
        .map(|output| {
            outputs.get(&output.name).cloned().ok_or_else(|| {
                graph_document("graph.outputs", "flattened output source is missing")
            })
        })
        .collect::<Result<Vec<_>>>()?;
    flattened.edges.sort();
    flattened.edges.dedup();

    let node_indices = flattened
        .nodes
        .iter()
        .enumerate()
        .map(|(index, node)| (node.address.clone(), index))
        .collect::<BTreeMap<_, _>>();
    if node_indices.len() != flattened.nodes.len() {
        return Err(graph_document(
            "graph.spatialPlan",
            "flattened node addresses are not unique",
        ));
    }
    let global_nodes = flattened
        .nodes
        .iter()
        .enumerate()
        .filter_map(|(index, node)| {
            matches!(node.spatial, NodeSpatialPolicy::Global { .. }).then_some(index)
        })
        .collect::<BTreeSet<_>>();
    let mut global_adjacency = BTreeMap::<usize, BTreeSet<usize>>::new();
    for edge in &flattened.edges {
        let Some(&from) = node_indices.get(&edge.from.node) else {
            continue;
        };
        let Some(&to) = node_indices.get(&edge.to.node) else {
            continue;
        };
        if global_nodes.contains(&from)
            && global_nodes.contains(&to)
            && flattened.nodes[from].spatial.level() == flattened.nodes[to].spatial.level()
        {
            global_adjacency.entry(from).or_default().insert(to);
            global_adjacency.entry(to).or_default().insert(from);
        }
    }

    let mut components = Vec::<Vec<usize>>::new();
    let mut assigned = BTreeSet::new();
    for &start in &global_nodes {
        if !assigned.insert(start) {
            continue;
        }
        let mut pending = vec![start];
        let mut component = Vec::new();
        while let Some(index) = pending.pop() {
            component.push(index);
            for &neighbour in global_adjacency.get(&index).into_iter().flatten() {
                if assigned.insert(neighbour) {
                    pending.push(neighbour);
                }
            }
        }
        component.sort_unstable();
        components.push(component);
    }

    let mut component_by_node = BTreeMap::<GraphNodeAddress, usize>::new();
    for (component, nodes) in components.iter().enumerate() {
        for &index in nodes {
            component_by_node.insert(flattened.nodes[index].address.clone(), component);
        }
    }
    let mut incoming = BTreeMap::<GraphNodeAddress, Vec<&SpatialPlanEdge>>::new();
    for edge in &flattened.edges {
        incoming.entry(edge.to.node.clone()).or_default().push(edge);
    }

    let mut stages = Vec::with_capacity(components.len());
    let mut global_stage_by_node = BTreeMap::new();
    for (component_index, component) in components.iter().enumerate() {
        let owner_level = flattened.nodes[component[0]].spatial.level();
        let member_addresses = component
            .iter()
            .map(|&index| flattened.nodes[index].address.clone())
            .collect::<BTreeSet<_>>();
        let mut closure_indices = component.iter().copied().collect::<BTreeSet<_>>();
        let mut pending = component.clone();
        let mut input_pins = BTreeSet::new();
        while let Some(index) = pending.pop() {
            let address = &flattened.nodes[index].address;
            for edge in incoming.get(address).into_iter().flatten() {
                if component_by_node
                    .get(&edge.from.node)
                    .is_some_and(|source| *source != component_index)
                {
                    input_pins.insert(edge.from.clone());
                    continue;
                }
                let source = *node_indices.get(&edge.from.node).ok_or_else(|| {
                    graph_document("graph.spatialPlan", "upstream node is missing")
                })?;
                if closure_indices.insert(source) {
                    pending.push(source);
                }
            }
        }

        let mut output_pins = BTreeSet::new();
        for edge in &flattened.edges {
            if member_addresses.contains(&edge.from.node)
                && !member_addresses.contains(&edge.to.node)
            {
                output_pins.insert(edge.from.clone());
            }
        }
        for output in &flattened.outputs {
            if member_addresses.contains(&output.node) {
                output_pins.insert(output.clone());
            }
        }

        let closure = closure_indices
            .iter()
            .map(|&index| flattened.nodes[index].address.clone())
            .collect::<Vec<_>>();
        let dependencies = closure_indices
            .iter()
            .flat_map(|&index| flattened.nodes[index].dependencies.iter().copied())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let minimum_input_level = closure_indices
            .iter()
            .map(|&index| flattened.nodes[index].spatial.level())
            .min()
            .unwrap_or(owner_level);
        let upstream_halo = global_stage_upstream_halo(
            component,
            &closure_indices,
            &flattened.nodes,
            &flattened.edges,
        )?;
        let estimate = closure_indices
            .iter()
            .map(|&index| flattened.nodes[index].estimate)
            .fold(GraphEstimate::default(), max_estimate);
        let nodes = component
            .iter()
            .map(|&index| flattened.nodes[index].address.clone())
            .collect::<Vec<_>>();
        let input_pins = input_pins.into_iter().collect::<Vec<_>>();
        let output_pins = output_pins.into_iter().collect::<Vec<_>>();
        let prerequisite_stages = input_pins
            .iter()
            .filter_map(|pin| global_stage_by_node.get(&pin.node).copied())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let id = global_stage_identity(&GlobalStageIdentityInputs {
            root_biome: root.biome,
            owner_level,
            nodes: &nodes,
            closure: &closure,
            flattened_nodes: &flattened.nodes,
            edges: &flattened.edges,
            input_pins: &input_pins,
            output_pins: &output_pins,
            dependencies: &dependencies,
            prerequisite_stages: &prerequisite_stages,
        });
        for node in &nodes {
            global_stage_by_node.insert(node.clone(), id);
        }
        stages.push(CompiledGlobalStage {
            id,
            owner_level,
            nodes,
            closure,
            input_pins,
            output_pins,
            dependencies,
            minimum_input_level,
            upstream_halo,
            estimate,
        });
    }
    Ok(CompiledSpatialPlan {
        global_stages: stages,
        global_stage_by_node,
    })
}

fn global_stage_upstream_halo(
    stage_nodes: &[usize],
    closure: &BTreeSet<usize>,
    nodes: &[SpatialPlanNode],
    edges: &[SpatialPlanEdge],
) -> Result<DecisionScalar> {
    let mut support_by_node = BTreeMap::<GraphNodeAddress, DecisionScalar>::new();
    for &index in closure {
        let node = &nodes[index];
        let upstream = edges
            .iter()
            .filter(|edge| edge.to.node == node.address)
            .filter_map(|edge| support_by_node.get(&edge.from.node).copied())
            .max()
            .unwrap_or(DecisionScalar::from_bits(0));
        let local = match node.spatial {
            NodeSpatialPolicy::Partitioned {
                influence_radius, ..
            } => influence_radius,
            NodeSpatialPolicy::Global { .. } => DecisionScalar::from_bits(0),
        };
        support_by_node.insert(node.address.clone(), upstream.checked_add(local)?);
    }
    stage_nodes
        .iter()
        .map(|&index| {
            support_by_node
                .get(&nodes[index].address)
                .copied()
                .ok_or_else(|| {
                    graph_document("graph.spatialPlan", "global stage support is missing")
                })
        })
        .collect::<Result<Vec<_>>>()
        .map(|support| {
            support
                .into_iter()
                .max()
                .unwrap_or(DecisionScalar::from_bits(0))
        })
}

fn flatten_spatial_unit(
    unit: &CompiledGraphUnit,
    interface_sources: &BTreeMap<String, QualifiedGraphPin>,
    inherited_dependencies: &[GraphDependencyFingerprint],
    demand: &CompiledDemandSlice,
    flattened: &mut FlattenedSpatialGraph,
) -> Result<BTreeMap<String, QualifiedGraphPin>> {
    let incoming = incoming_edges(&unit.edges);
    let module_path = unit
        .nodes
        .first()
        .map_or_else(Vec::new, |node| node.debug_symbol.module_path.clone());
    let unit_demand = demand.unit(&module_path).ok_or_else(|| {
        graph_document(
            "graph.spatialPlan",
            "demanded compiled unit slice is missing",
        )
    })?;
    let mut aliases = BTreeMap::<(u128, String), QualifiedGraphPin>::new();
    for node in unit
        .nodes
        .iter()
        .filter(|node| unit_demand.contains_node(node.definition.guid))
    {
        if node.definition.operator == GraphOperator::InterfaceInput {
            let name = match node.definition.parameter("name") {
                Some(GraphParameterValue::String(name)) => name,
                _ => {
                    return Err(graph_document(
                        "graph.spatialPlan.interfaceInput",
                        "interface input name is missing",
                    ));
                }
            };
            let source = interface_sources.get(name).cloned().ok_or_else(|| {
                graph_document(
                    "graph.spatialPlan.interfaceInput",
                    "module input source is missing",
                )
            })?;
            for output in &node.outputs {
                let qualified = QualifiedGraphPin {
                    node: node.address(),
                    pin: output.name.clone(),
                };
                if !demand.output_pins.contains(&qualified) {
                    continue;
                }
                aliases.insert((node.definition.guid, output.name.clone()), source.clone());
            }
            continue;
        }

        let address = node.address();
        if node.definition.operator == GraphOperator::ModuleCall {
            let module = node.module.as_deref().ok_or_else(|| {
                graph_document("graph.spatialPlan.moduleCall", "compiled module is missing")
            })?;
            let module_dependencies = merged_dependencies(
                &live_node_dependencies(node).collect::<Vec<_>>(),
                inherited_dependencies,
            );
            let mut module_inputs = BTreeMap::new();
            for input in node.inputs.iter().filter(|input| {
                demand.input_pins.contains(&QualifiedGraphPin {
                    node: address.clone(),
                    pin: input.name.clone(),
                })
            }) {
                let edge = incoming
                    .get(&node.definition.guid)
                    .into_iter()
                    .flatten()
                    .find(|edge| edge.to_pin == input.name)
                    .ok_or_else(|| {
                        graph_document(
                            "graph.spatialPlan.moduleCall",
                            "module input edge is missing",
                        )
                    })?;
                let source = resolve_spatial_source(edge, &aliases)?;
                module_inputs.insert(input.name.clone(), source);
            }
            let module_outputs = flatten_spatial_unit(
                module,
                &module_inputs,
                &module_dependencies,
                demand,
                flattened,
            )?;
            flattened
                .nodes
                .push(spatial_plan_node(unit, node, demand, module_dependencies)?);
            for (name, source) in &module_inputs {
                flattened.edges.push(SpatialPlanEdge {
                    from: source.clone(),
                    to: QualifiedGraphPin {
                        node: address.clone(),
                        pin: name.clone(),
                    },
                });
            }
            for output in node.outputs.iter().filter(|output| {
                demand.output_pins.contains(&QualifiedGraphPin {
                    node: address.clone(),
                    pin: output.name.clone(),
                })
            }) {
                let source = module_outputs.get(&output.name).cloned().ok_or_else(|| {
                    graph_document(
                        "graph.spatialPlan.moduleCall",
                        "module output source is missing",
                    )
                })?;
                let pin = QualifiedGraphPin {
                    node: address.clone(),
                    pin: output.name.clone(),
                };
                flattened.edges.push(SpatialPlanEdge {
                    from: source,
                    to: pin.clone(),
                });
                aliases.insert((node.definition.guid, output.name.clone()), pin);
            }
            continue;
        }

        let dependencies = merged_dependencies(
            &live_node_dependencies(node).collect::<Vec<_>>(),
            inherited_dependencies,
        );
        flattened
            .nodes
            .push(spatial_plan_node(unit, node, demand, dependencies)?);
        for edge in incoming
            .get(&node.definition.guid)
            .into_iter()
            .flatten()
            .filter(|edge| unit_demand.contains_edge(edge))
        {
            flattened.edges.push(SpatialPlanEdge {
                from: resolve_spatial_source(edge, &aliases)?,
                to: QualifiedGraphPin {
                    node: address.clone(),
                    pin: edge.to_pin.clone(),
                },
            });
        }
        for output in node.outputs.iter().filter(|output| {
            demand.output_pins.contains(&QualifiedGraphPin {
                node: address.clone(),
                pin: output.name.clone(),
            })
        }) {
            aliases.insert(
                (node.definition.guid, output.name.clone()),
                QualifiedGraphPin {
                    node: address.clone(),
                    pin: output.name.clone(),
                },
            );
        }
    }
    unit.outputs
        .iter()
        .filter(|output| unit_demand.outputs.contains(&output.name))
        .map(|output| {
            let source = aliases
                .get(&(output.node, output.pin.clone()))
                .cloned()
                .ok_or_else(|| {
                    graph_document("graph.spatialPlan", "unit output source is missing")
                })?;
            Ok((output.name.clone(), source))
        })
        .collect()
}

fn spatial_plan_node(
    unit: &CompiledGraphUnit,
    node: &CompiledGraphNode,
    demand: &CompiledDemandSlice,
    dependencies: Vec<GraphDependencyFingerprint>,
) -> Result<SpatialPlanNode> {
    Ok(SpatialPlanNode {
        address: node.address(),
        semantic_hash: live_node_semantic_hash(unit, node, demand, false)?,
        spatial: node.definition.spatial,
        estimate: node.estimate,
        dependencies,
    })
}

fn merged_dependencies(
    direct: &[GraphDependencyFingerprint],
    inherited: &[GraphDependencyFingerprint],
) -> Vec<GraphDependencyFingerprint> {
    direct
        .iter()
        .chain(inherited)
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn resolve_spatial_source(
    edge: &GraphEdge,
    aliases: &BTreeMap<(u128, String), QualifiedGraphPin>,
) -> Result<QualifiedGraphPin> {
    aliases
        .get(&(edge.from_node, edge.from_pin.clone()))
        .cloned()
        .ok_or_else(|| graph_document("graph.spatialPlan", "edge source alias is missing"))
}

struct GlobalStageIdentityInputs<'a> {
    root_biome: Uuid,
    owner_level: u8,
    nodes: &'a [GraphNodeAddress],
    closure: &'a [GraphNodeAddress],
    flattened_nodes: &'a [SpatialPlanNode],
    edges: &'a [SpatialPlanEdge],
    input_pins: &'a [QualifiedGraphPin],
    output_pins: &'a [QualifiedGraphPin],
    dependencies: &'a [GraphDependencyFingerprint],
    prerequisite_stages: &'a [[u8; 32]],
}

fn global_stage_identity(inputs: &GlobalStageIdentityInputs<'_>) -> [u8; 32] {
    let &GlobalStageIdentityInputs {
        root_biome,
        owner_level,
        nodes,
        closure,
        flattened_nodes,
        edges,
        input_pins,
        output_pins,
        dependencies,
        prerequisite_stages,
    } = inputs;
    let mut bytes = b"saffron-anima/vegetation-global-stage/v2\0".to_vec();
    bytes.extend_from_slice(&BIOME_GRAPH_VERSION.to_be_bytes());
    bytes.extend_from_slice(&BIOME_INTERFACE_VERSION.to_be_bytes());
    bytes.extend_from_slice(&BIOME_NODE_VERSION.to_be_bytes());
    bytes.extend_from_slice(&root_biome.value().to_be_bytes());
    bytes.push(owner_level);
    append_identity_collection(&mut bytes, "closure", closure.len());
    for node in closure {
        bytes.extend_from_slice(&node.canonical_bytes());
        if let Some(compiled) = flattened_nodes
            .iter()
            .find(|candidate| candidate.address == *node)
        {
            bytes.extend_from_slice(&compiled.semantic_hash);
        }
    }
    let closure_nodes = closure.iter().collect::<BTreeSet<_>>();
    let live_edges = edges
        .iter()
        .filter(|edge| {
            closure_nodes.contains(&edge.to.node)
                && (closure_nodes.contains(&edge.from.node) || input_pins.contains(&edge.from))
        })
        .collect::<Vec<_>>();
    append_identity_collection(&mut bytes, "edges", live_edges.len());
    for edge in live_edges {
        bytes.extend_from_slice(&edge.from.canonical_bytes());
        bytes.extend_from_slice(&edge.to.canonical_bytes());
    }
    for (name, pins) in [("inputs", input_pins), ("outputs", output_pins)] {
        append_identity_collection(&mut bytes, name, pins.len());
        for pin in pins {
            bytes.extend_from_slice(&pin.canonical_bytes());
        }
    }
    append_identity_collection(&mut bytes, "dependencies", dependencies.len());
    for dependency in dependencies {
        append_dependency_source(&mut bytes, dependency.source);
        bytes.extend_from_slice(&dependency.content_hash);
    }
    append_identity_collection(&mut bytes, "prerequisiteStages", prerequisite_stages.len());
    for prerequisite in prerequisite_stages {
        bytes.extend_from_slice(prerequisite);
    }
    append_identity_collection(&mut bytes, "members", nodes.len());
    for node in nodes {
        bytes.extend_from_slice(&node.canonical_bytes());
    }
    sha256(&bytes)
}
