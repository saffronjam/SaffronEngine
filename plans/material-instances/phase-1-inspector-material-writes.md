# Phase 1 — Unify material writes on `set-component`; fix the reverting fields

**Status:** NOT STARTED

Part of `plans/material-instances/` (entity materials reference `.smat` assets). This is the first,
**self-contained** phase and the one the user is actively blocked by: material parameters typed or
dragged in the Inspector silently revert. It fixes today's model in place and lands the terminal write
path (`set-component`) that the later phases keep — so no work here is thrown away.

## The bug, precisely

`set-material` writes through `SetMaterialParams` (`engine/crates/protocol/src/dto.rs`), which declares
only eight parameter fields — `base_color, albedo_texture, metallic_roughness_texture, metallic,
roughness, emissive, emissive_strength, unlit` — plus `entity`, `slot`, `smooth`. It has **no**
`#[serde(deny_unknown_fields)]`, so any other key sent by the editor is silently dropped by serde before
the handler sees it. The handler still succeeds and bumps `scene_version`; the editor's reconcile poll
(`store.ts`) then re-reads the component, gets the unchanged value, and overwrites the optimistic
overlay. The edit "reverts on blur" with no error toast.

Both inspector write routes feed this DTO (`editor/src/panels/InspectorPanel.tsx`):

- **Standalone `Material`** — `applyWrite` sends non-texture `Material` fields via
  `client.setMaterial(id, { [field]: … }, smooth)`. Dropped: `heightScale`, `normalStrength`,
  `alphaCutoff`, `doubleSided`, `blend`. (Its four extra textures survive by luck via the
  `setComponentField` fall-through; `albedoTexture`/`metallicRoughnessTexture` via `assignAsset`.)
- **`MaterialSet` slots** — `onSlotFieldChange` → `slotCoalescerFor(slotIndex, field)` →
  `client.setMaterial(id, { [field]: … }, smooth, slotIndex)`. This routes **every** field through the
  8-field DTO, so on top of the scalars/bools it also drops the `normal`/`occlusion`/`emissive`/`height`
  texture assignments (slots never use `assignAsset`).

Two smaller, related UI defects ride along:

- `FIELD_HINTS["Material.alphaClip"]` (`editor/src/components/fieldRenderer.tsx`) is **dead** — the
  serializer emits `blend` (a `BlendMode` string), never `alphaClip`. There is **no**
  `FIELD_HINTS["Material.blend"]`, so `blend` falls through to a raw text `Input`.
- The `component_schemas` "Material" schema (`engine/crates/protocol/src/schema.rs`) has
  `additionalProperties: false` but omits `doubleSided` (always emitted by `material_slot_to_json`) and
  `blend`; strict validation would reject a real component body.

## Goal

Route **all** inspector material-parameter writes — standalone `Material` and every `MaterialSet` slot,
every field type (scalars, colors, bools, enums, texture uuids) — through the generic full-DTO
`set-component` command, and **delete** `set-material` / `SetMaterialParams` entirely. Add the `blend`
field hint, delete the dead `alphaClip` hint, and complete the "Material" component schema.

The generic path already works for every other structured component and reads the whole DTO via the
registry deserializer `material_slot_from_json` (which reads **every** field), so all fields round-trip
with no per-field allow-list to drift.

## NO-LEGACY checklist for this phase

- Zero references to `set-material` / `SetMaterialParams` survive tree-wide (engine, protocol, `sa` CLI,
  `client.ts`, tests, `schemas/control/*` regenerated). The command is deleted, not deprecated.
- Exactly one command writes material parameters afterwards: `set-component`. The `Material` special
  branch in `applyWrite` and the entire `slotCoalescerFor`/`recordSlotEdit`/`onSlotFieldChange` bespoke
  slot machinery are removed in favour of the generic `onFieldChange`/`coalescerFor` path (adapted for
  the `slots[i]` nesting — see below).
- `FIELD_HINTS["Material.alphaClip"]` is gone; `FIELD_HINTS["Material.blend"]` exists as an enum select.

## Design decision: smoothing

`set-material` (like `set-transform`) supports `smooth` — animate numeric fields toward the target over
~25 ms instead of snapping. `set-component` does a hard set. Deleting `set-material` therefore drops the
~25 ms tween on material scalar drags.

**Decided (accepted by the project owner): dropping the material-scalar tween is fine — proceed.** The
`MaterialSet` slot path already never smoothed (the `set-material` slot branch ignores `smooth` by
design), so slots gain nothing to lose; standalone `Material` loses only a barely-perceptible tween.
Per-parameter smoothing, if wanted later, belongs as a *generic* capability of the component write (a
`smooth` flag on `set-component` that the resolver interpolates for numeric leaves) — **not** a
resurrected per-material DTO with a hand-listed field set, which is exactly the drift that caused this
bug. Do not re-introduce `set-material` to preserve smoothing.

`set-transform` is out of scope: Transform has three fields and is not part of the material mess; leave
its `smooth` path as-is (a later cleanup may generalize smoothing across `set-component`).

## Engine + protocol changes

**`engine/crates/protocol/src/dto.rs`.** Delete `SetMaterialParams` and its entry in the command list
(`command.rs` — the `"set-material"` summary registration and any `CommandName` inclusion). Regenerate
protocol artifacts afterwards.

**`engine/crates/control/src/commands_scene.rs`.** Delete the `set-material` command registration and
its handler (both the whole-`Material` branch and the `MaterialSet` slot branch, including the
`"material slot {n} out of range"` path). Confirm `set-component` already deserializes a full `Material`
/ `MaterialSet` body through the registry — it does (`material_slot_from_json` reads every field); no new
handler is needed. `set-component` is a read-modify-write of the whole component, so the editor sending
the full patched DTO is correct.

**`engine/crates/protocol/src/schema.rs`.** In `component_schemas`, add `doubleSided` (boolean) and
`blend` (string enum `opaque|masked|translucent`) to the "Material" property set so the
`additionalProperties: false` schema accepts a real body. The "MaterialSet" schema references "Material"
per slot, so it inherits the fix.

**Tests.** Remove/replace any test that drove `set-material` (search `commands_scene.rs` tests,
`tests/e2e/*`, `engine/crates/protocol/tests/schema_fragments.rs`, `tools/check-control-schema`). Add an
e2e assertion that a `set-component` write of a full `MaterialSet` slot body with a non-default
`heightScale` / `normalStrength` / `alphaCutoff` / `doubleSided` / `blend` and a `normalTexture` uuid
**round-trips** (write, re-read, assert equal) — this is the regression guard for the reported bug.

## Editor changes (`editor/`)

**`editor/src/control/client.ts`.** Delete the `setMaterial` wrapper. Keep `setComponent`,
`setComponentField`, `assignAsset`, `materialAssign`.

**`editor/src/panels/InspectorPanel.tsx`.**

1. In `applyWrite`, delete the `if (component === "Material") return client.setMaterial(...)` branch so
   `Material` falls through to `return client.setComponent(id, component, dto)` like every other
   component. Keep the texture-uuid handling, but see (3) on whether inspector texture edits should also
   move to `set-component`.
2. Replace the bespoke slot machinery (`slotCoalescerFor`, `recordSlotEdit`, and the `setMaterial` calls
   inside `onSlotFieldChange`/`onSlotFieldDragEnd`) with the generic component write. The generic
   `onFieldChange` already builds a patched full DTO and pushes it through `coalescerFor` →
   `sendWrite` → `applyWrite` → `client.setComponent`. For slots, `onSlotFieldChange` already builds the
   **whole** patched `slots` array (`applyOptimisticComponent("MaterialSet", { slots })`); route its
   coalesced send to `client.setComponent(id, "MaterialSet", { slots })` instead of the per-field
   `setMaterial`. The coalescer key can stay `MaterialSet#${slotIndex}.${field}` so distinct fields
   coalesce independently, but each send now transmits the full component DTO (read-modify-write), which
   `set-component` expects. Undo entries (`recordSlotEdit`) replay the same full-DTO `setComponent`.
3. **Texture edits.** For consistency (one write path), inspector texture-field edits on a `Material` or
   a slot should also go through the full-DTO `set-component` (the uuid is just another field the
   registry deserializer reads; textures are loaded lazily at `resolve_entity_materials`, so no
   assign-time validation is required for the value to persist). Keep `assignAsset` **only** for the
   drag-an-asset-directly-onto-an-entity auto-attach flow (`ensure_material` attaches a `Material` when
   dropping an albedo texture onto an entity with none — see `assign_asset_albedo_attaches_material` in
   `commands_asset.rs`); that flow is not an inspector field edit. Verify whether removing the
   `assignAsset` special-cases from `applyWrite` regresses the inspector's albedo/MR drop target — if the
   inspector relies on `assignAsset` to auto-create the component, keep that single case; otherwise route
   all inspector texture edits through `set-component`.

**`editor/src/components/fieldRenderer.tsx`.** In `FIELD_HINTS`: delete `"Material.alphaClip"`; add
`"Material.blend": { kind: "enum", options: ["opaque", "masked", "translucent"] }` (match the existing
enum-hint shape used elsewhere — check how e.g. a `ScriptSlot` enum or collider-shape enum renders and
mirror it). The same hint keys serve `MaterialSet` slots (the inspector keys slot fields under the
`Material` field namespace via `humanizeFieldName`/`resolveHint` — confirm the hint lookup uses the
`Material.` prefix for slot fields and, if not, add the parallel key).

## Verification

- `just engine` + `just prepare-for-commit` clean.
- `editor/`: `bun run check` (regenerates `@saffron/protocol` — `CommandName` must no longer contain
  `set-material`), `bun run lint`.
- `just e2e` green, including the new round-trip assertion above.
- **Manual (the user's repro), per `editor/AGENTS.md` "log then ask":** in `just run`, select a
  multi-material imported model, and confirm every field in a slot now sticks — type `heightScale` and
  `normalStrength`, drag `alphaCutoff` and `metallic`, toggle `doubleSided`, drop a normal-map texture on
  the slot's normal field, pick a `blend` value from the new select. All persist across unfocus and the
  next reconcile poll. Repeat on a single-material entity's `Material` component.

## Out of scope (later phases)

The inline PBR fields themselves are removed in Phase 3 (the slot becomes `{ material, overrides }`) and
the inspector becomes an override editor in Phase 5. This phase does **not** touch the data model — it
only makes today's inline fields writable through the terminal command. The steppiness of the
`metallic`/`roughness` slot drag (no interpolation) is the accepted no-smoothing behavior above, not a
bug to fix here.
