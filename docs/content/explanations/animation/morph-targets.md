+++
title = 'Morph targets'
weight = 6
math = true
+++

# Morph targets

A morph target, also called a blend shape, is a stored per-vertex offset from a mesh's base pose.
Driving a weight from 0 to 1 slides the mesh toward that offset, and several targets blended together
produce facial expressions and corrective shapes a skeleton cannot express. Anima imports morph
targets from [glTF](https://github.com/KhronosGroup/glTF/blob/main/specification/2.0/Specification.adoc),
stores them sparsely, and applies them in a compute pass before skinning, so a morphed mesh flows
through the rest of the frame as an ordinary deformed vertex stream.

## Two weight vectors

The durable weights live in `MorphComponent { weights, names }` on the mesh-bearing entity, seeded at
spawn from the asset's META `morph` block: one canonical `0..1` weight per target (the authored rest
weights, else zeros) and one name per target. Import preserves the glTF exporter convention
`mesh.extras.targetNames` in channel order and uses `morph_{k}` only for targets without an authored
name. Like `SkinnedMesh`, the component is import-managed identity; the editor treats it as neither
addable nor removable, and its length must match the mesh's target count.

Animation writes elsewhere. A weight curve is an
[`AnimTrack`](../animation-data-model/) with `path = Weights` carrying `morph_count`
weights per keyframe, sampled by the same evaluator that drives bone and node tracks. The evaluator
writes the runtime-only `MorphWeightOverride`, never the durable component, and removes the override
when the rig stops animating, so the mesh reverts to its rest weights. The GPU deform reads the
override when present, else the durable weights.

## Sparse storage

Most vertices do not move for most targets, so a dense per-target copy of the mesh would be almost
all zeros. Each target stores only the vertices it perturbs:

```rust
#[repr(C)]
pub struct MorphDelta {
    pub vertex_index: u32,  // index into the base vertex stream
    pub d_position: Vec3,   // position delta at weight 1.0
    pub d_normal: Vec3,     // normal delta at weight 1.0
}                           // exactly 28 bytes: the .smesh and GPU stride
```

Import drops a delta whose position and normal offsets both fall below `MORPH_DELTA_EPSILON_SQ`
(squared length `1e-12`), so a target keeps only the vertices it genuinely moves. No tangent delta is
stored: the deform kernel copies the base `Vertex` tangent through unchanged. A `.smesh` carries its
targets in an optional morph section behind the `MESH_FLAG_MORPH` header flag (see
[the `.smesh` format](../../geometry-and-assets/smesh-format/)).

## Deform: fixed-point atomic scatter

Each frame a compute pass writes $\text{base} + \sum_i w_i \, \delta_i$ into the shared deformed
vertex buffer that [compute skinning](../../frame-and-render-graph/compute-skinning/#the-deformed-buffer) owns.
`morph.slang` is one kernel dispatched three times per instance, selected by `push.pass`:

1. **Clear** — one thread per vertex zeroes a per-vertex accumulator of six `i32` lanes
   (position xyz + normal xyz).
2. **Scatter** — one thread per active delta quantizes $w \cdot \delta$ by `MORPH_FIXED_SCALE`
   (65536) and `InterlockedAdd`s it into the owning vertex's lanes.
3. **Resolve** — one thread per vertex dequantizes, adds the base vertex, renormalizes the normal,
   copies `uv0` and the tangent through, and stores the 48-byte `Vertex` at the instance's offset in
   the deformed buffer.

The fixed-point detour exists because integer atomics commute: the accumulated sum is bit-identical
regardless of GPU thread order, which a floating-point atomic add cannot guarantee. A golden unit
test mirrors the quantized math on the CPU, and the llvmpipe fallback GPU reproduces it exactly.

The CPU compacts the frame's active targets, those whose weight magnitude clears
`MORPH_WEIGHT_THRESHOLD` (`1e-3`), into one flat list shared by every dispatch, so a rest-pose morph
mesh dispatches nothing. An unskinned morph mesh draws its deformed slice as a static vertex stream.
A skinned one has the skin pass read and overwrite the same slice in place, so skinning deforms the
morphed base.

> [!NOTE]
> Morph and skin both declare the deformed buffer `StorageWriteCompute`, so the render graph orders
> morph before skin as a write-after-write dependency — the data dependency is the ordering, with no
> hand-placed barrier.

## Motion vectors and ray tracing

A second dispatch per instance runs the identical kernel with the previous frame's weights into the
prev-deformed buffer, the morph counterpart of the skin prev-pose path. The
[motion pass](../../screen-space-and-post/motion-vectors/) therefore reprojects real deformation motion, not just
object motion. `swap_morph_weights` caches each entity's weights across frames; an uncached entity
gets prev equal to cur, which reads as zero deformation motion on its first frame.

The ray-traced BLAS refits over the post-morph slice each frame. An unskinned morph instance enters
the TLAS at its node world matrix, since its deformed vertices stay mesh-local. A skinned-morph
instance rides the skinned RT path at identity because skinned deformed vertices are already
world-space.

## Driving weights

`set-morph-weights` validates the vector length against the target count, then writes the runtime
override when a live animation owns one (so the change shows at once), else the durable component.
`get-morph-weights` returns the live weights plus the target names. Weights are canonical `0..1` on
every surface. From Luau, `Entity:set_morph_weights({...})` reaches the same write seam, and the
Inspector renders one slider per target, labelled by the durable names and coalesced to one command
per edit burst.

```sh
sa set-morph-weights --entity <uuid> --weights '[0.25, 0.75]'
sa get-morph-weights --entity <uuid>
# → { "weights": [0.25, 0.75], "names": ["morph_0", "morph_1"] }
```

## In the code

| What | File | Symbols |
|---|---|---|
| Sparse delta + CPU aggregates | `geometry/src/types.rs` | `MorphDelta`, `MorphTarget`, `MorphData` |
| glTF decode + name/rest-weight metadata | `geometry/src/gltf_import.rs` | `finalize_morph`, `MORPH_DELTA_EPSILON_SQ` |
| Component seeding at spawn | `assets/src/spawn.rs` | `seed_morph`, `ModelSpawnInput` |
| Durable + runtime weights | `scene/src/component.rs` | `MorphComponent`, `MorphWeightOverride` |
| Evaluator → override | `animation/src/runtime.rs`, `animation/src/sample.rs` | `tick_node_rig`, `sample_weights` |
| Weight source per frame | `assets/src/render_scene.rs` | `morph_weights_for` |
| The three-pass kernel | `assets/shaders/morph.slang` | `computeMain`, `ActiveTarget` |
| Buffers, sets + dispatch replay | `rendering/src/skinning.rs` | `record_morph`, `wire_morph_dispatches`, `swap_morph_weights`, `MORPH_FIXED_SCALE` |
| Active-target compaction + RT wiring | `rendering/src/instancing.rs` | `build_active_targets`, `MORPH_WEIGHT_THRESHOLD` |
| Control commands | `control/src/commands_animation.rs` | `set-morph-weights`, `get-morph-weights` |
| Luau seam | `script/src/entity.rs` | `set_morph_weights` |

## Related

- [Animation data model](../animation-data-model/) — the clip/track model a `Weights` track shares
- [Playback runtime](../playback-runtime/) — the evaluator that writes the weight override
- [Compute skinning](../../frame-and-render-graph/compute-skinning/) — the deformed buffer and prev-pose apparatus this rides
- [.smesh format](../../geometry-and-assets/smesh-format/) — where the sparse deltas are baked
- [Motion vectors](../../screen-space-and-post/motion-vectors/) — the consumer of the prev-deformed buffer
