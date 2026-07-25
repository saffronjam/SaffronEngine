//! The viewport overlay options: the per-rig line skeleton and the world-space debug
//! overlays, plus the debug-overlay project.json serde.
//!
//! [`SkeletonOverlayOptions`] is session-only; [`DebugOverlayOptions`] is saved into
//! `project.json` by the save/load caller via [`debug_overlays_to_json`] /
//! [`debug_overlays_from_json`], so a reopened project restores the toggles.

use serde_json::{Value, json};

use saffron_json::json_bool_or;

/// The line-skeleton viewport overlay for the selected rig: bone segments + joint dots,
/// with optional per-joint RGB axes. Opt-in; drawn in Edit and Play.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SkeletonOverlayOptions {
    /// The master toggle (`set-skeleton-overlay`).
    pub show: bool,
    /// Per-joint RGB axis lines.
    pub axes: bool,
    /// Joint-dot radius in pixels (screen-constant at any zoom).
    pub joint_size: f32,
    /// The `get-asset-model` node index of the tinted joint while previewing; `-1` is none.
    pub highlight_joint: i32,
}

impl Default for SkeletonOverlayOptions {
    fn default() -> Self {
        Self {
            show: false,
            axes: false,
            joint_size: 4.0,
            highlight_joint: -1,
        }
    }
}

/// The viewport debug overlays (`set-debug-overlays`), drawn as world-space lines in the
/// editor overlay pass. Opt-in, Edit-only, saved into `project.json`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DebugOverlayOptions {
    /// Per-entity world AABB (the pick volume for static meshes).
    pub bounds: bool,
    /// The whole-scene AABB the shadow/DDGI fit uses.
    pub scene_aabb: bool,
    /// Point-light range spheres + spot cones.
    pub light_volumes: bool,
    /// The infinite analytic ground grid (a render-graph pass).
    pub grid: bool,
    /// Physics collision shapes (box/sphere/capsule wireframes); Edit + Play.
    pub colliders: bool,
    /// Resident vegetation runtime cells as wireframe boxes.
    pub vegetation_cells: bool,
    /// Per-plant conservative world bounds, colored by lifecycle.
    pub vegetation_bounds: bool,
    /// Rejected-candidate markers colored by rejection reason.
    pub vegetation_rejections: bool,
    /// Micro-density heatmap texels over the resident cells.
    pub vegetation_heatmap: bool,
    /// Published navigation contributions (obstacle/cost footprints) and dirty rebuild regions.
    pub vegetation_navigation: bool,
    /// Sampled wind-field vectors on a camera-centred ground grid.
    pub wind_vectors: bool,
}

/// Serializes the debug overlays to their `project.json` object. The keys are the frozen
/// wire spellings.
#[must_use]
pub fn debug_overlays_to_json(opts: &DebugOverlayOptions) -> Value {
    json!({
        "bounds": opts.bounds,
        "sceneAabb": opts.scene_aabb,
        "lightVolumes": opts.light_volumes,
        "grid": opts.grid,
        "colliders": opts.colliders,
        "vegetationCells": opts.vegetation_cells,
        "vegetationBounds": opts.vegetation_bounds,
        "vegetationRejections": opts.vegetation_rejections,
        "vegetationHeatmap": opts.vegetation_heatmap,
        "vegetationNavigation": opts.vegetation_navigation,
        "windVectors": opts.wind_vectors,
    })
}

/// Reads the debug overlays from a `project.json` object.
///
/// A non-object value leaves `opts` untouched; each missing or mistyped field keeps its
/// current value (the load is additive over the defaults the caller passes in).
pub fn debug_overlays_from_json(opts: &mut DebugOverlayOptions, value: &Value) {
    if !value.is_object() {
        return;
    }
    opts.bounds = json_bool_or(value, "bounds", opts.bounds);
    opts.scene_aabb = json_bool_or(value, "sceneAabb", opts.scene_aabb);
    opts.light_volumes = json_bool_or(value, "lightVolumes", opts.light_volumes);
    opts.grid = json_bool_or(value, "grid", opts.grid);
    opts.colliders = json_bool_or(value, "colliders", opts.colliders);
    opts.vegetation_cells = json_bool_or(value, "vegetationCells", opts.vegetation_cells);
    opts.vegetation_bounds = json_bool_or(value, "vegetationBounds", opts.vegetation_bounds);
    opts.vegetation_rejections =
        json_bool_or(value, "vegetationRejections", opts.vegetation_rejections);
    opts.vegetation_heatmap = json_bool_or(value, "vegetationHeatmap", opts.vegetation_heatmap);
    opts.vegetation_navigation =
        json_bool_or(value, "vegetationNavigation", opts.vegetation_navigation);
    opts.wind_vectors = json_bool_or(value, "windVectors", opts.wind_vectors);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_overlays_round_trip_through_json() {
        let opts = DebugOverlayOptions {
            bounds: true,
            scene_aabb: false,
            light_volumes: true,
            grid: true,
            colliders: false,
            vegetation_cells: true,
            vegetation_bounds: false,
            vegetation_rejections: true,
            vegetation_heatmap: false,
            vegetation_navigation: true,
            wind_vectors: false,
        };

        let value = debug_overlays_to_json(&opts);
        // The wire spellings are the frozen camelCase keys.
        assert_eq!(value["bounds"], json!(true));
        assert_eq!(value["sceneAabb"], json!(false));
        assert_eq!(value["lightVolumes"], json!(true));
        assert_eq!(value["grid"], json!(true));
        assert_eq!(value["colliders"], json!(false));
        assert_eq!(value["vegetationCells"], json!(true));
        assert_eq!(value["vegetationBounds"], json!(false));
        assert_eq!(value["vegetationRejections"], json!(true));
        assert_eq!(value["vegetationHeatmap"], json!(false));
        assert_eq!(value["windVectors"], json!(false));

        let mut read = DebugOverlayOptions::default();
        debug_overlays_from_json(&mut read, &value);
        assert_eq!(read, opts);
    }

    #[test]
    fn from_json_ignores_non_object_and_keeps_missing_fields() {
        let mut opts = DebugOverlayOptions {
            grid: true,
            ..DebugOverlayOptions::default()
        };

        // A non-object leaves everything as-is.
        debug_overlays_from_json(&mut opts, &json!(42));
        assert!(opts.grid);

        // A partial object only touches the present keys; `grid` is untouched.
        debug_overlays_from_json(&mut opts, &json!({ "bounds": true }));
        assert!(opts.bounds);
        assert!(opts.grid, "missing field keeps its current value");
    }
}
