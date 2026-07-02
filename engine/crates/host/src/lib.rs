//! The `saffron-host` headless viewport host: the integration apex that wires every
//! subsystem, renders offscreen, publishes frames into shared memory, and serves the
//! control plane.
//!
//! Depends on `saffron-core`, `saffron-app`, `saffron-window`, `saffron-rendering`,
//! `saffron-sceneedit`, `saffron-control`, `saffron-scene`, `saffron-animation`,
//! `saffron-physics`, `saffron-script`, `saffron-assets`.
//!
//! # The `unsafe` seam
//!
//! `#![allow(unsafe_code)]` is set crate-wide because the host owns one of the engine's FFI
//! seams. The frame transport to the editor is the POSIX shared-memory seqlock publisher: a
//! `MAP_SHARED` mapping whose 32-byte header + BGRA8 ring the producer writes through a raw
//! pointer, bumping the sequence last under a [`std::sync::atomic::fence`] `Release`. The
//! byte-exact producer ([`saffron_rendering::ShmPublish`]) is a renderer type — the
//! renderer's frame loop publishes the offscreen→BGRA8 readback through it. This crate owns
//! the *wiring*: which views are enabled and under which segment names, decided from the
//! editor-set environment ([`viewport_shm`]). The `unsafe` itself (mmap + the pointer-level
//! header/ring writes + `shm_unlink`) is confined to that producer; the host carries the
//! crate-root `allow` because it owns the seam end-to-end and the parent-death watch /
//! control-socket wiring also reaches for raw syscalls.

#![allow(unsafe_code)]

mod control_renderer;
mod layer;
mod overlay;
pub mod viewport_shm;

pub use control_renderer::HostControlRenderer;
pub use layer::{HostLayer, ParentWatch, TeardownStep};
pub use overlay::build_scene_edit_overlay;
pub use viewport_shm::{ShmView, ShmViewConfig, ViewportShmPublisher};

use saffron_app::{App, AppConfig, attach_layer, run};
use saffron_assets::engine_asset_path;
use saffron_window::WindowConfig;

/// Builds the editor host (window or headless device + renderer + the editor session), runs
/// the main loop, and returns the process exit code.
///
/// Reads the editor-spawn / shm environment the editor sets, attaches the [`HostLayer`] in
/// the app's `on_create` (wiring the renderer's present-only mode + default AA + the shm
/// segments), then drives [`saffron_app::run`] to completion. The loop's `wait_gpu_idle`
/// runs before [`HostLayer::on_detach`] tears the session down, before the renderer drops.
#[must_use]
pub fn run_host(title: impl Into<String>, width: u32, height: u32) -> i32 {
    saffron_log::init_logging();

    let editor_spawned = std::env::var_os("SAFFRON_EDITOR_NATIVE_VIEWPORT").is_some();

    // The viewport shm segments are created at startup (both views, if named) so the
    // editor's presenter can block-open each pane before its first frame.
    let shm = match ViewportShmPublisher::from_env() {
        Ok(shm) => shm,
        Err(err) => {
            tracing::error!("viewport shm publish: {err}");
            ViewportShmPublisher::new()
        }
    };
    let shm_publish = shm.any_enabled();
    if shm_publish {
        tracing::info!("viewport shm publish enabled");
    }

    let asset_root = engine_asset_path("assets");
    let config = AppConfig {
        window: WindowConfig {
            title: title.into(),
            width,
            height,
            // Headless editor mode creates no window, so `hidden` is moot; the standalone
            // windowed path shows its window.
            hidden: false,
        },
        on_create: Box::new(move |app: &mut App| {
            // The native-viewport host always renders present-only (no engine panels), driven
            // over the control plane. Default AA is 1× MSAA + TAA: TAA is the antialiaser, so
            // stacking 4× MSAA on top only multiplies attachment bandwidth + edge-fragment shading.
            if let Some(renderer) = app.frame_host.renderer_mut() {
                renderer.set_present_viewport_only(true);
                if let Err(err) = renderer.set_aa(1, false, false) {
                    tracing::warn!("default AA setup failed: {err}");
                }
            }

            let mut host = HostLayer::new(asset_root, editor_spawned, shm_publish);
            host.attach_shm_publisher(shm);
            // The project bring-up (from the editor-set environment) and the first-mesh
            // auto-select happen in `HostLayer::on_attach`, after the renderer exists and the
            // scene is loaded — not here, where no project is loaded yet.
            attach_layer(app, Box::new(host));
        }),
        // Teardown lives in `HostLayer::on_detach` (run after `wait_gpu_idle`, before the
        // renderer drops); `on_exit` has nothing left to do.
        on_exit: Box::new(|_app| {}),
    };

    run(config)
}
