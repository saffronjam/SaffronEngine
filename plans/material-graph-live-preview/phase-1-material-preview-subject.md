# Material asset as a sphere subject + live graph-edit reflection

**Status:** NOT STARTED
**Scope:** `saffron-control`, `saffron-sceneedit`
**Depends on:** **`primitive-meshes/`**, **`texture-material-previews/phase-3`** (furnisher `&mut Scene`
refactor)

## Goal

`enter-asset-preview` accepts a **material (`.smat`) asset** as a subject, furnished as a built-in
sphere carrying that material, into the modal `ViewId::AssetPreview` — and the sphere reflects live
edits to the material. This gives both the material-graph editor's live pane and a standalone material
"View" tab, on one code path.

## Touch points

- **`enter-asset-preview` (`control/src/commands_asset.rs`)** — extend the subject branch (added for
  textures in `texture-material-previews/phase-3`) to accept a `Material` asset: spawn one
  `Mesh { mesh: BUILTIN_SPHERE_MESH_ID }` bound to a `MaterialSet` whose slot 0 references the `.smat`
  id, via the refactored furnisher (floor + `DirectionalLight` + `SkyMode::Procedural` + framed orbit).
  Because the entity references the material **by id**, a subsequent `material-set-graph` /
  `material-update` mutates that `.smat` and the sphere re-renders next frame — no re-instantiation.
- **`routeView`** — a material asset "View" opens this sphere tab (materials currently have no 3D tab
  at all).

## Verification

- Opening a `.smat` in "View" shows it on an orbitable IBL sphere (real lighting, not the 2-light PNG).
- Editing the material (factors or graph) updates the sphere live without re-entering the preview.

## Notes

- This is the join point: the material-graph editor (this plan, phase-2/3) and the material "View" tab
  are the same host path with different editor framing. One furnisher, one subject branch.
