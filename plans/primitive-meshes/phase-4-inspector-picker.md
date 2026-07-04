# Inspector mesh picker: "Built-ins" group + "Built-in" chip

**Status:** NOT STARTED
**Scope:** editor (`AssetPicker`, `fieldRenderer`), `saffron-control`
**Depends on:** phase-2 (reserved ids exist), phase-3 (native spawn)

## Goal

The Inspector's mesh instance selector lists sphere/cube/plane as a fixed **"Built-ins"** group above
the catalog rows, and renders a **"Built-in" chip** on the trigger when the selected value is a reserved
id (`< 1024`) — never referencing a fake asset. `list-assets` stays catalog-only.

## Touch points

- **`editor/src/components/fieldRenderer.tsx`** — `"Mesh.mesh"` maps to
  `{ kind: "uuid", asset: "mesh" }` rendered as `<AssetPicker assetType="mesh">`. No mapping change.
- **`editor/src/components/AssetPicker.tsx`** — for `assetType === "mesh"`, inject a fixed **Built-ins**
  group (Cube/Plane/Sphere, sourced from a TS constant mirroring `BuiltinMesh` — *not* the catalog)
  above the catalog-filtered list. Render the trigger swatch/label as a **"Built-in" chip** when
  `value < 1024`. Reuse the existing per-type filter for the catalog rows.
- **`control/src/commands_asset.rs`** `assign-asset` — the Inspector writes `Mesh.mesh` via
  `assign-asset` (slot `"mesh"`), which resolves id → display name through the catalog. Add a built-in
  branch: a reserved id (`BuiltinMesh::from_reserved_id`) yields `display_name()` and skips the catalog
  lookup, so the Inspector shows "Cube", not an empty name.
- **`store.ts`** `refreshAssets` / `list-assets` — unchanged (catalog-only). Built-ins are injected
  client-side by the picker; they never appear in the Assets grid.

## Verification

- The mesh picker shows Cube/Plane/Sphere at the top; selecting one sets `Mesh.mesh` to `3/4/5` and the
  trigger shows the name + a "Built-in" chip.
- The Assets grid still lists only catalog assets (no primitives).
- Selecting a catalog mesh still works unchanged.

## Notes

- The `< 1024 → BuiltinMesh` check now exists in two languages (Rust `from_reserved_id`, a TS constant).
  Keep each side's decoder in one place so the rule is centralized per side.
