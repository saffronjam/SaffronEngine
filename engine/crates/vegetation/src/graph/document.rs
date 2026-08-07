//! The authored `.sbiome.graph` document and its canonical JSON form.

use std::collections::BTreeMap;

use saffron_core::Uuid;
use saffron_json::{Map, Value};
use saffron_spatial::{DecisionScalar, UnitInterval};

use crate::hash::sha256;
use crate::{Error, Result};

use super::*;

/// One typed node in an authored graph document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphNodeDefinition {
    pub guid: u128,
    /// Operator schema version.
    pub version: u32,
    /// Revision changed only when this node's semantics/configuration change.
    pub semantic_revision: u32,
    pub operator: GraphOperator,
    /// Declared authority class.
    pub authority: GraphAuthority,
    /// Spatial stage and finite support policy.
    pub spatial: NodeSpatialPolicy,
    /// Immutable invalidation dependencies.
    pub dependencies: Vec<GraphDependencySource>,
    /// Named/domain-separated random namespaces.
    pub seed_namespaces: BTreeMap<String, u128>,
    pub parameters: BTreeMap<String, GraphParameterValue>,
}

impl GraphNodeDefinition {
    /// Looks up one typed parameter.
    #[must_use]
    pub fn parameter(&self, name: &str) -> Option<&GraphParameterValue> {
        self.parameters.get(name)
    }

    /// Canonical current-schema bytes used by execution plans and cache identities.
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        saffron_json::dump_json_sorted(&node_to_json(self), -1).into_bytes()
    }
}

/// One directed typed edge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphEdge {
    pub from_node: u128,
    /// Source output pin.
    pub from_pin: String,
    pub to_node: u128,
    /// Destination input pin.
    pub to_pin: String,
}

/// One public module input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphInterfaceInput {
    /// Stable interface pin identity.
    pub id: u128,
    /// Stable interface pin name.
    pub name: String,
    pub domain: GraphDomain,
}

/// One public graph output and its source pin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphInterfaceOutput {
    /// Stable interface pin identity.
    pub id: u128,
    /// Stable interface output name.
    pub name: String,
    pub domain: GraphDomain,
    /// Source node.
    pub node: u128,
    /// Source pin.
    pub pin: String,
    /// Root authority sink; module outputs leave this `None`.
    pub sink: Option<GraphSink>,
}

/// One typed biome graph document stored in `.sbiome.graph`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BiomeGraphDocument {
    pub version: u32,
    /// Strict public-interface schema version.
    pub interface_version: u32,
    pub inputs: Vec<GraphInterfaceInput>,
    pub outputs: Vec<GraphInterfaceOutput>,
    /// Nodes in arbitrary authored order.
    pub nodes: Vec<GraphNodeDefinition>,
    /// Directed typed edges in arbitrary authored order.
    pub edges: Vec<GraphEdge>,
}

impl BiomeGraphDocument {
    /// Reads and validates the typed document shape from `.sbiome.graph` JSON.
    pub fn from_json(value: &Value) -> Result<Self> {
        let object = value
            .as_object()
            .ok_or_else(|| graph_document("graph", "expected object"))?;
        reject_unknown(
            object,
            &[
                "version",
                "interfaceVersion",
                "inputs",
                "outputs",
                "nodes",
                "edges",
            ],
            "graph",
        )?;
        let version = read_u32(object.get("version"), "graph.version")?;
        if version != BIOME_GRAPH_VERSION {
            return Err(Error::FormatVersion {
                format: ".sbiome graph",
                found: version,
                expected: BIOME_GRAPH_VERSION,
            });
        }
        let interface_version = read_u32(object.get("interfaceVersion"), "graph.interfaceVersion")?;
        if interface_version != BIOME_INTERFACE_VERSION {
            return Err(Error::FormatVersion {
                format: ".sbiome interface",
                found: interface_version,
                expected: BIOME_INTERFACE_VERSION,
            });
        }
        let inputs = read_array(object.get("inputs"), "graph.inputs")?
            .iter()
            .enumerate()
            .map(|(index, value)| parse_interface_input(value, index))
            .collect::<Result<Vec<_>>>()?;
        let outputs = read_array(object.get("outputs"), "graph.outputs")?
            .iter()
            .enumerate()
            .map(|(index, value)| parse_interface_output(value, index))
            .collect::<Result<Vec<_>>>()?;
        let nodes = read_array(object.get("nodes"), "graph.nodes")?
            .iter()
            .enumerate()
            .map(|(index, value)| parse_node(value, index))
            .collect::<Result<Vec<_>>>()?;
        let edges = read_array(object.get("edges"), "graph.edges")?
            .iter()
            .enumerate()
            .map(|(index, value)| parse_edge(value, index))
            .collect::<Result<Vec<_>>>()?;
        Ok(Self {
            version,
            interface_version,
            inputs,
            outputs,
            nodes,
            edges,
        })
    }

    /// Emits the one canonical typed JSON shape used by `.sbiome` authoring.
    #[must_use]
    pub fn to_json(&self) -> Value {
        let inputs = self
            .inputs
            .iter()
            .map(|input| {
                object([
                    ("id", Value::String(guid_text(input.id))),
                    ("name", Value::String(input.name.clone())),
                    ("domain", Value::String(input.domain.as_wire().to_owned())),
                ])
            })
            .collect();
        let outputs = self
            .outputs
            .iter()
            .map(|output| {
                let mut fields = vec![
                    ("id", Value::String(guid_text(output.id))),
                    ("name", Value::String(output.name.clone())),
                    ("domain", Value::String(output.domain.as_wire().to_owned())),
                    ("node", Value::String(guid_text(output.node))),
                    ("pin", Value::String(output.pin.clone())),
                ];
                if let Some(sink) = output.sink {
                    fields.push(("sink", Value::String(sink.as_wire().to_owned())));
                }
                object(fields)
            })
            .collect();
        let nodes = self.nodes.iter().map(node_to_json).collect();
        let edges = self
            .edges
            .iter()
            .map(|edge| {
                object([
                    ("fromNode", Value::String(guid_text(edge.from_node))),
                    ("fromPin", Value::String(edge.from_pin.clone())),
                    ("toNode", Value::String(guid_text(edge.to_node))),
                    ("toPin", Value::String(edge.to_pin.clone())),
                ])
            })
            .collect();
        object([
            ("version", Value::from(self.version)),
            ("interfaceVersion", Value::from(self.interface_version)),
            ("inputs", Value::Array(inputs)),
            ("outputs", Value::Array(outputs)),
            ("nodes", Value::Array(nodes)),
            ("edges", Value::Array(edges)),
        ])
    }

    /// Canonical content identity independent of authored object key order.
    #[must_use]
    pub fn identity(&self) -> [u8; 32] {
        let text = saffron_json::dump_json_sorted(&self.to_json(), -1);
        sha256(text.as_bytes())
    }
}

fn parse_interface_input(value: &Value, index: usize) -> Result<GraphInterfaceInput> {
    let path = format!("graph.inputs[{index}]");
    let object = value
        .as_object()
        .ok_or_else(|| graph_document(&path, "expected object"))?;
    reject_unknown(object, &["id", "name", "domain"], &path)?;
    Ok(GraphInterfaceInput {
        id: read_guid(object.get("id"), &format!("{path}.id"))?,
        name: read_string(object.get("name"), &format!("{path}.name"))?,
        domain: read_domain(object.get("domain"), &format!("{path}.domain"))?,
    })
}

fn parse_interface_output(value: &Value, index: usize) -> Result<GraphInterfaceOutput> {
    let path = format!("graph.outputs[{index}]");
    let object = value
        .as_object()
        .ok_or_else(|| graph_document(&path, "expected object"))?;
    reject_unknown(
        object,
        &["id", "name", "domain", "node", "pin", "sink"],
        &path,
    )?;
    let sink = object
        .get("sink")
        .map(|value| {
            let text = value
                .as_str()
                .ok_or_else(|| graph_document(&format!("{path}.sink"), "expected string"))?;
            GraphSink::from_wire(text)
                .ok_or_else(|| graph_document(&format!("{path}.sink"), "unknown sink"))
        })
        .transpose()?;
    Ok(GraphInterfaceOutput {
        id: read_guid(object.get("id"), &format!("{path}.id"))?,
        name: read_string(object.get("name"), &format!("{path}.name"))?,
        domain: read_domain(object.get("domain"), &format!("{path}.domain"))?,
        node: read_guid(object.get("node"), &format!("{path}.node"))?,
        pin: read_string(object.get("pin"), &format!("{path}.pin"))?,
        sink,
    })
}

fn parse_node(value: &Value, index: usize) -> Result<GraphNodeDefinition> {
    let path = format!("graph.nodes[{index}]");
    let object = value
        .as_object()
        .ok_or_else(|| graph_document(&path, "expected object"))?;
    reject_unknown(
        object,
        &[
            "guid",
            "version",
            "semanticRevision",
            "operator",
            "authority",
            "spatial",
            "dependencies",
            "seedNamespaces",
            "parameters",
        ],
        &path,
    )?;
    let operator_text = read_string(object.get("operator"), &format!("{path}.operator"))?;
    let operator = GraphOperator::from_wire(&operator_text)
        .ok_or_else(|| graph_document(&format!("{path}.operator"), "unknown operator"))?;
    let version = read_u32(object.get("version"), &format!("{path}.version"))?;
    if version != BIOME_NODE_VERSION {
        return Err(Error::FormatVersion {
            format: ".sbiome node",
            found: version,
            expected: BIOME_NODE_VERSION,
        });
    }
    let authority_text = read_string(object.get("authority"), &format!("{path}.authority"))?;
    let authority = GraphAuthority::from_wire(&authority_text)
        .ok_or_else(|| graph_document(&format!("{path}.authority"), "unknown authority"))?;
    let spatial = parse_spatial(object.get("spatial"), &format!("{path}.spatial"))?;
    let dependencies = object
        .get("dependencies")
        .map(|value| parse_dependencies(value, &format!("{path}.dependencies")))
        .transpose()?
        .unwrap_or_default();
    let seed_namespaces = object
        .get("seedNamespaces")
        .map(|value| parse_seed_namespaces(value, &format!("{path}.seedNamespaces")))
        .transpose()?
        .unwrap_or_default();
    let parameters = parse_parameters(
        object.get("parameters"),
        &format!("{path}.parameters"),
        operator,
    )?;
    Ok(GraphNodeDefinition {
        guid: read_guid(object.get("guid"), &format!("{path}.guid"))?,
        version,
        semantic_revision: read_u32(
            object.get("semanticRevision"),
            &format!("{path}.semanticRevision"),
        )?,
        operator,
        authority,
        spatial,
        dependencies,
        seed_namespaces,
        parameters,
    })
}

fn parse_edge(value: &Value, index: usize) -> Result<GraphEdge> {
    let path = format!("graph.edges[{index}]");
    let object = value
        .as_object()
        .ok_or_else(|| graph_document(&path, "expected object"))?;
    reject_unknown(object, &["fromNode", "fromPin", "toNode", "toPin"], &path)?;
    Ok(GraphEdge {
        from_node: read_guid(object.get("fromNode"), &format!("{path}.fromNode"))?,
        from_pin: read_string(object.get("fromPin"), &format!("{path}.fromPin"))?,
        to_node: read_guid(object.get("toNode"), &format!("{path}.toNode"))?,
        to_pin: read_string(object.get("toPin"), &format!("{path}.toPin"))?,
    })
}

fn parse_spatial(value: Option<&Value>, path: &str) -> Result<NodeSpatialPolicy> {
    let object = value
        .and_then(Value::as_object)
        .ok_or_else(|| graph_document(path, "expected object"))?;
    reject_unknown(object, &["scope", "level", "influenceRadiusBits"], path)?;
    let scope = read_string(object.get("scope"), &format!("{path}.scope"))?;
    let level = read_u8(object.get("level"), &format!("{path}.level"))?;
    match scope.as_str() {
        "partitioned" => {
            let radius = read_i32(
                object.get("influenceRadiusBits"),
                &format!("{path}.influenceRadiusBits"),
            )?;
            if radius < 0 {
                return Err(graph_document(path, "influence radius cannot be negative"));
            }
            Ok(NodeSpatialPolicy::Partitioned {
                level,
                influence_radius: DecisionScalar::from_bits(radius),
            })
        }
        "global" => Ok(NodeSpatialPolicy::Global { level }),
        _ => Err(graph_document(&format!("{path}.scope"), "unknown scope")),
    }
}

fn parse_dependencies(value: &Value, path: &str) -> Result<Vec<GraphDependencySource>> {
    read_array(Some(value), path)?
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let item_path = format!("{path}[{index}]");
            let object = value
                .as_object()
                .ok_or_else(|| graph_document(&item_path, "expected object"))?;
            reject_unknown(object, &["kind", "value"], &item_path)?;
            let kind = read_string(object.get("kind"), &format!("{item_path}.kind"))?;
            let value = object.get("value");
            match kind.as_str() {
                "asset" => Ok(GraphDependencySource::Asset(Uuid(read_u64(
                    value,
                    &format!("{item_path}.value"),
                )?))),
                "field" => Ok(GraphDependencySource::Field(read_field_channel(
                    value,
                    &format!("{item_path}.value"),
                )?)),
                "surface-provider" => Ok(GraphDependencySource::SurfaceProvider(read_u64(
                    value,
                    &format!("{item_path}.value"),
                )?)),
                "map-layer" => Ok(GraphDependencySource::MapLayer(read_guid(
                    value,
                    &format!("{item_path}.value"),
                )?)),
                _ => Err(graph_document(
                    &format!("{item_path}.kind"),
                    "unknown dependency",
                )),
            }
        })
        .collect()
}

fn parse_parameters(
    value: Option<&Value>,
    path: &str,
    operator: GraphOperator,
) -> Result<BTreeMap<String, GraphParameterValue>> {
    let empty = Map::new();
    let object = match value {
        Some(value) => value
            .as_object()
            .ok_or_else(|| graph_document(path, "expected object"))?,
        None => &empty,
    };
    let schema = operator.parameter_schema();
    let by_name: BTreeMap<_, _> = schema
        .iter()
        .map(|item| (item.name, item.parameter_type))
        .collect();
    let mut parameters = BTreeMap::new();
    for (name, value) in object {
        let parameter_type = by_name
            .get(name.as_str())
            .copied()
            .ok_or_else(|| graph_document(&format!("{path}.{name}"), "unknown parameter"))?;
        parameters.insert(
            name.clone(),
            parse_parameter_value(value, parameter_type, &format!("{path}.{name}"))?,
        );
    }
    Ok(parameters)
}

pub(super) fn parse_parameter_value(
    value: &Value,
    parameter_type: GraphParameterType,
    path: &str,
) -> Result<GraphParameterValue> {
    if let Some(binding) = value.as_object()
        && binding.len() == 1
        && binding.contains_key("$binding")
    {
        return Ok(GraphParameterValue::Binding(read_guid(
            binding.get("$binding"),
            &format!("{path}.$binding"),
        )?));
    }
    Ok(match parameter_type {
        GraphParameterType::Boolean => GraphParameterValue::Boolean(
            value
                .as_bool()
                .ok_or_else(|| graph_document(path, "expected boolean"))?,
        ),
        GraphParameterType::U32 => GraphParameterValue::U32(read_u32(Some(value), path)?),
        GraphParameterType::U64 => GraphParameterValue::U64(read_u64(Some(value), path)?),
        GraphParameterType::U32Vec3 => {
            let values = read_array(Some(value), path)?;
            if values.len() != 3 {
                return Err(graph_document(path, "expected three unsigned lanes"));
            }
            GraphParameterValue::U32Vec3([
                read_u32(Some(&values[0]), path)?,
                read_u32(Some(&values[1]), path)?,
                read_u32(Some(&values[2]), path)?,
            ])
        }
        GraphParameterType::Guid => GraphParameterValue::Guid(read_guid(Some(value), path)?),
        GraphParameterType::Asset => GraphParameterValue::Asset(Uuid(read_u64(Some(value), path)?)),
        GraphParameterType::Fixed => {
            GraphParameterValue::Fixed(DecisionScalar::from_bits(read_i32(Some(value), path)?))
        }
        GraphParameterType::Unit => {
            GraphParameterValue::Unit(UnitInterval::from_bits(read_u16(Some(value), path)?))
        }
        GraphParameterType::FixedVec3 => {
            let values = read_array(Some(value), path)?;
            if values.len() != 3 {
                return Err(graph_document(path, "expected three fixed-point lanes"));
            }
            GraphParameterValue::FixedVec3([
                DecisionScalar::from_bits(read_i32(Some(&values[0]), path)?),
                DecisionScalar::from_bits(read_i32(Some(&values[1]), path)?),
                DecisionScalar::from_bits(read_i32(Some(&values[2]), path)?),
            ])
        }
        GraphParameterType::WorldPosition => {
            let values = read_array(Some(value), path)?;
            if values.len() != 3 {
                return Err(graph_document(
                    path,
                    "expected three exact world-tick lanes",
                ));
            }
            let parse = |index: usize| {
                values[index]
                    .as_str()
                    .ok_or_else(|| graph_document(path, "expected world ticks as decimal strings"))?
                    .parse::<i128>()
                    .map_err(|_| graph_document(path, "world tick is outside signed 128-bit range"))
            };
            GraphParameterValue::WorldPosition([parse(0)?, parse(1)?, parse(2)?])
        }
        GraphParameterType::FieldChannel => {
            GraphParameterValue::FieldChannel(read_field_channel(Some(value), path)?)
        }
        GraphParameterType::FieldDerivative => {
            let value = value
                .as_str()
                .and_then(field_derivative_from_wire)
                .ok_or_else(|| graph_document(path, "unknown field derivative"))?;
            GraphParameterValue::FieldDerivative(value)
        }
        GraphParameterType::CombineOperation => {
            let value = value
                .as_str()
                .and_then(GraphCombineOperation::from_wire)
                .ok_or_else(|| graph_document(path, "unknown combine operation"))?;
            GraphParameterValue::CombineOperation(value)
        }
        GraphParameterType::DistanceSource => {
            let value = value
                .as_str()
                .and_then(GraphDistanceSource::from_wire)
                .ok_or_else(|| graph_document(path, "unknown distance source"))?;
            GraphParameterValue::DistanceSource(value)
        }
        GraphParameterType::ClusterMode => {
            let value = value
                .as_str()
                .and_then(GraphClusterMode::from_wire)
                .ok_or_else(|| graph_document(path, "unknown cluster mode"))?;
            GraphParameterValue::ClusterMode(value)
        }
        GraphParameterType::Curve => {
            let points = read_array(Some(value), path)?
                .iter()
                .enumerate()
                .map(|(index, point)| {
                    let values = point.as_array().ok_or_else(|| {
                        graph_document(&format!("{path}[{index}]"), "expected [unit, fixed]")
                    })?;
                    if values.len() != 2 {
                        return Err(graph_document(
                            &format!("{path}[{index}]"),
                            "expected [unit, fixed]",
                        ));
                    }
                    Ok((
                        UnitInterval::from_bits(read_u16(Some(&values[0]), path)?),
                        DecisionScalar::from_bits(read_i32(Some(&values[1]), path)?),
                    ))
                })
                .collect::<Result<Vec<_>>>()?;
            saffron_spatial::DecisionCurve::new(points.clone())?;
            GraphParameterValue::Curve(points)
        }
        GraphParameterType::String => GraphParameterValue::String(
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| graph_document(path, "expected string"))?,
        ),
        GraphParameterType::GuidList => {
            GraphParameterValue::GuidList(parse_guid_array(value, path)?)
        }
        GraphParameterType::TagList => GraphParameterValue::TagList(
            read_array(Some(value), path)?
                .iter()
                .map(|value| read_u64(Some(value), path))
                .collect::<Result<Vec<_>>>()?,
        ),
    })
}

fn node_to_json(node: &GraphNodeDefinition) -> Value {
    let dependencies = node
        .dependencies
        .iter()
        .map(|dependency| match dependency {
            GraphDependencySource::Asset(id) => dependency_json("asset", id.value().to_string()),
            GraphDependencySource::Field(channel) => {
                dependency_json("field", field_channel_wire(*channel))
            }
            GraphDependencySource::SurfaceProvider(id) => {
                dependency_json("surface-provider", id.to_string())
            }
            GraphDependencySource::MapLayer(id) => dependency_json("map-layer", guid_text(*id)),
        })
        .collect();
    let spatial = match node.spatial {
        NodeSpatialPolicy::Partitioned {
            level,
            influence_radius,
        } => object([
            ("scope", Value::String("partitioned".to_owned())),
            ("level", Value::from(level)),
            ("influenceRadiusBits", Value::from(influence_radius.bits())),
        ]),
        NodeSpatialPolicy::Global { level } => object([
            ("scope", Value::String("global".to_owned())),
            ("level", Value::from(level)),
        ]),
    };
    let parameters = node
        .parameters
        .iter()
        .map(|(name, value)| (name.clone(), parameter_to_json(value)))
        .collect();
    object([
        ("guid", Value::String(guid_text(node.guid))),
        ("version", Value::from(node.version)),
        ("semanticRevision", Value::from(node.semantic_revision)),
        (
            "operator",
            Value::String(node.operator.as_wire().to_owned()),
        ),
        (
            "authority",
            Value::String(node.authority.as_wire().to_owned()),
        ),
        ("spatial", spatial),
        ("dependencies", Value::Array(dependencies)),
        (
            "seedNamespaces",
            Value::Object(
                node.seed_namespaces
                    .iter()
                    .map(|(name, value)| (name.clone(), Value::String(guid_text(*value))))
                    .collect(),
            ),
        ),
        ("parameters", Value::Object(parameters)),
    ])
}

fn parameter_to_json(value: &GraphParameterValue) -> Value {
    match value {
        GraphParameterValue::Binding(parameter) => {
            object([("$binding", Value::String(guid_text(*parameter)))])
        }
        GraphParameterValue::Boolean(value) => Value::Bool(*value),
        GraphParameterValue::U32(value) => Value::from(*value),
        GraphParameterValue::U64(value) => Value::String(value.to_string()),
        GraphParameterValue::U32Vec3(value) => {
            Value::Array(value.iter().map(|value| Value::from(*value)).collect())
        }
        GraphParameterValue::Guid(value) => Value::String(guid_text(*value)),
        GraphParameterValue::Asset(value) => Value::String(value.value().to_string()),
        GraphParameterValue::Fixed(value) => Value::from(value.bits()),
        GraphParameterValue::Unit(value) => Value::from(value.bits()),
        GraphParameterValue::FixedVec3(value) => Value::Array(
            value
                .iter()
                .map(|value| Value::from(value.bits()))
                .collect(),
        ),
        GraphParameterValue::WorldPosition(value) => Value::Array(
            value
                .iter()
                .map(|value| Value::String(value.to_string()))
                .collect(),
        ),
        GraphParameterValue::FieldChannel(value) => Value::String(field_channel_wire(*value)),
        GraphParameterValue::FieldDerivative(value) => {
            Value::String(field_derivative_wire(*value).to_owned())
        }
        GraphParameterValue::CombineOperation(value) => Value::String(value.as_wire().to_owned()),
        GraphParameterValue::DistanceSource(value) => Value::String(value.as_wire().to_owned()),
        GraphParameterValue::ClusterMode(value) => Value::String(value.as_wire().to_owned()),
        GraphParameterValue::Curve(value) => Value::Array(
            value
                .iter()
                .map(|(x, y)| Value::Array(vec![Value::from(x.bits()), Value::from(y.bits())]))
                .collect(),
        ),
        GraphParameterValue::String(value) => Value::String(value.clone()),
        GraphParameterValue::GuidList(value) => Value::Array(
            value
                .iter()
                .map(|value| Value::String(guid_text(*value)))
                .collect(),
        ),
        GraphParameterValue::TagList(value) => Value::Array(
            value
                .iter()
                .map(|value| Value::String(value.to_string()))
                .collect(),
        ),
    }
}

fn dependency_json(kind: &str, value: String) -> Value {
    object([
        ("kind", Value::String(kind.to_owned())),
        ("value", Value::String(value)),
    ])
}
