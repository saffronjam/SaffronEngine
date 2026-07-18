+++
title = 'Picking'
weight = 7
+++

# Picking

Picking maps a viewport coordinate to the visible scene object beneath it. Anima tests editor billboards first, then casts a world-space ray against rendered mesh surfaces. A hit selects the nearest object; a miss clears the selection.

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

For each entity with `Transform` and `Mesh`, the picker follows this path:

1. Ignore entities tagged `PreviewGhost`, so a placement preview cannot become its own target.
2. Resolve the mesh and its CPU positions and indices through `AssetServer`.
3. Transform the eight local bounds corners and test the resulting world-space axis-aligned box.
4. Build or reuse the mesh's cached `MeshBvh`.
5. Transform the world ray into mesh-local space and call `MeshBvh::raycast`.
6. Transform the hit point back to world space and compare its world distance with other hits.

The cache is keyed by mesh asset ID, so entities sharing one mesh also share one hierarchy. Empty or degenerate triangle data produces no hierarchy and cannot be picked.

## Skinned meshes

Skinned geometry cannot use the rest-pose hierarchy for an exact surface hit. The picker rebuilds the current joint palette, applies each vertex's four joint weights on the CPU, and tests the deformed world-space triangles.

A conservative broad phase transforms the bind-pose bounds through every joint and unions the results. Only a ray that crosses this box pays for CPU skinning and triangle intersection. The deformation matches `skin.slang`, so selection follows the pose shown in the viewport without a GPU readback.

## Selection result

`pick_scene_surface` returns `SceneSurfaceHit` with the mesh entity, world-space point, and ray distance. `pick_entity` reduces that to an entity or `Entity::NULL`.

The control command performs one final ownership step: if the surface belongs to an expanded `ModelInstance` subtree, it selects the model root. The hierarchy therefore treats an imported model as one editor object even when the ray struck a nested mesh node.

| Click target | `PickResult.kind` | Selection |
|---|---|---|
| Light or camera glyph | `billboard` | Glyph entity |
| Static or skinned surface | `mesh` | Model root, or hit entity outside a model |
| Empty viewport | absent | Cleared |

## Source map

| What | File | Symbols |
|---|---|---|
| Ray construction and surface pick | `engine/crates/assets/src/render_scene.rs` | `viewport_ray`, `pick_scene_surface`, `pick_entity` |
| Static-mesh hierarchy cache | `engine/crates/assets/src/load.rs` | `AssetServer::mesh_pick_bvh` |
| Bounds and intersection math | `engine/crates/geometry/src/picking.rs` | `MeshBvh`, `ray_aabb_slab`, `ray_triangle`, `world_aabb_from_corners` |
| Joint palette | `engine/crates/scene/src/hierarchy.rs` | `Scene::joint_matrices` |
| Billboard priority and selection | `engine/crates/control/src/commands_scene.rs` | `pick_billboard`, `pick` |

## Related

- [Transforms](../transform-and-matrices/)
- [Selection](../../ui-and-editor/selection/)
- [Editor camera](../../ui-and-editor/editor-camera/)
