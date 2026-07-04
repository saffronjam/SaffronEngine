# Geometry generators: cube / plane / uv_sphere in saffron-geometry

**Status:** NOT STARTED
**Scope:** `saffron-geometry`, `saffron-rendering`
**Depends on:** — (nothing)

## Goal

One authoritative source of primitive geometry in `saffron-geometry`, producing `geometry::Mesh`
(`geometry/src/types.rs`) with **position, normal, uv0, and tangent**, one submesh each. Retire the
offscreen-only `make_preview_sphere` (`rendering/src/thumbnail_render.rs`) and the `#[cfg(test)]`
`unit_sphere` (`geometry/src/sdf.rs`) so there is exactly one generator per shape.

## Touch points

- **`geometry/src/` new module** (`primitives.rs`): `pub fn cube() -> Mesh`, `pub fn plane() -> Mesh`,
  `pub fn uv_sphere() -> Mesh`. Origin-centered, unit-scale (sphere radius 1, cube ±0.5, plane 1×1 on
  XZ facing +Y). Continuous UVs; sphere is the current UV-sphere topology (rings × sectors — reuse the
  32×48 from `make_preview_sphere`) or an icosphere (decide; see risks). Emit **tangents**.
- **`rendering/src/thumbnail_render.rs`** — `make_preview_sphere` becomes a thin caller of
  `geometry::uv_sphere()` (delete its private ring/sector loop). The thumbnail sphere and the
  scene-spawnable sphere are then the *same* geometry.
- **`geometry/src/sdf.rs`** — the test-only `unit_sphere` is replaced by `primitives::uv_sphere` in the
  SDF tests (or kept private if its analytic form is needed for the distance assertion).

## The tangent decision (pin here)

The scene draw path (normal mapping, POM/height in `lighting.slang`) needs a per-vertex tangent frame.
Importer meshes carry tangents from glTF or derive them at import. **Decide one source of truth:** the
primitive generators emit analytically-correct tangents (recommended — a sphere/plane/cube have exact
tangents), and confirm the vertex layout `Uploader::upload_mesh` (`rendering/src/upload.rs`) expects
matches what importer meshes produce. A primitive whose tangents disagree with importer meshes would
shade normal maps wrong on exactly the surfaces this whole plan set is meant to preview.

## Verification

- Unit test: each generator returns a watertight, correctly-wound mesh with unit normals and
  orthonormal TBN; sphere vertices lie on the unit sphere (reuse the old `unit_sphere_matches_analytic`
  assertion).
- `just engine` clean; the existing thumbnail render tests
  (`thumbnail_render.rs` studio-sphere brightness assertions) still pass with the refactored sphere.

## Risks

- **UV-sphere pole/wrap seams.** The classic UV sphere has a texture seam at the wrap meridian and
  degenerate poles. Fine for a lit preview; matters once Phase A displacement runs on it (seam cracks).
  An icosphere has continuous parameterization but non-square UVs. Decide per the displacement plan's
  needs — the near-term lit preview is unaffected either way.
