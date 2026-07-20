+++
title = 'Physics inspector'
weight = 7
+++

# Physics inspector

The [Inspector](../inspector/) authors physics components through the same registry-driven component path as the rest of the scene. Field hints select physics-specific widgets, while structured rig data receives dedicated editors that preserve its relationship to the imported skeleton.

## Component sections

`Rigidbody`, `Collider`, and `CharacterController` are available from Add Component on any entity. `KinematicBones` and `BonePhysics` are offered only when the entity has `SkinnedMesh`, because their arrays index that rig's bones.

The body/shape split is visible in separate sections. [Rigidbody and collider](../../physics/rigidbody-and-collider/) explains how motion and collision geometry combine, including the implicit static body created by a collider without a rigidbody. The Collider section repeats that state as `No Rigidbody - static body`.

The component fields arrive as protocol DTOs. A collider has the same nested shape in the Inspector and on the wire:

```json
{
  "shape": "capsule",
  "halfExtents": { "x": 0.35, "y": 0.9, "z": 0.35 },
  "sourceMesh": "0",
  "offset": { "x": 0.0, "y": 0.9, "z": 0.0 },
  "material": { "friction": 0.6, "restitution": 0.0 },
  "isSensor": false
}
```

## Physics field widgets

`FIELD_HINTS` maps a `Component.field` name to a reusable `FieldKind`. Physics uses these specialized forms:

| Field kind | Physics use |
|---|---|
| `enum` | Motion type, collider shape, and numeric collision-layer slot |
| `lockAxes` | X/Y/Z translation and rotation locks |
| `struct` | Nested collider friction and restitution sliders |
| converted `number` | Character maximum slope shown in degrees and stored in radians |

`EnumField` shows readable labels while preserving the protocol value. The collision-layer hint is numeric, so the visible Moving, Character, and Debris choices write integer slots. [Collision layers and triggers](../../physics/collision-layers-and-triggers/) defines the resulting matrix.

`LockAxesField` emits a one-axis patch. The dispatcher merges that patch into the existing `{x,y,z}` value before the Inspector writes the component. The nested material editor does the same for friction and restitution.

Every ordinary physics edit follows the Inspector's read-modify-write rule. The UI patches one field into the current DTO and sends the complete component through `set-component`. Continuous drags are optimistic and coalesced, while discrete switches and dropdowns create one undo entry.

## Fit to mesh

The Collider section includes Fit to mesh. `fit-collider` reads the entity's mesh forest, transforms its bounds into the body's local frame, and updates `halfExtents`, `offset`, and `sourceMesh`. The shape determines how those fitted bounds are interpreted; [Collision shapes](../../physics/collision-shapes/) covers the analytic and cooked cases.

The command bumps `sceneVersion`, and the reconcile poll reads back the derived values. Fit to mesh is a derived action and is not added to the editor's undo history. Adding a Collider runs the same fitting operation once when a resolvable mesh is present.

## Rig physics editors

`KinematicBones` has an enabled switch and a bone-mask control. An empty `driven` array means every joint; choosing a subset stores sorted bone indices. The physics world consumes the setting at the next Play transition, as described in [Kinematic bones](../../physics/kinematic-bones/).

`BonePhysicsEditor` presents one collapsible card per imported bone and filters cards by joint name. The array length and order remain tied to `SkinnedMesh.bones`, so cards cannot be added, removed, or reordered.

Each bone card edits collider half extents, mass, constraint type, swing/twist limits, and motor stiffness, damping, and maximum force. Limits are stored in radians and shown in degrees. These values build the rig's bodies and constraints at the next Play transition; the [Physics panel](../physics-panel/) controls the live ragdoll blend.

The [Character controller](../../physics/character-controller/) uses the generic grid for speed, slope, step height, and gravity factor. Runtime fields such as desired velocity and grounded state are returned by the component DTO, so the Inspector can show them while the Physics panel supplies live test movement.

## In the code

| What | File | Symbols |
|---|---|---|
| Field kinds and physics hints | `editor/src/components/fieldRenderer.tsx` | `FieldKind`, `FIELD_HINTS`, `renderField`, `renderByHint` |
| Enum and axis-lock controls | `editor/src/components/EnumField.tsx`, `editor/src/components/LockAxesField.tsx` | `EnumField`, `LockAxesField` |
| Component sections and fit action | `editor/src/panels/InspectorPanel.tsx` | `InspectorPanel`, `componentBody`, `onFitCollider`, `RIG_ONLY` |
| Kinematic bone mask | `editor/src/components/BoneMaskField.tsx` | `BoneMaskField` |
| Per-bone physics editor | `editor/src/components/BonePhysicsEditor.tsx` | `BonePhysicsEditor`, `BonePhysicsEntry` |
| Collider fitting command | `engine/crates/control/src/commands_physics.rs`, `engine/crates/control/src/selector.rs` | `fit-collider`, `fit_collider` |
| Physics component data | `engine/crates/scene/src/component.rs` | `Rigidbody`, `Collider`, `CharacterController`, `KinematicBones`, `BonePhysicsComponent` |

## Related

- [Inspector](../inspector/) - component ordering, optimistic edits, and undo boundaries
- [Rigidbody and collider](../../physics/rigidbody-and-collider/) - body and shape ownership
- [Collision shapes](../../physics/collision-shapes/) - size packing, fitting, and mesh cooking
- [Physics panel](../physics-panel/) - live play-world diagnostics and controls
- [Ragdoll](../../physics/ragdoll/) - how per-bone physics data builds a simulated rig
