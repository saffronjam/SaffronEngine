+++
title = 'Components'
weight = 5
math = false
+++

# Components

The built-in components the ECS world holds, exported by `saffron-scene`. Vectors are the matching `glam` type (`Vec3` is pinned at 12 bytes so the downstream std430 layouts stay correct). The [component registry](../../explanations/scene-and-ecs/component-registry/) drives serialization and the inspector; the registered set is `BUILTIN_COMPONENT_NAMES` in `registry.rs`.

| What | File | Symbols |
|---|---|---|
| The component structs and their defaults | `component.rs` | every type below |
| The registry and the canonical name list | `registry.rs` | `register_builtin_components`, `BUILTIN_COMPONENT_NAMES` |

## Identity and hierarchy

`Name` and `Transform` are non-removable. `Relationship` carries the durable parent link; `IdComponent`, `WorldTransform`, `ComponentOrder` are runtime/document-only and never serialize through a registry row.

| Type | JSON key | Fields (default) |
|---|---|---|
| `Name` | `Name` | `name: String` |
| `Transform` | `Transform` | `translation: Vec3 {0,0,0}`; `scale: Vec3 {1,1,1}`; `rotation: Vec3 {0,0,0}` (Euler XYZ radians) |
| `Relationship` | `Relationship` | `parent: Uuid` (`0` = root); `parent_handle`, `children` are runtime caches (never serialized) |

`IdComponent { id: Uuid }` is the stable identity, written by the document assembler. `WorldTransform { matrix: Mat4 }` is the per-frame composed world matrix. `ComponentOrder { names: Vec<String> }` is the authored inspector row order. None of the three is a registered, removable row.

## Rendering

| Type | JSON key | Fields (default) |
|---|---|---|
| `Mesh` | `Mesh` | `mesh: Uuid {0}` (asset id; the asset server resolves it) |
| `Camera` | `Camera` | `fov: f32 45.0`; `near_plane: f32 0.1`; `far_plane: f32 100.0`; `primary: bool true`; `show_model: bool true`; `show_frustum: bool true`; `frustum_max_distance: f32 10.0` |

The scene renders through the first primary camera. `show_model` / `show_frustum` / `frustum_max_distance` drive the Edit-only camera placeholder and frustum overlay.

## Materials

`MaterialSet` is the one per-entity material component: an ordered list of `MaterialSlot`s, one per submesh. Each slot references a `.smat` [material asset](../../explanations/materials-and-pipelines/native-materials/) and layers a sparse per-object override map on top. A single-material mesh is a `MaterialSet` with one slot; each [`Submesh.material_slot`](../../explanations/geometry-and-assets/mesh-and-vertex-layout/) indexes the list (clamped to the last slot).

| Type | JSON key | Fields (default) |
|---|---|---|
| `MaterialSet` | `MaterialSet` | `slots: Vec<MaterialSlot>` |
| `MaterialSlot` | (slot) | `material: Uuid {0}` (the `.smat` asset id; `0` → built-in default); `overrides: {}` (sparse override map) |
| `ModelInstance` | `ModelInstance` | `model_id: Uuid {0}` (marks the root of an expanded `.smodel`) |

`overrides` is opaque `{ paramName: value }` JSON holding only the parameters that deviate from the referenced material. The recognized keys are the exposed PBR parameters (colours and vectors are arrays, texture ids are decimal strings, matching the `.smat` wire shape):

| Key | Type | Default | Note |
|---|---|---|---|
| `baseColor` | color4 | `[1,1,1,1]` | RGBA |
| `metallic` | scalar | `0.0` | |
| `roughness` | scalar | `1.0` | |
| `emissive` | color3 | `[0,0,0]` | |
| `emissiveStrength` | scalar | `1.0` | |
| `normalStrength` | scalar | `1.0` | |
| `alphaCutoff` | scalar | `0.5` | the `masked` discard/coverage threshold |
| `heightScale` | scalar | `0.05` | parallax depth |
| `uvTiling` | vec2 | `[1,1]` | |
| `uvOffset` | vec2 | `[0,0]` | |
| `unlit` | bool | `false` | skip lighting (distinct PSO) |
| `doubleSided` | bool | `false` | |
| `blend` | enum | `"opaque"` | `opaque` / `masked` (alpha-test + alpha-to-coverage under MSAA) / `translucent` (sorted blend pass) |
| `albedoTexture` | texture | `"0"` | sRGB; `0` = none |
| `ormTexture` | texture | `"0"` | packed ORM (AO=R, roughness=G, metallic=B); linear |
| `normalTexture` | texture | `"0"` | tangent-space (+Y) |
| `emissiveTexture` | texture | `"0"` | modulates `emissive` |
| `heightTexture` | texture | `"0"` | R, for parallax |

Parameter meanings and defaults live in the `.smat` asset (see [native materials](../../explanations/materials-and-pipelines/native-materials/)); an override names only the ones that differ for this object.

## Lights

| Type | JSON key | Fields (default) |
|---|---|---|
| `DirectionalLight` | `DirectionalLight` | `direction: Vec3 {-0.5,-1,-0.3}` (travel direction); `color: Vec3 {1,1,1}`; `intensity: f32 1.0`; `ambient: f32 0.15` |
| `PointLight` | `PointLight` | `color: Vec3 {1,1,1}`; `intensity: f32 5.0`; `range: f32 10.0` (positioned at the `Transform` translation) |
| `SpotLight` | `SpotLight` | `direction: Vec3 {0,-1,0}`; `color: Vec3 {1,1,1}`; `intensity: f32 5.0`; `range: f32 10.0`; `inner_angle: f32 20.0`; `outer_angle: f32 30.0` (half-angle degrees) |
| `ReflectionProbe` | `ReflectionProbe` | `influence_radius: f32 10.0`; `intensity: f32 1.0`; `box_projection: bool false`; `box_extent: Vec3 {10,10,10}`; `dirty: bool true` (capture pending; runtime) |

## Animation and skinning

| Type | JSON key | Fields |
|---|---|---|
| `SkinnedMesh` | `SkinnedMesh` | `mesh: Uuid`; `root_bone: Uuid`; `bones: Vec<Uuid>` (glTF skin order); `inverse_bind: Vec<Mat4>`; `bone_handles` runtime cache |
| `Bone` | `Bone` | `tag: u8` — marks a skeleton joint (serialized as an empty object) |
| `AnimationPlayer` | `AnimationPlayer` | `clip: Uuid`; `time: f32`; `speed: f32 1.0`; `wrap: Wrap (Loop)`; `playing: bool`; plus runtime transition state (`prev_clip`, `transition`, `loop_blend`, `transition_mode: Transition`) |
| `FootIk` | `FootIk` | `enabled: bool`; `ground_height: f32`; `chains: Vec<FootChain>` (each `{upper, mid, end: i32, pole_vector: Vec3}`, indices into `SkinnedMesh::bones`) |
| `MorphComponent` | `Morph` | `weights: Vec<f32>` (canonical `0..1`, one per target); `names: Vec<String>` (parallel slider labels) |

`Wrap` is `Once | Loop | PingPong` (default `Loop`); `Transition` is `Inertialize | CrossFade` (default `Inertialize`). `PoseOverride { translation: Vec3, rotation: Quat, scale: Vec3 }` is the runtime, non-serialized animated local TRS the evaluator writes onto a driven bone (preferred over the bone's `Transform`). `MorphWeightOverride { weights: Vec<f32> }` is the animated counterpart the GPU morph deform reads; it is removed when the rig stops animating, so the mesh reverts to the durable `Morph` weights.

## Physics

| Type | JSON key | Fields (default) |
|---|---|---|
| `Rigidbody` | `Rigidbody` | `motion: Motion (Dynamic)`; `mass: f32 1.0`; `linear_damping: f32 0.05`; `angular_damping: f32 0.05`; `gravity_factor: f32 1.0`; `wind_factor: f32 0.0`; `lock_position: BVec3`; `lock_rotation: BVec3`; `collision_layer: i32 0` |
| `Collider` | `Collider` | `shape: Shape (Box)`; `half_extents: Vec3 {0.5,0.5,0.5}`; `source_mesh: Uuid 0`; `offset: Vec3 {0,0,0}`; `material: PhysicsMaterial`; `is_sensor: bool false` |
| `KinematicBones` | `KinematicBones` | `enabled: bool true`; `driven: Vec<i32>` (joint indices; empty = every joint) |
| `CharacterController` | `CharacterController` | `max_speed: f32 4.0`; `max_slope_angle: f32 ~0.785`; `max_step_height: f32 0.3`; `gravity_factor: f32 1.0`; plus runtime velocity/ground state |
| `BonePhysics` | `BonePhysics` | `bones: Vec<BonePhysics>` — reserved per-bone ragdoll metadata (parallel to the rig's `bones`) |

`Motion` is `Static | Kinematic | Dynamic` (default `Dynamic`). `Shape` is `Box | Sphere | Capsule | ConvexHull | Mesh` (default `Box`). `PhysicsMaterial` is `{ friction: f32 0.5, restitution: f32 0.0 }`. `Joint` (in the per-bone `BonePhysics` struct) is `Fixed | Hinge | SwingTwist | Free` (default `SwingTwist`).

## Scripting

| Type | JSON key | Fields |
|---|---|---|
| `Script` | `Script` | `scripts: Vec<ScriptSlot>`, run top-to-bottom each play tick |

`ScriptSlot` is `{ script_path: String, overrides: serde_json::Value }` — a `.lua` path relative to the project `src/` plus opaque per-instance field overrides (defaulted to `{}`; the engine never interprets them).

## Environment and vegetation

| Type | JSON key | Fields (default) |
|---|---|---|
| `VegetationField` | `VegetationField` | `map: Uuid {0}` (the `.svegmap` asset); `enabled: bool true` |
| `WindSource` | `WindSource` | `kind: WindSourceKind (Directional)`; `strength: f32 5.0` (m/s; `Volume` scales the global field); `radius: f32 20.0`; `falloff: f32 0.5` (fraction of radius); `enabled: bool true` |
| `FogVolume` | `FogVolume` | `shape: FogShape (Box)`; `extents: Vec3 {5,5,5}`; `radius: f32 5.0`; `edge_falloff: f32 1.0`; `density: f32 0.5`; `albedo: Vec3 {0.9,0.9,0.9}`; `emissive: Vec3 {0,0,0}`; `phase_g: f32 0.0`; `height_falloff: f32 0.0`; `noise_scale: f32 0.2`; `noise_intensity: f32 0.0`; `noise_detail: f32 0.5`; `wind: Vec3 {0,0,0}`; `speed: f32 0.1` |

`VegetationField` is the one scene-level vegetation component: local regions are layers inside the referenced map, not further field components. `FogShape` is `Box | Sphere` (default `Box`); a `Box` is bounded by local-space `extents`, a `Sphere` by `radius` around the entity origin, and density smoothsteps to zero over `edge_falloff` inside the bound. `WindSourceKind` is owned by `saffron-wind`.

## Related

- [Built-in components](../../explanations/scene-and-ecs/built-in-components/) — what each is for
- [Component registry](../../explanations/scene-and-ecs/component-registry/) — how a component is registered and serialized
- [Light components](../../explanations/lighting-and-brdf/light-components/) — the light types in the BRDF
