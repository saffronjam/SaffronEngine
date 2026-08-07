use crate::{FieldChannelDto, InteractionPolicyDto, PlantId, Uuid, VegetationGuid, WorldBoundsDto};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Coordinate space of one ordered map layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum LayerCoordinateSpaceDto {
    World,
    Surface,
    OwnerLocal,
}

/// Quantized field blending operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum FieldBlendOperatorDto {
    Replace,
    Add,
    Multiply,
    Minimum,
    Maximum,
}

/// Inclusion semantics for masks and analytic shapes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum InclusionOperatorDto {
    Include,
    Exclude,
}

/// One canonical plant-family weight in a species layer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct SpeciesWeightDto {
    pub family: Uuid,
    pub weight: u16,
}

/// One complete authored transform override.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PlantTransformOverrideDto {
    pub plant: PlantId,
    pub global_ticks: [String; 3],
    pub scale_bits: [i32; 3],
}

/// One complete authored persistent plant-state override.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PlantStateOverrideDto {
    pub plant: PlantId,
    pub health: Option<u16>,
    pub moisture: Option<u16>,
    pub fuel: Option<u16>,
    pub interaction_policy: Option<InteractionPolicyDto>,
}

/// Every operation in the single vegetation-map layer algebra.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
#[ts(export)]
pub enum VegetationLayerOperatorDto {
    ScalarField {
        channel: FieldChannelDto,
        tile_set: VegetationGuid,
        blend: FieldBlendOperatorDto,
        weight: u16,
    },
    VectorField {
        channel: FieldChannelDto,
        tile_set: VegetationGuid,
        value_bits: [i32; 3],
        blend: FieldBlendOperatorDto,
    },
    SpeciesWeights {
        weights: Vec<SpeciesWeightDto>,
    },
    Density {
        channel: FieldChannelDto,
        tile_set: VegetationGuid,
        blend: FieldBlendOperatorDto,
        weight: u16,
    },
    Mask {
        tile_set: VegetationGuid,
        operation: InclusionOperatorDto,
    },
    Volume {
        bounds: WorldBoundsDto,
        operation: InclusionOperatorDto,
        falloff_bits: i32,
    },
    Spline {
        spline: VegetationGuid,
        points: Vec<[String; 3]>,
        radius_bits: i32,
        operation: InclusionOperatorDto,
    },
    Anchors {
        plants: Vec<PlantId>,
    },
    Pins {
        plants: Vec<PlantId>,
    },
    TransformOverrides {
        overrides: Vec<PlantTransformOverrideDto>,
    },
    StateOverrides {
        overrides: Vec<PlantStateOverrideDto>,
    },
    Blocker {
        tile_set: VegetationGuid,
        categories: u32,
    },
}

/// One stable ordered vegetation-map layer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationLayerDto {
    pub id: VegetationGuid,
    pub name: String,
    pub coordinate_space: LayerCoordinateSpaceDto,
    pub bounds: WorldBoundsDto,
    pub operator: VegetationLayerOperatorDto,
    pub dependencies: Vec<VegetationGuid>,
    pub order: i32,
    pub locked: bool,
    pub muted: bool,
    pub revision: String,
}
