+++
title = 'Render quality tiers'
weight = 9
+++

# Render quality tiers

A render quality tier is one named knob — `low`, `medium`, `high`, or `ultra` — that expands to the
per-effect parameters of the scalable screen-space GI stack: SSGI, GTAO, and contact shadows. It
replaces the old per-effect on/off toggles with a single source of truth, so the editor viewport can
run a cheaper tier than a shipped game and the exported game can expose the tier as its
graphics-settings slider.

The screen-space GI stack is the expensive part of a frame and its cost is **resolution-bound, not
scene-bound** — SSGI alone is the single most expensive pass. A near-empty scene pays almost the same
as a full one, so the way to make a frame cheaper is to dial the effects down, not to simplify the
scene. The tier is that dial.

## What a tier sets

Each preset resolves to a [`RenderQuality`] the renderer applies to [`Ssao`]. The parameters it
drives are the ones already carried as runtime push-constants, so changing a tier costs nothing — no
shader recompile, no target rebuild:

| Tier | SSGI | GTAO | Contact shadows | SSGI steps | SSGI rays |
|---|---|---|---|---|---|
| `low` | off | off | off | — | — |
| `medium` | on | on | off | 4 | 3 |
| `high` | on | on | on | 8 | 4 |
| `ultra` | on | on | on | 12 | 6 |

`low` disables the screen-space stack entirely (direct + image-based lighting only) — the cheapest
interactive mode, mirroring how Unreal's Low scalability disables Lumen. `high` is the engine's
historical look and the default. A `custom` tier is the escape hatch: it carries hand-set parameters
rather than a preset (resolving from `high` as its base).

> [!NOTE]
> The tier covers the **scalable** screen-space effects. Architectural on/off switches (clustered
> lighting, IBL, DDGI, RT shadows, ReSTIR) stay as their own toggles — they are capability choices,
> not quality dials. The tier dials the SSGI step count, the SSGI ray count, the contact-shadow step
> count, and the enable flags — all runtime push-constants. SSGI + GTAO additionally trace at
> half-resolution and bilateral-upsample (see [SSGI](../ssgi/) / [GTAO](../gtao/)); that runs at every
> tier rather than being a per-tier toggle.

## Driving it

One control command sets the tier and one reads it back; both return the resolved per-effect state so
a caller (the editor's Render panel, the `sa` CLI, a game settings menu) sees what the tier means:

```sh
sa set-render-quality medium   # → { tier: "medium", ssgi: true, gtao: true, contactShadows: false }
sa get-render-quality          # → the active tier + resolved flags
```

An unknown tier name is a typed error, not a silent default. The active tier is saved with the
project (the `renderSettings.quality` block) and reported in `render-stats` (the `quality` field, the
knob the `ssao`/`ssgi`/`contactShadows` telemetry bools derive from), so the editor's Render panel
shows it as a dropdown beside anti-aliasing.

## Auto-quality (frame-budget controller)

The tier can also drive itself. With **dynamic resolution** on (`set-upscale {"dynamic":true}`, off by
default), a frame-budget controller watches each frame's work time against the budget
(`targetMs`, = `1000 / target_fps`) and steps the tier to hold it: a sustained run of over-budget
frames steps **down** (cheaper GI), a sustained run with comfortable headroom steps back **up**, and a
single hitch ≥ 2× budget steps down at once. Hysteresis (consecutive-frame thresholds + a post-switch
cooldown) stops it oscillating; it never auto-selects `ultra` (a deliberate stills tier). It drives two
dials. The first is the tier. Below the `low` floor it then steps the **render scale**: the
**input**-extent targets shrink to `round(display × ratio)` while the published frame stays native, and
the temporal upsampler ([TAAU](../taa/#temporal-upsampling-taau)) reconstructs the sub-native input to
a sharp display image — far better than the filtered-linear present blit it replaced. The order is
deliberate — going down it spends tier steps first (cheaper GI is less visible than fewer pixels), and
coming back up it restores resolution before raising the tier. A scale change reallocates only the
input-extent targets at a safe frame boundary (never mid-frame) and **keeps the display-extent TAA
history valid**, so a budget step is a one-frame trace of extra reconstruction softness, not a flush.
The ratio is reported by `get-upscale` and can be pinned manually with `set-upscale {"ratio":…}` (when
dynamic resolution is on the controller owns it); the budget itself is `set-upscale`'s `targetMs`.

## In the code

| What | File | Symbols |
|---|---|---|
| Tier → parameters | `rendering/src/quality.rs` | `QualityTier`, `RenderQuality`, `resolve`, `from_name` |
| Apply to the stack | `rendering/src/ssao.rs` | `Ssao::apply_quality` (SSGI / contact step pushes) |
| Renderer knob | `rendering/src/renderer.rs` | `set_render_quality`, `render_quality` |
| Control commands | `control/src/commands_render.rs` | `set-render-quality`, `get-render-quality`, `render_quality_result` |
| Project save/load | `rendering/src/render_settings.rs` | `RenderSettings::quality` |
| Editor UI | `editor/src/panels/RenderPanel.tsx` | the Quality `Select` |
| Auto-quality controller | `rendering/src/budget.rs` | `BudgetController`; `PerfConfig::auto_quality` |
| Upscale / dynamic-resolution command | `control/src/commands_render.rs` | `get-upscale`, `set-upscale`, `upscale_dto` |

## Related

- [ssgi](../ssgi/) — the most expensive pass the tier dials
- [gtao](../gtao/) and [contact-shadows](../contact-shadows/) — the other tier-gated effects
- [Reactive loop](../../app-lifecycle-and-window/main-loop-and-run/) — idles a static viewport; the
  tier reduces the cost of the frames that *do* render
