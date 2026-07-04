+++
title = 'Asset editor'
weight = 6
+++

# Asset editor

The asset editor is a full work-area tab that opens *any* model on its own — a live 3D preview, plus whatever inspection panels the model can support — without ever spawning the asset into the scene you are building. Double-click a model, a mesh, or a clip in the Assets panel and it opens in its own tab. It is the engine's equivalent of UE5's Persona, Unity's import preview, and Godot's Advanced Import Settings: asset-level inspection lives apart from the open level.

Every model opens here, rigged or static. A glTF prop with no skeleton gets the same framed, orbitable preview viewport a fully rigged character does — the editor simply shows fewer panels. v1 is a viewer, not an authoring tool: you can look at the model, orbit it, and (when it has them) walk its bones, switch clips, and play or scrub at full frame rate. Keyframe editing, notify tracks, retargeting, and sockets are out of scope.

## Capability-aware: what is the asset capable of?

The editor does not assume a model has a rig. On open it reads a **capability descriptor** for the asset and lights up only the panels the model can actually feed:

| Model shape | Panels shown |
|---|---|
| Static (no rig, no clips) | center preview viewport + floor toggle |
| Rigged (no clips) | + skeleton tree (left) + bone/axes overlay toggles |
| Rigged, with clips | + clip list (right) + scrubbable timeline (bottom) + transport |

This is additive by design. The descriptor is a nested struct (`AssetCapabilitiesDto`) carrying mesh/material/node counts, a `hasRig` flag with the bone count, and the clip count. New capabilities — physics bodies, sockets, LODs — arrive as one appended field and one more conditional panel, never a new command or a new editor. The engine's viewport overlays self-gate the same way: the skeleton overlay only builds when the previewed root carries a `SkinnedMesh` component, so it draws nothing for a static model without any special-casing.

## The third mode: preview

The scene edit context already had two modes routed through one accessor, `active_scene`: Edit hands back the authored scene, Play hands back a throwaway duplicate (see [Play mode](../play-mode/)). The asset editor adds a third — Preview — the same way. Entering a preview builds a fresh `Scene` holding only the previewed model plus its furnishing, and `active_scene` returns it while it is engaged. The render path, compute skinning, the animation evaluator, and every entity-addressed control command retarget to the preview for free, because they all already route through that one chokepoint.

Preview stays in `PlayState::Edit` (it is mutually exclusive with Play), so it is best read as "Edit, but looking at an isolated asset instead of your scene." The authored scene cannot leak into a save, because `save-project` serializes `ctx.scene_edit.scene` explicitly — never `active_scene`. The keystone invariant the end-to-end tests guard: entering a preview, scrubbing it, and leaving it returns `project.json` **byte-identical**, including the `editorCamera` block — and that holds for static models too, which carry no animation to scrub. The camera, the selection, and the skeleton-overlay preferences are stashed on enter and restored on exit, all engine-side, so even a CLI-driven `enter`/`exit` with no editor in the loop is leak-proof.

Commands that would mutate the authored scene or project — `new-project`, `open-project`, `load-scene`, `load-project`, `reload-project`, `delete-asset`, `import-model`, `assign-asset` — refuse while a preview is engaged ("exit the asset preview first"), the same way they refuse during Play. Entering a preview while Play is running is likewise refused, and `play` is refused while previewing.

## Its own viewport surface

The preview is a full second view, not a takeover. The renderer holds one offscreen target + shm ring + Wayland subsurface per [`ViewId`](../viewport-compositing/) — `Scene` and `AssetPreview` — and, like a major engine's per-view resource bundle, each view owns its own screenspace/post-process **descriptor sets** (bind groups) alongside those images, so switching the active view never leaves a set bound to the other view's images. The asset tab drives its **own** `assetPreview` surface (the bounds emitter is the shared `useSubsurfaceBounds` hook, parameterized by view id), permanently glued to its center pane. The engine publishes the preview scene through that view's ring; orbiting and scrubbing update its live frame at monitor refresh, with the scene viewport's own surface untouched beside it.

Only the active view is rendered each frame, so the two views never both pay GPU cost at once. Switching tabs parks the hidden view's surface (it keeps its last frame frozen) and unparks the shown one — no re-bind, no resize, no stale-size frame on the switch. The deliberate tradeoff for "preserve last" being color-only: per-view temporal accumulators (TAA history, motion vectors, ReSTIR, the DDGI volume) reset when a view re-activates, so GI and antialiasing re-converge over a few frames rather than the engine keeping two histories warm.

Switching *away* from the asset tab doesn't drop the preview — it switches the **active view** back to the scene (`set-active-view scene`): the preview `Scene` stays alive, `preview_active_view` flips off, and `active_scene` routes back to the authored scene (so the scene tab is fully editable again and `previewing()` reads false). Switching back (`set-active-view assetPreview`) re-activates it — no re-spawn, the orbit camera and selection restore instantly. The editor keeps the asset workspace mounted across the switch (its panels, model, and skeleton-tree state survive). The view round-trip preserves byte-identity just like exit does. Closing the asset tab exits for real (drops the preview); switching to the scene view while the user edited the scene leaves those edits intact.

## The model is asset data

What the editor inspects is not a scene object — it is data baked into the [`.smodel` container](../../geometry-and-assets/smodel-container/). The container's metadata chunk holds the node forest; a rigged model also holds a skin (joints, inverse binds, skeleton root, mesh node); the animation clips and materials are sub-assets of the same file. So the clip↔mesh↔material association is intrinsic — one file, one asset — with no catalog link to chase and no project version bump.

`get-asset-model {asset}` reads that metadata and returns the model's capabilities, its clips, and — when it is rigged — a flat parent-indexed bone tree. It accepts the model, a mesh sub-asset, or a clip sub-asset — all resolve to the same owning container, which is why opening a clip and opening its mesh focus the *same* tab. The bone list is the skeleton subtree (the joints and their intermediate ancestors, bounded at the skeleton root), so the mesh node and unrelated scene roots are excluded. A model with no skin in its container is not an error: it comes back with `hasRig=false` and an empty bone tree, and the editor simply omits the rig panels.

Because everything lives in the file, re-opening the project — or deleting the spawned entities — re-derives it from the container. There is nothing to persist.

## The panels

**The tab is keyed by the owning model**, not the clicked asset: a mesh and any of its clips open or focus one tab (`assetEditor:<modelId>`). That is why the single-preview engine constraint can never be violated by two tabs of one model, and why switching from model A to model B remounts the workspace (cleanup exits A, mount enters B) rather than silently leaving A previewing under B's panels.

- **Skeleton tree** (left, rigged only) — the bone hierarchy from `get-asset-model`, render joints emphasized and intermediate nodes muted. Clicking a bone tints it in the live overlay through a dedicated **highlight channel** (`set-skeleton-highlight {joint}`), addressed by the bone's node index — *not* scene selection. This is deliberate: selecting a bone entity would null the selection-keyed animation state the timeline reads, and the selection-keyed overlay only draws for a `SkinnedMesh`. Highlighting keeps the engine selection on the model, so the timeline stays fed and the overlay stays drawn. Selection also flows **in reverse**: clicking a joint dot in the viewport hits `pick-skeleton-joint {u,v}` (a screen-space nearest-joint test — the overlay dots aren't pickable entities), which drives the same highlight channel and scrolls the row into view.
- **Preview** (center, always) — the live model on a floor under a key light and procedural sky, framed on enter (a 3/4 view fit to the model's bounding sphere; the framing reads the `SkinnedMesh` or `Mesh` component, so static models frame correctly too, and it scales the near plane to the model so a small model dollies in close without clipping). Drag to orbit, wheel to dolly; both write an orbit *target* that a per-frame easer drains toward (the engine's `tau=0.025` feel — slight lag, smooth motion, no React render) and push a coalesced `set-camera`. Edit chrome (the gizmo, billboards, camera frustums) is suppressed — the preview is "Edit without chrome" — while the skeleton overlay defaults on for rigged models (its joint dots hold a constant on-screen size at any zoom). "Show floor" toggles via `set-asset-preview-options`. Opening masks the layout settle behind a "Preparing…" spinner: the panels + subsurface mount only once the capabilities are known, so the first frame already has the right panels at the final width.
- **Clip list + details** (right, with-clips only) — the model's own clips (its container's animation sub-assets, not the whole catalog). Clicking a clip loads it paused at frame 0 (`play-animation {paused}` — select loads, the transport plays). The details section reports the focused clip (duration, tracks, wrap) or the model (mesh, bones, joints, clips).
- **Timeline** (bottom, with-clips only) — the same transport and lane surface the dock Timeline uses, factored into shared components and mounted here against the previewed model. Play/pause/loop/step, Space to toggle, and full-rate scrubbing of the live preview through the existing 50 ms seek coalescer. The playhead self-advances on a rAF while playing (the reconcile poll only refetches on a version bump, not as time advances) and re-syncs to engine truth on each play/pause/seek. Scrub seeks carry a `seekBlend` so the pose eases between the sparse seeks (`seek-animation`'s self-transition) instead of snapping. The dock Timeline and the asset-editor timeline never render at once (the dock is hidden while a non-scene tab is active).

## Non-model subjects: textures and HDRIs

A standalone texture is not a model, but it still earns a lit 3D preview — a grey swatch is the wrong inspection view for a map that has a physical role. Every texture opens the asset editor too, and `enter-asset-preview` branches on the asset's [role](../../geometry-and-assets/asset-server-and-catalog/) instead of instantiating a container:

- **A non-HDR map** previews as *its channel doing its job* on one lit studio sphere. The role picks the slot — a roughness map drives roughness, a normal map perturbs the surface, an emissive map glows — by synthesizing an ephemeral single-slot material (`preview_material_for_texture`) seeded into the material cache under a reserved id, so the ordinary scene resolver shades the sphere with no catalog write. The sphere is orbit-only (the framed sphere is the whole subject, so the wheel does not dolly); the toolbar's **Applied / Flat** picker flips to the raw channel in the [image viewer](../viewport-panel/).
- **An HDRI** (`role == hdri`, or an `.hdr`/`.exr`) opens the AmbientCG-style environment rig: the equirect becomes both the visible backdrop and the IBL source (`SkyMode::Texture`), lighting **three PBR balls** — chrome, diffuse grey, colored satin — that read its reflections, irradiance, and color response. There is no directional key light (the environment lights everything), and the dolly stays enabled (three balls to move between). A −6…+6 **EV** slider sweeps the tonemap exposure to reveal clipped-vs-real range; that exposure is stashed on enter and restored on exit and on any switch back to the scene view, so the preview sweep never bleeds into the authored viewport.

The furnishing accepts a `PreviewEnv` (`Procedural` for a model or a lone sphere, `Hdri(id)` for the environment rig), so the shared enter/frame/commit path drives all three subject kinds. A height map is the one role still awaiting its true form: it previews through the übershader's existing parallax-occlusion mapping today, and gains a real displaced silhouette once vertex-shader displacement on the preview sphere lands.

## Opening it

Models, meshes, animation clips, **every texture, and materials** route to the asset editor; only non-previewable files ("other") open the [image viewer](../viewport-panel/) (also reached in reverse from a texture's **Flat** picker). A material opens as itself on the studio sphere — the [material-graph editor](../material-graph-live-preview/) stays the place to *edit* it, and shares this same preview machinery. The asset scan derives `rigged` and `duration` flags from the container and puts them on the catalog row, so a tile can show a clapperboard icon and a duration badge synchronously — but routing no longer gates on `rigged`: every model opens the editor, and the per-model capability descriptor (fetched once on tab mount) decides which panels appear.

## In the code

| What | File | Symbols |
|---|---|---|
| Preview scene + accessor + guards (engine) | `engine/crates/sceneedit/src/context.rs` | `preview_scene`, `preview_active_view`, `active_scene`, `previewing`, `preview_root_entity` |
| Enter/exit + view switch + furnishing + framing (engine) | `engine/crates/control/src/commands_asset.rs` | `enter-asset-preview`, `exit-asset-preview`, `set-active-view`, `set-asset-preview-options`, `activate_preview_view`, `deactivate_preview_view`, `furnish_preview_scene`, `leave_asset_preview`, `compute_preview_bounds` |
| Texture / HDRI preview subjects + env + exposure stash (engine) | `engine/crates/control/src/commands_asset.rs` | `enter_texture_preview`, `enter_hdri_preview`, `preview_material_for_texture`, `commit_preview_subject`, `PreviewEnv`, `saved_exposure` |
| Asset-model query + capabilities (engine) | `engine/crates/control/src/commands_asset.rs` | `get-asset-model`, `AssetCapabilitiesDto` |
| Skeleton overlay + highlight (engine) | `engine/crates/host/src/overlay.rs` | `build_skeleton_overlay` |
| Highlight + reverse joint pick + paused-pick + smooth scrub (engine) | `engine/crates/control/src/commands_animation.rs` | `set-skeleton-highlight`, `pick-skeleton-joint`, `play-animation` `paused`, `seek-animation` `seekBlend` |
| Workspace shell + orbit + lifecycle + capability gating | `editor/src/panels/AssetEditorWorkspace.tsx` | `AssetEditorWorkspace` |
| Side panels | `editor/src/panels/SkeletonTree.tsx` · `ClipList.tsx` | `SkeletonTree`, `ClipList` |
| Shared timeline | `editor/src/components/timeline/` | `TimelineTransport`, `TimelineSurface`, `TimelineTarget` |
| Per-view subsurface bounds (editor) | `editor/src/lib/useSubsurfaceBounds.ts` | `useSubsurfaceBounds` (view-keyed) |
| Keep-mounted + per-view park + set-active-view on tab switch (editor) | `editor/src/app/App.tsx` · `AssetEditorWorkspace.tsx` | `mountedAssetId`, `active` prop, `setViewportParked`, `setActiveView` |
| Tab + routing | `editor/src/state/store.ts` · `AssetsPanel.tsx` | `openAssetEditorTab`, `openAssetEditorForAsset`, `routeView` |
| Client wrappers | `editor/src/control/client.ts` | `getAssetModel`, `enterAssetPreview`, `exitAssetPreview`, `setActiveView`, `setViewportParked`, `setSkeletonHighlight`, `setAssetPreviewOptions` |

## Related

- [Play mode](../play-mode/) — the duplicate-scene pattern the preview extends into a third mode
- [Skeleton overlay](../../animation/skeleton-overlay/) — the line overlay the preview defaults on for rigged models, keyed with a highlight channel
- [Timeline](../../animation/timeline/) — the same transport + surface, mounted against the previewed model
- [`.smodel` container](../../geometry-and-assets/smodel-container/) — where the mesh, rig, clips, and materials live as one asset
- [Viewport panel](../viewport-panel/) — the bounds-emission machinery the preview reuses through `useSubsurfaceBounds`
