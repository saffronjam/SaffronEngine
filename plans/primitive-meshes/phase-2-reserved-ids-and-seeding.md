# Reserved ids + GPU cache seeding for built-in meshes

**Status:** IMPLEMENTED. `assets/src/lib.rs` adds `BUILTIN_{CUBE,PLANE,SPHERE}_MESH_ID` (3/4/5) +
`BuiltinMesh` (`reserved_id`/`from_reserved_id`/`display_name`/`geometry`). **Refinement:** instead of a
separate eager `ensure_builtin_meshes`, `load_mesh_asset` (`assets/src/load.rs`) resolves a reserved
built-in id **on demand** via `seed_builtin_mesh` (generate → upload → cache, no SDF, no catalog) — a
single resolution path that re-seeds automatically after a project-load cache clear. The CPU-mesh
concern is moot: `GpuMesh` already carries `cpu_positions`/`cpu_indices`, so `mesh_pick_bvh` works for
built-ins with no extra plumbing. Reserved-id assertions extended in the `reserved_sentinels` test.
**Scope:** `saffron-assets`
**Depends on:** phase-1 (the generators)

## Goal

A `BuiltinMesh` enum mapped to reserved ids, uploaded once and seeded into the GPU mesh cache with **no
catalog row**, resolving cache-first through the existing `load_mesh_asset` path. Generalizes the
proven `ensure_preview_floor_mesh` mechanism.

## Touch points

- **`assets/src/lib.rs`** (beside `DEFAULT_MATERIAL_ID`/`PREVIEW_FLOOR_MESH_ID`):
  ```rust
  pub const BUILTIN_CUBE_MESH_ID:   Uuid = Uuid(3);
  pub const BUILTIN_PLANE_MESH_ID:  Uuid = Uuid(4);
  pub const BUILTIN_SPHERE_MESH_ID: Uuid = Uuid(5);
  pub enum BuiltinMesh { Cube, Plane, Sphere }
  // reserved_id() -> 3/4/5, from_reserved_id(Uuid) -> Option<Self>, display_name() -> "Cube"/…
  ```
  `from_reserved_id` is the single `< 1024 → BuiltinMesh` decoder used everywhere (mirror it as a TS
  constant on the editor side).
- **`assets/src/load.rs`** — generalize `ensure_preview_floor_mesh` into `ensure_builtin_meshes(gpu)`:
  for each `BuiltinMesh`, generate its `geometry::Mesh` (phase-1), `gpu.upload_mesh(...)`, seed
  `mesh_by_uuid` under the reserved id. Keep the CPU `geometry::Mesh` reachable (cache it alongside, or
  keep it regenerable) for pick-BVH and thumbnail rendering — reserved ids have **no catalog container
  to reload from**.
- **`assets/src/load.rs`** — the CPU-mesh / pick-BVH path (`mesh_pick_bvh`): confirm the BVH builder can
  accept an injected/regenerated CPU mesh for a reserved id (it currently reads from a loadable
  catalog asset). If not, add a built-in branch that hands it the generated mesh.
- **Cache lifecycle:** wherever `clear_asset_caches` runs on project switch, **re-seed** built-ins (or
  exempt reserved ids from eviction). A primitive is project-independent and must survive reloads —
  today `ensure_preview_floor_mesh` is re-invoked per preview enter; built-ins must be seeded at least
  as reliably (seed at asset-server init and after any cache clear).

## Verification

- A `Mesh { mesh: BUILTIN_SPHERE_MESH_ID }` entity resolves to a `GpuMesh` via `load_mesh_asset`
  cache-first with no catalog lookup and renders.
- Reserved built-ins survive a project unload/reload (regression: entity keeps rendering).
- `just engine` + `clippy -D warnings` clean.

## Risks

- **CPU-mesh reachability** for pick-BVH and thumbnails is the one non-obvious piece — draw is
  cache-first and trivially works, but picking and thumbnailing want the CPU vertex data. Cache it at
  seed time.
