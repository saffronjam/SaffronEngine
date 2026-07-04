# Material-graph live preview — a mini "spin-the-sphere" 3D pane

**Status:** NOT STARTED

Replace the static PNG preview in the material-graph editor with a **live, orbitable 3D sphere** showing
the edited `.smat` under real IBL — a mini version of the asset "View" tab embedded in the graph editor.

## Key design decision — reuse the modal AssetPreview view (no concurrent view)

The research initially assumed this needed a *concurrent* third live view (the host renders exactly one
view per frame — verified: single `active_view`, single-slot shm publish). **It does not.** The
material-graph editor is a **full-work-area main tab** (`App.tsx` `MaterialGraphWorkspace`, gated on
`activeKind === "materialGraph"`) — when it is open, the main scene viewport is parked anyway. So the
preview is never concurrent with the scene; **only one preview is ever live at a time**, exactly like
the asset "View" tab.

Therefore: **reuse `ViewId::AssetPreview` + the existing modal `preview_scene`**, furnished with a
sphere carrying the edited material, presented through the `assetPreview` subsurface bound to the graph
editor's preview pane, with **pan-only orbit (no zoom)** — just like the model viewer, minus zoom. No
third view slot, no scheduler, no concurrent-multi-view machinery, no extra shm segment. The whole risky
part of the original dossier is avoided.

Scope note: only the **full-tab material-graph editor** gets the live sphere. The small dockable
Material *panel* (`MaterialEditorPanel`) — which can sit next to the scene viewport — keeps its static
thumbnail, since giving *it* a live 3D pane would reintroduce the concurrent-view problem for no real
gain.

## How it composes with the sibling plans

The "material asset on a sphere in `enter-asset-preview`" subject is built in
`texture-material-previews/phase-3` (the furnisher `&mut Scene` refactor + the material/texture subject
branch). This plan **reuses** that: the graph editor calls the same `enter-asset-preview` path for the
`.smat` under edit, so materials also gain a first-class "View" tab for free. The unique work here is the
**editor embedding** (subsurface pane in the graph tab, pan-only camera, view wiring) and the **PNG
cutover**.

## Diagnosis (current code)

- Preview pane is a static `<img src="data:image/png;base64,…">` (`MaterialGraphEditor.tsx`) fed by
  `client.previewRender(materialId, 256)` — a synchronous one-shot offscreen render on the studio
  sphere (`preview.slang`, fixed 2-light, **no IBL**), cached in a module-level `previewCache`,
  refreshed from a 500 ms debounced auto-apply.
- The live-view machinery (`ViewId::AssetPreview`, its `ViewTarget` + shm segment + subsurface,
  `AssetEditorWorkspace` orbit loop, `useSubsurfaceBounds`) already exists and is exactly what we bind
  to the pane.

## Phases

1. **`phase-1-material-preview-subject.md`** — material-asset-as-sphere subject in `enter-asset-preview`
   + live reflection of graph edits (builds on the shared furnisher refactor).
2. **`phase-2-editor-pane-and-camera.md`** — subsurface pane in the graph tab; pan-only orbit; extend
   `activeRenderView`/park wiring so `materialGraph` drives the `assetPreview` view; enter/exit on
   mount.
3. **`phase-3-png-cutover.md`** — replace the `<img>`, rewire the debounce to `materialSetGraph`-only,
   delete `previewRender`/`previewCache` from this tab.
4. **`phase-4-polish-and-keep-current.md`** — exposure control; `sa`; docs; (stretch) shape toggle.

## Dependencies

- **`primitive-meshes/`** — the spawnable sphere.
- **`texture-material-previews/phase-3`** — the furnisher `&mut Scene` refactor + the material-on-sphere
  `enter-asset-preview` subject.
