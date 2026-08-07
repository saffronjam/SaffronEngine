use super::*;

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
    let err = super::world::bodies::cook_shape_geometry(
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
        let err = super::world::bodies::cook_shape_geometry(
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
