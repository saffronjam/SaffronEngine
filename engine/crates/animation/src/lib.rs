//! Pure CPU pose math: the pose types, the track/clip samplers, and the pose-algebra helpers the
//! player runtime and IK build on. Zero FFI, no GPU concept.
//!
//! A sampled rotation is a [`glam::Vec4`] whose four lanes are already the quaternion in `xyzw`
//! order, so `Quat::from_vec4` reads it with no reorder.
//!
//! The only output toward rendering is the [`saffron_scene::PoseOverride`] [`tick_animation`] writes
//! onto each driven bone — a per-frame local TRS override that leaves the bone's authored
//! [`saffron_scene::Transform`] untouched, so Edit preview can scrub the timeline without dirtying
//! the saved project. Nothing flows back.

#![deny(unsafe_code)]

mod algebra;
mod error;
mod ik;
mod pose;
mod runtime;
mod sample;

pub use algebra::{apply_delta, blend_joint, pose_diff, quintic_decay, smoothstep01};
pub use error::{Error, Result};
pub use ik::solve_two_bone_ik;
pub use pose::{JointPose, PoseBuffer, PoseDelta, TwoBoneIkResult};
pub use runtime::{AnimationRuntime, ClipLoader, tick_animation};
pub use sample::{sample_track, sample_weights};

/// Whether the evaluator previews a single rig or advances every rig.
///
/// `Edit` previews one rig's clip non-destructively; `Play` advances every rig.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AnimMode {
    /// Preview only the timeline-selected rig.
    #[default]
    Edit,
    /// Advance every rig.
    Play,
}

#[cfg(test)]
mod tests {
    use glam::{Quat, Vec3, Vec4};
    use saffron_geometry::{AnimInterp, AnimPath, AnimTarget, AnimTrack};
    use saffron_test_support::{EPS, quat_close};

    use super::*;

    fn as_quat(v: Vec4) -> Quat {
        Quat::from_vec4(v)
    }

    #[test]
    fn linear_translation_endpoints_midpoint_and_clamp() {
        let track = AnimTrack {
            path: AnimPath::Translation,
            interp: AnimInterp::Linear,
            times: vec![0.0, 2.0],
            values: vec![0.0, 0.0, 0.0, 10.0, 0.0, 0.0],
            ..Default::default()
        };
        assert!(sample_track(&track, 0.0).truncate().distance(Vec3::ZERO) < EPS);
        assert!(
            sample_track(&track, 2.0)
                .truncate()
                .distance(Vec3::new(10.0, 0.0, 0.0))
                < EPS
        );
        assert!(
            sample_track(&track, 1.0)
                .truncate()
                .distance(Vec3::new(5.0, 0.0, 0.0))
                < EPS
        );
        // Clamp below the first key and above the last (no extrapolation).
        assert!(sample_track(&track, -1.0).truncate().distance(Vec3::ZERO) < EPS);
        assert!(
            sample_track(&track, 9.0)
                .truncate()
                .distance(Vec3::new(10.0, 0.0, 0.0))
                < EPS
        );
    }

    #[test]
    fn step_scale_holds_previous_key() {
        let track = AnimTrack {
            path: AnimPath::Scale,
            interp: AnimInterp::Step,
            times: vec![0.0, 1.0],
            values: vec![1.0, 1.0, 1.0, 3.0, 3.0, 3.0],
            ..Default::default()
        };
        assert!(sample_track(&track, 0.9).truncate().distance(Vec3::ONE) < EPS);
        assert!(
            sample_track(&track, 1.0)
                .truncate()
                .distance(Vec3::splat(3.0))
                < EPS
        );
    }

    #[test]
    fn cubic_spline_translation_bends_the_midpoint() {
        // Asymmetric tangents bend the midpoint to 0.75 (distinct from the linear
        // 0.5), proving the Hermite path runs.
        let track = AnimTrack {
            path: AnimPath::Translation,
            interp: AnimInterp::CubicSpline,
            times: vec![0.0, 1.0],
            values: vec![
                0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 2.0, 0.0, 0.0, // key0: in, value, out
                0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, // key1: in, value, out
            ],
            ..Default::default()
        };
        assert!(sample_track(&track, 0.0).truncate().distance(Vec3::ZERO) < EPS);
        assert!(
            sample_track(&track, 1.0)
                .truncate()
                .distance(Vec3::new(1.0, 0.0, 0.0))
                < EPS
        );
        assert!((sample_track(&track, 0.5).x - 0.75).abs() < EPS);
    }

    #[test]
    fn linear_rotation_is_slerp() {
        // 0 deg -> 90 deg about Y; the midpoint is exactly 45 deg.
        let s = 0.5_f32.sqrt();
        let track = AnimTrack {
            path: AnimPath::Rotation,
            interp: AnimInterp::Linear,
            times: vec![0.0, 1.0],
            // xyzw: identity, then 90 deg about Y.
            values: vec![0.0, 0.0, 0.0, 1.0, 0.0, s, 0.0, s],
            ..Default::default()
        };
        let q0 = Quat::IDENTITY;
        let q90 = Quat::from_axis_angle(Vec3::Y, 90.0_f32.to_radians());
        let q45 = Quat::from_axis_angle(Vec3::Y, 45.0_f32.to_radians());
        assert!(quat_close(as_quat(sample_track(&track, 0.0)), q0));
        assert!(quat_close(as_quat(sample_track(&track, 1.0)), q90));
        assert!(quat_close(as_quat(sample_track(&track, 0.5)), q45));
    }

    #[test]
    fn sample_weights_interpolates_each_lane_and_keeps_rest_when_empty() {
        // A 2-wide morph-weight track, two keys: lane 0 ramps 0→1, lane 1 ramps 1→0.
        let track = AnimTrack {
            target: AnimTarget::Node,
            path: AnimPath::Weights,
            interp: AnimInterp::Linear,
            morph_count: 2,
            times: vec![0.0, 1.0],
            values: vec![0.0, 1.0, 1.0, 0.0],
            ..Default::default()
        };
        let mut w = vec![0.0; 2];
        sample_weights(&track, 0.0, &mut w);
        assert!((w[0] - 0.0).abs() < EPS && (w[1] - 1.0).abs() < EPS);
        sample_weights(&track, 1.0, &mut w);
        assert!((w[0] - 1.0).abs() < EPS && (w[1] - 0.0).abs() < EPS);
        sample_weights(&track, 0.5, &mut w);
        assert!((w[0] - 0.5).abs() < EPS && (w[1] - 0.5).abs() < EPS);

        // Step holds the previous key.
        let step = AnimTrack {
            interp: AnimInterp::Step,
            ..track.clone()
        };
        let mut s = vec![0.0; 2];
        sample_weights(&step, 0.4, &mut s);
        assert!((s[0] - 0.0).abs() < EPS && (s[1] - 1.0).abs() < EPS);

        // A zero-count / empty track leaves the seeded rest weights untouched.
        let empty = AnimTrack {
            morph_count: 0,
            ..track.clone()
        };
        let mut rest = vec![0.3, 0.7];
        sample_weights(&empty, 0.5, &mut rest);
        assert!((rest[0] - 0.3).abs() < EPS && (rest[1] - 0.7).abs() < EPS);
    }

    #[test]
    fn pose_delta_round_trips_and_zero_weight_is_base() {
        // The delta carries `to` onto `from` at weight 1 and is the identity at
        // weight 0 — the reusable offset machinery transitions build on.
        let from = JointPose {
            translation: Vec3::new(1.0, 2.0, 3.0),
            rotation: Quat::from_axis_angle(Vec3::Y, 30.0_f32.to_radians()),
            scale: Vec3::splat(2.0),
        };
        let to = JointPose::default(); // identity rest
        let delta = pose_diff(&from, &to);
        let full = apply_delta(&to, &delta, 1.0);
        let none = apply_delta(&to, &delta, 0.0);
        assert!(full.translation.distance(from.translation) < EPS);
        assert!(quat_close(full.rotation, from.rotation));
        assert!(full.scale.distance(from.scale) < EPS);
        assert!(none.translation.distance(to.translation) < EPS);
        assert!(quat_close(none.rotation, to.rotation));
    }

    #[test]
    fn weight_curves_hit_their_endpoints() {
        // smoothstep01 is C1 from 0 to 1; quintic_decay is C2 from 1 to 0; both
        // clamp outside [0, 1].
        assert!((smoothstep01(0.0) - 0.0).abs() < EPS);
        assert!((smoothstep01(1.0) - 1.0).abs() < EPS);
        assert!((smoothstep01(0.5) - 0.5).abs() < EPS);
        assert!((smoothstep01(-1.0) - 0.0).abs() < EPS);
        assert!((smoothstep01(2.0) - 1.0).abs() < EPS);
        assert!((quintic_decay(0.0) - 1.0).abs() < EPS);
        assert!((quintic_decay(1.0) - 0.0).abs() < EPS);
        assert!((quintic_decay(0.5) - 0.5).abs() < EPS);
    }

    #[test]
    fn blend_joint_interpolates_endpoints() {
        let base = JointPose::default();
        let over = JointPose {
            translation: Vec3::new(4.0, 0.0, 0.0),
            rotation: Quat::from_axis_angle(Vec3::Y, 90.0_f32.to_radians()),
            scale: Vec3::splat(3.0),
        };
        let at0 = blend_joint(&base, &over, 0.0);
        let at1 = blend_joint(&base, &over, 1.0);
        assert!(at0.translation.distance(base.translation) < EPS);
        assert!(quat_close(at0.rotation, base.rotation));
        assert!(at1.translation.distance(over.translation) < EPS);
        assert!(quat_close(at1.rotation, over.rotation));
        let mid = blend_joint(&base, &over, 0.5);
        assert!(mid.translation.distance(Vec3::new(2.0, 0.0, 0.0)) < EPS);
    }

    #[test]
    fn empty_track_yields_path_identity() {
        let t = AnimTrack {
            path: AnimPath::Translation,
            ..Default::default()
        };
        assert!(sample_track(&t, 0.5).truncate().distance(Vec3::ZERO) < EPS);
        let s = AnimTrack {
            path: AnimPath::Scale,
            ..Default::default()
        };
        assert!(sample_track(&s, 0.5).truncate().distance(Vec3::ONE) < EPS);
        let r = AnimTrack {
            path: AnimPath::Rotation,
            ..Default::default()
        };
        assert!(quat_close(as_quat(sample_track(&r, 0.5)), Quat::IDENTITY));
    }
}
