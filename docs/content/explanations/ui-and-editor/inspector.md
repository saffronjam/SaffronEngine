+++
title = 'Inspector'
weight = 6
+++

# Inspector

The Inspector is the editor panel for viewing and editing the components of the selected entity. It is data-driven: it holds no per-component code, rendering whatever components and fields the engine returns and choosing a widget for each field from its value shape.

A component registered engine-side with `register_component!` appears here automatically, with a sensible widget even before it has an explicit hint. The panel describes itself entirely from the live `inspect` result, so the editor and the engine never drift on what an entity holds.

## How it works

The panel reads `componentsBySelected` (the `inspect` result, kept fresh by the [reconcile poll](../selection/)) and iterates its `components` map. For each present component it draws a header and walks its fields, handing every `(component, field, value)` to `renderField`:

```tsx
{Object.entries(dto).map(([field, value]) => (
  renderField(component, field, value,
    (next) => onFieldChange(component, field, next),
    { onDragStart, onDragEnd })
))}
```

The render path has no `if (component === "Transform")` branch. Components draw in the selected entity's authored order, which is returned by `inspect` and saved with the scene. New sections are added at the bottom, drag reordering writes a new order through the control plane, and the sort action restores the canonical order (`Name`, `Transform`, `Mesh`, …). Any unknown component falls in after the known set, but that is ordering only, never a render switch.

## Picking a widget

`renderField` resolves a field to a widget in three steps:

1. The explicit `FIELD_HINTS` table, keyed `Component.field`, which mirrors the engine's per-component widgets.
2. The value's shape — `{x,y,z}`→vec3, `{x,y,z,w}`→vec4, number, boolean, string.
3. A read-only text fallback, so an unmapped field is still visible.

A hint also carries min/max/step, slider-vs-drag, the option list for a closed enum (drawn as a Select — `Rigidbody.motion`, `Collider.shape`, `AnimationPlayer.wrap` and `transitionMode`), and the asset kind a uuid field picks from: `mesh`, `texture`, `material`, `model` (the `.smodel` a `ModelInstance` came from), or `animation` (an `AnimationPlayer` clip). Without a hint an enum or id would fall to step 3 and render as a free-text box, so each closed enum and each id reference is hinted. Field labels are sentence-cased from the wire key (`humanizeFieldName`: `albedoTexture` → "Albedo texture"), and color fields open a saturation/hue (and alpha) canvas in a popover rather than the native OS picker. Selecting **(none)** in a mesh or material-texture picker clears the slot — the engine treats the `0` asset id as "unassigned" rather than rejecting it.

One unit conversion lives at the widget boundary. `Transform.rotation` is radians on the wire but shown in degrees, driven by the hint's `convertRadians` flag. SpotLight `innerAngle`/`outerAngle` are degrees on both sides, so their `unit:"deg"` is a label and clamp only, no conversion. A `MaterialSet` slot's `baseColor`/`emissive` override rows use color swatches, its `metallic`/`roughness` are sliders, and its texture rows plus `Mesh.mesh` are [asset pickers](../asset-pickers-and-drag-drop/).

## Rig components

A few components carry data keyed to the skeleton, so they get a bespoke body instead of the raw-id JSON the generic grid would produce. `SkinnedMesh` is genuinely not user-authored — its bone-entity array and inverse-bind matrices are import-derived — so it stays read-only: mesh and root bone resolved to names, a joint count in place of the matrices. The other three are **editable by joint name**, never by raw index:

- **`FootIk`** — `enabled`/`groundHeight` are scalar fields; `chains` is an add/remove list of two-bone IK limbs, each picking Upper/Mid/End from a joint dropdown plus a pole vector.
- **`KinematicBones`** — `enabled` toggles the feature; `driven` is a joint-subset mask with an "All joints" switch (an empty wire array means *every* joint) and a per-joint checklist.
- **`BonePhysics`** — a fixed-length list (1:1 with the skeleton) of collapsible per-bone cards, each tuning the ragdoll body: collider half-extents, mass, the joint constraint type, its swing/twist limits (shown in degrees, stored in radians), and the PD drive gains.

Joint names resolve entirely client-side — the bone entities are already in the [hierarchy](../hierarchy-panel/) list (`SkinnedMesh.bones[i]` → joint entity → `Name`), so no engine round-trip is needed. All three write the whole array back through the normal `set-component` read-modify-write, and the edits apply at the right moment: foot IK reads `chains` each frame, while the kinematic bodies and the ragdoll are built from `driven`/`bones` when physics starts at the next Play (not mid-Play). The runtime ragdoll blend is still driven from the [Physics panel](../physics-panel/).

## Material slots

`MaterialSet` is not a flat field grid but an **override editor**, one card per slot. A slot's first row is a material picker bound to `MaterialSlot.material` — the `.smat` this submesh draws with — with a shortcut that opens that material in the [graph editor](../../materials-and-pipelines/node-graph-codegen/). Below it, one row per exposed PBR parameter (`baseColor`, `metallic`, `roughness`, the texture ids, `blend`, …) shows the value inherited from the referenced material until you override it; editing a row writes that key into the slot's sparse `overrides` map, and clearing it reverts to the inherited value.

Every slot edit — the material reference and each override — routes through `set-component-field` addressing the slot by `index` (`{component:"MaterialSet", field:"slots", index, value:{…}}`), so the engine merges the pushed object into `slots[index]`. The parameter widgets reuse the `Material.<field>` hints, so a slider stays a slider and a colour stays a swatch, exactly as a flat component would render.

## Read-modify-write

The engine's `set-component` rewrites the whole component rather than merging, so a single-field edit builds the full DTO with that one field patched and sends the lot:

```ts
const onFieldChange = (component, field, next) => {
  const current = (componentsObj[component] ?? {}) as Record<string, unknown>;
  const patched = { ...current, [field]: next };
  applyOptimisticComponent(component, patched);  // overlay immediately
  coalescerFor(component, field).push(patched);  // coalesced send
};
```

`applyOptimisticComponent` overlays the change on the live inspect result so the widget updates without waiting a poll interval. High-frequency edits — dragging a number, moving a slider — funnel through a per-(component,field) coalescer, and the drag brackets flip `store.dragActive` so the reconcile poll will not overwrite the optimistic value mid-drag.

A few fields take a narrower write than the full-DTO `set-component`:

- `Transform` uses the `set-transform` merge helper, sending only the changed field.
- `Mesh.mesh` uses the dedicated `assign-asset`.
- `MaterialSet` slot edits — the referenced material and each parameter override — route through `set-component-field` with the slot `index` (see [Material slots](#material-slots)).
- Any other uuid field uses the single-field merge `set-component-field`.

Everything else takes the generic full-DTO `set-component` write, exactly like every other structured component.

## Smoothed drags, drag-local widgets

A drag samples at the webview's pointer rate (~60 Hz), far below the engine's frame rate, so writing each sample directly would render as visible steps. `Transform` edits borrow the [gizmo's](../gizmo/) answer: mid-drag sends carry `smooth:1`, which makes `set-transform` record the numeric fields as per-entity targets instead of writing them, and the engine converges the live component toward those targets every rendered frame with the same ~25ms exponential the gizmo uses for pointer samples (`step_edit_smoothing`). Once within epsilon the value snaps exactly and the entry is dropped. Transform smoothing yields to a live gizmo drag on the same entity, and applies exact under preserve-children (each write must rebase the subtree). Material scrubs are not smoothed engine-side — each coalesced sample writes directly; the optimistic-local overlay is what keeps the widget itself fluid.

The widgets themselves never wait on that round trip. Every scrub widget (NumberDrag, SliderField, VectorEditor, ColorField) renders drag-local state through `useScrubValue`: the pointer updates the widget immediately, changes flow outward at most once per animation frame, and the prop only drives the widget when no gesture is active — so the color canvas or a scrubbed axis tracks the cursor exactly while the store, wire, and viewport follow.

The release always ends the stream with one exact write: the widget flushes its pending emit, then `onFieldDragEnd` re-pushes the field's latest optimistic value after clearing `dragActive`, and a non-smooth send both writes verbatim and cancels any pending animation for that entity. Texture and `unlit` are not animatable and apply immediately either way.

## Add and remove

`add-component` and `remove-component` are guarded the same way the engine guards them. Remove only shows for removable components: `Name`, `Transform`, and the import-managed identity components `ModelInstance` and `SkinnedMesh` are in `NON_REMOVABLE`, so the inspector cannot strip an entity of its identity, its place in the world, or its rig. The Add Component dropdown lists every registered component the entity lacks, minus two exclusions: components written only by import (`ModelInstance`, `SkinnedMesh`, and `MaterialSet`) never appear, and the rig sidecars that index a skeleton (`AnimationPlayer`, `FootIk`, `KinematicBones`, `BonePhysics`) appear only on an entity that already carries a `SkinnedMesh`. Selecting one calls `add-component`, and the engine appends the new section to the entity's stored component order.

Each section header has a drag handle. Dragging follows the tab-strip pattern: after a small pointer threshold, neighboring sections slide apart to show the landing slot, and the reordered list commits only on release. A drop sends the full visible component order to `set-component-order`, and undo/redo replays that same command. The hierarchy's selected-entity component subrows read the same order, so clicking a subrow always scrolls to the section in the matching position. The stored order excludes hidden structural sections such as `Relationship` and `Bone`.

## In the code

| What | File | Symbols |
|---|---|---|
| The generic panel | `editor/src/panels/InspectorPanel.tsx` | `InspectorPanel`, `NON_REMOVABLE`, `NON_ADDABLE`, `RIG_ONLY` |
| Component order + rig bodies | `editor/src/lib/componentOrder.ts`, `editor/src/panels/InspectorPanel.tsx` | `COMPONENT_ORDER`, `canonicalComponentNames`, `orderedComponentNames`, `ReadonlyRow` |
| Field-kind dispatch | `editor/src/components/fieldRenderer.tsx` | `renderField`, `resolveHint`, `FIELD_HINTS`, `AssetKind` |
| Color canvas + label casing | `editor/src/components/ColorField.tsx`, `editor/src/lib/humanize.ts` | `ColorField`, `humanizeFieldName` |
| Read-modify-write routing | `editor/src/panels/InspectorPanel.tsx` | `onFieldChange`, `sendWrite`, `coalescerFor` |
| Optimistic overlay | `editor/src/state/store.ts` | `applyOptimisticComponent`, `dragActive` |
| Edits (engine) | `engine/crates/control/src/commands_scene.rs` | `set-component`, `set-transform`, `set-component-field`, `set-component-order`, `add-component`, `remove-component` |
| Smoothed drags (engine) | `engine/crates/sceneedit/src/smoothing.rs` | `step_edit_smoothing`, `TransformSmoothTarget` |
| Drag-local widgets | `editor/src/lib/useScrubValue.ts` | `useScrubValue`, `ScrubValue` |

## Related

- [Component registry](../../scene-and-ecs/component-registry/) — the registered components `inspect` enumerates
- [Built-in components](../../scene-and-ecs/built-in-components/) — the structs being edited
- [Asset pickers](../asset-pickers-and-drag-drop/) — the mesh/material uuid fields
- [Scene commands](../../tooling-and-control/scene-commands/) — `inspect` and the component edit commands
