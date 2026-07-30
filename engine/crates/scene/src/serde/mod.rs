//! The byte-compatible component JSON serde.
//!
//! Every body here pins frozen wire bytes: key spellings, per-field defaults, the decimal-string
//! uuid encoding, the lowercase enum-name spellings, the named-vector shape, and the flat
//! column-major matrix layout. A single drift fails silently as corrupted data, so the bodies stay
//! imperative over the `saffron-json` lenient readers rather than deriving — the contract is
//! key-order- and default-sensitive in ways `#[derive(Deserialize)]` does not reproduce.
//!
//! Component scalars are stored as `f32`, but the wire promotes each to `f64` and dumps the shortest
//! round-trippable decimal of *that*, so every scalar goes through [`f32_value`]. The save path sorts
//! object keys, so the order of the `insert` calls is incidental to the output bytes.

use glam::Vec3;
use serde_json::{Map, Value};

use saffron_json::json_f32_or;

mod component;
mod environment;

pub use environment::{environment_from_json, environment_to_json};

/// Wraps an `f32` as a JSON number formatted from its f64 promotion. This is the
/// byte-equality seam — every component scalar is inserted through it.
fn f32_value(value: f32) -> Value {
    Value::from(f64::from(value))
}

fn json_i32_or(value: &Value, key: &str, default: i32) -> i32 {
    field(value, key)
        .and_then(Value::as_i64)
        .and_then(|value| i32::try_from(value).ok())
        .unwrap_or(default)
}

/// A named-object `vec3` → `{"x","y","z"}`. Never positional — quat/vec storage order is
/// config-dependent.
fn vec3_to_json(v: Vec3) -> Value {
    Value::Object(Map::from_iter([
        ("x".to_string(), f32_value(v.x)),
        ("y".to_string(), f32_value(v.y)),
        ("z".to_string(), f32_value(v.z)),
    ]))
}

/// Reads a `vec3` from a named object, each component defaulting to `0`.
fn vec3_from_json(j: &Value) -> Vec3 {
    Vec3::new(
        json_f32_or(j, "x", 0.0),
        json_f32_or(j, "y", 0.0),
        json_f32_or(j, "z", 0.0),
    )
}

/// Locates an object field as a borrowed value, the input to a nested-object read.
fn field<'a>(j: &'a Value, key: &str) -> Option<&'a Value> {
    j.as_object().and_then(|m| m.get(key))
}

/// Builds an object from an ordered list of entries. Key order is incidental (the output
/// is alphabetically sorted by `serde_json`), so this is purely a terse constructor.
fn object<const N: usize>(entries: [(&str, Value); N]) -> Value {
    Value::Object(Map::from_iter(
        entries.into_iter().map(|(k, v)| (k.to_string(), v)),
    ))
}
