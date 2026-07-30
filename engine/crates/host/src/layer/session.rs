//! The renderer-independent per-frame spine: the session update, the control poll, and the
//! play-edge and project-load transitions.

use glam::Vec2;

use saffron_animation::AnimMode;
use saffron_assets::{advance_time_of_day, scene_surface_field_snapshots};
use saffron_control::{ControlPollContext, ControlRenderer};

use crate::control_renderer::HostControlRenderer;
use saffron_core::TimeSpan;
use saffron_rendering::Renderer;
use saffron_scene::AnimationPlayer;
use saffron_sceneedit::{PlayState, update_scene_edit_camera};
use saffron_spatial::{
    ResidencyFacet, ResidencyMask, SourceLevel, SpatialSource, SpatialSourceId, WorldPosition,
};
use saffron_window::Window;

use super::*;

impl HostLayer {
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
            // Moving bodies and characters push the world interaction field: each
            // emits one impulse whose kick scales with speed and the step, so the
            // cosmetic bend tracks motion framerate-independently.
            let physics_cell = self.runtime.physics_cell();
            if let Some(physics) = physics_cell.borrow().as_ref() {
                for (position, velocity) in physics.motion_emitters() {
                    let horizontal = saffron_geometry::glam::Vec2::new(velocity.x, velocity.z);
                    let speed = horizontal.length();
                    if speed < 0.5 {
                        continue;
                    }
                    let rate = speed.min(8.0) * 4.0;
                    self.pending_interaction_impulses
                        .push(saffron_rendering::InteractionImpulse {
                            position: [position.x, position.z],
                            radius: 1.0,
                            strength: rate * step_dt,
                            direction: (horizontal / speed).to_array(),
                            depress: rate * 0.25 * step_dt,
                            reserved: 0.0,
                        });
                }
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
    pub(super) fn render_activity_reasons(&mut self) -> Vec<&'static str> {
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
    pub(super) fn any_animation_active(&mut self) -> bool {
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
    pub(super) fn poll_control(&mut self, window: &mut Window, renderer: &mut Renderer) -> bool {
        let vegetation_result = if self.editor.project_ready() {
            self.runtime.synchronize_vegetation(
                self.editor.active_scene(),
                &self.assets,
                &self.spatial,
            )
        } else {
            self.runtime.clear_vegetation(self.editor.active_scene())
        };
        if let Err(error) = vegetation_result {
            tracing::error!("vegetation runtime advance failed: {error}");
        }
        // Hand the sync's stage spans to the profiler. They are timed on the same monotonic clock
        // the renderer stamps its own spans with, so a capture shows residency, promotion,
        // collision and navigation INSIDE the frame they belong to rather than on a second
        // timeline — or, as before this, not at all.
        for (stage, start_ns, duration_ns) in self.runtime.vegetation_telemetry_mut().take_spans() {
            renderer.record_cpu_span(stage.name(), start_ns, duration_ns);
        }
        // The control plane's GPU-upload seam needs the host-owned one-off uploader; build
        // it before assembling the borrow (the asset commands resolve/upload through it).
        self.ensure_uploader(renderer);
        let Some(uploader) = self.uploader.as_ref() else {
            return false; // No uploader (device create failed): the control drain is skipped.
        };
        let mut control_renderer =
            HostControlRenderer::new(renderer, uploader, &mut self.gpu_scene_mirror);
        if self.runtime.vegetation_needs_regeneration() {
            let mut providers = None;
            control_renderer.with_gpu_uploader(&mut |gpu| {
                providers = Some(scene_surface_field_snapshots(
                    gpu,
                    self.editor.active_scene(),
                    &mut self.assets,
                ));
            });
            match providers {
                Some(Ok(providers)) => {
                    if let Err(error) = self
                        .runtime
                        .regenerate_missing_vegetation(&mut self.assets, &providers)
                    {
                        tracing::error!("vegetation runtime regeneration failed: {error}");
                    }
                }
                Some(Err(error)) => {
                    tracing::error!("vegetation runtime surface capture failed: {error}");
                }
                None => {
                    tracing::error!("vegetation runtime surface capture was unavailable");
                }
            }
        }
        // Lend the live play world into the `EngineContext::physics` borrow: a `RefMut` on the
        // runtime's shared world cell, held for the drain's duration. The cell is an owned `Rc`
        // clone, so borrowing it does not alias `self.editor`/`self.assets`; no simulation step
        // runs during the drain, so the world is free to borrow here.
        let vegetation_collision = self.runtime.vegetation_collision_report();
        let physics_cell = self.runtime.physics_cell();
        let mut physics = physics_cell.borrow_mut();
        let vegetation_status = self.runtime.vegetation_status().clone();
        let vegetation_regeneration_cells = self.runtime.missing_vegetation_cells();
        // The promotion authority reaches the control plane only in play: a transition or a save
        // barrier needs the live world its entity views belong to.
        let play_active = physics.is_some();
        let vegetation_cell = self.runtime.vegetation_cell();
        let mut vegetation_ref = vegetation_cell.borrow_mut();
        let (promotion, navigation, ecology, telemetry) =
            self.runtime.vegetation_control_authorities();
        // Promotion is play-only; the navigation seam and the ecology clock reach Edit as well —
        // biology owes its ticks whether or not the world is being played.
        let vegetation_promotion = play_active.then_some(promotion);
        let vegetation_navigation = Some(navigation);
        self.control.poll(ControlPollContext {
            window,
            renderer: &mut control_renderer,
            scene_edit: &mut self.editor,
            assets: &mut self.assets,
            spatial: &mut self.spatial,
            vegetation: &mut vegetation_ref,
            vegetation_status,
            vegetation_regeneration_cells,
            vegetation_collision,
            vegetation_promotion,
            vegetation_navigation,
            vegetation_ecology: Some(ecology),
            vegetation_telemetry: Some(telemetry),
            physics: physics.as_mut(),
        })
    }

    /// Updates the editor viewport's shared predicted residency source.
    pub(super) fn update_spatial_source(&mut self) {
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
        // The viewpoint claims render + editing data always. In play it additionally claims the
        // facets the simulation consumes near the camera: physics collision proxies and the
        // navigation contributions a consumer rebuilds from. Edit mode simulates nothing, so
        // claiming them there would decode bytes no one reads.
        let mut facets = ResidencyMask::one(ResidencyFacet::Render).with(ResidencyFacet::Editing);
        if self.runtime.has_physics() {
            facets = facets
                .with(ResidencyFacet::Physics)
                .with(ResidencyFacet::Navigation);
        }
        let ticks = position.global_ticks();
        let revision = ticks
            .into_iter()
            .flat_map(|value| value.to_le_bytes())
            .chain(std::iter::once(facets.bits()))
            .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
                (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
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
            facets,
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
    pub(super) fn reconcile_play_edge(&mut self) {
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
    pub(super) fn enter_play_session(&mut self) {
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
    pub(super) fn exit_play_session(&mut self) {
        self.runtime.stop();
        self.editor.script_instance_count = 0;
    }

    /// Routes what the runtime buffered this step into the editor's rings — the `sa.log` lines and
    /// the contained script errors — and reports whether any error fired (the caller defers a
    /// pause). Called after each gated `step` and after the play-edge `start`, when the editor is
    /// freely borrowable.
    pub(super) fn drain_runtime_sinks(&mut self) -> bool {
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
    pub(super) fn bootstrap_project(&mut self) {
        // Seed the loader inbox from the editor-set environment; the load itself runs non-blocking
        // across the first frames via `advance_project_load`, so the first frame is never stalled.
        self.control.bootstrap_project_from_env(&mut self.editor);
    }

    /// Advances the non-blocking project loader one bounded step against the live renderer, editor,
    /// and asset borrows (mirrors [`Self::poll_control`]'s borrow assembly). Returns `true` when the
    /// step did redraw-worthy work.
    pub(super) fn advance_project_load(&mut self, renderer: &mut Renderer) -> bool {
        self.ensure_uploader(renderer);
        let Some(uploader) = self.uploader.as_ref() else {
            return false;
        };
        let mut control_renderer =
            HostControlRenderer::new(renderer, uploader, &mut self.gpu_scene_mirror);
        self.control
            .advance_project_load(&mut control_renderer, &mut self.editor, &mut self.assets)
    }
}
