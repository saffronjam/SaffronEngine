# Built-in primitive meshes — cube / plane / sphere as native, non-asset geometry

**Status:** IMPLEMENTED (Phases 1–6). The reproducible, host-independent gate is green — workspace
`cargo build` + `clippy -D warnings` + `fmt --check`, the new geometry / reserved-id / wire unit
tests, and the editor `tsc` + `oxlint` + `vite build`. The **e2e** (`tests/e2e/primitives.test.ts`),
the control-schema contract, and the present-only smoke require a live host, which the dev sandbox
cannot drive (the standalone host's frame loop does not pump the control drain under
weston-headless/llvmpipe — an unmodified `scene.test.ts` times out identically); they validate in CI
/ the editor. Two design points the real code settled, noted in the phase files: primitives emit **no
tangents** (the `Vertex` format has none; shaders derive the frame), and built-ins are seeded
**on demand inside `load_mesh_asset`** rather than via a separate eager `ensure_builtin_meshes`.

This is the **root** plan of a four-plan set. Nothing interactive in the sibling plans
(`texture-material-previews/`, `material-graph-live-preview/`, `displacement/`) can ship without a
**scene-spawnable sphere/plane/cube** that is *not* a project asset. That facility lives here.

## Diagnosis (current code)

"Create → Cube" does not create a primitive — it launders a bundled glTF through the project catalog:

- `editor/src/panels/CreateMenu.tsx` lists `{ label: "Cube", preset: "cube" }` → `client.addEntity("cube")`.
- `engine/crates/protocol/src/dto.rs` — `enum AddEntityPreset { Empty, Cube, Model, … }`.
- `engine/crates/control/src/commands_scene.rs` `add-entity`: the `Cube | Model` arm is one shared path
  — it requires a loaded project (`project_ready()` → else `no project loaded`), resolves
  `engine_asset_path("models/cube.gltf")`, calls `ensure_builtin_model_asset(...)`, then
  `instantiate_model(...)`.
- `engine/crates/assets/src/import.rs` `ensure_builtin_model_asset`: on a cold cache
  `translate_model(cube.gltf)` → `bake_model(...)` → **`catalog.put(row)` for every baked row** (the
  model container *plus* its mesh + material sub-assets). Those become real `Model`/`Mesh`/`Material`
  catalog rows visible in the Assets panel — **the "fake cube asset" this plan removes.**

Two independent smells: a primitive *requires a project*, and adding a cube *mutates the catalog*.

## Approach — reserved-id native meshes

The engine already reserves ids `< 1024` for built-ins (`engine/crates/core/src/uuid.rs`,
`RESERVED_BELOW = 1024`; `Uuid::new()` mints from `[1024, u64::MAX]`). Two reserved ids already exist:
`DEFAULT_MATERIAL_ID = Uuid(1)`, `PREVIEW_FLOOR_MESH_ID = Uuid(2)` (`engine/crates/assets/src/lib.rs`).
`ensure_preview_floor_mesh` (`assets/src/load.rs`) is a **working prototype of exactly the pattern we
want**: translate geometry → `gpu.upload_mesh(...)` → seed `mesh_by_uuid` under a reserved id **with no
catalog row**, so it "renders without a catalog row that would serialize." Resolution is cache-first
(`load_mesh_asset` checks `mesh_by_uuid` before the catalog), so a reserved id resolves with zero
catalog interaction.

Generalize it:

- `BuiltinMesh { Cube, Plane, Sphere }` → reserved ids `Uuid(3/4/5)`.
- Geometry authored **once** in `saffron-geometry` (`uv_sphere()`, `cube()`, `plane()`), replacing the
  offscreen-only `make_preview_sphere` (`rendering/src/thumbnail_render.rs`) and the `#[cfg(test)]`
  `unit_sphere` (`geometry/src/sdf.rs`) — no second sphere implementation.
- The referencing component (`Mesh { mesh: Uuid }`), the draw path, `MaterialSet`, and the scene JSON
  need **no changes** — a primitive is just a reserved `Uuid` that serializes as `"mesh":"3"`.
- The Inspector mesh picker gains a fixed **"Built-ins"** group and renders a **"Built-in" chip** for a
  value `< 1024`; `list-assets` stays catalog-only, so built-ins are *choosable but never catalog rows*.
- Per NO-LEGACY: **delete** `ensure_builtin_model_asset` and the `Cube | Model` arm and rebuild the
  spawn on the native path in the same change.

## Phases (dependency-ordered)

1. **`phase-1-geometry-generators.md`** — `geometry::{cube,plane,uv_sphere}` with tangents; retire `make_preview_sphere`'s private loop.
2. **`phase-2-reserved-ids-and-seeding.md`** — `BuiltinMesh` + ids 3/4/5; `ensure_builtin_meshes`; survive project reloads; keep the CPU mesh for pick-BVH.
3. **`phase-3-native-spawn-cutover.md`** — native `Mesh` + `MaterialSet` spawn; drop the project gate; **delete** the fake-asset path; extend `AddEntityPreset`.
4. **`phase-4-inspector-picker.md`** — "Built-ins" group + "Built-in" chip; built-in-aware `assign-asset` name resolution.
5. **`phase-5-thumbnails-and-view.md`** — `get-thumbnail` + `enter-asset-preview` branches for container-less built-ins.
6. **`phase-6-docs-sa-gate.md`** — docs page + hub row; `sa` scriptability; e2e; green `just check`.

## Downstream dependencies

- `texture-material-previews/` Phase 3–5 need `BUILTIN_SPHERE_MESH_ID` spawnable in a real `Scene`.
- `material-graph-live-preview/` needs the sphere for the material preview scene.
- `displacement/phase-a` needs a pre-subdividable sphere/plane base mesh.

## Open decisions (carried in, resolved)

- **Tangents:** generators emit tangents (decided) — must match the vertex layout `upload_mesh` expects
  and how importer meshes carry tangents. Pin this in Phase 1.
- **`Model` preset:** becomes a real "instantiate a chosen model asset" command (takes a model id), not
  a behaviour-less cube alias. Decided in Phase 3.
- **`models/cube.gltf`:** retained only if still needed as a test fixture after the cutover; otherwise
  removed.
