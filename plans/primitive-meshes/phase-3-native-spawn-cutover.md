# Native primitive spawn + delete the fake-asset path

**Status:** NOT STARTED
**Scope:** `saffron-protocol`, `saffron-control`, `saffron-assets`, editor (`CreateMenu`)
**Depends on:** phase-2 (seeded reserved-id meshes)

This is the **cutover** — the old cube-model-asset path is removed here, not left running alongside.

## Goal

`add-entity {preset: cube|plane|sphere}` spawns a native entity carrying `Mesh { mesh: <builtin id> }`
+ a default `MaterialSet`, with **no project required** and **no catalog mutation**. The
`ensure_builtin_model_asset` path is deleted.

## Touch points

- **`protocol/src/dto.rs`** — `AddEntityPreset`: replace `Cube | Model` with `Cube | Plane | Sphere`.
  Redesign the `Model` preset (below). Run `xtask gen-protocol` / `bun run check`.
- **`control/src/commands_scene.rs`** `add-entity`: replace the `Cube | Model` arm with a native path:
  create entity → `assets.ensure_builtin_meshes(gpu)` → add `Mesh { mesh: BuiltinMesh::_.reserved_id() }`
  + default `MaterialSet { slots: vec![MaterialSlot::default()] }` (slot 0 → `DEFAULT_MATERIAL_ID`) +
  a name (`"Cube"`/`"Plane"`/`"Sphere"`). **Drop the `project_ready()` gate** for primitives.
- **`assets/src/import.rs`** — **delete** `ensure_builtin_model_asset`. Confirm no non-test caller
  remains (the preset arm was the only one).
- **`editor/src/panels/CreateMenu.tsx`** — Cube / Plane / Sphere menu entries → `client.addEntity(...)`.
- **`models/cube.gltf`** — decide: remove, or retain purely as a test fixture. It is no longer baked
  into any project catalog.

## The `Model` preset

Currently `Model` is a behaviour-less alias for Cube. NO-LEGACY forbids leaving a second path that
duplicates the primitive spawn. Redesign it as a real **"instantiate a chosen model asset"** command
(takes a model-asset id, calls `instantiate_model` into the authored scene) — this is the correct home
for "add a catalog model to the scene", distinct from primitives — **or** remove the preset entirely if
the asset-browser "Add to scene" flow already covers it (it does: `onInstantiate` →
`instantiateModel`). Prefer removal unless a menu-driven "add model…" entry is wanted.

## Verification

- Cube/Plane/Sphere spawn with **no project loaded** (regression on the old gate).
- The Assets panel shows **no** new `cube`/`Cube.mesh`/`Cube.material` rows after spawning a cube
  (the whole point).
- Spawned primitive serializes as `"mesh": "3"|"4"|"5"`, round-trips through save/load, renders on
  reload.
- `just engine` + `just prepare-for-commit` clean.

## Risks

- **Scene-load validation** must accept a reserved-but-unknown mesh id (`< 1024`) — verify no loader
  path rejects `"mesh":"3"` as a missing asset. It should resolve cache-first to the seeded built-in.
