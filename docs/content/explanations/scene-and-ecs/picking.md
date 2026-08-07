+++
title = 'Picking'
weight = 7
+++

# Picking

Picking maps a viewport coordinate to the visible object beneath it. Anima tests editor billboards first, then replays the frame's own drawn geometry into a one-texel selection target and reads the identity back. A hit selects the nearest object; a miss clears the selection.

The point of resolving on the GPU is agreement: the answer comes from the same binned cut, the same vertex path, and the same coverage test that produced the image on screen. A click on a leaf that the alpha cutout removed selects whatever stands behind it, a plant deformed by wind selects where the wind put it, and geometry that only the GPU knows how to build — reconstructed grass blades, amplified displacement, aggregate voxels — is selectable without a CPU counterpart for any of it.

## The selection pass

`pick` receives viewport UV coordinates with `(0, 0)` at the top left. It converts them to a pixel in the view's render extent and hands that to `Renderer::pick_selection_id`.

The renderer keeps, per view, what the last frame's graph left behind: the executor draw buckets, the indirect command stream and its counts, the descriptor set the records live in, and the camera they were binned for. A pick replays that state through one selection pipeline into three one-texel attachments — a `R32G32B32A32_UINT` identity, and world position and normal as floats — plus a private depth so the nearest surface wins.

One texel is enough because the viewport is translated rather than shrunk:

```rust
let viewport = vk::Viewport {
    x: -(pixel.0 as f32),
    y: -(pixel.1 as f32),
    width: source.extent.width as f32,
    height: source.extent.height as f32,
    min_depth: 0.0,
    max_depth: 1.0,
};
```

The scale is unchanged, so screen-space derivatives — and the stochastic coverage threshold they feed — are the ones the full-resolution frame used. Only the picked pixel lands inside the render area; everything else is scissored away. The work is one out-of-band submit on a click, never a per-frame cost, and the targets are 48 bytes.

The fragment writes the emitting draw record's representation, its GPU-scene instance slot, its content index, and its assembly use, after repeating the depth prepass's coverage test exactly.

## Resolving an identity

A GPU-scene slot is a device address, not an identity, and it never reaches the wire. The host translates it through the scene mirror, which is keyed by identity in both directions:

| Record | Mirror answer | `PickResult.kind` |
|---|---|---|
| Instance mirrored from an entity | `MirrorInstanceIdentity::Entity` | `mesh` |
| Instance mirrored from a resident plant | `MirrorInstanceIdentity::Plant` — cell plus `PlantId` | `vegetation` |
| Micro-blade record, or a micro field anchor | `MirrorInstanceIdentity::MicroField` | `micro-vegetation` |

A micro blade is regenerated from its field every frame and is deliberately identity-less: the result carries a world position for paint feedback and nothing that could be mistaken for a saved object.

The control command performs one final ownership step: if the picked entity belongs to an expanded `ModelInstance` subtree, it selects the model root. The hierarchy therefore treats an imported model as one editor object even when the click landed on a nested mesh node.

## Editor billboards

Point lights, spot lights, and cameras can be visible in the editor without a mesh. They are overlay glyphs, so no draw record exists for them and the selection target cannot answer for them. `pick_billboard` projects their world positions to viewport pixels and tests a 26-pixel square centred on each glyph, before the selection replay runs.

A billboard hit therefore wins over geometry, which is what makes a small editor control clickable with a wall behind it.

## The surface ray beside it

Picking no longer casts a CPU ray, but the [surface-field contract](../spatial-world/) it used to go through is still the engine's way of asking *where a ray meets the world* rather than *what is drawn at a pixel*. `query_scene_surface_ray` walks each mesh provider's cached bounding-volume hierarchy — CPU-skinning a skinned mesh through its current joint palette first — and returns an exact world position, tangent frame, UV, and stable triangle attachment.

That is the query behind the `query-surface-ray` command and behind asset placement, where a drop needs a point on a surface with no pixel involved. It answers about geometry the frame may never have drawn, which is exactly why it is not the picking path.

| Click target | `PickResult.kind` | Selection |
|---|---|---|
| Light or camera glyph | `billboard` | Glyph entity |
| Any drawn surface | `mesh` | Model root, or the hit entity outside a model |
| Macro plant | `vegetation` | None; the result carries the stable `plant` id |
| Micro blade | `micro-vegetation` | None; the result carries the world `position` |
| Nothing drawn there | absent | Cleared |

## Source map

| What | File | Symbols |
|---|---|---|
| Pick command and billboard priority | `engine/crates/control/src/commands_scene/spatial.rs` | `pick`, `pick_billboard` |
| Selection replay and readback | `engine/crates/rendering/src/renderer/selection_pick.rs` | `Renderer::pick_selection_id`, `capture_selection_source` |
| Readback decoding | `engine/crates/rendering/src/selection.rs` | `SelectionHit`, `SelectionReadback` |
| Selection pipeline | `engine/crates/rendering/src/pipelines/` | `Pipelines::request_selection_id`, `build_selection_id` |
| Identity fragment | `engine/assets/shaders/mesh.slang` | `selectionIdFragment`, `SelectionOutput` |
| Slot-to-identity translation | `engine/crates/assets/src/gpu_scene_mirror/mod.rs` | `GpuSceneMirror::identify_instance_slot`, `MirrorInstanceIdentity` |
| Surface ray for placement and queries | `engine/crates/assets/src/render_scene/` | `query_scene_surface_ray`, `pick_scene_surface`, `viewport_ray` |

## Related

- [Transforms](../transform-and-matrices/)
- [Selection](../../ui-and-editor/selection/)
- [Plant rendering](../../geometry-and-assets/plant-rendering/)
