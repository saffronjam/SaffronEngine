# Phase 4 — Model import attaches material references, not inline copies

**Status:** NOT STARTED

Part of `plans/material-instances/`. Close the fork at its source: a model import must produce entity
slots that **reference** the baked `.smat` material assets by id, instead of copying their parameters
inline. Depends on Phase 3 (the reference+override slot shape).

## Why

`AssetServer::bake_model` (`engine/crates/assets/src/import.rs`) already bakes each glTF/OBJ material as a
`.smat`-shaped chunk in the `.smodel` container and records an `AssetType::Material` catalog row. But
`apply_imported_materials` / `instantiate_model` (`engine/crates/assets/src/spawn.rs`) reads those back
and **copies the params inline** into a `MaterialSet`, keeping no back-reference. That inline copy is the
exact moment the entity's material and the `.smat` asset diverge — and it is what makes editing a `.smat`
have no effect on imported models. Phase 3 removed the inline fields, so this copy path no longer even
type-checks; this phase replaces it with reference-attach.

## Goal

- Imported materials are addressable `.smat` assets with stable ids (either standalone
  `assets/materials/<uuid>.smat` files, or container sub-assets with catalog ids that a slot can
  reference — pick the representation a `MaterialSet` slot's `material: Uuid` can point at, and make the
  catalog resolve it in `render_material.rs`).
- `apply_imported_materials` / `instantiate_model` attaches a `MaterialSet` whose slot `material` ids
  reference those baked assets, submesh-indexed, with empty `overrides`.
- A built-in **default `.smat`** exists and is referenced when a submesh has no source material, so a slot
  always references a valid material (no inline-fallback path).
- Re-import updates the referenced `.smat` assets in place, so live instances pick up the change (aligns
  with the existing `ModelInstance` reimport-finds-live-instances design).

## NO-LEGACY checklist

- The inline-copy branch in `apply_imported_materials` / `instantiate_model` is deleted, not guarded — no
  code path produces an inline-param slot anymore.
- Every spawned/imported entity references a material asset per slot; grep for inline material
  construction in `spawn.rs` returns nothing.

## Changes (sketch)

**`engine/crates/assets/src/import.rs`.** Ensure `bake_model` produces material assets with ids a slot
can reference (standalone `.smat` or catalog-addressable container sub-assets). Ship
`default_material_asset` (`material.rs`) as an installed built-in with a fixed well-known id.

**`engine/crates/assets/src/spawn.rs`.** Rewrite `apply_imported_materials` to attach reference slots;
delete the inline-param population. Map each submesh's source-material index to the corresponding baked
asset id.

**`engine/crates/assets/src/render_material.rs` / `catalog.rs`.** Make slot `material` ids resolve
through the catalog to the baked asset (including container sub-assets, if that is the chosen
representation).

**e2e / project smoke.** Update `tools/check-projects` and the import e2e tests to assert an imported
model's entity carries reference slots (non-zero `material` ids), and that editing a referenced material
changes the imported instance's rendered look.

## Verification

- `just engine` + `just prepare-for-commit`; `just e2e`; `tools/check-projects` smoke green.
- Import a multi-material glTF; confirm the entity's `MaterialSet` slots reference distinct `.smat` ids
  and the material editor opens the referenced asset; edit it and see the instance update.
