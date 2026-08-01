//! The per-component [`SceneSerialize`] bodies.

use glam::{BVec3, Mat4, Vec3};
use serde_json::{Map, Value};

use saffron_core::Uuid;
use saffron_json::{json_bool_or, json_f32_or, json_string_or, json_u64_or, uuid_to_json};

use super::{f32_value, field, json_i32_or, object, vec3_from_json, vec3_to_json};
use crate::component::{
    AnimationPlayer, AtmosphereRole, Bone, BonePhysics, BonePhysicsComponent, Camera,
    CharacterController, Collider, DirectionalLight, FogShape, FogVolume, FootChain, FootIk, Joint,
    KinematicBones, MaterialSet, MaterialSlot, Mesh, ModelInstance, MorphComponent, Motion, Name,
    PhysicsMaterial, PointLight, ReflectionProbe, Relationship, Rigidbody, Script, ScriptSlot,
    Shape, SkinnedMesh, SpotLight, Transform, Transition, VegetationField, WindSource, Wrap,
};
use crate::error::Result;
use crate::registry::SceneSerialize;

/// A named-object `bvec3` → `{"x","y","z"}` booleans.
fn bvec3_to_json(v: BVec3) -> Value {
    Value::Object(Map::from_iter([
        ("x".to_string(), Value::Bool(v.x)),
        ("y".to_string(), Value::Bool(v.y)),
        ("z".to_string(), Value::Bool(v.z)),
    ]))
}

/// Reads a `bvec3` from a named object, each component defaulting to `false`.
fn bvec3_from_json(j: &Value) -> BVec3 {
    BVec3::new(
        json_bool_or(j, "x", false),
        json_bool_or(j, "y", false),
        json_bool_or(j, "z", false),
    )
}

/// A nested object field for a vector read, or an empty object so the per-field defaults
/// apply.
fn object_field(j: &Value, key: &str) -> Value {
    field(j, key)
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()))
}

impl SceneSerialize for Name {
    fn to_json(&self) -> Value {
        object([("name", Value::String(self.name.clone()))])
    }

    fn load_json(&mut self, value: &Value) -> Result<()> {
        self.name = json_string_or(value, "name", String::new());
        Ok(())
    }
}

impl SceneSerialize for Transform {
    fn to_json(&self) -> Value {
        object([
            ("translation", vec3_to_json(self.translation)),
            ("scale", vec3_to_json(self.scale)),
            ("rotation", vec3_to_json(self.rotation)),
        ])
    }

    fn load_json(&mut self, value: &Value) -> Result<()> {
        self.translation = vec3_from_json(&object_field(value, "translation"));
        self.scale = vec3_from_json(&object_field(value, "scale"));
        self.rotation = vec3_from_json(&object_field(value, "rotation"));
        Ok(())
    }
}

impl SceneSerialize for Mesh {
    fn to_json(&self) -> Value {
        object([("mesh", uuid_to_json(self.mesh.value()))])
    }

    fn load_json(&mut self, value: &Value) -> Result<()> {
        self.mesh = Uuid(json_u64_or(value, "mesh", 0));
        Ok(())
    }
}

impl SceneSerialize for VegetationField {
    fn to_json(&self) -> Value {
        object([
            ("map", uuid_to_json(self.map.value())),
            ("enabled", Value::Bool(self.enabled)),
        ])
    }

    fn load_json(&mut self, value: &Value) -> Result<()> {
        self.map = Uuid(json_u64_or(value, "map", 0));
        self.enabled = json_bool_or(value, "enabled", true);
        Ok(())
    }
}

impl SceneSerialize for Camera {
    fn to_json(&self) -> Value {
        object([
            ("fov", f32_value(self.fov)),
            ("near", f32_value(self.near_plane)),
            ("far", f32_value(self.far_plane)),
            ("primary", Value::Bool(self.primary)),
            ("showModel", Value::Bool(self.show_model)),
            ("showFrustum", Value::Bool(self.show_frustum)),
            ("frustumMaxDistance", f32_value(self.frustum_max_distance)),
        ])
    }

    fn load_json(&mut self, value: &Value) -> Result<()> {
        self.fov = json_f32_or(value, "fov", 45.0);
        self.near_plane = json_f32_or(value, "near", 0.1);
        self.far_plane = json_f32_or(value, "far", 100.0);
        self.primary = json_bool_or(value, "primary", true);
        self.show_model = json_bool_or(value, "showModel", true);
        self.show_frustum = json_bool_or(value, "showFrustum", true);
        self.frustum_max_distance = json_f32_or(value, "frustumMaxDistance", 10.0);
        Ok(())
    }
}

/// Emits the shared material field set as a JSON object, reused by `Material` and each
/// `MaterialSlot` — identical field sets, so one serializer over `MaterialSlot` covers
/// both. `uv_tiling` / `uv_offset` are intentionally absent — they must not appear on the
/// wire.
/// Serializes one [`MaterialSlot`] — a `.smat` reference id plus its sparse override map.
fn material_slot_to_json(s: &MaterialSlot) -> Value {
    object([
        ("material", uuid_to_json(s.material.value())),
        ("overrides", s.overrides.clone()),
    ])
}

/// Reads a [`MaterialSlot`] from one entry of the `slots` array. `material` accepts a
/// decimal string or an unsigned number; a non-object `overrides` defaults to `{}`.
fn material_slot_from_json(sj: &Value) -> MaterialSlot {
    MaterialSlot {
        material: Uuid(json_u64_or(sj, "material", 0)),
        overrides: field(sj, "overrides")
            .filter(|v| v.is_object())
            .cloned()
            .unwrap_or_else(|| Value::Object(serde_json::Map::new())),
    }
}

impl SceneSerialize for MaterialSet {
    fn to_json(&self) -> Value {
        let slots: Vec<Value> = self.slots.iter().map(material_slot_to_json).collect();
        object([("slots", Value::Array(slots))])
    }

    fn load_json(&mut self, value: &Value) -> Result<()> {
        self.slots.clear();
        if let Some(Value::Array(slots)) = field(value, "slots") {
            for sj in slots {
                self.slots.push(material_slot_from_json(sj));
            }
        }
        Ok(())
    }
}

impl SceneSerialize for ModelInstance {
    fn to_json(&self) -> Value {
        object([("modelId", Value::String(self.model_id.value().to_string()))])
    }

    fn load_json(&mut self, value: &Value) -> Result<()> {
        self.model_id = Uuid(json_u64_or(value, "modelId", self.model_id.value()));
        Ok(())
    }
}

impl SceneSerialize for Script {
    fn to_json(&self) -> Value {
        let scripts: Vec<Value> = self
            .scripts
            .iter()
            .map(|s| {
                object([
                    ("scriptPath", Value::String(s.script_path.clone())),
                    ("overrides", s.overrides.clone()),
                ])
            })
            .collect();
        object([("scripts", Value::Array(scripts))])
    }

    fn load_json(&mut self, value: &Value) -> Result<()> {
        self.scripts.clear();
        if let Some(Value::Array(scripts)) = field(value, "scripts") {
            for sj in scripts {
                let overrides = field(sj, "overrides").cloned().unwrap_or(Value::Null);
                let overrides = if overrides.is_object() {
                    overrides
                } else {
                    Value::Object(Map::new())
                };
                self.scripts.push(ScriptSlot {
                    script_path: json_string_or(sj, "scriptPath", String::new()),
                    overrides,
                });
            }
        }
        Ok(())
    }
}

/// The lowercase wire name for a [`Wrap`].
fn wrap_name(wrap: Wrap) -> &'static str {
    match wrap {
        Wrap::Once => "once",
        Wrap::Loop => "loop",
        Wrap::PingPong => "pingpong",
    }
}

/// The lowercase wire name for a [`Transition`].
fn transition_name(transition: Transition) -> &'static str {
    match transition {
        Transition::Inertialize => "inertialize",
        Transition::CrossFade => "crossfade",
    }
}

impl SceneSerialize for AnimationPlayer {
    fn to_json(&self) -> Value {
        // `time` / `playing` are runtime-only (the editor Timeline preview drives them); only
        // the authored `autoplay` intent persists. Entering Play resets time/playing.
        object([
            ("clip", uuid_to_json(self.clip.value())),
            ("autoplay", Value::Bool(self.autoplay)),
            ("speed", f32_value(self.speed)),
            ("wrap", Value::String(wrap_name(self.wrap).to_string())),
            (
                "transitionMode",
                Value::String(transition_name(self.transition_mode).to_string()),
            ),
            ("loopBlend", f32_value(self.loop_blend)),
        ])
    }

    fn load_json(&mut self, value: &Value) -> Result<()> {
        self.clip = Uuid(json_u64_or(value, "clip", 0));
        self.autoplay = json_bool_or(value, "autoplay", false);
        self.speed = json_f32_or(value, "speed", 1.0);
        self.wrap = match json_string_or(value, "wrap", "loop".to_string()).as_str() {
            "once" => Wrap::Once,
            "pingpong" => Wrap::PingPong,
            _ => Wrap::Loop,
        };
        self.transition_mode =
            match json_string_or(value, "transitionMode", "inertialize".to_string()).as_str() {
                "crossfade" => Transition::CrossFade,
                _ => Transition::Inertialize,
            };
        self.loop_blend = json_f32_or(value, "loopBlend", 0.0);
        Ok(())
    }
}

impl SceneSerialize for DirectionalLight {
    fn to_json(&self) -> Value {
        object([
            (
                "atmosphereRole",
                Value::String(
                    match self.atmosphere_role {
                        AtmosphereRole::Sun => "sun",
                        AtmosphereRole::Moon => "moon",
                    }
                    .to_string(),
                ),
            ),
            ("direction", vec3_to_json(self.direction)),
            ("color", vec3_to_json(self.color)),
            ("intensity", f32_value(self.intensity)),
            ("ambient", f32_value(self.ambient)),
            (
                "volumetricScattering",
                f32_value(self.volumetric_scattering),
            ),
            (
                "castVolumetricShadow",
                Value::Bool(self.cast_volumetric_shadow),
            ),
        ])
    }

    fn load_json(&mut self, value: &Value) -> Result<()> {
        self.atmosphere_role =
            match json_string_or(value, "atmosphereRole", "sun".to_string()).as_str() {
                "moon" => AtmosphereRole::Moon,
                _ => AtmosphereRole::Sun,
            };
        self.direction = vec3_from_json(&object_field(value, "direction"));
        self.color = vec3_from_json(&object_field(value, "color"));
        self.intensity = json_f32_or(value, "intensity", 1.0);
        self.ambient = json_f32_or(value, "ambient", 0.15);
        self.volumetric_scattering = json_f32_or(value, "volumetricScattering", 1.0);
        self.cast_volumetric_shadow = json_bool_or(value, "castVolumetricShadow", true);
        Ok(())
    }
}

impl SceneSerialize for PointLight {
    fn to_json(&self) -> Value {
        object([
            ("color", vec3_to_json(self.color)),
            ("intensity", f32_value(self.intensity)),
            ("range", f32_value(self.range)),
            (
                "volumetricScattering",
                f32_value(self.volumetric_scattering),
            ),
            (
                "castVolumetricShadow",
                Value::Bool(self.cast_volumetric_shadow),
            ),
        ])
    }

    fn load_json(&mut self, value: &Value) -> Result<()> {
        self.color = vec3_from_json(&object_field(value, "color"));
        self.intensity = json_f32_or(value, "intensity", 5.0);
        self.range = json_f32_or(value, "range", 10.0);
        self.volumetric_scattering = json_f32_or(value, "volumetricScattering", 1.0);
        self.cast_volumetric_shadow = json_bool_or(value, "castVolumetricShadow", true);
        Ok(())
    }
}

impl SceneSerialize for SpotLight {
    fn to_json(&self) -> Value {
        object([
            ("direction", vec3_to_json(self.direction)),
            ("color", vec3_to_json(self.color)),
            ("intensity", f32_value(self.intensity)),
            ("range", f32_value(self.range)),
            ("innerAngle", f32_value(self.inner_angle)),
            ("outerAngle", f32_value(self.outer_angle)),
            (
                "volumetricScattering",
                f32_value(self.volumetric_scattering),
            ),
            (
                "castVolumetricShadow",
                Value::Bool(self.cast_volumetric_shadow),
            ),
        ])
    }

    fn load_json(&mut self, value: &Value) -> Result<()> {
        self.direction = vec3_from_json(&object_field(value, "direction"));
        self.color = vec3_from_json(&object_field(value, "color"));
        self.intensity = json_f32_or(value, "intensity", 5.0);
        self.range = json_f32_or(value, "range", 10.0);
        self.inner_angle = json_f32_or(value, "innerAngle", 20.0);
        self.outer_angle = json_f32_or(value, "outerAngle", 30.0);
        self.volumetric_scattering = json_f32_or(value, "volumetricScattering", 1.0);
        self.cast_volumetric_shadow = json_bool_or(value, "castVolumetricShadow", true);
        Ok(())
    }
}

impl SceneSerialize for ReflectionProbe {
    fn to_json(&self) -> Value {
        object([
            ("influenceRadius", f32_value(self.influence_radius)),
            ("intensity", f32_value(self.intensity)),
            ("boxProjection", Value::Bool(self.box_projection)),
            ("boxExtent", vec3_to_json(self.box_extent)),
        ])
    }

    fn load_json(&mut self, value: &Value) -> Result<()> {
        self.influence_radius = json_f32_or(value, "influenceRadius", 10.0);
        self.intensity = json_f32_or(value, "intensity", 1.0);
        self.box_projection = json_bool_or(value, "boxProjection", false);
        self.box_extent = vec3_from_json(&object_field(value, "boxExtent"));
        // Capture pending on every read.
        self.dirty = true;
        Ok(())
    }
}

/// The lowercase wire name for a [`FogShape`].
fn fog_shape_name(shape: FogShape) -> &'static str {
    match shape {
        FogShape::Box => "box",
        FogShape::Sphere => "sphere",
    }
}

impl SceneSerialize for FogVolume {
    fn to_json(&self) -> Value {
        object([
            (
                "shape",
                Value::String(fog_shape_name(self.shape).to_string()),
            ),
            ("extents", vec3_to_json(self.extents)),
            ("radius", f32_value(self.radius)),
            ("edgeFalloff", f32_value(self.edge_falloff)),
            ("density", f32_value(self.density)),
            ("albedo", vec3_to_json(self.albedo)),
            ("emissive", vec3_to_json(self.emissive)),
            ("phaseG", f32_value(self.phase_g)),
            ("heightFalloff", f32_value(self.height_falloff)),
            ("noiseScale", f32_value(self.noise_scale)),
            ("noiseIntensity", f32_value(self.noise_intensity)),
            ("noiseDetail", f32_value(self.noise_detail)),
            ("wind", vec3_to_json(self.wind)),
            ("speed", f32_value(self.speed)),
        ])
    }

    fn load_json(&mut self, value: &Value) -> Result<()> {
        self.shape = match json_string_or(value, "shape", "box".to_string()).as_str() {
            "sphere" => FogShape::Sphere,
            _ => FogShape::Box,
        };
        self.extents = vec3_from_json(&object_field(value, "extents"));
        self.radius = json_f32_or(value, "radius", 5.0);
        self.edge_falloff = json_f32_or(value, "edgeFalloff", 1.0);
        self.density = json_f32_or(value, "density", 0.5);
        self.albedo = vec3_from_json(&object_field(value, "albedo"));
        self.emissive = vec3_from_json(&object_field(value, "emissive"));
        self.phase_g = json_f32_or(value, "phaseG", 0.0);
        self.height_falloff = json_f32_or(value, "heightFalloff", 0.0);
        self.noise_scale = json_f32_or(value, "noiseScale", 0.2);
        self.noise_intensity = json_f32_or(value, "noiseIntensity", 0.0);
        self.noise_detail = json_f32_or(value, "noiseDetail", 0.5);
        self.wind = vec3_from_json(&object_field(value, "wind"));
        self.speed = json_f32_or(value, "speed", 0.1);
        Ok(())
    }
}

impl SceneSerialize for WindSource {
    fn to_json(&self) -> Value {
        object([
            ("kind", Value::String(self.kind.name().to_string())),
            ("strength", f32_value(self.strength)),
            ("radius", f32_value(self.radius)),
            ("falloff", f32_value(self.falloff)),
            ("enabled", Value::Bool(self.enabled)),
        ])
    }

    fn load_json(&mut self, value: &Value) -> Result<()> {
        self.kind = saffron_wind::WindSourceKind::from_name(&json_string_or(
            value,
            "kind",
            "directional".to_string(),
        ));
        self.strength = json_f32_or(value, "strength", 5.0);
        self.radius = json_f32_or(value, "radius", 20.0);
        self.falloff = json_f32_or(value, "falloff", 0.5);
        self.enabled = json_bool_or(value, "enabled", true);
        Ok(())
    }
}

impl SceneSerialize for Relationship {
    fn to_json(&self) -> Value {
        object([("parent", uuid_to_json(self.parent.value()))])
    }

    fn load_json(&mut self, value: &Value) -> Result<()> {
        self.parent = Uuid(json_u64_or(value, "parent", 0));
        Ok(())
    }
}

impl SceneSerialize for Bone {
    fn to_json(&self) -> Value {
        // A bone serializes as an empty object.
        Value::Object(Map::new())
    }

    fn load_json(&mut self, _value: &Value) -> Result<()> {
        Ok(())
    }
}

impl SceneSerialize for SkinnedMesh {
    fn to_json(&self) -> Value {
        let bones: Vec<Value> = self.bones.iter().map(|b| uuid_to_json(b.value())).collect();
        let inverse_bind: Vec<Value> = self
            .inverse_bind
            .iter()
            .map(|m| {
                // Column-major flat 16. Each element promotes f32 → f64 for byte-equality.
                Value::Array(m.to_cols_array().iter().map(|&f| f32_value(f)).collect())
            })
            .collect();
        object([
            ("mesh", uuid_to_json(self.mesh.value())),
            ("rootBone", uuid_to_json(self.root_bone.value())),
            ("bones", Value::Array(bones)),
            ("inverseBind", Value::Array(inverse_bind)),
        ])
    }

    fn load_json(&mut self, value: &Value) -> Result<()> {
        self.mesh = Uuid(json_u64_or(value, "mesh", 0));
        self.root_bone = Uuid(json_u64_or(value, "rootBone", 0));
        self.bones.clear();
        if let Some(Value::Array(bones)) = field(value, "bones") {
            for b in bones {
                self.bones.push(Uuid(wire_u64(b)));
            }
        }
        self.inverse_bind.clear();
        if let Some(Value::Array(mats)) = field(value, "inverseBind") {
            for mat in mats {
                let mut cols = [0.0f32; 16];
                cols.copy_from_slice(&Mat4::IDENTITY.to_cols_array());
                if let Value::Array(elems) = mat
                    && elems.len() == 16
                {
                    for (i, e) in elems.iter().enumerate() {
                        if let Some(f) = e.as_f64() {
                            cols[i] = f as f32;
                        }
                    }
                }
                self.inverse_bind.push(Mat4::from_cols_array(&cols));
            }
        }
        // `bone_handles` is a resolved cache — the relink rebuilds it.
        self.bone_handles.clear();
        Ok(())
    }
}

impl SceneSerialize for MorphComponent {
    fn to_json(&self) -> Value {
        let weights: Vec<Value> = self.weights.iter().map(|&w| f32_value(w)).collect();
        let names: Vec<Value> = self
            .names
            .iter()
            .map(|n| Value::String(n.clone()))
            .collect();
        object([
            ("weights", Value::Array(weights)),
            ("names", Value::Array(names)),
        ])
    }

    fn load_json(&mut self, value: &Value) -> Result<()> {
        self.weights.clear();
        if let Some(Value::Array(ws)) = field(value, "weights") {
            for w in ws {
                self.weights.push(w.as_f64().unwrap_or(0.0) as f32);
            }
        }
        self.names.clear();
        if let Some(Value::Array(ns)) = field(value, "names") {
            for n in ns {
                self.names.push(n.as_str().unwrap_or_default().to_owned());
            }
        }
        Ok(())
    }
}

/// A bare JSON value read as a `u64` with the lenient wire union: unsigned number, or a
/// decimal string parsed in full. Anything else is `0`.
fn wire_u64(value: &Value) -> u64 {
    match value {
        Value::Number(n) => n.as_u64().unwrap_or(0),
        Value::String(s) => s.parse::<u64>().unwrap_or(0),
        _ => 0,
    }
}

impl SceneSerialize for FootIk {
    fn to_json(&self) -> Value {
        let chains: Vec<Value> = self
            .chains
            .iter()
            .map(|c| {
                object([
                    ("upper", Value::from(c.upper)),
                    ("mid", Value::from(c.mid)),
                    ("end", Value::from(c.end)),
                    ("poleVector", vec3_to_json(c.pole_vector)),
                ])
            })
            .collect();
        object([
            ("enabled", Value::Bool(self.enabled)),
            ("groundHeight", f32_value(self.ground_height)),
            ("chains", Value::Array(chains)),
        ])
    }

    fn load_json(&mut self, value: &Value) -> Result<()> {
        self.enabled = json_bool_or(value, "enabled", false);
        self.ground_height = json_f32_or(value, "groundHeight", 0.0);
        self.chains.clear();
        if let Some(Value::Array(chains)) = field(value, "chains") {
            for entry in chains {
                self.chains.push(FootChain {
                    upper: json_i32_or(entry, "upper", -1),
                    mid: json_i32_or(entry, "mid", -1),
                    end: json_i32_or(entry, "end", -1),
                    pole_vector: vec3_from_json(&object_field(entry, "poleVector")),
                });
            }
        }
        Ok(())
    }
}

/// The lowercase wire name for a ragdoll [`Joint`].
fn joint_name(joint: Joint) -> &'static str {
    match joint {
        Joint::Fixed => "fixed",
        Joint::Hinge => "hinge",
        Joint::SwingTwist => "swingtwist",
        Joint::Free => "free",
    }
}

impl SceneSerialize for BonePhysicsComponent {
    fn to_json(&self) -> Value {
        let bones: Vec<Value> = self
            .bones
            .iter()
            .map(|b| {
                object([
                    ("shapeHalfExtents", vec3_to_json(b.shape_half_extents)),
                    ("mass", f32_value(b.mass)),
                    ("joint", Value::String(joint_name(b.joint).to_string())),
                    ("swingTwistLimits", vec3_to_json(b.swing_twist_limits)),
                    ("driveStiffness", f32_value(b.drive_stiffness)),
                    ("driveDamping", f32_value(b.drive_damping)),
                    ("driveMaxForce", f32_value(b.drive_max_force)),
                ])
            })
            .collect();
        object([("bones", Value::Array(bones))])
    }

    fn load_json(&mut self, value: &Value) -> Result<()> {
        self.bones.clear();
        if let Some(Value::Array(bones)) = field(value, "bones") {
            for entry in bones {
                let joint = match json_string_or(entry, "joint", "swingtwist".to_string()).as_str()
                {
                    "fixed" => Joint::Fixed,
                    "hinge" => Joint::Hinge,
                    "free" => Joint::Free,
                    _ => Joint::SwingTwist,
                };
                self.bones.push(BonePhysics {
                    shape_half_extents: vec3_from_json(&object_field(entry, "shapeHalfExtents")),
                    mass: json_f32_or(entry, "mass", 1.0),
                    joint,
                    swing_twist_limits: vec3_from_json(&object_field(entry, "swingTwistLimits")),
                    drive_stiffness: json_f32_or(entry, "driveStiffness", 0.0),
                    drive_damping: json_f32_or(entry, "driveDamping", 0.0),
                    drive_max_force: json_f32_or(entry, "driveMaxForce", 0.0),
                });
            }
        }
        Ok(())
    }
}

/// The lowercase wire name for a [`Motion`] type.
fn motion_name(motion: Motion) -> &'static str {
    match motion {
        Motion::Static => "static",
        Motion::Kinematic => "kinematic",
        Motion::Dynamic => "dynamic",
    }
}

impl SceneSerialize for Rigidbody {
    fn to_json(&self) -> Value {
        object([
            (
                "motion",
                Value::String(motion_name(self.motion).to_string()),
            ),
            ("mass", f32_value(self.mass)),
            ("linearDamping", f32_value(self.linear_damping)),
            ("angularDamping", f32_value(self.angular_damping)),
            ("gravityFactor", f32_value(self.gravity_factor)),
            ("windFactor", f32_value(self.wind_factor)),
            ("lockPosition", bvec3_to_json(self.lock_position)),
            ("lockRotation", bvec3_to_json(self.lock_rotation)),
            ("collisionLayer", Value::from(self.collision_layer)),
        ])
    }

    fn load_json(&mut self, value: &Value) -> Result<()> {
        self.motion = match json_string_or(value, "motion", "dynamic".to_string()).as_str() {
            "static" => Motion::Static,
            "kinematic" => Motion::Kinematic,
            _ => Motion::Dynamic,
        };
        self.mass = json_f32_or(value, "mass", 1.0);
        self.linear_damping = json_f32_or(value, "linearDamping", 0.05);
        self.angular_damping = json_f32_or(value, "angularDamping", 0.05);
        self.gravity_factor = json_f32_or(value, "gravityFactor", 1.0);
        self.wind_factor = json_f32_or(value, "windFactor", 0.0);
        self.lock_position = bvec3_from_json(&object_field(value, "lockPosition"));
        self.lock_rotation = bvec3_from_json(&object_field(value, "lockRotation"));
        self.collision_layer = json_i32_or(value, "collisionLayer", 0);
        Ok(())
    }
}

/// The lowercase wire name for a collider [`Shape`].
fn shape_name(shape: Shape) -> &'static str {
    match shape {
        Shape::Box => "box",
        Shape::Sphere => "sphere",
        Shape::Capsule => "capsule",
        Shape::ConvexHull => "convexhull",
        Shape::Mesh => "mesh",
    }
}

impl SceneSerialize for Collider {
    fn to_json(&self) -> Value {
        object([
            ("shape", Value::String(shape_name(self.shape).to_string())),
            ("halfExtents", vec3_to_json(self.half_extents)),
            (
                "sourceMesh",
                Value::String(self.source_mesh.value().to_string()),
            ),
            ("offset", vec3_to_json(self.offset)),
            (
                "material",
                object([
                    ("friction", f32_value(self.material.friction)),
                    ("restitution", f32_value(self.material.restitution)),
                ]),
            ),
            ("isSensor", Value::Bool(self.is_sensor)),
        ])
    }

    fn load_json(&mut self, value: &Value) -> Result<()> {
        self.shape = match json_string_or(value, "shape", "box".to_string()).as_str() {
            "sphere" => Shape::Sphere,
            "capsule" => Shape::Capsule,
            "convexhull" => Shape::ConvexHull,
            "mesh" => Shape::Mesh,
            _ => Shape::Box,
        };
        self.half_extents = vec3_from_json(&object_field(value, "halfExtents"));
        // `sourceMesh` is a bare value read through `wire_u64` (string or unsigned number;
        // anything else → 0).
        self.source_mesh = Uuid(field(value, "sourceMesh").map_or(0, wire_u64));
        self.offset = vec3_from_json(&object_field(value, "offset"));
        let material = object_field(value, "material");
        self.material = PhysicsMaterial {
            friction: json_f32_or(&material, "friction", 0.5),
            restitution: json_f32_or(&material, "restitution", 0.0),
        };
        self.is_sensor = json_bool_or(value, "isSensor", false);
        Ok(())
    }
}

impl SceneSerialize for KinematicBones {
    fn to_json(&self) -> Value {
        let driven: Vec<Value> = self.driven.iter().map(|&i| Value::from(i)).collect();
        object([
            ("enabled", Value::Bool(self.enabled)),
            ("driven", Value::Array(driven)),
        ])
    }

    fn load_json(&mut self, value: &Value) -> Result<()> {
        self.enabled = json_bool_or(value, "enabled", true);
        self.driven.clear();
        if let Some(Value::Array(driven)) = field(value, "driven") {
            for entry in driven {
                if let Some(i) = entry.as_i64() {
                    self.driven.push(i as i32);
                }
            }
        }
        Ok(())
    }
}

impl SceneSerialize for CharacterController {
    fn to_json(&self) -> Value {
        // Only the authored movement params round-trip; the runtime velocity/ground state
        // serialize as their defaults (move-character writes them at play time).
        object([
            ("maxSpeed", f32_value(self.max_speed)),
            ("maxSlopeAngle", f32_value(self.max_slope_angle)),
            ("maxStepHeight", f32_value(self.max_step_height)),
            ("gravityFactor", f32_value(self.gravity_factor)),
        ])
    }

    fn load_json(&mut self, value: &Value) -> Result<()> {
        self.max_speed = json_f32_or(value, "maxSpeed", 4.0);
        // The literal `0.785398`, not `FRAC_PI_4`, so a loaded default reads byte-identically.
        #[allow(clippy::approx_constant)]
        let slope_default = 0.785_398_f32;
        self.max_slope_angle = json_f32_or(value, "maxSlopeAngle", slope_default);
        self.max_step_height = json_f32_or(value, "maxStepHeight", 0.3);
        self.gravity_factor = json_f32_or(value, "gravityFactor", 1.0);
        // Runtime state resets on read.
        self.desired_velocity = Vec3::ZERO;
        self.vertical_velocity = 0.0;
        self.on_ground = false;
        Ok(())
    }
}
