//! The `ScriptHostBridge` POD seam and the bindings routed through it, driven as real `.luau`
//! fixtures against a real `Scene` and a recording stub bridge, so each case asserts the binding
//! reached the bridge with the entity's uuid and shaped the POD result.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use glam::Vec3;
use serde_json::json;

use saffron_core::Uuid;
use saffron_scene::{
    CharacterController, ComponentRegistry, Entity, IdComponent, Scene, Script, ScriptSlot,
    Transform, register_builtin_components,
};
use saffron_script::{
    ContactInfo, ScriptHitTarget, ScriptHost, ScriptHostBridge, ScriptRagdollState, ScriptRayHit,
    ScriptRunError,
};

/// Builds a [`ContactInfo`] for the tests' a/b uuids.
fn contact(a: Uuid, b: Uuid, begin: bool, sensor: bool, point: Vec3, normal: Vec3) -> ContactInfo {
    let tag = |uuid: Uuid| (uuid != Uuid(0)).then_some(ScriptHitTarget::SceneEntity(uuid));
    ContactInfo {
        target_a: tag(a),
        target_b: tag(b),
        begin,
        sensor,
        point,
        normal,
    }
}

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn registry() -> Arc<ComponentRegistry> {
    Arc::new(register_builtin_components())
}

/// One recorded bridge call, so the test can assert which method ran with which args.
#[derive(Clone, Debug, PartialEq)]
enum Call {
    Raycast {
        origin: Vec3,
        dir: Vec3,
        max_dist: f32,
    },
    ApplyImpulse(Uuid, Vec3),
    AddForce(Uuid, Vec3),
    SetVelocity(Uuid, Vec3),
    SetMorphWeights(Uuid, Vec<f32>),
    GetVelocity(Uuid),
    SetRagdollEnabled(Uuid, bool),
    SetRagdollBlend(Uuid, bool, f32),
    RagdollState(Uuid),
    LogSink(Uuid, String),
}

/// A recording stub bridge: returns fixed POD results and logs every call into a shared
/// buffer the test reads back. The `hit_entity` is the uuid `sa.raycast` resolves through
/// the lent scene.
#[derive(Default)]
struct RecordingBridge {
    calls: RefCell<Vec<Call>>,
    hit_entity: Uuid,
}

impl RecordingBridge {
    fn shared(hit_entity: Uuid) -> Rc<Self> {
        Rc::new(Self {
            calls: RefCell::new(Vec::new()),
            hit_entity,
        })
    }
    fn record(&self, call: Call) {
        self.calls.borrow_mut().push(call);
    }
    fn calls(&self) -> Vec<Call> {
        self.calls.borrow().clone()
    }
}

impl ScriptHostBridge for RecordingBridge {
    fn raycast(&self, origin: Vec3, dir: Vec3, max_dist: f32) -> ScriptRayHit {
        self.record(Call::Raycast {
            origin,
            dir,
            max_dist,
        });
        ScriptRayHit {
            hit: true,
            target: Some(ScriptHitTarget::SceneEntity(self.hit_entity)),
            point: Vec3::new(11.0, 12.0, 13.0),
            normal: Vec3::new(0.0, 1.0, 0.0),
            distance: 42.0,
        }
    }
    fn sphere_cast(&self, origin: Vec3, dir: Vec3, _radius: f32, max_dist: f32) -> ScriptRayHit {
        self.record(Call::Raycast {
            origin,
            dir,
            max_dist,
        });
        ScriptRayHit {
            hit: true,
            target: None,
            point: Vec3::new(1.0, 2.0, 3.0),
            normal: Vec3::new(0.0, 0.0, 1.0),
            distance: 7.0,
        }
    }
    fn apply_impulse(&self, entity: Uuid, impulse: Vec3) {
        self.record(Call::ApplyImpulse(entity, impulse));
    }
    fn add_force(&self, entity: Uuid, force: Vec3) {
        self.record(Call::AddForce(entity, force));
    }
    fn set_velocity(&self, entity: Uuid, velocity: Vec3) {
        self.record(Call::SetVelocity(entity, velocity));
    }
    fn set_morph_weights(&self, entity: Uuid, weights: &[f32]) {
        self.record(Call::SetMorphWeights(entity, weights.to_vec()));
    }
    fn get_velocity(&self, entity: Uuid) -> Vec3 {
        self.record(Call::GetVelocity(entity));
        Vec3::new(100.0, 200.0, 300.0)
    }
    fn set_ragdoll_enabled(&self, rig: Uuid, enable: bool) -> bool {
        self.record(Call::SetRagdollEnabled(rig, enable));
        true
    }
    fn set_ragdoll_blend(&self, rig: Uuid, active: bool, body_weight: f32) {
        self.record(Call::SetRagdollBlend(rig, active, body_weight));
    }
    fn ragdoll_state(&self, rig: Uuid) -> ScriptRagdollState {
        self.record(Call::RagdollState(rig));
        ScriptRagdollState {
            present: true,
            active: true,
            body_weight: 0.5,
            bones: 9,
        }
    }
    fn vegetation_raycast(
        &self,
        _origin: Vec3,
        _dir: Vec3,
        _max_dist: f32,
    ) -> Option<saffron_script::ScriptPlantHit> {
        None
    }

    fn vegetation_nearest(
        &self,
        _position: Vec3,
        _radius: f32,
    ) -> Option<saffron_script::ScriptPlantHit> {
        None
    }

    fn vegetation_in_radius(
        &self,
        _position: Vec3,
        _radius: f32,
        _limit: usize,
    ) -> Vec<saffron_script::ScriptPlantHit> {
        Vec::new()
    }

    fn vegetation_damage(&self, _plant: &str, _amount: f32) -> bool {
        false
    }

    fn vegetation_harvest(&self, _plant: &str, _phenotype: u32) -> bool {
        false
    }

    fn log_sink(&self, sender: Uuid, message: &str) {
        self.record(Call::LogSink(sender, message.to_owned()));
    }
}

/// Adds an entity with a single-slot `Script` carrying `script_path`.
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

fn uuid_of(scene: &Scene, e: Entity) -> Uuid {
    scene
        .component::<IdComponent>(e)
        .map(|id| id.id)
        .expect("entity has an id")
}

/// The physics methods + `sa.raycast` + `sa.log` route to the installed bridge with the
/// entity's uuid; the raycast POD shapes into `{hit, distance, point, normal, entity}`
/// with `entity` resolved through the lent scene.
#[test]
fn physics_bindings_route_to_the_bridge_with_the_entity_uuid() {
    let mut scene = Scene::new();
    let e = entity_with_script(&mut scene, "body", "physics_caller.luau");
    scene
        .add_component(e, Transform::default())
        .expect("transform");
    let uuid = uuid_of(&scene, e);

    let bridge = RecordingBridge::shared(uuid);
    let mut host = ScriptHost::new();
    host.install_bridge(bridge.clone() as Rc<dyn ScriptHostBridge>);
    host.start_scripts(&mut scene, registry(), &fixtures())
        .expect("start");
    let failure = host.tick_scripts(&mut scene, registry(), None, 0.016);
    assert!(failure.is_none(), "tick should run clean: {failure:?}");

    let calls = bridge.calls();
    assert!(
        calls.contains(&Call::ApplyImpulse(uuid, Vec3::new(1.0, 2.0, 3.0))),
        "apply_impulse routed with the uuid + impulse: {calls:?}"
    );
    assert!(calls.contains(&Call::AddForce(uuid, Vec3::new(4.0, 5.0, 6.0))));
    assert!(calls.contains(&Call::SetVelocity(uuid, Vec3::new(7.0, 8.0, 9.0))));
    assert!(calls.contains(&Call::GetVelocity(uuid)));
    assert!(calls.contains(&Call::SetRagdollEnabled(uuid, true)));
    assert!(calls.contains(&Call::SetRagdollBlend(uuid, true, 0.5)));
    assert!(calls.contains(&Call::RagdollState(uuid)));
    assert!(
        calls.contains(&Call::Raycast {
            origin: Vec3::ZERO,
            dir: Vec3::new(0.0, 0.0, 1.0),
            max_dist: 100.0,
        }),
        "raycast routed with origin/dir/max_dist: {calls:?}"
    );
    // sa.log routed to the sink, tagged with the running instance's uuid.
    assert!(
        calls
            .iter()
            .any(|c| matches!(c, Call::LogSink(sender, msg) if *sender == uuid && msg == "hello from physics_caller")),
        "sa.log routed to log_sink with the sender uuid: {calls:?}"
    );

    // get_velocity's echoed Vec3 (100,200,300) landed in the position.
    scene
        .with_component::<Transform, _>(e, |t| {
            assert_eq!(
                t.translation,
                Vec3::new(100.0, 200.0, 300.0),
                "get_velocity echo"
            );
            // raycast: hit=1, distance=42, entity resolved (entity_ok=1).
            assert_eq!(
                t.rotation,
                Vec3::new(1.0, 42.0, 1.0),
                "raycast result shape"
            );
            // ragdoll: enabled=1, present=1, bones=9.
            assert_eq!(t.scale, Vec3::new(1.0, 1.0, 9.0), "ragdoll surface");
        })
        .expect("transform present");
}

/// The default no-op bridge: `sa.raycast` yields `{hit = false}` and the physics methods
/// are silent no-ops (no panic), so an unbridged session degrades cleanly.
#[test]
fn default_noop_bridge_yields_a_miss() {
    let mut scene = Scene::new();
    let e = entity_with_script(&mut scene, "body", "physics_caller.luau");
    scene
        .add_component(e, Transform::default())
        .expect("transform");

    // No install_bridge: the host carries the NoopBridge.
    let mut host = ScriptHost::new();
    host.start_scripts(&mut scene, registry(), &fixtures())
        .expect("start");
    let failure = host.tick_scripts(&mut scene, registry(), None, 0.016);
    assert!(
        failure.is_none(),
        "noop bridge tick should run clean: {failure:?}"
    );

    scene
        .with_component::<Transform, _>(e, |t| {
            // get_velocity → zero; raycast → hit=0, distance=0, entity nil (entity_ok=0).
            assert_eq!(t.translation, Vec3::ZERO, "noop get_velocity is zero");
            assert_eq!(t.rotation, Vec3::ZERO, "noop raycast is a miss");
            // enable_ragdoll → false (0); present → false (0); bones → 0.
            assert_eq!(t.scale, Vec3::ZERO, "noop ragdoll surface");
        })
        .expect("transform present");
}

/// `move_character` is a pure Scene write — no bridge call — that sets the controller's
/// desired (horizontal) velocity and the jump impulse.
#[test]
fn move_character_writes_the_controller_without_a_bridge_call() {
    let mut scene = Scene::new();
    let e = entity_with_script(&mut scene, "walker", "walker.luau");
    scene
        .add_component(e, Transform::default())
        .expect("transform");
    scene
        .add_component(e, CharacterController::default())
        .expect("controller");
    let uuid = uuid_of(&scene, e);

    let bridge = RecordingBridge::shared(uuid);
    let mut host = ScriptHost::new();
    host.install_bridge(bridge.clone() as Rc<dyn ScriptHostBridge>);
    host.start_scripts(&mut scene, registry(), &fixtures())
        .expect("start");
    host.tick_scripts(&mut scene, registry(), None, 0.016);

    scene
        .with_component::<CharacterController, _>(e, |c| {
            // Y is ignored; horizontal velocity is (x, 0, z); jump sets vertical to 5.
            assert_eq!(c.desired_velocity, Vec3::new(2.0, 0.0, 3.0));
            assert_eq!(c.vertical_velocity, 5.0);
        })
        .expect("controller present");
    assert!(
        bridge.calls().is_empty(),
        "move_character must not touch the bridge: {:?}",
        bridge.calls()
    );
}

/// A scene with two scripted contact recorders.
fn two_recorders() -> (Scene, Entity, Entity, Uuid, Uuid) {
    let mut scene = Scene::new();
    let a = entity_with_script(&mut scene, "a", "contact_recorder.luau");
    let b = entity_with_script(&mut scene, "b", "contact_recorder.luau");
    for e in [a, b] {
        scene.add_component(e, Transform::default()).expect("xf");
    }
    let ua = uuid_of(&scene, a);
    let ub = uuid_of(&scene, b);
    (scene, a, b, ua, ub)
}

/// A sensor Begin invokes `on_trigger_enter(self, other)` on both entities' scripts; a
/// sensor End invokes `on_trigger_exit`.
#[test]
fn sensor_contact_invokes_trigger_handlers_on_both_entities() {
    let (mut scene, a, b, ua, ub) = two_recorders();
    let mut host = ScriptHost::new();
    host.start_scripts(&mut scene, registry(), &fixtures())
        .expect("start");

    // Sensor Begin → on_trigger_enter on both.
    let failure = host.dispatch_contact(
        &mut scene,
        registry(),
        contact(ua, ub, true, true, Vec3::ZERO, Vec3::ZERO),
    );
    assert!(
        failure.is_none(),
        "trigger enter should run clean: {failure:?}"
    );
    // Sensor End → on_trigger_exit on both.
    host.dispatch_contact(
        &mut scene,
        registry(),
        contact(ua, ub, false, true, Vec3::ZERO, Vec3::ZERO),
    );

    for e in [a, b] {
        scene
            .with_component::<Transform, _>(e, |t| {
                assert_eq!(t.rotation.x, 1.0, "one on_trigger_enter");
                assert_eq!(t.rotation.y, 1.0, "one on_trigger_exit");
                assert_eq!(t.translation.x, 0.0, "no solid on_contact fired");
            })
            .expect("xf");
    }
}

/// A solid Begin invokes `on_contact(self, other, point, normal)` with the manifold as a
/// Vec3 and `other` resolved to the live handle, on both entities.
#[test]
fn solid_contact_invokes_on_contact_with_manifold() {
    let (mut scene, a, b, ua, ub) = two_recorders();
    let mut host = ScriptHost::new();
    host.start_scripts(&mut scene, registry(), &fixtures())
        .expect("start");

    let point = Vec3::new(3.5, 0.0, 0.0);
    let normal = Vec3::new(0.0, 0.25, 0.0);
    let failure = host.dispatch_contact(
        &mut scene,
        registry(),
        contact(ua, ub, true, false, point, normal),
    );
    assert!(
        failure.is_none(),
        "on_contact should run clean: {failure:?}"
    );

    for e in [a, b] {
        scene
            .with_component::<Transform, _>(e, |t| {
                assert_eq!(t.translation.x, 1.0, "one on_contact");
                assert_eq!(t.translation.y, point.x, "point crossed as a Vec3");
                assert_eq!(t.translation.z, normal.y, "normal crossed as a Vec3");
                assert_eq!(
                    t.rotation.x, 0.0,
                    "no trigger handler fired for a solid touch"
                );
                assert_eq!(t.scale.x, 1.0, "other resolved to a live handle");
            })
            .expect("xf");
    }
}

/// A solid End has no handler — it dispatches nothing.
#[test]
fn solid_end_dispatches_nothing() {
    let (mut scene, a, _b, ua, ub) = two_recorders();
    let mut host = ScriptHost::new();
    host.start_scripts(&mut scene, registry(), &fixtures())
        .expect("start");

    let failure = host.dispatch_contact(
        &mut scene,
        registry(),
        contact(ua, ub, false, false, Vec3::ZERO, Vec3::ZERO),
    );
    assert!(failure.is_none());
    scene
        .with_component::<Transform, _>(a, |t| {
            assert_eq!(t.translation, Vec3::ZERO);
            assert_eq!(t.rotation, Vec3::ZERO);
        })
        .expect("xf");
}

/// A missing handler is a silent skip: an entity whose script has no `on_contact` is not
/// an error.
#[test]
fn missing_handler_is_skipped() {
    let mut scene = Scene::new();
    // `counter.luau` has on_create/on_update but no contact handlers.
    let a = entity_with_script(&mut scene, "a", "counter.luau");
    let b = entity_with_script(&mut scene, "b", "contact_recorder.luau");
    for e in [a, b] {
        scene.add_component(e, Transform::default()).expect("xf");
    }
    let ua = uuid_of(&scene, a);
    let ub = uuid_of(&scene, b);

    let mut host = ScriptHost::new();
    host.start_scripts(&mut scene, registry(), &fixtures())
        .expect("start");
    let failure = host.dispatch_contact(
        &mut scene,
        registry(),
        contact(
            ua,
            ub,
            true,
            false,
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(0.0, 1.0, 0.0),
        ),
    );
    assert!(
        failure.is_none(),
        "missing handler is a silent skip: {failure:?}"
    );
    // b still got its on_contact.
    scene
        .with_component::<Transform, _>(b, |t| assert_eq!(t.translation.x, 1.0))
        .expect("xf");
}

/// A failing handler halts the dispatch and is returned as a `ScriptRunError`; the VM
/// survives a subsequent dispatch.
#[test]
fn failing_handler_returns_a_script_run_error() {
    let mut scene = Scene::new();
    let a = entity_with_script(&mut scene, "a", "contact_faulty.luau");
    let b = entity_with_script(&mut scene, "b", "contact_recorder.luau");
    for e in [a, b] {
        scene.add_component(e, Transform::default()).expect("xf");
    }
    let ua = uuid_of(&scene, a);
    let ub = uuid_of(&scene, b);

    let mut host = ScriptHost::new();
    host.start_scripts(&mut scene, registry(), &fixtures())
        .expect("start");
    let failure = host.dispatch_contact(
        &mut scene,
        registry(),
        contact(ua, ub, true, false, Vec3::ZERO, Vec3::ZERO),
    );
    let ScriptRunError {
        entity_uuid,
        script,
        message,
    } = failure.expect("the faulting handler is surfaced");
    assert_eq!(entity_uuid, ua);
    assert_eq!(script, "contact_faulty.luau");
    assert!(
        message.contains("deliberate contact failure"),
        "carries the raised message: {message}"
    );

    // The VM survives: a second dispatch against the clean recorder runs fine.
    let again = host.dispatch_contact(
        &mut scene,
        registry(),
        contact(ub, ua, true, true, Vec3::ZERO, Vec3::ZERO),
    );
    assert!(again.is_none(), "the VM survives a faulting handler");
}
