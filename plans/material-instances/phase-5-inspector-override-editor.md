# Phase 5 — The inspector becomes a material override editor

**Status:** NOT STARTED

Part of `plans/material-instances/`. With the entity model now "slot references a `.smat` + sparse
overrides" (Phase 3) and imports producing references (Phase 4), the Inspector's material section becomes
an **override editor**, Unreal-Material-Instance style: it shows the referenced material's exposed
parameters, marks which are overridden, and edits write into `slot.overrides`. Depends on Phases 3 and 4.

## Why

Today the inspector renders a flat wall of inline PBR fields with no relationship to the material graph —
the source of the user's "these feel completely separated" and "we have a material builder *and*
material-tweaks in components?" confusion. The clean UI makes the two verbs explicit: **tweak this
object** (write a sparse override on the slot) vs **edit the material** (open the `.smat` graph, changing
it for every instance).

## Goal

- **Per-slot material picker.** Each slot shows its referenced `.smat` (name + thumbnail) with a picker to
  reassign it (writes the slot's `material` id; replaces the orphaned `material-assign` command with a
  first-class UI caller). Uses the existing `AssetPicker` (`editor/src/components/AssetPicker.tsx`) and
  material thumbnails.
- **Exposed-parameter override rows.** For the referenced material, fetch its exposed-parameter schema
  (Phase 2's `material-schema` / `material-get`) and render one row per parameter. A row shows the
  effective value (default/instance, or the override when set) with an **"overridden" marker** and a
  revert affordance. Editing writes into `slot.overrides` (sparse — only changed params); reverting
  removes the key. Reuse the existing field renderers (`fieldRenderer.tsx`, `NumberDrag`, `SliderField`,
  `ColorField`, `EnumField`, texture `AssetPicker`) driven by the exposed-parameter `type`.
- **"Edit material" button** opens the `.smat` in the material graph tab (`openMaterialGraph…` /
  `MaterialGraphEditor.tsx`), making clear that graph edits are shared across instances.
- All writes go through the single override write path from Phase 3 (generic `set-component` full-DTO, or
  the dedicated sparse override command) — optimistic-local + coalesced, matching every other field.

## NO-LEGACY checklist

- No inline-PBR-field rendering remains in `InspectorPanel.tsx` — the material section renders exclusively
  from the referenced material's exposed schema + the slot's sparse overrides. The bespoke
  `onSlotFieldChange`/`slotCoalescerFor` inline machinery (already reduced in Phase 1, repurposed in
  Phase 3) is gone in favour of the override rows.
- The dead `FIELD_HINTS` PBR entries that only made sense for inline fields are removed; hints are driven
  by the exposed-parameter `type`, not a per-field-name table.
- One and only one UI path assigns a slot's material reference (the picker); `material-assign` has a
  caller or is deleted in favour of the slot write.

## Changes (sketch)

**`editor/src/panels/InspectorPanel.tsx`** (and likely a new `MaterialSlotEditor.tsx` component under
`editor/src/components/`). Render the picker + override rows + "Edit material". Wire the overridden-marker
state from `slot.overrides` vs the fetched schema defaults.

**`editor/src/control/client.ts`.** Typed wrappers for the material-schema read and the slot
material-reference/override writes.

**Docs.** Update the material docs page: the entity-material workflow (reference a material, override
sparsely per object, or edit the shared material), and a short "how a MaterialSet slot maps to a
submaterial" note answering the original confusion (slots are submesh-indexed and import-managed).

## Verification

- `bun run check` / `bun run lint`; `just e2e`.
- **Manual (per `editor/AGENTS.md`):** select an imported multi-material entity; each slot shows its
  referenced material + thumbnail; override `baseColor` on one slot (marker appears, only that object
  changes, revert clears it); reassign a slot to another `.smat` via the picker; "Edit material" opens the
  graph and a graph edit updates every instance referencing it.

## Done state for the whole plan

After Phase 5: one entity material model (reference + sparse override), one parameter schema (the `.smat`
exposed list), imports that reference rather than copy, an inspector that cleanly separates "tweak this
object" from "edit the material", and no `set-material` / inline-PBR-blob path anywhere. Mark each phase
`COMPLETED` as it lands and update the repo `AGENTS.md` **Status** material line to describe the
reference+override entity model. Delete a phase file only once it is `COMPLETED` and no longer a useful
record (per the `plans/` convention, prefer marking `COMPLETED` over deleting while the feature is fresh).
