# Interactive 3D texture viewer + representation picker

**Status:** NOT STARTED
**Scope:** `saffron-control`, `saffron-sceneedit`, `saffron-protocol`, editor (`AssetsPanel`,
`AssetEditorWorkspace`, `App`)
**Depends on:** phase-1, phase-2, **`primitive-meshes/`** (spawnable sphere)

## Goal

A texture opens the real-renderer 3D "View" tab as a lit sphere carrying a synthesized single-slot
material, orbitable like a model. The flat `<img>` is demoted to a picker mode. A representation picker
(Applied / Flat / per-type toggles) ships here.

## Touch points

- **Furnisher refactor (shared prerequisite).** `furnish_preview_scene`, `compute_preview_bounds`,
  `spawn_preview_floor` (`control/src/commands_asset.rs`) are hardwired to
  `ctx.scene_edit.preview_scene`. Refactor them to take a `&mut Scene` (+ needed context) so they can
  furnish an arbitrary subject. **This same refactor is required by
  `material-graph-live-preview/phase-1`** — land it here, have that plan depend on it.
- **`enter-asset-preview` (`control/src/commands_asset.rs`)** — add a branch: for a `Texture` with a
  non-HDRI role, skip `instantiate_model`; spawn one `Mesh { mesh: BUILTIN_SPHERE_MESH_ID }` entity
  bound to a synthesized single-slot `MaterialSet` whose slot is chosen by `role`; reuse the refactored
  furnisher. Add a per-type representation field to the enter-preview params (default per role;
  overridable by the picker).
- **`routeView` (`editor/src/panels/AssetsPanel.tsx`)** — texture (any non-HDR role) →
  `openAssetEditorForAsset` (3D tab). Per NO-LEGACY, the standalone `openImageViewerTab` route is
  *replaced*; the flat image survives only as the picker's "Flat channel" mode inside
  `AssetEditorWorkspace`. Update all `routeView` callers (double-click + context "View").
- **`AssetEditorWorkspace.tsx`** — add the representation picker: **Applied** (lit ball, default) /
  **Flat channel** (the raw texture — the old image-viewer view) / per-type toggles (normal GL/DX flip,
  ORM R/G/B split strip). Drive the choice into the enter-preview representation field.
- **`sa` / `saffron-control`** — a matching control command keeps it scriptable.

## Verification

- Double-clicking a texture opens the 3D orbit tab with the map on a sphere in its correct role.
- The picker toggles Applied ↔ Flat ↔ per-type; GL/DX flip corrects an inverted normal (bumps↔dents).
- No standalone flat image-viewer tab remains; `openImageViewerTab` is gone (or is the picker's Flat
  mode only).

## Risks

- The furnisher refactor touches the asset-preview path used by models today — regression-test model
  "View" still works after it becomes scene-parameterized.
