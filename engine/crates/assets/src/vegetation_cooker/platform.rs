//! The portable platform profile and contract versions every cook key pins.

use saffron_vegetation::{CookPlatformProfile, CookVersionSet};

/// Builds the required portable content profile for this compile target.
pub fn portable_vegetation_platform_profile(content_profile: Option<&str>) -> CookPlatformProfile {
    CookPlatformProfile {
        target: target_triple(),
        content_profile: content_profile
            .filter(|profile| !profile.is_empty())
            .unwrap_or("portable-vulkan")
            .to_owned(),
        toolchain: include_str!("../../../../../rust-toolchain.toml")
            .trim()
            .to_owned(),
        features: vec![
            "compute".to_owned(),
            "indexed-multi-draw-indirect".to_owned(),
            "vulkan-1.4".to_owned(),
        ],
    }
}

/// Returns the exact semantic contract versions used by the production cooker.
#[must_use]
pub const fn vegetation_cook_versions() -> CookVersionSet {
    CookVersionSet::current()
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
fn target_triple() -> String {
    "aarch64-apple-darwin".to_owned()
}

#[cfg(all(target_arch = "x86_64", target_os = "macos"))]
fn target_triple() -> String {
    "x86_64-apple-darwin".to_owned()
}

#[cfg(all(target_arch = "x86_64", target_os = "linux", target_env = "gnu"))]
fn target_triple() -> String {
    "x86_64-unknown-linux-gnu".to_owned()
}

#[cfg(all(target_arch = "aarch64", target_os = "linux", target_env = "gnu"))]
fn target_triple() -> String {
    "aarch64-unknown-linux-gnu".to_owned()
}

#[cfg(all(target_arch = "x86_64", target_os = "linux", target_env = "musl"))]
fn target_triple() -> String {
    "x86_64-unknown-linux-musl".to_owned()
}

#[cfg(all(target_arch = "aarch64", target_os = "linux", target_env = "musl"))]
fn target_triple() -> String {
    "aarch64-unknown-linux-musl".to_owned()
}

#[cfg(all(target_arch = "x86_64", target_os = "windows", target_env = "msvc"))]
fn target_triple() -> String {
    "x86_64-pc-windows-msvc".to_owned()
}

#[cfg(all(target_arch = "aarch64", target_os = "windows", target_env = "msvc"))]
fn target_triple() -> String {
    "aarch64-pc-windows-msvc".to_owned()
}

#[cfg(not(any(
    all(target_arch = "aarch64", target_os = "macos"),
    all(target_arch = "x86_64", target_os = "macos"),
    all(target_arch = "x86_64", target_os = "linux", target_env = "gnu"),
    all(target_arch = "aarch64", target_os = "linux", target_env = "gnu"),
    all(target_arch = "x86_64", target_os = "linux", target_env = "musl"),
    all(target_arch = "aarch64", target_os = "linux", target_env = "musl"),
    all(target_arch = "x86_64", target_os = "windows", target_env = "msvc"),
    all(target_arch = "aarch64", target_os = "windows", target_env = "msvc")
)))]
fn target_triple() -> String {
    format!(
        "{}-unknown-{}",
        std::env::consts::ARCH,
        std::env::consts::OS
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portable_profile_is_complete_and_canonical() {
        let profile = portable_vegetation_platform_profile(None);
        assert!(!profile.target.is_empty());
        assert_eq!(profile.content_profile, "portable-vulkan");
        assert!(profile.toolchain.contains("1.96.0"));
        assert!(profile.identity().is_ok());
        assert_eq!(vegetation_cook_versions(), CookVersionSet::current());
    }
}
