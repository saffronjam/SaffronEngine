//! The editor fly-camera: its data, the backend-neutral per-frame fly input, the
//! yaw/pitch → forward/view math, the per-frame `update_scene_edit_camera` (the eye eases
//! toward a target pose + WASD/Space/Shift move), and the serde the control caller
//! round-trips into `project.json`.
//!
//! The camera eases toward a target each rendered frame with the shared `SMOOTH_TAU`, so the
//! ~60 Hz control samples become continuous motion at render FPS. It has two modes:
//!
//! - **Free fly** (the scene view): the eye carries a target pose
//!   (`target_position`/`target_yaw`/`target_pitch`) it eases toward; WASD moves the eye and
//!   its target together (instant translation); an absolute `set-camera` snaps both.
//! - **Orbit** (the preview panes): `orbit` is `Some` and the eye is *derived* on the arc as
//!   `pivot - forward(yaw,pitch) · distance`. The pivot, distance, and angles ease toward their
//!   targets, so a fast drag sweeps the circle at a fixed radius instead of the eye lerping
//!   straight across it (a chord). Preview enter frames into orbit mode; exit restores the
//!   stashed free camera.
//!
//! These are the scene-view eye, distinct from any ECS `Camera` / game camera. SceneEdit
//! stays SDL-free — `look_delta` and the move bools arrive as plain data the host fills.

use glam::{Mat4, Vec2, Vec3};
use saffron_json::json_f32_or;
use saffron_scene::CameraView;
use serde_json::{Map, Value};

use crate::smoothing::SMOOTH_TAU;

/// The orbit smoothing state: a pivot the eye circles at `distance`, each eased toward its
/// target so the eye sweeps the arc rather than lerping across it. `Some` on
/// [`SceneEditCamera`] while a preview pane orbits; the eye is derived from it plus the
/// camera's (also eased) yaw/pitch.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OrbitState {
    /// The point the eye orbits (world space).
    pub pivot: Vec3,
    /// The eye's distance from the pivot.
    pub distance: f32,
    /// The pivot the orbit eases toward.
    pub target_pivot: Vec3,
    /// The distance the orbit eases toward.
    pub target_distance: f32,
}

/// The viewport's own fly-camera.
///
/// Hold RMB over the viewport to look + WASD to move, Space up / Shift down. `yaw`/`pitch`
/// are degrees; at yaw 0 the camera looks down `-Z`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SceneEditCamera {
    /// The eye position (world space).
    pub position: Vec3,
    /// The yaw angle, degrees.
    pub yaw: f32,
    /// The pitch angle, degrees.
    pub pitch: f32,
    /// Vertical field of view, degrees.
    pub fov: f32,
    /// Near clip plane.
    pub near_plane: f32,
    /// Far clip plane.
    pub far_plane: f32,
    /// Move speed, units per second.
    pub move_speed: f32,
    /// Look speed, degrees per pixel.
    pub look_speed: f32,
    /// The eye position the camera eases toward (world space).
    pub target_position: Vec3,
    /// The yaw angle the camera eases toward, degrees.
    pub target_yaw: f32,
    /// The pitch angle the camera eases toward, degrees.
    pub target_pitch: f32,
    /// `Some` while orbiting (a preview pane): the eye is derived on the arc from this pivot +
    /// distance and the eased yaw/pitch, rather than eased in eye space.
    pub orbit: Option<OrbitState>,
    /// Latched while RMB is held (so a drag can leave the rect).
    pub controlling: bool,
}

impl Default for SceneEditCamera {
    fn default() -> Self {
        Self {
            position: Vec3::new(3.0, 2.5, 4.0),
            yaw: -37.0,
            pitch: -29.0,
            fov: 45.0,
            near_plane: 0.1,
            far_plane: 100.0,
            move_speed: 6.0,
            look_speed: 0.12,
            target_position: Vec3::new(3.0, 2.5, 4.0),
            target_yaw: -37.0,
            target_pitch: -29.0,
            orbit: None,
            controlling: false,
        }
    }
}

/// Backend-neutral per-frame fly-cam input the host fills, so SceneEdit stays SDL-free.
///
/// `look_delta` is summed relative-mouse motion (pixels) for the frame; the bools are the
/// move-key state during the RMB hold.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SceneEditCameraInput {
    /// RMB-fly engaged (keyboard grabbed).
    pub active: bool,
    /// Summed relative-mouse motion this frame (pixels).
    pub look_delta: Vec2,
    /// Forward (W).
    pub forward: bool,
    /// Back (S).
    pub back: bool,
    /// Left (A).
    pub left: bool,
    /// Right (D).
    pub right: bool,
    /// Up (Space).
    pub up: bool,
    /// Down (LShift).
    pub down: bool,
}

impl SceneEditCamera {
    /// The camera's forward (world space) from its yaw/pitch.
    ///
    /// Spherical from the degree angles: at yaw 0 / pitch 0 it looks down `-Z`
    /// (`cos(pitch)·sin(yaw), sin(pitch), -cos(pitch)·cos(yaw)`).
    #[must_use]
    pub fn forward(&self) -> Vec3 {
        let yaw = self.yaw.to_radians();
        let pitch = self.pitch.to_radians();
        Vec3::new(
            pitch.cos() * yaw.sin(),
            pitch.sin(),
            -pitch.cos() * yaw.cos(),
        )
        .normalize()
    }

    /// The camera as a scene [`CameraView`] (view + projection params), so `render_scene`
    /// and the gizmo draw from the same eye.
    ///
    /// The view is `look_at_rh(position, position + forward, +Y)`.
    #[must_use]
    pub fn view(&self) -> CameraView {
        CameraView {
            view: Mat4::look_at_rh(self.position, self.position + self.forward(), Vec3::Y),
            fov: self.fov,
            near_plane: self.near_plane,
            far_plane: self.far_plane,
        }
    }

    /// Snaps the targets onto the current pose (both the free-eye pose and, if orbiting, the
    /// pivot + distance), so an absolute set (framing, focus, project load) shows instantly
    /// without easing back toward a stale target.
    pub fn sync_target(&mut self) {
        self.target_position = self.position;
        self.target_yaw = self.yaw;
        self.target_pitch = self.pitch;
        if let Some(orbit) = self.orbit.as_mut() {
            orbit.target_pivot = orbit.pivot;
            orbit.target_distance = orbit.distance;
        }
    }

    /// Whether the camera has yet to reach its target — the render-activity signal that holds
    /// continuous render while the ease (fly look tail or preview orbit sweep) is in flight.
    /// The ease snaps exactly on convergence, so a settled camera reports `false`.
    #[must_use]
    pub fn is_easing(&self) -> bool {
        if self.yaw != self.target_yaw || self.pitch != self.target_pitch {
            return true;
        }
        match self.orbit {
            Some(orbit) => {
                orbit.pivot != orbit.target_pivot || orbit.distance != orbit.target_distance
            }
            None => self.position != self.target_position,
        }
    }

    /// The persisted editor view, the key set
    /// `{ position: {x,y,z}, yaw, pitch, fov }`. Saved into `project.json` by the control
    /// save/load caller so a reopened project shows the same framing.
    #[must_use]
    pub fn to_json(&self) -> Value {
        Value::Object(Map::from_iter([
            ("position".to_string(), vec3_to_json(self.position)),
            ("yaw".to_string(), f32_value(self.yaw)),
            ("pitch".to_string(), f32_value(self.pitch)),
            ("fov".to_string(), f32_value(self.fov)),
        ]))
    }

    /// Reads the persisted view back. A missing field
    /// keeps the current value, so a partial document does not zero the camera; a non-object
    /// is ignored entirely.
    pub fn from_json(&mut self, j: &Value) {
        if !j.is_object() {
            return;
        }
        if let Some(position) = j.get("position") {
            self.position = vec3_from_json(position);
        }
        self.yaw = json_f32_or(j, "yaw", self.yaw);
        self.pitch = json_f32_or(j, "pitch", self.pitch);
        self.fov = json_f32_or(j, "fov", self.fov);
        // A persisted view is the free fly camera; a loaded view shows at once — leave orbit
        // mode and snap the target so the eye does not ease from the pre-load pose.
        self.orbit = None;
        self.sync_target();
    }
}

/// Advances the editor camera one frame from host-gathered input, easing toward its target.
///
/// Look deltas accumulate into `target_yaw`/`target_pitch` and the angles ease toward them
/// each frame (`alpha = 1 - exp(-dt/TAU)`, the same constant as the gizmo drag), so the ~60 Hz
/// look samples do not staircase; the ease runs even when inactive, so the tail eases out.
/// Target pitch clamps to ±89°.
///
/// In **orbit mode** (`camera.orbit`, the preview panes) the pivot + distance ease too and the
/// eye is derived on the arc as `pivot - forward · distance`, so a fast drag sweeps the circle
/// at the eased radius instead of the eye cutting a chord across it. In **free-fly mode**
/// `controlling` latches while `active` holds (so a drag can leave the viewport rect), WASD
/// moves along the camera basis and Space/Shift along world `±Y` scaled by `move_speed · dt`
/// — translating the eye and its target together so fly movement stays instant — then the eye
/// eases toward `target_position`.
pub fn update_scene_edit_camera(
    camera: &mut SceneEditCamera,
    input: &SceneEditCameraInput,
    dt: f32,
) {
    camera.target_yaw += input.look_delta.x * camera.look_speed;
    camera.target_pitch -= input.look_delta.y * camera.look_speed;
    camera.target_pitch = camera.target_pitch.clamp(-89.0, 89.0);

    let alpha = 1.0 - (-dt.max(0.0) / SMOOTH_TAU).exp();
    ease_scalar(&mut camera.yaw, camera.target_yaw, alpha);
    ease_scalar(&mut camera.pitch, camera.target_pitch, alpha);
    let forward = camera.forward();

    if let Some(mut orbit) = camera.orbit {
        // Orbit: ease the pivot + radius, then place the eye on the arc looking at the pivot —
        // the sweep stays on the circle, never chording through it.
        ease_vec3(&mut orbit.pivot, orbit.target_pivot, alpha);
        ease_scalar(&mut orbit.distance, orbit.target_distance, alpha);
        camera.orbit = Some(orbit);
        camera.controlling = false;
        camera.position = orbit.pivot - forward * orbit.distance;
        return;
    }

    if input.active {
        camera.controlling = true;
        let right = forward.cross(Vec3::Y).normalize();
        let world_up = Vec3::Y;
        let mut mv = Vec3::ZERO;
        if input.forward {
            mv += forward;
        }
        if input.back {
            mv -= forward;
        }
        if input.right {
            mv += right;
        }
        if input.left {
            mv -= right;
        }
        if input.up {
            mv += world_up;
        }
        if input.down {
            mv -= world_up;
        }
        let delta = mv * (camera.move_speed * dt);
        camera.position += delta;
        camera.target_position += delta;
    } else {
        camera.controlling = false;
    }

    ease_vec3(&mut camera.position, camera.target_position, alpha);
}

/// The convergence epsilon: the eased pose snaps exactly onto its target once every field is
/// within this, so a settled camera reports `is_easing() == false` and stops requesting
/// render instead of asymptoting forever.
const POSE_EPSILON: f32 = 1e-4;

/// Eases `current` a fraction `alpha` toward `target`, snapping exactly within [`POSE_EPSILON`].
fn ease_scalar(current: &mut f32, target: f32, alpha: f32) {
    *current += (target - *current) * alpha;
    if (target - *current).abs() <= POSE_EPSILON {
        *current = target;
    }
}

/// Eases `current` a fraction `alpha` toward `target`, snapping exactly once every component
/// is within [`POSE_EPSILON`].
fn ease_vec3(current: &mut Vec3, target: Vec3, alpha: f32) {
    *current += (target - *current) * alpha;
    if (target - *current)
        .abs()
        .cmple(Vec3::splat(POSE_EPSILON))
        .all()
    {
        *current = target;
    }
}

/// Wraps an `f32` as a JSON number via its f64 promotion — the byte-equality seam every
/// scalar goes through.
fn f32_value(value: f32) -> Value {
    Value::from(f64::from(value))
}

/// A named-object `vec3` → `{"x","y","z"}`, matching the scene serde.
fn vec3_to_json(v: Vec3) -> Value {
    Value::Object(Map::from_iter([
        ("x".to_string(), f32_value(v.x)),
        ("y".to_string(), f32_value(v.y)),
        ("z".to_string(), f32_value(v.z)),
    ]))
}

/// Reads a `vec3` from a named object, each component defaulting to `0`.
fn vec3_from_json(j: &Value) -> Vec3 {
    Vec3::new(
        json_f32_or(j, "x", 0.0),
        json_f32_or(j, "y", 0.0),
        json_f32_or(j, "z", 0.0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vec3_close(a: Vec3, b: Vec3, eps: f32) {
        assert!(a.distance(b) < eps, "{a:?} != {b:?}");
    }

    #[test]
    fn forward_at_yaw_zero_looks_down_neg_z() {
        let cam = SceneEditCamera {
            yaw: 0.0,
            pitch: 0.0,
            ..SceneEditCamera::default()
        };
        // cos0·sin0 = 0, sin0 = 0, -cos0·cos0 = -1.
        vec3_close(cam.forward(), Vec3::NEG_Z, 1e-6);
    }

    #[test]
    fn forward_matches_cpp_at_a_sample_yaw_pitch() {
        let cam = SceneEditCamera {
            yaw: -37.0,
            pitch: -29.0,
            ..SceneEditCamera::default()
        };
        // cos(pitch)·sin(yaw), sin(pitch), -cos(pitch)·cos(yaw), then normalized.
        let yaw = (-37.0f32).to_radians();
        let pitch = (-29.0f32).to_radians();
        let expected = Vec3::new(
            pitch.cos() * yaw.sin(),
            pitch.sin(),
            -pitch.cos() * yaw.cos(),
        )
        .normalize();
        vec3_close(cam.forward(), expected, 1e-6);
        // It is a unit vector.
        assert!((cam.forward().length() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn view_is_look_at_from_eye_along_forward() {
        let cam = SceneEditCamera::default();
        let view = cam.view();
        let expected = Mat4::look_at_rh(cam.position, cam.position + cam.forward(), Vec3::Y);
        assert_eq!(view.view, expected);
        assert_eq!(view.fov, cam.fov);
        assert_eq!(view.near_plane, cam.near_plane);
        assert_eq!(view.far_plane, cam.far_plane);
    }

    #[test]
    fn wasd_moves_the_position_along_the_basis() {
        let mut cam = SceneEditCamera {
            yaw: 0.0,
            pitch: 0.0,
            ..SceneEditCamera::default()
        };
        cam.sync_target();
        let start = cam.position;
        let dt = 0.1;
        let input = SceneEditCameraInput {
            active: true,
            forward: true,
            ..SceneEditCameraInput::default()
        };
        update_scene_edit_camera(&mut cam, &input, dt);
        // At yaw/pitch 0 forward is -Z; W advances along it by move_speed·dt.
        let expected = start + Vec3::NEG_Z * (cam.move_speed * dt);
        vec3_close(cam.position, expected, 1e-5);
        assert!(cam.controlling, "active latches controlling");
    }

    #[test]
    fn up_and_down_move_along_world_y() {
        let mut cam = SceneEditCamera::default();
        let y0 = cam.position.y;
        let dt = 0.1;
        update_scene_edit_camera(
            &mut cam,
            &SceneEditCameraInput {
                active: true,
                up: true,
                ..SceneEditCameraInput::default()
            },
            dt,
        );
        assert!(cam.position.y > y0, "Space moves up world Y");
        let y1 = cam.position.y;
        update_scene_edit_camera(
            &mut cam,
            &SceneEditCameraInput {
                active: true,
                down: true,
                ..SceneEditCameraInput::default()
            },
            dt,
        );
        assert!((cam.position.y - (y1 - cam.move_speed * dt)).abs() < 1e-5);
    }

    #[test]
    fn look_eases_monotonically_toward_the_target_orientation() {
        let mut cam = SceneEditCamera {
            yaw: 0.0,
            pitch: 0.0,
            ..SceneEditCamera::default()
        };
        cam.sync_target();
        // One look sample of +x (yaw) pixels arrives, landing on the target orientation; the
        // eye then eases toward it with no further input.
        let dt = 1.0 / 60.0;
        let target_yaw = 100.0 * cam.look_speed;
        update_scene_edit_camera(
            &mut cam,
            &SceneEditCameraInput {
                active: true,
                look_delta: Vec2::new(100.0, 0.0),
                ..SceneEditCameraInput::default()
            },
            dt,
        );
        assert!(
            (cam.target_yaw - target_yaw).abs() < 1e-4,
            "the sample lands whole on the target orientation"
        );
        let mut prev = cam.yaw;
        // The eye keeps easing toward the target each frame, converging monotonically without
        // overshoot.
        for _ in 0..200 {
            update_scene_edit_camera(&mut cam, &SceneEditCameraInput::default(), dt);
            assert!(cam.yaw >= prev - 1e-6, "yaw advances monotonically");
            assert!(cam.yaw <= target_yaw + 1e-3, "no overshoot past the target");
            prev = cam.yaw;
        }
        assert!(
            (cam.yaw - target_yaw).abs() < 1e-2,
            "the ease converges to the sample ({} vs {target_yaw})",
            cam.yaw
        );
        assert!(!cam.is_easing(), "a converged camera stops easing");
    }

    #[test]
    fn pitch_clamps_to_plus_minus_89() {
        let mut cam = SceneEditCamera {
            pitch: 0.0,
            ..SceneEditCamera::default()
        };
        cam.sync_target();
        // A large sustained down-look (positive look_delta.y lowers pitch) over many frames
        // must clamp at -89.
        for _ in 0..2000 {
            update_scene_edit_camera(
                &mut cam,
                &SceneEditCameraInput {
                    active: true,
                    look_delta: Vec2::new(0.0, 1000.0),
                    ..SceneEditCameraInput::default()
                },
                1.0 / 60.0,
            );
        }
        assert!((cam.pitch - (-89.0)).abs() < 1e-3, "pitch clamps at -89");

        // And the other extreme.
        for _ in 0..2000 {
            update_scene_edit_camera(
                &mut cam,
                &SceneEditCameraInput {
                    active: true,
                    look_delta: Vec2::new(0.0, -1000.0),
                    ..SceneEditCameraInput::default()
                },
                1.0 / 60.0,
            );
        }
        assert!((cam.pitch - 89.0).abs() < 1e-3, "pitch clamps at +89");
    }

    #[test]
    fn controlling_latches_and_unlatches_with_active() {
        let mut cam = SceneEditCamera::default();
        assert!(!cam.controlling);
        update_scene_edit_camera(
            &mut cam,
            &SceneEditCameraInput {
                active: true,
                ..SceneEditCameraInput::default()
            },
            1.0 / 60.0,
        );
        assert!(cam.controlling, "active latches");
        update_scene_edit_camera(&mut cam, &SceneEditCameraInput::default(), 1.0 / 60.0);
        assert!(!cam.controlling, "inactive unlatches");
    }

    #[test]
    fn inactive_still_eases_the_look_tail() {
        let mut cam = SceneEditCamera {
            yaw: 0.0,
            ..SceneEditCamera::default()
        };
        cam.sync_target();
        // A target orientation the eye has not yet reached (e.g. a flick just before RMB
        // release) keeps easing out even with no active hold.
        cam.target_yaw = 10.0;
        let before = cam.yaw;
        update_scene_edit_camera(&mut cam, &SceneEditCameraInput::default(), 1.0 / 60.0);
        assert!(cam.yaw > before, "the tail keeps easing while inactive");
        assert!(!cam.controlling);
    }

    #[test]
    fn orbit_sweeps_the_arc_at_a_constant_radius() {
        let pivot = Vec3::new(1.0, 2.0, -3.0);
        let distance = 5.0;
        let mut cam = SceneEditCamera {
            orbit: Some(OrbitState {
                pivot,
                distance,
                target_pivot: pivot,
                target_distance: distance,
            }),
            yaw: 0.0,
            pitch: 0.0,
            target_yaw: 0.0,
            target_pitch: 0.0,
            ..SceneEditCamera::default()
        };
        cam.position = pivot - cam.forward() * distance;
        // A big yaw sweep: every frame the eye must sit exactly `distance` from the pivot — the
        // orbit follows the circle rather than lerping the eye straight across it (a chord).
        cam.target_yaw = 150.0;
        let dt = 1.0 / 60.0;
        for _ in 0..240 {
            update_scene_edit_camera(&mut cam, &SceneEditCameraInput::default(), dt);
            let radius = (cam.position - pivot).length();
            assert!(
                (radius - distance).abs() < 1e-3,
                "radius held on the arc: {radius} vs {distance}"
            );
        }
        assert!(
            (cam.yaw - 150.0).abs() < 1e-2,
            "the sweep reaches the target yaw"
        );
        assert!(!cam.is_easing(), "a settled orbit stops easing");
    }

    #[test]
    fn orbit_distance_eases_and_pivot_holds_on_a_dolly() {
        let pivot = Vec3::ZERO;
        let mut cam = SceneEditCamera {
            orbit: Some(OrbitState {
                pivot,
                distance: 4.0,
                target_pivot: pivot,
                target_distance: 4.0,
            }),
            yaw: 0.0,
            pitch: 0.0,
            target_yaw: 0.0,
            target_pitch: 0.0,
            ..SceneEditCamera::default()
        };
        cam.position = pivot - cam.forward() * 4.0;
        // Dolly the radius out to 8; the eye radius must ease monotonically toward it while the
        // pivot stays put (the eye looks at the same point throughout).
        cam.orbit.as_mut().unwrap().target_distance = 8.0;
        let dt = 1.0 / 60.0;
        let mut prev = (cam.position - pivot).length();
        for _ in 0..240 {
            update_scene_edit_camera(&mut cam, &SceneEditCameraInput::default(), dt);
            let radius = (cam.position - pivot).length();
            assert!(radius >= prev - 1e-6, "radius grows monotonically");
            assert!(radius <= 8.0 + 1e-3, "no overshoot past the dolly target");
            prev = radius;
        }
        assert!(
            (prev - 8.0).abs() < 1e-2,
            "the dolly converges to the target radius"
        );
        assert!(!cam.is_easing());
    }

    #[test]
    fn json_round_trips_position_yaw_pitch_fov() {
        let cam = SceneEditCamera {
            position: Vec3::new(1.5, -2.0, 3.25),
            yaw: 12.5,
            pitch: -7.0,
            fov: 60.0,
            ..SceneEditCamera::default()
        };
        let json = cam.to_json();
        // The exact key set.
        let object = json.as_object().unwrap();
        assert_eq!(object.len(), 4);
        assert!(object.contains_key("position"));
        assert!(object.contains_key("yaw"));
        assert!(object.contains_key("pitch"));
        assert!(object.contains_key("fov"));
        // position is a named {x,y,z} object.
        let position = object["position"].as_object().unwrap();
        assert_eq!(position.len(), 3);
        assert!(position.contains_key("x"));

        let mut restored = SceneEditCamera::default();
        restored.from_json(&json);
        vec3_close(restored.position, cam.position, 1e-6);
        assert!((restored.yaw - cam.yaw).abs() < 1e-6);
        assert!((restored.pitch - cam.pitch).abs() < 1e-6);
        assert!((restored.fov - cam.fov).abs() < 1e-6);
    }

    #[test]
    fn json_partial_leaves_unset_fields_unchanged() {
        let mut cam = SceneEditCamera {
            position: Vec3::new(9.0, 9.0, 9.0),
            yaw: 11.0,
            pitch: 22.0,
            fov: 33.0,
            ..SceneEditCamera::default()
        };
        // Only yaw is present; everything else keeps its current value.
        let partial = serde_json::json!({ "yaw": 45.0 });
        cam.from_json(&partial);
        assert!((cam.yaw - 45.0).abs() < 1e-6, "yaw is overwritten");
        assert_eq!(cam.position, Vec3::new(9.0, 9.0, 9.0), "position untouched");
        assert!((cam.pitch - 22.0).abs() < 1e-6, "pitch untouched");
        assert!((cam.fov - 33.0).abs() < 1e-6, "fov untouched");
    }

    #[test]
    fn json_non_object_is_ignored() {
        let mut cam = SceneEditCamera {
            yaw: 5.0,
            ..SceneEditCamera::default()
        };
        cam.from_json(&Value::Null);
        assert!((cam.yaw - 5.0).abs() < 1e-6);
        cam.from_json(&serde_json::json!(42));
        assert!((cam.yaw - 5.0).abs() < 1e-6);
    }
}
