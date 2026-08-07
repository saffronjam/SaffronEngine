+++
title = 'Inspector'
weight = 6
+++

# Inspector

The Inspector displays and edits the selected entity's components. Ordinary components use a registry-driven field grid, while components with nested collections or import-derived data use focused editors over the same control surface.

The panel reads `componentsBySelected`, which the [selection reconciliation](../selection/) lane fills from `inspect`. It shows an empty state when there is no selected and inspected entity.

## Component sections

`inspect` returns a component map and an authored `componentOrder`. `orderedComponentNames` keeps the present components in that order, appends missing names in canonical order, and hides `Relationship` and `Bone`. Parenting belongs to the [Hierarchy panel](../hierarchy-panel/), while the empty bone tag is represented by its entity row.

Each visible component appears in a section with a reorder handle and, when allowed, a remove button. A one-shot `focusComponent` signal from a hierarchy component row scrolls the matching section into view.

## Generic field grid

The generic body passes each `(component, field, value)` to `renderField`. `resolveHint` first checks `FIELD_HINTS`, then infers a widget from vector, number, or Boolean value shapes. Values without one of those shapes use a text input.

| Hint kind | Editor control | Typical use |
|---|---|---|
| `vec3`, `vec4` | Axis number editors | transforms, directions, and extents |
| `color3`, `color4` | Color popover | light and material colors |
| `number`, `slider` | Drag number or bounded slider | ranges, weights, and intensities |
| `bool` | Switch | feature flags |
| `enum` | Select | motion, shape, wrap, and blend modes |
| `lockAxes` | Axis locks | rigid-body position and rotation locks |
| `struct` | Nested field group | collider friction and restitution |
| `uuid` | Filtered asset picker | mesh, texture, material, model, and animation references |

Hints also provide bounds, step sizes, enum options, and asset kinds. [`AssetPicker`](../asset-pickers-and-drag-drop/) accepts catalog selection and drag-and-drop; choosing None writes the zero asset identifier.

Unit conversion happens at the widget boundary. `Transform.rotation` vectors and `CharacterController.maxSlopeAngle` scalars display degrees but write radians. Spot-light angles already use degrees on the wire, so their degree hint supplies display bounds without conversion.

## Structured component bodies

Components whose data is not useful as a flat JSON grid have dedicated bodies:

| Component | Inspector behavior |
|---|---|
| `Script` | Orders script slots, assigns or creates files, loads declared fields, and writes per-instance overrides. |
| `Morph` | Shows one 0-to-1 slider per imported target name and sends the complete weight vector. |
| `MaterialSet` | Edits each submesh slot's material reference and sparse object overrides. |
| `Collider` | Adds Fit to mesh and identifies a collider without `Rigidbody` as a static body. |
| `SkinnedMesh` | Resolves the imported mesh and root bone to names and reports joint count read-only. |
| `FootIk` | Edits scalar settings and two-bone chains through joint-name selectors. |
| `KinematicBones` | Edits enabled state and a joint mask, where an empty mask means all joints. |
| `BonePhysics` | Edits fixed per-joint collider, constraint, and drive cards. |

The rig editors derive joint names from `SkinnedMesh.bones` and the hierarchy entity list, so their pickers need no extra control request. [Physics inspector](../physics-inspector/) covers the authoring semantics of the physics components.

A `FogVolume` section shows a warning when scene fog is disabled or uses a non-volumetric mode. The component remains editable, but it contributes no density to the froxel grid in that environment state.

## Write routing

`set-component` replaces a complete component body. A generic field edit therefore clones the inspected DTO, patches one field, applies that DTO optimistically, and routes the payload according to its field type:

```ts
if (hint.kind === "uuid") {
  return component === "Mesh" && field === "mesh"
    ? client.assignAsset(entity, "mesh", assetId)
    : client.setComponentField(entity, component, field, assetId);
}
if (component === "Transform") {
  return client.setTransform(entity, { [field]: dto[field] }, smooth);
}
return client.setComponent(entity, component, dto);
```

Material slot changes bypass the top-level field path and call `set-component-field` with `field: "slots"` and a slot index. Script overrides use `set-script-override`, morph sliders use `set-morph-weights`, and Collider's Fit to mesh uses `fit-collider`.

## Gestures and undo

Generic fields use one coalescer per component and field. The first pointer or focus event captures the prior DTO and sets `dragActive`; intermediate values update the optimistic store and coalesced wire stream. Release clears the gate, sends the latest value once without transform smoothing, and records one undo entry from the captured DTO to the final DTO.

Transform samples sent during a drag set `smooth`, which moves engine-side values toward per-entity targets with the shared edit smoother. The exact release write cancels that target. Other generic fields apply their coalesced values directly.

Material slot gestures and script override gestures follow the same one-entry undo pattern with their narrower commands. Discrete switches, selections, and resets record an entry immediately. Morph weight scrubs and Fit to mesh update the engine without adding an undo entry.

## Material slots

Each `MaterialSet` slot binds one `.smat` asset for a mesh subrange. Its `overrides` object stores only parameters authored on that entity. Parameters absent from the map inherit from the referenced material and do not appear as rows.

The Override menu adds one supported parameter with its engine default. Editing writes the entire sparse override map into that slot, and removing a row deletes its key to restore inheritance. The material shortcut opens the referenced asset in the [material graph editor](../../materials-and-pipelines/node-graph-codegen/).

## Add, remove, and order

The Add Component menu follows `COMPONENT_ORDER` and excludes components managed by entity creation or import: `Name`, `MaterialSet`, `ModelInstance`, `SkinnedMesh`, and `Morph`. `AnimationPlayer`, `FootIk`, `KinematicBones`, and `BonePhysics` appear only when the entity has `SkinnedMesh`.

Remove is hidden for `Name`, `Transform`, `ModelInstance`, `SkinnedMesh`, and `Morph`. A successful add records remove as its inverse. A successful remove captures the prior body and order so undo can add the component, restore its values, and restore section order.

Section dragging uses a 4-pixel threshold and updates only a visual preview until release. The commit writes the full visible order through `set-component-order` and records the previous order for undo and redo. Sort Components writes the canonical order through the same path.

## In the code

| What | File | Symbols |
|---|---|---|
| Sections, routing, and undo capture | `editor/src/panels/InspectorPanel.tsx` | `InspectorPanel`, `componentBody`, `applyWrite`, `onFieldChange`, `recordFieldEdit` |
| Generic field dispatch | `editor/src/components/fieldRenderer.tsx` | `FIELD_HINTS`, `resolveHint`, `inferKind`, `renderField` |
| Component ordering | `editor/src/lib/componentOrder.ts` | `COMPONENT_ORDER`, `HIDDEN_COMPONENTS`, `canonicalComponentNames`, `orderedComponentNames` |
| Script slot editor | `editor/src/components/ScriptSlots.tsx` | `ScriptSlots`, `writeSlots`, `recordOverrideEdit` |
| Rig editors | `editor/src/components/FootChainsEditor.tsx`, `BoneMaskField.tsx`, `BonePhysicsEditor.tsx` | `FootChainsEditor`, `BoneMaskField`, `BonePhysicsEditor` |
| Optimistic store state | `editor/src/state/store.ts` | `applyOptimisticComponent`, `dragActive`, `pushEdit` |
| Component registry | `engine/crates/scene/src/registry.rs` | `register_builtin_components`, `ComponentRegistry::component_order`, `ComponentRegistry::set_component_order` |
| Scene edit commands | `engine/crates/control/src/commands_scene/` | `register_scene_commands`, `set-component`, `set-transform`, `set-component-field`, `set-component-order`, `add-component`, `remove-component` |

## Related

- [Component registry](../../scene-and-ecs/component-registry/) — defines the serializable component set exposed by `inspect`.
- [Built-in components](../../scene-and-ecs/built-in-components/) — describes the component data edited here.
- [Asset pickers](../asset-pickers-and-drag-drop/) — explains catalog filtering, clearing, and drag-and-drop.
- [Physics inspector](../physics-inspector/) — explains collider, body, controller, and bone-physics authoring.
- [Script-declared fields](../../scripting/script-declared-fields/) — explains slot schemas, defaults, and instance overrides.
- [Scene commands](../../tooling-and-control/scene-commands/) — documents component inspection and edit commands.
