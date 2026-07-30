//! The two-bone IK solver: a law-of-cosines knee solve with a signed-`atan2`
//! pole twist and the epsilon thicket that keeps every branch numerically safe.

use std::f32::consts::PI;

use glam::{Quat, Vec3};

use crate::pose::TwoBoneIkResult;

/// Shortest-arc rotation taking unit `from` onto unit `to`.
///
/// Falls back to a stable perpendicular axis for the antiparallel case and
/// identity for degenerate (near-zero) inputs.
fn rotation_between(mut from: Vec3, mut to: Vec3) -> Quat {
    let lf = from.length();
    let lt = to.length();
    if lf < 1e-8 || lt < 1e-8 {
        return Quat::IDENTITY;
    }
    from /= lf;
    to /= lt;
    let d = from.dot(to).clamp(-1.0, 1.0);
    if d > 1.0 - 1e-6 {
        return Quat::IDENTITY;
    }
    if d < -1.0 + 1e-6 {
        // Antiparallel: any perpendicular axis is a valid 180 deg flip.
        let mut axis = Vec3::X.cross(from);
        if axis.length() < 1e-6 {
            axis = Vec3::Y.cross(from);
        }
        return Quat::from_axis_angle(axis.normalize(), PI).normalize();
    }
    let axis = from.cross(to).normalize();
    Quat::from_axis_angle(axis, d.acos()).normalize()
}

/// Law-of-cosines interior angle opposite `opp`, with the two adjacent sides
/// `adj0`/`adj1`; clamped into `acos`'s domain.
fn angle_opposite(adj0: f32, adj1: f32, opp: f32) -> f32 {
    let cos_a = ((adj0 * adj0 + adj1 * adj1 - opp * opp) / (2.0 * adj0 * adj1)).clamp(-1.0, 1.0);
    cos_a.acos()
}

/// World-space delta rotations for a two-bone chain so its end effector reaches `target`, with the
/// mid joint twisted toward `pole_vector`.
///
/// The returned quaternions are world-space *delta* rotations: the caller pre-multiplies each onto the
/// joint's current world rotation, then strips the parent world rotation to land in local space. A
/// law-of-cosines solve after ozz's `IKTwoBoneJob` — re-bend the knee to the reach angle, swing the
/// chain onto the target, then twist the bend plane onto the pole.
///
/// Total over its domain: the reach clamp and the `max(len, 1e-6)` floors keep every `acos` and
/// division valid, so there is no `Result`. A target on the root returns identity rotations.
pub fn solve_two_bone_ik(
    root: Vec3,
    mid: Vec3,
    end: Vec3,
    target: Vec3,
    pole_vector: Vec3,
    upper_len: f32,
    lower_len: f32,
) -> TwoBoneIkResult {
    let mut out = TwoBoneIkResult::default();
    let a = upper_len.max(1e-6);
    let b = lower_len.max(1e-6);

    let to_target = target - root;
    let reach = to_target.length();
    if reach < 1e-6 {
        return out; // target on the root: nothing to aim at, stay put
    }
    // Clamp the reach into the chain's range so each acos stays valid (graceful over/under).
    let reach_clamped = reach.clamp((a - b).abs() + 1e-4, a + b - 1e-4);

    let start_mid = mid - root;
    let start_end = end - root;
    let len_start_end = start_end.length();

    // The bend axis: perpendicular to the limb plane. Seed it from the current bend; if the
    // chain is straight, fall back to the pole so the knee has a definite hinge direction.
    let mut bend_axis = start_mid.cross(start_end);
    if bend_axis.length() < 1e-6 {
        bend_axis = start_mid.cross(pole_vector);
        if bend_axis.length() < 1e-6 {
            bend_axis = start_mid.cross(Vec3::Z);
        }
        if bend_axis.length() < 1e-6 {
            bend_axis = Vec3::Z;
        }
    }
    bend_axis = bend_axis.normalize();

    // Knee bend at the mid joint: change the interior angle at the mid (between the reversed
    // upper segment and the lower segment) from its current value to the reach value. Rotating
    // the lower bone about the bend axis by this delta sets |start-end| to the clamped reach.
    let mut current_mid_angle = PI; // a degenerate (folded-back) chain reads as pi
    if len_start_end > 1e-6 {
        current_mid_angle = angle_opposite(a, b, len_start_end);
    }
    let target_mid_angle = angle_opposite(a, b, reach_clamped);
    let bend_delta = target_mid_angle - current_mid_angle;

    // Rotating the lower bone about the bend axis by ±bend_delta both yield a valid chain;
    // pick the sign that lands |start-end| on the reach length (handles the axis-sign
    // ambiguity in cross(start_mid, start_end) without a separate orientation argument).
    let mut bend = Quat::from_axis_angle(bend_axis, bend_delta);
    let mut start_end_bent = start_mid + bend * (end - mid);
    if (start_end_bent.length() - reach_clamped).abs() > 1e-3 {
        bend = Quat::from_axis_angle(bend_axis, -bend_delta);
        start_end_bent = start_mid + bend * (end - mid);
    }
    out.lower = bend.normalize();

    // Swing: rotate the whole chain about the root so the bent start->end points at the
    // target. Applied to the upper joint (the lower inherits it through the hierarchy).
    let swing = rotation_between(start_end_bent, to_target);
    out.upper = swing.normalize();

    // Pole: twist about the root->target axis so the mid joint sits in the plane spanned by
    // the target direction and the pole vector (knee/elbow points the intended way). The swung
    // mid joint (start-relative) is swing*start_mid; project it off the chain axis to get the
    // current knee direction, and likewise the desired pole direction.
    let target_dir = to_target / reach;
    let mid_bent = swing * start_mid; // start-relative mid after the swing
    let current_pole = mid_bent - target_dir * mid_bent.dot(target_dir);
    let desired_pole = pole_vector - target_dir * pole_vector.dot(target_dir);
    // Twist the chain about the root->target axis so the knee lands on the pole's side. The
    // rotation MUST be about target_dir (not the shortest arc between the poles): both poles are
    // perpendicular to target_dir, so when they are anti-aligned the shortest arc would pick an
    // arbitrary axis and flip the whole chain off the target. Use the signed angle about
    // target_dir instead, which keeps the solved end exactly on the target. Skip it on a
    // (near-)straight chain, where the knee lies on the axis and the pole plane is undefined.
    let pole_scale = a.max(b);
    if current_pole.length() > 0.02 * pole_scale && desired_pole.length() > 1e-5 {
        let cp = current_pole.normalize();
        let dp = desired_pole.normalize();
        let twist_angle = (cp.cross(dp).dot(target_dir)).atan2(cp.dot(dp));
        let twist = Quat::from_axis_angle(target_dir, twist_angle);
        out.upper = (twist * out.upper).normalize();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compose the solved world-space delta rotations back onto the chain and
    /// return the end-effector world position.
    ///
    /// The bone world rotations start at identity, so `upper` pre-multiplies the
    /// whole chain about the root and `lower` pre-multiplies the lower segment
    /// about the mid (`newMid + upper*lower*(end-mid)`). This is the load-bearing
    /// composition that pins `solve_two_bone_ik`'s contract: a returned pair of
    /// world deltas that places the end on the target.
    fn solved_end(root: Vec3, mid: Vec3, end: Vec3, res: &TwoBoneIkResult) -> Vec3 {
        let new_mid = root + res.upper * (mid - root);
        new_mid + res.upper * (res.lower * (end - mid))
    }

    #[test]
    fn ik_in_range_reaches_exactly() {
        // A straight chain along +X (lengths 1 + 1) reaches an in-range target
        // exactly, with no NaN. `|root->target| = sqrt(2)`, inside `[0, 2]`.
        let root = Vec3::ZERO;
        let mid = Vec3::new(1.0, 0.0, 0.0);
        let end = Vec3::new(2.0, 0.0, 0.0);
        let pole = Vec3::new(0.0, 1.0, 0.0);
        let target = Vec3::new(1.0, 1.0, 0.0);
        let res = solve_two_bone_ik(root, mid, end, target, pole, 1.0, 1.0);
        assert!(res.upper.is_finite(), "upper delta must not be NaN");
        assert!(res.lower.is_finite(), "lower delta must not be NaN");
        let reached = solved_end(root, mid, end, &res);
        assert!(reached.is_finite(), "reached end must not be NaN");
        assert!(
            reached.distance(target) < 1e-3,
            "end {reached:?} should reach an in-range target {target:?} exactly"
        );
    }

    #[test]
    fn ik_pre_bent_chain_reaches() {
        // The common case: the mid is already off-axis. The segment lengths match
        // upper/lower exactly so the law of cosines stays consistent, and the
        // chain still re-solves onto a 3D target (`|root->target| ~= 1.34`).
        let root = Vec3::ZERO;
        let bent_mid = Vec3::new(0.5, 0.866_025_4, 0.0); // |root->mid| = 1
        let bent_end = Vec3::new(1.5, 0.866_025_4, 0.0); // |mid->end| = 1 (along +X)
        let pole = Vec3::new(0.0, 1.0, 0.0);
        let target = Vec3::new(0.5, -1.2, 0.3);
        let res = solve_two_bone_ik(root, bent_mid, bent_end, target, pole, 1.0, 1.0);
        let reached = solved_end(root, bent_mid, bent_end, &res);
        assert!(reached.is_finite(), "reached end must not be NaN");
        assert!(
            reached.distance(target) < 1e-3,
            "end {reached:?} should reach target {target:?} from a pre-bent chain"
        );
    }

    #[test]
    fn ik_lifts_the_leg_fixture_without_straightening_it() {
        let root = Vec3::ZERO;
        let mid = Vec3::Y;
        let end = mid + Quat::from_rotation_z(67.5_f32.to_radians()) * Vec3::Y;
        let target = end + Vec3::new(0.0, 0.08, 0.0);
        let result = solve_two_bone_ik(root, mid, end, target, -Vec3::X, 1.0, 1.0);
        let reached = solved_end(root, mid, end, &result);

        assert!(
            reached.distance(target) < 1.0e-3,
            "end {reached:?} should reach the lifted target {target:?}"
        );
    }

    #[test]
    fn ik_over_reach_clamps() {
        // A target past the chain's reach (distance 5 > max reach 2) straightens
        // the chain toward the target: `|reached-root|` lands within `1e-2` of
        // `upper+lower` and the chain aims at the target, with no NaN.
        let root = Vec3::ZERO;
        let mid = Vec3::new(1.0, 0.0, 0.0);
        let end = Vec3::new(2.0, 0.0, 0.0);
        let pole = Vec3::new(0.0, 1.0, 0.0);
        let target = Vec3::new(5.0, 0.0, 0.0);
        let upper_len = 1.0_f32;
        let lower_len = 1.0_f32;
        let res = solve_two_bone_ik(root, mid, end, target, pole, upper_len, lower_len);
        assert!(res.upper.is_finite(), "upper delta must not be NaN");
        assert!(res.lower.is_finite(), "lower delta must not be NaN");
        let reached = solved_end(root, mid, end, &res);
        assert!(reached.is_finite(), "reached end must not be NaN");
        let dist = (reached - root).length();
        assert!(
            (dist - (upper_len + lower_len)).abs() < 1e-2,
            "over-reach should clamp to a straight chain (reach {dist}, expected {})",
            upper_len + lower_len
        );
        assert!(
            ((reached - root).normalize() - Vec3::new(1.0, 0.0, 0.0)).length() < 1e-2,
            "the clamped chain should aim at the target, got dir {:?}",
            (reached - root).normalize()
        );
    }

    #[test]
    fn ik_target_on_root_returns_identity() {
        // A target on the root has nothing to aim at: the solver returns identity
        // rotations and leaves the chain put.
        let root = Vec3::new(3.0, 4.0, 5.0);
        let res = solve_two_bone_ik(
            root,
            root + Vec3::X,
            root + Vec3::new(2.0, 0.0, 0.0),
            root,
            Vec3::Y,
            1.0,
            1.0,
        );
        assert_eq!(res.upper, Quat::IDENTITY);
        assert_eq!(res.lower, Quat::IDENTITY);
    }

    #[test]
    fn rotation_between_identity_for_aligned_and_degenerate() {
        // Aligned (parallel) inputs and near-zero inputs both fall through to
        // identity rather than building a degenerate axis.
        assert_eq!(rotation_between(Vec3::X, Vec3::X), Quat::IDENTITY);
        assert_eq!(
            rotation_between(Vec3::X, Vec3::new(2.0, 0.0, 0.0)),
            Quat::IDENTITY
        );
        assert_eq!(rotation_between(Vec3::ZERO, Vec3::X), Quat::IDENTITY);
        assert_eq!(rotation_between(Vec3::X, Vec3::ZERO), Quat::IDENTITY);
    }

    #[test]
    fn rotation_between_antiparallel() {
        // `from = +X`, `to = -X`: a 180 deg rotation about some axis perpendicular
        // to X that takes +X onto -X (the antiparallel-axis fallback). The
        // quaternion stays unit.
        let q = rotation_between(Vec3::X, Vec3::new(-1.0, 0.0, 0.0));
        let rotated = q * Vec3::X;
        assert!(rotated.is_finite(), "antiparallel flip must not be NaN");
        assert!(
            rotated.distance(Vec3::new(-1.0, 0.0, 0.0)) < 1e-3,
            "antiparallel flip should map +X onto -X, got {rotated:?}"
        );
        assert!(
            (q.length() - 1.0).abs() < 1e-5,
            "the flip quaternion should stay unit, got length {}",
            q.length()
        );
    }

    #[test]
    fn rotation_between_general_maps_from_onto_to() {
        let q = rotation_between(Vec3::X, Vec3::Y);
        let rotated = q * Vec3::X;
        assert!(rotated.distance(Vec3::Y) < 1e-3);
    }

    #[test]
    fn ik_pole_flips_knee_not_chain() {
        // Flipping the pole to the opposite side must move the *knee* to that side
        // while the *end* stays on the target. This proves the twist is a signed
        // `atan2` about the root->target axis, not a shortest-arc between the
        // projected poles (which, with the poles anti-aligned, would pick an
        // arbitrary axis and flip the whole chain off the target).
        let root = Vec3::ZERO;
        let mid = Vec3::new(1.0, 0.0, 0.0);
        let end = Vec3::new(2.0, 0.0, 0.0);
        let target = Vec3::new(1.0, 0.0, 0.0); // along +X, half-extended (knee must bend off-axis)
        let res_plus = solve_two_bone_ik(root, mid, end, target, Vec3::Y, 1.0, 1.0);
        let res_minus =
            solve_two_bone_ik(root, mid, end, target, Vec3::new(0.0, -1.0, 0.0), 1.0, 1.0);

        let knee_plus = root + res_plus.upper * (mid - root);
        let knee_minus = root + res_minus.upper * (mid - root);
        assert!(knee_plus.is_finite() && knee_minus.is_finite());
        // The +Y pole steers the knee to +Y; the -Y pole steers it to -Y. They
        // land on opposite sides of the target axis (the knee flips, not the chain).
        assert!(
            knee_plus.y > 1e-3,
            "knee should sit on the +Y side for a +Y pole, got {knee_plus:?}"
        );
        assert!(
            knee_minus.y < -1e-3,
            "knee should sit on the -Y side for a -Y pole, got {knee_minus:?}"
        );
        // The end stays exactly on the target in both solves: the chain did not flip off.
        assert!(
            solved_end(root, mid, end, &res_plus).distance(target) < 1e-3,
            "the +Y-pole solve must keep the end on target"
        );
        assert!(
            solved_end(root, mid, end, &res_minus).distance(target) < 1e-3,
            "the -Y-pole solve must keep the end on target"
        );
    }
}
