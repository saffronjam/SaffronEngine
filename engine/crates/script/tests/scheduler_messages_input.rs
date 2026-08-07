//! The coroutine scheduler, inter-script messages, the input reads through a lent
//! `ScriptInputState`, and the hierarchy/query bindings. The scheduler and message cases drive
//! real `.luau` fixtures through a real `ScriptHost`; the rest drive a VM inside a session
//! guard.

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::json;

use saffron_scene::{
    Camera, ComponentRegistry, Entity, Scene, Script, ScriptInputState, ScriptSlot, Transform,
    derive_script_input_edges, register_builtin_components,
};
use saffron_script::{EntityHandle, ScriptHost, ScriptVm, enter_session};

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn registry() -> Arc<ComponentRegistry> {
    Arc::new(register_builtin_components())
}

fn entity_with_script(scene: &mut Scene, name: &str, script_path: &str) -> Entity {
    let e = scene.create_entity(name);
    scene
        .add_component(
            e,
            Script {
                scripts: vec![ScriptSlot {
                    script_path: script_path.to_owned(),
                    overrides: json!({}),
                }],
            },
        )
        .expect("add Script");
    e
}

fn position(scene: &Scene, e: Entity) -> (f32, f32, f32) {
    scene
        .with_component::<Transform, _>(e, |t| (t.translation.x, t.translation.y, t.translation.z))
        .expect("Transform present")
}

/// `sa.wait(delay)` resumes only once the accumulated `dt` crosses the threshold across
/// several `tick_scripts(dt)` calls — dt-driven, deterministic timing (never wall-clock).
#[test]
fn scheduler_wait_resumes_when_accumulated_dt_crosses_the_threshold() {
    let mut scene = Scene::new();
    // delay defaults to 0.1 in the fixture's properties.
    let e = entity_with_script(&mut scene, "waiter", "scheduler_wait.luau");

    let mut host = ScriptHost::new();
    host.start_scripts(&mut scene, registry(), &fixtures())
        .expect("start");
    // on_create spawned the task but it has not woken: still at the seed position.
    assert_eq!(position(&scene, e), (0.0, 0.0, 0.0));

    // Four ticks of 0.02s = 0.08s accumulated: still under the 0.1s threshold.
    for _ in 0..4 {
        host.tick_scripts(&mut scene, registry(), None, 0.02);
    }
    assert_eq!(
        position(&scene, e),
        (0.0, 0.0, 0.0),
        "the task must not wake before the accumulated dt reaches the delay"
    );

    // Two more ticks → 0.12s accumulated, crossing 0.1s: the task wakes and marks x = 1.
    host.tick_scripts(&mut scene, registry(), None, 0.02);
    host.tick_scripts(&mut scene, registry(), None, 0.02);
    assert_eq!(
        position(&scene, e),
        (1.0, 0.0, 0.0),
        "the task should resume once the accumulated dt crosses the delay"
    );

    host.stop_scripts();
}

/// A faulting coroutine logs and the VM survives: the entity keeps ticking across the
/// fault, never crashing the host.
#[test]
fn faulting_coroutine_logs_and_the_vm_survives() {
    let mut scene = Scene::new();
    let e = entity_with_script(&mut scene, "faulty", "faulty_coroutine.luau");

    let mut host = ScriptHost::new();
    host.start_scripts(&mut scene, registry(), &fixtures())
        .expect("start");

    // Tick past the coroutine's 0.05s wait so it wakes and errors; on_update keeps
    // advancing x each tick regardless. None of these ticks returns an instance error
    // (the coroutine fault is contained inside the scheduler, not an on_update fault).
    for _ in 0..6 {
        assert!(
            host.tick_scripts(&mut scene, registry(), None, 0.02)
                .is_none(),
            "on_update must keep running; the coroutine fault is contained"
        );
    }
    // Six ticks of on_update → x advanced by 6 (the dead coroutine did not stop it).
    assert_eq!(position(&scene, e).0, 6.0);
    assert!(host.is_running(), "the VM survives the coroutine fault");

    host.stop_scripts();
}

/// `entity:send("ping", payload)` invokes the target's `ping(self, sender, payload)`
/// after the loop, with the right sender and payload — and only the target receives it.
#[test]
fn send_reaches_the_target_with_sender_and_payload() {
    let mut scene = Scene::new();
    let receiver = entity_with_script(&mut scene, "receiver", "receiver.luau");
    let other = entity_with_script(&mut scene, "bystander", "receiver.luau");
    let _sender = entity_with_script(&mut scene, "sender", "sender.luau");

    let mut host = ScriptHost::new();
    host.start_scripts(&mut scene, registry(), &fixtures())
        .expect("start");

    // One tick: the sender sends; the message dispatches after the loop.
    host.tick_scripts(&mut scene, registry(), None, 0.016);

    // The receiver got payload=7 (x), sender resolved to "sender" (y=1), one call (z=1).
    assert_eq!(position(&scene, receiver), (7.0, 1.0, 1.0));
    // The bystander (same script, different name) is not the target: untouched.
    assert_eq!(position(&scene, other), (0.0, 0.0, 0.0));

    host.stop_scripts();
}

/// `sa.broadcast` reaches every instance carrying the handler; the payload ref is
/// released each tick (no leak): a broadcast every tick keeps delivering cleanly.
#[test]
fn broadcast_reaches_every_instance_and_releases_the_payload() {
    let mut scene = Scene::new();
    let a = entity_with_script(&mut scene, "a", "receiver.luau");
    let b = entity_with_script(&mut scene, "b", "receiver.luau");
    let _caster = entity_with_script(&mut scene, "caster", "broadcaster.luau");

    let mut host = ScriptHost::new();
    host.start_scripts(&mut scene, registry(), &fixtures())
        .expect("start");

    // The broadcaster sends once on its first tick.
    host.tick_scripts(&mut scene, registry(), None, 0.016);
    assert_eq!(position(&scene, a), (3.0, 0.0, 1.0));
    assert_eq!(position(&scene, b), (3.0, 0.0, 1.0));

    // Further ticks send no more (the broadcaster only fires once): the receivers'
    // call count (z) stays at 1, and the run stays clean (the payload ref was released
    // after the first dispatch — no accumulation, no leak surfacing as an error).
    for _ in 0..3 {
        assert!(
            host.tick_scripts(&mut scene, registry(), None, 0.016)
                .is_none(),
            "subsequent ticks must run clean"
        );
    }
    assert_eq!(position(&scene, a).2, 1.0, "no further deliveries");

    host.stop_scripts();
}

/// The input reads reflect a lent `ScriptInputState`: held keys, the one-tick
/// press/release edges (after `derive_script_input_edges`), the mouse position/delta/
/// scroll/buttons, and case-insensitive key lookup.
#[test]
fn input_bindings_read_the_lent_snapshot() {
    let mut scene = Scene::new();
    let vm = ScriptVm::new().expect("vm");
    vm.register_no_scene_globals().expect("globals");
    vm.register_scene_globals().expect("scene globals");

    // Tick 1: w + left go down, pointer at (10, 20), scroll 2.
    let mut input = ScriptInputState {
        held: ["w"].iter().map(|s| (*s).to_owned()).collect(),
        mouse_buttons: ["left"].iter().map(|s| (*s).to_owned()).collect(),
        mouse_x: 10.0,
        mouse_y: 20.0,
        scroll: 2.0,
        ..ScriptInputState::default()
    };
    derive_script_input_edges(&mut input);

    {
        let _guard = enter_session(&mut scene, registry(), Some(&mut input));
        vm.run_string(
            r#"
            assert(sa.is_key_down("w"), "w held")
            assert(sa.is_key_down("W"), "key lookup is case-insensitive")
            assert(sa.is_key_pressed("w"), "w pressed this tick")
            assert(not sa.is_key_up("w"), "w not released this tick")
            assert(not sa.is_key_down("a"), "a not held")
            assert(sa.is_mouse_down("left"), "left mouse held")
            assert(sa.is_mouse_pressed("left"), "left pressed this tick")
            local m = sa.mouse_position()
            assert(m.x == 10 and m.y == 20, "mouse position")
            assert(sa.mouse_scroll() == 2, "scroll")
            "#,
            "input-tick-1",
        )
        .expect("tick-1 input script");
    }

    // Tick 2: w released, d pressed, left released, pointer moves to (13, 18).
    input.held = ["d"].iter().map(|s| (*s).to_owned()).collect();
    input.mouse_buttons.clear();
    input.mouse_x = 13.0;
    input.mouse_y = 18.0;
    derive_script_input_edges(&mut input);

    {
        let _guard = enter_session(&mut scene, registry(), Some(&mut input));
        vm.run_string(
            r#"
            assert(not sa.is_key_down("w"), "w no longer held")
            assert(sa.is_key_up("w"), "w released this tick")
            assert(sa.is_key_pressed("d"), "d pressed this tick")
            assert(sa.is_mouse_up("left"), "left released this tick")
            local d = sa.mouse_delta()
            assert(math.abs(d.x - 3) < 1e-5 and math.abs(d.y - (-2)) < 1e-5, "mouse delta")
            "#,
            "input-tick-2",
        )
        .expect("tick-2 input script");
    }
}

/// With no input lent, the bindings read their documented defaults — never an error.
#[test]
fn input_bindings_default_with_no_snapshot() {
    let mut scene = Scene::new();
    let vm = ScriptVm::new().expect("vm");
    vm.register_no_scene_globals().expect("globals");
    vm.register_scene_globals().expect("scene globals");

    let _guard = enter_session(&mut scene, registry(), None);
    vm.run_string(
        r#"
        assert(not sa.is_key_down("w"), "no input → not held")
        assert(not sa.is_key_pressed("w"), "no input → no press edge")
        assert(sa.mouse_scroll() == 0, "no input → zero scroll")
        local m = sa.mouse_position()
        assert(m.x == 0 and m.y == 0, "no input → zero position")
        "#,
        "input-default",
    )
    .expect("default input script");
}

/// `e:set_parent(p)` reparents (guarded), `e:parent()` / `e:children()` reflect it after
/// the relink, `sa.spawn` mints a root, and the query bindings resolve.
#[test]
fn hierarchy_and_query_bindings_resolve() {
    let mut scene = Scene::new();
    let parent = scene.create_entity("parent");
    let child = scene.create_entity("child");
    let cam = scene.create_entity("cam");
    scene
        .add_component(
            cam,
            Camera {
                primary: true,
                ..Camera::default()
            },
        )
        .expect("camera");

    let vm = ScriptVm::new().expect("vm");
    vm.register_no_scene_globals().expect("globals");
    vm.register_scene_globals().expect("scene globals");
    vm.lua()
        .globals()
        .set("parent", EntityHandle::new(parent))
        .expect("set parent");
    vm.lua()
        .globals()
        .set("child", EntityHandle::new(child))
        .expect("set child");

    let _guard = enter_session(&mut scene, registry(), None);
    vm.run_string(
        r#"
        -- Reparent the child under the parent (guarded; keeps world).
        assert(child:set_parent(parent), "set_parent should succeed")
        assert(child:parent():valid(), "child now has a parent")
        assert(child:parent():name() == "parent", "parent resolves by name")
        local kids = parent:children()
        assert(#kids == 1 and kids[1]:name() == "child", "children reflect the reparent")
        assert(not parent:parent():valid(), "parent is a root")

        -- A self-parent is refused by the cycle guard.
        assert(not parent:set_parent(parent), "self-parent is refused")

        -- Query bindings.
        assert(sa.get_entity_by_name("child"):valid(), "get_entity_by_name resolves")
        assert(sa.get_entity_by_name("nope"):valid() == false, "missing name → invalid")
        assert(sa.primary_camera():valid(), "primary_camera resolves")
        assert(sa.primary_camera():name() == "cam", "primary_camera is the camera entity")

        -- find_by_uuid round-trips through :uuid().
        local id = child:uuid()
        assert(sa.find_by_uuid(id):name() == "child", "find_by_uuid resolves the uuid")
        assert(sa.find_by_uuid("0"):valid() == false, "uuid 0 → invalid")

        -- spawn mints a new root.
        local spawned = sa.spawn("spawned")
        assert(spawned:valid(), "spawn mints a live entity")
        assert(spawned:name() == "spawned", "spawn carries the name")
        assert(not spawned:parent():valid(), "spawn is a root")
        "#,
        "hierarchy-queries",
    )
    .expect("hierarchy/query script should run clean");
}

/// `find_all_by_name` returns every match (the multi-match `get_entity_by_name` cannot).
#[test]
fn find_all_by_name_returns_every_match() {
    let mut scene = Scene::new();
    let _a = scene.create_entity("dup");
    let _b = scene.create_entity("dup");
    let _c = scene.create_entity("unique");

    let vm = ScriptVm::new().expect("vm");
    vm.register_no_scene_globals().expect("globals");
    vm.register_scene_globals().expect("scene globals");

    let _guard = enter_session(&mut scene, registry(), None);
    vm.run_string(
        r#"
        local dups = sa.find_all_by_name("dup")
        assert(#dups == 2, "two entities named 'dup'")
        local uniques = sa.find_all_by_name("unique")
        assert(#uniques == 1, "one entity named 'unique'")
        local none = sa.find_all_by_name("missing")
        assert(#none == 0, "no match → empty list")
        "#,
        "find-all",
    )
    .expect("find_all_by_name script");
}
