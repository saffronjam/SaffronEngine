//! macOS spawn-env, path, and opener conventions: the MoltenVK ICD guard for the spawned host,
//! `shm_unlink` cleanup (macOS POSIX shm has no filesystem path), per-user `$TMPDIR` sockets, the
//! Application Support data dir, and the `open`-based openers.

use std::path::PathBuf;
use std::process::Command;

/// Homebrew's MoltenVK ICD manifests, Apple-silicon prefix first. macOS has no native Vulkan; the
/// engine's Vulkan loader finds MoltenVK through `VK_ICD_FILENAMES` (a non-`DYLD_` var, so it
/// survives the spawn — macOS strips `DYLD_*` across exec).
const MOLTENVK_ICDS: [&str; 2] = [
    "/opt/homebrew/etc/vulkan/icd.d/MoltenVK_icd.json",
    "/usr/local/etc/vulkan/icd.d/MoltenVK_icd.json",
];

/// Point the engine's Vulkan loader at MoltenVK when the environment does not already say where
/// to look (an explicit `VK_ICD_FILENAMES` wins).
pub fn engine_env(command: &mut Command) {
    if std::env::var_os("VK_ICD_FILENAMES").is_none()
        && let Some(icd) = MOLTENVK_ICDS
            .iter()
            .find(|path| std::path::Path::new(path).exists())
    {
        command.env("VK_ICD_FILENAMES", icd);
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
