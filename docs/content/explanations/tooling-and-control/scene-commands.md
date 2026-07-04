+++
title = 'Scene commands'
weight = 3
+++

# Scene commands

The scene commands are the control-plane verbs that list, create, and edit entities in the running
editor's scene. Each edit routes through the target component's registered serialize/deserialize, so
the wire shape is identical to a scene file. There is no separate code path for editing from the CLI.

Most commands take an `{entity}` argument. `resolve_entity` accepts a UUID (number or numeric string)
or a name, and tries the UUID first because it is stable across reloads and resolves against the
**active** scene, so it finds runtime entities during play. A miss returns an error, not a null
entity.

## The commands

| Command | Params | Effect |
|---|---|---|
| `list-entities` | — | Returns every entity as `{id, name, parentId}`. |
| `list-components` | — | Returns the names of all registered component types. |
| `create-entity` | `{name?}` | Creates an entity (default name `Entity`); returns its `{id, name}`. |
| `destroy-entity` | `{entity}` | Destroys the entity; clears selection if it was selected. Returns `{destroyed: id}`. |
| `set-parent` | `{entity, parent?}` | Reparents the entity; an absent or `0` parent detaches it to root. |
| `add-component` | `{entity, component}` | Adds the named component with its default value. Errors if unknown or already present. |
| `remove-component` | `{entity, component}` | Removes the named component. Errors if unknown or marked non-removable. |
| `set-component` | `{entity, component, json}` | Applies a serialized component body via the registry's deserialize. |
| `set-component-order` | `{entity, components}` | Reorders the entity's components (inspector ordering). |
| `set-transform` | `{entity, translation?, rotation?, scale?}` | Merges the given fields over the current transform. Rotation is Euler XYZ radians. |
| `set-light` | `{entity?, direction?, color?, intensity?, ambient?}` | Edits the directional light (the given entity, else the first one found). |
| `select` | `{entity}` | Sets editor selection; returns `{id, name}`. |
| `get-selection` | — | Returns the current selection plus the `selectionVersion`/`sceneVersion` counters. |
| `deselect` | — | Clears the editor selection. |
| `add-entity` | `{preset?}` | Creates an entity from a preset (default `empty`); selects it; returns `{id, name}`. |
| `copy-entity` | `{entity}` | Deep-duplicates the entity (all components, new UUID); selects the copy; returns `{id, name}`. |
| `rename-entity` | `{entity, name}` | Sets the entity's Name component; returns its `{id, name}`. |
| `set-component-field` | `{entity, component, field, value, index?}` | Merges a single field into a component (generic; adds the component if missing). With `index`, addresses one element of an array field — an object `value` merges into that element, e.g. one `MaterialSet` slot. |
| `pick` | `{u=0.5, v=0.5}` | Ray-picks at a viewport UV (`0,0` = top-left) and selects the hit. Returns `{hit, id?, name?}`. |
| `inspect` | `{entity}` | Dumps every present component as JSON under `components`. |
| `focus` | `{entity}` | Moves the editor camera to look at the entity's transform. |
| `get-world-transform` | `{entity}` | Returns the entity's composed world translation + scale. |

## Presets

`add-entity` takes a `preset` naming what to spawn, matching the editor's **Create** menu (the set is
the `AddEntityPreset` enum). Examples: `empty` (Transform only), `cube` (the built-in cube mesh +
default material), `point-light`, `spot-light`, `directional-light`, `camera`. An unknown preset is
an error, not a silent fall-through to `empty`. Spawning a *catalog* model is the
[asset command](../asset-commands/) `instantiate-model`, not a preset.

## The editor camera and gizmo

| Command | Params | Effect |
|---|---|---|
| `get-camera` | — | Returns the editor fly-cam as `{position, yaw, pitch, fov, near, far}`. |
| `set-camera` | `{position?, yaw?, pitch?, fov?, near?, far?}` | Merges the given fields into the editor fly-cam. |
| `get-gizmo` | — | Returns the shared gizmo state `{op, space}`. |
| `set-gizmo` | `{op?, space?}` | Sets the gizmo `op` (`translate\|rotate\|scale`) and/or `space` (`world\|local`). |
| `get-debug-overlays` | — | Returns the viewport debug-overlay toggles `{bounds, sceneAabb, lightVolumes, grid, colliders}`. |
| `set-debug-overlays` | `{bounds?, sceneAabb?, lightVolumes?, grid?, colliders?}` | Toggles the [debug overlays](../../ui-and-editor/debug-visualization/); omitted fields stay unchanged. |
| `fly-input` | `{active, lookDx, lookDy, forward, …}` | Streams editor fly-cam input (look deltas in pixels accumulate until the next frame). |

`get-camera`/`set-camera` drive the same editor [fly-camera](../../ui-and-editor/) (`SceneEditCamera`)
the viewport uses — the scene-view eye, not an ECS `CameraComponent`. `set-camera` merges fields the
same way the transform commands do.

`get-gizmo`/`set-gizmo` read and write a single gizmo state. The engine's native overlay gizmo and the
editor's T/R/S shortcut both read it, so the gizmo mode stays consistent regardless of who set it.

Component and environment shapes are the DTO catalog; see [Shared types](../shared-types/) for the
DTO-first pipeline.

## Polling counters

`SceneEditContext` carries monotonically increasing counters a UI can poll to diff cheaply instead of
re-listing the whole scene each frame:

| Counter | Bumped when |
|---|---|
| `scene_version` | every scene-mutating command: `create-entity`, `destroy-entity`, `set-parent`, `add-component`, `remove-component`, `set-component`, `set-component-field`, `set-transform`, `set-light`, `set-environment`, `set-atmosphere`, `add-entity`, `copy-entity`, `rename-entity`, plus the [asset commands](../asset-commands/) that touch the scene (`instantiate-model`, `assign-asset`, `load-scene`/`load-project`, `new-project`/`open-project`). |
| `selection_version` | every `set_selection` (including `select`, `deselect`, `pick`, the auto-select on `add-entity`/`copy-entity`/`instantiate-model`, and an entity destroy or scene/project load that clears it). |

A client reads a counter once, then re-fetches the entity list or the selection only when the number
changes. The counters live on the context, not the wire, so any command that mutates the scene bumps
the right one regardless of who invoked it.

## Merge, don't reset

`set-transform`, `set-light`, and `set-component-field` first serialize the current value, copy the
provided field(s) over it, then deserialize the merged body. Setting only the translation therefore
leaves scale untouched. Vectors are `{x,y,z}` objects (`baseColor` is `{x,y,z,w}`), matching the
scene-file encoding.

## Picking and focus

`pick` builds a ray from the editor camera through the given viewport UV (converted to NDC `u*2-1,
v*2-1`) and calls `pick_entity`, which hits the nearest entity by world-space mesh AABB; the picker
flips the projection's `y` to match the renderer's clip space. Empty space returns `{hit:false}` and
deselects. `focus` reads the entity's `Transform.translation` and pulls the editor camera back along
its forward axis so the target sits in view. Both use the same editor
[fly-camera](../../ui-and-editor/) the viewport uses.

## In the code

| What | File | Symbols |
|---|---|---|
| Registration | `engine/crates/control/src/commands_scene.rs` | `register_scene_commands` |
| Entity resolution | `engine/crates/control/src/selector.rs` | `resolve_entity`, `entity_ref_dto`, `entity_uuid` |
| Component edits | `engine/crates/control/src/commands_scene.rs` | the `set-transform`, `set-light`, `set-component`, `set-component-field` rows |
| Presets + duplicate + rename | `engine/crates/control/src/commands_scene.rs` | the `add-entity`, `copy-entity`, `rename-entity` rows |
| Selection + picking | `engine/crates/control/src/commands_scene.rs` | the `select`/`get-selection`/`deselect`/`pick`/`focus` rows; `camera_dto` |
| Picker | `engine/crates/assets/src/render_scene.rs` | `pick_entity` |
| Editor camera + gizmo | `engine/crates/control/src/commands_scene.rs` | the `get-camera`/`set-camera`, `get-gizmo`/`set-gizmo`, `fly-input` rows |
| Poll counters | `engine/crates/sceneedit/src/context.rs` | `SceneEditContext::scene_version`, `selection_version`, `set_selection`, `active_scene` |
| The ECS behind the edits | `engine/crates/scene/src/scene.rs` | `Scene::for_each`, `find_entity_by_uuid`; `Name`, `IdComponent`, `Transform` |

## Related
- [Asset commands](../asset-commands/) — assigning meshes and textures, and `instantiate-model`
- [Shared types](../shared-types/) — the DTO-first wire contract these commands use
- [Scene & ECS](../../scene-and-ecs/) — the `hecs` world and component registry these commands drive
- [Control plane](../control-plane-architecture/) — how a command is registered and dispatched
