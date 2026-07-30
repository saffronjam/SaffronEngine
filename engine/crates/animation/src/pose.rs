//! The decomposed pose types clips sample into and the blend layer operates on.

use glam::{Quat, Vec3};

/// A single joint's local transform, decomposed — the form clips sample into and
/// the blend layer operates on.
///
/// Rotation is a unit quaternion in glam's `xyzw` order.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct JointPose {
    /// Local translation.
    pub translation: Vec3,
    /// Local rotation (unit quaternion).
    pub rotation: Quat,
    /// Local scale.
    pub scale: Vec3,
}

impl Default for JointPose {
    fn default() -> Self {
        Self {
            translation: Vec3::ZERO,
            rotation: Quat::IDENTITY,
            scale: Vec3::ONE,
        }
    }
}

/// A skeleton-sized pose, indexed 1:1 with a skinned mesh's bones.
///
/// `local` is the sampled/animated TRS, `override_` is where external producers (IK/physics) write,
/// and `weight` is the per-bone blend layer — `0` meaning pure animation. `override_` and `weight`
/// stay empty/zero until a producer fills them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PoseBuffer {
    /// The sampled/animated local TRS per joint.
    pub local: Vec<JointPose>,
    /// External-producer overrides (IK/physics); empty until a producer writes.
    pub override_: Vec<JointPose>,
    /// The per-bone blend weight; zero (or empty) means pure animation.
    pub weight: Vec<f32>,
}

/// A per-joint pose offset — the delta that carries one pose onto another.
///
/// Additive translation, a delta quaternion (`from * inverse(to)`), and a
/// multiplicative scale ratio. The same delta-pose machinery a physics handoff
/// (ragdoll) uses to nudge an animated target.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PoseDelta {
    /// Additive translation offset.
    pub translation: Vec3,
    /// Delta rotation (`from * inverse(to)`).
    pub rotation: Quat,
    /// Multiplicative scale ratio.
    pub scale: Vec3,
}

impl Default for PoseDelta {
    fn default() -> Self {
        Self {
            translation: Vec3::ZERO,
            rotation: Quat::IDENTITY,
            scale: Vec3::ONE,
        }
    }
}

/// The two world-space joint rotations a two-bone solve produces.
///
/// `upper` is the thigh/upper-arm and `lower` the shin/forearm. The end effector
/// is positioned by composing these onto the chain.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TwoBoneIkResult {
    /// World-space delta rotation for the upper joint.
    pub upper: Quat,
    /// World-space delta rotation for the lower joint.
    pub lower: Quat,
}

impl Default for TwoBoneIkResult {
    fn default() -> Self {
        Self {
            upper: Quat::IDENTITY,
            lower: Quat::IDENTITY,
        }
    }
}
