//! Assemble the macOS dev `.app` bundle: builds the shell + helper binaries and lays out
//! `Saffron Anima.app` with `Contents/Frameworks/Chromium Embedded Framework.framework` plus the
//! five helper apps — the layout CEF requires on macOS (there is no single-executable model).
//! The framework is copied from the crate-pinned CEF distribution (`CEF_PATH` /
//! `cef-dll-sys`-provisioned).
//!
//! ```sh
//! cargo run --bin bundle            # → target/bundle/Saffron Anima.app
//! ```

#[cfg(target_os = "macos")]
fn main() {
    use cef::build_util::mac::{BundleInfo, build_bundle};

    let out = std::env::args().nth(1).unwrap_or_else(|| {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/bundle")
            .to_string_lossy()
            .into_owned()
    });
    let info = BundleInfo::new(
        "Saffron Anima",
        "dev.saffron.anima.editor",
        "Saffron Anima",
        "en",
        "0.1.0".parse().expect("bundle version"),
    );
    match build_bundle(std::path::Path::new(&out), "saffron-editor-shell", info) {
        Ok(app) => println!("bundle: {}", app.display()),
        Err(err) => {
            eprintln!("bundle failed: {err}");
            std::process::exit(1);
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("the bundle assembler targets the macOS .app layout; nothing to do on this platform");
    std::process::exit(1);
}
