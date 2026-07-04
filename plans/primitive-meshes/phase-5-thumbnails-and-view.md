# Thumbnails + "View" tab for built-in primitives

**Status:** NOT STARTED
**Scope:** `saffron-control`, `saffron-assets`, `saffron-rendering`
**Depends on:** phase-2 (CPU mesh reachable), phase-3 (native spawn)

## Goal

A built-in primitive has a working picker thumbnail and can be opened in the 3D "View" tab, even though
it is not a catalog row.

## Touch points

- **`get-thumbnail` (`control/src/commands_asset.rs`, `assets/src/thumbnail.rs`)** — the swatch fetches
  `get-thumbnail` by id, which today resolves through the catalog/thumbnail worker; reserved ids have no
  catalog entry. Add a built-in branch: render the thumbnail on the primitive's **own geometry** via the
  offscreen thumbnail renderer (reuse `render_material_preview` for the sphere; render the cube/plane on
  *their* mesh, not a sphere — see risk). Key the cache by the reserved id + a fixed content hash.
- **`enter-asset-preview` (`control/src/commands_asset.rs`)** — it resolves an asset → its model
  container and **hard-rejects `container_id == 0`**. A primitive has no container. Add a synthetic
  branch: build the isolated `Scene::new()`, spawn an entity with `Mesh { mesh: <builtin id> }` + default
  `MaterialSet` instead of `instantiate_model`, then reuse `furnish_preview_scene` (floor +
  `DirectionalLight` + `SkyMode::Procedural` + framed orbit cam via `compute_preview_bounds`); return
  empty bones. (This branch is *also* the seam the texture/material previews extend — see
  `texture-material-previews/phase-3`.)

## Verification

- The mesh picker swatch shows a correct thumbnail for each primitive (sphere on a sphere, cube on a
  cube, plane on a plane).
- "View" on a primitive (reachable from the picker/inspector, since primitives are not grid rows) opens
  the 3D orbit tab showing the primitive.

## Risks

- **Thumbnail shape mismatch.** `render_material_preview` renders on a *sphere*. A cube or plane
  thumbnailed on a sphere is misleading. Either thread the primitive's own geometry through the
  thumbnail renderer (preferred — the generators from phase-1 make this cheap) or accept the mismatch /
  ship static icons. Prefer rendering the real shape.
