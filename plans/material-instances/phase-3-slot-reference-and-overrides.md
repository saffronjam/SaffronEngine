# Phase 3 — The entity slot references a material asset + sparse overrides

**Status:** COMPLETED

**As built:** `MaterialSlot` is now `{ material: Uuid, overrides: serde_json::Value }`
(`engine/crates/scene/src/component.rs`); the `Material` and `MaterialAsset` scene components are
deleted; `MaterialSet` is the one per-entity material component (a single-material mesh is one slot).
Serde (`scene/serde.rs`), the registry (`registry.rs`, `BUILTIN_COMPONENT_NAMES`), and the protocol
`component_schemas` (`protocol/schema.rs`) are updated; `resolve_entity_materials`
(`assets/render_material.rs`) is one path — resolve each slot's referenced `.smat` (parent-chain,
default fallback) + `apply_overrides`, submesh→slot clamped, whole-mesh flags/shader from slot 0. The
delete-asset cascade, residency prewarm, `assign-asset` (texture slots → slot-0 overrides), and
`material-assign` (points every slot's `material`) are migrated in `saffron-control`. All engine unit
tests + the `.smat` golden pass; the component byte-golden was updated to the new shape. The ORM
divergence is resolved (the slot/override texture set is the packed `ormTexture`, matching the `.smat`).

Part of `plans/material-instances/`. This is the model cutover: the entity stops storing inline PBR
params and instead **references** a `.smat` material asset per submesh, with an optional **sparse
override** map. The three material components collapse toward one reference+override shape, and
`resolve_entity_materials` is rebuilt to resolve `asset defaults → instance overrides → slot overrides`.
Depends on Phase 2 (the exposed-parameter schema the overrides validate against).

## Why

Today `Material` and `MaterialSlot` (`engine/crates/scene/src/component.rs`) are byte-identical inline
PBR blobs, and `MaterialAsset { material: Uuid }` is a separate component that *shadows* them. Three
components, two data shapes, one silently overriding the other (`resolve_entity_materials` precedence
`MaterialAsset → MaterialSet → Material`). The clean model has **one** shape: a submesh-indexed slot
that references an asset and sparsely overrides it — Unity `sharedMaterials[]` + `MaterialPropertyBlock`,
Unreal material-slot array + Material Instance, Godot per-surface Material Override.

## Goal

- `MaterialSlot` becomes `{ material: Uuid, overrides: SparseMap }` — a reference plus a sparse map keyed
  by exposed-parameter name (validated against the referenced material's Phase-2 schema). A `0` /
  missing `material` resolves to the built-in default `.smat` (Phase 4 ships it).
- **Delete** the inline PBR fields from `MaterialSlot` and `Material`. There is no inline parameter blob
  on an entity anymore.
- Collapse the three components toward the single reference model: a single-material mesh is a
  `MaterialSet` with one slot (or a thin `Material { material, overrides }` that is literally a one-slot
  set — pick one and delete the other; prefer **one** component, `MaterialSet`, with the single-material
  case being `slots.len() == 1`). The standalone `MaterialAsset { material }` component is subsumed by a
  slot's `material` reference and is deleted.
- `resolve_entity_materials` resolves each submesh's slot to a `SubmeshMaterial` by:
  `referenced material's resolved values (defaults ← parent chain ← instance overrides)  ←  slot.overrides`.

## NO-LEGACY checklist

- No inline PBR field (`base_color`, `metallic`, … , `blend_mode`, `double_sided`, the texture ids)
  survives on `MaterialSlot` / `Material` — grep returns nothing.
- Exactly one entity material component shape remains (`MaterialSet` of reference+override slots). The
  redundant `Material` and `MaterialAsset` components are deleted, every reader migrated in the same
  change (registry, serde, resolve, control commands, editor, e2e).
- The precedence ladder in `resolve_entity_materials` is gone — there is one resolution path (slot →
  referenced material + overrides), not a three-component fallback.

## Changes (sketch)

**`engine/crates/scene/src/component.rs` + `serde.rs` + `registry.rs`.** Redefine `MaterialSlot`;
delete inline fields; delete `Material` and `MaterialAsset` (or repurpose per the "one component"
decision above). Update `material_slot_to_json` / `material_slot_from_json` to (de)serialize
`{ material, overrides }`. `overrides` is opaque, sparse, editor-shaped JSON like `ScriptSlot.overrides`
already is (a good local precedent — see `ScriptSlot` in the same file).

**`engine/crates/assets/src/render_material.rs`.** Rewrite `resolve_entity_materials` /
`build_submesh_material` / `lower_slot` around the single reducer from Phase 2, applying `slot.overrides`
as the final layer. Remove the component-precedence comment/logic.

**`engine/crates/control/src/commands_scene.rs` / `commands_asset.rs`.** The slot override write is the
generic `set-component` full-DTO path from Phase 1 (writing `{ slots: [{ material, overrides }, …] }`),
or a dedicated sparse `set-material-override {entity, slot, field, value}` if per-key writes are cleaner
for the editor — decide based on the Phase 5 UI, but keep exactly one path. `material-assign` becomes
"set slot `material` reference" (wired to UI in Phase 5).

**Protocol + editor + e2e.** Regenerate `@saffron/protocol`; update `InspectorPanel.tsx` component
handling (full override editor is Phase 5, but the component must render without crashing here); update
`componentOrder.ts` (`COMPONENT_ORDER` / `NON_ADDABLE`) for the collapsed component set; migrate e2e
tests that construct `Material` / `MaterialSet` bodies.

## Verification

- `just engine` + `just prepare-for-commit`; `bun run check`; `just e2e`.
- A scene with a slot referencing material A renders A's look; setting a sparse `baseColor` override on
  the slot changes only that entity; editing material A propagates to all referencing entities.
- Golden scene JSON snapshot updated to the reference+override shape.
