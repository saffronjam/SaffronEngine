//! Body creation from the scene's collider and rigidbody components.

use std::collections::HashSet;

use glam::{Quat, Vec3};

use saffron_physics_sys::{self as sys, BodyCreate, INVALID_BODY_ID};
use saffron_scene::{
    BonePhysicsComponent, Collider, Entity, KinematicBones, Rigidbody, Scene, Shape, SkinnedMesh,
};

use crate::error::{Error, Result};
use crate::types::{MotionType, ObjectLayer, StaticTargetBodyCreate};

use super::*;

/// The Jolt `EAllowedDOFs::All` bitmask: all six translation+rotation axes free.
const ALLOWED_DOFS_ALL: u8 = 0b0011_1111;
const DOF_TRANSLATION_X: u8 = 0b0000_0001;
const DOF_TRANSLATION_Y: u8 = 0b0000_0010;
const DOF_TRANSLATION_Z: u8 = 0b0000_0100;
const DOF_ROTATION_X: u8 = 0b0000_1000;
const DOF_ROTATION_Y: u8 = 0b0001_0000;
const DOF_ROTATION_Z: u8 = 0b0010_0000;

impl World {
    /// Walk the scene's colliders and create a body for each, in deterministic entity-iteration
    /// order. Builds all five shapes: the analytic Box/Sphere/Capsule size from `half_extents`,
    /// and ConvexHull/Mesh cook their `source_mesh` `.smesh` through `cook` (vertices/indices fed
    /// in index order for a reproducible cooked shape). A per-collider failure — a `Mesh` on a
    /// Dynamic body, a missing cook source, a cook error, or a degenerate shape — is logged and
    /// the body skipped; the world still builds.
    pub fn populate(&mut self, scene: &mut Scene, cook: &mut MeshCook<'_>) {
        // Gather the collider rows first so the body-creation loop can borrow `scene` immutably for
        // the world-pose composition (the `for_each` closure holds a mutable borrow).
        let mut rows: Vec<(saffron_scene::Entity, Collider)> = Vec::new();
        scene.for_each::<&Collider, _>(|entity, collider| {
            rows.push((entity, *collider));
        });

        for (entity, _) in rows {
            if let Err(err) = self.add_entity_body(scene, entity, cook) {
                tracing::warn!(
                    "physics: skipping body for {}: {err}",
                    id_of(scene, entity).0
                );
            }
        }
    }

    /// Create the one body `entity`'s [`Collider`] (plus any [`Rigidbody`]) describes, registered
    /// under the entity's stable uuid, and return its raw `BodyID`. The per-entity path
    /// [`World::populate`] walks, and the way an entity that appears mid-play (a promoted macro
    /// plant) gains collision.
    ///
    /// # Errors
    ///
    /// [`Error::MissingCollider`] when the entity carries no collider or owns a
    /// [`CharacterController`](saffron_scene::CharacterController) instead (its capsule is a
    /// `CharacterVirtual`, never a world body — a static body there would block the sweep), the
    /// cook error for a `ConvexHull`/`Mesh` shape that could not be built, or
    /// [`Error::BodyCreate`] when Jolt rejected the shape or hit its body limit.
    pub fn add_entity_body(
        &mut self,
        scene: &Scene,
        entity: Entity,
        cook: &mut MeshCook<'_>,
    ) -> Result<u32> {
        if scene.has_component::<saffron_scene::CharacterController>(entity) {
            return Err(Error::MissingCollider);
        }
        let collider = scene
            .component::<Collider>(entity)
            .map_err(|_| Error::MissingCollider)?;

        // A collider with no rigidbody is an implicit Static body; with one, its motion wins.
        let rigidbody = scene.component::<Rigidbody>(entity).ok();
        let motion = rigidbody
            .map(|rb| MotionType::from_scene(rb.motion))
            .unwrap_or(MotionType::Static);

        // Cook the ConvexHull/Mesh geometry (and reject a Mesh on a Dynamic body) before touching
        // Jolt, so a typed cause reaches the caller.
        let geometry = cook_shape_geometry(&collider, motion, cook)?;

        // World translation/rotation compose on a cache miss (the play scene's caches may be cold
        // here), scale-free.
        let position = scene.world_translation(entity);
        let rotation = scene.world_rotation(entity);
        let object_layer = resolve_object_layer(rigidbody.as_ref(), motion, collider.is_sensor);

        let create = body_create(
            &collider,
            rigidbody.as_ref(),
            motion,
            object_layer,
            position,
            rotation,
        );
        let id = sys::create_body(
            &mut self.world,
            &create,
            &geometry.hull_points,
            &geometry.mesh_vertices,
            &geometry.mesh_indices,
        );
        if id == INVALID_BODY_ID {
            // The shim already logged the shape/body create failure.
            return Err(Error::BodyCreate);
        }

        let target = crate::WorldHitTarget::SceneEntity(id_of(scene, entity));
        self.index_by_body_id.insert(id, self.bodies.len());
        self.bodies.push(BodyEntry {
            entity,
            target,
            id,
            motion,
            sensor: collider.is_sensor,
            drag_area: wind_drag_area(&collider, rigidbody.as_ref()),
        });
        if motion == MotionType::Dynamic {
            self.dynamic_body_count += 1;
        }
        Ok(id)
    }

    /// Remove and destroy every body registered to `entity`, in one batch. The counterpart of
    /// [`World::add_entity_body`] for an entity that leaves mid-play (a demoted macro plant).
    pub fn remove_entity_bodies(&mut self, entity: Entity) {
        let ids: Vec<u32> = self
            .bodies
            .iter()
            .filter(|body| body.entity == entity)
            .map(|body| body.id)
            .collect();
        self.remove_bodies(&ids);
    }

    /// Create one Kinematic capsule body per driven joint of every enabled
    /// [`KinematicBones`] rig, so the animated pose shoves the dynamic world via
    /// `MoveKinematic` each step (binding mode b, animation→physics, no pose write-back). A rig is
    /// skipped when its bones are disabled or it has no [`SkinnedMesh`]; the `driven` list selects
    /// the joints (empty = every joint). Each capsule is sized from the matching
    /// [`BonePhysics::shape_half_extents`](saffron_scene::BonePhysics::shape_half_extents) (radius
    /// `.x`, half-height `.y`, with a `0.03` floor so a
    /// leaf/unfitted bone is never a degenerate capsule), seeded at the joint's fresh world pose,
    /// on the Moving layer. The bodies join `bodies` keyed by the joint entity in creation order
    /// and tear down with the world.
    pub fn build_bone_bodies(&mut self, scene: &mut Scene) {
        // Gather the enabled rigs first so the body-creation loop can read `scene` immutably for
        // the per-joint world-pose composition (the `for_each` closure holds a mutable borrow).
        let mut rigs: Vec<(Entity, Vec<i32>)> = Vec::new();
        scene.for_each::<&KinematicBones, _>(|rig, bones| {
            if bones.enabled {
                rigs.push((rig, bones.driven.clone()));
            }
        });

        for (rig, driven) in rigs {
            if !scene.has_component::<SkinnedMesh>(rig) {
                continue;
            }
            let bone_handles = scene
                .with_component::<SkinnedMesh, _>(rig, |s| s.bone_handles.clone())
                .unwrap_or_default();
            let bones = scene
                .with_component::<BonePhysicsComponent, _>(rig, |p| p.bones.clone())
                .ok();

            for (index, &joint) in bone_handles.iter().enumerate() {
                if !is_driven(&driven, index) || !scene.valid(joint) {
                    continue;
                }
                // Capsule from the per-bone shape_half_extents (radius .x, half-height .y), Y-up;
                // a small default for a leaf/unfitted bone so Jolt never rejects a degenerate one.
                let extents = bones
                    .as_ref()
                    .and_then(|b| b.get(index))
                    .map_or(Vec3::ZERO, |b| b.shape_half_extents);
                let (position, rotation) = fresh_world_pose(scene, joint);
                let create = BodyCreate {
                    shape: shape_raw(Shape::Capsule),
                    half_extents: [extents.x.max(0.03), extents.y.max(0.03), extents.z],
                    offset: [0.0; 3],
                    position: position.to_array(),
                    rotation: rotation.to_array(),
                    motion: MotionType::Kinematic.raw(),
                    object_layer: ObjectLayer::Moving.raw(),
                    is_sensor: false,
                    friction: 0.2,
                    restitution: 0.0,
                    linear_damping: 0.0,
                    angular_damping: 0.0,
                    gravity_factor: 1.0,
                    mass: 1.0,
                    allowed_dofs: ALLOWED_DOFS_ALL,
                };
                let id = sys::create_body(&mut self.world, &create, &[], &[], &[]);
                if id == INVALID_BODY_ID {
                    continue;
                }
                let uuid = id_of(scene, joint);
                let target = crate::WorldHitTarget::SceneEntity(uuid);
                self.index_by_body_id.insert(id, self.bodies.len());
                self.bodies.push(BodyEntry {
                    entity: joint,
                    target,
                    id,
                    motion: MotionType::Kinematic,
                    sensor: false,
                    drag_area: 0.0,
                });
            }
        }
    }

    /// Create one batch of static tagged-target bodies through Jolt's batched broadphase
    /// insertion, registering each created body under its row's
    /// [`WorldHitTarget`](crate::WorldHitTarget) so queries
    /// and contacts report the tagged owner. Returns one raw `BodyID` per input row,
    /// position-aligned; a failed row yields [`INVALID_BODY_ID`] and registers nothing.
    pub fn add_static_target_bodies(&mut self, rows: &[StaticTargetBodyCreate]) -> Vec<u32> {
        let creates: Vec<BodyCreate> = rows
            .iter()
            .map(|row| BodyCreate {
                shape: shape_raw(row.shape),
                half_extents: row.half_extents.to_array(),
                offset: [0.0; 3],
                position: row.position.to_array(),
                rotation: row.rotation.to_array(),
                motion: MotionType::Static.raw(),
                object_layer: if row.sensor {
                    ObjectLayer::Sensor
                } else {
                    ObjectLayer::Static
                }
                .raw(),
                is_sensor: row.sensor,
                friction: row.friction,
                restitution: 0.0,
                linear_damping: 0.0,
                angular_damping: 0.0,
                gravity_factor: 1.0,
                mass: 1.0,
                allowed_dofs: ALLOWED_DOFS_ALL,
            })
            .collect();
        let ids = sys::create_static_batch(&mut self.world, &creates);
        for (row, &id) in rows.iter().zip(&ids) {
            if id == INVALID_BODY_ID {
                continue;
            }
            self.index_by_body_id.insert(id, self.bodies.len());
            self.bodies.push(BodyEntry {
                entity: saffron_scene::Entity::NULL,
                target: row.target,
                id,
                motion: MotionType::Static,
                sensor: row.sensor,
                drag_area: 0.0,
            });
        }
        ids
    }

    /// Remove and destroy the listed bodies in one batch, dropping their registry rows.
    /// [`INVALID_BODY_ID`] sentinels are skipped. A contact already buffered for a removed body
    /// drains with a `None` target.
    pub fn remove_bodies(&mut self, ids: &[u32]) {
        let removed: HashSet<u32> = ids
            .iter()
            .copied()
            .filter(|&id| id != INVALID_BODY_ID)
            .collect();
        if removed.is_empty() {
            return;
        }
        sys::remove_bodies(&mut self.world, ids);
        self.dynamic_body_count -= self
            .bodies
            .iter()
            .filter(|entry| removed.contains(&entry.id) && entry.motion == MotionType::Dynamic)
            .count() as i32;
        self.bodies.retain(|entry| !removed.contains(&entry.id));
        self.index_by_body_id.clear();
        for (index, entry) in self.bodies.iter().enumerate() {
            self.index_by_body_id.insert(entry.id, index);
        }
    }
}

/// Resolve a body's object layer. Precedence: sensor > the moving slot the rigidbody's
/// `collision_layer` selects (0 = Moving, 1 = Character, 2 = Debris) > implicit Static (a lone
/// collider or an explicit Static rigidbody).
fn resolve_object_layer(
    rigidbody: Option<&Rigidbody>,
    motion: MotionType,
    is_sensor: bool,
) -> ObjectLayer {
    if is_sensor {
        return ObjectLayer::Sensor;
    }
    match rigidbody {
        Some(rb) if motion != MotionType::Static => match rb.collision_layer {
            1 => ObjectLayer::Character,
            2 => ObjectLayer::Debris,
            _ => ObjectLayer::Moving, // 0 = default moving; unknown clamps to Moving
        },
        _ => ObjectLayer::Static,
    }
}

/// The body's allowed degrees of freedom from its per-axis position/rotation locks, as the Jolt
/// `EAllowedDOFs` bitmask.
fn allowed_dofs(rb: &Rigidbody) -> u8 {
    let mut dofs = ALLOWED_DOFS_ALL;
    if rb.lock_position.x {
        dofs &= !DOF_TRANSLATION_X;
    }
    if rb.lock_position.y {
        dofs &= !DOF_TRANSLATION_Y;
    }
    if rb.lock_position.z {
        dofs &= !DOF_TRANSLATION_Z;
    }
    if rb.lock_rotation.x {
        dofs &= !DOF_ROTATION_X;
    }
    if rb.lock_rotation.y {
        dofs &= !DOF_ROTATION_Y;
    }
    if rb.lock_rotation.z {
        dofs &= !DOF_ROTATION_Z;
    }
    dofs
}

/// The cross-section in square metres the wind pushes this body on: the collider's own mean
/// axis-aligned face area, scaled by the body's authored [`Rigidbody::wind_factor`]. A body with
/// no rigidbody, or one left at the default factor, returns zero and the step skips it.
///
/// The geometric term is the mean of the three face areas of the box the half extents describe,
/// which is what a tumbling body presents on average. A sphere's extents carry the radius in `x`
/// and a capsule's the radius in `x` and the half-height in `y`, so each is squared out to the
/// box it inscribes first.
fn wind_drag_area(collider: &Collider, rigidbody: Option<&Rigidbody>) -> f32 {
    let factor = rigidbody.map_or(0.0, |rb| rb.wind_factor);
    if factor <= 0.0 {
        return 0.0;
    }
    let extents = match collider.shape {
        Shape::Sphere => Vec3::splat(collider.half_extents.x),
        Shape::Capsule => Vec3::new(
            collider.half_extents.x,
            collider.half_extents.y,
            collider.half_extents.x,
        ),
        Shape::Box | Shape::ConvexHull | Shape::Mesh => collider.half_extents,
    }
    .abs();
    factor * 4.0 * (extents.x * extents.y + extents.y * extents.z + extents.z * extents.x) / 3.0
}

/// The raw shape discriminant the bridge's `BodyCreate.shape` carries, mapping the scene
/// [`Shape`] enum to the shim's switch (`0` Box, `1` Sphere, `2` Capsule, `3` ConvexHull,
/// `4` Mesh). The shim's `Shape` enum is declared in the same order.
fn shape_raw(shape: Shape) -> u8 {
    match shape {
        Shape::Box => 0,
        Shape::Sphere => 1,
        Shape::Capsule => 2,
        Shape::ConvexHull => 3,
        Shape::Mesh => 4,
    }
}

/// The cooked geometry a ConvexHull/Mesh body is built from, flattened to the index-ordered slices
/// the bridge feeds Jolt. Empty for the analytic shapes (Box/Sphere/Capsule).
#[derive(Debug, Default)]
pub(crate) struct CookedGeometry {
    /// ConvexHull points, flattened `xyz` in index order.
    hull_points: Vec<f32>,
    /// Mesh vertex positions, flattened `xyz` in index order.
    mesh_vertices: Vec<f32>,
    /// Mesh triangle indices (flat).
    mesh_indices: Vec<u32>,
}

/// Resolve the cooked geometry a collider needs before its Jolt body is built. Analytic shapes
/// need none (an empty [`CookedGeometry`]); ConvexHull/Mesh cook their `source_mesh` through
/// `cook`, feeding vertices/indices in index order so the cooked shape is reproducible run-to-run.
/// A ConvexHull/Mesh with no `source_mesh` is an [`Error::NoCookSource`]; a `Mesh` shape on a
/// Dynamic body is rejected outright (Jolt's `MeshShape` is Static/Kinematic only).
pub(crate) fn cook_shape_geometry(
    collider: &Collider,
    motion: MotionType,
    cook: &mut MeshCook<'_>,
) -> Result<CookedGeometry> {
    match collider.shape {
        Shape::Box | Shape::Sphere | Shape::Capsule => Ok(CookedGeometry::default()),
        Shape::ConvexHull => {
            if collider.source_mesh.0 == 0 {
                return Err(Error::NoCookSource);
            }
            let mesh = cook(collider.source_mesh).map_err(Error::CookFailed)?;
            let mut hull_points = Vec::with_capacity(mesh.vertices.len() * 3);
            for vertex in &mesh.vertices {
                // index order — stable for determinism
                hull_points.extend_from_slice(&vertex.position.to_array());
            }
            if hull_points.is_empty() {
                return Err(Error::CookFailed(
                    "convex-hull source mesh has no vertices".to_owned(),
                ));
            }
            Ok(CookedGeometry {
                hull_points,
                ..CookedGeometry::default()
            })
        }
        Shape::Mesh => {
            if motion == MotionType::Dynamic {
                return Err(Error::MeshShapeOnDynamic);
            }
            if collider.source_mesh.0 == 0 {
                return Err(Error::NoCookSource);
            }
            let mesh = cook(collider.source_mesh).map_err(Error::CookFailed)?;
            let mut mesh_vertices = Vec::with_capacity(mesh.vertices.len() * 3);
            for vertex in &mesh.vertices {
                mesh_vertices.extend_from_slice(&vertex.position.to_array());
            }
            if mesh.indices.len() < 3 {
                return Err(Error::CookFailed("mesh source has no triangles".to_owned()));
            }
            Ok(CookedGeometry {
                mesh_vertices,
                mesh_indices: mesh.indices.clone(),
                ..CookedGeometry::default()
            })
        }
    }
}

/// Flatten a collider + rigidbody into the bridge's `BodyCreate` POD. The damping/mass/DOF fields
/// only matter for a Dynamic body (the shim ignores them otherwise), but they are always filled so
/// the struct is fully initialized.
fn body_create(
    collider: &Collider,
    rigidbody: Option<&Rigidbody>,
    motion: MotionType,
    object_layer: ObjectLayer,
    position: Vec3,
    rotation: Quat,
) -> BodyCreate {
    let dynamic = rigidbody.filter(|_| motion == MotionType::Dynamic);
    BodyCreate {
        shape: shape_raw(collider.shape),
        half_extents: collider.half_extents.to_array(),
        offset: collider.offset.to_array(),
        position: position.to_array(),
        rotation: rotation.to_array(),
        motion: motion.raw(),
        object_layer: object_layer.raw(),
        is_sensor: collider.is_sensor,
        friction: collider.material.friction,
        restitution: collider.material.restitution,
        linear_damping: dynamic.map(|rb| rb.linear_damping).unwrap_or(0.0),
        angular_damping: dynamic.map(|rb| rb.angular_damping).unwrap_or(0.0),
        gravity_factor: dynamic.map(|rb| rb.gravity_factor).unwrap_or(1.0),
        mass: dynamic.map(|rb| rb.mass).unwrap_or(1.0),
        allowed_dofs: dynamic.map(allowed_dofs).unwrap_or(ALLOWED_DOFS_ALL),
    }
}
