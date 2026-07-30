+++
title = 'Environment and presentation panels'
weight = 13
+++

# Environment and presentation panels

Scene-wide visual authoring is split by intent. Environment describes the world around the scene,
Render controls how much work the renderer performs, and Post controls how the finished image is
mapped and styled. The three panels share the full-height right dock, while the Inspector stays
beneath Hierarchy on the left. This gives scene-wide controls enough vertical space without making
them compete with entity editing.

## Environment authoring

The Environment panel groups authored state into Sky, Time, Weather, and Fog. Each tab contains
collapsible property sections. Essential detail shows the controls used for ordinary scene work;
All reveals the complete environment model. Search ignores the current tab and opens every matching
section, which makes an advanced field reachable without remembering its category.

Section headers show a dot when their current value differs from the engine's canonical default.
Reset restores that section as one undoable edit. Resetting artistic atmosphere, cloud, or fog state
preserves the sampling and temporal-quality fields owned by Render.

Time-of-day appearance curves open in a wide dialog instead of sharing the narrow property column.
Exposure, sky tint, cloud coverage, and cloud type each get a full-width curve editor. The panel
remembers its selected tab, detail level, and expanded sections in local editor state.

## Profiles and panel ownership

An environment profile is a complete `SceneEnvironment`, not a partial preset. Built-in profiles and
project `.senv` assets appear in one selector. Applying one replaces the active environment in a
single command and creates one undo entry. Saving creates a catalog asset; Update writes the active
environment back to the selected project profile while preserving its asset id.

Render owns environment quality: atmosphere transmittance and capture cadence, cloud march counts
and temporal reuse, and volumetric-fog grid and history controls. Post is divided into Tone, Color,
and Effects. Tone owns exposure and the view transform, Color owns grading and creative LUTs, and
Effects owns bloom. A setting appears in only one panel.

## Example

Choose Clear day from the Environment profile selector, open Weather, and raise cloud coverage. Save
the result as "Summer clouds" to reuse the complete setup in the project. The same flow is available
through the control plane:

```sh
sa apply-environment-profile --profile '{"kind":"builtin","profile":"clear-day"}'
sa set-clouds --coverage 0.45
sa save-environment-profile --name 'Summer clouds'
```

## In the code

| What | File | Symbols |
|---|---|---|
| Environment categories, search, reset, and profiles | `editor/src/panels/EnvironmentPanel.tsx` | `EnvironmentPanel`, `showGroup`, `applyProfile` |
| Progressive-disclosure section | `editor/src/components/PropertySection.tsx` | `PropertySection` |
| Quality ownership | `editor/src/panels/RenderPanel.tsx` | `RenderPanel`, `patchEnvironmentQuality` |
| Tone, Color, and Effects | `editor/src/panels/PostProcessPanel.tsx` | `PostProcessPanel`, `onTonemap`, `writeGrade`, `writeBloom` |
| Dock defaults | `editor/src/state/dockLayout.ts` | `defaultSceneLayout`, `DEFAULT_LEAF` |
| Profile storage | `engine/crates/assets/src/environment_profile.rs` | `builtin_environment_profiles`, `save_environment_profile`, `update_environment_profile` |
| Profile commands | `engine/crates/control/src/commands_scene/` | `list-environment-profiles`, `apply-environment-profile` |

## Related

- [Dock system](../dock-system/) — panel placement, persistence, and layout-version validation
- [Time of day](../../image-based-lighting/time-of-day/) — clock, ephemerides, and appearance curves
- [Procedural atmosphere](../../image-based-lighting/procedural-atmosphere/) — the physical sky model
- [Color grading](../../screen-space-and-post/color-grading/) — the Post panel's Color controls
- [Render quality tiers](../../screen-space-and-post/render-quality-tiers/) — renderer-wide quality presets
