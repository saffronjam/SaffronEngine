//! Coercing readers for the wire-contract bool fields.
//!
//! A bool *params* field accepts a JSON bool, a number (`!= 0`), or a string (everything but
//! `"0"`/`"false"`/`"off"` is true), because the `sa` CLI and the editor pass `1`/`0` and
//! `"on"`/`"off"` for toggles. Result bools serialize as plain JSON bools and need no coercion.

use serde::{Deserialize, Deserializer};
use serde_json::Value;

fn from_value<E: serde::de::Error>(value: &Value) -> Result<bool, E> {
    match value {
        Value::Bool(b) => Ok(*b),
        Value::Number(n) => Ok(n.as_f64().is_some_and(|f| f != 0.0)),
        Value::String(s) => Ok(!(s == "0" || s == "false" || s == "off")),
        other => Err(serde::de::Error::custom(format!(
            "expected a boolean, got {other}"
        ))),
    }
}

/// `#[serde(deserialize_with)]` for a required bool field.
pub fn boolean<'de, D: Deserializer<'de>>(deserializer: D) -> Result<bool, D::Error> {
    let value = Value::deserialize(deserializer)?;
    from_value(&value)
}

/// `#[serde(deserialize_with, default)]` for an optional bool field.
pub fn opt_boolean<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<bool>, D::Error> {
    match Option::<Value>::deserialize(deserializer)? {
        Some(Value::Null) | None => Ok(None),
        Some(value) => from_value(&value).map(Some),
    }
}

#[cfg(test)]
mod tests {
    use crate::{SetAnimationPlayingParams, ToggleParams};

    #[test]
    fn toggle_enabled_coerces_number_string_and_bool() {
        let one: ToggleParams =
            serde_json::from_value(serde_json::json!({ "enabled": 1 })).unwrap();
        assert_eq!(one.enabled, Some(true));
        let zero: ToggleParams =
            serde_json::from_value(serde_json::json!({ "enabled": 0 })).unwrap();
        assert_eq!(zero.enabled, Some(false));
        let on: ToggleParams =
            serde_json::from_value(serde_json::json!({ "enabled": "on" })).unwrap();
        assert_eq!(on.enabled, Some(true));
        let off: ToggleParams =
            serde_json::from_value(serde_json::json!({ "enabled": "off" })).unwrap();
        assert_eq!(off.enabled, Some(false));
        let real: ToggleParams =
            serde_json::from_value(serde_json::json!({ "enabled": true })).unwrap();
        assert_eq!(real.enabled, Some(true));
        let absent: ToggleParams = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(absent.enabled, None);
    }

    #[test]
    fn required_bool_param_coerces_a_number() {
        let p: SetAnimationPlayingParams =
            serde_json::from_value(serde_json::json!({ "entity": "rig", "playing": 1 })).unwrap();
        assert!(p.playing);
    }
}
