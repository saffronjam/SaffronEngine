//! The custom scheme identities. CEF requires custom schemes to be registered identically in
//! every process, and on macOS the subprocesses run from the helper binary — so the identities
//! live here, included by both the shell (`main.rs`) and the helper (`helper_main.rs`); the
//! handlers stay browser-process-only (`scheme.rs`, `appscheme.rs`).

/// `saffron-img://` — the storefront thumbnail/resource scheme (handler in `scheme.rs`).
pub const IMG_SCHEME_NAME: &str = "saffron-img";

/// `saffron-app://` — the packaged-frontend scheme (handler in `appscheme.rs`).
pub const APP_SCHEME_NAME: &str = "saffron-app";

/// `CEF_SCHEME_OPTION_STANDARD | SECURE | CORS_ENABLED | FETCH_ENABLED` — standard secure
/// schemes so Chromium resolves relative URLs, treats them as secure contexts, and allows
/// CORS + `fetch()`.
pub const SCHEME_OPTIONS: i32 = 1 | 8 | 16 | 64;

/// Register both schemes — called from `on_register_custom_schemes` in every process type.
pub fn register(registrar: &mut cef::SchemeRegistrar) {
    use cef::ImplSchemeRegistrar;
    registrar.add_custom_scheme(Some(&IMG_SCHEME_NAME.into()), SCHEME_OPTIONS);
    registrar.add_custom_scheme(Some(&APP_SCHEME_NAME.into()), SCHEME_OPTIONS);
}
