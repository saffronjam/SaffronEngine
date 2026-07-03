+++
title = 'Components'
weight = 2
+++

# Components

A component is a plain value struct: no base type, no methods, no attached behavior — only data the
[systems](../ecs-architecture/) read with `for_each`. Behavior such as serialize, deserialize, add,
remove, and clone is attached separately through the [component registry](../component-registry/),
so the struct depends on nothing but `glam` and `serde_json`.

Keeping a component pure data lets one entity be serialized, inspected, cloned, and rendered by
several unrelated subsystems, none aware of the others. The per-component serde lives in the
[registry](../component-registry/) instead, as one `impl` per component.

## Identity

Every entity from `create_entity` carries an id, a name, a transform, and a root relationship
automatically.

```rust
pub struct Name { pub name: String }
pub struct IdComponent { pub id: Uuid }
```

`IdComponent` is the entity's stable [`Uuid`](../scene-serialization/). ECS handles are not stable
across runs and can alias between worlds, so every cross-entity reference keys off the `Uuid`. It is
left unregistered and skipped during serialization — written as the entity's top-level `id`, not
inside `components`. `Name` is the label shown in the hierarchy.

## Transform

Rotation is stored as Euler XYZ in radians. The inspector edits these values directly, avoiding the
gimbal clipping that arises when the UI decomposes a quaternion. Matrix composition is its own page;
see [Transforms](../transform-and-matrices/).

```rust
pub struct Transform {
    pub translation: Vec3,
    pub scale: Vec3,
    pub rotation: Vec3,  // Euler XYZ radians
}
```

## Hierarchy

`Relationship` makes the entity a node in the [scene tree](../scene-hierarchy/): a durable parent
`Uuid` (`0` means root) plus runtime `parent_handle`/`children` caches that never serialize. Every
entity gets a root one from `create_entity`. It is registered non-removable, and parenting is edited
through `set_parent` rather than as a raw field.

```rust
pub struct Relationship {
    pub parent: Uuid,                       // Uuid(0) == root
    pub parent_handle: Option<Entity>,      // resolved cache, never serialized
    pub children: Vec<Entity>,              // derived cache, never serialized
}
```

## Mesh and material

`Mesh` references a mesh asset by [`Uuid`](../asset-catalog-in-scene/); the
[asset server](../../geometry-and-assets/asset-server-and-catalog/) resolves it to a GPU mesh at
draw time. The component holds no GPU handle, so it survives a project reload that rebuilds the
caches.

```rust
pub struct Mesh { pub mesh: Uuid }

pub struct Material {
    pub base_color: Vec4,
    pub albedo_texture: Uuid,               // 0 == none
    pub metallic_roughness_texture: Uuid,   // 0 == none; glTF MR map (rough=G, metal=B)
    pub metallic: f32,
    pub roughness: f32,
    pub emissive: Vec3,
    pub emissive_strength: f32,
    pub unlit: bool,                        // skip lighting — a distinct PSO
    pub blend_mode: BlendMode,              // opaque / masked / translucent
    // ... normal / occlusion / emissive / height maps, UV tiling, parallax, alpha cutoff
}
```

`Material` is per-entity and applies to the whole mesh. `albedo_texture == 0` means none: the
renderer binds its default white texture, so `base_color` shows directly. `metallic` and `roughness`
feed the [Cook-Torrance BRDF](../../lighting-and-brdf/cook-torrance-brdf/); `unlit` selects a
separate [PSO permutation](../../materials-and-pipelines/ubershader-and-specialization/). The
metallic-roughness map is glTF's packed map (roughness in G, metalness in B), a **linear** texture
unlike the sRGB albedo; the default white (when none) leaves the scalar factors unchanged.

A mesh imported with more than one material instead carries a `MaterialSet`: an ordered table of
`MaterialSlot`s, each with the same fields as `Material`. Every
[`Submesh.material_slot`](../../geometry-and-assets/mesh-and-vertex-layout/) indexes this table, so
each submesh draws with its own material.

```rust
pub struct MaterialSlot { /* same fields as Material */ }
pub struct MaterialSet { pub slots: Vec<MaterialSlot> }
```

An entity uses one or the other: single-material meshes (and hand-created entities) keep `Material`;
a multi-material import gets `MaterialSet`. A shared `.smat` asset is referenced by a third
component, `MaterialAsset { material: Uuid }`, which takes precedence over the inline material when
present. The [draw list](../../geometry-and-assets/draw-list/) reads whichever is present.

## Camera

```rust
pub struct Camera {
    pub fov: f32,                    // vertical, degrees
    pub near_plane: f32,
    pub far_plane: f32,
    pub primary: bool,               // scene renders through the first primary camera
    pub show_model: bool,
    pub show_frustum: bool,
    pub frustum_max_distance: f32,
}
```

The camera's view comes from the entity's `Transform`, not the component itself: `primary_camera`
inverts the entity's world matrix. The component carries only projection parameters. The scene
renders through the first camera flagged `primary`.

`show_model`, `show_frustum`, and `frustum_max_distance` control editor helpers only. In edit mode
the host draws the camera placeholder model and a frustum capped by `frustum_max_distance`. Play
mode renders neither helper.

## Light types

```rust
pub struct DirectionalLight {
    pub direction: Vec3,  // way the light travels; default (-0.5, -1.0, -0.3)
    pub color: Vec3,
    pub intensity: f32,
    pub ambient: f32,     // default 0.15
}

pub struct PointLight { pub color: Vec3, pub intensity: f32, pub range: f32 }

pub struct SpotLight {
    pub direction: Vec3,
    pub color: Vec3,
    pub intensity: f32,
    pub range: f32,
    pub inner_angle: f32,  // full intensity inside this half-angle (deg)
    pub outer_angle: f32,  // zero past this half-angle (deg)
}
```

The directional light is the sun; the scene shades through the first one and carries a flat
`ambient` floor. Point and spot lights sit at the entity's `Transform` translation, since the
components hold no position of their own, and are
[culled into clusters](../../shadows-and-culling/clustered-light-culling/) by the light system. See
[light components](../../lighting-and-brdf/light-components/) for how `render_scene` packs these into
the GPU light buffer.

## In the code

| What | File | Symbols |
|---|---|---|
| Identity + transform | `scene/src/component.rs` | `IdComponent`, `Name`, `Transform` |
| Hierarchy | `scene/src/component.rs` | `Relationship`, `WorldTransform` |
| Skeleton | `scene/src/component.rs` | `SkinnedMesh`, `Bone` |
| Renderables | `scene/src/component.rs` | `Mesh`, `Material`, `MaterialSet`, `MaterialSlot`, `MaterialAsset` |
| Camera | `scene/src/component.rs` | `Camera` |
| Lights + probe | `scene/src/component.rs` | `DirectionalLight`, `PointLight`, `SpotLight`, `ReflectionProbe` |
| Camera resolve | `scene/src/hierarchy.rs` | `primary_camera` |
| Where each is registered | `scene/src/registry.rs` | `register_builtin_components` |

## Related
- [Component registry](../component-registry/) — how serde is attached to these structs
- [Transforms](../transform-and-matrices/) — the transform's matrix composition
- [Light components](../../lighting-and-brdf/light-components/) — how lights reach the GPU
