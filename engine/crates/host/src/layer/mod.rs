//! The [`HostLayer`] apex: the single [`Layer`] that owns the editor session and wires every
//! subsystem into the run loop. Single-threaded throughout. `update_session` is the
//! renderer-independent spine, so the unit tests drive it with no GPU; the renderer-coupled half is
//! skipped entirely when no renderer is attached.

mod overlays;
mod present;
mod session;

pub(crate) use present::render_preview_scene_to_png;

#[cfg(test)]
mod tests;

use saffron_animation::AnimationRuntime;
use saffron_app::{App, Layer};
use saffron_assets::{AssetServer, GpuSceneMirror};
use saffron_control::ControlContext;
use saffron_runtime::RuntimeSession;

use saffron_core::TimeSpan;
use saffron_protocol::{GetScriptSchemaParams, GetScriptSchemaResult, ScriptFieldDto};
use saffron_rendering::Uploader;
use saffron_scene::{Entity, Mesh};
use saffron_sceneedit::{PlayState, ProjectPhase, SceneEditContext};
use saffron_signal::SubscriptionId;
use saffron_window::Window;

use crate::viewport_shm::ViewportShmPublisher;

/// The host's apex layer: the editor session plus the wired subsystems.
pub struct HostLayer {
    editor: SceneEditContext,
    assets: AssetServer,
    control: ControlContext,
    spatial: saffron_spatial::ResidencyManager,
    /// Measures the viewport's travel so its residency claim leads the camera instead of trailing
    /// it; a viewpoint carries no rigidbody to read a velocity from.
    spatial_motion: saffron_spatial::SourceMotion,
    /// The same `RuntimeSession` the standalone `saffron-player` runs, so "advance the world a
    /// frame" is one code path. Idle in Edit.
    runtime: RuntimeSession,
    /// The play state the host last reconciled, for the edge detection in
    /// [`HostLayer::reconcile_play_edge`].
    last_play_state: PlayState,
    /// Built lazily from the renderer's device on the first rendered frame.
    uploader: Option<Uploader>,
    /// One mirror serves the scene, asset-preview, and thumbnail worlds.
    gpu_scene_mirror: GpuSceneMirror,
    /// Both segments are created at startup so the editor's presenter can block-open each pane.
    shm: ViewportShmPublisher,

    /// Frames publish to shared memory; the editor owns the render size.
    shm_publish: bool,
    /// Tracks asset-preview transitions so the anim runtime is pruned once per edge.
    preview_active: bool,

    /// Whether the editor spawned this host; gates the parent-death watch.
    editor_spawned: bool,
    /// The parent pid captured once at construction; the watch fires on a mismatch.
    editor_pid: Option<rustix::process::Pid>,

    script_subscription: SubscriptionId,
    physics_subscription: SubscriptionId,
    /// Impulses the play step's moving bodies emitted, staged for the renderer's next field step.
    pending_interaction_impulses: Vec<saffron_rendering::InteractionImpulse>,

    rejection_overlay: RejectionOverlayCache,
    wind_overlay: WindOverlayCache,
    heatmap_overlay: HeatmapOverlayCache,
}

/// Ground heights cached per snapped origin; rows (base + velocity) rebuilt every frame the flag
/// is on.
#[derive(Default)]
struct WindOverlayCache {
    origin: Option<(i64, i64)>,
    heights: Vec<f32>,
    rows: Vec<(glam::Vec3, glam::Vec3)>,
}

/// Marker rows in world metres, paired with a rejection-reason byte.
#[derive(Default)]
struct RejectionOverlayCache {
    fingerprint: u64,
    rows: Vec<(glam::Vec3, u8)>,
}

/// Surface-cast texels in world metres, paired with density 0..1.
#[derive(Default)]
struct HeatmapOverlayCache {
    fingerprint: u64,
    rows: Vec<(glam::Vec3, f32)>,
}

/// What the parent-death watch resolved for a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParentWatch {
    Alive,
    ParentDied,
}

/// One step of the host's teardown sequence, in the order it runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TeardownStep {
    ControlClosed,
    /// The VM never touches the scene, so it tears down before the world.
    ScriptsStopped,
    PhysicsWorldDropped,
    /// Only after the last world is gone.
    JoltGlobalsShutdown,
    PlayHooksUnsubscribed,
    /// Before the renderer drops.
    GpuCachesCleared,
}

impl HostLayer {
    /// Builds the host layer. `editor_spawned` arms the parent-death watch, capturing `getppid()`
    /// now; `shm_publish` records that frames publish to shared memory, so the host never tracks a
    /// hidden window's size.
    #[must_use]
    pub fn new(
        asset_root: impl Into<std::path::PathBuf>,
        editor_spawned: bool,
        shm_publish: bool,
    ) -> Self {
        let editor = SceneEditContext::new();
        let assets = AssetServer::new(asset_root);

        let mut control = ControlContext::new();
        Self::register_script_schema_command(&mut control);

        let mut layer = Self {
            editor,
            assets,
            control,
            spatial: saffron_spatial::ResidencyManager::new(),
            spatial_motion: saffron_spatial::SourceMotion::default(),
            runtime: RuntimeSession::new(),
            last_play_state: PlayState::Edit,
            uploader: None,
            gpu_scene_mirror: GpuSceneMirror::new(),
            shm: ViewportShmPublisher::new(),
            shm_publish,
            preview_active: false,
            editor_spawned,
            editor_pid: if editor_spawned {
                rustix::process::getppid()
            } else {
                None
            },
            script_subscription: SubscriptionId(0),
            physics_subscription: SubscriptionId(0),
            pending_interaction_impulses: Vec::new(),
            rejection_overlay: RejectionOverlayCache::default(),
            wind_overlay: WindOverlayCache::default(),
            heatmap_overlay: HeatmapOverlayCache::default(),
        };
        layer.install_play_state_hooks();
        layer
    }

    /// Registers `get-script-schema`. It lives on the host rather than in `saffron-control`
    /// because the handler needs the Lua schema reader and only the host may depend on
    /// `saffron-script`.
    fn register_script_schema_command(control: &mut ControlContext) {
        control.register::<GetScriptSchemaParams, GetScriptSchemaResult>(
            "get-script-schema",
            "get-script-schema {path} — a project script's declared fields (path relative to src/)",
            |ctx, params: GetScriptSchemaParams| {
                if params.path.is_empty() || params.path.contains("..") {
                    return Err(saffron_control::Error::Command(
                        "path must be relative to the project src/".to_owned(),
                    ));
                }
                let file = std::path::Path::new(&ctx.scene_edit.project_root)
                    .join("src")
                    .join(&params.path);
                let fields = saffron_script::read_script_schema(&file)
                    .map_err(|e| saffron_control::Error::Command(e.to_string()))?;
                Ok(GetScriptSchemaResult {
                    fields: fields
                        .into_iter()
                        .map(|field| ScriptFieldDto {
                            name: field.name,
                            r#type: field.field_type.wire_name().to_owned(),
                            default_value: field.default_value,
                        })
                        .collect(),
                })
            },
        );
    }

    /// Subscribes the script-VM and physics-world lifecycle markers, storing their tokens for
    /// `on_detach`.
    ///
    /// The subscriptions are markers only: `publish_transition` takes `&mut self` on the editor, so
    /// a subscribed closure runs while the editor is borrowed and cannot reach the play scene,
    /// project root, or registry it would need to build with. The host therefore detects the
    /// Edit↔Playing edge itself in [`HostLayer::reconcile_play_edge`], once `poll_control` has
    /// released the borrow.
    fn install_play_state_hooks(&mut self) {
        self.script_subscription = self
            .editor
            .on_play_state_changed
            .subscribe(|_next: PlayState| false);
        self.physics_subscription = self
            .editor
            .on_play_state_changed
            .subscribe(|_next: PlayState| false);
    }

    #[must_use]
    pub fn editor(&self) -> &SceneEditContext {
        &self.editor
    }

    #[must_use]
    pub fn editor_mut(&mut self) -> &mut SceneEditContext {
        &mut self.editor
    }

    #[must_use]
    pub fn animation(&self) -> &AnimationRuntime {
        self.runtime.animation()
    }

    pub fn attach_shm_publisher(&mut self, shm: ViewportShmPublisher) {
        self.shm = shm;
    }

    /// The per-tick contact → script dispatch high-water cursor.
    #[must_use]
    pub fn contact_cursor(&self) -> u64 {
        self.runtime.contact_cursor()
    }

    /// Whether a live play physics world is present (`false` in Edit).
    #[must_use]
    pub fn has_physics(&self) -> bool {
        self.runtime.has_physics()
    }

    /// Whether a script VM is live (Playing/Paused).
    #[must_use]
    pub fn script_vm_active(&self) -> bool {
        self.runtime.script_vm_active()
    }

    #[must_use]
    pub fn shm_publishing(&self) -> bool {
        self.shm.any_enabled()
    }

    /// Whether the play-state lifecycle subscriptions are still live (a detach removes them).
    #[must_use]
    pub fn play_hooks_live(&self) -> bool {
        !self.editor.on_play_state_changed.is_empty()
    }

    /// Evaluates the parent-death watch against an observed parent pid.
    ///
    /// The editor spawns the host as a child, so a crash or SIGKILL that skips the editor's own
    /// teardown reparents the host and changes its parent pid. Takes the observed pid so a test can
    /// drive it without a real process tree.
    #[must_use]
    pub fn watch_parent(&self, current_ppid: Option<rustix::process::Pid>) -> ParentWatch {
        if self.editor_spawned && current_ppid != self.editor_pid {
            ParentWatch::ParentDied
        } else {
            ParentWatch::Alive
        }
    }

    fn current_ppid(&self) -> Option<rustix::process::Pid> {
        if self.editor_spawned {
            rustix::process::getppid()
        } else {
            self.editor_pid
        }
    }

    /// Auto-selects the first mesh entity so the embedded viewport starts with something
    /// selected (the native-viewport host has no hierarchy panel to select from).
    pub fn auto_select_first_mesh(&mut self) {
        let mut first = Entity::NULL;
        self.editor.scene.for_each::<&Mesh, _>(|entity, _| {
            if first == Entity::NULL {
                first = entity;
            }
        });
        if first != Entity::NULL {
            self.editor.set_selection(first);
        }
    }

    /// Runs the teardown sequence, discarding the step record.
    fn teardown(&mut self) {
        let mut steps = Vec::new();
        self.teardown_recording(&mut steps);
    }

    /// The teardown sequence, emitting each step into `steps` in execution order. The cross-object
    /// ordering is a runtime UAF if wrong rather than a compile error, so a test asserts the order
    /// by reading `steps`.
    fn teardown_recording(&mut self, steps: &mut Vec<TeardownStep>) {
        self.control.shutdown();
        steps.push(TeardownStep::ControlClosed);

        self.runtime.stop_scripts();
        steps.push(TeardownStep::ScriptsStopped);

        // Drop the world before the Jolt globals: a live world holds Jolt bodies, so shutting
        // down the `Factory`/registered types first would be a use-after-free.
        self.runtime.drop_physics_world();
        steps.push(TeardownStep::PhysicsWorldDropped);

        self.runtime.shutdown_physics_globals();
        steps.push(TeardownStep::JoltGlobalsShutdown);

        self.editor
            .on_play_state_changed
            .unsubscribe(self.script_subscription);
        self.editor
            .on_play_state_changed
            .unsubscribe(self.physics_subscription);
        steps.push(TeardownStep::PlayHooksUnsubscribed);

        // Every `Arc<DeviceResources>`, `Arc<GpuMesh>` and `Arc<GpuTexture>` must release before
        // the renderer frees the device and allocator, or the last drop frees a GPU resource after
        // its allocator is gone. The mirror retains such clones for its prototypes and interned
        // textures, so it resets here too. The loop already idled the GPU.
        self.uploader = None;
        self.assets.clear_asset_caches();
        self.gpu_scene_mirror = GpuSceneMirror::new();
        steps.push(TeardownStep::GpuCachesCleared);
    }
}

impl Layer for HostLayer {
    fn name(&self) -> &str {
        "HostLayer"
    }

    fn on_attach(&mut self, app: &mut App) {
        self.bootstrap_project();
        self.auto_select_first_mesh();
        // The bootstrap scene loads here rather than via a control command, so it raises no
        // mutation signal. Seed one redraw so the initial scene paints before the reactive loop
        // idles a static viewport.
        app.redraw.request_redraw();
    }

    fn on_update(&mut self, app: &mut App, dt: TimeSpan) {
        let current_ppid = self.current_ppid();
        // The monotonic simulation clock wind and other evolution sample; the calendar never
        // rewinds it.
        self.editor.simulation_time_s += f64::from(dt.seconds);

        // Control runs first so a command this frame takes effect this frame. `frame_host` and
        // `window` are distinct `App` fields, so they borrow disjointly.
        let mut mutated = false;
        if let Some(renderer) = app.frame_host.renderer_mut() {
            self.update_spatial_source(dt);
            // Headless editor mode has no window, but the control plane still takes a `Window`
            // facade: the size is unused in publish mode and the signals are inert without an
            // event loop.
            let mut headless = Window::headless();
            let window = app.window.as_mut().unwrap_or(&mut headless);
            mutated = self.poll_control(window, renderer);
            self.drive_preview_render_queue(renderer);
            // One bounded step, so a load never stalls the drain + publish loop.
            if self.advance_project_load(renderer) {
                mutated = true;
            }
        }

        if self.update_session(dt, current_ppid) == ParentWatch::ParentDied {
            app.running = false;
            return;
        }

        // The gizmo-drag smoothing reads the viewport size, so it runs only with a renderer.
        if let Some(renderer) = app.frame_host.renderer_mut() {
            let (w, h) = (renderer.viewport_width(), renderer.viewport_height());
            let cam = self.editor.camera.view();
            self.editor.step_native_gizmo_drag(&cam, w, h, dt.seconds);
        }

        // The reactive render loop holds continuous render while some state evolves on its own,
        // requests one frame when a mutating command landed, and otherwise idles the GPU on a
        // static scene. The renderer is read up front and written back after, because the
        // controller drive between them needs `app.redraw`.
        let reasons = self.render_activity_reasons();
        let (temporal_active, suppress) =
            app.frame_host.renderer_mut().map_or((false, false), |r| {
                (
                    r.aa_mode() == "taa" || r.ssgi_enabled(),
                    r.power_state().suppresses_render(),
                )
            });
        app.redraw.set_continuous(!reasons.is_empty());
        app.redraw.set_reasons(reasons.clone());
        app.redraw.set_temporal_active(temporal_active);
        // A project load swaps the scene and asset caches across frames, so a mid-load render draws
        // against a torn scene and, with the caches cleared, can wedge the GPU. `on_update` still
        // runs every iteration, so the control socket stays responsive and the loader advances; the
        // flip to `Ready` requests the redraw that paints the loaded scene.
        let loading = self.editor.project_phase == ProjectPhase::Loading;
        app.redraw.set_suppressed(suppress || loading);
        if mutated {
            app.redraw.request_redraw();
        }
        // Mirror the verdict into the renderer so `render-stats` reports idle / converged / reasons.
        let idle = app.redraw.is_idle();
        let converged = app.redraw.converged();
        let reason_strings: Vec<String> = reasons.iter().map(|r| (*r).to_owned()).collect();
        if let Some(renderer) = app.frame_host.renderer_mut() {
            renderer.set_reactive_state(idle, converged, reason_strings);
        }
    }

    fn on_ui(&mut self, app: &mut App) {
        let mut vegetation_mutated = false;
        if let Some(renderer) = app.frame_host.renderer_mut() {
            let window = app.window.as_ref();
            vegetation_mutated = self.render_ui(window, renderer);
        }
        // Streamed vegetation changes the visible scene without a control mutation, so the reactive
        // loop must keep painting until the temporal effects converge.
        if vegetation_mutated {
            app.redraw.request_redraw();
        }
    }

    fn on_detach(&mut self, _app: &mut App) {
        self.teardown();
    }
}
