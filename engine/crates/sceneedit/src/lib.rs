//! The editor's mutable session state: the scene being edited, the component registry,
//! selection, the version stamps, the gizmo op/space source of truth, the overlay options,
//! the smoothing queues, play state, and the asset-preview block.
//!
//! The backend-neutral editor core: no rendering and no windowing, so input arrives as plain
//! structs the host fills. The gizmo *geometry* lives in the host; only its hit-test,
//! projection, and drag *math* lives here.

#![deny(unsafe_code)]

mod camera;
mod context;
mod error;
mod gizmo;
mod overlay;
mod play;
mod project;
mod smoothing;

pub use camera::{OrbitState, SceneEditCamera, SceneEditCameraInput, update_scene_edit_camera};
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
