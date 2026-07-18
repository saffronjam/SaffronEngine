+++
title = 'Render quality tiers'
weight = 9
+++

# Render quality tiers

A render quality tier maps one name to the enable flags and sample counts of the scalable
screen-space lighting stack. The selected tier controls [SSGI](../ssgi/), [GTAO](../gtao/), and
[contact shadows](../contact-shadows/) together.

## Presets

`QualityTier::resolve` expands each preset into a `RenderQuality` value:

| Tier | SSGI | GTAO | Contact shadows | SSGI steps | SSGI rays | Contact steps |
|---|---|---|---|---:|---:|---:|
| `low` | off | off | off | 4 | 4 | 8 |
| `medium` | on | on | off | 4 | 3 | 8 |
| `high` | on | on | on | 8 | 4 | 12 |
| `ultra` | on | on | on | 12 | 6 | 16 |

`high` is the default. `custom` identifies a hand-set `RenderQuality`; its initial resolved values
match `high`. The editor presents the four presets in its Render panel.

The renderer passes the resolved value to `Ssao::apply_quality`. Enable flags decide whether the
screen-space passes run, while SSGI and contact-shadow counts travel in runtime push constants. A tier
change therefore does not compile shaders or resize render targets.

Other renderer switches remain independent. The tier does not select clustered lighting, IBL, DDGI,
ray-traced shadows, ReSTIR, anti-aliasing, or the volumetric-fog grid quality.

## Control and persistence

`set-render-quality` applies a named tier. `get-render-quality` reports the tier and its resolved
enable flags:

```sh
$ sa set-render-quality medium
{"tier":"medium","ssgi":true,"gtao":true,"contactShadows":false}

$ sa get-render-quality
{"tier":"medium","ssgi":true,"gtao":true,"contactShadows":false}
```

An unknown name returns a command error and leaves the active tier unchanged. Project serialization
stores the name in `renderSettings.quality`, and `render-stats` reports it in `quality` alongside the
resolved `ssgi`, `ssao`, and `contactShadows` flags.

## Frame-budget controller

Dynamic resolution enables `BudgetController`, which compares each frame's work time with the target
budget. `set-upscale` owns both the toggle and the target:

```sh
sa set-upscale '{"dynamic":true,"targetMs":16.67}'
```

The controller uses these thresholds:

| Condition | Required frames | Result |
|---|---:|---|
| Work time above budget | 12 consecutive | Step down once |
| Work time below 70% of budget | 90 consecutive | Step up once |
| Work time above twice the budget | 1 | Step down immediately |
| After any step | 30 | Hold during cooldown |

The automatic tier ladder is `high -> medium -> low`; `ultra` and `custom` are outside it. Once the
tier reaches `low`, further downsteps reduce the render scale through `1.0`, `0.83`, `0.71`, `0.59`,
and `0.5`. Upsteps restore render scale before increasing the tier.

Scale changes are queued until the next frame's resize point. The input extent changes while the
display extent and its TAA history remain fixed, so [TAAU](../taa/#temporal-upsampling-taau) resolves
the scaled scene into the published display size. `get-upscale` reports the active ratio, dynamic
state, target budget, and both extents.

## In the code

| What | File | Symbols |
|---|---|---|
| Tier definitions and resolved values | `quality.rs` | `QualityTier`, `RenderQuality`, `resolve`, `from_name` |
| Apply settings to screen-space effects | `ssao.rs` | `Ssao::apply_quality` |
| Renderer state | `renderer.rs` | `set_render_quality`, `render_quality`, `set_render_scale`, `apply_render_extent` |
| Frame-budget decisions | `budget.rs` | `BudgetController`, `BudgetStep` |
| Budget configuration | `frame_history.rs` | `PerfConfig`, `PerfConfig::budget_ms`, `PerfConfig::auto_quality` |
| Quality and upscale commands | `commands_render.rs` | `set-render-quality`, `get-render-quality`, `render_quality_result`, `set-upscale`, `upscale_dto` |
| Project persistence | `render_settings.rs` | `RenderSettings::quality` |
| Editor selector | `RenderPanel.tsx` | `QUALITY_TIERS`, `onQuality` |

## Related

- [SSGI](../ssgi/) — uses the tier's ray and march-step counts
- [GTAO](../gtao/) — follows the tier's ambient-occlusion enable flag
- [Contact shadows](../contact-shadows/) — use the tier's enable flag and step count
- [Temporal anti-aliasing](../taa/) — reconstructs scaled input at display resolution
