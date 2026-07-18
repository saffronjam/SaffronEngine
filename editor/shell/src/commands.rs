//! Phase 4 command surface: the name-dispatched command handlers.
//! The frontend bridge calls `invoke(name, args)`, which the IPC transport delivers here as
//! `{ command, args }`. `control` is the generic passthrough (all ~120 typed control commands); the
//! rest are the dedicated non-passthrough commands. Runs on an IPC worker thread — every handler is
//! thread-safe (socket / `std::process` / `fs`).
//!
//! Every command group is wired: `control` (passthrough), engine lifecycle, file/OS/trace helpers,
//! settings/recents, window controls, viewport geometry (`set_viewport_*`/`viewport_refresh_hz`),
//! the Asset Store (`store_*`/`connector_*` → [`crate::store_commands`]), and native dialogs
//! (`dialog_*` → [`crate::dialog`]). The only display-gated remnants are the *visual* pieces behind
//! these commands — the subsurface present loop and the `saffron-img://` thumbnail scheme.

use crate::control::{ControlError, control_request_with_params};
use crate::engine;
use crate::geometry::{app_data_dir, ensure_app_dirs, userdata_dir};
use crate::settings;
use crate::state::{ResizeEdge, ShellRequest, ShellState, WindowAction};
use crate::viewport;
use serde::Serialize;
use serde_json::{Value, json};
use std::sync::Arc;

/// The recent-projects MRU is bounded (drop the tail past this).
const RECENTS_CAP: usize = 12;

/// Dispatch one `invoke(command, args)` to its handler, returning the JSON result the bridge passes
/// to `onSuccess`, or a `ControlError` it passes to `onFailure`.
pub fn dispatch(
    state: &Arc<ShellState>,
    command: &str,
    args: Value,
) -> Result<Value, ControlError> {
    match command {
        "control" => {
            let cmd = str_arg(&args, "cmd")?;
            let params = args.get("params").cloned().unwrap_or(Value::Null);
            control_request_with_params(&state.socket_path, &cmd, params)
        }
        "start_engine" => {
            engine::start_engine(state).map_err(|err| err.to_string())?;
            Ok(Value::Null)
        }
        "engine_alive" => Ok(Value::Bool(engine::child_alive(&state.engine))),
        "quit_engine" => {
            engine::teardown(state);
            Ok(Value::Null)
        }
        "write_file" => {
            let path = str_arg(&args, "path")?;
            let bytes = bytes_arg(&args, "bytes")?;
            std::fs::write(&path, bytes).map_err(|err| format!("write {path}: {err}"))?;
            Ok(Value::Null)
        }
        "open_external" => {
            crate::os::open_url_in_browser(&str_arg(&args, "url")?)?;
            Ok(Value::Null)
        }
        "open_in_vscode" => {
            crate::os::open_in_vscode(&str_arg(&args, "path")?)?;
            Ok(Value::Null)
        }
        "open_project_folder" => {
            let absolute = crate::os::absolutize(&str_arg(&args, "path")?);
            crate::os::open_url_in_browser(&absolute.to_string_lossy())?;
            Ok(Value::Null)
        }
        "app_data_info" => {
            ensure_app_dirs().map_err(|err| err.to_string())?;
            Ok(json!({
                "appDataDir": app_data_dir().to_string_lossy(),
                "userdataDir": userdata_dir().to_string_lossy(),
                "envProject": std::env::var_os("SAFFRON_PROJECT").is_some(),
                "scratchProject": std::env::var_os("SAFFRON_SCRATCH_PROJECT").is_some(),
            }))
        }
        "serve_trace" => {
            let bytes = bytes_arg(&args, "bytes")?;
            Ok(Value::String(state.serve_trace(bytes)?))
        }
        // Window controls (`getCurrentWindow()`): actions marshal to the main thread via the inbox;
        // reads come off the shared geometry tracker. `window_show` reveals the toplevel (the shell
        // also auto-reveals after the first paint, so this is idempotent).
        "window_minimize" => {
            state.post(ShellRequest::Window(WindowAction::Minimize));
            Ok(Value::Null)
        }
        "window_toggle_maximize" => {
            state.post(ShellRequest::Window(WindowAction::ToggleMaximize));
            Ok(Value::Null)
        }
        "window_start_resize" => {
            let direction = str_arg(&args, "direction")?;
            let edge = ResizeEdge::from_wire(&direction).ok_or_else(|| {
                ControlError::from(format!(
                    "window_start_resize: unknown direction '{direction}'"
                ))
            })?;
            state.post(ShellRequest::Window(WindowAction::StartResize(edge)));
            Ok(Value::Null)
        }
        "set_pointer_lock" => {
            let locked = args.get("locked").and_then(Value::as_bool).unwrap_or(false);
            state.post(ShellRequest::Window(WindowAction::SetPointerLock(locked)));
            Ok(Value::Null)
        }
        "window_show" => {
            state.post(ShellRequest::Window(WindowAction::Show));
            Ok(Value::Null)
        }
        "window_close" => {
            state.request_exit();
            Ok(Value::Null)
        }
        "window_scale_factor" => Ok(json!(state.window_scale())),
        "window_is_maximized" => Ok(Value::Bool(state.window_maximized())),
        // Viewport geometry: the lifecycle commands write the shared cells the present loop reads.
        // The bounds/park state applies even before the (display-validated) present loop runs.
        "set_viewport_bounds" => {
            let view = str_arg(&args, "view")?;
            let which = viewport_view(&view)?;
            let bounds = args.get("bounds").ok_or_else(|| {
                ControlError::from("set_viewport_bounds: missing bounds".to_owned())
            })?;
            let f = |key: &str, default: f64| {
                bounds.get(key).and_then(Value::as_f64).unwrap_or(default)
            };
            let (x, y, width, height, scale) = (
                f("x", 0.0),
                f("y", 0.0),
                f("width", 0.0),
                f("height", 0.0),
                f("scale", 1.0),
            );
            state.viewports.view(which).set_bounds(
                x.round() as i32,
                y.round() as i32,
                width.round() as i32,
                height.round() as i32,
            );
            // Resizing the engine's render target recreates its offscreen chain (expensive), so only
            // the settled (debounced) bounds do it; live drag ticks stretch the current frame via the
            // subsurface. Ignore failures while the engine boots.
            if args
                .get("resizeEngine")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                let w = ((width * scale).round() as i64).max(1);
                let h = ((height * scale).round() as i64).max(1);
                let _ = control_request_with_params(
                    &state.socket_path,
                    "set-viewport-size",
                    json!({ "view": view, "width": w, "height": h }),
                );
            }
            Ok(Value::Null)
        }
        "set_viewport_parked" => {
            let view = str_arg(&args, "view")?;
            let parked = args.get("parked").and_then(Value::as_bool).unwrap_or(false);
            state
                .viewports
                .view(viewport_view(&view)?)
                .set_parked(parked);
            Ok(Value::Null)
        }
        "viewport_refresh_hz" => Ok(json!(f64::from(state.viewports.refresh_mhz()) / 1000.0)),
        "load_editor_settings" => {
            ensure_app_dirs().map_err(|err| err.to_string())?;
            to_value(&settings::read_settings())
        }
        "save_editor_settings" => {
            ensure_app_dirs().map_err(|err| err.to_string())?;
            let value = args.get("settings").cloned().unwrap_or(Value::Null);
            let parsed: settings::EditorSettings = serde_json::from_value(value)
                .map_err(|err| format!("save_editor_settings: bad settings: {err}"))?;
            settings::write_settings(&parsed)?;
            Ok(Value::Null)
        }
        "list_recent_projects" => {
            ensure_app_dirs().map_err(|err| err.to_string())?;
            let mut recents = settings::read_recents();
            recents
                .projects
                .retain(|project| std::path::Path::new(&project.path).exists());
            recents.projects.truncate(RECENTS_CAP);
            settings::write_recents(&recents)?;
            to_value(&recents)
        }
        "remember_recent_project" => {
            ensure_app_dirs().map_err(|err| err.to_string())?;
            let value = args.get("project").cloned().unwrap_or(Value::Null);
            let project: settings::RecentProject = serde_json::from_value(value)
                .map_err(|err| format!("remember_recent_project: bad project: {err}"))?;
            let mut recents = settings::read_recents();
            recents
                .projects
                .retain(|recent| recent.path != project.path);
            recents.projects.insert(0, project);
            recents.projects.truncate(RECENTS_CAP);
            settings::write_recents(&recents)?;
            to_value(&recents)
        }
        "dialog_open" => crate::dialog::open(&args),
        "dialog_save" => crate::dialog::save(&args),
        other if other.starts_with("store_") || other.starts_with("connector_") => {
            crate::store_commands::dispatch(state, command, args)
        }
        other => Err(ControlError::coded(
            format!("unknown command '{other}'"),
            "unknown-command",
        )),
    }
}

fn to_value<T: Serialize>(value: &T) -> Result<Value, ControlError> {
    serde_json::to_value(value).map_err(|err| ControlError::from(format!("encode reply: {err}")))
}

/// Resolve a wire view token (`scene` / `assetPreview`) to a `View`, rejecting anything else.
fn viewport_view(view: &str) -> Result<viewport::View, ControlError> {
    viewport::View::from_wire(view)
        .ok_or_else(|| ControlError::from(format!("unknown viewport view '{view}'")))
}

fn str_arg(args: &Value, key: &str) -> Result<String, ControlError> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| ControlError::from(format!("missing string arg '{key}'")))
}

/// Bytes cross the JSON wire as an array of `u8` (`number[]`); the Phase-5 bridge sends
/// `Array.from(uint8array)`. The router shifts large payloads to shared memory (its size threshold).
fn bytes_arg(args: &Value, key: &str) -> Result<Vec<u8>, ControlError> {
    serde_json::from_value(args.get(key).cloned().unwrap_or(Value::Null))
        .map_err(|err| ControlError::from(format!("bad bytes arg '{key}': {err}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> Arc<ShellState> {
        Arc::new(ShellState::default())
    }

    #[test]
    fn unknown_command_is_coded() {
        let err = dispatch(&state(), "no_such_command", json!({})).unwrap_err();
        assert_eq!(err.code.as_deref(), Some("unknown-command"));
    }

    #[test]
    fn app_data_info_reports_the_dirs() {
        let out = dispatch(&state(), "app_data_info", json!({})).unwrap();
        assert!(out.get("appDataDir").and_then(Value::as_str).is_some());
        assert!(out.get("userdataDir").and_then(Value::as_str).is_some());
        assert!(out.get("envProject").and_then(Value::as_bool).is_some());
    }

    #[test]
    fn missing_required_arg_errors() {
        // `open_external` needs a `url`; absent, it fails before spawning anything.
        let err = dispatch(&state(), "open_external", json!({})).unwrap_err();
        assert!(err.message.contains("url"));
    }
}
