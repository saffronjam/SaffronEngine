use super::*;

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
