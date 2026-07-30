use crate::{Uuid, Vec3};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// One exact signed world-tick coordinate encoded as decimal strings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SpatialTicksDto {
    pub x: String,
    pub y: String,
    pub z: String,
}

/// One canonical hierarchical world-cell key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct WorldCellKeyDto {
    pub x: String,
    pub y: String,
    pub z: String,
    pub level: u8,
    pub canonical_hex: String,
}

/// A half-open quantized position inside one base cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SpatialLocalPositionDto {
    pub x: u32,
    pub y: u32,
    pub z: u32,
}

/// An exact world position and its canonical owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SpatialWorldPositionDto {
    pub cell: WorldCellKeyDto,
    pub local: SpatialLocalPositionDto,
    pub global_ticks: SpatialTicksDto,
}

/// Converts a metre position or exact ticks to a canonical cell hierarchy.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SpatialCellParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub world: Option<Vec3>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ticks: Option<SpatialTicksDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level: Option<u8>,
}

/// Canonical position ownership and the requested ancestor cell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SpatialCellResult {
    pub position: SpatialWorldPositionDto,
    pub selected_cell: WorldCellKeyDto,
}

/// Query and authority capabilities of a surface provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SurfaceCapabilitiesDto {
    pub ray: bool,
    pub project: bool,
    pub nearest: bool,
    pub uv: bool,
    pub authoritative_attachments: bool,
    pub authoritative_fields: bool,
}

/// One exact half-open provider bounds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SpatialBoundsDto {
    pub min_ticks: SpatialTicksDto,
    pub max_ticks_exclusive: SpatialTicksDto,
}

/// One live surface provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SurfaceProviderDto {
    pub id: Uuid,
    pub entity: Uuid,
    pub name: String,
    pub revision: String,
    pub bounds: SpatialBoundsDto,
    pub primitive_count: String,
    pub max_tags_per_hit: u32,
    pub capabilities: SurfaceCapabilitiesDto,
}

/// Every live surface provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SurfaceProvidersResult {
    pub providers: Vec<SurfaceProviderDto>,
}

/// A scalar or vector channel exposed by a surface field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum SpatialFieldChannelDto {
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

/// The requested field derivative.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum SpatialFieldDerivativeDto {
    #[default]
    Value,
    Gradient,
    Hessian,
}

/// Samples a live provider field at a metre position.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SpatialSampleParams {
    pub provider: Uuid,
    pub channel: SpatialFieldChannelDto,
    /// Decimal stable identity required when `channel` is `user`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_channel: Option<String>,
    pub position: Vec3,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub derivative: Option<SpatialFieldDerivativeDto>,
}

/// One canonical scalar field sample.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SpatialSampleResult {
    pub provider: Uuid,
    pub channel: SpatialFieldChannelDto,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_channel: Option<String>,
    pub derivative: SpatialFieldDerivativeDto,
    pub value_bits: i32,
    pub value: f64,
    pub revision: String,
}

/// One independently resident cell facet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum ResidencyFacetDto {
    Render,
    Physics,
    Simulation,
    Editing,
    Navigation,
    Network,
}

/// Load and cleanup radii at one hierarchy level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SpatialSourceLevelDto {
    pub level: u8,
    pub load_radius_cells: u32,
    pub cleanup_radius_cells: u32,
}

/// One live residency source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SpatialSourceDto {
    pub id: String,
    pub revision: String,
    pub position: SpatialWorldPositionDto,
    pub velocity_mps: Vec3,
    pub prediction_seconds: f64,
    pub levels: Vec<SpatialSourceLevelDto>,
    pub facets: Vec<ResidencyFacetDto>,
    pub priority: i32,
}

/// Per-facet reference counts for one resident cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ResidencyCountsDto {
    pub render: u32,
    pub physics: u32,
    pub simulation: u32,
    pub editing: u32,
    pub navigation: u32,
    pub network: u32,
}

/// One resolved resident cell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SpatialResidencyCellDto {
    pub cell: WorldCellKeyDto,
    pub reference_counts: ResidencyCountsDto,
    pub priority: i32,
}

/// Complete source and cell residency status.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SpatialResidencyResult {
    pub sources: Vec<SpatialSourceDto>,
    pub cells: Vec<SpatialResidencyCellDto>,
}
