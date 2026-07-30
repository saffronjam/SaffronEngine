//! `saffron-player`: the standalone runtime that runs an exported Saffron app.
//!
//! It loads a project from the exported runtime data directory — beside its executable on Linux, or
//! under `Contents/Resources` in a macOS application bundle — and runs the scene as a live simulation
//! through the shared [`saffron_runtime::RuntimeSession`]. It links none of the editor stack, and
//! loads material shaders pre-baked as `.spv`, so it never invokes `slangc`.

#![deny(unsafe_code)]

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::rc::Rc;

use saffron_app::{App, AppConfig, Layer, attach_layer, run};
use saffron_assets::{
    AssetServer, GpuSceneMirror, ProjectHost, ProjectInfo, RenderSceneOptions, RendererScene,
    RendererUploader, advance_time_of_day, render_scene, scene_surface_field_snapshots,
};
use saffron_core::TimeSpan;
use saffron_protocol::AppManifest;
use saffron_rendering::{Renderer, Uploader};
use saffron_runtime::RuntimeSession;
use saffron_scene::{ComponentRegistry, Scene, ScriptInputState, register_builtin_components};
use saffron_spatial::{
    ResidencyFacet, ResidencyManager, ResidencyMask, SourceLevel, SpatialSource, SpatialSourceId,
    WorldPosition,
};
use saffron_window::keyboard::{KeyCode, PhysicalKey};
use saffron_window::{
    ElementState, MouseButton, MouseScrollDelta, Window, WindowConfig, WindowEvent,
};

fn main() -> ExitCode {
    saffron_log::init_logging();

    let project_dir = resolve_project_dir();
    let manifest = load_manifest(&project_dir);
    tracing::info!(
        "saffron-player: '{}' ({}x{}) from {}",
        manifest.title,
        manifest.width,
        manifest.height,
        project_dir.display()
    );
    // The window backend presents FIFO and has no fullscreen path, so an unsupported manifest
    // setting is surfaced rather than silently dropped.
    if manifest.fullscreen {
        tracing::warn!(
            "saffron-player: fullscreen requested but the window backend does not support it"
        );
    }
    if !manifest.vsync {
        tracing::warn!(
            "saffron-player: vsync disabled but the window backend uses FIFO presentation"
        );
    }

    let window = WindowConfig {
        title: manifest.title.clone(),
        width: manifest.width,
        height: manifest.height,
        hidden: false,
    };
    let config = AppConfig {
        window,
        on_create: Box::new(move |app: &mut App| {
            attach_layer(app, Box::new(PlayerLayer::new(project_dir, manifest)));
        }),
        on_exit: Box::new(|_app| {}),
    };
    let code = run(config);
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}

/// Resolves the project directory: an explicit CLI argument, then `SAFFRON_PROJECT`, then the
/// platform-native runtime data directory derived from the executable.
fn resolve_project_dir() -> PathBuf {
    resolve_project_dir_from(
        std::env::args().nth(1),
        std::env::var("SAFFRON_PROJECT")
            .ok()
            .filter(|v| !v.is_empty()),
        std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(Path::to_path_buf)),
    )
}

/// The precedence logic behind [`resolve_project_dir`], without the environment reads.
fn resolve_project_dir_from(
    arg: Option<String>,
    env: Option<String>,
    exe_dir: Option<PathBuf>,
) -> PathBuf {
    if let Some(arg) = arg {
        return PathBuf::from(arg);
    }
    if let Some(env) = env {
        return PathBuf::from(env);
    }
    exe_dir
        .map(|dir| platform_project_dir(&dir))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Maps the executable directory onto the staged project directory for this platform.
fn platform_project_dir(executable_dir: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    if executable_dir
        .file_name()
        .is_some_and(|name| name == "MacOS")
        && executable_dir
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|name| name == "Contents")
    {
        return executable_dir
            .parent()
            .expect("Contents parent checked")
            .join("Resources");
    }
    executable_dir.to_path_buf()
}

/// Reads `app.json` from the project directory, falling back field-by-field to defaults when it is
/// absent or unparseable.
fn load_manifest(dir: &Path) -> AppManifest {
    let path = dir.join("app.json");
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_else(|err| {
            tracing::warn!("saffron-player: app.json parse failed ({err}); using defaults");
            AppManifest::default()
        }),
        Err(_) => {
            tracing::info!(
                "saffron-player: no app.json at {}; using defaults",
                path.display()
            );
            AppManifest::default()
        }
    }
}

/// The [`ProjectHost`] adapter `AssetServer::load_project` needs. It wraps `&mut Renderer` directly,
/// because the player has no control plane to route through.
struct PlayerProjectHost<'a> {
    renderer: &'a mut Renderer,
}

impl ProjectHost for PlayerProjectHost<'_> {
    fn wait_gpu_idle(&mut self) {
        if let Err(err) = self.renderer.device().wait_idle() {
            tracing::error!("saffron-player: wait_gpu_idle: {err}");
        }
    }

    fn render_settings_to_json(&self) -> serde_json::Value {
        self.renderer.render_settings_to_json()
    }

    fn apply_render_settings(&mut self, settings: &serde_json::Value) {
        self.renderer.apply_render_settings(settings);
    }
}

/// The player's single [`Layer`].
struct PlayerLayer {
    project_dir: PathBuf,
    manifest: AppManifest,
    scene: Scene,
    assets: AssetServer,
    runtime: RuntimeSession,
    spatial: ResidencyManager,
    registry: ComponentRegistry,
    project: ProjectInfo,
    uploader: Option<Uploader>,
    gpu_scene_mirror: GpuSceneMirror,
    /// Shared with the window-signal closures that mutate it.
    input: Rc<RefCell<ScriptInputState>>,
    started: bool,
    warned_no_camera: bool,
}

impl PlayerLayer {
    fn new(project_dir: PathBuf, manifest: AppManifest) -> Self {
        let assets = AssetServer::new(project_dir.join("assets"));
        Self {
            project_dir,
            manifest,
            scene: Scene::new(),
            assets,
            runtime: RuntimeSession::new(),
            spatial: ResidencyManager::new(),
            registry: register_builtin_components(),
            project: ProjectInfo::default(),
            uploader: None,
            gpu_scene_mirror: GpuSceneMirror::new(),
            input: Rc::new(RefCell::new(ScriptInputState::default())),
            started: false,
            warned_no_camera: false,
        }
    }

    /// Writes the rendered frame to the path in `SAFFRON_CAPTURE_FRAME`, overwriting it each frame
    /// so the file holds the last one rendered.
    ///
    /// The player has no control plane to ask for a screenshot, so this is the seam that lets its
    /// output be compared against the host's. Paired with `SAFFRON_EXIT_AFTER_FRAMES` it makes a
    /// bounded run produce one deterministic image.
    fn capture_frame(&mut self, renderer: &mut Renderer) {
        let Some(path) = std::env::var_os("SAFFRON_CAPTURE_FRAME") else {
            return;
        };
        match renderer.encode_active_offscreen_png() {
            Ok(png) => {
                if let Err(err) = std::fs::write(&path, &png.bytes) {
                    tracing::error!("saffron-player: capture write failed: {err}");
                }
            }
            Err(err) => tracing::error!("saffron-player: capture encode failed: {err}"),
        }
    }

    /// Lazily builds the one-off uploader for asset GPU uploads.
    fn ensure_uploader(&mut self, renderer: &Renderer) {
        if self.uploader.is_some() {
            return;
        }
        let queue = renderer.device().graphics_queue.clone();
        match Uploader::new(renderer.device(), &queue) {
            Ok(uploader) => self.uploader = Some(uploader),
            Err(err) => tracing::error!("saffron-player: uploader create failed: {err}"),
        }
    }

    /// Routes the runtime's buffered script logs and errors to the console.
    fn drain_logs(&mut self) {
        for line in self.runtime.take_logs() {
            tracing::info!("[script] {}", line.message);
        }
        for err in self.runtime.take_errors() {
            tracing::error!("[script error] {}: {}", err.script, err.message);
        }
    }

    fn update_vegetation_source(&mut self) {
        const PLAYER_VIEW_SOURCE: SpatialSourceId = SpatialSourceId(1);
        let Some(camera) = self.scene.primary_camera() else {
            self.spatial.remove_source(PLAYER_VIEW_SOURCE);
            return;
        };
        let render_position = camera.view.inverse().w_axis.truncate();
        let Ok(position) =
            WorldPosition::from_render_relative(render_position, WorldPosition::origin())
        else {
            self.spatial.remove_source(PLAYER_VIEW_SOURCE);
            return;
        };
        let revision =
            position
                .global_ticks()
                .into_iter()
                .fold(0xcbf2_9ce4_8422_2325_u64, |hash, value| {
                    value.to_le_bytes().into_iter().fold(hash, |hash, byte| {
                        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
                    })
                });
        let facets = ResidencyMask::one(ResidencyFacet::Render)
            .with(ResidencyFacet::Physics)
            .with(ResidencyFacet::Simulation)
            .with(ResidencyFacet::Navigation);
        if let Err(error) = self.spatial.update_source(SpatialSource {
            id: PLAYER_VIEW_SOURCE,
            revision,
            position,
            velocity_mps: saffron_geometry::glam::DVec3::ZERO,
            prediction_seconds: 0.25,
            levels: vec![
                SourceLevel {
                    level: 0,
                    load_radius_cells: 4,
                    cleanup_radius_cells: 6,
                },
                SourceLevel {
                    level: 4,
                    load_radius_cells: 2,
                    cleanup_radius_cells: 3,
                },
            ],
            facets,
            priority: 100,
        }) {
            tracing::warn!("player vegetation spatial source rejected: {error}");
        }
    }

    /// Subscribes the window input signals into the shared [`ScriptInputState`]: held keys via the
    /// typed key signals, mouse state via the raw-event signal.
    fn wire_input(&self, window: &Window) {
        let input = Rc::clone(&self.input);
        window.on_key_pressed.subscribe(move |(key, _repeat)| {
            if let Some(name) = key_name(key) {
                input.borrow_mut().held.insert(name);
            }
            false
        });
        let input = Rc::clone(&self.input);
        window.on_key_released.subscribe(move |key| {
            if let Some(name) = key_name(key) {
                input.borrow_mut().held.remove(&name);
            }
            false
        });
        let input = Rc::clone(&self.input);
        window.on_raw_event.subscribe(move |event| {
            apply_mouse_event(&mut input.borrow_mut(), &event);
            false
        });
    }
}

impl Layer for PlayerLayer {
    fn name(&self) -> &str {
        "PlayerLayer"
    }

    fn on_attach(&mut self, app: &mut App) {
        let Some(renderer) = app.frame_host.renderer_mut() else {
            tracing::error!("saffron-player: no renderer; cannot start");
            return;
        };
        renderer.set_present_viewport_only(true);
        self.ensure_uploader(renderer);

        let selection = self.project_dir.join("project.json");
        let selection = selection.to_string_lossy().into_owned();
        {
            let mut host = PlayerProjectHost { renderer };
            match self.assets.load_project(
                &mut host,
                &self.registry,
                &mut self.scene,
                &mut self.project,
                &selection,
                "",
            ) {
                Ok(_sidecar) => tracing::info!(
                    "saffron-player: loaded project '{}'",
                    self.project.display_name
                ),
                Err(err) => {
                    tracing::error!("saffron-player: load project failed: {err}");
                    return;
                }
            }
        }

        let project_dir = self.project_dir.clone();
        self.runtime
            .start(&mut self.scene, &mut self.assets, &project_dir);
        self.drain_logs();
        self.started = true;

        if let Some(window) = app.window.as_ref() {
            self.wire_input(window);
        }
        tracing::info!("saffron-player: running '{}'", self.manifest.title);
    }

    fn on_update(&mut self, _app: &mut App, dt: TimeSpan) {
        if !self.started {
            return;
        }
        self.update_vegetation_source();
        if let Err(error) =
            self.runtime
                .synchronize_vegetation(&mut self.scene, &self.assets, &self.spatial)
        {
            tracing::error!("saffron-player: vegetation runtime advance failed: {error}");
        }
        {
            let mut input = self.input.borrow_mut();
            self.runtime
                .advance(&mut self.scene, &mut self.assets, dt.seconds, &mut input);
        }
        advance_time_of_day(&mut self.scene, dt.seconds);
        self.drain_logs();
    }

    fn on_ui(&mut self, app: &mut App) {
        if !self.started {
            return;
        }
        let Some(renderer) = app.frame_host.renderer_mut() else {
            return;
        };
        // Track the window size so the offscreen the present blits from stays native-resolution.
        if let Some(window) = app.window.as_ref() {
            let view = renderer.active_view_id();
            let _ = renderer.set_viewport_desired_size(view, window.width(), window.height());
        }
        self.ensure_uploader(renderer);
        let Some(uploader) = self.uploader.as_ref() else {
            return;
        };
        if self.runtime.vegetation_needs_regeneration() {
            let gpu = RendererUploader::new(
                uploader,
                renderer.descriptors(),
                renderer.skinning_enabled(),
            );
            match scene_surface_field_snapshots(&gpu, &mut self.scene, &mut self.assets) {
                Ok(providers) => {
                    if let Err(error) = self
                        .runtime
                        .regenerate_missing_vegetation(&mut self.assets, &providers)
                    {
                        tracing::error!(
                            "saffron-player: vegetation runtime regeneration failed: {error}"
                        );
                    }
                }
                Err(error) => tracing::error!(
                    "saffron-player: vegetation runtime surface capture failed: {error}"
                ),
            }
        }
        if renderer.viewport_width() == 0 || renderer.viewport_height() == 0 {
            return;
        }

        let world = renderer.active_view_id().gpu_scene_world();
        let vegetation_cell = self.runtime.vegetation_cell();
        let vegetation = vegetation_cell.borrow();
        if let Err(error) = self.gpu_scene_mirror.sync_renderer_world(
            world,
            &mut self.scene,
            vegetation.as_ref(),
            &mut self.assets,
            renderer,
            uploader,
        ) {
            tracing::error!("saffron-player: gpu scene mirror sync: {error}");
        }
        drop(vegetation);

        if let Some(cam) = self.scene.primary_camera() {
            let skinning = renderer.skinning_enabled();
            let mut driver = RendererScene::new(renderer, uploader, skinning);
            let options = RenderSceneOptions {
                show_editor_camera_models: false,
                show_grid: false,
            };
            render_scene(
                &mut driver,
                &mut self.scene,
                &mut self.assets,
                &mut self.gpu_scene_mirror,
                &cam,
                options,
            );
        } else if !self.warned_no_camera {
            tracing::warn!("saffron-player: scene has no primary camera; rendering sky only");
            self.warned_no_camera = true;
        }

        if let Err(err) = renderer.render_scene_offscreen() {
            tracing::error!("saffron-player: render_scene_offscreen: {err}");
        }
        self.capture_frame(renderer);
    }

    fn on_detach(&mut self, _app: &mut App) {
        // Teardown order mirrors the host's: stop scripts, drop the world, shut down the Jolt
        // globals, then release the GPU caches, so the last `Arc<GpuMesh>`/`Arc<GpuTexture>` drops
        // under a live-but-idle device. The loop already idled the GPU.
        self.runtime.stop_scripts();
        self.runtime.drop_physics_world();
        self.runtime.shutdown_physics_globals();
        self.uploader = None;
        self.assets.clear_asset_caches();
        // The mirror retains `Arc<GpuMesh>`/`Arc<GpuTexture>` clones for its prototypes and
        // interned textures. They must release with the rest, or the driver faults inside
        // `vkDestroyInstance`.
        self.gpu_scene_mirror = GpuSceneMirror::new();
    }
}

/// Maps a winit physical key to the lowercase name scripts read from `sa.input`, matching the control
/// plane's `to_ascii_lowercase` convention. Unmapped keys are ignored.
fn key_name(key: PhysicalKey) -> Option<String> {
    let PhysicalKey::Code(code) = key else {
        return None;
    };
    let name = match code {
        KeyCode::Space => "space",
        KeyCode::Enter => "enter",
        KeyCode::Escape => "escape",
        KeyCode::Tab => "tab",
        KeyCode::Backspace => "backspace",
        KeyCode::ArrowUp => "up",
        KeyCode::ArrowDown => "down",
        KeyCode::ArrowLeft => "left",
        KeyCode::ArrowRight => "right",
        KeyCode::ShiftLeft | KeyCode::ShiftRight => "shift",
        KeyCode::ControlLeft | KeyCode::ControlRight => "ctrl",
        KeyCode::AltLeft | KeyCode::AltRight => "alt",
        other => {
            let dbg = format!("{other:?}");
            if let Some(letter) = dbg.strip_prefix("Key") {
                return Some(letter.to_ascii_lowercase());
            }
            if let Some(digit) = dbg.strip_prefix("Digit") {
                return Some(digit.to_string());
            }
            return None;
        }
    };
    Some(name.to_string())
}

/// Folds a raw mouse [`WindowEvent`] into the gameplay input. Keyboard events arrive through the
/// typed signals, so they are ignored here.
fn apply_mouse_event(input: &mut ScriptInputState, event: &WindowEvent) {
    match event {
        WindowEvent::CursorMoved { position, .. } => {
            input.mouse_x = position.x as f32;
            input.mouse_y = position.y as f32;
        }
        WindowEvent::MouseInput { state, button, .. } => {
            if let Some(name) = mouse_button_name(*button) {
                match state {
                    ElementState::Pressed => {
                        input.mouse_buttons.insert(name);
                    }
                    ElementState::Released => {
                        input.mouse_buttons.remove(&name);
                    }
                }
            }
        }
        WindowEvent::MouseWheel { delta, .. } => {
            input.scroll += match delta {
                MouseScrollDelta::LineDelta(_, y) => *y,
                MouseScrollDelta::PixelDelta(p) => p.y as f32,
            };
        }
        _ => {}
    }
}

/// The lowercase name for a mouse button; others are ignored.
fn mouse_button_name(button: MouseButton) -> Option<String> {
    Some(
        match button {
            MouseButton::Left => "left",
            MouseButton::Right => "right",
            MouseButton::Middle => "middle",
            _ => return None,
        }
        .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use saffron_window::keyboard::NativeKeyCode;
    use winit::dpi::PhysicalPosition;
    use winit::event::{DeviceId, TouchPhase};

    fn code(code: KeyCode) -> PhysicalKey {
        PhysicalKey::Code(code)
    }

    #[test]
    fn key_name_maps_letters_lowercased() {
        // Pins the `Key<Letter>` Debug contract that the mapping parses.
        assert_eq!(key_name(code(KeyCode::KeyA)).as_deref(), Some("a"));
        assert_eq!(key_name(code(KeyCode::KeyW)).as_deref(), Some("w"));
        assert_eq!(key_name(code(KeyCode::KeyZ)).as_deref(), Some("z"));
    }

    #[test]
    fn key_name_maps_digits() {
        // Pins the `Digit<N>` Debug contract that the mapping parses.
        assert_eq!(key_name(code(KeyCode::Digit0)).as_deref(), Some("0"));
        assert_eq!(key_name(code(KeyCode::Digit5)).as_deref(), Some("5"));
        assert_eq!(key_name(code(KeyCode::Digit9)).as_deref(), Some("9"));
    }

    #[test]
    fn key_name_maps_named_and_modifier_keys() {
        assert_eq!(key_name(code(KeyCode::Space)).as_deref(), Some("space"));
        assert_eq!(key_name(code(KeyCode::Enter)).as_deref(), Some("enter"));
        assert_eq!(key_name(code(KeyCode::Escape)).as_deref(), Some("escape"));
        assert_eq!(key_name(code(KeyCode::Tab)).as_deref(), Some("tab"));
        assert_eq!(
            key_name(code(KeyCode::Backspace)).as_deref(),
            Some("backspace")
        );
        assert_eq!(key_name(code(KeyCode::ArrowUp)).as_deref(), Some("up"));
        assert_eq!(key_name(code(KeyCode::ArrowDown)).as_deref(), Some("down"));
        assert_eq!(key_name(code(KeyCode::ArrowLeft)).as_deref(), Some("left"));
        assert_eq!(
            key_name(code(KeyCode::ArrowRight)).as_deref(),
            Some("right")
        );

        assert_eq!(key_name(code(KeyCode::ShiftLeft)).as_deref(), Some("shift"));
        assert_eq!(
            key_name(code(KeyCode::ShiftRight)).as_deref(),
            Some("shift")
        );
        assert_eq!(
            key_name(code(KeyCode::ControlLeft)).as_deref(),
            Some("ctrl")
        );
        assert_eq!(
            key_name(code(KeyCode::ControlRight)).as_deref(),
            Some("ctrl")
        );
        assert_eq!(key_name(code(KeyCode::AltLeft)).as_deref(), Some("alt"));
        assert_eq!(key_name(code(KeyCode::AltRight)).as_deref(), Some("alt"));
    }

    #[test]
    fn key_name_ignores_unmapped_and_non_code_keys() {
        assert_eq!(key_name(code(KeyCode::F1)), None);

        assert_eq!(
            key_name(PhysicalKey::Unidentified(NativeKeyCode::Unidentified)),
            None
        );
    }

    #[test]
    fn mouse_button_name_maps_primary_buttons() {
        assert_eq!(
            mouse_button_name(MouseButton::Left).as_deref(),
            Some("left")
        );
        assert_eq!(
            mouse_button_name(MouseButton::Right).as_deref(),
            Some("right")
        );
        assert_eq!(
            mouse_button_name(MouseButton::Middle).as_deref(),
            Some("middle")
        );
        assert_eq!(mouse_button_name(MouseButton::Back), None);
        assert_eq!(mouse_button_name(MouseButton::Other(7)), None);
    }

    #[test]
    fn apply_mouse_event_tracks_cursor_position() {
        let mut input = ScriptInputState::default();
        apply_mouse_event(
            &mut input,
            &WindowEvent::CursorMoved {
                device_id: DeviceId::dummy(),
                position: PhysicalPosition::new(12.5, 34.0),
            },
        );
        assert_eq!(input.mouse_x, 12.5);
        assert_eq!(input.mouse_y, 34.0);
    }

    #[test]
    fn apply_mouse_event_folds_button_state() {
        let mut input = ScriptInputState::default();
        let press = WindowEvent::MouseInput {
            device_id: DeviceId::dummy(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        };
        apply_mouse_event(&mut input, &press);
        assert!(input.mouse_buttons.contains("left"));

        apply_mouse_event(
            &mut input,
            &WindowEvent::MouseInput {
                device_id: DeviceId::dummy(),
                state: ElementState::Released,
                button: MouseButton::Left,
            },
        );
        assert!(!input.mouse_buttons.contains("left"));

        apply_mouse_event(
            &mut input,
            &WindowEvent::MouseInput {
                device_id: DeviceId::dummy(),
                state: ElementState::Pressed,
                button: MouseButton::Back,
            },
        );
        assert!(input.mouse_buttons.is_empty());
    }

    #[test]
    fn apply_mouse_event_accumulates_scroll_across_deltas() {
        let mut input = ScriptInputState::default();
        apply_mouse_event(
            &mut input,
            &WindowEvent::MouseWheel {
                device_id: DeviceId::dummy(),
                delta: MouseScrollDelta::LineDelta(0.0, 1.5),
                phase: TouchPhase::Moved,
            },
        );
        apply_mouse_event(
            &mut input,
            &WindowEvent::MouseWheel {
                device_id: DeviceId::dummy(),
                delta: MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, -0.5)),
                phase: TouchPhase::Moved,
            },
        );

        assert_eq!(input.scroll, 1.0);
    }

    #[test]
    fn apply_mouse_event_ignores_non_mouse_events() {
        let mut input = ScriptInputState::default();
        apply_mouse_event(&mut input, &WindowEvent::CloseRequested);
        assert_eq!(input.mouse_x, 0.0);
        assert_eq!(input.mouse_y, 0.0);
        assert_eq!(input.scroll, 0.0);
        assert!(input.mouse_buttons.is_empty());
    }

    #[test]
    fn resolve_project_dir_prefers_cli_argument() {
        let dir = resolve_project_dir_from(
            Some("/from/arg".to_string()),
            Some("/from/env".to_string()),
            Some(PathBuf::from("/from/exe")),
        );
        assert_eq!(dir, PathBuf::from("/from/arg"));
    }

    #[test]
    fn resolve_project_dir_falls_back_to_env_then_exe_then_cwd() {
        assert_eq!(
            resolve_project_dir_from(
                None,
                Some("/from/env".to_string()),
                Some(PathBuf::from("/from/exe"))
            ),
            PathBuf::from("/from/env")
        );
        assert_eq!(
            resolve_project_dir_from(None, None, Some(PathBuf::from("/from/exe"))),
            PathBuf::from("/from/exe")
        );
        assert_eq!(
            resolve_project_dir_from(None, None, None),
            PathBuf::from(".")
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn platform_project_dir_resolves_macos_bundle_resources() {
        assert_eq!(
            platform_project_dir(Path::new("/Applications/Game.app/Contents/MacOS")),
            PathBuf::from("/Applications/Game.app/Contents/Resources")
        );
        assert_eq!(
            platform_project_dir(Path::new("/tmp/bin")),
            PathBuf::from("/tmp/bin")
        );
    }

    #[test]
    fn load_manifest_reads_partial_json_filling_defaults() {
        let dir =
            std::env::temp_dir().join(format!("saffron-player-manifest-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        std::fs::write(dir.join("app.json"), r#"{"title":"My Game","width":800}"#)
            .expect("write app.json");

        let manifest = load_manifest(&dir);
        assert_eq!(manifest.title, "My Game");
        assert_eq!(manifest.width, 800);

        assert_eq!(manifest.height, AppManifest::default().height);
        assert_eq!(manifest.vsync, AppManifest::default().vsync);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_manifest_defaults_when_absent_or_unparseable() {
        let dir = std::env::temp_dir().join(format!(
            "saffron-player-manifest-bad-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");

        assert_eq!(load_manifest(&dir), AppManifest::default());

        std::fs::write(dir.join("app.json"), "not json").expect("write app.json");
        assert_eq!(load_manifest(&dir), AppManifest::default());

        std::fs::remove_dir_all(&dir).ok();
    }
}
