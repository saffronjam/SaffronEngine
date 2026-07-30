+++
title = 'Scene commands'
weight = 3
+++

# Scene commands

Scene commands expose authored entities, play state, selection, environment settings, and editor viewport controls through the [control plane](../control-plane-architecture/). The React editor and `sa` CLI use the same handlers, so an edit has one engine-side meaning regardless of its caller.

This page explains the behavior shared across those commands. The [control-command reference](../../../reference/control-commands/) lists every parameter and result DTO.

## Active-scene routing

Most handlers call `SceneEditContext::active_scene` rather than selecting a world themselves. It routes to the asset preview when that view is active, the play duplicate during Play or Pause, and the authored scene in Edit.

Entity selectors accept a decimal UUID number, a numeric UUID string, or an exact entity name. UUID lookup runs first. A selector resolves inside the active scene, so the same command can inspect runtime entities that exist only in the play duplicate.

Edits made to the play duplicate disappear on Stop. Edits intended for the project belong in Edit mode.

## Entity structure

The structural commands use scene and registry primitives rather than modifying `hecs` storage directly:

| Operation | Behavior |
|---|---|
| List | `list-entities` omits placement ghosts and reports parent IDs and bone tags. |
| Create | `create-entity` seeds identity, name, transform, relationship, and component order. |
| Destroy | `destroy-entity` removes the selected entity's full subtree and clears selection if it points inside that subtree. |
| Parent | `set-parent` rejects self-parenting and cycles, preserves world placement, and accepts an absent or zero parent as the root. |
| Copy | `copy-entity` creates a new ID, copies every registered component and component order, and places the copy beside the source under the same parent. It does not traverse child entities. |
| Rename | `rename-entity` updates the `Name` component. |

`add-entity` builds editor presets: empty, cube, plane, sphere, three light types, camera, reflection probe, and fog volume. Primitive meshes use native reserved IDs and need no catalog entry. Model assets use `instantiate-model` from the asset command group.

```sh
CUBE_ID=$(sa -o json add-entity cube | jq -r '.id')
sa set-transform "$CUBE_ID" --translation '{"x":0,"y":1,"z":0}'
sa inspect "$CUBE_ID"
```

`add-entity` and `copy-entity` select the result. Structural successes increment `scene_version`; selection changes increment `selection_version`.

## Component edits

The component registry supplies stable names, default construction, JSON conversion, removal policy, and authored row order.

`add-component` rejects duplicates and appends the new row to component order. Adding `Collider` fits its shape to the entity mesh when possible; adding `KinematicBones` fits bone capsules. `remove-component` rejects the non-removable `Name`, `Transform`, and `Relationship` rows.

`set-component` treats its `json` value as the component's serialized body. Missing fields follow that component serializer's defaults, so callers use it when they intend to supply the complete shape.

Partial commands preserve omitted state:

- `set-transform` serializes the current transform, replaces supplied translation, rotation, or scale fields, and deserializes the merged body.
- `set-light` performs the same merge for the selected directional light, or the first directional light when no entity is supplied.
- `set-component-field` replaces one top-level field. With `index`, it addresses an array element and merges an object value into that element.

`set-transform` accepts `smooth: true` to approach targets over rendered frames. When gizmo state has `preserveChildren` enabled, the command instead applies the parent transform exactly and rebases direct child locals so their world placements stay fixed.

Raw writes to `Relationship` trigger a hierarchy relink. `set-parent` remains the structural operation that rejects cycles before changing authored data.

## Selection and framing

`select`, `deselect`, and `get-selection` manage editor selection. The selection result also carries scene, selection, play, and animation versions, making it the editor's lightweight reconciliation poll.

`pick` tests meshless light and camera billboards before mesh surfaces. Static meshes use cached BVHs; skinned meshes use the current deformed pose. A mesh hit inside a `ModelInstance` selects the model root, while a miss clears selection; a mesh hit also carries the world position and geometric normal. See [Picking](../../scene-and-ecs/picking/) for the geometric path.

`query-surface-ray` casts one explicit world ray (origin in metres, a direction that normalizes, an optional range) against the scene's surface providers and returns the nearest hit's position and normal without touching selection. The vegetation brush's straight-down projection re-lands stroke samples through it.

`focus` frames the selected entity's full renderable subtree. It uses the model bounds center and field of view to choose a distance, falling back to the entity's world translation when no mesh bounds resolve.

`inspect` returns all present registered components plus their authored order. `get-world-transform` returns composed world translation and scale for tooling that needs a hierarchy-resolved position.

## Environment and play

`get-environment` returns the complete scene-wide sky, ambient, atmosphere, cloud, time-of-day, wind,
and fog settings. `get-environment-defaults` returns the canonical reset value. `set-environment`,
`set-atmosphere`, `set-clouds`, `set-time-of-day`, `set-wind`, and `set-fog` merge individual optional
fields or an inline JSON object over the current state.

`list-environment-profiles` combines built-in profiles with project `.senv` assets. Applying a
profile replaces the complete active environment atomically. Saving creates a catalog asset from the
active environment, and updating replaces an existing project profile without changing its asset id.

```sh
sa apply-environment-profile --profile '{"kind":"builtin","profile":"overcast"}'
sa save-environment-profile --name 'Rainy afternoon'
```

The same registration group carries the play-state commands:

| Command | Transition |
|---|---|
| `play` | Edit to Play, or Pause to Play |
| `pause` | Play to Pause |
| `step` | Advance a paused session by a bounded frame count |
| `stop` | Discard the play duplicate and return to Edit |
| `get-play-state` | Read state and version counters |

Script commands set per-slot overrides, forward gameplay input, report runtime status, and drain sequenced log and error rings. Their lifecycle is covered by [Script components and the play runtime](../../scripting/script-components-and-runtime/).

## Editor viewport state

The editor camera is independent of ECS `Camera` components. `get-camera` reads its free-eye or orbit state. `set-camera` applies a free-eye pose, or eases an orbit target when both `pivot` and `distance` are present.

`get-gizmo` and `set-gizmo` share translate, rotate, scale, world/local, and preserve-children state with the native overlay. `gizmo-pointer` sends hover, begin, drag, and end phases in normalized device coordinates. Gizmo commands reject interaction outside Edit.

`fly-input` accumulates look deltas and held movement directions for the editor camera. `script-input` updates the gameplay input snapshot consumed by the next simulation tick. Keeping these streams separate prevents editor navigation from becoming game input implicitly.

## Version counters

Commands bump the counter for the state they change:

| Counter | Meaning |
|---|---|
| `scene_version` | Authored structure, components, transforms, environment, or restored edit state changed |
| `selection_version` | The selected entity changed or became invalid |
| `play_version` | Play-state transition occurred |
| `animation_version` | Animation or timeline state changed |

The editor compares these values before issuing heavier list and inspection requests. The counters describe authoritative engine state, so CLI edits and editor edits invalidate the same views.

## Source map

| What | File | Symbols |
|---|---|---|
| Scene-domain registrations | `engine/crates/control/src/commands_scene/` | `register_scene_commands` |
| Entity selectors and DTO conversion | `engine/crates/control/src/selector.rs` | `resolve_entity`, `entity_ref_dto` |
| Active-scene and version state | `engine/crates/sceneedit/src/context.rs` | `SceneEditContext::active_scene`, `SceneEditContext::set_selection` |
| Registry-backed component behavior | `engine/crates/scene/src/registry.rs` | `ComponentRegistry`, `ComponentTraits` |
| Hierarchy-safe structural edits | `engine/crates/scene/src/hierarchy.rs` | `Scene::set_parent`, `Scene::relink_hierarchy` |
| Surface picking and framing bounds | `engine/crates/assets/src/render_scene/` | `pick_entity`, `model_render_aabb` |
| Protocol shapes | `engine/crates/protocol/src/dto/` | `EntitySelector`, `SetTransformParams`, `SelectionResult` |
| Environment profiles | `engine/crates/assets/src/environment_profile.rs` | `builtin_environment_profiles`, `save_environment_profile`, `load_environment_profile` |

## Related

- [Control-command reference](../../../reference/control-commands/)
- [Asset commands](../asset-commands/)
- [Shared types](../shared-types/)
- [Scene and ECS](../../scene-and-ecs/)
- [Control plane](../control-plane-architecture/)
