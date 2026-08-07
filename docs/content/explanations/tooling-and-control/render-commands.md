+++
title = 'Render commands'
weight = 4
+++

# Render commands

The render commands are control-plane commands that flip renderer feature switches and read back the
last frame's draw counters. Each wraps one `ControlRenderer` accessor and returns the resolved state,
so a script can confirm what actually took effect rather than what it requested.

## Liveness and stats

| Command | Params | Effect |
|---|---|---|
| `ping` | — | Returns `{pong, engine, version, pid}`. Liveness check and engine info. |
| `help` | — | Lists every registered command with its one-line help (registry order). |
| `render-stats` | — | Returns last frame's scene counters plus the state of every render feature. |

`render-stats` is the broad read (`RenderStatsDto`). It returns draw calls, batches, instances,
triangles, descriptor binds, frame timing (`frameMs`, `fps`, `cpuFrameMs`, `gpuFrameMs`, `cpuWaitMs`),
VRAM usage/budget, and a flag for each toggleable feature: `clustered`, `depthPrepass`, `shadows`,
`ibl`, `ssao`, `contactShadows`, `ssgi`, `ddgi`, `rtSupported`, `rtShadows`, `restir`, `blasCount`,
`pipelines`, `hdr`, `exposureEv`, `aa`, and `viewMode`. `frameMs`/`fps` smooth the CPU run-loop delta;
`gpuFrameMs` is the GPU frame time from a timestamp-query ring (`0` when the queue exposes no valid
timestamp bits). `softwareGpu` flags the Mesa software rasterizer.

A parallel profiler group (`profiler.set-mode`, `pass-timings`, `profiler.capture-start`/`-stop`,
`frame-history`, `get-perf-config`, `drain-alarms`) reads deeper GPU telemetry; the toggles below are
the feature switches.

## Feature toggles

Most toggles take `ToggleParams { enabled }` — a `{0|1}` (also `true`/`false`, defaulting to `true`)
— and report back the boolean. A few take an enum string instead.

| Command | Params | Effect |
|---|---|---|
| `set-aa` | `{off\|fxaa\|taa\|msaa2\|msaa4\|msaa8}` | Anti-aliasing mode. Decodes to sample count + fxaa/taa flags. |
| `set-view-mode` | `{lit\|wireframe\|albedo\|normal\|roughness\|metallic\|emissive}` | Debug render-output mode ([debug visualization](../../ui-and-editor/debug-visualization/)). Read back via `render-stats.viewMode`. |
| `set-clustered` | `{0\|1}` | Clustered (Forward+) light culling vs. a brute-force loop. |
| `set-ibl` | `{0\|1}` | Image-based ambient vs. flat ambient. |
| `set-ssao` | `{0\|1}` | Screen-space ambient occlusion (GTAO). |
| `set-contact-shadows` | `{0\|1}` | Screen-space contact shadows. |
| `set-ssgi` | `{0\|1}` | Screen-space one-bounce global illumination. |
| `set-gi` | `{off\|ddgi}` | DDGI probe global illumination (multi-bounce). |
| `set-shadows` | `{0\|1}` | The directional shadow map. |
| `set-skinning` | `{0\|1}` | The GPU compute-skinning path. |
| `set-rt-shadows` | `{0\|1}` | Hardware ray-query shadows. Errors unless `rtSupported`. |
| `set-restir` | `{0\|1}` | ReSTIR stochastic many-light direct. Errors unless `rtSupported`. |
| `set-exposure` | `{ev}` | Tonemap exposure in stops; the renderer raises 2 to it. |
| `set-depth-prepass` | `{0\|1}` | The vertex-only depth pre-pass. |
| `set-viewport-size` | `{width, height}` | The active view's offscreen render size. |

## The toggle shape

Each boolean toggle reads `params.enabled` (the CLI's token coercion already maps `1`/`0`/`true`/
`false` into a JSON bool, and the typed DTO deserialize validates it), calls the renderer setter
through the `ControlRenderer` seam, and reads the state straight back.

```rust
ctx.renderer.set_ssao(params.enabled.unwrap_or(true));
Ok(SetSsaoResult { ssao: ctx.renderer.ssao_enabled() })
```

Returning the queried state rather than the requested value is deliberate. A feature the hardware
cannot provide reports its real result. Ray-query shadows and ReSTIR both gate on
`ControlRenderer::rt_supported` up front and error if ray tracing is unavailable, so a script never
falsely believes it turned RT on. `set-aa` is the one enum toggle: it maps the mode string to a sample
count plus fxaa/taa flags, rejects anything else, and returns the renderer's canonical `aa` mode.

A new render feature ships its `ControlRenderer` query/toggle pair *and* a matching command, so the
editor stays drivable and visually debuggable via a [screenshot](../screenshots-and-capture/) after
the toggle. `render-stats` is where each new flag surfaces.

## In the code

| What | File | Symbols |
|---|---|---|
| Registration | `engine/crates/control/src/commands_render/` | `register_render_commands` |
| Stats read-back | `engine/crates/control/src/commands_render/` | `render_stats_dto`; the `*_enabled` accessors on `ControlRenderer` |
| AA enum decode | `engine/crates/control/src/commands_render/` | `aa_mode_from_name`; `ControlRenderer::set_aa`, `aa_mode` |
| RT gating | `engine/crates/control/src/commands_render/` | the `set-rt-shadows`/`set-restir` rows; `ControlRenderer::rt_supported` |
| Exposure | `engine/crates/control/src/commands_render/` | the `set-exposure` row; `ControlRenderer::set_exposure`, `exposure_ev` |
| Renderer seam | `engine/crates/control/src/registry.rs` | the `ControlRenderer` trait |
| Stats DTO | `engine/crates/protocol/src/dto/` | `RenderStatsDto` |

## Related
- [Tonemapping and exposure](../../screen-space-and-post/tonemap-and-exposure/) — what `set-exposure` drives
- [Capture](../screenshots-and-capture/) — confirm a toggle visually
- [Control plane](../control-plane-architecture/) — registration and dispatch
