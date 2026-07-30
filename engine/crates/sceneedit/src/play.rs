//! Editor play mode: the `Edit → Playing ↔ Paused → Edit` state machine. All of it lives on
//! [`SceneEditContext`](crate::SceneEditContext), never on `Scene`, and never serializes into the
//! project.
//!
//! Play needs no undo: the duplicate *is* the playground and dropping it on `stop_play` *is* the
//! restore, so there is no restore step to get wrong. The authored scene is never writable through
//! [`active_scene`](crate::SceneEditContext::active_scene) during play. The duplicate comes from a
//! `scene_to_json` / `scene_from_json` round-trip rather than a structural `World` clone, so it
//! cannot diverge from the on-disk format.

/// Editor play mode: `Edit → Playing ↔ Paused → Edit`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum PlayState {
    /// Editing the authored scene (no simulation).
    #[default]
    Edit,
    /// Simulating the play duplicate.
    Playing,
    /// Paused on the play duplicate (single-step granted here).
    Paused,
}

impl PlayState {
    /// The control-plane name (`"edit"` / `"playing"` / `"paused"`).
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            PlayState::Edit => "edit",
            PlayState::Playing => "playing",
            PlayState::Paused => "paused",
        }
    }

    /// The state for a control-plane name, defaulting to [`PlayState::Edit`] on any
    /// unknown spelling.
    #[must_use]
    pub fn from_name(name: &str) -> Self {
        match name {
            "playing" => PlayState::Playing,
            "paused" => PlayState::Paused,
            _ => PlayState::Edit,
        }
    }
}

/// The bounded capacity of the script-error ring; the oldest entry is dropped at the cap.
pub const SCRIPT_ERROR_RING_CAP: usize = 256;

/// The bounded capacity of the script-log ring; the oldest entry is dropped at the cap.
pub const SCRIPT_LOG_RING_CAP: usize = 1024;

/// The deterministic single-step tick (the `step` frame duration), in seconds.
pub const PLAY_FIXED_STEP: f32 = 1.0 / 60.0;

/// The per-frame `dt` clamp, so a hitch never spikes the simulation, in seconds.
pub const PLAY_MAX_DELTA: f32 = 1.0 / 3.0;

/// One contained script failure, kept in a bounded ring on the context and drained over a
/// normal scene command (Control never imports the Lua runtime).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ScriptError {
    /// The monotonic sequence number (drain high-water mark).
    pub seq: i64,
    /// The owning entity's uuid, or `0` when the failure has no owning entity.
    pub entity_uuid: u64,
    /// The script name the failure fired in.
    pub script: String,
    /// The error message.
    pub message: String,
    /// The play tick the error fired on.
    pub tick: i64,
}

/// One `sa.log(...)` line, kept in a bounded ring on the context and drained over a normal
/// scene command (the same path as [`ScriptError`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ScriptLog {
    /// The monotonic sequence number (drain high-water mark).
    pub seq: i64,
    /// The logging entity's uuid, or `0` when logged outside an entity handler.
    pub entity_uuid: u64,
    /// The logged message.
    pub message: String,
    /// A wall-clock millisecond stamp, display-only (never serialized, never deterministic).
    pub epoch_ms: i64,
    /// The play tick the line was logged on.
    pub tick: i64,
}

use std::time::{SystemTime, UNIX_EPOCH};

use saffron_core::Uuid;
use saffron_scene::{AnimationPlayer, CameraView, Entity, IdComponent, Scene};

use crate::context::SceneEditContext;
use crate::error::{Error, Result};
use saffron_scene::ScriptInputState;

impl SceneEditContext {
    /// Sets `play_state`, bumps `play_version`, and publishes the change. The signal is the
    /// physics/scripting lifecycle seam.
    fn publish_transition(&mut self, next: PlayState) {
        self.play_state = next;
        self.play_version += 1;
        self.on_play_state_changed.publish(next);
    }

    /// The selected entity's uuid in `scene`, or `0` when nothing valid is selected.
    /// Resolved against an explicit scene because the selection handle is captured before
    /// crossing the duplicate boundary.
    fn selected_uuid_in(&self, scene: &Scene) -> u64 {
        if !scene.valid(self.selected) {
            return 0;
        }
        scene
            .component::<IdComponent>(self.selected)
            .map_or(0, |id| id.id.0)
    }

    /// Drops the smoothing queue.
    ///
    /// The smoothing targets hold raw `Entity` handles, which index one specific scene's
    /// world — a half-converged edit must never keep converging against the other scene
    /// across a transition.
    fn drop_smoothing(&mut self) {
        self.transform_smoothing.clear();
    }

    /// Enters play mode: duplicates the authored scene by a JSON round-trip, switches to
    /// the duplicate, and lands `Playing`.
    ///
    /// The duplicate is `scene_to_json` then `scene_from_json` into a fresh [`Scene`]
    /// sharing the catalog `Arc` — what a save/load would produce — never a structural
    /// world clone. The selection re-resolves into the duplicate by uuid (the captured
    /// handle indexes the authored world and could alias an unrelated play entity), the
    /// smoothing queues drop, and the script rings clear while their seq stays monotonic
    /// for the drain cursor.
    ///
    /// # Errors
    ///
    /// [`Error::PlayTransition`] if not in [`PlayState::Edit`], or
    /// [`Error::Scene`](crate::Error::Scene) if the duplicate fails to load.
    pub fn enter_play(&mut self) -> Result<()> {
        if self.play_state != PlayState::Edit {
            return Err(Error::PlayTransition(
                "already in play mode — stop first".to_string(),
            ));
        }

        let snapshot = self.scene.scene_to_json(&self.registry);
        let mut play = Scene::new();
        play.catalog = self.scene.catalog.clone();
        play.scene_from_json(&self.registry, &snapshot)?;

        // Play starts from a clean playback state: the editor Timeline preview (scrubbed time
        // / playing) is transient and must not leak in. Each player advances iff its authored
        // `autoplay` is set, from t = 0, with all transition/preview runtime state cleared.
        // The play scene is a throwaway duplicate (`stop_play` discards it), so this never
        // touches the authored scene.
        play.for_each::<&mut AnimationPlayer, _>(|_, p| {
            p.time = 0.0;
            p.playing = p.autoplay;
            p.preview_in_edit = false;
            p.ping_forward = true;
            p.prev_clip = Uuid(0);
            p.transition = 0.0;
            p.transition_duration = 0.0;
        });

        self.had_primary_camera = play.primary_camera().is_some();
        let selected_uuid = self.selected_uuid_in(&self.scene);
        self.drop_smoothing();
        self.play_tick = 0;
        // Each session drains fresh; the seq stays monotonic for the drain cursors.
        self.script_errors.clear();
        self.script_logs.clear();
        self.play_scene = Some(play);
        self.publish_transition(PlayState::Playing);

        // Re-resolve the selection into the duplicate by uuid: the old handle indexes the
        // authored world and could alias an unrelated play entity.
        let twin = self
            .play_scene
            .as_ref()
            .and_then(|scene| scene.find_entity_by_uuid(Uuid(selected_uuid)))
            .unwrap_or(Entity::NULL);
        self.set_selection(twin);

        tracing::info!("enter_play: duplicated the scene");
        Ok(())
    }

    /// Pauses simulation, holding the play duplicate.
    ///
    /// # Errors
    ///
    /// [`Error::PlayTransition`] unless currently [`PlayState::Playing`].
    pub fn pause_play(&mut self) -> Result<()> {
        if self.play_state != PlayState::Playing {
            return Err(Error::PlayTransition(
                "pause requires play mode".to_string(),
            ));
        }
        self.publish_transition(PlayState::Paused);
        Ok(())
    }

    /// Resumes simulation from paused, dropping any pending stepped frames.
    ///
    /// # Errors
    ///
    /// [`Error::PlayTransition`] unless currently [`PlayState::Paused`].
    pub fn resume_play(&mut self) -> Result<()> {
        if self.play_state != PlayState::Paused {
            return Err(Error::PlayTransition("resume requires pause".to_string()));
        }
        self.step_frames = 0;
        self.publish_transition(PlayState::Playing);
        Ok(())
    }

    /// Grants `frames` single-step ticks, consumed by [`Self::play_step_dt`] while paused.
    ///
    /// # Errors
    ///
    /// [`Error::PlayTransition`] unless [`PlayState::Paused`] and `frames >= 1`.
    pub fn step_play(&mut self, frames: i32) -> Result<()> {
        if self.play_state != PlayState::Paused {
            return Err(Error::PlayTransition("step requires pause".to_string()));
        }
        if frames < 1 {
            return Err(Error::PlayTransition(
                "step frames must be >= 1".to_string(),
            ));
        }
        self.step_frames += frames;
        Ok(())
    }

    /// Stops play and returns to Edit, dropping the duplicate.
    ///
    /// Idempotent in Edit (a no-op success). The discard *is* the restore: the authored
    /// scene was never writable through [`active_scene`](Self::active_scene) during play,
    /// so there is nothing to restore. The `scene_version` bump makes the editor's reconcile
    /// re-fetch the authored entity list. The selection restores by uuid; a runtime-spawned
    /// selection has no authored twin and clears.
    ///
    /// # Errors
    ///
    /// Never errors; returns `Result` for call-site symmetry with the other transitions.
    pub fn stop_play(&mut self) -> Result<()> {
        if self.play_state == PlayState::Edit {
            return Ok(());
        }

        let selected_uuid = self
            .play_scene
            .as_ref()
            .map_or(0, |scene| self.selected_uuid_in(scene));
        self.drop_smoothing();
        self.play_scene = None;
        self.step_frames = 0;
        self.script_input = ScriptInputState::default();
        // The discard is the restore: the authored scene was never writable through
        // active_scene during play. The scene_version bump makes the editor's heavy
        // reconcile re-fetch the authored entity list.
        self.scene_version += 1;
        self.publish_transition(PlayState::Edit);

        let twin = self
            .scene
            .find_entity_by_uuid(Uuid(selected_uuid))
            .unwrap_or(Entity::NULL);
        self.set_selection(twin);
        Ok(())
    }

    /// The camera to render through this frame.
    ///
    /// In Edit it is always the fly-camera. During play it cuts to the active scene's
    /// primary [`Camera`](saffron_scene::Camera), falling back to the fly-camera (never
    /// black) when the scene has none.
    pub fn render_camera_view(&mut self) -> CameraView {
        let fly = self.camera.view();
        if self.play_state == PlayState::Edit {
            return fly;
        }
        self.active_scene().primary_camera().unwrap_or(fly)
    }

    /// The simulation gate: returns the `dt` to step this frame, or `None` to skip.
    ///
    /// `None` in Edit, and while Paused with no stepped frame pending. Returns `Some(dt)` when
    /// Playing, or while consuming one of the granted stepped frames (at the fixed
    /// [`PLAY_FIXED_STEP`]). The returned `dt` clamps to [`PLAY_MAX_DELTA`] so a hitch never
    /// spikes the simulation. Advances `play_tick` on a stepping frame. The consumer (the host
    /// or the player) drives its own `RuntimeSession` with the returned `dt`, so SceneEdit stays
    /// free of script/physics deps — there is no simulation seam here.
    pub fn play_step_dt(&mut self, mut dt: f32) -> Option<f32> {
        if self.play_state == PlayState::Edit {
            return None;
        }
        let run = self.play_state == PlayState::Playing || self.step_frames > 0;
        if !run {
            return None;
        }
        if self.step_frames > 0 {
            self.step_frames -= 1;
            dt = PLAY_FIXED_STEP;
        }
        dt = dt.min(PLAY_MAX_DELTA);
        self.play_tick += 1;
        Some(dt)
    }

    /// Records one contained script failure in the bounded error ring.
    ///
    /// Stamps a monotonic `seq` and the current `play_tick`, dropping the oldest entry at
    /// [`SCRIPT_ERROR_RING_CAP`]. The seq survives `enter_play`'s clear so the drain cursor
    /// stays monotonic.
    pub fn push_script_error(&mut self, entity_uuid: u64, script: String, message: String) {
        self.script_error_seq += 1;
        if self.script_errors.len() >= SCRIPT_ERROR_RING_CAP {
            self.script_errors.remove(0);
        }
        self.script_errors.push(ScriptError {
            seq: self.script_error_seq,
            entity_uuid,
            script,
            message,
            tick: self.play_tick,
        });
    }

    /// Records one `sa.log(...)` line in the bounded log ring.
    ///
    /// Stamps a monotonic `seq`, a display-only wall-clock millisecond timestamp (never
    /// used for determinism), and the current `play_tick`, dropping the oldest entry at
    /// [`SCRIPT_LOG_RING_CAP`].
    pub fn push_script_log(&mut self, entity_uuid: u64, message: String) {
        self.script_log_seq += 1;
        if self.script_logs.len() >= SCRIPT_LOG_RING_CAP {
            self.script_logs.remove(0);
        }
        let epoch_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        self.script_logs.push(ScriptLog {
            seq: self.script_log_seq,
            entity_uuid,
            message,
            epoch_ms,
            tick: self.play_tick,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec3;
    use saffron_scene::{Camera, Transform};
    use std::cell::Cell;
    use std::rc::Rc;

    #[test]
    fn play_state_name_round_trips() {
        for state in [PlayState::Edit, PlayState::Playing, PlayState::Paused] {
            assert_eq!(PlayState::from_name(state.name()), state);
        }
        assert_eq!(PlayState::from_name("nonsense"), PlayState::Edit);
    }

    /// A fresh context with a `Cube` selected (translated to `(1,2,3)`), plus its uuid and
    /// the authored entity count — the shared fixture the play-mode tests build on.
    fn fixture() -> (SceneEditContext, Entity, u64, usize) {
        let mut ctx = SceneEditContext::new();
        let cube = ctx.scene.create_entity("Cube");
        ctx.scene
            .with_component_mut::<Transform, _>(cube, |t| t.translation = Vec3::new(1.0, 2.0, 3.0))
            .unwrap();
        let cube_uuid = ctx.scene.component::<IdComponent>(cube).unwrap().id.0;
        ctx.set_selection(cube);
        let edit_count = ctx.scene.len();
        (ctx, cube, cube_uuid, edit_count)
    }

    #[test]
    fn edit_state_rejects_pause_resume_step_and_stop_is_idempotent() {
        let (mut ctx, _cube, _uuid, _count) = fixture();
        assert!(ctx.pause_play().is_err(), "pause in edit rejects");
        assert!(ctx.resume_play().is_err(), "resume in edit rejects");
        assert!(ctx.step_play(1).is_err(), "step in edit rejects");
        assert!(
            ctx.stop_play().is_ok(),
            "stop in edit is an idempotent success"
        );
        assert_eq!(ctx.play_state, PlayState::Edit);
    }

    #[test]
    fn enter_play_lands_playing_bumps_version_and_re_enter_rejects() {
        let (mut ctx, _cube, _uuid, _count) = fixture();
        let version_before = ctx.play_version;
        assert!(ctx.enter_play().is_ok(), "enter_play from edit succeeds");
        assert_eq!(ctx.play_state, PlayState::Playing, "lands in playing");
        assert!(
            ctx.play_version > version_before,
            "enter_play bumps play_version"
        );
        assert!(
            ctx.enter_play().is_err(),
            "enter_play while playing rejects"
        );
    }

    #[test]
    fn the_seeded_camera_reads_as_primary_on_enter() {
        let (mut ctx, _cube, _uuid, _count) = fixture();
        ctx.enter_play().unwrap();
        assert!(ctx.had_primary_camera, "the seeded camera reads as primary");
    }

    /// Entering Play resets each player to a clean state: the editor preview's scrubbed
    /// `time` / `playing` never leak in — the play twin starts at `time = 0` and
    /// `playing = autoplay`.
    #[test]
    fn enter_play_resets_player_time_and_plays_only_when_autoplay() {
        // An entity whose player looks like a left-running editor preview (playing, scrubbed),
        // with autoplay OFF: the play twin must be reset to stopped at time 0.
        let mut ctx = SceneEditContext::new();
        let e = ctx.scene.create_entity("Anim");
        ctx.scene
            .add_component(
                e,
                AnimationPlayer {
                    playing: true,
                    time: 5.0,
                    autoplay: false,
                    preview_in_edit: true,
                    ..AnimationPlayer::default()
                },
            )
            .unwrap();
        let uuid = ctx.scene.component::<IdComponent>(e).unwrap().id.0;
        ctx.enter_play().unwrap();
        let play = ctx.play_scene.as_ref().expect("play scene");
        let twin = play.find_entity_by_uuid(Uuid(uuid)).expect("twin");
        let p = play.component::<AnimationPlayer>(twin).unwrap();
        assert_eq!(p.time, 0.0, "the scrubbed playhead resets to 0");
        assert!(
            !p.playing,
            "a preview-only player does not auto-play in Play"
        );
        assert!(!p.preview_in_edit, "preview flag is cleared for Play");

        // With autoplay ON, the same reset leaves the player advancing from time 0.
        let mut ctx2 = SceneEditContext::new();
        let e2 = ctx2.scene.create_entity("Anim2");
        ctx2.scene
            .add_component(
                e2,
                AnimationPlayer {
                    playing: false,
                    time: 9.0,
                    autoplay: true,
                    ..AnimationPlayer::default()
                },
            )
            .unwrap();
        let uuid2 = ctx2.scene.component::<IdComponent>(e2).unwrap().id.0;
        ctx2.enter_play().unwrap();
        let play2 = ctx2.play_scene.as_ref().expect("play scene");
        let twin2 = play2.find_entity_by_uuid(Uuid(uuid2)).expect("twin");
        let p2 = play2.component::<AnimationPlayer>(twin2).unwrap();
        assert_eq!(p2.time, 0.0, "play always starts at time 0");
        assert!(p2.playing, "autoplay starts the clip in Play");
    }

    #[test]
    fn duplicate_carries_entity_count_cube_uuid_and_transform() {
        let (mut ctx, _cube, cube_uuid, edit_count) = fixture();
        ctx.enter_play().unwrap();

        let play = ctx.play_scene.as_ref().expect("play scene present");
        assert_eq!(play.len(), edit_count, "duplicate carries the entity count");
        let play_cube = play
            .find_entity_by_uuid(Uuid(cube_uuid))
            .expect("cube uuid resolves in the duplicate");
        assert_eq!(
            play.component::<Transform>(play_cube).unwrap().translation,
            Vec3::new(1.0, 2.0, 3.0),
            "cube transform carried into play"
        );
        assert_eq!(
            ctx.selected_uuid_in(ctx.play_scene.as_ref().unwrap()),
            cube_uuid,
            "selection re-resolves to the play twin"
        );
    }

    #[test]
    fn active_scene_routes_to_the_duplicate_while_playing() {
        let (mut ctx, _cube, cube_uuid, _count) = fixture();
        ctx.enter_play().unwrap();
        // active_scene is the play duplicate: it resolves the cube uuid via the duplicate's
        // own world (the authored handle would alias).
        let resolves = ctx.active_scene().find_entity_by_uuid(Uuid(cube_uuid));
        assert!(resolves.is_some(), "active_scene routes to the duplicate");
    }

    #[test]
    fn render_camera_view_cuts_to_primary_then_falls_back_to_fly_cam() {
        let (mut ctx, _cube, _uuid, _count) = fixture();
        // A distinctive game-camera fov so the source is unambiguous: the fly-camera reports
        // ctx.camera.fov (45), the scene's primary camera reports 12.
        let mut edit_cam = Entity::NULL;
        ctx.scene.for_each::<&Camera, _>(|e, _| edit_cam = e);
        ctx.scene
            .with_component_mut::<Camera, _>(edit_cam, |c| c.fov = 12.0)
            .unwrap();

        assert_eq!(
            ctx.render_camera_view().fov,
            ctx.camera.fov,
            "edit renders through the fly-camera"
        );

        ctx.enter_play().unwrap();
        assert_eq!(
            ctx.render_camera_view().fov,
            12.0,
            "play cuts to the scene's primary camera"
        );

        // Demote the duplicate's camera: the view falls back to the fly-camera, never black.
        let mut play_cam = Entity::NULL;
        ctx.play_scene
            .as_mut()
            .unwrap()
            .for_each::<&Camera, _>(|e, _| play_cam = e);
        ctx.play_scene
            .as_mut()
            .unwrap()
            .with_component_mut::<Camera, _>(play_cam, |c| c.primary = false)
            .unwrap();
        assert_eq!(
            ctx.render_camera_view().fov,
            ctx.camera.fov,
            "play without a primary camera falls back to the fly-camera"
        );
    }

    #[test]
    fn pause_and_step_consume_stepped_frames_then_stay_inert() {
        let (mut ctx, _cube, _uuid, _count) = fixture();
        ctx.enter_play().unwrap();

        assert!(ctx.step_play(1).is_err(), "step while playing rejects");
        assert!(ctx.pause_play().is_ok(), "pause from playing succeeds");
        assert!(ctx.pause_play().is_err(), "pause while paused rejects");
        assert!(ctx.step_play(2).is_ok(), "step while paused succeeds");

        // The gate yields the fixed step for each granted stepped frame, then `None`.
        assert_eq!(ctx.play_step_dt(1.0), Some(PLAY_FIXED_STEP));
        assert_eq!(ctx.play_step_dt(1.0), Some(PLAY_FIXED_STEP));
        assert_eq!(
            ctx.step_frames, 0,
            "two gated steps consume the two stepped frames"
        );
        assert_eq!(
            ctx.play_step_dt(1.0),
            None,
            "a paused gate without steps stays inert"
        );
        assert_eq!(ctx.step_frames, 0);

        assert!(ctx.resume_play().is_ok(), "resume from paused succeeds");
        assert!(ctx.resume_play().is_err(), "resume while playing rejects");
    }

    #[test]
    fn play_step_dt_clamps_to_the_max_delta_while_playing() {
        let (mut ctx, _cube, _uuid, _count) = fixture();
        ctx.enter_play().unwrap();
        assert_eq!(
            ctx.play_step_dt(10.0),
            Some(PLAY_MAX_DELTA),
            "a hitch dt clamps to PLAY_MAX_DELTA"
        );
        assert_eq!(ctx.play_tick, 1, "a playing gate advances play_tick");
    }

    #[test]
    fn play_step_dt_is_none_in_edit() {
        let (mut ctx, _cube, _uuid, _count) = fixture();
        assert_eq!(ctx.play_step_dt(1.0), None, "the gate is None in Edit");
        assert_eq!(ctx.play_tick, 0);
    }

    #[test]
    fn stop_drops_duplicate_bumps_scene_version_and_authored_survives() {
        let (mut ctx, cube, _uuid, edit_count) = fixture();
        let authored = Vec3::new(1.0, 2.0, 3.0);
        ctx.enter_play().unwrap();

        // Mutate the duplicate and spawn a runtime entity — neither must touch the authored
        // scene.
        let play_cube = ctx
            .play_scene
            .as_ref()
            .unwrap()
            .find_entity_by_uuid(Uuid(_uuid))
            .unwrap();
        ctx.play_scene
            .as_mut()
            .unwrap()
            .with_component_mut::<Transform, _>(play_cube, |t| {
                t.translation = Vec3::new(9.0, 9.0, 9.0);
            })
            .unwrap();
        ctx.play_scene.as_mut().unwrap().create_entity("Runtime");

        // Walk Playing → Paused → Playing → Edit, exercising the stop path from playing.
        ctx.pause_play().unwrap();
        ctx.resume_play().unwrap();

        let scene_version_before = ctx.scene_version;
        assert!(ctx.stop_play().is_ok(), "stop from playing succeeds");
        assert_eq!(ctx.play_state, PlayState::Edit, "stop lands in edit");
        assert!(ctx.play_scene.is_none(), "stop drops the duplicate");
        assert!(
            ctx.scene_version > scene_version_before,
            "stop bumps scene_version"
        );
        assert_eq!(
            ctx.scene.component::<Transform>(cube).unwrap().translation,
            authored,
            "authored transform survives play edits untouched"
        );
        assert_eq!(
            ctx.scene.len(),
            edit_count,
            "a runtime-spawned entity does not survive stop"
        );
        assert_eq!(
            ctx.selected, cube,
            "selection restores to the authored entity by uuid"
        );
    }

    #[test]
    fn runtime_spawned_selection_clears_on_stop() {
        let (mut ctx, _cube, _uuid, _count) = fixture();
        ctx.enter_play().unwrap();
        // Select a runtime-spawned entity: it has no authored twin and must clear on stop.
        let runtime = ctx.play_scene.as_mut().unwrap().create_entity("Runtime");
        ctx.set_selection(runtime);
        assert!(ctx.stop_play().is_ok(), "a stop succeeds");
        assert_eq!(
            ctx.selected,
            Entity::NULL,
            "a runtime-spawned selection clears on stop"
        );
    }

    #[test]
    fn preview_routes_active_scene_while_play_state_stays_edit() {
        let (mut ctx, _cube, _uuid, _count) = fixture();
        assert!(!ctx.previewing(), "no preview by default");

        let mut preview = Scene::new();
        let preview_root = preview.create_entity("PreviewRoot");
        let preview_uuid = preview.component::<IdComponent>(preview_root).unwrap().id;
        ctx.preview_scene = Some(preview);
        ctx.preview_active_view = true;

        assert!(
            ctx.previewing(),
            "a set, active preview reads as previewing"
        );
        assert_eq!(ctx.play_state, PlayState::Edit, "preview stays in Edit");
        assert!(
            ctx.active_scene()
                .find_entity_by_uuid(preview_uuid)
                .is_some(),
            "active_scene routes to the preview"
        );

        ctx.preview_scene = None;
        ctx.preview_active_view = false;
        assert!(!ctx.previewing(), "clearing previewScene leaves preview");
    }

    #[test]
    fn script_log_ring_caps_with_monotonic_seq_cleared_on_enter_play() {
        let (mut ctx, _cube, cube_uuid, _count) = fixture();
        for _ in 0..(SCRIPT_LOG_RING_CAP + 8) {
            ctx.push_script_log(cube_uuid, "line".to_string());
        }
        assert_eq!(
            ctx.script_logs.len(),
            SCRIPT_LOG_RING_CAP,
            "script-log ring caps at SCRIPT_LOG_RING_CAP"
        );
        assert_eq!(
            ctx.script_log_seq as usize,
            SCRIPT_LOG_RING_CAP + 8,
            "script-log seq is monotonic"
        );

        let seq_before_replay = ctx.script_log_seq;
        ctx.enter_play().unwrap();
        assert!(
            ctx.script_logs.is_empty(),
            "enter_play clears the script-log ring"
        );
        assert_eq!(
            ctx.script_log_seq, seq_before_replay,
            "enter_play keeps the script-log seq monotonic"
        );
    }

    #[test]
    fn script_error_ring_caps_and_clears_keeping_seq() {
        let (mut ctx, _cube, cube_uuid, _count) = fixture();
        for _ in 0..(SCRIPT_ERROR_RING_CAP + 4) {
            ctx.push_script_error(cube_uuid, "s".to_string(), "boom".to_string());
        }
        assert_eq!(
            ctx.script_errors.len(),
            SCRIPT_ERROR_RING_CAP,
            "error ring caps"
        );
        assert_eq!(ctx.script_error_seq as usize, SCRIPT_ERROR_RING_CAP + 4);
        // The oldest was dropped: the front entry's seq is the (count - cap + 1)th stamped.
        assert_eq!(ctx.script_errors.first().unwrap().seq, 5);

        let seq_before = ctx.script_error_seq;
        ctx.enter_play().unwrap();
        assert!(
            ctx.script_errors.is_empty(),
            "enter_play clears the error ring"
        );
        assert_eq!(
            ctx.script_error_seq, seq_before,
            "the error seq stays monotonic"
        );
    }

    #[test]
    fn play_transition_publishes_the_signal() {
        let (mut ctx, _cube, _uuid, _count) = fixture();
        let states = Rc::new(Cell::new(Vec::<PlayState>::new()));
        {
            let states = Rc::clone(&states);
            ctx.on_play_state_changed.subscribe(move |s| {
                let mut v = states.take();
                v.push(s);
                states.set(v);
                false
            });
        }
        ctx.enter_play().unwrap();
        ctx.pause_play().unwrap();
        ctx.resume_play().unwrap();
        ctx.stop_play().unwrap();
        assert_eq!(
            states.take(),
            vec![
                PlayState::Playing,
                PlayState::Paused,
                PlayState::Playing,
                PlayState::Edit
            ],
            "every transition publishes its new state in order"
        );
    }
}
