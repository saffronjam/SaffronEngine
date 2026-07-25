//! The BLOCKING cross-arch bit-exact determinism gate.
//!
//! This harness proves that the vendored-Jolt-5.3.0 + `cxx` bridge, built with the determinism
//! flags (`JPH_CROSS_PLATFORM_DETERMINISTIC` + single precision + `-ffp-model=precise`
//! `-ffp-contract=off` + a confined `-mavx2`), produces a simulation whose per-step trace is
//! **byte-for-byte identical** run-to-run and build-to-build. Bit-exactness is a property of the
//! *build*, not of Jolt the library, so this gate is the only thing that proves the flag set
//! actually took. There is no tolerance knob: floats are compared as raw little-endian bytes,
//! folded into a SHA-256 whole-run hash.
//!
//! The scenario is a frozen fixture (hardcoded here, never loaded from disk so it cannot drift): a
//! 10-box dynamic stack on a static floor, one 4-bone passive SwingTwist ragdoll, one
//! `CharacterVirtual` driven by a fixed desired-velocity sequence, and one kinematic-bones rig
//! swept by a fixed step function so its capsules shove a parked dynamic box (the
//! `MoveKinematic`-vs-teleport contact-velocity subtlety, in the trace). It is stepped 600 fixed
//! substeps (10 s at `1/60`).
//!
//! ## Cross-arch status
//!
//! The toolbox is x86_64-only, so this harness verifies the **x86 half** thoroughly: the trace hash
//! is identical across repeated in-process runs *and* across a freshly rebuilt world, with the
//! determinism flags confirmed active end-to-end. The **aarch64 / ARM half** of the gate — the
//! non-negotiable `Rust-x86 hash == Rust-aarch64 hash` assertion — cannot be run on this hardware:
//! it must be run on the self-hosted aarch64 runner before the gate is unconditionally green. The
//! committed [`GOLDEN_TRACE_HASH`] is the x86_64 build's hash; the aarch64 runner re-derives the
//! same trace and asserts equality against it.

use glam::Vec3;
use saffron_core::Uuid;
use saffron_physics::{FIXED_STEP, MotionType, World};
use saffron_scene::{
    BonePhysics, BonePhysicsComponent, CharacterController, Collider, Entity, IdComponent, Joint,
    KinematicBones, Motion, Relationship, Rigidbody, Scene, Shape, SkinnedMesh, Transform,
};

/// Jolt's `Factory::sInstance` is a process-global the world bring-up touches through `sys::init`;
/// serialize the gate's world-building runs so they never race the unit tests in `lib.rs`.
static JOLT_GLOBAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn jolt_guard() -> std::sync::MutexGuard<'static, ()> {
    JOLT_GLOBAL.lock().unwrap_or_else(|p| p.into_inner())
}

/// The frozen scenario's step count: 600 fixed substeps = 10 s at `1/60`.
const STEP_COUNT: u32 = 600;

/// How many dynamic boxes the stack holds.
const STACK_HEIGHT: usize = 10;

/// The number of bones in the passive ragdoll.
const RAGDOLL_BONES: usize = 4;

/// The number of joints in the kinematic-bones rig.
const KINEMATIC_BONES: usize = 3;

/// The committed x86_64 golden trace hash (lowercase hex SHA-256 over the whole-run byte stream).
///
/// This is the comparison key the gate diffs against: it is recomputed from the live trace each run
/// and asserted equal here, and the aarch64 runner asserts its own trace hash equals this same
/// constant (the cross-arch half — see the module docs; deferred on x86-only hardware). A flag drift
/// (a missing `-ffp-contract=off`, a contracted FMA, an AVX-512 slip) changes this hash and fails
/// the gate. **Never relax the comparison to make a mismatch pass — escalate per the go/no-go rule.**
const GOLDEN_TRACE_HASH: &str = "9712c951875e10501a1d001dfd0d8a8d4c10eafd1dcf8ab1de074436570b7011";

/// A no-op mesh cook: the Box-only stack/floor of the fixture never reads a `.smesh`.
fn no_cook(_: Uuid) -> std::result::Result<saffron_geometry::Mesh, String> {
    Ok(saffron_geometry::Mesh::default())
}

/// The deterministic per-substep desired-velocity sequence the character is driven by. A fixed
/// function of the step index (never wall-clock or RNG), so the drive itself cannot introduce
/// nondeterminism: walk +x for the first third, idle for the middle third, walk +z for the last.
fn character_desired_velocity(step: u32) -> Vec3 {
    let third = STEP_COUNT / 3;
    if step < third {
        Vec3::new(2.0, 0.0, 0.0)
    } else if step < 2 * third {
        Vec3::ZERO
    } else {
        Vec3::new(0.0, 0.0, 2.0)
    }
}

/// The deterministic per-substep sweep position of the kinematic rig's root joint. A fixed
/// function of the step index (never wall-clock or RNG): the rig walks steadily along +x so its
/// kinematic bone capsules shove the dynamic box parked in their path.
fn kinematic_root_x(step: u32) -> f32 {
    -22.0 + (step as f32) * 0.02
}

/// The fixed scenario, fully owned: the scene, the live world, and the handles the trace samples in
/// creation order (the dynamic stack boxes, the character, the ragdoll rig uuid, the kinematic rig
/// joint entities, and the dynamic box those bones shove).
struct Scenario {
    scene: Scene,
    world: World,
    /// The dynamic stack-box entities, bottom-to-top (creation order).
    stack: Vec<Entity>,
    /// The `CharacterVirtual` entity.
    character: Entity,
    /// The ragdoll rig's stable id.
    ragdoll_rig: Uuid,
    /// The kinematic-bones rig's root joint entity, swept each substep.
    kinematic_root: Entity,
    /// The dynamic box the kinematic bones shove, sampled for the contact-velocity trace.
    kinematic_target: Uuid,
}

impl Scenario {
    /// Build the frozen scenario from scratch: a static floor, a `STACK_HEIGHT`-box dynamic stack,
    /// a `RAGDOLL_BONES`-bone passive SwingTwist ragdoll off to one side, and a `CharacterVirtual`
    /// on the floor. Everything is hardcoded so the fixture cannot drift.
    fn build() -> Scenario {
        let mut scene = Scene::new();

        // A wide static floor whose top face sits at y = 0 (centre at y = -0.5, half-height 0.5),
        // large enough that nothing walks or topples off it.
        spawn_static_box(
            &mut scene,
            "Floor",
            Vec3::new(0.0, -0.5, 0.0),
            Vec3::new(40.0, 0.5, 40.0),
        );

        // A vertical stack of 10 unit dynamic boxes, each resting on the one below with a small gap
        // so the solver settles them deterministically. Bottom box centre at y = 0.5.
        let mut stack = Vec::with_capacity(STACK_HEIGHT);
        for i in 0..STACK_HEIGHT {
            let y = 0.5 + (i as f32) * 1.02;
            let e = spawn_dynamic_box(&mut scene, &format!("Stack{i}"), Vec3::new(0.0, y, 0.0));
            stack.push(e);
        }

        // A 4-bone passive SwingTwist ragdoll lifted off to one side (well clear of the stack) so
        // it falls and settles without interacting with the stack — a clean, isolated ragdoll trace.
        let ragdoll_rig = spawn_chain_ragdoll(&mut scene, RAGDOLL_BONES, Vec3::new(10.0, 5.0, 0.0));

        // A character on the floor, away from both the stack and the ragdoll, driven by the fixed
        // desired-velocity sequence each substep.
        let character = spawn_character(&mut scene, Vec3::new(-10.0, 1.4, 0.0));

        // A kinematic-bones rig parked far along -x (clear of everything else), with a dynamic box
        // resting in the path of its +x sweep so the bone capsules shove it (a contact-velocity
        // trace, the load-bearing subtlety of MoveKinematic vs a teleport).
        let kinematic_root =
            spawn_kinematic_rig(&mut scene, KINEMATIC_BONES, Vec3::new(-22.0, 1.0, 0.0));
        let kinematic_target =
            spawn_dynamic_box(&mut scene, "KinTarget", Vec3::new(-18.0, 1.0, 0.0));
        let kinematic_target = scene.component::<IdComponent>(kinematic_target).unwrap().id;

        scene.relink_hierarchy();
        scene.update_world_transforms();

        let mut world = World::new().expect("world creation");
        let mut cook = no_cook;
        world.populate(&mut scene, &mut cook);
        world
            .add_character(character, &scene)
            .expect("character creation");
        let rig_entity = scene.find_entity_by_uuid(ragdoll_rig).expect("rig entity");
        world
            .enable_ragdoll(&scene, rig_entity)
            .expect("ragdoll build");
        world.build_bone_bodies(&mut scene);

        Scenario {
            scene,
            world,
            stack,
            character,
            ragdoll_rig,
            kinematic_root,
            kinematic_target,
        }
    }

    /// Run the full scenario, folding every step's body/character/ragdoll-part bytes into a
    /// SHA-256, and return `(hash_hex, finite, kinematic_shoved)`. `finite` is `false` if any
    /// sampled value was non-finite at any step (an exploded constraint), which the gate fails on;
    /// `kinematic_shoved` is `true` once the kinematic bone capsules pushed the target box (so the
    /// trace addition is proven load-bearing, not inert).
    fn run(self) -> (String, bool, bool) {
        self.run_traced(None)
    }

    /// [`Scenario::run`] with an optional per-step trace sink for [`divergence_probe`]: each
    /// step's bytes land in `TRACE_SECTIONS`-ordered buffers, which are folded into the run
    /// hash in that same order — the hashed byte stream is identical with or without a sink.
    fn run_traced(
        mut self,
        mut probe: Option<&mut Vec<[Vec<u8>; TRACE_SECTIONS.len()]>>,
    ) -> (String, bool, bool) {
        let mut hasher = Sha256::new();
        let mut finite = true;
        let mut kinematic_shoved = false;

        for step in 0..STEP_COUNT {
            let mut sections: [Vec<u8>; TRACE_SECTIONS.len()] = Default::default();
            // Drive the character with the fixed, step-indexed velocity before the step.
            let desired = character_desired_velocity(step);
            let _ = self
                .scene
                .with_component_mut::<CharacterController, _>(self.character, |c| {
                    c.desired_velocity = desired;
                });

            // Sweep the kinematic rig's root joint along +x by the fixed step function, then
            // refresh the world transforms so the per-bone MoveKinematic reads the new pose.
            let root_x = kinematic_root_x(step);
            let _ = self
                .scene
                .with_component_mut::<Transform, _>(self.kinematic_root, |t| {
                    t.translation.x = root_x;
                });
            self.scene.update_world_transforms();

            self.world.step(&mut self.scene, FIXED_STEP);

            // 1) Each dynamic stack box, in creation order: its world position + rotation, read off
            //    the entity's Transform (the step wrote it back). The rotation comes back as the
            //    Transform's Euler — but the trace must hash a stable, decompose-free quantity, so
            //    sample the body's raw world pose through the world's body list by uuid instead.
            for &e in &self.stack {
                let t = self
                    .scene
                    .component::<Transform>(e)
                    .expect("stack box transform");
                finite &= t.translation.is_finite();
                put_vec3(&mut sections[0], t.translation);
                // The Transform stores rotation as Euler radians; hash it directly (it is the
                // write-back the engine itself produces, so it is the load-bearing quantity).
                put_vec3(&mut sections[0], t.rotation);
            }

            // 2) The character's resolved world position.
            let cp = self
                .scene
                .component::<Transform>(self.character)
                .expect("character transform")
                .translation;
            finite &= cp.is_finite();
            put_vec3(&mut sections[1], cp);

            // 3) Each ragdoll part's WORLD transform, pre-blend, read straight from the solver.
            let parts = self.world.ragdoll_part_count(self.ragdoll_rig);
            for part in 0..parts {
                let (pos, rot) = self
                    .world
                    .ragdoll_part_transform(self.ragdoll_rig, part)
                    .expect("ragdoll part transform");
                finite &= pos.is_finite() && rot.is_finite();
                put_vec3(&mut sections[2], pos);
                put_f32(&mut sections[2], rot.x);
                put_f32(&mut sections[2], rot.y);
                put_f32(&mut sections[2], rot.z);
                put_f32(&mut sections[2], rot.w);
            }

            // 4) Each Kinematic bone body's resolved world position, in creation order (the bodies
            //    list keeps it), so the swept kinematic motion is part of the trace.
            for body in self.world.list_bodies() {
                if body.motion == MotionType::Kinematic {
                    finite &= body.position.is_finite();
                    put_vec3(&mut sections[3], body.position);
                }
            }

            // 5) The dynamic box the kinematic bones shove: its linear velocity, which only grows
            //    if the swept bodies imparted contact velocity (a teleport would leave it still).
            let kt = self.world.body_linear_velocity(self.kinematic_target);
            finite &= kt.is_finite();
            kinematic_shoved |= kt.x > 0.05;
            put_vec3(&mut sections[4], kt);

            for section in &sections {
                hasher.update(section);
            }
            if let Some(sink) = probe.as_deref_mut() {
                sink.push(sections);
            }
        }

        (hasher.finish_hex(), finite, kinematic_shoved)
    }
}

/// The named per-step trace sections [`Scenario::run_traced`] buffers, in hash order.
const TRACE_SECTIONS: [&str; 5] = [
    "stack write-back",
    "character position",
    "ragdoll parts",
    "kinematic bone bodies",
    "kinematic target velocity",
];

/// Append a `Vec3` as three little-endian `f32`s (12 bytes), the GPU/file layout glam guarantees.
fn put_vec3(buf: &mut Vec<u8>, v: Vec3) {
    put_f32(buf, v.x);
    put_f32(buf, v.y);
    put_f32(buf, v.z);
}

/// Append one `f32` as its 4 little-endian bytes — the frozen trace element. No tolerance: the raw
/// bit pattern is the comparison unit.
fn put_f32(buf: &mut Vec<u8>, value: f32) {
    buf.extend_from_slice(&value.to_le_bytes());
}

/// Spawn a static box collider of the given half-extents at `translation`.
fn spawn_static_box(scene: &mut Scene, name: &str, translation: Vec3, half: Vec3) -> Entity {
    let e = scene.create_entity(name);
    scene
        .with_component_mut::<Transform, _>(e, |t| t.translation = translation)
        .unwrap();
    scene
        .add_component(
            e,
            Collider {
                half_extents: half,
                ..Collider::default()
            },
        )
        .unwrap();
    e
}

/// Spawn a unit dynamic box at `translation` (half-extents 0.5, mass 1 kg, the component default).
fn spawn_dynamic_box(scene: &mut Scene, name: &str, translation: Vec3) -> Entity {
    let e = scene.create_entity(name);
    scene
        .with_component_mut::<Transform, _>(e, |t| t.translation = translation)
        .unwrap();
    scene.add_component(e, Collider::default()).unwrap();
    scene
        .add_component(
            e,
            Rigidbody {
                motion: Motion::Dynamic,
                ..Rigidbody::default()
            },
        )
        .unwrap();
    e
}

/// Spawn a `CharacterVirtual` entity: a capsule collider + the controller, on the floor.
fn spawn_character(scene: &mut Scene, translation: Vec3) -> Entity {
    let e = scene.create_entity("Character");
    scene
        .with_component_mut::<Transform, _>(e, |t| t.translation = translation)
        .unwrap();
    scene
        .add_component(
            e,
            Collider {
                shape: Shape::Capsule,
                half_extents: Vec3::new(0.3, 0.6, 0.3),
                ..Collider::default()
            },
        )
        .unwrap();
    scene
        .add_component(
            e,
            CharacterController {
                max_speed: 2.0,
                max_step_height: 0.3,
                ..CharacterController::default()
            },
        )
        .unwrap();
    e
}

/// Build a `count`-joint kinematic-bones rig whose joints are independent children of a rig at the
/// origin (siblings, so moving the root joint does not drag the others), each at world height
/// `origin.y` and spaced `+0.5` along +x from `origin.x`. The root joint's local x is swept each
/// substep (its local frame is the rig at the origin, so local == world). Carries a `SkinnedMesh`,
/// a `BonePhysicsComponent` of capsules, and an enabled `KinematicBones` (empty `driven` = every
/// joint). Returns the **root joint** entity.
fn spawn_kinematic_rig(scene: &mut Scene, count: usize, origin: Vec3) -> Entity {
    let rig = scene.create_entity("KinematicRig");
    // The rig stays at the origin so each joint's local translation is its world translation.
    let rig_uuid = scene.component::<IdComponent>(rig).unwrap().id;

    let mut bone_uuids = Vec::with_capacity(count);
    let mut root = None;
    for i in 0..count {
        let joint = scene.create_entity(format!("KinJoint{i}"));
        scene
            .with_component_mut::<Transform, _>(joint, |t| {
                t.translation = Vec3::new(origin.x + (i as f32) * 0.5, origin.y, origin.z);
            })
            .unwrap();
        scene
            .with_component_mut::<Relationship, _>(joint, |r| r.parent = rig_uuid)
            .unwrap();
        bone_uuids.push(scene.component::<IdComponent>(joint).unwrap().id);
        if i == 0 {
            root = Some(joint);
        }
    }

    scene
        .add_component(
            rig,
            SkinnedMesh {
                bones: bone_uuids,
                ..SkinnedMesh::default()
            },
        )
        .unwrap();
    scene
        .add_component(
            rig,
            BonePhysicsComponent {
                bones: vec![
                    BonePhysics {
                        shape_half_extents: Vec3::new(0.25, 0.25, 0.25),
                        ..BonePhysics::default()
                    };
                    count
                ],
            },
        )
        .unwrap();
    scene
        .add_component(
            rig,
            KinematicBones {
                enabled: true,
                driven: Vec::new(),
            },
        )
        .unwrap();
    root.expect("a kinematic rig has at least one joint")
}

/// Build a `count`-bone chain rig (root + children stacked along +y), each bone parented to the
/// previous, carrying a `SkinnedMesh` + a `BonePhysicsComponent` of SwingTwist bones, and return
/// the rig's stable uuid. The whole rig is offset to `origin`.
fn spawn_chain_ragdoll(scene: &mut Scene, count: usize, origin: Vec3) -> Uuid {
    let rig = scene.create_entity("RagdollRig");
    scene
        .with_component_mut::<Transform, _>(rig, |t| t.translation = origin)
        .unwrap();

    let mut bone_uuids = Vec::with_capacity(count);
    let mut prev: Option<Entity> = None;
    for i in 0..count {
        let bone = scene.create_entity(format!("Bone{i}"));
        scene
            .with_component_mut::<Transform, _>(bone, |t| {
                // Root bone sits at the rig origin; children stack +0.5 along y from the parent.
                t.translation = if i == 0 {
                    origin
                } else {
                    Vec3::new(0.0, 0.5, 0.0)
                };
            })
            .unwrap();
        if let Some(parent) = prev {
            let parent_uuid = scene.component::<IdComponent>(parent).unwrap().id;
            scene
                .with_component_mut::<Relationship, _>(bone, |r| r.parent = parent_uuid)
                .unwrap();
        } else {
            // The root bone is a child of the rig so its world pose is the rig origin.
            let rig_uuid = scene.component::<IdComponent>(rig).unwrap().id;
            scene
                .with_component_mut::<Relationship, _>(bone, |r| r.parent = rig_uuid)
                .unwrap();
        }
        bone_uuids.push(scene.component::<IdComponent>(bone).unwrap().id);
        prev = Some(bone);
    }

    scene
        .add_component(
            rig,
            SkinnedMesh {
                bones: bone_uuids,
                ..SkinnedMesh::default()
            },
        )
        .unwrap();
    scene
        .add_component(
            rig,
            BonePhysicsComponent {
                bones: vec![
                    BonePhysics {
                        shape_half_extents: Vec3::new(0.1, 0.25, 0.1),
                        mass: 1.0,
                        joint: Joint::SwingTwist,
                        swing_twist_limits: Vec3::splat(0.5),
                        ..BonePhysics::default()
                    };
                    count
                ],
            },
        )
        .unwrap();

    scene.component::<IdComponent>(rig).unwrap().id
}

/// The end-to-end determinism gate.
///
/// Asserts, in order:
/// 1. the determinism flags are confirmed active (the contract the hash depends on);
/// 2. the two hard features ran and produced finite, bounded output;
/// 3. the trace hash is identical across two from-scratch builds (in-process repeatability);
/// 4. the trace hash equals the committed x86 golden hash (build-to-build stability).
///
/// The aarch64 half (`Rust-x86 hash == Rust-aarch64 hash`) is DEFERRED-NEEDS-HARDWARE — it runs on
/// the self-hosted aarch64 runner against the same [`GOLDEN_TRACE_HASH`]; it cannot be run here.
#[test]
fn determinism_gate() {
    let _guard = jolt_guard();

    // (1) The flag contract the whole gate rests on — proven end-to-end (the shim's `#error`
    //     guards would have failed the build, and `build.rs` only sets `cfg(jolt_deterministic)`
    //     when both halves held). Surface it at runtime too so the gate's premise is explicit.
    assert!(
        saffron_physics_sys::is_deterministic(),
        "the shim was not compiled with JPH_CROSS_PLATFORM_DETERMINISTIC — the gate's premise fails"
    );
    assert!(
        saffron_physics_sys::is_single_precision(),
        "the shim was not compiled in single precision — the gate's premise fails"
    );

    // (2) + (3): run the scenario twice from scratch and compare. Each run builds a brand-new world
    //     and steps the fixed scenario; a deterministic build yields the identical byte stream.
    let (hash_a, finite_a, shoved_a) = Scenario::build().run();
    let (hash_b, finite_b, _shoved_b) = Scenario::build().run();

    assert!(
        finite_a && finite_b,
        "the scenario produced a non-finite value — the ragdoll/character blew up (a divergent or \
         unstable sim, never bit-exact)"
    );

    // The kinematic-bones rig must actually have shoved its target box (a contact-velocity push,
    // not a teleport) — otherwise the trace addition is inert and proves nothing.
    assert!(
        shoved_a,
        "the swept kinematic bones never imparted contact velocity to the target box — the \
         MoveKinematic drive is not working (a teleport gives zero contact velocity)"
    );

    assert_eq!(
        hash_a, hash_b,
        "two from-scratch runs of the fixed scenario produced different trace hashes — the sim is \
         NON-DETERMINISTIC on this build. This is a BLOCKING failure: escalate per the go/no-go \
         rule, do not relax the comparison."
    );

    // (4) Build-to-build stability: the committed golden hash. A mismatch means the build changed
    //     the float results (a flag drift, a contracted FMA, an arch slip) — a real regression of
    //     the determinism contract, not a tolerance to widen.
    assert_eq!(
        hash_a, GOLDEN_TRACE_HASH,
        "the trace hash drifted from the committed x86 golden hash. If the scenario or the bridge \
         legitimately changed, RE-FREEZE the golden hash to the new value (and re-run the aarch64 \
         half); otherwise this is a determinism-flag regression — escalate per the go/no-go rule."
    );
}

/// Diagnostic probe for a `determinism_gate` repeatability failure: runs the scenario twice with
/// per-step trace capture and names the first step + section whose bytes differ, so a
/// nondeterminism report points at the mechanism (stack write-back vs character vs ragdoll vs
/// kinematic bones vs contact velocity) instead of one whole-run hash mismatch. Ignored in the
/// normal run; run it with `--ignored` when the gate reports differing in-process hashes.
#[test]
#[ignore = "diagnostic probe; run with --ignored when determinism_gate reports differing hashes"]
fn divergence_probe() {
    let _guard = jolt_guard();

    let mut trace_a = Vec::new();
    let mut trace_b = Vec::new();
    let (hash_a, ..) = Scenario::build().run_traced(Some(&mut trace_a));
    let (hash_b, ..) = Scenario::build().run_traced(Some(&mut trace_b));
    if hash_a == hash_b {
        println!("no divergence: two runs produced identical traces ({hash_a})");
        return;
    }
    for (step, (a, b)) in trace_a.iter().zip(&trace_b).enumerate() {
        for (index, name) in TRACE_SECTIONS.iter().enumerate() {
            assert!(
                a[index] == b[index],
                "first divergence at step {step}, section '{name}':\n  run A: {:02x?}\n  run B: {:02x?}",
                a[index],
                b[index]
            );
        }
    }
    panic!(
        "hashes differ but no per-step section diverged — trace lengths {} vs {}",
        trace_a.len(),
        trace_b.len()
    );
}

/// A minimal, dependency-free SHA-256 (FIPS 180-4), used only to fold the trace byte stream into a
/// stable comparison key. Inlined as a test helper rather than pulling a crypto crate into the gate
/// — the algorithm is fixed and self-contained, so it adds no determinism risk of its own.
struct Sha256 {
    state: [u32; 8],
    buffer: [u8; 64],
    buffer_len: usize,
    total_len: u64,
}

impl Sha256 {
    const K: [u32; 64] = [
        0x428a_2f98,
        0x7137_4491,
        0xb5c0_fbcf,
        0xe9b5_dba5,
        0x3956_c25b,
        0x59f1_11f1,
        0x923f_82a4,
        0xab1c_5ed5,
        0xd807_aa98,
        0x1283_5b01,
        0x2431_85be,
        0x550c_7dc3,
        0x72be_5d74,
        0x80de_b1fe,
        0x9bdc_06a7,
        0xc19b_f174,
        0xe49b_69c1,
        0xefbe_4786,
        0x0fc1_9dc6,
        0x240c_a1cc,
        0x2de9_2c6f,
        0x4a74_84aa,
        0x5cb0_a9dc,
        0x76f9_88da,
        0x983e_5152,
        0xa831_c66d,
        0xb003_27c8,
        0xbf59_7fc7,
        0xc6e0_0bf3,
        0xd5a7_9147,
        0x06ca_6351,
        0x1429_2967,
        0x27b7_0a85,
        0x2e1b_2138,
        0x4d2c_6dfc,
        0x5338_0d13,
        0x650a_7354,
        0x766a_0abb,
        0x81c2_c92e,
        0x9272_2c85,
        0xa2bf_e8a1,
        0xa81a_664b,
        0xc24b_8b70,
        0xc76c_51a3,
        0xd192_e819,
        0xd699_0624,
        0xf40e_3585,
        0x106a_a070,
        0x19a4_c116,
        0x1e37_6c08,
        0x2748_774c,
        0x34b0_bcb5,
        0x391c_0cb3,
        0x4ed8_aa4a,
        0x5b9c_ca4f,
        0x682e_6ff3,
        0x748f_82ee,
        0x78a5_636f,
        0x84c8_7814,
        0x8cc7_0208,
        0x90be_fffa,
        0xa450_6ceb,
        0xbef9_a3f7,
        0xc671_78f2,
    ];

    fn new() -> Self {
        Self {
            state: [
                0x6a09_e667,
                0xbb67_ae85,
                0x3c6e_f372,
                0xa54f_f53a,
                0x510e_527f,
                0x9b05_688c,
                0x1f83_d9ab,
                0x5be0_cd19,
            ],
            buffer: [0u8; 64],
            buffer_len: 0,
            total_len: 0,
        }
    }

    fn update(&mut self, data: &[u8]) {
        self.total_len = self.total_len.wrapping_add(data.len() as u64);
        let mut offset = 0;
        while offset < data.len() {
            let take = (64 - self.buffer_len).min(data.len() - offset);
            self.buffer[self.buffer_len..self.buffer_len + take]
                .copy_from_slice(&data[offset..offset + take]);
            self.buffer_len += take;
            offset += take;
            if self.buffer_len == 64 {
                let block = self.buffer;
                self.process_block(&block);
                self.buffer_len = 0;
            }
        }
    }

    fn process_block(&mut self, block: &[u8; 64]) {
        let mut w = [0u32; 64];
        for (i, chunk) in block.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let mut h = self.state;
        for (k, wi) in Self::K.iter().zip(w.iter()) {
            let s1 = h[4].rotate_right(6) ^ h[4].rotate_right(11) ^ h[4].rotate_right(25);
            let ch = (h[4] & h[5]) ^ (!h[4] & h[6]);
            let t1 = h[7]
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(*k)
                .wrapping_add(*wi);
            let s0 = h[0].rotate_right(2) ^ h[0].rotate_right(13) ^ h[0].rotate_right(22);
            let maj = (h[0] & h[1]) ^ (h[0] & h[2]) ^ (h[1] & h[2]);
            let t2 = s0.wrapping_add(maj);
            h[7] = h[6];
            h[6] = h[5];
            h[5] = h[4];
            h[4] = h[3].wrapping_add(t1);
            h[3] = h[2];
            h[2] = h[1];
            h[1] = h[0];
            h[0] = t1.wrapping_add(t2);
        }
        for (i, hv) in h.iter().enumerate() {
            self.state[i] = self.state[i].wrapping_add(*hv);
        }
    }

    fn finish_hex(mut self) -> String {
        let bit_len = self.total_len.wrapping_mul(8);
        self.update(&[0x80]);
        while self.buffer_len != 56 {
            self.update(&[0x00]);
        }
        self.update(&bit_len.to_be_bytes());

        let mut hex = String::with_capacity(64);
        for word in &self.state {
            for byte in word.to_be_bytes() {
                hex.push_str(&format!("{byte:02x}"));
            }
        }
        hex
    }
}

#[cfg(test)]
mod sha_self_test {
    use super::Sha256;

    /// The two canonical FIPS 180-4 test vectors, so a bug in the inlined SHA-256 can never silently
    /// pass the gate by producing a self-consistent but wrong hash.
    #[test]
    fn sha256_known_answers() {
        let mut empty = Sha256::new();
        empty.update(b"");
        assert_eq!(
            empty.finish_hex(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );

        let mut abc = Sha256::new();
        abc.update(b"abc");
        assert_eq!(
            abc.finish_hex(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
