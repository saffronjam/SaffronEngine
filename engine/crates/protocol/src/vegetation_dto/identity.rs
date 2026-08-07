use crate::Uuid;
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize};
use std::borrow::Cow;
use ts_rs::TS;

fn is_canonical_guid(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// An opaque canonical plant identity encoded as 32 lowercase hexadecimal digits.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, TS)]
#[serde(transparent)]
#[ts(export, type = "string")]
pub struct PlantId(pub String);

impl<'de> Deserialize<'de> for PlantId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        if is_canonical_guid(&value) {
            Ok(Self(value))
        } else {
            Err(serde::de::Error::custom(
                "plant ID must be 32 lowercase hexadecimal digits",
            ))
        }
    }
}

impl JsonSchema for PlantId {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("PlantId")
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "pattern": "^[0-9a-f]{32}$",
            "minLength": 32,
            "maxLength": 32
        })
    }

    fn inline_schema() -> bool {
        true
    }
}

/// A stable 128-bit authored GUID encoded as a lowercase 32-digit hexadecimal string.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, TS)]
#[serde(transparent)]
#[ts(export, type = "string")]
pub struct VegetationGuid(pub String);

impl<'de> Deserialize<'de> for VegetationGuid {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        if is_canonical_guid(&value) {
            Ok(Self(value))
        } else {
            Err(serde::de::Error::custom(
                "vegetation GUID must be 32 lowercase hexadecimal digits",
            ))
        }
    }
}

impl JsonSchema for VegetationGuid {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("VegetationGuid")
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "pattern": "^[0-9a-f]{32}$",
            "minLength": 32,
            "maxLength": 32
        })
    }

    fn inline_schema() -> bool {
        true
    }
}

/// One canonical hierarchical world-cell key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct WorldCellDto {
    /// Signed coordinates are strings so the complete i64 range survives JavaScript.
    pub coordinates: [String; 3],
    pub level: u8,
}

/// One exact half-open world bound in global quantized ticks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct WorldBoundsDto {
    pub min_ticks: [String; 3],
    pub max_ticks_exclusive: [String; 3],
}

/// Canonical biological lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum PlantLifecycleDto {
    Seed,
    Sprout,
    Juvenile,
    Mature,
    Senescent,
    Dead,
    Stump,
    Removed,
}

/// Default gameplay interaction policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum InteractionPolicyDto {
    Decorative,
    Interactive,
    Harvestable,
    Structural,
}

/// Shared authoritative field-channel kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum FieldChannelKindDto {
    Altitude,
    Slope,
    Curvature,
    Concavity,
    Drainage,
    Moisture,
    Temperature,
    Precipitation,
    Sunlight,
    Exposure,
    WaterDistance,
    WaterDepth,
    SignedBlocker,
    SplineDistance,
    User,
}

/// Shared authoritative field channel, including the registered user namespace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct FieldChannelDto {
    pub kind: FieldChannelKindDto,
    pub user: Option<String>,
}

/// Stable surface attachment carried by a plant point.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct SurfaceAttachmentDto {
    pub provider: String,
    pub primitive: String,
    pub barycentric: [u16; 3],
    pub revision: String,
}

/// One row in the schema-hashed canonical vegetation point vocabulary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PlantPointDto {
    pub id: PlantId,
    pub owner: WorldCellDto,
    pub local_position: [u32; 3],
    pub orientation: [i16; 4],
    pub scale_bits: [i32; 3],
    pub bounds: WorldBoundsDto,
    pub family: Uuid,
    pub variation: u32,
    pub lifecycle: PlantLifecycleDto,
    pub phenotype: u32,
    pub representation_class: u32,
    pub deterministic_key: VegetationGuid,
    pub candidate: String,
    pub parent: Option<PlantId>,
    pub colony: Option<PlantId>,
    pub ecology_tick: String,
    pub health: u16,
    pub moisture: u16,
    pub fuel: u16,
    pub phenology: u16,
    pub flags: u32,
    pub interaction_policy: InteractionPolicyDto,
    pub provenance: u32,
    pub attachment: Option<SurfaceAttachmentDto>,
    pub surface_projection_bits: [i32; 3],
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plant_id_is_a_strict_opaque_string() {
        let value = "1d8b921f8252f69b6c1c88f9da096eea";
        assert_eq!(
            serde_json::to_string(&PlantId(value.to_owned())).unwrap(),
            format!("\"{value}\"")
        );
        assert!(serde_json::from_str::<PlantId>(&format!("\"{value}\"")).is_ok());
        assert!(serde_json::from_str::<PlantId>("\"1D8B921F8252F69B6C1C88F9DA096EEA\"").is_err());
        assert!(serde_json::from_str::<PlantId>("42").is_err());
    }

    #[test]
    fn vegetation_guid_is_a_strict_opaque_string() {
        let value = "0000000000000000000000000000002a";
        assert_eq!(
            serde_json::to_string(&VegetationGuid(value.to_owned())).unwrap(),
            format!("\"{value}\"")
        );
        assert!(serde_json::from_str::<VegetationGuid>(&format!("\"{value}\"")).is_ok());
        assert!(serde_json::from_str::<VegetationGuid>("\"2A\"").is_err());
        assert!(serde_json::from_str::<VegetationGuid>("42").is_err());
    }
}
