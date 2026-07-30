use std::sync::Mutex;

use glam::Vec2;
use saffron_physics::World;
use saffron_scene::Scene;
use saffron_sceneedit::PlayState;

use super::*;

impl HostLayer {
    /// Drives the host into a live mid-play session through the real play edge (`enter_play` →
    /// `reconcile_play_edge` → `RuntimeSession::start`), so a teardown test exercises a real
    /// play→quit. Requires the Jolt globals to install (`World::new`); the caller probes first
    /// and skips cleanly when the toolchain is absent.
    fn set_play_session_for_test(&mut self) {
        self.editor.enter_play().expect("enter play");
        self.reconcile_play_edge();
    }

    /// Whether the Jolt globals are still flagged installed (a test reads the post-teardown state).
    fn physics_init_for_test(&self) -> bool {
        self.runtime.physics_init()
    }
}

// The Jolt `Factory::sInstance` is a process global the world bring-up + `shutdown_physics`
// touch; serialize the tests that build/drop a world so they never race it (mirrors the
// physics crate's `JOLT_GLOBAL`). Recover from a poisoned lock — it only guards the global
// init race, so a panic in one test leaves no shared state to corrupt.
static JOLT_GLOBAL: Mutex<()> = Mutex::new(());
fn jolt_guard() -> std::sync::MutexGuard<'static, ()> {
    JOLT_GLOBAL.lock().unwrap_or_else(|p| p.into_inner())
}

/// A unique, never-touched asset root per test so two tests never share a scratch dir.
fn scratch_root(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "saffron-host-{tag}-{}-{:p}",
        std::process::id(),
        tag
    ))
}

/// A host layer built standalone (not editor-spawned), no shm — the GPU-free spine.
fn standalone(tag: &str) -> HostLayer {
    HostLayer::new(scratch_root(tag), false, false)
}

#[test]
fn host_layer_constructs_without_gpu() {
    let host = standalone("construct");

    // The editor, asset server, and animation runtime are live without a renderer.
    assert_eq!(host.editor().play_state, PlayState::Edit);
    assert!(!host.shm_publishing());
    assert!(!host.has_physics());

    // The clip loader is installed: a clip id that resolves against an empty catalog
    // returns the typed `ClipLoad` error (not a panic / not "no loader"), proving the
    // loader closure is wired.
    // (The loader is exercised indirectly; here we assert the two play-state hooks are
    // live tokens — dropping the layer must unsubscribe them with no dangling sub.)
    assert!(host.play_hooks_live(), "play-state hooks are subscribed");

    // Dropping (via an explicit detach) unsubscribes both play-state hooks.
    let mut host = host;
    host.teardown();
    assert!(
        !host.play_hooks_live(),
        "detach unsubscribed the play-state hooks (no dangling subscription)"
    );
}

#[test]
fn parent_death_sets_should_close() {
    // Editor-spawned: the captured pid is the live parent; a matching observation stays
    // alive, a mismatched one (the editor vanished → reparent) requests exit.
    let host = HostLayer::new(scratch_root("ppid"), true, false);
    let captured = host.editor_pid;

    assert_eq!(
        host.watch_parent(captured),
        ParentWatch::Alive,
        "an unchanged parent keeps running"
    );

    // A changed observation (the editor vanished → reparent away, parent now `None` or a
    // different pid) trips the watch. `None` is the unambiguous "no parent" reparent.
    assert_eq!(
        host.watch_parent(None),
        ParentWatch::ParentDied,
        "a changed parent (editor gone) requests exit"
    );

    // The update step turns the verdict into a session abort: an armed host that observes
    // a vanished parent returns `ParentDied` before touching the rest of the spine.
    let mut armed = HostLayer::new(scratch_root("ppid-armed"), true, false);
    assert_eq!(
        armed.update_session(TimeSpan::from_seconds(0.016), None),
        ParentWatch::ParentDied,
        "the session update aborts on the parent-death verdict"
    );

    // A non-editor-spawned host never watches, whatever the observed pid.
    let mut standalone = standalone("ppid-standalone");
    assert_eq!(
        standalone.watch_parent(None),
        ParentWatch::Alive,
        "a standalone host is never auto-killed by the parent watch"
    );
    assert_eq!(
        standalone.update_session(TimeSpan::from_seconds(0.016), None),
        ParentWatch::Alive,
        "a standalone session update runs the full spine"
    );
}

#[test]
fn preview_prune_clears_runtime() {
    let mut host = standalone("prune");

    // No preview yet: an update at Edit does not prune (the edge has not flipped).
    let dt = TimeSpan::from_seconds(0.016);
    host.update_session(dt, None);
    assert!(!host.editor().previewing());

    // Enter the asset preview as the active view (the `previewing()` edge false→true).
    let mut preview = Scene::new();
    let _ = preview.create_entity("preview-root");
    host.editor_mut().preview_scene = Some(preview);
    host.editor_mut().preview_active_view = true;
    assert!(host.editor().previewing());

    // The next update sees the edge and prunes the runtime exactly once. The runtime has
    // no per-entity entries to begin with (a fresh session), so the observable contract
    // is that the preview-active tracking flipped and the prune ran on the edge.
    assert_eq!(host.animation().session_entry_count(), 0);
    host.update_session(dt, None);
    assert!(
        host.preview_active,
        "the preview-active tracking flips on the enter edge"
    );
    assert_eq!(
        host.animation().session_entry_count(),
        0,
        "the runtime stays pruned across the preview"
    );

    // Leaving the preview (true→false) is the symmetric edge; the tracking flips back.
    host.editor_mut().preview_active_view = false;
    host.editor_mut().preview_scene = None;
    host.update_session(dt, None);
    assert!(
        !host.preview_active,
        "the preview-active tracking flips back on the leave edge"
    );
}

#[test]
fn update_order_is_animation_then_step() {
    // A play/step command this frame must take effect this frame: stepping while Paused
    // grants one fixed tick that `play_step_dt` (run after animation, inside update_session)
    // consumes, advancing `play_tick`. We arm a step, then a single update consumes it.
    let mut host = standalone("order");

    host.editor_mut().enter_play().expect("enter play");
    host.editor_mut().pause_play().expect("pause");
    let before = host.editor().play_tick;
    host.editor_mut().step_play(1).expect("step");

    let dt = TimeSpan::from_seconds(0.016);
    // The fly-cam look-delta is set non-zero, and the update must drain it to zero each
    // frame (a burst between frames is otherwise lost).
    host.editor_mut().fly_input.look_delta = Vec2::new(12.0, -7.0);

    let watch = host.update_session(dt, None);
    assert_eq!(watch, ParentWatch::Alive);

    assert_eq!(
        host.editor().play_tick,
        before + 1,
        "the stepped tick ran this frame (the gated step consumed it after animation)"
    );
    assert_eq!(
        host.editor().fly_input.look_delta,
        Vec2::ZERO,
        "the fly-cam look-delta is drained to zero each update"
    );
}

/// The play-edge wiring proper, CPU-only: entering Play builds a Jolt world from the play
/// scene, the per-frame `update_session` steps it (the `sim_tick` seam) and writes the
/// dynamic body's pose back into the play scene, and stopping drops the world and restores
/// Edit. This is the unit-level mirror of `physics-falling-box.test.ts`, exercised end to
/// end without a renderer.
#[test]
fn play_edge_builds_a_world_and_steps_the_box() {
    use saffron_scene::{Collider, Rigidbody, Transform};
    let _guard = jolt_guard();
    // Skip cleanly if the Jolt globals cannot install (no toolchain) — not a false pass.
    match World::new() {
        Ok(_) => {}
        Err(err) => {
            eprintln!("skipping: World::new failed: {err}");
            return;
        }
    }

    let mut host = standalone("play-edge");

    // A static floor (a wide thin collider, no rigidbody → implicitly static) and a dynamic
    // box dropped from y=5 (default 0.5 half-extent box, default Dynamic rigidbody).
    let scene = &mut host.editor_mut().scene;
    let floor = scene.create_entity("Floor");
    scene
        .add_component(
            floor,
            Collider {
                half_extents: glam::Vec3::new(10.0, 0.1, 10.0),
                ..Collider::default()
            },
        )
        .expect("floor collider");
    let cube = scene.create_entity("Box");
    scene
        .add_component(
            cube,
            Transform {
                translation: glam::Vec3::new(0.0, 5.0, 0.0),
                scale: glam::Vec3::ONE,
                rotation: glam::Vec3::ZERO,
            },
        )
        .expect("box transform");
    scene
        .add_component(cube, Collider::default())
        .expect("box collider");
    scene
        .add_component(cube, Rigidbody::default())
        .expect("box rigidbody");
    let cube_uuid = scene
        .component::<saffron_scene::IdComponent>(cube)
        .map(|id| id.id)
        .expect("box id");

    // No world in Edit — the authored box sits at its authored height.
    assert!(!host.has_physics(), "no world before play");
    assert_eq!(host.editor().scene.world_matrix(cube).w_axis.y, 5.0);

    // Enter play and tick: the first update reconciles the Edit→Playing edge (builds the
    // world from the play scene), then steps it.
    host.editor_mut().enter_play().expect("enter play");
    let dt = TimeSpan::from_seconds(0.016);
    // ~3s of fixed steps: the box falls under gravity and settles on the floor.
    for _ in 0..200 {
        host.update_session(dt, None);
    }

    assert!(host.has_physics(), "the world is live during play");
    // Read the play twin's world Y: it dropped well below the authored 5 and settled near
    // the floor top (0.1) + the box half-extent (0.5) ≈ 0.6, never tunneling through.
    let play_cube = host
        .editor_mut()
        .active_scene()
        .find_entity_by_uuid(cube_uuid)
        .expect("play twin");
    let settled_y = host
        .editor_mut()
        .active_scene()
        .world_matrix(play_cube)
        .w_axis
        .y;
    assert!(settled_y < 5.0, "the box fell from 5: now {settled_y}");
    assert!(
        (0.4..1.0).contains(&settled_y),
        "the box settled at ~floor-top + half-extent: {settled_y}"
    );

    // Stop: the world drops, Edit returns, and the authored box is untouched (never written
    // during play — the duplicate held every write).
    host.editor_mut().stop_play().expect("stop");
    host.update_session(dt, None);
    assert!(!host.has_physics(), "the world dropped on stop");
    assert_eq!(host.editor().play_state, PlayState::Edit);
    assert_eq!(
        host.editor().scene.world_matrix(cube).w_axis.y,
        5.0,
        "the authored box is back at its authored height"
    );

    host.teardown();
}

#[test]
fn teardown_unsubscribes_and_drops_in_order() {
    let _guard = jolt_guard();
    // Skip cleanly when the Jolt globals cannot install (no toolchain) — not a false pass.
    match World::new() {
        Ok(_) => {}
        Err(err) => {
            eprintln!("skipping: World::new failed: {err}");
            return;
        }
    }
    let mut host = standalone("teardown-order");

    // A play session active via the real play edge: a live Jolt world + script VM, with both
    // play-state subscriptions live. Quit can land here, mid-play.
    host.set_play_session_for_test();
    assert!(host.play_hooks_live(), "the play-state hooks start live");
    assert!(host.has_physics(), "the play world is present");
    assert!(
        host.physics_init_for_test(),
        "the Jolt globals are installed"
    );

    // Record the teardown order and assert it matches the pinned sequence.
    let mut steps = Vec::new();
    host.teardown_recording(&mut steps);
    assert_eq!(
        steps,
        vec![
            TeardownStep::ControlClosed,
            TeardownStep::ScriptsStopped,
            TeardownStep::PhysicsWorldDropped,
            TeardownStep::JoltGlobalsShutdown,
            TeardownStep::PlayHooksUnsubscribed,
            TeardownStep::GpuCachesCleared,
        ],
        "the teardown order drops every subsystem before the device"
    );

    // The world drops strictly before the Jolt-globals shutdown (a live world holds Jolt
    // bodies; shutting the Factory down first would be a UAF).
    let world_pos = steps
        .iter()
        .position(|s| *s == TeardownStep::PhysicsWorldDropped)
        .unwrap();
    let jolt_pos = steps
        .iter()
        .position(|s| *s == TeardownStep::JoltGlobalsShutdown)
        .unwrap();
    assert!(
        world_pos < jolt_pos,
        "the physics world dropped before the Jolt globals shut down"
    );

    // Post-teardown state: subscriptions gone, world gone, globals flagged down, script flag
    // cleared — back to the fresh, drop-safe state.
    assert!(
        !host.play_hooks_live(),
        "teardown unsubscribed both play-state hooks (no dangling subscription)"
    );
    assert!(!host.has_physics(), "the play world was dropped");
    assert!(
        !host.physics_init_for_test(),
        "the Jolt globals were shut down (physics_init cleared)"
    );
    assert!(!host.script_vm_active(), "the script VM flag was cleared");

    // Teardown is idempotent: a second pass is a clean no-op (a double on_detach must not
    // double-free or re-shutdown the globals).
    let mut again = Vec::new();
    host.teardown_recording(&mut again);
    assert!(!host.has_physics() && !host.physics_init_for_test());
}

#[test]
fn ref_caches_drop_before_renderer() {
    // The asset GPU `Ref` caches must empty strictly before the renderer's device/allocator
    // drops. The host has no renderer in the test harness, so we prove the host's half: the
    // teardown step that clears the caches (`GpuCachesCleared`) runs while the host is still
    // alive — i.e. before `on_detach` returns and the run loop drops the renderer. We assert
    // the cache-clear is the *last* teardown step (the renderer drop happens strictly after,
    // outside the host), and that the asset GPU caches are empty afterward.
    let _guard = jolt_guard();
    let mut host = standalone("ref-caches");

    let mut steps = Vec::new();
    host.teardown_recording(&mut steps);

    assert_eq!(
        steps.last().copied(),
        Some(TeardownStep::GpuCachesCleared),
        "clearing the GPU Ref caches is the final host teardown step, before the renderer drops"
    );
    // Every host-owned GPU cache is emptied (the last `Arc<GpuMesh>`/`Arc<GpuTexture>` drop
    // runs here, under the idle device, not after the allocator is gone).
    assert!(
        host.assets.asset_caches_are_empty(),
        "the GPU Ref caches are empty after the cache-clear step"
    );
}

#[test]
fn shm_drop_is_device_independent() {
    // Dropping the viewport shm publisher after the renderer is gone must still munmap +
    // shm_unlink cleanly — its `Drop` touches no device. Here there is no renderer at all, so
    // a host carrying an enabled segment that is dropped proves the shm teardown is fully
    // independent of any GPU state.
    use crate::viewport_shm::{ShmView, ShmViewConfig, ViewportShmPublisher};
    use std::ffi::CString;

    let name = saffron_test_support::unique_shm_name();
    let mut host = standalone("shm-drop");
    let mut shm = ViewportShmPublisher::new();
    shm.enable(ShmViewConfig {
        view: ShmView::Scene,
        name: name.clone(),
    })
    .expect("enable scene segment");
    host.attach_shm_publisher(shm);
    assert!(host.shm_publishing(), "the scene segment is enabled");

    // Tear the host's session down first (no renderer ever existed), then drop the host —
    // the shm publisher's `Drop` runs with no device present and unlinks the segment.
    host.teardown();
    drop(host);

    let cname = CString::new(name).unwrap();
    let opened = rustix::shm::open(
        cname.as_c_str(),
        rustix::shm::OFlags::RDONLY,
        rustix::shm::Mode::empty(),
    );
    assert!(
        opened.is_err(),
        "dropping the host shm_unlinked the segment with no device access"
    );
}
