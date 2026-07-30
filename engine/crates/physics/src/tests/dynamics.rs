use super::*;

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
