+++
title = 'Skeleton overlay'
weight = 3
+++

# Skeleton overlay

The skeleton overlay draws the selected rig as a line skeleton in the viewport: a segment from
each joint to its parent, a screen-constant dot per joint, and optional per-joint RGB axes. It is
read-only editor chrome for seeing where the bones are and what a clip moves, the same job as the
bone display in Unreal's
[Skeleton Editor](https://dev.epicgames.com/documentation/en-us/unreal-engine/skeleton-editor-in-unreal-engine).

## Scope and visibility

The overlay covers one rig at a time: the selected entity in the scene view, or the previewed
model's root while an [asset preview](../../ui-and-editor/asset-editor/) is active. It is opt-in;
`show` defaults off. `Scene::model_rig_entity` resolves the rig by a pre-order subtree search for
the first entity carrying a `SkinnedMesh`. An imported model's skinned mesh rides a child node,
so selecting the container works, and a selection with no rig in its subtree draws nothing.

The editor overlay splits into a depth-tested range and an on-top range, each drawn by its own
pipeline from one vertex buffer. The skeleton sits in the on-top range, whose pipeline runs no
depth test, so every joint stays visible through the skin that would otherwise occlude it.

It also draws in both Edit and Play. `build_scene_edit_overlay` gates the gizmo, billboards,
frustums, and debug overlays behind its `edit_chrome` flag, but calls the skeleton builder outside
that gate. Entering [Play](../../ui-and-editor/play-mode/) re-resolves the selection by uuid into
the play duplicate, so the overlay tracks the same rig while a clip runs.

## Per-joint geometry

`build_skeleton_overlay` walks `SkinnedMesh::bone_handles`, the resolved joint entities in glTF
skin order, each tagged with a `Bone` component. Per joint it:

1. Projects the joint's world translation to pixels with `viewport_project`; a joint behind the
   camera or outside the depth range is skipped.
2. Draws a 2 px pale-blue segment to the parent (`Relationship.parent_handle`) when it also
   carries `Bone`; root joints get no segment.
3. Draws an amber dot of `max(jointSize, 2.5)` px radius. The radius is in pixels, so dots hold
   their on-screen size at any distance.
4. With `axes` on, draws three 1.5 px lines of 0.08 world units along the world-rotation basis:
   X red, Y green, Z blue.

The geometry comes from the same feathered primitives the
[transform gizmo](../../ui-and-editor/gizmo/) uses. `add_line_flat` and `add_circle_fill` pack
signed edge coordinates into each `OverlayVertex`, and the overlay shader turns them into a
coverage alpha, giving lines and dots an analytic ~1 px feather at any thickness.

## Driving it

`set-skeleton-overlay` writes the options and `get-skeleton-overlay` reads them; both reply with
the full state. Every set parameter is optional, so a call patches only what it passes:

```sh
$ sa set-skeleton-overlay --show true --axes true
{
  "show": true,
  "axes": true,
  "jointSize": 4.0,
  "highlightJoint": -1
}
$ sa set-skeleton-overlay --show false     # hide again; axes and jointSize keep their values
```

The command clamps `jointSize` to at least 0.5, and the builder floors the drawn radius at
2.5 px. The options live on `SceneEditContext` as `SkeletonOverlayOptions` and are session state,
never written to `project.json`; the [debug overlays](../../ui-and-editor/debug-visualization/)
follow the opposite policy and persist.

## Highlight and picking in the asset preview

While an asset preview is active, two extra channels serve the asset editor's skeleton panel:

- `set-skeleton-highlight {joint}` tints one joint. `joint` is a `get-asset-model` node index,
  resolved through the preview's node-to-bone map (`preview_bone_by_node`) to the spawned bone
  entity; the matching dot draws at 1.8× radius in green. A negative `joint` clears the highlight.
- `pick-skeleton-joint {u, v, radiusPx?}` maps a normalized viewport click to the nearest
  projected joint within `radiusPx` (default 8 px) and returns `{found, nodeIndex}`. Clicking a
  joint in the preview viewport selects it in the skeleton tree through this command.

The highlight never blanks the rest of the skeleton: the overlay targets the previewed model's
root, and a bone entity has no `SkinnedMesh` of its own, so the whole rig keeps drawing while one
joint stands out.

## In the code

| What | File | Symbols |
|---|---|---|
| Skeleton geometry builder | `engine/crates/host/src/overlay/` | `build_skeleton_overlay`, `build_scene_edit_overlay` |
| Feathered primitives | `engine/crates/host/src/overlay/` | `add_line_flat`, `add_circle_fill` |
| Viewport projection | `engine/crates/sceneedit/src/gizmo.rs` | `viewport_project` |
| Overlay options | `engine/crates/sceneedit/src/overlay.rs` | `SkeletonOverlayOptions` |
| Rig resolution | `engine/crates/scene/src/hierarchy.rs` | `Scene::model_rig_entity` |
| Bone + parent components | `engine/crates/scene/src/component.rs` | `SkinnedMesh`, `Bone`, `Relationship` |
| Control commands | `engine/crates/control/src/commands_animation.rs` | `set-skeleton-overlay`, `get-skeleton-overlay`, `set-skeleton-highlight`, `pick-skeleton-joint` |
| On-top vs depth-tested PSOs | `engine/crates/rendering/src/pipelines/` | `request_overlay`, `request_overlay_depth` |

## Related

- [Playback runtime](../playback-runtime/) — the evaluator that animates the joints this draws
- [Animation data model](../animation-data-model/) — the skeleton and pose types it reads
- [Transform gizmo](../../ui-and-editor/gizmo/) — the overlay pass and feathered primitives it shares
- [Asset editor](../../ui-and-editor/asset-editor/) — the preview that drives highlight and picking
- [Play mode](../../ui-and-editor/play-mode/) — the play duplicate the selection re-resolves into
