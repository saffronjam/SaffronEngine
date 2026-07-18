+++
title = 'Components'
weight = 2
+++

# Components

Anima represents entity state as plain Rust components stored in a [`hecs`](https://docs.rs/hecs/0.11/hecs/) world. Components carry data; systems in the scene, rendering, animation, scripting, and physics crates decide what that data means each frame.

Serialization and editor operations do not live on a common base class. The [component registry](../component-registry/) associates each authored component type with a stable name, JSON conversion, copy operations, and a removability flag.

## Entity foundation

`Scene::create_entity` seeds five pieces of state:

| Component | Purpose | Persistence |
|---|---|---|
| `IdComponent` | Stable `Uuid` used outside the current ECS world | Top-level entity `id` |
| `Name` | Human-readable hierarchy label | Registered as `Name` |
| `Transform` | Local translation, Euler XYZ rotation in radians, and scale | Registered as `Transform` |
| `Relationship` | Parent ID plus resolved hierarchy caches | Only the parent ID persists |
| `ComponentOrder` | Authored Inspector and JSON row order | Top-level `componentOrder` |

`Name`, `Transform`, and `Relationship` are non-removable registry rows. An entity can lose every optional capability without losing its identity or place in the scene tree.

## Authored component families

The built-in registry contains these component rows:

| Family | Components | Role |
|---|---|---|
| Geometry | `Mesh`, `MaterialSet`, `ModelInstance` | Mesh asset, submesh material bindings, and source model identity |
| View | `Camera` | Perspective projection and editor camera helpers |
| Lighting | `DirectionalLight`, `PointLight`, `SpotLight`, `ReflectionProbe`, `FogVolume` | Direct lights, local reflection capture, and bounded participating media |
| Animation | `AnimationPlayer`, `SkinnedMesh`, `Morph`, `Bone`, `FootIk`, `BonePhysics`, `KinematicBones` | Clip playback, deformation, skeleton metadata, IK, and bone/physics coupling |
| Physics | `Rigidbody`, `Collider`, `CharacterController` | Motion, collision shape and material, and virtual-character state |
| Scripting | `Script` | Ordered `.lua` attachments and per-entity field overrides |

The canonical registry order and names live in `BUILTIN_COMPONENT_NAMES`. This list is also a completeness check: every listed name must resolve to one registry row, and every row must appear in the list.

## Asset references

Components store asset IDs rather than GPU handles or filesystem paths. `Mesh` has one mesh `Uuid`; `ModelInstance` records its `.smodel` ID; animation and material components follow the same rule. The [asset catalog](../asset-catalog-in-scene/) resolves those IDs after a project load rebuilds its caches.

`MaterialSet` contains one `MaterialSlot` per submesh binding. Each slot references a `.smat` asset and carries a sparse JSON object of per-entity overrides. A material ID of zero selects the built-in default material.

```rust
use saffron_core::Uuid;
use saffron_scene::{MaterialSet, MaterialSlot, Mesh, Scene};

fn make_renderable(scene: &mut Scene, mesh_id: Uuid) -> saffron_scene::Result<()> {
    let entity = scene.create_entity("Crate");
    scene.add_component(entity, Mesh { mesh: mesh_id })?;
    scene.add_component(
        entity,
        MaterialSet {
            slots: vec![MaterialSlot::default()],
        },
    )?;
    Ok(())
}
```

Changing a `.smat` updates every entity that references it. An override changes only that slot on that entity.

## Transform-owned placement

Several components deliberately omit position. Cameras derive their view from the entity's world transform. Point lights, reflection probes, fog volumes, rigid bodies, and character controllers use the entity translation; spot and directional lights add an authored direction.

This keeps placement in one component and lets parenting affect every spatial subsystem through the same [world-matrix update](../transform-and-matrices/).

## Runtime-only state

Some ECS values exist only to make frame processing efficient. `WorldTransform` caches the composed matrix. `PoseOverride` and `MorphWeightOverride` carry evaluated deformation state. `PreviewGhost` marks transient asset placement. `Relationship::parent_handle`, `Relationship::children`, and `SkinnedMesh::bone_handles` cache resolved entity handles.

These values are absent from the component registry or omitted by their serializers. A load reconstructs them from durable IDs and authored values, so serialized scenes never depend on unstable `hecs::Entity` handles.

## Source map

| What | File | Symbols |
|---|---|---|
| Component data types and defaults | `engine/crates/scene/src/component.rs` | `Transform`, `MaterialSet`, `Camera`, `Rigidbody`, `Script` |
| Entity creation and typed component access | `engine/crates/scene/src/scene.rs` | `Scene::create_entity`, `Scene::add_component`, `Scene::with_component` |
| Built-in row set | `engine/crates/scene/src/registry.rs` | `register_builtin_components`, `BUILTIN_COMPONENT_NAMES` |
| Registry macro | `engine/crates/scene/src/macros.rs` | `register_component!` |

## Related

- [Component registry](../component-registry/)
- [Transforms](../transform-and-matrices/)
- [Scene hierarchy](../scene-hierarchy/)
- [Light components](../../lighting-and-brdf/light-components/)
