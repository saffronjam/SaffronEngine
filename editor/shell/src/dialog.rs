//! Native file/folder pickers backed by `rfd` over the XDG
//! desktop portal (no GTK, no main-thread requirement), run on the shared tokio runtime from the IPC
//! worker thread so the CEF pump never stalls. Returns the chosen path(s) as the frontend's
//! `open`/`save` expect (a string, a string array for `multiple`, or `null` when cancelled).

use crate::async_rt::rt;
use crate::control::ControlError;
use rfd::AsyncFileDialog;
use serde_json::{Value, json};

/// `dialog_open` — a file or folder picker. `{ multiple, directory, filters, defaultPath, title }`.
pub fn open(args: &Value) -> Result<Value, ControlError> {
    let options = args.get("options").cloned().unwrap_or(Value::Null);
    let multiple = flag(&options, "multiple");
    let directory = flag(&options, "directory");
    rt().block_on(async move {
        let dialog = configure(AsyncFileDialog::new(), &options);
        if directory {
            Ok(path_or_null(dialog.pick_folder().await))
        } else if multiple {
            match dialog.pick_files().await {
                Some(handles) => Ok(Value::Array(
                    handles
                        .iter()
                        .map(|h| Value::String(h.path().to_string_lossy().into_owned()))
                        .collect(),
                )),
                None => Ok(Value::Null),
            }
        } else {
            Ok(path_or_null(dialog.pick_file().await))
        }
    })
}

/// `dialog_save` — a save picker. `{ filters, defaultPath, title }`.
pub fn save(args: &Value) -> Result<Value, ControlError> {
    let options = args.get("options").cloned().unwrap_or(Value::Null);
    rt().block_on(async move {
        let dialog = configure(AsyncFileDialog::new(), &options);
        Ok(path_or_null(dialog.save_file().await))
    })
}

/// Apply the shared `{ filters, defaultPath, title }` options to a dialog. `defaultPath` seeds both
/// the starting directory and (for save) the file name, matching the platform dialog's behaviour.
fn configure(mut dialog: AsyncFileDialog, options: &Value) -> AsyncFileDialog {
    if let Some(title) = options.get("title").and_then(Value::as_str) {
        dialog = dialog.set_title(title);
    }
    if let Some(filters) = options.get("filters").and_then(Value::as_array) {
        for filter in filters {
            let name = filter.get("name").and_then(Value::as_str).unwrap_or("");
            let exts: Vec<&str> = filter
                .get("extensions")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            dialog = dialog.add_filter(name, &exts);
        }
    }
    if let Some(default) = options.get("defaultPath").and_then(Value::as_str) {
        let path = std::path::Path::new(default);
        if let Some(parent) = path.parent().filter(|p| p.is_dir()) {
            dialog = dialog.set_directory(parent);
        }
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            dialog = dialog.set_file_name(name);
        }
    }
    dialog
}

fn flag(options: &Value, key: &str) -> bool {
    options.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn path_or_null(handle: Option<rfd::FileHandle>) -> Value {
    match handle {
        Some(h) => json!(h.path().to_string_lossy()),
        None => Value::Null,
    }
}
