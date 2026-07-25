//! The byte-compatible component JSON serde.
//!
//! Every body here pins the frozen wire bytes: the `project.json` scene block and the
//! control-plane scene payloads must stay byte-identical so the editor and project files
//! keep working. Each key spelling, each per-field default, the decimal-string uuid
//! encoding, the lowercase enum-name spellings, the named-vector shape, and the flat
//! column-major matrix layout are load-bearing — a single drift fails silently as
//! corrupted data.
//!
//! The bodies are imperative, built on the `saffron-json` lenient readers (`json_f32_or`,
//! `json_u64_or`, …) and `uuid_to_json`, plumbed into the registry's [`SceneSerialize`]
//! trait. Imperative bodies over serde derives is deliberate: the contract is key-order-
//! and default-sensitive, and the readers' "absent/mistyped → fallback" semantics are
//! exactly what a `#[derive(Deserialize)]` would not reproduce.
//!
//! ## Float byte-equality
//!
//! Component scalars are stored as `float` (f32) but the wire format promotes each to
//! `double` (f64), then dumps the shortest round-trippable decimal of that f64. serde_json's
//! Ryū does the same for an f64, so every f32 scalar is inserted here as `f64::from(value)`
//! ([`f32_value`]) — formatting the f64-promotion of the f32.
//!
//! ## Key ordering
//!
//! The scene document (like the asset `.smat`/`.smodel` formats) is byte-frozen with
//! **sorted** object keys. `serde_json` is built workspace-wide with `preserve_order` (the
//! control wire needs insertion order = field order), so the save path sorts keys explicitly
//! via `saffron_json::dump_json_sorted`. The emit order of the `insert` calls below is
//! therefore incidental to the output bytes.

use glam::{BVec3, Mat4, Vec3};
use serde_json::{Map, Value};

use saffron_core::Uuid;
use saffron_json::{json_bool_or, json_f32_or, json_string_or, json_u64_or, uuid_to_json};

use crate::component::{
    AnimationPlayer, AtmosphereRole, Bone, BonePhysics, BonePhysicsComponent, Camera,
    CharacterController, Collider, DirectionalLight, FogShape, FogVolume, FootChain, FootIk, Joint,
    KinematicBones, MaterialSet, MaterialSlot, Mesh, ModelInstance, MorphComponent, Motion, Name,
    PhysicsMaterial, PointLight, ReflectionProbe, Relationship, Rigidbody, Script, ScriptSlot,
    Shape, SkinnedMesh, SpotLight, Transform, Transition, VegetationField, WindSource, Wrap,
};
use crate::environment::{
    AtmosphereSettings, CloudSettings, FogMode, FogQuality, FogSettings, SceneEnvironment, SkyMode,
    TimeOfDaySettings, TodCurve, TodTintCurve, WindSettings,
};
use crate::error::Result;
use crate::registry::SceneSerialize;

/// Wraps an `f32` as a JSON number formatted from its f64 promotion. This is the
/// byte-equality seam — every component scalar is inserted through it.
fn f32_value(value: f32) -> Value {
    Value::from(f64::from(value))
}

fn json_i32_or(value: &Value, key: &str, default: i32) -> i32 {
    field(value, key)
        .and_then(Value::as_i64)
        .and_then(|value| i32::try_from(value).ok())
        .unwrap_or(default)
}

/// A named-object `vec3` → `{"x","y","z"}`. Never positional — quat/vec storage order is
/// config-dependent.
fn vec3_to_json(v: Vec3) -> Value {
    Value::Object(Map::from_iter([
        ("x".to_string(), f32_value(v.x)),
        ("y".to_string(), f32_value(v.y)),
        ("z".to_string(), f32_value(v.z)),
    ]))
}

/// Reads a `vec3` from a named object, each component defaulting to `0`.
fn vec3_from_json(j: &Value) -> Vec3 {
    Vec3::new(
        json_f32_or(j, "x", 0.0),
        json_f32_or(j, "y", 0.0),
        json_f32_or(j, "z", 0.0),
    )
}

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

/// Locates an object field as a borrowed value, the input to a nested-object read.
fn field<'a>(j: &'a Value, key: &str) -> Option<&'a Value> {
    j.as_object().and_then(|m| m.get(key))
}

/// A nested object field for a vector read, or an empty object so the per-field defaults
/// apply.
fn object_field(j: &Value, key: &str) -> Value {
    field(j, key)
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()))
}

/// Builds an object from an ordered list of entries. Key order is incidental (the output
/// is alphabetically sorted by `serde_json`), so this is purely a terse constructor.
fn object<const N: usize>(entries: [(&str, Value); N]) -> Value {
    Value::Object(Map::from_iter(
        entries.into_iter().map(|(k, v)| (k.to_string(), v)),
    ))
}

/// The lowercase wire name for a [`SkyMode`].
fn sky_mode_name(mode: SkyMode) -> &'static str {
    match mode {
        SkyMode::Color => "color",
        SkyMode::Texture => "texture",
        SkyMode::Procedural => "procedural",
    }
}

/// The wire name for a [`FogMode`].
fn fog_mode_name(mode: FogMode) -> &'static str {
    match mode {
        FogMode::Analytic => "analytic",
        FogMode::Volumetric => "volumetric",
    }
}

/// The wire name for a [`FogQuality`].
fn fog_quality_name(quality: FogQuality) -> &'static str {
    match quality {
        FogQuality::Low => "low",
        FogQuality::Medium => "medium",
        FogQuality::High => "high",
    }
}

/// Reads a [`FogQuality`] from its wire name, defaulting to `Medium` on an unknown spelling.
fn fog_quality_from_name(name: &str) -> FogQuality {
    match name {
        "low" => FogQuality::Low,
        "high" => FogQuality::High,
        _ => FogQuality::Medium,
    }
}

/// Reads a [`FogMode`] from its wire name, warning and defaulting to `Analytic` on an unknown spelling.
fn fog_mode_from_name(name: &str) -> FogMode {
    match name {
        "analytic" => FogMode::Analytic,
        "volumetric" => FogMode::Volumetric,
        other => {
            tracing::warn!("unknown fog mode '{other}', defaulting to analytic");
            FogMode::Analytic
        }
    }
}

/// Reads a [`SkyMode`] from its wire name, warning and defaulting to `Procedural` on an
/// unknown spelling.
fn sky_mode_from_name(name: &str) -> SkyMode {
    match name {
        "color" => SkyMode::Color,
        "texture" => SkyMode::Texture,
        "procedural" => SkyMode::Procedural,
        other => {
            tracing::warn!("unknown sky mode '{other}', defaulting to procedural");
            SkyMode::Procedural
        }
    }
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
        let kind = match self.kind {
            saffron_wind::WindSourceKind::Directional => "directional",
            saffron_wind::WindSourceKind::Point => "point",
            saffron_wind::WindSourceKind::Vortex => "vortex",
            saffron_wind::WindSourceKind::Wake => "wake",
            saffron_wind::WindSourceKind::Volume => "volume",
        };
        object([
            ("kind", Value::String(kind.to_string())),
            ("strength", f32_value(self.strength)),
            ("radius", f32_value(self.radius)),
            ("falloff", f32_value(self.falloff)),
            ("enabled", Value::Bool(self.enabled)),
        ])
    }

    fn load_json(&mut self, value: &Value) -> Result<()> {
        self.kind = match json_string_or(value, "kind", "directional".to_string()).as_str() {
            "point" => saffron_wind::WindSourceKind::Point,
            "vortex" => saffron_wind::WindSourceKind::Vortex,
            "wake" => saffron_wind::WindSourceKind::Wake,
            "volume" => saffron_wind::WindSourceKind::Volume,
            _ => saffron_wind::WindSourceKind::Directional,
        };
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

/// The `AtmosphereSettings` block, nested inside the
/// environment.
fn atmosphere_to_json(a: &AtmosphereSettings) -> Value {
    object([
        ("enabled", Value::Bool(a.enabled)),
        ("planetRadius", f32_value(a.planet_radius)),
        ("atmosphereHeight", f32_value(a.atmosphere_height)),
        ("rayleighScattering", vec3_to_json(a.rayleigh_scattering)),
        ("rayleighScaleHeight", f32_value(a.rayleigh_scale_height)),
        ("mieScattering", f32_value(a.mie_scattering)),
        ("mieScaleHeight", f32_value(a.mie_scale_height)),
        ("mieAnisotropy", f32_value(a.mie_anisotropy)),
        ("ozoneAbsorption", vec3_to_json(a.ozone_absorption)),
        ("sunDiskAngularRadius", f32_value(a.sun_disk_angular_radius)),
        ("sunDiskIntensity", f32_value(a.sun_disk_intensity)),
        (
            "moonDiskAngularRadius",
            f32_value(a.moon_disk_angular_radius),
        ),
        ("moonDiskIntensity", f32_value(a.moon_disk_intensity)),
        ("moonEarthshine", f32_value(a.moon_earthshine)),
        (
            "perPixelTransmittance",
            Value::Bool(a.per_pixel_transmittance),
        ),
        ("skyCaptureCadence", f32_value(a.sky_capture_cadence)),
    ])
}

/// Reads an [`AtmosphereSettings`] block, defaulting per field and leaving the vector
/// fields at their struct defaults when absent.
fn atmosphere_from_json(j: &Value) -> AtmosphereSettings {
    let mut a = AtmosphereSettings::default();
    if !j.is_object() {
        return a;
    }
    a.enabled = json_bool_or(j, "enabled", false);
    a.planet_radius = json_f32_or(j, "planetRadius", 6360.0);
    a.atmosphere_height = json_f32_or(j, "atmosphereHeight", 100.0);
    if let Some(v) = field(j, "rayleighScattering") {
        a.rayleigh_scattering = vec3_from_json(v);
    }
    a.rayleigh_scale_height = json_f32_or(j, "rayleighScaleHeight", 8.0);
    a.mie_scattering = json_f32_or(j, "mieScattering", 3.996);
    a.mie_scale_height = json_f32_or(j, "mieScaleHeight", 1.2);
    a.mie_anisotropy = json_f32_or(j, "mieAnisotropy", 0.8);
    if let Some(v) = field(j, "ozoneAbsorption") {
        a.ozone_absorption = vec3_from_json(v);
    }
    a.sun_disk_angular_radius = json_f32_or(j, "sunDiskAngularRadius", 0.00465);
    a.sun_disk_intensity = json_f32_or(j, "sunDiskIntensity", 1.0);
    a.moon_disk_angular_radius = json_f32_or(j, "moonDiskAngularRadius", 0.00496);
    a.moon_disk_intensity = json_f32_or(j, "moonDiskIntensity", 1.0);
    a.moon_earthshine = json_f32_or(j, "moonEarthshine", 0.02);
    a.per_pixel_transmittance = json_bool_or(j, "perPixelTransmittance", false);
    a.sky_capture_cadence = json_f32_or(j, "skyCaptureCadence", 9.0).clamp(1.0, 60.0);
    a
}

/// The `FogSettings` block, nested inside the environment.
fn fog_to_json(f: &FogSettings) -> Value {
    object([
        ("enabled", Value::Bool(f.enabled)),
        ("mode", Value::String(fog_mode_name(f.mode).to_string())),
        (
            "quality",
            Value::String(fog_quality_name(f.quality).to_string()),
        ),
        ("historyBlend", f32_value(f.history_blend)),
        ("neighborhoodClamp", Value::Bool(f.neighborhood_clamp)),
        ("lightClamp", f32_value(f.light_clamp)),
        ("baseDensity", f32_value(f.base_density)),
        ("scatterAlbedo", f32_value(f.scatter_albedo)),
        ("phaseG", f32_value(f.phase_g)),
        ("density", f32_value(f.density)),
        ("albedo", vec3_to_json(f.albedo)),
        ("height", f32_value(f.height)),
        ("heightFalloff", f32_value(f.height_falloff)),
        ("startDistance", f32_value(f.start_distance)),
        ("maxOpacity", f32_value(f.max_opacity)),
        ("emissive", vec3_to_json(f.emissive)),
        ("directionalColor", vec3_to_json(f.directional_color)),
        ("directionalExponent", f32_value(f.directional_exponent)),
        ("layer2Density", f32_value(f.layer2_density)),
        ("layer2Falloff", f32_value(f.layer2_falloff)),
        ("layer2Height", f32_value(f.layer2_height)),
        ("aerialPerspective", Value::Bool(f.aerial_perspective)),
        ("aerialIntensity", f32_value(f.aerial_intensity)),
    ])
}

/// Reads a [`FogSettings`] block, defaulting per field and leaving the vector fields at
/// their struct defaults when absent.
fn fog_from_json(j: &Value) -> FogSettings {
    let mut f = FogSettings::default();
    if !j.is_object() {
        return f;
    }
    f.enabled = json_bool_or(j, "enabled", false);
    if let Some(Value::String(s)) = field(j, "mode") {
        f.mode = fog_mode_from_name(s);
    }
    if let Some(Value::String(s)) = field(j, "quality") {
        f.quality = fog_quality_from_name(s);
    }
    f.history_blend = json_f32_or(j, "historyBlend", 0.05);
    f.neighborhood_clamp = json_bool_or(j, "neighborhoodClamp", false);
    f.light_clamp = json_f32_or(j, "lightClamp", 0.0);
    f.base_density = json_f32_or(j, "baseDensity", 0.02);
    f.scatter_albedo = json_f32_or(j, "scatterAlbedo", 0.9);
    f.phase_g = json_f32_or(j, "phaseG", 0.6);
    f.density = json_f32_or(j, "density", 0.02);
    if let Some(v) = field(j, "albedo") {
        f.albedo = vec3_from_json(v);
    }
    f.height = json_f32_or(j, "height", 0.0);
    f.height_falloff = json_f32_or(j, "heightFalloff", 0.2);
    f.start_distance = json_f32_or(j, "startDistance", 0.0);
    f.max_opacity = json_f32_or(j, "maxOpacity", 1.0);
    if let Some(v) = field(j, "emissive") {
        f.emissive = vec3_from_json(v);
    }
    if let Some(v) = field(j, "directionalColor") {
        f.directional_color = vec3_from_json(v);
    }
    f.directional_exponent = json_f32_or(j, "directionalExponent", 8.0);
    f.layer2_density = json_f32_or(j, "layer2Density", 0.0);
    f.layer2_falloff = json_f32_or(j, "layer2Falloff", 0.5);
    f.layer2_height = json_f32_or(j, "layer2Height", 0.0);
    f.aerial_perspective = json_bool_or(j, "aerialPerspective", false);
    f.aerial_intensity = json_f32_or(j, "aerialIntensity", 1.0);
    f
}

fn cloud_to_json(cloud: &CloudSettings) -> Value {
    object([
        ("enabled", Value::Bool(cloud.enabled)),
        ("coverage", f32_value(cloud.coverage)),
        ("cloudType", f32_value(cloud.cloud_type)),
        ("precipitation", f32_value(cloud.precipitation)),
        ("anvilBias", f32_value(cloud.anvil_bias)),
        ("layerAltitude", f32_value(cloud.layer_altitude)),
        ("layerHeight", f32_value(cloud.layer_height)),
        ("baseScale", f32_value(cloud.base_scale)),
        ("detailScale", f32_value(cloud.detail_scale)),
        ("detailStrength", f32_value(cloud.detail_strength)),
        ("curlStrength", f32_value(cloud.curl_strength)),
        ("weatherScale", f32_value(cloud.weather_scale)),
        ("weatherOffset", vec3_to_json(cloud.weather_offset)),
        (
            "weatherTexture",
            uuid_to_json(cloud.weather_texture.value()),
        ),
        ("primarySteps", Value::from(cloud.primary_steps)),
        ("lightSteps", Value::from(cloud.light_steps)),
        ("dropletDiameter", f32_value(cloud.droplet_diameter)),
        ("temporalFactor", f32_value(cloud.temporal_factor)),
        ("castCloudShadows", Value::Bool(cloud.cast_cloud_shadows)),
        (
            "cloudShadowStrength",
            f32_value(cloud.cloud_shadow_strength),
        ),
        (
            "cloudShadowOnSurfaceStrength",
            f32_value(cloud.cloud_shadow_on_surface_strength),
        ),
    ])
}

fn cloud_from_json(value: &Value) -> CloudSettings {
    let mut cloud = CloudSettings::default();
    if !value.is_object() {
        return cloud;
    }
    cloud.enabled = json_bool_or(value, "enabled", cloud.enabled);
    cloud.coverage = json_f32_or(value, "coverage", cloud.coverage);
    cloud.cloud_type = json_f32_or(value, "cloudType", cloud.cloud_type);
    cloud.precipitation = json_f32_or(value, "precipitation", cloud.precipitation);
    cloud.anvil_bias = json_f32_or(value, "anvilBias", cloud.anvil_bias);
    cloud.layer_altitude = json_f32_or(value, "layerAltitude", cloud.layer_altitude);
    cloud.layer_height = json_f32_or(value, "layerHeight", cloud.layer_height);
    cloud.base_scale = json_f32_or(value, "baseScale", cloud.base_scale);
    cloud.detail_scale = json_f32_or(value, "detailScale", cloud.detail_scale);
    cloud.detail_strength = json_f32_or(value, "detailStrength", cloud.detail_strength);
    cloud.curl_strength = json_f32_or(value, "curlStrength", cloud.curl_strength);
    cloud.weather_scale = json_f32_or(value, "weatherScale", cloud.weather_scale);
    if let Some(offset) = field(value, "weatherOffset") {
        cloud.weather_offset = vec3_from_json(offset);
    }
    cloud.weather_texture = Uuid(json_u64_or(
        value,
        "weatherTexture",
        cloud.weather_texture.value(),
    ));
    cloud.primary_steps = json_u64_or(value, "primarySteps", u64::from(cloud.primary_steps)) as u32;
    cloud.light_steps = json_u64_or(value, "lightSteps", u64::from(cloud.light_steps)) as u32;
    cloud.droplet_diameter = json_f32_or(value, "dropletDiameter", cloud.droplet_diameter);
    cloud.temporal_factor = json_f32_or(value, "temporalFactor", cloud.temporal_factor);
    cloud.cast_cloud_shadows = json_bool_or(value, "castCloudShadows", cloud.cast_cloud_shadows);
    cloud.cloud_shadow_strength =
        json_f32_or(value, "cloudShadowStrength", cloud.cloud_shadow_strength);
    cloud.cloud_shadow_on_surface_strength = json_f32_or(
        value,
        "cloudShadowOnSurfaceStrength",
        cloud.cloud_shadow_on_surface_strength,
    );
    cloud
}

fn wind_to_json(wind: &WindSettings) -> Value {
    object([
        ("orientation", f32_value(wind.orientation)),
        ("speed", f32_value(wind.speed)),
        ("gust", f32_value(wind.gust)),
        ("turbulenceOctaves", Value::from(wind.turbulence_octaves)),
        ("turbulenceRoughness", f32_value(wind.turbulence_roughness)),
        ("gustFrequency", f32_value(wind.gust_frequency)),
        ("referenceHeight", f32_value(wind.reference_height)),
        ("heightExponent", f32_value(wind.height_exponent)),
        ("seed", Value::from(wind.seed)),
    ])
}

fn wind_from_json(value: &Value) -> WindSettings {
    let mut wind = WindSettings::default();
    if !value.is_object() {
        return wind;
    }
    wind.orientation = json_f32_or(value, "orientation", wind.orientation);
    wind.speed = json_f32_or(value, "speed", wind.speed);
    wind.gust = json_f32_or(value, "gust", wind.gust);
    wind.turbulence_octaves = u32::try_from(json_u64_or(
        value,
        "turbulenceOctaves",
        wind.turbulence_octaves.into(),
    ))
    .unwrap_or(wind.turbulence_octaves);
    wind.turbulence_roughness =
        json_f32_or(value, "turbulenceRoughness", wind.turbulence_roughness);
    wind.gust_frequency = json_f32_or(value, "gustFrequency", wind.gust_frequency);
    wind.reference_height = json_f32_or(value, "referenceHeight", wind.reference_height);
    wind.height_exponent = json_f32_or(value, "heightExponent", wind.height_exponent);
    wind.seed = u32::try_from(json_u64_or(value, "seed", wind.seed.into())).unwrap_or(wind.seed);
    wind
}

fn tod_curve_to_json(curve: &TodCurve) -> Value {
    Value::Array(
        curve
            .0
            .iter()
            .map(|&(x, y)| object([("x", f32_value(x)), ("y", f32_value(y))]))
            .collect(),
    )
}

fn tod_curve_from_json(value: &Value) -> TodCurve {
    let Some(points) = value.as_array() else {
        return TodCurve::default();
    };
    TodCurve(
        points
            .iter()
            .filter(|point| point.is_object())
            .map(|point| (json_f32_or(point, "x", 0.0), json_f32_or(point, "y", 0.0)))
            .collect(),
    )
}

fn tint_curve_to_json(curve: &TodTintCurve) -> Value {
    object([
        ("master", tod_curve_to_json(&curve.master)),
        ("red", tod_curve_to_json(&curve.red)),
        ("green", tod_curve_to_json(&curve.green)),
        ("blue", tod_curve_to_json(&curve.blue)),
    ])
}

fn tint_curve_from_json(value: &Value) -> TodTintCurve {
    let mut curve = TodTintCurve::default();
    if let Some(value) = field(value, "master") {
        curve.master = tod_curve_from_json(value);
    }
    if let Some(value) = field(value, "red") {
        curve.red = tod_curve_from_json(value);
    }
    if let Some(value) = field(value, "green") {
        curve.green = tod_curve_from_json(value);
    }
    if let Some(value) = field(value, "blue") {
        curve.blue = tod_curve_from_json(value);
    }
    curve
}

fn time_of_day_to_json(settings: &TimeOfDaySettings) -> Value {
    object([
        ("enabled", Value::Bool(settings.enabled)),
        ("manualOverride", Value::Bool(settings.manual_override)),
        ("timeOfDay", f32_value(settings.time_of_day)),
        ("year", Value::from(settings.year)),
        ("month", Value::from(settings.month)),
        ("day", Value::from(settings.day)),
        ("latitude", f32_value(settings.latitude)),
        ("longitude", f32_value(settings.longitude)),
        ("dayLengthSeconds", f32_value(settings.day_length_seconds)),
        ("exposureCurve", tod_curve_to_json(&settings.exposure_curve)),
        ("tintCurve", tint_curve_to_json(&settings.tint_curve)),
        ("coverageCurve", tod_curve_to_json(&settings.coverage_curve)),
        (
            "cloudTypeCurve",
            tod_curve_to_json(&settings.cloud_type_curve),
        ),
    ])
}

fn time_of_day_from_json(value: &Value) -> TimeOfDaySettings {
    let mut settings = TimeOfDaySettings::default();
    if !value.is_object() {
        return settings;
    }
    settings.enabled = json_bool_or(value, "enabled", settings.enabled);
    settings.manual_override = json_bool_or(value, "manualOverride", settings.manual_override);
    settings.time_of_day = json_f32_or(value, "timeOfDay", settings.time_of_day);
    settings.year = json_i32_or(value, "year", settings.year);
    settings.month = json_i32_or(value, "month", settings.month);
    settings.day = json_i32_or(value, "day", settings.day);
    settings.latitude = json_f32_or(value, "latitude", settings.latitude);
    settings.longitude = json_f32_or(value, "longitude", settings.longitude);
    settings.day_length_seconds =
        json_f32_or(value, "dayLengthSeconds", settings.day_length_seconds);
    if let Some(curve) = field(value, "exposureCurve") {
        settings.exposure_curve = tod_curve_from_json(curve);
    }
    if let Some(curve) = field(value, "tintCurve") {
        settings.tint_curve = tint_curve_from_json(curve);
    }
    if let Some(curve) = field(value, "coverageCurve") {
        settings.coverage_curve = tod_curve_from_json(curve);
    }
    if let Some(curve) = field(value, "cloudTypeCurve") {
        settings.cloud_type_curve = tod_curve_from_json(curve);
    }
    settings
}

/// Serializes the [`SceneEnvironment`] block.
///
/// The scene-document phase writes this under the document's `environment` key; it is a
/// free function (not a [`SceneSerialize`] impl) because the environment lives on the
/// [`Scene`], not as an entity component.
#[must_use]
pub fn environment_to_json(env: &SceneEnvironment) -> Value {
    object([
        (
            "skyMode",
            Value::String(sky_mode_name(env.sky_mode).to_string()),
        ),
        ("clearColor", vec3_to_json(env.clear_color)),
        ("skyTexture", uuid_to_json(env.sky_texture.value())),
        ("skyIntensity", f32_value(env.sky_intensity)),
        ("skyRotation", f32_value(env.sky_rotation)),
        ("exposure", f32_value(env.exposure)),
        ("visible", Value::Bool(env.visible)),
        ("useSkyForAmbient", Value::Bool(env.use_sky_for_ambient)),
        ("ambientColor", vec3_to_json(env.ambient_color)),
        ("ambientIntensity", f32_value(env.ambient_intensity)),
        ("atmosphere", atmosphere_to_json(&env.atmosphere)),
        ("fog", fog_to_json(&env.fog)),
        ("cloud", cloud_to_json(&env.cloud)),
        ("wind", wind_to_json(&env.wind)),
        ("timeOfDay", time_of_day_to_json(&env.time_of_day)),
    ])
}

/// Reads a [`SceneEnvironment`] block, defaulting per field and leaving the vector fields
/// at their struct defaults when absent.
#[must_use]
pub fn environment_from_json(j: &Value) -> SceneEnvironment {
    let mut env = SceneEnvironment::default();
    if !j.is_object() {
        return env;
    }
    env.sky_mode = sky_mode_from_name(&json_string_or(j, "skyMode", "procedural".to_string()));
    if let Some(v) = field(j, "clearColor") {
        env.clear_color = vec3_from_json(v);
    }
    env.sky_texture = Uuid(json_u64_or(j, "skyTexture", 0));
    env.sky_intensity = json_f32_or(j, "skyIntensity", 1.0);
    env.sky_rotation = json_f32_or(j, "skyRotation", 0.0);
    env.exposure = json_f32_or(j, "exposure", 1.0);
    env.visible = json_bool_or(j, "visible", true);
    env.use_sky_for_ambient = json_bool_or(j, "useSkyForAmbient", true);
    if let Some(v) = field(j, "ambientColor") {
        env.ambient_color = vec3_from_json(v);
    }
    env.ambient_intensity = json_f32_or(j, "ambientIntensity", 0.15);
    if let Some(v) = field(j, "atmosphere") {
        env.atmosphere = atmosphere_from_json(v);
    }
    if let Some(v) = field(j, "fog") {
        env.fog = fog_from_json(v);
    }
    if let Some(v) = field(j, "cloud") {
        env.cloud = cloud_from_json(v);
    }
    if let Some(v) = field(j, "wind") {
        env.wind = wind_from_json(v);
    }
    if let Some(v) = field(j, "timeOfDay") {
        env.time_of_day = time_of_day_from_json(v);
    }
    env
}
