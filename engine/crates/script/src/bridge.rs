//! The host-callback POD seam: the [`ScriptHostBridge`] trait and the two Jolt-free
//! POD structs ([`ScriptRayHit`], [`ScriptRagdollState`]) the physics-reaching bindings
//! exchange with the host.
//!
//! Routing every physics reach through one trait over POD args is what keeps this crate off a
//! physics or sceneedit dependency edge; the host, which does depend on both, implements it.
//!
//! [`ScriptHost`](crate::ScriptHost) defaults its bridge to [`NoopBridge`], so a session with no
//! host-installed bridge sees `raycast` miss, `get_velocity` return zero, and the ragdoll and log
//! calls no-op rather than panic.

use glam::Vec3;

use saffron_core::Uuid;

/// A physics ray/sphere hit surfaced to Lua, Jolt-free POD.
///
/// The host fills it from `World::raycast`/`sphere_cast` (a plain field copy off the
/// physics crate's `RayHit`); this keeps `saffron-script` free of a physics edge — the
/// `sa.raycast`/`sa.spherecast` binding only ever sees this POD, then shapes it into the
/// `{hit, distance, point, normal, entity}` Lua table.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ScriptRayHit {
    /// Whether the ray/sweep hit anything.
    pub hit: bool,
    /// The struck body's tagged owner (`None` on a miss or an unowned body).
    pub target: Option<ScriptHitTarget>,
    /// World-space contact point.
    pub point: Vec3,
    /// World-space surface normal at the hit.
    pub normal: Vec3,
    /// Distance along the ray from the origin.
    pub distance: f32,
}

/// One macro plant a script-side vegetation query matched.
///
/// The identity is the canonical 32-digit hexadecimal string, the same text the control plane and
/// the `sa` CLI use, so a script can hand it straight back to an interaction call. Position and
/// bounds are render-relative metres, matching every other script-visible world value.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ScriptPlantHit {
    /// Canonical plant identity.
    pub plant: String,
    /// Render-relative plant position.
    pub position: Vec3,
    /// Metric distance from the query origin.
    pub distance: f32,
    /// Biological lifecycle name (`seed`, `sprout`, `mature`, `stump`, …).
    pub lifecycle: String,
    /// Persistent health in 0..1.
    pub health: f32,
    /// Gameplay interaction policy name (`decorative`, `interactive`, `harvestable`, `structural`).
    pub interaction_policy: String,
}

/// The tagged owner of a struck body, mirrored from the physics world-hit target so
/// `saffron-script` stays free of a physics dependency.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScriptHitTarget {
    /// A hecs scene entity, by stable uuid.
    SceneEntity(Uuid),
    /// An authoritative macro plant, by its full 128-bit identity.
    Vegetation(saffron_spatial::PlantId),
}

/// A rig's live ragdoll state surfaced to Lua, Jolt-free POD.
///
/// The host fills it from `World::ragdoll_state`; `sa.Entity:ragdoll_state()` shapes it
/// into the `{present, active, body_weight, bones}` Lua table.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ScriptRagdollState {
    /// `true` when the rig has a live ragdoll instance this play session.
    pub present: bool,
    /// `true` when the ragdoll's motors drive toward the animation (active vs passive).
    pub active: bool,
    /// The mean per-bone target weight (`0` = pure animation, `1` = pure physics).
    pub body_weight: f32,
    /// The ragdoll's bone count.
    pub bones: i32,
}

/// The host-callback seam the physics-reaching `sa.*` bindings dispatch through.
///
/// One method per bridge, over POD args only — so `saffron-script` reaches the live
/// physics world and the editor's script-log ring without importing `saffron-physics`
/// or `saffron-sceneedit`. The host implements it (`saffron-host`), routing each method
/// to a `World`/edit-context call; an unset bridge is [`NoopBridge`].
///
/// `ScriptHost` holds the installed bridge as an `Rc<dyn ScriptHostBridge>` and lends it
/// to the session for the duration of a start/tick/contact call — the bindings reach it
/// through [`crate::session::with_bridge`]. It is read-only during a session, so it
/// crosses as a shared `Rc` clone, not a moved value.
pub trait ScriptHostBridge {
    /// Cast a ray `origin + dir * max_dist` against the live world. A miss returns
    /// [`ScriptRayHit::default`].
    fn raycast(&self, origin: Vec3, dir: Vec3, max_dist: f32) -> ScriptRayHit;

    /// Sweep a sphere of `radius` along `origin + dir * max_dist` — a thicker probe than
    /// [`Self::raycast`].
    fn sphere_cast(&self, origin: Vec3, dir: Vec3, radius: f32, max_dist: f32) -> ScriptRayHit;

    /// Apply a center-of-mass impulse to the Dynamic body owned by `entity`. A
    /// non-Dynamic / unmapped body is a no-op on the host side.
    fn apply_impulse(&self, entity: Uuid, impulse: Vec3);

    /// Add a continuous force (applied over the next step) to `entity`'s Dynamic body.
    fn add_force(&self, entity: Uuid, force: Vec3);

    /// Set the absolute linear velocity of `entity`'s Dynamic body.
    fn set_velocity(&self, entity: Uuid, velocity: Vec3);

    /// Set the morph-target weights of `entity`'s morph mesh (canonical 0..1). A length
    /// mismatch or a non-morph entity is a no-op on the host side.
    fn set_morph_weights(&self, entity: Uuid, weights: &[f32]);

    /// The current linear velocity of `entity`'s Dynamic body, or zero when there is
    /// none.
    fn get_velocity(&self, entity: Uuid) -> Vec3;

    /// Go limp / restore the rig identified by `rig`; returns whether the toggle
    /// succeeded.
    fn set_ragdoll_enabled(&self, rig: Uuid, enable: bool) -> bool;

    /// Blend a rig between physics and animation: `active` arms/releases the motors,
    /// `body_weight` sets the global target weight.
    fn set_ragdoll_blend(&self, rig: Uuid, active: bool, body_weight: f32);

    /// The rig's live ragdoll state.
    fn ragdoll_state(&self, rig: Uuid) -> ScriptRagdollState;

    /// The closest macro plant along `origin + dir * max_dist` whose conservative bounds the ray
    /// enters. Bounds-level, never a physics cast: it reports plants with no collision body too.
    fn vegetation_raycast(&self, origin: Vec3, dir: Vec3, max_dist: f32) -> Option<ScriptPlantHit>;

    /// The macro plant nearest `position` within `radius`.
    fn vegetation_nearest(&self, position: Vec3, radius: f32) -> Option<ScriptPlantHit>;

    /// Every macro plant within `radius` of `position`, nearest first, capped at `limit`.
    fn vegetation_in_radius(
        &self,
        position: Vec3,
        radius: f32,
        limit: usize,
    ) -> Vec<ScriptPlantHit>;

    /// Apply `amount` of damage (0..1) to the plant, returning whether the mutation committed.
    fn vegetation_damage(&self, plant: &str, amount: f32) -> bool;

    /// Harvest the plant into `phenotype`, returning whether the mutation committed.
    fn vegetation_harvest(&self, plant: &str, phenotype: u32) -> bool;

    /// Route a `sa.log(...)` line to the editor's script-log ring, tagged with the uuid
    /// of the instance whose handler is running. Called *after* the engine log, so a
    /// no-op sink still writes the console.
    fn log_sink(&self, sender: Uuid, message: &str);
}

/// The default bridge: every method is a safe no-op (a missed raycast, zero velocity, a
/// dropped log line). Installed on a fresh [`ScriptHost`](crate::ScriptHost) so a session
/// without a host-installed bridge degrades cleanly.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoopBridge;

impl ScriptHostBridge for NoopBridge {
    fn raycast(&self, _origin: Vec3, _dir: Vec3, _max_dist: f32) -> ScriptRayHit {
        ScriptRayHit::default()
    }

    fn sphere_cast(&self, _origin: Vec3, _dir: Vec3, _radius: f32, _max_dist: f32) -> ScriptRayHit {
        ScriptRayHit::default()
    }

    fn apply_impulse(&self, _entity: Uuid, _impulse: Vec3) {}

    fn add_force(&self, _entity: Uuid, _force: Vec3) {}

    fn set_velocity(&self, _entity: Uuid, _velocity: Vec3) {}

    fn set_morph_weights(&self, _entity: Uuid, _weights: &[f32]) {}

    fn get_velocity(&self, _entity: Uuid) -> Vec3 {
        Vec3::ZERO
    }

    fn set_ragdoll_enabled(&self, _rig: Uuid, _enable: bool) -> bool {
        false
    }

    fn set_ragdoll_blend(&self, _rig: Uuid, _active: bool, _body_weight: f32) {}

    fn ragdoll_state(&self, _rig: Uuid) -> ScriptRagdollState {
        ScriptRagdollState::default()
    }

    fn vegetation_raycast(
        &self,
        _origin: Vec3,
        _dir: Vec3,
        _max_dist: f32,
    ) -> Option<ScriptPlantHit> {
        None
    }

    fn vegetation_nearest(&self, _position: Vec3, _radius: f32) -> Option<ScriptPlantHit> {
        None
    }

    fn vegetation_in_radius(
        &self,
        _position: Vec3,
        _radius: f32,
        _limit: usize,
    ) -> Vec<ScriptPlantHit> {
        Vec::new()
    }

    fn vegetation_damage(&self, _plant: &str, _amount: f32) -> bool {
        false
    }

    fn vegetation_harvest(&self, _plant: &str, _phenotype: u32) -> bool {
        false
    }

    fn log_sink(&self, _sender: Uuid, _message: &str) {}
}

#[cfg(test)]
mod tests {
    /// The crate-boundary contract (README §1): `saffron-script` stays
    /// `saffron-core` + `saffron-scene` only — the physics reach crosses the POD
    /// [`super::ScriptHostBridge`] seam, never a `saffron-physics`/`saffron-animation`
    /// dependency edge. This guards the manifest against an accidental edge a future
    /// change might add (which would defeat the whole point of the bridge).
    #[test]
    fn no_physics_or_animation_dependency_edge() {
        let manifest = include_str!("../Cargo.toml");
        let deps = manifest
            .split("[dependencies]")
            .nth(1)
            .expect("a [dependencies] section");
        // Stop at the next section header so dev-deps / other tables do not count.
        let deps = deps.split("\n[").next().unwrap_or(deps);
        for forbidden in ["saffron-physics", "saffron-animation", "saffron-sceneedit"] {
            assert!(
                !deps.contains(forbidden),
                "saffron-script must not depend on {forbidden} (the bridge POD seam crosses it)"
            );
        }
    }
}
