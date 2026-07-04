+++
title = 'Material-graph live preview'
weight = 6
+++

# Material-graph live preview

The material-graph editor's preview pane is a **live, orbitable 3D sphere** showing the material you are
editing under real image-based lighting — not a static thumbnail. Drag to spin it; every graph edit
morphs the surface on the pane's next frame. It is a mini version of the [asset editor](../asset-editor/)'s
"View" tab embedded in the graph editor, and it reuses that machinery wholesale rather than standing up
a parallel preview path.

## One view, reused — not a concurrent one

The host renders exactly one view per frame: a single active view, a single-slot shm publish. A live
material preview *sounds* like it needs a third concurrent view running alongside the scene, but it does
not. The material-graph editor is a full-work-area main tab — while it is open, the scene viewport is
parked anyway. So the preview is never on screen at the same time as the scene, and it can borrow the
existing modal **`assetPreview`** view the asset editor already owns.

Both preview-bearing tabs — the asset editor and the material graph — therefore drive the *same* view
and subsurface. Only one is ever active, so they are a **modal swap, never co-resident**: when a
material-graph tab becomes active it takes the preview, and the kept-mounted asset editor is released so
exactly one subject is ever live. `activeRenderView` maps both tab kinds to `assetPreview`; the park
effect unparks that view and parks the scene, the same handoff a tab switch already performs.

## The subject: a sphere that references the material by id

Entering the preview is the ordinary [`enter-asset-preview`](../../tooling-and-control/asset-commands/)
command with a **material** subject. `enter_material_preview` builds an isolated preview scene holding a
single built-in sphere whose `MaterialSet` slot 0 **references the `.smat` by id** — furnished by the
shared studio furnisher (floor, key light, procedural sky, framed orbit), identical to the model and
texture subjects.

The by-id reference is the whole trick behind live edits. A graph edit debounces to `material-set-graph`,
which writes the `.smat` and **invalidates the material cache**; because the sphere resolves its material
by id and the `assetPreview` view redraws every frame, the next frame re-resolves the edited material and
the sphere updates. There is no readback-to-PNG round trip, so you can keep orbiting while you edit. The
same `enter_material_preview` path also gives a standalone material a first-class "View" tab (double-click
a `.smat` in the [Assets panel](../assets-panel-and-thumbnails/)); the graph editor is where you *edit* it.

## Pan-only orbit + exposure

The eased orbit — input moves a target, a rAF loop drains current→target with the engine's tau, one
coalesced `set-camera` in flight — lives in one shared hook (`useOrbitCamera`), used by both the asset
editor and this pane. The material pane opts out of the wheel dolly (`enableZoom: false`): the framed
sphere is the whole subject, so you pan around it but never zoom. An **EV** slider sweeps the tonemap
exposure to judge the material across a stop range; that exposure is stashed on preview enter and
restored on exit (and on any switch back to the scene), so the sweep never dirties the authored
viewport's exposure.

The pane is a transparent hole down to the subsurface, exactly like the scene and asset-preview panels:
the graph editor's root paints no background, and its toolbar, node canvas, and the Preview header each
paint their own opaque surface, so only the sphere region shows the live frame through.

## In the code

| What | File | Symbols |
|---|---|---|
| Material subject + by-id live reflection (engine) | `engine/crates/control/src/commands_asset.rs` | `enter_material_preview`, the `Material` arm of `enter-asset-preview`, `furnish_preview_scene` |
| Cache invalidation on edit (engine) | `engine/crates/assets/src/material.rs` · `commands_asset.rs` | `update_material_asset`, `material-set-graph` |
| The live pane + enter/exit + EV (editor) | `editor/src/panels/MaterialGraphEditor.tsx` | `GraphCanvas`, `hostRef`, `useSubsurfaceBounds`, `onExposure` |
| Shared eased orbit hook | `editor/src/lib/useOrbitCamera.ts` | `useOrbitCamera` (`enableZoom`, `onClick`, `setFramed`) |
| Modal swap + view routing (editor) | `editor/src/app/App.tsx` | `activeRenderView`, `assetParked`, `mountedAssetId` |

## Related

- [Asset editor](../asset-editor/) — the preview view + orbit this pane reuses; the model/texture/HDRI subjects
- [Native materials](../../materials-and-pipelines/native-materials/) — the `.smat` the sphere shades with
- [Material node-graph codegen](../../materials-and-pipelines/node-graph-codegen/) — how the edited graph becomes the shader the preview renders
- [Viewport compositing](../viewport-compositing/) — the subsurface-below-the-webview foundation the transparent pane relies on
