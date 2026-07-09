//! Editor settings + recent-projects persistence. Both are
//! delta/transient UI memory under `appdata/` (siblings of `state.json`): settings holds only the
//! key-bindings the user changed (defaults live in the frontend registry), recents a bounded MRU
//! list. A missing or corrupt file falls back to defaults. Wire types are camelCase to match the
//! generated protocol.

use crate::geometry::app_data_dir;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditorSettings {
    #[serde(default)]
    pub key_bindings: HashMap<String, String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecentProject {
    pub path: String,
    pub name: String,
    pub display_name: String,
    pub last_opened_at: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct RecentProjects {
    pub projects: Vec<RecentProject>,
}

fn settings_path() -> PathBuf {
    app_data_dir().join("settings.json")
}

fn recents_path() -> PathBuf {
    app_data_dir().join("recent-projects.json")
}

/// A missing or corrupt settings file falls back to defaults (an empty delta map).
pub fn read_settings() -> EditorSettings {
    let Ok(text) = fs::read_to_string(settings_path()) else {
        return EditorSettings::default();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

pub fn write_settings(settings: &EditorSettings) -> Result<(), String> {
    let text = serde_json::to_string_pretty(settings)
        .map_err(|err| format!("encode editor settings: {err}"))?;
    fs::write(settings_path(), text).map_err(|err| format!("write editor settings: {err}"))
}

pub fn read_recents() -> RecentProjects {
    let Ok(text) = fs::read_to_string(recents_path()) else {
        return RecentProjects::default();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

pub fn write_recents(recents: &RecentProjects) -> Result<(), String> {
    let text = serde_json::to_string_pretty(recents)
        .map_err(|err| format!("encode recent projects: {err}"))?;
    fs::write(recents_path(), text).map_err(|err| format!("write recent projects: {err}"))
}
