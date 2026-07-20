//! OS-integration helpers shared by the command dispatch and the connectors' OAuth loopback: open a
//! URL/path in the host browser/editor (the opener candidates come from the platform backend), and
//! make an engine-relative path absolute. Stringly errors here (`Result<_, String>`)
//! because the sole consumers wrap them into their own typed errors at the boundary.

use crate::backend;
use crate::geometry::repo_root;
use std::path::PathBuf;

/// Engine-reported project paths are relative to the engine's cwd (the repo root); this process
/// spawns openers with its own cwd, so make the path absolute first.
pub fn absolutize(path: &str) -> PathBuf {
    let p = PathBuf::from(path);
    if p.is_absolute() {
        p
    } else {
        repo_root().join(p)
    }
}

pub fn open_url_in_browser(url: &str) -> Result<(), String> {
    spawn_first(backend::env::browser_opener_candidates(), url)
        .map_err(|err| format!("open {url}: {err}"))
}

pub fn open_in_vscode(path: &str) -> Result<(), String> {
    let absolute = absolutize(path);
    let target = absolute.to_string_lossy();
    spawn_first(backend::env::vscode_opener_candidates(), &target)
        .map_err(|err| format!("open {path} in vs code: {err}"))
}

/// Try each `argv` (program + fixed args) with `target` appended; succeed on the first that spawns.
fn spawn_first(candidates: &[&[&str]], target: &str) -> Result<(), String> {
    let mut last_err = String::from("no opener found");
    for argv in candidates {
        let (program, pre) = argv.split_first().expect("candidate is non-empty");
        match std::process::Command::new(program)
            .args(pre)
            .arg(target)
            .spawn()
        {
            Ok(_) => return Ok(()),
            Err(err) => last_err = format!("{program}: {err}"),
        }
    }
    Err(last_err)
}
