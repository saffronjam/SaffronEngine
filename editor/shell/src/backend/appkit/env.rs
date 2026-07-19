//! macOS spawn-env, path, and opener conventions: Vulkan driver and validation-layer discovery,
//! `shm_unlink` cleanup (macOS POSIX shm has no filesystem path), per-user `$TMPDIR` sockets, the
//! Application Support data dir, and the `open`-based openers.

use std::path::PathBuf;
use std::process::Command;

/// Homebrew's MoltenVK ICD manifests, Apple-silicon prefix first.
const MOLTENVK_ICDS: [&str; 2] = [
    "/opt/homebrew/etc/vulkan/icd.d/MoltenVK_icd.json",
    "/usr/local/etc/vulkan/icd.d/MoltenVK_icd.json",
];

/// Homebrew's validation-layer manifests and matching dynamic-library directories.
const VALIDATION_LAYERS: [(&str, &str); 2] = [
    (
        "/opt/homebrew/opt/vulkan-validationlayers/share/vulkan/explicit_layer.d",
        "/opt/homebrew/opt/vulkan-validationlayers/lib",
    ),
    (
        "/usr/local/opt/vulkan-validationlayers/share/vulkan/explicit_layer.d",
        "/usr/local/opt/vulkan-validationlayers/lib",
    ),
];

/// Point the engine's Vulkan loader at MoltenVK and the validation layer when the environment does
/// not already say where to look. Explicit Vulkan discovery variables win.
pub fn engine_env(command: &mut Command) {
    if std::env::var_os("VK_ICD_FILENAMES").is_none()
        && let Some(icd) = MOLTENVK_ICDS
            .iter()
            .find(|path| std::path::Path::new(path).exists())
    {
        command.env("VK_ICD_FILENAMES", icd);
    }
    if std::env::var_os("VK_LAYER_PATH").is_none()
        && let Some((manifest_dir, library_dir)) =
            VALIDATION_LAYERS
                .iter()
                .find(|(manifest_dir, library_dir)| {
                    std::path::Path::new(manifest_dir).is_dir()
                        && std::path::Path::new(library_dir).is_dir()
                })
    {
        command.env("VK_LAYER_PATH", manifest_dir);
        let mut fallback = std::ffi::OsString::from(library_dir);
        if let Some(existing) = std::env::var_os("DYLD_FALLBACK_LIBRARY_PATH")
            && !existing.is_empty()
        {
            fallback.push(":");
            fallback.push(existing);
        }
        command.env("DYLD_FALLBACK_LIBRARY_PATH", fallback);
    }
}

/// Best-effort removal of an engine viewport segment after a killed engine (a clean exit unlinks
/// its own). macOS POSIX shm objects have no filesystem path; `shm_unlink` removes the name.
pub fn remove_viewport_shm(name: &str) {
    if let Ok(cname) = std::ffi::CString::new(name) {
        // SAFETY: `cname` is a valid NUL-terminated shm object name; `shm_unlink` only removes
        // the name and has no other effect.
        unsafe {
            libc::shm_unlink(cname.as_ptr());
        }
    }
}

/// Where per-PID control sockets live: the per-user `$TMPDIR` (short enough that the socket path
/// fits `sockaddr_un`'s ~104-char limit), else `/tmp`.
pub fn runtime_socket_dir() -> PathBuf {
    std::env::var_os("TMPDIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
}

/// The installed-build app-data root: `~/Library/Application Support/saffron-anima`.
pub fn platform_data_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Library/Application Support/saffron-anima")
}

/// URL openers, tried in order.
pub fn browser_opener_candidates() -> &'static [&'static [&'static str]] {
    &[&["open"]]
}

/// VS Code openers, tried in order: the CLI shim, then the app by name.
pub fn vscode_opener_candidates() -> &'static [&'static [&'static str]] {
    &[&["code"], &["open", "-a", "Visual Studio Code"]]
}
