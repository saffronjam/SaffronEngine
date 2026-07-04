//! JSON Schema (draft 2020-12) fragments for the OpenRPC document.
//!
//! Each DTO emits a fragment shaped as
//! `{ type: "object", additionalProperties: false, properties, required }` with `required`
//! listing the non-`Option` fields in declaration order, so the contract-test schema oracle
//! (`tools/check-control-schema/check.ts`) validates the host's results. The `properties` map
//! carries `schemars`'s alphabetical key order (object-key order is validation-irrelevant); the
//! assembled document is later sorted into the byte-frozen order.
//!
//! `schemars` is the per-DTO fragment source: [`fragment_for`] reads `schema_for!(T)` and
//! normalizes it into the wire shape (drop the `$schema`/`title`/`format` noise, unwrap the
//! `Option` nullable unions, inline the enum `$defs`, and re-point struct `$ref`s at
//! `#/components/schemas/`). Two kinds of fact `schemars` cannot see cross DTO boundaries and
//! are applied as a small override table keyed by `(struct, field)`:
//!
//! - **Selectors.** `EntitySelector`/`AssetSelector` are `serde_json::Value` (opaque, owned by
//!   the runtime), so `schemars` reports `true` (any). The wire shape is
//!   `oneOf:[{type:string},{type:integer}]` (a uuid string or a raw id), so each selector field
//!   is listed in [`SELECTOR_FIELDS`]. A `Value` field with no entry is a `Json` blob and emits
//!   `{}` (any).
//! - **The four cross-boundary special-cases**: `EnvironmentDto` is the bare `$ref Environment`;
//!   `SelectionResult.entity` is `oneOf:[$ref EntityRef, null]`; `InspectResult.components` is
//!   `$ref Components`; `SetComponentParams.json` is `$ref ComponentBody`. These reference shapes
//!   the [`component_schemas`] block defines, and they hold opaque/cross-boundary shapes a derive
//!   cannot read.
//!
//! The hand-authored [`component_schemas`] block (the 21 component shapes + `Vec3`/`Vec4`/`BVec3`
//! via inline objects + `Components`/`ComponentBody`/`Environment`/`AtmosphereSettingsDto`)
//! describes the *opaque component blobs* the contract test validates — they are not protocol
//! DTOs with a derive to read, so they stay hand-authored.

use schemars::JsonSchema;
use serde_json::{Map, Value, json};

/// The wire shape of an `EntitySelector`/`AssetSelector`: a uuid decimal string or a raw id.
fn selector_schema() -> Value {
    json!({ "oneOf": [{ "type": "string" }, { "type": "integer" }] })
}

/// The `(struct, field)` pairs whose `serde_json::Value` field is a selector, not a `Json`
/// blob. Every other `Value` field emits `{}` (the `Json` case).
pub const SELECTOR_FIELDS: &[(&str, &str)] = &[
    ("FitColliderParams", "entity"),
    ("ApplyImpulseParams", "entity"),
    ("SetKinematicBonesParams", "entity"),
    ("MoveCharacterParams", "entity"),
    ("EnableRagdollParams", "entity"),
    ("SetRagdollParams", "entity"),
    ("GetRagdollParams", "entity"),
    ("SetScriptOverrideParams", "entity"),
    ("InstantiateModelParams", "asset"),
    ("ExtractSubAssetParams", "asset"),
    ("ClearExtractionParams", "asset"),
    ("ReimportModelParams", "asset"),
    ("ModelInfoParams", "asset"),
    ("AssetReferencesParams", "asset"),
    ("RenameAssetParams", "asset"),
    ("MoveAssetParams", "asset"),
    ("AssetUsagesParams", "asset"),
    ("AssetMetadataParams", "asset"),
    ("DeleteAssetParams", "asset"),
    ("AssignAssetParams", "entity"),
    ("AssignAssetParams", "asset"),
    ("MaterialAssignParams", "entity"),
    ("MaterialAssignParams", "material"),
    ("MaterialGetParams", "material"),
    ("MaterialUpdateParams", "material"),
    ("PreviewRenderParams", "material"),
    ("MaterialSetGraphParams", "material"),
    ("MaterialCreateInstanceParams", "parent"),
    ("MaterialSetOverrideParams", "material"),
    ("MaterialCompileParams", "material"),
    ("ThumbnailParams", "asset"),
    ("EntityParams", "entity"),
    ("SetParentParams", "entity"),
    ("SetParentParams", "parent"),
    ("ComponentParams", "entity"),
    ("SetComponentParams", "entity"),
    ("SetComponentOrderParams", "entity"),
    ("SetTransformParams", "entity"),
    ("SetLightParams", "entity"),
    ("GetAssetModelParams", "asset"),
    ("EnterAssetPreviewParams", "asset"),
    ("ListClipsParams", "asset"),
    ("PlayAnimationParams", "entity"),
    ("PlayAnimationParams", "clip"),
    ("SeekAnimationParams", "entity"),
    ("SetAnimationLoopParams", "entity"),
    ("SetAnimationPlayingParams", "entity"),
    ("AnimationStateParams", "entity"),
    ("SetFootIkParams", "entity"),
    ("GetFootIkParams", "entity"),
    ("RenameEntityParams", "entity"),
    ("SetComponentFieldParams", "entity"),
];

/// The four cross-boundary special-cases. `EnvironmentDto`
/// replaces its whole fragment; the others override one field's schema.
fn special_field(struct_name: &str, field: &str) -> Option<Value> {
    match (struct_name, field) {
        ("SelectionResult", "entity") => Some(json!({
            "oneOf": [{ "$ref": "#/components/schemas/EntityRef" }, { "type": "null" }]
        })),
        ("InspectResult", "components") => {
            Some(json!({ "$ref": "#/components/schemas/Components" }))
        }
        ("SetComponentParams", "json") => {
            Some(json!({ "$ref": "#/components/schemas/ComponentBody" }))
        }
        _ => None,
    }
}

/// The wire field names of DTO `T` in declaration order — the positional-argument order.
///
/// This is the per-field positional index made explicit: `args[i]` on the wire fills the
/// `i`-th declared field, when the named key is absent, uniformly across every field. The runtime
/// reads this order from `schemars` (`required` is declaration-ordered, and `properties`
/// preserves declaration order under `serde_json`'s `preserve_order`), so a typed command can
/// fold positional args onto named keys before deserializing — see `saffron_control`'s
/// `register`.
#[must_use]
pub fn positional_field_order<T: JsonSchema>() -> Vec<String> {
    let raw = serde_json::to_value(schemars::schema_for!(T))
        .expect("schemars schema serializes to a JSON value");
    raw.get("properties")
        .and_then(Value::as_object)
        .map(|props| props.keys().cloned().collect())
        .unwrap_or_default()
}

/// The frozen-wire JSON Schema fragment for DTO `T`, named `struct_name` (its OpenRPC schema
/// key). `EnvironmentDto` is the one whole-struct special-case (`$ref Environment`).
#[must_use]
pub fn fragment_for<T: JsonSchema>(struct_name: &str) -> Value {
    if struct_name == "EnvironmentDto" {
        return json!({ "$ref": "#/components/schemas/Environment" });
    }

    let raw = serde_json::to_value(schemars::schema_for!(T))
        .expect("schemars schema serializes to a JSON value");
    let defs = raw.get("$defs").and_then(Value::as_object).cloned();
    let props = raw
        .get("properties")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();

    let mut out_props = Map::new();
    for (field, schema) in &props {
        let resolved = if let Some(special) = special_field(struct_name, field) {
            special
        } else if is_any(schema) {
            // A `serde_json::Value` field: a selector or an opaque `Json` blob.
            if SELECTOR_FIELDS.contains(&(struct_name, field.as_str())) {
                selector_schema()
            } else {
                json!({})
            }
        } else {
            normalize(schema, defs.as_ref())
        };
        out_props.insert(field.clone(), resolved);
    }

    let required = raw.get("required").cloned().unwrap_or_else(|| json!([]));
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": Value::Object(out_props),
        "required": required,
    })
}

/// `true` or `{}` — the schemars rendering of an "accept anything" schema (a `Value` field).
fn is_any(schema: &Value) -> bool {
    match schema {
        Value::Bool(true) => true,
        Value::Object(map) => map.is_empty(),
        _ => false,
    }
}

/// Rewrite one schemars property schema into the frozen-wire shape: unwrap the
/// `Option` nullable, strip `format`, inline enum `$defs`, re-point struct `$ref`s, and recurse
/// into array `items`.
fn normalize(schema: &Value, defs: Option<&Map<String, Value>>) -> Value {
    let Value::Object(map) = schema else {
        return schema.clone();
    };

    // `Option<Enum>`/`Option<$ref>` arrives as `anyOf:[<inner>, {type:null}]`; unwrap the inner.
    if let Some(any_of) = map.get("anyOf").and_then(Value::as_array) {
        if let Some(inner) = any_of.iter().find(|entry| !is_null_type(entry)) {
            return normalize(inner, defs);
        }
    }

    // A `$ref` is either an inlined enum (`{type:string, enum:[...]}`) or a component ref.
    if let Some(reference) = map.get("$ref").and_then(Value::as_str) {
        return resolve_ref(reference, defs);
    }

    let mut out = Map::new();
    for (key, value) in map {
        match key.as_str() {
            // The wire shape is a bare `{type:integer}`/`{type:number}` for every numeric
            // field: the `format` tag (`float`/`int32`/`int64`) and the unsigned `minimum`/
            // `maximum` range bounds `schemars` derives have no place there.
            "format" | "minimum" | "maximum" | "description" | "title" | "$schema" => {}
            // `type:[X,"null"]` is an `Option<X>`; keep just `X`.
            "type" => {
                out.insert(key.clone(), strip_null(value));
            }
            "items" => {
                out.insert(key.clone(), normalize(value, defs));
            }
            _ => {
                out.insert(key.clone(), value.clone());
            }
        }
    }
    Value::Object(out)
}

/// Resolve a `#/$defs/X` reference: an enum inlines to `{type:string, enum:[...]}` (the wire
/// enum shape), a struct re-points to `#/components/schemas/X`.
fn resolve_ref(reference: &str, defs: Option<&Map<String, Value>>) -> Value {
    let name = reference.rsplit('/').next().unwrap_or(reference);
    if let Some(def) = defs.and_then(|d| d.get(name)) {
        if let Some(variants) = def.get("enum") {
            return json!({ "type": "string", "enum": variants.clone() });
        }
    }
    json!({ "$ref": format!("#/components/schemas/{name}") })
}

/// `{type:null}` or `{type:["...","null"]}`-shaped sentinel for the null arm of an `Option`.
fn is_null_type(schema: &Value) -> bool {
    schema
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|t| t == "null")
}

/// `["X","null"]` → `"X"`; a bare `"X"` passes through.
fn strip_null(type_value: &Value) -> Value {
    if let Value::Array(items) = type_value {
        if let Some(non_null) = items.iter().find(|item| item.as_str() != Some("null")) {
            return non_null.clone();
        }
    }
    type_value.clone()
}

/// The hand-authored component-schema block: the 21 scene-component shapes + `Vec3`/`Vec4` +
/// the `Components` aggregate, the `ComponentBody` union, `AtmosphereSettingsDto`, and the
/// `Environment` shape. These describe the opaque
/// component blobs the contract test validates, not protocol DTOs.
#[must_use]
pub fn component_schemas() -> Map<String, Value> {
    let vec3 = json!({ "$ref": "#/components/schemas/Vec3" });
    let uuid = json!({ "type": "string" });
    let bvec3 = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": { "x": { "type": "boolean" }, "y": { "type": "boolean" }, "z": { "type": "boolean" } },
        "required": ["x", "y", "z"],
    });

    let component_names = COMPONENT_NAMES;

    let mut schemas = Map::new();
    schemas.insert(
        "Name".into(),
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": { "name": { "type": "string" } },
            "required": ["name"],
        }),
    );
    schemas.insert(
        "Transform".into(),
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": { "translation": vec3, "scale": vec3, "rotation": vec3 },
            "required": ["translation", "scale", "rotation"],
        }),
    );
    schemas.insert(
        "Mesh".into(),
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": { "mesh": uuid },
            "required": ["mesh"],
        }),
    );
    schemas.insert(
        "Camera".into(),
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "fov": { "type": "number" },
                "near": { "type": "number" },
                "far": { "type": "number" },
                "primary": { "type": "boolean" },
                "showModel": { "type": "boolean" },
                "showFrustum": { "type": "boolean" },
                "frustumMaxDistance": { "type": "number" },
            },
            "required": ["fov", "near", "far", "primary", "showModel", "showFrustum", "frustumMaxDistance"],
        }),
    );
    schemas.insert(
        "MaterialSet".into(),
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "slots": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "additionalProperties": false,
                        "properties": {
                            "material": { "type": "string" },
                            "overrides": { "type": "object" },
                        },
                        "required": ["material", "overrides"],
                    },
                },
            },
            "required": ["slots"],
        }),
    );
    schemas.insert(
        "Script".into(),
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "scripts": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "additionalProperties": false,
                        "properties": { "scriptPath": { "type": "string" }, "overrides": { "type": "object" } },
                        "required": ["scriptPath", "overrides"],
                    },
                },
            },
            "required": ["scripts"],
        }),
    );
    schemas.insert(
        "DirectionalLight".into(),
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "direction": vec3,
                "color": vec3,
                "intensity": { "type": "number" },
                "ambient": { "type": "number" },
            },
            "required": ["direction", "color", "intensity", "ambient"],
        }),
    );
    schemas.insert(
        "PointLight".into(),
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": { "color": vec3, "intensity": { "type": "number" }, "range": { "type": "number" } },
            "required": ["color", "intensity", "range"],
        }),
    );
    schemas.insert(
        "SpotLight".into(),
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "direction": vec3,
                "color": vec3,
                "intensity": { "type": "number" },
                "range": { "type": "number" },
                "innerAngle": { "type": "number" },
                "outerAngle": { "type": "number" },
            },
            "required": ["direction", "color", "intensity", "range", "innerAngle", "outerAngle"],
        }),
    );
    schemas.insert(
        "ReflectionProbe".into(),
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "influenceRadius": { "type": "number" },
                "intensity": { "type": "number" },
                "boxProjection": { "type": "boolean" },
                "boxExtent": vec3,
            },
            "required": ["influenceRadius", "intensity", "boxProjection", "boxExtent"],
        }),
    );
    schemas.insert(
        "Relationship".into(),
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": { "parent": uuid },
            "required": ["parent"],
        }),
    );
    schemas.insert(
        "SkinnedMesh".into(),
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "mesh": uuid,
                "rootBone": uuid,
                "bones": { "type": "array", "items": uuid },
                "inverseBind": {
                    "type": "array",
                    "items": { "type": "array", "items": { "type": "number" }, "minItems": 16, "maxItems": 16 },
                },
            },
            "required": ["mesh", "rootBone", "bones", "inverseBind"],
        }),
    );
    schemas.insert(
        "Bone".into(),
        json!({ "type": "object", "additionalProperties": false, "properties": {} }),
    );
    schemas.insert(
        "ModelInstance".into(),
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": { "modelId": uuid },
            "required": ["modelId"],
        }),
    );
    schemas.insert(
        "FootIk".into(),
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "enabled": { "type": "boolean" },
                "groundHeight": { "type": "number" },
                "chains": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "additionalProperties": false,
                        "properties": {
                            "upper": { "type": "number" },
                            "mid": { "type": "number" },
                            "end": { "type": "number" },
                            "poleVector": vec3,
                        },
                        "required": ["upper", "mid", "end", "poleVector"],
                    },
                },
            },
            "required": ["enabled", "groundHeight", "chains"],
        }),
    );
    schemas.insert(
        "BonePhysics".into(),
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "bones": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "additionalProperties": false,
                        "properties": {
                            "shapeHalfExtents": vec3,
                            "mass": { "type": "number" },
                            "joint": { "type": "string", "enum": ["fixed", "hinge", "swingtwist", "free"] },
                            "swingTwistLimits": vec3,
                            "driveStiffness": { "type": "number" },
                            "driveDamping": { "type": "number" },
                            "driveMaxForce": { "type": "number" },
                        },
                        "required": [
                            "shapeHalfExtents", "mass", "joint", "swingTwistLimits",
                            "driveStiffness", "driveDamping", "driveMaxForce",
                        ],
                    },
                },
            },
            "required": ["bones"],
        }),
    );
    schemas.insert(
        "Rigidbody".into(),
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "motion": { "type": "string", "enum": ["static", "kinematic", "dynamic"] },
                "mass": { "type": "number" },
                "linearDamping": { "type": "number" },
                "angularDamping": { "type": "number" },
                "gravityFactor": { "type": "number" },
                "lockPosition": bvec3,
                "lockRotation": bvec3,
                "collisionLayer": { "type": "integer" },
            },
            "required": [
                "motion", "mass", "linearDamping", "angularDamping", "gravityFactor",
                "lockPosition", "lockRotation", "collisionLayer",
            ],
        }),
    );
    schemas.insert(
        "Collider".into(),
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "shape": { "type": "string", "enum": ["box", "sphere", "capsule", "convexhull", "mesh"] },
                "halfExtents": vec3,
                "sourceMesh": uuid,
                "offset": vec3,
                "material": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": { "friction": { "type": "number" }, "restitution": { "type": "number" } },
                    "required": ["friction", "restitution"],
                },
                "isSensor": { "type": "boolean" },
            },
            "required": ["shape", "halfExtents", "sourceMesh", "offset", "material", "isSensor"],
        }),
    );
    schemas.insert(
        "KinematicBones".into(),
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "enabled": { "type": "boolean" },
                "driven": { "type": "array", "items": { "type": "integer" } },
            },
            "required": ["enabled", "driven"],
        }),
    );
    schemas.insert(
        "CharacterController".into(),
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "maxSpeed": { "type": "number" },
                "maxSlopeAngle": { "type": "number" },
                "maxStepHeight": { "type": "number" },
                "gravityFactor": { "type": "number" },
            },
            "required": ["maxSpeed", "maxSlopeAngle", "maxStepHeight", "gravityFactor"],
        }),
    );
    schemas.insert(
        "AtmosphereSettingsDto".into(),
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "enabled": { "type": "boolean" },
                "planetRadius": { "type": "number" },
                "atmosphereHeight": { "type": "number" },
                "rayleighScattering": vec3,
                "rayleighScaleHeight": { "type": "number" },
                "mieScattering": { "type": "number" },
                "mieScaleHeight": { "type": "number" },
                "mieAnisotropy": { "type": "number" },
                "ozoneAbsorption": vec3,
                "sunDiskAngularRadius": { "type": "number" },
                "sunDiskIntensity": { "type": "number" },
            },
            "required": [
                "enabled", "planetRadius", "atmosphereHeight", "rayleighScattering",
                "rayleighScaleHeight", "mieScattering", "mieScaleHeight", "mieAnisotropy",
                "ozoneAbsorption", "sunDiskAngularRadius", "sunDiskIntensity",
            ],
        }),
    );

    let mut aggregate_props = Map::new();
    for name in component_names {
        aggregate_props.insert(
            (*name).into(),
            json!({ "$ref": format!("#/components/schemas/{name}") }),
        );
    }
    schemas.insert(
        "Components".into(),
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": Value::Object(aggregate_props),
        }),
    );
    let body: Vec<Value> = component_names
        .iter()
        .map(|name| json!({ "$ref": format!("#/components/schemas/{name}") }))
        .collect();
    schemas.insert("ComponentBody".into(), json!({ "oneOf": body }));
    schemas.insert(
        "Environment".into(),
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "skyMode": { "type": "string", "enum": ["color", "texture", "procedural"] },
                "clearColor": vec3,
                "skyTexture": uuid,
                "skyIntensity": { "type": "number" },
                "skyRotation": { "type": "number" },
                "exposure": { "type": "number" },
                "visible": { "type": "boolean" },
                "useSkyForAmbient": { "type": "boolean" },
                "ambientColor": vec3,
                "ambientIntensity": { "type": "number" },
                "atmosphere": { "$ref": "#/components/schemas/AtmosphereSettingsDto" },
            },
            "required": [
                "skyMode", "clearColor", "skyTexture", "skyIntensity", "skyRotation", "exposure",
                "visible", "useSkyForAmbient", "ambientColor", "ambientIntensity", "atmosphere",
            ],
        }),
    );

    schemas
}

/// The 21 scene-component shape names, in the canonical registration order — the order the
/// `Components` aggregate and the `ComponentBody` union enumerate.
pub const COMPONENT_NAMES: &[&str] = &[
    "Name",
    "Transform",
    "Mesh",
    "Camera",
    "MaterialSet",
    "Script",
    "DirectionalLight",
    "PointLight",
    "SpotLight",
    "ReflectionProbe",
    "Relationship",
    "SkinnedMesh",
    "Bone",
    "ModelInstance",
    "FootIk",
    "BonePhysics",
    "Rigidbody",
    "Collider",
    "KinematicBones",
    "CharacterController",
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::*;

    /// A canonical deep key-sort so two equivalent JSON objects compare equal regardless of
    /// `properties` insertion order (the phase gate's "after a canonical key sort").
    fn sorted(value: &Value) -> Value {
        match value {
            Value::Object(map) => {
                let mut keys: Vec<&String> = map.keys().collect();
                keys.sort();
                let mut out = serde_json::Map::new();
                for key in keys {
                    out.insert(key.clone(), sorted(&map[key]));
                }
                Value::Object(out)
            }
            Value::Array(items) => Value::Array(items.iter().map(sorted).collect()),
            other => other.clone(),
        }
    }

    #[test]
    fn plain_dto_matches_cpp_object_shape() {
        let got = fragment_for::<RaycastParams>("RaycastParams");
        let want = json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "origin": { "$ref": "#/components/schemas/Vec3" },
                "dir": { "$ref": "#/components/schemas/Vec3" },
                "maxDist": { "type": "number" },
            },
            "required": ["origin", "dir"],
        });
        assert_eq!(sorted(&got), sorted(&want));
    }

    #[test]
    fn positional_field_order_is_declaration_order_including_optionals() {
        // The positional-argument order interleaves required and optional fields by their
        // declaration position — `RaycastParams { origin, dir, max_dist: Option }` is
        // `[origin, dir, maxDist]`, the optional `maxDist` keeping its slot.
        assert_eq!(
            positional_field_order::<RaycastParams>(),
            ["origin", "dir", "maxDist"]
        );
        // A single-field params (the `create-entity {name}` / `set-exposure {ev}` shape).
        assert_eq!(positional_field_order::<CreateEntityParams>(), ["name"]);
        assert_eq!(positional_field_order::<SetExposureParams>(), ["ev"]);
        // A lone-optional params (`set-aa {mode?}`): the field still occupies index 0.
        assert_eq!(positional_field_order::<SetAaParams>(), ["mode"]);
        // A multi-field params with a non-trivial order (`set-component {entity, component, json}`).
        assert_eq!(
            positional_field_order::<SetComponentParams>(),
            ["entity", "component", "json"]
        );
    }

    #[test]
    fn required_lists_non_option_fields_in_declaration_order() {
        // `RagdollResult { present, active, body_weight, bones }` are all non-`Option`, so
        // `required` lists them all, in declaration order.
        let got = fragment_for::<RagdollResult>("RagdollResult");
        assert_eq!(
            got["required"],
            json!(["present", "active", "bodyWeight", "bones"])
        );
    }

    #[test]
    fn option_fields_drop_from_required_and_unwrap_nullable() {
        // `RaycastParams.max_dist` is `Option<f32>`: absent from `required`, and its property
        // schema is the unwrapped `{type:number}`, not `{type:[number,null]}`.
        let got = fragment_for::<RaycastParams>("RaycastParams");
        assert_eq!(got["required"], json!(["origin", "dir"]));
        assert_eq!(got["properties"]["maxDist"], json!({ "type": "number" }));
    }

    #[test]
    fn scalar_types_match_json_schema_for() {
        // `f32` → number, `u64`/`i32` → integer, `bool` → boolean, `Uuid` → string.
        let ragdoll = fragment_for::<RagdollResult>("RagdollResult");
        assert_eq!(
            ragdoll["properties"]["bodyWeight"],
            json!({ "type": "number" })
        );
        assert_eq!(ragdoll["properties"]["bones"], json!({ "type": "integer" }));
        assert_eq!(
            ragdoll["properties"]["present"],
            json!({ "type": "boolean" })
        );

        let frame = fragment_for::<FrameSampleDto>("FrameSampleDto");
        // `frame_index: u64` reports integer (the `int64` format is stripped).
        assert_eq!(
            frame["properties"]["frameIndex"],
            json!({ "type": "integer" })
        );

        let inspect = fragment_for::<InspectResult>("InspectResult");
        assert_eq!(inspect["properties"]["id"], json!({ "type": "string" }));
    }

    #[test]
    fn enum_field_inlines_string_enum() {
        // `SetAaParams.mode: Option<AaModeDto>` inlines to `{type:string, enum:[...]}` — not a
        // `$ref` — and drops from `required` (it is optional).
        let got = fragment_for::<SetAaParams>("SetAaParams");
        assert_eq!(
            got["properties"]["mode"],
            json!({ "type": "string", "enum": ["off", "fxaa", "taa", "msaa2", "msaa4", "msaa8"] })
        );
        assert_eq!(got["required"], json!([]));
    }

    #[test]
    fn array_of_refs_points_at_components() {
        let got = fragment_for::<EntityList>("EntityList");
        assert_eq!(
            got["properties"]["entities"],
            json!({ "type": "array", "items": { "$ref": "#/components/schemas/EntityListEntry" } })
        );
    }

    #[test]
    fn selection_result_entity_is_oneof_entityref_or_null() {
        let got = fragment_for::<SelectionResult>("SelectionResult");
        assert_eq!(
            got["properties"]["entity"],
            json!({ "oneOf": [{ "$ref": "#/components/schemas/EntityRef" }, { "type": "null" }] })
        );
    }

    #[test]
    fn inspect_result_components_is_ref_components() {
        let got = fragment_for::<InspectResult>("InspectResult");
        assert_eq!(
            got["properties"]["components"],
            json!({ "$ref": "#/components/schemas/Components" })
        );
    }

    #[test]
    fn set_component_params_json_is_ref_component_body() {
        let got = fragment_for::<SetComponentParams>("SetComponentParams");
        assert_eq!(
            got["properties"]["json"],
            json!({ "$ref": "#/components/schemas/ComponentBody" })
        );
        // The `entity` selector resolves to the uuid-or-id `oneOf`.
        assert_eq!(
            got["properties"]["entity"],
            json!({ "oneOf": [{ "type": "string" }, { "type": "integer" }] })
        );
    }

    #[test]
    fn environment_dto_is_bare_ref_environment() {
        let got = fragment_for::<EnvironmentDto>("EnvironmentDto");
        assert_eq!(got, json!({ "$ref": "#/components/schemas/Environment" }));
    }

    #[test]
    fn selector_field_is_oneof_string_or_integer() {
        // A selector param: `entity` is the uuid-or-id `oneOf`.
        let got = fragment_for::<ComponentParams>("ComponentParams");
        assert_eq!(
            got["properties"]["entity"],
            json!({ "oneOf": [{ "type": "string" }, { "type": "integer" }] })
        );
    }

    #[test]
    fn opaque_json_field_is_empty_schema() {
        // `MaterialSetGraphParams.graph: Value` is a `Json` blob (not a selector), so `{}`.
        let got = fragment_for::<MaterialSetGraphParams>("MaterialSetGraphParams");
        assert_eq!(got["properties"]["graph"], json!({}));
        // `material` is a selector, so the `oneOf`.
        assert_eq!(
            got["properties"]["material"],
            json!({ "oneOf": [{ "type": "string" }, { "type": "integer" }] })
        );
    }

    #[test]
    fn component_schemas_carry_the_hand_authored_shapes() {
        let schemas = component_schemas();
        // The 21 component shapes + Vec3/Vec4 are emitted elsewhere (the DTO structs), but the
        // aggregates + Environment + Atmosphere live here.
        for name in COMPONENT_NAMES {
            assert!(
                schemas.contains_key(*name),
                "missing component shape {name}"
            );
        }
        assert!(schemas.contains_key("Components"));
        assert!(schemas.contains_key("ComponentBody"));
        assert!(schemas.contains_key("Environment"));
        assert!(schemas.contains_key("AtmosphereSettingsDto"));
        assert_eq!(COMPONENT_NAMES.len(), 20);
    }

    #[test]
    fn component_schemas_match_committed_openrpc() {
        // The hand-authored block must equal the committed `openrpc.generated.json` byte-shape
        // (after a canonical key sort) for the 20 shapes + the aggregates + Environment.
        let committed: Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../schemas/control/openrpc.generated.json"
        )))
        .expect("committed openrpc.generated.json parses");
        let committed = &committed["components"]["schemas"];

        let schemas = component_schemas();
        for (name, got) in &schemas {
            let want = &committed[name];
            assert_eq!(
                sorted(got),
                sorted(want),
                "component shape {name} drifted from the committed openrpc"
            );
        }
    }

    #[test]
    fn bvec3_lock_fields_are_closed_boolean_objects() {
        let rigidbody = &component_schemas()["Rigidbody"];
        let lock = &rigidbody["properties"]["lockPosition"];
        assert_eq!(lock["type"], json!("object"));
        assert_eq!(lock["additionalProperties"], json!(false));
        assert_eq!(lock["properties"]["x"], json!({ "type": "boolean" }));
    }
}
