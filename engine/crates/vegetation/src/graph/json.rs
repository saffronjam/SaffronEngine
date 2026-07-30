//! JSON reading and canonical-byte primitives shared across the graph modules.

use std::collections::BTreeMap;

use saffron_json::{Map, Value};
use saffron_spatial::{FieldChannel, FieldDerivative};

use crate::{Error, Result};

use super::*;

pub(super) fn object<K>(fields: impl IntoIterator<Item = (K, Value)>) -> Value
where
    K: Into<String>,
{
    Value::Object(
        fields
            .into_iter()
            .map(|(key, value)| (key.into(), value))
            .collect(),
    )
}

pub(super) fn graph_document(path: &str, reason: &str) -> Error {
    Error::GraphDocument {
        path: path.to_owned(),
        reason: reason.to_owned(),
    }
}

pub(super) fn reject_unknown(
    object: &Map<String, Value>,
    allowed: &[&str],
    path: &str,
) -> Result<()> {
    if let Some(key) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(graph_document(&format!("{path}.{key}"), "unknown field"));
    }
    Ok(())
}

pub(super) fn read_array<'a>(value: Option<&'a Value>, path: &str) -> Result<&'a Vec<Value>> {
    value
        .and_then(Value::as_array)
        .ok_or_else(|| graph_document(path, "expected array"))
}

pub(super) fn read_string(value: Option<&Value>, path: &str) -> Result<String> {
    value
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| graph_document(path, "expected string"))
}

pub(super) fn read_u8(value: Option<&Value>, path: &str) -> Result<u8> {
    read_u64(value, path)
        .and_then(|value| u8::try_from(value).map_err(|_| graph_document(path, "u8 out of range")))
}

pub(super) fn read_u16(value: Option<&Value>, path: &str) -> Result<u16> {
    read_u64(value, path).and_then(|value| {
        u16::try_from(value).map_err(|_| graph_document(path, "u16 out of range"))
    })
}

pub(super) fn read_u32(value: Option<&Value>, path: &str) -> Result<u32> {
    read_u64(value, path).and_then(|value| {
        u32::try_from(value).map_err(|_| graph_document(path, "u32 out of range"))
    })
}

pub(super) fn read_u64(value: Option<&Value>, path: &str) -> Result<u64> {
    match value {
        Some(Value::String(value)) => value
            .parse()
            .map_err(|_| graph_document(path, "invalid unsigned decimal string")),
        Some(Value::Number(value)) => value
            .as_u64()
            .ok_or_else(|| graph_document(path, "expected unsigned integer")),
        _ => Err(graph_document(path, "expected unsigned integer")),
    }
}

pub(super) fn read_i32(value: Option<&Value>, path: &str) -> Result<i32> {
    value
        .and_then(Value::as_i64)
        .and_then(|value| i32::try_from(value).ok())
        .ok_or_else(|| graph_document(path, "expected i32"))
}

pub(super) fn read_guid(value: Option<&Value>, path: &str) -> Result<u128> {
    let text = value
        .and_then(Value::as_str)
        .ok_or_else(|| graph_document(path, "expected lowercase 32-digit hexadecimal GUID"))?;
    if text.len() != 32
        || !text.bytes().all(|byte| byte.is_ascii_hexdigit())
        || text.bytes().any(|byte| byte.is_ascii_uppercase())
    {
        return Err(graph_document(
            path,
            "expected lowercase 32-digit hexadecimal GUID",
        ));
    }
    u128::from_str_radix(text, 16).map_err(|_| graph_document(path, "invalid hexadecimal GUID"))
}

pub(super) fn parse_guid_array(value: &Value, path: &str) -> Result<Vec<u128>> {
    read_array(Some(value), path)?
        .iter()
        .enumerate()
        .map(|(index, value)| read_guid(Some(value), &format!("{path}[{index}]")))
        .collect()
}

pub(super) fn parse_seed_namespaces(value: &Value, path: &str) -> Result<BTreeMap<String, u128>> {
    let object = value
        .as_object()
        .ok_or_else(|| graph_document(path, "expected object"))?;
    object
        .iter()
        .map(|(name, value)| {
            if name.is_empty() {
                return Err(graph_document(path, "seed namespace name cannot be empty"));
            }
            Ok((
                name.clone(),
                read_guid(Some(value), &format!("{path}.{name}"))?,
            ))
        })
        .collect()
}

pub(super) fn read_domain(value: Option<&Value>, path: &str) -> Result<GraphDomain> {
    let text = value
        .and_then(Value::as_str)
        .ok_or_else(|| graph_document(path, "expected domain string"))?;
    GraphDomain::from_wire(text).ok_or_else(|| graph_document(path, "unknown domain"))
}

pub(super) fn read_field_channel(value: Option<&Value>, path: &str) -> Result<FieldChannel> {
    let text = value
        .and_then(Value::as_str)
        .ok_or_else(|| graph_document(path, "expected field channel"))?;
    field_channel_from_wire(text).ok_or_else(|| graph_document(path, "unknown field channel"))
}

pub(super) fn field_channel_from_wire(value: &str) -> Option<FieldChannel> {
    Some(match value {
        "altitude" => FieldChannel::Altitude,
        "slope" => FieldChannel::Slope,
        "curvature" => FieldChannel::Curvature,
        "concavity" => FieldChannel::Concavity,
        "drainage" => FieldChannel::Drainage,
        "moisture" => FieldChannel::Moisture,
        "temperature" => FieldChannel::Temperature,
        "precipitation" => FieldChannel::Precipitation,
        "sunlight" => FieldChannel::Sunlight,
        "exposure" => FieldChannel::Exposure,
        "water-distance" => FieldChannel::WaterDistance,
        "water-depth" => FieldChannel::WaterDepth,
        "signed-blocker" => FieldChannel::SignedBlocker,
        "spline-distance" => FieldChannel::SplineDistance,
        value if value.starts_with("user:") => FieldChannel::User(value[5..].parse().ok()?),
        _ => return None,
    })
}

pub(super) fn field_channel_wire(value: FieldChannel) -> String {
    match value {
        FieldChannel::Altitude => "altitude".to_owned(),
        FieldChannel::Slope => "slope".to_owned(),
        FieldChannel::Curvature => "curvature".to_owned(),
        FieldChannel::Concavity => "concavity".to_owned(),
        FieldChannel::Drainage => "drainage".to_owned(),
        FieldChannel::Moisture => "moisture".to_owned(),
        FieldChannel::Temperature => "temperature".to_owned(),
        FieldChannel::Precipitation => "precipitation".to_owned(),
        FieldChannel::Sunlight => "sunlight".to_owned(),
        FieldChannel::Exposure => "exposure".to_owned(),
        FieldChannel::WaterDistance => "water-distance".to_owned(),
        FieldChannel::WaterDepth => "water-depth".to_owned(),
        FieldChannel::SignedBlocker => "signed-blocker".to_owned(),
        FieldChannel::SplineDistance => "spline-distance".to_owned(),
        FieldChannel::User(id) => format!("user:{id}"),
    }
}

pub(super) fn field_derivative_from_wire(value: &str) -> Option<FieldDerivative> {
    Some(match value {
        "value" => FieldDerivative::Value,
        "gradient" => FieldDerivative::Gradient,
        "hessian" => FieldDerivative::Hessian,
        _ => return None,
    })
}

pub(super) const fn field_derivative_wire(value: FieldDerivative) -> &'static str {
    match value {
        FieldDerivative::Value => "value",
        FieldDerivative::Gradient => "gradient",
        FieldDerivative::Hessian => "hessian",
    }
}

pub(super) fn append_dependency_source(bytes: &mut Vec<u8>, source: GraphDependencySource) {
    bytes.extend_from_slice(&dependency_source_bytes(source));
}

pub(super) fn dependency_source_bytes(source: GraphDependencySource) -> Vec<u8> {
    let mut bytes = Vec::new();
    match source {
        GraphDependencySource::Asset(id) => {
            bytes.push(0);
            bytes.extend_from_slice(&id.value().to_be_bytes());
        }
        GraphDependencySource::Field(channel) => {
            bytes.push(1);
            let wire = field_channel_wire(channel);
            bytes.extend_from_slice(&(wire.len() as u64).to_be_bytes());
            bytes.extend_from_slice(wire.as_bytes());
        }
        GraphDependencySource::SurfaceProvider(provider) => {
            bytes.push(2);
            bytes.extend_from_slice(&provider.to_be_bytes());
        }
        GraphDependencySource::MapLayer(layer) => {
            bytes.push(3);
            bytes.extend_from_slice(&layer.to_be_bytes());
        }
    }
    bytes
}

pub(super) fn guid_text(value: u128) -> String {
    format!("{value:032x}")
}
