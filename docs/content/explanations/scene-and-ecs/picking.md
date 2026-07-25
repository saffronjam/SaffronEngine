+++
title = 'Picking'
weight = 7
+++

# Picking

Picking maps a viewport coordinate to the visible scene object beneath it. Anima tests editor billboards first, then sends one world-space ray through the shared [surface-field contract](../spatial-world/). A hit selects the nearest object; a miss clears the selection.

Surface testing uses two levels of rejection. A world-space bounding box removes distant entities cheaply. Static meshes then descend a cached mesh-local [bounding-volume hierarchy](https://pbr-book.org/4ed/Primitives_and_Intersection_Acceleration/Bounding_Volume_Hierarchies) to the few triangles crossed by the ray. This keeps selection accurate at a mesh silhouette without testing every triangle on every click.

## Viewport ray

The `pick` command receives viewport UV coordinates with `(0, 0)` at the top left. It converts them to normalized device coordinates as `(u * 2 - 1, v * 2 - 1)`, preserving the rendered image's downward-positive Y direction.

`viewport_ray` builds the same projection used for drawing, including Vulkan's Y flip. It unprojects the point at depth `0` and depth `1`, then normalizes the vector between them.

```rust
let ndc = Vec2::new(u * 2.0 - 1.0, v * 2.0 - 1.0);
let ray = viewport_ray((width, height), &camera, ndc);
```

The projection and depth convention must match the rendered frame. Omitting the Y flip would test a vertically mirrored screen position.

## Editor billboards

Point lights, spot lights, and cameras can be visible in the editor without a mesh. `pick_billboard` projects their world positions to viewport pixels and tests a 26-pixel square centered on each glyph. Mesh-bearing entities stay on the surface-pick path.

A billboard hit wins before any mesh test. This makes small editor controls selectable even when geometry lies behind their icon.

## Static meshes

For each entity with `Transform` and `Mesh`, the shared query follows this path:

1. Ignore entities tagged `PreviewGhost`, so a placement preview cannot become its own target.
2. Resolve the mesh and its CPU positions and indices through `AssetServer`.
3. Transform the eight local bounds corners and test the resulting world-space axis-aligned box.
4. Build or reuse the mesh's cached `MeshBvh`.

The surviving entity becomes a provider and enters the shared narrow phase:

1. Construct a `StaticMeshSurfaceProvider` with the entity's stable provider ID and content revision.
2. Transform the world ray into mesh-local space and call `MeshBvh::raycast_hit`.
3. Return the exact world position, geometric tangent frame, UV, weighted material tag, stable triangle attachment, and provider revision.
4. Compare metric world distance and then stable provider ID with other hits.

The cache is keyed by mesh asset ID, so entities sharing one mesh also share one hierarchy. Empty or degenerate triangle data produces no hierarchy and cannot be picked.

## Skinned meshes

Skinned geometry cannot use the rest-pose hierarchy for an exact surface hit. The shared query rebuilds the current joint palette, applies each vertex's four joint weights on the CPU, and tests the deformed world-space triangles.

A conservative broad phase transforms the bind-pose bounds through every joint and unions the results. Only a ray that crosses this box pays for CPU skinning and triangle intersection. The deformation matches `skin.slang`, so selection follows the pose shown in the viewport without a GPU readback. Capabilities explicitly mark the result as non-authoritative: it has no stable attachment, nearest query, or quantized field tile.

## Vegetation

The same viewport ray also queries the [vegetation world](../../geometry-and-assets/plant-rendering/). `VegetationWorld::query_ray` walks each resident cell's macro BVH and returns plants by stable `PlantId`, resolved through the CPU cell snapshot — never a GPU slot index. The nearest macro plant competes with the entity surface hit by metric distance.

Micro vegetation has no per-blade identity, so a micro hit is paint feedback rather than selection. `query_micro_ray` intersects the ray with each resident cell's floor plane and accepts the crossing only where the landing texel of a micro field tile carries nonzero density. The result is a world-space position; it loses distance ties to entity surfaces and macro plants.

## Selection result

`query_scene_surface_ray` returns a `SceneSurfaceHit` containing the mesh entity, complete `SurfaceHit`, and provider capabilities. `pick_scene_surface` builds the viewport ray and consumes that query; `pick_entity` reduces the result to an entity or `Entity::NULL`. Picking does not own a second triangle-intersection path.

The control command performs one final ownership step: if the surface belongs to an expanded `ModelInstance` subtree, it selects the model root. The hierarchy therefore treats an imported model as one editor object even when the ray struck a nested mesh node.

| Click target | `PickResult.kind` | Selection |
|---|---|---|
| Light or camera glyph | `billboard` | Glyph entity |
| Static or skinned surface | `mesh` | Model root, or hit entity outside a model |
| Macro plant | `vegetation` | None; the result carries the stable `plant` id |
| Micro field ground | `micro-vegetation` | None; the result carries the world `position` |
| Empty viewport | absent | Cleared |

## Source map

| What | File | Symbols |
|---|---|---|
| Ray construction and shared scene query | `engine/crates/assets/src/render_scene.rs` | `viewport_ray`, `query_scene_surface_ray`, `pick_scene_surface`, `pick_entity` |
| Static mesh surface adapter | `engine/crates/assets/src/mesh_surface.rs` | `StaticMeshSurfaceProvider`, `SurfaceField` |
| Static-mesh hierarchy cache | `engine/crates/assets/src/load.rs` | `AssetServer::mesh_pick_bvh` |
| Bounds and intersection math | `engine/crates/geometry/src/picking.rs` | `MeshBvh`, `raycast_hit`, `nearest_hit_transformed`, `ray_triangle_coordinates` |
| Joint palette | `engine/crates/scene/src/hierarchy.rs` | `Scene::joint_matrices` |
| Billboard priority and selection | `engine/crates/control/src/commands_scene.rs` | `pick_billboard`, `pick` |

## Related

- [Transforms](../transform-and-matrices/)
- [Selection](../../ui-and-editor/selection/)
- [Editor camera](../../ui-and-editor/editor-camera/)
