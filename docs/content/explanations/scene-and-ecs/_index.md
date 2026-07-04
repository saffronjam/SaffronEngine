+++
title = 'Scene & ECS'
weight = 6
bookCollapseSection = true
+++

# Scene & ECS

The scene is the game world, modelled as a `hecs` ECS of value components wrapped behind a fixed
`Scene` access surface. At its centre is the component registry, a struct-of-fn-pointers table that
describes a component to the serializer through one `register_component!` line. No central switch
needs editing when a component is added.

## Pages

| Page | Covers | Code |
|---|---|---|
| `ecs-architecture` | `hecs`-backed `Scene`/`Entity`, component-access methods, `for_each` | `scene/src/scene.rs` |
| `built-in-components` | Id, Name, Transform, Mesh, MaterialSet, Camera, the three light types | `scene/src/component.rs` |
| `transform-and-matrices` | `Transform` (Euler XYZ radians), `T·R·S` composition, the stable Euler extraction | `scene/src/hierarchy.rs` · `transform_matrix` |
| `scene-hierarchy` | parent/child via `Relationship`, cached world transforms, reparent + subtree destroy | `scene/src/hierarchy.rs` · `set_parent` |
| `component-registry` | the fn-pointer itable, `register_component!`, lookup by name/type | `scene/src/registry.rs` · `ComponentRegistry` |
| `scene-serialization` | registry-driven JSON save/load, uuid stability, version migration | `scene/src/document.rs` |
| `asset-catalog-in-scene` | `AssetCatalog` lives here; `Scene` holds an `Arc<AssetCatalog>` handle | `scene/src/environment.rs` · `AssetCatalog` |
| `picking` | ray vs. mesh triangles (AABB broad-phase), static + skinned, click-to-select | `assets/src/render_scene.rs` · `pick_entity` |
