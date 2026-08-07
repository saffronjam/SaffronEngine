+++
title = 'Debug visualization'
weight = 18
+++

# Debug visualization

Debug visualization exposes spatial bounds, lighting volumes, physics shapes, and intermediate render outputs inside the viewport. Line overlays preserve the normal shaded scene; view modes replace or augment its final output.

## World-space overlays

Nine project settings control the overlay geometry. `DebugOverlayOptions` is part of the scene-edit context and serializes into the project's `debugOverlays` object. These settings describe the editor view, so toggling them does not create an undo entry.

| Toggle | Geometry | Rendering behavior |
|---|---|---|
| Bounding Boxes | Green world AABBs for static meshes; magenta joint-union AABBs for skinned meshes; pale box or sphere bounds for fog volumes | Depth-tested and visible in Edit |
| Scene AABB | Yellow union of static and skinned mesh bounds | Matches the bounds used to fit directional shadows and DDGI |
| Light Volumes | Three range rings for a point light; a base ring and four edge lines for a spot light | Uses each light's world position, range, rotation, and outer angle |
| Grid | Analytic ground grid on the `y = 0` plane | Fullscreen render-graph pass with distance fade and depth output |
| Colliders | Box, sphere, capsule, or cook-source bounds for each `Collider` | Depth-tested in Edit and Play; hidden in an asset preview |
| Vegetation Cells | Blue wireframe boxes for the resident vegetation runtime cells | Depth-tested and visible in Edit |
| Vegetation Bounds | Per-plant conservative world bounds colored by lifecycle (green mature, yellow-green sprout/juvenile, orange senescent/dead, grey stump) | Depth-tested in Edit; caps at 4096 boxes so dense worlds stay interactive |
| Vegetation Rejections | One small marker cube per rejected candidate at its sampled world position, colored by rejection reason | Depth-tested in Edit; rows cache per manifest + resident set and cap at 4096 markers |
| Vegetation Heatmap | Thin surface-hugging tiles ramped green to red by micro density, one per occupied texel of a 16×16 fold per resident cell | Depth-tested in Edit; texels surface-cast once per manifest + resident set and cap at 4096 |
| Wind Vectors | Speed-colored arrows of the composed wind field on a camera-centred 16×16 ground grid (calm blue to storm red) | Depth-tested in Edit; heights re-cast when the snapped 2 m origin moves, velocities resampled every frame |

The mesh boxes are conservative spatial bounds. Static picking uses the same world AABB as a broad phase, then traverses the mesh BVH for an exact triangle hit. Skinned picking tests the joint-union box before CPU-skinning the vertices and testing the deformed triangles. The overlay therefore explains the broad-phase volume without implying that empty space inside it is selectable.

Collider geometry follows the physics body's position and rotation without entity scale. Solid colliders are cyan, sensors are green, and the selected collider is orange. A convex hull or triangle-mesh collider displays the source mesh bounds; ragdoll and character-controller shapes are separate runtime structures and do not appear in this overlay.

The grid is not line geometry. Its fragment shader reconstructs a world ray from the inverse view-projection matrix, intersects the ground plane, anti-aliases cell lines with `fwidth`, and writes depth. It runs after tonemapping and before the native line overlay.

## Controlling overlays

The Render panel's Debug section sends partial `set-debug-overlays` updates. Omitted fields keep their values, and the panel polls `get-debug-overlays` while open so command-line changes appear in the controls.

```sh
sa set-debug-overlays --bounds true --lightVolumes true
sa set-debug-overlays --colliders true
sa get-debug-overlays -o json
```

The final command returns all nine values:

```json
{
  "bounds": true,
  "sceneAabb": false,
  "lightVolumes": true,
  "grid": false,
  "colliders": true,
  "vegetationCells": false,
  "vegetationBounds": false,
  "vegetationRejections": false,
  "vegetationHeatmap": false,
  "windVectors": false
}
```

## View modes

The Topbar view-mode menu selects one `ViewMode` over `set-view-mode`. The choice is transient renderer state: it does not serialize into the project and does not participate in undo. `render-stats.viewMode` supplies the value shown by the menu.

| Group | Modes | Output |
|---|---|---|
| Shading | Lit, Unlit | Full PBR shading, or albedo plus emissive without lighting |
| Geometry | Wireframe, Lit Wireframe | Line rasterization alone, or line edges over the shaded scene |
| Lighting | Detail Lighting, Lighting Only, Reflections | Neutral material lighting, flat diffuse lighting, or IBL specular |
| Surface buffers | Albedo, Normal, Roughness, Metallic, Emissive | One evaluated material channel |
| Screen buffers | Depth, Ambient Occlusion, Motion Vectors | Linear view depth, screen-space AO, or colorized velocity |
| Analysis | Global Illumination, Light Complexity, Fog, Shadow Pages | Indirect light, punctual-light count, integrated volumetric fog, or virtual-shadow page residency |

Wireframe uses [`vk::PolygonMode::LINE`](https://registry.khronos.org/vulkan/specs/latest/man/html/VkPolygonMode.html) and requires the device's `fillModeNonSolid` feature. An unsupported device resolves Wireframe to the filled mesh pipeline and omits the Lit Wireframe edge pass.

Most modes use a debug-channel index packed into the light uniform buffer and interpreted by `evalViewMode`. Shadow Pages colours each surface by the directional [virtual-shadow](../../shadows-and-culling/virtual-shadow-maps/) page it samples (warm fine levels, cool coarse ones), dimming where no page is resident. Motion Vectors has a fullscreen visualization pass, Lit Wireframe adds a line-overlay pass, and Fog asks the volumetric composite pass to show integrated in-scatter and opacity. Ambient Occlusion returns white when its producing pass is disabled; Fog is meaningful when volumetric fog populates the froxel volume.

```sh
sa set-view-mode --mode light-complexity
sa render-stats -o json | jq .viewMode
# "light-complexity"
```

## In the code

| What | File | Symbols |
|---|---|---|
| Overlay state and project JSON | `engine/crates/sceneedit/src/overlay.rs` | `DebugOverlayOptions`, `debug_overlays_to_json`, `debug_overlays_from_json` |
| World-space line builders | `engine/crates/host/src/overlay/` | `build_debug_overlays`, `build_collider_overlays`, `build_scene_edit_overlay` |
| Grid pass and shader | `engine/crates/rendering/src/overlay.rs` · `engine/assets/shaders/grid.slang` | `record_grid`, `Renderer::set_show_grid` |
| View-mode state | `engine/crates/rendering/src/renderer/` | `ViewMode`, `Renderer::set_view_mode`, `ViewMode::debug_channel` |
| Surface-channel evaluation | `engine/assets/shaders/lighting.slang` | `debugViewChannel`, `evalViewMode` |
| Overlay commands | `engine/crates/control/src/commands_animation.rs` | `get-debug-overlays`, `set-debug-overlays` |
| View-mode command | `engine/crates/control/src/commands_render/` | `set-view-mode` |
| Editor controls | `editor/src/panels/RenderPanel.tsx` · `editor/src/panels/Topbar.tsx` | `DEBUG_OVERLAYS`, `ViewModeMenu` |
| View-mode registry | `editor/src/lib/view-modes.ts` | `VIEW_MODES`, `VIEW_MODE_BY_VALUE` |

## Related

- [Selection](../selection/) — exact static and skinned surface picking
- [Gizmo](../gizmo/) — the on-top transform overlay
- [Physics panel](../physics-panel/) — runtime body and contact diagnostics
- [Render graph overview](../../frame-and-render-graph/render-graph-overview/) — scheduling for grid and view-mode passes
