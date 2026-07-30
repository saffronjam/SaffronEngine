//! Typed graph input and parameter accessors.

use super::*;

use std::collections::BTreeMap;

use saffron_spatial::{DecisionScalar, FieldChannel, FieldDerivative, UnitInterval};

use crate::{
    CompiledGraphNode, Error, GraphClusterMode, GraphCombineOperation, GraphDistanceSource,
    GraphDomain, GraphParameterValue, Result,
};

pub(super) fn singleton(name: &str, value: GraphValue) -> BTreeMap<String, GraphValue> {
    BTreeMap::from([(name.to_owned(), value)])
}

pub(super) fn candidates_input<'a>(
    inputs: &'a BTreeMap<String, GraphValue>,
    name: &str,
) -> Result<&'a CandidateStream> {
    match inputs.get(name) {
        Some(GraphValue::Candidates(value)) => Ok(value),
        _ => missing_input(name, GraphDomain::Candidates),
    }
}

pub(super) fn optional_candidates_input<'a>(
    inputs: &'a BTreeMap<String, GraphValue>,
    name: &str,
) -> Result<Option<&'a CandidateStream>> {
    match inputs.get(name) {
        Some(GraphValue::Candidates(value)) => Ok(Some(value)),
        Some(_) => missing_input(name, GraphDomain::Candidates),
        None => Ok(None),
    }
}

pub(super) fn scalar_input<'a>(
    inputs: &'a BTreeMap<String, GraphValue>,
    name: &str,
) -> Result<&'a ScalarFieldSamples> {
    match inputs.get(name) {
        Some(GraphValue::Scalar(value)) => Ok(value),
        _ => missing_input(name, GraphDomain::ScalarField),
    }
}

pub(super) fn optional_scalar_input<'a>(
    inputs: &'a BTreeMap<String, GraphValue>,
    name: &str,
) -> Result<Option<&'a ScalarFieldSamples>> {
    match inputs.get(name) {
        Some(GraphValue::Scalar(value)) => Ok(Some(value)),
        Some(_) => missing_input(name, GraphDomain::ScalarField),
        None => Ok(None),
    }
}

pub(super) fn optional_vector_input<'a>(
    inputs: &'a BTreeMap<String, GraphValue>,
    name: &str,
) -> Result<Option<&'a VectorFieldSamples>> {
    match inputs.get(name) {
        Some(GraphValue::Vector(value)) => Ok(Some(value)),
        Some(_) => missing_input(name, GraphDomain::VectorField),
        None => Ok(None),
    }
}

pub(super) fn optional_surface_input<'a>(
    inputs: &'a BTreeMap<String, GraphValue>,
    name: &str,
) -> Result<Option<&'a ProjectedSurfaceSamples>> {
    match inputs.get(name) {
        Some(GraphValue::Surface(value)) => Ok(Some(value)),
        Some(_) => missing_input(name, GraphDomain::SurfaceField),
        None => Ok(None),
    }
}

pub(super) fn regions_input<'a>(
    inputs: &'a BTreeMap<String, GraphValue>,
    name: &str,
) -> Result<&'a [EvaluationRegion]> {
    match inputs.get(name) {
        Some(GraphValue::Regions(value)) => Ok(value),
        _ => missing_input(name, GraphDomain::Regions),
    }
}

pub(super) fn splines_input<'a>(
    inputs: &'a BTreeMap<String, GraphValue>,
    name: &str,
) -> Result<&'a [EvaluationSpline]> {
    match inputs.get(name) {
        Some(GraphValue::Splines(value)) => Ok(value),
        _ => missing_input(name, GraphDomain::Splines),
    }
}

pub(super) fn species_input<'a>(
    inputs: &'a BTreeMap<String, GraphValue>,
    name: &str,
) -> Result<&'a [crate::BiomePaletteEntry]> {
    match inputs.get(name) {
        Some(GraphValue::Species(value)) => Ok(value),
        _ => missing_input(name, GraphDomain::SpeciesTable),
    }
}

pub(super) fn communities_input<'a>(
    inputs: &'a BTreeMap<String, GraphValue>,
    name: &str,
) -> Result<&'a CommunityTables> {
    match inputs.get(name) {
        Some(GraphValue::Communities(value)) => Ok(value),
        _ => missing_input(name, GraphDomain::CommunityTable),
    }
}

fn missing_input<T>(name: &str, domain: GraphDomain) -> Result<T> {
    Err(Error::GraphDocument {
        path: format!("input.{name}"),
        reason: format!("expected {}", domain.as_wire()),
    })
}

pub(super) fn u32_parameter(node: &CompiledGraphNode, name: &str, fallback: u32) -> Result<u32> {
    match node.definition.parameter(name) {
        Some(GraphParameterValue::U32(value)) => Ok(*value),
        Some(_) => wrong_parameter(node, name),
        None => Ok(fallback),
    }
}

pub(super) fn u64_parameter(node: &CompiledGraphNode, name: &str, fallback: u64) -> Result<u64> {
    match node.definition.parameter(name) {
        Some(GraphParameterValue::U64(value)) => Ok(*value),
        Some(_) => wrong_parameter(node, name),
        None => Ok(fallback),
    }
}

pub(super) fn guid_parameter(node: &CompiledGraphNode, name: &str, fallback: u128) -> Result<u128> {
    match node.definition.parameter(name) {
        Some(GraphParameterValue::Guid(value)) => Ok(*value),
        Some(_) => wrong_parameter(node, name),
        None => Ok(fallback),
    }
}

pub(super) fn bool_parameter(node: &CompiledGraphNode, name: &str, fallback: bool) -> Result<bool> {
    match node.definition.parameter(name) {
        Some(GraphParameterValue::Boolean(value)) => Ok(*value),
        Some(_) => wrong_parameter(node, name),
        None => Ok(fallback),
    }
}

pub(super) fn fixed_parameter(
    node: &CompiledGraphNode,
    name: &str,
    fallback: DecisionScalar,
) -> Result<DecisionScalar> {
    match node.definition.parameter(name) {
        Some(GraphParameterValue::Fixed(value)) => Ok(*value),
        Some(_) => wrong_parameter(node, name),
        None => Ok(fallback),
    }
}

pub(super) fn unit_parameter(
    node: &CompiledGraphNode,
    name: &str,
    fallback: UnitInterval,
) -> Result<UnitInterval> {
    match node.definition.parameter(name) {
        Some(GraphParameterValue::Unit(value)) => Ok(*value),
        Some(_) => wrong_parameter(node, name),
        None => Ok(fallback),
    }
}

pub(super) fn fixed_vec3_parameter(
    node: &CompiledGraphNode,
    name: &str,
    fallback: [DecisionScalar; 3],
) -> Result<[DecisionScalar; 3]> {
    match node.definition.parameter(name) {
        Some(GraphParameterValue::FixedVec3(value)) => Ok(*value),
        Some(_) => wrong_parameter(node, name),
        None => Ok(fallback),
    }
}

pub(super) fn world_position_parameter(node: &CompiledGraphNode, name: &str) -> Result<[i128; 3]> {
    match node.definition.parameter(name) {
        Some(GraphParameterValue::WorldPosition(value)) => Ok(*value),
        _ => wrong_parameter(node, name),
    }
}

pub(super) fn u32_vec3_parameter(node: &CompiledGraphNode, name: &str) -> Result<[u32; 3]> {
    match node.definition.parameter(name) {
        Some(GraphParameterValue::U32Vec3(value)) => Ok(*value),
        _ => wrong_parameter(node, name),
    }
}

pub(super) fn string_parameter<'a>(
    node: &'a CompiledGraphNode,
    name: &str,
    fallback: &'a str,
) -> Result<&'a str> {
    match node.definition.parameter(name) {
        Some(GraphParameterValue::String(value)) => Ok(value),
        Some(_) => wrong_parameter(node, name),
        None => Ok(fallback),
    }
}

pub(super) fn curve_parameter<'a>(
    node: &'a CompiledGraphNode,
    name: &str,
) -> Result<&'a [(UnitInterval, DecisionScalar)]> {
    match node.definition.parameter(name) {
        Some(GraphParameterValue::Curve(value)) => Ok(value),
        _ => wrong_parameter(node, name),
    }
}

pub(super) fn field_parameter(node: &CompiledGraphNode, name: &str) -> Result<FieldChannel> {
    match node.definition.parameter(name) {
        Some(GraphParameterValue::FieldChannel(value)) => Ok(*value),
        _ => wrong_parameter(node, name),
    }
}

pub(super) fn field_derivative_parameter(
    node: &CompiledGraphNode,
    name: &str,
    fallback: FieldDerivative,
) -> Result<FieldDerivative> {
    match node.definition.parameter(name) {
        Some(GraphParameterValue::FieldDerivative(value)) => Ok(*value),
        Some(_) => wrong_parameter(node, name),
        None => Ok(fallback),
    }
}

pub(super) fn combine_operation_parameter(
    node: &CompiledGraphNode,
    name: &str,
) -> Result<GraphCombineOperation> {
    match node.definition.parameter(name) {
        Some(GraphParameterValue::CombineOperation(value)) => Ok(*value),
        _ => wrong_parameter(node, name),
    }
}

pub(super) fn distance_source_parameter(
    node: &CompiledGraphNode,
    name: &str,
) -> Result<GraphDistanceSource> {
    match node.definition.parameter(name) {
        Some(GraphParameterValue::DistanceSource(value)) => Ok(*value),
        _ => wrong_parameter(node, name),
    }
}

pub(super) fn cluster_mode_parameter(
    node: &CompiledGraphNode,
    name: &str,
) -> Result<GraphClusterMode> {
    match node.definition.parameter(name) {
        Some(GraphParameterValue::ClusterMode(value)) => Ok(*value),
        _ => wrong_parameter(node, name),
    }
}

pub(super) fn tag_list_parameter<'a>(node: &'a CompiledGraphNode, name: &str) -> Result<&'a [u64]> {
    match node.definition.parameter(name) {
        Some(GraphParameterValue::TagList(value)) => Ok(value),
        Some(_) => wrong_parameter(node, name),
        None => Ok(&[]),
    }
}

pub(super) fn guid_list_parameter<'a>(
    node: &'a CompiledGraphNode,
    name: &str,
) -> Result<&'a [u128]> {
    match node.definition.parameter(name) {
        Some(GraphParameterValue::GuidList(value)) => Ok(value),
        Some(_) => wrong_parameter(node, name),
        None => Ok(&[]),
    }
}

pub(super) fn wrong_parameter<T>(node: &CompiledGraphNode, name: &str) -> Result<T> {
    Err(Error::GraphDocument {
        path: format!(
            "graph.nodes.{:032x}.parameters.{name}",
            node.definition.guid
        ),
        reason: "parameter has the wrong type".to_owned(),
    })
}
