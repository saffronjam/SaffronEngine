use super::*;

#[test]
fn kinematic_bone_shoves_dynamics() {
    let _guard = jolt_guard();
    let mut scene = Scene::new();

    // One kinematic-bone rig with a single joint, started clear of a resting dynamic box at the
    // origin (gravity off so the box is the kinematic body's only velocity source — a clean
    // assertion). The joint starts at x = -1.5; each frame it sweeps toward +x into the box.
    let (_rig, joints) = spawn_kinematic_rig(&mut scene, &[Vec3::new(-1.5, 0.0, 0.0)], Vec::new());
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
