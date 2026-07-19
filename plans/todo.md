# Todo

## Editor UX

- Drag and drop models from the asset browser to create entities in the scene.
- Fix the laggy asset drag-in preview (ghost entity + async upload + broadphase pick) → `plans/asset-drag-preview/` (large assets stall the viewport during the drag preview).
- Let Inspector components have an explicit order, with add-at-bottom behavior, drag reordering, and a sort action.
- Fix browser UI quirks like drag-selecting elements so the editor feels like a normal desktop app.

## Rendering

- Improve PBR effect, seems a bit foggy.
- Improve lighting support for transparency, opacity, and self-shadowing.
- Performance: reactive idling, shadow caching, quality tiers, converge-then-stop → `plans/rendering-performance/` (a static scene currently pins the GPU at 100% / 281 W).
- True geometric displacement (research track): near-term VS-displaced preview sphere (real silhouettes); long-horizon in-scene compute adaptive tessellation → scratch buffer → BLAS. Skip fixed-function tessellation (UE removed it) and DMM (deprecated) → `plans/displacement/`.

## Physics and animation

- Physics-based two-way bound animations after physics.

## Data safety

- No "unsaved changes" guard anywhere: closing the editor window silently discards unsaved scene edits — no project dirty flag, no exit-time save prompt (`WindowEvent::CloseRequested` is unhandled in `editor/shell/`), no autosave/crash-recovery. Add a dirty flag → exit prompt → periodic autosave-to-sidecar → recover-on-open. (The only `dirty` field in the protocol is a reflection-probe staleness flag, unrelated.)
- Recoverable asset deletion: `delete-asset` / `delete-unused` hard-unlink via `std::fs::remove_file` (`manage.rs`) — no trash bin, no recovery (filesystem ops are correctly kept off the scene undo stack, so "undo delete" is not the mechanism). Add a project-local `.trash/` soft-delete (move-on-delete, restorable, reaped later) so a mistaken delete survives without relying on VCS.

## Assets

- Durable rename of an *embedded* (non-extracted) model sub-asset. Standalone assets, models, and extracted sub-assets now persist name/folder/colorspace to a co-located `.smeta` sidecar (survives a never-saved cold scan); an embedded sub-asset has no own file, so its rename still reverts to the container META until it is extracted. Needs a per-sub-asset override map inside the `.smodel` sidecar or a container-META rewrite.

## Game systems

- Research game UI and overlay authoring for health bars and HUDs, including how Unreal Engine 5 and Unity approach it.

> Audio, networking, and asset-store research moved out of this list: audio → `plans/pending-ideas/audio-system.md`, networking → `plans/pending-ideas/networking-multiplayer.md`, asset store → the `plans/assets-connectors/` plan.
