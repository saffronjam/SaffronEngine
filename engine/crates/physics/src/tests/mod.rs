//! Real-Jolt behaviour tests over the safe wrapper, sharing the rig and cook fixtures below.

mod character;
mod contacts;
mod dynamics;
mod kinematic;
mod queries;
mod ragdoll;
mod shapes;

use super::*;
use glam::{Quat, Vec3};
use saffron_animation::JointPose;
use saffron_core::Uuid;
use saffron_geometry::{Mesh, Vertex};
use saffron_scene::{
    BonePhysics, BonePhysicsComponent, CharacterController, Collider, Entity, IdComponent, Joint,
    KinematicBones, Mesh as MeshComponent, Motion, PoseOverride, Relationship, Rigidbody, Scene,
    Shape, SkinnedMesh, Transform,
};

/// A unit cube mesh (`±0.5` on every axis) as the cook source for the ConvexHull/Mesh shape
/// tests. The 8 corner vertices feed the convex hull; the 12 triangles (2 per face) feed the
/// triangle mesh. CCW winding is irrelevant to Jolt collision, so a simple fan per face.
fn unit_cube() -> Mesh {
    let corners = [
        Vec3::new(-0.5, -0.5, -0.5),
        Vec3::new(0.5, -0.5, -0.5),
        Vec3::new(0.5, 0.5, -0.5),
        Vec3::new(-0.5, 0.5, -0.5),
        Vec3::new(-0.5, -0.5, 0.5),
        Vec3::new(0.5, -0.5, 0.5),
        Vec3::new(0.5, 0.5, 0.5),
        Vec3::new(-0.5, 0.5, 0.5),
    ];
    let vertices = corners
        .iter()
        .map(|&position| Vertex {
            position,
            ..Vertex::default()
        })
        .collect();
    // 6 quads → 12 triangles, indices into the corner list.
    #[rustfmt::skip]
    let indices = vec![
        0, 1, 2, 0, 2, 3, // -z
        4, 6, 5, 4, 7, 6, // +z
        0, 4, 5, 0, 5, 1, // -y
        3, 2, 6, 3, 6, 7, // +y
        0, 3, 7, 0, 7, 4, // -x
        1, 5, 6, 1, 6, 2, // +x
    ];
    Mesh {
        vertices,
        indices,
        submeshes: Vec::new(),
    }
}

/// A cook that always returns a unit cube, for the ConvexHull shape + autofit tests.
fn cube_cook(_: Uuid) -> std::result::Result<Mesh, String> {
    Ok(unit_cube())
}

/// A flat horizontal quad at y = 0 spanning x,z ∈ `[-0.5, 0.5]`, wound CCW seen from above so
/// its collision normal faces +y — a single catch surface for the static-`Mesh`-floor test (a
/// closed cube's triangle winding would let a box tunnel through one face onto another).
fn flat_quad() -> Mesh {
    let corners = [
        Vec3::new(-0.5, 0.0, -0.5),
        Vec3::new(0.5, 0.0, -0.5),
        Vec3::new(0.5, 0.0, 0.5),
        Vec3::new(-0.5, 0.0, 0.5),
    ];
    let vertices = corners
        .iter()
        .map(|&position| Vertex {
            position,
            ..Vertex::default()
        })
        .collect();
    Mesh {
        vertices,
        indices: vec![0, 2, 1, 0, 3, 2],
        submeshes: Vec::new(),
    }
}

/// A cook that always returns the flat floor quad.
fn quad_cook(_: Uuid) -> std::result::Result<Mesh, String> {
    Ok(flat_quad())
}

// Jolt's `Factory::sInstance` is a process-global the world bring-up touches through
// `sys::init`; serialize the tests that build a world so they never race it.
static JOLT_GLOBAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Acquire the global serialization lock, recovering from a poisoned mutex: the lock only
/// guards the Jolt-global init race, so a panic in an earlier test leaves no shared state to
/// be corrupted — recovering keeps one failing test from cascading false failures.
fn jolt_guard() -> std::sync::MutexGuard<'static, ()> {
    JOLT_GLOBAL.lock().unwrap_or_else(|p| p.into_inner())
}

/// A no-op mesh cook — the analytic-shape populate paths never call it, but `populate`
/// requires the seam.
fn no_cook(_: Uuid) -> std::result::Result<saffron_geometry::Mesh, String> {
    Ok(saffron_geometry::Mesh::default())
}

/// Place an entity's translation and refresh the world-transform cache so the populate walk's
/// `world_translation` reads the intended position.
fn spawn_box(
    scene: &mut Scene,
    name: &str,
    translation: Vec3,
    rigidbody: Option<Rigidbody>,
) -> Uuid {
    let e = scene.create_entity(name);
    scene
        .with_component_mut::<Transform, _>(e, |t| t.translation = translation)
        .unwrap();
    // A unit box (half-extents 0.5) is the component default.
    scene.add_component(e, Collider::default()).unwrap();
    if let Some(rb) = rigidbody {
        scene.add_component(e, rb).unwrap();
    }
    scene.relink_hierarchy();
    scene.update_world_transforms();
    scene.component::<IdComponent>(e).unwrap().id
}

/// The y of an entity's local transform, by uuid.
fn body_y(scene: &Scene, uuid: Uuid) -> f32 {
    let e = scene.find_entity_by_uuid(uuid).unwrap();
    scene.component::<Transform>(e).unwrap().translation.y
}

/// Spawn a static box collider of the given half-extents at `translation` (a floor/ledge).
fn spawn_static_box(scene: &mut Scene, name: &str, translation: Vec3, half: Vec3) -> Entity {
    let e = scene.create_entity(name);
    scene
        .with_component_mut::<Transform, _>(e, |t| t.translation = translation)
        .unwrap();
    scene
        .add_component(
            e,
            Collider {
                half_extents: half,
                ..Collider::default()
            },
        )
        .unwrap();
    e
}

/// Spawn a `CharacterController` entity: a capsule collider (radius `r`, half-height `hh`) plus
/// the controller component, placed at `translation`.
fn spawn_character(
    scene: &mut Scene,
    translation: Vec3,
    controller: CharacterController,
) -> Entity {
    let e = scene.create_entity("Character");
    scene
        .with_component_mut::<Transform, _>(e, |t| t.translation = translation)
        .unwrap();
    scene
        .add_component(
            e,
            Collider {
                shape: Shape::Capsule,
                half_extents: Vec3::new(0.3, 0.6, 0.3),
                ..Collider::default()
            },
        )
        .unwrap();
    scene.add_component(e, controller).unwrap();
    e
}

/// Set a character's desired horizontal velocity (the controller component the step loop reads).
fn set_desired_velocity(scene: &mut Scene, character: Entity, velocity: Vec3) {
    scene
        .with_component_mut::<CharacterController, _>(character, |c| {
            c.desired_velocity = velocity;
        })
        .unwrap();
}

/// Build a simple `count`-bone chain rig (root + children stacked along +y) with a
/// `SkinnedMesh` + `BonePhysicsComponent`, returning the rig entity and its uuid. Each bone is
/// a child of the previous, so the ragdoll builds a real parent-constraint chain.
fn spawn_chain_rig(scene: &mut Scene, count: usize) -> (Entity, Uuid) {
    // The rig entity carries the SkinnedMesh + BonePhysics sidecar.
    let rig = scene.create_entity("Rig");
    // Bone entities, each one unit higher and parented to the previous.
    let mut bone_uuids = Vec::with_capacity(count);
    let mut prev: Option<Entity> = None;
    for i in 0..count {
        let bone = scene.create_entity(format!("Bone{i}"));
        scene
            .with_component_mut::<Transform, _>(bone, |t| {
                // Local +y offset from the parent so the chain stacks vertically.
                t.translation = Vec3::new(0.0, if i == 0 { 1.0 } else { 0.5 }, 0.0);
            })
            .unwrap();
        if let Some(parent) = prev {
            let parent_uuid = scene.component::<IdComponent>(parent).unwrap().id;
            scene
                .with_component_mut::<Relationship, _>(bone, |r| r.parent = parent_uuid)
                .unwrap();
        }
        bone_uuids.push(scene.component::<IdComponent>(bone).unwrap().id);
        prev = Some(bone);
    }
    scene
        .add_component(
            rig,
            SkinnedMesh {
                bones: bone_uuids,
                ..SkinnedMesh::default()
            },
        )
        .unwrap();
    scene
        .add_component(
            rig,
            BonePhysicsComponent {
                bones: vec![
                    BonePhysics {
                        shape_half_extents: Vec3::new(0.1, 0.25, 0.1),
                        mass: 1.0,
                        joint: Joint::SwingTwist,
                        swing_twist_limits: Vec3::splat(0.5),
                        ..BonePhysics::default()
                    };
                    count
                ],
            },
        )
        .unwrap();
    scene.relink_hierarchy();
    scene.update_world_transforms();
    let rig_uuid = scene.component::<IdComponent>(rig).unwrap().id;
    (rig, rig_uuid)
}

/// Spawn a dynamic body of an arbitrary shape at `translation`, returning its uuid.
fn spawn_dynamic_shape(
    scene: &mut Scene,
    name: &str,
    translation: Vec3,
    collider: Collider,
) -> Uuid {
    let e = scene.create_entity(name);
    scene
        .with_component_mut::<Transform, _>(e, |t| t.translation = translation)
        .unwrap();
    scene.add_component(e, collider).unwrap();
    scene
        .add_component(
            e,
            Rigidbody {
                motion: Motion::Dynamic,
                ..Rigidbody::default()
            },
        )
        .unwrap();
    scene.relink_hierarchy();
    scene.update_world_transforms();
    scene.component::<IdComponent>(e).unwrap().id
}

/// Spawn a static sensor volume (a lone collider with `is_sensor`, no rigidbody → Static body
/// in the Sensor layer) of the given half-extents at `translation`, returning its uuid.
fn spawn_sensor_box(scene: &mut Scene, name: &str, translation: Vec3, half: Vec3) -> Uuid {
    let e = scene.create_entity(name);
    scene
        .with_component_mut::<Transform, _>(e, |t| t.translation = translation)
        .unwrap();
    scene
        .add_component(
            e,
            Collider {
                half_extents: half,
                is_sensor: true,
                ..Collider::default()
            },
        )
        .unwrap();
    scene.relink_hierarchy();
    scene.update_world_transforms();
    scene.component::<IdComponent>(e).unwrap().id
}

/// One axis of an entity's local translation, by uuid (`axis`: 0 = x, 1 = y, 2 = z).
fn body_y_axis(scene: &Scene, uuid: Uuid, axis: usize) -> f32 {
    let e = scene.find_entity_by_uuid(uuid).unwrap();
    scene.component::<Transform>(e).unwrap().translation[axis]
}

/// Build a kinematic-bones rig of `count` independent joint entities (each a direct child of
/// the rig, so each joint's world pose is the rig translation + its own local translation),
/// carrying a `SkinnedMesh` + `BonePhysicsComponent` of capsules and a `KinematicBones`
/// component with the given `driven` list. Returns the rig entity and its per-joint entities in
/// bone order. Unlike `spawn_chain_rig`, the joints are siblings so a test can move one without
/// dragging the others.
fn spawn_kinematic_rig(
    scene: &mut Scene,
    joint_locals: &[Vec3],
    driven: Vec<i32>,
) -> (Entity, Vec<Entity>) {
    let count = joint_locals.len();
    let rig = scene.create_entity("KinRig");
    let rig_uuid = scene.component::<IdComponent>(rig).unwrap().id;

    let mut bone_uuids = Vec::with_capacity(count);
    let mut bone_entities = Vec::with_capacity(count);
    for (i, &local) in joint_locals.iter().enumerate() {
        let bone = scene.create_entity(format!("Joint{i}"));
        scene
            .with_component_mut::<Transform, _>(bone, |t| t.translation = local)
            .unwrap();
        scene
            .with_component_mut::<Relationship, _>(bone, |r| r.parent = rig_uuid)
            .unwrap();
        bone_uuids.push(scene.component::<IdComponent>(bone).unwrap().id);
        bone_entities.push(bone);
    }
    scene
        .add_component(
            rig,
            SkinnedMesh {
                bones: bone_uuids,
                ..SkinnedMesh::default()
            },
        )
        .unwrap();
    scene
        .add_component(
            rig,
            BonePhysicsComponent {
                bones: vec![
                    BonePhysics {
                        shape_half_extents: Vec3::new(0.25, 0.25, 0.25),
                        ..BonePhysics::default()
                    };
                    count
                ],
            },
        )
        .unwrap();
    scene
        .add_component(
            rig,
            KinematicBones {
                enabled: true,
                driven,
            },
        )
        .unwrap();
    scene.relink_hierarchy();
    scene.update_world_transforms();
    (rig, bone_entities)
}

/// The unsigned angle (radians) between two unit quaternions, accounting for the double cover.
fn quat_angle(a: Quat, b: Quat) -> f32 {
    a.normalize().angle_between(b.normalize())
}

/// Read a bone's current [`PoseOverride`] rotation, or identity when the bone carries none.
fn bone_override_rotation(scene: &Scene, bone: Entity) -> Quat {
    scene
        .component::<PoseOverride>(bone)
        .map(|p| p.rotation)
        .unwrap_or(Quat::IDENTITY)
}

/// A rig's bone entity handles in bone order, for reading the written `PoseOverride`s.
fn bone_handles(scene: &Scene, rig: Entity) -> Vec<Entity> {
    scene
        .with_component::<SkinnedMesh, _>(rig, |s| s.bone_handles.clone())
        .unwrap_or_default()
}
