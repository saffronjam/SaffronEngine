# Editor pane + pan-only orbit in the material-graph tab

**Status:** IMPLEMENTED. The `AssetEditorWorkspace` orbit machinery (eased target→current rAF loop,
coalesced `set-camera`, pointer/wheel handlers, framed-pose seed, joint-pick-on-click) was lifted into
`editor/src/lib/useOrbitCamera.ts` (`useOrbitCamera({ enableZoom, onClick? })`) — one orbit
implementation for both consumers; the asset editor now calls it (zoom on for models / the HDRI rig,
off for a lone texture sphere; `onClick` = joint pick only when rigged). The material-graph editor's
preview pane became a `hostRef` transparent hole + `useSubsurfaceBounds(hostRef, "assetPreview", {
enabled: previewReady })`, a **pan-only** orbit (`enableZoom: false`), and enter/exit on mount/unmount
(the tab renders only while active, so mount == activate). `App.tsx` `activeRenderView` + `assetParked`
now treat `materialGraph` as a preview-bearing tab (both it and `assetEditor` drive the single modal
`assetPreview` view). The two are a **modal swap, never co-resident owners** of `preview_scene`: the
kept-mounted asset editor is released (`mountedAssetId → null`) when a material-graph tab becomes
active, so exactly one preview subject is ever live. The `MaterialGraphWorkspace` wrapper + editor root
dropped their `bg-background` so the hole's ancestor chain is transparent; toolbar / ReactFlow panel /
Preview header paint their own opaque bg. Verified: `tsc` + `oxlint` (0 new warnings) + editor `build`
+ workspace build clean.
**Scope:** editor (`MaterialGraphEditor`, `App`, a reusable orbit hook)
**Depends on:** phase-1

## Goal

The material-graph editor's preview pane becomes the live `assetPreview` subsurface, orbitable with a
**pan-only** camera (rotate around the sphere; **no zoom**), presented only while the graph tab is
active.

## Touch points

- **`MaterialGraphEditor.tsx`** — the preview pane becomes a `hostRef` subsurface hole +
  `useSubsurfaceBounds(hostRef, "assetPreview", { enabled: tabActive && ready })`. On mount/activate,
  call `enter-asset-preview` for the edited material (phase-1); on unmount/deactivate, `exit-asset-
  preview`. Key the mount by material id so switching materials re-enters.
- **Camera loop** — lift the `AssetEditorWorkspace` orbit loop (rAF ease + `cameraCoalescer` →
  `client.setCamera`) into a small reusable hook. For this pane, **disable zoom** (drop the wheel→
  distance handler); keep drag→orbit (and optionally drag→pan). Distance stays fixed at the framed
  value from phase-1.
- **`App.tsx`** — extend `activeRenderView`: `activeKind === "materialGraph"` → `"assetPreview"` (today
  only `assetEditor` maps there). The existing park effect then parks the scene view and activates
  `assetPreview` for the material-graph tab, same as the asset-editor tab. Confirm two preview-bearing
  tabs (asset-editor + material-graph) can both be *open*; only the active one drives the modal
  `preview_scene` (they are never simultaneously active).

## Verification

- With the material-graph tab active, the pane shows the live sphere; dragging orbits it; the wheel
  does **not** zoom.
- Switching to the asset-editor "View" tab and back re-enters each preview correctly (modal swap).
- The scene viewport is parked while the graph tab is active (no double-render).

## Risks

- **Subsurface bounds inside a React-Flow pane.** The preview pane may sit inside a scrolling/zooming
  node-graph layout; `useSubsurfaceBounds` throttles/debounces bounds sync but a pan/zoom of the graph
  canvas must not thrash it. Test under React-Flow interaction; the pane should be a fixed sidebar
  region, not inside the pannable canvas.
