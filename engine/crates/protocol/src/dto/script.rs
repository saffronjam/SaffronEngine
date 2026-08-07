use crate::{EntitySelector, Uuid};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ScriptStatusResult {
    pub state: String,
    pub instances: i32,
    pub error_high_water: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ScriptErrorDto {
    pub seq: i64,
    pub entity: Uuid,
    pub script: String,
    pub message: String,
    pub tick: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DrainScriptErrorsParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DrainScriptErrorsResult {
    pub events: Vec<ScriptErrorDto>,
    pub high_water_seq: i64,
    pub oldest_seq: i64,
    pub overflowed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ScriptLogDto {
    pub seq: i64,
    pub entity: Uuid,
    pub message: String,
    pub epoch_ms: i64,
    pub tick: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DrainScriptLogsParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DrainScriptLogsResult {
    pub events: Vec<ScriptLogDto>,
    pub high_water_seq: i64,
    pub oldest_seq: i64,
    pub overflowed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct GetScriptSchemaParams {
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ScriptFieldDto {
    pub name: String,
    #[serde(rename = "type")]
    pub r#type: String,
    pub default_value: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct GetScriptSchemaResult {
    pub fields: Vec<ScriptFieldDto>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetScriptOverrideParams {
    pub entity: EntitySelector,
    pub slot: i32,
    pub name: String,
    pub value: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetScriptOverrideResult {
    pub script_path: String,
    pub overrides: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct CreateScriptParams {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct CreateScriptResult {
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ScriptInputParams {
    pub keys: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mouse_buttons: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mouse_x: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mouse_y: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scroll: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ScriptInputResult {
    pub keys: Vec<String>,
}
