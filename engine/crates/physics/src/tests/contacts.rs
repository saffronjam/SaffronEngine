use super::*;

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
