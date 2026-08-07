+++
title = 'Toggle render features'
weight = 7
math = false
+++

# Toggle render features

Switch renderer features on and off at runtime with the `sa set-*` commands.

## Steps

Each toggle takes `0|1` (or `off`/`on`); a few take a mode string instead. With the editor running:

```sh
sa set-aa msaa4              # off | fxaa | taa | msaa2 | msaa4 | msaa8
sa set-clustered 1          # clustered light culling vs brute-force loop
sa set-depth-prepass 1      # vertex-only depth pre-pass before the scene pass
```

The other toggles share the same shape:

```sh
sa set-shadows 1            # directional shadow map
sa set-ibl 1                # image-based ambient vs flat ambient
sa set-ssao 1               # screen-space ambient occlusion (GTAO)
sa set-exposure 1.5         # tonemap exposure in EV stops (engine raises 2^EV)
sa set-gi ddgi              # off | ddgi (probe global illumination)
```

`set-clustered 0` falls back to a brute-force per-pixel light loop (pixel-identical to the clustered path).

## View modes

`set-view-mode` swaps the whole viewport to a debug output — a flat shading mode, a single G-buffer
channel, or an analysis heatmap. It is transient (never persisted with the project) and reads back
through `render-stats` (`viewMode`):

```sh
sa set-view-mode lit               # full PBR shading (the default)
sa set-view-mode unlit             # albedo + emissive, no lighting
sa set-view-mode wireframe         # edges only (PolygonMode::LINE)
sa set-view-mode lit-wireframe     # shaded scene with wireframe edges overlaid
sa set-view-mode detail-lighting   # lighting on a neutral-grey material (normals kept)
sa set-view-mode lighting-only     # lighting on a flat white diffuse material
sa set-view-mode reflections       # IBL specular reflection only (mirror-like)
sa set-view-mode albedo            # per-channel buffers: albedo | normal | roughness |
sa set-view-mode normal            #   metallic | emissive | depth | ambient-occlusion | gi |
sa set-view-mode motion-vectors    #   motion-vectors
sa set-view-mode light-complexity  # point/spot lights reaching each pixel, as a heatmap
```

In the editor, the same set is the **View Modes** dropdown on the toolbar (left of the Tools
button) — a radio group with the per-channel buffers under a "Buffer Visualization" submenu. The
button's icon and label track the active mode.

`light-complexity` counts only point/spot lights (directional lights are not clustered), so a
pixel reached by none reads near-black; the ramp warms toward red around eight overlapping lights.
`motion-vectors` needs the motion target, which only exists with TAA or SSGI on (`set-aa taa` /
`set-ssgi 1`); otherwise that mode shows the shaded scene. `lit-wireframe` and `wireframe` need a
device with `fillModeNonSolid` (llvmpipe and real GPUs have it).

## Verify

- Read the live flags: `sa render-stats` reports `aa`, `clustered`, `depthPrepass`, `shadows`, `ibl`, `ssao`, `exposureEv`, and more.
- Capture before and after for a visual diff:
  ```sh
  sa set-aa off   && sa screenshot viewport /tmp/aa-off.png
  sa set-aa msaa4 && sa screenshot viewport /tmp/aa-msaa4.png
  ```

## In the code

| What | File | Symbols |
|---|---|---|
| AA / clustered / depth-prepass / exposure | `engine/crates/control/src/commands_render/` | `set-aa`, `set-clustered`, `set-depth-prepass`, `set-exposure` |
| Shadows / IBL / SSAO / GI | `engine/crates/control/src/commands_render/` | `set-shadows`, `set-ibl`, `set-ssao`, `set-gi` |
| View modes (enum + channel map) | `engine/crates/rendering/src/renderer/` | `ViewMode`, `ViewMode::debug_channel`, `set_view_mode` |
| View-mode shading paths | `engine/assets/shaders/lighting.slang` | `evalViewMode`, `heatmap`, `evalLighting` |
| Lit-wireframe / motion-vector passes | `engine/crates/rendering/src/renderer/` | `add_lit_wireframe_pass`, `add_motion_visualize_pass` |
| Toolbar View Modes dropdown | `editor/src/panels/Topbar.tsx`, `editor/src/lib/view-modes.ts` | `ViewModeMenu`, `VIEW_MODES` |
| Live flag readout | `engine/crates/control/src/commands_render/` | `render-stats` (`aaMode`, `clusteredEnabled`, …) |

## Related

- [MSAA](../../explanations/anti-aliasing/msaa/) and [FXAA](../../explanations/anti-aliasing/fxaa/)
- [Clustered forward lighting](../../explanations/lighting-and-brdf/clustered-forward/)
- [Tonemapping and exposure](../../explanations/screen-space-and-post/tonemap-and-exposure/)
