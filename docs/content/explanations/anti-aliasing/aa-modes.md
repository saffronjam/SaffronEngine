+++
title = 'AA modes'
weight = 3
+++

# AA modes

Anti-aliasing reduces the jagged edges that appear where a triangle's boundary crosses a pixel grid.
A renderer can address it at different points in the pipeline, and each point is a distinct mode with
its own cost and quality.

Anima offers three approaches and treats them as mutually exclusive: at most one is active at a
time. A single configuration call selects the active mode, and a CLI command fronts it for scripting
and inspection. The host starts with anti-aliasing off (1×); a loaded
project's saved [render settings](../../geometry-and-assets/project-serialization/) override it.

The editor gizmo overlay is unaffected by the active mode — it draws after the resolve and
[anti-aliases itself analytically](../../ui-and-editor/gizmo/).

## The modes

The three approaches differ in where they sample and how they combine:

| Mode | `set-aa` arg | What it is | Where |
|---|---|---|---|
| Off | `off` | no anti-aliasing | — |
| MSAA 2× / 4× / 8× | `msaa2` / `msaa4` / `msaa8` | multisampled scene color + depth, resolved into the offscreen | [MSAA](../msaa/) |
| FXAA | `fxaa` | luma-edge blur, one compute pass on a 1× scratch → offscreen | [FXAA](../fxaa/) |
| TAA | `taa` | jittered sampling accumulated over time: dilated reproject, Catmull-Rom history, YCoCg variance clip, velocity-adaptive blend, optional sharpen | [TAA](../../screen-space-and-post/taa/) |

## Selecting a mode

`Aa::set(msaa_samples, fxaa, taa)` takes a sample count plus FXAA and TAA flags and resolves
them into one active mode. MSAA wins if a sample count of 2 or more is requested; otherwise FXAA,
then TAA. The count maps to a `vk::SampleCountFlags` value and clamps to the largest count the
color and depth formats both support, so asking for `msaa8` on hardware that tops out at 4× yields
4×.

The `sa set-aa` command parses a mode string into those three arguments. `Aa::set_mode` does the
same folding from a name:

| String | `msaa_samples` | `fxaa` | `taa` |
|---|---|---|---|
| `off` | 1 | false | false |
| `msaa2` / `msaa4` / `msaa8` | 2 / 4 / 8 | false | false |
| `fxaa` | 1 | true | false |
| `taa` | 1 | false | true |

`Aa` is the single selector that holds the AA state: only `Aa::set` mutates it, and it never leaves
more than one mode active. That keeps the three modes from contradicting — there is one AA state, not
three independent toggles.

## What a switch does

Switching modes is a full reconfigure, not a flag flip. `Renderer::set_aa` waits for the GPU to go
idle, since the targets and pipeline state objects are about to be destroyed. It stores the new
sample count and flags through `Aa::set`, then rebuilds each view's AA targets
(`ViewTarget::build_aa_targets`): the multisampled MSAA pair, the 1× scratch that FXAA and TAA
share, and the TAA motion + motion-depth + ping-pong history set.

`Aa::set` returns `true` when the *MSAA sample count* changed. On that signal the renderer clears the
sample-count-baked PSO cache via `Pipelines::set_sample_count` and rebuilds the depth-prepass
pipeline, because the mesh and prepass PSOs bake the sample count.

## Reading back the active mode

`Aa::mode` (surfaced by `Renderer::aa_mode`) reports the current mode as a string, and
`sa render-stats` exposes it as the `aa` field. FXAA and TAA report by their flag; otherwise the
sample count decides — `"off"` at 1, `"msaaN"` above.

## In the code

| What | File | Symbols |
|---|---|---|
| Selector + clamp + exclusivity | `aa.rs` | `Aa::set`, `Aa::set_mode`, `Aa::mode`, `clamp_sample_count` |
| Reconfigure + PSO rebuild | `renderer.rs` | `Renderer::set_aa`, `Renderer::set_aa_mode`, `Renderer::aa_mode` |
| Target rebuild | `view_target.rs` | `ViewTarget::build_aa_targets` |
| Sample-count PSO cache | `pipelines.rs` | `Pipelines::set_sample_count` |
| CLI front | `commands_render.rs` | `set-aa`, `render-stats`, `aa_mode_from_name` |
| Upscale command (TAAU) | `commands_render.rs` | `get-upscale`, `set-upscale` (partial update) |

Under TAA the resolve doubles as a temporal upsampler: the scene renders sub-native and the resolve
reconstructs a sharp display image (`sa set-upscale '{"ratio":0.67}'`, a partial update). See
[TAA → Temporal upsampling](../../screen-space-and-post/taa/#temporal-upsampling-taau).

## Related

- [MSAA](../msaa/) — the rasterization-time mode
- [FXAA](../fxaa/) — the cheap post-process mode
- [TAA](../../screen-space-and-post/taa/) — the temporal mode
