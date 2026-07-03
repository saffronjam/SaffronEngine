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

`Material` is the per-entity material applied to the whole mesh. A multi-material mesh carries `MaterialSet` instead; a shared `.smat` asset is referenced by `MaterialAsset` (which takes precedence over the inline material when present).

| Type | JSON key | Fields (default) |
|---|---|---|
| `Material` | `Material` | see below |
| `MaterialSet` | `MaterialSet` | `slots: Vec<MaterialSlot>` (each slot has the `Material` fields) |
| `MaterialAsset` | `MaterialAsset` | `material: Uuid {0}` (the `.smat` asset id; `0` → built-in default) |
| `ModelInstance` | `ModelInstance` | `model_id: Uuid {0}` (marks the root of an expanded `.smodel`) |

`Material` fields (all `Uuid` texture ids default to `0` = none → renderer default):

| Field | Type | Default | Note |
|---|---|---|---|
| `base_color` | `Vec4` | `{1,1,1,1}` | RGBA |
| `albedo_texture` | `Uuid` | `0` | sRGB; 0 = default white |
| `metallic_roughness_texture` | `Uuid` | `0` | glTF map (rough=G, metal=B); linear |
| `metallic` | `f32` | `0.0` | |
| `roughness` | `f32` | `1.0` | |
| `emissive` | `Vec3` | `{0,0,0}` | |
| `emissive_strength` | `f32` | `1.0` | |
| `unlit` | `bool` | `false` | distinct PSO |
| `normal_texture` | `Uuid` | `0` | tangent-space (+Y) |
| `occlusion_texture` | `Uuid` | `0` | AO in R |
| `emissive_texture` | `Uuid` | `0` | modulates `emissive` |
| `height_texture` | `Uuid` | `0` | R, for parallax |
| `normal_strength` | `f32` | `1.0` | |
| `uv_tiling` | `Vec2` | `{1,1}` | |
| `uv_offset` | `Vec2` | `{0,0}` | |
| `height_scale` | `f32` | `0.05` | parallax depth |
| `blend` | `enum` | `"opaque"` | `opaque` / `masked` (alpha-test + alpha-to-coverage under MSAA) / `translucent` (sorted blend pass) |
| `alpha_cutoff` | `f32` | `0.5` | the `masked` discard/coverage threshold |

`MaterialSlot` has the same field set as `Material` and the same defaults.

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

`Wrap` is `Once | Loop | PingPong` (default `Loop`); `Transition` is `Inertialize | CrossFade` (default `Inertialize`). `PoseOverride { translation: Vec3, rotation: Quat, scale: Vec3 }` is the runtime, non-serialized animated local TRS the evaluator writes onto a driven bone (preferred over the bone's `Transform`).

## Physics

| Type | JSON key | Fields (default) |
|---|---|---|
| `Rigidbody` | `Rigidbody` | `motion: Motion (Dynamic)`; `mass: f32 1.0`; `linear_damping: f32 0.05`; `angular_damping: f32 0.05`; `gravity_factor: f32 1.0`; `lock_position: BVec3`; `lock_rotation: BVec3`; `collision_layer: i32 0` |
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

## Related

- [Built-in components](../../explanations/scene-and-ecs/built-in-components/) — what each is for
- [Component registry](../../explanations/scene-and-ecs/component-registry/) — how a component is registered and serialized
- [Light components](../../explanations/lighting-and-brdf/light-components/) — the light types in the BRDF
