use super::*;

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
