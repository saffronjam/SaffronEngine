//! JSON Schema fragments generated from the Rust wire DTOs.

use schemars::JsonSchema;
use serde_json::{Map, Value, json};

/// A standalone JSON Schema document for wire type `T`, including its local definitions.
#[must_use]
pub fn standalone_schema_for<T: JsonSchema>() -> Value {
    serde_json::to_value(schemars::schema_for!(T))
        .expect("schemars schema serializes to a JSON value")
}

/// The wire field names of DTO `T` in declaration order.
#[must_use]
pub fn positional_field_order<T: JsonSchema>() -> Vec<String> {
    let raw = standalone_schema_for::<T>();
    raw.get("properties")
        .and_then(Value::as_object)
        .map(|properties| properties.keys().cloned().collect())
        .unwrap_or_default()
}

/// The OpenRPC JSON Schema fragment for wire type `T`.
#[must_use]
pub fn fragment_for<T: JsonSchema>(type_name: &str) -> Value {
    let raw = standalone_schema_for::<T>();
    let defs = raw.get("$defs").and_then(Value::as_object).cloned();

    if let Some(properties) = raw.get("properties").and_then(Value::as_object) {
        let mut out = Map::new();
        for (field, schema) in properties {
            let schema = if is_any(schema) {
                json!({})
            } else {
                normalize(schema, defs.as_ref())
            };
            out.insert(field.clone(), schema);
        }
        return json!({
            "type": "object",
            "additionalProperties": false,
            "properties": Value::Object(out),
            "required": raw.get("required").cloned().unwrap_or_else(|| json!([])),
        });
    }

    let normalized = normalize(&raw, defs.as_ref());
    assert_ne!(normalized, Value::Null, "empty schema for {type_name}");
    normalized
}

fn is_any(schema: &Value) -> bool {
    matches!(schema, Value::Bool(true)) || schema.as_object().is_some_and(serde_json::Map::is_empty)
}

fn normalize(schema: &Value, defs: Option<&Map<String, Value>>) -> Value {
    let Value::Object(map) = schema else {
        return schema.clone();
    };

    if let Some(any_of) = map.get("anyOf").and_then(Value::as_array)
        && any_of.iter().any(is_null_type)
        && let Some(inner) = any_of.iter().find(|entry| !is_null_type(entry))
    {
        return normalize(inner, defs);
    }

    if let Some(reference) = map.get("$ref").and_then(Value::as_str) {
        return resolve_ref(reference, defs);
    }

    let mut out = Map::new();
    for (key, value) in map {
        match key.as_str() {
            "$defs" | "$schema" | "description" | "format" | "title" => {}
            "type" => {
                out.insert(key.clone(), strip_null(value));
            }
            _ => {
                out.insert(key.clone(), normalize_nested(value, defs));
            }
        }
    }
    Value::Object(out)
}

fn normalize_nested(value: &Value, defs: Option<&Map<String, Value>>) -> Value {
    match value {
        Value::Object(_) => normalize(value, defs),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| normalize_nested(item, defs))
                .collect(),
        ),
        _ => value.clone(),
    }
}

fn resolve_ref(reference: &str, defs: Option<&Map<String, Value>>) -> Value {
    let name = reference.rsplit('/').next().unwrap_or(reference);
    if let Some(definition) = defs.and_then(|definitions| definitions.get(name))
        && let Some(variants) = definition.get("enum")
    {
        return json!({ "type": "string", "enum": variants.clone() });
    }
    json!({ "$ref": format!("#/components/schemas/{name}") })
}

fn is_null_type(schema: &Value) -> bool {
    schema
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|value| value == "null")
}

fn strip_null(type_value: &Value) -> Value {
    if let Value::Array(items) = type_value
        && let Some(non_null) = items.iter().find(|item| item.as_str() != Some("null"))
    {
        return non_null.clone();
    }
    type_value.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::*;

    #[test]
    fn plain_dto_uses_component_refs() {
        let schema = fragment_for::<RaycastParams>("RaycastParams");
        assert_eq!(
            schema["properties"]["origin"],
            json!({ "$ref": "#/components/schemas/Vec3" })
        );
        assert_eq!(schema["required"], json!(["origin", "dir"]));
    }

    #[test]
    fn positional_fields_follow_declaration_order() {
        assert_eq!(
            positional_field_order::<RaycastParams>(),
            ["origin", "dir", "maxDist"]
        );
        assert_eq!(
            positional_field_order::<SetComponentParams>(),
            ["entity", "component", "json"]
        );
    }

    #[test]
    fn optional_fields_are_not_required_or_nullable() {
        let schema = fragment_for::<RaycastParams>("RaycastParams");
        assert_eq!(schema["required"], json!(["origin", "dir"]));
        assert_eq!(schema["properties"]["maxDist"], json!({ "type": "number" }));
    }

    #[test]
    fn selectors_are_named_rust_schema_refs() {
        let schema = fragment_for::<ComponentParams>("ComponentParams");
        assert_eq!(
            schema["properties"]["entity"],
            json!({ "$ref": "#/components/schemas/EntitySelector" })
        );
    }

    #[test]
    fn dynamic_component_fields_reference_rust_wire_types() {
        let inspect = fragment_for::<InspectResult>("InspectResult");
        assert_eq!(
            inspect["properties"]["components"],
            json!({ "$ref": "#/components/schemas/Components" })
        );

        let set = fragment_for::<SetComponentParams>("SetComponentParams");
        assert_eq!(
            set["properties"]["json"],
            json!({ "$ref": "#/components/schemas/ComponentBody" })
        );
    }

    #[test]
    fn environment_is_a_generated_object() {
        let schema = fragment_for::<EnvironmentDto>("EnvironmentDto");
        assert_eq!(schema["type"], json!("object"));
        assert_eq!(
            schema["properties"]["atmosphere"],
            json!({ "$ref": "#/components/schemas/AtmosphereSettingsDto" })
        );
        assert_eq!(
            schema["properties"]["timeOfDay"],
            json!({ "$ref": "#/components/schemas/TimeOfDaySettingsDto" })
        );
    }

    #[test]
    fn component_body_is_a_generated_union() {
        let schema = fragment_for::<ComponentBody>("ComponentBody");
        assert_eq!(
            schema["anyOf"].as_array().map(Vec::len),
            Some(COMPONENT_NAMES.len())
        );
    }

    #[test]
    fn component_aggregate_covers_the_registry_model() {
        let schema = fragment_for::<Components>("Components");
        let properties = schema["properties"].as_object().unwrap();
        assert_eq!(properties.len(), COMPONENT_NAMES.len());
        for name in COMPONENT_NAMES {
            assert!(properties.contains_key(*name), "missing component {name}");
        }
    }

    #[test]
    fn opaque_json_remains_open() {
        let schema = fragment_for::<MaterialSetGraphParams>("MaterialSetGraphParams");
        assert_eq!(schema["properties"]["graph"], json!({}));
    }
}
