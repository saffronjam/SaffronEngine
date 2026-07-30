//! Composed finite-halo support required around demanded outputs.

use std::collections::{BTreeMap, BTreeSet};

use saffron_spatial::DecisionScalar;

use crate::{Error, Result};

use super::*;

pub(super) fn compile_output_halo(
    nodes: &[CompiledGraphNode],
    edges: &[GraphEdge],
    outputs: &[GraphInterfaceOutput],
) -> Result<BTreeMap<String, [DecisionScalar; 63]>> {
    let incoming = incoming_edges(edges);
    let zero = DecisionScalar::from_bits(0);
    let mut result = outputs
        .iter()
        .map(|output| (output.name.clone(), [zero; 63]))
        .collect::<BTreeMap<_, _>>();
    for output_level in 0_u8..=62 {
        let mut support_by_pin = BTreeMap::<(u128, String), DecisionScalar>::new();
        for node in nodes {
            let upstream = incoming
                .get(&node.definition.guid)
                .into_iter()
                .flatten()
                .filter_map(|edge| {
                    support_by_pin
                        .get(&(edge.from_node, edge.from_pin.clone()))
                        .copied()
                })
                .max()
                .unwrap_or(zero);
            let local = level_influence(node.definition.spatial, output_level);
            for output in &node.outputs {
                let module = node
                    .module
                    .as_deref()
                    .and_then(|unit| unit.output_halo_by_level.get(&output.name))
                    .map_or(zero, |support| support[usize::from(output_level)]);
                let support = upstream.checked_add(local)?.checked_add(module)?;
                support_by_pin.insert((node.definition.guid, output.name.clone()), support);
            }
        }
        for output in outputs {
            let support = support_by_pin
                .get(&(output.node, output.pin.clone()))
                .copied()
                .ok_or_else(|| graph_document("graph.outputs", "output support is missing"))?;
            result.get_mut(&output.name).ok_or_else(|| {
                graph_document("graph.outputs", "output support table is missing")
            })?[usize::from(output_level)] = support;
        }
    }
    Ok(result)
}

pub(super) fn demanded_output_halo(
    root: &CompiledGraphUnit,
    demand: &CompiledDemandSlice,
    outputs: &[QualifiedGraphPin],
) -> Result<[DecisionScalar; 63]> {
    let mut result = [DecisionScalar::from_bits(0); 63];
    for output_level in 0_u8..=62 {
        let mut memo = BTreeMap::new();
        let mut visiting = BTreeSet::new();
        for pin in outputs {
            result[usize::from(output_level)] = result[usize::from(output_level)].max(
                demanded_pin_halo(root, demand, pin, output_level, &mut memo, &mut visiting)?,
            );
        }
    }
    Ok(result)
}

pub(super) fn enforce_demanded_halo_policies(
    root: &CompiledGraphUnit,
    demand: &CompiledDemandSlice,
) -> Result<()> {
    for (module_path, unit_demand) in &demand.units {
        let unit = compiled_unit_at_path(root, module_path)?;
        for output in unit
            .outputs
            .iter()
            .filter(|output| unit_demand.outputs.contains(&output.name))
        {
            let source = unit
                .nodes
                .iter()
                .find(|node| node.definition.guid == output.node)
                .ok_or_else(|| {
                    graph_document("graph.demand.halo", "unit output source is missing")
                })?;
            let pin = QualifiedGraphPin {
                node: source.address(),
                pin: output.pin.clone(),
            };
            for output_level in 0_u8..=62 {
                let support = demanded_pin_halo(
                    root,
                    demand,
                    &pin,
                    output_level,
                    &mut BTreeMap::new(),
                    &mut BTreeSet::new(),
                )?;
                if support > unit.maximum_influence_radius {
                    return Err(Error::GraphLimit {
                        resource: "composed influence radius",
                        requested: u64::try_from(support.bits()).unwrap_or(u64::MAX),
                        limit: u64::try_from(unit.maximum_influence_radius.bits()).unwrap_or(0),
                    });
                }
            }
        }
    }
    Ok(())
}

fn demanded_pin_halo(
    root: &CompiledGraphUnit,
    demand: &CompiledDemandSlice,
    pin: &QualifiedGraphPin,
    output_level: u8,
    memo: &mut BTreeMap<QualifiedGraphPin, DecisionScalar>,
    visiting: &mut BTreeSet<QualifiedGraphPin>,
) -> Result<DecisionScalar> {
    if let Some(support) = memo.get(pin).copied() {
        return Ok(support);
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
        .ok_or_else(|| graph_document("graph.demand.halo", "live output node is missing"))?;
    let mut upstream = DecisionScalar::from_bits(0);
    for (index, source) in demanded_upstream_pins(root, demand, pin, "graph.demand.halo")?
        .into_iter()
        .enumerate()
    {
        let support = demanded_pin_halo(root, demand, &source, output_level, memo, visiting)?;
        upstream = if index == 0 {
            support
        } else {
            upstream.max(support)
        };
    }
    let support = upstream.checked_add(level_influence(node.definition.spatial, output_level))?;
    visiting.remove(pin);
    memo.insert(pin.clone(), support);
    Ok(support)
}

/// Local support a node adds at `output_level`; a coarser or global stage adds none.
fn level_influence(spatial: NodeSpatialPolicy, output_level: u8) -> DecisionScalar {
    match spatial {
        NodeSpatialPolicy::Partitioned {
            level,
            influence_radius,
        } if level >= output_level => influence_radius,
        _ => DecisionScalar::from_bits(0),
    }
}
