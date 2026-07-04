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
```

An entity's material is one component, `MaterialSet`: an ordered list of `MaterialSlot`s, one per
submesh. A slot is a **reference plus overrides** — a `.smat`
[material asset](../../materials-and-pipelines/native-materials/) id and a sparse per-object override
map applied over that material's resolved parameters. A single-material mesh is a `MaterialSet` with
one slot.

```rust
pub struct MaterialSlot {
    pub material: Uuid,   // the referenced .smat asset; 0 == the built-in default
    pub overrides: Value, // sparse { paramName: value }, {} when nothing is overridden
}
pub struct MaterialSet { pub slots: Vec<MaterialSlot> }
```

Each [`Submesh.material_slot`](../../geometry-and-assets/mesh-and-vertex-layout/) indexes the list
(clamped to the last slot), so each submesh draws with its own slot. `material == 0` binds the
built-in default. `overrides` is opaque editor-shaped JSON; the engine applies only the recognized
exposed PBR parameters — `baseColor`, `metallic`, `roughness`, `emissive`, the texture ids
(`albedoTexture`, the packed `ormTexture`, …), and the rest of the exposed set — at resolve time.
Because a slot references the `.smat` rather than copying its factors, editing that material
re-renders every entity that points at it; the override map carries only the per-object deviations.
The [draw list](../../geometry-and-assets/draw-list/) resolves the set into one material per submesh.

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
| Renderables | `scene/src/component.rs` | `Mesh`, `MaterialSet`, `MaterialSlot` |
| Camera | `scene/src/component.rs` | `Camera` |
| Lights + probe | `scene/src/component.rs` | `DirectionalLight`, `PointLight`, `SpotLight`, `ReflectionProbe` |
| Camera resolve | `scene/src/hierarchy.rs` | `primary_camera` |
| Where each is registered | `scene/src/registry.rs` | `register_builtin_components` |

## Related
- [Component registry](../component-registry/) — how serde is attached to these structs
- [Transforms](../transform-and-matrices/) — the transform's matrix composition
- [Light components](../../lighting-and-brdf/light-components/) — how lights reach the GPU
