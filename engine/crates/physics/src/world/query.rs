//! The read-only inspection and spatial-query surface plus the velocity mutators.

use glam::Vec3;

use saffron_core::Uuid;
use saffron_physics_sys::{self as sys};

use crate::types::{BodyInfo, MotionType, RayHit, WorldStats};

use super::*;

impl World {
    /// A summary of the live world. `active` is always `true` from a live world (the
    /// `Option<World>` is the host's), kept for the wire DTO shape.
    #[must_use]
    pub fn stats(&self) -> WorldStats {
        WorldStats {
            active: true,
            body_count: i32::try_from(sys::world_body_count(&self.world)).unwrap_or(i32::MAX),
            dynamic_count: self.dynamic_body_count,
        }
    }

    /// Every tracked body's read-only snapshot, in creation order.
    #[must_use]
    pub fn list_bodies(&self) -> Vec<BodyInfo> {
        self.bodies
            .iter()
            .map(|entry| BodyInfo {
                target: Some(entry.target),
                motion: entry.motion,
                active: sys::body_is_active(&self.world, entry.id),
                position: Vec3::from_array(sys::body_position(&self.world, entry.id)),
            })
            .collect()
    }

    /// The live world's moving emitters for the cosmetic interaction field: every
    /// awake dynamic body and every character as (world position, world velocity).
    pub fn motion_emitters(&self) -> Vec<(Vec3, Vec3)> {
        let mut emitters = Vec::new();
        for entry in &self.bodies {
            if entry.motion != MotionType::Dynamic || !sys::body_is_active(&self.world, entry.id) {
                continue;
            }
            emitters.push((
                Vec3::from_array(sys::body_position(&self.world, entry.id)),
                Vec3::from_array(sys::body_linear_velocity(&self.world, entry.id)),
            ));
        }
        for entry in &self.characters {
            emitters.push((
                Vec3::from_array(sys::character_position(&self.world, entry.index)),
                entry.last_velocity,
            ));
        }
        emitters
    }

    /// Apply a center-of-mass impulse to the Dynamic body owned by `entity`. A non-Dynamic /
    /// unmapped target is a no-op with a warning (never a panic).
    pub fn apply_impulse(&mut self, entity: Uuid, impulse: Vec3) {
        match self.dynamic_body_id(entity) {
            Some(id) => sys::body_add_impulse(&mut self.world, id, impulse.to_array()),
            None => tracing::warn!(
                "physics: apply-impulse on a non-Dynamic / unmapped body ({})",
                entity.0
            ),
        }
    }

    /// Add a force (applied over the next step) to the Dynamic body owned by `entity`. A
    /// non-Dynamic / unmapped target is a no-op with a warning.
    pub fn add_force(&mut self, entity: Uuid, force: Vec3) {
        match self.dynamic_body_id(entity) {
            Some(id) => sys::body_add_force(&mut self.world, id, force.to_array()),
            None => tracing::warn!(
                "physics: add-force on a non-Dynamic / unmapped body ({})",
                entity.0
            ),
        }
    }

    /// Set the linear velocity of the Dynamic body owned by `entity`. A non-Dynamic / unmapped
    /// target is a no-op with a warning.
    pub fn set_linear_velocity(&mut self, entity: Uuid, velocity: Vec3) {
        match self.dynamic_body_id(entity) {
            Some(id) => sys::body_set_linear_velocity(&mut self.world, id, velocity.to_array()),
            None => tracing::warn!(
                "physics: set-velocity on a non-Dynamic / unmapped body ({})",
                entity.0
            ),
        }
    }

    /// The current linear velocity of the Dynamic body owned by `entity`, or zero when there is no
    /// such body.
    #[must_use]
    pub fn body_linear_velocity(&self, entity: Uuid) -> Vec3 {
        match self.dynamic_body_id(entity) {
            Some(id) => Vec3::from_array(sys::body_linear_velocity(&self.world, id)),
            None => Vec3::ZERO,
        }
    }

    /// A dynamic body's current angular velocity (radians per second about each world axis), for
    /// the promotion write-back. Zero for an entity with no dynamic body.
    #[must_use]
    pub fn body_angular_velocity(&self, entity: Uuid) -> Vec3 {
        match self.dynamic_body_id(entity) {
            Some(id) => Vec3::from_array(sys::body_angular_velocity(&self.world, id)),
            None => Vec3::ZERO,
        }
    }

    /// Cast a ray `origin + dir * max_dist` against the live world and return the closest hit,
    /// mapped back to its owner entity. Read-only: it takes `&self` so it cannot perturb the
    /// deterministic step — run it between steps (a command, or `on_update`), never mid-solve.
    /// `dir` is taken as supplied (not normalized): the hit `distance` is `fraction * max_dist` in
    /// `dir` units. A ray into empty space returns [`RayHit::default`] (`hit == false`).
    #[must_use]
    pub fn raycast(&self, origin: Vec3, dir: Vec3, max_dist: f32) -> RayHit {
        let hit = sys::raycast(&self.world, origin.to_array(), dir.to_array(), max_dist);
        self.map_ray_hit(hit)
    }

    /// Sweep a sphere of `radius` along `origin + dir * max_dist` against the live world — a
    /// thicker probe than [`World::raycast`], so it catches an edge a thin ray of the same
    /// origin/dir grazes — and return the closest hit mapped back to its owner entity. Read-only
    /// (`&self`). A sweep that clears everything returns [`RayHit::default`].
    #[must_use]
    pub fn sphere_cast(&self, origin: Vec3, dir: Vec3, radius: f32, max_dist: f32) -> RayHit {
        let hit = sys::sphere_cast(
            &self.world,
            origin.to_array(),
            dir.to_array(),
            radius,
            max_dist,
        );
        self.map_ray_hit(hit)
    }

    /// Convert a `-sys` [`sys::RayHit`] (POD with a raw `BodyID`) into the public [`RayHit`],
    /// mapping the struck body back to its owner entity uuid via `index_by_body_id`. An unmapped
    /// body (or a miss) yields `Uuid(0)`.
    fn map_ray_hit(&self, hit: sys::RayHit) -> RayHit {
        if !hit.hit {
            return RayHit::default();
        }
        RayHit {
            hit: true,
            target: self.body_target(hit.body),
            point: Vec3::from_array(hit.point),
            normal: Vec3::from_array(hit.normal),
            distance: hit.distance,
        }
    }

    /// Map a raw `BodyID` back to its tagged owner (`None` for an unmapped body). The query
    /// hits return a raw Jolt `BodyID`; the safe layer owns the body → target registry.
    fn body_target(&self, id: u32) -> Option<crate::WorldHitTarget> {
        self.index_by_body_id
            .get(&id)
            .map(|&i| self.bodies[i].target)
    }

    /// The raw `BodyID` of the Dynamic body owned by `uuid`, or `None`. Impulses/velocity apply
    /// only to Dynamic bodies — a Static/Kinematic one would silently ignore them, so it is
    /// excluded here.
    fn dynamic_body_id(&self, uuid: Uuid) -> Option<u32> {
        self.bodies
            .iter()
            .find(|e| {
                e.target == crate::WorldHitTarget::SceneEntity(uuid)
                    && e.motion == MotionType::Dynamic
            })
            .map(|e| e.id)
    }
}
