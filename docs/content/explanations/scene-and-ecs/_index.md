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
| `scene-mutation-journal` | Revisioned entity, component, and world-transform changes | `scene/src/journal.rs` · `SceneMutation` |
| `built-in-components` | Id, Name, Transform, Mesh, MaterialSet, Camera, the three light types | `scene/src/component.rs` |
| `transform-and-matrices` | `Transform` (Euler XYZ radians), `T·R·S` composition, the stable Euler extraction | `scene/src/hierarchy.rs` · `transform_matrix` |
| `scene-hierarchy` | parent/child via `Relationship`, cached world transforms, reparent + subtree destroy | `scene/src/hierarchy.rs` · `set_parent` |
| `component-registry` | the fn-pointer itable, `register_component!`, lookup by name/type | `scene/src/registry.rs` · `ComponentRegistry` |
| `scene-serialization` | registry-driven JSON save/load, uuid stability, version migration | `scene/src/document.rs` |
| `asset-catalog-in-scene` | `AssetCatalog` lives here; `Scene` holds an `Arc<AssetCatalog>` handle | `scene/src/environment.rs` · `AssetCatalog` |
| `picking` | ray vs. mesh triangles (AABB broad-phase), static + skinned, click-to-select | `assets/src/render_scene.rs` · `pick_entity` |
| `spatial-world` | exact world positions, hierarchical cells, surface fields, deterministic numerics, facet residency | `spatial/src/lib.rs` · `WorldCellKey` · `SurfaceField` |
| `vegetation-state` | Runtime cells, facet residency, queries, and strict persistence | `vegetation/src/runtime_world.rs` · `VegetationWorld`, `reduce_mutations` |
| `plant-promotion` | Transient entity views for macro plants, and state write-back | `runtime/src/vegetation_promotion.rs` · `VegetationPromotion`, `PlantOrigin` |
| `ecology-catchup` | Fixed ecology ticks, dependency regions, and budgeted catch-up | `vegetation/src/ecology_region.rs` · `advance_region` |
| `vegetation-navigation` | Obstacle/cost contributions and dirty-region delivery | `runtime/src/vegetation_navigation.rs` · `VegetationNavigationSeam` |
| `wind-field` | One deterministic sampled wind field every consumer reads identically | `wind/src/lib.rs` · `WindProfile`, `sample` |
