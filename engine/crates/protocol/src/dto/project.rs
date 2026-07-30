use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ProjectInfoDto {
    pub loaded: bool,
    pub root: String,
    pub path: String,
    pub name: String,
    pub display_name: String,
}

/// Coarse project-load lifecycle (mirrors `saffron_sceneedit::ProjectPhase`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum ProjectPhaseDto {
    Unloaded,
    Loading,
    Ready,
    Failed,
}

/// The current boot stage within `Loading` (mirrors `saffron_sceneedit::BootStage`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum BootStageDto {
    Manifest,
    Catalog,
    Scene,
    Install,
    Assets,
    Skybox,
    Accel,
    Ready,
    Failed,
}

/// Project-load phase + progress. `total == 0` means indeterminate (spinner). `version` is
/// monotonic; the editor dedups on it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ProjectStatusDto {
    pub phase: ProjectPhaseDto,
    pub stage: BootStageDto,
    pub done: i32,
    pub total: i32,
    pub label: String,
    pub current_item: String,
    pub error: String,
    pub version: i64,
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct NewProjectParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PathParams {
    pub path: String,
}

/// The per-project asset-store enablement block: which connectors the project has
/// enabled. Non-secret only — credentials live in the OS keyring, never here.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", default)]
#[ts(export)]
pub struct ProjectStoresDto {
    /// Enabled connector ids (e.g. `["polyhaven", "poly-pizza"]`).
    pub enabled: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct OptionalPathParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}
