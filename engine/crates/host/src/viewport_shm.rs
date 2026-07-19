//! Host-side wiring of the viewport shm publisher.
//!
//! The editor sets a per-view shm-segment environment variable
//! (`SAFFRON_VIEWPORT_SHM_SCENE` / `SAFFRON_VIEWPORT_SHM_ASSET`); each named view publishes
//! its rendered frames into its own POSIX-shm segment for the compositor-side presenter
//! (`editor/shell/src/presenter.rs`) instead of presenting to the hidden swapchain. Both segments are
//! created at startup so both panes have a ring the presenter can block-open; only the
//! active view bumps a new sequence each frame.
//!
//! The byte-exact producer is [`saffron_rendering::ShmPublish`] (the mmap + seqlock +
//! release fence). This module owns only the *selection*: which views to enable, under
//! which segment names, from the environment. The view tokens are the FROZEN wire strings
//! the reader's `View::from_wire` expects (`"scene"` / `"assetPreview"`), kept identical
//! end-to-end.

use saffron_rendering::ShmPublish;

/// The scene-view shm-segment environment variable the editor sets.
pub const ENV_SHM_SCENE: &str = "SAFFRON_VIEWPORT_SHM_SCENE";

/// The asset-preview-view shm-segment environment variable the editor sets.
pub const ENV_SHM_ASSET: &str = "SAFFRON_VIEWPORT_SHM_ASSET";

/// The two views the editor presents. The wire token is FROZEN end-to-end with the
/// reader's `View::from_wire` (`editor/shell/src/presenter.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShmView {
    /// The main scene viewport.
    Scene,
    /// The asset-preview viewport.
    AssetPreview,
}

impl ShmView {
    /// The control-plane / shm wire token. Exactly `"scene"` / `"assetPreview"`.
    pub fn wire(self) -> &'static str {
        match self {
            ShmView::Scene => "scene",
            ShmView::AssetPreview => "assetPreview",
        }
    }

    /// The dense slot index (`Scene` = 0).
    fn index(self) -> usize {
        match self {
            ShmView::Scene => 0,
            ShmView::AssetPreview => 1,
        }
    }

    /// The environment variable that names this view's segment.
    fn env_var(self) -> &'static str {
        match self {
            ShmView::Scene => ENV_SHM_SCENE,
            ShmView::AssetPreview => ENV_SHM_ASSET,
        }
    }
}

/// One view's resolved publish configuration: which view, under which segment name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShmViewConfig {
    /// The view this segment publishes.
    pub view: ShmView,
    /// The shm segment name (`shm_open` path, e.g. `/saffron-scene`).
    pub name: String,
}

/// Resolves the per-view shm publish configuration from the environment: a view is enabled
/// only when its variable is present and non-empty. A standalone / CLI / headless run may
/// set neither (or only the scene view); the returned vector preserves scene-then-asset
/// order.
pub fn configs_from_env() -> Vec<ShmViewConfig> {
    [ShmView::Scene, ShmView::AssetPreview]
        .into_iter()
        .filter_map(|view| {
            let name = std::env::var(view.env_var()).ok()?;
            (!name.is_empty()).then_some(ShmViewConfig { view, name })
        })
        .collect()
}

/// The host's owned set of viewport shm segments, one per enabled view.
///
/// Holds a [`ShmPublish`] per enabled view (created at startup so both panes have a ring
/// the presenter can block-open). The host drives `publish` for the active view each
/// frame from the renderer's BGRA8 readback. `Drop` on each [`ShmPublish`] unmaps and
/// `shm_unlink`s its segment.
#[derive(Default)]
pub struct ViewportShmPublisher {
    /// One slot per view index (`Scene` = 0, `AssetPreview` = 1); `None` when that view
    /// is not enabled this run.
    views: [Option<ShmPublish>; 2],
}

impl ViewportShmPublisher {
    /// Builds an empty publisher with no views enabled.
    pub fn new() -> Self {
        Self::default()
    }

    /// Builds the publisher from the environment and creates each enabled view's segment
    /// *now* (the startup-create-both rule: the presenter blocks until each named
    /// segment exists). `seq` stays `0` per segment until the first real publish, so the
    /// reader shows nothing for a not-yet-rendered view.
    ///
    /// # Errors
    ///
    /// Returns the underlying [`std::io::Error`] of the first segment that fails to
    /// create (a missing shm filesystem, a bad name); already-created segments are kept.
    pub fn from_env() -> std::io::Result<Self> {
        let mut publisher = Self::new();
        for config in configs_from_env() {
            publisher.enable(config)?;
        }
        Ok(publisher)
    }

    /// Enables one view's segment, creating it immediately. Replaces any prior segment
    /// for the same view (the new name's segment supersedes the old).
    ///
    /// # Errors
    ///
    /// Returns the underlying [`std::io::Error`] if the segment cannot be created.
    pub fn enable(&mut self, config: ShmViewConfig) -> std::io::Result<()> {
        let mut publish = ShmPublish::default();
        publish.enable(&config.name)?;
        self.views[config.view.index()] = Some(publish);
        Ok(())
    }

    /// Whether the given view has an enabled segment.
    pub fn is_enabled(&self, view: ShmView) -> bool {
        self.views[view.index()]
            .as_ref()
            .is_some_and(ShmPublish::enabled)
    }

    /// Whether any view publishes.
    pub fn any_enabled(&self) -> bool {
        self.views.iter().flatten().any(ShmPublish::enabled)
    }

    /// The publisher for a view, mutable, or `None` when that view is not enabled.
    pub fn view_mut(&mut self, view: ShmView) -> Option<&mut ShmPublish> {
        self.views[view.index()].as_mut()
    }

    /// Publishes one BGRA8 frame into a view's segment (a no-op when the view is not
    /// enabled). `pixels` must be tightly packed `width * height * 4` BGRA8 bytes — the
    /// renderer's offscreen→BGRA8 readback.
    pub fn publish(&mut self, view: ShmView, width: u32, height: u32, pixels: &[u8]) {
        if let Some(publish) = self.view_mut(view) {
            publish.publish(width, height, pixels);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use saffron_test_support::unique_shm_name;
    use std::sync::Mutex;

    /// Serializes the env-reading tests: `set_var`/`remove_var` are process-global, so two
    /// tests mutating the same variables must not interleave.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn wire_tokens_are_frozen() {
        assert_eq!(ShmView::Scene.wire(), "scene");
        assert_eq!(ShmView::AssetPreview.wire(), "assetPreview");
    }

    #[test]
    fn env_selects_only_present_non_empty_views_in_scene_then_asset_order() {
        let _guard = ENV_LOCK.lock().unwrap();
        let scene = unique_shm_name();
        let asset = unique_shm_name();
        // SAFETY: serialized by ENV_LOCK; no other thread reads these vars concurrently.
        unsafe {
            std::env::set_var(ENV_SHM_SCENE, &scene);
            std::env::set_var(ENV_SHM_ASSET, &asset);
        }
        let configs = configs_from_env();
        assert_eq!(
            configs,
            vec![
                ShmViewConfig {
                    view: ShmView::Scene,
                    name: scene
                },
                ShmViewConfig {
                    view: ShmView::AssetPreview,
                    name: asset
                },
            ]
        );

        // An empty value disables that view.
        // SAFETY: serialized by ENV_LOCK.
        unsafe {
            std::env::set_var(ENV_SHM_ASSET, "");
        }
        let configs = configs_from_env();
        assert_eq!(configs.len(), 1);
        assert_eq!(configs[0].view, ShmView::Scene);

        // SAFETY: serialized by ENV_LOCK.
        unsafe {
            std::env::remove_var(ENV_SHM_SCENE);
            std::env::remove_var(ENV_SHM_ASSET);
        }
        assert!(configs_from_env().is_empty(), "neither set → no views");
    }

    #[test]
    fn enable_creates_a_per_view_segment_and_publish_routes_to_it() {
        let mut publisher = ViewportShmPublisher::new();
        assert!(!publisher.any_enabled());

        publisher
            .enable(ShmViewConfig {
                view: ShmView::Scene,
                name: unique_shm_name(),
            })
            .expect("enable scene");
        assert!(publisher.is_enabled(ShmView::Scene));
        assert!(!publisher.is_enabled(ShmView::AssetPreview));
        assert!(publisher.any_enabled());

        // Publishing the enabled view bumps its sequence; the other view is a no-op.
        let (w, h) = (4u32, 2u32);
        let pixels = vec![0x33u8; (w * h * 4) as usize];
        publisher.publish(ShmView::AssetPreview, w, h, &pixels); // no-op: not enabled
        publisher.publish(ShmView::Scene, w, h, &pixels);

        let scene = publisher.view_mut(ShmView::Scene).expect("scene enabled");
        assert_eq!(scene.seq(), 1, "first publish bumps the scene seq to 1");
    }

    #[test]
    fn dropping_the_publisher_unlinks_every_segment() {
        use std::ffi::CString;
        let name = unique_shm_name();
        {
            let mut publisher = ViewportShmPublisher::new();
            publisher
                .enable(ShmViewConfig {
                    view: ShmView::Scene,
                    name: name.clone(),
                })
                .expect("enable");
        }
        // The segment is gone after drop: a read-only open finds nothing.
        let cname = CString::new(name).unwrap();
        let opened = rustix::shm::open(
            cname.as_c_str(),
            rustix::shm::OFlags::RDONLY,
            rustix::shm::Mode::empty(),
        );
        assert!(opened.is_err(), "drop shm_unlinks the segment");
    }
}
