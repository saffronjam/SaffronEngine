//! A slice over the live play-edge runtime: a dynamic box dropped above a static floor falls under
//! gravity during Play and the authored height is restored on Stop. This exercises the host's
//! on-play-edge runtime start + the per-frame simulation step + the physics write-back + the
//! drop-on-stop teardown.

use std::time::Duration;

use saffron_e2e::TestEngine;
use saffron_protocol::{EntityRef, PhysicsStateResult, WorldTransformResult};
use serde_json::json;

/// Floor top = floor center (0) + floor half-height (0.1); box half-extent = 0.5 (default).
const FLOOR_TOP: f32 = 0.1;
const BOX_HALF: f32 = 0.5;
const REST_Y: f32 = FLOOR_TOP + BOX_HALF; // ~0.6

/// The box entity's composed world-space Y.
fn box_y(engine: &mut TestEngine, box_id: &str) -> f32 {
    let wt: WorldTransformResult = engine
        .call("get-world-transform", json!({ "entity": box_id }))
        .expect("get-world-transform");
    wt.translation.y
}

#[test]
fn play_edge_drops_a_box_and_stop_restores_it() {
    let mut engine = TestEngine::boot(&[("SAFFRON_SCRATCH_PROJECT", "1")]).expect("boot engine");

    // Static floor: a thin wide collider with no rigidbody (implicitly static).
    let floor: EntityRef = engine
        .call("create-entity", json!({ "name": "Floor" }))
        .expect("create floor");
    let floor_id = floor.id.0.to_string();
    engine
        .call_raw(
            "set-transform",
            json!({ "entity": floor_id, "translation": { "x": 0, "y": 0, "z": 0 } }),
        )
        .expect("floor transform");
    engine
        .call_raw(
            "add-component",
            json!({ "entity": floor_id, "component": "Collider" }),
        )
        .expect("floor collider");
    engine
        .call_raw(
            "set-component-field",
            json!({
                "entity": floor_id,
                "component": "Collider",
                "field": "halfExtents",
                "value": { "x": 10, "y": 0.1, "z": 10 },
            }),
        )
        .expect("floor extents");

    // Dynamic box dropped from y=5 (default 0.5 half-extent box, default Dynamic rigidbody).
    let box_entity: EntityRef = engine
        .call("create-entity", json!({ "name": "Box" }))
        .expect("create box");
    let box_id = box_entity.id.0.to_string();
    engine
        .call_raw(
            "set-transform",
            json!({ "entity": box_id, "translation": { "x": 0, "y": 5, "z": 0 } }),
        )
        .expect("box transform");
    engine
        .call_raw(
            "add-component",
            json!({ "entity": box_id, "component": "Collider" }),
        )
        .expect("box collider");
    engine
        .call_raw(
            "add-component",
            json!({ "entity": box_id, "component": "Rigidbody" }),
        )
        .expect("box rigidbody");

    // Edit mode: no world; the box sits at its authored height.
    let edit_state: PhysicsStateResult = engine.call("physics-state", json!({})).expect("state");
    assert!(!edit_state.active, "no physics world in Edit");
    assert!(
        (box_y(&mut engine, &box_id) - 5.0).abs() < 1e-3,
        "box starts at authored y=5"
    );

    // Enter Play: the host builds the Jolt world on the edge and starts stepping it.
    engine.call_raw("play", json!({})).expect("play");
    engine.settle(Duration::from_millis(300));
    let falling = box_y(&mut engine, &box_id);
    assert!(falling < 5.0, "the box has started to fall: y={falling}");

    // The world reports the two bodies, one dynamic — proof the play-edge world was actually built.
    let play_state: PhysicsStateResult = engine
        .call("physics-state", json!({}))
        .expect("physics-state during play");
    assert!(play_state.active, "physics world active during Play");
    assert_eq!(play_state.body_count, 2, "floor + box");
    assert_eq!(play_state.dynamic_count, 1, "only the box is dynamic");

    // Let it settle, then sample twice to confirm it has come to rest (not still moving).
    engine.settle(Duration::from_millis(2000));
    let settled = box_y(&mut engine, &box_id);
    engine.settle(Duration::from_millis(400));
    let settled_later = box_y(&mut engine, &box_id);
    assert!(
        settled > REST_Y - 0.2,
        "did not tunnel through the floor: y={settled}"
    );
    assert!(
        settled < REST_Y + 0.3,
        "came to rest near floor top + half-extent: y={settled}"
    );
    assert!(
        (settled_later - settled).abs() < 0.05,
        "at rest, not drifting: {settled} -> {settled_later}"
    );

    // Stop discards the world; the authored scene was never written, so the box is back at y=5.
    engine.call_raw("stop", json!({})).expect("stop");
    engine.settle(Duration::from_millis(200));
    let stopped_state: PhysicsStateResult = engine
        .call("physics-state", json!({}))
        .expect("state after stop");
    assert!(!stopped_state.active, "world discarded on Stop");
    assert!(
        (box_y(&mut engine, &box_id) - 5.0).abs() < 1e-3,
        "authored box height restored after Stop"
    );

    // No validation errors across the whole play/stop cycle.
    let errors: Vec<String> = engine.validation_errors();
    assert!(
        errors.is_empty(),
        "validation layer flagged errors:\n{}",
        errors.join("\n")
    );

    engine.shutdown();
}
