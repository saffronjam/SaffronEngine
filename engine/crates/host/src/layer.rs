//! The [`HostLayer`] apex: the single [`Layer`] that owns the editor session and wires
//! every subsystem into the run loop.
//!
//! The host state lives as plain fields on this layer: the run loop owns the layer, the
//! layer owns its state, and the lifecycle is methods on it — single-threaded throughout.
//!
//! The per-frame work splits cleanly in two:
//!
//! - The **session update** ([`HostLayer::update_session`]) is the renderer-independent
//!   spine: the parent-death watch, the asset-preview prune, `tick_animation` →
//!   `tick_play`, the deferred script-error pause, and the fly-camera drain. It runs on
//!   pure CPU state, so the unit tests drive it with no GPU.
//! - The **renderer-coupled** work (the control poll, the gizmo-drag smoothing that reads
//!   the viewport size, and the whole `on_ui` scene render + overlay submit) reaches the
//!   concrete [`Renderer`] through [`saffron_app::FrameHost::renderer_mut`]; with no
//!   renderer attached (the test host) those steps are skipped.

use glam::Vec2;

use saffron_animation::{AnimMode, AnimationRuntime};
use saffron_app::{App, Layer};
use saffron_assets::{
    AssetServer, PREVIEW_THUMBNAIL_MATERIAL_ID, PreviewRenderKind, RenderSceneOptions,
    RendererScene, RendererUploader, advance_time_of_day, render_scene, write_thumbnail_cache,
};
use saffron_control::{ControlContext, PreviewSubject, build_preview_scene_for_thumbnail};
use saffron_runtime::RuntimeSession;

use crate::control_renderer::HostControlRenderer;
use saffron_core::TimeSpan;
use saffron_protocol::{GetScriptSchemaParams, GetScriptSchemaResult, ScriptFieldDto};
use saffron_rendering::{Renderer, Uploader};
use saffron_scene::{AnimationPlayer, CameraView, Entity, Mesh, Scene};
use saffron_sceneedit::{PlayState, ProjectPhase, SceneEditContext, update_scene_edit_camera};
use saffron_signal::SubscriptionId;
use saffron_spatial::{
    ResidencyFacet, ResidencyMask, SourceLevel, SpatialSource, SpatialSourceId, WorldPosition,
};
use saffron_window::Window;

use crate::overlay::build_scene_edit_overlay;
use crate::viewport_shm::{ShmView, ViewportShmPublisher};

/// The host's apex layer: the editor session plus the wired subsystems.
///
/// Owns its state by value. The load-bearing teardown (worker join, control-socket close,
/// physics drop, GPU-cache clear) is sequenced explicitly in [`HostLayer::on_detach`], which
/// the run loop calls after `wait_gpu_idle` and before the renderer drops.
pub struct HostLayer {
    /// The editor's mutable session state (scene / selection / play / gizmo / smoothing).
    editor: SceneEditContext,
    /// The live asset catalog + GPU caches + thumbnail worker.
    assets: AssetServer,
    /// The control plane: the command registry + the once-per-frame socket drain.
    control: ControlContext,
    /// Shared hierarchical cell residency and source state.
    spatial: saffron_spatial::ResidencyManager,
    /// The shared play-mode simulation spine: the Jolt world + script VM + animation runtime.
    /// Idle in Edit; `start`/`stop` on the Edit↔Play edge, `tick_animation` (both modes) and
    /// the gated `step` (play only) each frame. The same `RuntimeSession` the standalone
    /// `saffron-player` runs, so "advance the world a frame" is one code path.
    runtime: RuntimeSession,
    /// The play state the host last reconciled, for the edge detection that builds/tears the
    /// world + VM (run host-side after `poll_control` releases the editor borrow rather than
    /// inside the published transition).
    last_play_state: PlayState,
    /// The host-built one-off uploader the scene-render path resolves assets through,
    /// constructed lazily from the renderer's device on the first rendered frame.
    uploader: Option<Uploader>,
    /// The per-view viewport shm segments (empty until `run_host` attaches them from the
    /// editor-set environment). The segments exist at startup so the editor's presenter can
    /// block-open both panes.
    shm: ViewportShmPublisher,

    /// Frames publish to shared memory; the editor owns the render size.
    shm_publish: bool,
    /// Tracks asset-preview transitions so the anim runtime is pruned once per edge.
    preview_active: bool,

    /// Whether the editor spawned this host (`SAFFRON_EDITOR_NATIVE_VIEWPORT` set); gates
    /// the parent-death watch.
    editor_spawned: bool,
    /// The parent pid captured once at construction; the watch fires on a mismatch.
    editor_pid: Option<rustix::process::Pid>,

    /// The script-VM lifecycle subscription, unsubscribed on detach.
    script_subscription: SubscriptionId,
    /// The physics-world lifecycle subscription, unsubscribed on detach.
    physics_subscription: SubscriptionId,
}

/// What the parent-death watch resolved for a frame, split out so the watch is testable
/// without a live process tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParentWatch {
    /// The editor did not spawn us, or the parent is unchanged — keep running.
    Alive,
    /// The editor spawned us and `getppid` changed — the editor vanished, request exit.
    ParentDied,
}

/// One step of the host's teardown sequence, in the order it runs. The cross-object ordering is a
/// runtime UAF if wrong (not a compile error), so [`HostLayer::teardown`] emits each step into a
/// record a test reads to assert the order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TeardownStep {
    /// Close the control socket.
    ControlClosed,
    /// Stop the script VM (it never touches the scene, so it tears down before the world).
    ScriptsStopped,
    /// Drop the live play physics world (RAII frees its Jolt bodies/ragdolls).
    PhysicsWorldDropped,
    /// Shut down the Jolt process globals — only after the last world is gone.
    JoltGlobalsShutdown,
    /// Unsubscribe the two play-state lifecycle hooks.
    PlayHooksUnsubscribed,
    /// Drop the host's one-off uploader + clear the GPU `Ref` caches, before the renderer drops.
    GpuCachesCleared,
}

impl HostLayer {
    /// Builds the host layer: a seeded editor context, an asset server rooted at `asset_root`,
    /// the idle [`RuntimeSession`] (its world/VM build lazily on the play edge), the control
    /// context (which binds the socket), the host-owned `get-script-schema` command, and the two
    /// play-state lifecycle subscriptions held as live tokens.
    ///
    /// `editor_spawned` records whether the editor launched this host (the parent-death watch
    /// is armed only then, capturing `getppid()` now); `shm_publish` records that frames
    /// publish to shared memory (so the host never tracks a hidden window's size).
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
            runtime: RuntimeSession::new(),
            last_play_state: PlayState::Edit,
            uploader: None,
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
        };
        layer.install_play_state_hooks();
        layer
    }

    /// Registers the one host-owned control command (`get-script-schema`).
    ///
    /// Lives on the host, not in `saffron-control`, because the handler needs the Lua schema
    /// reader and only the host may depend on `saffron-script`. It rejects a bad path up front
    /// (the frozen guard), resolves
    /// `<project>/src/<path>`, reads the script's declared `properties` in a throwaway sandboxed
    /// VM, and maps each [`ScriptField`](saffron_script::ScriptField) to a `ScriptFieldDto`.
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

    /// Subscribes the script-VM and physics-world lifecycle markers on the editor's
    /// `on_play_state_changed` signal, storing their tokens for `on_detach` to unsubscribe.
    ///
    /// The VM / Jolt world cannot be built inside the published transition: `publish_transition`
    /// is `&mut self` on the editor, so a subscribed closure runs while the editor is borrowed
    /// and cannot reach the play scene / project root / registry it needs to build with. So the
    /// host detects the Edit↔Playing edge itself in [`HostLayer::reconcile_play_edge`], right
    /// after `poll_control` releases the editor borrow — the only borrow-sound shape, and the
    /// world exists before the next poll / tick. The subscriptions stay as the lifecycle seam
    /// markers (the "play hooks are live" invariant the editor exposes), torn down cleanly on
    /// detach.
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

    /// The editor session state (for the lifecycle wiring + tests).
    #[must_use]
    pub fn editor(&self) -> &SceneEditContext {
        &self.editor
    }

    /// The editor session state, mutably (for `run_host`'s project bring-up).
    #[must_use]
    pub fn editor_mut(&mut self) -> &mut SceneEditContext {
        &mut self.editor
    }

    /// The runtime's animation player (for tests asserting the preview-prune behavior).
    #[must_use]
    pub fn animation(&self) -> &AnimationRuntime {
        self.runtime.animation()
    }

    /// Attaches the viewport shm publisher `run_host` builds from the editor-set
    /// environment (both segments created at startup so the presenter can block-open each
    /// pane).
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

    /// Whether any viewport view publishes to shared memory.
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
    /// The editor spawns the host as a child and reparents it away (the parent pid changes)
    /// when it dies however it dies — a crash or SIGKILL that skips the editor's teardown.
    /// With the watch armed (`editor_spawned`), a mismatch against the captured pid means the
    /// editor vanished. Pure (takes the observed pid) so a test drives it without a real
    /// process tree.
    #[must_use]
    pub fn watch_parent(&self, current_ppid: Option<rustix::process::Pid>) -> ParentWatch {
        if self.editor_spawned && current_ppid != self.editor_pid {
            ParentWatch::ParentDied
        } else {
            ParentWatch::Alive
        }
    }

    /// Reads the live parent pid (the watch's per-frame observation).
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

    /// The renderer-independent per-frame spine: the parent-death verdict, the asset-preview
    /// prune on its transition edge, the runtime's `tick_animation` then the gated `step`
    /// (control runs before this, so a play/step command lands the same frame), the deferred
    /// script-error pause, the fly-camera look-delta drain, and the edit smoothing. Returns the
    /// parent-death verdict.
    pub fn update_session(
        &mut self,
        dt: TimeSpan,
        current_ppid: Option<rustix::process::Pid>,
    ) -> ParentWatch {
        let watch = self.watch_parent(current_ppid);
        if watch == ParentWatch::ParentDied {
            return watch;
        }

        // A play/stop command landed in this frame's `poll_control` (or a test drove
        // `enter_play`/`stop_play`): build or tear the world + VM on the Edit↔Playing edge
        // before this frame ticks, so the world steps the same frame play was entered.
        self.reconcile_play_edge();

        // Entering or leaving the asset preview swaps `active_scene` to a fresh entity set;
        // drop the runtime's per-entity transition/pose entries on the transition edge so a
        // re-entered preview starts clean and dead entries never accumulate across opens.
        let previewing = self.editor.previewing();
        if previewing != self.preview_active {
            self.runtime.prune_animation();
            self.preview_active = previewing;
        }

        // Animation runs every frame in both Edit (preview) and Play, before the simulation step
        // so a script can still override a bone the same frame physics settles it. `editor`,
        // `runtime`, and `assets` are distinct fields, so the borrows are disjoint.
        let anim_mode = if self.editor.play_state == PlayState::Edit {
            AnimMode::Edit
        } else {
            AnimMode::Play
        };
        {
            let scene = self.editor.active_scene();
            self.runtime
                .tick_animation(scene, &mut self.assets, dt.seconds, anim_mode);
        }

        // The gated simulation step. Control already drained this frame (in `on_update`), so a
        // play/pause/step command lands now: `play_step_dt` consumes a grant and yields the dt to
        // simulate, or `None` in Edit / inert Paused. The runtime steps physics + scripts over
        // the play scene (taking the editor's gameplay input by `&mut`, deriving edges in place).
        // A contained script failure surfaces through the drained errors and defers a pause —
        // never inside the step, which would re-enter the play machine.
        if let Some(step_dt) = self.editor.play_step_dt(dt.seconds) {
            {
                let (scene, input) = self.editor.play_scene_and_input();
                self.runtime.step(scene, step_dt, input);
            }
            if self.drain_runtime_sinks() {
                let _ = self.editor.pause_play();
            }
        }

        // Fly-cam: the editor streams pointer-lock look deltas over the control plane; drain
        // the accumulated delta each frame so a burst between frames is not lost.
        let input = self.editor.fly_input;
        self.editor.fly_input.look_delta = Vec2::ZERO;
        update_scene_edit_camera(&mut self.editor.camera, &input, dt.seconds);

        // Smoothed edits (`set-transform smooth:1`) converge here too.
        self.editor.step_edit_smoothing(dt.seconds);

        advance_time_of_day(self.editor.active_scene(), dt.seconds);

        ParentWatch::Alive
    }

    /// The named reasons the viewport must keep rendering continuously this frame — state that
    /// evolves on its own without a fresh command. Empty means a static scene the reactive loop may
    /// idle (after the keep-warm window). Interaction (camera/gizmo drag) streams control commands,
    /// so it is covered by the per-command redraw request, not listed here.
    fn render_activity_reasons(&mut self) -> Vec<&'static str> {
        let mut reasons = Vec::new();
        // Physics + scripts advance every frame while Playing (Paused / Edit do not).
        if self.editor.play_state == PlayState::Playing {
            reasons.push("play");
        }
        let time_of_day = &self.editor.active_scene().environment.time_of_day;
        if time_of_day.enabled && time_of_day.day_length_seconds > 0.0 {
            reasons.push("time-of-day");
        }
        // Smoothed edits (`set-transform smooth:1`) converge over frames.
        if !self.editor.transform_smoothing.is_empty() {
            reasons.push("smoothing");
        }
        // The camera eases toward its target over a few frames after the last input — the
        // fly-cam look tail and the preview orbit's `set-camera smooth` samples both.
        if self.editor.camera.controlling || self.editor.camera.is_easing() {
            reasons.push("camera");
        }
        // An active asset-placement preview drags a ghost that tracks the cursor; hold continuous
        // render so each drag-over command is serviced at frame rate instead of the idle wake-up
        // latency (otherwise previews queue behind the serialized control bridge and starve input).
        if self.editor.placement_preview.is_some() {
            reasons.push("placement-preview");
        }
        // A clip actively advancing (any rig in Play, or the preview-selected rig in Edit) changes
        // the image even with no new command.
        if self.any_animation_active() {
            reasons.push("animation");
        }
        // Queued main-graph preview tiles (material / texture) drain a small budget per tick; hold
        // full cadence until the queue empties so tiles fill in promptly instead of at idle latency.
        if self.assets.preview_render_pending() {
            reasons.push("thumbnails");
        }
        reasons
    }

    /// Whether any [`AnimationPlayer`] is advancing this frame: any playing rig while Playing, or a
    /// playing preview-in-edit rig while editing. A Paused player advances nothing (a step command
    /// is itself a mutation), so it does not hold continuous render.
    fn any_animation_active(&mut self) -> bool {
        let playing_mode = self.editor.play_state == PlayState::Playing;
        let editing = self.editor.play_state == PlayState::Edit;
        let scene = self.editor.active_scene();
        let mut active = false;
        scene.for_each::<&AnimationPlayer, _>(|_, player| {
            if player.playing && (playing_mode || (editing && player.preview_in_edit)) {
                active = true;
            }
        });
        active
    }

    /// Builds the `EngineContext` borrow from the host's own fields and drains the control
    /// socket once. The borrow struct is assembled here and never stored; `physics` crosses as
    /// the live play world or `None`.
    ///
    /// Returns `true` when a mutating command ran this drain (the reactive-redraw signal); a drain
    /// skipped for a missing uploader reports `false`.
    fn poll_control(&mut self, window: &mut Window, renderer: &mut Renderer) -> bool {
        // The control plane's GPU-upload seam needs the host-owned one-off uploader; build
        // it before assembling the borrow (the asset commands resolve/upload through it).
        self.ensure_uploader(renderer);
        let Some(uploader) = self.uploader.as_ref() else {
            return false; // No uploader (device create failed): the control drain is skipped.
        };
        let mut control_renderer = HostControlRenderer::new(renderer, uploader);
        // Lend the live play world into the `EngineContext::physics` borrow: a `RefMut` on the
        // runtime's shared world cell, held for the drain's duration. The cell is an owned `Rc`
        // clone, so borrowing it does not alias `self.editor`/`self.assets`; no simulation step
        // runs during the drain, so the world is free to borrow here.
        let physics_cell = self.runtime.physics_cell();
        let mut physics = physics_cell.borrow_mut();
        self.control.poll(
            window,
            &mut control_renderer,
            &mut self.editor,
            &mut self.assets,
            &mut self.spatial,
            physics.as_mut(),
        )
    }

    /// Updates the editor viewport's shared predicted residency source.
    fn update_spatial_source(&mut self) {
        const EDITOR_VIEW_SOURCE: SpatialSourceId = SpatialSourceId(1);
        let camera_position = self
            .editor
            .render_camera_view()
            .view
            .inverse()
            .w_axis
            .truncate();
        let Ok(position) =
            WorldPosition::from_render_relative(camera_position, WorldPosition::origin())
        else {
            self.spatial.remove_source(EDITOR_VIEW_SOURCE);
            return;
        };
        let ticks = position.global_ticks();
        let revision = ticks
            .into_iter()
            .fold(0xcbf2_9ce4_8422_2325_u64, |hash, value| {
                value.to_le_bytes().into_iter().fold(hash, |hash, byte| {
                    (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
                })
            });
        let source = SpatialSource {
            id: EDITOR_VIEW_SOURCE,
            revision,
            position,
            velocity_mps: glam::DVec3::ZERO,
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
            facets: ResidencyMask::one(ResidencyFacet::Render).with(ResidencyFacet::Editing),
            priority: 100,
        };
        if let Err(error) = self.spatial.update_source(source) {
            tracing::warn!("spatial source rejected: {error}");
        }
    }

    /// Reconciles the play world + script VM against the editor's play state on the Edit↔Playing
    /// edge.
    ///
    /// Runs each `on_update` right after `poll_control` releases the editor borrow — the only
    /// borrow-sound place, since the published transition holds `&mut editor`. On Edit→Playing it
    /// starts the runtime (builds the Jolt world from the play scene + the script VM); on →Edit it
    /// stops it (drops the world, stops the VM). Pause/Resume keep the session — only the Edit
    /// boundary builds or tears it.
    fn reconcile_play_edge(&mut self) {
        let now = self.editor.play_state;
        if now == self.last_play_state {
            return;
        }
        let was_edit = self.last_play_state == PlayState::Edit;
        let is_edit = now == PlayState::Edit;
        self.last_play_state = now;

        if was_edit && !is_edit {
            self.enter_play_session();
        } else if !was_edit && is_edit {
            self.exit_play_session();
        }
    }

    /// Starts the runtime on the Edit→Playing edge: builds the world + script VM from the play
    /// scene (`RuntimeSession::start`), mirrors the instance count onto the editor, and routes any
    /// `on_create` log lines into the editor's ring (the editor is freely borrowable here).
    fn enter_play_session(&mut self) {
        let project_root = std::path::PathBuf::from(&self.editor.project_root);
        {
            let scene = self.editor.active_scene();
            self.runtime.start(scene, &mut self.assets, &project_root);
        }
        self.editor.script_instance_count =
            i32::try_from(self.runtime.instance_count()).unwrap_or(i32::MAX);
        self.drain_runtime_sinks();
    }

    /// Stops the runtime on the Playing/Paused→Edit edge (drops the world + stops the VM) and
    /// clears the editor's instance count. The Jolt globals outlive every world, so they shut
    /// down only in the host teardown, after the last world is gone.
    fn exit_play_session(&mut self) {
        self.runtime.stop();
        self.editor.script_instance_count = 0;
    }

    /// Routes what the runtime buffered this step into the editor's rings — the `sa.log` lines and
    /// the contained script errors — and reports whether any error fired (the caller defers a
    /// pause). Called after each gated `step` and after the play-edge `start`, when the editor is
    /// freely borrowable.
    fn drain_runtime_sinks(&mut self) -> bool {
        for line in self.runtime.take_logs() {
            self.editor.push_script_log(line.sender, line.message);
        }
        let errors = self.runtime.take_errors();
        let had_error = !errors.is_empty();
        for err in errors {
            self.editor
                .push_script_error(err.entity_uuid.0, err.script, err.message);
        }
        had_error
    }

    /// Brings the project up from the editor-set environment once at attach time, before the
    /// first frame. Routes through the control context's one project-bring-up path, against the
    /// renderer's upload seam — so a host launched with `SAFFRON_PROJECT` /
    /// `SAFFRON_SCRATCH_PROJECT` / a `project.json` has a loaded scene before the loop starts,
    /// instead of an empty one waiting on the editor.
    fn bootstrap_project(&mut self) {
        // Seed the loader inbox from the editor-set environment; the load itself runs non-blocking
        // across the first frames via `advance_project_load`, so the first frame is never stalled.
        self.control.bootstrap_project_from_env(&mut self.editor);
    }

    /// Advances the non-blocking project loader one bounded step against the live renderer, editor,
    /// and asset borrows (mirrors [`Self::poll_control`]'s borrow assembly). Returns `true` when the
    /// step did redraw-worthy work.
    fn advance_project_load(&mut self, renderer: &mut Renderer) -> bool {
        self.ensure_uploader(renderer);
        let Some(uploader) = self.uploader.as_ref() else {
            return false;
        };
        let mut control_renderer = HostControlRenderer::new(renderer, uploader);
        self.control
            .advance_project_load(&mut control_renderer, &mut self.editor, &mut self.assets)
    }

    /// Renders a small budget of queued material / texture preview tiles through the main forward+
    /// graph (the interactive previewer's path) on the offscreen thumbnail view, writing each to the
    /// disk cache the editor repolls for. The worker cannot drive the main graph (it lives on the
    /// render thread), so [`saffron_assets::request_thumbnail`] enqueues these and this drains them.
    /// A small per-tick budget offsets the multi-frame converge cost of each tile.
    fn drive_preview_render_queue(&mut self, renderer: &mut Renderer) {
        /// Tiles rendered per `on_update` tick — each converges several frames, so keep it small.
        const MAX_PREVIEW_RENDERS_PER_TICK: usize = 2;

        if !self.assets.preview_render_pending() {
            return;
        }
        self.ensure_uploader(renderer);
        if self.uploader.is_none() {
            return;
        }
        let skinning = renderer.skinning_enabled();
        let jobs = self
            .assets
            .take_preview_render_jobs(MAX_PREVIEW_RENDERS_PER_TICK);
        let uploader = self.uploader.as_ref().expect("uploader present");
        let assets = &mut self.assets;
        for job in &jobs {
            let subject = match job.kind {
                PreviewRenderKind::Material(id) => PreviewSubject::Material(id),
                PreviewRenderKind::TextureRole { tid, role } => {
                    PreviewSubject::TextureRole { tid, role }
                }
                PreviewRenderKind::Mesh(id) => PreviewSubject::Mesh(id),
                PreviewRenderKind::Model(id) => PreviewSubject::Model(id),
                PreviewRenderKind::Hdri(tid) => PreviewSubject::Hdri(tid),
            };
            // Build the furnished scene (transient uploader over the renderer's descriptors), then
            // render it (the uploader borrow ends with the block, freeing the renderer).
            let (mut scene, _root, camera) = {
                let gpu = RendererUploader::new(uploader, renderer.descriptors(), skinning);
                build_preview_scene_for_thumbnail(
                    assets,
                    &gpu,
                    subject,
                    PREVIEW_THUMBNAIL_MATERIAL_ID,
                )
            };
            let view = camera.view();
            match render_preview_scene_to_png(
                renderer, uploader, skinning, &mut scene, assets, &view, job.size,
            ) {
                Ok(png) => {
                    if let Err(err) =
                        write_thumbnail_cache(std::path::Path::new(&job.cache_path), &png.bytes)
                    {
                        tracing::warn!("preview thumbnail cache write: {err}");
                    }
                }
                Err(err) => tracing::error!("preview thumbnail render: {err}"),
            }
            assets.finish_preview_render(&job.cache_path);
        }
    }

    /// Renders the scene through the active camera and submits the native gizmo overlay: track
    /// the viewport size in present mode, sync the gizmo, render the scene, then build + submit
    /// the edit overlay geometry.
    fn render_ui(&mut self, window: Option<&Window>, renderer: &mut Renderer) {
        // Publish mode: the editor owns the render size (set-viewport-size); the hidden
        // window's size is meaningless. Present mode tracks the window.
        if !self.shm_publish
            && let Some(window) = window
        {
            let view = renderer.active_view_id();
            let _ = renderer.set_viewport_desired_size(view, window.width(), window.height());
        }

        self.editor.sync_native_gizmo();
        let cam = self.editor.render_camera_view();
        let (view_width, view_height) = (renderer.viewport_width(), renderer.viewport_height());
        if view_width == 0 || view_height == 0 {
            return;
        }

        let options = RenderSceneOptions {
            show_editor_camera_models: self.editor.play_state == PlayState::Edit,
            show_grid: self.editor.debug_overlays.grid && self.editor.editor_chrome_visible(),
        };

        let skinning = renderer.skinning_enabled();
        self.ensure_uploader(renderer);
        if let Some(uploader) = self.uploader.as_ref() {
            let mut driver = RendererScene::new(renderer, uploader, skinning);
            let scene: &mut Scene = self.editor.active_scene();
            render_scene(&mut driver, scene, &mut self.assets, &cam, options);
        }

        self.submit_scene_edit_overlay(renderer, &cam, view_width, view_height);

        // Tell the renderer whether to fold the active view's BGRA8 shm readback into this
        // frame's command buffer, so it records the blit/copy inline — no separate submit, no
        // synchronous stall.
        if self.shm_publish {
            self.arm_active_view_shm(renderer);
        }

        // Execute the offscreen scene graph (pass 1: scene → offscreen). The editor/headless
        // host never presents a swapchain — the BGRA8 read-back into the shared-memory ring is
        // its frame transport. The copy is recorded into the frame command buffer above; this
        // submits it. A failure is logged, not fatal.
        if let Err(err) = renderer.render_scene_offscreen() {
            tracing::error!("render_scene_offscreen: {err}");
            return;
        }
        if self.shm_publish {
            self.publish_pipelined_view(renderer);
        }
    }

    /// Arms the renderer's per-view shm-publish flags from the host's segment wiring, so
    /// [`Renderer::render_scene_offscreen`] knows whether to fold the active view's readback
    /// into the frame command buffer.
    fn arm_active_view_shm(&mut self, renderer: &mut Renderer) {
        for (view, shm_view) in [
            (saffron_rendering::ViewId::Scene, ShmView::Scene),
            (
                saffron_rendering::ViewId::AssetPreview,
                ShmView::AssetPreview,
            ),
        ] {
            renderer.set_shm_publish_enabled(view, self.shm.is_enabled(shm_view));
        }
    }

    /// Drains the renderer's pipelined BGRA8 bytes (a frame whose GPU work completed a few
    /// frames ago, read back stall-free at the begin-frame fence wait) and publishes them
    /// into the view's shm segment. A no-op when no completed slot is staged.
    fn publish_pipelined_view(&mut self, renderer: &mut Renderer) {
        let Some((view_id, width, height, pixels)) = renderer.pending_shm_view() else {
            return;
        };
        let view = match view_id {
            saffron_rendering::ViewId::Scene => ShmView::Scene,
            saffron_rendering::ViewId::AssetPreview => ShmView::AssetPreview,
            // The Thumbnail view is never shm-published — its readback goes straight to a PNG, so
            // it never stages a pipelined slot here.
            saffron_rendering::ViewId::Thumbnail => return,
        };
        if !self.shm.is_enabled(view) {
            return;
        }
        self.shm.publish(view, width, height, pixels);
    }

    /// Builds the native gizmo overlay geometry and submits it. `edit_chrome` (Edit and not
    /// previewing) gates the gizmo / billboards / frustums / debug overlays; colliders + the
    /// skeleton draw outside it.
    fn submit_scene_edit_overlay(
        &mut self,
        renderer: &mut Renderer,
        cam: &CameraView,
        width: u32,
        height: u32,
    ) {
        let edit_chrome = self.editor.editor_chrome_visible();
        // The overlay's debug/collider builders resolve meshes through the renderer's uploader
        // + descriptors (the bindless texture binds); the gizmo / billboards / skeleton are
        // pure projection. Skinning is off for the resolve (bounds only, no skin stream).
        let (depth_tested, on_top) = match self.uploader.as_ref() {
            Some(uploader) => {
                let gpu = RendererUploader::new(uploader, renderer.descriptors(), false);
                build_scene_edit_overlay(
                    &mut self.editor,
                    &mut self.assets,
                    &gpu,
                    cam,
                    width,
                    height,
                    edit_chrome,
                )
            }
            None => (Vec::new(), Vec::new()),
        };
        renderer.submit_overlay(depth_tested, on_top);
    }

    /// Lazily builds the host-owned one-off [`Uploader`] from the renderer's device + queue.
    /// The uploader is `Arc`-rooted in the device resources, so it outlives any single-frame
    /// `&mut Renderer` borrow.
    fn ensure_uploader(&mut self, renderer: &Renderer) {
        if self.uploader.is_some() {
            return;
        }
        let queue = renderer.device().graphics_queue.clone();
        match Uploader::new(renderer.device(), &queue) {
            Ok(uploader) => self.uploader = Some(uploader),
            Err(err) => tracing::error!("uploader create failed: {err}"),
        }
    }

    /// The load-bearing teardown sequence, run from `on_detach` after the loop's `wait_gpu_idle`
    /// and before the renderer drops. Discards the step record; [`HostLayer::teardown_recording`]
    /// is the same sequence with the order observable.
    fn teardown(&mut self) {
        let mut steps = Vec::new();
        self.teardown_recording(&mut steps);
    }

    /// The teardown sequence, emitting each step into `steps` in execution order. The
    /// cross-object ordering is the host's load-bearing safety contribution (a runtime UAF if
    /// wrong, not a compile error), so the order is pinned here and asserted by a test reading
    /// `steps`. The production [`HostLayer::teardown`] passes a throwaway vec.
    ///
    /// The order:
    /// 1. Close the control socket.
    /// 2. Quit can land mid-play: stop the script VM (it never touches the scene), drop the
    ///    physics world (RAII frees its Jolt bodies + detaches ragdolls before they destruct),
    ///    then shut down the Jolt globals **after** that last world is gone (the
    ///    `Factory`/registered types outlive every body), then unsubscribe the two play-state
    ///    hooks. The first three steps delegate to the runtime, which owns the VM + world +
    ///    globals.
    /// 3. Drop the host's one-off uploader + clear every cached GPU `Ref` **before** the renderer
    ///    frees the device/allocator — otherwise the last `Arc<GpuMesh>`/`Arc<GpuTexture>` drop
    ///    would free a GPU resource after its allocator is gone (UAF). The loop already idled the
    ///    GPU, so clearing under an idle device is safe.
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

        // The uploader's `Arc<DeviceResources>` must release before the renderer frees the
        // device, so drop it alongside the cached GPU `Ref`s.
        self.uploader = None;
        self.assets.clear_asset_caches();
        steps.push(TeardownStep::GpuCachesCleared);
    }
}

impl Layer for HostLayer {
    fn name(&self) -> &str {
        "HostLayer"
    }

    fn on_attach(&mut self, app: &mut App) {
        // Bring the project up from the editor-set environment before the first frame, then
        // auto-select the first mesh entity so the native-viewport host (no hierarchy panel)
        // starts with something selected. Both need the renderer; the GPU-free test host skips
        // them and drives the session spine alone.
        self.bootstrap_project();
        self.auto_select_first_mesh();
        // The bootstrap scene loads here, not via a control command, so it raises no mutation
        // signal — seed one redraw so the initial scene paints (and the temporal effects converge
        // over the keep-warm window) before the reactive loop idles a static viewport.
        app.redraw.request_redraw();
    }

    fn on_update(&mut self, app: &mut App, dt: TimeSpan) {
        let current_ppid = self.current_ppid();

        // Control first (it reads + mutates the editor through `EngineContext`), so a command
        // this frame takes effect this frame. It needs the renderer + a window; with no
        // renderer attached (the test host) it is skipped and the session spine still runs.
        // `frame_host` and `window` are distinct `App` fields, so they borrow disjointly.
        let mut mutated = false;
        if let Some(renderer) = app.frame_host.renderer_mut() {
            self.update_spatial_source();
            // Headless editor mode has no window; the control plane still takes a `Window`
            // facade, so a standalone headless window stands in (its size is unused in publish
            // mode and its signals are inert without an event loop).
            let mut headless = Window::headless();
            let window = app.window.as_mut().unwrap_or(&mut headless);
            mutated = self.poll_control(window, renderer);
            // Render a small budget of queued preview tiles through the main graph on the offscreen
            // thumbnail view, writing each to the disk cache for the editor's repoll.
            self.drive_preview_render_queue(renderer);
            // Advance a non-blocking project load one bounded step (doc-worker poll / install /
            // residency), so a load never stalls the drain + publish loop.
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

        // Drive the reactive render loop: hold continuous render while some state evolves on its own
        // (a play sim, an edit smoothing, the fly-cam easing, a clip advancing), request one frame
        // when a mutating command landed, and otherwise let the loop idle the GPU on a static scene.
        // Read the renderer's temporal-active + power-state up front (one borrow), then drive the
        // controller and push the observability snapshot back (a second borrow; `frame_host` and
        // `redraw` are distinct `App` fields).
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
        // Suppress rendering while a project load is in flight: the scene + asset caches are being
        // swapped across frames, so a mid-load render would draw against a torn scene (and, with the
        // caches cleared, can wedge the GPU). The viewport is parked behind the loading screen
        // anyway. `on_update` still runs every iteration, so the control socket stays responsive and
        // the loader advances; the flip to `Ready` requests a redraw, so the loaded scene draws then.
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
        // The scene reads its catalog through an `Arc<AssetCatalog>` shared from the asset
        // server (the asset ops keep it in sync). The remaining `on_ui` work is the scene render
        // + overlay submit, which needs the renderer.
        if let Some(renderer) = app.frame_host.renderer_mut() {
            let window = app.window.as_ref();
            self.render_ui(window, renderer);
        }
    }

    fn on_detach(&mut self, _app: &mut App) {
        self.teardown();
    }
}

/// Renders a throwaway preview `scene` through the main forward+ graph on the offscreen
/// [`saffron_rendering::ViewId::Thumbnail`] view and returns the encoded PNG — the one render
/// primitive both the async Assets-tile queue and the sync `preview-render` seam drive. Converges
/// temporal effects and waits for the preview environment's asynchronous IBL refresh and derived
/// lighting capture before readback. Restores the prior active view *without* resetting its temporal
/// state, so a `Scene → Thumbnail → Scene` excursion never wipes the live viewport's accumulated
/// history.
pub(crate) fn render_preview_scene_to_png(
    renderer: &mut Renderer,
    uploader: &Uploader,
    skinning: bool,
    scene: &mut Scene,
    assets: &mut AssetServer,
    camera: &CameraView,
    size: u32,
) -> saffron_rendering::Result<saffron_rendering::ThumbnailPng> {
    /// Minimum frames rendered before readback so temporal effects converge to the previewer's look.
    const MIN_CONVERGE_FRAMES: u32 = 8;
    /// Safety bound for a failed asynchronous environment refresh.
    const MAX_CONVERGE_FRAMES: u32 = 256;

    let prev_view = renderer.active_view_id();
    renderer.set_active_view(saffron_rendering::ViewId::Thumbnail);
    let result = (|| {
        renderer.set_viewport_desired_size(saffron_rendering::ViewId::Thumbnail, size, size)?;
        let options = RenderSceneOptions {
            show_editor_camera_models: false,
            show_grid: false,
        };
        let mut converged = false;
        for frame in 0..MAX_CONVERGE_FRAMES {
            {
                let mut driver = RendererScene::new(renderer, uploader, skinning);
                render_scene(&mut driver, scene, assets, camera, options);
            }
            renderer.render_scene_offscreen()?;
            if frame + 1 >= MIN_CONVERGE_FRAMES && renderer.active_environment_converged() {
                converged = true;
                break;
            }
        }
        if !converged {
            return Err(saffron_rendering::Error::ShaderLoad(
                "thumbnail environment did not converge".to_owned(),
            ));
        }
        renderer.encode_active_offscreen_png()
    })();
    renderer.restore_active_view_no_reset(prev_view);
    result
}

#[cfg(test)]
impl HostLayer {
    /// Drives the host into a live mid-play session through the real play edge (`enter_play` →
    /// `reconcile_play_edge` → `RuntimeSession::start`), so a teardown test exercises a real
    /// play→quit. Requires the Jolt globals to install (`World::new`); the caller probes first
    /// and skips cleanly when the toolchain is absent.
    fn set_play_session_for_test(&mut self) {
        self.editor.enter_play().expect("enter play");
        self.reconcile_play_edge();
    }

    /// Whether the Jolt globals are still flagged installed (a test reads the post-teardown state).
    fn physics_init_for_test(&self) -> bool {
        self.runtime.physics_init()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use saffron_physics::World;
    use saffron_sceneedit::PlayState;
    use std::sync::Mutex;

    // The Jolt `Factory::sInstance` is a process global the world bring-up + `shutdown_physics`
    // touch; serialize the tests that build/drop a world so they never race it (mirrors the
    // physics crate's `JOLT_GLOBAL`). Recover from a poisoned lock — it only guards the global
    // init race, so a panic in one test leaves no shared state to corrupt.
    static JOLT_GLOBAL: Mutex<()> = Mutex::new(());
    fn jolt_guard() -> std::sync::MutexGuard<'static, ()> {
        JOLT_GLOBAL.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// A unique, never-touched asset root per test so two tests never share a scratch dir.
    fn scratch_root(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "saffron-host-{tag}-{}-{:p}",
            std::process::id(),
            tag
        ))
    }

    /// A host layer built standalone (not editor-spawned), no shm — the GPU-free spine.
    fn standalone(tag: &str) -> HostLayer {
        HostLayer::new(scratch_root(tag), false, false)
    }

    #[test]
    fn host_layer_constructs_without_gpu() {
        let host = standalone("construct");

        // The editor, asset server, and animation runtime are live without a renderer.
        assert_eq!(host.editor().play_state, PlayState::Edit);
        assert!(!host.shm_publishing());
        assert!(!host.has_physics());

        // The clip loader is installed: a clip id that resolves against an empty catalog
        // returns the typed `ClipLoad` error (not a panic / not "no loader"), proving the
        // loader closure is wired.
        // (The loader is exercised indirectly; here we assert the two play-state hooks are
        // live tokens — dropping the layer must unsubscribe them with no dangling sub.)
        assert!(host.play_hooks_live(), "play-state hooks are subscribed");

        // Dropping (via an explicit detach) unsubscribes both play-state hooks.
        let mut host = host;
        host.teardown();
        assert!(
            !host.play_hooks_live(),
            "detach unsubscribed the play-state hooks (no dangling subscription)"
        );
    }

    #[test]
    fn parent_death_sets_should_close() {
        // Editor-spawned: the captured pid is the live parent; a matching observation stays
        // alive, a mismatched one (the editor vanished → reparent) requests exit.
        let host = HostLayer::new(scratch_root("ppid"), true, false);
        let captured = host.editor_pid;

        assert_eq!(
            host.watch_parent(captured),
            ParentWatch::Alive,
            "an unchanged parent keeps running"
        );

        // A changed observation (the editor vanished → reparent away, parent now `None` or a
        // different pid) trips the watch. `None` is the unambiguous "no parent" reparent.
        assert_eq!(
            host.watch_parent(None),
            ParentWatch::ParentDied,
            "a changed parent (editor gone) requests exit"
        );

        // The update step turns the verdict into a session abort: an armed host that observes
        // a vanished parent returns `ParentDied` before touching the rest of the spine.
        let mut armed = HostLayer::new(scratch_root("ppid-armed"), true, false);
        assert_eq!(
            armed.update_session(TimeSpan::from_seconds(0.016), None),
            ParentWatch::ParentDied,
            "the session update aborts on the parent-death verdict"
        );

        // A non-editor-spawned host never watches, whatever the observed pid.
        let mut standalone = standalone("ppid-standalone");
        assert_eq!(
            standalone.watch_parent(None),
            ParentWatch::Alive,
            "a standalone host is never auto-killed by the parent watch"
        );
        assert_eq!(
            standalone.update_session(TimeSpan::from_seconds(0.016), None),
            ParentWatch::Alive,
            "a standalone session update runs the full spine"
        );
    }

    #[test]
    fn preview_prune_clears_runtime() {
        let mut host = standalone("prune");

        // No preview yet: an update at Edit does not prune (the edge has not flipped).
        let dt = TimeSpan::from_seconds(0.016);
        host.update_session(dt, None);
        assert!(!host.editor().previewing());

        // Enter the asset preview as the active view (the `previewing()` edge false→true).
        let mut preview = Scene::new();
        let _ = preview.create_entity("preview-root");
        host.editor_mut().preview_scene = Some(preview);
        host.editor_mut().preview_active_view = true;
        assert!(host.editor().previewing());

        // The next update sees the edge and prunes the runtime exactly once. The runtime has
        // no per-entity entries to begin with (a fresh session), so the observable contract
        // is that the preview-active tracking flipped and the prune ran on the edge.
        assert_eq!(host.animation().session_entry_count(), 0);
        host.update_session(dt, None);
        assert!(
            host.preview_active,
            "the preview-active tracking flips on the enter edge"
        );
        assert_eq!(
            host.animation().session_entry_count(),
            0,
            "the runtime stays pruned across the preview"
        );

        // Leaving the preview (true→false) is the symmetric edge; the tracking flips back.
        host.editor_mut().preview_active_view = false;
        host.editor_mut().preview_scene = None;
        host.update_session(dt, None);
        assert!(
            !host.preview_active,
            "the preview-active tracking flips back on the leave edge"
        );
    }

    #[test]
    fn update_order_is_animation_then_step() {
        // A play/step command this frame must take effect this frame: stepping while Paused
        // grants one fixed tick that `play_step_dt` (run after animation, inside update_session)
        // consumes, advancing `play_tick`. We arm a step, then a single update consumes it.
        let mut host = standalone("order");

        host.editor_mut().enter_play().expect("enter play");
        host.editor_mut().pause_play().expect("pause");
        let before = host.editor().play_tick;
        host.editor_mut().step_play(1).expect("step");

        let dt = TimeSpan::from_seconds(0.016);
        // The fly-cam look-delta is set non-zero, and the update must drain it to zero each
        // frame (a burst between frames is otherwise lost).
        host.editor_mut().fly_input.look_delta = Vec2::new(12.0, -7.0);

        let watch = host.update_session(dt, None);
        assert_eq!(watch, ParentWatch::Alive);

        assert_eq!(
            host.editor().play_tick,
            before + 1,
            "the stepped tick ran this frame (the gated step consumed it after animation)"
        );
        assert_eq!(
            host.editor().fly_input.look_delta,
            Vec2::ZERO,
            "the fly-cam look-delta is drained to zero each update"
        );
    }

    /// The play-edge wiring proper, CPU-only: entering Play builds a Jolt world from the play
    /// scene, the per-frame `update_session` steps it (the `sim_tick` seam) and writes the
    /// dynamic body's pose back into the play scene, and stopping drops the world and restores
    /// Edit. This is the unit-level mirror of `physics-falling-box.test.ts`, exercised end to
    /// end without a renderer.
    #[test]
    fn play_edge_builds_a_world_and_steps_the_box() {
        use saffron_scene::{Collider, Rigidbody, Transform};
        let _guard = jolt_guard();
        // Skip cleanly if the Jolt globals cannot install (no toolchain) — not a false pass.
        match World::new() {
            Ok(_) => {}
            Err(err) => {
                eprintln!("skipping: World::new failed: {err}");
                return;
            }
        }

        let mut host = standalone("play-edge");

        // A static floor (a wide thin collider, no rigidbody → implicitly static) and a dynamic
        // box dropped from y=5 (default 0.5 half-extent box, default Dynamic rigidbody).
        let scene = &mut host.editor_mut().scene;
        let floor = scene.create_entity("Floor");
        scene
            .add_component(
                floor,
                Collider {
                    half_extents: glam::Vec3::new(10.0, 0.1, 10.0),
                    ..Collider::default()
                },
            )
            .expect("floor collider");
        let cube = scene.create_entity("Box");
        scene
            .add_component(
                cube,
                Transform {
                    translation: glam::Vec3::new(0.0, 5.0, 0.0),
                    scale: glam::Vec3::ONE,
                    rotation: glam::Vec3::ZERO,
                },
            )
            .expect("box transform");
        scene
            .add_component(cube, Collider::default())
            .expect("box collider");
        scene
            .add_component(cube, Rigidbody::default())
            .expect("box rigidbody");
        let cube_uuid = scene
            .component::<saffron_scene::IdComponent>(cube)
            .map(|id| id.id)
            .expect("box id");

        // No world in Edit — the authored box sits at its authored height.
        assert!(!host.has_physics(), "no world before play");
        assert_eq!(host.editor().scene.world_matrix(cube).w_axis.y, 5.0);

        // Enter play and tick: the first update reconciles the Edit→Playing edge (builds the
        // world from the play scene), then steps it.
        host.editor_mut().enter_play().expect("enter play");
        let dt = TimeSpan::from_seconds(0.016);
        // ~3s of fixed steps: the box falls under gravity and settles on the floor.
        for _ in 0..200 {
            host.update_session(dt, None);
        }

        assert!(host.has_physics(), "the world is live during play");
        // Read the play twin's world Y: it dropped well below the authored 5 and settled near
        // the floor top (0.1) + the box half-extent (0.5) ≈ 0.6, never tunneling through.
        let play_cube = host
            .editor_mut()
            .active_scene()
            .find_entity_by_uuid(cube_uuid)
            .expect("play twin");
        let settled_y = host
            .editor_mut()
            .active_scene()
            .world_matrix(play_cube)
            .w_axis
            .y;
        assert!(settled_y < 5.0, "the box fell from 5: now {settled_y}");
        assert!(
            (0.4..1.0).contains(&settled_y),
            "the box settled at ~floor-top + half-extent: {settled_y}"
        );

        // Stop: the world drops, Edit returns, and the authored box is untouched (never written
        // during play — the duplicate held every write).
        host.editor_mut().stop_play().expect("stop");
        host.update_session(dt, None);
        assert!(!host.has_physics(), "the world dropped on stop");
        assert_eq!(host.editor().play_state, PlayState::Edit);
        assert_eq!(
            host.editor().scene.world_matrix(cube).w_axis.y,
            5.0,
            "the authored box is back at its authored height"
        );

        host.teardown();
    }

    #[test]
    fn teardown_unsubscribes_and_drops_in_order() {
        let _guard = jolt_guard();
        // Skip cleanly when the Jolt globals cannot install (no toolchain) — not a false pass.
        match World::new() {
            Ok(_) => {}
            Err(err) => {
                eprintln!("skipping: World::new failed: {err}");
                return;
            }
        }
        let mut host = standalone("teardown-order");

        // A play session active via the real play edge: a live Jolt world + script VM, with both
        // play-state subscriptions live. Quit can land here, mid-play.
        host.set_play_session_for_test();
        assert!(host.play_hooks_live(), "the play-state hooks start live");
        assert!(host.has_physics(), "the play world is present");
        assert!(
            host.physics_init_for_test(),
            "the Jolt globals are installed"
        );

        // Record the teardown order and assert it matches the pinned sequence.
        let mut steps = Vec::new();
        host.teardown_recording(&mut steps);
        assert_eq!(
            steps,
            vec![
                TeardownStep::ControlClosed,
                TeardownStep::ScriptsStopped,
                TeardownStep::PhysicsWorldDropped,
                TeardownStep::JoltGlobalsShutdown,
                TeardownStep::PlayHooksUnsubscribed,
                TeardownStep::GpuCachesCleared,
            ],
            "the teardown order drops every subsystem before the device"
        );

        // The world drops strictly before the Jolt-globals shutdown (a live world holds Jolt
        // bodies; shutting the Factory down first would be a UAF).
        let world_pos = steps
            .iter()
            .position(|s| *s == TeardownStep::PhysicsWorldDropped)
            .unwrap();
        let jolt_pos = steps
            .iter()
            .position(|s| *s == TeardownStep::JoltGlobalsShutdown)
            .unwrap();
        assert!(
            world_pos < jolt_pos,
            "the physics world dropped before the Jolt globals shut down"
        );

        // Post-teardown state: subscriptions gone, world gone, globals flagged down, script flag
        // cleared — back to the fresh, drop-safe state.
        assert!(
            !host.play_hooks_live(),
            "teardown unsubscribed both play-state hooks (no dangling subscription)"
        );
        assert!(!host.has_physics(), "the play world was dropped");
        assert!(
            !host.physics_init_for_test(),
            "the Jolt globals were shut down (physics_init cleared)"
        );
        assert!(!host.script_vm_active(), "the script VM flag was cleared");

        // Teardown is idempotent: a second pass is a clean no-op (a double on_detach must not
        // double-free or re-shutdown the globals).
        let mut again = Vec::new();
        host.teardown_recording(&mut again);
        assert!(!host.has_physics() && !host.physics_init_for_test());
    }

    #[test]
    fn ref_caches_drop_before_renderer() {
        // The asset GPU `Ref` caches must empty strictly before the renderer's device/allocator
        // drops. The host has no renderer in the test harness, so we prove the host's half: the
        // teardown step that clears the caches (`GpuCachesCleared`) runs while the host is still
        // alive — i.e. before `on_detach` returns and the run loop drops the renderer. We assert
        // the cache-clear is the *last* teardown step (the renderer drop happens strictly after,
        // outside the host), and that the asset GPU caches are empty afterward.
        let _guard = jolt_guard();
        let mut host = standalone("ref-caches");

        let mut steps = Vec::new();
        host.teardown_recording(&mut steps);

        assert_eq!(
            steps.last().copied(),
            Some(TeardownStep::GpuCachesCleared),
            "clearing the GPU Ref caches is the final host teardown step, before the renderer drops"
        );
        // Every host-owned GPU cache is emptied (the last `Arc<GpuMesh>`/`Arc<GpuTexture>` drop
        // runs here, under the idle device, not after the allocator is gone).
        assert!(
            host.assets.mesh_by_uuid.is_empty()
                && host.assets.texture_by_uuid.is_empty()
                && host.assets.model_by_uuid.is_empty(),
            "the GPU Ref caches are empty after the cache-clear step"
        );
    }

    #[test]
    fn shm_drop_is_device_independent() {
        // Dropping the viewport shm publisher after the renderer is gone must still munmap +
        // shm_unlink cleanly — its `Drop` touches no device. Here there is no renderer at all, so
        // a host carrying an enabled segment that is dropped proves the shm teardown is fully
        // independent of any GPU state.
        use crate::viewport_shm::{ShmView, ShmViewConfig, ViewportShmPublisher};
        use std::ffi::CString;

        let name = saffron_test_support::unique_shm_name();
        let mut host = standalone("shm-drop");
        let mut shm = ViewportShmPublisher::new();
        shm.enable(ShmViewConfig {
            view: ShmView::Scene,
            name: name.clone(),
        })
        .expect("enable scene segment");
        host.attach_shm_publisher(shm);
        assert!(host.shm_publishing(), "the scene segment is enabled");

        // Tear the host's session down first (no renderer ever existed), then drop the host —
        // the shm publisher's `Drop` runs with no device present and unlinks the segment.
        host.teardown();
        drop(host);

        let cname = CString::new(name).unwrap();
        let opened = rustix::shm::open(
            cname.as_c_str(),
            rustix::shm::OFlags::RDONLY,
            rustix::shm::Mode::empty(),
        );
        assert!(
            opened.is_err(),
            "dropping the host shm_unlinked the segment with no device access"
        );
    }
}
