//! The name-dispatched command handlers. The frontend bridge calls `invoke(name, args)`, which the
//! IPC transport delivers here as `{ command, args }`: `control` is the generic passthrough for
//! every typed control command, the rest are the dedicated shell commands (engine lifecycle,
//! file/OS/trace helpers, settings and recents, window controls, viewport geometry, the Asset Store,
//! and native dialogs). Runs on an IPC worker thread, so every handler is thread-safe.

use crate::control::{ControlError, control_request_with_params};
use crate::engine;
use crate::geometry::{app_data_dir, ensure_app_dirs, userdata_dir};
use crate::settings;
use crate::state::{FlyRequest, ResizeEdge, ShellRequest, ShellState, WindowAction};
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
        "session_start" => {
            let path = args
                .get("path")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_owned);
            let create = match args.get("create") {
                Some(spec) => Some((
                    spec.get("name")
                        .and_then(Value::as_str)
                        .ok_or_else(|| ControlError::bridge("create.name required"))?
                        .to_owned(),
                    spec.get("displayName")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                )),
                None => None,
            };
            engine::start_session(state, engine::SessionIntent { path, create })
                .map_err(|err| err.to_string())?;
            Ok(Value::Null)
        }
        "session_stop" => {
            engine::stop_session(state);
            Ok(Value::Null)
        }
        "session_status" => Ok(engine::session_status(state)),
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
        "fly_stream_start" => {
            state.post(ShellRequest::Fly(FlyRequest::Start {
                forward: str_arg(&args, "forward")?,
                back: str_arg(&args, "back")?,
                left: str_arg(&args, "left")?,
                right: str_arg(&args, "right")?,
                up: str_arg(&args, "up")?,
                down: str_arg(&args, "down")?,
            }));
            Ok(Value::Null)
        }
        "fly_stream_stop" => {
            state.post(ShellRequest::Fly(FlyRequest::Stop));
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
        "remove_recent_project" => {
            ensure_app_dirs().map_err(|err| err.to_string())?;
            let path = str_arg(&args, "path")?;
            let mut recents = settings::read_recents();
            settings::remove_recent(&mut recents, &path);
            settings::write_recents(&recents)?;
            to_value(&recents)
        }
        "delete_project" => {
            ensure_app_dirs().map_err(|err| err.to_string())?;
            let path = str_arg(&args, "path")?;
            let target =
                resolve_project_delete_target(std::path::Path::new(&path), &userdata_dir())?;
            std::fs::remove_dir_all(&target)
                .map_err(|err| format!("delete '{}': {err}", target.display()))?;
            let mut recents = settings::read_recents();
            settings::remove_recent(&mut recents, &path);
            settings::write_recents(&recents)?;
            to_value(&recents)
        }
        "project_name_available" => {
            ensure_app_dirs().map_err(|err| err.to_string())?;
            let name = str_arg(&args, "name")?;
            if name.is_empty() || name.contains(['/', '\\']) {
                return Ok(Value::Bool(false));
            }
            Ok(Value::Bool(!userdata_dir().join(&name).exists()))
        }
        "dialog_open" => crate::dialog::open(&args),
        "dialog_save" => crate::dialog::save(&args),
        other if other.starts_with("store_") || other.starts_with("connector_") => {
            crate::store_commands::dispatch(state, command, args)
        }
        other => Err(ControlError::bridge(format!("unknown command '{other}'"))),
    }
}

fn to_value<T: Serialize>(value: &T) -> Result<Value, ControlError> {
    serde_json::to_value(value).map_err(|err| ControlError::from(format!("encode reply: {err}")))
}

/// Resolve + fence a project-delete request: the selection (a project dir or its `project.json`)
/// must canonicalize to a directory strictly under `userdata_root` that contains `project.json`.
/// Projects elsewhere on disk are never deleted through the editor — the picker offers them
/// Hide only.
fn resolve_project_delete_target(
    selection: &std::path::Path,
    userdata_root: &std::path::Path,
) -> Result<std::path::PathBuf, String> {
    let dir = if selection
        .file_name()
        .is_some_and(|name| name == "project.json")
    {
        selection.parent().unwrap_or(selection)
    } else {
        selection
    };
    let dir = dir
        .canonicalize()
        .map_err(|err| format!("delete '{}': {err}", dir.display()))?;
    let root = userdata_root
        .canonicalize()
        .map_err(|err| format!("delete: userdata root: {err}"))?;
    if !dir.starts_with(&root) || dir == root {
        return Err(format!(
            "refusing to delete '{}': not a project under '{}'",
            dir.display(),
            root.display()
        ));
    }
    if !dir.join("project.json").exists() {
        return Err(format!(
            "refusing to delete '{}': no project.json inside",
            dir.display()
        ));
    }
    Ok(dir)
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
    fn delete_target_is_fenced_to_userdata_with_a_project_inside() {
        let root = std::env::temp_dir().join(format!("anima-delete-fence-{}", std::process::id()));
        let userdata = root.join("userdata");
        let project = userdata.join("scratch");
        std::fs::create_dir_all(&project).unwrap();

        // No project.json yet: refused even under the root.
        assert!(resolve_project_delete_target(&project, &userdata).is_err());
        std::fs::write(project.join("project.json"), "{}").unwrap();

        // The dir and its project.json both resolve to the dir.
        let via_dir = resolve_project_delete_target(&project, &userdata).unwrap();
        let via_json =
            resolve_project_delete_target(&project.join("project.json"), &userdata).unwrap();
        assert_eq!(via_dir, via_json);
        assert!(via_dir.ends_with("scratch"));

        // Outside the root (even with a project.json), and the root itself: refused.
        let outside = root.join("elsewhere");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("project.json"), "{}").unwrap();
        assert!(resolve_project_delete_target(&outside, &userdata).is_err());
        assert!(resolve_project_delete_target(&userdata, &userdata).is_err());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn remove_recent_drops_exactly_the_matching_row() {
        let row = |path: &str| settings::RecentProject {
            path: path.to_owned(),
            name: "p".to_owned(),
            display_name: "P".to_owned(),
            last_opened_at: String::new(),
        };
        let mut recents = settings::RecentProjects {
            projects: vec![row("/a"), row("/b"), row("/c")],
        };
        assert!(settings::remove_recent(&mut recents, "/b"));
        assert_eq!(recents.projects.len(), 2);
        assert!(!settings::remove_recent(&mut recents, "/b"));
        assert_eq!(recents.projects[0].path, "/a");
        assert_eq!(recents.projects[1].path, "/c");
    }

    #[test]
    fn session_status_reports_no_session_on_a_fresh_shell() {
        let out = dispatch(&state(), "session_status", json!({})).unwrap();
        assert_eq!(out.get("running"), Some(&Value::Bool(false)));
        assert_eq!(out.get("path").and_then(Value::as_str), Some(""));
    }

    #[test]
    fn session_start_requires_a_create_name() {
        // Fails arg validation before any spawn is attempted.
        let err = dispatch(&state(), "session_start", json!({ "create": {} })).unwrap_err();
        assert!(err.failure().message().contains("create.name"));
    }

    #[test]
    fn unknown_command_is_a_typed_bridge_failure() {
        let err = dispatch(&state(), "no_such_command", json!({})).unwrap_err();
        assert_eq!(err.failure().code(), "bridge");
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
        assert!(err.failure().message().contains("url"));
    }
}
