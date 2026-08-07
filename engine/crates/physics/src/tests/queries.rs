use super::*;

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
