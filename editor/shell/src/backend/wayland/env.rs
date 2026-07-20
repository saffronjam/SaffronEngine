//! Linux spawn-env, path, and opener conventions: the toolbox's NVIDIA ICD guard for the spawned
//! host, `/dev/shm`-backed POSIX shm cleanup, XDG directories, and the flatpak-escape openers.

use std::path::PathBuf;
use std::process::Command;

/// The host's NVIDIA ICD, mounted from the host into the toolbox. The engine is a Vulkan/ash
/// program; pointing it here keeps it on hardware instead of llvmpipe. (CEF's own GPU process
/// reaches the GPU via GL/EGL/ANGLE, not this var.)
const NVIDIA_ICD: &str = "/run/host/usr/share/vulkan/icd.d/nvidia_icd.x86_64.json";

/// The toolbox ships only Mesa ICD manifests; point Vulkan at the host's NVIDIA ICD so the
/// engine renders on hardware.
pub fn engine_env(command: &mut Command) {
    if std::env::var_os("VK_ICD_FILENAMES").is_none() && std::path::Path::new(NVIDIA_ICD).exists() {
        command.env("VK_ICD_FILENAMES", NVIDIA_ICD);
    }
}

/// Best-effort removal of an engine viewport segment after a killed engine (a clean exit unlinks
/// its own). Linux exposes POSIX shm as `/dev/shm` files.
pub fn remove_viewport_shm(name: &str) {
    let _ = std::fs::remove_file(format!("/dev/shm{name}"));
}

/// Where per-PID control sockets live: `XDG_RUNTIME_DIR`, else `/tmp`.
pub fn runtime_socket_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
}

/// The installed-build app-data root: the XDG data directory (`$XDG_DATA_HOME`, else
/// `~/.local/share`) under `saffron-anima`.
pub fn platform_data_dir() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("saffron-anima")
}

/// URL openers, tried in order: the flatpak host escape first (the toolbox has no `xdg-utils`),
/// then the desktop conventions.
pub fn browser_opener_candidates() -> &'static [&'static [&'static str]] {
    &[
        &["flatpak-spawn", "--host", "xdg-open"],
        &["xdg-open"],
        &["gio", "open"],
        &["gnome-open"],
        &["kde-open"],
    ]
}

/// VS Code openers, tried in order (host escape first).
pub fn vscode_opener_candidates() -> &'static [&'static [&'static str]] {
    &[&["flatpak-spawn", "--host", "code"], &["code"]]
}
