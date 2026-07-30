use crate::Uuid;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// An entity selector by decimal id or exact name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(untagged)]
#[ts(export)]
pub enum EntitySelector {
    Id(u64),
    Name(String),
}

impl EntitySelector {
    /// Returns the numeric selector value, including a decimal string.
    #[must_use]
    pub fn id(&self) -> Option<u64> {
        match self {
            Self::Id(value) => Some(*value),
            Self::Name(value) => value.parse().ok(),
        }
    }

    /// Returns the string selector value.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        match self {
            Self::Id(_) => None,
            Self::Name(value) => Some(value),
        }
    }
}

/// An asset selector by decimal id or exact asset name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(untagged)]
#[ts(export)]
pub enum AssetSelector {
    Id(u64),
    Name(String),
}

impl AssetSelector {
    /// Returns the numeric selector value, including a decimal string.
    #[must_use]
    pub fn id(&self) -> Option<u64> {
        match self {
            Self::Id(value) => Some(*value),
            Self::Name(value) => value.parse().ok(),
        }
    }

    /// Returns the string selector value.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        match self {
            Self::Id(_) => None,
            Self::Name(value) => Some(value),
        }
    }
}

/// A `{ x, y, z }` vector on the wire (a JSON object, not an array).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Vec3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

/// A `{ x, y, z, w }` vector on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Vec4 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub w: f32,
}

/// A resolved entity reference (the selection echo).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct EntityRef {
    pub id: Uuid,
    pub name: String,
}

/// The window identity + present options the exported `saffron-player` boots with, written to the
/// staged `app.json`. `#[serde(default)]` lets a partial manifest fall back field by field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", default)]
#[ts(export)]
pub struct AppManifest {
    /// The window title (and the default export folder name).
    pub title: String,
    /// The initial window width in pixels.
    pub width: u32,
    /// The initial window height in pixels.
    pub height: u32,
    /// Whether the window starts fullscreen.
    pub fullscreen: bool,
    /// Whether to present with vsync.
    pub vsync: bool,
}

impl Default for AppManifest {
    fn default() -> Self {
        Self {
            title: "Saffron App".to_string(),
            width: 1280,
            height: 720,
            fullscreen: false,
            vsync: true,
        }
    }
}

/// `export-app` params: cook the open project into a platform-native standalone application at
/// `outputDir`, using `app` as its runtime manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ExportAppParams {
    /// The destination path for the staged app (created if absent; `.app` is appended on macOS).
    pub output_dir: String,
    /// The runtime manifest to write into the staged `app.json`.
    pub app: AppManifest,
}

/// Stored bytes one cell facet occupies across a map's cells.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ExportVegetationFacetDto {
    /// Canonical facet name.
    pub facet: String,
    /// Cells that carry it.
    pub cells: String,
    /// Stored bytes it occupies across those cells.
    pub bytes: String,
}

/// One map's contribution to an exported package.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ExportVegetationMapDto {
    /// The authored map.
    pub map: Uuid,
    /// Identity of the manifest the package binds.
    pub manifest_identity: String,
    /// Compiled families the manifest names.
    pub plants: String,
    /// Cooked cells the manifest names.
    pub cells: String,
    /// Artifacts the manifest names that the store does not hold.
    pub missing: String,
    /// Whether the generation ships an initial persistent-state baseline.
    pub baseline: bool,
    /// Macro plants across every cell the manifest names.
    pub macro_plants: String,
    /// Stored bytes per cell facet, in canonical section order.
    pub facets: Vec<ExportVegetationFacetDto>,
}

/// `export-app` result: the staged app directory and any non-fatal warnings raised during cook.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ExportAppResult {
    /// The staged application root (a `.app` bundle on macOS, a flat directory elsewhere).
    pub path: String,
    /// Non-fatal warnings (e.g. a material that failed to pre-bake), for the editor to surface.
    pub warnings: Vec<String>,
    /// What the cooked vegetation closure contributed: one entry per map with a generation.
    pub vegetation: Vec<ExportVegetationMapDto>,
    /// Bytes the vegetation closure carries.
    pub vegetation_bytes: String,
    /// Attribution lines written to `ATTRIBUTION.txt`, one per packaged source that requires it.
    pub attributions: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
pub struct PingParams {}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
pub struct EmptyParams {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PingResult {
    pub pong: bool,
    pub engine: String,
    pub version: String,
    pub pid: i32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PathResult {
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct QuitResult {
    pub quitting: bool,
}
