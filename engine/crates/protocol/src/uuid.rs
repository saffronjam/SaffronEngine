//! The byte-frozen decimal-string `u64` wire newtype.

use std::borrow::Cow;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde_with::{DisplayFromStr, PickFirst, serde_as};
use ts_rs::TS;

/// A stable 64-bit identity as it crosses the control wire.
///
/// Serializes as a decimal *string*: ids span the full `u64` range past JavaScript's `2^53`
/// safe-integer limit, so a JSON number would silently corrupt them on a JS client. Reads accept
/// a string or a number.
#[serde_as]
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize, TS,
)]
#[serde(transparent)]
#[ts(export, type = "string")]
pub struct Uuid(#[serde_as(as = "PickFirst<(DisplayFromStr, _)>")] pub u64);

impl Uuid {
    /// The raw 64-bit value.
    #[must_use]
    pub fn value(self) -> u64 {
        self.0
    }
}

impl From<saffron_core::Uuid> for Uuid {
    fn from(id: saffron_core::Uuid) -> Self {
        Self(id.value())
    }
}

impl From<Uuid> for saffron_core::Uuid {
    fn from(id: Uuid) -> Self {
        saffron_core::Uuid(id.0)
    }
}

impl From<u64> for Uuid {
    fn from(value: u64) -> Self {
        Self(value)
    }
}

/// The wire schema is a JSON string, matching the decimal-string encoding `serde_as` emits.
impl JsonSchema for Uuid {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("Uuid")
    }

    fn schema_id() -> Cow<'static, str> {
        Cow::Borrowed("saffron_protocol::Uuid")
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        json_schema!({ "type": "string" })
    }

    fn inline_schema() -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emits_quoted_decimal_string_not_number() {
        assert_eq!(serde_json::to_string(&Uuid(42)).unwrap(), "\"42\"");
        assert_ne!(serde_json::to_string(&Uuid(42)).unwrap(), "42");
    }

    #[test]
    fn accepts_string_or_number_on_read() {
        assert_eq!(serde_json::from_str::<Uuid>("\"42\"").unwrap(), Uuid(42));
        assert_eq!(serde_json::from_str::<Uuid>("42").unwrap(), Uuid(42));
    }

    #[test]
    fn round_trips_past_2_53() {
        let big = Uuid(18_446_744_073_709_551_615);
        let text = serde_json::to_string(&big).unwrap();
        assert_eq!(text, "\"18446744073709551615\"");
        assert_eq!(serde_json::from_str::<Uuid>(&text).unwrap(), big);
    }

    #[test]
    fn cross_encoder_identity_with_saffron_json() {
        // The DTO derive and the imperative `saffron-json` helpers must emit byte-identical
        // output for a value, and each must parse the other's emit.
        for raw in [0_u64, 1023, 1024, 42, u64::MAX] {
            let derive_emit = serde_json::to_value(Uuid(raw)).unwrap();
            let imperative_emit = saffron_json::uuid_to_json(raw);
            assert_eq!(derive_emit, imperative_emit, "emit must be byte-identical");

            let object = serde_json::json!({ "id": derive_emit });
            assert_eq!(saffron_json::json_u64(&object, "id").unwrap(), raw);

            assert_eq!(
                serde_json::from_value::<Uuid>(imperative_emit).unwrap(),
                Uuid(raw)
            );
        }
    }

    #[test]
    fn converts_to_and_from_core_uuid() {
        let core = saffron_core::Uuid(99);
        let wire: Uuid = core.into();
        assert_eq!(wire, Uuid(99));
        let back: saffron_core::Uuid = wire.into();
        assert_eq!(back, core);
    }

    #[test]
    fn schema_is_string_not_integer() {
        let schema = schemars::schema_for!(Uuid);
        let value = serde_json::to_value(&schema).unwrap();
        assert_eq!(value.get("type").and_then(|t| t.as_str()), Some("string"));
    }

    #[test]
    fn ts_binding_is_string_alias() {
        assert_eq!(<Uuid as TS>::inline(), "string");
        assert_eq!(<Uuid as TS>::decl(), "type Uuid = string;");
    }
}
