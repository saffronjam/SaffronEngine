//! The `ScriptHost` lifecycle end to end: start/tick/stop, the class cache, field injection and
//! overrides, pause-on-error, and the deferred destroy plus relink.

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::json;

use saffron_scene::{
    ComponentRegistry, Entity, Scene, Script, ScriptSlot, Transform, register_builtin_components,
};
use saffron_script::ScriptHost;

/// The directory holding the test `.luau` fixtures.
fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// A shared component registry for the tests.
fn registry() -> Arc<ComponentRegistry> {
    Arc::new(register_builtin_components())
}

/// Adds an entity with a single-slot `Script` carrying `script_path` and `overrides`.
fn entity_with_script(
    scene: &mut Scene,
    name: &str,
    script_path: &str,
    overrides: serde_json::Value,
) -> Entity {
    let e = scene.create_entity(name);
    scene
        .add_component(
            e,
            Script {
                scripts: vec![ScriptSlot {
                    script_path: script_path.to_owned(),
                    overrides,
                }],
            },
        )
        .expect("add Script");
    e
}

/// Reads an entity's local Transform translation as a triple (after a session ends, so
/// the scene is back in the caller's hands).
fn position(scene: &Scene, e: Entity) -> (f32, f32, f32) {
    scene
        .with_component::<Transform, _>(e, |t| (t.translation.x, t.translation.y, t.translation.z))
        .expect("Transform present")
}

/// A scene with two scripted entities each running `counter.luau`.
#[test]
fn start_runs_on_create_once_then_tick_runs_on_update() {
    let mut scene = Scene::new();
    let a = entity_with_script(&mut scene, "a", "counter.luau", json!({}));
    let b = entity_with_script(&mut scene, "b", "counter.luau", json!({}));

    let mut host = ScriptHost::new();
    host.start_scripts(&mut scene, registry(), &fixtures())
        .expect("start");
    assert_eq!(host.instance_count(), 2, "two slots → two instances");
    assert!(host.is_running());

    // on_create ran once per instance: position.y == 1, position.x == 0 (no tick yet).
    assert_eq!(position(&scene, a), (0.0, 1.0, 0.0));
    assert_eq!(position(&scene, b), (0.0, 1.0, 0.0));

    // One tick: on_update advances x by speed (2) for each instance.
    assert!(
        host.tick_scripts(&mut scene, registry(), None, 0.016)
            .is_none()
    );
    assert_eq!(position(&scene, a), (2.0, 1.0, 0.0));
    assert_eq!(position(&scene, b), (2.0, 1.0, 0.0));

    // A second tick: instances persist across ticks (the same self table accumulates).
    assert!(
        host.tick_scripts(&mut scene, registry(), None, 0.016)
            .is_none()
    );
    assert_eq!(position(&scene, a), (4.0, 1.0, 0.0));
    assert_eq!(position(&scene, b), (4.0, 1.0, 0.0));

    host.stop_scripts();
    assert!(!host.is_running());
}

/// Field injection: a declared default is used when there is no override; an override
/// replaces it; a `sa.vec3` field is a fresh per-instance value (mutating one instance's
/// field does not bleed to another); a stale override key for a removed field is dropped.
#[test]
fn field_injection_defaults_overrides_and_per_instance_vec3() {
    let mut scene = Scene::new();
    // Entity A uses all declared defaults.
    let a = entity_with_script(&mut scene, "a", "fields.luau", json!({}));
    // Entity B overrides speed and the vec3 offset (a 3-number array), plus a stale key
    // for a field the class does not declare (must be dropped silently).
    let b = entity_with_script(
        &mut scene,
        "b",
        "fields.luau",
        json!({ "speed": 99, "offset": [100.0, 200.0, 300.0], "ghost": 7 }),
    );

    let mut host = ScriptHost::new();
    host.start_scripts(&mut scene, registry(), &fixtures())
        .expect("start");

    // A: default speed 3, offset (10,20,30) → on_create mutates offset.x to 11, writes
    // position = (speed=3, offset.x=11, offset.y=20), scale.x = #"ab" = 2.
    assert_eq!(position(&scene, a), (3.0, 11.0, 20.0));
    let scale_a = scene
        .with_component::<Transform, _>(a, |t| t.scale.x)
        .expect("scale");
    assert_eq!(scale_a, 2.0, "string default 'ab' has length 2");

    // B: override speed 99, offset (100,200,300) → on_create mutates offset.x to 101,
    // writes position = (99, 101, 200). The stale 'ghost' key was dropped (no error).
    assert_eq!(position(&scene, b), (99.0, 101.0, 200.0));

    // Per-instance vec3 isolation: A's offset.x became 11, B's became 101 — neither
    // bled into the other (a shared default would have compounded).
    host.stop_scripts();
}

/// Pause-on-error: the first instance whose `on_update` errors halts that tick and is
/// returned as a `ScriptRunError` with its uuid + script path + a traceback; the VM
/// survives a subsequent tick.
#[test]
fn tick_halts_on_first_error_and_vm_survives() {
    let mut scene = Scene::new();
    let bad = entity_with_script(&mut scene, "bad", "faulty.luau", json!({}));

    let mut host = ScriptHost::new();
    host.start_scripts(&mut scene, registry(), &fixtures())
        .expect("start");

    let bad_uuid = scene
        .component::<saffron_scene::IdComponent>(bad)
        .expect("id")
        .id;

    let err = host
        .tick_scripts(&mut scene, registry(), None, 0.016)
        .expect("the faulty instance must fail the tick");
    assert_eq!(err.entity_uuid, bad_uuid, "the failing entity's uuid");
    assert_eq!(err.script, "faulty.luau", "the failing script path");
    assert!(
        err.message.contains("deliberate update failure"),
        "message should carry the raised error: {}",
        err.message
    );
    assert!(
        err.message.contains("traceback") || err.message.contains("backtrace"),
        "message should carry a traceback: {}",
        err.message
    );

    // The VM survives: a second tick fails the same way, never panics.
    let again = host.tick_scripts(&mut scene, registry(), None, 0.016);
    assert!(
        again.is_some(),
        "the VM survives and the instance still fails"
    );
    assert!(host.is_running());
    host.stop_scripts();
}

/// Deferred destroy: `entity:destroy()` in `on_update` keeps the handle valid for the
/// rest of the handler, the entity is gone after the loop, and exactly one
/// relink_hierarchy ran (the survivors stay addressable).
#[test]
fn deferred_destroy_keeps_handle_valid_then_removes_after_loop() {
    let mut scene = Scene::new();
    let doomed = entity_with_script(&mut scene, "doomed", "self_destruct.luau", json!({}));
    // A second, non-scripted entity parented under the doomed one would be swept too;
    // here a sibling root just proves relink keeps the survivors intact.
    let survivor = scene.create_entity("survivor");

    let before = scene.len();
    assert!(scene.valid(doomed));

    let mut host = ScriptHost::new();
    host.start_scripts(&mut scene, registry(), &fixtures())
        .expect("start");

    // The script's on_update asserts the handle is still valid mid-handler; if the
    // destroy were immediate, that assert would error and the tick would return an
    // error. A clean None proves the deferral.
    assert!(
        host.tick_scripts(&mut scene, registry(), None, 0.016)
            .is_none(),
        "the self-destruct handler must run clean (destroy is deferred)"
    );

    // After the loop, the entity is gone and the survivor remains.
    assert!(
        !scene.valid(doomed),
        "the doomed entity was destroyed after the loop"
    );
    assert!(scene.valid(survivor), "the survivor is untouched");
    assert_eq!(scene.len(), before - 1, "exactly one entity removed");

    host.stop_scripts();
}

/// stop_scripts runs on_destroy on a now-detached scene (its entity access degrades to
/// a no-op, never an error) and drops the VM cleanly; a second start_scripts builds a
/// fresh session.
#[test]
fn stop_runs_on_destroy_detached_then_restarts_fresh() {
    let mut scene = Scene::new();
    let e = entity_with_script(&mut scene, "e", "lifecycle_log.luau", json!({}));

    let mut host = ScriptHost::new();
    host.start_scripts(&mut scene, registry(), &fixtures())
        .expect("start");
    assert_eq!(
        position(&scene, e),
        (7.0, 0.0, 0.0),
        "on_create wrote pos.x = 7"
    );
    assert_eq!(host.instance_count(), 1);

    // stop runs on_destroy with no scene bound — it must not panic or error, and the
    // host returns to the fresh state.
    host.stop_scripts();
    assert!(!host.is_running());
    assert_eq!(host.instance_count(), 0);

    // A second start builds a clean session on the same host.
    host.start_scripts(&mut scene, registry(), &fixtures())
        .expect("restart");
    assert_eq!(host.instance_count(), 1, "the restart rebuilt the instance");
    host.stop_scripts();
}

/// A slot whose class lacks `on_update` is a logged skip — the session continues with
/// the other slots, never a fatal start failure.
#[test]
fn slot_without_on_update_is_skipped_not_fatal() {
    let mut scene = Scene::new();
    let _good = entity_with_script(&mut scene, "good", "counter.luau", json!({}));
    let _bad = entity_with_script(&mut scene, "bad", "no_update.luau", json!({}));

    let mut host = ScriptHost::new();
    host.start_scripts(&mut scene, registry(), &fixtures())
        .expect("start should not be fatal on a bad slot");
    // Only the good slot became an instance; the no_update slot was skipped.
    assert_eq!(host.instance_count(), 1, "the bad slot was skipped");
    host.stop_scripts();
}

/// A missing script file is a logged skip too (the file does not exist under src_dir).
#[test]
fn missing_script_file_is_skipped() {
    let mut scene = Scene::new();
    let _e = entity_with_script(&mut scene, "e", "does_not_exist.luau", json!({}));

    let mut host = ScriptHost::new();
    host.start_scripts(&mut scene, registry(), &fixtures())
        .expect("start should tolerate a missing file");
    assert_eq!(
        host.instance_count(),
        0,
        "the missing-file slot was skipped"
    );
    host.stop_scripts();
}

/// The class cache: two slots pointing at the same script load the class once but get
/// independent instances (mutating one's field does not touch the other).
#[test]
fn class_cache_shares_the_class_table_not_the_instance() {
    let mut scene = Scene::new();
    // Two entities, same script, both use defaults.
    let a = entity_with_script(&mut scene, "a", "fields.luau", json!({}));
    let b = entity_with_script(&mut scene, "b", "fields.luau", json!({}));

    let mut host = ScriptHost::new();
    host.start_scripts(&mut scene, registry(), &fixtures())
        .expect("start");

    // Both started from offset (10,20,30); each on_create mutated its own offset.x to
    // 11 and wrote it to its own position.y. If the class default were shared, the
    // second instance would have seen 11 already and written 12.
    assert_eq!(position(&scene, a).1, 11.0);
    assert_eq!(position(&scene, b).1, 11.0);
    host.stop_scripts();
}
