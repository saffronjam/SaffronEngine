//! Byte-equality of the component JSON serde against the frozen wire bytes.
//!
//! Every `EXPECT_*` constant is the verbatim compact dump for the matching component value.
//! The tests construct the component, serialize it through the registry's per-type
//! fn-pointer, and assert the compact bytes match. Each also round-trips the fixture
//! (parse → re-serialize → compare) to prove the read path is byte-stable.
//!
//! The keys are alphabetically sorted in every fixture; since `serde_json` is built with
//! `preserve_order` for the control wire, the scene-save path and these tests serialize via
//! `saffron_json::dump_json_sorted` to keep that sorted byte shape. The float scalars carry
//! the full f64-promotion of the stored f32 (e.g. `0.1f` → `0.10000000149011612`).

use glam::{BVec3, Mat4, Vec3};
use serde_json::{Map, Value};

use saffron_core::Uuid;
use saffron_scene::{
    AnimationPlayer, Bone, BonePhysics, BonePhysicsComponent, Camera, CharacterController,
    Collider, ComponentRegistry, DirectionalLight, Entity, FootChain, FootIk, Joint,
    KinematicBones, Material, MaterialAsset, Mesh, ModelInstance, Motion, PhysicsMaterial,
    PointLight, ReflectionProbe, Relationship, Rigidbody, Scene, Script, ScriptSlot, Shape,
    SkinnedMesh, SpotLight, Transform, Transition, Wrap, environment_from_json,
    environment_to_json,
};

/// Serializes `component` through the registry's `serialize` fn-pointer for `name`, then
/// returns the compact JSON bytes — the exact path `serialize_entity` walks.
fn serialize_via_registry<C>(name: &str, component: C) -> String
where
    C: saffron_scene::Component,
{
    let reg = ComponentRegistry::default_builtins();
    let mut scene = Scene::new();
    let e = scene.create_entity("fixture");
    scene.add_component(e, component).unwrap();
    let row = reg.find_by_name(name).unwrap();
    let value = (row.serialize)(&scene, e);
    saffron_json::dump_json_sorted(&value, -1)
}

/// Deserializes the fixture text into a fresh component via the registry's `deserialize`
/// fn-pointer (which default-constructs the row then fills it), re-serializes it, and
/// asserts the bytes equal `expect` — the read path is byte-stable.
fn assert_round_trips(name: &str, expect: &str) {
    let reg = ComponentRegistry::default_builtins();
    let mut scene = Scene::new();
    let e = scene.create_entity("fixture");
    let parsed: Value = serde_json::from_str(expect).unwrap();
    let row = reg.find_by_name(name).unwrap();
    (row.deserialize)(&mut scene, e, &parsed).unwrap();
    let reserialized = (row.serialize)(&scene, e);
    assert_eq!(saffron_json::dump_json_sorted(&reserialized, -1), expect);
}

// A tiny helper for the registry, exposed only to the tests via an extension trait so the
// fixtures share one construction site.
trait RegistryExt {
    fn default_builtins() -> ComponentRegistry;
}
impl RegistryExt for ComponentRegistry {
    fn default_builtins() -> ComponentRegistry {
        saffron_scene::register_builtin_components()
    }
}

const EXPECT_TRANSFORM_DEFAULT: &str = r#"{"rotation":{"x":0.0,"y":0.0,"z":0.0},"scale":{"x":1.0,"y":1.0,"z":1.0},"translation":{"x":0.0,"y":0.0,"z":0.0}}"#;
const EXPECT_TRANSFORM_VALUES: &str = r#"{"rotation":{"x":0.10000000149011612,"y":0.20000000298023224,"z":0.30000001192092896},"scale":{"x":2.0,"y":2.0,"z":2.0},"translation":{"x":1.5,"y":-2.0,"z":3.25}}"#;
const EXPECT_CAMERA_DEFAULT: &str = r#"{"far":100.0,"fov":45.0,"frustumMaxDistance":10.0,"near":0.10000000149011612,"primary":true,"showFrustum":true,"showModel":true}"#;
const EXPECT_MATERIAL_DEFAULT: &str = r#"{"albedoTexture":"0","alphaCutoff":0.5,"baseColor":{"w":1.0,"x":1.0,"y":1.0,"z":1.0},"blend":"opaque","doubleSided":false,"emissive":{"x":0.0,"y":0.0,"z":0.0},"emissiveStrength":1.0,"emissiveTexture":"0","heightScale":0.05000000074505806,"heightTexture":"0","metallic":0.0,"metallicRoughnessTexture":"0","normalStrength":1.0,"normalTexture":"0","occlusionTexture":"0","roughness":1.0,"unlit":false}"#;
const EXPECT_MESH: &str = r#"{"mesh":"1024"}"#;
const EXPECT_MATERIALASSET: &str = r#"{"material":"4242"}"#;
const EXPECT_MODELINSTANCE: &str = r#"{"modelId":"9999"}"#;
const EXPECT_RELATIONSHIP: &str = r#"{"parent":"7"}"#;
const EXPECT_BONE: &str = r#"{}"#;
const EXPECT_ANIM_DEFAULT: &str = r#"{"autoplay":false,"clip":"0","loopBlend":0.0,"speed":1.0,"transitionMode":"inertialize","wrap":"loop"}"#;
const EXPECT_ANIM_VALUES: &str = r#"{"autoplay":true,"clip":"555","loopBlend":0.5,"speed":2.0,"transitionMode":"crossfade","wrap":"once"}"#;
const EXPECT_DIRLIGHT_DEFAULT: &str = r#"{"ambient":0.15000000596046448,"color":{"x":1.0,"y":1.0,"z":1.0},"direction":{"x":-0.5,"y":-1.0,"z":-0.30000001192092896},"intensity":1.0}"#;
const EXPECT_POINTLIGHT_DEFAULT: &str =
    r#"{"color":{"x":1.0,"y":1.0,"z":1.0},"intensity":5.0,"range":10.0}"#;
const EXPECT_SPOTLIGHT_DEFAULT: &str = r#"{"color":{"x":1.0,"y":1.0,"z":1.0},"direction":{"x":0.0,"y":-1.0,"z":0.0},"innerAngle":20.0,"intensity":5.0,"outerAngle":30.0,"range":10.0}"#;
const EXPECT_REFPROBE_DEFAULT: &str = r#"{"boxExtent":{"x":10.0,"y":10.0,"z":10.0},"boxProjection":false,"influenceRadius":10.0,"intensity":1.0}"#;
const EXPECT_SKINNED: &str = r#"{"bones":["100","200"],"inverseBind":[[1.0,0.0,0.0,0.0,0.0,1.0,0.0,0.0,0.0,0.0,1.0,0.0,0.0,0.0,0.0,1.0]],"mesh":"11","rootBone":"22"}"#;
const EXPECT_FOOTIK: &str = r#"{"chains":[{"end":2,"mid":1,"poleVector":{"x":0.0,"y":0.0,"z":1.0},"upper":0}],"enabled":true,"groundHeight":0.5}"#;
const EXPECT_BONEPHYS: &str = r#"{"bones":[{"driveDamping":0.20000000298023224,"driveMaxForce":100.0,"driveStiffness":1.0,"joint":"swingtwist","mass":2.0,"shapeHalfExtents":{"x":0.10000000149011612,"y":0.20000000298023224,"z":0.30000001192092896},"swingTwistLimits":{"x":0.5,"y":0.6000000238418579,"z":0.699999988079071}}]}"#;
const EXPECT_RIGIDBODY_DEFAULT: &str = r#"{"angularDamping":0.05000000074505806,"collisionLayer":0,"gravityFactor":1.0,"linearDamping":0.05000000074505806,"lockPosition":{"x":false,"y":false,"z":false},"lockRotation":{"x":false,"y":false,"z":false},"mass":1.0,"motion":"dynamic"}"#;
const EXPECT_RIGIDBODY_KIN: &str = r#"{"angularDamping":0.20000000298023224,"collisionLayer":3,"gravityFactor":0.0,"linearDamping":0.10000000149011612,"lockPosition":{"x":true,"y":false,"z":true},"lockRotation":{"x":false,"y":true,"z":false},"mass":5.0,"motion":"kinematic"}"#;
const EXPECT_COLLIDER_DEFAULT: &str = r#"{"halfExtents":{"x":0.5,"y":0.5,"z":0.5},"isSensor":false,"material":{"friction":0.5,"restitution":0.0},"offset":{"x":0.0,"y":0.0,"z":0.0},"shape":"box","sourceMesh":"0"}"#;
const EXPECT_COLLIDER_CAPSULE: &str = r#"{"halfExtents":{"x":0.30000001192092896,"y":1.0,"z":0.30000001192092896},"isSensor":true,"material":{"friction":0.800000011920929,"restitution":0.4000000059604645},"offset":{"x":0.0,"y":0.5,"z":0.0},"shape":"capsule","sourceMesh":"77"}"#;
const EXPECT_KINBONES_DEFAULT: &str = r#"{"driven":[],"enabled":true}"#;
const EXPECT_KINBONES_DRIVEN: &str = r#"{"driven":[0,3,7],"enabled":false}"#;
const EXPECT_CHARCTRL_DEFAULT: &str = r#"{"gravityFactor":1.0,"maxSlopeAngle":0.785398006439209,"maxSpeed":4.0,"maxStepHeight":0.30000001192092896}"#;
const EXPECT_SCRIPT: &str =
    r#"{"scripts":[{"overrides":{"health":100,"name":"hero"},"scriptPath":"player.lua"}]}"#;
const EXPECT_NAME: &str = r#"{"name":"Hello"}"#;
const EXPECT_ENV_DEFAULT: &str = r#"{"ambientColor":{"x":1.0,"y":1.0,"z":1.0},"ambientIntensity":0.15000000596046448,"atmosphere":{"atmosphereHeight":100.0,"enabled":false,"mieAnisotropy":0.800000011920929,"mieScaleHeight":1.2000000476837158,"mieScattering":3.996000051498413,"ozoneAbsorption":{"x":0.6499999761581421,"y":1.88100004196167,"z":0.08500000089406967},"planetRadius":6360.0,"rayleighScaleHeight":8.0,"rayleighScattering":{"x":5.802000045776367,"y":13.557999610900879,"z":33.099998474121094},"sunDiskAngularRadius":0.004650000017136335,"sunDiskIntensity":20.0},"clearColor":{"x":0.05000000074505806,"y":0.05999999865889549,"z":0.07999999821186066},"exposure":1.0,"skyIntensity":1.0,"skyMode":"procedural","skyRotation":0.0,"skyTexture":"0","useSkyForAmbient":true,"visible":true}"#;

#[test]
fn name_matches_cpp() {
    let c = saffron_scene::Name {
        name: "Hello".to_string(),
    };
    // `Name` is seeded by `create_entity`, so replace it then serialize.
    assert_eq!(serialize_via_registry("Name", c), EXPECT_NAME);
    assert_round_trips("Name", EXPECT_NAME);
}

#[test]
fn transform_matches_cpp() {
    assert_eq!(
        serialize_via_registry("Transform", Transform::default()),
        EXPECT_TRANSFORM_DEFAULT
    );
    let t = Transform {
        translation: Vec3::new(1.5, -2.0, 3.25),
        scale: Vec3::splat(2.0),
        rotation: Vec3::new(0.1, 0.2, 0.3),
    };
    assert_eq!(
        serialize_via_registry("Transform", t),
        EXPECT_TRANSFORM_VALUES
    );
    assert_round_trips("Transform", EXPECT_TRANSFORM_DEFAULT);
    assert_round_trips("Transform", EXPECT_TRANSFORM_VALUES);
}

#[test]
fn camera_matches_cpp() {
    assert_eq!(
        serialize_via_registry("Camera", Camera::default()),
        EXPECT_CAMERA_DEFAULT
    );
    assert_round_trips("Camera", EXPECT_CAMERA_DEFAULT);
}

#[test]
fn material_matches_cpp() {
    assert_eq!(
        serialize_via_registry("Material", Material::default()),
        EXPECT_MATERIAL_DEFAULT
    );
    assert_round_trips("Material", EXPECT_MATERIAL_DEFAULT);
}

#[test]
fn mesh_matches_cpp() {
    assert_eq!(
        serialize_via_registry("Mesh", Mesh { mesh: Uuid(1024) }),
        EXPECT_MESH
    );
    assert_round_trips("Mesh", EXPECT_MESH);
}

#[test]
fn material_asset_matches_cpp() {
    assert_eq!(
        serialize_via_registry(
            "MaterialAsset",
            MaterialAsset {
                material: Uuid(4242),
            }
        ),
        EXPECT_MATERIALASSET
    );
    assert_round_trips("MaterialAsset", EXPECT_MATERIALASSET);
}

#[test]
fn model_instance_matches_cpp() {
    assert_eq!(
        serialize_via_registry(
            "ModelInstance",
            ModelInstance {
                model_id: Uuid(9999),
            }
        ),
        EXPECT_MODELINSTANCE
    );
    assert_round_trips("ModelInstance", EXPECT_MODELINSTANCE);
}

#[test]
fn relationship_matches_cpp() {
    assert_eq!(
        serialize_via_registry(
            "Relationship",
            Relationship {
                parent: Uuid(7),
                ..Relationship::default()
            }
        ),
        EXPECT_RELATIONSHIP
    );
    assert_round_trips("Relationship", EXPECT_RELATIONSHIP);
}

#[test]
fn bone_serializes_as_empty_object() {
    assert_eq!(serialize_via_registry("Bone", Bone::default()), EXPECT_BONE);
    assert_round_trips("Bone", EXPECT_BONE);
}

#[test]
fn animation_player_matches_cpp() {
    assert_eq!(
        serialize_via_registry("AnimationPlayer", AnimationPlayer::default()),
        EXPECT_ANIM_DEFAULT
    );
    // `time` / `playing` are runtime-only (not serialized); `autoplay` is the persisted intent.
    let a = AnimationPlayer {
        clip: Uuid(555),
        autoplay: true,
        speed: 2.0,
        wrap: Wrap::Once,
        transition_mode: Transition::CrossFade,
        loop_blend: 0.5,
        ..AnimationPlayer::default()
    };
    assert_eq!(
        serialize_via_registry("AnimationPlayer", a),
        EXPECT_ANIM_VALUES
    );
    assert_round_trips("AnimationPlayer", EXPECT_ANIM_DEFAULT);
    assert_round_trips("AnimationPlayer", EXPECT_ANIM_VALUES);
}

#[test]
fn directional_light_matches_cpp() {
    assert_eq!(
        serialize_via_registry("DirectionalLight", DirectionalLight::default()),
        EXPECT_DIRLIGHT_DEFAULT
    );
    assert_round_trips("DirectionalLight", EXPECT_DIRLIGHT_DEFAULT);
}

#[test]
fn point_light_matches_cpp() {
    assert_eq!(
        serialize_via_registry("PointLight", PointLight::default()),
        EXPECT_POINTLIGHT_DEFAULT
    );
    assert_round_trips("PointLight", EXPECT_POINTLIGHT_DEFAULT);
}

#[test]
fn spot_light_matches_cpp() {
    assert_eq!(
        serialize_via_registry("SpotLight", SpotLight::default()),
        EXPECT_SPOTLIGHT_DEFAULT
    );
    assert_round_trips("SpotLight", EXPECT_SPOTLIGHT_DEFAULT);
}

#[test]
fn reflection_probe_matches_cpp() {
    assert_eq!(
        serialize_via_registry("ReflectionProbe", ReflectionProbe::default()),
        EXPECT_REFPROBE_DEFAULT
    );
    assert_round_trips("ReflectionProbe", EXPECT_REFPROBE_DEFAULT);
}

#[test]
fn skinned_mesh_matches_cpp() {
    let s = SkinnedMesh {
        mesh: Uuid(11),
        root_bone: Uuid(22),
        bones: vec![Uuid(100), Uuid(200)],
        inverse_bind: vec![Mat4::IDENTITY],
        bone_handles: vec![Entity::NULL],
    };
    assert_eq!(serialize_via_registry("SkinnedMesh", s), EXPECT_SKINNED);
    assert_round_trips("SkinnedMesh", EXPECT_SKINNED);
}

#[test]
fn foot_ik_matches_cpp() {
    let f = FootIk {
        enabled: true,
        ground_height: 0.5,
        chains: vec![FootChain {
            upper: 0,
            mid: 1,
            end: 2,
            pole_vector: Vec3::new(0.0, 0.0, 1.0),
        }],
    };
    assert_eq!(serialize_via_registry("FootIk", f), EXPECT_FOOTIK);
    assert_round_trips("FootIk", EXPECT_FOOTIK);
}

#[test]
fn bone_physics_matches_cpp() {
    let b = BonePhysicsComponent {
        bones: vec![BonePhysics {
            shape_half_extents: Vec3::new(0.1, 0.2, 0.3),
            mass: 2.0,
            joint: Joint::SwingTwist,
            swing_twist_limits: Vec3::new(0.5, 0.6, 0.7),
            drive_stiffness: 1.0,
            drive_damping: 0.2,
            drive_max_force: 100.0,
        }],
    };
    assert_eq!(serialize_via_registry("BonePhysics", b), EXPECT_BONEPHYS);
    assert_round_trips("BonePhysics", EXPECT_BONEPHYS);
}

#[test]
fn rigidbody_matches_cpp() {
    assert_eq!(
        serialize_via_registry("Rigidbody", Rigidbody::default()),
        EXPECT_RIGIDBODY_DEFAULT
    );
    let r = Rigidbody {
        motion: Motion::Kinematic,
        mass: 5.0,
        linear_damping: 0.1,
        angular_damping: 0.2,
        gravity_factor: 0.0,
        lock_position: BVec3::new(true, false, true),
        lock_rotation: BVec3::new(false, true, false),
        collision_layer: 3,
    };
    assert_eq!(serialize_via_registry("Rigidbody", r), EXPECT_RIGIDBODY_KIN);
    assert_round_trips("Rigidbody", EXPECT_RIGIDBODY_DEFAULT);
    assert_round_trips("Rigidbody", EXPECT_RIGIDBODY_KIN);
}

#[test]
fn collider_matches_cpp() {
    assert_eq!(
        serialize_via_registry("Collider", Collider::default()),
        EXPECT_COLLIDER_DEFAULT
    );
    let c = Collider {
        shape: Shape::Capsule,
        half_extents: Vec3::new(0.3, 1.0, 0.3),
        source_mesh: Uuid(77),
        offset: Vec3::new(0.0, 0.5, 0.0),
        material: PhysicsMaterial {
            friction: 0.8,
            restitution: 0.4,
        },
        is_sensor: true,
    };
    assert_eq!(
        serialize_via_registry("Collider", c),
        EXPECT_COLLIDER_CAPSULE
    );
    assert_round_trips("Collider", EXPECT_COLLIDER_DEFAULT);
    assert_round_trips("Collider", EXPECT_COLLIDER_CAPSULE);
}

#[test]
fn kinematic_bones_matches_cpp() {
    assert_eq!(
        serialize_via_registry("KinematicBones", KinematicBones::default()),
        EXPECT_KINBONES_DEFAULT
    );
    let k = KinematicBones {
        enabled: false,
        driven: vec![0, 3, 7],
    };
    assert_eq!(
        serialize_via_registry("KinematicBones", k),
        EXPECT_KINBONES_DRIVEN
    );
    assert_round_trips("KinematicBones", EXPECT_KINBONES_DEFAULT);
    assert_round_trips("KinematicBones", EXPECT_KINBONES_DRIVEN);
}

#[test]
fn character_controller_matches_cpp() {
    assert_eq!(
        serialize_via_registry("CharacterController", CharacterController::default()),
        EXPECT_CHARCTRL_DEFAULT
    );
    assert_round_trips("CharacterController", EXPECT_CHARCTRL_DEFAULT);
}

#[test]
fn script_overrides_pass_through_verbatim() {
    let mut overrides = Map::new();
    overrides.insert("health".to_string(), Value::from(100));
    overrides.insert("name".to_string(), Value::from("hero"));
    let s = Script {
        scripts: vec![ScriptSlot {
            script_path: "player.lua".to_string(),
            overrides: Value::Object(overrides),
        }],
    };
    assert_eq!(serialize_via_registry("Script", s), EXPECT_SCRIPT);
    assert_round_trips("Script", EXPECT_SCRIPT);
}

#[test]
fn environment_matches_cpp() {
    use saffron_scene::SceneEnvironment;
    let env = SceneEnvironment::default();
    assert_eq!(
        saffron_json::dump_json_sorted(&environment_to_json(&env), -1),
        EXPECT_ENV_DEFAULT
    );
    // Round-trip the read path.
    let parsed: Value = serde_json::from_str(EXPECT_ENV_DEFAULT).unwrap();
    let back = environment_from_json(&parsed);
    assert_eq!(
        saffron_json::dump_json_sorted(&environment_to_json(&back), -1),
        EXPECT_ENV_DEFAULT
    );
}

#[test]
fn unknown_enum_strings_default_to_cpp_value() {
    let reg = ComponentRegistry::default_builtins();
    let mut scene = Scene::new();
    let e = scene.create_entity("e");

    // SkyMode unknown → Procedural (and a warn, exercised by environment_from_json).
    let env = environment_from_json(&serde_json::json!({ "skyMode": "nonsense" }));
    assert_eq!(env.sky_mode, saffron_scene::SkyMode::Procedural);

    // Wrap unknown → Loop, Transition unknown → Inertialize.
    let row = reg.find_by_name("AnimationPlayer").unwrap();
    (row.deserialize)(
        &mut scene,
        e,
        &serde_json::json!({ "wrap": "??", "transitionMode": "??" }),
    )
    .unwrap();
    let a = scene.component::<AnimationPlayer>(e).unwrap();
    assert_eq!(a.wrap, Wrap::Loop);
    assert_eq!(a.transition_mode, Transition::Inertialize);

    // Motion unknown → Dynamic.
    let row = reg.find_by_name("Rigidbody").unwrap();
    (row.deserialize)(&mut scene, e, &serde_json::json!({ "motion": "??" })).unwrap();
    assert_eq!(
        scene.component::<Rigidbody>(e).unwrap().motion,
        Motion::Dynamic
    );

    // Shape unknown → Box.
    let row = reg.find_by_name("Collider").unwrap();
    (row.deserialize)(&mut scene, e, &serde_json::json!({ "shape": "??" })).unwrap();
    assert_eq!(scene.component::<Collider>(e).unwrap().shape, Shape::Box);

    // Joint unknown → SwingTwist (default per-bone).
    let row = reg.find_by_name("BonePhysics").unwrap();
    (row.deserialize)(
        &mut scene,
        e,
        &serde_json::json!({ "bones": [{ "joint": "??" }] }),
    )
    .unwrap();
    let bp = scene
        .with_component::<BonePhysicsComponent, _>(e, |b| b.bones[0].joint)
        .unwrap();
    assert_eq!(bp, Joint::SwingTwist);
}

/// The frozen-wire contract: no Uuid field ever emits a JSON number.
#[test]
fn no_uuid_field_emits_a_json_number() {
    // Serialize every component that carries a Uuid with a large, past-2^53 value and
    // assert the JSON carries the id as a decimal *string*, never a number.
    let big = Uuid(18_446_744_073_709_551_615);

    let mesh = serialize_via_registry("Mesh", Mesh { mesh: big });
    assert!(mesh.contains(r#""mesh":"18446744073709551615""#), "{mesh}");

    let rel = serialize_via_registry(
        "Relationship",
        Relationship {
            parent: big,
            ..Relationship::default()
        },
    );
    assert!(rel.contains(r#""parent":"18446744073709551615""#), "{rel}");

    let asset = serialize_via_registry("MaterialAsset", MaterialAsset { material: big });
    assert!(
        asset.contains(r#""material":"18446744073709551615""#),
        "{asset}"
    );

    let model = serialize_via_registry("ModelInstance", ModelInstance { model_id: big });
    assert!(
        model.contains(r#""modelId":"18446744073709551615""#),
        "{model}"
    );

    let col = serialize_via_registry(
        "Collider",
        Collider {
            source_mesh: big,
            ..Collider::default()
        },
    );
    assert!(
        col.contains(r#""sourceMesh":"18446744073709551615""#),
        "{col}"
    );

    // None of these payloads contains the bare numeric form of the id.
    for payload in [&mesh, &rel, &asset, &model, &col] {
        assert!(
            !payload.contains(":18446744073709551615"),
            "a Uuid leaked as a bare JSON number: {payload}"
        );
    }
}
