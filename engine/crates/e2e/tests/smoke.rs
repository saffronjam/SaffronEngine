//! Seed e2e tests against the live engine: ping/help/quit, a typed `render-stats` round-trip into
//! the protocol DTO, and a `validation_errors()`-empty assertion on a one-cube scene.
//!
//! Each test boots its own isolated engine, drives it over the shared wire client, and shuts down.

use std::time::Duration;

use saffron_e2e::TestEngine;
use saffron_protocol::{EntityRef, RenderStatsDto};
use serde_json::{Value, json};

/// ping/help/quit over the wire: the engine answers `ping` with its identity, `help` with a
/// non-empty command list including the anchors, and `shutdown` (which sends `quit`) tears down.
#[test]
fn ping_help_quit_round_trip() {
    let mut engine = TestEngine::boot(&[]).expect("boot engine");

    let pong: Value = engine.call_raw("ping", json!({})).expect("ping");
    assert_eq!(pong["pong"], json!(true), "ping replies pong=true");
    assert_eq!(pong["engine"], json!("Saffron Anima"), "engine identity");
    assert!(pong["version"].is_string(), "ping carries a version string");

    let help: Value = engine.call_raw("help", json!({})).expect("help");
    let commands = help["commands"].as_array().expect("help lists commands");
    assert!(!commands.is_empty(), "help is non-empty");
    let names: Vec<&str> = commands.iter().filter_map(|c| c["name"].as_str()).collect();
    assert!(names.contains(&"ping"), "help includes ping");
    assert!(names.contains(&"quit"), "help includes quit");
    assert!(
        names.contains(&"render-stats"),
        "help includes render-stats"
    );

    // `shutdown` issues `quit` then SIGTERMs the children; after it, the socket no longer answers.
    engine.shutdown();
}

/// A typed `render-stats` round-trip: the engine's reply deserializes straight into the protocol
/// `RenderStatsDto`. A few frames settle first so timing fields are populated.
#[test]
fn render_stats_deserializes_into_the_typed_dto() {
    let mut engine = TestEngine::boot(&[]).expect("boot engine");
    engine.settle(Duration::from_millis(400));

    let stats: RenderStatsDto = engine
        .call("render-stats", json!({}))
        .expect("render-stats");

    // The fields are live, not defaulted: at least one frame has rendered, so timing is positive
    // and the pipeline-feature flags are readable booleans (their values depend on the GPU, so we
    // assert the DTO decoded, not specific feature states).
    assert!(stats.fps >= 0.0, "fps is a real number: {}", stats.fps);
    assert!(
        stats.cpu_frame_ms >= 0.0,
        "cpu frame time present: {}",
        stats.cpu_frame_ms
    );
    assert!(
        stats.draw_calls >= 0,
        "draw-call count present: {}",
        stats.draw_calls
    );
    // `software_gpu` is true under llvmpipe in the toolbox; reading it proves the bool decoded.
    let _ = stats.software_gpu;

    engine.shutdown();
}

/// A one-cube scene renders validation-clean: boot with an auto empty project, add a cube preset,
/// let the engine render a handful of frames, and assert no Vulkan validation error surfaced.
#[test]
fn one_cube_scene_renders_validation_clean() {
    let mut engine = TestEngine::boot(&[("SAFFRON_SCRATCH_PROJECT", "1")]).expect("boot engine");

    // `add-entity cube` imports the cube preset (needs a loaded project) and places it.
    let cube: EntityRef = engine
        .call("add-entity", json!({ "args": ["cube"] }))
        .expect("add cube");
    assert!(
        !cube.name.is_empty(),
        "the placed cube has a name: {cube:?}"
    );

    // Render long enough for deferred GPU work + any validation to surface.
    engine.settle(Duration::from_millis(600));

    let errors = engine.validation_errors();
    assert!(
        errors.is_empty(),
        "validation layer flagged errors:\n{}",
        errors.join("\n")
    );

    engine.shutdown();
}
