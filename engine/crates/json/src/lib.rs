//! The serde_json gateway: the parse/dump entry points, the lenient typed readers
//! the control-command handlers depend on, and the decimal-string-`u64` wire encoding
//! the engine and the editor share byte-for-byte.
//!
//! Depends on `saffron-core`.

#![deny(unsafe_code)]

use saffron_core::Uuid;
use serde::Deserialize;
use serde::de::{Deserializer, Error as _};
use serde::ser::Serializer;
use serde_with::{DeserializeAs, SerializeAs};

pub use serde_json::{Map, Value, json};

/// Errors raised by the JSON gateway and the typed readers.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Text could not be parsed as JSON. The payload is the parser's message —
    /// genuinely unstructured, so a `String` is the right shape.
    #[error("invalid JSON: {0}")]
    Parse(String),
    /// A required object field was absent.
    #[error("missing key '{0}'")]
    MissingKey(String),
    /// A field was present but held the wrong JSON type for the requested read.
    #[error("key '{key}' is not {expected}")]
    WrongType {
        /// The object field that was read.
        key: String,
        /// A short noun phrase naming the expected type (e.g. `an unsigned integer`).
        expected: &'static str,
    },
}

/// The crate `Result` alias bound to the typed [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

/// Parses text into a JSON value, or a typed error.
///
/// # Errors
///
/// Returns [`Error::Parse`] if `text` is not valid JSON.
pub fn parse_json(text: &str) -> Result<Value> {
    serde_json::from_str(text).map_err(|e| Error::Parse(e.to_string()))
}

/// Serializes a JSON value to a string.
///
/// `indent < 0` produces compact output; `indent >= 0` pretty-prints with that many
/// spaces per level.
#[must_use]
pub fn dump_json(value: &Value, indent: i32) -> String {
    if indent < 0 {
        serde_json::to_string(value).unwrap_or_default()
    } else {
        let indent = usize::try_from(indent).unwrap_or(0);
        let pad = vec![b' '; indent];
        let formatter = serde_json::ser::PrettyFormatter::with_indent(&pad);
        let mut buf = Vec::new();
        let mut ser = serde_json::Serializer::with_formatter(&mut buf, formatter);
        if serde::Serialize::serialize(value, &mut ser).is_err() {
            return String::new();
        }
        String::from_utf8(buf).unwrap_or_default()
    }
}

/// Serializes a JSON value with every object's keys in **lexicographically sorted** order,
/// recursively — the byte-frozen asset formats (`.smat`, the `.smodel` META chunk) depend
/// on this for a stable source hash.
///
/// `serde_json` is built workspace-wide with `preserve_order` (the control wire needs
/// insertion order = field order), so object keys do not sort implicitly; the asset
/// encoders call this instead of [`dump_json`] to keep their sorted byte shape. `indent`
/// follows [`dump_json`]: `< 0` is compact, `>= 0` pretty-prints with that many spaces.
#[must_use]
pub fn dump_json_sorted(value: &Value, indent: i32) -> String {
    dump_json(&sort_keys(value), indent)
}

/// A deep copy of `value` with every object's keys re-inserted in sorted order (arrays keep
/// their element order; scalars pass through). Used by [`dump_json_sorted`].
fn sort_keys(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let mut sorted = serde_json::Map::with_capacity(map.len());
            for key in keys {
                sorted.insert(key.clone(), sort_keys(&map[key]));
            }
            Value::Object(sorted)
        }
        Value::Array(items) => Value::Array(items.iter().map(sort_keys).collect()),
        other => other.clone(),
    }
}

/// Emits a `u64` id as a decimal JSON *string*.
///
/// Ids span the full `u64` range, past JavaScript's `2^53` safe integer, so a JSON
/// number would silently lose precision on a JS client. The matching read
/// ([`json_u64`]) accepts a string *or* a number; this is the emit side of the frozen
/// wire contract and the protocol crate's `serde_with` derive must match it byte-for-byte.
#[must_use]
pub fn uuid_to_json(value: u64) -> Value {
    Value::String(value.to_string())
}

/// The `serde_with` adapter for the frozen decimal-string-`u64` wire encoding.
///
/// `#[serde_as(as = "WireUuid")]` on a [`Uuid`] field emits a decimal string and accepts
/// a string *or* a number on read — the exact lenient union [`uuid_to_json`] / [`json_u64`]
/// implement imperatively. This is the single source of the derive-driven encoding the
/// protocol crate reuses, so there is one wire encoding and it is defined once here.
pub struct WireUuid;

impl SerializeAs<Uuid> for WireUuid {
    fn serialize_as<S>(value: &Uuid, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&value.0.to_string())
    }
}

impl<'de> DeserializeAs<'de, Uuid> for WireUuid {
    fn deserialize_as<D>(deserializer: D) -> std::result::Result<Uuid, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        wire_u64(&value)
            .map(Uuid)
            .ok_or_else(|| D::Error::custom("expected a u64 as a decimal string or a number"))
    }
}

/// Reads a `u64` out of a JSON value with the lenient wire semantics: an unsigned
/// number, a non-negative integer, or a decimal string whose *entire* content parses.
/// A trailing-garbage string, a negative number, or any other type yields `None`.
fn wire_u64(value: &Value) -> Option<u64> {
    match value {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => s.parse::<u64>().ok(),
        _ => None,
    }
}

/// Locates `object[key]`, returning `None` when `object` is not an object or the key
/// is absent.
fn find_field<'a>(object: &'a Value, key: &str) -> Option<&'a Value> {
    object.as_object().and_then(|map| map.get(key))
}

/// Reads a `u64` field, accepting a number *or* a decimal string.
///
/// The lenient union mirrors the frozen wire contract: an unsigned number, a
/// non-negative integer, or a string whose entire content parses as a `u64`. A
/// trailing-garbage string (`"42x"`) or a negative number is rejected.
///
/// # Errors
///
/// [`Error::MissingKey`] if `key` is absent; [`Error::WrongType`] if it is present
/// but not an unsigned integer under the lenient rules.
pub fn json_u64(object: &Value, key: &str) -> Result<u64> {
    let field = find_field(object, key).ok_or_else(|| Error::MissingKey(key.to_owned()))?;
    wire_u64(field).ok_or_else(|| Error::WrongType {
        key: key.to_owned(),
        expected: "an unsigned integer",
    })
}

/// Reads a string field.
///
/// # Errors
///
/// [`Error::MissingKey`] if `key` is absent; [`Error::WrongType`] if it is not a string.
pub fn json_string(object: &Value, key: &str) -> Result<String> {
    let field = find_field(object, key).ok_or_else(|| Error::MissingKey(key.to_owned()))?;
    field
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| Error::WrongType {
            key: key.to_owned(),
            expected: "a string",
        })
}

/// Reads an `f64` field.
///
/// # Errors
///
/// [`Error::MissingKey`] if `key` is absent; [`Error::WrongType`] if it is not a number.
pub fn json_f64(object: &Value, key: &str) -> Result<f64> {
    let field = find_field(object, key).ok_or_else(|| Error::MissingKey(key.to_owned()))?;
    field.as_f64().ok_or_else(|| Error::WrongType {
        key: key.to_owned(),
        expected: "a number",
    })
}

/// Reads a boolean field.
///
/// # Errors
///
/// [`Error::MissingKey`] if `key` is absent; [`Error::WrongType`] if it is not a boolean.
pub fn json_bool(object: &Value, key: &str) -> Result<bool> {
    let field = find_field(object, key).ok_or_else(|| Error::MissingKey(key.to_owned()))?;
    field.as_bool().ok_or_else(|| Error::WrongType {
        key: key.to_owned(),
        expected: "a boolean",
    })
}

/// Reads a `u64` field, returning `fallback` when absent or mistyped.
#[must_use]
pub fn json_u64_or(object: &Value, key: &str, fallback: u64) -> u64 {
    json_u64(object, key).unwrap_or(fallback)
}

/// Reads a string field, returning `fallback` when absent or mistyped.
#[must_use]
pub fn json_string_or(object: &Value, key: &str, fallback: String) -> String {
    json_string(object, key).unwrap_or(fallback)
}

/// Reads a number field as `f32`, narrowing the `f64` wire value.
///
/// Returns `fallback` when absent or mistyped; otherwise reads the value as an `f64`
/// (the wire numeric type) and narrows it to `f32`.
#[must_use]
pub fn json_f32_or(object: &Value, key: &str, fallback: f32) -> f32 {
    json_f64(object, key).map_or(fallback, |value| value as f32)
}

/// Reads a boolean field, returning `fallback` when absent or mistyped.
#[must_use]
pub fn json_bool_or(object: &Value, key: &str, fallback: bool) -> bool {
    json_bool(object, key).unwrap_or(fallback)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_with::serde_as;

    #[test]
    fn parse_round_trips_compact_and_pretty() {
        let value = parse_json(r#"{"a":1,"b":[2,3]}"#).unwrap();
        assert_eq!(dump_json(&value, -1), r#"{"a":1,"b":[2,3]}"#);
        let pretty = dump_json(&value, 2);
        assert!(pretty.contains('\n'));
        // A pretty dump re-parses to the same value.
        assert_eq!(parse_json(&pretty).unwrap(), value);
    }

    #[test]
    fn parse_rejects_garbage() {
        assert!(matches!(parse_json("{not json"), Err(Error::Parse(_))));
    }

    #[test]
    fn uuid_emits_decimal_string_not_number() {
        // The silent-failure guard: a full-range id must serialize as a *string*,
        // never a number — the serialized bytes must carry quotes.
        let value = uuid_to_json(u64::MAX);
        assert_eq!(value, Value::String("18446744073709551615".to_owned()));
        let serialized = dump_json(&value, -1);
        assert_eq!(serialized, r#""18446744073709551615""#);
        assert!(serialized.starts_with('"') && serialized.ends_with('"'));
    }

    #[test]
    fn uuid_round_trips_through_json_u64() {
        // uuid_to_json then json_u64 recovers the full u64 range exactly.
        for raw in [0u64, 1023, 1024, 42, u64::MAX] {
            let object = serde_json::json!({ "id": uuid_to_json(raw) });
            assert_eq!(json_u64(&object, "id").unwrap(), raw);
        }
    }

    #[test]
    fn json_u64_accepts_number_string_and_full_range() {
        assert_eq!(json_u64(&serde_json::json!({ "k": 42 }), "k").unwrap(), 42);
        assert_eq!(
            json_u64(&serde_json::json!({ "k": "42" }), "k").unwrap(),
            42
        );
        let max = serde_json::json!({ "k": "18446744073709551615" });
        assert_eq!(json_u64(&max, "k").unwrap(), u64::MAX);
    }

    #[test]
    fn json_u64_rejects_trailing_garbage_negative_and_missing() {
        // A string with trailing garbage is rejected (whole-string parse).
        assert!(matches!(
            json_u64(&serde_json::json!({ "k": "42x" }), "k"),
            Err(Error::WrongType { .. })
        ));
        // A negative number is not an unsigned integer.
        assert!(matches!(
            json_u64(&serde_json::json!({ "k": -1 }), "k"),
            Err(Error::WrongType { .. })
        ));
        // A missing key is a distinct, typed error.
        assert!(matches!(
            json_u64(&serde_json::json!({ "other": 1 }), "k"),
            Err(Error::MissingKey(_))
        ));
    }

    #[test]
    fn strict_readers_report_missing_and_wrong_type() {
        let object = serde_json::json!({
            "s": "hi", "f": 1.5, "b": true,
            "wrong_s": 1, "wrong_f": "x", "wrong_b": 0,
        });

        assert_eq!(json_string(&object, "s").unwrap(), "hi");
        assert!((json_f64(&object, "f").unwrap() - 1.5).abs() < f64::EPSILON);
        assert!(json_bool(&object, "b").unwrap());

        assert!(matches!(
            json_string(&object, "missing"),
            Err(Error::MissingKey(_))
        ));
        assert!(matches!(
            json_f64(&object, "missing"),
            Err(Error::MissingKey(_))
        ));
        assert!(matches!(
            json_bool(&object, "missing"),
            Err(Error::MissingKey(_))
        ));

        assert!(matches!(
            json_string(&object, "wrong_s"),
            Err(Error::WrongType { .. })
        ));
        assert!(matches!(
            json_f64(&object, "wrong_f"),
            Err(Error::WrongType { .. })
        ));
        assert!(matches!(
            json_bool(&object, "wrong_b"),
            Err(Error::WrongType { .. })
        ));
    }

    #[test]
    fn or_readers_fall_back_then_read() {
        let object = serde_json::json!({
            "u": 7, "s": "set", "f": 2.5, "b": true,
            "bad_u": "x", "bad_f": "x", "bad_b": "x",
        });

        // Present and well-typed → the value.
        assert_eq!(json_u64_or(&object, "u", 99), 7);
        assert_eq!(json_string_or(&object, "s", "def".to_owned()), "set");
        assert!((json_f32_or(&object, "f", 0.0) - 2.5).abs() < f32::EPSILON);
        assert!(json_bool_or(&object, "b", false));

        // Missing → the fallback.
        assert_eq!(json_u64_or(&object, "missing", 99), 99);
        assert_eq!(json_string_or(&object, "missing", "def".to_owned()), "def");
        assert!((json_f32_or(&object, "missing", 1.25) - 1.25).abs() < f32::EPSILON);
        assert!(json_bool_or(&object, "missing", true));

        // Mistyped → the fallback.
        assert_eq!(json_u64_or(&object, "bad_u", 99), 99);
        assert!((json_f32_or(&object, "bad_f", 1.25) - 1.25).abs() < f32::EPSILON);
        assert!(json_bool_or(&object, "bad_b", true));
    }

    #[test]
    fn f32_or_narrows_f64_wire_value() {
        // A wire f64 with no exact f32 representation narrows to the nearest f32.
        let object = serde_json::json!({ "f": 0.1f64 });
        assert!((json_f32_or(&object, "f", 0.0) - 0.1f32).abs() < f32::EPSILON);
    }

    #[test]
    fn wire_uuid_adapter_emits_string_and_accepts_either() {
        #[serde_as]
        #[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
        struct Holder {
            #[serde_as(as = "WireUuid")]
            id: Uuid,
        }

        let holder = Holder { id: Uuid(u64::MAX) };
        let json = serde_json::to_string(&holder).unwrap();
        // The derive-driven encoding emits a decimal string, byte-identical to
        // uuid_to_json — the contract the protocol crate relies on.
        assert_eq!(json, r#"{"id":"18446744073709551615"}"#);

        // Read accepts a string …
        let from_string: Holder = serde_json::from_str(r#"{"id":"18446744073709551615"}"#).unwrap();
        assert_eq!(from_string, holder);
        // … or a number.
        let from_number: Holder = serde_json::from_str(r#"{"id":42}"#).unwrap();
        assert_eq!(from_number, Holder { id: Uuid(42) });
    }
}
