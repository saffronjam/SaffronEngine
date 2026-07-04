//! The editor's mutable session state: the scene being edited, the component registry,
//! selection, the version stamps, the gizmo op/space source of truth, the overlay options,
//! the smoothing queues, play state, and the asset-preview block.
//!
//! This is the backend-neutral editor core. It depends only on `saffron-core`,
//! `saffron-signal`, `saffron-scene`, and `saffron-json` — no Rendering, no SDL: input
//! arrives as plain structs the host fills. The gizmo *geometry* (`build_native_gizmo`)
//! lives in the host; only the hit-test / projection / drag *math* lives here.
//!
//! Delivered so far: the container and its invariants — [`SceneEditContext`], the version
//! stamps, [`SceneEditContext::set_selection`], the
//! [`active_scene`](SceneEditContext::active_scene) / [`previewing`](SceneEditContext::previewing)
//! accessors, [`register_builtin_components`], the [`ScriptInputState`] +
//! [`derive_script_input_edges`], and the fly-camera math + serde
//! ([`SceneEditCamera::forward`] / [`view`](SceneEditCamera::view) /
//! [`to_json`](SceneEditCamera::to_json) / [`from_json`](SceneEditCamera::from_json),
//! [`update_scene_edit_camera`]), and the play state machine — the
//! [`enter_play`](SceneEditContext::enter_play) /
//! [`pause_play`](SceneEditContext::pause_play) /
//! [`resume_play`](SceneEditContext::resume_play) / [`step_play`](SceneEditContext::step_play)
//! / [`stop_play`](SceneEditContext::stop_play) transitions, the
//! [`play_step_dt`](SceneEditContext::play_step_dt) gate (the consumer steps its own
//! simulation with the returned `dt`),
//! [`render_camera_view`](SceneEditContext::render_camera_view), and the script error/log
//! rings — and the gizmo math: the projection / hit-test ([`viewport_project`],
//! [`pixel_to_ndc`], [`camera_position`], [`ring_basis`], [`gizmo_axes`], [`handle_axis`],
//! [`gizmo_plane_corners`], [`axis_color`], [`SceneEditContext::hit_native_gizmo`]), the
//! translate/rotate/scale drag with `preserve_children` rebasing
//! ([`SceneEditContext::snapshot_native_gizmo_start`] /
//! [`apply_native_gizmo_drag`](SceneEditContext::apply_native_gizmo_drag)), the `tau = 0.025`
//! pointer + edit smoothing ([`SceneEditContext::step_native_gizmo_drag`] /
//! [`step_edit_smoothing`](SceneEditContext::step_edit_smoothing) + the
//! transform smooth-entry/cancel helpers), and
//! [`SceneEditContext::sync_native_gizmo`].

#![deny(unsafe_code)]

mod camera;
mod context;
mod error;
mod gizmo;
mod overlay;
mod play;
mod project;
mod smoothing;

pub use camera::{SceneEditCamera, SceneEditCameraInput, update_scene_edit_camera};
pub use context::{AssetDragPayload, PlacementPreview, SceneEditContext};
pub use error::{Error, Result};
pub use gizmo::{
    GizmoOp, GizmoProjection, GizmoSpace, NativeGizmoHandle, NativeGizmoMode, NativeGizmoSpace,
    NativeGizmoState, axis_color, camera_position, gizmo_axes, gizmo_plane_corners, handle_axis,
    pixel_to_ndc, point_segment_distance, ring_basis, viewport_project,
};
pub use overlay::{
    DebugOverlayOptions, SkeletonOverlayOptions, debug_overlays_from_json, debug_overlays_to_json,
};
pub use play::{
    PLAY_FIXED_STEP, PLAY_MAX_DELTA, PlayState, SCRIPT_ERROR_RING_CAP, SCRIPT_LOG_RING_CAP,
    ScriptError, ScriptLog,
};
pub use project::{
    BootStage, NewProjectSpec, ProjectLoadProgress, ProjectLoadRequest, ProjectPhase,
};
pub use saffron_scene::{ScriptInputState, derive_script_input_edges};
pub use smoothing::TransformSmoothTarget;

/// Re-exported from `saffron-scene`: the single canonical registration site for every
/// built-in serialized component. [`SceneEditContext::new`] calls it to populate the
/// context's registry.
pub use saffron_scene::register_builtin_components;
