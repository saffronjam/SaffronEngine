//! The safe Jolt wrapper above `saffron-physics-sys`: the per-play [`World`], body creation from
//! the scene's collider/rigidbody components, the deterministic fixed-step loop with dynamic
//! transform write-back, and the read-only + impulse/force/velocity surface.
//!
//! The `unsafe` Jolt boundary lives entirely in `saffron-physics-sys`; this crate holds
//! `#![deny(unsafe_code)]` and speaks only safe Rust + the POD bridge. There is one [`World`] type
//! and one code path per operation — mutators take `&mut World`, so "no world" is a type-level
//! impossibility (the `Option<World>` lives in the host).
//!
//! The world builds all five collider shapes (Box/Sphere/Capsule analytic + ConvexHull/Mesh cooked
//! through the [`MeshCook`] seam), drives the deterministic fixed-step loop with dynamic transform
//! write-back, the read-only/mutator surface, the character controller, the collider +
//! bone-capsule auto-fit, the sensor/trigger layer feeding the seq-stamped contact ring
//! ([`World::drain_contacts`]), and kinematic bone bodies that follow the animated pose via
//! `MoveKinematic` ([`World::build_bone_bodies`]). The full passive/active/partial ragdoll blend
//! layer composes per fixed step ([`World::drive_ragdolls_to_pose`] →
//! [`World::advance_ragdoll_blend`] → [`World::step`] → [`World::write_ragdoll_poses`], with
//! [`World::set_ragdoll_blend`]/[`World::ragdoll_state`] controlling it). The read-only spatial
//! queries ([`World::raycast`] / [`World::sphere_cast`]) close the public surface; they take
//! `&self` so a query can never perturb the deterministic step, and the host bridges them to the
//! `sa.raycast` / `sa.spherecast` script seam (the conversion documented on [`RayHit`]).
//!
//! Depends on `saffron-core`, `saffron-geometry`, `saffron-scene`, `saffron-physics-sys`.

#![deny(unsafe_code)]

mod error;
mod types;
mod world;

pub use error::{Error, Result};
pub use saffron_physics_sys::INVALID_BODY_ID;
pub use types::{
    BodyInfo, CONTACT_RING_CAP, ContactDrain, ContactEvent, ContactKind, FIXED_STEP, MotionType,
    ObjectLayer, PoseTarget, RagdollState, RayHit, StaticTargetBodyCreate, WorldHitTarget,
    WorldStats, layers_collide,
};
pub use world::{MeshCook, World, fit_bone_capsules, fit_collider_to_mesh, shutdown_physics};

#[cfg(test)]
mod tests {
    use super::*;
    use glam::{Quat, Vec3};
    use saffron_animation::JointPose;
    use saffron_core::Uuid;
    use saffron_geometry::{Mesh, Vertex};
    use saffron_scene::{
        BonePhysics, BonePhysicsComponent, CharacterController, Collider, Entity, IdComponent,
        Joint, KinematicBones, Mesh as MeshComponent, Motion, PoseOverride, Relationship,
        Rigidbody, Scene, Shape, SkinnedMesh, Transform,
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

    #[test]
    fn box_falls_under_gravity() {
        let _guard = jolt_guard();
        let mut scene = Scene::new();

        // A static floor (a lone collider, default unit box) centred at the origin: its top face
        // sits at y = 0.5.
        let _floor = spawn_box(&mut scene, "Floor", Vec3::ZERO, None);
        // A dynamic box dropped from y = 5.
        let dynamic = Rigidbody {
            motion: Motion::Dynamic,
            ..Rigidbody::default()
        };
        let falling = spawn_box(
            &mut scene,
            "Falling",
            Vec3::new(0.0, 5.0, 0.0),
            Some(dynamic),
        );

        let mut world = World::new().expect("world creation");
        let mut cook = no_cook;
        world.populate(&mut scene, &mut cook);

        let start_y = body_y(&scene, falling);
        assert_eq!(start_y, 5.0, "the box starts at the authored height");

        // A few steps in: gravity must have pulled it below the start.
        for _ in 0..10 {
            world.step(&mut scene, FIXED_STEP);
        }
        let mid_y = body_y(&scene, falling);
        assert!(
            mid_y < start_y,
            "the box fell under gravity ({mid_y} should be < {start_y})"
        );

        // Step well past the contact so the solver settles it on the floor.
        for _ in 0..300 {
            world.step(&mut scene, FIXED_STEP);
        }
        let rest_y = body_y(&scene, falling);
        // Floor top = 0.5, falling box half-height = 0.5, so the resting centre is ~1.0. Allow a
        // small penetration/settle tolerance.
        assert!(
            (rest_y - 1.0).abs() < 0.1,
            "the box came to rest on the floor (rest_y = {rest_y}, expected ~1.0)"
        );
        // And it did not tunnel through.
        assert!(rest_y > 0.5, "the box did not fall through the floor");
    }

    #[test]
    fn impulse_changes_velocity() {
        let _guard = jolt_guard();
        let mut scene = Scene::new();

        let dynamic = Rigidbody {
            motion: Motion::Dynamic,
            // No gravity so the impulse is the only velocity source — a clean assertion.
            gravity_factor: 0.0,
            ..Rigidbody::default()
        };
        let body = spawn_box(&mut scene, "Body", Vec3::new(0.0, 10.0, 0.0), Some(dynamic));

        let mut world = World::new().expect("world creation");
        let mut cook = no_cook;
        world.populate(&mut scene, &mut cook);

        assert_eq!(
            world.body_linear_velocity(body),
            Vec3::ZERO,
            "a fresh body is at rest"
        );

        // A unit box at mass 1 kg: a 5 kg·m/s impulse along +x gives ~5 m/s.
        world.apply_impulse(body, Vec3::new(5.0, 0.0, 0.0));
        let v = world.body_linear_velocity(body);
        assert!(
            (v.x - 5.0).abs() < 1e-3 && v.y.abs() < 1e-3 && v.z.abs() < 1e-3,
            "impulse set the velocity to ~(5,0,0); got {v:?}"
        );

        // A non-Dynamic / unmapped target is a no-op, never a panic.
        let absent = Uuid(123_456);
        world.apply_impulse(absent, Vec3::new(1.0, 0.0, 0.0));
        world.add_force(absent, Vec3::new(1.0, 0.0, 0.0));
        world.set_linear_velocity(absent, Vec3::new(1.0, 0.0, 0.0));
        assert_eq!(
            world.body_linear_velocity(absent),
            Vec3::ZERO,
            "an unmapped body reports zero velocity, no panic"
        );
    }

    #[test]
    fn stats_and_list() {
        let _guard = jolt_guard();
        let mut scene = Scene::new();

        // Two dynamic boxes + one static floor → three bodies, two dynamic, in creation order.
        let dynamic = Rigidbody {
            motion: Motion::Dynamic,
            ..Rigidbody::default()
        };
        let d0 = spawn_box(&mut scene, "D0", Vec3::new(0.0, 3.0, 0.0), Some(dynamic));
        let d1 = spawn_box(&mut scene, "D1", Vec3::new(2.0, 3.0, 0.0), Some(dynamic));
        let floor = spawn_box(&mut scene, "Floor", Vec3::ZERO, None);

        let mut world = World::new().expect("world creation");
        let mut cook = no_cook;
        world.populate(&mut scene, &mut cook);

        let stats = world.stats();
        assert!(stats.active);
        assert_eq!(stats.body_count, 3, "three bodies created");
        assert_eq!(stats.dynamic_count, 2, "two of them dynamic");

        let bodies = world.list_bodies();
        assert_eq!(bodies.len(), 3, "the list has every created body");
        // `for_each` iteration order is unspecified, so assert on contents/motion by uuid rather
        // than positional order; the creation-order invariant is internal to `bodies` and proven
        // by every entry being present exactly once.
        let uuids: Vec<Option<WorldHitTarget>> = bodies.iter().map(|b| b.target).collect();
        for expected in [d0, d1, floor] {
            assert!(
                uuids.contains(&Some(WorldHitTarget::SceneEntity(expected))),
                "body {expected:?} is listed"
            );
        }
        let dynamic_listed = bodies
            .iter()
            .filter(|b| b.motion == MotionType::Dynamic)
            .count();
        assert_eq!(dynamic_listed, 2, "two dynamic bodies in the list");
        let static_listed = bodies
            .iter()
            .filter(|b| b.motion == MotionType::Static)
            .count();
        assert_eq!(static_listed, 1, "one static body in the list");
    }

    #[test]
    fn layer_matrix_pins_v1_policy() {
        // The orchestration-side reference matches the load-bearing rows by name (the shim's copy
        // is asserted in `saffron-physics-sys`; this pins the Rust reference independently).
        assert!(layers_collide(ObjectLayer::Sensor, ObjectLayer::Static));
        assert!(!layers_collide(ObjectLayer::Sensor, ObjectLayer::Sensor));
        assert!(!layers_collide(ObjectLayer::Static, ObjectLayer::Static));
        assert!(!layers_collide(ObjectLayer::Debris, ObjectLayer::Debris));
        assert!(layers_collide(ObjectLayer::Moving, ObjectLayer::Static));
        assert!(layers_collide(ObjectLayer::Debris, ObjectLayer::Character));
    }

    #[test]
    fn character_walks_and_steps() {
        let _guard = jolt_guard();
        let mut scene = Scene::new();

        // A wide floor (top face at y = 0.5) and a wide raised platform at +x whose top is 0.2
        // above the floor — below the controller's 0.3 max step height, so WalkStairs lifts the
        // character. Both are large enough that the character cannot walk off an edge and fall.
        spawn_static_box(&mut scene, "Floor", Vec3::ZERO, Vec3::new(20.0, 0.5, 20.0));
        let ledge_top = 0.5 + 0.2;
        let ledge_half_y = 0.5;
        spawn_static_box(
            &mut scene,
            "Ledge",
            Vec3::new(8.0, ledge_top - ledge_half_y, 0.0),
            Vec3::new(6.0, ledge_half_y, 20.0),
        );

        // The capsule centre rests at floor_top + half_height + radius = 0.5 + 0.6 + 0.3 = 1.4;
        // spawn a touch above and let it settle, walking +x at 3 m/s into the ledge.
        let controller = CharacterController {
            max_speed: 3.0,
            max_step_height: 0.3,
            desired_velocity: Vec3::new(3.0, 0.0, 0.0),
            ..CharacterController::default()
        };
        let character = spawn_character(&mut scene, Vec3::new(0.0, 1.5, 0.0), controller);

        let mut world = World::new().expect("world creation");
        let mut cook = no_cook;
        world.populate(&mut scene, &mut cook);
        world
            .add_character(character, &scene)
            .expect("character creation");

        // Settle on flat ground first (no horizontal drive), then assert grounded. The desired
        // velocity lives on the controller component the step loop reads.
        set_desired_velocity(&mut scene, character, Vec3::ZERO);
        for _ in 0..30 {
            world.step(&mut scene, FIXED_STEP);
        }
        let settled_y = scene
            .component::<Transform>(character)
            .unwrap()
            .translation
            .y;
        assert!(
            scene
                .component::<CharacterController>(character)
                .unwrap()
                .on_ground,
            "the character is grounded on flat floor"
        );
        assert!(
            (settled_y - 1.4).abs() < 0.15,
            "settled on the floor near y = 1.4 (got {settled_y})"
        );

        // Now walk into the ledge; WalkStairs should lift it onto the platform top. 120 substeps at
        // 3 m/s ≈ 6 m of travel — past the platform front edge (x = 2) and well onto it (spans to
        // x = 14), so the character cannot overrun the far edge.
        set_desired_velocity(&mut scene, character, Vec3::new(3.0, 0.0, 0.0));
        for _ in 0..120 {
            world.step(&mut scene, FIXED_STEP);
        }
        let final_y = scene
            .component::<Transform>(character)
            .unwrap()
            .translation
            .y;
        assert!(
            final_y > settled_y + 0.1,
            "the character stepped up onto the ledge (final_y {final_y} > settled_y {settled_y})"
        );
        assert!(
            scene
                .component::<CharacterController>(character)
                .unwrap()
                .on_ground,
            "the character is grounded again on top of the ledge"
        );
    }

    #[test]
    fn passive_ragdoll_falls() {
        let _guard = jolt_guard();
        let mut scene = Scene::new();

        // A 3-bone chain rig in mid-air; no floor, so it falls freely under gravity.
        let (rig, rig_uuid) = spawn_chain_rig(&mut scene, 3);
        // Lift the whole rig high so it falls without hitting anything.
        scene
            .with_component_mut::<Transform, _>(rig, |t| t.translation = Vec3::new(0.0, 5.0, 0.0))
            .unwrap();
        scene.relink_hierarchy();
        scene.update_world_transforms();

        let mut world = World::new().expect("world creation");
        world.enable_ragdoll(&scene, rig).expect("ragdoll build");
        assert!(world.has_ragdoll(rig_uuid), "the rig has a live ragdoll");
        let parts = world.ragdoll_part_count(rig_uuid);
        assert_eq!(parts, 3, "the ragdoll has one part per bone");

        // Record the root part's start height.
        let (start_pos, _) = world
            .ragdoll_part_transform(rig_uuid, 0)
            .expect("root part transform");

        // Step it for half a second of sim.
        for _ in 0..30 {
            world.step(&mut scene, FIXED_STEP);
        }

        // Every part moved under gravity (fell), stayed finite, and is within a bounded
        // displacement (no explosion / joint blow-up).
        let mut min_part_y = f32::INFINITY;
        for part in 0..parts {
            let (pos, rot) = world
                .ragdoll_part_transform(rig_uuid, part)
                .expect("part transform");
            assert!(
                pos.is_finite() && rot.is_finite(),
                "part {part} pose is finite (pos {pos:?}, rot {rot:?})"
            );
            // Half a second of free fall is ~1.2 m; the parts are within a couple metres of the
            // start, never flung away by an unstable constraint.
            assert!(
                (pos - start_pos).length() < 5.0,
                "part {part} stayed within a bounded displacement of the start"
            );
            min_part_y = min_part_y.min(pos.y);
        }
        let (root_after, _) = world
            .ragdoll_part_transform(rig_uuid, 0)
            .expect("root part transform");
        assert!(
            root_after.y < start_pos.y - 0.3,
            "the ragdoll fell under gravity (root y {} < start y {})",
            root_after.y,
            start_pos.y
        );
    }

    #[test]
    fn ragdoll_teardown_clean() {
        let _guard = jolt_guard();
        let mut scene = Scene::new();

        let (rig, rig_uuid) = spawn_chain_rig(&mut scene, 3);

        // Enable a ragdoll, step it once so its bodies are live in the system, then drop the whole
        // world with the ragdoll still attached. The shim's JoltWorld destructor must detach every
        // ragdoll before its bodies destruct — no panic, no leaked-body assertion.
        {
            let mut world = World::new().expect("world creation");
            world.enable_ragdoll(&scene, rig).expect("ragdoll build");
            assert!(world.has_ragdoll(rig_uuid));
            world.step(&mut scene, FIXED_STEP);
            // `world` drops here with a live ragdoll — the teardown order must hold.
        }

        // And the explicit-disable path is clean too: build, disable, assert it is gone.
        {
            let mut world = World::new().expect("world creation");
            world.enable_ragdoll(&scene, rig).expect("ragdoll build");
            world.disable_ragdoll(rig_uuid);
            assert!(
                !world.has_ragdoll(rig_uuid),
                "disable_ragdoll removed the ragdoll"
            );
            // A second disable is a no-op, never a panic.
            world.disable_ragdoll(rig_uuid);
            // Stepping after disable still works (no dangling ragdoll state).
            world.step(&mut scene, FIXED_STEP);
        }
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

    #[test]
    fn sphere_rests_on_floor() {
        let _guard = jolt_guard();
        let mut scene = Scene::new();
        // A static unit-box floor (top at y = 0.5).
        spawn_box(&mut scene, "Floor", Vec3::ZERO, None);
        // A dynamic sphere of radius 0.5 (packed in .x) dropped from y = 5.
        let sphere = spawn_dynamic_shape(
            &mut scene,
            "Sphere",
            Vec3::new(0.0, 5.0, 0.0),
            Collider {
                shape: Shape::Sphere,
                half_extents: Vec3::new(0.5, 0.5, 0.5),
                ..Collider::default()
            },
        );

        let mut world = World::new().expect("world creation");
        let mut cook = no_cook;
        world.populate(&mut scene, &mut cook);
        assert_eq!(world.stats().body_count, 2, "floor + sphere");

        for _ in 0..400 {
            world.step(&mut scene, FIXED_STEP);
        }
        // Floor top = 0.5, sphere radius 0.5 → resting centre ~1.0.
        let rest_y = body_y(&scene, sphere);
        assert!(
            (rest_y - 1.0).abs() < 0.1 && rest_y > 0.5,
            "the sphere came to rest on the floor (rest_y = {rest_y}, expected ~1.0)"
        );
    }

    #[test]
    fn capsule_rests_on_floor() {
        let _guard = jolt_guard();
        let mut scene = Scene::new();
        spawn_box(&mut scene, "Floor", Vec3::ZERO, None);
        // A dynamic capsule: radius 0.3 (.x), cylinder half-height 0.4 (.y) → total half-height
        // 0.3 + 0.4 = 0.7. Dropped from y = 5.
        let capsule = spawn_dynamic_shape(
            &mut scene,
            "Capsule",
            Vec3::new(0.0, 5.0, 0.0),
            Collider {
                shape: Shape::Capsule,
                half_extents: Vec3::new(0.3, 0.4, 0.3),
                ..Collider::default()
            },
        );

        let mut world = World::new().expect("world creation");
        let mut cook = no_cook;
        world.populate(&mut scene, &mut cook);

        for _ in 0..400 {
            world.step(&mut scene, FIXED_STEP);
        }
        // Floor top = 0.5, capsule total half-height 0.7 → resting centre ~1.2 if upright. The
        // capsule may topple, but its centre cannot rest below the floor's top + its radius (0.8),
        // and cannot tunnel through.
        let rest_y = body_y(&scene, capsule);
        assert!(
            rest_y > 0.7 && rest_y < 1.4,
            "the capsule came to rest on the floor (rest_y = {rest_y})"
        );
    }

    #[test]
    fn convex_hull_behaves_like_a_box() {
        let _guard = jolt_guard();
        let mut scene = Scene::new();
        spawn_box(&mut scene, "Floor", Vec3::ZERO, None);
        // A dynamic ConvexHull cooked from the unit cube (corners at ±0.5) → a 1×1×1 box hull.
        let hull = spawn_dynamic_shape(
            &mut scene,
            "Hull",
            Vec3::new(0.0, 5.0, 0.0),
            Collider {
                shape: Shape::ConvexHull,
                source_mesh: Uuid(42),
                ..Collider::default()
            },
        );

        let mut world = World::new().expect("world creation");
        let mut cook = cube_cook;
        world.populate(&mut scene, &mut cook);
        assert_eq!(
            world.stats().body_count,
            2,
            "floor + cooked hull (the cook succeeded)"
        );

        for _ in 0..400 {
            world.step(&mut scene, FIXED_STEP);
        }
        // Like the unit box: floor top 0.5 + hull half-height 0.5 → resting centre ~1.0.
        let rest_y = body_y(&scene, hull);
        assert!(
            (rest_y - 1.0).abs() < 0.1 && rest_y > 0.5,
            "the convex hull rests like a box (rest_y = {rest_y}, expected ~1.0)"
        );
    }

    #[test]
    fn mesh_floor_catches_a_falling_box() {
        let _guard = jolt_guard();
        let mut scene = Scene::new();
        // A static Mesh floor cooked from a flat quad at y = 0 (the body is built scale-free, so
        // the cooked surface sits at the entity's world translation).
        let floor = scene.create_entity("MeshFloor");
        scene
            .with_component_mut::<Transform, _>(floor, |t| t.translation = Vec3::ZERO)
            .unwrap();
        scene
            .add_component(
                floor,
                Collider {
                    shape: Shape::Mesh,
                    source_mesh: Uuid(7),
                    ..Collider::default()
                },
            )
            .unwrap();
        // A dynamic box dropped onto the mesh floor (over the centre of the quad so it lands on it).
        let dynamic = Rigidbody {
            motion: Motion::Dynamic,
            ..Rigidbody::default()
        };
        let falling = spawn_box(
            &mut scene,
            "Falling",
            Vec3::new(0.0, 3.0, 0.0),
            Some(dynamic),
        );
        scene.relink_hierarchy();
        scene.update_world_transforms();

        let mut world = World::new().expect("world creation");
        let mut cook = quad_cook;
        world.populate(&mut scene, &mut cook);
        assert_eq!(world.stats().body_count, 2, "mesh floor + falling box");

        for _ in 0..400 {
            world.step(&mut scene, FIXED_STEP);
        }
        // The mesh floor surface is at y = 0; the box half-height is 0.5 → resting centre ~0.5.
        // The point: it was caught, not tunneled through to -infinity.
        let rest_y = body_y(&scene, falling);
        assert!(
            rest_y > -0.2,
            "the mesh floor caught the falling box (rest_y = {rest_y}, did not tunnel through)"
        );
        assert!(
            (rest_y - 0.5).abs() < 0.2,
            "the box settled on the mesh floor near y = 0.5 (got {rest_y})"
        );
    }

    #[test]
    fn mesh_on_dynamic_errors() {
        let _guard = jolt_guard();
        let mut scene = Scene::new();
        // A static box floor so the world has a body regardless.
        spawn_box(&mut scene, "Floor", Vec3::ZERO, None);
        // A DYNAMIC body with a Mesh collider — invalid (Jolt MeshShape is Static/Kinematic only).
        let _bad = spawn_dynamic_shape(
            &mut scene,
            "BadMesh",
            Vec3::new(0.0, 5.0, 0.0),
            Collider {
                shape: Shape::Mesh,
                source_mesh: Uuid(7),
                ..Collider::default()
            },
        );

        // The cook geometry resolver yields the typed error for a Mesh on a Dynamic body.
        let err = super::world::cook_shape_geometry(
            &Collider {
                shape: Shape::Mesh,
                source_mesh: Uuid(7),
                ..Collider::default()
            },
            MotionType::Dynamic,
            &mut cube_cook,
        )
        .expect_err("a Mesh on a Dynamic body must be a typed error");
        assert!(
            matches!(err, Error::MeshShapeOnDynamic),
            "the error is MeshShapeOnDynamic, got {err:?}"
        );

        // And the populate walk skips that body but still builds the world (the floor remains).
        let mut world = World::new().expect("world creation");
        let mut cook = cube_cook;
        world.populate(&mut scene, &mut cook);
        assert_eq!(
            world.stats().body_count,
            1,
            "only the floor was created; the dynamic-mesh body was skipped"
        );
    }

    #[test]
    fn no_cook_source_errors() {
        // A ConvexHull/Mesh collider with no source mesh (`source_mesh == 0`) is the typed
        // NoCookSource error — it never invokes the cook with a zero id.
        for shape in [Shape::ConvexHull, Shape::Mesh] {
            let err = super::world::cook_shape_geometry(
                &Collider {
                    shape,
                    source_mesh: Uuid(0),
                    ..Collider::default()
                },
                MotionType::Static,
                &mut |_| panic!("cook must not be called when there is no source mesh"),
            )
            .expect_err("a ConvexHull/Mesh with no source mesh must be a typed error");
            assert!(
                matches!(err, Error::NoCookSource),
                "the error is NoCookSource for {shape:?}, got {err:?}"
            );
        }
    }

    #[test]
    fn autofit_box() {
        let _guard = jolt_guard();
        let mut scene = Scene::new();
        // An entity scaled ×2 in x with a unit-cube mesh + a Box collider. The mesh AABB is ±0.5;
        // baking the world scale (2,1,1) into the half-extents gives (1.0, 0.5, 0.5).
        let e = scene.create_entity("Box");
        scene
            .with_component_mut::<Transform, _>(e, |t| t.scale = Vec3::new(2.0, 1.0, 1.0))
            .unwrap();
        scene.add_component(e, Collider::default()).unwrap();
        scene
            .add_component(e, MeshComponent { mesh: Uuid(99) })
            .unwrap();
        scene.relink_hierarchy();
        scene.update_world_transforms();

        let mut cook = cube_cook;
        assert!(
            fit_collider_to_mesh(&mut scene, e, &mut cook),
            "the fit succeeded"
        );
        let collider = scene.component::<Collider>(e).unwrap();
        let he = collider.half_extents;
        assert!(
            (he.x - 1.0).abs() < 1e-5 && (he.y - 0.5).abs() < 1e-5 && (he.z - 0.5).abs() < 1e-5,
            "box half-extents match the AABB with the world scale baked in (got {he:?})"
        );
        assert_eq!(
            collider.source_mesh,
            Uuid(99),
            "the cook source is recorded for hull/mesh shapes"
        );
        // The cube is centred, so the offset is zero.
        assert!(
            collider.offset.length() < 1e-5,
            "a centred mesh has a zero offset (got {:?})",
            collider.offset
        );
    }

    #[test]
    fn autofit_capsule() {
        let _guard = jolt_guard();
        let mut scene = Scene::new();
        // An entity scaled ×3 in y with a unit-cube mesh + a Capsule collider. The mesh AABB half
        // is (0.5, 0.5, 0.5); world scale (1,3,1) → half (0.5, 1.5, 0.5). Capsule: radius =
        // max(x, z) = 0.5; half-height = max(0, y_half - radius) = 1.5 - 0.5 = 1.0.
        let e = scene.create_entity("Capsule");
        scene
            .with_component_mut::<Transform, _>(e, |t| t.scale = Vec3::new(1.0, 3.0, 1.0))
            .unwrap();
        scene
            .add_component(
                e,
                Collider {
                    shape: Shape::Capsule,
                    ..Collider::default()
                },
            )
            .unwrap();
        scene
            .add_component(e, MeshComponent { mesh: Uuid(77) })
            .unwrap();
        scene.relink_hierarchy();
        scene.update_world_transforms();

        let mut cook = cube_cook;
        assert!(
            fit_collider_to_mesh(&mut scene, e, &mut cook),
            "the fit succeeded"
        );
        let he = scene.component::<Collider>(e).unwrap().half_extents;
        assert!(
            (he.x - 0.5).abs() < 1e-5,
            "capsule radius = max(x,z) = 0.5 (got {})",
            he.x
        );
        assert!(
            (he.y - 1.0).abs() < 1e-5,
            "capsule half-height = y_half - radius = 1.0 (got {})",
            he.y
        );
        assert!(
            (he.z - 0.5).abs() < 1e-5,
            "capsule radius mirrored in .z (got {})",
            he.z
        );
    }

    #[test]
    fn autofit_bone_capsules() {
        let _guard = jolt_guard();
        let mut scene = Scene::new();
        // A 3-bone chain (each child +0.5 along y from its parent in local space). The root and the
        // mid bone each have a child 0.5 away → half-height 0.25; the leaf has no child → 0.05.
        let (rig, _rig_uuid) = spawn_chain_rig(&mut scene, 3);
        // Clear the authored sizes so the fit is what we assert.
        scene
            .with_component_mut::<BonePhysicsComponent, _>(rig, |p| {
                for bone in &mut p.bones {
                    bone.shape_half_extents = Vec3::ZERO;
                }
            })
            .unwrap();

        assert!(fit_bone_capsules(&mut scene, rig), "the bone fit succeeded");
        let bones = scene
            .with_component::<BonePhysicsComponent, _>(rig, |p| p.bones.clone())
            .unwrap();
        assert_eq!(bones.len(), 3, "one sized capsule per bone");
        // Root + mid: child 0.5 away → half-height 0.25, radius max(0.25*0.3, 0.03) = 0.075.
        for i in [0usize, 1] {
            let he = bones[i].shape_half_extents;
            assert!(
                (he.y - 0.25).abs() < 1e-5,
                "bone {i} half-height spans to its child (0.25, got {})",
                he.y
            );
            assert!(
                (he.x - 0.075).abs() < 1e-5 && (he.z - he.x).abs() < 1e-5,
                "bone {i} radius is 0.3× the half-height (0.075, got {})",
                he.x
            );
        }
        // Leaf: no child → the leaf defaults (half-height 0.05, radius max(0.05*0.3, 0.03) = 0.03).
        let leaf = bones[2].shape_half_extents;
        assert!(
            (leaf.y - 0.05).abs() < 1e-5 && (leaf.x - 0.03).abs() < 1e-5,
            "the leaf bone uses the leaf defaults (got {leaf:?})"
        );
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

    #[test]
    fn solid_contact_begin_end() {
        let _guard = jolt_guard();
        let mut scene = Scene::new();

        // A static unit-box floor (top face at y = 0.5) and a dynamic box dropped from just above
        // it, so the touch resolves within a handful of substeps.
        let floor = spawn_box(&mut scene, "Floor", Vec3::ZERO, None);
        let dynamic = Rigidbody {
            motion: Motion::Dynamic,
            ..Rigidbody::default()
        };
        let falling = spawn_box(
            &mut scene,
            "Falling",
            Vec3::new(0.0, 1.2, 0.0),
            Some(dynamic),
        );

        let mut world = World::new().expect("world creation");
        let mut cook = no_cook;
        world.populate(&mut scene, &mut cook);

        // Step until the box lands and a Begin contact fires.
        let mut begin: Option<ContactEvent> = None;
        for _ in 0..120 {
            world.step(&mut scene, FIXED_STEP);
            if let Some(event) = world
                .drain_contacts(0)
                .events
                .into_iter()
                .find(|e| e.kind == ContactKind::Begin)
            {
                begin = Some(event);
                break;
            }
        }
        let begin = begin.expect("a Begin contact fired when the box landed on the floor");
        assert!(!begin.sensor, "a solid floor touch is not a sensor overlap");
        // The two bodies are the floor + the falling box (order is Jolt's, so check the set).
        let pair = [begin.target_a, begin.target_b];
        assert!(
            pair.contains(&Some(WorldHitTarget::SceneEntity(floor)))
                && pair.contains(&Some(WorldHitTarget::SceneEntity(falling))),
            "the Begin event names the floor + the falling box (got {pair:?})"
        );
        // A plausible contact: a finite point near the floor top (y ≈ 0.5) and an up-ish normal.
        assert!(
            begin.point.is_finite() && begin.point.y > 0.0 && begin.point.y < 1.0,
            "the contact point is near the floor surface (got {:?})",
            begin.point
        );
        assert!(
            begin.normal.is_finite() && begin.normal.length() > 0.5,
            "the contact normal is a real unit-ish direction (got {:?})",
            begin.normal
        );

        // Fling the box up and away so the bodies separate, producing an End contact.
        world.apply_impulse(falling, Vec3::new(0.0, 30.0, 0.0));
        let mut end: Option<ContactEvent> = None;
        let mut high_water = world.drain_contacts(0).high_water_seq;
        for _ in 0..120 {
            world.step(&mut scene, FIXED_STEP);
            if let Some(event) = world
                .drain_contacts(0)
                .events
                .into_iter()
                .find(|e| e.kind == ContactKind::End)
            {
                end = Some(event);
                break;
            }
        }
        let end = end.expect("an End contact fired when the box left the floor");
        assert!(
            end.seq > begin.seq,
            "the End event is stamped after the Begin (end {} > begin {})",
            end.seq,
            begin.seq
        );

        // `drain_contacts(0)` returns the full ring in seq order, Begin before End.
        let drain = world.drain_contacts(0);
        assert!(!drain.events.is_empty(), "the ring retained the events");
        let mut prev = 0u64;
        for event in &drain.events {
            assert!(event.seq > prev, "events are in ascending seq order");
            prev = event.seq;
        }
        high_water = high_water.max(drain.high_water_seq);
        assert_eq!(
            drain.high_water_seq, high_water,
            "high_water_seq is the newest stamped seq"
        );
        // A cursor at the newest seq sees nothing further, with no overflow.
        let caught_up = world.drain_contacts(drain.high_water_seq);
        assert!(
            caught_up.events.is_empty() && !caught_up.overflowed,
            "a caught-up cursor drains nothing and does not overflow"
        );
    }

    #[test]
    fn sensor_overlap() {
        let _guard = jolt_guard();
        let mut scene = Scene::new();

        // A static sensor volume centred at the origin and a fast dynamic body shot along +x
        // through it. No gravity, so the body travels straight and the only velocity change that
        // could occur would be a (forbidden) solid response from the sensor.
        let sensor = spawn_sensor_box(&mut scene, "Sensor", Vec3::ZERO, Vec3::splat(1.0));
        let body_collider = Collider {
            half_extents: Vec3::splat(0.25),
            ..Collider::default()
        };
        let body = spawn_dynamic_shape(
            &mut scene,
            "Probe",
            Vec3::new(-5.0, 0.0, 0.0),
            body_collider,
        );
        // Drive it at a steady +x velocity with gravity and damping off, so the only force that
        // could perturb it would be a (forbidden) solid response from the sensor.
        let body_entity = scene.find_entity_by_uuid(body).unwrap();
        scene
            .with_component_mut::<Rigidbody, _>(body_entity, |rb| {
                rb.gravity_factor = 0.0;
                rb.linear_damping = 0.0;
            })
            .unwrap();

        let mut world = World::new().expect("world creation");
        let mut cook = no_cook;
        world.populate(&mut scene, &mut cook);
        world.set_linear_velocity(body, Vec3::new(6.0, 0.0, 0.0));

        let mut saw_begin = false;
        let mut saw_end = false;
        for _ in 0..120 {
            // Re-assert the horizontal velocity each frame; a sensor must never have perturbed it.
            let v = world.body_linear_velocity(body);
            assert!(
                v.y.abs() < 1e-3 && v.z.abs() < 1e-3 && (v.x - 6.0).abs() < 1e-3,
                "the probe's velocity is unchanged by the sensor (overlap-only); got {v:?}"
            );
            world.step(&mut scene, FIXED_STEP);
            for event in world.drain_contacts(0).events {
                // Every event in this scene involves the sensor, so all must carry sensor = true.
                assert!(
                    event.sensor,
                    "a sensor overlap carries sensor = true (got {event:?})"
                );
                let pair = [event.target_a, event.target_b];
                assert!(
                    pair.contains(&Some(WorldHitTarget::SceneEntity(sensor)))
                        && pair.contains(&Some(WorldHitTarget::SceneEntity(body))),
                    "the overlap names the sensor + the probe (got {pair:?})"
                );
                match event.kind {
                    ContactKind::Begin => saw_begin = true,
                    ContactKind::End => saw_end = true,
                }
            }
        }
        assert!(saw_begin, "entering the sensor fired a Begin overlap");
        assert!(saw_end, "leaving the sensor fired an End overlap");
        // The body passed clean through to the far side — the sensor never solved a contact.
        let final_x = body_y_axis(&scene, body, 0);
        assert!(
            final_x > 1.0,
            "the probe passed through the sensor (final x {final_x} > 1.0, not blocked)"
        );
    }

    /// One axis of an entity's local translation, by uuid (`axis`: 0 = x, 1 = y, 2 = z).
    fn body_y_axis(scene: &Scene, uuid: Uuid, axis: usize) -> f32 {
        let e = scene.find_entity_by_uuid(uuid).unwrap();
        scene.component::<Transform>(e).unwrap().translation[axis]
    }

    #[test]
    fn ring_overflow() {
        let _guard = jolt_guard();
        let mut scene = Scene::new();

        // A wide static floor and a grid of dynamic boxes dropped just above it: each box → floor
        // touch is one Begin, and adjacent boxes touch each other, so a > 256-box grid produces
        // well over CONTACT_RING_CAP transitions in a few steps — forcing the ring to evict.
        spawn_static_box(&mut scene, "Floor", Vec3::ZERO, Vec3::new(40.0, 0.5, 40.0));
        let side = 18; // 18×18 = 324 boxes > the 256-event cap
        for i in 0..side {
            for j in 0..side {
                let x = (i as f32 - side as f32 / 2.0) * 0.6;
                let z = (j as f32 - side as f32 / 2.0) * 0.6;
                spawn_dynamic_shape(
                    &mut scene,
                    "GridBox",
                    Vec3::new(x, 0.9, z),
                    Collider {
                        half_extents: Vec3::splat(0.25),
                        ..Collider::default()
                    },
                );
            }
        }

        let mut world = World::new().expect("world creation");
        let mut cook = no_cook;
        world.populate(&mut scene, &mut cook);

        // Step until the ring has overflowed its cap (the boxes land within a few frames).
        for _ in 0..30 {
            world.step(&mut scene, FIXED_STEP);
            if world.drain_contacts(0).high_water_seq > CONTACT_RING_CAP as u64 {
                break;
            }
        }

        // From a cursor at 0 (stale: it predates the evicted tail), the drain reports the overflow
        // and an advanced oldest_seq, and the ring holds at most CONTACT_RING_CAP events.
        let drain = world.drain_contacts(0);
        assert!(
            drain.high_water_seq > CONTACT_RING_CAP as u64,
            "more than the cap of events were stamped (high_water {} > {CONTACT_RING_CAP})",
            drain.high_water_seq
        );
        assert!(
            drain.events.len() <= CONTACT_RING_CAP,
            "the ring is bounded at the cap (held {})",
            drain.events.len()
        );
        assert!(
            drain.oldest_seq > 1,
            "the oldest retained seq advanced past 1 (the head was evicted; got {})",
            drain.oldest_seq
        );
        assert!(
            drain.overflowed,
            "a stale cursor (0) is told it missed evicted events"
        );

        // A cursor at the oldest retained seq is not overflowed (it has not fallen behind the tail).
        let fresh = world.drain_contacts(drain.oldest_seq);
        assert!(
            !fresh.overflowed,
            "a cursor at the retained tail is not flagged as overflowed"
        );
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

    #[test]
    fn kinematic_bone_shoves_dynamics() {
        let _guard = jolt_guard();
        let mut scene = Scene::new();

        // One kinematic-bone rig with a single joint, started clear of a resting dynamic box at the
        // origin (gravity off so the box is the kinematic body's only velocity source — a clean
        // assertion). The joint starts at x = -1.5; each frame it sweeps toward +x into the box.
        let (_rig, joints) =
            spawn_kinematic_rig(&mut scene, &[Vec3::new(-1.5, 0.0, 0.0)], Vec::new());
        let joint = joints[0];

        // A dynamic box at the origin (gravity off), sitting in the sweep's path.
        let target = scene.create_entity("Target");
        scene
            .with_component_mut::<Transform, _>(target, |t| t.translation = Vec3::ZERO)
            .unwrap();
        scene
            .add_component(
                target,
                Collider {
                    half_extents: Vec3::splat(0.25),
                    ..Collider::default()
                },
            )
            .unwrap();
        scene
            .add_component(
                target,
                Rigidbody {
                    motion: Motion::Dynamic,
                    gravity_factor: 0.0,
                    ..Rigidbody::default()
                },
            )
            .unwrap();
        let box_uuid = scene.component::<IdComponent>(target).unwrap().id;
        scene.relink_hierarchy();
        scene.update_world_transforms();

        let mut world = World::new().expect("world creation");
        let mut cook = no_cook;
        world.populate(&mut scene, &mut cook);
        world.build_bone_bodies(&mut scene);

        // Two bodies: the dynamic target + the kinematic bone capsule.
        assert_eq!(
            world.stats().body_count,
            2,
            "the bone body joined the world"
        );

        assert_eq!(
            world.body_linear_velocity(box_uuid),
            Vec3::ZERO,
            "the target box is at rest before the sweep"
        );

        // Sweep the joint toward +x in 0.05 m increments each frame; the kinematic capsule follows
        // via MoveKinematic, so the swept motion imparts +x contact velocity to the box.
        for step in 1..=60 {
            let x = -1.5 + (step as f32) * 0.05;
            scene
                .with_component_mut::<Transform, _>(joint, |t| t.translation.x = x)
                .unwrap();
            scene.update_world_transforms();
            world.step(&mut scene, FIXED_STEP);
        }

        let velocity = world.body_linear_velocity(box_uuid);
        assert!(
            velocity.x > 0.1,
            "the swept kinematic bone imparted +x contact velocity to the box (got {velocity:?}); \
             a teleport would have left it at rest"
        );
    }

    #[test]
    fn driven_subset() {
        let _guard = jolt_guard();

        // A four-joint rig. With an empty `driven` list, one body per bone is created.
        let mut all_scene = Scene::new();
        spawn_kinematic_rig(
            &mut all_scene,
            &[
                Vec3::new(0.0, 0.0, 0.0),
                Vec3::new(1.0, 0.0, 0.0),
                Vec3::new(2.0, 0.0, 0.0),
                Vec3::new(3.0, 0.0, 0.0),
            ],
            Vec::new(),
        );
        let mut all_world = World::new().expect("world creation");
        let mut cook = no_cook;
        all_world.populate(&mut all_scene, &mut cook);
        all_world.build_bone_bodies(&mut all_scene);
        assert_eq!(
            all_world.stats().body_count,
            4,
            "an empty driven list creates one kinematic body per bone"
        );

        // The same rig, but `driven = [0, 2]` — only those two joints get a body.
        let mut subset_scene = Scene::new();
        spawn_kinematic_rig(
            &mut subset_scene,
            &[
                Vec3::new(0.0, 0.0, 0.0),
                Vec3::new(1.0, 0.0, 0.0),
                Vec3::new(2.0, 0.0, 0.0),
                Vec3::new(3.0, 0.0, 0.0),
            ],
            vec![0, 2],
        );
        let mut subset_world = World::new().expect("world creation");
        subset_world.populate(&mut subset_scene, &mut cook);
        subset_world.build_bone_bodies(&mut subset_scene);
        let bodies = subset_world.list_bodies();
        assert_eq!(
            bodies.len(),
            2,
            "a two-element driven list creates exactly two bodies"
        );

        // The two bodies are the listed joints (positions x = 0 and x = 2), in bone order.
        assert!(
            (bodies[0].position.x - 0.0).abs() < 1e-4,
            "the first bone body is joint 0 (x = 0); got {:?}",
            bodies[0].position
        );
        assert!(
            (bodies[1].position.x - 2.0).abs() < 1e-4,
            "the second bone body is joint 2 (x = 2); got {:?}",
            bodies[1].position
        );

        // A disabled rig creates no bodies at all.
        let mut off_scene = Scene::new();
        let (rig, _) = spawn_kinematic_rig(
            &mut off_scene,
            &[Vec3::ZERO, Vec3::new(1.0, 0.0, 0.0)],
            Vec::new(),
        );
        off_scene
            .with_component_mut::<KinematicBones, _>(rig, |k| k.enabled = false)
            .unwrap();
        let mut off_world = World::new().expect("world creation");
        off_world.populate(&mut off_scene, &mut cook);
        off_world.build_bone_bodies(&mut off_scene);
        assert_eq!(
            off_world.stats().body_count,
            0,
            "a disabled KinematicBones rig creates no bodies"
        );
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

    #[test]
    fn active_ragdoll_tracks_pose() {
        let _guard = jolt_guard();
        let mut scene = Scene::new();

        // A 3-bone chain rig (each bone a SwingTwist child of the previous), high in the air so the
        // motors drive the relative orientations while the whole rig free-falls.
        let (rig, rig_uuid) = spawn_chain_rig(&mut scene, 3);
        scene
            .with_component_mut::<Transform, _>(rig, |t| t.translation = Vec3::new(0.0, 5.0, 0.0))
            .unwrap();
        scene.relink_hierarchy();
        scene.update_world_transforms();

        let mut world = World::new().expect("world creation");
        world.enable_ragdoll(&scene, rig).expect("ragdoll build");

        // The single driven joint is bone 1 → parent bone 0. The SwingTwist motor controls the
        // child part's orientation *relative to its parent* (body space), which starts at identity
        // (the chain is built with no relative rotation between bones). Drive it toward a small
        // Z-rotation target inside the 0.5 rad swing/twist cone.
        world
            .set_ragdoll_blend(rig_uuid, Some(true), None, None, None)
            .expect("activate the ragdoll");
        let target_rotation = Quat::from_rotation_z(0.3);
        let target = PoseTarget {
            rig: rig_uuid,
            local: vec![
                JointPose {
                    rotation: target_rotation,
                    ..JointPose::default()
                };
                3
            ],
        };

        // The relative orientation between bone 1's part and its parent (bone 0) — the quantity the
        // SwingTwist motor drives. The error is how far that relative orientation is from the
        // target. Frame-faithful: it compares like-for-like (body-space relative vs the body-space
        // motor target).
        let measure = |world: &World| -> f32 {
            let (_, parent) = world.ragdoll_part_transform(rig_uuid, 0).unwrap();
            let (_, child) = world.ragdoll_part_transform(rig_uuid, 1).unwrap();
            let relative = parent.inverse() * child;
            quat_angle(relative, target_rotation)
        };

        // At rest (before driving) the relative orientation is ~identity, so the error to the
        // target is ~the target's own magnitude (0.3 rad). Step the full compose order; the motor
        // must close that gap, then settle to a stable steady state (the free-falling chain's
        // gravity torque leaves a small steady offset the finite-force PD motor balances against).
        let rest_error = measure(&world);
        let drive_once = |world: &mut World, scene: &mut Scene| {
            world.drive_ragdolls_to_pose(std::slice::from_ref(&target));
            world.advance_ragdoll_blend(FIXED_STEP);
            world.step(scene, FIXED_STEP);
            world.write_ragdoll_poses(scene);
        };

        // The minimum error reached while driving: the motor pulls the relative orientation onto
        // the target (it crosses near-zero on the way to its gravity-balanced equilibrium).
        let mut min_error = rest_error;
        for _ in 0..40 {
            drive_once(&mut world, &mut scene);
            min_error = min_error.min(measure(&world));
        }
        // The settled steady state: a few more steps must barely change it (no blow-up / oscillation).
        let settled = measure(&world);
        for _ in 0..60 {
            drive_once(&mut world, &mut scene);
        }
        let settled_late = measure(&world);

        assert!(
            (rest_error - 0.3).abs() < 0.05,
            "the undriven relative orientation starts near rest (error to target {rest_error} ≈ 0.3)"
        );
        assert!(
            min_error < 0.05,
            "the motor converged the joint onto the target (min error {min_error} « rest {rest_error})"
        );
        assert!(
            (settled_late - settled).abs() < 0.05,
            "the driven joint settled to a stable steady state (settled {settled} → {settled_late})"
        );
        assert!(
            settled_late < rest_error,
            "the driven steady state stays nearer the target than the undriven rest pose \
             (settled {settled_late} < rest {rest_error})"
        );
    }

    #[test]
    fn partial_blend() {
        let _guard = jolt_guard();
        let mut scene = Scene::new();

        // A 2-bone chain (passive ragdoll, weights default to 1.0 = pure physics). No floor: it
        // free-falls, but the relative bone-local orientations stay near rest.
        let (rig, rig_uuid) = spawn_chain_rig(&mut scene, 2);
        scene
            .with_component_mut::<Transform, _>(rig, |t| t.translation = Vec3::new(0.0, 5.0, 0.0))
            .unwrap();
        scene.relink_hierarchy();
        scene.update_world_transforms();
        let handles = bone_handles(&scene, rig);

        let mut world = World::new().expect("world creation");
        world.enable_ragdoll(&scene, rig).expect("ragdoll build");

        // Settle a few steps, then write the pure-physics pose (every weight is 1.0). Capture each
        // bone's resolved physics local rotation — the upper end of the blend.
        for _ in 0..15 {
            world.step(&mut scene, FIXED_STEP);
        }
        world.write_ragdoll_poses(&mut scene);
        let physics_rot: Vec<Quat> = handles
            .iter()
            .map(|&b| bone_override_rotation(&scene, b))
            .collect();

        // Seed each bone's PoseOverride with a distinct "animation" rotation (the lower end of the
        // blend) — well away from the physics pose so the midpoint is unambiguous.
        let anim_rot = Quat::from_rotation_x(0.8);
        for &bone in &handles {
            scene
                .with_component_mut::<PoseOverride, _>(bone, |p| p.rotation = anim_rot)
                .unwrap();
        }

        // Bone 1 → target weight 0.5; bone 0 stays at 1.0 (pure physics). Ease the per-bone weight
        // to the target without stepping, so the physics pose is unchanged across the write.
        world
            .set_ragdoll_blend(rig_uuid, None, None, Some(1), Some(0.5))
            .expect("partial weight on bone 1");
        for _ in 0..12 {
            world.advance_ragdoll_blend(FIXED_STEP);
        }
        world.write_ragdoll_poses(&mut scene);

        // Bone 0 (weight 1.0): pure physics — it ignored the seeded animation rotation.
        let bone0 = bone_override_rotation(&scene, handles[0]);
        assert!(
            quat_angle(bone0, physics_rot[0]) < 1e-3,
            "the weight-1 bone is pure physics (ignored the animation seed)"
        );
        assert!(
            quat_angle(bone0, anim_rot) > 0.1,
            "the weight-1 bone is not the animation pose"
        );

        // Bone 1 (weight 0.5): the geodesic midpoint of the animation and physics rotations — its
        // angle to each end is ~half the full span, and strictly between both ends.
        let bone1 = bone_override_rotation(&scene, handles[1]);
        let span = quat_angle(anim_rot, physics_rot[1]);
        assert!(
            span > 0.2,
            "the animation and physics poses are distinct enough to blend (span {span})"
        );
        let to_anim = quat_angle(bone1, anim_rot);
        let to_phys = quat_angle(bone1, physics_rot[1]);
        assert!(
            to_anim > 1e-3 && to_phys > 1e-3,
            "the half-weight bone is strictly between the two ends (to_anim {to_anim}, to_phys {to_phys})"
        );
        assert!(
            (to_anim - span * 0.5).abs() < 0.05 && (to_phys - span * 0.5).abs() < 0.05,
            "the half-weight bone is the midpoint (to_anim {to_anim}, to_phys {to_phys}, half-span {})",
            span * 0.5
        );
    }

    #[test]
    fn passive_release() {
        let _guard = jolt_guard();
        let mut scene = Scene::new();

        // A 3-bone chain high in the air. Enable, go active, then go passive: the motors release,
        // so the bodies fall freely under gravity.
        let (rig, rig_uuid) = spawn_chain_rig(&mut scene, 3);
        scene
            .with_component_mut::<Transform, _>(rig, |t| t.translation = Vec3::new(0.0, 8.0, 0.0))
            .unwrap();
        scene.relink_hierarchy();
        scene.update_world_transforms();

        let mut world = World::new().expect("world creation");
        world.enable_ragdoll(&scene, rig).expect("ragdoll build");
        world
            .set_ragdoll_blend(rig_uuid, Some(true), None, None, None)
            .expect("activate");
        world
            .set_ragdoll_blend(rig_uuid, Some(false), None, None, None)
            .expect("go passive");

        // `ragdoll_state` reports the mean target weight (default 1.0) and the bone count, with the
        // motors now inactive.
        let state = world.ragdoll_state(rig_uuid);
        assert!(state.present, "the rig has a live ragdoll");
        assert!(!state.active, "the motors were released (passive)");
        assert_eq!(state.bones, 3, "the ragdoll has one weight per bone");
        assert!(
            (state.body_weight - 1.0).abs() < 1e-6,
            "the default mean weight is pure physics (1.0), got {}",
            state.body_weight
        );
        // An absent rig reports the all-default (absent) state.
        assert_eq!(world.ragdoll_state(Uuid(987_654)), RagdollState::default());

        // Driving a passive ragdoll is a no-op (the motors are Off); the root falls under gravity.
        let (start_pos, _) = world
            .ragdoll_part_transform(rig_uuid, 0)
            .expect("root part transform");
        let target = PoseTarget {
            rig: rig_uuid,
            local: vec![
                JointPose {
                    rotation: Quat::from_rotation_z(0.3),
                    ..JointPose::default()
                };
                3
            ],
        };
        for _ in 0..30 {
            world.drive_ragdolls_to_pose(std::slice::from_ref(&target));
            world.advance_ragdoll_blend(FIXED_STEP);
            world.step(&mut scene, FIXED_STEP);
        }
        let (root_after, _) = world
            .ragdoll_part_transform(rig_uuid, 0)
            .expect("root part transform");
        assert!(
            root_after.y < start_pos.y - 0.3,
            "the released ragdoll fell freely under gravity (root y {} < start y {})",
            root_after.y,
            start_pos.y
        );
    }

    #[test]
    fn set_ragdoll_blend_errors() {
        let _guard = jolt_guard();
        let mut scene = Scene::new();

        let (rig, rig_uuid) = spawn_chain_rig(&mut scene, 3);
        let mut world = World::new().expect("world creation");

        // A missing rig (no ragdoll enabled yet) → NoRagdoll.
        let err = world
            .set_ragdoll_blend(rig_uuid, Some(true), None, None, None)
            .expect_err("a rig with no live ragdoll must error");
        assert!(matches!(err, Error::NoRagdoll), "got {err:?}");

        // Enable the ragdoll; now an out-of-range bone index → BoneOutOfRange.
        world.enable_ragdoll(&scene, rig).expect("ragdoll build");
        let err = world
            .set_ragdoll_blend(rig_uuid, None, None, Some(99), Some(0.5))
            .expect_err("an out-of-range bone must error");
        assert!(matches!(err, Error::BoneOutOfRange(99)), "got {err:?}");
        // A negative bone index is also out of range.
        let err = world
            .set_ragdoll_blend(rig_uuid, None, None, Some(-1), Some(0.5))
            .expect_err("a negative bone index must error");
        assert!(matches!(err, Error::BoneOutOfRange(-1)), "got {err:?}");

        // A valid in-range bone weight succeeds.
        world
            .set_ragdoll_blend(rig_uuid, None, None, Some(1), Some(0.25))
            .expect("an in-range bone weight succeeds");
    }

    #[test]
    fn ray_hits_box() {
        let _guard = jolt_guard();
        let mut scene = Scene::new();

        // A 2×2×2 static box centred at the origin: its +x face sits at x = 1.
        let target = spawn_static_box(&mut scene, "Target", Vec3::ZERO, Vec3::ONE);
        let target_uuid = scene.component::<IdComponent>(target).unwrap().id;

        let mut world = World::new().expect("world creation");
        let mut cook = no_cook;
        world.populate(&mut scene, &mut cook);

        // Cast +x from x = -5 toward the box: it strikes the -x face at x = -1, so the distance
        // along the unit ray is 4 and the hit point is on x = -1.
        let hit = world.raycast(Vec3::new(-5.0, 0.0, 0.0), Vec3::X, 10.0);
        assert!(hit.hit, "the ray must strike the static box");
        assert_eq!(
            hit.target,
            Some(WorldHitTarget::SceneEntity(target_uuid)),
            "the hit maps back to the box's owner entity"
        );
        assert!(
            (hit.point.x - (-1.0)).abs() < 1e-3,
            "the hit point is on the box's -x face (x = -1); got {}",
            hit.point.x
        );
        assert!(
            hit.point.y.abs() < 1e-3 && hit.point.z.abs() < 1e-3,
            "the hit point lies on the ray (y = z = 0); got {:?}",
            hit.point
        );
        assert!(
            (hit.distance - 4.0).abs() < 1e-3,
            "the distance along the ray is ~4 (from x = -5 to x = -1); got {}",
            hit.distance
        );
        // The surface normal at the -x face points back toward the ray origin (-x).
        assert!(
            (hit.normal.x - (-1.0)).abs() < 1e-2,
            "the -x face normal points -x; got {:?}",
            hit.normal
        );

        // A ray into empty space (pointing away from the box) hits nothing.
        let miss = world.raycast(Vec3::new(-5.0, 0.0, 0.0), Vec3::new(-1.0, 0.0, 0.0), 10.0);
        assert!(!miss.hit, "a ray into empty space hits nothing");
        assert_eq!(miss, RayHit::default(), "a miss is the default RayHit");
        // A ray that falls short of the box (max_dist too small) also misses.
        let short = world.raycast(Vec3::new(-5.0, 0.0, 0.0), Vec3::X, 1.0);
        assert!(!short.hit, "a ray that stops before the box hits nothing");
    }

    #[test]
    fn sphere_cast_thicker() {
        let _guard = jolt_guard();
        let mut scene = Scene::new();

        // A thin static box edge offset above the ray line: a 0.5-half box centred at
        // (0, 0.7, 0), so it spans y ∈ [0.2, 1.2]. A ray along +x at y = 0 passes below it
        // (misses), but a sphere of radius 0.5 swept along the same line is thick enough to catch
        // its lower edge.
        let edge = spawn_static_box(
            &mut scene,
            "Edge",
            Vec3::new(0.0, 0.7, 0.0),
            Vec3::splat(0.5),
        );
        let edge_uuid = scene.component::<IdComponent>(edge).unwrap().id;

        let mut world = World::new().expect("world creation");
        let mut cook = no_cook;
        world.populate(&mut scene, &mut cook);

        // A thin ray at y = 0 along +x slips under the box (its bottom face is at y = 0.2).
        let ray = world.raycast(Vec3::new(-5.0, 0.0, 0.0), Vec3::X, 10.0);
        assert!(!ray.hit, "a thin ray at y = 0 passes below the raised box");

        // A sphere of radius 0.5 swept along the same origin/dir is thick enough to reach the
        // box's lower edge.
        let swept = world.sphere_cast(Vec3::new(-5.0, 0.0, 0.0), Vec3::X, 0.5, 10.0);
        assert!(
            swept.hit,
            "a thicker sphere sweep catches the edge the thin ray missed"
        );
        assert_eq!(
            swept.target,
            Some(WorldHitTarget::SceneEntity(edge_uuid)),
            "the sweep hit maps back to the box's owner entity"
        );
        assert!(
            swept.distance > 0.0 && swept.distance < 10.0,
            "the sweep hit lies along the path; got {}",
            swept.distance
        );

        // The sweep into empty space (away from the box) still misses.
        let miss = world.sphere_cast(
            Vec3::new(-5.0, 0.0, 0.0),
            Vec3::new(-1.0, 0.0, 0.0),
            0.5,
            10.0,
        );
        assert!(!miss.hit, "a sweep into empty space hits nothing");
    }

    #[test]
    fn query_does_not_perturb() {
        let _guard = jolt_guard();

        // A deterministic scenario: a dynamic box falling onto a static floor, sampled each step.
        // Run it twice — once clean, once with raycasts/sphere-casts interleaved between every
        // step — and assert the per-step position trace is byte-for-byte identical. Queries take
        // `&self`, so they cannot perturb the sim; this exercises that at runtime.
        fn run_trace(interleave_queries: bool) -> Vec<[u8; 12]> {
            let mut scene = Scene::new();
            let _floor = spawn_box(&mut scene, "Floor", Vec3::ZERO, None);
            let dynamic = Rigidbody {
                motion: Motion::Dynamic,
                ..Rigidbody::default()
            };
            let falling = spawn_box(
                &mut scene,
                "Falling",
                Vec3::new(0.0, 5.0, 0.0),
                Some(dynamic),
            );
            let mut world = World::new().expect("world creation");
            let mut cook = no_cook;
            world.populate(&mut scene, &mut cook);

            let mut trace = Vec::new();
            for _ in 0..200 {
                if interleave_queries {
                    // Read-only probes at varied origins/dirs/radii — exercising both query paths
                    // and the body-lock normal read between sim steps.
                    let _ = world.raycast(Vec3::new(0.0, 5.0, 0.0), Vec3::NEG_Y, 20.0);
                    let _ = world.raycast(Vec3::new(-3.0, 0.5, 0.0), Vec3::X, 10.0);
                    let _ = world.sphere_cast(Vec3::new(0.0, 5.0, 0.0), Vec3::NEG_Y, 0.4, 20.0);
                    let _ = world.sphere_cast(Vec3::new(2.0, 1.0, 0.0), Vec3::NEG_X, 0.25, 10.0);
                }
                world.step(&mut scene, FIXED_STEP);
                // Sample the falling body's resolved local translation as raw little-endian bytes.
                let t = {
                    let e = scene.find_entity_by_uuid(falling).unwrap();
                    scene.component::<Transform>(e).unwrap().translation
                };
                let mut bytes = [0u8; 12];
                bytes[0..4].copy_from_slice(&t.x.to_le_bytes());
                bytes[4..8].copy_from_slice(&t.y.to_le_bytes());
                bytes[8..12].copy_from_slice(&t.z.to_le_bytes());
                trace.push(bytes);
            }
            trace
        }

        let clean = run_trace(false);
        let with_queries = run_trace(true);
        assert_eq!(
            clean, with_queries,
            "interleaving read-only raycasts/sphere-casts between steps changed the sim trace — a \
             query perturbed the deterministic step (it must not)"
        );
    }

    #[test]
    fn static_target_batch_round_trips_vegetation_hits() {
        let _guard = jolt_guard();
        let mut world = World::new().expect("world creation");

        let plant = saffron_spatial::PlantId::explicit([11; 16]).expect("plant id");
        let sensor_plant = saffron_spatial::PlantId::explicit([13; 16]).expect("plant id");
        let rows = [
            StaticTargetBodyCreate {
                target: WorldHitTarget::Vegetation(plant),
                shape: Shape::Capsule,
                half_extents: Vec3::new(0.3, 1.5, 0.3),
                position: Vec3::new(4.0, 1.5, 0.0),
                rotation: Quat::IDENTITY,
                sensor: false,
                friction: 0.5,
            },
            StaticTargetBodyCreate {
                target: WorldHitTarget::Vegetation(sensor_plant),
                shape: Shape::Sphere,
                half_extents: Vec3::new(0.8, 0.0, 0.0),
                position: Vec3::new(-4.0, 1.0, 0.0),
                rotation: Quat::IDENTITY,
                sensor: true,
                friction: 0.5,
            },
        ];
        let ids = world.add_static_target_bodies(&rows);
        assert_eq!(ids.len(), 2);
        assert!(
            ids.iter()
                .all(|&id| id != saffron_physics_sys::INVALID_BODY_ID)
        );

        // A ray into the solid capsule reports the tagged plant, never a forged entity uuid.
        let hit = world.raycast(Vec3::new(0.0, 1.5, 0.0), Vec3::X, 10.0);
        assert!(hit.hit, "the ray reaches the vegetation capsule");
        assert_eq!(hit.target, Some(WorldHitTarget::Vegetation(plant)));

        // The sensor body is query-visible through the body list with its own tagged owner.
        let sensor_info = world
            .list_bodies()
            .into_iter()
            .find(|body| body.target == Some(WorldHitTarget::Vegetation(sensor_plant)))
            .expect("the sensor body is listed");
        assert_eq!(sensor_info.motion, MotionType::Static);

        // Batch removal drops the bodies and their registry rows: the same ray now misses.
        world.remove_bodies(&ids);
        let miss = world.raycast(Vec3::new(0.0, 1.5, 0.0), Vec3::X, 10.0);
        assert!(!miss.hit, "the removed capsule no longer occludes the ray");
        assert!(
            world.list_bodies().into_iter().all(|body| body.target
                != Some(WorldHitTarget::Vegetation(plant))
                && body.target != Some(WorldHitTarget::Vegetation(sensor_plant))),
            "no registry row outlives the batch removal"
        );
    }
}
