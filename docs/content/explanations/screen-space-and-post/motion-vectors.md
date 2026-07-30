+++
title = 'Motion vectors'
weight = 5
math = true
+++

# Motion vectors

A motion vector maps a visible surface point from its current screen coordinate to the coordinate
where that point appeared in the previous frame. Temporal passes add this offset to the current UV to
find corresponding history. Camera movement, entity transforms, and vertex deformation all
contribute to the result.

Anima stores `previousUv - currentUv` in a single-sampled `rg16f` image at the scene's input render
extent. The values remain normalized UV offsets, so a display-resolution temporal resolve can sample
the input-resolution buffer without converting pixel units.

## Reprojection

The motion pass replays the frame's GPU-binned counted-indirect commands as a graphics prepass with
its own depth attachment. Its vertex shader, `vertexMainExecutor`, computes two clip positions for
each surface point:

$$
c_\text{cur}=P_\text{cur}M_\text{cur}x_\text{cur}, \qquad
c_\text{prev}=P_\text{prev}M_\text{prev}x_\text{prev}.
$$

$P$ is the camera view-projection matrix, $M$ is the instance model matrix, and $x$ is the vertex
position, pulled through buffer device address. The fragment shader performs the perspective divides
and writes

$$
v_\text{uv}=\frac{1}{2}
\left(\frac{c_{\text{prev},xy}}{c_{\text{prev},w}}-
      \frac{c_{\text{cur},xy}}{c_{\text{cur},w}}\right).
$$

The factor one half converts a normalized-device-coordinate delta from the $[-1,1]$ range to UV
space. Both matrices use the renderer's Y-flipped projection, so the stored Y direction matches the
images sampled by temporal shaders. Consumers reproject with `historyUv = uv + motion`.

## Three motion sources

Camera motion comes from the current and previous view-projection matrices. These matrices are stored
per render view, so switching between the scene and asset-preview views does not mix their histories.
The motion pass uses unjittered matrices; subpixel TAA jitter therefore does not create velocity on a
static surface. On a view's first frame, previous equals current and camera velocity is zero.

Rigid object motion comes from the GPU-scene instance record, whose `transform_kind` selects the
stored columns: a dynamic instance carries current and previous world matrices, while a static
instance stores only current columns and reprojects with zero object motion. A new entity uses its
current transform as the previous one, avoiding an artificial first-frame vector.

Deformation motion comes from the GPU-scene address block: the vertex shader reads this frame's
position from the `deformedVertices` arena and last frame's from `prevDeformedVertices`, while a mesh
with no deformation reads the same static stream for both. A displaced instance reads the same
micro-vertex slot in the amplification arena's current and previous-factor streams. This
lets bone motion, morph changes, and tessellation geomorphing produce per-pixel velocity rather than
only whole-object motion.

## Depth and consumers

The prepass clears an `rg16f` color attachment and a single-sampled D32 depth attachment, then draws
with depth test and write enabled. Its depth is independent of the main scene depth, whose sample
count and extent depend on the active anti-aliasing and render-scale configuration.

[TAA](../taa/) samples both images. It searches a 3x3 input-pixel neighborhood in `motion_depth` and
uses the motion vector at the nearest depth. This velocity dilation extends foreground motion across
silhouette pixels before history reprojection. The search chooses a stored vector; it does not alter
the vector value.

SSGI and DFAO temporal accumulation sample the motion image directly, reject invalid or mismatched
history, and write their next histories. ReSTIR reservoir reuse also declares a sampled read when the
motion prepass is present. The renderer schedules motion when TAA, SSGI, DFAO, or the volumetric-cloud
reprojection requires it; ReSTIR by itself does not arm the prepass.

## Example

Enable TAA to guarantee the motion prepass, then display the vector buffer:

```sh
sa set-aa taa
sa set-view-mode --mode motion-vectors
```

The visualization maps vector direction to hue and multiplies UV magnitude by 40 for brightness.
Static regions remain black. Restore the normal viewport with `sa set-view-mode --mode lit`.

## In the code

| What | File | Symbols |
|---|---|---|
| Reprojection shader | `engine/assets/shaders/motion.slang`, `engine/assets/shaders/global_gpu_data.slang` | `vertexMainExecutor`, `fragmentMain`, `Push`, `prevDeformedVertices` |
| Format, push, and draw recording | `engine/crates/rendering/src/aa.rs`, `engine/crates/rendering/src/scene_pass.rs` | `MOTION_FORMAT`, `MotionPush`, `record_executor_depth_family` |
| Graph pass and gates | `engine/crates/rendering/src/renderer/` | `add_motion_pass`, `want_motion`, `motion_depth_resource` |
| Per-view targets and camera history | `engine/crates/rendering/src/view_target/` | `motion`, `motion_depth`, `prev_view_proj`, `store_prev_view_proj` |
| Object and deformation history | `engine/crates/rendering/src/instancing.rs`, `engine/crates/rendering/src/skinning/`, `engine/crates/rendering/src/renderer/` | `DeformationWork`, `submit_gpu_scene_deformations`, `Skinning::prev_model`, `Skinning::prev_deformed_buffer` |
| Tessellation history | `engine/crates/rendering/src/renderer/`, `engine/crates/rendering/src/tessellation.rs`, `engine/crates/rendering/src/draw_list.rs` | `prev_factors`, `Tessellation::factor_layout_matches`, `TessDraw::prev_vertex_buffer` |
| Temporal consumers | `engine/assets/shaders/taa.slang`, `engine/assets/shaders/ssgi_accum.slang`, `engine/assets/shaders/dfao_accum.slang`, `engine/assets/shaders/restir_reuse.slang` | `DilatedMotion`, `histUv`, `motion` |
| Debug visualization | `engine/assets/shaders/motion_visualize.slang`, `engine/crates/rendering/src/renderer/` | `computeMain`, `add_motion_visualize_pass` |

## Related

- [TAA](../taa/): dilates velocity and reprojects display-resolution history
- [SSGI](../ssgi/): reprojects its radiance history through the same buffer
- [Compute skinning](../../frame-and-render-graph/compute-skinning/): produces current and previous deformed positions
- [Thin G-buffer](../thin-gbuffer/): a sibling geometry prepass for screen-space effects
