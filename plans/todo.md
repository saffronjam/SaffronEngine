# Todo

## Editor UX

- Drag and drop models from the asset browser to create entities in the scene.
- Fix the laggy asset drag-in preview (ghost entity + async upload + broadphase pick) → `plans/asset-drag-preview/` (large assets stall the viewport during the drag preview).
- Let Inspector components have an explicit order, with add-at-bottom behavior, drag reordering, and a sort action.
- Fix browser UI quirks like drag-selecting elements so the editor feels like a normal desktop app.

## Materials

- Collapse the two parallel material worlds (inline per-entity PBR blobs vs `.smat` graph assets) into one reference+override model, and fix the reverting/laggy Inspector material fields → `plans/material-instances/` (Phase 1 is a standalone Inspector correctness fix).

## Rendering

- Improve PBR effect, seems a bit foggy.
- Improve lighting support for transparency, opacity, and self-shadowing.
- Performance: reactive idling, shadow caching, quality tiers, converge-then-stop → `plans/rendering-performance/` (a static scene currently pins the GPU at 100% / 281 W).

## Physics and animation

- Physics-based two-way bound animations after physics.

## Game systems

- Research game UI and overlay authoring for health bars and HUDs, including how Unreal Engine 5 and Unity approach it.

> Audio, networking, and asset-store research moved out of this list: audio → `plans/pending-ideas/audio-system.md`, networking → `plans/pending-ideas/networking-multiplayer.md`, asset store → the `plans/assets-connectors/` plan.
