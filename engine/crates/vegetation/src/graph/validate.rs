//! Strict node, parameter, spatial-contract, and interface validation.

use std::collections::{BTreeMap, BTreeSet};

use saffron_json::Value;
use saffron_spatial::{DecisionScalar, UnitInterval};

use crate::{BiomeAsset, BiomeParameterType, BiomeRole, Error, Result};

use super::*;

pub(super) fn validate_node_definition(node: &GraphNodeDefinition) -> Result<()> {
    if node.version != BIOME_NODE_VERSION {
        return Err(Error::FormatVersion {
            format: ".sbiome node",
            found: node.version,
            expected: BIOME_NODE_VERSION,
        });
    }
    if node.semantic_revision == 0 {
        return Err(graph_document(
            &format!("graph.nodes.{:032x}.semanticRevision", node.guid),
            "semantic revision must be non-zero",
        ));
    }
    if let NodeSpatialPolicy::Partitioned {
        influence_radius, ..
    } = node.spatial
        && influence_radius.bits() < 0
    {
        return Err(Error::GraphUnboundedInfluence { node: node.guid });
    }
    let schema = node.operator.parameter_schema();
    for descriptor in &schema {
        if descriptor.required && !node.parameters.contains_key(descriptor.name) {
            return Err(graph_document(
                &format!(
                    "graph.nodes.{:032x}.parameters.{}",
                    node.guid, descriptor.name
                ),
                "required parameter is missing",
            ));
        }
    }
    validate_operator_parameters(node)?;
    let known: BTreeSet<_> = schema.iter().map(|parameter| parameter.name).collect();
    if let Some(unknown) = node
        .parameters
        .keys()
        .find(|key| !known.contains(key.as_str()))
    {
        return Err(graph_document(
            &format!("graph.nodes.{:032x}.parameters.{unknown}", node.guid),
            "unknown parameter",
        ));
    }
    for descriptor in &schema {
        if let Some(value) = node.parameters.get(descriptor.name)
            && !parameter_matches_type(value, descriptor.parameter_type)
        {
            return Err(graph_document(
                &format!(
                    "graph.nodes.{:032x}.parameters.{}",
                    node.guid, descriptor.name
                ),
                "parameter value does not match its declared type",
            ));
        }
    }
    let expected_seeds = node.operator.seed_namespace_names();
    if node.seed_namespaces.len() != expected_seeds.len()
        || expected_seeds
            .iter()
            .any(|name| !node.seed_namespaces.contains_key(*name))
    {
        return Err(graph_document(
            &format!("graph.nodes.{:032x}.seedNamespaces", node.guid),
            "seed namespace names do not match the operator's semantic streams",
        ));
    }
    if node
        .seed_namespaces
        .values()
        .any(|namespace| *namespace == 0)
    {
        return Err(graph_document(
            &format!("graph.nodes.{:032x}.seedNamespaces", node.guid),
            "seed namespaces must be non-zero",
        ));
    }
    let unique_seeds: BTreeSet<_> = node.seed_namespaces.values().copied().collect();
    if unique_seeds.len() != node.seed_namespaces.len() {
        return Err(graph_document(
            &format!("graph.nodes.{:032x}.seedNamespaces", node.guid),
            "seed namespaces must be unique",
        ));
    }
    let unique_dependencies: BTreeSet<_> = node.dependencies.iter().copied().collect();
    if unique_dependencies.len() != node.dependencies.len()
        || node.dependencies.iter().any(|dependency| match dependency {
            GraphDependencySource::Asset(id) => id.value() == 0,
            GraphDependencySource::SurfaceProvider(provider) => *provider == 0,
            GraphDependencySource::MapLayer(layer) => *layer == 0,
            GraphDependencySource::Field(_) => false,
        })
    {
        return Err(graph_document(
            &format!("graph.nodes.{:032x}.dependencies", node.guid),
            "dependencies must be unique and non-zero",
        ));
    }
    Ok(())
}

fn validate_operator_parameters(node: &GraphNodeDefinition) -> Result<()> {
    use GraphOperator as O;
    let positive_u32 = |name: &str| -> Result<()> {
        if required_u32(node, name)? == 0 {
            return Err(graph_document(
                &format!("graph.nodes.{:032x}.parameters.{name}", node.guid),
                "value must be positive",
            ));
        }
        Ok(())
    };
    let positive_fixed = |name: &str| -> Result<()> {
        if required_fixed(node, name)?.bits() <= 0 {
            return Err(graph_document(
                &format!("graph.nodes.{:032x}.parameters.{name}", node.guid),
                "value must be positive",
            ));
        }
        Ok(())
    };
    match node.operator {
        O::ExplicitAnchors => {
            if required_guid(node, "layer")? == 0 {
                return Err(graph_document(
                    &format!("graph.nodes.{:032x}.parameters.layer", node.guid),
                    "layer identity must be non-zero",
                ));
            }
        }
        O::StratifiedCoverage => positive_u32("count")?,
        O::BlueNoisePoisson => {
            positive_u32("count")?;
            positive_fixed("radius")?;
            if let Some(GraphParameterValue::U32(attempts)) = node.parameter("attempts")
                && *attempts == 0
            {
                return Err(graph_document(
                    &format!("graph.nodes.{:032x}.parameters.attempts", node.guid),
                    "value must be positive",
                ));
            }
        }
        O::SurfaceProjection => {
            positive_fixed("maxDistance")?;
            let direction = match node.parameter("direction") {
                Some(GraphParameterValue::FixedVec3(direction)) => direction,
                _ => {
                    return Err(graph_document(
                        "graph.nodes.surface-projection",
                        "direction is missing",
                    ));
                }
            };
            if direction.iter().all(|value| value.bits() == 0) {
                return Err(graph_document(
                    &format!("graph.nodes.{:032x}.parameters.direction", node.guid),
                    "direction must be non-zero",
                ));
            }
        }
        O::PaintedTile => {
            if required_guid(node, "layer")? == 0 {
                return Err(graph_document(
                    &format!("graph.nodes.{:032x}.parameters.layer", node.guid),
                    "layer identity must be non-zero",
                ));
            }
        }
        O::Noise => positive_fixed("frequency")?,
        O::Gradient => {
            let direction = match node.parameter("direction") {
                Some(GraphParameterValue::FixedVec3(direction)) => direction,
                _ => {
                    return Err(graph_document(
                        &format!("graph.nodes.{:032x}.parameters.direction", node.guid),
                        "direction is missing",
                    ));
                }
            };
            if direction.iter().all(|value| value.bits() == 0) {
                return Err(graph_document(
                    &format!("graph.nodes.{:032x}.parameters.direction", node.guid),
                    "direction must be non-zero",
                ));
            }
        }
        O::Remap => {
            if required_fixed(node, "inputMin")? >= required_fixed(node, "inputMax")? {
                return Err(graph_document(
                    &format!("graph.nodes.{:032x}.parameters.inputMax", node.guid),
                    "remap input range must be non-empty",
                ));
            }
        }
        O::Clamp => {
            if required_fixed(node, "minimum")? > required_fixed(node, "maximum")? {
                return Err(graph_document(
                    &format!("graph.nodes.{:032x}.parameters.maximum", node.guid),
                    "clamp range is inverted",
                ));
            }
        }
        O::DistanceField => positive_fixed("maximumDistance")?,
        O::WeightedElimination => {
            positive_u32("targetCount")?;
            positive_fixed("eliminationRadius")?;
            positive_u32("maximumNeighbours")?;
        }
        O::ClusterPatchColony => {
            positive_u32("children")?;
            positive_fixed("radius")?;
        }
        O::SplineFollow => positive_fixed("spacing")?,
        O::RecursiveCompanion => {
            positive_u32("children")?;
            positive_fixed("radius")?;
            positive_u32("maximumDepth")?;
        }
        O::Transform => {
            let minimum =
                optional_fixed(node, "scaleMinimum")?.unwrap_or(DecisionScalar::from_bits(65_536));
            let maximum =
                optional_fixed(node, "scaleMaximum")?.unwrap_or(DecisionScalar::from_bits(65_536));
            if minimum.bits() <= 0 || minimum > maximum {
                return Err(graph_document(
                    &format!("graph.nodes.{:032x}.parameters.scaleMaximum", node.guid),
                    "transform scale range is invalid",
                ));
            }
        }
        O::BoundsOverlap => {
            if optional_fixed(node, "padding")?.is_some_and(|padding| padding.bits() < 0) {
                return Err(graph_document(
                    &format!("graph.nodes.{:032x}.parameters.padding", node.guid),
                    "bounds padding cannot be negative",
                ));
            }
        }
        O::Competition => {
            let crown = match node.parameter("crownWeight") {
                Some(GraphParameterValue::Unit(value)) => *value,
                _ => UnitInterval::ZERO,
            };
            let root = match node.parameter("rootWeight") {
                Some(GraphParameterValue::Unit(value)) => *value,
                _ => UnitInterval::ZERO,
            };
            if crown == UnitInterval::ZERO && root == UnitInterval::ZERO {
                return Err(graph_document(
                    &format!("graph.nodes.{:032x}.parameters.crownWeight", node.guid),
                    "competition crown and root weights cannot both be zero",
                ));
            }
        }
        O::MicroOutput => {
            let dimensions = required_u32_vec3(node, "dimensions")?;
            if dimensions.contains(&0) {
                return Err(graph_document(
                    &format!("graph.nodes.{:032x}.parameters.dimensions", node.guid),
                    "micro dimensions must be positive",
                ));
            }
            let channels = match node.parameter("attributeChannels") {
                Some(GraphParameterValue::GuidList(channels)) => channels,
                None => return Ok(()),
                Some(_) => {
                    return Err(graph_document(
                        &format!(
                            "graph.nodes.{:032x}.parameters.attributeChannels",
                            node.guid
                        ),
                        "attribute channels must be a GUID list",
                    ));
                }
            };
            let unique = channels.iter().copied().collect::<BTreeSet<_>>();
            if unique.len() != channels.len() || unique.contains(&0) {
                return Err(graph_document(
                    &format!(
                        "graph.nodes.{:032x}.parameters.attributeChannels",
                        node.guid
                    ),
                    "attribute channels must be unique and non-zero",
                ));
            }
        }
        _ => {}
    }
    Ok(())
}

fn parameter_matches_type(value: &GraphParameterValue, expected: GraphParameterType) -> bool {
    matches!(
        (value, expected),
        (GraphParameterValue::Boolean(_), GraphParameterType::Boolean)
            | (GraphParameterValue::U32(_), GraphParameterType::U32)
            | (GraphParameterValue::U64(_), GraphParameterType::U64)
            | (GraphParameterValue::U32Vec3(_), GraphParameterType::U32Vec3)
            | (GraphParameterValue::Guid(_), GraphParameterType::Guid)
            | (GraphParameterValue::Asset(_), GraphParameterType::Asset)
            | (GraphParameterValue::Fixed(_), GraphParameterType::Fixed)
            | (GraphParameterValue::Unit(_), GraphParameterType::Unit)
            | (
                GraphParameterValue::FixedVec3(_),
                GraphParameterType::FixedVec3
            )
            | (
                GraphParameterValue::WorldPosition(_),
                GraphParameterType::WorldPosition
            )
            | (
                GraphParameterValue::FieldChannel(_),
                GraphParameterType::FieldChannel
            )
            | (
                GraphParameterValue::FieldDerivative(_),
                GraphParameterType::FieldDerivative
            )
            | (
                GraphParameterValue::CombineOperation(_),
                GraphParameterType::CombineOperation
            )
            | (
                GraphParameterValue::DistanceSource(_),
                GraphParameterType::DistanceSource
            )
            | (
                GraphParameterValue::ClusterMode(_),
                GraphParameterType::ClusterMode
            )
            | (GraphParameterValue::Curve(_), GraphParameterType::Curve)
            | (GraphParameterValue::String(_), GraphParameterType::String)
            | (
                GraphParameterValue::GuidList(_),
                GraphParameterType::GuidList
            )
            | (GraphParameterValue::TagList(_), GraphParameterType::TagList)
    )
}

pub(super) fn validate_spatial_contract(
    node: &GraphNodeDefinition,
    asset: &BiomeAsset,
) -> Result<()> {
    if node.operator.spatial_requirement() == GraphSpatialRequirement::Propagating
        && matches!(node.spatial, NodeSpatialPolicy::Partitioned { .. })
    {
        return Err(Error::GraphUnboundedInfluence { node: node.guid });
    }
    let NodeSpatialPolicy::Partitioned {
        influence_radius, ..
    } = node.spatial
    else {
        return Ok(());
    };
    if influence_radius > asset.policy.maximum_influence_radius {
        return Err(Error::GraphLimit {
            resource: "node influence radius",
            requested: u64::try_from(influence_radius.bits()).unwrap_or(u64::MAX),
            limit: u64::try_from(asset.policy.maximum_influence_radius.bits()).unwrap_or(0),
        });
    }
    let parameter_radius = match node.operator {
        GraphOperator::BlueNoisePoisson | GraphOperator::ClusterPatchColony => {
            required_fixed(node, "radius")?
        }
        GraphOperator::WeightedElimination => required_fixed(node, "eliminationRadius")?,
        GraphOperator::SurfaceProjection => required_fixed(node, "maxDistance")?,
        GraphOperator::DistanceField => required_fixed(node, "maximumDistance")?,
        GraphOperator::SplineFollow => {
            fixed_abs(optional_fixed(node, "edgeOffset")?.unwrap_or(DecisionScalar::from_bits(0)))?
        }
        GraphOperator::RecursiveCompanion => {
            let authored = required_fixed(node, "radius")?;
            let per_generation = asset
                .companions
                .iter()
                .map(|rule| rule.maximum_distance)
                .fold(authored, DecisionScalar::max);
            fixed_mul_u32(per_generation, required_u32(node, "maximumDepth")?)?
        }
        GraphOperator::BoundsOverlap => {
            fixed_abs(optional_fixed(node, "padding")?.unwrap_or(DecisionScalar::from_bits(0)))?
        }
        _ => DecisionScalar::from_bits(0),
    };
    if parameter_radius.bits() < 0 || parameter_radius > influence_radius {
        return Err(Error::GraphUnboundedInfluence { node: node.guid });
    }
    if matches!(
        node.operator,
        GraphOperator::VariableSpacing
            | GraphOperator::PriorityExclusion
            | GraphOperator::Competition
            | GraphOperator::BoundsOverlap
    ) && influence_radius.bits() <= 0
    {
        return Err(Error::GraphUnboundedInfluence { node: node.guid });
    }
    Ok(())
}

fn fixed_abs(value: DecisionScalar) -> Result<DecisionScalar> {
    Ok(DecisionScalar::from_bits(
        value.bits().checked_abs().ok_or(Error::NumericOverflow)?,
    ))
}

fn fixed_mul_u32(value: DecisionScalar, factor: u32) -> Result<DecisionScalar> {
    let bits = i64::from(value.bits())
        .checked_mul(i64::from(factor))
        .ok_or(Error::NumericOverflow)?;
    Ok(DecisionScalar::from_bits(
        i32::try_from(bits).map_err(|_| Error::NumericOverflow)?,
    ))
}

pub(super) fn validate_interface(document: &BiomeGraphDocument, role: BiomeRole) -> Result<()> {
    let input_names: BTreeSet<_> = document
        .inputs
        .iter()
        .map(|input| input.name.as_str())
        .collect();
    let output_names: BTreeSet<_> = document
        .outputs
        .iter()
        .map(|output| output.name.as_str())
        .collect();
    let pin_ids = document
        .inputs
        .iter()
        .map(|input| input.id)
        .chain(document.outputs.iter().map(|output| output.id))
        .collect::<BTreeSet<_>>();
    if input_names.len() != document.inputs.len()
        || output_names.len() != document.outputs.len()
        || pin_ids.len() != document.inputs.len() + document.outputs.len()
        || pin_ids.contains(&0)
        || input_names.contains("")
        || output_names.contains("")
        || document.outputs.iter().any(|output| output.pin.is_empty())
    {
        return Err(graph_document(
            "graph.interface",
            "interface pin IDs and names must be non-zero, non-empty, and unique",
        ));
    }
    let interface_nodes = document
        .nodes
        .iter()
        .filter(|node| node.operator == GraphOperator::InterfaceInput)
        .collect::<Vec<_>>();
    if role == BiomeRole::Root {
        if !document.inputs.is_empty() || !interface_nodes.is_empty() {
            return Err(graph_document(
                "graph.inputs",
                "root graphs cannot declare module interface inputs",
            ));
        }
        if document.outputs.iter().any(|output| output.sink.is_none()) {
            return Err(graph_document(
                "graph.outputs.sink",
                "root outputs require an authority sink",
            ));
        }
    }
    if role == BiomeRole::Module {
        if document.outputs.iter().any(|output| output.sink.is_some()) {
            return Err(graph_document(
                "graph.outputs.sink",
                "module outputs cannot declare root sinks",
            ));
        }
        let node_inputs = interface_nodes
            .iter()
            .map(|node| match node.parameter("name") {
                Some(GraphParameterValue::String(name)) if !name.is_empty() => Ok(name.as_str()),
                _ => Err(graph_document(
                    &format!("graph.nodes.{:032x}.name", node.guid),
                    "interface input requires a non-empty name",
                )),
            })
            .collect::<Result<Vec<_>>>()?;
        let unique_node_inputs = node_inputs.iter().copied().collect::<BTreeSet<_>>();
        if node_inputs.len() != document.inputs.len() || unique_node_inputs != input_names {
            return Err(graph_document(
                "graph.inputs",
                "every module input requires exactly one matching interface-input node",
            ));
        }
    }
    Ok(())
}

pub(super) fn validate_parameter_bindings(
    asset: &BiomeAsset,
    bindings: &[(u128, Value)],
) -> Result<()> {
    let parameters: BTreeSet<_> = asset
        .parameters
        .iter()
        .map(|parameter| parameter.id)
        .collect();
    let mut seen = BTreeSet::new();
    for (parameter, _value) in bindings {
        if !parameters.contains(parameter) || !seen.insert(*parameter) {
            return Err(graph_document(
                "biome.modules.bindings",
                "binding target is unknown or duplicated",
            ));
        }
    }
    Ok(())
}

pub(super) fn resolve_parameter_bindings(
    document: &mut BiomeGraphDocument,
    asset: &BiomeAsset,
    bindings: &[(u128, Value)],
) -> Result<()> {
    validate_parameter_bindings(asset, bindings)?;
    let parameters = asset
        .parameters
        .iter()
        .map(|parameter| (parameter.id, parameter))
        .collect::<BTreeMap<_, _>>();
    let supplied = bindings.iter().cloned().collect::<BTreeMap<_, _>>();
    for node in &mut document.nodes {
        let schema = node
            .operator
            .parameter_schema()
            .into_iter()
            .map(|parameter| (parameter.name, parameter.parameter_type))
            .collect::<BTreeMap<_, _>>();
        for (name, value) in &mut node.parameters {
            let GraphParameterValue::Binding(parameter_id) = value else {
                continue;
            };
            let parameter = parameters.get(parameter_id).ok_or_else(|| {
                graph_document(
                    &format!("graph.nodes.{:032x}.parameters.{name}", node.guid),
                    "binding references an unknown biome parameter",
                )
            })?;
            let expected = schema[name.as_str()];
            if !biome_parameter_matches_graph_type(parameter.parameter_type, expected) {
                return Err(graph_document(
                    &format!("graph.nodes.{:032x}.parameters.{name}", node.guid),
                    "biome parameter type is incompatible with the node parameter",
                ));
            }
            let raw = supplied
                .get(parameter_id)
                .unwrap_or(&parameter.default_value);
            *value = parse_parameter_value(
                raw,
                expected,
                &format!("graph.nodes.{:032x}.parameters.{name}", node.guid),
            )?;
        }
    }
    Ok(())
}

fn biome_parameter_matches_graph_type(
    parameter: BiomeParameterType,
    graph: GraphParameterType,
) -> bool {
    matches!(
        (parameter, graph),
        (BiomeParameterType::Scalar, GraphParameterType::Fixed)
            | (BiomeParameterType::Vector, GraphParameterType::FixedVec3)
            | (BiomeParameterType::Unit, GraphParameterType::Unit)
            | (BiomeParameterType::Plant, GraphParameterType::Asset)
            | (BiomeParameterType::Field, GraphParameterType::FieldChannel)
            | (BiomeParameterType::Boolean, GraphParameterType::Boolean)
    )
}

pub(super) fn required_u32(node: &GraphNodeDefinition, name: &str) -> Result<u32> {
    match node.parameter(name) {
        Some(GraphParameterValue::U32(value)) => Ok(*value),
        _ => Err(graph_document(
            &format!("graph.nodes.{:032x}.parameters.{name}", node.guid),
            "expected u32",
        )),
    }
}

pub(super) fn required_guid(node: &GraphNodeDefinition, name: &str) -> Result<u128> {
    match node.parameter(name) {
        Some(GraphParameterValue::Guid(value)) => Ok(*value),
        _ => Err(graph_document(
            &format!("graph.nodes.{:032x}.parameters.{name}", node.guid),
            "expected GUID",
        )),
    }
}

pub(super) fn required_fixed(node: &GraphNodeDefinition, name: &str) -> Result<DecisionScalar> {
    match node.parameter(name) {
        Some(GraphParameterValue::Fixed(value)) => Ok(*value),
        _ => Err(graph_document(
            &format!("graph.nodes.{:032x}.parameters.{name}", node.guid),
            "expected fixed scalar",
        )),
    }
}

pub(super) fn optional_fixed(
    node: &GraphNodeDefinition,
    name: &str,
) -> Result<Option<DecisionScalar>> {
    match node.parameter(name) {
        Some(GraphParameterValue::Fixed(value)) => Ok(Some(*value)),
        None => Ok(None),
        _ => Err(graph_document(
            &format!("graph.nodes.{:032x}.parameters.{name}", node.guid),
            "expected fixed scalar",
        )),
    }
}

pub(super) fn required_u32_vec3(node: &GraphNodeDefinition, name: &str) -> Result<[u32; 3]> {
    match node.parameter(name) {
        Some(GraphParameterValue::U32Vec3(value)) => Ok(*value),
        _ => Err(graph_document(
            &format!("graph.nodes.{:032x}.parameters.{name}", node.guid),
            "expected unsigned vector",
        )),
    }
}
