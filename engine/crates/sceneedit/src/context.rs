//! [`SceneEditContext`] — the editor's mutable session state core.
//!
//! An owned struct constructed with [`SceneEditContext::new`] and torn down by its automatic
//! `Drop`.

use glam::{Mat3, Quat, Vec3};

use saffron_core::Uuid;
use saffron_scene::{
    AssetType, Camera, ComponentRegistry, DirectionalLight, Entity, Scene, Transform,
    register_builtin_components,
};
use saffron_signal::SubscriberList;

use crate::camera::{SceneEditCamera, SceneEditCameraInput};
use crate::gizmo::{GizmoOp, GizmoSpace, NativeGizmoState};
use crate::overlay::{DebugOverlayOptions, SkeletonOverlayOptions};
use crate::play::{PlayState, ScriptError, ScriptLog};
use crate::project::{ProjectLoadProgress, ProjectLoadRequest, ProjectPhase};
use crate::smoothing::TransformSmoothTarget;
use saffron_scene::ScriptInputState;

/// The payload dragged from an asset tile onto a component picker field.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AssetDragPayload {
    /// The dragged asset id.
    pub id: u64,
    /// The dragged asset's type.
    pub asset_type: AssetType,
}

impl Default for AssetDragPayload {
    fn default() -> Self {
        Self {
            id: 0,
            asset_type: AssetType::Mesh,
        }
    }
}

/// A model placement preview: a [`PreviewGhost`](saffron_scene::PreviewGhost)-tagged subtree
/// instantiated once into the authored scene, then moved by a transform write on each drag-over.
/// Committing untags it; clearing destroys the subtree.
pub struct PlacementPreview {
    /// The model asset being previewed.
    pub asset: Uuid,
    /// The ghost subtree's root entity, living in the authored scene.
    pub root: Entity,
    /// The rest-pose world AABB of the ghost subtree, measured once at instantiate (before any
    /// placement transform). `None` until the mesh resolves (it may be uploading). Drives the
    /// drop-point offset so the model's bottom-center lands under the cursor.
    pub rest_bounds: Option<(Vec3, Vec3)>,
}

/// The editor's mutable state: the scene being edited, the component registry that drives
/// every panel, the selection (broadcast as a signal), the version stamps the control
/// plane diff-polls, the gizmo op/space source of truth, the overlay options, the
/// smoothing queues, play state, and the asset-preview block.
pub struct SceneEditContext {
    /// The authored scene.
    pub scene: Scene,
    /// The component reflection table that drives serde / add / remove.
    pub registry: ComponentRegistry,
    /// The current selection (`Entity::NULL` when nothing is selected).
    pub selected: Entity,
    /// Fires on every selection change, after `selection_version` is bumped.
    pub on_selection_changed: SubscriberList<Entity>,
    /// The current scene file path.
    pub scene_path: String,
    /// The project bring-up lifecycle phase — the single authoritative source of whether a
    /// project is loaded.
    pub project_phase: ProjectPhase,
    /// The per-frame load-progress snapshot the host loader writes and `project-status` reads.
    pub project_load: ProjectLoadProgress,
    /// A pending load request, set by a lifecycle command / bootstrap and taken by the loader.
    pub project_load_inbox: Option<ProjectLoadRequest>,
    /// Set by `cancel-load`; read + cleared by the loader to abort an in-flight load.
    pub project_cancel: bool,
    /// The project root directory.
    pub project_root: String,
    /// The project file path.
    pub project_path: String,
    /// The project's identifier name.
    pub project_name: String,
    /// The project's display name.
    pub project_display_name: String,
    /// The editor fly-camera (the scene-view eye).
    pub camera: SceneEditCamera,
    /// The transient model placement preview shown during asset DnD.
    pub placement_preview: Option<PlacementPreview>,

    /// Bumped by add/copy/destroy-entity + load (the control-plane diff poll key).
    pub scene_version: u64,
    /// Bumped on every selection change.
    pub selection_version: u64,

    /// The gizmo operation — the single source of truth (W/E/R cycles it).
    pub gizmo_op: GizmoOp,
    /// The gizmo reference space — the single source of truth (world/local).
    pub gizmo_space: GizmoSpace,
    /// Transform a parent without moving its children (their locals rebase to hold pose).
    pub preserve_children: bool,
    /// The overlay-gizmo hover/drag state (mode/space mirrored from the source above).
    pub native_gizmo: NativeGizmoState,
    /// The line-skeleton viewport overlay for the selected rig.
    pub skeleton_overlay: SkeletonOverlayOptions,
    /// The bounds / scene-AABB / light-volume viewport overlays.
    pub debug_overlays: DebugOverlayOptions,
    /// The asset-store enablement block (enabled connector ids + non-secret config),
    /// opaque JSON persisted in `project.json`; the editor owns its shape, credentials
    /// never live here.
    pub stores: serde_json::Value,
    /// Pending smoothed transform edits, one entry per entity.
    pub transform_smoothing: Vec<TransformSmoothTarget>,
    /// The latest fly-input command state; `look_delta` accumulates until the host drains
    /// it each frame.
    pub fly_input: SceneEditCameraInput,

    /// The play state (`Edit` / `Playing` / `Paused`).
    pub play_state: PlayState,
    /// The throwaway play duplicate; `None` in Edit.
    pub play_scene: Option<Scene>,
    /// Bumped on every play transition (reconcile-poll stamp).
    pub play_version: u64,
    /// Bumped by the animation commands (play/pause/seek/loop).
    pub animation_version: u64,
    /// Pending single-step ticks, granted only while Paused.
    pub step_frames: i32,
    /// Captured at `enter_play`; `false` drives the editor "no primary camera" warning.
    pub had_primary_camera: bool,
    /// The physics/scripting lifecycle seam.
    pub on_play_state_changed: SubscriberList<PlayState>,

    /// Ticks run since `enter_play` (error timestamps).
    pub play_tick: i64,
    /// Live script instances; set by the host wiring.
    pub script_instance_count: i32,
    /// The bounded script-error ring, oldest dropped at the cap.
    pub script_errors: Vec<ScriptError>,
    /// The last assigned `ScriptError.seq` (drain high-water).
    pub script_error_seq: i64,
    /// The bounded script-log ring, oldest dropped at the cap.
    pub script_logs: Vec<ScriptLog>,
    /// The last assigned `ScriptLog.seq` (drain high-water).
    pub script_log_seq: i64,
    /// Raw gameplay input for scripts (held keys + mouse); edges derived per tick.
    pub script_input: ScriptInputState,

    /// The isolated preview scene; `None` when not previewing.
    pub preview_scene: Option<Scene>,
    /// The model container being previewed (`0` = none).
    pub preview_asset: Uuid,
    /// The spawned model root entity in `preview_scene`.
    pub preview_root_entity: Entity,
    /// Node index → spawned joint entity uuid (empty for a static model).
    pub preview_bone_by_node: Vec<Uuid>,
    /// The authored-scene selection stashed across the preview.
    pub saved_selection: Entity,
    /// The fly-cam stashed on enter, restored on exit (byte-identity).
    pub saved_camera: SceneEditCamera,
    /// Overlay prefs stashed on enter (preview forces it on).
    pub saved_overlay: SkeletonOverlayOptions,
    /// The tonemap exposure (EV) stashed on enter, restored on exit — so an HDRI preview's EV
    /// sweep leaves the authored viewport's exposure untouched.
    pub saved_exposure: f32,
    /// The preview floor slab toggle.
    pub preview_show_floor: bool,
    /// The spawned floor slab in `preview_scene` (for the toggle).
    pub preview_floor_entity: Entity,
    /// `true` when the asset preview is the active view.
    pub preview_active_view: bool,
    /// The preview orbit fly-cam, parked while the scene view is active.
    pub parked_preview_camera: SceneEditCamera,
}

impl Default for SceneEditContext {
    fn default() -> Self {
        Self {
            scene: Scene::new(),
            registry: register_builtin_components(),
            selected: Entity::NULL,
            on_selection_changed: SubscriberList::new(),
            scene_path: String::new(),
            project_phase: ProjectPhase::default(),
            project_load: ProjectLoadProgress::default(),
            project_load_inbox: None,
            project_cancel: false,
            project_root: String::new(),
            project_path: String::new(),
            project_name: String::new(),
            project_display_name: String::new(),
            camera: SceneEditCamera::default(),
            placement_preview: None,
            scene_version: 0,
            selection_version: 0,
            gizmo_op: GizmoOp::default(),
            gizmo_space: GizmoSpace::default(),
            preserve_children: false,
            native_gizmo: NativeGizmoState::default(),
            skeleton_overlay: SkeletonOverlayOptions::default(),
            debug_overlays: DebugOverlayOptions::default(),
            stores: serde_json::Value::Null,
            transform_smoothing: Vec::new(),
            fly_input: SceneEditCameraInput::default(),
            play_state: PlayState::default(),
            play_scene: None,
            play_version: 0,
            animation_version: 0,
            step_frames: 0,
            had_primary_camera: false,
            on_play_state_changed: SubscriberList::new(),
            play_tick: 0,
            script_instance_count: 0,
            script_errors: Vec::new(),
            script_error_seq: 0,
            script_logs: Vec::new(),
            script_log_seq: 0,
            script_input: ScriptInputState::default(),
            preview_scene: None,
            preview_asset: Uuid(0),
            preview_root_entity: Entity::NULL,
            preview_bone_by_node: Vec::new(),
            saved_selection: Entity::NULL,
            saved_camera: SceneEditCamera::default(),
            saved_overlay: SkeletonOverlayOptions::default(),
            saved_exposure: 0.0,
            preview_show_floor: true,
            preview_floor_entity: Entity::NULL,
            preview_active_view: false,
            parked_preview_camera: SceneEditCamera::default(),
        }
    }
}

impl SceneEditContext {
    /// Constructs a seeded context: the registry is populated, a `Camera` looking at the
    /// origin and a `Sun` directional light are spawned, and the camera is selected.
    #[must_use]
    pub fn new() -> Self {
        let mut ctx = Self::default();

        // Seed a camera looking at the origin so a freshly spawned mesh is visible.
        let camera = ctx.scene.create_entity("Camera");
        let _ = ctx.scene.add_component(camera, Camera::default());
        let translation = Vec3::new(3.0, 2.5, 4.0);
        let rotation = euler_angles(quat_look_at(-translation.normalize(), Vec3::Y));
        let _ = ctx.scene.with_component_mut::<Transform, _>(camera, |t| {
            t.translation = translation;
            t.rotation = rotation;
        });

        let sun = ctx.scene.create_entity("Sun");
        let _ = ctx.scene.add_component(sun, DirectionalLight::default());

        ctx.set_selection(camera);
        ctx
    }

    /// True only when a project is fully open and resident. The guard the per-handler
    /// `require_project_loaded` and inline project checks read.
    #[must_use]
    pub fn project_ready(&self) -> bool {
        self.project_phase == ProjectPhase::Ready
    }

    /// The scene every consumer addresses: the asset preview while it is the active view,
    /// the play duplicate while playing/paused, the authored scene in Edit.
    ///
    /// Preview takes precedence (it is entered only from Edit). This is the single place
    /// that branches to pick a scene — nothing else may branch on `play_state` /
    /// `preview_scene`.
    pub fn active_scene(&mut self) -> &mut Scene {
        if self.preview_active_view
            && let Some(preview) = self.preview_scene.as_mut()
        {
            return preview;
        }
        if self.play_state == PlayState::Edit {
            return &mut self.scene;
        }
        self.play_scene.as_mut().expect("play scene present")
    }

    /// The play scene and the gameplay [`ScriptInputState`], borrowed disjointly for the
    /// runtime's simulation step.
    ///
    /// Only valid while playing (the consumer calls it under a `Some(dt)` from
    /// [`play_step_dt`](Self::play_step_dt), where the active scene is the play duplicate and
    /// preview is impossible). [`active_scene`](Self::active_scene) borrows all of `self`, so the
    /// host could not pass both the play scene and `&mut self.script_input` to the runtime in one
    /// call; this splits the two field borrows (distinct fields) in one place.
    pub fn play_scene_and_input(&mut self) -> (&mut Scene, &mut ScriptInputState) {
        (
            self.play_scene.as_mut().expect("play scene present"),
            &mut self.script_input,
        )
    }

    /// The component registry paired with the active scene, borrowed disjointly.
    ///
    /// [`Self::active_scene`] borrows all of `self`, so a registry method that also takes the
    /// active scene (`component_order`, `set_component_order`, `append_component_order`, …)
    /// cannot be called as `self.registry.method(self.active_scene(), …)`. This splits the
    /// two field borrows in one place: `registry` is a distinct field from the scene fields,
    /// so the compiler accepts the disjoint pair. The scene routing mirrors
    /// [`Self::active_scene`] exactly (preview view first, then play duplicate, then
    /// authored).
    pub fn registry_and_active_scene(&mut self) -> (&ComponentRegistry, &mut Scene) {
        let registry = &self.registry;
        if self.preview_active_view
            && let Some(preview) = self.preview_scene.as_mut()
        {
            return (registry, preview);
        }
        let scene = if self.play_state == PlayState::Edit {
            &mut self.scene
        } else {
            self.play_scene.as_mut().expect("play scene present")
        };
        (registry, scene)
    }

    /// `true` while the asset preview is the ACTIVE view.
    ///
    /// Commands that mutate the authored scene or project must refuse while this holds —
    /// [`Self::active_scene`] routes to the preview. With the scene view active this reads
    /// `false` even if a preview scene is kept alive in the background.
    #[must_use]
    pub fn previewing(&self) -> bool {
        self.preview_scene.is_some() && self.preview_active_view
    }

    /// Whether the editor's in-viewport chrome — the gizmo, entity billboards, camera
    /// frustums, and debug overlays — should render this frame.
    ///
    /// True only in Edit mode with the authored scene as the active view. It stays hidden in
    /// Play and while the asset preview owns the viewport — including a *parked* preview whose
    /// scene is still alive but not the active view (`preview_scene.is_some()` yet
    /// `!preview_active_view`), where the authored scene renders again and the chrome must come
    /// back. Keyed off [`previewing`](Self::previewing) so it can never drift from the grid
    /// gate the way a bare `preview_scene.is_none()` check does.
    #[must_use]
    pub fn editor_chrome_visible(&self) -> bool {
        self.play_state == PlayState::Edit && !self.previewing()
    }

    /// Sets the selection, bumps `selection_version`, and publishes the change.
    pub fn set_selection(&mut self, entity: Entity) {
        self.selected = entity;
        self.selection_version += 1;
        self.on_selection_changed.publish(entity);
    }
}

/// A quaternion looking in `direction` with the given `up`, in the right-handed
/// convention where the camera's forward maps to `-Z`.
fn quat_look_at(direction: Vec3, up: Vec3) -> Quat {
    let z = -direction;
    let x = up.cross(z).normalize();
    let y = z.cross(x);
    Quat::from_mat3(&Mat3::from_cols(x, y, z))
}

/// The Euler-XYZ angles of `q`.
///
/// This is the inverse of [`saffron_scene::quat_from_euler_xyz`] up to the gimbal pole, so
/// feeding the result back through the engine's `Transform` rotation rebuilds `q`.
fn euler_angles(q: Quat) -> Vec3 {
    let (x, y, z, w) = (q.x, q.y, q.z, q.w);
    let pitch = (2.0 * (y * z + w * x)).atan2(w * w - x * x - y * y + z * z);
    let yaw = f32::asin((-2.0 * (x * z - w * y)).clamp(-1.0, 1.0));
    let roll = (2.0 * (x * y + w * z)).atan2(w * w + x * x - y * y - z * z);
    Vec3::new(pitch, yaw, roll)
}

#[cfg(test)]
mod tests {
    use super::*;
    use saffron_scene::{IdComponent, quat_from_euler_xyz};
    use std::cell::Cell;
    use std::rc::Rc;

    #[test]
    fn new_seeds_camera_and_sun_and_selects_camera() {
        let mut ctx = SceneEditContext::new();

        // Exactly two entities: the Camera and the Sun.
        assert_eq!(ctx.scene.len(), 2);

        // Resolve the seeded entities by component presence.
        let mut cameras = Vec::new();
        let mut suns = Vec::new();
        ctx.scene.for_each::<&Camera, _>(|e, _| cameras.push(e));
        ctx.scene
            .for_each::<&DirectionalLight, _>(|e, _| suns.push(e));
        assert_eq!(cameras.len(), 1, "exactly one seeded camera");
        assert_eq!(suns.len(), 1, "exactly one seeded sun");

        let camera = cameras[0];
        let sun = suns[0];

        // The camera is selected, and the selection bump fired.
        assert_eq!(ctx.selected, camera);
        assert_eq!(ctx.selection_version, 1);

        // The camera sits at the authored eye and looks back toward the origin.
        let transform = ctx.scene.component::<Transform>(camera).unwrap();
        assert_eq!(transform.translation, Vec3::new(3.0, 2.5, 4.0));
        // The seeded rotation, fed back through the engine's Euler convention, points the
        // camera's forward (-Z) at the origin.
        let forward = (quat_from_euler_xyz(transform.rotation) * Vec3::NEG_Z).normalize();
        let to_origin = (-transform.translation).normalize();
        assert!(
            forward.distance(to_origin) < 1e-4,
            "camera forward {forward:?} should aim at the origin {to_origin:?}"
        );

        // The sun carries a directional light and is a distinct entity.
        assert!(ctx.scene.has_component::<DirectionalLight>(sun));
        assert_ne!(camera, sun);
    }

    #[test]
    fn new_populates_the_component_registry() {
        let ctx = SceneEditContext::new();

        // Every built-in serialized component resolves a row in the seeded registry.
        for &name in saffron_scene::BUILTIN_COMPONENT_NAMES {
            assert!(
                ctx.registry.find_by_name(name).is_some(),
                "component '{name}' should be registered"
            );
        }
        assert_eq!(
            ctx.registry.rows().len(),
            saffron_scene::BUILTIN_COMPONENT_NAMES.len(),
            "the seeded registry holds exactly the built-in set"
        );
        // A spot-check by name.
        assert_eq!(
            ctx.registry.find_by_name("Transform").map(|t| t.name),
            Some("Transform")
        );
    }

    #[test]
    fn set_selection_bumps_version_and_publishes() {
        let mut ctx = SceneEditContext::new();
        let baseline = ctx.selection_version;

        let seen = Rc::new(Cell::new(Entity::NULL));
        {
            let seen = Rc::clone(&seen);
            ctx.on_selection_changed.subscribe(move |e| {
                seen.set(e);
                false
            });
        }

        let other = ctx.scene.create_entity("Other");
        ctx.set_selection(other);

        assert_eq!(ctx.selected, other);
        assert_eq!(ctx.selection_version, baseline + 1);
        assert_eq!(
            seen.get(),
            other,
            "the subscriber received the new selection"
        );
    }

    #[test]
    fn active_scene_routes_edit_play_and_preview() {
        let mut ctx = SceneEditContext::new();

        // Edit: the authored scene. Tag it so we can identify which scene is returned.
        let authored_tag = ctx.scene.create_entity("authored-tag");
        let authored_uuid = ctx.scene.component::<IdComponent>(authored_tag).unwrap().id;
        assert!(
            ctx.active_scene()
                .find_entity_by_uuid(authored_uuid)
                .is_some(),
            "Edit routes to the authored scene"
        );
        assert!(!ctx.previewing());

        // Playing with a play duplicate present: routes to the duplicate.
        let mut play = Scene::new();
        let play_tag = play.create_entity("play-tag");
        let play_uuid = play.component::<IdComponent>(play_tag).unwrap().id;
        ctx.play_scene = Some(play);
        ctx.play_state = PlayState::Playing;
        assert!(
            ctx.active_scene().find_entity_by_uuid(play_uuid).is_some(),
            "Playing routes to the play duplicate"
        );
        assert!(
            ctx.active_scene()
                .find_entity_by_uuid(authored_uuid)
                .is_none(),
            "the authored scene is not the active one while playing"
        );
        assert!(!ctx.previewing(), "playing is not previewing");

        // A live preview scene that is NOT the active view does not route (the authored
        // scene is fully editable while the scene view is active).
        let mut preview = Scene::new();
        let preview_tag = preview.create_entity("preview-tag");
        let preview_uuid = preview.component::<IdComponent>(preview_tag).unwrap().id;
        ctx.preview_scene = Some(preview);
        ctx.preview_active_view = false;
        assert!(
            !ctx.previewing(),
            "preview kept alive but not the active view"
        );
        assert!(
            ctx.active_scene().find_entity_by_uuid(play_uuid).is_some(),
            "still routes to the play duplicate while preview is inactive"
        );

        // Preview as the active view takes precedence over everything.
        ctx.preview_active_view = true;
        assert!(ctx.previewing());
        assert!(
            ctx.active_scene()
                .find_entity_by_uuid(preview_uuid)
                .is_some(),
            "the active preview view takes precedence"
        );
    }

    #[test]
    fn editor_chrome_hidden_only_while_a_preview_owns_the_view() {
        let mut ctx = SceneEditContext::new();

        // Edit, no preview: the authored scene is live, so the gizmo/billboards/frustums show.
        ctx.play_state = PlayState::Edit;
        assert!(ctx.editor_chrome_visible());

        // Play hides the chrome regardless of the preview state.
        ctx.play_state = PlayState::Playing;
        assert!(!ctx.editor_chrome_visible());

        // A preview scene kept alive but PARKED (not the active view) still renders the
        // authored scene, so back in Edit the chrome must return — the regression: a bare
        // `preview_scene.is_none()` gate wrongly hid the gizmo here while the grid stayed up.
        ctx.play_state = PlayState::Edit;
        ctx.preview_scene = Some(Scene::new());
        ctx.preview_active_view = false;
        assert!(!ctx.previewing());
        assert!(
            ctx.editor_chrome_visible(),
            "a parked preview must not hide the authored scene's chrome"
        );

        // The preview as the active view (the orbit) hides the authored chrome.
        ctx.preview_active_view = true;
        assert!(ctx.previewing());
        assert!(!ctx.editor_chrome_visible());
    }
}
