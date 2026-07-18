+++
title = 'Transform gizmo'
weight = 4
+++

# Transform gizmo

The transform gizmo edits a selected entity directly in the viewport. Its handles, hit testing, and transform math run in the engine; the editor supplies pointer phases and controls the operation, reference space, and child-preservation option.

## Overlay geometry

The host builds the gizmo as `OverlayVertex` geometry after rendering the scene. The renderer composites it over the tonemapped display image at display resolution, so exposure and scene anti-aliasing do not alter its colors or edges. Handles use the on-top overlay range and remain visible through scene geometry.

Each vertex carries signed edge coordinates and pixel half-extents. `gizmo_overlay.slang` converts those values to alpha coverage across a one-pixel feather. This analytic coverage smooths axis lines, filled plane handles, box ends, and rotation rings after the scene's multisample or temporal resolve.

| Operation | Handles | Effect |
|---|---|---|
| Translate | X, Y, and Z axes; XY, YZ, and XZ planes | Moves along one axis or two axes |
| Rotate | X, Y, and Z rings | Changes the corresponding rotation channel |
| Scale | X, Y, and Z axes with box ends; center box | Scales one channel or all channels uniformly |

The host and hit tester share `gizmo_axes`, `gizmo_plane_corners`, and `ring_basis`. In World space the basis is the identity axes; in Local space it is rotated by the selected entity's world rotation. Handle length grows with camera distance, which keeps the projected control usable across zoom levels.

## Pointer gesture

The transparent viewport element maps pointer coordinates into normalized device coordinates and sends `gizmo-pointer`. The engine maps the values back to viewport pixels before applying the shared projection and hit-test math.

| Phase | Editor event | Engine action |
|---|---|---|
| `hover` | Pointer move with no button | Hit-test and highlight a handle |
| `begin` | Left press | Freeze the selected transform and activate the hovered handle |
| `drag` | Movement beyond 3 CSS pixels | Update the latest pointer target |
| `end` | Release or cancellation | Apply the release sample exactly and clear drag state |

Hover and drag samples pass through a 16 ms latest-value coalescer. The engine smooths a pending drag toward the newest sample on rendered frames with `alpha = 1 - exp(-dt / 0.025)`. The final `end` sample bypasses any remaining smoothing distance.

A press that stays within the threshold becomes a [selection](../selection/) pick at the press position. During a drag, `dragActive` prevents reconcile updates from replacing the in-progress inspector state. Each applied sample increments `sceneVersion`; release inspects the settled transform and records the whole gesture as one scene-tab undo entry.

## Shared gizmo state

`SceneEditContext` owns `gizmo_op`, `gizmo_space`, and `preserve_children`. The Topbar updates that state optimistically through `set-gizmo`, while the reconcile poll reads `get-gizmo` so an external control call appears in the UI.

```json
{
  "cmd": "set-gizmo",
  "params": {
    "op": "rotate",
    "space": "local",
    "preserveChildren": true
  }
}
```

The operation shortcuts default to W for Translate, E for Rotate, and R for Scale. They resolve through the [editor settings](../editor-settings/) registry, and the Topbar tooltips display the effective bindings. Gizmo controls and pointer commands reject writes outside Edit state. The overlay builder also hides edit chrome while an asset preview is active.

## Transform application

Drag begin freezes the entity's world translation and rotation, local scale, and parent world matrix. Translation and rotation are computed in world space, then converted into the frozen parent frame. Scale stays in the entity's local transform. A minimum scale factor prevents a handle from crossing through zero.

With Preserve Children disabled, a parent's transform carries its descendants through the normal relationship hierarchy. Enabling it freezes each direct child's world matrix at drag begin. After every parent update, the engine computes the child's new local matrix as:

```text
childLocal = inverse(parentWorld) * frozenChildWorld
```

`set_local_from_matrix` decomposes that matrix back into translation, rotation, and scale. Grandchildren follow their rebased direct parent. The same option applies to `set-transform`, so inspector edits and gizmo drags have matching subtree behavior.

`Transform` cannot store shear. A rotated child beneath a non-uniformly scaled parent may therefore change slightly when the rebased matrix contains shear; the decomposition preserves only its representable TRS components.

## In the code

| What | File | Symbols |
|---|---|---|
| Viewport gesture and undo capture | `editor/src/panels/ViewportPanel.tsx` | `ViewportPanel`, `DRAG_THRESHOLD_PX`, `GIZMO_STREAM_MS` |
| Latest-sample transport | `editor/src/control/coalesce.ts` | `makeCoalescer` |
| Topbar controls | `editor/src/panels/Topbar.tsx` | `Topbar` |
| Rebindable shortcuts | `editor/src/app/useGizmoShortcuts.ts` | `useGizmoShortcuts`, `GIZMO_COMMANDS` |
| Gizmo state, hit test, and drag math | `engine/crates/sceneedit/src/gizmo.rs` | `NativeGizmoState`, `SceneEditContext::hit_native_gizmo`, `SceneEditContext::apply_native_gizmo_drag`, `SceneEditContext::step_native_gizmo_drag` |
| Overlay builder | `engine/crates/host/src/overlay.rs` | `build_native_gizmo`, `build_scene_edit_overlay` |
| Overlay vertex and recorder | `engine/crates/rendering/src/overlay.rs` | `OverlayVertex`, `OverlayState`, `record_overlay` |
| Control commands | `engine/crates/control/src/commands_scene.rs` | `get-gizmo`, `set-gizmo`, `gizmo-pointer` |
| Child-local decomposition | `engine/crates/scene/src/hierarchy.rs` | `Scene::set_local_from_matrix` |

## Related

- [Selection](../selection/) — click picking and the shared viewport gesture
- [Editor camera](../editor-camera/) — projection used for drawing and hit testing
- [Undo and redo](../undo-redo/) — the scene-tab history entry created on release
- [Transform and matrices](../../scene-and-ecs/transform-and-matrices/) — local and world transform composition
- [Scene hierarchy](../../scene-and-ecs/scene-hierarchy/) — parent-child relationships and rebasing
