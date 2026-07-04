# Todo

## Editor UX

- Drag and drop models from the asset browser to create entities in the scene.
- Fix the laggy asset drag-in preview (ghost entity + async upload + broadphase pick) → `plans/asset-drag-preview/` (large assets stall the viewport during the drag preview).
- Let Inspector components have an explicit order, with add-at-bottom behavior, drag reordering, and a sort action.
- Fix browser UI quirks like drag-selecting elements so the editor feels like a normal desktop app.
- Built-in primitive meshes (cube/plane/sphere) as native, non-asset geometry — reserved-id meshes with a "Built-in" chip in the Inspector, replacing the cube-spawner that bakes a fake catalog asset → `plans/primitive-meshes/` (root prerequisite for the preview plans below).

## Materials

- Collapse the two parallel material worlds (inline per-entity PBR blobs vs `.smat` graph assets) into one reference+override model, and fix the reverting/laggy Inspector material fields → `plans/material-instances/` (Phase 1 is a standalone Inspector correctness fix).
- Live "spin-the-sphere" 3D preview in the material-graph editor (reusing the modal asset-preview view, pan-only camera), replacing the static PNG → `plans/material-graph-live-preview/`.

## Rendering

- Improve PBR effect, seems a bit foggy.
- Improve lighting support for transparency, opacity, and self-shadowing.
- Performance: reactive idling, shadow caching, quality tiers, converge-then-stop → `plans/rendering-performance/` (a static scene currently pins the GPU at 100% / 281 W).
- True geometric displacement (research track): near-term VS-displaced preview sphere (real silhouettes); long-horizon in-scene compute adaptive tessellation → scratch buffer → BLAS. Skip fixed-function tessellation (UE removed it) and DMM (deprecated) → `plans/displacement/`.

## Physics and animation

- Physics-based two-way bound animations after physics.

## Data safety

- No "unsaved changes" guard anywhere: closing the editor window silently discards unsaved scene edits — no project dirty flag, no exit-time save prompt (`WindowEvent::CloseRequested` is unhandled in `editor/src-tauri/`), no autosave/crash-recovery. Add a dirty flag → exit prompt → periodic autosave-to-sidecar → recover-on-open. (The only `dirty` field in the protocol is a reflection-probe staleness flag, unrelated.)
- Recoverable asset deletion: `delete-asset` / `delete-unused` hard-unlink via `std::fs::remove_file` (`manage.rs`) — no trash bin, no recovery (filesystem ops are correctly kept off the scene undo stack, so "undo delete" is not the mechanism). Add a project-local `.trash/` soft-delete (move-on-delete, restorable, reaped later) so a mistaken delete survives without relying on VCS.

## Assets

- Durable rename of an *embedded* (non-extracted) model sub-asset. Standalone assets, models, and extracted sub-assets now persist name/folder/colorspace to a co-located `.smeta` sidecar (survives a never-saved cold scan); an embedded sub-asset has no own file, so its rename still reverts to the container META until it is extracted. Needs a per-sub-asset override map inside the `.smodel` sidecar or a container-META rewrite.
- Preview textures & materials on a sphere — per-type (albedo/normal/roughness/metallic/height/AO/emissive/opacity/HDRI), as grid thumbnails and in the interactive 3D "View" tab, with a channel/representation picker and an AmbientCG-style HDRI environment preview (3-ball rig + EV exposure). Requires persisting a texture `role` the importer already computes then discards → `plans/texture-material-previews/`.

## Game systems

- Research game UI and overlay authoring for health bars and HUDs, including how Unreal Engine 5 and Unity approach it.

> Audio, networking, and asset-store research moved out of this list: audio → `plans/pending-ideas/audio-system.md`, networking → `plans/pending-ideas/networking-multiplayer.md`, asset store → the `plans/assets-connectors/` plan.
