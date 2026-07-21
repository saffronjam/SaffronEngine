//! The Asset Store command surface (`store_*` / `connector_*`). Runs on
//! an IPC worker thread; the async connector methods (reqwest/keyring) execute on a shared multi-thread
//! tokio runtime via `block_on`. Download progress streams back over the frontend `Channel` as
//! `channel:{id}` events. The final import crosses to the host over the control plane.

use crate::async_rt::rt as runtime;
use crate::connectors::{
    AssetPart, Credentials, SearchQuery, StoreKind, StoreResult, run_loopback_login,
};
use crate::control::{ControlError, control_request_with_params};
use crate::state::ShellState;
use serde::Serialize;
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

pub fn dispatch(
    state: &Arc<ShellState>,
    command: &str,
    args: Value,
) -> Result<Value, ControlError> {
    match command {
        "store_list_connectors" => to_value(&state.connectors.infos()),
        "store_search_session" => {
            let query: SearchQuery = from_arg(&args, "query")?;
            state
                .connectors
                .start_session(query)
                .map(Value::String)
                .map_err(|err| ControlError::from(err.to_string()))
        }
        "store_search_more" => {
            let session = str_arg(&args, "session")?;
            let count = args.get("count").and_then(Value::as_u64).unwrap_or(0) as usize;
            let handle = state
                .connectors
                .session(&session)
                .ok_or_else(|| ControlError::from("search session expired".to_owned()))?;
            let (results, exhausted) = runtime().block_on(async {
                let mut guard = handle.lock().await;
                let results = guard.next_batch(count).await;
                (results, guard.all_exhausted())
            });
            Ok(json!({ "results": results, "exhausted": exhausted }))
        }
        "store_asset_parts" => {
            let result: StoreResult = from_arg(&args, "result")?;
            let connector = connector_for(state, &result)?;
            let parts = runtime()
                .block_on(connector.parts(&result))
                .map_err(|err| ControlError::from(err.to_string()))?;
            to_value(&parts)
        }
        "store_asset_gallery" => {
            let result: StoreResult = from_arg(&args, "result")?;
            let connector = connector_for(state, &result)?;
            let gallery = runtime()
                .block_on(connector.gallery(&result))
                .map_err(|err| ControlError::from(err.to_string()))?;
            to_value(&gallery)
        }
        "store_import" => {
            let result: StoreResult = from_arg(&args, "result")?;
            let resolution = args
                .get("resolution")
                .and_then(Value::as_str)
                .map(str::to_owned);
            let channel = channel_id(&args, "onProgress");
            let connector = connector_for(state, &result)?;
            let mut descriptor = result.import_descriptor.clone();
            descriptor.resolution = resolution;
            let report = progress_reporter(state, channel);
            let path = runtime()
                .block_on(connector.download(&descriptor, &report))
                .map_err(|err| ControlError::from(err.to_string()))?;
            let path = path.to_string_lossy().into_owned();
            let attribution = attribution_of(&result);
            // The kind selects the host importer: model → .smodel, material/texture → .smat, hdri
            // → an environment texture.
            let (cmd, params) = match result.kind {
                StoreKind::Model => (
                    "import-model",
                    json!({ "path": path, "attribution": attribution }),
                ),
                StoreKind::Material | StoreKind::Texture => (
                    "material-import",
                    json!({ "path": path, "name": result.name, "attribution": attribution }),
                ),
                StoreKind::Hdri => ("import-texture", json!({ "path": path, "role": "hdri" })),
            };
            finish_import(state, cmd, params, &result.name)
        }
        "store_import_part" => {
            let result: StoreResult = from_arg(&args, "result")?;
            let part: AssetPart = from_arg(&args, "part")?;
            let resolution = args
                .get("resolution")
                .and_then(Value::as_str)
                .map(str::to_owned);
            let connector = connector_for(state, &result)?;
            let path = runtime()
                .block_on(connector.download_part(&part, resolution.as_deref()))
                .map_err(|err| ControlError::from(err.to_string()))?;
            let path = path.to_string_lossy().into_owned();
            let attribution = attribution_of(&result);
            let (cmd, params) = match part.import_kind {
                // The engine owns role→colorspace; send the connector's map role for a texture.
                StoreKind::Texture => {
                    ("import-texture", json!({ "path": path, "role": part.role }))
                }
                StoreKind::Hdri => ("import-texture", json!({ "path": path, "role": "hdri" })),
                StoreKind::Model => (
                    "import-model",
                    json!({ "path": path, "attribution": attribution }),
                ),
                StoreKind::Material => (
                    "material-import",
                    json!({ "path": path, "name": part.label, "attribution": attribution }),
                ),
            };
            finish_import(state, cmd, params, &part.label)
        }
        "connector_set_secret" => {
            let id = str_arg(&args, "connectorId")?;
            let secret = str_arg(&args, "secret")?;
            Credentials::global()
                .set_secret(&id, &secret)
                .map(|_| Value::Null)
                .map_err(|err| ControlError::from(err.to_string()))
        }
        "connector_clear_secret" => {
            let id = str_arg(&args, "connectorId")?;
            Credentials::global()
                .delete_secret(&id)
                .map(|_| Value::Null)
                .map_err(|err| ControlError::from(err.to_string()))
        }
        "connector_secret_status" => Ok(Value::Bool(
            Credentials::global().has_secret(&str_arg(&args, "connectorId")?),
        )),
        "connector_login" => {
            let id = str_arg(&args, "connectorId")?;
            let config = state.connectors.oauth_config(&id).ok_or_else(|| {
                ControlError::from(format!("connector '{id}' has no OAuth login"))
            })?;
            // The loopback flow blocks (opens the browser, waits for the callback); we are already
            // off the CEF UI thread, so run it here.
            run_loopback_login(&config)
                .map(|_| Value::Null)
                .map_err(|err| ControlError::from(err.to_string()))
        }
        other => Err(ControlError::bridge(format!(
            "unknown store command '{other}'"
        ))),
    }
}

fn connector_for(
    state: &Arc<ShellState>,
    result: &StoreResult,
) -> Result<Arc<dyn crate::connectors::StoreConnector>, ControlError> {
    state
        .connectors
        .connector(&result.store.id)
        .ok_or_else(|| ControlError::from(format!("unknown connector '{}'", result.store.id)))
}

/// A download-progress sink that streams the fraction to the frontend `Channel`, throttled to
/// whole-percent changes so a fast download doesn't flood the webview.
fn progress_reporter(
    state: &Arc<ShellState>,
    channel: Option<u64>,
) -> impl Fn(f64) + Send + Sync + use<> {
    let state = Arc::clone(state);
    let last_pct = AtomicU32::new(u32::MAX);
    move |fraction: f64| {
        let pct = (fraction.clamp(0.0, 1.0) * 100.0).round() as u32;
        if pct != last_pct.swap(pct, Ordering::Relaxed)
            && let Some(id) = channel
        {
            state.emit(&format!("channel:{id}"), json!(fraction));
        }
    }
}

/// The attribution record (license/author/source) sent with every import.
fn attribution_of(result: &StoreResult) -> Value {
    json!({
        "licenseId": result.license.id,
        "requiresAttribution": result.license.requires_attribution,
        "licenseUrl": result.license.url,
        "author": result.author,
        "sourceUrl": result.source_url,
        "storeId": result.store.id,
    })
}

/// Send the import command to the host and return the `{ id, name }` the frontend shows.
fn finish_import(
    state: &Arc<ShellState>,
    cmd: &str,
    params: Value,
    fallback_name: &str,
) -> Result<Value, ControlError> {
    let reply = control_request_with_params(&state.socket_path, cmd, params)?;
    Ok(json!({
        "id": reply
            .get("id")
            .or_else(|| reply.get("texture"))
            .and_then(Value::as_str)
            .unwrap_or_default(),
        "name": reply
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or(fallback_name),
    }))
}

fn channel_id(args: &Value, key: &str) -> Option<u64> {
    args.get(key)
        .and_then(|channel| channel.get("__saffronChannel"))
        .and_then(Value::as_u64)
}

fn str_arg(args: &Value, key: &str) -> Result<String, ControlError> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| ControlError::from(format!("missing string arg '{key}'")))
}

fn from_arg<T: serde::de::DeserializeOwned>(args: &Value, key: &str) -> Result<T, ControlError> {
    serde_json::from_value(args.get(key).cloned().unwrap_or(Value::Null))
        .map_err(|err| ControlError::from(format!("bad arg '{key}': {err}")))
}

fn to_value<T: Serialize>(value: &T) -> Result<Value, ControlError> {
    serde_json::to_value(value).map_err(|err| ControlError::from(format!("encode reply: {err}")))
}
